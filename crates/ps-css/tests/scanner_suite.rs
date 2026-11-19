//! Ported CSS scanner suite.
//!
//! Mechanically translated from `css_tag_scanner_test.cc` (see
//! `tool/translate_scanner_tests.py`); do not hand-edit expectations.
//! A prefix-mapping test double stands in for the domain machinery
//! (relative URLs gain the old-base prefix, exactly as the goldens
//! show); the scanner under test is production code.

use ps_css::scanner::{
    has_import, has_url, is_alternate_stylesheet, is_stylesheet_or_alternate, transform_urls,
    Scanner, TransformStatus, Transformer,
};

const OLD_BASE: &[u8] = b"http://old-base.com/";

/// Test double matching the golden mapping: relative URLs gain the
/// old-base prefix (with `"` percent-encoded, as GoogleUrl does);
/// absolute, fragment, and data URLs pass through. Full URL
/// canonicalization belongs to the URL layer; this double covers
/// exactly the mappings the goldens pin.
struct PrefixTransformer;

impl Transformer for PrefixTransformer {
    fn transform(&mut self, url: &mut Vec<u8>) -> TransformStatus {
        if url.contains(&b':') || url.starts_with(b"#") {
            return TransformStatus::NoChange;
        }
        let mut mapped = OLD_BASE.to_vec();
        let stripped = url.strip_prefix(b"/").unwrap_or(url);
        for byte in stripped {
            if *byte == b'"' {
                mapped.extend_from_slice(b"%22");
            } else {
                mapped.push(*byte);
            }
        }
        if mapped == *url {
            TransformStatus::NoChange
        } else {
            *url = mapped;
            TransformStatus::Success
        }
    }
}

fn transform(input: &[u8]) -> Vec<u8> {
    let mut transformer = PrefixTransformer;
    transform_urls(input, &mut transformer).expect("transform failed")
}

fn transform_streaming(pieces: &[&[u8]]) -> Vec<u8> {
    let mut transformer = PrefixTransformer;
    let mut scanner = Scanner::new();
    let mut result = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        let last = index + 1 == pieces.len();
        let mut out = Vec::new();
        assert!(scanner.transform_streaming(piece, last, &mut transformer, &mut out));
        result.extend_from_slice(b"portion=");
        result.extend_from_slice(&out);
        result.extend_from_slice(b", retain=");
        result.extend_from_slice(scanner.retained_for_reparse());
        result.extend_from_slice(b"|");
    }
    result
}

struct FailTransformer;

impl Transformer for FailTransformer {
    fn transform(&mut self, _url: &mut Vec<u8>) -> TransformStatus {
        TransformStatus::Failure
    }
}

#[test]
fn transform_urls_fails() {
    let mut transformer = FailTransformer;
    assert!(transform_urls(b"url(foo)", &mut transformer).is_none());
}

#[test]
fn streaming_char_by_char() {
    // Hand-written: feeds the input one byte at a time.
    let input = b"@import \"other.css\"; ul { list-style-image:url(a.png); }";
    let mut transformer = PrefixTransformer;
    let mut scanner = Scanner::new();
    let mut result = Vec::new();
    for (index, byte) in input.iter().enumerate() {
        let last = index + 1 == input.len();
        let mut out = Vec::new();
        assert!(scanner.transform_streaming(&[*byte], last, &mut transformer, &mut out));
        result.extend_from_slice(&out);
        result.push(b'|');
    }
    assert_eq!(
        result.as_slice(),
        b"||||||||||||||||||@import \"http://old-base.com/other.css\"|;| ||||ul {| |l|i|s|t|-|s|t|y|l|e|-|i|m|a|g|e|:||||||||||url(http://old-base.com/a.png)|;| |}|" as &[u8]);
}

#[test]
fn unquoted_url_long_whitespace_is_linear() {
    // Hand-written: 1MB of trailing whitespace must stay linear.
    let mut input = b"a{background:url(x".to_vec();
    input.extend(std::iter::repeat_n(b' ', 1 << 20));
    let start = std::time::Instant::now();
    let out = transform(&input);
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(out.as_slice(), b"a{background:url(http://old-base.com/x");
}

