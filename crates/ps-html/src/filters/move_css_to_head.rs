//! Stylesheet hoisting (`CssMoveToHeadFilter`).
//!
//! Moves `<style>` elements and stylesheet `<link>`s to the end of `<head>`
//! (or above the first `<script>`), keeping their relative order. Styles
//! inside `<noscript>` and `<style scoped>` stay put and bar further moves.

use crate::css_tag_scanner;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Moves styles to head or above scripts.
#[derive(Debug)]
pub struct MoveCssToHeadFilter {
    move_to_head: bool,
    move_above_scripts: bool,
    anchor: Option<(NodeId, bool)>,
    noscript_depth: u32,
    moved: u32,
}

impl MoveCssToHeadFilter {
    /// Builds the filter (`kMoveCssToHead`/`kMoveCssAboveScripts` flags).
    #[must_use]
    pub fn new(move_to_head: bool, move_above_scripts: bool) -> Self {
        MoveCssToHeadFilter {
            move_to_head,
            move_above_scripts,
            anchor: None,
            noscript_depth: 0,
            moved: 0,
        }
    }

    /// Styles moved so far (`css_elements_moved`).
    #[must_use]
    pub fn moved_count(&self) -> u32 {
        self.moved
    }
}

impl HtmlFilter for MoveCssToHeadFilter {
    fn name(&self) -> &'static str {
        "CssMoveToHead"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.anchor = None;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        // CommonFilter clears noscript tracking before the impl runs.
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        if self.anchor.is_none() {
            // The first head end (or script end) anchors all later moves.
            if self.move_to_head && dom.element_keyword(id) == Keyword::Head {
                self.anchor = Some((id, true));
            } else if self.move_above_scripts && dom.element_keyword(id) == Keyword::Script {
                self.anchor = Some((id, false));
            }
        } else if dom.element_keyword(id) == Keyword::Style
            || css_tag_scanner::parse_css_element(dom, id).is_some()
        {
            if self.noscript_depth > 0
                || (dom.element_keyword(id) == Keyword::Style
                    && dom.find_attribute(id, Keyword::Scoped).is_some())
            {
                // Noscript content and scoped styles stay — and bar
                // everything after them from moving.
                self.anchor = None;
            } else {
                self.moved += 1;
                let (anchor, is_head) = self.anchor.unwrap();
                if is_head {
                    dom.move_current_into(anchor);
                } else {
                    dom.move_current_before(anchor);
                }
            }
        }
    }
}
