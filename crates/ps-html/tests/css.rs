//! CSS-rewrite tests ported from `css_filter_test.cc`.
//!
//! NOTE: logging assertions wait for the log record; CSP cases wait
//! for the CSP subsystem; image-nested cases wait for the image
//! filters.
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
    CssFilter, BLOCKS_REWRITTEN, FALLBACK_FAILURES, FALLBACK_REWRITES, FLATTEN_CHARSET_MISMATCH,
    FLATTEN_COMPLEX_QUERIES, FLATTEN_INVALID_URL, FLATTEN_LIMIT_EXCEEDED, FLATTEN_MINIFY_FAILED,
    FLATTEN_RECURSION, PARSE_FAILURES, PARSE_MAX_BYTES_EXCEEDED, REWRITES_DROPPED,
    TOTAL_BYTES_SAVED, TOTAL_ORIGINAL_BYTES, USES,
};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const INPUT_STYLE: &str =
    ".background_blue { background-color: #f00; }\n.foreground_yellow { color: yellow; }\n";
const OUTPUT_STYLE: &str = ".background_blue{background-color:red}.foreground_yellow{color:#ff0}";

/// Validation flags (`CssRewriteTestBase::ValidationFlags` subset).
const EXPECT_SUCCESS: u32 = 1;
const EXPECT_CACHED: u32 = 2;
const EXPECT_NO_CHANGE: u32 = 4;
const EXPECT_FALLBACK: u32 = 8;
const EXPECT_FAILURE: u32 = 16;
const NO_STAT_CHECK: u32 = 32;
const NO_OTHER_CONTEXTS: u32 = 128;

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    /// Response mime verdict (`None` = unknown, C++ default).
    mime_is_xhtml: RefCell<Option<bool>>,
    /// Expected debug message (`debug_message_`, `%url%` substituted).
    debug_message: RefCell<String>,
}

impl Fixture {
    fn new() -> Self {
        Self::build(None, true)
    }

    /// Builds a fixture with the proxy test namer (`SetUseTestUrlNamer`
    /// + `SetProxyMode`).
    fn with_test_namer(proxy: ps_html::url_namer::ProxyExtent) -> Self {
        let mut namer = ps_html::url_namer::TestUrlNamer::new();
        namer.set_proxy_mode(proxy);
        Self::build(Some(Rc::new(namer)), true)
    }

    /// Builds a fixture with the real MD5 namer (`UseMd5Hasher`).
    fn with_md5() -> Self {
        Self::build(None, false)
    }

    fn build(namer: Option<Rc<dyn ps_html::url_namer::UrlNamer>>, mock: bool) -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend.clone(),
            metadata_backend.clone(),
            fetcher.clone(),
            START_MS,
        );
        if mock {
            server.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        }
        if let Some(namer) = namer {
            server.set_url_namer(namer);
        }
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options.borrow_mut().set_always_rewrite_css(true);
        options.borrow_mut().enable_filter(Filter::RewriteCss);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            http_backend,
            metadata_backend,
            mime_is_xhtml: RefCell::new(None),
            debug_message: RefCell::new(String::new()),
        }
    }

    /// Marks the response HTML (`SetHtmlMimetype`).
    fn set_html_mimetype(&self) {
        *self.mime_is_xhtml.borrow_mut() = Some(false);
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        if let Some(is_xhtml) = *self.mime_is_xhtml.borrow() {
            driver.set_is_xhtml(is_xhtml);
        }
        driver
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

    /// Parses through the CSS filter.
    fn parse(&self, url: &str, html: &str) -> String {
        String::from_utf8(self.parse_bytes(url, html.as_bytes())).unwrap()
    }

    /// Parses through the CSS filter, keeping raw bytes.
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
            PARSE_MAX_BYTES_EXCEEDED,
            REWRITES_DROPPED,
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

    /// Clears both caches.
    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
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
        fresh.set_url_namer(self.server.url_namer().clone());
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

    /// Expected rewritten leaf for CSS output (`ExpectedUrlForCss`).
    fn expected_url_for_css(&self, id: &str, expected_css_output: &[u8]) -> String {
        self.expected_url_for_name(&format!("{id}.css"), expected_css_output)
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
    fn external_html(css_url: &str, debug: &str) -> Vec<u8> {
        format!(
            "<head>\n  <title>Example style outline</title>\n  <!-- Style starts here -->\n  <link rel='stylesheet' type='text/css' href='{css_url}'>{debug}\n  <!-- Style ends here -->\n</head>"
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
            for name in [
                FLATTEN_CHARSET_MISMATCH,
                FLATTEN_INVALID_URL,
                FLATTEN_LIMIT_EXCEEDED,
                FLATTEN_MINIFY_FAILED,
                FLATTEN_RECURSION,
                FLATTEN_COMPLEX_QUERIES,
            ] {
                assert_eq!(self.stat(name), 0, "id: {id}");
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
        css_input: &[u8],
        css_output: &[u8],
        flags: u32,
    ) {
        self.mock_css(css_url, css_input, 300);
        let html_input = Self::external_html(css_url, "");
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
        let html_output = Self::external_html(&expected_new_url, &self.debug_message_for(css_url));
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
        css_input: impl AsRef<[u8]>,
        css_output: impl AsRef<[u8]>,
        flags: u32,
    ) {
        self.validate_rewrite_external_css_url(
            id,
            &format!("{TEST_DOMAIN}{id}.css"),
            css_input.as_ref(),
            css_output.as_ref(),
            flags,
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

    /// Validates a parse failure (`ValidateFailParse`).
    fn validate_fail_parse(&self, id: &str, css_input: impl AsRef<[u8]> + Clone) {
        self.validate_rewrite(id, css_input.clone(), css_input, EXPECT_FAILURE);
    }
}

#[test]
fn simple_rewrite_css_test() {
    let fixture = Fixture::new();
    fixture.validate_rewrite("rewrite_css", INPUT_STYLE, OUTPUT_STYLE, EXPECT_SUCCESS);
}

#[test]
fn simple_rewrite_css_test_external_unhealthy() {
    let fixture = Fixture::new();
    fixture.debug_with_message("");
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    fixture.validate_rewrite_external_css(
        "rewrite_css",
        INPUT_STYLE,
        OUTPUT_STYLE,
        EXPECT_NO_CHANGE,
    );
}

#[test]
fn css_rewrite_random_drop_all() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_rewrite_random_drop_percentage(100);
    fixture.debug_with_message("");
    for _ in 0..100 {
        fixture.validate_rewrite_external_css(
            "rewrite_css",
            INPUT_STYLE,
            OUTPUT_STYLE,
            EXPECT_NO_CHANGE,
        );
        fixture.clear_caches();
    }
}

#[test]
fn css_rewrite_random_drop_none() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_rewrite_random_drop_percentage(0);
    fixture.debug_with_message("");
    for _ in 0..100 {
        fixture.validate_rewrite_external_css(
            "rewrite_css",
            INPUT_STYLE,
            OUTPUT_STYLE,
            EXPECT_SUCCESS,
        );
        fixture.clear_caches();
    }
}

#[test]
fn rewrite_css_404() {
    let fixture = Fixture::new();
    fixture.debug_with_message(
        "<!--4xx status code, preventing rewriting of http://test.com/404.css-->",
    );
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("text/css"),
        "",
        "",
        &mut headers,
    );
    headers.set_first_line(1, 1, 404, "Not Found");
    headers.compute_caching();
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}404.css"), headers, b"");
    let input = "<link rel=stylesheet href='404.css'>";
    let expected = format!("{input}{}", fixture.debug_message_for(""));
    for _ in 0..2 {
        assert_eq!(
            fixture.parse(&format!("{TEST_DOMAIN}404.html"), input),
            expected
        );
    }
}

#[test]
fn not_rewritten_when_origin_type_is_not_css() {
    let fixture = Fixture::new();
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("text/plain"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), 100 * 1000, "");
    headers.headers_mut().remove_all("Last-Modified");
    headers.headers_mut().remove_all("Etag");
    headers.compute_caching();
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}note.txt"),
        headers,
        INPUT_STYLE.as_bytes(),
    );
    let html = "<link rel=stylesheet href='note.txt'>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}origin_type.html"), html),
        html
    );
    use ps_html::url_segment_encoder::{CssUrlEncoder, UrlSegmentEncoder};
    let encoded = CssUrlEncoder.encode(&["note.txt".as_bytes().to_vec()]);
    assert!(fixture
        .serve_url(&format!(
            "{TEST_DOMAIN}{}.pagespeed.cf.0.css",
            String::from_utf8_lossy(&encoded)
        ))
        .is_none());
    assert_eq!(fixture.stat(BLOCKS_REWRITTEN), 0);
}

#[test]
fn css_preserve_urls() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineCss);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteCss);
    fixture.options.borrow_mut().set_css_preserve_urls(true);
    assert!(!fixture.options.borrow().enabled(Filter::InlineCss));
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), INPUT_STYLE.as_bytes(), 100);
    let html = "<link rel=StyleSheet href=a.css>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}css_preserve_urls_on.html"), html),
        html
    );
    let (out_css, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.a.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(out_css, OUTPUT_STYLE.as_bytes());
}

#[test]
fn css_preserve_urls_overriding_extend() {
    let fixture = Fixture::new();
    let mut global = RewriteOptions::new();
    global.enable_filter(Filter::ExtendCacheCss);
    let mut vhost = RewriteOptions::new();
    vhost.enable_filter(Filter::RewriteCss);
    vhost.soft_enable_filter_for_testing(Filter::InlineCss);
    vhost.soft_enable_filter_for_testing(Filter::RewriteCss);
    vhost.set_css_preserve_urls(true);
    fixture.options.borrow_mut().merge(&global);
    fixture.options.borrow_mut().merge(&vhost);
    assert!(!fixture.options.borrow().enabled(Filter::InlineCss));
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), INPUT_STYLE.as_bytes(), 100);
    let html = "<link rel=StyleSheet href=a.css>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}css_preserve_urls_on.html"), html),
        html
    );
    let (out_css, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.a.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(out_css, OUTPUT_STYLE.as_bytes());
}

#[test]
fn css_preserve_urls_with_merged_cache_extend() {
    let fixture = Fixture::new();
    let mut global = RewriteOptions::new();
    global.enable_filter(Filter::RewriteCss);
    global.set_css_preserve_urls(true);
    let mut vhost = RewriteOptions::new();
    vhost.enable_filter(Filter::ExtendCacheCss);
    fixture.options.borrow_mut().merge(&global);
    fixture.options.borrow_mut().merge(&vhost);
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), INPUT_STYLE.as_bytes(), 100);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}css_preserve_urls_on.html"),
            "<link rel=StyleSheet href=a.css>"
        ),
        format!(
            "<link rel=StyleSheet href={}>",
            fixture.expected_url_for_css("a", OUTPUT_STYLE.as_bytes())
        )
    );
    let (out_css, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.a.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(out_css, OUTPUT_STYLE.as_bytes());
}

#[test]
fn css_preserve_urls_no_preemptive_rewrite() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineCss);
    fixture.options.borrow_mut().set_css_preserve_urls(true);
    fixture
        .options
        .borrow_mut()
        .set_in_place_preemptive_rewrite_css(false);
    assert!(!fixture.options.borrow().enabled(Filter::InlineCss));
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), INPUT_STYLE.as_bytes(), 100);
    let html = "<link rel=StyleSheet href=a.css>";
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}css_preserve_urls_on_no_preemptive.html"),
            html
        ),
        html
    );
    // We should not have attempted any rewriting.
    assert_eq!(fixture.stat(BLOCKS_REWRITTEN), 0);
    assert_eq!(fixture.stat(USES), 0);
    // But, if we fetch the CSS directly, we should receive the
    // optimized version.
    let (out_css, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.a.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(out_css, OUTPUT_STYLE.as_bytes());
}

