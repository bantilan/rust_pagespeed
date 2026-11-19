//! CSS image-rewriting tests ported from `css_image_rewriter_test.cc`.
//!
//! NOTE: the metrics case waits for the experiment subsystem.
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
use ps_html::filters::css::{
    CssFilter, BLOCKS_REWRITTEN, FALLBACK_FAILURES, FALLBACK_REWRITES, FLATTEN_CHARSET_MISMATCH,
    FLATTEN_COMPLEX_QUERIES, FLATTEN_INVALID_URL, FLATTEN_LIMIT_EXCEEDED, FLATTEN_MINIFY_FAILED,
    FLATTEN_RECURSION, PARSE_FAILURES, REWRITES_DROPPED, TOTAL_BYTES_SAVED, TOTAL_ORIGINAL_BYTES,
    USES,
};
use ps_html::filters::image_rewrite::ImageRewriteFilter;
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::RequestHeaders;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const BIKE_PNG: &str = "BikeCrashIcn.png";
const CUPPA_PNG: &str = "Cuppa.png";
const PUZZLE_JPG: &str = "Puzzle.jpg";
const DUMMY_CONTENT: &[u8] = b"Invalid PNG but it does not matter for this test";

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
    user_agent: RefCell<String>,
    request_headers: RefCell<RequestHeaders>,
    /// Expected debug message (`debug_message_`, `%url%` substituted).
    debug_message: RefCell<String>,
}

impl Fixture {
    fn new() -> Self {
        Self::build(None, true)
    }

    /// Builds a fixture with the proxy test namer (`SetUseTestUrlNamer`).
    fn with_test_namer() -> Self {
        let namer = Rc::new(ps_html::url_namer::TestUrlNamer::new());
        Self::build(Some(namer), true)
    }

    /// Builds a fixture with the real MD5 hasher (`UseMd5Hasher`).
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
        // Set up before filters so the CSS filter is created aware of these.
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::ExtendCacheImages);
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::FallbackRewriteCssUrls);
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::RewriteCss);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            http_backend,
            metadata_backend,
            user_agent: RefCell::new(String::new()),
            request_headers: RefCell::new(RequestHeaders::new()),
            debug_message: RefCell::new(String::new()),
        }
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver
    }

    /// Installs the UA (`SetCurrentUserAgent`).
    fn set_current_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
    }

    /// Stages a request header (`AddRequestAttribute`).
    fn add_request_attribute(&self, name: &str, value: &str) {
        self.request_headers
            .borrow_mut()
            .headers_mut()
            .add(name, value);
    }

    /// Sets up webp UA + Accept (`SetupForWebp`).
    fn setup_for_webp(&self) {
        self.set_current_user_agent("webp");
        self.add_request_attribute("Accept", "image/webp");
    }

    /// Sets up lossless-webp UA + Accept (`SetupForWebpLossless`).
    fn setup_for_webp_lossless(&self) {
        self.set_current_user_agent("webp-la");
        self.add_request_attribute("Accept", "image/webp");
    }

    /// Enables Debug and stages the expected message
    /// (`DebugWithMessage`).
    fn debug_with_message(&self, message: &str) {
        self.options.borrow_mut().enable_filter(Filter::Debug);
        *self.debug_message.borrow_mut() = message.to_string();
    }

    /// Stages the expected message (`debug_message_`).
    fn set_debug_message(&self, message: &str) {
        *self.debug_message.borrow_mut() = message.to_string();
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

    /// Scripts a fetch with default headers
    /// (`SetResponseWithDefaultHeaders`).
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

    /// Scripts a testdata image fetch (`AddFileToMockFetcher`).
    fn add_file_to_mock_fetcher(&self, url: &str, file: &str, content_type: &str, ttl_sec: i64) {
        let url = if url.contains("://") {
            url.to_string()
        } else {
            format!("{TEST_DOMAIN}{url}")
        };
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(file),
        )
        .unwrap_or_else(|_| panic!("missing image {file}"));
        self.set_response_with_default_headers(&url, content_type, &body, ttl_sec);
    }

    /// Parses through CSS, image, and cache-extension filters.
    fn parse(&self, url: &str, html: &str) -> String {
        String::from_utf8(self.parse_bytes(url, html.as_bytes())).unwrap()
    }

    /// Parses through the filter chain, keeping raw bytes.
    fn parse_bytes(&self, url: &str, html: &[u8]) -> Vec<u8> {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(ImageRewriteFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(CacheExtenderFilter::new(driver)) as Box<dyn HtmlFilter>);
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
        driver.add_filter(Rc::new(ImageRewriteFilter::new(driver.clone())));
        driver.add_filter(Rc::new(CacheExtenderFilter::new(driver.clone())));
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

    /// Serves through a fresh server sharing the fetcher and options.
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
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver.add_filter(Rc::new(CssFilter::new(driver.clone())));
        driver.add_filter(Rc::new(ImageRewriteFilter::new(driver.clone())));
        driver.add_filter(Rc::new(CacheExtenderFilter::new(driver.clone())));
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
            FALLBACK_FAILURES,
            FALLBACK_REWRITES,
            PARSE_FAILURES,
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
        let html_input = Self::inline_html(css_input, "");
        let debug = self.debug_message.borrow().clone();
        let html_output = Self::inline_html(css_output, &debug);
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
        self.validate_rewrite_external_css_url_with_expected(
            id,
            css_url,
            &expected_new_url,
            css_input,
            css_output,
            flags,
        );
    }

    /// Validates an external rewrite against an explicit URL (test
    /// namers mangle the outer link).
    fn validate_rewrite_external_css_url_with_expected(
        &self,
        id: &str,
        css_url: &str,
        expected_new_url: &str,
        css_input: &[u8],
        css_output: &[u8],
        flags: u32,
    ) {
        self.mock_css(css_url, css_input, 300);
        let html_input = Self::external_html(css_url, "");
        let rewrite_url = flags & (EXPECT_SUCCESS | EXPECT_CACHED | EXPECT_FALLBACK) != 0;
        let debug = self.debug_message.borrow().clone();
        let html_output = Self::external_html(&expected_new_url, &debug);
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

    /// Fetches a rewritten URL by filter id (`FetchResource`).
    fn fetch_resource(&self, path: &str, filter_id: &str, name: &str, ext: &str) -> Vec<u8> {
        let url = encode(path, filter_id, "0", name, ext);
        let (content, _) = self
            .serve_url(&url)
            .unwrap_or_else(|| panic!("serve {url}"));
        content
    }

    /// Clears UA and request headers (`ClearRewriteDriver`).
    fn clear_rewrite_driver(&self) {
        *self.user_agent.borrow_mut() = String::new();
        *self.request_headers.borrow_mut() = RequestHeaders::new();
    }

    /// Clears both caches (`lru_cache()->Clear()`).
    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
    }

    /// Serves from this and a fresh context, comparing
    /// (`ServeResourceFromManyContexts`).
    fn serve_resource_from_many_contexts(&self, url: &str, expected: &[u8]) {
        let (actual, _) = self.serve_url(url).unwrap_or_else(|| panic!("serve {url}"));
        assert_eq!(actual.as_slice(), expected, "url: {url}");
        let (actual, _) = self
            .serve_from_new_context(url)
            .unwrap_or_else(|| panic!("reserve {url}"));
        assert_eq!(actual.as_slice(), expected, "url: {url}");
    }

    /// Enables the recompress/convert set (`AddRecompressImageFilters`).
    fn add_recompress_image_filters(&self) {
        let mut options = self.options.borrow_mut();
        options.soft_enable_filter_for_testing(Filter::RecompressJpeg);
        options.soft_enable_filter_for_testing(Filter::RecompressPng);
        options.soft_enable_filter_for_testing(Filter::RecompressWebp);
        options.soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
        options.soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
        options.soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    }
}

