//! Single-resource rewrite tests ported from
//! `rewrite_single_resource_filter_test.cc` (rewrite path; serving
//! and deferred-fetch cases land with those layers).
//!
//! NOTE: inputs skip the body wrapper (it carries no URLs).
//! `BasicAsync` needs deferred completion and the `Fetch*` cases
//! need the serving path — both are skipped with the machinery
//! that serves them. Each `custom_encoder` param runs as its own
//! test (the C++ `TEST_P` cartesianizes over it).
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use harness::SharedSink;
use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::element::NodeId;
use ps_html::filter::HtmlFilter;
use ps_html::lexer::Lexer;
use ps_html::metadata::CachedResult;
use ps_html::name::Keyword;
use ps_html::output_resource::OutputResource;
use ps_html::parser::{Dom, Parser};
use ps_html::resource::{InputResource, Resource};
use ps_html::rewrite_context::{RewriteResult, SingleRewriteBehavior, SingleRewriteContext};
use ps_html::rewrite_filter::RewriteFilter;
use ps_html::server_context::{
    InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver, ServerContext,
};
use ps_html::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};
use ps_html::writer::WriterFilter;
use ps_http::content_type::mime_to_content_type;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
const FILTER_ID: &str = "tf";
const TTL_SEC: i64 = 300;
const START_MS: i64 = 1_700_000_000_000;

/// Custom-encoder marker (`kTestEncoderUrlExtra`).
const URL_EXTRA: &[u8] = b"UrlExtraStuff";

/// Test encoder: default codec with a marker prefix.
#[derive(Debug, Clone, Copy, Default)]
struct TestUrlEncoder;

impl UrlSegmentEncoder for TestUrlEncoder {
    fn encode(&self, urls: &[Vec<u8>]) -> Vec<u8> {
        let mut out = URL_EXTRA.to_vec();
        out.extend_from_slice(&DefaultEncoder.encode(urls));
        out
    }

    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>> {
        DefaultEncoder.decode(segment.strip_prefix(URL_EXTRA)?)
    }
}

static TEST_ENCODER: TestUrlEncoder = TestUrlEncoder;
static DEFAULT_ENCODER: DefaultEncoder = DefaultEncoder;

/// Doubler behavior (`TestRewriter::Context` + rewrite counting).
struct TestBehavior {
    rewrites: Rc<RefCell<u32>>,
    custom_encoder: bool,
}

impl SingleRewriteBehavior for TestBehavior {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        if self.custom_encoder {
            &TEST_ENCODER
        } else {
            &DEFAULT_ENCODER
        }
    }

    fn policy_permits_rendering(
        &self,
        _driver: &Rc<ResourceDriver>,
        _outputs: &[Option<ps_html::output_resource::OutputResource>],
    ) -> bool {
        true
    }

    fn rewrite_single(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut ps_html::output_resource::OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        *self.rewrites.borrow_mut() += 1;
        assert!(input.http_status_ok());
        let contents = input.contents();
        if contents == b"bad" {
            return RewriteResult::Failed;
        }
        if contents == b"$" {
            return RewriteResult::TooBusy;
        }
        let doubled = [contents.as_slice(), contents.as_slice()].concat();
        let inputs = [input.clone()];
        driver.write_output(
            &inputs,
            &doubled,
            mime_to_content_type("text/plain"),
            "",
            output,
            partition,
        );
        RewriteResult::Ok
    }
}

/// Serving half of the test filter: reconstructs decoded outputs
/// through a fresh context (C++ uses one filter object for both;
/// the port splits HTML and serving traits).
struct TestServeFilter {
    rewrites: Rc<RefCell<u32>>,
    custom_encoder: bool,
}

impl RewriteFilter for TestServeFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        if self.custom_encoder {
            &TEST_ENCODER
        } else {
            &DEFAULT_ENCODER
        }
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: OutputResource,
        fetch: &mut dyn AsyncFetch,
    ) -> bool {
        let behavior = TestBehavior {
            rewrites: self.rewrites.clone(),
            custom_encoder: self.custom_encoder,
        };
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.fetch(output, fetch)
    }
}