/// Builds a stylesheet of exactly `size` bytes plus its minified
/// form (`MakeCssOfSize`).
fn make_css_of_size(size: i64) -> (String, String) {
    let mut css = String::new();
    let mut minified = String::new();
    let mut i = 0;
    loop {
        let name = format!(".r{i}");
        let rule = format!("{name} {{ color: red }}\n");
        if css.len() as i64 + rule.len() as i64 > size {
            break;
        }
        css.push_str(&rule);
        minified.push_str(&format!("{name}{{color:red}}"));
        i += 1;
    }
    css.extend(std::iter::repeat_n(' ', (size - css.len() as i64) as usize));
    assert_eq!(size, css.len() as i64);
    (css, minified)
}

#[test]
fn default_is_two_mebibytes() {
    assert_eq!(2 * 1024 * 1024, RewriteOptions::new().css_parse_max_bytes());
}

#[test]
fn external_over_bound_is_left_alone() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    let (css, _) = make_css_of_size(4097);
    fixture.validate_rewrite_external_css("over_bound", &css, &css, EXPECT_NO_CHANGE);
    assert_eq!(fixture.stat(PARSE_MAX_BYTES_EXCEEDED), 1);
}

#[test]
fn external_at_bound_is_rewritten() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    let (css, minified) = make_css_of_size(4096);
    fixture.validate_rewrite_external_css("at_bound", &css, &minified, EXPECT_SUCCESS);
    assert_eq!(fixture.stat(PARSE_MAX_BYTES_EXCEEDED), 0);
}

#[test]
fn inline_over_bound_is_left_alone() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    let (css, _) = make_css_of_size(4097);
    fixture.validate_rewrite_inline_css("over_bound", &css, &css, EXPECT_NO_CHANGE);
    assert_eq!(fixture.stat(PARSE_MAX_BYTES_EXCEEDED), 1);
}

#[test]
fn inline_at_bound_is_rewritten() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    let (css, minified) = make_css_of_size(4096);
    fixture.validate_rewrite_inline_css("at_bound", &css, &minified, EXPECT_SUCCESS);
    assert_eq!(fixture.stat(PARSE_MAX_BYTES_EXCEEDED), 0);
}

#[test]
fn over_bound_skips_fallback_rewriting() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::FallbackRewriteCssUrls);
    let (mut css, _) = make_css_of_size(4092);
    css.push_str(" div{");
    assert_eq!(4097, css.len() as i64);
    fixture.validate_rewrite_external_css("over_bound_fallback", &css, &css, EXPECT_NO_CHANGE);
    assert_eq!(fixture.stat(PARSE_MAX_BYTES_EXCEEDED), 1);
}

#[test]
fn default_bound_leaves_large_stylesheet_alone() {
    let fixture = Fixture::new();
    let (css, _) = make_css_of_size(2 * 1024 * 1024 + 1);
    fixture.validate_rewrite_external_css("over_default", &css, &css, EXPECT_NO_CHANGE);
    assert_eq!(fixture.stat(PARSE_MAX_BYTES_EXCEEDED), 1);
}

#[test]
fn link_href_case_insensitive() {
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), INPUT_STYLE.as_bytes(), 100);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}case_insensitive.html"),
            "<link rel=StyleSheet href=a.css>"
        ),
        format!(
            "<link rel=StyleSheet href={}>",
            fixture.expected_url_for_css("a", OUTPUT_STYLE.as_bytes())
        )
    );
}

#[test]
fn url_too_long() {
    let fixture = Fixture::new();
    fixture.debug_with_message("<!--Rewritten URL segment too long.-->");
    let filename = "z".repeat((fixture.options.borrow().max_url_segment_size() - 4) as usize);
    fixture.validate_rewrite_external_css(&filename, INPUT_STYLE, INPUT_STYLE, EXPECT_NO_CHANGE);
}

