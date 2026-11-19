//! Async AdSense conversion (`MakeShowAdsAsyncFilter`).
//!
//! Rewrites blocking ShowAds snippets into async `adsbygoogle` elements:
//! a data script holding `google_*` assignments becomes an `<ins>` plus a
//! single async library load, and each following ShowAds API call script
//! becomes an API-push snippet. Snippet parsing rides
//! [`ShowAdsSnippetParser`](crate::show_ads_snippet_parser::ShowAdsSnippetParser);
//! rewritability gates on the element staying inside the flush window.
//!
//! TODO(step4): publish the conversion counters as the
//! `show_ads_snippets_converted` / `show_ads_api_replaced_for_async`
//! statistics variables once the driver owns a registry. (Upstream also
//! registers `show_ads_snippets_not_converted` but never increments it.)

use crate::ads_attribute::{self, lookup_ads_by_google_attribute_name};
use crate::ads_util;
use crate::element::{ElementStyle, LeafKind, NodeId, QuoteStyle};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::show_ads_snippet_parser::{AttributeMap, ShowAdsSnippetParser};

/// Converts ShowAds snippets to async adsbygoogle (`MakeShowAdsAsyncFilter`).
#[derive(Debug)]
pub struct MakeShowAdsAsyncFilter {
    snippet_parser: ShowAdsSnippetParser,
    current_script: Option<NodeId>,
    current_contents: Vec<u8>,
    has_ads_by_google_js: bool,
    pending_api_call_replacements: i32,
    snippets_converted: i64,
    api_calls_replaced: i64,
}

impl MakeShowAdsAsyncFilter {
    /// Builds the filter.
    #[must_use]
    pub fn new() -> Self {
        MakeShowAdsAsyncFilter {
            snippet_parser: ShowAdsSnippetParser::new(),
            current_script: None,
            current_contents: Vec::new(),
            has_ads_by_google_js: false,
            pending_api_call_replacements: 0,
            snippets_converted: 0,
            api_calls_replaced: 0,
        }
    }

    /// Converted snippets (`show_ads_snippets_converted`).
    #[must_use]
    pub fn snippets_converted(&self) -> i64 {
        self.snippets_converted
    }

    /// Replaced API calls (`show_ads_api_replaced_for_async`).
    #[must_use]
    pub fn api_calls_replaced(&self) -> i64 {
        self.api_calls_replaced
    }

    /// Whether parsed attributes qualify for conversion
    /// (`IsApplicableShowAds`): client present, integer width and height,
    /// and no non-`html` output mode.
    fn is_applicable(attributes: &AttributeMap) -> bool {
        if !attributes.contains_key(ads_attribute::GOOGLE_AD_CLIENT.as_ref()) {
            return false;
        }
        for key in [
            ads_attribute::GOOGLE_AD_WIDTH,
            ads_attribute::GOOGLE_AD_HEIGHT,
        ] {
            let Some(value) = attributes.get(key.as_ref()) else {
                return false;
            };
            if parse_int(value).is_none() {
                return false;
            }
        }
        if let Some(output) = attributes.get(ads_attribute::GOOGLE_AD_OUTPUT.as_ref()) {
            if output.as_slice() != b"html" {
                return false;
            }
        }
        true
    }

