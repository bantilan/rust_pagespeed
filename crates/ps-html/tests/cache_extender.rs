//! Cache-extender tests ported from `cache_extender_test.cc`.
//!
//! NOTE: inputs skip the body wrapper (it carries no URLs).
//! `MinimizeCacheHits` waits for the CSS outliner,
//! `DoNotExtendRewrittenCss` for the CSS rewriter, and `RenderCsp`
//! for the CSP subsystem; LRU-count and logging assertions wait for
//! cache statistics and the log record.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::filter::HtmlFilter;
use ps_html::filters::cache_extender::{CacheExtenderFilter, CACHE_EXTENSIONS, FILTER_ID};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
use ps_html::writer::WriterFilter;
use ps_http::content_type::ContentType;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const CSS_FILE: &str = "sub/a.css?v=1";
const CSS_TAIL: &str = "a.css?v=1";
const CSS_SUBDIR: &str = "sub/";
const CSS_DATA: &str = ".blue {color: blue; src: url(embedded.png);}";
const IMAGE_DATA: &str = "Not really JPEG but irrelevant for this test";
const JS_DATA: &str = "alert('hello, world!')";
const JS_INTROSPECTIVE: &str = "$('script')";
const SHORT_TTL_SEC: i64 = 100;
const MEDIUM_TTL_SEC: i64 = 100_000;
const LONG_TTL_SEC: i64 = 100_000_000;

static DEFAULT_ENCODER: DefaultEncoder = DefaultEncoder;

/// Recorded lawyer directive, replayed onto each driver (the C++
/// lawyer lives on shared options; the port's rides per driver).
#[derive(Clone, Debug)]
enum Mapping {
    Shard(String, String),
    Rewrite(String, String),
    Origin(String, String),
}

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    last_modified_on_origin: bool,
    mappings: RefCell<Vec<Mapping>>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_hasher(true)
    }

    /// Builds a fixture with the real MD5 namer (`UseMd5Hasher`).
    fn with_md5() -> Self {
        Self::with_hasher(false)
    }

    fn with_hasher(mock: bool) -> Self {
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
        Fixture {
            server: Rc::new(server),
            options: Rc::new(RefCell::new(RewriteOptions::new())),
            fetcher,
            last_modified_on_origin: true,
            mappings: RefCell::new(Vec::new()),
            http_backend,
            metadata_backend,
        }
    }

    /// Clears both caches (`lru_cache()->Clear()`).
    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
    }

    fn advance_ms(&self, delta_ms: i64) {
        self.server.timer().borrow_mut().advance_ms(delta_ms);
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let mut driver = ResourceDriver::new(self.server.clone(), self.options.clone(), url);
        for mapping in self.mappings.borrow().iter() {
            let ok = match mapping {
                Mapping::Shard(domain, shards) => {
                    driver.lawyer_mut().add_shard(domain, shards, None)
                }
                Mapping::Rewrite(to, from) => driver
                    .lawyer_mut()
                    .add_rewrite_domain_mapping(to, from, None),
                Mapping::Origin(to, from) => driver
                    .lawyer_mut()
                    .add_origin_domain_mapping(to, from, "", None),
            };
            assert!(ok, "mapping failed: {mapping:?}");
        }
        Rc::new(driver)
    }

    fn add_shard(&self, domain: &str, shards: &str) {
        self.mappings
            .borrow_mut()
            .push(Mapping::Shard(domain.to_string(), shards.to_string()));
    }

    fn add_rewrite_domain_mapping(&self, to: &str, from: &str) {
        self.mappings
            .borrow_mut()
            .push(Mapping::Rewrite(to.to_string(), from.to_string()));
    }

    fn add_origin_domain_mapping(&self, to: &str, from: &str) {
        self.mappings
            .borrow_mut()
            .push(Mapping::Origin(to.to_string(), from.to_string()));
    }

    /// Scripts a cacheable fetch, retaining Last-Modified
    /// (`InitResource`).
    fn init_resource(&self, name: &str, content_type: ContentType, content: &str, ttl_sec: i64) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server
            .set_default_long_cache_headers(Some(content_type), "", "", &mut headers);
        headers.set_date_and_caching(self.server.now_ms(), ttl_sec * 1000, "");
        if !self.last_modified_on_origin {
            headers.headers_mut().remove_all("Last-Modified");
            headers.headers_mut().remove_all("Etag");
        }
        headers.compute_caching();
        self.fetcher
            .set_response(&format!("{TEST_DOMAIN}{name}"), headers, content.as_bytes());
    }

    /// Scripts a resource under another host (rewrite/shard serving).
    fn init_resource_new_host(&self, host: &str, name: &str, mime: &str, content: &str) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type(mime),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), SHORT_TTL_SEC * 1000, "");
        headers.headers_mut().remove_all("Last-Modified");
        headers.headers_mut().remove_all("Etag");
        headers.compute_caching();
        self.fetcher
            .set_response(&format!("{host}{name}"), headers, content.as_bytes());
    }

    /// Default mock set (`InitTestWithoutFilters`).
    fn init_test_without_filters(&self, ttl_sec: i64) {
        self.init_resource(
            CSS_FILE,
            ps_http::content_type::mime_to_content_type("text/css").unwrap(),
            CSS_DATA,
            ttl_sec,
        );
        self.init_resource(
            "b.jpg",
            ps_http::content_type::mime_to_content_type("image/jpeg").unwrap(),
            IMAGE_DATA,
            ttl_sec,
        );
        self.init_resource(
            "c.js",
            ps_http::content_type::mime_to_content_type("application/javascript").unwrap(),
            JS_DATA,
            ttl_sec,
        );
        self.init_resource(
            "introspective.js",
            ps_http::content_type::mime_to_content_type("application/javascript").unwrap(),
            JS_INTROSPECTIVE,
            ttl_sec,
        );
        self.cache_extensions().clear();
    }

    /// Enables extension and mocks (`InitTest`).
    fn init_test(&self, ttl_sec: i64) {
        self.options.borrow_mut().enable_extend_cache_filters();
        self.init_test_without_filters(ttl_sec);
    }

    fn cache_extensions(&self) -> Arc<ps_stats::core::Variable> {
        self.server.variable(CACHE_EXTENSIONS)
    }

    /// Expected extended leaf (`Encode`).
    fn encode(&self, path: &str, name: &str, ext: &str) -> String {
        let encoded = DEFAULT_ENCODER.encode(&[name.as_bytes().to_vec()]);
        let hash = self.server.hasher().hash(b"");
        format!(
            "{}{}.pagespeed.{}.{}.{}",
            path,
            String::from_utf8_lossy(&encoded),
            FILTER_ID,
            hash,
            ext
        )
    }

    /// Parses one document through the extender.
    fn parse(&self, case: &str, html: &str) -> String {
        let url = format!("{TEST_DOMAIN}{case}.html");
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(
            Box::new(CacheExtenderFilter::new(self.driver_for(&url))) as Box<dyn HtmlFilter>
        );
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
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

    /// Serves a relative URL: the body on success.
    fn serve_relative_url(&self, rel_path: &str) -> Option<Vec<u8>> {
        self.serve_url(&format!("{TEST_DOMAIN}{rel_path}"))
            .map(|(body, _)| body)
    }

    fn validate_expected(&self, case: &str, input: &str, expected: &str) {
        assert_eq!(self.parse(case, input), expected, "case {case}");
    }
}

