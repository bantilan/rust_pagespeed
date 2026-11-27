//! Cache the real selector-filter output, including its complete CSS fallback.
use ps_cache::{cache::MemoryCache, purge::PurgeSet};
use ps_config::{filter::Filter, options::RewriteOptions, scope::Scope};
use ps_html::{
    chain::{ChainContext, ChainFilter},
    critical_selector_finder::FakeCriticalSelectorFinder,
    driver::Driver,
    html_cache::HtmlCacheSession,
    server_context::{ResourceDriver, ServerContext},
    writer::ByteSink,
};
use ps_http::{
    mock::MockUrlFetcher,
    request::{Method, RequestHeaders},
    response::{HttpOptions, ResponseHeaders},
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

const NOW: i64 = 1_700_000_000_000;
const URL: &str = "https://critical.test/page";
const CHROME: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const HTML: &[u8] = b"<html><head><style>div{color:red}span{color:blue}</style></head><body><div>critical</div><span>rest</span></body></html>";

#[derive(Clone, Default)]
struct Sink(Rc<RefCell<Vec<u8>>>);
impl ByteSink for Sink {
    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        self.0.borrow_mut().extend_from_slice(bytes);
        true
    }
}
struct Fixture {
    driver: Rc<ResourceDriver>,
    options: Rc<RefCell<RewriteOptions>>,
    finder: Rc<FakeCriticalSelectorFinder>,
    request: RequestHeaders,
    origin: ResponseHeaders,
    purge: PurgeSet,
}
impl Fixture {
    fn new(selectors: Option<&[&str]>) -> Self {
        let server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(MockUrlFetcher::new()),
            NOW,
        ));
        let finder = Rc::new(FakeCriticalSelectorFinder::with_selectors(
            selectors.unwrap_or(&[]),
        ));
        if selectors.is_some() {
            server.set_critical_selector_finder(finder.clone());
        }
        let mut opts = RewriteOptions::new();
        for name in ["HtmlCache", "HtmlCacheEarlyLookup", "ShowCacheTimestamp"] {
            opts.set_option(name, "on", Scope::Server).unwrap();
        }
        opts.set_option("HtmlCacheTtlSec", "60", Scope::Server)
            .unwrap();
        opts.filters_mut().enable(Filter::PrioritizeCriticalCss);
        let options = Rc::new(RefCell::new(opts));
        let driver = Rc::new(ResourceDriver::new(server, options.clone(), URL));
        let mut request = RequestHeaders::new();
        request.set_method(Method::Get);
        request.set_version(1, 1);
        request.headers_mut().add("Host", "critical.test");
        request.headers_mut().add("User-Agent", CHROME);
        request
            .headers_mut()
            .add("Accept", "text/html,image/avif,image/webp");
        driver.set_user_agent(CHROME);
        driver.set_request_headers(request.clone());
        let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        origin.set_date_and_caching(NOW, 60_000, ", public");
        origin.compute_caching();
        Self {
            driver,
            options,
            finder,
            request,
            origin,
            purge: PurgeSet::new(25000),
        }
    }
    fn session(&self) -> HtmlCacheSession {
        HtmlCacheSession::new(self.driver.clone(), URL)
    }
    fn render(&self) -> Vec<u8> {
        let sink = Sink::default();
        let mut parser = Driver::new(
            &self.options.borrow(),
            &ChainContext::new(true, true),
            sink.clone(),
            Some(self.driver.clone()),
        );
        assert!(!parser
            .skipped_slots()
            .iter()
            .any(|(_, slot)| *slot == ChainFilter::CriticalSelector));
        parser.start_parse(URL.as_bytes(), false);
        parser.write(HTML);
        parser.finish();
        let out = sink.0.borrow().clone();
        out
    }
    fn publish(&self) -> Vec<u8> {
        let mut cache = self.session();
        assert!(cache
            .lookup(&self.origin, &self.request, &self.purge, None, NOW)
            .is_none());
        assert!(
            cache.bypass_reason().is_none(),
            "critical CSS must be cache eligible"
        );
        let output = self.render();
        cache.write(&output);
        assert!(cache.finish(&self.request, NOW + 1).detail_line.is_some());
        output
    }
}

