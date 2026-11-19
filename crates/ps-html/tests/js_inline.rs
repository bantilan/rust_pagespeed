//! JavaScript-inline tests ported from `js_inline_filter_test.cc`.
//!
//! NOTE: `InlineCombined`, `InlineMinimizeInteraction`, and
//! `MinifiedScriptKeepsItsCharsetForEachPage` wait for those
//! filters; `BasicCsp` waits for the CSP subsystem; the flush-split
//! case waits for flush-window parity.
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
use ps_html::filters::js_inline::{JsInlineFilter, NUM_JS_INLINED};
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
    /// Response mime verdict (`None` = unknown, C++ default).
    mime_is_xhtml: RefCell<Option<bool>>,
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
            mime_is_xhtml: RefCell::new(None),
        }
    }

    /// Marks the response HTML (`SetHtmlMimetype`).
    fn set_html_mimetype(&self) {
        *self.mime_is_xhtml.borrow_mut() = Some(false);
    }

    /// Marks the response XHTML (`SetXhtmlMimetype`).
    fn set_xhtml_mimetype(&self) {
        *self.mime_is_xhtml.borrow_mut() = Some(true);
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        // Unset stays Unknown (C++ with no mimetype).
        if let Some(is_xhtml) = *self.mime_is_xhtml.borrow() {
            driver.set_is_xhtml(is_xhtml);
        }
        driver
    }

    /// Scripts a cacheable JS fetch.
    fn mock_js(&self, url: &str, content: &[u8]) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("application/javascript"),
            "",
            "",
            &mut headers,
        );
        headers.compute_caching();
        self.fetcher.set_response(url, headers, content);
    }

    fn num_js_inlined(&self) -> i64 {
        self.server.variable(NUM_JS_INLINED).get()
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

    /// Parses a document through the inline filter.
    fn parse(&self, url: &str, html: &str) -> String {
        // The chain adds the filter only when enabled.
        let enabled = self.options.borrow().enabled(Filter::InlineJavascript);
        let driver = self.driver_for(url);
        if let Some(charset) = Self::meta_charset(html) {
            driver.set_containing_charset(&charset);
        }
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        if enabled {
            parser.add_filter(Box::new(JsInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
        }
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    fn enable_debug(&self) {
        self.options.borrow_mut().enable_filter(Filter::Debug);
    }

    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
    }
}

/// Inline-case matrix (`TestInlineJavascriptGeneral`).
#[allow(clippy::too_many_arguments)]
fn test_inline_javascript_general(
    fixture: &Fixture,
    html_url: &str,
    doctype: &str,
    js_url: &str,
    js_out_url: &str,
    js_original_inline_body: &str,
    js_outline_body: &str,
    js_expected_inline_body: &str,
    expect_inline: bool,
    debug_string: &str,
) {
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineJavascript);
    let template = format!(
        "{doctype}<head>\n  <script src=\"%s\">{js_original_inline_body}</script>%s\n</head>\n<body>Hello, world!</body>\n"
    );
    let html_input = template.replacen("%s", js_url, 1).replacen("%s", "", 1);
    let outline_html_output = template.replacen("%s", js_out_url, 1).replacen("%s", "", 1);
    let expected_output = if !expect_inline {
        outline_html_output.clone()
    } else {
        format!(
            "{doctype}<head>\n  <script>{js_expected_inline_body}</script>\n</head>\n<body>Hello, world!</body>\n"
        )
    };
    let outline_debug_html_output = if debug_string.is_empty() {
        outline_html_output
    } else {
        template
            .replacen("%s", js_out_url, 1)
            .replacen("%s", &format!("<!--{debug_string}-->"), 1)
    };
    fixture.mock_js(js_url, js_outline_body.as_bytes());
    assert_eq!(fixture.parse(html_url, &html_input), expected_output);
    if !expect_inline {
        fixture.enable_debug();
        assert_eq!(
            fixture.parse(html_url, &html_input),
            outline_debug_html_output
        );
    }
}

fn test_inline_javascript(
    fixture: &Fixture,
    html_url: &str,
    js_url: &str,
    js_original_inline_body: &str,
    js_outline_body: &str,
    expect_inline: bool,
) {
    // Plain rendering tests run as HTML (C++ calls SetHtmlMimetype).
    fixture.set_html_mimetype();
    test_inline_javascript_general(
        fixture,
        html_url,
        "",
        js_url,
        js_url,
        js_original_inline_body,
        js_outline_body,
        js_outline_body,
        expect_inline,
        "",
    );
}

fn test_no_inline_javascript(
    fixture: &Fixture,
    html_url: &str,
    js_url: &str,
    js_original_inline_body: &str,
    js_outline_body: &str,
    debug_message: &str,
) {
    test_inline_javascript_general(
        fixture,
        html_url,
        "",
        js_url,
        js_url,
        js_original_inline_body,
        js_outline_body,
        js_outline_body,
        false,
        debug_message,
    );
}

const XHTML_DOCTYPE: &str = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";

fn test_inline_javascript_xhtml(
    fixture: &Fixture,
    html_url: &str,
    js_url: &str,
    js_outline_body: &str,
    expect_inline: bool,
) {
    // NOTE: no mimetype call here (C++ default Unknown wraps too).
    test_inline_javascript_general(
        fixture,
        html_url,
        XHTML_DOCTYPE,
        js_url,
        js_url,
        "",
        js_outline_body,
        &format!("//<![CDATA[\n{js_outline_body}\n//]]>"),
        expect_inline,
        "",
    );
}

#[test]
fn do_inline_javascript_no_mimetype() {
    let fixture = Fixture::new();
    test_inline_javascript_xhtml(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "function id(x) { return x; }\n",
        true,
    );
}

#[test]
fn do_inline_javascript_simple_html() {
    let fixture = Fixture::new();
    test_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        "function id(x) { return x; }\n",
        true,
    );
}