/// Three-resource document (`GenerateHtml`, debug off).
fn generate_html(fixture: &Fixture, css: &str, img: &str, js: &str) -> String {
    let _ = fixture;
    format!(
        "<link rel='stylesheet' href='{css}' type='text/css'>\n<img src='{img}'/>\n<script type='text/javascript' src='{js}'></script>\n"
    )
}

#[test]
fn do_extend() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    let input = generate_html(&fixture, CSS_FILE, "b.jpg", "c.js");
    let expected = generate_html(
        &fixture,
        &fixture.encode(CSS_SUBDIR, CSS_TAIL, "css"),
        &fixture.encode("", "b.jpg", "jpg"),
        &fixture.encode("", "c.js", "js"),
    );
    for i in 0..3 {
        fixture.validate_expected("do_extend", &input, &expected);
        assert_eq!(fixture.cache_extensions().get(), (i + 1) * 3);
    }
    // NOTE: LRU hit/miss/insert counts wait for cache statistics.
    assert_eq!(fixture.fetcher.fetch_count(), 3);
}

/// Preserve-URLs matrix (`CacheExtenderTestPreserveURLs::TestExtend`).
fn test_extend_preserve(img_extend: bool, css_extend: bool, js_extend: bool) {
    use ps_config::filter::Filter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheCss);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheImages);
    fixture
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::ExtendCacheScripts);
    if !img_extend {
        fixture.options.borrow_mut().set_image_preserve_urls(true);
    }
    if !css_extend {
        fixture.options.borrow_mut().set_css_preserve_urls(true);
    }
    if !js_extend {
        fixture.options.borrow_mut().set_js_preserve_urls(true);
    }
    fixture.init_test_without_filters(SHORT_TTL_SEC);
    let css = if css_extend {
        fixture.encode(CSS_SUBDIR, CSS_TAIL, "css")
    } else {
        CSS_FILE.to_string()
    };
    let img = if img_extend {
        fixture.encode("", "b.jpg", "jpg")
    } else {
        "b.jpg".to_string()
    };
    let js = if js_extend {
        fixture.encode("", "c.js", "js")
    } else {
        "c.js".to_string()
    };
    fixture.validate_expected(
        "do_extend",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(&fixture, &css, &img, &js),
    );
}

#[test]
fn preserve_image_urls_on() {
    test_extend_preserve(false, true, true);
}

#[test]
fn preserve_css_urls_on() {
    test_extend_preserve(true, false, true);
}

#[test]
fn preserve_js_urls_on() {
    test_extend_preserve(true, true, false);
}

#[test]
fn preserve_all_urls_on() {
    test_extend_preserve(false, false, false);
}

#[test]
fn dont_extend_introspective_default() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    let tag = "<script src=\"introspective.js\"></script>";
    fixture.validate_expected("dont_extend_introspective_js", tag, tag);
    assert_eq!(fixture.cache_extensions().get(), 0);
}

#[test]
fn do_extend_introspective_javascript() {
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
    fixture.init_test(SHORT_TTL_SEC);
    let input = "<script src=\"introspective.js\"></script>";
    let expected = format!(
        "<script src=\"{}\"></script>",
        fixture.encode("", "introspective.js", "js")
    );
    fixture.validate_expected("do_extend_introspective_js", input, &expected);
}

#[test]
fn do_extend_link_rel_case_insensitive() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    let input = format!("<link rel=StyleSheet href='{CSS_FILE}'>");
    let expected = format!(
        "<link rel=StyleSheet href='{}'>",
        fixture.encode(CSS_SUBDIR, CSS_TAIL, "css")
    );
    fixture.validate_expected("extend_ci", &input, &expected);
}

#[test]
fn do_extend_for_images_only() {
    use ps_config::filter::Filter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(Filter::ExtendCacheImages);
    let mut fixture = fixture;
    fixture.last_modified_on_origin = false;
    fixture.init_test_without_filters(SHORT_TTL_SEC);
    let input = generate_html(&fixture, CSS_FILE, "b.jpg", "c.js");
    let expected = generate_html(
        &fixture,
        CSS_FILE,
        &fixture.encode("", "b.jpg", "jpg"),
        "c.js",
    );
    for i in 0..3 {
        fixture.validate_expected("do_extend", &input, &expected);
        assert_eq!(fixture.cache_extensions().get(), i + 1);
    }
}

#[test]
fn url_too_long() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    let max_segment = fixture.options.borrow().max_url_segment_size() as usize;
    let long_string = "z".repeat(max_segment + 1);
    let css_name = format!("style.css?z={long_string}");
    let jpg_name = format!("image.jpg?z={long_string}");
    let js_name = format!("script.js?z={long_string}");
    fixture.init_resource(
        &css_name,
        ps_http::content_type::mime_to_content_type("text/css").unwrap(),
        CSS_DATA,
        SHORT_TTL_SEC,
    );
    fixture.init_resource(
        &jpg_name,
        ps_http::content_type::mime_to_content_type("image/jpeg").unwrap(),
        IMAGE_DATA,
        SHORT_TTL_SEC,
    );
    fixture.init_resource(
        &js_name,
        ps_http::content_type::mime_to_content_type("application/javascript").unwrap(),
        JS_DATA,
        SHORT_TTL_SEC,
    );
    // If the names were not too long, this would rewrite (as in DoExtend).
    let html = generate_html(&fixture, &css_name, &jpg_name, &js_name);
    fixture.validate_expected("url_too_long", &html, &html);
    assert_eq!(fixture.cache_extensions().get(), 0);
}

