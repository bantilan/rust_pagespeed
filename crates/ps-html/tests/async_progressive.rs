//! Progressive HTML rewriting tests: metadata HIT/MISS progression
//! across requests, concurrent-render dedup, and saturation
//! shedding. Misses schedule the optimization on the background
//! pool and serve original URLs immediately (fire-and-forget: no
//! waiting, no deadline involved); the completion warms the next
//! request to a metadata HIT. Queue-full sheds to fallback fast,
//! never parking past the deadline.
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::background::{BackgroundPool, PoolConfig};
use ps_html::cacheable_resource::{
    CacheableResourceBase, MockLoadCallback, NotCacheablePolicy, ResourceHooks,
};
use ps_html::chain::ChainContext;
use ps_html::driver::Driver;
use ps_html::filters::css::CssFilter;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::fetch::{AsyncFetch, AsyncFetcher, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::request::RequestContext;
use ps_http::response::{HttpOptions, ResponseHeaders};

const START_MS: i64 = 1_700_000_000_000;
const CSS_URL: &str = "http://test.com/a.css";
const CSS_BODY: &[u8] = b"div  {  color : red;  } /* progressive probe body */\n";

/// Hooks without behavior (fetch plumbing only).
#[derive(Clone, Default)]
struct TestHooks;

impl ResourceHooks for TestHooks {}

/// Scripted origin with overt latency + call count.
struct SlowFetcher {
    delay: Duration,
    count: AtomicU64,
    inner: MockUrlFetcher,
}

impl SlowFetcher {
    fn new(delay: Duration) -> Self {
        SlowFetcher {
            delay,
            count: AtomicU64::new(0),
            inner: MockUrlFetcher::new(),
        }
    }

    fn script_css(&self, url: &str, body: &[u8], now_ms: i64) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.set_date_and_caching(now_ms, 300_000, "");
        headers.compute_caching();
        self.inner.set_response(url, headers, body);
    }
}

impl AsyncFetcher for SlowFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        self.count.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.inner.fetch(url, fetch);
    }
}

#[derive(Debug)]
struct CountedHttpCache {
    inner: MemoryCache,
    reads: Arc<AtomicU64>,
}
impl ps_cache::cache::Cache for CountedHttpCache {
    fn get(&self, key: &str) -> ps_cache::cache::Lookup {
        self.reads.fetch_add(1, Ordering::SeqCst);
        ps_cache::cache::Cache::get(&self.inner, key)
    }
    fn put(&self, key: &str, value: &[u8]) {
        ps_cache::cache::Cache::put(&self.inner, key, value);
    }
    fn delete(&self, key: &str) {
        ps_cache::cache::Cache::delete(&self.inner, key);
    }
    fn name(&self) -> String {
        "CountedHttpCache".into()
    }
}

struct World {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Arc<SlowFetcher>,
    pool: Arc<BackgroundPool>,
    http_reads: Arc<AtomicU64>,
}

