//! AMP detection tests ported from `amp_document_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;

use harness::SharedSink;
use ps_html::filters::{AmpDocumentFilter, INVALID_AMP_DIRECTIVE_COMMENT, UTF8_LIGHTNING_BOLT};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

/// Parses with a flush at `split`, returning output + discovery verdicts.
fn run_with_flush_point(html: &[u8], split: usize) -> (Vec<u8>, Vec<bool>) {
    let verdicts = Rc::new(RefCell::new(Vec::new()));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    let captured = verdicts.clone();
    parser.add_filter(Box::new(AmpDocumentFilter::new(move |is_amp| {
        captured.borrow_mut().push(is_amp);
    })));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://example.com/amp_doc.html", false);
    parser.write(&html[..split]);
    parser.flush();
    parser.write(&html[split..]);
    parser.finish();
    let output = sink.bytes();
    let verdicts = verdicts.borrow().to_vec();
    (output, verdicts)
}

/// Every flush split must agree on one verdict (`RunWithEveryFlushPoint`).
fn assert_every_flush_point(html: &[u8], expect_amp: bool) {
    for split in 0..html.len() {
        let (_, verdicts) = run_with_flush_point(html, split);
        assert_eq!(verdicts, vec![expect_amp], "split {split}");
    }
}

#[test]
fn gives_up_after_buffering_a_lot() {
    // Comments before the root are tolerated up to 64KB, then give up.
    let mut comments = Vec::new();
    for _ in 0..(64 * 1024 / 200) {
        comments.extend_from_slice(b"<!-- 0123456789012345678901234567890123456789 -->");
    }
    let verdicts = Rc::new(RefCell::new(Vec::new()));
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    let captured = verdicts.clone();
    parser.add_filter(Box::new(AmpDocumentFilter::new(move |is_amp| {
        captured.borrow_mut().push(is_amp);
    })));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://example.com/amp_doc_many_comments.html", false);
    parser.write(&comments);
    parser.flush();
    assert!(verdicts.borrow().is_empty());
    for _ in 0..4 {
        parser.write(&comments);
    }
    parser.flush();
    assert_eq!(verdicts.borrow().to_vec(), vec![false]);
    parser.write(b"<html amp></html>");
    parser.finish();
}

#[test]
fn is_amp_html() {
    assert_every_flush_point(
        b"<!doctype foo> <html amp><head/><body></body></html>",
        true,
    );
}

#[test]
fn is_amp_html_case_insensitive() {
    assert_every_flush_point(
        b"<!doctype foo> <html AMP><head/><body></body></html>",
        true,
    );
    assert_every_flush_point(
        b"<!doctype foo> <html aMP><head/><body></body></html>",
        true,
    );
}

#[test]
fn is_amp_lightning_bolt() {
    let mut html = b"<!doctype foo>  <html ".to_vec();
    html.extend_from_slice(UTF8_LIGHTNING_BOLT);
    html.extend_from_slice(b"><head/><body></body></html>");
    assert_every_flush_point(&html, true);
}

#[test]
fn is_not_amp() {
    assert_every_flush_point(b"<!doctype foo>  <html><head/><body></body></html>", false);
}

#[test]
fn empty_html() {
    // The C++ loop is vacuous for empty input; assert the end-of-document
    // verdict directly instead.
    let (_, verdicts) = run_with_flush_point(b"", 0);
    assert_eq!(verdicts, vec![false]);
}

#[test]
fn just_text() {
    assert_every_flush_point(b"Hello, world!", false);
}

#[test]
fn just_image() {
    assert_every_flush_point(b"<img src='foo.png' />", false);
}

#[test]
fn doctype_only() {
    assert_every_flush_point(b"<!doctype foo>", false);
}

#[test]
fn doctype_text() {
    assert_every_flush_point(b"<!doctype foo>Hello", false);
}

#[test]
fn doctype_image() {
    assert_every_flush_point(b"<!doctype foo><img src='foo.png' />", false);
}

#[test]
fn two_html_tags_amp_in_first() {
    assert_every_flush_point(b"<!doctype foo><html amp></html><html></html>", true);
}

#[test]
fn two_html_tags_amp_in_second() {
    assert_every_flush_point(b"<!doctype foo><html></html><html amp></html>", false);
}

#[test]
fn too_late_for_amp_tag() {
    let input = b"<!doctype foo><other/><html amp><body></body></html>";
    let mut expected = b"<!doctype foo><other/><!--".to_vec();
    expected.extend_from_slice(INVALID_AMP_DIRECTIVE_COMMENT);
    expected.extend_from_slice(b"--><html amp><body></body></html>");
    let (output, verdicts) = run_with_flush_point(input, input.len());
    assert_eq!(output, expected);
    assert_eq!(verdicts, vec![false]);
}

#[test]
fn too_late_for_amp_text() {
    let input = b"<!doctype foo>  stuff <html amp><body></body></html>";
    let mut expected = b"<!doctype foo>  stuff <!--".to_vec();
    expected.extend_from_slice(INVALID_AMP_DIRECTIVE_COMMENT);
    expected.extend_from_slice(b"--><html amp><body></body></html>");
    let (output, verdicts) = run_with_flush_point(input, input.len());
    assert_eq!(output, expected);
    assert_eq!(verdicts, vec![false]);
}