#[test]
fn no_input_resource() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    // Test for not crashing on bad/disallowed URLs.
    let html = generate_html(
        &fixture,
        "swly://example.com/sub/a.css",
        "http://evil.com/b.jpg",
        "http://moreevil.com/c.js",
    );
    fixture.validate_expected("bad url", &html, &html);
}

#[test]
fn no_extend_already_cached_properly() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    let html = generate_html(&fixture, CSS_FILE, "b.jpg", "c.js");
    fixture.validate_expected("no_extend_cached_properly", &html, &html);
    assert_eq!(fixture.cache_extensions().get(), 0);
}

#[test]
fn extend_if_sharded() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    fixture.add_shard(TEST_DOMAIN, "shard0.com,shard1.com");
    // shard0 is always selected because the mock hasher returns 0.
    fixture.validate_expected(
        "extend_if_sharded",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(
            &fixture,
            &fixture.encode("http://shard0.com/sub/", CSS_TAIL, "css"),
            &fixture.encode("http://shard0.com/", "b.jpg", "jpg"),
            &fixture.encode("http://shard0.com/", "c.js", "js"),
        ),
    );
}

/// Stylesheet link (`CssLinkHref`).
fn css_link_href(href: &str) -> String {
    format!("<link rel='stylesheet' href='{href}' type='text/css'>\n")
}

#[test]
fn fragment_reference_kept_on_rewrite_domain() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    fixture.add_rewrite_domain_mapping("cdn.com", TEST_DOMAIN);
    let fragment_css = ".m { mask: url(#m) } .n { mask: url('#n') }";
    fixture.init_resource(
        "sub/f.css",
        ps_http::content_type::mime_to_content_type("text/css").unwrap(),
        fragment_css,
        LONG_TTL_SEC,
    );
    let out_css = fixture.encode("http://cdn.com/sub/", "f.css", "css");
    fixture.validate_expected(
        "fragment",
        &css_link_href("sub/f.css"),
        &css_link_href(&out_css),
    );
    // The extended stylesheet is built on request; ask for it on the
    // origin host, where its input is.
    let content = fixture
        .serve_url(&fixture.encode(&format!("{TEST_DOMAIN}sub/"), "f.css", "css"))
        .expect("served");
    assert_eq!(content.0, fragment_css.as_bytes());
}

#[test]
fn extend_if_origin_mapped_https() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    fixture.add_origin_domain_mapping(TEST_DOMAIN, "https://cdn.com");
    fixture.validate_expected(
        "extend_if_origin_mapped_https",
        &generate_html(
            &fixture,
            "https://cdn.com/sub/a.css?v=1",
            "https://cdn.com/b.jpg",
            "https://cdn.com/c.js",
        ),
        &generate_html(
            &fixture,
            &fixture.encode("https://cdn.com/sub/", CSS_TAIL, "css"),
            &fixture.encode("https://cdn.com/", "b.jpg", "jpg"),
            &fixture.encode("https://cdn.com/", "c.js", "js"),
        ),
    );
}

#[test]
fn no_link_canonical_if_domain_map_complexity() {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    fixture.add_rewrite_domain_mapping("cdn.com", TEST_DOMAIN);
    fixture.add_origin_domain_mapping(TEST_DOMAIN, "http://cdn.com");
    fixture.validate_expected(
        "extend_if_rewritten",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(
            &fixture,
            &fixture.encode("http://cdn.com/sub/", CSS_TAIL, "css"),
            &fixture.encode("http://cdn.com/", "b.jpg", "jpg"),
            &fixture.encode("http://cdn.com/", "c.js", "js"),
        ),
    );
    // No link header, due to the rewrite mapping.
    let (_, headers) = fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "b.jpg", "jpg"))
        .expect("served");
    assert!(!headers.headers().has("Link"));
    // Likewise for reconstruction.
    fixture.clear_caches();
    let (_, headers) = fixture
        .serve_url(&fixture.encode("http://cdn.com/", "b.jpg", "jpg"))
        .expect("served");
    assert!(!headers.headers().has("Link"));
}

/// Canonical-on-fallback matrix (`TestCanonicalOnFallback`).
fn test_canonical_on_fallback(fallback_to_pagespeed_resource: bool) {
    let fixture = Fixture::new();
    fixture.init_test(if fallback_to_pagespeed_resource {
        SHORT_TTL_SEC
    } else {
        LONG_TTL_SEC
    });
    // Wrong hash forces the pagespeed fallback; right hash with a
    // long TTL fails the rewrite and falls back to the input.
    let hash = if fallback_to_pagespeed_resource {
        "1"
    } else {
        "0"
    };
    let out_css = fixture.encode(&format!("{TEST_DOMAIN}sub/"), CSS_TAIL, "css");
    // NOTE: the C++ helper passes kCssTail ("a.css?v=1") as the name;
    // the port's encoder self-consistently matches.
    let out_jpg = fixture.encode(TEST_DOMAIN, "b.jpg", "jpg");
    let out_css = out_css.replacen(".ce.0.css", &format!(".ce.{hash}.css"), 1);
    let out_jpg = out_jpg.replacen(".ce.0.jpg", &format!(".ce.{hash}.jpg"), 1);

    let (_, headers) = fixture.serve_url(&out_css).expect("css served");
    // CSS isn't expected to get a Link: header.
    assert!(!headers.headers().has("Link"));

    let (_, headers) = fixture.serve_url(&out_jpg).expect("jpg served");
    // JPEG should get a Link: header (exactly one).
    assert_eq!(
        headers.headers().lookup1("Link"),
        Some(ResponseHeaders::rel_canonical_header_value(&format!(
            "{TEST_DOMAIN}b.jpg"
        )))
    );

    // Should also propagate existing ones (and not add incompatible ones).
    fixture.clear_caches();
    fixture.fetcher.add_response_header(
        &format!("{TEST_DOMAIN}b.jpg"),
        "Link",
        &ResponseHeaders::rel_canonical_header_value(&format!("{TEST_DOMAIN}notb.jpg")),
    );
    let (_, headers) = fixture.serve_url(&out_jpg).expect("jpg served");
    assert_eq!(
        headers.headers().lookup1("Link"),
        Some(ResponseHeaders::rel_canonical_header_value(&format!(
            "{TEST_DOMAIN}notb.jpg"
        )))
    );
}

