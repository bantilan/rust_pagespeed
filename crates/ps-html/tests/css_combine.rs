//! CSS-combining tests ported from `css_combine_filter_test.cc`.
//!
//! NOTE: logging assertions wait for the log record; async
//! delay-cache cases wait for the async reconstruction epic;
//! encoding-specific metadata-corruption cases have no equivalent
//! here.
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
use ps_html::filters::css_combine::{
    CssCombineFilter, COMBINE_OPPORTUNITIES, FILE_COUNT_REDUCTION,
};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const DOMAIN: &str = "http://combine_css.test/";
const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const YELLOW: &str = ".yellow {background-color: yellow;}";
const BLUE: &str = ".blue {color: blue;}\n";
const A_CSS_BODY: &str = ".c1 {\n background-color: blue;\n}\n";
const B_CSS_BODY: &str = ".c2 {\n color: yellow;\n}\n";
const C_CSS_BODY: &str = ".c3 {\n font-weight: bold;\n}\n";
const CSS_A: &str = "a.css";
const CSS_B: &str = "b.css";
const PROXY_MAP_DOMAIN: &str = "http://proxy.test/";
const XHTML_DTD: &str = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Arc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    mime_is_xhtml: RefCell<Option<bool>>,
}

impl Fixture {
    fn new() -> Self {
        Self::build(true)
    }

    /// Builds a fixture with the real MD5 namer (`UseMd5Hasher`).
    fn with_md5() -> Self {
        Self::build(false)
    }

    fn build(mock: bool) -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Arc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend.clone(),
            metadata_backend.clone(),
            fetcher.clone(),
            START_MS,
        );
        if mock {
            server.set_hasher(Arc::new(ps_cache::hash::MockHasher::new()));
        }
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::CombineCss);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            http_backend,
            metadata_backend,
            mime_is_xhtml: RefCell::new(None),
        }
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

    /// Marks the response HTML (`SetHtmlMimetype`).
    fn set_html_mimetype(&self) {
        *self.mime_is_xhtml.borrow_mut() = Some(false);
    }

    /// Scripts a cacheable CSS fetch (default headers).
    fn mock_css(&self, url: &str, content: &[u8]) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("text/css"),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), 600 * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(url, headers, content);
    }

    /// Scripts a 404 fetch (`SetFetchResponse404`).
    fn mock_404(&self, url: &str) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 404, "Not Found");
        self.fetcher.set_response(url, headers, b"");
    }

    fn setup_css_resources(&self, a_css_name: &str, b_css_name: &str) {
        self.mock_css(&format!("{DOMAIN}{a_css_name}"), A_CSS_BODY.as_bytes());
        self.mock_css(&format!("{DOMAIN}{b_css_name}"), B_CSS_BODY.as_bytes());
        self.mock_css(&format!("{DOMAIN}c.css"), C_CSS_BODY.as_bytes());
    }

    /// Parses through the combiner.
    fn parse(&self, url: &str, html: &str) -> String {
        String::from_utf8(self.parse_bytes(url, html.as_bytes())).unwrap()
    }

    fn parse_bytes(&self, url: &str, html: &[u8]) -> Vec<u8> {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(CssCombineFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html);
        parser.finish();
        sink.bytes()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
        driver.add_filter(Rc::new(CssCombineFilter::new(driver.clone())));
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
        driver.add_filter(Rc::new(CssCombineFilter::new(driver.clone())));
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

    /// Zeros combiner statistics (`ClearStats`).
    fn clear_stats(&self) {
        self.server.variable(COMBINE_OPPORTUNITIES).clear();
        self.server.variable(FILE_COUNT_REDUCTION).clear();
    }
}

/// Synthesizes an HTML css link element (`Link`).
fn link(href: &str, media: &str, close: bool) -> String {
    let mut out = format!("<link rel=\"stylesheet\" type=\"text/css\" href=\"{href}\"");
    if !media.is_empty() {
        out.push_str(&format!(" media=\"{media}\""));
    }
    if close {
        out.push('/');
    }
    out.push('>');
    out
}

/// Collects stylesheet link hrefs (`CollectCssLinks`).
fn collect_css_links(html: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(html);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
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
                    .find(|c: char| c == '>' || c.is_whitespace() || c == '/')
                    .unwrap_or(value.len());
                &value[..end]
            };
            out.push(href.to_string());
        }
        rest = &rest[end..];
    }
    out
}

/// Expected multipart leaf for names under a hash.
fn combined_leaf(names: &[&str], hash: &str) -> String {
    use ps_html::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    let inputs: Vec<Vec<u8>> = names.iter().map(|name| name.as_bytes().to_vec()).collect();
    format!(
        "{}.pagespeed.cc.{hash}.css",
        String::from_utf8_lossy(&MultipartEncoder.encode(&inputs))
    )
}

/// Full combining matrix (`CombineCssWithNames`).
#[allow(clippy::too_many_arguments)]
fn combine_css_with_names(
    fixture: &Fixture,
    id: &str,
    barrier_text: &str,
    debug_text: &str,
    is_barrier: bool,
    a_css_name: &str,
    b_css_name: &str,
    expect_combine: bool,
) {
    let html_url = format!("{DOMAIN}{id}.html");
    let html_input = format!(
        "<head>\n  {}\n  {}\n  <title>Hello, Instaweb</title>\n{barrier_text}</head>\n<body>\n  <div class='c1'>\n    <div class='c2'>\n      Yellow on Blue\n    </div>\n  </div>\n  {}\n</body>\n",
        link(a_css_name, "", false),
        link(b_css_name, "", false),
        link("c.css", "", false),
    );
    fixture.setup_css_resources(a_css_name, b_css_name);
    let file_count_before = fixture.stat(FILE_COUNT_REDUCTION);
    let output = fixture.parse(&html_url, &html_input);
    let css_urls = collect_css_links(output.as_bytes());
    assert!(!css_urls.is_empty(), "id: {id}");
    let combine_url = css_urls[0].clone();
    let expected_combination = if is_barrier {
        format!("{A_CSS_BODY}{B_CSS_BODY}")
    } else {
        format!("{A_CSS_BODY}{B_CSS_BODY}{C_CSS_BODY}")
    };
    let mut expected_reduction = file_count_before + if is_barrier { 1 } else { 2 };
    if !expect_combine {
        expected_reduction = 0;
    }
    assert_eq!(
        expected_reduction,
        fixture.stat(FILE_COUNT_REDUCTION),
        "id: {id}"
    );
    // NOTE: rewriter-log assertions wait for the log record.
    let expected_output = format!(
        "<head>\n  {}\n  \n  <title>Hello, Instaweb</title>\n{debug_text}{barrier_text}</head>\n<body>\n  <div class='c1'>\n    <div class='c2'>\n      Yellow on Blue\n    </div>\n  </div>\n  {}\n</body>\n",
        link(&combine_url, "", false),
        if is_barrier { link("c.css", "", false) } else { String::new() },
    );
    if expect_combine {
        assert!(output.contains(&expected_output), "id: {id}\n{output}");
        let base = ps_http::google_url::GoogleUrl::new(&html_url);
        let combine_gurl = ps_http::google_url::GoogleUrl::resolve(&base, &combine_url);
        let (content, headers) = fixture
            .serve_url(combine_gurl.spec())
            .unwrap_or_else(|| panic!("serve {}", combine_gurl.spec()));
        assert_eq!(headers.status_code(), 200);
        assert_eq!(content, expected_combination.as_bytes());
        let (recombined, _) = fixture
            .serve_from_new_context(combine_gurl.spec())
            .expect("reserve");
        assert_eq!(recombined, expected_combination.as_bytes());
    }
}

/// Basic combining matrix (`CombineCss`).
fn combine_css_matrix(
    fixture: &Fixture,
    id: &str,
    barrier_text: &str,
    debug_text: &str,
    is_barrier: bool,
) {
    combine_css_with_names(
        fixture,
        id,
        barrier_text,
        debug_text,
        is_barrier,
        CSS_A,
        CSS_B,
        true,
    );
}

#[test]
fn combine_css() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    combine_css_matrix(&fixture, "combine_css_no_hash", "", "", false);
}

