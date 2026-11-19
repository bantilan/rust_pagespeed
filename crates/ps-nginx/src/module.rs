//! Module definition, commands, and phase hooks.
//!
//! Ports the module skeleton half of `ngx_pagespeed.cc`: the
//! `pagespeed` directive table (3 contexts), the two module
//! structs (`ngx_pagespeed`, `ngx_pagespeed_etag_filter`), the
//! pre/postconfiguration hooks, the conf create/merge wiring,
//! and the phase handlers. Request-time filter BODIES are
//! later slices; the hooks here run the real gating and
//! chaining and decline to the next handler when their slice
//! has not landed yet.
//!
//! The `ngx` crate's `HttpModule` trait cannot express this
//! module (raw commands table, dual modules, lifecycle hooks,
//! manual filter chaining), so this file drives `ngx::ffi`
//! directly with per-call `SAFETY:` comments.

use crate::conf::{self, LocConf, MainConf, NgxOptions, SrvConf};
use crate::directives::{self, Directive};
use crate::filters;
use ngx::ffi::{
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_http_conf_ctx_t,
    ngx_http_core_main_conf_t, ngx_http_handler_pt, ngx_http_module_t, ngx_http_phase_handler_t,
    ngx_http_phases_NGX_HTTP_POST_READ_PHASE, ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE,
    ngx_http_request_t, ngx_int_t, ngx_module_t, ngx_pool_cleanup_add, ngx_str_t, ngx_uint_t,
    NGX_AGAIN, NGX_CONF_TAKE1, NGX_CONF_TAKE2,
    NGX_CONF_TAKE3, NGX_CONF_TAKE4, NGX_CONF_TAKE5, NGX_DECLINED, NGX_ERROR, NGX_HTTP_LIF_CONF,
    NGX_HTTP_LOC_CONF, NGX_HTTP_MAIN_CONF, NGX_HTTP_MODULE, NGX_HTTP_SRV_CONF, NGX_OK,
    nginx_version,
};
use ngx::core::{NGX_CONF_ERROR, NGX_CONF_OK};
use ps_config::options::{apply_nginx_directive, NginxFactoryOptions};
use ps_config::scope::Scope;
use std::ffi::c_void;
use std::mem;
use std::ptr;

// ---------------------------------------------------------------------------
// Directive table (pure data)
// ---------------------------------------------------------------------------

/// Directive word (`pagespeed` in all three contexts).
pub const DIRECTIVE_NAME: &str = "pagespeed";

/// `NGX_CONF_MULTI`: gone from modern nginx; the C++ shims it to
/// 0 for old versions, and so does the port.
pub const NGX_CONF_MULTI: ngx_uint_t = 0;

/// Argument mask: `TAKE1..TAKE5 | MULTI`.
pub const ARG_MASK: ngx_uint_t =
    NGX_CONF_TAKE1 as ngx_uint_t
    | NGX_CONF_TAKE2 as ngx_uint_t
    | NGX_CONF_TAKE3 as ngx_uint_t
    | NGX_CONF_TAKE4 as ngx_uint_t
    | NGX_CONF_TAKE5 as ngx_uint_t
    | NGX_CONF_MULTI;

/// Conf offset for all three entries (`NGX_HTTP_SRV_CONF_OFFSET`,
/// verbatim — the C++ carries a TODO questioning whether loc/main
/// entries should use their own offsets).
pub fn srv_conf_offset() -> ngx_uint_t {
    mem::offset_of!(ngx_http_conf_ctx_t, srv_conf) as ngx_uint_t
}

/// The three command specs: (context mask, handler name).
pub const COMMAND_SPECS: [(ngx_uint_t, &str); 3] = [
    (NGX_HTTP_MAIN_CONF as ngx_uint_t, "ps_main_configure"),
    (NGX_HTTP_SRV_CONF as ngx_uint_t, "ps_srv_configure"),
    (
        NGX_HTTP_LOC_CONF as ngx_uint_t | NGX_HTTP_LIF_CONF as ngx_uint_t,
        "ps_loc_configure",
    ),
];

// ---------------------------------------------------------------------------
// Postconfiguration plan (pure)
// ---------------------------------------------------------------------------

/// Content-phase slot: `PRECONTENT` on nginx ≥ 1.13.4 (the older
/// `PREACCESS` branch is dead on every supported version).
pub const CONTENT_PHASE: u32 = ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE as u32;

/// Post-read phase slot for the self-fetch relabel handler.
pub const POST_READ_PHASE: u32 = ngx_http_phases_NGX_HTTP_POST_READ_PHASE as u32;

/// Postconfiguration decision (`ps_init` gating).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostPlan {
    /// Install the four filter chains (factory present).
    pub install_filters: bool,
    /// Push the preaccess/content-phase handler.
    pub push_content_phase: bool,
    /// Push the post-read self-fetch handler.
    pub push_post_read: bool,
}

/// Plans postconfiguration: everything iff a `pagespeed`
/// directive created the factory (`cfg_m->driver_factory`).
#[must_use]
pub const fn postconfiguration_plan(factory_present: bool) -> PostPlan {
    PostPlan {
        install_filters: factory_present,
        push_content_phase: factory_present,
        push_post_read: factory_present,
    }
}

/// Etag postconfiguration decision (`ps_etag_filter_init`).
#[must_use]
pub const fn etag_plan(factory_present: bool) -> bool {
    factory_present
}

/// Minimum nginx for the `PRECONTENT` branch (`NGINX_1_13_4`).
pub const PRECONTENT_FLOOR: u32 = 1013004;

// ---------------------------------------------------------------------------
// Phase-engine surgery (pure core over the FFI type)
// ---------------------------------------------------------------------------

