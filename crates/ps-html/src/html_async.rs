//! Fire-and-forget HTML rewrite scheduling (C++ deadline-detach parity).
//!
//! On a metadata-cache MISS during HTML rewriting, C++ PageSpeed does not
//! stall page delivery on input fetching and optimization: it detaches the
//! rewrite, serves fallback (original) URLs, and completes the optimization
//! in the background, so the *next* HTML request hits warm metadata. This
//! module ports that shape onto the [`BackgroundPool`](super::background):
//!
//! - [`schedule_single_rewrite`] / [`schedule_inline_rewrite`] snapshot the
//!   Send-owned inputs of a missed rewrite (options, URLs, behavior params)
//!   and submit a deduped job keyed by partition key. The HTML request then
//!   serves original URLs immediately and marks the render detached, so the
//!   HTML cache never freezes unoptimized bytes.
//! - The pool worker rebuilds an equivalent driver from the snapshot (same
//!   options, shared caches/fetcher/statistics/hasher) and runs the exact
//!   sync miss core — fetch, partition, rewrite, partition write — without
//!   rendering. Completions land in the shared caches for later requests.
//!
//! Guarantees, inherited from the pool: concurrency safety (jobs own
//! everything they touch; shared state is `Send + Sync` caches), work
//! deduplication (concurrent same-key misses join one execution), and
//! bounded queues (saturation sheds to fallback content, retried by the
//! next request). Behaviors without a snapshot (combiners) and the
//! fetch/IPRO serving paths keep the sync behavior, so no optimization
//! is silently disabled — only deferred off the HTML critical path.
//!
//! Two deliberate production assumptions (documented, not silent): workers
//! use the standard URL namer (production always does; proxy-encoding tests
//! never install a pool) and a frozen clock snapshot (jobs are short).

use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::Cache as CacheBackend;
use ps_cache::hash::Hasher;
use ps_config::options::RewriteOptions;
use ps_http::fetch::AsyncFetcher;
use ps_http::google_url::GoogleUrl;
use ps_http::http_cache::HttpCache;
use ps_http::response::ResponseHeaders;
use ps_stats::core::Statistics;

use super::background::{BackgroundPool, SubmitError};
use super::image_url_encoder::ImageResourceContext;
use super::resource::{InputResource, Resource};
use super::server_context::{InlineAuthorizationPolicy, InputRole, ResourceDriver, ServerContext};

/// Scheduled-but-incomplete HTML rewrites (served original URLs; the
/// background completion warms the next request).
pub const HTML_ASYNC_SCHEDULED: &str = "html_async_rewrite_scheduled";
/// Background HTML rewrites run to completion on pool workers.
pub const HTML_ASYNC_COMPLETED: &str = "html_async_rewrite_completed";

/// Send-owned behavior parameters for a background rewrite.
///
/// Each variant mirrors one behavior's per-context state (the fields its
/// constructor takes beyond the driver); the worker rebuilds an equivalent
/// behavior from these plus a fresh driver. Behaviors with no variant keep
/// the sync path.
#[derive(Debug, Clone)]
pub enum BackgroundBehavior {
    /// CSS rewrite (`cf`): element/page charset agreement.
    Css {
        /// Root applicable media (link/style element).
        root_media: Vec<Vec<u8>>,
        /// Root charset (page/element agreement).
        root_charset: Vec<u8>,
        /// Root charset failure reason, if any.
        root_charset_failure: Option<String>,
    },
    /// JavaScript rewrite (`jm`).
    Javascript {
        /// Source-map twin mode.
        output_source_map: bool,
    },
    /// Image rewrite (`ic`).
    Image {
        /// Image request context (dimensions, format support).
        resource_context: ImageResourceContext,
        /// Rewrite place (attribute, CSS, srcset, ...).
        place: super::filters::image_rewrite::RewritePlace,
        /// Image index (debug identity).
        image_index: i32,
        /// Inside a noscript element.
        in_noscript_element: bool,
        /// CSS image inlining budget.
        css_image_inline_max_bytes: i64,
        /// Resized from rendered dimensions.
        is_resized_using_rendered_dimensions: bool,
        /// Containing page URL.
        base_url: String,
    },
    /// Cache extension (`ce`): tag-category role.
    CacheExtender {
        /// Input role.
        role: InputRole,
    },
    /// CSS inline (`ci`).
    InlineCss {
        /// Element charset attributes.
        attrs_charset: Vec<u8>,
        /// Page charset.
        htmls_charset: Vec<u8>,
        /// Inline size budget.
        size_threshold_bytes: i64,
    },
    /// JavaScript inline (`ji`).
    InlineJs {
        /// Element charset attributes.
        attrs_charset: Vec<u8>,
        /// Page charset.
        htmls_charset: Vec<u8>,
        /// Inline size budget.
        size_threshold_bytes: i64,
    },
    /// Google Font CSS inline (`gf`).
    InlineGoogleFontCss {
        /// Element charset attributes.
        attrs_charset: Vec<u8>,
        /// Page charset.
        htmls_charset: Vec<u8>,
        /// Inline size budget.
        size_threshold_bytes: i64,
    },
}