impl World {
    fn new(origin_ms: u64, deadline_ms: i64, threads: usize, queue_bound: usize) -> Self {
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options.borrow_mut().enable_filter(Filter::RewriteCss);
        options
            .borrow_mut()
            .set_option(
                "RewriteDeadlinePerFlushMs",
                &deadline_ms.to_string(),
                Scope::Server,
            )
            .unwrap();
        let fetcher = Arc::new(SlowFetcher::new(Duration::from_millis(origin_ms)));
        fetcher.script_css(CSS_URL, CSS_BODY, START_MS);
        let http_reads = Arc::new(AtomicU64::new(0));
        let server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(CountedHttpCache {
                inner: MemoryCache::new(),
                reads: http_reads.clone(),
            }),
            Arc::new(MemoryCache::new()),
            fetcher.clone() as Arc<dyn AsyncFetcher>,
            START_MS,
        ));
        let pool = BackgroundPool::new(
            PoolConfig {
                threads,
                queue_bound,
            },
            &server.statistics_shared(),
        );
        server.set_background_pool(Arc::clone(&pool));
        World {
            server,
            options,
            fetcher,
            pool,
            http_reads,
        }
    }

    /// Drives one full HTML request through the resource driver.
    fn drive(&self, css_url: &str) -> String {
        let html = format!("<head><link rel='stylesheet' href='{css_url}'></head><body></body>");
        self.drive_markup(&html)
    }

    fn drive_markup(&self, html: &str) -> String {
        self.drive_markup_on(self.server.clone(), html)
    }

    fn drive_markup_on(&self, server: Rc<ServerContext>, html: &str) -> String {
        let resource_driver = Rc::new(ResourceDriver::new(
            server,
            self.options.clone(),
            "http://test.com/page.html",
        ));
        let ctx = ChainContext::new(true, true);
        let sink = SharedSink::default();
        let mut driver = Driver::new(
            &self.options.borrow(),
            &ctx,
            sink.clone(),
            Some(resource_driver),
        );
        driver.start_parse(b"http://test.com/page.html", false);
        driver.write(html.as_bytes());
        driver.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Serves a rewritten URL through the CSS filter's fetch path.
    fn serve_url(&self, url: &str) -> Option<Vec<u8>> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.add_filter(Rc::new(CssFilter::new(driver.clone())));
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        let handled = driver.fetch_resource(url, &mut fetch);
        assert_eq!(handled, fetch.done());
        if handled && fetch.success() {
            Some(fetch.body().to_vec())
        } else {
            None
        }
    }

    fn stat(&self, name: &str) -> i64 {
        self.server
            .statistics()
            .get_variable(name)
            .unwrap_or_else(|| panic!("missing stat {name}"))
            .get()
    }

    fn await_completions(&self, want: i64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.stat("background_jobs_completed") < want {
            assert!(Instant::now() < deadline, "background jobs stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.pool.shutdown();
    }
}

/// Extracts the stylesheet href from a rendered page.
fn stylesheet_href(html: &str) -> String {
    let key = "href='";
    let start = html.find(key).expect("stylesheet href") + key.len();
    let end = html[start..].find('\'').expect("href end") + start;
    html[start..end].to_string()
}

#[test]
fn html_miss_falls_back_then_progresses_to_metadata_hit() {
    let world = World::new(300, 30, 2, 16);
    // First request: fire-and-forget schedule, original link kept,
    // nothing waited on (no timeout, no deadline miss — the request
    // never parks on background work).
    let start = Instant::now();
    let first = world.drive(CSS_URL);
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "first request must not block on the fetch"
    );
    assert!(first.contains("a.css"), "{first}");
    assert!(!first.contains("pagespeed"), "{first}");
    assert_eq!(world.stat("rewrite_cached_output_misses"), 1);
    assert_eq!(world.stat("rewrite_cached_output_hits"), 0);
    assert_eq!(world.stat("rewrite_cached_output_missed_deadline"), 0);
    assert_eq!(world.stat("background_jobs_timeouts"), 0);
    assert_eq!(world.stat("html_async_rewrite_scheduled"), 1);
    // The background completion optimizes and writes the partition,
    // so the very next request is already a metadata HIT rendering
    // the optimized URL with no fetch and no rewrite.
    world.await_completions(1);
    assert_eq!(world.stat("html_async_rewrite_completed"), 1);
    let second = world.drive(CSS_URL);
    assert!(second.contains(".pagespeed."), "{second}");
    assert_eq!(world.stat("rewrite_cached_output_misses"), 1);
    assert_eq!(world.stat("rewrite_cached_output_hits"), 1);
    assert_eq!(world.stat("html_async_rewrite_scheduled"), 1);
    let rewritten = stylesheet_href(&second);
    // Third request: HIT again, identical URL, still one origin fetch.
    let third = world.drive(CSS_URL);
    assert_eq!(stylesheet_href(&third), rewritten);
    assert_eq!(world.stat("rewrite_cached_output_misses"), 1);
    assert_eq!(world.stat("rewrite_cached_output_hits"), 2);
    assert_eq!(world.fetcher.count.load(Ordering::SeqCst), 1);
    // The fetch path byte-serves the rewritten URL (minified CSS,
    // not the original).
    let body = world.serve_url(&rewritten).expect("rewritten URL serves");
    assert!(!body.is_empty());
    assert!(body.len() < CSS_BODY.len(), "{}", body.len());
    assert!(body.windows(4).any(|w| w == b"div{"));
    assert!(!body.windows(2).any(|w| w == b"/*"));
}

