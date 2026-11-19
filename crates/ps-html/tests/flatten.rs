//! CSS import-flattening tests ported from
//! `css_flatten_imports_test.cc`.
//!
//! NOTE: logging assertions wait for the log record; LRU-count
//! assertions wait for a counting test backend (shapes differ by
//! architecture); image-nested cases wait for the image filters.
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
use ps_html::filters::css::{
    CssFilter, BLOCKS_REWRITTEN, FALLBACK_REWRITES, FLATTEN_CHARSET_MISMATCH,
    FLATTEN_COMPLEX_QUERIES, FLATTEN_INVALID_URL, FLATTEN_LIMIT_EXCEEDED, FLATTEN_MINIFY_FAILED,
    FLATTEN_RECURSION, PARSE_FAILURES, REWRITES_DROPPED, TOTAL_BYTES_SAVED, TOTAL_ORIGINAL_BYTES,
    USES,
};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

// Resource files.
const TOP_CSS_FILE: &str = "assets/styles.css";
const ONE_LEVEL_DOWN_FILE1: &str = "assets/nested1.css";
const ONE_LEVEL_DOWN_FILE2: &str = "assets/nested2.css";
const TWO_LEVELS_DOWN_FILE1: &str = "assets/nested/nested1.css";
const TWO_LEVELS_DOWN_FILE2: &str = "assets/nested/nested2.css";
const CSS_404_FILE: &str = "404.css";
const SIMPLE_CSS_FILE: &str = "simple.css";
const COMPLEX_CSS_FILE: &str = "complex.css";

// Contents (already minimized; note relative paths).
const TWO_LEVELS_DOWN_CONTENTS1: &str =
    ".background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}";
const TWO_LEVELS_DOWN_CONTENTS2: &str =
    ".background_green{background-color:#0f0}.foreground_rose{color:rose}";
const ONE_LEVEL_DOWN_CSS1: &str =
    ".background_blue{background-color:#00f}.foreground_gray{color:gray}";
const ONE_LEVEL_DOWN_CSS2: &str =
    ".background_white{background-color:#fff}.foreground_black{color:#000}";
const TOP_CSS: &str = ".background_red{background-color:red}.foreground_yellow{color:#ff0}";
const SIMPLE_CSS: &str = ".background_red{background-color:red}.foreground_yellow{color:#ff0}";
const COMPLEX_CSS: &str =
    "  @media screen and (min-width: 240px) {  .background_red{background-color:red}}";

/// Validation flags (`CssRewriteTestBase::ValidationFlags` subset).
const EXPECT_SUCCESS: u32 = 1;
const EXPECT_CACHED: u32 = 2;
const EXPECT_NO_CHANGE: u32 = 4;
const EXPECT_FALLBACK: u32 = 8;
const EXPECT_FAILURE: u32 = 16;
const NO_STAT_CHECK: u32 = 32;
const NO_OTHER_CONTEXTS: u32 = 128;
const FLATTEN_CHARSET_MISMATCH_FLAG: u32 = 1 << 15;
const FLATTEN_INVALID_URL_FLAG: u32 = 1 << 16;
const FLATTEN_LIMIT_EXCEEDED_FLAG: u32 = 1 << 17;
const FLATTEN_MINIFY_FAILED_FLAG: u32 = 1 << 18;
const FLATTEN_RECURSION_FLAG: u32 = 1 << 19;
const FLATTEN_COMPLEX_QUERIES_FLAG: u32 = 1 << 20;

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    debug_message: RefCell<String>,
    html_headers: RefCell<Option<ResponseHeaders>>,
}

impl Fixture {
    /// Base fixture (`CssFlattenImportsTest`).
    fn new() -> Self {
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .enable_filter(Filter::FlattenCssImports);
        options
            .borrow_mut()
            .enable_filter(Filter::ExtendCacheImages);
        options.borrow_mut().set_always_rewrite_css(true);
        Self::build(options)
    }

    /// Flatten-only fixture (`CssFlattenImportsOnlyTest`).
    fn flatten_only() -> Self {
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .enable_filter(Filter::FlattenCssImports);
        options.borrow_mut().set_always_rewrite_css(true);
        Self::build(options)
    }

    /// Flatten-plus-images fixture
    /// (`CssFlattenImportsAndRewriteImagesTest`).
    fn flatten_and_images() -> Self {
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .enable_filter(Filter::FlattenCssImports);
        options.borrow_mut().enable_filter(Filter::RecompressPng);
        options.borrow_mut().set_always_rewrite_css(true);
        Self::build(options)
    }