#[test]
fn helper_predicates() {
    assert!(has_import(b"a @import b"));
    assert!(has_import(b"a @IMPORT b"));
    assert!(!has_import(b"a @media b"));
    assert!(has_url(b"a url(b"));
    assert!(!has_url(b"a URL(b"));
    assert!(is_stylesheet_or_alternate(b"stylesheet"));
    assert!(is_stylesheet_or_alternate(b"alternate stylesheet"));
    assert!(!is_stylesheet_or_alternate(b"alternate"));
    assert!(is_alternate_stylesheet(b"alternate stylesheet"));
    assert!(!is_alternate_stylesheet(b"stylesheet"));
    assert!(!is_alternate_stylesheet(b""));
}

#[test]
fn empty() {
    assert_eq!(transform(b"").as_slice(), b"" as &[u8]);
}

#[test]
fn no_match() {
    assert_eq!(transform(b"hello").as_slice(), b"hello" as &[u8]);
}

#[test]
fn absolute() {
    let css_with_abs_path = b"a url(http://other_base/image.png) b";
    assert_eq!(
        transform(css_with_abs_path).as_slice(),
        css_with_abs_path as &[u8]
    );
}

#[test]
fn absolute_s_quote() {
    let css_with_abs_path = b"a url('http://other_base/image.png') b";
    assert_eq!(
        transform(css_with_abs_path).as_slice(),
        css_with_abs_path as &[u8]
    );
}

#[test]
fn absolute_d_quote() {
    let css_with_abs_path = b"a url(\"http://other_base/image.png\") b";
    assert_eq!(
        transform(css_with_abs_path).as_slice(),
        css_with_abs_path as &[u8]
    );
}

#[test]
fn unquoted_url_trailing_whitespace() {
    assert_eq!(
        transform(b"a{background:url(x   ").as_slice(),
        b"a{background:url(http://old-base.com/x" as &[u8]
    );
}

#[test]
fn relative() {
    assert_eq!(
        transform(b"a url(subdir/image.png) b").as_slice(),
        b"a url(http://old-base.com/subdir/image.png) b" as &[u8]
    );
}

#[test]
fn relative_s_quote() {
    assert_eq!(
        transform(b"a url('subdir/image.png') b").as_slice(),
        b"a url('http://old-base.com/subdir/image.png') b" as &[u8]
    );
}

#[test]
fn escape_s_quote() {
    assert_eq!(
        transform(b"a url('subdir/imag\\'e.png') b").as_slice(),
        b"a url('http://old-base.com/subdir/imag\\'e.png') b" as &[u8]
    );
}

#[test]
fn relative_s_quote_spaced() {
    assert_eq!(
        transform(b"a url( 'subdir/image.png' ) b").as_slice(),
        b"a url('http://old-base.com/subdir/image.png') b" as &[u8]
    );
}

#[test]
fn relative_d_quote() {
    assert_eq!(
        transform(b"a url(\"subdir/image.png\") b").as_slice(),
        b"a url(\"http://old-base.com/subdir/image.png\") b" as &[u8]
    );
}

#[test]
fn escape_d_quote() {
    assert_eq!(
        transform(b"a url(\"subdir/\\\"image.png\") b").as_slice(),
        b"a url(\"http://old-base.com/subdir/%22image.png\") b" as &[u8]
    );
}

#[test]
fn two_relative1_abs() {
    let input = b"a url(s/1.png) b url(2.png) c url(http://a/3.png) d";
    let expected = b"a url(http://old-base.com/s/1.png) b url(http://old-base.com/2.png) c url(http://a/3.png) d";
    assert_eq!(transform(input).as_slice(), expected as &[u8]);
}

#[test]
fn string_line_cont() {
    let k_expected = b"url('http://old-base.com/foobar') stuff";
    assert_eq!(
        transform(b"url('foo\\\nbar') stuff").as_slice(),
        k_expected as &[u8]
    );
    assert_eq!(
        transform(b"url('foo\\\rbar') stuff").as_slice(),
        k_expected as &[u8]
    );
    assert_eq!(
        transform(b"url('foo\\\r\nbar') stuff").as_slice(),
        k_expected as &[u8]
    );
    assert_eq!(
        transform(b"url('foo\\\x0cbar') stuff").as_slice(),
        k_expected as &[u8]
    );
}

#[test]
fn cont_and_unterminated() {
    assert_eq!(
        transform(b"@import \"foo\nbar\\\nbaz").as_slice(),
        b"@import \"http://old-base.com/foo\nbar\\\nbaz" as &[u8]
    );
}