#[test]
fn concurrent_html_renders_share_one_fetch() {
    use ps_stats::core::Statistics;

    // One slow origin, one pool, shared backends: three threads
    // each build their own (!Send) HTML stack and render the same
    // page at once. Dedup collapses the triple miss into one
    // background rewrite (one fetch); every render serves the
    // original link immediately, and the next render HITs.
    let http_backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let metadata_backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let fetcher = Arc::new(SlowFetcher::new(Duration::from_millis(200)));
    fetcher.script_css(CSS_URL, CSS_BODY, START_MS);
    let statistics = Arc::new(Statistics::new());
    let pool = BackgroundPool::new(
        PoolConfig {
            threads: 2,
            queue_bound: 16,
        },
        &statistics,
    );
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for _ in 0..3 {
        let (http, meta, fetcher, pool, barrier) = (
            Arc::clone(&http_backend),
            Arc::clone(&metadata_backend),
            Arc::clone(&fetcher),
            Arc::clone(&pool),
            Arc::clone(&barrier),
        );
        threads.push(std::thread::spawn(move || {
            let server = Rc::new(ServerContext::new_with_metadata(
                http,
                meta,
                fetcher as Arc<dyn AsyncFetcher>,
                START_MS,
            ));
            server.set_background_pool(Arc::clone(&pool));
            let options = Rc::new(RefCell::new(RewriteOptions::new()));
            options.borrow_mut().enable_filter(Filter::RewriteCss);
            options
                .borrow_mut()
                .set_option("RewriteDeadlinePerFlushMs", "5000", Scope::Server)
                .unwrap();
            barrier.wait();
            let resource_driver = Rc::new(ResourceDriver::new(
                server,
                options.clone(),
                "http://test.com/page.html",
            ));
            let ctx = ChainContext::new(true, true);
            let sink = SharedSink::default();
            let start = Instant::now();
            let mut driver =
                Driver::new(&options.borrow(), &ctx, sink.clone(), Some(resource_driver));
            driver.start_parse(b"http://test.com/page.html", false);
            driver.write(
                format!("<head><link rel='stylesheet' href='{CSS_URL}'></head><body></body>")
                    .as_bytes(),
            );
            driver.finish();
            // Fire-and-forget: even a 5s deadline never parks the
            // render on the 200ms background optimization.
            assert!(
                start.elapsed() < Duration::from_millis(150),
                "concurrent render must not wait for the rewrite"
            );
            String::from_utf8(sink.bytes()).unwrap()
        }));
    }
    let mut outputs = Vec::new();
    for thread in threads {
        outputs.push(thread.join().unwrap());
    }
    for output in &outputs {
        assert!(output.contains("a.css"), "{output}");
        assert!(!output.contains("pagespeed"), "{output}");
    }
    // One shared execution: three submitters, two deduped onto it.
    // (Per-thread servers own the schedule counter; the pool-level
    // submitted/dedup counters below are the shared witnesses.)
    assert_eq!(
        statistics
            .get_variable("background_jobs_submitted")
            .unwrap()
            .get(),
        3
    );
    assert_eq!(
        statistics
            .get_variable("background_jobs_dedup_hits")
            .unwrap()
            .get(),
        2
    );
    // The single completion warms every later render to a HIT.
    let deadline = Instant::now() + Duration::from_secs(10);
    while statistics
        .get_variable("background_jobs_completed")
        .unwrap()
        .get()
        < 1
    {
        assert!(Instant::now() < deadline, "background rewrite stalled");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fetcher.count.load(Ordering::SeqCst), 1);
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::clone(&http_backend),
        Arc::clone(&metadata_backend),
        Arc::clone(&fetcher) as Arc<dyn AsyncFetcher>,
        START_MS,
    ));
    server.set_background_pool(Arc::clone(&pool));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options.borrow_mut().enable_filter(Filter::RewriteCss);
    // Same effective options as the racing renders (partition keys
    // separate by full signature, so the deadline must match too).
    options
        .borrow_mut()
        .set_option("RewriteDeadlinePerFlushMs", "5000", Scope::Server)
        .unwrap();
    let resource_driver = Rc::new(ResourceDriver::new(
        server,
        options.clone(),
        "http://test.com/page.html",
    ));
    let ctx = ChainContext::new(true, true);
    let sink = SharedSink::default();
    let mut driver = Driver::new(&options.borrow(), &ctx, sink.clone(), Some(resource_driver));
    driver.start_parse(b"http://test.com/page.html", false);
    driver.write(
        format!("<head><link rel='stylesheet' href='{CSS_URL}'></head><body></body>").as_bytes(),
    );
    driver.finish();
    let warm = String::from_utf8(sink.bytes()).unwrap();
    assert!(warm.contains(".pagespeed."), "{warm}");
    assert_eq!(fetcher.count.load(Ordering::SeqCst), 1);
    pool.shutdown();
}

