//! Automatic gzip setup (`ngx_gzip_setter.{h,cc}`).
//!
//! Ports the config-time interplay: at preconfiguration the
//! setter scans the live gzip module's commands, verifies
//! their setter signatures, and hooks redirects so an explicit
//! gzip line rolls our automatic setup back. `pagespeed on`
//! then enables gzip for the location with the trusted
//! settings; `pagespeed gzip off` turns it back off. The
//! rollback record lives in [`crate::gzip_rollback`]; this
//! module holds the command scan plus the setters, all of
//! which need live nginx state.

use crate::gzip_rollback::RollbackLog;
use ngx::ffi::{
    ngx_command_t, ngx_conf_t, ngx_http_conf_ctx_t, ngx_module_t, NGX_CONF_UNSET, NGX_DIRECT_CONF,
    NGX_HTTP_GZIP_PROXIED_ANY, NGX_HTTP_LOC_CONF_OFFSET, NGX_HTTP_VERSION_10, NGX_LOG_INFO,
    NGX_LOG_WARN, NGX_MAIN_CONF,
};
use std::ffi::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

/// A hooked gzip command plus its module (`ngx_command_ctx`).
#[derive(Debug, Clone, Copy)]
struct CommandCtx {
    /// Hooked command, null when not found.
    command: *mut ngx_command_t,
    /// Owning module.
    module: *mut ngx_module_t,
}

impl CommandCtx {
    /// Empty slot.
    const fn empty() -> Self {
        CommandCtx {
            command: std::ptr::null_mut(),
            module: std::ptr::null_mut(),
        }
    }

    /// Whether the command was found.
    fn is_some(self) -> bool {
        !self.command.is_null()
    }
}

// SAFETY: config-thread only (preconfiguration and directive
// handlers never run concurrently).
unsafe impl Send for CommandCtx {}

/// Gzip content types the setter enables (`gzip_http_types`;
/// `text/html` stays implied).
const GZIP_HTTP_TYPES: &[&[u8]] = &[
    b"application/ecmascript",
    b"application/javascript",
    b"application/json",
    b"application/pdf",
    b"application/postscript",
    b"application/x-javascript",
    b"image/svg+xml",
    b"text/css",
    b"text/csv",
    b"text/javascript",
    b"text/plain",
    b"text/xml",
];

/// Whether the command writes location conf
/// (`HasLocalConfig`).
fn has_local_config(command: &ngx_command_t) -> bool {
    (command.type_ & (NGX_DIRECT_CONF as usize | NGX_MAIN_CONF as usize)) == 0
        && command.conf == NGX_HTTP_LOC_CONF_OFFSET
}

// Setter addresses are compared exactly like C++: for nginx's
// own extern symbols the dynamic linker resolves one address,
// so the comparison is stable.
/// Setter-signature checks (`IsNgx*Command`).
#[allow(unpredictable_function_pointer_comparisons)]
fn is_flag_command(command: &ngx_command_t) -> bool {
    command.set == Some(ngx::ffi::ngx_conf_set_flag_slot) && has_local_config(command)
}

/// Setter-signature checks (`IsNgx*Command`).
#[allow(unpredictable_function_pointer_comparisons)]
fn is_types_command(command: &ngx_command_t) -> bool {
    command.set == Some(ngx::ffi::ngx_http_types_slot) && has_local_config(command)
}

/// Setter-signature checks (`IsNgx*Command`).
#[allow(unpredictable_function_pointer_comparisons)]
fn is_enum_command(command: &ngx_command_t) -> bool {
    command.set == Some(ngx::ffi::ngx_conf_set_enum_slot) && has_local_config(command)
}

/// Setter-signature checks (`IsNgx*Command`).
#[allow(unpredictable_function_pointer_comparisons)]
fn is_bitmask_command(command: &ngx_command_t) -> bool {
    command.set == Some(ngx::ffi::ngx_conf_set_bitmask_slot) && has_local_config(command)
}

/// Compares a command name with a literal (`STR_EQ_LITERAL`).
///
/// # Safety
///
/// `name` must point at a readable `ngx_str_t`.
unsafe fn name_is(name: *const ngx::ffi::ngx_str_t, literal: &[u8]) -> bool {
    // SAFETY: caller guarantees a readable string.
    unsafe {
        let bytes = std::slice::from_raw_parts((*name).data, (*name).len);
        bytes == literal
    }
}

/// Automatic gzip setup (`NgxGZipSetter`).
pub struct GzipSetter {
    gzip: CommandCtx,
    vary: CommandCtx,
    http_version: CommandCtx,
    proxied: CommandCtx,
    types: CommandCtx,
    rollback: RollbackLog,
}

