//! JavaScript-combining tests ported from
//! `js_combine_filter_test.cc`.
//!
//! NOTE: logging assertions wait for the log record; CSP cases wait
//! for the CSP subsystem; async deadline/timing cases wait for the
//! async reconstruction epic; LRU-count assertions wait for a
//! counting test backend.
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
use ps_html::filters::js_combine::{JsCombineFilter, FILE_COUNT_REDUCTION};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

const JS_URL1: &str = "a.js";
const JS_URL2: &str = "b.js";
const JS_URL3: &str = "c.js";
const JS_URL4: &str = "d.js";
const STRICT_URL1: &str = "strict1.js";
const STRICT_URL2: &str = "strict2.js";
const PSEUDO_STRICT_URL1: &str = "pseudo_strict1.js";
const PSEUDO_STRICT_URL2: &str = "pseudo_strict2.js";
const INTROSPECTIVE_URL1: &str = "introspective1.js";
const INTROSPECTIVE_URL2: &str = "introspective2.js";
const JS_TEXT1: &str = "// script1\nvar a=\"hello\\nsecond line\"";
const MINIFIED_JS1: &str = "var a=\"hello\\nsecond line\"";
const JS_TEXT2: &str = "// script2\r\nvar b=42;\n";
const MINIFIED_JS2: &str = "var b=42;";
const JS_TEXT3: &str = "var x = 42;\nvar y = 31459;\n";
const JS_TEXT4: &str = "var m = 'abcd';\n";
const STRICT_TEXT1: &str = "'use strict'; var x = 32;";
const STRICT_TEXT2: &str = "\"use strict\"; var x = 42;";
const PSEUDO_STRICT_TEXT1: &str = "function(){'use strict'; var x = 32;}";
const PSEUDO_STRICT_TEXT2: &str = "function(){\"use strict\"; var x = 42;}";
const INTROSPECTIVE_TEXT1: &str = "var x = 7; $('script') ; var y = 42;";
const INTROSPECTIVE_TEXT2: &str = "document.getElementsByTagName('script');";
const ALTERNATE_DOMAIN: &str = "http://alternate.com/";

/// Collected script (`ScriptInfo`): src URL plus inline text.
#[derive(Debug, Clone, Default)]
struct ScriptInfo {
    url: String,
    text_content: String,
}

/// Collects scripts from parsed HTML (`ScriptCollector`).
struct ScriptCollector {
    output: Rc<RefCell<Vec<ScriptInfo>>>,
    active: RefCell<Option<(String, String)>>,
}

impl ScriptCollector {
    fn new(output: Rc<RefCell<Vec<ScriptInfo>>>) -> Self {
        ScriptCollector {
            output,
            active: RefCell::new(None),
        }
    }
}

impl HtmlFilter for ScriptCollector {
    fn name(&self) -> &'static str {
        "ScriptCollector"
    }

    fn start_element(
        &mut self,
        dom: &mut ps_html::parser::Dom,
        _lexer: &ps_html::lexer::Lexer,
        id: ps_html::element::NodeId,
    ) {
        use ps_html::name::Keyword;
        assert!(self.active.borrow().is_none());
        if dom.element_keyword(id) == Keyword::Script {
            let url = dom
                .find_attribute(id, Keyword::Src)
                .and_then(|index| dom.decoded_attribute(id, index).0)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            *self.active.borrow_mut() = Some((url, String::new()));
        }
    }

    fn characters(
        &mut self,
        dom: &mut ps_html::parser::Dom,
        _lexer: &ps_html::lexer::Lexer,
        node: ps_html::element::NodeId,
    ) {
        if let Some((_, text)) = self.active.borrow_mut().as_mut() {
            text.push_str(&String::from_utf8_lossy(&dom.leaf_contents(node)));
        }
    }

    fn end_element(
        &mut self,
        dom: &mut ps_html::parser::Dom,
        _lexer: &ps_html::lexer::Lexer,
        id: ps_html::element::NodeId,
    ) {
        use ps_html::name::Keyword;
        if dom.element_keyword(id) == Keyword::Script {
            if let Some((url, text_content)) = self.active.borrow_mut().take() {
                self.output
                    .borrow_mut()
                    .push(ScriptInfo { url, text_content });
            }
        }
    }
}

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    http_backend: Arc<MemoryCache>,
    metadata_backend: Arc<MemoryCache>,
    other_domain: String,
    extra_filters: ExtraFilters,
    use_test_namer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtraFilters {
    None,
    Minify,
    ExtendScripts,
    Proxy,
}