/// Test filter (`TestRewriter`): rewrites `<tag src>` inline.
struct TestRewriter {
    driver: Rc<ResourceDriver>,
    rewrites: Rc<RefCell<u32>>,
    custom_encoder: bool,
    doc_url: GoogleUrl,
}

impl HtmlFilter for TestRewriter {
    fn name(&self) -> &'static str {
        "TestRewriter"
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Tag {
            return;
        }
        let Some(index) = dom.find_attribute(id, Keyword::Src) else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        let Some(src) = decoded else {
            return;
        };
        let url = GoogleUrl::resolve(&self.doc_url, &String::from_utf8_lossy(&src));
        if !url.is_web_valid() {
            return;
        }
        let (resource, _authorized) = self.driver.create_input_resource(
            &url,
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            IntendedFor::General,
            InputRole::Unknown,
        );
        let Some(input) = resource else {
            return;
        };
        let handle: Rc<dyn Resource> = Rc::new(input.clone());
        let slot = self.driver.get_slot(Some(handle), dom, id, index, "test");
        let behavior = TestBehavior {
            rewrites: self.rewrites.clone(),
            custom_encoder: self.custom_encoder,
        };
        let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
        context.add_slot(slot, input);
        context.start(dom);
    }
}

struct Fixture {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    rewrites: Rc<RefCell<u32>>,
    custom_encoder: bool,
    fetcher: Rc<MockUrlFetcher>,
}

impl Fixture {
    fn new(custom_encoder: bool) -> Self {
        let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let fetcher = Rc::new(MockUrlFetcher::new());
        let mut server = ServerContext::new(backend.clone(), fetcher.clone(), START_MS);
        // C++ tests hash through a constant `MockHasher` ("0").
        server.set_hasher(Rc::new(ps_cache::hash::MockHasher::new()));
        let server = Rc::new(server);
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        Fixture {
            server,
            options,
            rewrites: Rc::new(RefCell::new(0)),
            custom_encoder,
            fetcher,
        }
    }

    fn fetcher(&self) -> Rc<MockUrlFetcher> {
        self.fetcher.clone()
    }

