//! JavaScript minifiers (`js_minify.{h,cc}`).
//!
//! Two implementations, matching C++ selection rules:
//! - [`minify_js`]: the tokenizer-based minifier (default;
//!   `MinifyUtf8Js`), driven by [`Tokenizer`](crate::tokenizer::Tokenizer)
//!   through a minifying filter that drops comments/whitespace except
//!   where needed for separation or semicolon insertion.
//! - [`legacy_minify_js`]: the heuristic JSMin-style minifier used when
//!   the experimental tokenizer is disabled.
//!
//! Both fail closed (callers keep the original on error); the tokenizer
//! path additionally records source-map mappings.
#![forbid(unsafe_code)]

use crate::keywords::{can_keyword_precede_regex, TokenType};
use crate::sourcemap::Mapping;
use crate::tokenizer::{Tokenizer, TokenizerPatterns};

/// Whitespace classes (`JsWhitespace`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Whitespace {
    None,
    Space,
    Linebreak,
}

/// Minifying filter over the tokenizer (`JsMinifyingTokenizer`).
#[derive(Debug)]
struct MinifyingFilter<'a> {
    tokenizer: Tokenizer<'a>,
    whitespace: Whitespace,
    prev_type: TokenType,
    prev_token: &'a [u8],
    next_type: TokenType,
    next_token: &'a [u8],
    next_position: Mapping,
    current: Mapping,
}

impl<'a> MinifyingFilter<'a> {
    fn new(patterns: &'a TokenizerPatterns, input: &'a [u8]) -> MinifyingFilter<'a> {
        MinifyingFilter {
            tokenizer: Tokenizer::new(patterns, input),
            whitespace: Whitespace::None,
            prev_type: TokenType::EndOfInput,
            prev_token: b"",
            next_type: TokenType::EndOfInput,
            next_token: b"",
            next_position: Mapping::default(),
            current: Mapping::default(),
        }
    }

    /// Next minified token plus its source position.
    fn next_token(&mut self) -> (TokenType, &'a [u8], Mapping) {
        let (kind, token, position) = self.next_helper();
        // Advance the generated position past the emitted token.
        let (gen_line, gen_col) = (&mut self.current.gen_line, &mut self.current.gen_col);
        update_line_col(token, gen_line, gen_col);
        (kind, token, position)
    }

    #[allow(clippy::too_many_lines)]
    fn next_helper(&mut self) -> (TokenType, &'a [u8], Mapping) {
        if self.next_type != TokenType::EndOfInput {
            self.prev_type = self.next_type;
            self.prev_token = self.next_token;
            let mut position = self.next_position.clone();
            position.gen_line = self.current.gen_line;
            position.gen_col = self.current.gen_col;
            let token = self.next_token;
            self.next_type = TokenType::EndOfInput;
            self.next_token = b"";
            return (self.prev_type, token, position);
        }
        let first_position = self.current.clone();
        loop {
            let (kind, span) = self.tokenizer.next_token();
            let token = self.tokenizer.text(span);
            let token_position = self.current.clone();
            update_line_col(token, &mut self.current.src_line, &mut self.current.src_col);
            match kind {
                TokenType::Whitespace => {
                    if self.whitespace == Whitespace::None {
                        self.whitespace = Whitespace::Space;
                    }
                }
                TokenType::LineSeparator => {
                    self.whitespace = Whitespace::Linebreak;
                }
                TokenType::SemiInsert => {
                    self.whitespace = Whitespace::None;
                    self.prev_type = kind;
                    self.prev_token = b"\n";
                    return (kind, b"\n", first_position);
                }
                TokenType::Comment => {
                    // Keep IE conditional-compilation comments
                    // (`/*@...@*/`); treat the rest as whitespace.
                    if token.len() >= 6 && token.starts_with(b"/*@") && token.ends_with(b"@*/") {
                        return (kind, token, first_position);
                    } else if self.whitespace == Whitespace::None {
                        self.whitespace = Whitespace::Space;
                    }
                }
                _ => {
                    let whitespace = self.whitespace;
                    self.whitespace = Whitespace::None;
                    if whitespace != Whitespace::None
                        && whitespace_needed_before(self.prev_type, self.prev_token, kind, token)
                    {
                        self.next_type = kind;
                        self.next_token = token;
                        self.next_position = token_position;
                        if whitespace == Whitespace::Linebreak {
                            return (TokenType::LineSeparator, b"\n", first_position);
                        }
                        return (TokenType::Whitespace, b" ", first_position);
                    }
                    self.prev_type = kind;
                    self.prev_token = token;
                    return (kind, token, token_position);
                }
            }
        }
    }
}

/// Updates line/col over a text chunk (byte-oriented, like C++).
fn update_line_col(text: &[u8], line: &mut i32, col: &mut i32) {
    for byte in text {
        if *byte == b'\n' {
            *line += 1;
            *col = 0;
        } else {
            *col += 1;
        }
    }
}

