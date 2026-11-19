//! Recursive-descent CSS parser (`parser.{h,cc}`).
//!
//! Parses byte slices (which may hold invalid UTF-8) into the [`ast`]
//! tree. Quirks mode defaults on; preservation mode off. Errors set
//! mask bits and remember the first [`MAX_ERRORS_REMEMBERED`] messages.
#![forbid(unsafe_code)]

use crate::ast::{
    error_number, Declaration, Declarations, ErrorInfo, FontFace, Import, Ruleset, RulesetType,
    Stylesheet, UnparsedRegion, AT_RULE_ERROR, BLOCK_ERROR, CHARSET_ERROR, CSS_COMMENT_ERROR,
    DECLARATION_ERROR, FUNCTION_ERROR, HTML_COMMENT_ERROR, IMPORT_ERROR, MEDIA_ERROR, NUMBER_ERROR,
    RULESET_ERROR, SELECTOR_ERROR, SKIPPED_TOKEN_ERROR, VALUE_ERROR,
};
use crate::color::HtmlColor;
use crate::identifier::{Ident, Identifier};
use crate::media::{MediaExpression, MediaQualifier, MediaQueries, MediaQuery};
use crate::property::{Prop, Property};
use crate::selector::{
    Combinator, Selector, Selectors, SimpleSelector, SimpleSelectorType, SimpleSelectors,
};
use crate::string_util::{parse_double, string_case_equals};
use crate::unicode_text::{decode_rune, is_interchange_valid, UnicodeText, RUNE_ERROR};
use crate::value::{FunctionParameters, Separator, Unit, Value, ValueType, Values};

/// Remembered error cap (`kMaxErrorsRemembered`).
pub const MAX_ERRORS_REMEMBERED: usize = 16;
/// Default nested-function depth (`kDefaultMaxFunctionDepth`).
pub const DEFAULT_MAX_FUNCTION_DEPTH: i32 = 10;
/// Error context bytes (`kErrorContext`).
const ERROR_CONTEXT: usize = 20;

/// Media-query copy budget (`MaxMediaQueriesBytes`).
#[must_use]
pub fn max_media_queries_bytes(css_bytes: i64) -> i64 {
    (1 << 20) + 64 * css_bytes
}

/// CSS whitespace (`IsSpace`): space, tab, CR, LF, FF only.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0C)
}

fn is_ascii(byte: u8) -> bool {
    byte < 0x80
}

/// Hex digit value (`DeHex`), or -1.
fn dehex(byte: u8) -> i32 {
    match byte {
        b'0'..=b'9' => i32::from(byte - b'0'),
        b'A'..=b'F' => i32::from(byte - b'A') + 10,
        b'a'..=b'f' => i32::from(byte - b'a') + 10,
        _ => -1,
    }
}

/// Whether a byte can start an identifier scan (`StartsIdent`).
fn starts_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || !is_ascii(byte)
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod parser_tests;

/// The parser (`Parser`).
#[derive(Debug)]
pub struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    quirks_mode: bool,
    preservation_mode: bool,
    max_function_depth: i32,
    media_queries_bytes: i64,
    errors_seen_mask: u64,
    unparseable_sections_seen_mask: u64,
    errors_seen: Vec<ErrorInfo>,
}

impl<'a> Parser<'a> {
    /// Borrows the input to parse.
    #[must_use]
    pub fn new(input: &'a [u8]) -> Parser<'a> {
        Parser {
            input,
            pos: 0,
            quirks_mode: true,
            preservation_mode: false,
            max_function_depth: DEFAULT_MAX_FUNCTION_DEPTH,
            media_queries_bytes: 0,
            errors_seen_mask: 0,
            unparseable_sections_seen_mask: 0,
            errors_seen: Vec::new(),
        }
    }

    /// Current offset (`CurrentOffset`).
    #[must_use]
    pub fn current_offset(&self) -> usize {
        self.pos
    }

    /// Whether input is exhausted (`Done`).
    #[must_use]
    pub fn done(&self) -> bool {
        debug_assert!(self.pos <= self.input.len());
        self.pos >= self.input.len()
    }

    /// Byte at the cursor, if any (for `*in_` reads in tests).
    #[cfg(test)]
    pub(crate) fn current_byte(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    /// Unconsumed input (for `in_` reads in tests).
    #[cfg(test)]
    pub(crate) fn remaining(&self) -> &[u8] {
        &self.input[self.pos..]
    }

    /// Quirks mode flag.
    #[must_use]
    pub fn quirks_mode(&self) -> bool {
        self.quirks_mode
    }

    /// Sets quirks mode (`set_quirks_mode`).
    pub fn set_quirks_mode(&mut self, quirks: bool) {
        self.quirks_mode = quirks;
    }

    /// Preservation mode flag.
    #[must_use]
    pub fn preservation_mode(&self) -> bool {
        self.preservation_mode
    }

    /// Sets preservation mode (`set_preservation_mode`).
    pub fn set_preservation_mode(&mut self, preservation: bool) {
        self.preservation_mode = preservation;
    }

    /// Maximum function depth.
    #[must_use]
    pub fn max_function_depth(&self) -> i32 {
        self.max_function_depth
    }

    /// Sets maximum function depth (`set_max_function_depth`).
    pub fn set_max_function_depth(&mut self, depth: i32) {
        self.max_function_depth = depth;
    }

    /// Error mask (`errors_seen_mask()`).
    #[must_use]
    pub fn errors_seen_mask(&self) -> u64 {
        self.errors_seen_mask
    }

    /// Unparseable-sections mask (`unparseable_sections_seen_mask()`).
    #[must_use]
    pub fn unparseable_sections_seen_mask(&self) -> u64 {
        self.unparseable_sections_seen_mask
    }

    /// Remembered errors (`errors_seen()`).
    #[must_use]
    pub fn errors_seen(&self) -> &[ErrorInfo] {
        &self.errors_seen
    }

    /// Records an error (`ReportParsingError`).
    fn report(&mut self, flag: u64, message: &[u8]) {
        self.errors_seen_mask |= flag;
        let begin = self.pos.saturating_sub(ERROR_CONTEXT);
        let end = (self.pos + ERROR_CONTEXT).min(self.input.len());
        let mut full = Vec::new();
        full.extend_from_slice(message);
        full.extend_from_slice(b" at byte ");
        full.extend_from_slice(self.pos.to_string().as_bytes());
        full.extend_from_slice(b" \"...");
        full.extend_from_slice(&self.input[begin..end]);
        full.extend_from_slice(b"...\"");
        if self.errors_seen.len() < MAX_ERRORS_REMEMBERED {
            self.errors_seen.push(ErrorInfo {
                error_num: error_number(flag),
                byte_offset: self.pos,
                message: full,
            });
        }
    }

    fn reportf(&mut self, flag: u64, message: String) {
        self.report(flag, message.as_bytes());
    }

    /// Reports with a raw byte suffix (`%c` over bytes).
    fn report_byte(&mut self, flag: u64, prefix: &[u8], byte: u8) {
        let mut message = prefix.to_vec();
        message.push(byte);
        self.report(flag, &message);
    }

    /// Skips whitespace and comments (`SkipSpace`).
    pub(crate) fn skip_space(&mut self) {
        while self.pos < self.input.len() {
            if is_space(self.input[self.pos]) {
                self.pos += 1;
            } else if self.input[self.pos..].starts_with(b"/*") {
                self.skip_comment();
            } else {
                return;
            }
        }
    }

    /// Skips a comment (`SkipComment`).
    pub(crate) fn skip_comment(&mut self) {
        debug_assert!(self.input[self.pos..].starts_with(b"/*"));
        self.pos += 2;
        while self.pos + 1 < self.input.len() {
            if self.input[self.pos] == b'*' && self.input[self.pos + 1] == b'/' {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
        self.report(CSS_COMMENT_ERROR, b"Unexpected EOF in CSS comment.");
        self.pos = self.input.len();
    }

    /// Skips one token (`SkipNextToken`).
    pub(crate) fn skip_next_token(&mut self) {
        self.skip_space();
        if self.done() {
            return;
        }
        match self.input[self.pos] {
            b'\'' => {
                self.parse_string(b'\'');
            }
            b'"' => {
                self.parse_string(b'"');
            }
            b'\\' => {
                self.parse_escape();
            }
            _ => {
                self.pos += 1;
            }
        }
    }

    /// Skips a matched block (`SkipMatching`).
    pub(crate) fn skip_matching(&mut self) -> bool {
        debug_assert!(matches!(self.input[self.pos], b'{' | b'[' | b'('));
        self.report(BLOCK_ERROR, b"Ignoring {}, [] or () block.");
        let mut stack = Vec::new();
        match self.input[self.pos] {
            b'(' => {
                self.pos += 1;
                stack.push(b')');
            }
            b'[' => {
                self.pos += 1;
                stack.push(b']');
            }
            b'{' => {
                self.pos += 1;
                stack.push(b'}');
            }
            _ => return false,
        }
        self.skip_space();
        while self.pos < self.input.len() {
            if self.input[self.pos] == *stack.last().unwrap_or(&0) {
                self.pos += 1;
                stack.pop();
                if stack.is_empty() {
                    return true;
                }
            } else {
                match self.input[self.pos] {
                    b'(' => {
                        self.pos += 1;
                        stack.push(b')');
                    }
                    b'[' => {
                        self.pos += 1;
                        stack.push(b']');
                    }
                    b'{' => {
                        self.pos += 1;
                        stack.push(b'}');
                    }
                    _ => {
                        self.skip_next_token();
                    }
                }
            }
            self.skip_space();
        }
        false
    }

    /// Skips to a delimiter (`SkipPastDelimiter`).
    pub(crate) fn skip_past_delimiter(&mut self, delimiter: u8) -> bool {
        self.skip_space();
        while self.pos < self.input.len() {
            if self.input[self.pos] == delimiter {
                self.pos += 1;
                return true;
            }
            match self.input[self.pos] {
                b'{' | b'[' | b'(' => {
                    self.skip_matching();
                }
                _ => {
                    self.skip_next_token();
                }
            }
            self.skip_space();
        }
        false
    }

    /// Skips to the next value token (`SkipToNextAny`).
    pub(crate) fn skip_to_next_any(&mut self) -> bool {
        self.skip_space();
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b'{' => {
                    self.report(SKIPPED_TOKEN_ERROR, b"Ignoring block between tokens.");
                    self.skip_matching();
                }
                b'@' => {
                    self.report(SKIPPED_TOKEN_ERROR, b"Ignoring @ident between tokens.");
                    self.pos += 1;
                    self.parse_ident();
                }
                b';' | b'}' | b'!' => return false,
                _ => return true,
            }
            self.skip_space();
        }
        false
    }

    /// Skips an unknown at-rule (`SkipToAtRuleEnd`).
    pub(crate) fn skip_to_at_rule_end(&mut self) -> bool {
        self.skip_space();
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b'}' => return true,
                b';' => {
                    self.pos += 1;
                    return true;
                }
                b'{' => return self.skip_matching(),
                b'[' | b'(' => {
                    self.skip_matching();
                }
                _ => {
                    self.skip_next_token();
                }
            }
            self.skip_space();
        }
        false
    }