    fn build(options: Rc<RefCell<RewriteOptions>>) -> Self {
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
        let fixture = Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            debug_message: RefCell::new(String::new()),
            html_headers: RefCell::new(None),
        };
        fixture.setup_responses();
        fixture
    }

    /// Standard file tree (`SetUpResponses`).
    fn setup_responses(&self) {
        self.mock_css(
            &format!("{TEST_DOMAIN}{TOP_CSS_FILE}"),
            top_css_contents().as_bytes(),
            100,
        );
        self.mock_css(
            &format!("{TEST_DOMAIN}{ONE_LEVEL_DOWN_FILE1}"),
            one_level_down_contents1().as_bytes(),
            100,
        );
        self.mock_css(
            &format!("{TEST_DOMAIN}{ONE_LEVEL_DOWN_FILE2}"),
            one_level_down_contents2().as_bytes(),
            100,
        );
        self.mock_css(
            &format!("{TEST_DOMAIN}{TWO_LEVELS_DOWN_FILE1}"),
            TWO_LEVELS_DOWN_CONTENTS1.as_bytes(),
            100,
        );
        self.mock_css(
            &format!("{TEST_DOMAIN}{TWO_LEVELS_DOWN_FILE2}"),
            TWO_LEVELS_DOWN_CONTENTS2.as_bytes(),
            100,
        );
        self.mock_css(
            &format!("{TEST_DOMAIN}{COMPLEX_CSS_FILE}"),
            COMPLEX_CSS.as_bytes(),
            100,
        );
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 404, "Not Found");
        headers.compute_caching();
        self.fetcher
            .set_response(&format!("{TEST_DOMAIN}{CSS_404_FILE}"), headers, b"");
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        if let Some(headers) = self.html_headers.borrow().as_ref() {
            driver.set_response_headers(headers.clone());
        }
        driver
    }

    /// Stages the driver (HTML) headers (`set_response_headers_ptr`).
    fn set_html_headers(&self, headers: ResponseHeaders) {
        *self.html_headers.borrow_mut() = Some(headers);
    }

    /// Enables Debug and stages the expected message
    /// (`DebugWithMessage`).
    fn debug_with_message(&self, message: &str) {
        self.options.borrow_mut().enable_filter(Filter::Debug);
        *self.debug_message.borrow_mut() = message.to_string();
    }

    /// Renders the staged message for a URL (`DebugMessage`).
    fn debug_message_for(&self, url: &str) -> String {
        self.debug_message.borrow().replace("%url%", url)
    }

    /// Scripts a cacheable CSS fetch (default headers, no validators).
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

    /// Scripts a CSS fetch with an explicit content type.
    fn mock_css_with_content_type(
        &self,
        url: &str,
        content_type: &str,
        content: &[u8],
        ttl_sec: i64,
    ) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("text/css"),
            "",
            "",
            &mut headers,
        );
        headers.headers_mut().replace("Content-Type", content_type);
        headers.set_date_and_caching(self.server.now_ms(), ttl_sec * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(url, headers, content);
    }

    /// Parses through the CSS filter.
    fn parse_bytes(&self, url: &str, html: &[u8]) -> Vec<u8> {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html);
        parser.finish();
        sink.bytes()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
        driver.add_filter(Rc::new(CssFilter::new(driver.clone())));
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        let handled = driver.fetch_resource(url, &mut fetch);
        assert_eq!(handled, fetch.done());
        if handled && fetch.success() {
            Some((fetch.body().to_vec(), fetch.response_headers().clone()))
        } else {
            None
        }
    }

    /// Serves through a fresh server sharing the fetcher and options
    /// (`ServeResourceFromNewContext`).
    fn serve_from_new_context(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let mut fresh = ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            self.fetcher.clone(),
            START_MS,
        );
        fresh.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        let server = Rc::new(fresh);
        let options = Rc::new(RefCell::new(self.options.borrow().clone()));
        let driver = Rc::new(ResourceDriver::new(server, options, url));
        driver.add_filter(Rc::new(CssFilter::new(driver.clone())));
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(url, ctx);
        let handled = driver.fetch_resource(url, &mut fetch);
        assert_eq!(handled, fetch.done());
        if handled && fetch.success() {
            Some((fetch.body().to_vec(), fetch.response_headers().clone()))
        } else {
            None
        }
    }

    fn stat(&self, name: &str) -> i64 {
        self.server.variable(name).get()
    }

    fn up_stat(&self, name: &str) -> i64 {
        self.server.up_down_counter(name).get()
    }

    /// Clears filter stats (`ResetStats`).
    fn reset_stats(&self) {
        for name in [
            BLOCKS_REWRITTEN,
            FALLBACK_REWRITES,
            PARSE_FAILURES,
            TOTAL_ORIGINAL_BYTES,
            USES,
            FLATTEN_CHARSET_MISMATCH,
            FLATTEN_INVALID_URL,
            FLATTEN_LIMIT_EXCEEDED,
            FLATTEN_MINIFY_FAILED,
            FLATTEN_RECURSION,
            FLATTEN_COMPLEX_QUERIES,
        ] {
            self.server.variable(name).clear();
        }
        self.server.up_down_counter(TOTAL_BYTES_SAVED).clear();
    }

    /// Expected rewritten leaf for an arbitrary name.
    fn expected_url_for_name(&self, name: &str, expected_css_output: &[u8]) -> String {
        use ps_cache::hash::Hasher;
        use ps_html::url_segment_encoder::{CssUrlEncoder, UrlSegmentEncoder};
        let hash = self.server.hasher().hash(expected_css_output);
        let encoded = CssUrlEncoder.encode(&[name.as_bytes().to_vec()]);
        format!(
            "{}.pagespeed.cf.{hash}.css",
            String::from_utf8_lossy(&encoded)
        )
    }

    /// Builds inline-style HTML (`ValidateRewriteInlineCss` shape).
    fn inline_html(css: &[u8], debug: &str) -> Vec<u8> {
        let mut html = b"<head>\n  <title>Example style outline</title>\n  <!-- Style starts here -->\n  <style type='text/css'>".to_vec();
        html.extend_from_slice(css);
        html.extend_from_slice(b"</style>");
        html.extend_from_slice(debug.as_bytes());
        html.extend_from_slice(b"\n  <!-- Style ends here -->\n</head>");
        html
    }

    /// Builds external-link HTML (`MakeHtmlWithExternalCssLink` shape).
    fn external_html(css_url: &str, debug: &str, link_extras: &str, meta_tag: &str) -> Vec<u8> {
        format!(
            "<head>\n  <title>Example style outline</title>\n{meta_tag}  <!-- Style starts here -->\n  <link rel='stylesheet' type='text/css' href='{css_url}'{link_extras}>{debug}\n  <!-- Style ends here -->\n</head>"
        )
        .into_bytes()
    }

    /// Parses and checks stats (`ValidateWithStats`).
    fn validate_with_stats(
        &self,
        id: &str,
        html_input: &[u8],
        html_output: &[u8],
        css_input: &[u8],
        css_output: &[u8],
        flags: u32,
    ) {
        self.reset_stats();
        assert_eq!(
            self.parse_bytes(&format!("{TEST_DOMAIN}{id}.html"), html_input),
            html_output,
            "id: {id}"
        );
        if flags & NO_STAT_CHECK == 0 {
            if flags & EXPECT_SUCCESS != 0 {
                assert_eq!(self.stat(BLOCKS_REWRITTEN), 1, "id: {id}");
                assert_eq!(self.stat(FALLBACK_REWRITES), 0, "id: {id}");
                assert_eq!(self.stat(PARSE_FAILURES), 0, "id: {id}");
                assert_eq!(self.stat(REWRITES_DROPPED), 0, "id: {id}");
                assert_eq!(
                    self.up_stat(TOTAL_BYTES_SAVED),
                    css_input.len() as i64 - css_output.len() as i64,
                    "id: {id}"
                );
                assert_eq!(
                    self.stat(TOTAL_ORIGINAL_BYTES),
                    css_input.len() as i64,
                    "id: {id}"
                );
                assert_eq!(self.stat(USES), 1, "id: {id}");
            } else if flags & EXPECT_CACHED != 0 {
                assert_eq!(self.stat(BLOCKS_REWRITTEN), 0, "id: {id}");
                assert_eq!(self.stat(FALLBACK_REWRITES), 0, "id: {id}");
                assert_eq!(self.stat(PARSE_FAILURES), 0, "id: {id}");
                assert_eq!(self.up_stat(TOTAL_BYTES_SAVED), 0, "id: {id}");
                assert_eq!(self.stat(TOTAL_ORIGINAL_BYTES), 0, "id: {id}");
                assert_eq!(self.stat(USES), 1, "id: {id}");
            } else if flags & EXPECT_NO_CHANGE != 0 {
                assert_eq!(self.stat(BLOCKS_REWRITTEN), 0, "id: {id}");
                assert_eq!(self.stat(FALLBACK_REWRITES), 0, "id: {id}");
                assert_eq!(self.stat(PARSE_FAILURES), 0, "id: {id}");
                assert_eq!(self.up_stat(TOTAL_BYTES_SAVED), 0, "id: {id}");
                assert_eq!(self.stat(TOTAL_ORIGINAL_BYTES), 0, "id: {id}");
                assert_eq!(self.stat(USES), 0, "id: {id}");
            } else if flags & EXPECT_FALLBACK != 0 {
                assert_eq!(self.stat(BLOCKS_REWRITTEN), 0, "id: {id}");
                assert_eq!(self.stat(FALLBACK_REWRITES), 1, "id: {id}");
                assert_eq!(self.stat(PARSE_FAILURES), 1, "id: {id}");
                assert_eq!(self.stat(REWRITES_DROPPED), 0, "id: {id}");
                assert_eq!(self.up_stat(TOTAL_BYTES_SAVED), 0, "id: {id}");
                assert_eq!(self.stat(TOTAL_ORIGINAL_BYTES), 0, "id: {id}");
                assert_eq!(self.stat(USES), 1, "id: {id}");
            } else {
                assert_ne!(flags & EXPECT_FAILURE, 0, "id: {id}");
                assert_eq!(self.stat(BLOCKS_REWRITTEN), 0, "id: {id}");
                assert_eq!(self.stat(FALLBACK_REWRITES), 0, "id: {id}");
                assert_eq!(self.stat(PARSE_FAILURES), 1, "id: {id}");
                assert_eq!(self.stat(REWRITES_DROPPED), 0, "id: {id}");
                assert_eq!(self.up_stat(TOTAL_BYTES_SAVED), 0, "id: {id}");
                assert_eq!(self.stat(TOTAL_ORIGINAL_BYTES), 0, "id: {id}");
                assert_eq!(self.stat(USES), 0, "id: {id}");
            }
        }
        if flags & EXPECT_CACHED == 0 {
            for (flag, name) in [
                (FLATTEN_CHARSET_MISMATCH_FLAG, FLATTEN_CHARSET_MISMATCH),
                (FLATTEN_INVALID_URL_FLAG, FLATTEN_INVALID_URL),
                (FLATTEN_LIMIT_EXCEEDED_FLAG, FLATTEN_LIMIT_EXCEEDED),
                (FLATTEN_MINIFY_FAILED_FLAG, FLATTEN_MINIFY_FAILED),
                (FLATTEN_RECURSION_FLAG, FLATTEN_RECURSION),
                (FLATTEN_COMPLEX_QUERIES_FLAG, FLATTEN_COMPLEX_QUERIES),
            ] {
                assert_eq!(
                    self.stat(name),
                    if flags & flag != 0 { 1 } else { 0 },
                    "id: {id}"
                );
            }
        }
    }

    /// Validates an inline rewrite (`ValidateRewriteInlineCss`).
    fn validate_rewrite_inline_css(
        &self,
        id: &str,
        css_input: impl AsRef<[u8]>,
        css_output: impl AsRef<[u8]>,
        flags: u32,
    ) {
        let (css_input, css_output) = (css_input.as_ref(), css_output.as_ref());
        let page_url = format!("{TEST_DOMAIN}{id}.html");
        let html_input = Self::inline_html(css_input, "");
        let html_output = Self::inline_html(css_output, &self.debug_message_for(&page_url));
        self.validate_with_stats(id, &html_input, &html_output, css_input, css_output, flags);
    }

    /// Validates an external rewrite (`ValidateRewriteExternalCssUrl`).
    fn validate_rewrite_external_css_url(
        &self,
        id: &str,
        css_url: &str,
        css_input: impl AsRef<[u8]>,
        css_output: impl AsRef<[u8]>,
        flags: u32,
        link_extras: &str,
        meta_tag: &str,
    ) {
        let (css_input, css_output) = (css_input.as_ref(), css_output.as_ref());
        self.mock_css(css_url, css_input, 300);
        let html_input = Self::external_html(css_url, "", link_extras, meta_tag);
        let rewrite_url = flags & (EXPECT_SUCCESS | EXPECT_CACHED | EXPECT_FALLBACK) != 0;
        let expected_new_url = if rewrite_url {
            let base = css_url
                .rsplit_once('/')
                .map(|(base, _)| format!("{base}/"))
                .unwrap();
            let leaf = css_url.rsplit('/').next().unwrap();
            format!("{base}{}", self.expected_url_for_name(leaf, css_output))
        } else {
            css_url.to_string()
        };
        let html_output = Self::external_html(
            &expected_new_url,
            &self.debug_message_for(css_url),
            link_extras,
            meta_tag,
        );
        self.validate_with_stats(id, &html_input, &html_output, css_input, css_output, flags);
        if rewrite_url {
            let (actual, headers) = self
                .serve_url(&expected_new_url)
                .unwrap_or_else(|| panic!("serve {expected_new_url}"));
            assert_eq!(actual, css_output, "url: {expected_new_url}");
            if flags & EXPECT_FALLBACK == 0 {
                use ps_http::response::{RequestProperties, ValidatorOption, VaryOption};
                assert!(headers.is_proxy_cacheable(
                    RequestProperties::default(),
                    VaryOption::RespectVaryOnResources,
                    ValidatorOption::NoValidator,
                ));
                assert!(headers.cache_ttl_ms() >= 365 * 24 * 3600 * 1000);
            }
            if flags & NO_OTHER_CONTEXTS == 0 {
                let (actual, _) = self
                    .serve_from_new_context(&expected_new_url)
                    .unwrap_or_else(|| panic!("reserve {expected_new_url}"));
                assert_eq!(actual, css_output, "url: {expected_new_url}");
            }
        }
    }

    /// Validates an external rewrite (`ValidateRewriteExternalCss`).
    fn validate_rewrite_external_css(
        &self,
        id: &str,
        css_input: impl AsRef<[u8]> + Clone,
        css_output: impl AsRef<[u8]> + Clone,
        flags: u32,
    ) {
        self.validate_rewrite_external_css_url(
            id,
            &format!("{TEST_DOMAIN}{id}.css"),
            css_input,
            css_output,
            flags,
            "",
            "",
        );
    }

    /// Validates inline plus external (`ValidateRewrite`).
    fn validate_rewrite(
        &self,
        id: &str,
        css_input: impl AsRef<[u8]> + Clone,
        css_output: impl AsRef<[u8]> + Clone,
        flags: u32,
    ) {
        self.validate_rewrite_inline_css(
            &format!("{id}-inline"),
            css_input.clone(),
            css_output.clone(),
            flags,
        );
        self.validate_rewrite_external_css(&format!("{id}-external"), css_input, css_output, flags);
    }
}

