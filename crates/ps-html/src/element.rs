//! DOM arena: elements, leaf nodes, and attributes.
//!
//! Ports `HtmlNode`, `HtmlElement` (+ `Attribute`), and the leaf node
//! classes. Nodes live in an arena indexed by [`NodeId`]; event-queue
//! positions are [`event::SlotId`] handles. Parent pointers, liveness, and
//! the decoded-value cache follow the C++ exactly.

use crate::keywords;
use crate::name::{HtmlName, Keyword};

/// Arena index of a node.
pub type NodeId = u32;
/// Absent node id.
pub const NO_NODE: NodeId = u32::MAX;

/// Attribute quote style (`HtmlElement::QuoteStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteStyle {
    NoQuote,
    SingleQuote,
    #[default]
    DoubleQuote,
}

impl QuoteStyle {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            QuoteStyle::NoQuote => "",
            QuoteStyle::SingleQuote => "'",
            QuoteStyle::DoubleQuote => "\"",
        }
    }
}

/// An element attribute: name, escaped value, quote style, decoded cache.
#[derive(Debug, Clone)]
pub struct Attribute {
    name: HtmlName,
    /// Escaped value as parsed; `None` means valueless (`<foo bar>`).
    escaped_value: Option<Vec<u8>>,
    /// Lazily computed decoded value: `None` = not computed yet;
    /// `Some(None)` = computed, failed or valueless.
    decoded_cache: Option<Option<Vec<u8>>>,
    decoding_error: bool,
    quote_style: QuoteStyle,
}

impl Attribute {
    #[must_use]
    pub fn new(name: HtmlName, escaped_value: Option<Vec<u8>>, quote_style: QuoteStyle) -> Self {
        Attribute {
            name,
            escaped_value,
            decoded_cache: None,
            decoding_error: false,
            quote_style,
        }
    }

    #[must_use]
    pub fn name(&self) -> &HtmlName {
        &self.name
    }

    #[must_use]
    pub fn name_str(&self) -> &[u8] {
        self.name.value()
    }

    #[must_use]
    pub fn keyword(&self) -> Keyword {
        self.name.keyword()
    }

    pub fn set_name(&mut self, name: HtmlName) {
        self.name = name;
    }

    /// Raw escaped value, or `None` for valueless attributes.
    #[must_use]
    pub fn escaped_value(&self) -> Option<&[u8]> {
        self.escaped_value.as_deref()
    }

    /// Decoded value, or `None` when valueless or undecodable
    /// (`DecodedValueOrNull`, computed once and cached).
    pub fn decoded_value_or_null(&mut self) -> Option<&[u8]> {
        if self.decoded_cache.is_none() {
            let (decoded, error) = match &self.escaped_value {
                Some(escaped) => match keywords::unescape(escaped) {
                    Some(decoded) => (Some(decoded), false),
                    None => (None, true),
                },
                None => (None, false),
            };
            self.decoding_error = error;
            self.decoded_cache = Some(decoded);
        }
        self.decoded_cache.as_ref().unwrap().as_deref()
    }

    #[must_use]
    pub fn decoding_error(&self) -> bool {
        self.decoding_error
    }

    #[must_use]
    pub fn quote_style(&self) -> QuoteStyle {
        self.quote_style
    }

    pub fn set_quote_style(&mut self, style: QuoteStyle) {
        self.quote_style = style;
    }

    /// Sets a decoded value, re-escaping it (`SetValue`). The quote style is
    /// repaired when the value needs quotes.
    pub fn set_value(&mut self, value: &[u8]) {
        let mut escaped = Vec::new();
        keywords::escape(value, &mut escaped);
        self.quote_style = quote_style_for_escaped_value(self.quote_style, Some(&escaped));
        self.escaped_value = Some(escaped);
        self.decoded_cache = Some(Some(value.to_vec()));
        self.decoding_error = false;
    }

    /// Sets the escaped value, invalidating the decoded cache
    /// (`SetEscapedValue`). Pass `None` for a valueless attribute.
    pub fn set_escaped_value(&mut self, value: Option<&[u8]>) {
        self.decoded_cache = None;
        self.decoding_error = false;
        self.escaped_value = value.map(<[u8]>::to_vec);
        self.quote_style =
            quote_style_for_escaped_value(self.quote_style, self.escaped_value.as_deref());
    }
}

