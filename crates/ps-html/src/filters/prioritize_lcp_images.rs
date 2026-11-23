//! LCP image prioritization (`PrioritizeLcpImagesFilter`).
//!
//! Preloads the hero image and lazy-hints the rest. Learned beacon
//! evidence wins: stamped original hashes match the critical set,
//! unstamped images match the reported LCP URL. Without any
//! evidence the first eligible image is the heuristic hero; when
//! evidence names other images, the first image is left alone
//! rather than preloaded on a contradicted guess. At most one
//! preload is inserted, skipped when the author already preloads
//! the target (element or Link header).
//!
//! CSS-background LCP (`bg` reports carry the loaded URL but no
//! element) preloads from evidence at end of document when no
//! image spent the single-preload budget: a head-level
//! `as=image` hint with high fetch priority, format-checked
//! against the request (learned bytes are format-specific).
#![forbid(unsafe_code)]

use crate::beacon_finder::{self, Availability, BeaconEvidence, BeaconFinder};
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::server_context::ResourceDriver;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Maximum preloaded heroes (`kMaxPrioritizedImages`).
const MAX_PRIORITIZED_IMAGES: u32 = 1;

/// First `<picture>` source candidate (later sources only count).
#[derive(Debug, Clone, Default)]
struct PictureSource {
    srcset: Vec<u8>,
    media: Vec<u8>,
    mime: Vec<u8>,
    count: u32,
}

/// LCP prioritization filter.
pub struct PrioritizeLcpImagesFilter {
    driver: Rc<ResourceDriver>,
    head_element: Option<NodeId>,
    in_body: bool,
    prioritized_count: u32,
    saw_eligible_image: bool,
    preloaded_srcs: HashSet<Vec<u8>>,
    author_preloaded_hrefs: HashSet<String>,
    link_headers_scanned: bool,
    picture_sources: HashMap<NodeId, PictureSource>,
    noscript_depth: u32,
}