/// Parks a 1-thread/1-queue pool: the returned sender releases both
/// gate jobs when dropped. Job A runs (started-signal proves the
/// worker is inside), B fills the queue; every later submit sheds.
fn saturate(pool: &Arc<BackgroundPool>) -> std::sync::mpsc::Sender<()> {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
    let gate_rx = Arc::new(std::sync::Mutex::new(gate_rx));
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    {
        let gate_rx = Arc::clone(&gate_rx);
        pool.submit("shed-gate-a", move || {
            let _ = started_tx.send(());
            let _ = gate_rx.lock().unwrap().recv();
            0u8
        })
        .unwrap();
    }
    started_rx.recv().unwrap();
    {
        let gate_rx = Arc::clone(&gate_rx);
        pool.submit("shed-gate-b", move || {
            let _ = gate_rx.lock().unwrap().recv();
            0u8
        })
        .unwrap();
    }
    gate_tx
}

#[test]
fn queue_full_load_sheds_without_waiting() {
    // Resource level: a saturated pool sheds the load (pending,
    // fallback) instead of fetching inline past the deadline.
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options
        .borrow_mut()
        .set_option("RewriteDeadlinePerFlushMs", "5000", Scope::Server)
        .unwrap();
    let fetcher = Arc::new(SlowFetcher::new(Duration::from_millis(300)));
    fetcher.script_css(CSS_URL, CSS_BODY, START_MS);
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        fetcher.clone() as Arc<dyn AsyncFetcher>,
        START_MS,
    ));
    let pool = BackgroundPool::new(
        PoolConfig {
            threads: 1,
            queue_bound: 1,
        },
        &server.statistics_shared(),
    );
    server.set_background_pool(Arc::clone(&pool));
    let gate = saturate(&pool);
    let driver = Rc::new(ResourceDriver::new(
        server.clone(),
        options,
        "http://test.com/page.html",
    ));
    let mut resource = CacheableResourceBase::<TestHooks>::new(
        driver.clone(),
        "test",
        CSS_URL,
        CSS_URL,
        None,
        TestHooks,
    );
    let start = Instant::now();
    let mut callback = MockLoadCallback::new();
    let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
    resource.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "shed load must not run the slow fetch inline"
    );
    assert!(resource.load_pending());
    assert_eq!(fetcher.count.load(Ordering::SeqCst), 0);
    let queued = server
        .statistics()
        .get_variable("background_jobs_queue_full")
        .unwrap()
        .get();
    assert_eq!(queued, 1);
    // Releasing the pool lets a retry fetch and link normally.
    drop(gate);
    let deadline = Instant::now() + Duration::from_secs(10);
    while server
        .statistics()
        .get_variable("background_jobs_completed")
        .unwrap()
        .get()
        < 2
    {
        assert!(Instant::now() < deadline, "gate jobs stalled");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut retry =
        CacheableResourceBase::<TestHooks>::new(driver, "test", CSS_URL, CSS_URL, None, TestHooks);
    // Fresh resource handle: the shed attempt concluded nothing.
    let mut callback = MockLoadCallback::new();
    let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
    retry.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(retry.contents(), CSS_BODY);
    pool.shutdown();
}

