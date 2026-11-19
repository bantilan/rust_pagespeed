//! Engine integration tests: translated goldens plus hand-written
//! chunk-split, flush, mutation, and limit tests.
#[path = "harness.rs"]
mod harness;

#[path = "goldens/generated_parse.rs"]
mod generated_parse;

use harness::{
    round_trip, run_with_filters, wrap_body, DeleteCounts, DeleteNodes, ExplicitCloseTag,
    InsertCommentOnFirstDiv, InsertScripts,
};
use ps_html::name::Keyword;
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

use harness::SharedSink;

/// Ports `ExpectAutoClose`/`ExpectNoAutoClose` with the explicit-close
/// filter: structural auto-closes serialize their end tags.
fn validate_auto_close(id: &str, tag1: &str, tag2: &str, closes: bool) {
    let input = format!("<div><{tag1}>x<{tag2}>y</div>");
    let expected = if closes {
        format!("<div><{tag1}>x</{tag1}><{tag2}>y</{tag2}></div>")
    } else {
        format!("<div><{tag1}>x<{tag2}>y</{tag2}></{tag1}></div>")
    };
    let wrapped_input = wrap_body(input.as_bytes());
    let wrapped_expected = wrap_body(expected.as_bytes());
    let output = run_with_filters(&wrapped_input, |parser| {
        parser.add_filter(Box::new(ExplicitCloseTag));
    });
    assert_eq!(output, wrapped_expected, "case {id}");
}

#[test]
fn auto_close_matrix() {
    // Port of ExpectAutoClose/ExpectNoAutoClose loops.
    let close_cases = [
        ("li", "li"),
        ("tr", "tr"),
        ("p", "div"),
        ("option", "option"),
        ("dd", "dt"),
    ];
    for (tag1, tag2) in close_cases {
        validate_auto_close(&format!("auto_close_{tag1}_{tag2}"), tag1, tag2, true);
    }
    let nest_cases = [("div", "div"), ("span", "b"), ("ul", "div")];
    for (tag1, tag2) in nest_cases {
        validate_auto_close(&format!("no_auto_close_{tag1}_{tag2}"), tag1, tag2, false);
    }
}

#[test]
fn chunk_splits_match_whole_parse() {
    // Every split point must produce the whole-parse output (the C++
    // ParseWithFlush idea, generalized to all boundaries).
    let inputs: &[&[u8]] = &[
        b"<div class=\"a\">Hello <b>world</b><!-- c --></div>",
        b"<script>if (a < b) { x = '</div>'; }</script><p>after",
        b"<table><tr><td>1<td>2</table><p>tail",
        b"<a href=\"a&amp;b\">x</a><img src=i>",
        b"<!DOCTYPE html><html><head><title>t</title></head><body>hi",
    ];
    for input in inputs {
        let expected = round_trip(input);
        for split in 0..=input.len() {
            let sink = SharedSink::default();
            let mut parser = Parser::new();
            parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
            parser.start_parse(b"http://test.com/split.html", false);
            parser.write(&input[..split]);
            parser.flush();
            parser.write(&input[split..]);
            parser.finish();
            assert_eq!(
                sink.bytes(),
                expected,
                "split at {split} of {}",
                String::from_utf8_lossy(input)
            );
        }
    }
}

#[test]
fn three_chunk_splits_match() {
    // A sample of two-split combinations over a tricky input.
    let input = b"<div><script>var s = \"</div>\"; /* <!-- */</script><p>x</p></div>";
    let expected = round_trip(input);
    let mut splits = Vec::new();
    for first in (0..input.len()).step_by(7) {
        for second in (first..=input.len()).step_by(11) {
            splits.push((first, second));
        }
    }
    assert!(splits.len() > 20);
    for (first, second) in splits {
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(b"http://test.com/split3.html", false);
        parser.write(&input[..first]);
        parser.flush();
        parser.write(&input[first..second]);
        parser.flush();
        parser.write(&input[second..]);
        parser.finish();
        assert_eq!(sink.bytes(), expected, "splits {first}/{second}");
    }
}

