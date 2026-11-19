//! Critical-selector tests ported from
//! `critical_selector_filter_test.cc`.
//!
//! NOTE: `ValidateRewriterLogging` asserts are dropped throughout
//! (logging waits for that subsystem). NOTE:
//! `do_not_lazy_load_if_nothing_rewritten` is deferred: it stages
//! a pending fetch via a wait fetcher, which has no sync-model
//! equivalent. NOTE: producer-first chain orders (`RewriteCss`,
//! `CombineCss`) stand in for C++ async interleaving, where the
//! selector filter runs first but renders after those rewrites
//! land; observable outputs match. NOTE: the beacon-backed
//! finder waits for property-cache/beacon; tests install the
//! fake with the C++ beaconed sets.
#[path = "harness.rs"]
mod harness;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::filter::Filter;
use ps_config::options::{Category, RewriteOptions};
use ps_html::critical_selector_finder::FakeCriticalSelectorFinder;
use ps_html::filter::HtmlFilter;
use ps_html::filters::critical_selector::{
    CriticalSelectorFilter, NOSCRIPT_STYLES_CLASS, NUM_CSS_NOT_USED, NUM_CSS_USED,
};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::writer::WriterFilter;
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;
const IE7_USER_AGENT: &str = "Mozilla/5.0 (Windows; U; MSIE 7.0; Windows NT 6.0; en-US)";

/// Extra chain filters, in order, around the selector filter.
#[derive(Clone, Copy)]
enum Chain {
    SelectorOnly,
    CssThenSelector,
    CombinerThenSelector,
    SelectorThenInline,
}

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    finder: Rc<FakeCriticalSelectorFinder>,
    user_agent: RefCell<String>,
    xhtml: Cell<bool>,
}

