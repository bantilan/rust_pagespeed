//! Size-limit redirect (`RedirectOnSizeLimitFilter`).
//!
//! Once the parser's size limit trips, injects `window.location` to the
//! document URL with `PageSpeed=off` at the first element outside
//! noscript — unless inline scripts are forbidden. The CSP verdict comes
//! from the driver; the document URL from the lexer.

use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Escapes text for a double-quoted JS string (`EscapeToJsStringLiteral`
/// with `add_quotes=false`).
fn escape_js_string(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let byte = text[index];
        let rest = &text[index..];
        match byte {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\'' => out.extend_from_slice(b"\\'"),
            b'<' if rest.starts_with(b"<!--")
                || (rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"<script")) =>
            {
                out.extend_from_slice(b"\\u003c");
            }
            b'-' if rest.starts_with(b"-->") => out.extend_from_slice(b"\\u002d"),
            b'/' if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"/script") => {
                out.extend_from_slice(b"\\/");
            }
            _ => out.push(byte),
        }
        index += 1;
    }
    out
}

/// Redirects oversized documents to themselves with rewriting off.
#[derive(Debug)]
pub struct RedirectOnSizeLimitFilter {
    csp_permits_inline_script: bool,
    redirect_inserted: bool,
    noscript_depth: u32,
}

impl RedirectOnSizeLimitFilter {
    /// Builds the filter (CSP inline-script verdict).
    #[must_use]
    pub fn new(csp_permits_inline_script: bool) -> Self {
        RedirectOnSizeLimitFilter {
            csp_permits_inline_script,
            redirect_inserted: false,
            noscript_depth: 0,
        }
    }

    fn insert_script_if_needed(
        &mut self,
        dom: &mut Dom,
        lexer: &Lexer,
        id: NodeId,
        is_start: bool,
    ) {
        if self.redirect_inserted
            || self.noscript_depth > 0
            || !lexer.size_limit_exceeded()
            || !self.csp_permits_inline_script
        {
            return;
        }
        let mut target = GoogleUrl::new(&String::from_utf8_lossy(lexer.document_url()));
        target = target.copy_and_add_query_param("PageSpeed", "off");
        let mut code = b"window.location=\"".to_vec();
        code.extend_from_slice(&escape_js_string(target.spec().as_bytes()));
        code.extend_from_slice(b"\";");
        let script = dom.new_element(Some(id), b"script");
        dom.add_attribute(script, b"type", b"text/javascript");
        let text = dom.new_leaf(Some(script), crate::element::LeafKind::Characters, code);
        if dom.element_keyword(id) == Keyword::Html {
            if is_start {
                dom.prepend_child(id, script);
            } else {
                dom.append_child(Some(id), script);
            }
        } else if is_start {
            dom.insert_node_before_node(id, script);
        } else {
            dom.insert_node_after_node(id, script);
        }
        dom.append_child(Some(script), text);
        self.redirect_inserted = true;
    }
}

impl HtmlFilter for RedirectOnSizeLimitFilter {
    fn name(&self) -> &'static str {
        "RedirectOnSizeLimit"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.redirect_inserted = false;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        self.insert_script_if_needed(dom, lexer, id, true);
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        self.insert_script_if_needed(dom, lexer, id, false);
    }
}
