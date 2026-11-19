//! Google-Font-CSS-inlining tests ported from
//! `google_font_css_inline_filter_test.cc`.
//!
//! NOTE: `LoadParallel` waits for the async reconstruction epic;
//! the sync port cannot hold two loads pending at once.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::filter::HtmlFilter;
use ps_html::filters::css_inline::CssInlineFilter;
use ps_html::filters::css_inline_import_to_link::{
    CssInlineImportToLinkConfig, CssInlineImportToLinkFilter,
};
use ps_html::filters::google_font_css_inline::GoogleFontCssInlineFilter;
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;
const ROBOTO: &str = "http://fonts.googleapis.com/css?family=Roboto";

/// UA-sensitive fetcher wrapper (`UserAgentSensitiveTestFetcher`):
/// refuses unauthorized origins, then serves the mock URL with the
/// request UA appended.
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
    session_origins: RefCell<Vec<String>>,
}

impl Fixture {
    fn new() -> Self {
        let backend = Arc::new(MemoryCache::new());
        let metadata = Arc::new(MemoryCache::new());
        let mock = Arc::new(MockUrlFetcher::new());
        let server = ServerContext::new_with_metadata(
            backend,
            metadata,
            Rc::new(UaSensitiveFetcher { base: mock.clone() }) as Rc<dyn AsyncFetcher>,
            START_MS,
        );
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::InlineGoogleFontCss);
        // Debug on for the font filter's explanation comments (but
        // without the standard debug-filter output).
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::Debug);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher: mock,
            user_agent: RefCell::new(String::new()),
            session_origins: RefCell::new(Vec::new()),
        }
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.set_user_agent(&self.user_agent.borrow());
        for origin in self.session_origins.borrow().iter() {
            driver
                .request_context()
                .borrow_mut()
                .add_session_authorized_fetch_origin(origin);
        }
        driver
    }

    /// Installs the UA and scripts the UA-specific font CSS
    /// (`ResetUserAgent`).
    fn reset_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
        // Font loader CSS gets Cache-Control:private, max-age=86400
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("text/css"),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), 86400 * 1000, ", private");
        headers.compute_caching();
        // Now upload some UA-specific CSS where UaSensitiveFetcher will
        // find it, for two fake UAs: Chromezilla and Safieri, used
        // since they are short, unlike real UA strings.
        self.fetcher.set_response(
            &format!("{ROBOTO}&UA=Chromezilla"),
            headers.clone(),
            b"font_chromezilla",
        );
        self.fetcher.set_response(
            &format!("{ROBOTO}&UA=Safieri"),
            headers.clone(),
            b"font_safieri",
        );
        // If other filters will try to fetch this, they won't have a UA.
        self.fetcher
            .set_response(&format!("{ROBOTO}&UA=unknown"), headers, b"font_huh");
    }

    /// Parses through the font filter.
    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(GoogleFontCssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }
}

/// Unquoted CSS link (`CssLinkHref`).
fn css_link_href(url: &str) -> String {
    format!("<link rel=stylesheet href={url}>")
}

#[test]
fn basic_operation() {
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}simple"), &css_link_href(ROBOTO)),
        "<style>font_chromezilla</style>"
    );

    // Different UAs get different cache entries
    fixture.reset_user_agent("Safieri");
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}simple2"), &css_link_href(ROBOTO)),
        "<style>font_safieri</style>"
    );
}

#[test]
fn usage_restrictions() {
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");

    fixture
        .options
        .borrow_mut()
        .set_modify_caching_headers(false);
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}incompat1"), &css_link_href(ROBOTO)),
        format!(
            "{}<!--Cannot inline font loader CSS when ModifyCachingHeaders is off-->",
            css_link_href(ROBOTO)
        )
    );

    fixture
        .options
        .borrow_mut()
        .set_modify_caching_headers(true);
    fixture
        .options
        .borrow_mut()
        .set_downstream_cache_purge_location_prefix("foo");
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}incompat2"), &css_link_href(ROBOTO)),
        format!(
            "{}<!--Cannot inline font loader CSS when using downstream cache-->",
            css_link_href(ROBOTO)
        )
    );
}

