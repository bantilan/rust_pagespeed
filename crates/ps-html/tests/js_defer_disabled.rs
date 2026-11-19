//! Defer-bootstrap tests ported from `js_defer_disabled_filter_test.cc`,
//! plus the deferred `DisablesScriptOnlyFromFirstSrc` chain case from
//! `js_disable_filter_test.cc`.
//!
//! NOTE: `JsDeferPreserveURLsOn` is not ported — it needs the full
//! `AddFilters` chain with the `DeferJavascript` pipeline filter.
#[path = "harness.rs"]
mod harness;

use harness::SharedSink;
use ps_html::filters::js_defer_disabled::JsDeferDisabledConfig;
use ps_html::filters::js_disable::JsDisableConfig;
use ps_html::filters::{JsDeferDisabledFilter, JsDisableFilter, SupportNoscriptFilter};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

const DEFER_JS_URL: &[u8] = b"/psajs/js_defer.0.js";

fn config_for(defer_applies: bool, asset_url: &[u8]) -> JsDeferDisabledConfig {
    JsDeferDisabledConfig {
        defer_applies,
        asset_url: asset_url.to_vec(),
    }
}

fn run_case(url: &[u8], input: &[u8], config: JsDeferDisabledConfig) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(JsDeferDisabledFilter::new(config)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url, false);
    parser.write(input);
    parser.finish();
    sink.bytes()
}

fn validate_expected(case: &str, input: &[u8], expected: &[u8], config: JsDeferDisabledConfig) {
    let url = format!("http://test.com/{case}.html");
    assert_eq!(
        run_case(url.as_bytes(), input, config),
        expected,
        "case {case}"
    );
}

#[test]
fn defer_script() {
    validate_expected(
        "defer_script",
        b"<html><head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func();</script></head><body>Hello, world!</body></html>",
        b"<html><head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func();</script></head><body>Hello, world!<script type=\"text/javascript\" src=\"/psajs/js_defer.0.js\"></script></body></html>",
        config_for(true, DEFER_JS_URL),
    );
}

#[test]
fn defer_script_multi_body() {
    validate_expected(
        "defer_script_multi_body",
        b"<html><head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func(); </script></head><body>Hello, world!</body><body><script type='text/psajs'> func2(); </script></body></html>",
        b"<html><head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func(); </script></head><body>Hello, world!</body><body><script type='text/psajs'> func2(); </script><script type=\"text/javascript\" src=\"/psajs/js_defer.0.js\"></script></body></html>",
        config_for(true, DEFER_JS_URL),
    );
}

