//! Shared rewrite host (`ServerContext`, resource slice).
//!
//! Owns the HTTP cache, fetcher, clock, statistics, and content
//! hashers shared by every resource and driver. Fetching, property
//! caches, finders, and serving land in later slices; this file grows
//! into the full context.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use std::collections::HashMap;

use ps_cache::cache::Cache as CacheBackend;
use ps_cache::hash::{Hasher, Md5Hasher};
use ps_cache::namer::ResourceNamer;
use ps_cache::timer::MockTimer;
use ps_config::options::RewriteOptions;
use ps_http::content_type::{extension_to_content_type, MAX_EXTENSION_LEN};
use ps_http::fetch::AsyncFetcher;
use ps_http::google_url::GoogleUrl;
use ps_http::http_cache::HttpCache;
use ps_http::request::{RequestContext, RequestHeaders};
use ps_http::response::ResponseHeaders;
use ps_http::user_agent_normalizer::{
    AndroidUserAgentNormalizer, IEUserAgentNormalizer, UserAgentNormalizer,
};
use ps_stats::core::{Statistics, UpDownCounter, Variable};

use super::domain_lawyer::DomainLawyer;
use super::element::NodeId;
use super::file_input_resource::FileInputResource;
use super::filters::domain_rewrite::{rewrite_url, RewriteResult as DomainRewriteResult};
use super::filters::url_left_trim::UrlLeftTrimFilter;
use super::output_resource::{OutputResource, OutputResourceKind};
use super::parser::Dom;
use super::resource::{DataUrlInputResource, InputResource, Resource};
use super::resource_slot::HtmlResourceSlot;
use super::rewrite_filter::RewriteFilter;
use super::url_input_resource::make_url_input_resource;
use super::url_namer::{ProxyExtent, StandardUrlNamer, UrlNamer};
use super::url_segment_encoder::UrlSegmentEncoder;

/// Inline authorization policy (`InlineAuthorizationPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineAuthorizationPolicy {
    /// Unauthorized URLs may still inline.
    InlineUnauthorizedResources,
    /// Only authorized URLs rewrite.
    InlineOnlyAuthorizedResources,
}

/// Intended use (`IntendedFor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntendedFor {
    /// Inlined into the page.
    Inlining,
    /// General rewriting.
    General,
}

/// CSS URL resolution outcome (`CssResolutionStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssResolutionStatus {
    /// Bases match; nothing to do.
    NoResolutionNeeded,
    /// URLs rewritten (bytes returned).
    Success,
    /// Transform failed.
    WriteFailed,
}

/// Rewrites CSS URLs across bases (`RewriteDomainTransformer`).
pub(crate) struct RewriteDomainTransformer<'a> {
    old_base: &'a GoogleUrl,
    new_base: &'a GoogleUrl,
    lawyer: &'a DomainLawyer,
    options: &'a RewriteOptions,
    namer: &'a dyn UrlNamer,
    hash_length: usize,
    signature_length: usize,
    trim_urls: bool,
}

impl<'a> RewriteDomainTransformer<'a> {
    /// Builds a rebasing transformer (`RewriteDomainTransformer`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        old_base: &'a GoogleUrl,
        new_base: &'a GoogleUrl,
        lawyer: &'a DomainLawyer,
        options: &'a RewriteOptions,
        namer: &'a dyn UrlNamer,
        hash_length: usize,
        signature_length: usize,
        trim_urls: bool,
    ) -> Self {
        RewriteDomainTransformer {
            old_base,
            new_base,
            lawyer,
            options,
            namer,
            hash_length,
            signature_length,
            trim_urls,
        }
    }
}

impl ps_css::scanner::Transformer for RewriteDomainTransformer<'_> {
    fn transform(&mut self, url: &mut Vec<u8>) -> ps_css::scanner::TransformStatus {
        use ps_css::scanner::TransformStatus;
        // Fragment-only references name the using document, not the
        // stylesheet location, so they stay as written.
        if url.first() == Some(&b'#') {
            return TransformStatus::NoChange;
        }
        let (result, rewritten) = rewrite_url(
            self.lawyer,
            self.options,
            self.namer,
            self.hash_length,
            self.signature_length,
            url,
            self.old_base,
            true,
            true,
        );
        if result == DomainRewriteResult::Fail {
            return TransformStatus::Failure;
        }
        let text = String::from_utf8_lossy(&rewritten);
        let out = if self.trim_urls {
            UrlLeftTrimFilter::trim(self.new_base, &text).unwrap_or_else(|| text.into_owned())
        } else {
            text.into_owned()
        };
        if out.as_bytes() == url.as_slice() {
            TransformStatus::NoChange
        } else {
            *url = out.into_bytes();
            TransformStatus::Success
        }
    }
}

/// Input role for CSP checks (`InputRole`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputRole {
    /// Scripts.
    Script,
    /// Stylesheets.
    Style,
    /// Images.
    Img,
    /// Unknown (most conservative).
    Unknown,
    /// Reconstruction (unchecked).
    Reconstruction,
}

/// Multiply-with-carry generator (`SimpleRandom`).
#[derive(Debug, Clone)]
pub struct SimpleRandom {
    z: u32,
    w: u32,
}

impl SimpleRandom {
    /// Seeds the generator (C++ seeds `z=10, w=25`).
    #[must_use]
    pub fn new() -> Self {
        SimpleRandom { z: 10, w: 25 }
    }

    /// Draws the next pseudo-random number (`Next`).
    pub fn next(&mut self) -> u32 {
        self.z = 36969u32
            .wrapping_mul(self.z & 65535)
            .wrapping_add(self.z >> 16);
        self.w = 18000u32
            .wrapping_mul(self.w & 65535)
            .wrapping_add(self.w >> 16);
        self.z.wrapping_shl(16).wrapping_add(self.w)
    }
}

impl Default for SimpleRandom {
    fn default() -> Self {
        Self::new()
    }
}

/// Message sink for background completions (`MessageHandler`
/// shape without the nginx log): pool workers report image
/// rewrite records here so the admin images page sees
/// fire-and-forget optimizations, not just request-thread ones.
/// Implemented by the server layer over its message buffer.
pub trait MessageSink: Send + Sync + std::fmt::Debug {
    /// Buffers one message line.
    fn message(&self, level: ps_config::message_handler::MessageLevel, text: String);
}

/// Shared rewrite host (`ServerContext`).
pub struct ServerContext {
    http_cache: Arc<HttpCache>,
    metadata_cache: Arc<dyn CacheBackend>,
    fetcher: Arc<dyn AsyncFetcher>,
    timer: Rc<RefCell<MockTimer>>,
    statistics: Arc<Statistics>,
    contents_hasher: Md5Hasher,
    hasher: Arc<dyn Hasher>,
    lock_hasher: Md5Hasher,
    url_namer: Rc<dyn UrlNamer>,
    simple_random: Rc<RefCell<SimpleRandom>>,
    ua_normalizers: Vec<Rc<dyn UserAgentNormalizer>>,
    critical_selector_finder:
        RefCell<Option<Rc<dyn super::critical_selector_finder::CriticalSelectorFinder>>>,
    background_pool: std::sync::OnceLock<Arc<super::background::BackgroundPool>>,
    message_sink: std::sync::OnceLock<Arc<dyn MessageSink>>,
}

impl ServerContext {
    /// Builds a context over explicit components (tests inject a mock
    /// fetcher, memory cache, and controllable clock).
    #[must_use]
    pub fn new(
        backend: Arc<dyn CacheBackend>,
        fetcher: Arc<dyn AsyncFetcher>,
        start_ms: i64,
    ) -> Self {
        Self::new_with_metadata(
            backend,
            Arc::new(ps_cache::cache::MemoryCache::new()),
            fetcher,
            start_ms,
        )
    }

    /// Builds a worker-side context over shared handles (background
    /// HTML-rewrite jobs): the exact shared HTTP cache (config must
    /// match the requesting context — a fresh `HttpCache` over the
    /// same backend would diverge), metadata, fetcher, statistics,
    /// and hasher. The clock is a frozen snapshot and the namer is
    /// the standard one (production always uses it).
    pub(crate) fn new_shared(
        http_cache: Arc<HttpCache>,
        metadata: Arc<dyn CacheBackend>,
        fetcher: Arc<dyn AsyncFetcher>,
        statistics: Arc<Statistics>,
        hasher: Arc<dyn Hasher>,
        start_ms: i64,
    ) -> Self {
        ServerContext {
            http_cache,
            metadata_cache: metadata,
            fetcher,
            timer: Rc::new(RefCell::new(MockTimer::new(start_ms))),
            statistics,
            contents_hasher: Md5Hasher::with_size(21),
            hasher,
            lock_hasher: Md5Hasher::with_size(20),
            url_namer: Rc::new(StandardUrlNamer),
            simple_random: Rc::new(RefCell::new(SimpleRandom::new())),
            ua_normalizers: vec![
                Rc::new(AndroidUserAgentNormalizer::new()) as Rc<dyn UserAgentNormalizer>,
                Rc::new(IEUserAgentNormalizer::new()) as Rc<dyn UserAgentNormalizer>,
            ],
            critical_selector_finder: RefCell::new(None),
            background_pool: std::sync::OnceLock::new(),
            message_sink: std::sync::OnceLock::new(),
        }
    }