#[test]
fn canonical_on_fallback_to_other_pagespeed() {
    test_canonical_on_fallback(true);
}

#[test]
fn canonical_on_fallback_to_input() {
    test_canonical_on_fallback(false);
}

#[test]
fn extend_if_sharded_and_rewritten() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    fixture.add_rewrite_domain_mapping("cdn.com", TEST_DOMAIN);
    // Domain-rewriting is performed first. Then we shard.
    fixture.add_shard("cdn.com", "shard0.com,shard1.com");
    fixture.validate_expected(
        "extend_if_sharded_and_rewritten",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(
            &fixture,
            &fixture.encode("http://shard0.com/sub/", CSS_TAIL, "css"),
            &fixture.encode("http://shard0.com/", "b.jpg", "jpg"),
            &fixture.encode("http://shard0.com/", "c.js", "js"),
        ),
    );
}

#[test]
fn extend_if_sharded_to_https() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    // This Origin Mapping ensures any fetches are converted to http so work.
    fixture.add_origin_domain_mapping(TEST_DOMAIN, "https://test.com");
    fixture.add_shard("https://test.com", "https://shard0.com,https://shard1.com");
    fixture.validate_expected(
        "extend_if_sharded_to_https",
        &generate_html(
            &fixture,
            "https://test.com/sub/a.css?v=1",
            "https://test.com/b.jpg",
            "https://test.com/c.js",
        ),
        &generate_html(
            &fixture,
            &fixture.encode("https://shard0.com/sub/", CSS_TAIL, "css"),
            &fixture.encode("https://shard0.com/", "b.jpg", "jpg"),
            &fixture.encode("https://shard0.com/", "c.js", "js"),
        ),
    );
}

#[test]
fn extend_if_sharded_and_rewriting_and_mapping_https() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    for (name, mime, data) in [
        (CSS_FILE, "text/css", CSS_DATA),
        ("b.jpg", "image/jpeg", IMAGE_DATA),
        ("c.js", "application/javascript", JS_DATA),
    ] {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        fixture.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type(mime),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(fixture.server.now_ms(), LONG_TTL_SEC * 1000, "");
        headers.headers_mut().remove_all("Last-Modified");
        headers.headers_mut().remove_all("Etag");
        headers.compute_caching();
        fixture.fetcher.set_response(
            &format!("http://www.test.com/{name}"),
            headers,
            data.as_bytes(),
        );
    }
    fixture.add_rewrite_domain_mapping("http://cdn.com", "http://test.com,http://www.test.com");
    fixture.add_rewrite_domain_mapping("https://cdn.com", "https://test.com,https://www.test.com");
    fixture.add_shard("http://cdn.com", "http://s1.cdn.com,http://s2.cdn.com");
    fixture.add_shard("https://cdn.com", "https://s1.cdn.com,https://s2.cdn.com");
    fixture.add_origin_domain_mapping("http://test.com", "https://test.com");
    fixture.add_origin_domain_mapping("http://test.com", "https://www.test.com");
    for (scheme, host) in [
        ("http", "test.com"),
        ("https", "test.com"),
        ("http", "www.test.com"),
        ("https", "www.test.com"),
    ] {
        let base = format!("{scheme}://{host}");
        let shard_base = format!("{scheme}://s1.cdn.com");
        fixture.validate_expected(
            &format!("extend_sharded_mapping_{scheme}_{host}"),
            &generate_html(
                &fixture,
                &format!("{base}/sub/a.css?v=1"),
                &format!("{base}/b.jpg"),
                &format!("{base}/c.js"),
            ),
            &generate_html(
                &fixture,
                &fixture.encode(&format!("{shard_base}/sub/"), CSS_TAIL, "css"),
                &fixture.encode(&format!("{shard_base}/"), "b.jpg", "jpg"),
                &fixture.encode(&format!("{shard_base}/"), "c.js", "js"),
            ),
        );
    }
}

#[test]
fn do_not_extend_oversized_gzip() {
    // A valid gzip body inflating past the handled cap must not
    // extend: the still-compressed bytes would serve as plain content.
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    let cap = fixture
        .options
        .borrow()
        .max_cacheable_response_content_length() as usize;
    let js = vec![b'a'; cap + 1];
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    let mut compressed = js.clone();
    assert!(ps_http::fetch::gzip_value(&mut headers, &mut compressed, 6));
    assert!(compressed.len() < js.len() / 100);
    let mut response = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("application/javascript"),
        "",
        "",
        &mut response,
    );
    response.set_date_and_caching(fixture.server.now_ms(), MEDIUM_TTL_SEC * 1000, "");
    response.compute_caching();
    response.headers_mut().add("Content-Encoding", "gzip");
    response.compute_caching();
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}c.js"), response, &compressed);
    let html = "<script type='text/javascript' src='c.js'></script>\n";
    fixture.validate_expected("no_extend_oversized_gzip", html, html);
    assert_eq!(fixture.cache_extensions().get(), 0);
}

#[test]
fn no_extend_origin_uncacheable() {
    // NOTE: the C++ case also asserts the "Uncacheable content"
    // debug comment (debug off here, so none emits).
    let fixture = Fixture::new();
    fixture.init_test(0);
    let html = generate_html(&fixture, CSS_FILE, "b.jpg", "c.js");
    fixture.validate_expected("no_extend_origin_not_cacheable", &html, &html);
    assert_eq!(fixture.cache_extensions().get(), 0);
}

/// Enables debug mode (`EnableDebug`).
fn enable_debug(fixture: &Fixture) {
    fixture
        .options
        .borrow_mut()
        .enable_filter(ps_config::filter::Filter::Debug);
}

