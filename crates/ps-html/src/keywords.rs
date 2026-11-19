//! HTML keyword relations, tag sets, and entity coding.
//!
//! Ports `HtmlKeywords`: the escape/unescape maps and helpers, the
//! element-set predicates (`IsLiteralTag`, `TagAllowsBriefTermination`,
//! ...), and the auto-close/contained relations. Tables live in
//! [`keywords_generated`](crate::keywords_generated).

#[allow(clippy::all)]
#[path = "keywords_generated.rs"]
mod generated;

use generated::{
    AUTO_CLOSE, CONTAINED, DECLARATION_TAGS, IMPLICITLY_CLOSED_TAGS, LITERAL_TAGS,
    MULTI_BYTE_ENTITIES, NON_BRIEF_TERMINATED_TAGS, OPTIONALLY_CLOSED_TAGS, SINGLE_BYTE_ENTITIES,
    SOMETIMES_LITERAL_TAGS,
};

use crate::name::Keyword;

fn in_sorted_set(set: &[Keyword], keyword: Keyword) -> bool {
    set.binary_search(&keyword).is_ok()
}

/// True for tags whose content lexes literally (`IsLiteralTag`).
#[must_use]
pub fn is_literal_tag(keyword: Keyword) -> bool {
    in_sorted_set(&LITERAL_TAGS, keyword)
}

/// True for tags that are literal only with matching close tags.
#[must_use]
pub fn is_sometimes_literal_tag(keyword: Keyword) -> bool {
    in_sorted_set(&SOMETIMES_LITERAL_TAGS, keyword)
}

/// True for implicitly closed tags (`IsImplicitlyClosedTag`).
#[must_use]
pub fn is_implicitly_closed_tag(keyword: Keyword) -> bool {
    in_sorted_set(&IMPLICITLY_CLOSED_TAGS, keyword)
}

/// True unless the tag forbids brief termination or is implicitly closed.
#[must_use]
pub fn tag_allows_brief_termination(keyword: Keyword) -> bool {
    !in_sorted_set(&NON_BRIEF_TERMINATED_TAGS, keyword) && !is_implicitly_closed_tag(keyword)
}

/// True for optionally-closed tags (`IsOptionallyClosedTag`).
#[must_use]
pub fn is_optionally_closed_tag(keyword: Keyword) -> bool {
    in_sorted_set(&OPTIONALLY_CLOSED_TAGS, keyword)
}

/// True for the dead DTD-declaration tags (`IsDeclarationElement`).
#[must_use]
pub fn is_declaration_tag(keyword: Keyword) -> bool {
    in_sorted_set(&DECLARATION_TAGS, keyword)
}

/// True when an open `k1` element auto-closes on seeing `k2` (`IsAutoClose`).
#[must_use]
pub fn is_auto_close(k1: Keyword, k2: Keyword) -> bool {
    AUTO_CLOSE.binary_search(&(k1, k2)).is_ok()
}

/// True when `k1` elements may only appear inside `k2` (`IsContained`).
#[must_use]
pub fn is_contained(k1: Keyword, k2: Keyword) -> bool {
    CONTAINED.binary_search(&(k1, k2)).is_ok()
}

/// HTML whitespace for escaping and shell-like splitting.
#[must_use]
pub fn is_html_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

/// Named entity for a single output byte, if the escape map has one.
///
/// The C++ inserts table-ordered, so the LAST single-byte sequence for a
/// byte wins; linear scan from the back reproduces that exactly.
fn escape_name_for(byte: u8) -> Option<&'static str> {
    SINGLE_BYTE_ENTITIES
        .iter()
        .rev()
        .find(|(_, value)| value.len() == 1 && value[0] == byte)
        .map(|(name, _)| *name)
}

