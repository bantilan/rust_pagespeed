//! Output filter chain (the six filter slots).
//!
//! Ports the filter registration half of `ngx_pagespeed.cc`:
//! the four init functions, their order, and the six filter
//! slots they install. Each init grabs the current filter top
//! into its namespace's next-pointer and installs itself, so
//! execution runs reverse to installation:
//!
//! ```text
//! init order:      in_place → fix_headers → base_fetch → html_rewrite
//! execution order: html_rewrite(header+body) → base_fetch(body)
//!                  → fix_headers(header) → in_place(header+body)
//! ```
//!
//! Filter BODIES: all six slots carry their real dispatch
//! (Slices 3c–5), declining where their slice has not landed
//! (fetch/drive bodies and statistics in Slice 6) — exactly the
//! decline path each filter takes when pagespeed is off for the
//! request.

use ngx::ffi::{
    ngx_chain_t, ngx_http_output_body_filter_pt, ngx_http_output_header_filter_pt,
    ngx_http_request_t, ngx_int_t,
};

/// Filter slots in wire execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterSlot {
    /// `ps_html_rewrite_header_filter` (Slice 3).
    HtmlRewriteHeader,
    /// `ps_html_rewrite_body_filter` (Slice 3).
    HtmlRewriteBody,
    /// `ps_base_fetch_filter` (Slice 4).
    BaseFetchBody,
    /// `ps_html_rewrite_fix_headers_filter` (Slice 3).
    FixHeadersHeader,
    /// `ps_in_place_check_header_filter` (in-place serving; Slice 5).
    InPlaceHeader,
    /// `ps_in_place_body_filter` (Slice 5).
    InPlaceBody,
}

/// Init-function order (`ps_init` comments: order is important).
pub const INIT_ORDER: [&str; 4] = ["in_place", "fix_headers", "base_fetch", "html_rewrite"];

/// Wire execution order (reverse of installation).
pub const EXECUTION_ORDER: [FilterSlot; 6] = [
    FilterSlot::HtmlRewriteHeader,
    FilterSlot::HtmlRewriteBody,
    FilterSlot::BaseFetchBody,
    FilterSlot::FixHeadersHeader,
    FilterSlot::InPlaceHeader,
    FilterSlot::InPlaceBody,
];

/// In-place serving filters (`namespace in_place`).
pub mod in_place {
    use super::*;
    use crate::recording::{HeadersKind, InPlaceRecorder, NOT_IN_CACHE_STATUS};

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;
    static mut NEXT_BODY: ngx_http_output_body_filter_pt = None;