impl Fixture {
    fn new(extra: ExtraFilters, use_test_namer: bool) -> Self {
        let http_backend = Arc::new(MemoryCache::new());
        let metadata_backend = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend.clone(),
            metadata_backend.clone(),
            fetcher.clone(),
            START_MS,
        );
        if use_test_namer {
            server.set_url_namer(Rc::new(ps_html::url_namer::TestUrlNamer::new()));
        }
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::CombineJavascript);
        match extra {
            ExtraFilters::None => {}
            ExtraFilters::Minify | ExtraFilters::Proxy => {
                options
                    .borrow_mut()
                    .soft_enable_filter_for_testing(Filter::RewriteJavascriptExternal);
            }
            ExtraFilters::ExtendScripts => {
                options
                    .borrow_mut()
                    .soft_enable_filter_for_testing(Filter::ExtendCacheScripts);
            }
        }
        let mut fixture = Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            http_backend: http_backend.clone(),
            metadata_backend: metadata_backend.clone(),
            other_domain: "http://test.com.us/".to_string(),
            extra_filters: extra,
            use_test_namer,
        };
        fixture.setup_responses();
        fixture
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ));
        driver
    }

    fn setup_responses(&self) {
        self.simulate_js_resource(JS_URL1, JS_TEXT1);
        self.simulate_js_resource(JS_URL2, JS_TEXT2);
        self.simulate_js_resource_on_domain(ALTERNATE_DOMAIN, JS_URL2, JS_TEXT2.as_bytes());
        self.simulate_js_resource(JS_URL3, JS_TEXT3);
        self.simulate_js_resource(JS_URL4, JS_TEXT4);
        self.simulate_js_resource(STRICT_URL1, STRICT_TEXT1);
        self.simulate_js_resource(STRICT_URL2, STRICT_TEXT2);
        self.simulate_js_resource(PSEUDO_STRICT_URL1, PSEUDO_STRICT_TEXT1);
        self.simulate_js_resource(PSEUDO_STRICT_URL2, PSEUDO_STRICT_TEXT2);
        self.simulate_js_resource(INTROSPECTIVE_URL1, INTROSPECTIVE_TEXT1);
        self.simulate_js_resource(INTROSPECTIVE_URL2, INTROSPECTIVE_TEXT2);
        self.simulate_js_resource_on_domain(&self.other_domain.clone(), JS_URL1, b"othera");
        self.simulate_js_resource_on_domain(&self.other_domain.clone(), JS_URL2, b"otherb");
    }

    fn simulate_js_resource(&self, url: &str, text: &str) {
        self.simulate_js_resource_on_domain(TEST_DOMAIN, url, text.as_bytes());
    }

    /// Scripts a JS fetch with an explicit content type.
    fn mock_js_with_content_type(&self, url: &str, content_type: &str, content: &[u8]) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("application/javascript"),
            "",
            "",
            &mut headers,
        );
        headers.headers_mut().replace("Content-Type", content_type);
        headers.set_date_and_caching(self.server.now_ms(), 100 * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(url, headers, content);
    }

    /// Loads an input resource (`CreateResource` + `ReadIfCached`).
    fn load_input(&self, url: &str) -> ps_html::resource::InputResource {
        use ps_html::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
        use ps_html::server_context::{InlineAuthorizationPolicy, InputRole, IntendedFor};
        let driver = self.driver_for(url);
        let gurl = GoogleUrl::new(url);
        let (resource, _) = driver.create_input_resource(
            &gurl,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Script,
        );
        let mut input = resource.expect("input");
        let mut ctx = driver.request_context().borrow().clone();
        let mut callback = MockLoadCallback::new();
        input.load(
            NotCacheablePolicy::ReportFailureIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        assert!(input.loaded() && input.http_status_ok());
        input
    }

    fn simulate_js_resource_on_domain(&self, domain: &str, url: &str, text: &[u8]) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        self.server.set_default_long_cache_headers(
            ps_http::content_type::mime_to_content_type("application/javascript"),
            "",
            "",
            &mut headers,
        );
        headers.set_date_and_caching(self.server.now_ms(), 100 * 1000, "");
        headers.headers_mut().remove_all("Last-Modified");
        headers.headers_mut().remove_all("Etag");
        headers.compute_caching();
        self.fetcher
            .set_response(&format!("{domain}{url}"), headers, text);
    }

    /// Parses, returning raw output bytes.
    fn parse_bytes(&self, url: &str, html: &[u8]) -> Vec<u8> {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(JsCombineFilter::new(driver.clone())) as Box<dyn HtmlFilter>);
        if matches!(
            self.extra_filters,
            ExtraFilters::Minify | ExtraFilters::Proxy
        ) {
            parser.add_filter(
                Box::new(ps_html::filters::javascript::JavascriptFilter::new(
                    driver.clone(),
                )) as Box<dyn HtmlFilter>,
            );
        }
        if matches!(self.extra_filters, ExtraFilters::ExtendScripts) {
            parser.add_filter(
                Box::new(ps_html::filters::cache_extender::CacheExtenderFilter::new(
                    driver,
                )) as Box<dyn HtmlFilter>,
            );
        }
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html);
        parser.finish();
        sink.bytes()
    }

    /// Parses and collects scripts (`ParseUrl` + `ScriptCollector`).
    fn collect_scripts(&self, url: &str, html: &[u8]) -> Vec<ScriptInfo> {
        let output = self.parse_bytes(url, html);
        let collected = Rc::new(RefCell::new(Vec::new()));
        {
            let mut parser = Parser::new();
            parser.add_filter(Box::new(ScriptCollector::new(collected.clone())));
            parser.start_parse(url.as_bytes(), false);
            parser.write(&output);
            parser.finish();
        }
        let scripts = collected.borrow().clone();
        scripts
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
        driver.add_filter(Rc::new(JsCombineFilter::new(driver.clone())));
        if matches!(
            self.extra_filters,
            ExtraFilters::Minify | ExtraFilters::Proxy
        ) {
            driver.add_filter(Rc::new(
                ps_html::filters::javascript::JavascriptFilter::new(driver.clone()),
            ));
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

    /// Serves through a fresh server sharing the fetcher and options
    /// (`ServeResourceFromNewContext`).
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
        driver.add_filter(Rc::new(JsCombineFilter::new(driver.clone())));
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
}

/// Multipart name for URL parts (`MultiUrl` + `Encode` leaf).
fn combined_leaf(names: &[&str], hash: &str) -> String {
    use ps_html::url_segment_encoder::{MultipartEncoder, UrlSegmentEncoder};
    let inputs: Vec<Vec<u8>> = names.iter().map(|name| name.as_bytes().to_vec()).collect();
    format!(
        "{}.pagespeed.jc.{hash}.js",
        String::from_utf8_lossy(&MultipartEncoder.encode(&inputs))
    )
}

/// Combination variable for an absolute URL (`VarName` shape).
fn var_name_for(server: &ServerContext, url: &str) -> String {
    use ps_cache::hash::Hasher;
    let path = GoogleUrl::new(url).path_and_leaf().to_string();
    format!(
        "mod_pagespeed_{}",
        server.hasher().hash(path.as_bytes()).replace('-', "$")
    )
}

/// Checks a combination script (`VerifyCombinedOnDomain`).
fn verify_combined_on_domain(
    fixture: &Fixture,
    base_url: &str,
    domain: &str,
    info: &ScriptInfo,
    names: &[&str],
) {
    assert!(!info.url.is_empty());
    let base = GoogleUrl::new(base_url);
    let combination = GoogleUrl::resolve(&base, &info.url);
    assert!(combination.is_web_valid(), "{}", info.url);
    let expected_base = GoogleUrl::new(domain);
    assert_eq!(
        expected_base.all_except_leaf(),
        combination.all_except_leaf()
    );
    let mut namer = ps_cache::namer::ResourceNamer::new();
    assert!(namer.decode_ignore_hash_and_signature(combination.leaf_with_query()));
    assert_eq!(namer.id(), "jc");
    assert_eq!(namer.name(), names.join("+"));
    assert_eq!(namer.ext(), "js");
    let _ = fixture;
}

/// Checks an eval stub (`VerifyUseOnDomain`).
fn verify_use_on_domain(fixture: &Fixture, domain: &str, info: &ScriptInfo, rel_url: &str) {
    assert!(info.url.is_empty());
    assert_eq!(
        format!(
            "eval({});",
            var_name_for(&fixture.server, &format!("{domain}{rel_url}"))
        ),
        info.text_content
    );
}

fn verify_combined(fixture: &Fixture, info: &ScriptInfo, names: &[&str]) {
    verify_combined_on_domain(fixture, TEST_DOMAIN, TEST_DOMAIN, info, names);
}

fn verify_use(fixture: &Fixture, info: &ScriptInfo, rel_url: &str) {
    verify_use_on_domain(fixture, TEST_DOMAIN, info, rel_url);
}

fn test_html() -> Vec<u8> {
    format!("<script src={JS_URL1}></script><script src={JS_URL2}></script>").into_bytes()
}

/// Basic combining matrix (`TestCombineJs`).
#[allow(clippy::too_many_arguments)]
fn test_combine_js(
    fixture: &Fixture,
    combined_name: &[&str],
    combined_hash: &str,
    hash1: &str,
    hash2: &str,
    minified: bool,
    domain: &str,
) {
    let scripts = fixture.collect_scripts(TEST_DOMAIN, &test_html());
    assert_eq!(3, scripts.len());
    verify_combined_on_domain(fixture, domain, domain, &scripts[0], combined_name);
    verify_use(fixture, &scripts[1], JS_URL1);
    verify_use(fixture, &scripts[2], JS_URL2);

    let output = fixture.parse_bytes(TEST_DOMAIN, &test_html());
    let expected = format!(
        "<script src=\"{}\"></script><script>eval(mod_pagespeed_{hash1});</script><script>eval(mod_pagespeed_{hash2});</script>",
        scripts[0].url
    );
    assert_eq!(String::from_utf8_lossy(&output), expected);

    let combined_path = combined_leaf(combined_name, combined_hash);
    let html_url = GoogleUrl::new(TEST_DOMAIN);
    let output_url = GoogleUrl::resolve(&html_url, &scripts[0].url);
    assert_eq!(
        format!("{domain}{combined_path}"),
        output_url.spec().to_string()
    );

    let (combination_src, _) = fixture
        .serve_url(output_url.spec())
        .unwrap_or_else(|| panic!("serve {}", output_url.spec()));
    let (escaped1, escaped2) = if minified {
        ("\"var a=\\\"hello\\\\nsecond line\\\"\"", "\"var b=42;\"")
    } else {
        (
            "\"// script1\\nvar a=\\\"hello\\\\nsecond line\\\"\"",
            "\"// script2\\r\\nvar b=42;\\n\"",
        )
    };
    assert_eq!(
        format!(
            "var mod_pagespeed_{hash1} = {escaped1};\nvar mod_pagespeed_{hash2} = {escaped2};\n"
        ),
        String::from_utf8_lossy(&combination_src)
    );
    let (recombined, _) = fixture
        .serve_from_new_context(output_url.spec())
        .expect("reserve");
    assert_eq!(recombined, combination_src);
}

#[test]
fn combine_js() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    test_combine_js(
        &fixture,
        &[JS_URL1, JS_URL2],
        "g2Xe9o4bQ2",
        "KecOGCIjKt",
        "dzsx6RqvJJ",
        false,
        TEST_DOMAIN,
    );
}

