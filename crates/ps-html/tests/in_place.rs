//! In-place rewrite tests ported from
//! `in_place_rewrite_context_test.cc`.
//!
//! NOTE: cache op-count assertions are adapted: origin-fetch
//! deltas, rewrite counts, and response bytes/headers/TTLs/etags
//! port exactly, but `HTTPCache`/`LRUCache` op tallies have no
//! counterpart (split backends here vs C++'s shared LRU).
//! NOTE: `wait_for_optimize_timeout` ports as
//! `wait_for_optimized_slow_origin_falls_back_then_warms` (the
//! IPRO deadline bounds origin fetches; optimization compute
//! itself stays inline).
//! `mid_stream_fetch_failure_*` (async failure staging) and
//! `load_from_file` (file input resources) are deferred to those
//! subsystems. Stale/freshen phases port via sync revalidation
//! (the mock clock stands in for `set_timer`). NOTE:
//! `set_split_writes` is sync-inert and omitted.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ps_cache::cache::{Cache, MemoryCache};
use ps_config::filter::Filter;
use ps_config::options::{AllowVaryOn, RewriteOptions};
use ps_config::scope::Scope;
use ps_html::background::{BackgroundPool, PoolConfig};
use ps_html::fake_filter::{FakeFilter, FakeImageFilter};
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::content_type::ContentType;
use ps_http::fetch::{AsyncFetch, AsyncFetcher, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::RequestHeaders;
use ps_http::response::{HttpOptions, ResponseHeaders};

const START_MS: i64 = 1_700_000_000_000;
const TTL_MS: i64 = 3_600_000;
/// Injected s-maxage window (`kInjectedSMaxAgeMs`).
const INJECTED_S_MAXAGE_MS: i64 = 10_000;
/// Rewritten etag (`W/"PSA-aj-0"`).
const ETAG_AJ: &str = "W/\"PSA-aj-0\"";
/// Synthesized original etag (`kEtag0`).
const ETAG_0: &str = "W/\"PSA-0\"";

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Arc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    img_filter: Rc<FakeImageFilter>,
    js_filter: Rc<FakeFilter>,
    css_filter: Rc<FakeFilter>,
    request_headers: RefCell<RequestHeaders>,
    user_agent: RefCell<String>,
    response_headers: RefCell<ResponseHeaders>,
    fetch_base: Cell<u64>,
    shards: RefCell<Vec<(String, String)>>,
}

/// Origin-latency wrapper: scripting and counts stay on the
/// inner mock; only the served latency changes.
struct LatencyFetcher {
    inner: Arc<MockUrlFetcher>,
    delay: Duration,
}

impl AsyncFetcher for LatencyFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.inner.fetch(url, fetch);
    }
}

impl Fixture {
    fn new() -> Self {
        Self::build(false)
    }

    /// Builds a fixture with the real MD5 hasher (`UseMd5Hasher`).
    fn with_md5() -> Self {
        Self::build(true)
    }

    fn build(md5: bool) -> Self {
        Self::build_with_origin_latency(md5, Duration::ZERO)
    }

