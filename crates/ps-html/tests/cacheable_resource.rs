//! Resource load tests ported from `cacheable_resource_base_test.cc`.
//!
//! NOTE: the C++ async machinery (locks, wait-fetchers, thread event
//! counts) collapses here — fetchers complete inline, so simultaneous
//! freshens run back-to-back with identical observable effects.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_cache::timer::YEAR_MS;
use ps_config::options::RewriteOptions;
use ps_config::Scope;
use ps_html::cacheable_resource::{
    CacheableResourceBase, FreshenCallback, InputInfo, LoadCallback, MockLoadCallback,
    NotCacheablePolicy, ResourceHooks,
};
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::request::RequestContext;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_URL: &str = "http://www.example.com/";
const TEST_REF: &str = "http://www.example.com/some_page";
const CONTENT: &str = "content!";
const START_MS: i64 = 1_700_000_000_000;

/// Test hooks with flippable prepare/load behavior.
#[derive(Clone, Default)]
struct TestHooks {
    do_prepare_request: bool,
    do_prepare_response: bool,
}

impl ResourceHooks for TestHooks {
    fn prepare_request(
        &self,
        ctx: &mut RequestContext,
        headers: &mut ps_http::request::RequestHeaders,
    ) {
        if self.do_prepare_request {
            headers.headers_mut().replace(header::REFERER, TEST_REF);
            ctx.add_session_authorized_fetch_origin(TEST_URL);
        }
    }

    fn prepare_response_headers(&self, headers: &mut ResponseHeaders) {
        if self.do_prepare_response {
            headers
                .headers_mut()
                .remove(header::CACHE_CONTROL, "private");
        }
    }
}

type TestResource = CacheableResourceBase<TestHooks>;

struct Fixture {
    server: Rc<ServerContext>,
    driver: Rc<ResourceDriver>,
    fetcher: Arc<MockUrlFetcher>,
    options: Rc<RefCell<RewriteOptions>>,
}

impl Fixture {
    fn new() -> Self {
        let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let fetcher = Arc::new(MockUrlFetcher::new());
        let server = Rc::new(ServerContext::new(backend, fetcher.clone(), START_MS));
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        CacheableResourceBase::<TestHooks>::init_stats(server.statistics(), "test");
        let driver = Rc::new(ResourceDriver::new(server.clone(), options.clone(), ""));
        Fixture {
            server,
            driver,
            fetcher,
            options,
        }
    }

    fn resource(&self) -> TestResource {
        TestResource::new(
            self.driver.clone(),
            "test",
            TEST_URL,
            TEST_URL,
            None,
            TestHooks::default(),
        )
    }

    fn request_context(&self) -> RequestContext {
        RequestContext::new(HttpOptions::default_for_tests())
    }

    fn advance_ms(&self, delta_ms: i64) {
        self.server.timer().borrow_mut().advance_ms(delta_ms);
    }

    /// Long-cache headers (`SetDefaultLongCacheHeaders`, year TTL).
    fn long_cache_headers(&self, mime: &str) -> ResponseHeaders {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, mime);
        headers.set_date_and_caching(self.server.now_ms(), YEAR_MS, "");
        headers.headers_mut().add(header::ETAG, "W/\"0\"");
        headers.set_last_modified(self.server.now_ms());
        headers.compute_caching();
        headers
    }

    /// Scripts a response with default headers
    /// (`SetResponseWithDefaultHeaders`, no validators).
    fn set_response_with_default_headers(&self, url: &str, mime: &str, body: &str, ttl_sec: i64) {
        let mut headers = self.long_cache_headers(mime);
        headers.set_date_and_caching(self.server.now_ms(), ttl_sec * 1000, "");
        headers.compute_caching();
        // Validators would make conditional refreshes trivially succeed.
        headers.headers_mut().remove_all(header::ETAG);
        headers.headers_mut().remove_all(header::LAST_MODIFIED);
        self.fetcher.set_response(url, headers, body.as_bytes());
    }

    fn load(
        &self,
        resource: &mut TestResource,
        policy: NotCacheablePolicy,
        ctx: &mut RequestContext,
    ) -> MockLoadCallback {
        let mut callback = MockLoadCallback::new();
        resource.load(policy, ctx, &mut callback);
        callback
    }

    fn check_stats(
        &self,
        resource: &TestResource,
        hits: i64,
        failures: i64,
        miss: i64,
        uncacheable_failure: i64,
        misses: i64,
    ) {
        assert_eq!(
            resource.stats(),
            (hits, failures, miss, uncacheable_failure, misses)
        );
    }
}