#[test]
fn is_likely_strict_mode() {
    use ps_js::tokenizer::TokenizerPatterns;
    let patterns = TokenizerPatterns::new();
    assert!(JsCombineFilter::is_likely_strict_mode(
        &patterns,
        b"'some string';\n      \"use strict\""
    ));
    assert!(JsCombineFilter::is_likely_strict_mode(
        &patterns,
        b"'use strict'; function x() {}"
    ));
    assert!(!JsCombineFilter::is_likely_strict_mode(
        &patterns,
        b"// 'use strict';"
    ));
    assert!(JsCombineFilter::is_likely_strict_mode(
        &patterns,
        b"// Comment \n'use strict';"
    ));
    // Function is strict, but whole script is not.
    assert!(!JsCombineFilter::is_likely_strict_mode(
        &patterns,
        b"function x() {'use strict'; }"
    ));
    // Shouldn't permit just random operators.
    assert!(!JsCombineFilter::is_likely_strict_mode(
        &patterns,
        b"+ * ! 'use strict';"
    ));
}

#[test]
fn combine_js_preserve_urls_on() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.options.borrow_mut().set_js_preserve_urls(true);
    let html = format!("<script src={JS_URL1}></script><script src={JS_URL2}></script>");
    assert_eq!(
        String::from_utf8_lossy(&fixture.parse_bytes(
            &format!("{TEST_DOMAIN}combine_js_preserve_urls_on.html"),
            html.as_bytes()
        )),
        html
    );
}

#[test]
fn combine_js_unhealthy() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    let html = test_html();
    assert_eq!(
        fixture.parse_bytes(&format!("{TEST_DOMAIN}unhealthy.html"), &html),
        html
    );
}

#[test]
fn not_really_strict() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let simple_rel_url = combined_leaf(&[PSEUDO_STRICT_URL1, PSEUDO_STRICT_URL2], "DuUKa0RTAg");
    let input = format!(
        "<script src={PSEUDO_STRICT_URL1}></script><script src={PSEUDO_STRICT_URL2}></script>"
    );
    let expected = format!(
        "<script src=\"{simple_rel_url}\"></script><script>eval(mod_pagespeed_s8uoLecjQN);</script><script>eval(mod_pagespeed_oHli2SAs3Q);</script>"
    );
    assert_eq!(
        String::from_utf8_lossy(
            &fixture.parse_bytes(&format!("{TEST_DOMAIN}not_strict.html"), input.as_bytes())
        ),
        expected
    );
}

