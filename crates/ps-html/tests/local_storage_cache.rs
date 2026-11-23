//! Local-storage-cache tests ported from
//! `local_storage_cache_filter_test.cc`.
//!
//! NOTE: the document wrapper is simplified (no title, whitespace,
//! or noscript-redirect: the redirect comes from a beacon filter
//! outside this chain). NOTE: the three first-view sub-cases are
//! deferred: they stage a pending fetch via a wait fetcher, which
//! has no sync-model equivalent (rewrites here complete inline).
//! NOTE: the failing-inline cases expect no runtime script: the
//! sync pipeline resolves the inline before the LSC pass runs, so
//! the transient marker is already gone; C++ inserts the script
//! because its inline resolves asynchronously ("C'est la vie").
//! NOTE: `img_local_storage_disabled` drops the `DeferIframe` line
//! (noscript wrapper only) and the IPRO line (that subsystem is
//! unported); neither affects the expected output.
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
use ps_html::filters::image_rewrite::ImageRewriteFilter;
use ps_html::filters::local_storage_cache::{LocalStorageCacheFilter, LSC_COOKIE_NAME};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::writer::WriterFilter;
use ps_http::mock::MockUrlFetcher;
use ps_http::request::RequestHeaders;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;
/// `format_http_date(START_MS + 100_000)`, pinned like the C++
/// `Tue, 02 Feb 2010 18:53:06 GMT`.
const EXPIRY: &str = "Tue, 14 Nov 2023 22:15:00 GMT";

const JUNK_CSS_FILENAME: &str = "http://example.com/junk.css";
const STYLES_CSS_FILENAME: &str = "styles.css";
const CUPPA_PNG_FILENAME: &str = "Cuppa.png";
const PUZZLE_JPG_FILENAME: &str = "Puzzle.jpg";

const JUNK_CSS_CONTENTS: &[u8] = b"@import url(junk://junk.com);";
const STYLES_CSS_CONTENTS: &[u8] =
    b".background_cyan{background-color:#0ff}.foreground_pink{color:#ffc0cb}";

const CSS_HASH: &str = "Fe1SLPZ14c";
const IMG_HASH: &str = "du_OhARrJl";
const HASH_30X30: &str = "07FPv8sBor";
const HASH_150X150: &str = "jSr1gEyima";

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Arc<MockUrlFetcher>,
    request_headers: RefCell<RequestHeaders>,
}