/// Encodes a rewritten leaf (`Encode`).
fn encode(prefix: &str, filter_id: &str, hash: &str, leaf: &str, ext: &str) -> String {
    // Only the image segment encoder `x`-prefixes its segment; the
    // CSS encoder `A.`-prefixes; the default encoder escapes as-is.
    // All three escape the leaf for URL segments.
    let escaped = ps_cache::namer::encode_url_segment_bytes(leaf.as_bytes());
    let segment = if filter_id == "ic" {
        format!("x{escaped}")
    } else if filter_id == "cf" {
        format!("A.{escaped}")
    } else {
        escaped
    };
    format!("{prefix}{segment}.pagespeed.{filter_id}.{hash}.{ext}")
}

/// Makes indented CSS with an image (`MakeIndentedCssWithImage`).
fn make_indented_css_with_image(image_url: &str) -> String {
    format!("body {{\n  background-image: url({image_url});\n}}\n")
}

/// Makes minified CSS with an image (`MakeMinifiedCssWithImage`).
fn make_minified_css_with_image(image_url: &str) -> String {
    format!("body{{background-image:url({image_url})}}")
}

#[test]
fn cache_extends_images_simple() {
    // Simplified version of CacheExtendsImages, which doesn't have many copies of
    // the same URL.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "body {\n  background-image: url(foo.png);\n}\n";
    let css_after = format!(
        "body{{background-image:url({})}}",
        encode("", "ce", "0", "foo.png", "png")
    );

    fixture.validate_rewrite(
        "cache_extends_images",
        CSS_BEFORE,
        &css_after,
        EXPECT_SUCCESS,
    );
}

#[test]
fn cache_extends_images_embedded_comma() {
    // Makes sure image-URL rewriting doesn't corrupt URLs with embedded
    // commas.  Earlier, we were escaping commas in URLs by backslashing
    // the "," and IE8 interprets those backslashes as forward slashes,
    // making the URL incorrect.
    const IMAGE_URL: &str = "foo,bar.png";
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(IMAGE_URL, "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "body {\n  background-image: url(foo,bar.png);\n}\n";
    let css_after = format!(
        "body{{background-image:url({})}}",
        encode("", "ce", "0", IMAGE_URL, "png")
    );

    fixture.validate_rewrite(
        "cache_extends_images",
        CSS_BEFORE,
        &css_after,
        EXPECT_SUCCESS,
    );
}

#[test]
fn cache_extends_images_embedded_space() {
    // Note that GoogleUrl will, internal to our system, convert the space to
    // a %20, so we'll be fetching the percentified form.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo%20bar.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "body {\n  background-image: url('foo bar.png');\n}\n";
    let css_after = format!(
        "body{{background-image:url({})}}",
        encode("", "ce", "0", "foo%20bar.png", "png")
    );

    fixture.validate_rewrite(
        "cache_extends_images",
        CSS_BEFORE,
        &css_after,
        EXPECT_SUCCESS,
    );
}

#[test]
fn cache_extends_images() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("bar.png", "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("baz.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "body {\n  background-image: url(foo.png);\n  list-style-image: url('bar.png');\n}\n.titlebar p.cfoo, #end p {\n  background: url(\"baz.png\");\n  list-style: url('foo.png');\n}\n.other {\n  background-image:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAUAAAAFCAYAAACNbyblAAAAHElEQVQI12P4//8/w38GIAXDIBKE0DHxgljNBAAO9TXL0Y4OHwAAAABJRU5ErkJggg==);  -proprietary-background-property: url(foo.png);\n}";
    let css_after = format!(
        "body{{background-image:url({});list-style-image:url({})}}.titlebar p.cfoo,#end p{{background:url({});list-style:url({})}}.other{{background-image:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAUAAAAFCAYAAACNbyblAAAAHElEQVQI12P4//8/w38GIAXDIBKE0DHxgljNBAAO9TXL0Y4OHwAAAABJRU5ErkJggg==);-proprietary-background-property:url(foo.png)}}",
        encode("", "ce", "0", "foo.png", "png"),
        encode("", "ce", "0", "bar.png", "png"),
        encode("", "ce", "0", "baz.png", "png"),
        encode("", "ce", "0", "foo.png", "png"),
    );

    fixture.validate_rewrite(
        "cache_extends_images",
        CSS_BEFORE,
        &css_after,
        EXPECT_SUCCESS,
    );
}

#[test]
fn dummy_ruleset() {
    // Simplified version of CacheExtendsImages, which doesn't have many copies of
    // the same URL.
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "@font-face { font-family: 'Robotnik'; font-style: normal }\nbody {\n  background-image: url(foo.png);\n}\n@to-infinity and beyond;\n";
    let css_after = format!(
        "@font-face{{font-family:'Robotnik';font-style:normal}}body{{background-image:url({})}}@to-infinity and beyond;",
        encode("", "ce", "0", "foo.png", "png")
    );

    fixture.validate_rewrite(
        "cache_extends_images",
        CSS_BEFORE,
        &css_after,
        EXPECT_SUCCESS,
    );
}

#[test]
fn recompress_images() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    const CSS: &str = "body {\n  background-image: url(foo.png);\n}\n";

    let css_after = format!(
        "body{{background-image:url({})}}",
        encode("", "ic", "0", "foo.png", "png")
    );

    fixture.validate_rewrite_external_css("recompress_css_images", CSS, &css_after, EXPECT_SUCCESS);
}

// See trims_image_urls below: change one, change them both!
#[test]
fn trims_image_urls() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::LeftTrimUrls);
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);
    fixture.validate_rewrite_external_css(
        "trims_css_urls",
        make_indented_css_with_image("foo.png"),
        make_minified_css_with_image(&encode("", "ce", "0", "foo.png", "png")),
        EXPECT_SUCCESS,
    );
}