impl PrioritizeLcpImagesFilter {
    /// Binds the filter to a resource driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        PrioritizeLcpImagesFilter {
            driver,
            head_element: None,
            in_body: false,
            prioritized_count: 0,
            saw_eligible_image: false,
            preloaded_srcs: HashSet::new(),
            author_preloaded_hrefs: HashSet::new(),
            link_headers_scanned: false,
            picture_sources: HashMap::new(),
            noscript_depth: 0,
        }
    }

    /// Loads the page variant's evidence for finder lookups.
    fn evidence(&self) -> BeaconEvidence {
        let server = self.driver.server_context();
        let oh = server.hasher().hash(self.driver.options_signature().as_bytes());
        let properties = self.driver.request_properties();
        let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
            properties.device_type(),
        );
        BeaconFinder::new(server.metadata_cache())
            .load(&self.driver.base_url().spec().to_string(), &oh, suffix)
            .into_record()
    }

    /// Whether the beacon finder has usable evidence.
    fn evidence_available(&self, evidence: &BeaconEvidence) -> bool {
        beacon_finder::availability(true, Some(evidence)) == Availability::Available
    }

    /// Notes a `<picture>` source candidate (first wins; `<video>`
    /// and `<audio>` sources never apply).
    fn note_picture_source(&mut self, dom: &mut Dom, id: NodeId) {
        if self.noscript_depth > 0 {
            return;
        }
        let Some(parent) = dom.arena_node(id).parent() else {
            return;
        };
        if !dom.element_name_equals(parent, b"picture") {
            return;
        }
        let Some(index) = dom.find_attribute(id, Keyword::Srcset) else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        let Some(srcset) = decoded else {
            return;
        };
        if srcset.is_empty() {
            return;
        }
        let entry = self.picture_sources.entry(parent).or_default();
        if entry.count == 0 {
            entry.srcset = srcset;
            if let Some(index) = dom.find_attribute(id, Keyword::Media) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(media) = decoded {
                    entry.media = media;
                }
            }
            if let Some(index) = dom.find_attribute(id, Keyword::Type) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(mime) = decoded {
                    entry.mime = mime;
                }
            }
        }
        entry.count += 1;
    }

    /// Notes an author image preload (`<link rel=preload as=image>`).
    fn note_author_preload(&mut self, dom: &mut Dom, id: NodeId) {
        if self.noscript_depth > 0 {
            return;
        }
        let rel = dom
            .find_attribute(id, Keyword::Rel)
            .and_then(|index| dom.decoded_attribute(id, index).0);
        let as_attr = dom
            .find_attribute(id, Keyword::As)
            .and_then(|index| dom.decoded_attribute(id, index).0);
        let href = dom
            .find_attribute(id, Keyword::Href)
            .and_then(|index| dom.decoded_attribute(id, index).0);
        let (Some(rel), Some(as_attr), Some(href)) = (rel, as_attr, href) else {
            return;
        };
        if !has_rel_token(&rel, "preload") || !ascii_eq(&as_attr, b"image") {
            return;
        }
        if href.is_empty() || ps_http::data_url::is_data_url(&href) {
            return;
        }
        let base = self.driver.base_url().clone();
        let reference = String::from_utf8_lossy(&href);
        let absolute = ps_http::google_url::GoogleUrl::resolve(&base, &reference);
        if absolute.is_valid() {
            self.author_preloaded_hrefs
                .insert(absolute.spec().to_string());
        }
    }

    /// Whether an element hides via `hidden` or inline
    /// display/visibility (ancestors included; stylesheets are
    /// invisible server-side).
    fn is_hidden(&self, dom: &mut Dom, mut id: NodeId) -> bool {
        loop {
            if dom.find_attribute_by_name(id, b"hidden").is_some() {
                return true;
            }
            if let Some(index) = dom.find_attribute(id, Keyword::Style) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(style) = decoded {
                    if inline_style_hides(&style) {
                        return true;
                    }
                }
            }
            let Some(parent) = dom.arena_node(id).parent() else {
                return false;
            };
            id = parent;
        }
    }

    /// Whether the element is a 1×1 (or smaller) tracker.
    fn looks_like_tiny_tracker(dom: &mut Dom, id: NodeId) -> bool {
        let width = dom
            .find_attribute(id, Keyword::Width)
            .and_then(|index| dom.decoded_attribute(id, index).0);
        let height = dom
            .find_attribute(id, Keyword::Height)
            .and_then(|index| dom.decoded_attribute(id, index).0);
        match (width, height) {
            (Some(width), Some(height)) => {
                let width_ok = std::str::from_utf8(&width)
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok());
                let height_ok = std::str::from_utf8(&height)
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok());
                matches!((width_ok, height_ok), (Some(w), Some(h)) if w <= 1 && h <= 1)
            }
            _ => false,
        }
    }

    /// Eligibility plus the raw src and absolute URL
    /// (`IsEligibleImage`).
    fn eligible_image(&self, dom: &mut Dom, id: NodeId) -> Option<(Vec<u8>, String)> {
        if self.is_hidden(dom, id) || Self::looks_like_tiny_tracker(dom, id) {
            return None;
        }
        let index = dom.find_attribute(id, Keyword::Src)?;
        let (decoded, _) = dom.decoded_attribute(id, index);
        let src = decoded?;
        if src.is_empty() || ps_http::data_url::is_data_url(&src) {
            return None;
        }
        let base = self.driver.base_url().clone();
        let reference = String::from_utf8_lossy(&src);
        let absolute = ps_http::google_url::GoogleUrl::resolve(&base, &reference);
        if !absolute.is_valid() {
            return None;
        }
        Some((src, absolute.spec().to_string()))
    }

    /// Whether an author preload (element or Link header) already
    /// covers a URL.
    fn is_covered_by_author_preload(&mut self, absolute_url: &str) -> bool {
        if !self.link_headers_scanned {
            self.link_headers_scanned = true;
            // Link header targets resolve against the request URL.
            let headers = self.driver.response_headers();
            collect_link_header_preloads(
                &headers,
                &self.driver.base_url().spec().to_string(),
                &mut self.author_preloaded_hrefs,
            );
        }
        self.author_preloaded_hrefs.contains(absolute_url)
    }

    /// Inserts the hero preload link (`InsertPreloadHint`).
    fn insert_preload_hint(&mut self, dom: &mut Dom, id: NodeId, src: &[u8], absolute_url: &str) {
        if self.is_covered_by_author_preload(absolute_url) {
            return;
        }
        // Selection precedence follows the browser: usable
        // `<source>` candidates win over the img's own srcset,
        // which wins over the bare src fallback.
        let mut imagesrcset = Vec::new();
        let mut media = Vec::new();
        let mut mime = Vec::new();
        if let Some(parent) = dom.arena_node(id).parent() {
            if let Some(sources) = self.picture_sources.get(&parent) {
                if sources.count > 1 {
                    // Viewport-dependent selection cannot be
                    // expressed in a single hint; element priority
                    // still applies.
                    return;
                }
                imagesrcset = sources.srcset.clone();
                media = sources.media.clone();
                mime = sources.mime.clone();
            }
        }
        if imagesrcset.is_empty() {
            if let Some(index) = dom.find_attribute(id, Keyword::Srcset) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(srcset) = decoded {
                    if !srcset.is_empty() {
                        imagesrcset = srcset;
                    }
                }
            }
        }
        if !self.preloaded_srcs.insert(src.to_vec()) {
            return;
        }
        let preload = dom.new_element(None, b"link");
        dom.add_attribute(preload, b"rel", b"preload");
        dom.add_attribute(preload, b"as", b"image");
        dom.add_attribute(preload, b"href", src);
        dom.add_attribute(preload, b"fetchpriority", b"high");
        if !imagesrcset.is_empty() {
            dom.add_attribute(preload, b"imagesrcset", &imagesrcset);
            // Sizes always live on the img, even for sources.
            if let Some(index) = dom.find_attribute_by_name(id, b"sizes") {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(sizes) = decoded {
                    if !sizes.is_empty() {
                        dom.add_attribute(preload, b"imagesizes", &sizes);
                    }
                }
            }
        }
        if !media.is_empty() {
            dom.add_attribute(preload, b"media", &media);
        }
        if !mime.is_empty() {
            dom.add_attribute(preload, b"type", &mime);
        }
        // Credentials must match or the preload and the image
        // land in different cache entries.
        if let Some(index) = dom.find_attribute_by_name(id, b"crossorigin") {
            let (decoded, _) = dom.decoded_attribute(id, index);
            dom.add_attribute(preload, b"crossorigin", &decoded.unwrap_or_default());
        }
        let append_to_head = self
            .head_element
            .is_some_and(|head| dom.can_append_child(head));
        if append_to_head {
            dom.append_child(self.head_element, preload);
        } else {
            // Fallback for malformed documents without a head.
            dom.insert_node_before_node(id, preload);
        }
    }

    /// Prioritizes the hero (`PrioritizeImage`): drops lazy
    /// loading, pins fetch priority, marks no-defer, preloads.
    fn prioritize_image(&mut self, dom: &mut Dom, id: NodeId, src: &[u8], absolute_url: &str) {
        // Ensure this candidate is not lazy/deferred.
        if let Some(index) = dom.find_attribute_by_name(id, b"loading") {
            let (decoded, _) = dom.decoded_attribute(id, index);
            if decoded.is_some_and(|value| ascii_eq(&value, b"lazy")) {
                dom.delete_attribute(id, Keyword::Loading);
            }
        }
        dom.delete_attribute(id, Keyword::Fetchpriority);
        dom.add_attribute(id, b"fetchpriority", b"high");
        if dom
            .find_attribute(id, Keyword::DataPagespeedNoDefer)
            .is_none()
        {
            dom.add_attribute(id, b"data-pagespeed-no-defer", b"1");
        }
        if dom
            .find_attribute(id, Keyword::PagespeedNoDefer)
            .is_none()
        {
            dom.add_attribute(id, b"pagespeed-no-defer", b"1");
        }
        self.insert_preload_hint(dom, id, src, absolute_url);
        self.prioritized_count += 1;
    }

    /// Lazy-hints a non-hero image, respecting author signals
    /// (`AddLazyLoadingHint`).
    fn add_lazy_loading_hint(dom: &mut Dom, id: NodeId) {
        if let Some(index) = dom.find_attribute(id, Keyword::Fetchpriority) {
            let (decoded, _) = dom.decoded_attribute(id, index);
            if decoded.is_some_and(|value| ascii_eq(&value, b"high")) {
                return;
            }
        }
        if let Some(index) = dom.find_attribute_by_name(id, b"loading") {
            let (decoded, _) = dom.decoded_attribute(id, index);
            if let Some(loading) = decoded {
                if ascii_eq(&loading, b"eager") {
                    return;
                }
                if !ascii_eq(&loading, b"lazy") {
                    dom.delete_attribute(id, Keyword::Loading);
                    dom.add_attribute(id, b"loading", b"lazy");
                }
            }
            return;
        }
        dom.add_attribute(id, b"loading", b"lazy");
    }

    /// Preloads a CSS-background LCP URL from learned evidence.
    /// Only when no image spent the single-preload budget, the
    /// author does not already cover the URL, a head exists to
    /// hold the hint, and the request supports the learned bytes'
    /// format (evidence is per device suffix, not per format, so
    /// a learned AVIF must not hint a non-AVIF browser).
    fn maybe_preload_background_lcp(&mut self, dom: &mut Dom) {
        if self.prioritized_count >= MAX_PRIORITIZED_IMAGES {
            return;
        }
        let evidence = self.evidence();
        if evidence.lcp_kind != beacon_finder::LCP_KIND_BACKGROUND
            || evidence.lcp_image_url.is_empty()
        {
            return;
        }
        let url = evidence.lcp_image_url.clone();
        if self.is_covered_by_author_preload(&url) {
            return;
        }
        let mut properties = self.driver.request_properties();
        let supports_avif = properties.supports_avif_rewritten_urls();
        let supports_webp = properties.supports_webp_rewritten_urls();
        if !bg_preload_format_supported(&url, supports_avif, supports_webp) {
            return;
        }
        if !self.preloaded_srcs.insert(url.as_bytes().to_vec()) {
            return;
        }
        let Some(head) = self.head_element else {
            return;
        };
        if !dom.can_append_child(head) {
            return;
        }
        // Backgrounds load without credentials, matching a
        // crossorigin-less preload exactly.
        let preload = dom.new_element(None, b"link");
        dom.add_attribute(preload, b"rel", b"preload");
        dom.add_attribute(preload, b"as", b"image");
        dom.add_attribute(preload, b"href", url.as_bytes());
        dom.add_attribute(preload, b"fetchpriority", b"high");
        dom.append_child(self.head_element, preload);
        self.prioritized_count += 1;
    }
}

