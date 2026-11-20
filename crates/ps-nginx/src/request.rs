//! Per-request state (`ps_request_ctx_t`) and module-context lifecycle.
//!
//! Plain request data (rewrite flags, caching decisions, URL, field
//! mirrors) lives here with its create/get/release wiring.
//! Factory-owned handles (base fetch, proxy fetch, driver,
//! recorder, inflater) join in their slices and tear down through
//! `release_request_ctx` in C++ order.

use crate::caching::{CacheControlLimits, PreserveCachingHeaders};
use crate::fetch::SharedBaseFetch;
use crate::recording::InPlaceRecorder;
use crate::send::HtmlSink;
use ps_http::inflate::GzipInflater;
use std::sync::Arc;

/// Per-request state (`ps_request_ctx_t`, plain-data half plus the
/// response decoder, fetch bridge, and in-place recorder; factory
/// handles join in their slices).
pub struct RequestCtx {
    /// HTML rewrite armed for this request.
    pub html_rewrite: bool,
    /// Serving an in-place resource.
    pub in_place: bool,
    /// Which caching headers to keep from the server response.
    pub preserve_caching_headers: PreserveCachingHeaders,
    /// Page privacy limits for the rewritten `Cache-Control`.
    pub html_cache_control_limits: CacheControlLimits,
    /// Request URL (query params stripped, proto honored).
    pub url_string: String,
    /// Upstream set `Location:` (mirrored on write-back).
    pub location_field_set: bool,
    /// PSOL emitted bare `Vary: Accept-Encoding`.
    pub psol_vary_accept_only: bool,
    /// Mirror incoming flushes into the fetch.
    pub follow_flushes: bool,
    /// Response decoder (`inflater_`), armed when the origin
    /// content-encoding is stripped for rewriting.
    pub inflater: Option<GzipInflater>,
    /// Output bridge (`base_fetch`), active while PageSpeed owns
    /// the response (nginx-side reference).
    pub base_fetch: Option<Arc<SharedBaseFetch>>,
    /// HTML rewrite sink (live while filtering a response).
    pub html_sink: Option<HtmlSink<'static>>,
    /// In-place recorder (`recorder`), active while caching a
    /// missed resource for future optimization.
    pub recorder: Option<InPlaceRecorder>,
}

impl std::fmt::Debug for RequestCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestCtx")
            .field("html_rewrite", &self.html_rewrite)
            .field("in_place", &self.in_place)
            .field("preserve_caching_headers", &self.preserve_caching_headers)
            .field("html_cache_control_limits", &self.html_cache_control_limits)
            .field("url_string", &self.url_string)
            .field("location_field_set", &self.location_field_set)
            .field("psol_vary_accept_only", &self.psol_vary_accept_only)
            .field("follow_flushes", &self.follow_flushes)
            .field("inflater_armed", &self.inflater.is_some())
            .field("fetch_active", &self.base_fetch.is_some())
            .field("recording", &self.recorder.is_some())
            .finish()
    }
}

impl RequestCtx {
    /// Initializes a resource-path context (`ps_resource_handler`
    /// `!html_rewrite` block, past allocation).
    #[must_use]
    pub fn for_resource(
        url_string: &str,
        follow_flushes: bool,
        preserve: PreserveCachingHeaders,
    ) -> Self {
        RequestCtx {
            html_rewrite: false,
            in_place: false,
            preserve_caching_headers: preserve,
            html_cache_control_limits: CacheControlLimits::default(),
            url_string: url_string.to_string(),
            location_field_set: false,
            psol_vary_accept_only: false,
            follow_flushes,
            inflater: None,
            base_fetch: None,
            html_sink: None,
            recorder: None,
        }
    }
}

/// Buffered bit this module owns (`NGX_HTTP_PAGESPEED_BUFFERED`).
pub const PAGESPEED_BUFFERED: u32 = 0x08;

/// Marks the request buffered or not (`ps_set_buffered`).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_buffered(r: *mut ngx::ffi::ngx_http_request_t, on: bool) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        let buffered = (*r).buffered();
        (*r).set_buffered(if on {
            buffered | PAGESPEED_BUFFERED
        } else {
            buffered & !PAGESPEED_BUFFERED
        });
    }
}

/// Reads this request's context (`ps_get_request_context`).
///
/// # Safety
///
/// `r` must point at a live request; the module must be loaded
/// (index set). Returns null when no context is attached.
#[must_use]
pub unsafe fn get_request_ctx(r: *mut ngx::ffi::ngx_http_request_t) -> *mut RequestCtx {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let index = crate::module::NGX_PAGE_SPEED.ctx_index;
        *(*r).ctx.add(index) as *mut RequestCtx
    }
}

/// Attaches a context to a request (`ngx_http_set_ctx`).
///
/// # Safety
///
/// `r` must point at a live request with no context attached yet;
/// `ctx` must point at a live boxed context the request takes
/// ownership of (released by `release_request_ctx`).
pub unsafe fn set_request_ctx(r: *mut ngx::ffi::ngx_http_request_t, ctx: *mut RequestCtx) {
    // SAFETY: caller guarantees as documented.
    unsafe {
        let index = crate::module::NGX_PAGE_SPEED.ctx_index;
        *(*r).ctx.add(index) = ctx as *mut std::ffi::c_void;
    }
}

/// Releases a request context (`ps_release_request_context`).
///
/// Teardown order (verbatim): proxy fetch `Done(false)`, inflater
/// delete, driver `Cleanup`, recorder `DoneAndSetHeaders`, then
/// base-fetch detach, then the context itself. The context drop
/// covers the decoder, the fetch bridge, and the recorder today;
/// each factory handle joins its teardown here with its slice.
unsafe extern "C" fn release_request_ctx(data: *mut std::ffi::c_void) {
    // SAFETY: `data` came from `Box::into_raw` in `attach_request_ctx`;
    // the cleanup runs once.
    unsafe {
        drop(Box::from_raw(data as *mut RequestCtx));
    }
}

/// Attaches a fresh context with request-cleanup teardown
/// (`ps_resource_handler` context block, wiring half). Returns
/// false (dropping the context) when the cleanup slot fails.
///
/// # Safety
///
/// `r` must point at a live request with no context attached yet.
pub unsafe fn attach_request_ctx(r: *mut ngx::ffi::ngx_http_request_t, ctx: RequestCtx) -> bool {
    use ngx::ffi::ngx_http_cleanup_add;
    let raw = Box::into_raw(Box::new(ctx));
    // SAFETY: caller guarantees a live request; the box outlives
    // the request via cleanup.
    unsafe {
        let cleanup = ngx_http_cleanup_add(r, 0);
        if cleanup.is_null() {
            drop(Box::from_raw(raw));
            return false;
        }
        (*cleanup).handler = Some(release_request_ctx);
        (*cleanup).data = raw as *mut std::ffi::c_void;
        set_request_ctx(r, raw);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caching::PreserveCachingHeaders;

    #[test]
    fn resource_ctx_defaults() {
        let ctx = RequestCtx::for_resource(
            "http://h/a",
            true,
            PreserveCachingHeaders::PreserveOnlyCacheControl,
        );
        assert!(!ctx.html_rewrite);
        assert!(!ctx.in_place);
        assert_eq!(
            PreserveCachingHeaders::PreserveOnlyCacheControl,
            ctx.preserve_caching_headers
        );
        assert_eq!("http://h/a", ctx.url_string);
        assert!(!ctx.location_field_set);
        assert!(!ctx.psol_vary_accept_only);
        assert!(ctx.follow_flushes);
        assert_eq!(0x08, PAGESPEED_BUFFERED);
    }
}