#[test]
fn writer_case_folds_names() {
    // Port of the C++ CaseFold test (no body wrap): tag and attribute
    // names fold to lowercase, values keep their case.
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    let mut writer = WriterFilter::new(sink.clone());
    writer.set_case_fold(true);
    parser.add_filter(Box::new(writer));
    parser.start_parse(b"http://test.com/fold.html", false);
    parser.write(b"<DiV><Other xY='AbC' Href='dEf'>Hello</OTHER></diV>");
    parser.finish();
    assert_eq!(
        sink.bytes(),
        b"<div><other xy='AbC' href='dEf'>Hello</other></div>"
    );
}

#[test]
fn empty_and_trivial_documents() {
    assert_eq!(round_trip(b""), b"");
    assert_eq!(round_trip(b"hello"), b"hello");
    assert_eq!(round_trip(b"<br>"), b"<br>");
    assert_eq!(round_trip(b"<br/>"), b"<br/>");
    assert_eq!(round_trip(b"<div></div>"), b"<div></div>");
    assert_eq!(round_trip(b"<DIV></DIV>"), b"<DIV></DIV>");
}

/// Runs one parse under a size limit, optionally flushing after every
/// byte (ports the C++ `SizeLimit`/`CheckOutput` flow; no body wrap).
fn run_with_size_limit(input: &[u8], limit: i64, flush_each_byte: bool) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.set_size_limit(limit);
    parser.start_parse(b"http://test.com/in.html", false);
    if flush_each_byte {
        for byte in input {
            parser.write(&[*byte]);
            parser.flush();
        }
    } else {
        parser.write(input);
    }
    parser.finish();
    assert_eq!(
        parser.size_limit_exceeded(),
        (input.len() as i64) > limit,
        "exceeded boundary for limit {limit}"
    );
    sink.bytes()
}

#[test]
fn size_limit_stops_parsing() {
    // Port of the C++ SizeLimit test (no body wrap).
    let input = concat!(
        "<html>",                                                  // 6 chars
        "<input type=\"text\"/>",                                  // 20 chars
        "<script type=\"text/javascript\">alert('123');</script>", // 53 chars
        "<!--[if IE]>...<![endif]-->",                             // 27 chars
        "<table><tr><td>blah</td></tr></table>",                   // 37 chars
        "</html>",                                                 // 7 chars
    );
    assert_eq!(round_trip(input.as_bytes()), input.as_bytes());

    // With no flushes, any tripped limit yields just `<html></html>`.
    for limit in 1..150 {
        let output = run_with_size_limit(input.as_bytes(), limit, false);
        assert_eq!(output, b"<html></html>", "no-flush limit {limit}");
    }

    // With per-byte flushes, each limit band pins a partial window.
    let head = "<html><input type=\"text\"/>";
    let script_empty = "<script type=\"text/javascript\"></script>";
    let script_full = "<script type=\"text/javascript\">alert('123');</script>";
    let comment = "<!--[if IE]>...<![endif]-->";
    let bands: &[(i64, i64, String)] = &[
        (1, 6, "<html></html>".to_string()),
        (6, 26, format!("{head}</html>")),
        (26, 57, format!("{head}{script_empty}</html>")),
        (57, 79, format!("{head}{script_full}</html>")),
        (
            79,
            113,
            format!("{head}{script_full}{comment}<table></table></html>"),
        ),
        (
            113,
            117,
            format!("{head}{script_full}{comment}<table><tr></tr></table></html>"),
        ),
        (
            117,
            121,
            format!("{head}{script_full}{comment}<table><tr><td></td></tr></table></html>"),
        ),
        (
            121,
            160,
            format!("{head}{script_full}{comment}<table><tr><td>blah</td></tr></table></html>"),
        ),
    ];
    for (start, end, expected) in bands {
        for limit in *start..*end {
            let output = run_with_size_limit(input.as_bytes(), limit, true);
            assert_eq!(output, expected.as_bytes(), "flush limit {limit}");
        }
    }
}

/// Runs one no-wrap parse, returning output + exceeded flag.
fn run_unbounded(input: &[u8]) -> (Vec<u8>, bool) {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/tok.html", false);
    parser.write(input);
    parser.finish();
    let exceeded = parser.size_limit_exceeded();
    (sink.bytes(), exceeded)
}