fn one_level_down_contents1() -> String {
    format!("@import url(nested/nested1.css);{ONE_LEVEL_DOWN_CSS1}")
}

fn one_level_down_contents2() -> String {
    format!("@import url(nested/nested2.css);{ONE_LEVEL_DOWN_CSS2}")
}

fn top_css_contents() -> String {
    format!("@import url(nested1.css);@import url(nested2.css);{TOP_CSS}")
}

fn flattened_top_css_contents() -> String {
    format!(
        "{TWO_LEVELS_DOWN_CONTENTS1}{ONE_LEVEL_DOWN_CSS1}{TWO_LEVELS_DOWN_CONTENTS2}{ONE_LEVEL_DOWN_CSS2}{TOP_CSS}"
    )
}

fn flattened_one_level_down_contents1() -> String {
    format!("{TWO_LEVELS_DOWN_CONTENTS1}{ONE_LEVEL_DOWN_CSS1}")
}

#[test]
fn flatten_inline_css() {
    let fixture = Fixture::new();
    let css_in = "@import url(http://test.com/simple.css);";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    fixture.validate_rewrite_inline_css("flatten_simple", css_in, SIMPLE_CSS, EXPECT_SUCCESS);
    let expected_savings = css_in.len() as i64 - SIMPLE_CSS.len() as i64;
    assert_eq!(fixture.up_stat(TOTAL_BYTES_SAVED), expected_savings);
}

