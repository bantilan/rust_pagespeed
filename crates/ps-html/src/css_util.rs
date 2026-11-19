//! CSS link utilities (`css_util`, media subset).
//!
//! Decides whether a stylesheet's `media` can affect the screen.
//! Selector helpers land with the filters that need them.

/// Strips a leading word (`StartsWithWord`): matches the word plus a
/// boundary (whitespace, `(`, or end).
fn starts_with_word<'a>(word: &str, data: &mut &'a str) -> bool {
    let Some(rest) = data.strip_prefix(word) else {
        return false;
    };
    let trimmed = rest.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('(') || trimmed.len() != rest.len() {
        *data = trimmed;
        return true;
    }
    false
}

/// Whether a media list can affect the screen
/// (`CanMediaAffectScreen`): empty, or any comma part naming
/// `screen`/`all` (generously: anything containing those tokens
/// first, honoring `not`).
#[must_use]
pub fn can_media_affect_screen(media: &str) -> bool {
    if media.is_empty() {
        return true;
    }
    for mut current in media.split(',') {
        current = current.trim_start();
        // A leading "only" is a no-op in CSS3.
        starts_with_word("only", &mut current);
        let initial_not = starts_with_word("not", &mut current);
        if starts_with_word("screen", &mut current)
            || starts_with_word("all", &mut current)
            || current.is_empty()
            || current.starts_with('(')
        {
            // Affects screen, unless there was an initial not.
            if !initial_not {
                return true;
            }
        } else if initial_not {
            // Something like "not print" that affects screen.
            return true;
        }
    }
    false
}

/// Splits a media attribute into trimmed queries
/// (`VectorizeMediaAttribute`): empties dropped; any `all`
/// collapses the whole list to empty (meaning "all media").
#[must_use]
pub fn vectorize_media_attribute(media: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for part in media.split(|byte| *byte == b',') {
        let trimmed = trim_ascii_whitespace(part);
        if trimmed.eq_ignore_ascii_case(b"all") {
            out.clear();
            break;
        } else if !trimmed.is_empty() {
            out.push(trimmed.to_vec());
        }
    }
    out
}

/// Joins media queries (`StringifyMediaVector`): empty means
/// `all`, otherwise comma-joined.
#[must_use]
pub fn stringify_media_vector(media: &[Vec<u8>]) -> Vec<u8> {
    if media.is_empty() {
        return b"all".to_vec();
    }
    let mut out = Vec::new();
    for (index, query) in media.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.extend_from_slice(query);
    }
    out
}

/// Trims ASCII whitespace on both ends.
fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

/// Beacon-comparable selector (`JsDetectableSelector`): drops
/// pseudo-class parts per compound, truncating at the first
/// compound left empty.
#[must_use]
pub fn js_detectable_selector(selector: &ps_css::selector::Selector) -> Vec<u8> {
    use ps_css::selector::{SimpleSelectorType, SimpleSelectors};
    let mut trimmed: ps_css::selector::Selector = Vec::new();
    for chain in selector.iter() {
        let mut kept = SimpleSelectors::new(chain.combinator());
        for simple in chain.iter() {
            if simple.kind() != SimpleSelectorType::Pseudoclass {
                kept.push(simple.clone());
            }
        }
        if kept.is_empty() {
            break;
        }
        trimmed.push(kept);
    }
    ps_css::selector::selector_to_css_string(&trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_screen_matrix() {
        assert!(can_media_affect_screen(""));
        assert!(can_media_affect_screen("screen"));
        assert!(can_media_affect_screen("all"));
        assert!(can_media_affect_screen("screen and (min-width: 1px)"));
        assert!(can_media_affect_screen("only screen"));
        assert!(can_media_affect_screen("not print"));
        assert!(can_media_affect_screen("print, screen"));
        assert!(!can_media_affect_screen("print"));
        assert!(!can_media_affect_screen("not screen"));
        assert!(!can_media_affect_screen("not all"));
        assert!(!can_media_affect_screen("print, projection"));
    }
}