#[test]
fn combine_css_unhealthy() {
    let fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    fixture.set_html_mimetype();
    fixture.setup_css_resources(CSS_A, CSS_B);
    let html_input = format!(
        "<head>\n  {}\n  {}\n",
        link(CSS_A, "", false),
        link(CSS_B, "", false)
    );
    assert_eq!(
        fixture.parse(&format!("{DOMAIN}unhealthy.html"), &html_input),
        html_input
    );
}

#[test]
fn fetch() {
    let fixture = Fixture::new();
    fixture.setup_css_resources(CSS_A, CSS_B);
    let combined_url = format!("{DOMAIN}{}", combined_leaf(&[CSS_A, CSS_B], "0"));
    let (content, _) = fixture.serve_url(&combined_url).expect("served");
    assert_eq!(content, format!("{A_CSS_BODY}{B_CSS_BODY}").as_bytes());
}

#[test]
fn fetch_unhealthy() {
    let fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    fixture.setup_css_resources(CSS_A, CSS_B);
    let combined_url = format!("{DOMAIN}{}", combined_leaf(&[CSS_A, CSS_B], "0"));
    let (content, _) = fixture.serve_url(&combined_url).expect("served");
    assert_eq!(content, format!("{A_CSS_BODY}{B_CSS_BODY}").as_bytes());
}

#[test]
fn combine_css_md5() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(&fixture, "combine_css_md5", "", "", false);
}

/// Debug-fixture barrier run (`CssCombineFilterWithDebugTest` shape).
fn combine_css_debug(id: &str, barrier_text: &str, debug_text: &str) {
    let fixture = Fixture::with_md5();
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture.set_html_mimetype();
    combine_css_matrix(&fixture, id, barrier_text, debug_text, true);
}

#[test]
fn ie_directive_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    let barrier = "<!--[if IE]>\n  <link rel=\"stylesheet\" type=\"text/css\" href=\"http://graphics8.nytimes.com/css/0.1/screen/build/homepage/ie.css\">\n<![endif]-->";
    combine_css_matrix(&fixture, "combine_css_ie", barrier, "", true);
}

#[test]
fn ie_directive_barrier_debug() {
    combine_css_debug(
        "combine_css_ie",
        "<!--[if IE]>\n  <link rel=\"stylesheet\" type=\"text/css\" href=\"http://graphics8.nytimes.com/css/0.1/screen/build/homepage/ie.css\">\n<![endif]-->",
        "<!--combine_css: Could not combine over barrier: IE directive-->",
    );
}

#[test]
fn style_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(
        &fixture,
        "combine_css_style",
        "<style>a { color: red }</style>\n",
        "",
        true,
    );
}

#[test]
fn style_barrier_debug() {
    combine_css_debug(
        "combine_css_style",
        "<style>a { color: red }</style>\n",
        "<!--combine_css: Could not combine over barrier: inline style-->",
    );
}

#[test]
fn bogus_link_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(
        &fixture,
        "combine_css_bogus_link",
        "<link rel='stylesheet' href='crazee://big/blue/fake' type='text/css'>\n",
        "",
        true,
    );
}

#[test]
fn bogus_link_barrier_debug() {
    combine_css_debug(
        "combine_css_bogus_link",
        "<link rel='stylesheet' href='crazee://big/blue/fake' type='text/css'>\n",
        "<!--combine_css: Could not combine over barrier: resource not rewritable-->",
    );
}

#[test]
fn alternate_stylesheet_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(
        &fixture,
        "alternate_stylesheet_barrier",
        "<link rel='alternate stylesheet' type='text/css' href='a.css'>",
        "",
        true,
    );
}

#[test]
fn alternate_stylesheet_barrier_debug() {
    combine_css_debug(
        "alternate_stylesheet_barrier",
        "<link rel='alternate stylesheet' type='text/css' href='a.css'>",
        "<!--combine_css: Could not combine over barrier: custom or alternate stylesheet attribute-->",
    );
}

#[test]
fn non_standard_attributes_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(
        &fixture,
        "non_standard_attributes_barrier",
        "<link rel='stylesheet' type='text/css' href='a.css' foo='bar'>",
        "",
        true,
    );
}

#[test]
fn non_standard_attributes_barrier_debug() {
    combine_css_debug(
        "non_standard_attributes_barrier",
        "<link rel='stylesheet' type='text/css' href='a.css' foo='bar'>",
        "<!--combine_css: Could not combine over barrier: potentially non-combinable attribute: &#39;foo&#39;-->",
    );
}

#[test]
fn non_standard_attributes_barrier_with_id() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(
        &fixture,
        "non_standard_attributes_with_id_barrier",
        "<link rel='stylesheet' type='text/css' href='a.css' foo='bar' id=baz>",
        "",
        true,
    );
}

#[test]
fn non_standard_attributes_barrier_with_id_debug() {
    combine_css_debug(
        "non_standard_attributes_with_id_barrier",
        "<link rel='stylesheet' type='text/css' href='a.css' foo='bar' id=baz>",
        "<!--combine_css: Could not combine over barrier: potentially non-combinable attributes: &#39;foo&#39; and &#39;id&#39;-->",
    );
}

#[test]
fn non_standard_attributes_barrier_with_allowed_id() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .permit_ids_for_css_combining("b?z");
    combine_css_matrix(
        &fixture,
        "non_standard_attributes_with_allowed_id_barrier",
        "<link rel='stylesheet' type='text/css' href='a.css' foo='bar' id=baz>",
        "",
        true,
    );
}

#[test]
fn non_standard_attributes_barrier_with_allowed_id_debug() {
    let fixture = Fixture::with_md5();
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .permit_ids_for_css_combining("b?z");
    combine_css_matrix(
        &fixture,
        "non_standard_attributes_with_allowed_id_barrier",
        "<link rel='stylesheet' type='text/css' href='a.css' foo='bar' id=baz>",
        "<!--combine_css: Could not combine over barrier: potentially non-combinable attribute: &#39;foo&#39;-->",
        true,
    );
}

#[test]
fn id_alone_is_no_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    fixture
        .options
        .borrow_mut()
        .permit_ids_for_css_combining("b?z");
    fixture.setup_css_resources("a.css", "b.css");
    let output = fixture.parse(
        &format!("{DOMAIN}index.html"),
        "<link rel='stylesheet' type='text/css' href='a.css' id=baz><link rel='stylesheet' type='text/css' href='b.css'>",
    );
    assert_eq!(1, collect_css_links(output.as_bytes()).len());
}

#[test]
fn combine_css_with_import_in_first() {
    let fixture = Fixture::new();
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_import1",
        &[
            CssLinkIn {
                url: "1.css".to_string(),
                content: b"@Import '1a.css';".to_vec(),
                media: String::new(),
                supply_mock: true,
            },
            CssLinkIn {
                url: "2.css".to_string(),
                content: YELLOW.as_bytes().to_vec(),
                media: String::new(),
                supply_mock: true,
            },
            CssLinkIn {
                url: "3.css".to_string(),
                content: YELLOW.as_bytes().to_vec(),
                media: String::new(),
                supply_mock: true,
            },
        ],
    );
    assert_eq!(1, out.len());
}

#[test]
fn combine_css_with_import_in_second() {
    let fixture = Fixture::new();
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_import1",
        &[
            CssLinkIn {
                url: "1.css".to_string(),
                content: YELLOW.as_bytes().to_vec(),
                media: String::new(),
                supply_mock: true,
            },
            CssLinkIn {
                url: "2.css".to_string(),
                content: b"@Import '2a.css';".to_vec(),
                media: String::new(),
                supply_mock: true,
            },
            CssLinkIn {
                url: "3.css".to_string(),
                content: YELLOW.as_bytes().to_vec(),
                media: String::new(),
                supply_mock: true,
            },
        ],
    );
    assert_eq!("1.css", out[0]);
    assert_eq!(2, out.len());
}

