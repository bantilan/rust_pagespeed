//! Response headers (`pagespeed/kernel/http/response_headers.{h,cc}`,
//! `caching_headers.{h,cc}`).
//!
//! Ordered headers plus computed caching fields, with protobuf-compatible
//! binary serialization (presence bits included). `compute_caching`
//! mirrors `ComputeCaching` exactly, including its quirks (stale `date_ms`
//! and `cache_ttl_ms` retention, one-way `is_implicitly_cacheable`).
#![forbid(unsafe_code)]

use crate::content_type::{mime_to_content_type, ContentType};
use crate::google_url::GoogleUrl;
use crate::headers::{element_has_directive, Headers};
use crate::names::header;
use crate::proto::{decode_response, encode_response, ResponseProto};
use crate::time::{format_http_date, parse_http_date};

/// Per-request HTTP options (`HttpOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HttpOptions {
    /// Whether `Vary` (beyond encoding/cookie) blocks resource caching.
    pub respect_vary: bool,
    /// TTL for resources without explicit caching headers.
    pub implicit_cache_ttl_ms: i64,
}

impl HttpOptions {
    /// Test/default options: no vary respect, 5-minute implicit TTL.
    #[must_use]
    pub const fn default_for_tests() -> HttpOptions {
        HttpOptions {
            respect_vary: false,
            implicit_cache_ttl_ms: 5 * 60 * 1000,
        }
    }
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions::default_for_tests()
    }
}

/// Whether `Vary` blocks resource caching (`VaryOption`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaryOption {
    /// Respect `Vary` on resources.
    RespectVaryOnResources,
    /// Ignore `Vary` on resources (HTML always respects it).
    IgnoreVaryOnResources,
}

/// Whether the request carries a validator (`ValidatorOption`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorOption {
    /// Request has a validator.
    HasValidator,
    /// Request has no validator.
    NoValidator,
}

/// Request properties relevant to caching
/// (`RequestHeaders::Properties`; default assumes cookies present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestProperties {
    /// Request carries a `Cookie` header.
    pub has_cookie: bool,
    /// Request carries a `Cookie2` header.
    pub has_cookie2: bool,
    /// Request carries `Authorization`.
    pub has_authorization: bool,
}

impl Default for RequestProperties {
    fn default() -> Self {
        RequestProperties {
            has_cookie: true,
            has_cookie2: true,
            has_authorization: false,
        }
    }
}

/// Parsed `max-age`/`s-maxage` directive (`LifetimeDirective`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LifetimeDirective {
    name: LifetimeName,
    embedded: bool,
    readable: bool,
    seconds: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifetimeName {
    None,
    MaxAge,
    SMaxAge,
}

/// Reads one Cache-Control element for lifetime directives
/// (`ReadLifetimeDirective`).
fn read_lifetime_directive(element: &str) -> LifetimeDirective {
    let mut result = LifetimeDirective {
        name: LifetimeName::None,
        embedded: false,
        readable: false,
        seconds: 0,
    };
    let element = element.trim();
    let joined = element.contains(';');
    let equals = element.find('=');
    let name = match equals {
        Some(pos) => &element[..pos],
        None => element,
    };
    let trimmed = name.trim();
    let exact = if starts_with_fold(trimmed, "max-age") {
        result.name = LifetimeName::MaxAge;
        Some("max-age")
    } else if starts_with_fold(trimmed, "s-maxage") {
        result.name = LifetimeName::SMaxAge;
        Some("s-maxage")
    } else {
        None
    };
    if let Some(exact_name) = exact {
        result.embedded = joined;
        result.readable = !joined
            && equals.is_some()
            && trimmed.eq_ignore_ascii_case(exact_name)
            && parse_lifetime_seconds(&element[equals.unwrap() + 1..]).is_some_and(|seconds| {
                result.seconds = seconds;
                true
            });
        return result;
    }
    if joined || element.contains('"') {
        let lower = element.to_ascii_lowercase();
        if lower.contains("max-age") {
            result.name = LifetimeName::MaxAge;
        } else if lower.contains("s-maxage") {
            result.name = LifetimeName::SMaxAge;
        }
        result.embedded = true;
    }
    result
}

fn starts_with_fold(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// Parses a lifetime value with optional quotes, clamped to i32
/// (`ParseLifetimeSeconds`).
fn parse_lifetime_seconds(value: &str) -> Option<i32> {
    let mut value = value;
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    }
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut result: i64 = 0;
    for byte in value.bytes() {
        if result <= i64::from(i32::MAX) {
            result = result * 10 + i64::from(byte - b'0');
        }
    }
    Some(result.min(i64::from(i32::MAX)) as i32)
}

/// Parsed cache-control state (`CachingHeaders` fields).
#[derive(Debug, Clone, Default)]
struct CacheControlState {
    max_age: Option<i32>,
    s_max_age: Option<i32>,
    public: bool,
    private: bool,
    no_cache: bool,
    no_store: bool,
    must_revalidate: bool,
    proxy_revalidate: bool,
    parse_error: bool,
    expires_ms: Option<i64>,
    expires_invalid: bool,
}

/// The caching computation (`CachingHeaders` + `InstawebCacheComputer`).
#[derive(Debug, Clone)]
struct CacheComputer<'a> {
    headers: &'a Headers,
    status: i32,
    url_has_query: bool,
    state: Option<CacheControlState>,
    freshness: Option<(bool, i64)>,
}

impl<'a> CacheComputer<'a> {
    fn new(headers: &'a Headers, status: i32) -> CacheComputer<'a> {
        CacheComputer {
            headers,
            status,
            url_has_query: false,
            state: None,
            freshness: None,
        }
    }

    fn lookup(&self, name: &str) -> Vec<String> {
        self.headers.lookup(name).unwrap_or_default()
    }

    fn state(&mut self) -> &CacheControlState {
        if self.state.is_none() {
            let mut state = CacheControlState::default();
            for element in self.lookup(header::CACHE_CONTROL) {
                let element = element.trim();
                let lifetime = read_lifetime_directive(element);
                if lifetime.name != LifetimeName::None {
                    if !lifetime.readable {
                        state.parse_error = true;
                    } else if lifetime.name == LifetimeName::MaxAge {
                        if state.max_age.is_none_or(|prev| lifetime.seconds < prev) {
                            state.max_age = Some(lifetime.seconds);
                        }
                    } else if state.s_max_age.is_none_or(|prev| lifetime.seconds < prev) {
                        state.s_max_age = Some(lifetime.seconds);
                    }
                }
                if element.eq_ignore_ascii_case("public") {
                    state.public = true;
                }
                for piece in element.split(';').map(str::trim).filter(|p| !p.is_empty()) {
                    if starts_with_fold(piece, "private") {
                        state.private = true;
                    } else if starts_with_fold(piece, "no-cache") {
                        state.no_cache = true;
                    } else if piece.eq_ignore_ascii_case("no-store") {
                        state.no_store = true;
                    } else if piece.eq_ignore_ascii_case("must-revalidate") {
                        state.must_revalidate = true;
                    } else if piece.eq_ignore_ascii_case("proxy-revalidate") {
                        state.proxy_revalidate = true;
                    }
                }
            }
            let expires = self.lookup(header::EXPIRES);
            if !expires.is_empty() {
                match parse_http_date(&expires[0]) {
                    Some(ms) => state.expires_ms = Some(ms),
                    None => state.expires_invalid = true,
                }
            }
            self.state = Some(state);
        }
        self.state
            .as_ref()
            .unwrap_or_else(|| unreachable!("just set"))
    }

    fn has_explicit_no_cache(&mut self) -> bool {
        let state = self.state().clone();
        if state.no_cache
            || state.no_store
            || state.s_max_age.is_some_and(|secs| secs <= 0)
            || state.max_age.is_some_and(|secs| secs <= 0)
            || state.parse_error
            || state.expires_invalid
        {
            return true;
        }
        for value in self.lookup(header::PRAGMA) {
            if value.eq_ignore_ascii_case("no-cache") {
                return true;
            }
        }
        self.lookup(header::VARY).iter().any(|value| value == "*")
    }

    /// Freshness lifetime, if explicitly defined
    /// (`ComputeFreshnessLifetimeMillis`).
    fn freshness_lifetime_ms(&mut self) -> Option<i64> {
        if self.freshness.is_none() {
            let result = if self.has_explicit_no_cache() {
                Some(0)
            } else {
                let mut lifetime = None;
                let state = self.state().clone();
                if let Some(max_age) = state.max_age {
                    lifetime = Some(1000i64 * i64::from(max_age));
                } else if let Some(expires_ms) = state.expires_ms {
                    let dates = self.lookup(header::DATE);
                    if dates.len() == 1 {
                        if let Some(date_ms) = parse_http_date(&dates[0]) {
                            lifetime = Some((expires_ms - date_ms).max(0));
                        }
                    }
                }
                if let Some(current) = lifetime {
                    if let Some(s_max_age) = self.state().s_max_age {
                        let capped = 1000i64 * i64::from(s_max_age);
                        lifetime = Some(current.min(capped));
                    }
                }
                lifetime
            };
            // is_explicitly_cacheable_ mirrors the Option-ness.
            self.freshness = Some((result.is_some(), result.unwrap_or(0)));
        }
        let (explicit, lifetime) = self.freshness.unwrap_or((false, 0));
        explicit.then_some(lifetime)
    }

    fn is_explicitly_cacheable(&mut self) -> bool {
        self.freshness_lifetime_ms()
            .is_some_and(|lifetime| lifetime > 0)
    }

    fn must_revalidate(&mut self) -> bool {
        self.state().must_revalidate
    }

    fn proxy_revalidate(&mut self) -> bool {
        self.state().proxy_revalidate
    }

    fn is_cacheable_status(&self) -> bool {
        matches!(self.status, 200 | 301) || (10001..10007).contains(&self.status)
    }

    fn is_redirect_status(&self) -> bool {
        crate::names::is_redirect_status(self.status as u16)
    }

    fn is_likely_static(&self) -> bool {
        if crate::names::is_redirect_status(self.status as u16) {
            return true;
        }
        determine_content_type(self.headers)
            .is_some_and(|content| content.is_likely_static_resource())
    }

    fn is_heuristically_cacheable(&mut self) -> bool {
        if self.freshness_lifetime_ms().is_some() {
            return false; // explicit lifetime: not heuristic territory.
        }
        if self.must_revalidate() {
            return false;
        }
        if self.url_has_query {
            return false;
        }
        if !self.is_cacheable_status() {
            return false;
        }
        self.is_likely_static()
    }

    fn is_cacheable(&mut self) -> bool {
        match self.freshness_lifetime_ms() {
            Some(lifetime) => lifetime > 0,
            None => self.is_heuristically_cacheable(),
        }
    }

    fn is_proxy_cacheable_inner(&mut self) -> bool {
        let cacheable = self.is_cacheable();
        cacheable && !self.state().private
    }
}

/// Determines the content type from the LAST Content-Type header
/// (`DetermineContentType` + mime-sniff LAST rule).
fn determine_content_type(headers: &Headers) -> Option<ContentType> {
    let mut last = None;
    for index in 0..headers.len() {
        if headers
            .name(index)
            .eq_ignore_ascii_case(header::CONTENT_TYPE)
        {
            last = Some(headers.value(index));
        }
    }
    last.and_then(|value| {
        let mime = value.split(';').next().unwrap_or(value);
        mime_to_content_type(mime)
    })
}

/// Computed caching fields with presence bits (the proto scalars).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ComputedCaching {
    expiration_time_ms: Option<i64>,
    date_ms: Option<i64>,
    last_modified_time_ms: Option<i64>,
    cache_ttl_ms: Option<i64>,
    browser_cacheable: Option<bool>,
    proxy_cacheable: Option<bool>,
    requires_browser_revalidation: Option<bool>,
    requires_proxy_revalidation: Option<bool>,
    is_implicitly_cacheable: Option<bool>,
}