/// Whether emitted output needs whitespace before a token
/// (`WhitespaceNeededBefore`).
fn whitespace_needed_before(
    prev_type: TokenType,
    prev_token: &[u8],
    kind: TokenType,
    token: &[u8],
) -> bool {
    if is_name_number_or_keyword(kind) {
        return is_name_number_or_keyword(prev_type) || prev_type == TokenType::Regex;
    }
    if token == b"." {
        if prev_type == TokenType::Number
            && !contains_dot_or_exp(prev_token)
            && (!prev_token.starts_with(b"0")
                || (!prev_token.contains(&b'x')
                    && !prev_token.contains(&b'X')
                    && (prev_token.contains(&b'8') || prev_token.contains(&b'9'))))
        {
            return true;
        }
        return false;
    }
    if prev_token.ends_with(b"/") {
        return token.starts_with(b"/");
    }
    if prev_token.ends_with(b"+") {
        return token.starts_with(b"+");
    }
    if prev_token.ends_with(b"<") {
        return token.starts_with(b"!") || token.starts_with(b"/");
    }
    if prev_token.ends_with(b"!") || prev_token.ends_with(b"-") {
        return token.starts_with(b"-");
    }
    false
}

fn contains_dot_or_exp(token: &[u8]) -> bool {
    token.contains(&b'.') || token.contains(&b'e') || token.contains(&b'E')
}

/// Name/number/keyword classes (`IsNameNumberOrKeyword`).
fn is_name_number_or_keyword(kind: TokenType) -> bool {
    !matches!(
        kind,
        TokenType::Comment
            | TokenType::Whitespace
            | TokenType::LineSeparator
            | TokenType::SemiInsert
            | TokenType::Regex
            | TokenType::StringLiteral
            | TokenType::Operator
            | TokenType::EndOfInput
            | TokenType::Error
    )
}

/// Whether to record a mapping step (`ShouldRecordStep`).
fn should_record_step(mappings: &[Mapping], next: &Mapping) -> bool {
    let Some(prev) = mappings.last() else {
        return true;
    };
    if next.gen_line == prev.gen_line {
        return next.src_line != prev.src_line
            || next.gen_col - prev.gen_col != next.src_col - prev.src_col;
    }
    true
}

/// Minifies with the tokenizer, recording source mappings
/// (`MinifyUtf8JsWithSourceMap`).
pub fn minify_utf8_with_source_map(
    patterns: &TokenizerPatterns,
    input: &[u8],
    mut mappings: Option<&mut Vec<Mapping>>,
) -> (Vec<u8>, bool) {
    let mut filter = MinifyingFilter::new(patterns, input);
    let mut out = Vec::new();
    let mut ok = true;
    loop {
        let (kind, token, position) = filter.next_token();
        match kind {
            TokenType::EndOfInput => break,
            TokenType::Error => {
                // Pass the remainder through unmodified, then fail.
                out.extend_from_slice(token);
                ok = false;
                break;
            }
            TokenType::SemiInsert => {
                out.push(b'\n');
                record_mapping(mappings.as_deref_mut(), &position);
            }
            _ => {
                out.extend_from_slice(token);
                record_mapping(mappings.as_deref_mut(), &position);
            }
        }
    }
    (out, ok)
}

fn record_mapping(mappings: Option<&mut Vec<Mapping>>, position: &Mapping) {
    if let Some(mappings) = mappings {
        if should_record_step(mappings, position) {
            mappings.push(position.clone());
        }
    }
}

/// Minifies with the tokenizer, discarding mappings (`MinifyUtf8Js`).
pub fn minify_utf8(patterns: &TokenizerPatterns, input: &[u8]) -> (Vec<u8>, bool) {
    minify_utf8_with_source_map(patterns, input, None)
}

/* Legacy heuristic minifier (JSMin-style). */

/// Legacy token classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyToken {
    Start,
    Char(u8),
    CcComment,
    Regex,
    Str,
    NameNumber,
    KeywordRegex,
    PlusPlus,
    MinusMinus,
}

impl LegacyToken {
    fn cannot_begin_or_end_statement(self) -> bool {
        matches!(
            self,
            LegacyToken::Start
                | LegacyToken::Char(
                    b'=' | b'<'
                        | b'>'
                        | b';'
                        | b':'
                        | b'?'
                        | b'|'
                        | b'^'
                        | b'&'
                        | b'*'
                        | b'/'
                        | b'%'
                        | b','
                        | b'.'
                )
        )
    }

    fn ends_primary_expression(self) -> bool {
        matches!(
            self,
            LegacyToken::NameNumber
                | LegacyToken::Regex
                | LegacyToken::Str
                | LegacyToken::Char(b')')
                | LegacyToken::Char(b']')
        )
    }
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'\\') || byte >= 127
}