#[test]
fn strip_bom() {
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{DOMAIN}a.css"), A_CSS_BODY.as_bytes());
    fixture.mock_css(
        &format!("{DOMAIN}b.css"),
        format!("\u{feff}{B_CSS_BODY}").as_bytes(),
    );
    let html_url = format!("{DOMAIN}bom.html");
    let input = format!(
        "<head>\n  {}\n  {}\n</head>\n",
        link("a.css", "", false),
        link("b.css", "", false)
    );
    let output = fixture.parse(&html_url, &input);
    let css_urls = collect_css_links(output.as_bytes());
    assert_eq!(1, css_urls.len());
    let base = ps_http::google_url::GoogleUrl::new(&html_url);
    let css_url = ps_http::google_url::GoogleUrl::resolve(&base, &css_urls[0]);
    let (actual, _) = fixture.serve_url(css_url.spec()).expect("served");
    assert!(!actual.windows(3).any(|w| w == b"\xef\xbb\xbf"));
    let reversed = format!(
        "<head>\n  {}\n  {}\n</head>\n",
        link("b.css", "", false),
        link("a.css", "", false)
    );
    let output = fixture.parse(&html_url, &reversed);
    let css_urls = collect_css_links(output.as_bytes());
    assert_eq!(1, css_urls.len());
    let css_url = ps_http::google_url::GoogleUrl::resolve(&base, &css_urls[0]);
    let (actual, _) = fixture.serve_url(css_url.spec()).expect("served");
    let first = actual
        .windows(3)
        .position(|w| w == b"\xef\xbb\xbf")
        .expect("bom");
    assert_eq!(0, first);
    let last = actual
        .windows(3)
        .rposition(|w| w == b"\xef\xbb\xbf")
        .expect("bom");
    assert_eq!(0, last);
}

#[test]
fn strip_bom_reconstruct() {
    let fixture = Fixture::new();
    let css_text = "div {background-image:url(fancy.png);}";
    fixture.mock_css(
        &format!("{TEST_DOMAIN}a.css"),
        format!("\u{feff}{css_text}").as_bytes(),
    );
    fixture.mock_css(
        &format!("{TEST_DOMAIN}b.css"),
        format!("\u{feff}{css_text}").as_bytes(),
    );
    let css_url = format!("{TEST_DOMAIN}{}", combined_leaf(&["a.css", "b.css"], "0"));
    let (out, _) = fixture.serve_url(&css_url).expect("served");
    assert_eq!(
        format!("\u{feff}{css_text}\n{css_text}").as_bytes(),
        out.as_slice()
    );
}

#[test]
fn combine_css_with_noscript_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    fixture.mock_css(&format!("{DOMAIN}d.css"), b".c4 {\n color: green;\n}\n");
    combine_css_matrix(
        &fixture,
        "combine_css_noscript",
        "<noscript>\n  <link rel='stylesheet' href='d.css' type='text/css'>\n</noscript>\n",
        "",
        true,
    );
}

#[test]
fn combine_css_with_fake_noscript_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(
        &fixture,
        "combine_css_fake_noscript",
        "<noscript>\n  <p>You have no scripts installed</p>\n</noscript>\n",
        "",
        false,
    );
}

#[test]
fn combine_css_with_media_barrier() {
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    fixture.mock_css(&format!("{DOMAIN}d.css"), b".c4 {\n color: green;\n}\n");
    combine_css_matrix(
        &fixture,
        "combine_css_media",
        "<link rel='stylesheet' href='d.css' type='text/css' media='print'>\n",
        "",
        true,
    );
}

#[test]
fn combine_css_with_non_media_barrier() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    fixture.mock_css(&format!("{DOMAIN}a.css"), A_CSS_BODY.as_bytes());
    fixture.mock_css(&format!("{DOMAIN}b.css"), B_CSS_BODY.as_bytes());
    fixture.mock_css(&format!("{DOMAIN}c.css"), C_CSS_BODY.as_bytes());
    fixture.mock_css(&format!("{DOMAIN}d.css"), b".c4 {\n color: green;\n}\n");
    let html_url = format!("{DOMAIN}no_media_barrier.html");
    let html_input = format!(
        "<head>\n  {}\n  {}\n  {}\n  {}\n</head>",
        link("a.css", "print", false),
        link("b.css", "print", false),
        link("c.css", "", false),
        link("d.css", "print", false),
    );
    let output = fixture.parse(&html_url, &html_input);
    let css_urls = collect_css_links(output.as_bytes());
    assert_eq!(3, css_urls.len());
    let expected_output = format!(
        "<head>\n  {}\n  \n  {}\n  {}\n</head>",
        link(&css_urls[0], "print", false),
        link("c.css", "", false),
        link("d.css", "print", false),
    );
    assert_eq!(output, expected_output);
}

/// Base-tag combining matrix (`CombineWithBaseTag`).
fn combine_with_base_tag(fixture: &Fixture, html_input: &str) -> Vec<String> {
    for (name, body) in [
        ("a.css", A_CSS_BODY),
        ("b.css", B_CSS_BODY),
        ("c.css", C_CSS_BODY),
    ] {
        fixture.mock_css(
            &format!("http://other_domain.test/foo/{name}"),
            body.as_bytes(),
        );
    }
    let output = fixture.parse(&format!("{DOMAIN}base_url.html"), html_input);
    collect_css_links(output.as_bytes())
}

#[test]
fn no_combine_css_base_url_out_of_order() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    let input = format!(
        "<head>\n  {}\n  <base href='http://other_domain.test/foo/'>\n  {}\n</head>\n",
        link("a.css", "", false),
        link("b.css", "", false),
    );
    let css_urls = combine_with_base_tag(&fixture, &input);
    assert_eq!(2, css_urls.len());
    assert_eq!(
        fixture.parse(&format!("{DOMAIN}base_url.html"), &input),
        input
    );
}

#[test]
fn combine_css_base_url_out_of_order() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    let input = format!(
        "<head>\n  {}\n  <base href='http://other_domain.test/foo/'>\n  {}\n  {}\n</head>\n",
        link("a.css", "", false),
        link("b.css", "", false),
        link("c.css", "", false),
    );
    let css_urls = combine_with_base_tag(&fixture, &input);
    assert_eq!(2, css_urls.len());
    let expected_output = format!(
        "<head>\n  {}\n  <base href='http://other_domain.test/foo/'>\n  {}\n  \n</head>\n",
        link("a.css", "", false),
        link(&css_urls[1], "", false),
    );
    assert_eq!(
        fixture.parse(&format!("{DOMAIN}base_url.html"), &input),
        expected_output
    );
    // Combined css_urls[1] stays relative, like the original URLs.
    let combine_url = format!("http://other_domain.test/foo/{}", css_urls[1]);
    assert_eq!(
        format!(
            "http://other_domain.test/foo/{}",
            combined_leaf(&["b.css", "c.css"], "0")
        ),
        combine_url
    );
    assert!(ps_http::google_url::GoogleUrl::new(&combine_url).is_web_valid());
}

#[test]
fn combine_css_absolute_base_url_out_of_order() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    let input = format!(
        "<head>\n  {}\n  <base href='http://other_domain.test/foo/'>\n  {}\n</head>\n",
        link("http://other_domain.test/foo/a.css", "", false),
        link("b.css", "", false),
    );
    let css_urls = combine_with_base_tag(&fixture, &input);
    assert_eq!(1, css_urls.len());
    // Combined css_urls[0] is absolute because the first original URL
    // was absolute, even though the next URL is relative.
    assert_eq!(
        format!(
            "http://other_domain.test/foo/{}",
            combined_leaf(&["a.css", "b.css"], "0")
        ),
        css_urls[0]
    );
    let expected_output = format!(
        "<head>\n  {}\n  <base href='http://other_domain.test/foo/'>\n  \n</head>\n",
        link(&css_urls[0], "", false),
    );
    assert_eq!(
        fixture.parse(&format!("{DOMAIN}base_url.html"), &input),
        expected_output
    );
    assert!(ps_http::google_url::GoogleUrl::new(&css_urls[0]).is_web_valid());
}

#[test]
fn combine_css_base_url_correctly_ordered() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    let input = format!(
        "<head>\n  <base href='http://other_domain.test/foo/'>\n  {}\n  {}\n</head>\n",
        link("a.css", "", false),
        link("b.css", "", false),
    );
    let css_urls = combine_with_base_tag(&fixture, &input);
    assert_eq!(1, css_urls.len());
    let expected_output = format!(
        "<head>\n  <base href='http://other_domain.test/foo/'>\n  {}\n  \n</head>\n",
        link(&css_urls[0], "", false),
    );
    assert_eq!(
        fixture.parse(&format!("{DOMAIN}base_url.html"), &input),
        expected_output
    );
    // Combined css_urls[0] stays relative, like the original URLs.
    let combine_url = format!("http://other_domain.test/foo/{}", css_urls[0]);
    assert_eq!(
        format!(
            "http://other_domain.test/foo/{}",
            combined_leaf(&["a.css", "b.css"], "0")
        ),
        combine_url
    );
    assert!(ps_http::google_url::GoogleUrl::new(&combine_url).is_web_valid());
}

