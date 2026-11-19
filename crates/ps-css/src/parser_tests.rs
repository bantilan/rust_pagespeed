//! Ported C++ parser suite (white-box).
//!
//! Mechanically translated from `parser_unittest.cc` (see
//! `tool/translate_parser_tests.py`); do not hand-edit expectations.
//! The fixture helpers below are hand-ported once; test bodies are
//! generated.

#![allow(
    unused_mut,
    unused_assignments,
    unused_variables,
    non_snake_case,
    clippy::unnecessary_unwrap,
    clippy::approx_constant
)]

use super::*;
use crate::ast::declarations_to_css_string;
#[allow(unused_imports)]
use crate::ast::{
    AT_RULE_ERROR, BLOCK_ERROR, CHARSET_ERROR, CSS_COMMENT_ERROR, DECLARATION_ERROR,
    FUNCTION_ERROR, HTML_COMMENT_ERROR, IMPORT_ERROR, MEDIA_ERROR, NO_ERROR, NUMBER_ERROR,
    RULESET_ERROR, SELECTOR_ERROR, SKIPPED_TOKEN_ERROR, UTF8_ERROR, VALUE_ERROR,
};
use crate::identifier::{Ident, Identifier};
use crate::media::media_queries_to_css_string;
use crate::media::MediaQualifier;
use crate::property::{Prop, Property};
use crate::tags::HtmlTag;
use crate::unicode_text::UnicodeText;
use crate::value::{values_to_css_string, Unit, Value, ValueType, Values};

/// gtest `EXPECT_DOUBLE_EQ` semantics: equal within 4 ULPs.
fn assert_ulp_eq(left: f64, right: f64) {
    if left == right {
        return;
    }
    let left_bits = left.to_bits() as i64;
    let right_bits = right.to_bits() as i64;
    if (left_bits < 0) != (right_bits < 0) {
        panic!("not almost equal: {left} vs {right}");
    }
    assert!(
        (left_bits - right_bits).abs() <= 4,
        "not almost equal: {left} vs {right}"
    );
}

fn test_unescape(input: &[u8], parselen: i32, value: u32) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    assert_eq!(value, parser.parse_escape());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_ident(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    assert_eq!(golden, parser.parse_ident().as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_dstring(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    assert_eq!(golden, parser.parse_string(b'"').as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_sstring(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    assert_eq!(golden, parser.parse_string(b'\'').as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

#[allow(clippy::too_many_arguments)]
fn test_any_num(
    input: &[u8],
    parselen: i32,
    num: f64,
    unit: Unit,
    preservation: bool,
    verbatim: &[u8],
) {
    let mut parser = Parser::new(input);
    parser.set_preservation_mode(preservation);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    let value = parser.parse_any().expect("value");
    assert_eq!(value.lexical_type(), ValueType::Number);
    assert_eq!(value.dimension(), unit);
    assert_ulp_eq(value.float_value(), num);
    assert_eq!(expected_len, parser.current_offset());
    assert_eq!(verbatim, value.bytes_in_original_buffer());
}

fn test_any_num_other_unit(input: &[u8], parselen: i32, num: f64, unit: &[u8]) {
    // NOTE: upstream accepts the golden number but never checks it.
    let _ = num;
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    let value = parser.parse_any().expect("value");
    assert_eq!(value.lexical_type(), ValueType::Number);
    assert_eq!(value.dimension(), Unit::Other);
    assert_eq!(unit, value.dimension_unit_text().as_slice());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_any_string(input: &[u8], parselen: i32, kind: ValueType, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    let value = parser.parse_any().expect("value");
    assert_eq!(kind, value.lexical_type());
    assert_eq!(golden, value.string_value().as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_any_ident(input: &[u8], parselen: i32, ident: Ident) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    let value = parser.parse_any().expect("value");
    assert_eq!(ValueType::Ident, value.lexical_type());
    assert_eq!(ident, value.identifier().ident());
    assert_eq!(expected_len, parser.current_offset());
}

fn test_any_other_ident(input: &[u8], parselen: i32, golden: &[u8]) {
    let mut parser = Parser::new(input);
    let expected_len = if parselen == -1 {
        input.len()
    } else {
        parselen as usize
    };
    let value = parser.parse_any().expect("value");
    assert_eq!(ValueType::Ident, value.lexical_type());
    assert_eq!(Ident::Other, value.identifier().ident());
    assert_eq!(golden, value.identifier_text().as_bytes());
    assert_eq!(expected_len, parser.current_offset());
}

fn parse_and_expand_background(input: &[u8], quirks: bool) -> Option<Declarations> {
    let mut parser = Parser::new(input);
    parser.set_quirks_mode(quirks);
    let values = parser.parse_values(Prop::Background)?;
    if values.is_empty() {
        return None;
    }
    let background = Declaration::new(Property::from_prop(Prop::Background), values, false);
    let mut declarations = Declarations::new();
    expand_background(&background, &mut declarations);
    if declarations.is_empty() {
        return None;
    }
    Some(declarations)
}

fn test_background_position(input: &[u8], x: &[u8], y: &[u8]) {
    let declarations = parse_and_expand_background(input, true).expect("decls");
    let mut found_x = false;
    let mut found_y = false;
    for declaration in &declarations {
        match declaration.prop() {
            Prop::BackgroundPositionX => {
                assert_eq!(x, declaration.values()[0].to_css_string());
                found_x = true;
            }
            Prop::BackgroundPositionY => {
                assert_eq!(y, declaration.values()[0].to_css_string());
                found_y = true;
            }
            _ => {}
        }
    }
    assert!(found_x);
    assert!(found_y);
}

enum MethodToTest {
    ParseStylesheet,
    ParseCharset,
    ExtractCharset,
}

fn trap_eof(contents: &[u8], method: MethodToTest) {
    // Copy into an owned buffer (never NUL-terminated) so an
    // over-read cannot hide behind a terminator.
    let owned = contents.to_vec();
    let mut parser = Parser::new(&owned);
    match method {
        MethodToTest::ParseStylesheet => {
            let _ = parser.parse_stylesheet();
            assert_ne!(NO_ERROR, parser.errors_seen_mask());
        }
        MethodToTest::ParseCharset => {
            let _ = parser.parse_charset();
        }
        MethodToTest::ExtractCharset => {
            let _ = parser.extract_charset();
        }
    }
}

fn skip_past(delim: u8, input: &[u8]) -> &[u8] {
    let mut parser = Parser::new(input);
    assert!(parser.skip_past_delimiter(delim));
    // Re-slice from the input: the cursor survives the parser.
    &input[parser.current_offset()..]
}

fn failure_skip_past(delim: u8, input: &[u8]) {
    let mut parser = Parser::new(input);
    assert!(!parser.skip_past_delimiter(delim));
    assert!(parser.done());
}

#[test]
fn bounds_copied_media_queries() {
    // Hand-written: builds the input programmatically.
    let mut css = b"@media screen and (min-width: ".to_vec();
    css.extend(std::iter::repeat_n(b'1', 1024));
    css.extend_from_slice(b"px) {");
    for _ in 0..5000 {
        css.extend_from_slice(b".a{}");
    }
    css.extend_from_slice(b"}");
    let mut parser = Parser::new(&css);
    let _ = parser.parse_stylesheet();
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
    assert!((MEDIA_ERROR & parser.errors_seen_mask()) != 0);
}

// DIVERGENCE (unescape): the committed test expects `\\abcdef` to
// decode to a space, but `UniLib::IsInterchangeValid(0xABCDEF)`
// returns true (verified against compiled `unilib.cc`), so the
// committed `ParseEscape` returns 0xABCDEF. The expectation below is
// actual C++ behavior; downstream `push_back` still coerces the
// out-of-range value to a space, so parsed output is unaffected.
#[test]
fn unescape() {
    test_unescape(b"\\abcdef aabc", 8, 0xABCDEF);
    test_unescape(b"\\A", 2, 0xA);
    test_unescape(b"\\A0b5C\r\n", 8, 0xa0b5c);
    test_unescape(b"\\AB ", 4, 0xAB);
}

// DIVERGENCE (value_validation): the committed test expects the
// parser to drop non-color `border-color` values, but the committed
// parser has no such validation (`ParseAnyExpectingColor` falls back
// to `ParseAny`), keeping all 10 declarations with no errors —
// verified by compiling and running the vendored parser. The passing
// minify goldens (e.g. `width: 10` kept verbatim) confirm the
// no-validation behavior, so the port follows the code, not the
// stale expectation.
#[test]
fn value_validation() {
    let mut a = Parser::new(b"width: {$width}");
    let _ = a.parse_declarations();
    a = Parser::new(b"border-color: \"string\"; border-color: url(\"abc\"); border-color: 12345; border-color: none; border-color: inherited; border-color: red; border-color: #123456; border-color: transparent; border-color: inherit; border-color: unknown; ");
    let t = Some(a.parse_declarations());
    assert_eq!(NO_ERROR, a.errors_seen_mask());
    assert_eq!(10usize, t.as_ref().unwrap().len());
    let expected = [
        b"border-color: \"string\"".as_slice(),
        b"border-color: url(abc)".as_slice(),
        b"border-color: 12345".as_slice(),
        b"border-color: none".as_slice(),
        b"border-color: inherited".as_slice(),
        b"border-color: #ff0000".as_slice(),
        b"border-color: #123456".as_slice(),
        b"border-color: transparent".as_slice(),
        b"border-color: inherit".as_slice(),
        b"border-color: unknown".as_slice(),
    ];
    for (index, golden) in expected.iter().enumerate() {
        assert_eq!(
            t.as_ref().unwrap()[index].to_css_string().as_slice(),
            *golden
        );
    }
}

// DIVERGENCE (accept_all_values): same root cause as
// value_validation — non-preservation mode keeps (not drops) the
// declaration, with no error. Verified against the compiled parser.
#[test]
fn accept_all_values() {
    let mut p = Parser::new(b"display: -moz-inline-box");
    p.set_preservation_mode(true);
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(1usize, declarations.as_ref().unwrap()[0].values().len());
    let value = Some(&(declarations.as_ref().unwrap()[0].values()[0]));
    assert_eq!(ValueType::Ident, value.as_ref().unwrap().lexical_type());
    assert_eq!(Ident::Other, value.as_ref().unwrap().identifier().ident());
    assert_eq!(
        b"-moz-inline-box",
        (value.as_ref().unwrap().identifier().ident_text()).as_bytes()
    );
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"display: -moz-inline-box" as &[u8]
    );
    let mut p2 = Parser::new(b"display: -moz-inline-box");
    p2.set_preservation_mode(false);
    declarations = Some(p2.parse_declarations());
    assert_eq!(NO_ERROR, p2.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"display: -moz-inline-box" as &[u8]
    );
}

// DIVERGENCE (at_font_face): the committed expectation drops the
// `font-variant` declaration, but the committed parser keeps it
// (verified byte-identical against the compiled parser, mask 130 in
// both). Only that segment differs; the rest is unchanged.
#[test]
fn at_font_face() {
    let mut parser: Parser;
    let mut stylesheet: Option<Stylesheet>;
    parser =
        Parser::new(b"@font-face{font-family:'Ubuntu';font-style:normal}\n.foo { width: 1px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n@font-face { font-family: \"Ubuntu\"; font-style: normal }\n.foo {width: 1px}\n" as &[u8]);
    parser = Parser::new(b"@font-face{font-family:'Ubuntu';font-style:normal}.foo { width: 1px; }");
    parser.set_preservation_mode(true);
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n@font-face { font-family: \"Ubuntu\"; font-style: normal }\n.foo {width: 1px}\n" as &[u8]);
    parser = Parser::new(b"@media print {\n  .foo { width: 1px; }\n  @font-face { font-family: 'Ubuntu'; font-style: normal; }\n  .bar { height: 2em; }\n}\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n@media print { @font-face { font-family: \"Ubuntu\"; font-style: normal } }\n@media print { .foo {width: 1px} }\n@media print { .bar {height: 2em} }\n" as &[u8]);
    parser = Parser::new(b"@media print {\n  @font-face { font-family: 'Dothraki'; src: local('Khal'), url('dothraki.woff') format('woff'); }\n}\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n@media print { @font-face { font-family: \"Dothraki\"; src: local(\"Khal\") , url(dothraki.woff) format(\"woff\") } }\n\n" as &[u8]);
    parser = Parser::new(b"@font-face {\n  font-family: MainText;\n  src: url(gentium.eot);\n  src: local(\"Gentium\"), url('gentium.ttf') format('truetype'), url(gentium.woff);\n  font-style: italic;\n  font-weight: 800;\n  font-stretch: ultra-condensed;\n  unicode-range: U+590-5ff, u+4??, U+1F63B;\n  font-variant: historical-forms, character-variant(cv13), annotiation(circled);\n  font-feature-settings: 'hwid', 'swsh' 2;\n}\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n@font-face { font-family: MainText; src: url(gentium.eot); src: local(\"Gentium\") , url(gentium.ttf) format(\"truetype\") , url(gentium.woff); font-style: italic; font-weight: 800; font-stretch: ultra-condensed; font-variant: historical-forms , character-variant(cv13) , annotiation(circled); font-feature-settings: \"hwid\" , \"swsh\" 2 }\n\n" as &[u8]);
}

#[test]
fn error_number() {
    assert_eq!(0, crate::ast::error_number(UTF8_ERROR));
    assert_eq!(1, crate::ast::error_number(DECLARATION_ERROR));
    assert_eq!(8, crate::ast::error_number(RULESET_ERROR));
    assert_eq!(14, crate::ast::error_number(AT_RULE_ERROR));
}

#[test]
fn ident() {
    test_ident(b"abcd rexo\n", 4, b"abcd");
    test_ident(
        b"\xe5\x8f\xb0\xe7\x81\xa3\xe8\x8f\xaf\xe8\xaa\x9e",
        12,
        b"\xe5\x8f\xb0\xe7\x81\xa3\xe8\x8f\xaf\xe8\xaa\x9e",
    );
    test_ident(b"\\41\\42 \\43 \\44", 14, b"ABCD");
    test_ident(b"\\41\\42 \\43 \\44g'r,'rcg.,',", 15, b"ABCDg");
    test_ident(b"\\41\\42 \\43 \\44\r\ng'r,'rcg.,',", 17, b"ABCDg");
    test_ident(b"-blah-_67", 9, b"-blah-_67");
    test_ident(b"\\!\\&\\^\\*\\\\e", 11, b"!&^*\\e");
}

#[test]
fn string() {
    test_sstring(
        b"'ab\\'aoe\"\\'eo\xe7\x81\xa3'\xe7\x81\xa3",
        17,
        b"ab'aoe\"'eo\xe7\x81\xa3",
    );
    test_dstring(
        b"\"ab'aoe\\\"'eo\xe7\x81\xa3\"\xe7\x81\xa3",
        16,
        b"ab'aoe\"'eo\xe7\x81\xa3",
    );
    test_sstring(b"'ab\naoeu", 3, b"ab");
    test_dstring(b"\"ab\naoeu", 3, b"ab");
    test_dstring(b"\"ab\\\naoeu\"", 10, b"abaoeu");
}

#[test]
fn anynum() {
    test_any_num(b"3.1415 4aone", 6, 3.1415, Unit::NoUnit, false, b"");
    test_any_num(b".1415 4aone", 5, 0.1415, Unit::NoUnit, true, b".1415");
    test_any_num(b"5 4aone", 1, 5.0, Unit::NoUnit, true, b"5");
    test_any_num(b"0.1415pt 4aone", 8, 0.1415, Unit::Pt, true, b"0.1415");
    test_any_num(b".1415pc 4aone", 7, 0.1415, Unit::Pc, true, b".1415");
    test_any_num(b"5s 4aone", 2, 5.0, Unit::S, false, b"");
    test_any_num_other_unit(b"5sacks 4aone", 6, 5.0, b"sacks");
    test_any_num_other_unit(b"5\xe7\x81\xa3 4aone", 4, 5.0, b"\xe7\x81\xa3");
}

#[test]
fn anystring() {
    test_any_ident(b"none b c d e", 4, Ident::None);
    test_any_ident(b"none; b c d e", 4, Ident::None);
    test_any_ident(b"none  ; b c d e", 4, Ident::None);
    test_any_other_ident(b"a b c d e", 1, b"a");
    test_any_other_ident(b"a; b c d e", 1, b"a");
    test_any_other_ident(b"a  ; b c d e", 1, b"a");
    test_any_string(
        b"'ab\\'aoe\"\\'eo\xe7\x81\xa3'\xe7\x81\xa3  ; b c d e",
        17,
        ValueType::String,
        b"ab'aoe\"'eo\xe7\x81\xa3",
    );
}

#[test]
fn color() {
    let mut a = Parser::new(b"abCdEF brc.,aoek");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#abcdef" as &[u8]
    );
    a = Parser::new(b"abCdEF brc.,aoek");
    a.set_quirks_mode(false);
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"#abCdEF brc.,aoek");
    a.set_quirks_mode(false);
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#abcdef" as &[u8]
    );
    a = Parser::new(b"abC btneo");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#aabbcc" as &[u8]
    );
    a = Parser::new(b"#white something");
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"#white something");
    a.set_quirks_mode(false);
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"white something");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#ffffff" as &[u8]
    );
    a = Parser::new(b"white something");
    a.set_quirks_mode(false);
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#ffffff" as &[u8]
    );
    a = Parser::new(b"buttonface something");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#ece9d8" as &[u8]
    );
    a = Parser::new(b"'abCdEF' brc.,aoek");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#abcdef" as &[u8]
    );
    a = Parser::new(b"'abCdEF' brc.,aoek");
    a.set_quirks_mode(false);
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"'#abCdEF brc'.,aoek");
    a.set_quirks_mode(false);
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"\"abC\" btneo");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#aabbcc" as &[u8]
    );
    a = Parser::new(b"'#white' something");
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"'#white' something");
    a.set_quirks_mode(false);
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"'white' something");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#ffffff" as &[u8]
    );
    a = Parser::new(b"'white' something");
    a.set_quirks_mode(false);
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#ffffff" as &[u8]
    );
    a = Parser::new(b"100%");
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"100px");
    assert!(!(a.parse_color().is_defined()));
    a = Parser::new(b"100");
    assert_eq!(
        a.parse_color().to_css_string().as_slice(),
        b"#110000" as &[u8]
    );
    a = Parser::new(b"100px");
    let mut t = a.parse_any_expecting_color();
    assert_eq!(ValueType::Number, t.as_ref().unwrap().lexical_type());
    assert_eq!(
        t.as_ref().unwrap().to_css_string().as_slice(),
        b"100px" as &[u8]
    );
    a = Parser::new(b"rgb(12,25,30)");
    t = a.parse_any();
    assert_eq!(
        t.as_ref().unwrap().color_value().to_css_string().as_slice(),
        b"#0c191e" as &[u8]
    );
    a = Parser::new(b"rgb( 12% , 25%, 30%)");
    t = a.parse_any();
    assert_eq!(
        t.as_ref().unwrap().color_value().to_css_string().as_slice(),
        b"#1e3f4c" as &[u8]
    );
    a = Parser::new(b"rgb( 12% , 25% 30%)");
    t = a.parse_any();
    assert!(t.is_none());
    a = Parser::new(b"0000ff");
    t = a.parse_any_expecting_color();
    assert_eq!(ValueType::Color, t.as_ref().unwrap().lexical_type());
    assert_eq!(
        t.as_ref().unwrap().to_css_string().as_slice(),
        b"#0000ff" as &[u8]
    );
    assert_eq!(NO_ERROR, a.errors_seen_mask());
    a = Parser::new(b"0000ff");
    a.set_quirks_mode(false);
    t = a.parse_any_expecting_color();
    assert_eq!(ValueType::Number, t.as_ref().unwrap().lexical_type());
    assert_eq!(
        t.as_ref().unwrap().to_css_string().as_slice(),
        b"0ff" as &[u8]
    );
    assert_eq!(NO_ERROR, a.errors_seen_mask());
    a = Parser::new(b"0000ff");
    a.set_quirks_mode(false);
    a.set_preservation_mode(true);
    t = a.parse_any_expecting_color();
    assert_eq!(ValueType::Number, t.as_ref().unwrap().lexical_type());
    assert_eq!(
        t.as_ref().unwrap().to_css_string().as_slice(),
        b"0ff" as &[u8]
    );
    assert_eq!(VALUE_ERROR, a.errors_seen_mask());
}

