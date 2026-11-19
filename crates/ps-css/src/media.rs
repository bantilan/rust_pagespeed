//! Media queries (`media.h`, `media.cc`).
#![forbid(unsafe_code)]

use crate::unicode_text::UnicodeText;

// C++ object sizes on 64-bit libstdc++ (verified with a layout probe):
// `sizeof(MediaQueries)` = 24 (bare vector), `sizeof(MediaQuery)` =
// 56, `sizeof(MediaExpression)` = 56.
const SIZEOF_MEDIA_QUERIES: i64 = 24;
const SIZEOF_MEDIA_QUERY: i64 = 56;
const SIZEOF_MEDIA_EXPRESSION: i64 = 56;

/// One media feature (`MediaExpression`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaExpression {
    name: UnicodeText,
    has_value: bool,
    value: UnicodeText,
}

impl MediaExpression {
    /// Feature without a value, e.g. `(color)`.
    #[must_use]
    pub fn without_value(name: UnicodeText) -> MediaExpression {
        MediaExpression {
            name,
            has_value: false,
            value: UnicodeText::new(),
        }
    }

    /// Feature with an unparsed value, e.g. `(max-width: 500px)`.
    #[must_use]
    pub fn with_value(name: UnicodeText, value: UnicodeText) -> MediaExpression {
        MediaExpression {
            name,
            has_value: true,
            value,
        }
    }

    /// Feature name (`name()`).
    #[must_use]
    pub fn name(&self) -> &UnicodeText {
        &self.name
    }

    /// Whether a value is present (`has_value()`).
    #[must_use]
    pub fn has_value(&self) -> bool {
        self.has_value
    }

    /// Raw value (`value()`).
    #[must_use]
    pub fn value(&self) -> &UnicodeText {
        &self.value
    }

    /// Debug serialization (`MediaExpression::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::escapes::{escape_unicode_identifier, escape_unicode_string};
        let mut out = b"(".to_vec();
        out.extend_from_slice(&escape_unicode_identifier(&self.name));
        if self.has_value {
            out.extend_from_slice(b": ");
            out.extend_from_slice(&escape_unicode_string(&self.value));
        }
        out.push(b')');
        out
    }
}

/// Media query qualifiers (`MediaQuery::MediaQualifier`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MediaQualifier {
    Only,
    Not,
    #[default]
    None,
}

/// One media query (`MediaQuery`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaQuery {
    qualifier: MediaQualifier,
    media_type: UnicodeText,
    expressions: Vec<MediaExpression>,
}

impl MediaQuery {
    /// Empty query (`MediaQuery()`).
    #[must_use]
    pub fn new() -> MediaQuery {
        MediaQuery::default()
    }

    /// Qualifier (`qualifier()`).
    #[must_use]
    pub fn qualifier(&self) -> MediaQualifier {
        self.qualifier
    }

    /// Media type (`media_type()`).
    #[must_use]
    pub fn media_type(&self) -> &UnicodeText {
        &self.media_type
    }

    /// Expressions (`expressions()`).
    #[must_use]
    pub fn expressions(&self) -> &[MediaExpression] {
        &self.expressions
    }

    /// Expression `i` (`expression(i)`).
    #[must_use]
    pub fn expression(&self, index: usize) -> &MediaExpression {
        &self.expressions[index]
    }

    /// Sets the qualifier (`set_qualifier`).
    pub fn set_qualifier(&mut self, qualifier: MediaQualifier) {
        self.qualifier = qualifier;
    }

    /// Sets the media type (`set_media_type`).
    pub fn set_media_type(&mut self, media_type: UnicodeText) {
        self.media_type = media_type;
    }

    /// Adds an expression (`add_expression`).
    pub fn add_expression(&mut self, expression: MediaExpression) {
        self.expressions.push(expression);
    }

    /// Debug serialization (`MediaQuery::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::escapes::escape_unicode_identifier;
        use crate::value::join_css_strings;
        let mut out = Vec::new();
        match self.qualifier {
            MediaQualifier::Only => out.extend_from_slice(b"only "),
            MediaQualifier::Not => out.extend_from_slice(b"not "),
            MediaQualifier::None => {}
        }
        out.extend_from_slice(&escape_unicode_identifier(&self.media_type));
        if !self.media_type.is_empty() && !self.expressions.is_empty() {
            out.extend_from_slice(b" and ");
        }
        out.extend_from_slice(&join_css_strings(
            self.expressions.iter().map(MediaExpression::to_css_string),
            b" and ",
        ));
        out
    }
}

/// Media query list (`MediaQueries`).
pub type MediaQueries = Vec<MediaQuery>;

/// Debug serialization (`MediaQueries::ToString`).
#[must_use]
pub fn media_queries_to_css_string(queries: &MediaQueries) -> Vec<u8> {
    crate::value::join_css_strings(queries.iter().map(MediaQuery::to_css_string), b", ")
}

/// Approximate heap bytes (`ApproximateBytes`).
#[must_use]
pub fn approximate_bytes(queries: &MediaQueries) -> i64 {
    let mut bytes = SIZEOF_MEDIA_QUERIES;
    for query in queries {
        bytes += SIZEOF_MEDIA_QUERY + query.media_type.byte_len() as i64;
        for expression in &query.expressions {
            bytes += SIZEOF_MEDIA_EXPRESSION
                + expression.name.byte_len() as i64
                + if expression.has_value {
                    expression.value.byte_len() as i64
                } else {
                    0
                };
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approximate_vectors() {
        let queries: MediaQueries = Vec::new();
        assert_eq!(approximate_bytes(&queries), 24);
        let mut query = MediaQuery::new();
        query.set_media_type(UnicodeText::from_utf8(b"screen"));
        query.add_expression(MediaExpression::with_value(
            UnicodeText::from_utf8(b"max-width"),
            UnicodeText::from_utf8(b"500px"),
        ));
        let queries = vec![query];
        assert_eq!(approximate_bytes(&queries), 24 + 56 + 6 + 56 + 9 + 5);
    }
}