#[test]
fn combine_css_no_input() {
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}b.css"), b".a {}");
    let html_input = "<head>\n  <link rel='stylesheet' href='a_broken.css' type='text/css'>\n  <link rel='stylesheet' href='b.css' type='text/css'>\n</head>\n<body><div class='c1'><div class='c2'><p>\n  Yellow on Blue</p></div></div></body>";
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}combine_css_missing_input"),
            html_input
        ),
        html_input
    );
}

#[test]
fn combine_css_xhtml() {
    test_xhtml(false);
}

#[test]
fn combine_css_xhtml_with_flush() {
    test_xhtml(true);
}

/// XHTML combining matrix (`TestXhtml`).
fn test_xhtml(flush: bool) {
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}a.css"), YELLOW.as_bytes());
    fixture.mock_css(&format!("{TEST_DOMAIN}b.css"), BLUE.as_bytes());
    let combined_url = combined_leaf(&["a.css", "b.css"], "0");
    *fixture.mime_is_xhtml.borrow_mut() = Some(true);
    let input_beginning = format!(
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\"><div>{}{}",
        link("a.css", "", false),
        link("b.css", "", false)
    );
    let driver = fixture.driver_for(TEST_DOMAIN);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(CssCombineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(TEST_DOMAIN.as_bytes(), false);
    parser.write(input_beginning.as_bytes());
    if flush {
        parser.flush();
    }
    parser.write(b"</div>");
    parser.finish();
    let output = String::from_utf8(sink.bytes()).unwrap();
    assert_eq!(
        format!(
            "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\"><div>{}</div>",
            link(&combined_url, "", true)
        ),
        output
    );
}

/// Decomposes a combined URL into base + segments (`DecomposeCombinedUrl`).
fn decompose_combined_url(base_url: &str, url: &str) -> (String, Vec<String>) {
    use ps_html::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    let base = ps_http::google_url::GoogleUrl::new(base_url);
    let resolved = ps_http::google_url::GoogleUrl::resolve(&base, url);
    assert!(resolved.is_web_valid(), "{url}");
    let leaf = resolved.leaf_with_query().to_string();
    let mut namer = ps_cache::namer::ResourceNamer::new();
    assert!(namer.decode_ignore_hash_and_signature(&leaf), "{url}");
    assert_eq!(namer.id(), "cc");
    let segments = MultipartEncoder
        .decode(namer.name().as_bytes())
        .expect("multipart")
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .collect();
    (resolved.all_except_leaf().to_string(), segments)
}

/// Barrier-matrix input (`CssLink::Add` shape).
struct CssLinkIn {
    url: String,
    content: Vec<u8>,
    media: String,
    supply_mock: bool,
}

fn self_link(href: &str, media: &str) -> String {
    link(href, media, false)
}

/// Barrier matrix (`BarrierTestHelper`), returning output hrefs.
fn barrier_test_helper(fixture: &Fixture, id: &str, links: &[CssLinkIn]) -> Vec<String> {
    let mut html_input = "<head>\n".to_string();
    for link in links {
        if !link.url.is_empty() {
            if link.supply_mock {
                fixture.mock_css(&format!("{}{}", TEST_DOMAIN, link.url), &link.content);
            }
            html_input.push_str(&format!("  {}\n", self_link(&link.url, &link.media)));
        } else {
            html_input.push_str(std::str::from_utf8(&link.content).unwrap());
        }
    }
    html_input.push_str(
        "</head>\n<body>\n  <div class='yellow'>\n    Hello, mod_pagespeed!\n  </div>\n</body>\n",
    );
    let output = fixture.parse(&format!("{TEST_DOMAIN}{id}.html"), &html_input);
    collect_css_links(output.as_bytes())
}

fn css_link_in(url: &str, content: &[u8], media: &str, supply_mock: bool) -> CssLinkIn {
    CssLinkIn {
        url: url.to_string(),
        content: content.to_vec(),
        media: media.to_string(),
        supply_mock,
    }
}

#[test]
fn combine_css_many_files() {
    let fixture = Fixture::new();
    let links: Vec<CssLinkIn> = (0..100)
        .map(|i| {
            css_link_in(
                &format!("styles/yellow{i}.css"),
                YELLOW.as_bytes(),
                "",
                true,
            )
        })
        .collect();
    let out = barrier_test_helper(&fixture, "combine_css_many_files", &links);
    assert_eq!(2, out.len());
    let (base, segments) = decompose_combined_url(TEST_DOMAIN, &out[0]);
    assert_eq!(format!("{TEST_DOMAIN}styles/"), base);
    assert_eq!(70, segments.len());
    let (base, segments) = decompose_combined_url(TEST_DOMAIN, &out[1]);
    assert_eq!(format!("{TEST_DOMAIN}styles/"), base);
    assert_eq!(30, segments.len());
}

#[test]
fn combine_css_many_files_one_orphan() {
    let fixture = Fixture::new();
    let mut links: Vec<CssLinkIn> = (0..70)
        .map(|i| {
            css_link_in(
                &format!("styles/yellow{i}.css"),
                YELLOW.as_bytes(),
                "",
                true,
            )
        })
        .collect();
    links.push(css_link_in(
        "styles/last_one.css",
        YELLOW.as_bytes(),
        "",
        true,
    ));
    let out = barrier_test_helper(&fixture, "combine_css_many_files", &links);
    assert_eq!(2, out.len());
    let (base, segments) = decompose_combined_url(TEST_DOMAIN, &out[0]);
    assert_eq!(format!("{TEST_DOMAIN}styles/"), base);
    assert_eq!(70, segments.len());
    assert_eq!("styles/last_one.css", out[1]);
}

#[test]
fn combine_css_missing_resource() {
    let fixture = Fixture::new();
    let a = format!("{DOMAIN}a.css");
    let c = format!("{DOMAIN}c.css");
    fixture.mock_css(&a, A_CSS_BODY.as_bytes());
    fixture.mock_css(&c, C_CSS_BODY.as_bytes());
    let (content, _) = fixture
        .serve_url(&format!(
            "{DOMAIN}{}",
            combined_leaf(&["a.css", "c.css"], "0")
        ))
        .expect("served");
    assert_eq!(content, format!("{A_CSS_BODY}{C_CSS_BODY}").as_bytes());
}

#[test]
fn combine_css_not_cached() {
    let fixture = Fixture::new();
    let out = barrier_test_helper(
        &fixture,
        "combine_css_not_cached",
        &[
            css_link_in("1.css", YELLOW.as_bytes(), "", true),
            css_link_in("2.css", YELLOW.as_bytes(), "", true),
            css_link_in("3.css", YELLOW.as_bytes(), "", false),
            css_link_in("4.css", YELLOW.as_bytes(), "", true),
        ],
    );
    assert_eq!(3, out.len());
    let (_, segments) = decompose_combined_url(TEST_DOMAIN, &out[0]);
    assert_eq!(2, segments.len());
    assert_eq!("1.css", segments[0]);
    assert_eq!("2.css", segments[1]);
    assert_eq!("3.css", out[1]);
    assert_eq!("4.css", out[2]);
}

#[test]
fn combine_style_tag() {
    let fixture = Fixture::new();
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_style",
        &[
            css_link_in("1.css", YELLOW.as_bytes(), "", true),
            css_link_in("2.css", YELLOW.as_bytes(), "", true),
            css_link_in("", b"<style>a { color: red }</style>\n", "", false),
            css_link_in("4.css", YELLOW.as_bytes(), "", true),
        ],
    );
    assert_eq!(2, out.len());
    let (_, segments) = decompose_combined_url(TEST_DOMAIN, &out[0]);
    assert_eq!(2, segments.len());
    assert_eq!("1.css", segments[0]);
    assert_eq!("2.css", segments[1]);
    assert_eq!("4.css", out[1]);
}

