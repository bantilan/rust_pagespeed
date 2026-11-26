//! Document object model and parse driver (`HtmlParse`).
//!
//! [`Dom`] owns the arena, event queue, cursor, and all mutation operations;
//! [`Parser`] adds the lexer, filters, and the write/flush/finish lifecycle.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::element::{
    Arena, Attribute, ElementData, ElementStyle, LeafData, LeafKind, NodeData, NodeId, QuoteStyle,
};
use crate::event::{Event, EventQueue, SlotId, NO_SLOT};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::{HtmlName, Keyword};

/// Log message collected during parsing (tests assert on these).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub warning: bool,
    pub line: i32,
    pub text: String,
}

/// The mutable document: arena, queue, cursor, deferrals, messages.
pub struct Dom {
    arena: Arena,
    queue: EventQueue,
    current: SlotId,
    skip_increment: bool,
    need_sanity_check: bool,
    need_coalesce: bool,
    coalesce_characters: bool,
    buffer_events: bool,
    running_filters: bool,
    current_filter: Option<usize>,
    deferred_nodes: HashMap<NodeId, Vec<SlotId>>,
    deferred_deleted: HashSet<NodeId>,
    open_deferred: HashMap<usize, NodeId>,
    delayed_start_literal: Option<SlotId>,
    messages: Vec<Message>,
    /// Set by the flush listener to request a flush after this write.
    pub flush_requested: bool,
}

impl Dom {
    #[must_use]
    pub fn new() -> Self {
        Dom {
            arena: Arena::new(),
            queue: EventQueue::new(),
            current: NO_SLOT,
            skip_increment: false,
            need_sanity_check: false,
            need_coalesce: false,
            coalesce_characters: true,
            buffer_events: false,
            running_filters: false,
            current_filter: None,
            deferred_nodes: HashMap::new(),
            deferred_deleted: HashSet::new(),
            open_deferred: HashMap::new(),
            delayed_start_literal: None,
            messages: Vec::new(),
            flush_requested: false,
        }
    }

    // -- Messages ------------------------------------------------------

    pub fn message_info(&mut self, _id: &[u8], line: i32, text: String) {
        self.messages.push(Message {
            warning: false,
            line,
            text,
        });
    }

    pub fn message_warning(&mut self, _id: &[u8], line: i32, text: String) {
        self.messages.push(Message {
            warning: true,
            line,
            text,
        });
    }

    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    // -- Factory -------------------------------------------------------

    /// Creates an element node (`NewElement`, style `AutoClose`).
    pub fn new_element(&mut self, parent: Option<NodeId>, name: &[u8]) -> NodeId {
        self.arena.push(NodeData::Element(ElementData {
            name: HtmlName::new(name),
            attributes: Vec::new(),
            style: ElementStyle::AutoClose,
            parent,
            begin: NO_SLOT,
            end: NO_SLOT,
            begin_line_number: 0,
            end_line_number: 0,
            live: true,
        }))
    }

    /// Creates a leaf node of any kind.
    pub fn new_leaf(
        &mut self,
        parent: Option<NodeId>,
        kind: LeafKind,
        contents: Vec<u8>,
    ) -> NodeId {
        self.arena.push(NodeData::Leaf(LeafData {
            kind,
            contents,
            parent,
            slot: NO_SLOT,
            live: true,
        }))
    }

    // -- Node access ---------------------------------------------------

    #[must_use]
    pub fn arena_node(&self, id: NodeId) -> &NodeData {
        self.arena.get(id)
    }

    pub fn arena_node_mut(&mut self, id: NodeId) -> &mut NodeData {
        self.arena.get_mut(id)
    }

    #[must_use]
    pub fn arena_len(&self) -> usize {
        self.arena.len()
    }

    fn element(&self, id: NodeId) -> &ElementData {
        match self.arena.get(id) {
            NodeData::Element(element) => element,
            NodeData::Leaf(_) => panic!("expected element node"),
        }
    }

    fn element_mut(&mut self, id: NodeId) -> &mut ElementData {
        match self.arena.get_mut(id) {
            NodeData::Element(element) => element,
            NodeData::Leaf(_) => panic!("expected element node"),
        }
    }

    fn leaf_mut(&mut self, id: NodeId) -> &mut LeafData {
        match self.arena.get_mut(id) {
            NodeData::Leaf(leaf) => leaf,
            NodeData::Element(_) => panic!("expected leaf node"),
        }
    }

    #[must_use]
    pub fn element_keyword(&self, id: NodeId) -> Keyword {
        self.element(id).name.keyword()
    }

    /// Original tag spelling borrowed from the DOM, without copying.
    #[must_use]
    pub fn element_name_bytes(&self, id: NodeId) -> &[u8] {
        self.element(id).name.value()
    }

    #[must_use]
    pub fn element_name(&self, id: NodeId) -> Vec<u8> {
        self.element(id).name.value().to_vec()
    }

    #[must_use]
    pub fn element_name_equals(&self, id: NodeId, name: &[u8]) -> bool {
        self.element(id).name.value().eq_ignore_ascii_case(name)
    }

    #[must_use]
    pub fn element_begin_line(&self, id: NodeId) -> i32 {
        self.element(id).begin_line_number
    }

    #[must_use]
    pub fn element_end_line(&self, id: NodeId) -> i32 {
        self.element(id).end_line_number
    }

    pub fn set_element_begin_line(&mut self, id: NodeId, line: i32) {
        self.element_mut(id).begin_line_number = line;
    }

    pub fn set_element_end_line(&mut self, id: NodeId, line: i32) {
        self.element_mut(id).end_line_number = line;
    }

    pub fn set_element_parent(&mut self, id: NodeId, parent: Option<NodeId>) {
        self.element_mut(id).parent = parent;
    }

    /// Renames an element (`set_name`).
    pub fn set_element_name(&mut self, id: NodeId, name: &[u8]) {
        self.element_mut(id).name = HtmlName::new(name);
    }

    pub fn set_element_begin(&mut self, id: NodeId, slot: SlotId) {
        self.element_mut(id).begin = slot;
    }

    pub fn set_element_end(&mut self, id: NodeId, slot: SlotId) {
        self.element_mut(id).end = slot;
    }

    #[must_use]
    pub fn element_style(&self, id: NodeId) -> ElementStyle {
        self.element(id).style
    }

