//! Cache-backed resources (`CacheableResourceBase`, sync completion).
//!
//! Loads through the HTTP cache, fetching on miss and remembering
//! failures; stale entries freshen in the background. The C++
//! callback/lock machinery collapses here: fetchers complete inline
//! (see [`AsyncFetcher`](ps_http::fetch::AsyncFetcher)), so locks,
//! redundant-fetch yielding, and async event counts are mute points,
//! noted where they would hook in.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::ttl::FetchStatus;
use ps_http::content_type::{extension_to_content_type, ContentType};
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::google_url::GoogleUrl;
use ps_http::http_cache::{classify_failure, FindResult, FindStatus, HttpCache};
use ps_http::names::header;
use ps_http::request::{RequestContext, RequestHeaders};
use ps_http::response::{
    HttpOptions, RequestProperties, ResponseHeaders, ValidatorOption, VaryOption,
};
use ps_http::value::HttpValue;
use ps_stats::core::{Statistics, Variable};

use super::resource::Resource;
use super::server_context::{ResourceDriver, ServerContext};

/// Uncacheable follow-up (`NotCacheablePolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotCacheablePolicy {
    /// Load private/volatile content anyway.
    LoadEvenIfNotCacheable,
    /// Fail uncacheable loads.
    ReportFailureIfNotCacheable,
}

/// Load completion (`Resource::AsyncCallback`).
pub trait LoadCallback {
    /// Reports completion (`Done`).
    fn done(&mut self, lock_failure: bool, resource_ok: bool);
}

/// Recording test callback (`MockResourceCallback`).
#[derive(Debug, Default)]
pub struct MockLoadCallback {
    done: bool,
    lock_failure: bool,
    success: bool,
}

impl MockLoadCallback {
    /// Builds an uncompleted callback.
    #[must_use]
    pub fn new() -> Self {
        MockLoadCallback::default()
    }

    /// Whether completion ran.
    #[must_use]
    pub fn done(&self) -> bool {
        self.done
    }

    /// Whether the load succeeded (valid once done).
    #[must_use]
    pub fn success(&self) -> bool {
        self.success
    }
}

impl LoadCallback for MockLoadCallback {
    fn done(&mut self, lock_failure: bool, resource_ok: bool) {
        self.done = true;
        self.lock_failure = lock_failure;
        self.success = resource_ok;
    }
}

/// Input validity record (`InputInfo`, no proto).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InputInfo {
    /// Partition index.
    pub index: Option<i32>,
    /// Validity rule.
    pub info_type: InputInfoType,
    /// Last-modified (or mtime) watermark.
    pub last_modified_time_ms: Option<i64>,
    /// Cache expiry watermark.
    pub expiration_time_ms: Option<i64>,
    /// Source file (file inputs only).
    pub filename: Option<String>,
    /// Original fetch time.
    pub date_ms: Option<i64>,
    /// Content hash for change detection.
    pub input_content_hash: Option<String>,
    /// Whether later filters must stand down.
    pub disable_further_processing: Option<bool>,
    /// Source URL (purgeable inputs only).
    pub url: Option<String>,
}

/// Input validity rule (`InputInfo::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputInfoType {
    /// Valid until the expiry watermark.
    #[default]
    Cached,
    /// Valid while the file is unchanged.
    FileBased,
    /// Never expires (`data:` URLs).
    AlwaysValid,
}

impl InputInfo {
    /// Whether a content hash is recorded.
    #[must_use]
    pub fn has_input_content_hash(&self) -> bool {
        self.input_content_hash.is_some()
    }
}

/// Freshen completion (`Resource::FreshenCallback`).
pub trait FreshenCallback {
    /// Record to extend, if the caller tracks one.
    fn input_info_mut(&mut self) -> Option<&mut InputInfo>;

    /// Reports completion (`Done`).
    fn done(&mut self, lock_failure: bool, extend_success: bool);
}

/// Per-subclass fetch hooks (`PrepareRequest`/`PrepareResponseHeaders`).
pub trait ResourceHooks {
    /// Adjusts the outgoing request (headers, session origins).
    fn prepare_request(&self, _ctx: &mut RequestContext, _headers: &mut RequestHeaders) {}

    /// Adjusts fetched headers before caching.
    fn prepare_response_headers(&self, _headers: &mut ResponseHeaders) {}
}

/// No-op hooks.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHooks;

impl ResourceHooks for NoHooks {}

/// Stale warning served with fallback bodies
/// (`kStaleWarningHeaderValue`).
const STALE_WARNING: &str = "110 Response is stale";

/// Capturing fetch for one load (`LoadFetchCallback`, lock-free:
///
/// locks serialize redundant concurrent fetches, mute in a
/// synchronous port).
struct CaptureFetch {
    url: String,
    request_headers: RequestHeaders,
    response_headers: ResponseHeaders,
    extra_response_headers: ResponseHeaders,
    request_context: RequestContext,
    body: Vec<u8>,
    writes: u32,
    done: bool,
    success: bool,
}

impl CaptureFetch {
    fn new(
        url: &str,
        request_headers: RequestHeaders,
        http_options: HttpOptions,
        request_context: RequestContext,
    ) -> Self {
        CaptureFetch {
            url: url.to_string(),
            request_headers,
            response_headers: ResponseHeaders::new(http_options),
            extra_response_headers: ResponseHeaders::new(http_options),
            request_context,
            body: Vec::new(),
            writes: 0,
            done: false,
            success: false,
        }
    }
}

impl AsyncFetch for CaptureFetch {
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

    fn handle_write(&mut self, bytes: &[u8]) -> bool {
        self.writes += 1;
        self.body.extend_from_slice(bytes);
        true
    }

    fn handle_done(&mut self, success: bool) {
        self.done = true;
        self.success = success;
    }
}

/// Outcome of one fetch attempt.
struct FetchOutcome {
    headers: ResponseHeaders,
    body: Vec<u8>,
    wrote: bool,
    success: bool,
    served_fallback: bool,
    /// The fetcher held the fetch (`!done`): inconclusive, never a
    /// failure, and nothing may be remembered from it.
    pending: bool,
}

/// Fetch outcome plus its store verdict: what background jobs
/// return and both completion paths link.
struct FetchJobResult {
    headers: ResponseHeaders,
    body: Vec<u8>,
    wrote: bool,
    success: bool,
    served_fallback: bool,
    pending: bool,
    cached: bool,
    status: FetchStatus,
}