fn can_suppress_linebreak(prev: LegacyToken, next: LegacyToken) -> bool {
    if prev.cannot_begin_or_end_statement()
        || matches!(
            prev,
            LegacyToken::Char(b'(' | b'[' | b'{' | b'!' | b'~' | b'+' | b'-')
        )
    {
        return true;
    }
    if next.cannot_begin_or_end_statement() || matches!(next, LegacyToken::Char(b')' | b']' | b'}'))
    {
        return true;
    }
    if prev.ends_primary_expression()
        && matches!(next, LegacyToken::Char(b'(' | b'[' | b'+' | b'-'))
    {
        return true;
    }
    false
}

/// Legacy minifier state.
struct LegacyMinifier<'a> {
    input: &'a [u8],
    index: usize,
    output: Vec<u8>,
    whitespace: Whitespace,
    prev: LegacyToken,
    error: bool,
    collapse_strings: bool,
}

impl<'a> LegacyMinifier<'a> {
    fn new(input: &'a [u8], collapse_strings: bool) -> LegacyMinifier<'a> {
        LegacyMinifier {
            input,
            index: 0,
            output: Vec::new(),
            whitespace: Whitespace::None,
            prev: LegacyToken::Start,
            error: false,
            collapse_strings,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.index + 1).copied()
    }

    fn change_token(&mut self, next: LegacyToken) {
        if self.whitespace == Whitespace::Linebreak && !can_suppress_linebreak(self.prev, next) {
            self.output.push(b'\n');
        }
        self.whitespace = Whitespace::None;
        self.prev = next;
    }

    fn insert_space_if_needed(&mut self) {
        match self.whitespace {
            Whitespace::Space => self.output.push(b' '),
            Whitespace::Linebreak => self.output.push(b'\n'),
            Whitespace::None => {}
        }
        self.whitespace = Whitespace::None;
    }

    fn consume_block_comment(&mut self) {
        let begin = self.index;
        self.index += 2;
        let may_be_ccc = self.index < self.input.len() && self.input[self.index] == b'@';
        while self.index < self.input.len() {
            if self.input[self.index] == b'*' && self.peek() == Some(b'/') {
                self.index += 2;
                if may_be_ccc && self.input[self.index - 3] == b'@' {
                    self.change_token(LegacyToken::CcComment);
                    self.output
                        .extend_from_slice(&self.input[begin..self.index]);
                } else if self.whitespace == Whitespace::None {
                    self.whitespace = Whitespace::Space;
                }
                return;
            }
            self.index += 1;
        }
        self.error = true;
    }

    fn consume_line_comment(&mut self) {
        while self.index < self.input.len()
            && self.input[self.index] != b'\n'
            && self.input[self.index] != b'\r'
        {
            self.index += 1;
        }
        self.whitespace = Whitespace::Linebreak;
    }

    fn consume_name_or_number(&mut self) {
        if matches!(
            self.prev,
            LegacyToken::NameNumber | LegacyToken::KeywordRegex | LegacyToken::Regex
        ) {
            self.insert_space_if_needed();
        }
        let begin = self.index;
        while self.index < self.input.len() && is_identifier_byte(self.input[self.index]) {
            self.index += 1;
        }
        let token = &self.input[begin..self.index];
        let text = std::str::from_utf8(token).unwrap_or("");
        self.change_token(if can_keyword_precede_regex(text) {
            LegacyToken::KeywordRegex
        } else {
            LegacyToken::NameNumber
        });
        self.output.extend_from_slice(token);
    }

    fn consume_regex(&mut self) {
        let begin = self.index;
        self.index += 1;
        let mut within_brackets = false;
        while self.index < self.input.len() {
            let byte = self.input[self.index];
            self.index += 1;
            if byte == b'\\' {
                self.index += 1;
            } else if byte == b'/' {
                if !within_brackets {
                    if self.prev == LegacyToken::Char(b'/') {
                        self.insert_space_if_needed();
                    }
                    self.change_token(LegacyToken::Regex);
                    self.output
                        .extend_from_slice(&self.input[begin..self.index]);
                    return;
                }
            } else if byte == b'[' {
                within_brackets = true;
            } else if byte == b']' {
                within_brackets = false;
            } else if byte == b'\n' {
                break;
            }
        }
        self.error = true;
    }

    fn consume_string(&mut self) {
        let begin = self.index;
        let quote = self.input[begin];
        self.index += 1;
        while self.index < self.input.len() {
            let byte = self.input[self.index];
            self.index += 1;
            if byte == b'\\' {
                self.index += 1;
            } else if byte == quote {
                self.change_token(LegacyToken::Str);
                if self.collapse_strings {
                    self.output.push(quote);
                    self.output.push(quote);
                } else {
                    self.output
                        .extend_from_slice(&self.input[begin..self.index]);
                }
                return;
            }
        }
        self.error = true;
    }

    fn minify(&mut self) {
        while self.index < self.input.len() && !self.error {
            let byte = self.input[self.index];
            if byte == b'\n' || byte == b'\r' {
                self.whitespace = Whitespace::Linebreak;
                self.index += 1;
            } else if byte == b' ' || byte == b'\t' {
                if self.whitespace == Whitespace::None {
                    self.whitespace = Whitespace::Space;
                }
                self.index += 1;
            } else if byte == b'\'' || byte == b'"' {
                self.consume_string();
            } else if byte == b'/' {
                match self.peek() {
                    Some(b'/') => self.consume_line_comment(),
                    Some(b'*') => self.consume_block_comment(),
                    _ if self.prev.ends_primary_expression() => {
                        self.change_token(LegacyToken::Char(b'/'));
                        self.output.push(byte);
                        self.index += 1;
                    }
                    _ => self.consume_regex(),
                }
            } else if is_identifier_byte(byte) {
                self.consume_name_or_number();
            } else if (byte == b'<' && self.input[self.index..].starts_with(b"<!--"))
                || (byte == b'-'
                    && (self.whitespace == Whitespace::Linebreak
                        || self.prev == LegacyToken::Start)
                    && self.input[self.index..].starts_with(b"-->"))
            {
                // `<!--` anywhere, or `-->` at a line start, opens a line
                // comment.
                self.consume_line_comment();
            } else if byte == b'+' && self.peek() == Some(b'+') {
                if self.prev == LegacyToken::Char(b'+') || self.prev == LegacyToken::PlusPlus {
                    self.insert_space_if_needed();
                }
                self.change_token(LegacyToken::PlusPlus);
                self.output.extend_from_slice(b"++");
                self.index += 2;
            } else if byte == b'-' && self.peek() == Some(b'-') {
                if self.prev == LegacyToken::Char(b'-')
                    || self.prev == LegacyToken::MinusMinus
                    || self.prev == LegacyToken::Char(b'!')
                {
                    self.insert_space_if_needed();
                }
                self.change_token(LegacyToken::MinusMinus);
                self.output.extend_from_slice(b"--");
                self.index += 2;
            } else if byte == b'`' {
                // Template literals fail closed (significant newlines).
                self.error = true;
            } else {
                if (self.prev == LegacyToken::Char(byte) && (byte == b'+' || byte == b'-'))
                    || (self.prev == LegacyToken::PlusPlus && byte == b'+')
                    || (self.prev == LegacyToken::MinusMinus && byte == b'-')
                    || (self.prev == LegacyToken::Char(b'<') && byte == b'!')
                    || (self.prev == LegacyToken::Char(b'!') && byte == b'-')
                {
                    self.insert_space_if_needed();
                }
                self.change_token(LegacyToken::Char(byte));
                self.output.push(byte);
                self.index += 1;
            }
        }
    }

    fn finish(mut self) -> Option<Vec<u8>> {
        self.minify();
        if self.error {
            None
        } else {
            Some(self.output)
        }
    }
}

/// Legacy minify (`legacy::MinifyJs`).
pub fn legacy_minify_js(input: &[u8]) -> Option<Vec<u8>> {
    LegacyMinifier::new(input, false).finish()
}

/// Legacy minify with string collapsing
/// (`legacy::MinifyJsAndCollapseStrings`).
pub fn legacy_minify_js_collapse_strings(input: &[u8]) -> Option<Vec<u8>> {
    LegacyMinifier::new(input, true).finish()
}

/// Legacy minified size (`legacy::GetMinifiedJsSize`).
pub fn legacy_minified_size(input: &[u8]) -> Option<usize> {
    legacy_minify_js(input).map(|out| out.len())
}

/// Collapsed minified size (`legacy::GetMinifiedStringCollapsedJsSize`).
pub fn legacy_minified_collapsed_size(input: &[u8]) -> Option<usize> {
    legacy_minify_js_collapse_strings(input).map(|out| out.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::TokenizerPatterns;

    // The C++ suites (`tests/minify_corpus.rs`, `tests/minify_special.rs`)
    // cover all minification expectations; this unit test only adds the
    // stronger no-crash sweep (both minifiers over every prefix, where C++
    // sweeps the legacy minifier alone).
    #[test]
    fn no_crash_on_prefixes_both_minifiers() {
        let input = b"var x = 'asd \\' lse'\nvar y /*comment*/ = /regex/\nvar z = \"x =\" + x\n";
        let patterns = TokenizerPatterns::new();
        for end in 0..=input.len() {
            let _ = legacy_minify_js(&input[..end]);
            let _ = minify_utf8(&patterns, &input[..end]);
        }
    }
}
