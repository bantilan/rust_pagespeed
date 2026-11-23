//! CSS inlining (`CssInlineFilter`, `ci`).
//!
//! Replaces short external stylesheets with inline `<style>`
//! blocks, absolutifying relative URLs to the document base.
//!
//! NOTE: local-storage-cache interplay waits for that filter; CSP
//! gating waits for the CSP subsystem.

use std::rc::Rc;

use ps_cache::hash::Hasher;
use ps_config::filter::Filter;
use ps_config::options::Category;
use ps_http::google_url::GoogleUrl;

use crate::css_tag_scanner::parse_css_element;
use crate::css_util::can_media_affect_screen;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::metadata::CachedResult;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::rewrite_context::{InlineBehavior, InlineRewriteContext};
use crate::script_scanner::ScriptTagScanner;
use crate::server_context::{
    CssResolutionStatus, InlineAuthorizationPolicy, InputRole, ResourceDriver,
};

/// Filter id (`RewriteOptions::kCssInlineId`).
pub const FILTER_ID: &str = "ci";

/// Inline counter (`CssInlineFilter::kNumCssInlined`).
pub const NUM_CSS_INLINED: &str = "num_css_inlined";

/// Per-context behavior (charsets vary by element and page).
pub(crate) struct CssInlineBehavior {
    driver: Rc<ResourceDriver>,
    attrs_charset: Vec<u8>,
    htmls_charset: Vec<u8>,
    size_threshold_bytes: i64,
}

impl CssInlineBehavior {
    /// Rebuilds the behavior on a background worker from a snapshot.
    pub(crate) fn for_background(
        driver: Rc<ResourceDriver>,
        attrs_charset: Vec<u8>,
        htmls_charset: Vec<u8>,
        size_threshold_bytes: i64,
    ) -> Self {
        CssInlineBehavior {
            driver,
            attrs_charset,
            htmls_charset,
            size_threshold_bytes,
        }
    }
}

impl InlineBehavior for CssInlineBehavior {
    fn background_snapshot(&self) -> Option<crate::html_async::BackgroundBehavior> {
        Some(crate::html_async::BackgroundBehavior::InlineCss {
            attrs_charset: self.attrs_charset.clone(),
            htmls_charset: self.htmls_charset.clone(),
            size_threshold_bytes: self.size_threshold_bytes,
        })
    }

    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn input_role(&self) -> InputRole {
        InputRole::Style
    }

    fn allow_unauthorized_domain(&self, driver: &Rc<ResourceDriver>) -> InlineAuthorizationPolicy {
        // Inlining unauthorized CSS is safe: no new content executes
        // versus the unoptimized page.
        if driver
            .options()
            .has_inline_unauthorized_resource_type(Category::Stylesheet)
        {
            InlineAuthorizationPolicy::InlineUnauthorizedResources
        } else {
            InlineAuthorizationPolicy::InlineOnlyAuthorizedResources
        }
    }

    fn intended_for_inlining(&self) -> bool {
        true
    }

    fn cache_key_suffix(&self) -> String {
        let charsets = format!(
            "{}/{}",
            String::from_utf8_lossy(&self.htmls_charset),
            String::from_utf8_lossy(&self.attrs_charset)
        )
        .to_lowercase();
        self.driver
            .server_context()
            .lock_hasher()
            .hash(charsets.as_bytes())
    }

    fn policy_permits_rendering(&self) -> bool {
        // NOTE: `PermitsInlineStyle` waits for the CSP subsystem.
        true
    }

    fn should_inline(
        &self,
        _driver: &Rc<ResourceDriver>,
        input: &InputResource,
        reason: &mut String,
    ) -> bool {
        let contents = input.contents();
        if contents.len() as i64 > self.size_threshold_bytes {
            *reason = format!(
                "CSS not inlined since it's bigger than {} bytes",
                self.size_threshold_bytes
            );
            return false;
        }
        if contents.len() >= 10 && contents[0] == 0x1f && contents[1] == 0x8b {
            *reason = "CSS not inlined because it appears to be gzip-encoded".to_string();
            return false;
        }
        // A "</style>" inside would end the tag early.
        if contents.contains(&b'<') {
            *reason = "CSS not inlined since it contains a less-than sign".to_string();
            return false;
        }
        let css_charset = crate::rewrite_filter::charset_for_stylesheet(
            input.charset().as_bytes(),
            &contents,
            &self.attrs_charset,
            &self.htmls_charset,
        );
        if !self.htmls_charset.eq_ignore_ascii_case(&css_charset) {
            let clean = ps_css::combine::strip_utf8_bom(&contents).0;
            if clean.iter().any(|byte| *byte >= 0x80) {
                *reason = format!(
                    "CSS not inlined due to apparent charset incompatibility; we think the HTML is {} while the CSS is {}",
                    String::from_utf8_lossy(&self.htmls_charset),
                    String::from_utf8_lossy(&css_charset)
                );
                return false;
            }
        }
        true
    }

