//! Fetch traits and inflating fetches (`async_fetch.{h,cc}`,
//! `url_async_fetcher.h`, `inflating_fetch.{h,cc}`).
//!
//! The C++ fetch flow is callback-driven (`HeadersComplete` → `Write`
//! → `Flush` → `Done`); this port keeps the same staged trait surface
//! so fetcher implementations (mock, serf, native) plug in unchanged,
//! while cache code drives fetches to completion synchronously.
//! Gzip layer add/remove helpers mirror `InflatingFetch` exactly,
//! including the `Vary: Accept-Encoding` and
//! `X-Original-Content-Length` bookkeeping.
#![forbid(unsafe_code)]

use crate::names::header;
use crate::request::{RequestContext, RequestHeaders};
use crate::response::ResponseHeaders;

/// Unspecified fetch timeout (`UrlAsyncFetcher::kUnspecifiedTimeoutMs`).
pub const UNSPECIFIED_TIMEOUT_MS: i64 = -1;

/// Maximum HTML page size for inflation (`kHtmlPageLimitBytes` = 512KB).
pub const HTML_PAGE_LIMIT_BYTES: usize = 512 * 1024;

/// In-flight fetch (`AsyncFetch`).
///
/// Staged flow: the fetcher fills request/response headers, calls
/// `headers_complete()`, streams `write()` chunks, then `done(success)`.
/// `extra_response_headers` contribute additional headers (e.g. fetcher
/// diagnostics) without disturbing the origin set.
pub trait AsyncFetch: Send {
    /// Fetch URL.
    fn url(&self) -> &str;
    /// Borrow request headers.
    fn request_headers(&self) -> &RequestHeaders;
    /// Mutably borrow request headers.
    fn request_headers_mut(&mut self) -> &mut RequestHeaders;
    /// Borrow response headers.
    fn response_headers(&self) -> &ResponseHeaders;
    /// Mutably borrow response headers.
    fn response_headers_mut(&mut self) -> &mut ResponseHeaders;
    /// Extra headers merged on completion.
    fn extra_response_headers(&self) -> &ResponseHeaders;
    /// Mutably borrow extra headers.
    fn extra_response_headers_mut(&mut self) -> &mut ResponseHeaders;
    /// Request context.
    fn request_context(&self) -> &RequestContext;
    /// Buffer for the response body (`response_writer` equivalent).
    fn body_writer(&mut self) -> &mut Vec<u8>;
    /// Called when headers complete (`HeadersComplete`).
    fn headers_complete(&mut self) {}
    /// Streams a body chunk (`HandleWrite`, returns false to abort).
    fn handle_write(&mut self, bytes: &[u8]) -> bool {
        self.body_writer().extend_from_slice(bytes);
        true
    }
    /// Flushes buffered output (`HandleFlush`).
    fn handle_flush(&mut self) {}
    /// Completes the fetch (`HandleDone`).
    fn handle_done(&mut self, _success: bool) {}
}

/// Buffering fetch used by tests and the cache (`StringAsyncFetch`-like).
#[derive(Debug)]
pub struct BufferingFetch {
    url: String,
    request_headers: RequestHeaders,
    response_headers: ResponseHeaders,
    extra_response_headers: ResponseHeaders,
    request_context: RequestContext,
    body: Vec<u8>,
    headers_complete: bool,
    done: bool,
    success: bool,
}

impl BufferingFetch {
    /// Creates a fetch for a URL with default options.
    #[must_use]
    pub fn new(url: &str, request_context: RequestContext) -> BufferingFetch {
        let options = request_context.options();
        BufferingFetch {
            url: url.to_string(),
            request_headers: RequestHeaders::new(),
            response_headers: ResponseHeaders::new(options),
            extra_response_headers: ResponseHeaders::new(options),
            request_context,
            body: Vec::new(),
            headers_complete: false,
            done: false,
            success: false,
        }
    }

    /// Response body collected so far.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Whether headers completed.
    #[must_use]
    pub fn headers_complete(&self) -> bool {
        self.headers_complete
    }

    /// Whether the fetch finished.
    #[must_use]
    pub fn done(&self) -> bool {
        self.done
    }

    /// Completion status (valid once done).
    #[must_use]
    pub fn success(&self) -> bool {
        self.success
    }
}

