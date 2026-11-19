//! Test harness mirroring HtmlParseTestBase: body wrapping, writer capture,
//! and cascade comparison.
use std::cell::RefCell;
use std::rc::Rc;

use ps_html::element::{ElementStyle, NodeId};
use ps_html::filter::{HtmlFilter, ScriptUsage};
use ps_html::lexer::Lexer;
use ps_html::name::Keyword;
use ps_html::parser::{Dom, Parser};
use ps_html::writer::{ByteSink, WriterFilter};

/// Shared sink so tests can read output without taking the filter apart.
#[derive(Debug, Clone, Default)]
pub struct SharedSink {
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl SharedSink {
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.borrow().clone()
    }
}

impl ByteSink for SharedSink {
    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        self.bytes.borrow_mut().extend_from_slice(bytes);
        true
    }
}

/// Wraps a fragment like `AddHtmlBody` (`<html><body>\n...\n</body></html>\n`).
pub fn wrap_body(fragment: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fragment.len() + 32);
    out.extend_from_slice(b"<html><body>\n");
    out.extend_from_slice(fragment);
    out.extend_from_slice(b"</body></html>\n");
    out
}

/// Runs `build` against a fresh parser and returns the writer output.
/// `build` registers filters (the writer is appended last automatically).
pub fn run_with_filters(input: &[u8], build: impl FnOnce(&mut Parser)) -> Vec<u8> {
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    build(&mut parser);
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/doc.html", false);
    parser.write(input);
    parser.finish();
    sink.bytes()
}

/// Parses `input` through a writer-only chain and returns the bytes.
pub fn round_trip(input: &[u8]) -> Vec<u8> {
    run_with_filters(input, |_| {})
}

/// Ports `ValidateExpected`: wraps input and expectation in a body and
/// compares the round-trip serialization byte-for-byte.
pub fn validate_expected(id: &str, input: &[u8], expected: &[u8]) {
    let wrapped_input = wrap_body(input);
    let wrapped_expected = wrap_body(expected);
    let output = round_trip(&wrapped_input);
    assert_eq!(output, wrapped_expected, "case {id}");
}

/// Shares a stateful filter with the test (the `filter_` pointer
/// equivalent): the parser owns one clone, the test reads the other.
#[derive(Debug)]
pub struct SharedFilter<F> {
    filter: Rc<RefCell<F>>,
}

impl<F> Clone for SharedFilter<F> {
    fn clone(&self) -> Self {
        SharedFilter {
            filter: self.filter.clone(),
        }
    }
}

impl<F> SharedFilter<F> {
    /// Wraps a filter for shared ownership.
    pub fn new(filter: F) -> Self {
        SharedFilter {
            filter: Rc::new(RefCell::new(filter)),
        }
    }

    /// Reads shared state after parsing.
    pub fn with<R>(&self, read: impl FnOnce(&F) -> R) -> R {
        read(&self.filter.borrow())
    }
}

impl<F: HtmlFilter> HtmlFilter for SharedFilter<F> {
    fn name(&self) -> &'static str {
        "SharedFilter"
    }

    fn determine_enabled(&mut self, dom: &Dom, lexer: &Lexer) -> bool {
        self.filter.borrow_mut().determine_enabled(dom, lexer)
    }

    fn can_modify_urls(&self) -> bool {
        self.filter.borrow().can_modify_urls()
    }

    fn script_usage(&self) -> ScriptUsage {
        self.filter.borrow().script_usage()
    }

    fn start_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        self.filter.borrow_mut().start_document(dom, lexer);
    }

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        self.filter.borrow_mut().end_document(dom, lexer);
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, element: NodeId) {
        self.filter.borrow_mut().start_element(dom, lexer, element);
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, element: NodeId) {
        self.filter.borrow_mut().end_element(dom, lexer, element);
    }

    fn cdata(&mut self, dom: &mut Dom, lexer: &Lexer, node: NodeId) {
        self.filter.borrow_mut().cdata(dom, lexer, node);
    }

    fn comment(&mut self, dom: &mut Dom, lexer: &Lexer, node: NodeId) {
        self.filter.borrow_mut().comment(dom, lexer, node);
    }

    fn ie_directive(&mut self, dom: &mut Dom, lexer: &Lexer, node: NodeId) {
        self.filter.borrow_mut().ie_directive(dom, lexer, node);
    }

    fn characters(&mut self, dom: &mut Dom, lexer: &Lexer, node: NodeId) {
        self.filter.borrow_mut().characters(dom, lexer, node);
    }

    fn directive(&mut self, dom: &mut Dom, lexer: &Lexer, node: NodeId) {
        self.filter.borrow_mut().directive(dom, lexer, node);
    }

    fn flush(&mut self, dom: &mut Dom, lexer: &Lexer) {
        self.filter.borrow_mut().flush(dom, lexer);
    }

    fn render_done(&mut self, dom: &mut Dom, lexer: &Lexer) {
        self.filter.borrow_mut().render_done(dom, lexer);
    }

    fn disabled_reason(&self) -> Option<String> {
        self.filter.borrow().disabled_reason()
    }

    fn init_parse(&mut self) {
        self.filter.borrow_mut().init_parse();
    }

    fn start_parse(&mut self) {
        self.filter.borrow_mut().start_parse();
    }

    fn end_parse(&mut self) {
        self.filter.borrow_mut().end_parse();
    }

    fn start_render(&mut self) {
        self.filter.borrow_mut().start_render();
    }
}

