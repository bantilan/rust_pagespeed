//! HTTP cache (`http_cache.{h,cc}`, `http_cache_failure.{h,cc}`).
//!
//! Synchronous port of `HTTPCache`: composite `v5/` keys, remembered
//! failures, force caching, gzip-on-store, ETag injection, and the
//! validity/fallback serving rules. Async callbacks collapse into direct
//! return values; statistics live in owned counters mirroring the
//! `cache_*` variables.
#![forbid(unsafe_code)]

use crate::fetch::{gzip_value, ungzip_value_if_compressed};
use crate::names::header;
use crate::request::RequestContext;
use crate::response::{
    HttpOptions, RequestProperties, ResponseHeaders, ValidatorOption, VaryOption,
};
use crate::value::HttpValue;
use ps_cache::cache::{Cache, KeyState, Lookup};
use ps_cache::hash::{Hasher, Md5Hasher};
use ps_cache::ttl::{FailurePolicy, FetchStatus};
use std::sync::Arc;

/// ETag prefix for cache-generated tags (`kEtagPrefix`).
pub const ETAG_PREFIX: &str = "W/\"PSA-";
/// Unlimited body size (`kCacheSizeUnlimited`).
pub const SIZE_UNLIMITED: i64 = -1;

/// Find outcome (`HTTPCache::FindResultStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindStatus {
    /// Fresh entry served.
    Found,
    /// Entry missing or unusable.
    NotFound,
    /// Fresh remembered failure served.
    RecentFailure,
}

/// Find result: status plus an optional entry/failure.
#[derive(Debug, Clone)]
pub struct FindResult {
    /// Outcome.
    pub status: FindStatus,
    /// Remembered failure status (for `RecentFailure`).
    pub failure: FetchStatus,
    /// Served headers (for `Found`).
    pub headers: Option<ResponseHeaders>,
    /// Served body (for `Found`).
    pub body: Vec<u8>,
    /// Stale-but-usable fallback, when the backend entry expired or the
    /// request asked to bypass freshness.
    pub fallback_headers: Option<ResponseHeaders>,
    /// Fallback body.
    pub fallback_body: Vec<u8>,
}

/// Owned HTTP-cache counters (the `cache_*` variables).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpCacheStats {
    /// Backend + (de)serialization latency, microseconds.
    pub time_us: i64,
    /// Fresh hits.
    pub hits: u64,
    /// Misses (including failures served as misses).
    pub misses: u64,
    /// Backend hits.
    pub backend_hits: u64,
    /// Backend misses.
    pub backend_misses: u64,
    /// Stale fallbacks attached.
    pub fallbacks: u64,
    /// Expirations observed.
    pub expirations: u64,
    /// Entries written.
    pub inserts: u64,
    /// Entries deleted.
    pub deletes: u64,
}

/// HTTP cache (`HTTPCache`).
pub struct HttpCache {
    backend: Arc<dyn Cache>,
    hasher: Arc<dyn Hasher>,
    stats: std::sync::Mutex<HttpCacheStats>,
    version_prefix: String,
    force_caching: bool,
    disable_html_caching_on_https: bool,
    ignore_failure_puts: bool,
    cache_levels: i32,
    compression_level: i32,
    max_body_bytes: std::sync::atomic::AtomicI64,
    failure_policy: FailurePolicy,
}

impl std::fmt::Debug for HttpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCache")
            .field("name", &self.name())
            .field("stats", &self.stats())
            .finish()
    }
}

impl HttpCache {
    /// Creates a cache over a backend (ctor defaults: version 5,
    /// compression off (0 = unzip mode), unlimited body size).
    #[must_use]
    pub fn new(backend: Arc<dyn Cache>) -> HttpCache {
        HttpCache {
            backend,
            hasher: Arc::new(Md5Hasher::new()),
            stats: std::sync::Mutex::new(HttpCacheStats::default()),
            version_prefix: ps_cache::keys::http_version_prefix(),
            force_caching: false,
            disable_html_caching_on_https: false,
            ignore_failure_puts: false,
            cache_levels: 1,
            compression_level: 0,
            max_body_bytes: std::sync::atomic::AtomicI64::new(SIZE_UNLIMITED),
            failure_policy: FailurePolicy::new(),
        }
    }

