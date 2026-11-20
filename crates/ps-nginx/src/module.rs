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

use crate::conf::{self, FactorySlot, LocConf, MainConf, NgxOptions, SrvConf};
use crate::directives::{self, Directive};
use crate::factory::{
    apply_factory_directive_1, apply_factory_directive_2, DriverFactory, FactoryDirectiveOutcome,
    PostConfigContext, post_config, remote_config_warning,
};
use crate::filters;
use crate::routing::{self, ContentAction, RoutingInputs, STATIC_ASSET_PREFIX};
use crate::self_fetch;
use ngx::ffi::{
    ngx_array_push, ngx_command_t, ngx_conf_t, ngx_connection_local_sockaddr, ngx_cycle_t,
    ngx_http_conf_ctx_t, ngx_http_core_loc_conf_t, ngx_http_core_main_conf_t,
    ngx_http_core_srv_conf_t, ngx_http_handler_pt, ngx_http_module_t, ngx_http_phase_handler_t,
    ngx_http_phases_NGX_HTTP_POST_READ_PHASE, ngx_http_phases_NGX_HTTP_PRECONTENT_PHASE,
    ngx_http_request_t, ngx_int_t, ngx_module_t, ngx_pool_cleanup_add, ngx_str_t, ngx_table_elt_t,
    ngx_uint_t, NGX_AGAIN, NGX_CONF_TAKE1, NGX_CONF_TAKE2, NGX_CONF_TAKE3, NGX_CONF_TAKE4,
    NGX_CONF_TAKE5, NGX_DECLINED, NGX_DONE, NGX_ERROR, NGX_HTTP_INTERNAL_SERVER_ERROR,
    NGX_HTTP_LIF_CONF, NGX_HTTP_LOC_CONF, NGX_HTTP_MAIN_CONF, NGX_HTTP_MODULE, NGX_HTTP_SRV_CONF,
    NGX_OK, nginx_version,
};
use ngx::core::{NGX_CONF_ERROR, NGX_CONF_OK};
use ps_config::message_handler::MessageLevel;
use ps_config::options::{apply_nginx_directive, check_nginx_scope, nginx_config_error, SetOutcome};
use ps_config::property::strip_mod_pagespeed_prefix;
use ps_config::scope::Scope;
use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};

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
// Init-module plan (pure core of `ps_init_module`)
// ---------------------------------------------------------------------------

/// Reload-guard text (logged, non-fatal: the cycle stays down).
pub const RELOAD_UNSUPPORTED: &str = "ngx_pagespeed: reload with master_process off is not \
    supported; optimization will stay off until nginx is restarted";

/// Missing-resolver text (fatal).
pub const RESOLVER_REQUIRED: &str = "UseNativeFetcher is on, please configure a resolver.";

/// PostConfig error prefix (handler report).
pub const POST_CONFIG_PREFIX: &str = "ngx_pagespeed is enabled. ";

/// One server's remote-config inputs (warning loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteWarningInput {
    /// Remote-configuration URL (empty when unset).
    pub url: String,
    /// Whether an external (non-blocking) cache is configured.
    pub external_cache: bool,
}

/// Init-module inputs (`ps_init_module` past the null check).
pub struct InitModuleInputs<'a> {
    /// Cycle factory (resolver already recorded).
    pub factory: &'a DriverFactory,
    /// Collected server contexts.
    pub contexts: &'a [PostConfigContext<'a>],
    /// First cycle in this process.
    pub initial_cycle: bool,
    /// Single-process mode (`master_process off`).
    pub single_process: bool,
    /// Configuration test (`nginx -t`).
    pub test_config: bool,
    /// Remote-config inputs, one per server with a URL.
    pub remotes: &'a [RemoteWarningInput],
}

/// Init-module decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitModuleOutcome {
    /// Reload refused: log and return OK without bringing up.
    pub reload_unsupported: bool,
    /// Fatal error text (already prefixed), if the cycle fails.
    pub error: Option<String>,
    /// Whether any server context exists (else delete factory).
    pub has_contexts: bool,
    /// Whether to init null statistics (no shared-mem stats).
    pub init_null_stats: bool,
    /// Whether to run `RootInit` (skipped for config tests).
    pub root_init: bool,
    /// Remote-config warnings, one per affected server.
    pub warnings: Vec<String>,
}

/// Plans module init (`ps_init_module` past context collection):
/// the reload guard first, then PostConfig; empty cycles delete
/// the factory and skip everything else, otherwise the resolver
/// check and the remote warnings run. Shared-memory statistics
/// setup and `RootInit` run live in later slices; the flags land
/// now.
#[must_use]
pub fn init_module_plan(inputs: InitModuleInputs) -> InitModuleOutcome {
    if !inputs.initial_cycle && inputs.single_process {
        return InitModuleOutcome {
            reload_unsupported: true,
            error: None,
            has_contexts: false,
            init_null_stats: false,
            root_init: false,
            warnings: Vec::new(),
        };
    }
    let post = post_config(inputs.factory, inputs.contexts);
    if let Some((_, message)) = post.error {
        return InitModuleOutcome {
            reload_unsupported: false,
            error: Some(format!("{POST_CONFIG_PREFIX}{message}")),
            has_contexts: !inputs.contexts.is_empty(),
            init_null_stats: false,
            root_init: false,
            warnings: Vec::new(),
        };
    }
    // Empty cycles delete the factory and skip the resolver
    // block, stats init, and `RootInit` entirely.
    if inputs.contexts.is_empty() {
        return InitModuleOutcome {
            reload_unsupported: false,
            error: None,
            has_contexts: false,
            init_null_stats: false,
            root_init: false,
            warnings: Vec::new(),
        };
    }
    if !inputs.factory.check_resolver() {
        return InitModuleOutcome {
            reload_unsupported: false,
            error: Some(RESOLVER_REQUIRED.to_string()),
            has_contexts: !inputs.contexts.is_empty(),
            init_null_stats: false,
            root_init: false,
            warnings: Vec::new(),
        };
    }
    let mut warnings = Vec::new();
    for remote in inputs.remotes {
        if let Some(warning) = remote_config_warning(
            inputs.factory.factory_options.use_native_fetcher,
            &remote.url,
            remote.external_cache,
        ) {
            warnings.push(warning);
        }
    }
    InitModuleOutcome {
        reload_unsupported: false,
        error: None,
        has_contexts: !inputs.contexts.is_empty(),
        init_null_stats: !post.wants_global_statistics,
        root_init: !inputs.test_config,
        warnings,
    }
}

