//! HTML serialization (`HtmlWriterFilter`).
//!
//! Serializes the event stream back to bytes: tag names and attributes,
//! brief/implicit/explicit close styles with lazy-close validation,
//! comments, CDATA, directives, and the 16KB staging buffer. Output goes to
//! a caller-supplied [`ByteSink`].

use crate::element::{ElementStyle, NodeId};
use crate::filter::{HtmlFilter, ScriptUsage};
use crate::lexer::Lexer;
use crate::parser::Dom;

/// Byte sink for serialized output.
pub trait ByteSink {
    /// Writes bytes; returns false on error (counted, like the C++).
    fn write_bytes(&mut self, bytes: &[u8]) -> bool;
    /// Flushes downstream; returns false on error.
    fn flush_sink(&mut self) -> bool {
        true
    }
}

impl ByteSink for Vec<u8> {
    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        self.extend_from_slice(bytes);
        true
    }
}

/// Staging buffer size (`kOutputBufferSize`).
pub const OUTPUT_BUFFER_SIZE: usize = 16384;

/// Serializing filter, normally last in the chain.
pub struct WriterFilter<Sink: ByteSink> {
    sink: Sink,
    output_buffer: Vec<u8>,
    lazy_close_element: Option<NodeId>,
    column: usize,
    max_column: i64,
    case_fold: bool,
    write_errors: u32,
}

impl<Sink: ByteSink> WriterFilter<Sink> {
    #[must_use]
    pub fn new(sink: Sink) -> Self {
        WriterFilter {
            sink,
            output_buffer: Vec::new(),
            lazy_close_element: None,
            column: 0,
            max_column: -1,
            case_fold: false,
            write_errors: 0,
        }
    }

    pub fn set_max_column(&mut self, max_column: i64) {
        self.max_column = max_column;
    }

    pub fn set_case_fold(&mut self, case_fold: bool) {
        self.case_fold = case_fold;
    }

    #[must_use]
    pub fn write_errors(&self) -> u32 {
        self.write_errors
    }

    /// Takes the sink back (draining first), for tests.
    pub fn into_sink(mut self) -> Sink {
        self.drain_buffer();
        self.sink
    }

    fn terminate_lazy_close(&mut self) {
        if self.lazy_close_element.take().is_some() {
            self.write_raw(b">");
            self.column += 1;
        }
    }

    fn emit_bytes(&mut self, bytes: &[u8]) {
        self.terminate_lazy_close();
        // Track the column from the last newline.
        self.column += bytes.len();
        if let Some(pos) = bytes.iter().rposition(|byte| *byte == b'\n') {
            self.column = bytes.len() - pos - 1;
        }
        self.write_raw(bytes);
    }

    fn write_raw(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() >= OUTPUT_BUFFER_SIZE {
            self.drain_buffer();
            if !self.sink.write_bytes(bytes) {
                self.write_errors += 1;
            }
        } else {
            if self.output_buffer.len() + bytes.len() > OUTPUT_BUFFER_SIZE {
                self.drain_buffer();
            }
            self.output_buffer.extend_from_slice(bytes);
        }
    }

    fn drain_buffer(&mut self) {
        if !self.output_buffer.is_empty() {
            if !self.sink.write_bytes(&self.output_buffer) {
                self.write_errors += 1;
            }
            self.output_buffer.clear();
        }
    }

    fn emit_name(&mut self, name: &[u8]) {
        if self.case_fold {
            let lowered: Vec<u8> = name.iter().map(|byte| byte.to_ascii_lowercase()).collect();
            self.emit_bytes(&lowered);
        } else {
            self.emit_bytes(name);
        }
    }

    /// Resolves `AutoClose` styles for synthesized elements.
    fn element_style(&self, dom: &Dom, id: NodeId) -> ElementStyle {
        let style = dom.element_style(id);
        if style != ElementStyle::AutoClose {
            return style;
        }
        let keyword = dom.element_keyword(id);
        if crate::keywords::is_implicitly_closed_tag(keyword)
            || crate::keywords::is_optionally_closed_tag(keyword)
        {
            ElementStyle::ImplicitClose
        } else if crate::keywords::tag_allows_brief_termination(keyword) {
            ElementStyle::BriefClose
        } else {
            ElementStyle::ExplicitClose
        }
    }
}