    /// Builds a fixture whose origin answers after `latency`
    /// (deadline tests; the mock keeps scripting and counts).
    fn build_with_origin_latency(md5: bool, latency: Duration) -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Arc::new(MockUrlFetcher::new());
        let server_fetcher: Arc<dyn AsyncFetcher> = if latency.is_zero() {
            fetcher.clone()
        } else {
            Arc::new(LatencyFetcher {
                inner: fetcher.clone(),
                delay: latency,
            })
        };
        let mut server = ServerContext::new_with_metadata(
            http_backend.clone(),
            metadata_backend.clone(),
            server_fetcher,
            START_MS,
        );
        if !md5 {
            server.set_hasher(Arc::new(ps_cache::hash::MockHasher::new()));
        }
        Fixture {
            server: Rc::new(server),
            options: Rc::new(RefCell::new(RewriteOptions::new())),
            fetcher,
            http_backend,
            metadata_backend,
            img_filter: Rc::new(FakeImageFilter::new()),
            js_filter: Rc::new(FakeFilter::new("jm")),
            css_filter: Rc::new(FakeFilter::new("cf")),
            request_headers: RefCell::new(RequestHeaders::new()),
            user_agent: RefCell::new(String::new()),
            response_headers: RefCell::new(ResponseHeaders::new(HttpOptions::default_for_tests())),
            fetch_base: Cell::new(0),
            shards: RefCell::new(Vec::new()),
        }
    }

    /// Scripts the shared responses and servants (`Init`).
    fn init(&self) {
        self.add_response(
            "http://www.example.com/cacheable.html",
            "text/html",
            b"good",
            TTL_MS,
            "original_etag",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable.jpg",
            "image/jpeg",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable_jpg",
            "image/jpeg",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/notransform.jpg",
            "image/jpeg",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            true,
        );
        self.add_response(
            "http://www.example.com/vary_star.jpg",
            "image/jpeg",
            b"good",
            TTL_MS,
            "",
            "*",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/vary_ua.jpg",
            "image/jpeg",
            b"good",
            TTL_MS,
            "",
            "User-Agent",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/vary_origin.jpg",
            "image/jpeg",
            b"good",
            TTL_MS,
            "",
            "Origin",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable.png",
            "image/png",
            b"good",
            TTL_MS,
            "original_etag",
            "",
            true,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable.gif",
            "image/gif",
            b"good",
            TTL_MS,
            "original_etag",
            "",
            true,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable.webp",
            "image/webp",
            b"good",
            TTL_MS,
            "original_etag",
            "",
            true,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable.js",
            "application/javascript",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable_js.jpg",
            "application/javascript",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable.css",
            "text/css",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/nocacheable.html",
            "text/html",
            b"bad",
            -1,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/nocacheable.js",
            "application/javascript",
            b"good",
            -1,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheablemod.js",
            "application/javascript",
            b"good",
            0,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable_js_type.json",
            "application/javascript",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable_json_type.json",
            "application/json",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        self.add_response(
            "http://www.example.com/cacheable_json_synonym_type.json",
            "application/x-json",
            b"good",
            TTL_MS,
            "",
            "",
            false,
            false,
        );
        let mut private_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        Self::set_default_headers("application/javascript", &mut private_headers);
        private_headers.set_date_and_caching(START_MS, 1200, ",private");
        self.fetcher.set_response(
            "http://www.example.com/privatecacheable.js",
            private_headers,
            b"good",
        );
        let mut bad_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        bad_headers.parse_first_line("HTTP/1.1 404 Not Found");
        bad_headers.set_date(START_MS);
        self.fetcher
            .set_response("http://www.example.com/bad.url", bad_headers, b"ugly");
        let mut redirect_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        redirect_headers.parse_first_line("HTTP/1.1 301 Moved Permanently");
        redirect_headers.compute_caching();
        redirect_headers.set_time_header("Expires", START_MS + 36000);
        redirect_headers
            .headers_mut()
            .replace("Cache-Control", "max-age=36");
        redirect_headers
            .headers_mut()
            .add("Cache-Control", "public");
        redirect_headers
            .headers_mut()
            .add("Content-Type", "image/jpeg");
        self.fetcher.set_response(
            "http://www.example.com/redir.url",
            redirect_headers,
            b"Location: http://www.example.com/final.url",
        );

        self.options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::RecompressJpeg);
        self.options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::RewriteJavascriptExternal);
        self.options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::RewriteJavascriptInline);
        self.options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::RewriteCss);
        self.options
            .borrow_mut()
            .set_in_place_rewriting_enabled(true);
        // Only allow to vary on "Accept" header.
        self.options
            .borrow_mut()
            .set_allow_vary_on(AllowVaryOn::parse("accept").expect("accept parses"));
        self.reset_stats();
    }

    /// Scripts one origin response (`AddResponse`).
    #[allow(clippy::too_many_arguments)]
    fn add_response(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        ttl_ms: i64,
        etag: &str,
        vary: &str,
        write_to_cache: bool,
        no_transform: bool,
    ) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        Self::set_default_headers(content_type, &mut headers);
        if ttl_ms > 0 {
            headers.set_date_and_caching(START_MS, ttl_ms, "");
        } else {
            headers.set_date(START_MS);
            if ttl_ms < 0 {
                headers.headers_mut().replace("Cache-Control", "no-cache");
            } else {
                headers.headers_mut().replace("Cache-Control", "public");
            }
        }
        if !vary.is_empty() {
            headers.headers_mut().replace("Vary", vary);
        }
        if no_transform {
            headers
                .headers_mut()
                .replace("Cache-Control", "no-transform");
        }
        if !etag.is_empty() {
            headers.headers_mut().add("Etag", etag);
        }
        self.fetcher.set_response(url, headers.clone(), body);
        if write_to_cache {
            headers.compute_caching();
            let mut value = ps_http::value::HttpValue::new();
            value.set_headers(headers);
            value.write_body(body);
            self.server.http_cache().put_value(
                url,
                "",
                ps_http::response::RequestProperties::default(),
                HttpOptions::default_for_tests(),
                &value,
                START_MS,
            );
        }
    }

    fn set_default_headers(content_type: &str, headers: &mut ResponseHeaders) {
        headers.parse_first_line("HTTP/1.1 200 OK");
        headers.headers_mut().replace("Content-Type", content_type);
    }

    /// Records a shard mapping, replayed onto each driver.
    fn add_shard(&self, domain: &str, shards: &str) {
        self.shards
            .borrow_mut()
            .push((domain.to_string(), shards.to_string()));
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let mut driver = ResourceDriver::new(self.server.clone(), self.options.clone(), url);
        for (domain, shards) in self.shards.borrow().iter() {
            assert!(driver.lawyer_mut().add_shard(domain, shards, None));
        }
        let driver = Rc::new(driver);
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver.add_filter(self.img_filter.clone());
        driver.add_filter(self.js_filter.clone());
        driver.add_filter(self.css_filter.clone());
        driver
    }

    /// Fetches and checks the response (`FetchAndCheckResponse`).
    /// Pass `expected_ttl` as `None` for 304s: C++ asserts the
    /// member-reuse stale TTL there, which has no counterpart
    /// without shared-mutable headers.
    fn fetch_and_check_response(
        &self,
        url: &str,
        expected_body: &[u8],
        expected_success: bool,
        expected_ttl: Option<i64>,
        etag: Option<&str>,
        date_ms: i64,
    ) {
        let driver = self.driver_for(url);
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        *fetch.request_headers_mut() = self.request_headers.borrow().clone();
        assert!(driver.fetch_resource(url, &mut fetch), "{url}");
        assert!(fetch.done(), "{url}");
        assert_eq!(fetch.success(), expected_success, "{url}");
        assert_eq!(fetch.body(), expected_body, "{url}");
        // `NotifyingFetch::HandleDone` computes caching on
        // completion; mirror it before asserting flags.
        fetch.response_headers_mut().compute_caching();
        self.response_headers
            .borrow_mut()
            .copy_from(fetch.response_headers());
        let headers = self.response_headers.borrow();
        if let Some(expected_ttl) = expected_ttl {
            assert_eq!(headers.cache_ttl_ms(), expected_ttl, "{url}");
        }
        assert_eq!(headers.headers().lookup1("Etag").as_deref(), etag, "{url}");
        assert_eq!(headers.date_ms(), date_ms, "{url}");
    }

    /// Clears member state between fetches
    /// (`ResetHeadersAndStats`).
    fn reset_headers_and_stats(&self) {
        self.response_headers.borrow_mut().clear();
        self.img_filter.clear_stats();
        self.js_filter.clear_stats();
        self.css_filter.clear_stats();
        *self.request_headers.borrow_mut() = RequestHeaders::new();
        self.fetch_base.set(self.fetcher.fetch_count());
        self.server.http_cache().clear_stats();
    }

    /// Origin fetches since the last reset.
    fn fetch_delta(&self) -> u64 {
        self.fetcher.fetch_count() - self.fetch_base.get()
    }

    /// Resets all counters (`ClearStats` after `Init`).
    fn reset_stats(&self) {
        self.img_filter.clear_stats();
        self.js_filter.clear_stats();
        self.css_filter.clear_stats();
        self.fetch_base.set(self.fetcher.fetch_count());
        self.server.http_cache().clear_stats();
    }

    /// Moves the clock to an absolute time (`SetTimeMs`).
    fn set_time_ms(&self, time_ms: i64) {
        let timer = self.server.timer();
        let now = timer.borrow().now_ms();
        timer.borrow_mut().advance_ms(time_ms - now);
        self.sync_mock_clock();
    }

    /// Advances the clock (`AdvanceTimeMs`).
    fn advance_time_ms(&self, delta_ms: i64) {
        self.server.timer().borrow_mut().advance_ms(delta_ms);
        self.sync_mock_clock();
    }

    /// Shares the mock clock with the fetcher (`set_timer`).
    fn sync_mock_clock(&self) {
        let now = self.server.now_ms();
        self.fetcher.set_now_ms(Some(now));
    }

    /// Installs the UA (`ResetUserAgent` + `SetCurrentUserAgent`).
    fn reset_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
    }

    /// Stages a request header (`AddRequestAttribute`).
    fn add_request_attribute(&self, name: &str, value: &str) {
        self.request_headers
            .borrow_mut()
            .headers_mut()
            .add(name, value);
    }

    /// Warm-cache invariants (`CheckWarmCache`, adapted): no
    /// origin fetch, no rewrites.
    fn check_warm_cache(&self, id: &str) {
        assert_eq!(self.fetch_delta(), 0, "{id}");
        assert_eq!(self.img_filter.num_rewrites(), 0, "{id}");
        assert_eq!(self.js_filter.num_rewrites(), 0, "{id}");
        assert_eq!(self.css_filter.num_rewrites(), 0, "{id}");
        assert_eq!(
            self.server
                .variable(ps_html::ipro::IN_PLACE_OVERSIZED_OPT_STREAM)
                .get(),
            0,
            "{id}"
        );
    }
}

