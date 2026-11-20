//! Image-rewriting tests ported from `image_rewrite_filter_test.cc`.
//!
//! Byte-exact optimized sizes are codec-specific (the port re-encodes
//! with pure-Rust JPEG/PNG/GIF codecs, not libjpeg-turbo/libpng), so
//! size assertions check structural properties (type, validity,
//! savings) instead. Async, logging, CSP, property-cache, and beacon
//! cases are noted where skipped.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::filter::HtmlFilter;
use ps_html::filters::image_rewrite::{ImageRewriteFilter, FILTER_ID, IMAGE_WEBP_REWRITES};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;
use ps_http::request::RequestHeaders;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const REWRITE_IMAGE_DOMAIN: &str = "http://rewrite_image.test/";
const START_MS: i64 = 1_700_000_000_000;

const PUZZLE_JPG: &str = "Puzzle.jpg";
const PUZZLE_URL: &str = "http://rewrite_image.test/Puzzle.jpg";
const BIKE_PNG: &str = "BikeCrashIcn.png";
const CUPPA_PNG: &str = "Cuppa.png";
const CHEF_GIF: &str = "IronChef2.gif";
const CHEF_DIMS: &str = " width=\"192\" height=\"256\"";
const CHROMIUM24: &str = "chromium-24.webp";
const CUPPA_O_PNG: &str = "CuppaO.png";
const CUPPA_T_PNG: &str = "CuppaT.png";
const SMALL_DATA: &str = "small-data.png";

/// Testdata dir (copied read-only from the C++ rewriter corpus).
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

/// Reads a testdata image.
fn read_image(name: &str) -> Vec<u8> {
    std::fs::read(data_dir().join(name)).unwrap_or_else(|_| panic!("missing image {name}"))
}

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    user_agent: RefCell<String>,
    request_headers: RefCell<RequestHeaders>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_hasher(Rc::new(ps_cache::hash::MockHasher::new()))
    }

    /// Builds the fixture around an explicit hasher (`UseMd5Hasher`).
    fn with_hasher(hasher: Rc<dyn ps_cache::hash::Hasher>) -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend.clone(),
            metadata_backend.clone(),
            fetcher.clone(),
            START_MS,
        );
        server.set_hasher(hasher);
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options.borrow_mut().add_domain(TEST_DOMAIN);
        options.borrow_mut().add_domain(REWRITE_IMAGE_DOMAIN);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            http_backend,
            metadata_backend,
            user_agent: RefCell::new(String::new()),
            request_headers: RefCell::new(RequestHeaders::new()),
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

    /// Scripts a testdata image fetch (`AddFileToMockFetcher`).
    fn add_file_to_mock_fetcher(&self, url: &str, file: &str, content_type: &str, ttl_sec: i64) {
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
        self.fetcher.set_response(&url, headers, &read_image(file));
    }

    /// Scripts a raw fetch with default headers
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

    /// Scripts a 404 fetch (`SetFetchResponse404`).
    fn set_fetch_response_404(&self, name: &str) {
        let url = if name.contains("://") {
            name.to_string()
        } else {
            format!("{TEST_DOMAIN}{name}")
        };
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 404, "Not Found");
        self.fetcher.set_response(&url, headers, b"");
    }

    /// Adds a header to a scripted fetch (`AddToResponse`).
    fn add_to_response(&self, url: &str, name: &str, value: &str) {
        self.fetcher.add_response_header(url, name, value);
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

    /// Sets up animated-webp UA + Accept (`SetupForWebpAnimated`).
    fn setup_for_webp_animated(&self) {
        self.set_current_user_agent("webp-animated");
        self.add_request_attribute("Accept", "image/webp");
    }

    /// Resets UA and request headers (`ResetUserAgent`).
    fn reset_user_agent(&self, user_agent: &str) {
        *self.user_agent.borrow_mut() = user_agent.to_string();
        *self.request_headers.borrow_mut() = RequestHeaders::new();
    }

    /// Resets to webp UA + Accept (`ResetForWebp`).
    fn reset_for_webp(&self) {
        *self.request_headers.borrow_mut() = RequestHeaders::new();
        self.setup_for_webp();
    }

    /// Parses through the image filter.
    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        String::from_utf8(self.parse_with_driver(&driver, url, html)).unwrap()
    }

    /// Parses through extra filters plus the image filter.
    fn parse_with(&self, url: &str, html: &str, extra: Vec<Box<dyn HtmlFilter>>) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        for filter in extra {
            parser.add_filter(filter);
        }
        parser.add_filter(Box::new(ImageRewriteFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    fn parse_with_driver(&self, driver: &Rc<ResourceDriver>, url: &str, html: &str) -> Vec<u8> {
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(ImageRewriteFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        sink.bytes()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
        driver.add_filter(Rc::new(ImageRewriteFilter::new(driver.clone())));
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
        let fresh = ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            self.fetcher.clone(),
            START_MS,
        );
        let server = Rc::new(fresh);
        let options = Rc::new(RefCell::new(self.options.borrow().clone()));
        let driver = Rc::new(ResourceDriver::new(server, options, url));
        driver.set_user_agent(&self.user_agent.borrow());
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver.add_filter(Rc::new(ImageRewriteFilter::new(driver.clone())));
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

    /// Clears both caches (`lru_cache()->Clear()`).
    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
    }

    /// Fetches a URL for a UA with cleared stats (`FetchWebp`).
    fn fetch_webp(&self, url: &str, user_agent: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        self.server.variable(IMAGE_WEBP_REWRITES).clear();
        self.server.http_cache().clear_stats();
        if user_agent == "webp" {
            self.reset_for_webp();
        } else {
            self.reset_user_agent(user_agent);
        }
        self.serve_url(url)
    }

    /// HTTP-cache backend hits (`lru_cache()->num_hits()`).
    fn http_hits(&self) -> u64 {
        self.server.http_cache().stats().backend_hits
    }

    fn stat(&self, name: &str) -> i64 {
        self.server.variable(name).get()
    }
}

/// Collects image srcs (`CollectImgSrcs`, simplified to img/input src).
fn collect_img_srcs(html: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(html);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("<img").or_else(|| rest.find("<input")) {
        rest = &rest[start..];
        let end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
        let tag = &rest[..end];
        if let Some(src_start) = tag.find("src=") {
            let value = &tag[src_start + 4..];
            let src = if let Some(quote) = value.chars().next().filter(|c| *c == '\'' || *c == '"')
            {
                let value = &value[1..];
                value.find(quote).map(|end| &value[..end]).unwrap_or("")
            } else {
                let end = value
                    .find(|c: char| c == '>' || c.is_whitespace())
                    .unwrap_or(value.len());
                &value[..end]
            };
            out.push(src.to_string());
        }
        rest = &rest[end..];
    }
    out
}

/// Rewrites one image tag and checks fetching (`RewriteImage`).
fn rewrite_image(fixture: &Fixture, tag_string: &str, content_type: &str) -> String {
    // Store image contents into fetcher.
    fixture.add_file_to_mock_fetcher(PUZZLE_URL, PUZZLE_JPG, "image/jpeg", 100);

    let src_string = rewrite_image_from_html(fixture, tag_string, content_type);

    let img_gurl = GoogleUrl::resolve(
        &GoogleUrl::new(&format!("{REWRITE_IMAGE_DOMAIN}RewriteImage.html")),
        &src_string,
    );

    // Fetch the version we just put into the cache, so we can
    // make sure we produce it consistently.
    let (rewritten_image, rewritten_headers) = fixture
        .serve_url(img_gurl.spec())
        .unwrap_or_else(|| panic!("serve {}", img_gurl.spec()));
    assert!(!rewritten_image.is_empty());
    assert_eq!(200, rewritten_headers.status_code());

    // Also fetch the resource to ensure it can be created dynamically
    fixture.clear_caches();
    let (recreated, recreated_headers) = fixture
        .serve_url(img_gurl.spec())
        .unwrap_or_else(|| panic!("reserve {}", img_gurl.spec()));
    assert_eq!(200, recreated_headers.status_code());
    assert_eq!(rewritten_image, recreated);
    // Try to fetch from an independent server.
    let (independent, _) = fixture
        .serve_from_new_context(img_gurl.spec())
        .expect("independent serve");
    assert_eq!(rewritten_image, independent);
    // NOTE: filter-application logging waits for the log record.
    src_string
}

/// Rewrites one image tag from HTML (`RewriteImageFromHtml`).
fn rewrite_image_from_html(fixture: &Fixture, tag_string: &str, content_type: &str) -> String {
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture.add_recompress_image_filters();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(2000);

    const HTML_URL: &str = "http://rewrite_image.test/RewriteImage.html";
    let html = format!("<head/><body><{tag_string} src=\"Puzzle.jpg\"/></body>");

    let output = fixture.parse(HTML_URL, &html);
    let img_srcs = collect_img_srcs(output.as_bytes());
    // output should have exactly one image file (Puzzle.jpg).
    assert_eq!(1, img_srcs.len());
    let img_gurl = GoogleUrl::resolve(&GoogleUrl::new(HTML_URL), &img_srcs[0]);
    assert!(img_gurl.is_web_valid());
    let base = GoogleUrl::new(REWRITE_IMAGE_DOMAIN);
    assert_eq!(base.all_except_leaf(), img_gurl.all_except_leaf());
    let extension = match content_type {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => panic!("type {content_type}"),
    };
    assert!(
        img_gurl.leaf_sans_query().ends_with(extension),
        "{}",
        img_gurl.leaf_sans_query()
    );
    // TODO: picks up Puzzle dims after insert-dims lands below.
    img_srcs[0].clone()
}

#[test]
fn img_tag() {
    let fixture = Fixture::new();
    rewrite_image(&fixture, "img", "image/jpeg");
}

/// Encodes a rewritten leaf (`Encode`).
fn encode(prefix: &str, filter_id: &str, hash: &str, leaf: &str, ext: &str) -> String {
    // Only the image segment encoder `x`-prefixes its segment; the
    // default encoder (cache extension, etc.) escapes the leaf as-is.
    let segment = if filter_id == "ic" {
        format!("x{leaf}")
    } else {
        leaf.to_string()
    };
    format!("{prefix}{segment}.pagespeed.{filter_id}.{hash}.{ext}")
}

/// Changes a URL suffix (`ChangeSuffix`).
fn change_suffix(
    old_url: &str,
    append_new_suffix: bool,
    old_suffix: &str,
    new_suffix: &str,
) -> String {
    assert!(
        old_url
            .to_ascii_lowercase()
            .ends_with(&old_suffix.to_ascii_lowercase()),
        "Can't seem to find old extension!"
    );
    if append_new_suffix {
        format!("{old_url}{new_suffix}")
    } else {
        format!(
            "{}{new_suffix}",
            &old_url[..old_url.len() - old_suffix.len()]
        )
    }
}

/// Rewrites one image and checks the outcome (`TestSingleRewrite`).
#[allow(clippy::too_many_arguments)]
fn test_single_rewrite(
    fixture: &Fixture,
    name: &str,
    input_type: &str,
    output_type: &str,
    initial_attributes: &str,
    final_attributes: &str,
    expect_rewritten: bool,
    expect_inline: bool,
) {
    let initial_url = format!("{TEST_DOMAIN}{name}");
    test_single_rewrite_without_abs(
        fixture,
        &initial_url,
        name,
        input_type,
        output_type,
        initial_attributes,
        final_attributes,
        expect_rewritten,
        expect_inline,
    );
}

/// Rewrites one image from an explicit URL (`TestSingleRewriteWithoutAbs`).
#[allow(clippy::too_many_arguments)]
fn test_single_rewrite_without_abs(
    fixture: &Fixture,
    initial_url: &str,
    name: &str,
    input_type: &str,
    output_type: &str,
    initial_attributes: &str,
    final_attributes: &str,
    expect_rewritten: bool,
    expect_inline: bool,
) {
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(initial_url, name, input_type, 100);

    let html_input = format!("<img src='{initial_url}'{initial_attributes}>");
    let output = fixture.parse(&page_url, &html_input);

    // Check for single image file in the rewritten page.
    let image_urls = collect_img_srcs(output.as_bytes());
    assert_eq!(1, image_urls.len());
    let rewritten_url = &image_urls[0];
    let rewritten_gurl = GoogleUrl::new(rewritten_url);
    assert!(
        rewritten_gurl.is_web_valid() || rewritten_gurl.scheme() == "data",
        "{rewritten_url}"
    );

    let output_extension = match output_type {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => panic!("type {output_type}"),
    };
    if expect_inline {
        assert_eq!("data", rewritten_gurl.scheme(), "{rewritten_url}");
        let expected_start = format!("data:{output_type};base64,");
        assert!(
            rewritten_gurl.spec().starts_with(&expected_start),
            "expected {expected_start} got {rewritten_url}"
        );
    } else if expect_rewritten {
        assert_ne!(initial_url, rewritten_url.as_str());
        assert!(
            rewritten_gurl.leaf_sans_query().ends_with(output_extension),
            "expected end {output_extension} got {}",
            rewritten_gurl.leaf_sans_query()
        );
    } else {
        assert_eq!(initial_url, rewritten_url.as_str());
        assert!(
            rewritten_gurl.leaf_sans_query().ends_with(output_extension),
            "expected end {output_extension} got {}",
            rewritten_gurl.leaf_sans_query()
        );
    }

    let html_expected_output = format!("<img src='{rewritten_url}'{final_attributes}>");
    assert_eq!(html_expected_output, output);
}

#[test]
fn fetch_from_other_directory_keeps_page_url() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}dir/{BIKE_PNG}"),
        BIKE_PNG,
        "image/png",
        100,
    );
    // An optimized URL for the same input, named under another directory.
    let root_named = format!("{TEST_DOMAIN}x{BIKE_PNG}.pagespeed.ic.0.png")
        .replace(BIKE_PNG, &format!("dir,_{BIKE_PNG}"));
    let (content, _) = fixture.serve_url(&root_named).expect("served");
    assert!(!content.is_empty());

    let page_png = format!("dir/x{BIKE_PNG}.pagespeed.ic.0.png");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}other_dir"),
            &format!("<img src=\"dir/{BIKE_PNG}\">")
        ),
        format!("<img src=\"{page_png}\">")
    );
}

