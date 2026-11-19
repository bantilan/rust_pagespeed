//! Head-section repair (`AddHeadFilter`).
//!
//! Adds a `<head>` section when none precedes the body, and optionally
//! merges multiple heads into the first (`combine_multiple_heads`, from
//! `kCombineHeads`).

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Ensures (and optionally unifies) the head section.
#[derive(Debug)]
pub struct AddHeadFilter {
    combine_multiple_heads: bool,
    found_head: bool,
    head: Option<NodeId>,
}

impl AddHeadFilter {
    /// Builds the filter (`combine_multiple_heads` mirrors `kCombineHeads`).
    #[must_use]
    pub fn new(combine_multiple_heads: bool) -> Self {
        AddHeadFilter {
            combine_multiple_heads,
            found_head: false,
            head: None,
        }
    }
}

impl HtmlFilter for AddHeadFilter {
    fn name(&self) -> &'static str {
        "AddHead"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.found_head = false;
        self.head = None;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.found_head {
            return;
        }
        let keyword = dom.element_keyword(id);
        if keyword == Keyword::Head {
            self.found_head = true;
            self.head = Some(id);
        } else if keyword != Keyword::Html {
            // Add head before the first non-head non-html element.
            let parent = dom.arena_node(id).parent();
            let head = dom.new_element(parent, b"head");
            dom.insert_node_before_node(id, head);
            self.head = Some(head);
            self.found_head = true;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !self.combine_multiple_heads || dom.element_keyword(id) != Keyword::Head {
            return;
        }
        if let Some(head) = self.head {
            if id != head && dom.is_rewritable(head) {
                // Move this head's contents into the first head, then drop it.
                let moved = dom.move_current_into(head) && dom.delete_saving_children(id);
                debug_assert!(moved, "failed to move or delete head");
            }
        }
    }

    fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        if !self.found_head {
            // Degenerate case: no elements (or only <html> elements).
            let head = dom.new_element(None, b"head");
            dom.insert_node_before_current(head);
            self.found_head = true;
        }
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // Heads cannot combine across a flush.
        self.head = None;
    }
}