/// Escapes a value for HTML output (`HtmlKeywords::Escape`).
///
/// Every non-space byte that is over 127, under 32, or one of `"'&<>`
/// becomes a named entity when the map has one, else `&#%02d;`.
pub fn escape(input: &[u8], output: &mut Vec<u8>) {
    output.reserve(input.len());
    for &byte in input {
        let needs_escape = !is_html_space(byte)
            && (byte > 127 || byte < 32 || matches!(byte, b'"' | b'\'' | b'&' | b'<' | b'>'));
        if !needs_escape {
            output.push(byte);
            continue;
        }
        match escape_name_for(byte) {
            Some(name) => {
                output.push(b'&');
                output.extend_from_slice(name.as_bytes());
                output.push(b';');
            }
            None => {
                let text = format!("&#{byte:02};");
                output.extend_from_slice(text.as_bytes());
            }
        }
    }
}

/// Numeric accumulator clamp (`AccumulateHexValue`/`AccumulateDecimalValue`).
const NUMERIC_CLAMP: u32 = 0x110000;

/// Largest numeric value representable in 8-bit output.
const MAX_SINGLE_BYTE_VALUE: u32 = 0xFF;

/// Decodes HTML entities (`HtmlKeywords::Unescape`).
///
/// Returns `None` (total failure, like the NULL `StringPiece`) when any byte
/// exceeds 127, when a numeric value exceeds 0xFF, or when a known
/// multi-byte sequence appears. Unknown sequences pass through literally
/// (with their semicolon, when present) without failing.
pub fn unescape(input: &[u8]) -> Option<Vec<u8>> {
    // Fast path: no ampersand means no work and no error... except bytes
    // over 127 still fail the whole call. The C++ scans for '&' lazily but
    // checks every byte it visits; replicate exactly.
    let mut output = Vec::new();
    let mut escape: Vec<u8> = Vec::new();
    let mut numeric_value: u32 = 0;
    let mut accumulate_numeric = false;
    let mut hex_mode = false;
    let mut in_escape = false;
    let mut found_ampersand = false;
    let mut pos = 0;
    while pos < input.len() {
        let byte = input[pos];
        if !in_escape {
            if byte == b'&' {
                if !found_ampersand {
                    found_ampersand = true;
                    output.extend_from_slice(&input[..pos]);
                }
                in_escape = true;
                escape.clear();
                numeric_value = 0;
                accumulate_numeric = false;
                hex_mode = false;
            } else if byte > 127 {
                return None;
            } else if found_ampersand {
                output.push(byte);
            }
        } else if escape.is_empty() && byte == b'#' {
            escape.push(byte);
            accumulate_numeric = true;
            if pos + 1 < input.len() && (input[pos + 1] == b'x' || input[pos + 1] == b'X') {
                hex_mode = true;
                pos += 1;
            }
        } else if byte == b';' {
            if !try_unescape(
                accumulate_numeric,
                numeric_value,
                &escape,
                true,
                &mut output,
            ) {
                return None;
            }
            in_escape = false;
        } else if byte > 127 {
            return None;
        } else {
            let improperly_terminated = if accumulate_numeric {
                let digit = if hex_mode {
                    hex_digit_value(byte)
                } else if byte.is_ascii_digit() {
                    Some(u32::from(byte - b'0'))
                } else {
                    None
                };
                match digit {
                    Some(digit) => {
                        let base = if hex_mode { 16 } else { 10 };
                        numeric_value = numeric_value.saturating_mul(base).saturating_add(digit);
                        if numeric_value > NUMERIC_CLAMP {
                            numeric_value = NUMERIC_CLAMP;
                        }
                        false
                    }
                    None => true,
                }
            } else {
                !byte.is_ascii_alphanumeric()
            };
            if improperly_terminated {
                if try_unescape(
                    accumulate_numeric,
                    numeric_value,
                    &escape,
                    false,
                    &mut output,
                ) {
                    // Re-examine this byte now that the escape resolved.
                    pos -= 1;
                    in_escape = false;
                } else {
                    return None;
                }
            } else {
                escape.push(byte);
            }
        }
        pos += 1;
    }
    if !found_ampersand {
        return Some(input.to_vec());
    }
    if in_escape {
        if escape.is_empty() {
            output.push(b'&');
        } else if !try_unescape(
            accumulate_numeric,
            numeric_value,
            &escape,
            false,
            &mut output,
        ) {
            return None;
        }
    }
    Some(output)
}

