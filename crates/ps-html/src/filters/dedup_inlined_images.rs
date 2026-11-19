//! Repeated inlined-image elimination (`DedupInlinedImagesFilter`).
//!
//! The first occurrence of an inlined image (a `data:image/...` URL
//! longer than the restoring snippet) gains an `id`; later copies
//! lose their `src` and grow a `<script>` that copies the first
//! image's pixels at runtime. The utility functions ride one shared
//! script inserted before the first duplicate.

use std::collections::HashMap;
use std::rc::Rc;

use ps_http::data_url::is_data_image_url;

use super::common::{add_js_to_element, XhtmlStatus};
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::static_assets::{get_asset, StaticAsset};

/// Skip images this short or shorter (`kMinimumImageCutoff`): the
/// restoring snippet is about this big, so shorter images save nothing.
pub const MINIMUM_IMAGE_CUTOFF: usize = 241;

/// Utility-script initializer (`kDiiInitializer`).
pub const DII_INITIALIZER: &[u8] = b"pagespeed.dedupInlinedImagesInit();";

/// Found-candidates statistic (`kCandidatesFound`).
pub const CANDIDATES_FOUND: &str = "num_dedup_inlined_images_candidates_found";

/// Replaced-candidates statistic (`kCandidatesReplaced`).
pub const CANDIDATES_REPLACED: &str = "num_dedup_inlined_images_candidates_replaced";

/// Content hash (`server_context()->hasher()->Hash`); tests use the
/// `MockHasher` stand-in, which answers `"0"` for every input.
pub type ContentHash = Rc<dyn Fn(&[u8]) -> String>;

/// Driver inputs for inlined-image dedup.
#[derive(Clone)]
pub struct DedupInlinedImagesConfig {
    /// Content hasher (keys the first-seen map).
    pub hasher: ContentHash,
    /// Whether the user agent runs the lazyload-style runtime
    /// (`RequestProperties::SupportsLazyloadImages`).
    pub supports_lazyload_images: bool,
    /// Whether this is an XHR (`IsXmlHttpRequest`).
    pub is_xml_http_request: bool,
    /// `kDebug` (debug asset builds).
    pub debug: bool,
    /// `kPedantic` (script type attributes).
    pub pedantic: bool,
    /// Response mimetype XHTML certainty.
    pub xhtml: XhtmlStatus,
}

impl Default for DedupInlinedImagesConfig {
    fn default() -> Self {
        DedupInlinedImagesConfig {
            hasher: Rc::new(|_| "0".to_string()),
            supports_lazyload_images: true,
            is_xml_http_request: false,
            debug: false,
            pedantic: false,
            xhtml: XhtmlStatus::IsNotXhtml,
        }
    }
}

/// Dedups repeated inlined images (`DedupInlinedImagesFilter`).
pub struct DedupInlinedImagesFilter {
    config: DedupInlinedImagesConfig,
    /// Utility script inserted yet.
    script_inserted: bool,
    /// First-seen image id by content hash.
    hash_to_id: HashMap<String, String>,
    /// Monotonic snippet counter.
    snippet_id: u32,
    /// Nested `<noscript>` depth (deduping inside is pointless).
    noscript_depth: u32,
    /// Inlined images seen (`kCandidatesFound`).
    candidates_found: u64,
    /// Duplicates replaced with scripts (`kCandidatesReplaced`).
    candidates_replaced: u64,
}

