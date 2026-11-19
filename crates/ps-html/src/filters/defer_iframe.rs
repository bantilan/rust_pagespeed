//! Iframe deferral (`DeferIframeFilter`).
//!
//! Renames `<iframe>` to `<pagespeed_iframe>` with a converter script,
//! plus one shared support script. Enablement (UA supports defer),
//! assets, and the `AddJsToElement` inputs all come from the driver.

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::filters::common::{add_js_to_element, XhtmlStatus};
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Support-script initializer (`kDeferIframeInit`).
pub const DEFER_IFRAME_INIT: &[u8] = b"pagespeed.deferIframeInit();";
/// Per-iframe converter script (`kDeferIframeIframeJs`).
pub const DEFER_IFRAME_IFRAME_JS: &[u8] =
    b"\nif(window.pagespeed){if(pagespeed.deferIframe){pagespeed.deferIframe.convertToIframe();}}";

/// Defers iframes past first paint.
#[derive(Debug)]
pub struct DeferIframeFilter {
    js: Vec<u8>,
    defer_supported: bool,
    pedantic: bool,
    xhtml: XhtmlStatus,
    script_inserted: bool,
    noscript_depth: u32,
}

impl DeferIframeFilter {
    /// Builds the filter (asset bytes, UA-defer support, `AddJsToElement`
    /// inputs).
    #[must_use]
    pub fn new(js: &[u8], defer_supported: bool, pedantic: bool, xhtml: XhtmlStatus) -> Self {
        DeferIframeFilter {
            js: js.to_vec(),
            defer_supported,
            pedantic,
            xhtml,
            script_inserted: false,
            noscript_depth: 0,
        }
    }
}

impl HtmlFilter for DeferIframeFilter {
    fn name(&self) -> &'static str {
        "DeferIframe"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        self.defer_supported
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.script_inserted = false;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        if self.noscript_depth > 0 || dom.element_keyword(id) != Keyword::Iframe {
            return;
        }
        if !self.script_inserted {
            let parent = dom.arena_node(id).parent();
            let script = dom.new_element(parent, b"script");
            dom.insert_node_before_node(id, script);
            let mut js = self.js.clone();
            js.extend_from_slice(DEFER_IFRAME_INIT);
            add_js_to_element(dom, lexer, script, &js, self.pedantic, self.xhtml);
            self.script_inserted = true;
        }
        dom.set_element_name(id, b"pagespeed_iframe");
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        if self.noscript_depth > 0 {
            return;
        }
        // The rename in StartElement already landed: match the new keyword.
        if dom.element_keyword(id) == Keyword::PagespeedIframe {
            let script = dom.new_element(Some(id), b"script");
            dom.add_attribute(script, b"type", b"text/javascript");
            let text = dom.new_leaf(
                Some(script),
                crate::element::LeafKind::Characters,
                DEFER_IFRAME_IFRAME_JS.to_vec(),
            );
            dom.append_child(Some(id), script);
            dom.append_child(Some(script), text);
        }
    }
}