// See trims_image_urls above: change one, change them both!
#[test]
fn trims_image_urls_url_namer() {
    use ps_html::url_namer::TestUrlNamer;
    // Check that we really are using TestUrlNamer and not UrlNamer.
    let mut leaf = ps_cache::namer::ResourceNamer::new();
    leaf.set_name("foo.png");
    leaf.set_id("ce");
    leaf.set_hash("0");
    leaf.set_ext("png");
    assert_ne!(
        TestUrlNamer::encode_url(TEST_DOMAIN, TEST_DOMAIN, "", &leaf),
        format!("{TEST_DOMAIN}foo.png.pagespeed.ce.0.png"),
    );

    // A verbatim copy of the test above but using TestUrlNamer.
    let fixture = Fixture::with_test_namer();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::LeftTrimUrls);
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);
    let css_input = make_indented_css_with_image("foo.png");
    let css_output = make_minified_css_with_image(&encode("", "ce", "0", "foo.png", "png"));
    let mut leaf = ps_cache::namer::ResourceNamer::new();
    leaf.set_name("A.trims_css_urls.css");
    leaf.set_id("cf");
    leaf.set_hash("0");
    leaf.set_ext("css");
    let expected_new_url = TestUrlNamer::encode_url(TEST_DOMAIN, TEST_DOMAIN, "/", &leaf);
    fixture.validate_rewrite_external_css_url_with_expected(
        "trims_css_urls",
        &format!("{TEST_DOMAIN}trims_css_urls.css"),
        &expected_new_url,
        css_input.as_bytes(),
        css_output.as_bytes(),
        EXPECT_SUCCESS,
    );
}

#[test]
fn inline_paths() {
    // Make sure we properly handle CSS relative references when we have the same
    // inline CSS in different places. This is also a regression test for a bug
    // during development of async + inline case which caused us to do
    // null rewrites from cache.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::LeftTrimUrls);
    fixture.set_response_with_default_headers("dir/foo.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "body {\n  background-image: url(http://test.com/dir/foo.png);\n}\n";

    // Note: Original URL was absolute, so rewritten one is as well.
    let css_after = make_minified_css_with_image(&encode("dir/", "ce", "0", "foo.png", "png"));
    fixture.validate_rewrite_inline_css("nosubdir", CSS_BEFORE, &css_after, EXPECT_SUCCESS);

    let css_after_rel = make_minified_css_with_image(&encode("", "ce", "0", "foo.png", "png"));
    fixture.validate_rewrite_inline_css(
        "dir/yessubdir",
        CSS_BEFORE,
        &css_after_rel,
        EXPECT_SUCCESS,
    );
}

#[test]
fn rewrite_cached() {
    // Make sure we produce the same output from cache.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::LeftTrimUrls);
    fixture.set_response_with_default_headers("dir/foo.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_BEFORE: &str = "body {\n  background-image: url(http://test.com/dir/foo.png);\n}\n";

    let css_after = make_minified_css_with_image(&encode("dir/", "ce", "0", "foo.png", "png"));
    fixture.validate_rewrite_inline_css("nosubdir", CSS_BEFORE, &css_after, EXPECT_SUCCESS);

    fixture.validate_rewrite_inline_css(
        "nosubdir2",
        CSS_BEFORE,
        &css_after,
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
    // Should not re-serialize. Works only under the new flow...
    assert_eq!(0, fixture.up_stat(TOTAL_BYTES_SAVED));
}

#[test]
fn cache_extends_repeated_top_level() {
    // Test to make sure that if we cache extend inside CSS we can do it
    // for the same image in HTML at the same time.
    const IMG: &str = "img.png";
    let extended_img = encode("", "ce", "0", "img.png", "png");

    const CSS: &str = "stylesheet.css";
    let rewritten_css = encode("", "cf", "0", "stylesheet.css", "css");

    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(IMG, "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers(
        CSS,
        "text/css",
        make_minified_css_with_image(IMG).as_bytes(),
        100,
    );

    let html_in = format!("<link rel='stylesheet' href='{CSS}'><img src='{IMG}'>");
    let html_out =
        format!("<link rel='stylesheet' href='{rewritten_css}'><img src='{extended_img}'>");
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}repeated_top_level"), &html_in),
        html_out
    );

    let (css_out, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}{rewritten_css}"))
        .expect("serve css");
    assert_eq!(
        css_out,
        make_minified_css_with_image(&extended_img).as_bytes()
    );
}

#[test]
fn use_correct_base_url() {
    // Initialize resources.
    const CSS_URL: &str = "http://www.example.com/bar/style.css";
    const CSS_BEFORE: &str = "body { background: url(image.png); }";
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(CSS_URL, "text/css", CSS_BEFORE.as_bytes(), 100);
    const IMAGE_URL: &str = "http://www.example.com/bar/image.png";
    fixture.set_response_with_default_headers(IMAGE_URL, "image/png", DUMMY_CONTENT, 100);
    fixture
        .options
        .borrow_mut()
        .add_domain("http://www.example.com/");

    // Construct URL for rewritten image.
    let expected_image_url = encode("", "ce", "0", "image.png", "png");

    let css_after = format!("body{{background:url({expected_image_url})}}");

    // Construct URL for rewritten CSS.
    let expected_css_url = encode("bar/", "cf", "0", "style.css", "css");

    const HTML_BEFORE: &str = "<head>\n  <link rel='stylesheet' href='bar/style.css'>\n</head>";
    let html_after =
        format!("<head>\n  <link rel='stylesheet' href='{expected_css_url}'>\n</head>");

    // Make sure that image.png uses http://www.example.com/bar/style.css as
    // base URL instead of http://www.example.com/.
    assert_eq!(
        fixture.parse("http://www.example.com/", HTML_BEFORE),
        html_after
    );

    let (actual_css_after, _) = fixture
        .serve_url(&format!("http://www.example.com/{expected_css_url}"))
        .expect("serve css");
    assert_eq!(css_after.as_bytes(), actual_css_after.as_slice());
}

#[test]
fn minify_images_embedded_space() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);

    fixture.validate_rewrite(
        "minify",
        make_indented_css_with_image("'foo bar.png'"),
        make_minified_css_with_image("foo\\ bar.png"),
        EXPECT_SUCCESS,
    );
}

#[test]
fn cache_extends_when_css_grows() {
    // We run most tests with set_always_rewrite_css(true) which bypasses
    // checks on whether rewriting is worthwhile or not. Test to make sure we make
    // the right decision when we do do the check in the case where the produced
    // CSS is actually larger, but contains rewritten resources.
    // (We want to rewrite the CSS in that case)
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_always_rewrite_css(false);
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);
    fixture.validate_rewrite(
        "cache_extends_images_growcheck",
        make_indented_css_with_image("foo.png"),
        make_minified_css_with_image(&encode("", "ce", "0", "foo.png", "png")),
        EXPECT_SUCCESS,
    );
}

#[test]
fn cache_inline_parse_failures() {
    const INVALID_CSS: &str = " div{";

    let fixture = Fixture::new();
    fixture.validate_rewrite_inline_css(
        "inline-invalid",
        INVALID_CSS,
        INVALID_CSS,
        EXPECT_FALLBACK,
    );
    assert_eq!(1, fixture.stat(PARSE_FAILURES));

    // kNoStatCheck because we are explicitly depending on an extra failure
    // not being recorded.
    fixture.validate_rewrite_inline_css(
        "inline-invalid2",
        INVALID_CSS,
        INVALID_CSS,
        EXPECT_FALLBACK | NO_STAT_CHECK,
    );
    // Shouldn't reparse -- and stats are reset between runs.
    assert_eq!(0, fixture.stat(PARSE_FAILURES));
}

#[test]
fn cache_extends_images_in_style_attributes() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("bar.png", "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("baz.png", "image/png", DUMMY_CONTENT, 100);

    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteStyleAttributes);

    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}cache_extend_images_simple"),
            "<div style=\"  background-image: url(foo.png);\n  list-style-image: url('bar.png');\n\"/>"
        ),
        format!(
            "<div style=\"background-image:url({});list-style-image:url({})\"/>",
            encode("", "ce", "0", "foo.png", "png"),
            encode("", "ce", "0", "bar.png", "png"),
        )
    );

    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}cache_extend_images"),
            "<div style=\"  background: url(baz.png);\n  list-style: url(&quot;foo.png&quot;);\n\"/>"
        ),
        format!(
            "<div style=\"background:url({});list-style:url({})\"/>",
            encode("", "ce", "0", "baz.png", "png"),
            encode("", "ce", "0", "foo.png", "png"),
        )
    );

    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}dont_cache_extend_data_urls"),
            "<div style=\"  background-image:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAUAAAAFCAYAAACNbyblAAAAHElEQVQI12P4//8/w38GIAXDIBKE0DHxgljNBAAO9TXL0Y4OHwAAAABJRU5ErkJggg==);  -proprietary-background-property: url(foo.png);\n\"/>"
        ),
        "<div style=\"background-image:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAUAAAAFCAYAAACNbyblAAAAHElEQVQI12P4//8/w38GIAXDIBKE0DHxgljNBAAO9TXL0Y4OHwAAAABJRU5ErkJggg==);-proprietary-background-property:url(foo.png)\"/>"
    );
}

