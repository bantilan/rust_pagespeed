//! `NgxMessageHandler`: pagespeed messages → `ngx_log_error`.
//!
//! Ports `ngx_message_handler.{h,cc}`: the severity map, the
//! `[ngx_pagespeed <version>]` formats, the stderr fallback, and
//! the crash-signal installer. The log sink (an `ngx_log_t`)
//! only exists inside nginx; the handler holds it opaquely and
//! every FFI touch carries a `SAFETY:` comment.

use ngx::ffi::{ngx_log_t, ngx_uint_t, NGX_LOG_ALERT, NGX_LOG_ERR, NGX_LOG_INFO, NGX_LOG_WARN};
use ps_config::message_handler::MessageLevel;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Tag prefixed to every message (`kModuleName`).
pub const MODULE_TAG: &str = "ngx_pagespeed";

/// Module version stamped into every message
/// (`kModPagespeedVersion`, here the crate version).
pub const MODULE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Crash signals trapped for backtraces (`InstallCrashHandler`).
pub const CRASH_SIGNALS: &[i32] = &[libc::SIGTRAP, libc::SIGABRT, libc::SIGFPE, libc::SIGSEGV];

/// Maps a message level to an nginx log level (`GetNgxLogLevel`).
#[must_use]
pub const fn ngx_log_level_for(level: MessageLevel) -> ngx_uint_t {
    match level {
        MessageLevel::Info => NGX_LOG_INFO as ngx_uint_t,
        MessageLevel::Warning => NGX_LOG_WARN as ngx_uint_t,
        MessageLevel::Error => NGX_LOG_ERR as ngx_uint_t,
        MessageLevel::Fatal => NGX_LOG_ALERT as ngx_uint_t,
    }
}

/// Formats a plain message (`MessageSImpl`): `[tag version] body`.
#[must_use]
pub fn format_message(message: &str) -> String {
    format!("[{MODULE_TAG} {MODULE_VERSION}] {message}")
}

/// Formats a file:line message (`FileMessageSImpl`).
#[must_use]
pub fn format_file_message(file: &str, line: u32, message: &str) -> String {
    format!("[{MODULE_TAG} {MODULE_VERSION}] {file}:{line}:{message}")
}

/// Log sink for pagespeed messages (`NgxMessageHandler`).
///
/// A null log routes to stderr (the `GoogleMessageHandler`
/// fallback); message-buffer accounting lives in ps-config's
/// handler and is wired by the driver factory slice.
pub struct NgxMessageHandler {
    log: *mut ngx_log_t,
}

impl std::fmt::Debug for NgxMessageHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NgxMessageHandler")
            .field("log", &format_args!("{:p}", self.log))
            .finish()
    }
}

// SAFETY: the handler only forwards a raw log pointer to
// `ngx_log_error`, which is thread-safe in nginx; the pointer
// itself is set at config time before workers fork.
unsafe impl Send for NgxMessageHandler {}
unsafe impl Sync for NgxMessageHandler {}

impl NgxMessageHandler {
    /// Creates a handler with no log (stderr fallback).
    #[must_use]
    pub const fn new() -> Self {
        NgxMessageHandler { log: ptr::null_mut() }
    }

    /// Points the handler at an nginx log (`set_log`).
    ///
    /// # Safety
    ///
    /// `log` must be null or point at a live `ngx_log_t` that
    /// outlives every message call.
    pub unsafe fn set_log(&mut self, log: *mut ngx_log_t) {
        // SAFETY: caller guarantees the pointer as documented.
        self.log = log;
    }

    /// Returns the current log, null when unset (`log()`).
    #[must_use]
    pub fn log(&self) -> *mut ngx_log_t {
        self.log
    }

    /// Emits a message (`MessageSImpl`).
    ///
    /// # Safety
    ///
    /// When a log is set it must point at a live `ngx_log_t`.
    pub unsafe fn message(&self, level: MessageLevel, text: &str) {
        let formatted = format_message(text);
        if self.log.is_null() {
            eprintln!("{formatted}");
        } else {
            // SAFETY: level gating reads `log_level` from the live
            // log the caller guarantees; the macro then calls
            // `ngx_log_error` with a stack buffer.
            ngx::ngx_log_error!(ngx_log_level_for(level), self.log, "{}", formatted);
        }
    }

