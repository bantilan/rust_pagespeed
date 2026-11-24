//! Request headers and request context (minimal Step 1 surface).
//!
//! Ports `RequestHeaders` (method + properties derivation) and the
//! `RequestContext` subset the cache and fetchers consult (options,
//! gzip acceptance, fetch timing). Full logging/tracing/userdata lands
//! with the server integration.
#![forbid(unsafe_code)]

use crate::headers::Headers;
use crate::names::header;
use crate::response::{HttpOptions, RequestProperties};
use ps_config::options::{QueryStatus, RewriteOptions};

/// HTTP request methods (`RequestHeaders::Method` plus the purge
/// method PageSpeed issues downstream; `Unknown` covers both unset
/// and the C++ `kError` for unrecognized tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Method {
    /// Unset or unrecognized.
    #[default]
    Unknown,
    /// `GET`.
    Get,
    /// `HEAD`.
    Head,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `DELETE`.
    Delete,
    /// `OPTIONS`.
    Options,
    /// `TRACE`.
    Trace,
    /// `CONNECT`.
    Connect,
    /// `PATCH`.
    Patch,
    /// Downstream purge method.
    Purge,
}

impl Method {
    /// Parses a method token (case-sensitive, as compared in C++).
    #[must_use]
    pub fn parse(token: &str) -> Method {
        match token {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            "TRACE" => Method::Trace,
            "CONNECT" => Method::Connect,
            "PATCH" => Method::Patch,
            "PURGE" => Method::Purge,
            _ => Method::Unknown,
        }
    }

    /// Renders the method token (`method_string`): `Unknown`
    /// renders `"ERROR"`, verbatim.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Connect => "CONNECT",
            Method::Patch => "PATCH",
            Method::Purge => "PURGE",
            Method::Unknown => "ERROR",
        }
    }

    /// Parses a method name (`MethodFromString`): the ten known
    /// tokens matched case-insensitively, anything else `Unknown`.
    #[must_use]
    pub fn from_string(name: &str) -> Method {
        if name.eq_ignore_ascii_case("GET") {
            Method::Get
        } else if name.eq_ignore_ascii_case("HEAD") {
            Method::Head
        } else if name.eq_ignore_ascii_case("POST") {
            Method::Post
        } else if name.eq_ignore_ascii_case("PUT") {
            Method::Put
        } else if name.eq_ignore_ascii_case("DELETE") {
            Method::Delete
        } else if name.eq_ignore_ascii_case("OPTIONS") {
            Method::Options
        } else if name.eq_ignore_ascii_case("TRACE") {
            Method::Trace
        } else if name.eq_ignore_ascii_case("CONNECT") {
            Method::Connect
        } else if name.eq_ignore_ascii_case("PATCH") {
            Method::Patch
        } else if name.eq_ignore_ascii_case("PURGE") {
            Method::Purge
        } else {
            Method::Unknown
        }
    }

    /// Canonical token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Method::Unknown => "",
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Connect => "CONNECT",
            Method::Patch => "PATCH",
            Method::Purge => "PURGE",
        }
    }

    /// HEAD distinct from POST (the enum keeps both spellings).
    #[must_use]
    pub const fn is_head(self) -> bool {
        matches!(self, Method::Head)
    }
}

/// Request headers (`RequestHeaders`): method plus the shared list.
#[derive(Debug, Clone, Default)]
pub struct RequestHeaders {
    headers: Headers,
    method: Method,
    major_version: Option<i32>,
    minor_version: Option<i32>,
}

impl RequestHeaders {
    /// Creates empty request headers.
    #[must_use]
    pub fn new() -> RequestHeaders {
        RequestHeaders::default()
    }

    /// Borrows the header list.
    #[must_use]
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// Mutably borrows the header list.
    pub fn headers_mut(&mut self) -> &mut Headers {
        &mut self.headers
    }

    /// Request method.
    #[must_use]
    pub fn method(&self) -> Method {
        self.method
    }

