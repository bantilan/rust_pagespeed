//! Native fetcher (`NgxUrlAsyncFetcher` lifecycle + task protocol).
//!
//! Ports the fetch state machine: the pending/active/completed
//! sets with their shutdown/cancel rules, byte/count accounting,
//! the fetcher-key builder, the request serialization, the
//! user-agent fixup, and the keepalive protocol. Resolver calls,
//! socket I/O, nginx timers, and buffer pools stay FFI (the live
//! task driver calls back into the transition functions here).
//!
//! Serf is unsupported (2111 lines plus external serf/apr
//! libraries, unavailable offline): serf-configured fetchers
//! route here with a loud warning (see `allocate`).

use crate::message::MODULE_VERSION;
use ps_http::names::header;
use ps_http::request::RequestHeaders;
use std::collections::HashMap;

/// Task pool size (`ngx_create_pool(12288)` in `NgxFetch::Init`).
pub const FETCH_POOL_BYTES: usize = 12288;
/// Input buffer size (`ngx_create_temp_buf(pool, 4096)`).
pub const FETCH_INPUT_BYTES: usize = 4096;
/// Idle keepalive hold (`NgxConnection::keepalive_timeout_ms`).
pub const KEEPALIVE_TIMEOUT_MS: i64 = 60000;
/// Keepalive request header (`NgxConnection::ka_header`).
pub const KEEPALIVE_HEADER: &str = "keep-alive 60000";
/// Fallback user agent (`FixUserAgent` default).
pub const USER_AGENT_BASE: &str = "NgxNativeFetcher";
/// Subrequest product token (`kModPagespeedSubrequestUserAgent`).
pub const USER_AGENT_PRODUCT: &str = "mod_pagespeed";

/// User-agent suffix (` (mod_pagespeed/VERSION-LASTCHANGE)`).
/// Rust builds carry no changelist, so it reads `-0`.
#[must_use]
pub fn user_agent_suffix() -> String {
    format!(" ({USER_AGENT_PRODUCT}/{MODULE_VERSION}-0)")
}

/// Serf fallback warning (port-authored: C++ has no serf-less path).
pub const SERF_FALLBACK_WARNING: &str =
    "Serf fetching is not available in this build; falling back to the native fetcher.";

/// Fetcher init failure (`NgxUrlAsyncFetcher` ctor).
pub const INIT_FAILED: &str = "NgxUrlAsyncFetcher failed to init, fetching disabled.";
/// Pool failure (`Init`).
pub const INIT_POOL_FAILED: &str = "NgxUrlAsyncFetcher::Init create pool failed";

/// Proxy parse failure (`Init`, verbatim; C++ logs the proxy even
/// though it may carry credentials).
#[must_use]
pub fn init_proxy_failed(proxy: &str) -> String {
    format!("NgxUrlAsyncFetcher::Init parse proxy[{proxy}] failed")
}

/// Resolver start failure (`Init`).
pub const RESOLVE_START_FAILED: &str =
    "NgxFetch: Couldn't start resolving, is there a proper resolver configured in nginx.conf?";

/// Resolve failure (task id stands in for the C++ `%p`, which can
/// never match twice).
#[must_use]
pub fn resolve_failed(task: u64, host: &str) -> String {
    format!("NgxFetch {task}: failed to resolve host [{host}]")
}

/// No-IPv4 failure (same id substitution; the native fetcher is
/// IPv4-only).
#[must_use]
pub fn no_suitable_address(task: u64, host: &str) -> String {
    format!("NgxFetch {task}: no suitable address for host [{host}]")
}

/// Request-build failure (resolve path and IP-literal path share it).
pub const INIT_REQUEST_FAILED: &str = "NgxFetch: InitRequest failed";

/// Start failure (`StartFetch` warning).
#[must_use]
pub fn fetch_failed_to_start(url: &str) -> String {
    format!("Fetch failed to start: {url}")
}

