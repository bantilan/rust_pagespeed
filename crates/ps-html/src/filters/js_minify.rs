//! Inline `<script>` minification (`JavascriptFilter` minify subset).
//!
//! Ports the inline half of C++ `JavascriptFilter`: character
//! data inside a JavaScript `<script>` without `src` runs
//! through the JS minifier and is replaced on success.
//! External scripts, library identification, source maps,
//! CSP gating, and XHTML CDATA-marker checks stay out (the
//! full filter needs fetch machinery and driver channels).
//! The end-tag guard below is verbatim: minification must
//! never change where an HTML parser ends the element.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::keywords::is_sometimes_literal_tag;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};

/// End tags of elements whose content reads as text (a script
/// inside one would end it there).
const TEXT_ELEMENT_END_TAGS: &[&[u8]] = &[
    b"</script",
    b"</noscript",
    b"</noembed",
    b"</noframes",
    b"</textarea",
    b"</title",
    b"</style",
    b"</iframe",
    b"</xmp",
];

/// Whether text holds a text-element end tag
/// (`HasTextElementEndTag`).
fn has_text_element_end_tag(text: &[u8]) -> bool {
    if !text.windows(2).any(|pair| pair == b"</") {
        return false;
    }
    TEXT_ELEMENT_END_TAGS.iter().any(|tag| {
        text.windows(tag.len())
            .any(|window| window.eq_ignore_ascii_case(tag))
    })
}

/// Whether minification could move the element end
/// (`MinifiedScriptMayEndElement`).
fn minified_may_end_element(original: &[u8], minified: &[u8]) -> bool {
    if minified == original {
        return false;
    }
    if has_text_element_end_tag(minified) {
        return true;
    }
    let has_comment_open = minified.windows(4).any(|w| w == b"<!--");
    let has_script_open = minified
        .windows(7)
        .any(|w| w.eq_ignore_ascii_case(b"<script"));
    has_comment_open && has_script_open
}

/// Minifies inline scripts.
pub struct JsMinifyFilter {
    /// Inside a minifiable inline script.
    in_script: bool,
    /// Open sometimes-literal elements (scripts inside them
    /// read as text and must not change).
    literal_stack: Vec<Keyword>,
    /// Shared tokenizer patterns.
    patterns: &'static ps_js::tokenizer::TokenizerPatterns,
}

impl Default for JsMinifyFilter {
    fn default() -> Self {
        JsMinifyFilter {
            in_script: false,
            literal_stack: Vec::new(),
            patterns: ps_js::tokenizer::TokenizerPatterns::shared(),
        }
    }
}

impl HtmlFilter for JsMinifyFilter {
    fn name(&self) -> &'static str {
        "JsMinify"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.in_script = false;
        self.literal_stack.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if is_sometimes_literal_tag(keyword) {
            self.literal_stack.push(keyword);
        }
        if keyword != Keyword::Script {
            return;
        }
        let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
        if classification == ScriptClassification::JavaScript && src.is_none() {
            self.in_script = true;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if self.literal_stack.last() == Some(&keyword) {
            self.literal_stack.pop();
        }
        if keyword == Keyword::Script {
            self.in_script = false;
        }
    }

    fn characters(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if !self.in_script || !self.literal_stack.is_empty() || lexer.passing_through() {
            return;
        }
        let contents = dom.leaf_contents_mut(id);
        let (minified, success) = ps_js::minify::minify_utf8(&self.patterns, contents);
        if success && !minified_may_end_element(contents, &minified) {
            *contents = minified;
        }
    }
}