#[test]
fn cacheable_html_url_no_rewriting() {
    let fixture = Fixture::new();
    fixture.init();
    // All these entries find no in-place rewrite metadata and no
    // rewriting happens.
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.html",
        b"good",
        true,
        Some(TTL_MS),
        Some("original_etag"),
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.html",
        b"good",
        true,
        Some(TTL_MS),
        Some("original_etag"),
        START_MS,
    );
    fixture.check_warm_cache("second_html");

    fixture.advance_time_ms(2 * TTL_MS);
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.html",
        b"good",
        true,
        Some(TTL_MS),
        Some("original_etag"),
        START_MS + 2 * TTL_MS,
    );
    // Cache entry is stale, so we must fetch again.
    assert_eq!(fixture.fetch_delta(), 1);
}

#[test]
fn wait_for_optimized_first_request() {
    // By setting this flag we should get an optimized response
    // on the first request unless we hit a rewrite timeout but
    // in this test it will complete in time.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();

    // The optimized content from the fake rewriter has ":ic"
    // appended to original content.
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 1);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_OVERSIZED_OPT_STREAM)
            .get(),
        0
    );

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    // Second fetch hits the metadata cache and the rewritten
    // resource is served out.
    fixture.check_warm_cache("second_fetch_2");
}

#[test]
fn wait_for_optimized_slow_origin_falls_back_then_warms() {
    // `wait_for_optimize_timeout` behavior: with waiting enabled, a
    // slow origin fails fast at the IPRO deadline (the flush
    // deadline stays huge, proving the override selects the IPRO
    // value end to end); the background completion then warms the
    // origin entry so the next request serves optimized bytes.
    let fixture = Fixture::build_with_origin_latency(false, Duration::from_millis(300));
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .set_in_place_rewrite_deadline_ms(30);
    fixture
        .options
        .borrow_mut()
        .set_option("RewriteDeadlinePerFlushMs", "30000", Scope::Server)
        .unwrap();
    fixture.init();
    let pool = BackgroundPool::new(
        PoolConfig {
            threads: 2,
            queue_bound: 16,
        },
        &fixture.server.statistics_shared(),
    );
    fixture.server.set_background_pool(Arc::clone(&pool));

    // First request: bounded wait, failed fetch (the server falls
    // back to its native method for the origin bytes).
    let start = Instant::now();
    let driver = fixture.driver_for("http://www.example.com/cacheable.jpg");
    let ctx = driver.request_context().borrow().clone();
    let mut fetch = BufferingFetch::new("http://www.example.com/cacheable.jpg", ctx);
    assert!(driver.fetch_resource("http://www.example.com/cacheable.jpg", &mut fetch));
    assert!(fetch.done());
    assert!(!fetch.success());
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "first request must fail fast at the IPRO deadline"
    );

    // Background completion warms the origin entry; the next
    // request rewrites and serves optimized bytes, fetching the
    // origin exactly once overall.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let completed = fixture
            .server
            .statistics()
            .get_variable("background_jobs_completed")
            .unwrap()
            .get();
        if completed >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "background fetch never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    pool.shutdown();
}

#[test]
fn wait_for_optimize_with_disabled_filter() {
    // Wait for optimized but if the resource fails to optimize
    // we should get back the original resource.
    let fixture = Fixture::with_md5();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();

    // Turn off optimization. The filter will still run but
    // return false in rewrite.
    fixture.img_filter.set_enabled(false);
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good",
        true,
        Some(TTL_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_OVERSIZED_OPT_STREAM)
            .get(),
        0
    );

    fixture.reset_headers_and_stats();
    // The second time we get the cached original, which should
    // have an md5'd etag.
    let expected_etag = format!("W/\"PSA-{}\"", fixture.server.hasher().hash(b"good"));
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good",
        true,
        Some(TTL_MS),
        Some(&expected_etag),
        START_MS,
    );
    // Second fetch hits the metadata cache, sees that the
    // rewrite failed and fetches and serves the original
    // resource from cache.
    fixture.check_warm_cache("second_fetch_3");
}

#[test]
fn wait_for_optimize_no_transform() {
    // Confirm that when cache-control:no-transform is present in
    // the response headers that the in-place optimizer does not
    // optimize the resource.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();

    // Don't rewrite since it's no-transform.
    fixture.fetch_and_check_response(
        "http://www.example.com/notransform.jpg",
        b"good",
        true,
        Some(TTL_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert!(fixture
        .response_headers
        .borrow()
        .headers()
        .has_value("Cache-Control", "no-transform"));

    fixture.reset_headers_and_stats();
    // Don't rewrite since it's no-transform.
    fixture.fetch_and_check_response(
        "http://www.example.com/notransform.jpg",
        b"good",
        true,
        Some(TTL_MS),
        Some(ETAG_0),
        START_MS,
    );
    // The second fetch should return the cached original after
    // seeing that it can't be rewritten.
    fixture.check_warm_cache("second_fetch_4");
}

#[test]
fn optimize_on_no_transform_if_option_false() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_disable_rewrite_on_no_transform(false);
    fixture.init();
    fixture.fetch_and_check_response(
        "http://www.example.com/notransform.jpg",
        b"good",
        true,
        Some(TTL_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 1);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        "http://www.example.com/notransform.jpg",
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    // Second fetch hits the metadata cache and the rewritten
    // resource is served out.
    fixture.check_warm_cache("second_fetch_notransform");
}

#[test]
fn wait_for_optimize_resource_too_big() {
    // Wait for optimized but if it's larger than the
    // RecordingFetch can handle make sure we piece together the
    // original resource properly. (Sync NOTE: the whole body is
    // observed at once, so split writes are omitted.)
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();

    // By setting cache max to 2, the response overflows.
    fixture.server.http_cache().set_max_body_bytes(2);
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good",
        true,
        Some(TTL_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_OVERSIZED_OPT_STREAM)
            .get(),
        1
    );

    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good",
        true,
        Some(TTL_MS),
        None,
        START_MS,
    );
    // Second fetch should also completely miss because the
    // first fetch was too big to stuff in the cache.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    // NOTE: the oversized stat is cumulative (no per-phase
    // statistics reset here), so it reads 2 after two misses.
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_OVERSIZED_OPT_STREAM)
            .get(),
        2
    );
}