#[test]
fn no_absolutify_same_dir() {
    let fixture = Fixture::new();
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_style",
        &[
            css_link_in(
                "1.css",
                b".yellow {background-image: url('1.png');}\n",
                "",
                true,
            ),
            css_link_in(
                "2.css",
                b".yellow {background-image: url('2.png');}\n",
                "",
                true,
            ),
        ],
    );
    assert_eq!(1, out.len());
    // Note: the urls are not absolutified.
    let expected =
        ".yellow {background-image: url('1.png');}\n.yellow {background-image: url('2.png');}\n";
    let css_url = format!("{}{}", TEST_DOMAIN, out[0]);
    let (actual, _) = fixture.serve_url(&css_url).expect("served");
    assert_eq!(actual, expected.as_bytes());
}

#[test]
fn do_rewrite_for_different_dir() {
    let fixture = Fixture::new();
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_style",
        &[
            css_link_in(
                "1.css",
                b".yellow {background-image: url('1.png');}\n",
                "",
                true,
            ),
            css_link_in(
                "foo/2.css",
                b".yellow {background-image: url('2.png');}\n",
                "",
                true,
            ),
        ],
    );
    assert_eq!(1, out.len());
    let expected = ".yellow {background-image: url('1.png');}\n.yellow {background-image: url('foo/2.png');}\n";
    let css_url = format!("{}{}", TEST_DOMAIN, out[0]);
    let (actual, _) = fixture.serve_url(&css_url).expect("served");
    assert_eq!(actual, expected.as_bytes());
}

#[test]
fn shard_subresources() {
    let fixture = Fixture::with_md5();
    fixture.options.borrow_mut().add_domain(TEST_DOMAIN);
    fixture
        .options
        .borrow_mut()
        .add_shard("test.com", "shard1.com,shard2.com");
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_style",
        &[
            css_link_in(
                "1.css",
                b".yellow {background-image: url('1.png');}\n",
                "",
                true,
            ),
            css_link_in(
                "2.css",
                b".yellow {background-image: url('2.png');}\n",
                "",
                true,
            ),
        ],
    );
    assert_eq!(1, out.len());
    // Note: the urls are sharded to absolute domains.
    let expected = ".yellow {background-image: url('http://shard1.com/1.png');}\n.yellow {background-image: url('http://shard2.com/2.png');}\n";
    let (actual, _) = fixture.serve_url(&out[0]).expect("served");
    assert_eq!(actual, expected.as_bytes());
}

#[test]
fn fragment_reference_kept_with_shards() {
    let fixture = Fixture::with_md5();
    fixture.options.borrow_mut().add_domain(TEST_DOMAIN);
    fixture
        .options
        .borrow_mut()
        .add_shard("test.com", "shard1.com,shard2.com");
    let out = barrier_test_helper(
        &fixture,
        "combine_fragment",
        &[
            css_link_in(
                "1.css",
                b".a {mask: url('#m'); background-image: url('1.png');}\n",
                "",
                true,
            ),
            css_link_in("2.css", b".b {mask: url(#n);}\n", "", true),
        ],
    );
    assert_eq!(1, out.len());
    let expected = ".a {mask: url('#m'); background-image: url('http://shard1.com/1.png');}\n.b {mask: url(#n);}\n";
    let (actual, _) = fixture.serve_url(&out[0]).expect("served");
    assert_eq!(actual, expected.as_bytes());
}

#[test]
fn cross_across_paths_exceeding_url_size() {
    let fixture = Fixture::new();
    let long_name = "z".repeat(600);
    let out = barrier_test_helper(
        &fixture,
        "cross_paths",
        &[
            css_link_in(&format!("{long_name}/a.css"), YELLOW.as_bytes(), "", true),
            css_link_in(&format!("{long_name}/b.css"), BLUE.as_bytes(), "", true),
            css_link_in("sites/all/modules/ckeditor/ckeditor.css?3", b"z", "", true),
        ],
    );
    assert_eq!(2, out.len());
    let css_url = format!("{}{}", TEST_DOMAIN, out[0]);
    let (actual, _) = fixture.serve_url(&css_url).expect("served");
    let gurl = ps_http::google_url::GoogleUrl::new(&css_url);
    assert!(gurl.is_web_valid());
    assert_eq!(format!("{TEST_DOMAIN}{long_name}/"), gurl.all_except_leaf());
    let (_, segments) = decompose_combined_url(TEST_DOMAIN, &out[0]);
    assert_eq!(vec!["a.css".to_string(), "b.css".to_string()], segments);
    assert_eq!(actual, format!("{YELLOW}\n{BLUE}").as_bytes());
}

#[test]
fn cross_across_paths_disallowed() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option(
            "CombineAcrossPaths",
            "false",
            ps_config::scope::Scope::Directory,
        )
        .unwrap();
    let out = barrier_test_helper(
        &fixture,
        "cross_paths",
        &[
            css_link_in("a/a.css", YELLOW.as_bytes(), "", true),
            css_link_in("b/b.css", BLUE.as_bytes(), "", true),
        ],
    );
    assert_eq!(2, out.len());
    assert_eq!("a/a.css", out[0]);
    assert_eq!("b/b.css", out[1]);
}

#[test]
fn cross_mapped_domain() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("a.com", "b.com");
    fixture.mock_css("http://a.com/1.css", YELLOW.as_bytes());
    fixture.mock_css("http://b.com/2.css", BLUE.as_bytes());
    let out = barrier_test_helper(
        &fixture,
        "combine_css_with_style",
        &[
            css_link_in("http://a.com/1.css", YELLOW.as_bytes(), "", false),
            css_link_in("http://b.com/2.css", BLUE.as_bytes(), "", false),
        ],
    );
    assert_eq!(1, out.len());
    let (actual, _) = fixture.serve_url(&out[0]).expect("served");
    assert_eq!(
        format!("http://a.com/{}", combined_leaf(&["1.css", "2.css"], "0")),
        out[0]
    );
    assert_eq!(actual, format!("{YELLOW}\n{BLUE}").as_bytes());
}

#[test]
fn cross_unmapped_domain() {
    verify_cross_unmapped_domain_not_rewritten(&Fixture::new());
}

#[test]
fn cross_unmapped_domain_with_unauth_enabled() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_inline_unauthorized_resource_type(ps_config::options::Category::Stylesheet);
    verify_cross_unmapped_domain_not_rewritten(&fixture);
}

/// Unmapped cross-domain links never combine
/// (`VerifyCrossUnmappedDomainNotRewritten`).
fn verify_cross_unmapped_domain_not_rewritten(fixture: &Fixture) {
    fixture.options.borrow_mut().add_domain("a.com");
    fixture.options.borrow_mut().add_domain("b.com");
    fixture.mock_css("http://a.com/1.css", YELLOW.as_bytes());
    fixture.mock_css("http://b.com/2.css", BLUE.as_bytes());
    let out = barrier_test_helper(
        fixture,
        "combine_css_with_style",
        &[
            css_link_in("http://a.com/1.css", YELLOW.as_bytes(), "", false),
            css_link_in("http://b.com/2.css", BLUE.as_bytes(), "", false),
        ],
    );
    assert_eq!(2, out.len());
    assert_eq!("http://a.com/1.css", out[0]);
    assert_eq!("http://b.com/2.css", out[1]);
}

/// Unquoted CSS link (`CssLinkHref`).
fn css_link_href(url: &str) -> String {
    format!("<link rel=stylesheet href={url}>")
}

/// Absolute combination URL with hash "0" (`Encode` with
/// `kCssCombinerId`).
fn encode_combination(names: &[Vec<u8>]) -> String {
    use ps_html::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    format!(
        "{TEST_DOMAIN}{}.pagespeed.cc.0.css",
        String::from_utf8_lossy(&MultipartEncoder.encode(names))
    )
}

/// Fetches a resource URL, ignoring the body (`TryFetchResource`).
fn try_fetch_resource(fixture: &Fixture, url: &str) -> bool {
    fixture.serve_url(url).is_some()
}

/// Parses and expects byte-identical output (`ValidateNoChanges`).
fn validate_no_changes(fixture: &Fixture, id: &str, html: &str) {
    assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}{id}"), html), html);
}

