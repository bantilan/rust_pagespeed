//! Decode tests ported from `decode_rewritten_urls_filter_test.cc`
//! (log-record assertions skipped: no logging channel yet).
#[path = "harness.rs"]
mod harness;

use harness::SharedSink;
use ps_config::options::RewriteOptions;
use ps_html::filters::decode_rewritten_urls::{
    DecodeRewrittenUrlsConfig, DecodeRewrittenUrlsFilter,
};
use ps_html::parser::Parser;
use ps_html::url_namer::TestUrlNamer;
use ps_html::writer::WriterFilter;

#[test]
fn test_all() {
    let input = b"<html><head><link type=\"text/css\" rel=\"stylesheet\" href=\"http://test.com/a.css\"/><link type=\"text/css\" rel=\"stylesheet\" href=\"b.css.pagespeed.ce.0.css\" media=\"print\"/><link type=\"text/css\" rel=\"stylesheet\" href=\"http://www.test.com/I.e.css+f.css.pagespeed.cc.0.css\" media=\"print\"/></head><body><script src=\"http://test.com/c.js.pagespeed.jm.555.js\"></script><script src=\"http://test.com/d.js.pagespeed.b.jm.0.js\"></script></body></html>";
    let expected = b"<html><head><link type=\"text/css\" rel=\"stylesheet\" href=\"http://test.com/a.css\"/><link type=\"text/css\" rel=\"stylesheet\" href=\"http://test.com/b.css\" media=\"print\"/><link type=\"text/css\" rel=\"stylesheet\" href=\"http://www.test.com/I.e.css+f.css.pagespeed.cc.0.css\" media=\"print\"/></head><body><script src=\"http://test.com/c.js\"></script><script src=\"http://test.com/d.js\"></script></body></html>";
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(DecodeRewrittenUrlsFilter::new(
        DecodeRewrittenUrlsConfig {
            options: RewriteOptions::new(),
            hash_length: 10,
            signature_length: 0,
            url_valued: Vec::new(),
        },
        TestUrlNamer::new(),
    )));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/different_urls.html", false);
    parser.write(input);
    parser.finish();
    assert_eq!(sink.bytes(), expected);
}