impl Fixture {
    fn build(mock_hasher: bool) -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Arc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend,
            metadata_backend,
            fetcher.clone(),
            START_MS,
        );
        if mock_hasher {
            server.set_hasher(Arc::new(ps_cache::hash::MockHasher::new()));
        }
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options.borrow_mut().enable_filter(Filter::InlineCss);
        options.borrow_mut().enable_filter(Filter::InlineImages);
        options
            .borrow_mut()
            .enable_filter(Filter::LocalStorageCache);
        options.borrow_mut().set_image_inline_max_bytes(2000);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            request_headers: RefCell::new(RequestHeaders::new()),
        }
    }

    /// Default test hasher (`MockHasher`, hashing to `"0"`).
    fn new() -> Self {
        Self::build(true)
    }

    /// Real MD5 hasher (`UseMd5Hasher`).
    fn with_md5() -> Self {
        let fixture = Self::build(false);
        fixture.set_up();
        fixture
    }

    /// Scripts the shared resources (`MySetUp`).
    fn set_up(&self) {
        self.set_response_with_default_headers(
            JUNK_CSS_FILENAME,
            "text/css",
            JUNK_CSS_CONTENTS,
            100,
        );
        self.set_response_with_default_headers(
            STYLES_CSS_FILENAME,
            "text/css",
            STYLES_CSS_CONTENTS,
            100,
        );
        self.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{CUPPA_PNG_FILENAME}"),
            CUPPA_PNG_FILENAME,
            "image/png",
            100,
        );
        self.add_file_to_mock_fetcher(
            &format!("{TEST_DOMAIN}{PUZZLE_JPG_FILENAME}"),
            PUZZLE_JPG_FILENAME,
            "image/jpeg",
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

    fn add_file_to_mock_fetcher(&self, url: &str, file: &str, content_type: &str, ttl_sec: i64) {
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(file),
        )
        .unwrap_or_else(|_| panic!("missing image {file}"));
        self.set_response_with_default_headers(url, content_type, &body, ttl_sec);
    }

    /// Stages a `Cookie` request header.
    fn set_cookie(&self, cookie: &str) {
        self.request_headers
            .borrow_mut()
            .headers_mut()
            .add("Cookie", cookie);
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver.set_request_headers(self.request_headers.borrow().clone());
        driver
    }

    /// Parses through inline, image, and LSC filters (chain order).
    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssInlineFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(ImageRewriteFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(LocalStorageCacheFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Case comparison (`TestLocalStorage`).
    fn test_local_storage(
        &self,
        case_id: &str,
        head_html_in: &str,
        head_html_out: &str,
        body_html_in: &str,
        body_html_out: &str,
    ) {
        let html_in =
            format!("<html><head>{head_html_in}</head><body>{body_html_in}</body></html>");
        let expected_out =
            format!("<html><head>{head_html_out}</head><body>{body_html_out}</body></html>");
        let url = format!("http://test.com/{case_id}.html");
        assert_eq!(
            self.parse(&url, &html_in),
            expected_out,
            "test id:{case_id}"
        );
    }

    /// Expected runtime script plus a snippet
    /// (`InsertScriptBefore`).
    fn insert_script_before(snippet: &str) -> String {
        let js = get_asset(StaticAsset::LocalStorageCacheJs, false);
        format!(
            "<script type=\"text/javascript\" data-pagespeed-no-defer>{}pagespeed.localStorageCacheInit();</script>{snippet}",
            String::from_utf8_lossy(js),
        )
    }

    /// Exact data URL of a testdata image (the C++ goldens equal
    /// the file bytes).
    fn data_url(&self, file: &str, mime: &str) -> String {
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(file),
        )
        .unwrap_or_else(|_| panic!("missing image {file}"));
        format!(
            "data:{mime};base64,{}",
            ps_cache::base64::mime_encode(&body)
        )
    }
}

#[test]
fn simple() {
    let fixture = Fixture::new();
    fixture.set_up();
    fixture.test_local_storage("simple", "", "", "<div/>", "<div/>");
}

#[test]
fn link() {
    let fixture = Fixture::new();
    fixture.set_up();
    fixture.test_local_storage(
        "link",
        "<link rel='stylesheet' href='styles.css'>",
        &Fixture::insert_script_before(&format!(
            "<style data-pagespeed-lsc-url=\"{TEST_DOMAIN}styles.css\" data-pagespeed-lsc-hash=\"0\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">{}</style>",
            String::from_utf8_lossy(STYLES_CSS_CONTENTS),
        )),
        "<div/>",
        "<div/>",
    );
}

#[test]
fn link_rewrite_context_not_executed() {
    // The domains are different so the RewriteContext is never
    // kicked off, which should result in no local storage cache
    // changes at all.
    let fixture = Fixture::new();
    fixture.set_up();
    fixture.test_local_storage(
        "link_rewrite_context_not_executed",
        "<link rel='stylesheet' href='http://example.com/junk.css'>",
        "<link rel='stylesheet' href='http://example.com/junk.css'>",
        "<div/>",
        "<div/>",
    );
    assert_eq!(
        fixture.fetcher.fetch_count(),
        0,
        "no context means no fetch"
    );
}

