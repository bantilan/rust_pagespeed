//! Diagnostic tests ported from `debug_filter_test.cc`.
//!
//! NOTE: `FlushWithDelayedCache` and `EndWithDelayedCache` are not
//! ported — they need the cache-extender pipeline plus metadata-cache
//! delay control. The dynamically-disabled lists below reflect this
//! harness's chain (debug + writer), not the C++ full chain.
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use harness::SharedSink;
use ps_html::filter::HtmlFilter;
use ps_html::filters::debug::{DebugConfig, DebugFilter};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

/// Driving harness with a mock microsecond clock.
struct Harness {
    parser: Parser,
    sink: SharedSink,
    clock: Rc<RefCell<i64>>,
}

fn config_for(clock: &Rc<RefCell<i64>>, disabled: &Rc<RefCell<Vec<String>>>) -> DebugConfig {
    DebugConfig {
        clock: clock.clone(),
        critical_images: None,
        enabled_filters: String::new(),
        enabled_options: String::new(),
        beacon_enabled: false,
        disabled: disabled.clone(),
    }
}

fn setup(config: DebugConfig, url: &[u8]) -> Harness {
    let clock = config.clock.clone();
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(DebugFilter::new(config.clone())));
    parser.set_dynamically_disabled_list(config.disabled.clone());
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(url, false);
    Harness {
        parser,
        sink,
        clock,
    }
}

impl Harness {
    fn advance(&self, us: i64) {
        *self.clock.borrow_mut() += us;
    }

    fn output(&self) -> Vec<u8> {
        self.sink.bytes()
    }
}

fn fresh_cells() -> (Rc<RefCell<i64>>, Rc<RefCell<Vec<String>>>) {
    (Rc::new(RefCell::new(0)), Rc::new(RefCell::new(Vec::new())))
}

fn erase(text: &mut Vec<u8>, needle: &[u8]) {
    if needle.is_empty() {
        return;
    }
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text.as_slice();
    while let Some(pos) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        out.extend_from_slice(&rest[..pos]);
        rest = &rest[pos + needle.len()..];
    }
    out.extend_from_slice(rest);
    *text = out;
}

fn split_messages(text: &[u8]) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.windows(4).position(|window| window == b"<!--") {
        let after = &rest[pos + 4..];
        match after.windows(4).position(|window| window == b"<!--") {
            Some(end) => {
                messages.push(after[..end].to_vec());
                rest = &after[end..];
            }
            None => {
                messages.push(after.to_vec());
                break;
            }
        }
    }
    messages
}

/// Ports `ParseAndMaybeFlushTwice`.
fn parse_and_maybe_flush_twice(do_flush: bool) -> (Vec<u8>, Harness) {
    let (clock, disabled) = fresh_cells();
    let mut harness = setup(config_for(&clock, &disabled), b"http://test.com/");
    harness.advance(1);
    harness.parser.write(b"<token>");
    harness.advance(10); // 11us elapsed so far.
    if do_flush {
        harness.parser.flush();
    }
    harness.advance(100); // 111us elapsed so far.
    harness.parser.write(b"<token>");
    harness.advance(1000); // 1111us elapsed so far.
    if do_flush {
        harness.parser.flush();
    }
    harness.advance(10000); // 11111us elapsed so far.
    harness.parser.write(b"<token>");
    harness.advance(100000); // 111111us elapsed so far.
    harness.parser.finish();
    let output = harness.output();
    (output, harness)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn two_flushes() {
    let (output, _) = parse_and_maybe_flush_twice(true);
    let mut text = output.clone();
    // The engine spells the unknown token element per-window (`<token/>`
    // or `<token>`); neither spelling occurs inside the messages.
    erase(&mut text, b"<token/>");
    erase(&mut text, b"<token>");
    erase(&mut text, b"-->");
    let messages = split_messages(&text);
    // Two flushes yield three chunk messages plus the end summary.
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0], DebugFilter::format_flush_message(11, 0, 0, 11));
    assert_eq!(
        messages[1],
        DebugFilter::format_flush_message(1111, 0, 0, 1100)
    );
    assert_eq!(
        messages[2],
        DebugFilter::format_flush_message(111111, 0, 0, 110000)
    );
    let summary = DebugFilter::format_end_document_message(
        111111,
        0,
        0,
        111111,
        2,
        false,
        &Default::default(),
        &[],
    );
    assert!(contains(&messages[3], &summary));
}

#[test]
fn zero_flushes() {
    let (output, _) = parse_and_maybe_flush_twice(false);
    let mut text = output.clone();
    // The engine spells the unknown token element per-window (`<token/>`
    // or `<token>`); neither spelling occurs inside the messages.
    erase(&mut text, b"<token/>");
    erase(&mut text, b"<token>");
    erase(&mut text, b"-->");
    let messages = split_messages(&text);
    // Same totals, no flush messages, flush count zero.
    assert_eq!(messages.len(), 1);
    let summary = DebugFilter::format_end_document_message(
        111111,
        0,
        0,
        111111,
        0,
        false,
        &Default::default(),
        &[],
    );
    assert!(contains(&messages[0], &summary));
}

