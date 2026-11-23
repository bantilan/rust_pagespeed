//! Background fetch tests: deadline waits, background completion,
//! cross-request reuse, duplicate suppression, failures, shutdown.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::background::{BackgroundPool, PoolConfig};
use ps_html::cacheable_resource::{
    CacheableResourceBase, LoadCallback, MockLoadCallback, NotCacheablePolicy, ResourceHooks,
};
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::request::RequestContext;
use ps_http::response::{HttpOptions, ResponseHeaders};

const START_MS: i64 = 1_700_000_000_000;
const TEST_URL: &str = "http://test.com/a.css";

/// Hooks without behavior (fetch plumbing only).
#[derive(Clone, Default)]
struct TestHooks;

impl ResourceHooks for TestHooks {}

type TestResource = CacheableResourceBase<TestHooks>;

/// Scripted fetcher with an overt latency + call count.
struct SleepFetcher {
    delay: Duration,
    count: AtomicU64,
    inner: MockUrlFetcher,
}

impl SleepFetcher {
    fn new(delay: Duration) -> Self {
        SleepFetcher {
            delay,
            count: AtomicU64::new(0),
            inner: MockUrlFetcher::new(),
        }
    }
}

impl AsyncFetcher for SleepFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        self.count.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        self.inner.fetch(url, fetch);
    }
}

struct Fixture {
    server: Rc<ServerContext>,
    driver: Rc<ResourceDriver>,
    fetcher: Arc<SleepFetcher>,
    options: Rc<RefCell<RewriteOptions>>,
    pool: Arc<BackgroundPool>,
}

impl Fixture {
    /// Server with a slow scripted fetcher, a pool, and a deadline.
    fn new(fetch_delay: Duration, deadline_ms: i64) -> Self {
        let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let fetcher = Arc::new(SleepFetcher::new(fetch_delay));
        let server = Rc::new(ServerContext::new(
            backend,
            fetcher.clone() as Arc<dyn AsyncFetcher>,
            START_MS,
        ));
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .set_option(
                "RewriteDeadlinePerFlushMs",
                &deadline_ms.to_string(),
                Scope::Server,
            )
            .unwrap();
        let pool = BackgroundPool::new(
            PoolConfig {
                threads: 2,
                queue_bound: 16,
            },
            &server.statistics_shared(),
        );
        server.set_background_pool(Arc::clone(&pool));
        let driver = Rc::new(ResourceDriver::new(
            server.clone(),
            options.clone(),
            "http://test.com/page.html",
        ));
        Fixture {
            server,
            driver,
            fetcher,
            options,
            pool,
        }
    }

    fn resource(&self) -> TestResource {
        TestResource::new(
            self.driver.clone(),
            "test",
            TEST_URL,
            TEST_URL,
            None,
            TestHooks,
        )
    }