/// Response headers (`ResponseHeaders`).
#[derive(Debug, Clone, Default)]
pub struct ResponseHeaders {
    headers: Headers,
    status_code: Option<i32>,
    reason_phrase: Option<String>,
    major_version: Option<i32>,
    minor_version: Option<i32>,
    computed: ComputedCaching,
    dirty: bool,
    force_cache_ttl_ms: i64,
    force_cached: bool,
    http_options: HttpOptions,
}

impl ResponseHeaders {
    /// Creates empty headers with options (`ResponseHeaders(options)`).
    #[must_use]
    pub fn new(http_options: HttpOptions) -> ResponseHeaders {
        let mut headers = ResponseHeaders {
            http_options,
            ..ResponseHeaders::default()
        };
        headers.clear();
        headers
    }

    /// Borrows the header list.
    #[must_use]
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// Mutably borrows the header list (marks caching dirty).
    pub fn headers_mut(&mut self) -> &mut Headers {
        self.dirty = true;
        &mut self.headers
    }

    /// Clears everything (`Clear`), including the proto presence quirks:
    /// `proxy_cacheable` and `cache_ttl_ms` keep stale values.
    pub fn clear(&mut self) {
        self.headers.clear();
        self.major_version = None;
        self.minor_version = None;
        self.status_code = None;
        self.reason_phrase = None;
        let stale_proxy = self.computed.proxy_cacheable;
        let stale_ttl = self.computed.cache_ttl_ms;
        self.computed = ComputedCaching {
            browser_cacheable: Some(false),
            requires_browser_revalidation: Some(false),
            requires_proxy_revalidation: Some(false),
            proxy_cacheable: stale_proxy,
            cache_ttl_ms: stale_ttl,
            ..ComputedCaching::default()
        };
        self.dirty = false;
        self.force_cache_ttl_ms = -1;
    }

    /// Copies from another instance (`CopyFrom`).
    pub fn copy_from(&mut self, other: &ResponseHeaders) {
        *self = other.clone();
    }

    /// Status code, 0 when unset (proto default).
    #[must_use]
    pub fn status_code(&self) -> i32 {
        self.status_code.unwrap_or(0)
    }

    /// Whether a status code is set.
    #[must_use]
    pub fn has_status_code(&self) -> bool {
        self.status_code.is_some()
    }

    /// Whether a header carries a value, byte-exact (`HasValue`).
    #[must_use]
    pub fn has_value(&self, name: &str, value: &str) -> bool {
        self.headers.has_value(name, value)
    }

    /// Sets the status code (marks caching dirty).
    pub fn set_status_code(&mut self, code: i32) {
        self.dirty = true;
        self.status_code = Some(code);
    }

    /// Reason phrase, `"(null)"` when unset (as in C++).
    #[must_use]
    pub fn reason_phrase(&self) -> &str {
        self.reason_phrase.as_deref().unwrap_or("(null)")
    }

    /// Sets the reason phrase.
    pub fn set_reason_phrase(&mut self, reason: &str) {
        self.reason_phrase = Some(reason.to_string());
    }

    /// Major version, 1 when unset (proto `default = 1`).
    #[must_use]
    pub fn major_version(&self) -> i32 {
        self.major_version.unwrap_or(1)
    }

    /// Minor version, 0 when unset.
    #[must_use]
    pub fn minor_version(&self) -> i32 {
        self.minor_version.unwrap_or(0)
    }

    /// Whether a major version is set.
    #[must_use]
    pub fn has_major_version(&self) -> bool {
        self.major_version.is_some()
    }

    /// Sets major/minor versions.
    pub fn set_version(&mut self, major: i32, minor: i32) {
        self.major_version = Some(major);
        self.minor_version = Some(minor);
    }

    /// Sets status line parts (`set_first_line`).
    pub fn set_first_line(&mut self, major: i32, minor: i32, status: i32, reason: &str) {
        self.set_version(major, minor);
        self.set_status_code(status);
        self.set_reason_phrase(reason);
    }

    /// Sets status + canonical reason (`SetStatusAndReason`).
    pub fn set_status_and_reason(&mut self, code: u16) {
        self.set_status_code(i32::from(code));
        self.set_reason_phrase(crate::names::StatusCode::new(code).reason_phrase());
    }

