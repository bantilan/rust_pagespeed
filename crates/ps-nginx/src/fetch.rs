//! Base fetch bridge (`NgxBaseFetch` core plus the handler/filter
//! decision cores).
//!
//! Ports PageSpeed's output collection as single-threaded state
//! rules: write buffering with the 1 MiB flush window, flush/done
//! latches, the chain-copy policy (`NGX_AGAIN` until done,
//! exactly-once `last_buf`), the two-reference lifecycle with
//! in-flight event accounting, detach/suppress event gating, and
//! the handler/filter/read-callback decision matrices. The shared
//! wrapper (mutex, atomics), the pipe event channel, thread
//! integration, cache validation, and the nginx-call assembly land
//! in Slice 6; the nginx-call-free rules here are exact and tested.

use crate::caching::PreserveCachingHeaders;
use ps_http::response::{HttpOptions, ResponseHeaders};

/// Fetch role (`NgxBaseFetchType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFetchType {
    /// In-place lookup.
    IproLookup,
    /// HTML transform.
    HtmlTransform,
    /// `.pagespeed.` resource.
    PagespeedResource,
    /// Admin page.
    AdminPage,
    /// Proxied origin fetch.
    PagespeedProxy,
}

/// Role name for logs (`BaseFetchTypeToCStr`).
#[must_use]
pub const fn base_fetch_type_name(fetch_type: BaseFetchType) -> &'static str {
    match fetch_type {
        BaseFetchType::PagespeedResource => "ps resource",
        BaseFetchType::HtmlTransform => "html transform",
        BaseFetchType::AdminPage => "admin page",
        BaseFetchType::IproLookup => "ipro lookup",
        BaseFetchType::PagespeedProxy => "pagespeed proxy",
    }
}

/// Buffered-output flush window (`kMaxBufferedOutputBytes`, 1 MiB).
pub const MAX_BUFFERED_OUTPUT_BYTES: usize = 1 << 20;

/// Event kinds the fetch posts (`kHeadersComplete`/`kFlush`/`kDone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchEvent {
    /// Headers available.
    HeadersComplete,
    /// More body available.
    Flush,
    /// Fetch complete.
    Done,
}

/// Event byte, as written on the pipe.
#[must_use]
pub const fn event_byte(event: FetchEvent) -> u8 {
    match event {
        FetchEvent::HeadersComplete => b'H',
        FetchEvent::Flush => b'F',
        FetchEvent::Done => b'D',
    }
}

/// Event channel (the Slice 6 pipe): posts an event for a fetch,
/// reporting delivery. Slices 4–5 run against test doubles.
pub trait EventChannel {
    /// Posts an event; false when the channel is gone.
    fn write_event(&mut self, event: FetchEvent) -> bool;
}

/// Torn-down channel (post-`Terminate()` null): every write fails,
/// so events are dropped with no reference change.
pub struct NullChannel;

impl EventChannel for NullChannel {
    fn write_event(&mut self, _event: FetchEvent) -> bool {
        false
    }
}

/// Base fetch (`NgxBaseFetch` state core).
pub struct BaseFetch {
    url: String,
    fetch_type: BaseFetchType,
    preserve_caching_headers: PreserveCachingHeaders,
    response_headers: ResponseHeaders,
    buffer: Vec<u8>,
    need_flush: bool,
    done_called: bool,
    last_buf_sent: bool,
    references: i32,
    detached: bool,
    suppress: bool,
    content_length: Option<i64>,
}

impl BaseFetch {
    /// Creates a fetch (ctor past the mutex): two references (proxy
    /// plus nginx sides), empty buffer, nothing latched.
    #[must_use]
    pub fn new(
        url: &str,
        fetch_type: BaseFetchType,
        preserve_caching_headers: PreserveCachingHeaders,
        http_options: HttpOptions,
    ) -> Self {
        BaseFetch {
            url: url.to_string(),
            fetch_type,
            preserve_caching_headers,
            response_headers: ResponseHeaders::new(http_options),
            buffer: Vec::new(),
            need_flush: false,
            done_called: false,
            last_buf_sent: false,
            references: 2,
            detached: false,
            suppress: false,
            content_length: None,
        }
    }