// No rewriting if neither option is enabled.
#[test]
fn style_attributes_neither_enabled() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    const DIV_BEFORE: &str = "<div style=\"background-image:url(foo.png)\"/>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}options_disabled"), DIV_BEFORE),
        DIV_BEFORE
    );
}

// No rewriting if only 'style' is enabled.
#[test]
fn style_attributes_only_style_enabled() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteStyleAttributesWithUrl);
    const DIV_BEFORE: &str = "<div style=\"background-image:url(foo.png)\"/>";
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}recompress_images_disabled"),
            DIV_BEFORE
        ),
        DIV_BEFORE
    );
}

// No rewriting if only 'recompress' is enabled.
#[test]
fn style_attributes_only_recompress_enabled() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    fixture.add_recompress_image_filters();
    const DIV_BEFORE: &str = "<div style=\"background-image:url(foo.png)\"/>";
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}recompress_images_disabled"),
            DIV_BEFORE
        ),
        DIV_BEFORE
    );
}

// Rewrite iff both options are enabled.
#[test]
fn style_attributes_recompress_and_style_enabled() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributesWithUrl);
    let div_after = format!(
        "<div style=\"background-image:url({})\"/>",
        encode("", "ic", "0", "foo.png", "png")
    );
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}options_enabled"),
            "<div style=\"background-image:url(foo.png)\"/>"
        ),
        div_after
    );
}

#[test]
fn style_attributes_recompress_and_webp_and_style_enabled() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}foo.jpg"),
        PUZZLE_JPG,
        "image/jpeg",
        100,
    );
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributesWithUrl);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp();
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}webp"),
            "<div style=\"background-image:url(foo.jpg)\"/>"
        ),
        "<div style=\"background-image:url(xfoo.jpg.pagespeed.ic.0.webp)\"/>"
    );
}

#[test]
fn style_attributes_recompress_and_webp_lossless_and_style_enabled() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}foo.jpg"),
        PUZZLE_JPG,
        "image/jpeg",
        100,
    );
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributesWithUrl);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_lossless();
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}webp-lossless"),
            "<div style=\"background-image:url(foo.jpg)\"/>"
        ),
        "<div style=\"background-image:url(xfoo.jpg.pagespeed.ic.0.webp)\"/>"
    );
}

const IE7_UA: &str = "Mozilla/5.0 (Windows; U; MSIE 7.0; Windows NT 6.0; en-US)";
const IE9_UA: &str = "Mozilla/5.0 (Windows; U; MSIE 9.0; WIndows NT 9.0; en-US))";
const FIREFOX_UA: &str =
    "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:27.0) Gecko/20100101 Firefox/27.0";

/// Rewrites HTML, scans the rewritten HTML for a substring, and returns
/// the number of image rewrites that were executed (`TestInlineWithUA`).
fn test_inline_with_ua(
    fixture: &Fixture,
    id: &str,
    html_input: &str,
    expected_substring: &str,
    user_agent: &str,
) -> i64 {
    let start_image_rewrites = fixture.stat(ps_html::filters::image_rewrite::IMAGE_REWRITES);
    fixture.clear_rewrite_driver();
    if user_agent == "webp" {
        fixture.setup_for_webp();
    } else {
        fixture.set_current_user_agent(user_agent);
    }
    let output = fixture.parse(&format!("{TEST_DOMAIN}{id}"), html_input);
    assert!(
        output.contains(expected_substring),
        "{expected_substring} in {output}"
    );
    fixture.stat(ps_html::filters::image_rewrite::IMAGE_REWRITES) - start_image_rewrites
}

