//! Driver resource API contract tests (hand-written: upstream covers
//! creation only through the context suites, which land next).
//!
//! Each case pins one `ResourceDriver` behavior those suites will
//! exercise: authorization verdicts, allow/deny/inline rules, data
//! and invalid inputs, proxy-decode retries, slot dedup, and output
//! creation with naming and size caps.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::element::NodeId;
use ps_html::filter::HtmlFilter;
use ps_html::lexer::Lexer;
use ps_html::name::Keyword;
use ps_html::output_resource::OutputResourceKind;
use ps_html::parser::{Dom, Parser};
use ps_html::resource::{InputResource, Resource};
use ps_html::server_context::{
    InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver, ServerContext,
};
use ps_html::url_segment_encoder::DefaultEncoder;
use ps_html::writer::WriterFilter;
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;

use harness::SharedSink;

fn fixture() -> (
    Rc<ResourceDriver>,
    Arc<MockUrlFetcher>,
    Rc<RefCell<RewriteOptions>>,
) {
    fixture_with_namer(None)
}

fn fixture_with_namer(
    namer: Option<Rc<dyn ps_html::url_namer::UrlNamer>>,
) -> (
    Rc<ResourceDriver>,
    Arc<MockUrlFetcher>,
    Rc<RefCell<RewriteOptions>>,
) {
    let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let fetcher = Arc::new(MockUrlFetcher::new());
    let mut server = ServerContext::new(backend, fetcher.clone(), 1_700_000_000_000);
    if let Some(namer) = namer {
        server.set_url_namer(namer);
    }
    let server = Rc::new(server);
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    let driver = Rc::new(ResourceDriver::new(
        server,
        options.clone(),
        "http://test.com/dir/page.html",
    ));
    (driver, fetcher, options)
}

fn create(driver: &Rc<ResourceDriver>, url: &str) -> (Option<InputResource>, bool) {
    driver.create_input_resource(
        &GoogleUrl::new(url),
        InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
        IntendedFor::General,
        InputRole::Style,
    )
}

#[test]
fn same_origin_authorizes() {
    let (driver, _fetcher, _options) = fixture();
    let (resource, authorized) = create(&driver, "http://test.com/dir/a.css");
    assert!(authorized);
    let resource = resource.expect("same-origin creates");
    assert_eq!(resource.url(), "http://test.com/dir/a.css");
    assert_eq!(resource.cache_key(), "http://test.com/dir/a.css");
}

#[test]
fn cross_origin_needs_inline_policy() {
    let (driver, _fetcher, _options) = fixture();
    // Strict: denied outright.
    let (resource, authorized) = create(&driver, "http://other.com/a.css");
    assert!(!authorized);
    assert!(resource.is_none());
    // Lenient: created unauthorized, under an unauth key.
    let (resource, authorized) = driver.create_input_resource(
        &GoogleUrl::new("http://other.com/a.css"),
        InlineAuthorizationPolicy::InlineUnauthorizedResources,
        IntendedFor::General,
        InputRole::Style,
    );
    assert!(!authorized);
    let resource = resource.expect("lenient creates");
    assert_eq!(resource.cache_key(), "unauth://other.com/a.css");
}

#[test]
fn disallowed_urls_refuse() {
    let (driver, _fetcher, options) = fixture();
    options
        .borrow_mut()
        .allow_resources_mut()
        .disallow("*secret*");
    let (resource, authorized) = create(&driver, "http://test.com/secret.css");
    assert!(!authorized);
    assert!(resource.is_none());
    // ... unless inlining is the intent and the inline list allows it.
    options.borrow_mut().allow_when_inlining("*secret*");
    let (resource, authorized) = driver.create_input_resource(
        &GoogleUrl::new("http://test.com/secret.css"),
        InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
        IntendedFor::Inlining,
        InputRole::Style,
    );
    assert!(authorized);
    assert!(resource.is_some());
}

#[test]
fn data_urls_skip_quietly() {
    let (driver, _fetcher, _options) = fixture();
    let (resource, authorized) = create(&driver, "data:text/css,hi");
    assert!(authorized);
    assert!(resource.is_none());
}

#[test]
fn invalid_base_refuses() {
    let (driver, _fetcher, _options) = fixture();
    drop(driver);
    let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
    let fetcher = Arc::new(MockUrlFetcher::new());
    let server = Rc::new(ServerContext::new(backend, fetcher, 1_700_000_000_000));
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    let driver = Rc::new(ResourceDriver::new(server, options, "not a url"));
    let (resource, authorized) = create(&driver, "http://test.com/a.css");
    assert!(!authorized);
    assert!(resource.is_none());
}

