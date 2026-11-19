//! Hand-written C++ minify tests (flow-heavy cases).
//!
//! Companions to the mechanically translated `minify_corpus.rs`: these seven
//! tests need loops, `StrCat`, source-map goldens, or the collapse API, so
//! the control flow is hand-written while every string/golden below was
//! extracted mechanically from `js_minify_test.cc` (see
//! `translate_minify_tests.py --consts`).

use ps_js::keywords::{can_keyword_precede_regex, keyword_names};
use ps_js::minify::{
    legacy_minified_collapsed_size, legacy_minified_size, legacy_minify_js,
    legacy_minify_js_collapse_strings, minify_utf8, minify_utf8_with_source_map,
};
use ps_js::sourcemap::Mapping;
use ps_js::tokenizer::TokenizerPatterns;

fn patterns() -> TokenizerPatterns {
    TokenizerPatterns::new()
}

fn check_both(input: &[u8], expected: &[u8]) {
    let patterns = patterns();
    let (out, ok) = minify_utf8(&patterns, input);
    assert!(ok, "new minifier failed on {input:?}");
    assert_eq!(out, expected, "new minifier on {input:?}");
    let out = legacy_minify_js(input).expect("old minifier failed");
    assert_eq!(out, expected, "old minifier on {input:?}");
    assert_eq!(legacy_minified_size(input), Some(expected.len()));
}

fn check_new(input: &[u8], expected: &[u8]) {
    let patterns = patterns();
    let (out, ok) = minify_utf8(&patterns, input);
    assert!(ok, "new minifier failed on {input:?}");
    assert_eq!(out, expected, "new minifier on {input:?}");
}

fn mappings_to_string(mappings: &[Mapping]) -> String {
    let mut out = String::from("{");
    for mapping in mappings {
        out.push_str(&format!(
            "({}, {}, {}, {}, {}), ",
            mapping.gen_line, mapping.gen_col, mapping.src_file, mapping.src_line, mapping.src_col
        ));
    }
    out.push('}');
    out
}

#[test]
fn do_not_create_html_end_tag() {
    // "<" followed by a regular expression keeps its separator so the
    // output never gains an HTML end tag such as "</textarea".
    let names = [
        "script", "noscript", "noembed", "noframes", "textarea", "title", "style", "iframe", "xmp",
        "TEXTAREA", "NoScript", "Style", "XMP",
    ];
    for name in names {
        let input = format!("var ok = 1 < /* note */ /{name}/ > 0;");
        let expected = format!("var ok=1< /{name}/>0;");
        check_new(input.as_bytes(), expected.as_bytes());
        let input = format!("var ok = 1 < /{name}/.test(s);");
        let expected = format!("var ok=1< /{name}/.test(s);");
        check_new(input.as_bytes(), expected.as_bytes());
        let input = format!("var ok = 1 <\n/{name}/ > 0;");
        let expected = format!("var ok=1<\n/{name}/>0;");
        check_new(input.as_bytes(), expected.as_bytes());
        let input = format!("x = y << /* c */ /{name}/g.lastIndex;");
        let expected = format!("x=y<< /{name}/g.lastIndex;");
        check_new(input.as_bytes(), expected.as_bytes());
    }
    // Text that already has the pair keeps it as it is.
    check_new(b"var ok = 1</textarea/ > 0;", b"var ok=1</textarea/>0;");
    // Nor at the very end of the input, where minification may stop and
    // keep the rest of the input as it is.
    let patterns = patterns();
    for input in [
        "x = 1 <",
        "x = 1 <  ",
        "x = 1 < /",
        "x = 1 < /textarea",
        "x = 1 < /* c */ /textarea",
        "x = 1 <\n/TITLE",
    ] {
        let (output, _) = minify_utf8(&patterns, input.as_bytes());
        assert!(
            !output.windows(2).any(|pair| pair == b"</"),
            "gained end tag: {output:?}"
        );
    }
    check_new(b"x = 1 <", b"x=1<");
    // Other tokens after "<" still join.
    check_new(b"if (a < b) { c = a <= b; }", b"if(a<b){c=a<=b;}");
}