    /// Swaps the content hasher (`set_hasher`).
    pub fn set_hasher(&mut self, hasher: Arc<dyn Hasher>) {
        self.hasher = hasher;
    }

    /// Counter snapshot.
    #[must_use]
    pub fn stats(&self) -> HttpCacheStats {
        *lock(&self.stats)
    }

    /// Clears counters but keeps entries (`ClearStats`).
    pub fn clear_stats(&self) {
        *lock(&self.stats) = HttpCacheStats::default();
    }

    /// Backend name (`FormatName`).
    #[must_use]
    pub fn name(&self) -> String {
        format!("HTTPCache({})", self.backend.name())
    }

    /// Sets the key version (`SetVersion`).
    pub fn set_version(&mut self, version: u32) {
        self.version_prefix = format!("v{version}/");
    }

    /// Enables force caching.
    pub fn set_force_caching(&mut self, force: bool) {
        self.force_caching = force;
    }

    /// Whether caching is forced (`force_caching`).
    #[must_use]
    pub fn force_caching(&self) -> bool {
        self.force_caching
    }

    /// Failure TTL in seconds (`failure_caching_ttl_sec`).
    #[must_use]
    pub fn failure_caching_ttl_sec(&self, status: FetchStatus) -> i32 {
        self.failure_policy.ttl_sec(status)
    }

    /// Disables HTML caching on HTTPS origins.
    pub fn set_disable_html_caching_on_https(&mut self, disable: bool) {
        self.disable_html_caching_on_https = disable;
    }

    /// Drops failure puts (`SetIgnoreFailurePuts`, test hook).
    pub fn set_ignore_failure_puts(&mut self) {
        self.ignore_failure_puts = true;
    }

    /// Sets the cache level count (multi-level freshness checks).
    pub fn set_cache_levels(&mut self, levels: i32) {
        self.cache_levels = levels;
    }

    /// Sets the gzip-on-store level (`SetCompressionLevel`; 0 unzips).
    pub fn set_compression_level(&mut self, level: i32) {
        self.compression_level = level;
    }