#[test]
fn defer_script_optimized() {
    let output = run_case(
        b"http://test.com/optimized.html",
        b"<body><script type='text/psajs' src='foo.js'></script></body>",
        config_for(true, DEFER_JS_URL),
    );
    assert!(
        output
            .windows(b"js_defer.0.js".len())
            .any(|window| window == b"js_defer.0.js"),
        "js_defer.js should have been included: {}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn defer_script_debug() {
    let output = run_case(
        b"http://test.com/optimized.html",
        b"<head></head><body><script type='text/psajs' src='foo.js'></script></body>",
        config_for(true, b"/psajs/js_defer_debug.0.js"),
    );
    assert!(
        output
            .windows(b"js_defer_debug.0.js".len())
            .any(|window| window == b"js_defer_debug.0.js"),
        "js_defer_debug.js should have been included: {}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn invalid_user_agent() {
    let input = b"<head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func();</script></head><body>Hello, world!</body>";
    validate_expected(
        "defer_script",
        input,
        input,
        config_for(false, DEFER_JS_URL),
    );
}

#[test]
fn allow_mobile_user_agent() {
    validate_expected(
        "defer_script",
        b"<head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func();</script></head><body>Hello, world!</body>",
        b"<head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func();</script></head><body>Hello, world!<script type=\"text/javascript\" src=\"/psajs/js_defer.0.js\"></script></body>",
        config_for(true, DEFER_JS_URL),
    );
}

#[test]
fn disallow_mobile_user_agent() {
    let input = b"<head><script type='text/psajs' src='http://www.google.com/javascript/ajax_apis.js'></script><script type='text/psajs'> func();</script></head><body>Hello, world!</body>";
    validate_expected(
        "defer_script",
        input,
        input,
        config_for(false, DEFER_JS_URL),
    );
}

#[test]
fn defer_js_url_from_gstatic() {
    validate_expected(
        "defer_script_url",
        b"<html><body>Hello, world!</body><body><script type='text/psajs'> func2(); </script></body></html>",
        b"<html><body>Hello, world!</body><body><script type='text/psajs'> func2(); </script><script type=\"text/javascript\" src=\"//www.gstatic.com/psa/static/1-js_defer.js\"></script></body></html>",
        config_for(true, b"//www.gstatic.com/psa/static/1-js_defer.js"),
    );
}

#[test]
fn defer_js_url_from_non_gstatic() {
    validate_expected(
        "defer_script_url",
        b"<html><body>Hello, world!</body><body><script type='text/psajs'> func2(); </script></body></html>",
        b"<html><body>Hello, world!</body><body><script type='text/psajs'> func2(); </script><script type=\"text/javascript\" src=\"/psajs/js_defer.0.js\"></script></body></html>",
        config_for(true, DEFER_JS_URL),
    );
}

#[test]
fn disable_chain() {
    // Ports `DisablesScriptOnlyFromFirstSrc`: JsDisable retags, the
    // noscript fallback prepends, and the defer bootstrap appends last.
    // The document URL reproduces the C++ case-id dummy URL verbatim.
    let url = b"http://test.com/http://example.com/.html";
    let noscript_url = b"http://test.com/http://example.com/.html?PageSpeed=noscript";
    let input = "<body><noscript>This is original noscript tag</noscript>\
        <script random=\"true\">hi1</script>\
        <div id=\\\"contentContainer\\\"><h1>Hello 1</h1><div id=\\\"middleFooter\\\"><h3>Hello 3</h3></div></div>\
        <script random=\"false\">hi2</script>\
        <script src=\"1.js?a#12296;=en\"></script></body>";
    let mut expected = b"<body><noscript><meta HTTP-EQUIV=\"refresh\" content=\"0;url='".to_vec();
    expected.extend_from_slice(noscript_url);
    expected.extend_from_slice(b"'\" /><style><!--table,div,span,font,p{display:none} --></style><div style=\"display:block\">Please click <a href=\"");
    expected.extend_from_slice(noscript_url);
    expected.extend_from_slice(
        b"\">here</a> if you are not redirected within a few seconds.</div></noscript>",
    );
    expected.extend_from_slice(b"<noscript>This is original noscript tag</noscript>\
        <script random=\"true\" type=\"text/psajs\" data-pagespeed-orig-index=\"0\">hi1</script>\
        <div id=\\\"contentContainer\\\"><h1>Hello 1</h1><div id=\\\"middleFooter\\\"><h3>Hello 3</h3></div></div>\
        <script random=\"false\" type=\"text/psajs\" data-pagespeed-orig-index=\"1\">hi2</script>\
        <script src=\"1.js?a#12296;=en\" type=\"text/psajs\" data-pagespeed-orig-index=\"2\"></script>\
        <script type=\"text/javascript\" data-pagespeed-no-defer>window.pagespeed = window.pagespeed || {};window.pagespeed.defer_js_experimental=true;</script>\
        <script type=\"text/javascript\" src=\"/psajs/js_defer.0.js\"></script></body>");

    let mut allow = ps_config::wildcard::WildcardGroup::new();
    allow.disallow("*donotmove*");
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(JsDisableFilter::new(JsDisableConfig {
        base_url: url.to_vec(),
        allow,
        defer_applies: true,
        is_ie: false,
        defer_js_experimental: true,
        prioritizing_scripts: false,
    })));
    parser.add_filter(Box::new(JsDeferDisabledFilter::new(config_for(
        true,
        DEFER_JS_URL,
    ))));
    parser.add_filter(Box::new(SupportNoscriptFilter::new(noscript_url, true)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url, false);
    parser.write(input.as_bytes());
    parser.finish();
    assert_eq!(sink.bytes(), expected);
}