#[test]
fn rewrite_empty_css_test() {
    let fixture = Fixture::new();
    // NOTE: empty inline styles record nothing (no CharactersNode).
    fixture.validate_rewrite_inline_css(
        "rewrite_empty_css-inline",
        "",
        "",
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
    assert_eq!(fixture.stat(BLOCKS_REWRITTEN), 0);
    assert_eq!(fixture.up_stat(TOTAL_BYTES_SAVED), 0);
    assert_eq!(fixture.stat(PARSE_FAILURES), 0);
    fixture.validate_rewrite_external_css(
        "rewrite_empty_css-external",
        "",
        "",
        EXPECT_FAILURE | NO_STAT_CHECK,
    );
    assert_eq!(fixture.up_stat(TOTAL_BYTES_SAVED), 0);
    assert_eq!(fixture.stat(PARSE_FAILURES), 0);
}

#[test]
fn empty_output() {
    let fixture = Fixture::new();
    fixture.validate_rewrite(
        "empty_output",
        "/* body { background: blue; } */\n",
        "",
        EXPECT_SUCCESS,
    );
}

#[test]
fn transition_time_units() {
    let fixture = Fixture::new();
    fixture.validate_rewrite(
        "transition_time",
        "foo {  -moz-transition-delay: 0s, 0s; }",
        "foo{-moz-transition-delay:0s , 0s}",
        EXPECT_SUCCESS,
    );
}

#[test]
fn rewrite_repeated() {
    let fixture = Fixture::new();
    // NOTE: log-record assertions wait for the log record.
    fixture.validate_rewrite_external_css("rep", " div { } ", "div{}", EXPECT_SUCCESS);
    assert_eq!(fixture.stat(BLOCKS_REWRITTEN), 1);
    assert_eq!(fixture.stat(USES), 1);
    fixture.reset_stats();
    fixture.validate_rewrite_external_css(
        "rep",
        " div { } ",
        "div{}",
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
    // NOTE: LRU insert/reinsert counts wait for a counting test
    // backend; reuse still shows as no new blocks plus one use.
    assert_eq!(fixture.stat(BLOCKS_REWRITTEN), 0);
    assert_eq!(fixture.stat(USES), 1);
}

#[test]
fn rewrite_repeated_parse_error() {
    let fixture = Fixture::new();
    fixture.debug_with_message("<!--CSS rewrite failed: Parse error in %url%-->");
    fixture.validate_rewrite_external_css("rep_fail", "@media }}", "", EXPECT_FAILURE);
    assert_eq!(fixture.stat(PARSE_FAILURES), 1);
    fixture.validate_rewrite_external_css(
        "rep_fail",
        "@media }}",
        "",
        EXPECT_FAILURE | NO_STAT_CHECK,
    );
    // Second time, we remember failure and so don't try to reparse.
    assert_eq!(fixture.stat(PARSE_FAILURES), 0);
}

#[test]
fn non_utf8() {
    let fixture = Fixture::new();
    // gb2312 (Not valid UTF-8, multi-byte).
    fixture.validate_rewrite(
        "font",
        b"a { font-family: \"\xcb\xce\xcc\xe5\"; }".as_slice(),
        b"a{font-family: \"\xcb\xce\xcc\xe5\"}".as_slice(),
        EXPECT_SUCCESS,
    );
    // Windows-1252 (Not valid UTF-8, single-byte).
    fixture.validate_rewrite(
        "string",
        b".foo { content: \"r\xe9sum\xe9\"; }".as_slice(),
        b".foo{content: \"r\xe9sum\xe9\"}".as_slice(),
        EXPECT_SUCCESS,
    );
    // Shift_JIS (Not valid UTF-8, multi-byte, second byte may not set
    // high bit).
    fixture.validate_rewrite(
        "ident_value",
        b".foo { -moz-charset: \x83\x56\x83\x74\x83\x67\x83\x57; }".as_slice(),
        b".foo{-moz-charset: \x83\x56\x83\x74\x83\x67\x83\x57}".as_slice(),
        EXPECT_SUCCESS,
    );
    // KOI8-R (Not valid UTF-8, single-byte).
    fixture.validate_rewrite(
        "ident_param",
        b".foo { \xeb\xef\xe9-8: standard; }".as_slice(),
        b".foo{\xeb\xef\xe9-8: standard}".as_slice(),
        EXPECT_SUCCESS,
    );
    // EUC-KR (Not valid UTF-8, multi-byte).
    fixture.validate_rewrite(
        "ident_selector",
        b".\xb8\xc0 { color: red; }".as_slice(),
        b".\xb8\xc0 {color:red}".as_slice(),
        EXPECT_SUCCESS,
    );
    // Verbatim example from http://www.baidu.com/
    fixture.validate_rewrite(
        "baidu",
        b"#lk span {font:14px \"\xcb\xce\xcc\xe5\"}".as_slice(),
        b"#lk span{font:14px \"\xcb\xce\xcc\xe5\"}".as_slice(),
        EXPECT_SUCCESS,
    );
}

#[test]
fn non_8bit_encoding() {
    let fixture = Fixture::new();
    fixture.debug_with_message("");
    // 0x83 0x7D == KATAKANA LETTER MA; 0x7D == '}'.
    fixture.validate_rewrite(
        "string-ma",
        b".foo { font-family: \"\x83\x7d\"; color: red }".as_slice(),
        b".foo{font-family: \"\x83\x7d\";color:red}".as_slice(),
        EXPECT_SUCCESS,
    );
    // 0x83 0x7B == KATAKANA LETTER BO; 0x7B == '{'.
    fixture.validate_rewrite(
        "string-bo",
        b".foo { font-family: \"\x83\x7b\"; color: red }".as_slice(),
        b".foo{font-family: \"\x83\x7b\";color:red}".as_slice(),
        EXPECT_SUCCESS,
    );
    // Note: This text currently fails to be parsed. But if that
    // changes, update this test to the correct golden rewrite.
    fixture.debug_with_message("<!--CSS rewrite failed: Parse error in %url%-->");
    fixture.validate_fail_parse(
        "ident-ma",
        b".foo { -win-magic: bar\x83\x7d; color: red }".as_slice(),
    );
    fixture.validate_fail_parse(
        "ident-bo",
        b".foo { -win-magic: bar\x83\x7b; color: red }".as_slice(),
    );
}

/// Corrupt-URL matrix (`TestCorruptUrl`).
fn test_corrupt_url(new_suffix: &str) {
    let fixture = Fixture::new();
    fixture.debug_with_message("");
    fixture.validate_rewrite_external_css("rep", " div { } ", "div{}", EXPECT_SUCCESS);
    let css_url = fixture.expected_url_for_css("rep", b"div{}");
    assert!(css_url.ends_with(".css"));
    let munged = format!(
        "{TEST_DOMAIN}{}{new_suffix}",
        css_url.strip_suffix(".css").unwrap()
    );
    let (output, _) = fixture.serve_url(&munged).expect("served");
    assert_eq!(output, b"div{}");
    fixture.validate_rewrite_external_css(
        "rep",
        " div { } ",
        "div{}",
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
}

#[test]
fn no_extension_corruption() {
    test_corrupt_url(".css%22");
}

#[test]
fn no_query_corruption() {
    test_corrupt_url(".css?query");
}

#[test]
fn no_wrong_ext_corruption() {
    test_corrupt_url(".html");
}
/// Good minifier vectors (`RewriteVariousCss`; output equals
/// input). Extracted mechanically from `css_filter_test.cc`; any
/// divergence from the C++ baseline is a port bug.
const VARIOUS_GOOD: &[&[u8]] = &[
    b"a.b #c.d e#d,f:g>h+i>j{color:red}",
    b"a{border:solid 1px #ccc}",
    b"a{border:none!important}",
    b"a{background-image:url(foo.png)}",
    b"a{background-position:-19px 60%}",
    b"a{margin:0}",
    b"a{padding:.01em -.25em}",
    b"a{-moz-border-radius-topleft:0}",
    b".ds{display:-moz-inline-box}",
    b"a{background:none}",
    b"a{color:inherit}",
    b"@import url(http://www.example.com);",
    b"@media a,b{a{color:red}}",
    b"@charset \"foobar\";",
    b"@charset utf-8;",
    b"@font-face{font-family:'Ubuntu';font-style:normal}",
    b"a{content:\"Odd chars: \\(\\)\\,\\\"\\'\"}",
    b"a{content:\"Unicode: \\A0\\A0\"}",
    b"img{clip:rect(0,60px,200px,0)}",
    b"p.normal::selection{background:#c00;color:#fff}",
    b"::-moz-focus-inner{border:0}",
    b"input::-webkit-input-placeholder{color:#ababab}a{box-shadow:-1px -2px 2px rgba(0,0,0,.15)}",
    b"a{-moz-transform:rotate(7deg)}",
    b"a{filter:progid:DXImageTransform.Microsoft.Alpha(Opacity=80)}",
    b"body{font-family:font\\,1,font\\,2}",
    b"#MyForm\\.myfield{property:value}",
    b"\\*{color:red}",
    b"a{property\\:more:value\\;more}a{width:overflow:hidden}",
    b"div{margin:100px\\9 }",
    b"div{margin\\9 :100px}",
    b"div\\9 {margin:100px}",
    b"a{color:red\\9 }",
    b"a{background:none\\9 }",
    b"a{color:WindowText}",
    b"a{color:FooBar}",
    b".border8{border-radius: 36px / 12px }",
    b"a{-webkit-transition-property:opacity , -webkit-transform}",
    b"div:nth-child(1n) {color:red}",
    b"a{color: red\\0/ ;background-color:green}",
    b"a{font-family: font\\0  ;color:red}",
    b"@media screen and (min-width:0 \\\\0){.foo{color:red}}",
    b"a{font:bold verdana 10px }",
    b"a{foo: +bar }",
    b"a{color: rgb(foo,+,) }",
    b"@import styles.css;a{color:red}",
    b"@import \"styles.css\", \"other.css\";a{color:red}",
    b"@import url(styles.css), url(other.css);a{color:red}",
    b"@import \"styles.css\"...;a{color:red}",
    b"@media screen and (max-width:290px){a{color:red}}",
    b"@media only print and (color){a{color:red}}",
    b"@media only screen and(min-resolution:240dpi){ .bar{ background: red; }}",
    b"@media (max-de vice-width: 850px) { .pm-thumb-106 { width: 80px; } }",
    b"@media screen\\0{ .select:before { width: 18px; } }",
    b"@media screen and (min-width:0 \\0) { .foo { color: red; } }",
    b"@media , { a { color: red; } }",
    b"@keyframes wiggle { 0% { transform: rotate(6deg); } }",
    b"a{font: menu foobar }",
    b"a{padding-top: 1.em }",
    b"a{color: red !ie }",
    b"a{color: !important red }",
    b"a{color: red !important blue }",
    b"a{webkit-transition-property:color , background-color}",
    b"a{//display: inline-block }",
    b".ad_300x250{/margin-top:-120px }",
    b"a{background-repeat;no-repeat }",
    b"a{margin-right:0;width:113px;*/ }",
    b"a{z-i ndex:19 }",
    b"a{width:352px;height62px ;display:block}",
    b"a{color: #5552 }",
    b"a{1font-family:Tahoma , Arial , sans-serif}",
    b"a{text align:center }",
    b".hp .col ul, {display:inline}",
    b"body:not(:target) {color:red}",
    b"a:not(.button):hover {color:red}",
    b"# new_results_notification{font-size:12px}",
    b".bold: {font-weight:bold}",
    b"a{_top: expression(0+((e=document.documen))) }",
    b"a{width: expression(this.width > 120 ? 120:tr) }",
    b"a{_height: expression((parentNode.offsetHeight-17))}",
    b"a{progid:DXImageTransform.Microsoft.AlphaImageLoader(src=/images/lb/internet_e) }",
    b"a{progid:DXImageTransform.Microsoft.AlphaImageLoader(src=\"/images/lb/internet_e\") }",
    b"a{progid:DXImageTransform.Microsoft.AlphaImageLoader(src='/images/lb/internet_e') }",
    b"a[}]{color:red}",
    b"a{font:12px,clean}",
    b"@-moz-document url-prefix() { input[type=\"file\"] {border:none}}",
    b".foo{background-image: (/i.png);width:10px}",
    b".bigAd{background:14url(http://himg2.huanqiu.com/bigAd.jpg) center}",
    b".foo{background: url\n\n(../images/btn_login_min.gif) }",
    b"@-webkit-keyframes \"foo\" { 0 { transform: scale (.85) } }",
];

/// Failing minifier vectors (`RewriteVariousCss`).
const VARIOUS_FAIL: &[&[u8]] = &[
    b"@import url(styles.css)",
    b"@import url(styles.css) screen",
    b"@charset 'utf-8'",
    b"@media print",
    b"@media print ",
    b"@media print {",
    b"@media print { .a { color: red",
    b"@media print { .a { color: red; }",
    b"@font-face",
    b"@font-face ",
    b"@font-face {",
    b"@media screen { @font-face",
    b"@media screen { @font-face {",
    b"@media screen { @font-face { src: url(foo.woff)",
    b"@media screen { @font-face {}",
    b"@media screen { @font-face { src: url(foo.woff); }",
    b"@foobar",
    b"@foobar ",
    b"@foobar {",
    b"@foobar }",
    b"{",
    b"}",
    b"}{",
    b"a { color: red; }}}",
    b"a[{] { color: red; }",
    b"a {",
    b"a }",
    b"a { *color{: red; }",
    b"a { *color}: red; }",
    b"a { filter: progid:DXImageTransform.Microsoft.AlphaImageLoader(src=/images/{lb/internet_e); }",
    b"a { color: red",
    b"a { color: red;\n .foo { margin: 10px; }",
    b"a { color: red;\n h1 { margin: 10px; }",
    b"/*",
    b"a { color: red; }/*",
    b"a { color/*: red; }",
    b"a { color: /*red; }",
    b"\xff\xfe/\x00*\x00*\x00/\x00",
    b"</foobar>",
    b"-ms-filter:\"progid:DXImageTransform.Microsoft.Alpha\"(Opacity=70)",
    b"a",
    b"a { color: red }\n */",
    b"a { color: red }\n // Comment",
    b"a { color: red } .foo",
    b"<!--\n\n // -->",
    b"<!-- \xe8\xae\xa9\xe5\x9b\x9b\xe5\xbc\xa0\xe5\x9b\xbe\xe7\x89\x87\xe9\x83\xbd\xe5\x8f\xaf\xe4\xbb\xa5\xe9\x87\x8d\xe5\x8f\xa0\xe5\x9c\xa8\xe4\xb8\x80\xe8\xb5\xb7 -->",
    b"---------------------------",
];

/// Shrinking minifier vectors (`ToOptimize`).
const TO_OPTIMIZE: &[(&[u8], &[u8])] = &[
    (b".gb1, .gb3 {}", b".gb1,.gb3{}"),
    (
        b"@media screen and (color) { .foo {} .bar {} }",
        b"@media screen and (color){.foo{}.bar{}}",
    ),
    (b".lst:focus { outline:none; }", b".lst:focus{outline:none}"),
];

/// Real-world minifier vectors (`ComplexCssTest`).
const COMPLEX: &[(&[u8], &[u8])] = &[
    (b"#userlogin, #userloginForm {\n  border: solid 1px #cccccc;\n  padding: 1.2em;\n  float: left;\n}\n", b"#userlogin,#userloginForm{border:solid 1px #ccc;padding:1.2em;float:left}"),
    (b"h3 .editsection { font-size: 76%; font-weight: normal; }\n", b"h3 .editsection{font-size:76%;font-weight:normal}"),
    (b"div.magnify a, div.magnify img {\n  display: block;\n  border: none !important;\n  background: none !important;\n}\n", b"div.magnify a,div.magnify img{display:block;border:none!important;background:none!important}"),
    (b"#ca-watch.icon a:hover {\n  background-image: url('images/watch-icons.png?1');\n  background-position: -19px 60%;\n}\n", b"#ca-watch.icon a:hover{background-image:url(images/watch-icons.png?1);background-position:-19px 60%}"),
    (b"body {\n  background: White;\n  /*font-size: 11pt !important;*/\n  color: Black;\n  margin: 0;\n  padding: 0;\n}\n", b"body{background:#fff;color:#000;margin:0;padding:0}"),
    (b".suggestions-result{\n  color:black;\n  color:WindowText;\n  padding:0.01em 0.25em;\n}\n", b".suggestions-result{color:#000;color:WindowText;padding:.01em .25em}"),
    (b".ui-corner-tl { -moz-border-radius-topleft: 0; -webkit-border-top-left-radius: 0; }\n", b".ui-corner-tl{-moz-border-radius-topleft:0;-webkit-border-top-left-radius:0}"),
    (b".ui-tabs .ui-tabs-nav li.ui-tabs-selected a, .ui-tabs .ui-tabs-nav li.ui-state-disabled a, .ui-tabs .ui-tabs-nav li.ui-state-processing a { cursor: pointer; }\n", b".ui-tabs .ui-tabs-nav li.ui-tabs-selected a,.ui-tabs .ui-tabs-nav li.ui-state-disabled a,.ui-tabs .ui-tabs-nav li.ui-state-processing a{cursor:pointer}"),
    (b".ui-datepicker-cover {\n  display: none; /*sorry for IE5*/\n  display/**/: block; /*sorry for IE5*/\n  position: absolute; /*must have*/\n  z-index: -1; /*must have*/\n  filter: mask(); /*must have*/\n  top: -4px; /*must have*/\n  left: -4px; /*must have*/\n  width: 200px; /*must have*/\n  height: 200px; /*must have*/\n}\n", b".ui-datepicker-cover{display:none;display:block;position:absolute;z-index:-1;filter:mask();top:-4px;left:-4px;width:200px;height:200px}"),
    (b".shift {\n  -moz-transform: rotate(7deg);\n  -webkit-transform: rotate(7deg);\n  -moz-transform: skew(-25deg);\n  -webkit-transform: skew(-25deg);\n  -moz-transform: scale(0.5);\n  -webkit-transform: scale(0.5);\n  -moz-transform: translate(3em, 0);\n  -webkit-transform: translate(3em, 0);\n}\n", b".shift{-moz-transform:rotate(7deg);-webkit-transform:rotate(7deg);-moz-transform:skew(-25deg);-webkit-transform:skew(-25deg);-moz-transform:scale(.5);-webkit-transform:scale(.5);-moz-transform:translate(3em,0);-webkit-transform:translate(3em,0)}"),
    (b"a { font-family: trebuchet ms; }", b"a{font-family:trebuchet\\ ms}"),
    (b"body { font: 2em sans-serif; }", b"body{font:2em sans-serif}"),
    (b"body { font: 0.75em sans-serif; }", b"body{font:.75em sans-serif}"),
    (b"#breadcrumbs ul { list-style-type: none; }", b"#breadcrumbs ul{list-style-type:none}"),
    (b".menu { font: menu; }               ", b".menu{font:menu}"),
    (b"#some_id {\nbackground: #cccccc url(images/picture.png) 50% 50% repeat-x;\n}\n", b"#some_id{background:#ccc url(images/picture.png) 50% 50% repeat-x}"),
    (b".gac_od { border-color: -moz-use-text-color #E7E7E7 #E7E7E7 -moz-use-text-color; }", b".gac_od{border-color:-moz-use-text-color #e7e7e7 #e7e7e7 -moz-use-text-color}"),
    (b"a { *padding-bottom: 0px; }", b"a{*padding-bottom: 0px}"),
    (b"#element { width: 1px; _width: 3px; }", b"#element{width:1px;_width:3px}"),
    (b"body {\n  background-image:-webkit-gradient(linear, 50% 0%, 50% 100%, from(rgb(232, 237, 240)), to(rgb(252, 252, 253)));\n  color: red;\n}\n.foo { color: rgba(1, 2, 3, 0.4); }\n", b"body{background-image:-webkit-gradient(linear,50% 0%,50% 100%,from(#e8edf0),to(#fcfcfd));color:red}.foo{color:rgba(1,2,3,.4)}"),
    (b"body {counter-reset:section;}\nh1 {counter-reset:subsection;}\nh1:before\n{\ncounter-increment:section;\ncontent:\"Section \" counter(section) \". \";\n}\nh2:before \n{\ncounter-increment:subsection;\ncontent:counter(section) \".\" counter(subsection) \" \";\n}\n", b"body{counter-reset:section}h1{counter-reset:subsection}h1:before{counter-increment:section;content:\"Section \" counter(section) \". \"}h2:before{counter-increment:subsection;content:counter(section) \".\" counter(subsection) \" \"}"),
    (b"a { font-family: Arial; }", b"a{font-family:Arial}"),
    (b"#foo { z-index: 2147483649; }", b"#foo{z-index:2147483649}"),
    (b"#foo { z-index: 123456789012345678901234567890; }", b"#foo{z-index:123456789012345678901234567890}"),
    (b".ad-contain .ad-jump {\n  color: #000;\n  font: bold 1.54545455em/0.823529412 \"Benton Sans Bold\", Arial, Helvetica, sans-serif;   /* 17px / 11px; 14px / 17px */\n  margin-bottom: 1em;\n}", b".ad-contain .ad-jump{color:#000;font:bold 1.54545455em/.823529412 \"Benton Sans Bold\",Arial,Helvetica,sans-serif;margin-bottom:1em}"),
    (b"a { content: \"Special chars: \\n\\r\\t\\A \\D \\9\" }", b"a{content:\"Special chars: \\n\\r\\t\\A \\D \\9\"}"),
    (b"@media screen {  body { counter-reset:section }  h1 { counter-reset:subsection }}@media screen,printer { a { color:red } }@media screen,printer { b { color:green } }@media screen,printer { c { color:blue } }@media screen         { d { color:black } }@media screen,printer { e { color:white } }", b"@media screen{body{counter-reset:section}h1{counter-reset:subsection}}@media screen,printer{a{color:red}b{color:green}c{color:#00f}}@media screen{d{color:#000}}@media screen,printer{e{color:#fff}}"),
    (b"@charset \"UTF-8\";\na { color: red }\n", b"@charset \"UTF-8\";a{color:red}"),
    (b".mui-navbar-wrap, .mui-navbar-clone {opacity:1;-webkit-transform:translateX(0);-webkit-transition-property:opacity,-webkit-transform;-webkit-transition-duration:400ms;}", b".mui-navbar-wrap,.mui-navbar-clone{opacity:1;-webkit-transform:translateX(0);-webkit-transition-property:opacity , -webkit-transform;-webkit-transition-duration:400ms}"),
    (b".gbxms{background-color:#ccc;display:block;position:absolute;z-index:1;top:-1px;left:-2px;right:-2px;bottom:-2px;opacity:.4;-moz-border-radius:3px;filter:progid:DXImageTransform.Microsoft.Blur(pixelradius=5);*opacity:1;*top:-2px;*left:-5px;*right:5px;*bottom:4px;-ms-filter:\"progid:DXImageTransform.Microsoft.Blur(pixelradius=5)\";opacity:1\\0/;top:-4px\\0/;left:-6px\\0/;right:5px\\0/;bottom:4px\\0/}", b".gbxms{background-color:#ccc;display:block;position:absolute;z-index:1;top:-1px;left:-2px;right:-2px;bottom:-2px;opacity:.4;-moz-border-radius:3px;filter:progid:DXImageTransform.Microsoft.Blur(pixelradius=5);*opacity:1;*top:-2px;*left:-5px;*right:5px;*bottom:4px;-ms-filter:\"progid:DXImageTransform.Microsoft.Blur(pixelradius=5)\";opacity:1\\0/;top:-4px\\0/;left:-6px\\0/;right:5px\\0/;bottom:4px\\0/}"),
    (b".cnn_html_slideshow_controls > .cnn_html_slideshow_pager_container > .cnn_html_slideshow_pager > li\n{\n  font-size: 16px;\n  -webkit-transition-property: color, background-color;\n  -webkit-transition-duration: 0.5s;\n}\n", b".cnn_html_slideshow_controls>.cnn_html_slideshow_pager_container>.cnn_html_slideshow_pager>li{font-size:16px;-webkit-transition-property:color , background-color;-webkit-transition-duration:.5s}"),
    (b"a.login,a.home{position:absolute;right:15px;top:15px;display:block;float:right;height:29px;line-height:27px;font-size:15px;font-weight:bold;color:rgba(255,255,255,0.7)!important;color:#fff;text-shadow:0 -1px 0 rgba(0,0,0,0.2);background:#607890;padding:0 12px;opacity:.9;text-decoration:none;border:1px solid #2e4459;-moz-border-radius:6px;-webkit-border-radius:6px;border-radius:6px;-moz-box-shadow:0 1px 0 rgba(255,255,255,0.15),0 1px 0 rgba(255,255,255,0.15) inset;-webkit-box-shadow:0 1px 0 rgba(255,255,255,0.15),0 1px 0 rgba(255,255,255,0.15) inset;box-shadow:0 1px 0 rgba(255,255,255,0.15),0 1px 0 rgba(255,255,255,0.15) inset}", b"a.login,a.home{position:absolute;right:15px;top:15px;display:block;float:right;height:29px;line-height:27px;font-size:15px;font-weight:bold;color:rgba(255,255,255,.7)!important;color:#fff;text-shadow:0 -1px 0 rgba(0,0,0,.2);background:#607890;padding:0 12px;opacity:.9;text-decoration:none;border:1px solid #2e4459;-moz-border-radius:6px;-webkit-border-radius:6px;border-radius:6px;-moz-box-shadow:0 1px 0 rgba(255,255,255,.15) , 0 1px 0 rgba(255,255,255,.15) inset;-webkit-box-shadow:0 1px 0 rgba(255,255,255,.15) , 0 1px 0 rgba(255,255,255,.15) inset;box-shadow:0 1px 0 rgba(255,255,255,.15) , 0 1px 0 rgba(255,255,255,.15) inset}"),
    (b".authorization .mail .login input, .authorization .pswd input {float: left; width: 100%; font-size: 75%; -moz-box-sizing: border-box; -webkit-box-sizing: border-box; box-sizing: border-box; height: 21px; padding: 2px; #height: 13px}\n.authorization .mail .domain select {float: right; width: 97%; #width: 88%; font-size: 75%; height: 21px; -moz-box-sizing: border-box; -webkit-box-sizing: border-box; box-sizing: border-box}\n.weather_review .main img.attention {position: absolute; z-index: 5; left: -10px; top: 6px; width: 29px; height: 26px; \nbackground: url('http://limg3.imgsmail.ru/r/weather_new/ico_attention.png'); \n//background-image: none; \nfilter: progid:DXImageTransform.Microsoft.AlphaImageLoader(src=\"http://limg3.imgsmail.ru/r/weather_new/ico_attention.png\", sizingMethod=\"crop\"); \n} \n.rb_body {font-size: 12px; padding: 0 0 0 10px; overflow: hidden; text-align: left; //display: inline-block;}\n.rb_h4 {border-bottom: 1px solid #0857A6; color: #0857A6; font-size: 17px; font-weight: bold; text-decoration: none;}\n", b".authorization .mail .login input,.authorization .pswd input{float:left;width:100%;font-size:75%;-moz-box-sizing:border-box;-webkit-box-sizing:border-box;box-sizing:border-box;height:21px;padding:2px;#height: 13px}.authorization .mail .domain select{float:right;width:97%;#width: 88%;font-size:75%;height:21px;-moz-box-sizing:border-box;-webkit-box-sizing:border-box;box-sizing:border-box}.weather_review .main img.attention{position:absolute;z-index:5;left:-10px;top:6px;width:29px;height:26px;background:url(http://limg3.imgsmail.ru/r/weather_new/ico_attention.png);//background-image: none;filter: progid:DXImageTransform.Microsoft.AlphaImageLoader(src=\"http://limg3.imgsmail.ru/r/weather_new/ico_attention.png\", sizingMethod=\"crop\")}.rb_body{font-size:12px;padding:0 0 0 10px;overflow:hidden;text-align:left;//display: inline-block}.rb_h4{border-bottom:1px solid #0857a6;color:#0857a6;font-size:17px;font-weight:bold;text-decoration:none}"),
    (b".file_manager .loading { _position: absolute;_top: expression(0+((e=document.documentElement.scrollTop)?e:document.body.scrollTop)+'px'); color: red; }\n.connect_widget .page_stream img{max-width:120px;width:expression(this.width > 120 ? 120:true); color: red; }\n", b".file_manager .loading{_position:absolute;_top: expression(0+((e=document.documentElement.scrollTop)?e:document.body.scrollTop)+'px');color:red}.connect_widget .page_stream img{max-width:120px;width:expression(this.width > 120 ? 120:true);color:red}"),
    (b"#fancybox-loading.fancybox-ie6 {\n        position: absolute; margin-top: 0;\n        top: expression( (-20 + (document.documentElement.clientHeight ? document.documentElement.clientHeight/2 : document.body.clientHeight/2 ) + ( ignoreMe = document.documentElement.scrollTop ? document.documentElement.scrollTop : document.body.scrollTop )) + 'px');\n}\n", b"#fancybox-loading.fancybox-ie6{position:absolute;margin-top:0;top: expression( (-20 + (document.documentElement.clientHeight ? document.documentElement.clientHeight/2 : document.body.clientHeight/2 ) + ( ignoreMe = document.documentElement.scrollTop ? document.documentElement.scrollTop : document.body.scrollTop )) + 'px')}"),
    (b".imdb_lb .header{width:726px;width=728px;height:12px;padding:1px;border-bottom:1px #000000 solid;background:#eeeeee;font-size:10px;text-align:left;}.cboxIE #cboxTopLeft{background:transparent;filter:progid:DXImageTransform.Microsoft.AlphaImageLoader(src=/images/lb/internet_explorer/borderTopLeft.png, sizingMethod='scale');}", b".imdb_lb .header{width:726px;width=728px;height:12px;padding:1px;border-bottom:1px #000 solid;background:#eee;font-size:10px;text-align:left}.cboxIE #cboxTopLeft{background:transparent;filter:progid:DXImageTransform.Microsoft.AlphaImageLoader(src=/images/lb/internet_explorer/borderTopLeft.png, sizingMethod='scale')}"),
    (b".login-form .input-text{ width:144px;padding:6px 3px; background-color:#fff;background-position:0 -170px;background-repeat;no-repeat}td.pop_content .dialog_body{padding:10px;border-bottom:1px# solid #ccc}", b".login-form .input-text{width:144px;padding:6px 3px;background-color:#fff;background-position:0 -170px;background-repeat;no-repeat}td.pop_content .dialog_body{padding:10px;border-bottom:1px# solid #ccc}"),
    (b".hp .col ul, {\n  display: inline !important;\n  zoom: 1;\n  vertical-align: top;\n  margin-left: -10px;\n  position: relative;\n}\n", b".hp .col ul, {display:inline!important;zoom:1;vertical-align:top;margin-left:-10px;position:relative}"),
    (b".ciuNoteEditBox .topLeft\n{\n        background-position:left top;\n\tbackground-repeat:no-repeat;\n\tfont-size:4px;\n\t\n\t\n\tpadding: 0px 0px 0px 1px; \n\t\n\twidth:7px;\n}\n\n// css hack to make font-size 0px in only ff2.0 and older (http://pornel.net/firefoxhack)\n.ciuNoteBox .topLeft,\n.ciuNoteEditBox .topLeft, x:-moz-any-link {\n\tfont-size: 0px;\n}\n", b".ciuNoteEditBox .topLeft{background-position:left top;background-repeat:no-repeat;font-size:4px;padding:0 0 0 1px;width:7px}// css hack to make font-size 0px in only ff2.0 and older (http://pornel.net/firefoxhack)\n.ciuNoteBox .topLeft,\n.ciuNoteEditBox .topLeft, x:-moz-any-link {font-size:0}"),
    (b"/* Opera\xef\xbc\x88\xef\xbc\x8bFirefox\xe3\x80\x81Safari\xef\xbc\x89 */\nbody:not(:target) .sh_heading_main_b, body:not(:target) .sh_heading_main_b_wide{\n  background:url(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAoCAYAAAA/tpB3AAAAQ0lEQVR42k3EMQLAIAgEMP//WkRQVMB2YLgMae/XMhOLCMzdq3svds7B9t6VmWFrLWzOWakqJiLYGKNiZqz3jhHR+wBZbpvd95zR6QAAAABJRU5ErkJggg==\") repeat-x left top;\n}\n/* Firefox\xef\xbc\x88\xef\xbc\x8bGoogle Chrome2\xef\xbc\x89 */\nhtml:not([lang*=]) .sh_heading_main_b,\nhtml:not([lang*=]) .sh_heading_main_b_wide{\n\t/* For Mozilla/Gecko (Firefox etc) */\n\tbackground:-moz-linear-gradient(top, #FFFFFF, #F0F0F0);\n\t/* For WebKit (Safari, Google Chrome etc) */\n\tbackground:-webkit-gradient(linear, left top, left bottom, from(#FFFFFF), to(#F0F0F0));\n}\n/* Safari */\nhtml:not(:only-child:only-child) .sh_heading_main_b,\nhtml:not(:only-child:only-child) .sh_heading_main_b_wide{\n\t/* For WebKit (Safari, Google Chrome etc) */\n\tbackground: -webkit-gradient(linear, left top, left bottom, from(#FFFFFF), to(#F0F0F0));\n}\n", b"body:not(:target) .sh_heading_main_b, body:not(:target) .sh_heading_main_b_wide{background:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAoCAYAAAA/tpB3AAAAQ0lEQVR42k3EMQLAIAgEMP//WkRQVMB2YLgMae/XMhOLCMzdq3svds7B9t6VmWFrLWzOWakqJiLYGKNiZqz3jhHR+wBZbpvd95zR6QAAAABJRU5ErkJggg==) repeat-x left top}html:not([lang*=]) .sh_heading_main_b,\nhtml:not([lang*=]) .sh_heading_main_b_wide{background:-moz-linear-gradient(top,#fff,#f0f0f0);background:-webkit-gradient(linear,left top,left bottom,from(#fff),to(#f0f0f0))}html:not(:only-child:only-child) .sh_heading_main_b,\nhtml:not(:only-child:only-child) .sh_heading_main_b_wide{background:-webkit-gradient(linear,left top,left bottom,from(#fff),to(#f0f0f0))}"),
    (b"@import \"styles.css\"foo; a { color: red; }", b"@import url(styles.css) foo;a{color:red}"),
    (b"@media; a { color: red; }", b"a{color:red}"),
    (b"@media screen, print; a { color: red; }", b"a{color:red}"),
    (b"@-webkit-keyframes wiggle {\n  0% {-webkit-transform:rotate(6deg);}\n  50% {-webkit-transform:rotate(-6deg);}\n  100% {-webkit-transform:rotate(6deg);}\n}\n@-moz-keyframes wiggle {\n  0% {-moz-transform:rotate(6deg);}\n  50% {-moz-transform:rotate(-6deg);}\n  100% {-moz-transform:rotate(6deg);}\n}\n@keyframes wiggle {\n  0% {transform:rotate(6deg);}\n  50% {transform:rotate(-6deg);}\n  100% {transform:rotate(6deg);}\n}\n", b"@-webkit-keyframes wiggle {\n  0% {-webkit-transform:rotate(6deg);}\n  50% {-webkit-transform:rotate(-6deg);}\n  100% {-webkit-transform:rotate(6deg);}\n}@-moz-keyframes wiggle {\n  0% {-moz-transform:rotate(6deg);}\n  50% {-moz-transform:rotate(-6deg);}\n  100% {-moz-transform:rotate(6deg);}\n}@keyframes wiggle {\n  0% {transform:rotate(6deg);}\n  50% {transform:rotate(-6deg);}\n  100% {transform:rotate(6deg);}\n}"),
    (b"@font-face {font-family:'Ubuntu';font-style:normal;font-weight:normal;src:local('Ubuntu'), url('http://themes.googleusercontent.com/static/fonts/ubuntu/v2/2Q-AW1e_taO6pHwMXcXW5w.ttf') format('truetype')}@font-face{font-family:'Ubuntu';font-style:normal;font-weight:bold;src:local('Ubuntu Bold'), local('Ubuntu-Bold'), url('http://themes.googleusercontent.com/static/fonts/ubuntu/v2/0ihfXUL2emPh0ROJezvraKCWcynf_cDxXwCLxiixG1c.ttf') format('truetype')}", b"@font-face{font-family:'Ubuntu';font-style:normal;font-weight:normal;src:local('Ubuntu') , url(http://themes.googleusercontent.com/static/fonts/ubuntu/v2/2Q-AW1e_taO6pHwMXcXW5w.ttf) format('truetype')}@font-face{font-family:'Ubuntu';font-style:normal;font-weight:bold;src:local('Ubuntu Bold') , local('Ubuntu-Bold') , url(http://themes.googleusercontent.com/static/fonts/ubuntu/v2/0ihfXUL2emPh0ROJezvraKCWcynf_cDxXwCLxiixG1c.ttf) format('truetype')}"),
    (b"@font-face { font-family: 'Roboto Condensed'; font-style: normal; font-weight: 400; src: local('Roboto Condensed Regular'), local('RobotoCondensed-Regular'), url(http://stc.v3.news.zing.vn/css/fonts/Roboto-Condensed.woff) format('woff'); }\n", b"@font-face{font-family:'Roboto Condensed';font-style:normal;font-weight:400;src:local('Roboto Condensed Regular') , local('RobotoCondensed-Regular') , url(http://stc.v3.news.zing.vn/css/fonts/Roboto-Condensed.woff) format('woff')}"),
    (b"@font-face {\n  font-family: 'BebasNeueRegular';\n  src: url('fonts/BebasNeue-webfont.eot');\n  src: url('fonts/BebasNeue-webfont.eot?#iefix') format('embedded-opentype'),\n    url('fonts/BebasNeue-webfont.woff') format('woff'),\n    url('fonts/BebasNeue-webfont.ttf') format('truetype'),\n    url('fonts/BebasNeue-webfont.svg#BebasNeueRegular') format('svg');\n  font-weight: normal;\n  font-style: normal;\n}\n", b"@font-face{font-family:'BebasNeueRegular';src:url(fonts/BebasNeue-webfont.eot);src:url(fonts/BebasNeue-webfont.eot?#iefix) format('embedded-opentype') , url(fonts/BebasNeue-webfont.woff) format('woff') , url(fonts/BebasNeue-webfont.ttf) format('truetype') , url(fonts/BebasNeue-webfont.svg#BebasNeueRegular) format('svg');font-weight:normal;font-style:normal}"),
    (b"@media screen and (-webkit-min-device-pixel-ratio:0) {\n  @font-face {\n    font-family: 'tiefontello';\n    src: url('fonts/tiefontello.svg?88026028#fontello') format('svg');\n  }\n}\n", b"@media screen and (-webkit-min-device-pixel-ratio:0){@font-face{font-family:'tiefontello';src:url(fonts/tiefontello.svg?88026028#fontello) format('svg')}}"),
    (b"@media only screen and (min-device-width: 320px) and (max-device-width: 480px) {\n        body {                padding: 0;\n        }\n        #page {\n                margin-top: 0;\n        }\n        #branding {\n                border-top: none;\n        }\n\n}\n", b"@media only screen and (min-device-width:320px) and (max-device-width:480px){body{padding:0}#page{margin-top:0}#branding{border-top:none}}"),
    (b"@media screen { .a { color: red; } }\n@media screen and (color) { .b { color: green; } }\n@media not screen { .c { color: blue; } }\n@media only screen { .d { color: cyan; } }\n", b"@media screen{.a{color:red}}@media screen and (color){.b{color:green}}@media not screen{.c{color:#00f}}@media only screen{.d{color:#0ff}}"),
    (b"[class^=\"icon-\"],[class*=\" icon-\"] { color: red }", b"[class^=\"icon-\"],[class*=\" icon-\"]{color:red}"),
    (b"body { color: DECAFB }", b"body{color: DECAFB }"),
    (b"#post_content, #post_content p{\n font-family:Helvetica;\n font-size:16px;\n color:333;\n line-height:24px;\n margin-bottom:15px;\n}", b"#post_content,#post_content p{font-family:Helvetica;font-size:16px;color:333;line-height:24px;margin-bottom:15px}"),
    (b"#ac { background:url(\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAG4AAAAfCAA AAAAjTqdDAAAAGXRFWHRTb2Z0d2FyZQBBZG9iZS\") no-repeat; }", b"#ac{background:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAG4AAAAfCAA\\ AAAAjTqdDAAAAGXRFWHRTb2Z0d2FyZQBBZG9iZS) no-repeat}"),
    (b"td { line-height: 0.8em; margin: -0.9in; }", b"td{line-height:.8em;margin:-.9in}"),
    (b"::-moz-selection {background: #f36921 ; color: #fff ; text-shadow:none}", b"::-moz-selection{background:#f36921;color:#fff;text-shadow:none}"),
    (b"A:link, A:visited { color: 0000ff }", b"A:link,A:visited{color: 0000ff }"),
    (b".filehistory a img,#file img:hover{background:white url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAAAAAA6mKC9AAAAGElEQVQYV2N4DwX/oYBhgARgDJjEAAkAAEC99wFuu0VFAAAAAElFTkSuQmCC) repeat;background:white url(http://static.uncyc.org/skins/common/images/Checker-16x16.png?2012-02-15T12:25:00Z) repeat!ie}", b".filehistory a img,#file img:hover{background:#fff url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAAAAAA6mKC9AAAAGElEQVQYV2N4DwX/oYBhgARgDJjEAAkAAEC99wFuu0VFAAAAAElFTkSuQmCC) repeat;background:white url(http://static.uncyc.org/skins/common/images/Checker-16x16.png?2012-02-15T12:25:00Z) repeat!ie}"),
    (b"body{font:13px/1.231,clean;*font-size:small;*font:x-small;font-family:Arial !important}", b"body{font:13px/1.231,clean;*font-size:small;*font:x-small;font-family:Arial!important}"),
    (b".a { color: red; }\n@import url('foo.css');\n.b { color: blue; }\n", b".a{color:red}@import url('foo.css');.b{color:#00f}"),
    (b"* html #profilePhoto {\n  height: expression((this.flag == undefined) ? (this.scrollHeight > 500) ? this.flag = '500px' : 'auto' : '');\n}", b"* html #profilePhoto{height: expression((this.flag == undefined) ? (this.scrollHeight > 500) ? this.flag = '500px' : 'auto' : '')}"),
    (b".b-search__button{position:relative;min-width:50px;margin:0 0 0 -1px;padding:1px 0 1px 1px;-webkit-border-top-left-radius:2px;border-top-left-radius:2px;-webkit-border-bottom-left-radius:2px;border-bottom-left-radius:2px;background:-webkit-gradient(linear,0 0,0 100%,from(rgba(192,192,192,.6)),to(rgba(49,49,49,.3)));background:-o-linear-gradient(top,(rgba(192,192,192,.6)),(rgba(49,49,49,.3)))}", b".b-search__button{position:relative;min-width:50px;margin:0 0 0 -1px;padding:1px 0 1px 1px;-webkit-border-top-left-radius:2px;border-top-left-radius:2px;-webkit-border-bottom-left-radius:2px;border-bottom-left-radius:2px;background:-webkit-gradient(linear,0 0,0 100%,from(rgba(192,192,192,.6)),to(rgba(49,49,49,.3)));background:-o-linear-gradient(top,(rgba(192,192,192,.6)),(rgba(49,49,49,.3)))}"),
    (b"@media (color) and screen { .a { color: red; } }", b"@media (color) and screen { .a { color: red; } }"),
    (b"@media only screen and(-webkit-min-device-pixel-ratio:1.5),only screen and(min--moz-device-pixel-ratio:1.5),only screen and(min-resolution:240dpi){.ui-icon-plus,.ui-icon-minus,.ui-icon-delete,.ui-icon-arrow-r,.ui-icon-arrow-l,.ui-icon-arrow-u,.ui-icon-arrow-d,.ui-icon-check,.ui-icon-gear,.ui-icon-refresh,.ui-icon-forward,.ui-icon-back,.ui-icon-grid,.ui-icon-star,.ui-icon-alert,.ui-icon-info,.ui-icon-home,.ui-icon-search,.ui-icon-searchfield:after,.ui-icon-checkbox-off,.ui-icon-checkbox-on,.ui-icon-radio-off,.ui-icon-radio-on{background-image:url(images/icons-36-white.png);-moz-background-size:776px 18px;-o-background-size:776px 18px;-webkit-background-size:776px 18px;background-size:776px 18px;}.ui-icon-alt{background-image:url(images/icons-36-black.png);}}", b"@media only screen and(-webkit-min-device-pixel-ratio:1.5),only screen and(min--moz-device-pixel-ratio:1.5),only screen and(min-resolution:240dpi){.ui-icon-plus,.ui-icon-minus,.ui-icon-delete,.ui-icon-arrow-r,.ui-icon-arrow-l,.ui-icon-arrow-u,.ui-icon-arrow-d,.ui-icon-check,.ui-icon-gear,.ui-icon-refresh,.ui-icon-forward,.ui-icon-back,.ui-icon-grid,.ui-icon-star,.ui-icon-alert,.ui-icon-info,.ui-icon-home,.ui-icon-search,.ui-icon-searchfield:after,.ui-icon-checkbox-off,.ui-icon-checkbox-on,.ui-icon-radio-off,.ui-icon-radio-on{background-image:url(images/icons-36-white.png);-moz-background-size:776px 18px;-o-background-size:776px 18px;-webkit-background-size:776px 18px;background-size:776px 18px;}.ui-icon-alt{background-image:url(images/icons-36-black.png);}}"),
    (b"@media all and(-webkit-max-device-pixel-ratio:10000),\n   not all and(-webkit-min-device-pixel-ratio:0) {\n\n\t:root .RadTreeView_rtl .rtPlus,\n\t:root .RadTreeView_rtl .rtMinus\n\t{\n\t\tposition: relative;\n\t\tmargin-left: 2px;\n\t\tmargin-right: -13px;\n\t\tright: -15px;\n\t}\n}\n", b"@media all and(-webkit-max-device-pixel-ratio:10000),\n   not all and(-webkit-min-device-pixel-ratio:0) {\n\n\t:root .RadTreeView_rtl .rtPlus,\n\t:root .RadTreeView_rtl .rtMinus\n\t{\n\t\tposition: relative;\n\t\tmargin-left: 2px;\n\t\tmargin-right: -13px;\n\t\tright: -15px;\n\t}\n}"),
    (b"@media screen and (min-width: 0 \\0){.ending-actions li a .icon-top,.ending-actions li a .icon-feed{vertical-align:text-bottom}.nav-previous{margin:20px auto 0}}", b"@media screen and (min-width: 0 \\0){.ending-actions li a .icon-top,.ending-actions li a .icon-feed{vertical-align:text-bottom}.nav-previous{margin:20px auto 0}}"),
    (b"@media screen {\n  .foo { color: red; }\n  @font-face { font-family: 'Cibric'; }\n}\n", b"@media screen{@font-face{font-family:'Cibric'}}@media screen{.foo{color:red}}"),
];

/// Real-world parse failures (`ComplexCssTest`).
const COMPLEX_FAIL: &[&[u8]] = &[
    b"}}",
    b"@foobar this is totally wrong CSS syntax }",
    b"@media screen and (max-width: 478px) {\n  .main-header h2 {\n    height: auto;\n    margin-top: 2px;\n    width: 70px;\n",
    b"   .dd-container { \n           position:relative; \n           margin:0px;\n   }\xe2\x80\x8b \n   .dd-selected-text { \n           font-weight:bold;\n           cursor:pointer !important;\n   }\xe2\x80\x8b",
    b"-moz-appearance:none!important;html{height:100%}",
    b"-moz-foo { -webkit-bar: -ie-quuz }",
];

#[test]
fn rewrite_various_css() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    for (i, css) in VARIOUS_GOOD.iter().enumerate() {
        fixture.validate_rewrite(
            &format!("distilled_css_good{i}"),
            *css,
            *css,
            EXPECT_SUCCESS,
        );
    }
    // An @media range comparison survives an external
    // stylesheet rewrite unchanged; inline it would trip the
    // markup gate instead.
    let range = b"@media not (-moz-dimension-constraints:20 < width < 300 and 45 < height < 1000){a{color:red}}".as_slice();
    fixture.validate_rewrite_external_css("range_comparison_query", range, range, EXPECT_SUCCESS);
    fixture.debug_with_message("<!--CSS rewrite failed: Parse error in %url%-->");
    for (i, css) in VARIOUS_FAIL.iter().enumerate() {
        fixture.validate_fail_parse(&format!("distilled_css_fail{i}"), *css);
    }
}

#[test]
fn embedded_tag() {
    let fixture = Fixture::new();
    // We can only test this with ExternalCss, because inline it
    // actually works :)
    let input = b"a { color: red } </style><style>.foo { color: green }".as_slice();
    fixture.validate_rewrite_external_css("embedded_tag", input, input, EXPECT_FAILURE);
}

#[test]
fn to_optimize() {
    let fixture = Fixture::new();
    for (i, (input, expected)) in TO_OPTIMIZE.iter().enumerate() {
        fixture.validate_rewrite(
            &format!("to_optimize_{i}"),
            *input,
            *expected,
            EXPECT_SUCCESS,
        );
    }
}

#[test]
fn complex_css_test() {
    let fixture = Fixture::new();
    for (i, (input, expected)) in COMPLEX.iter().enumerate() {
        fixture.validate_rewrite(
            &format!("complex_css{i}"),
            *input,
            *expected,
            EXPECT_SUCCESS,
        );
    }
    fixture.debug_with_message("<!--CSS rewrite failed: Parse error in %url%-->");
    for (i, css) in COMPLEX_FAIL.iter().enumerate() {
        fixture.validate_fail_parse(&format!("complex_css_parse_fail{i}"), *css);
    }
}

#[test]
fn no_always_rewrite_css() {
    let fixture = Fixture::new();
    // When we force always_rewrite_css, we can expand some statements.
    fixture.options.borrow_mut().set_always_rewrite_css(true);
    fixture.debug_with_message("");
    fixture.validate_rewrite(
        "expanding_example",
        "@import 'http://www.example.com';",
        "@import url(http://www.example.com);",
        EXPECT_SUCCESS,
    );
    // With it set false, we do not expand CSS (as long as we didn't do
    // anything else, like rewrite sub-resources).
    fixture.options.borrow_mut().set_always_rewrite_css(false);
    fixture.debug_with_message("<!--CSS rewrite failed: Cannot improve %url%-->");
    fixture.validate_rewrite(
        "non_expanding_example",
        "@import 'http://www.example.com';",
        "@import 'http://www.example.com';",
        EXPECT_NO_CHANGE,
    );
    // When we force always_rewrite_css, we allow rewriting something to
    // nothing.
    fixture.options.borrow_mut().set_always_rewrite_css(true);
    fixture.debug_with_message("");
    fixture.validate_rewrite("contracting_example", "  ", "", EXPECT_SUCCESS);
    // We still contract it with set_always_rewrite_css(false).
    fixture.options.borrow_mut().set_always_rewrite_css(false);
    fixture.validate_rewrite("contracting_example2", "  ", "", EXPECT_SUCCESS);
}

#[test]
fn remove_comments() {
    let fixture = Fixture::new();
    fixture.validate_rewrite(
        "remove_comments",
        " /* This comment will be removed. */ ",
        "",
        EXPECT_SUCCESS,
    );
}

#[test]
fn inline_css_optimized_from_cache() {
    let fixture = Fixture::new();
    fixture.validate_rewrite(
        "css_html",
        "body {color:DECAFB}",
        "body{color:DECAFB}",
        EXPECT_SUCCESS,
    );
    fixture.validate_rewrite(
        "css_html",
        "body {color:DECAFB}",
        "body{color:DECAFB}",
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
}

#[test]
fn no_quirks_mode_fixes() {
    let fixture = Fixture::new();
    fixture.validate_rewrite(
        "quirks_html",
        "body {color:DECAFB}",
        "body{color:DECAFB}",
        EXPECT_SUCCESS,
    );
    // NOTE: the XHTML case reuses the cached HTML result; the sync
    // port rewrites again with identical output.
    fixture.validate_rewrite(
        "quirks_xhtml",
        "body {color:DECAFB}",
        "body{color:DECAFB}",
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
}

#[test]
fn retain_extra_headers() {
    let fixture = Fixture::new();
    let url = format!("{TEST_DOMAIN}retain.css");
    fixture.mock_css(&url, INPUT_STYLE.as_bytes(), 300);
    fixture
        .fetcher
        .add_response_header(&url, "Etag", "Custom-Etag");
    fixture
        .fetcher
        .add_response_header(&url, "extra", "attribute");
    fixture
        .fetcher
        .add_response_header(&url, "Set-Cookie", "Custom-Cookie");
    let (content, response) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.retain.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(content, OUTPUT_STYLE.as_bytes());
    assert_eq!(
        response.headers().lookup1("extra"),
        Some("attribute".to_string())
    );
    assert_eq!(response.headers().lookup1("Set-Cookie"), None);
    assert_eq!(
        response.headers().lookup1("Etag"),
        Some("W/\"0\"".to_string())
    );
}

#[test]
fn rewrite_style_attribute() {
    let fixture = Fixture::new();
    // Test that nothing happens if rewriting is disabled (default).
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}RewriteStyleAttribute.html"),
            "<div style='background-color: #f00; color: yellow;'/>"
        ),
        "<div style='background-color: #f00; color: yellow;'/>"
    );
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributes);
    // Test no rewriting.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}no-rewriting.html"),
            "<div style='background-color:red;color:#ff0'/>"
        ),
        "<div style='background-color:red;color:#ff0'/>"
    );
    // Test successful rewriting.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}rewrite-simple.html"),
            "<div style='background-color: #f00; color: yellow;'/>"
        ),
        "<div style='background-color:red;color:#ff0'/>"
    );
    // NOTE: log-record assertions wait for the log record.
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    headers.set_first_line(1, 1, 404, "Not Found");
    headers.compute_caching();
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}404.css"), headers, b"");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}rewrite-mixed.html"),
            "<div style=\"  background-image: url('images/watch-icons.png?1');\n  background-position: -19px 60%;\">\n<link rel=stylesheet href='404.css'>\n<span style=\"font-family: Verdana\">Verdana</span>\n</div>"
        ),
        "<div style=\"background-image:url(images/watch-icons.png?1);background-position:-19px 60%\">\n<link rel=stylesheet href='404.css'><!--4xx status code, preventing rewriting of http://test.com/404.css-->\n<span style=\"font-family:Verdana\">Verdana</span>\n</div>"
    );
    // Test that nothing happens if we have a style attribute on a
    // style element, which is actually invalid.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}rewrite-style-with-style.html"),
            "<style style='background-color: #f00; color: yellow;'/>"
        ),
        "<style style='background-color: #f00; color: yellow;'/>"
    );
}

