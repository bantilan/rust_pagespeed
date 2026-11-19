//! CSS selectors (`selector.h`, `selector.cc`).
#![forbid(unsafe_code)]

use crate::tags::HtmlTag;
use crate::unicode_text::UnicodeText;

/// Simple selector types (`SimpleSelector::Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimpleSelectorType {
    ElementType,
    Universal,
    ExistAttribute,
    ExactAttribute,
    OneOfAttribute,
    BeginHyphenAttribute,
    BeginWithAttribute,
    EndWithAttribute,
    SubstringAttribute,
    Class,
    Id,
    Pseudoclass,
    Lang,
}

impl SimpleSelectorType {
    /// Maps an attribute operator to its type
    /// (`AttributeTypeFromOperator`).
    #[must_use]
    pub fn from_attribute_operator(oper: u8) -> SimpleSelectorType {
        match oper {
            b'=' => SimpleSelectorType::ExactAttribute,
            b'~' => SimpleSelectorType::OneOfAttribute,
            b'|' => SimpleSelectorType::BeginHyphenAttribute,
            b'^' => SimpleSelectorType::BeginWithAttribute,
            b'$' => SimpleSelectorType::EndWithAttribute,
            b'*' => SimpleSelectorType::SubstringAttribute,
            _ => panic!("invalid attribute operator {oper}"),
        }
    }
}

/// One simple selector (`SimpleSelector`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleSelector {
    kind: SimpleSelectorType,
    element_type: HtmlTag,
    element_text: UnicodeText,
    attribute: UnicodeText,
    value: UnicodeText,
}

impl SimpleSelector {
    /// Element type selector (`NewElementType`).
    #[must_use]
    pub fn new_element_type(name: UnicodeText) -> SimpleSelector {
        let tag = HtmlTag::find_builtin(name.as_bytes());
        SimpleSelector {
            kind: SimpleSelectorType::ElementType,
            element_type: tag,
            element_text: name,
            attribute: UnicodeText::new(),
            value: UnicodeText::new(),
        }
    }

    /// Universal selector (`NewUniversal`).
    #[must_use]
    pub fn new_universal() -> SimpleSelector {
        SimpleSelector::binary(
            SimpleSelectorType::Universal,
            UnicodeText::new(),
            UnicodeText::new(),
        )
    }

    /// Existence attribute selector (`NewExistAttribute`).
    #[must_use]
    pub fn new_exist_attribute(attribute: UnicodeText) -> SimpleSelector {
        SimpleSelector::binary(
            SimpleSelectorType::ExistAttribute,
            attribute,
            UnicodeText::new(),
        )
    }

    /// Binary attribute selector (`NewBinaryAttribute`).
    #[must_use]
    pub fn new_binary_attribute(
        kind: SimpleSelectorType,
        attribute: UnicodeText,
        value: UnicodeText,
    ) -> SimpleSelector {
        SimpleSelector::binary(kind, attribute, value)
    }

    /// Class selector (`NewClass`).
    #[must_use]
    pub fn new_class(classname: UnicodeText) -> SimpleSelector {
        SimpleSelector::binary(
            SimpleSelectorType::Class,
            UnicodeText::from_utf8(b"class"),
            classname,
        )
    }

    /// ID selector (`NewId`).
    #[must_use]
    pub fn new_id(id: UnicodeText) -> SimpleSelector {
        SimpleSelector::binary(SimpleSelectorType::Id, UnicodeText::from_utf8(b"id"), id)
    }

    /// Pseudoclass selector (`NewPseudoclass`).
    #[must_use]
    pub fn new_pseudoclass(pseudoclass: UnicodeText, separator: UnicodeText) -> SimpleSelector {
        SimpleSelector::binary(SimpleSelectorType::Pseudoclass, separator, pseudoclass)
    }

    /// Language selector (`NewLang`).
    #[must_use]
    pub fn new_lang(lang: UnicodeText) -> SimpleSelector {
        SimpleSelector::binary(SimpleSelectorType::Lang, UnicodeText::new(), lang)
    }

    fn binary(
        kind: SimpleSelectorType,
        attribute: UnicodeText,
        value: UnicodeText,
    ) -> SimpleSelector {
        SimpleSelector {
            kind,
            element_type: HtmlTag::Unknown,
            element_text: UnicodeText::new(),
            attribute,
            value,
        }
    }

    /// Selector type (`type()`).
    #[must_use]
    pub fn kind(&self) -> SimpleSelectorType {
        self.kind
    }

    /// Recognized tag (`element_type()`).
    #[must_use]
    pub fn element_type(&self) -> HtmlTag {
        self.element_type
    }

