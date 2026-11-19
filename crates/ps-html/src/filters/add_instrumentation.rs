//! RUM instrumentation injection (`AddInstrumentationFilter`).
//!
//! Inserts a timing bootstrap at the top of `<head>` (after any title or
//! meta), an unload beacon script at head end when unload reporting is on,
//! and a load beacon script at body end. Beacon URLs, experiment and
//! timing inputs, response status, request id, XHTML certainty, and the
//! bot verdict all come from the driver; the page URL reads from the lexer
//! at insertion time.

use super::common::{add_js_to_element, BodyEndTracker, XhtmlStatus};
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::js_escape::escape_to_js_string_literal;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::static_assets::{get_asset, StaticAsset};

/// Head bootstrap, pedantic spelling (`kHeadScriptPedantic`).
const HEAD_SCRIPT_PEDANTIC: &[u8] =
    b"<script type='text/javascript'>window.mod_pagespeed_start = Number(new Date());</script>";

/// Head bootstrap, HTML5 spelling (`kHeadScriptNonPedantic`).
const HEAD_SCRIPT_NON_PEDANTIC: &[u8] =
    b"<script>window.mod_pagespeed_start = Number(new Date());</script>";

/// Driver inputs for instrumentation injection.
#[derive(Debug, Clone, Default)]
pub struct AddInstrumentationConfig {
    /// Client is a bot (disables the filter).
    pub is_bot: bool,
    /// `kPedantic` (type attributes + pedantic head script).
    pub pedantic: bool,
    /// `kDebug` (debug asset builds).
    pub debug: bool,
    /// `report_unload_time`.
    pub report_unload_time: bool,
    /// `enable_extended_instrumentation`.
    pub enable_extended_instrumentation: bool,
    /// `defer_instrumentation_script` (drops the no-defer marker).
    pub defer_instrumentation_script: bool,
    /// Beacon endpoint for HTTP pages.
    pub beacon_http: Vec<u8>,
    /// Beacon endpoint for HTTPS pages.
    pub beacon_https: Vec<u8>,
    /// Page was fetched over HTTPS.
    pub is_https: bool,
    /// Running experiment id (pre-gated: set and nonzero).
    pub experiment_id: Option<i64>,
    /// Header-fetch latency ms (`hft`).
    pub header_fetch_ms: Option<i64>,
    /// Fetch latency ms (`ft`).
    pub fetch_ms: Option<i64>,
    /// Time to first byte ms (`s_ttfb`).
    pub ttfb_ms: Option<i64>,
    /// Response status code (`rc`, unless 200).
    pub status_code: Option<i32>,
    /// Request id (`id`, when positive).
    pub request_id: Option<i64>,
    /// Response mimetype XHTML certainty.
    pub xhtml: XhtmlStatus,
}

/// Beacon init event (`kLoadTag` / `kUnloadTag` select the JS event name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeaconEvent {
    Load,
    Unload,
}

/// Injects RUM instrumentation (`AddInstrumentationFilter`).
#[derive(Debug)]
pub struct AddInstrumentationFilter {
    config: AddInstrumentationConfig,
    found_head: bool,
    added_head_script: bool,
    added_unload_script: bool,
    body_end: BodyEndTracker,
    script_added_count: i64,
}

