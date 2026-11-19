//! JavaScript-rewrite tests ported from `javascript_filter_test.cc`,
//! run under both minifiers (`experimental` param).
//!
//! NOTE: logging assertions wait for the log record; CSP cases wait
//! for the CSP subsystem.
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
use ps_html::filters::javascript::{JavascriptFilter, MINIFY_USES};
use ps_html::filters::js_inline::JsInlineFilter;
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const JS_DATA: &str =
    "alert     (    'hello, world!'    )  /* removed */ <!-- removed -->  // single-line-comment";
const JS_MIN_DATA: &str = "alert('hello, world!')";
const ORIG_JS: &str = "hello.js";
const UNAUTH_JS: &str = "http://other.domain.com/hello.js";
const LIBRARY_URL: &str = "https://www.example.com/hello/1.0/hello.js";
const INTROSPECTIVE_JS: &str = "<script type='text/javascript' src='introspective.js'></script>";

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    experimental: bool,
    /// Response mime verdict (`None` = unknown, C++ default).
    mime_is_xhtml: RefCell<Option<bool>>,
}

impl Fixture {
    fn new(experimental: bool) -> Self {
        Self::with_hasher(experimental, true)
    }

    /// Builds a fixture with the real MD5 namer (`UseMd5Hasher`).
    fn with_md5(experimental: bool) -> Self {
        Self::with_hasher(experimental, false)
    }

    fn with_hasher(experimental: bool, mock: bool) -> Self {
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
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .set_option(
                "UseExperimentalJsMinifier",
                if experimental { "true" } else { "false" },
                ps_config::Scope::ProcessStrict,
            )
            .unwrap();
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            http_backend,
            metadata_backend,
            experimental,
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
        if let Some(is_xhtml) = *self.mime_is_xhtml.borrow() {
            driver.set_is_xhtml(is_xhtml);
        }
        driver
    }

    /// Scripts a cacheable JS fetch (default headers, no validators).
    fn mock_js(&self, url: &str, content: &[u8], ttl_sec: i64) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("application/javascript"),
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

    fn init_test(&self, ttl_sec: i64) {
        self.mock_js(
            &format!("{TEST_DOMAIN}{ORIG_JS}"),
            JS_DATA.as_bytes(),
            ttl_sec,
        );
        self.mock_js(UNAUTH_JS, JS_DATA.as_bytes(), ttl_sec);
    }

    /// Registers the minified JS as a canonical library.
    fn register_library(&self) {
        use ps_cache::hash::{Hasher, Md5Hasher};
        let hash = Md5Hasher::with_size(21).hash(JS_MIN_DATA.as_bytes());
        assert_eq!(hash.len(), 21);
        assert!(self.options.borrow_mut().register_library(
            JS_MIN_DATA.len() as i64,
            &hash,
            LIBRARY_URL
        ));
    }

    /// Expected rewritten leaf (`expected_rewritten_path_`).
    fn rewritten_leaf(&self) -> String {
        let encoder = ps_html::url_segment_encoder::DefaultEncoder;
        use ps_html::url_segment_encoder::UrlSegmentEncoder;
        let encoded = encoder.encode(&[ORIG_JS.as_bytes().to_vec()]);
        format!("{}.pagespeed.jm.0.js", String::from_utf8_lossy(&encoded))
    }

    /// Parses through the minify filter (plus the outliner and
    /// combiner when enabled, first, like `AddFilters`).
    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        if self.options.borrow().enabled(Filter::OutlineJavascript) {
            parser.add_filter(Box::new(ps_html::filters::js_outline::JsOutlineFilter::new(
                driver.clone(),
            )) as Box<dyn HtmlFilter>);
        }
        if self.options.borrow().enabled(Filter::CombineJavascript) {
            parser.add_filter(Box::new(ps_html::filters::js_combine::JsCombineFilter::new(
                driver.clone(),
            )) as Box<dyn HtmlFilter>);
        }
        parser.add_filter(Box::new(JavascriptFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
        driver.add_filter(Rc::new(JavascriptFilter::new(driver.clone())));
        driver.add_filter(Rc::new(JavascriptFilter::new_source_map(driver.clone())));
        if self.options.borrow().enabled(Filter::CombineJavascript) {
            driver.add_filter(Rc::new(ps_html::filters::js_combine::JsCombineFilter::new(
                driver.clone(),
            )));
        }
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

    /// Puts a value straight into the HTTP cache (`HTTPCache::Put`).
    fn put_http(&self, key: &str, headers: &ResponseHeaders, body: &[u8]) {
        use ps_http::response::{HttpOptions, RequestProperties};
        use ps_http::value::HttpValue;
        let mut value = HttpValue::new();
        value.set_headers(headers.clone());
        value.write_body(body);
        let options = self.options.borrow();
        assert!(self.server.http_cache().put_value(
            key,
            "",
            RequestProperties::default(),
            HttpOptions {
                respect_vary: options.respect_vary(),
                implicit_cache_ttl_ms: options.implicit_cache_ttl_ms(),
            },
            &value,
            self.server.now_ms(),
        ));
    }

    /// Finds a value in the HTTP cache (`HttpBlockingFind`).
    fn http_find(&self, key: &str) -> bool {
        use ps_http::http_cache::FindStatus;
        use ps_http::response::{RequestProperties, VaryOption};
        let mut ctx = ps_http::request::RequestContext::new(HttpOptions::default_for_tests());
        self.server
            .http_cache()
            .find(
                key,
                "",
                RequestProperties::default(),
                VaryOption::IgnoreVaryOnResources,
                &|_| true,
                -1,
                &mut ctx,
                self.server.now_ms(),
            )
            .status
            == FindStatus::Found
    }

    fn clear_caches(&self) {
        self.http_backend.clear();
        self.metadata_backend.clear();
    }
}

fn generate_html(src: &str) -> String {
    format!("<script type='text/javascript' src='{src}'></script>\n")
}

macro_rules! javascript_tests {
    ($mod_name:ident, $experimental:expr) => {
        mod $mod_name {
            use super::*;

            fn fixture() -> Fixture {
                Fixture::new($experimental)
            }

            #[test]
            fn do_rewrite() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                // NOTE: rewriter-logging assertions wait for the log record.
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}do_rewrite.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(
                    fixture.stat("javascript_total_bytes_saved"),
                    (JS_DATA.len() - JS_MIN_DATA.len()) as i64
                );
                assert_eq!(fixture.stat("javascript_total_original_bytes"), JS_DATA.len() as i64);
                assert_eq!(fixture.stat(MINIFY_USES), 1);
            }

            #[test]
            fn dont_rewrite_unauthorized_domain() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = generate_html(UNAUTH_JS);
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}dont_rewrite.html"), &html), html);
            }