#[test]
fn check_filters_and_options() {
    let (output, _) = parse_and_maybe_flush_twice(false);
    assert!(contains(&output, b"mod_pagespeed on"));
    assert!(contains(&output, b"Filters:"));
    assert!(contains(&output, b"Options:"));
}

#[test]
fn flush_in_style_tag() {
    // Flush comments buffer out of literal tags until the close tag.
    let (clock, disabled) = fresh_cells();
    let mut harness = setup(config_for(&clock, &disabled), b"http://test.com/");
    harness.advance(1);
    harness.parser.write(b"<style>");
    harness.parser.write(b".a { color:red; }");
    harness.advance(10); // 11us elapsed so far.
    harness.parser.flush();
    harness.advance(10); // 21us elapsed so far.
    harness.parser.write(b".b { color:blue; }");
    harness.advance(10); // 31us elapsed so far.
    harness.parser.flush();
    harness.advance(10); // 41us elapsed so far.
    harness.parser.write(b"</style>");
    harness.advance(10); // 51us elapsed so far.
    harness.parser.finish();
    let output = harness.output();

    let mut expected = b"<!--".to_vec();
    expected.extend_from_slice(&DebugFilter::format_flush_message(11, 0, 0, 11));
    expected.extend_from_slice(b"-->");
    expected.extend_from_slice(b"<style>.a { color:red; }.b { color:blue; }</style>");
    expected.extend_from_slice(b"<!--");
    expected.extend_from_slice(&DebugFilter::format_flush_message(31, 0, 0, 20));
    expected.extend_from_slice(b"-->");
    expected.extend_from_slice(b"<!--");
    expected.extend_from_slice(&DebugFilter::format_flush_message(51, 0, 0, 20));
    expected.extend_from_slice(b"-->");
    assert!(
        contains(&output, &expected),
        "{}",
        String::from_utf8_lossy(&output)
    );

    let mut summary =
        DebugFilter::format_end_document_message(51, 0, 0, 51, 2, false, &Default::default(), &[]);
    summary.extend_from_slice(b"-->");
    assert!(
        contains(&output, &summary),
        "{}",
        String::from_utf8_lossy(&output)
    );
}

#[test]
fn critical_image_message() {
    let (clock, disabled) = fresh_cells();
    let mut config = config_for(&clock, &disabled);
    config.beacon_enabled = true;
    config.critical_images = Some(HashSet::from(["http://test.com/a.jpg".to_string()]));
    let mut harness = setup(config, b"http://test.com/");
    harness
        .parser
        .write(b"<img src=\"a.jpg\"><img src=\"b.jpg\">");
    harness.parser.finish();
    let output = harness.output();
    assert!(
        contains(&output, b"Critical Images:\n\thttp://test.com/a.jpg"),
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert!(!contains(&output, b"http://test.com/b.jpg"));
}

#[test]
fn critical_image_message_blank_src() {
    // A valueless src must not crash.
    let (clock, disabled) = fresh_cells();
    let mut config = config_for(&clock, &disabled);
    config.beacon_enabled = true;
    config.critical_images = Some(HashSet::new());
    let mut harness = setup(config, b"http://test.com/");
    harness.parser.write(b"<img src>");
    harness.parser.finish();
    let output = harness.output();
    assert!(contains(&output, b"No critical images detected.\n"));
}

#[test]
fn no_disabled_filters() {
    let (clock, disabled) = fresh_cells();
    let mut harness = setup(
        config_for(&clock, &disabled),
        b"http://test.com/no_disabled_filters.html",
    );
    harness.parser.write(b"<!-- Empty body -->");
    harness.parser.finish();
    let output = harness.output();
    assert!(contains(&output, b"No filters were disabled"));
    assert!(!contains(&output, b"The following filters were disabled:"));
}

/// Test filter that always disables itself with an optional reason.
struct DisabledFilter {
    name: &'static str,
    reason: Option<String>,
}

impl HtmlFilter for DisabledFilter {
    fn name(&self) -> &'static str {
        self.name
    }

    fn determine_enabled(
        &mut self,
        _dom: &ps_html::parser::Dom,
        _lexer: &ps_html::lexer::Lexer,
    ) -> bool {
        false
    }

    fn disabled_reason(&self) -> Option<String> {
        self.reason.clone()
    }
}

#[test]
fn disabled_filters() {
    let (clock, disabled) = fresh_cells();
    let config = config_for(&clock, &disabled);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(DisabledFilter {
        name: "disabled_filter_1",
        reason: None,
    }));
    parser.add_filter(Box::new(DisabledFilter {
        name: "disabled_filter_2",
        reason: Some("Reasons".to_string()),
    }));
    parser.add_filter(Box::new(DisabledFilter {
        name: "disabled_filter_3",
        reason: None,
    }));
    parser.add_filter(Box::new(DebugFilter::new(config.clone())));
    parser.set_dynamically_disabled_list(config.disabled.clone());
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/disabled_filters.html", false);
    parser.write(b"<!-- Empty body -->");
    parser.finish();
    let output = sink.bytes();
    assert!(!contains(&output, b"No filters were disabled"));
    assert!(
        contains(
            &output,
            b"The following filters were disabled for this request:\n\tdisabled_filter_1\n\tdisabled_filter_2: Reasons\n\tdisabled_filter_3\n"
        ),
        "{}",
        String::from_utf8_lossy(&output)
    );
}
