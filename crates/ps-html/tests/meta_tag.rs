//! Meta-tag propagation tests ported from `meta_tag_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;

use harness::SharedSink;
use ps_html::filters::{MetaTagFilter, SharedHeaders};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;
use ps_http::response::{
    HttpOptions, RequestProperties, ResponseHeaders, ValidatorOption, VaryOption,
};

/// Pessimistic proxy-cacheable check (the C++ zero-arg overload).
fn is_proxy_cacheable(headers: &ResponseHeaders) -> bool {
    headers.is_proxy_cacheable(
        RequestProperties::default(),
        VaryOption::RespectVaryOnResources,
        ValidatorOption::NoValidator,
    )
}

const CT: &str = "Content-Type";
const XHTML_MIME: &str = "application/xhtml+xml";
const NOW_MS: i64 = 1_700_000_000_000;

/// Fresh headers with `Content-Type: text/html` (the C++ fixture default).
fn headers_with_html() -> SharedHeaders {
    let headers: SharedHeaders = Rc::new(RefCell::new(ResponseHeaders::new(
        HttpOptions::default_for_tests(),
    )));
    headers.borrow_mut().headers_mut().replace(CT, "text/html");
    headers
}

/// Parses without altering bytes, returning output + shared headers.
fn run_meta(html: &[u8], headers: &SharedHeaders) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(MetaTagFilter::new(headers)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/meta.html", false);
    parser.write(html);
    parser.finish();
    sink.bytes()
}

fn content_type(headers: &SharedHeaders) -> Option<Vec<String>> {
    headers.borrow().headers().lookup(CT)
}

const META_TAG_DOC: &[u8] = b"<html><head>\
    <meta http-equiv=\"Content-Type\" content=\"text/html;  charset=UTF-8\">\
    <META HTTP-EQUIV=\"CONTENT-LANGUAGE\" CONTENT=\"en-US,fr\">\
    </head><body></body></html>";

#[test]
fn filter_name() {
    use ps_html::filter::HtmlFilter;
    use ps_html::filters::MetaTagFilter;
    let headers = headers_with_html();
    assert_eq!(
        <MetaTagFilter as HtmlFilter>::name(&MetaTagFilter::new(&headers)),
        "ConvertMetaTags"
    );
}

