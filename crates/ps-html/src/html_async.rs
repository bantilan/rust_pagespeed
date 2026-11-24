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
//! next request). Custom behaviors without a snapshot and the
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
    /// Adjacent stylesheet candidates; worker uses the original partitioner.
    CombineCss {
        inputs: Vec<(String, bool)>,
        current_base: String,
    },
    /// Adjacent script candidates and per-element/page charsets.
    CombineJs {
        scripts: Vec<(String, Vec<u8>)>,
        containing_charset: Vec<u8>,
    },
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
    /// Preserve font-service UA keys and response hooks on the worker.
    pub font_service: bool,
    /// Partition key the HTML lookup missed (the worker writes here).
    pub partition_key: String,
    /// Containing page URL.
    pub base_url: String,
    /// Effective rewrite options for the request.
    pub options: RewriteOptions,
    /// Request user agent.
    pub user_agent: String,
    /// Preserve explicit format rejection and downstream capability gates.
    pub request_headers: ps_http::request::RequestHeaders,
    /// Preserve fetch options and session-authorized origins.
    pub request_context: ps_http::request::RequestContext,
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
    /// Message sink for completion records (`None` keeps the
    /// request-thread-only reporting).
    pub message_sink: Option<Arc<dyn super::server_context::MessageSink>>,
    /// Optional cache-domain ownership; claim occurs only on the pool worker.
    pub coordinator: Option<Arc<dyn ps_cache::rewrite_coordinator::RewriteCoordinator>>,
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
    /// No pool installed (synchronous embedding/test compatibility).
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
        message_sink: server.message_sink(),
        coordinator: server.rewrite_coordinator(),
    }
}

/// Submits a background single-input rewrite, fire-and-forget.
///
/// Dedup key: the partition key (concurrent same-resource misses join
/// one execution). Returns [`ScheduleOutcome::SyncFallback`] only when
/// no pool is installed. Shutdown and saturation both shed to original URLs.
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

/// Schedules a combination without loading any candidate bytes on the HTML thread.
pub(crate) fn schedule_combination(
    driver: &ResourceDriver,
    behavior: BackgroundBehavior,
    key: &str,
) {
    let Some(pool) = driver.server_context().background_pool() else {
        return;
    };
    let (base_url, options, user_agent, response_charset, now_ms) = snapshot_driver(driver);
    let snapshot = HtmlRewriteSnapshot {
        behavior,
        input_url: String::new(),
        input_authorized: false,
        font_service: false,
        partition_key: key.to_string(),
        base_url,
        options,
        user_agent,
        request_headers: driver.request_headers().clone(),
        request_context: driver.request_context().borrow().clone(),
        response_charset,
        now_ms,
    };
    let shared = snapshot_shared(driver);
    let job_key = format!("html-combine:{key}");
    if pool
        .submit(&job_key, move || run_rewrite_job(snapshot, shared))
        .is_ok()
    {
        driver
            .server_context()
            .variable(HTML_ASYNC_SCHEDULED)
            .add(1);
    }
    driver.note_detached_rewrite();
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
        font_service: matches!(input, InputResource::Font(_)),
        partition_key: partition_key.to_string(),
        base_url,
        options,
        user_agent,
        request_headers: driver.request_headers().clone(),
        request_context: driver.request_context().borrow().clone(),
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
        Err(SubmitError::Shutdown) => ScheduleOutcome::Shed,
    }
}

/// Runs one background rewrite on a pool thread: rebuilds an equivalent
/// driver from the snapshot and runs the sync miss core to partitions
/// (no rendering). Returns whether partitions were written.
fn run_rewrite_job(snapshot: HtmlRewriteSnapshot, mut shared: HtmlRewriteShared) -> bool {
    use ps_cache::rewrite_coordinator::{LeasedMetadataCache, RewriteClaim};
    let mut leased = None;
    if let Some(coordinator) = shared.coordinator.take() {
        let kind = if matches!(
            &snapshot.behavior,
            BackgroundBehavior::CombineCss { .. } | BackgroundBehavior::CombineJs { .. }
        ) {
            "html-combine"
        } else {
            "html-rewrite"
        };
        match coordinator.claim(&format!("{kind}:{}", snapshot.partition_key)) {
            RewriteClaim::Owned(lease) => {
                let cache = Arc::new(LeasedMetadataCache::new(shared.metadata.clone(), lease));
                shared.metadata = cache.clone();
                leased = Some(cache);
            }
            RewriteClaim::Retry => return false,
            RewriteClaim::Local => (),
        }
    }
    let statistics = shared.statistics.clone();
    let wrote = run_rewrite_job_inner(snapshot, shared)
        && leased
            .as_ref()
            .is_none_or(|cache| cache.publications_valid());
    if wrote {
        statistics.add_variable(HTML_ASYNC_COMPLETED).add(1);
    }
    wrote
}