/// Whether child init proceeds (`ps_init_child_process` null
/// checks: a deleted factory is the port's null factory).
#[must_use]
pub const fn child_process_proceeds(factory_deleted: bool) -> bool {
    !factory_deleted
}

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
/// flag, then delegates option lines to ps-config with the
/// factory fallback. Returns the config error text on failure
/// (null/`NGX_CONF_OK` mapping and the ownership-fixup half
/// happen in the FFI wrapper).
pub fn apply_directive_line(
    options: &mut Option<NgxOptions>,
    factory: &mut DriverFactory,
    scope: Scope,
    args: &[&str],
) -> Result<LineOutcome, String> {
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
            let warnings = apply_option_or_factory(slot, factory, args[0], &[], scope)?;
            Ok(LineOutcome::with_warnings(gzip, warnings))
        }
        Directive::Gzip(value) => Ok(LineOutcome::gzip(GzipCall::SetForLocation(value))),
        Directive::InvalidGzip => Err("Invalid pagespeed gzip setting".to_string()),
        Directive::DirPrep => {
            let (name, path) = (args[0], args[1]);
            directives::prepare_dir(name, path)?;
            ensure_options(options);
            let slot = options.as_mut().expect("ensured above");
            let warnings = apply_option_or_factory(slot, factory, name, &args[1..], scope)?;
            Ok(LineOutcome {
                gzip: GzipCall::None,
                dir_prep: Some(DirPrepPath {
                    directive: name.to_string(),
                    path: path.to_string(),
                }),
                warnings,
            })
        }
        Directive::Options => {
            ensure_options(options);
            let slot = options.as_mut().expect("ensured above");
            if args.len() == 1 && args[0].eq_ignore_ascii_case("ClearInheritedScripts") {
                slot.clear_inherited_scripts = true;
            }
            let warnings = apply_option_or_factory(slot, factory, args[0], &args[1..], scope)?;
            Ok(LineOutcome::with_warnings(GzipCall::None, warnings))
        }
    }
}