/// Destructor note (info).
#[must_use]
pub fn destruct_active(active: usize) -> String {
    format!("Destruct NgxUrlAsyncFetcher with [{active}] active fetchers")
}

/// Active-fetch listing (info, one per fetch).
#[must_use]
pub fn active_fetch_report(url: &str) -> String {
    format!("Active fetch: {url}")
}

/// Body overflow (the connection must not be reused).
#[must_use]
pub fn body_overflow(task: u64) -> String {
    format!("NgxFetch {task}: response body exceeds its content-length")
}

/// Negative content length (error).
pub const NEGATIVE_CONTENT_LENGTH: &str = "Negative content-length in response header";

/// Status-line parse failure (warning).
pub const STATUS_LINE_FAILED: &str = "NgxFetch: failed to parse status line";

/// Task pool failure (error).
pub const TASK_POOL_FAILED: &str = "NgxFetch: ngx_create_pool failed";

/// Task URL parse failure (error; `detail` is nginx's parse error).
#[must_use]
pub fn parse_url_failed(url: &str, detail: &str) -> String {
    format!("NgxFetch: ParseUrl() failed for [{url}]:{detail}")
}

/// Timeout-event allocation failure (error).
pub const TIMEOUT_ALLOC_FAILED: &str = "NgxFetch: ngx_pcalloc failed for timeout";
/// Request-struct allocation failure (error; the "for timer" text
/// is verbatim even though the struct is the request).
pub const REQUEST_ALLOC_FAILED: &str = "NgxFetch: ngx_pcalloc failed for timer";
/// Status-struct allocation failure (error).
pub const STATUS_ALLOC_FAILED: &str = "NgxFetch: ngx_pcalloc failed for status";

/// Double-completion (`CallbackDone` guard; C++ `LOG(FATAL)`s with
/// a report plea).
#[must_use]
pub fn double_done(task: u64, url: &str) -> String {
    format!(
        "BUG: NgxFetch callback called more than once on same fetch\
         {url}({task}).Please report this at\
         https://groups.google.com/forum/#!forum/ngx-pagespeed-discuss"
    )
}

/// Scheme strip (`ParseUrl` prefix half): byte offset plus default
/// port. Case-insensitive; anything else stays whole on port 80.
/// The `ngx_parse_url` remainder runs live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrippedScheme {
    /// Bytes to skip past the scheme.
    pub offset: usize,
    /// Default port for the scheme.
    pub default_port: u16,
}

/// Strips the URL scheme.
#[must_use]
pub fn strip_scheme(url: &str) -> StrippedScheme {
    if url.len() >= 7 && url[..7].eq_ignore_ascii_case("http://") {
        StrippedScheme {
            offset: 7,
            default_port: 80,
        }
    } else if url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://") {
        StrippedScheme {
            offset: 8,
            default_port: 443,
        }
    } else {
        StrippedScheme {
            offset: 0,
            default_port: 80,
        }
    }
}

/// Rewrites the outgoing user agent (`FixUserAgent`): joins any
/// present values with spaces, defaults to the native token,
/// appends the version suffix unless already there.
pub fn fix_user_agent(headers: &mut RequestHeaders) {
    let mut user_agent = String::new();
    if let Some(values) = headers.headers().lookup(header::USER_AGENT) {
        for (index, value) in values.iter().enumerate() {
            if index != 0 {
                user_agent.push(' ');
            }
            user_agent.push_str(value);
        }
        headers.headers_mut().remove_all(header::USER_AGENT);
    }
    if user_agent.is_empty() {
        user_agent.push_str(USER_AGENT_BASE);
    }
    let suffix = user_agent_suffix();
    if !user_agent.ends_with(suffix.as_str()) {
        user_agent.push_str(&suffix);
    }
    headers.headers_mut().add(header::USER_AGENT, &user_agent);
}