impl AsyncFetch for BufferingFetch {
    fn url(&self) -> &str {
        &self.url
    }

    fn request_headers(&self) -> &RequestHeaders {
        &self.request_headers
    }

    fn request_headers_mut(&mut self) -> &mut RequestHeaders {
        &mut self.request_headers
    }

    fn response_headers(&self) -> &ResponseHeaders {
        &self.response_headers
    }

    fn response_headers_mut(&mut self) -> &mut ResponseHeaders {
        &mut self.response_headers
    }

    fn extra_response_headers(&self) -> &ResponseHeaders {
        &self.extra_response_headers
    }

    fn extra_response_headers_mut(&mut self) -> &mut ResponseHeaders {
        &mut self.extra_response_headers
    }

    fn request_context(&self) -> &RequestContext {
        &self.request_context
    }

    fn body_writer(&mut self) -> &mut Vec<u8> {
        &mut self.body
    }

    fn headers_complete(&mut self) {
        self.headers_complete = true;
    }

    fn handle_done(&mut self, success: bool) {
        self.done = true;
        self.success = success;
    }
}

/// URL fetcher (`UrlAsyncFetcher`).
pub trait AsyncFetcher: Send + Sync {
    /// Fetches a URL into `fetch`, driving it to `Done`
    /// (`Fetch(url, handler, fetch)`; handlers fold into `Result` here).
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch);

    /// Fetch with an explicit timeout (`FetchWithGzip` uses this shape).
    fn fetch_with_timeout(&self, url: &str, fetch: &mut dyn AsyncFetch, _timeout_ms: i64) {
        self.fetch(url, fetch);
    }

    /// Whether HTTPS URLs are supported (`SupportsHttps`).
    fn supports_https(&self) -> bool {
        true
    }

    /// Fetcher timeout (`timeout_ms`, -1 when unspecified).
    fn timeout_ms(&self) -> i64 {
        UNSPECIFIED_TIMEOUT_MS
    }

    /// Shuts the fetcher down (`ShutDown`).
    fn shutdown(&self) {}
}

/// Inflating fetch wrapper (`InflatingFetch`).
///
/// Sits between a fetcher and a base fetch: ungzips gzipped bodies on
/// the fly when the origin did not send `Vary: Accept-Encoding`
/// (adding it), tracking the original length in
/// `X-Original-Content-Length`. Bodies over the limit fail ungzipping
/// and pass through untouched.
pub struct InflatingFetch<'a> {
    base: &'a mut dyn AsyncFetch,
    inflated: Vec<u8>,
    request_checked_for_accept_encoding: bool,
    compression_desired: bool,
}

impl<'a> InflatingFetch<'a> {
    /// Wraps a base fetch.
    pub fn new(base: &'a mut dyn AsyncFetch) -> InflatingFetch<'a> {
        InflatingFetch {
            base,
            inflated: Vec::new(),
            request_checked_for_accept_encoding: false,
            compression_desired: false,
        }
    }

    /// Requests gzipped content from the origin
    /// (`EnableGzipFromBackend`): adds `Accept-Encoding: gzip`
    /// unless gzip or deflate is already allowed.
    pub fn enable_gzip_from_backend(&mut self) {
        if !self.is_compression_allowed_in_request() {
            self.base
                .request_headers_mut()
                .headers_mut()
                .add(header::ACCEPT_ENCODING, header::GZIP);
        }
    }

    /// Whether the request already allows a compressed response
    /// (computed once, then latched).
    fn is_compression_allowed_in_request(&mut self) -> bool {
        if !self.request_checked_for_accept_encoding {
            self.request_checked_for_accept_encoding = true;
            if let Some(values) = self
                .base
                .request_headers()
                .headers()
                .lookup(header::ACCEPT_ENCODING)
            {
                for value in values {
                    if value.eq_ignore_ascii_case(header::GZIP)
                        || value.eq_ignore_ascii_case(header::DEFLATE)
                    {
                        self.compression_desired = true;
                        break;
                    }
                }
            }
        }
        self.compression_desired
    }
}

impl AsyncFetch for InflatingFetch<'_> {
    fn url(&self) -> &str {
        self.base.url()
    }

    fn request_headers(&self) -> &RequestHeaders {
        self.base.request_headers()
    }

    fn request_headers_mut(&mut self) -> &mut RequestHeaders {
        self.base.request_headers_mut()
    }