#[test]
fn cacheable_jpg_url_rewriting_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    let url = "http://www.example.com/cacheable.jpg";
    // First serve streams the not-yet-optimized bytes.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 1);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    // Second fetch hits the metadata cache and the rewritten
    // resource is served out.
    fixture.check_warm_cache("second_fetch_6");

    fixture.reset_headers_and_stats();
    // We get a 304 if we send a request with an If-None-Match
    // matching the hash of the rewritten resource.
    fixture.add_request_attribute("If-None-Match", ETAG_AJ);
    fixture.fetch_and_check_response(url, b"", true, None, None, 0);
    assert_eq!(fixture.response_headers.borrow().status_code(), 304);
    // We hit the metadata cache and find that the etag matches
    // the hash of the rewritten resource.
    assert_eq!(fixture.fetch_delta(), 0);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    // The etag doesn't match and hence we serve the full
    // response.
    fixture.add_request_attribute("If-None-Match", "no-match");
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    assert_eq!(fixture.response_headers.borrow().status_code(), 200);
    // We hit the metadata cache, but the etag doesn't match so
    // we fetch the rewritten resource from the HTTPCache and
    // serve it out.
    fixture.check_warm_cache("etag_mismatch");

    // Delete the rewritten resource from cache to check if
    // reconstruction works.
    let rewritten = "http://www.example.com/cacheable.jpg.pagespeed.ic.0.jpg";
    let composite = fixture.server.http_cache().composite_key(rewritten, "");
    fixture.http_backend.delete(&composite);

    fixture.reset_headers_and_stats();
    // Original resource is served with the date set to start
    // time, with the injected s-maxage window. The ETag we
    // check for here is the ETag HTTPCache synthesized for the
    // original resource.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        Some(ETAG_0),
        START_MS,
    );
    // We find the metadata in cache, but don't find the
    // rewritten resource. Hence, we reconstruct the resource
    // and insert it into cache.
    assert_eq!(fixture.fetch_delta(), 0);
    assert_eq!(fixture.img_filter.num_rewrites(), 1);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    // For only the next request, update the date header so that
    // freshening succeeds.
    fixture.fetcher.set_update_date_headers(true);
    fixture.reset_headers_and_stats();
    let time_ms = START_MS + TTL_MS - 2 * 60_000;
    fixture.set_time_ms(time_ms);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(2 * 60_000),
        Some(ETAG_AJ),
        time_ms,
    );
    // This fetch hits the metadata cache and the rewritten
    // resource is served out. Freshening is triggered here and
    // we insert the freshened response and metadata into the
    // cache.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.fetcher.set_update_date_headers(false);

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS * 5 / 4);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS * 3 / 4 - 2 * 60_000),
        Some(ETAG_AJ),
        START_MS + TTL_MS * 5 / 4,
    );
    // Since the previous request freshened the metadata, this
    // fetch hits the metadata cache and the rewritten resource
    // is served out. Note that no freshening needs to be
    // triggered here.
    fixture.check_warm_cache("freshened_metadata");

    fixture.advance_time_ms(2 * TTL_MS);
    fixture.reset_headers_and_stats();
    // Serves the original bytes again, with the injected window.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        fixture.server.now_ms(),
    );
    // The metadata and cache entry is stale now. Fetch the
    // content and serve out the original. We will however
    // notice that the contents did not actually change and
    // update the metadata cache promptly, without rewriting.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

impl Fixture {
    /// Content-type/caching matrix (`CheckCachingAndContentType`).
    fn check_caching_and_content_type(
        &self,
        url: &str,
        expected_mime_type: &str,
        cache_body: &str,
        filter_prefix: &str,
    ) {
        // First serve streams the not-yet-optimized bytes with
        // the injected s-maxage window.
        self.fetch_and_check_response(
            url,
            cache_body.as_bytes(),
            true,
            Some(INJECTED_S_MAXAGE_MS),
            None,
            START_MS,
        );
        assert_eq!(self.fetch_delta(), 1);
        assert_eq!(self.img_filter.num_rewrites(), 0);
        assert_eq!(self.js_filter.num_rewrites(), 1);
        assert_eq!(self.css_filter.num_rewrites(), 0);

        // Make sure the content type is unmodified.
        assert_eq!(
            self.response_headers
                .borrow()
                .headers()
                .lookup1("Content-Type")
                .as_deref(),
            Some(expected_mime_type),
        );

        // Try a second fetch and ensure we get a cache hit.
        self.reset_headers_and_stats();
        self.fetch_and_check_response(
            url,
            format!("{cache_body}:{filter_prefix}").as_bytes(),
            true,
            Some(TTL_MS),
            Some(ETAG_AJ),
            START_MS,
        );
        assert_eq!(self.fetch_delta(), 0);
        assert_eq!(self.img_filter.num_rewrites(), 0);
        assert_eq!(self.js_filter.num_rewrites(), 0);
        assert_eq!(self.css_filter.num_rewrites(), 0);
        assert_eq!(
            self.response_headers
                .borrow()
                .headers()
                .lookup1("Content-Type")
                .as_deref(),
            Some(expected_mime_type),
        );
    }

