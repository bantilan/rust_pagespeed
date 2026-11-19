//! JavaScript-outlining tests ported from
//! `js_outline_filter_test.cc`.
//!
//! NOTE: CSP cases wait for the CSP subsystem.
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
use ps_html::filters::js_outline::JsOutlineFilter;
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::writer::WriterFilter;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::mock::MockUrlFetcher;
use ps_http::response::ResponseHeaders;

const TEST_DOMAIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    fetcher: Rc<MockUrlFetcher>,
    debug_message: RefCell<String>,
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
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new_with_metadata(
            http_backend,
            metadata_backend,
            fetcher.clone(),
            START_MS,
        );
        if mock {
            server.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        }
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options.borrow_mut().set_js_outline_min_bytes(0);
        options
            .borrow_mut()
            .soft_enable_filter_for_testing(Filter::OutlineJavascript);
        Fixture {
            server: Rc::new(server),
            options,
            fetcher,
            debug_message: RefCell::new(String::new()),
        }
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ))
    }

    /// Enables Debug and stages the expected message.
    fn debug_with_message(&self, message: &str) {
        self.options.borrow_mut().enable_filter(Filter::Debug);
        *self.debug_message.borrow_mut() = message.to_string();
    }

    fn parse(&self, url: &str, html: &str) -> String {
        let driver = self.driver_for(url);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(JsOutlineFilter::new(driver)) as Box<dyn HtmlFilter>);
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    /// Serves a URL: body plus headers on success.
    fn serve_url(&self, url: &str) -> Option<(Vec<u8>, ResponseHeaders)> {
        let driver = self.driver_for(url);
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

    /// Outlines (or not) one script block (`OutlineScript`).
    fn outline_script(&self, id: &str, expect_outline: bool) {
        use ps_cache::hash::Hasher;
        let script_text = "FOOBAR";
        let hash = self.server.hasher().hash(script_text.as_bytes());
        let outline_url = format!("{TEST_DOMAIN}_.pagespeed.jo.{hash}.js");
        let wrong_hash_outline_url = format!("{TEST_DOMAIN}_.pagespeed.jo.not{hash}.js");
        let html_input = format!(
            "<head>\n  <title>Example style outline</title>\n  <!-- Script starts here -->\n  <script type='text/javascript'>{script_text}</script>\n  <!-- Script ends here -->\n</head>"
        );
        let expected_output = if expect_outline {
            format!(
                "<head>\n  <title>Example style outline</title>\n  <!-- Script starts here -->\n  <script type='text/javascript' src=\"{outline_url}\"></script>\n  <!-- Script ends here -->\n</head>"
            )
        } else {
            format!(
                "<head>\n  <title>Example style outline</title>\n  <!-- Script starts here -->\n  <script type='text/javascript'>{script_text}</script>{}\n  <!-- Script ends here -->\n</head>",
                self.debug_message.borrow()
            )
        };
        let output = self.parse(&format!("{TEST_DOMAIN}{id}"), &html_input);
        assert!(output.contains(&expected_output), "id: {id}\n{output}");
        if expect_outline {
            let (actual_outline, headers) = self
                .serve_url(&outline_url)
                .unwrap_or_else(|| panic!("serve {outline_url}"));
            assert_eq!(actual_outline, script_text.as_bytes());
            assert_eq!(
                headers.headers().lookup1("Content-Type"),
                Some("application/javascript".to_string())
            );
            // Wrong hashes must not serve (outlined bytes may come
            // from private pages).
            assert!(self.serve_url(&wrong_hash_outline_url).is_none());
        }
    }
}

#[test]
fn outline_script() {
    let fixture = Fixture::new();
    fixture.outline_script("outline_scripts_no_hash", true);
}

#[test]
fn outline_script_md5() {
    let fixture = Fixture::with_md5();
    fixture.outline_script("outline_scripts_md5", true);
}

