//! Header plumbing between nginx and PageSpeed
//! (`copy_*_headers_from_ngx`, `ps_set/get_cache_control`, the
//! request-option-override token).
//!
//! Copies are pure over extracted header lists; the FFI wrappers
//! walk nginx's header parts and pool. The response-to-nginx copy
//! (`copy_response_headers_to_ngx`) lands with the Slice 4 serving
//! path, its only consumer.

use ps_http::headers::Headers;
use ps_http::names::header;
use ps_http::request::{Method, RequestHeaders};
use ps_http::response::ResponseHeaders;

/// One extracted header (`key`, `value`, hashed into the output).
pub type RawHeader<'a> = (&'a str, &'a str, bool);

/// Reads an `ngx_str_t` lossily (`str_to_string_piece` for text we
/// match on; invalid UTF-8 mangles and fails closed downstream).
///
/// # Safety
///
/// `s` must point at a readable string.
pub(crate) unsafe fn ngx_str_text(s: *const ngx::ffi::ngx_str_t) -> String {
    // SAFETY: caller guarantees a readable string.
    unsafe {
        let bytes = std::slice::from_raw_parts((*s).data, (*s).len);
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Visits every element of an nginx list in order
/// (`NgxListIterator`), including unhashed entries (callers
/// filter). Stops early when `f` returns false.
///
/// # Safety
///
/// `list` must point at a live, well-formed `ngx_list_t` of
/// `ngx_table_elt_t` elements.
pub unsafe fn for_each_header(
    list: *mut ngx::ffi::ngx_list_t,
    mut f: impl FnMut(*mut ngx::ffi::ngx_table_elt_t) -> bool,
) {
    use ngx::ffi::{ngx_list_part_t, ngx_table_elt_t};
    // SAFETY: caller guarantees a live well-formed list.
    unsafe {
        let mut part = &mut (*list).part as *mut ngx_list_part_t;
        let mut index: usize = 0;
        loop {
            if index >= (*part).nelts {
                if (*part).next.is_null() {
                    return;
                }
                part = (*part).next;
                index = 0;
                continue;
            }
            let elt = (*part).elts.cast::<ngx_table_elt_t>().add(index);
            index += 1;
            if !f(elt) {
                return;
            }
        }
    }
}

/// Splits an nginx version (`http_version / 1000`, `% 1000`).
#[must_use]
pub fn split_version(http_version: usize) -> (i32, i32) {
    #[allow(clippy::cast_possible_wrap)]
    let major = (http_version / 1000) as i32;
    #[allow(clippy::cast_possible_wrap)]
    let minor = (http_version % 1000) as i32;
    (major, minor)
}

/// Copies request headers (`copy_request_headers_from_ngx` past
/// extraction): version, method, then every hashed header.
pub fn copy_request_headers(
    http_version: usize,
    method_name: &str,
    headers: &[RawHeader],
    out: &mut RequestHeaders,
) {
    let (major, minor) = split_version(http_version);
    out.set_version(major, minor);
    out.set_method(Method::from_string(method_name));
    copy_header_table(headers, out.headers_mut());
}

/// Copies response headers (`copy_response_headers_from_ngx` past
/// extraction): version, hashed headers, status, the out-of-band
/// content type, a defaulted `Date`, then caching computation.
/// `now_ms` feeds the default `Date` (injected clock).
pub fn copy_response_headers(
    http_version: usize,
    status: usize,
    content_type: &str,
    headers: &[RawHeader],
    now_ms: i64,
    out: &mut ResponseHeaders,
) {
    let (major, minor) = split_version(http_version);
    out.set_version(major, minor);
    copy_header_table(headers, out.headers_mut());
    #[allow(clippy::cast_possible_wrap)]
    out.set_status_code(status as i32);
    out.headers_mut().add(header::CONTENT_TYPE, content_type);
    if !out.headers().has(header::DATE) {
        out.set_date(now_ms);
    }
    out.compute_caching();
}

/// Copies hashed headers (`copy_headers_from_table`).
fn copy_header_table(headers: &[RawHeader], out: &mut Headers) {
    for (name, value, hashed) in headers {
        if *hashed {
            out.add(name, value);
        }
    }
}

/// Request-option-override token names
/// (`IsRequestOptionOverrideName`).
pub const OVERRIDE_TOKEN_NAMES: [&str; 2] = [
    "ModPagespeedRequestOptionOverride",
    "PageSpeedRequestOptionOverride",
];

/// Whether a header name is the override token
/// (`IsRequestOptionOverrideName`, case-insensitive).
#[must_use]
pub fn is_request_option_override_name(name: &str) -> bool {
    OVERRIDE_TOKEN_NAMES
        .iter()
        .any(|token| name.eq_ignore_ascii_case(token))
}

/// Whether a response header must lose its hash
/// (`ps_remove_option_override_token` per header): hashed entries
/// carrying the token.
#[must_use]
pub fn strips_override_token(hashed: bool, name: &str) -> bool {
    hashed && is_request_option_override_name(name)
}

/// Joins `Cache-Control` segments (`ps_get_cache_control` past
/// extraction): hashed values comma-joined. On modern nginx the
/// header-missing `false` is dead (an absent head joins to the
/// empty string), so this always yields a value.
#[must_use]
pub fn join_cache_control(values: &[&str]) -> String {
    values.join(", ")
}

/// Shortcut populated alongside a written entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundShortcut {
    /// `Date` shortcut.
    Date,
    /// `ETag` shortcut.
    Etag,
    /// `Expires` shortcut.
    Expires,
    /// `Last-Modified` shortcut.
    LastModified,
    /// `Location` shortcut (origin set one).
    Location,
    /// `Server` shortcut.
    Server,
    /// `Content-Length` shortcut plus length.
    ContentLength {
        /// Headers-wide parsed length.
        length: i64,
    },
    /// `Content-Encoding` shortcut.
    ContentEncoding,
    /// `Refresh` shortcut.
    Refresh,
    /// `Content-Range` shortcut.
    ContentRange,
    /// `Accept-Ranges` shortcut.
    AcceptRanges,
    /// `WWW-Authenticate` shortcut.
    WwwAuthenticate,
}

/// Outbound header disposition (`copy_response_headers_to_ngx` per
/// header).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundDisposition {
    /// Dropped (unpreserved, NUL bytes, unreadable length,
    /// `Connection`, `Keep-Alive`, `Transfer-Encoding`).
    Skip,
    /// List entry, written verbatim (pool-copied).
    Write {
        /// Header name to emit.
        name: String,
        /// Header value to emit.
        value: String,
        /// Shortcut to populate, if any.
        shortcut: Option<OutboundShortcut>,
        /// Flag bare `Vary: Accept-Encoding` on the context.
        set_psol_vary_accept_only: bool,
    },
    /// `Cache-Control` line (single-line writer).
    CacheControl {
        /// Header value to set.
        value: String,
    },
    /// `Content-Type` shortcut plus length.
    ContentType {
        /// Header value to set.
        value: String,
    },
}

