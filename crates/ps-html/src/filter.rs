//! Filter interface: the callbacks the engine drives per event.
//!
//! Ports `HtmlFilter`. All event methods default to no-ops; filters
//! override what they need and mutate through [`Dom`](crate::parser::Dom).

use crate::element::NodeId;
use crate::lexer::Lexer;
use crate::parser::Dom;

/// Script-injection classification (`HtmlFilter::ScriptUsage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptUsage {
    NeverInjectsScripts,
    PotentiallyInjectsScripts,
}

/// An HTML filter: observes events and mutates the queue/DOM between flushes.
pub trait HtmlFilter {
    /// Filter name for logging and order goldens.
    fn name(&self) -> &'static str;

    /// Decides whether this filter runs for the current document/options.
    /// Called before any events flow; the parser caches the result.
    fn determine_enabled(&mut self, _dom: &Dom, lexer: &Lexer) -> bool {
        true
    }

    /// Why the filter disabled itself (`disabled_reason`, appended to the
    /// dynamically-disabled list as `name: reason` when present).
    fn disabled_reason(&self) -> Option<String> {
        None
    }

    /// Parse starting (`InitParse`): reset stopwatch state. Fires on every
    /// filter; only timing filters listen.
    fn init_parse(&mut self) {}

    /// A text chunk is about to lex (`StartParse`).
    fn start_parse(&mut self) {}

    /// A text chunk finished lexing (`EndParse`).
    fn end_parse(&mut self) {}

    /// A flush is about to render (`StartRender`), before any phase runs.
    fn start_render(&mut self) {}

    /// True when this filter may change URLs (drives URL-trim decisions).
    fn can_modify_urls(&self) -> bool {
        true
    }

    /// Script-injection classification.
    fn script_usage(&self) -> ScriptUsage {
        ScriptUsage::PotentiallyInjectsScripts
    }

    /// New document: clear per-document state (instances are reused).
    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {}
    /// End of document, immediately before the last flush.
    fn end_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {}
    /// Element start tag seen.
    fn start_element(&mut self, _dom: &mut Dom, lexer: &Lexer, _element: NodeId) {}
    /// Element end tag seen.
    fn end_element(&mut self, _dom: &mut Dom, lexer: &Lexer, _element: NodeId) {}
    /// CDATA section.
    fn cdata(&mut self, _dom: &mut Dom, lexer: &Lexer, _node: NodeId) {}
    /// HTML comment (not an IE directive).
    fn comment(&mut self, _dom: &mut Dom, lexer: &Lexer, _node: NodeId) {}
    /// IE conditional directive.
    fn ie_directive(&mut self, _dom: &mut Dom, lexer: &Lexer, _node: NodeId) {}
    /// Raw characters between tags.
    fn characters(&mut self, _dom: &mut Dom, lexer: &Lexer, _node: NodeId) {}
    /// HTML directive (`<!...>`).
    fn directive(&mut self, _dom: &mut Dom, lexer: &Lexer, _node: NodeId) {}
    /// A flush is occurring; streamed-output filters emit now.
    fn flush(&mut self, _dom: &mut Dom, lexer: &Lexer) {}
    /// All pre-render filtering done; rendering follows.
    fn render_done(&mut self, _dom: &mut Dom, lexer: &Lexer) {}
}