/// Runs option dispatch with the factory fallback
/// (`ParseAndSetOptions` past the script block): the option
/// table first, the system factory on unknown names (one- and
/// two-arg lines only). Returns warn-and-ignore texts alongside.
fn apply_option_or_factory(
    slot: &mut NgxOptions,
    factory: &mut DriverFactory,
    directive: &str,
    args: &[&str],
    scope: Scope,
) -> Result<Vec<String>, String> {
    match apply_nginx_directive(
        &mut slot.core,
        &mut factory.factory_options,
        &mut slot.system,
        directive,
        args,
        scope,
    ) {
        Ok(()) => Ok(Vec::new()),
        Err(err) if err.outcome == SetOutcome::NameUnknown => {
            // Gate rejections bypass the factory (the C++ gate
            // returns before dispatch).
            if check_nginx_scope(directive, scope).is_err() {
                return Err(err.message);
            }
            let stripped = strip_mod_pagespeed_prefix(directive);
            let outcome = match args.len() {
                1 => apply_factory_directive_1(factory, stripped, args[0], scope),
                2 => apply_factory_directive_2(factory, stripped, args[0], args[1], scope),
                _ => FactoryDirectiveOutcome::NotFactory,
            };
            match outcome {
                FactoryDirectiveOutcome::Ok => Ok(Vec::new()),
                FactoryDirectiveOutcome::WarnIgnored(warning) => Ok(vec![warning]),
                // The wrap joins the raw words, prefix included.
                FactoryDirectiveOutcome::Invalid(detail) => {
                    let mut full = String::from(directive);
                    for arg in args {
                        full.push(' ');
                        full.push_str(arg);
                    }
                    Err(nginx_config_error(&full, &detail))
                }
                FactoryDirectiveOutcome::NotFactory => Err(err.message),
            }
        }
        Err(err) => Err(err.message),
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
    /// Gzip-setter call the line requests (Slice 7 executes).
    pub gzip: GzipCall,
    /// Directory needing ownership fixup, if any.
    pub dir_prep: Option<DirPrepPath>,
    /// Warn-and-ignore texts (the FFI wrapper logs them).
    pub warnings: Vec<String>,
}

impl LineOutcome {
    fn gzip(gzip: GzipCall) -> Self {
        LineOutcome::with_warnings(gzip, Vec::new())
    }

    fn with_warnings(gzip: GzipCall, warnings: Vec<String>) -> Self {
        LineOutcome { gzip, dir_prep: None, warnings }
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
/// run the factory lifecycle; thread startup, fetcher init, and
/// cache init inside them land with their slices (live-tested in
/// Slice 7).
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
    init_module: Some(init_module),
    init_process: Some(init_child_process),
    init_thread: None,
    exit_thread: None,
    exit_process: Some(exit_child_process),
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

/// Preconfiguration (`ps_pre_init`): arms the gzip setter.
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t`.
pub unsafe extern "C" fn preconfiguration(_cf: *mut ngx_conf_t) -> ngx_int_t {
    // Slice 7: `g_gzip_setter.Init(cf)` scans the live gzip
    // module's commands (no unit-testable core).
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
        // The factory is eagerly owned, so presence is the
        // deletion latch (`driver_factory != NULL` ⟺
        // `!factory_deleted`: the deleter sets both together).
        let plan = postconfiguration_plan(!(*cfg).factory.deleted);
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
        if !cfg.is_null() && etag_plan(!(*cfg).factory.deleted) {
            filters::etag::init();
        }
    }
    NGX_OK as ngx_int_t
}

// ---------------------------------------------------------------------------
// Module hooks (FFI bodies; live-tested in Slice 7)
// ---------------------------------------------------------------------------

/// Live factory generation (`active_driver_factory`, 0 = none).
static ACTIVE_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Generation counter for cycle factories.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Assigns the next factory generation (config time).
fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::SeqCst)
}

/// Reads the live factory generation, if any.
fn active_generation() -> Option<u64> {
    match ACTIVE_GENERATION.load(Ordering::SeqCst) {
        0 => None,
        generation => Some(generation),
    }
}

/// Retargets the live factory generation.
fn set_active_generation(generation: Option<u64>) {
    ACTIVE_GENERATION.store(generation.unwrap_or(0), Ordering::SeqCst);
}

/// Deletes the cycle factory against the live active pointer
/// (`ps_delete_cycle_factory`).
fn delete_cycle_factory_live(slot: &mut FactorySlot) {
    let mut active = active_generation();
    conf::delete_cycle_factory(slot, &mut active);
    set_active_generation(active);
}

/// Reads our main conf from a cycle (`ngx_http_cycle_get_module_main_conf`).
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t`; the module must be
/// loaded (index set). Returns null when this cycle carries no
/// main conf for the module.
unsafe fn cycle_main_conf_of(cycle: *mut ngx_cycle_t) -> *mut MainConf {
    use ngx::ffi::ngx_http_module;
    // SAFETY: caller guarantees a live cycle; both indices were
    // assigned by nginx at load.
    unsafe {
        let http_index = ngx_http_module.index;
        let ctx = *(*cycle).conf_ctx.add(http_index) as *mut ngx_http_conf_ctx_t;
        if ctx.is_null() {
            return ptr::null_mut();
        }
        let ours = NGX_PAGE_SPEED.ctx_index;
        *(*ctx).main_conf.add(ours) as *mut MainConf
    }
}

/// Collects core server confs (`cmcf->servers` walk).
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t` with its http
/// config contexts built.
unsafe fn core_servers(cycle: *mut ngx_cycle_t) -> Vec<*mut ngx_http_core_srv_conf_t> {
    use ngx::ffi::{ngx_http_core_module, ngx_http_module};
    // SAFETY: caller guarantees a live configured cycle.
    unsafe {
        let http_index = ngx_http_module.index;
        let ctx = *(*cycle).conf_ctx.add(http_index) as *mut ngx_http_conf_ctx_t;
        if ctx.is_null() {
            return Vec::new();
        }
        let core_index = ngx_http_core_module.ctx_index;
        let cmcf = *(*ctx).main_conf.add(core_index) as *mut ngx_http_core_main_conf_t;
        if cmcf.is_null() {
            return Vec::new();
        }
        let elts = (*cmcf).servers.elts as *mut *mut ngx_http_core_srv_conf_t;
        if elts.is_null() {
            return Vec::new();
        }
        (0..(*cmcf).servers.nelts).map(|i| *elts.add(i)).collect()
    }
}

/// Whether this is the first cycle in the process (`old_cycle`
/// unset or context-less).
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t`.
unsafe fn is_initial_cycle(cycle: *mut ngx_cycle_t) -> bool {
    // SAFETY: caller guarantees a live cycle.
    unsafe {
        let old = (*cycle).old_cycle;
        old.is_null() || (*old).conf_ctx.is_null()
    }
}

/// Whether nginx runs single-process (`master_process off`).
fn is_single_process() -> bool {
    use ngx::ffi::{NGX_PROCESS_SINGLE, ngx_process};
    // SAFETY: nginx global, always readable.
    unsafe { ngx_process == NGX_PROCESS_SINGLE as ngx_uint_t }
}

/// Whether this is a configuration test (`nginx -t`).
fn is_test_config() -> bool {
    use ngx::ffi::ngx_test_config;
    // SAFETY: nginx global, always readable.
    unsafe { ngx_test_config != 0 }
}

/// Ignores SIGPIPE (`ps_ignore_sigpipe`).
fn ignore_sigpipe() {
    // SAFETY: zeroed `sigaction` with `SIG_IGN`; installing it is
    // async-signal-safe.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = libc::SIG_IGN;
        libc::sigemptyset(&mut act.sa_mask);
        act.sa_flags = 0;
        libc::sigaction(libc::SIGPIPE, &act, ptr::null_mut());
    }
}

/// Module init (`ps_init_module`).
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t` past configuration.
pub unsafe extern "C" fn init_module(cycle: *mut ngx_cycle_t) -> ngx_int_t {
    // SAFETY: caller guarantees a live configured cycle.
    unsafe {
        let cfg = cycle_main_conf_of(cycle);
        if cfg.is_null() {
            return NGX_OK as ngx_int_t;
        }
        // Collect server contexts, remote inputs, and the first
        // server's resolver settings (verbatim: `(*cscfp)`).
        let ours = NGX_PAGE_SPEED.ctx_index;
        let core = ngx::ffi::ngx_http_core_module.ctx_index;
        let mut contexts = Vec::new();
        let mut remotes = Vec::new();
        let mut first_clcf: Option<*mut ngx_http_core_loc_conf_t> = None;
        for (index, cscf) in core_servers(cycle).iter().enumerate() {
            if cscf.is_null() {
                continue;
            }
            let sctx = (**cscf).ctx;
            if sctx.is_null() {
                continue;
            }
            if index == 0 {
                let clcf = *(*sctx).loc_conf.add(core) as *mut ngx_http_core_loc_conf_t;
                if !clcf.is_null() {
                    first_clcf = Some(clcf);
                }
            }
            let srv = *(*sctx).srv_conf.add(ours) as *mut SrvConf;
            if srv.is_null() {
                continue;
            }
            if let Some(context) = (*srv).context.as_ref() {
                remotes.push(RemoteWarningInput {
                    url: context.options.core.remote_configuration_url(),
                    external_cache: !context.options.system.memcached_servers.is_empty()
                        || context.options.system.redis_server.is_some(),
                });
                contexts.push(PostConfigContext { options: &context.options });
            }
        }
        // The resolver and the log sink install before the plan
        // runs: C++ logs through the main conf's handler, the
        // port through the freshly wired factory handler, so
        // `LoggingInit` moves ahead of the checks (logging-only
        // reorder; every decision keeps its order).
        {
            let main = &mut *cfg;
            if let Some(clcf) = first_clcf {
                main.driver_factory.set_resolver(!(*clcf).resolver.is_null());
                main.driver_factory
                    .set_resolver_timeout((*clcf).resolver_timeout as i64);
            }
            main.driver_factory.logging_init((*cycle).log, true);
        }
        let outcome = {
            let main = &*cfg;
            init_module_plan(InitModuleInputs {
                factory: &main.driver_factory,
                contexts: &contexts,
                initial_cycle: is_initial_cycle(cycle),
                single_process: is_single_process(),
                test_config: is_test_config(),
                remotes: &remotes,
            })
        };
        // SAFETY: the cycle log outlives the hook; every emit
        // below runs through the wired factory handler.
        if outcome.reload_unsupported {
            let main = &*cfg;
            main.driver_factory
                .message_handler
                .message(MessageLevel::Error, RELOAD_UNSUPPORTED);
            return NGX_OK as ngx_int_t;
        }
        if let Some(error) = &outcome.error {
            let main = &*cfg;
            main.driver_factory
                .message_handler
                .message(MessageLevel::Error, error);
            return NGX_ERROR as ngx_int_t;
        }
        if !outcome.has_contexts {
            let main = &mut *cfg;
            delete_cycle_factory_live(&mut main.factory);
            return NGX_OK as ngx_int_t;
        }
        ignore_sigpipe();
        if outcome.init_null_stats {
            // Slice 7: `ps_stats::init_all` on the null registry.
        }
        if outcome.root_init {
            // Slice 6d: `RootInit` (cache volumes).
        }
        set_active_generation((*cfg).factory.generation);
        for warning in &outcome.warnings {
            let main = &*cfg;
            main.driver_factory
                .message_handler
                .message(MessageLevel::Warning, warning);
        }
        NGX_OK as ngx_int_t
    }
}

/// Child-process init (`ps_init_child_process`).
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t`.
pub unsafe extern "C" fn init_child_process(cycle: *mut ngx_cycle_t) -> ngx_int_t {
    // SAFETY: caller guarantees a live cycle.
    unsafe {
        let cfg = cycle_main_conf_of(cycle);
        if cfg.is_null() {
            return NGX_OK as ngx_int_t;
        }
        let main = &mut *cfg;
        if !child_process_proceeds(main.factory.deleted) {
            return NGX_OK as ngx_int_t;
        }
        // Slice 6c: `NgxBaseFetch::Initialize(cycle)` fails the
        // worker when it returns false.
        main.driver_factory.logging_init((*cycle).log, true);
        // Slice 6d slots in here: `ChildInit`, then per-server
        // proxy-fetch factories plus message handlers.
        main.driver_factory.start_threads();
        NGX_OK as ngx_int_t
    }
}

/// Child-process exit (`ps_exit_child_process`).
///
/// # Safety
///
/// `cycle` must point at a live `ngx_cycle_t`.
pub unsafe extern "C" fn exit_child_process(cycle: *mut ngx_cycle_t) {
    // SAFETY: caller guarantees a live cycle.
    unsafe {
        let cfg = cycle_main_conf_of(cycle);
        if cfg.is_null() || (*cfg).factory.deleted {
            return;
        }
        // Slice 6c: `NgxBaseFetch::Terminate()`.
        (*cfg).driver_factory.shutdown();
    }
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
    // No `InitApr`: Rust has no APR to initialize, and the
    // filter tables are compile-time (no `Initialize()` step).
    let mut conf = Box::new(MainConf::default());
    conf.factory.generation = Some(next_generation());
    // SAFETY: live conf; the cycle log outlives config parsing;
    // the box outlives the pool via cleanup.
    unsafe {
        conf.driver_factory.logging_init((*(*cf).cycle).log, false);
        if let Some(info) = conf.driver_factory.init() {
            conf.driver_factory
                .message_handler
                .message(MessageLevel::Info, &info);
        }
        let raw = Box::into_raw(conf);
        set_cleanup(cf, Some(drop_main_conf), raw as *mut c_void);
        raw as *mut c_void
    }
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
        conf::merge_loc_conf(parent_opts, loc, srv_context_options(srv), srv.disabled());
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
            apply_directive_line(&mut srv.options, &mut main.driver_factory, scope, &refs)
        } else {
            let loc = &mut *loc_conf_of(cf);
            apply_directive_line(&mut loc.options, &mut main.driver_factory, scope, &refs)
        };
        match outcome {
            Ok(applied) => {
                // Slice 7 executes the gzip call (live gzip conf).
                let _ = applied.gzip;
                // Slice 7 logs these at warn level (live `ngx_log_t`).
                let _ = applied.warnings;
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
                let mode = (*main_conf_of(cf)).driver_factory.factory_options.script_variables;
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

/// Reads our server conf from a live request (`ps_get_srv_config`).
///
/// # Safety
///
/// `r` must point at a live request; the module must be loaded
/// (index set).
pub unsafe fn srv_conf_of_request(r: *mut ngx_http_request_t) -> *mut SrvConf {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let index = NGX_PAGE_SPEED.ctx_index as usize;
        *(*r).srv_conf.add(index) as *mut SrvConf
    }
}

/// Reads an `ngx_str_t` lossily (`str_to_string_piece` for text we
/// match on; invalid UTF-8 mangles and fails closed downstream).
///
/// # Safety
///
/// `s` must point at a readable string.
unsafe fn ngx_str_text(s: *const ngx_str_t) -> String {
    // SAFETY: caller guarantees a readable string.
    unsafe {
        let bytes = std::slice::from_raw_parts((*s).data, (*s).len);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Reads an optional request header value.
///
/// # Safety
///
/// `header`, when non-null, must point at a readable table entry.
unsafe fn header_value(header: *mut ngx_table_elt_t) -> Option<String> {
    if header.is_null() {
        return None;
    }
    // SAFETY: non-null per the check above.
    unsafe { Some(ngx_str_text(&raw const (*header).value)) }
}

/// Whether the worker is shutting down (`ngx_terminate || ngx_exiting`).
fn shutting_down() -> bool {
    // SAFETY: nginx-owned globals; read-only here.
    unsafe { ngx::ffi::ngx_terminate != 0 || ngx::ffi::ngx_exiting != 0 }
}

/// Whether the request arrived over TLS (`ps_is_https`).
///
/// # Safety
///
/// `r` must point at a live request.
unsafe fn request_is_https(r: *mut ngx_http_request_t) -> bool {
    // SAFETY: caller guarantees a live request.
    unsafe { !(*(*r).connection).ssl.is_null() }
}

/// Reads the local port (`ps_determine_port` sockaddr half).
///
/// # Safety
///
/// `r` must point at a live request with a connected socket.
unsafe fn local_port(r: *mut ngx_http_request_t) -> i32 {
    use ngx::ffi::{AF_INET6, sockaddr_in, sockaddr_in6};
    // SAFETY: caller guarantees a live connected request; the
    // family selects the valid sockaddr view.
    unsafe {
        let local = (*(*r).connection).local_sockaddr;
        if (*local).sa_family as u32 == AF_INET6 {
            u16::from_be((*(local as *mut sockaddr_in6)).sin6_port) as i32
        } else {
            u16::from_be((*(local as *mut sockaddr_in)).sin_port) as i32
        }
    }
}

/// Reads the local address text, if the lookup succeeds
/// (`ngx_connection_local_sockaddr` into a scratch buffer).
///
/// # Safety
///
/// `connection` must point at a live connection.
unsafe fn local_address_text(connection: *mut ngx::ffi::ngx_connection_t) -> Vec<u8> {
    // Scratch capacity: comfortably above `NGX_SOCKADDR_STRLEN`
    // (55 with IPv6); the callee honors `len` as the bound.
    let mut buf = vec![0u8; 128];
    // SAFETY: caller guarantees a live connection; the buffer
    // outlives the call.
    unsafe {
        let mut s = ngx_str_t { len: buf.len(), data: buf.as_mut_ptr() };
        if ngx_connection_local_sockaddr(connection, &mut s, 0) != NGX_OK as ngx_int_t {
            return Vec::new();
        }
        buf.truncate(s.len);
        buf
    }
}

/// Determines the request port (`ps_determine_port`): the Host
/// header first, else the connection's local port.
///
/// # Safety
///
/// `r` must point at a live request with a connected socket.
unsafe fn request_port(r: *mut ngx_http_request_t) -> i32 {
    // SAFETY: caller guarantees a live request.
    unsafe {
        let host = header_value((*r).headers_in.host);
        let port = routing::parse_host_port(host.as_deref());
        if port != routing::NO_PORT {
            return port;
        }
        local_port(r)
    }
}

/// Determines the request host (`ps_determine_host`): the server
/// name nginx selected, else the local address text.
///
/// # Safety
///
/// `r` must point at a live request with a connected socket.
unsafe fn request_host(r: *mut ngx_http_request_t) -> String {
    // SAFETY: caller guarantees a live request.
    unsafe {
        let host = ngx_str_text(&raw const (*r).headers_in.server);
        if !host.is_empty() {
            return host;
        }
        let bytes = local_address_text((*r).connection);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Builds the full request URL (`ps_determine_url`).
///
/// # Safety
///
/// `r` must point at a live request with a connected socket.
unsafe fn request_url_text(r: *mut ngx_http_request_t) -> String {
    // SAFETY: caller guarantees a live request.
    unsafe {
        let https = request_is_https(r);
        let port = request_port(r);
        let host = request_host(r);
        let uri = ngx_str_text(&raw const (*r).unparsed_uri);
        routing::build_url(https, port, &host, &uri)
    }
}

/// Content handler (`ps_content_handler`).
///
/// # Safety
///
/// `r` must point at a live request with a connected socket.
pub unsafe extern "C" fn content_handler(r: *mut ngx_http_request_t) -> ngx_int_t {
    // SAFETY: caller guarantees a live request.
    unsafe {
        let cfg_s = &*srv_conf_of_request(r);
        if cfg_s.disabled() {
            return NGX_DECLINED as ngx_int_t;
        }
        let Some(ctx) = cfg_s.context.as_ref() else {
            return NGX_DECLINED as ngx_int_t;
        };
        if !ctx.initialized() {
            return NGX_DECLINED as ngx_int_t;
        }
        ctx.poll_cache_flush();
        let url_text = request_url_text(r);
        let agent = header_value((*r).headers_in.user_agent);
        let method = ngx_str_text(&raw const (*r).method_name);
        let route = routing::route_request(&RoutingInputs {
            disabled: false,
            shutting_down: shutting_down(),
            err_status: (*r).err_status != 0,
            url_text: &url_text,
            subrequest: routing::is_pagespeed_subrequest(agent.as_deref()),
            // Slice 6 reads this off the live factory.
            static_asset_prefix: STATIC_ASSET_PREFIX,
            options: &ctx.options,
            https: request_is_https(r),
            method: &method,
        });
        match routing::content_decision(false, true, route) {
            ContentAction::Error => NGX_ERROR as ngx_int_t,
            ContentAction::Decline => NGX_DECLINED as ngx_int_t,
            // Later slices install the beacon, simple, and
            // resource bodies; decline until they land.
            ContentAction::Beacon | ContentAction::Simple | ContentAction::Resource => {
                NGX_DECLINED as ngx_int_t
            }
        }
    }
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
        let rc = content_handler(r);
        if rc == NGX_DECLINED as ngx_int_t {
            (*r).write_event_handler = Some(ngx_http_core_run_phases);
            (*r).phase_handler += 1;
            return NGX_AGAIN as ngx_int_t;
        }
        ngx_http_finalize_request(r, rc);
    }
    NGX_OK as ngx_int_t
}

/// Restores a relabeled connection at request cleanup
/// (`ps_self_fetch_restore`).
unsafe extern "C" fn self_fetch_restore(data: *mut c_void) {
    // SAFETY: `data` is the pool-allocated relabel scratch whose
    // connection is still live at cleanup time.
    unsafe {
        let relabel = &*(data as *const self_fetch::SelfFetchRelabel);
        self_fetch::restore_self_fetch(&relabel.saved);
    }
}

/// Body-discard failure outcome (`ps_discard_request_body` past
/// the call): the connection leaves keepalive, and lingers closed
/// when body bytes may remain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscardFailureOutcome {
    /// Keepalive stays on (always false here: failures leave it).
    pub keepalive: bool,
    /// Drain leftover bytes before closing.
    pub lingering_close: bool,
}

/// Plans a failed body discard: lingering closes when the length
/// is positive or the body is chunked.
#[must_use]
pub const fn discard_failure_outcome(content_length: i64, chunked: bool) -> DiscardFailureOutcome {
    DiscardFailureOutcome {
        keepalive: false,
        lingering_close: content_length > 0 || chunked,
    }
}

/// Discards the request body before answering directly
/// (`ps_discard_request_body`).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn discard_request_body(r: *mut ngx_http_request_t) {
    use ngx::ffi::ngx_http_discard_request_body;
    // SAFETY: caller guarantees a live request.
    unsafe {
        if ngx_http_discard_request_body(r) != NGX_OK as ngx_int_t {
            let outcome = discard_failure_outcome(
                (*r).headers_in.content_length_n,
                (*r).headers_in.chunked() != 0,
            );
            (*r).set_keepalive(u32::from(outcome.keepalive));
            (*r).set_lingering_close(u32::from(outcome.lingering_close));
        }
    }
}

/// Reads the attached context, aborting when absent (`CHECK(ctx
/// != NULL)` shared by the suspend/resume path).
///
/// # Safety
///
/// `r` must point at a live request.
unsafe fn require_request_ctx(r: *mut ngx_http_request_t) -> *mut crate::request::RequestCtx {
    // SAFETY: caller guarantees a live request.
    let ctx = unsafe { crate::request::get_request_ctx(r) };
    assert!(!ctx.is_null(), "async wait without a request context");
    ctx
}

/// Suspends the request while PageSpeed works
/// (`ps_async_wait_response`): holds a reference, arms the abort
/// read handler (client close or HTTP/2 reset cleans up instead
/// of resuming), idles the write handler, and buffers output.
///
/// # Safety
///
/// `r` must point at a live request with an attached context.
pub unsafe extern "C" fn async_wait_response(r: *mut ngx_http_request_t) -> ngx_int_t {
    use ngx::ffi::{ngx_http_request_empty_handler, ngx_http_test_reading};
    // SAFETY: caller guarantees a live request.
    unsafe {
        let _ = require_request_ctx(r);
        let count = (*r).count();
        (*r).set_count(count + 1);
        (*r).read_event_handler = Some(ngx_http_test_reading);
        (*r).write_event_handler = Some(ngx_http_request_empty_handler);
        crate::request::set_buffered(r, true);
        NGX_DONE as ngx_int_t
    }
}

/// Resets context state for decline (`ps_decline_request` ctx
/// half): restores the HTML-rewrite flags and detaches the fetch
/// bridge. `release_driver` cleans the driver (always `Some` once
/// Slice 6 owns drivers).
pub fn decline_request_state(
    ctx: &mut crate::request::RequestCtx,
    release_driver: Option<&mut dyn FnMut()>,
) {
    if let Some(release) = release_driver {
        release();
    }
    ctx.location_field_set = false;
    ctx.psol_vary_accept_only = false;
    ctx.html_rewrite = true;
    ctx.in_place = false;
    if let Some(mut fetch) = ctx.base_fetch.take() {
        fetch.detach();
    }
}

/// Advances nginx state past decline: holds a reference, steps to
/// the next phase, restores the blocking read handler with the
/// phase runner, and unbuffers.
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn advance_past_decline(r: *mut ngx_http_request_t) {
    use ngx::ffi::{ngx_http_block_reading, ngx_http_core_run_phases};
    // SAFETY: caller guarantees a live request.
    unsafe {
        let count = (*r).count();
        (*r).set_count(count + 1);
        (*r).phase_handler += 1;
        (*r).read_event_handler = Some(ngx_http_block_reading);
        (*r).write_event_handler = Some(ngx_http_core_run_phases);
        crate::request::set_buffered(r, false);
    }
}

/// Releases an in-place lookup back to nginx (`ps_decline_request`).
/// Plain linkage (like C++): only our own code calls it, and the
/// driver callback is not FFI-safe.
///
/// # Safety
///
/// `r` must point at a live request with an attached context.
pub unsafe fn decline_request(
    r: *mut ngx_http_request_t,
    release_driver: Option<&mut dyn FnMut()>,
) -> ngx_int_t {
    use ngx::ffi::{ngx_http_core_run_phases, ngx_http_run_posted_requests, NGX_DONE};
    // SAFETY: caller guarantees a live request.
    unsafe {
        let ctx = &mut *require_request_ctx(r);
        decline_request_state(ctx, release_driver);
        advance_past_decline(r);
        ngx_http_core_run_phases(r);
        ngx_http_run_posted_requests((*r).connection);
        NGX_DONE as ngx_int_t
    }
}

/// Post-read self-fetch handler (`ps_self_fetch_post_read_handler`).
///
/// Mirrors `plan_self_fetch` branch-for-branch (allocation sits
/// between the peer check and the relabel, so the plan fn cannot
/// be called directly).
///
/// # Safety
///
/// `r` must point at a live request with a connected socket.
pub unsafe extern "C" fn self_fetch_handler(r: *mut ngx_http_request_t) -> ngx_int_t {
    // SAFETY: caller guarantees a live connected request.
    unsafe {
        if r != (*r).main {
            return NGX_DECLINED as ngx_int_t;
        }
        let agent = header_value((*r).headers_in.user_agent);
        if !routing::is_pagespeed_subrequest(agent.as_deref()) {
            return NGX_DECLINED as ngx_int_t;
        }
        let connection = (*r).connection;
        // A failed local lookup reads as NULL (a NULL `s` only
        // refreshes the cached sockaddr); loopback peers still match.
        let local = if ngx_connection_local_sockaddr(connection, ptr::null_mut(), 0)
            == NGX_OK as ngx_int_t
        {
            (*connection).local_sockaddr as *const libc::sockaddr
        } else {
            ptr::null()
        };
        let peer = (*connection).sockaddr as *const libc::sockaddr;
        if !self_fetch::peer_is_this_server(peer, local) {
            return NGX_DECLINED as ngx_int_t;
        }
        let cleanup = ngx_pool_cleanup_add(
            (*r).pool,
            mem::size_of::<self_fetch::SelfFetchRelabel>(),
        );
        if cleanup.is_null() {
            return NGX_HTTP_INTERNAL_SERVER_ERROR as ngx_int_t;
        }
        let relabel = (*cleanup).data as *mut self_fetch::SelfFetchRelabel;
        self_fetch::relabel_self_fetch(connection, &mut (*relabel).label, &mut (*relabel).saved);
        (*cleanup).handler = Some(self_fetch_restore);
        NGX_DECLINED as ngx_int_t
    }
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
    fn discard_failure_matrix() {
        assert_eq!(
            DiscardFailureOutcome { keepalive: false, lingering_close: true },
            discard_failure_outcome(10, false)
        );
        assert_eq!(
            DiscardFailureOutcome { keepalive: false, lingering_close: true },
            discard_failure_outcome(0, true)
        );
        assert_eq!(
            DiscardFailureOutcome { keepalive: false, lingering_close: false },
            discard_failure_outcome(0, false)
        );
        assert_eq!(
            DiscardFailureOutcome { keepalive: false, lingering_close: false },
            discard_failure_outcome(-1, false)
        );
    }

    /// Simulates module load for ctx-slot tests: nginx assigns the
    /// context index once at startup; tests pin it to slot zero the
    /// same way (race-free; no test depends on the unset value).
    fn simulate_module_load() {
        use std::sync::Once;
        static LOAD: Once = Once::new();
        LOAD.call_once(|| {
            // SAFETY: single assignment under `Once`.
            unsafe {
                NGX_PAGE_SPEED.ctx_index = 0;
            }
        });
    }

    #[test]
    fn async_wait_arms_and_holds() {
        use crate::request::RequestCtx;
        use ngx::ffi::{
            ngx_http_request_empty_handler, ngx_http_request_t, ngx_http_test_reading,
        };
        use std::mem::MaybeUninit;
        simulate_module_load();
        // SAFETY: all structures are test-owned; stub addresses are
        // stored, never called.
        unsafe {
            let mut request: ngx_http_request_t = MaybeUninit::zeroed().assume_init();
            let mut ctx = RequestCtx::for_resource(
                "http://h/a",
                true,
                crate::caching::PreserveCachingHeaders::DontPreserve,
            );
            let mut slots = [std::ptr::null_mut()];
            slots[0] = &mut ctx as *mut _ as *mut _;
            request.ctx = slots.as_mut_ptr();
            let rc = async_wait_response(&mut request);
            assert_eq!(NGX_DONE as ngx_int_t, rc);
            assert_eq!(1, request.count());
            assert!(request.buffered() & crate::request::PAGESPEED_BUFFERED != 0);
            let read = request.read_event_handler.expect("read handler armed");
            assert_eq!(read as *const (), ngx_http_test_reading as *const ());
            let write = request.write_event_handler.expect("write handler armed");
            assert_eq!(write as *const (), ngx_http_request_empty_handler as *const ());
        }
    }

    #[test]
    #[should_panic(expected = "async wait without a request context")]
    fn async_wait_requires_context() {
        use ngx::ffi::ngx_http_request_t;
        use std::mem::MaybeUninit;
        // SAFETY: zeroed request with an empty ctx slot (`CHECK`).
        // Calls the plain helper: panics cannot unwind through the
        // `extern "C"` wrapper, so the wrapper itself is untestable
        // here and aborts live instead, like `CHECK`.
        simulate_module_load();
        unsafe {
            let mut request: ngx_http_request_t = MaybeUninit::zeroed().assume_init();
            let mut slots = [std::ptr::null_mut()];
            request.ctx = slots.as_mut_ptr();
            let _ = require_request_ctx(&mut request);
        }
    }

    #[test]
    fn decline_resets_and_advances() {
        use crate::caching::PreserveCachingHeaders;
        use crate::fetch::{BaseFetch, BaseFetchType};
        use crate::request::RequestCtx;
        use ngx::ffi::{ngx_http_block_reading, ngx_http_core_run_phases, ngx_http_request_t};
        use ps_http::response::HttpOptions;
        use std::mem::MaybeUninit;
        // Context half: flags restored, fetch detached.
        let mut ctx = RequestCtx::for_resource("http://h/a", true, PreserveCachingHeaders::DontPreserve);
        ctx.location_field_set = true;
        ctx.psol_vary_accept_only = true;
        ctx.in_place = true;
        ctx.base_fetch = Some(BaseFetch::new(
            "http://h/a",
            BaseFetchType::IproLookup,
            PreserveCachingHeaders::DontPreserve,
            HttpOptions::default_for_tests(),
        ));
        let mut released = false;
        decline_request_state(&mut ctx, Some(&mut || released = true));
        assert!(released);
        assert!(!ctx.location_field_set);
        assert!(!ctx.psol_vary_accept_only);
        assert!(ctx.html_rewrite);
        assert!(!ctx.in_place);
        assert!(ctx.base_fetch.is_none());
        // Nginx half over a fabricated request.
        unsafe {
            let mut request: ngx_http_request_t = MaybeUninit::zeroed().assume_init();
            crate::request::set_buffered(&mut request, true);
            request.phase_handler = 3;
            advance_past_decline(&mut request);
            assert_eq!(1, request.count());
            assert_eq!(4, request.phase_handler);
            assert_eq!(0, request.buffered() & crate::request::PAGESPEED_BUFFERED);
            let read = request.read_event_handler.expect("read handler restored");
            assert_eq!(read as *const (), ngx_http_block_reading as *const ());
            let write = request.write_event_handler.expect("write handler restored");
            assert_eq!(write as *const (), ngx_http_core_run_phases as *const ());
        }
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
        let mut factory = DriverFactory::default();
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
        // Over-arity reports the unknown-name error.
        let many = ["a"; 11];
        assert_eq!(
            Err("\"a\" not recognized or too many arguments".to_string()),
            apply_directive_line(&mut options, &mut factory, Scope::Server, &many)
        );
    }

    #[test]
    fn factory_fallback_lines() {
        let mut factory = DriverFactory::default();
        let mut options: Option<NgxOptions> = None;
        // Top level applies.
        let out = apply_directive_line(
            &mut options,
            &mut factory,
            Scope::ProcessStrict,
            &["NumRewriteThreads", "4"],
        )
        .unwrap();
        assert_eq!(4, factory.num_rewrite_threads);
        assert!(out.warnings.is_empty());
        // Bad values wrap the full line around the (empty) detail.
        assert_eq!(
            Err("\"NumRewriteThreads many\" ".to_string()),
            apply_directive_line(
                &mut options,
                &mut factory,
                Scope::ProcessStrict,
                &["NumRewriteThreads", "many"]
            )
        );
        // The hard-error five pass the gate at server scope but
        // the factory rejects them there; the gate only stops
        // them at directory scope.
        assert_eq!(
            Err("\"NumRewriteThreads 4\" 'NumRewriteThreads' is global and can't be set at this scope.".to_string()),
            apply_directive_line(
                &mut options,
                &mut factory,
                Scope::Server,
                &["NumRewriteThreads", "4"]
            )
        );
        assert_eq!(
            Err("\"NumRewriteThreads\" cannot be set at this scope.".to_string()),
            apply_directive_line(
                &mut options,
                &mut factory,
                Scope::Directory,
                &["NumRewriteThreads", "4"]
            )
        );
        // The warn four apply at main, warn-and-ignore at server.
        apply_directive_line(&mut options, &mut factory, Scope::ProcessStrict, &[
            "ForceCaching",
            "on",
        ])
        .unwrap();
        assert!(factory.force_caching);
        factory.force_caching = false;
        let out = apply_directive_line(&mut options, &mut factory, Scope::Server, &[
            "ForceCaching",
            "on",
        ])
        .unwrap();
        assert!(!factory.force_caching);
        assert_eq!(
            vec!["'ForceCaching' is global and is ignored at this scope".to_string()],
            out.warnings
        );
        // ... and the gate rejects them at directory scope.
        assert_eq!(
            Err("\"ForceCaching\" cannot be set at this scope.".to_string()),
            apply_directive_line(&mut options, &mut factory, Scope::Directory, &[
                "ForceCaching",
                "on"
            ])
        );
        // Ranged detail, wrapped.
        assert_eq!(
            Err(
                "\"MessageBufferSize -1\" MessageBufferSize must be between 0 and 1073741824."
                    .to_string()
            ),
            apply_directive_line(
                &mut options,
                &mut factory,
                Scope::ProcessStrict,
                &["MessageBufferSize", "-1"]
            )
        );
        // The `ModPagespeed` prefix strips for matching but survives
        // in the wrapped line.
        assert_eq!(
            Err("\"ModPagespeedNumRewriteThreads many\" ".to_string()),
            apply_directive_line(
                &mut options,
                &mut factory,
                Scope::ProcessStrict,
                &["ModPagespeedNumRewriteThreads", "many"]
            )
        );
        // Non-factory unknowns keep their error.
        assert_eq!(
            Err("\"Bogus\" not recognized or too many arguments".to_string()),
            apply_directive_line(&mut options, &mut factory, Scope::Server, &["Bogus", "x"])
        );
    }

    #[test]
    fn init_module_matrix() {
        use crate::factory::PostConfigContext;
        let factory = DriverFactory::default();
        let plain = NgxOptions::new();
        let contexts = [PostConfigContext { options: &plain }];
        let plan = |factory: &DriverFactory,
                    contexts: &[PostConfigContext],
                    initial_cycle: bool,
                    single_process: bool,
                    test_config: bool,
                    remotes: &[RemoteWarningInput]| {
            init_module_plan(InitModuleInputs {
                factory,
                contexts,
                initial_cycle,
                single_process,
                test_config,
                remotes,
            })
        };
        // The reload guard fires before everything else.
        let out = plan(&factory, &contexts, false, true, false, &[]);
        assert!(out.reload_unsupported);
        assert_eq!(None, out.error);
        // PostConfig errors prefix and fail.
        let out = plan(&factory, &contexts, true, false, false, &[]);
        assert!(!out.reload_unsupported);
        assert_eq!(
            Some("ngx_pagespeed is enabled. FileCachePath must not be empty".to_string()),
            out.error
        );
        assert!(out.has_contexts);
        // Missing resolver fails only with the native fetcher.
        let mut cached = NgxOptions::new();
        cached.system.file_cache_path = "/cache".to_string();
        let contexts = [PostConfigContext { options: &cached }];
        let out = plan(&factory, &contexts, true, false, false, &[]);
        assert_eq!(None, out.error);
        assert!(out.has_contexts);
        assert!(out.root_init);
        let mut native = DriverFactory::default();
        native.factory_options.use_native_fetcher = true;
        let out = plan(&native, &contexts, true, false, false, &[]);
        assert_eq!(Some(RESOLVER_REQUIRED.to_string()), out.error);
        native.set_resolver(true);
        let out = plan(&native, &contexts, true, false, true, &[]);
        assert_eq!(None, out.error);
        assert!(!out.root_init);
        // Empty cycles delete the factory and skip the resolver
        // block, stats init, and `RootInit` entirely (even a
        // resolver-less native fetcher passes here).
        native.set_resolver(false);
        let out = plan(&native, &[], true, false, false, &[]);
        assert!(!out.has_contexts);
        assert_eq!(None, out.error);
        assert!(!out.init_null_stats);
        assert!(!out.root_init);
        native.set_resolver(true);
        // Stats follow PostConfig (`cached` stays borrowed below,
        // so probe through a clone).
        let mut probe = cached.clone();
        probe.system.statistics_enabled = false;
        let probe_contexts = [PostConfigContext { options: &probe }];
        let out = plan(&factory, &probe_contexts, true, false, false, &[]);
        assert!(out.init_null_stats);
        probe.system.statistics_enabled = true;
        let probe_contexts = [PostConfigContext { options: &probe }];
        let out = plan(&factory, &probe_contexts, true, false, false, &[]);
        assert!(!out.init_null_stats);
        // Remote warnings, one per affected server.
        let remotes = [
            RemoteWarningInput { url: "http://x/rc".to_string(), external_cache: true },
            RemoteWarningInput { url: String::new(), external_cache: false },
        ];
        let out = plan(&native, &contexts, true, false, false, &remotes);
        assert_eq!(1, out.warnings.len());
        assert!(out.warnings[0].contains("may stall up to RemoteConfigurationTimeoutMs"));
        let out = plan(&factory, &contexts, true, false, false, &remotes);
        assert!(out.warnings.is_empty());
        // Child init proceeds unless the factory was deleted.
        assert!(child_process_proceeds(false));
        assert!(!child_process_proceeds(true));
    }

    #[test]
    fn dir_prep_line_reports_path() {
        let mut factory = DriverFactory::default();
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