#[test]
fn learned_output_preserves_critical_and_full_css_on_normal_and_early_hits() {
    let f = Fixture::new(Some(&["div"]));
    let output = f.publish();
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("<style>div{color:red}</style>"), "{text}");
    assert!(text.contains("<noscript"), "{text}");
    assert!(
        text.contains("span{color:blue}"),
        "full CSS must be retained: {text}"
    );
    for t in [2, 30000] {
        assert_eq!(
            f.session()
                .lookup(&f.origin, &f.request, &f.purge, None, NOW + t)
                .unwrap()
                .body,
            output
        );
        assert_eq!(
            f.session()
                .lookup_early(&f.request, &f.purge, None, NOW + t)
                .unwrap()
                .0
                .body,
            output
        );
    }
}

#[test]
fn missing_selectors_cache_full_styles_then_learned_evidence_rekeys_both_paths() {
    let f = Fixture::new(None);
    assert_eq!(f.publish(), HTML);
    assert!(f
        .session()
        .lookup_early(&f.request, &f.purge, None, NOW + 2)
        .is_some());
    f.finder.set_selectors(&["span"]);
    f.driver
        .server_context()
        .set_critical_selector_finder(f.finder.clone());
    assert!(f
        .session()
        .lookup_early(&f.request, &f.purge, None, NOW + 3)
        .is_none());
    let output = f.publish();
    assert!(
        String::from_utf8_lossy(&output).contains("<style>span{color:#00f}</style>"),
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(
        f.session()
            .lookup(&f.origin, &f.request, &f.purge, None, NOW + 4)
            .unwrap()
            .body,
        output
    );
    // Selector order is not evidence: equivalent sets preserve a HIT.
    f.finder.set_selectors(&["span", "div"]);
    f.publish();
    f.finder.set_selectors(&["div", "span"]);
    assert!(f
        .session()
        .lookup_early(&f.request, &f.purge, None, NOW + 5)
        .is_some());
}

#[test]
fn evidence_changed_during_render_never_publishes_under_old_key() {
    let f = Fixture::new(Some(&["div"]));
    let mut cache = f.session();
    assert!(cache
        .lookup(&f.origin, &f.request, &f.purge, None, NOW)
        .is_none());
    f.finder.set_selectors(&["span"]);
    let output = f.render();
    // The filter must use the captured lookup input, not the newer set.
    assert!(String::from_utf8_lossy(&output).contains("<style>div{color:red}</style>"));
    cache.write(&output);
    let finish = cache.finish(&f.request, NOW + 1);
    assert!(finish.detail_line.is_none());
    assert!(String::from_utf8_lossy(&finish.show_comment.unwrap())
        .contains("critical-css-evidence-changed"));
    assert!(f
        .session()
        .lookup_early(&f.request, &f.purge, None, NOW + 2)
        .is_none());
}

#[test]
fn sampling_and_private_requests_still_cannot_publish_or_reuse_css_html() {
    let f = Fixture::new(Some(&["div"]));
    let mut sampled = f.session();
    sampled.lookup(&f.origin, &f.request, &f.purge, None, NOW);
    sampled.write(b"<script>CriticalCssBeacon</script>");
    assert!(sampled.finish(&f.request, NOW + 1).detail_line.is_none());
    f.publish();
    for (name, value) in [
        ("Cookie", "wordpress_logged_in_x=private"),
        ("Authorization", "Bearer private"),
    ] {
        let mut private = f.request.clone();
        private.headers_mut().add(name, value);
        assert!(f
            .session()
            .lookup(&f.origin, &private, &f.purge, None, NOW + 2)
            .is_none());
        assert!(f
            .session()
            .lookup_early(&private, &f.purge, None, NOW + 2)
            .is_none());
    }
    f.driver
        .set_user_agent("Mozilla/5.0 (compatible; MSIE 7.0; Windows NT 6.0)");
    assert!(f
        .session()
        .lookup(&f.origin, &f.request, &f.purge, None, NOW + 3)
        .is_none());
}