/// Test filter making implicit closes explicit (`ExplicitCloseTag`).
pub struct ExplicitCloseTag;

impl HtmlFilter for ExplicitCloseTag {
    fn name(&self) -> &'static str {
        "ExplicitCloseTag"
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        match dom.element_style(id) {
            ElementStyle::AutoClose | ElementStyle::Unclosed => {
                dom.set_element_style(id, ElementStyle::ExplicitClose);
            }
            _ => {}
        }
    }
}

/// Test filter inserting `<!--hello-->` at the first div
/// (`InsertCommentOnFirstDivFilter`).
pub struct InsertCommentOnFirstDiv {
    at_start: bool,
    first: bool,
}

impl InsertCommentOnFirstDiv {
    #[must_use]
    pub fn new(at_start: bool) -> Self {
        InsertCommentOnFirstDiv {
            at_start,
            first: true,
        }
    }

    fn insert(&mut self, at_start: bool, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if self.first && at_start == self.at_start && dom.element_keyword(id) == Keyword::Div {
            dom.insert_comment(b"hello", lexer.parent());
            self.first = false;
        }
    }
}

impl HtmlFilter for InsertCommentOnFirstDiv {
    fn name(&self) -> &'static str {
        "InsertCommentOnFirstDivFilter"
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        self.insert(true, dom, lexer, id);
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        self.insert(false, dom, lexer, id);
    }
}

/// Test filter inserting a script around the head element
/// (`InsertScriptsFilter`).
pub struct InsertScripts {
    before: bool,
    at_start: bool,
    external: bool,
}

impl InsertScripts {
    #[must_use]
    pub fn new(before: bool, at_start: bool, external: bool) -> Self {
        InsertScripts {
            before,
            at_start,
            external,
        }
    }

    fn insert(&mut self, at_start: bool, dom: &mut Dom, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Head && at_start == self.at_start {
            if self.before {
                dom.insert_script_before_current(b"inserted", self.external);
            } else {
                dom.insert_script_after_current(b"inserted", self.external);
            }
        }
    }
}

impl HtmlFilter for InsertScripts {
    fn name(&self) -> &'static str {
        "InsertScriptsFilter"
    }

    fn script_usage(&self) -> ScriptUsage {
        ScriptUsage::PotentiallyInjectsScripts
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.insert(true, dom, id);
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.insert(false, dom, id);
    }
}

/// Shared callback/deletion counters (`CountingCallbacksFilter` counters).
#[derive(Debug, Clone, Default)]
pub struct DeleteCounts {
    counts: Rc<RefCell<DeleteCountValues>>,
}

/// Counter values behind [`DeleteCounts`].
#[derive(Debug, Clone, Copy, Default)]
pub struct DeleteCountValues {
    pub starts: usize,
    pub ends: usize,
    pub chars: usize,
    pub deleted: usize,
}

impl DeleteCounts {
    #[must_use]
    pub fn values(&self) -> DeleteCountValues {
        *self.counts.borrow()
    }
}

/// Test filter deleting nodes mid-parse (`DeleteNodesFilter`).
pub struct DeleteNodes {
    delete_node_type: Keyword,
    delete_from_type: Keyword,
    delete_on_open_tag: bool,
    save_children: bool,
    make_invisible: bool,
    pending: Vec<NodeId>,
    counts: Rc<RefCell<DeleteCountValues>>,
}

impl DeleteNodes {
    #[must_use]
    pub fn new(counts: &DeleteCounts) -> Self {
        DeleteNodes {
            delete_node_type: Keyword::NotAKeyword,
            delete_from_type: Keyword::NotAKeyword,
            delete_on_open_tag: false,
            save_children: true,
            make_invisible: false,
            pending: Vec::new(),
            counts: counts.counts.clone(),
        }
    }

    pub fn set_delete_node_type(&mut self, keyword: Keyword) {
        self.delete_node_type = keyword;
    }

    pub fn set_delete_from_type(&mut self, keyword: Keyword) {
        self.delete_from_type = keyword;
    }

    pub fn set_delete_on_open_tag(&mut self, on_open: bool) {
        self.delete_on_open_tag = on_open;
    }

    fn delete_elements(&mut self, dom: &mut Dom) {
        for &id in &self.pending {
            let success = if self.make_invisible {
                dom.make_element_invisible(id)
            } else if self.save_children {
                dom.delete_saving_children(id)
            } else {
                dom.delete_node(id)
            };
            if success {
                self.counts.borrow_mut().deleted += 1;
            }
        }
        self.pending.clear();
    }
}

impl HtmlFilter for DeleteNodes {
    fn name(&self) -> &'static str {
        "DeleteNodesFilter"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.pending.clear();
        // Config survives across documents (repeat runs share settings).
        let mut counts = self.counts.borrow_mut();
        counts.starts = 0;
        counts.ends = 0;
        counts.chars = 0;
        counts.deleted = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.counts.borrow_mut().starts += 1;
        if dom.element_keyword(id) == self.delete_node_type {
            self.pending.push(id);
        }
        if self.delete_on_open_tag && dom.element_keyword(id) == self.delete_from_type {
            self.delete_elements(dom);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.counts.borrow_mut().ends += 1;
        if !self.delete_on_open_tag && dom.element_keyword(id) == self.delete_from_type {
            self.delete_elements(dom);
        }
    }

    fn characters(&mut self, _dom: &mut Dom, _lexer: &Lexer, _id: NodeId) {
        self.counts.borrow_mut().chars += 1;
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // A flushed element can no longer be deleted.
        self.pending.clear();
    }
}