#[test]
fn charset_mismatch_not_inlined() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("application/javascript"),
        "",
        "",
        &mut headers,
    );
    headers.compute_caching();
    headers.merge_content_type_str("text/javascript; charset=iso-8859-1");
    headers.compute_caching();
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}latin1.js"),
        headers.clone(),
        b"var s = 'caf\xe9';\n",
    );
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}ascii.js"),
        headers,
        b"var s = 'cafe';\n",
    );
    let utf8_page = "<meta charset=\"utf-8\">";
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            &format!("{utf8_page}<script src=latin1.js></script>")
        ),
        format!("{utf8_page}<script src=latin1.js></script>")
    );
    // Pure ASCII is the same in both charsets, so it is still inlined.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            &format!("{utf8_page}<script src=ascii.js></script>")
        ),
        format!("{utf8_page}<script>var s = 'cafe';\n</script>")
    );
}

#[test]
fn charset_match_inlined() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("application/javascript"),
        "",
        "",
        &mut headers,
    );
    headers.compute_caching();
    headers.merge_content_type_str("text/javascript; charset=UTF-8");
    headers.compute_caching();
    let body = "var s = 'caf\u{e9}';\n";
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}utf8.js"), headers, body.as_bytes());
    let utf8_page = "<meta charset=\"utf-8\">";
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}i.html"),
            &format!("{utf8_page}<script src=utf8.js></script>")
        ),
        format!("{utf8_page}<script>{body}</script>")
    );
}

#[test]
fn shared_script_decided_per_page_charset_in_either_order() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let body = "var s = '\u{4e2d}';\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}shared.js"), body.as_bytes());
    let utf8_page = "<meta charset=\"utf-8\">";
    let gbk_page = "<meta charset=\"gbk\">";
    let script = "<script src=shared.js charset=utf-8></script>";
    let inlined = format!("<script charset=utf-8>{body}</script>");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}u.html"),
            &format!("{utf8_page}{script}")
        ),
        format!("{utf8_page}{inlined}")
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}g.html"),
            &format!("{gbk_page}{script}")
        ),
        format!("{gbk_page}{script}")
    );
    fixture.clear_caches();
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}g.html"),
            &format!("{gbk_page}{script}")
        ),
        format!("{gbk_page}{script}")
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}u.html"),
            &format!("{utf8_page}{script}")
        ),
        format!("{utf8_page}{inlined}")
    );
}