#[test]
fn keyword_precedes_regex() {
    // Every keyword that may precede a regex must lex "/./" as a regex
    // (not division) so the trailing comment is still removed.
    for name in keyword_names() {
        if can_keyword_precede_regex(name) {
            let input = format!("{name} /./   /* hi there */;");
            let expected = format!("{name}/./;");
            check_both(input.as_bytes(), expected.as_bytes());
        }
    }
}

#[test]
fn do_not_crash() {
    // All prefixes of the crash string through the legacy minifier,
    // including the NUL-terminated full length (`sizeof` counts the NUL).
    let mut full = CRASH_TEST_STRING.to_vec();
    full.push(0);
    for i in 0..=full.len() {
        let _ = legacy_minify_js(&full[..i]);
    }
}

#[test]
fn collapsing_string_test() {
    let out = legacy_minify_js_collapse_strings(COLLAPSING_STRING).expect("collapse failed");
    assert_eq!(out.len(), COLLAPSED_STRING.len());
    assert_eq!(out, COLLAPSED_STRING);
    assert_eq!(
        legacy_minified_collapsed_size(COLLAPSING_STRING),
        Some(COLLAPSED_STRING.len())
    );
}

#[test]
fn source_maps_simple() {
    let patterns = patterns();
    let mut mappings = Vec::new();
    let (out, ok) = minify_utf8_with_source_map(&patterns, MAP_SIMPLE_BEFORE, Some(&mut mappings));
    assert!(ok);
    assert_eq!(out, MAP_SIMPLE_AFTER);
    assert_eq!(
        mappings_to_string(&mappings).as_bytes(),
        MAP_SIMPLE_EXPECTED
    );
}

#[test]
fn source_maps_complex() {
    let patterns = patterns();
    let mut mappings = Vec::new();
    let (out, ok) = minify_utf8_with_source_map(&patterns, BEFORE_COMPILATION, Some(&mut mappings));
    assert!(ok);
    assert_eq!(out, AFTER_COMPILATION_NEW);
    assert_eq!(
        mappings_to_string(&mappings).as_bytes(),
        MAP_COMPLEX_EXPECTED
    );
}

#[test]
fn signed_char_doesnt_sign_extend() {
    // The C++ input is `{0xe0, 0xb2, 0xa0, 0x00}` read as a
    // NUL-terminated StringPiece, i.e. just U+0CA0 (an identifier).
    check_both(b"\xe0\xb2\xa0", b"\xe0\xb2\xa0");
}

// Mechanically extracted consts (see translate_minify_tests.py).
const BEFORE_COMPILATION: &[u8] = b"// is.js\n\n// (c) 2001 Douglas Crockford\n// 2001 June 3\n\n\n// is\n\n// The -is- object is used to identify the browser.  Every browser edition\n// identifies itself, but there is no standard way of doing it, and some of\n// the identification is deceptive. This is because the authors of web\n// browsers are liars. For example, Microsoft's IE browsers claim to be\n// Mozilla 4. Netscape 6 claims to be version 5.\n\nvar is = {\n    ie:      navigator.appName == 'Microsoft Internet Explorer',\n    java:    navigator.javaEnabled(),\n    ns:      navigator.appName == 'Netscape',\n    ua:      navigator.userAgent.toLowerCase(),\n    version: parseFloat(navigator.appVersion.substr(21)) ||\n             parseFloat(navigator.appVersion),\n    win:     navigator.platform == 'Win32'\n}\nis.mac = is.ua.indexOf('mac') >= 0;\nif (is.ua.indexOf('opera') >= 0) {\n    is.ie = is.ns = false;\n    is.opera = true;\n}\nif (is.ua.indexOf('gecko') >= 0) {\n    is.ie = is.ns = false;\n    is.gecko = true;\n}\n";
const AFTER_COMPILATION_NEW: &[u8] = b"var is={ie:navigator.appName=='Microsoft Internet Explorer',java:navigator.javaEnabled(),ns:navigator.appName=='Netscape',ua:navigator.userAgent.toLowerCase(),version:parseFloat(navigator.appVersion.substr(21))||parseFloat(navigator.appVersion),win:navigator.platform=='Win32'}\nis.mac=is.ua.indexOf('mac')>=0;if(is.ua.indexOf('opera')>=0){is.ie=is.ns=false;is.opera=true;}if(is.ua.indexOf('gecko')>=0){is.ie=is.ns=false;is.gecko=true;}";
const CRASH_TEST_STRING: &[u8] =
    b"var x = 'asd \\' lse'\nvar y /*comment*/ = /regex/\nvar z = \"x =\" + x\n";