impl HtmlFilter for PrioritizeLcpImagesFilter {
    fn name(&self) -> &'static str {
        "PrioritizeLcpImages"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        true
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.head_element = None;
        self.in_body = false;
        self.prioritized_count = 0;
        self.saw_eligible_image = false;
        self.preloaded_srcs.clear();
        self.author_preloaded_hrefs.clear();
        self.link_headers_scanned = false;
        self.picture_sources.clear();
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if keyword == Keyword::Noscript {
            self.noscript_depth += 1;
            return;
        }
        if keyword == Keyword::Head {
            self.head_element = Some(id);
            return;
        }
        if keyword == Keyword::Body {
            self.in_body = true;
            return;
        }
        if keyword == Keyword::Source {
            self.note_picture_source(dom, id);
            return;
        }
        if keyword == Keyword::Link {
            self.note_author_preload(dom, id);
            return;
        }
        if self.noscript_depth > 0 || !self.in_body {
            return;
        }
        if keyword != Keyword::Img || !dom.is_rewritable(id) {
            return;
        }
        let Some((src, absolute_url)) = self.eligible_image(dom, id) else {
            return;
        };
        // Prefer accepted LCP evidence over document position.
        let evidence = self.evidence();
        let evidence_available = self.evidence_available(&evidence);
        // The beacon reports hashes of ORIGINAL image URLs, but
        // this filter runs post-rewrite: prefer the stamped
        // original hash, falling back to the current src.
        let mut learned_critical = false;
        if evidence_available {
            if let Some(index) = dom.find_attribute(id, Keyword::DataPagespeedUrlHash) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(stamped) = decoded {
                    let stamped = String::from_utf8_lossy(&stamped).into_owned();
                    learned_critical =
                        beacon_finder::html_critical_images(&evidence).contains(&stamped);
                }
            } else {
                learned_critical =
                    beacon_finder::is_html_critical_image(&absolute_url, &evidence);
            }
            if !learned_critical {
                // Unstamped LCP images match by loaded URL: the
                // beacon reports the URL the browser fetched,
                // which equals the post-rewrite src observed here.
                learned_critical = !evidence.lcp_image_url.is_empty()
                    && evidence.lcp_image_url == absolute_url;
            }
        }
        if learned_critical {
            if self.prioritized_count < MAX_PRIORITIZED_IMAGES {
                self.prioritize_image(dom, id, &src, &absolute_url);
            }
            // Learned-critical images past the hero slot are left
            // alone, never lazy.
        } else if !self.saw_eligible_image && !evidence_available {
            // No evidence at all: the first eligible image is the
            // heuristic hero.
            self.prioritize_image(dom, id, &src, &absolute_url);
        } else if !self.saw_eligible_image {
            // Evidence exists but names other images: leave the
            // first image alone rather than spending the single
            // preload on a contradicted guess.
        } else {
            Self::add_lazy_loading_hint(dom, id);
        }
        self.saw_eligible_image = true;
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if keyword == Keyword::Body {
            self.in_body = false;
        } else if keyword == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
    }

    fn end_document(&mut self, dom: &mut Dom, _lexer: &Lexer) {
        self.maybe_preload_background_lcp(dom);
    }
}