impl GzipSetter {
    /// Empty setter (the record starts at `Init`).
    #[must_use]
    pub fn new() -> Self {
        GzipSetter {
            gzip: CommandCtx::empty(),
            vary: CommandCtx::empty(),
            http_version: CommandCtx::empty(),
            proxied: CommandCtx::empty(),
            types: CommandCtx::empty(),
            rollback: RollbackLog::new(),
        }
    }

    /// Whether automatic setup is armed (`enabled`).
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.rollback.enabled()
    }

    /// Scans the gzip commands and hooks the redirects
    /// (`Init`).
    ///
    /// # Safety
    ///
    /// `cf` must point at a live `ngx_conf_t` in
    /// preconfiguration.
    pub unsafe fn init(&mut self, cf: *mut ngx_conf_t) {
        // SAFETY: caller guarantees a live conf.
        unsafe {
            self.rollback.begin_cycle(false);
            #[cfg(not(ngx_http_gzip))]
            {
                let log = (*cf).log;
                ngx::ngx_log_error!(NGX_LOG_WARN, log, "pagespeed: gzip not compiled into nginx");
                return;
            }
            #[cfg(ngx_http_gzip)]
            self.rollback.begin_cycle(true);
            let mut gzip_mismatch = false;
            let mut other_mismatch = false;
            let modules = (*(*cf).cycle).modules;
            let mut m = 0;
            while !(*modules.add(m)).is_null() {
                let module = *modules.add(m);
                if !(*module).commands.is_null() {
                    let mut c = 0;
                    while (*(*module).commands.add(c)).name.len != 0 {
                        let current = &mut *(*module).commands.add(c);
                        let log = (*cf).log;
                        if self.gzip.command.is_null() && name_is(&raw const current.name, b"gzip")
                        {
                            if is_flag_command(current) {
                                current.set = Some(gzip_redirect_set_flag_slot);
                                self.gzip.command = current as *mut ngx_command_t;
                                self.gzip.module = module;
                            } else {
                                ngx::ngx_log_error!(
                                    NGX_LOG_WARN,
                                    log,
                                    "pagespeed: cannot set gzip, signature mismatch"
                                );
                                gzip_mismatch = true;
                            }
                        }
                        if self.http_version.command.is_null()
                            && name_is(&raw const current.name, b"gzip_http_version")
                        {
                            if is_enum_command(current) {
                                current.set = Some(gzip_redirect_set_enum_slot);
                                self.http_version.command = current as *mut ngx_command_t;
                                self.http_version.module = module;
                            } else {
                                ngx::ngx_log_error!(
                                    NGX_LOG_WARN,
                                    log,
                                    "pagespeed: cannot set gzip_http_version, signature mismatch"
                                );
                                other_mismatch = true;
                            }
                        }
                        if self.proxied.command.is_null()
                            && name_is(&raw const current.name, b"gzip_proxied")
                        {
                            if is_bitmask_command(current) {
                                current.set = Some(gzip_redirect_set_bitmask_slot);
                                self.proxied.command = current as *mut ngx_command_t;
                                self.proxied.module = module;
                            } else {
                                ngx::ngx_log_error!(
                                    NGX_LOG_WARN,
                                    log,
                                    "pagespeed: cannot set gzip_proxied, signature mismatch"
                                );
                                other_mismatch = true;
                            }
                        }
                        if self.types.command.is_null()
                            && name_is(&raw const current.name, b"gzip_types")
                        {
                            if is_types_command(current) {
                                current.set = Some(gzip_redirect_http_types_slot);
                                self.types.command = current as *mut ngx_command_t;
                                self.types.module = module;
                            } else {
                                ngx::ngx_log_error!(
                                    NGX_LOG_WARN,
                                    log,
                                    "pagespeed: cannot set gzip_types, signature mismatch"
                                );
                                other_mismatch = true;
                            }
                        }
                        if self.vary.command.is_null()
                            && name_is(&raw const current.name, b"gzip_vary")
                        {
                            if is_flag_command(current) {
                                current.set = Some(gzip_redirect_set_flag_slot);
                                self.vary.command = current as *mut ngx_command_t;
                                self.vary.module = module;
                            } else {
                                ngx::ngx_log_error!(
                                    NGX_LOG_WARN,
                                    log,
                                    "pagespeed: cannot set gzip_vary, signature mismatch"
                                );
                                other_mismatch = true;
                            }
                        }
                        c += 1;
                    }
                }
                m += 1;
            }
            let log = (*cf).log;
            if gzip_mismatch {
                // Already logged.
            } else if self.gzip.command.is_null() {
                ngx::ngx_log_error!(
                    NGX_LOG_WARN,
                    log,
                    "pagespeed: cannot set gzip, command not found"
                );
                self.rollback.begin_cycle(false);
            } else if other_mismatch {
                // Already logged.
            } else if self.vary.command.is_null() {
                ngx::ngx_log_error!(NGX_LOG_WARN, log, "pagespeed: missing gzip_vary");
            } else if self.types.command.is_null() {
                ngx::ngx_log_error!(NGX_LOG_WARN, log, "pagespeed: missing gzip_types");
            } else if self.http_version.command.is_null() {
                ngx::ngx_log_error!(NGX_LOG_WARN, log, "pagespeed: missing gzip_http_version");
            } else if self.proxied.command.is_null() {
                ngx::ngx_log_error!(NGX_LOG_WARN, log, "pagespeed: missing gzip_proxied");
            }
            // Else success.
        }
    }

    /// Resolves a command's location-conf slot
    /// (`GetConfPtr`): the inline helper expanded, since
    /// bindgen skips it.
    ///
    /// # Safety
    ///
    /// `cf` must point at a live `ngx_conf_t`; `ctx` at a found
    /// command whose module index is assigned.
    unsafe fn conf_ptr(cf: *mut ngx_conf_t, ctx: &CommandCtx) -> *mut u8 {
        // SAFETY: caller guarantees as documented.
        unsafe {
            let http = (*cf).ctx as *mut ngx_http_conf_ctx_t;
            let loc = *(*http).loc_conf.add((*ctx.module).ctx_index);
            (loc as *mut u8).add((*ctx.command).offset)
        }
    }

    /// Writes a flag slot, recording it (`SetNgxConfFlag`).
    ///
    /// # Safety
    ///
    /// As in [`GzipSetter::conf_ptr`].
    unsafe fn set_flag(&mut self, cf: *mut ngx_conf_t, ctx: &CommandCtx, value: isize) {
        // SAFETY: caller guarantees as documented.
        unsafe {
            let slot = Self::conf_ptr(cf, ctx) as *mut isize;
            *slot = value;
            self.rollback.record_flag(slot);
        }
    }

    /// Writes a uint slot, recording it (`SetNgxConfEnum` /
    /// `SetNgxConfBitmask`).
    ///
    /// # Safety
    ///
    /// As in [`GzipSetter::conf_ptr`].
    unsafe fn set_uint(&mut self, cf: *mut ngx_conf_t, ctx: &CommandCtx, value: usize) {
        // SAFETY: caller guarantees as documented.
        unsafe {
            let slot = Self::conf_ptr(cf, ctx) as *mut usize;
            *slot = value;
            self.rollback.record_uint(slot);
        }
    }

    /// Enables gzip for a location (`EnableGZipForLocation`).
    ///
    /// # Safety
    ///
    /// `cf` must point at a live `ngx_conf_t` in a directive
    /// handler, after [`GzipSetter::init`].
    pub unsafe fn enable_for_location(&mut self, cf: *mut ngx_conf_t) {
        // SAFETY: caller guarantees as documented.
        unsafe {
            if !self.enabled() {
                return;
            }
            // A second call for the same location is ignored
            // (no duplicate types).
            if *(Self::conf_ptr(cf, &self.gzip) as *mut isize) == 1 {
                return;
            }
            self.set_for_location(cf, true);
            if self.vary.is_some() {
                let ctx = self.vary;
                self.set_flag(cf, &ctx, 1);
            }
            if self.http_version.is_some() {
                let ctx = self.http_version;
                self.set_uint(cf, &ctx, NGX_HTTP_VERSION_10 as usize);
            }
            if self.proxied.is_some() {
                let ctx = self.proxied;
                self.set_uint(cf, &ctx, NGX_HTTP_GZIP_PROXIED_ANY as usize);
            }
            self.add_http_types(cf);
        }
    }

    /// Sets gzip for a location (`SetGZipForLocation`).
    ///
    /// # Safety
    ///
    /// As in [`GzipSetter::enable_for_location`].
    pub unsafe fn set_for_location(&mut self, cf: *mut ngx_conf_t, value: bool) {
        // SAFETY: caller guarantees as documented.
        unsafe {
            if !self.enabled() {
                return;
            }
            if self.gzip.is_some() {
                let ctx = self.gzip;
                self.set_flag(cf, &ctx, isize::from(value));
            }
        }
    }

    /// Adds the gzip content types (`AddGZipHTTPTypes`).
    ///
    /// # Safety
    ///
    /// As in [`GzipSetter::enable_for_location`].
    unsafe fn add_http_types(&mut self, cf: *mut ngx_conf_t) {
        use ngx::ffi::{ngx_http_types_slot, ngx_pnalloc, ngx_str_t};
        // SAFETY: caller guarantees as documented.
        unsafe {
            if !self.types.is_some() {
                return;
            }
            if (*(*cf).args).nalloc < 2 {
                let log = (*cf).log;
                ngx::ngx_log_error!(
                    NGX_LOG_WARN,
                    log,
                    "pagespeed: unexpected small cf->args in gzip_types"
                );
                return;
            }
            let ctx = self.types;
            let command = ctx.command;
            let http = (*cf).ctx as *mut ngx_http_conf_ctx_t;
            let gzip_conf = *(*http).loc_conf.add((*ctx.module).ctx_index);
            let elts = (*(*cf).args).elts as *mut ngx_str_t;
            let old_elt0 = *elts;
            let old_elt1 = *elts.add(1);
            let old_nelts = (*(*cf).args).nelts;
            static NAME: &[u8; 11] = b"gzip_types\0";
            *elts = ngx_str_t {
                len: 10,
                data: NAME.as_ptr() as *mut u8,
            };
            (*(*cf).args).nelts = 2;
            for mime in GZIP_HTTP_TYPES {
                // Pool copies leak on rollback, like nginx's own
                // tokens (verbatim comment kept short).
                let data = ngx_pnalloc((*cf).pool, mime.len() + 1) as *mut u8;
                if data.is_null() {
                    break;
                }
                std::ptr::copy_nonoverlapping(mime.as_ptr(), data, mime.len());
                *data.add(mime.len()) = 0;
                *elts.add(1) = ngx_str_t {
                    len: mime.len(),
                    data,
                };
                ngx_http_types_slot(cf, command, gzip_conf);
            }
            (*(*cf).args).nelts = old_nelts;
            *elts.add(1) = old_elt1;
            *elts = old_elt0;
            let slot = (gzip_conf as *mut u8).add((*command).offset) as *mut *mut ();
            self.rollback.record_types(slot);
        }
    }

    /// Rolls back automatic setup on explicit configuration
    /// (`RollBackAndDisable`).
    ///
    /// # Safety
    ///
    /// `cf` must point at a live `ngx_conf_t` in a directive
    /// handler.
    pub unsafe fn rollback_and_disable(&mut self, cf: *mut ngx_conf_t) {
        // SAFETY: caller guarantees as documented.
        unsafe {
            let log = (*cf).log;
            ngx::ngx_log_error!(
                NGX_LOG_INFO,
                log,
                "pagespeed: rollback gzip, explicit configuration"
            );
            self.rollback.roll_back(
                |slot| *slot = NGX_CONF_UNSET as isize,
                |slot| *slot = usize::MAX,
                |slot| {
                    let array = *slot as *mut ngx::ffi::ngx_array_t;
                    ngx::ffi::ngx_array_destroy(array);
                    *slot = std::ptr::null_mut();
                },
            );
        }
    }
}

