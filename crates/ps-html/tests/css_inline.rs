//! CSS-inline tests ported from `css_inline_filter_test.cc`.
//!
//! NOTE: inputs keep full documents (the C++ helper wraps in
//! head/body). `InlineAndPrioritizeCss`, `InlineCombined`, and
//! `InlineMinimizeInteraction` wait for those filters; `BasicCsp`
//! waits for the CSP subsystem. Meta-charset plumbing belongs to
//! the scan filter; the fixture sets the verdict directly.
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
use ps_html::filters::cache_extender::CacheExtenderFilter;
use ps_html::filters::css_inline::{CssInlineFilter, NUM_CSS_INLINED};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    shards: RefCell<Vec<(String, String)>>,
}

impl Fixture {
    fn new() -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend.clone(),
            metadata_backend.clone(),
            fetcher.clone(),
            START_MS,
        );
        server.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        Fixture {
            server: Rc::new(server),
            options: Rc::new(RefCell::new(RewriteOptions::new())),
            fetcher,
            http_backend,
            metadata_backend,
            shards: RefCell::new(Vec::new()),
        }
    }

    /// Clears both caches (`lru_cache()->Clear()`).
    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
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
        Rc::new(driver)
    }

    /// Scripts a cacheable CSS fetch (`SetResponseWithDefaultHeaders`).
    fn mock_css(&self, url: &str, content: &[u8], ttl_sec: i64) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("text/css"),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), ttl_sec * 1000, "");
        headers.headers_mut().remove_all("Last-Modified");
        headers.headers_mut().remove_all("Etag");
        headers.compute_caching();
        self.fetcher.set_response(url, headers, content);
    }

    fn num_css_inlined(&self) -> i64 {
        self.server.variable(NUM_CSS_INLINED).get()
    }

    /// Extracts `<meta charset>` (scan-filter derivation deferred).
    fn meta_charset(html: &str) -> Option<String> {
        let lower = html.to_lowercase();
        let meta = lower.find("<meta")?;
        let after = &html[meta..];
        let end = after.find('>')?;
        let tag = &after[..end];
        let pos = tag.to_lowercase().find("charset")?;
        let rest = tag[pos + "charset".len()..].trim_start();
        let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            Some(rest[1..].split(quote).next().unwrap_or("").to_string())
        } else {
            Some(
                rest.split(|c: char| c.is_whitespace() || c == '/' || c == '>')
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
        }
    }

    /// Parses a full document through the inline filter.
    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        if let Some(charset) = Self::meta_charset(html) {
            driver.set_containing_charset(&charset);
        }
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Parses through inline + extender sharing one driver (chain order).
    fn parse_inline_and_extend(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        if let Some(charset) = Self::meta_charset(html) {
            driver.set_containing_charset(&charset);
        }
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssInlineFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(CacheExtenderFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    fn enable_debug(&self) {
        self.options.borrow_mut().enable_filter(Filter::Debug);
    }
}

/// Inline-case matrix (`TestInlineCssWithOutputUrl`).
#[allow(clippy::too_many_arguments)]
fn test_inline_css_with_output_url(
    fixture: &Fixture,
    html_url: &str,
    head_extras: &str,
    css_url: &str,
    css_out_url: &str,
    other_attrs: &str,
    css_original_body: &str,
    expect_inline: bool,
    css_rewritten_body: &str,
    debug_string: &str,
) {
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    let attrs = if other_attrs.is_empty() {
        String::new()
    } else {
        format!(" {other_attrs}")
    };
    let template = format!(
        "<head>\n{head_extras}  <link rel=\"stylesheet\" href=\"%s\"{attrs}>%s\n</head>\n<body>Hello, world!</body>\n"
    );
    let html_input = template.replacen("%s", css_url, 1).replacen("%s", "", 1);
    let outline_html_output = template
        .replacen("%s", css_out_url, 1)
        .replacen("%s", "", 1);
    let outline_debug_html_output = if debug_string.is_empty() {
        outline_html_output.clone()
    } else {
        template
            .replacen("%s", css_out_url, 1)
            .replacen("%s", &format!("<!--{debug_string}-->"), 1)
    };
    fixture.mock_css(css_url, css_original_body.as_bytes(), 300);
    let expected_output = if !expect_inline {
        outline_html_output
    } else {
        format!(
            "<head>\n{head_extras}  <style{attrs}>{css_rewritten_body}</style>\n</head>\n<body>Hello, world!</body>\n"
        )
    };
    assert_eq!(fixture.parse(html_url, &html_input), expected_output);
    if !expect_inline {
        fixture.enable_debug();
        assert_eq!(
            fixture.parse(html_url, &html_input),
            outline_debug_html_output
        );
    }
}

fn test_inline_css(
    fixture: &Fixture,
    html_url: &str,
    css_url: &str,
    other_attrs: &str,
    css_original_body: &str,
    expect_inline: bool,
    css_rewritten_body: &str,
) {
    test_inline_css_with_output_url(
        fixture,
        html_url,
        "",
        css_url,
        css_url,
        other_attrs,
        css_original_body,
        expect_inline,
        css_rewritten_body,
        "",
    );
}

fn test_no_inline_css(
    fixture: &Fixture,
    html_url: &str,
    css_url: &str,
    other_attrs: &str,
    css_original_body: &str,
    css_rewritten_body: &str,
    debug_string: &str,
) {
    test_inline_css_with_output_url(
        fixture,
        html_url,
        "",
        css_url,
        css_url,
        other_attrs,
        css_original_body,
        false,
        css_rewritten_body,
        debug_string,
    );
}

#[test]
fn inline_css_simple() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "",
        css,
        true,
        css,
    );
}

