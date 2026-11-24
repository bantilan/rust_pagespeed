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
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

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

/// Event channel (re-exported: the trait lives with the pipe).
pub use crate::pipe::EventChannel;

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
    request: *mut ngx::ffi::ngx_http_request_t,
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
    /// `compress_zstd` armed by the caller (filter enabled and client
    /// negotiates zstd); the encoding decision itself waits for
    /// response headers at collection time.
    zstd_armed: bool,
    /// Live zstd frame once collection commits to encoding (`None`
    /// while undecided or serving identity).
    zstd_stream: Option<ps_http::zstd::ZstdStreamEncoder>,
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
        zstd_armed: bool,
    ) -> Self {
        BaseFetch {
            url: url.to_string(),
            request: std::ptr::null_mut(),
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
            zstd_armed,
            zstd_stream: None,
        }
    }

    /// Whether zstd encoding was armed at creation.
    #[must_use]
    pub fn zstd_armed(&self) -> bool {
        self.zstd_armed
    }

    /// Whether collection committed to zstd encoding.
    #[must_use]
    pub fn zstd_encoding(&self) -> bool {
        self.zstd_stream.is_some()
    }

    /// Fetch URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Attaches the owning request (`request_`; live only).
    pub fn set_request(&mut self, r: *mut ngx::ffi::ngx_http_request_t) {
        self.request = r;
    }

    /// Owning request, null when unset.
    #[must_use]
    pub fn request(&self) -> *mut ngx::ffi::ngx_http_request_t {
        self.request
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

    /// Commits to zstd encoding when armed and the response allows
    /// it: body-carrying status on a non-HEAD request, no encoding
    /// yet, compressible (or absent) content type. Mutates the
    /// response headers and clears the known length (unknown until
    /// the stream finishes); nginx chunks the response. Safe to
    /// re-run: a committed stream short-circuits, and encoded
    /// headers are ineligible by construction.
    fn decide_zstd_encoding(&mut self, head_request: bool) {
        use ps_http::names::header;
        if !self.zstd_armed || self.zstd_stream.is_some() {
            return;
        }
        let eligible = !crate::fetch_body::response_has_no_body(
            self.response_headers.status_code(),
            head_request,
        ) && ps_http::zstd::response_eligible_for_zstd(
            self.response_headers
                .headers()
                .lookup_first(header::CONTENT_TYPE)
                .as_deref(),
            self.response_headers
                .headers()
                .lookup_first(header::CONTENT_ENCODING)
                .as_deref(),
        );
        if !eligible {
            return;
        }
        // Encoder init is ~infallible (valid level); on failure serve
        // identity rather than fail the request.
        if let Ok(stream) = ps_http::zstd::ZstdStreamEncoder::new() {
            self.zstd_stream = Some(stream);
            ps_http::zstd::apply_zstd_response_headers(self.response_headers_mut());
            self.content_length = None;
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
        &mut self,
        r: *mut ngx::ffi::ngx_http_request_t,
        location_field_set: bool,
        set_psol_vary: &mut bool,
    ) -> bool {
        // SAFETY: caller guarantees a live request.
        unsafe {
            self.decide_zstd_encoding(crate::request::request_header_only(r) != 0);
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
    /// Once zstd encoding commits, bytes pass through the frame
    /// encoder here (the tail appends at done); encode failures
    /// surface as `Error` like allocation failures.
    pub fn copy_buffer_to_nginx(&mut self, allocate: bool) -> CopyOutcome {
        assert!(
            !(self.done_called && self.last_buf_sent),
            "CopyBufferToNginx() was called after the last buffer was sent"
        );
        if !self.done_called && self.buffer.is_empty() {
            return CopyOutcome::Again {
                bytes: Vec::new(),
                flush: false,
            };
        }
        if !allocate {
            return CopyOutcome::Error;
        }
        let mut bytes = std::mem::take(&mut self.buffer);
        let flush = self.need_flush;
        self.need_flush = false;
        if let Some(stream) = self.zstd_stream.as_mut() {
            let mut encoded = match stream.write_chunk(&bytes) {
                Ok(out) => out,
                Err(_) => return CopyOutcome::Error,
            };
            if flush {
                match stream.flush_chunk() {
                    Ok(out) => encoded.extend(out),
                    Err(_) => return CopyOutcome::Error,
                }
            }
            bytes = encoded;
        }
        if self.done_called {
            if let Some(stream) = self.zstd_stream.take() {
                match stream.finish() {
                    Ok(tail) => bytes.extend(tail),
                    Err(_) => return CopyOutcome::Error,
                }
            }
            self.last_buf_sent = true;
            return CopyOutcome::Ok { bytes, flush };
        }
        CopyOutcome::Again { bytes, flush }
    }
}

/// Live base-fetch count (`active_base_fetches`).
static ACTIVE_BASE_FETCHES: AtomicI32 = AtomicI32::new(0);

/// Reads the live count (the Terminate drain loop).
#[must_use]
pub fn active_base_fetches() -> i32 {
    ACTIVE_BASE_FETCHES.load(Ordering::SeqCst)
}

/// Reclaims an event sender (the reader half of the Arc pairing).
///
/// # Safety
///
/// `event.sender` must be a live `Arc::into_raw` holder from a
/// post, reclaimed exactly once.
#[must_use]
pub unsafe fn reclaim_event_sender(event: crate::pipe::PipeEvent) -> Arc<SharedBaseFetch> {
    // SAFETY: caller guarantees a live paired holder.
    unsafe { Arc::from_raw(event.sender as *const SharedBaseFetch) }
}

/// Thread-shared base fetch: state under lock, lifetime by `Arc`
/// (one clone per outstanding C++ reference: the two sides plus
/// each in-flight event). `BaseFetch.references` shadows the
/// count for the rule core; the two agree at every quiescent
/// point (pinned below).
pub struct SharedBaseFetch {
    state: Mutex<BaseFetch>,
}

impl SharedBaseFetch {
    /// Creates the two-sided fetch (`new NgxBaseFetch`): one
    /// `Arc` per side, bumping the live count. The owning
    /// request rides along so pipe events dispatch back to it
    /// (null only in tests, whose events never dispatch).
    /// `zstd_armed` pre-negotiates `compress_zstd` (filter on and
    /// client accepts); collection still checks the response.
    #[must_use]
    pub fn new_pair(
        url: &str,
        r: *mut ngx::ffi::ngx_http_request_t,
        fetch_type: BaseFetchType,
        preserve_caching_headers: PreserveCachingHeaders,
        http_options: HttpOptions,
        zstd_armed: bool,
    ) -> (Arc<Self>, Arc<Self>) {
        ACTIVE_BASE_FETCHES.fetch_add(1, Ordering::SeqCst);
        let mut state = BaseFetch::new(
            url,
            fetch_type,
            preserve_caching_headers,
            http_options,
            zstd_armed,
        );
        state.set_request(r);
        let fetch = Arc::new(SharedBaseFetch {
            state: Mutex::new(state),
        });
        (Arc::clone(&fetch), fetch)
    }

    /// Runs a rule under the state lock (`Lock`/`Unlock`).
    pub fn with_state<R>(&self, f: impl FnOnce(&mut BaseFetch) -> R) -> R {
        f(&mut lock(&self.state))
    }

    /// Buffers output (`HandleWrite`), pairing one `Arc` with any
    /// posted event.
    pub fn handle_write(
        self: &Arc<Self>,
        bytes: &[u8],
        slot: &Mutex<Option<crate::pipe::PipeChannel>>,
    ) {
        let mut post = crate::pipe::ArcPost::new(slot, self);
        self.with_state(|fetch| fetch.handle_write(bytes, &mut post));
    }

    /// Latches a flush (`HandleFlush`), pairing one `Arc` with the
    /// posted event.
    pub fn handle_flush(self: &Arc<Self>, slot: &Mutex<Option<crate::pipe::PipeChannel>>) {
        let mut post = crate::pipe::ArcPost::new(slot, self);
        self.with_state(|fetch| fetch.handle_flush(&mut post));
    }

    /// Latches headers-complete (`HandleHeadersComplete`), pairing
    /// one `Arc` with the posted event.
    pub fn handle_headers_complete(
        self: &Arc<Self>,
        status_ok: bool,
        slot: &Mutex<Option<crate::pipe::PipeChannel>>,
    ) {
        let mut post = crate::pipe::ArcPost::new(slot, self);
        self.with_state(|fetch| fetch.handle_headers_complete(status_ok, &mut post));
    }

    /// Latches completion (`HandleDone`), pairing one `Arc` with
    /// the posted event. Reports the shadow delete flag; the
    /// caller drops its pagespeed-side `Arc` after.
    pub fn handle_done(self: &Arc<Self>, slot: &Mutex<Option<crate::pipe::PipeChannel>>) -> bool {
        let mut post = crate::pipe::ArcPost::new(slot, self);
        self.with_state(|fetch| fetch.handle_done(&mut post))
    }

    /// Drops the nginx-side reference (`Detach`): reports the
    /// shadow delete flag; the caller drops its `Arc` after.
    pub fn detach(&self) -> bool {
        self.with_state(|fetch| fetch.detach())
    }

    /// Releases one in-flight event (`DecrementRefCount`): reports
    /// the shadow delete flag; the caller drops the event's
    /// reclaimed `Arc` after.
    pub fn decrement_ref_count(&self) -> bool {
        self.with_state(|fetch| fetch.decrement_ref_count())
    }

    /// Shadow reference count (equals `Arc::strong_count` at rest).
    #[must_use]
    pub fn references(&self) -> i32 {
        self.with_state(|fetch| fetch.references())
    }
}

impl Drop for SharedBaseFetch {
    /// Releases the live count (the C++ destructor's decrement).
    fn drop(&mut self) {
        ACTIVE_BASE_FETCHES.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Locks fetch state (poison recovers: a panicked rule must not
/// wedge the fetch forever).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
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
        && !matches!(
            fetch_type,
            BaseFetchType::HtmlTransform | BaseFetchType::IproLookup
        )
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
        assert_eq!(
            rc, NGX_DONE as ngx::ffi::ngx_int_t,
            "unexpected header filter code"
        );
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
    Finalize {
        rc: ngx::ffi::ngx_int_t,
        hold_upstream_ref: bool,
        run_posted: bool,
    },
    /// Upstream-owned `NGX_AGAIN`: run posted requests as decided,
    /// leaving finalization to the upstream.
    UpstreamAgain { run_posted: bool },
}

/// Releases the ctx fetch (`ps_release_base_fetch`): detaches
/// and clears (dropping nginx's reference).
///
/// # Safety
///
/// `ctx` must point at a live request context.
pub unsafe fn release_base_fetch(ctx: *mut crate::request::RequestCtx) {
    // SAFETY: caller guarantees a live context.
    unsafe {
        if let Some(fetch) = (*ctx).base_fetch.take() {
            fetch.detach();
            drop(fetch);
        }
    }
}

/// Serves one event's output (`ps_base_fetch_handler`): headers on
/// first call, then body chains.
///
/// # Safety
///
/// `r` must point at a live request with an attached context
/// holding a fetch.
pub unsafe fn base_fetch_handler(r: *mut ngx::ffi::ngx_http_request_t) -> ngx::ffi::ngx_int_t {
    use ngx::ffi::{
        ngx_http_clean_header, ngx_http_filter_finalize_request, ngx_int_t, NGX_AGAIN, NGX_DONE,
        NGX_ERROR, NGX_HTTP_INTERNAL_SERVER_ERROR, NGX_OK,
    };
    // SAFETY: caller guarantees a live request with context.
    unsafe {
        let ctx = crate::request::get_request_ctx(r);
        {
            let uri = crate::headers::ngx_str_text(&raw const (*r).uri);
            let log = (*(*r).connection).log;
            ngx::ngx_log_error!(
                ngx::ffi::NGX_LOG_DEBUG_HTTP,
                log,
                "ps fetch handler: {}",
                uri
            );
        }
        if ngx::ffi::ngx_terminate != 0 || ngx::ffi::ngx_exiting != 0 {
            crate::request::set_buffered(r, false);
            release_base_fetch(ctx);
            return NGX_ERROR as ngx_int_t;
        }
        if crate::request::request_header_sent(r) == 0 {
            let (status_code, fetch_type) = (*ctx)
                .base_fetch
                .as_ref()
                .expect("handler needs fetch")
                .with_state(|fetch| (fetch.response_headers().status_code(), fetch.fetch_type()));
            let status_ok = status_code != 0 && status_code < 400;
            // Error statuses on content-generator responses defer
            // to nginx's default error handling.
            if !status_ok
                && !matches!(
                    fetch_type,
                    BaseFetchType::HtmlTransform | BaseFetchType::IproLookup
                )
            {
                release_base_fetch(ctx);
                return ngx_http_filter_finalize_request(
                    r,
                    std::ptr::null_mut(),
                    status_code as ngx_int_t,
                );
            }
            if (*ctx).preserve_caching_headers
                != crate::caching::PreserveCachingHeaders::DontPreserve
            {
                let preserve_all = (*ctx).preserve_caching_headers
                    == crate::caching::PreserveCachingHeaders::PreserveAll;
                crate::headers::for_each_header(&mut (*r).headers_out.headers, |header| {
                    let key = std::slice::from_raw_parts((*header).key.data, (*header).key.len);
                    let is = |literal: &[u8]| key.eq_ignore_ascii_case(literal);
                    if !(is(b"Cache-Control")
                        || (preserve_all
                            && (is(b"Etag")
                                || is(b"Date")
                                || is(b"Last-Modified")
                                || is(b"Expires"))))
                    {
                        (*header).hash = 0;
                        if is(b"Location") {
                            (*r).headers_out.location = std::ptr::null_mut();
                        }
                    }
                    true
                });
            } else {
                ngx_http_clean_header(r);
            }
            // The module answers the request itself, so the body
            // goes unless this is an in-place miss or an HTML
            // transform (whose owner keeps the body).
            let ipro_lookup_miss = fetch_type == BaseFetchType::IproLookup && !status_ok;
            if !ipro_lookup_miss && fetch_type != BaseFetchType::HtmlTransform {
                crate::module::discard_request_body(r);
            }
            let location_field_set = (*ctx).location_field_set;
            let mut set_psol_vary = false;
            let rc = (*ctx)
                .base_fetch
                .as_ref()
                .expect("handler needs fetch")
                .with_state(|fetch| {
                    fetch.collect_headers(r, location_field_set, &mut set_psol_vary)
                });
            if !rc {
                release_base_fetch(ctx);
                return NGX_HTTP_INTERNAL_SERVER_ERROR as ngx_int_t;
            }
            (*ctx).psol_vary_accept_only = set_psol_vary;
            let rc = crate::filters::base_fetch::next_header_filter(r);
            if rc == NGX_ERROR as ngx_int_t || rc > NGX_OK as ngx_int_t {
                release_base_fetch(ctx);
                return ngx_http_filter_finalize_request(r, std::ptr::null_mut(), rc);
            }
            if rc < NGX_OK as ngx_int_t && rc != NGX_AGAIN as ngx_int_t {
                assert!(
                    rc == NGX_DONE as ngx_int_t,
                    "unexpected header filter code: {rc}"
                );
                return NGX_DONE as ngx_int_t;
            }
            crate::request::set_buffered(r, true);
        }
        let (rc, chain) = (*ctx)
            .base_fetch
            .as_ref()
            .expect("handler needs fetch")
            .with_state(|fetch| fetch.collect_accumulated_writes((*r).pool));
        if rc == NGX_ERROR as ngx_int_t {
            crate::request::set_buffered(r, false);
            release_base_fetch(ctx);
            return NGX_HTTP_INTERNAL_SERVER_ERROR as ngx_int_t;
        }
        if rc == NGX_AGAIN as ngx_int_t && chain.is_null() {
            return NGX_AGAIN as ngx_int_t;
        }
        if rc == NGX_OK as ngx_int_t {
            crate::request::set_buffered(r, false);
            release_base_fetch(ctx);
        }
        crate::filters::base_fetch::body_filter(r, chain)
    }
}

/// Plans the event callback: stale events skip; a finalized
/// connection aborts with an error; otherwise the handler runs and
/// the request finalizes — except upstream-owned `NGX_AGAIN`,
/// which just runs what was posted.
#[must_use]
pub fn read_callback_plan(inputs: &ReadCallbackInputs) -> ReadCallbackAction {
    use ngx::ffi::{NGX_AGAIN, NGX_ERROR};
    if inputs.refcount_reached_zero || inputs.detached || inputs.ctx_gone || inputs.disassociated {
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
            false,
        )
    }

    #[test]
    fn names_and_bytes() {
        assert_eq!(
            "ps resource",
            base_fetch_type_name(BaseFetchType::PagespeedResource)
        );
        assert_eq!(
            "html transform",
            base_fetch_type_name(BaseFetchType::HtmlTransform)
        );
        assert_eq!("admin page", base_fetch_type_name(BaseFetchType::AdminPage));
        assert_eq!(
            "ipro lookup",
            base_fetch_type_name(BaseFetchType::IproLookup)
        );
        assert_eq!(
            "pagespeed proxy",
            base_fetch_type_name(BaseFetchType::PagespeedProxy)
        );
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
            false,
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
        let mut failing = FakeChannel {
            events: Vec::new(),
            fail: true,
        };
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
            CopyOutcome::Again {
                bytes: Vec::new(),
                flush: false
            },
            fetch.copy_buffer_to_nginx(true)
        );
        // Buffered bytes ship with the flush latch, then clear it.
        let mut channel = FakeChannel::default();
        fetch.handle_write(b"hello", &mut channel);
        fetch.handle_flush(&mut channel);
        assert_eq!(
            CopyOutcome::Again {
                bytes: b"hello".to_vec(),
                flush: true
            },
            fetch.copy_buffer_to_nginx(true)
        );
        assert_eq!(0, fetch.buffered_len());
        // Done ships the rest with last_buf, exactly once.
        fetch.handle_write(b"bye", &mut channel);
        fetch.handle_done(&mut channel);
        assert_eq!(
            CopyOutcome::Ok {
                bytes: b"bye".to_vec(),
                flush: false
            },
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

    fn armed_html_fetch() -> BaseFetch {
        BaseFetch::new(
            "http://h/p",
            BaseFetchType::HtmlTransform,
            PreserveCachingHeaders::DontPreserve,
            HttpOptions::default_for_tests(),
            true,
        )
    }

    fn serve_html(fetch: &mut BaseFetch, status: u16, content_type: &str) {
        fetch.response_headers_mut().set_status_and_reason(status);
        fetch
            .response_headers_mut()
            .headers_mut()
            .add("Content-Type", content_type);
    }

    #[test]
    fn zstd_decision_commits_eligible_html() {
        let mut fetch = armed_html_fetch();
        serve_html(&mut fetch, 200, "text/html; charset=utf-8");
        fetch.set_content_length(15398);
        assert!(!fetch.zstd_encoding());
        fetch.decide_zstd_encoding(false);
        assert!(fetch.zstd_encoding());
        let headers = fetch.response_headers();
        assert_eq!(
            headers.headers().lookup1("Content-Encoding"),
            Some("zstd".to_string())
        );
        assert!(headers
            .headers()
            .lookup("Vary")
            .unwrap_or_default()
            .iter()
            .any(|v| v == "Accept-Encoding"));
        assert!(fetch.content_length().is_none());
        // Re-running is a no-op (no duplicate encoding line).
        fetch.decide_zstd_encoding(false);
        assert_eq!(
            fetch
                .response_headers()
                .headers()
                .lookup("Content-Encoding"),
            Some(vec!["zstd".to_string()])
        );
    }

    #[test]
    fn zstd_decision_declines() {
        // Not armed.
        let mut fetch = resource_fetch();
        serve_html(&mut fetch, 200, "text/html");
        fetch.decide_zstd_encoding(false);
        assert!(!fetch.zstd_encoding());
        // No body statuses.
        for status in [204, 304] {
            let mut fetch = armed_html_fetch();
            serve_html(&mut fetch, status, "text/html");
            fetch.decide_zstd_encoding(false);
            assert!(!fetch.zstd_encoding(), "status {status}");
        }
        // HEAD requests keep identity (nginx drops the body).
        let mut fetch = armed_html_fetch();
        serve_html(&mut fetch, 200, "text/html");
        fetch.decide_zstd_encoding(true);
        assert!(!fetch.zstd_encoding());
        // Already encoded: never stack.
        let mut fetch = armed_html_fetch();
        serve_html(&mut fetch, 200, "text/html");
        fetch
            .response_headers_mut()
            .headers_mut()
            .add("Content-Encoding", "gzip");
        fetch.decide_zstd_encoding(false);
        assert!(!fetch.zstd_encoding());
        // Incompressible type.
        let mut fetch = armed_html_fetch();
        serve_html(&mut fetch, 200, "image/jpeg");
        fetch.decide_zstd_encoding(false);
        assert!(!fetch.zstd_encoding());
        // Declined means untouched headers.
        assert!(!fetch.response_headers().headers().has("Content-Encoding"));
    }

    #[test]
    fn zstd_collection_round_trips() {
        let mut fetch = armed_html_fetch();
        serve_html(&mut fetch, 200, "text/html");
        fetch.decide_zstd_encoding(false);
        assert!(fetch.zstd_encoding());
        let mut channel = FakeChannel::default();
        let first = b"<html><head><title>t</title></head><body>".repeat(20);
        let second = b"<p>hello compression</p></body></html>".repeat(20);
        fetch.handle_write(&first, &mut channel);
        fetch.handle_flush(&mut channel);
        let mut shipped = Vec::new();
        match fetch.copy_buffer_to_nginx(true) {
            CopyOutcome::Again { bytes, flush } => {
                assert!(flush);
                shipped.extend(bytes);
            }
            other => panic!("expected Again, got {other:?}"),
        }
        fetch.handle_write(&second, &mut channel);
        fetch.handle_done(&mut channel);
        match fetch.copy_buffer_to_nginx(true) {
            CopyOutcome::Ok { bytes, .. } => shipped.extend(bytes),
            other => panic!("expected Ok, got {other:?}"),
        }
        assert!(!shipped.is_empty());
        let joined = [first, second].concat();
        assert_ne!(shipped, joined);
        let decoded = zstd::bulk::decompress(&shipped, joined.len()).expect("valid zstd frame");
        assert_eq!(decoded, joined);
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
        assert!(discards_request_body(
            BaseFetchType::PagespeedResource,
            true
        ));
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
        assert_eq!(
            HeaderFilterOutcome::Done,
            header_filter_outcome(NGX_DONE as isize)
        );
        assert_eq!(
            HeaderFilterOutcome::Continue,
            header_filter_outcome(NGX_OK as isize)
        );
        assert_eq!(
            HeaderFilterOutcome::Continue,
            header_filter_outcome(NGX_AGAIN as isize)
        );
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
            Finalize {
                rc: NGX_OK as isize,
                hold_upstream_ref: false,
                run_posted: true
            },
            read_callback_plan(&callback_inputs())
        );
        // Finalized connections abort with an error, skipping posted.
        let mut inputs = callback_inputs();
        inputs.connection_error_non_ipro = true;
        assert_eq!(
            Finalize {
                rc: NGX_ERROR as isize,
                hold_upstream_ref: false,
                run_posted: false
            },
            read_callback_plan(&inputs)
        );
        // Upstream-owned AGAIN only runs what was posted.
        let mut inputs = callback_inputs();
        inputs.upstream_active = true;
        inputs.handler_rc = NGX_AGAIN as isize;
        assert_eq!(
            UpstreamAgain { run_posted: true },
            read_callback_plan(&inputs)
        );
        // Other upstream results hold a reference and always run posted.
        let mut inputs = callback_inputs();
        inputs.upstream_active = true;
        assert_eq!(
            Finalize {
                rc: NGX_OK as isize,
                hold_upstream_ref: true,
                run_posted: true
            },
            read_callback_plan(&inputs)
        );
        let mut inputs = callback_inputs();
        inputs.upstream_active = true;
        inputs.connection_error_non_ipro = true;
        assert_eq!(
            Finalize {
                rc: NGX_ERROR as isize,
                hold_upstream_ref: true,
                run_posted: true
            },
            read_callback_plan(&inputs)
        );
    }

    #[test]
    fn shared_arc_protocol_matches_shadow() {
        use crate::pipe::{self, PipeChannel};
        // Local slot + real pipe (never touches the global).
        let slot: Mutex<Option<PipeChannel>> = Mutex::new(None);
        let before = active_base_fetches();
        let (ps_side, ngx_side) = SharedBaseFetch::new_pair(
            "http://h/a",
            std::ptr::null_mut(),
            BaseFetchType::PagespeedResource,
            PreserveCachingHeaders::DontPreserve,
            HttpOptions::default_for_tests(),
            false,
        );
        assert_eq!(before + 1, active_base_fetches());
        assert_eq!(2, ps_side.references());
        assert_eq!(2, Arc::strong_count(&ps_side));
        // The owning request rides along for event dispatch.
        {
            let probe = 0x1234 as *mut ngx::ffi::ngx_http_request_t;
            let (owned, owned_ngx) = SharedBaseFetch::new_pair(
                "http://h/a",
                probe,
                BaseFetchType::PagespeedResource,
                PreserveCachingHeaders::DontPreserve,
                HttpOptions::default_for_tests(),
                false,
            );
            assert!(owned.with_state(|fetch| fetch.request()) == probe);
            drop(owned);
            drop(owned_ngx);
        }
        // Post through a live channel: shadow and strong move together.
        let fds = pipe::open_pipe().expect("pipe");
        assert!(pipe::make_nonblocking(fds.read_fd));
        assert!(pipe::make_nonblocking(fds.write_fd));
        pipe::install_channel_into(&slot, PipeChannel::new(fds));
        ps_side.handle_flush(&slot);
        assert_eq!(3, ps_side.references());
        assert_eq!(3, Arc::strong_count(&ps_side));
        // Read side: reclaim the event Arc, release, drop.
        let channel = pipe::take_channel_from(&slot).expect("installed");
        let mut reclaimed = None;
        let outcome = channel.drain(&mut |event| {
            assert_eq!(event_byte(FetchEvent::Flush), event.event_type);
            // SAFETY: paired by the post above.
            reclaimed = Some(unsafe { Arc::from_raw(event.sender as *const SharedBaseFetch) });
        });
        assert!(matches!(outcome, pipe::DrainOutcome::Dispatched(_)));
        let event_arc = reclaimed.expect("reclaimed");
        assert_eq!(3, Arc::strong_count(&ps_side));
        assert!(!event_arc.decrement_ref_count());
        assert_eq!(2, event_arc.references());
        drop(event_arc);
        assert_eq!(2, Arc::strong_count(&ps_side));
        // Done posts nothing (channel gone) and drops a shadow;
        // the caller drops its pagespeed-side Arc after.
        assert!(!ps_side.handle_done(&slot));
        drop(ps_side);
        assert_eq!(1, ngx_side.references());
        assert_eq!(1, Arc::strong_count(&ngx_side));
        // Detach zeroes the shadow; dropping nginx's Arc releases
        // the live count.
        assert!(ngx_side.detach());
        assert_eq!(0, ngx_side.references());
        drop(ngx_side);
        assert_eq!(before, active_base_fetches());
        channel.shutdown_write();
        pipe::close_fd(fds.read_fd);
    }
}