    fn response_headers(&self) -> &ResponseHeaders {
        self.base.response_headers()
    }

    fn response_headers_mut(&mut self) -> &mut ResponseHeaders {
        self.base.response_headers_mut()
    }

    fn extra_response_headers(&self) -> &ResponseHeaders {
        self.base.extra_response_headers()
    }

    fn extra_response_headers_mut(&mut self) -> &mut ResponseHeaders {
        self.base.extra_response_headers_mut()
    }

    fn request_context(&self) -> &RequestContext {
        self.base.request_context()
    }

    fn body_writer(&mut self) -> &mut Vec<u8> {
        &mut self.inflated
    }

    fn headers_complete(&mut self) {
        // Ungzip decision happens at Done time (needs the full body).
        self.base.headers_complete();
    }

    fn handle_done(&mut self, success: bool) {
        // Attempt ungzip; on any failure the raw bytes pass through.
        let mut raw = std::mem::take(&mut self.inflated);
        let mut headers = self.base.response_headers().clone();
        if success && ungzip_value_if_compressed(&mut headers, &mut raw) {
            *self.base.response_headers_mut() = headers;
        }
        if !raw.is_empty() {
            self.base.handle_write(&raw);
        }
        self.base.handle_done(success);
    }
}

/// Maps a zlib compression level to flate2 (`-1` = default).
fn compression_for_level(level: i32) -> flate2::Compression {
    use flate2::Compression;
    if level < 0 {
        Compression::default()
    } else {
        Compression::new(level.clamp(0, 9) as u32)
    }
}

/// Adds a gzip layer (`InflatingFetch::GzipValue`).
///
/// Refuses (false) when already gzipped. Otherwise records the original
/// length (overwriting any marker), drops `Transfer-Encoding`, adds a
/// `Content-Encoding: gzip` line, sets Content-Length, and ensures
/// `Vary: Accept-Encoding`.
#[must_use]
pub fn gzip_value(headers: &mut ResponseHeaders, body: &mut Vec<u8>, level: i32) -> bool {
    use flate2::write::GzEncoder;
    use std::io::Write;
    if headers.is_gzipped() {
        return false;
    }
    let mut encoder = GzEncoder::new(Vec::new(), compression_for_level(level));
    let Ok(()) = encoder.write_all(body) else {
        return false;
    };
    let Ok(compressed) = encoder.finish() else {
        return false;
    };
    if !headers
        .headers()
        .has_value(header::VARY, header::ACCEPT_ENCODING)
    {
        headers
            .headers_mut()
            .add(header::VARY, header::ACCEPT_ENCODING);
    }
    let content_length = headers.find_content_length().unwrap_or(body.len() as i64);
    headers.headers_mut().remove_all(header::TRANSFER_ENCODING);
    headers.headers_mut().replace(
        header::X_ORIGINAL_CONTENT_LENGTH,
        &content_length.to_string(),
    );
    headers.headers_mut().add(header::CONTENT_ENCODING, "gzip");
    headers
        .headers_mut()
        .replace(header::CONTENT_LENGTH, &compressed.len().to_string());
    *body = compressed;
    true
}

/// Removes a gzip layer when present (`UnGzipValueIfCompressed`).
///
/// Requires a non-empty body whose first coding is exactly `gzip`;
/// inflation beyond `HTML_PAGE_LIMIT_BYTES` fails like corruption.
/// Headers and body stay untouched on failure.
#[must_use]
pub fn ungzip_value_if_compressed(headers: &mut ResponseHeaders, body: &mut Vec<u8>) -> bool {
    use flate2::bufread::GzDecoder;
    use std::io::Read;
    if body.is_empty() || !headers.is_gzipped() {
        return false;
    }
    let mut decoder = GzDecoder::new(body.as_slice());
    let mut inflated = Vec::new();
    let mut limited = (&mut decoder).take(HTML_PAGE_LIMIT_BYTES as u64 + 1);
    if limited.read_to_end(&mut inflated).is_err() || inflated.len() > HTML_PAGE_LIMIT_BYTES {
        return false;
    }
    *body = inflated;
    if !headers
        .headers()
        .has_value(header::VARY, header::ACCEPT_ENCODING)
    {
        headers
            .headers_mut()
            .add(header::VARY, header::ACCEPT_ENCODING);
    }
    headers.headers_mut().remove_all(header::TRANSFER_ENCODING);
    headers
        .headers_mut()
        .remove(header::CONTENT_ENCODING, header::GZIP);
    headers
        .headers_mut()
        .replace(header::CONTENT_LENGTH, &body.len().to_string());
    true
}

