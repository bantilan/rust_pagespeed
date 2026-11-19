//! HTML4 type attributes (`PedanticFilter`).
//!
//! Adds `type="text/css"` to `<style>` and `type="text/javascript"` to
//! JavaScript `<script>` elements missing them, outside HTML5 documents.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};

/// Adds HTML4-required type attributes.
#[derive(Debug, Default)]
pub struct PedanticFilter;

impl HtmlFilter for PedanticFilter {
    fn name(&self) -> &'static str {
        "Pedantic"
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if lexer.doctype().is_version_5() {
            return;
        }
        let keyword = dom.element_keyword(id);
        if keyword == Keyword::Style && dom.find_attribute(id, Keyword::Type).is_none() {
            dom.add_attribute(id, b"type", b"text/css");
        }
        if keyword == Keyword::Script && dom.find_attribute(id, Keyword::Type).is_none() {
            // No type attribute: only tag JavaScript as such.
            let (classification, _) = ScriptTagScanner::parse_script_element(dom, id);
            if classification == ScriptClassification::JavaScript {
                dom.add_attribute(id, b"type", b"text/javascript");
            }
        }
    }
}