    fn driver_for(&self, url: &str) -> Rc<ResourceDriver> {
        Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            url,
        ))
    }

    /// Scripts a cacheable text fetch (`MockResource`).
    fn mock_resource(&self, name: &str, content: &str) {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add("Content-Type", "text/plain");
        headers.set_date_and_caching(self.server.now_ms(), TTL_SEC * 1000, "");
        headers.compute_caching();
        self.fetcher()
            .set_response(&format!("{TEST_DOMAIN}{name}"), headers, content.as_bytes());
    }

    /// Default mock set (`SetUp`).
    fn mock_default_resources(&self) {
        self.mock_resource("a.tst", "good");
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add("Content-Type", "text/css");
        headers.set_date_and_caching(self.server.now_ms(), TTL_SEC * 1000, "");
        headers.compute_caching();
        self.fetcher()
            .set_response(&format!("{TEST_DOMAIN}bad.tst"), headers, b"bad");
        self.mock_resource("busy.tst", "$");
        let mut missing = ResponseHeaders::new(HttpOptions::default_for_tests());
        missing.set_first_line(1, 1, 404, "Not Found");
        missing.headers_mut().add("Content-Type", "text/plain");
        missing.set_date_and_caching(self.server.now_ms(), TTL_SEC * 1000, "");
        missing.compute_caching();
        self.fetcher()
            .set_response(&format!("{TEST_DOMAIN}404.tst"), missing, b"");
    }

    /// Expected output leaf (`OutputName`).
    fn output_name(&self, name: &str) -> String {
        let encoder: &dyn UrlSegmentEncoder = if self.custom_encoder {
            &TEST_ENCODER
        } else {
            &DEFAULT_ENCODER
        };
        let encoded = encoder.encode(&[name.as_bytes().to_vec()]);
        let hash = self.server.hasher().hash(b"");
        format!(
            "{}.pagespeed.{}.{}.txt",
            String::from_utf8_lossy(&encoded),
            FILTER_ID,
            hash
        )
    }

    /// Expected output tag (`OutputTag`).
    fn output_tag(&self, name: &str) -> String {
        format!("<tag src=\"{}\"/>", self.output_name(name))
    }

    /// Serves a relative URL (`ServeRelativeUrl`): the body on
    /// success, `None` on failure.
    fn serve_relative_url(&self, rel_path: &str) -> Option<Vec<u8>> {
        let url = format!("{TEST_DOMAIN}{rel_path}");
        let driver = self.driver_for(&url);
        driver.add_filter(Rc::new(TestServeFilter {
            rewrites: self.rewrites.clone(),
            custom_encoder: self.custom_encoder,
        }));
        let ctx = driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(&url, ctx);
        let handled = driver.fetch_resource(&url, &mut fetch);
        // The fetch completes if and only if the URL decoded.
        assert_eq!(handled, fetch.done());
        if handled && fetch.success() {
            Some(fetch.body().to_vec())
        } else {
            None
        }
    }

    /// Parses one document through the test filter.
    fn parse(&self, case: &str, html: &str) -> String {
        let url = format!("{TEST_DOMAIN}{case}.html");
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(TestRewriter {
            driver: self.driver_for(&url),
            rewrites: self.rewrites.clone(),
            custom_encoder: self.custom_encoder,
            doc_url: GoogleUrl::new(&url),
        }));
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(url.as_bytes(), false);
        parser.write(html.as_bytes());
        parser.finish();
        String::from_utf8(sink.bytes()).unwrap()
    }

    fn validate_expected(&self, case: &str, input: &str, expected: &str) {
        assert_eq!(self.parse(case, input), expected, "case {case}");
    }

    fn advance_ms(&self, delta_ms: i64) {
        self.server.timer().borrow_mut().advance_ms(delta_ms);
    }
}

