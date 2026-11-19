//! Proxy-rewritten resource counting (`RewrittenContentScanningFilter`).
//!
//! Counts image, script, stylesheet, and other-resource URLs that land on
//! the proxy domain (hyperlinks, prefetches, and empty values never
//! count). Upstream the end-of-document count lands in the DOM-cohort
//! property cache under [`NUM_PROXIED_REWRITTEN_RESOURCES_PROPERTY`];
//! that write stays driver-side until the property cache lands, and the
//! count reads back here.

use ps_config::options::Category;
use ps_http::google_url::GoogleUrl;

use crate::base_tracker::BaseTracker;
use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::lexer::Lexer;
use crate::parser::Dom;
use crate::resource_tag_scanner::scan_element;
use crate::url_namer::UrlNamer;

use super::url_left_trim::UrlValuedAttributes;

/// Property-cache property for the count
/// (`kNumProxiedRewrittenResourcesProperty`).
pub const NUM_PROXIED_REWRITTEN_RESOURCES_PROPERTY: &str = "num_proxied_rewritten_resources";

/// Counts proxy-encoded resources (`RewrittenContentScanningFilter`).
#[derive(Debug)]
pub struct RewrittenContentScanningFilter<N: UrlNamer> {
    namer: N,
    base: BaseTracker,
    url_valued: UrlValuedAttributes,
    num_proxied_rewritten_resources: i32,
}

impl<N: UrlNamer> RewrittenContentScanningFilter<N> {
    /// Builds the filter around a URL namer.
    #[must_use]
    pub fn new(namer: N, url_valued: UrlValuedAttributes) -> Self {
        RewrittenContentScanningFilter {
            namer,
            base: BaseTracker::default(),
            url_valued,
            num_proxied_rewritten_resources: 0,
        }
    }

    /// Proxy-encoded resources seen.
    #[must_use]
    pub fn num_proxied_rewritten_resources(&self) -> i32 {
        self.num_proxied_rewritten_resources
    }
}

impl<N: UrlNamer> HtmlFilter for RewrittenContentScanningFilter<N> {
    fn name(&self) -> &'static str {
        "RewrittenContentScanning"
    }

    fn start_document(&mut self, _dom: &mut Dom, lexer: &Lexer) {
        let url = String::from_utf8_lossy(lexer.document_url());
        self.base.reset(&url);
        self.num_proxied_rewritten_resources = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.base.observe(dom, id);
        let found: Vec<(usize, Category)> = scan_element(dom, id, &self.url_valued);
        for (index, category) in found {
            match category {
                Category::Image
                | Category::Script
                | Category::Stylesheet
                | Category::OtherResource => {}
                Category::Hyperlink | Category::Prefetch => continue,
            }
            let (decoded, _error) = dom.decoded_attribute(id, index);
            let Some(url) = decoded else { continue };
            if url.is_empty() {
                continue;
            }
            let reference = String::from_utf8_lossy(&url);
            let resolved = GoogleUrl::resolve(&self.base.base(), &reference);
            if self.namer.is_proxy_encoded(&resolved) {
                self.num_proxied_rewritten_resources += 1;
            }
        }
    }
}