#[test]
fn invalid_fetch_cache() {
    // Regression test for crashes when we're asked to do an invalid
    // fetch and then repeat it for a rewriter inside an XHTML-DTD page.
    let fixture = Fixture::new();
    fixture.mock_404(&format!("{TEST_DOMAIN}404a.css"));
    fixture.mock_404(&format!("{TEST_DOMAIN}404b.css"));
    assert!(!try_fetch_resource(
        &fixture,
        &encode_combination(&[b"404a.css".to_vec(), b"404b.css".to_vec()])
    ));
    validate_no_changes(
        &fixture,
        "invalid",
        &format!(
            "{XHTML_DTD}{}{}",
            css_link_href("404a.css"),
            css_link_href("404b.css")
        ),
    );
}

#[test]
fn fetch_with_no_inputs_is_refused() {
    // A combined-resource URL whose name lists no inputs has nothing to
    // reconstruct from; it must be refused before it reaches the combiner.
    let fixture = Fixture::new();
    assert!(!try_fetch_resource(&fixture, &encode_combination(&[])));
}

#[test]
fn fetch_result_does_not_decide_html_combination() {
    // Partition() does not put a stylesheet that starts with an @import after
    // another one.  A fetch of that combination is still served, but it must
    // leave the rewriting of a page with the same two links to Partition().
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), YELLOW.as_bytes());
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{CSS_B}"),
        b"@import url(c.css);\n.b {color: blue;}",
    );
    let url = encode_combination(&[CSS_A.as_bytes().to_vec(), CSS_B.as_bytes().to_vec()]);
    let (content, _) = fixture.serve_url(&url).expect("served");
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains(YELLOW));
    assert!(text.contains("@import"));
    validate_no_changes(
        &fixture,
        "import_in_second_after_fetch",
        &format!("{}{}", css_link_href(CSS_A), css_link_href(CSS_B)),
    );
}

#[test]
fn fetch_of_many_inputs_combines_in_order() {
    // The inputs of a fetch are loaded a few at a time; the combination still
    // holds all of them, in order.
    let fixture = Fixture::new();
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut expected = String::new();
    for i in 0..20 {
        let name = format!("{i}.css");
        let body = format!(".c{i} {{color: blue;}}\n");
        fixture.mock_css(&format!("{TEST_DOMAIN}{name}"), body.as_bytes());
        expected.push_str(&body);
        names.push(name.into_bytes());
    }
    let (content, _) = fixture
        .serve_url(&encode_combination(&names))
        .expect("served");
    assert_eq!(content, expected.as_bytes());
    assert_eq!(20u64, fixture.fetcher.fetch_count());
}

#[test]
fn fetch_of_inputs_beyond_cacheable_size_is_refused() {
    // A URL may name the same input many times.  Once the inputs loaded so far
    // exceed what could be cached, the rest are not loaded and the fetch is
    // refused.
    const MAX_BYTES: i64 = 1000;
    let fixture = Fixture::new();
    fixture.server.http_cache().set_max_body_bytes(MAX_BYTES);
    let body = " ".repeat((MAX_BYTES / 3) as usize);
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), body.as_bytes());
    let two = vec![CSS_A.as_bytes().to_vec(), CSS_A.as_bytes().to_vec()];
    assert!(try_fetch_resource(&fixture, &encode_combination(&two)));
    let before = fixture.fetcher.fetch_count();
    let hundred = vec![CSS_A.as_bytes().to_vec(); 100];
    assert!(!try_fetch_resource(&fixture, &encode_combination(&hundred)));
    // The metadata, the output and a few of the 100 inputs were looked up.
    assert!(fixture.fetcher.fetch_count() - before < 10);
}

/// Combines two stylesheets, munges the leaf suffix, and requires the
/// munged URL to still serve without polluting later rewrites
/// (`TestCorruptUrl`).
fn test_corrupt_url(new_suffix: &str) {
    let fixture = Fixture::new();
    let run = |fixture: &Fixture| {
        barrier_test_helper(
            fixture,
            "no_ext_corrupt",
            &[
                css_link_in("1.css", YELLOW.as_bytes(), "", true),
                css_link_in("2.css", YELLOW.as_bytes(), "", true),
            ],
        )
    };
    let out = run(&fixture);
    assert_eq!(1, out.len());
    let normal_url = out[0].clone();
    assert!(normal_url.ends_with(".css"), "{normal_url}");
    let munged_url = format!(
        "{}{new_suffix}",
        &normal_url[..normal_url.len() - ".css".len()]
    );
    let base = ps_http::google_url::GoogleUrl::new(TEST_DOMAIN);
    let munged = ps_http::google_url::GoogleUrl::resolve(&base, &munged_url);
    fixture
        .serve_url(munged.spec())
        .unwrap_or_else(|| panic!("serve {munged_url}"));
    // Now re-do it and make sure the new suffix didn't get stuck in the URL.
    let out = run(&fixture);
    assert_eq!(1, out.len());
    assert_eq!(out[0], normal_url);
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

#[test]
fn no_combine_parse_errors() {
    // Notice: This CSS file does not close its { and thus will break the
    // next stylesheet if they are combined, changing the page.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b"h1 { color: red");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b"h2 { color: blue; }");
    validate_no_changes(
        &fixture,
        "bad_parse",
        &format!("{}{}", css_link_href(CSS_A), css_link_href(CSS_B)),
    );
}

#[test]
fn no_combine_parse_errors_at_rule() {
    // Notice: This CSS file does not close its { and thus will break the
    // next stylesheet if they are combined, changing the page.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b"@foobar { color: red");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b"h2 { color: blue; }");
    validate_no_changes(
        &fixture,
        "bad_parse",
        &format!("{}{}", css_link_href(CSS_A), css_link_href(CSS_B)),
    );
}

#[test]
fn no_combine_parse_errors_unclosed_comment() {
    // Notice: This CSS file does not close its /* and thus would break the
    // next stylesheet if they were combined.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b"h1 { color: red; } /* ");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b"h2 { color: blue; }");
    validate_no_changes(
        &fixture,
        "bad_parse",
        &format!("{}{}", css_link_href(CSS_A), css_link_href(CSS_B)),
    );
}

#[test]
fn no_combine_parse_error_unclosed_import() {
    // Notice: This CSS file does not close its @import (with a ;)
    // and thus would break the next stylesheet if they were combined.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b"@import url(b.css)");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b"h2 { color: blue; }");
    validate_no_changes(
        &fixture,
        "bad_parse",
        &format!("{}{}", css_link_href(CSS_A), css_link_href(CSS_B)),
    );
}

#[test]
fn robustness_unclosed_string() {
    // Make sure we are robust against something with unterminated string.
    let fixture = Fixture::new();
    fixture.mock_css(
        &format!("{TEST_DOMAIN}{CSS_A}"),
        b"q::before {padding: 0px; \"content: foo;}",
    );
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b"h2 { color: blue; }");
    validate_no_changes(
        &fixture,
        "unterm_str",
        &format!("{}{}", css_link_href(CSS_A), css_link_href(CSS_B)),
    );
}

#[test]
fn deal_with_params() {
    let fixture = Fixture::new();
    fixture.set_html_mimetype();
    combine_css_with_names(
        &fixture,
        "with_params",
        "",
        "",
        false,
        "a.css?U",
        "b.css?rev=138",
        true,
    );
}

#[test]
fn alternate_stylesheets() {
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b"h1 { color: red; }");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b"h2 { color: blue; }");
    let combo = combined_leaf(&[CSS_A, CSS_B], "0");

    // Normal (persistent) CSS links are combined.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}persistent"),
            "<link rel='stylesheet' href='a.css'><link rel='stylesheet' href='b.css'>"
        ),
        format!("<link rel='stylesheet' href='{combo}'/>")
    );

    // Make sure we accept mixed case for the keyword.
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}mixed_case"),
            "<link rel=' StyleSheet' href='a.css'><link rel='styleSHEET  ' href='b.css'>"
        ),
        format!("<link rel=' StyleSheet' href='{combo}'/>")
    );

    // Preferred CSS links are not because we don't want to combine styles with
    // different titles.
    validate_no_changes(
        &fixture,
        "preferred_different",
        "<link rel='stylesheet' href='a.css' title='foo'><link rel='stylesheet' href='b.css' title='bar'>",
    );

    // TODO(sligocki): Should we combine ones with the same title?
    validate_no_changes(
        &fixture,
        "preferred_same",
        "<link rel='stylesheet' href='a.css' title='foo'><link rel='stylesheet' href='b.css' title='foo'>",
    );

    // Alternate CSS links, likewise.
    validate_no_changes(
        &fixture,
        "alternate_different",
        "<link rel='alternate stylesheet' href='a.css' title='foo'><link rel='alternate stylesheet' href='b.css' title='bar'>",
    );

    // TODO(sligocki): Should we combine ones with the same title?
    validate_no_changes(
        &fixture,
        "alternate_same",
        "<link rel='alternate stylesheet' href='a.css' title='foo'><link rel='alternate stylesheet' href='b.css' title='foo'>",
    );
}

