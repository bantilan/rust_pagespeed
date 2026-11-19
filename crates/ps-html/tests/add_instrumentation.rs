//! Instrumentation tests ported from `add_instrumentation_filter_test.cc`.
//!
//! NOTE: the three `AddInstrumentationAmpTest` cases are not ported —
//! they need driver machinery (event buffering until the AMP verdict plus
//! dynamic disabling of script-injecting filters) that has not landed.
#[path = "harness.rs"]
mod harness;

use harness::{SharedFilter, SharedSink};
use ps_config::options::BeaconUrl;
use ps_html::filters::add_instrumentation::{AddInstrumentationConfig, AddInstrumentationFilter};
use ps_html::filters::common::XhtmlStatus;
use ps_html::js_escape::escape_to_js_string_literal;
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

const BEACON: &str = "http://example.com/beacon?org=xxx";
const INJECT_INPUT: &[u8] = b"<head></head><head></head><body></body><body></body>";

fn beacon() -> BeaconUrl {
    BeaconUrl::parse(BEACON).expect("beacon parses")
}

fn config_for(beacon: &BeaconUrl) -> AddInstrumentationConfig {
    AddInstrumentationConfig {
        beacon_http: beacon.http.as_bytes().to_vec(),
        beacon_https: beacon.https.as_bytes().to_vec(),
        xhtml: XhtmlStatus::IsNotXhtml,
        ..AddInstrumentationConfig::default()
    }
}

fn run_case(
    url: &[u8],
    input: &[u8],
    config: AddInstrumentationConfig,
) -> (Vec<u8>, SharedFilter<AddInstrumentationFilter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(AddInstrumentationFilter::new(config));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url, false);
    parser.write(input);
    parser.finish();
    (sink.bytes(), filter)
}

/// Ports `CreateInitString`: the init call with an escaped page URL.
fn init_string(page_url: &[u8], beacon: &str, event: &str, params: &str) -> Vec<u8> {
    let mut out = b"pagespeed.addInstrumentationInit('".to_vec();
    out.extend_from_slice(beacon.as_bytes());
    out.extend_from_slice(b"', '");
    out.extend_from_slice(event.as_bytes());
    out.extend_from_slice(b"', '");
    out.extend_from_slice(params.as_bytes());
    out.extend_from_slice(b"', '");
    out.extend_from_slice(&escape_to_js_string_literal(page_url, false));
    out.extend_from_slice(b"');");
    out
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Ports `RunInjection`: parses the stock document and checks the head
/// script fired exactly once.
fn run_injection(
    url: &[u8],
    config: AddInstrumentationConfig,
) -> (Vec<u8>, SharedFilter<AddInstrumentationFilter>) {
    let (output, filter) = run_case(url, INJECT_INPUT, config);
    assert_eq!(
        filter.with(|filter| filter.script_added_count()),
        1,
        "head script fires once: {}",
        String::from_utf8_lossy(&output)
    );
    (output, filter)
}

#[test]
fn script_injection() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let (output, _) = run_injection(url, config_for(&beacon));
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "")
    ));
}

#[test]
fn script_injection_with_navigation() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.report_unload_time = true;
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "beforeunload", "")
    ));
}

#[test]
fn script_injection_with_https() {
    let beacon = beacon();
    let url = b"https://example.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.is_https = true;
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.https, "load", "")
    ));
}

#[test]
fn script_injection_with_https_unload_and_xhtml() {
    let beacon = beacon();
    let url = b"https://example.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.is_https = true;
    config.report_unload_time = true;
    config.xhtml = XhtmlStatus::IsXhtml;
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.https, "beforeunload", "")
    ));
}

#[test]
fn experiment_id_reporting() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.experiment_id = Some(2);
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "&exptid=2")
    ));
}

#[test]
fn extended_instrumentation() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.enable_extended_instrumentation = true;
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "")
    ));
    assert!(contains(&output, b"getResourceTimingData=function()"));
}

