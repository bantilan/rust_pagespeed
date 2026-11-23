//! Google Analytics snippet insertion (`InsertGAFilter`).
//!
//! Inserts the GA tracking snippet (ga.js, analytics.js, or gtag.js by
//! account shape and options) at body end when the page has none, and
//! splices experiment reporting into existing snippets: custom-variable
//! preamble for ga.js, content-experiment calls for ga.js (via a
//! synchronous `cx/api.js` load) and analytics.js (before the pageview
//! send). Existing-snippet detection is substring-based with a tokenizer
//! state machine for the analytics.js rewrite. Account id, experiment
//! inputs, and flags all come from the driver.

use ps_js::keywords::TokenType;
use ps_js::tokenizer::{Tokenizer, TokenizerPatterns};

use super::common::BodyEndTracker;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Custom-variable experiment preamble (`kGAExperimentSnippet`).
const GA_EXPERIMENT_HEAD: &[u8] = b"var _gaq = _gaq || [];";
/// Custom-variable push, split around slot and spec.
const GA_EXPERIMENT_PUSH_HEAD: &[u8] = b"_gaq.push(['_setCustomVar', ";
/// Custom-variable push middle.
const GA_EXPERIMENT_PUSH_MID: &[u8] = b", 'ExperimentState', '";
/// Custom-variable push tail.
const GA_EXPERIMENT_PUSH_TAIL: &[u8] = b"']);";

/// ga.js async snippet around account, domain, speed tracking.
const GA_JS_HEAD: &[u8] =
    b"if (window.parent == window) {var _gaq = _gaq || [];_gaq.push(['_setAccount', '";
/// ga.js snippet domain part.
const GA_JS_DOMAIN_HEAD: &[u8] = b"']);_gaq.push(['_setDomainName', '";
/// ga.js snippet tail around speed tracking.
const GA_JS_TAIL_HEAD: &[u8] = b"']);_gaq.push(['_setAllowLinker', true]);";
/// ga.js snippet closer.
const GA_JS_TAIL: &[u8] = b"_gaq.push(['_trackPageview']);(function() {var ga = document.createElement('script'); ga.type = 'text/javascript';ga.async = true;ga.src = 'https://ssl.google-analytics.com/ga.js';var s = document.getElementsByTagName('script')[0];s.parentNode.insertBefore(ga, s);})();}";

/// analytics.js snippet around account, speed tracking, experiment.
const ANALYTICS_JS_HEAD: &[u8] = b"if (window.parent == window) {(function(i,s,o,g,r,a,m){i['GoogleAnalyticsObject']=r;i[r]=i[r]||function(){(i[r].q=i[r].q||[]).push(arguments)},i[r].l=1*new Date();a=s.createElement(o), m=s.getElementsByTagName(o)[0];a.async=1;a.src=g;m.parentNode.insertBefore(a,m)})(window,document,'script','//www.google-analytics.com/analytics.js','ga');ga('create', '";
/// analytics.js `create` tail around speed tracking.
const ANALYTICS_JS_CREATE_TAIL: &[u8] = b"', 'auto'";
/// analytics.js closer.
const ANALYTICS_JS_TAIL: &[u8] = b";";

/// gtag.js snippet around the account id (twice).
const GTAG_HEAD: &[u8] = b"if (window.parent == window) {window.dataLayer = window.dataLayer || [];function gtag(){dataLayer.push(arguments);}gtag('js', new Date());gtag('config', '";
/// gtag.js loader around the account id.
const GTAG_LOADER_HEAD: &[u8] = b"');(function(){var s = document.createElement('script');s.async = true;s.src = 'https://www.googletagmanager.com/gtag/js?id=";
/// gtag.js closer.
const GTAG_TAIL: &[u8] = b"';var f = document.getElementsByTagName('script')[0];if (f && f.parentNode) {f.parentNode.insertBefore(s, f);} else {(document.head || document.documentElement).appendChild(s);}})();}";