// https://github.com/apache/incubator-pagespeed-mod/issues/781
#[test]
fn style_attributes_serve_css_to_different_ua() {
    let fixture = Fixture::new();
    // The C++ fixture enables no cache extension.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}bike.png"),
        BIKE_PNG,
        "image/png",
        100,
    );
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RewriteStyleAttributesWithUrl);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(10000);
    fixture
        .options
        .borrow_mut()
        .set_css_image_inline_max_bytes(10000);

    const HTML_INPUT: &str = "<div style=\"background-image:url(bike.png)\"/>";
    const JPEG_FILE: &str = "<div style=\"background-image:url(xbike.png.pagespeed.ic.0.jpg)\"/>";
    const JPEG_INLINE: &str = "background-image:url(data:image/jpeg;base64";
    const WEBP_INLINE: &str = "background-image:url(data:image/webp;base64";

    // Test to make sure we don't poison the cache with metadata from
    // the wrong UA by making sure we get the expected results in each
    // order.  The return value of TestInlineWithUA is the number of
    // image-rewrites that were executed.  Note that ie7, ie9, and ff
    // each share the same rewritten jpeg file, though ie7 can't inline
    // it.  Thus after clearing cache, we should expect to see exactly
    // two new image rewrites: 1 for jpeg, one for webp.
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "ie7", HTML_INPUT, JPEG_FILE, IE7_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ff", HTML_INPUT, JPEG_INLINE, FIREFOX_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie9", HTML_INPUT, JPEG_INLINE, IE9_UA)
    );
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "webp", HTML_INPUT, WEBP_INLINE, "webp")
    );

    fixture.clear_caches();
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "ie7", HTML_INPUT, JPEG_FILE, IE7_UA)
    );
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "webp", HTML_INPUT, WEBP_INLINE, "webp")
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ff", HTML_INPUT, JPEG_INLINE, FIREFOX_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie9", HTML_INPUT, JPEG_INLINE, IE9_UA)
    );

    fixture.clear_caches();
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "ff", HTML_INPUT, JPEG_INLINE, FIREFOX_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie9", HTML_INPUT, JPEG_INLINE, IE9_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie7", HTML_INPUT, JPEG_FILE, IE7_UA)
    );
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "webp", HTML_INPUT, WEBP_INLINE, "webp")
    );

    fixture.clear_caches();
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "ff", HTML_INPUT, JPEG_INLINE, FIREFOX_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie9", HTML_INPUT, JPEG_INLINE, IE9_UA)
    );
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "webp", HTML_INPUT, WEBP_INLINE, "webp")
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie7", HTML_INPUT, JPEG_FILE, IE7_UA)
    );

    fixture.clear_caches();
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "webp", HTML_INPUT, WEBP_INLINE, "webp")
    );
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "ie7", HTML_INPUT, JPEG_FILE, IE7_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ff", HTML_INPUT, JPEG_INLINE, FIREFOX_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie9", HTML_INPUT, JPEG_INLINE, IE9_UA)
    );

    fixture.clear_caches();
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "webp", HTML_INPUT, WEBP_INLINE, "webp")
    );
    assert_eq!(
        1,
        test_inline_with_ua(&fixture, "ff", HTML_INPUT, JPEG_INLINE, FIREFOX_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie9", HTML_INPUT, JPEG_INLINE, IE9_UA)
    );
    assert_eq!(
        0,
        test_inline_with_ua(&fixture, "ie7", HTML_INPUT, JPEG_FILE, IE7_UA)
    );
}

/// Reads the inline test image (`SetUpTestImageFile`).
fn test_image_file_contents() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(CUPPA_PNG),
    )
    .expect("test image")
}

/// Makes the test image data URL (`TestImageDataUrl`).
fn test_image_data_url() -> String {
    let content_type = ps_http::content_type::mime_to_content_type("image/png").expect("png type");
    String::from_utf8(ps_http::data_url::make_data_url(
        content_type,
        ps_http::data_url::Encoding::Base64,
        &test_image_file_contents(),
    ))
    .unwrap()
}

/// Builds the inline-image fixture (`InlineCssImageRewriterTest`).
fn inline_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineImages);
    let contents = test_image_file_contents();
    assert!(!contents.is_empty());
    fixture.set_response_with_default_headers(
        &format!("{TEST_DOMAIN}{CUPPA_PNG}"),
        "image/png",
        &contents,
        100,
    );
    fixture
}

/// Sets both inline budgets (`SetMaxBytes`).
fn set_max_bytes(fixture: &Fixture, image_inline_max_bytes: i64, css_image_inline_max_bytes: i64) {
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(image_inline_max_bytes);
    fixture
        .options
        .borrow_mut()
        .set_css_image_inline_max_bytes(css_image_inline_max_bytes);
    assert_eq!(
        image_inline_max_bytes,
        fixture.options.borrow().image_inline_max_bytes()
    );
    assert_eq!(
        css_image_inline_max_bytes,
        fixture.options.borrow().css_image_inline_max_bytes()
    );
}

#[test]
fn inline_images() {
    let fixture = inline_fixture();
    let num_bytes = test_image_file_contents().len() as i64;
    set_max_bytes(&fixture, num_bytes + 1, num_bytes + 1);
    let input_css = format!("body {{\n  background-image: url({CUPPA_PNG});\n}}\n");
    let expected_css = format!("body{{background-image:url({})}}", test_image_data_url());

    // Skip the stat check because inlining *increases* the CSS size and
    // causes the check to fail. Inlining eliminates a resource fetch, so
    // it should normally be a net win in practice.
    fixture.validate_rewrite(
        "inline_css_images",
        input_css,
        expected_css,
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
}

#[test]
fn no_inline_when_image_too_large_for_css() {
    let fixture = inline_fixture();
    let num_bytes = test_image_file_contents().len() as i64;
    set_max_bytes(&fixture, num_bytes + 1, num_bytes);
    let input_css = format!("body {{\n  background-image: url({CUPPA_PNG});\n}}\n");
    let expected_css = format!(
        "body{{background-image:url({})}}",
        encode("", "ce", "0", CUPPA_PNG, "png")
    );

    fixture.validate_rewrite(
        "no_inline_when_image_too_large_for_css",
        input_css,
        expected_css,
        EXPECT_SUCCESS,
    );
}

#[test]
fn inline_in_external_css_only() {
    let fixture = inline_fixture();
    let num_bytes = test_image_file_contents().len() as i64;
    set_max_bytes(&fixture, num_bytes, num_bytes + 1);
    let input_css = format!("body {{\n  background-image: url({CUPPA_PNG});\n}}\n");
    let expected_inline_css = format!(
        "body{{background-image:url({})}}",
        encode("", "ce", "0", CUPPA_PNG, "png")
    );
    let expected_outline_css = format!("body{{background-image:url({})}}", test_image_data_url());

    fixture.validate_rewrite_inline_css(
        "no_inline_in_inline",
        &input_css,
        &expected_inline_css,
        EXPECT_SUCCESS,
    );
    fixture.validate_rewrite_external_css(
        "inline_in_external",
        &input_css,
        &expected_outline_css,
        EXPECT_SUCCESS | NO_STAT_CHECK,
    );
}

#[test]
fn css_image_preserve_urls() {
    use ps_html::filters::image_rewrite::IMAGE_REWRITES;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.options.borrow_mut().set_image_preserve_urls(true);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    const CSS: &str = "body {\n  background-image: url(foo.png);\n}\n";

    const CSS_AFTER: &str = "body{background-image:url(foo.png)}";
    // The CSS should minify but the URL shouldn't change.
    fixture.validate_rewrite_external_css(
        "compress_preserve_css_images",
        CSS,
        CSS_AFTER,
        EXPECT_SUCCESS,
    );

    // We should have optimized the image even though we didn't render the URL.
    fixture.server.http_cache().clear_stats();
    fixture.server.variable(IMAGE_REWRITES).clear();
    let out_img_url = encode(TEST_DOMAIN, "ic", "0", "foo.png", "png");
    let (out_img, _) = fixture.serve_url(&out_img_url).expect("serve img");
    assert!(!out_img.is_empty());
    // Served from cache: a backend hit, no new rewrite.
    assert_eq!(1, fixture.server.http_cache().stats().backend_hits);
    assert_eq!(0, fixture.stat(IMAGE_REWRITES));
}

#[test]
fn css_image_preserve_urls_no_preemptive_rewrite() {
    use ps_html::filters::image_rewrite::IMAGE_REWRITES;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.options.borrow_mut().set_image_preserve_urls(true);
    fixture
        .options
        .borrow_mut()
        .set_in_place_preemptive_rewrite_css_images(false);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    const CSS: &str = "body {\n  background-image: url(foo.png);\n}\n";

    const CSS_AFTER: &str = "body{background-image:url(foo.png)}";
    // The CSS should minify but the URL shouldn't change.
    fixture.validate_rewrite_external_css(
        "compress_preserve_css_images",
        CSS,
        CSS_AFTER,
        EXPECT_SUCCESS,
    );

    // We should not find a cache hit when requesting the image, indicating it
    // has not been optimized, in contrast with the CssImagePreserveUrls test:
    // serving reconstructs (one rewrite executed).
    fixture.server.http_cache().clear_stats();
    fixture.server.variable(IMAGE_REWRITES).clear();
    let out_img_url = encode(TEST_DOMAIN, "ic", "0", "foo.png", "png");
    let (out_img, _) = fixture.serve_url(&out_img_url).expect("serve img");
    assert!(!out_img.is_empty());
    assert_eq!(1, fixture.stat(IMAGE_REWRITES));
}

// Fallback rewriter tests

#[test]
fn cache_extends_images_simple_fallback() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);

    // Note: Extra }s cause parse failure.
    const CSS_TEMPLATE: &str = "body {\n  background-image: url(%s);\n}}}}}\n";
    let css_before = CSS_TEMPLATE.replace("%s", "foo.png");
    let css_after = CSS_TEMPLATE.replace("%s", &encode("", "ce", "0", "foo.png", "png"));

    fixture.validate_rewrite("unparseable", &css_before, &css_after, EXPECT_FALLBACK);
}