    /// Builds a context with an explicit metadata cache (tests that
    /// flush or inspect metadata hold the backend).
    #[must_use]
    pub fn new_with_metadata(
        backend: Arc<dyn CacheBackend>,
        metadata: Arc<dyn CacheBackend>,
        fetcher: Arc<dyn AsyncFetcher>,
        start_ms: i64,
    ) -> Self {
        let statistics = Arc::new(Statistics::new());
        // Full console roster (`RewriteStats::InitStats` plus every
        // filter/cache/fetcher `InitStats`).
        super::rewrite_stats::init_stats(&statistics);
        let hasher: Arc<dyn Hasher> = Arc::new(Md5Hasher::new());
        let mut http_cache = HttpCache::new(backend);
        http_cache.set_hasher(hasher.clone());
        ServerContext {
            http_cache: Arc::new(http_cache),
            metadata_cache: metadata,
            fetcher,
            timer: Rc::new(RefCell::new(MockTimer::new(start_ms))),
            statistics,
            contents_hasher: Md5Hasher::with_size(21),
            hasher,
            // `kHashBytes` (20); never mocked, even in tests.
            lock_hasher: Md5Hasher::with_size(20),
            url_namer: Rc::new(StandardUrlNamer),
            simple_random: Rc::new(RefCell::new(SimpleRandom::new())),
            ua_normalizers: vec![
                Rc::new(AndroidUserAgentNormalizer::new()) as Rc<dyn UserAgentNormalizer>,
                Rc::new(IEUserAgentNormalizer::new()) as Rc<dyn UserAgentNormalizer>,
            ],
            critical_selector_finder: RefCell::new(None),
            background_pool: std::sync::OnceLock::new(),
            message_sink: std::sync::OnceLock::new(),
        }
    }

    /// Draws from the shared generator (`simple_random()->Next()`).
    pub fn simple_random_next(&self) -> u32 {
        self.simple_random.borrow_mut().next()
    }

    /// Borrows the HTTP cache.
    #[must_use]
    pub fn http_cache(&self) -> &HttpCache {
        self.http_cache.as_ref()
    }

    /// Shares the HTTP cache (background completion stores here).
    #[must_use]
    pub fn http_cache_shared(&self) -> Arc<HttpCache> {
        Arc::clone(&self.http_cache)
    }

    /// Mutably borrows the HTTP cache (host applies its config
    /// before sharing; reconfiguring a shared cache is a bug).
    pub fn http_cache_mut(&mut self) -> &mut HttpCache {
        Arc::get_mut(&mut self.http_cache).expect("http_cache already shared")
    }

    /// Installs the background rewrite pool (factory-owned, once).
    pub fn set_background_pool(&self, pool: Arc<super::background::BackgroundPool>) {
        let _ = self.background_pool.set(pool);
    }

    /// Borrows the background rewrite pool, if installed.
    #[must_use]
    pub fn background_pool(&self) -> Option<Arc<super::background::BackgroundPool>> {
        self.background_pool.get().cloned()
    }

    /// Installs the background message sink (server-owned, once).
    pub fn set_message_sink(&self, sink: Arc<dyn MessageSink>) {
        let _ = self.message_sink.set(sink);
    }

    /// Borrows the background message sink, if installed.
    #[must_use]
    pub fn message_sink(&self) -> Option<Arc<dyn MessageSink>> {
        self.message_sink.get().cloned()
    }

    /// Borrows the UA normalizers (`user_agent_normalizers`).
    #[must_use]
    pub fn user_agent_normalizers(&self) -> &[Rc<dyn UserAgentNormalizer>] {
        &self.ua_normalizers
    }

    /// Installs the critical-selector finder
    /// (`set_critical_selector_finder`).
    pub fn set_critical_selector_finder(
        &self,
        finder: Rc<dyn super::critical_selector_finder::CriticalSelectorFinder>,
    ) {
        *self.critical_selector_finder.borrow_mut() = Some(finder);
    }

    /// Borrows the critical-selector finder, if any.
    #[must_use]
    pub fn critical_selector_finder(
        &self,
    ) -> Option<Rc<dyn super::critical_selector_finder::CriticalSelectorFinder>> {
        self.critical_selector_finder.borrow().clone()
    }

    /// Borrows the fetcher.
    #[must_use]
    pub fn fetcher(&self) -> &Arc<dyn AsyncFetcher> {
        &self.fetcher
    }

    /// Borrows the shared clock.
    #[must_use]
    pub fn timer(&self) -> &Rc<RefCell<MockTimer>> {
        &self.timer
    }