/// Send-owned inputs for one background rewrite job.
#[derive(Debug, Clone)]
pub struct HtmlRewriteSnapshot {
    /// Behavior parameters.
    pub behavior: BackgroundBehavior,
    /// Input URL.
    pub input_url: String,
    /// Whether the input's domain is authorized (verdict from the HTML
    /// thread; the worker recreates the resource unchecked).
    pub input_authorized: bool,
    /// Partition key the HTML lookup missed (the worker writes here).
    pub partition_key: String,
    /// Containing page URL.
    pub base_url: String,
    /// Effective rewrite options for the request.
    pub options: RewriteOptions,
    /// Request user agent.
    pub user_agent: String,
    /// Page response charset, when the containing headers name one
    /// (CSS charset agreement must match the sync path).
    pub response_charset: Option<String>,
    /// Clock snapshot for cache decisions.
    pub now_ms: i64,
}

/// Shared handles a background job needs (all `Send + Sync`).
#[derive(Clone)]
pub struct HtmlRewriteShared {
    /// Shared HTTP cache (exact instance: config must match).
    pub http_cache: Arc<HttpCache>,
    /// Shared metadata cache (partition tables land here).
    pub metadata: Arc<dyn CacheBackend>,
    /// Origin fetcher.
    pub fetcher: Arc<dyn AsyncFetcher>,
    /// Shared statistics registry.
    pub statistics: Arc<Statistics>,
    /// Naming hasher.
    pub hasher: Arc<dyn Hasher>,
}

/// Scheduling verdict for the HTML-thread caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// Scheduled (or joined an in-flight duplicate): serve original
    /// URLs now; the completion warms the next request.
    Scheduled,
    /// Queue saturated: serve original URLs now (shed load); a later
    /// request retries once workers drain.
    Shed,
    /// Pool gone (shutdown): run the sync path so the request still
    /// completes, degraded.
    SyncFallback,
}

/// Snapshots scheduling inputs from a live HTML driver.
pub fn snapshot_driver(
    driver: &ResourceDriver,
) -> (String, RewriteOptions, String, Option<String>, i64) {
    let options = driver.options().clone();
    let charset = driver
        .response_headers()
        .determine_charset()
        .filter(|charset| !charset.is_empty());
    (
        driver.base_url().spec().to_string(),
        options,
        driver.user_agent(),
        charset,
        driver.server_context().now_ms(),
    )
}

/// Snapshots shared handles from a live HTML driver.
pub fn snapshot_shared(driver: &ResourceDriver) -> HtmlRewriteShared {
    let server = driver.server_context();
    HtmlRewriteShared {
        http_cache: server.http_cache_shared(),
        metadata: server.metadata_cache().clone(),
        fetcher: server.fetcher().clone(),
        statistics: server.statistics_shared(),
        hasher: server.hasher().clone(),
    }
}

/// Submits a background single-input rewrite, fire-and-forget.
///
/// Dedup key: the partition key (concurrent same-resource misses join
/// one execution). Returns [`ScheduleOutcome::SyncFallback`] only when
/// no pool is installed or it shut down.
pub fn schedule_single_rewrite(
    driver: &ResourceDriver,
    behavior: BackgroundBehavior,
    input: &InputResource,
    partition_key: &str,
) -> ScheduleOutcome {
    schedule_rewrite(driver, behavior, input, partition_key)
}

/// Submits a background inline rewrite, fire-and-forget (same
/// dedup/completion contract as [`schedule_single_rewrite`]).
pub fn schedule_inline_rewrite(
    driver: &ResourceDriver,
    behavior: BackgroundBehavior,
    input: &InputResource,
    partition_key: &str,
) -> ScheduleOutcome {
    schedule_rewrite(driver, behavior, input, partition_key)
}

fn schedule_rewrite(
    driver: &ResourceDriver,
    behavior: BackgroundBehavior,
    input: &InputResource,
    partition_key: &str,
) -> ScheduleOutcome {
    let Some(pool) = driver.server_context().background_pool() else {
        return ScheduleOutcome::SyncFallback;
    };
    schedule_on_pool(driver, &pool, behavior, input, partition_key)
}

