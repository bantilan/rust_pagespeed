//! Streaming CSS URL scanner (`css_tag_scanner.{h,cc}`).
//!
//! Finds `@import` and `url()` uses (skipping strings and comments)
//! and rewrites them through a [`Transformer`]. This is the fallback
//! path when the stylesheet does not parse; HTML-element helpers
//! (`ParseCssElement`) belong to later steps.
#![forbid(unsafe_code)]

use crate::escapes::escape_url;
use crate::util::{is_html_space, trim_leading_whitespace, trim_whitespace};

/// `url(` marker (`kUriValue`).
pub const URI_VALUE: &[u8; 4] = b"url(";

/// Transformer outcome (`Transformer::TransformStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformStatus {
    /// URL replaced; serialize the new use.
    Success,
    /// URL unchanged; pass bytes through.
    NoChange,
    /// Fail the whole transform.
    Failure,
}

/// URL rewriter callback (`Transformer`).
pub trait Transformer {
    /// Possibly replaces `url` in place.
    fn transform(&mut self, url: &mut Vec<u8>) -> TransformStatus;
}

/// Lexing outcome (`LexResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lex {
    No,
    Yes,
    Interrupted,
}

/// Found URL flavor (`UrlKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UrlKind {
    None,
    Import,
    Url,
}

/// Consumes an expected literal (`EatLiteral`).
fn eat_literal(includes_end: bool, expected: &[u8], input: &mut &[u8]) -> Lex {
    if input.starts_with(expected) {
        *input = &input[expected.len()..];
        return Lex::Yes;
    }
    if includes_end {
        return Lex::No;
    }
    if input.len() >= expected.len() {
        return Lex::No;
    }
    Lex::Interrupted
}

/// Extracts until a terminator (`CssExtractUntil`).
fn extract_until(
    is_string: bool,
    includes_end: bool,
    term: u8,
    input: &mut &[u8],
    out: &mut Vec<u8>,
    found_term: &mut bool,
) -> Lex {
    *found_term = false;
    let original = *input;
    out.clear();
    while let Some((&byte, _)) = input.split_first() {
        *input = &input[1..];
        if byte == term {
            *found_term = true;
            return Lex::Yes;
        } else if byte == b'\\' {
            let Some((&escape, _)) = input.split_first() else {
                if includes_end {
                    return Lex::No;
                }
                *input = original;
                return Lex::Interrupted;
            };
            *input = &input[1..];
            match escape {
                b',' | b'"' | b'\'' | b'\\' | b'(' | b')' => {
                    out.push(escape);
                }
                b'\n' | b'\r' | b'\x0C' if is_string => {
                    if escape == b'\r' {
                        eat_literal(includes_end, b"\n", input);
                    }
                }
                _ => return Lex::No,
            }
        } else if !is_string && is_html_space(byte) {
            let rest = *input;
            let mut ahead = 0;
            let mut closed = false;
            let mut other = false;
            while ahead < rest.len() {
                let byte = rest[ahead];
                if is_html_space(byte) {
                    ahead += 1;
                } else if byte == term {
                    closed = true;
                    break;
                } else {
                    other = true;
                    break;
                }
            }
            if closed {
                out.extend_from_slice(&rest[..ahead]);
                *input = &rest[ahead..];
            } else if other {
                // Put the space back; extraction ends unterminated.
                *input = &original[original.len() - input.len() - 1..];
                return Lex::Yes;
            } else {
                *input = &rest[ahead..];
            }
        } else if byte == b'\n' || byte == b'\r' || byte == b'\x0C' {
            *input = &original[original.len() - input.len() - 1..];
            break;
        } else {
            out.push(byte);
        }
    }
    if !includes_end {
        *input = original;
        return Lex::Interrupted;
    }
    Lex::Yes
}

/// Extracts a quoted string (`CssExtractString`).
fn extract_string(
    includes_end: bool,
    input: &mut &[u8],
    out: &mut Vec<u8>,
    quote: &mut u8,
    found_term: &mut bool,
) -> Lex {
    if input.starts_with(b"'") || input.starts_with(b"\"") {
        let original = *input;
        *quote = input[0];
        *input = &input[1..];
        let result = extract_until(true, includes_end, *quote, input, out, found_term);
        if result == Lex::No {
            *input = original;
        }
        return result;
    }
    if input.is_empty() && !includes_end {
        return Lex::Interrupted;
    }
    Lex::No
}

/// Skips a string body (`CssSkipString`).
fn skip_string(includes_end: bool, quote: u8, input: &mut &[u8]) -> Lex {
    while let Some((&byte, _)) = input.split_first() {
        *input = &input[1..];
        if byte == quote || byte == b'\n' || byte == b'\r' || byte == b'\x0C' {
            return Lex::Yes;
        } else if byte == b'\\' {
            if let Some((&next, _)) = input.split_first() {
                *input = &input[1..];
                if next == b'\r' {
                    eat_literal(includes_end, b"\n", input);
                }
            }
        }
    }
    if !includes_end {
        return Lex::Interrupted;
    }
    Lex::Yes
}