/// Inserts the pagespeed phase step (`ps_preaccess_handler`).
///
/// Overwrites the current slot with the phase checker and steps
/// the handler index back so the engine re-runs from the slot
/// before — verbatim, including the pre-1.13.4 shift loop being
/// absent (dead on every supported version). The live wrapper
/// (`preaccess_handler`) cannot size the engine array, so it
/// repeats these two lines over a raw slot; the rule itself is
/// pinned here.
pub fn insert_phase_step(
    handlers: &mut [ngx_http_phase_handler_t],
    at: usize,
    checker: ngx_http_phase_handler_t,
) -> usize {
    handlers[at] = checker;
    at - 1
}

/// Builds the phase-step slot (`ps_preaccess_handler` body).
pub fn phase_step_slot(at: usize) -> ngx_http_phase_handler_t {
    ngx_http_phase_handler_t {
        checker: Some(phase_handler),
        handler: None,
        next: (at + 1) as ngx_uint_t,
    }
}

// ---------------------------------------------------------------------------
// Directive application (pure core)
// ---------------------------------------------------------------------------

/// Applies one parsed directive line to level options
/// (`ps_configure` past argument extraction).
///
/// Handles the master switch, the gzip toggle reporting, the
/// directory prep creation half, and the `ClearInheritedScripts`
/// flag, then delegates option lines to ps-config. Returns the
/// config error text on failure (null/`NGX_CONF_OK` mapping and
/// the ownership-fixup half happen in the FFI wrapper).
pub fn apply_directive_line(
    options: &mut Option<NgxOptions>,
    factory: &mut NginxFactoryOptions,
    scope: Scope,
    args: &[&str],
) -> Result<LineOutcome, String> {
    if args.len() > directives::MAX_DIRECTIVE_ARGS {
        return Err("too many arguments".to_string());
    }
    match directives::classify(args) {
        // The master switch arms the gzip setter AND falls through
        // to option parsing; `gzip` returns after the setter call.
        Directive::On | Directive::Off => {
            let gzip = if args[0].eq_ignore_ascii_case("on") {
                GzipCall::EnableForLocation
            } else {
                GzipCall::SetForLocation(false)
            };
            ensure_options(options);
            let slot = options.as_mut().expect("ensured above");
            apply_nginx_directive(
                &mut slot.core,
                factory,
                &mut slot.system,
                args[0],
                &[],
                scope,
            )
            .map_err(|err| err.message)?;
            Ok(LineOutcome::gzip(gzip))
        }
        Directive::Gzip(value) => Ok(LineOutcome::gzip(GzipCall::SetForLocation(value))),
        Directive::InvalidGzip => Err("Invalid pagespeed gzip setting".to_string()),
        Directive::DirPrep => {
            let (name, path) = (args[0], args[1]);
            directives::prepare_dir(name, path)?;
            ensure_options(options);
            let slot = options.as_mut().expect("ensured above");
            apply_nginx_directive(
                &mut slot.core,
                factory,
                &mut slot.system,
                name,
                &args[1..],
                scope,
            )
            .map_err(|err| err.message)?;
            Ok(LineOutcome {
                gzip: GzipCall::None,
                dir_prep: Some(DirPrepPath {
                    directive: name.to_string(),
                    path: path.to_string(),
                }),
            })
        }
        Directive::Options => {
            ensure_options(options);
            let slot = options.as_mut().expect("ensured above");
            if args.len() == 1 && args[0].eq_ignore_ascii_case("ClearInheritedScripts") {
                slot.clear_inherited_scripts = true;
            }
            apply_nginx_directive(
                &mut slot.core,
                factory,
                &mut slot.system,
                args[0],
                &args[1..],
                scope,
            )
            .map_err(|err| err.message)?;
            Ok(LineOutcome::gzip(GzipCall::None))
        }
    }
}

/// A `pagespeed` path needing ownership fixup (`ps_init_dir`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirPrepPath {
    /// Directive word (`LogDir` / `FileCachePath`).
    pub directive: String,
    /// Prepared path.
    pub path: String,
}

/// Outcome of one applied directive line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineOutcome {
    /// Gzip-setter call the line requests (Slice 6 executes).
    pub gzip: GzipCall,
    /// Directory needing ownership fixup, if any.
    pub dir_prep: Option<DirPrepPath>,
}

impl LineOutcome {
    fn gzip(gzip: GzipCall) -> Self {
        LineOutcome { gzip, dir_prep: None }
    }
}

// ---------------------------------------------------------------------------
// Module statics (FFI roots; live-tested in Slice 7)
// ---------------------------------------------------------------------------

/// Module signature for the staged 1.31.8 tree
/// (`NGX_MODULE_SIGNATURE` = `8,4,8,` + the 34 config bits).
pub const MODULE_SIGNATURE: &std::ffi::CStr =
    c"8,4,8,0011111111010111011111111111111111";

/// `NGX_MODULE_UNSET_INDEX` (`(ngx_uint_t) -1`, cast macro).
pub const MODULE_UNSET_INDEX: ngx_uint_t = ngx_uint_t::MAX;

static DIRECTIVE_WORD: &[u8; 10] = b"pagespeed\0";