    /// Skips one media query (`SkipToMediaQueryEnd`).
    pub(crate) fn skip_to_media_query_end(&mut self) {
        self.skip_space();
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b',' | b'{' | b';' | b'}' => return,
                b'[' | b'(' => {
                    self.skip_matching();
                }
                _ => {
                    self.parse_any();
                }
            }
            self.skip_space();
        }
    }

    /// Parses an identifier (`ParseIdent`).
    pub(crate) fn parse_ident(&mut self) -> UnicodeText {
        let mut text = UnicodeText::new();
        while self.pos < self.input.len() {
            let byte = self.input[self.pos];
            if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
                text.push_codepoint(u32::from(byte));
                self.pos += 1;
            } else if !is_ascii(byte) {
                let (rune, len) = decode_rune(&self.input[self.pos..]);
                if len > 0 && rune != RUNE_ERROR {
                    if rune >= 161 {
                        text.push_codepoint(rune);
                        self.pos += len;
                    } else {
                        return text;
                    }
                } else {
                    self.report(crate::ast::UTF8_ERROR, b"UTF8 parsing error in identifier");
                    self.pos += 1;
                }
            } else if byte == b'\\' {
                text.push_codepoint(self.parse_escape());
            } else {
                return text;
            }
        }
        text
    }

    /// Parses an escape, returning its codepoint (`ParseEscape`).
    pub(crate) fn parse_escape(&mut self) -> u32 {
        self.skip_space();
        debug_assert!(self.pos < self.input.len());
        debug_assert_eq!(self.input[self.pos], b'\\');
        self.pos += 1;
        if self.done() {
            return u32::from(b'\\');
        }
        let mut codepoint = 0u32;
        if dehex(self.input[self.pos]) == -1 {
            let (rune, len) = decode_rune(&self.input[self.pos..]);
            if len > 0 && rune != RUNE_ERROR {
                self.pos += len;
            } else {
                self.report(crate::ast::UTF8_ERROR, b"UTF8 parsing error");
                self.pos += 1;
            }
            codepoint = rune;
        } else {
            for _ in 0..6 {
                if self.pos >= self.input.len() {
                    break;
                }
                let digit = dehex(self.input[self.pos]);
                if digit == -1 {
                    break;
                }
                self.pos += 1;
                codepoint = (codepoint << 4) | digit as u32;
            }
            if self.input[self.pos..].starts_with(b"\r\n") {
                self.pos += 2;
            } else if self.pos < self.input.len() && is_space(self.input[self.pos]) {
                self.pos += 1;
            }
        }
        if !is_interchange_valid(codepoint) {
            self.reportf(
                crate::ast::UTF8_ERROR,
                format!("Invalid CSS-escaped Unicode value: 0x{codepoint:X}"),
            );
            codepoint = u32::from(b' ');
        }
        codepoint
    }

    /// Parses a string (`ParseString`).
    pub(crate) fn parse_string(&mut self, delimiter: u8) -> UnicodeText {
        self.skip_space();
        debug_assert!(self.pos < self.input.len());
        debug_assert_eq!(self.input[self.pos], delimiter);
        self.pos += 1;
        if self.done() {
            return UnicodeText::new();
        }
        let mut text = UnicodeText::new();
        while self.pos < self.input.len() {
            let byte = self.input[self.pos];
            if byte == delimiter {
                self.pos += 1;
                return text;
            }
            if byte == b'\n' {
                return text;
            }
            if byte == b'\\' {
                if self.pos + 1 < self.input.len() && self.input[self.pos + 1] == b'\n' {
                    self.pos += 2;
                } else {
                    text.push_codepoint(self.parse_escape());
                }
                continue;
            }
            if !is_ascii(byte) {
                let (rune, len) = decode_rune(&self.input[self.pos..]);
                if len > 0 && rune != RUNE_ERROR {
                    text.push_codepoint(rune);
                    self.pos += len;
                } else {
                    self.report(crate::ast::UTF8_ERROR, b"UTF8 parsing error in string");
                    self.pos += 1;
                }
            } else {
                text.push_codepoint(u32::from(byte));
                self.pos += 1;
            }
        }
        text
    }

    /// Parses a string or identifier (`ParseStringOrIdent`).
    pub(crate) fn parse_string_or_ident(&mut self) -> UnicodeText {
        self.skip_space();
        if self.done() {
            return UnicodeText::new();
        }
        match self.input[self.pos] {
            b'\'' => self.parse_string(b'\''),
            b'"' => self.parse_string(b'"'),
            _ => self.parse_ident(),
        }
    }

    /// Parses a string value (`ParseStringValue`).
    pub(crate) fn parse_string_value(&mut self, delimiter: u8) -> Value {
        let start = self.pos;
        let contents = self.parse_string(delimiter);
        let mut value = Value::text(ValueType::String, contents);
        if self.preservation_mode {
            value.set_bytes_in_original_buffer(&self.input[start..self.pos]);
        }
        value
    }

    /// Parses a number with unit (`ParseNumber`).
    pub(crate) fn parse_number(&mut self) -> Option<Value> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let begin = self.pos;
        if self.input[self.pos] == b'-' || self.input[self.pos] == b'+' {
            self.pos += 1;
        }
        while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos + 1 < self.input.len()
            && self.input[self.pos] == b'.'
            && self.input[self.pos + 1].is_ascii_digit()
        {
            self.pos += 1;
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
        }
        let num = if self.pos == begin {
            None
        } else {
            parse_double(&self.input[begin..self.pos])
        };
        let Some(num) = num else {
            let mut message = b"Failed to parse number ".to_vec();
            message.extend_from_slice(&self.input[begin..self.pos]);
            self.report(NUMBER_ERROR, &message);
            return None;
        };
        let verbatim = (begin, self.pos);
        let mut value = if self.done() {
            Value::number(num, Unit::NoUnit)
        } else if self.input[self.pos] == b'%' {
            self.pos += 1;
            Value::number(num, Unit::Percent)
        } else if starts_ident(self.input[self.pos]) {
            let unit = self.parse_ident();
            Value::number_with_unit(num, &unit)
        } else {
            Value::number(num, Unit::NoUnit)
        };
        if self.preservation_mode {
            value.set_bytes_in_original_buffer(&self.input[verbatim.0..verbatim.1]);
        }
        Some(value)
    }

    /// Parses a color (`ParseColor`).
    pub(crate) fn parse_color(&mut self) -> HtmlColor {
        self.skip_space();
        if self.done() {
            return HtmlColor::undefined();
        }
        let mut hexdigits = [0u8; 6];
        let mut count = 0;
        let old_pos = self.pos;
        if self.input[self.pos] == b'"' || self.input[self.pos] == b'\'' {
            self.pos += 1;
            if self.done() {
                return HtmlColor::undefined();
            }
        }
        let rgb_valid = self.quirks_mode || self.input[self.pos] == b'#';
        if self.input[self.pos] == b'#' {
            self.pos += 1;
        }
        while self.pos < self.input.len() && count < 6 {
            let digit = dehex(self.input[self.pos]);
            if digit == -1 {
                break;
            }
            hexdigits[count] = digit as u8;
            count += 1;
            self.pos += 1;
        }
        if self.input[old_pos] == b'"' || self.input[old_pos] == b'\'' {
            if self.done() || self.input[self.pos] != self.input[old_pos] {
                count = 0;
            } else {
                self.pos += 1;
            }
        }
        if count == 3 || count == 6 {
            if !self.done() && (self.input[self.pos] == b'%' || starts_ident(self.input[self.pos]))
            {
                return HtmlColor::undefined();
            } else if !rgb_valid {
                if self.preservation_mode {
                    self.report(VALUE_ERROR, b"Quirks-mode color encountered");
                }
                return HtmlColor::undefined();
            }
        }
        if count == 3 {
            return HtmlColor::rgb(
                hexdigits[0] | hexdigits[0] << 4,
                hexdigits[1] | hexdigits[1] << 4,
                hexdigits[2] | hexdigits[2] << 4,
            );
        } else if count == 6 {
            return HtmlColor::rgb(
                hexdigits[1] | hexdigits[0] << 4,
                hexdigits[3] | hexdigits[2] << 4,
                hexdigits[5] | hexdigits[4] << 4,
            );
        }
        self.pos = old_pos;
        let mut name_valid = true;
        debug_assert!(!self.done());
        if self.input[self.pos] == b'#' {
            self.pos += 1;
            name_valid = false;
        }
        let ident = self.parse_string_or_ident();
        let mut color = HtmlColor::undefined();
        if name_valid {
            color = HtmlColor::from_name(ident.as_bytes());
            if !color.is_defined() && !self.preservation_mode {
                if let Some(system) = crate::color::system_color(ident.as_str()) {
                    color = system;
                }
            }
        }
        color
    }

    /// Parses function parameters (`ParseFunction`).
    pub(crate) fn parse_function(&mut self, max_function_depth: i32) -> Option<FunctionParameters> {
        let mut params = FunctionParameters::new();
        self.skip_space();
        let mut separator = Separator::Space;
        while !self.done() {
            match self.input[self.pos] {
                b')' => return Some(params),
                b',' => {
                    separator = Separator::Comma;
                    self.pos += 1;
                }
                b' ' => {
                    self.pos += 1;
                }
                _ => {
                    let Some(value) = self.parse_any_with_function_depth(max_function_depth) else {
                        self.report(FUNCTION_ERROR, b"Cannot parse parameter in function");
                        return None;
                    };
                    if !self.done()
                        && self.input[self.pos] != b' '
                        && self.input[self.pos] != b','
                        && self.input[self.pos] != b')'
                    {
                        let byte = self.input[self.pos];
                        let mut message = b"Function parameter contains unexpected char '".to_vec();
                        message.push(byte);
                        message.push(b'\'');
                        self.report(FUNCTION_ERROR, &message);
                        return None;
                    }
                    params.add(separator, value);
                    separator = Separator::Space;
                }
            }
            self.skip_space();
        }
        None
    }

    /// Converts a number/percentage to an RGB byte (`ValueToRGB`).
    pub(crate) fn value_to_rgb(value: Option<&Value>) -> u8 {
        let mut result = 0i32;
        if let Some(value) = value {
            if value.lexical_type() == ValueType::Number {
                if value.dimension() == Unit::Percent {
                    result = (value.float_value() / 100.0 * 255.0) as i32;
                } else {
                    result = value.integer_value();
                }
            }
        }
        result.clamp(0, 255) as u8
    }

    /// Parses `rgb()` contents (`ParseRgbColor`).
    pub(crate) fn parse_rgb_color(&mut self) -> Option<Value> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let mut rgb = [0u8; 3];
        for (index, slot) in rgb.iter_mut().enumerate() {
            let value = self.parse_number()?;
            if value.lexical_type() != ValueType::Number
                || (value.dimension() != Unit::Percent && value.dimension() != Unit::NoUnit)
            {
                break;
            }
            *slot = Parser::value_to_rgb(Some(&value));
            self.skip_space();
            if self.done()
                || (self.input[self.pos] != b',' && self.input[self.pos] != b')')
                || (self.input[self.pos] == b')' && index != 2)
            {
                break;
            }
            if self.input[self.pos] == b')' {
                return Some(Value::color(HtmlColor::rgb(rgb[0], rgb[1], rgb[2])));
            }
            debug_assert_eq!(self.input[self.pos], b',');
            self.pos += 1;
        }
        None
    }

    /// Parses `url()` contents (`ParseUrl`).
    pub(crate) fn parse_url(&mut self) -> Option<Value> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let mut text = UnicodeText::new();
        if self.input[self.pos] == b'\'' {
            text = self.parse_string(b'\'');
        } else if self.input[self.pos] == b'"' {
            text = self.parse_string(b'"');
        } else {
            while self.pos < self.input.len() {
                let byte = self.input[self.pos];
                if is_space(byte) || byte == b')' {
                    break;
                } else if byte == b'\\' {
                    text.push_codepoint(self.parse_escape());
                } else if !is_ascii(byte) {
                    let (rune, len) = decode_rune(&self.input[self.pos..]);
                    if len > 0 && rune != RUNE_ERROR {
                        text.push_codepoint(rune);
                        self.pos += len;
                    } else {
                        self.report(crate::ast::UTF8_ERROR, b"UTF8 parsing error in URL");
                        self.pos += 1;
                    }
                } else {
                    text.push_codepoint(u32::from(byte));
                    self.pos += 1;
                }
            }
        }
        self.skip_space();
        if !self.done() && self.input[self.pos] == b')' {
            return Some(Value::text(ValueType::Uri, text));
        }
        None
    }

    /// Parses a value that may be a color (`ParseAnyExpectingColor`).
    pub(crate) fn parse_any_expecting_color(&mut self) -> Option<Value> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let old_pos = self.pos;
        let color = self.parse_color();
        if color.is_defined() {
            return Some(Value::color(color));
        }
        self.pos = old_pos;
        self.parse_any()
    }

    /// Parses a value (`ParseAny`).
    pub(crate) fn parse_any(&mut self) -> Option<Value> {
        self.parse_any_with_function_depth(self.max_function_depth)
    }

    /// Parses a value with a depth limit (`ParseAnyWithFunctionDepth`).
    pub(crate) fn parse_any_with_function_depth(
        &mut self,
        max_function_depth: i32,
    ) -> Option<Value> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let old_pos = self.pos;
        let mut result = None;
        match self.input[self.pos] {
            b'0'..=b'9' | b'.' => {
                result = self.parse_number();
            }
            b'(' | b'[' => {
                let byte = self.input[self.pos];
                self.report_byte(VALUE_ERROR, b"Unsupported value starting with ", byte);
                let delimiter = if byte == b'(' { b')' } else { b']' };
                self.pos += 1;
                self.skip_past_delimiter(delimiter);
            }
            b'"' => {
                result = Some(self.parse_string_value(b'"'));
            }
            b'\'' => {
                result = Some(self.parse_string_value(b'\''));
            }
            b'#' => {
                let color = self.parse_color();
                if color.is_defined() {
                    result = Some(Value::color(color));
                }
            }
            b',' => {
                result = Some(Value::token(ValueType::Comma));
                self.pos += 1;
            }
            b'+' => {
                result = self.parse_number();
            }
            b'-' if self.pos < self.input.len() - 1
                && (self.input[self.pos + 1].is_ascii_digit()
                    || self.input[self.pos + 1] == b'.') =>
            {
                result = self.parse_number();
            }
            b'-' => {
                result = self.parse_ident_or_function(max_function_depth);
            }
            _ => {
                result = self.parse_ident_or_function(max_function_depth);
            }
        }
        if result.is_none() && self.pos == old_pos {
            self.report(VALUE_ERROR, b"Ignoring chars in value.");
            self.pos += 1;
        }
        result
    }

    /// Parses an identifier or function call (the `default:` arm).
    pub(crate) fn parse_ident_or_function(&mut self, max_function_depth: i32) -> Option<Value> {
        let id = self.parse_ident();
        if id.is_empty() {
            return None;
        }
        if self.done() || self.input[self.pos] != b'(' {
            return Some(Value::ident(Identifier::from_text(&id)));
        }
        self.pos += 1;
        let mut result = None;
        if max_function_depth > 0 {
            if string_case_equals(id.as_bytes(), b"url") {
                result = self.parse_url();
            } else if string_case_equals(id.as_bytes(), b"rgb") {
                result = self.parse_rgb_color();
            } else if string_case_equals(id.as_bytes(), b"rect") {
                match self.parse_function(max_function_depth - 1) {
                    Some(params) if params.len() == 4 => {
                        result = Some(Value::rect(params));
                    }
                    _ => {
                        self.report(
                            FUNCTION_ERROR,
                            b"Could not parse parameters for function rect",
                        );
                    }
                }
            } else {
                match self.parse_function(max_function_depth - 1) {
                    Some(params) => {
                        result = Some(Value::function(id.clone(), params));
                    }
                    None => {
                        let mut message =
                            b"Could not parse function parameters for function ".to_vec();
                        message.extend_from_slice(id.as_bytes());
                        self.report(FUNCTION_ERROR, &message);
                    }
                }
            }
            self.skip_space();
            if !self.done() && self.input[self.pos] != b')' {
                self.report(FUNCTION_ERROR, b"Ignored chars at end of function.");
            }
        } else {
            self.report(FUNCTION_ERROR, b"Functions nested too deeply.");
        }
        self.skip_past_delimiter(b')');
        result
    }
}