fn hex_digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some(u32::from(byte - b'0')),
        b'a'..=b'f' => Some(10 + u32::from(byte - b'a')),
        b'A'..=b'F' => Some(10 + u32::from(byte - b'A')),
        _ => None,
    }
}

/// Resolves one escape (`TryUnescape`); false fails the whole unescape.
fn try_unescape(
    accumulate_numeric: bool,
    numeric_value: u32,
    escape: &[u8],
    was_terminated: bool,
    output: &mut Vec<u8>,
) -> bool {
    if accumulate_numeric && escape.len() > 1 {
        if numeric_value <= MAX_SINGLE_BYTE_VALUE {
            output.push(numeric_value as u8);
        } else {
            return false;
        }
    } else if let Some(value) = lookup_sensitive(escape) {
        output.extend_from_slice(value);
    } else if is_known_multibyte(escape) {
        // Known multi-byte sequences cannot be represented: fail rather
        // than emit a mojibake `&amp;`-prefixed literal.
        return false;
    } else if let Some(value) = lookup_insensitive(escape) {
        output.extend_from_slice(value);
    } else if escape.eq_ignore_ascii_case(b"apos") {
        // &apos; is not legal HTML but universally attempted; map to "'".
        // (Deliberately absent from the escape map so "'" encodes as &#39;.)
        output.push(b'\'');
    } else {
        // Unknown words pass through literally, semicolon included.
        output.push(b'&');
        output.extend_from_slice(escape);
        if was_terminated {
            output.push(b';');
        }
    }
    true
}

/// Exact-case single-byte lookup.
fn lookup_sensitive(escape: &[u8]) -> Option<&'static [u8]> {
    SINGLE_BYTE_ENTITIES
        .iter()
        .find(|(name, _)| name.as_bytes() == escape)
        .map(|(_, value)| *value)
}

/// Case-insensitive single-byte lookup, excluding case-sensitive pairs.
///
/// Names whose lowercase form collides with another entry (AElig/aelig)
/// resolve only through the sensitive map, so `&Aelig;` errors instead of
/// resolving unpredictably.
fn lookup_insensitive(escape: &[u8]) -> Option<&'static [u8]> {
    let mut found = None;
    for (name, value) in SINGLE_BYTE_ENTITIES {
        if name.as_bytes().eq_ignore_ascii_case(escape) {
            if found.is_some() {
                return None;
            }
            found = Some(value);
        }
    }
    found
}

/// Case-insensitive multi-byte membership (binary search, like the C++).
fn is_known_multibyte(escape: &[u8]) -> bool {
    MULTI_BYTE_ENTITIES
        .binary_search_by(|entry| compare_case_insensitive(entry.0.as_bytes(), escape))
        .is_ok()
}

