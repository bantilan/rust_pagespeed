//! HTTP names and status codes (`http_names.{h,cc}`).
//!
//! Header-name constants, status codes with reason phrases, and the
//! hop-by-hop / end-to-end header sets used by sanitizing.
#![forbid(unsafe_code)]

/// Well-known header names (`HttpAttributes`).
pub mod header {
    pub const ACCEPT: &str = "Accept";
    pub const ACCEPT_ENCODING: &str = "Accept-Encoding";
    pub const ACCEPT_RANGES: &str = "Accept-Ranges";
    pub const AGE: &str = "Age";
    pub const ALLOW: &str = "Allow";
    pub const ALT_SVC: &str = "Alt-Svc";
    pub const ALTERNATE_PROTOCOL: &str = "Alternate-Protocol";
    pub const AUTHORIZATION: &str = "Authorization";
    pub const CACHE_CONTROL: &str = "Cache-Control";
    pub const CONNECTION: &str = "Connection";
    pub const CONTENT_DISPOSITION: &str = "Content-Disposition";
    pub const CONTENT_ENCODING: &str = "Content-Encoding";
    pub const CONTENT_LANGUAGE: &str = "Content-Language";
    pub const CONTENT_LENGTH: &str = "Content-Length";
    pub const CONTENT_RANGE: &str = "Content-Range";
    pub const CONTENT_TYPE: &str = "Content-Type";
    pub const COOKIE: &str = "Cookie";
    pub const COOKIE2: &str = "Cookie2";
    pub const DATE: &str = "Date";
    pub const ETAG: &str = "Etag";
    pub const EXPECT: &str = "Expect";
    pub const EXPIRES: &str = "Expires";
    pub const FROM: &str = "From";
    pub const GZIP: &str = "gzip";
    pub const DEFLATE: &str = "deflate";
    pub const HOST: &str = "Host";
    pub const IF_MODIFIED_SINCE: &str = "If-Modified-Since";
    pub const IF_NONE_MATCH: &str = "If-None-Match";
    pub const KEEP_ALIVE: &str = "Keep-Alive";
    pub const LAST_MODIFIED: &str = "Last-Modified";
    pub const LINK: &str = "Link";
    pub const LOCATION: &str = "Location";
    pub const NO_CACHE: &str = "no-cache";
    pub const NO_CACHE_MAX_AGE_0: &str = "max-age=0, no-cache";
    pub const PRAGMA: &str = "Pragma";
    pub const PROXY_AUTHENTICATE: &str = "Proxy-Authenticate";
    pub const PROXY_AUTHORIZATION: &str = "Proxy-Authorization";
    pub const REFERER: &str = "Referer";
    pub const SAVE_DATA: &str = "Save-Data";
    pub const SERVER: &str = "Server";
    pub const SET_COOKIE: &str = "Set-Cookie";
    pub const SET_COOKIE2: &str = "Set-Cookie2";
    pub const TE: &str = "TE";
    pub const TRAILERS: &str = "Trailers";
    pub const TRANSFER_ENCODING: &str = "Transfer-Encoding";
    pub const UPGRADE: &str = "Upgrade";
    pub const USER_AGENT: &str = "User-Agent";
    pub const VARY: &str = "Vary";
    pub const VIA: &str = "Via";
    pub const WARNING: &str = "Warning";
    pub const X_ORIGINAL_CONTENT_LENGTH: &str = "X-Original-Content-Length";
    pub const X_PSA_BLOCKING_REWRITE: &str = "X-PSA-Blocking-Rewrite";
    pub const X_PSA_CLIENT_OPTIONS: &str = "X-PSA-Client-Options";
    pub const X_PSA_LOAD_SHED: &str = "X-Psa-Load-Shed";
    pub const X_PSA_PURGE_REQUEST: &str = "X-PSA-Purge-Request";
    pub const X_REQUESTED_WITH: &str = "X-Requested-With";
}

/// Ajax marker value (`HttpAttributes::kXmlHttpRequest`).
pub const XML_HTTP_REQUEST: &str = "XMLHttpRequest";

/// Hop-by-hop headers (`SortedHopByHopHeaders`), case-insensitively sorted.
pub const HOP_BY_HOP_HEADERS: &[&str] = &[
    "Alt-Svc",
    "Alternate-Protocol",
    "Connection",
    "Keep-Alive",
    "Proxy-Authenticate",
    "Proxy-Authorization",
    "Set-Cookie",
    "Set-Cookie2",
    "TE",
    "Trailers",
    "Transfer-Encoding",
    "Upgrade",
];