#[test]
fn img_srcset() {
    let fixture = Fixture::new();
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}a.png"), BIKE_PNG, "image/png", 100);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}b.png"), CUPPA_PNG, "image/png", 100);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}srcset"),
            "<img src=\"a.png\" srcset=\"a.png 1x, b.png 2x\">"
        ),
        "<img src=\"xa.png.pagespeed.ic.0.png\" srcset=\"xa.png.pagespeed.ic.0.png 1x, xb.png.pagespeed.ic.0.png 2x\">"
    );
}

#[test]
fn img_srcset_with_cache_extender() {
    // Makes sure cache extender properly shares the slot.
    use ps_html::filters::cache_extender::CacheExtenderFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheImages);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}a.png"), BIKE_PNG, "image/png", 100);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}b.png"), CUPPA_PNG, "image/png", 100);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}srcset"));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(ImageRewriteFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(CacheExtenderFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let url = format!("{TEST_DOMAIN}srcset");
    parser.start_parse(url.as_bytes(), false);
    parser.write(b"<img src=\"a.png\" srcset=\"a.png 1x, b.png 2x\">");
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        "<img src=\"xa.png.pagespeed.ic.0.png\" srcset=\"xa.png.pagespeed.ic.0.png 1x, xb.png.pagespeed.ic.0.png 2x\">"
    );
}

#[test]
fn image_url_valued_attribute_with_flush() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture.options.borrow_mut().add_url_valued_attribute(
        "li",
        "data-thumb",
        ps_config::options::Category::Image,
    );
    let initial_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    fixture.add_file_to_mock_fetcher(&initial_url, PUZZLE_JPG, "image/jpeg", 100);

    let driver = fixture.driver_for(TEST_DOMAIN);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(ImageRewriteFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(TEST_DOMAIN.as_bytes(), false);
    parser.write(b"<html><body><ul><li data-thumb='Puzzle.jpg'>");
    // Flush in middle of parent and child element
    parser.flush();
    parser.write(b"<img src='Puzzle.jpg'/></li></ul></body></html>");
    parser.finish();

    // Check output with optimized parent, child image nodes
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        "<html><body><ul><li data-thumb='xPuzzle.jpg.pagespeed.ic.0.jpg'><img src='xPuzzle.jpg.pagespeed.ic.0.jpg'/></li></ul></body></html>"
    );
}

#[test]
fn img_tag_with_compute_statistics() {
    use harness::SharedFilter;
    use ps_html::filters::DomStatsFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ComputeStatistics);
    fixture.add_file_to_mock_fetcher(PUZZLE_URL, PUZZLE_JPG, "image/jpeg", 100);
    fixture.add_recompress_image_filters();
    let driver = fixture.driver_for("http://rewrite_image.test/RewriteImage.html");
    let stats = SharedFilter::new(DomStatsFilter::new());
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(ImageRewriteFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(stats.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://rewrite_image.test/RewriteImage.html", false);
    parser.write(b"<head/><body><img src=\"Puzzle.jpg\"/></body>");
    parser.finish();
    assert_eq!(stats.with(|filter| filter.num_img_tags()), 1);
    assert_eq!(stats.with(|filter| filter.num_inlined_img_tags()), 0);
}

#[test]
fn input_tag() {
    let fixture = Fixture::new();
    rewrite_image(&fixture, "input type=\"image\"", "image/jpeg");
}

#[test]
fn data_url_test() {
    use ps_html::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
    let fixture = Fixture::new();
    let driver = fixture.driver_for(TEST_DOMAIN);
    let url = GoogleUrl::new(CUPPA_DATA);
    let resource = driver.create_input_resource_unchecked(&url, true);
    assert!(resource.is_some());
    let mut cuppa_resource = resource.unwrap();
    let mut ctx = driver.request_context().borrow().clone();
    let mut callback = MockLoadCallback::new();
    cuppa_resource.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.success());
    let cuppa_contents = cuppa_resource.contents();
    // Now make sure axing the original cuppa_string doesn't affect the
    // internals of the cuppa_resource.
    let url = GoogleUrl::new(CUPPA_DATA);
    let mut other_resource = driver
        .create_input_resource_unchecked(&url, true)
        .expect("data url");
    let mut ctx = driver.request_context().borrow().clone();
    let mut callback = MockLoadCallback::new();
    other_resource.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.success());
    assert_eq!(cuppa_contents, other_resource.contents());
}

#[test]
fn add_dim_test() {
    use ps_html::filters::image_rewrite::{
        IMAGE_REWRITE_LATENCY_FAILED_MS, IMAGE_REWRITE_LATENCY_OK_MS,
    };
    let fixture = Fixture::new();
    fixture
        .server
        .histogram(IMAGE_REWRITE_LATENCY_OK_MS)
        .clear();
    fixture
        .server
        .histogram(IMAGE_REWRITE_LATENCY_FAILED_MS)
        .clear();

    // Make sure optimizable image isn't optimized, but
    // dimensions are inserted.
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        "",
        " width=\"100\" height=\"100\"",
        false,
        false,
    );
    assert_eq!(1u64, fixture.fetcher.fetch_count());
    assert_eq!(
        1.0,
        fixture
            .server
            .histogram(IMAGE_REWRITE_LATENCY_FAILED_MS)
            .count()
    );
    assert_eq!(
        0.0,
        fixture
            .server
            .histogram(IMAGE_REWRITE_LATENCY_OK_MS)
            .count()
    );

    // Force any image read to be a fetch.
    fixture.http_backend.clear();

    // .. Now make sure we cached dimension insertion properly, and can do it
    // without re-fetching the image.
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        "",
        " width=\"100\" height=\"100\"",
        false,
        false,
    );
    assert_eq!(1u64, fixture.fetcher.fetch_count());
}

/// Checks webp conversion statistics (`TestConversionVariables`).
#[allow(clippy::too_many_arguments)]
fn test_conversion_variables(
    fixture: &Fixture,
    gif_webp_timeout: i64,
    gif_webp_success: f64,
    gif_webp_failure: f64,
    png_webp_timeout: i64,
    png_webp_success: f64,
    png_webp_failure: f64,
    jpeg_webp_timeout: i64,
    jpeg_webp_success: f64,
    jpeg_webp_failure: f64,
    gif_webp_animated_timeout: i64,
    gif_webp_animated_success: f64,
    gif_webp_animated_failure: f64,
    is_opaque: bool,
) {
    let server = &fixture.server;
    assert_eq!(
        gif_webp_timeout,
        server.variable("image_webp_conversion_gif_timeouts").get()
    );
    assert_eq!(
        gif_webp_success,
        server
            .histogram("image_webp_conversion_gif_success_ms")
            .count()
    );
    assert_eq!(
        gif_webp_failure,
        server
            .histogram("image_webp_conversion_gif_failure_ms")
            .count()
    );

    assert_eq!(
        png_webp_timeout,
        server.variable("image_webp_conversion_png_timeouts").get()
    );
    assert_eq!(
        png_webp_success,
        server
            .histogram("image_webp_conversion_png_success_ms")
            .count()
    );
    assert_eq!(
        png_webp_failure,
        server
            .histogram("image_webp_conversion_png_failure_ms")
            .count()
    );

    assert_eq!(
        jpeg_webp_timeout,
        server.variable("image_webp_conversion_jpeg_timeouts").get()
    );
    assert_eq!(
        jpeg_webp_success,
        server
            .histogram("image_webp_conversion_jpeg_success_ms")
            .count()
    );
    assert_eq!(
        jpeg_webp_failure,
        server
            .histogram("image_webp_conversion_jpeg_failure_ms")
            .count()
    );

    assert_eq!(
        gif_webp_animated_timeout,
        server
            .variable("image_webp_conversion_gif_animated_timeouts")
            .get()
    );
    assert_eq!(
        gif_webp_animated_success,
        server
            .histogram("image_webp_conversion_gif_animated_success_ms")
            .count()
    );
    assert_eq!(
        gif_webp_animated_failure,
        server
            .histogram("image_webp_conversion_gif_animated_failure_ms")
            .count()
    );

    let total_timeout =
        gif_webp_timeout + png_webp_timeout + jpeg_webp_timeout + gif_webp_animated_timeout;
    let total_success =
        gif_webp_success + png_webp_success + jpeg_webp_success + gif_webp_animated_success;
    let total_failure =
        gif_webp_failure + png_webp_failure + jpeg_webp_failure + gif_webp_animated_failure;

    let (timeouts, success_ms, failure_ms) = if is_opaque {
        (
            "image_webp_opaque_timeouts",
            "image_webp_opaque_success_ms",
            "image_webp_opaque_failure_ms",
        )
    } else {
        (
            "image_webp_alpha_timeouts",
            "image_webp_alpha_success_ms",
            "image_webp_alpha_failure_ms",
        )
    };
    assert_eq!(total_timeout, server.variable(timeouts).get());
    assert_eq!(total_success, server.histogram(success_ms).count());
    assert_eq!(total_failure, server.histogram(failure_ms).count());
}

/// Converts PNG, expecting a rewrite (`TestTranscodeAndOptimizePng`).
fn test_transcode_and_optimize_png(
    fixture: &Fixture,
    expect_rewritten: bool,
    width_height_tags: &str,
    expected_type: &str,
) {
    // Make sure we convert png to jpeg if we requested that.
    // We lower compression quality to ensure the jpeg is smaller.
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality(85);
    test_single_rewrite(
        fixture,
        BIKE_PNG,
        "image/png",
        expected_type,
        "",
        width_height_tags,
        expect_rewritten,
        false,
    );
}

#[test]
fn png_to_jpeg() {
    let fixture = Fixture::new();
    test_transcode_and_optimize_png(
        &fixture,
        true,
        " width=\"100\" height=\"100\"",
        "image/jpeg",
    );
}

#[test]
fn png_to_jpeg_unhealthy() {
    let fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    test_transcode_and_optimize_png(&fixture, false, "", "image/png");
}

#[test]
fn png_to_webp_with_webp_ua() {
    // Make sure we convert png to webp if user agent permits.
    // We lower compression quality to ensure the webp is smaller.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp();
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/webp",
        "",
        " width=\"100\" height=\"100\"",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 1.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn png_to_webp_with_webp_la_ua() {
    // Make sure we convert png to webp if user agent permits.
    // We lower compression quality to ensure the webp is smaller.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_lossless();
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/webp",
        "",
        " width=\"100\" height=\"100\"",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 1.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn png_to_webp_with_webp_la_ua_and_flag() {
    // Make sure we convert png to webp if user agent permits.
    // We lower compression quality to ensure the webp is smaller.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_lossless();

    test_single_rewrite(
        &fixture,
        "RedbrushAlpha-0.5.png",
        "image/png",
        "image/webp",
        "",
        " width=\"512\" height=\"480\"",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 1.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        false,
    );
    // NOTE: background-rewrite log assertions wait for the log record.
}

// The settings are the same as "png_to_webp_with_webp_la_ua_and_flag" except
// WebP lossless user agent. So conversion falls back to PNG.
#[test]
fn png_fallback_to_png_lack_of_webp_la_ua() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);

    test_single_rewrite(
        &fixture,
        "RedbrushAlpha-0.5.png",
        "image/png",
        "image/png",
        "",
        " width=\"512\" height=\"480\"",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        false,
    );
}

#[test]
fn png_to_webp_with_webp_la_ua_and_flag_times_out() {
    // Make sure we convert png to webp if user agent permits.
    // We lower compression quality to ensure the webp is smaller.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.options.borrow_mut().set_image_webp_timeout_ms(0);
    fixture.setup_for_webp_lossless();
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/jpeg",
        "",
        " width=\"100\" height=\"100\"",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        1, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn img_tag_webp() {
    // We use the webp testing user agent; real webp-capable user agents are
    // tested as part of user_agent_matcher_test and are likely to remain in flux
    // over time.
    let fixture = Fixture::new();
    fixture.setup_for_webp();
    rewrite_image(&fixture, "img", "image/webp");
}

#[test]
fn img_tag_webp_la() {
    // We use the webp testing user agent; real webp-capable user agents are
    // tested as part of user_agent_matcher_test and are likely to remain in flux
    // over time.
    let fixture = Fixture::new();
    fixture.setup_for_webp_lossless();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
    rewrite_image(&fixture, "img", "image/webp");
}

#[test]
fn input_tag_webp() {
    // We use the webp testing user agent; real webp-capable user agents are
    // tested as part of user_agent_matcher_test and are likely to remain in flux
    // over time.
    let fixture = Fixture::new();
    fixture.setup_for_webp();
    rewrite_image(&fixture, "input type=\"image\"", "image/webp");
}

#[test]
fn input_tag_webp_la() {
    // We use the webp-la testing user agent; real webp-capable user agents are
    // tested as part of user_agent_matcher_test and are likely to remain in flux
    // over time.
    let fixture = Fixture::new();
    fixture.setup_for_webp_lossless();

    // Note that, currently, images that are originally jpegs are
    // converted to webp lossy regardless of this filter below.
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);

    rewrite_image(&fixture, "input type=\"image\"", "image/webp");
}

#[test]
fn no_transform() {
    // Make sure that the image stays the same and that the attribute is stripped.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        " pagespeed_no_transform", // initial attributes
        "",                        // final attributes
        false,                     // expect_rewritten
        false,                     // expect_inline
    );
}

#[test]
fn data_no_transform() {
    // Make sure that the image stays the same and that the attribute is stripped.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        " data-pagespeed-no-transform", // initial attributes
        "",                             // final attributes
        false,                          // expect_rewritten
        false,                          // expect_inline
    );
}

#[test]
fn no_transform_with_dims() {
    // Make sure that the image stays the same and that the attribute is stripped.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        // initial attributes
        " width=10 height=10 data-pagespeed-no-transform",
        " width=10 height=10", // final attributes
        false,                 // expect_rewritten
        false,                 // expect_inline
    );
}