/// Submits onto an explicit pool (the caller already resolved it).
fn schedule_on_pool(
    driver: &ResourceDriver,
    pool: &Arc<BackgroundPool>,
    behavior: BackgroundBehavior,
    input: &InputResource,
    partition_key: &str,
) -> ScheduleOutcome {
    let (base_url, options, user_agent, response_charset, now_ms) = snapshot_driver(driver);
    let shared = snapshot_shared(driver);
    let snapshot = HtmlRewriteSnapshot {
        behavior,
        input_url: input.url().to_string(),
        input_authorized: input.is_authorized_domain(),
        partition_key: partition_key.to_string(),
        base_url,
        options,
        user_agent,
        response_charset,
        now_ms,
    };
    let key = format!("html-rewrite:{}", snapshot.partition_key);
    match pool.submit(&key, move || run_rewrite_job(snapshot, shared)) {
        Ok(_) => {
            driver
                .server_context()
                .variable(HTML_ASYNC_SCHEDULED)
                .add(1);
            ScheduleOutcome::Scheduled
        }
        Err(SubmitError::QueueFull) => ScheduleOutcome::Shed,
        Err(SubmitError::Shutdown) => ScheduleOutcome::SyncFallback,
    }
}

/// Runs one background rewrite on a pool thread: rebuilds an equivalent
/// driver from the snapshot and runs the sync miss core to partitions
/// (no rendering). Returns whether partitions were written.
fn run_rewrite_job(snapshot: HtmlRewriteSnapshot, shared: HtmlRewriteShared) -> bool {
    let HtmlRewriteShared {
        http_cache,
        metadata,
        fetcher,
        statistics,
        hasher,
    } = shared;
    let server = Rc::new(ServerContext::new_shared(
        http_cache,
        metadata,
        fetcher,
        statistics.clone(),
        hasher,
        snapshot.now_ms,
    ));
    let driver = Rc::new(ResourceDriver::new(
        server,
        Rc::new(std::cell::RefCell::new(snapshot.options)),
        &snapshot.base_url,
    ));
    driver.set_user_agent(&snapshot.user_agent);
    if let Some(charset) = snapshot.response_charset.as_deref() {
        // CSS charset agreement reads the containing headers; mirror
        // the page charset so the worker agrees with the sync path.
        let mut headers = ResponseHeaders::default();
        headers.headers_mut().add(
            ps_http::names::header::CONTENT_TYPE,
            &format!("text/html; charset={charset}"),
        );
        driver.set_response_headers(headers);
    }
    let input_url = GoogleUrl::new(&snapshot.input_url);
    if !input_url.is_web_valid() {
        return false;
    }
    let Some(input) = driver.create_input_resource_unchecked(&input_url, snapshot.input_authorized)
    else {
        return false;
    };
    let wrote = run_behavior_rewrite(&driver, snapshot.behavior, input, &snapshot.partition_key);
    if wrote {
        statistics.add_variable(HTML_ASYNC_COMPLETED).add(1);
    }
    wrote
}

