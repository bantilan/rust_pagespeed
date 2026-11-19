//! CSS output escaping (`tostring.{h,cc}` escape helpers).
//!
//! Byte-oriented: predicates classify raw bytes; non-ASCII bytes
//! always pass through.
#![forbid(unsafe_code)]

use crate::unicode_text::UnicodeText;

/// Whether a byte is safe unescaped in an unquoted URL (`IsUrlSafe`).
fn is_url_safe(byte: u8) -> bool {
    if (0x21..=0x7E).contains(&byte) {
        !matches!(
            byte,
            b' ' | b'"' | b'\'' | b'\\' | b'(' | b')' | b'{' | b'}' | b'[' | b']'
        )
    } else {
        // Non-ASCII passes; controls (incl. TAB) escape.
        byte >= 0x80
    }
}

/// Whether a byte is safe unescaped in a string (`IsStringSafe`).
fn is_string_safe(byte: u8) -> bool {
    byte == b' ' || is_url_safe(byte)
}

/// Whether a byte is safe unescaped in an identifier (`IsIdentSafe`).
fn is_ident_safe(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte >= 0x80
}

/// Escapes one ASCII byte (`AppendEscapedAsciiChar`): control
/// whitespace becomes a hex escape (`\A ` etc.), as does any hex
/// digit (`\32 ` for `2`, uppercase, space-terminated); all other
/// bytes take a plain backslash.
fn append_escaped(byte: u8, out: &mut Vec<u8>) {
    debug_assert!(byte.is_ascii());
    match byte {
        b'\n' => out.extend_from_slice(b"\\A "),
        b'\r' => out.extend_from_slice(b"\\D "),
        b'\x0C' => out.extend_from_slice(b"\\C "),
        b'\t' => out.extend_from_slice(b"\\9 "),
        _ if byte.is_ascii_hexdigit() => {
            out.push(b'\\');
            out.extend_from_slice(format!("{byte:X} ").as_bytes());
        }
        _ => {
            out.push(b'\\');
            out.push(byte);
        }
    }
}

/// Escapes a string body (`EscapeString`).
#[must_use]
pub fn escape_string(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for byte in text {
        if is_string_safe(*byte) {
            out.push(*byte);
        } else {
            append_escaped(*byte, &mut out);
        }
    }
    out
}

/// Escapes an unquoted URL (`EscapeUrl`).
#[must_use]
pub fn escape_url(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for byte in text {
        if is_url_safe(*byte) {
            out.push(*byte);
        } else {
            append_escaped(*byte, &mut out);
        }
    }
    out
}

/// Escapes identifier text (`EscapeIdentifier`).
#[must_use]
pub fn escape_identifier(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for byte in text {
        if is_ident_safe(*byte) {
            out.push(*byte);
        } else {
            append_escaped(*byte, &mut out);
        }
    }
    out
}

/// Escapes a name that must lex as an identifier
/// (`EscapeIdentifierName`): a lone `-` becomes `\-`, and a leading
/// digit (after an optional `-`) goes through the escaper (which
/// drops hex digits, see [`append_escaped`]).
#[must_use]
pub fn escape_identifier_name(text: &[u8]) -> Vec<u8> {
    if text == b"-" {
        return b"\\-".to_vec();
    }
    let digit_pos = usize::from(!text.is_empty() && text[0] == b'-');
    if digit_pos >= text.len() || !text[digit_pos].is_ascii_digit() {
        return escape_identifier(text);
    }
    let mut out = escape_identifier(&text[..digit_pos]);
    append_escaped(text[digit_pos], &mut out);
    out.extend_from_slice(&escape_identifier(&text[digit_pos + 1..]));
    out
}

/// Escapes identifier text from [`UnicodeText`].
#[must_use]
pub fn escape_unicode_identifier(text: &UnicodeText) -> Vec<u8> {
    escape_identifier(text.as_bytes())
}

/// Escapes URL text from [`UnicodeText`].
#[must_use]
pub fn escape_unicode_url(text: &UnicodeText) -> Vec<u8> {
    escape_url(text.as_bytes())
}

/// Escapes string text from [`UnicodeText`].
#[must_use]
pub fn escape_unicode_string(text: &UnicodeText) -> Vec<u8> {
    escape_string(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_vectors() {
        assert_eq!(escape_identifier(b"foo-bar_2"), b"foo-bar_2");
        assert_eq!(escape_identifier(b"a b"), b"a\\ b");
        assert_eq!(escape_identifier(b"caf\xc3\xa9"), "café".as_bytes());
        assert_eq!(escape_url(b"a\nb"), b"a\\A b");
        assert_eq!(escape_url(b"a\tb"), b"a\\9 b");
        assert_eq!(escape_string(b"a\"b"), b"a\\\"b");
        assert_eq!(escape_url(b"a(b)"), b"a\\(b\\)");
        assert_eq!(escape_identifier_name(b"-"), b"\\-");
        assert_eq!(escape_identifier_name(b"5foo"), b"\\35 foo");
        assert_eq!(escape_identifier_name(b"-5foo"), b"-\\35 foo");
        assert_eq!(escape_identifier_name(b"2xl"), b"\\32 xl");
        assert_eq!(escape_identifier_name(b"foo"), b"foo");
    }
}