impl Fixture {
    fn new() -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend,
            metadata_backend,
            fetcher.clone(),
            START_MS,
        );
        server.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        let server = Rc::new(server);
        let finder = Rc::new(FakeCriticalSelectorFinder::with_selectors(&["div", "*"]));
        server.set_critical_selector_finder(finder.clone());
        let fixture = Fixture {
            server,
            options: Rc::new(RefCell::new(RewriteOptions::new())),
            fetcher,
            finder,
            user_agent: RefCell::new(String::new()),
            xhtml: Cell::new(false),
        };
        fixture.set_up();
        fixture
    }

    /// Scripts the shared stylesheets (`SetUp`).
    fn set_up(&self) {
        self.set_response_with_default_headers(
            "a.css",
            "text/css",
            b"div,span,*::first-letter { display: block; }p { display: inline; }",
            100,
        );
        self.set_response_with_default_headers(
            "b.css",
            "text/css",
            b"@media screen,print { * { margin: 0px; } }",
            100,
        );
        self.set_response_with_default_headers(
            "http://unauthorized.com/unauth.css",
            "text/css",
            b"div { background-color: blue }random { color: white }",
            100,
        );
    }

    fn set_response_with_default_headers(
        &self,
        url: &str,
        content_type: &str,
        body: &[u8],
        ttl_sec: i64,
    ) {
        let url = if url.contains("://") {
            url.to_string()
        } else {
            format!("{TEST_DOMAIN}{url}")
        };
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type(content_type),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), ttl_sec * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(&url, headers, body);
    }

    /// Installs the UA (`SetCurrentUserAgent`).
    fn set_current_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
    }

    /// Selects the MIME type (`SetHtmlMimetype`/`SetXhtmlMimetype`).
    fn set_xhtml(&self, xhtml: bool) {
        self.xhtml.set(xhtml);
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_is_xhtml(self.xhtml.get());
        driver
    }

    /// Parses through a filter chain (`ValidateExpected` compares
    /// verbatim: `AddHtmlTags` is off in C++).
    fn parse_chain(&self, url: &str, html: &str, chain: Chain) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        let mut selector =
            || Box::new(CriticalSelectorFilter::new(driver.clone())) as Box<dyn HtmlFilter>;
        match chain {
            Chain::SelectorOnly => {
                parser.add_filter(selector());
            }
            Chain::CssThenSelector => {
                parser.add_filter(
                    Box::new(ps_html::filters::css::CssFilter::new(driver.clone()))
                        as Box<dyn HtmlFilter>,
                );
                parser.add_filter(selector());
            }
            Chain::CombinerThenSelector => {
                parser.add_filter(
                    Box::new(ps_html::filters::css_combine::CssCombineFilter::new(
                        driver.clone(),
                    )) as Box<dyn HtmlFilter>,
                );
                parser.add_filter(selector());
            }
            Chain::SelectorThenInline => {
                parser.add_filter(selector());
                parser.add_filter(Box::new(ps_html::filters::css_inline::CssInlineFilter::new(
                    driver.clone(),
                )) as Box<dyn HtmlFilter>);
            }
        }
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    fn parse(&self, url: &str, html: &str) -> String {
        self.parse_chain(url, html, Chain::SelectorOnly)
    }

    /// Case comparison (`ValidateExpected`).
    fn validate_expected(&self, case_id: &str, html_in: &str, expected: &str) {
        let url = format!("{TEST_DOMAIN}{case_id}.html");
        assert_eq!(self.parse(&url, html_in), expected, "test id:{case_id}");
    }

    /// Unchanged comparison (`ValidateNoChanges`).
    fn validate_no_changes(&self, case_id: &str, html: &str) {
        self.validate_expected(case_id, html, html);
    }

    /// Stylesheet link (`CssLinkHref`).
    fn css_link_href(url: &str) -> String {
        format!("<link rel=stylesheet href={url}>")
    }

    /// Stylesheet link with media (`CssLinkHrefMedia`).
    fn css_link_href_media(url: &str, media: &str) -> String {
        format!("<link rel=stylesheet href={url} media=\"{media}\">")
    }

    /// Noscript-wrapped originals (`WrapForJsLoad`).
    fn wrap_for_js_load(css: &str) -> String {
        format!("<noscript class=\"{NOSCRIPT_STYLES_CLASS}\">{css}</noscript>")
    }

    /// Loader script (`JsLoader`).
    fn js_loader() -> String {
        let js = get_asset(StaticAsset::CriticalCssLoaderJs, false);
        format!(
            "<script data-pagespeed-no-defer type=\"text/javascript\">{}pagespeed.CriticalCssLoader.Run();</script>",
            String::from_utf8_lossy(js),
        )
    }

    /// Bundle plus loader (`LoadRestOfCss`).
    fn load_rest_of_css(css: &str) -> String {
        format!("{}{}", Self::wrap_for_js_load(css), Self::js_loader())
    }
}