#[test]
fn handle_404() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    enable_debug(&fixture);
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
    let link = "<link rel=stylesheet href='404.css'>";
    let expected =
        format!("{link}<!--4xx status code, preventing rewriting of {TEST_DOMAIN}404.css-->");
    for _ in 0..2 {
        // Validate twice to make sure caching doesn't break it.
        fixture.validate_expected("404", link, &expected);
    }
    // Check with metadata cache flushed but not http cache.
    fixture
        .metadata_backend
        .delete_with_prefix_for_testing("rname/");
    fixture.validate_expected("404", link, &expected);
}

#[test]
fn handle_503() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    enable_debug(&fixture);
    let url = format!("{TEST_DOMAIN}503.css");
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    headers.set_first_line(1, 1, 503, "Service Unavailable");
    headers.compute_caching();
    fixture.fetcher.set_response(&url, headers, b"");
    let link = "<link rel=stylesheet href='503.css'>";
    let expected = format!("{link}<!--Fetch failure, preventing rewriting of {url}-->");
    for _ in 0..2 {
        // Validate twice to make sure caching doesn't break it.
        fixture.validate_expected("503", link, &expected);
    }
    // Check with metadata cache flushed but not http cache.
    fixture
        .metadata_backend
        .delete_with_prefix_for_testing("rname/");
    fixture.validate_expected("503", link, &expected);
}

/// Unauthorized matrix (`VerifyUnauthorizedResourcesNotExtended`).
fn verify_unauthorized_resources_not_extended(unauth_enabled: bool) {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    if unauth_enabled {
        // NOTE: inline-unauthorized types predate the port's allow
        // groups; creation still refuses unauthorized domains.
        let _ = unauth_enabled;
    }
    enable_debug(&fixture);
    for (name, mime, data) in [
        (
            "http://unauth.example.com/unauth.js",
            "application/javascript",
            JS_DATA,
        ),
        ("http://unauth.example.com/unauth.css", "text/css", CSS_DATA),
    ] {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        fixture.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type(mime),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(fixture.server.now_ms(), SHORT_TTL_SEC * 1000, "");
        headers.headers_mut().remove_all("Last-Modified");
        headers.headers_mut().remove_all("Etag");
        headers.compute_caching();
        fixture.fetcher.set_response(name, headers, data.as_bytes());
    }
    let js = "<script src='http://unauth.example.com/unauth.js'></script>";
    let css = "<link rel=stylesheet href='http://unauth.example.com/unauth.css'>";
    let comment = "<!--The preceding resource was not rewritten because its domain (unauth.example.com) is not authorized-->";
    fixture.validate_expected(
        "dont_extend_unauth",
        &format!("{js}{css}"),
        &format!("{js}{comment}{css}{comment}"),
    );
    assert_eq!(fixture.cache_extensions().get(), 0);
}

#[test]
fn do_not_extend_unauthorized_resources() {
    verify_unauthorized_resources_not_extended(false);
}

#[test]
fn do_not_extend_unauthorized_resources_with_unauth_enabled() {
    verify_unauthorized_resources_not_extended(true);
}

#[test]
fn dont_extend_introspective_debug() {
    let fixture = Fixture::new();
    enable_debug(&fixture);
    fixture.init_test(SHORT_TTL_SEC);
    let tag = "<script src=\"introspective.js\"></script>";
    let expected = format!(
        "{tag}<!--This script contains introspective JavaScript and is unsafe to replace.-->"
    );
    fixture.validate_expected("dont_extend_introspective_js", tag, &expected);
    assert_eq!(fixture.cache_extensions().get(), 0);
}

/// Served-files matrix (`TestServeFiles`).
fn test_serve_files(fixture: &Fixture, last_modified_on_origin: bool) {
    let expected_last_modified_ms = if last_modified_on_origin { START_MS } else { 0 };

    // To ensure there's no absolutification (below) of embedded.png's
    // URL in the served CSS file, we have to serve it from test.com.
    fixture.advance_ms(10_000);
    let (content, headers) = fixture
        .serve_url(&fixture.encode(&format!("{TEST_DOMAIN}sub/"), CSS_TAIL, "css"))
        .expect("css served");
    let fetch1_ms = fixture.server.now_ms();
    assert_eq!(headers.last_modified_time_ms(), expected_last_modified_ms);
    assert_eq!(
        headers.headers().has("Last-Modified"),
        last_modified_on_origin
    );
    assert!(fetch1_ms > expected_last_modified_ms);
    // No canonical on .css
    assert!(!headers.headers().has("Link"));

    fixture.advance_ms(10_000);
    assert_eq!(content, CSS_DATA.as_bytes()); // no absolutification
    let (content, headers) = fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "b.jpg", "jpg"))
        .expect("jpg served");
    let link = headers.headers().lookup1("Link").expect("canonical link");
    // .jpg should get a canonical link.
    assert_eq!(link, "<http://test.com/b.jpg>; rel=\"canonical\"");
    let fetch2_ms = fixture.server.now_ms();
    assert!(fetch2_ms > fetch1_ms);
    assert_eq!(headers.last_modified_time_ms(), expected_last_modified_ms);
    assert_eq!(
        headers.headers().has("Last-Modified"),
        last_modified_on_origin
    );
    assert_eq!(content, IMAGE_DATA.as_bytes());

    let (content, headers) = fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "c.js", "js"))
        .expect("js served");
    assert_eq!(headers.last_modified_time_ms(), expected_last_modified_ms);
    assert_eq!(
        headers.headers().has("Last-Modified"),
        last_modified_on_origin
    );
    // No canonical on .js
    assert!(!headers.headers().has("Link"));
    assert_eq!(content, JS_DATA.as_bytes());
}

#[test]
fn serve_files() {
    let mut fixture = Fixture::new();
    fixture.last_modified_on_origin = true;
    fixture.init_test(SHORT_TTL_SEC);
    test_serve_files(&fixture, true);
}

#[test]
fn serve_files_no_last_modified_on_origin() {
    let mut fixture = Fixture::new();
    fixture.last_modified_on_origin = false;
    fixture.init_test(SHORT_TTL_SEC);
    test_serve_files(&fixture, false);
}

#[test]
fn extend_unhealthy() {
    // NOTE: LRU/cache counts wait for cache statistics.
    let fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    fixture.init_test(SHORT_TTL_SEC);
    let html = generate_html(&fixture, CSS_FILE, "b.jpg", "c.js");
    for _ in 0..3 {
        fixture.validate_expected("unhealthy", &html, &html);
    }
    assert_eq!(fixture.cache_extensions().get(), 0);
    assert_eq!(fixture.fetcher.fetch_count(), 0);
}

