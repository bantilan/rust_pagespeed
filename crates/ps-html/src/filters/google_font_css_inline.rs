//! Google Font CSS inlining (`GoogleFontCssInlineFilter`, `gf`).
//!
//! Inlines small loader CSS files made by Google Font Service,
//! keyed by normalized user agent. Mirrors `CssInlineFilter`
//! (which C++ subclasses): same element checks, same inline
//! rendering, but font-service resources and its own size cap,
//! plus usage restrictions that HTML caching must stay UA-aware.

use std::rc::Rc;

use ps_cache::hash::Hasher;
use ps_config::filter::Filter;
use ps_http::google_url::GoogleUrl;

use crate::css_tag_scanner::parse_css_element;
use crate::css_util::can_media_affect_screen;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::font_service_input_resource::FontServiceInputResource;
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

/// Filter id (`RewriteOptions::kGoogleFontCssInlineId`).
pub const FILTER_ID: &str = "gf";

/// Per-context behavior (charsets vary by element and page).
pub(crate) struct GoogleFontCssInlineBehavior {
    driver: Rc<ResourceDriver>,
    attrs_charset: Vec<u8>,
    htmls_charset: Vec<u8>,
    size_threshold_bytes: i64,
}

impl GoogleFontCssInlineBehavior {
    /// Rebuilds the behavior on a background worker from a snapshot.
    pub(crate) fn for_background(
        driver: Rc<ResourceDriver>,
        attrs_charset: Vec<u8>,
        htmls_charset: Vec<u8>,
        size_threshold_bytes: i64,
    ) -> Self {
        GoogleFontCssInlineBehavior {
            driver,
            attrs_charset,
            htmls_charset,
            size_threshold_bytes,
        }
    }
}

impl InlineBehavior for GoogleFontCssInlineBehavior {
    fn background_snapshot(&self) -> Option<crate::html_async::BackgroundBehavior> {
        Some(crate::html_async::BackgroundBehavior::InlineGoogleFontCss {
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

    fn allow_unauthorized_domain(&self, _driver: &Rc<ResourceDriver>) -> InlineAuthorizationPolicy {
        // Google font resources don't have to be authorized.
        InlineAuthorizationPolicy::InlineUnauthorizedResources
    }

    fn intended_for_inlining(&self) -> bool {
        true
    }

    fn create_resource(
        &self,
        driver: &Rc<ResourceDriver>,
        url: &GoogleUrl,
    ) -> (Option<InputResource>, bool) {
        let resource =
            FontServiceInputResource::make_font_service(url, driver).map(InputResource::Font);
        (resource, true)
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
        driver
            .server_context()
            .variable(crate::filters::css_inline::NUM_CSS_INLINED)
            .add(1);
    }
}

/// Google-Font-CSS-inlining filter (`GoogleFontCssInlineFilter`).
pub struct GoogleFontCssInlineFilter {
    driver: Rc<ResourceDriver>,
    size_threshold_bytes: i64,
    in_body: bool,
    noscript_depth: u32,
}

impl GoogleFontCssInlineFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        let size_threshold_bytes = driver.options().google_font_css_inline_max_bytes();
        driver.add_resource_url_claimant(Rc::new(|url| {
            FontServiceInputResource::is_font_service_url(url)
        }));
        GoogleFontCssInlineFilter {
            driver,
            size_threshold_bytes,
            in_body: false,
            noscript_depth: 0,
        }
    }
}

impl HtmlFilter for GoogleFontCssInlineFilter {
    fn name(&self) -> &'static str {
        "GoogleFontCssInline"
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
        // Only font-service URLs reach the context; anything else is
        // silently someone else's stylesheet.
        let (decoded, _) = dom.decoded_attribute(id, link.href);
        let href = decoded.as_deref().unwrap_or(b"");
        let url = GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(href));
        if !FontServiceInputResource::is_font_service_url(&url) {
            return;
        }
        // Unfortunately some options prevent us from doing anything,
        // since they can make the HTML cached in a way unaware of
        // font UA dependencies.
        if !self.driver.options().modify_caching_headers() {
            self.driver.insert_debug_comment(
                dom,
                "Cannot inline font loader CSS when ModifyCachingHeaders is off",
                id,
            );
            return;
        }
        if !self
            .driver
            .options()
            .downstream_cache_purge_location_prefix()
            .is_empty()
        {
            self.driver.insert_debug_comment(
                dom,
                "Cannot inline font loader CSS when using downstream cache",
                id,
            );
            return;
        }
        let attrs_charset = dom
            .find_attribute(id, Keyword::Charset)
            .and_then(|index| dom.decoded_attribute(id, index).0)
            .unwrap_or_default();
        let behavior = GoogleFontCssInlineBehavior {
            driver: self.driver.clone(),
            attrs_charset,
            htmls_charset: self.driver.containing_charset().into_bytes(),
            size_threshold_bytes: self.size_threshold_bytes,
        };
        let base_url = self.driver.base_url().clone();
        let mut context = InlineRewriteContext::new(self.driver.clone(), behavior, id, base_url);
        context.start_inlining(dom, link.href, decoded.as_deref());
    }
}