#[test]
fn outline_script_with_base() {
    let fixture = Fixture::new();
    // Outlines land at the document location, never under <base>.
    let input = "<base href='http://cdn.example.com/file.html'><script>42;</script>";
    let hash = {
        use ps_cache::hash::Hasher;
        fixture.server.hasher().hash(b"42;")
    };
    let expected = format!(
        "<base href='http://cdn.example.com/file.html'><script src=\"{TEST_DOMAIN}_.pagespeed.jo.{hash}.js\"></script>"
    );
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}test.html"), input),
        expected
    );
}

#[test]
fn no_outline_script() {
    let fixture = Fixture::new();
    let html_input = "<head>\n  <title>Example style outline</title>\n  <!-- Script starts here -->\n  <script type='text/javascript' src='http://othersite/script.js'></script>\n  <!-- Script ends here -->\n</head>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}no_outline_script"), html_input),
        html_input
    );
}

#[test]
fn url_not_too_long() {
    let fixture = Fixture::new();
    fixture.outline_script("url_not_too_long", true);
}

#[test]
fn js_preserve_url() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_js_preserve_urls(true);
    fixture.outline_script("js_preserve_url", false);
}

#[test]
fn js_preserve_url_off() {
    let fixture = Fixture::new();
    fixture.options.borrow_mut().set_js_preserve_urls(false);
    fixture.outline_script("js_preserve_url_off", true);
}

#[test]
fn url_too_long() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .set_option("MaxUrlSize", "0", ps_config::scope::Scope::Directory)
        .unwrap();
    fixture.debug_with_message("<!--Rewritten URL too long: http://test.com/_.pagespeed.jo.#.-->");
    fixture.outline_script("url_too_long", false);
}

#[test]
fn empty_script() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}empty_script"), "<script></script>"),
        "<script></script>"
    );
}

#[test]
fn outline_skips_integrity() {
    let fixture = Fixture::new();
    let html = "<script integrity=\"sha256-abc\">alert('foo');</script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}outline_integrity"), html),
        html
    );
}

#[test]
fn outline_strips_async_defer() {
    let fixture = Fixture::new();
    let hash = {
        use ps_cache::hash::Hasher;
        fixture.server.hasher().hash(b"alert('foo');")
    };
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}outline_strip_async"),
            "<script async defer>alert('foo');</script>"
        ),
        format!("<script src=\"{TEST_DOMAIN}_.pagespeed.jo.{hash}.js\"></script>")
    );
}

#[test]
fn outline_preserves_attributes() {
    let fixture = Fixture::new();
    let hash = {
        use ps_cache::hash::Hasher;
        fixture.server.hasher().hash(b"alert('foo');")
    };
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}outline_attrs"),
            "<script nonce=\"n\" crossorigin=\"anonymous\" referrerpolicy=\"no-referrer\" fetchpriority=\"high\">alert('foo');</script>"
        ),
        format!(
            "<script nonce=\"n\" crossorigin=\"anonymous\" referrerpolicy=\"no-referrer\" fetchpriority=\"high\" src=\"{TEST_DOMAIN}_.pagespeed.jo.{hash}.js\"></script>"
        )
    );
}

#[test]
fn outline_skips_module() {
    let fixture = Fixture::new();
    let html = "<script type=\"module\">import x from '/x.js';</script>";
    assert_eq!(
        fixture.parse(&format!("{TEST_DOMAIN}outline_module"), html),
        html
    );
}

#[test]
fn rewrite_domain() {
    let fixture = Fixture::new();
    fixture
        .options
        .borrow_mut()
        .add_rewrite_domain_mapping("http://cdn.com", TEST_DOMAIN);
    let expected_url = format!("http://cdn.com/_.pagespeed.jo.0.js");
    assert_eq!(
        fixture.parse(
            &format!("{TEST_DOMAIN}rewrite_domain"),
            "<script>alert('foo');</script>"
        ),
        format!("<script src=\"{expected_url}\"></script>")
    );
    let (content, _) = fixture.serve_url(&expected_url).expect("served");
    assert_eq!(content, b"alert('foo');");
}