#[test]
fn stats() {
    let fixture = Fixture::new();
    fixture.parse(&format!("{TEST_DOMAIN}stats_no_link"), "");
    // 0 opportunities with 0 links.
    assert_eq!(0, fixture.stat(COMBINE_OPPORTUNITIES));
    assert_eq!(0, fixture.stat(FILE_COUNT_REDUCTION));
    fixture.clear_stats();

    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b".a { color: red; }");
    fixture.parse(
        &format!("{TEST_DOMAIN}stats_one_link"),
        &link(CSS_A, "", false),
    );
    // 0 opportunities with 1 link.
    assert_eq!(0, fixture.stat(COMBINE_OPPORTUNITIES));
    assert_eq!(0, fixture.stat(FILE_COUNT_REDUCTION));
    fixture.clear_stats();

    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b".b { color: green; }");
    fixture.mock_css(&format!("{TEST_DOMAIN}c.css"), b".c { color: blue; }");
    fixture.parse(
        &format!("{TEST_DOMAIN}stats_3_links"),
        &format!(
            "{}{}{}",
            link(CSS_A, "", false),
            link(CSS_B, "", false),
            link("c.css", "", false)
        ),
    );
    // 2 opportunity with 3 links.
    assert_eq!(2, fixture.stat(COMBINE_OPPORTUNITIES));
    assert_eq!(2, fixture.stat(FILE_COUNT_REDUCTION));
    fixture.clear_stats();

    fixture.parse(
        &format!("{TEST_DOMAIN}stats_partial"),
        &format!(
            "{}{}{}",
            link(CSS_A, "", false),
            link(CSS_B, "", false),
            // media="print" so that it can't be combined.
            link("c.css", "print", false)
        ),
    );
    // 2 opportunities, but only one reduction because last is not combinable.
    assert_eq!(2, fixture.stat(COMBINE_OPPORTUNITIES));
    assert_eq!(1, fixture.stat(FILE_COUNT_REDUCTION));
    fixture.clear_stats();
}

// NOTE: `StatsWithDelay` waits for the async reconstruction epic (its
// wait-fetcher and callback stats have no sync equivalent here).

#[test]
fn combine_css_recombine() {
    // Make sure that if we re-parse the same html twice we do not
    // end up recomputing the CSS (and writing to cache) again. The
    // port asserts the observable half: byte-identical output with
    // no new fetches (the LRU insert counters have no equivalent).
    let fixture = Fixture::with_md5();
    fixture.set_html_mimetype();
    combine_css_matrix(&fixture, "combine_css_recombine", "", "", false);
    let fetches_before = fixture.fetcher.fetch_count();
    fixture.setup_css_resources(CSS_A, CSS_B);
    let html_input = format!(
        "<head>\n  {}\n  {}\n  <title>Hello, Instaweb</title>\n</head>\n<body>\n  <div class='c1'>\n    <div class='c2'>\n      Yellow on Blue\n    </div>\n  </div>\n  {}\n</body>\n",
        link(CSS_A, "", false),
        link(CSS_B, "", false),
        link("c.css", "", false),
    );
    let html_url = format!("{DOMAIN}combine_css_recombine.html");
    let first = fixture.parse(&html_url, &html_input);
    let second = fixture.parse(&html_url, &html_input);
    assert_eq!(first, second);
    assert_eq!(fetches_before, fixture.fetcher.fetch_count());
}

#[test]
fn two_combinations_twice() {
    // Two combinations around an unloadable stylesheet. The port covers
    // the sync half (stable partitions); the async/sync cache-key race
    // and explicit cache-key surgery have no equivalent here.
    let fixture = Fixture::new();
    fixture.mock_404(&format!("{TEST_DOMAIN}404.css"));
    let out = barrier_test_helper(
        &fixture,
        "two_comb",
        &[
            css_link_in(CSS_A, YELLOW.as_bytes(), "", true),
            css_link_in(CSS_B, YELLOW.as_bytes(), "", true),
            css_link_in("404.css", YELLOW.as_bytes(), "", false),
            css_link_in("c.css", YELLOW.as_bytes(), "", true),
            css_link_in("d.css", YELLOW.as_bytes(), "", true),
        ],
    );
    assert_eq!(3, out.len());
    assert_eq!(combined_leaf(&[CSS_A, CSS_B], "0"), out[0]);
    assert_eq!("404.css", out[1]);
    assert_eq!(combined_leaf(&["c.css", "d.css"], "0"), out[2]);

    // Now do it again: same partitions, still stable.
    let out = barrier_test_helper(
        &fixture,
        "two_comb",
        &[
            css_link_in(CSS_A, YELLOW.as_bytes(), "", true),
            css_link_in(CSS_B, YELLOW.as_bytes(), "", true),
            css_link_in("404.css", YELLOW.as_bytes(), "", false),
            css_link_in("c.css", YELLOW.as_bytes(), "", true),
            css_link_in("d.css", YELLOW.as_bytes(), "", true),
        ],
    );
    assert_eq!(3, out.len());
    assert_eq!(combined_leaf(&[CSS_A, CSS_B], "0"), out[0]);
    assert_eq!("404.css", out[1]);
    assert_eq!(combined_leaf(&["c.css", "d.css"], "0"), out[2]);
}

// NOTE: `OobSlotIndexMetadataTreatedAsMiss` corrupts metadata-cache
// entries, which have no equivalent in this sync port.

#[test]
fn xhtml_combine_link_closed() {
    // http://github.com/apache/incubator-pagespeed-mod/issues/306
    // XHTML text should not have unclosed links.  But if they do, like
    // in Issue 252, then we should leave them alone.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b".a {}");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b".b {}");
    let links = format!(
        "{}{}",
        link(CSS_A, "screen", true),
        link(CSS_B, "screen", true)
    );
    let combo = combined_leaf(&[CSS_A, CSS_B], "0");
    let html_format = |doctype: &str, links: &str| {
        format!(
            "<head>\n  {doctype}\n  {links}\n</head>\n<body><div class='c1'><div class='c2'><p>\n  Yellow on Blue</p></div></div></body>"
        )
    };
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}xhtml_combination_closed"),
            &html_format(XHTML_DTD, &links)
        ),
        html_format(XHTML_DTD, &link(&combo, "screen", true))
    );
}

#[test]
fn css_combine_across_proxy_domains() {
    // Tests Issue 600 in which CSS files in a MapProxyDomain were not
    // combined with local files but they should have been after mapping
    // into the same domain.
    let fixture = Fixture::new();
    // Proxy http://proxy.test/ onto http://test.com/proxied/
    let proxy_target = format!("{TEST_DOMAIN}proxied/");
    fixture
        .options
        .borrow_mut()
        .add_proxy_domain_mapping(&proxy_target, PROXY_MAP_DOMAIN);
    fixture.set_html_mimetype();

    // Create http://test.com/a.css and http://proxy.test/b.css
    let a_local_css_url = format!("{TEST_DOMAIN}{CSS_A}");
    let b_proxy_css_url = format!("{PROXY_MAP_DOMAIN}{CSS_B}");
    fixture.mock_css(&a_local_css_url, A_CSS_BODY.as_bytes());
    fixture.mock_css(&b_proxy_css_url, B_CSS_BODY.as_bytes());

    // Parse html that links to both css files.
    let html_input = format!(
        "<head>\n  {}\n  {}\n</head>\n",
        link(&a_local_css_url, "", false),
        link(&b_proxy_css_url, "", false),
    );
    let output = fixture.parse(&format!("{TEST_DOMAIN}base_url.html"), &html_input);

    // The two css files should be combined since they're now in the same domain.
    let css_urls = collect_css_links(output.as_bytes());
    assert_eq!(1, css_urls.len());
    // Encode doesn't allow a '/' as it expects a leaf so we have to add
    // proxied/ after the encoding occurs.
    let encoded = format!("{TEST_DOMAIN}{}", combined_leaf(&[CSS_A, CSS_B], "0"))
        .replace(CSS_B, "proxied,_b.css");
    assert_eq!(encoded, css_urls[0]);

    // Make sure we can fetch the combined resource.
    let (content, _) = fixture.serve_url(&css_urls[0]).expect("served");
    assert_eq!(content, format!("{A_CSS_BODY}{B_CSS_BODY}").as_bytes());

    // Now clear the cache and reconstruct it.
    fixture.http_backend.clear();
    fixture.metadata_backend.clear();
    let (content, _) = fixture.serve_url(&css_urls[0]).expect("reserve");
    assert_eq!(content, format!("{A_CSS_BODY}{B_CSS_BODY}").as_bytes());
}