    /// Fetch URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Fetch role.
    #[must_use]
    pub const fn fetch_type(&self) -> BaseFetchType {
        self.fetch_type
    }

    /// Caching-header preservation for the serve path.
    #[must_use]
    pub const fn preserve_caching_headers(&self) -> PreserveCachingHeaders {
        self.preserve_caching_headers
    }

    /// Buffered output bytes.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    /// Live reference count.
    #[must_use]
    pub fn references(&self) -> i32 {
        self.references
    }

    /// Whether nginx released its reference.
    #[must_use]
    pub fn detached(&self) -> bool {
        self.detached
    }

    /// Whether completion was signaled.
    #[must_use]
    pub fn done_called(&self) -> bool {
        self.done_called
    }

    /// Borrows the response headers.
    #[must_use]
    pub fn response_headers(&self) -> &ResponseHeaders {
        &self.response_headers
    }

    /// Mutably borrows the response headers.
    pub fn response_headers_mut(&mut self) -> &mut ResponseHeaders {
        &mut self.response_headers
    }

    /// Sets the known content length (`SetContentLength`).
    pub fn set_content_length(&mut self, length: i64) {
        self.content_length = Some(length);
    }

    /// Known content length, if set.
    #[must_use]
    pub fn content_length(&self) -> Option<i64> {
        self.content_length
    }

    /// Buffers output (`HandleWrite`): a window's worth of held
    /// output posts a flush event so large responses stream.
    pub fn handle_write(&mut self, bytes: &[u8], channel: &mut dyn EventChannel) {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() >= MAX_BUFFERED_OUTPUT_BYTES {
            self.need_flush = true;
            self.request_collection(FetchEvent::Flush, channel);
        }
    }

    /// Latches a flush and posts it (`HandleFlush`).
    pub fn handle_flush(&mut self, channel: &mut dyn EventChannel) {
        self.need_flush = true;
        self.request_collection(FetchEvent::Flush, channel);
    }

    /// Latches headers-complete (`HandleHeadersComplete`): posts
    /// the event, and suppresses later ones for a failed in-place
    /// lookup (its `Done` still posts, avoiding a race with nginx
    /// tearing the fetch down). The 404 stat bump needs live
    /// statistics (Slice 6).
    pub fn handle_headers_complete(&mut self, status_ok: bool, channel: &mut dyn EventChannel) {
        self.request_collection(FetchEvent::HeadersComplete, channel);
        if self.fetch_type == BaseFetchType::IproLookup && !status_ok {
            self.suppress = true;
        }
    }

    /// Latches completion (`HandleDone`): posts, then drops the
    /// fetch-side reference, reporting whether the fetch deleted
    /// itself. Panics on a second call (`CHECK(!done_called_)`).
    pub fn handle_done(&mut self, channel: &mut dyn EventChannel) -> bool {
        assert!(!self.done_called, "Done already called!");
        self.done_called = true;
        self.request_collection(FetchEvent::Done, channel);
        self.decref_and_delete_if_unreferenced()
    }

    /// Drops the nginx-side reference (`Detach`).
    pub fn detach(&mut self) -> bool {
        self.detached = true;
        self.decref_and_delete_if_unreferenced()
    }

    /// Accounts one in-flight event (`IncrementRefCount`).
    pub fn increment_ref_count(&mut self) {
        self.references += 1;
    }

    /// Releases one in-flight event (`DecrementRefCount`),
    /// reporting whether the fetch deleted itself.
    pub fn decrement_ref_count(&mut self) -> bool {
        self.decref_and_delete_if_unreferenced()
    }

    /// Decrements, reporting self-deletion
    /// (`DecrefAndDeleteIfUnreferenced`).
    fn decref_and_delete_if_unreferenced(&mut self) -> bool {
        self.references -= 1;
        self.references == 0
    }

