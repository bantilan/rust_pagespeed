//! Ported C++ minify suite.
//!
//! Control flow is hand-written; every input/expected string below
//! was extracted mechanically from `css_minify_test.cc`. Any
//! divergence from the C++ baseline is a port bug.
//! (`OutOfRangeNumericValueSaturates` lives as `integer_saturation`
//! in `value.rs`.)

use ps_css::ast::NO_ERROR;
use ps_css::minify::Minifier;
use ps_css::parser::Parser;

/// Parses with default modes, expands, and minifies (the
/// `Css::Parser` + `ParseStylesheet` + `CssMinify::Stylesheet` shape).
fn minify_expanded(input: &[u8]) -> (Vec<u8>, bool) {
    let mut parser = Parser::new(input);
    let stylesheet = parser.parse_stylesheet();
    let mut minifier = Minifier::new(false);
    minifier.minify_stylesheet(&stylesheet);
    minifier.finish()
}

/// Parses preservation/no-quirks and minifies, serving the original
/// on error (`RewriteCss`).
fn rewrite_css(input: &[u8]) -> Vec<u8> {
    let mut parser = Parser::new(input);
    parser.set_preservation_mode(true);
    parser.set_quirks_mode(false);
    let stylesheet = parser.parse_raw_stylesheet();
    if parser.errors_seen_mask() != NO_ERROR {
        return input.to_vec();
    }
    let mut minifier = Minifier::new(false);
    minifier.minify_stylesheet(&stylesheet);
    minifier.finish().0
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn rewrite_css_incomplete_unicode() {
    // The C++ input is NUL-terminated, dropping the trailing 0 byte;
    // the error makes RewriteCss serve the input back unchanged.
    let input =
        &REWRITE_CSS_INCOMPLETE_UNICODE_INPUT[..REWRITE_CSS_INCOMPLETE_UNICODE_INPUT.len() - 1];
    assert_eq!(rewrite_css(input), input);
}

#[test]
fn minify_stylesheeet_collecting_urls() {
    let mut minifier = Minifier::new(true);
    assert!(minifier.parse_stylesheet(MINIFY_STYLESHEEET_COLLECTING_URLS_INPUT));
    let urls = minifier.urls().expect("collector").to_vec();
    assert_eq!(urls, vec![b"foo.png".to_vec()]);
    assert_eq!(
        minifier.finish().0,
        MINIFY_STYLESHEEET_COLLECTING_URLS_EXPECTED
    );
}

#[test]
fn minify_import_stylesheeet_collecting_urls() {
    let mut minifier = Minifier::new(true);
    assert!(minifier.parse_stylesheet(MINIFY_IMPORT_STYLESHEEET_COLLECTING_URLS_INPUT));
    let urls = minifier.urls().expect("collector").to_vec();
    assert_eq!(urls, vec![b"foo.png".to_vec()]);
    assert_eq!(
        minifier.finish().0,
        MINIFY_IMPORT_STYLESHEEET_COLLECTING_URLS_EXPECTED
    );
}

#[test]
fn minify_invalid() {
    let mut minifier = Minifier::new(false);
    assert!(!minifier.parse_stylesheet(MINIFY_INVALID_INPUT));
}

#[test]
fn do_not_fix_bad_colors_or_units() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(DO_NOT_FIX_BAD_COLORS_OR_UNITS_INPUT));
    assert_eq!(minifier.finish().0, DO_NOT_FIX_BAD_COLORS_OR_UNITS_EXPECTED);
}

#[test]
fn remove_zero_length_but_not_time_or_percent_suffix() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(REMOVE_ZERO_LENGTH_BUT_NOT_TIME_OR_PERCENT_SUFFIX_INPUT));
    assert_eq!(
        minifier.finish().0,
        REMOVE_ZERO_LENGTH_BUT_NOT_TIME_OR_PERCENT_SUFFIX_EXPECTED
    );
}

#[test]
fn parsing_and_minifying_background_and_font() {
    let (out, ok) = minify_expanded(PARSING_AND_MINIFYING_BACKGROUND_AND_FONT_INPUT);
    assert!(ok);
    assert_eq!(out, PARSING_AND_MINIFYING_BACKGROUND_AND_FONT_EXPECTED);
}

#[test]
fn parsing_and_minifing_viewport_units() {
    let (out, ok) = minify_expanded(PARSING_AND_MINIFING_VIEWPORT_UNITS_INPUT);
    assert!(ok);
    assert_eq!(out, PARSING_AND_MINIFING_VIEWPORT_UNITS_EXPECTED);
}

#[test]
fn stray_single_quote1() {
    let (out, ok) = minify_expanded(STRAY_SINGLE_QUOTE1_INPUT);
    assert!(ok);
    assert_eq!(out, STRAY_SINGLE_QUOTE1_EXPECTED);
}