    /// Image success flow (`ExpectInPlaceImageSuccessFlow`).
    fn expect_in_place_image_success_flow(&self, url: &str) {
        // The first serve streams the not-yet-optimized bytes
        // with the injected s-maxage window.
        self.fetch_and_check_response(
            url,
            b"good",
            true,
            Some(INJECTED_S_MAXAGE_MS),
            Some("original_etag"),
            START_MS,
        );
        // First fetch misses initial metadata cache lookup,
        // finds original in cache; the resource gets rewritten
        // and the rewritten resource gets inserted into cache.
        assert_eq!(self.fetch_delta(), 0, "{url}");
        assert_eq!(self.img_filter.num_rewrites(), 1, "{url}");
        assert_eq!(self.js_filter.num_rewrites(), 0, "{url}");
        assert_eq!(self.css_filter.num_rewrites(), 0, "{url}");

        self.reset_headers_and_stats();
        self.set_time_ms(START_MS + TTL_MS / 2);
        self.fetch_and_check_response(
            url,
            b"good:ic",
            true,
            Some(TTL_MS / 2),
            Some(ETAG_AJ),
            START_MS + TTL_MS / 2,
        );
        // Second fetch hits the metadata cache and the
        // rewritten resource is served out.
        self.check_warm_cache("second_fetch_1");

        self.advance_time_ms(2 * TTL_MS);
        self.reset_headers_and_stats();
        // Serves the original bytes again, with the injected
        // window.
        self.fetch_and_check_response(
            url,
            b"good",
            true,
            Some(INJECTED_S_MAXAGE_MS),
            Some("original_etag"),
            self.server.now_ms(),
        );
        // The metadata and cache entry is stale now. Fetch the
        // content and serve out the original. The background
        // rewrite work then revalidates the response and
        // updates metadata.
        assert_eq!(self.fetch_delta(), 1, "{url}");
        assert_eq!(self.img_filter.num_rewrites(), 0, "{url}");
        assert_eq!(self.js_filter.num_rewrites(), 0, "{url}");
        assert_eq!(self.css_filter.num_rewrites(), 0, "{url}");
    }
}

#[test]
fn cacheable_png_url_rewriting_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.expect_in_place_image_success_flow("http://www.example.com/cacheable.png");
}

#[test]
fn cacheable_png_url_rewriting_succeeds_with_shards() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.add_shard(
        "http://www.example.com",
        "http://s1.example.com/,http://s2.example.com/",
    );
    fixture.expect_in_place_image_success_flow("http://www.example.com/cacheable.png");
}

#[test]
fn cacheable_gif_url_rewriting_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.expect_in_place_image_success_flow("http://www.example.com/cacheable.gif");
}

#[test]
fn cacheable_webp_url_rewriting_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.expect_in_place_image_success_flow("http://www.example.com/cacheable.webp");
}

#[test]
fn cacheable_png_url_rewriting_fails() {
    // Setup the image filter to fail at rewriting.
    let fixture = Fixture::new();
    fixture.init();
    fixture.img_filter.set_enabled(false);
    let url = "http://www.example.com/cacheable.png";
    // The first serve streams the not-yet-optimized bytes.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        Some("original_etag"),
        START_MS,
    );
    // First fetch misses initial metadata lookup, finds
    // original in cache. The rewrite fails and metadata is
    // inserted into the cache indicating that the rewriting
    // didn't succeed.
    assert_eq!(fixture.fetch_delta(), 0);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(TTL_MS),
        Some("original_etag"),
        START_MS,
    );
    // Second fetch hits the metadata cache, sees that the
    // rewrite failed and fetches and serves the original
    // resource from cache.
    fixture.check_warm_cache("second_fetch_7");
}

#[test]
fn cacheable_js_url_rewriting_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    let url = "http://www.example.com/cacheable.js";
    // First serve streams the not-yet-optimized bytes.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 1);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        url,
        b"good:jm",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    // Second fetch hits the metadata cache and the rewritten
    // resource is served out.
    fixture.check_warm_cache("second_fetch_8");

    fixture.advance_time_ms(2 * TTL_MS);
    fixture.reset_headers_and_stats();
    // Serves the original bytes again, with the injected window.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        fixture.server.now_ms(),
    );
    // The metadata and cache entry is stale now. Fetch the
    // content and serve it out without rewriting. The
    // background rewrite will then revalidate a previous
    // rewrite's result and reuse it.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

#[test]
fn cacheable_js_url_rewriting_with_stale_serving() {
    let fixture = Fixture::new();
    fixture.init();
    fixture
        .options
        .borrow_mut()
        .set_metadata_cache_staleness_threshold_ms(TTL_MS);
    let url = "http://www.example.com/cacheable.js";
    // First serve streams the not-yet-optimized bytes.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 1);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        url,
        b"good:jm",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    fixture.check_warm_cache("second_fetch_9");

    fixture.set_time_ms(START_MS + (3 * TTL_MS) / 2);
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        url,
        b"good:jm",
        true,
        Some(5 * 60_000),
        Some(ETAG_AJ),
        START_MS + (3 * TTL_MS) / 2,
    );
    // The metadata and cache entry is stale now. We serve the
    // rewritten resource here, but trigger a fetch and rewrite
    // to update the metadata.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

#[test]
fn cacheable_js_url_modified_implicit_cache_ttl() {
    let fixture = Fixture::new();
    fixture.init();
    fixture
        .response_headers
        .borrow_mut()
        .set_implicit_cache_ttl_ms(500 * 1000);
    // The implicit TTL applies to the recorded entry; the
    // streamed not-yet-optimized serve is clamped to the
    // injected s-maxage window.
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheablemod.js",
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
}

#[test]
fn cacheable_css_url_if_css_rewriting_disabled() {
    let fixture = Fixture::new();
    fixture.init();
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::RewriteCss);
    let url = "http://www.example.com/cacheable.css";
    // First serve streams the not-yet-optimized bytes.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.reset_headers_and_stats();
    // The ETag we check for here is the ETag HTTPCache
    // synthesized for the original resource.
    fixture.fetch_and_check_response(url, b"good", true, Some(TTL_MS), Some(ETAG_0), START_MS);
    // Second fetch hits the metadata cache, finds that the
    // result is not optimizable. It then looks up cache for
    // the original and finds it.
    fixture.check_warm_cache("second_fetch_10");
}