#[test]
fn inline_css_unhealthy() {
    let fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    let css = "BODY { color: red; }\n";
    test_inline_css_with_output_url(
        &fixture,
        "http://www.example.com/index.html",
        "",
        "http://www.example.com/styles.css",
        "http://www.example.com/styles.css",
        "",
        css,
        false,
        css,
        "",
    );
}

#[test]
fn inline_css_404() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("text/plain"),
        "",
        "",
        &mut headers,
    );
    headers.set_first_line(1, 1, 404, "Not Found");
    headers.compute_caching();
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}404.css"), headers, b"");
    let html = "<link rel=stylesheet href='404.css'>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}404.html"), html), html);
    // Second time, to make sure caching doesn't break it.
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}404.html"), html), html);
}

#[test]
fn inline_css_cached() {
    // Doing it twice should be safe, too.
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    for _ in 0..2 {
        test_inline_css(
            &fixture,
            "http://www.example.com/index.html",
            "http://www.example.com/styles.css",
            "",
            css,
            true,
            css,
        );
    }
}

#[test]
fn inline_css_rewrite_urls1() {
    // CSS with a relative URL that needs to be changed:
    let fixture = Fixture::new();
    let css1 = "BODY { background-image: url('bg.png'); }\n";
    let css2 = "BODY { background-image: url('foo/bar/bg.png'); }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/foo/bar/baz.css",
        "",
        css1,
        true,
        css2,
    );
}

#[test]
fn inline_css_rewrite_urls2() {
    // CSS with a relative URL, this time with ".." in it:
    let fixture = Fixture::new();
    let css1 = "BODY { background-image: url('../quux/bg.png'); }\n";
    let css2 = "BODY { background-image: url('foo/quux/bg.png'); }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/foo/bar/baz.css",
        "",
        css1,
        true,
        css2,
    );
}

#[test]
fn shard_subresources() {
    let fixture = Fixture::new();
    fixture.add_shard("www.example.com", "shard1.com,shard2.com");
    let css_in = ".p1 { background-image: url('b1.png'); }.p2 { background-image: url('b2.png'); }";
    let css_out = ".p1 { background-image: url('http://shard2.com/b1.png'); }.p2 { background-image: url('http://shard1.com/b2.png'); }";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/baz.css",
        "",
        css_in,
        true,
        css_out,
    );
}

#[test]
fn no_rewrite_urls_same_dir() {
    let fixture = Fixture::new();
    let css = "BODY { background-image: url('bg.png'); }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/baz.css",
        "",
        css,
        true,
        css,
    );
}