fn compare_case_insensitive(left: &[u8], right: &[u8]) -> std::cmp::Ordering {
    let common = left.len().min(right.len());
    for i in 0..common {
        let a = left[i].to_ascii_uppercase();
        let b = right[i].to_ascii_uppercase();
        if a != b {
            return a.cmp(&b);
        }
    }
    left.len().cmp(&right.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_basics() {
        let mut out = Vec::new();
        escape(b"<a href=\"x&y\">", &mut out);
        assert_eq!(out, b"&lt;a href=&quot;x&amp;y&quot;&gt;");
        // Single quotes and control bytes encode numerically when unnamed.
        let mut out = Vec::new();
        escape(b"it's\x01ok", &mut out);
        assert_eq!(out, b"it&#39;s&#01;ok");
        // Spaces pass through; DEL (127) is not escaped.
        let mut out = Vec::new();
        escape(b"a b\x7f", &mut out);
        assert_eq!(out, b"a b\x7f");
    }

    #[test]
    fn unescape_basics() {
        assert_eq!(unescape(b"a&amp;b").as_deref(), Some(b"a&b".as_slice()));
        assert_eq!(
            unescape(b"&lt;&gt;&quot;").as_deref(),
            Some(b"<>\"".as_slice())
        );
        // Case-insensitive single-byte lookup for unique names.
        assert_eq!(unescape(b"&AMP;").as_deref(), Some(b"&".as_slice()));
        assert_eq!(unescape(b"&QUOT;").as_deref(), Some(b"\"".as_slice()));
        // Case-sensitive pairs resolve only in exact case.
        assert_eq!(unescape(b"&AElig;").as_deref(), Some(b"\xc6".as_slice()));
        assert_eq!(unescape(b"&aelig;").as_deref(), Some(b"\xe6".as_slice()));
        // Wrong case falls through to literal passthrough.
        assert_eq!(unescape(b"&Aelig;").as_deref(), Some(b"&Aelig;".as_slice()));
        // Bare ampersand passes through.
        assert_eq!(
            unescape(b"fish &amp; chips &").as_deref(),
            Some(b"fish & chips &".as_slice())
        );
        // Slovenska: &apos; maps to a quote.
        assert_eq!(unescape(b"&apos;").as_deref(), Some(b"'".as_slice()));
    }

    #[test]
    fn unescape_numeric() {
        assert_eq!(unescape(b"&#65;").as_deref(), Some(b"A".as_slice()));
        assert_eq!(unescape(b"&#x41;").as_deref(), Some(b"A".as_slice()));
        assert_eq!(unescape(b"&#X4a;").as_deref(), Some(b"J".as_slice()));
        assert_eq!(unescape(b"&#0;").as_deref(), Some(b"\x00".as_slice()));
        // Over 0xFF fails the entire unescape.
        assert_eq!(unescape(b"&#256;"), None);
        assert_eq!(unescape(b"&#x110000;"), None);
        assert_eq!(unescape(b"a&#257;b"), None);
        // Unterminated numerics resolve at the boundary.
        assert_eq!(unescape(b"&#65x").as_deref(), Some(b"Ax".as_slice()));
    }

    #[test]
    fn unescape_failure_modes() {
        // Known multi-byte sequences fail the whole call.
        assert_eq!(unescape(b"&alpha;"), None);
        assert_eq!(unescape(b"x&hellip;y"), None);
        // Any byte over 127 fails the whole call.
        assert_eq!(unescape(b"caf\xe9"), None);
        assert_eq!(unescape("caf\u{e9}".as_bytes()), None);
        // Unknown sequences pass through literally, semicolon included.
        assert_eq!(
            unescape(b"&nosuchentity;").as_deref(),
            Some(b"&nosuchentity;".as_slice())
        );
        assert_eq!(unescape(b"a&b").as_deref(), Some(b"a&b".as_slice()));
    }

    #[test]
    fn tag_sets_match_cpp() {
        use crate::name::Keyword::*;
        assert!(is_literal_tag(Script));
        assert!(is_literal_tag(Style));
        assert!(is_literal_tag(Textarea));
        assert!(!is_literal_tag(Div));
        assert!(is_sometimes_literal_tag(Noscript));
        assert!(!is_sometimes_literal_tag(Textarea));
        assert!(is_implicitly_closed_tag(Br));
        assert!(is_implicitly_closed_tag(Img));
        assert!(!is_implicitly_closed_tag(Div));
        assert!(!tag_allows_brief_termination(Br));
        assert!(!tag_allows_brief_termination(Div));
        assert!(!tag_allows_brief_termination(Script));
        assert!(tag_allows_brief_termination(B));
        assert!(is_optionally_closed_tag(P));
        assert!(is_optionally_closed_tag(Li));
        assert!(!is_optionally_closed_tag(Div));
        assert!(is_auto_close(P, Div));
        assert!(is_auto_close(Li, Li));
        assert!(!is_auto_close(Div, Div));
        assert!(is_contained(Li, Ul));
        assert!(!is_contained(Div, Ul));
        // The o:p release quirk: unknown tags auto-close each other.
        assert!(is_auto_close(NotAKeyword, NotAKeyword));
    }
}