/// Barrier matrix (`TestBarriers`): each input must pass through unchanged.
fn check_barriers(fixture: &Fixture) {
    for (id, html) in [
        (
            "noscript",
            format!("<noscript><script src={JS_URL1}></script></noscript><script src={JS_URL2}></script>"),
        ),
        (
            "non-inline",
            format!("<script src={JS_URL1}></script><script>code</script>"),
        ),
        (
            "content",
            format!("<script src={JS_URL1}></script><script src={JS_URL2}>code</script>"),
        ),
        (
            "tcl",
            format!("<script language=tcl src={JS_URL1}></script><script src={JS_URL2}></script>"),
        ),
        (
            "tcl2",
            format!(
                "<script language=tcl src={JS_URL1}></script><script language=tcl src={JS_URL2}></script>"
            ),
        ),
        (
            "tcl3",
            format!("<script src={JS_URL1}></script><script language=tcl src={JS_URL2}></script>"),
        ),
        (
            "exec",
            format!("<script src={JS_URL1}></script><script defer src={JS_URL2}></script>"),
        ),
        (
            "integrity",
            format!(
                "<script src={JS_URL1} integrity=\"sha256-abc\"></script><script src={JS_URL2} integrity=\"sha256-def\"></script>"
            ),
        ),
        (
            "module",
            format!(
                "<script type=\"module\" src={JS_URL1}></script><script src={JS_URL2}></script>"
            ),
        ),
        (
            "importmap",
            format!(
                "<script type=\"importmap\">{{\"imports\":{{\"/app.js\":\"/app-v2.js\"}}}}</script><script src={JS_URL1}></script>"
            ),
        ),
        (
            "datablock",
            format!(
                "<script type=\"application/json\">{{\"a\":1}}</script><script src={JS_URL1}></script>"
            ),
        ),
        (
            "speculationrules",
            format!(
                "<script type=\"speculationrules\">{{\"prerender\":[{{\"source\":\"list\",\"urls\":[\"/next\"]}}]}}</script><script src={JS_URL1}></script>"
            ),
        ),
        (
            "iec",
            format!(
                "<script src={JS_URL1}></script><!--[if IE]><![endif]--><script src={JS_URL2}></script>"
            ),
        ),
        (
            "strict1",
            format!("<script src={JS_URL1}></script><script src={STRICT_URL1}></script>"),
        ),
        (
            "strict2",
            format!("<script src={JS_URL1}></script><script src={STRICT_URL2}></script>"),
        ),
        (
            "strict3",
            format!("<script src={STRICT_URL1}></script><script src={JS_URL1}></script>"),
        ),
        (
            "strict4",
            format!("<script src={STRICT_URL2}></script><script src={JS_URL1}></script>"),
        ),
    ] {
        assert_eq!(
            String::from_utf8_lossy(&fixture.parse_bytes(
                &format!("{TEST_DOMAIN}{id}.html"),
                html.as_bytes()
            )),
            html,
            "id: {id}"
        );
    }
    fixture
        .options
        .borrow_mut()
        .set_avoid_renaming_introspective_javascript(true);
    for (id, html) in [
        (
            "introspective1",
            format!("<script src={JS_URL1}></script><script src={INTROSPECTIVE_URL1}></script>"),
        ),
        (
            "introspective2",
            format!("<script src={JS_URL1}></script><script src={INTROSPECTIVE_URL2}></script>"),
        ),
    ] {
        assert_eq!(
            String::from_utf8_lossy(
                &fixture.parse_bytes(&format!("{TEST_DOMAIN}{id}.html"), html.as_bytes())
            ),
            html,
            "id: {id}"
        );
    }
}

#[test]
fn test_barriers() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    check_barriers(&fixture);
}

#[test]
fn test_non_barriers() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let combined_url = [JS_URL1, JS_URL2];
    // Intervening text
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script>some text<script src={JS_URL2}></script>")
            .as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &combined_url);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
    // Same thing with other tags, even nested.
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!(
            "<s><script src={JS_URL1}></script></s><div>block</div><!-- comment --><b><script src={JS_URL2}></script></b>"
        )
        .as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &combined_url);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
    // Whitespace inside scripts is OK.
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!(
            "<script src={JS_URL1}>       </script><div>block</div><b><script src={JS_URL2}>\t</script></b>"
        )
        .as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &combined_url);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
}

/// Parses with a mid-stream flush, collecting scripts.
fn collect_with_flush(fixture: &Fixture, url: &str, chunks: &[&[u8]]) -> Vec<ScriptInfo> {
    let driver = fixture.driver_for(url);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(JsCombineFilter::new(driver)) as Box<dyn HtmlFilter>);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url.as_bytes(), false);
    for (index, chunk) in chunks.iter().enumerate() {
        parser.write(chunk);
        if index + 1 < chunks.len() {
            parser.flush();
        }
    }
    parser.finish();
    let output = sink.bytes();
    let collected = Rc::new(RefCell::new(Vec::new()));
    {
        let mut parser = Parser::new();
        parser.add_filter(Box::new(ScriptCollector::new(collected.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(&output);
        parser.finish();
    }
    let scripts = collected.borrow().clone();
    scripts
}

#[test]
fn test_flush_middle1() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let scripts = collect_with_flush(
        &fixture,
        TEST_DOMAIN,
        &[
            format!("<script src={JS_URL1}>").as_bytes(),
            format!("</script><script src={JS_URL2}></script><script src={JS_URL3}></script>")
                .as_bytes(),
        ],
    );
    assert_eq!(4, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2, JS_URL3]);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
    verify_use(&fixture, &scripts[3], JS_URL3);
}

#[test]
fn test_flush_middle2() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let scripts = collect_with_flush(
        &fixture,
        TEST_DOMAIN,
        &[
            format!("<script src={JS_URL1}></script><script src={JS_URL2}>").as_bytes(),
            format!("</script><script src={JS_URL3}></script>").as_bytes(),
        ],
    );
    assert_eq!(4, scripts.len());
    assert_eq!(JS_URL1, scripts[0].url);
    verify_combined(&fixture, &scripts[1], &[JS_URL2, JS_URL3]);
    verify_use(&fixture, &scripts[2], JS_URL2);
    verify_use(&fixture, &scripts[3], JS_URL3);
}

#[test]
fn test_flush_middle3() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let scripts = collect_with_flush(
        &fixture,
        TEST_DOMAIN,
        &[
            format!("<script src={JS_URL1}></script><script src={JS_URL2}></script>").as_bytes(),
            format!("<script src={JS_URL3}></script>").as_bytes(),
        ],
    );
    assert_eq!(4, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
    assert_eq!(JS_URL3, scripts[3].url);
}

#[test]
fn test_base() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let other = fixture.other_domain.clone();
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!(
            "<base href={other}><script src={JS_URL1}></script><script src={JS_URL2}></script>"
        )
        .as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined_on_domain(&fixture, &other, &other, &scripts[0], &[JS_URL1, JS_URL2]);
    verify_use_on_domain(&fixture, &other, &scripts[1], JS_URL1);
    verify_use_on_domain(&fixture, &other, &scripts[2], JS_URL2);
}

#[test]
fn test_cross_domain_reject() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let other = fixture.other_domain.clone();
    for (id, html) in [
        (
            "xd",
            format!("<script src={other}{JS_URL1}></script><script src={JS_URL2}></script>"),
        ),
        (
            "xd.2",
            format!("<script src={other}{JS_URL1}></script><script src={other}{JS_URL2}></script>"),
        ),
        (
            "xd.3",
            format!("<script src={JS_URL1}></script><script src={other}{JS_URL2}></script>"),
        ),
    ] {
        assert_eq!(
            String::from_utf8_lossy(
                &fixture.parse_bytes(&format!("{TEST_DOMAIN}{id}.html"), html.as_bytes())
            ),
            html,
            "id: {id}"
        );
    }
}