#[test]
fn link_url_transformation_fails() {
    // The CSS rewriting fails so the local storage cache
    // attributes are omitted. (Sync NOTE: C++ still inserts the
    // JS because its rewriting is asynchronous; here the failure
    // resolves before the LSC pass, so no script is inserted.)
    let fixture = Fixture::new();
    fixture.set_up();
    fixture.options.borrow_mut().add_domain("example.com");
    fixture.test_local_storage(
        "link_url_transformation_fails",
        "<link rel='stylesheet' href='http://example.com/junk.css'>",
        "<link rel='stylesheet' href='http://example.com/junk.css'>",
        "<div/>",
        "<div/>",
    );
    assert!(
        fixture.fetcher.fetch_count() >= 1,
        "junk.css must actually be fetched (authorized domain)"
    );
}

#[test]
fn link_dont_inline() {
    // The CSS inlining fails because we've turned the limit down
    // low. We expect no LSC attributes in the result. (Sync NOTE:
    // C++ still inserts the JS; see above.)
    let fixture = Fixture::new();
    fixture.set_up();
    fixture.options.borrow_mut().set_css_inline_max_bytes(10);
    fixture.test_local_storage(
        "link_dont_inline",
        "<link rel='stylesheet' href='styles.css'>",
        "<link rel='stylesheet' href='styles.css'>",
        "<div/>",
        "<div/>",
    );
    assert!(
        fixture.fetcher.fetch_count() >= 1,
        "styles.css must actually be fetched (inline refused, not skipped)"
    );
}

#[test]
fn img() {
    let fixture = Fixture::new();
    fixture.set_up();
    let data_url = fixture.data_url(CUPPA_PNG_FILENAME, "image/png");
    fixture.test_local_storage(
        "img",
        "",
        "",
        &format!("<img src='{CUPPA_PNG_FILENAME}'>"),
        &Fixture::insert_script_before(&format!(
            "<img src='{data_url}' data-pagespeed-lsc-url=\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\" data-pagespeed-lsc-hash=\"0\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">",
        )),
    );
}

#[test]
fn img_too_big() {
    // (Sync NOTE: C++ still inserts the JS; see above.)
    let fixture = Fixture::new();
    fixture.set_up();
    fixture.test_local_storage(
        "img_too_big",
        "",
        "",
        &format!("<img src='{PUZZLE_JPG_FILENAME}'>"),
        &format!("<img src='{PUZZLE_JPG_FILENAME}'>"),
    );
    assert!(
        fixture.fetcher.fetch_count() >= 1,
        "Puzzle.jpg must actually be fetched (inline refused, not skipped)"
    );
}

#[test]
fn img_local_storage_disabled() {
    let fixture = Fixture::new();
    fixture.set_up();
    fixture
        .options
        .borrow_mut()
        .disable_filter(Filter::LocalStorageCache);
    fixture.test_local_storage(
        "img_local_storage_disabled",
        "",
        "",
        &format!("<img src='{PUZZLE_JPG_FILENAME}'>"),
        &format!("<img src='{PUZZLE_JPG_FILENAME}'>"),
    );
}

#[test]
fn cookie_set() {
    // Only the CSS hash is suppressed by the cookie.
    let fixture = Fixture::with_md5();
    fixture.set_cookie(&format!("{LSC_COOKIE_NAME}={CSS_HASH}"));
    let data_url = fixture.data_url(CUPPA_PNG_FILENAME, "image/png");
    fixture.test_local_storage(
        "cookie_set",
        &format!("<link rel='stylesheet' href='{STYLES_CSS_FILENAME}'>"),
        &Fixture::insert_script_before(&format!(
            "<script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineCss(\"{TEST_DOMAIN}{STYLES_CSS_FILENAME}\");</script>",
        )),
        &format!("<img src='{CUPPA_PNG_FILENAME}'>"),
        &format!(
            "<img src='{data_url}' data-pagespeed-lsc-url=\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\" data-pagespeed-lsc-hash=\"{IMG_HASH}\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">",
        ),
    );
}