/// Inflates gzip bytes within a cap (`GzipInflater::InflateWithLimit`):
/// corruption or overflow past `max_bytes` fails instead of
/// inflating without bound.
#[must_use]
pub fn inflate_gzip_with_limit(body: &[u8], max_bytes: u64) -> Option<Vec<u8>> {
    use flate2::bufread::GzDecoder;
    use std::io::Read;
    let mut decoder = GzDecoder::new(body);
    let mut inflated = Vec::new();
    let mut limited = (&mut decoder).take(max_bytes.saturating_add(1));
    if limited.read_to_end(&mut inflated).is_err() {
        return None;
    }
    if inflated.len() as u64 > max_bytes {
        return None;
    }
    Some(inflated)
}

/// Adds `public` for Google-CDN caching
/// (`AsyncFetch::FixCacheControlForGoogleCache`): when a `Via`
/// value matches `X.Y google`, merges `public` into Cache-Control
/// unless a visibility directive already governs.
pub fn fix_cache_control_for_google_cache(
    request_headers: &RequestHeaders,
    response_headers: &mut ResponseHeaders,
) {
    let Some(values) = request_headers.headers().lookup(header::VIA) else {
        return;
    };
    if !values.iter().any(|value| is_google_cache_via(value)) {
        return;
    }
    let current = response_headers
        .headers()
        .lookup(header::CACHE_CONTROL)
        .unwrap_or_default();
    if current.iter().any(|value| {
        ["private", "public", "no-cache", "no-store"]
            .iter()
            .any(|directive| value.eq_ignore_ascii_case(directive))
    }) {
        return;
    }
    let mut merged = current.join(", ");
    if merged.is_empty() {
        merged.push_str("public");
    } else {
        merged.push_str(", public");
    }
    response_headers
        .headers_mut()
        .replace(header::CACHE_CONTROL, &merged);
}