    fn render_without_inline_data(
        &self,
        driver: &Rc<ResourceDriver>,
        dom: &mut Dom,
        element: NodeId,
    ) {
        // Remove any LSC attributes as they're pointless if we
        // don't inline.
        super::local_storage_cache::LocalStorageCacheFilter::remove_lsc_attributes(
            driver, dom, element,
        );
    }

    fn render_inline(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        partition: &CachedResult,
        base_url: &GoogleUrl,
        dom: &mut Dom,
        element: NodeId,
    ) {
        // Cached bytes (the input may never have loaded on a hit).
        let contents = partition
            .inlined_data
            .clone()
            .expect("rendering without inlined data");
        let clean = ps_css::combine::strip_utf8_bom(&contents).0;
        let input_base = GoogleUrl::new(input.url());
        let (status, rewritten) = driver.resolve_css_urls(&input_base, base_url.spec(), clean);
        let rewritten = match status {
            CssResolutionStatus::NoResolutionNeeded => clean.to_vec(),
            CssResolutionStatus::WriteFailed => {
                // Remove any LSC attributes as they're now pointless.
                super::local_storage_cache::LocalStorageCacheFilter::remove_lsc_attributes(
                    driver, dom, element,
                );
                return;
            }
            CssResolutionStatus::Success => rewritten,
        };
        let parent = dom.arena_node(element).parent();
        let style = dom.new_element(parent, b"style");
        if !dom.replace_node(element, style) {
            return;
        }
        let text = dom.new_leaf(Some(style), crate::element::LeafKind::Characters, rewritten);
        dom.append_child(Some(style), text);
        let count = dom.element_attributes(element).len();
        let mut copied = Vec::new();
        let mut has_pagespeed_lsc_url = false;
        let mut has_pagespeed_lsc_hash = false;
        for index in 0..count {
            let (keyword, name, quotes) = {
                let attributes = dom.element_attributes(element);
                (
                    attributes[index].keyword(),
                    attributes[index].name_str().to_vec(),
                    attributes[index].quote_style(),
                )
            };
            match keyword {
                Keyword::Href | Keyword::Rel | Keyword::Type => {}
                Keyword::DataPagespeedLscHash => {
                    // If we have a hash, we /must/ have an url as
                    // well, so the fallthrough will set the url
                    // flag too (the hash case must come before the
                    // url case).
                    has_pagespeed_lsc_hash = true;
                    has_pagespeed_lsc_url = true;
                    let (decoded, _) = dom.decoded_attribute(element, index);
                    copied.push((name, decoded.unwrap_or_default(), quotes));
                }
                Keyword::DataPagespeedLscUrl => {
                    has_pagespeed_lsc_url = true;
                    let (decoded, _) = dom.decoded_attribute(element, index);
                    copied.push((name, decoded.unwrap_or_default(), quotes));
                }
                _ => {
                    let (decoded, _) = dom.decoded_attribute(element, index);
                    copied.push((name, decoded.unwrap_or_default(), quotes));
                }
            }
        }
        for (name, value, quotes) in copied {
            dom.add_attribute(style, &name, &value);
            if let Some(added) = dom.element_attributes_mut(style).last_mut() {
                added.set_quote_style(quotes);
            }
        }
        // NOTE: critical-CSS href tagging waits for that system.
        // If we don't already have a data-pagespeed-lsc-url then
        // EndElement must not have called AddStorableResource or
        // LSC is disabled; in either case there is no point in
        // trying to add the LSC attributes. OTOH, if have an url
        // and a hash then we've already got all the attributes we
        // need.
        if has_pagespeed_lsc_url && !has_pagespeed_lsc_hash {
            super::local_storage_cache::LocalStorageCacheFilter::add_lsc_attributes(
                input.url().as_bytes(),
                partition,
                driver,
                dom,
                style,
            );
        }
        driver.server_context().variable(NUM_CSS_INLINED).add(1);
    }
}

