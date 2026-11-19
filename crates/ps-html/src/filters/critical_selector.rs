//! Critical-CSS prioritization (`CriticalSelectorFilter`, id `cl`).
//!
//! Splits page CSS into above-the-fold critical rules, inlined
//! where the stylesheets sat, and the full stylesheets, moved to
//! body end inside `<noscript>` blocks plus a loader script. The
//! split is driven by beaconed critical selectors read from the
//! installed [`CriticalSelectorFinder`](crate::critical_selector_finder::CriticalSelectorFinder).
//!
//! Sync-model notes: summaries recompute per element (the C++
//! metadata-cache entries, keyed with a selector suffix, are a
//! pure performance cache); inline styles summarize directly
//! instead of via data-URL input resources; slot cancellation
//! and pending states cannot interleave single-threaded
//! execution. Logging of the HTML status waits for the logging
//! subsystem.

use std::collections::HashSet;
use std::rc::Rc;

use ps_config::filter::Filter;
use ps_config::options::Category;
use ps_css::ast::{RulesetType, Stylesheet, NO_ERROR};
use ps_css::minify::Minifier;
use ps_css::parser::Parser as CssParser;

use crate::cacheable_resource::{MockLoadCallback, NotCacheablePolicy};
use crate::css_tag_scanner::{is_alternate_stylesheet, is_stylesheet_or_alternate};
use crate::css_util::{
    can_media_affect_screen, js_detectable_selector, stringify_media_vector,
    vectorize_media_attribute,
};
use crate::element::{LeafKind, NodeId};
use crate::filter::HtmlFilter;
use crate::filters::common::{add_js_to_element, BodyEndTracker};
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource::Resource;
use crate::server_context::{
    CssResolutionStatus, InlineAuthorizationPolicy, InputRole, IntendedFor, ResourceDriver,
};

/// Filter id (`CriticalSelectorFilter::id`).
pub const FILTER_ID: &str = "cl";
/// Noscript bundle marker class (`kNoscriptStylesClass`).
pub const NOSCRIPT_STYLES_CLASS: &str = "psa_add_styles";
/// Summarized styles (`kNumCssUsedForCriticalCssComputation`).
pub const NUM_CSS_USED: &str = "num_css_used_for_critical_css_computation";
/// Skipped styles (`kNumCssNotUsedForCriticalCssComputation`).
pub const NUM_CSS_NOT_USED: &str = "num_css_not_used_for_critical_css_computation";
/// Loader invocation appended to the runtime.
const LOADER_RUN: &[u8] = b"pagespeed.CriticalCssLoader.Run();";

/// Summary outcome (`SummaryState`; pending/cancelled states
/// cannot occur synchronously).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SummaryState {
    Ok,
    CssParseError,
    ResourceCreationFailed,
    InputUnavailable,
}

/// One computed summary (`SummaryInfo`).
struct SummaryInfo {
    state: SummaryState,
    data: Vec<u8>,
    location: String,
    base: String,
    media_from_html: Vec<u8>,
    rel: Vec<u8>,
    is_external: bool,
    is_inside_noscript: bool,
}

/// Bundled original CSS (`CssElement`/`CssStyleElement`): a
/// cloned element plus snapshotted inline contents, if any.
struct SavedCss {
    element: NodeId,
    chars: Option<Vec<u8>>,
    inside_noscript: bool,
}

/// Critical-selector filter (`CriticalSelectorFilter`).
pub struct CriticalSelectorFilter {
    driver: Rc<ResourceDriver>,
    critical_selectors: HashSet<String>,
    summaries: Vec<SummaryInfo>,
    css_elements: Vec<SavedCss>,
    style_element: Option<NodeId>,
    noscript_depth: u32,
    saw_end_document: bool,
    any_rendered: bool,
    body_end: BodyEndTracker,
    can_run: bool,
    disabled_reason: Option<String>,
}