/// Serializes the origin request (`InitRequest` past `Connect`):
/// `METHOD uri HTTP/1.0`, a derived `Host` (always with `:port`)
/// when none was given, then the headers in order. When the live
/// connection is keepalive the `Connection` header joins first.
/// Origin-form always, even via proxy (verbatim).
pub fn build_fetch_request(
    headers: &mut RequestHeaders,
    uri: &str,
    host: &str,
    port: u16,
    keepalive_connected: bool,
) -> Vec<u8> {
    if keepalive_connected {
        headers
            .headers_mut()
            .add(header::CONNECTION, KEEPALIVE_HEADER);
    }
    let mut out = Vec::new();
    out.extend_from_slice(headers.method().as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(uri.as_bytes());
    out.extend_from_slice(b" HTTP/1.0\r\n");
    let have_host = headers
        .headers()
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("Host"));
    if !have_host {
        out.extend_from_slice(b"Host: ");
        out.extend_from_slice(host.as_bytes());
        out.extend_from_slice(format!(":{port}").as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    for (name, value) in headers.headers().iter() {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// Keepalive reuse decision (`CallbackDone`): only successful
/// responses whose first decisive `Connection` value says
/// `keep-alive` (exact, case-sensitive; `close` vetoes).
#[must_use]
pub fn keepalive_response_wanted(success: bool, connection_values: &[&str]) -> bool {
    if !success {
        return false;
    }
    for value in connection_values {
        if *value == "keep-alive" {
            return true;
        }
        if *value == "close" {
            break;
        }
    }
    false
}

/// One-way keepalive latch (`NgxConnection::set_keepalive`: once
/// disabled, never re-enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepaliveLatch {
    keepalive: bool,
}

impl KeepaliveLatch {
    /// Starts enabled (the pool decides per response).
    #[must_use]
    pub fn new() -> Self {
        KeepaliveLatch { keepalive: true }
    }

    /// Narrows toward disabled.
    pub fn set_keepalive(&mut self, keepalive: bool) {
        self.keepalive = self.keepalive && keepalive;
    }

    /// Whether reuse is still allowed.
    #[must_use]
    pub const fn keepalive(self) -> bool {
        self.keepalive
    }
}

impl Default for KeepaliveLatch {
    fn default() -> Self {
        KeepaliveLatch::new()
    }
}

/// Fetch task id.
pub type FetchId = u64;

/// One tracked fetch (live tasks add resolver, connection, and
/// buffer state around this record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchTask {
    /// Task id.
    pub id: FetchId,
    /// Fetch URL.
    pub url: String,
    /// Bytes received so far.
    pub bytes_received: i64,
}

/// Fetch submission outcome (`Fetch` past the inflation wrap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSubmit {
    /// Queued; the caller notifies the main thread.
    Queued(FetchId),
    /// Shut down; the caller completes the callback with failure.
    Rejected,
}

/// Task start decision (`StartFetch` past activation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDecision {
    /// Run the live task driver.
    Proceed,
    /// Shut down mid-start; the caller fails the task.
    Abort,
}

/// Native fetcher (`NgxUrlAsyncFetcher` lifecycle core).
#[derive(Debug)]
pub struct NativeFetcher {
    next_id: FetchId,
    pending: Vec<FetchTask>,
    active: HashMap<FetchId, FetchTask>,
    completed: Vec<FetchTask>,
    shutdown: bool,
    fetchers_count: i32,
    byte_count: i64,
}

impl NativeFetcher {
    /// Creates an idle fetcher (counters zeroed, like C++).
    #[must_use]
    pub fn new() -> Self {
        NativeFetcher {
            next_id: 0,
            pending: Vec::new(),
            active: HashMap::new(),
            completed: Vec::new(),
            shutdown: false,
            fetchers_count: 0,
            byte_count: 0,
        }
    }