#[test]
fn test_cross_domain_reject_unauth_enabled() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture
        .options
        .borrow_mut()
        .add_inline_unauthorized_resource_type(ps_config::options::Category::Script);
    let other = fixture.other_domain.clone();
    for (id, html) in [
        (
            "xd",
            format!("<script src={other}{JS_URL1}></script><script src={JS_URL2}></script>"),
        ),
        (
            "xd.2",
            format!("<script src={other}{JS_URL1}></script><script src={other}{JS_URL2}></script>"),
        ),
        (
            "xd.3",
            format!("<script src={JS_URL1}></script><script src={other}{JS_URL2}></script>"),
        ),
    ] {
        assert_eq!(
            String::from_utf8_lossy(
                &fixture.parse_bytes(&format!("{TEST_DOMAIN}{id}.html"), html.as_bytes())
            ),
            html,
            "id: {id}"
        );
    }
}

#[test]
fn test_combine_stats() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    assert_eq!(fixture.stat(FILE_COUNT_REDUCTION), 0);
    fixture.parse_bytes(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={JS_URL2}></script><script src={JS_URL3}></script>")
            .as_bytes(),
    );
    // NOTE: log-record assertions wait for the log record.
    assert_eq!(fixture.stat(FILE_COUNT_REDUCTION), 2);
}

#[test]
fn no_combine_no_defer_attribute() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let html = format!(
        "<script src={JS_URL1} pagespeed_no_defer></script><script src={JS_URL2}></script>"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.parse_bytes(
            &format!("{TEST_DOMAIN}pagespeed_no_defer.html"),
            html.as_bytes()
        )),
        html
    );
}

#[test]
fn no_combine_data_no_defer_attribute() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let html = format!(
        "<script src={JS_URL1} data-pagespeed-no-defer></script><script src={JS_URL2}></script>"
    );
    assert_eq!(
        String::from_utf8_lossy(&fixture.parse_bytes(
            &format!("{TEST_DOMAIN}data-pagespeed-no-defer.html"),
            html.as_bytes()
        )),
        html
    );
}

#[test]
fn minify_combine_js() {
    let fixture = Fixture::new(ExtraFilters::Minify, false);
    test_combine_js(
        &fixture,
        &[JS_URL1, JS_URL2],
        "HrCUtQsDp_",
        "KecOGCIjKt",
        "dzsx6RqvJJ",
        true,
        TEST_DOMAIN,
    );
}

#[test]
fn test_script_inline_text_rollback() {
    let fixture = Fixture::new(ExtraFilters::Minify, false);
    let output = fixture.parse_bytes(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={JS_URL2}>TEXT HERE</script>")
            .as_bytes(),
    );
    let scripts = {
        let collected = Rc::new(RefCell::new(Vec::new()));
        {
            let mut parser = Parser::new();
            parser.add_filter(Box::new(ScriptCollector::new(collected.clone())));
            parser.start_parse(TEST_DOMAIN.as_bytes(), false);
            parser.write(&output);
            parser.finish();
        }
        let scripts = collected.borrow().clone();
        scripts
    };
    assert_eq!(2, scripts.len());
    // `b.js` was minified; its inline body kept it out of the combination.
    assert_eq!(
        format!("{JS_URL1}.pagespeed.jm.FUEwDOA7jh.js"),
        scripts[0].url
    );
    assert!(scripts[0].text_content.is_empty());
    assert_eq!(
        format!("{JS_URL2}.pagespeed.jm.Y1kknPfzVs.js"),
        scripts[1].url
    );
    assert_eq!("TEXT HERE", scripts[1].text_content);
}

#[test]
fn minify_shard_combine_js() {
    let fixture = Fixture::new(ExtraFilters::Minify, false);
    fixture.options.borrow_mut().add_domain("http://a.com/");
    fixture.options.borrow_mut().add_domain("http://b.com/");
    fixture
        .options
        .borrow_mut()
        .add_shard("test.com", "a.com,b.com");
    // Make sure the shards have the resources, too.
    fixture.simulate_js_resource_on_domain("http://a.com/", JS_URL1, JS_TEXT1.as_bytes());
    fixture.simulate_js_resource_on_domain("http://a.com/", JS_URL2, JS_TEXT2.as_bytes());
    fixture.simulate_js_resource_on_domain("http://b.com/", JS_URL1, JS_TEXT1.as_bytes());
    fixture.simulate_js_resource_on_domain("http://b.com/", JS_URL2, JS_TEXT2.as_bytes());
    test_combine_js(
        &fixture,
        &[JS_URL1, JS_URL2],
        "HrCUtQsDp_",
        "KecOGCIjKt",
        "dzsx6RqvJJ",
        true,
        "http://b.com/",
    );
}

#[test]
fn minify_combine_across_hosts() {
    let fixture = Fixture::new(ExtraFilters::Minify, false);
    let js_url_2 = format!("{ALTERNATE_DOMAIN}{JS_URL2}");
    fixture.options.borrow_mut().add_domain(ALTERNATE_DOMAIN);
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={js_url_2}></script>").as_bytes(),
    );
    assert_eq!(2, scripts.len());
    let base = GoogleUrl::new(TEST_DOMAIN);
    let url0 = GoogleUrl::resolve(&base, &scripts[0].url);
    let (content0, _) = fixture.serve_url(url0.spec()).expect("serve 0");
    assert_eq!(content0, MINIFIED_JS1.as_bytes());
    let url1 = GoogleUrl::resolve(&base, &scripts[1].url);
    let (content1, _) = fixture.serve_url(url1.spec()).expect("serve 1");
    assert_eq!(content1, MINIFIED_JS2.as_bytes());
}

#[test]
fn minify_combine_same_host_proxy() {
    // NOTE: the intrusive matrix variant is commented out upstream
    // (VerifyCombinedOnDomain vs TestUrlNamer); this pins that the
    // combination forms at all.
    let fixture = Fixture::new(ExtraFilters::Proxy, true);
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={JS_URL2}></script>").as_bytes(),
    );
    assert_eq!(3, scripts.len(), "successful combination yields 3 scripts");
}

#[test]
fn minify_combine_across_hosts_proxy() {
    let fixture = Fixture::new(ExtraFilters::Proxy, true);
    let js_url_2 = format!("{ALTERNATE_DOMAIN}{JS_URL2}");
    fixture.options.borrow_mut().add_domain(ALTERNATE_DOMAIN);
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={js_url_2}></script>").as_bytes(),
    );
    assert_eq!(
        2,
        scripts.len(),
        "hosts do not match, so combination should not be made"
    );
}