#[test]
fn saturated_pool_serves_fallback_html_fast_then_recovers() {
    let world = World::new(300, 30, 1, 1);
    let gate = saturate(&world.pool);
    // Every render sheds its rewrite submit: original links served
    // fast (far under the 300ms origin), queue-full counted, and the
    // origin never touched inline.
    for page in 0..3 {
        let url = format!("http://test.com/shed{page}.css");
        world.fetcher.script_css(&url, CSS_BODY, START_MS);
        let start = Instant::now();
        let html = world.drive(&url);
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "saturated render must shed, not fetch inline"
        );
        assert!(html.contains(&format!("shed{page}.css")), "{html}");
        assert!(!html.contains("pagespeed"), "{html}");
    }
    assert_eq!(world.stat("background_jobs_queue_full"), 3);
    assert_eq!(world.stat("rewrite_cached_output_misses"), 3);
    assert_eq!(world.stat("rewrite_cached_output_missed_deadline"), 0);
    // Shed submits never schedule (the counter stays uncreated).
    assert!(world
        .server
        .statistics()
        .get_variable("html_async_rewrite_scheduled")
        .is_none());
    assert_eq!(world.fetcher.count.load(Ordering::SeqCst), 0);
    // Draining the pool recovers the full progression: the next
    // render schedules (fire-and-forget, still original links), the
    // background job optimizes and writes the partition, and the
    // render after HITs it.
    drop(gate);
    let url = "http://test.com/shed0.css";
    let shed = world.drive(url);
    assert!(!shed.contains("pagespeed"), "{shed}");
    assert_eq!(world.stat("html_async_rewrite_scheduled"), 1);
    world.await_completions(3);
    let recovered = world.drive(url);
    assert!(recovered.contains(".pagespeed."), "{recovered}");
}

#[test]
fn font_service_miss_defers_and_warm_hit_keeps_ua_hooks() {
    let world = World::new(300, 10, 2, 16);
    let url = "http://fonts.googleapis.com/css?family=Roboto";
    world
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineGoogleFontCss);
    world.fetcher.script_css(url, CSS_BODY, START_MS);
    let start = Instant::now();
    let first = world.drive(url);
    assert!(start.elapsed() < Duration::from_millis(250));
    assert!(first.contains(url), "{first}");
    world.await_completions(1);
    let second = world.drive(url);
    assert!(second.contains("<style"), "{second}");
    assert!(!second.contains("<link"), "{second}");
    let fetches = world.fetcher.count.load(Ordering::SeqCst);
    assert_eq!(second, world.drive(url));
    assert_eq!(fetches, world.fetcher.count.load(Ordering::SeqCst));
}

#[test]
fn combinations_defer_then_match_sync_without_refetching() {
    for (filter, markup, body) in [
        (Filter::CombineCss, "<head><link rel='stylesheet' href='a.css'><link rel='stylesheet' href='b.css'></head><body></body>", CSS_BODY),
        (Filter::CombineJavascript, "<head></head><body><script src='a.css'></script><script src='b.css'></script></body>", b"var x = 1;".as_slice()),
    ] {
        let world = World::new(100, 10, 2, 16);
        *world.options.borrow_mut() = RewriteOptions::new();
        world.options.borrow_mut().filters_mut().set_level(ps_config::filter::RewriteLevel::PassThrough);
        world.options.borrow_mut().enable_filter(filter);
        world.fetcher.script_css(CSS_URL, body, START_MS);
        world.fetcher.script_css("http://test.com/b.css", body, START_MS);
        let start = Instant::now();
        let first = world.drive_markup(markup);
        assert!(start.elapsed() < Duration::from_millis(150));
        assert!(!first.contains(".pagespeed."), "{first}");
        world.await_completions(1);
        assert_eq!(world.stat("html_async_rewrite_completed"), 1);
        let second = world.drive_markup(markup);
        assert!(second.contains(".pagespeed."), "{second}");
        let calls = world.fetcher.count.load(Ordering::SeqCst);
        let reads = world.http_reads.load(Ordering::SeqCst);
        assert_eq!(second, world.drive_markup(markup));
        assert_eq!(calls, world.fetcher.count.load(Ordering::SeqCst));
        assert_eq!(reads, world.http_reads.load(Ordering::SeqCst), "metadata HIT must not read resource bytes from the HTTP cache");
        let sync_server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()), Arc::new(MemoryCache::new()), world.fetcher.clone(), START_MS));
        assert_eq!(second, world.drive_markup_on(sync_server, markup));
        // Expiration invalidates the combination, preserving original URLs
        // while a new background job refreshes it.
        world.server.timer().borrow_mut().set_ms(START_MS + 400_000);
        let expired = world.drive_markup(markup);
        assert!(!expired.contains(".pagespeed."), "{expired}");
    }
}

