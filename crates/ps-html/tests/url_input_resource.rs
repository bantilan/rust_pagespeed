//! URL resource tests ported from `url_input_resource_test.cc`.
//!
//! NOTE: per-case drivers stand in for `SetBaseUrlForFetch`. The
//! mock records (rather than fails) `PageSpeed: off` mismatches, so
//! each fetch asserts the recording. The concurrency test runs its
//! two first-reads back-to-back: threads are mute under sync
//! completion, but single inflation with describing headers is still
//! pinned.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_config::options::RewriteOptions;
use ps_html::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
use ps_html::resource::Resource;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_html::url_input_resource::{make_url_input_resource, UrlInputResource};
use ps_http::content_type::mime_to_content_type;
use ps_http::google_url::{GoogleUrl, PORT_UNSPECIFIED};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::RequestContext;
use ps_http::response::{HttpOptions, ResponseHeaders};

const TEST_DOMAIN: &str = "http://test.com/";
/// Origin referer the module sends on its own fetches.
const TEST_ORIGIN: &str = "http://test.com/";
const START_MS: i64 = 1_700_000_000_000;

struct Fixture {
    server: Rc<ServerContext>,
    fetcher: Arc<MockUrlFetcher>,
    options: Rc<RefCell<RewriteOptions>>,
}

impl Fixture {
    fn new() -> Self {
        let backend: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let fetcher = Arc::new(MockUrlFetcher::new());
        let server = Rc::new(ServerContext::new(backend, fetcher.clone(), START_MS));
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        Fixture {
            server,
            fetcher,
            options,
        }
    }

    /// Driver with a case base URL (`SetBaseUrlForFetch`).
    fn driver_for_base(&self, base_url: &str) -> Rc<ResourceDriver> {
        Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            base_url,
        ))
    }

    /// Scripts a cacheable JPEG fetch, verifying the opt-out header.
    fn prepare_resource_fetch(&self, url: &str) {
        self.fetcher.set_verify_pagespeed_header_off(true);
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add("Content-Type", "image/jpeg");
        headers.set_date_and_caching(self.server.now_ms(), 100 * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(url, headers, b"payload");
    }

    /// Scripts a gzipped fetch.
    fn prepare_gzipped_fetch(&self, url: &str, payload: &[u8]) {
        self.fetcher.set_verify_pagespeed_header_off(true);
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add("Content-Type", "image/jpeg");
        headers.headers_mut().add("Content-Encoding", "gzip");
        headers.set_date_and_caching(self.server.now_ms(), 100 * 1000, "");
        headers.compute_caching();
        self.fetcher.set_response(url, headers, payload);
    }

    #[allow(clippy::too_many_arguments)]
    fn check_fetch_has_referer(
        &self,
        url: &str,
        fetch_url: &str,
        base_url: &str,
        is_background: bool,
        authorized: bool,
        auth_expected: bool,
        expected_key: &str,
        expected_referer: &str,
    ) {
        self.prepare_resource_fetch(fetch_url);
        let driver = self.driver_for_base(base_url);
        let jpeg = mime_to_content_type("image/jpeg");
        let mut resource = make_url_input_resource(driver, jpeg, url, authorized);
        assert_eq!(resource.url(), url);
        assert_eq!(resource.cache_key(), expected_key);
        resource.set_is_background_fetch(is_background);
        let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
        let mut callback = MockLoadCallback::new();
        resource.load(
            NotCacheablePolicy::LoadEvenIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        assert!(callback.done());
        assert!(callback.success());
        assert_eq!(
            self.fetcher.last_referer().as_deref(),
            Some(expected_referer).filter(|s| !s.is_empty())
        );
        assert_eq!(self.fetcher.last_had_pagespeed_off(), Some(true));
        if !authorized {
            let origin = GoogleUrl::new(fetch_url).origin().to_string();
            assert_eq!(
                auth_expected,
                ctx.is_session_authorized_fetch_origin(&origin)
            );
        }
    }
}

#[test]
fn background_fetch_referer_same_domain() {
    let fixture = Fixture::new();
    let url = format!("{TEST_DOMAIN}1.jpg");
    fixture.check_fetch_has_referer(&url, &url, TEST_DOMAIN, true, true, true, &url, TEST_ORIGIN);
}

#[test]
fn extract_gzipped_resource() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.txt";
    #[rustfmt::skip]
    let payload: &[u8] = b"\x1f\x8b\x08\x08\x64\x7d\x33\x56\x00\x03\x68\x65\x6c\x6c\x6f\x00\xcb\x48\xcd\xc9\xc9\x07\x00\x86\xa6\x10\x36\x05\x00\x00\x00\x00";
    fixture.prepare_gzipped_fetch(url, payload);
    let driver = fixture.driver_for_base(TEST_DOMAIN);
    let jpeg = mime_to_content_type("image/jpeg");
    let mut resource = make_url_input_resource(driver, jpeg, url, false);
    assert_eq!(resource.url(), url);
    assert_eq!(resource.cache_key(), "unauth://other.com/1.txt");
    resource.set_is_background_fetch(true);
    let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
    let mut callback = MockLoadCallback::new();
    resource.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.done());
    assert!(callback.success());
    assert_eq!(fixture.fetcher.last_referer().as_deref(), Some(TEST_ORIGIN));
    assert_eq!(resource.contents(), b"hello");
}

