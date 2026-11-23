//! HTML-cache cookie-policy integration tests: public session API
//! driven through configured options, covering default allowlist
//! acceptance, key stability, configuration overrides, and backward
//! compatibility.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_cache::purge::PurgeSet;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::html_cache::HtmlCacheSession;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::{Method, RequestHeaders};
use ps_http::response::{HttpOptions, ResponseHeaders};

const PAGE_URL: &str = "http://cookies.test/page.html";
const START_MS: i64 = 1_700_000_000_000;

/// Configured driver harness over memory caches.
struct CookiesHarness {
    options: Rc<RefCell<RewriteOptions>>,
    driver: Rc<ResourceDriver>,
    purge: PurgeSet,
}

impl CookiesHarness {
    fn new() -> Self {
        let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let server = Rc::new(ServerContext::new_with_metadata(
            http,
            metadata,
            Arc::new(MockUrlFetcher::new()),
            START_MS,
        ));
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .set_option("HtmlCache", "on", Scope::Server)
            .unwrap();
        options
            .borrow_mut()
            .set_option("HtmlCacheTtlSec", "60", Scope::Server)
            .unwrap();
        let driver = Rc::new(ResourceDriver::new(server, Rc::clone(&options), PAGE_URL));
        CookiesHarness {
            options,
            driver,
            purge: PurgeSet::new(25_000),
        }
    }

    fn session(&self) -> HtmlCacheSession {
        HtmlCacheSession::new(Rc::clone(&self.driver), PAGE_URL)
    }

    fn counter(&self, name: &str) -> i64 {
        self.driver.server_context().variable(name).get()
    }
}

fn get_request() -> RequestHeaders {
    let mut request = RequestHeaders::new();
    request.set_method(Method::Get);
    request.set_version(1, 1);
    request.headers_mut().add("Host", "cookies.test");
    request.headers_mut().add("Accept", "text/html");
    request
}

fn cookie_request(cookies: &str) -> RequestHeaders {
    let mut request = get_request();
    request.headers_mut().add("Cookie", cookies);
    request
}

fn ok_origin() -> ResponseHeaders {
    let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
    origin.set_status_and_reason(200);
    origin.headers_mut().add("Content-Type", "text/html");
    origin
        .headers_mut()
        .add("Date", &ps_http::time::format_http_date(START_MS));
    origin
        .headers_mut()
        .add("Cache-Control", "public, max-age=300");
    origin.compute_caching();
    origin
}

#[test]
fn analytics_requests_cache_by_default_without_configuration() {
    // No cookie directives configured: the built-in allowlist is on.
    let harness = CookiesHarness::new();
    assert!(harness.options.borrow().html_cache_ignore_common_cookies());
    let origin = ok_origin();
    // A browser-like analytics cookie jar stores …
    let jar = "_ga=GA1.1.111; _gid=222; _gat=1; __utma=3; _fbp=4; _fbc=5; _clck=6; _clsk=7; \
         _hjSession_8=9; _hjFirstSeen=10; _gcl_au=11; __gads=12; _fplc=13";
    let request = cookie_request(jar);
    let mut first = harness.session();
    assert!(first
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    first.write(b"<html>analytics</html>");
    let finish = first.finish(&request, START_MS + 1000);
    assert!(finish.detail_line.is_some());
    assert!(finish.bypass_line.is_none());
    assert_eq!(1, harness.counter("html_cache_inserts"));
    assert_eq!(1, harness.counter("html_cache_cookie_allowlisted"));
    // … and a different visitor's values hit the same variant.
    let other = cookie_request("_ga=GA1.9.999; _gid=888");
    let mut second = harness.session();
    let entry = second
        .lookup(&origin, &other, &harness.purge, None, START_MS + 2000)
        .expect("shared analytics variant");
    assert_eq!(b"<html>analytics</html>", entry.body.as_slice());
    assert_eq!(1, harness.counter("html_cache_hits"));
    assert_eq!(0, harness.counter("html_cache_bypasses"));
}

#[test]
fn personalization_cookies_still_bypass_after_upgrade() {
    // Backward compatibility: unknown cookies bypass exactly as before.
    let harness = CookiesHarness::new();
    let origin = ok_origin();
    for cookies in [
        "sessionid=abc",
        "wordpress_logged_in_x=y",
        "_ga=1; cart=2",
        "a=1",
    ] {
        let request = cookie_request(cookies);
        let mut session = harness.session();
        assert!(session
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        let finish = session.finish(&request, START_MS + 1000);
        assert!(finish.detail_line.is_none());
        let line = finish.bypass_line.expect("named bypass");
        assert!(line.starts_with("PS_HTML_BYPASS\t"), "{line}");
    }
    assert_eq!(4, harness.counter("html_cache_bypasses"));
    assert_eq!(0, harness.counter("html_cache_inserts"));
    assert_eq!(0, harness.counter("html_cache_cookie_allowlisted"));
}

#[test]
fn directives_customize_and_disable_the_policy() {
    let harness = CookiesHarness::new();
    let origin = ok_origin();
    // Custom ignores extend the built-in list via config directives.
    harness
        .options
        .borrow_mut()
        .parse_and_set_1("HtmlCacheIgnoreCookie", "tracker", Scope::Server)
        .unwrap();
    harness
        .options
        .borrow_mut()
        .parse_and_set_1("HtmlCacheIgnoreCookie", "_pk_*", Scope::Server)
        .unwrap();
    let request = cookie_request("_ga=1; tracker=2; _pk_id.1.2=3");
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    assert!(session.bypass_reason().is_none());
    session.write(b"<html/>");
    let finish = session.finish(&request, START_MS + 1000);
    assert!(finish.detail_line.is_some());
    // Explicit bypass rules override both allowlists.
    harness
        .options
        .borrow_mut()
        .parse_and_set_1("HtmlCacheBypassCookie", "_ga", Scope::Server)
        .unwrap();
    let request = cookie_request("_ga=1");
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
        .is_none());
    assert!(session.bypass_reason().is_some());
    // Disabling the common list restores strict bypassing while
    // custom ignores keep working.
    harness
        .options
        .borrow_mut()
        .set_option("HtmlCacheIgnoreCommonCookies", "off", Scope::Server)
        .unwrap();
    let request = cookie_request("_gid=1");
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &request, &harness.purge, None, START_MS + 3000)
        .is_none());
    assert!(session.bypass_reason().is_some());
    let request = cookie_request("tracker=1");
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &request, &harness.purge, None, START_MS + 4000)
        .is_none());
    assert!(session.bypass_reason().is_none());
}

#[test]
fn authorization_and_origin_cookies_still_protect() {
    // Existing protections are untouched by the cookie policy.
    let harness = CookiesHarness::new();
    let origin = ok_origin();
    let mut authed = cookie_request("_ga=1");
    authed.headers_mut().add("Authorization", "Bearer x");
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &authed, &harness.purge, None, START_MS)
        .is_none());
    assert!(session.bypass_reason().is_some());
    let mut set_cookie = ok_origin();
    set_cookie.headers_mut().add("Set-Cookie", "a=1");
    set_cookie.compute_caching();
    let request = cookie_request("_ga=1");
    let mut session = harness.session();
    assert!(session
        .lookup(&set_cookie, &request, &harness.purge, None, START_MS)
        .is_none());
    assert!(session.bypass_reason().is_some());
    assert_eq!(2, harness.counter("html_cache_bypasses"));
}