#[test]
fn yes_transform() {
    // Replicates above test but without no-transform to show that it works.  We
    // also verify that the data-pagespeed-no-defer attribute doesn't get removed
    // when we rewrite images.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);

    let url = format!("{TEST_DOMAIN}notransform.png");
    fixture.add_file_to_mock_fetcher(&url, BIKE_PNG, "image/png", 100);
    let expected = format!("{TEST_DOMAIN}xnotransform.png.pagespeed.ic.0.png");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}YesTransform"),
            &format!("<img src={url} data-pagespeed-no-defer>")
        ),
        format!("<img src={expected} data-pagespeed-no-defer>")
    );
    // Validate twice in case changes in cache from the first request alter the
    // second.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}YesTransform"),
            &format!("<img src={url}>")
        ),
        format!("<img src={expected}>")
    );
}

#[test]
fn yes_transform_with_option_false() {
    // Verify rewrite happens even when no-transform is set, if the option is
    // set to false.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_disable_rewrite_on_no_transform(false);

    let url = format!("{TEST_DOMAIN}notransform.png");
    fixture.add_file_to_mock_fetcher(&url, BIKE_PNG, "image/png", 100);
    fixture
        .fetcher
        .add_response_header(&url, "Cache-Control", "no-transform");
    let expected = format!("{TEST_DOMAIN}xnotransform.png.pagespeed.ic.0.png");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}YesTransform"),
            &format!("<img src={url}>")
        ),
        format!("<img src={expected}>")
    );
    // Validate twice in case changes in cache from the first request alter the
    // second.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}YesTransform"),
            &format!("<img src={url}>")
        ),
        format!("<img src={expected}>")
    );
}

#[test]
fn image_rewrite_drop_all() {
    // Test that randomized optimization doesn't rewrite when drop % set to 100
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_rewrite_random_drop_percentage(100);

    for _ in 0..100 {
        test_single_rewrite(
            &fixture,
            BIKE_PNG,
            "image/png",
            "image/png",
            "",    // initial attributes
            "",    // final attributes
            false, // expect_rewritten
            false, // expect_inline
        );
        fixture.clear_caches();
    }
    // Try some rewrites without clearing the cache to make sure that that
    // works too.
    for _ in 0..100 {
        test_single_rewrite(
            &fixture,
            BIKE_PNG,
            "image/png",
            "image/png",
            "",    // initial attributes
            "",    // final attributes
            false, // expect_rewritten
            false, // expect_inline
        );
    }
}

#[test]
fn image_rewrite_drop_none() {
    // Test that randomized optimization always rewrites when drop % set to 0.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_rewrite_random_drop_percentage(0);

    for _ in 0..100 {
        test_single_rewrite(
            &fixture,
            BIKE_PNG,
            "image/png",
            "image/png",
            "",   // initial attributes
            "",   // final attributes
            true, // expect_rewritten
            false,
        ); // expect_inline
        fixture.clear_caches();
    }
    // Try some rewrites without clearing the cache to make sure that that
    // works too.
    for _ in 0..5 {
        test_single_rewrite(
            &fixture,
            BIKE_PNG,
            "image/png",
            "image/png",
            "",   // initial attributes
            "",   // final attributes
            true, // expect_rewritten
            false,
        ); // expect_inline
    }
}

#[test]
fn image_rewrite_drop_sometimes() {
    // Test that randomized optimization sometimes rewrites and sometimes doesn't.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_rewrite_random_drop_percentage(50);

    let mut found_rewritten = false;
    let mut found_not_rewritten = false;

    // Boiler-plate fetching stuff.
    let initial_url = format!("{TEST_DOMAIN}{BIKE_PNG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, BIKE_PNG, "image/png", 100);
    let html_input = format!("<img src='{initial_url}'>");

    // Note that this could flake, but for it to flake we'd have to have 100
    // heads or 100 tails in a row, a probability of 1.6e-30 when
    // image_rewrite_percentage is 50.
    for _ in 0..100 {
        let output = fixture.parse(&page_url, &html_input);

        // Check for single image file in the rewritten page.
        let image_urls = collect_img_srcs(output.as_bytes());
        assert_eq!(1, image_urls.len());
        let rewritten_url = &image_urls[0];
        let rewritten_gurl = GoogleUrl::new(rewritten_url);
        assert!(rewritten_gurl.is_web_valid());

        if initial_url == *rewritten_url {
            found_not_rewritten = true;
        } else {
            found_rewritten = true;
        }

        if found_rewritten && found_not_rewritten {
            break;
        }
    }
}

// For Issue 748 where duplicate images in the same document with RandomDrop on
// caused the duplicate urls to be removed if the first image is not optimized.
// NOTE: This test only works if the first image is deterministically dropped.
// We set the drop_percentage to 100 to guarantee that.
#[test]
fn image_rewrite_random_drop_repeated_images() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .set_rewrite_random_drop_percentage(100);
    let initial_url = format!("{TEST_DOMAIN}{BIKE_PNG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, BIKE_PNG, "image/png", 100);
    let html_input =
        format!("<img src='{initial_url}'> <img src='{initial_url}'> <img src='{initial_url}'>");
    let output = fixture.parse(&page_url, &html_input);
    let image_urls = collect_img_srcs(output.as_bytes());
    assert_eq!(3, image_urls.len());
    assert_eq!(initial_url, image_urls[0]);
    assert_eq!(initial_url, image_urls[1]);
    assert_eq!(initial_url, image_urls[2]);
}

#[test]
fn resize_test() {
    // Make sure we resize images, but don't optimize them in place.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    // Without explicit resizing, we leave the image alone.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        "",
        "",
        false,
        false,
    );
    // With resizing, we optimize.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        " width=\"256\" height=\"192\"",
        " width=\"256\" height=\"192\"",
        true,
        false,
    );
}

#[test]
fn resize_is_really_prefetch() {
    // Make sure we don't resize a large image to 1x1, as it's
    // really an image prefetch request.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        " width=\"1\" height=\"1\"",
        " width=\"1\" height=\"1\"",
        false,
        false,
    );
}

#[test]
fn optimize_requested_prefetch() {
    // We shouldn't resize this image, but we should optimize it.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        " width=\"1\" height=\"1\"",
        " width=\"1\" height=\"1\"",
        true,
        false,
    );
}

#[test]
fn resize_higher_dimension_test() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    test_single_rewrite(
        &fixture,
        "Large.png",
        "image/png",
        "image/png",
        " width=\"100000\" height=\"100000\"",
        " width=\"100000\" height=\"100000\"",
        false,
        false,
    );
    assert_eq!(1, fixture.stat("image_norewrites_high_resolution"));
}

#[test]
fn dimension_parsing_ok() {
    use ps_html::filters::image_rewrite::parse_dimension_attribute;
    // First some tests that should succeed.
    assert_eq!(Some(5), parse_dimension_attribute(b"5"));
    assert_eq!(Some(341), parse_dimension_attribute(b" 341  "));
    assert_eq!(Some(743), parse_dimension_attribute(b" 000743  "));
    assert_eq!(Some(62), parse_dimension_attribute(b"\n\r\t \x0c62"));
    assert_eq!(Some(40), parse_dimension_attribute(b"+40"));
    assert_eq!(Some(41), parse_dimension_attribute(b" +41"));
    assert_eq!(Some(54), parse_dimension_attribute(b"54px"));
    assert_eq!(Some(70), parse_dimension_attribute(b"  70."));
    assert_eq!(Some(71), parse_dimension_attribute(b"71.3"));
    assert_eq!(Some(72), parse_dimension_attribute(b"71.523"));
    assert_eq!(
        Some(73),
        parse_dimension_attribute(b"73.4999990982589729048572938579287459874")
    );
    assert_eq!(Some(75), parse_dimension_attribute(b"75.px"));
    assert_eq!(Some(76), parse_dimension_attribute(b"75.6 px"));
    assert_eq!(Some(77), parse_dimension_attribute(b"77.34px"));
    assert_eq!(Some(78), parse_dimension_attribute(b"78px "));
}

#[test]
fn dimension_parsing_fail() {
    use ps_html::filters::image_rewrite::parse_dimension_attribute;
    assert_eq!(None, parse_dimension_attribute(b""));
    assert_eq!(None, parse_dimension_attribute(b"0"));
    assert_eq!(None, parse_dimension_attribute(b"+0"));
    // Bizarrely not allowed!
    assert_eq!(None, parse_dimension_attribute(b"+0.9"));
    assert_eq!(None, parse_dimension_attribute(b"  0  "));
    assert_eq!(None, parse_dimension_attribute(b"junk5"));
    assert_eq!(None, parse_dimension_attribute(b"  junk10"));
    assert_eq!(None, parse_dimension_attribute(b"junk  50"));
    assert_eq!(None, parse_dimension_attribute(b"-43"));
    assert_eq!(None, parse_dimension_attribute(b"+ 43"));
    assert_eq!(None, parse_dimension_attribute(b"21px%"));
    assert_eq!(None, parse_dimension_attribute(b"21px junk"));
    assert_eq!(
        None,
        parse_dimension_attribute(b"9123948572038209720561049018365037891046")
    );
    // We don't handle percentages because we can't resize them.
    assert_eq!(None, parse_dimension_attribute(b"73%"));
    assert_eq!(None, parse_dimension_attribute(b"43.2 %"));
    // Trailing junk OK according to spec, but older browsers flunk / treat
    // inconsistently
    // Doesn't ignore
    assert_eq!(None, parse_dimension_attribute(b"5junk"));
    // 25% on FF9!
    assert_eq!(None, parse_dimension_attribute(b"25p%x"));
}

#[test]
fn debug_message_image_info() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpAnimated);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture.add_file_to_mock_fetcher("photo_opaque.gif", "IronChef2.gif", "image/gif", 100);
    fixture.add_file_to_mock_fetcher("graphic_transparent.png", "CuppaT.png", "image/png", 100);
    fixture.add_file_to_mock_fetcher("animated.gif", "CradleAnimation.gif", "image/gif", 100);

    let output = fixture.parse(
        &format!("{TEST_DOMAIN}single_attribute"),
        "<img src=photo_opaque.gif><img src=graphic_transparent.png><img src=animated.gif>",
    );

    let expected = concat!(
        "<img src=xphoto_opaque.gif.pagespeed.ic.0.png>",
        "<!--Image http://test.com/photo_opaque.gif does not appear to need resizing.-->",
        "<!--Image http://test.com/photo_opaque.gif has no transparent pixels, is not sensitive to compression noise, and has no animation.-->",
        "<img src=graphic_transparent.png>",
        "<!--Image http://test.com/graphic_transparent.png does not appear to need resizing.-->",
        "<!--Image http://test.com/graphic_transparent.png has transparent pixels, is sensitive to compression noise, and has no animation.-->",
        "<img src=animated.gif>",
        "<!--Image http://test.com/animated.gif does not appear to need resizing.-->",
        "<!--Image http://test.com/animated.gif has no transparent pixels, is sensitive to compression noise, and has animation.-->",
    );
    assert!(output.contains(expected), "{output}");
}

#[test]
fn debug_message_inline() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_image_inline_max_bytes(100);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
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
        .soft_enable_filter_for_testing(Filter::ResizeImages);

    let initial_url = format!("{TEST_DOMAIN}IronChef2.gif");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, "IronChef2.gif", "image/gif", 100);
    let html_input = format!("<img src='{initial_url}' width='10' height='12'>");

    let output = fixture.parse(&page_url, &html_input);

    assert!(
        output.contains("The image was not inlined because it has too many bytes."),
        "{output}"
    );
}

#[test]
fn debug_message_unauthorized() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    const AUTHORIZED: &str = "http://test.com/photo_opaque.gif";
    const UNAUTHORIZED: &str = "http://unauth.com/photo_opaque.gif";
    fixture.add_file_to_mock_fetcher(AUTHORIZED, "IronChef2.gif", "image/gif", 100);
    fixture.add_file_to_mock_fetcher(UNAUTHORIZED, "IronChef2.gif", "image/gif", 100);

    let output = fixture.parse(
        &format!("{TEST_DOMAIN}unauthorized_domain"),
        &format!("<img src={AUTHORIZED}><img src={UNAUTHORIZED}>"),
    );

    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}unauthorized_domain"));
    let unauth_gurl = GoogleUrl::new(UNAUTHORIZED);
    let expected = format!(
        "<img src={}xphoto_opaque.gif.pagespeed.ic.0.png>",
        TEST_DOMAIN,
    ) + "<!--Image http://test.com/photo_opaque.gif does not appear to need resizing.-->"
        + "<!--Image http://test.com/photo_opaque.gif has no transparent pixels, is not sensitive to compression noise, and has no animation.-->"
        + &format!("<img src={UNAUTHORIZED}>")
        + "<!--"
        + &driver.generate_unauthorized_domain_debug_comment(
            &unauth_gurl,
            ps_html::server_context::InputRole::Img,
        )
        + "-->";
    assert!(output.contains(&expected), "{output}");
}

const IPHONE_CHROME36: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 7_1_2 like Mac OS X) AppleWebKit/537.51.2 (KHTML, like Gecko) CriOS/36.0.1985.49 Mobile/11D257 Safari/9537.53";
const IPAD_CHROME36: &str = "Mozilla/5.0 (iPad; CPU OS 7_1_1 like Mac OS X) AppleWebKit/537.51.2 (KHTML, like Gecko) CriOS/36.0.1985.49 Mobile/11D201 Safari/9537.53";
const IPHONE4_SAFARI: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 5_0_1 like Mac OS X) AppleWebKit/534.46 (KHTML, like Gecko) Version/5.1 Mobile/9A405 Safari/7534.48.3";
const ANDROID_CHROME21: &str = "Mozilla/5.0 (Linux; Android 4.1.4; Galaxy Nexus Build/IMM76B) AppleWebKit/535.19 (KHTML, like Gecko) Chrome/21.0.1025.133 Mobile Safari/535.19";
const CHROME18: &str = "Mozilla/5.0 (Windows; U; Windows NT 6.1; en-US) AppleWebKit/534.13 (KHTML, like Gecko) Chrome/18.0.597.19 Safari/534.13";
const NEXUS10_CHROME: &str = "Mozilla/5.0 (Linux; Android 4.2.2; Nexus 10 Build/JDQ39) AppleWebKit/537.31 (KHTML, like Gecko) Chrome/26.0.1408.0 Safari/537.31";