#[test]
fn serve_files_unhealthy() {
    let mut fixture = Fixture::new();
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    fixture.last_modified_on_origin = true;
    // Serving reconstructs despite sick caches.
    fixture.init_test(SHORT_TTL_SEC);
    test_serve_files(&fixture, true);
}

#[test]
fn consistent_hash_with_rewrite() {
    // NOTE: LRU insert/hit counts wait for cache statistics.
    use ps_cache::hash::{Hasher, Md5Hasher};
    let fixture = Fixture::with_md5();
    fixture.add_rewrite_domain_mapping("http://new.com/", TEST_DOMAIN);
    fixture.init_test(SHORT_TTL_SEC);
    // First do the HTML rewrite.
    let hash = Md5Hasher::new().hash(CSS_DATA.as_bytes());
    let extended_css = format!(
        "http://new.com/sub/{}.pagespeed.ce.{hash}.css",
        String::from_utf8_lossy(&DEFAULT_ENCODER.encode(&[CSS_TAIL.as_bytes().to_vec()]))
    );
    fixture.validate_expected(
        "consistent_hash",
        &css_link_href(CSS_FILE),
        &css_link_href(&extended_css),
    );
    // The mock must also serve under the new name (see the C++
    // TODO about canonicalizing cache URLs).
    fixture.init_resource_new_host("http://new.com/", CSS_FILE, "text/css", CSS_DATA);
    let (content, _) = fixture.serve_url(&extended_css).expect("served");
    assert_eq!(content, CSS_DATA.as_bytes());
}

#[test]
fn consistent_hash_with_shard() {
    // NOTE: LRU insert/hit counts wait for cache statistics.
    let fixture = Fixture::with_md5();
    fixture.add_rewrite_domain_mapping("http://new.com/", TEST_DOMAIN);
    fixture.add_shard("http://new.com/", "shard1.com,shard2.com");
    fixture.init_test(SHORT_TTL_SEC);
    // First do the HTML rewrite (hash pinned by the C++ test).
    let extended_css = format!(
        "http://shard2.com/sub/{}.pagespeed.ce.MnXHB3ChUY.css",
        String::from_utf8_lossy(&DEFAULT_ENCODER.encode(&[CSS_TAIL.as_bytes().to_vec()]))
    );
    fixture.validate_expected(
        "consistent_hash",
        &css_link_href(CSS_FILE),
        &css_link_href(&extended_css),
    );
    fixture.init_resource_new_host("http://shard2.com/", CSS_FILE, "text/css", CSS_DATA);
    let (content, _) = fixture.serve_url(&extended_css).expect("served");
    // Through the luck of hashes, the embedded image shards
    // differently than the CSS file.
    let expected = ".blue {color: blue; src: url(http://shard1.com/sub/embedded.png);}";
    assert_eq!(content, expected.as_bytes());
    use ps_cache::hash::{Hasher, Md5Hasher};
    assert_eq!(Md5Hasher::new().hash(&content), "MnXHB3ChUY");
}

#[test]
fn serve_files_with_rewrite_domains_enabled() {
    let fixture = Fixture::new();
    fixture.add_rewrite_domain_mapping("http://new.com/", TEST_DOMAIN);
    fixture.init_test(SHORT_TTL_SEC);
    let content = fixture
        .serve_url(&fixture.encode(&format!("{TEST_DOMAIN}sub/"), CSS_TAIL, "css"))
        .expect("served");
    assert_eq!(
        content.0,
        format!(".blue {{color: blue; src: url(http://new.com/sub/embedded.png);}}").as_bytes()
    );
}

#[test]
fn serve_files_with_rewrite_domain_and_path_enabled() {
    let fixture = Fixture::new();
    fixture.add_rewrite_domain_mapping("http://new.com/test/", TEST_DOMAIN);
    fixture.init_test(SHORT_TTL_SEC);
    let content = fixture
        .serve_url(&fixture.encode(&format!("{TEST_DOMAIN}sub/"), CSS_TAIL, "css"))
        .expect("served");
    assert_eq!(
        content.0,
        format!(".blue {{color: blue; src: url(http://new.com/test/sub/embedded.png);}}")
            .as_bytes()
    );
}

#[test]
fn serve_files_with_shard() {
    let fixture = Fixture::new();
    fixture.add_rewrite_domain_mapping("http://new.com/", TEST_DOMAIN);
    fixture.add_shard("http://new.com/", "shard1.com,shard2.com");
    fixture.init_test(SHORT_TTL_SEC);
    let content = fixture
        .serve_url(&fixture.encode(&format!("{TEST_DOMAIN}sub/"), CSS_TAIL, "css"))
        .expect("served");
    assert_eq!(
        content.0,
        format!(".blue {{color: blue; src: url(http://shard1.com/sub/embedded.png);}}").as_bytes()
    );
}

#[test]
fn serve_files_from_delayed_fetch() {
    // NOTE: the C++ helper serves from a fresh server; clearing the
    // caches observes the same reconstruction-from-origin.
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    for (url, expected) in [
        (
            fixture.encode(&format!("{TEST_DOMAIN}sub/"), CSS_TAIL, "css"),
            CSS_DATA,
        ),
        (fixture.encode(TEST_DOMAIN, "b.jpg", "jpg"), IMAGE_DATA),
        (fixture.encode(TEST_DOMAIN, "c.js", "js"), JS_DATA),
    ] {
        fixture.clear_caches();
        let content = fixture.serve_url(&url).expect("served");
        assert_eq!(content.0, expected.as_bytes());
    }
}

/// Corrupt-URL matrix (`TestCorruptUrl`).
fn test_corrupt_url(junk: &str, append_junk: bool) {
    let fixture = Fixture::new();
    fixture.init_test(SHORT_TTL_SEC);
    let a_ext = fixture.encode(CSS_SUBDIR, CSS_TAIL, "css");
    let b_ext = fixture.encode("", "b.jpg", "jpg");
    let c_ext = fixture.encode("", "c.js", "js");
    fixture.validate_expected(
        "no_ext_corrupt_fetched",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(&fixture, &a_ext, &b_ext, &c_ext),
    );
    let change_suffix = |url: &str, suffix: &str| {
        if append_junk {
            format!("{url}{junk}")
        } else {
            format!("{}{junk}", url.strip_suffix(suffix).unwrap())
        }
    };
    for (ext, suffix) in [(&a_ext, ".css"), (&b_ext, ".jpg"), (&c_ext, ".js")] {
        let corrupted = change_suffix(&format!("{TEST_DOMAIN}{ext}"), suffix);
        assert!(fixture.serve_url(&corrupted).is_some(), "serve {corrupted}");
    }
    fixture.validate_expected(
        "no_ext_corrupt_cached",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(&fixture, &a_ext, &b_ext, &c_ext),
    );
}