#[test]
fn token_size_limit_stops_huge_tokens() {
    // Ports the C++ TokenSizeLimit tests (no body wrap): a single token
    // past 10MB trips size_limit_exceeded and aborts the rest.
    const MAX_TOKEN: usize = 10 * 1024 * 1024;
    let mut huge_comment = b"<!--".to_vec();
    huge_comment.extend(std::iter::repeat_n(b'A', MAX_TOKEN + 2));
    let (_, exceeded) = run_unbounded(&huge_comment);
    assert!(exceeded, "unclosed huge comment must trip the guard");

    let mut closed_comment = b"<!--".to_vec();
    closed_comment.extend(std::iter::repeat_n(b'A', MAX_TOKEN + 1));
    closed_comment.extend_from_slice(b"--><div id=\"marker\">visible</div>");
    let (output, exceeded) = run_unbounded(&closed_comment);
    assert!(exceeded);
    assert!(
        !output.windows(6).any(|w| w == b"marker"),
        "bytes past the guard must not parse"
    );

    let (output, exceeded) = run_unbounded(b"<!-- short comment --><div id=\"marker\"></div>");
    assert!(!exceeded);
    assert!(output.windows(6).any(|w| w == b"marker"));

    let mut huge_textarea = b"<textarea>".to_vec();
    huge_textarea.extend(std::iter::repeat_n(b'B', MAX_TOKEN + 2));
    let (_, exceeded) = run_unbounded(&huge_textarea);
    assert!(exceeded, "huge textarea must trip the guard");
}

#[test]
fn pass_through_serves_rest_verbatim() {
    // Past the threshold, bytes flow unparsed but complete.
    let input = wrap_body(b"<div>hello <b>world</b></div>");
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/passthrough.html", false);
    parser.set_pass_through_after(20);
    parser.write(&input);
    parser.finish();
    assert!(parser.passing_through());
    assert_eq!(sink.bytes(), input);
}

#[test]
fn buffer_reuse_across_documents() {
    // One parser instance serves many documents (state resets per parse).
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    for _ in 0..3 {
        parser.start_parse(b"http://test.com/reuse.html", false);
        parser.write(b"<p>one</p>");
        parser.finish();
    }
    assert_eq!(sink.bytes(), b"<p>one</p><p>one</p><p>one</p>");
}

#[test]
fn writer_batches_small_tokens() {
    // Small tokens batch into few downstream writes (16KB staging).
    use ps_html::writer::ByteSink;
    use std::cell::RefCell;
    use std::rc::Rc;
    #[derive(Debug, Default)]
    struct Counts {
        writes: usize,
        bytes: usize,
    }
    #[derive(Debug, Clone, Default)]
    struct CountingSink {
        counts: Rc<RefCell<Counts>>,
    }
    impl ByteSink for CountingSink {
        fn write_bytes(&mut self, bytes: &[u8]) -> bool {
            let mut counts = self.counts.borrow_mut();
            counts.writes += 1;
            counts.bytes += bytes.len();
            true
        }
    }
    let sink = CountingSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/batch.html", false);
    parser.write(&wrap_body(b"<div>a</div><div>b</div><div>c</div>"));
    parser.finish();
    // One staged write (plus none wasted): everything fit in 16KB.
    let counts = sink.counts.borrow();
    assert_eq!(counts.writes, 1);
    assert!(counts.bytes > 50);
}

#[test]
fn comment_around_first_div() {
    // Ports CommentInsideFirstDiv/CommentAfterFirstDiv (no body wrap).
    let cases = [
        (true, "1<!--hello--><div>2</div>3<div>4</div>5"),
        (false, "1<div>2</div><!--hello-->3<div>4</div>5"),
    ];
    for (at_start, expected) in cases {
        let output = run_with_filters(b"1<div>2</div>3<div>4</div>5", |parser| {
            parser.add_filter(Box::new(InsertCommentOnFirstDiv::new(at_start)));
        });
        assert_eq!(output, expected.as_bytes(), "at_start={at_start}");
    }
}

