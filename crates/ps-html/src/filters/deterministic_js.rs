//! Deterministic-JS injection (`DeterministicJsFilter`).
//!
//! Prepends the deterministic-date/random support script to `<head>` so
//! downstream passes observe stable `Date`/`Math.random`. The JS bytes,
//! Pedantic flag, and XHTML status all come from the driver.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::filters::common::{add_js_to_element, XhtmlStatus};
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Injects the deterministic-JS support script into head.
#[derive(Debug)]
pub struct DeterministicJsFilter {
    js: Vec<u8>,
    pedantic: bool,
    xhtml: XhtmlStatus,
}

impl DeterministicJsFilter {
    /// Builds the filter (asset bytes plus the `AddJsToElement` inputs).
    #[must_use]
    pub fn new(js: &[u8], pedantic: bool, xhtml: XhtmlStatus) -> Self {
        DeterministicJsFilter {
            js: js.to_vec(),
            pedantic,
            xhtml,
        }
    }
}

impl HtmlFilter for DeterministicJsFilter {
    fn name(&self) -> &'static str {
        "DeterministicJs"
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Head {
            return;
        }
        let script = dom.new_element(Some(id), b"script");
        dom.insert_node_after_current(script);
        add_js_to_element(dom, lexer, script, &self.js, self.pedantic, self.xhtml);
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
    }
}