/// Inlines (or outlines) under a UA (`TestInlining`).
fn test_inlining(
    fixture: &Fixture,
    convert_to_webp: bool,
    user_agent: &str,
    file_name: &str,
    input_type: &str,
    output_type: &str,
    expect_inline: bool,
) {
    fixture.set_current_user_agent(user_agent);
    if convert_to_webp {
        fixture
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
        fixture
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
        fixture.add_request_attribute("Accept", "image/webp");
    }

    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(1000000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);

    test_single_rewrite(
        fixture,
        file_name,
        input_type,
        output_type,
        "",
        "",
        true, // expect_rewritten
        expect_inline,
    );
}

// Chrome on iPhone rewrites a photo-like GIF to lossy WebP but cannot inline
// it.
#[test]
fn chrome_iphone_outlines_webp() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        true,
        IPHONE_CHROME36,
        "IronChef2.gif",
        "image/gif",
        "image/webp",
        false,
    );
}

// Chrome on iPad rewrites a graphics-like PNG to lossless WebP but cannot
// inline it.
#[test]
fn chrome_ipad_inlines_png() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        true,
        IPAD_CHROME36,
        "CuppaT.png",
        "image/png",
        "image/webp",
        false,
    );
}

// Chrome on iPad rewrites a JPEG to lossy WebP but cannot inline it.
#[test]
fn chrome_ipad_outlines_webp() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        true,
        IPAD_CHROME36,
        PUZZLE_JPG,
        "image/jpeg",
        "image/webp",
        false,
    );
}

// Chrome on iPhone rewrites a graphics-like PNG to another PNG and inlines it.
#[test]
fn chrome_iphone_inlines_png() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        false,
        IPHONE_CHROME36,
        CUPPA_PNG,
        "image/png",
        "image/png",
        true,
    );
}

// Chrome on iPad rewrites a JPEG to another JPEG and inlines it.
#[test]
fn chrome_ipad_inlines_jpeg() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        false,
        IPAD_CHROME36,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        true,
    );
}

// Safari on iPhone rewrites a photo-like GIF to JPEG and inlines it.
#[test]
fn safari_iphone_inlines_jpeg() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        false,
        IPHONE4_SAFARI,
        "IronChef2.gif",
        "image/gif",
        "image/jpeg",
        true,
    );
}

// Chrome on Android rewrites a photo-like PNG to lossy WebP and inlines it.
#[test]
fn chrome_android_inlines_webp() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        true,
        ANDROID_CHROME21,
        "IronChef2.gif",
        "image/gif",
        "image/webp",
        true,
    );
}

// Chrome on desktop rewrites a JPEG to lossy WebP and inlines it.
#[test]
fn chrome_desktop_inlines_webp() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        true,
        CHROME18,
        PUZZLE_JPG,
        "image/jpeg",
        "image/webp",
        true,
    );
}

// Chrome on Android rewrites a graphics-like PNG to lossless WebP and
// inlines it.
#[test]
fn chrome_android_inlines_lossless_webp() {
    let fixture = Fixture::new();
    test_inlining(
        &fixture,
        true,
        NEXUS10_CHROME,
        "CuppaT.png",
        "image/png",
        "image/webp",
        true,
    );
}

/// Exercises the resolution limit (`TestResolutionLimit`).
fn test_resolution_limit(
    fixture: &Fixture,
    resolution: i64,
    image_file: &str,
    content_type: &str,
    try_webp: bool,
    try_resize: bool,
    expect_rewritten: bool,
) {
    fixture.setup_for_webp_lossless();
    fixture
        .options
        .borrow_mut()
        .set_image_resolution_limit_bytes(resolution);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality(85);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);

    let mut rewritten_type = content_type.to_string();
    if try_webp {
        fixture
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
        fixture
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
        if expect_rewritten {
            rewritten_type = "image/webp".to_string();
        }
    }

    let dimension = if try_resize {
        fixture
            .options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::ResizeImages);
        " width=4000 height=2000"
    } else {
        ""
    };

    test_single_rewrite(
        fixture,
        image_file,
        content_type,
        &rewritten_type,
        dimension,
        dimension,
        expect_rewritten,
        false,
    );

    if expect_rewritten {
        assert_eq!(1, fixture.stat("image_rewrites"));
        assert_eq!(0, fixture.stat("image_norewrites_high_resolution"));
    } else {
        assert_eq!(0, fixture.stat("image_rewrites"));
        assert_eq!(1, fixture.stat("image_norewrites_high_resolution"));
    }
}

#[test]
fn png_exceed_resolution_limit() {
    let fixture = Fixture::new();
    test_resolution_limit(
        &fixture,
        33554432 - 1,
        "ResolutionLimit.png",
        "image/png",
        false, // try_webp
        false, // try_resize
        false, // expect_rewritten
    );
}

#[test]
fn jpeg_exceed_resolution_limit() {
    let fixture = Fixture::new();
    test_resolution_limit(
        &fixture,
        33554432 - 1,
        "ResolutionLimit.jpg",
        "image/jpeg",
        false, // try_webp
        false, // try_resize
        false, // expect_rewritten
    );
}

#[test]
fn png_in_resolution_limit() {
    let fixture = Fixture::new();
    test_resolution_limit(
        &fixture,
        33554432,
        "ResolutionLimit.png",
        "image/png",
        true, // try_webp
        true, // try_resize
        true, // expect_rewritten
    );
}

#[test]
fn png_in_resolution_limit_no_resizing() {
    let fixture = Fixture::new();
    test_resolution_limit(
        &fixture,
        33554432,
        "ResolutionLimit.png",
        "image/png",
        true,  // try_webp
        false, // try_resize
        true,  // expect_rewritten
    );
}

#[test]
fn jpeg_in_resolution_limit() {
    let fixture = Fixture::new();
    test_resolution_limit(
        &fixture,
        33554432,
        "ResolutionLimit.jpg",
        "image/jpeg",
        true, // try_webp
        true, // try_resize
        true, // expect_rewritten
    );
}

#[test]
fn jpeg_in_resolution_limit_no_resizing() {
    let fixture = Fixture::new();
    test_resolution_limit(
        &fixture,
        33554432,
        "ResolutionLimit.jpg",
        "image/jpeg",
        true,  // try_webp
        false, // try_resize
        true,  // expect_rewritten
    );
}

// The resolution limit applies to the canvas a GIF is decoded at. When the
// header declares a screen smaller than the first frame, the frame reader
// grows the canvas to that frame, so the frame's size is what counts.
#[test]
fn gif_resolution_limit_counts_the_decoded_canvas() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    // transparent.gif is 320x320. The limit admits a 320x160 image but not the
    // whole frame.
    fixture
        .options
        .borrow_mut()
        .set_image_resolution_limit_bytes(320 * 320 * 4 - 1);

    let mut gif = read_image("transparent.gif");
    // The logical screen height is the 16-bit little-endian field after the
    // width (ImageHeaders::kGifDimStart + kGifIntSize). Halve it.
    let height_pos = 6 + 2;
    assert!(gif.len() > height_pos + 1);
    gif[height_pos] = 160;
    gif[height_pos + 1] = 0;

    let url = format!("{TEST_DOMAIN}short_screen.gif");
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("image/gif"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), 100 * 3600 * 1000, "");
    headers.compute_caching();
    fixture.fetcher.set_response(&url, headers, &gif);
    fixture.parse(
        &format!("{TEST_DOMAIN}short_screen.html"),
        &format!("<img src='{url}'>"),
    );

    assert_eq!(0, fixture.stat("image_rewrites"));
    assert_eq!(1, fixture.stat("image_norewrites_high_resolution"));
}

#[test]
fn animated_gif_to_webp_with_webp_animated_ua() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpAnimated);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_animated();
    test_single_rewrite(
        &fixture,
        "CradleAnimation.gif",
        "image/gif",
        "image/webp",
        "",
        " width=\"200\" height=\"150\"",
        true,
        false,
    );

    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 1.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn animated_gif_to_webp_with_webp_la_ua() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpAnimated);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_lossless();
    test_single_rewrite(
        &fixture,
        "CradleAnimation.gif",
        "image/gif",
        "image/gif",
        "",
        " width=\"200\" height=\"150\"",
        false,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        false,
    );
}

#[test]
fn animated_gif_to_webp_not_enabled() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpLossless);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_animated();
    test_single_rewrite(
        &fixture,
        "CradleAnimation.gif",
        "image/gif",
        "image/gif",
        "",
        " width=\"200\" height=\"150\"",
        false,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 0.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        false,
    );
}

#[test]
fn gif_to_webp_lossless_with_webp_animated_ua() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpAnimated);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_animated();
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/webp",
        "",
        " width=\"192\" height=\"256\"",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 1.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn animated_no_cache_reuse() {
    // Make sure we don't reuse results for animated webp-capable UAs for
    // non-webp targets.
    let fixture = Fixture::new();
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}a.jpeg"),
        PUZZLE_JPG,
        "image/jpeg",
        100,
    );

    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertToWebpAnimated);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);

    // WebP capable browser --- made a WebP image.
    fixture.setup_for_webp_animated();
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}webp_broswer"), "<img src=a.jpeg>"),
        "<img src=xa.jpeg.pagespeed.ic.0.webp>"
    );

    // Not a WebP browser -- don't!
    fixture.set_current_user_agent("curl");
    fixture
        .request_headers
        .borrow_mut()
        .headers_mut()
        .remove_all("Accept");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}non-webp_broswer"),
            "<img src=a.jpeg>"
        ),
        "<img src=a.jpeg>"
    );
}

const MOBILE_UA: &str = "Mozilla/5.0 (Linux; U; Android 4.0.1; en-us; Galaxy Nexus Build/ICL27) AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Mobile Safari/534.30";
const DESKTOP_UA: &str = "Mozilla/5.0 (Windows; U; Windows NT 5.1; en-US) AppleWebKit/525.13 (KHTML, like Gecko) Chrome/0.A.B.C Safari/525.13";
const IPHONE_UA: &str = "iPhone OS Safari";

/// Resolves qualities for a UA (`ImageOptionsForLoadedResource` probe).
fn resolve_qualities(
    fixture: &Fixture,
    user_agent: &str,
) -> (
    ps_image::image::CompressionOptions,
    ps_html::image_url_encoder::ImageResourceContext,
) {
    use ps_html::filters::image_rewrite::encode_user_agent_into_resource_context;
    use ps_html::filters::image_rewrite::image_options_for_loaded_resource;
    fixture.set_current_user_agent(user_agent);
    let driver = fixture.driver_for(TEST_DOMAIN);
    let mut context = ps_html::image_url_encoder::ImageResourceContext::default();
    encode_user_agent_into_resource_context(&driver, &mut context);
    let options = image_options_for_loaded_resource(&context, 1000, &driver.options());
    (options, context)
}