#[test]
fn cacheable_css_url_rewriting_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    // NOTE: `EnableCachePurge` only arms purge machinery; the
    // flush below clears the backends directly.
    let url = "http://www.example.com/cacheable.css";
    // First serve streams the not-yet-optimized bytes.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 1);

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        url,
        b"good:cf",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    // Second fetch hits the metadata cache and the rewritten
    // resource is served out.
    fixture.check_warm_cache("second_css");

    fixture.advance_time_ms(2 * TTL_MS);
    fixture.reset_headers_and_stats();
    let date_of_css_ms = fixture.server.now_ms();
    // Serves the original bytes again, with the injected window.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        date_of_css_ms,
    );
    // The metadata and cache entry is stale now. Fetch the
    // content and serve it out without rewriting. The
    // background rewrite attempt will end up reusing the old
    // result due to revalidation, however.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    fixture.fetcher.set_update_date_headers(true);
    fixture.sync_mock_clock();
    // Having flushed cache, we are now back to serving the
    // origin content, with the injected window.
    fixture.http_backend.clear();
    fixture.metadata_backend.clear();
    fixture.reset_headers_and_stats();
    let date_of_css_ms = fixture.server.now_ms();
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        date_of_css_ms,
    );

    // Next time we'll serve optimized content.
    fixture.advance_time_ms(TTL_MS / 2);
    fixture.reset_headers_and_stats();
    let expected_ttl_ms = TTL_MS - (fixture.server.now_ms() - date_of_css_ms);
    fixture.fetch_and_check_response(
        url,
        b"good:cf",
        true,
        Some(expected_ttl_ms),
        Some(ETAG_AJ),
        fixture.server.now_ms(),
    );
}

#[test]
fn head_request_does_not_rewrite_empty_input() {
    // A HEAD request for a cached, not yet rewritten original
    // gets no body and does not start a rewrite of an empty
    // input: later GETs get the original and then the full
    // optimized body.
    let fixture = Fixture::new();
    fixture.init();
    let url = "http://www.example.com/cacheable.css";
    fixture.reset_headers_and_stats();

    {
        let driver = fixture.driver_for(url);
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        fetch
            .request_headers_mut()
            .set_method(ps_http::request::Method::Head);
        assert!(driver.fetch_resource(url, &mut fetch));
        assert!(fetch.done());
        assert_eq!(fetch.body(), b"");
    }
    assert_eq!(fixture.css_filter.num_rewrites(), 0);

    // GETs afterwards: the first serves the original bytes and
    // rewrites in the background, the next serves the full
    // optimized body.
    for expected_body in [b"good".as_slice(), b"good:cf".as_slice()] {
        let driver = fixture.driver_for(url);
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        assert!(driver.fetch_resource(url, &mut fetch));
        assert!(fetch.done());
        assert_eq!(fetch.body(), expected_body);
    }
    assert_eq!(fixture.css_filter.num_rewrites(), 1);
}

#[test]
fn css_in_undecodable_coding_is_served_as_sent() {
    // A stylesheet the origin sends in a coding the rewriters
    // cannot decode is served to each client as the origin sent
    // it, coding header included. It is not stored, so every
    // request goes to the origin, and it is never optimized.
    let fixture = Fixture::new();
    fixture.init();
    let url = "http://www.example.com/coded.css";
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    Fixture::set_default_headers("text/css", &mut headers);
    headers.set_date_and_caching(START_MS, TTL_MS, "");
    headers.headers_mut().add("Content-Encoding", "zstd");
    fixture.fetcher.set_response(url, headers, b"good");

    let mut total_origin_fetches = 0;
    for round in ["first", "repeat"] {
        fixture.reset_headers_and_stats();
        fixture.fetch_and_check_response(url, b"good", true, Some(TTL_MS), None, START_MS);
        assert_eq!(
            fixture
                .response_headers
                .borrow()
                .headers()
                .lookup1("Content-Encoding")
                .as_deref(),
            Some("zstd"),
            "{round}"
        );
        total_origin_fetches += fixture.fetch_delta();
        assert_eq!(fixture.css_filter.num_rewrites(), 0, "{round}");
    }
    assert_eq!(total_origin_fetches, 2);
}

#[test]
fn stored_css_in_other_coding_is_not_served_or_rewritten() {
    // A copy of a stylesheet in a coding the cache cannot
    // adapt, stored before that was refused on the way in, is
    // neither served nor used as the input of the optimization:
    // the stylesheet is fetched again, and the optimized result
    // is built from what the origin sends now.
    let fixture = Fixture::new();
    fixture.init();
    let url = "http://www.example.com/cacheable.css";
    let mut coded_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    Fixture::set_default_headers("text/css", &mut coded_headers);
    coded_headers.set_date_and_caching(START_MS, TTL_MS, "");
    coded_headers.headers_mut().add("Content-Encoding", "br");
    coded_headers.compute_caching();
    // NOTE: `put_value` refuses undecodable codings, so the
    // pre-existing entry is written to the backend directly.
    let mut stored = ps_http::value::HttpValue::new();
    stored.set_headers(coded_headers);
    stored.write_body(b"stored copy in another coding");
    let composite = fixture.server.http_cache().composite_key(url, "");
    fixture.http_backend.put(&composite, &stored.encode());

    // Served path: the origin's own bytes, not the stored copy.
    fixture.fetch_and_check_response(
        url,
        b"good",
        true,
        Some(INJECTED_S_MAXAGE_MS),
        None,
        START_MS,
    );
    assert!(!fixture
        .response_headers
        .borrow()
        .headers()
        .has("Content-Encoding"));
    assert!(fixture.fetch_delta() >= 1);
    assert_eq!(fixture.css_filter.num_rewrites(), 1);

    // Rewrite input: the optimized result is the origin's
    // stylesheet, rewritten.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.fetch_and_check_response(
        url,
        b"good:cf",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    assert_eq!(fixture.fetch_delta(), 0);
}

#[test]
fn non_cacheable_url_no_rewriting() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.fetch_and_check_response(
        "http://www.example.com/nocacheable.html",
        b"bad",
        true,
        Some(0),
        None,
        fixture.server.now_ms(),
    );
    // First fetch misses initial cache lookup, succeeds at
    // fetch and we don't insert into cache because it's not
    // cacheable. Don't attempt to rewrite this since its not
    // cacheable.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

#[test]
fn non_cacheable_url_rewriting() {
    // Tests that with correct flags set, the uncacheable
    // resource will be rewritten. Also checks, that resource
    // will not be inserted.
    let fixture = Fixture::new();
    fixture.init();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .set_rewrite_uncacheable_resources(true);

    // The ttl is just a value in proto, actual cacheable values
    // will be checked below.
    fixture.fetch_and_check_response(
        "http://www.example.com/nocacheable.js",
        b"good:jm",
        true,
        Some(ps_cache::timer::YEAR_MS),
        Some(ETAG_AJ),
        fixture.server.now_ms(),
    );
    // Shouldn't be cacheable at all.
    assert!(!fixture.response_headers.borrow().is_browser_cacheable());
    assert!(!fixture.response_headers.borrow().is_proxy_cacheable(
        ps_http::response::RequestProperties::default(),
        ps_http::response::VaryOption::IgnoreVaryOnResources,
        ps_http::response::ValidatorOption::NoValidator,
    ));
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    // Should have been rewritten.
    assert_eq!(fixture.js_filter.num_rewrites(), 1);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_UNCACHEABLE_REWRITES)
            .get(),
        1
    );
}