    /// Parses `HTTP/x.y ...` (`ParseFirstLine`; warns and ignores garbage).
    pub fn parse_first_line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("HTTP/") {
            self.parse_first_line_helper(rest);
        }
    }

    /// Parses `x.y status [reason]` (`ParseFirstLineHelper`, sscanf
    /// `%d.%d %d %[^\n\t]s` semantics: skips leading whitespace per
    /// conversion, requires at least 3 conversions, defaults the reason).
    pub fn parse_first_line_helper(&mut self, line: &str) {
        let mut rest = line;
        let major: i32 = match scan_int(&mut rest) {
            Some(value) => value,
            None => return,
        };
        // The '.' matches literally (no whitespace skipping, unlike %d).
        rest = match rest.strip_prefix('.') {
            Some(tail) => tail,
            None => return,
        };
        let minor: i32 = match scan_int(&mut rest) {
            Some(value) => value,
            None => return,
        };
        let status: i32 = match scan_int(&mut rest) {
            Some(value) => value,
            None => return,
        };
        // The format space before %[ skips blanks; an empty remainder
        // means the 4th conversion failed, defaulting the reason.
        let reason = rest.trim_start();
        let reason = if reason.is_empty() {
            crate::names::StatusCode::new(status as u16).reason_phrase()
        } else {
            reason
        };
        self.set_first_line(major, minor, status, reason);
    }

    /// Recomputes caching fields (`ComputeCaching`; no-op when clean).
    pub fn compute_caching(&mut self) {
        if !self.dirty {
            return;
        }
        self.force_cached = false;
        if let Some(date_ms) = self.parse_date_header(header::DATE) {
            self.computed.date_ms = Some(date_ms);
        }
        // NOTE: a missing Date leaves any stale date_ms in place (C++ has
        // no else-clear here), unlike Last-Modified below.
        match self.parse_date_header(header::LAST_MODIFIED) {
            Some(ms) => self.computed.last_modified_time_ms = Some(ms),
            None => self.computed.last_modified_time_ms = None,
        }
        let mut computer = CacheComputer::new(&self.headers, self.status_code());
        let content = determine_content_type(&self.headers);
        let mut force_enabled = false;
        if self.force_cache_ttl_ms > 0 && self.status_code() == 200 {
            force_enabled = match content {
                None => true,
                Some(content) => !content.is_html_like(),
            };
        }
        let browser_ok = computer.is_cacheable();
        let allowed = computer.is_cacheable_status();
        self.computed.browser_cacheable =
            Some(self.computed.date_ms.is_some() && allowed && (force_enabled || browser_ok));
        self.computed.requires_browser_revalidation = Some(computer.must_revalidate());
        self.computed.requires_proxy_revalidation = Some(
            computer.proxy_revalidate()
                || self.computed.requires_browser_revalidation == Some(true),
        );
        if self.computed.browser_cacheable == Some(true) {
            let proxy_ok = computer.is_proxy_cacheable_inner();
            let mut cache_ttl_ms = self.http_options.implicit_cache_ttl_ms;
            if computer.is_explicitly_cacheable() {
                cache_ttl_ms = computer.freshness_lifetime_ms().unwrap_or(cache_ttl_ms);
            }
            if force_enabled && (self.force_cache_ttl_ms > cache_ttl_ms || !proxy_ok) {
                cache_ttl_ms = self.force_cache_ttl_ms;
                self.force_cached = true;
            }
            self.computed.cache_ttl_ms = Some(cache_ttl_ms);
            self.computed.expiration_time_ms =
                Some(self.computed.date_ms.unwrap_or(0) + cache_ttl_ms);
            self.computed.proxy_cacheable = Some(self.force_cached || proxy_ok);
            // Never proxy-cache HTML/redirects carrying cookies.
            if (content.is_some_and(|content| content.is_html_like())
                || computer.is_redirect_status())
                && (self.headers.has(header::SET_COOKIE) || self.headers.has(header::SET_COOKIE2))
            {
                self.computed.proxy_cacheable = Some(false);
            }
            if self.computed.proxy_cacheable == Some(true)
                && !self.force_cached
                && !computer.is_explicitly_cacheable()
            {
                self.computed.is_implicitly_cacheable = Some(true);
                let date_ms = self.computed.date_ms.unwrap_or(0);
                let suffix = self.cache_control_values_to_preserve();
                self.set_date_and_caching(date_ms, cache_ttl_ms, &suffix);
            }
        } else {
            self.computed.expiration_time_ms = Some(0);
            self.computed.proxy_cacheable = Some(false);
        }
        self.dirty = false;
    }

    /// Whether the response is dirty (needs `compute_caching`).
    #[must_use]
    pub fn cache_fields_dirty(&self) -> bool {
        self.dirty
    }

    /// Proxy-cacheable check (`IsProxyCacheable`).
    #[must_use]
    pub fn is_proxy_cacheable(
        &self,
        req: RequestProperties,
        vary: VaryOption,
        validator: ValidatorOption,
    ) -> bool {
        debug_assert!(!self.dirty, "call compute_caching() first");
        if self.computed.proxy_cacheable != Some(true) {
            return false;
        }
        if req.has_authorization
            && !self
                .headers
                .has_value_case_insensitive(header::CACHE_CONTROL, "public")
        {
            return false;
        }
        let html_like =
            determine_content_type(&self.headers).is_some_and(|content| content.is_html_like());
        if let Some(values) = self.headers.lookup(header::VARY) {
            for value in values {
                if value.is_empty() || value.eq_ignore_ascii_case(header::ACCEPT_ENCODING) {
                    continue;
                }
                if value.eq_ignore_ascii_case(header::COOKIE) {
                    if req.has_cookie || !html_like || validator == ValidatorOption::NoValidator {
                        return false;
                    }
                } else if value.eq_ignore_ascii_case(header::COOKIE2) {
                    if req.has_cookie2 || !html_like || validator == ValidatorOption::NoValidator {
                        return false;
                    }
                } else if vary == VaryOption::RespectVaryOnResources || html_like {
                    return false;
                }
            }
        }
        true
    }

    /// Browser-cacheable flag (`IsBrowserCacheable`).
    #[must_use]
    pub fn is_browser_cacheable(&self) -> bool {
        debug_assert!(!self.dirty, "call compute_caching() first");
        self.computed.browser_cacheable == Some(true)
    }

    /// Requires browser revalidation (`RequiresBrowserRevalidation`).
    #[must_use]
    pub fn requires_browser_revalidation(&self) -> bool {
        debug_assert!(!self.dirty, "call compute_caching() first");
        self.computed.requires_browser_revalidation == Some(true)
    }

    /// Requires proxy revalidation (`RequiresProxyRevalidation`).
    #[must_use]
    pub fn requires_proxy_revalidation(&self) -> bool {
        debug_assert!(!self.dirty, "call compute_caching() first");
        self.computed.requires_proxy_revalidation == Some(true)
    }

    /// Absolute expiry ms (`CacheExpirationTimeMs`).
    #[must_use]
    pub fn cache_expiration_time_ms(&self) -> i64 {
        debug_assert!(!self.dirty, "call compute_caching() first");
        self.computed.expiration_time_ms.unwrap_or(0)
    }

    /// Cache TTL ms (`cache_ttl_ms`, stale-keeping semantics included).
    #[must_use]
    pub fn cache_ttl_ms(&self) -> i64 {
        self.computed.cache_ttl_ms.unwrap_or(0)
    }

    /// Date header ms (`date_ms`, 0 when absent).
    #[must_use]
    pub fn date_ms(&self) -> i64 {
        self.computed.date_ms.unwrap_or(0)
    }

    /// Whether a Date was parsed (`has_date_ms`).
    #[must_use]
    pub fn has_date_ms(&self) -> bool {
        self.computed.date_ms.is_some()
    }

    /// Last-Modified ms (`last_modified_time_ms`, 0 when absent).
    #[must_use]
    pub fn last_modified_time_ms(&self) -> i64 {
        self.computed.last_modified_time_ms.unwrap_or(0)
    }

    /// Whether implicitly cacheable (`is_implicitly_cacheable`).
    #[must_use]
    pub fn is_implicitly_cacheable(&self) -> bool {
        self.computed.is_implicitly_cacheable == Some(true)
    }

    /// Whether force-cached by the last computation.
    #[must_use]
    pub fn force_cached(&self) -> bool {
        self.force_cached
    }

    /// Current options.
    #[must_use]
    pub const fn http_options(&self) -> HttpOptions {
        self.http_options
    }

    /// Whether the status is 3xx non-304 (`IsRedirectStatus`).
    #[must_use]
    pub fn is_redirect_status(&self) -> bool {
        let status = self.status_code();
        (300..400).contains(&status) && status != 304
    }

    /// Whether the status is 4xx/5xx (`IsErrorStatus`).
    #[must_use]
    pub fn is_error_status(&self) -> bool {
        (400..600).contains(&self.status_code())
    }

    /// Whether the status is 5xx (`IsServerErrorStatus`).
    #[must_use]
    pub fn is_server_error_status(&self) -> bool {
        (500..600).contains(&self.status_code())
    }

    /// Whether marked gzipped (`IsGzipped`): Content-Encoding is present
    /// and its first value is exactly `gzip`.
    #[must_use]
    pub fn is_gzipped(&self) -> bool {
        self.headers
            .lookup(header::CONTENT_ENCODING)
            .is_some_and(|values| values.first().is_some_and(|first| first == "gzip"))
    }

    /// Whether the last encoding is exactly `gzip` (`WasGzippedLast`).
    #[must_use]
    pub fn was_gzipped_last(&self) -> bool {
        self.headers
            .lookup(header::CONTENT_ENCODING)
            .is_some_and(|values| values.last().is_some_and(|last| last == "gzip"))
    }

    /// Whether the encoding is anything but a lone `gzip`
    /// (`HasContentCodingOtherThanSingleGzip`): absent headers are fine,
    /// otherwise the first coding must be exactly `gzip` and a multi
    /// coding must not end in `gzip`.
    #[must_use]
    pub fn has_content_coding_other_than_single_gzip(&self) -> bool {
        match self.headers.lookup(header::CONTENT_ENCODING) {
            Some(values) => {
                let gzipped = values.first().is_some_and(|first| first == "gzip");
                !gzipped || values.len() > 1 && values.last().is_some_and(|last| last == "gzip")
            }
            None => false,
        }
    }

    /// Parses a date header (`ParseDateHeader`).
    #[must_use]
    pub fn parse_date_header(&self, name: &str) -> Option<i64> {
        self.headers
            .lookup1(name)
            .and_then(|value| parse_http_date(&value))
    }

    /// Parses an HTTP date (`ParseTime`).
    #[must_use]
    pub fn parse_time(text: &str) -> Option<i64> {
        parse_http_date(text)
    }

    /// Sets a time header from ms (`SetTimeHeader`).
    pub fn set_time_header(&mut self, name: &str, time_ms: i64) {
        self.headers_mut().replace(name, &format_http_date(time_ms));
    }

    /// Sets Date (`set_date`).
    pub fn set_date(&mut self, date_ms: i64) {
        self.set_time_header(header::DATE, date_ms);
    }

    /// Sets Last-Modified (`set_last_modified`).
    pub fn set_last_modified(&mut self, time_ms: i64) {
        self.set_time_header(header::LAST_MODIFIED, time_ms);
    }

    /// Sets `Cache-Control: public, max-age=N` (`SetCacheControlPublic`).
    pub fn set_cache_control_public(&mut self, max_age_sec: i64) {
        self.headers_mut().replace(
            header::CACHE_CONTROL,
            &format!("public, max-age={max_age_sec}"),
        );
    }

    /// Records the total input size (`SetOriginalContentLength`).
    pub fn set_original_content_length(&mut self, size: i64) {
        self.headers_mut()
            .replace(header::X_ORIGINAL_CONTENT_LENGTH, &size.to_string());
    }

    /// Whether a Cache-Control directive is present
    /// (`HasCacheControlDirective`).
    #[must_use]
    pub fn has_cache_control_directive(&self, directive: &str) -> bool {
        self.headers().has_cache_control_directive(directive)
    }

    /// Replaces `max-age`, keeping the other values
    /// (`SetCacheControlMaxAge`): `Expires` moves to
    /// `date + ttl`, the line becomes `max-age` plus every
    /// non-empty
    /// value that does not start with `max-age` (`,`-joined,
    /// no space), and caching recomputes when it was clean.
    pub fn set_cache_control_max_age(&mut self, ttl_ms: i64) {
        let recompute_caching = !self.dirty;
        self.set_time_header(header::EXPIRES, self.date_ms() + ttl_ms);
        let mut new_value = format!("max-age={}", ttl_ms / 1000);
        if let Some(values) = self.headers.lookup(header::CACHE_CONTROL) {
            for value in values {
                if !value.is_empty() && !starts_with_fold(&value, "max-age") {
                    new_value.push(',');
                    new_value.push_str(&value);
                }
            }
        }
        self.headers_mut()
            .replace(header::CACHE_CONTROL, &new_value);
        if recompute_caching {
            self.compute_caching();
        }
    }

    /// Keeps listed directives across lifetime rewrites
    /// (`CacheControlValuesToPreserve`): `no-transform`,
    /// `no-store`, then every standalone (non-embedded)
    /// `s-maxage` value, each `, `-prefixed (empty when none).
    #[must_use]
    pub fn cache_control_values_to_preserve(&self) -> String {
        let mut to_preserve = String::new();
        if self.has_cache_control_directive("no-transform") {
            to_preserve = String::from(", no-transform");
        }
        if self.has_cache_control_directive("no-store") {
            to_preserve.push_str(", no-store");
        }
        if let Some(values) = self.headers.lookup(header::CACHE_CONTROL) {
            for value in values {
                let lifetime = read_lifetime_directive(&value);
                if lifetime.name == LifetimeName::SMaxAge && !lifetime.embedded {
                    to_preserve.push_str(", ");
                    to_preserve.push_str(&value);
                }
            }
        }
        to_preserve
    }

    /// Sets Date + Cache-Control in one step
    /// (`SetDateAndCaching`): `Expires` stamps `date + ttl`
    /// (even at ttl 0) and the line becomes
    /// `max-age` plus the caller suffix verbatim (callers
    /// carry their own leading `, `).
    pub fn set_date_and_caching(&mut self, date_ms: i64, ttl_ms: i64, cache_control_suffix: &str) {
        self.set_date(date_ms);
        self.set_time_header(header::EXPIRES, date_ms + ttl_ms);
        self.headers_mut().replace(
            header::CACHE_CONTROL,
            &format!("max-age={}{}", ttl_ms / 1000, cache_control_suffix),
        );
    }

    /// Sets Content-Length (`SetContentLength`).
    pub fn set_content_length(&mut self, length: usize) {
        self.headers_mut()
            .replace(header::CONTENT_LENGTH, &length.to_string());
    }

    /// Clamps shared-cache lifetime (`SetSMaxAge`): lowers or
    /// appends `s-maxage`, refusing when directives forbid it.
    pub fn set_s_max_age(&mut self, s_maxage_sec: i64) {
        let existing = self
            .headers()
            .lookup(header::CACHE_CONTROL)
            .map(|values| values.join(", "))
            .unwrap_or_default();
        if let Some(updated) = Self::apply_s_max_age(s_maxage_sec, &existing) {
            self.headers_mut().replace(header::CACHE_CONTROL, &updated);
        }
    }

    /// Computes the clamped Cache-Control (`ApplySMaxAge`):
    /// `None` when nothing changes.
    fn apply_s_max_age(s_maxage_sec: i64, existing_cache_control: &str) -> Option<String> {
        let s_maxage_str = format!("s-maxage={s_maxage_sec}");
        let existing_cache_control = existing_cache_control.trim();
        if existing_cache_control.is_empty() {
            return Some(s_maxage_str);
        }
        let mut segments: Vec<String> = existing_cache_control
            .split(',')
            .map(|segment| segment.trim().to_string())
            .filter(|segment| !segment.is_empty())
            .collect();
        for segment in &segments {
            if element_has_directive(segment, "no-transform") {
                // We're not allowed to touch this, so don't.
                return None;
            }
            if element_has_directive(segment, "no-cache")
                || element_has_directive(segment, "no-store")
                || element_has_directive(segment, "private")
            {
                // Downstream shared caches shouldn't be caching
                // these, and adding s-maxage might confuse one
                // into thinking that it should actually go ahead
                // and cache, so if any of these are present, don't
                // add it.
                return None;
            }
            let lifetime = read_lifetime_directive(segment);
            if lifetime.name != LifetimeName::None && !lifetime.readable {
                // A lifetime we cannot read makes the response
                // stale for us, so don't invite a shared cache to
                // keep it.
                return None;
            }
        }
        // It's not clear from the RFC what we should do if there
        // are multiple s-maxages with different values. The most
        // conservative thing is probably to update them
        // individually, so let's do that.
        let mut found_existing_s_maxage = false;
        let mut updated_existing_s_maxage = false;
        for segment in &mut segments {
            let lifetime = read_lifetime_directive(segment);
            if lifetime.name == LifetimeName::SMaxAge {
                // Found existing s-maxage. If it's larger than
                // s_maxage_sec update it.
                found_existing_s_maxage = true;
                if i64::from(lifetime.seconds) > s_maxage_sec {
                    *segment = s_maxage_str.clone();
                    updated_existing_s_maxage = true;
                }
            }
        }
        if found_existing_s_maxage {
            if updated_existing_s_maxage {
                return Some(segments.join(", "));
            }
            return None;
        }
        // Didn't find s-maxage; look for max-age. It's not clear
        // how to handle multiple max-ages either. Since s-maxage
        // overrides max-age if present, and we don't want to
        // accidentally make something more cacheable, we only
        // add s-maxage if it's lower than the lowest existing
        // max-age header.
        let mut found_existing_maxage = false;
        let mut lowest_existing_maxage_value = s_maxage_sec + 1;
        for segment in &segments {
            let lifetime = read_lifetime_directive(segment);
            if lifetime.name == LifetimeName::MaxAge {
                found_existing_maxage = true;
                if i64::from(lifetime.seconds) < lowest_existing_maxage_value {
                    lowest_existing_maxage_value = i64::from(lifetime.seconds);
                }
            }
        }
        if found_existing_maxage && lowest_existing_maxage_value <= s_maxage_sec {
            return None;
        }
        Some(format!("{existing_cache_control}, {s_maxage_str}"))
    }

    /// Finds Content-Length, requiring exactly one valid value
    /// (`FindContentLength`).
    #[must_use]
    pub fn find_content_length(&self) -> Option<i64> {
        let value = self.headers.lookup1(header::CONTENT_LENGTH)?;
        value.parse::<i64>().ok()
    }

    /// Overrides the implicit cache TTL
    /// (`set_implicit_cache_ttl_ms`).
    pub fn set_implicit_cache_ttl_ms(&mut self, ttl_ms: i64) {
        self.http_options.implicit_cache_ttl_ms = ttl_ms;
        self.dirty = true;
    }

    /// Reads the implicit cache TTL (`implicit_cache_ttl_ms`).
    #[must_use]
    pub fn implicit_cache_ttl_ms(&self) -> i64 {
        self.http_options.implicit_cache_ttl_ms
    }

    /// Enables forced caching with a TTL (`ForceCaching`): positive TTLs
    /// arm the override and recompute when the fields were clean.
    pub fn force_caching(&mut self, ttl_ms: i64) {
        let recompute = !self.dirty;
        if ttl_ms > 0 {
            self.force_cache_ttl_ms = ttl_ms;
            self.dirty = true;
            if recompute {
                self.compute_caching();
            }
        }
    }

    /// Updates date/age headers after force-caching
    /// (`UpdateCacheHeadersIfForceCached`); returns whether anything
    /// changed (false when not force-cached or still dirty).
    pub fn update_cache_headers_if_force_cached(&mut self) -> bool {
        if self.dirty {
            return false;
        }
        if self.force_cached {
            let date_ms = self.date_ms();
            let ttl_ms = self.cache_ttl_ms();
            self.headers_mut().remove_all(header::PRAGMA);
            self.headers_mut().remove_all(header::CACHE_CONTROL);
            self.set_date_and_caching(date_ms, ttl_ms, "");
            self.compute_caching();
            return true;
        }
        false
    }

    /// Removes hop-by-hop headers (`Sanitize`): headers named by the
    /// `Connection` list (except `Connection` itself) plus the well-known
    /// hop-by-hop set (including cookies). Returns whether anything was
    /// removed. Caching headers (Date, ETag, Age, Content-Length) survive.
    pub fn sanitize(&mut self) -> bool {
        let mut changed = false;
        if let Some(values) = self.headers.lookup(header::CONNECTION) {
            for value in values {
                if is_hop_by_hop_indication(&value)
                    && !value.eq_ignore_ascii_case(header::CONNECTION)
                {
                    changed = self.headers_mut().remove_all(&value) || changed;
                }
            }
        }
        changed = self
            .headers_mut()
            .remove_all_from_sorted(crate::names::HOP_BY_HOP_HEADERS)
            || changed;
        changed
    }

    /// Builds a canonical link value (`RelCanonicalHeaderValue`).
    #[must_use]
    pub fn rel_canonical_header_value(url: &str) -> String {
        format!("<{}>; rel=\"canonical\"", GoogleUrl::sanitize(url))
    }

    /// Whether a Link header names a canonical URL
    /// (`HasLinkRelCanonical`: `rel` before `canonical`).
    #[must_use]
    pub fn has_link_rel_canonical(&self) -> bool {
        self.headers()
            .lookup(crate::names::header::LINK)
            .unwrap_or_default()
            .iter()
            .any(|link| {
                let rel = link.find("rel");
                let canonical = link.rfind("canonical");
                matches!((rel, canonical), (Some(rel), Some(canonical)) if rel < canonical)
            })
    }

    /// Determines the content type (`DetermineContentType`).
    #[must_use]
    pub fn determine_content_type(&self) -> Option<ContentType> {
        determine_content_type(&self.headers)
    }

    /// Determines the charset (`DetermineCharset`, needs a content type).
    #[must_use]
    pub fn determine_charset(&self) -> Option<String> {
        let mut last = None;
        for index in 0..self.headers.len() {
            if self
                .headers
                .name(index)
                .eq_ignore_ascii_case(header::CONTENT_TYPE)
            {
                last = Some(self.headers.value(index));
            }
        }
        let value = last?;
        let (_, charset) = crate::content_type::parse_content_type(value)?;
        determine_content_type(&self.headers)?;
        if charset.is_empty() {
            None
        } else {
            Some(charset)
        }
    }

    /// Merges another response's Content-Type in (`MergeContentType`).
    pub fn merge_content_type(&mut self, fresh: &ResponseHeaders) -> bool {
        let Some(original) = self.headers.lookup1(header::CONTENT_TYPE) else {
            // No original: adopt fresh verbatim (or drop).
            return match fresh.headers.lookup1(header::CONTENT_TYPE) {
                Some(value) => {
                    self.headers_mut().replace(header::CONTENT_TYPE, &value);
                    true
                }
                None => {
                    self.headers_mut().remove_all(header::CONTENT_TYPE);
                    true
                }
            };
        };
        let Some(fresh_value) = fresh.headers.lookup1(header::CONTENT_TYPE) else {
            // No fresh: adopt original verbatim (C++ early return).
            self.headers_mut().replace(header::CONTENT_TYPE, &original);
            return true;
        };
        self.combine_content_types(&original, &fresh_value)
    }

    /// Merges a Content-Type value (`MergeContentType`): rejects control
    /// bytes; with no header adopts via combine, with one merges, with
    /// several leaves everything alone. Reports whether anything changed.
    pub fn merge_content_type_str(&mut self, content_type: &str) -> bool {
        if !content_type
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte))
        {
            return false;
        }
        let count = self
            .headers
            .lookup(header::CONTENT_TYPE)
            .map(|values| values.len())
            .unwrap_or(0);
        if count < 1 {
            self.combine_content_types("", content_type)
        } else if count == 1 {
            let old = self
                .headers
                .lookup1(header::CONTENT_TYPE)
                .unwrap_or_default();
            self.combine_content_types(&old, content_type)
        } else {
            false
        }
    }

    /// Combines original/fresh content types, preferring the original
    /// (`CombineContentTypes`): unparseable originals adopt a fresh value
    /// with a MIME part; missing pieces fill from fresh (no dangling
    /// semicolon when neither side has a charset); complete originals
    /// stay untouched.
    fn combine_content_types(&mut self, original: &str, fresh: &str) -> bool {
        let Some((mime, charset)) = crate::content_type::parse_content_type(original) else {
            // No original to keep: adopt fresh verbatim, but never a bare
            // charset (`; charset=x` is not a valid Content-Type).
            return match crate::content_type::parse_content_type(fresh) {
                Some((fresh_mime, _)) if !fresh_mime.is_empty() => {
                    self.headers_mut().replace(header::CONTENT_TYPE, fresh);
                    true
                }
                _ => false,
            };
        };
        if charset.is_empty() || mime.is_empty() {
            let Some((fresh_mime, fresh_charset)) = crate::content_type::parse_content_type(fresh)
            else {
                return false;
            };
            let charset = if charset.is_empty() {
                fresh_charset
            } else {
                charset
            };
            let mime = if mime.is_empty() { fresh_mime } else { mime };
            let full = if charset.is_empty() {
                mime
            } else {
                format!("{mime}; charset={charset}")
            };
            self.headers_mut().replace(header::CONTENT_TYPE, &full);
            true
        } else {
            // Complete original: nothing to fill in.
            false
        }
    }

    /// Merges headers from another response (`UpdateFrom`).
    pub fn update_from(&mut self, fresh: &ResponseHeaders) {
        if fresh.has_status_code() {
            self.set_status_code(fresh.status_code());
            if let Some(reason) = &fresh.reason_phrase {
                self.set_reason_phrase(reason);
            }
        }
        self.merge_content_type(fresh);
        self.headers_mut().update_from(&fresh.headers);
        self.dirty = true;
    }

    /// Fixes Date/Expires/Last-Modified sanity (`FixDateHeaders`).
    pub fn fix_date_headers(&mut self, now_ms: i64) {
        // Missing, past, or far-future Dates reset to now; a present
        // Date also shifts Expires by the same delta.
        const MAX_DRIFT_MS: i64 = 3 * 60 * 1000;
        let parsed = self.parse_date_header("Date");
        let stale = parsed.is_none()
            || parsed.is_some_and(|date| date < now_ms || date > now_ms + MAX_DRIFT_MS);
        if !stale {
            return;
        }
        let recompute = !self.cache_fields_dirty();
        match parsed {
            Some(date_ms) => {
                self.set_date(now_ms);
                let delta_ms = now_ms - date_ms;
                if let Some(expires) = self.parse_date_header("Expires") {
                    self.set_time_header("Expires", expires + delta_ms);
                }
            }
            None => {
                self.set_date(now_ms);
                self.headers_mut().remove_all("Expires");
            }
        }
        if recompute {
            self.compute_caching();
        }
    }

    /// Whether the entry is inside its freshening window
    /// (`IsImminentlyExpiring`).
    #[must_use]
    pub fn is_imminently_expiring(&self, now_ms: i64) -> bool {
        // Freshen only resources living past the implicit TTL, once
        // inside the last stretch (`kRefreshExpirePercent` = 80).
        let ttl_ms = self.cache_expiration_time_ms() - self.date_ms();
        if ttl_ms < self.http_options.implicit_cache_ttl_ms {
            return false;
        }
        let threshold = self
            .http_options
            .implicit_cache_ttl_ms
            .min(20 * ttl_ms / 100);
        self.cache_expiration_time_ms() - now_ms < threshold
    }

    /// Estimated in-memory size (`SizeEstimate`).
    #[must_use]
    pub fn size_estimate(&self) -> usize {
        // C++ sizes the proto bytes; approximate: 64 + scalar fields +
        // header bytes. Used only for advisory cache sizing.
        let mut size = 64 + self.reason_phrase().len();
        for index in 0..self.headers.len() {
            size += self.headers.name(index).len() + self.headers.value(index).len();
        }
        size
    }

    /// Serializes status line + headers as HTTP text (`WriteAsHttp`).
    #[must_use]
    pub fn write_as_http(&self) -> String {
        let mut out = format!(
            "HTTP/{}.{} {} {}\r\n",
            self.major_version(),
            self.minor_version(),
            self.status_code(),
            self.reason_phrase()
        );
        out.push_str(&self.headers.write_as_http());
        out
    }

    /// Debug string (`ToString`, via `Display`).
    #[must_use]
    pub fn debug_string(&self) -> String {
        format!("{self}")
    }

    /// Serializes to protobuf-compatible bytes (`WriteAsBinary`).
    pub fn write_as_binary(&mut self) -> Vec<u8> {
        if self.dirty {
            self.compute_caching();
        }
        let proto = ResponseProto {
            status_code: self.status_code,
            reason_phrase: self.reason_phrase.clone(),
            minor_version: self.minor_version,
            major_version: self.major_version,
            expiration_time_ms: self.computed.expiration_time_ms,
            date_ms: self.computed.date_ms,
            browser_cacheable: self.computed.browser_cacheable,
            proxy_cacheable: self.computed.proxy_cacheable,
            headers: self
                .headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            last_modified_time_ms: self.computed.last_modified_time_ms,
            cache_ttl_ms: self.computed.cache_ttl_ms,
            is_implicitly_cacheable: self.computed.is_implicitly_cacheable,
            requires_browser_revalidation: self.computed.requires_browser_revalidation,
            requires_proxy_revalidation: self.computed.requires_proxy_revalidation,
        };
        encode_response(&proto)
    }

    /// Deserializes protobuf-compatible bytes (`ReadFromBinary`).
    pub fn read_from_binary(&mut self, bytes: &[u8]) -> Result<(), String> {
        let proto = decode_response(bytes)?;
        self.clear();
        self.status_code = proto.status_code;
        self.reason_phrase = proto.reason_phrase;
        self.minor_version = proto.minor_version;
        self.major_version = proto.major_version;
        for (name, value) in proto.headers {
            self.headers.add(&name, &value);
        }
        self.computed.expiration_time_ms = proto.expiration_time_ms;
        self.computed.date_ms = proto.date_ms;
        self.computed.browser_cacheable = proto.browser_cacheable;
        self.computed.proxy_cacheable = proto.proxy_cacheable;
        self.computed.last_modified_time_ms = proto.last_modified_time_ms;
        self.computed.cache_ttl_ms = proto.cache_ttl_ms;
        self.computed.is_implicitly_cacheable = proto.is_implicitly_cacheable;
        self.computed.requires_browser_revalidation = proto.requires_browser_revalidation;
        self.computed.requires_proxy_revalidation = proto.requires_proxy_revalidation;
        self.dirty = false;
        Ok(())
    }
}

