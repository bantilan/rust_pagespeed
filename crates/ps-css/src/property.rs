//! CSS properties (`property.h`, `property.gperf`).
//!
//! Recognized properties map to [`Prop`]; anything else keeps its
//! ASCII-lowercased text under [`Prop::Other`]. Lookup folds ASCII case
//! (gperf `%ignore-case`).
#![forbid(unsafe_code)]

use crate::string_util::lowercase_ascii;
use crate::unicode_text::UnicodeText;

#[allow(clippy::all)]
#[path = "gen/property_gen.rs"]
mod generated;

pub use generated::Prop;

/// A property: recognized enum or stored text (`Property`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    prop: Prop,
    other: UnicodeText,
}

impl Property {
    /// Classifies text (`Property(UnicodeText)`).
    #[must_use]
    pub fn from_text(text: &UnicodeText) -> Property {
        let prop = Prop::from_text(text.as_bytes());
        let other = if prop == Prop::Other {
            lowercase_ascii(text)
        } else {
            UnicodeText::new()
        };
        Property { prop, other }
    }

    /// Wraps a known property (`Property(Prop)`).
    #[must_use]
    pub fn from_prop(prop: Prop) -> Property {
        Property {
            prop,
            other: UnicodeText::new(),
        }
    }

    /// The property enum (`prop()`).
    #[must_use]
    pub fn prop(&self) -> Prop {
        self.prop
    }

    /// The property as text (`prop_text()`).
    #[must_use]
    pub fn prop_text(&self) -> Vec<u8> {
        if self.prop == Prop::Other {
            self.other.as_bytes().to_vec()
        } else {
            self.prop.text().as_bytes().to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_vectors() {
        assert_eq!(
            Property::from_text(&UnicodeText::from_utf8(b"color")).prop(),
            Prop::Color
        );
        assert_eq!(
            Property::from_text(&UnicodeText::from_utf8(b"font-family")).prop(),
            Prop::FontFamily
        );
        // Uppercase still hits the table (canonical text).
        let upper = Property::from_text(&UnicodeText::from_utf8(b"COLOR"));
        assert_eq!(upper.prop(), Prop::Color);
        assert_eq!(upper.prop_text(), b"color");
        // Unknown keeps (lowercased) text.
        let custom = Property::from_text(&UnicodeText::from_utf8(b"--My-Var"));
        assert_eq!(custom.prop(), Prop::Other);
        assert_eq!(custom.prop_text(), b"--my-var");
        assert_eq!(Prop::Color.text(), "color");
        assert_eq!(Prop::Other.text(), "OTHER");
    }
}
