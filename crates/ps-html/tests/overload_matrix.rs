//! Overload-as-miss matrix: every backend consumer must treat a contended
//! (`Overload`) lookup exactly like a miss — fall through, re-render, or
//! refetch — never as success, corruption, or a negative decision.
//!
//! The shared tier reports `Overload` (rather than blocking) whenever a
//! shard lock is contended or an epoch saturates, so this contract is what
//! keeps contention from corrupting higher layers.
use ps_cache::cache::{Cache, KeyState, Lookup, MemoryCache};
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::beacon_finder::{BeaconFinder, EvidenceLookup};
use ps_html::chain::ChainContext;
use ps_html::driver::Driver;
use ps_html::html_cache::HtmlCacheSession;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::http_cache::{FindStatus, HttpCache};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::{Method, RequestContext, RequestHeaders};
use ps_http::response::{HttpOptions, ResponseHeaders, VaryOption};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const START_MS: i64 = 1_700_000_000_000;

/// Backend that reports every lookup contended.
#[derive(Debug)]
struct OverloadCache;

impl Cache for OverloadCache {
    fn get(&self, _key: &str) -> Lookup {
        Lookup::miss(KeyState::Overload)
    }
    fn put(&self, _key: &str, _value: &[u8]) {}
    fn delete(&self, _key: &str) {}
    fn name(&self) -> String {
        "OverloadStub".to_string()
    }
}

#[derive(Clone, Default)]
struct SharedSink {
    output: Rc<RefCell<Vec<u8>>>,
}

impl ps_html::writer::ByteSink for SharedSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        self.output.borrow_mut().extend_from_slice(bytes);
        true
    }
}

fn counter(server: &ServerContext, name: &str) -> i64 {
    server.variable(name).get()
}

#[test]
fn http_cache_overload_is_not_found() {
    let cache = HttpCache::new(Arc::new(OverloadCache));
    let mut request = RequestContext::new(HttpOptions::default());
    let found = cache.find(
        "http://o.test/style.css",
        "",
        Default::default(),
        VaryOption::IgnoreVaryOnResources,
        &|_| true,
        0,
        &mut request,
        START_MS,
    );
    assert_eq!(found.status, FindStatus::NotFound);
}

#[test]
fn html_cache_overload_is_miss_not_bypass() {
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(OverloadCache),
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
    let driver = Rc::new(ResourceDriver::new(
        server.clone(),
        Rc::clone(&options),
        "http://o.test/page.html",
    ));
    let mut request = RequestHeaders::new();
    request.set_method(Method::Get);
    request.set_version(1, 1);
    request.headers_mut().add("Host", "o.test");
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
    let purge = ps_cache::purge::PurgeSet::new(25_000);
    let mut session = HtmlCacheSession::new(driver, "http://o.test/page.html");
    assert!(session
        .lookup(&origin, &request, &purge, None, START_MS)
        .is_none());
    // Miss path taken (a bypass would not bump misses).
    assert_eq!(1, counter(&server, "html_cache_misses"));
    assert_eq!(0, counter(&server, "html_cache_hits"));
}

#[test]
fn beacon_overload_is_missing_not_corrupt() {
    let stub: Arc<dyn Cache> = Arc::new(OverloadCache);
    let finder = BeaconFinder::new(&stub);
    assert!(matches!(
        finder.load("http://o.test/page.html", "options", "@Desktop"),
        EvidenceLookup::Missing
    ));
}

#[test]
fn partition_overload_rewrites_from_miss() {
    // End-to-end through a CSS rewrite: overloaded metadata reads as a
    // metadata miss, so the rewrite still proceeds (nothing corrupts,
    // nothing is skipped).
    let css_url = "http://o.test/a.css";
    let fetcher = MockUrlFetcher::new();
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    headers.set_first_line(1, 1, 200, "OK");
    headers.headers_mut().add("Content-Type", "text/css");
    headers.set_date_and_caching(START_MS, 300_000, "");
    headers.compute_caching();
    fetcher.set_response(css_url, headers, b"div  {  display  :  block  }  /* tint */");
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(OverloadCache),
        Arc::new(fetcher),
        START_MS,
    ));
    let mut options = RewriteOptions::new();
    options.filters_mut().enable(Filter::RewriteCss);
    let options = Rc::new(RefCell::new(options));
    let resource_driver = Rc::new(ResourceDriver::new(
        server.clone(),
        Rc::clone(&options),
        "http://o.test/page.html",
    ));
    let sink = SharedSink::default();
    let context = ChainContext::new(true, true);
    let mut driver = Driver::new(
        &options.borrow(),
        &context,
        sink.clone(),
        Some(resource_driver),
    );
    driver.start_parse(b"http://o.test/page.html", false);
    driver.write(
        format!("<html><head><link rel='stylesheet' href='{css_url}'></head><body></body></html>")
            .as_bytes(),
    );
    driver.finish();
    let out = String::from_utf8_lossy(&sink.output.borrow()).into_owned();
    assert!(out.contains(".pagespeed."), "{out}");
    assert_eq!(1, counter(&server, "rewrite_cached_output_misses"));
}