/// analytics.js speed tracking, bare and inside a fields object.
const ANALYTICS_SPEED_TRACKING: &[u8] = b", {'siteSpeedSampleRate': 100}";
/// analytics.js speed tracking, fields-object form.
const ANALYTICS_SPEED_TRACKING_MINIMAL: &[u8] = b"'siteSpeedSampleRate': 100,";
/// Content-experiments client library.
const CX_API_URL: &[u8] = b"//www.google-analytics.com/cx/api.js";
/// ga.js content-experiment call around variant id and experiment id.
const CX_SET_CHOSEN_VARIATION_HEAD: &[u8] = b"cxApi.setChosenVariation(";
/// Chosen-variation call middle.
const CX_SET_CHOSEN_VARIATION_MID: &[u8] = b", '";
/// Chosen-variation call tail.
const CX_SET_CHOSEN_VARIATION_TAIL: &[u8] = b"');";
/// Non-numeric variant warning around the variant id.
const CX_NON_NUMERIC_HEAD: &[u8] = b"\n/* mod_pagespeed cannot inject experiment variant '";
/// Non-numeric variant warning tail.
const CX_NON_NUMERIC_TAIL: &[u8] = b" because it's not a number */\n";
/// analytics.js content-experiment calls around experiment and variant ids.
const CX_SET_EXP_HEAD: &[u8] = b"ga('set', 'expId', '";
/// analytics.js experiment middle.
const CX_SET_EXP_MID: &[u8] = b"');ga('set', 'expVar', '";
/// analytics.js experiment tail.
const CX_SET_EXP_TAIL: &[u8] = b"');";
/// ga.js speed tracking push.
const GA_SPEED_TRACKING: &[u8] = b"_gaq.push(['_setSiteSpeedSampleRate', 100]);";

/// Driver inputs for GA insertion.
#[derive(Debug, Clone, Default)]
pub struct InsertGAConfig {
    /// GA account id (empty disables).
    pub ga_id: Vec<u8>,
    /// `increase_speed_tracking`.
    pub increase_speed_tracking: bool,
    /// `use_analytics_js`.
    pub use_analytics_js: bool,
    /// `running_experiment`.
    pub running_experiment: bool,
    /// `is_content_experiment`.
    pub is_content_experiment: bool,
    /// Experiment id (set and nonzero only).
    pub experiment_id: Option<i64>,
    /// `ToExperimentString` (`Experiment: {id}` when specified).
    pub experiment_string: String,
    /// Custom-variable slot (`experiment_ga_slot`).
    pub experiment_ga_slot: u32,
    /// Content experiment id.
    pub content_experiment_id: String,
    /// Content experiment variant id.
    pub content_experiment_variant_id: String,
}

/// Snippet classification (`AnalyticsStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnalyticsStatus {
    NoSnippet,
    UnusableSnippet,
    GaJs,
    AnalyticsJs,
}

/// Tokenizer states for the analytics.js rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    Initial,
    GotGa,
    GotGaFuncCall,
    GotGaCreate,
    GotGaCreateComma,
    GotFieldsObject,
    GotGaSend,
    GotGaSendComma,
    GotGaSendPageview,
    Success,
}

/// Inserts GA snippets (`InsertGAFilter`).
#[derive(Debug)]
pub struct InsertGAFilter {
    config: InsertGAConfig,
    patterns: &'static TokenizerPatterns,
    script_element: Option<NodeId>,
    found_snippet: bool,
    added_analytics_js: bool,
    added_experiment_snippet: bool,
    seen_sync_ga_js: bool,
    postponed_script_body: Vec<u8>,
    body_end: BodyEndTracker,
    inserted_snippets: i64,
}

