//! Rewriting filter contract (`RewriteFilter`).
//!
//! Resource filters rewrite slot URLs through contexts; this trait
//! carries the filter side (identity, encoder, related sets, charset
//! rules). Context creation returns nothing until the
//! `RewriteContext` machine lands.

use std::rc::Rc;

use ps_config::filter::Filter;
use ps_css::inline::charset_for_bom;
use ps_css::parser::Parser as CssParser;
use ps_http::fetch::AsyncFetch;

use super::output_resource::OutputResource;
use super::server_context::ResourceDriver;
use super::url_segment_encoder::{DefaultEncoder, UrlSegmentEncoder};

/// Shared default encoder (filters own theirs in C++; the unit
/// struct shares one here).
static DEFAULT_ENCODER: DefaultEncoder = DefaultEncoder;

/// Rewriting filter (`RewriteFilter`).
pub trait RewriteFilter {
    /// Filter id prefix (`id`, e.g. `"cf"`).
    fn id(&self) -> &'static str;

    /// URL segment codec (`encoder`, default single-URL).
    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &DEFAULT_ENCODER
    }

    /// Whether output recomputes per use (`ComputeOnTheFly`).
    fn compute_on_the_fly(&self) -> bool {
        false
    }

    /// Related filters for URL option encoding (`RelatedFilters`).
    fn related_filters(&self) -> &'static [Filter] {
        &[]
    }

    /// Related option names, case-insensitively sorted
    /// (`RelatedOptions`).
    fn related_options(&self) -> Option<&'static [&'static str]> {
        None
    }

    /// Serves a decoded output (`MakeRewriteContext` + `Fetch`,
    /// fused: the sync model runs the fetch rewrite inline).
    /// Filters that cannot reconstruct return false (404).
    fn fetch(
        &self,
        _driver: &Rc<ResourceDriver>,
        _output: OutputResource,
        _fetch: &mut dyn AsyncFetch,
    ) -> bool {
        false
    }

    /// Rewrites fetched bytes for in-place serving (the sync
    /// stand-in for a nested `RewriteSingle`): `None` means
    /// unoptimizable. Only test servants implement this; real
    /// filters need slot/URL context their HTML paths own, so
    /// in-place serving treats them as unoptimizable for now.
    fn rewrite_in_place(
        &self,
        _driver: &Rc<ResourceDriver>,
        _input: &[u8],
        _content_type: &ps_http::content_type::ContentType,
    ) -> Option<InPlaceOutput> {
        None
    }

    /// Encodes UA capacities for the IPRO cache key
    /// (`EncodeUserAgentIntoResourceContext`).
    fn encode_user_agent_into_resource_context(
        &self,
        _driver: &Rc<ResourceDriver>,
        _context: &mut super::image_url_encoder::ImageResourceContext,
    ) {
    }
}

/// Nested rewrite result for in-place serving.
pub struct InPlaceOutput {
    /// Rewritten bytes.
    pub bytes: Vec<u8>,
    /// Output content type (override or input type).
    pub content_type: ps_http::content_type::ContentType,
    /// Post-optimization image format, when known.
    pub optimized_image_type: Option<ps_image::format::ImageType>,
}

/// Script charset, WHATWG order (`GetCharsetForScript`): resource
/// charset, attribute, BOM, enclosing page.
#[must_use]
pub fn charset_for_script(
    resource_charset: &[u8],
    script_contents: &[u8],
    attribute_charset: &[u8],
    enclosing_charset: &[u8],
) -> Vec<u8> {
    if !resource_charset.is_empty() {
        return resource_charset.to_vec();
    }
    if !attribute_charset.is_empty() {
        return attribute_charset.to_vec();
    }
    let bom = charset_for_bom(script_contents);
    if !bom.is_empty() {
        return bom.to_vec();
    }
    if !enclosing_charset.is_empty() {
        return enclosing_charset.to_vec();
    }
    Vec::new()
}

/// Stylesheet charset (`GetCharsetForStylesheet`): resource charset,
/// `@charset`, BOM, attribute, enclosing page.
#[must_use]
pub fn charset_for_stylesheet(
    resource_charset: &[u8],
    stylesheet_contents: &[u8],
    attribute_charset: &[u8],
    enclosing_charset: &[u8],
) -> Vec<u8> {
    if !resource_charset.is_empty() {
        return resource_charset.to_vec();
    }
    let stripped = ps_css::combine::strip_utf8_bom(stylesheet_contents).0;
    let mut parser = CssParser::new(stripped);
    let at_charset = parser.extract_charset();
    if parser.errors_seen_mask() == 0 && !at_charset.is_empty() {
        return at_charset.as_bytes().to_vec();
    }
    let bom = charset_for_bom(stylesheet_contents);
    if !bom.is_empty() {
        return bom.to_vec();
    }
    if !attribute_charset.is_empty() {
        return attribute_charset.to_vec();
    }
    if !enclosing_charset.is_empty() {
        return enclosing_charset.to_vec();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ProbeFilter;

    impl RewriteFilter for ProbeFilter {
        fn id(&self) -> &'static str {
            "cf"
        }
    }

    #[test]
    fn filter_defaults() {
        let filter = ProbeFilter;
        assert_eq!(filter.id(), "cf");
        assert!(!filter.compute_on_the_fly());
        assert!(filter.related_filters().is_empty());
        assert!(filter.related_options().is_none());
        // The default codec round-trips single URLs.
        let encoded = filter.encoder().encode(&[b"a.css".to_vec()]);
        assert_eq!(
            filter.encoder().decode(&encoded),
            Some(vec![b"a.css".to_vec()])
        );
    }

    #[test]
    fn script_charset_priority() {
        let bom = b"\xef\xbb\xbfvar x = 1;";
        // Resource beats everything.
        assert_eq!(
            charset_for_script(b"text/plain", b"", b"windows-1252", b"utf-8"),
            b"text/plain"
        );
        // Attribute beats BOM and page.
        assert_eq!(
            charset_for_script(b"", bom, b"windows-1252", b"utf-8"),
            b"windows-1252"
        );
        // BOM beats the page.
        assert_eq!(charset_for_script(b"", bom, b"", b"utf-8"), b"utf-8");
        // Page is the fallback.
        assert_eq!(
            charset_for_script(b"", b"var x;", b"", b"latin1"),
            b"latin1"
        );
        // Nothing known: empty.
        assert!(charset_for_script(b"", b"var x;", b"", b"").is_empty());
    }

    #[test]
    fn stylesheet_charset_priority() {
        let at = b"@charset \"shift-jis\"; a { color: red; }";
        // Resource beats everything.
        assert_eq!(
            charset_for_stylesheet(b"text/css", b"", b"", b""),
            b"text/css"
        );
        // Clean `@charset` beats BOM, attribute, and page.
        assert_eq!(
            charset_for_stylesheet(b"", at, b"windows-1252", b"utf-8"),
            b"shift-jis"
        );
        // A BOM beats attribute and page.
        let bom = b"\xef\xbb\xbfa { color: red; }";
        assert_eq!(
            charset_for_stylesheet(b"", bom, b"windows-1252", b"utf-8"),
            b"utf-8"
        );
        // Attribute beats the page.
        assert_eq!(
            charset_for_stylesheet(b"", b"a {}", b"windows-1252", b"utf-8"),
            b"windows-1252"
        );
        // Page is the fallback.
        assert_eq!(
            charset_for_stylesheet(b"", b"a {}", b"", b"latin1"),
            b"latin1"
        );
        // Nothing known: empty.
        assert!(charset_for_stylesheet(b"", b"a {}", b"", b"").is_empty());
    }
}