#[test]
fn url() {
    let mut a = Parser::new(b"url(blah)");
    let mut t = a.parse_any();
    assert_eq!(ValueType::Uri, t.as_ref().unwrap().lexical_type());
    assert_eq!(b"blah", (t.as_ref().unwrap().string_value()).as_bytes());
    a = Parser::new(b"url( blah )");
    t = a.parse_any();
    assert_eq!(ValueType::Uri, t.as_ref().unwrap().lexical_type());
    assert_eq!(b"blah", (t.as_ref().unwrap().string_value()).as_bytes());
    a = Parser::new(b"url( blah extra)");
    t = a.parse_any();
    assert_eq!(None, t.as_ref());
}

#[test]
fn rect() {
    let mut a = Parser::new(b"rect( 12,  10,auto  200px)");
    let mut t = a.parse_any();
    assert_eq!(ValueType::Rect, t.as_ref().unwrap().lexical_type());
    assert_eq!(4usize, t.as_ref().unwrap().parameters().len());
    assert_eq!(
        ValueType::Number,
        t.as_ref().unwrap().parameters()[0].lexical_type()
    );
    assert_eq!(12, t.as_ref().unwrap().parameters()[0].integer_value());
    assert_eq!(
        ValueType::Ident,
        t.as_ref().unwrap().parameters()[2].lexical_type()
    );
    assert_eq!(
        Ident::Auto,
        t.as_ref().unwrap().parameters()[2].identifier().ident()
    );
    a = Parser::new(b"rect(auto)");
    t = a.parse_any();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"rect()");
    t = a.parse_any();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"rect(13 10 auto 4)");
    t = a.parse_any();
    assert_eq!(13, t.as_ref().unwrap().parameters()[0].integer_value());
    a = Parser::new(b"rect(14,10,1,2)");
    t = a.parse_any();
    assert_eq!(14, t.as_ref().unwrap().parameters()[0].integer_value());
    a = Parser::new(b"rect(15 10 1)");
    t = a.parse_any();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"rect(16 10 1 2 3)");
    t = a.parse_any();
    assert_eq!(None, t.as_ref());
}

#[test]
fn background() {
    let mut decls = parse_and_expand_background(b"#333", true);
    assert_eq!(6usize, decls.as_ref().unwrap().len());
    decls = parse_and_expand_background(b"fff", true);
    assert!(decls.is_some());
    assert!(parse_and_expand_background(b"fff", false).is_none());
    decls = parse_and_expand_background(b"fff000", true);
    assert!(decls.is_some());
    assert!(parse_and_expand_background(b"fff000", false).is_none());
    decls = parse_and_expand_background(b"100%", true);
    assert!(decls.is_some());
    assert_eq!(6usize, decls.as_ref().unwrap().len());
    assert_eq!(Prop::BackgroundColor, decls.as_ref().unwrap()[0].prop());
    assert_eq!(
        Ident::Transparent,
        decls.as_ref().unwrap()[0].values()[0].identifier().ident()
    );
    assert_eq!(Prop::BackgroundPositionX, decls.as_ref().unwrap()[4].prop());
    assert_eq!(
        decls.as_ref().unwrap()[4].values()[0]
            .to_css_string()
            .as_slice(),
        b"100%" as &[u8]
    );
    assert!(parse_and_expand_background(b"", true).is_none());
    assert!(parse_and_expand_background(b";", true).is_none());
    assert!(parse_and_expand_background(b"\"string\"", true).is_none());
    assert!(parse_and_expand_background(b"normal", true).is_none());
    decls = parse_and_expand_background(b"inherit", true);
    assert_eq!(6usize, decls.as_ref().unwrap().len());
    for i in 0..6 {
        assert_eq!(
            Ident::Inherit,
            decls.as_ref().unwrap()[i].values()[0].identifier().ident()
        );
    }
    assert!(parse_and_expand_background(b"inherit none", true).is_none());
    assert!(parse_and_expand_background(b"none inherit", true).is_none());
    decls = parse_and_expand_background(b"none", true);
    assert_eq!(
        Ident::Transparent,
        decls.as_ref().unwrap()[0].values()[0].identifier().ident()
    );
    assert_eq!(
        Ident::None,
        decls.as_ref().unwrap()[1].values()[0].identifier().ident()
    );
    assert_eq!(
        Ident::Repeat,
        decls.as_ref().unwrap()[2].values()[0].identifier().ident()
    );
    assert_eq!(
        Ident::Scroll,
        decls.as_ref().unwrap()[3].values()[0].identifier().ident()
    );
    decls = parse_and_expand_background(b"fixed", true);
    assert_eq!(
        Ident::Fixed,
        decls.as_ref().unwrap()[3].values()[0].identifier().ident()
    );
    decls = parse_and_expand_background(b"transparent", true);
    assert_eq!(
        Ident::Transparent,
        decls.as_ref().unwrap()[0].values()[0].identifier().ident()
    );
    decls = parse_and_expand_background(b"none url(abc)", true);
    assert_eq!(
        ValueType::Uri,
        decls.as_ref().unwrap()[1].values()[0].lexical_type()
    );
    decls = parse_and_expand_background(b"none red fixed", true);
    assert_eq!(
        decls.as_ref().unwrap()[0].values()[0]
            .color_value()
            .to_css_string()
            .as_slice(),
        b"#ff0000" as &[u8]
    );
    assert_eq!(
        Ident::None,
        decls.as_ref().unwrap()[1].values()[0].identifier().ident()
    );
    assert_eq!(
        Ident::Fixed,
        decls.as_ref().unwrap()[3].values()[0].identifier().ident()
    );
    test_background_position(b"none", b"0%", b"0%");
    test_background_position(b"10", b"10", b"50%");
    test_background_position(b"10 20%", b"10", b"20%");
    test_background_position(b"10 100%", b"10", b"100%");
    test_background_position(b"top left", b"left", b"top");
    test_background_position(b"left top", b"left", b"top");
    test_background_position(b"bottom", b"50%", b"bottom");
    test_background_position(b"bottom center", b"center", b"bottom");
    test_background_position(b"center bottom", b"center", b"bottom");
    test_background_position(b"left", b"left", b"50%");
    test_background_position(b"left center", b"left", b"center");
    test_background_position(b"center left", b"left", b"center");
    test_background_position(b"center", b"center", b"50%");
    test_background_position(b"center center", b"center", b"center");
    test_background_position(b"center 30%", b"center", b"30%");
    test_background_position(b"30% center", b"30%", b"center");
    test_background_position(b"30% bottom", b"30%", b"bottom");
    test_background_position(b"left 30%", b"left", b"30%");
    test_background_position(b"30% left", b"left", b"30%");
    test_background_position(b"30% 20% 50%", b"30%", b"20%");
    test_background_position(b"bottom center right", b"center", b"bottom");
    test_background_position(b"bottom right top", b"right", b"bottom");
    test_background_position(b"bottom top right", b"right", b"top");
    test_background_position(b"top right left", b"right", b"top");
    test_background_position(b"right left top", b"left", b"top");
}

