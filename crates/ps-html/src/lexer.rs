//! HTML tokenizer (`HtmlLexer`).
//!
//! Byte-at-a-time state machine producing events into the [`Dom`](crate::parser::Dom).
//! Owns the element stack, buffers, line numbers, doctype, and the size
//! limits. Listener dispatch happens synchronously from event emission, so
//! [`parse`](Lexer::parse) takes the listener list alongside the DOM.

use crate::doctype::DocType;
use crate::element::{ElementStyle, NodeId, QuoteStyle};
use crate::event::{Event, SlotId, NO_SLOT};
use crate::filter::HtmlFilter;
use crate::keywords;
use crate::name::{self, Keyword};
use crate::parser::Dom;

/// Token/attribute accumulation ceiling (10MB, `kMaxTokenSize`).
const MAX_TOKEN_SIZE: usize = 10 * 1024 * 1024;
/// Default pass-through threshold (`kHtmlPageLimitBytes`).
pub const DEFAULT_PASS_THROUGH_AFTER: i64 = 8 * 1024 * 1024;
/// Maximum element nesting (`kMaxNestingDepth`).
const MAX_NESTING_DEPTH: usize = 512;

/// Lexer states (HTML5 spec names where they exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    Tag,
    TagOpen,
    TagCloseNoName,
    TagClose,
    TagCloseTerminate,
    TagBriefClose,
    CommentStart1,
    CommentStart2,
    CommentBody,
    CommentEnd1,
    CommentEnd2,
    CommentEndBang,
    CdataStart1,
    CdataStart2,
    CdataStart3,
    CdataStart4,
    CdataStart5,
    CdataStart6,
    CdataBody,
    CdataEnd1,
    CdataEnd2,
    TagAttribute,
    TagAttrName,
    TagAttrNameSpace,
    TagAttrEq,
    TagAttrVal,
    TagAttrValDq,
    TagAttrValSq,
    LiteralTag,
    ScriptTag,
    Directive,
    BogusComment,
}

/// Byte-oriented HTML lexer.
pub struct Lexer {
    state: State,
    token: Vec<u8>,
    attr_name: Vec<u8>,
    attr_value: Vec<u8>,
    attr_quote: QuoteStyle,
    has_attr_value: bool,
    literal: Vec<u8>,
    literal_close: Vec<u8>,
    /// Element under construction (created in the arena at tag name end).
    element: Option<NodeId>,
    /// Open-element stack; slot 0 is the `None` sentinel (`kStartStack` = 1).
    element_stack: Vec<Option<NodeId>>,
    line: i32,
    tag_start_line: i32,
    id: Vec<u8>,
    script_html_comment: bool,
    script_html_comment_script: bool,
    discard_for_recovery: bool,
    doctype: DocType,
    content_is_xml_like: bool,
    // Limits.
    size_limit: i64,
    bytes_parsed: u64,
    size_limit_exceeded: bool,
    skip_parsing: bool,
    pass_through_after: i64,
    passing_through: bool,
}

impl Lexer {
    #[must_use]
    pub fn new() -> Self {
        Lexer {
            state: State::Start,
            token: Vec::new(),
            attr_name: Vec::new(),
            attr_value: Vec::new(),
            attr_quote: QuoteStyle::NoQuote,
            has_attr_value: false,
            literal: Vec::new(),
            literal_close: Vec::new(),
            element: None,
            element_stack: vec![None],
            line: 1,
            tag_start_line: -1,
            id: Vec::new(),
            script_html_comment: false,
            script_html_comment_script: false,
            discard_for_recovery: false,
            doctype: DocType::Unknown,
            content_is_xml_like: false,
            size_limit: -1,
            bytes_parsed: 0,
            size_limit_exceeded: false,
            skip_parsing: false,
            pass_through_after: DEFAULT_PASS_THROUGH_AFTER,
            passing_through: false,
        }
    }

    /// Starts a document (`StartParse`).
    pub fn start_parse(&mut self, id: &[u8], content_is_xml_like: bool) {
        self.line = 1;
        self.tag_start_line = -1;
        self.id = id.to_vec();
        self.content_is_xml_like = content_is_xml_like;
        self.has_attr_value = false;
        self.attr_quote = QuoteStyle::NoQuote;
        self.state = State::Start;
        self.element_stack.clear();
        self.element_stack.push(None);
        self.element = None;
        self.token.clear();
        self.attr_name.clear();
        self.attr_value.clear();
        self.literal.clear();
        self.size_limit_exceeded = false;
        self.skip_parsing = false;
        self.bytes_parsed = 0;
        self.passing_through = false;
        self.script_html_comment = false;
        self.script_html_comment_script = false;
        self.discard_for_recovery = false;
        self.doctype = DocType::Unknown;
    }