#[test]
fn failed_candidate_does_not_discard_successful_combinations() {
    let world = World::new(0, 10, 2, 16);
    *world.options.borrow_mut() = RewriteOptions::new();
    world.options.borrow_mut().enable_filter(Filter::CombineCss);
    world
        .fetcher
        .script_css("http://test.com/b.css", CSS_BODY, START_MS);
    let html = "<head><link rel='stylesheet' href='a.css'><link rel='stylesheet' href='b.css'><link rel='stylesheet' href='missing.css'></head><body></body>";
    world.drive_markup(html);
    world.await_completions(1);
    let second = world.drive_markup(html);
    assert!(second.contains(".pagespeed.cc."), "{second}");
    assert!(second.contains("missing.css"), "{second}");
    let calls = world.fetcher.count.load(Ordering::SeqCst);
    assert_eq!(second, world.drive_markup(html));
    assert_eq!(calls, world.fetcher.count.load(Ordering::SeqCst));
    let sync = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        world.fetcher.clone(),
        START_MS,
    ));
    assert_eq!(second, world.drive_markup_on(sync, html));
}

#[test]
fn image_background_completion_warms_bytes_before_publishing_url() {
    use ps_html::filters::image_rewrite::ImageRewriteFilter;
    let world = World::new(300, 30, 1, 8);
    world.options.borrow_mut().enable_filter(Filter::RecompressJpeg);
    let url = "http://test.com/Puzzle.jpg";
    let image = include_bytes!("data/Puzzle.jpg");
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    headers.set_first_line(1, 1, 200, "OK");
    headers
        .headers_mut()
        .add(header::CONTENT_TYPE, "image/jpeg");
    headers.set_date_and_caching(START_MS, 300_000, "");
    headers.compute_caching();
    world.fetcher.inner.set_response(url, headers, image);
    let html = format!("<html><body><img src='{url}'></body></html>");
    let start = Instant::now();
    let first = world.drive_markup(&html);
    let first_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "HTML waited for image fetch"
    );
    assert!(first.contains(url), "{first}");
    world.await_completions(1);
    let warm = world.drive_markup(&html);
    let rewritten = warm
        .split("src='")
        .nth(1)
        .expect("image src")
        .split('\'')
        .next()
        .unwrap();
    assert!(rewritten.contains(".pagespeed.ic."), "{warm}");
    let rewrites = world.stat("image_rewrites");
    let origin_calls = world.fetcher.count.load(Ordering::SeqCst);
    let driver = Rc::new(ResourceDriver::new(
        world.server.clone(),
        world.options.clone(),
        rewritten,
    ));
    driver.add_filter(Rc::new(ImageRewriteFilter::new(driver.clone())));
    let mut response = BufferingFetch::new(rewritten, driver.request_context().borrow().clone());
    let serve_start = Instant::now();
    assert!(driver.fetch_resource(rewritten, &mut response));
    assert!(response.done() && response.success());
    assert!(!response.body().is_empty());
    eprintln!("image HTML fallback {first_ms:.3} ms; ready {:.3} ms; first optimized serve {:.3} ms; optimized bytes {}", start.elapsed().as_secs_f64()*1000.0, serve_start.elapsed().as_secs_f64()*1000.0, response.body().len());
    assert!(
        !rewritten.contains(".ic.0."),
        "published URL must have completed bytes: {warm}"
    );
    assert_eq!(
        rewrites,
        world.stat("image_rewrites"),
        "first image serve re-optimized"
    );
    assert_eq!(
        origin_calls,
        world.fetcher.count.load(Ordering::SeqCst),
        "first image serve fetched origin"
    );

}