    /// Current time in milliseconds.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.timer.borrow().now_ms()
    }

    /// Borrows the statistics registry.
    #[must_use]
    pub fn statistics(&self) -> &Statistics {
        self.statistics.as_ref()
    }

    /// Clones the statistics registry handle (for backends that
    /// outlive the borrow, like the IPRO recording backend).
    #[must_use]
    pub fn statistics_shared(&self) -> Arc<Statistics> {
        Arc::clone(&self.statistics)
    }

    /// Shares a statistics registry (worker-global aggregation;
    /// contexts otherwise keep their own).
    pub fn set_statistics(&mut self, statistics: Arc<Statistics>) {
        self.statistics = statistics;
    }

    /// Looks up a counter, adding it when absent.
    pub fn variable(&self, name: &str) -> Arc<Variable> {
        self.statistics.add_variable(name)
    }

    /// Looks up a bidirectional counter, adding it when absent
    /// (`GetUpDownCounter`).
    pub fn histogram(&self, name: &str) -> Arc<ps_stats::Histogram> {
        self.statistics.add_histogram(name)
    }

    pub fn up_down_counter(&self, name: &str) -> Arc<UpDownCounter> {
        self.statistics.add_up_down_counter(name)
    }

    /// Borrows the content hasher (`contents_hasher`).
    #[must_use]
    pub fn contents_hasher(&self) -> &Md5Hasher {
        &self.contents_hasher
    }

    /// Borrows the naming hasher (`hasher`, output URLs).
    #[must_use]
    pub fn hasher(&self) -> &Arc<dyn Hasher> {
        &self.hasher
    }

    /// Installs a hasher (`set_hasher`, tests use constants).
    pub fn set_hasher(&mut self, hasher: Arc<dyn Hasher>) {
        self.hasher = hasher.clone();
        self.http_cache_mut().set_hasher(hasher);
    }

    /// Borrows the lock hasher (`lock_hasher`, partition keys and
    /// lock names; a real MD5 even under test hashers).
    #[must_use]
    pub fn lock_hasher(&self) -> &Md5Hasher {
        &self.lock_hasher
    }

    /// Borrows the URL namer (`url_namer`).
    #[must_use]
    pub fn url_namer(&self) -> &Rc<dyn UrlNamer> {
        &self.url_namer
    }

    /// Installs a URL namer (tests use the path-encoding namer).
    pub fn set_url_namer(&mut self, namer: Rc<dyn UrlNamer>) {
        self.url_namer = namer;
    }

    /// Borrows the metadata cache (rewrite partition tables).
    #[must_use]
    pub fn metadata_cache(&self) -> &Arc<dyn CacheBackend> {
        &self.metadata_cache
    }

    /// Installs a metadata cache backend.
    pub fn set_metadata_cache(&mut self, cache: Arc<dyn CacheBackend>) {
        self.metadata_cache = cache;
    }

    /// Long-cache output headers (`SetDefaultLongCacheHeaders`).
    pub fn set_default_long_cache_headers(
        &self,
        content_type: Option<ps_http::content_type::ContentType>,
        charset: &str,
        suffix: &str,
        headers: &mut ps_http::response::ResponseHeaders,
    ) {
        headers.set_first_line(1, 1, 200, "OK");
        headers
            .headers_mut()
            .remove_all(ps_http::names::header::CONTENT_TYPE);
        if let Some(content_type) = content_type {
            let mut value = content_type.mime_type().to_string();
            if !charset.is_empty() {
                value.push_str("; charset=");
                value.push_str(charset);
            }
            headers
                .headers_mut()
                .add(ps_http::names::header::CONTENT_TYPE, &value);
        }
        headers.set_date_and_caching(self.now_ms(), ps_cache::timer::YEAR_MS, suffix);
        headers
            .headers_mut()
            .replace(ps_http::names::header::ETAG, "W/\"0\"");
        if headers
            .headers()
            .lookup1(ps_http::names::header::LAST_MODIFIED)
            .is_none()
        {
            headers.set_last_modified(self.now_ms());
        }
        headers.compute_caching();
    }

    /// Whether a URL names a pagespeed resource
    /// (`IsPagespeedResource`, structural: decodable leaf, hash, and
    /// known filter id; the driver refines this with live filters).
    #[must_use]
    pub fn is_pagespeed_resource(&self, url: &GoogleUrl) -> bool {
        if !url.is_web_valid() {
            return false;
        }
        let leaf = url.leaf_sans_query();
        let mut namer = ResourceNamer::new();
        let hash_len = self.hasher.hash_size_in_chars();
        #[allow(clippy::cast_possible_wrap)]
        let hash_len = hash_len as i32;
        if !namer.decode(&leaf, hash_len, 0) || namer.hash().is_empty() {
            return false;
        }
        let id = namer.id();
        id == "co" || id == "jo" || ps_config::filter::Filter::from_id(id).is_some()
    }

    /// Copies non-caching headers input→output
    /// (`MergeNonCachingResponseHeaders`).
    pub fn merge_non_caching_response_headers(
        &self,
        input: &ps_http::response::ResponseHeaders,
        output: &mut ps_http::response::ResponseHeaders,
    ) {
        use ps_http::names::header;
        // `kExcludedAttributes`, case-insensitive.
        const EXCLUDED: &[&str] = &[
            header::CACHE_CONTROL,
            header::CONTENT_ENCODING,
            header::CONTENT_LENGTH,
            header::CONTENT_TYPE,
            header::DATE,
            header::ETAG,
            header::EXPIRES,
            header::LAST_MODIFIED,
            header::SET_COOKIE,
            header::SET_COOKIE2,
            header::TRANSFER_ENCODING,
            header::VARY,
        ];
        for (name, value) in input.headers().iter() {
            if EXCLUDED
                .iter()
                .any(|excluded| name.eq_ignore_ascii_case(excluded))
            {
                continue;
            }
            output.headers_mut().add(name, value);
        }
    }

    /// Narrows output caching to the inputs'
    /// (`ApplyInputCacheControl`).
    pub fn apply_input_cache_control(
        &self,
        inputs: &[super::resource::InputResource],
        headers: &mut ps_http::response::ResponseHeaders,
    ) {
        use ps_http::names::header;
        use ps_http::response::{RequestProperties, ValidatorOption, VaryOption};
        headers.compute_caching();
        let mut proxy_cacheable = headers.is_proxy_cacheable(
            RequestProperties::default(),
            VaryOption::IgnoreVaryOnResources,
            ValidatorOption::HasValidator,
        );
        let mut browser_cacheable = headers.is_browser_cacheable();
        let mut no_store = headers.has_cache_control_directive("no-store");
        let mut is_public = true;
        let mut saw_nonempty_resource = false;
        let mut max_age = headers.cache_ttl_ms();
        for input in inputs {
            // Only HTTP-OK inputs constrain the output; unloaded or
            // failed inputs are skipped.
            if !input.http_status_ok() {
                continue;
            }
            let input_headers = input.response_headers();
            // NOTE: Clone-then-compute: the shared borrow cannot mutate.
            let mut computed = input_headers.clone();
            computed.compute_caching();
            max_age = max_age.min(computed.cache_ttl_ms());
            proxy_cacheable &= computed.is_proxy_cacheable(
                RequestProperties::default(),
                VaryOption::IgnoreVaryOnResources,
                ValidatorOption::HasValidator,
            );
            browser_cacheable &= computed.is_browser_cacheable();
            no_store |= computed.has_cache_control_directive("no-store");
            is_public &= computed
                .headers()
                .has_value_case_insensitive(header::CACHE_CONTROL, "public");
            saw_nonempty_resource = true;
        }
        if proxy_cacheable {
            if is_public && saw_nonempty_resource {
                // Append `public` unless a visibility verdict exists
                // (`SetCacheControlPublic`, no max-age form).
                let has_verdict =
                    headers
                        .headers()
                        .lookup(header::CACHE_CONTROL)
                        .is_some_and(|values| {
                            values.iter().any(|value| {
                                ["private", "public", "no-cache", "no-store"]
                                    .iter()
                                    .any(|verdict| value.eq_ignore_ascii_case(verdict))
                            })
                        });
                if !has_verdict {
                    let joined = headers
                        .headers()
                        .lookup(header::CACHE_CONTROL)
                        .map(|values| values.join(", "))
                        .unwrap_or_default();
                    let value = if joined.is_empty() {
                        "public".to_string()
                    } else {
                        format!("{joined}, public")
                    };
                    headers.headers_mut().replace(header::CACHE_CONTROL, &value);
                }
            }
        } else {
            let directives;
            if browser_cacheable {
                directives = ",private";
            } else {
                max_age = 0;
                directives = if no_store {
                    ",no-cache,no-store"
                } else {
                    ",no-cache"
                };
            }
            headers.set_date_and_caching(headers.date_ms(), max_age, directives);
            headers.headers_mut().remove(header::ETAG, "W/\"0\"");
            headers.compute_caching();
        }
    }

    /// Sums input sizes (`AddOriginalContentLengthHeader`).
    pub fn add_original_content_length_header(
        &self,
        inputs: &[super::resource::InputResource],
        headers: &mut ps_http::response::ResponseHeaders,
    ) {
        use ps_http::names::header;
        let mut input_size: i64 = 0;
        let mut all_known = !inputs.is_empty();
        for input in inputs {
            let input_headers = input.response_headers();
            if let Some(known) = input_headers
                .headers()
                .lookup1(header::X_ORIGINAL_CONTENT_LENGTH)
            {
                if let Ok(size) = known.parse::<i64>() {
                    input_size += size;
                    continue;
                }
            }
            // Unloaded inputs leave the total unknown.
            if input_headers.status_code() != 0 {
                input_size += input.contents().len() as i64;
            } else {
                all_known = false;
            }
        }
        if all_known {
            headers.set_original_content_length(input_size);
        }
    }
}

/// Per-page driver view for resources (`RewriteDriver`, resource slice).
///
/// Carries what resource loading reads off the driver: live options,
/// the base URL (fetch referers), the cache fragment, and the bot
/// verdict. Merges into the HTML driver once chains own resources.
pub struct ResourceDriver {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    base_url: GoogleUrl,
    cache_fragment: String,
    is_bot: bool,
    request_context: RefCell<RequestContext>,
    lawyer: DomainLawyer,
    claimants: RefCell<Vec<Rc<dyn Fn(&GoogleUrl) -> bool>>>,
    slots: RefCell<HashMap<(NodeId, usize), Rc<RefCell<HtmlResourceSlot>>>>,
    claimed_elements: RefCell<std::collections::HashSet<NodeId>>,
    filters: RefCell<HashMap<&'static str, Rc<dyn RewriteFilter>>>,
    debug_output_bytes: RefCell<i64>,
    debug_output_truncated: RefCell<bool>,
    debug_truncation_noted: RefCell<bool>,
    containing_charset: RefCell<String>,
    is_amp: RefCell<bool>,
    xhtml_status: RefCell<XhtmlStatus>,
    response_headers: Rc<RefCell<ResponseHeaders>>,
    user_agent: RefCell<String>,
    request_headers: RefCell<RequestHeaders>,
    image_records: RefCell<Vec<ImageRewriteRecord>>,
    beacon_evidence_identity: RefCell<Option<Vec<u8>>>,
    fetch_budget_ms: RefCell<Option<i64>>,
    ipro_serving: Cell<bool>,
    detached_rewrites: Cell<u64>,
}

/// One successful image rewrite, as listed by the admin images
/// page (`ImageRewriteRow`): the `message_line` rendering is the
/// single source of truth for the `PS_IMAGE_DETAIL` marker format
/// the admin parser consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRewriteRecord {
    /// Rewrite completion time in ms.
    pub timestamp_ms: i64,
    /// Input size in bytes.
    pub original_bytes: i64,
    /// Output size in bytes.
    pub optimized_bytes: i64,
    /// Output MIME type.
    pub output_mime_type: String,
    /// Input URL.
    pub original_url: String,
    /// Rewritten URL.
    pub optimized_url: String,
}

impl ImageRewriteRecord {
    /// Marker prefix (`PS_IMAGE_DETAIL\t`).
    pub const MARKER: &'static str = "PS_IMAGE_DETAIL\t";

    /// Renders the message-buffer line (marker plus six tab
    /// fields: timestamp, sizes, MIME, original URL, output URL).
    #[must_use]
    pub fn message_line(&self) -> String {
        format!(
            "{}{}\t{}\t{}\t{}\t{}\t{}",
            Self::MARKER,
            self.timestamp_ms,
            self.original_bytes,
            self.optimized_bytes,
            self.output_mime_type,
            self.original_url,
            self.optimized_url,
        )
    }
}

/// Response-mime XHTML verdict (`RewriteDriver::XhtmlStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XhtmlStatus {
    /// No mimetype known.
    #[default]
    Unknown,
    /// XHTML mime type.
    IsXhtml,
    /// Non-XML mime type.
    IsNotXhtml,
}

/// Comments per list cap (`kMaxDebugCommentsPerList`).
const MAX_DEBUG_COMMENTS_PER_LIST: usize = 32;

/// Comment delimiter bytes (`kCommentDelimiterBytes`, `<!--` + `-->`).
const COMMENT_DELIMITER_BYTES: i64 = 7;

/// RAII marker for in-place serving: while alive, fetch waits use
/// the IPRO deadline when waiting is enabled.
pub struct IproScope<'a> {
    driver: &'a ResourceDriver,
    previous: bool,
}

impl Drop for IproScope<'_> {
    fn drop(&mut self) {
        self.driver.ipro_serving.set(self.previous);
    }
}