    /// Replaces a data script with the library load plus `<ins>`
    /// (`ReplaceShowAdsWithAdsByGoogleElement`).
    fn replace_with_ads_by_google_element(
        &mut self,
        dom: &mut Dom,
        attributes: &AttributeMap,
        id: NodeId,
    ) {
        if !dom.is_rewritable(id) {
            return;
        }
        let parent = dom.arena_node(id).parent();
        if !self.has_ads_by_google_js {
            let script = dom.new_element(parent, b"script");
            dom.set_element_style(script, ElementStyle::ExplicitClose);
            dom.add_escaped_attribute(script, b"async", None, QuoteStyle::DoubleQuote);
            dom.add_attribute(script, b"src", ads_util::ADS_BY_GOOGLE_JAVASCRIPT_SRC);
            dom.insert_node_before_node(id, script);
            self.has_ads_by_google_js = true;
        }
        let width = &attributes[ads_attribute::GOOGLE_AD_WIDTH.as_ref()];
        let height = &attributes[ads_attribute::GOOGLE_AD_HEIGHT.as_ref()];
        let mut style = b"display:inline-block;width:".to_vec();
        style.extend_from_slice(width);
        style.extend_from_slice(b"px;height:");
        style.extend_from_slice(height);
        style.extend_from_slice(b"px");
        let ins = dom.new_element(parent, b"ins");
        dom.set_element_style(ins, ElementStyle::ExplicitClose);
        dom.add_attribute(ins, b"class", ads_util::ADS_BY_GOOGLE_CLASS);
        dom.add_attribute(ins, b"style", &style);
        for (name, value) in attributes {
            if name.as_slice() == ads_attribute::GOOGLE_AD_WIDTH
                || name.as_slice() == ads_attribute::GOOGLE_AD_HEIGHT
            {
                continue;
            }
            let renamed = lookup_ads_by_google_attribute_name(name);
            let name = if renamed.is_empty() { name } else { &renamed };
            dom.add_attribute(ins, name, value);
        }
        dom.insert_node_before_node(id, ins);
        self.pending_api_call_replacements += 1;
        dom.delete_node(id);
        self.snippets_converted += 1;
    }

    /// Replaces a ShowAds API call with the adsbygoogle push
    /// (`ReplaceShowAdsApiCallWithAdsByGoogleApiCall`).
    fn replace_api_call(&mut self, dom: &mut Dom, id: NodeId) {
        let parent = dom.arena_node(id).parent();
        let script = dom.new_element(parent, b"script");
        dom.insert_node_before_node(id, script);
        let snippet = dom.new_leaf(
            Some(script),
            LeafKind::Characters,
            ads_util::ADS_BY_GOOGLE_API_CALL_JAVASCRIPT.to_vec(),
        );
        dom.append_child(Some(script), snippet);
        dom.delete_node(id);
        self.api_calls_replaced += 1;
    }

    /// Raw `src` value (`EscapedAttributeValue`, absent when missing).
    fn escaped_src(dom: &Dom, id: NodeId) -> Option<&[u8]> {
        let index = dom.find_attribute(id, Keyword::Src)?;
        dom.element_attributes(id)[index].escaped_value()
    }
}

impl Default for MakeShowAdsAsyncFilter {
    fn default() -> Self {
        MakeShowAdsAsyncFilter::new()
    }
}

/// Strict decimal integer (`base::StringToInt` for tokenizer-produced
/// values): optional sign, ASCII digits, nothing else.
fn parse_int(text: &[u8]) -> Option<i32> {
    let text = text.strip_prefix(b"+").unwrap_or(text);
    let (text, negative) = match text.strip_prefix(b"-") {
        Some(rest) => (rest, true),
        None => (text, false),
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

impl HtmlFilter for MakeShowAdsAsyncFilter {
    fn name(&self) -> &'static str {
        "MakeShowAdsAsync"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.current_script = None;
        self.current_contents.clear();
        self.has_ads_by_google_js = false;
        self.pending_api_call_replacements = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) != Keyword::Script {
            return;
        }
        if let Some(src) = Self::escaped_src(dom, id) {
            if ads_util::is_ads_by_google_js_src(src) {
                self.has_ads_by_google_js = true;
            }
        }
        if self.current_script.is_none() {
            self.current_script = Some(id);
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.current_script != Some(id) {
            return;
        }
        if dom.is_rewritable(id) {
            let parsed = self.snippet_parser.parse_strict(&self.current_contents);
            let applicable = parsed.as_ref().is_some_and(Self::is_applicable);
            if applicable {
                self.replace_with_ads_by_google_element(
                    dom,
                    parsed.as_ref().expect("applicable implies parsed"),
                    id,
                );
            } else if self.pending_api_call_replacements > 0 {
                if let Some(src) = Self::escaped_src(dom, id) {
                    if ads_util::is_show_ads_api_call_js_src(src) {
                        self.replace_api_call(dom, id);
                        self.pending_api_call_replacements -= 1;
                    }
                }
            }
        }
        self.current_script = None;
        self.current_contents.clear();
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.current_script.is_some() {
            self.current_contents
                .extend_from_slice(&dom.leaf_contents(id));
        }
    }
}