#[test]
fn css_do_not_combine_across_not_proxied_domains() {
    // Dual to css_combine_across_proxy_domains to ensure that if no mapping
    // occurs we do not combine CSS files from different domains.
    let fixture = Fixture::new();
    fixture.set_html_mimetype();

    // Create http://test.com/a.css and http://proxy.test/b.css
    let a_local_css_url = format!("{TEST_DOMAIN}{CSS_A}");
    let b_proxy_css_url = format!("{PROXY_MAP_DOMAIN}{CSS_B}");
    fixture.mock_css(&a_local_css_url, A_CSS_BODY.as_bytes());
    fixture.mock_css(&b_proxy_css_url, B_CSS_BODY.as_bytes());

    // Parse html that links to both css files.
    let html_input = format!(
        "<head>\n  {}\n  {}\n</head>\n",
        link(&a_local_css_url, "", false),
        link(&b_proxy_css_url, "", false),
    );
    let output = fixture.parse(&format!("{TEST_DOMAIN}base_url.html"), &html_input);

    // The two css files should not be combined since they're not mapped to
    // the same domain.
    let css_urls = collect_css_links(output.as_bytes());
    assert_eq!(2, css_urls.len());
    assert_eq!(a_local_css_url, css_urls[0]);
    assert_eq!(b_proxy_css_url, css_urls[1]);
}

#[test]
fn debug_unauthorized_domain() {
    use ps_html::filters::debug::{DebugConfig, DebugFilter};
    use std::collections::BTreeSet;

    // Cross-domain links stay put; with the debug filter in the chain
    // the end-of-document summary still lands. The port asserts the
    // zero-duration message shape (frozen clock, like C++'s mock timer)
    // rather than the full page literal.
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_filter(Filter::Debug);
    fixture.options.borrow_mut().add_domain("a.com");
    fixture.options.borrow_mut().add_domain("b.com");
    fixture.mock_css("http://a.com/1.css", YELLOW.as_bytes());
    fixture.mock_css("http://b.com/2.css", BLUE.as_bytes());
    let html_input = format!(
        "<head>\n  {}\n  {}\n</head>\n<body>\n  <div class='yellow'>\n    Hello, mod_pagespeed!\n  </div>\n</body>\n",
        link("http://a.com/1.css", "", false),
        link("http://b.com/2.css", "", false),
    );
    let driver = fixture.driver_for(&format!("{TEST_DOMAIN}combine_css_with_style.html"));
    let config = DebugConfig {
        clock: Rc::new(RefCell::new(0)),
        critical_images: None,
        enabled_filters: String::new(),
        enabled_options: String::new(),
        beacon_enabled: false,
        disabled: Rc::new(RefCell::new(Vec::new())),
    };
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(CssCombineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(DebugFilter::new(config)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let url = format!("{TEST_DOMAIN}combine_css_with_style.html");
    parser.start_parse(url.as_bytes(), false);
    parser.write(html_input.as_bytes());
    parser.finish();
    let output = String::from_utf8(sink.bytes()).unwrap();
    let css_urls = collect_css_links(output.as_bytes());
    assert_eq!(2, css_urls.len());
    assert_eq!("http://a.com/1.css", css_urls[0]);
    assert_eq!("http://b.com/2.css", css_urls[1]);
    let message =
        DebugFilter::format_end_document_message(0, 0, 0, 0, 0, false, &BTreeSet::new(), &[]);
    let message = format!("{}-->", String::from_utf8_lossy(&message));
    assert!(output.contains(&message), "{output}");
}

#[test]
fn claims_xhtml_but_has_unclosed_link() {
    // https://github.com/apache/incubator-pagespeed-mod/issues/252
    // XHTML text should not have unclosed links.  But if they do, like
    // in Issue 252, then we should leave them alone.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), b".a {}");
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_B}"), b".b {}");
    let unclosed_links = format!(
        "  {}\n  <script type='text/javascript' src='c.js'></script>  {}",
        link(CSS_A, "", false),
        link(CSS_B, "", false)
    );
    let combo = combined_leaf(&[CSS_A, CSS_B], "0");
    let combination = format!(
        "  {}\n  <script type='text/javascript' src='c.js'></script>  ",
        link(&combo, "", true)
    );
    let html_format = |doctype: &str, links: &str| {
        format!("<head>\n  {doctype}\n  {links}\n</head>\n<body><div class='c1'><div class='c2'><p>\n  Yellow on Blue</p></div></div></body>")
    };
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}claims_xhtml_but_has_unclosed_links"),
            &html_format(XHTML_DTD, &unclosed_links)
        ),
        html_format(XHTML_DTD, &combination)
    );
}

/// Builds a stylesheet of exactly `size` bytes out of numbered rulesets
/// and trailing whitespace (`MakeCombineCssOfSize`).
fn make_combine_css_of_size(size: usize) -> Vec<u8> {
    let mut css = Vec::new();
    for i in 0.. {
        let rule = format!(".c{i}{{color:red}}\n");
        if css.len() + rule.len() > size {
            break;
        }
        css.extend_from_slice(rule.as_bytes());
    }
    css.resize(size, b' ');
    css
}

/// Links small.css and big.css and returns the rewritten stylesheet
/// URLs (`CombineWithBig`).
fn combine_with_big(fixture: &Fixture, id: &str, big_size: usize) -> Vec<String> {
    fixture.set_html_mimetype();
    fixture.mock_css(&format!("{DOMAIN}small.css"), b".s{color:blue}");
    fixture.mock_css(
        &format!("{DOMAIN}big.css"),
        &make_combine_css_of_size(big_size),
    );
    let output = fixture.parse(
        &format!("{DOMAIN}{id}.html"),
        "<link rel='stylesheet' type='text/css' href='small.css'><link rel='stylesheet' type='text/css' href='big.css'>",
    );
    collect_css_links(output.as_bytes())
}

#[test]
fn over_bound_is_not_combined() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    let css_urls = combine_with_big(&fixture, "over", 4096 + 1);
    assert_eq!(2, css_urls.len());
    assert_eq!("small.css", css_urls[0]);
    assert_eq!("big.css", css_urls[1]);
    assert!(fixture.stat(ps_html::filters::css::PARSE_MAX_BYTES_EXCEEDED) > 0);
}

#[test]
fn at_bound_is_combined() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_css_parse_max_bytes(4096);
    let css_urls = combine_with_big(&fixture, "at", 4096);
    assert_eq!(1, css_urls.len());
    assert!(css_urls[0].contains("small.css+big.css"), "{}", css_urls[0]);
    assert_eq!(
        0,
        fixture.stat(ps_html::filters::css::PARSE_MAX_BYTES_EXCEEDED)
    );
}

#[test]
fn fetch_with_oversized_name_is_refused() {
    // No URL with a segment longer than MaxSegmentLength is ever generated, so
    // none is served either, and nothing such a URL names is looked at.
    let fixture = Fixture::new();
    fixture.mock_css(&format!("{TEST_DOMAIN}{CSS_A}"), YELLOW.as_bytes());
    let many = vec![CSS_A.as_bytes().to_vec(); 200];
    let url = encode_combination(&many);
    assert!((fixture.options.borrow().max_url_segment_size() as usize) < url.len());
    assert!(!try_fetch_resource(&fixture, &url));
    assert_eq!(0u64, fixture.fetcher.fetch_count());
}