    /// Posts an event for collection (`RequestCollection`): skipped
    /// while suppressed; the reference is taken before the write
    /// and returned when the write fails, so the fetch cannot die
    /// between the two. Against a torn-down channel the write
    /// fails and the reference nets to zero, like the C++ null.
    fn request_collection(&mut self, event: FetchEvent, channel: &mut dyn EventChannel) {
        if self.suppress {
            return;
        }
        self.increment_ref_count();
        if !channel.write_event(event) {
            self.decref_and_delete_if_unreferenced();
        }
    }

    /// Copies response headers into nginx (`CollectHeaders`):
    /// the known content length first (clearing the shortcut the
    /// copy re-adds), then every header. False on pool exhaustion.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request.
    pub unsafe fn collect_headers(
        &self,
        r: *mut ngx::ffi::ngx_http_request_t,
        location_field_set: bool,
        set_psol_vary: &mut bool,
    ) -> bool {
        // SAFETY: caller guarantees a live request.
        unsafe {
            if let Some(length) = self.content_length {
                (*r).headers_out.content_length = std::ptr::null_mut();
                (*r).headers_out.content_length_n = length;
            }
            crate::headers::copy_response_headers_to_nginx(
                r,
                &self.response_headers,
                self.preserve_caching_headers,
                location_field_set,
                set_psol_vary,
            )
        }
    }

    /// Collects buffered output into a chain
    /// (`CollectAccumulatedWrites`): the code plus the chain (null
    /// when nothing is ready).
    ///
    /// # Safety
    ///
    /// `pool` must point at a live pool.
    pub unsafe fn collect_accumulated_writes(
        &mut self,
        pool: *mut ngx::ffi::ngx_pool_t,
    ) -> (ngx::ffi::ngx_int_t, *mut ngx::ffi::ngx_chain_t) {
        use ngx::ffi::{NGX_AGAIN, NGX_ERROR, NGX_OK};
        // SAFETY: caller guarantees a live pool.
        unsafe {
            match self.copy_buffer_to_nginx(true) {
                CopyOutcome::Again { bytes, .. } if bytes.is_empty() => {
                    (NGX_AGAIN as ngx::ffi::ngx_int_t, std::ptr::null_mut())
                }
                CopyOutcome::Again { bytes, flush } => {
                    match crate::headers::string_piece_to_buffer_chain(pool, &bytes, false, flush) {
                        Some(link) => (NGX_AGAIN as ngx::ffi::ngx_int_t, link),
                        None => (NGX_ERROR as ngx::ffi::ngx_int_t, std::ptr::null_mut()),
                    }
                }
                CopyOutcome::Ok { bytes, flush } => {
                    match crate::headers::string_piece_to_buffer_chain(pool, &bytes, true, flush) {
                        Some(link) => (NGX_OK as ngx::ffi::ngx_int_t, link),
                        None => (NGX_ERROR as ngx::ffi::ngx_int_t, std::ptr::null_mut()),
                    }
                }
                CopyOutcome::Error => (NGX_ERROR as ngx::ffi::ngx_int_t, std::ptr::null_mut()),
            }
        }
    }

    /// Copies buffered output for nginx (`CopyBufferToNginx`):
    /// `Again` while more may come, `Ok` once done (setting
    /// `last_buf` exactly once), `Error` on allocation failure.
    /// Panics when called after the last buffer shipped (`CHECK`).
    pub fn copy_buffer_to_nginx(&mut self, allocate: bool) -> CopyOutcome {
        assert!(
            !(self.done_called && self.last_buf_sent),
            "CopyBufferToNginx() was called after the last buffer was sent"
        );
        if !self.done_called && self.buffer.is_empty() {
            return CopyOutcome::Again { bytes: Vec::new(), flush: false };
        }
        if !allocate {
            return CopyOutcome::Error;
        }
        let bytes = std::mem::take(&mut self.buffer);
        let flush = self.need_flush;
        self.need_flush = false;
        if self.done_called {
            self.last_buf_sent = true;
            return CopyOutcome::Ok { bytes, flush };
        }
        CopyOutcome::Again { bytes, flush }
    }
}

