//! CSS utilities (`css_util.{h,cc}` pure parts).
//!
//! Media conversions, dimension extraction. HTML-bound helpers
//! (`StyleExtractor`, element scanning) belong to later steps.
#![forbid(unsafe_code)]

use crate::ast::Declarations;
use crate::media::{MediaQueries, MediaQuery};
use crate::property::Prop;
use crate::unicode_text::UnicodeText;
use crate::value::{Unit, ValueType, Values};

/// No-value marker (`kNoValue`).
pub const NO_VALUE: i32 = -1;

/// HTML/CSS whitespace (`IsHtmlSpace`): space, tab, CR, LF, FF.
#[must_use]
pub fn is_html_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0C)
}

/// Trims HTML whitespace on both ends (`TrimWhitespace`).
#[must_use]
pub fn trim_whitespace(text: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < text.len() && is_html_space(text[start]) {
        start += 1;
    }
    let mut end = text.len();
    while end > start && is_html_space(text[end - 1]) {
        end -= 1;
    }
    &text[start..end]
}

/// Trims leading HTML whitespace, reporting whether any was trimmed
/// (`TrimLeadingWhitespace`).
pub fn trim_leading_whitespace(text: &mut &[u8]) -> bool {
    let start_len = text.len();
    while let Some((&first, _)) = text.split_first() {
        if !is_html_space(first) {
            break;
        }
        *text = &text[1..];
    }
    text.len() != start_len
}

/// Whether a query is too complex to flatten (`IsComplexMediaQuery`):
/// any qualifier or any expression.
#[must_use]
pub fn is_complex_media_query(query: &MediaQuery) -> bool {
    query.qualifier() != crate::media::MediaQualifier::None || !query.expressions().is_empty()
}

/// Converts queries to trimmed media-type strings, rejecting complex
/// queries (`ConvertMediaQueriesToStringVector`).
#[must_use]
pub fn media_queries_to_strings(queries: &MediaQueries) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for query in queries {
        if is_complex_media_query(query) {
            return None;
        }
        let trimmed = trim_whitespace(query.media_type().as_bytes());
        if !trimmed.is_empty() {
            out.push(trimmed.to_vec());
        }
    }
    Some(out)
}

/// Converts media-type strings back to queries
/// (`ConvertStringVectorToMediaQueries`).
#[must_use]
pub fn strings_to_media_queries(strings: &[Vec<u8>]) -> MediaQueries {
    let mut out = MediaQueries::new();
    for string in strings {
        let trimmed = trim_whitespace(string);
        if !trimmed.is_empty() {
            let mut query = MediaQuery::new();
            query.set_media_type(UnicodeText::from_utf8(trimmed));
            out.push(query);
        }
    }
    out
}

/// Clears the vector if it names `all` (`ClearVectorIfContainsMediaAll`).
pub fn clear_vector_if_contains_media_all(media: &mut Vec<Vec<u8>>) {
    if media
        .iter()
        .any(|entry| crate::string_util::string_case_equals(entry, b"all"))
    {
        media.clear();
    }
}

/// Splits a media attribute on commas (`VectorizeMediaAttribute`):
/// each element trimmed, empties dropped, and any `all` (case-insensitive)
/// clears the whole vector.
pub fn vectorize_media_attribute(input: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for piece in input.split(|byte| *byte == b',') {
        let trimmed = trim_whitespace(piece);
        if crate::string_util::string_case_equals(trimmed, b"all") {
            out.clear();
            break;
        } else if !trimmed.is_empty() {
            out.push(trimmed.to_vec());
        }
    }
    out
}

/// Joins a media vector (`StringifyMediaVector`): comma-joined, or `all`
/// for an empty vector (the inverse of vectorization's `all` rule).
#[must_use]
pub fn stringify_media_vector(media: &[Vec<u8>]) -> Vec<u8> {
    if media.is_empty() {
        return b"all".to_vec();
    }
    let mut out = Vec::new();
    for (index, entry) in media.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.extend_from_slice(entry);
    }
    out
}