    /// Original-case element text (`element_text()`).
    #[must_use]
    pub fn element_text(&self) -> &UnicodeText {
        &self.element_text
    }

    /// Whether attribute/value accessors are valid
    /// (`IsAttributeCondition`).
    #[must_use]
    pub fn is_attribute_condition(&self) -> bool {
        matches!(
            self.kind,
            SimpleSelectorType::ExistAttribute
                | SimpleSelectorType::ExactAttribute
                | SimpleSelectorType::OneOfAttribute
                | SimpleSelectorType::BeginHyphenAttribute
                | SimpleSelectorType::BeginWithAttribute
                | SimpleSelectorType::EndWithAttribute
                | SimpleSelectorType::SubstringAttribute
                | SimpleSelectorType::Class
                | SimpleSelectorType::Id
        )
    }

    /// Attribute name (`attribute()`).
    #[must_use]
    pub fn attribute(&self) -> &UnicodeText {
        debug_assert!(self.is_attribute_condition());
        &self.attribute
    }

    /// Attribute value (`value()`).
    #[must_use]
    pub fn value(&self) -> &UnicodeText {
        debug_assert!(self.is_attribute_condition());
        &self.value
    }

    /// Pseudoclass name (`pseudoclass()`).
    #[must_use]
    pub fn pseudoclass(&self) -> &UnicodeText {
        debug_assert_eq!(self.kind, SimpleSelectorType::Pseudoclass);
        &self.value
    }

    /// Pseudoclass separator, `:` or `::` (`pseudoclass_separator()`).
    #[must_use]
    pub fn pseudoclass_separator(&self) -> &UnicodeText {
        debug_assert_eq!(self.kind, SimpleSelectorType::Pseudoclass);
        &self.attribute
    }

    /// Language (`lang()`).
    #[must_use]
    pub fn lang(&self) -> &UnicodeText {
        debug_assert_eq!(self.kind, SimpleSelectorType::Lang);
        &self.value
    }

    /// Debug serialization (`SimpleSelector::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::escapes::{
            escape_identifier_name, escape_unicode_identifier, escape_unicode_string,
        };
        let mut out = Vec::new();
        match self.kind {
            SimpleSelectorType::ElementType => {
                out.extend_from_slice(&escape_unicode_identifier(&self.element_text));
            }
            SimpleSelectorType::Universal => out.push(b'*'),
            SimpleSelectorType::ExistAttribute => {
                out.push(b'[');
                out.extend_from_slice(&escape_unicode_identifier(&self.attribute));
                out.push(b']');
            }
            SimpleSelectorType::ExactAttribute
            | SimpleSelectorType::OneOfAttribute
            | SimpleSelectorType::BeginHyphenAttribute
            | SimpleSelectorType::SubstringAttribute
            | SimpleSelectorType::BeginWithAttribute
            | SimpleSelectorType::EndWithAttribute => {
                let operator = match self.kind {
                    SimpleSelectorType::ExactAttribute => "=",
                    SimpleSelectorType::OneOfAttribute => "~=",
                    SimpleSelectorType::BeginHyphenAttribute => "|=",
                    SimpleSelectorType::SubstringAttribute => "*=",
                    SimpleSelectorType::BeginWithAttribute => "^=",
                    _ => "$=",
                };
                out.push(b'[');
                out.extend_from_slice(&escape_unicode_identifier(&self.attribute));
                out.extend_from_slice(operator.as_bytes());
                out.push(b'"');
                out.extend_from_slice(&escape_unicode_string(&self.value));
                out.extend_from_slice(b"\"]");
            }
            SimpleSelectorType::Class => {
                out.push(b'.');
                out.extend_from_slice(&escape_identifier_name(self.value.as_bytes()));
            }
            SimpleSelectorType::Id => {
                out.push(b'#');
                out.extend_from_slice(&escape_identifier_name(self.value.as_bytes()));
            }
            SimpleSelectorType::Pseudoclass => {
                out.extend_from_slice(self.pseudoclass_separator().as_bytes());
                out.extend_from_slice(&escape_unicode_identifier(self.pseudoclass()));
            }
            SimpleSelectorType::Lang => {
                out.extend_from_slice(b":lang(");
                out.extend_from_slice(&escape_unicode_identifier(self.lang()));
                out.push(b')');
            }
        }
        out
    }
}

/// Combinators (`SimpleSelectors::Combinator`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    None,
    Descendant,
    Child,
    Sibling,
}

/// AND-chain of simple selectors (`SimpleSelectors`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleSelectors {
    combinator: Combinator,
    selectors: Vec<SimpleSelector>,
}

impl SimpleSelectors {
    /// Empty chain with a combinator.
    #[must_use]
    pub fn new(combinator: Combinator) -> SimpleSelectors {
        SimpleSelectors {
            combinator,
            selectors: Vec::new(),
        }
    }