/// Chain-copy outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    /// More may come (`NGX_AGAIN`); carries bytes plus the flush
    /// flag (`last_buf` never set here).
    Again { bytes: Vec<u8>, flush: bool },
    /// Done (`NGX_OK`); the chain carries `last_buf`.
    Ok { bytes: Vec<u8>, flush: bool },
    /// Allocation failure (`NGX_ERROR`).
    Error,
}

/// Body-filter gate (`ps_base_fetch_filter` head).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseFetchGate {
    /// Header-only request completes (`NGX_OK`).
    Complete,
    /// No active fetch: pass the chain down.
    PassThrough,
    /// Active fetch: serve accumulated output.
    Serve,
}

/// Gates the base-fetch body filter.
#[must_use]
pub const fn base_fetch_gate(header_only: bool, fetch_active: bool) -> BaseFetchGate {
    if header_only {
        return BaseFetchGate::Complete;
    }
    if !fetch_active {
        return BaseFetchGate::PassThrough;
    }
    BaseFetchGate::Serve
}

/// Whether an error status declines to nginx (`ps_base_fetch_handler`
/// head): responses PageSpeed generates itself (neither HTML
/// transforms nor in-place lookups) let nginx render error statuses.
#[must_use]
pub const fn handler_defers_error(status_code: i32, fetch_type: BaseFetchType) -> bool {
    let status_ok = status_code != 0 && status_code < 400;
    !status_ok
        && !matches!(fetch_type, BaseFetchType::HtmlTransform | BaseFetchType::IproLookup)
}

/// Whether a response header survives the preserve pass: every
/// header goes unless preservation says keep (`Cache-Control`
/// always; Etag/Date/Last-Modified/Expires only when preserving
/// all).
#[must_use]
pub fn preserve_header_kept(name: &str, preserve: PreserveCachingHeaders) -> bool {
    if name.eq_ignore_ascii_case("Cache-Control") {
        return true;
    }
    matches!(preserve, PreserveCachingHeaders::PreserveAll)
        && (name.eq_ignore_ascii_case("Etag")
            || name.eq_ignore_ascii_case("Date")
            || name.eq_ignore_ascii_case("Last-Modified")
            || name.eq_ignore_ascii_case("Expires"))
}

/// Whether the handler discards the request body: every generated
/// response does, except the in-place lookup miss (the server's own
/// handler may need it) and HTML transforms (the upstream owns it).
#[must_use]
pub const fn discards_request_body(fetch_type: BaseFetchType, status_ok: bool) -> bool {
    let ipro_lookup_miss = matches!(fetch_type, BaseFetchType::IproLookup) && !status_ok;
    !ipro_lookup_miss && !matches!(fetch_type, BaseFetchType::HtmlTransform)
}

/// Send-header outcome (`ps_base_fetch_handler` rc handling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFilterOutcome {
    /// Finalize with this code (`NGX_ERROR` or above `NGX_OK`).
    Finalize(ngx::ffi::ngx_int_t),
    /// In-place check declined (`NGX_DONE`, `CHECK`ed).
    Done,
    /// Continue serving (`NGX_OK`/`NGX_AGAIN`).
    Continue,
}

/// Routes the send-headers result code.
#[must_use]
pub fn header_filter_outcome(rc: ngx::ffi::ngx_int_t) -> HeaderFilterOutcome {
    use ngx::ffi::{NGX_AGAIN, NGX_DONE, NGX_ERROR, NGX_OK};
    if rc == NGX_ERROR as ngx::ffi::ngx_int_t || rc > NGX_OK as ngx::ffi::ngx_int_t {
        HeaderFilterOutcome::Finalize(rc)
    } else if rc < NGX_OK as ngx::ffi::ngx_int_t && rc != NGX_AGAIN as ngx::ffi::ngx_int_t {
        assert_eq!(rc, NGX_DONE as ngx::ffi::ngx_int_t, "unexpected header filter code");
        HeaderFilterOutcome::Done
    } else {
        HeaderFilterOutcome::Continue
    }
}