/// True when an escaped value cannot stay unquoted (`EscapedValueNeedsQuotes`):
/// empty, or holding whitespace, quotes, angle brackets, or backtick.
#[must_use]
pub fn escaped_value_needs_quotes(value: &[u8]) -> bool {
    value.is_empty()
        || value.iter().any(|byte| {
            matches!(
                byte,
                b' ' | b'\t' | b'\n' | 0x0c | b'\r' | b'"' | b'\'' | b'<' | b'>' | b'`'
            )
        })
}

/// Repairs an unquoted style for a value that needs quotes
/// (`QuoteStyleForEscapedValue`): single quotes when the value holds a
/// double quote but no single quote, else double quotes.
#[must_use]
pub fn quote_style_for_escaped_value(
    style: QuoteStyle,
    escaped_value: Option<&[u8]>,
) -> QuoteStyle {
    if style == QuoteStyle::NoQuote {
        if let Some(value) = escaped_value {
            if escaped_value_needs_quotes(value) {
                if value.contains(&b'"') && !value.contains(&b'\'') {
                    return QuoteStyle::SingleQuote;
                }
                return QuoteStyle::DoubleQuote;
            }
        }
    }
    style
}

/// Element close style (`HtmlElement::Style`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementStyle {
    AutoClose,
    BriefClose,
    ExplicitClose,
    ImplicitClose,
    Invisible,
    Unclosed,
}

/// Element node data.
#[derive(Debug, Clone)]
pub struct ElementData {
    pub name: HtmlName,
    pub attributes: Vec<Attribute>,
    pub style: ElementStyle,
    pub parent: Option<NodeId>,
    pub begin: crate::event::SlotId,
    pub end: crate::event::SlotId,
    pub begin_line_number: i32,
    pub end_line_number: i32,
    pub live: bool,
}

/// Leaf node kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafKind {
    Characters,
    Comment,
    Cdata,
    IEDirective,
    Directive,
}

/// Leaf node data (characters, comments, CDATA, directives).
#[derive(Debug, Clone)]
pub struct LeafData {
    pub kind: LeafKind,
    pub contents: Vec<u8>,
    pub parent: Option<NodeId>,
    pub slot: crate::event::SlotId,
    pub live: bool,
}

/// Arena node.
#[derive(Debug, Clone)]
pub enum NodeData {
    Element(ElementData),
    Leaf(LeafData),
}

impl NodeData {
    #[must_use]
    pub fn parent(&self) -> Option<NodeId> {
        match self {
            NodeData::Element(element) => element.parent,
            NodeData::Leaf(leaf) => leaf.parent,
        }
    }

    pub fn set_parent(&mut self, parent: Option<NodeId>) {
        match self {
            NodeData::Element(element) => element.parent = parent,
            NodeData::Leaf(leaf) => leaf.parent = parent,
        }
    }

    #[must_use]
    pub fn live(&self) -> bool {
        match self {
            NodeData::Element(element) => element.live,
            NodeData::Leaf(leaf) => leaf.live,
        }
    }

    /// Queue slot of the start event (elements) or the leaf event.
    #[must_use]
    pub fn begin(&self) -> crate::event::SlotId {
        match self {
            NodeData::Element(element) => element.begin,
            NodeData::Leaf(leaf) => leaf.slot,
        }
    }

    /// Queue slot of the end event (elements) or the leaf event.
    #[must_use]
    pub fn end(&self) -> crate::event::SlotId {
        match self {
            NodeData::Element(element) => element.end,
            NodeData::Leaf(leaf) => leaf.slot,
        }
    }
}

/// Node arena. Ids are append-only indices; nodes are never removed (the C++
/// frees them all at end-of-parse), so ids stay valid for the document.
#[derive(Debug, Default)]
pub struct Arena {
    nodes: Vec<NodeData>,
}

impl Arena {
    #[must_use]
    pub fn new() -> Self {
        Arena::default()
    }