#[test]
fn dont_absolutify_css_import_urls() {
    let fixture = Fixture::new();
    let styles_css = ".background_red{background-color:red}.foreground_yellow{color:#ff0}";
    let css_in = format!(
        "@import url(media/print.css) print;@import url(media/screen.css) screen;{styles_css}"
    );
    fixture.mock_css(&format!("{TEST_DOMAIN}styles.css"), css_in.as_bytes(), 100);
    let html = format!(
        "<head>\n  <title>Example style outline</title>\n  <!-- Style starts here -->\n  <style type='text/css'>{css_in}</style>\n  <!-- Style ends here -->\n</head>"
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}dont_absolutify_css_import_urls.html"),
            &html
        ),
        html
    );
}

#[test]
fn dont_absolutify_empty_url() {
    let fixture = Fixture::new();
    fixture.validate_rewrite(
        "empty_url_in_rule",
        "#gallery { list-style: none outside url(''); }",
        "#gallery{list-style:none outside url()}",
        EXPECT_SUCCESS,
    );
    fixture.validate_rewrite(
        "empty_url_in_import",
        "@import url('');",
        "@import url();",
        EXPECT_SUCCESS,
    );
}

#[test]
fn simple_fetch() {
    let fixture = Fixture::new();
    fixture.mock_css(
        &format!("{TEST_DOMAIN}style.css"),
        INPUT_STYLE.as_bytes(),
        100,
    );
    let (output, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.style.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(output, OUTPUT_STYLE.as_bytes());
}

#[test]
fn simple_fetch_unhealthy() {
    let fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}style.css"),
        INPUT_STYLE.as_bytes(),
        100,
    );
    let (output, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}A.style.css.pagespeed.cf.0.css"))
        .expect("served");
    assert_eq!(output, OUTPUT_STYLE.as_bytes());
}