/// Routes one outbound header: preservation first, then NUL
/// rejection, then `Content-Length` validation (headers-wide
/// lookup must succeed non-negative and this value all digits),
/// then the `Cache-Control`/`Content-Type` shortcuts (exact-case
/// names), the always-skipped connection/transfer names, the
/// internal-etag rename (`etag` name plus `W/` value), and finally
/// a plain write with a shortcut table lookup. `content_length`
/// is the headers-wide `FindContentLength` result.
#[must_use]
pub fn route_outbound_header(
    name: &str,
    value: &str,
    preserve: crate::caching::PreserveCachingHeaders,
    content_length: Option<i64>,
    location_field_set: bool,
) -> OutboundDisposition {
    use crate::caching::PreserveCachingHeaders;
    let preserved = match preserve {
        PreserveCachingHeaders::PreserveAll => !name.eq_ignore_ascii_case("Content-Length"),
        PreserveCachingHeaders::PreserveOnlyCacheControl => {
            name.eq_ignore_ascii_case("Cache-Control")
        }
        PreserveCachingHeaders::DontPreserve => true,
    };
    if !preserved || value.as_bytes().contains(&0) {
        return OutboundDisposition::Skip;
    }
    if name.eq_ignore_ascii_case("Content-Length")
        && (content_length.is_none_or(|length| length < 0)
            || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return OutboundDisposition::Skip;
    }
    if name == "Cache-Control" {
        return OutboundDisposition::CacheControl {
            value: value.to_string(),
        };
    }
    if name == "Content-Type" {
        return OutboundDisposition::ContentType {
            value: value.to_string(),
        };
    }
    if name.eq_ignore_ascii_case("Connection")
        || name.eq_ignore_ascii_case("Keep-Alive")
        || name.eq_ignore_ascii_case("Transfer-Encoding")
    {
        return OutboundDisposition::Skip;
    }
    let (name, shortcut, psol_vary) =
        route_outbound_write(name, value, content_length, location_field_set);
    OutboundDisposition::Write {
        name: name.to_string(),
        value: value.to_string(),
        shortcut,
        set_psol_vary_accept_only: psol_vary,
    }
}