impl Default for GzipSetter {
    fn default() -> Self {
        Self::new()
    }
}

// SAFETY: config-thread only (preconfiguration and directive
// handlers never run concurrently); the raw pointers never
// cross threads.
unsafe impl Send for GzipSetter {}

/// Process-global setter (`g_gzip_setter`).
static SETTER: OnceLock<Mutex<GzipSetter>> = OnceLock::new();

/// Borrows the global setter.
fn setter() -> &'static Mutex<GzipSetter> {
    SETTER.get_or_init(|| Mutex::new(GzipSetter::new()))
}

/// Locks the setter (poison recovers: config parsing must not
/// wedge on an earlier panic).
fn lock_setter() -> std::sync::MutexGuard<'static, GzipSetter> {
    setter().lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs the preconfiguration scan (`g_gzip_setter.Init`).
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in preconfiguration.
pub unsafe fn init_gzip_setter(cf: *mut ngx_conf_t) {
    // SAFETY: caller guarantees a live conf.
    unsafe {
        lock_setter().init(cf);
    }
}

/// Enables gzip for a location through the global setter.
///
/// # Safety
///
/// `cf` must point at a live `ngx_conf_t` in a directive
/// handler, after [`init_gzip_setter`].
pub unsafe fn enable_gzip_for_location(cf: *mut ngx_conf_t) {
    // SAFETY: caller guarantees as documented.
    unsafe {
        lock_setter().enable_for_location(cf);
    }
}