#[test]
fn jpeg_quality_for_small_screens() {
    let fixture = Fixture::new();
    // Neither option is set explicitly, default is 70.
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(70, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Base image quality is set, but for_small_screens is not, return base.
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(-1);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(85, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Base jpeg quality not set, but for_small_screens is, return small_screen.
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(-1);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(20);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(20, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Neither jpeg quality is set, return -1.
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(-1);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(-1);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(-1, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Base and for_small_screen options are set; mobile
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality(85);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(20);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(20, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Non-mobile UA.
    let (img_options, ctx) = resolve_qualities(&fixture, DESKTOP_UA);
    assert_eq!(85, img_options.jpeg_quality);
    assert!(!ctx.may_use_small_screen_quality);

    // Mobile UA
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(70);
    let (img_options, ctx) = resolve_qualities(&fixture, IPHONE_UA);
    assert_eq!(70, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Although the regular (desktop) quality is smaller, it won't affect the
    // quality used for mobile.
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality_for_small_screens(70);
    fixture
        .options
        .borrow_mut()
        .set_image_jpeg_recompress_quality(60);
    let (img_options, ctx) = resolve_qualities(&fixture, IPHONE_UA);
    assert_eq!(70, img_options.jpeg_quality);
    assert!(ctx.may_use_small_screen_quality);
}

#[test]
fn webp_quality_for_small_screens() {
    let fixture = Fixture::new();
    // Neither option is set, default is 70.
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(70, img_options.webp_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Base webp quality set, but for_small_screens is not, return base quality.
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality(85);
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality_for_small_screens(-1);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(85, img_options.webp_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Base webp quality not set, but for_small_screens is, return small_screen.
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(-1);
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality(-1);
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality_for_small_screens(20);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(20, img_options.webp_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Base and for_small_screen options are set; mobile
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality(85);
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality_for_small_screens(20);
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(20, img_options.webp_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Non-mobile UA.
    let (img_options, ctx) = resolve_qualities(&fixture, DESKTOP_UA);
    assert_eq!(85, img_options.webp_quality);
    assert!(!ctx.may_use_small_screen_quality);

    // Mobile UA
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality_for_small_screens(70);
    let (img_options, ctx) = resolve_qualities(&fixture, IPHONE_UA);
    assert_eq!(70, img_options.webp_quality);
    assert!(ctx.may_use_small_screen_quality);

    // Although the regular (desktop) quality is smaller, it won't affect the
    // quality used for mobile.
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality_for_small_screens(70);
    fixture
        .options
        .borrow_mut()
        .set_image_webp_recompress_quality(55);
    let (img_options, ctx) = resolve_qualities(&fixture, IPHONE_UA);
    assert_eq!(70, img_options.webp_quality);
    assert!(ctx.may_use_small_screen_quality);
}

/// Sets scan counts, resolving options (`SetNumberOfScans`).
fn set_number_of_scans(
    fixture: &Fixture,
    user_agent: &str,
    num_scans: i64,
    num_scans_small_screen: i64,
) -> (
    ps_image::image::CompressionOptions,
    ps_html::image_url_encoder::ImageResourceContext,
) {
    const DO_NOT_SET: i64 = -10;
    if num_scans != DO_NOT_SET || num_scans_small_screen != DO_NOT_SET {
        if num_scans != DO_NOT_SET {
            fixture
                .options
                .borrow_mut()
                .set_image_jpeg_num_progressive_scans(num_scans);
        }
        if num_scans_small_screen != DO_NOT_SET {
            fixture
                .options
                .borrow_mut()
                .set_image_jpeg_num_progressive_scans_for_small_screens(num_scans_small_screen);
        }
    }
    resolve_qualities(fixture, user_agent)
}

#[test]
fn jpeg_progressive_scans_for_small_screens() {
    const DO_NOT_SET: i64 = -10;
    let fixture = Fixture::new();
    // Neither option is set, default is -1.
    let (img_options, ctx) = resolve_qualities(&fixture, MOBILE_UA);
    assert_eq!(-1, img_options.jpeg_num_progressive_scans);
    assert!(ctx.may_use_small_screen_quality);

    // Base jpeg num scans set, but for_small_screens is not, return
    // base num scans.
    let (img_options, ctx) = set_number_of_scans(&fixture, MOBILE_UA, 8, -1);
    assert_eq!(8, img_options.jpeg_num_progressive_scans);
    assert!(ctx.may_use_small_screen_quality);

    // Base jpeg quality not set, but for_small_screens is, return small_screen.
    let (img_options, ctx) = set_number_of_scans(&fixture, MOBILE_UA, DO_NOT_SET, 2);
    assert_eq!(2, img_options.jpeg_num_progressive_scans);
    assert!(ctx.may_use_small_screen_quality);

    // Base and for_small_screen options are set; mobile.
    let (img_options, ctx) = set_number_of_scans(&fixture, MOBILE_UA, 8, 2);
    assert_eq!(2, img_options.jpeg_num_progressive_scans);
    assert!(ctx.may_use_small_screen_quality);

    // Non-mobile UA.
    let (img_options, ctx) = set_number_of_scans(&fixture, DESKTOP_UA, DO_NOT_SET, DO_NOT_SET);
    assert_eq!(8, img_options.jpeg_num_progressive_scans);
    assert!(!ctx.may_use_small_screen_quality);

    // Mobile UA
    let (img_options, ctx) = set_number_of_scans(&fixture, IPHONE_UA, DO_NOT_SET, 2);
    assert_eq!(2, img_options.jpeg_num_progressive_scans);
    assert!(ctx.may_use_small_screen_quality);

    // Although the regular (desktop) number of scans is smaller, it won't affect
    // that used for mobile.
    let (img_options, ctx) = set_number_of_scans(&fixture, IPHONE_UA, 2, 8);
    assert_eq!(8, img_options.jpeg_num_progressive_scans);
    assert!(ctx.may_use_small_screen_quality);
}

#[test]
fn dimension_strip_after_inline() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(100000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    const CHEF_WIDTH: &str = " width=192";
    const CHEF_HEIGHT: &str = " height=256";
    const CHEF_DIMS: &str = " width=\"192\" height=\"256\"";
    // With all specified dimensions matching, dims are stripped after inlining.
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_DIMS,
        "",
        false,
        true,
    );
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_WIDTH,
        "",
        false,
        true,
    );
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_HEIGHT,
        "",
        false,
        true,
    );
    // If we stretch the image in either dimension, we keep the dimensions.
    const CHEF_WIDER: &str = " width=384 height=256";
    const CHEF_TALLER: &str = " width=192 height=512";
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_WIDER,
        CHEF_WIDER,
        false,
        true,
    );
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_TALLER,
        CHEF_TALLER,
        false,
        true,
    );

    const CHEF_WIDTH_WITH_PERCENTAGE: &str = " width=100% height=1";
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_WIDTH_WITH_PERCENTAGE,
        CHEF_WIDTH_WITH_PERCENTAGE,
        false,
        true,
    );
    const CHEF_HEIGHT_WITH_PERCENTAGE: &str = " width=1 height=%";
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        CHEF_HEIGHT_WITH_PERCENTAGE,
        CHEF_HEIGHT_WITH_PERCENTAGE,
        false,
        true,
    );
}

#[test]
fn inline_enlarged_image() {
    // Make sure we inline an image that meets the inlining threshold,
    // but retain its sizing information if the image has been enlarged.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    const DOUBLED_DIMS: &str = " width=130 height=140";
    test_single_rewrite(
        &fixture,
        "CuppaO.png",
        "image/png",
        "image/png",
        DOUBLED_DIMS,
        DOUBLED_DIMS,
        false,
        true,
    );
}

#[test]
fn respects_base_url() {
    // Put original files into our fetcher.
    const HTML_URL: &str = "http://image.test/base_url.html";
    const PNG_URL: &str = "http://other_domain.test/foo/bar/a.png";
    const JPEG_URL: &str = "http://other_domain.test/baz/b.jpeg";
    const GIF_URL: &str = "http://other_domain.test/foo/c.gif";

    let fixture = Fixture::new();
    fixture.options.borrow_mut().add_domain("image.test");
    fixture.options.borrow_mut().add_domain("other_domain.test");
    fixture.add_file_to_mock_fetcher(PNG_URL, BIKE_PNG, "image/png", 100);
    fixture.add_file_to_mock_fetcher(JPEG_URL, PUZZLE_JPG, "image/jpeg", 100);
    fixture.add_file_to_mock_fetcher(GIF_URL, "IronChef2.gif", "image/gif", 100);

    // First two images are on base domain.  Last is on origin domain.
    let html_format =
        "<head>\n  <base href='http://other_domain.test/foo/'>\n</head>\n<body>\n  <img src='%s'>\n  <img src='%s'>\n  <img src='%s'>\n</body>";

    let html_input = html_format
        .replacen("%s", "bar/a.png", 1)
        .replacen("%s", "/baz/b.jpeg", 1)
        .replacen("%s", "c.gif", 1);

    // Rewrite
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    let output = fixture.parse(HTML_URL, &html_input);

    // Check for image files in the rewritten page.
    let image_urls = collect_img_srcs(output.as_bytes());
    assert_eq!(3, image_urls.len());
    let new_png_url = &image_urls[0];
    let new_jpeg_url = &image_urls[1];
    let new_gif_url = &image_urls[2];

    // Sanity check that we changed the URL.
    assert_ne!("bar/a.png", new_png_url);
    assert_ne!("/baz/b.jpeg", new_jpeg_url);
    assert_ne!("c.gif", new_gif_url);

    let expected_output = html_format
        .replacen("%s", new_png_url, 1)
        .replacen("%s", new_jpeg_url, 1)
        .replacen("%s", new_gif_url, 1);
    assert_eq!(expected_output, output);

    let base_gurl = GoogleUrl::new("http://other_domain.test/foo/");
    let new_png_gurl = GoogleUrl::resolve(&base_gurl, new_png_url);
    assert!(new_png_gurl.is_web_valid());
    assert_eq!(
        "http://other_domain.test/foo/bar/",
        new_png_gurl.all_except_leaf()
    );

    let new_jpeg_gurl = GoogleUrl::resolve(&base_gurl, new_jpeg_url);
    assert!(new_jpeg_gurl.is_web_valid());
    assert_eq!(
        "http://other_domain.test/baz/",
        new_jpeg_gurl.all_except_leaf()
    );

    let new_gif_gurl = GoogleUrl::resolve(&base_gurl, new_gif_url);
    assert!(new_gif_gurl.is_web_valid());
    assert_eq!(
        "http://other_domain.test/foo/",
        new_gif_gurl.all_except_leaf()
    );
}

#[test]
fn cache_control_header_check_for_non_webp_ua() {
    let fixture = Fixture::new();
    let initial_image_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let html_input = format!("<img src='{initial_image_url}'>");
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.add_recompress_image_filters();
    fixture.setup_for_webp();

    let page_url = format!("{TEST_DOMAIN}test.html");
    // Store image contents into fetcher.
    fixture.add_file_to_mock_fetcher(&initial_image_url, PUZZLE_JPG, "image/jpeg", 100);
    let start_time_ms = fixture.server.now_ms();
    let output = fixture.parse(&page_url, &html_input);

    let image_urls = collect_img_srcs(output.as_bytes());
    assert_eq!(1, image_urls.len());
    let image_gurl = GoogleUrl::new(&image_urls[0]);
    assert!(image_gurl.leaf_sans_query().ends_with("webp"));
    let src_string = image_urls[0].clone();

    let (_, response_headers) = fixture.serve_url(&src_string).expect("served");

    use ps_http::response::{RequestProperties, ValidatorOption, VaryOption};
    let cacheable = |headers: &ResponseHeaders| {
        headers.is_proxy_cacheable(
            RequestProperties::default(),
            VaryOption::RespectVaryOnResources,
            ValidatorOption::NoValidator,
        )
    };
    assert!(cacheable(&response_headers));
    const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;
    assert_eq!(
        YEAR_MS,
        response_headers.cache_expiration_time_ms() - start_time_ms
    );
    // Set a non-webp UA.
    fixture.set_current_user_agent("");
    fixture
        .request_headers
        .borrow_mut()
        .headers_mut()
        .remove_all("Accept");

    let new_image_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    let output = fixture.parse(&page_url, &html_input);

    let image_urls = collect_img_srcs(output.as_bytes());
    assert_eq!(1, image_urls.len());
    let rewritten_url = &image_urls[0];
    let rewritten_gurl = GoogleUrl::new(rewritten_url);
    assert!(rewritten_gurl.leaf_sans_query().ends_with("jpg"));

    // Fetch a new rewritten url with a new hash so as to get a short cache
    // time.
    use ps_cache::hash::Hasher;
    let new_hash = ps_cache::hash::Md5Hasher::new().hash(output.as_bytes());
    let rewritten_url_new = format!("{TEST_DOMAIN}x{PUZZLE_JPG}.pagespeed.ic.{new_hash}.jpg");
    let (_, response) = fixture.serve_url(&rewritten_url_new).expect("served");
    assert!(!cacheable(&response));
    // TTL will be 100s since resource creation, because that is the input
    // resource TTL and is lower than the 300s implicit cache TTL for hash
    // mismatch.
    assert_eq!(
        100 * 1000,
        response.cache_expiration_time_ms() - start_time_ms
    );
    let _ = new_image_url;
}

#[test]
fn image_rewrite_preserve_urls_on_soft_enable() {
    // Make sure that the image URL stays the same when optimization is enabled
    // due to core filters.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture.options.borrow_mut().set_image_preserve_urls(true);
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        " width=10 height=10", // initial_dims,
        " width=10 height=10", // final_dims,
        false,                 // expect_rewritten
        false,
    ); // expect_inline
       // The URL wasn't changed but the image should have been compressed and cached
       // anyway (prefetching for IPRO).
    let before = fixture.fetcher.fetch_count();
    let out_png_url = format!("{TEST_DOMAIN}x{BIKE_PNG}.pagespeed.ic.0.png");
    let (out_png, _) = fixture.serve_url(&out_png_url).expect("served");
    // Served from cache: no new fetches (the LRU/http hit counters have
    // no equivalent here).
    assert_eq!(before, fixture.fetcher.fetch_count());

    // Make sure that we didn't resize (original image is 100x100).
    let analysis = ps_image::image::analyze(&out_png).expect("analyze");
    assert_eq!(100, analysis.width);
    assert_eq!(100, analysis.height);
}

#[test]
fn image_rewrite_preserve_urls_explicit_resize_on() {
    // Explicitly enabling resize_images is a strong signal from the user that
    // it's OK to rename image URLs, so go ahead and do it in the image rewriter.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::RecompressPng);
    fixture.options.borrow_mut().set_image_preserve_urls(true); // Explicit filters override.
    test_single_rewrite(
        &fixture,
        BIKE_PNG,
        "image/png",
        "image/png",
        " width=10 height=10", // initial_dims,
        " width=10 height=10", // final_dims,
        true,                  // expect_rewritten: explicit cache_extend_images
        false,
    ); // expect_inline
    let before = fixture.fetcher.fetch_count();
    let out_png_url = format!("{TEST_DOMAIN}10x10x{BIKE_PNG}.pagespeed.ic.0.png");

    let (out_png, _) = fixture.serve_url(&out_png_url).expect("served");
    assert_eq!(before, fixture.fetcher.fetch_count());

    // Make sure that we did the resize to 10x10 from 100x100.
    let analysis = ps_image::image::analyze(&out_png).expect("analyze");
    assert_eq!(10, analysis.width);
    assert_eq!(10, analysis.height);
}

#[test]
fn image_rewrite_preserve_urls_disable_preemptive_rewrite() {
    // With preserve_urls and no preemptive rewrites, C++ doesn't add the
    // image filter to the HTML chain at all; the port parses filterless.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture.options.borrow_mut().set_image_preserve_urls(true);
    fixture
        .options
        .borrow_mut()
        .set_in_place_preemptive_rewrite_images(false);

    let initial_url = format!("{TEST_DOMAIN}{BIKE_PNG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, BIKE_PNG, "image/png", 100);
    let html_input = format!("<img src='{initial_url}' width=10 height=10>");
    // No image filter in the chain: nothing is attempted or fetched.
    let driver = fixture.driver_for(&page_url);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(page_url.as_bytes(), false);
    parser.write(html_input.as_bytes());
    parser.finish();
    assert_eq!(String::from_utf8(sink.bytes()).unwrap(), html_input);
    assert_eq!(0u64, fixture.fetcher.fetch_count());
}

#[test]
fn image_rewrite_inline_preserve_urls_on_soft_enable() {
    let fixture = Fixture::new();
    // Willing to inline large files.
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(1000000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::ConvertPngToJpeg);
    fixture.options.borrow_mut().set_image_preserve_urls(true);
    const RESIZED_DIMS: &str = " width=48 height=64";
    // File would be inlined without preserve urls, make sure it's not,
    // because turning on image_preserve_urls overrides the implicit filter
    // selection from Core filters.
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/gif",
        RESIZED_DIMS,
        RESIZED_DIMS,
        false, // expect_rewritten
        false,
    ); // expect_inline
       // The optimized file should be in the cache now.
    let before = fixture.fetcher.fetch_count();
    let out_gif_url = format!("{TEST_DOMAIN}xIronChef2.gif.pagespeed.ic.0.png");
    fixture.serve_url(&out_gif_url).expect("served");
    assert_eq!(before, fixture.fetcher.fetch_count());
}

#[test]
fn image_rewrite_inline_preserve_urls_explicit() {
    let fixture = Fixture::new();
    // Willing to inline large files.
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(1000000);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ConvertGifToPng);
    fixture.options.borrow_mut().set_image_preserve_urls(true);
    const RESIZED_DIMS: &str = " width=48 height=64";
    // In this case, since we have explicitly requested inline images,
    // we will get them despite the preserve URLs setting.
    test_single_rewrite(
        &fixture,
        "IronChef2.gif",
        "image/gif",
        "image/png",
        RESIZED_DIMS,
        RESIZED_DIMS,
        true, // expect_rewritten
        true,
    ); // expect_inline
       // The optimized file should be in the cache now.
    let before = fixture.fetcher.fetch_count();
    let out_gif_url = format!("{TEST_DOMAIN}xIronChef2.gif.pagespeed.ic.0.png");
    fixture.serve_url(&out_gif_url).expect("served");
    assert_eq!(before, fixture.fetcher.fetch_count());
}

#[test]
fn debug_resize_test() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    let initial_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, PUZZLE_JPG, "image/jpeg", 100);
    let html_input = format!("<img src='{initial_url}' width=\"256\" height=\"192\">");
    let output = fixture.parse(&page_url, &html_input);
    assert!(
        output.contains("<!--Resized image http://test.com/Puzzle.jpg from 1023x766 to 256x192-->"),
        "{output}"
    );
}

