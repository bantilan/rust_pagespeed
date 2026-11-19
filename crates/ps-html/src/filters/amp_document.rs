//! AMP detection (`AmpDocumentFilter`).
//!
//! Watches the document head: `<html amp>` (or the ⚡ spelling) first
//! declares AMP; any other element, non-whitespace text, 64KB of buffered
//! trivia, or end of input first declares not-AMP. A late `<html amp>`
//! is kept but flagged with a warning comment. The verdict fires exactly
//! once through the discovery callback.

use std::fmt;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// UTF-8 lightning bolt (`kUtf8LightningBolt`, U+26A1).
pub const UTF8_LIGHTNING_BOLT: &[u8] = "⚡".as_bytes();

/// Late-AMP warning comment (`kInvalidAmpDirectiveComment`).
pub const INVALID_AMP_DIRECTIVE_COMMENT: &[u8] =
    b"Invalid AMP directive -- will be ignored and will probably be rejected by the AMP validator.";

/// Trivia budget before giving up (`kMaxBufferedBytes`).
const MAX_BUFFERED_BYTES: usize = 64 * 1024;

/// Per-event overhead counted toward the budget.
const PER_EVENT_OVERHEAD: usize = 32;

/// AMP verdict listener (`BoolCallback`).
pub type AmpDiscoveredCallback = Box<dyn FnMut(bool)>;

/// Detects self-declared AMP documents.
pub struct AmpDocumentFilter {
    is_known: bool,
    saw_doctype: bool,
    buffered_bytes: usize,
    discovered: AmpDiscoveredCallback,
}

impl fmt::Debug for AmpDocumentFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AmpDocumentFilter")
            .field("is_known", &self.is_known)
            .field("saw_doctype", &self.saw_doctype)
            .field("buffered_bytes", &self.buffered_bytes)
            .finish_non_exhaustive()
    }
}

impl AmpDocumentFilter {
    /// Builds the detector with a discovery callback (fires exactly once).
    pub fn new(discovered: impl FnMut(bool) + 'static) -> Self {
        AmpDocumentFilter {
            is_known: false,
            saw_doctype: false,
            buffered_bytes: 0,
            discovered: Box::new(discovered),
        }
    }

    fn declare(&mut self, is_amp: bool) {
        debug_assert!(!self.is_known);
        (self.discovered)(is_amp);
        self.is_known = true;
    }

    fn note_undecided_event(&mut self, bytes: usize) {
        if self.is_known {
            return;
        }
        self.buffered_bytes += bytes + PER_EVENT_OVERHEAD;
        if self.buffered_bytes > MAX_BUFFERED_BYTES {
            self.declare(false);
        }
    }

    fn note_leaf(&mut self, dom: &Dom, id: NodeId) {
        self.note_undecided_event(dom.leaf_contents(id).len());
    }
}

impl HtmlFilter for AmpDocumentFilter {
    fn name(&self) -> &'static str {
        "AmpDocumentFilter"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.is_known = false;
        self.saw_doctype = false;
        self.buffered_bytes = 0;
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        if !self.is_known {
            self.declare(false);
        }
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        let mut declare_not_amp = !self.is_known;
        if dom.element_keyword(id) == Keyword::Html {
            let amp = dom
                .find_attribute(id, Keyword::Amp)
                .or_else(|| dom.find_attribute_by_name(id, UTF8_LIGHTNING_BOLT));
            if amp.is_some() {
                declare_not_amp = false;
                if !self.is_known {
                    self.declare(true);
                } else {
                    // Decided already: keep the tag but warn it is ignored.
                    dom.insert_comment(INVALID_AMP_DIRECTIVE_COMMENT, lexer.parent());
                }
            }
        } else if !self.saw_doctype && dom.element_name(id).eq_ignore_ascii_case(b"!doctype") {
            // Only reachable in lexers that surface doctype as an element;
            // the streaming lexer emits directives as leaves instead.
            self.saw_doctype = true;
            declare_not_amp = false;
        }
        if declare_not_amp {
            self.declare(false);
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if !self.is_known {
            let blank = dom
                .leaf_contents(id)
                .iter()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | 0x0c | b'\r'));
            if !blank {
                self.declare(false);
            }
        }
    }

    fn comment(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.note_leaf(dom, id);
    }

    fn cdata(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.note_leaf(dom, id);
    }

    fn directive(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.note_leaf(dom, id);
    }

    fn ie_directive(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.note_leaf(dom, id);
    }
}