impl std::fmt::Display for ResponseHeaders {
    /// Debug rendering (C++ `ToString`).
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            out,
            "HTTP/{}.{} {} {}, headers {{",
            self.major_version(),
            self.minor_version(),
            self.status_code(),
            self.reason_phrase()
        )?;
        for index in 0..self.headers.len() {
            writeln!(
                out,
                "  '{}': '{}'",
                self.headers.name(index),
                self.headers.value(index)
            )?;
        }
        out.write_str("}")
    }
}

/// Whether a Connection-listed token names a hop-by-hop header
/// (`IsHopByHopIndication`): anything that is not empty, `keep-alive`,
/// `close`, `timeout=`/`max=` prefixed, or an end-to-end header.
fn is_hop_by_hop_indication(value: &str) -> bool {
    if value.is_empty()
        || value.eq_ignore_ascii_case("keep-alive")
        || value.eq_ignore_ascii_case("close")
        || starts_with_fold(value, "timeout=")
        || starts_with_fold(value, "max=")
    {
        return false;
    }
    !crate::names::END_TO_END_HEADERS
        .binary_search_by(|probe| probe.to_ascii_lowercase().cmp(&value.to_ascii_lowercase()))
        .is_ok()
}

/// Scans a C `%d` conversion: skips ASCII whitespace, optional sign,
/// requires at least one digit (`sscanf` subset for status lines).
fn scan_int(rest: &mut &str) -> Option<i32> {
    let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let mut chars = trimmed.chars();
    let mut value: i32 = 0;
    let mut negative = false;
    match chars.next()? {
        '+' => {}
        '-' => negative = true,
        digit if digit.is_ascii_digit() => {
            value = i32::from(digit as u8 - b'0');
        }
        _ => return None,
    }
    for ch in chars {
        if !ch.is_ascii_digit() {
            break;
        }
        value = value
            .saturating_mul(10)
            .saturating_add(i32::from(ch as u8 - b'0'));
    }
    // Advance past the optional sign and the digit run.
    let mut tail = trimmed;
    if tail.starts_with(['+', '-']) {
        tail = &tail[1..];
    }
    let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    *rest = &tail[digits..];
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_200(body: &[(&str, &str)]) -> ResponseHeaders {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.set_first_line(1, 1, 200, "OK");
        response
            .headers_mut()
            .add(header::DATE, "Sun, 06 Nov 1994 08:49:37 GMT");
        for (name, value) in body {
            response.headers_mut().add(name, value);
        }
        response.compute_caching();
        response
    }

    #[test]
    fn first_line_parsing() {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.parse_first_line("HTTP/1.1 200 OK");
        assert_eq!((response.major_version(), response.minor_version()), (1, 1));
        assert_eq!(response.status_code(), 200);
        assert_eq!(response.reason_phrase(), "OK");
        // Missing reason defaults canonically.
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.parse_first_line("HTTP/1.0 404");
        assert_eq!(response.reason_phrase(), "Not Found");
        // Garbage ignored (C++ warns and ignores).
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.parse_first_line("bogus");
        assert!(!response.has_status_code());
        // Literal-dot requirement: "1 .1 200" does not parse.
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.parse_first_line("HTTP/1 .1 200 OK");
        assert!(!response.has_status_code());
        // Defaults for unset fields.
        assert_eq!(response.major_version(), 1);
        assert_eq!(response.reason_phrase(), "(null)");
        // Redirect predicate excludes 304.
        response.set_status_code(302);
        assert!(response.is_redirect_status());
        response.set_status_code(304);
        assert!(!response.is_redirect_status());
        response.set_status_code(500);
        assert!(response.is_error_status() && response.is_server_error_status());
        assert!(!response.is_redirect_status());
    }

    #[test]
    fn caching_matrix() {
        // Explicit max-age: cacheable with exact TTL.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60"),
        ]);
        assert!(response.is_browser_cacheable());
        assert_eq!(response.cache_ttl_ms(), 60_000);
        assert_eq!(
            response.cache_expiration_time_ms(),
            response.date_ms() + 60_000
        );
        assert!(!response.is_implicitly_cacheable());
        // no-cache kills caching entirely.
        let response = headers_200(&[("Cache-Control", "no-cache")]);
        assert!(!response.is_browser_cacheable());
        assert_eq!(response.cache_expiration_time_ms(), 0);
        // max-age=0 is explicit no-cache.
        let response = headers_200(&[("Cache-Control", "max-age=0")]);
        assert!(!response.is_browser_cacheable());
        // Minimum max-age wins.
        let response = headers_200(&[("Cache-Control", "max-age=60, max-age=30")]);
        assert_eq!(response.cache_ttl_ms(), 30_000);
        // Bad lifetime directives poison explicitly.
        let response = headers_200(&[("Cache-Control", "max-age=bogus")]);
        assert!(!response.is_browser_cacheable());
        // Expires-Date delta without max-age.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Expires", "Sun, 06 Nov 1994 08:51:37 GMT"),
        ]);
        assert_eq!(response.cache_ttl_ms(), 120_000);
        // s-maxage caps max-age (but no explicit max-age: s-maxage alone
        // is not an explicit lifetime for the TTL choice... it sets the
        // parse state; freshness comes from max-age first).
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=600, s-maxage=60"),
        ]);
        assert_eq!(response.cache_ttl_ms(), 60_000);
        // Heuristic: static type + validators, no explicit lifetime.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Last-Modified", "Sun, 06 Nov 1994 08:00:00 GMT"),
        ]);
        assert!(response.is_browser_cacheable());
        assert!(response.is_implicitly_cacheable());
        assert_eq!(response.cache_ttl_ms(), 300_000); // implicit default
                                                      // Heuristic refused for non-static types.
        let response = headers_200(&[("Content-Type", "text/html")]);
        assert!(!response.is_browser_cacheable());
        // Heuristic refused without a Date (browser_cacheable needs date).
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.set_first_line(1, 1, 200, "OK");
        response.headers_mut().add("Content-Type", "image/png");
        response.compute_caching();
        assert!(!response.is_browser_cacheable());
        // Redirects are likely-static (heuristic path).
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.set_first_line(1, 1, 301, "Moved");
        response
            .headers_mut()
            .add(header::DATE, "Sun, 06 Nov 1994 08:49:37 GMT");
        response.compute_caching();
        assert!(response.is_browser_cacheable());
        // Non-cacheable statuses: 404 not in the cacheable set.
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.set_first_line(1, 1, 404, "Not Found");
        response
            .headers_mut()
            .add(header::DATE, "Sun, 06 Nov 1994 08:49:37 GMT");
        response
            .headers_mut()
            .add(header::CACHE_CONTROL, "max-age=60");
        response.compute_caching();
        assert!(!response.is_browser_cacheable());
    }

    #[test]
    fn must_revalidate_blocks_heuristic() {
        // must-revalidate with no explicit lifetime: heuristic refused.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "must-revalidate"),
            ("Last-Modified", "Sun, 06 Nov 1994 08:00:00 GMT"),
        ]);
        assert!(!response.is_browser_cacheable());
        assert!(response.requires_browser_revalidation());
        assert!(response.requires_proxy_revalidation()); // OR of the two
                                                         // ...but explicit max-age still caches.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60, must-revalidate"),
        ]);
        assert!(response.is_browser_cacheable());
        assert!(response.requires_browser_revalidation());
    }

    #[test]
    fn proxy_cacheable_matrix() {
        let plain = RequestProperties {
            has_cookie: false,
            has_cookie2: false,
            has_authorization: false,
        };
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60"),
        ]);
        assert!(response.is_proxy_cacheable(
            plain,
            VaryOption::RespectVaryOnResources,
            ValidatorOption::HasValidator
        ));
        // private blocks proxy caching.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60, private"),
        ]);
        assert!(!response.is_proxy_cacheable(
            plain,
            VaryOption::RespectVaryOnResources,
            ValidatorOption::HasValidator
        ));
        // Authorization without public blocks proxy caching.
        let authed = RequestProperties {
            has_authorization: true,
            ..plain
        };
        assert!(!response.is_proxy_cacheable(
            authed,
            VaryOption::RespectVaryOnResources,
            ValidatorOption::HasValidator
        ));
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60, public"),
        ]);
        assert!(response.is_proxy_cacheable(
            authed,
            VaryOption::RespectVaryOnResources,
            ValidatorOption::HasValidator
        ));
        // Vary: Cookie blocks non-HTML outright (!html_like fails the
        // exception); HTML without cookies and with a validator passes.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60"),
            ("Vary", "Cookie"),
        ]);
        assert!(!response.is_proxy_cacheable(
            plain,
            VaryOption::IgnoreVaryOnResources,
            ValidatorOption::HasValidator
        ));
        let response = headers_200(&[
            ("Content-Type", "text/html"),
            ("Cache-Control", "max-age=60"),
            ("Vary", "Cookie"),
        ]);
        assert!(response.is_proxy_cacheable(
            plain,
            VaryOption::IgnoreVaryOnResources,
            ValidatorOption::HasValidator
        ));
        let cookied = RequestProperties {
            has_cookie: true,
            ..plain
        };
        assert!(!response.is_proxy_cacheable(
            cookied,
            VaryOption::IgnoreVaryOnResources,
            ValidatorOption::HasValidator
        ));
        // Vary: * is an explicit no-cache.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60"),
            ("Vary", "*"),
        ]);
        assert!(!response.is_browser_cacheable());
    }

    #[test]
    fn sanitize_and_force() {
        let mut response = headers_200(&[
            ("Connection", "keep-alive, X-Custom-Hop"),
            ("X-Custom-Hop", "1"),
            ("Age", "5"),
            ("Set-Cookie", "a=1"),
            ("X-Keep", "yes"),
        ]);
        assert!(response.sanitize());
        // Well-known hop-by-hop set gone (Connection itself included).
        assert!(!response.headers().has("Connection"));
        assert!(!response.headers().has("Set-Cookie"));
        // Connection-indicated custom header gone...
        assert!(!response.headers().has("X-Custom-Hop"));
        // ...but keep-alive is exempt and caching headers survive.
        assert!(response.headers().has("Age"));
        assert!(response.headers().has("Date"));
        assert!(response.headers().has("X-Keep"));
        // Second pass changes nothing.
        assert!(!response.sanitize());

        // Force-caching overrides no-cache for non-HTML 200s.
        let mut response =
            headers_200(&[("Content-Type", "image/png"), ("Cache-Control", "no-cache")]);
        assert!(!response.is_browser_cacheable());
        response.force_caching(120_000);
        response.compute_caching();
        assert!(response.is_browser_cacheable());
        assert!(response.force_cached());
        assert_eq!(response.cache_ttl_ms(), 120_000);
        assert!(response.update_cache_headers_if_force_cached());
        assert_eq!(
            response.headers().lookup_joined("Cache-Control"),
            "max-age=120"
        );
        // ...but never for HTML.
        let mut response =
            headers_200(&[("Content-Type", "text/html"), ("Cache-Control", "no-cache")]);
        response.force_caching(120_000);
        response.compute_caching();
        assert!(!response.is_browser_cacheable());
    }

    #[test]
    fn gzip_and_length_helpers() {
        let mut response = headers_200(&[]);
        // Absent encoding is fine (not "other than single gzip").
        assert!(!response.has_content_coding_other_than_single_gzip());
        response.headers_mut().add("Content-Encoding", "gzip");
        assert!(response.is_gzipped());
        assert!(response.was_gzipped_last());
        assert!(!response.has_content_coding_other_than_single_gzip());
        // First value still exactly gzip -> still "gzipped"; the quirk
        // is that only multi-codings ENDING in gzip (or non-gzip-first
        // ones) count as "other than single gzip".
        response
            .headers_mut()
            .replace("Content-Encoding", "gzip, deflate");
        assert!(response.is_gzipped());
        assert!(!response.has_content_coding_other_than_single_gzip());
        response
            .headers_mut()
            .replace("Content-Encoding", "deflate, gzip");
        assert!(!response.is_gzipped());
        assert!(response.has_content_coding_other_than_single_gzip());
        response.set_content_length(42);
        assert_eq!(response.find_content_length(), Some(42));
        response.headers_mut().add("Content-Length", "43");
        assert_eq!(response.find_content_length(), None); // several values
    }

    #[test]
    fn binary_presence_quirks() {
        // Clear() leaves proxy_cacheable/cache_ttl stale by design.
        let mut response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60"),
        ]);
        response.clear();
        // Fresh-object shape holds for the cleared fields...
        assert!(!response.is_browser_cacheable());
        assert_eq!(response.date_ms(), 0);
        // ...while proxy/ttl survive the clear.
        let bytes = response.write_as_binary();
        let mut back = ResponseHeaders::new(HttpOptions::default_for_tests());
        back.read_from_binary(&bytes).unwrap();
        assert_eq!(back.cache_ttl_ms(), 60_000);
        assert_eq!(back.computed.proxy_cacheable, Some(true));
        // Round-trip preserves computed state byte-for-byte.
        let response = headers_200(&[
            ("Content-Type", "image/png"),
            ("Cache-Control", "max-age=60"),
        ]);
        let mut response = response;
        let bytes = response.write_as_binary();
        let mut back = ResponseHeaders::new(HttpOptions::default_for_tests());
        back.read_from_binary(&bytes).unwrap();
        assert_eq!(back.write_as_binary(), bytes);
        assert_eq!(back.cache_ttl_ms(), 60_000);
        assert_eq!(back.status_code(), 200);
        // Malformed input errors instead of panicking.
        assert!(back.read_from_binary(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn combine_complete_value_unchanged() {
        // Ports CombineContentTypesCompleteValueUnchanged: a complete
        // original reports no change and stays clean.
        let mut base = headers_200(&[("Content-Type", "text/html; charset=UTF-8")]);
        assert!(!base.cache_fields_dirty());
        assert!(!base
            .combine_content_types("text/html; charset=UTF-8", "text/html; charset=ISO-8859-1"));
        assert_eq!(
            base.headers().lookup1("Content-Type"),
            Some("text/html; charset=UTF-8".to_string())
        );
        assert!(!base.cache_fields_dirty());
    }

    #[test]
    fn combine_without_charset() {
        // Ports CombineContentTypesWithoutCharset: no dangling semicolon.
        let mut base = headers_200(&[("Content-Type", "text/html")]);
        assert!(base.combine_content_types("text/html", "text/html"));
        assert_eq!(
            base.headers().lookup1("Content-Type"),
            Some("text/html".to_string())
        );
        assert!(base.cache_fields_dirty());
    }

    #[test]
    fn combine_fills_missing_charset() {
        // Ports CombineContentTypesFillsMissingCharset.
        let mut base = headers_200(&[("Content-Type", "text/html")]);
        assert!(base.combine_content_types("text/html", "; charset=UTF-8"));
        assert_eq!(
            base.headers().lookup1("Content-Type"),
            Some("text/html; charset=UTF-8".to_string())
        );
        assert!(base.cache_fields_dirty());
    }

    #[test]
    fn merge_complete_value_unchanged() {
        // Ports MergeContentTypeCompleteValueUnchanged.
        let mut base = headers_200(&[("Content-Type", "text/html; charset=UTF-8")]);
        assert!(!base.cache_fields_dirty());
        assert!(!base.merge_content_type_str("; charset=UTF-8"));
        assert!(!base.merge_content_type_str("text/html; charset=UTF-8"));
        assert_eq!(
            base.headers().lookup1("Content-Type"),
            Some("text/html; charset=UTF-8".to_string())
        );
        assert!(!base.cache_fields_dirty());
    }

    #[test]
    fn merge_update_and_http_shape() {
        let mut base = headers_200(&[("Content-Type", "text/html; charset=UTF-8")]);
        let fresh = headers_200(&[("Content-Type", "text/html; charset=UTF-16")]);
        // Complete originals report no change (C++ `CombineContentTypes`
        // returns false); the value still prefers the original charset.
        assert!(!base.merge_content_type(&fresh));
        assert_eq!(
            base.headers().lookup1("Content-Type"),
            Some("text/html; charset=UTF-8".to_string())
        );
        assert_eq!(base.determine_charset(), Some("UTF-8".to_string()));
        let text = base.write_as_http();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
        assert!(text.contains("Content-Type: text/html; charset=UTF-8\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }

    const FIXTURE_NOW_MS: i64 = 1_700_000_000_000;

    fn plain_200() -> ResponseHeaders {
        let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
        response.set_first_line(1, 1, 200, "OK");
        response
            .headers_mut()
            .add("Content-Type", "text/html; charset=UTF-8");
        response
    }

    #[test]
    fn fixup_missing_date() {
        let mut headers = plain_200();
        headers.fix_date_headers(FIXTURE_NOW_MS);
        headers.compute_caching();
        assert_eq!(headers.date_ms(), FIXTURE_NOW_MS);
        assert!(headers.headers().lookup1("Expires").is_none());
    }

    #[test]
    fn do_not_correct_valid_date() {
        let mut headers = plain_200();
        headers.set_date(FIXTURE_NOW_MS);
        headers.compute_caching();
        // A clock set back a second leaves the date alone.
        headers.fix_date_headers(FIXTURE_NOW_MS - 1000);
        assert_eq!(headers.date_ms(), FIXTURE_NOW_MS);
        assert!(headers.headers().lookup1("Expires").is_none());
    }

    #[test]
    fn fixup_stale_date() {
        let mut headers = plain_200();
        headers.set_date(FIXTURE_NOW_MS);
        headers.compute_caching();
        // A clock set forward a second rewrites the date.
        headers.fix_date_headers(FIXTURE_NOW_MS + 1000);
        assert_eq!(headers.date_ms(), FIXTURE_NOW_MS + 1000);
        assert!(headers.headers().lookup1("Expires").is_none());
    }

    #[test]
    fn fixup_stale_date_with_expires() {
        let mut headers = plain_200();
        headers.set_date(FIXTURE_NOW_MS);
        headers.set_time_header("Expires", FIXTURE_NOW_MS + 5 * 60 * 1000);
        headers.compute_caching();
        assert_eq!(
            headers.cache_expiration_time_ms(),
            FIXTURE_NOW_MS + 5 * 60 * 1000
        );
        // The date moves and Expires shifts by the same delta.
        headers.fix_date_headers(FIXTURE_NOW_MS + 1000);
        assert_eq!(headers.date_ms(), FIXTURE_NOW_MS + 1000);
        assert!(headers.headers().lookup1("Expires").is_some());
        assert_eq!(
            headers.cache_expiration_time_ms(),
            FIXTURE_NOW_MS + 1000 + 5 * 60 * 1000
        );
    }

    #[test]
    fn fixup_stale_date_with_max_age() {
        let mut headers = plain_200();
        headers.set_date(FIXTURE_NOW_MS);
        headers.headers_mut().add("Cache-Control", "max-age=300");
        headers.compute_caching();
        assert_eq!(
            headers.cache_expiration_time_ms(),
            FIXTURE_NOW_MS + 5 * 60 * 1000
        );
        assert!(headers.headers().lookup1("Expires").is_none());
        headers.fix_date_headers(FIXTURE_NOW_MS + 1000);
        assert_eq!(headers.date_ms(), FIXTURE_NOW_MS + 1000);
        // Still no Expires entry, but expiry tracks the new date.
        assert!(headers.headers().lookup1("Expires").is_none());
        assert_eq!(
            headers.cache_expiration_time_ms(),
            FIXTURE_NOW_MS + 1000 + 5 * 60 * 1000
        );
    }

    #[test]
    fn missing_date_removes_expires() {
        let mut headers = plain_200();
        headers.set_time_header("Expires", FIXTURE_NOW_MS + 5 * 60 * 1000);
        headers.compute_caching();
        assert!(headers.headers().lookup1("Date").is_none());
        assert!(headers.headers().lookup1("Expires").is_some());
        headers.fix_date_headers(FIXTURE_NOW_MS);
        assert!(headers.headers().lookup1("Date").is_some());
        assert!(headers.headers().lookup1("Expires").is_none());
    }

    #[test]
    fn imminence_window() {
        // Entries living past the implicit TTL (5min here) freshen
        // inside their final stretch (20% of a 1000s TTL = 200s).
        let mut headers = plain_200();
        headers.set_date_and_caching(FIXTURE_NOW_MS, 1000 * 1000, "");
        headers.compute_caching();
        assert!(!headers.is_imminently_expiring(FIXTURE_NOW_MS));
        assert!(!headers.is_imminently_expiring(FIXTURE_NOW_MS + 799 * 1000));
        assert!(headers.is_imminently_expiring(FIXTURE_NOW_MS + 900 * 1000));
        // Short-lived entries never freshen.
        let mut brief = plain_200();
        brief.set_date_and_caching(FIXTURE_NOW_MS, 60 * 1000, "");
        brief.compute_caching();
        assert!(!brief.is_imminently_expiring(FIXTURE_NOW_MS + 59 * 1000));
    }

    #[test]
    fn caching_writers_match_cxx() {
        fn cache_control(headers: &ResponseHeaders) -> String {
            headers.headers().lookup_joined(header::CACHE_CONTROL)
        }
        // Raw stored line (lookup_joined would re-split comma fields).
        fn cache_control_raw(headers: &ResponseHeaders) -> String {
            headers
                .headers()
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(header::CACHE_CONTROL))
                .map(|(_, value)| value.to_string())
                .unwrap_or_default()
        }
        // SetDateAndCaching: Expires always stamps (even at ttl 0)
        // and the suffix appends verbatim.
        let mut headers = plain_200();
        headers.set_date_and_caching(FIXTURE_NOW_MS, 0, ", no-cache");
        assert_eq!("max-age=0, no-cache", cache_control(&headers));
        assert_eq!(
            headers.headers().lookup1(header::DATE),
            headers.headers().lookup1(header::EXPIRES)
        );
        let mut headers = plain_200();
        headers.set_date_and_caching(FIXTURE_NOW_MS, 60_000, "");
        assert_eq!("max-age=60", cache_control(&headers));
        // CacheControlValuesToPreserve: no-transform/no-store plus
        // standalone s-maxage values, `, `-prefixed.
        let headers = headers_200(&[("Cache-Control", "no-store, s-maxage=10")]);
        assert_eq!(
            ", no-store, s-maxage=10",
            headers.cache_control_values_to_preserve()
        );
        let headers = headers_200(&[("Cache-Control", "s-maxage=10")]);
        assert_eq!(", s-maxage=10", headers.cache_control_values_to_preserve());
        let headers = headers_200(&[("Cache-Control", "max-age=60")]);
        assert!(headers.cache_control_values_to_preserve().is_empty());
        // SetCacheControlMaxAge takes ms, keeps non-max-age values
        // `,`-joined, and recomputes when clean.
        let mut headers = headers_200(&[("Cache-Control", "public")]);
        headers.set_cache_control_max_age(300_000);
        assert_eq!("max-age=300,public", cache_control_raw(&headers));
        assert_eq!(300_000, headers.cache_ttl_ms());
    }
}