impl DedupInlinedImagesFilter {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: DedupInlinedImagesConfig) -> Self {
        DedupInlinedImagesFilter {
            config,
            script_inserted: false,
            hash_to_id: HashMap::new(),
            snippet_id: 0,
            noscript_depth: 0,
            candidates_found: 0,
            candidates_replaced: 0,
        }
    }

    /// Returns the statistic counters (found, replaced).
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        (self.candidates_found, self.candidates_replaced)
    }

    /// Raw `src` when the element is a dedup candidate
    /// (`IsDedupCandidate`): an `<img>` outside noscript whose raw
    /// `src` is a data-image URL longer than the cutoff.
    fn dedup_candidate_src<'a>(&self, dom: &'a Dom, id: NodeId) -> Option<&'a [u8]> {
        if self.noscript_depth > 0 || dom.element_keyword(id) != Keyword::Img {
            return None;
        }
        let index = dom.find_attribute(id, Keyword::Src)?;
        let src = dom.element_attributes(id)[index].escaped_value()?;
        if is_data_image_url(src) && src.len() > MINIMUM_IMAGE_CUTOFF {
            Some(src)
        } else {
            None
        }
    }

    /// Inserts the utility script before a duplicate
    /// (`InsertOurScriptElement`).
    fn insert_utility_script(&mut self, dom: &mut Dom, lexer: &Lexer, before: NodeId) {
        let mut js = get_asset(StaticAsset::DedupInlinedImages, self.config.debug).to_vec();
        js.extend_from_slice(DII_INITIALIZER);
        let parent = dom.arena_node(before).parent();
        let script = dom.new_element(parent, b"script");
        dom.insert_node_before_node(before, script);
        add_js_to_element(
            dom,
            lexer,
            script,
            &js,
            self.config.pedantic,
            self.config.xhtml,
        );
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        self.script_inserted = true;
    }
}

impl HtmlFilter for DedupInlinedImagesFilter {
    fn name(&self) -> &'static str {
        "DedupInlinedImages"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        // Like lazyload images (which this imitates), disabled without
        // the runtime or inside XHRs.
        self.config.supports_lazyload_images && !self.config.is_xml_http_request
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.script_inserted = false;
        self.snippet_id = 0;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        // Before the first duplicate is rewritten, its runtime must be
        // in place (never inside noscript — that would be dumb).
        if self.script_inserted {
            return;
        }
        if let Some(src) = self.dedup_candidate_src(dom, id) {
            let hash = (self.config.hasher)(src);
            if self.hash_to_id.contains_key(&hash) {
                self.insert_utility_script(dom, lexer, id);
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
        }
        let Some(src) = self.dedup_candidate_src(dom, id).map(<[u8]>::to_vec) else {
            return;
        };
        self.candidates_found += 1;
        let hash = (self.config.hasher)(&src);
        // Source and destination both need an id; keep an existing one.
        let existing = dom
            .find_attribute(id, Keyword::Id)
            .and_then(|index| dom.element_attributes(id)[index].escaped_value())
            .filter(|id| !id.is_empty())
            .map(<[u8]>::to_vec);
        let element_id = match existing {
            Some(id) => id,
            None => {
                self.snippet_id += 1;
                let generated = format!("pagespeed_img_{hash}{}", self.snippet_id);
                dom.add_attribute(id, b"id", generated.as_bytes());
                generated.into_bytes()
            }
        };
        if let Some(from) = self.hash_to_id.get(&hash).cloned() {
            // A repeat: drop the bytes, restore them from the first copy.
            debug_assert!(self.script_inserted);
            self.candidates_replaced += 1;
            self.snippet_id += 1;
            let script_id = format!("pagespeed_script_{}", self.snippet_id);
            let mut snippet = b"if(window.pagespeed){if(pagespeed.dedupInlinedImages){pagespeed.dedupInlinedImages.".to_vec();
            snippet.extend_from_slice(b"inlineImg('");
            snippet.extend_from_slice(from.as_bytes());
            snippet.extend_from_slice(b"','");
            snippet.extend_from_slice(&element_id);
            snippet.extend_from_slice(b"','");
            snippet.extend_from_slice(script_id.as_bytes());
            snippet.extend_from_slice(b"');}}");
            let script = dom.new_element(dom.arena_node(id).parent(), b"script");
            dom.insert_node_after_node(id, script);
            add_js_to_element(
                dom,
                lexer,
                script,
                &snippet,
                self.config.pedantic,
                self.config.xhtml,
            );
            dom.add_attribute(script, b"id", script_id.as_bytes());
            dom.add_escaped_attribute(
                script,
                b"data-pagespeed-no-defer",
                None,
                crate::element::QuoteStyle::DoubleQuote,
            );
            dom.delete_attribute(id, Keyword::Src);
        } else {
            self.hash_to_id
                .insert(hash, String::from_utf8_lossy(&element_id).into_owned());
        }
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.hash_to_id.clear();
    }
}