impl<Sink: ByteSink> HtmlFilter for WriterFilter<Sink> {
    fn name(&self) -> &'static str {
        "HtmlWriter"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        true
    }

    fn can_modify_urls(&self) -> bool {
        false
    }

    fn script_usage(&self) -> ScriptUsage {
        ScriptUsage::NeverInjectsScripts
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.lazy_close_element = None;
        self.column = 0;
        self.write_errors = 0;
        self.output_buffer.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.element_style(dom, id) == ElementStyle::Invisible {
            return;
        }
        self.emit_bytes(b"<");
        let name = dom.element_name(id);
        self.emit_name(&name);
        let attributes = dom.element_attributes(id).clone();
        for attribute in &attributes {
            let mut attr_length = 1 + attribute.name_str().len();
            if self.max_column > 0 {
                if let Some(value) = attribute.escaped_value() {
                    attr_length += 1 + value.len();
                }
                if self.column + attr_length > self.max_column as usize {
                    self.emit_bytes(b"\n");
                }
            }
            self.emit_bytes(b" ");
            let attr_name = attribute.name().value().to_vec();
            self.emit_name(&attr_name);
            if let Some(value) = attribute.escaped_value() {
                self.emit_bytes(b"=");
                let quote = attribute.quote_style().as_str();
                self.emit_bytes(quote.as_bytes());
                self.emit_bytes(value);
                self.emit_bytes(quote.as_bytes());
            }
        }
        if self.element_style(dom, id) == ElementStyle::BriefClose {
            self.lazy_close_element = Some(id);
        } else {
            self.emit_bytes(b">");
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        match self.element_style(dom, id) {
            ElementStyle::AutoClose => {
                // GetElementStyle never returns AutoClose; unreachable.
            }
            ElementStyle::ImplicitClose | ElementStyle::Invisible | ElementStyle::Unclosed => {
                // The ">" went out at the start tag; nothing more to write.
            }
            ElementStyle::BriefClose => {
                if self.lazy_close_element == Some(id) {
                    self.lazy_close_element = None;
                    // Unquoted or valueless trailing attributes need a space
                    // so parsers don't eat the '/' as attribute text.
                    let attributes = dom.element_attributes(id);
                    if let Some(last) = attributes.last() {
                        if last.escaped_value().is_none()
                            || last.quote_style() == crate::element::QuoteStyle::NoQuote
                        {
                            self.emit_bytes(b" ");
                        }
                    }
                    self.emit_bytes(b"/>");
                } else {
                    // Content intervened: fall back to explicit close.
                    self.emit_bytes(b"</");
                    let name = dom.element_name(id);
                    self.emit_name(&name);
                    self.emit_bytes(b">");
                }
            }
            ElementStyle::ExplicitClose => {
                self.emit_bytes(b"</");
                let name = dom.element_name(id);
                self.emit_name(&name);
                self.emit_bytes(b">");
            }
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let contents = dom.leaf_contents(id);
        self.emit_bytes(&contents);
    }

    fn cdata(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.emit_bytes(b"<![CDATA[");
        let contents = dom.leaf_contents(id);
        self.emit_bytes(&contents);
        self.emit_bytes(b"]]>");
    }

    fn comment(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.emit_bytes(b"<!--");
        let contents = dom.leaf_contents(id);
        self.emit_bytes(&contents);
        self.emit_bytes(b"-->");
    }

    fn ie_directive(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.emit_bytes(b"<!--");
        let contents = dom.leaf_contents(id);
        self.emit_bytes(&contents);
        self.emit_bytes(b"-->");
    }

    fn directive(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.emit_bytes(b"<!");
        let contents = dom.leaf_contents(id);
        self.emit_bytes(&contents);
        self.emit_bytes(b">");
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // Empty emit flushes any lazily closed trailing element.
        self.emit_bytes(b"");
        self.drain_buffer();
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.drain_buffer();
        if !self.sink.flush_sink() {
            self.write_errors += 1;
        }
    }
}
