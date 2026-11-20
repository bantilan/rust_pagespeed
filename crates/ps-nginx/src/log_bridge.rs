//! Log bridge: library diagnostics → the nginx error log.
//!
//! Ports `log_message_handler.{h,cc}`: the severity map (with
//! `VLOG` falling through to `DEBUG`), the trailing-newline
//! trim, the `[ngx_pagespeed <version>]` format, and the
//! install-time verbosity rule. There is no Chromium `LOG()`
//! macro to hook in Rust; instead the bridge records the log
//! sink plus the verbose flag the install step derives, and the
//! message path consults them.

use crate::message::{MODULE_TAG, MODULE_VERSION};
use ngx::ffi::{
    ngx_log_t, ngx_uint_t, NGX_LOG_ALERT, NGX_LOG_DEBUG, NGX_LOG_ERR, NGX_LOG_INFO, NGX_LOG_WARN,
};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Severity codes (`logging::LOG_*`; `VLOG(n)` arrives as `-n`).
pub const LOG_INFO: i32 = 0;
pub const LOG_WARNING: i32 = 1;
pub const LOG_ERROR: i32 = 2;
pub const LOG_ERROR_REPORT: i32 = 3;
pub const LOG_FATAL: i32 = 4;

/// Maps a severity to an nginx log level (`GetNgxLogLevel`).
#[must_use]
pub const fn severity_level(severity: i32) -> ngx_uint_t {
    match severity {
        LOG_INFO => NGX_LOG_INFO as ngx_uint_t,
        LOG_WARNING => NGX_LOG_WARN as ngx_uint_t,
        LOG_ERROR => NGX_LOG_ERR as ngx_uint_t,
        LOG_ERROR_REPORT | LOG_FATAL => NGX_LOG_ALERT as ngx_uint_t,
        _ => NGX_LOG_DEBUG as ngx_uint_t,
    }
}

/// Strips one trailing newline (`LogMessageHandler` trim).
#[must_use]
pub fn trim_trailing_newline(message: &str) -> &str {
    message.strip_suffix('\n').unwrap_or(message)
}

/// Formats a bridged message: `[ngx_pagespeed <version>] body`.
#[must_use]
pub fn format_log_message(message: &str) -> String {
    format!(
        "[{MODULE_TAG} {MODULE_VERSION}] {}",
        trim_trailing_newline(message)
    )
}

/// Whether install enables verbose logging: the nginx log level
/// is at `DEBUG` or above (`Install`).
#[must_use]
pub const fn should_enable_verbose(nginx_log_level: ngx_uint_t) -> bool {
    nginx_log_level >= NGX_LOG_DEBUG as ngx_uint_t
}

/// Installed sink (`ngx_log`).
static BRIDGE_LOG: AtomicPtr<ngx_log_t> = AtomicPtr::new(ptr::null_mut());
/// Verbose (`VLOG(2)+`) flag (`SetMinLogLevel(-2)`).
static BRIDGE_VERBOSE: AtomicBool = AtomicBool::new(false);

/// Installs the bridge (`Install`). Call once at startup.
///
/// # Safety
///
/// `log` must point at an `ngx_log_t` that stays live until
/// process exit; installation is process global.
pub unsafe fn install(log: *mut ngx_log_t) {
    // SAFETY: caller guarantees a live log; the level read is a
    // plain field load.
    let level = unsafe { (*log).log_level };
    BRIDGE_LOG.store(log, Ordering::SeqCst);
    BRIDGE_VERBOSE.store(should_enable_verbose(level), Ordering::SeqCst);
}

/// Returns the installed sink, null when uninstalled.
#[must_use]
pub fn installed_log() -> *mut ngx_log_t {
    BRIDGE_LOG.load(Ordering::SeqCst)
}

/// Whether verbose logging is enabled.
#[must_use]
pub fn verbose_enabled() -> bool {
    BRIDGE_VERBOSE.load(Ordering::SeqCst)
}

/// Emits one bridged diagnostic (`LogMessageHandler`).
///
/// Fatal severities append a backtrace and then panic, mirroring
/// the C++ crash-for-dump (`BreakDebugger`); every other level
/// just logs. With no installed sink the message goes to stderr.
///
/// # Safety
///
/// When a sink is installed it must point at a live `ngx_log_t`.
pub unsafe fn emit(severity: i32, message: &str) {
    let mut body = trim_trailing_newline(message).to_string();
    if severity == LOG_FATAL {
        let trace = std::backtrace::Backtrace::capture();
        body.push_str(&trace.to_string());
    }
    let formatted = format!("[{MODULE_TAG} {MODULE_VERSION}] {body}");
    let log = BRIDGE_LOG.load(Ordering::SeqCst);
    if log.is_null() {
        eprintln!("{formatted}");
    } else {
        // SAFETY: caller guarantees the installed log is live.
        ngx::ngx_log_error!(severity_level(severity), log, "{}", formatted);
    }
    if severity == LOG_FATAL {
        panic!("fatal bridged diagnostic: {body}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_map() {
        assert_eq!(NGX_LOG_INFO as ngx_uint_t, severity_level(LOG_INFO));
        assert_eq!(NGX_LOG_WARN as ngx_uint_t, severity_level(LOG_WARNING));
        assert_eq!(NGX_LOG_ERR as ngx_uint_t, severity_level(LOG_ERROR));
        assert_eq!(
            NGX_LOG_ALERT as ngx_uint_t,
            severity_level(LOG_ERROR_REPORT)
        );
        assert_eq!(NGX_LOG_ALERT as ngx_uint_t, severity_level(LOG_FATAL));
        assert_eq!(NGX_LOG_DEBUG as ngx_uint_t, severity_level(-1));
        assert_eq!(NGX_LOG_DEBUG as ngx_uint_t, severity_level(-2));
        assert_eq!(NGX_LOG_DEBUG as ngx_uint_t, severity_level(99));
    }

    #[test]
    fn newline_trimmed_once() {
        assert_eq!("a", trim_trailing_newline("a\n"));
        assert_eq!("a\n", trim_trailing_newline("a\n\n"));
        assert_eq!("a", trim_trailing_newline("a"));
        assert_eq!("", trim_trailing_newline(""));
    }

    #[test]
    fn format_matches_cxx() {
        assert_eq!(
            format!("[ngx_pagespeed {MODULE_VERSION}] hi"),
            format_log_message("hi\n")
        );
    }

    #[test]
    fn verbosity_follows_debug_level() {
        assert!(should_enable_verbose(NGX_LOG_DEBUG as ngx_uint_t));
        assert!(!should_enable_verbose(NGX_LOG_INFO as ngx_uint_t));
    }

    #[test]
    fn uninstalled_bridge_reports_null() {
        // No install in unit tests: sink stays null, verbosity off.
        // (Runs before any install; install is covered live in Slice 7.)
        assert!(installed_log().is_null());
        assert!(!verbose_enabled());
    }

    #[test]
    fn emit_without_sink_goes_to_stderr() {
        // SAFETY: no sink installed, so no log is touched.
        unsafe {
            emit(LOG_INFO, "hello\n");
            emit(LOG_WARNING, "plain");
        }
    }

    #[test]
    #[should_panic(expected = "fatal bridged diagnostic")]
    fn fatal_emit_panics_for_dump() {
        // SAFETY: no sink installed, so no log is touched.
        unsafe {
            emit(LOG_FATAL, "boom");
        }
    }
}
