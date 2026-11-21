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
    /// HTML cache hit served; swallow the origin body.
    pub html_cache_served: bool,
    /// HTML cache hit entry awaiting serve (stashed by the header
    /// tail because serving touches fetch state, whose guard the
    /// finish call holds).
    pub html_cache_hit: Option<ps_cache::html::HtmlCacheEntry>,
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
            .field("html_cache_served", &self.html_cache_served)
            .field("html_cache_hit", &self.html_cache_hit.is_some())
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
            html_cache_served: false,
            html_cache_hit: None,
        }
    }
}

/// Buffered bit this module owns (`NGX_HTTP_PAGESPEED_BUFFERED`).
pub const PAGESPEED_BUFFERED: u32 = 0x08;

/// Corrected `ngx_http_request_t` bitfield access.
///
/// bindgen models the post-`port` run with 32-bit containers, but
/// gcc packs it in 16-bit containers here (the run starts 2-byte
/// aligned, so `count:16` fills the first unit): bits 46-47 are
/// dead padding and every field at bindgen offset >= 46 sits 2
/// bits later at runtime. bindgen's own accessors are therefore
/// wrong for those fields: `buffered()` read `logged` plus the
/// low `buffered` bits, and `set_filter_need_in_memory(1)` set
/// `buffered |= 0x08`, which deferred every declined response's
/// finalize into a `send_timeout` hang. C-verified field by field
/// against the staged tree (set-one-bit probes:
/// `no_buffering`@60, `gzip_vary`@66, `header_only`@80,
/// `keepalive`@82, `lingering_close`@83, `header_sent`@92,
/// `done`@96, `logged`@97, `buffered`@99:4,
/// `filter_need_in_memory`@104); fields below bit 46 (`count`,
/// `blocked`, ...) match bindgen and stay direct, as do the
/// `ngx_buf_t`, `ngx_connection_t`, and `ngx_http_headers_in_t`
/// runs (aligned starts, C-verified). Re-verify with the probes
/// before touching any other field here.
fn request_bit(r: *mut ngx::ffi::ngx_http_request_t, bindgen_offset: usize, width: u8) -> u32 {
    let bit = if bindgen_offset >= 46 {
        bindgen_offset + 2
    } else {
        bindgen_offset
    };
    // SAFETY: caller guarantees a live request; the unit spans
    // the whole run.
    unsafe { (*r)._bitfield_1.get(bit, width) as u32 }
}

/// Writes one corrected request bitfield (see [`request_bit`]).
///
/// # Safety
///
/// `r` must point at a live request.
unsafe fn set_request_bit(
    r: *mut ngx::ffi::ngx_http_request_t,
    bindgen_offset: usize,
    width: u8,
    val: u32,
) {
    let bit = if bindgen_offset >= 46 {
        bindgen_offset + 2
    } else {
        bindgen_offset
    };
    // SAFETY: caller guarantees a live request; the unit spans
    // the whole run.
    unsafe {
        (*r)._bitfield_1.set(bit, width, u64::from(val));
    }
}

/// Reads the request `buffered` mask (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn request_buffered(r: *mut ngx::ffi::ngx_http_request_t) -> u32 {
    request_bit(r, 97, 4)
}

/// Writes the request `buffered` mask (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_request_buffered(r: *mut ngx::ffi::ngx_http_request_t, val: u32) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        set_request_bit(r, 97, 4, val);
    }
}

/// Marks the request buffered or not (`ps_set_buffered`).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_buffered(r: *mut ngx::ffi::ngx_http_request_t, on: bool) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        let buffered = request_buffered(r);
        set_request_buffered(
            r,
            if on {
                buffered | PAGESPEED_BUFFERED
            } else {
                buffered & !PAGESPEED_BUFFERED
            },
        );
    }
}

/// Flags in-memory filtering (`filter_need_in_memory`, corrected
/// offset): the copy filter buffers file ranges in memory.
/// Must not use bindgen's accessor (it sets `buffered`).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_request_filter_need_in_memory(r: *mut ngx::ffi::ngx_http_request_t) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        set_request_bit(r, 102, 1, 1);
    }
}

/// Reads `header_only` (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn request_header_only(r: *mut ngx::ffi::ngx_http_request_t) -> u32 {
    request_bit(r, 78, 1)
}

/// Reads `header_sent` (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn request_header_sent(r: *mut ngx::ffi::ngx_http_request_t) -> u32 {
    request_bit(r, 90, 1)
}

/// Writes `gzip_vary` (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_request_gzip_vary(r: *mut ngx::ffi::ngx_http_request_t, val: u32) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        set_request_bit(r, 64, 1, val);
    }
}

/// Writes `keepalive` (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_request_keepalive(r: *mut ngx::ffi::ngx_http_request_t, val: u32) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        set_request_bit(r, 80, 1, val);
    }
}

/// Writes `lingering_close` (corrected offset).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn set_request_lingering_close(r: *mut ngx::ffi::ngx_http_request_t, val: u32) {
    // SAFETY: caller guarantees a live request.
    unsafe {
        set_request_bit(r, 81, 1, val);
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

    /// Corrected offsets land on distinct live bits (each
    /// C-verified field reads back what was written, and
    /// neighbors never alias — the bindgen +2 shift).
    #[test]
    fn corrected_bitfields_round_trip() {
        use std::mem::MaybeUninit;
        let mut request: ngx::ffi::ngx_http_request_t =
            unsafe { MaybeUninit::zeroed().assume_init() };
        let r = &mut request as *mut _;
        unsafe {
            set_request_buffered(r, 0);
            assert_eq!(0, request_buffered(r));
            set_request_buffered(r, 0x0F);
            assert_eq!(0x0F, request_buffered(r));
            // The pagespeed bit survives beside nginx's own.
            set_request_buffered(r, 0x04);
            set_buffered(r, true);
            assert_eq!(0x0C, request_buffered(r));
            set_buffered(r, false);
            assert_eq!(0x04, request_buffered(r));
            set_request_buffered(r, 0);
            // One-bit fields round-trip without aliasing the
            // `buffered` nibble or each other.
            set_request_filter_need_in_memory(r);
            assert_eq!(0, request_buffered(r));
            set_request_gzip_vary(r, 1);
            set_request_keepalive(r, 1);
            set_request_lingering_close(r, 1);
            assert_eq!(0, request_header_only(r));
            assert_eq!(0, request_header_sent(r));
            assert_eq!(0, request_buffered(r));
            // Raw unit check: `buffered` sits at run bits
            // 99-102 (byte 12 bits 3-6), `need_in_memory` at
            // run bit 104 (byte 13 bit 0).
            set_request_buffered(r, 0x0F);
            let raw: &[u8] =
                std::slice::from_raw_parts(&raw const request._bitfield_1 as *const u8, 15);
            assert_eq!(0x78, raw[12] & 0x78);
            set_request_buffered(r, 0);
            set_request_filter_need_in_memory(r);
            let raw: &[u8] =
                std::slice::from_raw_parts(&raw const request._bitfield_1 as *const u8, 15);
            assert_eq!(0x01, raw[13] & 0x01);
        }
    }
}
