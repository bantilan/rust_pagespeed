//! Deferred-JS bootstrap injection (`JsDeferDisabledFilter`).
//!
//! Appends the outlined `js_defer` script at body end so `text/psajs`
//! scripts retagged by `JsDisable` execute in order. The UA verdict
//! (`SupportsJsDefer`) and the resolved asset URL (`GetAssetUrl`, which
//! selects the debug build under `kDebug` and the gstatic host when
//! configured) both come from the driver.

use super::common::BodyEndTracker;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::parser::Dom;

/// Driver inputs for defer bootstrapping.
#[derive(Debug, Clone, Default)]
pub struct JsDeferDisabledConfig {
    /// UA supports defer (`ShouldApply`).
    pub defer_applies: bool,
    /// Resolved `DEFER_JS` asset URL (`GetAssetUrl`).
    pub asset_url: Vec<u8>,
}

/// Appends the defer bootstrap (`JsDeferDisabledFilter`).
#[derive(Debug)]
pub struct JsDeferDisabledFilter {
    config: JsDeferDisabledConfig,
    body_end: BodyEndTracker,
}

impl JsDeferDisabledFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: JsDeferDisabledConfig) -> Self {
        JsDeferDisabledFilter {
            config,
            body_end: BodyEndTracker::default(),
        }
    }
}

impl HtmlFilter for JsDeferDisabledFilter {
    fn name(&self) -> &'static str {
        "JsDeferDisabled"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        self.config.defer_applies
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.body_end.clear();
    }

    fn start_element(&mut self, _dom: &mut Dom, _lexer: &Lexer, _id: NodeId) {
        self.body_end.clear();
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_characters(dom, id);
    }

    fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        if !self.config.defer_applies {
            return;
        }
        let script = dom.new_element(None, b"script");
        dom.add_attribute(script, b"type", b"text/javascript");
        dom.add_attribute(script, b"src", &self.config.asset_url);
        self.body_end.insert(dom, script);
    }
}