#[test]
fn stray_single_quote2() {
    let mut minifier = Minifier::new(true);
    assert!(minifier.parse_stylesheet(STRAY_SINGLE_QUOTE2_INPUT));
    assert_eq!(minifier.finish().0, STRAY_SINGLE_QUOTE2_EXPECTED);
}

#[test]
fn stray_single_quote3() {
    let (out, ok) = minify_expanded(STRAY_SINGLE_QUOTE3_INPUT);
    assert!(ok);
    assert_eq!(out, STRAY_SINGLE_QUOTE3_EXPECTED);
}

#[test]
fn calc_function_with_zero_value_and_unit() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(CALC_FUNCTION_WITH_ZERO_VALUE_AND_UNIT_INPUT));
    let (out, _) = minifier.finish();
    assert!(
        contains(&out, CALC_FUNCTION_WITH_ZERO_VALUE_AND_UNIT_SUBSTR),
        "missing substring in {out:?}"
    );
}

#[test]
fn css_unicode_range_descriptor() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(CSS_UNICODE_RANGE_DESCRIPTOR_INPUT));
    let (out, _) = minifier.finish();
    assert!(
        contains(&out, CSS_UNICODE_RANGE_DESCRIPTOR_SUBSTR),
        "missing substring in {out:?}"
    );
}

#[test]
fn class_name_leading_digit_keeps_escape() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(CLASS_NAME_LEADING_DIGIT_KEEPS_ESCAPE_INPUT));
    assert_eq!(
        minifier.finish().0,
        CLASS_NAME_LEADING_DIGIT_KEEPS_ESCAPE_EXPECTED
    );
}

#[test]
fn class_name_unescaped_leading_digit_kept_verbatim() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(CLASS_NAME_UNESCAPED_LEADING_DIGIT_KEPT_VERBATIM_INPUT));
    assert_eq!(
        minifier.finish().0,
        CLASS_NAME_UNESCAPED_LEADING_DIGIT_KEPT_VERBATIM_EXPECTED
    );
}

#[test]
fn class_name_unescaped_paren_kept_verbatim() {
    let mut minifier = Minifier::new(false);
    assert!(minifier.parse_stylesheet(CLASS_NAME_UNESCAPED_PAREN_KEPT_VERBATIM_INPUT));
    assert_eq!(
        minifier.finish().0,
        CLASS_NAME_UNESCAPED_PAREN_KEPT_VERBATIM_EXPECTED
    );
}
const REWRITE_CSS_INCOMPLETE_UNICODE_INPUT: &[u8] = b"@media (jax-width: X\xc8\xc2\x8f\x87) \x00";
const MINIFY_STYLESHEEET_COLLECTING_URLS_INPUT: &[u8] =
    b".a {\n  background-color: darkgreen;\n  background-image: url(foo.png);\n}";
const MINIFY_STYLESHEEET_COLLECTING_URLS_EXPECTED: &[u8] =
    b".a{background-color:#006400;background-image:url(foo.png)}";
// MinifyStylesheeetCollectingUrls url[0] = b"foo.png"
const MINIFY_IMPORT_STYLESHEEET_COLLECTING_URLS_INPUT: &[u8] = b"@import   'foo.png';";
const MINIFY_IMPORT_STYLESHEEET_COLLECTING_URLS_EXPECTED: &[u8] = b"@import url(foo.png);";
// MinifyImportStylesheeetCollectingUrls url[0] = b"foo.png"
const MINIFY_INVALID_INPUT: &[u8] = b"{";
const DO_NOT_FIX_BAD_COLORS_OR_UNITS_INPUT: &[u8] = b".a {\n  width: 10;\n  height: 20px;;\n  background-color: 0f0f0f;\n  foreground-color: #f0f0f0;\n}";
const DO_NOT_FIX_BAD_COLORS_OR_UNITS_EXPECTED: &[u8] =
    b".a{width:10;height:20px;background-color: 0f0f0f;foreground-color:#f0f0f0}";
const REMOVE_ZERO_LENGTH_BUT_NOT_TIME_OR_PERCENT_SUFFIX_INPUT: &[u8] =
    b".a {\n  width: 0px;\n  height: 0%;\n  -moz-transition-delay: 0s, 0s;\n}";
const REMOVE_ZERO_LENGTH_BUT_NOT_TIME_OR_PERCENT_SUFFIX_EXPECTED: &[u8] =
    b".a{width:0;height:0%;-moz-transition-delay:0s , 0s}";
const PARSING_AND_MINIFYING_BACKGROUND_AND_FONT_INPUT: &[u8] =
    b".a {\n  font:normal 16px Foo, sans-serif;\n}\nbody {\n  background: #fff;\n}";
