//! URL left-trim tests ported from `url_left_trim_filter_test.cc`.
#[path = "harness.rs"]
mod harness;

use harness::SharedSink;
use ps_html::filters::UrlLeftTrimFilter;
use ps_html::filters::url_left_trim::{URL_TRIM_SAVED_BYTES, URL_TRIMS};
use ps_html::parser::Parser;
use ps_html::writer::WriterFilter;
use ps_http::google_url::GoogleUrl;
use ps_stats::core::Statistics;

fn one_trim(base: &GoogleUrl, changed: bool, init: &str, expected: &str) {
    assert_eq!(
        UrlLeftTrimFilter::trim(base, init).as_deref(),
        changed.then_some(expected),
        "base {} url {init}",
        base.spec()
    );
}

fn test_anchors(base_url: &str) {
    let base = GoogleUrl::new(base_url);
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/?var=val#anchor",
        "/dir/?var=val#anchor",
    );
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/#anchor",
        "/dir/#anchor",
    );
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/foo.html",
        "foo.html",
    );
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/abc/f?g=h",
        "abc/f?g=h",
    );
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/f?g=h#anchor",
        "f?g=h#anchor",
    );
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/index.html#",
        "index.html#",
    );
    one_trim(
        &base,
        true,
        "http://www.example.com/dir/index.html?f=g#bottom",
        "index.html?f=g#bottom",
    );
    one_trim(&base, false, "#top", "");
}

#[test]
fn simple_trims() {
    let base = GoogleUrl::new("http://foo.bar/baz/");
    one_trim(&base, false, "http://www.google.com/", "");
    one_trim(&base, true, "http://foo.bar/baz/", "/baz/");
    one_trim(&base, true, "http://foo.bar/baz/quux", "quux");
    one_trim(&base, true, "/baz/quux", "quux");
    one_trim(&base, true, "//foo.bar/img/img1.jpg", "/img/img1.jpg");
    one_trim(&base, false, "/img/img1.jpg", "");
    // False because `/baz/` is 5 chars long.
    one_trim(&base, false, "http:", "");
    one_trim(&base, true, "//foo.bar/baz/quux", "quux");
    one_trim(&base, false, "baz/img.jpg", "");
}

#[test]
fn rooted_trims() {
    let base = GoogleUrl::new("http://foo.bar/");
    one_trim(&base, false, "http://www.google.com/", "");
    one_trim(&base, true, "http://foo.bar/baz/", "baz/");
    one_trim(&base, false, "//www.google.com/", "");
    one_trim(&base, true, "/baz/", "baz/");
    one_trim(&base, false, "quux", "");
}

#[test]
fn directories() {
    let base = GoogleUrl::new("http://www.example.com/foo/bar/index.html");
    one_trim(&base, false, "..", "..");
}

#[test]
fn dots() {
    let base = GoogleUrl::new("http://foo/bar/");
    one_trim(&base, true, "foo/bar/../baz/x.html", "foo/baz/x.html");
}

#[test]
fn xkcd() {
    let base = GoogleUrl::new("http://forums.xkcd.com/");
    one_trim(&base, false, "http://xkcd.com/", "");
}

#[test]
fn one_dot() {
    let base = GoogleUrl::new("http://foo.bar/baz/index.html");
    one_trim(&base, true, "./cows/index.html", "cows/index.html");
}

#[test]
fn query() {
    let base = GoogleUrl::new("http://foo.bar/index.html");
    one_trim(&base, true, "http://foo.bar/?a=b", "/?a=b");
}

#[test]
fn trim_query() {
    let base = GoogleUrl::new("http://foo.bar/baz/index.html");
    one_trim(
        &base,
        true,
        "http://foo.bar/baz/other.html?a=b",
        "other.html?a=b",
    );
}

#[test]
fn double_slash_path() {
    let base = GoogleUrl::new("http://foo.bar/baz/index.html");
    one_trim(
        &base,
        true,
        "http://foo.bar/baz//other.html",
        "/baz//other.html",
    );
}

#[test]
fn double_slash_beginning_path() {
    let base = GoogleUrl::new("http://foo.bar/index.html");
    one_trim(&base, false, "http://foo.bar//other.html", "");
}

#[test]
fn triple_slash_path() {
    let base = GoogleUrl::new("http://foo.bar/example/index.html");
    one_trim(
        &base,
        true,
        "http://foo.bar/example///other.html",
        "/example///other.html",
    );
}

#[test]
fn anchors() {
    test_anchors("http://www.example.com/dir/?var=val");
    test_anchors("http://www.example.com/dir/index.html");
    test_anchors("http://www.example.com/dir/index.html#top");
    test_anchors("http://www.example.com/dir/index.html?f=g");
    test_anchors("http://www.example.com/dir/index.html?f=g&y=z#bottom");
}

fn validate_expected(case: &str, input: &[u8], expected: &[u8]) {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(UrlLeftTrimFilter::new(Vec::new())));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(input);
    parser.finish();
    assert_eq!(sink.bytes(), expected, "case {case}");
}

fn image(url: &str) -> Vec<u8> {
    format!("<img src='{url}'>").into_bytes()
}

#[test]
fn no_changes() {
    let input = b"<head><base href='ftp://what.the/heck/'/><link rel='stylesheet' href='http://what.the.cow/heck/'/></head><body><a href='spdy://www.google.com/'>google</a><img src='file:///where/the/heck.jpg'/></body>";
    validate_expected("none_forward", input, input);
}