/// Accumulated-writes outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectOutcome {
    /// Collection failed (`NGX_HTTP_INTERNAL_SERVER_ERROR`).
    InternalError,
    /// Nothing to send yet (`NGX_AGAIN`).
    WaitForMore,
    /// Done: release the fetch, then serve the chain.
    ReleaseAndServe,
    /// Serve the chain, staying buffered.
    Serve,
}

/// Routes the collect result (`NGX_ERROR`, `NGX_AGAIN` with no
/// chain, `NGX_OK`, anything else).
#[must_use]
pub fn collect_outcome(rc: ngx::ffi::ngx_int_t, chain_is_null: bool) -> CollectOutcome {
    use ngx::ffi::{NGX_AGAIN, NGX_ERROR, NGX_OK};
    if rc == NGX_ERROR as ngx::ffi::ngx_int_t {
        CollectOutcome::InternalError
    } else if rc == NGX_AGAIN as ngx::ffi::ngx_int_t && chain_is_null {
        CollectOutcome::WaitForMore
    } else if rc == NGX_OK as ngx::ffi::ngx_int_t {
        CollectOutcome::ReleaseAndServe
    } else {
        CollectOutcome::Serve
    }
}

/// Event-callback inputs (`ReadCallback` past extraction).
pub struct ReadCallbackInputs {
    /// The event's reference was the last one.
    pub refcount_reached_zero: bool,
    /// The fetch already detached.
    pub detached: bool,
    /// The request context is gone.
    pub ctx_gone: bool,
    /// The event's fetch is disassociated from the request.
    pub disassociated: bool,
    /// A finalized connection on a non-lookup fetch.
    pub connection_error_non_ipro: bool,
    /// Handler result (used unless connection-aborted).
    pub handler_rc: ngx::ffi::ngx_int_t,
    /// An upstream still owns the request.
    pub upstream_active: bool,
}

/// Event-callback action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadCallbackAction {
    /// Skip the event.
    Skip,
    /// Finalize the request, holding an upstream reference while an
    /// upstream owns it, then running posted requests as decided.
    Finalize { rc: ngx::ffi::ngx_int_t, hold_upstream_ref: bool, run_posted: bool },
    /// Upstream-owned `NGX_AGAIN`: run posted requests as decided,
    /// leaving finalization to the upstream.
    UpstreamAgain { run_posted: bool },
}