#[test]
fn do_not_inline_css_with_media_not_screen() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    test_no_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "media=\"print\"",
        css,
        "",
        "CSS not inlined because media does not match screen",
    );
}

#[test]
fn do_inline_css_with_media_all() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "media=\"all\"",
        css,
        true,
        css,
    );
}

#[test]
fn do_inline_css_with_media_screen() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "media=\"print, audio ,, ,sCrEeN \"",
        css,
        true,
        css,
    );
}

#[test]
fn do_inline_css_with_media_query() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "media=\"only (color)\"",
        css,
        true,
        css,
    );
}

#[test]
fn empty() {
    // Don't inline empty resources (defensive: issue 1050).
    let fixture = Fixture::new();
    test_no_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "",
        "",
        "",
        "Resource is empty, preventing rewriting of http://www.example.com/styles.css",
    );
}

#[test]
fn inline_css_with_invalid_media() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    // Indecipherable without screen/all: no inline.
    test_no_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "media=\"not!?#?;valid\"",
        css,
        "",
        "CSS not inlined because media does not match screen",
    );
    // ...but screen anywhere still inlines.
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "media=\"not!?#?;valid,screen\"",
        css,
        true,
        css,
    );
}

#[test]
fn do_not_inline_css_too_big() {
    let fixture = Fixture::new();
    let length = 2 * 2048;
    let css = format!(
        "BODY {{ background-image: url('{}' ); }}\n",
        "z".repeat(length)
    );
    test_no_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/styles.css",
        "",
        &css,
        "",
        "CSS not inlined since it&#39;s bigger than 2048 bytes",
    );
}

#[test]
fn do_inline_css_different_domain() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_inline_unauthorized_resource_type(ps_config::options::Category::Stylesheet);
    let css = "BODY { color: red; }\n";
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://unauth.com/styles.css",
        "",
        css,
        true,
        css,
    );
    assert_eq!(fixture.num_css_inlined(), 1);
}

#[test]
fn do_not_inline_css_different_domain() {
    // Note: this only fails because unauth.com is not authorized.
    let fixture = Fixture::new();
    test_no_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://unauth.com/styles.css",
        "",
        "BODY { color: red; }\n",
        "",
        "The preceding resource was not rewritten because its domain (unauth.com) is not authorized",
    );
    assert_eq!(fixture.num_css_inlined(), 0);
}

#[test]
fn correctly_inline_css_with_imports() {
    let fixture = Fixture::new();
    test_inline_css(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/dir/styles.css",
        "",
        "@import \"foo.css\"; BODY { color: red; }\n",
        true,
        "@import \"dir/foo.css\"; BODY { color: red; }\n",
    );
}

#[test]
fn claims_xhtml_but_has_unclosed_link() {
    // XHTML text should not have unclosed links, but when it does
    // (issue 252) we still inline them.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), b".a {}", 300);
    let html_format = "<head>\n  %s\n  %s\n  <script type='text/javascript' src='c.js'></script></head>\n<body><div class=\"c1\"><div class=\"c2\"><p>\n  Yellow on Blue</p></div></div></body>";
    let dtd = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\">";
    let unclosed = "  <link rel='stylesheet' href='a.css' type='text/css'>\n";
    let inlined = "  <style>.a {}</style>\n";
    let input = html_format
        .replacen("%s", dtd, 1)
        .replacen("%s", unclosed, 1);
    let expected = html_format
        .replacen("%s", dtd, 1)
        .replacen("%s", inlined, 1);
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}index.html"), &input),
        expected
    );
}

#[test]
fn dont_inline_in_noscript() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}a.css"),
        b"div {display:block;}",
        3000,
    );
    let html = "<noscript><link rel=stylesheet href=\"a.css\"></noscript>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}index.html"), html),
        html
    );
}