    /// Emits a file:line message (`FileMessageSImpl`).
    ///
    /// # Safety
    ///
    /// When a log is set it must point at a live `ngx_log_t`.
    pub unsafe fn file_message(&self, level: MessageLevel, file: &str, line: u32, text: &str) {
        let formatted = format_file_message(file, line, text);
        if self.log.is_null() {
            eprintln!("{formatted}");
        } else {
            // SAFETY: as in `message`.
            ngx::ngx_log_error!(ngx_log_level_for(level), self.log, "{}", formatted);
        }
    }
}

impl Default for NgxMessageHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Log used by the crash handler (`global_log`).
static CRASH_LOG: AtomicPtr<ngx_log_t> = AtomicPtr::new(ptr::null_mut());

/// Crash-signal entry point (`signal_handler`).
///
/// Arms a 2s `alarm` abort, logs a backtrace at `ALERT` (stderr
/// when no log is installed), then `SIGKILL`s the process.
/// Like the C++, this runs async-signal-unsafe code (allocation,
/// logging) inside a handler and accepts the risk, contained by
/// the alarm; a crash dump is worth more than strict safety.
extern "C" fn crash_signal_handler(sig: i32) {
    // SAFETY: `alarm`, `kill`, and `getpid` are async-signal-safe.
    unsafe {
        libc::alarm(2);
    }
    let trace = std::backtrace::Backtrace::capture();
    let log = CRASH_LOG.load(Ordering::SeqCst);
    if log.is_null() {
        eprintln!("Trapped signal [{sig}]\n{trace}");
    } else {
        // SAFETY: the installed log is a live `ngx_log_t`; see
        // the caveat above for the deliberate signal unsafety.
        ngx::ngx_log_error!(NGX_LOG_ALERT, log, "Trapped signal [{}]\n{}", sig, trace);
    }
    // SAFETY: `kill`/`getpid` are async-signal-safe; SIGKILL
    // never returns.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
}

/// Installs the crash handler (`InstallCrashHandler`).
///
/// # Safety
///
/// `log` must be null or point at an `ngx_log_t` that stays
/// live until process exit; installing handlers is process
/// global. Call once at startup.
pub unsafe fn install_crash_handler(log: *mut ngx_log_t) {
    CRASH_LOG.store(log, Ordering::SeqCst);
    for &sig in CRASH_SIGNALS {
        // SAFETY: `signal` with a plain `extern "C"` handler is
        // the faithful port; previous dispositions are replaced.
        // The handler address crosses `signal(2)`'s integer-typed
        // API via a pointer cast, per the compiler's suggestion.
        unsafe {
            libc::signal(sig, crash_signal_handler as *const () as libc::sighandler_t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_nginx() {
        assert_eq!(NGX_LOG_INFO as ngx_uint_t, ngx_log_level_for(MessageLevel::Info));
        assert_eq!(NGX_LOG_WARN as ngx_uint_t, ngx_log_level_for(MessageLevel::Warning));
        assert_eq!(NGX_LOG_ERR as ngx_uint_t, ngx_log_level_for(MessageLevel::Error));
        assert_eq!(NGX_LOG_ALERT as ngx_uint_t, ngx_log_level_for(MessageLevel::Fatal));
    }

    #[test]
    fn formats_carry_tag_and_version() {
        assert_eq!(
            format!("[ngx_pagespeed {MODULE_VERSION}] hello"),
            format_message("hello")
        );
        assert_eq!(
            format!("[ngx_pagespeed {MODULE_VERSION}] f.cc:7:hello"),
            format_file_message("f.cc", 7, "hello")
        );
    }

    #[test]
    fn crash_set_matches_cxx() {
        assert_eq!(
            vec![libc::SIGTRAP, libc::SIGABRT, libc::SIGFPE, libc::SIGSEGV],
            CRASH_SIGNALS
        );
    }

    #[test]
    fn handler_defaults_to_null_log() {
        let handler = NgxMessageHandler::new();
        assert!(handler.log().is_null());
    }

    #[test]
    fn install_and_restore_crash_handler() {
        // SAFETY: installing then immediately restoring default
        // dispositions; no signal is delivered in between.
        unsafe {
            install_crash_handler(ptr::null_mut());
            for &sig in CRASH_SIGNALS {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
    }
}