impl ResourceDriver {
    /// Builds a driver view over a server context.
    #[must_use]
    pub fn new(
        server: Rc<ServerContext>,
        options: Rc<RefCell<RewriteOptions>>,
        base_url: &str,
    ) -> Self {
        let mut driver = ResourceDriver {
            server,
            options,
            base_url: GoogleUrl::new(base_url),
            cache_fragment: String::new(),
            is_bot: false,
            request_context: RefCell::new(RequestContext::new(
                ps_http::response::HttpOptions::default_for_tests(),
            )),
            lawyer: DomainLawyer::new(),
            claimants: RefCell::new(Vec::new()),
            slots: RefCell::new(HashMap::new()),
            claimed_elements: RefCell::new(std::collections::HashSet::new()),
            filters: RefCell::new(HashMap::new()),
            debug_output_bytes: RefCell::new(0),
            debug_output_truncated: RefCell::new(false),
            debug_truncation_noted: RefCell::new(false),
            containing_charset: RefCell::new(String::new()),
            is_amp: RefCell::new(false),
            xhtml_status: RefCell::new(XhtmlStatus::Unknown),
            response_headers: Rc::new(RefCell::new(ResponseHeaders::default())),
            user_agent: RefCell::new(String::new()),
            request_headers: RefCell::new(RequestHeaders::new()),
            image_records: RefCell::new(Vec::new()),
            beacon_evidence_identity: RefCell::new(None),
            fetch_budget_ms: RefCell::new(None),
            ipro_serving: Cell::new(false),
            detached_rewrites: Cell::new(0),
        };
        driver.rebuild_lawyer();
        driver
    }

    /// Wait budget for one background fetch: `None` means unbounded
    /// (deadline disabled, C++ parity), otherwise milliseconds
    /// remaining in this flush window (possibly zero). Serving an
    /// in-place resource with waiting enabled swaps in the IPRO
    /// deadline (C++
    /// `InPlaceRewriteContext::GetRewriteDeadlineAlarmMs`).
    pub fn fetch_wait_budget_ms(&self) -> Option<i64> {
        let options = self.options();
        let configured = if self.ipro_serving.get() && options.in_place_wait_for_optimized() {
            options.in_place_rewrite_deadline_ms()
        } else {
            options.rewrite_deadline_per_flush_ms()
        };
        if configured <= 0 {
            return None;
        }
        let mut budget = self.fetch_budget_ms.borrow_mut();
        Some((*budget.get_or_insert(configured)).max(0))
    }