/// Dispatches one snapshot to its concrete behavior/context pair.
fn run_behavior_rewrite(
    driver: &Rc<ResourceDriver>,
    behavior: BackgroundBehavior,
    input: InputResource,
    partition_key: &str,
) -> bool {
    use super::rewrite_context::{InlineRewriteContext, SingleRewriteContext};
    match behavior {
        BackgroundBehavior::Css {
            root_media,
            root_charset,
            root_charset_failure,
        } => {
            let behavior = super::filters::css::CssBehavior::for_background(
                driver.clone(),
                root_media,
                root_charset,
                root_charset_failure,
            );
            let mut context = SingleRewriteContext::new(driver.clone(), behavior);
            context.add_fetch_slot(input);
            context.run_background_to_partitions(partition_key)
        }
        BackgroundBehavior::Javascript { output_source_map } => {
            let behavior = super::filters::javascript::JavascriptBehavior::for_background(
                driver.clone(),
                output_source_map,
            );
            let mut context = SingleRewriteContext::new(driver.clone(), behavior);
            context.add_fetch_slot(input);
            context.run_background_to_partitions(partition_key)
        }
        BackgroundBehavior::Image {
            resource_context,
            place,
            image_index,
            in_noscript_element,
            css_image_inline_max_bytes,
            is_resized_using_rendered_dimensions,
            base_url,
        } => {
            let behavior = super::filters::image_rewrite::ImageRewriteBehavior::new(
                driver.clone(),
                resource_context,
                place,
                image_index,
                in_noscript_element,
                css_image_inline_max_bytes,
                is_resized_using_rendered_dimensions,
                GoogleUrl::new(&base_url),
            );
            let mut context = SingleRewriteContext::new(driver.clone(), behavior);
            context.add_fetch_slot(input);
            context.run_background_to_partitions(partition_key)
        }
        BackgroundBehavior::CacheExtender { role } => {
            let behavior = super::filters::cache_extender::CacheExtenderBehavior::for_background(
                driver.clone(),
                role,
            );
            let mut context = SingleRewriteContext::new(driver.clone(), behavior);
            context.add_fetch_slot(input);
            context.run_background_to_partitions(partition_key)
        }
        BackgroundBehavior::InlineCss {
            attrs_charset,
            htmls_charset,
            size_threshold_bytes,
        } => {
            let behavior = super::filters::css_inline::CssInlineBehavior::for_background(
                driver.clone(),
                attrs_charset,
                htmls_charset,
                size_threshold_bytes,
            );
            let mut context = InlineRewriteContext::new(
                driver.clone(),
                behavior,
                0,
                driver.base_url().clone(),
            );
            context.run_background_inline(input, partition_key)
        }
        BackgroundBehavior::InlineJs {
            attrs_charset,
            htmls_charset,
            size_threshold_bytes,
        } => {
            let behavior = super::filters::js_inline::JsInlineBehavior::for_background(
                driver.clone(),
                attrs_charset,
                htmls_charset,
                size_threshold_bytes,
            );
            let mut context = InlineRewriteContext::new(
                driver.clone(),
                behavior,
                0,
                driver.base_url().clone(),
            );
            context.run_background_inline(input, partition_key)
        }
        BackgroundBehavior::InlineGoogleFontCss {
            attrs_charset,
            htmls_charset,
            size_threshold_bytes,
        } => {
            let behavior =
                super::filters::google_font_css_inline::GoogleFontCssInlineBehavior::for_background(
                    driver.clone(),
                    attrs_charset,
                    htmls_charset,
                    size_threshold_bytes,
                );
            let mut context = InlineRewriteContext::new(
                driver.clone(),
                behavior,
                0,
                driver.base_url().clone(),
            );
            context.run_background_inline(input, partition_key)
        }
    }
}