#[test]
fn private_cacheable_url_rewriting() {
    // Tests, that with correct flags set the private cacheable
    // resource will be rewritten. Also checks, that the
    // resource will not be cached.
    let fixture = Fixture::new();
    fixture.init();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .set_rewrite_uncacheable_resources(true);

    // The ttl is just a value in proto, actual cacheable values
    // will be checked below.
    fixture.fetch_and_check_response(
        "http://www.example.com/privatecacheable.js",
        b"good:jm",
        true,
        Some(1000),
        Some(ETAG_AJ),
        fixture.server.now_ms(),
    );
    // Should be cacheable.
    assert!(fixture.response_headers.borrow().is_browser_cacheable());
    // But only in a private way.
    assert!(!fixture.response_headers.borrow().is_proxy_cacheable(
        ps_http::response::RequestProperties::default(),
        ps_http::response::VaryOption::IgnoreVaryOnResources,
        ps_http::response::ValidatorOption::NoValidator,
    ));
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    // Should have been rewritten.
    assert_eq!(fixture.js_filter.num_rewrites(), 1);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_UNCACHEABLE_REWRITES)
            .get(),
        1
    );
}

#[test]
fn bad_url_no_rewriting() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.fetch_and_check_response(
        "http://www.example.com/bad.url",
        b"ugly",
        true,
        Some(0),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

#[test]
fn permanent_redirect_no_rewriting() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();
    fixture.fetch_and_check_response(
        "http://www.example.com/redir.url",
        b"Location: http://www.example.com/final.url",
        true,
        Some(36000),
        None,
        START_MS,
    );
    // Don't attempt to rewrite this since it's not a 200
    // response.
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

#[test]
fn fetch_failed_no_rewriting() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.fetch_and_check_response(
        "http://www.notincache.com",
        b"",
        false,
        Some(0),
        None,
        START_MS,
    );
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 0);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
}

#[test]
fn handle_resource_creation_failure() {
    // Regression test. Trying to in-place optimize https
    // resources with a fetcher that didn't support https would
    // fail to invoke the callbacks and leak the rewrite driver.
    // (Sync NOTE: this fetcher has no per-scheme support flag,
    // so the unscripted https URL attempts and fails like any
    // missed fetch, surfacing the attempt date.)
    let fixture = Fixture::new();
    fixture.init();
    fixture.fetch_and_check_response(
        "https://www.example.com",
        b"",
        false,
        Some(0),
        None,
        START_MS,
    );
}

#[test]
fn response_header_mime_type_update() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();
    // We are going to rewrite a PNG image below. Assume it will
    // be converted to a JPEG.
    fixture.img_filter.set_output_content_type(
        &ps_http::content_type::mime_to_content_type("image/jpeg").expect("jpeg known"),
    );
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.png",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Content-Type")
            .as_deref(),
        Some("image/jpeg"),
    );
}

#[test]
fn optimize_for_browser_encoding_and_header() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InPlaceOptimizeForBrowser);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.init();

    // Image with correct extension in URL.
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // Image with no extension in URL.
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable_jpg",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // CSS with correct extension in URL.
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.css",
        b"good:cf",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("User-Agent"),
    );

    // HTML with correct extension in URL.
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.html",
        b"good",
        true,
        Some(TTL_MS),
        Some("original_etag"),
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        None,
    );

    // Javascript with correct extension in URL.
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.js",
        b"good:jm",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        None,
    );

    // Javascript with jpeg extension in URL.
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable_js.jpg",
        b"good:jm",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 0);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        None,
    );

    // Bad content with unknown extension.
    fixture.reset_headers_and_stats();
    fixture.fetch_and_check_response(
        "http://www.example.com/bad.url",
        b"ugly",
        true,
        Some(0),
        None,
        START_MS,
    );
    assert_eq!(fixture.css_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.img_filter.num_encode_user_agent(), 1);
    assert_eq!(fixture.js_filter.num_encode_user_agent(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        None,
    );
}

#[test]
fn optimize_for_browser_rewriting() {
    // When in_place_wait_for_optimized is true, force_rewrite
    // is set to true and the nested RewriteContext will not
    // check for rewritten content if input is ready.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .set_private_not_vary_for_ie(true);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InPlaceOptimizeForBrowser);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.init();
    let url = "http://www.example.com/cacheable.jpg";

    // First fetch with kTestUserAgentWebP. This will miss
    // everything (metadata lookup, original content, and
    // rewritten content). Vary: Accept header should be added.
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_WEBP);
    fixture.add_request_attribute("Accept", "image/webp");
    fixture.fetch_and_check_response(url, b"good:ic", true, Some(TTL_MS), Some(ETAG_AJ), START_MS);
    assert_eq!(fixture.fetch_delta(), 1);
    assert_eq!(fixture.img_filter.num_rewrites(), 1);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // The second fetch uses a different user agent,
    // kTestUserAgentNoWebP. This will miss the metadata cache
    // so it will start fetch input (cache hit) and rewrite
    // content (cache miss). Vary: Accept header should be be
    // added.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_NO_WEBP);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    assert_eq!(fixture.fetch_delta(), 0);
    assert_eq!(fixture.img_filter.num_rewrites(), 1);
    assert_eq!(fixture.js_filter.num_rewrites(), 0);
    assert_eq!(fixture.css_filter.num_rewrites(), 0);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // The third fetch uses an IE 9 user agent string, which
    // should result in a Cache-Control: private resource and no
    // Vary header.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent("Mozilla/5.0 (Windows; U; MSIE 9.0; WIndows NT 9.0; en-US))");
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    fixture.check_warm_cache("no_webp_to_ie");
    assert!(!fixture.response_headers.borrow().headers().has("Vary"));
    let cache_controls = fixture
        .response_headers
        .borrow()
        .headers()
        .lookup("Cache-Control")
        .unwrap_or_default();
    assert_eq!(cache_controls.len(), 2);
    assert_eq!(cache_controls[1], "private");

    // Fetch again still with kTestUserAgentWebP, but omits the
    // Accept:webp header. Metadata cache hits. No input fetch
    // and rewriting. Vary: Accept header should be be added.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_WEBP);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    fixture.check_warm_cache("no_webp_without_accept");
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // Fetch another time, switching to just sending Accept:
    // webp and using kTestUserAgentNoWebP. Metadata cache hits.
    // No input fetch and rewriting. Vary: Accept header should
    // be added.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_NO_WEBP);
    fixture.add_request_attribute("Accept", "image/webp");
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    fixture.check_warm_cache("back_to_webp");
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );
}