    /// Sets the method.
    pub fn set_method(&mut self, method: Method) {
        self.method = method;
    }

    /// Major version, 1 when unset.
    #[must_use]
    pub fn major_version(&self) -> i32 {
        self.major_version.unwrap_or(1)
    }

    /// Minor version, 0 when unset.
    #[must_use]
    pub fn minor_version(&self) -> i32 {
        self.minor_version.unwrap_or(0)
    }

    /// Sets major/minor versions.
    pub fn set_version(&mut self, major: i32, minor: i32) {
        self.major_version = Some(major);
        self.minor_version = Some(minor);
    }

    /// Serializes as an HTTP request with an empty URL (`ToString`):
    /// `METHOD  HTTP/x.y` plus `Name: Value` lines in order, CRLF
    /// terminated with a trailing blank line. The HTML cache hashes
    /// this into its identity string.
    #[must_use]
    pub fn to_http_string(&self) -> String {
        let mut out = format!(
            "{}  HTTP/{}.{}\r\n",
            self.method.token(),
            self.major_version(),
            self.minor_version()
        );
        for (name, value) in self.headers.iter() {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out
    }

    /// Serializes like [`RequestHeaders::to_http_string`] but drops
    /// `Cookie`/`Cookie2` lines. The HTML cache hashes this into its
    /// identity string so allowlisted cookie values (per-client
    /// analytics IDs) never split cache keys; requests that reach
    /// key generation hold only ignored cookies, so this renders
    /// byte-identical output for cookie-free requests.
    #[must_use]
    pub fn to_http_string_without_cookies(&self) -> String {
        let mut out = format!(
            "{}  HTTP/{}.{}\r\n",
            self.method.token(),
            self.major_version(),
            self.minor_version()
        );
        for (name, value) in self.headers.iter() {
            if name.eq_ignore_ascii_case(header::COOKIE) || name.eq_ignore_ascii_case(header::COOKIE2)
            {
                continue;
            }
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        out
    }

    /// Derives caching properties (`GetProperties`).
    #[must_use]
    pub fn properties(&self) -> RequestProperties {
        RequestProperties {
            has_cookie: self.headers.has(header::COOKIE),
            has_cookie2: self.headers.has(header::COOKIE2),
            has_authorization: self.headers.has(header::AUTHORIZATION),
        }
    }

    /// Whether the request accepts gzip (`AcceptsGzip`).
    #[must_use]
    pub fn accepts_gzip(&self) -> bool {
        self.headers
            .lookup(header::ACCEPT_ENCODING)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case("gzip"))
            })
    }

    /// Whether the request accepts zstd (`compress_zstd`
    /// negotiation): proper token parse with quality values,
    /// unlike the whole-token gzip check above.
    #[must_use]
    pub fn accepts_zstd(&self) -> bool {
        if !self.headers.has(header::ACCEPT_ENCODING) {
            return false;
        }
        crate::zstd::client_accepts_zstd(Some(&self.headers.lookup_joined(header::ACCEPT_ENCODING)))
    }

    /// Whether the request is an Ajax call (`IsXmlHttpRequest`):
    /// `X-Requested-With: XMLHttpRequest`, case-insensitively.
    #[must_use]
    pub fn is_xml_http_request(&self) -> bool {
        self.headers
            .lookup_first(header::X_REQUESTED_WITH)
            .is_some_and(|value| value.eq_ignore_ascii_case(crate::names::XML_HTTP_REQUEST))
    }

    /// All cookies (`GetAllCookies`): every `Cookie` line split on
    /// `;` (empties dropped), each pair split on the first `=`
    /// with both sides trimmed, sorted by name like the
    /// `CookieMultimap` (stable for repeats).
    #[must_use]
    pub fn get_all_cookies(&self) -> Vec<(String, String)> {
        let mut cookies = Vec::new();
        for (name, value) in self.headers.iter() {
            if !name.eq_ignore_ascii_case(header::COOKIE) {
                continue;
            }
            for pair in value.split(';').filter(|piece| !piece.is_empty()) {
                let (cookie_name, cookie_value) = match pair.find('=') {
                    Some(pos) => (pair[..pos].trim(), pair[pos + 1..].trim()),
                    None => (pair.trim(), ""),
                };
                cookies.push((cookie_name.to_string(), cookie_value.to_string()));
            }
        }
        cookies.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        cookies
    }
}