#[test]
fn string_unterminated() {
    assert_eq!(
        transform(b"@import 'foo\n\"bar stuff").as_slice(),
        b"@import 'http://old-base.com/foo\n\"bar stuff" as &[u8]
    );
    assert_eq!(
        transform(b"@import 'foo\x0c\"bar stuff").as_slice(),
        b"@import 'http://old-base.com/foo\x0c\"bar stuff" as &[u8]
    );
}

#[test]
fn string_multine_terminated() {
    assert_eq!(
        transform(b"@import 'foo\nbar' stuff").as_slice(),
        b"@import 'http://old-base.com/foo\nbar' stuff" as &[u8]
    );
}

#[test]
fn url_proper_close() {
    assert_eq!(
        transform(b"url('foo).bar')").as_slice(),
        b"url('http://old-base.com/foo\\).bar')" as &[u8]
    );
}

#[test]
fn url_unquoted() {
    assert_eq!(
        transform(b"url(/foo bar)").as_slice(),
        b"url(http://old-base.com/foo bar)" as &[u8]
    );
}

#[test]
fn lots_of_whitespace() {
    assert_eq!(
        transform(b"url(/foo \t  \x0c     )").as_slice(),
        b"url(http://old-base.com/foo)" as &[u8]
    );
    assert_eq!(
        transform(b"url(  \r\n  /foo \t  \x0c     )").as_slice(),
        b"url(http://old-base.com/foo)" as &[u8]
    );
}

#[test]
fn dont_unescape_too_much() {
    assert_eq!(
        transform(b"url(/\\)stuff)").as_slice(),
        b"url(http://old-base.com/\\)stuff)" as &[u8]
    );
    assert_eq!(
        transform(b"url(\"/\\\"stuff\")").as_slice(),
        b"url(\"http://old-base.com/%22stuff\")" as &[u8]
    );
}

#[test]
fn import_url() {
    assert_eq!(
        transform(b"a @import url(style.css) div { display: block; }").as_slice(),
        b"a @import url(http://old-base.com/style.css) div { display: block; }" as &[u8]
    );
}

#[test]
fn import_url_quote() {
    assert_eq!(
        transform(b"a @import url('style.css') div { display: block; }").as_slice(),
        b"a @import url('http://old-base.com/style.css') div { display: block; }" as &[u8]
    );
}

#[test]
fn import_url_quote_no_close_paren() {
    let k_input = b"a @import url('style.css' div { display: block; }";
    assert_eq!(transform(k_input).as_slice(), k_input as &[u8]);
}

#[test]
fn import_s_quote() {
    assert_eq!(
        transform(b"a @import 'style.css' div { display: block; }").as_slice(),
        b"a @import 'http://old-base.com/style.css' div { display: block; }" as &[u8]
    );
}

#[test]
fn import_d_quote() {
    assert_eq!(
        transform(b"a @import \t \"style.css\" div { display: block; }").as_slice(),
        b"a @import \"http://old-base.com/style.css\" div { display: block; }" as &[u8]
    );
}

#[test]
fn import_s_quote_d_quote() {
    assert_eq!(
        transform(b"a @import 'style.css'\"screen\";").as_slice(),
        b"a @import 'http://old-base.com/style.css'\"screen\";" as &[u8]
    );
}

#[test]
fn broken_escape() {
    assert_eq!(
        transform(b"@import 'foo/\\1234'; url(foo\\").as_slice(),
        b"@import 'foo/\\1234'; url(foo\\" as &[u8]
    );
}

#[test]
fn url_inside_string_is_text() {
    let k_two_strings = b"p{content:\"url(a\"} q r{content:\"}s{}\"}";
    let k_escaped_quote = b"p{content:'\\'url(a'}";
    let k_line_continuation = b"p{content:'\\\r\nurl(a'}";
    let k_escaped_quote_in_name = b"p\\'q{content:'url(a'}";
    assert_eq!(transform(k_two_strings).as_slice(), k_two_strings as &[u8]);
    assert_eq!(
        transform(k_escaped_quote).as_slice(),
        k_escaped_quote as &[u8]
    );
    assert_eq!(
        transform(k_line_continuation).as_slice(),
        k_line_continuation as &[u8]
    );
    assert_eq!(
        transform(k_escaped_quote_in_name).as_slice(),
        k_escaped_quote_in_name as &[u8]
    );
    assert_eq!(
        transform(b"p{content:'url(a'}q{b:url(a)}").as_slice(),
        b"p{content:'url(a'}q{b:url(http://old-base.com/a)}" as &[u8]
    );
    assert_eq!(
        transform(b"p{content:'a\n}q{b:url(a)}").as_slice(),
        b"p{content:'a\n}q{b:url(http://old-base.com/a)}" as &[u8]
    );
}