    /// Header-filter plan.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum InPlaceHeaderAction {
        /// Pass through untouched.
        Next,
        /// Feed preliminary headers to the recorder, then pass on.
        RecordPreliminary,
        /// Cache hit: clear the lookup flag (+ stats/log in the FFI),
        /// then pass on.
        ServeFromCache,
        /// Cache miss on a body-bearing request (the sync
        /// bridge arms at the miss site instead): flag
        /// in-memory, decline.
        StartRecording,
        /// Anything else (finalized connection, unrecordable
        /// status): decline.
        Decline,
    }

    /// Plans the header filter: recording requests feed headers;
    /// lookups serve hits, record misses with bodies, and decline
    /// everything else.
    #[must_use]
    pub const fn in_place_header_plan(
        has_ctx: bool,
        recording: bool,
        in_place: bool,
        connection_error: bool,
        status_code: i32,
        header_only: bool,
    ) -> InPlaceHeaderAction {
        if !has_ctx {
            return InPlaceHeaderAction::Next;
        }
        if recording {
            assert!(!in_place, "recording implies the lookup finished");
            return InPlaceHeaderAction::RecordPreliminary;
        }
        if !in_place {
            return InPlaceHeaderAction::Next;
        }
        if connection_error {
            return InPlaceHeaderAction::Decline;
        }
        let status_ok = status_code != 0 && status_code < 400;
        if status_ok {
            return InPlaceHeaderAction::ServeFromCache;
        }
        if status_code == NOT_IN_CACHE_STATUS && !header_only {
            return InPlaceHeaderAction::StartRecording;
        }
        InPlaceHeaderAction::Decline
    }

    /// Header filter (`ps_in_place_check_header_filter`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized (postconfiguration ran).
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        use crate::module::decline_request;
        // SAFETY: caller guarantees a live request.
        unsafe {
            let pass_to_next = move || {
                let next = NEXT_HEADER.expect("in-place header chain not initialized");
                // SAFETY: `r` is live per the caller contract.
                next(r)
            };
            let ctx = crate::request::get_request_ctx(r);
            let (recording, in_place) = if ctx.is_null() {
                (false, false)
            } else {
                ((*ctx).recorder.is_some(), (*ctx).in_place)
            };
            match in_place_header_plan(
                !ctx.is_null(),
                recording,
                in_place,
                (*(*r).connection).error() != 0,
                (*r).headers_out.status as i32,
                crate::request::request_header_only(r) != 0,
            ) {
                InPlaceHeaderAction::Next => pass_to_next(),
                InPlaceHeaderAction::RecordPreliminary => {
                    let ctx = &mut *ctx;
                    let recorder = ctx.recorder.as_mut().expect("planned recording");
                    let mut headers = ps_http::response::ResponseHeaders::new(
                        ps_http::response::HttpOptions::default(),
                    );
                    copy_response_headers_from_request(r, &mut headers);
                    recorder.consider_response_headers(HeadersKind::Preliminary, &mut headers);
                    pass_to_next()
                }
                InPlaceHeaderAction::ServeFromCache => {
                    (*ctx).in_place = false;
                    // Slice 6 bumps `ipro_served` and logs the serve.
                    pass_to_next()
                }
                InPlaceHeaderAction::StartRecording => {
                    // Unreachable in the sync bridge: the content
                    // phase arms the recorder at the miss site
                    // (the lookup never pends), so this keeps the
                    // plan shape and declines without creating.
                    crate::request::set_request_filter_need_in_memory(r);
                    decline_request(r, None)
                }
                InPlaceHeaderAction::Decline => decline_request(r, None),
            }
        }
    }

    /// Copies live response headers for the recorder.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request with a connected socket.
    unsafe fn copy_response_headers_from_request(
        r: *mut ngx_http_request_t,
        headers: &mut ps_http::response::ResponseHeaders,
    ) {
        use crate::headers::{copy_response_headers, for_each_header, ngx_str_text};
        // SAFETY: caller guarantees a live request.
        unsafe {
            let mut pairs: Vec<(String, String, bool)> = Vec::new();
            for_each_header(&mut (*r).headers_out.headers, |header| {
                pairs.push((
                    ngx_str_text(&raw const (*header).key),
                    ngx_str_text(&raw const (*header).value),
                    (*header).hash != 0,
                ));
                true
            });
            let refs: Vec<crate::headers::RawHeader<'_>> = pairs
                .iter()
                .map(|(name, value, hashed)| (name.as_str(), value.as_str(), *hashed))
                .collect();
            let content_type = ngx_str_text(&raw const (*r).headers_out.content_type);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            copy_response_headers(
                (*r).http_version,
                (*r).headers_out.status,
                &content_type,
                &refs,
                now_ms,
                headers,
            );
        }
    }

    /// Drives one body chain through the recorder: non-empty
    /// buffers write, flush flags flush, and the first `last_buf`
    /// (or failure) completes the recording against the final
    /// headers and drops the recorder. Returns whether the
    /// recorder finished (the FFI clears the slot then).
    pub fn drive_recording(
        buffers: &[(Vec<u8>, bool, bool)],
        recorder: &mut InPlaceRecorder,
        final_headers: &mut ps_http::response::ResponseHeaders,
    ) -> bool {
        for (bytes, flush, last_buf) in buffers {
            if !bytes.is_empty() {
                recorder.write(bytes);
            }
            if *flush {
                recorder.flush();
            }
            if *last_buf || recorder.failed() {
                recorder.done_and_set_headers(final_headers, *last_buf);
                return true;
            }
        }
        false
    }

    /// Body filter (`ps_in_place_body_filter`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `in` is the body chain
    /// (possibly null at flush boundaries); the chain must be
    /// initialized.
    pub unsafe extern "C" fn body_filter(
        r: *mut ngx_http_request_t,
        chain: *mut ngx_chain_t,
    ) -> ngx_int_t {
        // SAFETY: caller guarantees a live request.
        unsafe {
            let pass_to_next = move |chain: *mut ngx_chain_t| {
                let next = NEXT_BODY.expect("in-place body chain not initialized");
                // SAFETY: `r`/`chain` per the caller contract.
                next(r, chain)
            };
            let ctx = crate::request::get_request_ctx(r);
            if ctx.is_null() || (*ctx).recorder.is_none() {
                return pass_to_next(chain);
            }
            let ctx = &mut *ctx;
            let recorder = ctx.recorder.as_mut().expect("checked above");
            let mut buffers: Vec<(Vec<u8>, bool, bool)> = Vec::new();
            let mut link = chain;
            while !link.is_null() {
                let buf = (*link).buf;
                let bytes = std::slice::from_raw_parts(
                    (*buf).pos,
                    (*buf).last.offset_from((*buf).pos) as usize,
                );
                if !bytes.is_empty() {
                    assert!(
                        (*buf).temporary() != 0 || (*buf).memory() != 0 || (*buf).mmap() != 0,
                        "in-place body buffers stay in memory"
                    );
                }
                buffers.push((bytes.to_vec(), (*buf).flush() != 0, (*buf).last_buf() != 0));
                link = (*link).next;
            }
            let mut final_headers =
                ps_http::response::ResponseHeaders::new(ps_http::response::HttpOptions::default());
            copy_response_headers_from_request(r, &mut final_headers);
            if drive_recording(&buffers, recorder, &mut final_headers) {
                ctx.recorder = None;
            }
            pass_to_next(chain)
        }
    }

    /// Installs the in-place filters (`ps_in_place_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration on the configuration
    /// thread: grabs both filter tops.
    pub unsafe fn init() {
        use ngx::ffi::{ngx_http_top_body_filter, ngx_http_top_header_filter};
        // SAFETY: postconfiguration context; top pointers are
        // live nginx globals.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
            NEXT_BODY = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::recording::{HeadersKind, RecordingBackend};
        use ps_http::response::{HttpOptions, RequestProperties, ResponseHeaders};

        #[test]
        fn header_plan_matrix() {
            use InPlaceHeaderAction::{
                Decline, Next, RecordPreliminary, ServeFromCache, StartRecording,
            };
            assert_eq!(
                Next,
                in_place_header_plan(false, false, false, false, 0, false)
            );
            assert_eq!(
                Next,
                in_place_header_plan(true, false, false, false, 200, false)
            );
            assert_eq!(
                RecordPreliminary,
                in_place_header_plan(true, true, false, false, 200, false)
            );
            assert_eq!(
                Decline,
                in_place_header_plan(true, false, true, true, 200, false)
            );
            assert_eq!(
                ServeFromCache,
                in_place_header_plan(true, false, true, false, 200, false)
            );
            assert_eq!(
                ServeFromCache,
                in_place_header_plan(true, false, true, false, 200, true)
            );
            assert_eq!(
                StartRecording,
                in_place_header_plan(true, false, true, false, 501, false)
            );
            assert_eq!(
                Decline,
                in_place_header_plan(true, false, true, false, 501, true)
            );
            assert_eq!(
                Decline,
                in_place_header_plan(true, false, true, false, 404, false)
            );
            assert_eq!(
                Decline,
                in_place_header_plan(true, false, true, false, 0, false)
            );
        }

        #[test]
        #[should_panic(expected = "recording implies the lookup finished")]
        fn recording_with_lookup_panics() {
            let _ = in_place_header_plan(true, true, true, false, 200, false);
        }

        /// Null backend: recording decisions without cache effects.
        struct NullBackend;

        impl RecordingBackend for NullBackend {
            fn remember_failure(
                &mut self,
                _url: &str,
                _fragment: &str,
                _kind: crate::recording::RecordingFailure,
            ) {
            }
            fn put(
                &mut self,
                _url: &str,
                _fragment: &str,
                _properties: RequestProperties,
                _headers: &ResponseHeaders,
                _body: &[u8],
            ) {
            }
            fn note_stat(&mut self, _stat: crate::recording::RecorderStat) {}
            fn log_info(&mut self, _text: String) {}
        }

        #[test]
        fn drive_writes_flushes_and_completes() {
            use ps_http::names::header;
            let mut recorder = InPlaceRecorder::new(
                "http://h/a.css",
                "",
                RequestProperties {
                    has_cookie: false,
                    has_cookie2: false,
                    has_authorization: false,
                },
                false,
                1024,
                0,
                -1,
                Some(Box::new(NullBackend)),
            );
            let mut prelim = ResponseHeaders::new(HttpOptions::default_for_tests());
            prelim.set_status_code(200);
            prelim.headers_mut().add(header::CONTENT_TYPE, "text/css");
            prelim
                .headers_mut()
                .add(header::CACHE_CONTROL, "max-age=60");
            prelim
                .headers_mut()
                .add(header::DATE, "Thu, 01 Jan 1970 00:00:00 GMT");
            recorder.consider_response_headers(HeadersKind::Preliminary, &mut prelim);
            let mut final_headers = prelim.clone();
            // A flush-only buffer records nothing but completes nothing.
            assert!(!drive_recording(
                &[(Vec::new(), true, false)],
                &mut recorder,
                &mut final_headers
            ));
            // Body bytes write; the last buffer completes and drops.
            assert!(drive_recording(
                &[
                    (b"data".to_vec(), false, false),
                    (b"!".to_vec(), false, true)
                ],
                &mut recorder,
                &mut final_headers
            ));
            assert_eq!(b"data!".to_vec(), recorder.contents().to_vec());
        }
    }
}

/// Header-fixup filter (`namespace fix_headers`).
pub mod fix_headers {
    use super::*;
    use crate::caching::PreserveCachingHeaders;
    use crate::headers::{for_each_header, ngx_str_text};

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;