    /// Whether shutdown latched.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    /// Queues a fetch (`Fetch`): shutdown rejects; otherwise the
    /// task parks in pending and the caller writes the event.
    pub fn fetch(&mut self, url: &str) -> FetchSubmit {
        if self.shutdown {
            return FetchSubmit::Rejected;
        }
        self.next_id += 1;
        let task = FetchTask {
            id: self.next_id,
            url: url.to_string(),
            bytes_received: 0,
        };
        self.pending.push(task);
        FetchSubmit::Queued(self.next_id)
    }

    /// Starts drained fetches (`ReadCallback`): drops the previous
    /// completed set, then hands over every pending task in order.
    pub fn drain_pending(&mut self) -> Vec<FetchTask> {
        self.completed.clear();
        std::mem::take(&mut self.pending)
    }

    /// Activates one drained task (`StartFetch`): joins active and
    /// counts up before checking shutdown, so an abort still
    /// balances through completion.
    pub fn start_fetch(&mut self, task: FetchTask) -> StartDecision {
        let id = task.id;
        self.active.insert(id, task);
        self.fetchers_count += 1;
        if self.shutdown {
            StartDecision::Abort
        } else {
            StartDecision::Proceed
        }
    }

    /// Completes a task (`FetchComplete`): folds its bytes, counts
    /// down, and parks it for the next drain's cleanup. Panics on
    /// an unknown id (double completion is `LOG(FATAL)` in C++).
    pub fn fetch_complete(&mut self, id: FetchId, bytes_received: i64) {
        let mut task = self.active.remove(&id).expect("fetch completed twice");
        task.bytes_received = bytes_received;
        self.byte_count += bytes_received;
        self.fetchers_count -= 1;
        self.completed.push(task);
    }

    /// Shuts down (`ShutDown`): latches, then hands over every
    /// pending task for failure (never started, so no accounting).
    /// The caller tears the event channel down and fails the
    /// returned tasks; active ones go through `cancel_active`.
    pub fn shutdown(&mut self) -> Vec<FetchTask> {
        self.shutdown = true;
        std::mem::take(&mut self.pending)
    }

    /// Cancels active fetches (`CancelActiveFetches`): every
    /// active task completes with failure (bytes still fold).
    /// Returns the cancelled ids for the live driver.
    pub fn cancel_active(&mut self) -> Vec<FetchId> {
        let ids: Vec<FetchId> = self.active.keys().copied().collect();
        for id in &ids {
            let task = self.active.remove(id).expect("snapshotted above");
            self.byte_count += task.bytes_received;
            self.fetchers_count -= 1;
            self.completed.push(task);
        }
        ids
    }

    /// Whether fetches may be outstanding (`AnyPendingFetches`,
    /// which despite the name consults the active set).
    #[must_use]
    pub fn has_pending_fetches(&self) -> bool {
        !self.active.is_empty()
    }

    /// Approximate active count (error reporting only).
    #[must_use]
    pub fn approximate_active_fetches(&self) -> usize {
        self.active.len()
    }

    /// Active-fetch listing (`PrintActiveFetches`, info level).
    #[must_use]
    pub fn active_fetch_report(&self) -> Vec<String> {
        self.active
            .values()
            .map(|task| active_fetch_report(&task.url))
            .collect()
    }

    /// Live active count.
    #[must_use]
    pub fn fetchers_count(&self) -> i32 {
        self.fetchers_count
    }

    /// Total bytes folded through completion.
    #[must_use]
    pub fn byte_count(&self) -> i64 {
        self.byte_count
    }
}

impl Default for NativeFetcher {
    fn default() -> Self {
        NativeFetcher::new()
    }
}

impl Drop for NativeFetcher {
    /// Shutdown must precede destruction (`DCHECK(shutdown_)`).
    fn drop(&mut self) {
        debug_assert!(self.shutdown, "shut down before destructing NativeFetcher");
    }
}

