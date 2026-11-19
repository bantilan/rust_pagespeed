//! Interchange-valid UTF-8 text (`unicodetext.h`, `unilib.h`,
//! `third_party/utf/rune.c`).
//!
//! A [`UnicodeText`] owns interchange-valid UTF-8 bytes: construction
//! coerces every structurally invalid byte and every non-interchange
//! codepoint to a space ([`UniLib::IsInterchangeValid`]). The CSS code
//! never uses the unsafe/aliasing constructors observably differently
//! from copies, so a single owned type is behavior-identical.
#![forbid(unsafe_code)]

/// Decoding error marker (`Runeerror`, U+FFFD).
pub const RUNE_ERROR: u32 = 0xFFFD;
/// Maximum rune value (`Runemax`).
pub const RUNE_MAX: u32 = 0x10FFFF;

/// Decodes one rune, Plan 9 `charntorune` semantics: `(value, bytes)`.
/// Incomplete input yields `(RUNE_ERROR, 0)`; complete-but-invalid
/// yields `(RUNE_ERROR, 1)`. Surrogates decode fine here (they only
/// fail [`is_valid_codepoint`]); overlongs, out-of-range values, and
/// 5-byte leads fail.
#[must_use]
pub fn decode_rune(input: &[u8]) -> (u32, usize) {
    if input.is_empty() {
        return (RUNE_ERROR, 0);
    }
    let first = input[0];
    if first < 0x80 {
        return (u32::from(first), 1);
    }
    if input.len() < 2 {
        return (RUNE_ERROR, 0);
    }
    // Second byte must trail (checked before lead classification).
    if input[1] & 0xC0 != 0x80 {
        return (RUNE_ERROR, 1);
    }
    let cont = |byte: u8| u32::from(byte & 0x3F);
    if first < 0xE0 {
        if first < 0xC0 {
            return (RUNE_ERROR, 1);
        }
        let value = (u32::from(first) << 6 | cont(input[1])) & 0x7FF;
        if value <= 0x7F {
            return (RUNE_ERROR, 1);
        }
        return (value, 2);
    }
    if input.len() < 3 {
        return (RUNE_ERROR, 0);
    }
    if input[2] & 0xC0 != 0x80 {
        return (RUNE_ERROR, 1);
    }
    if first < 0xF0 {
        let value = (u32::from(first) << 12 | cont(input[1]) << 6 | cont(input[2])) & 0xFFFF;
        if value <= 0x7FF {
            return (RUNE_ERROR, 1);
        }
        return (value, 3);
    }
    if input.len() < 4 {
        return (RUNE_ERROR, 0);
    }
    if input[3] & 0xC0 != 0x80 {
        return (RUNE_ERROR, 1);
    }
    if first < 0xF8 {
        let value =
            (u32::from(first) << 18 | cont(input[1]) << 12 | cont(input[2]) << 6 | cont(input[3]))
                & 0x1F_FFFF;
        if value <= 0xFFFF || value > RUNE_MAX {
            return (RUNE_ERROR, 1);
        }
        return (value, 4);
    }
    (RUNE_ERROR, 1)
}

/// Whether a decoded rune is structurally valid
/// (`isvalidcharntorune`): anything but an error marker, except a real
/// U+FFFD (3 bytes) still counts as valid.
#[must_use]
pub fn is_valid_decode(value: u32, len: usize) -> bool {
    value != RUNE_ERROR || len == 3
}

/// Whether a codepoint is a valid value (`IsValidCodepoint`):
/// below the surrogates or above them through U+10FFFF.
#[must_use]
pub fn is_valid_codepoint(code: u32) -> bool {
    code < 0xD800 || (0xE000..=0x10FFFF).contains(&code)
}

/// Whether a codepoint is interchange-valid (`IsInterchangeValid`).
#[must_use]
pub fn is_interchange_valid(code: u32) -> bool {
    !((0x00..=0x08).contains(&code)
        || code == 0x0B
        || (0x0E..=0x1F).contains(&code)
        || (0x7F..=0x9F).contains(&code)
        || (0xD800..=0xDFFF).contains(&code)
        || (0xFDD0..=0xFDEF).contains(&code)
        || code & 0xFFFE == 0xFFFE)
}

