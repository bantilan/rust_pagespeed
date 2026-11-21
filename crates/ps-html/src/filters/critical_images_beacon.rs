//! Critical-images beacon insertion (`CriticalImagesBeaconFilter`).
//!
//! Tracks candidate image URL hashes on every page, stamps
//! `data-pagespeed-url-hash` once evidence exists (or a beacon is
//! due), and inserts the beacon JS with a fresh nonce before the
//! first image when sampling is due. End of document registers
//! candidates and snapshots the post-render evidence identity for
//! the HTML cache. The filter is always enabled when chained (it
//! must see original image URLs, hence pre-render); insertion
//! itself needs UA beacon support.
#![forbid(unsafe_code)]

use crate::beacon_finder::{self, Availability, BeaconFinder, DownstreamBeacon};
use crate::element::NodeId;
use crate::name::Keyword;
use crate::filter::HtmlFilter;
use crate::js_escape::escape_to_js_string_literal;
use crate::lexer::Lexer;
use crate::parser::Dom;
use crate::server_context::ResourceDriver;
use crate::static_assets::{get_asset, StaticAsset};
use ps_config::filter::Filter;
use std::collections::HashSet;
use std::rc::Rc;

/// Nginx beacon endpoint fallback (the host default the options
/// layer leaves to the caller).
const DEFAULT_BEACON_URL: &str = "/ngx_pagespeed_beacon";

/// Critical-images beacon filter.
pub struct CriticalImagesBeaconFilter {
    driver: Rc<ResourceDriver>,
    image_url_hashes: HashSet<String>,
    nonce: Option<String>,
    options_hash: String,
    insert_beacon_js: bool,
    stamp_hashes: bool,
    added_beacon_js: bool,
    noscript_depth: u32,
    prepared: bool,
}