impl FetchJobResult {
    /// Held outcome: inconclusive, never stored.
    fn pending() -> Self {
        FetchJobResult {
            headers: ResponseHeaders::new(HttpOptions::default()),
            body: Vec::new(),
            wrote: false,
            success: false,
            served_fallback: false,
            pending: true,
            cached: false,
            status: FetchStatus::NotSet,
        }
    }
}

impl FetchOutcome {
    /// Blank failure outcome (unmappable URL).
    fn failed(http_options: HttpOptions) -> Self {
        FetchOutcome {
            headers: ResponseHeaders::new(http_options),
            body: Vec::new(),
            wrote: false,
            success: false,
            served_fallback: false,
            pending: false,
        }
    }
}

/// Owned fetch inputs: prepared on the requesting thread (mapping,
/// headers, snapshots) and executable on any thread. Everything a
/// background completion needs to fetch and store, with no borrows.
#[derive(Debug, Clone)]
struct FetchPlan {
    fetch_url: String,
    req_headers: RequestHeaders,
    http_options: HttpOptions,
    request_context: RequestContext,
    fallback: Option<(ResponseHeaders, Vec<u8>)>,
    conditional_added: bool,
    serve_stale: bool,
    cache_key: String,
    fragment: String,
    override_ttl_ms: i64,
    ttl_overridden: bool,
    respect_vary: VaryOption,
    min_cache_time_ms: i64,
    force_caching: bool,
    now_ms: i64,
}

/// Executes a plan against a fetcher (`run_fetch` execution half,
/// thread-agnostic).
fn execute_fetch(plan: &FetchPlan, fetcher: &dyn AsyncFetcher) -> FetchOutcome {
    let mut fetch = CaptureFetch::new(
        &plan.fetch_url,
        plan.req_headers.clone(),
        plan.http_options,
        plan.request_context.clone(),
    );
    fetcher.fetch(&plan.fetch_url, &mut fetch);
    let mut headers = fetch.response_headers;
    let mut body = fetch.body;
    let mut success = fetch.success;
    let wrote = fetch.writes > 0;
    // A fetcher that never completed the fetch held it: the
    // outcome is inconclusive, never a failure.
    let pending = !fetch.done;
    if plan.serve_stale && headers.is_server_error_status() {
        // Serve the stale fallback with a warning, uncached.
        if let Some((fallback_headers, fallback_body)) = plan.fallback.as_ref() {
            headers = fallback_headers.clone();
            headers.headers_mut().add(header::WARNING, STALE_WARNING);
            headers.compute_caching();
            body = fallback_body.clone();
            success = true;
            return FetchOutcome {
                headers,
                body,
                wrote,
                success,
                served_fallback: true,
                pending: false,
            };
        }
    }
    if plan.conditional_added && headers.status_code() == 304 {
        // A 304 revalidates the stale entry: serve it fresh.
        if let Some((fallback_headers, fallback_body)) = plan.fallback.as_ref() {
            headers = fallback_headers.clone();
            if headers.is_implicitly_cacheable() {
                headers.set_cache_control_max_age(plan.http_options.implicit_cache_ttl_ms);
                headers.compute_caching();
            }
            body = fallback_body.clone();
            success = true;
        }
    }
    FetchOutcome {
        headers,
        body,
        wrote,
        success,
        served_fallback: false,
        pending,
    }
}

/// Stores a fetch outcome (`AddToCache` as a free function so
/// background completions share the exact sync logic).
#[allow(clippy::too_many_arguments)]
fn store_fetch_outcome<H: ResourceHooks>(
    http_cache: &HttpCache,
    hooks: &H,
    headers: &mut ResponseHeaders,
    body: &[u8],
    wrote: bool,
    fetch_success: bool,
    cache_key: &str,
    fragment: &str,
    http_options: HttpOptions,
    override_ttl_ms: i64,
    ttl_overridden: bool,
    respect_vary: VaryOption,
    min_cache_time_ms: i64,
    force_caching: bool,
    now_ms: i64,
) -> (bool, FetchStatus) {
    // NOTE: extra response headers merge here in C++; the sync
    // fetch carries none.
    hooks.prepare_response_headers(headers);
    headers.compute_caching();
    headers.fix_date_headers(now_ms);
    if ttl_overridden {
        headers.force_caching(override_ttl_ms);
    }
    let cacheable = valid_and_cacheable(
        headers,
        respect_vary,
        min_cache_time_ms,
        force_caching,
        http_cache,
        now_ms,
    );
    let status = classify_failure(headers, body, fetch_success && wrote, cacheable);
    if status == FetchStatus::Ok {
        let mut value = HttpValue::new();
        value.set_headers(headers.clone());
        value.write_body(body);
        http_cache.put_value(
            cache_key,
            fragment,
            RequestProperties::default(),
            http_options,
            &value,
            now_ms,
        );
        (true, status)
    } else {
        http_cache.remember_failure(cache_key, fragment, status, now_ms);
        (false, status)
    }
}

/// Runs one fetch+store job: the exact body every background
/// fetch executes, so HTML-embedded and in-place fetches share one
/// fetch/store path with the sync completions.
fn run_fetch_job<H: ResourceHooks>(
    plan: &FetchPlan,
    fetcher: &dyn AsyncFetcher,
    http_cache: &HttpCache,
    hooks: &H,
) -> FetchJobResult {
    let outcome = execute_fetch(plan, fetcher);
    if outcome.pending {
        return FetchJobResult::pending();
    }
    let mut headers = outcome.headers;
    let (cached, status) = store_fetch_outcome(
        http_cache,
        hooks,
        &mut headers,
        &outcome.body,
        outcome.wrote,
        outcome.success,
        &plan.cache_key,
        &plan.fragment,
        plan.http_options,
        plan.override_ttl_ms,
        plan.ttl_overridden,
        plan.respect_vary,
        plan.min_cache_time_ms,
        plan.force_caching,
        plan.now_ms,
    );
    FetchJobResult {
        headers,
        body: outcome.body,
        wrote: outcome.wrote,
        success: outcome.success,
        served_fallback: outcome.served_fallback,
        pending: false,
        cached,
        status,
    }
}

/// Deadline-bounded origin fetch, for serving paths that bypass
/// `CacheableResource` (in-place serving fetches origins directly).
#[derive(Debug)]
pub(crate) enum OriginFetch {
    /// No pool (or bots excluded, or unmappable URL): the caller
    /// runs its direct sync fetch, exactly as before.
    Direct,
    /// Completed within budget (transport outcome included, so HTTP
    /// errors still arrive as responses, never timeouts). Boxed: the
    /// payload rides one match arm, not every stack copy.
    Ready(Box<BoundedOriginFetch>),
    /// Budget exceeded or fetch held: the job still warms the cache
    /// for the next request; the caller takes its failure path.
    Pending,
}