    /// Fix-up plan.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FixHeadersAction {
        /// Pass through untouched.
        Next,
        /// Strip the override token, then pass on.
        StripTokenThenNext,
        /// Full fix-up, then pass on.
        FixThenNext {
            /// Rewrite `Cache-Control` to the disabled value.
            rewrite_cache_control: bool,
        },
    }

    /// Plans the fix-up: every main response loses the token; live
    /// HTML rewrites additionally lose etag/last-modified/expires
    /// (plus the `Cache-Control` rewrite unless headers are
    /// preserved wholesale).
    #[must_use]
    pub const fn fix_headers_plan(
        is_main: bool,
        html_rewrite: bool,
        has_ctx: bool,
        preserve: PreserveCachingHeaders,
    ) -> FixHeadersAction {
        if !is_main {
            return FixHeadersAction::Next;
        }
        if !has_ctx || !html_rewrite || matches!(preserve, PreserveCachingHeaders::PreserveAll) {
            return FixHeadersAction::StripTokenThenNext;
        }
        FixHeadersAction::FixThenNext {
            rewrite_cache_control: matches!(preserve, PreserveCachingHeaders::DontPreserve),
        }
    }

    /// Header filter (`ps_html_rewrite_fix_headers_filter`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized.
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        use crate::caching::{disabled_cache_control, keep_cache_control_limits};
        use crate::headers::set_cache_control;
        use ngx::ffi::NGX_ERROR;
        // SAFETY: caller guarantees a live request.
        unsafe {
            let pass_to_next = move || {
                let next = NEXT_HEADER.expect("fix-headers chain not initialized");
                // SAFETY: `r` is live per the caller contract.
                next(r)
            };
            let ctx = crate::request::get_request_ctx(r);
            let (html_rewrite, preserve, limits) = if ctx.is_null() {
                (
                    false,
                    PreserveCachingHeaders::DontPreserve,
                    crate::caching::CacheControlLimits::default(),
                )
            } else {
                (
                    (*ctx).html_rewrite,
                    (*ctx).preserve_caching_headers,
                    (*ctx).html_cache_control_limits,
                )
            };
            match fix_headers_plan(r == (*r).main, html_rewrite, !ctx.is_null(), preserve) {
                FixHeadersAction::Next => pass_to_next(),
                FixHeadersAction::StripTokenThenNext => {
                    strip_override_token(r);
                    pass_to_next()
                }
                FixHeadersAction::FixThenNext {
                    rewrite_cache_control,
                } => {
                    strip_override_token(r);
                    if rewrite_cache_control {
                        // Collect the origin's Cache-Control lines
                        // (exact-case key match, like `Lookup`).
                        let mut values: Vec<String> = Vec::new();
                        for_each_header(&mut (*r).headers_out.headers, |header| {
                            let name = ngx_str_text(&raw const (*header).key);
                            if (*header).hash != 0 && name == "Cache-Control" {
                                values.push(ngx_str_text(&raw const (*header).value));
                            }
                            true
                        });
                        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
                        let merged =
                            keep_cache_control_limits(&disabled_cache_control(&refs), &limits);
                        let pool = (*r).pool;
                        let rendered = crate::directives::pool_cstring(pool, &merged);
                        if rendered.is_null() {
                            return NGX_ERROR as ngx_int_t;
                        }
                        if !set_cache_control(r, rendered) {
                            return NGX_ERROR as ngx_int_t;
                        }
                    }
                    clear_etag(r);
                    clear_last_modified(r);
                    if !(*r).headers_out.expires.is_null() {
                        (*(*r).headers_out.expires).hash = 0;
                        (*r).headers_out.expires = std::ptr::null_mut();
                    }
                    pass_to_next()
                }
            }
        }
    }

    /// Strips the override token from every response
    /// (`ps_remove_option_override_token`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request.
    unsafe fn strip_override_token(r: *mut ngx_http_request_t) {
        use crate::headers::strips_override_token;
        // SAFETY: caller guarantees a live request.
        unsafe {
            for_each_header(&mut (*r).headers_out.headers, |header| {
                let name = ngx_str_text(&raw const (*header).key);
                if strips_override_token((*header).hash != 0, &name) {
                    (*header).hash = 0;
                }
                true
            });
        }
    }

    /// Clears the etag (`ngx_http_clear_etag`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request.
    unsafe fn clear_etag(r: *mut ngx_http_request_t) {
        // SAFETY: caller guarantees a live request.
        unsafe {
            if !(*r).headers_out.etag.is_null() {
                (*(*r).headers_out.etag).hash = 0;
                (*r).headers_out.etag = std::ptr::null_mut();
            }
        }
    }

    /// Clears last-modified (`ngx_http_clear_last_modified`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request.
    unsafe fn clear_last_modified(r: *mut ngx_http_request_t) {
        // SAFETY: caller guarantees a live request.
        unsafe {
            (*r).headers_out.last_modified_time = -1;
            if !(*r).headers_out.last_modified.is_null() {
                (*(*r).headers_out.last_modified).hash = 0;
                (*r).headers_out.last_modified = std::ptr::null_mut();
            }
        }
    }

    /// Installs the fixup filter
    /// (`ps_html_rewrite_fix_headers_filter_init`; header only).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::ngx_http_top_header_filter;
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn plan_matrix() {
            use FixHeadersAction::{FixThenNext, Next, StripTokenThenNext};
            use PreserveCachingHeaders::{DontPreserve, PreserveAll, PreserveOnlyCacheControl};
            assert_eq!(Next, fix_headers_plan(false, true, true, DontPreserve));
            assert_eq!(
                StripTokenThenNext,
                fix_headers_plan(true, true, false, DontPreserve)
            );
            assert_eq!(
                StripTokenThenNext,
                fix_headers_plan(true, false, true, DontPreserve)
            );
            assert_eq!(
                StripTokenThenNext,
                fix_headers_plan(true, true, true, PreserveAll)
            );
            assert_eq!(
                FixThenNext {
                    rewrite_cache_control: true
                },
                fix_headers_plan(true, true, true, DontPreserve)
            );
            assert_eq!(
                FixThenNext {
                    rewrite_cache_control: false
                },
                fix_headers_plan(true, true, true, PreserveOnlyCacheControl)
            );
        }
    }
}

/// Base-fetch filter (`namespace ps_base_fetch`).
pub mod base_fetch {
    use super::*;
    use crate::fetch::{base_fetch_gate, BaseFetchGate};
    use crate::headers::ngx_str_text;

    static mut NEXT_BODY: ngx_http_output_body_filter_pt = None;
    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;

    /// Runs the next header filter (`ngx_http_next_header_filter`).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; call after `init`.
    pub unsafe fn next_header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        // SAFETY: caller guarantees a live request.
        unsafe {
            let next = NEXT_HEADER.expect("base-fetch chain not initialized");
            next(r)
        }
    }

    /// Body filter (`ps_base_fetch_filter`): header-only requests
    /// complete; requests without an active fetch pass through;
    /// the rest forward whatever is ready and report `NGX_AGAIN`
    /// (done-ness belongs to the fetch, not the filter).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `in` is the body chain;
    /// the chain must be initialized.
    pub unsafe extern "C" fn body_filter(
        r: *mut ngx_http_request_t,
        chain: *mut ngx_chain_t,
    ) -> ngx_int_t {
        use ngx::ffi::{NGX_AGAIN, NGX_OK};
        // SAFETY: caller guarantees a live request.
        unsafe {
            let pass_to_next = move |chain: *mut ngx_chain_t| {
                let next = NEXT_BODY.expect("base-fetch chain not initialized");
                // SAFETY: `r`/`chain` per the caller contract.
                next(r, chain)
            };
            let ctx = crate::request::get_request_ctx(r);
            let fetch_active = !ctx.is_null() && (*ctx).base_fetch.is_some();
            match base_fetch_gate(crate::request::request_header_only(r) != 0, fetch_active) {
                BaseFetchGate::Complete => NGX_OK as ngx_int_t,
                BaseFetchGate::PassThrough => pass_to_next(chain),
                BaseFetchGate::Serve => {
                    let uri = ngx_str_text(&raw const (*r).uri);
                    let log = (*(*r).connection).log;
                    ngx::ngx_log_error!(
                        ngx::ffi::NGX_LOG_DEBUG_HTTP,
                        log,
                        "http pagespeed write filter \"{}\"",
                        uri
                    );
                    if !chain.is_null() || (*(*r).connection).buffered() != 0 {
                        let rc = pass_to_next(chain);
                        if rc != NGX_OK as ngx_int_t {
                            return rc;
                        }
                    }
                    NGX_AGAIN as ngx_int_t
                }
            }
        }
    }

    /// Installs the filter (`ps_base_fetch_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::{ngx_http_top_body_filter, ngx_http_top_header_filter};
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            NEXT_BODY = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
    }
}