            #[test]
            fn dont_rewrite_unauthorized_domain_with_unauth_option_set() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                fixture
                    .options
                    .borrow_mut()
                    .add_inline_unauthorized_resource_type(ps_config::options::Category::Script);
                // Unauthorized types only open inlining, not minification.
                let html = generate_html(UNAUTH_JS);
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}dont_rewrite.html"), &html), html);
            }

            #[test]
            fn dont_rewrite_disallowed_scripts() {
                let fixture = Fixture::new($experimental);
                fixture.mock_js(&format!("{TEST_DOMAIN}a.js"), b"document.write('a');", 100);
                fixture.options.borrow_mut().disallow("*a.js*");
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::InlineJavascript);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                let html = format!("<script type='text/javascript' src='a.js'></script>\n");
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}inline_disallowed.html"), &html),
                    html
                );
            }

            #[test]
            fn do_inline_allowed_for_inlining_scripts() {
                let fixture = Fixture::new($experimental);
                fixture.set_html_mimetype();
                fixture.mock_js(&format!("{TEST_DOMAIN}a.js"), b"document.write('a');", 100);
                fixture
                    .options
                    .borrow_mut()
                    .allow_only_when_inlining("*a.js*");
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::InlineJavascript);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                // Inlining-allowed scripts inline through the ji filter.
                let url = format!("{TEST_DOMAIN}inline_allowed.html");
                let driver = fixture.driver_for(&url);
                let sink = SharedSink::default();
                let mut parser = Parser::new();
                parser.add_filter(
                    Box::new(ps_html::filters::js_inline::JsInlineFilter::new(driver.clone()))
                        as Box<dyn HtmlFilter>,
                );
                parser.add_filter(
                    Box::new(JavascriptFilter::new(driver)) as Box<dyn HtmlFilter>
                );
                parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
                parser.start_parse(url.as_bytes(), false);
                parser.write(b"<script type='text/javascript' src='a.js'></script>\n");
                parser.finish();
                assert_eq!(
                    String::from_utf8(sink.bytes()).unwrap(),
                    "<script type='text/javascript'>document.write('a');</script>\n"
                );
            }

            #[test]
            fn do_rewrite_unhealthy() {
                let fixture = fixture();
                fixture.http_backend.set_is_healthy(false);
                fixture.metadata_backend.set_is_healthy(false);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}do_rewrite.html"), &html),
                    html
                );
            }

            #[test]
            fn rewrite_already_cached_properly() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100_000_000);
                // But we will rewrite because we can make the data smaller.
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}rewrite_cached.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
            }

            #[test]
            fn no_rewrite_origin_uncacheable() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(0);
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}no_cacheable.html"), &html),
                    html
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(fixture.stat("javascript_total_bytes_saved"), 0);
                assert_eq!(fixture.stat("javascript_total_original_bytes"), 0);
                assert_eq!(fixture.stat(MINIFY_USES), 0);
            }

            #[test]
            fn identify_library() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}identify_library.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(LIBRARY_URL)
                );
                assert_eq!(fixture.stat("javascript_libraries_identified"), 1);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
            }

            #[test]
            fn identify_library_twice() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let input = format!("{}{}", generate_html(ORIG_JS), generate_html(ORIG_JS));
                let expected = format!("{}{}", generate_html(LIBRARY_URL), generate_html(LIBRARY_URL));
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}identify_twice.html"), &input),
                    expected
                );
                // The second rewrite uses cached data from the first.
                assert_eq!(fixture.stat("javascript_libraries_identified"), 1);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
            }

            #[test]
            fn js_preserve_urls_on_test() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .soft_enable_filter_for_testing(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .soft_enable_filter_for_testing(Filter::CanonicalizeJavascriptLibraries);
                fixture.options.borrow_mut().set_js_preserve_urls(true);
                assert!(fixture.options.borrow().enabled(Filter::RewriteJavascriptExternal));
                assert!(!fixture.options.borrow().enabled(Filter::CanonicalizeJavascriptLibraries));
                fixture.init_test(100);
                // The URL doesn't change under preserve.
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}js_preserved.html"), &html),
                    html
                );
                // But the JS was preemptively optimized: serving hits.
                // NOTE: LRU hit/miss/insert counts wait for cache statistics.
                let (out_js, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(out_js, JS_MIN_DATA.as_bytes());
            }

            #[test]
            fn js_preserve_overriding_extend() {
                let fixture = fixture();
                fixture.register_library();
                let mut global = RewriteOptions::new();
                global.enable_filter(Filter::ExtendCacheCss);
                let mut vhost = RewriteOptions::new();
                vhost.soft_enable_filter_for_testing(Filter::RewriteJavascriptExternal);
                vhost.soft_enable_filter_for_testing(Filter::CanonicalizeJavascriptLibraries);
                vhost.set_js_preserve_urls(true);
                fixture.options.borrow_mut().merge(&global);
                fixture.options.borrow_mut().merge(&vhost);
                assert!(fixture.options.borrow().enabled(Filter::RewriteJavascriptExternal));
                fixture.init_test(100);
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}js_preserved.html"), &html),
                    html
                );
                let (out_js, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(out_js, JS_MIN_DATA.as_bytes());
            }

            #[test]
            fn js_extend_overriding_preserve() {
                let fixture = fixture();
                fixture.register_library();
                let mut global = RewriteOptions::new();
                global.set_js_preserve_urls(true);
                global.enable_filter(Filter::RewriteJavascriptExternal);
                let mut vhost = RewriteOptions::new();
                vhost.enable_filter(Filter::ExtendCacheScripts);
                fixture.options.borrow_mut().merge(&global);
                fixture.options.borrow_mut().merge(&vhost);
                assert!(fixture.options.borrow().enabled(Filter::RewriteJavascriptExternal));
                fixture.init_test(100);
                // The vhost extend-switch wins: the URL updates.
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}js_extend.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
                let (out_js, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(out_js, JS_MIN_DATA.as_bytes());
            }

            #[test]
            fn js_preserve_urls_no_preemptive_rewrite_test() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .soft_enable_filter_for_testing(Filter::RewriteJavascriptExternal);
                fixture.options.borrow_mut().set_js_preserve_urls(true);
                fixture
                    .options
                    .borrow_mut()
                    .set_in_place_preemptive_rewrite_javascript(false);
                assert!(fixture.options.borrow().enabled(Filter::RewriteJavascriptExternal));
                fixture.init_test(100);
                // Without preemptive rewrites the filter is not added
                // at all (chain-construction gate, mirrored here).
                let add_filter = !fixture.options.borrow().js_preserve_urls()
                    || fixture.options.borrow().in_place_preemptive_rewrite_javascript()
                    || fixture.options.borrow().enabled(Filter::RewriteJavascriptInline);
                assert!(!add_filter);
                // Fetching directly still optimizes on demand.
                let (out_js, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(out_js, JS_MIN_DATA.as_bytes());
            }

            #[test]
            fn identify_library_no_minification() {
                // Library identification works without minification.
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}identify_nomin.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(LIBRARY_URL)
                );
                assert_eq!(fixture.stat("javascript_libraries_identified"), 1);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(fixture.stat("javascript_total_bytes_saved"), 0);
            }

            #[test]
            fn disallowed_urls_not_checked_for_canonicalization() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.options.borrow_mut().disallow("*hello.js*");
                fixture.init_test(100);
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}no_ident.html"), &html),
                    html
                );
                assert_eq!(fixture.stat("javascript_libraries_identified"), 0);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(fixture.stat("javascript_total_bytes_saved"), 0);
            }

            #[test]
            fn allow_when_inlining_urls_still_not_checked() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture
                    .options
                    .borrow_mut()
                    .allow_only_when_inlining("*hello.js*");
                fixture.init_test(100);
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}no_ident.html"), &html),
                    html
                );
                assert_eq!(fixture.stat("javascript_libraries_identified"), 0);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(fixture.stat("javascript_total_bytes_saved"), 0);
            }

            #[test]
            fn identify_failure_no_minification() {
                // No libraries registered: minification runs (for
                // identification) but nothing changes on the page.
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = generate_html(ORIG_JS);
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}ident_fail.html"), &html),
                    html
                );
                assert_eq!(fixture.stat("javascript_libraries_identified"), 0);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
            }

            #[test]
            fn ignore_library_no_identification() {
                // Registered but unenabled: minify normally instead.
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}ignore_lib.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
                assert_eq!(fixture.stat("javascript_libraries_identified"), 0);
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
            }

            /// Served-files matrix (`TestServeFiles`).
            fn test_serve_files(
                fixture: &Fixture,
                mime: &str,
                orig_name: &str,
                orig_content: &str,
                rewritten_name: &str,
                rewritten_content: &str,
            ) {
                use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
                let encoder = DefaultEncoder;
                let encoded = encoder.encode(&[rewritten_name.as_bytes().to_vec()]);
                let rewritten_path = format!(
                    "{TEST_DOMAIN}{}.pagespeed.jm.0.js",
                    String::from_utf8_lossy(&encoded)
                );
                // Seed the HTTP cache, then serve from it.
                let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
                fixture.server.set_default_long_cache_headers(
                    ps_http::content_type::mime_to_content_type(mime),
                    "",
                    "",
                    &mut headers,
                );
                fixture.put_http(&rewritten_path, &headers, rewritten_content.as_bytes());
                let (content, _) = fixture
                    .serve_url(&rewritten_path)
                    .expect("cached serve");
                assert_eq!(content, rewritten_content.as_bytes());
                // Nuke the cache, serve via reconstruction.
                fixture.clear_caches();
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}{orig_name}"),
                    orig_content.as_bytes(),
                    100,
                );
                let (content, _) = fixture
                    .serve_url(&rewritten_path)
                    .expect("reconstructed serve");
                assert_eq!(content, rewritten_content.as_bytes());
                // The entry is cached now.
                assert!(fixture.http_find(&rewritten_path));
            }

            #[test]
            fn serve_files() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                test_serve_files(
                    &fixture,
                    "application/javascript",
                    ORIG_JS,
                    JS_DATA,
                    ORIG_JS,
                    JS_MIN_DATA,
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(
                    fixture.stat("javascript_total_bytes_saved"),
                    (JS_DATA.len() - JS_MIN_DATA.len()) as i64
                );
                assert_eq!(
                    fixture.stat("javascript_total_original_bytes"),
                    JS_DATA.len() as i64
                );
                // No uses: the URL never went into HTML, just served.
                assert_eq!(fixture.stat(MINIFY_USES), 0);
                // Serve from cleared caches (fresh-server equivalent).
                fixture.clear_caches();
                let (content, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(content, JS_MIN_DATA.as_bytes());
            }

            #[test]
            fn serve_files_unhealthy() {
                let fixture = fixture();
                fixture.http_backend.set_is_healthy(false);
                fixture.metadata_backend.set_is_healthy(false);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture.init_test(100);
                test_serve_files(
                    &fixture,
                    "application/javascript",
                    ORIG_JS,
                    JS_DATA,
                    ORIG_JS,
                    JS_MIN_DATA,
                );
            }

            #[test]
            fn serve_rewritten_library() {
                // A library's rewritten URL still serves useful
                // (unminified) content, without corrupting later
                // identification.
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let (content, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(content, JS_DATA.as_bytes());
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}identify_library.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(LIBRARY_URL)
                );
            }

            #[test]
            fn serve_json_file() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                test_serve_files(
                    &fixture,
                    "application/json",
                    "hello.json",
                    "  {  'foo' :  [ 'bar' , 'baz' ]  }  ",
                    "hello.json",
                    "{'foo':['bar','baz']}",
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(fixture.stat(MINIFY_USES), 0);
            }

            #[test]
            fn retain_extra_headers() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                let url = format!("{TEST_DOMAIN}{ORIG_JS}");
                fixture.mock_js(&url, JS_DATA.as_bytes(), 300);
                fixture.fetcher.add_response_header(&url, "Etag", "Custom-Etag");
                fixture.fetcher.add_response_header(&url, "extra", "attribute");
                fixture.fetcher.add_response_header(&url, "Set-Cookie", "Custom-Cookie");
                let (content, response) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{}", fixture.rewritten_leaf()))
                    .expect("served");
                assert_eq!(content, JS_MIN_DATA.as_bytes());
                // Extra non-blacklisted header is preserved.
                assert_eq!(
                    response.headers().lookup1("extra"),
                    Some("attribute".to_string())
                );
                // Blacklisted headers are stripped (or changed).
                assert_eq!(response.headers().lookup1("Set-Cookie"), None);
                assert_eq!(
                    response.headers().lookup1("Etag"),
                    Some("W/\"0\"".to_string())
                );
            }

            #[test]
            fn invalid_input_mimetype() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
                fixture.server.set_default_long_cache_headers(
                    ps_http::content_type::mime_to_content_type("application/javascript"),
                    "",
                    "",
                    &mut headers,
                );
                headers.set_date_and_caching(fixture.server.now_ms(), 100 * 1000, "");
                headers.compute_caching();
                headers.headers_mut().replace("Content-Type", "text/semicolon-inserted");
                headers.compute_caching();
                fixture.fetcher.set_response(
                    &format!("{TEST_DOMAIN}script.notjs"),
                    headers,
                    JS_DATA.as_bytes(),
                );
                let html = generate_html("script.notjs");
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}wrong_mime.html"), &html),
                    html
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
            }

            #[test]
            fn not_rewritten_when_origin_type_is_not_script() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
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
                    JS_DATA.as_bytes(),
                );
                let html = generate_html("note.txt");
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}origin_type.html"), &html),
                    html
                );
                use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
                let encoded =
                    DefaultEncoder.encode(&["note.txt".as_bytes().to_vec()]);
                assert!(fixture
                    .serve_url(&format!(
                        "{TEST_DOMAIN}{}.pagespeed.jm.0.js",
                        String::from_utf8_lossy(&encoded)
                    ))
                    .is_none());
                assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
            }

            #[test]
            fn rewrite_js_404() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
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
                for _ in 0..2 {
                    assert_eq!(
                        fixture.parse(
                            &format!("{TEST_DOMAIN}404.html"),
                            "<script src='404.js'></script>"
                        ),
                        "<script src='404.js'></script>"
                    );
                    assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
                    assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                    assert_eq!(fixture.stat(MINIFY_USES), 0);
                }
            }

            /// Corrupt-URL matrix (`TestCorruptUrl`).
            fn test_corrupt_url(fixture: &Fixture, new_suffix: &str, append: bool) {
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}no_corrupt.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
                let leaf = fixture.rewritten_leaf();
                assert!(leaf.ends_with(".js"));
                let munged = if append {
                    format!("{TEST_DOMAIN}{leaf}{new_suffix}")
                } else {
                    format!(
                        "{TEST_DOMAIN}{}{new_suffix}",
                        leaf.strip_suffix(".js").unwrap()
                    )
                };
                assert!(fixture.serve_url(&munged).is_some(), "serve {munged}");
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}no_corrupt.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
            }

            #[test]
            fn no_extension_corruption() {
                test_corrupt_url(&fixture(), ".js%22", false);
            }

            #[test]
            fn no_query_corruption() {
                test_corrupt_url(&fixture(), ".js?query", false);
            }

            #[test]
            fn no_wrong_ext_corruption() {
                test_corrupt_url(&fixture(), ".html", false);
            }

            fn inline_js(body: &str) -> String {
                format!("<script type='text/javascript'>{body}</script>\n")
            }

            #[test]
            fn inline_javascript() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}inline.html"),
                        &inline_js(JS_DATA)
                    ),
                    inline_js(JS_MIN_DATA)
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
                assert_eq!(
                    fixture.stat("javascript_total_bytes_saved"),
                    (JS_DATA.len() - JS_MIN_DATA.len()) as i64
                );
                assert_eq!(
                    fixture.stat("javascript_total_original_bytes"),
                    JS_DATA.len() as i64
                );
                assert_eq!(fixture.stat(MINIFY_USES), 1);
            }

            #[test]
            fn no_minification_inline_js() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let small = "alert('hello');";
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}inline.html"),
                        &inline_js(small)
                    ),
                    inline_js(small)
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat("javascript_did_not_shrink"), 1);
                assert_eq!(fixture.stat("javascript_minification_failures"), 0);
            }

            #[test]
            fn strip_inline_whitespace() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}strip.html"),
                        &format!("<script src='{ORIG_JS}'>   \t\n   </script>")
                    ),
                    format!("<script src='{}'></script>", fixture.rewritten_leaf())
                );
            }

            #[test]
            fn retain_inline_data() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}retain.html"),
                        &format!("<script src='{ORIG_JS}'> data </script>")
                    ),
                    format!(
                        "<script src='{}'> data </script>",
                        fixture.rewritten_leaf()
                    )
                );
            }

            fn cdata_wrap(body: &str) -> String {
                format!("//<![CDATA[\n{body}\n//]]>")
            }

            fn cdata_alt_wrap(body: &str) -> String {
                format!("//<![CDATA[\r{body}\r//]]>")
            }

            #[test]
            fn cdata_javascript_no_mimetype() {
                // No mimetype: the wrapper is kept (rewrapped).
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}cdata.html"),
                        &inline_js(&cdata_wrap(JS_DATA))
                    ),
                    inline_js(&cdata_wrap(JS_MIN_DATA))
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}cdata.html"),
                        &inline_js(&cdata_alt_wrap(JS_DATA))
                    ),
                    inline_js(&cdata_wrap(JS_MIN_DATA))
                );
            }

            #[test]
            fn cdata_javascript_html_mimetype() {
                // Explicit HTML: the wrapper is dropped.
                let fixture = fixture();
                fixture.set_html_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}cdata.html"),
                        &inline_js(&cdata_wrap(JS_DATA))
                    ),
                    inline_js(JS_MIN_DATA)
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}cdata.html"),
                        &inline_js(&cdata_alt_wrap(JS_DATA))
                    ),
                    inline_js(JS_MIN_DATA)
                );
            }

            #[test]
            fn cdata_javascript_xhtml_mimetype() {
                let fixture = fixture();
                fixture.set_xhtml_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}cdata.html"),
                        &inline_js(&cdata_wrap(JS_DATA))
                    ),
                    inline_js(&cdata_wrap(JS_MIN_DATA))
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}cdata.html"),
                        &inline_js(&cdata_alt_wrap(JS_DATA))
                    ),
                    inline_js(&cdata_wrap(JS_MIN_DATA))
                );
            }

            #[test]
            fn xhtml_inline_javascript() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let dtd = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}xhtml.html"),
                        &format!("{dtd}{}", inline_js(&cdata_wrap(JS_DATA)))
                    ),
                    format!("{dtd}{}", inline_js(&cdata_wrap(JS_MIN_DATA)))
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}xhtml.html"),
                        &format!("{dtd}{}", inline_js(&cdata_alt_wrap(JS_DATA)))
                    ),
                    format!("{dtd}{}", inline_js(&cdata_wrap(JS_MIN_DATA)))
                );
            }

            #[test]
            fn xhtml_inline_javascript_never_emits_cdata_end() {
                // Minification would join brackets into "]]>": keep it all.
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let dtd = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";
                let bracket = "x[y[0] ] > 1;";
                let html = format!("{dtd}{}", inline_js(&cdata_wrap(bracket)));
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}cdata_end.html"), &html),
                    html
                );
            }

            #[test]
            fn xhtml_inline_javascript_without_cdata_never_emits_cdata_end() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let dtd = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";
                let html = format!("{dtd}{}", inline_js("x[y[0] ] > 1;"));
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}no_cdata.html"), &html),
                    html
                );
            }

            #[test]
            fn xhtml_inline_script_keeps_cdata_markers() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let dtd = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">";
                for (case, body) in [
                    ("commented", "<![CDATA[\nf = function () { g(); }\n//]]>"),
                    (
                        "two_sections",
                        "<![CDATA[]]>/*<![CDATA[*/ var s = \"<i>x</i>\"; /*]]>*/",
                    ),
                    ("joined", "<![CDATA[ x[y[0] ] > 1 ]]>"),
                ] {
                    let html = format!("{dtd}{}", inline_js(body));
                    assert_eq!(
                        fixture.parse(&format!("{TEST_DOMAIN}{case}.html"), &html),
                        html,
                        "case {case}"
                    );
                }
            }

            #[test]
            fn xhtml_inline_script_keeps_xml_comment() {
                let fixture = fixture();
                fixture.set_xhtml_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = inline_js("<!--\nvar s = \"<i>x</i>\";\n//-->");
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}xml_comment.html"), &html),
                    html
                );
            }

            #[test]
            fn html_inline_script_loses_xml_comment() {
                let fixture = fixture();
                fixture.set_html_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}xml_comment.html"),
                        &inline_js("<!--\nvar s = \"<i>x</i>\";\n//-->")
                    ),
                    inline_js("var s=\"<i>x</i>\";")
                );
            }

            #[test]
            fn inline_script_inside_noscript_like_element_is_kept() {
                // NOTE: the C++ loop also covers noframes/noembed; the
                // port's literal-tag parsing defers those siblings.
                let fixture = fixture();
                fixture.set_html_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = "<p><noscript><p><script type='text/javascript'>var i = 0;</script></p></noscript></p>";
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}noscript.html"), html),
                    html
                );
            }

            #[test]
            fn inline_minify_does_not_create_text_element_end_tag() {
                let fixture = fixture();
                fixture.set_html_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                for name in [
                    "script", "noscript", "noembed", "noframes", "textarea", "title", "style",
                    "iframe", "xmp", "SCRIPT", "NOSCRIPT", "TextArea", "TiTlE", "STYLE",
                    "iFrame", "Xmp", "NoEmbed", "NOFRAMES",
                ] {
                    let script = format!("var ok = 1 < /* note */ /{name}/ > 0;");
                    let minified = format!("var ok=1< /{name}/>0;");
                    assert!(
                        !minified.to_lowercase().contains(&format!("</{name}").to_lowercase()),
                        "{name}"
                    );
                    if $experimental {
                        assert_eq!(
                            fixture.parse(
                                &format!("{TEST_DOMAIN}div_{name}.html"),
                                &format!("<div>{}</div>", inline_js(&script))
                            ),
                            format!("<div>{}</div>", inline_js(&minified)),
                            "div {name}"
                        );
                        let out = fixture.parse(
                            &format!("{TEST_DOMAIN}textarea_{name}.html"),
                            &format!("<textarea/>{}<textarea>", inline_js(&script)),
                        );
                        assert!(out.contains(&inline_js(&minified)), "{name}: {out}");
                    } else {
                        // The legacy minifier joins them; the guard
                        // keeps the original instead.
                        let html = format!("<div>{}</div>", inline_js(&script));
                        assert_eq!(
                            fixture.parse(&format!("{TEST_DOMAIN}div_{name}.html"), &html),
                            html,
                            "div {name}"
                        );
                        let out = fixture.parse(
                            &format!("{TEST_DOMAIN}textarea_{name}.html"),
                            &format!("<textarea/>{}<textarea>", inline_js(&script)),
                        );
                        assert!(
                            !out.to_lowercase().contains(&format!("1</{name}").to_lowercase()),
                            "{name}: {out}"
                        );
                    }
                }
                // A script that has such an end tag is left as it is.
                let html = inline_js("var s = \"</textarea>\";");
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}existing.html"), &html),
                    html
                );
            }

            #[test]
            fn minified_script_with_text_element_end_tag_is_not_used() {
                use ps_html::filters::javascript::JavascriptFilter;
                assert!(JavascriptFilter::minified_script_may_end_element(
                    b"/* </textarea> */ var ok = 1 < /textarea/ > 0;",
                    b"var ok=1</textarea/>0;"
                ));
                assert!(JavascriptFilter::minified_script_may_end_element(
                    b"var s = \"</TITLE>\";",
                    b"var s=\"</TITLE>\";"
                ));
                assert!(JavascriptFilter::minified_script_may_end_element(
                    b"/* x */ var s = '</sCrIpT';",
                    b"var s='</sCrIpT';"
                ));
                assert!(!JavascriptFilter::minified_script_may_end_element(
                    b"var ok = 1 < /textarea/ > 0;",
                    b"var ok=1< /textarea/>0;"
                ));
                assert!(!JavascriptFilter::minified_script_may_end_element(
                    b"var s = \"</div>\";",
                    b"var s=\"</div>\";"
                ));
            }

            #[test]
            fn inline_minify_less_than_at_end_of_script() {
                let fixture = fixture();
                fixture.set_html_mimetype();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                for script in [
                    "var ok = 1 <",
                    "var ok = 1 <   ",
                    "var ok = 1 </",
                    "var ok = 1 < /",
                    "var ok = 1 < /textarea",
                ] {
                    let out = fixture.parse(
                        &format!("{TEST_DOMAIN}end.html"),
                        &inline_js(script),
                    );
                    assert!(!out.to_lowercase().contains("1</t"), "{script}: {out}");
                }
            }

            #[test]
            fn backslash_in_regexp() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = inline_js("/http:\\/\\/[^/]+\\//");
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}backslash.html"), &html),
                    html
                );
            }

            #[test]
            fn weird_src_crash() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(&format!("{TEST_DOMAIN}foo%3Cbar"), JS_DATA.as_bytes(), 300);
                use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
                let encoded = DefaultEncoder.encode(&["foo%3Cbar".as_bytes().to_vec()]);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}weird.html"),
                        "<script src=foo<bar>Content"
                    ),
                    format!(
                        "<script src={}.pagespeed.jm.0.js>Content",
                        String::from_utf8_lossy(&encoded)
                    )
                );
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}weird.html"), "<script<foo>"),
                    "<script<foo>"
                );
            }

            #[test]
            fn minification_failure() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}foo.js"),
                    b"/* truncated comment",
                    100,
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}fail.html"),
                        "<script src=foo.js></script>"
                    ),
                    "<script src=foo.js></script>"
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 0);
                assert_eq!(fixture.stat("javascript_minification_failures"), 1);
                assert_eq!(fixture.stat(MINIFY_USES), 0);
                assert_eq!(fixture.stat("javascript_did_not_shrink"), 1);
            }

            #[test]
            fn reuse_rewrite() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}reuse1.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
                // First time: minify and use.
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat(MINIFY_USES), 1);
                let (blocks, uses) = (
                    fixture.stat("javascript_blocks_minified"),
                    fixture.stat(MINIFY_USES),
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}reuse2.html"),
                        &generate_html(ORIG_JS)
                    ),
                    generate_html(&fixture.rewritten_leaf())
                );
                // Second time: reuse (no new minification).
                assert_eq!(fixture.stat("javascript_blocks_minified"), blocks);
                assert_eq!(fixture.stat(MINIFY_USES), uses + 1);
            }

            #[test]
            fn no_reuse_inline() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}reuse1.html"),
                        &inline_js(JS_DATA)
                    ),
                    inline_js(JS_MIN_DATA)
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 1);
                assert_eq!(fixture.stat(MINIFY_USES), 1);
                // Second time: minified again (documented behavior).
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}reuse2.html"),
                        &inline_js(JS_DATA)
                    ),
                    inline_js(JS_MIN_DATA)
                );
                assert_eq!(fixture.stat("javascript_blocks_minified"), 2);
                assert_eq!(fixture.stat(MINIFY_USES), 2);
            }

            #[test]
            fn extra_cdata_on_malformed_input() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let link = "<![CDATA[<link href='http://fonts.googleapis.com/css'>]]>";
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}broken_cdata.html"),
                        &inline_js(&format!("\n{link}\n"))
                    ),
                    inline_js(link)
                );
            }

            #[test]
            fn valid_cdata() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}valid_cdata.html"),
                        &inline_js(&cdata_wrap("alert ( 'foo' ) ; \n"))
                    ),
                    inline_js(&cdata_wrap("alert('foo');"))
                );
            }

            #[test]
            fn flush_in_inline_js() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let url = format!("{TEST_DOMAIN}flush.html");
                let driver = fixture.driver_for(&url);
                let sink = SharedSink::default();
                let mut parser = Parser::new();
                parser.add_filter(
                    Box::new(JavascriptFilter::new(driver)) as Box<dyn HtmlFilter>
                );
                parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
                parser.start_parse(url.as_bytes(), false);
                parser.write(b"<html><body><script>  alert  (  'Hel");
                parser.flush();
                parser.write(b"lo, World!'  )  </script></body></html>");
                parser.finish();
                assert_eq!(
                    String::from_utf8(sink.bytes()).unwrap(),
                    "<html><body><script>alert('Hello, World!')</script></body></html>"
                );
            }

            #[test]
            fn flush_in_end_tag() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let url = format!("{TEST_DOMAIN}flush_end.html");
                let driver = fixture.driver_for(&url);
                let sink = SharedSink::default();
                let mut parser = Parser::new();
                parser.add_filter(
                    Box::new(JavascriptFilter::new(driver)) as Box<dyn HtmlFilter>
                );
                parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
                parser.start_parse(url.as_bytes(), false);
                parser.write(b"<html><body><script>  alert  (  'Hello, World!'  )  </scr");
                parser.flush();
                parser.write(b"ipt></body></html>");
                parser.finish();
                assert_eq!(
                    String::from_utf8(sink.bytes()).unwrap(),
                    "<html><body><script>alert('Hello, World!')</script></body></html>"
                );
            }

            #[test]
            fn flush_after_begin_script() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let url = format!("{TEST_DOMAIN}flush_begin.html");
                let driver = fixture.driver_for(&url);
                let sink = SharedSink::default();
                let mut parser = Parser::new();
                parser.add_filter(
                    Box::new(JavascriptFilter::new(driver)) as Box<dyn HtmlFilter>
                );
                parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
                parser.start_parse(url.as_bytes(), false);
                parser.write(b"<html><body><script>");
                parser.flush();
                parser.write(b"alert  (  'Hello, World!'  )  </script><script></script></body></html>");
                parser.finish();
                assert_eq!(
                    String::from_utf8(sink.bytes()).unwrap(),
                    "<html><body><script>alert('Hello, World!')</script><script></script></body></html>"
                );
            }

            #[test]
            fn strip_inline_whitespace_flush() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let url = format!("{TEST_DOMAIN}strip_flush.html");
                let driver = fixture.driver_for(&url);
                let sink = SharedSink::default();
                let mut parser = Parser::new();
                parser.add_filter(
                    Box::new(JavascriptFilter::new(driver)) as Box<dyn HtmlFilter>
                );
                parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
                parser.start_parse(url.as_bytes(), false);
                parser.write(format!("<script src='{ORIG_JS}'>   \t\n   ").as_bytes());
                parser.flush();
                parser.write(b"</script>");
                parser.finish();
                assert_eq!(
                    String::from_utf8(sink.bytes()).unwrap(),
                    format!("<script src='{}'></script>", fixture.rewritten_leaf())
                );
            }

            /// Source-map matrix (`SourceMapTest`).
            fn source_map_test(
                input_js: &str,
                expected_output_js: &str,
                expected_mapping_vlq: &str,
            ) {
                use ps_cache::hash::Hasher;
                let fixture = Fixture::with_md5($experimental);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}input.js"),
                    input_js.as_bytes(),
                    100,
                );
                let expected_map = format!(
                    ")]}}'\n{{\"mappings\":\"{expected_mapping_vlq}\",\"names\":[],\"sources\":[\"http://test.com/input.js?PageSpeed=off\"],\"version\":3}}\n"
                );
                let map_hash = fixture.server.hasher().hash(expected_map.as_bytes());
                let source_map_url = format!("{TEST_DOMAIN}input.js.pagespeed.sm.{map_hash}.map");
                let mut expected_output = expected_output_js.to_string();
                if $experimental {
                    expected_output.push_str(&format!("\n//# sourceMappingURL={source_map_url}\n"));
                }
                let js_hash = fixture.server.hasher().hash(expected_output.as_bytes());
                let rewritten_js_name = format!("input.js.pagespeed.jm.{js_hash}.js");
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}source_maps.html"),
                        &generate_html("input.js")
                    ),
                    generate_html(&rewritten_js_name)
                );
                let (output_js, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}{rewritten_js_name}"))
                    .expect("js served");
                assert_eq!(output_js, expected_output.as_bytes());
                if $experimental {
                    let (map, _) = fixture.serve_url(&source_map_url).expect("map served");
                    assert_eq!(map, expected_map.as_bytes());
                    // Fresh-server equivalent: cleared caches rebuild.
                    fixture.clear_caches();
                    let (map, _) = fixture.serve_url(&source_map_url).expect("map served");
                    assert_eq!(map, expected_map.as_bytes());
                    // Wrong-hash maps fail with the mismatch message.
                    let wrong_url =
                        format!("{TEST_DOMAIN}input.js.pagespeed.sm.Different.map");
                    for _ in 0..2 {
                        let (map, headers) =
                            fixture.serve_url(&wrong_url).expect("mismatch served");
                        assert_eq!(headers.status_code(), 404);
                        assert_eq!(
                            map,
                            b"Hash from URL does not match rewritten hash.".as_slice()
                        );
                    }
                }
            }

            #[test]
            fn source_maps_simple() {
                source_map_test("  foo  bar  ", "foo bar", "AAAE,IAAK");
            }

            #[test]
            fn source_maps_medium() {
                source_map_test(
                    "alert     (    'hello, world!'    ) \n /* removed */ <!-- removed --> \n // single-line-comment\ndocument.write( \"<!-- comment -->\" );",
                    "alert('hello, world!')\ndocument.write(\"<!-- comment -->\");",
                    "AAAA,KAAU,CAAK,eAAmB;AAGlC,eAAgB,kBAAmB",
                );
            }

            #[test]
            fn source_map_unsanitary_url() {
                let fixture = fixture();
                if !$experimental {
                    return;
                }
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(&format!("{TEST_DOMAIN}input.js"), JS_DATA.as_bytes(), 100);
                fixture.fetcher.set_strip_query_params(true);
                use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
                let encoded = DefaultEncoder.encode(&["input.js?evil=\n".as_bytes().to_vec()]);
                let (output_js, _) = fixture
                    .serve_url(&format!(
                        "{TEST_DOMAIN}{}.pagespeed.jm.0.js",
                        String::from_utf8_lossy(&encoded)
                    ))
                    .expect("served");
                // No newline may leak into the mapping URL.
                let expected = format!(
                    "{JS_MIN_DATA}\n//# sourceMappingURL=http://test.com/input.js,qevil=.pagespeed.sm.0.map\n"
                );
                assert_eq!(output_js, expected.as_bytes());
            }

            #[test]
            fn source_map_on_demand_not_enabled() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .disable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}input.js"),
                    b"  foo  bar  ",
                    100,
                );
                let expected_map = ")]}'\n{\"mappings\":\"AAAE,IAAK\",\"names\":[],\"sources\":[\"http://test.com/input.js?PageSpeed=off\"],\"version\":3}\n";
                let served = fixture.serve_url(&format!("{TEST_DOMAIN}input.js.pagespeed.sm.0.map"));
                if $experimental {
                    assert_eq!(served.expect("served").0, expected_map.as_bytes());
                }
            }

            #[test]
            fn source_map_no_opt() {
                let fixture = fixture();
                if !$experimental {
                    return;
                }
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(&format!("{TEST_DOMAIN}input.js"), b"", 100);
                // Empty contents: no fallback to raw JS for maps.
                for _ in 0..2 {
                    assert!(fixture
                        .serve_url(&format!("{TEST_DOMAIN}input.js.pagespeed.sm.0.map"))
                        .is_none());
                }
            }

            #[test]
            fn inline_and_not_external() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .disable_filter(Filter::RewriteJavascriptExternal);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}inline_not_external.html"),
                        &format!("{}{}", inline_js(JS_DATA), generate_html(ORIG_JS))
                    ),
                    format!("{}{}", inline_js(JS_MIN_DATA), generate_html(ORIG_JS))
                );
            }

            #[test]
            fn inline_and_not_external_preserve() {
                // Preserve-urls must not affect inline minification.
                let fixture = fixture();
                fixture.options.borrow_mut().set_js_preserve_urls(true);
                fixture
                    .options
                    .borrow_mut()
                    .set_in_place_preemptive_rewrite_javascript(false);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .disable_filter(Filter::RewriteJavascriptExternal);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}inline_not_external.html"),
                        &format!("{}{}", inline_js(JS_DATA), generate_html(ORIG_JS))
                    ),
                    format!("{}{}", inline_js(JS_MIN_DATA), generate_html(ORIG_JS))
                );
            }

            #[test]
            fn inline_and_canonical_not_external() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture
                    .options
                    .borrow_mut()
                    .disable_filter(Filter::RewriteJavascriptExternal);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}inline_canonical.html"),
                        &format!("{}{}", inline_js(JS_DATA), generate_html(ORIG_JS))
                    ),
                    format!("{}{}", inline_js(JS_MIN_DATA), generate_html(ORIG_JS))
                );
            }

            #[test]
            fn external_and_not_inline() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .disable_filter(Filter::RewriteJavascriptInline);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}external_not_inline.html"),
                        &format!("{}{}", inline_js(JS_DATA), generate_html(ORIG_JS))
                    ),
                    format!("{}{}", inline_js(JS_DATA), generate_html(&fixture.rewritten_leaf()))
                );
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

            /// Fallback sanitization matrix (shared helper shape).
            fn test_fallback_sanitization(mime: &str, expect_load: bool) {
                let fixture = Fixture::new($experimental);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
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
                use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
                let encoded = DefaultEncoder.encode(&[leaf.as_bytes().to_vec()]);
                let served = fixture.serve_url(&format!(
                    "{TEST_DOMAIN}{}.pagespeed.jm.0.js",
                    String::from_utf8_lossy(&encoded)
                ));
                assert_eq!(served.is_some(), expect_load, "mime {mime}");
                if let Some((content, headers)) = served {
                    assert_eq!(content, b"this isn't a real file");
                    assert_eq!(
                        headers.determine_content_type().map(|content_type| content_type.mime_type().to_string()),
                        Some(mime.to_string())
                    );
                    assert_eq!(
                        headers.headers().lookup1("X-Content-Type-Options"),
                        Some("nosniff".to_string())
                    );
                }
            }

            #[test]
            fn aris() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::Debug);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}introspective.js"),
                    b"var script_tags = document.getElementsByTagName('script');",
                    100,
                );
                let out = fixture.parse(
                    &format!("{TEST_DOMAIN}introspective.html"),
                    &generate_html("introspective.js"),
                );
                assert!(
                    out.contains(&format!(
                        "{}<!--{}-->",
                        generate_html("introspective.js").trim_end(),
                        ps_js::rewrite::INTROSPECTION_COMMENT
                    )),
                    "{out}"
                );
            }

            #[test]
            fn aris_combine_js() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CombineJavascript);
                fixture.options.borrow_mut().enable_filter(Filter::Debug);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}introspective.js"),
                    b"var script_tags = document.getElementsByTagName('script');",
                    100,
                );
                fixture.mock_js(&format!("{TEST_DOMAIN}a.js"), JS_DATA.as_bytes(), 100);
                fixture.mock_js(&format!("{TEST_DOMAIN}b.js"), JS_DATA.as_bytes(), 100);
                let html_before = format!(
                    "{}{}{}",
                    generate_html("introspective.js"),
                    generate_html("a.js"),
                    generate_html("b.js")
                );
                let out = fixture.parse(&format!("{TEST_DOMAIN}introspective"), &html_before);
                let expected = format!(
                    "{}<!--{}-->\n<script src=\"a.js+b.js.pagespeed.jc.0.js\"></script><script>eval(mod_pagespeed_0);</script>\n<script>eval(mod_pagespeed_0);</script>\n",
                    generate_html("introspective.js").trim_end(),
                    ps_js::rewrite::INTROSPECTION_COMMENT
                );
                assert!(out.contains(&expected), "{out}");
            }

            #[test]
            fn dont_combine_identified() {
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CombineJavascript);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                // A registered library rewrites to its canonical URL
                // instead of combining.
                let html_before =
                    format!("{}{}", generate_html(ORIG_JS), generate_html(ORIG_JS));
                let html_after =
                    format!("{}{}", generate_html(LIBRARY_URL), generate_html(LIBRARY_URL));
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}dont_combine_identified"), &html_before),
                    html_after
                );
            }

            #[test]
            fn no_source_map_js_combine() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CombineJavascript);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(&format!("{TEST_DOMAIN}a.js"), JS_DATA.as_bytes(), 100);
                fixture.mock_js(&format!("{TEST_DOMAIN}b.js"), JS_DATA.as_bytes(), 100);
                let html_before =
                    format!("{}{}", generate_html("a.js"), generate_html("b.js"));
                let html_after = "<script src=\"a.js+b.js.pagespeed.jc.0.js\"></script><script>eval(mod_pagespeed_0);</script>\n<script>eval(mod_pagespeed_0);</script>\n";
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}introspective"), &html_before),
                    html_after
                );
                // Note: there is no //# sourceMappingURL in combine output.
                let expected_output = format!(
                    "var mod_pagespeed_0 = \"{JS_MIN_DATA}\";\nvar mod_pagespeed_0 = \"{JS_MIN_DATA}\";\n"
                );
                let (output_js, _) = fixture
                    .serve_url(&format!("{TEST_DOMAIN}a.js+b.js.pagespeed.jc.0.js"))
                    .expect("served");
                assert_eq!(output_js, expected_output.as_bytes());
            }

            #[test]
            fn proper_source_map_for_pagespeed_input() {
                if !$experimental {
                    return;
                }
                use ps_cache::hash::Hasher;
                let fixture = Fixture::with_md5($experimental);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::OutlineJavascript);
                fixture.options.borrow_mut().set_js_outline_min_bytes(0);
                let input_js = "foo = 1;";
                let outlined_tail = format!(
                    "_.pagespeed.jo.{}.js",
                    fixture.server.hasher().hash(input_js.as_bytes())
                );
                // Note: no ?PageSpeed=off for pagespeed inputs.
                let expected_map = format!(
                    ")]}}'\n{{\"mappings\":\"AAAA,GAAI,CAAE\",\"names\":[],\"sources\":[\"{TEST_DOMAIN}{outlined_tail}\"],\"version\":3}}\n"
                );
                // Nested pagespeed names segment-encode (`.pagespeed.` → `,M`).
                use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
                let encoded_tail = String::from_utf8_lossy(
                    &DefaultEncoder.encode(&[outlined_tail.as_bytes().to_vec()]),
                )
                .into_owned();
                let source_map_url = format!(
                    "{TEST_DOMAIN}{encoded_tail}.pagespeed.sm.{}.map",
                    fixture.server.hasher().hash(expected_map.as_bytes())
                );
                let expected_output_js =
                    format!("foo=1;\n//# sourceMappingURL={source_map_url}\n");
                let rewritten_js_url = format!(
                    "{TEST_DOMAIN}{encoded_tail}.pagespeed.jm.{}.js",
                    fixture.server.hasher().hash(expected_output_js.as_bytes())
                );
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}source_map_pagespeed"),
                        &format!("<script>{input_js}</script>")
                    ),
                    format!("<script src=\"{rewritten_js_url}\"></script>")
                );
                let (output_js, _) = fixture.serve_url(&rewritten_js_url).expect("served js");
                assert_eq!(output_js, expected_output_js.as_bytes());
                let (source_map, _) = fixture.serve_url(&source_map_url).expect("served map");
                assert_eq!(source_map, expected_map.as_bytes());
            }

            #[test]
            fn aris_source_maps() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::IncludeJsSourceMaps);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::Debug);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.mock_js(
                    &format!("{TEST_DOMAIN}introspective.js"),
                    b"var script_tags = document.getElementsByTagName('script');",
                    100,
                );
                let out = fixture.parse(
                    &format!("{TEST_DOMAIN}introspective.html"),
                    &generate_html("introspective.js"),
                );
                assert!(
                    out.contains(&format!(
                        "{}<!--{}-->",
                        generate_html("introspective.js").trim_end(),
                        ps_js::rewrite::INTROSPECTION_COMMENT
                    )),
                    "{out}"
                );
            }

            #[test]
            fn dont_rewrite_integrity_script() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = format!("<script src=\"{ORIG_JS}\" integrity=\"sha256-abc\"></script>");
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}integrity.html"), &html), html);
            }

            #[test]
            fn dont_rewrite_module_script() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = format!("<script type=\"module\" src=\"{ORIG_JS}\"></script>");
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}module.html"), &html), html);
            }

            #[test]
            fn dont_rewrite_import_map() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = "<script type=\"importmap\">{\"imports\":{\"/app.js\":\"/app-v2.js\"}}</script>";
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}importmap.html"), html), html);
            }

            #[test]
            fn dont_rewrite_data_block() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                for html in [
                    "<script type=\"application/json\">{\"a\":  1}</script>",
                    "<script type=\"speculationrules\">{\"prerender\":[{\"source\":\"list\",\"urls\":[\"/next\"]}]}</script>",
                ] {
                    assert_eq!(
                        fixture.parse(&format!("{TEST_DOMAIN}datablock.html"), html),
                        html
                    );
                }
            }

            #[test]
            fn minify_nomodule_script() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}nomodule.html"),
                        &format!("<script nomodule src=\"{ORIG_JS}\"></script>")
                    ),
                    format!("<script nomodule src=\"{}\"></script>", fixture.rewritten_leaf())
                );
            }

            #[test]
            fn modern_syntax_template_literal() {
                // Fail-closed until the minifier understands template
                // literals: byte-identical output.
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = "<script>var t = `hello ${name},\nwelcome!`;</script>";
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}template.html"), html), html);
            }

            #[test]
            fn modern_syntax_operators() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                assert_eq!(
                    fixture.parse(
                        &format!("{TEST_DOMAIN}modern.html"),
                        "<script>var x = a?.b ?? c; var y = [...d, ...e];</script>"
                    ),
                    if $experimental {
                        "<script>var x = a?.b ?? c; var y = [...d, ...e];</script>"
                    } else {
                        "<script>var x=a?.b??c;var y=[...d,...e];</script>"
                    }
                );
            }

            #[test]
            fn modern_syntax_class_async() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let input = "<script>class C { f = async () => { await g(); }; }</script>";
                assert_eq!(
                    fixture.parse(&format!("{TEST_DOMAIN}modern.html"), input),
                    if $experimental {
                        input
                    } else {
                        "<script>class C{f=async()=>{await g();};}</script>"
                    }
                );
            }

            #[test]
            fn minify_failure_passthrough() {
                let fixture = fixture();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptInline);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let html = "<script>var x = \"oops;</script>";
                assert_eq!(fixture.parse(&format!("{TEST_DOMAIN}broken.html"), html), html);
                assert_eq!(fixture.stat("javascript_minification_failures"), 1);
            }

            #[test]
            fn dont_inline_identified() {
                // A canonical-redirected library is not inlined after.
                let fixture = fixture();
                fixture.register_library();
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::RewriteJavascriptExternal);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::InlineJavascript);
                fixture
                    .options
                    .borrow_mut()
                    .enable_filter(Filter::CanonicalizeJavascriptLibraries);
                fixture.init_test(100);
                let url = format!("{TEST_DOMAIN}dont_inline_ident.html");
                let driver = fixture.driver_for(&url);
                let sink = SharedSink::default();
                let mut parser = Parser::new();
                // Minification precedes inlining in chain order.
                parser.add_filter(
                    Box::new(JavascriptFilter::new(driver.clone())) as Box<dyn HtmlFilter>
                );
                parser.add_filter(
                    Box::new(ps_html::filters::js_inline::JsInlineFilter::new(driver))
                        as Box<dyn HtmlFilter>,
                );
                parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
                parser.start_parse(url.as_bytes(), false);
                parser.write(generate_html(ORIG_JS).as_bytes());
                parser.finish();
                assert_eq!(
                    String::from_utf8(sink.bytes()).unwrap(),
                    generate_html(LIBRARY_URL)
                );
            }
        }
    };
}

javascript_tests!(experimental, true);
javascript_tests!(legacy, false);
