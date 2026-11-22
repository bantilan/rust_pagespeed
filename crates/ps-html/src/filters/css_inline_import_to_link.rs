//! Inline `@import` to `<link>` conversion (`CssInlineImportToLinkFilter`).
//!
//! Pulls leading `@import` rules out of a `<style>` element into `<link>`
//! elements (converting at most the import prefix; trailing CSS stays
//! inline). Conversion needs a rewritable text/css style without
//! href/rel/scoped/integrity, non-empty import URLs, and matching media;
//! a mid-style flush vetoes the rewrite. The XHTML verdict selects brief
//! link closes.
//!
//! The conversion counter publishes through
//! [`CssInlineImportToLinkFilter::init_stats`] (`InitStats`); without it
//! the filter counts locally only.

use std::sync::Arc;

use ps_css::ast::Import;
use ps_css::media::MediaQueries;
use ps_css::parser::Parser as CssParser;
use ps_css::util as css_util;
use ps_stats::core::{Statistics, Variable};

use super::common::XhtmlStatus;
use crate::element::{ElementStyle, NodeId};
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::script_scanner::ScriptTagScanner;

/// Converted styles (`css_imports_to_links`).
pub const IMPORTS_TO_LINKS: &str = "css_imports_to_links";

/// Driver inputs for import conversion.
#[derive(Debug, Clone, Default)]
pub struct CssInlineImportToLinkConfig {
    /// Response mimetype XHTML certainty.
    pub xhtml: XhtmlStatus,
}

/// Converts leading `@import`s to links (`CssInlineImportToLinkFilter`).
#[derive(Debug)]
pub struct CssInlineImportToLinkFilter {
    config: CssInlineImportToLinkConfig,
    style_element: Option<NodeId>,
    style_characters: Option<NodeId>,
    imports_to_links: i64,
    converted: Option<Arc<Variable>>,
}

impl CssInlineImportToLinkFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: CssInlineImportToLinkConfig) -> Self {
        CssInlineImportToLinkFilter {
            config,
            style_element: None,
            style_characters: None,
            imports_to_links: 0,
            converted: None,
        }
    }

    /// Resolves statistics counters (`InitStats`).
    pub fn init_stats(&mut self, statistics: &Statistics) {
        self.converted = Some(statistics.add_variable(IMPORTS_TO_LINKS));
    }

    /// Converted styles (`css_imports_to_links`).
    #[must_use]
    pub fn imports_to_links(&self) -> i64 {
        self.imports_to_links
    }

    /// Clears per-style tracking.
    fn reset_state(&mut self) {
        self.style_element = None;
        self.style_characters = None;
    }

    /// Pulls the import prefix into links (`InlineImportToLinkStyle`).
    fn inline_import_to_link_style(&mut self, dom: &mut Dom) {
        let Some(style) = self.style_element else {
            return;
        };
        let Some(chars) = self.style_characters else {
            return;
        };
        if !dom.is_rewritable(style)
            || dom.find_attribute(style, Keyword::Href).is_some()
            || dom.find_attribute(style, Keyword::Rel).is_some()
            || dom.find_attribute(style, Keyword::Scoped).is_some()
            || ScriptTagScanner::has_integrity_attribute(dom, style)
        {
            return;
        }
        // The style's media attribute, if any (undecodable fails).
        let mut media_attribute = Vec::new();
        if let Some(index) = dom.find_attribute(style, Keyword::Media) {
            let (decoded, _error) = dom.decoded_attribute(style, index);
            let Some(decoded) = decoded else { return };
            media_attribute = decoded;
        }
        let contents = dom.leaf_contents(chars);
        let mut parser = CssParser::new(&contents);
        let mut imports = Vec::new();
        let mut media = Vec::new();
        let mut style_media = Vec::new();
        let mut style_media_determined = false;
        let mut ok = true;
        while ok {
            let Some(import) = parser.parse_next_import() else {
                break;
            };
            media.push(media_attribute.clone());
            let last = media.len() - 1;
            ok = check_conversion(
                &import,
                &media_attribute,
                &mut media[last],
                &mut style_media_determined,
                &mut style_media,
            );
            imports.push(import);
        }
        if !ok || imports.is_empty() || parser.errors_seen_mask() != 0 {
            return;
        }
        for (index, import) in imports.iter().enumerate() {
            let url = import.link().as_bytes();
            let parent = dom.arena_node(style).parent();
            let link = dom.new_element(parent, b"link");
            if self.config.xhtml != XhtmlStatus::IsNotXhtml {
                dom.set_element_style(link, ElementStyle::BriefClose);
            }
            dom.add_attribute(link, b"rel", b"stylesheet");
            dom.add_attribute(link, b"href", url);
            // Style attributes copy onto every link; a style media
            // discards the remembered one (keeping the "original").
            let count = dom.element_attributes(style).len();
            for attr in 0..count {
                let (keyword, name, value, quote) = {
                    let attribute = &dom.element_attributes(style)[attr];
                    (
                        attribute.keyword(),
                        attribute.name_str().to_vec(),
                        attribute.escaped_value().map(<[u8]>::to_vec),
                        attribute.quote_style(),
                    )
                };
                if keyword == Keyword::Media {
                    media[index].clear();
                }
                dom.add_escaped_attribute(link, &name, value.as_deref(), quote);
            }
            if !media[index].is_empty() {
                dom.add_attribute(link, b"media", &media[index]);
            }
            dom.insert_node_before_node(style, link);
        }
        if parser.done() {
            // Only @imports: remove the emptied style.
            dom.delete_node(style);
        } else {
            // Leave trailing CSS behind.
            let offset = parser.current_offset();
            dom.leaf_contents_mut(chars).drain(..offset);
        }
        self.imports_to_links += 1;
        if let Some(converted) = &self.converted {
            converted.add(1);
        }
    }
}