/// Intersects two sorted vectors in place, keeping the inner's
/// survivors (`EliminateElementsNotIn`). An empty inner takes the
/// outer wholesale; an empty outer keeps the inner as-is.
pub fn eliminate_elements_not_in(inner: &mut Vec<Vec<u8>>, outer: &[Vec<u8>]) {
    if outer.is_empty() {
        return;
    }
    if inner.is_empty() {
        *inner = outer.to_vec();
        return;
    }
    let mut kept = Vec::new();
    let (mut i, mut o) = (0, 0);
    while i < inner.len() {
        if o >= outer.len() {
            break;
        } else if outer[o] == inner[i] {
            kept.push(inner[i].clone());
            o += 1;
            i += 1;
        } else if outer[o] < inner[i] {
            o += 1;
        } else {
            i += 1;
        }
    }
    *inner = kept;
}

/// Extracts the first pixel value (`GetValueDimension`).
#[must_use]
pub fn get_value_dimension(values: &Values) -> i32 {
    for value in values {
        if value.lexical_type() == ValueType::Number && value.dimension() == Unit::Px {
            return value.integer_value();
        }
    }
    NO_VALUE
}

/// Dimension availability (`DimensionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimensionState {
    NoDimensions,
    HasWidthOnly,
    HasHeightOnly,
    HasBothDimensions,
    NotParsable,
}

/// Extracts width/height (`GetDimensions`).
#[must_use]
pub fn get_dimensions(decls: &Declarations) -> (DimensionState, i32, i32) {
    let mut has_width = false;
    let mut has_height = false;
    let mut width = NO_VALUE;
    let mut height = NO_VALUE;
    for decl in decls {
        if has_width && has_height {
            break;
        }
        match decl.prop() {
            Prop::Width => {
                width = get_value_dimension(decl.values());
                has_width = true;
            }
            Prop::Height => {
                height = get_value_dimension(decl.values());
                has_height = true;
            }
            _ => {}
        }
    }
    let state = if has_width && has_height && width != NO_VALUE && height != NO_VALUE {
        DimensionState::HasBothDimensions
    } else if (has_width && width == NO_VALUE) || (has_height && height == NO_VALUE) {
        DimensionState::NotParsable
    } else if has_width {
        DimensionState::HasWidthOnly
    } else if has_height {
        DimensionState::HasHeightOnly
    } else {
        DimensionState::NoDimensions
    };
    (state, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_vectors() {
        assert_eq!(trim_whitespace(b"  a b \t\r\n"), b"a b");
        assert_eq!(trim_whitespace(b"\x0bno"), b"\x0bno");
        let mut text = b"  x" as &[u8];
        assert!(trim_leading_whitespace(&mut text));
        assert_eq!(text, b"x");
        assert!(!trim_leading_whitespace(&mut text));
    }

    #[test]
    fn eliminate_vectors() {
        let mut inner = vec![b"b".to_vec(), b"c".to_vec(), b"e".to_vec()];
        eliminate_elements_not_in(&mut inner, &[b"a".to_vec(), b"c".to_vec()]);
        assert_eq!(inner, vec![b"c".to_vec()]);
        let mut inner = Vec::new();
        eliminate_elements_not_in(&mut inner, &[b"a".to_vec()]);
        assert_eq!(inner, vec![b"a".to_vec()]);
        let mut inner = vec![b"a".to_vec()];
        eliminate_elements_not_in(&mut inner, &[]);
        assert_eq!(inner, vec![b"a".to_vec()]);
        let mut media = vec![b"screen".to_vec(), b"ALL".to_vec()];
        clear_vector_if_contains_media_all(&mut media);
        assert!(media.is_empty());
    }

    #[test]
    fn vectorize_and_stringify_media() {
        assert_eq!(
            vectorize_media_attribute(b"screen,printer"),
            vec![b"screen".to_vec(), b"printer".to_vec()]
        );
        // Whitespace trims, empties drop.
        assert_eq!(
            vectorize_media_attribute(b" screen , printer ,, "),
            vec![b"screen".to_vec(), b"printer".to_vec()]
        );
        // Any `all` clears everything.
        assert!(vectorize_media_attribute(b"screen,ALL").is_empty());
        assert!(vectorize_media_attribute(b"all").is_empty());
        // Stringify inverts, with `all` for empty.
        assert_eq!(
            stringify_media_vector(&[b"screen".to_vec(), b"print".to_vec()]),
            b"screen,print"
        );
        assert_eq!(stringify_media_vector(&[]), b"all");
    }
}