impl AddInstrumentationFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: AddInstrumentationConfig) -> Self {
        AddInstrumentationFilter {
            config,
            found_head: false,
            added_head_script: false,
            added_unload_script: false,
            body_end: BodyEndTracker::default(),
            script_added_count: 0,
        }
    }

    /// Head-script insertions (`instrumentation_filter_script_added_count`).
    #[must_use]
    pub fn script_added_count(&self) -> i64 {
        self.script_added_count
    }

    /// Inserts the head bootstrap unless the current element is title or
    /// meta (`AddHeadScript`).
    fn add_head_script(&mut self, dom: &mut Dom, keyword: Keyword) {
        if keyword == Keyword::Title || keyword == Keyword::Meta {
            return;
        }
        self.added_head_script = true;
        let markup = if self.config.pedantic {
            HEAD_SCRIPT_PEDANTIC
        } else {
            HEAD_SCRIPT_NON_PEDANTIC
        };
        let leaf = dom.new_leaf(None, LeafKind::Characters, markup.to_vec());
        dom.insert_node_before_current(leaf);
        self.script_added_count += 1;
    }

    /// Builds a beacon script body (`GetScriptJs`): static assets once,
    /// then the `addInstrumentationInit` call.
    fn script_js(&self, lexer: &Lexer, event: BeaconEvent) -> Vec<u8> {
        let mut js = Vec::new();
        // Only add the static JS once (the first call inlines it).
        if !self.added_unload_script {
            if self.config.enable_extended_instrumentation {
                js.extend_from_slice(get_asset(
                    StaticAsset::ExtendedInstrumentation,
                    self.config.debug,
                ));
            }
            js.extend_from_slice(get_asset(
                StaticAsset::AddInstrumentation,
                self.config.debug,
            ));
        }
        let js_event = match event {
            BeaconEvent::Load => "load",
            BeaconEvent::Unload => "beforeunload",
        };
        let beacon = if self.config.is_https {
            &self.config.beacon_https
        } else {
            &self.config.beacon_http
        };
        let mut params = Vec::new();
        if let Some(id) = self.config.experiment_id {
            params.extend_from_slice(format!("&exptid={id}").as_bytes());
        }
        if let Some(ms) = self.config.header_fetch_ms {
            params.extend_from_slice(format!("&hft={ms}").as_bytes());
        }
        if let Some(ms) = self.config.fetch_ms {
            params.extend_from_slice(format!("&ft={ms}").as_bytes());
        }
        if let Some(ms) = self.config.ttfb_ms {
            params.extend_from_slice(format!("&s_ttfb={ms}").as_bytes());
        }
        if let Some(code) = self.config.status_code {
            if code > 0 && code != 200 {
                params.extend_from_slice(format!("&rc={code}").as_bytes());
            }
        }
        if let Some(id) = self.config.request_id {
            if id > 0 {
                params.extend_from_slice(format!("&id={id}").as_bytes());
            }
        }
        let page_url = escape_to_js_string_literal(lexer.document_url(), false);
        let beacon_url = escape_to_js_string_literal(beacon, false);
        js.extend_from_slice(b"\npagespeed.addInstrumentationInit('");
        js.extend_from_slice(&beacon_url);
        js.extend_from_slice(b"', '");
        js.extend_from_slice(js_event.as_bytes());
        js.extend_from_slice(b"', '");
        js.extend_from_slice(&params);
        js.extend_from_slice(b"', '");
        js.extend_from_slice(&page_url);
        js.extend_from_slice(b"');");
        js
    }

    /// Builds one beacon script element (no-defer marker unless deferred).
    fn new_beacon_script(&self, dom: &mut Dom, parent: Option<NodeId>) -> NodeId {
        let script = dom.new_element(parent, b"script");
        if !self.config.defer_instrumentation_script {
            dom.add_escaped_attribute(
                script,
                b"data-pagespeed-no-defer",
                None,
                crate::element::QuoteStyle::DoubleQuote,
            );
        }
        script
    }
}

impl HtmlFilter for AddInstrumentationFilter {
    fn name(&self) -> &'static str {
        "AddInstrumentation"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        !self.config.is_bot
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.found_head = false;
        self.added_head_script = false;
        self.added_unload_script = false;
        self.body_end.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.clear();
        let keyword = dom.element_keyword(id);
        if self.found_head && !self.added_head_script {
            self.add_head_script(dom, keyword);
        }
        if !self.found_head && keyword == Keyword::Head {
            self.found_head = true;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
        if self.found_head && dom.element_keyword(id) == Keyword::Head {
            if !self.added_head_script {
                self.add_head_script(dom, Keyword::Head);
            }
            if self.config.report_unload_time && !self.added_unload_script {
                let js = self.script_js(lexer, BeaconEvent::Unload);
                let script = self.new_beacon_script(dom, Some(id));
                dom.insert_node_before_current(script);
                add_js_to_element(
                    dom,
                    lexer,
                    script,
                    &js,
                    self.config.pedantic,
                    self.config.xhtml,
                );
                self.added_unload_script = true;
            }
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_characters(dom, id);
    }

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        // The head filter chain guarantees a head element; without one
        // there is nowhere sensible to anchor (C++ logs a warning).
        if !self.found_head {
            return;
        }
        let js = self.script_js(lexer, BeaconEvent::Load);
        let script = self.new_beacon_script(dom, None);
        self.body_end.insert(dom, script);
        add_js_to_element(
            dom,
            lexer,
            script,
            &js,
            self.config.pedantic,
            self.config.xhtml,
        );
    }
}