#[test]
fn repeat_views() {
    // NOTE: the first view (pending fetch staging) is deferred.
    let fixture = Fixture::with_md5();
    let css = format!("<link rel='stylesheet' href='{STYLES_CSS_FILENAME}'>");
    let img = format!("<img src='{CUPPA_PNG_FILENAME}'>");

    // Second view inlines them both and adds an expiry to both.
    let data_url = fixture.data_url(CUPPA_PNG_FILENAME, "image/png");
    let inlined_css = format!(
        "<style data-pagespeed-lsc-url=\"{TEST_DOMAIN}{STYLES_CSS_FILENAME}\" data-pagespeed-lsc-hash=\"{CSS_HASH}\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">{}</style>",
        String::from_utf8_lossy(STYLES_CSS_CONTENTS),
    );
    let inlined_img = format!(
        "<img src='{data_url}' data-pagespeed-lsc-url=\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\" data-pagespeed-lsc-hash=\"{IMG_HASH}\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">",
    );
    fixture.test_local_storage(
        "second_view",
        &css,
        &Fixture::insert_script_before(&inlined_css),
        &img,
        &inlined_img,
    );

    // The JavaScript would set these cookies for the next request.
    fixture.set_cookie(&format!("{LSC_COOKIE_NAME}={CSS_HASH}!{IMG_HASH}"));

    // Third view sends scripts in place of the link and img.
    let scripted_css = format!(
        "<script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineCss(\"{TEST_DOMAIN}{STYLES_CSS_FILENAME}\");</script>",
    );
    let scripted_img = format!(
        "<script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineImg(\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\", \"{IMG_HASH}\");</script>",
    );
    fixture.test_local_storage(
        "third_view",
        &css,
        &Fixture::insert_script_before(&scripted_css),
        &img,
        &scripted_img,
    );
}

#[test]
fn repeat_views_with_other_attributes() {
    // NOTE: the first view (pending fetch staging) is deferred.
    let fixture = Fixture::with_md5();
    let css = format!("<link rel='stylesheet' href='{STYLES_CSS_FILENAME}'>");
    let img = format!(
        "<img src='{CUPPA_PNG_FILENAME}' alt='A cup of joe' alt=\"A cup of joe\" alt='A cup of joe&#39;s \"joe\"' alt=\"A cup of joe's &quot;joe&quot;\">",
    );

    // Second view inlines them both and adds an expiry to both.
    let data_url = fixture.data_url(CUPPA_PNG_FILENAME, "image/png");
    let inlined_css = format!(
        "<style data-pagespeed-lsc-url=\"{TEST_DOMAIN}{STYLES_CSS_FILENAME}\" data-pagespeed-lsc-hash=\"{CSS_HASH}\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">{}</style>",
        String::from_utf8_lossy(STYLES_CSS_CONTENTS),
    );
    let inlined_img = format!(
        "<img src='{data_url}' alt='A cup of joe' alt=\"A cup of joe\" alt='A cup of joe&#39;s \"joe\"' alt=\"A cup of joe's &quot;joe&quot;\" data-pagespeed-lsc-url=\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\" data-pagespeed-lsc-hash=\"{IMG_HASH}\" data-pagespeed-lsc-expiry=\"{EXPIRY}\">",
    );
    fixture.test_local_storage(
        "second_view",
        &css,
        &Fixture::insert_script_before(&inlined_css),
        &img,
        &inlined_img,
    );

    // The JavaScript would set these cookies for the next request.
    fixture.set_cookie(&format!("{LSC_COOKIE_NAME}={CSS_HASH}!{IMG_HASH}"));

    // Third view sends scripts in place of the link and img.
    let scripted_css = format!(
        "<script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineCss(\"{TEST_DOMAIN}{STYLES_CSS_FILENAME}\");</script>",
    );
    let scripted_img = format!(
        "<script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineImg(\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\", \"{IMG_HASH}\", \"alt=A cup of joe\", \"alt=A cup of joe\", \"alt=A cup of joe\\'s \\\"joe\\\"\", \"alt=A cup of joe\\'s \\\"joe\\\"\");</script>",
    );
    fixture.test_local_storage(
        "third_view",
        &css,
        &Fixture::insert_script_before(&scripted_css),
        &img,
        &scripted_img,
    );
}