#[test]
fn inline_cache_extend_interaction() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option("CssInlineMaxBytes", "400", ps_config::Scope::ProcessStrict)
        .unwrap();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ExtendCacheCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}a.css"),
        b"div {display:block;}",
        3000,
    );
    let link = "<link rel=stylesheet href=\"a.css\">";
    let expected = "<style>div {display:block;}</style>";
    assert_eq!(
        fixture.parse_inline_and_extend(&format!("{TEST_DOMAIN}i.html"), link),
        expected
    );
    // The extender never produced an output: the CSS got inlined.
    assert_eq!(
        fixture
            .server
            .variable(ps_html::filters::cache_extender::CACHE_EXTENSIONS)
            .get(),
        0
    );
    // Again, hitting cache-hit paths for the inliner.
    assert_eq!(
        fixture.parse_inline_and_extend(&format!("{TEST_DOMAIN}i.html"), link),
        expected
    );
    assert_eq!(
        fixture
            .server
            .variable(ps_html::filters::cache_extender::CACHE_EXTENSIONS)
            .get(),
        0
    );
}

#[test]
fn inline_cache_extend_interaction_repeated() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option("CssInlineMaxBytes", "400", ps_config::Scope::ProcessStrict)
        .unwrap();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ExtendCacheCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}a.css"),
        b"div {display:block;}",
        3000,
    );
    let link = "<link rel=stylesheet href=\"a.css\">";
    let inlined = "<style>div {display:block;}</style>";
    let input = format!("{link}{link}");
    let expected = format!("{inlined}{inlined}");
    assert_eq!(
        fixture.parse_inline_and_extend(&format!("{TEST_DOMAIN}i.html"), &input),
        expected
    );
    assert_eq!(
        fixture
            .server
            .variable(ps_html::filters::cache_extender::CACHE_EXTENSIONS)
            .get(),
        0
    );
    assert_eq!(
        fixture.parse_inline_and_extend(&format!("{TEST_DOMAIN}i.html"), &input),
        expected
    );
    assert_eq!(
        fixture
            .server
            .variable(ps_html::filters::cache_extender::CACHE_EXTENSIONS)
            .get(),
        0
    );
}

#[test]
fn charset_determination() {
    // Exercises `GetCharsetForStylesheet` (which has no unit test of
    // its own) over loaded resources.
    use ps_html::resource::Resource;
    let fixture = Fixture::new();
    let bodies: &[(&str, &[u8])] = &[
        ("x.css", b"BODY { color: red; }"),
        ("y.css", b"\xef\xbb\xbfBODY { color: green; }"),
        ("z.css", b"\xef\xbb\xbfBODY { color: blue; }"),
    ];
    for (name, body) in bodies {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        fixture.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("application/javascript"),
            "",
            "",
            &mut headers,
        );
        headers.compute_caching();
        if *name == "z.css" {
            headers.merge_content_type_str("text/css; charset=iso-8859-1");
            headers.compute_caching();
        }
        fixture
            .fetcher
            .set_response(&format!("{TEST_DOMAIN}{name}"), headers, body);
    }
    let load = |name: &str| {
        let driver = fixture.driver_for(&format!("{TEST_DOMAIN}index.html"));
        let url = ps_http::google_url::GoogleUrl::new(&format!("{TEST_DOMAIN}{name}"));
        let (resource, _) = driver.create_input_resource(
            &url,
            ps_html::server_context::InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            ps_html::server_context::IntendedFor::General,
            ps_html::server_context::InputRole::Style,
        );
        let mut input = resource.expect("resource");
        let mut ctx = driver.request_context().borrow().clone();
        let mut callback = ps_html::cacheable_resource::MockLoadCallback::new();
        input.load(
            ps_html::cacheable_resource::NotCacheablePolicy::ReportFailureIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        assert!(callback.success());
        input
    };
    let x = load("x.css");
    let y = load("y.css");
    let z = load("z.css");
    let get = ps_html::rewrite_filter::charset_for_stylesheet;
    // Nothing set: charset should be empty.
    assert!(get(x.charset().as_bytes(), &x.contents(), b"", b"").is_empty());
    // Only the containing charset is set.
    assert_eq!(
        get(x.charset().as_bytes(), &x.contents(), b"", b"us-ascii"),
        b"us-ascii"
    );
    // The containing charset is trumped by the element's charset attribute.
    assert_eq!(
        get(x.charset().as_bytes(), &x.contents(), b"gb", b"us-ascii"),
        b"gb"
    );
    // The element's charset attribute is trumped by the resource's BOM.
    assert_eq!(
        get(y.charset().as_bytes(), &y.contents(), b"gb", b"us-ascii"),
        b"utf-8"
    );
    // The resource's BOM is trumped by the resource's header.
    assert_eq!(
        get(z.charset().as_bytes(), &z.contents(), b"gb", b"us-ascii"),
        b"iso-8859-1"
    );
}