impl<'a> Parser<'a> {
    /// Parses a value list (`ParseValues`).
    pub(crate) fn parse_values(&mut self, prop: Prop) -> Option<Values> {
        self.skip_space();
        if self.done() {
            return Some(Vec::new());
        }
        let expecting_color = is_prop_expecting_color(prop);
        let mut values = Values::new();
        while self.skip_to_next_any() {
            let value = if expecting_color {
                self.parse_any_expecting_color()
            } else {
                self.parse_any()
            };
            values.push(value?);
        }
        if values.is_empty() {
            None
        } else {
            Some(values)
        }
    }

    /// Parses a font family list (`ParseFontFamily`).
    pub(crate) fn parse_font_family(&mut self, values: &mut Values) -> bool {
        self.skip_space();
        if self.done() {
            return true;
        }
        loop {
            let old_pos = self.pos;
            let value = self.parse_any();
            let Some(value) = value else {
                self.report(VALUE_ERROR, b"Unexpected token in font-family.");
                self.pos = old_pos;
                return false;
            };
            match value.lexical_type() {
                ValueType::String => {
                    values.push(value);
                }
                ValueType::Ident => {
                    let mut family = UnicodeText::new();
                    family.append(&value.identifier_text());
                    while self.skip_to_next_any() && !self.done() && self.input[self.pos] != b',' {
                        let inner_old = self.pos;
                        let next = self.parse_any();
                        match next {
                            Some(next) if next.lexical_type() == ValueType::Ident => {
                                family.push_codepoint(u32::from(b' '));
                                family.append(&next.identifier_text());
                            }
                            _ => {
                                self.report(
                                    VALUE_ERROR,
                                    b"Unexpected token after identifier in font-family.",
                                );
                                self.pos = inner_old;
                                return false;
                            }
                        }
                    }
                    values.push(Value::ident(Identifier::from_text(&family)));
                }
                _ => {
                    self.report(VALUE_ERROR, b"Unexpected token in font-family.");
                    return false;
                }
            }
            self.skip_space();
            if !self.done() && self.input[self.pos] == b',' {
                self.pos += 1;
            } else {
                return true;
            }
        }
    }

