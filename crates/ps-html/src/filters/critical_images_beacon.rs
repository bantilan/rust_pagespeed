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
use crate::filter::HtmlFilter;
use crate::js_escape::escape_to_js_string_literal;
use crate::lexer::Lexer;
use crate::name::Keyword;
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
    body_end: crate::filters::common::BodyEndTracker,
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
            body_end: Default::default(),
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
        let oh = server
            .hasher()
            .hash(self.driver.options_signature().as_bytes());
        self.options_hash = oh.clone();
        let suffix = beacon_finder::evidence_suffix(&self.driver);
        let page_url = self.driver.base_url().spec().to_string();
        let finder = BeaconFinder::new(server.metadata_cache());
        let downstream = self.downstream_gate();
        let now_ms = Self::now_ms();
        let nonce_value =
            ((server.simple_random_next() as u64) << 32) | server.simple_random_next() as u64;
        let reinstrument = options.beacon_reinstrument_time_sec();
        let prepared = finder.update(&page_url, &oh, suffix, |evidence| {
            evidence.single_decision = options.beacon_single_decision();
            let nonce = if beacon_finder::should_beacon(now_ms, Some(evidence), downstream) {
                beacon_finder::prepare_beacon(evidence, now_ms, reinstrument, nonce_value)
            } else {
                None
            };
            (nonce, evidence.clone())
        });
        let (nonce, evidence) =
            prepared.unwrap_or_else(|| (None, finder.load(&page_url, &oh, suffix).into_record()));
        self.nonce = nonce;
        self.insert_beacon_js = self.nonce.is_some();
        self.stamp_hashes = self.insert_beacon_js
            || beacon_finder::availability(true, Some(&evidence)) == Availability::Available;
        // Snapshot the pre-render identity; end of document
        // refreshes it after candidate registration.
        self.driver
            .set_beacon_evidence_identity(beacon_finder::html_evidence_identity(&evidence));
    }

    /// Builds the beacon init call (`Run(...)`) appended to the
    /// static beacon JS.
    fn init_call(&self) -> Vec<u8> {
        let options = self.driver.options();
        let is_https = self.driver.base_url().scheme() == "https";
        let mut beacon_url = options
            .beacon_url()
            .map(|urls| if is_https { urls.https } else { urls.http })
            .unwrap_or_else(|| DEFAULT_BEACON_URL.to_string());
        if options.html_cache_simple_variants() {
            beacon_url.push(if beacon_url.contains('?') { '&' } else { '?' });
            beacon_url.push_str("bvg=");
            beacon_url.push_str(beacon_finder::evidence_suffix(&self.driver));
        }
        let nonce = self.nonce.clone().unwrap_or_default();
        let send_at_onload = !options.filters().enabled(Filter::LazyloadImages);
        let resize = options
            .filters()
            .enabled(Filter::ResizeToRenderedImageDimensions);
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
    fn maybe_add_beacon_javascript(&mut self, dom: &mut Dom, lexer: &Lexer, at_end: bool) {
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
        if at_end {
            self.body_end.insert(dom, script);
        } else {
            dom.insert_node_before_current(script);
        }
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
                self.maybe_add_beacon_javascript(dom, lexer, false);
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
        self.body_end.clear();
        self.added_beacon_js = false;
        self.noscript_depth = 0;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.clear();
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
    }

    fn characters(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        self.body_end.observe_characters(dom, id);
    }

    fn end_element(&mut self, dom: &mut Dom, lexer: &Lexer, id: NodeId) {
        self.body_end.observe_end(dom, id);
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

    fn end_document(&mut self, dom: &mut Dom, lexer: &Lexer) {
        // Image-free pages still need to report text/background LCP and
        // consume the nonce we issued; otherwise sampling never settles.
        self.maybe_add_beacon_javascript(dom, lexer, true);
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
        let oh = server
            .hasher()
            .hash(self.driver.options_signature().as_bytes());
        let suffix = beacon_finder::evidence_suffix(&self.driver);
        let page_url = self.driver.base_url().spec();
        let finder = BeaconFinder::new(server.metadata_cache());
        let evidence = finder
            .update(&page_url, &oh, suffix, |evidence| {
                beacon_finder::update_candidates(
                    evidence,
                    &self.image_url_hashes,
                    !self.insert_beacon_js,
                );
                evidence.clone()
            })
            .unwrap_or_else(|| finder.load(&page_url, &oh, suffix).into_record());
        self.driver
            .set_beacon_evidence_identity(beacon_finder::html_evidence_identity(&evidence));
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
        let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(ps_cache::cache::MemoryCache::new());
        let metadata: Arc<dyn ps_cache::cache::Cache> =
            Arc::new(ps_cache::cache::MemoryCache::new());
        let server = Rc::new(ServerContext::new_with_metadata(
            http,
            metadata,
            Arc::new(MockUrlFetcher::new()),
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
    fn public_evidence_has_exactly_six_format_device_groups() {
        let mobile = "Mozilla/5.0 (Linux; Android 13; Mobile) AppleWebKit/537.36 Chrome/120.0 Mobile Safari/537.36";
        let firefox = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:147.0) Gecko/20100101 Firefox/147.0";
        let mut groups = HashSet::new();
        for (ua, device) in [(CHROME, "desktop"), (mobile, "mobile")] {
            for (accept, format) in [
                ("image/avif,image/webp", "avif"),
                ("image/avif;q=0,image/webp", "webp"),
                ("image/avif;q=0,image/webp;q=0", "fallback"),
            ] {
                let driver = driver_for(ua);
                let mut headers = ps_http::request::RequestHeaders::new();
                headers.headers_mut().add("User-Agent", ua);
                headers.headers_mut().add("Accept", accept);
                driver.set_request_headers(headers);
                let suffix = beacon_finder::evidence_suffix(&driver);
                assert_eq!(suffix, format!("{format}-{device}"));
                assert!(beacon_finder::public_evidence_suffix(suffix).is_some());
                groups.insert(suffix);
                let filter = CriticalImagesBeaconFilter::new(Rc::clone(&driver));
                assert!(String::from_utf8(filter.init_call()).unwrap().contains(&format!("bvg={suffix}")));
                if device == "desktop" {
                    driver.set_user_agent(firefox);
                    assert_eq!(beacon_finder::evidence_suffix(&driver), suffix);
                }
            }
        }
        assert_eq!(groups.len(), 6);
        let tablet = driver_for("Mozilla/5.0 (iPad; CPU OS 16_0 like Mac OS X) AppleWebKit/605.1.15 Version/16.0 Mobile/15E148 Safari/604.1");
        let mut headers = tablet.request_headers().clone();
        headers.headers_mut().add("Accept", "image/avif,image/webp");
        tablet.set_request_headers(headers);
        assert_eq!(beacon_finder::evidence_suffix(&tablet), "avif-mobile");

        assert!(beacon_finder::public_evidence_suffix("chrome-avif-desktop").is_none());
    }

    #[test]
    fn accepted_decision_does_not_insert_another_collector() {
        let driver = driver_for(CHROME);
        let mut first = CriticalImagesBeaconFilter::new(Rc::clone(&driver));
        first.prepare();
        let nonce = first.nonce.clone().unwrap();
        let server = driver.server_context();
        let oh = server.hasher().hash(driver.options_signature().as_bytes());
        let finder = BeaconFinder::new(server.metadata_cache());
        let report = beacon_finder::BeaconReport {
            nonce,
            html_hashes: Some(HashSet::new()),
            lcp_kind: beacon_finder::LCP_KIND_TEXT.into(),
            ..Default::default()
        };
        assert_eq!(finder.update(driver.base_url().spec(), &oh, beacon_finder::evidence_suffix(&driver), |evidence| {
            assert!(evidence.single_decision);
            let outcome = beacon_finder::accept_beacon(evidence, &report, CriticalImagesBeaconFilter::now_ms());
            evidence.next_beacon_ms = 0;
            outcome
        }), Some(beacon_finder::AcceptOutcome::Accepted));
        let mut later = CriticalImagesBeaconFilter::new(driver);
        later.prepare();
        assert!(!later.insert_beacon_js);
        assert!(later.nonce.is_none());
        assert!(later.stamp_hashes);
    }

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