impl InsertGAFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: InsertGAConfig) -> Self {
        InsertGAFilter {
            config,
            patterns: TokenizerPatterns::shared(),
            script_element: None,
            found_snippet: false,
            added_analytics_js: false,
            added_experiment_snippet: false,
            seen_sync_ga_js: false,
            postponed_script_body: Vec::new(),
            body_end: BodyEndTracker::default(),
            inserted_snippets: 0,
        }
    }

    /// Inserted snippets (`inserted_ga_snippets`).
    #[must_use]
    pub fn inserted_snippets(&self) -> i64 {
        self.inserted_snippets
    }

    /// Classifies one inline script (`FindSnippetInScript`).
    fn find_snippet_in_script(&mut self, contents: &[u8]) -> AnalyticsStatus {
        // dc.js counts as old-style ga.js.
        if !self.seen_sync_ga_js
            && (contains(contents, b"google-analytics.com/ga.js")
                || contains(contents, b"stats.g.doubleclick.net/dc.js"))
        {
            // Probably the first half of a split synchronous snippet;
            // the second half (tracked below) confirms it.
            self.seen_sync_ga_js = true;
        }
        let mut quoted = b"'".to_vec();
        quoted.extend_from_slice(&self.config.ga_id);
        quoted.push(b'\'');
        let mut double_quoted = b"\"".to_vec();
        double_quoted.extend_from_slice(&self.config.ga_id);
        double_quoted.push(b'"');
        if !contains(contents, &quoted) && !contains(contents, &double_quoted) {
            return AnalyticsStatus::NoSnippet;
        }
        if contains(contents, b".google-analytics.com/urchin.js") {
            // urchin.js is too old to touch.
            return AnalyticsStatus::UnusableSnippet;
        } else if contains(contents, b".google-analytics.com/ga.js")
            || contains(contents, b"stats.g.doubleclick.net/dc.js")
        {
            return AnalyticsStatus::GaJs;
        } else if self.seen_sync_ga_js
            && contains(contents, b"_getTracker")
            && contains(contents, b"_trackPageview")
        {
            // Second half of a split synchronous snippet.
            return AnalyticsStatus::GaJs;
        } else if find_subslice(contents, b".google-analytics.com/analytics.js") != Some(0) {
            // NOTE: the C++ reads `else if (s.find(...))` with no
            // comparison — true for every position except 0, *including*
            // not-found. Scripts without analytics.js at position 0 land
            // here by that quirk, and the port keeps it byte-behavioral.
            return AnalyticsStatus::AnalyticsJs;
        }
        AnalyticsStatus::UnusableSnippet
    }

    /// Whether experiment tracking applies (`ShouldInsertExperimentTracking`).
    fn should_insert_experiment_tracking(&self, is_analytics_js: bool) -> bool {
        if !self.config.running_experiment {
            return false;
        }
        if is_analytics_js && !self.config.is_content_experiment {
            // analytics.js only reports through content experiments.
            return false;
        }
        self.config.experiment_id.is_some()
    }

    /// analytics.js experiment snippet.
    fn analytics_js_experiment_snippet(&self) -> Vec<u8> {
        let mut out = CX_SET_EXP_HEAD.to_vec();
        out.extend_from_slice(self.config.content_experiment_id.as_bytes());
        out.extend_from_slice(CX_SET_EXP_MID);
        out.extend_from_slice(self.config.content_experiment_variant_id.as_bytes());
        out.extend_from_slice(CX_SET_EXP_TAIL);
        out
    }

    /// ga.js experiment snippet (numeric variants call out, others warn).
    fn ga_js_experiment_snippet(&self) -> Vec<u8> {
        match parse_int(self.config.content_experiment_variant_id.as_bytes()) {
            Some(variant) => {
                let mut out = CX_SET_CHOSEN_VARIATION_HEAD.to_vec();
                out.extend_from_slice(variant.to_string().as_bytes());
                out.extend_from_slice(CX_SET_CHOSEN_VARIATION_MID);
                out.extend_from_slice(self.config.content_experiment_id.as_bytes());
                out.extend_from_slice(CX_SET_CHOSEN_VARIATION_TAIL);
                out
            }
            None => {
                let mut out = CX_NON_NUMERIC_HEAD.to_vec();
                out.extend_from_slice(self.config.content_experiment_variant_id.as_bytes());
                out.extend_from_slice(CX_NON_NUMERIC_TAIL);
                out
            }
        }
    }

    /// Custom-variable experiment preamble.
    fn custom_var_experiment_snippet(&self, speed_tracking: &[u8]) -> Vec<u8> {
        let mut out = GA_EXPERIMENT_HEAD.to_vec();
        out.extend_from_slice(speed_tracking);
        out.extend_from_slice(GA_EXPERIMENT_PUSH_HEAD);
        out.extend_from_slice(self.config.experiment_ga_slot.to_string().as_bytes());
        out.extend_from_slice(GA_EXPERIMENT_PUSH_MID);
        out.extend_from_slice(self.config.experiment_string.as_bytes());
        out.extend_from_slice(GA_EXPERIMENT_PUSH_TAIL);
        out
    }

    /// Full ga.js async snippet.
    fn ga_js_snippet(&self, domain: &[u8], speed_tracking: &[u8]) -> Vec<u8> {
        let mut out = GA_JS_HEAD.to_vec();
        out.extend_from_slice(&self.config.ga_id);
        out.extend_from_slice(GA_JS_DOMAIN_HEAD);
        out.extend_from_slice(domain);
        out.extend_from_slice(GA_JS_TAIL_HEAD);
        out.extend_from_slice(speed_tracking);
        out.extend_from_slice(GA_JS_TAIL);
        out
    }

    /// Full analytics.js snippet.
    fn analytics_js_snippet(&self, speed_tracking: &[u8], experiment: &[u8]) -> Vec<u8> {
        let mut out = ANALYTICS_JS_HEAD.to_vec();
        out.extend_from_slice(&self.config.ga_id);
        out.extend_from_slice(ANALYTICS_JS_CREATE_TAIL);
        out.extend_from_slice(speed_tracking);
        out.extend_from_slice(ANALYTICS_JS_TAIL);
        out.extend_from_slice(experiment);
        out
    }

    /// Full gtag.js snippet.
    fn gtag_js_snippet(&self) -> Vec<u8> {
        let mut out = GTAG_HEAD.to_vec();
        out.extend_from_slice(&self.config.ga_id);
        out.extend_from_slice(GTAG_LOADER_HEAD);
        out.extend_from_slice(&self.config.ga_id);
        out.extend_from_slice(GTAG_TAIL);
        out
    }

    /// Rewrites one inline script (`RewriteInlineScript`).
    fn rewrite_inline_script(&mut self, dom: &mut Dom, chars: NodeId) {
        let contents = dom.leaf_contents(chars);
        let status = self.find_snippet_in_script(&contents);
        if status == AnalyticsStatus::NoSnippet {
            return;
        }
        self.found_snippet = true;
        if !self.should_insert_experiment_tracking(status == AnalyticsStatus::AnalyticsJs) {
            return;
        }
        if status == AnalyticsStatus::UnusableSnippet {
            return;
        }
        if status == AnalyticsStatus::AnalyticsJs {
            if let Some(rewritten) = self.rewrite_analytics_js(&contents) {
                *dom.leaf_contents_mut(chars) = rewritten;
                self.added_experiment_snippet = true;
            }
            return;
        }
        if self.config.is_content_experiment {
            // Blank this script; the loader plus edited body append at
            // script end (safer than inserting mid-script post-flush).
            self.postponed_script_body = contents;
            dom.leaf_contents_mut(chars).clear();
        } else {
            let speed = if self.config.increase_speed_tracking {
                GA_SPEED_TRACKING
            } else {
                b""
            };
            let snippet = self.custom_var_experiment_snippet(speed);
            let mut body = snippet;
            body.extend_from_slice(&dom.leaf_contents(chars));
            *dom.leaf_contents_mut(chars) = body;
            self.added_experiment_snippet = true;
        }
    }

    /// Splices speed tracking and the experiment call into analytics.js
    /// (`RewriteInlineScript` analytics branch).
    fn rewrite_analytics_js(&self, contents: &[u8]) -> Option<Vec<u8>> {
        let mut tokenizer = Tokenizer::new(&self.patterns, contents);
        let mut rewritten = Vec::with_capacity(contents.len());
        let mut state = ParseState::Initial;
        let mut send_offset = None;
        loop {
            let (kind, span) = tokenizer.next_token();
            if kind == TokenType::EndOfInput {
                break;
            }
            if kind == TokenType::Error {
                return None;
            }
            let token = tokenizer.text(span);
            if matches!(
                kind,
                TokenType::Whitespace | TokenType::Comment | TokenType::LineSeparator
            ) {
                // Allowed everywhere; state holds.
            } else if state == ParseState::Initial
                && kind == TokenType::Identifier
                && token == b"ga"
            {
                send_offset = Some(rewritten.len());
                state = ParseState::GotGa;
            } else if state == ParseState::GotGa && kind == TokenType::Operator && token == b"(" {
                state = ParseState::GotGaFuncCall;
            } else if state == ParseState::GotGaFuncCall
                && kind == TokenType::StringLiteral
                && string_literal_matches(token, b"create")
            {
                state = ParseState::GotGaCreate;
            } else if state == ParseState::GotGaFuncCall
                && kind == TokenType::StringLiteral
                && (string_literal_matches(token, b"send")
                    || string_literal_ends_with(token, b".send"))
            {
                state = ParseState::GotGaSend;
            } else if state == ParseState::GotGaCreate
                && kind == TokenType::Operator
                && token == b","
            {
                state = ParseState::GotGaCreateComma;
            } else if state == ParseState::GotGaCreate
                && kind == TokenType::Operator
                && token == b")"
            {
                // Create closed with no fields object: splice tracking.
                if self.config.increase_speed_tracking {
                    rewritten.extend_from_slice(ANALYTICS_SPEED_TRACKING);
                }
                state = ParseState::Initial;
            } else if state == ParseState::GotGaCreateComma && kind == TokenType::StringLiteral {
                state = ParseState::GotGaCreate;
            } else if state == ParseState::GotGaCreateComma
                && kind == TokenType::Operator
                && token == b"{"
            {
                state = ParseState::GotFieldsObject;
            } else if state == ParseState::GotFieldsObject {
                // Splice tracking ahead of the first field.
                if self.config.increase_speed_tracking {
                    rewritten.extend_from_slice(ANALYTICS_SPEED_TRACKING_MINIMAL);
                }
                state = ParseState::Initial;
            } else if state == ParseState::GotGaSend && kind == TokenType::Operator && token == b","
            {
                state = ParseState::GotGaSendComma;
            } else if state == ParseState::GotGaSendComma
                && kind == TokenType::StringLiteral
                && string_literal_matches(token, b"pageview")
            {
                state = ParseState::GotGaSendPageview;
            } else if state == ParseState::GotGaSendPageview
                && kind == TokenType::Operator
                && (token == b"," || token == b")")
            {
                let offset = send_offset.expect("send offset set with ga call");
                let snippet = self.analytics_js_experiment_snippet();
                rewritten.splice(offset..offset, snippet);
                state = ParseState::Success;
            } else if state == ParseState::Success {
                // Changes made; pass the rest through.
            } else {
                state = ParseState::Initial;
            }
            rewritten.extend_from_slice(token);
        }
        if state == ParseState::Success {
            Some(rewritten)
        } else {
            None
        }
    }

    /// Appends postponed loader + body at script end (`HandleEndScript`).
    fn handle_end_script(&mut self, dom: &mut Dom) {
        if !self.postponed_script_body.is_empty() {
            dom.insert_script_after_current(CX_API_URL, true);
            let mut body = self.ga_js_experiment_snippet();
            body.extend_from_slice(&self.postponed_script_body);
            dom.insert_script_after_current(&body, false);
            self.added_experiment_snippet = true;
            self.postponed_script_body.clear();
        }
        self.script_element = None;
    }
}