/// Directive table (`ps_commands`: 3 contexts + null).
pub static mut PS_COMMANDS: [ngx_command_t; 4] = [
    ngx_command_t {
        name: ngx_str_t { len: 9, data: DIRECTIVE_WORD.as_ptr() as *mut u8 },
        type_: NGX_HTTP_MAIN_CONF as ngx_uint_t | ARG_MASK,
        set: Some(ps_main_configure),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_str_t { len: 9, data: DIRECTIVE_WORD.as_ptr() as *mut u8 },
        type_: NGX_HTTP_SRV_CONF as ngx_uint_t | ARG_MASK,
        set: Some(ps_srv_configure),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    ngx_command_t {
        name: ngx_str_t { len: 9, data: DIRECTIVE_WORD.as_ptr() as *mut u8 },
        type_: NGX_HTTP_LOC_CONF as ngx_uint_t | NGX_HTTP_LIF_CONF as ngx_uint_t | ARG_MASK,
        set: Some(ps_loc_configure),
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
    // `ngx_null_command`.
    ngx_command_t {
        name: ngx_str_t { len: 0, data: ptr::null_mut() },
        type_: 0,
        set: None,
        conf: 0,
        offset: 0,
        post: ptr::null_mut(),
    },
];

/// HTTP module context (`ps_module`).
pub static mut PS_HTTP_MODULE: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: Some(preconfiguration),
    postconfiguration: Some(postconfiguration),
    create_main_conf: Some(create_main_conf),
    init_main_conf: None,
    create_srv_conf: Some(create_srv_conf),
    merge_srv_conf: Some(merge_srv_conf),
    create_loc_conf: Some(create_loc_conf),
    merge_loc_conf: Some(merge_loc_conf),
};

/// Etag filter context (`ps_etag_filter_module`: post only).
pub static mut PS_ETAG_MODULE: ngx_http_module_t = ngx_http_module_t {
    preconfiguration: None,
    postconfiguration: Some(etag_postconfiguration),
    create_main_conf: None,
    init_main_conf: None,
    create_srv_conf: None,
    merge_srv_conf: None,
    create_loc_conf: None,
    merge_loc_conf: None,
};

/// Main module (`ngx_pagespeed`).
///
/// Lifecycle hooks (`init_module`, `init_process`, `exit_process`)
/// stay null until Slice 6 wires the driver factory and threading;
/// until then the module configures, chains filters, and declines
/// every request (unoptimized passthrough).
pub static mut NGX_PAGE_SPEED: ngx_module_t = ngx_module_t {
    ctx_index: MODULE_UNSET_INDEX,
    index: MODULE_UNSET_INDEX,
    name: ptr::null_mut(),
    spare0: 0,
    spare1: 0,
    version: nginx_version as ngx_uint_t,
    signature: MODULE_SIGNATURE.as_ptr(),
    ctx: &raw mut PS_HTTP_MODULE as *mut c_void,
    commands: &raw mut PS_COMMANDS as *mut ngx_command_t,
    type_: NGX_HTTP_MODULE as ngx_uint_t,
    init_master: None,
    init_module: None,
    init_process: None,
    init_thread: None,
    exit_thread: None,
    exit_process: None,
    exit_master: None,
    spare_hook0: 0,
    spare_hook1: 0,
    spare_hook2: 0,
    spare_hook3: 0,
    spare_hook4: 0,
    spare_hook5: 0,
    spare_hook6: 0,
    spare_hook7: 0,
};

/// Etag filter module (`ngx_pagespeed_etag_filter`).
pub static mut NGX_ETAG_FILTER: ngx_module_t = ngx_module_t {
    ctx_index: MODULE_UNSET_INDEX,
    index: MODULE_UNSET_INDEX,
    name: ptr::null_mut(),
    spare0: 0,
    spare1: 0,
    version: nginx_version as ngx_uint_t,
    signature: MODULE_SIGNATURE.as_ptr(),
    ctx: &raw mut PS_ETAG_MODULE as *mut c_void,
    commands: ptr::null_mut(),
    type_: NGX_HTTP_MODULE as ngx_uint_t,
    init_master: None,
    init_module: None,
    init_process: None,
    init_thread: None,
    exit_thread: None,
    exit_process: None,
    exit_master: None,
    spare_hook0: 0,
    spare_hook1: 0,
    spare_hook2: 0,
    spare_hook3: 0,
    spare_hook4: 0,
    spare_hook5: 0,
    spare_hook6: 0,
    spare_hook7: 0,
};

// ---------------------------------------------------------------------------
// Conf-context accessors (hand-expanded `ngx_http_*_get_module_*_conf` macros)
// ---------------------------------------------------------------------------

/// Reads our main conf from a conf context (`..._MAIN_CONF`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` whose `ctx` is a live
/// `ngx_http_conf_ctx_t`; the module must be loaded (index set).
pub unsafe fn main_conf_of(cf: *mut ngx_conf_t) -> *mut MainConf {
    // SAFETY: caller guarantees as documented; `ctx_index` was
    // assigned by nginx at load.
    unsafe {
        let ctx = (*cf).ctx as *mut ngx_http_conf_ctx_t;
        let index = NGX_PAGE_SPEED.ctx_index as usize;
        *(*ctx).main_conf.add(index) as *mut MainConf
    }
}

/// Reads our server conf (`..._SRV_CONF`).
///
/// # Safety
///
/// As in `main_conf_of`.
pub unsafe fn srv_conf_of(cf: *mut ngx_conf_t) -> *mut SrvConf {
    // SAFETY: as in `main_conf_of`.
    unsafe {
        let ctx = (*cf).ctx as *mut ngx_http_conf_ctx_t;
        let index = NGX_PAGE_SPEED.ctx_index as usize;
        *(*ctx).srv_conf.add(index) as *mut SrvConf
    }
}

/// Reads our location conf (`..._LOC_CONF`).
///
/// # Safety
///
/// As in `main_conf_of`.
pub unsafe fn loc_conf_of(cf: *mut ngx_conf_t) -> *mut LocConf {
    // SAFETY: as in `main_conf_of`.
    unsafe {
        let ctx = (*cf).ctx as *mut ngx_http_conf_ctx_t;
        let index = NGX_PAGE_SPEED.ctx_index as usize;
        *(*ctx).loc_conf.add(index) as *mut LocConf
    }
}

// ---------------------------------------------------------------------------
// Hooks (FFI bodies; live-tested in Slice 7)
// ---------------------------------------------------------------------------

/// Preconfiguration (`ps_pre_init`): arms the gzip setter (Slice 6).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`.
pub unsafe extern "C" fn preconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
    // Slice 6: `g_gzip_setter.Init(cf)` (config-time gzip scan).
    NGX_OK as ngx_int_t
}