#[test]
fn basic_operation() {
    let fixture = Fixture::new();
    let css = format!(
        "<style>*,p {{display: none; }} span {{display: inline; }}</style>{}{}",
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = concat!(
        "<style>*{display:none}</style>",
        "<style>div,*::first-letter{display:block}</style>",
        "<style>@media screen{*{margin:0}}</style>",
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "basic",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
}

#[test]
fn unauthorized_css() {
    let fixture = Fixture::new();
    let css = format!(
        "<style>*,p {{display: none; }} span {{display: inline; }}</style>{}{}{}",
        Fixture::css_link_href("http://unauthorized.com/unauth.css"),
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = format!(
        "{}{}{}{}",
        "<style>*{display:none}</style>",
        Fixture::css_link_href("http://unauthorized.com/unauth.css"),
        "<style>div,*::first-letter{display:block}</style>",
        "<style>@media screen{*{margin:0}}</style>",
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "basic",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
    assert_eq!(fixture.server.variable(NUM_CSS_USED).get(), 3);
    assert_eq!(fixture.server.variable(NUM_CSS_NOT_USED).get(), 1);
}

#[test]
fn allow_unauthorized_css() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_inline_unauthorized_resource_type(Category::Stylesheet);
    let css = format!(
        "<style>*,p {{display: none; }} span {{display: inline; }}</style>{}{}{}",
        Fixture::css_link_href("http://unauthorized.com/unauth.css"),
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = concat!(
        "<style>*{display:none}</style>",
        "<style>div{background-color:#00f}</style>",
        "<style>div,*::first-letter{display:block}</style>",
        "<style>@media screen{*{margin:0}}</style>",
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "basic",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
    assert_eq!(fixture.server.variable(NUM_CSS_USED).get(), 4);
    assert_eq!(fixture.server.variable(NUM_CSS_NOT_USED).get(), 0);
}

#[test]
fn styles_in_body() {
    let fixture = Fixture::new();
    let css_a = format!(
        "<style>*,p {{display: none; }} span {{display: inline; }}</style>{}",
        Fixture::css_link_href("a.css"),
    );
    let css_b = Fixture::css_link_href("b.css");
    let critical_css_a = concat!(
        "<style>*{display:none}</style>",
        "<style>div,*::first-letter{display:block}</style>",
    );
    let critical_css_b = "<style>@media screen{*{margin:0}}</style>";
    let html = format!("<head></head><body>{css_a}<div>Stuff</div>{css_b}</body>");
    fixture.validate_expected(
        "style_in_body",
        &html,
        &format!(
            "<head></head><body>{critical_css_a}<div>Stuff</div>{critical_css_b}{}</body>",
            Fixture::load_rest_of_css(&format!("{css_a}{css_b}")),
        ),
    );
}

#[test]
fn escaped_closing_style_tag_is_inlined_verbatim_in_html() {
    // String escapes are preserved verbatim by serialization, so
    // an escaped closing tag is safe inside an HTML <style>
    // element and stays inlined.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(
        "c.css",
        "text/css",
        b"div{content:\\\"\\\\3C/style\\\\3E\\\"}",
        100,
    );
    let out = fixture.parse(
        &format!("{TEST_DOMAIN}escaped_closing_style_html.html"),
        &format!(
            "<head>{}</head><body><div>Stuff</div></body>",
            Fixture::css_link_href("c.css"),
        ),
    );
    assert!(
        out.contains("<style>div{content:\\\"\\\\3C/style\\\\3E\\\"}</style>"),
        "{out}"
    );
}

#[test]
fn less_than_in_kept_rule_keeps_the_link_in_xhtml() {
    // In XHTML a kept rule containing '<' keeps the link: the
    // conservative check applies to the whole document.
    let fixture = Fixture::new();
    fixture.set_xhtml(true);
    fixture.set_response_with_default_headers("c.css", "text/css", b"div{content:\"<main\"}", 100);
    fixture.validate_no_changes(
        "lt_kept_rule_xhtml",
        &format!(
            "<head>{}</head><body><div>Stuff</div></body>",
            Fixture::css_link_href("c.css"),
        ),
    );
}

#[test]
fn empty_block() {
    // Do not insert empty <style> blocks. Our critical selector
    // sets do not talk about 'i' so this should do nothing.
    let fixture = Fixture::new();
    let css = "<style>i { font-style:italic; }</style>";
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "empty_block",
        &html,
        &format!(
            "<head></head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(css),
        ),
    );
}

#[test]
fn disabled_for_ie() {
    let fixture = Fixture::new();
    fixture.set_current_user_agent(IE7_USER_AGENT);
    let css = format!(
        "<style>*,p {{display: none; }} span {{display: inline; }}</style>{}{}",
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_no_changes("on_ie", &html);
}

#[test]
fn no_script() {
    let fixture = Fixture::new();
    let css1 = "<style>*,p {display: none; } span {display: inline; }</style>";
    let css2 = format!("<noscript>{}</noscript>", Fixture::css_link_href("a.css"),);
    let css3 = Fixture::css_link_href("b.css");
    let css = format!("{css1}{css2}{css3}");
    let critical_css = concat!(
        "<style>*{display:none}</style>",
        "<noscript></noscript>",
        "<style>@media screen{*{margin:0}}</style>",
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "noscript",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}{}{}{}</body>",
            Fixture::wrap_for_js_load(css1),
            css2,
            Fixture::wrap_for_js_load(&css3),
            Fixture::js_loader(),
        ),
    );
}

#[test]
fn alternate() {
    let fixture = Fixture::new();
    let css = format!(
        "<link rel=\"alternate stylesheet\" href=\"a.css\">{}",
        Fixture::css_link_href("b.css"),
    );
    let critical_css = "<style>@media screen{*{margin:0}}</style>";
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "alternate",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
}

#[test]
fn media() {
    let fixture = Fixture::new();
    let css = format!(
        "<style media=screen,print>*,p {{display: none; }} span {{display: inline; }}</style>{}{}",
        Fixture::css_link_href_media("a.css", "screen"),
        Fixture::css_link_href_media("b.css", "screen and (color), aural"),
    );
    let critical_css = concat!(
        "<style media=\"screen\">*{display:none}</style>",
        "<style media=\"screen\">div,*::first-letter{display:block}</style>",
        "<style media=\"screen and (color)\">@media screen{*{margin:0}}</style>",
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "foo",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
}

#[test]
fn non_screen_media() {
    let fixture = Fixture::new();
    let css = format!(
        "<style media=print>*,p {{display: none; }} span {{display: inline; }}</style>{}{}",
        Fixture::css_link_href_media("a.css", "screen"),
        Fixture::css_link_href_media("b.css", "screen and (color), aural"),
    );
    let critical_css = concat!(
        "<style media=\"screen\">div,*::first-letter{display:block}</style>",
        "<style media=\"screen and (color)\">@media screen{*{margin:0}}</style>",
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    fixture.validate_expected(
        "non_screen_media",
        &html,
        &format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
}

#[test]
fn same_css_different_selectors() {
    // We should not reuse results for same CSS when selectors are
    // different. (Sync NOTE: summaries recompute per parse, so
    // the pcache-entry clearing in C++ has no counterpart.)
    let fixture = Fixture::new();
    let css = "<style>div,span { display: inline-block; }</style>";
    // Check what we compute for a page with div.
    fixture.validate_expected(
        "with_div",
        &format!("{css}<div>Foo</div>"),
        &format!(
            "<style>div{{display:inline-block}}</style><div>Foo</div>{}",
            Fixture::load_rest_of_css(css),
        ),
    );
    // Update the selector list with span: both div and span are
    // now in the critical set.
    fixture.finder.set_selectors(&["div", "span", "*"]);
    fixture.validate_expected(
        "with_div_span",
        &format!("{css}<span>Foo</span>"),
        &format!(
            "<style>div,span{{display:inline-block}}</style><span>Foo</span>{}",
            Fixture::load_rest_of_css(css),
        ),
    );
    // Only span is left.
    fixture.finder.set_selectors(&["span"]);
    fixture.validate_expected(
        "with_span",
        &format!("{css}<span>Foo</span>"),
        &format!(
            "<style>span{{display:inline-block}}</style><span>Foo</span>{}",
            Fixture::load_rest_of_css(css),
        ),
    );
}

#[test]
fn retain_pseudo_only() {
    // Make sure we handle things like :hover OK.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(
        "c.css",
        "text/css",
        b":hover { border: 2px solid red; }",
        100,
    );
    let link = Fixture::css_link_href("c.css");
    fixture.validate_expected(
        "hover",
        &link,
        &format!(
            "<style>:hover{{border:2px solid red}}</style>{}",
            Fixture::load_rest_of_css(&link),
        ),
    );
}

#[test]
fn retain_unparseable() {
    // Make sure we keep unparseable fragments around,
    // particularly when the problem is with the selector, as
    // well as with the entire region.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(
        "c.css",
        "text/css",
        b"!huh! {background: white; } @huh { display: block; }",
        100,
    );
    let link = Fixture::css_link_href("c.css");
    fixture.validate_expected(
        "partly_unparseable",
        &link,
        &format!(
            "<style>!huh! {{background:#fff}}@huh {{ display: block; }}</style>{}",
            Fixture::load_rest_of_css(&link),
        ),
    );
}

#[test]
fn no_selector_info() {
    // We shouldn't change things when there is no info on
    // selectors available.
    let fixture = Fixture::new();
    fixture.finder.set_selectors(&[]);
    let css = "<style>div,span { display: inline-block; }</style>";
    let html = format!("{css}<div>Foo</div>");
    fixture.validate_no_changes("no_sel_info", &html);
    fixture.validate_no_changes("no_sel_info", &html);
}

#[test]
fn resolve_urls_properly() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(
        "dir/c.css",
        "text/css",
        b"* { background-image: url(d.png); }",
        100,
    );
    let link = Fixture::css_link_href("dir/c.css");
    fixture.validate_expected(
        "rel_path",
        &link,
        &format!(
            "<style>*{{background-image:url(dir/d.png)}}</style>{}",
            Fixture::load_rest_of_css(&link),
        ),
    );
}

#[test]
fn properly_used_optimized() {
    // Make sure that the lazy loading code for rest of CSS
    // actually uses optimized resources. (Sync NOTE: the CSS
    // filter runs first so its rewrites are visible.)
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteCss);
    let css = format!(
        "{}{}",
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = concat!(
        "<style>div,*::first-letter{display:block}</style>",
        "<style>@media screen{*{margin:0}}</style>",
    );
    let optimized_css = format!(
        "{}{}",
        Fixture::css_link_href("A.a.css.pagespeed.cf.0.css"),
        Fixture::css_link_href("A.b.css.pagespeed.cf.0.css"),
    );
    let html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    let url = format!("{TEST_DOMAIN}with_rewrite_css.html");
    assert_eq!(
        fixture.parse_chain(&url, &html, Chain::CssThenSelector),
        format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&optimized_css),
        ),
    );
}