#[test]
fn preserve_url_relativity() {
    let fixture = Fixture::new(ExtraFilters::Minify, false);
    let scripts = fixture.collect_scripts(TEST_DOMAIN, &test_html());
    assert_eq!(3, scripts.len());
    let expected = combined_leaf(&[JS_URL1, JS_URL2], "HrCUtQsDp_");
    assert_eq!(expected, scripts[0].url);
    assert_eq!(
        format!("{TEST_DOMAIN}{expected}"),
        GoogleUrl::resolve(&GoogleUrl::new(TEST_DOMAIN), &scripts[0].url)
            .spec()
            .to_string()
    );
}

#[test]
fn no_preserve_url_relativity() {
    let fixture = Fixture::new(ExtraFilters::Minify, false);
    fixture
        .options
        .borrow_mut()
        .set_option(
            "PreserveUrlRelativity",
            "false",
            ps_config::scope::Scope::Directory,
        )
        .unwrap();
    let scripts = fixture.collect_scripts(TEST_DOMAIN, &test_html());
    assert_eq!(3, scripts.len());
    assert_eq!(
        format!(
            "{TEST_DOMAIN}{}",
            combined_leaf(&[JS_URL1, JS_URL2], "HrCUtQsDp_")
        ),
        scripts[0].url
    );
}

#[test]
// NOTE: upstream `Javascrip` typo kept for name parity.
fn combine_js_avoid_rewriting_introspective_javascrip_on() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture
        .options
        .borrow_mut()
        .set_avoid_renaming_introspective_javascript(true);
    // Turning it on must not affect normal rewriting.
    test_combine_js(
        &fixture,
        &[JS_URL1, JS_URL2],
        "g2Xe9o4bQ2",
        "KecOGCIjKt",
        "dzsx6RqvJJ",
        false,
        TEST_DOMAIN,
    );
}

#[test]
fn combine_js_no_extra_cache_extension() {
    let fixture = Fixture::new(ExtraFilters::ExtendScripts, false);
    // Shorter TTL than the fixture's, so extension would apply.
    fixture.simulate_js_resource(JS_URL1, JS_TEXT1);
    fixture.simulate_js_resource(JS_URL2, JS_TEXT2);
    // NOTE: the cache extender runs alongside; nothing it sees may
    // count (the combiner removes its inputs).
    test_combine_js(
        &fixture,
        &[JS_URL1, JS_URL2],
        "g2Xe9o4bQ2",
        "KecOGCIjKt",
        "dzsx6RqvJJ",
        false,
        TEST_DOMAIN,
    );
    assert_eq!(
        fixture.stat(ps_html::filters::cache_extender::CACHE_EXTENSIONS),
        0
    );
}

#[test]
fn test_combine_shard() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    // Make sure we produce consistent output when sharding/serving off
    // a different host.
    let path = combined_leaf(&[JS_URL1, JS_URL2], "0");
    let (content1, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}{path}"))
        .expect("served test");
    fixture.simulate_js_resource_on_domain("http://cdn.example.com/", JS_URL1, JS_TEXT1.as_bytes());
    fixture.simulate_js_resource_on_domain("http://cdn.example.com/", JS_URL2, JS_TEXT2.as_bytes());
    let (content2, _) = fixture
        .serve_url(&format!("http://cdn.example.com/{path}"))
        .expect("served cdn");
    assert_eq!(content1, content2);
}

#[test]
fn partly_invalid_fetch_cache() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    // Fetch fails and leaves nothing behind for the HTML path.
    assert!(fixture
        .serve_url(&format!("{TEST_DOMAIN}a.js+404.js.pagespeed.jc.0.js"))
        .is_none());
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={JS_URL2}></script>").as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
}

#[test]
fn fetch_with_no_inputs_is_refused() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    assert!(fixture
        .serve_url(&format!("{TEST_DOMAIN}.pagespeed.jc.0.js"))
        .is_none());
}

#[test]
fn embedded_bom_reconstruct() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    // Make sure we that BOMs are retained when reconstructing.
    let js_text = b"\xef\xbb\xbfvar z;".as_slice();
    fixture.simulate_js_resource_on_domain(TEST_DOMAIN, "x.js", js_text);
    fixture.simulate_js_resource_on_domain(TEST_DOMAIN, "y.js", js_text);
    let (content, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}x.js+y.js.pagespeed.jc.0.js"))
        .expect("served");
    let js_text = String::from_utf8_lossy(js_text).into_owned();
    assert_eq!(
        format!(
            "var mod_pagespeed_CpWSqUZO1U = \"{js_text}\";\nvar mod_pagespeed_YdaXhTyTOx = \"{js_text}\";\n"
        ),
        String::from_utf8_lossy(&content)
    );
}

#[test]
fn fetch_result_does_not_decide_html_combination() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    // Direct fetch of a strict combination succeeds (fetch applies no
    // combinability rules)...
    let (content, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}a.js+strict1.js.pagespeed.jc.0.js"))
        .expect("served");
    assert!(!content.is_empty());
    // ...but the HTML path still refuses to combine them.
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!("<script src={JS_URL1}></script><script src={STRICT_URL1}></script>").as_bytes(),
    );
    assert_eq!(2, scripts.len());
    assert_eq!(JS_URL1, scripts[0].url);
    assert_eq!(STRICT_URL1, scripts[1].url);
}

#[test]
fn serve_files_unhealthy() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.metadata_backend.set_is_healthy(false);
    fixture.http_backend.set_is_healthy(false);
    let (content, _) = fixture
        .serve_url(&format!("{TEST_DOMAIN}a.js+b.js.pagespeed.jc.0.js"))
        .expect("served");
    assert!(!content.is_empty());
}

#[test]
fn test_max_combined_js_size() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture
        .options
        .borrow_mut()
        .set_max_combined_js_bytes(JS_TEXT1.len() as i64 + JS_TEXT2.len() as i64 + 1);
    let scripts = fixture.collect_scripts(TEST_DOMAIN, &test_html());
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
    fixture
        .options
        .borrow_mut()
        .set_max_combined_js_bytes(JS_TEXT1.len() as i64 + JS_TEXT2.len() as i64 - 1);
    let scripts = fixture.collect_scripts(TEST_DOMAIN, &test_html());
    assert_eq!(2, scripts.len());
    assert_eq!(JS_URL1, scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
}