#[test]
fn font_family() {
    let mut a = Parser::new(b" Arial font, 'Sans', system, menu new ");
    let mut t = Some(Vec::new());
    assert!(a.parse_font_family(t.as_mut().unwrap()));
    assert_eq!(4usize, t.as_ref().unwrap().len());
    assert_eq!(ValueType::Ident, t.as_ref().unwrap()[0].lexical_type());
    assert_eq!(
        b"Arial font",
        (t.as_ref().unwrap()[0].identifier_text()).as_bytes()
    );
    assert_eq!(ValueType::String, t.as_ref().unwrap()[1].lexical_type());
    assert_eq!(
        b"system",
        (t.as_ref().unwrap()[2].identifier_text()).as_bytes()
    );
    assert_eq!(
        b"menu new",
        (t.as_ref().unwrap()[3].identifier_text()).as_bytes()
    );
    a = Parser::new(b"Verdana 3");
    t = Some(Vec::new());
    assert!(!(a.parse_font_family(t.as_mut().unwrap())));
    a = Parser::new(b"Verdana :");
    t = Some(Vec::new());
    assert!(!(a.parse_font_family(t.as_mut().unwrap())));
    a = Parser::new(b"Verdana ;");
    t = Some(Vec::new());
    assert!(a.parse_font_family(t.as_mut().unwrap()));
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(ValueType::Ident, t.as_ref().unwrap()[0].lexical_type());
    assert_eq!(
        b"Verdana",
        (t.as_ref().unwrap()[0].identifier_text()).as_bytes()
    );
    let mut d: Option<Declarations>;
    a = Parser::new(b"font-family: foo");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(1usize, d.as_ref().unwrap().len());
    assert_eq!(1usize, d.as_ref().unwrap()[0].values().len());
    a = Parser::new(b"font-family: ,foo");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
    a = Parser::new(b"font-family: foo,");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
    a = Parser::new(b"font-family: '',foo");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(1usize, d.as_ref().unwrap().len());
    assert_eq!(2usize, d.as_ref().unwrap()[0].values().len());
    a = Parser::new(b"font-family: '',,foo");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
    a = Parser::new(b"font-family: 'bar' foo");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
    a = Parser::new(b"font-family: 'bar' 'foo'");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
    a = Parser::new(b"font-family: bar 'foo'");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
    a = Parser::new(b"font-family: 'bar'foo");
    d = Some(a.parse_raw_declarations());
    assert!((d.as_ref()).is_some());
    assert_eq!(0usize, d.as_ref().unwrap().len());
}

#[test]
fn font() {
    let mut a = Parser::new(b"font: caption");
    let mut declarations = Some(a.parse_declarations());
    let expected_caption_expansion = b"font: caption; font-style: normal; font-variant: normal; font-weight: normal; font-size: 10.6667px; line-height: normal; font-family: caption";
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        expected_caption_expansion as &[u8]
    );
    a = Parser::new(b"font: inherit");
    declarations = Some(a.parse_declarations());
    let expected_inherit_expansion = b"font: inherit; font-style: inherit; font-variant: inherit; font-weight: inherit; font-size: inherit; line-height: inherit; font-family: inherit";
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        expected_inherit_expansion as &[u8]
    );
    a = Parser::new(b"normal 10px /120% Arial 'Sans'");
    let mut t = a.parse_font();
    assert!((t.as_ref()).is_none());
    a = Parser::new(b"normal 10px /120% Arial, 'Sans'");
    t = a.parse_font();
    assert_eq!(7usize, t.as_ref().unwrap().len());
    assert_ulp_eq(10.0, t.as_ref().unwrap()[3].float_value());
    assert_eq!(Unit::Percent, t.as_ref().unwrap()[4].dimension());
    a = Parser::new(b"italic 10px Arial, Sans");
    t = a.parse_font();
    assert_eq!(7usize, t.as_ref().unwrap().len());
    assert_ulp_eq(10.0, t.as_ref().unwrap()[3].float_value());
    assert_eq!(Ident::Normal, t.as_ref().unwrap()[4].identifier().ident());
    a = Parser::new(b"SMALL-caps normal x-large Arial");
    t = a.parse_font();
    assert_eq!(6usize, t.as_ref().unwrap().len());
    assert_eq!(Ident::Normal, t.as_ref().unwrap()[0].identifier().ident());
    assert_eq!(
        Ident::SmallCaps,
        t.as_ref().unwrap()[1].identifier().ident()
    );
    assert_eq!(Ident::XLarge, t.as_ref().unwrap()[3].identifier().ident());
    assert_eq!(Ident::Normal, t.as_ref().unwrap()[4].identifier().ident());
    a = Parser::new(b"bolder 100 120 Arial");
    t = a.parse_font();
    assert_eq!(6usize, t.as_ref().unwrap().len());
    assert_eq!(100, t.as_ref().unwrap()[2].integer_value());
    assert_eq!(120, t.as_ref().unwrap()[3].integer_value());
    assert_eq!(Ident::Normal, t.as_ref().unwrap()[4].identifier().ident());
    a = Parser::new(b"10px normal");
    t = a.parse_font();
    assert_eq!(6usize, t.as_ref().unwrap().len());
    assert_eq!(10, t.as_ref().unwrap()[3].integer_value());
    assert_eq!(Ident::Normal, t.as_ref().unwrap()[5].identifier().ident());
    a = Parser::new(b"normal 10px ");
    t = a.parse_font();
    assert_eq!(5usize, t.as_ref().unwrap().len());
    a = Parser::new(b"10px/12pt ");
    t = a.parse_font();
    assert_eq!(5usize, t.as_ref().unwrap().len());
    a = Parser::new(b"menu 10px");
    t = a.parse_font();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"Arial, menu ");
    t = a.parse_font();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"transparent 10px ");
    t = a.parse_font();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"normal / 10px Arial");
    t = a.parse_font();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"normal 10px/ Arial");
    t = a.parse_font();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b"normal 10px Arial #333");
    t = a.parse_font();
    assert_eq!(None, t.as_ref());
}

#[test]
fn numbers() {
    let mut p: Parser;
    let mut v: Option<Value>;
    p = Parser::new(b"1");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(1, v.as_ref().unwrap().integer_value());
    assert_eq!(Unit::NoUnit, v.as_ref().unwrap().dimension());
    assert!(p.done());
    p = Parser::new(b"1;");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(1, v.as_ref().unwrap().integer_value());
    assert_eq!(Unit::NoUnit, v.as_ref().unwrap().dimension());
    assert_eq!(b';', p.current_byte().unwrap());
    p = Parser::new(b"3vm;");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(3, v.as_ref().unwrap().integer_value());
    assert_eq!(Unit::Vm, v.as_ref().unwrap().dimension());
    assert_eq!(b';', p.current_byte().unwrap());
    p = Parser::new(b"1em;");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(1, v.as_ref().unwrap().integer_value());
    assert_eq!(Unit::Em, v.as_ref().unwrap().dimension());
    assert_eq!(b';', p.current_byte().unwrap());
    p = Parser::new(b"1.1em;");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(1.1, v.as_ref().unwrap().float_value());
    assert_eq!(Unit::Em, v.as_ref().unwrap().dimension());
    assert_eq!(b';', p.current_byte().unwrap());
    p = Parser::new(b".1");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(0.1, v.as_ref().unwrap().float_value());
    assert_eq!(Unit::NoUnit, v.as_ref().unwrap().dimension());
    assert!(p.done());
    p = Parser::new(b"1.em;");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(1, v.as_ref().unwrap().integer_value());
    assert_eq!(Unit::NoUnit, v.as_ref().unwrap().dimension());
    assert_eq!(b'.', p.current_byte().unwrap());
    p = Parser::new(b"1.");
    v = p.parse_number();
    assert_eq!(ValueType::Number, v.as_ref().unwrap().lexical_type());
    assert_eq!(1, v.as_ref().unwrap().integer_value());
    assert_eq!(Unit::NoUnit, v.as_ref().unwrap().dimension());
    assert_eq!(b'.', p.current_byte().unwrap());
}

#[test]
fn values() {
    let mut a = Parser::new(b"rgb(12,25,30) url(blah) url('blah.png') 12% !important 'arial'");
    let mut t = a.parse_values(Prop::Other);
    assert_eq!(4usize, t.as_ref().unwrap().len());
    assert_eq!(ValueType::Color, t.as_ref().unwrap()[0].lexical_type());
    assert_eq!(ValueType::Uri, t.as_ref().unwrap()[1].lexical_type());
    assert_eq!(ValueType::Uri, t.as_ref().unwrap()[2].lexical_type());
    assert_eq!(ValueType::Number, t.as_ref().unwrap()[3].lexical_type());
    assert_eq!(Unit::Percent, t.as_ref().unwrap()[3].dimension());
    a = Parser::new(b"rgb( 12,  25,30) @ignored  url( blah  ) rect(12 10 auto 200px) { should be {nested }discarded } ident;");
    t = a.parse_values(Prop::Other);
    assert_eq!(4usize, t.as_ref().unwrap().len());
    assert_eq!(ValueType::Color, t.as_ref().unwrap()[0].lexical_type());
    assert_eq!(ValueType::Uri, t.as_ref().unwrap()[1].lexical_type());
    assert_eq!(ValueType::Rect, t.as_ref().unwrap()[2].lexical_type());
    assert_eq!(ValueType::Ident, t.as_ref().unwrap()[3].lexical_type());
    assert_eq!(
        b"ident",
        (t.as_ref().unwrap()[3].identifier_text()).as_bytes()
    );
    let mut val = Some((t.as_ref().unwrap()[2]).clone());
    assert_eq!(ValueType::Rect, val.as_ref().unwrap().lexical_type());
    assert_eq!(4usize, val.as_ref().unwrap().parameters().len());
    assert_eq!(
        ValueType::Number,
        val.as_ref().unwrap().parameters()[0].lexical_type()
    );
    assert_eq!(12, val.as_ref().unwrap().parameters()[0].integer_value());
    assert_eq!(
        ValueType::Ident,
        val.as_ref().unwrap().parameters()[2].lexical_type()
    );
    assert_eq!(
        b"auto",
        (val.as_ref().unwrap().parameters()[2].identifier_text()).as_bytes()
    );
}