/// Routes the generic write: the internal-etag rename plus the
/// shortcut table (exact-case names throughout, except
/// `Content-Length`).
fn route_outbound_write<'a>(
    name: &'a str,
    value: &str,
    content_length: Option<i64>,
    location_field_set: bool,
) -> (&'a str, Option<OutboundShortcut>, bool) {
    if name.eq_ignore_ascii_case("etag") && value.starts_with("W/") {
        return ("@psol-etag", None, false);
    }
    let psol_vary = name == "Vary" && value == "Accept-Encoding";
    let shortcut = match name {
        "Date" => Some(OutboundShortcut::Date),
        "Etag" => Some(OutboundShortcut::Etag),
        "Expires" => Some(OutboundShortcut::Expires),
        "Last-Modified" => Some(OutboundShortcut::LastModified),
        "Location" if location_field_set => Some(OutboundShortcut::Location),
        "Server" => Some(OutboundShortcut::Server),
        "Content-Encoding" => Some(OutboundShortcut::ContentEncoding),
        "Refresh" => Some(OutboundShortcut::Refresh),
        "Content-Range" => Some(OutboundShortcut::ContentRange),
        "Accept-Ranges" => Some(OutboundShortcut::AcceptRanges),
        "WWW-Authenticate" => Some(OutboundShortcut::WwwAuthenticate),
        _ => None,
    };
    let shortcut = shortcut.or_else(|| {
        if name.eq_ignore_ascii_case("Content-Length") {
            content_length.map(|length| OutboundShortcut::ContentLength { length })
        } else {
            None
        }
    });
    (name, shortcut, psol_vary)
}

/// Writes PageSpeed response headers into nginx
/// (`copy_response_headers_to_ngx`): routes every header, failing
/// the request on pool exhaustion. Returns false on allocation
/// failure. `set_psol_vary` reports whether any header flagged
/// bare `Vary: Accept-Encoding` (the caller mirrors it onto the
/// context).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn copy_response_headers_to_nginx(
    r: *mut ngx::ffi::ngx_http_request_t,
    headers: &ps_http::response::ResponseHeaders,
    preserve: crate::caching::PreserveCachingHeaders,
    location_field_set: bool,
    set_psol_vary: &mut bool,
) -> bool {
    use crate::directives::pool_cstring;
    use ngx::ffi::{ngx_list_push, ngx_str_t};
    // SAFETY: caller guarantees a live request.
    unsafe {
        (*r).headers_out.status = headers.status_code() as ngx::ffi::ngx_uint_t;
        let content_length = headers.find_content_length();
        for (name, value) in headers.headers().iter() {
            match route_outbound_header(name, value, preserve, content_length, location_field_set) {
                OutboundDisposition::Skip => {}
                OutboundDisposition::CacheControl { value } => {
                    let rendered = pool_cstring((*r).pool, &value);
                    if rendered.is_null() {
                        return false;
                    }
                    if !set_cache_control(r, rendered) {
                        return false;
                    }
                }
                OutboundDisposition::ContentType { value } => {
                    let rendered = pool_cstring((*r).pool, &value);
                    if rendered.is_null() {
                        return false;
                    }
                    let bytes =
                        std::slice::from_raw_parts(rendered as *const u8, libc::strlen(rendered));
                    (*r).headers_out.content_type = ngx_str_t {
                        len: bytes.len(),
                        data: rendered.cast(),
                    };
                    (*r).headers_out.content_type_len =
                        bytes.iter().position(|b| *b == b';').unwrap_or(bytes.len());
                    (*r).headers_out.content_type_lowcase = std::ptr::null_mut();
                }
                OutboundDisposition::Write {
                    name,
                    value,
                    shortcut,
                    set_psol_vary_accept_only,
                } => {
                    let pushed = ngx_list_push(&mut (*r).headers_out.headers);
                    if pushed.is_null() {
                        return false;
                    }
                    let key = pool_cstring((*r).pool, &name);
                    let val = pool_cstring((*r).pool, &value);
                    if key.is_null() || val.is_null() {
                        return false;
                    }
                    let header = &mut *pushed.cast::<ngx::ffi::ngx_table_elt_t>();
                    header.hash = 1;
                    header.key = ngx_str_t {
                        len: name.len(),
                        data: key.cast(),
                    };
                    header.value = ngx_str_t {
                        len: value.len(),
                        data: val.cast(),
                    };
                    if set_psol_vary_accept_only {
                        *set_psol_vary = true;
                    }
                    apply_outbound_shortcut(r, shortcut, header);
                }
            }
        }
        true
    }
}