#[test]
fn headers_fetch_timing_reporting() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.header_fetch_ms = Some(200);
    config.fetch_ms = Some(500);
    config.ttfb_ms = Some(300);
    let (output, _) = run_injection(url, config);
    assert!(
        contains(
            &output,
            &init_string(url, &beacon.http, "load", "&hft=200&ft=500&s_ttfb=300")
        ),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn quoting() {
    let beacon = beacon();
    let url = b"http://example.com/?');alert('foo)";
    let (output, _) = run_case(url, b"<head></head><body></body>", config_for(&beacon));
    assert!(!contains(&output, b"?');alert('foo)"));
}

#[test]
fn beacon_url_is_js_escaped() {
    let beacon =
        BeaconUrl::parse("http://example.com/beacon?x=');alert(1);//").expect("beacon parses");
    let url = b"http://test.com/index.html?a&b";
    let (output, _) = run_injection(url, config_for(&beacon));
    let mut quoted = vec![b'\''];
    quoted.extend_from_slice(&escape_to_js_string_literal(beacon.http.as_bytes(), false));
    quoted.extend_from_slice(b"',");
    assert!(
        contains(&output, &quoted),
        "{}",
        String::from_utf8_lossy(&output)
    );
    // The unescaped splice would terminate the literal at the quote after x=.
    assert!(
        !contains(&output, b"x=');alert(1);//"),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn script_after_title_and_meta() {
    let beacon = beacon();
    let (output, _) = run_case(
        b"http://test.com/index.html?a&b",
        b"<head><meta name='abc' /><title></title></head><body></body>",
        config_for(&beacon),
    );
    // NOTE: the writer normalizes ` />` to `/>` (pinned engine-wide, e.g.
    // in the base-tag tests); the filter point is the script after title.
    assert!(
        contains(&output, b"<head><meta name='abc'/><title></title><script"),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn non200_response() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.status_code = Some(403);
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "&rc=403")
    ));
}

#[test]
fn request_id() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.request_id = Some(1234567890);
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "&id=1234567890")
    ));
}

#[test]
fn no_defer_marker_present() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let (output, _) = run_injection(url, config_for(&beacon));
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "")
    ));
    assert!(contains(&output, b"data-pagespeed-no-defer"));
}

#[test]
fn no_defer_marker_deferred() {
    let beacon = beacon();
    let url = b"http://test.com/index.html?a&b";
    let mut config = config_for(&beacon);
    config.defer_instrumentation_script = true;
    let (output, _) = run_injection(url, config);
    assert!(contains(
        &output,
        &init_string(url, &beacon.http, "load", "")
    ));
    assert!(!contains(&output, b"data-pagespeed-no-defer"));
}

#[test]
fn disable_for_bots() {
    let beacon = beacon();
    let mut config = config_for(&beacon);
    config.is_bot = true;
    let (output, _) = run_case(b"http://test.com/index.html?a&b", INJECT_INPUT, config);
    assert_eq!(output, INJECT_INPUT);
}

#[test]
fn script_tag_type_attribute() {
    let beacon = beacon();
    let input = b"<!DOCTYPE html><html><head></head><body><img src='Puzzle.jpg'/></body></html>";
    let (output, _) = run_case(b"http://test.com/", input, config_for(&beacon));
    assert!(
        contains(&output, b"<script>window.mod_pagespeed_start"),
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        contains(&output, b"<script data-pagespeed-no-defer>"),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn script_tag_type_attribute_pedantic() {
    let beacon = beacon();
    let input = b"<!DOCTYPE html><html><head></head><body><img src='Puzzle.jpg'/></body></html>";
    let mut config = config_for(&beacon);
    config.pedantic = true;
    let (output, _) = run_case(b"http://test.com/", input, config);
    assert!(
        contains(
            &output,
            b"<script type='text/javascript'>window.mod_pagespeed_start"
        ),
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        contains(
            &output,
            b"<script data-pagespeed-no-defer type=\"text/javascript\">"
        ),
        "{}",
        String::from_utf8_lossy(&output)
    );
}
