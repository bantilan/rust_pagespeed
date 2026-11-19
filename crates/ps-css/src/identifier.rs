//! CSS identifiers (`identifier.h`, `identifier.gperf`).
//!
//! Recognized idents map to [`Ident`]; anything else keeps its text
//! under [`Ident::Other`]. Lookup folds ASCII case (gperf `%ignore-case`);
//! matched idents report canonical lowercase text.
#![forbid(unsafe_code)]

use crate::unicode_text::UnicodeText;

#[allow(clippy::all)]
#[path = "gen/identifier_gen.rs"]
mod generated;

pub use generated::Ident;

/// An identifier: recognized enum or verbatim text (`Identifier`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    ident: Ident,
    other: UnicodeText,
}

impl Identifier {
    /// Unknown identifier (`Identifier()`).
    #[must_use]
    pub fn unknown() -> Identifier {
        Identifier {
            ident: Ident::GoogUnknown,
            other: UnicodeText::new(),
        }
    }

    /// Const unknown identifier (for static fallbacks).
    #[must_use]
    pub const fn const_unknown() -> Identifier {
        Identifier {
            ident: Ident::GoogUnknown,
            other: UnicodeText::new_const_empty(),
        }
    }

    /// Classifies text (`Identifier(const UnicodeText&)`).
    #[must_use]
    pub fn from_text(text: &UnicodeText) -> Identifier {
        let ident = Ident::from_text(text.as_bytes());
        let other = if ident == Ident::Other {
            text.clone()
        } else {
            UnicodeText::new()
        };
        Identifier { ident, other }
    }

    /// Wraps a known ident (`Identifier(Ident)`).
    #[must_use]
    pub fn from_ident(ident: Ident) -> Identifier {
        Identifier {
            ident,
            other: UnicodeText::new(),
        }
    }

    /// The ident enum (`ident()`).
    #[must_use]
    pub fn ident(&self) -> Ident {
        self.ident
    }

    /// The ident as text (`ident_text()`).
    #[must_use]
    pub fn ident_text(&self) -> UnicodeText {
        if self.ident == Ident::Other {
            self.other.clone()
        } else {
            UnicodeText::from_utf8(self.ident.text().as_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_vectors() {
        assert_eq!(
            Identifier::from_text(&UnicodeText::from_utf8(b"normal")).ident(),
            Ident::Normal
        );
        assert_eq!(
            Identifier::from_text(&UnicodeText::from_utf8(b"no-repeat")).ident(),
            Ident::NoRepeat
        );
        // Case-insensitive match, canonical lowercase text.
        let upper = Identifier::from_text(&UnicodeText::from_utf8(b"NORMAL"));
        assert_eq!(upper.ident(), Ident::Normal);
        assert_eq!(upper.ident_text().as_bytes(), b"normal");
        // Unknown keeps text.
        let custom = Identifier::from_text(&UnicodeText::from_utf8(b"--my-var"));
        assert_eq!(custom.ident(), Ident::Other);
        assert_eq!(custom.ident_text().as_bytes(), b"--my-var");
        assert_eq!(Ident::Normal.text(), "normal");
        assert_eq!(Ident::Other.text(), "OTHER");
    }
}