impl CriticalSelectorFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        CriticalSelectorFilter {
            driver,
            critical_selectors: HashSet::new(),
            summaries: Vec::new(),
            css_elements: Vec::new(),
            style_element: None,
            noscript_depth: 0,
            saw_end_document: false,
            any_rendered: false,
            body_end: BodyEndTracker::default(),
            can_run: false,
            disabled_reason: None,
        }
    }

    /// Reads the beaconed selectors for this page.
    fn read_critical_selectors(&self) -> HashSet<String> {
        self.driver
            .server_context()
            .critical_selector_finder()
            .map(|finder| finder.get_critical_selectors(self.driver.base_url().spec()))
            .unwrap_or_default()
    }

    /// Computes the critical subset of a parsed stylesheet
    /// (`Summarize`): drops non-screen media queries and
    /// non-critical selectors, keeps unparsed regions, minifies.
    fn summarize(&self, stylesheet: &mut Stylesheet) -> Vec<u8> {
        let mut index = 0;
        while index < stylesheet.rulesets().len() {
            if stylesheet.rulesets()[index].kind() == RulesetType::UnparsedRegion {
                // Couldn't parse this as a rule, leave unaltered.
                index += 1;
                continue;
            }
            let mut any_media_apply = stylesheet.rulesets()[index].media_queries().is_empty();
            let mut kept_media = Vec::new();
            for query in stylesheet.rulesets()[index].media_queries().iter() {
                let text = query.to_css_string();
                if can_media_affect_screen(&String::from_utf8_lossy(&text)) {
                    any_media_apply = true;
                    kept_media.push(query.clone());
                }
            }
            let mut any_selectors_apply = false;
            let mut kept_selectors = Vec::new();
            if any_media_apply {
                // Zero selectors (partial parse errors) retain
                // conservatively.
                any_selectors_apply = stylesheet.rulesets()[index].selectors().is_empty();
                for selector in stylesheet.rulesets()[index].selectors().iter() {
                    let portion = js_detectable_selector(selector);
                    if portion.is_empty()
                        || self
                            .critical_selectors
                            .contains(&String::from_utf8_lossy(&portion).into_owned())
                    {
                        any_selectors_apply = true;
                        kept_selectors.push(selector.clone());
                    }
                }
            }
            if any_selectors_apply && any_media_apply {
                // Just remove the irrelevant selectors & media.
                let ruleset = &mut stylesheet.rulesets_mut()[index];
                *ruleset.media_queries_mut() = kept_media;
                if !ruleset.selectors().is_dummy() {
                    let mut selectors = ps_css::selector::Selectors::new();
                    for selector in kept_selectors {
                        selectors.push(selector);
                    }
                    *ruleset.selectors_mut() = selectors;
                }
                index += 1;
            } else {
                // Remove the entire production.
                stylesheet.rulesets_mut().remove(index);
            }
        }
        let mut minifier = Minifier::new(false);
        minifier.minify_stylesheet(stylesheet);
        minifier.finish().0
    }

    /// Summarizes fetched CSS bytes (`RewriteSingle`): `None` on
    /// size-limit, parse, or markup failures.
    fn rewrite_css(&self, contents: &[u8], url: &str) -> Option<Vec<u8>> {
        if crate::filters::css::over_parse_max_bytes(
            &self.driver,
            contents.len() as i64,
            url,
            "CSS not summarized",
        )
        .is_some()
        {
            return None;
        }
        let (text, _) = ps_css::combine::strip_utf8_bom(contents);
        let mut parser = CssParser::new(text);
        parser.set_preservation_mode(true);
        parser.set_quirks_mode(false);
        let mut stylesheet = parser.parse_raw_stylesheet();
        if parser.errors_seen_mask() != NO_ERROR {
            return None;
        }
        let data = self.summarize(&mut stylesheet);
        if ps_css::inline::may_contain_markup(&data) {
            return None;
        }
        Some(data)
    }

    /// Summarizes an inline `<style>` block
    /// (`StartInlineRewrite`, run synchronously).
    fn start_inline_rewrite(&mut self, dom: &mut Dom, style: NodeId, text: NodeId) {
        let contents = dom.leaf_contents(text);
        let pos = self.summaries.len();
        let mut summary = SummaryInfo {
            state: SummaryState::CssParseError,
            data: Vec::new(),
            location: "(inline style)".to_string(),
            base: self.driver.base_url().spec().to_string(),
            media_from_html: Vec::new(),
            rel: Vec::new(),
            is_external: false,
            is_inside_noscript: self.noscript_depth > 0,
        };
        if let Some(index) = dom.find_attribute(style, Keyword::Media) {
            let (decoded, _) = dom.decoded_attribute(style, index);
            summary.media_from_html = decoded.unwrap_or_default();
        }
        match self.rewrite_css(&contents, &summary.base.clone()) {
            Some(data) => {
                summary.state = SummaryState::Ok;
                summary.data = data;
                self.summaries.push(summary);
                self.render_summary(dom, pos, style, Some(text));
            }
            None => {
                self.summaries.push(summary);
                self.will_not_render_summary(dom, pos, style, Some(text));
            }
        }
    }

    /// Summarizes an external stylesheet (`StartExternalRewrite`,
    /// run synchronously).
    fn start_external_rewrite(
        &mut self,
        dom: &mut Dom,
        link: NodeId,
        href_index: usize,
        rel: Vec<u8>,
    ) {
        let (decoded, _) = dom.decoded_attribute(link, href_index);
        let href = decoded.unwrap_or_default();
        let url = ps_http::google_url::GoogleUrl::resolve(
            self.driver.base_url(),
            &String::from_utf8_lossy(&href),
        );
        let pos = self.summaries.len();
        let mut summary = SummaryInfo {
            state: SummaryState::ResourceCreationFailed,
            data: Vec::new(),
            location: String::from_utf8_lossy(&href).into_owned(),
            base: url.spec().to_string(),
            media_from_html: Vec::new(),
            rel,
            is_external: true,
            is_inside_noscript: self.noscript_depth > 0,
        };
        if let Some(index) = dom.find_attribute(link, Keyword::Media) {
            let (decoded, _) = dom.decoded_attribute(link, index);
            summary.media_from_html = decoded.unwrap_or_default();
        }
        let policy = if self
            .driver
            .options()
            .has_inline_unauthorized_resource_type(Category::Stylesheet)
        {
            InlineAuthorizationPolicy::InlineUnauthorizedResources
        } else {
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources
        };
        let (resource, authorized) = self.driver.create_input_resource(
            &url,
            policy,
            IntendedFor::Inlining,
            InputRole::Style,
        );
        let Some(mut input) = resource else {
            if authorized {
                summary.location = if href.is_empty() {
                    self.driver.base_url().spec().to_string()
                } else {
                    String::from_utf8_lossy(&href).into_owned()
                };
            }
            self.summaries.push(summary);
            self.will_not_render_summary(dom, pos, link, None);
            if self.driver.debug_mode() && (authorized || href.is_empty()) {
                // NOTE: unauthorized URLs stay quiet here (the CSS
                // filter already commented).
                let node = dom.new_leaf(
                    None,
                    LeafKind::Comment,
                    format!(
                        "{}: Cannot create resource: either its domain is unauthorized and InlineUnauthorizedResources is not enabled, or it cannot be fetched (check the server logs)",
                        self.name()
                    )
                    .into_bytes(),
                );
                dom.insert_node_before_current(node);
            }
            return;
        };
        let mut callback = MockLoadCallback::new();
        let mut ctx = self.driver.request_context().borrow().clone();
        input.load(
            NotCacheablePolicy::ReportFailureIfNotCacheable,
            &mut ctx,
            &mut callback,
        );
        if !callback.success() {
            summary.state = SummaryState::InputUnavailable;
            self.summaries.push(summary);
            self.will_not_render_summary(dom, pos, link, None);
            return;
        }
        summary.base = input.url().to_string();
        summary.location = input.url().to_string();
        let contents = input.contents();
        match self.rewrite_css(&contents, &summary.base.clone()) {
            Some(data) => {
                summary.state = SummaryState::Ok;
                summary.data = data;
                self.summaries.push(summary);
                self.render_summary(dom, pos, link, None);
            }
            None => {
                summary.state = SummaryState::CssParseError;
                self.summaries.push(summary);
                self.will_not_render_summary(dom, pos, link, None);
            }
        }
    }

    /// Renders a summary into the element (`RenderSummary`).
    fn render_summary(
        &mut self,
        dom: &mut Dom,
        pos: usize,
        element: NodeId,
        char_node: Option<NodeId>,
    ) {
        self.remember_full_css(dom, pos, element, char_node);
        let (is_external, base, data) = {
            let summary = &self.summaries[pos];
            (
                summary.is_external,
                summary.base.clone(),
                summary.data.clone(),
            )
        };
        // If we're inlining an external CSS file, make sure to
        // adjust the URLs inside to the new base.
        let css_to_use: Vec<u8> = if is_external {
            let input_base = ps_http::google_url::GoogleUrl::new(&base);
            let page_base = self.driver.base_url().spec().to_string();
            match self.driver.resolve_css_urls(&input_base, &page_base, &data) {
                (CssResolutionStatus::Success, rewritten) => rewritten,
                _ => data,
            }
        } else {
            data
        };
        // Check the bytes that are actually written, whatever
        // produced them.
        if ps_css::inline::may_contain_markup(&css_to_use) {
            return;
        }
        // Update the DOM --- either an existing style element, or
        // replace link with style.
        let mut element = element;
        if let Some(text) = char_node {
            *dom.leaf_contents_mut(text) = css_to_use.clone();
        } else {
            let parent = dom.arena_node(element).parent();
            let style = dom.new_element(parent, b"style");
            dom.replace_node(element, style);
            let text = dom.new_leaf(Some(style), LeafKind::Characters, css_to_use.clone());
            dom.append_child(Some(style), text);
            element = style;
        }

        // Update the media attribute to just the media that's
        // relevant to screen.
        let summary = &self.summaries[pos];
        let all_media = vectorize_media_attribute(&summary.media_from_html);
        let is_external = summary.is_external;
        let rel = summary.rel.clone();
        let is_inside_noscript = summary.is_inside_noscript;
        dom.delete_attribute(element, Keyword::Media);
        let mut drop_entire_element = false;
        if css_to_use.is_empty() {
            // Don't keep empty blocks around.
            drop_entire_element = true;
        } else if is_inside_noscript {
            // Optimize summary version for scriptable environment,
            // since noscript environment will eagerly load the
            // whole CSS anyway at the foot of the page.
            drop_entire_element = true;
        } else if is_external && is_alternate_stylesheet(&rel) {
            // Likewise drop alternate stylesheets, they're
            // non-critical.
            drop_entire_element = true;
        } else if !all_media.is_empty() {
            let mut relevant_media = Vec::new();
            for medium in &all_media {
                if can_media_affect_screen(&String::from_utf8_lossy(medium)) {
                    relevant_media.push(medium.clone());
                }
            }
            if !relevant_media.is_empty() {
                dom.add_attribute(element, b"media", &stringify_media_vector(&relevant_media));
            } else {
                // None of the media applied to the screen, so
                // remove the entire element.
                drop_entire_element = true;
            }
        }
        if drop_entire_element {
            dom.delete_node(element);
            // Deleting mid-pass erases the pending end event, so
            // the style tracker would otherwise go stale (C++
            // renders asynchronously, after parse events drain).
            if self.style_element == Some(element) {
                self.style_element = None;
            }
        }

        // We've altered the CSS, so we should generate code to
        // load the entire thing.
        self.any_rendered = true;
    }

    /// Records an unsummarized element for bundling
    /// (`WillNotRenderSummary`).
    fn will_not_render_summary(
        &mut self,
        dom: &mut Dom,
        pos: usize,
        element: NodeId,
        char_node: Option<NodeId>,
    ) {
        self.remember_full_css(dom, pos, element, char_node);
    }

    /// Snapshots original CSS for the lazy-load bundle
    /// (`RememberFullCss`).
    fn remember_full_css(
        &mut self,
        dom: &mut Dom,
        pos: usize,
        element: NodeId,
        char_node: Option<NodeId>,
    ) {
        let chars = char_node.map(|text| dom.leaf_contents(text));
        let inside_noscript = self.summaries[pos].is_inside_noscript;
        let clone = dom.clone_element(element);
        self.css_elements.push(SavedCss {
            element: clone,
            chars,
            inside_noscript,
        });
    }

    /// Emits the debug summary and stats (`ReportSummariesDone`).
    fn report_summaries_done(&self, dom: &mut Dom) {
        if self.driver.debug_mode() {
            let mut comment = format!("Summary computation status for {}\n", self.name());
            for (index, summary) in self.summaries.iter().enumerate() {
                let status = match summary.state {
                    SummaryState::Ok => "Computed OK\n",
                    SummaryState::CssParseError => {
                        "Unrecoverable CSS parse error or resource contains closing style tag\n"
                    }
                    SummaryState::ResourceCreationFailed => {
                        "Cannot create resource: either its domain is unauthorized and InlineUnauthorizedResources is not enabled, or it cannot be fetched (check the server logs)\n"
                    }
                    SummaryState::InputUnavailable => {
                        "Fetch failed or resource not publicly cacheable\n"
                    }
                };
                comment.push_str(&format!("Resource {index} {}: {status}", summary.location));
            }
            let mut escaped = Vec::new();
            crate::keywords::escape(comment.as_bytes(), &mut escaped);
            let node = dom.new_leaf(None, LeafKind::Comment, escaped);
            self.body_end.insert(dom, node);
        }
        for summary in &self.summaries {
            let name = if summary.state == SummaryState::Ok {
                NUM_CSS_USED
            } else {
                NUM_CSS_NOT_USED
            };
            self.driver.server_context().variable(name).add(1);
        }
    }
    /// Emits the lazy-load bundle (`RenderDone` body).
    fn emit_bundle(&mut self, dom: &mut Dom, lexer: &Lexer) {
        if !self.css_elements.is_empty() && self.any_rendered {
            let mut noscript = None;
            let mut previous_noscript = None;
            for index in 0..self.css_elements.len() {
                let saved = &self.css_elements[index];
                // Insert the full CSS, but hide all the style,
                // link tags inside noscript blocks so that
                // look-ahead parser cannot find them; and mark
                // the portions that were visible to scripting-
                // aware browser with class = psa_add_styles.
                if index == 0 || Some(saved.inside_noscript) != previous_noscript {
                    let element = dom.new_element(None, b"noscript");
                    if !saved.inside_noscript {
                        dom.add_attribute(element, b"class", NOSCRIPT_STYLES_CLASS.as_bytes());
                    }
                    self.body_end.insert(dom, element);
                    noscript = Some(element);
                    previous_noscript = Some(saved.inside_noscript);
                }
                let parent = noscript.expect("noscript group open");
                let saved = &self.css_elements[index];
                dom.append_child(Some(parent), saved.element);
                if let Some(chars) = saved.chars.clone() {
                    let text = dom.new_leaf(Some(saved.element), LeafKind::Characters, chars);
                    dom.append_child(Some(saved.element), text);
                }
            }
            let script = dom.new_element(None, b"script");
            dom.add_escaped_attribute(
                script,
                b"data-pagespeed-no-defer",
                None,
                crate::element::QuoteStyle::DoubleQuote,
            );
            self.body_end.insert(dom, script);
            let debug = self.driver.options().enabled(Filter::Debug);
            let mut js = crate::static_assets::get_asset(
                crate::static_assets::StaticAsset::CriticalCssLoaderJs,
                debug,
            )
            .to_vec();
            // NOTE: the test-only dont-apply-original-css flag has
            // no covering test; the invocation is always appended.
            js.extend_from_slice(LOADER_RUN);
            let xhtml = match self.driver.mime_type_xhtml_status() {
                crate::server_context::XhtmlStatus::IsNotXhtml => {
                    crate::filters::common::XhtmlStatus::IsNotXhtml
                }
                crate::server_context::XhtmlStatus::IsXhtml => {
                    crate::filters::common::XhtmlStatus::IsXhtml
                }
                crate::server_context::XhtmlStatus::Unknown => {
                    crate::filters::common::XhtmlStatus::Unknown
                }
            };
            add_js_to_element(
                dom,
                lexer,
                script,
                &js,
                self.driver.options().enabled(Filter::Pedantic),
                xhtml,
            );
        }
    }
}