/// Plans the event callback: stale events skip; a finalized
/// connection aborts with an error; otherwise the handler runs and
/// the request finalizes — except upstream-owned `NGX_AGAIN`,
/// which just runs what was posted.
#[must_use]
pub fn read_callback_plan(inputs: &ReadCallbackInputs) -> ReadCallbackAction {
    use ngx::ffi::{NGX_AGAIN, NGX_ERROR};
    if inputs.refcount_reached_zero
        || inputs.detached
        || inputs.ctx_gone
        || inputs.disassociated
    {
        return ReadCallbackAction::Skip;
    }
    let (rc, run_posted) = if inputs.connection_error_non_ipro {
        (NGX_ERROR as ngx::ffi::ngx_int_t, false)
    } else {
        (inputs.handler_rc, true)
    };
    if inputs.upstream_active && rc == NGX_AGAIN as ngx::ffi::ngx_int_t {
        return ReadCallbackAction::UpstreamAgain { run_posted };
    }
    ReadCallbackAction::Finalize {
        rc,
        hold_upstream_ref: inputs.upstream_active,
        run_posted: run_posted || inputs.upstream_active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recording channel.
    #[derive(Default)]
    struct FakeChannel {
        events: Vec<FetchEvent>,
        fail: bool,
    }

    impl EventChannel for FakeChannel {
        fn write_event(&mut self, event: FetchEvent) -> bool {
            if self.fail {
                return false;
            }
            self.events.push(event);
            true
        }
    }

    fn resource_fetch() -> BaseFetch {
        BaseFetch::new(
            "http://h/a",
            BaseFetchType::PagespeedResource,
            PreserveCachingHeaders::DontPreserve,
            HttpOptions::default_for_tests(),
        )
    }

    #[test]
    fn names_and_bytes() {
        assert_eq!("ps resource", base_fetch_type_name(BaseFetchType::PagespeedResource));
        assert_eq!("html transform", base_fetch_type_name(BaseFetchType::HtmlTransform));
        assert_eq!("admin page", base_fetch_type_name(BaseFetchType::AdminPage));
        assert_eq!("ipro lookup", base_fetch_type_name(BaseFetchType::IproLookup));
        assert_eq!("pagespeed proxy", base_fetch_type_name(BaseFetchType::PagespeedProxy));
        assert_eq!(b'H', event_byte(FetchEvent::HeadersComplete));
        assert_eq!(b'F', event_byte(FetchEvent::Flush));
        assert_eq!(b'D', event_byte(FetchEvent::Done));
        assert_eq!(1 << 20, MAX_BUFFERED_OUTPUT_BYTES);
    }

    #[test]
    fn lifecycle_and_refcounts() {
        let mut fetch = resource_fetch();
        assert_eq!(2, fetch.references());
        assert!(!fetch.detached());
        let mut channel = FakeChannel::default();
        // Small writes buffer silently.
        fetch.handle_write(b"hello", &mut channel);
        assert_eq!(5, fetch.buffered_len());
        assert!(channel.events.is_empty());
        // A window's worth posts a flush.
        fetch.handle_write(&vec![b'x'; MAX_BUFFERED_OUTPUT_BYTES], &mut channel);
        assert_eq!(vec![FetchEvent::Flush], channel.events);
        assert_eq!(3, fetch.references());
        // Delivering the event returns the reference.
        assert!(!fetch.decrement_ref_count());
        assert_eq!(2, fetch.references());
        // Explicit flush posts too.
        fetch.handle_flush(&mut channel);
        assert_eq!(2, channel.events.len());
        // Done drops the fetch-side reference.
        assert!(!fetch.handle_done(&mut channel));
        assert!(fetch.done_called());
        // Detach drops the nginx-side reference; in-flight events
        // keep it alive until delivered.
        assert!(!fetch.detach());
        assert!(fetch.detached());
        assert!(!fetch.decrement_ref_count());
        assert!(fetch.decrement_ref_count());
    }

    #[test]
    #[should_panic(expected = "Done already called!")]
    fn done_twice_panics() {
        let mut fetch = resource_fetch();
        let mut channel = FakeChannel::default();
        fetch.handle_done(&mut channel);
        fetch.handle_done(&mut channel);
    }

    #[test]
    fn suppressed_and_failed_events() {
        // Failed in-place lookups suppress later events (their Done
        // still posts, racing teardown safely).
        let mut fetch = BaseFetch::new(
            "http://h/a",
            BaseFetchType::IproLookup,
            PreserveCachingHeaders::DontPreserve,
            HttpOptions::default_for_tests(),
        );
        let mut channel = FakeChannel::default();
        fetch.handle_headers_complete(false, &mut channel);
        assert_eq!(vec![FetchEvent::HeadersComplete], channel.events);
        let refs = fetch.references();
        fetch.handle_flush(&mut channel);
        assert_eq!(1, channel.events.len());
        assert_eq!(refs, fetch.references());
        // Failed writes return the optimistic reference.
        let mut fetch = resource_fetch();
        let mut failing = FakeChannel { events: Vec::new(), fail: true };
        fetch.handle_flush(&mut failing);
        assert!(failing.events.is_empty());
        assert_eq!(2, fetch.references());
        // A torn-down channel drops events with no change.
        let mut fetch = resource_fetch();
        let mut null = NullChannel;
        fetch.handle_flush(&mut null);
        assert_eq!(2, fetch.references());
    }

    #[test]
    fn copy_policy() {
        let mut fetch = resource_fetch();
        // Nothing buffered, not done: wait.
        assert_eq!(
            CopyOutcome::Again { bytes: Vec::new(), flush: false },
            fetch.copy_buffer_to_nginx(true)
        );
        // Buffered bytes ship with the flush latch, then clear it.
        let mut channel = FakeChannel::default();
        fetch.handle_write(b"hello", &mut channel);
        fetch.handle_flush(&mut channel);
        assert_eq!(
            CopyOutcome::Again { bytes: b"hello".to_vec(), flush: true },
            fetch.copy_buffer_to_nginx(true)
        );
        assert_eq!(0, fetch.buffered_len());
        // Done ships the rest with last_buf, exactly once.
        fetch.handle_write(b"bye", &mut channel);
        fetch.handle_done(&mut channel);
        assert_eq!(
            CopyOutcome::Ok { bytes: b"bye".to_vec(), flush: false },
            fetch.copy_buffer_to_nginx(true)
        );
        // Allocation failure reports without consuming.
        let mut fetch = resource_fetch();
        fetch.handle_write(b"hello", &mut channel);
        assert_eq!(CopyOutcome::Error, fetch.copy_buffer_to_nginx(false));
        assert_eq!(5, fetch.buffered_len());
    }

    #[test]
    #[should_panic(expected = "after the last buffer was sent")]
    fn copy_after_last_panics() {
        let mut fetch = resource_fetch();
        let mut channel = FakeChannel::default();
        fetch.handle_done(&mut channel);
        let _ = fetch.copy_buffer_to_nginx(true);
        let _ = fetch.copy_buffer_to_nginx(true);
    }

    #[test]
    fn gate_matrix() {
        use BaseFetchGate::{Complete, PassThrough, Serve};
        assert_eq!(Complete, base_fetch_gate(true, true));
        assert_eq!(Complete, base_fetch_gate(true, false));
        assert_eq!(PassThrough, base_fetch_gate(false, false));
        assert_eq!(Serve, base_fetch_gate(false, true));
        // Error statuses defer to nginx except on transforms/lookups.
        assert!(handler_defers_error(404, BaseFetchType::PagespeedResource));
        assert!(handler_defers_error(500, BaseFetchType::AdminPage));
        assert!(!handler_defers_error(404, BaseFetchType::HtmlTransform));
        assert!(!handler_defers_error(404, BaseFetchType::IproLookup));
        assert!(!handler_defers_error(200, BaseFetchType::PagespeedResource));
        assert!(!handler_defers_error(0, BaseFetchType::HtmlTransform));
        // Preserve pass keeps Cache-Control, plus the rest when all.
        use PreserveCachingHeaders::{DontPreserve, PreserveAll, PreserveOnlyCacheControl};
        assert!(preserve_header_kept("Cache-Control", DontPreserve));
        assert!(!preserve_header_kept("ETag", DontPreserve));
        assert!(!preserve_header_kept("ETag", PreserveOnlyCacheControl));
        assert!(preserve_header_kept("etag", PreserveAll));
        assert!(preserve_header_kept("Date", PreserveAll));
        assert!(preserve_header_kept("Last-Modified", PreserveAll));
        assert!(preserve_header_kept("Expires", PreserveAll));
        assert!(!preserve_header_kept("Location", PreserveAll));
        assert!(!preserve_header_kept("X-Custom", PreserveAll));
        // Body discard skips lookup misses and transforms.
        assert!(discards_request_body(BaseFetchType::PagespeedResource, true));
        assert!(discards_request_body(BaseFetchType::AdminPage, false));
        assert!(discards_request_body(BaseFetchType::IproLookup, true));
        assert!(!discards_request_body(BaseFetchType::IproLookup, false));
        assert!(!discards_request_body(BaseFetchType::HtmlTransform, true));
    }

    #[test]
    fn outcome_matrices() {
        use ngx::ffi::{NGX_AGAIN, NGX_DONE, NGX_ERROR, NGX_OK};
        // Send-header codes.
        assert_eq!(
            HeaderFilterOutcome::Finalize(NGX_ERROR as isize),
            header_filter_outcome(NGX_ERROR as isize)
        );
        assert_eq!(HeaderFilterOutcome::Finalize(1), header_filter_outcome(1));
        assert_eq!(HeaderFilterOutcome::Done, header_filter_outcome(NGX_DONE as isize));
        assert_eq!(HeaderFilterOutcome::Continue, header_filter_outcome(NGX_OK as isize));
        assert_eq!(HeaderFilterOutcome::Continue, header_filter_outcome(NGX_AGAIN as isize));
        // Collect codes.
        assert_eq!(
            CollectOutcome::InternalError,
            collect_outcome(NGX_ERROR as isize, true)
        );
        assert_eq!(
            CollectOutcome::WaitForMore,
            collect_outcome(NGX_AGAIN as isize, true)
        );
        assert_eq!(
            CollectOutcome::Serve,
            collect_outcome(NGX_AGAIN as isize, false)
        );
        assert_eq!(
            CollectOutcome::ReleaseAndServe,
            collect_outcome(NGX_OK as isize, false)
        );
    }

    #[test]
    #[should_panic(expected = "unexpected header filter code")]
    fn odd_header_code_panics() {
        let _ = header_filter_outcome(-3);
    }

    fn callback_inputs() -> ReadCallbackInputs {
        use ngx::ffi::NGX_OK;
        ReadCallbackInputs {
            refcount_reached_zero: false,
            detached: false,
            ctx_gone: false,
            disassociated: false,
            connection_error_non_ipro: false,
            handler_rc: NGX_OK as isize,
            upstream_active: false,
        }
    }

    #[test]
    fn read_callback_matrix() {
        use ngx::ffi::{NGX_AGAIN, NGX_ERROR, NGX_OK};
        use ReadCallbackAction::{Finalize, Skip, UpstreamAgain};
        // Stale events skip.
        let mut inputs = callback_inputs();
        inputs.refcount_reached_zero = true;
        assert_eq!(Skip, read_callback_plan(&inputs));
        let mut inputs = callback_inputs();
        inputs.detached = true;
        assert_eq!(Skip, read_callback_plan(&inputs));
        let mut inputs = callback_inputs();
        inputs.ctx_gone = true;
        assert_eq!(Skip, read_callback_plan(&inputs));
        let mut inputs = callback_inputs();
        inputs.disassociated = true;
        assert_eq!(Skip, read_callback_plan(&inputs));
        // Plain finalize.
        assert_eq!(
            Finalize { rc: NGX_OK as isize, hold_upstream_ref: false, run_posted: true },
            read_callback_plan(&callback_inputs())
        );
        // Finalized connections abort with an error, skipping posted.
        let mut inputs = callback_inputs();
        inputs.connection_error_non_ipro = true;
        assert_eq!(
            Finalize { rc: NGX_ERROR as isize, hold_upstream_ref: false, run_posted: false },
            read_callback_plan(&inputs)
        );
        // Upstream-owned AGAIN only runs what was posted.
        let mut inputs = callback_inputs();
        inputs.upstream_active = true;
        inputs.handler_rc = NGX_AGAIN as isize;
        assert_eq!(UpstreamAgain { run_posted: true }, read_callback_plan(&inputs));
        // Other upstream results hold a reference and always run posted.
        let mut inputs = callback_inputs();
        inputs.upstream_active = true;
        assert_eq!(
            Finalize { rc: NGX_OK as isize, hold_upstream_ref: true, run_posted: true },
            read_callback_plan(&inputs)
        );
        let mut inputs = callback_inputs();
        inputs.upstream_active = true;
        inputs.connection_error_non_ipro = true;
        assert_eq!(
            Finalize { rc: NGX_ERROR as isize, hold_upstream_ref: true, run_posted: true },
            read_callback_plan(&inputs)
        );
    }
}