#[test]
fn fragment_reference_kept_on_rewrite_domain() {
    // A fragment-only reference stays as written in critical
    // rules inlined from a stylesheet in another directory, with
    // a rewrite domain set.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://cdn.com", TEST_DOMAIN);
    fixture.set_response_with_default_headers(
        "dir/f.css",
        "text/css",
        b"div { mask: url(#m); }",
        100,
    );
    let html = format!(
        "<head>{}</head><body><div>Stuff</div></body>",
        Fixture::css_link_href("dir/f.css"),
    );
    fixture.validate_expected(
        "fragment",
        &html,
        &format!(
            "<head><style>div{{mask:url(#m)}}</style></head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&Fixture::css_link_href("dir/f.css")),
        ),
    );
}

#[test]
fn combiner_interaction() {
    // Only one <style> element since combine_css ran before us.
    // (Sync NOTE: the combiner runs first so its combination is
    // visible.)
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::CombineCss);
    let css = format!(
        "{}{}",
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = concat!(
        "<style>div,*::first-letter{display:block}",
        "@media screen{*{margin:0}}</style>",
    );
    let combined_url = combined_leaf(&["a.css", "b.css"], "0");
    let url = format!("{TEST_DOMAIN}with_combiner.html");
    assert_eq!(
        fixture.parse_chain(&url, &css, Chain::CombinerThenSelector),
        format!(
            "{critical_css}{}",
            Fixture::load_rest_of_css(&Fixture::css_link_href(&combined_url)),
        ),
    );
}