#[test]
fn inline_with_compatible_bom() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    let css_with_bom = format!("\u{feff}{css}");
    test_inline_css_with_output_url(
        &fixture,
        "http://www.example.com/index.html",
        "  <meta charset=\"UTF-8\">\n",
        "http://www.example.com/styles.css",
        "http://www.example.com/styles.css",
        "",
        &css_with_bom,
        true,
        css,
        "",
    );
}

#[test]
fn do_not_inline_with_incompatible_bom_and_non_ascii() {
    let fixture = Fixture::new();
    let css = "BODY { color: red; /* \u{410} */ }\n";
    let css_with_bom = format!("\u{feff}{css}");
    test_inline_css_with_output_url(
        &fixture,
        "http://www.example.com/index.html",
        "  <meta charset=\"ISO-8859-1\">\n",
        "http://www.example.com/styles.css",
        "http://www.example.com/styles.css",
        "",
        &css_with_bom,
        false,
        "",
        "CSS not inlined due to apparent charset incompatibility; we think the HTML is ISO-8859-1 while the CSS is utf-8",
    );
}

#[test]
fn do_inline_with_incompatible_bom_and_ascii() {
    // Even though the content is labeled utf-8, it keeps to the ASCII
    // subset, so it's safe to inline.
    let fixture = Fixture::new();
    let css = "BODY { color: red; }\n";
    let css_with_bom = format!("\u{feff}{css}");
    test_inline_css_with_output_url(
        &fixture,
        "http://www.example.com/index.html",
        "  <meta charset=\"ISO-8859-1\">\n",
        "http://www.example.com/styles.css",
        "http://www.example.com/styles.css",
        "",
        &css_with_bom,
        true,
        css,
        "",
    );
}

#[test]
fn alternate_stylesheet() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(&format!("{TEST_DOMAIN}foo.css"), b"a{margin:0}", 100);
    // Normal (persistent) CSS links are inlined.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<link rel='stylesheet' href='foo.css'>"
        ),
        "<style>a{margin:0}</style>"
    );
    // Make sure we accept mixed case for the keyword.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<link rel=' StyleSheet ' href='foo.css'>"
        ),
        "<style>a{margin:0}</style>"
    );
    // Preferred links are not: inline styles cannot take a title.
    let html = "<link rel='stylesheet' href='foo.css' title='foo'>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
    // Alternate links, likewise.
    let html = "<link rel='alternate stylesheet' href='foo.css' title='foo'>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
}

#[test]
fn carry_across_other_attributes() {
    // Carry across attributes such as id and class to the inlined style tag.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(&format!("{TEST_DOMAIN}foo.css"), b"a{margin:0}", 100);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<link rel='stylesheet' href='foo.css' id='my-stylesheet' class='a b c' lulz='!@$@#$%@4lulz'>"
        ),
        "<style id='my-stylesheet' class='a b c' lulz='!@$@#$%@4lulz'>a{margin:0}</style>"
    );
    // But respect pagespeed_no_transform.
    let html = "<link rel='stylesheet' href='foo.css' id='my-stylesheet' class='a b c' pagespeed_no_transform>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
    let html = "<link rel='stylesheet' href='foo.css' id='my-stylesheet' class='a b c' data-pagespeed-no-transform>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
}

#[test]
fn no_rel() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(&format!("{TEST_DOMAIN}foo.css"), b"a{margin:0}", 100);
    // We don't mess with links that lack rel attributes.
    let html = "<link href='foo.css'>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
}