impl CriticalImagesBeaconFilter {
    /// Binds the filter to a resource driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        CriticalImagesBeaconFilter {
            driver,
            image_url_hashes: HashSet::new(),
            nonce: None,
            options_hash: String::new(),
            insert_beacon_js: false,
            stamp_hashes: false,
            added_beacon_js: false,
            noscript_depth: 0,
            prepared: false,
        }
    }

    /// Current wall time in ms (the mock timer never advances in
    /// production, so sampling runs on the clock).
    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|age| age.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Downstream-cache beacon gate for this request.
    fn downstream_gate(&self) -> DownstreamBeacon {
        let options = self.driver.options();
        let headers = self.driver.request_headers();
        let header = headers.headers().lookup1("PS-ShouldBeacon");
        beacon_finder::downstream_beacon_gate(&options, header.as_deref())
    }

    /// Prepares insertion state once per document
    /// (`DetermineEnabled`): loads evidence, issues a nonce when
    /// sampling is due, and decides stamping.
    fn prepare(&mut self) {
        if self.prepared {
            return;
        }
        self.prepared = true;
        let properties = self.driver.request_properties();
        if !properties.supports_critical_images_beacon() {
            return;
        }
        let server = Rc::clone(self.driver.server_context());
        let options = self.driver.options();
        let oh = server.hasher().hash(options.signature().as_bytes());
        self.options_hash = oh.clone();
        let device = properties.device_type();
        let suffix =
            ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(device);
        let page_url = self.driver.base_url().spec().to_string();
        let finder = BeaconFinder::new(server.metadata_cache());
        let mut evidence = finder.load(&page_url, &oh, suffix).into_record();
        let downstream = self.downstream_gate();
        let now_ms = Self::now_ms();
        let due = beacon_finder::should_beacon(now_ms, Some(&evidence), downstream);
        let nonce_value = ((server.simple_random_next() as u64) << 32)
            | server.simple_random_next() as u64;
        let reinstrument = options.beacon_reinstrument_time_sec();
        if due {
            self.nonce = beacon_finder::prepare_beacon(
                &mut evidence,
                now_ms,
                reinstrument,
                nonce_value,
            );
        }
        self.insert_beacon_js = self.nonce.is_some();
        self.stamp_hashes = self.insert_beacon_js
            || beacon_finder::availability(true, Some(&evidence)) == Availability::Available;
        if self.nonce.is_some() {
            finder.store(&page_url, &oh, suffix, &evidence);
        }
        // Snapshot the pre-render identity; end of document
        // refreshes it after candidate registration.
        self.driver
            .set_beacon_evidence_identity(beacon_finder::encode_evidence(&evidence));
    }

    /// Builds the beacon init call (`Run(...)`) appended to the
    /// static beacon JS.
    fn init_call(&self) -> Vec<u8> {
        let options = self.driver.options();
        let is_https = self.driver.base_url().scheme() == "https";
        let beacon_url = options
            .beacon_url()
            .map(|urls| {
                if is_https {
                    urls.https
                } else {
                    urls.http
                }
            })
            .unwrap_or_else(|| DEFAULT_BEACON_URL.to_string());
        let nonce = self.nonce.clone().unwrap_or_default();
        let send_at_onload = !options.filters().enabled(Filter::LazyloadImages);
        let resize = options.filters().enabled(Filter::ResizeToRenderedImageDimensions);
        let debug = options.filters().enabled(Filter::Debug);
        let mut js = get_asset(StaticAsset::CriticalImagesBeaconJs, debug).to_vec();
        js.extend_from_slice(b"\npagespeed.CriticalImages.Run('");
        js.extend_from_slice(&escape_to_js_string_literal(beacon_url.as_bytes(), false));
        js.extend_from_slice(b"','");
        js.extend_from_slice(&escape_to_js_string_literal(
            self.driver.base_url().spec().as_bytes(),
            false,
        ));
        js.extend_from_slice(b"','");
        js.extend_from_slice(&escape_to_js_string_literal(
            self.options_hash.as_bytes(),
            false,
        ));
        js.extend_from_slice(b"',");
        js.extend_from_slice(if send_at_onload { b"true" } else { b"false" });
        js.extend_from_slice(b",");
        js.extend_from_slice(if resize { b"true" } else { b"false" });
        js.extend_from_slice(b",'");
        js.extend_from_slice(&escape_to_js_string_literal(nonce.as_bytes(), false));
        js.extend_from_slice(b"');");
        js
    }

    /// Inserts the beacon script before the current node, once.
    fn maybe_add_beacon_javascript(&mut self, dom: &mut Dom, lexer: &Lexer) {
        if !self.insert_beacon_js || self.added_beacon_js {
            return;
        }
        self.added_beacon_js = true;
        let js = self.init_call();
        let script = dom.new_element(None, b"script");
        dom.add_escaped_attribute(
            script,
            b"data-pagespeed-no-defer",
            None,
            crate::element::QuoteStyle::DoubleQuote,
        );
        dom.insert_node_before_current(script);
        let xhtml = match self.driver.mime_type_xhtml_status() {
            crate::server_context::XhtmlStatus::IsXhtml => {
                crate::filters::common::XhtmlStatus::IsXhtml
            }
            _ => crate::filters::common::XhtmlStatus::IsNotXhtml,
        };
        crate::filters::common::add_js_to_element(dom, lexer, script, &js, false, xhtml);
        self.driver
            .server_context()
            .variable(beacon_finder::BEACON_ADDED_COUNT)
            .add(1);
    }

    /// Hashes an image's original absolute src, collecting the
    /// candidate and stamping when active.
    fn note_image(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        if self.noscript_depth > 0 || !dom.is_rewritable(id) {
            return;
        }
        let Some(index) = dom.find_attribute(id, Keyword::Src) else {
            return;
        };
        let (decoded, _) = dom.decoded_attribute(id, index);
        let Some(src) = decoded else {
            return;
        };
        let base = self.driver.base_url().clone();
        let reference = String::from_utf8_lossy(&src);
        let absolute = ps_http::google_url::GoogleUrl::resolve(&base, &reference);
        if !absolute.is_valid() {
            return;
        }
        let hash = beacon_finder::url_hash(absolute.spec());
        self.image_url_hashes.insert(hash.clone());
        if self.stamp_hashes {
            dom.add_escaped_attribute(
                id,
                b"data-pagespeed-url-hash",
                Some(hash.as_bytes()),
                crate::element::QuoteStyle::DoubleQuote,
            );
            if self.insert_beacon_js {
                self.maybe_add_beacon_javascript(dom, lexer);
            }
        }
    }
}