#[test]
fn cache_extends_repeated_top_level_fallback() {
    // Test to make sure that if we cache extend inside CSS we can do it
    // for the same image in HTML at the same time.
    const IMG: &str = "img.png";
    let extended_img = encode("", "ce", "0", "img.png", "png");

    const CSS: &str = "stylesheet.css";
    let rewritten_css = encode("", "cf", "0", "stylesheet.css", "css");
    // Note: Extra }s cause parse failure.
    const CSS_TEMPLATE: &str = "body{background-image:url(%s)}}}}}";

    let fixture = Fixture::new();
    fixture.set_response_with_default_headers(IMG, "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers(
        CSS,
        "text/css",
        CSS_TEMPLATE.replace("%s", IMG).as_bytes(),
        100,
    );

    let html_in = format!("<link rel='stylesheet' href='{CSS}'><img src='{IMG}'>");
    let html_out =
        format!("<link rel='stylesheet' href='{rewritten_css}'><img src='{extended_img}'>");
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}repeated_top_level"), &html_in),
        html_out
    );

    let (css_out, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}{rewritten_css}"))
        .expect("serve css");
    assert_eq!(
        css_out,
        CSS_TEMPLATE.replace("%s", &extended_img).as_bytes()
    );
}

#[test]
fn cache_extends_images_fallback() {
    let fixture = Fixture::new();
    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("bar.png", "image/png", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("baz.png", "image/png", DUMMY_CONTENT, 100);

    const CSS_TEMPLATE: &str = "body {\n  background-image: url(%s);\n  list-style-image: url('%s');\n}\n.titlebar p.cfoo, #end p {\n  background: url(\"%s\");\n  list-style: url('%s');\n}\n.other {\n  background-image:url(data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAUAAAAFCAYAAACNbyblAAAAHElEQVQI12P4//8/w38GIAXDIBKE0DHxgljNBAAO9TXL0Y4OHwAAAABJRU5ErkJggg==);  -proprietary-background-property: url(%s);\n}}}}}}";
    // Note: Extra }s cause parse failure (appended above).
    let css_before = CSS_TEMPLATE
        .replacen("%s", "foo.png", 1)
        .replacen("%s", "bar.png", 1)
        .replacen("%s", "baz.png", 1)
        .replacen("%s", "foo.png", 1)
        .replacen("%s", "foo.png", 1);
    let css_after = CSS_TEMPLATE
        .replacen("%s", &encode("", "ce", "0", "foo.png", "png"), 1)
        .replacen("%s", &encode("", "ce", "0", "bar.png", "png"), 1)
        .replacen("%s", &encode("", "ce", "0", "baz.png", "png"), 1)
        .replacen("%s", &encode("", "ce", "0", "foo.png", "png"), 1)
        .replacen("%s", &encode("", "ce", "0", "foo.png", "png"), 1);

    fixture.validate_rewrite(
        "cache_extends_images",
        &css_before,
        &css_after,
        EXPECT_FALLBACK,
    );
}

#[test]
fn recompress_images_fallback() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    // Note: Extra }s cause parse failure.
    const CSS_TEMPLATE: &str = "body {\n  background-image: url(%s);\n}}}}}\n";
    let css_before = CSS_TEMPLATE.replace("%s", "foo.png");
    let css_after = CSS_TEMPLATE.replace("%s", &encode("", "ic", "0", "foo.png", "png"));

    fixture.validate_rewrite_external_css(
        "recompress_css_images",
        &css_before,
        &css_after,
        EXPECT_FALLBACK,
    );
}

// Make sure we don't break import URLs or other non-image URLs.
#[test]
fn fallback_imports_and_unknown_content_type() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);

    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}image.png"),
        BIKE_PNG,
        "image/png",
        100,
    );
    fixture.set_response_with_default_headers("style.css", "text/css", DUMMY_CONTENT, 100);
    fixture.set_response_with_default_headers("zero.css", "text/css", DUMMY_CONTENT, 0);
    fixture.set_response_with_default_headers("doc.html", "text/html", DUMMY_CONTENT, 100);

    fixture.set_response_with_default_headers(
        "behavior.htc",
        "text/x-component",
        DUMMY_CONTENT,
        100,
    );
    fixture.set_response_with_default_headers(
        "font.ttf",
        "application/octet-stream",
        DUMMY_CONTENT,
        100,
    );
    fixture.set_response_with_default_headers(
        "font.eot",
        "application/vnd.ms-fontobject",
        DUMMY_CONTENT,
        100,
    );

    const CSS_TEMPLATE: &str = "@import '%s';@import url(zero.css);@font-face {\n  font-family: name;\n  src: url('font.ttf'), url(font.eot);\n}\nbody {\n  background-image: url(%s);\n  behavior: url(behavior.htc);\n  -moz-content-file: url(doc.html);\n}}}}}\n";
    let css_before = CSS_TEMPLATE
        .replacen("%s", "style.css", 1)
        .replacen("%s", "image.png", 1);
    let css_after = CSS_TEMPLATE
        .replacen("%s", &encode("", "ce", "0", "style.css", "css"), 1)
        .replacen("%s", &encode("", "ic", "0", "image.png", "png"), 1);

    fixture.validate_rewrite_external_css(
        "recompress_css_images",
        &css_before,
        &css_after,
        EXPECT_FALLBACK,
    );
}