#[test]
fn resolve_when_combine_across_paths() {
    // Make sure we get proper URL resolution when doing
    // combine-across-paths. (Sync NOTE: combiner first.)
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::CombineCss);
    fixture.set_response_with_default_headers(
        "dir/a.css",
        "text/css",
        b"* { background-image: url(/dir/d.png); }",
        100,
    );
    let css = format!(
        "{}{}",
        Fixture::css_link_href("dir/a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = concat!(
        "<style>*{background-image:url(dir/d.png)}",
        "@media screen{*{margin:0}}</style>",
    );
    let combined_url = combined_leaf(&["dir/a.css", "b.css"], "0");
    let url = format!("{TEST_DOMAIN}with_combiner_rel.html");
    assert_eq!(
        fixture.parse_chain(&url, &css, Chain::CombinerThenSelector),
        format!(
            "{critical_css}{}",
            Fixture::load_rest_of_css(&Fixture::css_link_href(&combined_url)),
        ),
    );
}

#[test]
fn avoid_trying_to_inline_twice() {
    // When CriticalSelectorFilter and InlineCss are both
    // enabled, only CriticalSelectorFilter will be applied.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineCss);
    let css = format!(
        "{}{}",
        Fixture::css_link_href("a.css"),
        Fixture::css_link_href("b.css"),
    );
    let critical_css = concat!(
        "<style>div,*::first-letter{display:block}</style>",
        "<style>@media screen{*{margin:0}}</style>",
    );
    let input_html = format!("<head>{css}</head><body><div>Stuff</div></body>");
    let url = format!("{TEST_DOMAIN}with_inline_css.html");
    assert_eq!(
        fixture.parse_chain(&url, &input_html, Chain::SelectorThenInline),
        format!(
            "<head>{critical_css}</head><body><div>Stuff</div>{}</body>",
            Fixture::load_rest_of_css(&css),
        ),
    );
}