#[test]
fn legacy_letter_url_still_fetches() {
    let fixture = Fixture::new();
    fixture.mock_css(
        &format!("{TEST_DOMAIN}style.css"),
        INPUT_STYLE.as_bytes(),
        100,
    );
    for leaf in [
        "W.style.css.pagespeed.cf.0.css",
        "V.style.css.pagespeed.cf.0.css",
    ] {
        let (output, _) = fixture
            .serve_url(&format!("{TEST_DOMAIN}{leaf}"))
            .unwrap_or_else(|| panic!("serve {leaf}"));
        assert_eq!(output, OUTPUT_STYLE.as_bytes(), "leaf: {leaf}");
    }
}

#[test]
fn empty_leaf_fetch() {
    let fixture = Fixture::new();
    // CSS URL ends in /
    fixture.mock_css(&format!("{TEST_DOMAIN}style/"), INPUT_STYLE.as_bytes(), 100);
    // NOTE: intentionally explicit (no `Encode` helper), exactly like
    // the C++; `empty_leaf_full` covers the full flow.
    let (output, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}style/I..pagespeed.cf.Hash.css"))
        .expect("served");
    assert_eq!(output, OUTPUT_STYLE.as_bytes());
}

#[test]
fn empty_leaf_full() {
    let fixture = Fixture::new();
    // CSS URL ends in /
    fixture.validate_rewrite_external_css_url(
        "empty_leaf",
        &format!("{TEST_DOMAIN}style/"),
        INPUT_STYLE.as_bytes(),
        OUTPUT_STYLE.as_bytes(),
        EXPECT_SUCCESS,
    );
}