    /// Sets the body-size cap (`set_max_cacheable_response_content_length`;
    /// negative values other than -1 ignored, as in C++).
    pub fn set_max_body_bytes(&self, max: i64) {
        if max >= SIZE_UNLIMITED {
            self.max_body_bytes
                .store(max, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Replaces the failure policy.
    pub fn set_failure_policy(&mut self, policy: FailurePolicy) {
        self.failure_policy = policy;
    }

    /// Builds a composite key (`CompositeKey`).
    #[must_use]
    pub fn composite_key(&self, key: &str, fragment: &str) -> String {
        if fragment.is_empty() {
            format!("{}{key}", self.version_prefix)
        } else {
            format!("{}{fragment}/{key}", self.version_prefix)
        }
    }

    /// Formats a cache-generated ETag (`FormatEtag`): `W/"PSA-<hash>"`.
    #[must_use]
    pub fn format_etag(&self, content: &[u8]) -> String {
        format!("{ETAG_PREFIX}{}\"", self.hasher.hash(content))
    }

    /// Whether a body size is cacheable (`IsCacheableBodySize`).
    #[must_use]
    pub fn is_cacheable_body_size(&self, size: i64) -> bool {
        let max = self
            .max_body_bytes
            .load(std::sync::atomic::Ordering::Relaxed);
        max == SIZE_UNLIMITED || size <= max
    }

    /// Whether content-length permits caching (`IsCacheableContentLength`).
    #[must_use]
    pub fn is_cacheable_content_length(&self, headers: &ResponseHeaders) -> bool {
        match headers.find_content_length() {
            Some(length) => self.is_cacheable_body_size(length),
            None => true,
        }
    }

    /// Whether the URL may be cached (`MayCacheUrl`): any valid URL,
    /// minus HTTPS HTML when disabled.
    #[must_use]
    pub fn may_cache_url(&self, url: &str, headers: &ResponseHeaders) -> bool {
        if !is_any_valid_url(url) {
            return false;
        }
        if self.disable_html_caching_on_https && url.starts_with("https://") {
            let html_like = headers
                .determine_content_type()
                .is_some_and(|content| content.is_html_like());
            return !html_like;
        }
        true
    }

    /// Whether headers are expired (`IsExpired`; force caching never is).
    #[must_use]
    pub fn is_expired(&self, headers: &ResponseHeaders, now_ms: i64) -> bool {
        if self.force_caching {
            return false;
        }
        headers.cache_expiration_time_ms() <= now_ms
    }

    /// Looks up an entry (`Find` + `HTTPCacheCallback::ValidateCandidate`).
    ///
    /// `is_fresh` decides freshness beyond TTL (multi-level caches pass a
    /// stricter check); `override_ttl_ms` forces revalidation windows.
    #[allow(clippy::too_many_arguments)]
    pub fn find(
        &self,
        key: &str,
        fragment: &str,
        req: RequestProperties,
        vary: VaryOption,
        is_fresh: &dyn Fn(&ResponseHeaders) -> bool,
        override_ttl_ms: i64,
        request: &mut RequestContext,
        now_ms: i64,
    ) -> FindResult {
        let start = std::time::Instant::now();
        let accepts_gzip = request.accepts_gzip();
        let composite = self.composite_key(key, fragment);
        let lookup = self.backend.get(&composite);
        let backend_available = lookup.state == KeyState::Available;
        {
            let mut stats = lock(&self.stats);
            if backend_available {
                stats.backend_hits += 1;
            } else {
                stats.backend_misses += 1;
            }
        }
        let mut result = FindResult {
            status: FindStatus::NotFound,
            failure: FetchStatus::NotSet,
            headers: None,
            body: Vec::new(),
            fallback_headers: None,
            fallback_body: Vec::new(),
        };
        let mut is_expired = false;
        if let Lookup {
            state: KeyState::Available,
            value: Some(blob),
        } = lookup
        {
            let mut value = HttpValue::new();
            if value.decode(&blob).is_ok() {
                // ValidateCandidate gate chain.
                let mut headers = value.headers().clone();
                let proxy_ok = self.force_caching
                    || headers.is_proxy_cacheable(req, vary, ValidatorOption::HasValidator);
                let mut sanitized = headers.clone();
                let unsanitary = sanitized.sanitize();
                let bad_coding = headers.has_content_coding_other_than_single_gzip();
                if proxy_ok && !unsanitary && !bad_coding {
                    if override_ttl_ms > 0 {
                        headers.force_caching(override_ttl_ms);
                        headers.compute_caching();
                    }
                    is_expired = self.is_expired(&headers, now_ms);
                    let fresh = !is_expired && is_fresh(&headers);
                    let status = headers.status_code();
                    if is_failure_status(status) {
                        let mut valid = fresh;
                        if override_ttl_ms > 0 && status == 10003 {
                            valid = false;
                        }
                        if valid {
                            result.status = FindStatus::RecentFailure;
                            result.failure = decode_failure_status(status);
                        }
                    } else if fresh {
                        result.status = FindStatus::Found;
                        if headers.update_cache_headers_if_force_cached() {
                            // Rebuild the value with the new headers.
                            let body = value.body().to_vec();
                            value = HttpValue::new();
                            value.set_headers(headers.clone());
                            value.write_body(&body);
                        }
                        result.headers = Some(headers.clone());
                        result.body = value.body().to_vec();
                    } else if self.force_caching
                        || headers.is_proxy_cacheable(req, vary, ValidatorOption::HasValidator)
                    {
                        // Attach a stale fallback, unzipped when the
                        // client cannot accept gzip.
                        let mut fallback_headers = headers.clone();
                        let mut fallback_body = value.body().to_vec();
                        if !accepts_gzip
                            && ungzip_value_if_compressed(&mut fallback_headers, &mut fallback_body)
                        {
                            // Ungzipped in place.
                        }
                        result.fallback_headers = Some(fallback_headers);
                        result.fallback_body = fallback_body;
                    }
                }
            }
        }
        {
            let mut stats = lock(&self.stats);
            match result.status {
                FindStatus::Found => stats.hits += 1,
                _ => {
                    stats.misses += 1;
                    if result.fallback_headers.is_some() {
                        stats.fallbacks += 1;
                    }
                    if is_expired {
                        stats.expirations += 1;
                    }
                }
            }
        }
        // Ungzip served entries for clients that cannot accept gzip.
        if result.status == FindStatus::Found && !accepts_gzip {
            if let (Some(headers), body) = (result.headers.as_mut(), &mut result.body) {
                let _ = ungzip_value_if_compressed(headers, body);
            }
        }
        if result.status != FindStatus::Found {
            result.headers = None;
            result.body.clear();
        }
        // Single-level latency accounting (`ReportLatencyMs` at level 1).
        let elapsed_us = start.elapsed().as_micros() as i64;
        lock(&self.stats).time_us += elapsed_us;
        request.set_cache_lookup_ms(elapsed_us / 1000);
        result
    }

    /// Stores headers + body (`Put`, headers/content form).
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &self,
        key: &str,
        fragment: &str,
        req: RequestProperties,
        vary: VaryOption,
        headers: &mut ResponseHeaders,
        body: &[u8],
        now_ms: i64,
    ) -> bool {
        // NOTE: no status-range gate here: magic failure statuses (10001+)
        // must pass through Put, exactly as in C++.
        if !self.may_cache_url(key, headers) {
            return false;
        }
        headers.compute_caching();
        if (self.is_expired(headers, now_ms)
            || !headers.is_proxy_cacheable(req, vary, ValidatorOption::HasValidator)
            || !self.is_cacheable_body_size(body.len() as i64))
            && !self.force_caching
        {
            return false;
        }
        let Some((mut value, mut stored_headers)) = self.apply_header_changes(headers, Some(body))
        else {
            return false;
        };
        self.put_internal(key, fragment, &mut value, &mut stored_headers, true, now_ms);
        lock(&self.stats).inserts += 1;
        true
    }

    /// Stores a prebuilt value (`Put`, HTTPValue form).
    pub fn put_value(
        &self,
        key: &str,
        fragment: &str,
        req: RequestProperties,
        options: HttpOptions,
        value: &HttpValue,
        now_ms: i64,
    ) -> bool {
        let mut headers = value.headers().clone();
        let _ = options;
        if !self.may_cache_url(key, &headers) {
            return false;
        }
        headers.compute_caching();
        if !self.force_caching
            && !(headers.is_proxy_cacheable(
                req,
                VaryOption::RespectVaryOnResources,
                ValidatorOption::HasValidator,
            ) && self.is_cacheable_body_size(value.body().len() as i64))
        {
            return false;
        }
        let Some((mut stored, mut stored_headers)) =
            self.apply_header_changes_from_value(&headers, value)
        else {
            return false;
        };
        self.put_internal(
            key,
            fragment,
            &mut stored,
            &mut stored_headers,
            false,
            now_ms,
        );
        lock(&self.stats).inserts += 1;
        true
    }

    /// Sanitizes + injects ETag (`ApplyHeaderChangesForPut`, content form).
    /// Returns the value to store, or `None` to skip the write.
    fn apply_header_changes(
        &self,
        headers: &mut ResponseHeaders,
        body: Option<&[u8]>,
    ) -> Option<(HttpValue, ResponseHeaders)> {
        if headers.status_code() != 200 && self.ignore_failure_puts {
            return None;
        }
        let body = body?;
        let mut stored = headers.clone();
        stored.sanitize();
        if stored.headers().lookup1(header::ETAG).is_none() {
            let etag = self.format_etag(body);
            stored.headers_mut().add(header::ETAG, &etag);
        }
        let mut value = HttpValue::new();
        value.set_headers(stored.clone());
        value.write_body(body);
        Some((value, stored))
    }

    /// Sanitizes + injects ETag (`ApplyHeaderChangesForPut`, value form).
    fn apply_header_changes_from_value(
        &self,
        headers: &ResponseHeaders,
        value: &HttpValue,
    ) -> Option<(HttpValue, ResponseHeaders)> {
        if headers.status_code() != 200 && self.ignore_failure_puts {
            return None;
        }
        let mut stored = headers.clone();
        let mutated = stored.sanitize();
        let mut out = HttpValue::new();
        let body = value.body().to_vec();
        if stored.headers().lookup1(header::ETAG).is_none() {
            let etag = self.format_etag(&body);
            stored.headers_mut().add(header::ETAG, &etag);
            out.set_headers(stored.clone());
            out.write_body(&body);
            return Some((out, stored));
        }
        if mutated {
            out.set_headers(stored.clone());
            out.write_body(&body);
            return Some((out, stored));
        }
        // Nothing changed: store the input value as-is.
        Some((value.clone(), stored))
    }

    /// Compresses (or unzips) and writes (`PutInternal`).
    fn put_internal(
        &self,
        key: &str,
        fragment: &str,
        value: &mut HttpValue,
        headers: &mut ResponseHeaders,
        _preserve_response_headers: bool,
        _now_ms: i64,
    ) {
        // Gzip compressible, non-empty, uncompressed bodies (level != 0);
        // unzip stored gzip when the level is 0. Mutation happens on the
        // stored copies; the caller's headers stay intact either way.
        if !value.body().is_empty() && self.compression_level != 0 {
            let compressible = headers
                .determine_content_type()
                .is_some_and(|content| content.is_compressible());
            if compressible && !headers.is_gzipped() {
                // Canonicalize the marker order (x-original last).
                if let Some(marker) = headers.headers().lookup1(header::X_ORIGINAL_CONTENT_LENGTH) {
                    headers
                        .headers_mut()
                        .remove_all(header::X_ORIGINAL_CONTENT_LENGTH);
                    headers
                        .headers_mut()
                        .add(header::X_ORIGINAL_CONTENT_LENGTH, &marker);
                }
                headers.compute_caching();
                let mut body = value.body().to_vec();
                if gzip_value(headers, &mut body, self.compression_level) {
                    value.set_headers(headers.clone());
                    let mut fresh = HttpValue::new();
                    fresh.set_headers(headers.clone());
                    fresh.write_body(&body);
                    *value = fresh;
                }
            }
        } else if self.compression_level == 0 && headers.is_gzipped() {
            let mut body = value.body().to_vec();
            if ungzip_value_if_compressed(headers, &mut body) {
                let mut fresh = HttpValue::new();
                fresh.set_headers(headers.clone());
                fresh.write_body(&body);
                *value = fresh;
            }
        }
        self.backend
            .put(&self.composite_key(key, fragment), &value.encode());
    }

    /// Deletes an entry (`Delete`).
    pub fn delete(&self, key: &str, fragment: &str) {
        lock(&self.stats).deletes += 1;
        self.backend.delete(&self.composite_key(key, fragment));
    }

    /// Remembers a fetch failure (`RememberFailure`): stores a magic
    /// status with the policy TTL through the regular gated `Put` path.
    pub fn remember_failure(
        &self,
        key: &str,
        fragment: &str,
        failure: FetchStatus,
        now_ms: i64,
    ) -> bool {
        let code = encode_failure_status(failure);
        let ttl_ms = i64::from(self.failure_policy.ttl_sec(failure)) * 1000;
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_status_code(code);
        headers.set_date_and_caching(now_ms, ttl_ms, "");
        headers.compute_caching();
        self.put(
            key,
            fragment,
            RequestProperties::default(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            &[],
            now_ms,
        )
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// Minimal URL validity for caching (`GoogleUrl::IsAnyValid` subset):
/// non-empty with a scheme separator (the `gfnt:` scheme stays allowed).
fn is_any_valid_url(url: &str) -> bool {
    !url.is_empty() && url.contains("://") || url.starts_with("gfnt:")
}

/// Whether a status is a remembered failure (`IsFailureCachingStatus`).
#[must_use]
pub fn is_failure_status(status: i32) -> bool {
    (10001..10007).contains(&status)
}

/// Classifies a fetch outcome (`HttpCacheFailure::ClassifyFailure`).
#[must_use]
pub fn classify_failure(
    headers: &ResponseHeaders,
    contents: &[u8],
    fetch_success: bool,
    cacheable: bool,
) -> FetchStatus {
    let status = headers.status_code();
    if fetch_success && !headers.is_error_status() {
        if contents.is_empty() && !headers.is_redirect_status() {
            // Empty 200s are remembered, not cached.
            FetchStatus::Empty
        } else if !cacheable {
            if status == 200 {
                FetchStatus::Uncacheable200
            } else {
                FetchStatus::UncacheableError
            }
        } else if status == 200 {
            FetchStatus::Ok
        } else {
            FetchStatus::OtherError
        }
    } else if headers.headers().has(crate::names::header::X_PSA_LOAD_SHED) {
        FetchStatus::Dropped
    } else if (400..500).contains(&status) {
        FetchStatus::Error4xx
    } else {
        FetchStatus::OtherError
    }
}

/// Maps a failure to its magic status (`EncodeFailureCachingStatus`).
#[must_use]
pub const fn encode_failure_status(failure: FetchStatus) -> i32 {
    match failure {
        FetchStatus::NotSet => 10001, // unused; mirrors table order
        FetchStatus::Ok => 200,
        FetchStatus::Uncacheable200 => 10003,
        FetchStatus::UncacheableError => 10002,
        FetchStatus::Error4xx => 10004,
        FetchStatus::OtherError => 10001,
        FetchStatus::Dropped => 10006,
        FetchStatus::Empty => 10005,
    }
}

/// Maps a magic status back (`DecodeFailureCachingStatus`).
#[must_use]
pub const fn decode_failure_status(status: i32) -> FetchStatus {
    match status {
        10001 => FetchStatus::OtherError,
        10002 => FetchStatus::UncacheableError,
        10003 => FetchStatus::Uncacheable200,
        10004 => FetchStatus::Error4xx,
        10005 => FetchStatus::Empty,
        10006 => FetchStatus::Dropped,
        _ => FetchStatus::NotSet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::cache::MemoryCache;

    const NOW: i64 = 1_700_000_000_000;

    fn cache() -> HttpCache {
        HttpCache::new(Arc::new(MemoryCache::new()))
    }

    fn fresh() -> Box<dyn Fn(&ResponseHeaders) -> bool> {
        Box::new(|_| true)
    }

    fn plain_req() -> RequestProperties {
        RequestProperties {
            has_cookie: false,
            has_cookie2: false,
            has_authorization: false,
        }
    }

    fn context() -> RequestContext {
        let mut context = RequestContext::new(HttpOptions::default_for_tests());
        context.set_accepts_gzip(true);
        context
    }

    fn cacheable_png(ttl_ms: i64) -> ResponseHeaders {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.set_date(NOW);
        headers.headers_mut().add(header::CONTENT_TYPE, "image/png");
        headers
            .headers_mut()
            .add(header::CACHE_CONTROL, &format!("max-age={}", ttl_ms / 1000));
        headers
    }

    #[test]
    fn put_find_round_trip_and_stats() {
        let cache = cache();
        let mut headers = cacheable_png(60_000);
        assert!(cache.put(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"PNGDATA",
            NOW
        ));
        assert_eq!(cache.stats().inserts, 1);
        let mut context = context();
        let result = cache.find(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW + 1000,
        );
        assert_eq!(result.status, FindStatus::Found);
        assert_eq!(result.body, b"PNGDATA");
        assert_eq!(result.headers.unwrap().status_code(), 200);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().backend_hits, 1);
        // Miss on unknown keys.
        let result = cache.find(
            "http://x/missing",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW,
        );
        assert_eq!(result.status, FindStatus::NotFound);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().backend_misses, 1);
        // Delete removes.
        cache.delete("http://x/a.png", "");
        assert_eq!(cache.stats().deletes, 1);
        let result = cache.find(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW,
        );
        assert_eq!(result.status, FindStatus::NotFound);
    }

    #[test]
    fn etag_injection_and_sanitize_on_store() {
        let cache = cache();
        let mut headers = cacheable_png(60_000);
        headers.headers_mut().add(header::SET_COOKIE, "a=1");
        assert!(cache.put(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"PNGDATA",
            NOW
        ));
        let mut context = context();
        let result = cache.find(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW,
        );
        let stored = result.headers.unwrap();
        // Cookie stripped, generated ETag present.
        assert!(!stored.headers().has(header::SET_COOKIE));
        let etag = stored.headers().lookup1(header::ETAG).unwrap();
        assert!(
            etag.starts_with("W/\"PSA-") && etag.ends_with('"'),
            "{etag}"
        );
        // Caller's headers untouched by the store path.
        assert!(headers.headers().has(header::SET_COOKIE));
    }

    #[test]
    fn expiry_fallback_and_failure_memory() {
        let cache = cache();
        let mut headers = cacheable_png(1_000);
        assert!(cache.put(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"PNGDATA",
            NOW
        ));
        // Past expiry: miss with a stale fallback attached.
        let mut context = context();
        let result = cache.find(
            "http://x/a.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW + 60_000,
        );
        assert_eq!(result.status, FindStatus::NotFound);
        assert_eq!(result.fallback_body, b"PNGDATA");
        assert_eq!(cache.stats().expirations, 1);
        assert_eq!(cache.stats().fallbacks, 1);
        // Remembered failures serve as RecentFailure within TTL...
        assert!(cache.remember_failure("http://x/down", "", FetchStatus::OtherError, NOW));
        let result = cache.find(
            "http://x/down",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW + 1000,
        );
        assert_eq!(result.status, FindStatus::RecentFailure);
        assert_eq!(result.failure, FetchStatus::OtherError);
        // ...and expire into misses past the policy TTL (300s).
        let result = cache.find(
            "http://x/down",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW + 301_000,
        );
        assert_eq!(result.status, FindStatus::NotFound);
        // Failure status codec round-trips.
        for status in [
            FetchStatus::OtherError,
            FetchStatus::UncacheableError,
            FetchStatus::Uncacheable200,
            FetchStatus::Error4xx,
            FetchStatus::Empty,
            FetchStatus::Dropped,
        ] {
            assert!(is_failure_status(encode_failure_status(status)));
            assert_eq!(decode_failure_status(encode_failure_status(status)), status);
        }
        assert_eq!(encode_failure_status(FetchStatus::OtherError), 10001);
        assert_eq!(encode_failure_status(FetchStatus::Uncacheable200), 10003);
    }

    #[test]
    fn compression_and_gzip_negotiation() {
        let mut cache = cache();
        cache.set_compression_level(6);
        let mut headers = cacheable_png(60_000);
        headers
            .headers_mut()
            .replace(header::CONTENT_TYPE, "text/css");
        let body = b".a{color:red}".repeat(100);
        assert!(cache.put(
            "http://x/a.css",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            &body,
            NOW
        ));
        // Gzip-capable client: served gzipped.
        let mut context = context();
        let result = cache.find(
            "http://x/a.css",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW,
        );
        assert_eq!(result.status, FindStatus::Found);
        assert!(result.headers.as_ref().unwrap().is_gzipped());
        assert!(result.body.len() < body.len());
        // Plain client: ungzipped on serve.
        let mut plain = RequestContext::new(HttpOptions::default_for_tests());
        let result = cache.find(
            "http://x/a.css",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut plain,
            NOW,
        );
        assert_eq!(result.body, body);
        assert!(!result.headers.as_ref().unwrap().is_gzipped());
    }

    #[test]
    fn keys_versions_fragments_and_gates() {
        let mut cache = cache();
        assert_eq!(cache.composite_key("k", ""), "v5/k");
        assert_eq!(cache.composite_key("k", "frag"), "v5/frag/k");
        cache.set_version(7);
        assert_eq!(cache.composite_key("k", ""), "v7/k");
        assert_eq!(cache.name(), "HTTPCache(MemoryCache)");
        // Fragments partition entries.
        let mut headers = cacheable_png(60_000);
        assert!(cache.put(
            "http://x/a.png",
            "frag-a",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"A",
            NOW
        ));
        let mut context = context();
        let result = cache.find(
            "http://x/a.png",
            "frag-b",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &fresh(),
            0,
            &mut context,
            NOW,
        );
        assert_eq!(result.status, FindStatus::NotFound);
        // Uncacheable bodies refused (private + oversize).
        let mut headers = cacheable_png(60_000);
        headers
            .headers_mut()
            .replace(header::CACHE_CONTROL, "max-age=60, private");
        assert!(!cache.put(
            "http://x/p.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"x",
            NOW
        ));
        cache.set_max_body_bytes(4);
        let mut headers = cacheable_png(60_000);
        assert!(!cache.put(
            "http://x/big.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"12345",
            NOW
        ));
        // Negative limits other than -1 ignored.
        cache.set_max_body_bytes(-5);
        let mut headers = cacheable_png(60_000);
        assert!(!cache.put(
            "http://x/big2.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"12345",
            NOW
        ));
        // Force caching overrides the gates.
        cache.set_force_caching(true);
        assert!(cache.put(
            "http://x/big2.png",
            "",
            plain_req(),
            VaryOption::RespectVaryOnResources,
            &mut headers,
            b"12345",
            NOW
        ));
    }
}