/// Sets gzip for a location through the global setter.
///
/// # Safety
///
/// As in [`enable_gzip_for_location`].
pub unsafe fn set_gzip_for_location(cf: *mut ngx_conf_t, value: bool) {
    // SAFETY: caller guarantees as documented.
    unsafe {
        lock_setter().set_for_location(cf, value);
    }
}

/// Redirects an explicit `gzip`/`gzip_vary` line: rolls back
/// automatic setup, then runs the original setter.
unsafe extern "C" fn gzip_redirect_set_flag_slot(
    cf: *mut ngx_conf_t,
    cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    // SAFETY: nginx directive-handler contract.
    unsafe {
        if lock_setter().enabled() {
            lock_setter().rollback_and_disable(cf);
        }
        ngx::ffi::ngx_conf_set_flag_slot(cf, cmd, conf)
    }
}

/// Redirects an explicit `gzip_types` line.
unsafe extern "C" fn gzip_redirect_http_types_slot(
    cf: *mut ngx_conf_t,
    cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    // SAFETY: nginx directive-handler contract.
    unsafe {
        if lock_setter().enabled() {
            lock_setter().rollback_and_disable(cf);
        }
        ngx::ffi::ngx_http_types_slot(cf, cmd, conf)
    }
}

/// Redirects an explicit `gzip_http_version` line.
unsafe extern "C" fn gzip_redirect_set_enum_slot(
    cf: *mut ngx_conf_t,
    cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    // SAFETY: nginx directive-handler contract.
    unsafe {
        if lock_setter().enabled() {
            lock_setter().rollback_and_disable(cf);
        }
        ngx::ffi::ngx_conf_set_enum_slot(cf, cmd, conf)
    }
}

