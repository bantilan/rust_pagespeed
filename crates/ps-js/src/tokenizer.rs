//! Lossless JavaScript tokenizer (`js_tokenizer.{h,cc}`).
//!
//! Breaks input into tokens (comments and whitespace included, so token
//! text concatenates back to the input byte-for-byte), classifies
//! whitespace for semicolon insertion, and disambiguates division from
//! regex literals via a minimal parse stack. Regexes mirror the RE2
//! patterns (`bytes::Regex` with `\A` anchors; RE2's byte-matching `.`
//! becomes `[\x00-\xFF]`, since `(?s:.)` rejects invalid UTF-8 bytes);
//! numerics are hand-matched for POSIX leftmost-longest semantics.
#![forbid(unsafe_code)]

use crate::keywords::{lookup, TokenType};
use regex::bytes::Regex;
use std::collections::VecDeque;

/// Compiled token patterns (`JsTokenizerPatterns`).
#[derive(Debug)]
pub struct TokenizerPatterns {
    identifier: Regex,
    identifier_continue_char: Regex,
    line_comment: Regex,
    operator: Regex,
    regex_literal: Regex,
    string_literal: Regex,
    whitespace: Regex,
    line_continuation: Regex,
}

impl TokenizerPatterns {
    /// Compiles the patterns (share one instance across tokenizers).
    ///
    /// Every pattern is `\A`-anchored (RE2::Consume semantics). RE2 matches
    /// `.` and `[^...]` byte-wise (a lone invalid-UTF-8 byte matches as one
    /// unit), while this crate's unicode classes require valid UTF-8, so:
    /// - "any byte" is `(?-u:[\x00-\xFF])`;
    /// - negated classes with ASCII-only exclusions use `(?-u:[^...])`, which
    ///   is byte-exact, plus a high-byte fallback where the class stays in
    ///   unicode mode;
    /// - U+2028/U+2029 exclusions are spelled with the `E2D` fragment below
    ///   (a `\xE2` byte unless it starts U+2028/U+2029), since a negated
    ///   unicode class cannot be combined with a byte fallback;
    /// - ZWNJ/ZWJ/BOM use `\u{...}` codepoints: `\xE2` in unicode mode means
    ///   U+00E2, not the raw byte.
    #[must_use]
    pub fn new() -> TokenizerPatterns {
        // Any single byte, valid UTF-8 or not.
        let any = r"(?-u:[\x00-\xFF])";
        // A `\xE2` byte unless it starts U+2028/U+2029 (`\xE2\x80\xA8|A9`).
        let e2d = r"(?-u:\xE2(?:\z|[\x00-\x7F\x81-\xFF]|\x80(?:\z|[\x00-\xA7\xAA-\xFF])))";
        // Regex-literal body char: not `/`, `\`, `[`, CR, LF, U+2028/29.
        let body = format!(r"(?:(?-u:[^/\\\x5b\r\n\xE2])|{e2d})");
        // Class-content char: not `]`, `\`, CR, LF, U+2028/29.
        let class_char = format!(r"(?:(?-u:[^\\\]\r\n\xE2])|{e2d})");
        // Escape follower: not CR, LF, U+2028/29.
        let follower = format!(r"(?:(?-u:[^\r\n\xE2])|{e2d})");
        // Identifier characters (ZWNJ/ZWJ continue, but never start).
        let id_start = r"[$_\p{Lu}\p{Ll}\p{Lt}\p{Lm}\p{Lo}\p{Nl}]";
        let id_continue =
            r"[$_\p{Lu}\p{Ll}\p{Lt}\p{Lm}\p{Lo}\p{Nl}\p{Mn}\p{Mc}\p{Nd}\p{Pc}\u{200c}\u{200d}]";
        let escape = r"\\u[0-9A-Fa-f]{4}";
        let identifier = format!(r"({id_start}|{escape})({id_continue}|{escape})*");
        let flags = format!(r"({id_continue}|{escape})*");
        let regex_literal =
            format!(r"\A/(?:{body}|\x5b(?:{class_char}|\\{follower})*\x5d|\\{follower})+/{flags}");
        // The string pattern needs `"` inside, so it is built with format!.
        let string = format!(
            "\\A(?:'(?:{any}*?(?:\\\\(?:\\r\\n|\\n\\r|\\n|{any}))?)*?['\n\r\\p{{Zl}}\\p{{Zp}}]|\"(?:{any}*?(?:\\\\(?:\\r\\n|\\n\\r|\\n|{any}))?)*?[\"\n\r\\p{{Zl}}\\p{{Zp}}])"
        );
        // A high byte (fallback for ASCII-exclusion negated classes, where
        // the unicode alternative only fails on invalid UTF-8).
        let high = r"(?-u:[\x80-\xFF])";
        TokenizerPatterns {
            identifier: Regex::new(&format!(r"\A(?:{identifier})")).unwrap(),
            identifier_continue_char: Regex::new(&format!(r"\A(?:{id_continue}|\\|{escape})"))
                .unwrap(),
            line_comment: Regex::new(&format!(
                r"\A(?://|<!--|-->){any}*?([\r\n\p{{Zl}}\p{{Zp}}]|\z)"
            ))
            .unwrap(),
            // NOTE: C++ spells the middle alternative `[!=]{0,2}`, but that
            // spelling cannot produce its own test suite's expectations
            // under any RE2 semantics: `===`/`!==` (3 chars, one token)
            // exceed `{0,2}`, `!!` must split into two tokens, and the
            // empty match would shadow `<`/`>` (later alternatives) while
            // `Emit` asserts `num_chars > 0`. Verified against real RE2
            // (vendored `third_party/re2`): it yields `!!`/`==`+`=`, i.e.
            // the committed C++ tests fail against the committed C++
            // pattern. The tests encode correct maximal-munch JS lexing,
            // so the port implements that: `(?:!={0,2}|={1,3})`.
            operator: Regex::new(
                r"\A(?:&&|\|\||\+\+|--|~|[*/%^&|+-]=?|(?:!={0,2}|={1,3})|<{1,2}=?|>{1,3}=?)",
            )
            .unwrap(),
            regex_literal: Regex::new(&regex_literal).unwrap(),
            string_literal: Regex::new(&string).unwrap(),
            whitespace: Regex::new(r"\A(?:([\n\r\p{Zl}\p{Zp}])|[ \f\t\v\u{feff}\p{Zs}])+").unwrap(),
            // NOTE: the C++ pattern ends with `(in|instanceof)($|[^...])`,
            // but a negated `\p` class is not byte-exact here; that case is
            // tested in code (`after_in_keyword`), which is equivalent.
            line_continuation: Regex::new(&format!(
                r"\A(?:[=(*/%^&|<>?:,.]|!=|\+(?:$|[^+]|{high})|-(?:$|[^-]|{high}))"
            ))
            .unwrap(),
        }
    }
}