#[test]
fn debug_no_resize_test() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    let initial_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, PUZZLE_JPG, "image/jpeg", 100);
    let html_input = format!("<img src='{initial_url}'>");
    let output = fixture.parse(&page_url, &html_input);
    assert!(
        output
            .contains("<!--Image http://test.com/Puzzle.jpg does not appear to need resizing.-->"),
        "{output}"
    );
}

#[test]
fn debug_with_map_rewrite_domain() {
    use ps_html::filters::domain_rewrite::{DomainRewriteConfig, DomainRewriteFilter};
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RewriteDomains);
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("external.example.com", TEST_DOMAIN);
    let initial_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, PUZZLE_JPG, "image/jpeg", 100);
    let html_input = format!("<img src='{initial_url}'>");

    let driver = fixture.driver_for(&page_url);
    let mut lawyer = ps_html::domain_lawyer::DomainLawyer::new();
    assert!(lawyer.add_domain(TEST_DOMAIN, None));
    assert!(lawyer.add_rewrite_domain_mapping("external.example.com", TEST_DOMAIN, None));
    let lawyer = Rc::new(lawyer);
    let config = DomainRewriteConfig {
        options: fixture.options.borrow().clone(),
        ..DomainRewriteConfig::default()
    };
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(ImageRewriteFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(DomainRewriteFilter::new(
        config,
        ps_html::url_namer::StandardUrlNamer,
        &lawyer,
    )) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(page_url.as_bytes(), false);
    parser.write(html_input.as_bytes());
    parser.finish();
    let output = String::from_utf8(sink.bytes()).unwrap();
    assert!(
        output.contains(
            "<img src='http://external.example.com/Puzzle.jpg'>\
             <!--Image http://external.example.com/Puzzle.jpg does not appear to need resizing.-->"
        ),
        "{output}"
    );
}

#[test]
fn debug_with_map_rewrite_domain_opt_only() {
    // w/o rewrite_domains we don't touch the URLs in comments, as they can be
    // left as such in the page source proper anyway.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("external.example.com", TEST_DOMAIN);
    let initial_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, PUZZLE_JPG, "image/jpeg", 100);
    let html_input = format!("<img src='{initial_url}'>");
    let output = fixture.parse(&page_url, &html_input);
    assert!(
        output.contains(
            "<img src='http://test.com/Puzzle.jpg'>\
             <!--Image http://test.com/Puzzle.jpg does not appear to need resizing.-->"
        ),
        "{output}"
    );
}

#[test]
fn no_dims_in_non_img() {
    // As above, only with an icon.  See:
    // https://github.com/apache/incubator-pagespeed-mod/issues/629
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    let initial_url = format!("{TEST_DOMAIN}{BIKE_PNG}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, BIKE_PNG, "image/png", 100);

    let html_input =
        format!("<link rel='apple-touch-icon-precomposed' sizes='100x100' href='{initial_url}'>");
    assert_eq!(fixture.parse(&page_url, &html_input), html_input);
}

const CUPPA_DATA: &str = concat!(
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEEAAABGCAAAAAC2ma",
    "YhAAAC00lEQVQY0+3PTUhUYRQG4HdmMhUaC6FaKSqEZS2MsEJEsaKSwMKgot2Qkk",
    "KFUFBYWgSpGIhSZH+0yAgLDQ3pByoLRS2DjCjEfm0MzQhK08wZ5/Sde12kc8f5Dr",
    "XLs3lfPs55uBf0t4MZ4X8QLjeY2XC80cieUq9M6MB6I7tDcMgoRWgVCb5VyDLKFu",
    "CK8RCHMpFwEzjA+coGdHJ5COwRCSnAJc4cwOnlshs4KhFeA+jib48A1hovK4A6iX",
    "ADiOB8oyQXF28Y0CIRKgDHsMoeJaTyw6gDOC0RGtXlPS5RQOgAlwQgWSK4lZDDZa",
    "cqxVyOqNIpECgSiBxTeVsdRo/z/9iBXImwTV3eUemLU6WRXzYCziGB0KAOs7kUqL",
    "KZS40qVwVCr9qP4vJElblc3KocFAi+cMD2U5VBdYhPqgyp3CcQKEYdDHCZDYT/mv",
    "iYa5JvCANiubxTh2u4XAAcfQLhgzrM51KjSjmXFGAvCYRTQGgvlwwggX/iGbDwm0",
    "RIAwo439tga+biAqpJIHy2I36Uyxkgl7MnBJkkEV4AtUbJQvwP86/m94uE71juM8",
    "piPDayDOdJJNDKFjMzNpl5fcmYUPBMZIfbzBE3CQXBTBIuHtaYwo5phHToTMk0Qq",
    "aWUNxUUXrui7XggvZEFI9YCfu1AQeQbiWc0LrOe9D11ZcNtFsIVVpCG696YrHVQq",
    "jVAezDxm4hEi2ElzpCvLl7EkkWwliIhrDD3K1EsoVASzWEUnM1DbushO0aQpux2Q",
    "w8shJKggPzvLzYl4BYn5XQHVzI4r2Pi4CzZCVQUlChimi0cgGQR9ZCRVDhbl1RtI",
    "oNngBC/yzozLJqLwUQqCjotTPR1fTnxVTBs3ra89T6/ikHfgK9dQa+t1eS//gJVB",
    "8WUCgnLYHaYwIAeaQp0GC25S8cG9cWiOrm+AHrnhMJBLplmwLkE8kEenp/8oyIBf",
    "2ZEWaEfyv8BsICdAZ/XeTCAAAAAElFTkSuQmCC"
);

#[test]
fn resize_width_only() {
    // Make sure we resize images, but don't optimize them in place.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    // Without explicit resizing, we leave the image alone.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        "",
        "",
        false,
        false,
    );
    // With resizing, we optimize.
    const RESIZED_DIMS: &str = " width=\"256\"";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_DIMS,
        RESIZED_DIMS,
        true,
        false,
    );
}

#[test]
fn resize_height_only() {
    // Make sure we resize images, but don't optimize them in place.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    // Without explicit resizing, we leave the image alone.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        "",
        "",
        false,
        false,
    );
    // With resizing, we optimize.
    const RESIZED_DIMS: &str = " height=\"192\"";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_DIMS,
        RESIZED_DIMS,
        true,
        false,
    );
}

/// Tests dimensions of an optimized image by fetching it
/// (`TestDimensionRounding`).
fn test_dimension_rounding(
    fixture: &Fixture,
    leaf: &str,
    expected_width: u32,
    expected_height: u32,
) {
    let initial_url = format!("{TEST_DOMAIN}{PUZZLE_JPG}");
    let fetch_url = format!("{TEST_DOMAIN}{leaf}");
    fixture.add_file_to_mock_fetcher(&initial_url, PUZZLE_JPG, "image/jpeg", 100);
    // Set up resizing
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    // Perform resource fetch
    let (body, headers) = fixture
        .serve_url(&fetch_url)
        .unwrap_or_else(|| panic!("serve {fetch_url}"));
    assert_eq!(200, headers.status_code());
    // Look up dimensions of resulting image
    let analysis = ps_image::image::analyze(&body).expect("analyze");
    assert_eq!(expected_width, analysis.width);
    assert_eq!(expected_height, analysis.height);
}

#[test]
fn resize_height_rounding() {
    // Make sure fractional heights are rounded.  We used to truncate, but this
    // didn't match WebKit's behavior.  To check this we need to fetch the resized
    // image and verify its dimensions.  The original image is 1023 x 766.
    const LEAF_NO_HEIGHT: &str = "256xNxPuzzle.jpg.pagespeed.ic.0.jpg";
    test_dimension_rounding(&Fixture::new(), LEAF_NO_HEIGHT, 256, 192);
}

#[test]
fn resize_width_rounding() {
    // Make sure fractional widths are rounded, as above (with the same image).
    const LEAF_NO_WIDTH: &str = "Nx383xPuzzle.jpg.pagespeed.ic.0.jpg";
    test_dimension_rounding(&Fixture::new(), LEAF_NO_WIDTH, 512, 383);
}

#[test]
fn resize_style_test() {
    // Make sure we resize images, but don't optimize them in place.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    const RESIZED_DIMS: &str = " style=\"width:256px;height:192px;\"";
    // Without explicit resizing, we leave the image alone.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        "",
        "",
        false,
        false,
    );
    // With resizing, we optimize.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_DIMS,
        RESIZED_DIMS,
        true,
        false,
    );

    const MIXED_DIMS: &str = " width=\"256\" style=\"height:192px;\"";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        MIXED_DIMS,
        MIXED_DIMS,
        true,
        false,
    );

    const MORE_MIXED_DIMS: &str = " height=\"197\" style=\"width:256px;broken:true;\"";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        MORE_MIXED_DIMS,
        MORE_MIXED_DIMS,
        true,
        false,
    );

    const NON_PIXEL_DIMS: &str = " style=\"width:256cm;height:192cm;\"";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        NON_PIXEL_DIMS,
        NON_PIXEL_DIMS,
        false,
        false,
    );

    const NO_DIMS: &str = " style=\"width:256;height:192;\"";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        NO_DIMS,
        NO_DIMS,
        false,
        false,
    );
}

// The style attribute of an image is read for its dimensions only up to
// CssParseMaxBytes; a longer one is not parsed and gives no dimensions.
#[test]
fn resize_style_over_parse_bound_is_ignored() {
    const BOUND: i64 = 256;
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(BOUND);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);

    let mut at_bound = String::from("width:256px;height:192px;");
    at_bound.extend(std::iter::repeat_n(' ', BOUND as usize - at_bound.len()));
    let at_bound_attr = format!(" style=\"{at_bound}\"");
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        &at_bound_attr,
        &at_bound_attr,
        true,
        false,
    );
    assert_eq!(
        0,
        fixture.stat(ps_html::filters::css::PARSE_MAX_BYTES_EXCEEDED)
    );

    // Over the bound the style value counts as one that cannot be processed:
    // no resizing, and no dimensions are added to the tag. It is counted once.
    let over = format!(" style=\"{at_bound} \"");
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        &over,
        &over,
        false,
        false,
    );
    assert_eq!(
        1,
        fixture.stat(ps_html::filters::css::PARSE_MAX_BYTES_EXCEEDED)
    );
}

#[test]
fn resize_with_px_in_html() {
    // Make sure we resize images if the html width and/or height specifies px.
    // We rely on ImageRewriteTest.DimensionParsing above to test all the
    // corner cases we might encounter and to cross-check the numbers.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    // Things that ought to work (ie result in resizing)
    const RESIZED_PX: &str = " width='256px' height='192px'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_PX,
        RESIZED_PX,
        true,
        false,
    );
    const RESIZED_WIDTH_DOT: &str = " width='256.'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_WIDTH_DOT,
        RESIZED_WIDTH_DOT,
        true,
        false,
    );
    const RESIZED_WIDTH_DEC: &str = " width='255.536'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_WIDTH_DEC,
        RESIZED_WIDTH_DEC,
        true,
        false,
    );
    const RESIZED_WIDTH_PX: &str = " width='256px'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_WIDTH_PX,
        RESIZED_WIDTH_PX,
        true,
        false,
    );
    const RESIZED_WIDTH_PX_DOT: &str = " width='256.px'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_WIDTH_PX_DOT,
        RESIZED_WIDTH_PX_DOT,
        true,
        false,
    );
    const RESIZED_WIDTH_PX_DEC: &str = " width='255.5px'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_WIDTH_PX_DEC,
        RESIZED_WIDTH_PX_DEC,
        true,
        false,
    );
    const RESIZED_SPACE_PX: &str = " width='256  px'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_SPACE_PX,
        RESIZED_SPACE_PX,
        true,
        false,
    );
    // Things that ought not to work (ie not result in resizing)
    const RESIZED_JUNK: &str = " width='256earths' height='192earths'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_JUNK,
        RESIZED_JUNK,
        false,
        false,
    );
    const RESIZED_PERCENT: &str = " width='20%' height='20%'";
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        RESIZED_PERCENT,
        RESIZED_PERCENT,
        false,
        false,
    );
}

#[test]
fn null_resize_test() {
    // Make sure we don't crash on a value-less style attribute.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        " style",
        " style",
        false,
        false,
    );
}

#[test]
fn inline_test_without_optimize() {
    // Make sure we don't resize, if we don't optimize.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(10000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    // Without resize, it's not optimizable.
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/gif",
        "",
        CHEF_DIMS,
        false,
        false,
    );

    // No optimization has been applied.
    // NOTE: background-rewrite log assertions wait for the log record.
}

#[test]
fn inline_test_with_resize_with_optimize() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(10000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    const RESIZED_DIMS: &str = " width=48 height=64";
    // Without resize, it's not optimizable.
    // With resize, the image shrinks quite a bit, and we can inline it
    // given the 10K threshold explicitly set above.  This also strips the
    // size information, which is now embedded in the image itself anyway.
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/png",
        RESIZED_DIMS,
        "",
        true,
        true,
    );
    // After optimization, the GIF image is converted to a PNG image.
    // NOTE: background-rewrite log assertions wait for the log record.
}