#[test]
fn repeat_views_of_same_image_at_different_sizes() {
    // NOTE: the first view (pending fetch staging) is deferred.
    // Enable "rewrite_images" so that the instances are rescaled.
    let fixture = Fixture::with_md5();
    for filter in [
        Filter::ConvertGifToPng,
        Filter::ConvertJpegToProgressive,
        Filter::InlineImages,
        Filter::JpegSubsampling,
        Filter::RecompressJpeg,
        Filter::RecompressPng,
        Filter::RecompressWebp,
        Filter::ResizeImages,
        Filter::StripImageColorProfile,
        Filter::StripImageMetaData,
    ] {
        fixture.options.borrow_mut().enable_filter(filter);
    }
    let imgs = format!(
        "<img src='{CUPPA_PNG_FILENAME}' width=\"30\" height=\"30\"><img src='{CUPPA_PNG_FILENAME}' width=\"150\" height=\"150\">",
    );

    // Second view inlines them and adds an expiry. The inline
    // bytes are encoder-specific, so the data URLs are checked
    // structurally; hashes, dimensions, and expiries exactly.
    let out = fixture.parse(
        "http://test.com/second_view.html",
        &format!("<html><head></head><body>{imgs}</body></html>"),
    );
    let body = out
        .strip_prefix("<html><head></head><body>")
        .and_then(|rest| rest.strip_suffix("</body></html>"))
        .unwrap_or("");
    let script = Fixture::insert_script_before("");
    assert!(
        body.starts_with(&script),
        "missing runtime script: {}",
        &body[..body.len().min(200)]
    );
    let tags: Vec<&str> = body[script.len()..].split("<img ").skip(1).collect();
    assert_eq!(tags.len(), 2, "second view body: {body}");
    fn payload(tag: &str) -> &str {
        tag.strip_prefix("src='data:image/png;base64,")
            .and_then(|rest| rest.split('\'').next())
            .unwrap_or("")
    }
    let first = payload(tags[0]);
    let second = payload(tags[1]);
    assert!(!first.is_empty() && !second.is_empty(), "body: {body}");
    assert_ne!(first, second, "resized payloads must differ");
    // The 30x30 dims are dropped (they match the cached image);
    // the 150x150 ones are kept.
    assert!(
        !tags[0].contains("width=") && !tags[0].contains("height="),
        "first tag: {}",
        tags[0]
    );
    for (tag, hash) in [(tags[0], HASH_30X30), (tags[1], HASH_150X150)] {
        assert!(
            tag.contains(&format!(
                "data-pagespeed-lsc-url=\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\""
            )),
            "tag: {tag}"
        );
        assert!(
            tag.contains(&format!("data-pagespeed-lsc-hash=\"{hash}\"")),
            "tag: {tag}"
        );
        assert!(
            tag.contains(&format!("data-pagespeed-lsc-expiry=\"{EXPIRY}\"")),
            "tag: {tag}"
        );
    }
    assert!(
        tags[1].contains("width=\"150\"") && tags[1].contains("height=\"150\""),
        "second tag: {}",
        tags[1]
    );

    // The JavaScript would set this cookie for the next request.
    fixture.set_cookie(&format!("{LSC_COOKIE_NAME}={HASH_30X30}!{HASH_150X150}"));

    // Third view sends scripts in place of the imgs.
    let scripted_imgs = format!(
        "<script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineImg(\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\", \"{HASH_30X30}\", \"width=30\", \"height=30\");</script><script data-pagespeed-no-defer>pagespeed.localStorageCache.inlineImg(\"{TEST_DOMAIN}{CUPPA_PNG_FILENAME}\", \"{HASH_150X150}\", \"width=150\", \"height=150\");</script>",
    );
    fixture.test_local_storage(
        "third_view",
        "",
        "",
        &imgs,
        &Fixture::insert_script_before(&scripted_imgs),
    );
}