fn run_rewrite_job_inner(snapshot: HtmlRewriteSnapshot, shared: HtmlRewriteShared) -> bool {
    let HtmlRewriteShared {
        http_cache,
        metadata,
        fetcher,
        statistics,
        hasher,
        message_sink,
        coordinator: _,
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
    driver.set_request_headers(snapshot.request_headers);
    *driver.request_context().borrow_mut() = snapshot.request_context;
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
    let tables = match &snapshot.behavior {
        BackgroundBehavior::CombineCss {
            inputs,
            current_base,
        } => Some(
            super::filters::css_combine::CssCombineFilter::background_combine(
                driver.clone(),
                inputs.clone(),
                current_base,
            ),
        ),
        BackgroundBehavior::CombineJs {
            scripts,
            containing_charset,
        } => Some(
            super::filters::js_combine::JsCombineFilter::background_combine(
                driver.clone(),
                scripts.clone(),
                containing_charset.clone(),
            ),
        ),
        _ => None,
    };
    if let Some(tables) = tables {
        if let Some(tables) = tables {
            let bytes = super::metadata::encode_partitions(&tables);
            driver
                .server_context()
                .metadata_cache()
                .put(&snapshot.partition_key, &bytes);
            return true;
        }
        return false;
    }
    let input_url = GoogleUrl::new(&snapshot.input_url);
    if !input_url.is_web_valid() {
        return false;
    }
    let input = if snapshot.font_service {
        super::font_service_input_resource::FontServiceInputResource::make_font_service(
            &input_url, &driver,
        )
        .map(InputResource::Font)
    } else {
        driver.create_input_resource_unchecked(&input_url, snapshot.input_authorized)
    };
    let Some(input) = input else {
        return false;
    };
    let wrote = run_behavior_rewrite(&driver, snapshot.behavior, input, &snapshot.partition_key);
    // Report worker-side image records (request-thread drivers
    // drain their own; without this the admin images page misses
    // every background optimization).
    if let Some(sink) = message_sink.as_ref() {
        for record in driver.take_image_records() {
            sink.message(
                ps_config::message_handler::MessageLevel::Info,
                record.message_line(),
            );
        }
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
        BackgroundBehavior::CombineCss { .. } | BackgroundBehavior::CombineJs { .. } => false,
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
            let mut context =
                InlineRewriteContext::new(driver.clone(), behavior, 0, driver.base_url().clone());
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
            let mut context =
                InlineRewriteContext::new(driver.clone(), behavior, 0, driver.base_url().clone());
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
            let mut context =
                InlineRewriteContext::new(driver.clone(), behavior, 0, driver.base_url().clone());
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

    const START_MS_FOR_SINK_TEST: i64 = 1_700_000_000_000;

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

    #[cfg(target_os = "linux")]
    struct CoordinatorDirectory(std::path::PathBuf);
    #[cfg(target_os = "linux")]
    impl CoordinatorDirectory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "ps-html-coordinator-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn coordinator(&self) -> Arc<dyn ps_cache::rewrite_coordinator::RewriteCoordinator> {
            Arc::new(
                ps_cache::rewrite_coordinator::SharedRewriteCoordinator::open(
                    &self.0.join("owners-v2"),
                    8,
                    Duration::from_millis(5),
                )
                .unwrap(),
            )
        }
    }
    #[cfg(target_os = "linux")]
    impl Drop for CoordinatorDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn coordinated_css_and_js_combinations_warm_later_html() {
        for (filter, mime, extension, body, output_id) in [
            (
                Filter::CombineCss,
                "text/css",
                "css",
                b"div { display: block; }".as_slice(),
                ".pagespeed.cc.",
            ),
            (
                Filter::CombineJavascript,
                "application/javascript",
                "js",
                b"window.example = 123;".as_slice(),
                ".pagespeed.jc.",
            ),
        ] {
            let directory = CoordinatorDirectory::new();
            let fetcher = MockUrlFetcher::new();
            let first = format!("http://test.com/a.{extension}");
            let second = format!("http://test.com/b.{extension}");
            let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
            headers.set_first_line(1, 1, 200, "OK");
            headers.headers_mut().add(header::CONTENT_TYPE, mime);
            headers.set_date_and_caching(START_MS_FOR_SINK_TEST, 300_000, "");
            headers.compute_caching();
            for url in [&first, &second] {
                fetcher.set_response(url, headers.clone(), body);
            }
            let server = Rc::new(ServerContext::new_with_metadata(
                Arc::new(MemoryCache::new()),
                Arc::new(MemoryCache::new()),
                Arc::new(fetcher),
                START_MS_FOR_SINK_TEST,
            ));
            server.set_rewrite_coordinator(directory.coordinator());
            let pool = BackgroundPool::new(
                super::super::background::PoolConfig {
                    threads: 1,
                    queue_bound: 8,
                },
                &server.statistics_shared(),
            );
            server.set_background_pool(pool.clone());
            let mut options = RewriteOptions::new();
            options.filters_mut().enable(filter);
            let options = Rc::new(RefCell::new(options));
            let elements = if extension == "css" {
                format!(
                    "<link rel='stylesheet' href='{first}'><link rel='stylesheet' href='{second}'>"
                )
            } else {
                format!("<script src='{first}'></script><script src='{second}'></script>")
            };
            let html = format!("<html><head>{elements}</head><body></body></html>");
            let (cold, detached) = drive_html(&server, &options, &html);
            assert!(cold.contains(&first) && cold.contains(&second));
            assert!(detached > 0);
            wait_for(&server, HTML_ASYNC_COMPLETED, 1);
            let (warm, _) = drive_html(&server, &options, &html);
            pool.shutdown();
            assert!(warm.contains(output_id), "combination absent: {warm}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn remote_claim_defers_fetch_and_release_allows_metadata_publication() {
        use ps_cache::rewrite_coordinator::RewriteClaim;
        let directory = CoordinatorDirectory::new();
        let peer = directory.coordinator();
        let remote = match peer.claim("html-rewrite:test-partition") {
            RewriteClaim::Owned(lease) => lease,
            _ => panic!("remote claim failed"),
        };
        let (server, options) = css_server("http://test.com/a.css");
        server.set_rewrite_coordinator(directory.coordinator());
        let driver =
            ResourceDriver::new(server.clone(), options.clone(), "http://test.com/page.html");
        let shared = snapshot_shared(&driver);
        let snapshot = HtmlRewriteSnapshot {
            behavior: BackgroundBehavior::Css {
                root_media: vec![],
                root_charset: vec![],
                root_charset_failure: None,
            },
            input_url: "http://test.com/a.css".into(),
            input_authorized: true,
            font_service: false,
            partition_key: "test-partition".into(),
            base_url: "http://test.com/page.html".into(),
            options: options.borrow().clone(),
            user_agent: String::new(),
            request_headers: driver.request_headers().clone(),
            request_context: driver.request_context().borrow().clone(),
            response_charset: None,
            now_ms: START_MS_FOR_SINK_TEST,
        };
        assert!(!run_rewrite_job(snapshot.clone(), shared.clone()));
        assert_eq!(counter(&server, HTML_ASYNC_COMPLETED), 0);
        assert!(server
            .metadata_cache()
            .get("test-partition")
            .value
            .is_none());
        drop(remote);
        assert!(run_rewrite_job(snapshot, shared));
        assert_eq!(counter(&server, HTML_ASYNC_COMPLETED), 1);
        assert!(server
            .metadata_cache()
            .get("test-partition")
            .value
            .is_some());
    }

    #[cfg(target_os = "linux")]
    struct LoggedFetcher {
        inner: Arc<dyn AsyncFetcher>,
        directory: std::path::PathBuf,
        gated: bool,
    }
    #[cfg(target_os = "linux")]
    impl AsyncFetcher for LoggedFetcher {
        fn fetch(&self, url: &str, fetch: &mut dyn ps_http::fetch::AsyncFetch) {
            use std::io::Write;
            let mut log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.directory.join("fetches"))
                .unwrap();
            writeln!(log, "fetch").unwrap();
            if self.gated {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !self.directory.join("release").exists() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            self.inner.fetch(url, fetch);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn coordinated_worker_subprocess() {
        let Some(directory) = std::env::var_os("PS_HTML_COORDINATOR_CHILD") else {
            return;
        };
        let directory = std::path::PathBuf::from(directory);
        let mode = std::env::var("PS_HTML_COORDINATOR_MODE").unwrap();
        let (server, options) = css_server("http://test.com/a.css");
        let driver =
            ResourceDriver::new(server.clone(), options.clone(), "http://test.com/page.html");
        let mut shared = snapshot_shared(&driver);
        shared.coordinator = Some(Arc::new(
            ps_cache::rewrite_coordinator::SharedRewriteCoordinator::open(
                &directory.join("owners-v2"),
                8,
                Duration::from_millis(5),
            )
            .unwrap(),
        ));
        let backend = Arc::new(ps_cache::cache::FileCache::new(
            &directory.join("cache"),
            ps_cache::cache::FileCachePolicy {
                clean_interval_ms: -1,
                target_size_bytes: 16_777_216,
                target_inode_count: 0,
                path_length_limit: 8192,
            },
            START_MS_FOR_SINK_TEST,
        ));
        shared.metadata = backend.clone();
        shared.http_cache = Arc::new(HttpCache::new(backend));
        shared.fetcher = Arc::new(LoggedFetcher {
            inner: shared.fetcher.clone(),
            directory: directory.clone(),
            gated: mode == "gated",
        });
        let snapshot = HtmlRewriteSnapshot {
            behavior: BackgroundBehavior::Css {
                root_media: vec![],
                root_charset: vec![],
                root_charset_failure: None,
            },
            input_url: "http://test.com/a.css".into(),
            input_authorized: true,
            font_service: false,
            partition_key: "process-partition".into(),
            base_url: "http://test.com/page.html".into(),
            options: options.borrow().clone(),
            user_agent: String::new(),
            request_headers: driver.request_headers().clone(),
            request_context: driver.request_context().borrow().clone(),
            response_charset: None,
            now_ms: START_MS_FOR_SINK_TEST,
        };
        let pool = BackgroundPool::new(
            super::super::background::PoolConfig {
                threads: 1,
                queue_bound: 2,
            },
            &server.statistics_shared(),
        );
        let job = pool
            .submit("test-job", move || run_rewrite_job(snapshot, shared))
            .unwrap();
        let wrote = job
            .wait_timeout::<bool>(Duration::from_secs(12))
            .map(|value| *value);
        pool.shutdown();
        assert_eq!(wrote, Some(mode != "duplicate"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn separate_worker_processes_fetch_once_and_recover_killed_optimizer() {
        fn child(path: &std::path::Path, mode: &str) -> std::process::Command {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "html_async::tests::coordinated_worker_subprocess",
                    "--nocapture",
                ])
                .env("PS_HTML_COORDINATOR_CHILD", path)
                .env("PS_HTML_COORDINATOR_MODE", mode)
                .stdout(std::process::Stdio::null());
            command
        }
        for kill_owner in [false, true] {
            let directory = CoordinatorDirectory::new();
            let mut first = child(&directory.0, "gated").spawn().unwrap();
            let fetches = directory.0.join("fetches");
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fetches.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            if !fetches.exists() {
                let _ = first.kill();
                let _ = first.wait();
                panic!("first worker never fetched");
            }
            let duplicate = child(&directory.0, "duplicate").status();
            if kill_owner {
                first.kill().unwrap();
            } else {
                std::fs::write(directory.0.join("release"), b"go").unwrap();
            }
            let first_status = first.wait().unwrap();
            assert!(duplicate.unwrap().success());
            assert_eq!(
                std::fs::read_to_string(&fetches).unwrap().lines().count(),
                1
            );
            if kill_owner {
                assert!(child(&directory.0, "recover").status().unwrap().success());
            } else {
                assert!(first_status.success());
            }
            let backend = ps_cache::cache::FileCache::new(
                &directory.0.join("cache"),
                ps_cache::cache::FileCachePolicy {
                    clean_interval_ms: -1,
                    target_size_bytes: 16_777_216,
                    target_inode_count: 0,
                    path_length_limit: 8192,
                },
                START_MS_FOR_SINK_TEST,
            );
            let tables = backend
                .get("process-partition")
                .value
                .expect("worker did not publish partition metadata");
            let tables = super::super::metadata::decode_partitions(&tables).unwrap();
            assert!(tables
                .partition
                .iter()
                .any(|result| result.optimizable && result.url.contains(".pagespeed.cf.")));
            assert_eq!(
                std::fs::read_to_string(&fetches).unwrap().lines().count(),
                if kill_owner { 2 } else { 1 }
            );
        }
    }

    #[derive(Debug)]
    struct RequestThreadForbiddenCoordinator;
    impl ps_cache::rewrite_coordinator::RewriteCoordinator for RequestThreadForbiddenCoordinator {
        fn claim(&self, _key: &str) -> ps_cache::rewrite_coordinator::RewriteClaim {
            panic!("foreground or rejected job must never claim ownership")
        }
    }

    #[test]
    fn miss_schedules_async_and_serves_original_then_optimized() {
        let css_url = "http://test.com/a.css";
        let (server, options) = css_server(css_url);
        #[cfg(target_os = "linux")]
        let directory = CoordinatorDirectory::new();
        #[cfg(target_os = "linux")]
        server.set_rewrite_coordinator(directory.coordinator());
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
        #[cfg(target_os = "linux")]
        let directory = CoordinatorDirectory::new();
        #[cfg(target_os = "linux")]
        server.set_rewrite_coordinator(directory.coordinator());
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
    fn worker_image_rewrite_reports_record_to_sink() {
        use ps_config::filter::Filter;
        use std::sync::Mutex;

        /// Collecting test sink.
        #[derive(Debug, Default)]
        struct CollectingSink {
            lines: Mutex<Vec<String>>,
        }

        impl super::super::server_context::MessageSink for CollectingSink {
            fn message(&self, _level: ps_config::message_handler::MessageLevel, text: String) {
                self.lines.lock().unwrap().push(text);
            }
        }

        // Worker-side eager image optimization (the CSS/fetch shape)
        // must report its admin row through the installed sink;
        // request-thread drivers drain their own, so without this
        // every background optimization would vanish from the admin
        // images page.
        let image_url = "http://test.com/Puzzle.jpg";
        let fetcher = MockUrlFetcher::new();
        let server_probe = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(MockUrlFetcher::new()),
            START_MS_FOR_SINK_TEST,
        ));
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        server_probe.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("image/jpeg"),
            "",
            "",
            &mut headers,
        );
        let image_bytes = include_bytes!("../tests/data/Puzzle.jpg");
        fetcher.set_response(image_url, headers, image_bytes);
        let server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(fetcher),
            START_MS_FOR_SINK_TEST,
        ));
        let sink = Arc::new(CollectingSink::default());
        server.set_message_sink(sink.clone());
        let mut options = RewriteOptions::new();
        options.soft_enable_filter_for_testing(Filter::RecompressJpeg);
        options.soft_enable_filter_for_testing(Filter::RecompressPng);
        options.soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
        let options = Rc::new(RefCell::new(options));
        let driver = Rc::new(ResourceDriver::new(
            server.clone(),
            options.clone(),
            "http://test.com/page.html",
        ));
        let shared = snapshot_shared(&driver);
        assert!(shared.message_sink.is_some());
        let snapshot = HtmlRewriteSnapshot {
            behavior: BackgroundBehavior::Image {
                resource_context: ImageResourceContext::default(),
                place: super::super::filters::image_rewrite::RewritePlace::Fetch,
                image_index: 0,
                in_noscript_element: false,
                css_image_inline_max_bytes: 0,
                is_resized_using_rendered_dimensions: false,
                base_url: "http://test.com/page.html".to_string(),
            },
            input_url: image_url.to_string(),
            input_authorized: true,
            font_service: false,
            partition_key: "test-sink-partition".to_string(),
            base_url: "http://test.com/page.html".to_string(),
            options: options.borrow().clone(),
            user_agent: String::new(),
            request_headers: driver.request_headers().clone(),
            request_context: driver.request_context().borrow().clone(),
            response_charset: None,
            now_ms: START_MS_FOR_SINK_TEST,
        };
        assert!(run_rewrite_job(snapshot, shared));
        let lines = sink.lines.lock().unwrap();
        assert_eq!(1, lines.len(), "{lines:?}");
        assert!(lines[0].starts_with("PS_IMAGE_DETAIL\t"), "{}", lines[0]);
        assert!(lines[0].contains("Puzzle.jpg"), "{}", lines[0]);
        // The partition landed in the shared metadata cache too.
        assert_eq!(
            server.metadata_cache().get("test-sink-partition").state,
            ps_cache::cache::KeyState::Available
        );
    }

    #[test]
    fn no_pool_keeps_sync_rewrite() {
        // Without a pool (tests, minimal embeds) the miss path still
        // optimizes inline: behavior unchanged.
        let css_url = "http://test.com/a.css";
        let (server, options) = css_server(css_url);
        server.set_rewrite_coordinator(Arc::new(RequestThreadForbiddenCoordinator));
        let html = format!(
            "<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>"
        );
        let (out, detached) = drive_html(&server, &options, &html);
        assert!(out.contains("pagespeed"), "sync rewrite preserved: {out}");
        assert_eq!(detached, 0);
        assert_eq!(counter(&server, HTML_ASYNC_SCHEDULED), 0);
    }

    #[test]
    fn shutdown_pool_keeps_original_url() {
        // A gone pool keeps valid fallback HTML without an inline fetch.
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
        server.set_rewrite_coordinator(Arc::new(RequestThreadForbiddenCoordinator));
        let html = format!(
            "<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>"
        );
        let (out, detached) = drive_html(&server, &options, &html);
        assert!(!out.contains("pagespeed"), "{out}");
        assert!(out.contains(css_url), "{out}");
        assert!(detached > 0);
    }
}