#[test]
fn some_changes() {
    validate_expected(
        "some_forward",
        b"<head><base href='http://foo.bar/baz/'/><link rel='stylesheet' href='http://foo.bar/baz/'/></head><body><a href='http://www.google.com/'>google</a><img src='http://foo.bar/baz/nav.jpg'/><img src='http://foo.bar/img/img1.jpg'/><img src='/baz/img2.jpg'/><img src='//foo.bar/baz/widget.png'/><a href='./xyz/something.html'>text!</a></body>",
        b"<head><base href='http://foo.bar/baz/'/><link rel='stylesheet' href='/baz/'/></head><body><a href='http://www.google.com/'>google</a><img src='nav.jpg'/><img src='/img/img1.jpg'/><img src='img2.jpg'/><img src='widget.png'/><a href='xyz/something.html'>text!</a></body>",
    );
}

#[test]
fn two_bases() {
    validate_expected(
        "first_doc",
        b"<head><base href='http://foo/'/></head><body><a href='http://foo/abc'>link</a><img src='www.google.com/pretty_picture.jpg'><img src='http://foo/bar/123.png'></body>",
        b"<head><base href='http://foo/'/></head><body><a href='abc'>link</a><img src='www.google.com/pretty_picture.jpg'><img src='bar/123.png'></body>",
    );
    validate_expected(
        "second_doc",
        b"<head><base href='http://newurl/baz/'/></head><body><a href='http://foo/baz/abc'>text</a><a href='http://newurl/baz/target'>more text</a><img src='www.google.com/pretty_picture.jpg'><img src='/baz/image.jpg'></body>",
        b"<head><base href='http://newurl/baz/'/></head><body><a href='http://foo/baz/abc'>text</a><a href='target'>more text</a><img src='www.google.com/pretty_picture.jpg'><img src='image.jpg'></body>",
    );
}

#[test]
fn partial_url() {
    let input = b"<head><base href='http://abcdef/123'/></head><body><a href='abcdef/something'>link</a><img src='http://abcdefg'></body>";
    validate_expected("partial_url", input, input);
}

#[test]
fn midway_base_url() {
    validate_expected(
        "midway_base",
        b"<head><link rel='stylesheet' href='http://foo.bar/baz'/><a href='baz.html'>strange link in header</a><base href='http://foo.bar'></head><body><img src='//foo.bar/img.jpg'></body>",
        b"<head><link rel='stylesheet' href='http://foo.bar/baz'/><a href='baz.html'>strange link in header</a><base href='http://foo.bar'></head><body><img src='img.jpg'></body>",
    );
}

#[test]
fn annoying_wiki() {
    validate_expected(
        "wiki",
        b"<head><base href='http://en.wikipedia.org/wiki/Labrador_Retriever'/></head><body><img src='/wiki/img.jpg'><a href='/wiki/File:puppy.jpg'>dog</a></body>",
        b"<head><base href='http://en.wikipedia.org/wiki/Labrador_Retriever'/></head><body><img src='img.jpg'><a href='/wiki/File:puppy.jpg'>dog</a></body>",
    );
}

#[test]
fn blank_base() {
    let input = b"<head><base href=''></head><body><a href='http://www.google.com/'>foo</a></body>";
    validate_expected("wiki", input, input);
}

#[test]
fn relative_base() {
    validate_expected(
        "wiki",
        b"<head><base href='/directory/'></head><body><img src='/directory/img.jpg'></body>",
        b"<head><base href='/directory/'></head><body><img src='img.jpg'></body>",
    );
}

#[test]
fn undecodable_base_poison_gate() {
    // Hand test for `other_base_problem`: the multibyte `&lang;` fails to
    // decode, so nothing trims anywhere — not even the same-origin link
    // that would otherwise shorten against the document base.
    let input = b"<head><base href='http://foo.bar/&lang;'/></head><body><a href='http://test.com/undecodable.html'>self</a><img src='http://foo.bar/img.jpg'></body>";
    validate_expected("undecodable", input, input);
}

#[test]
fn query_with_encoded_ampersand() {
    validate_expected(
        "ampersand",
        &image("http://test.com/discuss/a.php?&action=vtopic&amp;forum=2"),
        &image("discuss/a.php?&amp;action=vtopic&amp;forum=2"),
    );
    validate_expected(
        "improperly_terminated_multi_byte_escape",
        &image("http://test.com/?q=klaatu+nikto+barada&lang=fr"),
        &image("http://test.com/?q=klaatu+nikto+barada&lang=fr"),
    );
}

#[test]
fn publishes_trim_counters() {
    let stats = Statistics::new();
    let mut filter = UrlLeftTrimFilter::new(Vec::new());
    filter.init_stats(&stats);
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/stats.html", false);
    parser.write(
        b"<head><base href='http://foo.bar/baz/'/></head><body><img src='http://foo.bar/baz/nav.jpg'/></body>",
    );
    parser.finish();
    assert_eq!(stats.get_variable(URL_TRIMS).unwrap().get(), 1);
    // "http://foo.bar/baz/nav.jpg" (26) trims to "nav.jpg" (7).
    assert_eq!(stats.get_variable(URL_TRIM_SAVED_BYTES).unwrap().get(), 19);
}