#[test]
fn skip_corner_cases() {
    let mut p = Parser::new(b"\x0c /* foobar /* */ foobar */");
    p.skip_space();
    assert_eq!(b"foobar */", p.remaining());
    p = Parser::new(b"{[ (]}) foo\\]\\}bar ] \\} } Now it's closed. }");
    assert!(p.skip_matching());
    assert_eq!(b" Now it's closed. }", p.remaining());
    p = Parser::new(b"[/*]*/ 'fake ]' () { \"also fake }\" ]} ] Finally.");
    assert!(p.skip_matching());
    assert_eq!(b" Finally.", p.remaining());
    p = Parser::new(b"(");
    assert!(!(p.skip_matching()));
    assert_eq!(b"", p.remaining());
    p = Parser::new(b"foo({[)]}, bar\\)(), ')', /*)*/,), baz");
    assert!(p.skip_past_delimiter(b','));
    assert_eq!(b" baz", p.remaining());
    p = Parser::new(b"{[](} f\\(oo)} @rule bar");
    assert!(p.skip_to_next_any());
    assert_eq!(b"bar", p.remaining());
    p = Parser::new(b"not all and (color), print { .a { color: red; } } .b { color: green; }");
    assert!(p.skip_to_at_rule_end());
    assert_eq!(b" .b { color: green; }", p.remaining());
    p = Parser::new(b"and(\"don't\" { stop, here }) { } .b { color: green; }");
    assert!(p.skip_to_at_rule_end());
    assert_eq!(b" .b { color: green; }", p.remaining());
    p = Parser::new(b"screen; .a { color: red; }");
    assert!(p.skip_to_at_rule_end());
    assert_eq!(b" .a { color: red; }", p.remaining());
    p = Parser::new(b"screen and (color, print");
    assert!(!(p.skip_to_at_rule_end()));
    assert_eq!(b"", p.remaining());
    p = Parser::new(b"not all and (color), print { .a { color: red; } }");
    p.skip_to_media_query_end();
    assert_eq!(b", print { .a { color: red; } }", p.remaining());
    p = Parser::new(b"and(\"don't\", stop, here), screen { }");
    p.skip_to_media_query_end();
    assert_eq!(b", screen { }", p.remaining());
    p = Parser::new(b"screen { .a { color: red; } }");
    p.skip_to_media_query_end();
    assert_eq!(b"{ .a { color: red; } }", p.remaining());
    p = Parser::new(b"screen; .a { color: red; }");
    p.skip_to_media_query_end();
    assert_eq!(b"; .a { color: red; }", p.remaining());
    p = Parser::new(b"screen and (color, print");
    p.skip_to_media_query_end();
    assert_eq!(b"", p.remaining());
}

#[test]
fn skip_matching() {
    let truetestcases = [
        b"{{{{}}}} serif" as &[u8],
        b"{ {  { {  }    }   }    } serif" as &[u8],
        b"{@ident1{{ @ident {}}}} serif" as &[u8],
        b"{{ident{{}ident2}}} serif" as &[u8],
    ];
    for case in truetestcases {
        let mut p = Parser::new(case);
        assert!(p.skip_matching());
        let mut values: Values = Vec::new();
        assert!(p.parse_font_family(&mut values));
        assert_eq!(1usize, values.len());
        assert_eq!(ValueType::Ident, values[0].lexical_type());
        assert_eq!(b"serif", (values[0].identifier_text()).as_bytes());
    }
    let falsetestcases = [
        b"{{{{}}} serif" as &[u8],
        b"{{{{}}}}} serif" as &[u8],
        b"{{{{}}}}}" as &[u8],
    ];
    for case in falsetestcases {
        let mut p = Parser::new(case);
        p.skip_matching();
        let mut values: Values = Vec::new();
        p.parse_font_family(&mut values);
        assert_eq!(0usize, values.len());
    }
}

#[test]
fn declarations() {
    let mut a = Parser::new(b"color: #333; line-height: 1.3;text-align: justify; font-family: \"Gill Sans MT\",\"Gill Sans\", GillSans, Arial, Helvetica, sans-serif");
    let mut t = Some(a.parse_declarations());
    assert_eq!(4usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Color, t.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::LineHeight, t.as_ref().unwrap()[1].prop());
    assert_eq!(Prop::TextAlign, t.as_ref().unwrap()[2].prop());
    assert_eq!(Prop::FontFamily, t.as_ref().unwrap()[3].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[0].values().len());
    assert_eq!(
        ValueType::Color,
        t.as_ref().unwrap()[0].values()[0].lexical_type()
    );
    assert_eq!(
        t.as_ref().unwrap()[0].values()[0]
            .color_value()
            .to_css_string()
            .as_slice(),
        b"#333333" as &[u8]
    );
    assert_eq!(6usize, t.as_ref().unwrap()[3].values().len());
    assert_eq!(
        ValueType::String,
        t.as_ref().unwrap()[3].values()[0].lexical_type()
    );
    assert_eq!(
        b"Gill Sans MT",
        (t.as_ref().unwrap()[3].values()[0].string_value()).as_bytes()
    );
    a = Parser::new(b"background-color: 333; color: \"abcdef\";background-color: #red; color: \"white\";background-color: rgb(255, 10%, 10)");
    t = Some(a.parse_declarations());
    assert_eq!(4usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::BackgroundColor, t.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::Color, t.as_ref().unwrap()[1].prop());
    assert_eq!(Prop::Color, t.as_ref().unwrap()[2].prop());
    assert_eq!(Prop::BackgroundColor, t.as_ref().unwrap()[3].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[0].values().len());
    assert_eq!(
        ValueType::Color,
        t.as_ref().unwrap()[0].values()[0].lexical_type()
    );
    assert_eq!(
        t.as_ref().unwrap()[0].values()[0]
            .color_value()
            .to_css_string()
            .as_slice(),
        b"#333333" as &[u8]
    );
    assert_eq!(1usize, t.as_ref().unwrap()[1].values().len());
    assert_eq!(
        ValueType::Color,
        t.as_ref().unwrap()[1].values()[0].lexical_type()
    );
    assert_eq!(
        t.as_ref().unwrap()[1].values()[0]
            .color_value()
            .to_css_string()
            .as_slice(),
        b"#abcdef" as &[u8]
    );
    assert_eq!(1usize, t.as_ref().unwrap()[2].values().len());
    assert_eq!(
        ValueType::Color,
        t.as_ref().unwrap()[2].values()[0].lexical_type()
    );
    assert_eq!(
        t.as_ref().unwrap()[2].values()[0]
            .color_value()
            .to_css_string()
            .as_slice(),
        b"#ffffff" as &[u8]
    );
    assert_eq!(1usize, t.as_ref().unwrap()[3].values().len());
    assert_eq!(
        ValueType::Color,
        t.as_ref().unwrap()[3].values()[0].lexical_type()
    );
    assert_eq!(
        t.as_ref().unwrap()[3].values()[0]
            .color_value()
            .to_css_string()
            .as_slice(),
        b"#ff190a" as &[u8]
    );
    a = Parser::new(b"background: #333 fixed no-repeat; ");
    t = Some(a.parse_declarations());
    assert_eq!(7usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Background, t.as_ref().unwrap()[0].prop());
    assert_eq!(3usize, t.as_ref().unwrap()[0].values().len());
    assert_eq!(Prop::BackgroundColor, t.as_ref().unwrap()[1].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[1].values().len());
    assert_eq!(Prop::BackgroundImage, t.as_ref().unwrap()[2].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[2].values().len());
    assert_eq!(Prop::BackgroundRepeat, t.as_ref().unwrap()[3].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[3].values().len());
    assert_eq!(Prop::BackgroundAttachment, t.as_ref().unwrap()[4].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[4].values().len());
    assert_eq!(Prop::BackgroundPositionX, t.as_ref().unwrap()[5].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[5].values().len());
    assert_eq!(Prop::BackgroundPositionY, t.as_ref().unwrap()[6].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[6].values().len());
    a = Parser::new(b"font: small-caps 24px Arial, 'Sans', monospace; ");
    t = Some(a.parse_declarations());
    assert_eq!(7usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Font, t.as_ref().unwrap()[0].prop());
    assert_eq!(8usize, t.as_ref().unwrap()[0].values().len());
    assert_eq!(Prop::FontStyle, t.as_ref().unwrap()[1].prop());
    assert_eq!(Prop::FontVariant, t.as_ref().unwrap()[2].prop());
    assert_eq!(Prop::FontWeight, t.as_ref().unwrap()[3].prop());
    assert_eq!(Prop::FontSize, t.as_ref().unwrap()[4].prop());
    assert_eq!(Prop::LineHeight, t.as_ref().unwrap()[5].prop());
    assert_eq!(Prop::FontFamily, t.as_ref().unwrap()[6].prop());
    assert_eq!(3usize, t.as_ref().unwrap()[6].values().len());
    assert_eq!(
        b"monospace",
        (t.as_ref().unwrap()[6].values()[2].identifier_text()).as_bytes()
    );
    a = Parser::new(b"{font-size: #333; color:red");
    t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"{font-size: #333; color:red");
    a.set_quirks_mode(false);
    t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"font-size {background: #333; color:red");
    t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"font-size {background: #333; color:red");
    a.set_quirks_mode(false);
    t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"font-size }background: #333; color:red");
    t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"font-size }background: #333; color:red");
    a.set_quirks_mode(false);
    t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"top:1px; {font-size: #333; color:red}");
    t = Some(a.parse_declarations());
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Top, t.as_ref().unwrap()[0].prop());
    a = Parser::new(b"top:1px; {font-size: #333; color:red}");
    a.set_quirks_mode(false);
    t = Some(a.parse_declarations());
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Top, t.as_ref().unwrap()[0].prop());
    a = Parser::new(b"display:block; 'width: 100%;\n height: 100%; color:red");
    t = Some(a.parse_declarations());
    assert_eq!(2usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Display, t.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::Color, t.as_ref().unwrap()[1].prop());
    a = Parser::new(b"display:block; 'width: 100%;\n {height: 100%; color:red}; top: 1px");
    t = Some(a.parse_declarations());
    assert_eq!(2usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::Display, t.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::Top, t.as_ref().unwrap()[1].prop());
}

#[test]
fn illegal_constructs() {
    let mut a = Parser::new(b"width: {$width}");
    let mut t = Some(a.parse_declarations());
    assert_eq!(0usize, t.as_ref().unwrap().len());
    a = Parser::new(b"font-family: \"Gill Sans MT;");
    t = Some(a.parse_declarations());
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::FontFamily, t.as_ref().unwrap()[0].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[0].values().len());
    assert_eq!(
        ValueType::String,
        t.as_ref().unwrap()[0].values()[0].lexical_type()
    );
    assert_eq!(
        b"Gill Sans MT;",
        (t.as_ref().unwrap()[0].values()[0].string_value()).as_bytes()
    );
    a = Parser::new(b"font-family: 'Gill Sans MT");
    t = Some(a.parse_declarations());
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(Prop::FontFamily, t.as_ref().unwrap()[0].prop());
    assert_eq!(1usize, t.as_ref().unwrap()[0].values().len());
    assert_eq!(
        ValueType::String,
        t.as_ref().unwrap()[0].values()[0].lexical_type()
    );
    assert_eq!(
        b"Gill Sans MT",
        (t.as_ref().unwrap()[0].values()[0].string_value()).as_bytes()
    );
}

#[test]
fn universalselector() {
    let mut p = Parser::new(b"*");
    let mut t = p.parse_simple_selectors(false);
    assert_eq!(Combinator::None, t.as_ref().unwrap().combinator());
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(SimpleSelectorType::Universal, t.as_ref().unwrap()[0].kind());
}

#[test]
fn universalselectorcondition() {
    let mut a = Parser::new(b" *[foo=bar]");
    let mut t = a.parse_simple_selectors(true);
    assert_eq!(Combinator::Descendant, t.as_ref().unwrap().combinator());
    assert_eq!(2usize, t.as_ref().unwrap().len());
    assert_eq!(SimpleSelectorType::Universal, t.as_ref().unwrap()[0].kind());
    assert_eq!(
        SimpleSelectorType::ExactAttribute,
        t.as_ref().unwrap()[1].kind()
    );
    a = Parser::new(b" *[foo=");
    t = a.parse_simple_selectors(true);
    assert!(t.is_none());
}

#[test]
fn comment_breaking_descendant_combinator() {
    let mut p = Parser::new(b" a b/*foo*/c /*foo*/d/*foo*/ e { }");
    let mut t = p.parse_ruleset();
    assert_eq!(1usize, t.as_ref().unwrap().selectors().len());
    let s = Some(&(t.as_ref().unwrap().selectors()[0]));
    assert_eq!(5usize, s.as_ref().unwrap().len());
    assert_eq!(Combinator::None, s.as_ref().unwrap()[0].combinator());
    assert_eq!(Combinator::Descendant, s.as_ref().unwrap()[1].combinator());
    assert_eq!(Combinator::Descendant, s.as_ref().unwrap()[2].combinator());
    assert_eq!(Combinator::Descendant, s.as_ref().unwrap()[3].combinator());
    assert_eq!(Combinator::Descendant, s.as_ref().unwrap()[4].combinator());
}

#[test]
fn comment_breaking_child_combinator() {
    let mut p = Parser::new(b" a >b/*f>oo*/>c /*fo>o*/>d/*f>oo*/ > e>f { }");
    let mut t = p.parse_ruleset();
    assert_eq!(1usize, t.as_ref().unwrap().selectors().len());
    let s = Some(&(t.as_ref().unwrap().selectors()[0]));
    assert_eq!(6usize, s.as_ref().unwrap().len());
    assert_eq!(Combinator::None, s.as_ref().unwrap()[0].combinator());
    assert_eq!(Combinator::Child, s.as_ref().unwrap()[1].combinator());
    assert_eq!(Combinator::Child, s.as_ref().unwrap()[2].combinator());
    assert_eq!(Combinator::Child, s.as_ref().unwrap()[3].combinator());
    assert_eq!(Combinator::Child, s.as_ref().unwrap()[4].combinator());
    assert_eq!(Combinator::Child, s.as_ref().unwrap()[5].combinator());
}