#[test]
fn import_inside_string_is_text() {
    let k_import_in_string = b"p{content:\"@import\"}q{content:\"}s{}\"}";
    assert_eq!(
        transform(k_import_in_string).as_slice(),
        k_import_in_string as &[u8]
    );
}

#[test]
fn custom_property_string_is_text() {
    let k_custom_properties = b":root{--a:\"url(b\"} p q{--c:\"}s{}\"}";
    assert_eq!(
        transform(k_custom_properties).as_slice(),
        k_custom_properties as &[u8]
    );
}

#[test]
fn url_inside_comment_is_text() {
    assert_eq!(
        transform(b"/* it's url(a) */url(a)/*/ url(a").as_slice(),
        b"/* it's url(a) */url(http://old-base.com/a)/*/ url(a" as &[u8]
    );
    assert_eq!(
        transform(b"/*! \"url(a) */p{b:url(a)}").as_slice(),
        b"/*! \"url(a) */p{b:url(http://old-base.com/a)}" as &[u8]
    );
}

#[test]
fn undecodable_string_is_stepped_over() {
    assert_eq!(
        transform(b"url('a\\62 c') url(x) 'url(x'").as_slice(),
        b"url('a\\62 c') url(http://old-base.com/x) 'url(x'" as &[u8]
    );
    assert_eq!(
        transform(b"url(\"a\\62 )}p{}\") q{b:url(x)}").as_slice(),
        b"url(\"a\\62 )}p{}\") q{b:url(http://old-base.com/x)}" as &[u8]
    );
    assert_eq!(
        transform(b"@import 'a\\62 c'; url(x) 'url(x'").as_slice(),
        b"@import 'a\\62 c'; url(http://old-base.com/x) 'url(x'" as &[u8]
    );
}

#[test]
fn streaming_string_and_comment() {
    let out = transform_streaming(&[b"p{content:'url(", b"a'}", b"q{b:url(a)}"]);
    assert_eq!(out.as_slice(), b"portion=p{content:, retain='url(|portion='url(a'}, retain=|portion=q{b:url(http://old-base.com/a)}, retain=|" as &[u8]);
    let out = transform_streaming(&[b"a/", b"* url(x", b") */b{c:url(x)}"]);
    assert_eq!(out.as_slice(), b"portion=a, retain=/|portion=, retain=/* url(x|portion=/* url(x) */b{c:url(http://old-base.com/x)}, retain=|" as &[u8]);
}

#[test]
fn streaming_url_interrupt() {
    let out = transform_streaming(&[b"u", b"rl(", b"\"foo", b".png\"", b") bar u", b"x"]);
    assert_eq!(out.as_slice(), b"portion=, retain=u|portion=, retain=url(|portion=, retain=url(\"foo|portion=, retain=url(\"foo.png\"|portion=url(\"http://old-base.com/foo.png\") bar , retain=u|portion=ux, retain=|" as &[u8]);
}

#[test]
fn streaming_other_at_rule() {
    let out = transform_streaming(&[b"@export", b" \"foo.png\";"]);
    assert_eq!(
        out.as_slice(),
        b"portion=@export, retain=|portion= \"foo.png\";, retain=|" as &[u8]
    );
}

#[test]
fn streaming_url_arg_interrupt() {
    let out = transform_streaming(&[b"background-image:url(", b"foo.png", b")"]);
    assert_eq!(out.as_slice(), b"portion=background-image:, retain=url(|portion=, retain=url(foo.png|portion=url(http://old-base.com/foo.png), retain=|" as &[u8]);
}

#[test]
fn streaming_import_interrupt() {
    let out = transform_streaming(&[b"@", b"imp", b"ort", b" ", b" \"foo.css", b"\";"]);
    assert_eq!(out.as_slice(), b"portion=, retain=@|portion=, retain=@imp|portion=, retain=@import|portion=, retain=@import |portion=, retain=@import  \"foo.css|portion=@import \"http://old-base.com/foo.css\";, retain=|" as &[u8]);
}

#[test]
fn streaming_escape() {
    let out = transform_streaming(&[b"background-image: url(\"foo\\", b"\"bar\")"]);
    assert_eq!(out.as_slice(), b"portion=background-image: , retain=url(\"foo\\|portion=url(\"http://old-base.com/foo%22bar\"), retain=|" as &[u8]);
}