/// HTML-rewrite filters (`namespace html_rewrite`).
pub mod html_rewrite {
    use super::*;
    use crate::headers::{for_each_header, ngx_str_text};
    use crate::request::RequestCtx;
    use ps_http::content_type::mime_to_content_type;
    use ps_http::inflate::{GzipInflater, InflateType, MAX_INFLATED_HTML_BYTES};

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;
    static mut NEXT_BODY: ngx_http_output_body_filter_pt = None;

    /// Early header gates (`ps_html_rewrite_header_filter` head):
    /// disabled servers, subrequests, and requests without an
    /// armed context pass through untouched.
    #[must_use]
    pub const fn header_early_gate(disabled: bool, is_main: bool, ctx_armed: bool) -> bool {
        disabled || !is_main || !ctx_armed
    }

    /// Content gate: errored responses disarm; only HTML-like
    /// content types proceed to the resource handler.
    #[must_use]
    pub fn header_content_gate(err_status: bool, content_type: &str) -> bool {
        if err_status {
            return false;
        }
        mime_to_content_type(content_type).is_some_and(|content| content.is_html_like())
    }

    /// Content-encoding disposition.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EncodingAction {
        /// Leave the encoding alone.
        Pass,
        /// Strip the header and decode (`GzipInflater`).
        Inflate(InflateType),
    }

    /// Classifies the response encoding: stacked encodings pass
    /// through untouched; single `gzip`/`deflate` (case-insensitive)
    /// decode.
    #[must_use]
    pub fn classify_content_encoding(stacked: bool, value: Option<&str>) -> EncodingAction {
        if stacked {
            return EncodingAction::Pass;
        }
        match value {
            Some(value) if value.eq_ignore_ascii_case("deflate") => {
                EncodingAction::Inflate(InflateType::Deflate)
            }
            Some(value) if value.eq_ignore_ascii_case("gzip") => {
                EncodingAction::Inflate(InflateType::Gzip)
            }
            _ => EncodingAction::Pass,
        }
    }

    /// Whether response headers carry stacked content encodings
    /// (`ps_has_stacked_content_encoding`): comma-separated fields
    /// counted across every `Content-Encoding` line (case-insensitive
    /// key, unhashed lines included); more than one field stacks.
    #[must_use]
    pub fn has_stacked_content_encoding(headers: &[(&str, &str)]) -> bool {
        let mut field_count = 0u32;
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("Content-Encoding") || value.is_empty() {
                continue;
            }
            for (index, byte) in value.bytes().enumerate() {
                if byte == b',' || index == value.len() - 1 {
                    field_count += 1;
                }
            }
            if field_count > 1 {
                return true;
            }
        }
        false
    }

    /// Whether an HTML response header is stripped
    /// (`ps_strip_html_headers` per header): `Accept-Ranges`, and
    /// exactly `Vary: Accept-Encoding` (the gzip filter re-adds it).
    #[must_use]
    pub fn strips_html_header(name: &str, value: &str) -> bool {
        name.eq_ignore_ascii_case("Accept-Ranges")
            || (name.eq_ignore_ascii_case("Vary") && value.eq_ignore_ascii_case("Accept-Encoding"))
    }

    /// Fresh rewritten-HTML lifetime (`ProxyFetch::SetupForHtml`
    /// caching half): ttl 0 unless the HTML cache time allows more
    /// and the origin line lacks whole-value `no-cache` /
    /// `must-revalidate`; the suffix preserves `no-transform`,
    /// `no-store`, and standalone `s-maxage` values. Replaces
    /// `Cache-Control` and stamps `Expires` (`SetDateAndCaching`),
    /// so an in-place `s-maxage` stamp never survives on a
    /// rewritten page (the fix-up filter drops the recombined
    /// line's tail below).
    fn setup_for_html_caching(
        headers: &mut ps_http::response::ResponseHeaders,
        max_html_cache_time_ms: i64,
    ) {
        use crate::caching::{read_lifetime_directive, LifetimeName};
        use ps_http::names::header;
        let values = headers
            .headers()
            .lookup(header::CACHE_CONTROL)
            .unwrap_or_default();
        let whole_value = |want: &str| values.iter().any(|v| v.eq_ignore_ascii_case(want));
        let (ttl_ms, suffix) = if max_html_cache_time_ms == 0
            || whole_value("no-cache")
            || whole_value("must-revalidate")
        {
            let mut suffix = String::from(", no-cache");
            if headers.has_cache_control_directive("no-transform") {
                suffix.push_str(", no-transform");
            }
            if headers.has_cache_control_directive("no-store") {
                suffix.push_str(", no-store");
            }
            for value in &values {
                let lifetime = read_lifetime_directive(value);
                if lifetime.name == LifetimeName::SMaxAge && !value.contains(';') {
                    suffix.push_str(", ");
                    suffix.push_str(value);
                }
            }
            (0, suffix)
        } else {
            (
                max_html_cache_time_ms.min(headers.cache_ttl_ms()),
                String::from(", private"),
            )
        };
        let date_ms = headers.date_ms();
        headers.set_time_header(header::EXPIRES, date_ms + ttl_ms);
        headers.headers_mut().replace(
            header::CACHE_CONTROL,
            &format!("max-age={}{suffix}", ttl_ms / 1000),
        );
    }

    /// `html_cache` diagnostics tail (`SetupForHtml`, also
    /// gated on `show_cache_timestamp`): per-request
    /// diagnostics must never freeze in a browser/CDN cache.
    fn setup_for_html_cache_diagnostics(headers: &mut ps_http::response::ResponseHeaders) {
        use ps_http::names::header;
        let date_ms = headers.date_ms();
        headers.set_time_header(header::EXPIRES, date_ms);
        headers
            .headers_mut()
            .replace(header::CACHE_CONTROL, "max-age=0, private, no-store");
        for name in ["Content-MD5", "Digest", "Content-Digest", "Repr-Digest"] {
            headers.headers_mut().remove_all(name);
        }
        headers.compute_caching();
    }

    /// AVIF `Vary` additions: rewritten HTML can carry
    /// User-Agent/Accept-selected AVIF URLs, so both vary keys are
    /// ensured present.
    #[must_use]
    pub fn avif_vary_additions(
        avif_enabled: bool,
        has_ua: bool,
        has_accept: bool,
    ) -> Vec<(&'static str, &'static str)> {
        let mut additions = Vec::new();
        if !avif_enabled {
            return additions;
        }
        if !has_ua {
            additions.push(("Vary", "User-Agent"));
        }
        if !has_accept {
            additions.push(("Vary", "Accept"));
        }
        additions
    }

    /// Header filter (`ps_html_rewrite_header_filter`).
    ///
    /// Runs the live gates, then declines: Slice 6 installs the
    /// resource-handler call plus `finish_html_header_filter`,
    /// which run the encoding, strip, copy, and dispatch tail.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized.
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        use crate::module::srv_conf_of_request;
        // SAFETY: caller guarantees a live request.
        unsafe {
            let pass_to_next = move || {
                let next = NEXT_HEADER.expect("html-rewrite header chain not initialized");
                // SAFETY: `r` is live per the caller contract.
                next(r)
            };
            let cfg_s = &*srv_conf_of_request(r);
            let ctx = crate::request::get_request_ctx(r);
            if header_early_gate(
                cfg_s.disabled(),
                r == (*r).main,
                !ctx.is_null() && (*ctx).html_rewrite,
            ) {
                return pass_to_next();
            }
            if let Some(context) = cfg_s.context.as_ref() {
                if context.initialized() {
                    context.poll_cache_flush();
                }
            }
            let ctx = &mut *ctx;
            let content_type = ngx_str_text(&raw const (*r).headers_out.content_type);
            if !header_content_gate((*r).err_status != 0, &content_type) {
                ctx.html_rewrite = false;
                return pass_to_next();
            }
            {
                let uri = ngx_str_text(&raw const (*r).uri);
                let log = (*(*r).connection).log;
                ngx::ngx_log_error!(
                    ngx::ffi::NGX_LOG_DEBUG_HTTP,
                    log,
                    "http pagespeed html rewrite header filter \"{}\"",
                    uri
                );
            }
            let rc = crate::module::resource_response_filtered(r);
            if rc != ngx::ffi::NGX_OK as ngx::ffi::ngx_int_t {
                ctx.html_rewrite = false;
                return pass_to_next();
            }
            let ctx_ptr = crate::request::get_request_ctx(r);
            let fetch = (*ctx_ptr)
                .base_fetch
                .as_ref()
                .expect("html branch attaches a fetch");
            fetch.with_state(|base| {
                finish_html_header_filter(r, ctx_ptr, base.response_headers_mut())
            })
        }
    }

    /// Post-resource header tail (runs after Slice 6's resource call
    /// succeeds): encoding strip with decoder arming, privacy-limit
    /// noting, HTML header strip, location mirroring, header copy
    /// into the fetch, AVIF vary keys, buffering, `NGX_AGAIN`.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request with a connected socket;
    /// `ctx` at its attached armed context; `base_headers` at the
    /// fetch's live response headers.
    pub unsafe fn finish_html_header_filter(
        r: *mut ngx_http_request_t,
        ctx: *mut RequestCtx,
        base_headers: &mut ps_http::response::ResponseHeaders,
    ) -> ngx_int_t {
        use crate::caching::note_cache_control_limits;
        use crate::headers::copy_response_headers;
        use crate::module::srv_conf_of_request;
        use ngx::ffi::NGX_AGAIN;
        use ps_config::filter::Filter;
        use ps_http::names::header;
        // SAFETY: caller guarantees as documented.
        unsafe {
            // Content-encoding handling.
            let encoding = (*r).headers_out.content_encoding;
            if !encoding.is_null() && (*encoding).value.len > 0 {
                let mut pairs = Vec::new();
                for_each_header(&mut (*r).headers_out.headers, |header| {
                    let name = ngx_str_text(&raw const (*header).key);
                    let value = ngx_str_text(&raw const (*header).value);
                    // The pairs borrow from nginx memory; stash owned
                    // copies for the pure check below.
                    pairs.push((name, value));
                    true
                });
                let refs: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(n, v)| (n.as_str(), v.as_str()))
                    .collect();
                let stacked = has_stacked_content_encoding(&refs);
                let value = ngx_str_text(&raw const (*encoding).value);
                if let EncodingAction::Inflate(inflate_type) =
                    classify_content_encoding(stacked, Some(&value))
                {
                    (*encoding).hash = 0;
                    (*r).headers_out.content_encoding = std::ptr::null_mut();
                    let mut inflater = GzipInflater::new(inflate_type);
                    let _ = inflater.init();
                    inflater.set_max_inflated_bytes(MAX_INFLATED_HTML_BYTES);
                    (*ctx).inflater = Some(inflater);
                }
            }
            // Privacy limits before the rewrite computes caching headers.
            let mut limits = crate::caching::CacheControlLimits::default();
            for_each_header(&mut (*r).headers_out.headers, |header| {
                if (*header).hash != 0 {
                    let name = ngx_str_text(&raw const (*header).key);
                    if name.eq_ignore_ascii_case("Cache-Control") {
                        let value = ngx_str_text(&raw const (*header).value);
                        note_cache_control_limits(&value, &mut limits);
                    }
                }
                true
            });
            (*ctx).html_cache_control_limits = limits;
            strip_html_headers(r);
            (*ctx).location_field_set = !(*r).headers_out.location.is_null();
            // Copy response headers into the fetch, then AVIF vary keys.
            let pairs = collect_response_headers(r);
            let refs: Vec<crate::headers::RawHeader<'_>> = pairs
                .iter()
                .map(|(name, value, hashed)| (name.as_str(), value.as_str(), *hashed))
                .collect();
            let content_type = ngx_str_text(&raw const (*r).headers_out.content_type);
            copy_response_headers(
                (*r).http_version,
                (*r).headers_out.status,
                &content_type,
                &refs,
                now_ms(),
                base_headers,
            );
            let cfg_s = &*srv_conf_of_request(r);
            let avif = cfg_s
                .context
                .as_ref()
                .is_some_and(|ctx| ctx.options.core.filters().enabled(Filter::ConvertToAvif));
            let (max_html_cache_time_ms, cache_diagnostics) = cfg_s
                .context
                .as_ref()
                .map(|ctx| {
                    (
                        ctx.options.core.max_html_cache_time_ms(),
                        ctx.options.core.html_cache() || ctx.options.core.show_cache_timestamp(),
                    )
                })
                .unwrap_or((0, false));
            for (name, value) in avif_vary_additions(
                avif,
                base_headers.has_value(header::VARY, header::USER_AGENT),
                base_headers.has_value(header::VARY, header::ACCEPT),
            ) {
                base_headers.headers_mut().add(name, value);
            }
            // Fresh rewritten-HTML lifetime
            // (`ProxyFetch::SetupForHtml` caching).
            setup_for_html_caching(base_headers, max_html_cache_time_ms);
            if cache_diagnostics {
                setup_for_html_cache_diagnostics(base_headers);
            }
            // Rewritten bytes invalidate the origin's validators
            // (`ProxyFetch::SetupForHtml`).
            base_headers.headers_mut().remove_all(header::ETAG);
            base_headers.headers_mut().remove_all(header::LAST_MODIFIED);
            base_headers
                .headers_mut()
                .remove_all(header::CONTENT_LENGTH);
            crate::request::set_buffered(r, true);
            crate::request::set_request_filter_need_in_memory(r);
            NGX_AGAIN as ngx_int_t
        }
    }

    /// Collects response headers for the copy core (the core
    /// skips unhashed entries itself).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request.
    unsafe fn collect_response_headers(r: *mut ngx_http_request_t) -> Vec<(String, String, bool)> {
        // SAFETY: caller guarantees a live request.
        unsafe {
            let mut pairs = Vec::new();
            for_each_header(&mut (*r).headers_out.headers, |header| {
                pairs.push((
                    ngx_str_text(&raw const (*header).key),
                    ngx_str_text(&raw const (*header).value),
                    (*header).hash != 0,
                ));
                true
            });
            pairs
        }
    }

    /// Strips HTML response headers (`ps_strip_html_headers`):
    /// content length plus `Accept-Ranges` / `Vary: Accept-Encoding`.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request.
    unsafe fn strip_html_headers(r: *mut ngx_http_request_t) {
        // SAFETY: caller guarantees a live request.
        unsafe {
            (*r).headers_out.content_length_n = -1;
            if !(*r).headers_out.content_length.is_null() {
                (*(*r).headers_out.content_length).hash = 0;
                (*r).headers_out.content_length = std::ptr::null_mut();
            }
            for_each_header(&mut (*r).headers_out.headers, |header| {
                let name = ngx_str_text(&raw const (*header).key);
                let value = ngx_str_text(&raw const (*header).value);
                if strips_html_header(&name, &value) {
                    (*header).hash = 0;
                }
                true
            });
        }
    }

    /// Current time in ms since the epoch (`PosixTimer::NowMs`).
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Body filter (`ps_html_rewrite_body_filter`).
    ///
    /// Runs the live gates; armed requests send the chain through
    /// the fetch sink and pass NULL down.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `chain` is the body chain;
    /// the chain must be initialized.
    pub unsafe extern "C" fn body_filter(
        r: *mut ngx_http_request_t,
        chain: *mut ngx_chain_t,
    ) -> ngx_int_t {
        use crate::module::srv_conf_of_request;
        // SAFETY: caller guarantees a live request.
        unsafe {
            let pass_to_next = move |chain: *mut ngx_chain_t| {
                let next = NEXT_BODY.expect("html-rewrite body chain not initialized");
                // SAFETY: `r`/`chain` per the caller contract.
                next(r, chain)
            };
            let cfg_s = &*srv_conf_of_request(r);
            if cfg_s.disabled() {
                return pass_to_next(chain);
            }
            if r != (*r).main {
                return pass_to_next(chain);
            }
            let ctx = crate::request::get_request_ctx(r);
            if ctx.is_null() || !(*ctx).html_rewrite {
                return pass_to_next(chain);
            }
            assert_eq!(
                0,
                (*r).err_status,
                "errored request reached the body filter"
            );
            // Armed: send the chain through the fetch sink (a null
            // chain sends nothing) and pass NULL down.
            {
                let uri = ngx_str_text(&raw const (*r).uri);
                let log = (*(*r).connection).log;
                ngx::ngx_log_error!(
                    ngx::ffi::NGX_LOG_DEBUG_HTTP,
                    log,
                    "http pagespeed html rewrite body filter \"{}\"",
                    uri
                );
            }
            let sink = (*ctx)
                .html_sink
                .as_mut()
                .expect("html branch attaches a sink");
            let _outcome = finish_html_body_filter(ctx, chain, sink);
            pass_to_next(std::ptr::null_mut())
        }
    }

    /// Sends one body chain through the fetch
    /// (`ps_html_rewrite_body_filter` past gating): extracts buffer
    /// views (clearing `last_buf` up front), sends, marks every
    /// buffer sent. Returns the send outcome for warning reporting.
    ///
    /// # Safety
    ///
    /// `ctx` must point at an attached armed context; `chain` at a
    /// live chain (possibly null, which sends nothing).
    pub unsafe fn finish_html_body_filter(
        ctx: *mut RequestCtx,
        chain: *mut ngx_chain_t,
        sink: &mut dyn crate::send::BodySink,
    ) -> crate::send::SendOutcome {
        use crate::send::{send_to_pagespeed, SendBuffer, SendOutcome};
        use ngx::ffi::ngx_read_file;
        // File-backed buffers (static files) carry no memory
        // bytes, and dynamic modules run above the copy filter
        // that would convert them, so their ranges are read here
        // in chunks. C++ never sees this case through its own
        // choreography; the fed bytes are what matters.
        const FILE_CHUNK_BYTES: usize = 65536;
        // SAFETY: caller guarantees as documented.
        unsafe {
            let mut outcome = SendOutcome::default();
            let mut bufs = Vec::new();
            let mut link = chain;
            while !link.is_null() {
                let buf = (*link).buf;
                let flush = (*buf).flush() != 0;
                let last_buf = (*buf).last_buf() != 0;
                if (*buf).in_file() != 0 && !(*buf).file.is_null() {
                    let file = (*buf).file;
                    let mut offset = (*buf).file_pos;
                    let end = (*buf).file_last;
                    let mut sent_any = false;
                    while offset < end {
                        #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
                        let want = ((end - offset) as usize).min(FILE_CHUNK_BYTES);
                        let mut chunk = vec![0u8; want];
                        let got = ngx_read_file(file, chunk.as_mut_ptr(), want, offset);
                        if got <= 0 {
                            break;
                        }
                        #[allow(clippy::cast_sign_loss)]
                        let got = got as usize;
                        chunk.truncate(got);
                        offset += got as i64;
                        sent_any = true;
                        let last_chunk = offset >= end;
                        let partial = send_to_pagespeed(
                            &[SendBuffer {
                                data: &chunk,
                                flush: flush && last_chunk,
                                last_buf: last_buf && last_chunk,
                            }],
                            (*ctx).inflater.as_mut(),
                            (*ctx).follow_flushes,
                            sink,
                        );
                        outcome.buffers_sent += partial.buffers_sent;
                        outcome.fetch_done |= partial.fetch_done;
                        if outcome.inflate_error.is_none() {
                            outcome.inflate_error = partial.inflate_error;
                        }
                    }
                    if !sent_any {
                        // Empty or unreadable range: still deliver
                        // the flags so completion fires.
                        let partial = send_to_pagespeed(
                            &[SendBuffer {
                                data: &[],
                                flush,
                                last_buf,
                            }],
                            (*ctx).inflater.as_mut(),
                            (*ctx).follow_flushes,
                            sink,
                        );
                        outcome.buffers_sent += partial.buffers_sent;
                        outcome.fetch_done |= partial.fetch_done;
                        if outcome.inflate_error.is_none() {
                            outcome.inflate_error = partial.inflate_error;
                        }
                    }
                } else {
                    let data = std::slice::from_raw_parts(
                        (*buf).pos,
                        (*buf).last.offset_from((*buf).pos) as usize,
                    );
                    let partial = send_to_pagespeed(
                        &[SendBuffer {
                            data,
                            flush,
                            last_buf,
                        }],
                        (*ctx).inflater.as_mut(),
                        (*ctx).follow_flushes,
                        sink,
                    );
                    outcome.buffers_sent += partial.buffers_sent;
                    outcome.fetch_done |= partial.fetch_done;
                    if outcome.inflate_error.is_none() {
                        outcome.inflate_error = partial.inflate_error;
                    }
                }
                (*buf).set_last_buf(0);
                bufs.push(buf);
                link = (*link).next;
            }
            for buf in bufs {
                (*buf).pos = (*buf).last;
            }
            outcome
        }
    }

    /// Installs the rewrite filters (`ps_html_rewrite_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::{ngx_http_top_body_filter, ngx_http_top_header_filter};
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
            NEXT_BODY = ngx_http_top_body_filter;
            ngx_http_top_body_filter = Some(body_filter);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::caching::PreserveCachingHeaders;

        #[test]
        fn gate_matrix() {
            assert!(header_early_gate(true, true, true));
            assert!(header_early_gate(false, false, true));
            assert!(header_early_gate(false, true, false));
            assert!(!header_early_gate(false, true, true));
            assert!(!header_content_gate(true, "text/html"));
            assert!(header_content_gate(false, "text/html"));
            assert!(header_content_gate(false, "text/html; charset=utf-8"));
            assert!(header_content_gate(false, "TEXT/HTML"));
            assert!(!header_content_gate(false, "text/css"));
            assert!(!header_content_gate(false, "application/octet-stream"));
            assert!(!header_content_gate(false, ""));
        }

        #[test]
        fn encoding_classification() {
            use EncodingAction::{Inflate, Pass};
            assert_eq!(Pass, classify_content_encoding(true, Some("gzip")));
            assert_eq!(
                Inflate(InflateType::Gzip),
                classify_content_encoding(false, Some("gzip"))
            );
            assert_eq!(
                Inflate(InflateType::Gzip),
                classify_content_encoding(false, Some("GZip"))
            );
            assert_eq!(
                Inflate(InflateType::Deflate),
                classify_content_encoding(false, Some("deflate"))
            );
            assert_eq!(Pass, classify_content_encoding(false, Some("br")));
            assert_eq!(Pass, classify_content_encoding(false, Some("")));
            assert_eq!(Pass, classify_content_encoding(false, None));
            // Stacking counts comma fields across every line.
            assert!(!has_stacked_content_encoding(&[]));
            assert!(!has_stacked_content_encoding(&[(
                "Content-Encoding",
                "gzip"
            )]));
            assert!(has_stacked_content_encoding(&[(
                "Content-Encoding",
                "gzip, deflate"
            )]));
            assert!(has_stacked_content_encoding(&[
                ("Content-Encoding", "gzip"),
                ("Content-Encoding", "deflate"),
            ]));
            assert!(!has_stacked_content_encoding(&[("Content-Encoding", "")]));
            assert!(!has_stacked_content_encoding(&[("X-Other", "a,b")]));
        }

        #[test]
        fn strip_and_vary_rules() {
            assert!(strips_html_header("Accept-Ranges", "bytes"));
            assert!(strips_html_header("accept-ranges", "bytes"));
            assert!(strips_html_header("Vary", "Accept-Encoding"));
            assert!(strips_html_header("vary", "accept-encoding"));
            assert!(!strips_html_header("Vary", "Cookie"));
            assert!(!strips_html_header("Vary", "Accept-Encoding, Cookie"));
            assert!(!strips_html_header("Cache-Control", "no-cache"));
            assert!(avif_vary_additions(false, false, false).is_empty());
            assert_eq!(
                vec![("Vary", "User-Agent"), ("Vary", "Accept")],
                avif_vary_additions(true, false, false)
            );
            assert_eq!(
                vec![("Vary", "Accept")],
                avif_vary_additions(true, true, false)
            );
            assert!(avif_vary_additions(true, true, true).is_empty());
        }

        #[test]
        fn setup_for_html_caching_replaces_lifetime() {
            use ps_http::names::header;
            use ps_http::response::{HttpOptions, ResponseHeaders};
            fn headers_with(cache_control: Option<&str>) -> ResponseHeaders {
                let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
                headers.set_status_code(200);
                headers
                    .headers_mut()
                    .add(header::DATE, "Thu, 01 Jan 1970 00:00:00 GMT");
                if let Some(value) = cache_control {
                    headers.headers_mut().add(header::CACHE_CONTROL, value);
                }
                headers
            }
            fn cache_control(headers: &ResponseHeaders) -> String {
                headers.headers().lookup_joined(header::CACHE_CONTROL)
            }
            // The in-place stamp recombines into the fresh line (the
            // fix-up filter drops its tail below).
            let mut headers = headers_with(Some("s-maxage=10"));
            setup_for_html_caching(&mut headers, 0);
            assert_eq!("max-age=0, no-cache, s-maxage=10", cache_control(&headers));
            assert_eq!(
                "Thu, 01 Jan 1970 00:00:00 GMT",
                headers.headers().lookup_joined(header::EXPIRES)
            );
            // No origin line: bare disabled lifetime.
            let mut headers = headers_with(None);
            setup_for_html_caching(&mut headers, 0);
            assert_eq!("max-age=0, no-cache", cache_control(&headers));
            // Whole-value `no-cache` forces ttl 0 even with headroom.
            let mut headers = headers_with(Some("no-cache"));
            setup_for_html_caching(&mut headers, 60_000);
            assert_eq!("max-age=0, no-cache", cache_control(&headers));
            // A combined line is not a whole-value match: the min
            // of the cap and the origin ttl wins, privately (the
            // engage copy computes caching first, like production).
            let mut headers = headers_with(Some("max-age=60"));
            headers.compute_caching();
            setup_for_html_caching(&mut headers, 60_000);
            assert_eq!("max-age=60, private", cache_control(&headers));
            let mut headers = headers_with(Some("max-age=60"));
            headers.compute_caching();
            setup_for_html_caching(&mut headers, 30_000);
            assert_eq!("max-age=30, private", cache_control(&headers));
            // Preserved suffixes ride along.
            let mut headers = headers_with(Some("no-store"));
            setup_for_html_caching(&mut headers, 0);
            assert_eq!("max-age=0, no-cache, no-store", cache_control(&headers));
            // Diagnostics tail freezes nothing.
            let mut headers = headers_with(Some("max-age=60"));
            headers.headers_mut().add("Digest", "md5=abc");
            setup_for_html_cache_diagnostics(&mut headers);
            assert_eq!("max-age=0, private, no-store", cache_control(&headers));
            assert!(headers.headers().lookup_joined("Digest").is_empty());
        }

        /// Recording sink for the finish test.
        #[derive(Default)]
        struct FakeSink {
            written: Vec<u8>,
            dones: usize,
        }

        impl crate::send::BodySink for FakeSink {
            fn write(&mut self, bytes: &[u8]) {
                self.written.extend_from_slice(bytes);
            }

            fn flush(&mut self) {}

            fn done(&mut self) {
                self.dones += 1;
            }
        }

        #[test]
        fn finish_consumes_fabricated_chain() {
            use ngx::ffi::{ngx_buf_t, ngx_chain_t};
            use std::mem::MaybeUninit;
            // SAFETY: all structures are test-owned; no nginx involved.
            unsafe {
                let mut first_bytes = *b"hello ";
                let mut second_bytes = *b"world";
                let mut first_buf: ngx_buf_t = MaybeUninit::zeroed().assume_init();
                first_buf.pos = first_bytes.as_mut_ptr();
                first_buf.last = first_bytes.as_mut_ptr().add(first_bytes.len());
                let mut second_buf: ngx_buf_t = MaybeUninit::zeroed().assume_init();
                second_buf.pos = second_bytes.as_mut_ptr();
                second_buf.last = second_bytes.as_mut_ptr().add(second_bytes.len());
                second_buf.set_last_buf(1);
                let mut second_link = ngx_chain_t {
                    buf: &mut second_buf,
                    next: std::ptr::null_mut(),
                };
                let mut first_link = ngx_chain_t {
                    buf: &mut first_buf,
                    next: &mut second_link,
                };
                let mut ctx = RequestCtx {
                    html_rewrite: true,
                    in_place: false,
                    preserve_caching_headers: PreserveCachingHeaders::DontPreserve,
                    html_cache_control_limits: Default::default(),
                    url_string: String::new(),
                    location_field_set: false,
                    psol_vary_accept_only: false,
                    follow_flushes: false,
                    inflater: None,
                    base_fetch: None,
                    html_sink: None,
                    recorder: None,
                };
                let mut sink = FakeSink::default();
                let outcome = finish_html_body_filter(&mut ctx, &mut first_link, &mut sink);
                assert_eq!(b"hello world".to_vec(), sink.written);
                assert_eq!(1, sink.dones);
                assert_eq!(2, outcome.buffers_sent);
                assert!(outcome.fetch_done);
                // Buffers marked sent; the last flag cleared.
                assert_eq!(first_buf.pos, first_buf.last);
                assert_eq!(second_buf.pos, second_buf.last);
                assert_eq!(0, second_buf.last_buf());
            }
        }
    }
}