#[test]
fn no_extension_corruption() {
    test_corrupt_url("%22", true);
}

#[test]
fn no_query_corruption() {
    test_corrupt_url("?query", true);
}

#[test]
fn no_wrong_ext_corruption() {
    test_corrupt_url(".html", false);
}

#[test]
fn made_on_the_fly() {
    // Fetches go through on-the-fly construction, not the cache.
    let fixture = Fixture::new();
    fixture.init_test(MEDIUM_TTL_SEC);
    let b_ext = fixture.encode("", "b.jpg", "jpg");
    fixture.validate_expected(
        "and_img",
        "<img src=\"b.jpg\">",
        &format!("<img src=\"{b_ext}\">"),
    );
    assert_eq!(fixture.server.variable("cached_resource_fetches").get(), 0);
    assert_eq!(
        fixture
            .server
            .variable("succeeded_filter_resource_fetches")
            .get(),
        0
    );
    let out = fixture
        .serve_url(&format!("{TEST_DOMAIN}{b_ext}"))
        .expect("served");
    assert_eq!(out.0, IMAGE_DATA.as_bytes());
    assert_eq!(fixture.server.variable("cached_resource_fetches").get(), 0);
    assert_eq!(
        fixture
            .server
            .variable("succeeded_filter_resource_fetches")
            .get(),
        1
    );
}

#[test]
fn retain_extra_headers() {
    let fixture = Fixture::new();
    fixture.init_resource(
        "retain.css",
        ps_http::content_type::mime_to_content_type("text/css").unwrap(),
        CSS_DATA,
        300,
    );
    fixture.options.borrow_mut().enable_extend_cache_filters();
    let url = format!("{TEST_DOMAIN}retain.css");
    fixture
        .fetcher
        .add_response_header(&url, "Etag", "Custom-Etag");
    fixture
        .fetcher
        .add_response_header(&url, "extra", "attribute");
    fixture
        .fetcher
        .add_response_header(&url, "Set-Cookie", "Custom-Cookie");
    let (_, response) = fixture
        .serve_url(&format!(
            "{TEST_DOMAIN}{}",
            fixture.encode("", "retain.css", "css")
        ))
        .expect("served");
    // Extra non-blacklisted header is preserved.
    assert_eq!(
        response.headers().lookup("extra"),
        Some(vec!["attribute".to_string()])
    );
    // Blacklisted headers are stripped (or changed).
    assert!(response.headers().lookup("Set-Cookie").is_none());
}

#[test]
fn trim_url_interaction() {
    use ps_html::filters::url_left_trim::UrlLeftTrimFilter;
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .enable_filter(ps_config::filter::Filter::LeftTrimUrls);
    fixture.init_test(MEDIUM_TTL_SEC);
    let url = format!("{TEST_DOMAIN}ce_then_trim.html");
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    let driver = fixture.driver_for(&url);
    parser.add_filter(Box::new(CacheExtenderFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
    let valued = fixture.options.borrow().url_valued_attributes().to_vec();
    parser.add_filter(Box::new(UrlLeftTrimFilter::new(valued)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    let input = format!("<link rel='stylesheet' href='{CSS_FILE}' type='text/css'>\n");
    parser.start_parse(url.as_bytes(), false);
    parser.write(input.as_bytes());
    parser.finish();
    let a_ext = fixture.encode(CSS_SUBDIR, CSS_TAIL, "css");
    let expected = format!("<link rel='stylesheet' href='{a_ext}' type='text/css'>\n");
    assert_eq!(String::from_utf8(sink.bytes()).unwrap(), expected);
}

#[test]
fn defang_html() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    // Executable types must not extend (XSS under loose lawyers).
    for (name, mime, data) in [
        ("a.html", "text/html", "boo!"),
        ("a.xhtml", "application/xhtml+xml", "bwahahaha!"),
        ("a.xml", "text/xml", "boo!"),
    ] {
        fixture.init_resource(
            name,
            ps_http::content_type::mime_to_content_type(mime).unwrap(),
            data,
            SHORT_TTL_SEC,
        );
    }
    let before = format!(
        "{}{}{}",
        css_link_href("a.html"),
        css_link_href("a.xhtml"),
        css_link_href("a.xml")
    );
    fixture.validate_expected("defang", &before, &before);
}

#[test]
fn unrecognized_content_type_is_not_guessed_from_url() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("application/javascript"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), MEDIUM_TTL_SEC * 1000, "");
    headers.compute_caching();
    headers
        .headers_mut()
        .replace("Content-Type", "application/x-unlisted-type");
    headers.compute_caching();
    for name in [
        "data.bin?format=.js",
        "data.bin?format=.css",
        "files/named.js",
    ] {
        fixture.fetcher.set_response(
            &format!("{TEST_DOMAIN}{name}"),
            headers.clone(),
            JS_DATA.as_bytes(),
        );
    }
    let html = "<script src='data.bin?format=.js'></script>\
        <link rel=stylesheet href='data.bin?format=.css'>\
        <script src='files/named.js'></script>";
    fixture.validate_expected("unrecognized_type", html, html);
    assert_eq!(fixture.cache_extensions().get(), 0);
    assert!(fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "data.bin?format=.js", "js"))
        .is_none());
    assert!(fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "data.bin?format=.css", "css"))
        .is_none());
    assert!(fixture
        .serve_url(&fixture.encode(&format!("{TEST_DOMAIN}files/"), "named.js", "js"))
        .is_none());
    assert_eq!(fixture.cache_extensions().get(), 0);
}