#[test]
fn dont_flatten_attribute_css() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributes);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    // Test that rewriting of attributes is enabled and working.
    assert_eq!(
        String::from_utf8(fixture.parse_bytes(
            &format!("{TEST_DOMAIN}rewrite-attribute-setup.html"),
            b"<div style='background-color: #f00; color: yellow;'/>"
        ))
        .unwrap(),
        "<div style='background-color:red;color:#ff0'/>"
    );
    // Test that we don't rewrite @import's in attributes since that's
    // invalid.
    assert_eq!(
        String::from_utf8(fixture.parse_bytes(
            &format!("{TEST_DOMAIN}rewrite-attribute.html"),
            b"<div style='@import url(http://test.com/simple.css)'/>"
        ))
        .unwrap(),
        "<div style='@import url(http://test.com/simple.css)'/>"
    );
}

#[test]
fn flatten_noop() {
    let fixture = Fixture::new();
    fixture.validate_rewrite_external_css("flatten_noop", SIMPLE_CSS, SIMPLE_CSS, EXPECT_SUCCESS);
}

#[test]
fn flatten_404() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--4xx status code, preventing rewriting of http://test.com/404.css-->",
    );
    let css_in = "@import url(http://test.com/404.css);";
    fixture.validate_rewrite_external_css("flatten_404", css_in, css_in, EXPECT_SUCCESS);
}

#[test]
fn dont_flatten_with_unauthorized_css() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: Cannot import http://unauth.com/assets/styles.css as it is on an unauthorized domain-->",
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    let unauthorized_import_css = "@import url(http://unauth.com/assets/styles.css);\n@import url(http://test.com/simple.css);\na { color:red }";
    let rewritten_unauthorized_import_css = "@import url(http://unauth.com/assets/styles.css);@import url(http://test.com/simple.css);a{color:red}";
    fixture.validate_rewrite_external_css(
        "dont_flatten_unauthorized_css_import",
        unauthorized_import_css,
        rewritten_unauthorized_import_css,
        EXPECT_SUCCESS,
    );
    let authorized_top_level_css =
        "@import url(auth_parent_with_unauth_child_import.css);b { color: blue }";
    let rewritten_authorized_top_level_css =
        "@import url(auth_parent_with_unauth_child_import.css);b{color:#00f}";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}auth_parent_with_unauth_child_import.css"),
        unauthorized_import_css.as_bytes(),
        100,
    );
    fixture.validate_rewrite_external_css(
        "dont_flatten_nested_unauthorized_css_import",
        authorized_top_level_css,
        rewritten_authorized_top_level_css,
        EXPECT_SUCCESS,
    );
}

#[test]
fn flatten_invalid_css() {
    let fixture = Fixture::new();
    fixture.debug_with_message("<!--CSS rewrite failed: Parse error in %url%-->");
    fixture.validate_rewrite_external_css(
        "flatten_invalid_css_media",
        "@media }}",
        "@media }}",
        EXPECT_FAILURE,
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}styles.css"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    // This gets a parse error but thanks to the idea of "unparseable
    // sections" in the CSS parser it's not treated as an error and the
    // "bad" text is kept. Because the error was in the bogus @import
    // statement, we do NOT flatten.
    fixture.debug_with_message("");
    fixture.validate_rewrite_external_css(
        "flatten_unparseable_css_import",
        "@import styles.css; a { color:red }",
        "@import styles.css;a{color:red}",
        EXPECT_SUCCESS,
    );
    // Same as above, but since the @import itself is valid we DO flatten.
    let flattened_invalid = format!("{SIMPLE_CSS}a{{#color: 333 }}");
    fixture.debug_with_message("");
    fixture.validate_rewrite_external_css(
        "flatten_unparseable_css_rule",
        "@import url(styles.css) ;a{ #color: 333 }",
        &flattened_invalid,
        EXPECT_SUCCESS,
    );
    // This gets a non-recoverable parse error because of mismatched
    // {}s. We do not want to recover from these types of parse errors
    // because combining/flattening files like this would spread the
    // breakage.
    fixture.debug_with_message("<!--CSS rewrite failed: Parse error in %url%-->");
    let error_css = "@import url(styles.css);a{{ color:red }";
    fixture.validate_rewrite_external_css(
        "no_flatten_error_css_rule",
        error_css,
        error_css,
        EXPECT_FAILURE,
    );
    // Make sure we don't flatten if the @imported CSS has a
    // non-recoverable parse error.
    fixture.mock_css(
        &format!("{TEST_DOMAIN}error.css"),
        b"a {{ color: red }",
        100,
    );
    // Note: Rewrite succeeds, but flatten fails.
    fixture.debug_with_message(
        "<!--Flattening failed: Cannot parse the CSS in http://test.com/error.css-->",
    );
    fixture.validate_rewrite_external_css(
        "no_flatten_error_in_import",
        "@import url(error.css); body { color: #000 }",
        "@import url(error.css);body{color:#000}",
        EXPECT_SUCCESS | FLATTEN_MINIFY_FAILED_FLAG,
    );
    // Note that this is not run as ValidateRewrite(), because that
    // would re-use the rewritten value and thus not increment
    // kFlattenImportsMinifyFailed the second time.
}