/// Etag filter (`ps_etag_header_filter`, own module).
pub mod etag {
    use super::*;
    use crate::headers::for_each_header;

    static mut NEXT_HEADER: ngx_http_output_header_filter_pt = None;

    /// Internal weak-etag name (`kInternalEtagName`).
    pub const INTERNAL_ETAG_NAME: &str = "@psol-etag";

    /// Whether a key names the internal etag: length-checked,
    /// case-insensitive.
    #[must_use]
    pub fn is_internal_etag_name(key: &[u8]) -> bool {
        key.len() == INTERNAL_ETAG_NAME.len()
            && key.eq_ignore_ascii_case(INTERNAL_ETAG_NAME.as_bytes())
    }

    /// Header filter (`ps_etag_header_filter`): renames the first
    /// internal etag to `ETag`, clears `gzip_vary` for accept-only
    /// vary responses, and applies `s-maxage` to recordings.
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; the chain must be
    /// initialized.
    pub unsafe extern "C" fn header_filter(r: *mut ngx_http_request_t) -> ngx_int_t {
        use ngx::ffi::NGX_ERROR;
        // SAFETY: caller guarantees a live request.
        unsafe {
            static NAME: &[u8; 5] = b"ETag\0";
            for_each_header(&mut (*r).headers_out.headers, |header| {
                let key = std::slice::from_raw_parts((*header).key.data, (*header).key.len);
                if is_internal_etag_name(key) {
                    (*header).key.data = NAME.as_ptr() as *mut u8;
                    (*header).key.len = 4;
                    (*r).headers_out.etag = header;
                    return false;
                }
                true
            });
            let ctx = crate::request::get_request_ctx(r);
            // The C++ gates this on `NGX_HTTP_GZIP`; the staged tree
            // has it (pinned by test), so this runs unconditionally.
            if !ctx.is_null() && (*ctx).psol_vary_accept_only {
                crate::request::set_request_gzip_vary(r, 0);
            }
            // Never on responses to requests that carried
            // Authorization: s-maxage would grant downstream caches
            // a permission the origin did not give (RFC 9111 3.5).
            if !ctx.is_null()
                && (*ctx).recorder.is_some()
                && (*r).headers_in.authorization.is_null()
                && apply_recording_s_max_age(r, &mut *ctx).is_err()
            {
                return NGX_ERROR as ngx_int_t;
            }
            let next = NEXT_HEADER.expect("etag chain not initialized");
            // SAFETY: `r` is live per the caller contract.
            next(r)
        }
    }