const PARSING_AND_MINIFYING_BACKGROUND_AND_FONT_EXPECTED: &[u8] = b".a{font:16px Foo,sans-serif;font-style:normal;font-variant:normal;font-weight:normal;font-size:16px;line-height:normal;font-family:Foo,sans-serif}body{background:#fff;background-color:#fff;background-image:none;background-repeat:repeat;background-attachment:scroll;background-position-x:0%;background-position-y:0%}";
const PARSING_AND_MINIFING_VIEWPORT_UNITS_INPUT: &[u8] =
    b".a {\n  margin-top: 70vh;\n  margin-bottom: 20vw;\n}\n";
const PARSING_AND_MINIFING_VIEWPORT_UNITS_EXPECTED: &[u8] =
    b".a{margin-top:70vh;margin-bottom:20vw}";
const STRAY_SINGLE_QUOTE1_INPUT: &[u8] =
    b".view_all a{\n  display: block;\n  'width: 100%;\n  padding: 5px 0 1px 0}";
const STRAY_SINGLE_QUOTE1_EXPECTED: &[u8] = b".view_all a{display:block}";
const STRAY_SINGLE_QUOTE2_INPUT: &[u8] =
    b".view_all a{\n  display: block;\n  'width: 100%;\n  padding: 5px 0 1px 0;}";
const STRAY_SINGLE_QUOTE2_EXPECTED: &[u8] =
    b".view_all a{display:block;'width: 100%;\n  padding: 5px 0 1px 0}";
const STRAY_SINGLE_QUOTE3_INPUT: &[u8] = b".view_all a{\n  display: block;\n  'width: 100%; border:1px solid red;\n  padding: 5px 0 1px 0;  margin: 1px;}";
const STRAY_SINGLE_QUOTE3_EXPECTED: &[u8] = b".view_all a{display:block;margin:1px}";
const CALC_FUNCTION_WITH_ZERO_VALUE_AND_UNIT_INPUT: &[u8] =
    b"@font-face {\n width: calc(600px - 0px)}";
const CALC_FUNCTION_WITH_ZERO_VALUE_AND_UNIT_SUBSTR: &[u8] = b"width:calc(600px - 0px)";
const CSS_UNICODE_RANGE_DESCRIPTOR_INPUT: &[u8] =
    b"@font-face {\n unicode-range: U+0400-045F, U+0490-0491, U+04B0-04B1, U+2116;\n}";
const CSS_UNICODE_RANGE_DESCRIPTOR_SUBSTR: &[u8] =
    b"unicode-range:U+0400-045F,U+0490-0491,U+04B0-04B1,U+2116";
const CLASS_NAME_LEADING_DIGIT_KEEPS_ESCAPE_INPUT: &[u8] = b".\\32 xl\\:col-\\(--col-span-2xl\\) {\n    grid-column: var(--col-span-2xl);\n}\n\n.\\32 xl\\:grid-cols-\\(--grid-cols-2xl\\) {\n    grid-template-columns: var(--grid-cols-2xl);\n}\n@media (min-width: 96rem) { .\\32 xl\\:p-4 { padding: 1rem } }\n.-\\31 x, .\\000032 xl, #\\31 23 { color: red }\n";
const CLASS_NAME_LEADING_DIGIT_KEEPS_ESCAPE_EXPECTED: &[u8] = b".\\32 xl\\:col-\\(--col-span-2xl\\){grid-column:var(--col-span-2xl)}.\\32 xl\\:grid-cols-\\(--grid-cols-2xl\\){grid-template-columns:var(--grid-cols-2xl)}@media (min-width:96rem){.\\32 xl\\:p-4{padding:1rem}}.-\\31 x,.\\32 xl,#\\31 23{color:red}";
const CLASS_NAME_UNESCAPED_LEADING_DIGIT_KEPT_VERBATIM_INPUT: &[u8] =
    b".1a { color: red }\n#123 , .b { color: blue }\n.c { color: #ffff00 }\n";
const CLASS_NAME_UNESCAPED_LEADING_DIGIT_KEPT_VERBATIM_EXPECTED: &[u8] =
    b".1a {color:red}#123 , .b {color:#00f}.c{color:#ff0}";
const CLASS_NAME_UNESCAPED_PAREN_KEPT_VERBATIM_INPUT: &[u8] = b".\\32 xl\\:col-(--col-span-2xl) {\n    grid-column: var(--col-span-2xl);\n}\n\n.\\32 xl\\:grid-cols-(--grid-cols-2xl) {\n    grid-template-columns: var(--grid-cols-2xl);\n}\n";
const CLASS_NAME_UNESCAPED_PAREN_KEPT_VERBATIM_EXPECTED: &[u8] = b".\\32 xl\\:col-(--col-span-2xl) {grid-column:var(--col-span-2xl)}.\\32 xl\\:grid-cols-(--grid-cols-2xl) {grid-template-columns:var(--grid-cols-2xl)}";