#[test]
fn optimize_for_browser_no_private_for_ie() {
    // Similar to test above, but set private_not_vary_for_ie
    // to false and omit detailed checking of cache hit
    // statistics, focusing just on a behavioral test.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .set_private_not_vary_for_ie(false);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InPlaceOptimizeForBrowser);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.init();
    let url = "http://www.example.com/cacheable.jpg";

    // First fetch with kTestUserAgentWebP.
    // Vary: Accept header should be added.
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_WEBP);
    fixture.add_request_attribute("Accept", "image/webp");
    fixture.fetch_and_check_response(url, b"good:ic", true, Some(TTL_MS), Some(ETAG_AJ), START_MS);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // The second fetch uses a different user agent,
    // kTestUserAgentNoWebP. Vary: Accept header should be be
    // added.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_NO_WEBP);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // The third fetch uses an IE 9 user agent string, which
    // should *also* have a Vary: Accept header since
    // private_not_vary_for_ie == false.
    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent("Mozilla/5.0 (Windows; U; MSIE 9.0; WIndows NT 9.0; en-US))");
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );
}

#[test]
fn accept_header_merging() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InPlaceOptimizeForBrowser);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.init();
    fixture.add_request_attribute("Accept", "image/webp");
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("Accept"),
    );

    // We don't actually optimize the Vary: * resource. See
    // `HasExplicitNoCacheDirective`. Inexplicably (?), we also
    // change its ttl to 0 in spite of incoming ttl headers.
    fixture.fetch_and_check_response(
        "http://www.example.com/vary_star.jpg",
        b"good",
        true,
        Some(0),
        None,
        START_MS,
    );
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        Some("*"),
    );

    // TODO(jmaessen): Right now we're not properly passing
    // through Vary: headers from the fetched resource. When
    // jmarantz's pending change lands, we will do so, and these
    // tests should be re-enabled accordingly. (Kept disabled
    // here too.)
}

#[test]
fn no_accept_header_for_lossless_or_animated() {
    // Make sure that InPlaceRewriteContext won't add "Vary:
    // Accept" header to an image optimized to WebP lossless or
    // WebP animated. Note that we're using FakeImageFilter in
    // this test. If we use the real filter, ImageRewriteFilter,
    // an image will never be converted to WebP lossless nor
    // WebP animated, unless we're allowed to vary on
    // user-agent.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InPlaceOptimizeForBrowser);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.init();
    fixture.add_request_attribute("Accept", "image/webp");

    // First check lossless case.
    fixture
        .img_filter
        .set_optimized_image_type(ps_image::format::ImageType::WebpLosslessOrAlpha);
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.png",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert!(!fixture.response_headers.borrow().headers().has("Vary"));

    // Now check animated case.
    fixture
        .img_filter
        .set_optimized_image_type(ps_image::format::ImageType::WebpAnimated);
    fixture.fetch_and_check_response(
        "http://www.example.com/cacheable.jpg",
        b"good:ic",
        true,
        Some(TTL_MS),
        Some(ETAG_AJ),
        START_MS,
    );
    assert!(!fixture.response_headers.borrow().headers().has("Vary"));
}

#[test]
fn optimize_for_browser_negative() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();
    let url = "http://www.example.com/cacheable.jpg";

    // Vary: User-Agent header should not be added no matter the
    // user-agent.
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_WEBP);
    fixture.add_request_attribute("Accept", "image/webp");
    fixture.fetch_and_check_response(url, b"good:ic", true, Some(TTL_MS), Some(ETAG_AJ), START_MS);
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        None,
    );

    fixture.reset_headers_and_stats();
    fixture.set_time_ms(START_MS + TTL_MS / 2);
    fixture.reset_user_agent(ps_http::user_agent_matcher::TEST_USER_AGENT_NO_WEBP);
    fixture.fetch_and_check_response(
        url,
        b"good:ic",
        true,
        Some(TTL_MS / 2),
        Some(ETAG_AJ),
        START_MS + TTL_MS / 2,
    );
    assert_eq!(
        fixture
            .response_headers
            .borrow()
            .headers()
            .lookup1("Vary")
            .as_deref(),
        None,
    );
}

#[test]
fn json_with_js_content_type_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.check_caching_and_content_type(
        "http://www.example.com/cacheable_js_type.json",
        "application/javascript",
        "good",
        "jm",
    );
}

#[test]
fn json_with_json_content_type_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.check_caching_and_content_type(
        "http://www.example.com/cacheable_json_type.json",
        "application/json",
        "good",
        "jm",
    );
}

#[test]
fn json_with_json_content_type_synonym_succeeds() {
    let fixture = Fixture::new();
    fixture.init();
    fixture.check_caching_and_content_type(
        "http://www.example.com/cacheable_json_synonym_type.json",
        "application/x-json",
        "good",
        "jm",
    );
}

#[test]
fn wait_for_optimize_oversized_no_short_content_length() {
    // When a buffered response crosses the cache size limit, the
    // fetch falls back to passing the original body through. It
    // must not declare a Content-Length shorter than the body it
    // actually delivers.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_in_place_wait_for_optimized(true);
    fixture.init();
    fixture.server.http_cache().set_max_body_bytes(1);

    let url = "http://www.example.com/cacheable.jpg";
    let driver = fixture.driver_for(url);
    let ctx = driver.request_context().borrow().clone();
    let mut fetch = BufferingFetch::new(url, ctx);
    assert!(driver.fetch_resource(url, &mut fetch));
    assert!(fetch.done());
    assert!(fetch.success());
    // The client receives the complete, unmodified body, and no
    // Content-Length shorter than the body is declared.
    assert_eq!(fetch.body(), b"good");
    match fetch.response_headers().headers().lookup1("Content-Length") {
        None => {}
        Some(length) => assert!(
            length.parse::<i64>().is_ok_and(|n| n >= 4),
            "short content-length: {length}"
        ),
    }
    assert_eq!(
        fixture
            .server
            .variable(ps_html::ipro::IN_PLACE_OVERSIZED_OPT_STREAM)
            .get(),
        1
    );
}