    #[must_use]
    pub fn doctype(&self) -> DocType {
        self.doctype
    }

    #[must_use]
    pub fn passing_through(&self) -> bool {
        self.passing_through
    }

    #[must_use]
    pub fn size_limit_exceeded(&self) -> bool {
        self.size_limit_exceeded
    }

    pub fn set_size_limit(&mut self, limit: i64) {
        self.size_limit = limit;
    }

    pub fn set_pass_through_after(&mut self, limit: i64) {
        self.pass_through_after = limit;
    }

    /// Innermost open element (`Parent`), or `None` past the sentinel.
    #[must_use]
    pub fn parent(&self) -> Option<NodeId> {
        self.element_stack.last().copied().flatten()
    }

    /// Document URL passed to `start_parse` (`driver()->google_url()`).
    #[must_use]
    pub fn document_url(&self) -> &[u8] {
        &self.id
    }

    /// Tokenizes one chunk (`HtmlLexer::Parse`), emitting events into the DOM
    /// and fanning each out to listeners synchronously.
    pub fn parse(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], text: &[u8]) {
        if self.passing_through {
            if !text.is_empty() {
                self.pass_through(dom, listeners, text);
            }
            return;
        }
        let parsed_before = self.bytes_parsed;
        self.bytes_parsed += text.len() as u64;
        if self.size_limit > 0 && self.bytes_parsed > self.size_limit as u64 {
            self.size_limit_exceeded = true;
        }
        let mut index = 0;
        while index < text.len() {
            if self.skip_parsing {
                return;
            }
            if self.pass_through_after >= 0
                && parsed_before + index as u64 >= self.pass_through_after as u64
                && self.state == State::Start
            {
                self.passing_through = true;
                let id = std::mem::take(&mut self.id);
                dom.message_warning(
                    &id,
                    self.line,
                    format!(
                        "HTML past {} bytes is served unoptimized",
                        self.pass_through_after
                    ),
                );
                self.id = id;
                self.pass_through(dom, listeners, &text[index..]);
                return;
            }
            let byte = text[index];
            if byte == b'\n' {
                self.line += 1;
            }
            if self.token.len() > MAX_TOKEN_SIZE
                || self.attr_value.len() > MAX_TOKEN_SIZE
                || self.literal.len() > MAX_TOKEN_SIZE
                || self.attr_name.len() > MAX_TOKEN_SIZE
            {
                self.size_limit_exceeded = true;
                self.skip_parsing = true;
                return;
            }
            self.literal.push(byte);
            match self.state {
                State::Start => self.eval_start(dom, listeners, byte),
                State::Tag => self.eval_tag(dom, listeners, byte),
                State::TagOpen => self.eval_tag_open(dom, listeners, byte),
                State::TagCloseNoName => self.eval_tag_close_no_name(dom, listeners, byte),
                State::TagClose | State::TagCloseTerminate => {
                    self.eval_tag_close(dom, listeners, byte)
                }
                State::TagBriefClose => self.eval_tag_brief_close(dom, listeners, byte),
                State::CommentStart1 => self.eval_comment_start1(dom, listeners, byte),
                State::CommentStart2 => self.eval_comment_start2(dom, listeners, byte),
                State::CommentBody => self.eval_comment_body(dom, listeners, byte),
                State::CommentEnd1 => self.eval_comment_end1(dom, listeners, byte),
                State::CommentEnd2 => self.eval_comment_end2(dom, listeners, byte),
                State::CommentEndBang => self.eval_comment_end_bang(dom, listeners, byte),
                State::CdataStart1 => {
                    self.eval_cdata_start(dom, listeners, byte, b'C', State::CdataStart2)
                }
                State::CdataStart2 => {
                    self.eval_cdata_start(dom, listeners, byte, b'D', State::CdataStart3)
                }
                State::CdataStart3 => {
                    self.eval_cdata_start(dom, listeners, byte, b'A', State::CdataStart4)
                }
                State::CdataStart4 => {
                    self.eval_cdata_start(dom, listeners, byte, b'T', State::CdataStart5)
                }
                State::CdataStart5 => {
                    self.eval_cdata_start(dom, listeners, byte, b'A', State::CdataStart6)
                }
                State::CdataStart6 => {
                    self.eval_cdata_start(dom, listeners, byte, b'[', State::CdataBody)
                }
                State::CdataBody => self.eval_cdata_body(byte),
                State::CdataEnd1 => self.eval_cdata_end1(byte),
                State::CdataEnd2 => self.eval_cdata_end2(dom, listeners, byte),
                State::TagAttribute => self.eval_attribute(dom, listeners, byte),
                State::TagAttrName => self.eval_attr_name(dom, listeners, byte),
                State::TagAttrNameSpace => self.eval_attr_name_space(dom, listeners, byte),
                State::TagAttrEq => self.eval_attr_eq(dom, listeners, byte),
                State::TagAttrVal => self.eval_attr_val(dom, listeners, byte),
                State::TagAttrValDq => self.eval_attr_val_quoted(dom, listeners, byte, b'"'),
                State::TagAttrValSq => self.eval_attr_val_quoted(dom, listeners, byte, b'\''),
                State::LiteralTag => self.eval_literal_tag(dom, listeners, byte),
                State::ScriptTag => self.eval_script_tag(dom, listeners, byte),
                State::Directive => self.eval_directive(dom, listeners, byte),
                State::BogusComment => self.eval_bogus_comment(dom, listeners, byte),
            }
            if self.passing_through {
                // The nesting cap refused an element: its start tag is still
                // in the literal buffer and goes out as text with the rest.
                self.pass_through(dom, listeners, &text[index + 1..]);
                return;
            }
            index += 1;
        }
    }

    /// End of input (`FinishParse`): flushes literals, reports truncation,
    /// and closes open elements.
    pub fn finish_parse(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>]) {
        if !self.token.is_empty() {
            self.syntax_error(dom, format!("End-of-file in mid-token"));
            self.token.clear();
        }
        if !self.attr_name.is_empty() {
            self.syntax_error(dom, format!("End-of-file in mid-attribute-name"));
            self.attr_name.clear();
        }
        if !self.attr_value.is_empty() {
            self.syntax_error(dom, format!("End-of-file in mid-attribute-value"));
            self.attr_value.clear();
        }
        if !self.literal.is_empty() {
            self.emit_literal(dom, listeners);
        }
        while self.element_stack.len() > 1 {
            let id = self.element_stack.pop().unwrap().unwrap();
            let name = dom.element_name(id);
            self.token = name;
            let style = if self.skip_parsing {
                ElementStyle::ExplicitClose
            } else {
                ElementStyle::Unclosed
            };
            self.close_element(dom, listeners, id, style);
            if !keywords::is_optionally_closed_tag(dom.element_keyword(id)) {
                let id_bytes = std::mem::take(&mut self.id);
                dom.message_info(
                    &id_bytes,
                    dom.element_begin_line(id),
                    format!("End-of-file with open tag"),
                );
                self.id = id_bytes;
            }
        }
        self.element = None;
    }

    fn syntax_error(&mut self, dom: &mut Dom, text: String) {
        let id = std::mem::take(&mut self.id);
        dom.message_info(&id, self.line, text);
        self.id = id;
    }

    // -- Emission helpers -------------------------------------------------

    fn add_event(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        event: Event,
        line: i32,
    ) {
        let slot = dom.add_event(event, line);
        if !listeners.is_empty() {
            dispatch_event(self, dom, listeners, slot);
        }
    }

    fn pass_through(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], text: &[u8]) {
        self.literal.extend_from_slice(text);
        self.emit_literal(dom, listeners);
    }

    fn emit_literal(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>]) {
        if !self.literal.is_empty() {
            let contents = std::mem::take(&mut self.literal);
            let parent = self.parent();
            let id = dom.new_leaf(parent, crate::element::LeafKind::Characters, contents);
            let line = self.tag_start_line;
            self.add_event(dom, listeners, Event::Leaf(id), line);
        }
        self.state = State::Start;
    }

    fn emit_comment(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>]) {
        use crate::element::LeafKind;
        self.literal.clear();
        let token = std::mem::take(&mut self.token);
        let is_ie = contains(&token, b"[if") || contains(&token, b"[endif]");
        let parent = self.parent();
        let line = self.tag_start_line;
        if is_ie {
            let id = dom.new_leaf(parent, LeafKind::IEDirective, token);
            self.add_event(dom, listeners, Event::Leaf(id), line);
        } else {
            let id = dom.new_leaf(parent, LeafKind::Comment, token);
            self.add_event(dom, listeners, Event::Leaf(id), line);
        }
        self.state = State::Start;
    }

    fn emit_cdata(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>]) {
        self.literal.clear();
        let token = std::mem::take(&mut self.token);
        let parent = self.parent();
        let line = self.tag_start_line;
        let id = dom.new_leaf(parent, crate::element::LeafKind::Cdata, token);
        self.add_event(dom, listeners, Event::Leaf(id), line);
        self.state = State::Start;
    }

    fn emit_directive(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>]) {
        self.literal.clear();
        let token = std::mem::take(&mut self.token);
        self.doctype.parse(&token, self.content_is_xml_like);
        let parent = self.parent();
        let line = self.line;
        let id = dom.new_leaf(parent, crate::element::LeafKind::Directive, token);
        self.add_event(dom, listeners, Event::Leaf(id), line);
        self.state = State::Start;
    }

    fn make_element(&mut self, dom: &mut Dom) {
        if self.element.is_none() {
            if self.token.is_empty() {
                self.syntax_error(dom, "Making element with empty tag name".to_string());
            }
            let parent = self.parent();
            let id = dom.new_element(parent, &self.token);
            dom.set_element_begin_line(id, self.tag_start_line);
            self.element = Some(id);
            self.token.clear();
        }
    }

    fn make_attribute(&mut self, dom: &mut Dom, has_value: bool) {
        let name = std::mem::take(&mut self.attr_name);
        let value = if has_value {
            Some(std::mem::take(&mut self.attr_value))
        } else {
            None
        };
        if !self.discard_for_recovery {
            if let Some(id) = self.element {
                dom.add_escaped_attribute(id, &name, value.as_deref(), self.attr_quote);
            }
        }
        self.attr_value.clear();
        self.attr_quote = QuoteStyle::NoQuote;
        self.has_attr_value = false;
        self.state = State::TagAttribute;
    }

    fn close_element(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        id: NodeId,
        style: ElementStyle,
    ) {
        let slot = dom.close_element(id, style, self.line);
        if !listeners.is_empty() {
            dispatch_event(self, dom, listeners, slot);
        }
        if self.size_limit_exceeded {
            self.skip_parsing = true;
        }
    }

    fn emit_tag_open(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        allow_implicit_close: bool,
    ) {
        if self.discard_for_recovery {
            self.state = State::Start;
            self.literal.clear();
            return;
        }
        let id = match self.element {
            Some(id) => id,
            None => {
                self.state = State::Start;
                return;
            }
        };
        let next_keyword = dom.element_keyword(id);
        // Pop auto-closed ancestors, fixing the new parent chain.
        loop {
            let open = match self.parent() {
                Some(open) => open,
                None => break,
            };
            if !keywords::is_auto_close(dom.element_keyword(open), next_keyword) {
                break;
            }
            self.element_stack.pop();
            self.close_element(dom, listeners, open, ElementStyle::AutoClose);
            dom.set_element_parent(id, self.parent());
        }
        if self.element_stack.len() - 1 >= MAX_NESTING_DEPTH {
            self.passing_through = true;
            let id_bytes = std::mem::take(&mut self.id);
            dom.message_warning(
                &id_bytes,
                self.line,
                format!(
                    "HTML nested deeper than {} elements is served unoptimized from here",
                    MAX_NESTING_DEPTH
                ),
            );
            self.id = id_bytes;
            self.element = None;
            self.state = State::Start;
            return;
        }
        self.literal.clear();
        let line = self.tag_start_line;
        self.add_event(dom, listeners, Event::StartElement(id), line);
        dom.set_element_begin(id, dom.last_slot());
        if self.size_limit_exceeded {
            self.skip_parsing = true;
        }
        self.element_stack.push(Some(id));
        if keywords::is_literal_tag(next_keyword) {
            self.state = if next_keyword == Keyword::Script {
                State::ScriptTag
            } else {
                State::LiteralTag
            };
            self.script_html_comment = false;
            self.script_html_comment_script = false;
            let mut close = b"</".to_vec();
            close.extend_from_slice(&dom.element_name(id));
            close.push(b'>');
            self.literal_close = close;
        } else {
            self.state = State::Start;
        }
        if allow_implicit_close && keywords::is_implicitly_closed_tag(next_keyword) {
            self.token = dom.element_name(id);
            self.emit_tag_close(dom, listeners, ElementStyle::ImplicitClose);
        }
        self.element = None;
    }

    fn emit_tag_brief_close(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>]) {
        if !self.discard_for_recovery {
            let popped = self.element_stack.pop().flatten();
            if let Some(id) = popped {
                self.close_element(dom, listeners, id, ElementStyle::BriefClose);
            }
        }
        self.state = State::Start;
    }

    fn emit_tag_close(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        style: ElementStyle,
    ) {
        if let Some(id) = self.pop_element_matching_tag(dom, listeners) {
            dom.set_element_end_line(id, self.line);
            self.close_element(dom, listeners, id, style);
        } else {
            let token = String::from_utf8_lossy(&self.token).into_owned();
            self.syntax_error(
                dom,
                format!("Unexpected close-tag `{token}', no tags are open"),
            );
            // Emit the unmatched close tag as literal characters to stay
            // byte-accurate with the input.
            self.emit_literal(dom, listeners);
        }
        self.literal.clear();
        self.token.clear();
        self.state = State::Start;
    }

    /// Pops the stack down to the named element, closing skipped elements
    /// as unclosed (with warnings unless optionally closed). Gives up
    /// (returning `None`) past a containment owner.
    fn pop_element_matching_tag(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
    ) -> Option<NodeId> {
        let keyword = name::lookup(&self.token);
        let mut close_index = None;
        for (index, entry) in self.element_stack.iter().enumerate().rev() {
            let Some(id) = entry else { continue };
            if dom.element_name_equals(*id, &self.token) {
                close_index = Some(index);
                break;
            } else if keywords::is_contained(keyword, dom.element_keyword(*id)) {
                return None;
            }
            if index == 1 {
                break;
            }
        }
        let close_index = close_index?;
        while self.element_stack.len() - 1 > close_index {
            let skipped = self.element_stack.pop().flatten().unwrap();
            if !keywords::is_optionally_closed_tag(dom.element_keyword(skipped)) {
                let name = String::from_utf8_lossy(&dom.element_name(skipped)).into_owned();
                let id_bytes = std::mem::take(&mut self.id);
                dom.message_info(
                    &id_bytes,
                    dom.element_begin_line(skipped),
                    format!("Unclosed element `{name}'"),
                );
                self.id = id_bytes;
            }
            self.close_element(dom, listeners, skipped, ElementStyle::Unclosed);
        }
        let id = self.element_stack.pop().flatten().unwrap();
        Some(id)
    }

    /// Partial-match recovery: emit the retained literal minus this byte,
    /// then re-evaluate the byte as a potential token start (`Restart`).
    fn restart(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        self.literal.pop();
        self.emit_literal(dom, listeners);
        self.literal.push(byte);
        self.eval_start(dom, listeners, byte);
    }

    // -- State evaluators -------------------------------------------------

    fn eval_start(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'<' {
            self.literal.pop();
            self.emit_literal(dom, listeners);
            self.literal.push(byte);
            self.state = State::Tag;
            self.discard_for_recovery = false;
            self.tag_start_line = self.line;
        } else {
            self.state = State::Start;
        }
    }

    fn eval_tag(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'/' {
            self.state = State::TagCloseNoName;
        } else if is_legal_tag_first_char(byte) {
            self.state = State::TagOpen;
            self.discard_for_recovery = false;
            self.token.push(byte);
        } else if byte == b'!' {
            self.state = State::CommentStart1;
        } else if byte == b'?' {
            self.state = State::BogusComment;
        } else {
            let text = format!("Invalid tag syntax: unexpected sequence `<{byte}'");
            self.syntax_error(dom, text);
            self.eval_start(dom, listeners, byte);
        }
    }

    fn eval_tag_open(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if is_legal_tag_char(byte) {
            self.token.push(byte);
        } else if byte == b'>' {
            self.make_element(dom);
            self.emit_tag_open(dom, listeners, true);
        } else if byte == b'/' {
            self.state = State::TagBriefClose;
        } else if keywords::is_html_space(byte) {
            self.state = State::TagAttribute;
        } else {
            let token = String::from_utf8_lossy(&self.token).into_owned();
            self.syntax_error(
                dom,
                format!("Invalid character `{byte}' while parsing tag `{token}'"),
            );
            self.token.clear();
            self.state = State::Start;
        }
    }

    fn eval_tag_brief_close(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'>' {
            if !self.discard_for_recovery {
                self.make_element(dom);
            }
            self.finish_attribute(dom, listeners, byte, self.has_attr_value, true);
        } else {
            if !self.attr_name.is_empty() {
                let has_value = self.has_attr_value;
                self.make_attribute(dom, has_value);
            }
            self.state = State::TagAttribute;
            self.eval_attribute(dom, listeners, byte);
        }
    }

    fn eval_tag_close_no_name(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if is_legal_tag_char(byte) {
            self.token.push(byte);
            self.state = State::TagClose;
        } else if byte == b'>' {
            // `</>` — syntax error, then re-evaluate as literal text.
            self.token.clear();
            self.eval_start(dom, listeners, byte);
        } else {
            self.state = State::BogusComment;
        }
    }

    fn eval_tag_close(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if self.state != State::TagCloseTerminate && is_legal_tag_char(byte) {
            self.token.push(byte);
        } else if keywords::is_html_space(byte) {
            if self.token.is_empty() {
                // `</ a>`: ignore whitespace before the name.
            } else {
                self.state = State::TagCloseTerminate;
            }
        } else if byte == b'>' {
            self.emit_tag_close(dom, listeners, ElementStyle::ExplicitClose);
        } else {
            let token = String::from_utf8_lossy(&self.token).into_owned();
            self.syntax_error(
                dom,
                format!("Invalid tag syntax: expected `>' after `</{token}' got `{byte}'"),
            );
            self.token.clear();
            self.eval_start(dom, listeners, byte);
        }
    }

    fn eval_directive(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'>' {
            self.emit_directive(dom, listeners);
        } else {
            self.token.push(byte);
        }
    }

    fn eval_comment_start1(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'-' {
            self.state = State::CommentStart2;
        } else if byte == b'[' {
            self.state = State::CdataStart1;
        } else if is_legal_tag_char(byte) && byte != b'<' {
            self.state = State::Directive;
            self.eval_directive(dom, listeners, byte);
        } else {
            self.syntax_error(dom, "Invalid comment syntax".to_string());
            self.restart(dom, listeners, byte);
        }
    }

    fn eval_comment_start2(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'-' {
            self.state = State::CommentBody;
        } else {
            self.syntax_error(dom, "Invalid comment syntax".to_string());
            self.restart(dom, listeners, byte);
        }
    }

    fn eval_comment_body(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'-' {
            self.state = State::CommentEnd1;
        } else if byte == b'>' && self.token.is_empty() {
            // `<!-->` is a complete empty comment.
            self.emit_comment(dom, listeners);
            self.state = State::Start;
        } else {
            self.token.push(byte);
        }
    }

    fn eval_comment_end1(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'-' {
            self.state = State::CommentEnd2;
        } else if byte == b'>' && self.token.is_empty() {
            // `<!--->` is a complete empty comment.
            self.emit_comment(dom, listeners);
            self.state = State::Start;
        } else {
            self.token.push(b'-');
            self.token.push(byte);
            self.state = State::CommentBody;
        }
    }

    fn eval_comment_end2(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'>' {
            self.emit_comment(dom, listeners);
            self.state = State::Start;
        } else if byte == b'-' {
            self.token.push(b'-');
        } else if byte == b'!' {
            self.state = State::CommentEndBang;
        } else {
            self.token.extend_from_slice(b"--");
            self.token.push(byte);
            self.state = State::CommentBody;
        }
    }

    fn eval_comment_end_bang(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'>' {
            self.emit_comment(dom, listeners);
            self.state = State::Start;
        } else {
            self.token.extend_from_slice(b"--!");
            self.state = State::CommentBody;
            self.eval_comment_body(dom, listeners, byte);
        }
    }

    fn eval_cdata_start(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
        expect: u8,
        next: State,
    ) {
        if byte == expect {
            self.state = next;
        } else {
            self.syntax_error(dom, "Invalid CDATA syntax".to_string());
            self.restart(dom, listeners, byte);
        }
    }

    fn eval_cdata_body(&mut self, byte: u8) {
        if byte == b']' {
            self.state = State::CdataEnd1;
        } else {
            self.token.push(byte);
        }
    }

    fn eval_cdata_end1(&mut self, byte: u8) {
        if byte == b']' {
            self.state = State::CdataEnd2;
        } else {
            self.token.push(b']');
            self.token.push(byte);
            self.state = State::CdataBody;
        }
    }

    fn eval_cdata_end2(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'>' {
            self.emit_cdata(dom, listeners);
            self.state = State::Start;
        } else {
            self.token.extend_from_slice(b"]]");
            self.token.push(byte);
            self.state = State::CdataBody;
        }
    }

    fn eval_attribute(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if !self.discard_for_recovery {
            self.make_element(dom);
        }
        self.attr_name.clear();
        self.attr_value.clear();
        if byte == b'>' {
            self.emit_tag_open(dom, listeners, true);
        } else if byte == b'/' {
            self.state = State::TagBriefClose;
        } else if is_legal_attr_name_char(byte) {
            self.attr_name.push(byte);
            self.state = State::TagAttrName;
        } else if !keywords::is_html_space(byte) {
            let text = format!("Unexpected char `{byte}' in attribute list");
            self.syntax_error(dom, text);
            self.attr_name.push(byte);
            self.state = State::TagAttrName;
        }
    }

    fn eval_attr_name(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'=' {
            self.state = State::TagAttrEq;
            self.has_attr_value = true;
        } else if keywords::is_html_space(byte) {
            self.state = State::TagAttrNameSpace;
        } else if byte == b'>' {
            self.make_attribute(dom, false);
            self.emit_tag_open(dom, listeners, true);
        } else if byte == b'/' {
            self.state = State::TagBriefClose;
        } else {
            self.attr_name.push(byte);
        }
    }

    fn eval_attr_name_space(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'=' {
            self.state = State::TagAttrEq;
            self.has_attr_value = true;
        } else if keywords::is_html_space(byte) {
            self.state = State::TagAttrNameSpace;
        } else if byte == b'>' {
            self.make_attribute(dom, false);
            self.emit_tag_open(dom, listeners, true);
        } else if byte == b'/' {
            self.state = State::TagBriefClose;
        } else {
            self.make_attribute(dom, false);
            self.state = State::TagAttrName;
            self.attr_name.push(byte);
        }
    }

    fn finish_attribute(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
        has_value: bool,
        brief_close: bool,
    ) {
        if keywords::is_html_space(byte) {
            self.make_attribute(dom, has_value);
            self.state = State::TagAttribute;
        } else if byte == b'>' {
            if !self.attr_name.is_empty() {
                self.make_attribute(dom, has_value);
            }
            self.emit_tag_open(dom, listeners, !brief_close);
            if brief_close && !self.passing_through {
                self.emit_tag_brief_close(dom, listeners);
            }
            self.has_attr_value = false;
        }
    }

    fn eval_attr_eq(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'"' {
            self.attr_quote = QuoteStyle::DoubleQuote;
            self.state = State::TagAttrValDq;
        } else if byte == b'\'' {
            self.attr_quote = QuoteStyle::SingleQuote;
            self.state = State::TagAttrValSq;
        } else if keywords::is_html_space(byte) {
            // Spaces allowed between '=' and the value.
        } else if byte == b'>' {
            self.finish_attribute(dom, listeners, byte, true, false);
        } else {
            self.state = State::TagAttrVal;
            self.attr_quote = QuoteStyle::NoQuote;
            self.eval_attr_val(dom, listeners, byte);
        }
    }

    fn eval_attr_val(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if keywords::is_html_space(byte) || byte == b'>' {
            self.finish_attribute(dom, listeners, byte, true, false);
        } else {
            self.attr_value.push(byte);
        }
    }

    fn eval_attr_val_quoted(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
        quote: u8,
    ) {
        let _ = listeners;
        if byte == quote {
            self.make_attribute(dom, true);
        } else {
            self.attr_value.push(byte);
        }
    }

    fn eval_literal_tag(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if can_end_tag(byte) && !self.literal.is_empty() {
            // `byte` stands in for the '>' of literal_close_; match "</x".
            let prev_len = self.literal.len() - 1;
            let close_len = self.literal_close.len() - 1;
            if prev_len >= close_len
                && self.literal[prev_len - close_len..prev_len]
                    .eq_ignore_ascii_case(&self.literal_close[..close_len])
            {
                self.literal.truncate(prev_len - close_len);
                self.emit_literal(dom, listeners);
                self.token.clear();
                // "</style>" -> "style".
                self.token
                    .extend_from_slice(&self.literal_close[2..self.literal_close.len() - 1]);
                self.emit_tag_close(dom, listeners, ElementStyle::ExplicitClose);
                if byte == b'/' {
                    self.discard_for_recovery = true;
                    self.state = State::TagBriefClose;
                } else if byte != b'>' {
                    self.discard_for_recovery = true;
                    self.state = State::TagAttribute;
                }
            }
        }
    }

    fn eval_script_tag(&mut self, dom: &mut Dom, listeners: &mut [Box<dyn HtmlFilter>], byte: u8) {
        if byte == b'-' && self.literal.ends_with(b"<!--") {
            self.script_html_comment = true;
        }
        if can_end_tag(byte) && !self.literal.is_empty() {
            let prev = &self.literal[..self.literal.len() - 1];
            if ends_ignore_case(prev, b"</script") {
                if self.script_html_comment_script {
                    self.script_html_comment_script = false;
                } else {
                    self.script_html_comment = false;
                    self.script_html_comment_script = false;
                    // `prev` ends with `</script`; `byte` is its terminator.
                    let end = self.literal.len() - 1;
                    let close_start = end - "</script".len();
                    self.token = self.literal[close_start + 2..end].to_vec();
                    self.literal.truncate(close_start);
                    self.emit_literal(dom, listeners);
                    self.emit_tag_close(dom, listeners, ElementStyle::ExplicitClose);
                    if matches!(byte, b'\t' | b'\n' | b'\r' | 0x0c | b' ') {
                        self.discard_for_recovery = true;
                        self.state = State::TagAttribute;
                    } else if byte == b'/' {
                        self.discard_for_recovery = true;
                        self.state = State::TagBriefClose;
                    }
                }
            } else if self.script_html_comment && ends_ignore_case(prev, b"<script") {
                self.script_html_comment_script = true;
            } else if byte == b'>' && self.literal.ends_with(b"-->") {
                self.script_html_comment = false;
                self.script_html_comment_script = false;
            }
        }
    }

    fn eval_bogus_comment(
        &mut self,
        dom: &mut Dom,
        listeners: &mut [Box<dyn HtmlFilter>],
        byte: u8,
    ) {
        if byte == b'>' {
            self.emit_literal(dom, listeners);
            self.state = State::Start;
        }
    }
}