impl Default for TokenizerPatterns {
    fn default() -> Self {
        TokenizerPatterns::new()
    }
}

/// Process-wide compiled patterns, built once on first use.
///
/// Compilation costs ~1ms (eight regexes); every JavaScript filter
/// used to pay it per request (~4 builds per HTML page). The table
/// is immutable after construction and `Regex` is `Send + Sync`,
/// so sharing one instance across requests and threads is safe.
static SHARED_PATTERNS: std::sync::OnceLock<TokenizerPatterns> = std::sync::OnceLock::new();

impl TokenizerPatterns {
    /// Borrows the shared table, compiling it once (`shared`).
    #[must_use]
    pub fn shared() -> &'static TokenizerPatterns {
        SHARED_PATTERNS.get_or_init(TokenizerPatterns::new)
    }
}

/// Parse states (`JsTokenizer::ParseState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    StartOfInput,
    Expression,
    Operator,
    Period,
    QuestionMark,
    OpenBrace,
    OpenBracket,
    OpenParen,
    BlockKeyword,
    BlockHeader,
    ReturnThrow,
    JumpKeyword,
    OtherKeyword,
}

/// JSON-detection steps (`JsonStep`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonStep {
    Start,
    OpenBrace,
    OpenBraceString,
    IsObject,
    IsNotObject,
}

/// The tokenizer (`JsTokenizer`).
#[derive(Debug)]
pub struct Tokenizer<'a> {
    patterns: &'a TokenizerPatterns,
    input: &'a [u8],
    offset: usize,
    parse_stack: Vec<ParseState>,
    lookahead: VecDeque<(TokenType, (usize, usize))>,
    json_step: JsonStep,
    start_of_line: bool,
    error: bool,
}

