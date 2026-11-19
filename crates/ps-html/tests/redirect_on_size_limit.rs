//! Size-limit redirect tests ported from `redirect_on_size_limit_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use harness::SharedSink;
use ps_html::filters::RedirectOnSizeLimitFilter;
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;

const INPUT: &[u8] = b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script><!--[if IE]>...<![endif]--><table><tr><td>blah</td></tr></table></html>";

fn redirect_script() -> Vec<u8> {
    b"<script type=\"text/javascript\">window.location=\"http://test.com/in.html?PageSpeed=off\";</script>".to_vec()
}

fn run_split(input: &[u8], limit: i64, split: usize) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(RedirectOnSizeLimitFilter::new(true)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/in.html", false);
    parser.set_size_limit(limit);
    parser.write(&input[..split]);
    parser.flush();
    parser.write(&input[split..]);
    parser.finish();
    sink.bytes()
}

fn check_output(start: i64, end: i64, expected: &[u8]) {
    for limit in start..end {
        let split = (INPUT.len() - 1).min(limit as usize);
        let output = run_split(INPUT, limit, split);
        assert_eq!(output, expected, "limit {limit} split {split}");
    }
}

#[test]
fn one_flush_window() {
    let mut expected = b"<html>".to_vec();
    expected.extend_from_slice(&redirect_script());
    expected.extend_from_slice(b"</html>");
    check_output(1, 6, &expected);
    check_output(150, 152, INPUT);
}

#[test]
fn flush_before_limit() {
    // Limit 0 disables the check entirely.
    check_output(0, 1, INPUT);
    let script = redirect_script();

    let mut expected = b"<html>".to_vec();
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"<input type=\"text\"/></html>");
    check_output(6, 26, &expected);

    let mut expected = b"<html><input type=\"text\"/>".to_vec();
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"<script type=\"text/javascript\"></script></html>");
    check_output(26, 57, &expected);

    let mut expected = b"<html><input type=\"text\"/>".to_vec();
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"<script type=\"text/javascript\">alert('123');</script></html>");
    check_output(57, 79, &expected);

    let mut expected =
        b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script>"
            .to_vec();
    expected.extend_from_slice(b"<!--[if IE]>...<![endif]-->");
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"<table></table></html>");
    check_output(79, 113, &expected);

    let mut expected =
        b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script>"
            .to_vec();
    expected.extend_from_slice(b"<!--[if IE]>...<![endif]-->");
    expected.extend_from_slice(b"<table>");
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"<tr></tr></table></html>");
    check_output(113, 117, &expected);

    let mut expected =
        b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script>"
            .to_vec();
    expected.extend_from_slice(b"<!--[if IE]>...<![endif]-->");
    expected.extend_from_slice(b"<table><tr>");
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"<td></td></tr></table></html>");
    check_output(117, 121, &expected);

    let mut expected =
        b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script>"
            .to_vec();
    expected.extend_from_slice(b"<!--[if IE]>...<![endif]-->");
    expected.extend_from_slice(b"<table><tr><td>blah</td>");
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"</tr></table></html>");
    check_output(121, 130, &expected);

    let mut expected =
        b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script>"
            .to_vec();
    expected.extend_from_slice(b"<!--[if IE]>...<![endif]-->");
    expected.extend_from_slice(b"<table><tr><td>blah</td></tr>");
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"</table></html>");
    check_output(130, 135, &expected);

    let mut expected =
        b"<html><input type=\"text\"/><script type=\"text/javascript\">alert('123');</script>"
            .to_vec();
    expected.extend_from_slice(b"<!--[if IE]>...<![endif]-->");
    expected.extend_from_slice(b"<table><tr><td>blah</td></tr></table>");
    expected.extend_from_slice(&script);
    expected.extend_from_slice(b"</html>");
    check_output(135, 150, &expected);

    check_output(150, 160, INPUT);
}

#[test]
fn escaping_and_flush() {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(RedirectOnSizeLimitFilter::new(true)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/in.html?'(", false);
    parser.set_size_limit(100);
    parser.write(b"<html><input type=\"text\"/><script type=\"text/javascript\">");
    parser.flush();
    parser.write(b"alert('123');</script><!--[if IE]>...<![endif]--><table><tr><td>blah</td></tr></table></html>");
    parser.finish();
    assert_eq!(
        sink.bytes(),
        b"<html><input type=\"text\"/><script type=\"text/javascript\">window.location=\"http://test.com/in.html?\\'(&PageSpeed=off\";</script><script type=\"text/javascript\">alert('123');</script></html>"
    );
}

#[test]
fn no_redirect_when_csp_forbids_inline() {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(RedirectOnSizeLimitFilter::new(false)));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/in.html", false);
    parser.set_size_limit(80);
    parser.write(b"<html><meta http-equiv=\"Content-Security-Policy\" content=\"script-src *;\">");
    parser.flush();
    parser.write(b"<table><tr><td>blah</td></tr></table></html>");
    parser.finish();
    let output = sink.bytes();
    assert!(
        !output
            .windows(b"window.location".len())
            .any(|window| window == b"window.location"),
        "{}",
        String::from_utf8_lossy(&output)
    );
}