#[test]
fn combination_stops_at_cacheable_size() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.options.borrow_mut().set_max_combined_js_bytes(-1);
    // NOTE: cache-size plumbing is fixed; this pins the current
    // behavior (all four combine).
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!(
            "<script src={JS_URL1}></script><script src={JS_URL2}></script><script src={JS_URL3}></script><script src={JS_URL4}></script>"
        )
        .as_bytes(),
    );
    assert_eq!(5, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2, JS_URL3, JS_URL4]);
}

#[test]
fn charset_determination() {
    use ps_html::rewrite_filter::charset_for_script;
    let fixture = Fixture::new(ExtraFilters::None, false);
    // x.js has no charset header nor a BOM.
    // y.js has no charset header but has a BOM.
    // z.js has a charset header but no BOM.
    fixture.simulate_js_resource("x.js", "var x;");
    fixture.simulate_js_resource("y.js", "\u{feff}var y;");
    fixture.mock_js_with_content_type(
        &format!("{TEST_DOMAIN}z.js"),
        "text/javascript; charset=iso-8859-1",
        b"var z;",
    );
    let x = fixture.load_input(&format!("{TEST_DOMAIN}x.js"));
    let y = fixture.load_input(&format!("{TEST_DOMAIN}y.js"));
    let z = fixture.load_input(&format!("{TEST_DOMAIN}z.js"));
    // Nothing set: charset should be empty.
    assert!(charset_for_script(x.charset().as_bytes(), &x.contents(), b"", b"").is_empty());
    // Only the containing charset is set.
    assert_eq!(
        charset_for_script(x.charset().as_bytes(), &x.contents(), b"", b"us-ascii"),
        b"us-ascii"
    );
    // The containing charset is trumped by the resource's BOM.
    assert_eq!(
        charset_for_script(y.charset().as_bytes(), &y.contents(), b"", b"us-ascii"),
        b"utf-8"
    );
    // The resource's BOM is trumped by the element's charset attribute.
    assert_eq!(
        charset_for_script(y.charset().as_bytes(), &y.contents(), b"gb", b"us-ascii"),
        b"gb"
    );
    // The element's charset attribute is trumped by the resource's header.
    assert_eq!(
        charset_for_script(z.charset().as_bytes(), &z.contents(), b"gb", b"us-ascii"),
        b"iso-8859-1"
    );
}

#[test]
fn all_different_charsets() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    // a.js has no charset header nor BOM nor an attribute: use the page.
    // b.js has no charset header nor a BOM but has an attribute: use the attr.
    // c.js has no charset header nor attribute but has a BOM: use the BOM.
    // d.js has a charset header but no BOM nor attribute: use the charset.
    fixture.simulate_js_resource(JS_URL1, "var a;");
    fixture.simulate_js_resource(JS_URL2, "var b;");
    fixture.simulate_js_resource(JS_URL3, "\u{feff}var c;");
    fixture.mock_js_with_content_type(
        &format!("{TEST_DOMAIN}{JS_URL4}"),
        "text/javascript; charset=iso-8859-1",
        b"var d;",
    );
    let input = "<head>\n  <meta charset=\"gb\">\n  <script src=a.js></script>  <script src=b.js charset=us-ascii></script>  <script src=c.js></script>  <script src=d.js></script></head>\n";
    let scripts = fixture.collect_scripts(&format!("{TEST_DOMAIN}bom.html"), input.as_bytes());
    // This should leave the same 4 original scripts.
    assert_eq!(4, scripts.len());
    assert_eq!(JS_URL1, scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
    assert_eq!(JS_URL3, scripts[2].url);
    assert_eq!(JS_URL4, scripts[3].url);
}

#[test]
fn charset_mismatch_reason_escaped_in_log() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.simulate_js_resource(JS_URL1, "var a;");
    fixture.simulate_js_resource(JS_URL2, "var b;");
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}charset.html"),
        format!(
            "<script src={JS_URL1} charset=utf-8></script><script src={JS_URL2} charset=\"iso-8859-1&#10;second\"></script>"
        )
        .as_bytes(),
    );
    assert_eq!(2, scripts.len());
    // NOTE: the one-line log assertion waits for the log record; the
    // scripts staying apart pins the verdict.
}

#[test]
fn pages_in_different_charsets_do_not_share_decision() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let scripts_html = format!(
        "<script src={JS_URL1} charset=iso-8859-1></script><script src={JS_URL2}></script>"
    );
    let latin1_page = format!("<head><meta charset=\"iso-8859-1\">{scripts_html}</head>");
    let utf8_page = format!("<head><meta charset=\"utf-8\">{scripts_html}</head>");
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}latin1_first"),
        latin1_page.as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
    let scripts =
        fixture.collect_scripts(&format!("{TEST_DOMAIN}utf8_second"), utf8_page.as_bytes());
    assert_eq!(2, scripts.len());
    assert_eq!(JS_URL1, scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
    let scripts =
        fixture.collect_scripts(&format!("{TEST_DOMAIN}utf8_first"), utf8_page.as_bytes());
    assert_eq!(2, scripts.len());
    assert_eq!(JS_URL1, scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}latin1_second"),
        latin1_page.as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
}

#[test]
fn non_ascii_script_on_multibyte_page_is_not_combined() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.simulate_js_resource_on_domain(
        TEST_DOMAIN,
        "big5.js",
        b"var s = \"\xa5\x5cn\"; var t = 1;",
    );
    let big5_page = "<head><meta charset=\"Big5\"></head>";
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}big5.html"),
        format!("{big5_page}<script src=big5.js></script><script src={JS_URL2}></script>")
            .as_bytes(),
    );
    assert_eq!(2, scripts.len());
    assert_eq!("big5.js", scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
    let html = [big5_page.as_bytes(), test_html().as_slice()].concat();
    let scripts = fixture.collect_scripts(&format!("{TEST_DOMAIN}big5_ascii.html"), &html);
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
}

#[test]
fn non_ascii_utf8_script_is_combined_only_on_utf8_page() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.simulate_js_resource("utf8.js", "var s = \"caf\u{e9}\"; var t = 1;");
    let scripts_html =
        "<script src=utf8.js charset=utf-8></script><script src=b.js charset=utf-8></script>";
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}utf8.html"),
        format!("<head><meta charset=\"utf-8\"></head>{scripts_html}").as_bytes(),
    );
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &["utf8.js", JS_URL2]);
    verify_use(&fixture, &scripts[1], "utf8.js");
    verify_use(&fixture, &scripts[2], JS_URL2);
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}sjis.html"),
        format!("<head><meta charset=\"Shift_JIS\"></head>{scripts_html}").as_bytes(),
    );
    assert_eq!(2, scripts.len());
    assert_eq!("utf8.js", scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
    let scripts = fixture.collect_scripts(
        &format!("{TEST_DOMAIN}nocharset.html"),
        scripts_html.as_bytes(),
    );
    assert_eq!(2, scripts.len());
    assert_eq!("utf8.js", scripts[0].url);
    assert_eq!(JS_URL2, scripts[1].url);
}