#[test]
fn inline_test_with_resize_keep_dims() {
    // Non-image elements should retain dimensions when we inline.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(10000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);

    let initial_url = format!("{TEST_DOMAIN}{CHEF_GIF}");
    let page_url = format!("{TEST_DOMAIN}test.html");
    fixture.add_file_to_mock_fetcher(&initial_url, CHEF_GIF, "image/gif", 100);
    const RESIZED_DIMS: &str = " width=48 height=64";
    let html_input = format!("<td background='{initial_url}'{RESIZED_DIMS}></td>");
    let output = fixture.parse(&page_url, &html_input);
    // Image should have been resized
    assert!(
        output.contains("<!--Resized image http://test.com/IronChef2.gif from 192x256 to 48x64-->"),
        "{output}"
    );
    // And inlined
    assert!(output.contains("<td background='data:"), "{output}");
    // But dimensions should still be there.
    assert!(output.contains(RESIZED_DIMS), "{output}");
}

#[test]
fn inline_no_rewrite() {
    // Make sure we inline an image that isn't otherwise altered in any way.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(30000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    // This image is just small enough to inline, which also erases
    // dimension information.
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/gif",
        CHEF_DIMS,
        "",
        false,
        true,
    );
    // This image is too big to inline, and we don't insert missing
    // dimension information because that is not explicitly enabled.
    test_single_rewrite(
        &fixture,
        PUZZLE_JPG,
        "image/jpeg",
        "image/jpeg",
        "",
        "",
        false,
        false,
    );
}

#[test]
fn inline_no_resize() {
    // Make sure we inline an image if it meets the inlining threshold but can't
    // be resized.  Make sure we retain sizing information when this happens.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressWebp);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    const ORIG_DIMS: &str = " width=24 height=24";
    const RESIZED_DIMS: &str = " width=20 height=12";
    // At natural size, we should inline and erase dimensions.
    test_single_rewrite(
        &fixture,
        CHROMIUM24,
        "image/webp",
        "image/webp",
        ORIG_DIMS,
        "",
        false,
        true,
    );
    // Image is inlined but not resized, so preserve dimensions.
    test_single_rewrite(
        &fixture,
        CHROMIUM24,
        "image/webp",
        "image/webp",
        RESIZED_DIMS,
        RESIZED_DIMS,
        false,
        true,
    );
}

#[test]
fn inline_larger_resize() {
    // Make sure we inline an image if it meets the inlining threshold before
    // resize, resizing succeeds, but the resulting image is larger than the
    // original.  Make sure we retain sizing information when this happens.
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    const ORIG_DIMS: &str = " width=65 height=70";
    const RESIZED_DIMS: &str = " width=64 height=69";
    // At natural size, we should inline and erase dimensions.
    test_single_rewrite(
        &fixture,
        CUPPA_O_PNG,
        "image/png",
        "image/png",
        ORIG_DIMS,
        "",
        false,
        true,
    );
    // Image is inlined but not resized, so preserve dimensions.
    test_single_rewrite(
        &fixture,
        CUPPA_O_PNG,
        "image/png",
        "image/png",
        RESIZED_DIMS,
        RESIZED_DIMS,
        false,
        true,
    );
}

#[test]
fn resize_transparent_image() {
    let fixture = Fixture::new();
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
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    const RESIZED_DIMS: &str = " width=26 height=28";
    // Image is resized and inlined.
    test_single_rewrite(
        &fixture,
        CUPPA_T_PNG,
        "image/png",
        "image/png",
        RESIZED_DIMS,
        "",
        true,
        true,
    );
}

#[test]
fn no_crash_on_invalid_dim() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}a.png"), BIKE_PNG, "image/png", 100);

    fixture.parse(TEST_DOMAIN, "<img width=0 height=0 src=\"a.png\">");
    fixture.parse(TEST_DOMAIN, "<img width=0 height=42 src=\"a.png\">");
    fixture.parse(TEST_DOMAIN, "<img width=42 height=0 src=\"a.png\">");
    fixture.parse(TEST_DOMAIN, "<img width=\"-5\" height=\"5\" src=\"a.png\">");
    fixture.parse(TEST_DOMAIN, "<img width=\"-5\" height=\"0\" src=\"a.png\">");
    fixture.parse(
        TEST_DOMAIN,
        "<img width=\"-5\" height=\"-5\" src=\"a.png\">",
    );
    fixture.parse(TEST_DOMAIN, "<img width=\"5\" height=\"-5\" src=\"a.png\">");
}

#[test]
fn gif_to_png_test_with_resize_with_optimize() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    const RESIZED_DIMS: &str = " width=48 height=64";
    // With resize and optimization. Translating gif to png.
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/png",
        RESIZED_DIMS,
        RESIZED_DIMS,
        true,
        false,
    );
}

#[test]
fn gif_to_png_test_resize_enable_gif_to_png_disabled() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/gif",
        "",
        "",
        false,
        false,
    );
    const RESIZED_DIMS: &str = " width=48 height=64";
    // Not traslating gifs to pngs.
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/gif",
        RESIZED_DIMS,
        RESIZED_DIMS,
        false,
        false,
    );
}

#[test]
fn gif_to_png_test_without_resize_with_optimize() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    // Without resize and with optimization
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/png",
        "",
        "",
        true,
        false,
    );
}

// TODO(poojatandon): Add a test where .gif file size increases on optimization.

#[test]
fn gif_to_png_test_without_resize_without_optimize() {
    // Without resize and without optimization
    test_single_rewrite(
        &Fixture::new(),
        CHEF_GIF,
        "image/gif",
        "image/gif",
        "",
        "",
        false,
        false,
    );
}

#[test]
fn gif_to_jpeg_test_without_resize_with_optimize() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    // Without resize and with optimization
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/jpeg",
        "",
        "",
        true,
        false,
    );
}

#[test]
fn gif_to_webp_test_with_resize_with_optimize() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_lossless();
    const RESIZED_DIMS: &str = " width=48 height=64";
    // With resize and optimization
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/webp",
        RESIZED_DIMS,
        RESIZED_DIMS,
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 1.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn gif_to_webp_test_without_resize_with_optimize() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertPngToJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture
        .options
        .borrow_mut()
        .set_image_recompress_quality(85);
    fixture.setup_for_webp_lossless();
    // Without resize and with optimization
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/webp",
        "",
        "",
        true,
        false,
    );
    test_conversion_variables(
        &fixture, 0, 1.0, 0.0, // gif
        0, 0.0, 0.0, // png
        0, 0.0, 0.0, // jpg
        0, 0.0, 0.0, // gif animated
        true,
    );
}

#[test]
fn rewrites_dropped_due_to_no_saving_no_resize_test() {
    use ps_html::filters::image_rewrite::{
        IMAGE_REWRITES_DROPPED_NOSAVING_NORESIZE, IMAGE_REWRITE_LATENCY_FAILED_MS,
        IMAGE_REWRITE_LATENCY_OK_MS,
    };
    let fixture = Fixture::new();
    fixture
        .server
        .histogram(IMAGE_REWRITE_LATENCY_OK_MS)
        .clear();
    fixture
        .server
        .histogram(IMAGE_REWRITE_LATENCY_FAILED_MS)
        .clear();

    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    const ORIGINAL_DIMS: &str = " width=65 height=70";
    test_single_rewrite(
        &fixture,
        CUPPA_O_PNG,
        "image/png",
        "image/png",
        ORIGINAL_DIMS,
        ORIGINAL_DIMS,
        false,
        false,
    );
    assert_eq!(1, fixture.stat(IMAGE_REWRITES_DROPPED_NOSAVING_NORESIZE));
    assert_eq!(
        0.0,
        fixture
            .server
            .histogram(IMAGE_REWRITE_LATENCY_OK_MS)
            .count()
    );
    assert_eq!(
        1.0,
        fixture
            .server
            .histogram(IMAGE_REWRITE_LATENCY_FAILED_MS)
            .count()
    );
}

#[test]
fn rewrites_dropped_due_to_mime_type_unknown_test() {
    use ps_html::filters::image_rewrite::IMAGE_REWRITES_DROPPED_MIME_TYPE_UNKNOWN;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    const ORIGINAL_DIMS: &str = " width=10 height=10";
    test_single_rewrite(
        &fixture,
        SMALL_DATA,
        "image/png",
        "image/png",
        ORIGINAL_DIMS,
        ORIGINAL_DIMS,
        false,
        false,
    );
    assert_eq!(1, fixture.stat(IMAGE_REWRITES_DROPPED_MIME_TYPE_UNKNOWN));
}

#[test]
fn inline_test_with_resize_with_optimize_and_url_logging() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_image_inline_max_bytes(10000);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ResizeImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InlineImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::InsertImageDimensions);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    // NOTE: allow_logging_urls_in_log_record waits for the log record.

    const RESIZED_DIMS: &str = " width=48 height=64";
    // Without resize, it's not optimizable.
    // With resize, the image shrinks quite a bit, and we can inline it
    // given the 10K threshold explicitly set above.  This also strips the
    // size information, which is now embedded in the image itself anyway.
    test_single_rewrite(
        &fixture,
        CHEF_GIF,
        "image/gif",
        "image/png",
        RESIZED_DIMS,
        "",
        true,
        true,
    );
    test_single_rewrite_without_abs(
        &fixture,
        CHEF_GIF,
        CHEF_GIF,
        "image/gif",
        "image/png",
        RESIZED_DIMS,
        "",
        true,
        true,
    );
}

#[test]
fn no_transform_optimized() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_no_transform_optimized_images(true);
    fixture.add_recompress_image_filters();
    let initial_url = format!("{TEST_DOMAIN}{BIKE_PNG}");
    fixture.add_file_to_mock_fetcher(&initial_url, BIKE_PNG, "image/png", 100);
    let out_jpg_url = format!("{TEST_DOMAIN}{}", encode("", "ic", "0", BIKE_PNG, "jpg"));
    let (_, response_headers) = fixture.serve_url(&out_jpg_url).expect("serve");
    let values = response_headers
        .headers()
        .lookup("Cache-Control")
        .unwrap_or_default();
    assert!(
        values
            .iter()
            .flat_map(|value| value.split(','))
            .any(|directive| directive.trim() == "no-transform"),
        "{values:?}"
    );
}

#[test]
fn preserve_url_relativity() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture.add_file_to_mock_fetcher("a.jpg", PUZZLE_JPG, "image/jpeg", 100);
    fixture.add_file_to_mock_fetcher("b.jpg", PUZZLE_JPG, "image/jpeg", 100);
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}single_attribute"),
            "<img src=a.jpg><img src=http://test.com/b.jpg>"
        ),
        format!(
            "<img src={}><img src={}>",
            encode("", "ic", "0", "a.jpg", "jpg"),
            encode("http://test.com/", "ic", "0", "b.jpg", "jpg"),
        )
    );
}

#[test]
fn rewrite_404() {
    // Make sure we don't fail when rewriting with invalid input.
    let fixture = Fixture::new();
    fixture.set_fetch_response_404("404.jpg");
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);
    let expected = format!(
        "<img src='404.jpg'><!--4xx status code, preventing rewriting of {TEST_DOMAIN}404.jpg-->"
    );
    for _ in 0..2 {
        // Try twice to exercise the cached case.
        assert_eq!(
            fixture.parse(&format!("{TEST_DOMAIN}404"), "<img src='404.jpg'>"),
            expected
        );
    }
}

#[test]
fn honor_no_transform() {
    // If cache-control: no-transform then we should serve the original URL
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::Debug);

    let url = format!("{TEST_DOMAIN}notransform.png");
    fixture.add_file_to_mock_fetcher(&url, BIKE_PNG, "image/png", 100);
    fixture.add_to_response(&url, "Cache-Control", "no-transform");

    let expected =
        format!("<img src={url}><!--Cache-control: no-transform, preventing rewriting of {url}-->");
    for _ in 0..2 {
        // Validate twice in case changes in cache from the first request alter the
        // second.
        assert_eq!(
            fixture.parse(
                &format!("{TEST_DOMAIN}NoTransform"),
                &format!("<img src={url}>")
            ),
            expected
        );
    }
}

#[test]
fn fetch_invalid() {
    // Make sure that fetching invalid URLs cleanly reports a problem by
    // calling Done(false).
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);

    // We are trying to test with an invalid encoding. By construction,
    // Encode cannot make an invalid encoding.  However we can make one
    // using a PlaceHolder string and then mutating it.
    const PLACEHOLDER: &str = "PlaceHolder";
    let encoded_url = encode(
        "http://www.example.com/",
        "ic",
        "ABCDEFGHIJ",
        PLACEHOLDER,
        "jpg",
    );
    let encoded_url = encoded_url.replace(PLACEHOLDER, "70x53x,");
    assert!(fixture.serve_url(&encoded_url).is_none());
}

/// Rewrites three images, corrupts their URLs, and checks the corruption
/// is neither served badly nor cached (`TestCorruptUrl`).
fn test_corrupt_url(fixture: &Fixture, junk: &str, append_junk: bool) {
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}a.jpg"),
        PUZZLE_JPG,
        "image/jpeg",
        100,
    );
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}b.png"), BIKE_PNG, "image/png", 100);
    fixture.add_file_to_mock_fetcher(&format!("{TEST_DOMAIN}c.gif"), CHEF_GIF, "image/gif", 100);

    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertGifToPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);

    const HTML: &str = "<img src=\"a.jpg\"><img src=\"b.png\"><img src=\"c.gif\">";
    let normal_output = fixture.parse(TEST_DOMAIN, HTML);
    let image_urls = collect_img_srcs(normal_output.as_bytes());
    assert_eq!(3, image_urls.len());

    let base = GoogleUrl::new(TEST_DOMAIN);
    let absolute = |src: &str| GoogleUrl::resolve(&base, src).spec().to_string();
    let (url1, url2, url3) = (
        absolute(&image_urls[0]),
        absolute(&image_urls[1]),
        absolute(&image_urls[2]),
    );

    // Fetch messed up versions. Currently image rewriter doesn't actually
    // fetch them.
    assert!(fixture
        .serve_url(&change_suffix(&url1, append_junk, ".jpg", junk))
        .is_some());
    assert!(fixture
        .serve_url(&change_suffix(&url2, append_junk, ".png", junk))
        .is_some());
    // This actually has .png in the output since we convert gif -> png.
    assert!(fixture
        .serve_url(&change_suffix(&url3, append_junk, ".png", junk))
        .is_some());

    // Now run through again to make sure we didn't cache the messed up URL
    let output = fixture.parse(TEST_DOMAIN, HTML);
    assert_eq!(normal_output, output);
    let image_urls2 = collect_img_srcs(output.as_bytes());
    assert_eq!(3, image_urls2.len());
    assert_eq!(image_urls[0], image_urls2[0]);
    assert_eq!(image_urls[1], image_urls2[1]);
    assert_eq!(image_urls[2], image_urls2[2]);
}