    pub fn push(&mut self, node: NodeData) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(node);
        id
    }

    #[must_use]
    pub fn get(&self, id: NodeId) -> &NodeData {
        &self.nodes[id as usize]
    }

    pub fn get_mut(&mut self, id: NodeId) -> &mut NodeData {
        &mut self.nodes[id as usize]
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_value_triad() {
        // <tag a="val">, <tag a="">, <tag a>, undecodable.
        let mut with_value = Attribute::new(
            HtmlName::new(b"href"),
            Some(b"a&amp;b".to_vec()),
            QuoteStyle::DoubleQuote,
        );
        assert_eq!(with_value.escaped_value(), Some(b"a&amp;b".as_slice()));
        assert_eq!(with_value.decoded_value_or_null(), Some(b"a&b".as_slice()));
        assert!(!with_value.decoding_error());

        let mut empty = Attribute::new(
            HtmlName::new(b"alt"),
            Some(Vec::new()),
            QuoteStyle::DoubleQuote,
        );
        assert_eq!(empty.decoded_value_or_null(), Some(b"".as_slice()));

        let mut valueless = Attribute::new(HtmlName::new(b"selected"), None, QuoteStyle::NoQuote);
        assert_eq!(valueless.decoded_value_or_null(), None);
        assert!(!valueless.decoding_error());

        let mut undecodable = Attribute::new(
            HtmlName::new(b"title"),
            Some(b"mu\xc3\xb1ecos".to_vec()),
            QuoteStyle::DoubleQuote,
        );
        assert_eq!(undecodable.decoded_value_or_null(), None);
        assert!(undecodable.decoding_error());
    }

    #[test]
    fn set_value_reescapes_and_repairs_quotes() {
        let mut attr = Attribute::new(
            HtmlName::new(b"href"),
            Some(b"old".to_vec()),
            QuoteStyle::NoQuote,
        );
        attr.set_value(b"a&b c");
        assert_eq!(attr.escaped_value(), Some(b"a&amp;b c".as_slice()));
        assert_eq!(attr.decoded_value_or_null(), Some(b"a&b c".as_slice()));
        assert_eq!(attr.quote_style(), QuoteStyle::DoubleQuote);

        // A double quote in a decoded value escapes to `&quot;`, leaving no
        // unsafe chars behind, so the attribute stays unquoted (C++ `SetValue`
        // decides from the escaped value).
        let mut attr = Attribute::new(
            HtmlName::new(b"href"),
            Some(b"old".to_vec()),
            QuoteStyle::NoQuote,
        );
        attr.set_value(b"say\"hi");
        assert_eq!(attr.escaped_value(), Some(b"say&quot;hi".as_slice()));
        assert_eq!(attr.quote_style(), QuoteStyle::NoQuote);

        // The single-quote repair only triggers for escaped values carrying a
        // literal double quote (C++ `EscapedValueWithDoubleQuoteGetsSingleQuote`).
        let mut attr = Attribute::new(
            HtmlName::new(b"href"),
            Some(b"old".to_vec()),
            QuoteStyle::NoQuote,
        );
        attr.set_escaped_value(Some(b"37\"a b"));
        assert_eq!(attr.quote_style(), QuoteStyle::SingleQuote);
    }

    #[test]
    fn set_escaped_value_invalidates_cache() {
        let mut attr = Attribute::new(
            HtmlName::new(b"href"),
            Some(b"a&amp;b".to_vec()),
            QuoteStyle::DoubleQuote,
        );
        assert_eq!(attr.decoded_value_or_null(), Some(b"a&b".as_slice()));
        attr.set_escaped_value(Some(b"c&amp;d"));
        assert_eq!(attr.decoded_value_or_null(), Some(b"c&d".as_slice()));
        attr.set_escaped_value(None);
        assert_eq!(attr.decoded_value_or_null(), None);
    }

    #[test]
    fn quote_need_predicate() {
        assert!(escaped_value_needs_quotes(b""));
        assert!(escaped_value_needs_quotes(b"a b"));
        assert!(escaped_value_needs_quotes(b"a`b"));
        assert!(!escaped_value_needs_quotes(b"abc-123"));
    }
}