#[test]
fn converts_tags() {
    let headers = headers_with_html();
    let output = run_meta(META_TAG_DOC, &headers);
    assert_eq!(output, META_TAG_DOC);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

#[test]
fn rejects_invalid_attribute() {
    // CR/LF inside the value fails the control-byte check: no header added.
    let headers = headers_with_html();
    headers.borrow_mut().headers_mut().remove_all(CT);
    let doc = b"<html><head>\
        <meta http-equiv=\"Content-Type\" content=\"text/html;      charset=U\r\nTF-8\">\
        </head><body></body></html>";
    let output = run_meta(doc, &headers);
    assert_eq!(output, doc);
    assert!(content_type(&headers).is_none());
}

const META_TAG_DOUBLE_DOC: &[u8] = b"<html><head>\
    <meta http-equiv=\"Content-Type\" content=\"text/html;  charset=UTF-8\">\
    <meta http-equiv=\"Content-Type\" content=\"text/html;  charset=UTF-8\">\
    <META HTTP-EQUIV=\"CONTENT-LANGUAGE\" CONTENT=\"en-US,FR\">\
    <META HTTP-EQUIV=\"CONTENT-LANGUAGE\" CONTENT=\"en-US,fr\">\
    </head><body></body></html>";

#[test]
fn converts_tags_once() {
    let headers = headers_with_html();
    let output = run_meta(META_TAG_DOUBLE_DOC, &headers);
    assert_eq!(output, META_TAG_DOUBLE_DOC);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

#[test]
fn equiv_without_value_is_safe() {
    let headers = headers_with_html();
    let doc = b"<meta http-equiv='NoValue'>";
    let output = run_meta(doc, &headers);
    assert_eq!(output, doc);
    assert_eq!(
        content_type(&headers).unwrap(),
        vec!["text/html".to_string()]
    );
}

const META_TAG_CONFLICT_DOC: &[u8] = b"<html><head>\
    <meta http-equiv=\"Content-Type\" content=\"text/html;  charset=UTF-8\">\
    <meta http-equiv=\"Content-Type\" content=\"text/xml;  charset=UTF-16\">\
    <meta http-equiv=\"Content-Type\" content=\"text/xml\">\
    </head><body></body></html>";

#[test]
fn first_tag_wins_conflicts() {
    let headers = headers_with_html();
    let output = run_meta(META_TAG_CONFLICT_DOC, &headers);
    assert_eq!(output, META_TAG_CONFLICT_DOC);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

const META_TAG_CHARSET_DOC: &[u8] = b"<html><head>\
    <meta http-equiv=\"Content-Type\" content=\"text/html\">\
    <meta charset=\"UTF-8\">\
    <meta http-equiv=\"Content-Type\" content=\"text/xml; charset=UTF-16\">\
    </head><body></body></html>";

#[test]
fn converts_charset() {
    let headers = headers_with_html();
    let output = run_meta(META_TAG_CHARSET_DOC, &headers);
    assert_eq!(output, META_TAG_CHARSET_DOC);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

const META_TAG_CHARSET_ONLY_DOC: &[u8] =
    b"<html><head><meta charset=\"UTF-8\"></head><body></body></html>";

#[test]
fn converts_charset_only() {
    let headers = headers_with_html();
    let output = run_meta(META_TAG_CHARSET_ONLY_DOC, &headers);
    assert_eq!(output, META_TAG_CHARSET_ONLY_DOC);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

#[test]
fn charset_without_upstream_dropped() {
    let headers = headers_with_html();
    headers.borrow_mut().headers_mut().remove_all(CT);
    let output = run_meta(META_TAG_CHARSET_ONLY_DOC, &headers);
    assert_eq!(output, META_TAG_CHARSET_ONLY_DOC);
    assert!(content_type(&headers).is_none());
}

#[test]
fn caching_fields_recomputed_after_charset() {
    let headers = headers_with_html();
    {
        let mut borrowed = headers.borrow_mut();
        borrowed.set_status_and_reason(200);
        borrowed.set_date(NOW_MS);
        borrowed.headers_mut().add("Cache-Control", "max-age=300");
        borrowed.compute_caching();
        assert!(!borrowed.cache_fields_dirty());
        assert!(borrowed.is_browser_cacheable());
    }
    let ttl = headers.borrow().cache_ttl_ms();
    let output = run_meta(META_TAG_CHARSET_ONLY_DOC, &headers);
    assert_eq!(output, META_TAG_CHARSET_ONLY_DOC);
    let borrowed = headers.borrow();
    assert_eq!(
        borrowed.headers().lookup1(CT).as_deref(),
        Some("text/html; charset=UTF-8")
    );
    assert!(!borrowed.cache_fields_dirty());
    assert!(borrowed.is_browser_cacheable());
    assert_eq!(borrowed.cache_ttl_ms(), ttl);
}

#[test]
fn caching_fields_reflect_new_content_type() {
    let headers = headers_with_html();
    {
        let mut borrowed = headers.borrow_mut();
        borrowed.headers_mut().remove_all(CT);
        borrowed.set_status_and_reason(200);
        borrowed.set_date(NOW_MS);
        borrowed.headers_mut().add("Cache-Control", "no-cache");
        borrowed.compute_caching();
        borrowed.force_caching(300 * 1000);
        assert!(!borrowed.cache_fields_dirty());
        assert!(borrowed.is_browser_cacheable());
        assert!(is_proxy_cacheable(&borrowed));
    }
    let output = run_meta(META_TAG_DOC, &headers);
    assert_eq!(output, META_TAG_DOC);
    let borrowed = headers.borrow();
    // Adopted verbatim (double space intact).
    assert_eq!(
        borrowed.headers().lookup1(CT).as_deref(),
        Some("text/html;  charset=UTF-8")
    );
    assert!(!borrowed.cache_fields_dirty());
    assert!(!borrowed.is_browser_cacheable());
    assert!(!is_proxy_cacheable(&borrowed));
}

const META_TAG_DO_NOTHING_DOC: &[u8] = b"<html><head>\
    <meta http-equiv=\"\" content=\"\">\
    <meta http-equiv=\"  \" content=\"\">\
    <meta http-equiv=\"  :\" content=\"\">\
    <meta http-equiv=\"Content-Length\" content=\"123\">\
    </head><body></body></html>";

#[test]
fn leaves_non_content_metas_alone() {
    let headers = headers_with_html();
    let output = run_meta(META_TAG_DO_NOTHING_DOC, &headers);
    assert_eq!(output, META_TAG_DO_NOTHING_DOC);
    let borrowed = headers.borrow();
    assert_eq!(borrowed.headers().len(), 1);
    assert_eq!(borrowed.headers().lookup1(CT).as_deref(), Some("text/html"));
}

const META_TAG_NOSCRIPT_DOC: &[u8] = b"<html><head><noscript>\
    <meta http-equiv=\"Content-Type\" content=\"text/html; charset=UTF-8\">\
    </noscript></head><body></body></html>";

#[test]
fn ignores_noscript_metas() {
    // The C++ test reuses the do-nothing doc (covering nothing noscript);
    // run both: the faithful case plus the doc that exercises the path.
    for doc in [META_TAG_DO_NOTHING_DOC, META_TAG_NOSCRIPT_DOC] {
        let headers = headers_with_html();
        let output = run_meta(doc, &headers);
        assert_eq!(output, doc);
        let borrowed = headers.borrow();
        assert_eq!(borrowed.headers().len(), 1);
        assert_eq!(borrowed.headers().lookup1(CT).as_deref(), Some("text/html"));
    }
}

#[test]
fn unquoted_content_with_charset_attr() {
    // `content=text/html;` ends at the space; the sibling charset
    // attribute completes it.
    let headers = headers_with_html();
    let doc = b"<html><head>\
        <meta http-equiv=Content-Type content=text/html; charset=UTF-8>\
        </head><body></body></html>";
    let output = run_meta(doc, &headers);
    assert_eq!(output, doc);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html; charset=utf-8");
}

#[test]
fn no_override_with_bogus_xhtml_unknown() {
    let headers = headers_with_html();
    headers.borrow_mut().headers_mut().remove_all(CT);
    let doc = b"<meta http-equiv=\"Content-Type\" content=\"text/xhtml; charset=UTF-8\">";
    let output = run_meta(doc, &headers);
    assert_eq!(output, doc);
    assert!(content_type(&headers).is_none());
}

#[test]
fn no_override_with_real_xhtml_unknown() {
    let headers = headers_with_html();
    headers.borrow_mut().headers_mut().remove_all(CT);
    let doc = format!("<meta http-equiv=\"Content-Type\" content=\"{XHTML_MIME} ;charset=UTF-8\">");
    let output = run_meta(doc.as_bytes(), &headers);
    assert_eq!(output, doc.as_bytes());
    assert!(content_type(&headers).is_none());
}

#[test]
fn no_override_with_bogus_xhtml_known() {
    let headers = headers_with_html();
    let doc = b"<meta http-equiv=\"Content-Type\" content=\"text/xhtml; charset=UTF-8\">";
    let output = run_meta(doc, &headers);
    assert_eq!(output, doc);
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html");
}

#[test]
fn no_override_with_real_xhtml_known() {
    let headers = headers_with_html();
    let doc = format!("<meta http-equiv=\"Content-Type\" content=\"{XHTML_MIME};charset=UTF-8\">");
    let output = run_meta(doc.as_bytes(), &headers);
    assert_eq!(output, doc.as_bytes());
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].to_ascii_lowercase(), "text/html");
}

#[test]
fn no_override_charset_when_both_xhtml() {
    let headers = headers_with_html();
    let initial = format!("{XHTML_MIME}; charset=UTF-8");
    headers.borrow_mut().headers_mut().replace(CT, &initial);
    let doc =
        format!("<meta http-equiv=\"Content-Type\" content=\"{XHTML_MIME}; charset=KOI8-R\">");
    let output = run_meta(doc.as_bytes(), &headers);
    assert_eq!(output, doc.as_bytes());
    let values = content_type(&headers).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], initial);
}