// Test that the fallback fetcher fails smoothly.
#[test]
fn fallback_fails() {
    // Note: //// is not a valid URL leading to fallback rewrite failure.
    const BAD_CSS: &str = ".foo { url(////); }}}}}}}";
    let fixture = Fixture::new();
    fixture.validate_rewrite("fallback_fails", BAD_CSS, BAD_CSS, EXPECT_FAILURE);
}

// Check that we absolutify URLs when moving CSS.
#[test]
fn fallback_absolutify() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://new_domain.com", TEST_DOMAIN);
    // Turn off trimming to make sure we can see full absolutifications.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::LeftTrimUrls);

    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 0);

    // Note: Extra }s cause parse failure.
    const CSS_TEMPLATE: &str = ".foo { background: url(%s); }}}}";
    let css_before = CSS_TEMPLATE.replace("%s", "foo.png");
    let css_after = CSS_TEMPLATE.replace("%s", "http://new_domain.com/foo.png");

    // We only test inline CSS because ValidateRewriteExternalCss doesn't work
    // with AddRewriteDomainMapping.
    fixture.validate_rewrite_inline_css("change_domain", &css_before, &css_after, EXPECT_FALLBACK);

    // Test loading from other domains.
    fixture.set_response_with_default_headers(
        "other_domain.css",
        "text/css",
        css_before.as_bytes(),
        100,
    );

    const REWRITTEN_URL: &str = "http://test.com/I.other_domain.css.pagespeed.cf.0.css";
    fixture.serve_resource_from_many_contexts(REWRITTEN_URL, css_after.as_bytes());
}

// Check that we don't absolutify URLs when not moving them.
#[test]
fn fallback_no_absolutify() {
    let fixture = Fixture::new();
    // Turn off trimming to make sure we can see full absolutifications.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::LeftTrimUrls);

    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 0);

    // Note: Extra }s cause parse failure.
    const CSS: &str = ".foo { background: url(foo.png); }}}}";

    fixture.validate_rewrite("change_domain", CSS, CSS, EXPECT_FALLBACK);
}

#[test]
fn fetch_rewrite_failure() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://new_domain.com", TEST_DOMAIN);
    // Turn off trimming to make sure we can see full absolutifications.
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::LeftTrimUrls);
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::FallbackRewriteCssUrls);

    fixture.set_response_with_default_headers("foo.png", "image/png", DUMMY_CONTENT, 0);

    // Note: Extra }s cause parse failure.
    const CSS_TEMPLATE: &str = ".foo { background: url(%s); }}}}";
    let css_before = CSS_TEMPLATE.replace("%s", "foo.png");
    let css_after = CSS_TEMPLATE.replace("%s", "http://new_domain.com/foo.png");

    // Test loading from other domains.
    fixture.set_response_with_default_headers(
        "other_domain.css",
        "text/css",
        css_before.as_bytes(),
        100,
    );

    let content = fixture.fetch_resource(TEST_DOMAIN, "cf", "other_domain.css", "css");
    assert_eq!(css_after.as_bytes(), content.as_slice());
    assert_eq!(0, fixture.stat(FALLBACK_REWRITES));
    assert_eq!(1, fixture.stat(PARSE_FAILURES));

    // Check that this still works correctly the second time (this loads the
    // result from cache and so goes through a different code path).
    let content = fixture.fetch_resource(TEST_DOMAIN, "cf", "other_domain.css", "css");
    assert_eq!(css_after.as_bytes(), content.as_slice());
}

// A fragment-only reference stays as written when a stylesheet that does
// not parse has its references rewritten onto a rewrite domain, with and
// without fallback URL rewriting.
#[test]
fn fragment_reference_kept_when_parse_fails() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://new_domain.com", TEST_DOMAIN);
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::LeftTrimUrls);

    // Note: Extra }s cause parse failure.
    const CSS: &str = ".m { mask: url(#m); } .n { mask: url('#n'); }}}}";
    fixture.set_response_with_default_headers("frag.css", "text/css", CSS.as_bytes(), 100);
    let content = fixture.fetch_resource(TEST_DOMAIN, "cf", "frag.css", "css");
    assert_eq!(CSS.as_bytes(), content.as_slice());
    assert_eq!(1, fixture.stat(FALLBACK_REWRITES));
}

#[test]
fn fragment_reference_kept_when_parse_fails_no_fallback() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://new_domain.com", TEST_DOMAIN);
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::LeftTrimUrls);
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::FallbackRewriteCssUrls);

    // Note: Extra }s cause parse failure.
    const CSS: &str = ".m { mask: url(#m); } .n { mask: url('#n'); }}}}";
    fixture.set_response_with_default_headers("frag.css", "text/css", CSS.as_bytes(), 100);
    let content = fixture.fetch_resource(TEST_DOMAIN, "cf", "frag.css", "css");
    assert_eq!(CSS.as_bytes(), content.as_slice());
    assert_eq!(0, fixture.stat(FALLBACK_REWRITES));
}

#[test]
fn inline_images_in_fallback_mode() {
    let fixture = inline_fixture();
    let num_bytes = test_image_file_contents().len() as i64;
    set_max_bytes(&fixture, num_bytes + 1, num_bytes + 1);
    // This ought to not parse.
    const CSS_TEMPLATE: &str = "body {\n  background-image: url(%s);\n}}}}}\n";
    let input_css = CSS_TEMPLATE.replace("%s", CUPPA_PNG);
    let expected_css = CSS_TEMPLATE.replace("%s", &test_image_data_url());

    // Skip the stat check because inlining *increases* the CSS size and
    // causes the check to fail. Inlining eliminates a resource fetch, so
    // it should normally be a net win in practice.
    fixture.validate_rewrite(
        "inline_images_in_fallback_mode",
        &input_css,
        &expected_css,
        EXPECT_FALLBACK | NO_STAT_CHECK,
    );
}