/// Whether a Via value names the Google cache (`X.Y google`,
/// case-insensitive host, numeric version).
fn is_google_cache_via(value: &str) -> bool {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    tokens.len() == 2
        && tokens[1].eq_ignore_ascii_case("google")
        && tokens[0].parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::RequestContext;
    use crate::response::HttpOptions;

    fn fetch_at(url: &str) -> BufferingFetch {
        BufferingFetch::new(url, RequestContext::new(HttpOptions::default_for_tests()))
    }

    /// Splits a fetch into owned headers + body for helper-level tests.
    fn split_fetch(fetch: &BufferingFetch) -> (ResponseHeaders, Vec<u8>) {
        (fetch.response_headers().clone(), fetch.body().to_vec())
    }

    #[test]
    fn gzip_round_trip_and_bookkeeping() {
        let mut fetch = fetch_at("http://x/a.css");
        let body = b"body { color: red; }".repeat(20);
        fetch.handle_write(&body);
        let (mut headers, mut stored) = split_fetch(&fetch);
        assert!(gzip_value(&mut headers, &mut stored, 6));
        assert!(stored.len() < body.len());
        assert!(headers.is_gzipped());
        assert_eq!(
            headers.headers().lookup1(header::X_ORIGINAL_CONTENT_LENGTH),
            Some(body.len().to_string())
        );
        assert!(headers
            .headers()
            .lookup(header::VARY)
            .is_some_and(|values| values.contains(&"Accept-Encoding".to_string())));
        // Second gzip refuses (already gzipped).
        assert!(!gzip_value(&mut headers, &mut stored, 6));
        // Ungzip restores bytes and headers.
        assert!(ungzip_value_if_compressed(&mut headers, &mut stored));
        assert_eq!(stored, body);
        assert!(!headers.is_gzipped());
        assert_eq!(headers.find_content_length(), Some(body.len() as i64));
        // The original-length marker survives ungzipping (C++ keeps it).
        assert_eq!(
            headers.headers().lookup1(header::X_ORIGINAL_CONTENT_LENGTH),
            Some(body.len().to_string())
        );
    }

    #[test]
    fn ungzip_refusals() {
        use crate::response::ResponseHeaders;
        // Not gzipped: untouched.
        let mut headers = ResponseHeaders::new(crate::response::HttpOptions::default_for_tests());
        let mut body = b"plain".to_vec();
        assert!(!ungzip_value_if_compressed(&mut headers, &mut body));
        assert_eq!(body, b"plain");
        // Corrupt gzip: untouched (valid gzip magic, truncated stream).
        headers.headers_mut().add(header::CONTENT_ENCODING, "gzip");
        body = b"\x1f\x8b\x08\x00truncated".to_vec();
        assert!(!ungzip_value_if_compressed(&mut headers, &mut body));
    }

    #[test]
    fn google_cache_via_merges_public() {
        use crate::request::RequestHeaders;
        let mut request = RequestHeaders::new();
        request.headers_mut().add(header::VIA, "1.1 google");
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response
            .headers_mut()
            .add(header::CACHE_CONTROL, "max-age=300");
        fix_cache_control_for_google_cache(&request, &mut response);
        assert_eq!(
            response.headers().lookup(header::CACHE_CONTROL),
            Some(vec!["max-age=300".to_string(), "public".to_string()])
        );
        // Governed directives win over the merge.
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.headers_mut().add(header::CACHE_CONTROL, "private");
        fix_cache_control_for_google_cache(&request, &mut response);
        assert_eq!(
            response.headers().lookup(header::CACHE_CONTROL),
            Some(vec!["private".to_string()])
        );
        // Non-google Via values leave headers alone.
        let mut plain = RequestHeaders::new();
        plain.headers_mut().add(header::VIA, "1.1 example");
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response
            .headers_mut()
            .add(header::CACHE_CONTROL, "max-age=300");
        fix_cache_control_for_google_cache(&plain, &mut response);
        assert_eq!(
            response.headers().lookup(header::CACHE_CONTROL),
            Some(vec!["max-age=300".to_string()])
        );
    }

    #[test]
    fn inflating_wrapper_flow() {
        let mut base = fetch_at("http://x/a.css");
        {
            let mut wrapper = InflatingFetch::new(&mut base);
            wrapper
                .response_headers_mut()
                .set_first_line(1, 1, 200, "OK");
            // Simulate a gzipped fetch through the wrapper.
            let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
            let mut body = b"body { color: red; }".repeat(20);
            assert!(gzip_value(&mut headers, &mut body, 6));
            *wrapper.response_headers_mut() = headers;
            wrapper.headers_complete();
            wrapper.handle_write(&body);
            wrapper.handle_done(true);
        }
        assert!(base.done() && base.success());
        assert_eq!(base.body(), b"body { color: red; }".repeat(20).as_slice());
        assert!(!base.response_headers().is_gzipped());
    }

    #[test]
    fn gzip_from_backend_adds_accept_encoding_once() {
        let mut base = fetch_at("http://x/a.css");
        {
            let mut wrapper = InflatingFetch::new(&mut base);
            wrapper.enable_gzip_from_backend();
        }
        assert_eq!(
            base.request_headers()
                .headers()
                .lookup(header::ACCEPT_ENCODING),
            Some(vec!["gzip".to_string()])
        );
        // The latch caches the original answer: two calls on a
        // fresh fetch add twice (unreachable in practice; called
        // once per fetch).
        let mut base = fetch_at("http://x/a.css");
        {
            let mut wrapper = InflatingFetch::new(&mut base);
            wrapper.enable_gzip_from_backend();
            wrapper.enable_gzip_from_backend();
        }
        assert_eq!(
            base.request_headers()
                .headers()
                .lookup(header::ACCEPT_ENCODING),
            Some(vec!["gzip".to_string(), "gzip".to_string()])
        );
        // Pre-existing deflate disables the add (case-insensitive).
        let mut base = fetch_at("http://x/a.css");
        base.request_headers_mut()
            .headers_mut()
            .add(header::ACCEPT_ENCODING, "Deflate");
        {
            let mut wrapper = InflatingFetch::new(&mut base);
            wrapper.enable_gzip_from_backend();
        }
        assert_eq!(
            base.request_headers()
                .headers()
                .lookup(header::ACCEPT_ENCODING),
            Some(vec!["Deflate".to_string()])
        );
    }
}