impl<'a> Tokenizer<'a> {
    /// Borrows patterns and input (both must outlive the tokenizer).
    pub fn new(patterns: &'a TokenizerPatterns, input: &'a [u8]) -> Tokenizer<'a> {
        Tokenizer {
            patterns,
            input,
            offset: 0,
            parse_stack: vec![ParseState::StartOfInput],
            lookahead: VecDeque::new(),
            json_step: JsonStep::Start,
            start_of_line: true,
            error: false,
        }
    }

    /// Whether an error latched (`has_error`).
    #[must_use]
    pub fn has_error(&self) -> bool {
        self.error
    }

    /// Parse-stack rendering for tests (`ParseStackForTest`).
    #[must_use]
    pub fn parse_stack_for_test(&self) -> String {
        self.parse_stack
            .iter()
            .map(|state| match state {
                ParseState::StartOfInput => "Start",
                ParseState::Expression => "Expr",
                ParseState::Operator => "Oper",
                ParseState::Period => ".",
                ParseState::QuestionMark => "?",
                ParseState::OpenBrace => "{",
                ParseState::OpenBracket => "[",
                ParseState::OpenParen => "(",
                ParseState::BlockKeyword => "BkKwd",
                ParseState::BlockHeader => "BkHdr",
                ParseState::ReturnThrow => "RetTh",
                ParseState::JumpKeyword => "Jump",
                ParseState::OtherKeyword => "Other",
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Next token (`NextToken`): type plus byte range into the input.
    pub fn next_token(&mut self) -> (TokenType, (usize, usize)) {
        if let Some((kind, span)) = self.lookahead.pop_front() {
            return (kind, span);
        }
        if self.error {
            return self.error_token();
        }
        if self.offset >= self.input.len() {
            self.parse_stack.clear();
            return (TokenType::EndOfInput, (self.offset, self.offset));
        }
        let byte = self.input[self.offset];
        match byte {
            b' ' | b'\x0c' | b'\n' | b'\r' | b'\t' | b'\x0b' => {
                match self.try_consume_whitespace(true) {
                    Some((kind, span)) => (kind, span),
                    None => self.error_token(), // unreachable for ASCII ws
                }
            }
            b'{' => self.consume_open_brace(),
            b'}' => self.consume_close_brace(),
            b'[' => self.consume_open_bracket(),
            b']' => self.consume_close_bracket(),
            b'(' => self.consume_open_paren(),
            b')' => self.consume_close_paren(),
            b':' => self.consume_colon(),
            b',' => self.consume_comma(),
            b'.' => self.consume_period(),
            b'?' => self.consume_question_mark(),
            b';' => self.consume_semicolon(),
            b'/' => self.consume_slash(),
            b'\'' | b'"' => self.consume_string(),
            b'0'..=b'9' => self.consume_number(),
            _ => {
                if let Some(token) = self.try_consume_identifier_or_keyword() {
                    return token;
                }
                if let Some(token) = self.try_consume_comment() {
                    return token;
                }
                if let Some((kind, span)) = self.try_consume_whitespace(true) {
                    return (kind, span);
                }
                self.consume_operator()
            }
        }
    }

    /// Token text for a span.
    #[must_use]
    pub fn text(&self, span: (usize, usize)) -> &'a [u8] {
        // Copy the input reference first so the slice outlives `&self`.
        let input: &'a [u8] = self.input;
        &input[span.0..span.1]
    }

    fn remaining(&self) -> &'a [u8] {
        &self.input[self.offset..]
    }

    fn error_token(&mut self) -> (TokenType, (usize, usize)) {
        self.error = true;
        let span = (self.offset, self.input.len());
        self.offset = self.input.len();
        (TokenType::Error, span)
    }

    /// Emits `len` bytes (`Emit`), tracking line starts and JSON shape.
    fn emit(&mut self, kind: TokenType, len: usize) -> (TokenType, (usize, usize)) {
        debug_assert!(len > 0);
        let span = (self.offset, self.offset + len);
        let text = &self.input[span.0..span.1];
        if !matches!(
            kind,
            TokenType::Comment
                | TokenType::Whitespace
                | TokenType::LineSeparator
                | TokenType::SemiInsert
        ) {
            self.start_of_line = false;
            match self.json_step {
                JsonStep::Start => {
                    if kind == TokenType::Operator && text == b"{" {
                        self.json_step = JsonStep::OpenBrace;
                    } else {
                        self.json_step = JsonStep::IsNotObject;
                    }
                }
                JsonStep::OpenBrace => {
                    if kind == TokenType::StringLiteral {
                        self.json_step = JsonStep::OpenBraceString;
                    } else {
                        self.json_step = JsonStep::IsNotObject;
                    }
                }
                JsonStep::OpenBraceString => {
                    if kind == TokenType::Operator && text == b":" {
                        self.json_step = JsonStep::IsObject;
                        // Stack is [Start, {]; reshape to [Start, Oper, {, Oper].
                        debug_assert_eq!(self.parse_stack.len(), 2);
                        self.parse_stack.pop();
                        self.parse_stack.push(ParseState::Operator);
                        self.parse_stack.push(ParseState::OpenBrace);
                        self.parse_stack.push(ParseState::Operator);
                    } else {
                        self.json_step = JsonStep::IsNotObject;
                    }
                }
                JsonStep::IsObject | JsonStep::IsNotObject => {}
            }
        }
        self.offset += len;
        (kind, span)
    }

    fn push_block_header(&mut self) {
        if *self.parse_stack.last().unwrap_or(&ParseState::StartOfInput) != ParseState::BlockHeader
        {
            self.parse_stack.push(ParseState::BlockHeader);
        }
    }

    fn push_expression(&mut self) {
        while matches!(
            self.parse_stack.last(),
            Some(ParseState::Expression | ParseState::Operator | ParseState::Period)
        ) {
            self.parse_stack.pop();
        }
        self.parse_stack.push(ParseState::Expression);
    }

    fn push_operator(&mut self) {
        if *self.parse_stack.last().unwrap_or(&ParseState::StartOfInput) != ParseState::Operator {
            self.parse_stack.push(ParseState::Operator);
        }
    }

    fn can_precede_object_literal(state: ParseState) -> bool {
        matches!(
            state,
            ParseState::Operator
                | ParseState::QuestionMark
                | ParseState::OpenBracket
                | ParseState::OpenParen
                | ParseState::ReturnThrow
        )
    }

    fn back(&self) -> ParseState {
        *self.parse_stack.last().unwrap_or(&ParseState::StartOfInput)
    }

    fn consume_open_brace(&mut self) -> (TokenType, (usize, usize)) {
        match self.back() {
            ParseState::Expression
            | ParseState::Period
            | ParseState::BlockKeyword
            | ParseState::JumpKeyword
            | ParseState::OtherKeyword => self.error_token(),
            _ => {
                self.parse_stack.push(ParseState::OpenBrace);
                self.emit(TokenType::Operator, 1)
            }
        }
    }

    fn consume_close_brace(&mut self) -> (TokenType, (usize, usize)) {
        loop {
            match self.back() {
                ParseState::OpenBrace => {
                    self.parse_stack.pop();
                    break;
                }
                ParseState::StartOfInput
                | ParseState::OpenBracket
                | ParseState::OpenParen
                | ParseState::BlockKeyword => return self.error_token(),
                _ => {
                    self.parse_stack.pop();
                }
            }
        }
        if self.back() == ParseState::BlockHeader {
            self.parse_stack.pop();
        }
        if Self::can_precede_object_literal(self.back()) {
            self.push_expression();
        }
        self.emit(TokenType::Operator, 1)
    }

    fn consume_open_bracket(&mut self) -> (TokenType, (usize, usize)) {
        match self.back() {
            ParseState::Period
            | ParseState::BlockKeyword
            | ParseState::JumpKeyword
            | ParseState::OtherKeyword => self.error_token(),
            _ => {
                self.parse_stack.push(ParseState::OpenBracket);
                self.emit(TokenType::Operator, 1)
            }
        }
    }

    fn consume_close_bracket(&mut self) -> (TokenType, (usize, usize)) {
        loop {
            match self.back() {
                ParseState::OpenBracket => {
                    self.parse_stack.pop();
                    break;
                }
                ParseState::StartOfInput
                | ParseState::OpenBrace
                | ParseState::OpenParen
                | ParseState::BlockKeyword
                | ParseState::BlockHeader => return self.error_token(),
                _ => {
                    self.parse_stack.pop();
                }
            }
        }
        self.push_expression();
        self.emit(TokenType::Operator, 1)
    }

    fn consume_open_paren(&mut self) -> (TokenType, (usize, usize)) {
        match self.back() {
            ParseState::Period | ParseState::JumpKeyword | ParseState::OtherKeyword => {
                self.error_token()
            }
            _ => {
                self.parse_stack.push(ParseState::OpenParen);
                self.emit(TokenType::Operator, 1)
            }
        }
    }

    fn consume_close_paren(&mut self) -> (TokenType, (usize, usize)) {
        loop {
            match self.back() {
                ParseState::OpenParen => {
                    self.parse_stack.pop();
                    break;
                }
                ParseState::StartOfInput
                | ParseState::OpenBrace
                | ParseState::OpenBracket
                | ParseState::BlockKeyword
                | ParseState::BlockHeader => return self.error_token(),
                _ => {
                    self.parse_stack.pop();
                }
            }
        }
        if self.back() == ParseState::BlockKeyword {
            self.parse_stack.pop();
            self.push_block_header();
        } else {
            self.push_expression();
        }
        self.emit(TokenType::Operator, 1)
    }

    fn consume_block_comment(&mut self) -> (TokenType, (usize, usize)) {
        let rest = self.remaining();
        // Find "*/" from offset 2 (the opening "/*" cannot close itself).
        let body = &rest[2.min(rest.len())..];
        match body.windows(2).position(|w| w == b"*/") {
            Some(pos) => self.emit(TokenType::Comment, 2 + pos + 2),
            None => self.error_token(),
        }
    }

    fn consume_line_comment(&mut self) -> (TokenType, (usize, usize)) {
        let rest = self.remaining();
        let captures = match self.patterns.line_comment.captures(rest) {
            Some(captures) => captures,
            None => return self.error_token(),
        };
        let full = captures.get(0).map_or(0, |m| m.end());
        let linebreak = captures.get(1).map_or(0, |m| m.len());
        self.emit(TokenType::Comment, full - linebreak)
    }

    fn try_consume_comment(&mut self) -> Option<(TokenType, (usize, usize))> {
        let rest = self.remaining();
        if rest.starts_with(b"/*") {
            return Some(self.consume_block_comment());
        }
        if rest.starts_with(b"//")
            || rest.starts_with(b"<!--")
            || (self.start_of_line && rest.starts_with(b"-->"))
        {
            return Some(self.consume_line_comment());
        }
        None
    }

    fn consume_colon(&mut self) -> (TokenType, (usize, usize)) {
        loop {
            match self.back() {
                ParseState::QuestionMark => {
                    self.parse_stack.pop();
                    self.push_operator();
                    return self.emit(TokenType::Operator, 1);
                }
                ParseState::StartOfInput | ParseState::BlockHeader => {
                    return self.emit(TokenType::Operator, 1);
                }
                ParseState::OpenBrace => {
                    let len = self.parse_stack.len();
                    if Self::can_precede_object_literal(self.parse_stack[len - 2]) {
                        self.push_operator();
                    }
                    return self.emit(TokenType::Operator, 1);
                }
                ParseState::Expression | ParseState::OtherKeyword => {
                    self.parse_stack.pop();
                }
                _ => return self.error_token(),
            }
        }
    }

    fn consume_comma(&mut self) -> (TokenType, (usize, usize)) {
        if self.back() == ParseState::Expression {
            let len = self.parse_stack.len();
            let prev = self.parse_stack[len - 2];
            if prev == ParseState::OtherKeyword
                || prev == ParseState::OpenBracket
                || (prev == ParseState::OpenBrace
                    && Self::can_precede_object_literal(self.parse_stack[len - 3]))
            {
                self.parse_stack.pop();
            } else {
                self.push_operator();
            }
        } else if self.back() != ParseState::OpenBracket {
            return self.error_token();
        }
        self.emit(TokenType::Operator, 1)
    }

    fn try_consume_identifier_or_keyword(&mut self) -> Option<(TokenType, (usize, usize))> {
        // ASCII fast path (falls back to the Unicode regex on non-ASCII or
        // backslashes, where only `\uXXXX` escapes are identifier chars).
        let rest = self.remaining();
        let first = *rest.first()?;
        let ascii_start = first.is_ascii_alphabetic() || first == b'_' || first == b'$';
        let mut use_regex = first >= 0x80 || first == b'\\';
        let mut index = 0;
        if !use_regex && ascii_start {
            index = 1;
            while index < rest.len() {
                let byte = rest[index];
                if byte >= 0x80 || byte == b'\\' {
                    use_regex = true;
                    break;
                }
                if !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$') {
                    break;
                }
                index += 1;
            }
        } else if !use_regex {
            return None;
        }
        if use_regex {
            let matched = self
                .patterns
                .identifier
                .find(rest)
                .map(|m| m.end())
                .unwrap_or(0);
            if matched == 0 {
                return None;
            }
            index = matched;
        }
        debug_assert!(index > 0);
        let word = std::str::from_utf8(&rest[..index]).unwrap_or("");
        let (mut kind, _) = lookup(word);
        // Reserved-after-period / object-property names are identifiers.
        let back = self.back();
        let len = self.parse_stack.len();
        if back == ParseState::Period
            || (back == ParseState::OpenBrace
                && Self::can_precede_object_literal(self.parse_stack[len - 2]))
        {
            self.push_expression();
            return Some(self.emit(TokenType::Identifier, index));
        }
        match kind {
            TokenType::NotAKeyword
            | TokenType::Implements
            | TokenType::Interface
            | TokenType::Let
            | TokenType::Package
            | TokenType::Private
            | TokenType::Protected
            | TokenType::Public
            | TokenType::Static
            | TokenType::Yield => {
                kind = TokenType::Identifier;
                if self.back() != ParseState::BlockKeyword {
                    self.push_expression();
                }
            }
            TokenType::False | TokenType::Null | TokenType::This | TokenType::True => {
                self.push_expression();
            }
            TokenType::Catch
            | TokenType::For
            | TokenType::Function
            | TokenType::If
            | TokenType::Switch
            | TokenType::While
            | TokenType::With => {
                self.parse_stack.push(ParseState::BlockKeyword);
            }
            TokenType::Do | TokenType::Else | TokenType::Finally | TokenType::Try => {
                self.push_block_header();
            }
            TokenType::Case
            | TokenType::Delete
            | TokenType::In
            | TokenType::Instanceof
            | TokenType::New
            | TokenType::Typeof
            | TokenType::Void => {
                self.push_operator();
            }
            TokenType::Return | TokenType::Throw => {
                self.parse_stack.push(ParseState::ReturnThrow);
            }
            TokenType::Break | TokenType::Continue | TokenType::Debugger => {
                self.parse_stack.push(ParseState::JumpKeyword);
            }
            TokenType::Const | TokenType::Default | TokenType::Var => {
                self.parse_stack.push(ParseState::OtherKeyword);
            }
            TokenType::Class
            | TokenType::Enum
            | TokenType::Export
            | TokenType::Extends
            | TokenType::Import
            | TokenType::Super => {
                return Some(self.error_token());
            }
            _ => {
                return Some(self.error_token());
            }
        }
        Some(self.emit(kind, index))
    }

    fn consume_number(&mut self) -> (TokenType, (usize, usize)) {
        match match_numeric_literal(self.remaining()) {
            Some(len) if len > 0 => {
                self.push_expression();
                self.emit(TokenType::Number, len)
            }
            _ => self.error_token(),
        }
    }

    fn consume_operator(&mut self) -> (TokenType, (usize, usize)) {
        let len = self
            .patterns
            .operator
            .find(self.remaining())
            .map(|m| m.end())
            .unwrap_or(0);
        if len == 0 {
            // Unrecognized character.
            return self.error_token();
        }
        let (kind, span) = self.emit(TokenType::Operator, len);
        let token = &self.input[span.0..span.1];
        if (token == b"++" || token == b"--") && self.back() == ParseState::Expression {
            // Postfix: leave the expression state alone.
        } else {
            self.push_operator();
        }
        (kind, span)
    }

    fn consume_period(&mut self) -> (TokenType, (usize, usize)) {
        let rest = self.remaining();
        if rest.len() >= 2 && rest[1].is_ascii_digit() {
            return self.consume_number();
        }
        self.parse_stack.push(ParseState::Period);
        self.emit(TokenType::Operator, 1)
    }

    fn consume_question_mark(&mut self) -> (TokenType, (usize, usize)) {
        if self.back() != ParseState::Expression {
            return self.error_token();
        }
        self.parse_stack.push(ParseState::QuestionMark);
        self.emit(TokenType::Operator, 1)
    }

    fn consume_regex(&mut self) -> (TokenType, (usize, usize)) {
        let len = self
            .patterns
            .regex_literal
            .find(self.remaining())
            .map(|m| m.end())
            .unwrap_or(0);
        if len == 0 {
            return self.error_token();
        }
        self.push_expression();
        self.emit(TokenType::Regex, len)
    }

    fn consume_semicolon(&mut self) -> (TokenType, (usize, usize)) {
        loop {
            match self.back() {
                ParseState::OpenBracket => return self.error_token(),
                ParseState::OpenParen => {
                    let len = self.parse_stack.len();
                    if self.parse_stack[len - 2] != ParseState::BlockKeyword {
                        return self.error_token();
                    }
                    break;
                }
                ParseState::StartOfInput | ParseState::OpenBrace => break,
                _ => {
                    self.parse_stack.pop();
                }
            }
        }
        self.emit(TokenType::Operator, 1)
    }

    fn consume_slash(&mut self) -> (TokenType, (usize, usize)) {
        let rest = self.remaining();
        if rest.len() >= 2 {
            if rest[1] == b'/' {
                return self.consume_line_comment();
            } else if rest[1] == b'*' {
                return self.consume_block_comment();
            }
        }
        match self.back() {
            ParseState::Expression => self.consume_operator(),
            ParseState::StartOfInput
            | ParseState::Operator
            | ParseState::QuestionMark
            | ParseState::OpenBrace
            | ParseState::OpenBracket
            | ParseState::OpenParen
            | ParseState::BlockHeader
            | ParseState::ReturnThrow => self.consume_regex(),
            _ => self.error_token(),
        }
    }

    fn consume_string(&mut self) -> (TokenType, (usize, usize)) {
        let rest = self.remaining();
        let quote = rest[0];
        let len = self
            .patterns
            .string_literal
            .find(rest)
            .map(|m| m.end())
            .unwrap_or(0);
        if len == 0 || rest[len - 1] != quote {
            return self.error_token();
        }
        // len > 0 here, so len - 1 cannot underflow (checked above).
        // Reject literals closed on an escaped quote (odd backslash run).
        let mut backslashes = 0;
        let close = len - 1;
        while backslashes + 1 < close && rest[close - 1 - backslashes] == b'\\' {
            backslashes += 1;
        }
        if backslashes % 2 == 1 {
            return self.error_token();
        }
        self.push_expression();
        self.emit(TokenType::StringLiteral, len)
    }

    fn try_consume_whitespace(
        &mut self,
        allow_semicolon_insertion: bool,
    ) -> Option<(TokenType, (usize, usize))> {
        let rest = self.remaining();
        let mut has_linebreak = false;
        let mut use_regex = false;
        let mut size = 0;
        for byte in rest {
            if *byte >= 0x80 {
                use_regex = true;
                break;
            } else if *byte == b'\n' || *byte == b'\r' {
                has_linebreak = true;
            } else if !matches!(*byte, b' ' | b'\t' | b'\x0c' | b'\x0b') {
                break;
            }
            size += 1;
        }
        if use_regex {
            let captures = self.patterns.whitespace.captures(rest)?;
            has_linebreak = captures.get(1).is_some();
            size = captures.get(0).map_or(0, |m| m.end());
        }
        if size == 0 {
            return None;
        }
        let (_, span) = self.emit(TokenType::Whitespace, size);
        if has_linebreak {
            self.start_of_line = true;
            if allow_semicolon_insertion && self.try_insert_linebreak_semicolon() {
                return Some((TokenType::SemiInsert, span));
            }
            return Some((TokenType::LineSeparator, span));
        }
        Some((TokenType::Whitespace, span))
    }

    /// The `(in|instanceof)($|[^...])` arm of `kLineContinuationRegex`,
    /// tested in code: a negated `\p` class is not byte-exact in this
    /// crate's unicode mode, so the positive identifier-continue class is
    /// matched instead (equivalent by De Morgan, exact for both valid
    /// UTF-8 and raw bytes).
    fn after_in_keyword(&self) -> bool {
        let rest = self.remaining();
        // Longest first (equivalent to the regex alternation's backtrack).
        for keyword in [b"instanceof".as_slice(), b"in".as_slice()] {
            if rest.starts_with(keyword) {
                let after = &rest[keyword.len()..];
                return after.is_empty() || !self.patterns.identifier_continue_char.is_match(after);
            }
        }
        false
    }

    /// ASI decision (`TryInsertLinebreakSemicolon`).
    fn try_insert_linebreak_semicolon(&mut self) -> bool {
        debug_assert!(self.lookahead.is_empty());
        // Skip comments/whitespace into the lookahead queue.
        loop {
            if self.offset >= self.input.len() {
                break;
            }
            let checkpoint = (self.offset, self.json_step, self.start_of_line);
            let saved_stack = self.parse_stack.clone();
            if let Some(token) = self.try_consume_comment() {
                let _ = checkpoint;
                let _ = saved_stack;
                self.lookahead.push_back(token);
                continue;
            }
            // Whitespace lookahead must not itself insert semicolons.
            let before = self.offset;
            if let Some(token) = self.try_consume_whitespace(false) {
                let _ = before;
                self.lookahead.push_back(token);
                continue;
            }
            break;
        }
        if self.offset >= self.input.len()
            || self.input[self.offset] == b';'
            || self.input[self.offset] == b'}'
        {
            return false;
        }
        let insert = match self.back() {
            ParseState::StartOfInput
            | ParseState::OpenBrace
            | ParseState::OpenBracket
            | ParseState::OpenParen
            | ParseState::BlockKeyword
            | ParseState::BlockHeader => false,
            ParseState::Expression => {
                // Unclosed paren/bracket vetoes (for-loop headers).
                let mut veto = false;
                for state in self.parse_stack.iter().rev() {
                    match state {
                        ParseState::OpenParen | ParseState::OpenBracket => {
                            veto = true;
                            break;
                        }
                        ParseState::OpenBrace | ParseState::BlockHeader => break,
                        _ => {}
                    }
                }
                if veto {
                    false
                } else {
                    // Next token must not continue the statement.
                    self.patterns
                        .line_continuation
                        .find(self.remaining())
                        .is_none()
                        && !self.after_in_keyword()
                }
            }
            ParseState::Operator | ParseState::Period | ParseState::QuestionMark => false,
            ParseState::ReturnThrow | ParseState::JumpKeyword => true,
            ParseState::OtherKeyword => false,
        };
        if !insert {
            return false;
        }
        while !matches!(
            self.back(),
            ParseState::StartOfInput | ParseState::OpenBrace
        ) {
            self.parse_stack.pop();
        }
        true
    }
}