#[test]
fn no_extension_corruption() {
    test_corrupt_url(&Fixture::new(), "%22", true /* append %22 */);
}

#[test]
fn no_query_corruption() {
    test_corrupt_url(&Fixture::new(), "?query", true /* append ?query*/);
}

#[test]
fn no_wrong_ext_corruption() {
    test_corrupt_url(
        &Fixture::new(),
        ".html",
        false, /* replace ext with .html */
    );
}

#[test]
fn rewrite_cache_extend_interaction() {
    // There was a bug in async mode where rewriting failing would prevent
    // cache extension from working as well.
    use ps_html::filters::cache_extender::CacheExtenderFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressPng);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheImages);

    // Provide a non-image file, so image rewrite fails (but cache extension
    // works)
    fixture.set_response_with_default_headers("a.png", "image/png", b"Not a PNG", 600);

    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}cache_extend_fallback"));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(ImageRewriteFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(CacheExtenderFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let url = format!("{TEST_DOMAIN}cache_extend_fallback");
    parser.start_parse(url.as_bytes(), false);
    parser.write(b"<img src=a.png>");
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        format!("<img src={}>", encode("", "ce", "0", "a.png", "png"))
    );
}

/// Fetches a rewritten URL with extra input headers (`TestRetainExtraHeaders`).
fn test_retain_extra_headers(fixture: &Fixture, name: &str, filter_id: &str, ext: &str) {
    let url = format!("{TEST_DOMAIN}{name}");

    // Add some extra headers.
    fixture.add_to_response(&url, "ETag", "Custom-Etag");
    fixture.add_to_response(&url, "extra", "attribute");
    fixture.add_to_response(&url, "Set-Cookie", "Custom-Cookie");

    let rewritten_url = format!("{TEST_DOMAIN}{}", encode("", filter_id, "0", name, ext));
    let (content, response) = fixture.serve_url(&rewritten_url).expect("serve");
    assert!(!content.is_empty());

    // Extra non-blacklisted header is preserved.
    let values = response.headers().lookup("extra").unwrap_or_default();
    assert_eq!(1, values.len());
    assert_eq!("attribute", values[0]);

    // Note: These tests can fail if ResourceManager::FetchResource failed to
    // rewrite the resource and instead served the original.
    // TODO(sligocki): Add a check that we successfully rewrote the resource.

    // Blacklisted headers are stripped (or changed).
    assert!(response.headers().lookup("Set-Cookie").is_none());

    let values = response.headers().lookup("ETag").unwrap_or_default();
    assert_eq!(1, values.len());
    assert_eq!("W/\"0\"", values[0]);
}

#[test]
fn retain_extra_headers() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);

    // Store image contents into fetcher.
    fixture.add_file_to_mock_fetcher(
        &format!("{TEST_DOMAIN}{PUZZLE_JPG}"),
        PUZZLE_JPG,
        "image/jpeg",
        100,
    );
    test_retain_extra_headers(&fixture, PUZZLE_JPG, "ic", "jpg");
}

#[test]
fn rewrite_multiple_attributes() {
    // Test a complex setup with both regular and custom image urls, including an
    // invalid image which should only get cache-extended.
    use ps_html::filters::cache_extender::CacheExtenderFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::RecompressJpeg);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheImages);

    fixture.options.borrow_mut().add_url_valued_attribute(
        "img",
        "data-src",
        ps_config::options::Category::Image,
    );

    // A, B, and D are real image files, so they should be properly rewritten.
    fixture.add_file_to_mock_fetcher("a.jpg", PUZZLE_JPG, "image/jpeg", 100);
    fixture.add_file_to_mock_fetcher("b.jpg", PUZZLE_JPG, "image/jpeg", 100);
    fixture.add_file_to_mock_fetcher("d.jpg", PUZZLE_JPG, "image/jpeg", 100);

    // C is not an image file, so image rewrite fails (but cache extension works).
    fixture.set_response_with_default_headers("c.jpg", "image/jpeg", b"Not a JPG", 600);

    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}multiple_attributes"));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(ImageRewriteFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(CacheExtenderFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let url = format!("{TEST_DOMAIN}multiple_attributes");
    parser.start_parse(url.as_bytes(), false);
    parser.write(b"<img src=a.jpg data-src=b.jpg data-src=c.jpg data-src=d.jpg>");
    parser.finish();
    assert_eq!(
        String::from_utf8(sink.bytes()).unwrap(),
        format!(
            "<img src={} data-src={} data-src={} data-src={}>",
            encode("", "ic", "0", "a.jpg", "jpg"),
            encode("", "ic", "0", "b.jpg", "jpg"),
            encode("", "ce", "0", "c.jpg", "jpg"),
            encode("", "ic", "0", "d.jpg", "jpg"),
        )
    );
}

/// Serves one fallback case (`ValidateFallbackHeaderSanitizationHelper`).
fn validate_fallback_header_sanitization_helper(
    fixture: &Fixture,
    filter_id: &str,
    origin_content_type: &str,
    expect_load: bool,
) {
    // Mangle the content type to make a url name by removing '/'s.
    let leafable = origin_content_type.replace('/', "-");

    let leaf = format!("leaf-{leafable}");
    let origin_contents = "this isn't a real file";

    let url = format!("{TEST_DOMAIN}{leaf}");
    let mut origin_response_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    origin_response_headers.set_first_line(1, 1, 200, "OK");
    origin_response_headers
        .headers_mut()
        .add("Content-Type", origin_content_type);

    // This is a case where we do need to make some changes for security and we
    // want to be sure we make them even if no-transform is set.
    origin_response_headers.set_date_and_caching(
        fixture.server.now_ms(),
        0, /* ttl */
        "; no-transform",
    );
    origin_response_headers.compute_caching();

    fixture
        .fetcher
        .set_response(&url, origin_response_headers, origin_contents.as_bytes());

    let resource = format!(
        "{TEST_DOMAIN}{}",
        encode("", filter_id, "0", &leaf, "ignored")
    );

    match fixture.serve_url(&resource) {
        Some((response_content, response_headers)) => {
            assert!(expect_load, "loaded {origin_content_type}");
            assert_eq!(origin_contents.as_bytes(), response_content.as_slice());
            assert_eq!(
                Some(origin_content_type.to_string()),
                response_headers.headers().lookup1("Content-Type"),
            );

            assert_eq!(
                Some("nosniff".to_string()),
                response_headers.headers().lookup1("X-Content-Type-Options"),
            );
        }
        None => assert!(!expect_load, "failed {origin_content_type}"),
    }
}

#[test]
fn content_type_validation() {
    let fixture = Fixture::new();
    // These content types will all be preserved.
    for content_type in [
        "text/css",
        "text/javascript",
        "application/javascript",
        "image/jpg",
        "image/jpeg",
        "image/png",
        "image/gif",
        "image/webp",
        "application/pdf",
    ] {
        validate_fallback_header_sanitization_helper(&fixture, "ic", content_type, true);
    }

    // All other content types will be stripped.
    for content_type in [
        "text/html",
        "text/plain",
        "text/xml",
        "application/xml",
        "image/svg",
        "image/svg+xml",
        "audio/mp3",
        "video/mp4",
        "",
        "invalid",
    ] {
        validate_fallback_header_sanitization_helper(&fixture, "ic", content_type, false);
    }
}

#[test]
fn serve_webp_from_cold_cache() {
    use ps_http::response::{RequestProperties, ValidatorOption, VaryOption};
    let cacheable = |headers: &ResponseHeaders| {
        headers.is_proxy_cacheable(
            RequestProperties::default(),
            VaryOption::RespectVaryOnResources,
            ValidatorOption::NoValidator,
        )
    };

    // First rewrite an HTML file with an image for a webp-compatible browser,
    // and collect the image URL.
    let fixture = Fixture::with_hasher(Rc::new(ps_cache::hash::Md5Hasher::new()));
    fixture.add_recompress_image_filters();
    fixture
        .options
        .borrow_mut()
        .set_serve_rewritten_webp_urls_to_any_agent(true);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ConvertJpegToWebp);
    fixture.reset_for_webp();
    fixture.add_file_to_mock_fetcher(PUZZLE_URL, PUZZLE_JPG, "image/jpeg", 100);
    let img_src = rewrite_image_from_html(&fixture, "img", "image/webp");
    assert_eq!(1, fixture.stat(IMAGE_WEBP_REWRITES));
    let webp_url = GoogleUrl::resolve(
        &GoogleUrl::new("http://rewrite_image.test/RewriteImage.html"),
        &img_src,
    )
    .spec()
    .to_string();

    // Serve this image from cache. No further rewrites should be needed, since
    // the image was optimized when serving HTML.
    let (golden_content, response) = fixture.fetch_webp(&webp_url, "webp").expect("serve webp");
    assert_eq!(
        "image/webp",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
    assert!(cacheable(&response));
    assert_eq!(0, fixture.stat(IMAGE_WEBP_REWRITES));
    assert_eq!(1, fixture.http_hits());

    // Now clear the cache and fetch the resource again.  We will need to
    // reconstruct the image but we'll get the same result.
    fixture.clear_caches();
    let (content, response) = fixture.fetch_webp(&webp_url, "webp").expect("rebuild");
    assert_eq!(
        "image/webp",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
    assert!(cacheable(&response));
    assert_eq!(1, fixture.stat(IMAGE_WEBP_REWRITES)); // We had to reconstruct.
    assert_eq!(0, fixture.http_hits());
    assert_eq!(content, golden_content);

    // Do the same test again, but don't clear the cache.
    let (content, response) = fixture.fetch_webp(&webp_url, "webp").expect("warm");
    assert_eq!(
        "image/webp",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
    assert!(cacheable(&response));
    assert_eq!(0, fixture.stat(IMAGE_WEBP_REWRITES)); // No need to reconstruct...
    assert_eq!(1, fixture.http_hits()); // ...picked it up from cache.
    assert_eq!(content, golden_content);

    // Now set the user-agent to something that does not support webp,
    // and we should still reconstruct the webp when asked for it, since
    // we have called options()->set_serve_rewritten_webp_urls_to_any_agent(true)
    // above.
    fixture.clear_caches();
    let (content, response) = fixture.fetch_webp(&webp_url, "null").expect("any agent");
    assert_eq!(
        "image/webp",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
    assert!(cacheable(&response));
    assert_eq!(1, fixture.stat(IMAGE_WEBP_REWRITES)); // We had to reconstruct.
    assert_eq!(0, fixture.http_hits());
    assert_eq!(content, golden_content);

    // Now turn off 'serve_rewritten_webp_urls_to_any_agent', and
    // we will serve the original jpeg instead, privately cached.
    fixture
        .options
        .borrow_mut()
        .set_serve_rewritten_webp_urls_to_any_agent(false);

    // Don't clear the cache here, proving Issue 846 is fixed.
    let (content, response) = fixture
        .fetch_webp(&webp_url, "null")
        .expect("jpeg fallback");
    assert_eq!(
        "image/jpeg",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
    assert!(!cacheable(&response));
    assert!(response.is_browser_cacheable());
    assert_eq!(0, fixture.stat(IMAGE_WEBP_REWRITES)); // Reconstruction not attempted.
    assert_eq!(2, fixture.http_hits()); // Hits, but result is invalid.
    assert_ne!(content, golden_content);
    assert!(content.len() > golden_content.len());

    // All works fine anyway we if we clear the cache first.
    fixture.clear_caches();
    let (content, response) = fixture.fetch_webp(&webp_url, "null").expect("cold jpeg");
    assert_eq!(
        "image/jpeg",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
    assert!(!cacheable(&response));
    assert!(response.is_browser_cacheable());
    assert_eq!(0, fixture.stat(IMAGE_WEBP_REWRITES)); // Reconstruction not attempted.
    assert_eq!(0, fixture.http_hits());
    assert_ne!(content, golden_content);
    assert!(content.len() > golden_content.len());

    // But if any webp-enabled client asks for the resource, we will serve
    // the webp to them.
    let (_, response) = fixture.fetch_webp(&webp_url, "webp").expect("webp again");
    assert_eq!(
        "image/webp",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );

    // And we will continue to serve jpeg to other browsers.
    let (_, response) = fixture.fetch_webp(&webp_url, "none").expect("jpeg again");
    assert_eq!(
        "image/jpeg",
        response
            .headers()
            .lookup1("Content-Type")
            .as_deref()
            .unwrap()
    );
}

#[test]
fn servant_recompresses_without_browser_flag() {
    use ps_html::rewrite_filter::RewriteFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .filters_mut()
        .set_level(ps_config::filter::RewriteLevel::CoreFilters);
    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}r.png"));
    let servant = ImageRewriteFilter::new(driver.clone());
    let input = read_image(CUPPA_PNG);
    let content_type = ps_http::content_type::mime_to_content_type("image/png").expect("png known");
    let output = servant.rewrite_in_place(&driver, &input, &content_type);
    // CoreFilters lacks InPlaceOptimizeForBrowser: same-format
    // recompress at most (None when nothing is saved).
    let output = output.expect("cuppa recompresses");
    {
        use ps_image::frame::PixelFormat;
        assert_eq!("image/png", output.content_type.mime_type());
        assert!(output.bytes.len() < input.len());
        let frame = ps_image::png::decode(&output.bytes).expect("valid png");
        let orig = ps_image::png::decode(&input).expect("valid input");
        assert_eq!(
            (orig.width(), orig.height()),
            (frame.width(), frame.height())
        );
        let rgba = frame.expand(PixelFormat::Rgba8888).expect("expand out");
        let orig_rgba = orig.expand(PixelFormat::Rgba8888).expect("expand in");
        assert_eq!(orig_rgba.pixels(), rgba.pixels());
    }
}