const COLLAPSING_STRING: &[u8] =
    b"var x = 'asd \\' lse'\nvar y /*comment*/ = /re'gex/\nvar z = \"x =\" + x\n";
const COLLAPSED_STRING: &[u8] = b"var x=''\nvar y=/re'gex/\nvar z=\"\"+x";
const MAP_SIMPLE_BEFORE: &[u8] = b"/* Simple hello world program. */\nalert( 'Hello, World!' );\n";
const MAP_SIMPLE_AFTER: &[u8] = b"alert('Hello, World!');";
const MAP_SIMPLE_EXPECTED: &[u8] = b"{(0, 0, 0, 1, 0), (0, 6, 0, 1, 7), (0, 21, 0, 1, 23), }";
const MAP_COMPLEX_EXPECTED: &[u8] = b"{(0, 0, 0, 14, 0), (0, 6, 0, 14, 7), (0, 7, 0, 14, 9), (0, 8, 0, 15, 4), (0, 11, 0, 15, 13), (0, 28, 0, 15, 31), (0, 30, 0, 15, 34), (0, 60, 0, 16, 4), (0, 65, 0, 16, 13), (0, 89, 0, 17, 4), (0, 92, 0, 17, 13), (0, 109, 0, 17, 31), (0, 111, 0, 17, 34), (0, 122, 0, 18, 4), (0, 125, 0, 18, 13), (0, 159, 0, 19, 4), (0, 167, 0, 19, 13), (0, 210, 0, 19, 57), (0, 212, 0, 20, 13), (0, 245, 0, 21, 4), (0, 249, 0, 21, 13), (0, 267, 0, 21, 32), (0, 269, 0, 21, 35), (0, 276, 0, 22, 0), (1, 0, 0, 23, 0), (1, 6, 0, 23, 7), (1, 7, 0, 23, 9), (1, 27, 0, 23, 30), (1, 29, 0, 23, 33), (1, 31, 0, 24, 0), (1, 33, 0, 24, 3), (1, 56, 0, 24, 27), (1, 58, 0, 24, 30), (1, 60, 0, 24, 33), (1, 61, 0, 25, 4), (1, 66, 0, 25, 10), (1, 67, 0, 25, 12), (1, 72, 0, 25, 18), (1, 73, 0, 25, 20), (1, 79, 0, 26, 4), (1, 87, 0, 26, 13), (1, 88, 0, 26, 15), (1, 93, 0, 27, 0), (1, 94, 0, 28, 0), (1, 96, 0, 28, 3), (1, 119, 0, 28, 27), (1, 121, 0, 28, 30), (1, 123, 0, 28, 33), (1, 124, 0, 29, 4), (1, 129, 0, 29, 10), (1, 130, 0, 29, 12), (1, 135, 0, 29, 18), (1, 136, 0, 29, 20), (1, 142, 0, 30, 4), (1, 150, 0, 30, 13), (1, 151, 0, 30, 15), (1, 156, 0, 31, 0), }";