/// Matches a numeric literal with POSIX leftmost-longest semantics
/// (`kNumericLiteralPosixRegex`): hex, octal, or decimal with optional
/// fraction and exponent. Returns the match length.
fn match_numeric_literal(input: &[u8]) -> Option<usize> {
    if input.is_empty() {
        return None;
    }
    let mut best = 0;
    // Hex: 0[xX][0-9a-fA-F]+
    if input.len() >= 3
        && input[0] == b'0'
        && (input[1] == b'x' || input[1] == b'X')
        && input[2].is_ascii_hexdigit()
    {
        let mut len = 3;
        while len < input.len() && input[len].is_ascii_hexdigit() {
            len += 1;
        }
        best = best.max(len);
    }
    // Octal: 0[0-7]+
    if input.len() >= 2 && input[0] == b'0' && matches!(input[1], b'0'..=b'7') {
        let mut len = 2;
        while len < input.len() && matches!(input[len], b'0'..=b'7') {
            len += 1;
        }
        best = best.max(len);
    }
    // Decimal: int part, optional fraction, optional exponent. The int
    // part matches [1-9][0-9]*, 0-with-8-or-9, lone 0, or empty-with-dot.
    let mut pos = 0;
    let mut has_int = false;
    if input
        .first()
        .is_some_and(|byte| (b'1'..=b'9').contains(byte))
    {
        while pos < input.len() && input[pos].is_ascii_digit() {
            pos += 1;
        }
        has_int = true;
    } else if input.first() == Some(&b'0') {
        // 0, or 0 with 8/9 somewhere (else the octal branch owns it).
        let mut scan = 1;
        let mut has_89 = false;
        while scan < input.len() && input[scan].is_ascii_digit() {
            if input[scan] == b'8' || input[scan] == b'9' {
                has_89 = true;
            }
            scan += 1;
        }
        if has_89 || scan == 1 {
            pos = scan;
            has_int = true;
        } else {
            // Pure octal digits beyond the 0: decimal branch takes the
            // lone 0 only if a fraction/exponent follows (else octal won
            // with a longer match anyway).
            pos = 1;
            has_int = true;
        }
    }
    // Fraction: .[0-9]* (even empty) after an int part, or .[0-9]+ alone.
    let mut frac_end = pos;
    if pos < input.len() && input[pos] == b'.' {
        let mut end = pos + 1;
        while end < input.len() && input[end].is_ascii_digit() {
            end += 1;
        }
        frac_end = end;
    } else if !has_int && input.len() >= 2 && input[0] == b'.' && input[1].is_ascii_digit() {
        let mut end = 2;
        while end < input.len() && input[end].is_ascii_digit() {
            end += 1;
        }
        frac_end = end;
        has_int = true;
    }
    if has_int || frac_end > pos {
        // Exponent: [eE][+-]?[0-9]+ — only when digits follow.
        let mut end = frac_end.max(pos);
        if end < input.len() && (input[end] == b'e' || input[end] == b'E') {
            let mut exp = end + 1;
            if exp < input.len() && (input[exp] == b'+' || input[exp] == b'-') {
                exp += 1;
            }
            if exp < input.len() && input[exp].is_ascii_digit() {
                while exp < input.len() && input[exp].is_ascii_digit() {
                    exp += 1;
                }
                end = exp;
            }
        }
        // A bare "." with no int and no fraction digits is not a number
        // (but frac_end > pos with has_int false implies digits: covered).
        if end > 0 && (has_int || frac_end > 0) {
            // Exclude the lone-dot case: ".x" must not match.
            if !(frac_end == 1 && !has_int && input[0] == b'.') {
                best = best.max(end);
            }
        }
    }
    if best == 0 {
        return None;
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_patterns_are_one_instance() {
        // Regression: filters must reuse the process-wide table
        // instead of compiling eight regexes per request (~1ms).
        let first = TokenizerPatterns::shared() as *const TokenizerPatterns;
        let second = TokenizerPatterns::shared() as *const TokenizerPatterns;
        assert_eq!(first, second);
        // The shared table tokenizes identically to a fresh build.
        let fresh = TokenizerPatterns::new();
        let input = b"var x = 1; // hi\n";
        let mut a = Tokenizer::new(TokenizerPatterns::shared(), input);
        let mut b = Tokenizer::new(&fresh, input);
        loop {
            let ta = a.next_token();
            let tb = b.next_token();
            assert_eq!(ta, tb);
            if ta.0 == crate::keywords::TokenType::EndOfInput {
                break;
            }
        }
    }
}