impl HtmlFilter for CriticalSelectorFilter {
    fn name(&self) -> &'static str {
        "CriticalSelectorFilter"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        // We shouldn't do anything if there is no information on
        // critical selectors in the property cache. Unfortunately,
        // we also cannot run safely in case of IE, since we do not
        // understand IE conditional comments well enough to
        // replicate their behavior in the load-everything section.
        let selectors = self.read_critical_selectors();
        let ua_supports = self.driver.request_properties().supports_critical_css();
        self.can_run = ua_supports && !selectors.is_empty();
        // NOTE: the HTML-status log record waits for logging.
        self.disabled_reason = if self.can_run {
            None
        } else if !ua_supports {
            Some("User agent not supported".to_string())
        } else {
            Some("No critical selector info in cache".to_string())
        };
        self.can_run
    }

    fn disabled_reason(&self) -> Option<String> {
        self.disabled_reason.clone()
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // Read critical selector info from pcache.
        self.critical_selectors = self.read_critical_selectors();
        self.summaries.clear();
        self.css_elements.clear();
        self.style_element = None;
        self.noscript_depth = 0;
        self.saw_end_document = false;
        self.any_rendered = false;
        self.body_end.clear();
    }

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        self.saw_end_document = true;
        self.report_summaries_done(dom);
        // NOTE: C++ emits the bundle at RenderDone (after every
        // pass); post-phase filters here never observe that (the
        // writer streams first), so end-of-pass emission stands
        // in: all summaries resolved synchronously above.
        self.emit_bundle(dom, lexer);
        self.css_elements.clear();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.clear();
        match dom.element_keyword(id) {
            Keyword::Noscript => self.noscript_depth += 1,
            Keyword::Style => {
                // Scoped styles are left untouched: already
                // inlined, unsafe to move, and cascade-first
                // regardless of position.
                if dom.find_attribute(id, Keyword::Scoped).is_none() {
                    self.style_element = Some(id);
                }
            }
            _ => {}
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_characters(dom, id);
        if let Some(style) = self.style_element {
            self.start_inline_rewrite(dom, style, id);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
        if dom.element_keyword(id) == Keyword::Noscript && self.noscript_depth > 0 {
            self.noscript_depth -= 1;
        }
        if self.style_element == Some(id) {
            self.style_element = None;
            return;
        }
        if dom.element_keyword(id) != Keyword::Link {
            return;
        }
        let (rel, href) = {
            let rel = dom
                .find_attribute(id, Keyword::Rel)
                .and_then(|index| dom.decoded_attribute(id, index).0)
                .unwrap_or_default();
            let href = dom.find_attribute(id, Keyword::Href);
            (rel, href)
        };
        if !is_stylesheet_or_alternate(&rel) {
            return;
        }
        if let Some(href) = href {
            self.start_external_rewrite(dom, id, href, rel);
        }
    }
}