/// Populates one outbound shortcut.
///
/// # Safety
///
/// `r` must point at a live request; `header` at its freshly
/// pushed entry.
unsafe fn apply_outbound_shortcut(
    r: *mut ngx::ffi::ngx_http_request_t,
    shortcut: Option<OutboundShortcut>,
    header: *mut ngx::ffi::ngx_table_elt_t,
) {
    // SAFETY: caller guarantees as documented.
    unsafe {
        match shortcut {
            None => {}
            Some(OutboundShortcut::Date) => (*r).headers_out.date = header,
            Some(OutboundShortcut::Etag) => (*r).headers_out.etag = header,
            Some(OutboundShortcut::Expires) => (*r).headers_out.expires = header,
            Some(OutboundShortcut::LastModified) => (*r).headers_out.last_modified = header,
            Some(OutboundShortcut::Location) => (*r).headers_out.location = header,
            Some(OutboundShortcut::Server) => (*r).headers_out.server = header,
            Some(OutboundShortcut::ContentLength { length }) => {
                (*r).headers_out.content_length_n = length;
                (*r).headers_out.content_length = header;
            }
            Some(OutboundShortcut::ContentEncoding) => {
                (*r).headers_out.content_encoding = header;
            }
            Some(OutboundShortcut::Refresh) => (*r).headers_out.refresh = header,
            Some(OutboundShortcut::ContentRange) => (*r).headers_out.content_range = header,
            Some(OutboundShortcut::AcceptRanges) => (*r).headers_out.accept_ranges = header,
            Some(OutboundShortcut::WwwAuthenticate) => {
                (*r).headers_out.www_authenticate = header;
            }
        }
    }
}

/// Builds a single-buffer output chain (`string_piece_to_buffer_chain`):
/// a temporary pool buffer NUL-terminated past its end, with the
/// `last_buf`/`flush` flags set. `None` on allocation failure.
///
/// # Safety
///
/// `pool` must point at a live pool.
pub unsafe fn string_piece_to_buffer_chain(
    pool: *mut ngx::ffi::ngx_pool_t,
    data: &[u8],
    send_last_buf: bool,
    send_flush: bool,
) -> Option<*mut ngx::ffi::ngx_chain_t> {
    use ngx::ffi::{ngx_alloc_chain_link, ngx_buf_t, ngx_palloc, ngx_pcalloc};
    // SAFETY: caller guarantees a live pool.
    unsafe {
        let buffer = ngx_pcalloc(pool, std::mem::size_of::<ngx_buf_t>()) as *mut ngx_buf_t;
        if buffer.is_null() {
            return None;
        }
        let start = ngx_palloc(pool, data.len() + 1) as *mut u8;
        if start.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), start, data.len());
        *start.add(data.len()) = 0;
        (*buffer).pos = start;
        (*buffer).last = start.add(data.len());
        (*buffer).set_last_buf(u32::from(send_last_buf));
        (*buffer).set_sync(0);
        (*buffer).set_flush(u32::from(send_flush));
        (*buffer).set_temporary(1);
        let link = ngx_alloc_chain_link(pool);
        if link.is_null() {
            return None;
        }
        (*link).buf = buffer;
        (*link).next = std::ptr::null_mut();
        Some(link)
    }
}