/// Pushes a phase handler (`ngx_array_push` + store).
///
/// # Safety
///
/// `cmcf` must point at a live core main conf; `phase` must be a
/// valid phase index.
unsafe fn push_phase_handler(
    cmcf: *mut ngx_http_core_main_conf_t,
    phase: usize,
    handler: ngx_http_handler_pt,
) -> ngx_int_t {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let slot = ngx_array_push(
            &raw mut (*cmcf).phases[phase].handlers,
        ) as *mut ngx_http_handler_pt;
        if slot.is_null() {
            return NGX_ERROR as ngx_int_t;
        }
        *slot = handler;
    }
    NGX_OK as ngx_int_t
}

/// Postconfiguration (`ps_init`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in postconfiguration.
pub unsafe extern "C" fn postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
    // SAFETY: caller guarantees a live conf.
    unsafe {
        let cfg = main_conf_of(cf);
        let plan = postconfiguration_plan(!(*cfg).factory.generation.is_none());
        if !plan.install_filters {
            return NGX_OK as ngx_int_t;
        }
        // Init order is load-bearing (see `filters`).
        filters::in_place::init();
        filters::fix_headers::init();
        filters::base_fetch::init();
        filters::html_rewrite::init();
        let ctx = (*cf).ctx as *mut ngx_http_conf_ctx_t;
        // Core module index 0 is stable: `ngx_http_core_module`
        // is always the first http module.
        let cmcf = *(*ctx).main_conf as *mut ngx_http_core_main_conf_t;
        if push_phase_handler(cmcf, CONTENT_PHASE as usize, Some(preaccess_handler))
            != NGX_OK as ngx_int_t
        {
            return NGX_ERROR as ngx_int_t;
        }
        if push_phase_handler(cmcf, POST_READ_PHASE as usize, Some(self_fetch_handler))
            != NGX_OK as ngx_int_t
        {
            return NGX_ERROR as ngx_int_t;
        }
    }
    NGX_OK as ngx_int_t
}

/// Etag postconfiguration (`ps_etag_filter_init`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in postconfiguration.
pub unsafe extern "C" fn etag_postconfiguration(cf: *mut ngx_conf_t) -> ngx_int_t {
    // SAFETY: caller guarantees a live conf.
    unsafe {
        let cfg = main_conf_of(cf);
        if !cfg.is_null() && etag_plan(!(*cfg).factory.generation.is_none()) {
            filters::etag::init();
        }
    }
    NGX_OK as ngx_int_t
}

/// Registers a pool cleanup (`ps_set_conf_cleanup_handler`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`; `data` must stay
/// valid until the cleanup runs.
unsafe fn set_cleanup(
    cf: *mut ngx_conf_t,
    handler: Option<unsafe extern "C" fn(*mut c_void)>,
    data: *mut c_void,
) {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let cleanup = ngx_pool_cleanup_add((*cf).pool, 0);
        if cleanup.is_null() {
            use ngx::ffi::NGX_LOG_ERR;
            ngx::ngx_conf_log_error!(NGX_LOG_ERR, cf, "failed to register a cleanup handler");
        } else {
            (*cleanup).handler = handler;
            (*cleanup).data = data;
        }
    }
}

/// Drops a boxed conf at pool cleanup.
unsafe extern "C" fn drop_main_conf(data: *mut c_void) {
    // SAFETY: `data` came from `Box::into_raw` below; the pool
    // runs cleanups once.
    unsafe {
        drop(Box::from_raw(data as *mut MainConf));
    }
}

/// Drops a boxed server conf at pool cleanup.
unsafe extern "C" fn drop_srv_conf(data: *mut c_void) {
    // SAFETY: as in `drop_main_conf`.
    unsafe {
        drop(Box::from_raw(data as *mut SrvConf));
    }
}

/// Drops a boxed location conf at pool cleanup.
unsafe extern "C" fn drop_loc_conf(data: *mut c_void) {
    // SAFETY: as in `drop_main_conf`.
    unsafe {
        drop(Box::from_raw(data as *mut LocConf));
    }
}

/// Creates the main conf (`ps_create_main_conf`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`.
pub unsafe extern "C" fn create_main_conf(cf: *mut ngx_conf_t) -> *mut c_void {
    // Slice 6 owns the factory (`InitApr`, `NgxRewriteDriverFactory`,
    // `LoggingInit`, `Init`); until then the slot records presence
    // only. Note the C++ creates the factory eagerly here and
    // `ps_init_module` retargets the active pointer later.
    let conf = Box::new(MainConf::default());
    let raw = Box::into_raw(conf);
    // SAFETY: live conf; the box outlives the pool via cleanup.
    unsafe {
        set_cleanup(cf, Some(drop_main_conf), raw as *mut c_void);
    }
    raw as *mut c_void
}

/// Creates the server conf (`ps_create_srv_conf`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`.
pub unsafe extern "C" fn create_srv_conf(cf: *mut ngx_conf_t) -> *mut c_void {
    let conf = Box::new(SrvConf::default());
    let raw = Box::into_raw(conf);
    // SAFETY: live conf; the box outlives the pool via cleanup.
    unsafe {
        set_cleanup(cf, Some(drop_srv_conf), raw as *mut c_void);
    }
    raw as *mut c_void
}

/// Creates the location conf (`ps_create_loc_conf`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`.
pub unsafe extern "C" fn create_loc_conf(cf: *mut ngx_conf_t) -> *mut c_void {
    let conf = Box::new(LocConf::default());
    let raw = Box::into_raw(conf);
    // SAFETY: live conf; the box outlives the pool via cleanup.
    unsafe {
        set_cleanup(cf, Some(drop_loc_conf), raw as *mut c_void);
    }
    raw as *mut c_void
}

