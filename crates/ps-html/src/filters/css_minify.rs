//! Inline `<style>` minification (`CssFilter` minify subset).
//!
//! Ports the inline half of C++ `CssFilter`: character data
//! inside `<style>` runs through the CSS minifier and is
//! replaced on success. External stylesheets, URL rewriting,
//! combining, CSP gating, and size-cap logging stay out (the
//! full filter needs fetch machinery); failures keep the
//! original text, like C++.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Minifies inline style blocks.
#[derive(Debug, Default)]
pub struct CssMinifyFilter {
    /// Inside a `<style>` element.
    in_style: bool,
}

impl HtmlFilter for CssMinifyFilter {
    fn name(&self) -> &'static str {
        "CssMinify"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.in_style = false;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Style {
            self.in_style = true;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Style {
            self.in_style = false;
        }
    }

    fn characters(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if !self.in_style || lexer.passing_through() {
            return;
        }
        let contents = dom.leaf_contents_mut(id);
        if let Some(minified) = ps_css::minify::minify_css(contents) {
            *contents = minified;
        }
    }
}