#[test]
fn flatten_empty_media() {
    let fixture = Fixture::new();
    fixture.validate_rewrite_external_css("flatten_empty_media", "@media {}", "", EXPECT_SUCCESS);
}

#[test]
fn flatten_simple() {
    let fixture = Fixture::new();
    let css_in = "@import url(http://test.com/simple.css);";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    fixture.validate_rewrite_external_css("flatten_simple", css_in, SIMPLE_CSS, EXPECT_SUCCESS);
    // Check things work when data is already cached.
    fixture.validate_rewrite_external_css(
        "flatten_simple",
        css_in,
        SIMPLE_CSS,
        EXPECT_CACHED | NO_OTHER_CONTEXTS,
    );
}

/// Flattening-limit matrix (`TestLimit`).
fn test_limit(
    fixture: &Fixture,
    test_id: &str,
    limit_exceeded: bool,
    flattening_limit: i64,
    actual_amount: i64,
    css_in: &str,
    css_out: &str,
) {
    fixture
        .options
        .borrow_mut()
        .set_css_flatten_max_bytes(flattening_limit);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    let mut flags = EXPECT_SUCCESS;
    if limit_exceeded {
        flags |= FLATTEN_LIMIT_EXCEEDED_FLAG;
        fixture.debug_with_message(&format!(
            "<!--Flattening failed: Flattening limit ({flattening_limit}) exceeded ({actual_amount})-->"
        ));
    } else {
        fixture.debug_with_message("");
    }
    fixture.validate_rewrite_external_css(test_id, css_in, css_out, flags);
    // The fetcher is cleared in C++, so content must come from the
    // cache; kNoOtherContexts because other contexts lack the value.
    fixture.validate_rewrite_external_css(
        test_id,
        css_in,
        css_out,
        EXPECT_CACHED | NO_OTHER_CONTEXTS | (flags & FLATTEN_LIMIT_EXCEEDED_FLAG),
    );
}

#[test]
fn flatten_under_large_limit() {
    let fixture = Fixture::new();
    let css_in =
        "@import url(http://test.com/simple.css);\n@import url(http://test.com/simple.css);\n";
    let css_out = format!("{SIMPLE_CSS}{SIMPLE_CSS}");
    test_limit(
        &fixture,
        "flatten_under_limit",
        false,
        1 + css_out.len() as i64,
        0,
        css_in,
        &css_out,
    );
}

#[test]
fn dont_flatten_over_medium_limit() {
    let fixture = Fixture::new();
    let css_in =
        "@import url(http://test.com/simple.css);\n@import url(http://test.com/simple.css);\n";
    let css_out =
        "@import url(http://test.com/simple.css);@import url(http://test.com/simple.css);";
    test_limit(
        &fixture,
        "dont_flatten_over_limit",
        true,
        1 + css_out.len() as i64,
        54 + css_out.len() as i64,
        css_in,
        css_out,
    );
}

#[test]
fn dont_flatten_over_tiny_limit() {
    let fixture = Fixture::new();
    let css_in =
        "@import url(http://test.com/simple.css);\n@import url(http://test.com/simple.css);\n";
    let css_out =
        "@import url(http://test.com/simple.css);@import url(http://test.com/simple.css);";
    test_limit(
        &fixture,
        "dont_flatten_over_tiny_limit",
        true,
        10,
        67,
        css_in,
        css_out,
    );
}

#[test]
fn flatten_empty() {
    let fixture = Fixture::new();
    // We intentionally do not inline any empty resources.
    fixture.mock_css(&format!("{TEST_DOMAIN}empty.css"), b"", 100);
    let css_in = "@import url(http://test.com/empty.css);";
    fixture.validate_rewrite_external_css("flatten_empty", css_in, css_in, EXPECT_SUCCESS);
    // Check things work when data is already cached.
    fixture.validate_rewrite_external_css(
        "flatten_empty",
        css_in,
        css_in,
        EXPECT_CACHED | NO_OTHER_CONTEXTS,
    );
}

#[test]
fn flatten_simple_rewrite_on_the_fly() {
    let fixture = Fixture::new();
    // import.css @import's simple.css; fetching the rewritten
    // filename yields the flattened contents.
    fixture.mock_css(
        &format!("{TEST_DOMAIN}import.css"),
        b"@import url(http://test.com/simple.css);",
        100,
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    // NOTE: cache-activity counts wait for a counting backend.
    let (content, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.import.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(content, SIMPLE_CSS.as_bytes());
}

#[test]
fn flatten_nested() {
    let fixture = Fixture::new();
    let css_in = format!("@import url(http://test.com/{TOP_CSS_FILE});");
    fixture.validate_rewrite_external_css(
        "flatten_nested",
        &css_in,
        &flattened_top_css_contents(),
        EXPECT_SUCCESS,
    );
}

#[test]
fn flatten_from_cache_directly() {
    let fixture = Fixture::new();
    // NOTE: cache-activity counts wait for a counting backend.
    let css_in = format!("@import url(http://test.com/{TOP_CSS_FILE});");
    fixture.validate_rewrite_external_css(
        "flatten_from_cache_directly",
        &css_in,
        &flattened_top_css_contents(),
        EXPECT_SUCCESS,
    );
    fixture.validate_rewrite_external_css(
        "flatten_from_cache_directly",
        &css_in,
        &flattened_top_css_contents(),
        EXPECT_CACHED | NO_OTHER_CONTEXTS,
    );
    // Access one of the cached ones directly.
    let css_in = format!("@import url(http://test.com/{TWO_LEVELS_DOWN_FILE1});");
    fixture.validate_rewrite_external_css(
        "flatten_from_cache_directly_repeat",
        &css_in,
        TWO_LEVELS_DOWN_CONTENTS1,
        EXPECT_SUCCESS | NO_OTHER_CONTEXTS,
    );
}

#[test]
fn flatten_recursion() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: Recursive @import of http://test.com/recursive.css-->",
    );
    let css_in = "@import url(http://test.com/recursive.css);";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}recursive.css"),
        css_in.as_bytes(),
        100,
    );
    fixture.validate_rewrite_external_css(
        "flatten_recursive",
        css_in,
        css_in,
        EXPECT_SUCCESS | FLATTEN_RECURSION_FLAG,
    );
}

#[test]
fn flatten_simple_media() {
    let fixture = Fixture::new();
    let css_in = "@import url(http://test.com/simple.css) screen;";
    let css_out = format!("@media screen{{{SIMPLE_CSS}}}");
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{SIMPLE_CSS_FILE}"),
        css_out.as_bytes(),
        100,
    );
    fixture.validate_rewrite_external_css("flatten_simple_media", css_in, &css_out, EXPECT_SUCCESS);
    // Check things work when data is already cached.
    fixture.validate_rewrite_external_css(
        "flatten_simple_media",
        css_in,
        &css_out,
        EXPECT_CACHED | NO_OTHER_CONTEXTS,
    );
}