/// Length of the leading interchange-valid span
/// (`SpanInterchangeValid`).
#[must_use]
pub fn span_interchange_valid(bytes: &[u8]) -> usize {
    let mut pos = 0;
    while pos < bytes.len() {
        let (value, len) = decode_rune(&bytes[pos..]);
        // A real U+FFFD decodes with len 3; errors report len <= 1.
        if (value == RUNE_ERROR && len <= 1) || !is_interchange_valid(value) {
            break;
        }
        pos += len;
    }
    pos
}

/// Coerces bytes to interchange-valid UTF-8
/// (`ConvertToInterchangeValid`): each structurally valid (but
/// non-interchange) character collapses to one space, each invalid
/// byte to one space.
#[must_use]
pub fn coerce_to_interchange_valid(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    while pos < bytes.len() {
        let good = span_interchange_valid(&bytes[pos..]);
        if good > 0 {
            out.extend_from_slice(&bytes[pos..pos + good]);
            pos += good;
            if pos == bytes.len() {
                break;
            }
        }
        let (value, len) = decode_rune(&bytes[pos..]);
        if is_valid_decode(value, len) {
            pos += len;
        } else {
            pos += 1;
        }
        out.push(b' ');
    }
    out
}

/// Owned interchange-valid UTF-8 text (`UnicodeText`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnicodeText(Vec<u8>);

impl UnicodeText {
    /// Empty text.
    #[must_use]
    pub fn new() -> UnicodeText {
        UnicodeText(Vec::new())
    }

    /// Const empty text (for static fallbacks).
    #[must_use]
    pub const fn new_const_empty() -> UnicodeText {
        UnicodeText(Vec::new())
    }

    /// Copies bytes, coercing to interchange-valid (`CopyUTF8`).
    #[must_use]
    pub fn from_utf8(bytes: &[u8]) -> UnicodeText {
        UnicodeText(coerce_to_interchange_valid(bytes))
    }

    /// Appends a codepoint (`push_back`): invalid or non-interchange
    /// values append a space instead.
    pub fn push_codepoint(&mut self, code: u32) {
        if !is_valid_codepoint(code) {
            self.0.push(b' ');
            return;
        }
        let mut buf = [0u8; 4];
        // Validated above, so this cannot fail.
        let text = char::from_u32(code).map_or(" ", |c| {
            c.encode_utf8(&mut buf);
            // Re-borrow the filled prefix as str.
            std::str::from_utf8(&buf[..c.len_utf8()]).unwrap_or(" ")
        });
        if is_interchange_valid(code) {
            self.0.extend_from_slice(text.as_bytes());
        } else {
            self.0.push(b' ');
        }
    }

    /// Appends another text (`append`).
    pub fn append(&mut self, other: &UnicodeText) {
        self.0.extend_from_slice(&other.0);
    }

