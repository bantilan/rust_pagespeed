//! Proxy-resource counting tests ported from
//! `rewritten_content_scanning_filter_test.cc`.
//!
//! NOTE: the C++ suite reads the count back through the DOM-cohort
//! property cache; that write stays driver-side, so these tests read the
//! filter counter directly (the values asserted are the same).
#[path = "harness.rs"]
mod harness;

use harness::{SharedFilter, SharedSink};
use ps_html::filters::RewrittenContentScanningFilter;
use ps_html::parser::Parser;
use ps_html::url_namer::TestUrlNamer;
use ps_html::writer::WriterFilter;

fn count_for(case: &str, input: &[u8]) -> i32 {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(RewrittenContentScanningFilter::new(
        TestUrlNamer::new(),
        Vec::new(),
    ));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(input);
    parser.finish();
    filter.with(|filter| filter.num_proxied_rewritten_resources())
}

#[test]
fn no_rewritten_resource() {
    assert_eq!(
        count_for(
            "no_rewritten_resource",
            b"<html><head></head><body><img src=\"1.jpeg\"/><script src=\"1.js\"/></body></html>",
        ),
        0
    );
}

#[test]
fn count_rewritten_resource() {
    assert_eq!(
        count_for(
            "count_rewritten_resource",
            b"<html><head></head><body><link rel=\"stylesheet\" href=\"http://cdn.com/d.css.pagespeed.cf.0.css\"/><script src=\"http://cdn.com/c.js.pagespeed.jm.0.js\"/><script src=\"http://d.com/c.js.pagespeed.jm.0.js\"/></body></html>",
        ),
        2
    );
}