#[test]
fn alternate_stylesheet() {
    let fixture = Fixture::new();
    fixture.mock_css(
        &format!("{TEST_DOMAIN}foo.css"),
        INPUT_STYLE.as_bytes(),
        100,
    );
    let new_url = fixture.expected_url_for_css("foo", OUTPUT_STYLE.as_bytes());
    for (id, rel, rewritten) in [
        ("preferred_stylesheet", "stylesheet", true),
        ("alternate_stylesheet", "alternate stylesheet", true),
        ("alternate_stylesheet2", " StyleSheet alterNATE  ", true),
        (
            "alternate_stylesheet_and_more",
            "  foo stylesheet alternate bar ",
            true,
        ),
        ("alternate_not_stylesheet", "alternate snowflake", false),
    ] {
        let input = format!("<link rel='{rel}' href='foo.css' title='foo'>");
        let expected = if rewritten {
            format!("<link rel='{rel}' href='{new_url}' title='foo'>")
        } else {
            input.clone()
        };
        assert_eq!(
            fixture.parse(&format!("{TEST_DOMAIN}{id}.html"), &input),
            expected,
            "id: {id}"
        );
    }
}

#[test]
fn inline_style_does_not_gain_literal_markup() {
    let fixture = Fixture::new();
    for (id, html) in [
        (
            "selector",
            "<style>li[title=\"1\\3C 2\\3E 0\"]{color:blue}</style>",
        ),
        ("charset", "<style>@charset \"\\3Cpre\";</style>"),
        ("id_start_escape", "<style>#\\3Cnav{margin:2px}</style>"),
    ] {
        assert_eq!(
            fixture.parse(&format!("{TEST_DOMAIN}{id}.html"), html),
            html,
            "id: {id}"
        );
    }
}