#[test]
fn inline_decision_is_scoped_to_page_charset() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("application/javascript"),
        "",
        "",
        &mut headers,
    );
    headers.compute_caching();
    headers.merge_content_type_str("text/javascript; charset=UTF-8");
    headers.compute_caching();
    let script = "var s = '\u{4e2d}';\n";
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}shared.js"),
        headers,
        script.as_bytes(),
    );
    let utf8 = "<meta charset=\"utf-8\"><script src=shared.js></script>";
    let gbk = "<meta charset=\"gbk\"><script src=shared.js></script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}u.html"), utf8),
        format!("<meta charset=\"utf-8\"><script>{script}</script>")
    );
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}g.html"), gbk), gbk);
    fixture.clear_caches();
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}g.html"), gbk), gbk);
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}u.html"), utf8),
        format!("<meta charset=\"utf-8\"><script>{script}</script>")
    );
}

#[test]
fn inline_js_preserve_urls_on() {
    // Make sure that we don't inline when preserve urls is on.
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_js_preserve_urls(true);
    test_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        "function id(x) { return x; }\n",
        false,
    );
}

#[test]
fn do_inline_javascript_simple_xhtml() {
    let fixture = Fixture::new();
    fixture.set_xhtml_mimetype();
    test_inline_javascript_xhtml(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "function id(x) { return x; }\n",
        true,
    );
}

#[test]
fn script_inside_noscript_like_element_is_not_inlined() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    fixture.mock_js(&format!("{TEST_DOMAIN}script.js"), b"var s = 'a';\n");
    for (case, html) in [
        (
            "noscript",
            "<noscript><script src=\"script.js\"></script></noscript>",
        ),
        (
            "noembed",
            "<noembed><p><script src=\"script.js\"></script></p></noembed>",
        ),
        (
            "noframes",
            "<noframes><script src=\"script.js\"></script></noframes>",
        ),
    ] {
        assert_eq!(
            fixture.parse(&format!("{TEST_DOMAIN}{case}.html"), html),
            html
        );
    }
    assert_eq!(fixture.num_js_inlined(), 0);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}elsewhere.html"),
            "<div><script src=\"script.js\"></script></div>"
        ),
        "<div><script>var s = 'a';\n</script></div>"
    );
}

#[test]
fn inlined_script_on_ce_html_page_is_in_cdata() {
    // CE-HTML is XHTML: inlined scripts render as XML, in CDATA.
    let fixture = Fixture::new();
    test_inline_javascript_xhtml(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "var label = \"<b/>\";\n",
        true,
    );
}

#[test]
fn do_inline_javascript_whitespace() {
    let fixture = Fixture::new();
    // Whitespace between <script> and </script>:
    test_inline_javascript(
        &fixture,
        "http://www.example.com/index2.html",
        "http://www.example.com/script2.js",
        "\n    \n  ",
        "function id(x) { return x; }\n",
        true,
    );
}

#[test]
fn do_inline_javascript_different_domain() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_inline_unauthorized_resource_type(ps_config::options::Category::Script);
    test_inline_javascript(
        &fixture,
        "http://www.example.net/index.html",
        "http://scripts.example.org/script2.js",
        "",
        "function id(x) { return x; }\n",
        true,
    );
    assert_eq!(fixture.num_js_inlined(), 1);
}

#[test]
fn do_not_inline_javascript_different_domain() {
    let fixture = Fixture::new();
    test_no_inline_javascript(
        &fixture,
        "http://www.example.net/index.html",
        "http://scripts.example.org/script.js",
        "",
        "function id(x) { return x; }\n",
        "The preceding resource was not rewritten because its domain (scripts.example.org) is not authorized",
    );
    assert_eq!(fixture.num_js_inlined(), 0);
}