#[test]
fn non_css() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("text/xml"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), 100 * 1000, "");
    headers.compute_caching();
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}foo.xsl"),
        headers,
        b"<xsl:variable name='foo' select='bar'>",
    );
    let html = "<link rel='stylesheet' href='foo.xsl' type='text/xsl'/>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
}

/// Closing-tag matrix (`VerifyNoInliningForClosingStyleTag`).
fn verify_no_inlining_for_closing_style_tag(closing_style_tag: &str) {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.css"),
        format!("a{{margin:0}}{closing_style_tag}").as_bytes(),
        100,
    );
    // We don't mess with links that contain a closing style tag.
    let html = "<link rel='stylesheet' href='foo.css'>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), html), html);
    fixture.enable_debug();
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}i.html"), html),
        format!("{html}<!--CSS not inlined since it contains a less-than sign-->")
    );
}

#[test]
fn no_inlining_of_close_style_tag() {
    verify_no_inlining_for_closing_style_tag("</style>");
}

#[test]
fn no_inlining_of_close_style_tag_with_capitalization() {
    verify_no_inlining_for_closing_style_tag("</Style>");
}

#[test]
fn no_inlining_of_close_style_tag_with_spaces() {
    verify_no_inlining_for_closing_style_tag("</style abc>");
}

#[test]
fn no_inlining_of_any_tag() {
    verify_no_inlining_for_closing_style_tag("td[headers=\"<x>\"]{padding:3px}");
}

#[test]
fn escaped_closing_style_tag_is_inlined_verbatim_in_html() {
    // CSS escapes are not decoded by the HTML parser, so an escaped
    // closing tag is verbatim-safe inside <style> and stays inlined.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    let css = "a{content:\"\\3C/style\\3E\"}";
    fixture.mock_css(&format!("{TEST_DOMAIN}foo.css"), css.as_bytes(), 100);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<link rel='stylesheet' href='foo.css'>"
        ),
        format!("<style>{css}</style>")
    );
}

#[test]
fn disabled_for_amp() {
    use ps_html::filters::amp_document::AmpDocumentFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.css"),
        b"/* pretend there is a @font-face here */",
        100,
    );
    fixture.enable_debug();
    let url = format!("{TEST_DOMAIN}amp.html");
    let driver = fixture.driver_for(&url);
    let amp_driver = driver.clone();
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(AmpDocumentFilter::new(move |is_amp| {
        amp_driver.set_is_amp_document(is_amp);
    })) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(CssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let html = "<html amp><link rel='stylesheet' href='foo.css'>";
    parser.start_parse(url.as_bytes(), false);
    parser.write(html.as_bytes());
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        format!("{html}<!--CSS inlining not supported by PageSpeed for AMP documents-->")
    );
    // Make sure same stylesheet gets inlined elsewhere.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<link rel='stylesheet' href='foo.css'>"
        ),
        "<style>/* pretend there is a @font-face here */</style>"
    );
}

#[test]
fn check_inlining_of_link_style_tag_in_body_pedantic() {
    use ps_html::filters::pedantic::PedanticFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.options.borrow_mut().enable_filter(Filter::Pedantic);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.css"),
        b"/* pretend there is a @font-face here */",
        100,
    );
    fixture.enable_debug();
    // Don't inline style links in body with pedantic and no mover.
    let parse = |html: &str| {
        let url = format!("{TEST_DOMAIN}i.html");
        let driver = fixture.driver_for(&url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(PedanticFilter) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    };
    assert_eq!(
        parse("<html><head></head><body><link property='stylesheet'rel='stylesheet' href='foo.css'></body></html>"),
        "<html><head></head><body><link property='stylesheet' rel='stylesheet' href='foo.css'><!--CSS not inlined because style link element in html body--></body></html>"
    );
    // ...but do inline them in head (pedantic adds the type).
    assert_eq!(
        parse("<html><head><link property='stylesheet'rel='stylesheet' href='foo.css'></head><body></body></html>"),
        "<html><head><style property='stylesheet' type=\"text/css\">/* pretend there is a @font-face here */</style></head><body></body></html>"
    );
}