/// Recording freshen callback (`MockFreshenCallback`, owning its record).
struct MockFreshenCallback {
    input: InputInfo,
    done: bool,
    extend_success: bool,
}

impl MockFreshenCallback {
    fn new(input: InputInfo) -> Self {
        MockFreshenCallback {
            input,
            done: false,
            extend_success: false,
        }
    }
}

impl FreshenCallback for MockFreshenCallback {
    fn input_info_mut(&mut self) -> Option<&mut InputInfo> {
        Some(&mut self.input)
    }

    fn done(&mut self, _lock_failure: bool, extend_success: bool) {
        self.done = true;
        self.extend_success = extend_success;
    }
}

#[test]
fn basic_cached() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(TEST_URL, "text/plain", CONTENT, 1000);

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), CONTENT.as_bytes());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 0, 0, 0, 0, 1);

    // Second read is cached.
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), CONTENT.as_bytes());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 1, 0, 0, 0, 1);

    // 900s into a 1000s TTL the entry is imminently expiring, so the
    // hit also triggers a background freshen (one more fetch).
    fixture.advance_ms(900 * 1000);
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), CONTENT.as_bytes());
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    fixture.check_stats(&resource, 2, 0, 0, 0, 1);
}

#[test]
fn fallback() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option("ServeStaleIfFetchError", "on", Scope::ProcessStrict)
        .unwrap();
    fixture.fetcher.disable();
    fixture.fetcher.set_error_message(b"oww!");

    let headers = fixture.long_cache_headers("text/plain");
    let mut resource = fixture.resource();
    resource.link_fallback_value(&headers, b"fallback");

    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), b"fallback");
    assert_eq!(
        resource.fetch_response_status(),
        ps_cache::ttl::FetchStatus::OtherError
    );
}

#[test]
fn fallback_empty() {
    // Empty fallbacks must not trip any emptiness checks.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option("ServeStaleIfFetchError", "on", Scope::ProcessStrict)
        .unwrap();
    fixture.fetcher.disable();
    fixture.fetcher.set_error_message(b"oww!");

    let headers = fixture.long_cache_headers("text/plain");
    let mut resource = fixture.resource();
    resource.link_fallback_value(&headers, b"");

    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert!(resource.contents().is_empty());
    assert_eq!(
        resource.fetch_response_status(),
        ps_cache::ttl::FetchStatus::OtherError
    );
}

#[test]
fn private() {
    let fixture = Fixture::new();
    let mut headers = fixture.long_cache_headers("text/plain");
    headers.headers_mut().add(header::CACHE_CONTROL, "private");
    fixture
        .fetcher
        .set_response(TEST_URL, headers, CONTENT.as_bytes());

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 0, 0, 0, 0, 1);

    // The non-cacheability itself is cached: no refetch.
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 0, 0, 0, 1, 1);
}

#[test]
fn private_for_fetch() {
    let fixture = Fixture::new();
    let mut headers = fixture.long_cache_headers("text/plain");
    headers.headers_mut().add(header::CACHE_CONTROL, "private");
    fixture
        .fetcher
        .set_response(TEST_URL, headers, CONTENT.as_bytes());

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), CONTENT.as_bytes());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 0, 0, 0, 0, 1);

    // Uncacheable but explicitly loadable: refetch every time.
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), CONTENT.as_bytes());
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    fixture.check_stats(&resource, 0, 0, 1, 0, 1);
}

#[test]
fn fetch_failure() {
    // NOTE: `fail_on_unexpected` only gates a C++ test assertion; the
    // Rust mock fails unset URLs unconditionally, same observable.
    let fixture = Fixture::new();
    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 0, 0, 0, 0, 1);

    // The failure is remembered: no refetch.
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    fixture.check_stats(&resource, 0, 1, 0, 0, 1);

    // Past the failure TTL (300s for errors): refetch.
    fixture.advance_ms(2 * 300 * 1000);
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    fixture.check_stats(&resource, 0, 1, 0, 0, 2);
}

#[test]
fn drop_handling() {
    let fixture = Fixture::new();
    // Load-shed drop: serving 503 keeps the entry unusable.
    let mut dropped = fixture.long_cache_headers("text/html");
    dropped.set_status_and_reason(503);
    dropped.headers_mut().add(header::X_PSA_LOAD_SHED, "1");
    fixture
        .fetcher
        .set_response("http://www.example.com/", dropped, b"hmm");

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!resource.is_valid_and_cacheable());
    resource.reset();

    // The origin recovers, but the background load still sees the drop.
    let mut found = fixture.long_cache_headers("text/css");
    found.set_status_and_reason(200);
    fixture
        .fetcher
        .set_response("http://www.example.com/", found, b"* {display: stylish; }");
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(!resource.is_valid_and_cacheable());
    resource.reset();

    // User-facing loads retry past remembered drops.
    resource.set_is_background_fetch(false);
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(resource.is_valid_and_cacheable());
}