/// Scans request headers for PageSpeed options (`ScanHeader`).
///
/// Request headers carry options exactly like query parameters;
/// `allow_options` gates everything after the bare `PageSpeed=`
/// switch unless a `*RequestOptionOverride` header matches the
/// token read off the options object. Consumed
/// `ModPagespeed*`/`PageSpeed*` headers and any
/// `X-PSA-Client-Options` header are removed.
#[must_use]
pub fn scan_headers_for_options(
    headers: &mut RequestHeaders,
    allow_options: bool,
    options: &mut RewriteOptions,
) -> QueryStatus {
    let token = options.request_option_override();
    let mut context: Option<&mut RequestContext> = None;
    crate::query::scan_header_list(
        headers.headers_mut(),
        allow_options,
        &token,
        &mut context,
        options,
        false,
    )
}

/// Fetch timing record (the `TimingInfo` subset HTTPCache writes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchTiming {
    /// Cache-lookup latency in ms.
    pub cache_lookup_ms: i64,
    /// Backend fetch latency in ms.
    pub fetch_ms: i64,
}

/// Minimal request context (`RequestContext` subset).
#[derive(Debug, Clone)]
pub struct RequestContext {
    options: HttpOptions,
    timing: FetchTiming,
    accepts_gzip: bool,
    session_authorized_origins: std::collections::HashSet<String>,
    sticky_query_parameters: Option<String>,
}

impl RequestContext {
    /// Creates a context with options.
    #[must_use]
    pub fn new(options: HttpOptions) -> RequestContext {
        RequestContext {
            options,
            timing: FetchTiming::default(),
            accepts_gzip: false,
            session_authorized_origins: std::collections::HashSet::new(),
            sticky_query_parameters: None,
        }
    }

    /// Authorizes an origin for this session
    /// (`AddSessionAuthorizedFetchOrigin`).
    pub fn add_session_authorized_fetch_origin(&mut self, origin: &str) {
        self.session_authorized_origins.insert(origin.to_string());
    }

    /// Whether an origin is session-authorized
    /// (`IsSessionAuthorizedFetchOrigin`).
    #[must_use]
    pub fn is_session_authorized_fetch_origin(&self, origin: &str) -> bool {
        self.session_authorized_origins.contains(origin)
    }

    /// Request options.
    #[must_use]
    pub const fn options(&self) -> HttpOptions {
        self.options
    }

    /// Replaces the request options (`set_options`): the
    /// resource path computes these after option determination.
    pub fn set_options(&mut self, options: HttpOptions) {
        self.options = options;
    }

    /// Whether the client accepts gzip (`accepts_gzip`).
    #[must_use]
    pub const fn accepts_gzip(&self) -> bool {
        self.accepts_gzip
    }

    /// Sets gzip acceptance.
    pub fn set_accepts_gzip(&mut self, accepts: bool) {
        self.accepts_gzip = accepts;
    }

    /// Records cache-lookup latency.
    pub fn set_cache_lookup_ms(&mut self, ms: i64) {
        self.timing.cache_lookup_ms = ms;
    }

    /// Records backend latency.
    pub fn set_fetch_ms(&mut self, ms: i64) {
        self.timing.fetch_ms = ms;
    }

    /// Current timing record.
    #[must_use]
    pub const fn timing(&self) -> FetchTiming {
        self.timing
    }

    /// Records the sticky query-parameters token
    /// (`set_sticky_query_parameters_token`).
    pub fn set_sticky_query_parameters_token(&mut self, token: &str) {
        self.sticky_query_parameters = Some(token.to_string());
    }