#[test]
fn oversized_gzipped_resource_not_inflated() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.txt";
    // One byte past the cacheable cap: inflating must refuse.
    let mut huge = vec![b'a'; 16_777_216 + 1];
    let mut scratch = ResponseHeaders::new(HttpOptions::default_for_tests());
    scratch.set_first_line(1, 1, 200, "OK");
    assert!(ps_http::fetch::gzip_value(&mut scratch, &mut huge, 6));
    assert!(huge.len() < (16_777_216 + 1) / 100);
    fixture.prepare_gzipped_fetch(url, &huge);
    let driver = fixture.driver_for_base(TEST_DOMAIN);
    let jpeg = mime_to_content_type("image/jpeg");
    let mut resource = make_url_input_resource(driver, jpeg, url, false);
    resource.set_is_background_fetch(true);
    let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
    let mut callback = MockLoadCallback::new();
    resource.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.done());
    assert!(callback.success());
    // The compressed bytes are kept, as for any broken input.
    assert_eq!(resource.contents().len(), huge.len());
}

#[test]
fn first_reads_inflate_once_with_describing_headers() {
    // Sequential port of the concurrency test: both first-reads see
    // the full inflation with matching headers.
    let fixture = Fixture::new();
    let mut contents = String::new();
    let mut i = 0;
    while contents.len() < 12 << 20 {
        contents.push_str(&format!("var v{i} = {};\n", i * 31));
        i += 1;
    }
    let mut compressed = contents.clone().into_bytes();
    let mut scratch = ResponseHeaders::new(HttpOptions::default_for_tests());
    scratch.set_first_line(1, 1, 200, "OK");
    assert!(ps_http::fetch::gzip_value(&mut scratch, &mut compressed, 6));
    let url = "http://other.com/1.txt";
    fixture.prepare_gzipped_fetch(url, &compressed);
    let driver = fixture.driver_for_base(TEST_DOMAIN);
    let jpeg = mime_to_content_type("image/jpeg");
    let mut resource = make_url_input_resource(driver, jpeg, url, false);
    resource.set_is_background_fetch(true);
    let mut ctx = RequestContext::new(HttpOptions::default_for_tests());
    let mut callback = MockLoadCallback::new();
    resource.load(
        NotCacheablePolicy::LoadEvenIfNotCacheable,
        &mut ctx,
        &mut callback,
    );
    assert!(callback.success());
    // Nothing has read the contents yet, so inflation is still ahead.
    let first = resource.contents();
    let second = resource.contents();
    assert_eq!(contents.len(), first.len());
    assert_eq!(contents.len(), second.len());
    assert_eq!(contents.as_bytes(), first.as_slice());
    assert_eq!(contents.as_bytes(), second.as_slice());
    let headers = resource.uncompressed_headers();
    assert_eq!(Some(contents.len() as i64), headers.find_content_length());
    assert_eq!(
        Some(contents.len() as i64),
        resource.uncompressed_headers().find_content_length()
    );
    assert!(!resource
        .uncompressed_headers()
        .headers()
        .has("Content-Encoding"));
}

#[test]
fn background_fetch_referer_domain() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(url, url, TEST_DOMAIN, true, true, true, url, TEST_ORIGIN);
}

#[test]
fn non_background_fetch_with_referer_missing() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(url, url, TEST_DOMAIN, false, true, true, url, TEST_ORIGIN);
}

#[test]
fn own_fetch_does_not_forward_visitor_referer() {
    // The visitor's referer never forwards (fresh request headers);
    // the page origin goes instead.
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(url, url, TEST_DOMAIN, false, true, true, url, TEST_ORIGIN);
}