/// Skips a comment body (`CssSkipComment`).
fn skip_comment(includes_end: bool, input: &mut &[u8]) -> Lex {
    if let Some(end) = find_subslice(input, b"*/") {
        *input = &input[end + 2..];
        return Lex::Yes;
    }
    if !includes_end {
        return Lex::Interrupted;
    }
    *input = &input[input.len()..];
    Lex::Yes
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The scanner (`CssTagScanner`).
#[derive(Debug, Default)]
pub struct Scanner {
    reparse: Vec<u8>,
}

impl Scanner {
    /// Empty scanner.
    #[must_use]
    pub fn new() -> Scanner {
        Scanner::default()
    }

    /// Bytes retained for the next chunk (`RetainedForReparse`).
    #[must_use]
    pub fn retained_for_reparse(&self) -> &[u8] {
        &self.reparse
    }

    /// Transforms URLs in one chunk (`TransformUrlsStreaming`).
    /// `includes_end` marks the final chunk. Appends output to `out`.
    pub fn transform_streaming(
        &mut self,
        contents: &[u8],
        includes_end: bool,
        transformer: &mut dyn Transformer,
        out: &mut Vec<u8>,
    ) -> bool {
        let mut owned;
        let contents = if self.reparse.is_empty() {
            contents
        } else {
            owned = self.reparse.clone();
            owned.extend_from_slice(contents);
            self.reparse.clear();
            owned.as_slice()
        };
        // Byte offsets; `contents` borrows `owned`/`self.reparse`
        // above, so copy out before any mutation below.
        let buffer = contents.to_vec();
        let mut out_begin = 0;
        let mut out_end = 0;
        let mut pos = 0;
        let mut reparse_candidate = 0;
        while pos < buffer.len() {
            let byte = buffer[pos];
            pos += 1;
            let mut remaining = &buffer[pos..];
            let mut have_url = UrlKind::None;
            let mut is_quoted = false;
            let mut have_term_quote = false;
            let mut have_term_paren = false;
            let mut quote = b'?';
            let mut url = Vec::new();
            if byte == b'@' {
                match eat_literal(includes_end, b"import", &mut remaining) {
                    Lex::Yes => {
                        trim_leading_whitespace(&mut remaining);
                        match extract_string(
                            includes_end,
                            &mut remaining,
                            &mut url,
                            &mut quote,
                            &mut have_term_quote,
                        ) {
                            Lex::Yes => {
                                have_url = UrlKind::Import;
                                is_quoted = true;
                            }
                            Lex::Interrupted => {
                                self.reparse = buffer[reparse_candidate..].to_vec();
                                out.extend_from_slice(&buffer[out_begin..out_end]);
                                return true;
                            }
                            Lex::No => {}
                        }
                    }
                    Lex::Interrupted => {
                        self.reparse = buffer[reparse_candidate..].to_vec();
                        out.extend_from_slice(&buffer[out_begin..out_end]);
                        return true;
                    }
                    Lex::No => {}
                }
            } else if byte == b'u' {
                match eat_literal(includes_end, b"rl(", &mut remaining) {
                    Lex::Yes => {
                        trim_leading_whitespace(&mut remaining);
                        match extract_string(
                            includes_end,
                            &mut remaining,
                            &mut url,
                            &mut quote,
                            &mut have_term_quote,
                        ) {
                            Lex::Yes => {
                                trim_leading_whitespace(&mut remaining);
                                match eat_literal(includes_end, b")", &mut remaining) {
                                    Lex::Yes => {
                                        have_url = UrlKind::Url;
                                        is_quoted = true;
                                        have_term_paren = true;
                                    }
                                    Lex::Interrupted => {
                                        self.reparse = buffer[reparse_candidate..].to_vec();
                                        out.extend_from_slice(&buffer[out_begin..out_end]);
                                        return true;
                                    }
                                    Lex::No => {}
                                }
                            }
                            Lex::Interrupted => {
                                self.reparse = buffer[reparse_candidate..].to_vec();
                                out.extend_from_slice(&buffer[out_begin..out_end]);
                                return true;
                            }
                            Lex::No => {
                                if !remaining.starts_with(b"'") && !remaining.starts_with(b"\"") {
                                    let mut wrapped = Vec::new();
                                    match extract_until(
                                        false,
                                        includes_end,
                                        b')',
                                        &mut remaining,
                                        &mut wrapped,
                                        &mut have_term_paren,
                                    ) {
                                        Lex::Yes => {
                                            url = trim_whitespace(&wrapped).to_vec();
                                            have_url = UrlKind::Url;
                                        }
                                        Lex::Interrupted => {
                                            self.reparse = buffer[reparse_candidate..].to_vec();
                                            out.extend_from_slice(&buffer[out_begin..out_end]);
                                            return true;
                                        }
                                        Lex::No => {}
                                    }
                                }
                            }
                        }
                    }
                    Lex::Interrupted => {
                        self.reparse = buffer[reparse_candidate..].to_vec();
                        out.extend_from_slice(&buffer[out_begin..out_end]);
                        return true;
                    }
                    Lex::No => {}
                }
            } else if byte == b'"' || byte == b'\'' {
                if skip_string(includes_end, byte, &mut remaining) == Lex::Interrupted {
                    self.reparse = buffer[reparse_candidate..].to_vec();
                    out.extend_from_slice(&buffer[out_begin..out_end]);
                    return true;
                }
            } else if byte == b'/' {
                let mut comment = eat_literal(includes_end, b"*", &mut remaining);
                if comment == Lex::Yes {
                    comment = skip_comment(includes_end, &mut remaining);
                }
                if comment == Lex::Interrupted {
                    self.reparse = buffer[reparse_candidate..].to_vec();
                    out.extend_from_slice(&buffer[out_begin..out_end]);
                    return true;
                }
            } else if byte == b'\\' {
                if remaining.is_empty() {
                    if !includes_end {
                        self.reparse = buffer[reparse_candidate..].to_vec();
                        out.extend_from_slice(&buffer[out_begin..out_end]);
                        return true;
                    }
                } else {
                    remaining = &remaining[1..];
                }
            }
            pos = buffer.len() - remaining.len();
            if have_url != UrlKind::None {
                match transformer.transform(&mut url) {
                    TransformStatus::Success => {
                        out.extend_from_slice(&buffer[out_begin..out_end]);
                        serialize_url_use(
                            have_url,
                            &url,
                            is_quoted,
                            have_term_quote,
                            quote,
                            have_term_paren,
                            out,
                        );
                        out_begin = pos;
                    }
                    TransformStatus::Failure => return false,
                    TransformStatus::NoChange => {}
                }
            }
            out_end = pos;
            reparse_candidate = pos;
        }
        out.extend_from_slice(&buffer[out_begin..out_end]);
        true
    }
}

/// Serializes a rewritten URL use (`SerializeUrlUse`).
fn serialize_url_use(
    kind: UrlKind,
    url: &[u8],
    is_quoted: bool,
    have_term_quote: bool,
    quote: u8,
    have_term_paren: bool,
    out: &mut Vec<u8>,
) {
    debug_assert_ne!(kind, UrlKind::None);
    if kind == UrlKind::Import {
        out.extend_from_slice(b"@import ");
    } else {
        out.extend_from_slice(b"url(");
    }
    if is_quoted {
        out.push(quote);
    }
    out.extend_from_slice(&escape_url(url));
    if have_term_quote {
        out.push(quote);
    }
    if have_term_paren {
        out.push(b')');
    }
}

/// Transforms all URLs in complete input (`TransformUrls`).
/// Returns `None` when the transformer fails.
#[must_use]
pub fn transform_urls(contents: &[u8], transformer: &mut dyn Transformer) -> Option<Vec<u8>> {
    let mut scanner = Scanner::new();
    let mut out = Vec::new();
    if scanner.transform_streaming(contents, true, transformer, &mut out) {
        Some(out)
    } else {
        None
    }
}

/// Whether the input mentions `@import` (`HasImport`, case-insensitive).
#[must_use]
pub fn has_import(contents: &[u8]) -> bool {
    let mut pos = 0;
    while let Some(at) = contents[pos..].iter().position(|byte| *byte == b'@') {
        let rest = &contents[pos + at + 1..];
        if rest.len() >= 6 && rest[..6].eq_ignore_ascii_case(b"import") {
            return true;
        }
        pos += at + 1;
    }
    false
}

/// Whether the input contains `url(` (`HasUrl`, case-sensitive).
#[must_use]
pub fn has_url(contents: &[u8]) -> bool {
    find_subslice(contents, b"url(").is_some()
}

/// Whether a `rel` value names a stylesheet (`IsStylesheetOrAlternate`).
#[must_use]
pub fn is_stylesheet_or_alternate(attribute_value: &[u8]) -> bool {
    attribute_value
        .split(|byte| *byte == b' ')
        .any(|word| !word.is_empty() && word.eq_ignore_ascii_case(b"stylesheet"))
}

/// Whether a `rel` value names an alternate stylesheet
/// (`IsAlternateStylesheet`).
#[must_use]
pub fn is_alternate_stylesheet(attribute_value: &[u8]) -> bool {
    let mut has_stylesheet = false;
    let mut has_alternate = false;
    for word in attribute_value.split(|byte| *byte == b' ') {
        if word.eq_ignore_ascii_case(b"stylesheet") {
            has_stylesheet = true;
        } else if word.eq_ignore_ascii_case(b"alternate") {
            has_alternate = true;
        }
    }
    has_stylesheet && has_alternate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_vectors() {
        assert!(has_import(b"a @import b"));
        assert!(has_import(b"a @IMPORT b"));
        assert!(!has_import(b"a @media b"));
        assert!(!has_import(b"a @ b"));
        assert!(has_url(b"a url(b"));
        assert!(!has_url(b"a URL(b"));
        assert!(is_stylesheet_or_alternate(b"stylesheet"));
        assert!(is_stylesheet_or_alternate(b"alternate stylesheet"));
        assert!(!is_stylesheet_or_alternate(b"icon"));
        assert!(is_alternate_stylesheet(b"alternate stylesheet"));
        assert!(!is_alternate_stylesheet(b"stylesheet"));
    }
}