#[test]
fn do_not_inline_javascript_inline_contents() {
    // Inline contents (possible data store for the external script):
    let fixture = Fixture::new();
    test_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "{\"json\": true}",
        "function id(x) { return x; }\n",
        false,
    );
}

#[test]
fn do_not_inline_javascript_too_big() {
    let fixture = Fixture::new();
    let length = 2 * 2048;
    test_no_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        &format!(
            "function longstr() {{ return '{}'; }}\n",
            "z".repeat(length)
        ),
        "JS not inlined since it&#39;s bigger than 2048 bytes",
    );
}

#[test]
fn do_not_inline_introspective_javascript_by_default() {
    let fixture = Fixture::new();
    test_no_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        "function close() { return $('script'); }\n",
        "JS not inlined since it may be looking for its source",
    );
}

#[test]
fn do_inline_introspective_javascript() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option(
            "AvoidRenamingIntrospectiveJavascript",
            "false",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    test_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        "function close() { return $('script'); }\n",
        true,
    );
}

#[test]
fn dont_inline_disallowed() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().disallow("*script.js*");
    test_no_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        "function close() { return 'inline!'; }\n",
        "The preceding resource was not rewritten because its domain (www.example.com) is not authorized",
    );
}

#[test]
fn do_inline_disallowed_if_allowed_when_inlining() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .allow_only_when_inlining("*script.js*");
    test_inline_javascript(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "",
        "function close() { return 'inline!'; }\n",
        true,
    );
}

#[test]
fn do_inline_javascript_xhtml() {
    let fixture = Fixture::new();
    test_inline_javascript_xhtml(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "function id(x) { return x; }\n",
        true,
    );
}

#[test]
fn do_not_inline_javascript_xhtml_with_cdata_end() {
    // External script contains "]]>":
    let fixture = Fixture::new();
    test_inline_javascript_xhtml(
        &fixture,
        "http://www.example.com/index.html",
        "http://www.example.com/script.js",
        "function end(x) { return ']]>'; }\n",
        false,
    );
}

#[test]
fn cached_rewrite() {
    // Make sure we work fine when result is cached.
    let fixture = Fixture::new();
    for _ in 0..2 {
        test_inline_javascript(
            &fixture,
            "http://www.example.com/index.html",
            "http://www.example.com/script.js",
            "",
            "function id(x) { return x; }\n",
            true,
        );
    }
}

#[test]
fn cached_with_succesors() {
    // A following cache extender must not trip over the src the
    // inliner deleted while using cached results.
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineJavascript);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheScripts);
    fixture.mock_js(
        &format!("{TEST_DOMAIN}script.js"),
        b"function id(x) { return x; }\n",
    );
    let html_input = "<script src=\"script.js\"></script>";
    let html_output = "<script>function id(x) { return x; }\n</script>";
    for _ in 0..2 {
        let url = format!("{TEST_DOMAIN}inline_with_succ.html");
        let driver = fixture.driver_for(&url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(JsInlineFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(
            Box::new(ps_html::filters::cache_extender::CacheExtenderFilter::new(
                driver,
            )) as Box<dyn HtmlFilter>,
        );
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html_input.as_bytes());
        parser.finish();
        assert_eq!(String::from_utf8(sink.bytes()).unwrap(), html_output);
    }
}

#[test]
fn script_with_script_tags() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let js = "alert('<script></script>');\
        alert('<sCrIpT></ScRiPt>');\
        alert('</SCRIPT foo>');\
        alert('<Script</sCRIPT');\
        alert('</scr>');";
    fixture.mock_js(&format!("{TEST_DOMAIN}a.js"), js.as_bytes());
    let expected = "alert('<\\u0073cript></\\u0073cript>');\
        alert('<\\u0073CrIpT></\\u0053cRiPt>');\
        alert('</\\u0053CRIPT foo>');\
        alert('<\\u0053cript</\\u0073CRIPT');\
        alert('</scr>');";
    let url = format!("{TEST_DOMAIN}inline_with_close_script.html");
    let input = format!(
        "<head>\n  <script src='{TEST_DOMAIN}a.js'></script>\n</head>\n<body>Hello, world!</body>\n"
    );
    let output =
        format!("<head>\n  <script>{expected}</script>\n</head>\n<body>Hello, world!</body>\n");
    assert_eq!(fixture.parse(&url, &input), output);
}

