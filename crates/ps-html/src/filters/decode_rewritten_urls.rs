//! Rewritten-URL decoding (`DecodeRewrittenUrlsFilter`).
//!
//! Replaces singly-encoded `.pagespeed.` URLs with their originals so
//! downstream consumers never see rewritten names; combined (multi-URL)
//! encodings are left alone. Decoding runs through
//! [`decode_url`](crate::decode_url::decode_url) with driver-supplied
//! options, namer, and hash/signature lengths. Rewrite logging stays
//! driver-side (no logging channel here yet).

use ps_config::options::RewriteOptions;
use ps_http::data_url::is_data_url;
use ps_http::google_url::GoogleUrl;

use crate::base_tracker::BaseTracker;
use crate::decode_url::{decode_url, DecodeConfig};
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::parser::Dom;
use crate::resource_tag_scanner::scan_element;
use crate::url_namer::UrlNamer;

use super::url_left_trim::UrlValuedAttributes;

/// Driver inputs for URL decoding.
#[derive(Debug, Clone, Default)]
pub struct DecodeRewrittenUrlsConfig {
    /// Rewrite options (oblivious gate + forbidden list).
    pub options: RewriteOptions,
    /// Hasher length in characters (hash/signature split).
    pub hash_length: usize,
    /// Signature length in characters (0 unless URL signing).
    pub signature_length: usize,
    /// Operator URL-valued attributes.
    pub url_valued: UrlValuedAttributes,
}

/// Decodes rewritten URLs (`DecodeRewrittenUrlsFilter`).
#[derive(Debug)]
pub struct DecodeRewrittenUrlsFilter<N: UrlNamer> {
    config: DecodeRewrittenUrlsConfig,
    namer: N,
    base: BaseTracker,
}

impl<N: UrlNamer> DecodeRewrittenUrlsFilter<N> {
    /// Builds the filter from driver inputs.
    #[must_use]
    pub fn new(config: DecodeRewrittenUrlsConfig, namer: N) -> Self {
        DecodeRewrittenUrlsFilter {
            config,
            namer,
            base: BaseTracker::default(),
        }
    }
}

impl<N: UrlNamer> HtmlFilter for DecodeRewrittenUrlsFilter<N> {
    fn name(&self) -> &'static str {
        "DecodeRewrittenUrls"
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
        let found: Vec<usize> = scan_element(dom, id, &self.config.url_valued)
            .iter()
            .map(|(index, _)| *index)
            .collect();
        for index in found {
            let (decoded, _error) = dom.decoded_attribute(id, index);
            let Some(url) = decoded else { continue };
            if url.is_empty() || is_data_url(&url) {
                continue;
            }
            let reference = String::from_utf8_lossy(&url);
            let resolved = GoogleUrl::resolve(&self.base.base(), &reference);
            if !resolved.is_web_valid() {
                continue;
            }
            let decoded = decode_url(
                &resolved,
                &DecodeConfig {
                    options: &self.config.options,
                    namer: &self.namer,
                    hash_length: self.config.hash_length,
                    signature_length: self.config.signature_length,
                },
            );
            // Combined encodings name several URLs; only singles replace.
            if let Some(decoded) = decoded {
                if decoded.len() == 1 {
                    dom.element_attributes_mut(id)[index].set_value(decoded[0].as_bytes());
                }
            }
        }
    }
}
