//! Subresource-hint stripping (`StripSubresourceHintsFilter`).
//!
//! Drops `<link rel=subresource|preload>` hints the rewriters supersede:
//! unconditionally for `subresource` (type unknown), and for `preload`
//! unless its `as` type is preserved (unknown types kept). Only allowed,
//! authorized URLs strip; anything else (and everything when URLs are
//! frozen) stays. Domain authorization rides the shared `DomainLawyer`.

use std::rc::Rc;

use ps_config::wildcard::WildcardGroup;
use ps_http::google_url::GoogleUrl;

use crate::base_tracker::BaseTracker;
use crate::domain_lawyer::DomainLawyer;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;

/// Driver inputs for hint stripping.
#[derive(Debug, Clone, Default)]
pub struct StripSubresourceHintsConfig {
    /// Another filter rewrites URLs (`can_modify_urls`).
    pub can_modify_urls: bool,
    /// CSS URLs preserved (`css_preserve_urls`).
    pub css_preserve_urls: bool,
    /// JS URLs preserved (`js_preserve_urls`).
    pub js_preserve_urls: bool,
    /// Image URLs preserved (`image_preserve_urls`).
    pub image_preserve_urls: bool,
    /// Allow/disallow rules (`IsAllowed`).
    pub allow_resources: WildcardGroup,
}

/// Strips superseded hints (`StripSubresourceHintsFilter`).
#[derive(Debug)]
pub struct StripSubresourceHintsFilter {
    config: StripSubresourceHintsConfig,
    lawyer: Rc<DomainLawyer>,
    base: BaseTracker,
}

impl StripSubresourceHintsFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: StripSubresourceHintsConfig, lawyer: &Rc<DomainLawyer>) -> Self {
        StripSubresourceHintsFilter {
            config,
            lawyer: lawyer.clone(),
            base: BaseTracker::default(),
        }
    }

    /// Decoded attribute value, empty when missing (`AttributeValue`).
    fn attribute_value(dom: &mut Dom, id: NodeId, name: &[u8]) -> Vec<u8> {
        let index = match name {
            b"rel" => dom.find_attribute(id, Keyword::Rel),
            b"href" => dom.find_attribute(id, Keyword::Href),
            _ => dom.find_attribute_by_name(id, name),
        };
        match index {
            Some(index) => dom.decoded_attribute(id, index).0.unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

impl HtmlFilter for StripSubresourceHintsFilter {
    fn name(&self) -> &'static str {
        "StripSubresourceHints"
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
        if !self.config.can_modify_urls || dom.element_keyword(id) != Keyword::Link {
            return;
        }
        let rel = Self::attribute_value(dom, id, b"rel");
        let is_subresource = rel.as_slice() == b"subresource";
        let is_preload = rel.as_slice() == b"preload";
        if !is_subresource && !is_preload {
            return;
        }
        if is_preload {
            // `subresource` strips blind (type unknown); `preload` keeps
            // preserved and unknown types.
            match Self::attribute_value(dom, id, b"as").as_slice() {
                b"image" if self.config.image_preserve_urls => return,
                b"script" if self.config.js_preserve_urls => return,
                b"style" if self.config.css_preserve_urls => return,
                b"image" | b"script" | b"style" => {}
                _ => return,
            }
        }
        let href = Self::attribute_value(dom, id, b"href");
        let href_str = String::from_utf8_lossy(&href);
        if !self.config.allow_resources.is_match(&href_str, true) {
            return;
        }
        let decoded = GoogleUrl::resolve(&self.base.base(), &href_str);
        if !decoded.is_web_valid() {
            return;
        }
        if self
            .lawyer
            .is_domain_authorized(&self.base.base(), &decoded)
        {
            dom.delete_node(id);
        }
    }
}