/// A bounded origin fetch that completed in time.
#[derive(Debug)]
pub(crate) struct BoundedOriginFetch {
    /// Store-processed headers (dated, TTL-overridden as configured).
    pub headers: ResponseHeaders,
    /// Response body (present even for HTTP errors, as direct).
    pub body: Vec<u8>,
    /// Transport success (the `AsyncFetch` verdict, as direct).
    pub success: bool,
    /// Whether the job cached the entry (skip a manual put then).
    pub cached: bool,
}

/// Fetches an origin URL through the background pool when one is
/// registered, waiting up to the driver's fetch budget (the IPRO
/// deadline while in-place serving with waiting enabled, else the
/// flush deadline). The dedup key shares the resource-fetch shape,
/// so simultaneous identical fetches collapse onto one job.
pub(crate) fn fetch_origin_bounded(
    driver: &ResourceDriver,
    url: &str,
    ctx: &RequestContext,
) -> OriginFetch {
    let server = driver.server_context();
    let Some(pool) = server.background_pool() else {
        return OriginFetch::Direct;
    };
    let options = driver.options();
    if driver.is_bot() && options.disable_background_fetches_for_bots() {
        return OriginFetch::Direct;
    }
    let gurl = GoogleUrl::new(url);
    if !gurl.is_web_valid() {
        return OriginFetch::Direct;
    }
    let lawyer = driver.lawyer();
    let (fetch_url, host_header) = if let Some((stripped, _)) = lawyer.strip_proxy_suffix(&gurl) {
        (stripped, None)
    } else if let Some((mapped, host, is_proxy)) = lawyer.map_origin_url(&gurl) {
        (mapped, if is_proxy { None } else { Some(host) })
    } else {
        // Unauthorized: keep the direct posture (which never
        // mapped), rather than inventing a new failure here.
        return OriginFetch::Direct;
    };
    let mut req_headers = RequestHeaders::new();
    if let Some(host) = host_header {
        req_headers.headers_mut().replace(header::HOST, &host);
    }
    let plan = FetchPlan {
        fetch_url,
        req_headers,
        http_options: HttpOptions {
            respect_vary: options.respect_vary(),
            implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
        },
        request_context: ctx.clone(),
        fallback: None,
        conditional_added: false,
        serve_stale: options.serve_stale_if_fetch_error(),
        cache_key: url.to_string(),
        fragment: driver.cache_fragment().to_string(),
        override_ttl_ms: options.override_caching_ttl_ms(),
        ttl_overridden: options.is_cache_ttl_overridden(url),
        respect_vary: VaryOption::IgnoreVaryOnResources,
        min_cache_time_ms: options.min_resource_cache_time_to_rewrite_ms(),
        force_caching: server.http_cache().force_caching(),
        now_ms: server.now_ms(),
    };
    let key = format!(
        "fetch:{}\n{:?}\n{:?}",
        plan.fetch_url, plan.req_headers, plan.fallback
    );
    let hooks = NoHooks;
    let fetcher = server.fetcher().clone();
    let http_cache = server.http_cache_shared();
    let job_plan = plan.clone();
    let handle = match pool.submit(&key, move || {
        run_fetch_job(&job_plan, fetcher.as_ref(), http_cache.as_ref(), &hooks)
    }) {
        Ok(handle) => handle,
        // Pool saturated or gone: degrade to inline, like the
        // resource path (slower, never failed).
        Err(_) => {
            let result = run_fetch_job(
                &plan,
                server.fetcher().as_ref(),
                server.http_cache(),
                &NoHooks,
            );
            if result.pending {
                return OriginFetch::Pending;
            }
            return OriginFetch::Ready(Box::new(BoundedOriginFetch {
                headers: result.headers,
                body: result.body,
                success: result.success,
                cached: result.cached,
            }));
        }
    };
    let start = std::time::Instant::now();
    let result: Option<Arc<FetchJobResult>> = match driver.fetch_wait_budget_ms() {
        None => handle.wait_unbounded(),
        Some(ms) if ms <= 0 => None,
        Some(ms) => handle.wait_timeout(std::time::Duration::from_millis(ms as u64)),
    };
    driver.deduct_fetch_wait_ms(start.elapsed().as_millis() as i64);
    let Some(result) = result else {
        server.variable("background_jobs_timeouts").add(1);
        return OriginFetch::Pending;
    };
    if result.pending {
        return OriginFetch::Pending;
    }
    OriginFetch::Ready(Box::new(BoundedOriginFetch {
        headers: result.headers.clone(),
        body: result.body.clone(),
        success: result.success,
        cached: result.cached,
    }))
}

/// Validity check behind [`store_fetch_outcome`] (same inputs as
/// `is_valid_and_cacheable_impl`, owned for thread use).
fn valid_and_cacheable(
    headers: &ResponseHeaders,
    respect_vary: VaryOption,
    min_cache_time_ms: i64,
    force_caching: bool,
    http_cache: &HttpCache,
    now_ms: i64,
) -> bool {
    if headers.status_code() != 200 {
        return false;
    }
    // Conservatively assume cookies: responses varying on them
    // must not serve optimized bytes blindly.
    let mut cacheable = headers.is_proxy_cacheable(
        RequestProperties::default(),
        respect_vary,
        ValidatorOption::NoValidator,
    );
    cacheable &= headers.cache_ttl_ms() >= min_cache_time_ms;
    if !cacheable && !force_caching {
        return false;
    }
    !http_cache.is_expired(headers, now_ms)
}