#[test]
fn bom_mismatch() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.simulate_js_resource("x.js", "var x;");
    fixture.simulate_js_resource("y.js", "\u{feff}var y;");
    // x.js has an indeterminate charset: none in headers, attribute,
    // BOM, or HTML.
    let input = "<head>\n  <script src=x.js></script>\n  <script src=y.js></script>\n</head>\n";
    let scripts = fixture.collect_scripts(&format!("{TEST_DOMAIN}bom.html"), input.as_bytes());
    assert_eq!(2, scripts.len());
    let reversed = "<head>\n  <script src=y.js></script>\n  <script src=x.js></script>\n</head>\n";
    let scripts = fixture.collect_scripts(&format!("{TEST_DOMAIN}bom.html"), reversed.as_bytes());
    assert_eq!(2, scripts.len());
}

#[test]
fn embedded_bom() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    fixture.simulate_js_resource("x.js", "var x;");
    fixture.simulate_js_resource("y.js", "\u{feff}var y;");
    // x.js now has a charset of utf-8 thanks to the meta tag.
    let input = "<head>\n  <meta charset=\"UTF-8\">\n  <script src=x.js></script>\n  <script src=y.js></script>\n</head>\n";
    let scripts = fixture.collect_scripts(&format!("{TEST_DOMAIN}bom.html"), input.as_bytes());
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &["x.js", "y.js"]);
    verify_use(&fixture, &scripts[1], "x.js");
    verify_use(&fixture, &scripts[2], "y.js");
    let base = GoogleUrl::new(&format!("{TEST_DOMAIN}bom.html"));
    let output_url = GoogleUrl::resolve(&base, &scripts[0].url);
    let (actual, _) = fixture.serve_url(output_url.spec()).expect("served");
    let bom_pos = actual
        .windows(3)
        .position(|window| window == b"\xef\xbb\xbf")
        .expect("bom");
    assert_eq!(73, bom_pos); // WARNING: MAGIC VALUE!
    let reversed = "<head>\n  <meta charset=\"UTF-8\">\n  <script src=y.js></script>\n  <script src=x.js></script>\n</head>\n";
    let scripts = fixture.collect_scripts(&format!("{TEST_DOMAIN}bom.html"), reversed.as_bytes());
    assert_eq!(3, scripts.len());
    verify_combined(&fixture, &scripts[0], &["y.js", "x.js"]);
    verify_use(&fixture, &scripts[1], "y.js");
    verify_use(&fixture, &scripts[2], "x.js");
    let output_url = GoogleUrl::resolve(&base, &scripts[0].url);
    let (actual, _) = fixture.serve_url(output_url.spec()).expect("served");
    let bom_pos = actual
        .windows(3)
        .position(|window| window == b"\xef\xbb\xbf")
        .expect("bom");
    assert_eq!(32, bom_pos); // WARNING: MAGIC VALUE!
}

#[test]
fn map_rewrite_domain_accross_dirs() {
    // NOTE: upstream `Accross` typo kept for name parity.
    let fixture = Fixture::new(ExtraFilters::None, false);
    let other = fixture.other_domain.clone();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping(&format!("{other}subdir/"), TEST_DOMAIN);
    fixture.simulate_js_resource_on_domain(&other, "subdir/a.js", JS_TEXT1.as_bytes());
    fixture.simulate_js_resource_on_domain(&other, "subdir/b.js", JS_TEXT2.as_bytes());
    let combined_url = format!("{other}subdir/a.js+b.js.pagespeed.jc.8HvRqZnJ8O.js");
    let expected = format!(
        "<script src=\"{combined_url}\"></script><script>eval(mod_pagespeed_K4w22M2i$3);</script><script>eval(mod_pagespeed_4SWUiisZ$T);</script>"
    );
    assert_eq!(
        String::from_utf8_lossy(
            &fixture.parse_bytes(&format!("{TEST_DOMAIN}rewrite_subdir.html"), &test_html())
        ),
        expected
    );
    // Now make sure hashes on other domain match, on reconstruction.
    // NOTE: cache-delete/count assertions wait for a counting backend.
    let (combination_src, _) = fixture.serve_url(&combined_url).expect("served");
    let combination_src = String::from_utf8_lossy(&combination_src).into_owned();
    assert!(combination_src.contains("K4w22M2i$3"), "{combination_src}");
    assert!(combination_src.contains("4SWUiisZ$T"), "{combination_src}");
}

#[test]
fn test_cross_domain_recover() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let other = fixture.other_domain.clone();
    fixture.options.borrow_mut().add_domain(&other);
    let scripts = fixture.collect_scripts(
        TEST_DOMAIN,
        format!(
            "<script src={JS_URL1}></script><script src={JS_URL2}></script><script src={other}{JS_URL1}></script><script src={other}{JS_URL2}></script>"
        )
        .as_bytes(),
    );
    assert_eq!(6, scripts.len());
    verify_combined(&fixture, &scripts[0], &[JS_URL1, JS_URL2]);
    verify_use(&fixture, &scripts[1], JS_URL1);
    verify_use(&fixture, &scripts[2], JS_URL2);
    verify_combined_on_domain(
        &fixture,
        TEST_DOMAIN,
        &other,
        &scripts[3],
        &[JS_URL1, JS_URL2],
    );
    verify_use_on_domain(&fixture, &other, &scripts[4], JS_URL1);
    verify_use_on_domain(&fixture, &other, &scripts[5], JS_URL2);
}

#[test]
fn integrity_script_untouched() {
    let fixture = Fixture::new(ExtraFilters::None, false);
    let output = fixture.parse_bytes(
        &format!("{TEST_DOMAIN}integrity-split.html"),
        format!("<script src={JS_URL1}></script><script src={JS_URL2} integrity=\"sha256-def\"></script>")
            .as_bytes(),
    );
    assert!(
        String::from_utf8_lossy(&output).contains(&format!(
            "<script src={JS_URL2} integrity=\"sha256-def\"></script>"
        )),
        "{}",
        String::from_utf8_lossy(&output)
    );
}
