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
#[derive(Clone)]
pub struct NgxMessageHandler {
    log: *mut ngx_log_t,
    buffer: Option<std::sync::Arc<std::sync::Mutex<ps_admin::log_buffer::LogBuffer>>>,
}

impl std::fmt::Debug for NgxMessageHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NgxMessageHandler")
            .field("log", &format_args!("{:p}", self.log))
            .field("buffered", &self.buffer.is_some())
            .finish()
    }
}

// SAFETY: the handler only forwards a raw log pointer to
// `ngx_log_error`, which is thread-safe in nginx; the pointer
// itself is set at config time before workers fork.
unsafe impl Send for NgxMessageHandler {}
unsafe impl Sync for NgxMessageHandler {}

impl NgxMessageHandler {
    /// Creates a handler with no log (stderr fallback) and no
    /// message buffer.
    #[must_use]
    pub fn new() -> Self {
        NgxMessageHandler {
            log: ptr::null_mut(),
            buffer: None,
        }
    }

    /// Attaches the shared message buffer (ring fanout).
    pub fn set_buffer(
        &mut self,
        buffer: std::sync::Arc<std::sync::Mutex<ps_admin::log_buffer::LogBuffer>>,
    ) {
        self.buffer = Some(buffer);
    }

    /// Dumps the buffered messages (`Dump`): `None` when no
    /// buffer is attached or message buffering is disabled.
    #[must_use]
    pub fn dump_messages(&self) -> Option<String> {
        let buffer = self.buffer.as_ref()?;
        buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dump_checked()
    }

    /// Borrows the shared message buffer, when attached (the
    /// per-request admin site renders history from this).
    #[must_use]
    pub fn message_buffer(
        &self,
    ) -> Option<&std::sync::Arc<std::sync::Mutex<ps_admin::log_buffer::LogBuffer>>> {
        self.buffer.as_ref()
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
        if let Some(buffer) = self.buffer.as_ref() {
            buffer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .message(level, formatted);
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

/// Buffer-only message sink for pool workers
/// (`ps_html::MessageSink`): background completions report here so
/// the admin pages see fire-and-forget optimizations. Deliberately
/// skips the error log (a raw `ngx_log_t` must never be touched
/// from pool threads, whose lifetime the log does not bound).
#[derive(Debug, Clone)]
pub struct BufferMessageSink {
    buffer: std::sync::Arc<std::sync::Mutex<ps_admin::log_buffer::LogBuffer>>,
}

impl BufferMessageSink {
    /// Wraps the shared message buffer.
    #[must_use]
    pub fn new(
        buffer: std::sync::Arc<std::sync::Mutex<ps_admin::log_buffer::LogBuffer>>,
    ) -> Self {
        BufferMessageSink { buffer }
    }
}

impl ps_html::server_context::MessageSink for BufferMessageSink {
    fn message(&self, level: MessageLevel, text: String) {
        self.buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .message(level, format_message(&text));
    }
}

impl ps_config::message_handler::MessageHandler for &NgxMessageHandler {
    /// Forwards through a shared borrow (the inherent emit is
    /// `&self` already): lets request paths pass the server
    /// handler while holding other shared borrows.
    fn message(&mut self, level: MessageLevel, text: String) {
        // SAFETY: by the contract on the inherent method, which
        // the server handler upholds as documented there.
        unsafe {
            (**self).message(level, &text);
        }
    }
}

impl ps_config::message_handler::MessageHandler for NgxMessageHandler {
    /// Forwards option-time messages (`Message`): the server
    /// handler's log is attached at child init and outlives
    /// every request, so the caller's contract holds for the
    /// handler the resource path passes here.
    fn message(&mut self, level: MessageLevel, text: String) {
        // SAFETY: by the documented construction above.
        unsafe {
            NgxMessageHandler::message(self, level, &text);
        }
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
        assert_eq!(
            NGX_LOG_INFO as ngx_uint_t,
            ngx_log_level_for(MessageLevel::Info)
        );
        assert_eq!(
            NGX_LOG_WARN as ngx_uint_t,
            ngx_log_level_for(MessageLevel::Warning)
        );
        assert_eq!(
            NGX_LOG_ERR as ngx_uint_t,
            ngx_log_level_for(MessageLevel::Error)
        );
        assert_eq!(
            NGX_LOG_ALERT as ngx_uint_t,
            ngx_log_level_for(MessageLevel::Fatal)
        );
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
    fn buffer_sink_reports_without_log() {
        use ps_html::server_context::MessageSink;
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(
            ps_admin::log_buffer::LogBuffer::new(1 << 20),
        ));
        let sink = BufferMessageSink::new(std::sync::Arc::clone(&buffer));
        sink.message(MessageLevel::Info, "PS_IMAGE_DETAIL\t1".to_string());
        let dump = buffer.lock().unwrap().dump();
        assert!(dump.contains("PS_IMAGE_DETAIL\t1"), "{dump}");
    }

    #[test]
    fn messages_fan_out_to_buffer() {
        use std::sync::{Arc, Mutex};
        let buffer = Arc::new(Mutex::new(ps_admin::log_buffer::LogBuffer::new(1000)));
        let mut handler = NgxMessageHandler::new();
        // SAFETY: null log falls back to stderr; no live log needed.
        unsafe {
            handler.message(MessageLevel::Warning, "unbuffered");
        }
        assert!(buffer.lock().unwrap().dump().is_empty());
        handler.set_buffer(Arc::clone(&buffer));
        // SAFETY: as above.
        unsafe {
            handler.message(MessageLevel::Warning, "buffered");
        }
        assert!(buffer.lock().unwrap().dump().contains("buffered"));
    }

    #[test]
    fn dump_needs_buffer_and_capacity() {
        use std::sync::{Arc, Mutex};
        assert_eq!(None, NgxMessageHandler::new().dump_messages());
        let mut handler = NgxMessageHandler::new();
        handler.set_buffer(Arc::new(Mutex::new(
            ps_admin::log_buffer::LogBuffer::disabled(),
        )));
        assert_eq!(None, handler.dump_messages());
        let buffer = Arc::new(Mutex::new(ps_admin::log_buffer::LogBuffer::new(1000)));
        handler.set_buffer(Arc::clone(&buffer));
        // SAFETY: null log falls back to stderr; no live log needed.
        unsafe {
            handler.message(MessageLevel::Warning, "buffered");
        }
        assert!(handler.dump_messages().unwrap().contains("buffered"));
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