#[test]
fn flatten_nested_media() {
    let fixture = Fixture::new();
    let styles_contents = format!(
        "@import url(print.css) print;@import url(screen.css) screen;@media all{{{SIMPLE_CSS}}}"
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}styles.css"),
        styles_contents.as_bytes(),
        100,
    );
    let print_css = ".background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}";
    let print_all_css = ".background_green{background-color:#0f0}.foreground_rose{color:rose}";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}print.css"),
        format!("@import url(screen.css) screen;{print_css}@media all{{{print_all_css}}}")
            .as_bytes(),
        100,
    );
    let screen_css = ".background_blue{background-color:#00f}.foreground_gray{color:gray}";
    let screen_all_css = ".background_white{background-color:#fff}.foreground_black{color:#000}";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}screen.css"),
        format!("@import url(print.css) print;{screen_css}@media all{{{screen_all_css}}}")
            .as_bytes(),
        100,
    );
    let css_in = "@import url(http://test.com/styles.css);";
    let css_out = format!(
        "@media print{{{print_css}{print_all_css}}}@media screen{{{screen_css}{screen_all_css}}}{SIMPLE_CSS}"
    );
    fixture.validate_rewrite_external_css("flatten_nested_media", css_in, &css_out, EXPECT_SUCCESS);
    // Check things work when data is already cached.
    fixture.validate_rewrite_external_css(
        "flatten_nested_media",
        css_in,
        &css_out,
        EXPECT_CACHED | NO_OTHER_CONTEXTS,
    );
}

#[test]
fn flatten_all_media() {
    let fixture = Fixture::new();
    fixture.mock_css(
        &format!("{TEST_DOMAIN}all.css"),
        b"*{display: inline-block;}",
        100,
    );
    fixture.validate_rewrite_external_css(
        "flatten_all_media",
        "@import url(all.css) all;",
        "*{display:inline-block}",
        EXPECT_SUCCESS,
    );
    // Now double-check against an incompatible one.
    fixture.mock_css(
        &format!("{TEST_DOMAIN}print.css"),
        b"img{display: none;}",
        100,
    );
    fixture.validate_rewrite_external_css(
        "flatten_all_media2",
        "@import url(print.css) print;",
        "@media print{img{display:none}}",
        EXPECT_SUCCESS,
    );
}

#[test]
fn flatten_font_face() {
    let fixture = Fixture::new();
    fixture.mock_css(
        &format!("{TEST_DOMAIN}font.css"),
        b"@font-face { font-family: 'cyborgo'; }",
        100,
    );
    fixture.validate_rewrite_external_css(
        "flatten_font_face",
        "@import url(font.css);",
        "@font-face{font-family:'cyborgo'}",
        EXPECT_SUCCESS,
    );
}

#[test]
fn flatten_cache_depends_on_media() {
    let fixture = Fixture::new();
    // NOTE: cache-activity counts wait for a counting backend.
    let css_screen = format!("@media screen{{{SIMPLE_CSS}}}");
    let css_print =
        "@media print{.background_white{background-color:#fff}.foreground_black{color:#000}}";
    let css_contents = format!("{css_screen}{css_print}");
    fixture.mock_css(
        &format!("{TEST_DOMAIN}mixed.css"),
        css_contents.as_bytes(),
        100,
    );
    // When we @import with media screen we should cache the file in
    // its entirety, and the screen-specific results, separately.
    let screen_in = "@import url(http://test.com/mixed.css) screen;";
    fixture.validate_rewrite_external_css(
        "flatten_mixed_media_screen",
        screen_in,
        &css_screen,
        EXPECT_SUCCESS,
    );
    // When we @import with media print we should find the cached file
    // but generate and cache the print-specific results.
    let print_in = "@import url(http://test.com/mixed.css) print;";
    fixture.validate_rewrite_external_css(
        "flatten_mixed_media_print",
        print_in,
        css_print,
        EXPECT_SUCCESS,
    );
    // Now when we @import with media screen we should find cached data.
    fixture.validate_rewrite_external_css(
        "flatten_mixed_media_screen_repeat",
        screen_in,
        &css_screen,
        EXPECT_SUCCESS,
    );
    // Ditto for re-fetching print.
    fixture.validate_rewrite_external_css(
        "flatten_mixed_media_print_repeat",
        print_in,
        css_print,
        EXPECT_SUCCESS,
    );
}

/// Charset matrix (`TestFlattenWithHtmlCharset`): header/meta/equiv
/// charsets drive the HTML side; the CSS side is always UTF-8.
fn test_flatten_with_html_charset(
    header_charset: &str,
    meta_tag_charset: &str,
    http_equiv_charset: &str,
    should_succeed: bool,
    debug_message: &str,
) {
    let fixture = Fixture::new();
    fixture.debug_with_message(debug_message);
    let styles_contents =
        format!("@charset \"UTF-8\";@import url(print.css);@import url(screen.css);{SIMPLE_CSS}");
    fixture.mock_css_with_content_type(
        &format!("{TEST_DOMAIN}styles.css"),
        "text/css; charset=utf-8",
        styles_contents.as_bytes(),
        100,
    );
    let mut driver_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    if !header_charset.is_empty() {
        driver_headers.headers_mut().add(
            "Content-Type",
            &format!("text/css; charset={header_charset}"),
        );
    }
    driver_headers.compute_caching();
    fixture.set_html_headers(driver_headers);
    let print_css = ".background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}print.css"),
        print_css.as_bytes(),
        100,
    );
    let screen_css = ".background_blue{background-color:#00f}.foreground_gray{color:gray}";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}screen.css"),
        format!("@charset \"UtF-8\";{screen_css}").as_bytes(),
        100,
    );
    let css_in = "@import url(http://test.com/styles.css);";
    let meta_tag = if meta_tag_charset == "utf-8" {
        "  <meta charset=\"utf-8\">"
    } else if meta_tag_charset == "iso-8859-1" {
        "  <meta charset=ISO-8859-1>"
    } else if http_equiv_charset == "utf-8" {
        "  <meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">"
    } else if http_equiv_charset == "iso-8859-1" {
        "  <meta http-equiv=\"Content-Type\" content=text/html; charset=ISO-8859-1>"
    } else {
        ""
    };
    assert!(meta_tag_charset.is_empty() || http_equiv_charset.is_empty());
    if should_succeed {
        let css_out = format!("{print_css}{screen_css}{SIMPLE_CSS}");
        fixture.validate_rewrite_external_css_url(
            "flatten_nested_media",
            &format!("{TEST_DOMAIN}flatten_nested_media.css"),
            css_in.as_bytes(),
            css_out.as_bytes(),
            EXPECT_SUCCESS | NO_OTHER_CONTEXTS,
            "",
            meta_tag,
        );
        // Check things work when data is already cached.
        fixture.validate_rewrite_external_css_url(
            "flatten_nested_media",
            &format!("{TEST_DOMAIN}flatten_nested_media.css"),
            css_in.as_bytes(),
            css_out.as_bytes(),
            EXPECT_CACHED | NO_OTHER_CONTEXTS,
            "",
            meta_tag,
        );
    } else {
        fixture.validate_rewrite_external_css_url(
            "flatten_nested_media",
            &format!("{TEST_DOMAIN}flatten_nested_media.css"),
            css_in.as_bytes(),
            css_in.as_bytes(),
            EXPECT_SUCCESS | NO_OTHER_CONTEXTS | FLATTEN_CHARSET_MISMATCH_FLAG,
            "",
            meta_tag,
        );
        fixture.validate_rewrite_external_css_url(
            "flatten_nested_media",
            &format!("{TEST_DOMAIN}flatten_nested_media.css"),
            css_in.as_bytes(),
            css_in.as_bytes(),
            EXPECT_CACHED | NO_OTHER_CONTEXTS,
            "",
            meta_tag,
        );
    }
}