#[test]
fn ruleset_starts_with_combinator() {
    let mut p = Parser::new(b" >a { }");
    let mut t = p.parse_ruleset();
    assert!(t.is_none());
}

#[test]
fn simple_selectors() {
    let mut a = Parser::new(b"*[lang|=fr]");
    let mut t = a.parse_simple_selectors(false);
    assert_eq!(Combinator::None, t.as_ref().unwrap().combinator());
    {
        let c = Some(&(t.as_ref().unwrap()[0]));
        assert!((c).is_some());
        assert_eq!(SimpleSelectorType::Universal, c.as_ref().unwrap().kind());
    }
    {
        let c = Some(&(t.as_ref().unwrap()[1]));
        assert!((c).is_some());
        assert_eq!(
            SimpleSelectorType::BeginHyphenAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"lang", (c.as_ref().unwrap().attribute()).as_bytes());
        assert_eq!(b"fr", (c.as_ref().unwrap().value()).as_bytes());
    }
    a = Parser::new(b"> P:first_child:hover[class~='hidden'][width]#content[id*=logo][id^=logo][id$=\"logo\"][lang=en].anotherclass.moreclass #next");
    t = a.parse_simple_selectors(true);
    assert_eq!(Combinator::Child, t.as_ref().unwrap().combinator());
    let mut it = t.as_ref().unwrap().iter().rev();
    {
        let c = it.next();
        assert_eq!(SimpleSelectorType::Class, c.as_ref().unwrap().kind());
        assert_eq!(b"moreclass", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(SimpleSelectorType::Class, c.as_ref().unwrap().kind());
        assert_eq!(b"anotherclass", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(
            SimpleSelectorType::ExactAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"lang", (c.as_ref().unwrap().attribute()).as_bytes());
        assert_eq!(b"en", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(
            SimpleSelectorType::EndWithAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"id", (c.as_ref().unwrap().attribute()).as_bytes());
        assert_eq!(b"logo", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(
            SimpleSelectorType::BeginWithAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"id", (c.as_ref().unwrap().attribute()).as_bytes());
        assert_eq!(b"logo", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(
            SimpleSelectorType::SubstringAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"id", (c.as_ref().unwrap().attribute()).as_bytes());
        assert_eq!(b"logo", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(SimpleSelectorType::Id, c.as_ref().unwrap().kind());
        assert_eq!(b"content", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(
            SimpleSelectorType::ExistAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"width", (c.as_ref().unwrap().attribute()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(
            SimpleSelectorType::OneOfAttribute,
            c.as_ref().unwrap().kind()
        );
        assert_eq!(b"class", (c.as_ref().unwrap().attribute()).as_bytes());
        assert_eq!(b"hidden", (c.as_ref().unwrap().value()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(SimpleSelectorType::Pseudoclass, c.as_ref().unwrap().kind());
        assert_eq!(b"hover", (c.as_ref().unwrap().pseudoclass()).as_bytes());
    }
    {
        let c = it.next();
        assert_eq!(SimpleSelectorType::Pseudoclass, c.as_ref().unwrap().kind());
        assert_eq!(
            b"first_child",
            (c.as_ref().unwrap().pseudoclass()).as_bytes()
        );
    }
    {
        let c = it.next();
        assert_eq!(SimpleSelectorType::ElementType, c.as_ref().unwrap().kind());
        assert_eq!(b"P", (c.as_ref().unwrap().element_text()).as_bytes());
        assert_eq!(HtmlTag::P, c.as_ref().unwrap().element_type());
    }
    assert!(it.next().is_none());
}

#[test]
fn bad_simple_selectors() {
    let mut a: Parser;
    let mut t: Option<SimpleSelectors>;
    a = Parser::new(b"");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"{}");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"#");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"# {");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"#{");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"##");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"*[class=");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"*[class=hidden];");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"*[class=hidden].;");
    t = a.parse_simple_selectors(false);
    assert!(t.is_none());
    a = Parser::new(b"#a {");
    t = a.parse_simple_selectors(false);
    assert!(t.is_some());
}

#[test]
fn selectors() {
    let mut a = Parser::new(b"h1 p #id {");
    let mut t = a.parse_selectors();
    assert!(t.is_some());
    assert_eq!(1usize, t.as_ref().unwrap().len());
    assert_eq!(3usize, t.as_ref().unwrap()[0].len());
    assert_eq!(b'{', a.current_byte().unwrap());
    a = Parser::new(b" h1 p #id , div.p > h2 > div.t #id");
    t = a.parse_selectors();
    assert!(t.is_some());
    assert_eq!(2usize, t.as_ref().unwrap().len());
    assert_eq!(3usize, t.as_ref().unwrap()[0].len());
    assert_eq!(4usize, t.as_ref().unwrap()[1].len());
    assert!(a.done());
    a = Parser::new(b"/*c*/h1 p #id/*c*/,/*c*/div.p > h2 > div.t #id/*c*/");
    t = a.parse_selectors();
    assert!(t.is_some());
    assert_eq!(2usize, t.as_ref().unwrap().len());
    assert_eq!(3usize, t.as_ref().unwrap()[0].len());
    assert_eq!(4usize, t.as_ref().unwrap()[1].len());
    assert!(a.done());
    a = Parser::new(b"{}");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert_eq!(b'{', a.current_byte().unwrap());
    a = Parser::new(b"");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert!(a.done());
    a = Parser::new(b"  ,h1 p #id {");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert_eq!(b'{', a.current_byte().unwrap());
    a = Parser::new(b"  , {");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert_eq!(b'{', a.current_byte().unwrap());
    a = Parser::new(b"h1 p #id, {");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert_eq!(b'{', a.current_byte().unwrap());
    a = Parser::new(b"h1 p #id, {");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert_eq!(b'{', a.current_byte().unwrap());
    a = Parser::new(b"h1 p #id;");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert!(a.done());
    a = Parser::new(b" h1 p[class=/*{*/ #id , h2 #id");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert!(a.done());
    a = Parser::new(b" h1 #id. , h2 #id");
    t = a.parse_selectors();
    assert!(t.is_none());
    assert!(a.done());
}

#[test]
fn rulesets() {
    let mut a = Parser::new(b"h1 p #id ;");
    let mut t = a.parse_ruleset();
    assert_eq!(None, t.as_ref());
    a = Parser::new(b", { }");
    t = a.parse_ruleset();
    assert!(t.is_none());
    a = Parser::new(b", h1 p #id, { };");
    t = a.parse_ruleset();
    assert!(t.is_none());
    a = Parser::new(b"h1 p + #id { font-size: 7px; width:10pt !important;}");
    t = a.parse_ruleset();
    assert_eq!(1usize, t.as_ref().unwrap().selectors().len());
    assert_eq!(3usize, t.as_ref().unwrap().selector(0).len());
    assert_eq!(
        Combinator::Sibling,
        t.as_ref().unwrap().selectors()[0][2].combinator()
    );
    assert_eq!(2usize, t.as_ref().unwrap().declarations().len());
    assert!(!(t.as_ref().unwrap().declarations()[0].is_important()));
    assert_eq!(Prop::Width, t.as_ref().unwrap().declarations()[1].prop());
    assert!((t.as_ref().unwrap().declarations()[1].is_important()));
    a = Parser::new(b"h1 p + #id , h1:first_child { font-size: 10px; }");
    t = a.parse_ruleset();
    assert_eq!(2usize, t.as_ref().unwrap().selectors().len());
    assert_eq!(3usize, t.as_ref().unwrap().selector(0).len());
    assert_eq!(1usize, t.as_ref().unwrap().selector(1).len());
    assert_eq!(
        Combinator::Sibling,
        t.as_ref().unwrap().selectors()[0][2].combinator()
    );
    assert_eq!(1usize, t.as_ref().unwrap().declarations().len());
    assert!(!(t.as_ref().unwrap().declarations()[0].is_important()));
}

#[test]
fn atrules() {
    let mut a = Parser::new(b"@IMPORT url(assets/style.css) screen,printer;");
    let mut t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert_eq!(1usize, t.as_ref().unwrap().imports().len());
    assert_eq!(
        b"assets/style.css",
        (t.as_ref().unwrap().import(0).link()).as_bytes()
    );
    assert_eq!(2usize, t.as_ref().unwrap().import(0).media_queries().len());
    assert!((a.done()));
    a = Parser::new(b"@import url(foo.css)");
    t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert_ne!(NO_ERROR, a.errors_seen_mask());
    assert_eq!(1usize, t.as_ref().unwrap().imports().len());
    a = Parser::new(b"@charset \"ISO-8859-1\" ;");
    t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert!((a.done()));
    a = Parser::new(b"@media print,screen {\n\tbody { font-size: 10pt }\n}");
    t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert_eq!(1usize, t.as_ref().unwrap().rulesets().len());
    assert_eq!(1usize, t.as_ref().unwrap().ruleset(0).selectors().len());
    assert_eq!(2usize, t.as_ref().unwrap().ruleset(0).media_queries().len());
    assert_eq!(
        MediaQualifier::None,
        t.as_ref().unwrap().ruleset(0).media_query(0).qualifier()
    );
    assert_eq!(
        b"print",
        (t.as_ref().unwrap().ruleset(0).media_query(0).media_type()).as_bytes()
    );
    assert_eq!(
        0usize,
        t.as_ref()
            .unwrap()
            .ruleset(0)
            .media_query(0)
            .expressions()
            .len()
    );
    assert_eq!(
        MediaQualifier::None,
        t.as_ref().unwrap().ruleset(0).media_query(1).qualifier()
    );
    assert_eq!(
        b"screen",
        (t.as_ref().unwrap().ruleset(0).media_query(1).media_type()).as_bytes()
    );
    assert_eq!(
        0usize,
        t.as_ref()
            .unwrap()
            .ruleset(0)
            .media_query(1)
            .expressions()
            .len()
    );
    assert_eq!(1usize, t.as_ref().unwrap().ruleset(0).selectors()[0].len());
    assert_eq!(
        HtmlTag::Body,
        t.as_ref().unwrap().ruleset(0).selector(0)[0][0].element_type()
    );
    assert_eq!(1usize, t.as_ref().unwrap().ruleset(0).declarations().len());
    assert_eq!(
        Prop::FontSize,
        t.as_ref().unwrap().ruleset(0).declarations()[0].prop()
    );
    assert!((a.done()));
    a = Parser::new(b"@page :left { margin-left: 4cm; margin-right: 3cm; }");
    t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert_eq!(0usize, t.as_ref().unwrap().rulesets().len());
    assert!((a.done()));
    a = Parser::new(b"@media print { a { color: red; }  p { color: blue; } }");
    t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert_eq!(2usize, t.as_ref().unwrap().rulesets().len());
    assert_eq!(1usize, t.as_ref().unwrap().ruleset(0).media_queries().len());
    assert_eq!(
        b"print",
        (t.as_ref().unwrap().ruleset(0).media_query(0).media_type()).as_bytes()
    );
    assert_eq!(1usize, t.as_ref().unwrap().ruleset(1).media_queries().len());
    assert_eq!(
        b"print",
        (t.as_ref().unwrap().ruleset(1).media_query(0).media_type()).as_bytes()
    );
    let _ = t.as_ref().unwrap().to_css_string();
    a = Parser::new(b"@font-face { font-family: 'Cabin'; src: local('Wingdings'); }");
    t = Some(Stylesheet::new());
    a.parse_statement(None, t.as_mut().unwrap());
    assert_eq!(0usize, t.as_ref().unwrap().rulesets().len());
    assert_eq!(1usize, t.as_ref().unwrap().font_faces().len());
    assert_eq!(
        2usize,
        t.as_ref().unwrap().font_face(0).declarations().len()
    );
}

#[test]
fn stylesheets() {
    let mut a = Parser::new(b"\n\t@import \"mystyle.css\" all; @import url(\"mystyle.css\" );\n\tBODY {\ncolor:black !important; \nbackground: white !important; }\n* {\n\tcolor: inherit !important;\nbackground: transparent;\n}\n\n<!-- html comments * { font-size: 1 } -->\nH1 + *[REL-up] {}");
    let mut t = Some(a.parse_stylesheet());
    assert_eq!(NO_ERROR, a.errors_seen_mask());
    assert_eq!(2usize, t.as_ref().unwrap().imports().len());
    assert_eq!(
        b"mystyle.css",
        (t.as_ref().unwrap().import(0).link()).as_bytes()
    );
    assert_eq!(1usize, t.as_ref().unwrap().import(0).media_queries().len());
    assert_eq!(
        b"all",
        (t.as_ref().unwrap().import(0).media_queries()[0].media_type()).as_bytes()
    );
    assert_eq!(
        b"mystyle.css",
        (t.as_ref().unwrap().import(1).link()).as_bytes()
    );
    assert_eq!(4usize, t.as_ref().unwrap().rulesets().len());
    assert!(a.done());
}

#[test]
fn parse_raw_stylesheet_does_not_expand() {
    {
        let mut p = Parser::new(b"a { background: none; }");
        let mut stylesheet = Some(p.parse_raw_stylesheet());
        assert_eq!(1usize, stylesheet.as_ref().unwrap().rulesets().len());
        assert_eq!(
            1usize,
            stylesheet.as_ref().unwrap().ruleset(0).declarations().len()
        );
        assert_eq!(
            1usize,
            stylesheet
                .as_ref()
                .unwrap()
                .ruleset(0)
                .declaration(0)
                .values()
                .len()
        );
        assert!(p.done());
    }
    {
        let mut p = Parser::new(b"a { font: 12px verdana; }");
        let mut stylesheet = Some(p.parse_raw_stylesheet());
        assert_eq!(1usize, stylesheet.as_ref().unwrap().rulesets().len());
        assert_eq!(
            1usize,
            stylesheet.as_ref().unwrap().ruleset(0).declarations().len()
        );
        let values = stylesheet
            .as_ref()
            .unwrap()
            .ruleset(0)
            .declaration(0)
            .values();
        assert_eq!(6usize, values.len());
        assert_eq!(Ident::Normal, values[0].identifier().ident());
        assert_eq!(Ident::Normal, values[1].identifier().ident());
        assert_eq!(Ident::Normal, values[2].identifier().ident());
        assert_ulp_eq(12.0, values[3].float_value());
        assert_eq!(Unit::Px, values[3].dimension());
        assert_eq!(Ident::Normal, values[4].identifier().ident());
        assert_eq!(b"verdana", (values[5].identifier_text()).as_bytes());
        assert!(p.done());
    }
}

#[test]
fn parse_stylesheet_does_expand() {
    {
        let mut p = Parser::new(b"a { background: none; }");
        let mut stylesheet = Some(p.parse_stylesheet());
        assert_eq!(1usize, stylesheet.as_ref().unwrap().rulesets().len());
        let declarations = stylesheet.as_ref().unwrap().ruleset(0).declarations();
        assert_eq!(7usize, declarations.len());
        assert_eq!(Prop::Background, declarations[0].prop());
        assert_eq!(Prop::BackgroundColor, declarations[1].prop());
        assert_eq!(Prop::BackgroundImage, declarations[2].prop());
        assert_eq!(Prop::BackgroundRepeat, declarations[3].prop());
        assert_eq!(Prop::BackgroundAttachment, declarations[4].prop());
        assert_eq!(Prop::BackgroundPositionX, declarations[5].prop());
        assert_eq!(Prop::BackgroundPositionY, declarations[6].prop());
        assert!(p.done());
    }
    {
        let mut p = Parser::new(b"a { font: 12px verdana; }");
        let mut stylesheet = Some(p.parse_stylesheet());
        assert_eq!(1usize, stylesheet.as_ref().unwrap().rulesets().len());
        let declarations = stylesheet.as_ref().unwrap().ruleset(0).declarations();
        assert_eq!(7usize, declarations.len());
        assert_eq!(Prop::Font, declarations[0].prop());
        assert_eq!(6usize, declarations[0].values().len());
        assert_eq!(Prop::FontStyle, declarations[1].prop());
        assert_eq!(Prop::FontVariant, declarations[2].prop());
        assert_eq!(Prop::FontWeight, declarations[3].prop());
        assert_eq!(Prop::FontSize, declarations[4].prop());
        assert_eq!(Prop::LineHeight, declarations[5].prop());
        assert_eq!(Prop::FontFamily, declarations[6].prop());
    }
}

#[test]
fn percentage_colors() {
    let hundred = Value::number(100.0, Unit::Percent);
    assert_eq!(255, Parser::value_to_rgb(Some(&hundred)));
    let zero = Value::number(0.0, Unit::Percent);
    assert_eq!(0, Parser::value_to_rgb(Some(&zero)));
}

#[test]
fn value_equality() {
    let hundred = Value::number(100.0, Unit::Percent);
    let hundred2 = Value::number(100.0, Unit::Percent);
    let zero = Value::number(0.0, Unit::Percent);
    let auto_ident = Identifier::from_ident(Ident::Auto);
    let ident = Value::ident(auto_ident);
    assert!(hundred.equals(&(hundred2)));
    assert!(!(hundred.equals(&(zero))));
    assert!(!(hundred.equals(&(ident))));
}

#[test]
fn utf8_error() {
    let mut p = Parser::new(b"font-family: \"\xcb\xce\xcc\xe5\"");
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(UTF8_ERROR, p.errors_seen_mask());
}

#[test]
fn declaration_error() {
    let mut p = Parser::new(b"font-family ; ");
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(0usize, declarations.as_ref().unwrap().len());
    assert_eq!(DECLARATION_ERROR, p.errors_seen_mask());
    p = Parser::new(b"padding-top: 1.em");
    declarations = Some(p.parse_declarations());
    assert!((DECLARATION_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"color: red !ie");
    declarations = Some(p.parse_declarations());
    assert!((DECLARATION_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"color: red !important really");
    declarations = Some(p.parse_declarations());
    assert!((DECLARATION_ERROR & p.errors_seen_mask()) != 0);
}

#[test]
fn selector_error() {
    let mut p = Parser::new(b".bold: { font-weight: bold }");
    let mut stylesheet = Some(p.parse_stylesheet());
    assert_eq!(0usize, stylesheet.as_ref().unwrap().rulesets().len());
    assert!((SELECTOR_ERROR & p.errors_seen_mask()) != 0);
    let mut p2 = Parser::new(b"div:nth-child(1n) { color: red; }");
    stylesheet = Some(p2.parse_stylesheet());
    assert!((SELECTOR_ERROR & p2.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\ndiv:nth-child {color: #ff0000}\n" as &[u8]
    );
    let mut p3 = Parser::new(b"}}");
    stylesheet = Some(p3.parse_stylesheet());
    assert_eq!(0usize, stylesheet.as_ref().unwrap().rulesets().len());
    assert!((SELECTOR_ERROR & p3.errors_seen_mask()) != 0);
    let mut p4 = Parser::new(b"div[too=many=equals] { color: red; }");
    stylesheet = Some(p4.parse_stylesheet());
    assert!((SELECTOR_ERROR & p4.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\ndiv[too=\"many\"] {color: #ff0000}\n" as &[u8]
    );
}

#[test]
fn media_error() {
    let mut p = Parser::new(b"@media screen and (max-width^?`) { .a { color: red; } }");
    let mut stylesheet = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n@media not all { .a {color: #ff0000} }\n" as &[u8]
    );
    p = Parser::new(b"@media screen and (max-width^?`), print { .a { color: red; } }");
    stylesheet = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n@media not all, print { .a {color: #ff0000} }\n" as &[u8]
    );
    p = Parser::new(b"@media { .a { color: red; } }");
    stylesheet = Some(p.parse_stylesheet());
    assert!(!((MEDIA_ERROR & p.errors_seen_mask()) != 0));
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.a {color: #ff0000}\n" as &[u8]
    );
}

#[test]
fn html_comment_error() {
    let mut good = Parser::new(b"<!-- a { color: red } -->");
    let mut stylesheet = Some(good.parse_stylesheet());
    assert_eq!(NO_ERROR, good.errors_seen_mask());
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\na {color: #ff0000}\n" as &[u8]
    );
    let bad_strings = [
        b"<    a { color: red } -->" as &[u8],
        b"<!   a { color: red } -->" as &[u8],
        b"<!-  a { color: red } -->" as &[u8],
        b"<!-- a { color: red } --" as &[u8],
        b"<!-- a { color: red } ->" as &[u8],
        b"<!-- a { color: red } -" as &[u8],
        b"<>a { color: red }" as &[u8],
    ];
    for case in bad_strings {
        let mut bad = Parser::new(case);
        stylesheet = Some(bad.parse_stylesheet());
        assert!((HTML_COMMENT_ERROR & bad.errors_seen_mask()) != 0);
    }
}

#[test]
fn value_error() {
    let mut p = Parser::new(b"(12)");
    let mut value = p.parse_any();
    assert!((VALUE_ERROR & p.errors_seen_mask()) != 0);
    assert!((value.as_ref()).is_none());
}

#[test]
fn skipped_token_error() {
    let mut p = Parser::new(b"12pt @foo Arial");
    let mut values = p.parse_values(Prop::Font);
    assert!((SKIPPED_TOKEN_ERROR & p.errors_seen_mask()) != 0);
    assert_eq!(
        values_to_css_string(values.as_ref().unwrap()).as_slice(),
        b"12pt Arial" as &[u8]
    );
}

#[test]
fn charset_error() {
    let mut p = Parser::new(b"@charset \"UTF-8\";");
    let mut stylesheet = Some(p.parse_stylesheet());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n@charset \"UTF-8\";\n\n\n\n" as &[u8]
    );
    let mut p2 = Parser::new(b"@charset foobar;");
    stylesheet = Some(p2.parse_stylesheet());
    assert_eq!(CHARSET_ERROR, p2.errors_seen_mask());
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n\n" as &[u8]
    );
    let mut p3 = Parser::new(b"@charset \"UTF-8\" \"or 9\";");
    stylesheet = Some(p3.parse_stylesheet());
    assert_eq!(CHARSET_ERROR, p3.errors_seen_mask());
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n\n" as &[u8]
    );
    let mut p4 = Parser::new(b"@charset \"UTF-8\"");
    stylesheet = Some(p4.parse_stylesheet());
    assert_eq!(CHARSET_ERROR, p4.errors_seen_mask());
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n@charset \"UTF-8\";\n\n\n\n" as &[u8]
    );
}

#[test]
fn accept_correct_values() {
    let mut p = Parser::new(b"list-style-type: none");
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"list-style-type: none" as &[u8]
    );
}

#[test]
fn verbatim_declarations() {
    let mut p = Parser::new(b"color: red; z-i ndex: 42; width: 1px");
    p.set_preservation_mode(false);
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(DECLARATION_ERROR, p.errors_seen_mask());
    assert_eq!(2usize, declarations.as_ref().unwrap().len());
    assert_eq!(Prop::Color, declarations.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::Width, declarations.as_ref().unwrap()[1].prop());
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"color: #ff0000; width: 1px" as &[u8]
    );
    let mut p2 = Parser::new(b"color: red; z-i ndex: 42; width: 1px");
    p2.set_preservation_mode(true);
    declarations = Some(p2.parse_declarations());
    assert_eq!(NO_ERROR, p2.errors_seen_mask());
    assert_eq!(DECLARATION_ERROR, p2.unparseable_sections_seen_mask());
    assert_eq!(3usize, declarations.as_ref().unwrap().len());
    assert_eq!(Prop::Color, declarations.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::Unparseable, declarations.as_ref().unwrap()[1].prop());
    assert_eq!(
        b"z-i ndex: 42",
        declarations.as_ref().unwrap()[1].bytes_in_original_buffer()
    );
    assert_eq!(Prop::Width, declarations.as_ref().unwrap()[2].prop());
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"color: #ff0000; /* Unparsed declaration: */ z-i ndex: 42; width: 1px" as &[u8]
    );
}

#[test]
fn css_hacks() {
    let mut p = Parser::new(b"*border: 0px");
    p.set_preservation_mode(false);
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(DECLARATION_ERROR, p.errors_seen_mask());
    let mut p2 = Parser::new(b"*border: 0px");
    p2.set_preservation_mode(true);
    declarations = Some(p2.parse_declarations());
    assert_eq!(NO_ERROR, p2.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(Prop::Unparseable, declarations.as_ref().unwrap()[0].prop());
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"/* Unparsed declaration: */ *border: 0px" as &[u8]
    );
    let mut p3 = Parser::new(b"width: 1px; _width: 3px;");
    declarations = Some(p3.parse_declarations());
    assert_eq!(NO_ERROR, p3.errors_seen_mask());
    assert_eq!(2usize, declarations.as_ref().unwrap().len());
    assert_eq!(Prop::Width, declarations.as_ref().unwrap()[0].prop());
    assert_eq!(Prop::Other, declarations.as_ref().unwrap()[1].prop());
    assert_eq!(
        declarations.as_ref().unwrap()[1].prop_text().as_slice(),
        b"_width" as &[u8]
    );
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"width: 1px; _width: 3px" as &[u8]
    );
}

#[test]
fn function() {
    let mut p = Parser::new(b"box-shadow: -1px -2px 2px rgba(0, 13, 255, .15)");
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(4usize, declarations.as_ref().unwrap()[0].values().len());
    let val = Some(&(declarations.as_ref().unwrap()[0].values()[3]));
    assert_eq!(ValueType::Function, val.as_ref().unwrap().lexical_type());
    assert_eq!(
        UnicodeText::from_utf8(b"rgba"),
        (val.as_ref().unwrap().function_name()).clone()
    );
    let params = val.as_ref().unwrap().parameters();
    assert_eq!(4usize, params.len());
    assert_eq!(ValueType::Number, params[0].lexical_type());
    assert_eq!(0, params[0].integer_value());
    assert_eq!(ValueType::Number, params[1].lexical_type());
    assert_eq!(13, params[1].integer_value());
    assert_eq!(ValueType::Number, params[2].lexical_type());
    assert_eq!(255, params[2].integer_value());
    assert_eq!(ValueType::Number, params[3].lexical_type());
    assert_ulp_eq(0.15, params[3].float_value());
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"box-shadow: -1px -2px 2px rgba(0, 13, 255, 0.15)" as &[u8]
    );
}

#[test]
fn complex_function() {
    let mut p =
        Parser::new(b"-webkit-gradient(linear, left top, left bottom, from(#ccc), to(#ddd))");
    let mut val = p.parse_any();
    assert_eq!(ValueType::Function, val.as_ref().unwrap().lexical_type());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(
        val.as_ref().unwrap().to_css_string().as_slice(),
        b"-webkit-gradient(linear, left top, left bottom, from(#cccccc), to(#dddddd))" as &[u8]
    );
}

#[test]
fn max_nested_functions() {
    let mut p = Parser::new(b"a(b(1,2,3))");
    p.set_max_function_depth(1);
    let mut val = p.parse_any();
    assert!((val.as_ref()).is_none());
    assert!((FUNCTION_ERROR & p.errors_seen_mask()) != 0);
}

#[test]
fn counter() {
    let mut p = Parser::new(b"content: \"Section \" counter(section)");
    let mut declarations = Some(p.parse_declarations());
    assert_eq!(NO_ERROR, p.errors_seen_mask());
    assert_eq!(1usize, declarations.as_ref().unwrap().len());
    assert_eq!(2usize, declarations.as_ref().unwrap()[0].values().len());
    let val = Some(&(declarations.as_ref().unwrap()[0].values()[1]));
    assert_eq!(ValueType::Function, val.as_ref().unwrap().lexical_type());
    assert_eq!(
        UnicodeText::from_utf8(b"counter"),
        (val.as_ref().unwrap().function_name()).clone()
    );
    let params = val.as_ref().unwrap().parameters();
    assert_eq!(1usize, params.len());
    assert_eq!(ValueType::Ident, params[0].lexical_type());
    assert_eq!(
        UnicodeText::from_utf8(b"section"),
        (params[0].identifier_text()).clone()
    );
    assert_eq!(
        declarations_to_css_string(declarations.as_ref().unwrap()).as_slice(),
        b"content: \"Section \" counter(section)" as &[u8]
    );
}

#[test]
fn parse_next_import() {
    let mut parser = Parser::new(b"@IMPORT url(assets/style.css) screen,printer;");
    let mut import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(parser.done());
    if import.is_some() {
        assert_eq!(
            b"assets/style.css",
            (import.as_ref().unwrap().link()).as_bytes()
        );
        assert_eq!(2usize, import.as_ref().unwrap().media_queries().len());
    }
    parser = Parser::new(b"\n\t@import \"mystyle.css\" all; \n");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(parser.done());
    if import.is_some() {
        assert_eq!(b"mystyle.css", (import.as_ref().unwrap().link()).as_bytes());
        assert_eq!(1usize, import.as_ref().unwrap().media_queries().len());
    }
    parser = Parser::new(b"\n\t@import url(\"mystyle.css\"); \n");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(parser.done());
    if import.is_some() {
        assert_eq!(b"mystyle.css", (import.as_ref().unwrap().link()).as_bytes());
        assert_eq!(0usize, import.as_ref().unwrap().media_queries().len());
    }
    parser = Parser::new(b"*border: 0px");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_none());
    assert!(!(parser.done()));
    parser = Parser::new(b"@import \"mystyle.css\" all;\n*border: 0px");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(!(parser.done()));
    parser = Parser::new(b"@import \"mystyle.css\" all;\n@import url(\"mystyle.css\" );\n");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(!(parser.done()));
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(parser.done());
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_none());
    assert!(parser.done());
    parser =
        Parser::new(b"@import \"mystyle.css\" all;\n@import url(\"mystyle.css\" );\n*border: 0px");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(!(parser.done()));
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_some());
    assert!(!(parser.done()));
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_none());
    assert!(!(parser.done()));
    parser = Parser::new(b"@charset \"ISO-8859-1\";\n@import \"mystyle.css\" all;");
    import = parser.parse_next_import();
    assert!((import.as_ref()).is_none());
}