/// Looks up the `http2` variable index (`ngx_http_get_variable_index`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`.
unsafe fn http2_variable_index(cf: *mut ngx_conf_t) -> ngx_int_t {
    use ngx::ffi::{ngx_http_get_variable_index, ngx_str_t};
    static NAME: &[u8; 6] = b"http2\0";
    let mut name = ngx_str_t { len: 5, data: NAME.as_ptr() as *mut u8 };
    // SAFETY: live conf; static name outlives the call.
    unsafe { ngx_http_get_variable_index(cf, &mut name) }
}

/// Merges server confs (`ps_merge_srv_conf`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`; `parent`/`child`
/// must point at live `SrvConf`s.
pub unsafe extern "C" fn merge_srv_conf(
    cf: *mut ngx_conf_t,
    parent: *mut c_void,
    child: *mut c_void,
) -> *mut std::ffi::c_char {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let cfg_m = &mut *main_conf_of(cf);
        let parent_opts = (*(parent as *mut SrvConf)).options.as_ref();
        let srv = &mut *(child as *mut SrvConf);
        let http2 = http2_variable_index(cf);
        match conf::merge_srv_conf(cfg_m, parent_opts, srv, http2, |p| {
            std::path::Path::new(p).is_dir()
        }) {
            Ok(_) => NGX_CONF_OK,
            Err(text) => {
                // Static verbatim texts; nginx only reads them.
                leak_static_cstring(text)
            }
        }
    }
}

/// Merges location confs (`ps_merge_loc_conf`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`; `parent`/`child`
/// must point at live `LocConf`s.
pub unsafe extern "C" fn merge_loc_conf(
    cf: *mut ngx_conf_t,
    parent: *mut c_void,
    child: *mut c_void,
) -> *mut std::ffi::c_char {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let parent_opts = (*(parent as *mut LocConf)).options.as_ref();
        let loc = &mut *(child as *mut LocConf);
        let srv = &*srv_conf_of(cf);
        // `ps_disabled`: no context, or an unplugged one.
        let disabled = srv
            .context
            .as_ref()
            .is_none_or(|ctx| ctx.options.unplugged());
        conf::merge_loc_conf(parent_opts, loc, srv_context_options(srv), disabled);
        NGX_CONF_OK
    }
}

/// Returns the server's global options, if the context seeded.
fn srv_context_options(srv: &SrvConf) -> Option<&NgxOptions> {
    srv.context.as_ref().map(|ctx| &ctx.options)
}

/// Leaks a static error string as nginx expects (`char*` it reads).
fn leak_static_cstring(text: &'static str) -> *mut std::ffi::c_char {
    // The three merge texts are `'static`; leak one copy each —
    // config time only, bounded.
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let leaked = Box::leak(bytes.into_boxed_slice());
    leaked.as_mut_ptr() as *mut std::ffi::c_char
}

/// Reads directive words from `cf->args`, skipping `pagespeed`.
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in a directive handler.
unsafe fn directive_args(cf: *mut ngx_conf_t) -> Vec<String> {
    use ngx::ffi::{ngx_array_t, ngx_str_t};
    // SAFETY: caller guarantees a live conf in a directive handler.
    unsafe {
        let args = (*cf).args as *mut ngx_array_t;
        let elts = (*args).elts as *mut ngx_str_t;
        let nelts = (*args).nelts;
        let mut out = Vec::new();
        for i in 1..nelts {
            let word = &*elts.add(i);
            let bytes = std::slice::from_raw_parts(word.data, word.len);
            out.push(String::from_utf8_lossy(bytes).into_owned());
        }
        out
    }
}