    /// Clears the text.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Whether the text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Raw UTF-8 bytes (`utf8_data` + `utf8_length`).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Text as `str` (always valid UTF-8 by construction).
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Coercion only emits valid UTF-8 or spaces.
        std::str::from_utf8(&self.0).unwrap_or("")
    }

    /// Byte length (`utf8_length`).
    #[must_use]
    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_vectors() {
        assert_eq!(decode_rune(b""), (RUNE_ERROR, 0));
        assert_eq!(decode_rune(b"a"), (0x61, 1));
        assert_eq!(decode_rune(b"\xc3\xa9"), (0xE9, 2));
        assert_eq!(decode_rune(b"\xe2\x82\xac"), (0x20AC, 3));
        assert_eq!(decode_rune(b"\xf0\x9f\x98\x80"), (0x1F600, 4));
        // Truncated.
        assert_eq!(decode_rune(b"\xc3"), (RUNE_ERROR, 0));
        assert_eq!(decode_rune(b"\xe2\x82"), (RUNE_ERROR, 0));
        assert_eq!(decode_rune(b"\xf0\x9f\x98"), (RUNE_ERROR, 0));
        // Bad trails consume one byte.
        assert_eq!(decode_rune(b"\xc3\x28"), (RUNE_ERROR, 1));
        assert_eq!(decode_rune(b"\xe2\x82\x28"), (RUNE_ERROR, 1));
        // Overlongs rejected.
        assert_eq!(decode_rune(b"\xc0\xaf"), (RUNE_ERROR, 1));
        assert_eq!(decode_rune(b"\xe0\x80\xaf"), (RUNE_ERROR, 1));
        assert_eq!(decode_rune(b"\xf0\x80\x80\xaf"), (RUNE_ERROR, 1));
        // Lone trail lead.
        assert_eq!(decode_rune(b"\x80"), (RUNE_ERROR, 0));
        assert_eq!(decode_rune(b"\x80a"), (RUNE_ERROR, 1));
        // Surrogates decode (only push_back rejects them).
        assert_eq!(decode_rune(b"\xed\xa0\x80"), (0xD800, 3));
        // Out of range.
        assert_eq!(decode_rune(b"\xf4\x90\x80\x80"), (RUNE_ERROR, 1));
        // 5-byte lead.
        assert_eq!(decode_rune(b"\xf8\x88\x80\x80\x80"), (RUNE_ERROR, 1));
        // Real U+FFFD.
        assert_eq!(decode_rune(b"\xef\xbf\xbd"), (0xFFFD, 3));
        assert!(is_valid_decode(0xFFFD, 3));
        assert!(!is_valid_decode(RUNE_ERROR, 1));
    }

    #[test]
    fn interchange_predicate() {
        assert!(is_interchange_valid(0x09));
        assert!(is_interchange_valid(0x0A));
        assert!(is_interchange_valid(0x0C));
        assert!(is_interchange_valid(0x0D));
        assert!(!is_interchange_valid(0x00));
        assert!(!is_interchange_valid(0x0B));
        assert!(!is_interchange_valid(0x1F));
        assert!(!is_interchange_valid(0x7F));
        assert!(!is_interchange_valid(0x9F));
        assert!(!is_interchange_valid(0xD800));
        assert!(!is_interchange_valid(0xFDD0));
        assert!(!is_interchange_valid(0xFFFE));
        assert!(!is_interchange_valid(0x1FFFE));
        assert!(is_interchange_valid(0xFFFD));
        assert!(is_interchange_valid(0xE9));
    }

    #[test]
    fn coercion_vectors() {
        // Clean input passes through.
        assert_eq!(coerce_to_interchange_valid(b"abc"), b"abc");
        assert_eq!(
            coerce_to_interchange_valid("héllo".as_bytes()),
            "héllo".as_bytes()
        );
        // Invalid bytes become one space each.
        assert_eq!(coerce_to_interchange_valid(b"a\xe9b"), b"a b");
        // A non-interchange (but valid) char becomes one space.
        assert_eq!(coerce_to_interchange_valid("a\x7fb".as_bytes()), b"a b");
        assert_eq!(coerce_to_interchange_valid(b"a\xef\xb7\x90b"), b"a b");
        // Real U+FFFD survives.
        assert_eq!(
            coerce_to_interchange_valid(b"a\xef\xbf\xbdb"),
            b"a\xef\xbf\xbdb"
        );
        // NUL becomes a space.
        assert_eq!(coerce_to_interchange_valid(b"a\x00b"), b"a b");
    }

    #[test]
    fn push_back_rules() {
        let mut text = UnicodeText::new();
        text.push_codepoint(0x41);
        text.push_codepoint(0xE9);
        text.push_codepoint(0xD800); // surrogate -> space
        text.push_codepoint(0x110000); // out of range -> space
        text.push_codepoint(0x07); // control -> space
        text.push_codepoint(0xFFFE); // noncharacter -> space
        assert_eq!(text.as_bytes(), "Aé    ".as_bytes());
    }
}