/// Redirects an explicit `gzip_proxied` line.
unsafe extern "C" fn gzip_redirect_set_bitmask_slot(
    cf: *mut ngx_conf_t,
    cmd: *mut ngx_command_t,
    conf: *mut c_void,
) -> *mut c_char {
    // SAFETY: nginx directive-handler contract.
    unsafe {
        if lock_setter().enabled() {
            lock_setter().rollback_and_disable(cf);
        }
        ngx::ffi::ngx_conf_set_bitmask_slot(cf, cmd, conf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ngx::ffi::{ngx_command_t, ngx_str_t, NGX_HTTP_LOC_CONF_OFFSET};

    /// Builds a fabricated command with a name, type, setter,
    /// and conf offset.
    fn command(
        name: &'static [u8],
        type_: usize,
        set: Option<
            unsafe extern "C" fn(*mut ngx_conf_t, *mut ngx_command_t, *mut c_void) -> *mut c_char,
        >,
        conf: usize,
    ) -> ngx_command_t {
        ngx_command_t {
            name: ngx_str_t {
                len: name.len(),
                data: name.as_ptr() as *mut u8,
            },
            type_,
            set,
            conf,
            offset: 0,
            post: std::ptr::null_mut(),
        }
    }

    #[test]
    fn signature_checks_match_setters() {
        let flag = command(
            b"gzip",
            0,
            Some(ngx::ffi::ngx_conf_set_flag_slot),
            NGX_HTTP_LOC_CONF_OFFSET,
        );
        assert!(is_flag_command(&flag));
        assert!(!is_enum_command(&flag));
        // Main-conf placement fails the local check.
        let main = command(
            b"gzip",
            NGX_MAIN_CONF as usize,
            Some(ngx::ffi::ngx_conf_set_flag_slot),
            NGX_HTTP_LOC_CONF_OFFSET,
        );
        assert!(!is_flag_command(&main));
        let types = command(
            b"gzip_types",
            0,
            Some(ngx::ffi::ngx_http_types_slot),
            NGX_HTTP_LOC_CONF_OFFSET,
        );
        assert!(is_types_command(&types));
        let version = command(
            b"gzip_http_version",
            0,
            Some(ngx::ffi::ngx_conf_set_enum_slot),
            NGX_HTTP_LOC_CONF_OFFSET,
        );
        assert!(is_enum_command(&version));
        let proxied = command(
            b"gzip_proxied",
            0,
            Some(ngx::ffi::ngx_conf_set_bitmask_slot),
            NGX_HTTP_LOC_CONF_OFFSET,
        );
        assert!(is_bitmask_command(&proxied));
    }

    #[test]
    fn setter_starts_disarmed() {
        assert!(!GzipSetter::new().enabled());
    }
}
