//! Google-Font-Service-input-resource tests ported from
//! `google_font_service_input_resource_test.cc`.
//!
//! NOTE: `LoadParallel` waits for the async reconstruction epic;
//! the sync port cannot hold two loads pending at once.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
use ps_html::font_service_input_resource::{
    FontServiceHooks, FontServiceInputResource, STAT_PREFIX,
};
use ps_html::resource::Resource;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;
const ROBOTO: &str = "http://fonts.googleapis.com/css?family=Roboto";
const ROBOTO_SSL: &str = "https://fonts.googleapis.com/css?family=Roboto";
const NON_CSS: &str = "http://fonts.googleapis.com/some.txt";

/// UA-sensitive fetcher wrapper (`UserAgentSensitiveTestFetcher`).
struct UaSensitiveFetcher {
    base: Arc<MockUrlFetcher>,
}

impl AsyncFetcher for UaSensitiveFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        let parsed = GoogleUrl::new(url);
        assert!(parsed.is_web_valid());
        if !fetch
            .request_context()
            .is_session_authorized_fetch_origin(parsed.origin())
        {
            fetch.handle_done(false);
            return;
        }
        let ua = fetch
            .request_headers()
            .headers()
            .lookup1(header::USER_AGENT)
            .unwrap_or_else(|| "unknown".to_string());
        let with_ua = parsed.copy_and_add_query_param("UA", &ua);
        self.base.fetch(with_ua.spec(), fetch);
    }

    fn supports_https(&self) -> bool {
        self.base.supports_https()
    }
}

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Arc<MockUrlFetcher>,
    user_agent: RefCell<String>,
}

impl Fixture {
    fn new() -> Self {
        let backend = Arc::new(MemoryCache::new());
        let metadata = Arc::new(MemoryCache::new());
        let mock = Arc::new(MockUrlFetcher::new());
        let server = ServerContext::new_with_metadata(
            backend,
            metadata,
            Arc::new(UaSensitiveFetcher { base: mock.clone() }) as Arc<dyn AsyncFetcher>,
            START_MS,
        );
        let server = Rc::new(server);
        ps_html::cacheable_resource::CacheableResourceBase::<FontServiceHooks>::init_stats(
            server.statistics(),
            STAT_PREFIX,
        );
        let fixture = Fixture {
            server,
            options: Rc::new(RefCell::new(RewriteOptions::new())),
            fetcher: mock,
            user_agent: RefCell::new(String::new()),
        };
        // Font loader CSS gets Cache-Control:private, max-age=86400
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        fixture.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("text/css"),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(fixture.server.now_ms(), 86400 * 1000, ", private");
        headers.compute_caching();
        // Set them up in spots where UaSensitiveFetcher would direct them.
        fixture.fetcher.set_response(
            &format!("{ROBOTO}&UA=Chromezilla"),
            headers.clone(),
            b"font_chromezilla",
        );
        fixture.fetcher.set_response(
            &format!("{ROBOTO}&UA=Safieri"),
            headers.clone(),
            b"font_safieri",
        );
        fixture.fetcher.set_response(
            &format!("{ROBOTO_SSL}&UA=Chromezilla"),
            headers.clone(),
            b"sfont_chromezilla",
        );
        fixture.fetcher.set_response(
            &format!("{ROBOTO_SSL}&UA=Safieri"),
            headers,
            b"sfont_safieri",
        );

        let mut non_css = ResponseHeaders::new(HttpOptions::default_for_tests());
        fixture.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("text/plain"),
            "",
            "",
            &mut non_css,
        );
        fixture.fetcher.set_response(
            &format!("{NON_CSS}?UA=Chromezilla"),
            non_css,
            b"something weird",
        );
        fixture
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.set_user_agent(&self.user_agent.borrow());
        driver
    }

    /// Installs the UA (`ResetUserAgent`).
    fn reset_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
    }

    /// Makes a font resource for the URL under the current UA.
    fn make(&self, url: &str) -> (Rc<ResourceDriver>, Option<FontServiceInputResource>) {
        let driver = self.driver_for(TEST_DOMAIN);
        let gurl = GoogleUrl::new(url);
        let resource = FontServiceInputResource::make_font_service(&gurl, &driver);
        (driver, resource)
    }
}

/// Loads a resource to completion, returning the callback.
fn load(
    resource: &mut FontServiceInputResource,
    policy: NotCacheablePolicy,
    driver: &Rc<ResourceDriver>,
) -> MockLoadCallback {
    let mut callback = MockLoadCallback::new();
    let mut ctx = driver.request_context().borrow().clone();
    resource.load(policy, &mut ctx, &mut callback);
    callback
}

