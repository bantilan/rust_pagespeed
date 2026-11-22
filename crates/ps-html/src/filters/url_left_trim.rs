//! Left-trimming absolute URLs (`UrlLeftTrimFilter`).
//!
//! Rewrites URL-valued attributes to the shortest relative form that still
//! resolves identically: same-origin URLs lose the origin and, when the
//! target stays under the base directory, the shared directory prefix. The
//! pure [`UrlLeftTrimFilter::trim`] ports `Trim` byte-for-byte (including
//! the `//`-path, bare-query, and colon guards plus the re-resolve safety
//! net); [`UrlLeftTrimFilter`] applies it to every scanned attribute except
//! on `<base>` elements, gated on a web-valid live base.
//!
//! Trim counters publish through [`UrlLeftTrimFilter::init_stats`]
//! (`InitStats`); without it the filter counts locally only.

use std::sync::Arc;

use ps_config::options::Category;
use ps_http::google_url::GoogleUrl;
use ps_stats::core::{Statistics, Variable};

use crate::base_tracker::BaseTracker;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource_tag_scanner::scan_element;

/// Successful trims (`url_trims`).
pub const URL_TRIMS: &str = "url_trims";
/// Bytes saved (`url_trim_saved_bytes`).
pub const URL_TRIM_SAVED_BYTES: &str = "url_trim_saved_bytes";

/// Operator URL-valued attributes (passed through to the scanner).
pub type UrlValuedAttributes = Vec<(String, String, Category)>;

/// Resolved statistics counters (`None` until [`UrlLeftTrimFilter::init_stats`]).
#[derive(Debug, Default)]
struct UrlLeftTrimStats {
    trims: Option<Arc<Variable>>,
    saved_bytes: Option<Arc<Variable>>,
}

/// Trims absolute URLs down to relative ones (`UrlLeftTrimFilter`).
#[derive(Debug)]
pub struct UrlLeftTrimFilter {
    base: BaseTracker,
    url_valued: UrlValuedAttributes,
    trim_count: i64,
    trim_saved_bytes: i64,
    stats: UrlLeftTrimStats,
}

impl UrlLeftTrimFilter {
    /// Builds the filter (base resets from the document URL).
    #[must_use]
    pub fn new(url_valued: UrlValuedAttributes) -> Self {
        UrlLeftTrimFilter {
            base: BaseTracker::default(),
            url_valued,
            trim_count: 0,
            trim_saved_bytes: 0,
            stats: UrlLeftTrimStats::default(),
        }
    }

    /// Resolves statistics counters (`InitStats`).
    pub fn init_stats(&mut self, statistics: &Statistics) {
        self.stats.trims = Some(statistics.add_variable(URL_TRIMS));
        self.stats.saved_bytes = Some(statistics.add_variable(URL_TRIM_SAVED_BYTES));
    }

    /// Shortens `url` against `base` (`Trim`); `None` keeps the original.
    ///
    /// The origin and directory prefixes index into the resolved spec by
    /// byte length, so callers must pass canonical specs (as resolution
    /// produces); anything inconsistent fails closed through the
    /// re-resolve check below.
    #[must_use]
    pub fn trim(base: &GoogleUrl, url: &str) -> Option<String> {
        if !base.is_web_valid() || url.is_empty() {
            return None;
        }
        let long_url = GoogleUrl::resolve(base, url);
        if !long_url.is_web_valid() {
            return None;
        }
        let buffer = long_url.spec();
        let mut to_trim = 0;
        let origin = base.origin();
        if origin.len() < buffer.len() && long_url.origin() == origin {
            to_trim = origin.len();
            let path = base.path_sans_leaf();
            // A path starting with "//" is not the same resource with one
            // slash, so the origin must stay.
            if buffer[to_trim..].starts_with("//") {
                to_trim = 0;
            } else if to_trim + path.len() < buffer.len()
                && long_url.path_sans_leaf().starts_with(path)
            {
                // Bare `?query` URLs keep their leading slash (`?a=b`
                // would resolve against the base leaf instead).
                if !long_url.has_query() || !long_url.leaf_sans_query().is_empty() {
                    to_trim += path.len();
                    // Undo when the remainder would start a
                    // network-path, fragment, or query reference.
                    if matches!(buffer.as_bytes()[to_trim], b'/' | b'#' | b'?') {
                        to_trim -= path.len();
                    }
                }
            }
        }
        // Scheme-only stripping stays disabled (IE8 double-fetch).
        let trimmed = &buffer[to_trim..];
        if trimmed.len() < url.len() {
            // A colon before any slash either keeps the scheme (no
            // savings) or unmasks scheme-looking path junk; revert.
            if let Some(colon) = trimmed.find(':') {
                if !trimmed[..colon].contains('/') {
                    return None;
                }
            }
            let resolved = GoogleUrl::resolve(base, trimmed);
            if !resolved.is_web_valid() || resolved != long_url {
                return None;
            }
            return Some(trimmed.to_string());
        }
        None
    }

    /// Successful trims (`url_trims`).
    #[must_use]
    pub fn trim_count(&self) -> i64 {
        self.trim_count
    }

    /// Bytes saved (`url_trim_saved_bytes`).
    #[must_use]
    pub fn trim_saved_bytes(&self) -> i64 {
        self.trim_saved_bytes
    }

    /// Trims one attribute value in place (`TrimAttribute`).
    fn trim_attribute(&mut self, dom: &mut Dom, id: NodeId, index: usize) {
        let (decoded, _error) = dom.decoded_attribute(id, index);
        if let Some(value) = decoded {
            if !value.is_empty() {
                let original = String::from_utf8_lossy(&value);
                if let Some(trimmed) = Self::trim(&self.base.base(), &original) {
                    self.trim_count += 1;
                    let saved = original.len() as i64 - trimmed.len() as i64;
                    self.trim_saved_bytes += saved;
                    if let Some(trims) = &self.stats.trims {
                        trims.add(1);
                    }
                    if let Some(saved_bytes) = &self.stats.saved_bytes {
                        saved_bytes.add(saved);
                    }
                    dom.element_attributes_mut(id)[index].set_value(trimmed.as_bytes());
                }
            }
        }
    }
}

impl HtmlFilter for UrlLeftTrimFilter {
    fn name(&self) -> &'static str {
        "UrlLeftTrim"
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
        if dom.element_keyword(id) == Keyword::Base || !self.base.base_url_is_valid() {
            return;
        }
        // Collect indices first: trimming mutates attribute values.
        let indices: Vec<usize> = scan_element(dom, id, &self.url_valued)
            .iter()
            .map(|(index, _)| *index)
            .collect();
        for index in indices {
            self.trim_attribute(dom, id, index);
        }
    }
}