/// Bytes taken by the HTML comments in `html` (`DebugCommentBytes`).
fn debug_comment_bytes(html: &str) -> i64 {
    let mut bytes = 0;
    let mut pos = 0;
    while let Some(start) = html[pos..].find("<!--") {
        let start = pos + start;
        let Some(end) = html[start..].find("-->") else {
            break;
        };
        let end = start + end;
        bytes += (end + 3 - start) as i64;
        pos = end + 3;
    }
    bytes
}

/// Counts substring occurrences (`CountSubstring`).
fn count_substring(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

// Check that we still absolutify URLs even if we fail to parse CSS while
// rewriting on a fetch. This can come up if you have different rewrite
// options on the HTML and resources-serving servers or if the resource
// changes between the HTML and resource servers (race condition during push).
#[test]
fn image_debug_messages_stay_within_budget() {
    const BUDGET: i64 = 600;
    const IMAGES: usize = 40;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(20000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_debug_output_max_bytes(BUDGET);
    let mut css = String::new();
    for i in 0..IMAGES {
        let name = format!("img{i}.png");
        fixture.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{name}"),
            BIKE_PNG,
            "image/png",
            100,
        );
        css.push_str(&format!("a{i}{{background-image:url({name})}}\n"));
    }
    let html = format!("<style>{css}</style>");
    const INLINE_MSG: &str = "The image was not inlined because it has too many bytes.";

    let output = fixture.parse(&format!("{TEST_DOMAIN}budget.html"), &html);
    assert!(BUDGET + 100 >= debug_comment_bytes(&output), "{output}");

    // With no budget, the same verdict for many images is kept once.
    fixture.options.borrow_mut().set_debug_output_max_bytes(0);
    let output = fixture.parse(&format!("{TEST_DOMAIN}budget.html"), &html);
    assert_eq!(1, count_substring(&output, INLINE_MSG));
}

// With debug off, nothing about the images is kept with the stylesheet. This
// guards behaviour that already holds; it does not fail without a change.
#[test]
fn no_image_debug_messages_stored_with_debug_off() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(20000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    const HTML: &str = "<style>body{background-image:url(foo.png)}</style>";
    let output = fixture.parse(&format!("{TEST_DOMAIN}off.html"), HTML);
    assert_eq!(0, count_substring(&output, "<!--"));

    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    let output = fixture.parse(&format!("{TEST_DOMAIN}off.html"), HTML);
    assert_eq!(0, count_substring(&output, "not inlined"));
    assert_eq!(0, count_substring(&output, "does not appear to need"));
}

#[test]
fn debug_message() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(20000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}foo.png"), BIKE_PNG, "image/png", 100);
    const CSS: &str = "body {\n  background-image: url(foo.png);\n}\n";

    let css_after = format!(
        "body{{background-image:url({})}}",
        encode("", "ic", "0", "foo.png", "png")
    );

    fixture.set_debug_message(
        "<!--The image was not inlined because it has too many bytes.-->\
         <!--Image http://test.com/foo.png does not appear to need resizing.-->\
         <!--Image http://test.com/foo.png has no transparent pixels, is not sensitive to compression noise, and has no animation.-->");
    fixture.validate_rewrite_inline_css(
        "recompress_css_images",
        CSS,
        &css_after,
        NO_STAT_CHECK | EXPECT_CACHED,
    );
}

/// Extracts the single background-image URL (`ExtractCssBackgroundImage`).
fn extract_css_background_image(css: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(css);
    let Some(start) = text.find("url(") else {
        return Vec::new();
    };
    let rest = &text[start + 4..];
    let Some(end) = rest.find(')') else {
        return Vec::new();
    };
    rest[..end].as_bytes().to_vec()
}

/// Collects stylesheet hrefs (`CollectCssLinks`).
fn collect_css_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<link") {
        rest = &rest[start..];
        let end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
        let tag = &rest[..end];
        if let Some(href_start) = tag.find("href=") {
            let value = &tag[href_start + 5..];
            let href = if let Some(quote) = value.chars().next().filter(|c| *c == '\'' || *c == '"')
            {
                let value = &value[1..];
                value.find(quote).map(|end| &value[..end]).unwrap_or("")
            } else {
                let end = value
                    .find(|c: char| c == '>' || c.is_whitespace())
                    .unwrap_or(value.len());
                &value[..end]
            };
            out.push(href.to_string());
        }
        rest = &rest[end..];
    }
    out
}

const IPHONE_UA: &str = "Apple iPhone OS v2.1.1 CoreMedia v1.0.0.5F138";
const CHROME15_UA: &str = "Mozilla/5.0 (Windows; U; Windows NT 6.1; en-US) AppleWebKit/534.13 (KHTML, like Gecko) Chrome/15.0.597.19 Safari/534.13";

/// Rewrites CSS for a UA and reads back the embedded JPEG quality
/// (`RewriteCssImageCheckForQuality`).
fn rewrite_css_image_check_for_quality(fixture: &Fixture, user_agent: &str) -> i32 {
    const CSS_FILE: &str = "a.css";
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}{PUZZLE_JPG}"),
        PUZZLE_JPG,
        "image/jpeg",
        100,
    );
    let in_css = format!("div{{background-image:url({PUZZLE_JPG})}}");
    fixture.set_response_with_default_headers(CSS_FILE, "text/css", in_css.as_bytes(), 100);
    fixture.set_current_user_agent(user_agent);

    // Using a "0" hash would result in the rewritten url having the same hash
    // for mobile and non-mobile UAs. Hence, using Md5 hasher.
    let output = fixture.parse(
        &format!("{TEST_DOMAIN}image_in_css"),
        &format!("<link rel='stylesheet' href='{CSS_FILE}'>"),
    );
    let css_links = collect_css_links(&output);
    assert_eq!(1, css_links.len());
    let css_url = format!("{TEST_DOMAIN}{}", css_links[0]);
    let (out_css, _) = fixture.serve_url(&css_url).expect("serve css");

    // Find the image URL embedded in the CSS output.
    let image_url = extract_css_background_image(&out_css);
    if image_url.is_empty() {
        return -1;
    }
    let image_url = format!("{TEST_DOMAIN}{}", String::from_utf8_lossy(&image_url));

    let (out_image, _) = fixture.serve_url(&image_url).expect("serve img");
    ps_image::jpeg::estimate_quality(&out_image).map_or(-1, i32::from)
}

#[test]
fn rewrite_css_images_verify_quality() {
    let fixture = Fixture::with_md5();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteCss);
    // NOTE: image_max_rewrites_at_once waits for the load controller.
    fixture.options.borrow_mut().set_always_rewrite_css(true);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(60);

    // Recompress quality set for desktop is 85 and mobile is 60. Verify this
    // applied correctly by verifying the quality of the output image.
    assert_eq!(60, rewrite_css_image_check_for_quality(&fixture, IPHONE_UA));
    assert_eq!(
        85,
        rewrite_css_image_check_for_quality(&fixture, CHROME15_UA)
    );
}