    /// Marks IPRO serving for this driver until the guard drops
    /// (restores nesting correctly).
    pub fn enter_ipro_scope(&self) -> IproScope<'_> {
        let previous = self.ipro_serving.replace(true);
        IproScope {
            driver: self,
            previous,
        }
    }

    /// Whether this driver is serving an in-place resource (as
    /// opposed to rewriting HTML).
    #[must_use]
    pub fn is_ipro_serving(&self) -> bool {
        self.ipro_serving.get()
    }

    /// Wait budget for background work issued while rewriting HTML:
    /// the remaining per-flush deadline, or zero when deadlines are
    /// disabled. Unlike [`Self::fetch_wait_budget_ms`] (which returns
    /// `None` for "wait forever" under an explicit disable, C++
    /// parity for the resource path), the HTML path is never
    /// unbounded: with deadlines disabled the HTML request does not
    /// wait at all (fire-and-forget; the background completion warms
    /// the next request), because background optimization must never
    /// stall page delivery.
    #[must_use]
    pub fn html_wait_budget_ms(&self) -> i64 {
        match self.fetch_wait_budget_ms() {
            Some(remaining) => remaining.max(0),
            None => 0,
        }
    }

    /// Deducts waited milliseconds from the flush-window budget.
    pub fn deduct_fetch_wait_ms(&self, spent_ms: i64) {
        if let Some(budget) = self.fetch_budget_ms.borrow_mut().as_mut() {
            *budget -= spent_ms.max(0);
        }
    }

    /// Restores the full flush-window budget (C++ rearms per window).
    pub fn reset_fetch_budget(&self) {
        *self.fetch_budget_ms.borrow_mut() = None;
    }

    /// Records a successful image rewrite for the admin images page
    /// (drained into the message buffer at request completion).
    pub fn record_image_rewrite(&self, record: ImageRewriteRecord) {
        self.image_records.borrow_mut().push(record);
    }

    /// Takes accumulated image rewrite records, leaving the list empty.
    #[must_use]
    pub fn take_image_records(&self) -> Vec<ImageRewriteRecord> {
        std::mem::take(&mut *self.image_records.borrow_mut())
    }

    /// Snapshots the post-render beacon evidence identity (the
    /// beacon filter writes this after candidate registration so
    /// the HTML cache can detect mid-render evidence changes).
    pub fn set_beacon_evidence_identity(&self, identity: Vec<u8>) {
        *self.beacon_evidence_identity.borrow_mut() = Some(identity);
    }

    /// Reads the post-render beacon evidence identity, if the
    /// beacon filter recorded one.
    #[must_use]
    pub fn beacon_evidence_identity(&self) -> Option<Vec<u8>> {
        self.beacon_evidence_identity.borrow().clone()
    }

    /// Records a detached rewrite (a context that finalized with
    /// deadline-pending inputs and served fallback content). The
    /// HTML cache refuses to publish such renders
    /// (`num_detached_rewrites`).
    pub fn note_detached_rewrite(&self) {
        self.detached_rewrites.set(self.detached_rewrites.get() + 1);
    }

    /// Detached rewrites finalized on this driver.
    #[must_use]
    pub fn detached_rewrites(&self) -> u64 {
        self.detached_rewrites.get()
    }

    /// Reads the request user agent (`user_agent`).
    #[must_use]
    pub fn user_agent(&self) -> String {
        self.user_agent.borrow().clone()
    }

    /// Installs the request user agent (tests staging UA-sensitive
    /// resources call this instead of `SetCurrentUserAgent`).
    pub fn set_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
    }

    /// Installs the request headers (`SetRequestHeaders`).
    pub fn set_request_headers(&self, headers: RequestHeaders) {
        *self.request_headers.borrow_mut() = headers;
    }

    /// Borrows the request headers.
    #[must_use]
    pub fn request_headers(&self) -> std::cell::Ref<'_, RequestHeaders> {
        self.request_headers.borrow()
    }

    /// Computes request capabilities (`request_properties`, rebuilt
    /// per call; C++ caches on the driver).
    #[must_use]
    pub fn request_properties(&self) -> super::request_properties::RequestProperties {
        let mut properties = super::request_properties::RequestProperties::new();
        properties.set_user_agent(&self.user_agent.borrow());
        properties.parse_request_headers(&self.request_headers.borrow());
        properties
    }

    /// Borrows the containing document headers (`response_headers`,
    /// set by the server layer or tests).
    #[must_use]
    pub fn response_headers(&self) -> std::cell::Ref<'_, ResponseHeaders> {
        self.response_headers.borrow()
    }

    /// Installs the containing document headers
    /// (`set_response_headers_ptr`, tests staging HTML charsets).
    pub fn set_response_headers(&self, headers: ResponseHeaders) {
        *self.response_headers.borrow_mut() = headers;
    }

    /// Shares the containing document headers with driver-bound
    /// filters (`MetaTagFilter` mutates these mid-parse; the
    /// handle keeps both sides live).
    #[must_use]
    pub fn response_headers_shared(&self) -> Rc<RefCell<ResponseHeaders>> {
        self.response_headers.clone()
    }

    /// Rebuilds the domain lawyer from options (`domain_lawyer()`
    /// reads live options in C++; the port snapshots at construction
    /// and after mapping changes).
    pub fn rebuild_lawyer(&mut self) {
        use ps_config::options::DomainMappingKind;
        let mut lawyer = DomainLawyer::new();
        let options = self.options.borrow();
        lawyer.set_proxy_suffix(options.proxy_suffix());
        for domain in options.domains() {
            lawyer.add_domain(domain, None);
        }
        for (domain, shards) in options.shards() {
            lawyer.add_shard(domain, shards, None);
        }
        for mapping in options.domain_mappings() {
            if mapping.kind == DomainMappingKind::Rewrite {
                lawyer.add_rewrite_domain_mapping(&mapping.to, &mapping.from, None);
            } else if mapping.kind == DomainMappingKind::Origin {
                lawyer.add_origin_domain_mapping(
                    &mapping.to,
                    &mapping.from,
                    &mapping.host_header,
                    None,
                );
            } else if mapping.kind == DomainMappingKind::Proxy {
                lawyer.add_proxy_domain_mapping(
                    &mapping.from,
                    &mapping.to,
                    &mapping.host_header,
                    None,
                );
            }
        }
        drop(options);
        self.lawyer = lawyer;
    }

    /// Borrows the server context.
    #[must_use]
    pub fn server_context(&self) -> &Rc<ServerContext> {
        &self.server
    }

    /// Borrows the live options.
    #[must_use]
    pub fn options(&self) -> std::cell::Ref<'_, RewriteOptions> {
        self.options.borrow()
    }

    /// Whether resources may rewrite (`can_rewrite_resources`,
    /// strobed from metadata health; the port reads it live since
    /// contexts complete inline).
    #[must_use]
    pub fn can_rewrite_resources(&self) -> bool {
        self.server.metadata_cache().is_healthy()
    }

    /// Borrows the base URL.
    #[must_use]
    pub fn base_url(&self) -> &GoogleUrl {
        &self.base_url
    }

    /// Base URL with proxy encoding stripped (`decoded_base_url`).
    #[must_use]
    pub fn decoded_base_url(&self) -> GoogleUrl {
        let base = self.base_url.clone();
        if let Some(decoded) = self.server_context().url_namer().decode(&base) {
            GoogleUrl::new(&decoded)
        } else {
            base
        }
    }

    /// Borrows the cache fragment (`CacheFragment`).
    #[must_use]
    pub fn cache_fragment(&self) -> &str {
        &self.cache_fragment
    }

    /// Whether the request is a bot (`request_properties()->IsBot()`).
    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.is_bot
    }

    /// Borrows the shared request context.
    #[must_use]
    pub fn request_context(&self) -> &RefCell<RequestContext> {
        &self.request_context
    }

    /// Borrows the domain lawyer.
    #[must_use]
    pub fn lawyer(&self) -> &DomainLawyer {
        &self.lawyer
    }

    /// Mutably borrows the domain lawyer (test mappings).
    pub fn lawyer_mut(&mut self) -> &mut DomainLawyer {
        &mut self.lawyer
    }

    /// Installs domain mappings (`set_domain_lawyer` equivalent).
    pub fn set_lawyer(&mut self, lawyer: DomainLawyer) {
        self.lawyer = lawyer;
    }

    /// Registers a URL claimant (`AddResourceUrlClaimant`): dedicated
    /// handlers (fonts) veto regular input resources.
    pub fn add_resource_url_claimant(&self, claimant: Rc<dyn Fn(&GoogleUrl) -> bool>) {
        self.claimants.borrow_mut().push(claimant);
    }

    /// Whether any claimant takes a URL (`IsResourceUrlClaimed`).
    #[must_use]
    pub fn is_resource_url_claimed(&self, url: &GoogleUrl) -> bool {
        self.claimants.borrow().iter().any(|claim| claim(url))
    }

    /// Whether a load passes CSP (`IsLoadPermittedByCsp`): allow-all
    /// until the driver owns a CSP context.
    #[must_use]
    pub fn is_load_permitted_by_csp(&self, _url: &GoogleUrl, _role: InputRole) -> bool {
        true
    }

    /// Whether a URL may rewrite in this document (`MayRewriteUrl`),
    /// returning (may_rewrite, is_authorized_domain).
    pub fn may_rewrite_url(
        &self,
        domain_url: &GoogleUrl,
        input_url: &GoogleUrl,
        policy: InlineAuthorizationPolicy,
        intended_for: IntendedFor,
    ) -> (bool, bool) {
        let mut authorized = false;
        if domain_url.is_web_valid() {
            let options = self.options();
            if options.is_allowed(input_url.spec())
                || (intended_for == IntendedFor::Inlining
                    && options.is_allowed_when_inlining(input_url.spec()))
            {
                authorized = self.lawyer.is_domain_authorized(domain_url, input_url);
                if !authorized && policy == InlineAuthorizationPolicy::InlineUnauthorizedResources {
                    // Rewritable but unauthorized: the resource lands
                    // in the unauthorized cache namespace.
                    return (true, false);
                }
            }
        }
        (authorized, authorized)
    }

    /// Creates an input resource for a URL (`CreateInputResource`),
    /// returning the resource plus its authorization verdict.
    pub fn create_input_resource(
        self: &Rc<Self>,
        input_url: &GoogleUrl,
        policy: InlineAuthorizationPolicy,
        intended_for: IntendedFor,
        role: InputRole,
    ) -> (Option<InputResource>, bool) {
        let mut authorized = true;
        if input_url.scheme() == "data" {
            // Data URLs are never worth optimizing here; ignore quietly.
            return (None, authorized);
        }
        if !self.base_url.is_web_valid() {
            return (None, false);
        }
        if !self.is_load_permitted_by_csp(input_url, role) {
            return (None, false);
        }
        // Authorization compares against the decoded base, so
        // proxy-encoded drivers authorize their decoded origin.
        let decoded_base = self.decoded_base_url();
        let (mut may_rewrite, mut is_authorized) =
            self.may_rewrite_url(&decoded_base, input_url, policy, intended_for);
        authorized = is_authorized;
        if !may_rewrite {
            // Proxy re-encodings decode, then re-check.
            if let Some(decoded) = self.server.url_namer.decode(input_url) {
                let decoded_url = GoogleUrl::new(&decoded);
                (may_rewrite, is_authorized) =
                    self.may_rewrite_url(&decoded_base, &decoded_url, policy, intended_for);
                authorized = is_authorized;
            }
        }
        if !may_rewrite {
            self.server
                .variable("resource_url_domain_rejections")
                .add(1);
            return (None, authorized);
        }
        self.server
            .variable("resource_url_domain_acceptances")
            .add(1);
        (
            self.create_input_resource_unchecked(input_url, authorized),
            authorized,
        )
    }

    /// Creates an input resource without checks
    /// (`CreateInputResourceUnchecked`).
    pub fn create_input_resource_unchecked(
        self: &Rc<Self>,
        url: &GoogleUrl,
        authorized: bool,
    ) -> Option<InputResource> {
        if self.is_resource_url_claimed(url) {
            return None;
        }
        if url.scheme() == "data" {
            return DataUrlInputResource::make(url.spec()).map(InputResource::DataUrl);
        }
        if url.scheme() != "http" && url.scheme() != "https" {
            return None;
        }
        let content_type = extension_to_content_type(&url.leaf_sans_query());
        // File-load mappings win over HTTP fetching, like C++: the
        // mapped file must carry a known static type (C++
        // ShouldLoadFromFile's content-type gate), while the
        // resource keeps the URL leaf's type (possibly none).
        let options = self.options();
        if !options.file_load_policy().is_empty() {
            if let Some(filename) = options
                .file_load_policy()
                .map_url_to_filename(url.all_except_query())
            {
                if let Some(mapped_type) = extension_to_content_type(&filename) {
                    if mapped_type.is_likely_static_resource() {
                        let file_ttl_ms = options
                            .was_set("lfct")
                            .then(|| options.load_from_file_cache_ttl_ms());
                        return Some(InputResource::File(FileInputResource::make(
                            url.spec(),
                            &filename,
                            content_type,
                            authorized,
                            options.max_cacheable_response_content_length(),
                            file_ttl_ms,
                            self.server.now_ms(),
                        )));
                    }
                }
            }
        }
        let fetch_is_http_like = match self.lawyer.map_origin_url(url) {
            Some((mapped, _, _)) => GoogleUrl::new(&mapped),
            None => return None,
        };
        let https_ok = self.server.fetcher.supports_https();
        if fetch_is_http_like.scheme() != "http"
            && !(fetch_is_http_like.scheme() == "https" && https_ok)
        {
            return None;
        }
        Some(InputResource::Url(make_url_input_resource(
            self.clone(),
            content_type,
            url.spec(),
            authorized,
        )))
    }

    /// Fetches (or reuses) the slot for an attribute (`GetSlot`).
    pub fn get_slot(
        &self,
        resource: Option<super::resource::ResourceHandle>,
        dom: &mut Dom,
        element: NodeId,
        attribute: usize,
        driver_id: &str,
    ) -> Rc<RefCell<HtmlResourceSlot>> {
        let key = (element, attribute);
        if let Some(slot) = self.slots.borrow().get(&key) {
            return slot.clone();
        }
        let slot = Rc::new(RefCell::new(HtmlResourceSlot::new(
            resource, dom, element, attribute, driver_id,
        )));
        self.slots.borrow_mut().insert(key, slot.clone());
        slot
    }

    /// Fills a namer id/name/options (`PopulateResourceNamer`).
    pub fn populate_resource_namer(
        &self,
        filter_id: &str,
        name: &str,
        full_name: &mut ResourceNamer,
    ) {
        full_name.set_id(filter_id);
        full_name.set_name(name);
        full_name.set_experiment(&self.options().experiment_state_str());
        // Option segments need the filter registry (related sets);
        // default-off, so empty until chains own filters.
        full_name.set_options("");
        let _ = filter_id;
    }

    /// Names an output for one input (`GenerateOutputResourceNameAndUrl`),
    /// returning the encoded name plus the mapped URL.
    pub fn generate_output_resource_name_and_url(
        &self,
        encoder: &dyn UrlSegmentEncoder,
        input_url: &str,
    ) -> Result<(Vec<u8>, GoogleUrl), String> {
        let unmapped = GoogleUrl::new(input_url);
        if !self.options().is_allowed(unmapped.spec()) {
            return Err(format!("Rewriting disallowed for {}", unmapped.spec()));
        }
        let mapped = match self
            .lawyer
            .map_request_to_domain(&unmapped, unmapped.spec())
        {
            Some((_, resolved)) => resolved,
            None => {
                return Err(format!("Domain not authorized for {}", unmapped.spec()));
            }
        };
        let leaf = mapped.leaf_with_query().as_bytes().to_vec();
        Ok((encoder.encode(&[leaf]), mapped))
    }

    /// Maps an input URL to its output base (`map_request_to_domain`
    /// with the authorization gate).
    pub fn map_output_base(&self, input_url: &str) -> Result<GoogleUrl, String> {
        let unmapped = GoogleUrl::new(input_url);
        if !self.options().is_allowed(unmapped.spec()) {
            return Err(format!("Rewriting disallowed for {}", unmapped.spec()));
        }
        match self
            .lawyer
            .map_request_to_domain(&unmapped, unmapped.spec())
        {
            Some((_, resolved)) => Ok(resolved),
            None => Err(format!("Domain not authorized for {}", unmapped.spec())),
        }
    }

    /// Creates an output resource from a pre-encoded name.
    pub fn create_output_resource_with_name(
        self: &Rc<Self>,
        filter_id: &str,
        name: &[u8],
        mapped: &GoogleUrl,
        unmapped: &GoogleUrl,
        kind: super::output_resource::OutputResourceKind,
    ) -> Result<super::output_resource::OutputResource, String> {
        self.create_output_resource_with_mapped_path(
            mapped.all_except_leaf(),
            unmapped.all_except_leaf(),
            filter_id,
            name,
            kind,
        )
    }

    /// Creates an output resource from mapped paths
    /// (`CreateOutputResourceWithMappedPath`), enforcing size caps.
    #[allow(clippy::too_many_arguments)]
    pub fn create_output_resource_with_mapped_path(
        self: &Rc<Self>,
        mapped_path: &str,
        unmapped_path: &str,
        filter_id: &str,
        name: &[u8],
        kind: super::output_resource::OutputResourceKind,
    ) -> Result<super::output_resource::OutputResource, String> {
        let mut full_name = ResourceNamer::new();
        let name_text = String::from_utf8_lossy(name).into_owned();
        self.populate_resource_namer(filter_id, &name_text, &mut full_name);
        let signature_len = if self.options().url_signing_key().is_empty() {
            0
        } else {
            ps_cache::signature::Sha1Signature::new().signature_size_in_chars()
        };
        let eventual = full_name.eventual_size(self.server.hasher().as_ref(), signature_len)
            + MAX_EXTENSION_LEN;
        if eventual as i64 > self.options().max_url_segment_size() {
            return Err("Rewritten URL segment too long.".to_string());
        }
        let mut no_hash = false;
        let mut extra_len = 0;
        if full_name.hash().is_empty() {
            no_hash = true;
            let width = self.server.hasher().hash_size_in_chars();
            full_name.set_hash(&"#".repeat(width));
            extra_len = MAX_EXTENSION_LEN;
        }
        // The output's base is the driver's decoded base URL
        // (proxy encoding stripped, leaf removed).
        let decoded_base = self.decoded_base_url().all_except_leaf().to_string();
        let mut resource = OutputResource::new(
            self.clone(),
            mapped_path,
            unmapped_path,
            &decoded_base,
            full_name,
            kind,
        );
        if self.options().max_url_size() < resource.url().len() as i64 + extra_len as i64 {
            return Err(format!("Rewritten URL too long: {}", resource.url()));
        }
        if no_hash {
            resource.clear_hash();
        }
        Ok(resource)
    }

    /// Creates an output resource from an unmapped URL
    /// (`CreateOutputResourceWithUnmappedUrl`).
    pub fn create_output_resource_with_unmapped_url(
        self: &Rc<Self>,
        unmapped: &GoogleUrl,
        filter_id: &str,
        name: &[u8],
        kind: super::output_resource::OutputResourceKind,
    ) -> Result<super::output_resource::OutputResource, String> {
        if !self.options().is_allowed(unmapped.spec()) {
            return Err(format!("Rewriting disallowed for {}", unmapped.spec()));
        }
        let mapped = match self.lawyer.map_request_to_domain(unmapped, unmapped.spec()) {
            Some((_, resolved)) => resolved,
            None => {
                return Err(format!("Domain not authorized for {}", unmapped.spec()));
            }
        };
        self.create_output_resource_with_mapped_path(
            mapped.all_except_leaf(),
            unmapped.all_except_leaf(),
            filter_id,
            name,
            kind,
        )
    }

    /// Creates an output resource for one input
    /// (`CreateOutputResourceFromResource`).
    pub fn create_output_resource_from_resource(
        self: &Rc<Self>,
        filter_id: &str,
        encoder: &dyn UrlSegmentEncoder,
        input: &dyn Resource,
        kind: super::output_resource::OutputResourceKind,
    ) -> Result<super::output_resource::OutputResource, String> {
        let (name, mapped) = self.generate_output_resource_name_and_url(encoder, input.url())?;
        let unmapped = GoogleUrl::new(input.url());
        assert!(input.is_authorized_domain());
        self.create_output_resource_with_mapped_path(
            mapped.all_except_leaf(),
            unmapped.all_except_leaf(),
            filter_id,
            &name,
            kind,
        )
    }

    /// Borrows the options signature for this request.
    ///
    /// The signature is memoized inside [`RewriteOptions`] itself and
    /// recomputed only after a mutation, so all of a request's
    /// partition keys, beacon identities, and CSS payload hashes
    /// share one computation — including across live-driver options
    /// mutations, which invalidate the cache.
    #[must_use]
    pub fn options_signature(&self) -> String {
        self.options().signature()
    }

    /// Hashes the options signature for partition keys
    /// (`partition_signature_hash`, hashed with the lock hasher so
    /// test naming hashers cannot flatten it).
    #[must_use]
    pub fn partition_signature_hash(&self) -> String {
        self.server
            .lock_hasher()
            .hash(self.options_signature().as_bytes())
    }

    /// Writes rewritten bytes to an output (`Write`), caching the
    /// result and linking the partition. The partition arrives
    /// explicitly (index threading stands in for C++'s pointer link).
    #[allow(clippy::too_many_arguments)]
    pub fn write_output(
        &self,
        inputs: &[super::resource::InputResource],
        contents: &[u8],
        content_type: Option<ps_http::content_type::ContentType>,
        charset: &str,
        output: &mut super::output_resource::OutputResource,
        partition: &mut super::metadata::CachedResult,
    ) -> bool {
        use ps_http::response::{RequestProperties, ValidatorOption, VaryOption};
        output.set_type(content_type);
        output.set_charset(charset);
        let on_the_fly = output.kind() == super::output_resource::OutputResourceKind::OnTheFly;
        let suffix = output.cache_control_suffix().to_string();
        let meta_data = output.response_headers_mut();
        // Transfer Last-Modified for single-input on-the-fly outputs.
        if inputs.len() == 1 && on_the_fly {
            let mut clear_last_modified = false;
            if let Some(last_modified) = inputs[0]
                .response_headers()
                .headers()
                .lookup1(ps_http::names::header::LAST_MODIFIED)
            {
                meta_data
                    .headers_mut()
                    .add(ps_http::names::header::LAST_MODIFIED, &last_modified);
            } else {
                clear_last_modified = true;
            }
            self.server
                .set_default_long_cache_headers(content_type, charset, &suffix, meta_data);
            if clear_last_modified {
                meta_data
                    .headers_mut()
                    .remove_all(ps_http::names::header::LAST_MODIFIED);
            }
        } else {
            self.server
                .set_default_long_cache_headers(content_type, charset, &suffix, meta_data);
        }
        self.server.apply_input_cache_control(inputs, meta_data);
        self.server
            .add_original_content_length_header(inputs, meta_data);

        output.begin_write().extend_from_slice(contents);
        output.end_write();

        let cacheable = {
            let headers = output.response_headers();
            let mut computed = headers.clone();
            computed.compute_caching();
            computed.is_proxy_cacheable(
                RequestProperties::default(),
                VaryOption::IgnoreVaryOnResources,
                ValidatorOption::HasValidator,
            )
        };
        if output.kind() != super::output_resource::OutputResourceKind::OnTheFly
            && output.kind() != super::output_resource::OutputResourceKind::Inline
            && (self.server.http_cache().force_caching() || cacheable)
        {
            let mut value = ps_http::value::HttpValue::new();
            value.set_headers(output.response_headers().clone());
            value.write_body(output.body());
            let options = self.options();
            self.server.http_cache().put_value(
                &output.http_cache_key(),
                self.cache_fragment(),
                RequestProperties::default(),
                ps_http::response::HttpOptions {
                    respect_vary: options.respect_vary(),
                    implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
                },
                &value,
                self.server.now_ms(),
            );
        }
        // NOTE: filesystem output dumps need the file writer.

        if output.kind() == super::output_resource::OutputResourceKind::Rewritten
            || output.kind() == super::output_resource::OutputResourceKind::OnTheFly
        {
            partition.optimizable = true;
            partition.url = output.url().to_string();
        }
        true
    }

    /// Registers a serving filter (`AddFilter` resource map).
    pub fn add_filter(&self, filter: Rc<dyn RewriteFilter>) {
        self.filters.borrow_mut().insert(filter.id(), filter);
    }

    /// Registers every serving filter (`AddFilters` shape):
    /// `.pagespeed.` fetches decode to a filter id and 404 without
    /// its servant, so serving drivers register the full set.
    /// Tests keep bare drivers unless they register explicitly.
    pub fn register_serving_filters(self: &Rc<Self>) {
        use super::filters::cache_extender::CacheExtenderFilter;
        use super::filters::css::CssFilter;
        use super::filters::css_combine::CssCombineFilter;
        use super::filters::image_combine::ImageCombineFilter;
        use super::filters::image_rewrite::ImageRewriteFilter;
        use super::filters::javascript::JavascriptFilter;
        use super::filters::js_combine::JsCombineFilter;
        self.add_filter(Rc::new(CacheExtenderFilter::new(self.clone())));
        self.add_filter(Rc::new(CssFilter::new(self.clone())));
        self.add_filter(Rc::new(CssCombineFilter::new(self.clone())));
        self.add_filter(Rc::new(ImageCombineFilter::new(self.clone())));
        self.add_filter(Rc::new(ImageRewriteFilter::new(self.clone())));
        self.add_filter(Rc::new(JavascriptFilter::new(self.clone())));
        self.add_filter(Rc::new(JsCombineFilter::new(self.clone())));
    }

    /// Looks up a serving filter (`FindFilter`).
    #[must_use]
    pub fn find_filter(&self, id: &str) -> Option<Rc<dyn RewriteFilter>> {
        self.filters.borrow().get(id).cloned()
    }

    /// Marks an element claimed by its creating filter: C++
    /// renders combinations at harvest, after inline decisions,
    /// so its inline pass never sees them; our sync pre-pass
    /// replays the combined element to inline filters, which
    /// stand down on claimed elements instead.
    pub fn claim_element(&self, id: NodeId) {
        self.claimed_elements.borrow_mut().insert(id);
    }

    /// Whether a combiner claimed an element this parse.
    #[must_use]
    pub fn is_element_claimed(&self, id: NodeId) -> bool {
        self.claimed_elements.borrow().contains(&id)
    }

    /// Whether debug comments emit (`DebugMode`).
    #[must_use]
    pub fn debug_mode(&self) -> bool {
        self.options().enabled(ps_config::filter::Filter::Debug)
    }

    /// Borrows the containing document charset (`containing_charset`;
    /// the scan filter derives it from headers/meta/BOM — tests set
    /// it directly until that lands).
    #[must_use]
    pub fn containing_charset(&self) -> String {
        self.containing_charset.borrow().clone()
    }

    /// Sets the containing document charset.
    pub fn set_containing_charset(&self, charset: &str) {
        *self.containing_charset.borrow_mut() = charset.to_string();
    }

    /// Whether the document is AMP (`is_amp_document`).
    #[must_use]
    pub fn is_amp_document(&self) -> bool {
        *self.is_amp.borrow()
    }

    /// Sets the AMP verdict.
    pub fn set_is_amp_document(&self, is_amp: bool) {
        *self.is_amp.borrow_mut() = is_amp;
    }

    /// Whether the document is XHTML (`MimeTypeXhtmlStatus`).
    #[must_use]
    pub fn is_xhtml(&self) -> bool {
        *self.xhtml_status.borrow() == XhtmlStatus::IsXhtml
    }

    /// Borrows the mime XHTML verdict (`MimeTypeXhtmlStatus`).
    #[must_use]
    pub fn mime_type_xhtml_status(&self) -> XhtmlStatus {
        *self.xhtml_status.borrow()
    }

    /// Sets the XHTML verdict.
    pub fn set_is_xhtml(&self, is_xhtml: bool) {
        *self.xhtml_status.borrow_mut() = if is_xhtml {
            XhtmlStatus::IsXhtml
        } else {
            XhtmlStatus::IsNotXhtml
        };
    }

    /// Reserves debug bytes (`ReserveDebugOutputBytes`; non-positive
    /// caps are unbounded).
    fn reserve_debug_output_bytes(&self, num_bytes: i64) -> bool {
        let limit = self.options().debug_output_max_bytes();
        if limit <= 0 {
            return true;
        }
        let used = *self.debug_output_bytes.borrow();
        if *self.debug_output_truncated.borrow() || num_bytes > limit - used {
            *self.debug_output_truncated.borrow_mut() = true;
            return false;
        }
        *self.debug_output_bytes.borrow_mut() = used + num_bytes;
        true
    }

    /// Inserts one budgeted comment after a node
    /// (`InsertBoundedDebugComment`), returning the comment node.
    fn insert_bounded_debug_comment(
        &self,
        dom: &mut Dom,
        escaped: &str,
        node: NodeId,
    ) -> Option<NodeId> {
        if !self.reserve_debug_output_bytes(escaped.len() as i64 + COMMENT_DELIMITER_BYTES) {
            if !*self.debug_truncation_noted.borrow() {
                *self.debug_truncation_noted.borrow_mut() = true;
                let message = format!(
                    "Debug output truncated at DebugOutputMaxBytes={}",
                    self.options().debug_output_max_bytes()
                );
                let parent = dom.arena_node(node).parent();
                let id = dom.new_leaf(
                    parent,
                    super::element::LeafKind::Comment,
                    message.into_bytes(),
                );
                dom.insert_node_after_node(node, id);
            }
            return None;
        }
        let parent = dom.arena_node(node).parent();
        let id = dom.new_leaf(
            parent,
            super::element::LeafKind::Comment,
            escaped.as_bytes().to_vec(),
        );
        dom.insert_node_after_node(node, id);
        Some(id)
    }

    /// Inserts an escaped debug comment after a node
    /// (`InsertDebugComment`).
    pub fn insert_debug_comment(&self, dom: &mut Dom, unescaped: &str, node: NodeId) {
        if !self.debug_mode() || !dom.is_rewritable(node) {
            return;
        }
        let mut escaped = Vec::new();
        super::keywords::escape(unescaped.as_bytes(), &mut escaped);
        let escaped = String::from_utf8_lossy(&escaped).into_owned();
        self.insert_bounded_debug_comment(dom, &escaped, node);
    }

    /// Inserts debug comments after an element (`InsertDebugComments`,
    /// at most 32 plus an omission note).
    pub fn insert_debug_comments(&self, dom: &mut Dom, messages: &[String], element: NodeId) {
        if !self.debug_mode() || !dom.is_rewritable(element) {
            return;
        }
        let mut preceding = element;
        let inserted = messages.len().min(MAX_DEBUG_COMMENTS_PER_LIST);
        for message in messages.iter().take(inserted) {
            let mut escaped = Vec::new();
            super::keywords::escape(message.as_bytes(), &mut escaped);
            let escaped = String::from_utf8_lossy(&escaped).into_owned();
            let Some(comment) = self.insert_bounded_debug_comment(dom, &escaped, preceding) else {
                return;
            };
            preceding = comment;
        }
        if messages.len() > inserted {
            let note = format!("{} more debug messages omitted", messages.len() - inserted);
            self.insert_bounded_debug_comment(dom, &note, preceding);
        }
    }

    /// Explains an unauthorized URL (`GenerateUnauthorizedDomainDebugComment`).
    #[must_use]
    pub fn generate_unauthorized_domain_debug_comment(
        &self,
        gurl: &GoogleUrl,
        role: InputRole,
    ) -> String {
        let mut comment = "The preceding resource was not rewritten because ".to_string();
        if gurl.is_web_valid() {
            comment.push_str(&format!("its domain ({}) is not authorized", gurl.host()));
        } else if gurl.is_web_or_data_valid() {
            comment.push_str("it is a data URI");
        } else if !self.is_load_permitted_by_csp(gurl, role) {
            comment.push_str("CSP disallows its fetch");
        } else {
            comment.push_str("it is not authorized");
        }
        let mut escaped = Vec::new();
        super::keywords::escape(comment.as_bytes(), &mut escaped);
        String::from_utf8_lossy(&escaped).into_owned()
    }

    /// Inserts the unauthorized-domain comment
    /// (`InsertUnauthorizedDomainDebugComment`).
    pub fn insert_unauthorized_domain_debug_comment(
        &self,
        dom: &mut Dom,
        url: &str,
        role: InputRole,
        element: NodeId,
    ) {
        if !self.debug_mode() || !dom.is_rewritable(element) {
            return;
        }
        let comment = self.generate_unauthorized_domain_debug_comment(&GoogleUrl::new(url), role);
        self.insert_bounded_debug_comment(dom, &comment, element);
    }

    /// URL signature length (`SignatureLength`).
    #[must_use]
    pub fn signature_length(&self) -> usize {
        if self.options().url_signing_key().is_empty() {
            0
        } else {
            ps_cache::signature::Sha1Signature::new().signature_size_in_chars()
        }
    }

    /// Decodes a pagespeed leaf (`Decode`).
    #[must_use]
    pub fn decode_leaf(&self, leaf: &str) -> Option<ResourceNamer> {
        let mut namer = ResourceNamer::new();
        let hash_len = self.server.hasher().hash_size_in_chars();
        let signatures = self.signature_length();
        #[allow(clippy::cast_possible_wrap)]
        let ok = namer.decode(leaf, hash_len as i32, signatures as i32);
        ok.then_some(namer)
    }

    /// Whether CSS extends (`MayCacheExtendCss`).
    #[must_use]
    pub fn may_cache_extend_css(&self) -> bool {
        self.options()
            .enabled(ps_config::filter::Filter::ExtendCacheCss)
    }

    /// Whether images extend (`MayCacheExtendImages`).
    #[must_use]
    pub fn may_cache_extend_images(&self) -> bool {
        self.options()
            .enabled(ps_config::filter::Filter::ExtendCacheImages)
    }

    /// Whether PDFs extend (`MayCacheExtendPdfs`, default off).
    #[must_use]
    pub fn may_cache_extend_pdfs(&self) -> bool {
        self.options()
            .enabled(ps_config::filter::Filter::ExtendCachePdfs)
    }

    /// Whether scripts extend (`MayCacheExtendScripts`).
    #[must_use]
    pub fn may_cache_extend_scripts(&self) -> bool {
        self.options()
            .enabled(ps_config::filter::Filter::ExtendCacheScripts)
    }

    /// Resolves CSS URLs across bases (`ResolveCssUrls`),
    /// returning the outcome plus the (possibly rewritten) bytes.
    #[must_use]
    pub fn resolve_css_urls(
        &self,
        input_base: &GoogleUrl,
        output_base: &str,
        contents: &[u8],
    ) -> (CssResolutionStatus, Vec<u8>) {
        let output_base = GoogleUrl::new(output_base);
        let (absolutify, proxy_mode) = self.should_absolutify_url(input_base, &output_base);
        if !absolutify {
            return (CssResolutionStatus::NoResolutionNeeded, contents.to_vec());
        }
        let options = self.options();
        let namer = self.server.url_namer().clone();
        let mut transformer = RewriteDomainTransformer {
            old_base: input_base,
            new_base: &output_base,
            lawyer: &self.lawyer,
            options: &options,
            namer: &*namer,
            hash_length: self.server.hasher().hash_size_in_chars(),
            signature_length: self.signature_length(),
            trim_urls: !proxy_mode,
        };
        match ps_css::scanner::transform_urls(contents, &mut transformer) {
            Some(rewritten) => (CssResolutionStatus::Success, rewritten),
            None => (CssResolutionStatus::WriteFailed, contents.to_vec()),
        }
    }

    /// Whether CSS URLs need rebasing (`ShouldAbsolutifyUrl`),
    /// plus the proxy-mode trim verdict.
    pub fn should_absolutify_url(
        &self,
        input_base: &GoogleUrl,
        output_base: &GoogleUrl,
    ) -> (bool, bool) {
        if self.server.url_namer().proxy_mode() == ProxyExtent::Full {
            return (true, true);
        }
        if input_base.all_except_leaf() != output_base.all_except_leaf() {
            return (true, false);
        }
        (self.lawyer.will_domain_change(input_base), false)
    }

    /// Whether an input shares the decoded base origin
    /// (`MatchesBaseUrl`).
    #[must_use]
    pub fn matches_base_url(&self, input_url: &GoogleUrl) -> bool {
        let decoded_base = self.decoded_base_url();
        decoded_base.is_web_valid()
            && self.options().is_allowed(input_url.spec())
            && decoded_base.origin() == input_url.origin()
    }

    /// Decodes a served URL (`DecodeOutputResource`), returning the
    /// output plus its filter (none for outlined resources).
    #[must_use]
    pub fn decode_output_resource(
        self: &Rc<Self>,
        gurl: &GoogleUrl,
    ) -> Option<(OutputResource, Option<Rc<dyn RewriteFilter>>)> {
        if self.options().oblivious_pagespeed_urls() || !gurl.is_web_valid() {
            return None;
        }
        let leaf = gurl.leaf_sans_query();
        let namer = self.decode_leaf(&leaf)?;
        // Hashless names cannot produce computable URLs.
        if namer.hash().is_empty() {
            return None;
        }
        // NOTE: proxy-encoded bases land with the proxy namer.
        let base = gurl.all_except_leaf().to_string();
        let id = namer.id().to_string();
        let (kind, filter) = match self.find_filter(&id) {
            Some(filter) => {
                let kind = if filter.compute_on_the_fly() {
                    OutputResourceKind::OnTheFly
                } else {
                    OutputResourceKind::Rewritten
                };
                (kind, Some(filter))
            }
            // Outline filters are plain HTML filters, but their
            // outputs still serve from cache.
            None if id == "co" || id == "jo" => (OutputResourceKind::Outlined, None),
            None => return None,
        };
        if let Some(filter) = &filter {
            if filter.encoder().decode(namer.name().as_bytes()).is_none() {
                return None;
            }
        }
        if self.options().forbidden_by_id(&id) {
            return None;
        }
        let output = OutputResource::new(self.clone(), &base, &base, &base, namer, kind);
        if !output.check_signature() {
            return None;
        }
        Some((output, filter))
    }

    /// Serves a resource URL (`FetchResource`), driving the fetch to
    /// completion inline. True when the URL decoded (in-place serving
    /// waits for the IPRO context).
    pub fn fetch_resource(
        self: &Rc<Self>,
        url: &str,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        use ps_http::http_cache::FindStatus;
        use ps_http::names::header;
        use ps_http::response::{RequestProperties, VaryOption};
        let gurl = GoogleUrl::new(url);
        let Some((output, filter)) = self.decode_output_resource(&gurl) else {
            if self.options().in_place_rewriting_enabled() {
                // This is an ajax resource (`FetchInPlaceResource`
                // with `proxy_mode`, verbatim).
                return super::ipro::fetch_in_place_resource(self, url, fetch, true);
            }
            return false;
        };
        // Reload validations always hit: hashed URLs never change.
        if fetch
            .request_headers()
            .headers()
            .has(header::IF_MODIFIED_SINCE)
        {
            fetch.response_headers_mut().set_status_and_reason(304);
            fetch.headers_complete();
            fetch.handle_done(true);
            return true;
        }
        if output.kind() == super::output_resource::OutputResourceKind::OnTheFly {
            // On-the-fly outputs skip the cache: ask the filter.
            match filter {
                Some(filter) => {
                    let variable = if filter.fetch(self, output, fetch) {
                        "succeeded_filter_resource_fetches"
                    } else {
                        "failed_filter_resource_fetches"
                    };
                    self.server.variable(variable).add(1);
                    return true;
                }
                None => {
                    fetch.response_headers_mut().set_status_and_reason(404);
                    fetch.handle_done(false);
                    return true;
                }
            }
        }
        let key = output.http_cache_key();
        let mut ctx = self.request_context.borrow().clone();
        // If the user cares, don't try to send a rewritten .pagespeed. webp
        // resource to a browser that can't handle it (`CacheCallback::IsCacheValid`).
        let serve_webp_to_any = self.options().serve_rewritten_webp_urls_to_any_agent();
        let accepts_webp =
            super::device_properties::accepts_image_format(fetch.request_headers(), "image/webp");
        let found = self.server.http_cache().find(
            &key,
            self.cache_fragment(),
            RequestProperties::default(),
            VaryOption::IgnoreVaryOnResources,
            &|headers| {
                if !serve_webp_to_any
                    && !accepts_webp
                    && headers
                        .determine_content_type()
                        .is_some_and(|content| content.mime_type() == "image/webp")
                {
                    return false;
                }
                true
            },
            -1,
            &mut ctx,
            self.server.now_ms(),
        );
        if found.status == FindStatus::Found {
            self.server.variable("cached_resource_fetches").add(1);
            if let Some(headers) = found.headers {
                fetch.response_headers_mut().copy_from(&headers);
            }
            fetch.headers_complete();
            let ok = fetch.handle_write(&found.body);
            fetch.handle_done(ok);
            return true;
        }
        if output.writing_complete() {
            // A written-but-uncached output still serves (re-cached
            // for next time).
            let mut value = ps_http::value::HttpValue::new();
            value.set_headers(output.response_headers().clone());
            value.write_body(output.body());
            let options = self.options();
            self.server.http_cache().put_value(
                &key,
                self.cache_fragment(),
                RequestProperties::default(),
                ps_http::response::HttpOptions {
                    respect_vary: options.respect_vary(),
                    implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
                },
                &value,
                self.server.now_ms(),
            );
            fetch
                .response_headers_mut()
                .copy_from(output.response_headers());
            fetch.headers_complete();
            let ok = fetch.handle_write(output.body());
            fetch.handle_done(ok);
            return true;
        }
        match filter {
            Some(filter) => {
                let variable = if filter.fetch(self, output, fetch) {
                    "succeeded_filter_resource_fetches"
                } else {
                    "failed_filter_resource_fetches"
                };
                self.server.variable(variable).add(1);
                true
            }
            None => {
                fetch.response_headers_mut().set_status_and_reason(404);
                fetch.handle_done(false);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_http::mock::MockUrlFetcher;

    fn driver_with(options: RewriteOptions) -> Rc<ResourceDriver> {
        let server = Rc::new(ServerContext::new(
            Arc::new(ps_cache::cache::MemoryCache::new()),
            Arc::new(MockUrlFetcher::new()),
            0,
        ));
        Rc::new(ResourceDriver::new(
            server,
            Rc::new(RefCell::new(options)),
            "http://test.com/page.html",
        ))
    }

    #[test]
    fn options_signature_memo_matches_direct() {
        let driver = driver_with(RewriteOptions::new());
        let direct = driver.options().signature();
        // First call computes, later calls reuse.
        assert_eq!(driver.options_signature(), direct);
        assert_eq!(driver.options_signature(), direct);
        assert_eq!(
            driver.partition_signature_hash(),
            driver.partition_signature_hash()
        );
    }

    #[test]
    fn options_signature_separates_cache_keys() {
        let plain = driver_with(RewriteOptions::new());
        let mut altered = RewriteOptions::new();
        altered
            .filters_mut()
            .enable(ps_config::filter::Filter::InlineCss);
        let altered = driver_with(altered);
        // Different rewrite options must key partitions apart.
        assert_ne!(
            plain.partition_signature_hash(),
            altered.partition_signature_hash()
        );
        assert_ne!(plain.options_signature(), altered.options_signature());
    }
}