#[test]
fn missing_content_type_is_guessed_from_path_only() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("application/javascript"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), MEDIUM_TTL_SEC * 1000, "");
    headers.compute_caching();
    headers.headers_mut().remove_all("Content-Type");
    headers.compute_caching();
    for name in ["data.bin?format=.js", "named.js?format=.bin"] {
        fixture.fetcher.set_response(
            &format!("{TEST_DOMAIN}{name}"),
            headers.clone(),
            JS_DATA.as_bytes(),
        );
    }
    assert!(fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "data.bin?format=.js", "js"))
        .is_none());
    assert_eq!(fixture.cache_extensions().get(), 0);
    let (content, fetched_headers) = fixture
        .serve_url(&fixture.encode(TEST_DOMAIN, "named.js?format=.bin", "js"))
        .expect("guessed js served");
    assert_eq!(content, JS_DATA.as_bytes());
    assert_eq!(
        fetched_headers
            .determine_content_type()
            .map(|content_type| content_type.mime_type().to_string()),
        Some("application/javascript".to_string())
    );
}

#[test]
fn alternate_stylesheet() {
    let fixture = Fixture::new();
    fixture.init_test(MEDIUM_TTL_SEC);
    let new_url = fixture.encode(CSS_SUBDIR, CSS_TAIL, "css");
    for (rel, expect_rewrite) in [
        ("stylesheet", true),
        ("alternate stylesheet", true),
        (" StyleSheet alterNATE  ", true),
        ("  foo stylesheet alternate bar ", true),
        ("alternate snowflake", false),
    ] {
        let input = format!("<link rel='{rel}' href='{CSS_FILE}' title='foo'>");
        let expected = if expect_rewrite {
            format!("<link rel='{rel}' href='{new_url}' title='foo'>")
        } else {
            input.clone()
        };
        fixture.validate_expected("alternate_stylesheet", &input, &expected);
    }
}

#[test]
fn preserve_url_relativity() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option(
            "PreserveUrlRelativity",
            "true",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    fixture.init_test(MEDIUM_TTL_SEC);
    fixture.validate_expected(
        "preserve_url_relativity",
        "<img src=b.jpg>",
        "<img src=b.jpg.pagespeed.ce.0.jpg>",
    );
}

#[test]
fn no_preserve_url_relativity() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option(
            "PreserveUrlRelativity",
            "false",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    fixture.init_test(MEDIUM_TTL_SEC);
    fixture.validate_expected(
        "preserve_url_relativity",
        "<img src=b.jpg>",
        "<img src=http://test.com/b.jpg.pagespeed.ce.0.jpg>",
    );
}

/// Fallback sanitization matrix (`ValidateFallbackHeaderSanitization`).
fn test_fallback_sanitization(mime: &str, expect_load: bool) {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    let leaf = format!("leaf-{}", mime.replace('/', "-"));
    let mut origin_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    origin_headers.set_first_line(1, 1, 200, "OK");
    origin_headers.headers_mut().add("Content-Type", mime);
    origin_headers.set_date_and_caching(fixture.server.now_ms(), 0, "; no-transform");
    origin_headers.compute_caching();
    fixture.fetcher.set_response(
        &format!("{TEST_DOMAIN}{leaf}"),
        origin_headers,
        b"this isn't a real file",
    );
    let url = format!("{TEST_DOMAIN}{}", fixture.encode("", &leaf, "ignored"));
    let served = fixture.serve_url(&url);
    assert_eq!(served.is_some(), expect_load, "mime {mime}");
    if let Some((content, headers)) = served {
        assert_eq!(content, b"this isn't a real file");
        assert_eq!(
            headers
                .determine_content_type()
                .map(|content_type| content_type.mime_type().to_string()),
            Some(mime.to_string())
        );
        assert_eq!(
            headers.headers().lookup1("X-Content-Type-Options"),
            Some("nosniff".to_string())
        );
    }
}

#[test]
fn content_type_validation() {
    for mime in [
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
        test_fallback_sanitization(mime, true);
    }
    for mime in [
        "text/html",
        "text/plain",
        "text/xml",
        "application/xml",
        "image/svg",
    ] {
        test_fallback_sanitization(mime, false);
    }
}

#[test]
fn srcset() {
    let fixture = Fixture::new();
    fixture.init_test(100);
    fixture.init_resource(
        "a.jpg",
        ps_http::content_type::mime_to_content_type("image/jpeg").unwrap(),
        IMAGE_DATA,
        100,
    );
    fixture.init_resource(
        "b.jpg",
        ps_http::content_type::mime_to_content_type("image/jpeg").unwrap(),
        IMAGE_DATA,
        100,
    );
    let a_url = fixture.encode("", "a.jpg", "jpg");
    let b_url = fixture.encode("", "b.jpg", "jpg");
    fixture.validate_expected(
        "srcset",
        "<img src=a.jpg srcset=\"a.jpg 1x, b.jpg 2x\">",
        &format!("<img src={a_url} srcset=\"{a_url} 1x, {b_url} 2x\">"),
    );
}

#[test]
fn vary_origin() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().enable_extend_cache_filters();
    let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    fixture.server.set_default_long_cache_headers(
        ps_http::content_type::mime_to_content_type("text/css"),
        "",
        "",
        &mut headers,
    );
    headers.set_date_and_caching(fixture.server.now_ms(), 100 * 1000, "");
    headers.compute_caching();
    headers.headers_mut().add("Vary", "Origin");
    headers.compute_caching();
    fixture
        .fetcher
        .set_response(&format!("{TEST_DOMAIN}a.css"), headers, b"css file");
    let extended = fixture.encode("", "a.css", "css");
    fixture.validate_expected(
        "vary origin",
        &css_link_href("a.css"),
        &css_link_href(&extended),
    );
}

#[test]
fn extend_if_rewritten() {
    let fixture = Fixture::new();
    fixture.init_test(LONG_TTL_SEC);
    fixture.add_rewrite_domain_mapping("cdn.com", TEST_DOMAIN);
    fixture.validate_expected(
        "extend_if_rewritten",
        &generate_html(&fixture, CSS_FILE, "b.jpg", "c.js"),
        &generate_html(
            &fixture,
            &fixture.encode("http://cdn.com/sub/", CSS_TAIL, "css"),
            &fixture.encode("http://cdn.com/", "b.jpg", "jpg"),
            &fixture.encode("http://cdn.com/", "c.js", "js"),
        ),
    );
    assert_eq!(fixture.cache_extensions().get(), 3);
}