/// CSS-inlining filter (`CssInlineFilter`).
pub struct CssInlineFilter {
    driver: Rc<ResourceDriver>,
    size_threshold_bytes: i64,
    in_body: bool,
    noscript_depth: u32,
}

impl CssInlineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        let size_threshold_bytes = driver.options().css_inline_max_bytes();
        CssInlineFilter {
            driver,
            size_threshold_bytes,
            in_body: false,
            noscript_depth: 0,
        }
    }
}

impl HtmlFilter for CssInlineFilter {
    fn name(&self) -> &'static str {
        "CssInline"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.in_body = false;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        match dom.element_keyword(id) {
            Keyword::Body => self.in_body = true,
            Keyword::Noscript => self.noscript_depth += 1,
            _ => {}
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript && self.noscript_depth > 0 {
            self.noscript_depth -= 1;
        }
        // Don't inline inside <noscript>.
        if self.noscript_depth > 0 {
            return;
        }
        let Some(link) = parse_css_element(dom, id) else {
            return;
        };
        if dom.has_children_in_window(id) {
            return;
        }
        if self.driver.is_element_claimed(id) {
            return;
        }
        if self.driver.is_amp_document() {
            self.driver.insert_debug_comment(
                dom,
                "CSS inlining not supported by PageSpeed for AMP documents",
                id,
            );
            return;
        }
        if ScriptTagScanner::has_integrity_attribute(dom, id) {
            self.driver.insert_debug_comment(
                dom,
                "CSS not inlined because of the integrity attribute",
                id,
            );
            return;
        }
        if !can_media_affect_screen(&String::from_utf8_lossy(&link.media)) {
            self.driver.insert_debug_comment(
                dom,
                "CSS not inlined because media does not match screen",
                id,
            );
            return;
        }
        if self.in_body
            && self.driver.options().enabled(Filter::Pedantic)
            && !self.driver.options().enabled(Filter::MoveCssToHead)
        {
            self.driver.insert_debug_comment(
                dom,
                "CSS not inlined because style link element in html body",
                id,
            );
            return;
        }
        // Ask the LSC filter to work out how to handle this
        // element. A return value of true means we don't have to
        // rewrite it so can skip that.
        let (decoded, _) = dom.decoded_attribute(id, link.href);
        let mut state = super::local_storage_cache::InlineState::new();
        if !super::local_storage_cache::LocalStorageCacheFilter::add_storable_resource(
            decoded.as_deref().unwrap_or_default(),
            &self.driver,
            false,
            dom,
            id,
            &mut state,
        ) {
            // If we're rewriting we need the LSC filter to add the
            // URL as an attribute so that it knows to insert the
            // LSC specific javascript. C++ marks after
            // StartInlining because rendering is async; here the
            // sync inline replaces the link during StartInlining,
            // so the marker must already sit on the live link for
            // the render to copy it onto the <style>.
            super::local_storage_cache::LocalStorageCacheFilter::add_storable_resource(
                decoded.as_deref().unwrap_or_default(),
                &self.driver,
                true,
                dom,
                id,
                &mut state,
            );
            let attrs_charset = dom
                .find_attribute(id, Keyword::Charset)
                .and_then(|index| dom.decoded_attribute(id, index).0)
                .unwrap_or_default();
            let behavior = CssInlineBehavior {
                driver: self.driver.clone(),
                attrs_charset,
                htmls_charset: self.driver.containing_charset().into_bytes(),
                size_threshold_bytes: self.size_threshold_bytes,
            };
            let base_url = self.driver.base_url().clone();
            let mut context =
                InlineRewriteContext::new(self.driver.clone(), behavior, id, base_url);
            let initiated = context.start_inlining(dom, link.href, decoded.as_deref());
            if !initiated {
                // C++ never marks when initiation fails; drop
                // the speculative marker directly (no candidate
                // was recorded, so no removal stat either).
                dom.delete_attribute(id, Keyword::DataPagespeedLscUrl);
            }
        }
    }
}