/// Reads hashed `Cache-Control` values from the output chain head
/// (`ps_get_cache_control`, live branch).
///
/// # Safety
///
/// `r` must point at a live request.
pub unsafe fn get_cache_control(r: *mut ngx::ffi::ngx_http_request_t) -> String {
    use ngx::ffi::ngx_table_elt_t;
    // SAFETY: caller guarantees a live request.
    unsafe {
        let mut values = Vec::new();
        let mut cc = (*r).headers_out.cache_control;
        while !cc.is_null() {
            let elt = &*cc.cast::<ngx_table_elt_t>();
            if elt.hash != 0 {
                let bytes = std::slice::from_raw_parts(elt.value.data, elt.value.len);
                values.push(String::from_utf8_lossy(bytes).into_owned());
            }
            cc = elt.next;
        }
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        join_cache_control(&refs)
    }
}

/// Writes the single `Cache-Control` line (`ps_set_cache_control`,
/// live branch): reuses the chain head (zeroing followers) or
/// pushes a new hashed entry. Returns false when the pool
/// allocation fails.
///
/// # Safety
///
/// `r` must point at a live request; `cache_control` must point
/// at a live NUL-terminated string (pool-owned, as produced by
/// `string_piece_to_pool_string`).
pub unsafe fn set_cache_control(
    r: *mut ngx::ffi::ngx_http_request_t,
    cache_control: *mut std::ffi::c_char,
) -> bool {
    use ngx::ffi::{ngx_list_push, ngx_str_t};
    // SAFETY: caller guarantees a live request and string.
    unsafe {
        let head = (*r).headers_out.cache_control;
        let cc = if head.is_null() {
            let pushed = ngx_list_push(&mut (*r).headers_out.headers);
            if pushed.is_null() {
                return false;
            }
            let elt = &mut *pushed.cast::<ngx::ffi::ngx_table_elt_t>();
            (*r).headers_out.cache_control = pushed.cast();
            elt.next = std::ptr::null_mut();
            elt.hash = 1;
            static KEY: &[u8; 14] = b"Cache-Control\0";
            elt.key = ngx_str_t {
                len: 13,
                data: KEY.as_ptr() as *mut u8,
            };
            elt
        } else {
            let mut follower = (*head.cast::<ngx::ffi::ngx_table_elt_t>()).next;
            while !follower.is_null() {
                (*follower.cast::<ngx::ffi::ngx_table_elt_t>()).hash = 0;
                follower = (*follower.cast::<ngx::ffi::ngx_table_elt_t>()).next;
            }
            let elt = &mut *head.cast::<ngx::ffi::ngx_table_elt_t>();
            elt.next = std::ptr::null_mut();
            elt
        };
        cc.value.len = libc::strlen(cache_control);
        cc.value.data = cache_control.cast();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_http::request::Method;
    use ps_http::response::HttpOptions;

    fn responses() -> ResponseHeaders {
        ResponseHeaders::new(HttpOptions::default_for_tests())
    }

    #[test]
    fn version_splits_on_thousands() {
        assert_eq!((1, 1), split_version(1001));
        assert_eq!((1, 0), split_version(1000));
        assert_eq!((2, 0), split_version(2000));
        assert_eq!((0, 9), split_version(9));
    }

    #[test]
    fn request_copy_skips_unhashed() {
        let mut out = RequestHeaders::new();
        copy_request_headers(
            1001,
            "post",
            &[
                ("Host", "h", true),
                ("X-Drop", "x", false),
                ("Cookie", "c", true),
            ],
            &mut out,
        );
        assert_eq!(1, out.major_version());
        assert_eq!(1, out.minor_version());
        assert_eq!(Method::Post, out.method());
        assert!(out.headers().has("Host"));
        assert!(!out.headers().has("X-Drop"));
        assert!(out.headers().has("Cookie"));
        // Unknown methods read back unknown.
        let mut out = RequestHeaders::new();
        copy_request_headers(1000, "BREW", &[], &mut out);
        assert_eq!(Method::Unknown, out.method());
        assert_eq!(1, out.major_version());
        assert_eq!(0, out.minor_version());
    }

    #[test]
    fn response_copy_adds_type_and_date() {
        let mut out = responses();
        copy_response_headers(
            1001,
            200,
            "text/html",
            &[("X-Keep", "1", true), ("X-Drop", "x", false)],
            1_700_000_000_000,
            &mut out,
        );
        assert_eq!(1, out.major_version());
        assert_eq!(1, out.minor_version());
        assert_eq!(200, out.status_code());
        assert!(out.headers().has("X-Keep"));
        assert!(!out.headers().has("X-Drop"));
        assert!(out.headers().has(header::CONTENT_TYPE));
        // No Date present: the injected clock supplies one.
        assert!(out.headers().has(header::DATE));
        // A present Date is left alone.
        let mut out = responses();
        copy_response_headers(
            1001,
            200,
            "text/html",
            &[("Date", "Thu, 01 Jan 1970 00:00:00 GMT", true)],
            0,
            &mut out,
        );
        assert_eq!(
            Some("Thu, 01 Jan 1970 00:00:00 GMT".to_string()),
            out.headers().lookup1(header::DATE)
        );
    }

    #[test]
    fn override_token_strips() {
        assert!(is_request_option_override_name(
            "ModPagespeedRequestOptionOverride"
        ));
        assert!(is_request_option_override_name(
            "pagespeedrequestoptionoverride"
        ));
        assert!(!is_request_option_override_name("X-Other"));
        assert!(!is_request_option_override_name("ModPagespeed"));
        assert!(strips_override_token(
            true,
            "PageSpeedRequestOptionOverride"
        ));
        assert!(!strips_override_token(
            false,
            "PageSpeedRequestOptionOverride"
        ));
        assert!(!strips_override_token(true, "Cache-Control"));
    }

    #[test]
    fn cache_control_joins() {
        assert_eq!("", join_cache_control(&[]));
        assert_eq!("a", join_cache_control(&["a"]));
        assert_eq!("a, b", join_cache_control(&["a", "b"]));
    }

    #[test]
    fn outbound_routing_matrix() {
        use crate::caching::PreserveCachingHeaders::{
            DontPreserve, PreserveAll, PreserveOnlyCacheControl,
        };
        use OutboundDisposition::{CacheControl, ContentType, Skip, Write};
        let route = |name: &str, value: &str| {
            route_outbound_header(name, value, DontPreserve, Some(9), false)
        };
        // Preservation gates first.
        assert_eq!(
            Skip,
            route_outbound_header("X-Custom", "v", PreserveOnlyCacheControl, None, false)
        );
        assert_eq!(
            CacheControl {
                value: "v".to_string()
            },
            route_outbound_header("Cache-Control", "v", PreserveOnlyCacheControl, None, false)
        );
        assert_eq!(
            Skip,
            route_outbound_header("Content-Length", "9", PreserveAll, Some(9), false)
        );
        // NUL bytes never emit.
        assert_eq!(Skip, route("X-Custom", "a\0b"));
        // Length needs a known non-negative whole plus all digits.
        assert!(matches!(route("Content-Length", "9"), Write { .. }));
        assert_eq!(Skip, route("Content-Length", "+9"));
        assert_eq!(Skip, route("Content-Length", "9 "));
        assert_eq!(
            Skip,
            route_outbound_header("Content-Length", "9", DontPreserve, None, false)
        );
        assert_eq!(
            Skip,
            route_outbound_header("Content-Length", "9", DontPreserve, Some(-1), false)
        );
        // Exact-case shortcuts.
        assert_eq!(
            CacheControl {
                value: "max-age=1".to_string()
            },
            route("Cache-Control", "max-age=1")
        );
        assert!(matches!(route("cache-control", "max-age=1"), Write { .. }));
        assert_eq!(
            ContentType {
                value: "text/html".to_string()
            },
            route("Content-Type", "text/html")
        );
        // Connection scaffolding always drops.
        assert_eq!(Skip, route("Connection", "keep-alive"));
        assert_eq!(Skip, route("Connection", "close"));
        assert_eq!(Skip, route("Keep-Alive", "timeout=5"));
        assert_eq!(Skip, route("Transfer-Encoding", "chunked"));
        // Weak etags rename; only etags with W/ values.
        assert_eq!(
            Write {
                name: "@psol-etag".to_string(),
                value: "W/\"1\"".to_string(),
                shortcut: None,
                set_psol_vary_accept_only: false,
            },
            route("ETag", "W/\"1\"")
        );
        assert_eq!(
            Write {
                name: "X-Foo".to_string(),
                value: "W/bar".to_string(),
                shortcut: None,
                set_psol_vary_accept_only: false,
            },
            route("X-Foo", "W/bar")
        );
        // Bare Vary flags the context; other varies write plainly.
        assert_eq!(
            Write {
                name: "Vary".to_string(),
                value: "Accept-Encoding".to_string(),
                shortcut: None,
                set_psol_vary_accept_only: true,
            },
            route("Vary", "Accept-Encoding")
        );
        assert_eq!(
            Write {
                name: "Vary".to_string(),
                value: "Cookie".to_string(),
                shortcut: None,
                set_psol_vary_accept_only: false,
            },
            route("Vary", "Cookie")
        );
        // Shortcuts populate by exact name.
        assert_eq!(
            Write {
                name: "Date".to_string(),
                value: "d".to_string(),
                shortcut: Some(OutboundShortcut::Date),
                set_psol_vary_accept_only: false,
            },
            route("Date", "d")
        );
        assert_eq!(
            Write {
                name: "Content-Length".to_string(),
                value: "9".to_string(),
                shortcut: Some(OutboundShortcut::ContentLength { length: 9 }),
                set_psol_vary_accept_only: false,
            },
            route("Content-Length", "9")
        );
        // Location mirrors only when the origin set one.
        let located = route_outbound_header("Location", "/x", DontPreserve, None, true);
        assert_eq!(
            Write {
                name: "Location".to_string(),
                value: "/x".to_string(),
                shortcut: Some(OutboundShortcut::Location),
                set_psol_vary_accept_only: false,
            },
            located
        );
        let unlocated = route_outbound_header("Location", "/x", DontPreserve, None, false);
        assert!(matches!(unlocated, Write { shortcut: None, .. }));
    }

    #[test]
    fn walker_covers_chained_parts() {
        use ngx::ffi::{ngx_list_part_t, ngx_list_t, ngx_str_t, ngx_table_elt_t};
        use std::mem::MaybeUninit;
        // SAFETY: all structures are test-owned; no nginx involved.
        unsafe {
            let mut names = [[b'a', 0], [b'b', 0], [b'c', 0]];
            let mut values = [[b'1', 0], [b'2', 0], [b'3', 0]];
            let elt = |name: *mut u8, value: *mut u8| ngx_table_elt_t {
                hash: 1,
                key: ngx_str_t { len: 1, data: name },
                value: ngx_str_t {
                    len: 1,
                    data: value,
                },
                lowcase_key: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
            };
            // Deliberately unhash the middle element: the walker
            // still visits it (callers filter).
            let mut first = [
                elt(names[0].as_mut_ptr(), values[0].as_mut_ptr()),
                elt(names[1].as_mut_ptr(), values[1].as_mut_ptr()),
            ];
            first[1].hash = 0;
            let mut second = [elt(names[2].as_mut_ptr(), values[2].as_mut_ptr())];
            let mut tail = ngx_list_part_t {
                elts: second.as_mut_ptr() as *mut _,
                nelts: 1,
                next: std::ptr::null_mut(),
            };
            let head = ngx_list_part_t {
                elts: first.as_mut_ptr() as *mut _,
                nelts: 2,
                next: &mut tail,
            };
            let mut list: ngx_list_t = MaybeUninit::zeroed().assume_init();
            list.part = head;
            // Full walk visits all three in order.
            let mut seen = Vec::new();
            for_each_header(&mut list, |elt| {
                seen.push((*elt).key.len);
                seen.push((*elt).value.len);
                true
            });
            assert_eq!(vec![1, 1, 1, 1, 1, 1], seen);
            // Early stop after the first element.
            let mut count = 0;
            for_each_header(&mut list, |_| {
                count += 1;
                false
            });
            assert_eq!(1, count);
            // Zero-length head part falls through to the tail.
            list.part.nelts = 0;
            let mut count = 0;
            for_each_header(&mut list, |_| {
                count += 1;
                true
            });
            assert_eq!(1, count);
        }
    }
}