    /// Applies `s-maxage` to a recording response: reads the global
    /// effective value, rewrites the line, and saves the original
    /// for the cache write (always `Some` on modern nginx: the
    /// presence flag below it is dead). `Err` on pool exhaustion
    /// (C++ would segfault; the port fails the request instead).
    ///
    /// # Safety
    ///
    /// `r` must point at a live request; `ctx` at its attached
    /// context with an active recorder.
    unsafe fn apply_recording_s_max_age(
        r: *mut ngx_http_request_t,
        ctx: *mut crate::request::RequestCtx,
    ) -> Result<(), ()> {
        use crate::caching::apply_s_max_age;
        use crate::headers::{get_cache_control, set_cache_control};
        // SAFETY: caller guarantees as documented.
        unsafe {
            let cfg_s = &*crate::module::srv_conf_of_request(r);
            let Some(global) = cfg_s.context.as_ref().map(|ctx| &ctx.options) else {
                return Ok(());
            };
            let s_maxage_sec = global.core.effective_in_place_s_max_age_sec();
            if s_maxage_sec == -1 {
                return Ok(());
            }
            let existing = get_cache_control(r);
            let Some(updated) = apply_s_max_age(s_maxage_sec, &existing) else {
                return Ok(());
            };
            let recorder = (*ctx).recorder.as_mut().expect("checked by caller");
            recorder.save_cache_control(Some(&existing));
            let rendered = crate::directives::pool_cstring((*r).pool, &updated);
            if rendered.is_null() || !set_cache_control(r, rendered) {
                return Err(());
            }
            Ok(())
        }
    }