/// A cache-backed resource (`CacheableResourceBase`).
#[derive(Clone)]
pub struct CacheableResourceBase<H: ResourceHooks> {
    driver: Rc<ResourceDriver>,
    url: String,
    cache_key: String,
    content_type: Option<ContentType>,
    charset: String,
    headers: ResponseHeaders,
    body: Vec<u8>,
    fallback_headers: ResponseHeaders,
    fallback_body: Vec<u8>,
    fetch_status: FetchStatus,
    /// The last load attempt held its fetch (`!done`): inconclusive,
    /// remembered by nothing, retried by everything.
    last_load_pending: bool,
    is_background_fetch: bool,
    authorized: bool,
    enable_cache_purge: bool,
    proactive_freshening: bool,
    respect_vary: VaryOption,
    // Read by `IsSafeToRewrite`, landing with the rewrite contexts.
    #[allow(dead_code)]
    disable_rewrite_on_no_transform: bool,
    /// Inflated body + headers, computed once (`extracted_*`).
    extracted: RefCell<Option<(Vec<u8>, ResponseHeaders)>>,
    /// Whether extraction was attempted (`kExtractNotComputed` flip).
    extraction_done: RefCell<bool>,
    hooks: H,
    hits: Arc<Variable>,
    recent_fetch_failures: Arc<Variable>,
    recent_uncacheables_miss: Arc<Variable>,
    recent_uncacheables_failure: Arc<Variable>,
    misses: Arc<Variable>,
}

impl<H: ResourceHooks> CacheableResourceBase<H> {
    /// Builds a resource over a driver (`CacheableResourceBase`).
    pub fn new(
        driver: Rc<ResourceDriver>,
        stat_prefix: &str,
        url: &str,
        cache_key: &str,
        content_type: Option<ContentType>,
        hooks: H,
    ) -> Self {
        let (
            http_options,
            enable_cache_purge,
            proactive_freshening,
            respect_vary,
            disable_rewrite_on_no_transform,
        ) = {
            let options = driver.options();
            (
                HttpOptions {
                    respect_vary: options.respect_vary(),
                    implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
                },
                options.enable_cache_purge(),
                options.proactive_resource_freshening(),
                if options.respect_vary() {
                    VaryOption::RespectVaryOnResources
                } else {
                    VaryOption::IgnoreVaryOnResources
                },
                options.disable_rewrite_on_no_transform(),
            )
        };
        let server = driver.server_context().clone();
        let stats = |suffix: &str| server.variable(&format!("{stat_prefix}{suffix}"));
        CacheableResourceBase {
            driver,
            url: url.to_string(),
            cache_key: cache_key.to_string(),
            content_type,
            charset: String::new(),
            headers: ResponseHeaders::new(http_options),
            body: Vec::new(),
            fallback_headers: ResponseHeaders::new(http_options),
            fallback_body: Vec::new(),
            fetch_status: FetchStatus::NotSet,
            last_load_pending: false,
            is_background_fetch: true,
            authorized: true,
            enable_cache_purge,
            proactive_freshening,
            respect_vary,
            disable_rewrite_on_no_transform,
            extracted: RefCell::new(None),
            extraction_done: RefCell::new(false),
            hooks,
            hits: stats("_hit"),
            recent_fetch_failures: stats("_recent_fetch_failure"),
            recent_uncacheables_miss: stats("_recent_uncacheable_miss"),
            recent_uncacheables_failure: stats("_recent_uncacheable_failure"),
            misses: stats("_miss"),
        }
    }

    /// Registers the per-resource counters (`InitStats`).
    pub fn init_stats(statistics: &Statistics, prefix: &str) {
        for suffix in [
            "_hit",
            "_recent_fetch_failure",
            "_recent_uncacheable_miss",
            "_recent_uncacheable_failure",
            "_miss",
        ] {
            statistics.add_variable(&format!("{prefix}{suffix}"));
        }
    }

    /// Reads the five counters (hits, fetch failures, uncacheable
    /// miss/failure, misses) for tests.
    #[must_use]
    pub fn stats(&self) -> (i64, i64, i64, i64, i64) {
        (
            self.hits.get(),
            self.recent_fetch_failures.get(),
            self.recent_uncacheables_miss.get(),
            self.recent_uncacheables_failure.get(),
            self.misses.get(),
        )
    }

    /// Borrows the cache key.
    #[must_use]
    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    /// Overrides the cache key (tests vary it per resource).
    pub fn set_cache_key(&mut self, key: &str) {
        self.cache_key = key.to_string();
    }

    /// Borrows the content type, if determined.
    #[must_use]
    pub fn content_type(&self) -> Option<ContentType> {
        self.content_type
    }

    /// Mutably borrows the fetch hooks (tests flip behavior here).
    pub fn hooks_mut(&mut self) -> &mut H {
        &mut self.hooks
    }

    /// Borrows the detected charset, if any.
    #[must_use]
    pub fn charset(&self) -> &str {
        &self.charset
    }

