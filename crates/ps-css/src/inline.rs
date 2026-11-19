//! Inline/outline/move decisions (`css_inline_filter.cc`,
//! `css_outline_filter.cc`, `css_move_to_head_filter.cc` pure parts).
//!
//! Eligibility gates with exact failure reasons. DOM flow lives in
//! later steps.
#![forbid(unsafe_code)]

use crate::parser::Parser;

/// UTF-8 BOM bytes (`kUtf8Bom`).
pub const UTF8_BOM: &[u8; 3] = b"\xef\xbb\xbf";

/// Detects a BOM charset (`GetCharsetForBom`).
#[must_use]
pub fn charset_for_bom(contents: &[u8]) -> &[u8] {
    if contents.is_empty() {
        return b"";
    }
    if contents[0] >= b' ' && contents[0] <= b'~' {
        return b"";
    }
    if contents.starts_with(b"\xef\xbb\xbf") {
        return b"utf-8";
    }
    if contents.starts_with(b"\xfe\xff") {
        return b"utf-16be";
    }
    // UTF-16LE's BOM prefixes UTF-32LE's, so check long first.
    if contents.starts_with(b"\xff\xfe\x00\x00") {
        return b"utf-32le";
    }
    if contents.starts_with(b"\xff\xfe") {
        return b"utf-16le";
    }
    if contents.starts_with(b"\x00\x00\xfe\xff") {
        return b"utf-32be";
    }
    b""
}

/// Whether the contents look gzip-encoded
/// (`GzipInflater::HasGzipMagicBytes`).
#[must_use]
pub fn has_gzip_magic_bytes(contents: &[u8]) -> bool {
    contents.len() >= 10 && contents[0] == 0x1F && contents[1] == 0x8B
}

/// Whether the contents could read as markup (`MayContainMarkup`).
#[must_use]
pub fn may_contain_markup(contents: &[u8]) -> bool {
    contents.contains(&b'<')
}

/// Determines a stylesheet's charset (`GetCharsetForStylesheet`):
/// headers, then leading `@charset`, then BOM, then the element's
/// charset attribute.
#[must_use]
pub fn charset_for_stylesheet(
    headers_charset: &[u8],
    contents: &[u8],
    attribute_charset: &[u8],
) -> Vec<u8> {
    if !headers_charset.is_empty() {
        return headers_charset.to_vec();
    }
    let mut parser = Parser::new(contents);
    let at_charset = parser.extract_charset();
    if parser.errors_seen_mask() == 0 && !at_charset.is_empty() {
        return at_charset.as_bytes().to_vec();
    }
    let bom_charset = charset_for_bom(contents);
    if !bom_charset.is_empty() {
        return bom_charset.to_vec();
    }
    attribute_charset.to_vec()
}

/// Inline-eligibility failure reasons (`ShouldInline` messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineVeto {
    /// Over `css_inline_max_bytes`.
    TooBig(i64),
    /// Looks gzip-encoded.
    Gzipped,
    /// Contains `<`.
    Markup,
    /// Charset mismatch with non-ASCII content.
    Charset {
        /// HTML charset.
        html: Vec<u8>,
        /// CSS charset.
        css: Vec<u8>,
    },
}

impl InlineVeto {
    /// Renders the C++ debug message.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            InlineVeto::TooBig(limit) => {
                format!("CSS not inlined since it's bigger than {limit} bytes").into_bytes()
            }
            InlineVeto::Gzipped => {
                b"CSS not inlined because it appears to be gzip-encoded".to_vec()
            }
            InlineVeto::Markup => b"CSS not inlined since it contains a less-than sign".to_vec(),
            InlineVeto::Charset { html, css } => {
                let mut out = b"CSS not inlined due to apparent charset incompatibility; we think the HTML is ".to_vec();
                out.extend_from_slice(html);
                out.extend_from_slice(b" while the CSS is ");
                out.extend_from_slice(css);
                out
            }
        }
    }
}