    pub fn set_element_style(&mut self, id: NodeId, style: ElementStyle) {
        self.element_mut(id).style = style;
    }

    #[must_use]
    pub fn element_attributes(&self, id: NodeId) -> &Vec<Attribute> {
        &self.element(id).attributes
    }

    pub fn element_attributes_mut(&mut self, id: NodeId) -> &mut Vec<Attribute> {
        &mut self.element_mut(id).attributes
    }

    pub fn add_escaped_attribute(
        &mut self,
        id: NodeId,
        name: &[u8],
        value: Option<&[u8]>,
        quote: QuoteStyle,
    ) {
        let attribute = Attribute::new(HtmlName::new(name), value.map(<[u8]>::to_vec), quote);
        self.element_mut(id).attributes.push(attribute);
    }

    /// Adds a decoded-value attribute, escaping it with double quotes
    /// (`AddAttribute`).
    pub fn add_attribute(&mut self, id: NodeId, name: &[u8], decoded_value: &[u8]) {
        let mut attribute = Attribute::new(HtmlName::new(name), None, QuoteStyle::DoubleQuote);
        attribute.set_value(decoded_value);
        attribute.set_quote_style(QuoteStyle::DoubleQuote);
        self.element_mut(id).attributes.push(attribute);
    }

    /// Finds the first attribute with this keyword (`FindAttribute`).
    #[must_use]
    pub fn find_attribute(&self, id: NodeId, keyword: Keyword) -> Option<usize> {
        self.element(id)
            .attributes
            .iter()
            .position(|attr| attr.keyword() == keyword)
    }

    /// Finds the first attribute with this exact name (`FindAttribute`;
    /// case-sensitive, as in C++).
    #[must_use]
    pub fn find_attribute_by_name(&self, id: NodeId, name: &[u8]) -> Option<usize> {
        self.element(id)
            .attributes
            .iter()
            .position(|attr| attr.name_str() == name)
    }

    /// Decoded value plus decoding-error flag for one attribute
    /// (`DecodedValueOrNull` + `decoding_error`).
    pub fn decoded_attribute(&mut self, id: NodeId, index: usize) -> (Option<Vec<u8>>, bool) {
        let attribute = &mut self.element_attributes_mut(id)[index];
        let decoded = attribute.decoded_value_or_null().map(<[u8]>::to_vec);
        (decoded, attribute.decoding_error())
    }

    /// Requests a flush after the current write (`RequestFlush`).
    pub fn request_flush(&mut self) {
        self.flush_requested = true;
    }

    /// Renames one attribute in place, keeping its value, quoting, and
    /// position (`SetAttributeName`).
    pub fn set_attribute_name(&mut self, id: NodeId, index: usize, name: &[u8]) {
        use crate::name::HtmlName;
        self.element_mut(id).attributes[index].set_name(HtmlName::new(name));
    }