#[test]
fn own_background_fetch_sends_origin_only_referer() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        "http://test.com/dir/page.html?x=1",
        true,
        true,
        true,
        url,
        TEST_ORIGIN,
    );
}

#[test]
fn own_fetch_referer_omits_userinfo_of_base_url() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        "http://user:secret@test.com:8080/p?q",
        true,
        true,
        true,
        url,
        "http://test.com:8080/",
    );
}

#[test]
fn own_background_fetch_without_valid_base_sends_no_referer() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(url, url, "", true, true, true, url, "");
}

#[test]
fn own_fetch_without_valid_base_sends_no_referer() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(url, url, "", false, true, true, url, "");
}

#[test]
fn unauthorized_domain_http() {
    let fixture = Fixture::new();
    let url = "http://other.com/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        TEST_DOMAIN,
        true,
        false,
        true,
        "unauth://other.com/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
#[should_panic(expected = "no cache key")]
fn unauthorized_domain_wrong_protocol() {
    let fixture = Fixture::new();
    let driver = fixture.driver_for_base(TEST_DOMAIN);
    let jpeg = mime_to_content_type("image/jpeg");
    let _ = make_url_input_resource(driver, jpeg, "ftp://other.com/1.jpg", false);
}

#[test]
#[should_panic(expected = "no cache key")]
fn unauthorized_domain_relative_url() {
    let fixture = Fixture::new();
    let driver = fixture.driver_for_base(TEST_DOMAIN);
    let jpeg = mime_to_content_type("image/jpeg");
    let _ = make_url_input_resource(driver, jpeg, "/1.jpg", false);
}

#[test]
fn unauthorized_domain_http_with_correct_port() {
    let fixture = Fixture::new();
    fixture.check_fetch_has_referer(
        "http://other.com:80/1.jpg",
        "http://other.com/1.jpg",
        TEST_DOMAIN,
        true,
        false,
        true,
        "unauth://other.com/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn no_unauthorized_domain_http_with_wrong_port() {
    let fixture = Fixture::new();
    let url = "http://other.com:1234/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        TEST_DOMAIN,
        true,
        false,
        false,
        "unauth://other.com:1234/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn no_unauthorized_domain_http_with_https_port() {
    let fixture = Fixture::new();
    let url = "http://other.com:443/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        TEST_DOMAIN,
        true,
        false,
        false,
        "unauth://other.com:443/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn unauthorized_domain_https() {
    let fixture = Fixture::new();
    let url = "https://other.com/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        TEST_DOMAIN,
        true,
        false,
        true,
        "unauths://other.com/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn unauthorized_domain_https_with_correct_port() {
    let fixture = Fixture::new();
    fixture.check_fetch_has_referer(
        "https://other.com:443/1.jpg",
        "https://other.com/1.jpg",
        TEST_DOMAIN,
        true,
        false,
        true,
        "unauths://other.com/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn no_unauthorized_domain_https_with_wrong_port() {
    let fixture = Fixture::new();
    let url = "https://other.com:1234/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        TEST_DOMAIN,
        true,
        false,
        false,
        "unauths://other.com:1234/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn no_unauthorized_domain_https_with_http_port() {
    let fixture = Fixture::new();
    let url = "https://other.com:80/1.jpg";
    fixture.check_fetch_has_referer(
        url,
        url,
        TEST_DOMAIN,
        true,
        false,
        false,
        "unauths://other.com:80/1.jpg",
        TEST_ORIGIN,
    );
}

#[test]
fn int_port() {
    // Default ports read as unspecified, like GURL.
    for (spec, effective, int_port) in [
        ("http://www.google.com/", 80, PORT_UNSPECIFIED),
        ("http://www.google.com:80/", 80, PORT_UNSPECIFIED),
        ("http://www.google.com:443/", 443, 443),
        ("http://www.google.com:1234/", 1234, 1234),
        ("https://www.google.com/", 443, PORT_UNSPECIFIED),
        ("https://www.google.com:443/", 443, PORT_UNSPECIFIED),
        ("https://www.google.com:80/", 80, 80),
        ("https://www.google.com:1234/", 1234, 1234),
    ] {
        let url = GoogleUrl::new(spec);
        assert!(url.is_web_valid(), "{spec}");
        assert_eq!(effective, url.effective_int_port(), "{spec}");
        assert_eq!(int_port, url.int_port(), "{spec}");
    }
}
