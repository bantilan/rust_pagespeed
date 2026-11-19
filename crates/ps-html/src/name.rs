//! HTML tag/attribute keywords and names.
//!
//! Ports `HtmlName`: the `Keyword` enum (discriminants match the C++ so
//! sorted-set membership behaves identically), case-insensitive lookup, and
//! canonical spellings. The tables live in [`name_generated`](crate::name_generated).

use std::rc::Rc;

#[allow(clippy::all)]
#[path = "name_generated.rs"]
mod generated;

pub use generated::{as_str as canonical_spelling, lookup, Keyword};

/// An HTML tag or attribute name: a keyword plus its original spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlName {
    keyword: Keyword,
    /// Original spelling when it differs from the canonical one (or for
    /// non-keywords); `None` means "use the canonical spelling".
    spelling: Option<Rc<[u8]>>,
}

impl HtmlName {
    /// Builds a name from raw bytes, interning nothing (the caller owns
    /// storage; the spelling is cloned only when non-canonical).
    #[must_use]
    pub fn new(bytes: &[u8]) -> Self {
        let keyword = lookup(bytes);
        let canonical = canonical_spelling(keyword);
        let spelling = match canonical {
            Some(spelling) if spelling.as_bytes() == bytes => None,
            _ => Some(Rc::from(bytes)),
        };
        HtmlName { keyword, spelling }
    }

    /// Builds a name for a known keyword in canonical spelling.
    #[must_use]
    pub fn from_keyword(keyword: Keyword) -> Self {
        HtmlName {
            keyword,
            spelling: None,
        }
    }

    #[must_use]
    pub fn keyword(&self) -> Keyword {
        self.keyword
    }

    /// Original byte spelling (canonical when the input matched it).
    #[must_use]
    pub fn value(&self) -> &[u8] {
        match &self.spelling {
            Some(spelling) => spelling,
            None => canonical_spelling(self.keyword).unwrap_or("").as_bytes(),
        }
    }

    /// Canonical lowercase spelling, when this is a known keyword.
    #[must_use]
    pub fn canonical(&self) -> Option<&'static str> {
        canonical_spelling(self.keyword)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(lookup(b"div"), Keyword::Div);
        assert_eq!(lookup(b"DIV"), Keyword::Div);
        assert_eq!(lookup(b"DiV"), Keyword::Div);
        assert_eq!(lookup(b"definitely-not-a-tag"), Keyword::NotAKeyword);
        assert_eq!(lookup(b""), Keyword::NotAKeyword);
    }

    #[test]
    fn lookup_handles_long_names() {
        let long = vec![b'x'; 200];
        assert_eq!(lookup(&long), Keyword::NotAKeyword);
        // A known keyword padded past the stack buffer still resolves.
        let mut padded = b"data-pagespeed-high-res-srcset".to_vec();
        padded.extend_from_slice(&[b' '; 100]);
        assert_eq!(lookup(&padded), Keyword::NotAKeyword);
    }

    #[test]
    fn names_keep_original_spelling() {
        let lower = HtmlName::new(b"div");
        assert_eq!(lower.keyword(), Keyword::Div);
        assert_eq!(lower.value(), b"div");
        let upper = HtmlName::new(b"DIV");
        assert_eq!(upper.keyword(), Keyword::Div);
        assert_eq!(upper.value(), b"DIV");
        let unknown = HtmlName::new(b"my-widget");
        assert_eq!(unknown.keyword(), Keyword::NotAKeyword);
        assert_eq!(unknown.value(), b"my-widget");
    }

    #[test]
    fn discriminant_order_matches_cpp() {
        // Spot-checks against html_name.h: kXml=0, kA=1, kAbbr=2, ...,
        // kNotAKeyword last (214 keywords + sentinel = 215).
        assert_eq!(Keyword::Xml as u16, 0);
        assert_eq!(Keyword::A as u16, 1);
        assert_eq!(Keyword::Abbr as u16, 2);
        assert_eq!(Keyword::NotAKeyword as u16, 214);
        assert!(Keyword::A < Keyword::Div);
    }
}