/// Fetcher-key inputs (`GetFetcherKey`).
pub struct FetcherKeyInput<'a> {
    /// Context unplugged (collapses the key to `"unplugged"`).
    pub unplugged: bool,
    /// Factory `list_outstanding_urls_on_error`.
    pub list_errors: bool,
    /// `FetchProxy` value.
    pub proxy: &'a str,
    /// `FetchWithGzip` value.
    pub gzip: bool,
    /// Factory `track_original_content_length`.
    pub track_content_length: bool,
    /// `FetcherTimeOutMs` value.
    pub timeout_ms: i64,
}

/// Builds the fetcher map key (`GetFetcherKey`, verbatim layout).
/// Slurping is unsupported so its branch is omitted; the
/// https/cert suffix stays byte-identical at default (empty)
/// values.
#[must_use]
pub fn fetcher_key(input: &FetcherKeyInput) -> String {
    if input.unplugged {
        return "unplugged".to_string();
    }
    format!(
        "{}\n{}\n{}\n{}timeout: {}\n\nhttps: \ncert_dir: \ncert_file: ",
        if input.list_errors {
            "list_errors"
        } else {
            "no_errors"
        },
        input.proxy,
        if input.gzip {
            "fetch_with_gzip"
        } else {
            "no_gzip"
        },
        if input.track_content_length {
            "track_content_length\n"
        } else {
            "no_track\n"
        },
        input.timeout_ms,
    )
}

