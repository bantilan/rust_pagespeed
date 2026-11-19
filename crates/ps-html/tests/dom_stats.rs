//! DOM statistics tests ported from `dom_stats_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use std::collections::HashSet;

use harness::{SharedFilter, SharedSink};
use ps_html::filters::DomStatsFilter;
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

fn run_case(
    case: &str,
    input: &[u8],
    critical: Option<HashSet<String>>,
) -> (Vec<u8>, SharedFilter<DomStatsFilter>) {
    let sink = SharedSink::default();
    let mut inner = DomStatsFilter::new();
    if let Some(images) = critical {
        inner.set_critical_images(images);
    }
    let stats = SharedFilter::new(inner);
    let mut parser = Parser::new();
    parser.add_filter(Box::new(stats.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(input);
    parser.finish();
    (sink.bytes(), stats)
}

fn validate_no_changes(
    case: &str,
    input: &[u8],
    critical: Option<HashSet<String>>,
) -> SharedFilter<DomStatsFilter> {
    let (output, stats) = run_case(case, input, critical);
    assert_eq!(output, input, "case {case}");
    stats
}

#[test]
fn img_tags() {
    let stats = validate_no_changes(
        "img_tags",
        b"<html><body><img><img src='a'><noscript><img><img src='b'></noscript></body></html>",
        None,
    );
    assert_eq!(stats.with(|filter| filter.num_img_tags()), 4);
}

#[test]
fn inlined_img_tags() {
    let stats = validate_no_changes(
        "inlined_img",
        b"<html><body><img src='data:abc'></body></html>",
        None,
    );
    assert_eq!(stats.with(|filter| filter.num_inlined_img_tags()), 1);
}

#[test]
fn external_css() {
    let stats = validate_no_changes(
        "external_css",
        b"<html><body><link rel=stylesheet href='abc'><link rel='alternate stylesheet' href='def'><link rel='stylesheet'><link rel='junk' href='ghi'></body></html>",
        None,
    );
    assert_eq!(stats.with(|filter| filter.num_external_css()), 2);
}

#[test]
fn num_scripts() {
    let stats = validate_no_changes(
        "num_scripts",
        b"<html><body><script src='abc'></script><script></script></body></html>",
        None,
    );
    assert_eq!(stats.with(|filter| filter.num_scripts()), 2);
}

#[test]
fn critical_images_used() {
    let critical: HashSet<String> = ["a", "c", "d"]
        .iter()
        .map(|leaf| format!("http://test.com/{leaf}"))
        .collect();
    let stats = validate_no_changes(
        "critical_images_used",
        b"<html><body><img src='a'><img src='a'><img src='b'></body></html>",
        Some(critical),
    );
    // Image 'a' is the only critical image used and it is used twice.
    assert_eq!(stats.with(|filter| filter.num_critical_images_used()), 2);
}