/// Runs one `pagespeed` line at main scope (`ps_main_configure`).
///
/// # Safety
///
/// Standard directive-handler contract: live `cf`.
pub unsafe extern "C" fn ps_main_configure(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut std::ffi::c_char {
    // SAFETY: directive-handler contract.
    unsafe { run_directive_line(cf, Scope::ProcessStrict, true) }
}

/// Runs one `pagespeed` line at server scope (`ps_srv_configure`).
///
/// # Safety
///
/// Standard directive-handler contract: live `cf`.
pub unsafe extern "C" fn ps_srv_configure(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut std::ffi::c_char {
    // SAFETY: directive-handler contract.
    unsafe { run_directive_line(cf, Scope::Server, true) }
}

/// Runs one `pagespeed` line at location scope (`ps_loc_configure`).
///
/// # Safety
///
/// Standard directive-handler contract: live `cf`.
pub unsafe extern "C" fn ps_loc_configure(
    cf: *mut ngx_conf_t,
    _cmd: *mut ngx_command_t,
    _conf: *mut c_void,
) -> *mut std::ffi::c_char {
    // SAFETY: directive-handler contract.
    unsafe { run_directive_line(cf, Scope::Directory, false) }
}

/// Shared directive-line runner (`ps_configure`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in a directive handler.
unsafe fn run_directive_line(
    cf: *mut ngx_conf_t,
    scope: Scope,
    server_level: bool,
) -> *mut std::ffi::c_char {
    // SAFETY: caller guarantees a live conf.
    unsafe {
        let words = directive_args(cf);
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let main = &mut *main_conf_of(cf);
        // The main block routes through the server conf's options
        // slot, exactly like the C++ (all three entries share the
        // server offset); locations use their own slot.
        let outcome = if server_level {
            let srv = &mut *srv_conf_of(cf);
            apply_directive_line(&mut srv.options, &mut main.factory_options, scope, &refs)
        } else {
            let loc = &mut *loc_conf_of(cf);
            apply_directive_line(&mut loc.options, &mut main.factory_options, scope, &refs)
        };
        match outcome {
            Ok(applied) => {
                // Slice 6 executes the gzip call.
                let _ = applied.gzip;
                if let Some(prep) = applied.dir_prep {
                    let ccf = core_conf_of(cf);
                    if let Err(text) = crate::directives::fix_dir_ownership(
                        &prep.directive,
                        &prep.path,
                        (*ccf).user,
                        (*ccf).group,
                    ) {
                        let pool = (*cf).pool;
                        let rendered = crate::directives::pool_cstring(pool, &text);
                        if rendered.is_null() {
                            return NGX_CONF_ERROR;
                        }
                        return rendered;
                    }
                }
                // `$ps_dollar` when script variables are on.
                let mode = (*main_conf_of(cf)).factory_options.script_variables;
                if directives::should_register_dollar(mode)
                    && register_dollar_variable(cf).is_null()
                {
                    return DOLLAR_ERROR.as_ptr() as *mut std::ffi::c_char;
                }
                NGX_CONF_OK
            }
            Err(text) => {
                let pool = (*cf).pool;
                let rendered = crate::directives::pool_cstring(pool, &text);
                if rendered.is_null() {
                    NGX_CONF_ERROR
                } else {
                    rendered
                }
            }
        }
    }
}

/// Verbatim `$ps_dollar` registration failure (`ps_configure`).
static DOLLAR_ERROR: &[u8; 61] = b"Failed to add global configuration variable for '$ps_dollar'\0";

/// Reads the core conf (`ngx_get_conf(cycle->conf_ctx, core)`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in a directive handler.
unsafe fn core_conf_of(cf: *mut ngx_conf_t) -> *mut ngx::ffi::ngx_core_conf_t {
    use ngx::ffi::ngx_core_module;
    // SAFETY: caller guarantees a live conf; core index assigned.
    unsafe {
        let cycle = (*cf).cycle;
        let index = ngx_core_module.index as usize;
        *(*cycle).conf_ctx.add(index) as *mut ngx::ffi::ngx_core_conf_t
    }
}

/// Registers the `$ps_dollar` variable (`ps_configure`).
///
/// Returns null on failure.
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in a directive handler.
unsafe fn register_dollar_variable(cf: *mut ngx_conf_t) -> *mut ngx::ffi::ngx_http_variable_t {
    use ngx::ffi::{ngx_http_add_variable, NGX_HTTP_VAR_CHANGEABLE};
    static NAME: &[u8; 10] = b"ps_dollar\0";
    // SAFETY: caller guarantees a live conf.
    unsafe {
        let mut name = ngx_str_t { len: 9, data: NAME.as_ptr() as *mut u8 };
        let var = ngx_http_add_variable(cf, &mut name, NGX_HTTP_VAR_CHANGEABLE as ngx_uint_t);
        if !var.is_null() {
            (*var).get_handler = Some(crate::directives::ps_dollar);
        }
        var
    }
}

/// Preaccess/content-phase handler (`ps_preaccess_handler`).
///
/// # Safety
///
/// `r` must point at a live request in phase processing.
pub unsafe extern "C" fn preaccess_handler(r: *mut ngx_http_request_t) -> ngx_int_t {
    use ngx::ffi::ngx_http_core_module;
    // SAFETY: caller guarantees a live request in phase processing.
    unsafe {
        let core_index = ngx_http_core_module.ctx_index as usize;
        let cmcf = *(*r).main_conf.add(core_index) as *mut ngx_http_core_main_conf_t;
        let at = (*r).phase_handler as usize;
        // Same two lines as `insert_phase_step`, over the raw
        // engine slot (the engine array carries no length).
        let slot = (*cmcf).phase_engine.handlers.add(at);
        *slot = phase_step_slot(at);
        (*r).phase_handler = (at - 1) as ngx_int_t;
    }
    NGX_DECLINED as ngx_int_t
}

/// Request content stub (Slice 2 installs `ps_content_handler`).
///
/// # Safety
///
/// `r` must point at a live request.
unsafe fn content_stub(_r: *mut ngx_http_request_t) -> ngx_int_t {
    NGX_DECLINED as ngx_int_t
}

/// Phase step handler (`ps_phase_handler`).
///
/// # Safety
///
/// `r` must point at a live request; `ph` the current step.
pub unsafe extern "C" fn phase_handler(
    r: *mut ngx_http_request_t,
    _ph: *mut ngx_http_phase_handler_t,
) -> ngx_int_t {
    use ngx::ffi::{ngx_http_core_run_phases, ngx_http_finalize_request, ngx_http_request_empty_handler};
    // SAFETY: caller guarantees a live request.
    unsafe {
        (*r).write_event_handler = Some(ngx_http_request_empty_handler);
        let rc = content_stub(r);
        if rc == NGX_DECLINED as ngx_int_t {
            (*r).write_event_handler = Some(ngx_http_core_run_phases);
            (*r).phase_handler += 1;
            return NGX_AGAIN as ngx_int_t;
        }
        ngx_http_finalize_request(r, rc);
    }
    NGX_OK as ngx_int_t
}

/// Post-read self-fetch handler stub (`ps_self_fetch_post_read_handler`).
///
/// Slice 2 installs the subrequest check plus relabel/restore;
/// until then every request declines.
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe extern "C" fn self_fetch_handler(r: *mut ngx_http_request_t) -> ngx_int_t {
    let _ = r;
    NGX_DECLINED as ngx_int_t
}

// ---------------------------------------------------------------------------
// Dynamic-module export table (hand-written `ngx_modules!`)
// ---------------------------------------------------------------------------

// The `ngx::ngx_modules!` macro's `count!` helper does not expand
// on this toolchain, so the three tables nginx looks up when
// loading the `.so` are spelled out (`ngx_modules`,
// `ngx_module_names`, `ngx_module_order`).

/// Exported module pointers, null-terminated.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut ngx_modules: [*const ngx_module_t; 3] = [
    &raw const NGX_PAGE_SPEED,
    &raw const NGX_ETAG_FILTER,
    ptr::null(),
];

/// Exported module names, null-terminated.
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut ngx_module_names: [*const std::ffi::c_char; 3] = [
    c"NGX_PAGE_SPEED".as_ptr(),
    c"NGX_ETAG_FILTER".as_ptr(),
    ptr::null(),
];

/// Exported module order (empty).
#[no_mangle]
#[allow(non_upper_case_globals)]
pub static mut ngx_module_order: [*const std::ffi::c_char; 1] = [ptr::null()];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bound_nginx_version;
    use std::mem::MaybeUninit;

    #[test]
    fn command_table_shape() {
        assert_eq!(3, COMMAND_SPECS.len());
        assert_eq!(NGX_HTTP_MAIN_CONF as ngx_uint_t, COMMAND_SPECS[0].0);
        assert_eq!(NGX_HTTP_SRV_CONF as ngx_uint_t, COMMAND_SPECS[1].0);
        assert_eq!(
            NGX_HTTP_LOC_CONF as ngx_uint_t | NGX_HTTP_LIF_CONF as ngx_uint_t,
            COMMAND_SPECS[2].0
        );
        // TAKE1..TAKE5 all set (MULTI is 0 on modern nginx).
        for take in [
            NGX_CONF_TAKE1,
            NGX_CONF_TAKE2,
            NGX_CONF_TAKE3,
            NGX_CONF_TAKE4,
            NGX_CONF_TAKE5,
        ] {
            assert_ne!(0, ARG_MASK & take as ngx_uint_t);
        }
        // All three entries share the server offset (verbatim,
        // including the upstream TODO questioning it): 8 is the
        // second of three pointers on 64-bit.
        assert_eq!(8, srv_conf_offset());
        // The live table matches the spec: names, masks, null end.
        // SAFETY: read-only access to our own static; single-threaded
        // test with no writer.
        #[allow(static_mut_refs)]
        unsafe {
            for (i, (mask, _)) in COMMAND_SPECS.iter().enumerate() {
                let cmd = &*PS_COMMANDS.as_ptr().add(i);
                assert_eq!(9, cmd.name.len);
                let bytes = std::slice::from_raw_parts(cmd.name.data, cmd.name.len);
                assert_eq!(b"pagespeed", bytes);
                assert_eq!(*mask | ARG_MASK, cmd.type_);
                assert!(cmd.set.is_some());
            }
            let null = &*PS_COMMANDS.as_ptr().add(3);
            assert_eq!(0, null.name.len);
            assert!(null.name.data.is_null());
            assert_eq!(0, null.type_);
            assert!(null.set.is_none());
        }
    }

    #[test]
    fn postconfiguration_gating() {
        let on = postconfiguration_plan(true);
        assert!(on.install_filters && on.push_content_phase && on.push_post_read);
        let off = postconfiguration_plan(false);
        assert!(!off.install_filters && !off.push_content_phase && !off.push_post_read);
        assert!(etag_plan(true));
        assert!(!etag_plan(false));
        assert_eq!(8, CONTENT_PHASE);
        assert_eq!(0, POST_READ_PHASE);
        // The PRECONTENT branch assumes nginx ≥ 1.13.4.
        assert!(bound_nginx_version() >= PRECONTENT_FLOOR);
    }

    #[test]
    fn phase_insertion_overwrites_and_steps_back() {
        // SAFETY: fabricated engine array; no nginx involved.
        unsafe {
            let blank = || MaybeUninit::<ngx_http_phase_handler_t>::zeroed().assume_init();
            let mut engine = [blank(), blank(), blank()];
            let at = insert_phase_step(&mut engine, 1, phase_step_slot(1));
            assert_eq!(0, at);
            assert!(engine[1].checker.is_some());
            assert!(engine[1].handler.is_none());
            assert_eq!(2, engine[1].next);
            // Neighbors untouched (no shifting on modern nginx).
            assert!(engine[0].checker.is_none());
            assert!(engine[2].checker.is_none());
        }
    }

    #[test]
    fn signature_matches_generated_bits() {
        use ngx::ffi::{
            NGX_MODULE_SIGNATURE_1, NGX_MODULE_SIGNATURE_10, NGX_MODULE_SIGNATURE_11,
            NGX_MODULE_SIGNATURE_12, NGX_MODULE_SIGNATURE_13, NGX_MODULE_SIGNATURE_14,
            NGX_MODULE_SIGNATURE_15, NGX_MODULE_SIGNATURE_16, NGX_MODULE_SIGNATURE_17,
            NGX_MODULE_SIGNATURE_18, NGX_MODULE_SIGNATURE_19, NGX_MODULE_SIGNATURE_2,
            NGX_MODULE_SIGNATURE_20, NGX_MODULE_SIGNATURE_21, NGX_MODULE_SIGNATURE_22,
            NGX_MODULE_SIGNATURE_23, NGX_MODULE_SIGNATURE_24, NGX_MODULE_SIGNATURE_25,
            NGX_MODULE_SIGNATURE_26, NGX_MODULE_SIGNATURE_27, NGX_MODULE_SIGNATURE_28,
            NGX_MODULE_SIGNATURE_29, NGX_MODULE_SIGNATURE_3, NGX_MODULE_SIGNATURE_30,
            NGX_MODULE_SIGNATURE_31, NGX_MODULE_SIGNATURE_32, NGX_MODULE_SIGNATURE_33,
            NGX_MODULE_SIGNATURE_34, NGX_MODULE_SIGNATURE_4, NGX_MODULE_SIGNATURE_5,
            NGX_MODULE_SIGNATURE_6, NGX_MODULE_SIGNATURE_7, NGX_MODULE_SIGNATURE_8,
            NGX_MODULE_SIGNATURE_9,
        };
        let bits = [
            NGX_MODULE_SIGNATURE_1, NGX_MODULE_SIGNATURE_2, NGX_MODULE_SIGNATURE_3,
            NGX_MODULE_SIGNATURE_4, NGX_MODULE_SIGNATURE_5, NGX_MODULE_SIGNATURE_6,
            NGX_MODULE_SIGNATURE_7, NGX_MODULE_SIGNATURE_8, NGX_MODULE_SIGNATURE_9,
            NGX_MODULE_SIGNATURE_10, NGX_MODULE_SIGNATURE_11, NGX_MODULE_SIGNATURE_12,
            NGX_MODULE_SIGNATURE_13, NGX_MODULE_SIGNATURE_14, NGX_MODULE_SIGNATURE_15,
            NGX_MODULE_SIGNATURE_16, NGX_MODULE_SIGNATURE_17, NGX_MODULE_SIGNATURE_18,
            NGX_MODULE_SIGNATURE_19, NGX_MODULE_SIGNATURE_20, NGX_MODULE_SIGNATURE_21,
            NGX_MODULE_SIGNATURE_22, NGX_MODULE_SIGNATURE_23, NGX_MODULE_SIGNATURE_24,
            NGX_MODULE_SIGNATURE_25, NGX_MODULE_SIGNATURE_26, NGX_MODULE_SIGNATURE_27,
            NGX_MODULE_SIGNATURE_28, NGX_MODULE_SIGNATURE_29, NGX_MODULE_SIGNATURE_30,
            NGX_MODULE_SIGNATURE_31, NGX_MODULE_SIGNATURE_32, NGX_MODULE_SIGNATURE_33,
            NGX_MODULE_SIGNATURE_34,
        ];
        let mut expected = format!(
            "{},{},{},",
            mem::size_of::<usize>(),
            mem::size_of::<libc::c_int>(),
            mem::size_of::<libc::time_t>()
        );
        for bit in bits {
            expected.push_str(bit.to_str().unwrap());
        }
        assert_eq!(expected, MODULE_SIGNATURE.to_str().unwrap());
        assert_eq!(ngx_uint_t::MAX, MODULE_UNSET_INDEX);
    }

    #[test]
    fn directive_lines_dispatch() {
        use ps_config::options::EnabledState;
        let mut factory = NginxFactoryOptions::default();
        // Master switch reports the gzip call AND parses the state.
        let mut options: Option<NgxOptions> = None;
        let out = apply_directive_line(&mut options, &mut factory, Scope::Server, &["on"]).unwrap();
        assert_eq!(GzipCall::EnableForLocation, out.gzip);
        assert_eq!(EnabledState::On, options.as_ref().unwrap().core.enabled_state());
        let out =
            apply_directive_line(&mut options, &mut factory, Scope::Server, &["off"]).unwrap();
        assert_eq!(GzipCall::SetForLocation(false), out.gzip);
        assert_eq!(
            EnabledState::Unplugged,
            options.as_ref().unwrap().core.enabled_state()
        );
        // `gzip` returns after the setter call; options untouched.
        let out = apply_directive_line(
            &mut options,
            &mut factory,
            Scope::Server,
            &["gzip", "off"],
        )
        .unwrap();
        assert_eq!(GzipCall::SetForLocation(false), out.gzip);
        assert_eq!(
            EnabledState::Unplugged,
            options.as_ref().unwrap().core.enabled_state()
        );
        assert_eq!(
            Err("Invalid pagespeed gzip setting".to_string()),
            apply_directive_line(&mut options, &mut factory, Scope::Server, &["gzip", "maybe"])
        );
        // Option line creates options and parses.
        let out = apply_directive_line(
            &mut options,
            &mut factory,
            Scope::Server,
            &["RewriteLevel", "CoreFilters"],
        )
        .unwrap();
        assert_eq!(GzipCall::None, out.gzip);
        assert!(out.dir_prep.is_none());
        assert!(options.is_some());
        // ClearInheritedScripts flags the level object.
        apply_directive_line(&mut options, &mut factory, Scope::Server, &["ClearInheritedScripts"])
            .unwrap();
        assert!(options.as_ref().unwrap().clear_inherited_scripts);
        // Over-arity rejected.
        let many = ["a"; 11];
        assert!(apply_directive_line(&mut options, &mut factory, Scope::Server, &many).is_err());
    }

    #[test]
    fn dir_prep_line_reports_path() {
        let mut factory = NginxFactoryOptions::default();
        let mut options: Option<NgxOptions> = None;
        let base = std::env::temp_dir().join(format!("ps-nginx-line-{}", std::process::id()));
        let path = base.join("cache").to_str().unwrap().to_string();
        let out = apply_directive_line(
            &mut options,
            &mut factory,
            Scope::Server,
            &["FileCachePath", &path],
        )
        .unwrap();
        assert_eq!(
            Some(DirPrepPath { directive: "FileCachePath".to_string(), path: path.clone() }),
            out.dir_prep
        );
        assert_eq!(path, options.unwrap().system.file_cache_path);
        assert!(std::path::Path::new(&path).is_dir());
        std::fs::remove_dir_all(&base).unwrap();
        // Relative path: creation-half error text.
        let mut options: Option<NgxOptions> = None;
        assert_eq!(
            Err("FileCachePath relative must start with a slash".to_string()),
            apply_directive_line(&mut options, &mut factory, Scope::Server, &["FileCachePath", "relative"])
        );
    }
}


/// Ensures a level options object exists (`*options == NULL` → new).
fn ensure_options(options: &mut Option<NgxOptions>) {
    if options.is_none() {
        *options = Some(NgxOptions::new());
    }
}

/// Gzip-setter call the line requests (`g_gzip_setter`, Slice 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GzipCall {
    /// No gzip interplay.
    None,
    /// `EnableGZipForLocation(cf)`.
    EnableForLocation,
    /// `SetGZipForLocation(cf, value)`.
    SetForLocation(bool),
}