    /// Parses a font shorthand (`ParseFont`).
    pub(crate) fn parse_font(&mut self) -> Option<Values> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let mut values = Values::new();
        if !self.skip_to_next_any() {
            return None;
        }
        let mut current = self.parse_any()?;
        if current.lexical_type() == ValueType::Ident {
            match current.identifier().ident() {
                Ident::Caption
                | Ident::Icon
                | Ident::Menu
                | Ident::MessageBox
                | Ident::SmallCaption
                | Ident::StatusBar
                | Ident::Inherit => {
                    if self.skip_to_next_any() {
                        self.report(VALUE_ERROR, b"Font has incorrect values.");
                        return None;
                    }
                    values.push(current);
                    return Some(values);
                }
                _ => {}
            }
        }
        let mut font_style = Value::ident(Identifier::from_ident(Ident::Normal));
        let mut font_variant = Value::ident(Identifier::from_ident(Ident::Normal));
        let mut font_weight = Value::ident(Identifier::from_ident(Ident::Normal));
        let line_height = Value::ident(Identifier::from_ident(Ident::Normal));
        // Parses style/variant/weight; on exit `current` holds the
        // font-size candidate (the `check_fontsize` label).
        loop {
            enum Advance {
                Style,
                Variant,
                Weight,
                Size,
            }
            let advance = if current.lexical_type() == ValueType::Ident {
                match current.identifier().ident() {
                    Ident::Normal => None,
                    Ident::Italic | Ident::Oblique => Some(Advance::Style),
                    Ident::SmallCaps => Some(Advance::Variant),
                    Ident::Bold | Ident::Bolder | Ident::Lighter => Some(Advance::Weight),
                    _ => Some(Advance::Size),
                }
            } else if current.lexical_type() == ValueType::Number
                && current.dimension() == Unit::NoUnit
            {
                match current.integer_value() {
                    100 | 200 | 300 | 400 | 500 | 600 | 700 | 800 | 900 => Some(Advance::Weight),
                    _ => Some(Advance::Size),
                }
            } else {
                Some(Advance::Size)
            };
            match advance {
                None => {}
                Some(Advance::Style) => {
                    font_style = std::mem::replace(&mut current, Value::token(ValueType::Default));
                }
                Some(Advance::Variant) => {
                    font_variant =
                        std::mem::replace(&mut current, Value::token(ValueType::Default));
                }
                Some(Advance::Weight) => {
                    font_weight = std::mem::replace(&mut current, Value::token(ValueType::Default));
                }
                Some(Advance::Size) => break,
            }
            if !self.skip_to_next_any() {
                return None;
            }
            current = self.parse_any()?;
        }
        let font_size = match current.lexical_type() {
            ValueType::Ident => match current.identifier().ident() {
                Ident::XxSmall
                | Ident::XSmall
                | Ident::Small
                | Ident::Medium
                | Ident::Large
                | Ident::XLarge
                | Ident::XxLarge
                | Ident::Larger
                | Ident::Smaller => current,
                _ => return None,
            },
            ValueType::Number => current,
            _ => return None,
        };
        let mut line_height = line_height;
        if self.skip_to_next_any() && self.input[self.pos] == b'/' {
            self.pos += 1;
            if !self.skip_to_next_any() {
                return None;
            }
            let height = self.parse_any()?;
            match height.lexical_type() {
                ValueType::Ident => {
                    if height.identifier().ident() != Ident::Normal {
                        return None;
                    }
                }
                ValueType::Number => {
                    line_height = height;
                }
                _ => return None,
            }
        }
        values.push(font_style);
        values.push(font_variant);
        values.push(font_weight);
        values.push(font_size);
        values.push(line_height);
        if !self.parse_font_family(&mut values) {
            return None;
        }
        Some(values)
    }

    /// Parses raw declarations (`ParseRawDeclarations`).
    pub fn parse_raw_declarations(&mut self) -> Declarations {
        self.skip_space();
        if self.done() {
            return Vec::new();
        }
        let mut declarations = Declarations::new();
        while self.pos < self.input.len() {
            let decl_start = self.pos;
            let start_mask = self.errors_seen_mask;
            let mut ignore_this_decl = false;
            match self.input[self.pos] {
                b';' => {
                    self.pos += 1;
                }
                b'}' => return declarations,
                _ => {
                    let id = self.parse_ident();
                    if id.is_empty() {
                        self.report(DECLARATION_ERROR, b"Ignoring empty property");
                        ignore_this_decl = true;
                    } else {
                        let prop = Property::from_text(&id);
                        self.skip_space();
                        if self.done() || self.input[self.pos] != b':' {
                            let mut message = b"Ignoring property with no values ".to_vec();
                            message.extend_from_slice(&prop.prop_text());
                            self.report(DECLARATION_ERROR, &message);
                            ignore_this_decl = true;
                        } else {
                            self.pos += 1;
                            let values = match prop.prop() {
                                Prop::Font => self.parse_font(),
                                Prop::FontFamily => {
                                    let mut family = Values::new();
                                    if !self.parse_font_family(&mut family) || family.is_empty() {
                                        None
                                    } else {
                                        Some(family)
                                    }
                                }
                                _ => self.parse_values(prop.prop()),
                            };
                            match values {
                                None => {
                                    let mut message =
                                        b"Failed to parse values for property ".to_vec();
                                    message.extend_from_slice(&prop.prop_text());
                                    self.report(DECLARATION_ERROR, &message);
                                    ignore_this_decl = true;
                                }
                                Some(values) => {
                                    if self.preservation_mode && self.errors_seen_mask != start_mask
                                    {
                                        let mut message =
                                            b"Error while parsing values for property ".to_vec();
                                        message.extend_from_slice(&prop.prop_text());
                                        self.report(DECLARATION_ERROR, &message);
                                        ignore_this_decl = true;
                                    } else {
                                        let mut important = false;
                                        let mut bad_important = false;
                                        if self.pos < self.input.len()
                                            && self.input[self.pos] == b'!'
                                        {
                                            self.pos += 1;
                                            self.skip_space();
                                            let ident = self.parse_ident();
                                            if string_case_equals(ident.as_bytes(), b"important") {
                                                important = true;
                                            } else {
                                                let mut message =
                                                    b"Unexpected !-identifier: !".to_vec();
                                                message.extend_from_slice(ident.as_bytes());
                                                self.report(DECLARATION_ERROR, &message);
                                                bad_important = true;
                                            }
                                        }
                                        if bad_important {
                                            ignore_this_decl = true;
                                        } else {
                                            self.skip_space();
                                            if self.done()
                                                || self.input[self.pos] == b';'
                                                || self.input[self.pos] == b'}'
                                            {
                                                declarations.push(Declaration::new(
                                                    prop, values, important,
                                                ));
                                            } else {
                                                let byte = self.input[self.pos];
                                                let mut message = b"Unexpected char ".to_vec();
                                                message.push(byte);
                                                message
                                                    .extend_from_slice(b" at end of declaration");
                                                self.report(DECLARATION_ERROR, &message);
                                                ignore_this_decl = true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            self.skip_space();
            if ignore_this_decl {
                self.errors_seen_mask |= DECLARATION_ERROR;
                while self.pos < self.input.len()
                    && self.input[self.pos] != b';'
                    && self.input[self.pos] != b'}'
                {
                    match self.input[self.pos] {
                        b'{' | b'[' | b'(' => {
                            self.skip_matching();
                        }
                        _ => {
                            self.skip_next_token();
                        }
                    }
                    self.skip_space();
                }
                if self.preservation_mode {
                    declarations.push(Declaration::unparseable(&self.input[decl_start..self.pos]));
                    self.unparseable_sections_seen_mask |= self.errors_seen_mask;
                    self.errors_seen_mask = start_mask;
                }
            }
        }
        declarations
    }

    /// Expands shorthand declarations (`ExpandDeclarations`).
    pub fn expand_declarations(original: &mut Declarations) -> Declarations {
        let mut expanded = Declarations::new();
        for declaration in original.drain(..) {
            // The original stays first; expansions follow it.
            let mut additions = Declarations::new();
            expand_shorthand_properties(&mut additions, &declaration);
            if declaration.prop() == Prop::Background {
                expand_background(&declaration, &mut additions);
            }
            expanded.push(declaration);
            expanded.extend(additions);
        }
        expanded
    }

    /// Parses declarations with expansion (`ParseDeclarations`).
    pub fn parse_declarations(&mut self) -> Declarations {
        let mut original = self.parse_raw_declarations();
        Parser::expand_declarations(&mut original)
    }
}

/// Expands a background shorthand (`ExpandBackground`).
pub(crate) fn expand_background(
    original: &Declaration,
    new_declarations: &mut Declarations,
) -> bool {
    let values = original.values();
    let important = original.is_important();
    let mut background_color = Value::ident(Identifier::from_ident(Ident::Transparent));
    let mut background_image = Value::ident(Identifier::from_ident(Ident::None));
    let mut background_repeat = Value::ident(Identifier::from_ident(Ident::Repeat));
    let mut background_attachment = Value::ident(Identifier::from_ident(Ident::Scroll));
    let mut position_x: Option<Value> = None;
    let mut position_y: Option<Value> = None;
    let mut first_is_ambiguous = false;
    let mut is_first = true;
    for (index, value) in values.iter().enumerate() {
        match value.lexical_type() {
            ValueType::Color => {
                background_color = value.clone();
            }
            ValueType::Uri => {
                background_image = value.clone();
            }
            ValueType::Number => {
                if position_x.is_none() {
                    position_x = Some(value.clone());
                    first_is_ambiguous = true;
                } else if position_y.is_none() {
                    position_y = Some(value.clone());
                }
            }
            ValueType::Ident => match value.identifier().ident() {
                Ident::Center => {
                    if position_x.is_none() {
                        position_x = Some(value.clone());
                        first_is_ambiguous = true;
                    } else if position_y.is_none() {
                        position_y = Some(value.clone());
                    }
                }
                Ident::Left | Ident::Right => {
                    if position_x.is_none() || position_y.is_none() {
                        if position_x.is_some() && first_is_ambiguous {
                            position_y = position_x.take();
                        }
                        position_x = Some(value.clone());
                        first_is_ambiguous = false;
                    }
                }
                Ident::Top | Ident::Bottom => {
                    if position_x.is_none() || position_y.is_none() {
                        position_y = Some(value.clone());
                    }
                }
                Ident::Repeat | Ident::RepeatX | Ident::RepeatY | Ident::NoRepeat => {
                    background_repeat = value.clone();
                }
                Ident::Scroll | Ident::Fixed => {
                    background_attachment = value.clone();
                }
                Ident::Transparent => {
                    background_color = value.clone();
                }
                Ident::None => {
                    background_image = value.clone();
                }
                Ident::Inherit => {
                    if !(index == 0 && values.len() == 1) {
                        return false;
                    }
                    background_color = value.clone();
                    background_image = value.clone();
                    background_repeat = value.clone();
                    background_attachment = value.clone();
                    position_x = Some(value.clone());
                    position_y = Some(value.clone());
                }
                _ => return false,
            },
            _ => return false,
        }
        is_first = false;
    }
    if is_first {
        return false;
    }
    new_declarations.push(Declaration::single(
        Property::from_prop(Prop::BackgroundColor),
        &background_color,
        important,
    ));
    new_declarations.push(Declaration::single(
        Property::from_prop(Prop::BackgroundImage),
        &background_image,
        important,
    ));
    new_declarations.push(Declaration::single(
        Property::from_prop(Prop::BackgroundRepeat),
        &background_repeat,
        important,
    ));
    new_declarations.push(Declaration::single(
        Property::from_prop(Prop::BackgroundAttachment),
        &background_attachment,
        important,
    ));
    if position_x.is_none() && position_y.is_none() {
        position_x = Some(Value::number(0.0, Unit::Percent));
        position_y = Some(Value::number(0.0, Unit::Percent));
    } else if position_x.is_none() {
        position_x = Some(Value::number(50.0, Unit::Percent));
    } else if position_y.is_none() {
        position_y = Some(Value::number(50.0, Unit::Percent));
    }
    new_declarations.push(Declaration::single(
        Property::from_prop(Prop::BackgroundPositionX),
        position_x.as_ref().expect("position x"),
        important,
    ));
    new_declarations.push(Declaration::single(
        Property::from_prop(Prop::BackgroundPositionY),
        position_y.as_ref().expect("position y"),
        important,
    ));
    true
}

/// Expands font shorthands (`ExpandShorthandProperties`).
fn expand_shorthand_properties(declarations: &mut Declarations, declaration: &Declaration) {
    if declaration.prop() != Prop::Font {
        return;
    }
    let values = declaration.values();
    let important = declaration.is_important();
    let mut edited: Option<Values> = None;
    if values.len() == 1 {
        let value = &values[0];
        if value.lexical_type() == ValueType::Ident {
            match value.identifier().ident() {
                Ident::Caption
                | Ident::Icon
                | Ident::Menu
                | Ident::MessageBox
                | Ident::SmallCaption
                | Ident::StatusBar => {
                    edited = Some(vec![
                        Value::ident(Identifier::from_ident(Ident::Normal)),
                        Value::ident(Identifier::from_ident(Ident::Normal)),
                        Value::ident(Identifier::from_ident(Ident::Normal)),
                        Value::number(32.0 / 3.0, Unit::Px),
                        Value::ident(Identifier::from_ident(Ident::Normal)),
                        value.clone(),
                    ]);
                }
                Ident::Inherit => {
                    edited = Some(vec![value.clone(); 6]);
                }
                _ => {}
            }
        }
    }
    let values = edited.as_ref().unwrap_or(values);
    if values.len() < 5 {
        return;
    }
    declarations.push(Declaration::single(
        Property::from_prop(Prop::FontStyle),
        &values[0],
        important,
    ));
    declarations.push(Declaration::single(
        Property::from_prop(Prop::FontVariant),
        &values[1],
        important,
    ));
    declarations.push(Declaration::single(
        Property::from_prop(Prop::FontWeight),
        &values[2],
        important,
    ));
    declarations.push(Declaration::single(
        Property::from_prop(Prop::FontSize),
        &values[3],
        important,
    ));
    declarations.push(Declaration::single(
        Property::from_prop(Prop::LineHeight),
        &values[4],
        important,
    ));
    if values.len() > 5 {
        declarations.push(Declaration::new(
            Property::from_prop(Prop::FontFamily),
            values[5..].to_vec(),
            important,
        ));
    }
}

/// Whether a property expects colors (`IsPropExpectingColor`).
fn is_prop_expecting_color(prop: Prop) -> bool {
    matches!(
        prop,
        Prop::BorderColor
            | Prop::BorderTopColor
            | Prop::BorderRightColor
            | Prop::BorderBottomColor
            | Prop::BorderLeftColor
            | Prop::Border
            | Prop::BorderTop
            | Prop::BorderRight
            | Prop::BorderBottom
            | Prop::BorderLeft
            | Prop::BackgroundColor
            | Prop::Background
            | Prop::Color
            | Prop::OutlineColor
            | Prop::Outline
    )
}

impl<'a> Parser<'a> {
    /// Parses an attribute selector (`ParseAttributeSelector`).
    pub(crate) fn parse_attribute_selector(&mut self) -> Option<SimpleSelector> {
        debug_assert!(self.pos < self.input.len());
        debug_assert_eq!(self.input[self.pos], b'[');
        self.pos += 1;
        self.skip_space();
        let attr = self.parse_ident();
        self.skip_space();
        let mut result = None;
        if !attr.is_empty() && self.pos < self.input.len() {
            let oper = self.input[self.pos];
            match self.input[self.pos] {
                b'~' | b'|' | b'^' | b'$' | b'*' => {
                    self.pos += 1;
                    if self.done() || self.input[self.pos] != b'=' {
                        // Fall through with no selector.
                    } else {
                        self.pos += 1;
                        let value = self.parse_string_or_ident();
                        if !value.is_empty() {
                            result = Some(SimpleSelector::new_binary_attribute(
                                SimpleSelectorType::from_attribute_operator(oper),
                                attr.clone(),
                                value,
                            ));
                        }
                    }
                }
                b'=' => {
                    self.pos += 1;
                    let value = self.parse_string_or_ident();
                    if !value.is_empty() {
                        result = Some(SimpleSelector::new_binary_attribute(
                            SimpleSelectorType::from_attribute_operator(oper),
                            attr.clone(),
                            value,
                        ));
                    }
                }
                _ => {
                    result = Some(SimpleSelector::new_exist_attribute(attr.clone()));
                }
            }
        }
        self.skip_space();
        if !self.done() && self.input[self.pos] != b']' {
            self.report(SELECTOR_ERROR, b"Ignoring chars in attribute selector.");
        }
        if self.skip_past_delimiter(b']') {
            result
        } else {
            None
        }
    }

    /// Whether a class/id name starts illegally
    /// (`AtInvalidNameStart`).
    pub(crate) fn at_invalid_name_start(&self) -> bool {
        let mut pos = self.pos;
        if pos < self.input.len() && self.input[pos] == b'-' {
            pos += 1;
            if pos == self.input.len() {
                return true;
            }
            let next = self.input[pos];
            if !(next.is_ascii_alphanumeric()
                || next == b'-'
                || next == b'_'
                || next == b'\\'
                || !is_ascii(next))
            {
                return true;
            }
        }
        pos < self.input.len() && self.input[pos].is_ascii_digit()
    }

    /// Parses one simple selector (`ParseSimpleSelector`).
    pub(crate) fn parse_simple_selector(&mut self) -> Option<SimpleSelector> {
        if self.done() {
            return None;
        }
        match self.input[self.pos] {
            b'#' => {
                self.pos += 1;
                if self.at_invalid_name_start() {
                    self.parse_ident();
                    self.report(SELECTOR_ERROR, b"Id name is not an identifier.");
                    return None;
                }
                let id = self.parse_ident();
                if id.is_empty() {
                    return None;
                }
                Some(SimpleSelector::new_id(id))
            }
            b'.' => {
                self.pos += 1;
                if self.at_invalid_name_start() {
                    self.parse_ident();
                    self.report(SELECTOR_ERROR, b"Class name is not an identifier.");
                    return None;
                }
                let classname = self.parse_ident();
                if classname.is_empty() {
                    return None;
                }
                Some(SimpleSelector::new_class(classname))
            }
            b':' => {
                self.pos += 1;
                let mut separator = UnicodeText::from_utf8(b":");
                if !self.done() && self.input[self.pos] == b':' {
                    self.pos += 1;
                    separator = UnicodeText::from_utf8(b"::");
                }
                let pseudoclass = self.parse_ident();
                if !self.done() && self.input[self.pos] == b'(' {
                    self.report(SELECTOR_ERROR, b"Cannot parse parameters for pseudoclass.");
                    self.pos += 1;
                    if !self.skip_past_delimiter(b')') {
                        return None;
                    }
                }
                if pseudoclass.is_empty() {
                    return None;
                }
                Some(SimpleSelector::new_pseudoclass(pseudoclass, separator))
            }
            b'[' => self.parse_attribute_selector(),
            b'*' => {
                self.pos += 1;
                Some(SimpleSelector::new_universal())
            }
            _ => {
                let ident = self.parse_ident();
                if ident.is_empty() {
                    return None;
                }
                Some(SimpleSelector::new_element_type(ident))
            }
        }
    }

    /// Whether a simple-selector chain may end here
    /// (`AtValidSimpleSelectorsTerminator`).
    pub(crate) fn at_valid_simple_selectors_terminator(&self) -> bool {
        if self.done() {
            return true;
        }
        match self.input[self.pos] {
            b' ' | b'\t' | b'\r' | b'\n' | 0x0C | b',' | b'{' | b'>' | b'+' => true,
            b'/' => self.pos + 1 < self.input.len() && self.input[self.pos + 1] == b'*',
            _ => false,
        }
    }

    /// Parses a simple-selector sequence (`ParseSimpleSelectors`).
    pub(crate) fn parse_simple_selectors(
        &mut self,
        expecting_combinator: bool,
    ) -> Option<SimpleSelectors> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let combinator = if !expecting_combinator {
            Combinator::None
        } else {
            match self.input[self.pos] {
                b'>' => {
                    self.pos += 1;
                    Combinator::Child
                }
                b'+' => {
                    self.pos += 1;
                    Combinator::Sibling
                }
                _ => Combinator::Descendant,
            }
        };
        let mut selectors = SimpleSelectors::new(combinator);
        self.skip_space();
        if self.done() {
            return None;
        }
        let mut old_pos = self.pos;
        while let Some(simple) = self.parse_simple_selector() {
            selectors.push(simple);
            old_pos = self.pos;
        }
        if !selectors.is_empty()
            && self.pos == old_pos
            && self.at_valid_simple_selectors_terminator()
        {
            return Some(selectors);
        }
        None
    }

    /// Parses selectors (`ParseSelectors`).
    pub fn parse_selectors(&mut self) -> Option<Selectors> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let mut success = true;
        let mut selectors = Selectors::new();
        let mut selector = Selector::new();
        let mut expecting_combinator = false;
        while self.pos < self.input.len() && self.input[self.pos] != b'{' {
            if self.input[self.pos] == b',' {
                if selector.is_empty() {
                    success = false;
                    self.report(SELECTOR_ERROR, b"Could not parse ruleset: unexpected ,");
                } else {
                    selectors.push(std::mem::take(&mut selector));
                }
                self.pos += 1;
                expecting_combinator = false;
            } else {
                let old_pos = self.pos;
                match self.parse_simple_selectors(expecting_combinator) {
                    None => {
                        success = false;
                        if self.pos == old_pos {
                            debug_assert!(!self.done());
                            let byte = self.input[self.pos];
                            let mut message = b"Could not parse selector: illegal char ".to_vec();
                            message.push(byte);
                            self.report(SELECTOR_ERROR, &message);
                            self.pos += 1;
                        }
                    }
                    Some(simple) => {
                        selector.push(simple);
                    }
                }
                expecting_combinator = true;
            }
            self.skip_space();
        }
        // The trailing selector joins the list even when empty; an
        // empty trailing selector fails the whole parse.
        selectors.push(std::mem::take(&mut selector));
        if selectors.at(selectors.len() - 1).is_empty() {
            success = false;
        }
        if success {
            Some(selectors)
        } else {
            None
        }
    }

    /// Parses the next `@import` (`ParseNextImport`).
    pub fn parse_next_import(&mut self) -> Option<Import> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let old_pos = self.pos;
        if self.input[self.pos] != b'@' {
            return None;
        }
        self.pos += 1;
        let ident = self.parse_ident();
        if !string_case_equals(ident.as_bytes(), b"import") {
            self.pos = old_pos;
            return None;
        }
        let import = self.parse_import();
        self.skip_to_at_rule_end();
        self.skip_space();
        import
    }

    /// Parses a lone `@import` (`ParseAsSingleImport`).
    pub fn parse_as_single_import(&mut self) -> Option<Import> {
        let import = self.parse_next_import()?;
        if self.done() {
            return Some(import);
        }
        None
    }

    /// Extracts a leading `@charset` (`ExtractCharset`).
    pub fn extract_charset(&mut self) -> UnicodeText {
        let mut result = UnicodeText::new();
        if !self.done() && self.input[self.pos] == b'@' {
            self.pos += 1;
            let ident = self.parse_ident();
            if string_case_equals(ident.as_bytes(), b"charset") {
                result = self.parse_charset();
                self.skip_space();
                if self.done() || self.input[self.pos] != b';' {
                    self.report(CHARSET_ERROR, b"@charset not closed properly.");
                    result.clear();
                }
            }
        }
        result
    }

    /// Parses an `@charset` body (`ParseCharset`).
    pub(crate) fn parse_charset(&mut self) -> UnicodeText {
        let mut result = UnicodeText::new();
        self.skip_space();
        if self.done() {
            self.report(CHARSET_ERROR, b"Unexpected EOF parsing @charset.");
            return result;
        }
        match self.input[self.pos] {
            b'\'' => {
                result = self.parse_string(b'\'');
            }
            b'"' => {
                result = self.parse_string(b'"');
            }
            _ => {
                self.report(CHARSET_ERROR, b"@charset lacks string.");
            }
        }
        result
    }

    /// Parses a ruleset (`ParseRuleset`).
    pub(crate) fn parse_ruleset(&mut self) -> Option<Ruleset> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let mut success = true;
        let start_pos = self.pos;
        let start_mask = self.errors_seen_mask;
        let mut ruleset = Ruleset::new();
        let mut selectors = self.parse_selectors();
        if self.done() {
            self.report(
                SELECTOR_ERROR,
                b"Selectors without declarations at end of doc.",
            );
            return None;
        }
        if self.preservation_mode && start_mask != self.errors_seen_mask {
            selectors = None;
        }
        match selectors {
            None => {
                self.report(SELECTOR_ERROR, b"Failed to parse selector");
                if self.preservation_mode {
                    ruleset.set_selectors(Selectors::dummy(&self.input[start_pos..self.pos]));
                    self.unparseable_sections_seen_mask |= self.errors_seen_mask;
                    self.errors_seen_mask = start_mask;
                } else {
                    success = false;
                }
            }
            Some(selectors) => {
                ruleset.set_selectors(selectors);
            }
        }
        debug_assert!(!self.done());
        debug_assert_eq!(self.input[self.pos], b'{');
        self.pos += 1;
        ruleset.set_declarations(self.parse_raw_declarations());
        self.skip_space();
        if self.done() || self.input[self.pos] != b'}' {
            self.report(RULESET_ERROR, b"Ignored chars at end of ruleset.");
        }
        self.skip_past_delimiter(b'}');
        if success {
            Some(ruleset)
        } else {
            None
        }
    }

    /// Parses a media query list (`ParseMediaQueries`).
    pub fn parse_media_queries(&mut self) -> MediaQueries {
        let mut queries = MediaQueries::new();
        self.skip_space();
        if self.done() || self.input[self.pos] == b';' || self.input[self.pos] == b'{' {
            return queries;
        }
        while self.pos < self.input.len() {
            let mut query = self.parse_media_query();
            if query.is_none() {
                let mut replacement = MediaQuery::new();
                replacement.set_qualifier(MediaQualifier::Not);
                replacement.set_media_type(UnicodeText::from_utf8(b"all"));
                query = Some(replacement);
            }
            queries.push(query.expect("media query"));
            self.skip_space();
            if self.done() {
                return queries;
            }
            match self.input[self.pos] {
                b';' | b'{' => return queries,
                b',' => {
                    self.pos += 1;
                }
                _ => {
                    self.report(MEDIA_ERROR, b"Unexpected char while parsing media query.");
                    return queries;
                }
            }
        }
        queries
    }

    /// Parses one media query (`ParseMediaQuery`).
    pub(crate) fn parse_media_query(&mut self) -> Option<MediaQuery> {
        self.skip_space();
        let mut query = MediaQuery::new();
        let mut id = self.parse_ident();
        self.skip_space();
        if string_case_equals(id.as_bytes(), b"not") {
            query.set_qualifier(MediaQualifier::Not);
            id = self.parse_ident();
        } else if string_case_equals(id.as_bytes(), b"only") {
            query.set_qualifier(MediaQualifier::Only);
            id = self.parse_ident();
        }
        let mut need_and = false;
        let mut found_and = false;
        if !id.is_empty() {
            query.set_media_type(id);
            need_and = true;
        }
        let mut done = false;
        self.skip_space();
        while !self.done() && !done {
            match self.input[self.pos] {
                b';' | b'{' | b',' => {
                    done = true;
                }
                b'(' => {
                    if need_and != found_and {
                        self.report(MEDIA_ERROR, b"Missing or extra 'and' in media query");
                        self.skip_to_media_query_end();
                        return None;
                    }
                    need_and = true;
                    found_and = false;
                    self.pos += 1;
                    self.skip_space();
                    let name = self.parse_ident();
                    self.skip_space();
                    if self.done() {
                        self.report(MEDIA_ERROR, b"Unexpected EOF in media query.");
                        return None;
                    }
                    match self.input[self.pos] {
                        b')' => {
                            self.pos += 1;
                            query.add_expression(MediaExpression::without_value(name));
                        }
                        b':' => {
                            self.pos += 1;
                            self.skip_space();
                            if self.done() {
                                self.report(MEDIA_ERROR, b"Unexpected EOF in media query.");
                                return Some(query);
                            }
                            let begin = self.pos;
                            if self.skip_past_delimiter(b')') {
                                let end = self.pos - 1;
                                debug_assert!(begin <= end);
                                let value = UnicodeText::from_utf8(&self.input[begin..end]);
                                query.add_expression(MediaExpression::with_value(name, value));
                            } else {
                                self.report(MEDIA_ERROR, b"Unclosed media query.");
                                self.skip_to_media_query_end();
                                return None;
                            }
                        }
                        _ => {
                            self.report(MEDIA_ERROR, b"Failed to parse media expression.");
                            self.skip_past_delimiter(b')');
                            self.skip_to_media_query_end();
                            return None;
                        }
                    }
                }
                _ => {
                    let ident = self.parse_ident();
                    if string_case_equals(ident.as_bytes(), b"and") {
                        if found_and {
                            self.report(MEDIA_ERROR, b"Multiple 'and' tokens in a row.");
                            self.skip_to_media_query_end();
                            return None;
                        } else if !self.done() && self.input[self.pos] == b'(' {
                            self.report(
                                MEDIA_ERROR,
                                b"Space required between 'and' and '(' tokens.",
                            );
                            self.skip_to_media_query_end();
                            return None;
                        }
                        found_and = true;
                    } else if self.pos >= self.input.len() {
                        self.report(MEDIA_ERROR, b"Unexpected EOF");
                        self.skip_to_media_query_end();
                        return None;
                    } else if ident.is_empty() {
                        let byte = self.input[self.pos];
                        let mut message = b"Unexpected char in media query: ".to_vec();
                        message.push(byte);
                        self.report(MEDIA_ERROR, &message);
                        self.skip_to_media_query_end();
                        return None;
                    } else {
                        let mut message =
                            b"Unexpected identifier separating media queries: ".to_vec();
                        message.extend_from_slice(ident.as_bytes());
                        self.report(MEDIA_ERROR, &message);
                        self.skip_to_media_query_end();
                        return None;
                    }
                }
            }
            self.skip_space();
        }
        if found_and {
            self.report(MEDIA_ERROR, b"Unexpected trailing 'and' token.");
            self.skip_to_media_query_end();
            return None;
        }
        if query.media_type().is_empty() && query.expressions().is_empty() {
            self.report(MEDIA_ERROR, b"Unexpected empty media query.");
            return None;
        }
        Some(query)
    }

    /// Parses an `@import` body (`ParseImport`).
    pub(crate) fn parse_import(&mut self) -> Option<Import> {
        self.skip_space();
        if self.done() {
            return None;
        }
        let value = self.parse_any()?;
        if value.lexical_type() != ValueType::String && value.lexical_type() != ValueType::Uri {
            self.report(IMPORT_ERROR, b"Unexpected token while parsing @import");
            return None;
        }
        let mut import = Import::new();
        import.set_link(value.string_value().clone());
        self.skip_space();
        if self.done() || self.input[self.pos] == b';' {
            import.set_media_queries(MediaQueries::new());
        } else {
            let start_mask = self.errors_seen_mask;
            let media = self.parse_media_queries();
            if self.preservation_mode && self.errors_seen_mask != start_mask {
                self.report(IMPORT_ERROR, b"Error parsing media for @import.");
                return None;
            }
            import.set_media_queries(media);
        }
        Some(import)
    }

    /// Parses `@font-face` (`ParseFontFace`).
    pub(crate) fn parse_font_face(&mut self) -> Option<FontFace> {
        let mut font_face = FontFace::new();
        self.skip_space();
        if self.done() {
            self.report(AT_RULE_ERROR, b"Unexpected EOF in @font-face.");
            return None;
        }
        if self.input[self.pos] != b'{' {
            self.report(AT_RULE_ERROR, b"Expected '{' after @font-face.");
            return None;
        }
        self.pos += 1;
        font_face.set_declarations(self.parse_raw_declarations());
        self.skip_space();
        if self.done() || self.input[self.pos] != b'}' {
            self.report(AT_RULE_ERROR, b"Ignored chars at end of @font-face.");
        }
        self.skip_past_delimiter(b'}');
        Some(font_face)
    }

    /// Parses one statement (`ParseStatement`).
    pub(crate) fn parse_statement(
        &mut self,
        media_queries: Option<&MediaQueries>,
        stylesheet: &mut Stylesheet,
    ) {
        self.skip_space();
        if self.done() {
            return;
        }
        let old_pos = self.pos;
        let start_mask = self.errors_seen_mask;
        if self.input[self.pos] == b'@' {
            let mut correctly_terminated = true;
            self.pos += 1;
            let ident = self.parse_ident();
            if string_case_equals(ident.as_bytes(), b"import") {
                if media_queries.is_some() {
                    self.report(IMPORT_ERROR, b"@import found inside @media");
                    correctly_terminated = self.skip_to_at_rule_end();
                } else if !stylesheet.rulesets().is_empty() || !stylesheet.font_faces().is_empty() {
                    self.report(IMPORT_ERROR, b"@import found after rulesets.");
                    correctly_terminated = self.skip_to_at_rule_end();
                } else {
                    let import = self.parse_import();
                    self.skip_space();
                    match import {
                        Some(import) => {
                            if self.done() {
                                self.report(IMPORT_ERROR, b"Unexpected EOF in @import statement.");
                                correctly_terminated = false;
                                stylesheet.imports_mut().push(import);
                            } else if self.input[self.pos] == b';' {
                                self.pos += 1;
                                stylesheet.imports_mut().push(import);
                            } else {
                                self.report(IMPORT_ERROR, b"Ignoring chars at end of @import.");
                                correctly_terminated = self.skip_to_at_rule_end();
                            }
                        }
                        None => {
                            self.report(IMPORT_ERROR, b"Failed to parse @import.");
                            correctly_terminated = self.skip_to_at_rule_end();
                        }
                    }
                }
            } else if string_case_equals(ident.as_bytes(), b"charset") {
                if media_queries.is_some() {
                    self.report(CHARSET_ERROR, b"@charset found inside @media");
                    correctly_terminated = self.skip_to_at_rule_end();
                } else if !stylesheet.rulesets().is_empty()
                    || !stylesheet.imports().is_empty()
                    || !stylesheet.font_faces().is_empty()
                {
                    self.report(CHARSET_ERROR, b"@charset found after other rules.");
                    correctly_terminated = self.skip_to_at_rule_end();
                } else {
                    let charset = self.parse_charset();
                    self.skip_space();
                    if self.preservation_mode && self.errors_seen_mask != start_mask {
                        self.report(CHARSET_ERROR, b"Failed to parse @charset.");
                        correctly_terminated = self.skip_to_at_rule_end();
                    } else if self.done() {
                        self.report(CHARSET_ERROR, b"Unexpected EOF in @charset statement.");
                        correctly_terminated = false;
                        stylesheet.charsets_mut().push(charset);
                    } else if self.input[self.pos] == b';' {
                        self.pos += 1;
                        stylesheet.charsets_mut().push(charset);
                    } else {
                        self.report(CHARSET_ERROR, b"Ignoring chars at end of @charset.");
                        correctly_terminated = self.skip_to_at_rule_end();
                    }
                }
            } else if string_case_equals(ident.as_bytes(), b"media") {
                if media_queries.is_some() {
                    self.report(MEDIA_ERROR, b"@media found inside @media");
                    correctly_terminated = self.skip_to_at_rule_end();
                } else {
                    let media = self.parse_media_queries();
                    if self.preservation_mode && self.errors_seen_mask != start_mask {
                        self.report(MEDIA_ERROR, b"Error parsing media queries, ignoring block.");
                        correctly_terminated = self.skip_to_at_rule_end();
                    } else if self.done() {
                        self.report(MEDIA_ERROR, b"Unexpected EOF in @media statement");
                        correctly_terminated = false;
                    } else if self.input[self.pos] == b';' {
                        self.pos += 1;
                        return;
                    } else if self.input[self.pos] != b'{' {
                        self.report(MEDIA_ERROR, b"Malformed @media statement.");
                        correctly_terminated = self.skip_to_at_rule_end();
                    } else {
                        self.pos += 1;
                        self.skip_space();
                        let copied_bytes = crate::media::approximate_bytes(&media);
                        while self.pos < self.input.len() && self.input[self.pos] != b'}' {
                            self.media_queries_bytes += copied_bytes;
                            if self.media_queries_bytes
                                > max_media_queries_bytes(self.input.len() as i64)
                            {
                                self.report(MEDIA_ERROR, b"Too many copies of @media queries.");
                                self.pos = self.input.len();
                                break;
                            }
                            let inner_old = self.pos;
                            self.parse_statement(Some(&media), stylesheet);
                            if self.pos == inner_old {
                                let byte = self.input[self.pos];
                                let mut message =
                                    b"Could not parse ruleset: illegal char ".to_vec();
                                message.push(byte);
                                self.report(SELECTOR_ERROR, &message);
                                self.pos += 1;
                            }
                            self.skip_space();
                        }
                        if self.pos < self.input.len() {
                            debug_assert_eq!(self.input[self.pos], b'}');
                            self.pos += 1;
                        } else {
                            self.report(MEDIA_ERROR, b"Unexpected EOF in @media statement.");
                            correctly_terminated = false;
                        }
                    }
                }
            } else if string_case_equals(ident.as_bytes(), b"font-face") {
                let font_face = self.parse_font_face();
                if (self.preservation_mode && self.errors_seen_mask != start_mask)
                    || font_face.is_none()
                {
                    self.report(AT_RULE_ERROR, b"Could not parse @font-face rule.");
                    correctly_terminated = self.skip_to_at_rule_end();
                } else if let Some(mut font_face) = font_face {
                    match media_queries {
                        Some(media) => {
                            font_face.set_media_queries(media.clone());
                        }
                        None => {
                            font_face.set_media_queries(MediaQueries::new());
                        }
                    }
                    stylesheet.font_faces_mut().push(font_face);
                }
            } else {
                let mut message = b"Cannot parse unknown @-statement: ".to_vec();
                message.extend_from_slice(ident.as_bytes());
                self.report(AT_RULE_ERROR, &message);
                correctly_terminated = self.skip_to_at_rule_end();
            }
            if self.errors_seen_mask != start_mask && correctly_terminated && self.preservation_mode
            {
                let mut ruleset =
                    Ruleset::dummy_unparsed(UnparsedRegion::new(&self.input[old_pos..self.pos]));
                if let Some(media) = media_queries {
                    ruleset.set_media_queries(media.clone());
                }
                stylesheet.rulesets_mut().push(ruleset);
                self.unparseable_sections_seen_mask |= self.errors_seen_mask;
                self.errors_seen_mask = start_mask;
            }
        } else {
            let ruleset = self.parse_ruleset();
            match ruleset {
                None if old_pos == self.pos => {
                    let byte = self.input[self.pos];
                    let mut message = b"Could not parse ruleset: illegal char ".to_vec();
                    message.push(byte);
                    self.report(SELECTOR_ERROR, &message);
                    self.pos += 1;
                }
                None => {}
                Some(mut ruleset) => {
                    if let Some(media) = media_queries {
                        ruleset.set_media_queries(media.clone());
                    }
                    stylesheet.rulesets_mut().push(ruleset);
                }
            }
        }
    }

    /// Parses a raw stylesheet (`ParseRawStylesheet`).
    pub fn parse_raw_stylesheet(&mut self) -> Stylesheet {
        self.skip_space();
        if self.done() {
            return Stylesheet::new();
        }
        let mut stylesheet = Stylesheet::new();
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b'<' => {
                    self.pos += 1;
                    if self.input.len() - self.pos >= 3
                        && self.input[self.pos..self.pos + 3] == *b"!--"
                    {
                        self.pos += 3;
                    } else {
                        self.report(HTML_COMMENT_ERROR, b"< without following !--");
                    }
                }
                b'-' => {
                    self.pos += 1;
                    if self.input.len() - self.pos >= 2
                        && self.input[self.pos..self.pos + 2] == *b"->"
                    {
                        self.pos += 2;
                    } else {
                        self.report(HTML_COMMENT_ERROR, b"- without following ->");
                    }
                }
                _ => {
                    self.parse_statement(None, &mut stylesheet);
                }
            }
            self.skip_space();
        }
        debug_assert!(self.done());
        stylesheet
    }

    /// Parses a stylesheet with expansion (`ParseStylesheet`).
    pub fn parse_stylesheet(&mut self) -> Stylesheet {
        let mut stylesheet = self.parse_raw_stylesheet();
        for ruleset in stylesheet.rulesets_mut() {
            if ruleset.kind() == RulesetType::Ruleset {
                let mut original = std::mem::take(ruleset.declarations_mut());
                let expanded = Parser::expand_declarations(&mut original);
                ruleset.set_declarations(expanded);
            }
        }
        stylesheet
    }
}