#[test]
fn inline_style_without_markup_is_minified() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}plain.html"),
            "<style>p { margin: 0; }</style>"
        ),
        "<style>p{margin:0}</style>"
    );
}

/// Attribute cache-key matrix (`RewriteStyleAttributeDifferentDirs*`).
fn test_style_attribute_different_dirs(css: &str, min_css: &str, expected_blocks: i64) {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributes);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}main_page.html"),
            &format!("<div style='{css}'/>")
        ),
        format!("<div style='{min_css}'/>")
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}subdir/file.html"),
            &format!("<div style='{css}'/>")
        ),
        format!("<div style='{min_css}'/>")
    );
    assert_eq!(fixture.stat(BLOCKS_REWRITTEN), expected_blocks);
}

#[test]
fn rewrite_style_attribute_different_dirs_no_url() {
    // Make sure we don't pointlessly produce different cache keys for
    // attribute CSS w/o URLs in different directories.
    test_style_attribute_different_dirs("color : yellow;  ", "color:#ff0", 1);
}

#[test]
fn rewrite_style_attribute_different_dirs_abs_url() {
    // Make sure we don't pointlessly produce different cache keys for
    // attribute CSS with same absolute URLs in different directories.
    test_style_attribute_different_dirs(
        "background : url(http://example.com/artful.png); ",
        "background:url(http://example.com/artful.png)",
        1,
    );
}

#[test]
fn rewrite_style_attribute_different_dirs_rel_url() {
    // When URLs inside the inline CSS are relative (and resolve to
    // different bases) we should get 2 separate rewrites.
    test_style_attribute_different_dirs(
        "background : url(artful.png); ",
        "background:url(artful.png)",
        2,
    );
}

/// Parses with a mid-stream flush.
fn parse_with_flush(fixture: &Fixture, url: &str, before: &[u8], after: &[u8]) -> Vec<u8> {
    let driver = fixture.driver_for(url);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(CssFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url.as_bytes(), false);
    parser.write(before);
    parser.flush();
    parser.write(after);
    parser.finish();
    sink.bytes()
}

#[test]
fn flush_in_inline_css() {
    let fixture = Fixture::new();
    assert_eq!(
        parse_with_flush(
            &fixture,
            &format!("{TEST_DOMAIN}flush.html"),
            b"<html><body><style>.a { co",
            b"lor: red; }</style></body></html>",
        ),
        b"<html><body><style>.a{color:red}</style></body></html>",
    );
}

#[test]
fn flush_in_end_tag() {
    let fixture = Fixture::new();
    assert_eq!(
        parse_with_flush(
            &fixture,
            &format!("{TEST_DOMAIN}flush.html"),
            b"<html><body><style>.a { color: red; }</st",
            b"yle></body></html>",
        ),
        b"<html><body><style>.a{color:red}</style></body></html>",
    );
}

/// Collects stylesheet link hrefs (`CollectCssLinks`, href scan).
fn collect_css_links(html: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(html);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("href=") {
        rest = &rest[start + 5..];
        let (value, tail) = match rest.chars().next() {
            Some(quote @ ('\'' | '"')) => {
                let rest = &rest[1..];
                match rest.find(quote) {
                    Some(end) => (rest[..end].to_string(), &rest[end + 1..]),
                    None => break,
                }
            }
            _ => {
                let end = rest
                    .find(|c: char| c == '>' || c.is_whitespace())
                    .unwrap_or(rest.len());
                (rest[..end].to_string(), &rest[end..])
            }
        };
        out.push(value);
        rest = tail;
    }
    out
}