#[test]
fn insert_scripts_around_head() {
    // Ports the eight Insert*Script tests (no body wrap): (before,
    // at_start, external, expected).
    let cases: &[(bool, bool, bool, &str)] = &[
        (
            false,
            true,
            false,
            "<head><script>inserted</script>text</head>",
        ),
        (
            true,
            false,
            false,
            "<head>text<script>inserted</script></head>",
        ),
        (
            true,
            true,
            false,
            "<script>inserted</script><head>text</head>",
        ),
        (
            false,
            false,
            false,
            "<head>text</head><script>inserted</script>",
        ),
        (
            false,
            true,
            true,
            "<head><script src=\"inserted\"></script>text</head>",
        ),
        (
            true,
            false,
            true,
            "<head>text<script src=\"inserted\"></script></head>",
        ),
        (
            true,
            true,
            true,
            "<script src=\"inserted\"></script><head>text</head>",
        ),
        (
            false,
            false,
            true,
            "<head>text</head><script src=\"inserted\"></script>",
        ),
    ];
    for (before, at_start, external, expected) in cases {
        let output = run_with_filters(b"<head>text</head>", |parser| {
            parser.add_filter(Box::new(InsertScripts::new(*before, *at_start, *external)));
        });
        assert_eq!(
            output,
            expected.as_bytes(),
            "before={before} at_start={at_start} external={external}"
        );
    }
}

#[test]
fn delete_saving_children_cases() {
    // Ports the eight EventListOrderTest DeleteSavingChildren cases (no
    // body wrap): (id, on_open, node, from, input, expected, starts,
    // ends, chars).
    let div_p = "<div><p>1</p></div><span>2</span>";
    let cases: &[(
        &str,
        bool,
        Keyword,
        Keyword,
        &str,
        &str,
        usize,
        usize,
        usize,
    )] = &[
        (
            "open",
            true,
            Keyword::Div,
            Keyword::Div,
            div_p,
            "<p>1</p><span>2</span>",
            3,
            2,
            2,
        ),
        (
            "close",
            false,
            Keyword::Div,
            Keyword::Div,
            div_p,
            "<p>1</p><span>2</span>",
            3,
            3,
            2,
        ),
        (
            "inner",
            true,
            Keyword::Div,
            Keyword::P,
            div_p,
            "<p>1</p><span>2</span>",
            3,
            2,
            2,
        ),
        (
            "outer",
            true,
            Keyword::Div,
            Keyword::Span,
            div_p,
            "<p>1</p><span>2</span>",
            3,
            3,
            2,
        ),
        (
            "inner_middle",
            false,
            Keyword::Div,
            Keyword::P,
            "<div><p>1</p>2<span>3</span></div><span>4</span>",
            "<p>1</p>2<span>3</span><span>4</span>",
            4,
            3,
            4,
        ),
        (
            "inner_end",
            false,
            Keyword::Div,
            Keyword::P,
            div_p,
            "<p>1</p><span>2</span>",
            3,
            2,
            2,
        ),
        (
            "inner_deep",
            false,
            Keyword::Div,
            Keyword::P,
            "<div><a><p>1</p>2<span>3</span></a></div><span>4</span>",
            "<a><p>1</p>2<span>3</span></a><span>4</span>",
            5,
            4,
            4,
        ),
        (
            "outer_distant",
            false,
            Keyword::Div,
            Keyword::A,
            "<div><p>1</p></div><span>2</span><a>3</a>",
            "<p>1</p><span>2</span><a>3</a>",
            4,
            4,
            3,
        ),
    ];
    for (id, on_open, node, from, input, expected, starts, ends, chars) in cases {
        let counts = DeleteCounts::default();
        let mut deleter = DeleteNodes::new(&counts);
        deleter.set_delete_on_open_tag(*on_open);
        deleter.set_delete_node_type(*node);
        deleter.set_delete_from_type(*from);
        let sink = SharedSink::default();
        let mut parser = Parser::new();
        parser.add_filter(Box::new(deleter));
        parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
        parser.start_parse(b"http://test.com/del.html", false);
        parser.write(input.as_bytes());
        parser.finish();
        assert_eq!(sink.bytes(), expected.as_bytes(), "case {id}");
        let values = counts.values();
        assert_eq!(values.starts, *starts, "case {id} starts");
        assert_eq!(values.ends, *ends, "case {id} ends");
        assert_eq!(values.chars, *chars, "case {id} chars");
        assert_eq!(values.deleted, 1, "case {id} deleted");
    }
}