#[test]
fn proxy_encodings_decode_then_recheck() {
    let namer = Rc::new(ps_html::url_namer::TestUrlNamer::new());
    let (driver, _fetcher, options) = fixture_with_namer(Some(namer));
    // The proxy spelling is disallowed, but it decodes to an allowed
    // same-origin URL, which creates.
    options
        .borrow_mut()
        .allow_resources_mut()
        .disallow("*cdn.com*");
    let (resource, authorized) =
        create(&driver, "http://cdn.com/http/test.com/http/test.com/a.css");
    assert!(authorized);
    let resource = resource.expect("decoded URL creates");
    assert_eq!(
        resource.cache_key(),
        "http://cdn.com/http/test.com/http/test.com/a.css"
    );
}

#[test]
fn claimed_urls_refuse() {
    let (driver, _fetcher, _options) = fixture();
    driver.add_resource_url_claimant(Rc::new(|url: &GoogleUrl| url.spec().contains("claimed")));
    assert!(driver.is_resource_url_claimed(&GoogleUrl::new("http://test.com/claimed.css")));
    assert!(!driver.is_resource_url_claimed(&GoogleUrl::new("http://test.com/free.css")));
    let (resource, authorized) = create(&driver, "http://test.com/claimed.css");
    assert!(authorized);
    assert!(resource.is_none());
}

#[test]
fn slots_deduplicate() {
    /// Probes slot dedup from inside a parse (slots need live DOM).
    struct Probe {
        driver: Rc<ResourceDriver>,
        link: Option<NodeId>,
    }
    impl HtmlFilter for Probe {
        fn name(&self) -> &'static str {
            "SlotProbe"
        }
        fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
            if dom.element_keyword(id) == Keyword::Link {
                self.link = Some(id);
            }
        }
        fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
            let link = self.link.expect("link parsed");
            let first = self.driver.get_slot(None, dom, link, 0, "slot_test");
            let again = self.driver.get_slot(None, dom, link, 0, "slot_test");
            assert!(Rc::ptr_eq(&first, &again));
            let other = self.driver.get_slot(None, dom, link, 1, "slot_test");
            assert!(!Rc::ptr_eq(&first, &other));
        }
    }
    let (driver, _fetcher, _options) = fixture();
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(Probe { driver, link: None }));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/dir/page.html", false);
    parser.write(b"<link href=\"a.css\" src=\"b.css\"/>");
    parser.finish();
    assert_eq!(sink.bytes(), b"<link href=\"a.css\" src=\"b.css\"/>");
}

#[test]
fn output_creation_names_and_caps() {
    let (driver, _fetcher, options) = fixture();
    // Happy path: encoder leaf on the mapped base.
    let input = GoogleUrl::new("http://test.com/dir/a.css");
    let authorizing = driver
        .create_output_resource_from_resource(
            "cf",
            &DefaultEncoder,
            &make_url_for(&driver, input.spec()),
            OutputResourceKind::Rewritten,
        )
        .expect("names output");
    assert!(
        authorizing.url().starts_with("http://test.com/dir/"),
        "{}",
        authorizing.url()
    );
    assert!(
        authorizing.url().contains(".pagespeed.cf."),
        "{}",
        authorizing.url()
    );
    // Disallowed inputs fail closed with a reason.
    options
        .borrow_mut()
        .allow_resources_mut()
        .disallow("*evil*");
    let error = driver
        .create_output_resource_with_unmapped_url(
            &GoogleUrl::new("http://test.com/evil.css"),
            "cf",
            b"evil.css",
            OutputResourceKind::Rewritten,
        )
        .expect_err("disallowed fails");
    assert!(error.contains("disallowed"), "{error}");
    // Absurd size caps fail closed with a reason.
    options
        .borrow_mut()
        .set_option("MaxUrlSize", "10", ps_config::Scope::ProcessStrict)
        .unwrap();
    let error = driver
        .create_output_resource_with_unmapped_url(
            &GoogleUrl::new("http://test.com/dir/a.css"),
            "cf",
            b"a.css",
            OutputResourceKind::Rewritten,
        )
        .expect_err("oversize fails");
    assert!(error.contains("too long"), "{error}");
}

/// Builds an input handle for output-creation tests.
fn make_url_for(driver: &Rc<ResourceDriver>, url: &str) -> InputResource {
    driver
        .create_input_resource(
            &GoogleUrl::new(url),
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Style,
        )
        .0
        .expect("input creates")
}