#[test]
fn parse_single_import() {
    let mut parser = Parser::new(b"@IMPORT url(assets/style.css) screen,printer;");
    let mut import = parser.parse_as_single_import();
    assert!((import.as_ref()).is_some());
    if import.is_some() {
        assert_eq!(
            b"assets/style.css",
            (import.as_ref().unwrap().link()).as_bytes()
        );
        assert_eq!(2usize, import.as_ref().unwrap().media_queries().len());
    }
    parser = Parser::new(b"\n\t@import \"mystyle.css\" all; \n");
    import = parser.parse_as_single_import();
    assert!((import.as_ref()).is_some());
    if import.is_some() {
        assert_eq!(b"mystyle.css", (import.as_ref().unwrap().link()).as_bytes());
        assert_eq!(1usize, import.as_ref().unwrap().media_queries().len());
    }
    parser = Parser::new(b"\n\t@import url(\"mystyle.css\"); \n");
    import = parser.parse_as_single_import();
    assert!((import.as_ref()).is_some());
    if import.is_some() {
        assert_eq!(b"mystyle.css", (import.as_ref().unwrap().link()).as_bytes());
        assert_eq!(0usize, import.as_ref().unwrap().media_queries().len());
    }
    parser = Parser::new(b"*border: 0px");
    import = parser.parse_as_single_import();
    assert!((import.as_ref()).is_none());
    parser = Parser::new(b"@import \"mystyle.css\" all;\n@import url(\"mystyle.css\" );\n");
    import = parser.parse_as_single_import();
    assert!((import.as_ref()).is_none());
    parser = Parser::new(b"@charset \"ISO-8859-1\";\n@import \"mystyle.css\" all;");
    import = parser.parse_as_single_import();
    assert!((import.as_ref()).is_none());
}