#[test]
fn dont_mix_unopt_and_opt_styles() {
    // Specific test for a bug where inline style blocks would
    // not be treated correctly. Even though they should have
    // been rewritten by rewrite_css, the unoptimized <style>
    // block would be copied to the bottom of the page. (Sync
    // NOTE: the CSS filter runs first. The
    // `set_enable_cache_purge` line has no setter here; it only
    // triggers a serving code path.)
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteCss);
    fixture.set_response_with_default_headers(
        "background.png",
        "image/png",
        b"Dummy contents",
        100,
    );
    fixture.set_response_with_default_headers(
        "extern.css",
        "text/css",
        b"* {background: url(background.png)}\n.not-critical { margin: 0 }",
        100,
    );
    let input_css = format!(
        "  <style>div {{ background: url(background.png) }}.not-critical {{ color: red; }}</style>\n  {}\n",
        Fixture::css_link_href("extern.css"),
    );
    let background_ce = "background.png.pagespeed.ce.0.png";
    let critical_css = format!(
        "  <style>div{{background:url({background_ce})}}</style>\n  <style>*{{background:url({background_ce})}}</style>\n",
    );
    // Both inline and external CSS should be rewritten here.
    let rewritten_css = format!(
        "<style>div{{background:url({background_ce})}}.not-critical{{color:red}}</style>{}",
        Fixture::css_link_href("A.extern.css.pagespeed.cf.0.css"),
    );
    let input_html = format!(
        "<head>\n{input_css}</head>\n<body>\n  <div>Foo</div>\n  <span>Bar</span>\n</body>\n",
    );
    let url = format!("{TEST_DOMAIN}dont_mix_unopt_and_opt.html");
    assert_eq!(
        fixture.parse_chain(&url, &input_html, Chain::CssThenSelector),
        format!(
            "<head>\n{critical_css}</head>\n<body>\n  <div>Foo</div>\n  <span>Bar</span>\n{}</body>\n",
            Fixture::load_rest_of_css(&rewritten_css),
        ),
    );
}

#[test]
fn subset_that_would_hold_markup_keeps_the_link() {
    // On an HTML page a critical subset that would hold markup
    // keeps the link as it is, wherever the link is.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(
        "c.css",
        "text/css",
        b"div{content:\"</noembed>\"}",
        100,
    );
    fixture.validate_no_changes(
        "closing_tag",
        &format!(
            "<head><noembed>{}</noembed></head><body><div>Stuff</div></body>",
            Fixture::css_link_href("c.css"),
        ),
    );
    fixture.set_response_with_default_headers(
        "d.css",
        "text/css",
        b"div{content:\"< input>\"}",
        100,
    );
    fixture.validate_no_changes(
        "start_tag",
        &format!(
            "<head></head><body><select>{}</select><div>Stuff</div></body>",
            Fixture::css_link_href("d.css"),
        ),
    );
    fixture.set_response_with_default_headers("e.css", "text/css", b"div{content:\"<!--\"}", 100);
    fixture.validate_no_changes(
        "comment_opener",
        &format!(
            "<head>{}</head><body><div>Stuff</div></body>",
            Fixture::css_link_href("e.css"),
        ),
    );
}

/// Expected multipart leaf for names under a hash (mirrors the
/// combiner suite helper).
fn combined_leaf(names: &[&str], hash: &str) -> String {
    use ps_html::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    let inputs: Vec<Vec<u8>> = names.iter().map(|name| name.as_bytes().to_vec()).collect();
    format!(
        "{}.pagespeed.cc.{hash}.css",
        String::from_utf8_lossy(&MultipartEncoder.encode(&inputs))
    )
}