#[test]
fn flatten_nested_charsets_ok() {
    // HTML = utf-8 (1st argument), CSS = utf-8 (always).
    test_flatten_with_html_charset("utf-8", "", "", true, "");
}

#[test]
fn flatten_nested_charsets_mismatch() {
    // HTML = iso-8859-1 (default), CSS = utf-8 (always).
    test_flatten_with_html_charset(
        "",
        "",
        "",
        false,
        "<!--Flattening failed: The charset of http://test.com/styles.css (utf-8 from headers) is different from that of its parent (inline): iso-8859-1 from unknown-->",
    );
}

#[test]
fn flatten_fails_if_link_has_wrong_charset() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: The charset of the HTML (iso-8859-1, the default) is different from the charset attribute on the preceding element (utf-8)-->",
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}styles.css"),
        SIMPLE_CSS.as_bytes(),
        100,
    );
    let css_in = "@import url(http://test.com/styles.css);";
    fixture.validate_rewrite_external_css_url(
        "flatten_link_charset",
        &format!("{TEST_DOMAIN}flatten_link_charset.css"),
        css_in.as_bytes(),
        css_in.as_bytes(),
        EXPECT_SUCCESS | NO_OTHER_CONTEXTS | FLATTEN_CHARSET_MISMATCH_FLAG,
        " charset='utf-8'",
        "",
    );
}

#[test]
fn flatten_respects_meta_tag_charset() {
    // HTML = utf-8 (2nd argument), CSS = utf-8 (always).
    test_flatten_with_html_charset("", "utf-8", "", true, "");
}

#[test]
fn flatten_respects_http_equiv_charset() {
    // HTML = utf-8 (3rd argument), CSS = utf-8 (always).
    test_flatten_with_html_charset("", "", "utf-8", true, "");
}

#[test]
fn flatten_respects_http_equiv_charset_unquoted() {
    // HTML = iso-8859-1 (3rd argument), CSS = utf-8 (always).
    test_flatten_with_html_charset(
        "",
        "",
        "iso-8859-1",
        false,
        "<!--Flattening failed: The charset of http://test.com/styles.css (utf-8 from headers) is different from that of its parent (inline): ISO-8859-1 from unknown-->",
    );
}

#[test]
// NOTE: upstream `Precendence` typo kept for name parity.
fn header_takes_precendence_over_meta_tag1() {
    // HTML = utf-8 (1st argument), CSS = utf-8 (always).
    test_flatten_with_html_charset("utf-8", "iso-8859-1", "", true, "");
}

#[test]
// NOTE: upstream `Precendence` typo kept for name parity.
fn header_takes_precendence_over_meta_tag2() {
    // HTML = iso-8859-1 (1st argument), CSS = utf-8 (always).
    test_flatten_with_html_charset(
        "iso-8859-1",
        "utf-8",
        "",
        false,
        "<!--Flattening failed: The charset of http://test.com/styles.css (utf-8 from headers) is different from that of its parent (inline): iso-8859-1 from unknown-->",
    );
}

#[test]
fn invalid_grandchild_url() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: Invalid import URL //// in http://test.com/child.css-->",
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}child.css"),
        b"@import url(////);",
        100,
    );
    fixture.validate_rewrite_external_css(
        "invalid_url",
        "@import 'child.css';",
        "@import url(child.css);",
        EXPECT_SUCCESS | FLATTEN_INVALID_URL_FLAG,
    );
}

#[test]
fn no_flatten_media_queries() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: Complex media queries in the @import of inline-->",
    );
    fixture.validate_rewrite(
        "media_queries",
        "@import url(child.css) not screen;",
        "@import url(child.css) not screen;",
        EXPECT_SUCCESS | FLATTEN_COMPLEX_QUERIES_FLAG,
    );
}

#[test]
fn no_flatten_media_queries_child() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: Complex media queries in the @import of inline-->",
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}child.css"),
        b"@import url(g.css) screen and (color);",
        100,
    );
    fixture.validate_rewrite_external_css(
        "invalid_url",
        "@import 'child.css';",
        "@import url(child.css);",
        EXPECT_SUCCESS | FLATTEN_COMPLEX_QUERIES_FLAG,
    );
}

#[test]
fn flatten_too_complex_nested() {
    let fixture = Fixture::new();
    let css_in = format!("@import url(http://test.com/{COMPLEX_CSS_FILE});");
    // First time should load the CSS files into cache.
    fixture.validate_rewrite_external_css(
        "flatten_too_complex_nested",
        &css_in,
        &css_in,
        EXPECT_SUCCESS | FLATTEN_COMPLEX_QUERIES_FLAG,
    );
    // Re-optimize. The CSS output should equal the input (and
    // especially not be an empty string).
    fixture.validate_rewrite_external_css(
        "flatten_too_complex_nested_repeat",
        &css_in,
        &css_in,
        EXPECT_SUCCESS | FLATTEN_COMPLEX_QUERIES_FLAG,
    );
}

#[test]
fn merge_media_queries() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: A media query is too complex in http://test.com/child.css-->",
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}child.css"),
        b"@media screen and (color) { .a { color: red; } }\n@media print and (max-width: 400px), only screen { .b { color: blue } }",
        100,
    );
    fixture.validate_rewrite_external_css(
        "invalid_url",
        "@import url(child.css) screen;",
        "@import url(child.css) screen;",
        EXPECT_SUCCESS | FLATTEN_COMPLEX_QUERIES_FLAG,
    );
}