/// Text-element names in the case forms the table test uses.
const TEXT_ELEMENT_NAMES: &[&str] = &[
    "script", "noscript", "noembed", "noframes", "textarea", "title", "style", "iframe", "xmp",
    "SCRIPT", "NOSCRIPT", "TextArea", "TiTlE", "STYLE", "iFrame", "Xmp", "NoEmbed", "NOFRAMES",
];

/// A name with its first letter as a `\uNNNN` escape.
fn escape_first_letter(name: &str) -> String {
    format!("\\u{:04x}{}", name.as_bytes()[0] as u32, &name[1..])
}

#[test]
fn text_element_end_tags_are_escaped() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    for (i, name) in TEXT_ELEMENT_NAMES.iter().enumerate() {
        let format = "var a = \"</%1$s>\";\n\
            var b = '</%1$s';\n\
            var c = /[</%1$s]/.test(a);\n\
            var d = 1 </%1$s/ > 0;\n\
            var e = /[</%1$s]/u.test(a) || 1 </%1$s/u > 0;\n\
            var f = /[<\\/]%2$s/v.test(a);\n\
            /* </%1$s> */\n\
            // </%1$s>\n";
        let js = format.replace("%1$s", name).replace("%2$s", name);
        let escaped_name = escape_first_letter(name);
        let escaped = format.replace("%1$s", &escaped_name).replace("%2$s", name);
        // The inlined text holds no such end tag anymore.
        assert!(!escaped
            .to_lowercase()
            .contains(&format!("</{}", name.to_lowercase())));
        let js_url = format!("t{i}.js");
        fixture.mock_js(&format!("{TEST_DOMAIN}{js_url}"), js.as_bytes());
        assert_eq!(
            fixture.parse(
                &format!("{TEST_DOMAIN}{js_url}"),
                &format!("<script src=\"{js_url}\"></script>")
            ),
            format!("<script>{escaped}</script>"),
            "name {name}"
        );
    }
}

#[test]
fn less_than_at_end_of_script_is_kept() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    for (i, js) in ["var ok = 1 <", "var ok = 1 </", "<", "</"]
        .iter()
        .enumerate()
    {
        let js_url = format!("e{i}.js");
        fixture.mock_js(&format!("{TEST_DOMAIN}{js_url}"), js.as_bytes());
        assert_eq!(
            fixture.parse(
                &format!("{TEST_DOMAIN}{js_url}"),
                &format!("<script src=\"{js_url}\"></script>")
            ),
            format!("<script>{js}</script>"),
            "js {js}"
        );
    }
}

#[test]
fn regex_flags_after_less_than_are_kept() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let js = "var r = /x</gi, s = /y</ims;\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}script.js"), js.as_bytes());
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}flags.html"),
            "<script src=\"script.js\"></script>"
        ),
        format!("<script>{js}</script>")
    );
}

#[test]
fn template_literal_is_inlined_only_without_escape() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let plain = "var t = `<b>${x}</b>`;\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}plain.js"), plain.as_bytes());
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}plain.html"),
            "<script src=\"plain.js\"></script>"
        ),
        format!("<script>{plain}</script>")
    );
    let end_tag = "var t = String.raw`</textarea>`;\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}end.js"), end_tag.as_bytes());
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}end.html"),
            "<script src=\"end.js\"></script>"
        ),
        "<script src=\"end.js\"></script>"
    );
    let script_tag = "var t = `<script>`;\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}open.js"), script_tag.as_bytes());
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}open.html"),
            "<script src=\"open.js\"></script>"
        ),
        "<script src=\"open.js\"></script>"
    );
    assert_eq!(fixture.num_js_inlined(), 1);
}