#[test]
fn creation() {
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");

    let (_, resource) = fixture.make("efpeRO#@($@#K$!@($");
    assert!(resource.is_none());

    let (_, resource) = fixture.make("http://example.com/foo.css");
    assert!(resource.is_none());

    let (_, resource) = fixture.make(ROBOTO);
    let resource = resource.expect("roboto");
    assert_eq!(ROBOTO, resource.url());
    assert_eq!(
        "gfnt://fonts.googleapis.com/css?family=Roboto&X-PS-UA=Chromezilla",
        resource.cache_key()
    );

    let (_, resource) = fixture.make(ROBOTO_SSL);
    let resource = resource.expect("roboto ssl");
    assert_eq!(ROBOTO_SSL, resource.url());
    assert_eq!(
        "gfnts://fonts.googleapis.com/css?family=Roboto&X-PS-UA=Chromezilla",
        resource.cache_key()
    );
}

#[test]
fn load_resource() {
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");

    let (driver, resource) = fixture.make(ROBOTO);
    let mut resource = resource.expect("roboto");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(b"font_chromezilla", resource.contents().as_slice());
    assert_eq!(1u64, fixture.fetcher.fetch_count());

    // Make sure it's cached.
    let (driver, resource) = fixture.make(ROBOTO);
    let mut resource = resource.expect("roboto");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(b"font_chromezilla", resource.contents().as_slice());
    assert_eq!(1u64, fixture.fetcher.fetch_count());

    // But that different UA gets different string.
    fixture.reset_user_agent("Safieri");
    let (driver, resource) = fixture.make(ROBOTO);
    let mut resource = resource.expect("roboto");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(b"font_safieri", resource.contents().as_slice());
    assert_eq!(2u64, fixture.fetcher.fetch_count());
}

#[test]
fn ua_normalization() {
    let fixture = Fixture::new();
    const IE7A: &str = "Mozilla/4.0 (compatible; MSIE 7.0; Windows NT 5.1; Trident/4.0; \
        .NET CLR 2.0.50727; .NET CLR 3.0.4506.2152; .NET CLR 3.5.30729)";

    let url = GoogleUrl::new(ROBOTO);
    let url_plus_ua = url.copy_and_add_query_param("UA", IE7A);
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("text/css"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), 86400 * 1000, ", private");
    headers.compute_caching();
    fixture
        .fetcher
        .set_response(url_plus_ua.spec(), headers, b"font_IE7");

    // Try fetches with a couple of possible aliases. The one we uploaded it under
    // is first, since it's the only one the fetcher replies to.
    fixture.reset_user_agent(IE7A);
    let (driver, resource) = fixture.make(ROBOTO);
    let mut resource = resource.expect("roboto");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(b"font_IE7", resource.contents().as_slice());
    assert_eq!(1u64, fixture.fetcher.fetch_count());

    // Different list of .NET versions.
    const IE7B: &str = "Mozilla/4.0 (compatible; MSIE 7.0; Windows NT 5.1; Trident/4.0; \
        .NET CLR 2.0.50727; .NET4.0C; .NET4.0E)";
    fixture.reset_user_agent(IE7B);
    let (driver, resource) = fixture.make(ROBOTO);
    let mut resource = resource.expect("roboto");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::ReportFailureIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(b"font_IE7", resource.contents().as_slice());
    assert_eq!(1u64, fixture.fetcher.fetch_count());
}

// NOTE: `LoadParallel` waits for the async reconstruction epic (see
// file header).

#[test]
fn fetch_failure() {
    let fixture = Fixture::new();
    // Regression test --- don't crash when fetch fails.
    // Bug discovered by accident due to a bug in a test.
    fixture.reset_user_agent("Huhzilla");
    let (driver, resource) = fixture.make(ROBOTO);
    let mut resource = resource.expect("roboto");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(!callback.success());
    assert!(resource.contents().is_empty());
    assert_eq!(1u64, fixture.fetcher.fetch_count());
}

#[test]
fn dont_load_non_css() {
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");

    let (driver, resource) = fixture.make(NON_CSS);
    let mut resource = resource.expect("non css");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(!resource.http_status_ok());
    assert_eq!(1u64, fixture.fetcher.fetch_count());

    // Make sure we don't end up caching a success, either.
    let (driver, resource) = fixture.make(NON_CSS);
    let mut resource = resource.expect("non css");
    let callback = load(
        &mut resource,
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &driver,
    );
    assert!(callback.done());
    assert!(!resource.http_status_ok());
    assert_eq!(1u64, fixture.fetcher.fetch_count());
}

#[test]
fn is_font_service_url_test() {
    assert!(FontServiceInputResource::is_font_service_url(
        &GoogleUrl::new(ROBOTO)
    ));
    assert!(FontServiceInputResource::is_font_service_url(
        &GoogleUrl::new(ROBOTO_SSL)
    ));
    assert!(!FontServiceInputResource::is_font_service_url(
        &GoogleUrl::new("http://example.com/")
    ));
}