#[test]
fn protocol_relative() {
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}proto_rel"),
            &css_link_href("//fonts.googleapis.com/css?family=Roboto")
        ),
        "<style>font_chromezilla</style>"
    );
}

#[test]
fn size_limit() {
    let fixture = Fixture::new();
    // GoogleFontCssInlineFilter uses google_font_css_inline_max_bytes.
    // Set a threshold at font_safieri, which should prevent longer
    // font_chromezilla from inlining.
    fixture
        .options
        .borrow_mut()
        .set_google_font_css_inline_max_bytes("font_safieri".len() as i64);
    fixture.reset_user_agent("Chromezilla");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}slightly_long"),
            &css_link_href(ROBOTO)
        ),
        format!(
            "{}<!--CSS not inlined since it&#39;s bigger than 12 bytes-->",
            css_link_href(ROBOTO)
        )
    );

    fixture.reset_user_agent("Safieri");
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}short"), &css_link_href(ROBOTO)),
        "<style>font_safieri</style>"
    );
}

#[test]
fn via_inline_import() {
    // Tests to make sure that if InlineImportToLink filter is on we
    // convert a <style>@import'ing </style> this as well.
    let fixture = Fixture::new();
    fixture.reset_user_agent("Chromezilla");
    let import = format!("<style>@import \"{ROBOTO}\";</style>");
    let parse = |fixture: &Fixture, html: &str| {
        let driver = fixture.driver_for(&format!("{TEST_DOMAIN}import"));
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssInlineImportToLinkFilter::new(
            CssInlineImportToLinkConfig {
                xhtml: ps_html::filters::common::XhtmlStatus::IsNotXhtml,
            },
        )) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(GoogleFontCssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        let url = format!("{TEST_DOMAIN}import");
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    };
    assert_eq!(parse(&fixture, &import), "<style>font_chromezilla</style>");

    fixture.reset_user_agent("Safieri");
    assert_eq!(parse(&fixture, &import), "<style>font_safieri</style>");
}

#[test]
fn with_wide_authorization() {
    let fixture = Fixture::new();
    // Check that we don't rely solely on authorization to properly
    // dispatch the URL to us.
    fixture.options.borrow_mut().add_domain("*");
    fixture
        .session_origins
        .borrow_mut()
        .push("http://fonts.googleapis.com".to_string());
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineCss);
    fixture.reset_user_agent("Chromezilla");
    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}with_domain_*"));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(
        Box::new(GoogleFontCssInlineFilter::new(driver.clone())) as Box<dyn HtmlFilter>
    );
    parser.add_filter(Box::new(CssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let url = format!("{TEST_DOMAIN}with_domain_*");
    parser.start_parse(url.as_bytes(), false);
    parser.write(css_link_href(ROBOTO).as_bytes());
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        "<style>font_chromezilla</style>"
    );
}

#[test]
fn with_wide_authorization_without_font_filter() {
    // Since font inlining isn't on, the regular inliner complains. This isn't
    // ideal, but doing otherwise requires inline_css to know about
    // inline_google_font_css, which also seems suboptimal.
    let fixture = Fixture::new();
    fixture.options.borrow_mut().add_domain("*");
    fixture
        .session_origins
        .borrow_mut()
        .push("http://fonts.googleapis.com".to_string());
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineCss);
    fixture.reset_user_agent("Chromezilla");
    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}with_domain_*_without_font_filter"));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(CssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let url = format!("{TEST_DOMAIN}with_domain_*_without_font_filter");
    parser.start_parse(url.as_bytes(), false);
    parser.write(css_link_href(ROBOTO).as_bytes());
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        format!(
            "{}<!--Uncacheable content, preventing rewriting of {ROBOTO}-->",
            css_link_href(ROBOTO)
        )
    );
}