/// ASCII case-insensitive byte equality.
fn ascii_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

/// Whether an inline style hides its element (`display:none` or
/// `visibility:hidden`, `!important`-tolerant).
fn inline_style_hides(style_value: &[u8]) -> bool {
    let text = String::from_utf8_lossy(style_value);
    for declaration in text.split(';') {
        let Some((name, mut value)) = declaration.split_once(':') else {
            continue;
        };
        let name = name.trim();
        value = value.trim();
        if let Some(stripped) = value
            .strip_suffix("!important")
            .or_else(|| value.strip_suffix("!IMPORTANT"))
        {
            value = stripped.trim();
        }
        if (name.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
            || (name.eq_ignore_ascii_case("visibility") && value.eq_ignore_ascii_case("hidden"))
        {
            return true;
        }
    }
    false
}

/// Whether a token appears in a space-separated rel list.
fn has_rel_token(rel_value: &[u8], token: &str) -> bool {
    String::from_utf8_lossy(rel_value)
        .split([' ', '\t'])
        .any(|part| part.eq_ignore_ascii_case(token))
}

/// Collects absolutized author image preloads from Link response
/// headers (RFC 8288 entries with rel preload and as image).
fn collect_link_header_preloads(
    headers: &ps_http::response::ResponseHeaders,
    base_url: &str,
    out: &mut HashSet<String>,
) {
    let Some(values) = headers.headers().lookup("Link") else {
        return;
    };
    for value in values {
        // Split top-level link-values on commas, honoring quotes.
        let bytes = value.as_bytes();
        let mut pos = 0;
        while pos < bytes.len() {
            let mut in_quotes = false;
            let mut end = pos;
            while end < bytes.len() {
                if bytes[end] == b'"' {
                    in_quotes = !in_quotes;
                } else if bytes[end] == b',' && !in_quotes {
                    break;
                }
                end += 1;
            }
            let entry = &bytes[pos..end];
            pos = if end < bytes.len() { end + 1 } else { end };
            let entry = trim_bytes(entry);
            if !entry.starts_with(b"<") {
                continue;
            }
            let Some(close) = entry.iter().position(|byte| *byte == b'>') else {
                continue;
            };
            if close < 2 {
                continue;
            }
            let target = trim_bytes(&entry[1..close]);
            let mut is_preload = false;
            let mut is_image = false;
            for part in entry[close + 1..].split(|byte| *byte == b';') {
                let Some(eq) = part.iter().position(|byte| *byte == b'=') else {
                    continue;
                };
                let name = trim_bytes(&part[..eq]);
                let mut param = trim_bytes(&part[eq + 1..]).to_vec();
                if param.len() >= 2 && param.starts_with(b"\"") && param.ends_with(b"\"") {
                    param = param[1..param.len() - 1].to_vec();
                }
                if ascii_eq(&name, b"rel") && has_rel_token(&param, "preload") {
                    is_preload = true;
                } else if ascii_eq(&name, b"as") && ascii_eq(&param, b"image") {
                    is_image = true;
                }
            }
            if is_preload && is_image && !target.is_empty() && !ps_http::data_url::is_data_url(target)
            {
                let base = ps_http::google_url::GoogleUrl::new(base_url);
                let reference = String::from_utf8_lossy(target);
                let absolute =
                    ps_http::google_url::GoogleUrl::resolve(&base, &reference);
                if absolute.is_valid() {
                    out.insert(absolute.spec().to_string());
                }
            }
        }
    }
}

/// Trims ASCII whitespace from both ends.
fn trim_bytes(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

/// Whether the request can use learned background-LCP bytes:
/// AVIF/WebP need matching support, universal raster kinds
/// always pass, and anything unrecognized (or extensionless)
/// fails closed so one variant never hints another's bytes.
fn bg_preload_format_supported(url: &str, supports_avif: bool, supports_webp: bool) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let Some(extension) = path.rsplit('.').next() else {
        return false;
    };
    // No dot, a trailing dot, or a slash after the last dot
    // (the dot belongs to a directory): not a usable extension.
    if !path.contains('.') || extension.is_empty() || extension.contains('/') {
        return false;
    }
    if extension.eq_ignore_ascii_case("avif") {
        supports_avif
    } else if extension.eq_ignore_ascii_case("webp") {
        supports_webp
    } else {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "svg" | "ico" | "bmp"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::bg_preload_format_supported;

    #[test]
    fn format_matrix() {
        // AVIF/WebP follow capabilities.
        assert!(bg_preload_format_supported("http://h/x.avif", true, false));
        assert!(!bg_preload_format_supported(
            "http://h/x.avif",
            false,
            true
        ));
        assert!(bg_preload_format_supported(
            "http://h/x.webp",
            false,
            true
        ));
        assert!(!bg_preload_format_supported(
            "http://h/x.webp",
            false,
            false
        ));
        // Universal kinds always pass.
        assert!(bg_preload_format_supported(
            "http://h/x.jpg",
            false,
            false
        ));
        assert!(bg_preload_format_supported(
            "http://h/x.JPEG?y=1",
            false,
            false
        ));
        assert!(bg_preload_format_supported(
            "http://h/x.png#frag",
            false,
            false
        ));
        // Unknown or missing extensions fail closed.
        assert!(!bg_preload_format_supported("http://h/x.jxl", true, true));
        assert!(!bg_preload_format_supported("http://h/noext", true, true));
        assert!(!bg_preload_format_supported(
            "http://h/dir.name/x",
            true,
            true
        ));
        assert!(!bg_preload_format_supported("http://h/x.", true, true));
    }
}