#[test]
fn media_queries() {
    let mut p = Parser::new(b"@import url(a.css);\n@import url(b.css) screen;\n@import url(c.css) NOT (max-width: 300px) and (color);\n@import url(d.css) only print and (color), not screen;\n@media { .a { color: red; } }\n@media onLy screen And (max-width: 250px) { .a { color: green } }\n.a { color: blue; }\n@media (nonsense: foo(')', \")\")) { body { color: red } }\n");
    let mut s = Some(p.parse_stylesheet());
    assert_eq!(4usize, s.as_ref().unwrap().imports().len());
    assert_eq!(0usize, s.as_ref().unwrap().import(0).media_queries().len());
    assert_eq!(1usize, s.as_ref().unwrap().import(1).media_queries().len());
    assert_eq!(
        MediaQualifier::None,
        s.as_ref().unwrap().import(1).media_queries()[0].qualifier()
    );
    assert_eq!(
        b"screen",
        (s.as_ref().unwrap().import(1).media_queries()[0].media_type()).as_bytes()
    );
    assert_eq!(
        0usize,
        s.as_ref().unwrap().import(1).media_queries()[0]
            .expressions()
            .len()
    );
    assert_eq!(1usize, s.as_ref().unwrap().import(2).media_queries().len());
    assert_eq!(
        MediaQualifier::Not,
        s.as_ref().unwrap().import(2).media_queries()[0].qualifier()
    );
    assert_eq!(
        b"",
        (s.as_ref().unwrap().import(2).media_queries()[0].media_type()).as_bytes()
    );
    assert_eq!(
        2usize,
        s.as_ref().unwrap().import(2).media_queries()[0]
            .expressions()
            .len()
    );
    assert_eq!(
        b"max-width",
        (s.as_ref().unwrap().import(2).media_queries()[0]
            .expression(0)
            .name())
        .as_bytes()
    );
    assert!(s.as_ref().unwrap().import(2).media_queries()[0]
        .expression(0)
        .has_value());
    assert_eq!(
        b"300px",
        (s.as_ref().unwrap().import(2).media_queries()[0]
            .expression(0)
            .value())
        .as_bytes()
    );
    assert_eq!(
        b"color",
        (s.as_ref().unwrap().import(2).media_queries()[0]
            .expression(1)
            .name())
        .as_bytes()
    );
    assert!(
        !(s.as_ref().unwrap().import(2).media_queries()[0]
            .expression(1)
            .has_value())
    );
    assert_eq!(2usize, s.as_ref().unwrap().import(3).media_queries().len());
    assert_eq!(
        MediaQualifier::Only,
        s.as_ref().unwrap().import(3).media_queries()[0].qualifier()
    );
    assert_eq!(
        b"print",
        (s.as_ref().unwrap().import(3).media_queries()[0].media_type()).as_bytes()
    );
    assert_eq!(
        1usize,
        s.as_ref().unwrap().import(3).media_queries()[0]
            .expressions()
            .len()
    );
    assert_eq!(
        b"color",
        (s.as_ref().unwrap().import(3).media_queries()[0]
            .expression(0)
            .name())
        .as_bytes()
    );
    assert!(
        !(s.as_ref().unwrap().import(3).media_queries()[0]
            .expression(0)
            .has_value())
    );
    assert_eq!(
        MediaQualifier::Not,
        s.as_ref().unwrap().import(3).media_queries()[1].qualifier()
    );
    assert_eq!(
        b"screen",
        (s.as_ref().unwrap().import(3).media_queries()[1].media_type()).as_bytes()
    );
    assert_eq!(
        0usize,
        s.as_ref().unwrap().import(3).media_queries()[1]
            .expressions()
            .len()
    );
    assert_eq!(4usize, s.as_ref().unwrap().rulesets().len());
    assert_eq!(0usize, s.as_ref().unwrap().ruleset(0).media_queries().len());
    assert_eq!(1usize, s.as_ref().unwrap().ruleset(1).media_queries().len());
    assert_eq!(
        MediaQualifier::Only,
        s.as_ref().unwrap().ruleset(1).media_query(0).qualifier()
    );
    assert_eq!(
        b"screen",
        (s.as_ref().unwrap().ruleset(1).media_query(0).media_type()).as_bytes()
    );
    assert_eq!(
        1usize,
        s.as_ref()
            .unwrap()
            .ruleset(1)
            .media_query(0)
            .expressions()
            .len()
    );
    assert_eq!(
        b"max-width",
        (s.as_ref()
            .unwrap()
            .ruleset(1)
            .media_query(0)
            .expression(0)
            .name())
        .as_bytes()
    );
    assert!(s
        .as_ref()
        .unwrap()
        .ruleset(1)
        .media_query(0)
        .expression(0)
        .has_value());
    assert_eq!(
        b"250px",
        (s.as_ref()
            .unwrap()
            .ruleset(1)
            .media_query(0)
            .expression(0)
            .value())
        .as_bytes()
    );
    assert_eq!(0usize, s.as_ref().unwrap().ruleset(2).media_queries().len());
    assert_eq!(1usize, s.as_ref().unwrap().ruleset(3).media_queries().len());
    assert_eq!(
        MediaQualifier::None,
        s.as_ref().unwrap().ruleset(3).media_query(0).qualifier()
    );
    assert_eq!(
        b"",
        (s.as_ref().unwrap().ruleset(3).media_query(0).media_type()).as_bytes()
    );
    assert_eq!(
        1usize,
        s.as_ref()
            .unwrap()
            .ruleset(3)
            .media_query(0)
            .expressions()
            .len()
    );
    assert_eq!(
        b"nonsense",
        (s.as_ref()
            .unwrap()
            .ruleset(3)
            .media_query(0)
            .expression(0)
            .name())
        .as_bytes()
    );
    assert!(s
        .as_ref()
        .unwrap()
        .ruleset(3)
        .media_query(0)
        .expression(0)
        .has_value());
    assert_eq!(
        b"foo(')', \")\")",
        (s.as_ref()
            .unwrap()
            .ruleset(3)
            .media_query(0)
            .expression(0)
            .value())
        .as_bytes()
    );
}