/// End-to-end headers (`SortedEndToEndHeaders`), case-insensitively sorted.
pub const END_TO_END_HEADERS: &[&str] = &[
    "Accept",
    "Accept-Encoding",
    "Access-Control-Allow-Credentials",
    "Access-Control-Allow-Origin",
    "Age",
    "Allow",
    "Authorization",
    "Cache-Control",
    "Content-Disposition",
    "Content-Encoding",
    "Content-Language",
    "Content-Length",
    "Content-Type",
    "Cookie",
    "Cookie2",
    "Date",
    "Etag",
    "Expires",
    "Host",
    "If-Modified-Since",
    "If-None-Match",
    "Last-Modified",
    "Link",
    "Location",
    "Origin",
    "Pragma",
    "Purpose",
    "Referer",
    "Refresh",
    "Server",
    "Set-Cookie",
    "Set-Cookie2",
    "User-Agent",
    "Vary",
    "Via",
    "Warning",
];

/// HTTP status codes (`HttpStatus::Code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusCode(pub u16);

impl StatusCode {
    /// Builds from a raw code.
    #[must_use]
    pub const fn new(code: u16) -> StatusCode {
        StatusCode(code)
    }

    /// Reason phrase (`HttpStatus::GetReasonPhrase`; unknown codes take
    /// the blame as "Internal Server Error").
    #[must_use]
    pub const fn reason_phrase(self) -> &'static str {
        match self.0 {
            100 => "Continue",
            101 => "Switching Protocols",
            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            203 => "Non-Authoritative Information",
            204 => "No Content",
            205 => "Reset Content",
            206 => "Partial Content",
            300 => "Multiple Choices",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            304 => "Not Modified",
            305 => "Use Proxy",
            307 => "Temporary Redirect",
            400 => "Bad Request",
            401 => "Unauthorized",
            402 => "Payment Required",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            406 => "Not Acceptable",
            407 => "Proxy Authentication Required",
            408 => "Request Time-out",
            409 => "Conflict",
            410 => "Gone",
            411 => "Length Required",
            412 => "Precondition Failed",
            413 => "Request Entity Too Large",
            414 => "Request-URI Too Large",
            415 => "Unsupported Media Type",
            416 => "Requested range not satisfiable",
            417 => "Expectation Failed",
            500 => "Internal Server Error",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Time-out",
            505 => "HTTP Version Not Supported",
            520 => "Proxy Publisher Failure",
            521 => "Proxy Failure",
            522 => "Proxy Config Failure",
            523 => "Proxy Declined Request",
            _ => "Internal Server Error",
        }
    }
}

/// Whether a status is a redirect for heuristic caching
/// (`IsRedirectStatusCode`: 301, 302, 303, 307 — note 304/305 excluded).
#[must_use]
pub const fn is_redirect_status(code: u16) -> bool {
    matches!(code, 301 | 302 | 303 | 307)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_phrases() {
        assert_eq!(StatusCode::new(200).reason_phrase(), "OK");
        assert_eq!(StatusCode::new(404).reason_phrase(), "Not Found");
        assert_eq!(StatusCode::new(408).reason_phrase(), "Request Time-out");
        assert_eq!(
            StatusCode::new(416).reason_phrase(),
            "Requested range not satisfiable"
        );
        assert_eq!(
            StatusCode::new(520).reason_phrase(),
            "Proxy Publisher Failure"
        );
        assert_eq!(
            StatusCode::new(599).reason_phrase(),
            "Internal Server Error"
        );
    }

    #[test]
    fn header_sets_sorted() {
        for set in [HOP_BY_HOP_HEADERS, END_TO_END_HEADERS] {
            let mut sorted = set.to_vec();
            sorted.sort_by_key(|a| a.to_ascii_lowercase());
            assert_eq!(set, sorted.as_slice());
        }
        assert_eq!(HOP_BY_HOP_HEADERS.len(), 12);
        assert_eq!(END_TO_END_HEADERS.len(), 36);
        assert!(is_redirect_status(301));
        assert!(!is_redirect_status(304));
        assert!(!is_redirect_status(200));
    }
}