#[test]
fn no_flatten_media_queries_at_media() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--Flattening failed: A media query is too complex in http://test.com/child.css-->",
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}child.css"),
        b"@media screen and (color) { .a { color: red; } }\n@media not print and (max-width: 400px) { .b { color: blue; } }\n",
        100,
    );
    fixture.validate_rewrite_external_css(
        "invalid_url",
        "@import url(child.css) screen;",
        "@import url(child.css) screen;",
        EXPECT_SUCCESS | FLATTEN_COMPLEX_QUERIES_FLAG,
    );
}

/// Relative image URLs in relative CSS (`TestRelativeImageUrlInRelativeCssUrl`).
fn test_relative_image_url_in_relative_css_url(trim_urls: bool, cache_extend: bool) {
    assert!(
        !cache_extend,
        "cache-extending variants wait for nested image contexts"
    );
    let fixture = Fixture::flatten_only();
    if trim_urls {
        fixture
            .options
            .borrow_mut()
            .enable_filter(Filter::LeftTrimUrls);
    }
    // Setup the image we refer to.
    let foo_png_path = format!("{TEST_DOMAIN}a/images/foo.png");
    fixture.mock_css(
        &foo_png_path,
        b"Invalid PNG but does not matter for this test",
        100,
    );
    // Setup the CSS that refers to it.
    let template = ".background_red{background-color:red}.foreground_yellow{color:#ff0}.body{background-image:url(%s)}";
    // The input CSS refers to ../images/foo.png from /a/b/simple.css,
    // so the image path is /a/images/foo.png, used when the CSS is
    // flattened into the base document (base '/').
    let simple_css_path = format!("{TEST_DOMAIN}a/b/{SIMPLE_CSS_FILE}");
    fixture.mock_css(
        &simple_css_path,
        template.replace("%s", "../images/foo.png").as_bytes(),
        100,
    );
    let import_simple_css = format!("@import url({simple_css_path});");
    let foo_png_output = format!(
        "{}a/images/foo.png",
        if trim_urls { "" } else { TEST_DOMAIN }
    );
    let simple_css_out = template.replace("%s", &foo_png_output);
    fixture.validate_rewrite_inline_css(
        "flatten_relative",
        import_simple_css.as_bytes(),
        simple_css_out.as_bytes(),
        EXPECT_SUCCESS,
    );
}

/// Nested flattening with image URLs (`TestFlattenNested`,
/// non-cache-extending halves).
fn test_flatten_nested(trim_urls: bool) {
    let fixture = Fixture::flatten_only();
    if trim_urls {
        fixture
            .options
            .borrow_mut()
            .enable_filter(Filter::LeftTrimUrls);
    }
    // /foo.png
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.png"),
        b"Invalid PNG but does not matter for this test",
        100,
    );
    let foo_domain = if trim_urls { "" } else { TEST_DOMAIN }.to_string();
    let foo_path = format!("{foo_domain}foo.png");
    // /image1.css loads /foo.png as a background image.
    let css1_before = "body {\n  background-image: url(foo.png);\n}\n";
    let css1_after = format!("body{{background-image:url({foo_path})}}");
    fixture.mock_css(
        &format!("{TEST_DOMAIN}image1.css"),
        css1_before.as_bytes(),
        100,
    );
    // /nested/bar.png
    fixture.mock_css(
        &format!("{TEST_DOMAIN}nested/bar.png"),
        b"Invalid PNG but does not matter for this test",
        100,
    );
    let bar_domain = if trim_urls {
        "nested/".to_string()
    } else {
        format!("{TEST_DOMAIN}nested/")
    };
    let bar_path = format!("{bar_domain}bar.png");
    // /nested/image2.css loads /nested/bar.png & /foo.png.
    let css2_before = "body {\n  background-image: url(bar.png);\n}\ndiv {\n  background-image: url(../foo.png);\n}\n";
    let css2_after =
        format!("body{{background-image:url({bar_path})}}div{{background-image:url({foo_path})}}");
    fixture.mock_css(
        &format!("{TEST_DOMAIN}nested/image2.css"),
        css2_before.as_bytes(),
        100,
    );
    // /foo-then-bar.css @imports image1.css then nested/image2.css.
    let top1_before = "@import url(image1.css);@import url(nested/image2.css);";
    let top1_after = format!("{css1_after}{css2_after}");
    // /bar-then-foo.css @imports nested/image2.css then image1.css.
    let top2_before = "@import url(nested/image2.css);@import url(image1.css);";
    let top2_after = format!("{css2_after}{css1_after}");
    // Phew! Load them both. bar-then-foo.css should use cached data.
    fixture.validate_rewrite_external_css(
        "flatten_then_cache_extend_nested1",
        top1_before,
        &top1_after,
        EXPECT_SUCCESS,
    );
    fixture.validate_rewrite_external_css(
        "flatten_then_cache_extend_nested2",
        top2_before,
        &top2_after,
        EXPECT_SUCCESS,
    );
}

#[test]
fn flatten_only_inline_css_with_relative_image() {
    // Proves that URLs are absolutified when CSS is flattened but not
    // rewritten.
    test_relative_image_url_in_relative_css_url(false, false);
    test_flatten_nested(false);
}

#[test]
fn flatten_and_trim_inline_css_with_relative_image() {
    // Proves that URLs are fixed when CSS is flattened -and- -trimmed-
    // but not rewritten.
    test_relative_image_url_in_relative_css_url(true, false);
    test_flatten_nested(true);
}

#[test]
fn flatten_from_cache_indirectly() {
    let fixture = Fixture::new();
    // NOTE: cache-activity counts wait for a counting backend.
    let css_in = format!("@import url(http://test.com/{TOP_CSS_FILE});");
    fixture.validate_rewrite_external_css(
        "flatten_from_cache_indirectly",
        &css_in,
        &flattened_top_css_contents(),
        EXPECT_SUCCESS,
    );
    // Access one of the cached ones from a different file (via @import).
    let contents = format!("@import url({ONE_LEVEL_DOWN_FILE1});");
    fixture.mock_css(
        &format!("{TEST_DOMAIN}alternative.css"),
        contents.as_bytes(),
        100,
    );
    let css_in = "@import url(http://test.com/alternative.css);";
    fixture.validate_rewrite_external_css(
        "flatten_from_cache_indirectly_repeat",
        css_in,
        &flattened_one_level_down_contents1(),
        EXPECT_SUCCESS,
    );
}