#[test]
fn invalid_media_queries() {
    let mut p: Parser;
    let mut s: Option<Stylesheet>;
    p = Parser::new(b"@media all and(color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media all (color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media all(color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media all and and (color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media all and and(color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media all and { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media and (color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
    p = Parser::new(b"@media and(color) { a { color: red; } }");
    s = Some(p.parse_stylesheet());
    assert!((MEDIA_ERROR & p.errors_seen_mask()) != 0);
}

#[test]
fn extract_charset() {
    let mut parser: Parser;
    let mut charset = UnicodeText::new();
    parser = Parser::new(b"@charset \"ISO-8859-1\" ;");
    charset = parser.extract_charset();
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(b"ISO-8859-1", (charset).as_bytes());
    parser = Parser::new(b"@charset foobar;");
    charset = parser.extract_charset();
    assert_eq!(CHARSET_ERROR, parser.errors_seen_mask());
    assert_eq!(b"", (charset).as_bytes());
    parser = Parser::new(b"@charset \"UTF-8\" \"or 9\";");
    charset = parser.extract_charset();
    assert_eq!(CHARSET_ERROR, parser.errors_seen_mask());
    assert_eq!(b"", (charset).as_bytes());
    parser = Parser::new(b"@charsets \"UTF-8\" and \"ISO-8859-1\";");
    charset = parser.extract_charset();
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(b"", (charset).as_bytes());
    parser = Parser::new(b"@IMPORT url(assets/style.css) screen,printer");
    charset = parser.extract_charset();
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(b"", (charset).as_bytes());
    parser = Parser::new(b"wotcha!");
    charset = parser.extract_charset();
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(b"", (charset).as_bytes());
}

#[test]
fn unexpected_at_rule() {
    let mut parser: Parser;
    let mut stylesheet: Option<Stylesheet>;
    parser = Parser::new(b"@creature { toughness: 2; power: 2; abilities: double-strike; protection: black green; }\n.foo {width: 1px}\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert!((AT_RULE_ERROR & parser.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.foo {width: 1px}\n" as &[u8]
    );
    parser = Parser::new(b"@creature { toughness: 2; power: 2; abilities: double-strike; protection: black green; }\n.foo {width: 1px}\n");
    parser.set_preservation_mode(true);
    stylesheet = Some(parser.parse_stylesheet());
    assert_eq!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n\n/* Unparsed region: */ @creature { toughness: 2; power: 2; abilities: double-strike; protection: black green; }\n.foo {width: 1px}\n" as &[u8]);
    parser = Parser::new(b"@page :first { margin-top: 8cm; }\n.foo { width: 1px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert!((AT_RULE_ERROR & parser.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.foo {width: 1px}\n" as &[u8]
    );
    parser = Parser::new(b"@keyframes wiggle {\n  0% {transform:rotate(6deg);}\n  50% {transform:rotate(6deg);}\n  100% {transform:rotate(6deg);}\n}\n@-webkit-keyframes wiggle {\n  0% {transform:rotate(6deg);}\n  50% {transform:rotate(6deg);}\n  100% {transform:rotate(6deg);}\n}\n.foo { width: 1px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert!((AT_RULE_ERROR & parser.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.foo {width: 1px}\n" as &[u8]
    );
    parser = Parser::new(b"@font-feature-values Jupiter Sans {\n  @swash {\n    delicate: 1;\n    flowing: 2;\n  }\n}\n.foo { width: 2px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert!((AT_RULE_ERROR & parser.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.foo {width: 2px}\n" as &[u8]
    );
    parser = Parser::new(b"@namespace foo \"http://example.com/ns/foo\";\n.foo { width: 1px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert!((AT_RULE_ERROR & parser.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.foo {width: 1px}\n" as &[u8]
    );
    parser = Parser::new(b"@use-klingon;\n.foo { width: 1px; }");
    stylesheet = Some(parser.parse_stylesheet());
    assert!((AT_RULE_ERROR & parser.errors_seen_mask()) != 0);
    assert_eq!(
        stylesheet.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.foo {width: 1px}\n" as &[u8]
    );
    parser = Parser::new(b"@media screen {\n  .bar { height: 2px; on-hover: @use-klingon}\n  .baz { height: 4px }\n}\n.foo {\n  three-dee: @three-dee { @background-lighting { azimuth: 30deg; } };\n  width: 1px;\n}\n");
    stylesheet = Some(parser.parse_stylesheet());
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
    assert_eq!(stylesheet.as_ref().unwrap().to_css_string().as_slice(), b"/* AUTHOR */\n\n\n\n@media screen { .bar {height: 2px} }\n@media screen { .baz {height: 4px} }\n.foo {width: 1px}\n" as &[u8]);
}

#[test]
fn e_o_f_media() {
    trap_eof(b"@media", MethodToTest::ParseStylesheet);
    trap_eof(b"@media ", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (", MethodToTest::ParseStylesheet);
    trap_eof(b"@media ( ", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size ", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size:", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size: ", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size: foo", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size: foo ", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size: foo)", MethodToTest::ParseStylesheet);
    trap_eof(b"@media (size: foo) ", MethodToTest::ParseStylesheet);
    trap_eof(b"@media ( size : foo ) ", MethodToTest::ParseStylesheet);
}

#[test]
fn e_o_f_other() {
    trap_eof(b".a { margin: 5", MethodToTest::ParseStylesheet);
    trap_eof(b".a { margin: 5.5", MethodToTest::ParseStylesheet);
    trap_eof(b".a { color: rgb", MethodToTest::ParseStylesheet);
    trap_eof(b".a { color: rgb(80, 80, 80", MethodToTest::ParseStylesheet);
    trap_eof(b".a[", MethodToTest::ParseStylesheet);
    trap_eof(b"", MethodToTest::ExtractCharset);
    trap_eof(b"", MethodToTest::ParseCharset);
    trap_eof(b"'foo'", MethodToTest::ParseCharset);
}

#[test]
fn skip_past_delimiter() {
    assert_eq!(b" 6 7 8 9", skip_past(b'5', b"1 2 3 4 5 6 7 8 9"));
    assert_eq!(b" 1, bar", skip_past(b',', b"foo(a, b), 1, bar"));
    assert_eq!(b" bar }", skip_past(b'}', b"foo: 'end brace: }'; } bar }"));
    assert_eq!(b" h1 { color: blue}\n", skip_past(b'}', b"@three-dee {\n  @background-lighting {n    azimuth: 30deg;\n    elevation: 190deg;\n  }\n  h1 { color: red}\n}\n} h1 { color: blue}\n"));
    assert_eq!(
        b"\nfoo4: bar4\n",
        skip_past(
            b'}',
            b"foo1: 'bar1}'\nfoo2: 'bar2}\nfoo3: bar3}\nfoo4: bar4\n"
        )
    );
    assert_eq!(b"\"}", skip_past(b'}', b"(([))])}{)'})\"'}))}\"}"));
    failure_skip_past(b'5', b"abcdef");
    failure_skip_past(b'}', b"'}");
    failure_skip_past(b'}', b"(([))])}{)'})\"'}))\"}'[]");
}

#[test]
fn skip_past_delimiter_recusive_depth() {
    let bad = vec![b'{'; 1000000];
    failure_skip_past(b'}', &bad);
}

#[test]
fn parse_media_queries() {
    let mut a = Parser::new(b"screen");
    let mut q = Some(a.parse_media_queries());
    assert!((q.as_ref()).is_some());
    assert_eq!(1usize, q.as_ref().unwrap().len());
    assert_eq!(MediaQualifier::None, q.as_ref().unwrap()[0].qualifier());
    assert_eq!(b"screen", (q.as_ref().unwrap()[0].media_type()).as_bytes());
    a = Parser::new(b"only screen");
    q = Some(a.parse_media_queries());
    assert_eq!(MediaQualifier::Only, q.as_ref().unwrap()[0].qualifier());
    assert_eq!(b"screen", (q.as_ref().unwrap()[0].media_type()).as_bytes());
    a = Parser::new(b"screen and (max-width: 640px)");
    q = Some(a.parse_media_queries());
    assert_eq!(b"screen", (q.as_ref().unwrap()[0].media_type()).as_bytes());
    assert_eq!(1usize, q.as_ref().unwrap()[0].expressions().len());
    assert_eq!(
        b"max-width",
        (q.as_ref().unwrap()[0].expression(0).name()).as_bytes()
    );
    assert!(q.as_ref().unwrap()[0].expression(0).has_value());
    assert_eq!(
        b"640px",
        (q.as_ref().unwrap()[0].expression(0).value()).as_bytes()
    );
    a = Parser::new(b"screen and (max-width:  640 px  )");
    q = Some(a.parse_media_queries());
    assert_eq!(b"screen", (q.as_ref().unwrap()[0].media_type()).as_bytes());
    assert_eq!(1usize, q.as_ref().unwrap()[0].expressions().len());
    assert_eq!(
        b"max-width",
        (q.as_ref().unwrap()[0].expression(0).name()).as_bytes()
    );
    assert!(q.as_ref().unwrap()[0].expression(0).has_value());
    assert_eq!(
        b"640 px  ",
        (q.as_ref().unwrap()[0].expression(0).value()).as_bytes()
    );
    a = Parser::new(b"not screen and (max-width: 500px), projection and (color)");
    q = Some(a.parse_media_queries());
    assert_eq!(2usize, q.as_ref().unwrap().len());
    assert_eq!(MediaQualifier::Not, q.as_ref().unwrap()[0].qualifier());
    assert_eq!(b"screen", (q.as_ref().unwrap()[0].media_type()).as_bytes());
    assert_eq!(1usize, q.as_ref().unwrap()[0].expressions().len());
    assert_eq!(
        b"max-width",
        (q.as_ref().unwrap()[0].expression(0).name()).as_bytes()
    );
    assert!(q.as_ref().unwrap()[0].expression(0).has_value());
    assert_eq!(
        b"500px",
        (q.as_ref().unwrap()[0].expression(0).value()).as_bytes()
    );
    assert_eq!(
        b"projection",
        (q.as_ref().unwrap()[1].media_type()).as_bytes()
    );
    assert_eq!(1usize, q.as_ref().unwrap()[1].expressions().len());
    assert_eq!(
        b"color",
        (q.as_ref().unwrap()[1].expression(0).name()).as_bytes()
    );
    assert!(!(q.as_ref().unwrap()[1].expression(0).has_value()));
    a = Parser::new(b"");
    q = Some(a.parse_media_queries());
    assert!((q.as_ref()).is_some());
    assert_eq!(0usize, q.as_ref().unwrap().len());
    a = Parser::new(b"foobar");
    q = Some(a.parse_media_queries());
    assert_eq!(1usize, q.as_ref().unwrap().len());
    assert_eq!(b"foobar", (q.as_ref().unwrap()[0].media_type()).as_bytes());
    a = Parser::new(b"screen and (max-width: 640px)");
    q = Some(a.parse_media_queries());
    assert_eq!(
        media_queries_to_css_string(q.as_ref().unwrap()).as_slice(),
        b"screen and (max-width: 640px)" as &[u8]
    );
    a = Parser::new(b"screen (max-width: 640px)");
    q = Some(a.parse_media_queries());
    assert_eq!(
        media_queries_to_css_string(q.as_ref().unwrap()).as_slice(),
        b"not all" as &[u8]
    );
}

#[test]
fn import_in_middle() {
    let mut p = Parser::new(b".a { color: red; }\n@import url('foo.css');\n.b { color: blue; }\n");
    let mut s = Some(p.parse_stylesheet());
    assert_eq!(0usize, s.as_ref().unwrap().imports().len());
    assert_eq!(2usize, s.as_ref().unwrap().rulesets().len());
    assert!((IMPORT_ERROR & p.errors_seen_mask()) != 0);
    assert_eq!(
        s.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n\n.a {color: #ff0000}\n.b {color: #0000ff}\n" as &[u8]
    );
    p = Parser::new(
        b"@font-face { font-family: 'InFront'; }\n@import url('foo.css');\n.b { color: blue; }\n",
    );
    s = Some(p.parse_stylesheet());
    assert_eq!(0usize, s.as_ref().unwrap().imports().len());
    assert_eq!(1usize, s.as_ref().unwrap().font_faces().len());
    assert_eq!(1usize, s.as_ref().unwrap().rulesets().len());
    assert!((IMPORT_ERROR & p.errors_seen_mask()) != 0);
    assert_eq!(
        s.as_ref().unwrap().to_css_string().as_slice(),
        b"/* AUTHOR */\n\n\n@font-face { font-family: \"InFront\" }\n.b {color: #0000ff}\n"
            as &[u8]
    );
}

#[test]
fn parse_any_parens() {
    let mut p = Parser::new(b"(2 + 3) 9 7)");
    let mut value = p.parse_any();
    assert_eq!(b" 9 7)", p.remaining());
}

#[test]
fn bad_partial_import() {
    let kBadPartialImport = b"@import url(R\xd5\x9b";
    let mut parser = Parser::new(kBadPartialImport);
    let _ = parser.parse_stylesheet();
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
}

#[test]
fn bad_partial_import_encoding() {
    let kBadPartialImportEncoding = b"@import url(R\xd5";
    let mut parser = Parser::new(kBadPartialImportEncoding);
    let _ = parser.parse_stylesheet();
    assert_ne!(NO_ERROR, parser.errors_seen_mask());
}