    fn script_css(&self, body: &[u8]) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.set_date_and_caching(self.server.now_ms(), 300_000, "");
        headers.compute_caching();
        self.fetcher.inner.set_response(TEST_URL, headers, body);
    }

    fn load(&self, resource: &mut TestResource) -> bool {
        let mut callback = MockLoadCallback::new();
        let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
        resource.load(
            NotCacheablePolicy::LoadEvenIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        assert!(callback.done());
        callback.success()
    }

    /// Waits (bounded) for a background completion.
    fn await_completion(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.stat("background_jobs_completed") < 1 {
            assert!(
                Instant::now() < deadline,
                "background fetch never completed"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn stat(&self, name: &str) -> i64 {
        self.server
            .statistics()
            .get_variable(name)
            .unwrap_or_else(|| panic!("missing stat {name}"))
            .get()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.pool.shutdown();
    }
}

#[test]
fn fast_fetch_completes_inline_through_pool() {
    let fixture = Fixture::new(Duration::from_millis(0), 1000);
    fixture.script_css(b"a{color:red}");
    let mut resource = fixture.resource();
    assert!(fixture.load(&mut resource));
    assert_eq!(resource.contents(), b"a{color:red}");
    assert_eq!(fixture.stat("background_jobs_submitted"), 1);
    assert_eq!(fixture.stat("background_jobs_completed"), 1);
    assert_eq!(fixture.fetcher.count.load(Ordering::SeqCst), 1);
}

#[test]
fn slow_fetch_times_out_then_warms_cache() {
    let fixture = Fixture::new(Duration::from_millis(300), 20);
    fixture.script_css(b"a{color:red}");
    // First request: bounded wait, then the existing failure path
    // (callers render originals) — far faster than the fetch.
    let mut first = fixture.resource();
    let start = Instant::now();
    assert!(!fixture.load(&mut first));
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "first request must not block on the fetch"
    );
    assert_eq!(fixture.stat("background_jobs_timeouts"), 1);
    // Background completion warms the cache: the next request hits
    // without fetching again.
    fixture.await_completion();
    let mut second = fixture.resource();
    assert!(fixture.load(&mut second));
    assert_eq!(second.contents(), b"a{color:red}");
    assert_eq!(fixture.fetcher.count.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_loads_share_one_fetch() {
    use ps_stats::core::Statistics;

    // One slow fetcher, one pool, one backend: three threads each
    // build their own (!Send) server/driver/resource stack and load
    // the same URL at once. Dedup must collapse the triple miss
    // into a single fetch, and every waiter succeeds.
    let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let fetcher = Arc::new(SleepFetcher::new(Duration::from_millis(200)));
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    headers.set_first_line(1, 1, 200, "OK");
    headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
    headers.set_date_and_caching(START_MS, 300_000, "");
    headers.compute_caching();
    fetcher.inner.set_response(TEST_URL, headers, b"shared");
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
        let (backend, fetcher, pool, barrier) = (
            Arc::clone(&backend),
            Arc::clone(&fetcher),
            Arc::clone(&pool),
            Arc::clone(&barrier),
        );
        threads.push(std::thread::spawn(move || {
            let server = Rc::new(ServerContext::new(
                backend,
                fetcher as Arc<dyn ps_http::fetch::AsyncFetcher>,
                START_MS,
            ));
            server.set_background_pool(Arc::clone(&pool));
            let options = Rc::new(RefCell::new(RewriteOptions::new()));
            options
                .borrow_mut()
                .set_option("RewriteDeadlinePerFlushMs", "5000", Scope::Server)
                .unwrap();
            let driver = Rc::new(ResourceDriver::new(
                server,
                options,
                "http://test.com/page.html",
            ));
            let mut resource = CacheableResourceBase::<TestHooks>::new(
                driver, "test", TEST_URL, TEST_URL, None, TestHooks,
            );
            barrier.wait();
            let mut callback = MockLoadCallback::new();
            let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
            resource.load(
                NotCacheablePolicy::LoadEvenIfNotCacheable,
                &mut ctx,
                &mut callback,
            );
            assert!(callback.done());
            assert!(callback.success());
            assert_eq!(resource.contents(), b"shared");
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(fetcher.count.load(Ordering::SeqCst), 1);
    pool.shutdown();
}

#[test]
fn failed_fetch_preserves_fallback() {
    let fixture = Fixture::new(Duration::from_millis(0), 1000);
    // Nothing scripted: the fetch fails fast through the pool.
    let mut resource = fixture.resource();
    assert!(!fixture.load(&mut resource));
    assert_eq!(fixture.stat("background_jobs_completed"), 1);
    // Failure memory still works (no refetch on immediate retry).
    let mut retry = fixture.resource();
    assert!(!fixture.load(&mut retry));
    assert_eq!(fixture.fetcher.count.load(Ordering::SeqCst), 1);
}

#[test]
fn exhausted_budget_skips_waiting() {
    let fixture = Fixture::new(Duration::from_millis(300), 30);
    fixture.script_css(b"a{}");
    // First load consumes the whole window budget waiting.
    let mut first = fixture.resource();
    assert!(!fixture.load(&mut first));
    // Second load finds no budget left: immediate fallback.
    let mut second = fixture.resource();
    let start = Instant::now();
    assert!(!fixture.load(&mut second));
    assert!(
        start.elapsed() < Duration::from_millis(100),
        "exhausted budget must not wait"
    );
    fixture.await_completion();
}

#[test]
fn ipro_scope_selects_ipro_deadline() {
    let fixture = Fixture::new(Duration::from_millis(0), 50);
    fixture
        .options
        .borrow_mut()
        .set_in_place_rewrite_deadline_ms(7);
    // Outside IPRO serving: the flush deadline governs.
    assert_eq!(fixture.driver.fetch_wait_budget_ms(), Some(50));
    // In scope but not waiting: still the flush deadline.
    fixture.driver.reset_fetch_budget();
    {
        let _scope = fixture.driver.enter_ipro_scope();
        assert_eq!(fixture.driver.fetch_wait_budget_ms(), Some(50));
    }
    // In scope and waiting: the IPRO deadline overrides.
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.driver.reset_fetch_budget();
    {
        let _scope = fixture.driver.enter_ipro_scope();
        assert_eq!(fixture.driver.fetch_wait_budget_ms(), Some(7));
    }
    // Leaving the scope restores the flush deadline.
    fixture.driver.reset_fetch_budget();
    assert_eq!(fixture.driver.fetch_wait_budget_ms(), Some(50));
    // A non-positive IPRO deadline disables the wait in scope.
    fixture
        .options
        .borrow_mut()
        .set_in_place_rewrite_deadline_ms(0);
    fixture.driver.reset_fetch_budget();
    {
        let _scope = fixture.driver.enter_ipro_scope();
        assert_eq!(fixture.driver.fetch_wait_budget_ms(), None);
    }
}

#[test]
fn shutdown_keeps_requests_working() {
    let fixture = Fixture::new(Duration::from_millis(0), 1000);
    fixture.script_css(b"a{}");
    fixture.pool.shutdown();
    // Post-shutdown loads run inline (degraded, never failed).
    let mut resource = fixture.resource();
    assert!(fixture.load(&mut resource));
    assert_eq!(resource.contents(), b"a{}");
    assert_eq!(fixture.stat("background_jobs_shutdown"), 1);
}