    /// The sticky query-parameters token, if set.
    #[must_use]
    pub fn sticky_query_parameters_token(&self) -> Option<&str> {
        self.sticky_query_parameters.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_config::filter::Filter;

    #[test]
    fn method_from_string_covers_table() {
        for (token, method) in [
            ("GET", Method::Get),
            ("HEAD", Method::Head),
            ("POST", Method::Post),
            ("PUT", Method::Put),
            ("DELETE", Method::Delete),
            ("OPTIONS", Method::Options),
            ("TRACE", Method::Trace),
            ("CONNECT", Method::Connect),
            ("PATCH", Method::Patch),
            ("PURGE", Method::Purge),
        ] {
            assert_eq!(method, Method::from_string(token));
            assert_eq!(method, Method::from_string(&token.to_ascii_lowercase()));
            assert_eq!(token, method.token());
        }
        assert_eq!(Method::Unknown, Method::from_string("BREW"));
        assert_eq!(Method::Unknown, Method::from_string(""));
        let mut headers = RequestHeaders::new();
        assert_eq!(1, headers.major_version());
        assert_eq!(0, headers.minor_version());
        headers.set_version(1, 1);
        assert_eq!(1, headers.major_version());
        assert_eq!(1, headers.minor_version());
    }

    #[test]
    fn xhr_cookies_and_sticky() {
        let mut headers = RequestHeaders::new();
        assert!(!headers.is_xml_http_request());
        headers
            .headers_mut()
            .add("X-Requested-With", "xmlhttprequest");
        assert!(headers.is_xml_http_request());
        headers.headers_mut().add("Cookie", "b=2; a=1");
        headers.headers_mut().add("cookie", "a=3; novalue; c=");
        let cookies = headers.get_all_cookies();
        // Sorted by name (stable for repeats), values trimmed.
        assert_eq!(
            cookies,
            vec![
                ("a".to_string(), "1".to_string()),
                ("a".to_string(), "3".to_string()),
                ("b".to_string(), "2".to_string()),
                ("c".to_string(), "".to_string()),
                ("novalue".to_string(), "".to_string()),
            ]
        );
        let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
        assert_eq!(None, ctx.sticky_query_parameters_token());
        ctx.set_sticky_query_parameters_token("tok");
        assert_eq!(Some("tok"), ctx.sticky_query_parameters_token());
    }

    #[test]
    fn header_scan_consumes_option_headers() {
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("PageSpeedFilters", "rewrite_css");
        headers.headers_mut().add("X-Unrelated", "keep");
        let mut options = RewriteOptions::new();
        assert_eq!(
            QueryStatus::Success,
            scan_headers_for_options(&mut headers, true, &mut options)
        );
        assert!(options.enabled(Filter::RewriteCss));
        // Consumed option headers are removed; the rest stay.
        assert!(headers.headers().lookup("PageSpeedFilters").is_none());
        assert!(headers.headers().lookup("X-Unrelated").is_some());
    }

    #[test]
    fn header_scan_applies_client_options() {
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("X-PSA-Client-Options", "v=1,m=2");
        let mut options = RewriteOptions::new();
        assert_eq!(
            QueryStatus::Success,
            scan_headers_for_options(&mut headers, true, &mut options)
        );
        assert!(!options.enabled(Filter::RewriteCss));
        // Proxy-only headers never reach the client.
        assert!(headers
            .headers()
            .lookup(header::X_PSA_CLIENT_OPTIONS)
            .is_none());
    }

    #[test]
    fn header_scan_honors_the_override_token() {
        use ps_config::scope::Scope;
        let mut options = RewriteOptions::new();
        options
            .set_option("RequestOptionOverride", "token", Scope::Directory)
            .unwrap();
        // Without the token, options stay gated.
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("PageSpeedFilters", "rewrite_css");
        assert_eq!(
            QueryStatus::NoneFound,
            scan_headers_for_options(&mut headers, false, &mut options)
        );
        assert!(!options.enabled(Filter::RewriteCss));
        // With it, the same header applies.
        let mut headers = RequestHeaders::new();
        headers
            .headers_mut()
            .add("PageSpeedRequestOptionOverride", "token");
        headers.headers_mut().add("PageSpeedFilters", "rewrite_css");
        assert_eq!(
            QueryStatus::Success,
            scan_headers_for_options(&mut headers, false, &mut options)
        );
        assert!(options.enabled(Filter::RewriteCss));
    }

    #[test]
    fn methods_and_properties() {
        assert_eq!(Method::parse("GET"), Method::Get);
        assert_eq!(Method::parse("PURGE"), Method::Purge);
        assert_eq!(Method::parse("get"), Method::Unknown);
        assert_eq!(Method::Get.token(), "GET");
        assert_eq!(Method::Get.as_str(), "GET");
        assert_eq!(Method::Unknown.as_str(), "ERROR");
        assert!(Method::parse("HEAD").is_head());

        let mut request = RequestHeaders::new();
        request.set_method(Method::Get);
        request.headers_mut().add("Cookie", "a=1");
        request
            .headers_mut()
            .add("Accept-Encoding", "gzip, deflate");
        let props = request.properties();
        assert!(props.has_cookie && !props.has_cookie2 && !props.has_authorization);
        assert!(request.accepts_gzip());
    }

    #[test]
    fn accepts_zstd_negotiation() {
        let with_encoding = |value: Option<&str>| {
            let mut request = RequestHeaders::new();
            if let Some(value) = value {
                request.headers_mut().add("Accept-Encoding", value);
            }
            request.accepts_zstd()
        };
        assert!(with_encoding(Some("gzip, deflate, br, zstd")));
        assert!(with_encoding(Some("zstd")));
        assert!(!with_encoding(Some("gzip, deflate, br")));
        assert!(!with_encoding(Some("zstd;q=0")));
        assert!(!with_encoding(None));
        // Multi-line headers join: an explicit q=0 still wins over *.
        let mut split = RequestHeaders::new();
        split.headers_mut().add("Accept-Encoding", "zstd;q=0");
        split.headers_mut().add("Accept-Encoding", "*;q=1");
        assert!(!split.accepts_zstd());
    }

    #[test]
    fn context_timing() {
        let mut context = RequestContext::new(HttpOptions::default_for_tests());
        context.set_cache_lookup_ms(3);
        context.set_fetch_ms(40);
        assert_eq!(
            context.timing(),
            FetchTiming {
                cache_lookup_ms: 3,
                fetch_ms: 40,
            }
        );
    }

    #[test]
    fn http_string_shape() {
        use super::Method;
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Get);
        headers.set_version(1, 1);
        headers.headers_mut().add("Host", "x.test");
        headers.headers_mut().add("Accept", "text/html");
        assert_eq!(
            headers.to_http_string(),
            "GET  HTTP/1.1\r\nHost: x.test\r\nAccept: text/html\r\n\r\n"
        );
    }

    #[test]
    fn http_string_without_cookies() {
        use super::Method;
        let mut headers = RequestHeaders::new();
        headers.set_method(Method::Get);
        headers.set_version(1, 1);
        headers.headers_mut().add("Host", "x.test");
        headers.headers_mut().add("Cookie", "_ga=1; _gid=2");
        headers.headers_mut().add("Accept", "text/html");
        headers.headers_mut().add("Cookie2", "$Version=1");
        assert_eq!(
            headers.to_http_string_without_cookies(),
            "GET  HTTP/1.1\r\nHost: x.test\r\nAccept: text/html\r\n\r\n"
        );
        // Cookie-free requests render byte-identically either way.
        let mut plain = RequestHeaders::new();
        plain.set_method(Method::Get);
        plain.set_version(1, 1);
        plain.headers_mut().add("Host", "x.test");
        assert_eq!(
            plain.to_http_string_without_cookies(),
            plain.to_http_string()
        );
    }
}