    /// Deletes the first attribute with this keyword (`DeleteAttribute`).
    pub fn delete_attribute(&mut self, id: NodeId, keyword: Keyword) -> bool {
        let attributes = &mut self.element_mut(id).attributes;
        if let Some(index) = attributes.iter().position(|attr| attr.keyword() == keyword) {
            attributes.remove(index);
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn leaf_contents(&self, id: NodeId) -> Vec<u8> {
        match self.arena.get(id) {
            NodeData::Leaf(leaf) => leaf.contents.clone(),
            NodeData::Element(_) => panic!("expected leaf node"),
        }
    }

    pub fn leaf_contents_mut(&mut self, id: NodeId) -> &mut Vec<u8> {
        &mut self.leaf_mut(id).contents
    }

    // -- Queue ----------------------------------------------------------

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn queue_head(&self) -> SlotId {
        self.queue.head()
    }

    #[must_use]
    pub fn last_slot(&self) -> SlotId {
        self.queue.tail()
    }

    #[must_use]
    pub fn queue_event(&self, slot: SlotId) -> (Event, i32) {
        let (event, line) = self.queue.get(slot);
        (event, line as i32)
    }

    #[must_use]
    pub fn queue_next(&self, slot: SlotId) -> SlotId {
        self.queue.next(slot)
    }

    #[must_use]
    pub fn queue_snapshot(&self) -> Vec<Event> {
        self.queue.snapshot()
    }

    /// Appends an event (`AddEvent`); leaf nodes learn their slot.
    pub fn add_event(&mut self, event: Event, line: i32) -> SlotId {
        let slot = self.queue.push_back(event, line as u32);
        if let Event::Leaf(id) = event {
            self.leaf_mut(id).slot = slot;
        }
        self.need_sanity_check = true;
        self.need_coalesce = true;
        slot
    }

    /// Closes an element, re-inserting a delayed literal start tag first
    /// (`CloseElement`). Returns the end-event slot.
    pub fn close_element(&mut self, id: NodeId, style: ElementStyle, line: i32) -> SlotId {
        if let Some(held) = self.delayed_start_literal.take() {
            // Re-insert the held literal start before the trailing
            // characters block (normally at the queue front here).
            let tail = self.queue.tail();
            let mut insert_at_begin = true;
            if tail != NO_SLOT {
                let trailing_characters = match self.queue.get(tail).0 {
                    Event::Leaf(id) => matches!(
                        self.arena.get(id),
                        crate::element::NodeData::Leaf(leaf)
                            if leaf.kind == crate::element::LeafKind::Characters
                    ),
                    _ => false,
                };
                if trailing_characters && tail != self.queue.head() {
                    let before = self.queue.prev(tail);
                    self.queue.link_before(held, before);
                    insert_at_begin = false;
                }
            }
            if insert_at_begin {
                // link_before(head) == push_front.
                let head = self.queue.head();
                self.queue.link_before(held, head);
            }
            if let Event::StartElement(held_id) = self.queue.get(held).0 {
                self.set_element_begin(held_id, held);
            }
        }
        if self.element_style(id) != ElementStyle::Invisible {
            self.set_element_style(id, style);
        }
        let slot = self.add_event(Event::EndElement(id), line);
        self.set_element_end(id, slot);
        self.set_element_end_line(id, line);
        slot
    }

    // -- Rewritability ---------------------------------------------------

    #[must_use]
    pub fn is_in_window(&self, slot: SlotId) -> bool {
        slot != NO_SLOT
    }

    #[must_use]
    pub fn is_rewritable_ignoring_deferral(&self, id: NodeId) -> bool {
        let node = self.arena.get(id);
        node.live() && self.is_in_window(node.begin()) && self.is_in_window(node.end())
    }

    #[must_use]
    pub fn is_rewritable_ignoring_end(&self, id: NodeId) -> bool {
        let node = self.arena.get(id);
        node.live() && !self.deferred_nodes.contains_key(&id) && self.is_in_window(node.begin())
    }

    #[must_use]
    pub fn is_rewritable(&self, id: NodeId) -> bool {
        self.is_rewritable_ignoring_deferral(id) && !self.deferred_nodes.contains_key(&id)
    }

    #[must_use]
    pub fn can_append_child(&self, id: NodeId) -> bool {
        let node = self.arena.get(id);
        node.live() && !self.deferred_nodes.contains_key(&id) && self.is_in_window(node.end())
    }

    // -- Cursor ----------------------------------------------------------

    #[must_use]
    pub fn current(&self) -> SlotId {
        self.current
    }

    pub fn set_current(&mut self, slot: SlotId) {
        self.current = slot;
    }

    pub fn next_event(&mut self) {
        if self.skip_increment {
            self.skip_increment = false;
        } else {
            self.current = self.queue.next(self.current);
        }
    }

    // -- Structural mutation ----------------------------------------------

    /// Synthesizes queue events for a fresh node before `at`.
    fn synthesize_events(&mut self, id: NodeId, at: SlotId, line: i32) {
        match self.arena.get(id) {
            NodeData::Element(_) => {
                let begin = self.queue_insert_before(at, Event::StartElement(id), line);
                self.set_element_begin(id, begin);
                let end = self.queue_insert_before(at, Event::EndElement(id), line);
                self.set_element_end(id, end);
            }
            NodeData::Leaf(_) => {
                let slot = self.queue_insert_before(at, Event::Leaf(id), line);
                self.leaf_mut(id).slot = slot;
            }
        }
    }

    fn queue_insert_before(&mut self, at: SlotId, event: Event, line: i32) -> SlotId {
        let slot = self.queue.insert_before(at, event, line as u32);
        self.need_sanity_check = true;
        self.need_coalesce = true;
        slot
    }

    pub fn insert_node_before_event(&mut self, at: SlotId, id: NodeId) {
        let line = if at == NO_SLOT {
            0
        } else {
            self.queue_event(at).1
        };
        self.synthesize_events(id, at, line);
    }

    pub fn insert_node_after_event(&mut self, at: SlotId, id: NodeId) {
        let next = self.queue.next(at);
        self.insert_node_before_event(next, id);
    }

    pub fn insert_node_before_node(&mut self, existing: NodeId, id: NodeId) {
        let begin = self.arena.get(existing).begin();
        let parent = self.arena.get(existing).parent();
        self.arena.get_mut(id).set_parent(parent);
        self.insert_node_before_event(begin, id);
    }

    pub fn insert_node_after_node(&mut self, existing: NodeId, id: NodeId) {
        let end = self.arena.get(existing).end();
        let parent = self.arena.get(existing).parent();
        self.arena.get_mut(id).set_parent(parent);
        self.insert_node_after_event(end, id);
    }

    pub fn prepend_child(&mut self, parent: NodeId, id: NodeId) {
        let begin = self.arena.get(parent).begin();
        self.arena.get_mut(id).set_parent(Some(parent));
        let next = self.queue.next(begin);
        self.insert_node_before_event(next, id);
    }

    pub fn append_child(&mut self, parent: Option<NodeId>, id: NodeId) {
        match parent {
            Some(parent) => {
                let end = self.arena.get(parent).end();
                self.arena.get_mut(id).set_parent(Some(parent));
                self.insert_node_before_event(end, id);
            }
            None => {
                self.insert_node_before_event(NO_SLOT, id);
            }
        }
    }

    /// Inserts before the current event, inferring the parent from context
    /// (`InsertNodeBeforeCurrent`).
    pub fn insert_node_before_current(&mut self, id: NodeId) {
        if self.arena.get(id).parent().is_none() && self.current != NO_SLOT {
            let (event, _) = self.queue.get(self.current);
            match event {
                Event::EndElement(parent) => {
                    self.arena.get_mut(id).set_parent(Some(parent));
                }
                Event::StartElement(node) | Event::Leaf(node) => {
                    let parent = self.arena.get(node).parent();
                    self.arena.get_mut(id).set_parent(parent);
                }
                Event::StartDocument | Event::EndDocument => {}
            }
        }
        let current = self.current;
        self.insert_node_before_event(current, id);
    }

    /// Inserts after the current event, leaving the cursor on the new
    /// node's last event (`InsertNodeAfterCurrent`): the end event for
    /// elements (so back-to-back insertions serialize as siblings), the
    /// lone event for leaves.
    pub fn insert_node_after_current(&mut self, id: NodeId) {
        if self.arena.get(id).parent().is_none() && self.current != NO_SLOT {
            let (event, _) = self.queue.get(self.current);
            let parent = match event {
                Event::EndElement(node) => self.arena.get(node).parent(),
                Event::StartElement(node) => Some(node),
                Event::Leaf(node) => self.arena.get(node).parent(),
                Event::StartDocument | Event::EndDocument => None,
            };
            self.arena.get_mut(id).set_parent(parent);
        }
        let next = self.queue.next(self.current);
        self.insert_node_before_event(next, id);
        self.current = self.arena.get(id).end();
    }

    /// Inserts a script element before the current event
    /// (`InsertScriptBeforeCurrent`): external scripts get a `src`
    /// attribute, inline ones a text child.
    pub fn insert_script_before_current(&mut self, text: &[u8], external: bool) {
        let script = self.new_element(None, b"script");
        self.insert_node_before_current(script);
        self.setup_script(text, external, script);
    }

    /// Inserts a script element after the current event
    /// (`InsertScriptAfterCurrent`).
    pub fn insert_script_after_current(&mut self, text: &[u8], external: bool) {
        let script = self.new_element(None, b"script");
        self.insert_node_after_current(script);
        self.setup_script(text, external, script);
    }

    fn setup_script(&mut self, text: &[u8], external: bool, script: NodeId) {
        if external {
            self.add_attribute(script, b"src", text);
        } else {
            let child = self.new_leaf(
                Some(script),
                crate::element::LeafKind::Characters,
                text.to_vec(),
            );
            self.append_child(Some(script), child);
        }
    }

    /// Wraps `[first, last]` in a fresh parent (`AddParentToSequence`).
    pub fn add_parent_to_sequence(&mut self, first: NodeId, last: NodeId, parent: NodeId) -> bool {
        let original_parent = self.arena.get(first).parent();
        let fresh =
            self.arena.get(parent).begin() == NO_SLOT && self.arena.get(parent).end() == NO_SLOT;
        if !self.is_rewritable(first)
            || !self.is_rewritable(last)
            || self.arena.get(last).parent() != original_parent
            || !fresh
        {
            return false;
        }
        let first_begin = self.arena.get(first).begin();
        self.insert_node_before_event(first_begin, parent);
        // Move the parent's end event to just after last->end().
        let parent_end = self.arena.get(parent).end();
        let after_last = self.queue.next(self.arena.get(last).end());
        self.queue.detach(parent_end);
        self.queue.link_before(parent_end, after_last);
        let last_end = self.arena.get(last).end();
        self.fix_parents(first_begin, last_end, Some(parent));
        self.need_sanity_check = true;
        self.need_coalesce = true;
        true
    }

    /// Reparents nodes in `[begin, end_inclusive]` (`FixParents`).
    pub fn fix_parents(&mut self, begin: SlotId, end_inclusive: SlotId, parent: Option<NodeId>) {
        let first_node = match self.queue.get(begin).0 {
            Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => Some(id),
            Event::StartDocument | Event::EndDocument => None,
        };
        let Some(first) = first_node else { return };
        let original_parent = self.arena.get(first).parent();
        let mut slot = begin;
        loop {
            let node = match self.queue.get(slot).0 {
                Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => Some(id),
                Event::StartDocument | Event::EndDocument => None,
            };
            if let Some(node) = node {
                if self.arena.get(node).parent() == original_parent {
                    self.arena.get_mut(node).set_parent(parent);
                }
            }
            if slot == end_inclusive {
                break;
            }
            slot = self.queue.next(slot);
        }
    }

    /// Deletes a node and its whole subtree (`DeleteNode`).
    pub fn delete_node(&mut self, id: NodeId) -> bool {
        if self.is_rewritable(id) {
            let mut slot = self.arena.get(id).begin();
            let end = self.arena.get(id).end();
            loop {
                let done = slot == end;
                let (event, _) = self.queue.get(slot);
                if !self.skip_increment && slot == self.current {
                    self.skip_increment = true;
                    self.current = self.queue.next(end);
                }
                let next = self.queue.erase(slot);
                match event {
                    Event::EndElement(nested) => self.mark_dead(nested),
                    Event::Leaf(nested) => self.mark_dead(nested),
                    Event::StartElement(_) | Event::StartDocument | Event::EndDocument => {}
                }
                if done {
                    break;
                }
                slot = next;
            }
            self.need_sanity_check = true;
            self.need_coalesce = true;
            true
        } else if self.is_rewritable_ignoring_end(id) && self.current != NO_SLOT {
            // The end is not in this window but current_ is on this node's
            // start (or the node itself, if a leaf): defer and drop it.
            let (event, _) = self.queue.get(self.current);
            let node = match event {
                Event::StartElement(node) | Event::Leaf(node) => Some(node),
                _ => None,
            };
            if node == Some(id) {
                self.defer_current_node(self.current_filter);
                self.deferred_deleted.insert(id);
                return true;
            }
            false
        } else {
            false
        }
    }

    fn mark_dead(&mut self, id: NodeId) {
        match self.arena.get_mut(id) {
            NodeData::Element(element) => {
                element.live = false;
                element.begin = NO_SLOT;
                element.end = NO_SLOT;
            }
            NodeData::Leaf(leaf) => {
                leaf.live = false;
                leaf.slot = NO_SLOT;
            }
        }
    }

    /// Deletes an element but keeps its children (`DeleteSavingChildren`).
    pub fn delete_saving_children(&mut self, id: NodeId) -> bool {
        if !self.is_rewritable(id) {
            return false;
        }
        let parent = self.arena.get(id).parent();
        let first = self.queue.next(self.arena.get(id).begin());
        let last = self.arena.get(id).end();
        if first != last {
            let inclusive = self.queue.prev(last);
            self.fix_parents(first, inclusive, parent);
            // Move children out: after the end event when deleting from the
            // start tag (so they still get visited), else before the begin.
            let at_end = self.current == NO_SLOT;
            let from_start = !at_end
                && matches!(self.queue.get(self.current).0, Event::StartElement(node) if node == id);
            if from_start {
                let after_end = self.queue.next(last);
                self.queue.splice(after_end, first, last);
            } else {
                let begin = self.arena.get(id).begin();
                self.queue.splice(begin, first, last);
            }
            self.need_sanity_check = true;
            self.need_coalesce = true;
        }
        self.delete_node(id)
    }

    /// Hides an element from serialization (`MakeElementInvisible`).
    pub fn make_element_invisible(&mut self, id: NodeId) -> bool {
        if self.is_rewritable_ignoring_end(id) {
            self.set_element_style(id, ElementStyle::Invisible);
            true
        } else {
            false
        }
    }

    /// True when the element has children in this window.
    #[must_use]
    pub fn has_children_in_window(&self, id: NodeId) -> bool {
        if !self.is_rewritable(id) {
            return false;
        }
        let begin = self.arena.get(id).begin();
        if begin == NO_SLOT {
            return false;
        }
        self.queue.next(begin) != self.arena.get(id).end()
    }

    /// Replaces one node with another (`ReplaceNode`).
    pub fn replace_node(&mut self, existing: NodeId, id: NodeId) -> bool {
        if !self.is_rewritable(existing) {
            return false;
        }
        self.insert_node_before_node(existing, id);
        self.delete_node(existing)
    }

    /// Clones an element's tag, style, and attributes (`CloneElement`).
    pub fn clone_element(&mut self, id: NodeId) -> NodeId {
        let element = self.element(id);
        let name = HtmlName::new(&element.name.value().to_vec());
        let style = element.style;
        let attributes = element.attributes.clone();
        let fresh = self.new_element(None, &name.value().to_vec());
        self.set_element_style(fresh, style);
        *self.element_attributes_mut(fresh) = attributes;
        fresh
    }

    // -- Deferral --------------------------------------------------------

    /// Removes the current node from the queue into a holding list
    /// (`DeferCurrentNode`), tracking cross-window elements per filter.
    pub fn defer_current_node(&mut self, filter: Option<usize>) {
        let (event, _) = self.queue.get(self.current);
        let node = match event {
            Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => id,
            Event::StartDocument | Event::EndDocument => return,
        };
        let begin = self.arena.get(node).begin();
        let end = self.arena.get(node).end();
        let mut held = Vec::new();
        if end != NO_SLOT {
            // Case 1: the whole node sits in this window.
            let mut slot = begin;
            loop {
                let next = self.queue.detach(slot);
                held.push(slot);
                if slot == end {
                    break;
                }
                slot = next;
            }
            self.current = self.queue.next(end);
        } else {
            // Case 2: the end has not been lexed yet; hold to queue end.
            let mut slot = begin;
            while slot != NO_SLOT {
                let next = self.queue.detach(slot);
                held.push(slot);
                slot = next;
            }
            self.current = NO_SLOT;
            if let Some(filter) = filter {
                self.open_deferred.insert(filter, node);
            }
        }
        self.skip_increment = true;
        self.deferred_nodes.insert(node, held);
        self.need_sanity_check = true;
        self.need_coalesce = true;
    }

    /// Restores a deferred node before the cursor (`RestoreDeferredNode`).
    pub fn restore_deferred_node(&mut self, id: NodeId) {
        if !self.is_rewritable_ignoring_deferral(id) {
            return;
        }
        let Some(held) = self.deferred_nodes.remove(&id) else {
            return;
        };
        // Recompute the top node's parent from the cursor position.
        if self.current != NO_SLOT {
            let (event, _) = self.queue.get(self.current);
            let parent = match event {
                Event::StartElement(node) => Some(node),
                Event::EndElement(node) | Event::Leaf(node) => self.arena.get(node).parent(),
                Event::StartDocument | Event::EndDocument => None,
            };
            self.arena.get_mut(id).set_parent(parent);
            self.next_event();
            let at = self.current;
            for slot in held {
                self.queue.link_before(slot, at);
            }
            self.current = self.arena.get(id).begin();
        } else {
            for slot in &held {
                self.queue.link_before(*slot, NO_SLOT);
            }
        }
        self.need_sanity_check = true;
        self.need_coalesce = true;
    }

    /// Drops all deferred nodes, warning about never-replaced ones.
    pub fn clear_deferred_nodes(&mut self) {
        let ids: Vec<NodeId> = self.deferred_nodes.keys().copied().collect();
        for id in ids {
            if !self.deferred_deleted.contains(&id) {
                self.messages.push(Message {
                    warning: true,
                    line: 0,
                    text: "Removed node never replaced".to_string(),
                });
            }
        }
        self.deferred_nodes.clear();
        self.deferred_deleted.clear();
        self.open_deferred.clear();
    }

    // -- Coalescing and sanity --------------------------------------------

    /// Merges adjacent characters nodes (`CoalesceAdjacentCharactersNodes`).
    pub fn coalesce_characters(&mut self) {
        use crate::element::{LeafKind, NodeData};
        let mut prev: Option<NodeId> = None;
        let mut slot = self.queue.head();
        while slot != NO_SLOT {
            let next = self.queue.next(slot);
            let node = match self.queue.get(slot).0 {
                Event::Leaf(id) => match self.arena.get(id) {
                    NodeData::Leaf(leaf) if leaf.kind == LeafKind::Characters => Some(id),
                    _ => None,
                },
                _ => None,
            };
            match (node, prev) {
                (Some(node), Some(prev)) => {
                    let contents = self.leaf_contents(node);
                    self.leaf_contents_mut(prev).extend_from_slice(&contents);
                    self.queue.erase(slot);
                    self.mark_dead(node);
                    self.need_sanity_check = true;
                }
                (node, _) => {
                    prev = node;
                }
            }
            slot = next;
        }
    }

    /// Holds a trailing literal start tag for the next window (`DelayLiteralTag`).
    pub fn delay_literal_tag(&mut self, is_literal: impl Fn(Keyword) -> bool) {
        let tail = self.queue.tail();
        if tail == NO_SLOT {
            return;
        }
        if let Event::StartElement(id) = self.queue.get(tail).0 {
            if is_literal(self.element_keyword(id)) {
                self.queue.detach(tail);
                self.delayed_start_literal = Some(tail);
            }
        }
        self.current = NO_SLOT;
    }

    /// Moves the current node into a new parent (`MoveCurrentInto`).
    pub fn move_current_into(&mut self, parent: NodeId) -> bool {
        if self.current == NO_SLOT || !self.arena.get(parent).live() {
            return false;
        }
        let node = match self.queue.get(self.current).0 {
            Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => id,
            Event::StartDocument | Event::EndDocument => return false,
        };
        let target = self.arena.get(parent).end();
        if self.move_current_before_slot(target) {
            self.arena.get_mut(node).set_parent(Some(parent));
            true
        } else {
            false
        }
    }

    /// Moves the current node before another node (`MoveCurrentBefore`).
    pub fn move_current_before(&mut self, sibling: NodeId) -> bool {
        if self.current == NO_SLOT || !self.arena.get(sibling).live() {
            return false;
        }
        let node = match self.queue.get(self.current).0 {
            Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => id,
            Event::StartDocument | Event::EndDocument => return false,
        };
        let target = self.arena.get(sibling).begin();
        if self.move_current_before_slot(target) {
            let parent = self.arena.get(sibling).parent();
            self.arena.get_mut(node).set_parent(parent);
            true
        } else {
            false
        }
    }

    /// Splices the current node's events before a slot, repositioning the
    /// cursor past its original location (`MoveCurrentBeforeEvent`). Only
    /// valid when the cursor sits on the node's end event.
    fn move_current_before_slot(&mut self, target: SlotId) -> bool {
        if target == NO_SLOT || self.current == NO_SLOT {
            return false;
        }
        let node = match self.queue.get(self.current).0 {
            Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => id,
            Event::StartDocument | Event::EndDocument => return false,
        };
        let begin = self.arena.get(node).begin();
        let end = self.arena.get(node).end();
        if self.current != end
            || begin == NO_SLOT
            || end == NO_SLOT
            || self.is_descendant_of_slot(target, node)
        {
            return false;
        }
        let after_end = self.queue.next(end);
        self.queue.splice(target, begin, after_end);
        // Land on the event before after_end, so iteration continues just
        // past the node's original position (not over the moved range).
        self.current = after_end;
        if after_end != NO_SLOT {
            self.current = self.queue.prev(after_end);
        }
        self.need_sanity_check = true;
        self.need_coalesce = true;
        true
    }

    /// True when the node at `slot` descends from `ancestor` (or is it).
    fn is_descendant_of_slot(&self, slot: SlotId, ancestor: NodeId) -> bool {
        let node = match self.queue.get(slot).0 {
            Event::StartElement(id) | Event::EndElement(id) | Event::Leaf(id) => Some(id),
            Event::StartDocument | Event::EndDocument => None,
        };
        let mut node = node;
        while let Some(id) = node {
            if id == ancestor {
                return true;
            }
            node = self.arena.get(id).parent();
        }
        false
    }

    /// Verifies parent pointers against the event stream (`SanityCheck`).
    /// Returns the first inconsistency found, if any.
    #[must_use]
    pub fn sanity_check(&mut self) -> Option<String> {
        let mut stack: Vec<NodeId> = Vec::new();
        let mut expect_parent: Option<NodeId> = None;
        let mut slot = self.queue.head();
        while slot != NO_SLOT {
            let event = self.queue.get(slot).0;
            match event {
                Event::StartElement(id) => {
                    if self.arena.get(id).parent() != expect_parent && expect_parent.is_some() {
                        return Some(format!("start parent mismatch at {slot}"));
                    }
                    if self.arena.get(id).begin() != slot {
                        return Some(format!("start begin mismatch at {slot}"));
                    }
                    if !self.arena.get(id).live() {
                        return Some(format!("dead start node at {slot}"));
                    }
                    stack.push(id);
                    expect_parent = Some(id);
                }
                Event::EndElement(id) => {
                    if self.arena.get(id).end() != slot {
                        return Some(format!("end mismatch at {slot}"));
                    }
                    if !self.arena.get(id).live() {
                        return Some(format!("dead end node at {slot}"));
                    }
                    if let Some(top) = stack.pop() {
                        if top != id {
                            return Some(format!("stack mismatch at {slot}"));
                        }
                    }
                    expect_parent = stack.last().copied();
                    if self.arena.get(id).parent() != expect_parent && expect_parent.is_some() {
                        return Some(format!("end parent mismatch at {slot}"));
                    }
                }
                Event::Leaf(id) => {
                    if !self.arena.get(id).live() {
                        return Some(format!("dead leaf at {slot}"));
                    }
                    if self.arena.get(id).end() != slot {
                        return Some(format!("leaf slot mismatch at {slot}"));
                    }
                    if self.arena.get(id).parent() != expect_parent && expect_parent.is_some() {
                        return Some(format!("leaf parent mismatch at {slot}"));
                    }
                }
                Event::StartDocument | Event::EndDocument => {}
            }
            slot = self.queue.next(slot);
        }
        self.need_sanity_check = false;
        None
    }

    /// Drops all queued events, detaching nodes (`ClearEvents`).
    pub fn clear_events(&mut self) {
        let mut slot = self.queue.head();
        while slot != NO_SLOT {
            let next = self.queue.next(slot);
            match self.queue.get(slot).0 {
                Event::StartElement(id) => {
                    self.set_element_begin(id, NO_SLOT);
                }
                Event::EndElement(id) => {
                    self.set_element_end(id, NO_SLOT);
                    self.element_mut(id).attributes.clear();
                }
                Event::Leaf(id) => {
                    self.leaf_mut(id).slot = NO_SLOT;
                    self.leaf_mut(id).contents.clear();
                }
                Event::StartDocument | Event::EndDocument => {}
            }
            self.queue.erase(slot);
            slot = next;
        }
        self.need_sanity_check = false;
        self.need_coalesce = false;
    }

    /// Inserts an escaped comment at a safe position (`InsertComment`).
    pub fn insert_comment(&mut self, unescaped: &[u8], lexer_parent: Option<NodeId>) -> bool {
        let mut escaped = Vec::new();
        crate::keywords::escape(unescaped, &mut escaped);
        if self.queue.head() == NO_SLOT {
            if let Some(parent) = lexer_parent {
                if let crate::element::NodeData::Element(element) = self.arena.get(parent) {
                    if crate::keywords::is_literal_tag(element.name.keyword()) {
                        return false;
                    }
                }
            }
            let id = self.new_leaf(lexer_parent, LeafKind::Comment, escaped);
            let slot = self.queue.push_back(Event::Leaf(id), 0);
            self.leaf_mut(id).slot = slot;
            return true;
        }
        let mut pos = self.current;
        if pos == NO_SLOT {
            pos = self.queue.tail();
        }
        match self.queue.get(pos).0 {
            Event::StartElement(id) => {
                let parent = self.arena.get(id).parent();
                let comment = self.new_leaf(parent, LeafKind::Comment, escaped);
                self.insert_node_before_event(pos, comment);
            }
            Event::EndElement(id) => {
                let parent = self.arena.get(id).parent();
                let comment = self.new_leaf(parent, LeafKind::Comment, escaped);
                self.insert_node_after_event(pos, comment);
            }
            _ => {
                let parent = match self.queue.get(pos).0 {
                    Event::Leaf(id) => self.arena.get(id).parent(),
                    _ => None,
                };
                let comment = self.new_leaf(parent, LeafKind::Comment, escaped);
                if self.current == NO_SLOT {
                    self.insert_node_after_event(pos, comment);
                } else {
                    self.insert_node_before_event(pos, comment);
                }
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Driver: lexer + filters + write/flush/finish lifecycle.
// ---------------------------------------------------------------------------

/// A registered filter with its cached enablement.
pub struct FilterRecord {
    pub filter: Box<dyn HtmlFilter>,
    pub enabled: bool,
    determined: bool,
    key: usize,
}

/// Filter phase within one flush (`FlushAsync` runs early-pre, then pre,
/// then — after rewrite initiation — the post-render parse chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterPhase {
    EarlyPre,
    Pre,
    Post,
}

/// Streaming HTML parser: bytes in, filtered events serialized by the
/// writer filter. Instances are reusable across documents.
pub struct Parser {
    lexer: Lexer,
    dom: Dom,
    early_pre: Vec<FilterRecord>,
    pre: Vec<FilterRecord>,
    filters: Vec<FilterRecord>,
    listeners: Vec<Box<dyn HtmlFilter>>,
    next_filter_key: usize,
    url: Vec<u8>,
    finished: bool,
    disabled_list: Option<Rc<RefCell<Vec<String>>>>,
}

impl Parser {
    #[must_use]
    pub fn new() -> Self {
        Parser {
            lexer: Lexer::new(),
            dom: Dom::new(),
            early_pre: Vec::new(),
            pre: Vec::new(),
            filters: Vec::new(),
            listeners: Vec::new(),
            next_filter_key: 0,
            url: Vec::new(),
            finished: false,
            disabled_list: None,
        }
    }

    /// Installs the dynamically-disabled filter list
    /// (`SetDynamicallyDisabledFilterList`): the debug filter reads the
    /// same cell the parser fills during enablement resolution.
    pub fn set_dynamically_disabled_list(&mut self, list: Rc<RefCell<Vec<String>>>) {
        self.disabled_list = Some(list);
    }

    /// Starts a document (`StartParse`). `content_is_xml_like` stands in
    /// for the content-type check until that layer lands.
    pub fn start_parse(&mut self, url: &[u8], content_is_xml_like: bool) {
        self.url = url.to_vec();
        self.finished = false;
        self.dom = Dom::new();
        for record in self
            .early_pre
            .iter_mut()
            .chain(self.pre.iter_mut())
            .chain(self.filters.iter_mut())
        {
            record.filter.init_parse();
            record.determined = false;
        }
        if let Some(list) = &self.disabled_list {
            list.borrow_mut().clear();
        }
        self.lexer.start_parse(url, content_is_xml_like);
        self.dom.add_event(Event::StartDocument, 1);
        let (dom, lexer, listeners) = self.parts_for_listeners();
        for listener in listeners.iter_mut() {
            listener.start_document(dom, lexer);
        }
    }

    fn parts_for_listeners(&mut self) -> (&mut Dom, &Lexer, &mut [Box<dyn HtmlFilter>]) {
        (&mut self.dom, &self.lexer, &mut self.listeners)
    }

    /// Feeds bytes (`ParseText` / `Write`). May be called repeatedly;
    /// bytes stream through the lexer incrementally.
    pub fn write(&mut self, bytes: &[u8]) {
        self.for_each_filter(|filter| filter.start_parse());
        let (dom, listeners, lexer) = self.parts_for_lex();
        lexer.parse(dom, listeners, bytes);
        self.for_each_filter(|filter| filter.end_parse());
    }

    /// Runs a phase hook over every chain filter in registration order.
    fn for_each_filter(&mut self, hook: impl Fn(&mut dyn HtmlFilter)) {
        for record in self
            .early_pre
            .iter_mut()
            .chain(self.pre.iter_mut())
            .chain(self.filters.iter_mut())
        {
            hook(record.filter.as_mut());
        }
    }

    fn parts_for_lex(&mut self) -> (&mut Dom, &mut [Box<dyn HtmlFilter>], &mut Lexer) {
        (&mut self.dom, &mut self.listeners, &mut self.lexer)
    }

    /// Runs all enabled filters over the queued window, then clears it
    /// (`FlushAsync`: early-pre, pre, render-done, then the post chain;
    /// only the post chain waits out event buffering).
    pub fn flush(&mut self) {
        if self.dom.running_filters {
            return;
        }
        self.for_each_filter(|filter| filter.start_render());
        self.determine_behavior();
        for listener in &mut self.listeners {
            let (dom, lexer) = (&mut self.dom, &self.lexer);
            listener.flush(dom, lexer);
        }
        self.apply_phase(FilterPhase::EarlyPre);
        self.apply_phase(FilterPhase::Pre);
        // Rewrite initiation would run here (async contexts land with the
        // resource pipelines); render-done still precedes the post chain.
        self.render_done_phase(FilterPhase::EarlyPre);
        self.render_done_phase(FilterPhase::Pre);
        if !self.dom.buffer_events {
            self.apply_phase(FilterPhase::Post);
            self.dom.clear_events();
        }
        if self.dom.flush_requested {
            self.dom.flush_requested = false;
        }
    }

    /// Runs one phase's enabled filters in registration order.
    fn apply_phase(&mut self, phase: FilterPhase) {
        let count = self.phase_list(phase).len();
        for position in 0..count {
            if self.phase_list(phase)[position].enabled {
                self.apply_filter(phase, position);
            }
        }
    }

    /// Notifies one phase's enabled filters that rendering is done.
    fn render_done_phase(&mut self, phase: FilterPhase) {
        let count = self.phase_list(phase).len();
        for position in 0..count {
            if self.phase_list(phase)[position].enabled {
                let (dom, lexer) = (&mut self.dom, &self.lexer);
                let filter = match phase {
                    FilterPhase::EarlyPre => &mut self.early_pre[position].filter,
                    FilterPhase::Pre => &mut self.pre[position].filter,
                    FilterPhase::Post => &mut self.filters[position].filter,
                };
                filter.render_done(dom, lexer);
            }
        }
    }

    fn phase_list(&self, phase: FilterPhase) -> &[FilterRecord] {
        match phase {
            FilterPhase::EarlyPre => &self.early_pre,
            FilterPhase::Pre => &self.pre,
            FilterPhase::Post => &self.filters,
        }
    }

    /// Ends input: closes open elements, emits the end event, and flushes.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        {
            let (dom, listeners, lexer) = self.parts_for_lex();
            lexer.finish_parse(dom, listeners);
        }
        let slot = self.dom.add_event(Event::EndDocument, 0);
        {
            let (dom, lexer, listeners) = self.parts_for_listeners();
            for listener in listeners.iter_mut() {
                listener.end_document(dom, lexer);
            }
            let _ = slot;
        }
        self.flush();
    }

    /// Registers a post-render chain filter (runs in registration
    /// order per flush, after the pre phases).
    pub fn add_filter(&mut self, filter: Box<dyn HtmlFilter>) {
        self.push_filter(FilterPhase::Post, filter);
    }

    /// Registers an early pre-render filter.
    pub fn add_early_pre_render_filter(&mut self, filter: Box<dyn HtmlFilter>) {
        self.push_filter(FilterPhase::EarlyPre, filter);
    }

    /// Registers a pre-render filter.
    pub fn add_pre_render_filter(&mut self, filter: Box<dyn HtmlFilter>) {
        self.push_filter(FilterPhase::Pre, filter);
    }

    fn push_filter(&mut self, phase: FilterPhase, filter: Box<dyn HtmlFilter>) {
        let key = self.next_filter_key;
        self.next_filter_key += 1;
        let list = match phase {
            FilterPhase::EarlyPre => &mut self.early_pre,
            FilterPhase::Pre => &mut self.pre,
            FilterPhase::Post => &mut self.filters,
        };
        list.push(FilterRecord {
            filter,
            enabled: true,
            determined: false,
            key,
        });
    }

    /// Registers an event listener (sees every event as lexed).
    pub fn add_event_listener(&mut self, listener: Box<dyn HtmlFilter>) {
        self.listeners.push(listener);
    }

    /// Runs one filter over the current window (`ApplyFilter`).
    fn apply_filter(&mut self, phase: FilterPhase, position: usize) {
        let key = self.phase_list(phase)[position].key;
        // Route open-deferred spillover for case-2 deferrals first.
        if let Some(node) = self.dom.open_deferred.remove(&key) {
            let end = self.dom.arena_node(node).end();
            // Collect this window's events up to (and including) the end
            // event when closed, or the whole window when still open.
            let stop = if end == NO_SLOT {
                NO_SLOT
            } else {
                self.dom.queue_next(end)
            };
            let mut moved = Vec::new();
            let mut slot = self.dom.queue_head();
            while slot != stop && slot != NO_SLOT {
                let next = self.dom.queue_next(slot);
                moved.push(slot);
                slot = next;
            }
            let mut held = self.dom.deferred_nodes.remove(&node).unwrap_or_default();
            for slot in moved {
                self.dom.queue.detach(slot);
                held.push(slot);
            }
            self.dom.deferred_nodes.insert(node, held);
            if end == NO_SLOT {
                self.dom.open_deferred.insert(key, node);
            }
        }

        if self.dom.coalesce_characters && self.dom.need_coalesce {
            self.dom.coalesce_characters();
            self.dom.delay_literal_tag(crate::keywords::is_literal_tag);
            self.dom.need_coalesce = false;
        }
        self.dom.current_filter = Some(key);
        self.dom.current = self.dom.queue_head();
        self.dom.running_filters = true;
        loop {
            let current = self.dom.current;
            if current == NO_SLOT {
                break;
            }
            let (event, _) = self.dom.queue_event(current);
            let (dom, lexer) = (&mut self.dom, &self.lexer);
            let filter = match phase {
                FilterPhase::EarlyPre => &mut self.early_pre[position].filter,
                FilterPhase::Pre => &mut self.pre[position].filter,
                FilterPhase::Post => &mut self.filters[position].filter,
            };
            match event {
                Event::StartDocument => filter.start_document(dom, lexer),
                Event::EndDocument => filter.end_document(dom, lexer),
                Event::StartElement(id) => filter.start_element(dom, lexer, id),
                Event::EndElement(id) => filter.end_element(dom, lexer, id),
                Event::Leaf(id) => match dom.arena_node(id) {
                    NodeData::Leaf(leaf) => match leaf.kind {
                        crate::element::LeafKind::Characters => filter.characters(dom, lexer, id),
                        crate::element::LeafKind::Comment => filter.comment(dom, lexer, id),
                        crate::element::LeafKind::Cdata => filter.cdata(dom, lexer, id),
                        crate::element::LeafKind::IEDirective => {
                            filter.ie_directive(dom, lexer, id)
                        }
                        crate::element::LeafKind::Directive => filter.directive(dom, lexer, id),
                    },
                    NodeData::Element(_) => {}
                },
            }
            self.dom.next_event();
        }
        {
            let (dom, lexer) = (&mut self.dom, &self.lexer);
            let filter = match phase {
                FilterPhase::EarlyPre => &mut self.early_pre[position].filter,
                FilterPhase::Pre => &mut self.pre[position].filter,
                FilterPhase::Post => &mut self.filters[position].filter,
            };
            filter.flush(dom, lexer);
        }
        self.dom.running_filters = false;
        if self.dom.need_sanity_check {
            if let Some(problem) = self.dom.sanity_check() {
                self.dom.messages.push(Message {
                    warning: true,
                    line: 0,
                    text: format!("sanity: {problem}"),
                });
            }
        }
        self.dom.current_filter = None;
    }

    /// Resolves enablement for all filters once per document.
    fn determine_behavior(&mut self) {
        for record in self
            .early_pre
            .iter_mut()
            .chain(self.pre.iter_mut())
            .chain(self.filters.iter_mut())
        {
            if !record.determined {
                record.determined = true;
                let (dom, lexer) = (&mut self.dom, &self.lexer);
                record.enabled = record.filter.determine_enabled(dom, lexer);
                if !record.enabled {
                    if let Some(list) = &self.disabled_list {
                        let mut entry = record.filter.name().to_string();
                        if let Some(reason) = record.filter.disabled_reason() {
                            entry.push_str(": ");
                            entry.push_str(&reason);
                        }
                        list.borrow_mut().push(entry);
                    }
                }
            }
        }
    }

    // -- Introspection and limits --------------------------------------

    #[must_use]
    pub fn dom(&self) -> &Dom {
        &self.dom
    }

    pub fn dom_mut(&mut self) -> &mut Dom {
        &mut self.dom
    }

    #[must_use]
    pub fn lexer(&self) -> &Lexer {
        &self.lexer
    }

    #[must_use]
    pub fn messages(&self) -> &[Message] {
        self.dom.messages()
    }

    pub fn set_coalesce_characters(&mut self, coalesce: bool) {
        self.dom.coalesce_characters = coalesce;
    }

    pub fn set_size_limit(&mut self, limit: i64) {
        self.lexer.set_size_limit(limit);
    }

    #[must_use]
    pub fn size_limit_exceeded(&self) -> bool {
        self.lexer.size_limit_exceeded()
    }

    pub fn set_pass_through_after(&mut self, limit: i64) {
        self.lexer.set_pass_through_after(limit);
    }

    #[must_use]
    pub fn passing_through(&self) -> bool {
        self.lexer.passing_through()
    }

    /// Whether a listener requested a flush after this write.
    #[must_use]
    pub fn flush_requested(&self) -> bool {
        self.dom.flush_requested
    }

    #[must_use]
    pub fn filter_names(&self) -> Vec<&'static str> {
        self.filters
            .iter()
            .map(|record| record.filter.name())
            .collect()
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}