/// Selects the fetcher implementation (`AllocateFetcher`): always
/// native; serf-configured callers get the loud fallback warning
/// (C++ would build serf here).
#[must_use]
pub fn allocate_fetcher(use_native_fetcher: bool) -> Option<String> {
    if use_native_fetcher {
        None
    } else {
        Some(SERF_FALLBACK_WARNING.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_http::request::Method;

    #[test]
    fn scheme_strip_matrix() {
        assert_eq!(
            strip_scheme("http://h/p"),
            StrippedScheme {
                offset: 7,
                default_port: 80
            }
        );
        assert_eq!(
            strip_scheme("HTTP://h/p"),
            StrippedScheme {
                offset: 7,
                default_port: 80
            }
        );
        assert_eq!(
            strip_scheme("https://h/p"),
            StrippedScheme {
                offset: 8,
                default_port: 443
            }
        );
        assert_eq!(
            strip_scheme("h/p"),
            StrippedScheme {
                offset: 0,
                default_port: 80
            }
        );
        assert_eq!(
            strip_scheme(""),
            StrippedScheme {
                offset: 0,
                default_port: 80
            }
        );
    }

    #[test]
    fn user_agent_fixup() {
        let suffix = user_agent_suffix();
        assert!(suffix.starts_with(" (mod_pagespeed/"));
        assert!(suffix.ends_with("-0)"));
        // Empty defaults to the native token plus suffix.
        let mut headers = RequestHeaders::new();
        fix_user_agent(&mut headers);
        assert_eq!(
            headers.headers().lookup1(header::USER_AGENT),
            Some(format!("NgxNativeFetcher{suffix}"))
        );
        // Present values join with spaces; the suffix appends once.
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add(header::USER_AGENT, "a");
        headers.headers_mut().add(header::USER_AGENT, "b");
        fix_user_agent(&mut headers);
        fix_user_agent(&mut headers);
        assert_eq!(
            headers.headers().lookup(header::USER_AGENT),
            Some(vec![format!("a b{suffix}")])
        );
    }

    #[test]
    fn request_serialization() {
        // Derived host always carries the port; keepalive adds the
        // connection header ahead of serialization.
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Get);
        headers.headers_mut().add("Accept", "*/*");
        let bytes = build_fetch_request(&mut headers, "/p?q=1", "h", 80, true);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(
            text,
            "GET /p?q=1 HTTP/1.0\r\n\
             Host: h:80\r\n\
             Accept: */*\r\n\
             Connection: keep-alive 60000\r\n\
             \r\n"
        );
        // An explicit host (any case) suppresses the derived one.
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Head);
        headers.headers_mut().add("HOST", "x:8080");
        let bytes = build_fetch_request(&mut headers, "/", "h", 80, false);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "HEAD / HTTP/1.0\r\nHOST: x:8080\r\n\r\n");
        assert_eq!("ERROR", Method::Unknown.as_str());
    }

    #[test]
    fn keepalive_rules() {
        assert!(keepalive_response_wanted(true, &["keep-alive"]));
        assert!(!keepalive_response_wanted(false, &["keep-alive"]));
        assert!(!keepalive_response_wanted(true, &[]));
        // First decisive value wins; matching is exact.
        assert!(!keepalive_response_wanted(true, &["close", "keep-alive"]));
        assert!(!keepalive_response_wanted(true, &["Keep-Alive"]));
        let mut latch = KeepaliveLatch::new();
        assert!(latch.keepalive());
        latch.set_keepalive(false);
        assert!(!latch.keepalive());
        latch.set_keepalive(true);
        assert!(!latch.keepalive());
    }

    #[test]
    fn lifecycle_queues_activates_completes() {
        let mut fetcher = NativeFetcher::new();
        assert_eq!(FetchSubmit::Queued(1), fetcher.fetch("http://h/a"));
        assert_eq!(FetchSubmit::Queued(2), fetcher.fetch("http://h/b"));
        assert!(!fetcher.has_pending_fetches());
        // Drain hands over pending in order; starting activates.
        let drained = fetcher.drain_pending();
        assert_eq!(
            vec!["http://h/a", "http://h/b"],
            drained.iter().map(|t| t.url.as_str()).collect::<Vec<_>>()
        );
        assert!(fetcher.drain_pending().is_empty());
        assert_eq!(
            StartDecision::Proceed,
            fetcher.start_fetch(drained[0].clone())
        );
        assert_eq!(
            StartDecision::Proceed,
            fetcher.start_fetch(drained[1].clone())
        );
        assert_eq!(2, fetcher.fetchers_count());
        assert!(fetcher.has_pending_fetches());
        assert_eq!(2, fetcher.approximate_active_fetches());
        let mut report = fetcher.active_fetch_report();
        report.sort();
        assert_eq!(
            report,
            vec![
                "Active fetch: http://h/a".to_string(),
                "Active fetch: http://h/b".to_string(),
            ]
        );
        // Completion folds bytes, counts down, parks for cleanup.
        fetcher.fetch_complete(1, 100);
        assert_eq!(100, fetcher.byte_count());
        assert_eq!(1, fetcher.fetchers_count());
        // Shutdown rejects new work and hands over pending (none).
        assert!(fetcher.shutdown().is_empty());
        assert!(fetcher.is_shutdown());
        assert_eq!(FetchSubmit::Rejected, fetcher.fetch("http://h/c"));
        // Active tasks cancel with their bytes folded.
        assert_eq!(vec![2], fetcher.cancel_active());
        assert_eq!(0, fetcher.fetchers_count());
        assert!(!fetcher.has_pending_fetches());
    }

    #[test]
    fn shutdown_hands_over_pending_unstarted() {
        let mut fetcher = NativeFetcher::new();
        assert_eq!(FetchSubmit::Queued(1), fetcher.fetch("http://h/a"));
        let pending = fetcher.shutdown();
        assert_eq!(vec![1], pending.iter().map(|t| t.id).collect::<Vec<_>>());
        assert_eq!(0, fetcher.fetchers_count());
        assert_eq!(0, fetcher.byte_count());
        // A start racing shutdown still activates, then aborts.
        let mut fetcher = NativeFetcher::new();
        assert_eq!(FetchSubmit::Queued(1), fetcher.fetch("http://h/a"));
        let drained = fetcher.drain_pending();
        let _ = fetcher.shutdown();
        assert_eq!(
            StartDecision::Abort,
            fetcher.start_fetch(drained[0].clone())
        );
        assert_eq!(1, fetcher.fetchers_count());
        fetcher.fetch_complete(1, 0);
        assert_eq!(0, fetcher.fetchers_count());
    }

    #[test]
    #[should_panic(expected = "fetch completed twice")]
    fn double_complete_panics() {
        let mut fetcher = NativeFetcher::new();
        assert_eq!(FetchSubmit::Queued(1), fetcher.fetch("http://h/a"));
        let drained = fetcher.drain_pending();
        assert_eq!(
            StartDecision::Proceed,
            fetcher.start_fetch(drained[0].clone())
        );
        fetcher.fetch_complete(1, 0);
        // Shut down first so unwinding past the drop stays quiet.
        let _ = fetcher.shutdown();
        fetcher.fetch_complete(1, 0);
    }

    #[test]
    #[should_panic(expected = "shut down before destructing")]
    fn drop_without_shutdown_panics() {
        let _fetcher = NativeFetcher::new();
    }

    #[test]
    fn fetcher_key_verbatim() {
        assert_eq!(
            fetcher_key(&FetcherKeyInput {
                unplugged: true,
                list_errors: true,
                proxy: "p",
                gzip: true,
                track_content_length: true,
                timeout_ms: 1,
            }),
            "unplugged"
        );
        assert_eq!(
            fetcher_key(&FetcherKeyInput {
                unplugged: false,
                list_errors: false,
                proxy: "",
                gzip: false,
                track_content_length: false,
                timeout_ms: 5000,
            }),
            "no_errors\n\nno_gzip\nno_track\ntimeout: 5000\n\nhttps: \ncert_dir: \ncert_file: "
        );
        assert_eq!(
            fetcher_key(&FetcherKeyInput {
                unplugged: false,
                list_errors: true,
                proxy: "http://proxy/",
                gzip: true,
                track_content_length: true,
                timeout_ms: 2500,
            }),
            "list_errors\nhttp://proxy/\nfetch_with_gzip\ntrack_content_length\ntimeout: 2500\n\nhttps: \ncert_dir: \ncert_file: "
        );
    }

    #[test]
    fn selection_warns_on_serf() {
        assert_eq!(None, allocate_fetcher(true));
        assert_eq!(
            Some(SERF_FALLBACK_WARNING.to_string()),
            allocate_fetcher(false)
        );
    }

    #[test]
    fn message_texts() {
        assert_eq!(
            INIT_FAILED,
            "NgxUrlAsyncFetcher failed to init, fetching disabled."
        );
        assert_eq!(
            init_proxy_failed("http://p/"),
            "NgxUrlAsyncFetcher::Init parse proxy[http://p/] failed"
        );
        assert_eq!(
            resolve_failed(7, "h"),
            "NgxFetch 7: failed to resolve host [h]"
        );
        assert_eq!(
            no_suitable_address(7, "h"),
            "NgxFetch 7: no suitable address for host [h]"
        );
        assert_eq!(
            fetch_failed_to_start("http://h/a"),
            "Fetch failed to start: http://h/a"
        );
        assert_eq!(
            destruct_active(2),
            "Destruct NgxUrlAsyncFetcher with [2] active fetchers"
        );
        assert_eq!(
            REQUEST_ALLOC_FAILED,
            "NgxFetch: ngx_pcalloc failed for timer"
        );
        assert_eq!(KEEPALIVE_HEADER, "keep-alive 60000");
        assert_eq!(FETCH_POOL_BYTES, 12288);
        assert_eq!(FETCH_INPUT_BYTES, 4096);
        assert_eq!(
            double_done(9, "http://h/a"),
            "BUG: NgxFetch callback called more than once on same fetch\
             http://h/a(9).Please report this at\
             https://groups.google.com/forum/#!forum/ngx-pagespeed-discuss"
        );
    }
}