    /// Installs the etag filter (`ps_etag_filter_init`).
    ///
    /// # Safety
    ///
    /// Call once from the etag module's postconfiguration.
    pub unsafe fn init() {
        use ngx::ffi::ngx_http_top_header_filter;
        // SAFETY: postconfiguration context.
        unsafe {
            NEXT_HEADER = ngx_http_top_header_filter;
            ngx_http_top_header_filter = Some(header_filter);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn internal_etag_matches() {
            assert!(is_internal_etag_name(b"@psol-etag"));
            assert!(is_internal_etag_name(b"@PSOL-ETAG"));
            assert!(!is_internal_etag_name(b"ETag"));
            assert!(!is_internal_etag_name(b"@psol-etagx"));
            assert!(!is_internal_etag_name(b"@psol-eta"));
            assert!(!is_internal_etag_name(b""));
        }

        #[test]
        fn staged_nginx_has_gzip() {
            // The `gzip_vary` write assumes `NGX_HTTP_GZIP`.
            assert_eq!(1, ngx::ffi::NGX_HTTP_GZIP);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_slots_in_load_bearing_order() {
        assert_eq!(6, EXECUTION_ORDER.len());
        assert_eq!(
            [
                FilterSlot::HtmlRewriteHeader,
                FilterSlot::HtmlRewriteBody,
                FilterSlot::BaseFetchBody,
                FilterSlot::FixHeadersHeader,
                FilterSlot::InPlaceHeader,
                FilterSlot::InPlaceBody,
            ],
            EXECUTION_ORDER
        );
        assert_eq!(
            ["in_place", "fix_headers", "base_fetch", "html_rewrite"],
            INIT_ORDER
        );
    }
}