#[test]
fn inlining_of_link_style_tag_in_body() {
    use ps_html::filters::move_css_to_head::MoveCssToHeadFilter;
    use ps_html::filters::pedantic::PedanticFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.options.borrow_mut().enable_filter(Filter::Pedantic);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::MoveCssToHead);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.css"),
        b"/* pretend there is a @font-face here */",
        100,
    );
    fixture.enable_debug();
    // With the mover, body links inline and move to head.
    let url = format!("{TEST_DOMAIN}i.html");
    let driver = fixture.driver_for(&url);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(CssInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(PedanticFilter) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(MoveCssToHeadFilter::new(true, false)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url.as_bytes(), false);
    parser
        .write(b"<html><head></head><body><link property='stylesheet'rel='stylesheet' href='foo.css'></body></html>");
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        "<html><head><style property='stylesheet' type=\"text/css\">/* pretend there is a @font-face here */</style></head><body></body></html>"
    );
}

#[test]
fn check_inlining_of_link_style_tag_in_body_non_pedantic() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.css"),
        b"/* pretend there is a @font-face here */",
        100,
    );
    fixture.enable_debug();
    // Inlining stays in body without pedantic/mover.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<html><head></head><body><link property='stylesheet'rel='stylesheet' href='foo.css'></body></html>"
        ),
        "<html><head></head><body><style property='stylesheet'>/* pretend there is a @font-face here */</style></body></html>"
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            "<html><head><link property='stylesheet'rel='stylesheet' href='foo.css'></head><body></body></html>"
        ),
        "<html><head><style property='stylesheet'>/* pretend there is a @font-face here */</style></head><body></body></html>"
    );
}

/// Markup-placement matrix (`ExpectExternal`): stylesheets holding
/// markup stay external wherever their link sits.
fn expect_external(id: &str, css: &str, before: &str, after: &str) {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    let url = format!("{id}.css");
    fixture.mock_css(&format!("{TEST_DOMAIN}{url}"), css.as_bytes(), 100);
    let html = format!("{before}<link rel='stylesheet' href='{url}'>{after}");
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}i.html"), &html), html);
}

#[test]
fn markup_placement_closing_tag() {
    expect_external(
        "noembed",
        "a{margin:0}/*</noembed>*/",
        "<noembed>",
        "</noembed>",
    );
    expect_external(
        "noframes",
        "a{margin:0}/*</noframes>*/",
        "<noframes>",
        "</noframes>",
    );
    expect_external("textarea", "a{margin:0}/*</textarea>*/", "<textarea/>", "");
}

#[test]
fn markup_placement_start_tag() {
    expect_external("select", "a{margin:0}/*<input>*/", "<select>", "</select>");
    expect_external(
        "frameset",
        "a{margin:0}/*<frame>*/",
        "<frameset>",
        "</frameset>",
    );
}

#[test]
fn markup_placement_comment_opener() {
    expect_external("comment", "a{margin:0}/*<!--*/", "", "");
}

#[test]
fn shared_stylesheet_decided_per_page_charset_in_either_order() {
    // Whichever page is rewritten first, the utf-8 page inlines and
    // the gbk page keeps it external: no verdict leaks via metadata.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineCss);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}shared.css"),
        "a:after{content:'\u{4e2d}'}".as_bytes(),
        3000,
    );
    let utf8_page = "<meta charset=\"utf-8\">";
    let gbk_page = "<meta charset=\"gbk\">";
    let link = "<link rel=stylesheet href=shared.css charset=utf-8>";
    let css = "a:after{content:'\u{4e2d}'}";
    let inlined = format!("<style charset=utf-8>{css}</style>");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}utf8.html"),
            &format!("{utf8_page}{link}")
        ),
        format!("{utf8_page}{inlined}")
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}gbk.html"),
            &format!("{gbk_page}{link}")
        ),
        format!("{gbk_page}{link}")
    );
    fixture.clear_caches();
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}gbk.html"),
            &format!("{gbk_page}{link}")
        ),
        format!("{gbk_page}{link}")
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}utf8.html"),
            &format!("{utf8_page}{link}")
        ),
        format!("{utf8_page}{inlined}")
    );
}