/// Whether one import converts (`CheckConversionOfImportToLink`).
fn check_conversion(
    import: &Import,
    media_attribute: &[u8],
    link_media: &mut Vec<u8>,
    style_media_determined: &mut bool,
    style_media: &mut Vec<Vec<u8>>,
) -> bool {
    if import.link().as_bytes().is_empty() {
        // Empty URLs are problematic.
        return false;
    }
    if import.media_queries().is_empty() {
        // No media queries: the style's media (already defaulted) rides.
        return true;
    }
    if media_matches(media_attribute, import.media_queries()) {
        // A simple query matching the style's is also good.
        return true;
    }
    // Without style media the import's media rides along; with style
    // media both sides must vectorize identically (order-insensitive).
    let mut import_media = match css_util::media_queries_to_strings(import.media_queries()) {
        Some(media) => media,
        None => return false,
    };
    if media_attribute.is_empty() {
        *link_media = css_util::stringify_media_vector(&import_media);
        true
    } else {
        if !*style_media_determined {
            *style_media = css_util::vectorize_media_attribute(media_attribute);
            style_media.sort();
            *style_media_determined = true;
        }
        // `all` vectorizes empty on both sides alike.
        css_util::clear_vector_if_contains_media_all(&mut import_media);
        import_media.sort();
        *style_media == import_media
    }
}

/// Whether the import carries one simple query equal to the style media.
fn media_matches(media_attribute: &[u8], queries: &MediaQueries) -> bool {
    if media_attribute.is_empty() || queries.len() != 1 {
        return false;
    }
    if css_util::is_complex_media_query(&queries[0]) {
        return false;
    }
    queries[0].media_type().as_bytes() == media_attribute
}

impl HtmlFilter for CssInlineImportToLinkFilter {
    fn name(&self) -> &'static str {
        "CssInlineImportToLink"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.reset_state();
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.reset_state();
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.style_element.is_none() && dom.element_keyword(id) == Keyword::Style {
            // Only text/css styles (or untyped ones) qualify.
            let convertible = match dom.find_attribute(id, Keyword::Type) {
                None => true,
                Some(index) => {
                    let (decoded, _error) = dom.decoded_attribute(id, index);
                    decoded.as_deref().is_none_or(|mime| mime == b"text/css")
                }
            };
            if convertible {
                self.style_element = Some(id);
                self.style_characters = None;
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.style_element == Some(id) {
            self.inline_import_to_link_style(dom);
            self.reset_state();
        }
    }

    fn characters(&mut self, _dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if self.style_element.is_some() {
            self.style_characters = Some(id);
        }
    }

    fn flush(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // A mid-style flush vetoes the rewrite.
        if self.style_element.is_some() {
            self.reset_state();
        }
    }
}