#[test]
fn text_element_end_tags_are_escaped_on_xhtml_page() {
    let fixture = Fixture::new();
    fixture.set_xhtml_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let js = "var s = \"</title>\";\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}script.js"), js.as_bytes());
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}xhtml.html"),
            "<script src=\"script.js\"></script>"
        ),
        "<script>//<![CDATA[\nvar s = \"</\\u0074itle>\";\n\n//]]></script>"
    );
}

#[test]
fn inlined_script_does_not_end_enclosing_element() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let js = "var s = \"</textarea/><span id=promoted>m</span>\";\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}ta.js"), js.as_bytes());
    let out = fixture.parse(
        &format!("{TEST_DOMAIN}textarea.html"),
        "<textarea/><script src=\"ta.js\"></script></textarea>",
    );
    assert!(out.contains("var s = \"</\\u0074extarea/><span"), "{out}");
    assert!(!out.contains("\"</textarea"), "{out}");
    let noscript_js = "var s = \"</noscript><span id=promoted>m</span>\";\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}ns.js"), noscript_js.as_bytes());
    let out = fixture.parse(
        &format!("{TEST_DOMAIN}noscript.html"),
        "<noscript><script src=\"ns.js\"></script></noscript>",
    );
    assert!(!out.contains("\"</noscript"), "{out}");
}

#[test]
fn no_inline_async_script() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    fixture.mock_js(
        &format!("{TEST_DOMAIN}script.js"),
        b"function id(x) { return x; }\n",
    );
    let html = "<script async src=script.js></script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}no_inline_async.html"), html),
        html
    );
}

#[test]
fn no_inline_defer_script() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    fixture.mock_js(
        &format!("{TEST_DOMAIN}script.js"),
        b"function id(x) { return x; }\n",
    );
    let html = "<script defer src=script.js></script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}no_inline_defer.html"), html),
        html
    );
}

#[test]
fn inline_preserves_attributes() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    fixture.mock_js(
        &format!("{TEST_DOMAIN}script.js"),
        b"function id(x) { return x; }\n",
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}inline_attrs.html"),
            "<script src=script.js nonce=\"n\" crossorigin=\"anonymous\" referrerpolicy=\"no-referrer\" fetchpriority=\"high\" nomodule></script>"
        ),
        "<script nonce=\"n\" crossorigin=\"anonymous\" referrerpolicy=\"no-referrer\" fetchpriority=\"high\" nomodule>function id(x) { return x; }\n</script>"
    );
}

#[test]
fn no_inline_module_script() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    fixture.mock_js(&format!("{TEST_DOMAIN}mod.js"), b"import x from '/x.js';\n");
    let html = "<script type=\"module\" src=mod.js></script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}no_inline_module.html"), html),
        html
    );
}

#[test]
fn flush_splitting_script_tag() {
    // Inlining works even when the mid-tag point straddles a flush.
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let js = "function id(x) { return x; }\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}script.js"), js.as_bytes());
    let url = format!("{TEST_DOMAIN}inline_with_flush.html");
    let driver = fixture.driver_for(&url);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(JsInlineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url.as_bytes(), false);
    parser.write(b"<script src=\"script.js\">  \n  \n  ");
    parser.flush();
    parser.write(b"  \n  \n  </script>");
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        format!("<script>{js}</script>")
    );
}

#[test]
fn no_flush_splitting_script_tag() {
    // Sanity check: without the flush the same input inlines too.
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
    let js = "function id(x) { return x; }\n";
    fixture.mock_js(&format!("{TEST_DOMAIN}script.js"), js.as_bytes());
    let html = "<script src=\"script.js\">  \n  \n    \n  \n  </script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}inline_with_flush.html"), html),
        format!("<script>{js}</script>")
    );
}

#[test]
fn inline_js_404() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineJavascript);
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
        .set_response(&format!("{TEST_DOMAIN}404.js"), headers, b"");
    let html = "<script src='404.js'></script>";
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}404.html"), html), html);
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}404.html"), html), html);
}