#[test]
fn freshen_info() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(TEST_URL, "text/plain", CONTENT, 1000);

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    assert!(callback.done());
    assert!(callback.success());

    let mut input = InputInfo::default();
    resource.fill_in_partition_input_info(true, &mut input);
    let mut input2 = input.clone();

    // 900s on, freshening refetches and extends the record.
    fixture.advance_ms(900 * 1000);
    let mut freshen_cb = MockFreshenCallback::new(input);
    resource.freshen(Some(&mut freshen_cb));
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    assert!(freshen_cb.done);
    assert!(freshen_cb.extend_success);
    assert_eq!(
        Some(1000 * 1000 + fixture.server.now_ms()),
        freshen_cb.input.expiration_time_ms
    );

    // Fresh again: the second freshen extends from cache alone.
    let mut freshen_cb2 = MockFreshenCallback::new(input2.clone());
    resource.freshen(Some(&mut freshen_cb2));
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    assert!(freshen_cb2.done);
    assert!(freshen_cb2.extend_success);
    assert_eq!(
        Some(1000 * 1000 + fixture.server.now_ms()),
        freshen_cb2.input.expiration_time_ms
    );
    let _ = &mut input2;
}

#[test]
fn same_url_different_key() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(TEST_URL, "text/plain", CONTENT, 1000);

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert_eq!(fixture.fetcher.fetch_count(), 1);
    assert!(callback.done());
    assert!(callback.success());

    let mut resource2 = fixture.resource();
    resource2.set_cache_key("http://other_key.org/");
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource2,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    // Two fetches: different keys share nothing.
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    assert!(callback.done());
    assert!(callback.success());

    // Both cached now.
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert_eq!(fixture.fetcher.fetch_count(), 2);
    assert!(callback.done());
    assert!(callback.success());

    // Deleting the first entry leaves the second intact.
    fixture.server.http_cache().delete(TEST_URL, "");
    resource.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert_eq!(fixture.fetcher.fetch_count(), 3);
    assert!(callback.done());
    assert!(callback.success());

    resource2.reset();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource2,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert_eq!(fixture.fetcher.fetch_count(), 3);
    assert!(callback.done());
    assert!(callback.success());

    // Freshening both extends both records (the C++ overlap is a
    // lock check, mute with inline completion).
    let mut input = InputInfo::default();
    resource.fill_in_partition_input_info(true, &mut input);
    let mut input2 = InputInfo::default();
    resource2.fill_in_partition_input_info(true, &mut input2);
    fixture.advance_ms(900 * 1000);
    let mut freshen_cb = MockFreshenCallback::new(input);
    resource.freshen(Some(&mut freshen_cb));
    let mut freshen_cb2 = MockFreshenCallback::new(input2);
    resource2.freshen(Some(&mut freshen_cb2));
    assert_eq!(fixture.fetcher.fetch_count(), 5);
    assert_eq!(
        Some(1000 * 1000 + fixture.server.now_ms()),
        freshen_cb.input.expiration_time_ms
    );
    assert_eq!(
        Some(1000 * 1000 + fixture.server.now_ms()),
        freshen_cb2.input.expiration_time_ms
    );
}

#[test]
fn prepare_hooks() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(TEST_URL, "text/plain", CONTENT, 1000);

    let mut resource = fixture.resource();
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(resource.contents(), CONTENT.as_bytes());
    assert_eq!(fixture.fetcher.fetch_count(), 1);

    // Without the hook: no referer, no session authorization.
    assert_eq!(fixture.fetcher.last_referer(), None);
    assert!(!ctx.is_session_authorized_fetch_origin(TEST_URL));

    // With the hook: referer recorded, origin session-authorized.
    resource.reset();
    resource.hooks_mut().do_prepare_request = true;
    fixture.server.http_cache().delete(TEST_URL, "");
    let mut ctx = fixture.request_context();
    let callback = fixture.load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &mut ctx,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(fixture.fetcher.last_referer().as_deref(), Some(TEST_REF));
    assert!(ctx.is_session_authorized_fetch_origin(TEST_URL));
}