// Snapshots cross threads to pool workers: every field must be `Send`.
#[allow(dead_code)]
fn assert_send() {
    fn is_send<T: Send>() {}
    is_send::<HtmlRewriteSnapshot>();
    is_send::<HtmlRewriteShared>();
    is_send::<BackgroundBehavior>();
    is_send::<RewriteOptions>();
    let _ = InlineAuthorizationPolicy::InlineOnlyAuthorizedResources;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::cache::MemoryCache;
    use ps_config::filter::Filter;
    use ps_http::mock::MockUrlFetcher;
    use ps_http::names::header;
    use ps_http::response::HttpOptions;
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    /// Shared output sink for capturing rewritten HTML.
    #[derive(Clone, Default)]
    struct SharedVec {
        output: Rc<RefCell<Vec<u8>>>,
    }

    impl crate::writer::ByteSink for SharedVec {
        fn write_bytes(&mut self, bytes: &[u8]) -> bool {
            self.output.borrow_mut().extend_from_slice(bytes);
            true
        }
    }

    fn css_server(css_url: &str) -> (Rc<ServerContext>, Rc<RefCell<RewriteOptions>>) {
        let fetcher = MockUrlFetcher::new();
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.set_date_and_caching(1_700_000_000_000, 300_000, "");
        headers.compute_caching();
        fetcher.set_response(
            css_url,
            headers,
            b"div  {  display  :  block  }  /* tint */",
        );
        let server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(fetcher),
            1_700_000_000_000,
        ));
        let mut options = RewriteOptions::new();
        options.filters_mut().enable(Filter::RewriteCss);
        (server, Rc::new(RefCell::new(options)))
    }

    fn drive_html(
        server: &Rc<ServerContext>,
        options: &Rc<RefCell<RewriteOptions>>,
        html: &str,
    ) -> (String, u64) {
        let resource_driver = Rc::new(ResourceDriver::new(
            server.clone(),
            options.clone(),
            "http://test.com/page.html",
        ));
        let sink = SharedVec::default();
        let ctx = crate::chain::ChainContext::new(true, true);
        let mut driver = crate::driver::Driver::new(
            &options.borrow(),
            &ctx,
            sink.clone(),
            Some(resource_driver.clone()),
        );
        driver.start_parse(b"http://test.com/page.html", false);
        driver.write(html.as_bytes());
        driver.finish();
        let out = String::from_utf8_lossy(&sink.output.borrow()).into_owned();
        (out, resource_driver.detached_rewrites())
    }

    fn counter(server: &ServerContext, name: &str) -> i64 {
        server
            .statistics()
            .get_variable(name)
            .map(|v| v.get())
            .unwrap_or(0)
    }

    fn wait_for(server: &ServerContext, name: &str, want: i64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while counter(server, name) < want {
            assert!(Instant::now() < deadline, "{name} never reached {want}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn miss_schedules_async_and_serves_original_then_optimized() {
        let css_url = "http://test.com/a.css";
        let (server, options) = css_server(css_url);
        let pool = BackgroundPool::new(
            super::super::background::PoolConfig {
                threads: 2,
                queue_bound: 64,
            },
            &server.statistics_shared(),
        );
        server.set_background_pool(Arc::clone(&pool));
        let html = format!(
            "<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>"
        );

        // First view: metadata miss schedules the optimization and
        // serves the original URL immediately (detached render).
        let (first, detached) = drive_html(&server, &options, &html);
        assert!(
            !first.contains("pagespeed"),
            "miss must serve original URLs, got: {first}"
        );
        assert!(first.contains(css_url), "original href kept: {first}");
        assert_eq!(detached, 1, "miss render detaches (no HTML-cache publish)");
        assert_eq!(counter(&server, HTML_ASYNC_SCHEDULED), 1);

        // The background completion lands without any further HTML.
        wait_for(&server, HTML_ASYNC_COMPLETED, 1);

        // Second view: warm metadata serves the optimized URL inline,
        // with no new background work.
        let (second, detached) = drive_html(&server, &options, &html);
        assert!(
            second.contains("pagespeed"),
            "warm view must serve optimized URLs, got: {second}"
        );
        assert_eq!(detached, 0, "hit render is complete, not detached");
        assert_eq!(counter(&server, HTML_ASYNC_SCHEDULED), 1);
        pool.shutdown();
    }

    #[test]
    fn inline_miss_defers_and_warm_view_inlines() {
        let css_url = "http://test.com/b.css";
        let (server, _) = css_server(css_url);
        let mut options = RewriteOptions::new();
        options.filters_mut().enable(Filter::InlineCss);
        let options = Rc::new(RefCell::new(options));
        let pool = BackgroundPool::new(
            super::super::background::PoolConfig {
                threads: 2,
                queue_bound: 64,
            },
            &server.statistics_shared(),
        );
        server.set_background_pool(Arc::clone(&pool));
        let html = format!(
            "<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>"
        );

        // First view defers: the link stays external.
        let (first, detached) = drive_html(&server, &options, &html);
        assert!(first.contains(css_url), "miss keeps the link: {first}");
        assert_eq!(detached, 1);
        assert_eq!(counter(&server, HTML_ASYNC_SCHEDULED), 1);

        wait_for(&server, HTML_ASYNC_COMPLETED, 1);

        // Warm view inlines the stylesheet.
        let (second, detached) = drive_html(&server, &options, &html);
        assert!(
            !second.contains(css_url),
            "warm view inlines, dropping the link: {second}"
        );
        assert!(second.contains("display"), "inlined bytes render: {second}");
        assert_eq!(detached, 0);
        pool.shutdown();
    }

    #[test]
    fn no_pool_keeps_sync_rewrite() {
        // Without a pool (tests, minimal embeds) the miss path still
        // optimizes inline: behavior unchanged.
        let css_url = "http://test.com/a.css";
        let (server, options) = css_server(css_url);
        let html = format!(
            "<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>"
        );
        let (out, detached) = drive_html(&server, &options, &html);
        assert!(out.contains("pagespeed"), "sync rewrite preserved: {out}");
        assert_eq!(detached, 0);
        assert_eq!(counter(&server, HTML_ASYNC_SCHEDULED), 0);
    }

    #[test]
    fn shutdown_pool_falls_back_to_sync() {
        // A gone pool degrades to inline completion, never failure.
        let css_url = "http://test.com/a.css";
        let (server, options) = css_server(css_url);
        let pool = BackgroundPool::new(
            super::super::background::PoolConfig {
                threads: 1,
                queue_bound: 64,
            },
            &server.statistics_shared(),
        );
        server.set_background_pool(Arc::clone(&pool));
        pool.shutdown();
        let html = format!(
            "<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>"
        );
        let (out, _) = drive_html(&server, &options, &html);
        assert!(
            out.contains("pagespeed"),
            "shutdown degrades to sync: {out}"
        );
    }
}
