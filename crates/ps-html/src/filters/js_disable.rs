//! JavaScript disabling (`JsDisableFilter`).
//!
//! Retypes classic scripts to inert `text/psajs` (or `text/prioritypsajs`)
//! with stable indices, rewrites `onload` handlers through a marker, adds
//! the IE compatibility meta, and optionally appends the experimental
//! defer script. UA verdicts, base URL, allowlist, and option flags all
//! come from the driver; log records stay driver-side.

use ps_config::wildcard::WildcardGroup;
use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::{HtmlName, Keyword};
use crate::parser::Dom;
use crate::script_scanner::{ScriptClassification, ScriptTagScanner};

use super::common::BodyEndTracker;

/// Experimental defer bootstrap (`kEnableJsExperimental`).
pub const ENABLE_JS_EXPERIMENTAL: &[u8] =
    b"window.pagespeed = window.pagespeed || {};window.pagespeed.defer_js_experimental=true;";
/// Element onload shim (`kElementOnloadCode`).
pub const ELEMENT_ONLOAD_CODE: &[u8] =
    b"var elem=this;if (this==window) elem=document.body;elem.setAttribute('data-pagespeed-loaded', 1)";

/// Driver inputs for JS disabling.
#[derive(Debug, Clone, Default)]
pub struct JsDisableConfig {
    /// Document base URL (resolves script sources).
    pub base_url: Vec<u8>,
    /// Allow/disallow patterns (`IsAllowed`, default allow).
    pub allow: WildcardGroup,
    /// UA supports defer (`ShouldApply`).
    pub defer_applies: bool,
    /// UA is IE (compatibility meta).
    pub is_ie: bool,
    /// `enable_defer_js_experimental`.
    pub defer_js_experimental: bool,
    /// `enable_prioritizing_scripts`.
    pub prioritizing_scripts: bool,
}

/// Disables JavaScript in place for later deferral.
#[derive(Debug)]
pub struct JsDisableFilter {
    config: JsDisableConfig,
    base_url: GoogleUrl,
    index: u32,
    ie_meta_tag_written: bool,
    body_end: BodyEndTracker,
}

impl JsDisableFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: JsDisableConfig) -> Self {
        let base_url = GoogleUrl::new(&String::from_utf8_lossy(&config.base_url));
        JsDisableFilter {
            config,
            base_url,
            index: 0,
            ie_meta_tag_written: false,
            body_end: BodyEndTracker::default(),
        }
    }

    /// Prepends the IE compatibility meta once (`InsertMetaTagForIE`).
    fn insert_meta_tag_for_ie(&mut self, dom: &mut Dom, id: NodeId) {
        if self.ie_meta_tag_written {
            return;
        }
        self.ie_meta_tag_written = true;
        if !self.config.is_ie {
            return;
        }
        let head = if dom.element_keyword(id) == Keyword::Head {
            id
        } else {
            let parent = dom.arena_node(id).parent();
            let head = dom.new_element(parent, b"head");
            dom.insert_node_before_current(head);
            head
        };
        let meta = dom.new_element(Some(head), b"meta");
        dom.add_attribute(meta, b"http-equiv", b"X-UA-Compatible");
        dom.add_attribute(meta, b"content", b"IE=edge");
        dom.prepend_child(head, meta);
    }

    /// Appends the experimental defer script (`InsertJsDeferExperimentalScript`).
    fn insert_experimental_script(&mut self, dom: &mut Dom) {
        if !self.config.defer_js_experimental {
            return;
        }
        let script = dom.new_element(None, b"script");
        dom.add_attribute(script, b"type", b"text/javascript");
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        let text = dom.new_leaf(
            Some(script),
            crate::element::LeafKind::Characters,
            ENABLE_JS_EXPERIMENTAL.to_vec(),
        );
        self.body_end.insert(dom, script);
        dom.append_child(Some(script), text);
    }
}

impl HtmlFilter for JsDisableFilter {
    fn name(&self) -> &'static str {
        "JsDisableFilter"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        self.config.defer_applies
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.index = 0;
        self.ie_meta_tag_written = false;
        self.body_end.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.clear();
        match dom.element_keyword(id) {
            Keyword::Head | Keyword::Body if !self.ie_meta_tag_written => {
                self.insert_meta_tag_for_ie(dom, id);
            }
            Keyword::Head | Keyword::Body => {}
            _ => {
                let (classification, src) = ScriptTagScanner::parse_script_element(dom, id);
                if classification == ScriptClassification::JavaScript {
                    if dom
                        .find_attribute(id, Keyword::DataPagespeedNoDefer)
                        .is_some()
                        || dom.find_attribute(id, Keyword::PagespeedNoDefer).is_some()
                    {
                        return;
                    }
                    // Honor disallow: external sources outside the allowlist stay.
                    if let Some(src) = src {
                        let (decoded, _) = dom.decoded_attribute(id, src);
                        if let Some(url) = decoded {
                            let resolved =
                                GoogleUrl::resolve(&self.base_url, &String::from_utf8_lossy(&url));
                            if resolved.is_web_valid()
                                && !self.config.allow.is_match(resolved.spec(), true)
                            {
                                return;
                            }
                        }
                    }
                    // Rename the first type attribute, drop the rest, then tag.
                    if let Some(first) = dom.find_attribute(id, Keyword::Type) {
                        dom.element_attributes_mut(id)[first]
                            .set_name(HtmlName::new(b"data-pagespeed-orig-type"));
                    }
                    while dom.delete_attribute(id, Keyword::Type) {}
                    let prioritize = dom
                        .find_attribute(id, Keyword::DataPagespeedPrioritize)
                        .is_some()
                        && self.config.prioritizing_scripts;
                    dom.add_attribute(
                        id,
                        b"type",
                        if prioritize {
                            b"text/prioritypsajs".as_slice()
                        } else {
                            b"text/psajs".as_slice()
                        },
                    );
                    let index = self.index.to_string();
                    self.index += 1;
                    dom.add_attribute(id, b"data-pagespeed-orig-index", index.as_bytes());
                }
            }
        }
        if let Some(onload) = dom.find_attribute(id, Keyword::Onload) {
            dom.element_attributes_mut(id)[onload]
                .set_name(HtmlName::new(b"data-pagespeed-onload"));
            dom.add_escaped_attribute(
                id,
                b"onload",
                Some(ELEMENT_ONLOAD_CODE),
                crate::element::QuoteStyle::DoubleQuote,
            );
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_characters(dom, id);
    }

    fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        self.insert_experimental_script(dom);
    }
}