// ---------------------------------------------------------------------------
// Predicates.
// ---------------------------------------------------------------------------

/// ASCII letters only (locale-independent `isalpha`).
fn is_legal_tag_first_char(byte: u8) -> bool {
    byte.is_ascii_alphabetic()
}

/// Tag-name continuation chars (the "bogus" C++ set, ported exactly).
fn is_legal_tag_char(byte: u8) -> bool {
    byte > 127 || byte.is_ascii_alphanumeric() || matches!(byte, b'<' | b'-' | b'#' | b'_' | b':')
}

/// Attribute-name chars: anything but `=`, `>`, `/`, whitespace.
fn is_legal_attr_name_char(byte: u8) -> bool {
    byte > 127 || (byte != b'=' && byte != b'>' && byte != b'/' && !keywords::is_html_space(byte))
}

/// Tag-name terminators inside literal/script scanning (`CanEndTag`).
fn can_end_tag(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\r' | b'\n' | 0x0c | b' ' | b'/' | b'>')
}

/// Byte substring search.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && (0..=haystack.len() - needle.len()).any(|i| &haystack[i..i + needle.len()] == needle)
}

/// Case-insensitive suffix test.
fn ends_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack[haystack.len() - needle.len()..].eq_ignore_ascii_case(needle)
}

/// Runs one queued event through every listener (`AddEvent` fan-out).
fn dispatch_event(
    lexer: &Lexer,
    dom: &mut Dom,
    listeners: &mut [Box<dyn HtmlFilter>],
    slot: SlotId,
) {
    use crate::element::{LeafKind, NodeData};
    let (event, _) = dom.queue_event(slot);
    for listener in listeners.iter_mut() {
        match event {
            Event::StartDocument => listener.start_document(dom, lexer),
            Event::EndDocument => listener.end_document(dom, lexer),
            Event::StartElement(id) => listener.start_element(dom, lexer, id),
            Event::EndElement(id) => listener.end_element(dom, lexer, id),
            Event::Leaf(id) => match dom.arena_node(id) {
                NodeData::Leaf(leaf) => match leaf.kind {
                    LeafKind::Characters => listener.characters(dom, lexer, id),
                    LeafKind::Comment => listener.comment(dom, lexer, id),
                    LeafKind::Cdata => listener.cdata(dom, lexer, id),
                    LeafKind::IEDirective => listener.ie_directive(dom, lexer, id),
                    LeafKind::Directive => listener.directive(dom, lexer, id),
                },
                NodeData::Element(_) => {}
            },
        }
    }
}
