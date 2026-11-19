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
use ps_config::options::{QueryStatus, RewriteOptions, Value};

/// HTTP request methods (`RequestHeaders::Method`, request subset plus
/// the purge method PageSpeed issues downstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Method {
    /// Unset.
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
            "PURGE" => Method::Purge,
            _ => Method::Unknown,
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
}

/// Scans request headers for PageSpeed options (`ScanHeader`).
///
/// Request headers carry options exactly like query parameters;
/// `allow_options` gates everything after the bare `PageSpeed=`
/// switch unless a `*RequestOptionOverride` header matches the
/// configured token. Consumed `ModPagespeed*`/`PageSpeed*`
/// headers and any `X-PSA-Client-Options` header are removed.
#[must_use]
pub fn scan_headers_for_options(
    headers: &mut RequestHeaders,
    allow_options: bool,
    options: &mut RewriteOptions,
) -> QueryStatus {
    let mut allow_options = allow_options;
    let mut status = QueryStatus::NoneFound;

    // Check to see if the override token exists.
    let token = match options.get_by_name("RequestOptionOverride") {
        Some(Value::Str(token)) => token,
        _ => String::new(),
    };
    if !allow_options && !token.is_empty() {
        for (name, value) in headers.headers().iter() {
            if name == "ModPagespeedRequestOptionOverride"
                || name == "PageSpeedRequestOptionOverride"
            {
                allow_options = value == token;
                break;
            }
        }
    }

    // Tracks the headers that need to be removed.
    let mut to_remove: Vec<(String, String)> = Vec::new();
    // Snapshot the pairs: scanning may mutate options but not headers.
    let pairs: Vec<(String, String)> = headers
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    for (name, value) in &pairs {
        match options.scan_query(name, value, allow_options) {
            QueryStatus::NoneFound => {}
            QueryStatus::Success => {
                if name.starts_with("ModPagespeed") || name.starts_with("PageSpeed") {
                    to_remove.push((name.clone(), value.clone()));
                }
                status = QueryStatus::Success;
            }
            QueryStatus::Invalid => return QueryStatus::Invalid,
        }
    }
    for (name, value) in &to_remove {
        headers.headers_mut().remove(name, value);
    }
    // kXPsaClientOptions is meant for proxy only. Remove it in any case.
    headers
        .headers_mut()
        .remove_all(header::X_PSA_CLIENT_OPTIONS);
    status
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_config::filter::Filter;

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
}
