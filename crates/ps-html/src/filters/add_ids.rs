//! Structural element ids (`AddIdsFilter`).
//!
//! Tags ids onto div-like elements (and anything classed) so later passes
//! can name them: `PageSpeed[-explicit-id]-count-path`, where the path
//! counts the element among its siblings at each level below the nearest
//! explicit id.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Id prefix (`kIdPrefix`).
const ID_PREFIX: &[u8] = b"PageSpeed";
/// Stack marker for an explicit id (`kIsId`).
const IS_ID: i32 = -1;

fn is_div_like(tag: Keyword) -> bool {
    matches!(
        tag,
        Keyword::Article
            | Keyword::Aside
            | Keyword::Content
            | Keyword::Div
            | Keyword::Footer
            | Keyword::Form
            | Keyword::Header
            | Keyword::Main
            | Keyword::Menu
            | Keyword::Nav
            | Keyword::Section
            | Keyword::Table
            | Keyword::Tr
            | Keyword::Ul
    )
}

fn is_ignored(tag: Keyword) -> bool {
    matches!(tag, Keyword::Html | Keyword::Body)
}

/// Tags structural ids onto div-like and classed elements.
#[derive(Debug, Default)]
pub struct AddIdsFilter {
    div_count_stack: Vec<i32>,
    /// Explicit ids seen, as (element, attribute index).
    id_stack: Vec<(NodeId, usize)>,
}

impl AddIdsFilter {
    fn stack_encoding(&self, dom: &Dom) -> Vec<u8> {
        debug_assert!(!self.div_count_stack.is_empty());
        debug_assert_ne!(IS_ID, *self.div_count_stack.last().unwrap());
        let mut result = ID_PREFIX.to_vec();
        if let Some((id, index)) = self.id_stack.last() {
            // A valueless id contributes an empty segment (C++ appends
            // StringPiece(NULL) as "").
            let escaped = dom.element_attributes(*id)[*index]
                .escaped_value()
                .unwrap_or(b"");
            result.push(b'-');
            result.extend_from_slice(escaped);
        }
        // Encode the counts below the nearest explicit id (or all).
        let mut count_index = self.div_count_stack.len() - 1;
        while count_index > 0 && self.div_count_stack[count_index - 1] != IS_ID {
            count_index -= 1;
        }
        for count in &self.div_count_stack[count_index..] {
            result.push(b'-');
            result.extend_from_slice(count.to_string().as_bytes());
        }
        result
    }
}

impl HtmlFilter for AddIdsFilter {
    fn name(&self) -> &'static str {
        "AddIdsFilter"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.div_count_stack.clear();
        self.div_count_stack.push(0);
        self.id_stack.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let tag = dom.element_keyword(id);
        if let Some(index) = dom.find_attribute(id, Keyword::Id) {
            // Explicit ids anchor the stack (attribute indices are stable:
            // this filter only ever appends attributes).
            self.id_stack.push((id, index));
            self.div_count_stack.push(IS_ID);
        } else if is_ignored(tag) {
            return;
        } else if is_div_like(tag) || dom.find_attribute(id, Keyword::Class).is_some() {
            // C++ feeds the encoding (explicit-id bytes included)
            // through AddAttribute, escaping it a second time.
            let encoding = self.stack_encoding(dom);
            dom.add_attribute(id, b"id", &encoding);
        }
        // Every tracked element gets a child counter, explicit ids too.
        self.div_count_stack.push(0);
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        debug_assert!(!self.div_count_stack.is_empty());
        debug_assert_ne!(IS_ID, *self.div_count_stack.last().unwrap());
        // Pointer equality in C++; (element, first-id index) here —
        // equivalent because attributes are only ever appended.
        let matched = matches!(self.id_stack.last(), Some((top_id, top_index))
            if *top_id == id && dom.find_attribute(id, Keyword::Id) == Some(*top_index));
        if matched {
            debug_assert!(self.div_count_stack.len() > 2);
            // ... my_count_in_parent kIsId child_count: drop the child
            // count and the id; the kIsId marker falls below.
            self.div_count_stack.pop();
            self.id_stack.pop();
        } else if is_ignored(dom.element_keyword(id)) {
            return;
        }
        self.div_count_stack.pop();
        if let Some(parent_count) = self.div_count_stack.last_mut() {
            *parent_count += 1;
        }
        debug_assert!(!self.div_count_stack.is_empty());
        debug_assert_ne!(IS_ID, *self.div_count_stack.last().unwrap());
    }
}