/// Absolutification matrix (`TestUrlAbsolutification`).
#[allow(clippy::too_many_arguments)]
fn test_url_absolutification(
    id: &str,
    css_input: &[u8],
    expected_output: &[u8],
    expect_unparseable_section: bool,
    enable_image_rewriting: bool,
    enable_proxy_mode: bool,
    enable_mapping_and_sharding: bool,
    use_test_namer: bool,
) {
    use ps_html::url_namer::{ProxyExtent, TestUrlNamer};
    let fixture = if use_test_namer {
        Fixture::with_test_namer(if enable_proxy_mode {
            ProxyExtent::Full
        } else {
            ProxyExtent::None
        })
    } else {
        Fixture::new()
    };
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteCss);
    if !enable_image_rewriting {
        for filter in [
            Filter::RecompressJpeg,
            Filter::RecompressPng,
            Filter::RecompressWebp,
            Filter::ConvertPngToJpeg,
            Filter::ConvertJpegToWebp,
            Filter::ConvertGifToPng,
            Filter::ConvertToWebpLossless,
            Filter::LeftTrimUrls,
            Filter::ExtendCacheImages,
            Filter::SpriteImages,
        ] {
            fixture.options.borrow_mut().disable_filter(filter);
        }
    }
    if enable_mapping_and_sharding {
        fixture.options.borrow_mut().add_domain("http://cdn.com/");
        fixture.options.borrow_mut().add_domain("http://test.com/");
        fixture
            .options
            .borrow_mut()
            .add_shard("cdn.com", "cdn1.com,cdn2.com");
        let driver = fixture.driver_for(&format!("{TEST_DOMAIN}foo.css"));
        assert!(!driver
            .lawyer()
            .do_domains_serve_same_content("cdn.com", "test.com"));
        fixture
            .options
            .borrow_mut()
            .add_rewrite_domain_mapping("http://cdn.com", "http://test.com");
        let driver = fixture.driver_for(&format!("{TEST_DOMAIN}foo.css"));
        assert!(driver
            .lawyer()
            .do_domains_serve_same_content("cdn.com", "test.com"));
        assert!(driver.lawyer().can_rewrite_domains());
        let src = ps_http::google_url::GoogleUrl::new("http://test.com/foo.css");
        assert_eq!(
            driver.should_absolutify_url(&src, &src),
            (true, false),
            "id: {id}"
        );
        let dst = ps_http::google_url::GoogleUrl::new("http://cdn.com/foo.css");
        assert_eq!(
            driver.should_absolutify_url(&src, &dst),
            (true, false),
            "id: {id}"
        );
    }
    fixture.mock_css(&format!("{TEST_DOMAIN}foo.css"), css_input, 100);
    // The input has (or lacks) parseable-only sections, guarding
    // against parser drift.
    {
        use ps_css::ast::NO_ERROR;
        use ps_css::parser::Parser;
        let mut parser = Parser::new(css_input);
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let _ = parser.parse_raw_stylesheet();
        assert_eq!(parser.errors_seen_mask(), NO_ERROR, "id: {id}");
        assert_eq!(
            parser.unparseable_sections_seen_mask() != NO_ERROR,
            expect_unparseable_section,
            "id: {id}"
        );
    }
    let output = fixture.parse_bytes(
        &format!("{TEST_DOMAIN}{id}.html"),
        b"<link rel=stylesheet href=foo.css>",
    );
    let css_urls = collect_css_links(&output);
    assert!(!css_urls.is_empty(), "id: {id}");
    let leaf = fixture.expected_url_for_name("foo.css", expected_output);
    let expected_url = if enable_mapping_and_sharding {
        format!("http://cdn1.com/{leaf}")
    } else if use_test_namer {
        use ps_cache::namer::ResourceNamer;
        let mut namer = ResourceNamer::new();
        namer.set_id("cf");
        namer.set_hash("0");
        namer.set_name("A.foo.css");
        namer.set_ext("css");
        TestUrlNamer::encode_url(TEST_DOMAIN, "http://test.com", "/", &namer)
    } else {
        leaf
    };
    assert_eq!(css_urls[0], expected_url, "id: {id}");
    let base = ps_http::google_url::GoogleUrl::new(TEST_DOMAIN);
    let css_url = ps_http::google_url::GoogleUrl::resolve(&base, &css_urls[0]);
    let (actual_output, _) = fixture
        .serve_url(css_url.spec())
        .unwrap_or_else(|| panic!("serve {}", css_url.spec()));
    assert_eq!(actual_output, expected_output, "id: {id}");
}

#[test]
fn dont_absolutify_urls_if_no_domain_mapping() {
    let css_input = b"body{background:url(a.png)}@foobar {background: url(a.png), url( http://test.com/b.png ), url('sub/c.png'), url( \"/sub/d.png\"  )}".as_slice();
    test_url_absolutification(
        "dont_absolutify_unparseable_urls_etc_with",
        css_input,
        css_input,
        true,
        true,
        false,
        false,
        false,
    );
    test_url_absolutification(
        "dont_absolutify_unparseable_urls_etc_without",
        css_input,
        css_input,
        true,
        false,
        false,
        false,
        false,
    );
}

#[test]
fn absolutify_unparseable_urls_with_domain_mapping() {
    let css_input = b"body{background:url(a.png)}@foobar {background: url(a.png), url( http://test.com/b.png ), url('sub/c.png'), url( \"/sub/d.png\"  )}".as_slice();
    let css_output = b"body{background:url(http://cdn2.com/a.png)}@foobar {background: url(http://cdn2.com/a.png), url(http://cdn1.com/b.png), url('http://cdn1.com/sub/c.png'), url(\"http://cdn2.com/sub/d.png\")}".as_slice();
    test_url_absolutification(
        "absolutify_unparseable_urls_etc_with",
        css_input,
        css_output,
        true,
        true,
        false,
        true,
        false,
    );
    test_url_absolutification(
        "absolutify_unparseable_urls_etc_without",
        css_input,
        css_output,
        true,
        false,
        false,
        true,
        false,
    );
}

#[test]
fn dont_absolutify_cursor_urls_without_domain_mapping() {
    let css_input = b":link,:visited { cursor: url(example.svg) pointer }".as_slice();
    let expected_output = b":link,:visited{cursor:url(example.svg) pointer}".as_slice();
    test_url_absolutification(
        "dont_absolutify_cursor_urls_etc_with",
        css_input,
        expected_output,
        false,
        true,
        false,
        false,
        false,
    );
    test_url_absolutification(
        "dont_absolutify_cursor_urls_etc_without",
        css_input,
        expected_output,
        false,
        false,
        false,
        false,
        false,
    );
}

#[test]
fn absolutify_cursor_urls_with_domain_mapping() {
    test_url_absolutification(
        "absolutify_cursor_urls_with_domain_mapping",
        b":link,:visited { cursor: url(example.svg) pointer }".as_slice(),
        b":link,:visited{cursor:url(http://cdn2.com/example.svg) pointer}".as_slice(),
        false,
        true,
        false,
        true,
        false,
    );
}

#[test]
fn absolutify_unparseable_urls() {
    let css_input = b"@foobar { background: url(a.png), url( http://test.com/b.png ), url('sub/c.png'), url( \"/sub/d.png\"  ); }\n".as_slice();
    let expected_output = b"@foobar { background: url(http://test.com/a.png), url( http://test.com/b.png ), url('http://test.com/sub/c.png'), url(\"http://test.com/sub/d.png\"); }".as_slice();
    test_url_absolutification(
        "absolutify_unparseable_urls_with",
        css_input,
        expected_output,
        true,
        true,
        true,
        false,
        true,
    );
    test_url_absolutification(
        "do_absolutify_unparseable_urls_without",
        css_input,
        expected_output,
        true,
        false,
        true,
        false,
        true,
    );
}

#[test]
fn absolutify_parseable_urls() {
    test_url_absolutification(
        "absolutify_parseable_urls_with",
        b"body { background: url(a.png); }\n".as_slice(),
        b"body{background:url(http://test.com/a.png)}".as_slice(),
        false,
        true,
        true,
        false,
        true,
    );
    test_url_absolutification(
        "absolutify_parseable_urls_without",
        b"body { background: url(a.png); }\n".as_slice(),
        b"body{background:url(http://test.com/a.png)}".as_slice(),
        false,
        false,
        true,
        false,
        true,
    );
}

#[test]
fn absolutify_other_urls_with_proxy() {
    test_url_absolutification(
        "absolutify_other_urls_with_proxy",
        b":link,:visited { cursor: url(example.svg) pointer }\n.png .itab_prev { behavior: url(/js/iepngfix.htc) }\n.foo { bar: url('baz.ext'); }".as_slice(),
        b":link,:visited{cursor:url(http://test.com/example.svg) pointer}.png .itab_prev{behavior:url(http://test.com/js/iepngfix.htc)}.foo{bar:url(http://test.com/baz.ext)}".as_slice(),
        false,
        true,
        true,
        false,
        true,
    );
}

#[test]
fn absolutify_with_bom() {
    let css_input =
        b"\xef\xbb\xbf@import url(x.ss);\nbody { background: url(a.png); }\n".as_slice();
    let expected_output = b"\xef\xbb\xbf@import url(http://test.com/x.ss);body{background:url(http://test.com/a.png)}".as_slice();
    test_url_absolutification(
        "absolutify_with_bom_with",
        css_input,
        expected_output,
        true,
        true,
        true,
        false,
        true,
    );
    test_url_absolutification(
        "do_absolutify_with_bom_without",
        css_input,
        expected_output,
        true,
        false,
        true,
        false,
        true,
    );
}

#[test]
fn absolutify_serving_fallback() {
    let css_input = b"@import url(x.ss);\nbody { background: url(a.png); }\n".as_slice();
    let expected_output = b"@import url(http://cdn.example.com/x.ss);body{background:url(http://cdn.example.com/a.png)}".as_slice();
    let fixture = Fixture::with_md5();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://cdn.example.com", "http://test.com");
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteCss);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteDomains);
    fixture.mock_css(&format!("{TEST_DOMAIN}foo.css"), css_input, 100);
    // Wrong-hash serve falls back through URL resolution; run twice.
    for _ in 0..2 {
        let (content, _) = fixture
            .serve_url(&format!("{TEST_DOMAIN}A.foo.css.pagespeed.cf.0.css"))
            .expect("served");
        assert_eq!(content, expected_output);
    }
}

#[test]
fn inline_style_that_would_hold_markup_is_left_alone() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    for (id, html) in [
        (
            "noscript",
            "<noscript><style>*[title=\"\\3C/noscript\\3E\"] { color : red }</style></noscript>",
        ),
        (
            "select",
            "<select><style>*[title=\"\\3C input\\3E\"] { color : red }</style></select>",
        ),
        (
            "frameset",
            "<frameset><style>*[title=\"\\3C frame\\3E\"] { color : red }</style></frameset>",
        ),
    ] {
        assert_eq!(
            fixture.parse(&format!("{TEST_DOMAIN}{id}.html"), html),
            html,
            "id: {id}"
        );
    }
}

/// Reads a testdata image (mirrors `image_rewrite.rs`).
fn read_image(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    std::fs::read(dir.join(name)).unwrap_or_else(|_| panic!("missing image {name}"))
}

#[test]
fn servant_flattens_imports_and_rewrites_images() {
    use ps_config::filter::RewriteLevel;
    use ps_html::rewrite_filter::RewriteFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .filters_mut()
        .set_level(RewriteLevel::CoreFilters);
    fixture.mock_css(
        &format!("{TEST_DOMAIN}inner.css"),
        b".inner { margin: 0; }\n",
        300,
    );
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("image/png"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), 300 * 1000, "");
    headers.compute_caching();
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}bg.png"),
        headers,
        &read_image("Cuppa.png"),
    );
    let css_url = format!("{TEST_DOMAIN}outer.css");
    let driver = fixture.driver_for(&css_url);
    let servant = CssFilter::new(driver.clone());
    let input =
        b"@import url(\"inner.css\");\nbody { background: url(\"bg.png\"); color: blue; }\n";
    let content_type = ps_http::content_type::mime_to_content_type("text/css").expect("css known");
    let output = servant
        .rewrite_in_place(&driver, input, &content_type)
        .expect("servant rewrites");
    let text = String::from_utf8(output.bytes).expect("utf8 css");
    assert!(!text.contains("@import"), "import flattened: {text}");
    assert!(text.contains(".inner{margin:0}"), "inner inlined: {text}");
    assert!(
        text.contains(".pagespeed.ic."),
        "image url rewritten: {text}"
    );
    assert!(!text.contains("url(bg.png)"), "original gone: {text}");
}

#[test]
fn servant_leaves_unfetchable_imports_and_images() {
    use ps_config::filter::RewriteLevel;
    use ps_html::rewrite_filter::RewriteFilter;
    // Nothing staged: every nested fetch fails closed and the
    // servant degrades to minify-only.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .filters_mut()
        .set_level(RewriteLevel::CoreFilters);
    let css_url = format!("{TEST_DOMAIN}outer.css");
    let driver = fixture.driver_for(&css_url);
    let servant = CssFilter::new(driver.clone());
    let input =
        b"@import url(\"missing.css\");\nbody { background: url(\"missing.png\"); color: blue; }\n";
    let content_type = ps_http::content_type::mime_to_content_type("text/css").expect("css known");
    let output = servant
        .rewrite_in_place(&driver, input, &content_type)
        .expect("minify still applies");
    let text = String::from_utf8(output.bytes).expect("utf8 css");
    assert!(text.contains("@import"), "import kept: {text}");
    assert!(text.contains("missing.png"), "image kept: {text}");
}