macro_rules! single_resource_tests {
    ($suffix:ident, $custom:expr) => {
        mod $suffix {
            use super::*;

            fn fixture() -> Fixture {
                let fixture = Fixture::new($custom);
                fixture.mock_default_resources();
                fixture
            }

            #[test]
            fn basic_operation() {
                let fixture = fixture();
                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                fixture.validate_expected("no_query", in_tag, &out_tag);
                fixture.validate_expected("more_query", in_tag, &out_tag);
                fixture.validate_expected(
                    "multi_tag",
                    &format!("{in_tag}{in_tag}"),
                    &format!("{out_tag}{out_tag}"),
                );
                // One rewrite serves all three tags.
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn basic_async() {
                let fixture = fixture();
                fixture.fetcher().set_waiting(true);
                let in_tag = "<tag src=\"a.tst\"/>";
                // First fetch should not rewrite since resources
                // haven't loaded yet.
                fixture.validate_expected("async.not_yet", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 0);
                // Now let it load.
                fixture.fetcher().call_callbacks();
                // This time should rewrite.
                let out_tag = fixture.output_tag("a.tst");
                fixture.validate_expected("async.loaded", in_tag, &out_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn version_change() {
                let fixture = fixture();
                // `ResetSignature(kOrigOutlineMinBytes)` up front: the
                // restore later must reproduce this exact signature.
                fixture.options.borrow_mut().set_css_outline_min_bytes(1234);
                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                let input = format!("{in_tag}{in_tag}{in_tag}");
                let expected = format!("{out_tag}{out_tag}{out_tag}");
                fixture.validate_expected("vc1", &input, &expected);
                // Should only have to rewrite once here.
                assert_eq!(*fixture.rewrites.borrow(), 1);
                // The next attempt should still use cache.
                fixture.validate_expected("vc2", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 1);
                // Change the rewrite options -- this won't affect the
                // actual result but will result in an effective flush.
                fixture.options.borrow_mut().set_css_outline_min_bytes(1235);
                fixture.validate_expected("vc3", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 2);
                // And now we're caching again.
                fixture.validate_expected("vc4", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 2);
                // Restore. The old meta-data cache entries can be re-used.
                fixture.options.borrow_mut().set_css_outline_min_bytes(1234);
                fixture.validate_expected("vc5", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 2);
                // And now we're caching again.
                fixture.validate_expected("vc6", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 2);
            }

            #[test]
            fn version_change_bad() {
                let fixture = fixture();
                let in_tag = "<tag src=\"bad.tst\"/>";
                fixture.validate_expected("vc.bad", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
                // Cached with old version.
                fixture.validate_expected("vc.bad2", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
                // Upgraded -- retried.
                fixture.options.borrow_mut().set_css_outline_min_bytes(42);
                fixture.validate_expected("vc.bad3", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 2);
                // And now cached again.
                fixture.validate_expected("vc.bad4", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 2);
                // Downgrade -- retried.
                fixture.options.borrow_mut().set_css_outline_min_bytes(21);
                fixture.validate_expected("vc.bad5", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 3);
                // And now cached again.
                fixture.validate_expected("vc.bad6", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 3);
            }

            #[test]
            fn cache_bad() {
                let fixture = Fixture::new($custom);
                fixture.mock_resource("bad.tst", "bad");
                let in_tag = "<tag src=\"bad.tst\"/>";
                let input = format!("{in_tag}{in_tag}{in_tag}");
                fixture.validate_expected("cache.bad", &input, &input);
                // Should call rewrite once, and then remember it's not
                // optimizable.
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn cache_busy() {
                // NOTE: C++ parses three tags and still sees one rewrite:
                // in-flight contexts coalesce on the partition key
                // (`RegisterForPartitionKey`). The sync model completes
                // each context inline, so identical busy tags each retry
                // (the render is identical; only the count differs).
                let fixture = Fixture::new($custom);
                fixture.mock_resource("busy.tst", "$");
                let in_tag = "<tag src=\"busy.tst\"/>";
                fixture.validate_expected("cache.busy", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn cache_404() {
                let fixture = fixture();
                let in_tag = "<tag src=\"404.tst\"/>";
                let input = format!("{in_tag}{in_tag}{in_tag}");
                fixture.validate_expected("cache.404", &input, &input);
                // Should call rewrite zero times (as 404), and remember
                // it's not optimizable.
                assert_eq!(*fixture.rewrites.borrow(), 0);
            }

            #[test]
            fn invalid_url() {
                let fixture = fixture();
                // Make sure we don't have problems with bad URLs.
                let in_tag = "<tag src=\"http://evil.com\"/>";
                fixture.validate_expected("bad_url", in_tag, in_tag);
            }

            #[test]
            fn cache_expire() {
                let fixture = fixture();
                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                fixture.validate_expected("tc1", in_tag, &out_tag);
                fixture.advance_ms(TTL_SEC * 1000 / 2);
                fixture.validate_expected("tc2", in_tag, &out_tag);
                // Past the input TTL the entry revalidates by hash:
                // same bytes, no rewrite.
                fixture.advance_ms(TTL_SEC * 1000 * 2);
                fixture.validate_expected("tc3", in_tag, &out_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn fetch_bad_status() {
                let fixture = fixture();
                let mut redirect = ResponseHeaders::new(HttpOptions::default_for_tests());
                redirect.set_first_line(1, 1, 302, "Found");
                redirect.headers_mut().add("Content-Type", "text/plain");
                redirect.set_date_and_caching(fixture.server.now_ms(), TTL_SEC * 1000, "");
                redirect.compute_caching();
                fixture
                    .fetcher()
                    .set_response(&format!("{TEST_DOMAIN}redirect"), redirect, b"");
                let in_tag = "<tag src=\"/redirect\"/>";
                fixture.validate_expected("redirected_resource", in_tag, in_tag);

                let mut teapot = ResponseHeaders::new(HttpOptions::default_for_tests());
                teapot.set_first_line(1, 1, 418, "I'm a teapot");
                teapot.headers_mut().add("Content-Type", "text/plain");
                teapot.set_date_and_caching(fixture.server.now_ms(), TTL_SEC * 1000, "");
                teapot.compute_caching();
                fixture
                    .fetcher()
                    .set_response(&format!("{TEST_DOMAIN}pot-1"), teapot, b"");
                let in_tag = "<tag src=\"/pot-1\"/>";
                fixture.validate_expected("teapot_resource", in_tag, in_tag);
                // The second time, this resource will be cached with its
                // bad status code.
                fixture.validate_expected("teapot_resource", in_tag, in_tag);
            }

            #[test]
            fn fetch_good() {
                let fixture = fixture();
                let out = fixture
                    .serve_relative_url(&fixture.output_name("a.tst"))
                    .expect("served");
                assert_eq!(out, b"goodgood");
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn fetch_good_cache1() {
                let fixture = fixture();
                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                let input = format!("{in_tag}{in_tag}{in_tag}");
                let expected = format!("{out_tag}{out_tag}{out_tag}");
                fixture.validate_expected("compute_cached", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 1);

                let out = fixture
                    .serve_relative_url(&fixture.output_name("a.tst"))
                    .expect("served");
                assert_eq!(out, b"goodgood");
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn fetch_good_cache2() {
                let fixture = fixture();
                let out = fixture
                    .serve_relative_url(&fixture.output_name("a.tst"))
                    .expect("served");
                assert_eq!(out, b"goodgood");
                assert_eq!(*fixture.rewrites.borrow(), 1);

                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                let input = format!("{in_tag}{in_tag}{in_tag}");
                let expected = format!("{out_tag}{out_tag}{out_tag}");
                fixture.validate_expected("reused_cached", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn fetch_first_versioned() {
                let fixture = fixture();
                let out = fixture
                    .serve_relative_url(&fixture.output_name("a.tst"))
                    .expect("served");
                assert_eq!(out, b"goodgood");
                assert_eq!(*fixture.rewrites.borrow(), 1);

                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                let input = format!("{in_tag}{in_tag}{in_tag}");
                let expected = format!("{out_tag}{out_tag}{out_tag}");
                fixture.validate_expected("reused_cached", &input, &expected);
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }

            #[test]
            fn fetch_rewrite_failed() {
                let fixture = fixture();
                let out = fixture
                    .serve_relative_url(&fixture.output_name("bad.tst"))
                    .expect("fallback served");
                assert_eq!(out, b"bad");
                assert_eq!(*fixture.rewrites.borrow(), 1);
                assert_eq!(fixture.fetcher().fetch_count(), 1);

                // Make sure the above also cached the failure.
                let in_tag = "<tag src=\"bad.tst\"/>";
                fixture.validate_expected("postfetch.bad", in_tag, in_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
                assert_eq!(fixture.fetcher().fetch_count(), 1);
            }

            #[test]
            fn fetch_404() {
                let fixture = fixture();
                assert_eq!(
                    fixture.serve_relative_url(&fixture.output_name("404.tst")),
                    None
                );
            }

            #[test]
            fn fetch_invalid_resource_name() {
                let fixture = fixture();
                assert_eq!(
                    fixture.serve_relative_url("404,.tst.pagespeed.tf.0.txt"),
                    None
                );
            }

            #[test]
            fn cache_expire_with_reuse_enabled() {
                // NOTE: the C++ `reuse_by_content_hash_` knob is a dead
                // field (declared, never read); both expiry tests share
                // one shape and expect revalidation without a rewrite.
                let fixture = fixture();
                let in_tag = "<tag src=\"a.tst\"/>";
                let out_tag = fixture.output_tag("a.tst");
                fixture.validate_expected("initial", in_tag, &out_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
                // Everything expires out of the cache but has the same
                // content hash, so no more rewrites should be needed.
                fixture.advance_ms(TTL_SEC * 1000 * 2);
                fixture.validate_expected("expire", in_tag, &out_tag);
                assert_eq!(*fixture.rewrites.borrow(), 1);
            }
        }
    };
}

single_resource_tests!(plain, false);
single_resource_tests!(custom, true);