    /// Whether contents are loaded (nonzero status).
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.headers.status_code() != 0
    }

    /// Borrows the response headers.
    #[must_use]
    pub fn response_headers(&self) -> &ResponseHeaders {
        &self.headers
    }

    /// Whether the status is 200 (`HttpStatusOk`).
    #[must_use]
    pub fn http_status_ok(&self) -> bool {
        self.headers.status_code() == 200
    }

    /// Fetch outcome classification.
    #[must_use]
    pub fn fetch_response_status(&self) -> FetchStatus {
        self.fetch_status
    }

    /// Marks background (vs user-facing) fetching.
    pub fn set_is_background_fetch(&mut self, background: bool) {
        self.is_background_fetch = background;
    }

    /// Uncompressed contents (`ExtractUncompressedContents`), inflated
    /// at most once; oversized or corrupt bodies read raw.
    #[must_use]
    pub fn contents(&self) -> Vec<u8> {
        if self.ensure_extracted() {
            self.extracted
                .borrow()
                .as_ref()
                .expect("extracted above")
                .0
                .clone()
        } else {
            self.body.clone()
        }
    }

    /// Headers matching [`contents`](Self::contents)
    /// (`UncompressedHeaders`): inflated reads drop the gzip coding and
    /// report the inflated length.
    #[must_use]
    pub fn uncompressed_headers(&self) -> ResponseHeaders {
        if self.ensure_extracted() {
            self.extracted
                .borrow()
                .as_ref()
                .expect("extracted above")
                .1
                .clone()
        } else {
            self.headers.clone()
        }
    }

    /// Inflates once within the size cap (`EnsureExtractedIfNeeded`),
    /// returning whether the extracted pair is live.
    fn ensure_extracted(&self) -> bool {
        if *self.extraction_done.borrow() {
            return self.extracted.borrow().is_some();
        }
        *self.extraction_done.borrow_mut() = true;
        if !self.headers.is_gzipped() {
            return false;
        }
        let limit = self
            .driver
            .options()
            .max_cacheable_response_content_length();
        let max_bytes = if limit < 0 { u64::MAX } else { limit as u64 };
        if let Some(inflated) = ps_http::fetch::inflate_gzip_with_limit(&self.body, max_bytes) {
            let mut headers = self.headers.clone();
            headers
                .headers_mut()
                .remove(header::CONTENT_ENCODING, "gzip");
            headers
                .headers_mut()
                .replace(header::CONTENT_LENGTH, &inflated.len().to_string());
            headers.compute_caching();
            *self.extracted.borrow_mut() = Some((inflated, headers));
            true
        } else {
            // Broken input stays raw, and never retries.
            false
        }
    }

    /// Whether the loaded resource is fresh and proxy-cacheable.
    #[must_use]
    pub fn is_valid_and_cacheable(&self) -> bool {
        self.is_valid_and_cacheable_impl(&self.headers.clone())
    }

    /// Validity check against explicit headers (also gates caching).
    #[must_use]
    pub fn is_valid_and_cacheable_impl(&self, headers: &ResponseHeaders) -> bool {
        if headers.status_code() != 200 {
            return false;
        }
        // Conservatively assume cookies: responses varying on them
        // must not serve optimized bytes blindly.
        let mut cacheable = headers.is_proxy_cacheable(
            RequestProperties::default(),
            self.respect_vary,
            ValidatorOption::NoValidator,
        );
        cacheable &= headers.cache_ttl_ms()
            >= self
                .driver
                .options()
                .min_resource_cache_time_to_rewrite_ms();
        if !cacheable && !self.server().http_cache().force_caching() {
            return false;
        }
        !self
            .server()
            .http_cache()
            .is_expired(headers, self.now_ms())
    }

    /// Whether the loaded resource may rewrite (`IsSafeToRewrite`),
    /// with the failure reason appended.
    pub fn is_safe_to_rewrite(&self, rewrite_uncacheable: bool) -> (bool, String) {
        let server = self.server();
        let mut reason = String::new();
        if self.headers.status_code() != 200 {
            reason.push_str(match self.fetch_status {
                FetchStatus::Dropped => "Fetch was dropped due to load, ",
                FetchStatus::Error4xx => "4xx status code, ",
                FetchStatus::Uncacheable200 | FetchStatus::UncacheableError => {
                    "Uncacheable content, "
                }
                FetchStatus::Empty => "Resource is empty, ",
                FetchStatus::OtherError => "Fetch failure, ",
                FetchStatus::NotSet => "Resource headers are ",
                FetchStatus::Ok => "Fetch status OK but !HttpStatusOk in IsSafeToRewrite!  ",
            });
        } else if !rewrite_uncacheable && !self.is_valid_and_cacheable() {
            reason.push_str(
                if server.http_cache().is_expired(&self.headers, self.now_ms()) {
                    "Cached content expired, "
                } else {
                    "Invalid or uncacheable content, "
                },
            );
        } else if self.disable_rewrite_on_no_transform
            && self.headers.has_cache_control_directive("no-transform")
        {
            reason.push_str("Cache-control: no-transform, ");
        } else if self.headers.headers().lookup1("X-Sendfile").is_some()
            || self.headers.headers().lookup1("X-Accel-Redirect").is_some()
        {
            reason.push_str("Sendfile in header, unsafe to rewrite! ");
        } else if self.contents().is_empty() {
            reason.push_str("Resource is empty, ");
        } else if self.headers.is_gzipped() && !self.extracted_ready() {
            reason.push_str("Compressed content could not be inflated, ");
        } else {
            server
                .variable("num_cache_control_rewritable_resources")
                .add(1);
            return (true, reason);
        }
        reason.push_str(&format!("preventing rewriting of {}", self.url));
        server
            .variable("num_cache_control_not_rewritable_resources")
            .add(1);
        (false, reason)
    }

    /// Whether extraction produced inflated bytes.
    fn extracted_ready(&self) -> bool {
        self.ensure_extracted()
    }

    /// Sets the content type from headers, else the URL extension
    /// (`DetermineContentType`).
    pub fn determine_content_type(&mut self) {
        let mut content_type = self.headers.determine_content_type();
        if let Some(charset) = self.headers.determine_charset() {
            self.charset = charset;
        }
        if content_type.is_none() && !self.headers.headers().has(header::CONTENT_TYPE) {
            let leaf = GoogleUrl::new(self.url.trim()).leaf_sans_query();
            content_type = extension_to_content_type(&leaf);
        }
        self.content_type = content_type;
    }

    /// Links headers + body into the resource (`Link`), dropping any
    /// previous extraction.
    pub fn link(&mut self, headers: &ResponseHeaders, body: &[u8]) {
        self.headers = headers.clone();
        self.body = body.to_vec();
        *self.extracted.borrow_mut() = None;
        *self.extraction_done.borrow_mut() = false;
    }

    /// Links a stale fallback (`LinkFallbackValue`); empty values do
    /// not displace the current one.
    pub fn link_fallback_value(&mut self, headers: &ResponseHeaders, body: &[u8]) {
        if headers.status_code() != 0 || !body.is_empty() {
            self.fallback_headers = headers.clone();
            self.fallback_body = body.to_vec();
        }
    }

    /// Wipes loaded state, keeping configuration (`Reset`, test use).
    pub fn reset(&mut self) {
        self.headers.clear();
        self.body.clear();
        self.fallback_headers.clear();
        self.fallback_body.clear();
        self.last_load_pending = false;
        *self.extracted.borrow_mut() = None;
        *self.extraction_done.borrow_mut() = false;
    }

    /// Whether the last load attempt held its fetch: the outcome is
    /// inconclusive and nothing was remembered.
    #[must_use]
    pub fn load_pending(&self) -> bool {
        self.last_load_pending
    }

    /// Loads, calling back inline (`LoadAsync`, sync completion).
    pub fn load(
        &mut self,
        policy: NotCacheablePolicy,
        ctx: &mut RequestContext,
        callback: &mut dyn LoadCallback,
    ) where
        H: Clone + Send + 'static,
    {
        if self.loaded() {
            self.refresh_if_imminently_expiring();
            callback.done(false, true);
            return;
        }
        self.load_and_callback(policy, ctx, callback);
    }

    /// Refreshes a soon-expiring cache entry (`Freshen`).
    ///
    /// Without a callback only the cache entry refreshes; with one,
    /// its record extends on a hash match (`UpdateInputInfoForFreshen`).
    pub fn freshen(&mut self, mut callback: Option<&mut dyn FreshenCallback>) {
        let now = self.now_ms();
        let stale = |headers: &ResponseHeaders| !headers.is_imminently_expiring(now);
        let mut driver_ctx = self.driver.request_context().borrow_mut();
        let found = self.find_cached_with(&mut driver_ctx, &stale);
        drop(driver_ctx);
        if found.status == FindStatus::NotFound && !self.should_skip_background_fetch() {
            let stale_fallback = found
                .fallback_headers
                .as_ref()
                .map(|headers| (headers, found.fallback_body.as_slice()));
            let mut driver_ctx = self.driver.request_context().borrow_mut();
            let outcome = self.run_fetch_with_fallback(&mut driver_ctx, stale_fallback);
            drop(driver_ctx);
            if outcome.pending {
                // Held freshen: nothing concluded, nothing cached.
                if let Some(callback) = callback {
                    callback.done(false, false);
                }
                return;
            }
            let mut headers = outcome.headers;
            let cached = if outcome.served_fallback {
                self.fetch_status = FetchStatus::OtherError;
                false
            } else {
                let (cached, status) =
                    self.add_to_cache(&mut headers, &outcome.body, outcome.wrote, outcome.success);
                self.fetch_status = status;
                cached
            };
            let mut ok = outcome.success || outcome.served_fallback;
            if (!cached && !outcome.served_fallback) || !outcome.wrote {
                ok = false;
            }
            if let Some(callback) = callback.as_deref_mut() {
                ok &= self.update_input_info_for_freshen(&headers, &outcome.body, callback);
            }
            if let Some(callback) = callback {
                callback.done(false, ok);
            }
        } else if let Some(callback) = callback {
            let ok = found.status == FindStatus::Found && self.freshen_cache_hit(&found, callback);
            // NOTE: lock-failure `true` on the cache path, like C++.
            callback.done(true, ok);
        }
    }

    /// Extends a record from a fresh cache hit (freshen cache path).
    fn freshen_cache_hit(&self, found: &FindResult, callback: &mut dyn FreshenCallback) -> bool {
        match found.headers.as_ref() {
            Some(headers) => self.update_input_info_for_freshen(headers, &found.body, callback),
            None => false,
        }
    }

    /// Triggers a background refresh when expiry looms
    /// (`RefreshIfImminentlyExpiring`).
    pub fn refresh_if_imminently_expiring(&mut self) {
        if self.server().http_cache().force_caching() {
            return;
        }
        if self.headers.clone().is_imminently_expiring(self.now_ms()) {
            self.freshen(None);
        }
    }

    /// Records validity watermarks (`FillInPartitionInputInfo`).
    pub fn fill_in_partition_input_info(&self, include_hash: bool, input: &mut InputInfo) {
        assert!(self.loaded(), "filling input info for an unloaded resource");
        input.info_type = InputInfoType::Cached;
        Self::fill_in_from_headers(&self.headers, input);
        if include_hash && self.is_valid_and_cacheable() {
            input.input_content_hash = Some(self.contents_hash());
        } else {
            input.input_content_hash = None;
        }
        if (self.enable_cache_purge || self.proactive_freshening) && !self.url.is_empty() {
            input.url = Some(self.url.clone());
        }
    }

    /// Copies watermarks from headers
    /// (`FillInPartitionInputInfoFromResponseHeaders`).
    pub fn fill_in_from_headers(headers: &ResponseHeaders, input: &mut InputInfo) {
        if headers.headers().has(header::LAST_MODIFIED) {
            input.last_modified_time_ms = Some(headers.last_modified_time_ms());
        }
        input.expiration_time_ms = Some(headers.cache_expiration_time_ms());
        input.date_ms = Some(headers.date_ms());
    }

    /// Content hash (`ContentsHash`, via the contents hasher).
    #[must_use]
    pub fn contents_hash(&self) -> String {
        use ps_cache::hash::Hasher;
        self.server().contents_hasher().hash(&self.contents())
    }

    fn server(&self) -> Rc<ServerContext> {
        self.driver.server_context().clone()
    }

    fn now_ms(&self) -> i64 {
        self.server().now_ms()
    }

    fn http_options(&self) -> HttpOptions {
        let options = self.driver.options();
        HttpOptions {
            respect_vary: options.respect_vary(),
            implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
        }
    }

    fn should_skip_background_fetch(&self) -> bool {
        self.is_background_fetch
            && self.driver.options().disable_background_fetches_for_bots()
            && self.driver.is_bot()
    }

    /// Looks the cache key up (`HTTPCache::Find`).
    fn find_cached_with(
        &self,
        ctx: &mut RequestContext,
        is_fresh: &dyn Fn(&ResponseHeaders) -> bool,
    ) -> FindResult {
        let override_ttl = {
            let options = self.driver.options();
            if options.is_cache_ttl_overridden(&self.cache_key) {
                options.override_caching_ttl_ms()
            } else {
                -1
            }
        };
        self.server().http_cache().find(
            &self.cache_key,
            self.driver.cache_fragment(),
            RequestProperties::default(),
            self.respect_vary,
            is_fresh,
            override_ttl,
            ctx,
            self.now_ms(),
        )
    }

    fn load_and_callback(
        &mut self,
        policy: NotCacheablePolicy,
        ctx: &mut RequestContext,
        callback: &mut dyn LoadCallback,
    ) where
        H: Clone + Send + 'static,
    {
        let result = self.find_cached_with(ctx, &|_| true);
        match result.status {
            FindStatus::Found => {
                self.hits.add(1);
                let headers = result.headers.expect("found entry without headers");
                self.link(&headers, &result.body);
                self.determine_content_type();
                self.refresh_if_imminently_expiring();
                self.fetch_status = if headers.status_code() == 200 {
                    FetchStatus::Ok
                } else {
                    FetchStatus::OtherError
                };
                callback.done(false, true);
            }
            FindStatus::RecentFailure => {
                let retry = (policy == NotCacheablePolicy::LoadEvenIfNotCacheable
                    && matches!(
                        result.failure,
                        FetchStatus::Uncacheable200
                            | FetchStatus::UncacheableError
                            | FetchStatus::Empty
                    ))
                    || (!self.is_background_fetch && result.failure == FetchStatus::Dropped);
                if retry {
                    self.recent_uncacheables_miss.add(1);
                    self.load_and_save_to_cache(policy, ctx, callback);
                } else {
                    if matches!(
                        result.failure,
                        FetchStatus::Uncacheable200 | FetchStatus::UncacheableError
                    ) {
                        self.recent_uncacheables_failure.add(1);
                    } else {
                        self.recent_fetch_failures.add(1);
                    }
                    self.fetch_status = result.failure;
                    callback.done(false, false);
                }
            }
            FindStatus::NotFound => {
                self.misses.add(1);
                if let Some(headers) = result.fallback_headers.as_ref() {
                    self.link_fallback_value(headers, &result.fallback_body);
                }
                self.load_and_save_to_cache(policy, ctx, callback);
            }
        }
    }

    fn load_and_save_to_cache(
        &mut self,
        policy: NotCacheablePolicy,
        ctx: &mut RequestContext,
        callback: &mut dyn LoadCallback,
    ) where
        H: Clone + Send + 'static,
    {
        if self.should_skip_background_fetch() {
            callback.done(true, false);
            return;
        }
        if let Some(pool) = self.server().background_pool() {
            self.load_async(&pool, policy, ctx, callback);
            return;
        }
        let outcome = self.run_fetch(ctx);
        self.last_load_pending = outcome.pending;
        if outcome.pending {
            // Held fetches conclude nothing: no cache write, no
            // failure memory, no stats. Callers retry later.
            self.headers.clear();
            callback.done(false, false);
            return;
        }
        let mut headers = outcome.headers;
        let body = outcome.body;
        let (cached, status) =
            self.add_to_cache(&mut headers, &body, outcome.wrote, outcome.success);
        self.link_outcome(
            &FetchJobResult {
                headers,
                body,
                wrote: outcome.wrote,
                success: outcome.success,
                served_fallback: outcome.served_fallback,
                pending: false,
                cached,
                status,
            },
            policy,
            callback,
        );
    }

    /// Fetches through the background pool (async C++ path): submits
    /// a deduped fetch+store job, waits up to the flush-window
    /// budget, and falls back to the existing failure path on
    /// timeout or rejection while the job still warms the cache.
    fn load_async(
        &mut self,
        pool: &std::sync::Arc<super::background::BackgroundPool>,
        policy: NotCacheablePolicy,
        ctx: &mut RequestContext,
        callback: &mut dyn LoadCallback,
    ) where
        H: Clone + Send + 'static,
    {
        let fallback = if self.fallback_headers.status_code() != 0 || !self.fallback_body.is_empty()
        {
            Some((&self.fallback_headers, self.fallback_body.as_slice()))
        } else {
            None
        };
        let Some(plan) = self.plan_fetch(ctx, fallback) else {
            // Unmappable URL: same failure as the sync path, no job.
            let outcome = FetchOutcome::failed(self.http_options());
            self.last_load_pending = false;
            let mut headers = outcome.headers;
            let body = outcome.body;
            let (cached, status) =
                self.add_to_cache(&mut headers, &body, outcome.wrote, outcome.success);
            self.link_outcome(
                &FetchJobResult {
                    headers,
                    body,
                    wrote: outcome.wrote,
                    success: outcome.success,
                    served_fallback: outcome.served_fallback,
                    pending: false,
                    cached,
                    status,
                },
                policy,
                callback,
            );
            return;
        };
        let key = format!(
            "fetch:{}\n{:?}\n{:?}",
            plan.fetch_url, plan.req_headers, plan.fallback
        );
        let hooks = self.hooks.clone();
        let fetcher = self.server().fetcher().clone();
        let http_cache = self.server().http_cache_shared();
        let job_plan = plan.clone();
        let handle = match pool.submit(&key, move || {
            run_fetch_job(&job_plan, fetcher.as_ref(), http_cache.as_ref(), &hooks)
        }) {
            Ok(handle) => handle,
            // Pool saturated or gone: run inline so the request
            // still completes (degraded, never failed).
            Err(_) => {
                let outcome = execute_fetch(&plan, self.server().fetcher().as_ref());
                self.last_load_pending = outcome.pending;
                if outcome.pending {
                    self.headers.clear();
                    callback.done(false, false);
                    return;
                }
                let mut headers = outcome.headers;
                let body = outcome.body;
                let (cached, status) =
                    self.add_to_cache(&mut headers, &body, outcome.wrote, outcome.success);
                self.link_outcome(
                    &FetchJobResult {
                        headers,
                        body,
                        wrote: outcome.wrote,
                        success: outcome.success,
                        served_fallback: outcome.served_fallback,
                        pending: false,
                        cached,
                        status,
                    },
                    policy,
                    callback,
                );
                return;
            }
        };
        let start = std::time::Instant::now();
        let result: Option<std::sync::Arc<FetchJobResult>> =
            match self.driver.fetch_wait_budget_ms() {
                None => handle.wait_unbounded(),
                Some(ms) if ms <= 0 => None,
                Some(ms) => handle.wait_timeout(std::time::Duration::from_millis(ms as u64)),
            };
        self.driver
            .deduct_fetch_wait_ms(start.elapsed().as_millis() as i64);
        let Some(result) = result else {
            // Deadline exceeded (or shutdown): serve fallback now;
            // the job still warms the cache for the next request.
            // Failure memory stays clean: nothing failed yet. The
            // input reads pending so contexts skip writing
            // partitions (C++ drops uncompleted contexts).
            self.server().variable("background_jobs_timeouts").add(1);
            self.last_load_pending = true;
            self.headers.clear();
            callback.done(false, false);
            return;
        };
        if result.pending {
            self.last_load_pending = true;
            self.headers.clear();
            callback.done(false, false);
            return;
        }
        self.last_load_pending = false;
        // Headers arrive store-processed (the job ran the shared
        // store path); link without re-storing.
        self.link_outcome(&result, policy, callback);
    }

    /// Links a stored outcome, records stats, and completes
    /// (post-store tail shared by sync and async paths).
    fn link_outcome(
        &mut self,
        result: &FetchJobResult,
        policy: NotCacheablePolicy,
        callback: &mut dyn LoadCallback,
    ) {
        let no_cache_ok = policy == NotCacheablePolicy::LoadEvenIfNotCacheable;
        let mut ok = result.success;
        if result.served_fallback {
            // Fallback bodies serve but never cache.
            self.fetch_status = FetchStatus::OtherError;
            ok = true;
        } else {
            self.fetch_status = result.status;
            if (!result.cached && !no_cache_ok) || !result.wrote {
                ok = false;
            }
        }
        if ok {
            self.link(&result.headers, &result.body);
            self.determine_content_type();
        } else {
            // Validity bits must not reflect a half-read failure.
            self.headers.clear();
        }
        let server = self.server();
        if ok {
            server.variable("num_resource_fetch_successes").add(1);
        } else {
            server.variable("num_resource_fetch_failures").add(1);
        }
        callback.done(false, ok);
    }

    /// Fetches, serving fallback or cached bodies when warranted.
    fn run_fetch(&self, ctx: &mut RequestContext) -> FetchOutcome {
        let fallback = if self.fallback_headers.status_code() != 0 || !self.fallback_body.is_empty()
        {
            Some((&self.fallback_headers, self.fallback_body.as_slice()))
        } else {
            None
        };
        self.run_fetch_with_fallback(ctx, fallback)
    }

    /// Fetches with an explicit stale entry for conditional/fallback use.
    fn run_fetch_with_fallback(
        &self,
        ctx: &mut RequestContext,
        fallback: Option<(&ResponseHeaders, &[u8])>,
    ) -> FetchOutcome {
        match self.plan_fetch(ctx, fallback) {
            Some(plan) => execute_fetch(&plan, self.server().fetcher().as_ref()),
            None => FetchOutcome::failed(self.http_options()),
        }
    }

    /// Prepares owned fetch inputs on the requesting thread
    /// (mapping, headers, snapshots); `None` when the URL cannot
    /// map, matching the sync failure outcome.
    fn plan_fetch(
        &self,
        ctx: &mut RequestContext,
        fallback: Option<(&ResponseHeaders, &[u8])>,
    ) -> Option<FetchPlan> {
        let mut req_headers = RequestHeaders::new();
        if !req_headers.headers().has(header::REFERER) {
            let base = self.driver.base_url();
            if base.is_web_valid() {
                req_headers.headers_mut().add(
                    header::REFERER,
                    &format!("{}://{}/", base.scheme(), base.host_and_port()),
                );
            }
        }
        // Options-manager mapping (`RewriteOptionsManager::PrepareRequest`):
        // strip a proxy suffix or map the origin (canonicalizing either
        // way), authorize stripped origins, and pin the Host header.
        let gurl = GoogleUrl::new(&self.url);
        if !gurl.is_web_valid() {
            // C++ stalls the state machine here; failing is saner.
            return None;
        }
        let lawyer = self.driver.lawyer();
        let (fetch_url, host_header) =
            if let Some((stripped, host)) = lawyer.strip_proxy_suffix(&gurl) {
                ctx.add_session_authorized_fetch_origin(&format!("{}://{}", gurl.scheme(), host));
                (stripped, None)
            } else if let Some((mapped, host, is_proxy)) = lawyer.map_origin_url(&gurl) {
                (mapped, if is_proxy { None } else { Some(host) })
            } else {
                return None;
            };
        if let Some(host) = host_header {
            req_headers.headers_mut().replace(header::HOST, &host);
        }
        let mut conditional_added = false;
        if let Some((fallback_headers, _)) = fallback {
            if !req_headers.headers().has(header::IF_MODIFIED_SINCE)
                && !req_headers.headers().has(header::IF_NONE_MATCH)
                && fallback_headers.status_code() == 200
            {
                if let Some(etag) = fallback_headers.headers().lookup1(header::ETAG) {
                    if !etag.starts_with(ps_http::http_cache::ETAG_PREFIX) {
                        req_headers.headers_mut().add(header::IF_NONE_MATCH, &etag);
                        conditional_added = true;
                    }
                }
                if let Some(modified) = fallback_headers.headers().lookup1(header::LAST_MODIFIED) {
                    req_headers
                        .headers_mut()
                        .add(header::IF_MODIFIED_SINCE, &modified);
                    conditional_added = true;
                }
            }
        }
        self.hooks.prepare_request(ctx, &mut req_headers);
        let options = self.driver.options();
        let owned_fallback = fallback.map(|(headers, body)| (headers.clone(), body.to_vec()));
        Some(FetchPlan {
            fetch_url,
            req_headers,
            http_options: self.http_options(),
            request_context: ctx.clone(),
            fallback: owned_fallback,
            conditional_added,
            serve_stale: options.serve_stale_if_fetch_error(),
            cache_key: self.cache_key.clone(),
            fragment: self.driver.cache_fragment().to_string(),
            override_ttl_ms: options.override_caching_ttl_ms(),
            ttl_overridden: options.is_cache_ttl_overridden(&self.url),
            respect_vary: self.respect_vary,
            min_cache_time_ms: options.min_resource_cache_time_to_rewrite_ms(),
            force_caching: self.server().http_cache().force_caching(),
            now_ms: self.now_ms(),
        })
    }

    /// Caches a fetch outcome (`AddToCache`), returning whether it
    /// stuck plus the classification for the caller to record.
    fn add_to_cache(
        &self,
        headers: &mut ResponseHeaders,
        body: &[u8],
        wrote: bool,
        fetch_success: bool,
    ) -> (bool, FetchStatus) {
        let options = self.driver.options();
        store_fetch_outcome(
            self.server().http_cache(),
            &self.hooks,
            headers,
            body,
            wrote,
            fetch_success,
            &self.cache_key,
            self.driver.cache_fragment(),
            self.http_options(),
            options.override_caching_ttl_ms(),
            options.is_cache_ttl_overridden(&self.url),
            self.respect_vary,
            options.min_resource_cache_time_to_rewrite_ms(),
            self.server().http_cache().force_caching(),
            self.now_ms(),
        )
    }

    fn update_input_info_for_freshen(
        &self,
        headers: &ResponseHeaders,
        body: &[u8],
        callback: &mut dyn FreshenCallback,
    ) -> bool {
        let Some(input) = callback.input_info_mut() else {
            return false;
        };
        if !input.has_input_content_hash() || !self.is_valid_and_cacheable_impl(headers) {
            return false;
        }
        use ps_cache::hash::Hasher;
        let fresh_hash = self.server().contents_hasher().hash(body);
        if Some(&fresh_hash) == input.input_content_hash.as_ref() {
            Self::fill_in_from_headers(headers, input);
            true
        } else {
            false
        }
    }
}

impl<H: ResourceHooks> std::fmt::Debug for CacheableResourceBase<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheableResourceBase")
            .field("url", &self.url)
            .field("cache_key", &self.cache_key)
            .field("status", &self.headers.status_code())
            .field("fetch_status", &self.fetch_status)
            .finish_non_exhaustive()
    }
}

impl<H: ResourceHooks> Resource for CacheableResourceBase<H> {
    fn url(&self) -> &str {
        &self.url
    }

    fn is_authorized_domain(&self) -> bool {
        self.authorized
    }

    fn set_is_authorized_domain(&mut self, authorized: bool) {
        self.authorized = authorized;
    }
}