/// Substring search.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// First occurrence of a needle.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Quoted literal equality, quotes excluded (`StringLiteralMatches`).
fn string_literal_matches(literal: &[u8], desired: &[u8]) -> bool {
    literal.len() >= 2 && literal[1..literal.len() - 1] == *desired
}

/// Quoted literal suffix, quotes excluded (`StringLiteralEndsWith`).
fn string_literal_ends_with(literal: &[u8], desired: &[u8]) -> bool {
    literal.len() >= 2 && literal[1..literal.len() - 1].ends_with(desired)
}

/// Strict decimal integer (`base::StringToInt` shape): leading spaces,
/// optional sign, digits, then end.
fn parse_int(text: &[u8]) -> Option<i32> {
    let mut text = text;
    while let Some(rest) = text.strip_prefix(b" ") {
        text = rest;
    }
    let (text, negative) = match text.strip_prefix(b"-") {
        Some(rest) => (rest, true),
        None => (text.strip_prefix(b"+").unwrap_or(text), false),
    };
    if text.is_empty() || !text.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut value: i64 = 0;
    for byte in text {
        value = value.checked_mul(10)?.checked_add((byte - b'0') as i64)?;
    }
    if negative {
        value = -value;
    }
    i32::try_from(value).ok()
}

impl HtmlFilter for InsertGAFilter {
    fn name(&self) -> &'static str {
        "InsertGA"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.found_snippet = false;
        self.script_element = None;
        self.added_analytics_js = false;
        self.added_experiment_snippet = false;
        self.body_end.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.clear();
        if !self.config.ga_id.is_empty()
            && !self.found_snippet
            && dom.element_keyword(id) == Keyword::Script
            && self.script_element.is_none()
        {
            self.script_element = Some(id);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
        if self.config.ga_id.is_empty() {
            return;
        }
        if dom.element_keyword(id) == Keyword::Script {
            self.handle_end_script(dom);
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        // NOTE: no tracker upcall here, exactly like the C++ override
        // skipping `CommonFilter::Characters`.
        if self.script_element.is_some() && !self.found_snippet && !self.added_experiment_snippet {
            self.rewrite_inline_script(dom, id);
        }
    }

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        if self.found_snippet || self.added_analytics_js || self.config.ga_id.is_empty() {
            return;
        }
        let is_tag = self.config.ga_id.starts_with(b"GT-") || self.config.ga_id.starts_with(b"G-");
        let js_text;
        if is_tag {
            // Experiment reporting is unsupported for gtag.js snippets.
            js_text = self.gtag_js_snippet();
        } else if self.config.use_analytics_js {
            let speed = if self.config.increase_speed_tracking {
                ANALYTICS_SPEED_TRACKING
            } else {
                b"".as_slice()
            };
            let experiment = if self.should_insert_experiment_tracking(true) {
                self.analytics_js_experiment_snippet()
            } else {
                Vec::new()
            };
            js_text = self.analytics_js_snippet(speed, &experiment);
        } else {
            if self.should_insert_experiment_tracking(false) {
                if self.config.is_content_experiment {
                    let cx = dom.new_element(None, b"script");
                    dom.add_attribute(cx, b"src", CX_API_URL);
                    self.body_end.insert(dom, cx);
                    let experiment = self.ga_js_experiment_snippet();
                    let domain = document_host(lexer);
                    let js = self.ga_js_snippet(&domain, speed_tracking_of(&self.config));
                    let mut full = experiment;
                    full.extend_from_slice(&js);
                    let script = dom.new_element(None, b"script");
                    self.body_end.insert(dom, script);
                    let child =
                        dom.new_leaf(Some(script), crate::element::LeafKind::Characters, full);
                    dom.append_child(Some(script), child);
                    self.added_analytics_js = true;
                    self.inserted_snippets += 1;
                    return;
                }
                let experiment = self.custom_var_experiment_snippet(b"");
                let domain = document_host(lexer);
                let js = self.ga_js_snippet(&domain, speed_tracking_of(&self.config));
                let mut full = experiment;
                full.extend_from_slice(&js);
                let script = dom.new_element(None, b"script");
                self.body_end.insert(dom, script);
                let child = dom.new_leaf(Some(script), crate::element::LeafKind::Characters, full);
                dom.append_child(Some(script), child);
                self.added_analytics_js = true;
                self.inserted_snippets += 1;
                return;
            }
            let domain = document_host(lexer);
            js_text = self.ga_js_snippet(&domain, speed_tracking_of(&self.config));
        }
        let script = dom.new_element(None, b"script");
        self.body_end.insert(dom, script);
        let child = dom.new_leaf(Some(script), crate::element::LeafKind::Characters, js_text);
        dom.append_child(Some(script), child);
        self.added_analytics_js = true;
        self.inserted_snippets += 1;
    }
}

/// ga.js speed tracking when enabled.
fn speed_tracking_of(config: &InsertGAConfig) -> &[u8] {
    if config.increase_speed_tracking {
        GA_SPEED_TRACKING
    } else {
        b""
    }
}

/// Document host for the ga.js snippet domain.
fn document_host(lexer: &Lexer) -> Vec<u8> {
    let url = String::from_utf8_lossy(lexer.document_url());
    ps_http::google_url::GoogleUrl::new(&url)
        .host()
        .into_bytes()
}