impl HtmlFilter for CriticalImagesBeaconFilter {
    fn name(&self) -> &'static str {
        "CriticalImagesBeacon"
    }

    fn determine_enabled(&mut self, _dom: &Dom, _lexer: &Lexer) -> bool {
        self.prepare();
        true
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        // Per-render transient state only: prepared insertion
        // state survives (determine runs before this).
        self.image_url_hashes.clear();
        self.added_beacon_js = false;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        let keyword = dom.element_keyword(id);
        if keyword == Keyword::Noscript {
            self.noscript_depth = self.noscript_depth.saturating_sub(1);
            return;
        }
        if keyword != Keyword::Img && keyword != Keyword::Input {
            return;
        }
        if keyword == Keyword::Img {
            self.note_image(dom, lexer, id);
        }
    }

    fn end_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.finish_document();
        // Reset for filter reuse (`Clear`).
        self.image_url_hashes.clear();
        self.nonce = None;
        self.options_hash.clear();
        self.insert_beacon_js = false;
        self.stamp_hashes = false;
        self.added_beacon_js = false;
        self.noscript_depth = 0;
        self.prepared = false;
    }
}

impl CriticalImagesBeaconFilter {
    /// Registers candidates and snapshots post-render identity
    /// (split for unit tests).
    fn finish_document(&mut self) {
        let server = Rc::clone(self.driver.server_context());
        let options = self.driver.options();
        let oh = server.hasher().hash(options.signature().as_bytes());
        let properties = self.driver.request_properties();
        let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
            properties.device_type(),
        );
        let page_url = self.driver.base_url().spec();
        let finder = BeaconFinder::new(server.metadata_cache());
        let mut evidence = finder.load(&page_url, &oh, suffix).into_record();
        // New candidates reset beaconing to high frequency; without
        // insertion the rebeacon timestamp clears to force sampling.
        if beacon_finder::update_candidates(
            &mut evidence,
            &self.image_url_hashes,
            !self.insert_beacon_js,
        )
        {
            finder.store(&page_url, &oh, suffix, &evidence);
        }
        self.driver
            .set_beacon_evidence_identity(beacon_finder::encode_evidence(&evidence));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_context::ServerContext;
    use ps_http::mock::MockUrlFetcher;
    use std::cell::RefCell;
    use std::sync::Arc;

    fn driver_for(user_agent: &str) -> Rc<ResourceDriver> {
        let http: Arc<dyn ps_cache::cache::Cache> =
            Arc::new(ps_cache::cache::MemoryCache::new());
        let metadata: Arc<dyn ps_cache::cache::Cache> =
            Arc::new(ps_cache::cache::MemoryCache::new());
        let server = Rc::new(ServerContext::new_with_metadata(
            http,
            metadata,
            Rc::new(MockUrlFetcher::new()),
            1_700_000_000_000,
        ));
        let options = Rc::new(RefCell::new(ps_config::options::RewriteOptions::new()));
        let driver = Rc::new(ResourceDriver::new(
            server,
            options,
            "http://beacon.test/page.html",
        ));
        driver.set_user_agent(user_agent);
        driver
    }

    const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

    #[test]
    fn prepare_inserts_on_first_visit() {
        let driver = driver_for(CHROME);
        let properties = driver.request_properties();
        assert!(properties.supports_critical_images_beacon());
        let mut filter = CriticalImagesBeaconFilter::new(driver);
        filter.prepare();
        assert!(filter.insert_beacon_js);
        assert!(filter.stamp_hashes);
        assert!(filter.nonce.is_some());
    }
}