    /// Combinator to the previous chain (`combinator()`).
    #[must_use]
    pub fn combinator(&self) -> Combinator {
        self.combinator
    }

    /// Pushes a simple selector.
    pub fn push(&mut self, selector: SimpleSelector) {
        self.selectors.push(selector);
    }

    /// Get by index (`get(i)`).
    #[must_use]
    pub fn get(&self, index: usize) -> &SimpleSelector {
        &self.selectors[index]
    }

    /// Iterates the chain.
    pub fn iter(&self) -> std::slice::Iter<'_, SimpleSelector> {
        self.selectors.iter()
    }

    /// Chain length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.selectors.len()
    }

    /// Whether the chain is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selectors.is_empty()
    }

    /// Debug serialization (`SimpleSelectors::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        use crate::value::join_css_strings;
        let mut out = Vec::new();
        match self.combinator {
            Combinator::Child => out.extend_from_slice(b"> "),
            Combinator::Sibling => out.extend_from_slice(b"+ "),
            Combinator::None | Combinator::Descendant => {}
        }
        out.extend_from_slice(&join_css_strings(
            self.selectors.iter().map(SimpleSelector::to_css_string),
            b"",
        ));
        out
    }
}

/// Debug serialization (`Selector::ToString`).
#[must_use]
pub fn selector_to_css_string(selector: &Selector) -> Vec<u8> {
    crate::value::join_css_strings(selector.iter().map(SimpleSelectors::to_css_string), b" ")
}

/// Chain of simple-selector sequences (`Selector`).
pub type Selector = Vec<SimpleSelectors>;

impl std::ops::Index<usize> for SimpleSelectors {
    type Output = SimpleSelector;

    fn index(&self, index: usize) -> &SimpleSelector {
        &self.selectors[index]
    }
}

/// Selector list with dummy flag (`Selectors`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selectors {
    selectors: Vec<Selector>,
    is_dummy: bool,
    dummy_bytes: Vec<u8>,
}

impl Selectors {
    /// Empty list.
    #[must_use]
    pub fn new() -> Selectors {
        Selectors::default()
    }

    /// Dummy list holding verbatim bytes (preservation mode).
    #[must_use]
    pub fn dummy(bytes: &[u8]) -> Selectors {
        Selectors {
            selectors: Vec::new(),
            is_dummy: true,
            dummy_bytes: bytes.to_vec(),
        }
    }

    /// Whether this is a dummy (`is_dummy()`).
    #[must_use]
    pub fn is_dummy(&self) -> bool {
        self.is_dummy
    }

    /// Verbatim bytes of a dummy (`bytes_in_original_buffer()`).
    #[must_use]
    pub fn bytes_in_original_buffer(&self) -> &[u8] {
        &self.dummy_bytes
    }

    /// Replaces dummy bytes (`set_bytes_in_original_buffer`).
    pub fn set_bytes_in_original_buffer(&mut self, bytes: &[u8]) {
        self.dummy_bytes = bytes.to_vec();
    }

    /// Pushes a selector.
    pub fn push(&mut self, selector: Selector) {
        self.selectors.push(selector);
    }

    /// Get by index (`at(i)`).
    #[must_use]
    pub fn at(&self, index: usize) -> &Selector {
        &self.selectors[index]
    }

    /// Iterates selectors.
    pub fn iter(&self) -> std::slice::Iter<'_, Selector> {
        self.selectors.iter()
    }

    /// List length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.selectors.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selectors.is_empty()
    }

    /// Debug serialization (`Selectors::ToString`).
    #[must_use]
    pub fn to_css_string(&self) -> Vec<u8> {
        if self.is_dummy {
            let mut out = b"/* Unparsed selectors: */ ".to_vec();
            out.extend_from_slice(&self.dummy_bytes);
            return out;
        }
        crate::value::join_css_strings(self.selectors.iter().map(selector_to_css_string), b", ")
    }
}

impl std::ops::Index<usize> for Selectors {
    type Output = Selector;

    fn index(&self, index: usize) -> &Selector {
        &self.selectors[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_vectors() {
        let element = SimpleSelector::new_element_type(UnicodeText::from_utf8(b"p"));
        assert_eq!(element.kind(), SimpleSelectorType::ElementType);
        assert_eq!(element.element_type(), HtmlTag::P);
        let class = SimpleSelector::new_class(UnicodeText::from_utf8(b"a"));
        assert_eq!(class.attribute().as_str(), "class");
        assert_eq!(
            SimpleSelectorType::from_attribute_operator(b'~'),
            SimpleSelectorType::OneOfAttribute
        );
    }
}