/// Decides inline eligibility (`ShouldInline`).
pub fn should_inline(
    contents: &[u8],
    headers_charset: &[u8],
    attribute_charset: &[u8],
    html_charset: &[u8],
    max_bytes: i64,
) -> Result<(), InlineVeto> {
    if contents.len() as i64 > max_bytes {
        return Err(InlineVeto::TooBig(max_bytes));
    }
    if has_gzip_magic_bytes(contents) {
        return Err(InlineVeto::Gzipped);
    }
    if may_contain_markup(contents) {
        return Err(InlineVeto::Markup);
    }
    let css_charset = charset_for_stylesheet(headers_charset, contents, attribute_charset);
    if !html_charset.eq_ignore_ascii_case(&css_charset) {
        let clean = contents.strip_prefix(UTF8_BOM).unwrap_or(contents);
        if clean.iter().any(|byte| *byte >= 0x80) {
            return Err(InlineVeto::Charset {
                html: html_charset.to_vec(),
                css: css_charset,
            });
        }
    }
    Ok(())
}

/// Whether a `<style>` block outlines (`EndElementImpl` size gate).
#[must_use]
pub fn should_outline(contents_len: i64, min_bytes: i64) -> bool {
    contents_len >= min_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_vectors() {
        assert_eq!(charset_for_bom(b""), b"");
        assert_eq!(charset_for_bom(b".a{}"), b"");
        assert_eq!(charset_for_bom(b"\xef\xbb\xbf.a{}"), b"utf-8");
        assert_eq!(charset_for_bom(b"\xfe\xff"), b"utf-16be");
        assert_eq!(charset_for_bom(b"\xff\xfe\x00\x00"), b"utf-32le");
        assert_eq!(charset_for_bom(b"\xff\xfea"), b"utf-16le");
        assert_eq!(charset_for_bom(b"\x00\x00\xfe\xff"), b"utf-32be");
        assert_eq!(charset_for_bom(b"\x00a"), b"");
        assert!(has_gzip_magic_bytes(&[0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 3]));
        assert!(!has_gzip_magic_bytes(b"\x1f\x8bshort"));
        assert!(may_contain_markup(b"a</style>"));
        assert!(!may_contain_markup(b"a{color:red}"));
    }

    #[test]
    fn inline_vectors() {
        // Too big.
        assert_eq!(
            should_inline(b"0123456789", b"", b"", b"utf-8", 5),
            Err(InlineVeto::TooBig(5))
        );
        assert_eq!(
            InlineVeto::TooBig(5).message(),
            b"CSS not inlined since it's bigger than 5 bytes"
        );
        // Gzipped.
        assert_eq!(
            should_inline(
                &[0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 3],
                b"",
                b"",
                b"utf-8",
                100
            ),
            Err(InlineVeto::Gzipped)
        );
        // Markup.
        assert_eq!(
            should_inline(b"a</style>", b"", b"", b"utf-8", 100),
            Err(InlineVeto::Markup)
        );
        // Charset mismatch with non-ASCII content.
        assert_eq!(
            should_inline(b"a{content:\"\xe9\"}", b"utf-8", b"", b"windows-1252", 100),
            Err(InlineVeto::Charset {
                html: b"windows-1252".to_vec(),
                css: b"utf-8".to_vec(),
            })
        );
        // Same mismatch but pure ASCII: allowed.
        assert_eq!(
            should_inline(b"a{color:red}", b"utf-8", b"", b"windows-1252", 100),
            Ok(())
        );
        // Headers charset wins over @charset.
        assert_eq!(
            should_inline(
                b"@charset \"shift_jis\";.a{color:red}",
                b"utf-8",
                b"",
                b"utf-8",
                100
            ),
            Ok(())
        );
        assert_eq!(
            charset_for_stylesheet(b"", b"@charset \"shift_jis\";.a{}", b""),
            b"shift_jis"
        );
    }

    #[test]
    fn outline_vectors() {
        assert!(should_outline(100, 100));
        assert!(!should_outline(99, 100));
    }
}
