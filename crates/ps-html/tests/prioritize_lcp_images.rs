//! LCP prioritization tests: heuristic hero, learned evidence,
//! author preloads, and skips.
mod harness;

use harness::run_with_filters;
use ps_html::filters::prioritize_lcp_images::PrioritizeLcpImagesFilter;
use ps_html::server_context::{ResourceDriver, ServerContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const PAGE_URL: &str = "http://lcp.test/page.html";
const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

struct Harness {
    server: Rc<ServerContext>,
    options: Rc<RefCell<ps_config::options::RewriteOptions>>,
}

impl Harness {
    fn new() -> Self {
        let http: Arc<dyn ps_cache::cache::Cache> =
            Arc::new(ps_cache::cache::MemoryCache::new());
        let metadata: Arc<dyn ps_cache::cache::Cache> =
            Arc::new(ps_cache::cache::MemoryCache::new());
        let server = Rc::new(ServerContext::new_with_metadata(
            http,
            metadata,
            Arc::new(ps_http::mock::MockUrlFetcher::new()),
            1_700_000_000_000,
        ));
        Harness {
            server,
            options: Rc::new(RefCell::new(ps_config::options::RewriteOptions::new())),
        }
    }

    fn driver(&self) -> Rc<ResourceDriver> {
        self.driver_for(CHROME_UA)
    }

    fn driver_for(&self, user_agent: &str) -> Rc<ResourceDriver> {
        let driver = Rc::new(ResourceDriver::new(
            Rc::clone(&self.server),
            Rc::clone(&self.options),
            PAGE_URL,
        ));
        driver.set_user_agent(user_agent);
        driver
    }

    fn render(&self, driver: &Rc<ResourceDriver>, html: &[u8]) -> Vec<u8> {
        let filter = PrioritizeLcpImagesFilter::new(Rc::clone(driver));
        run_with_filters(html, |parser| {
            parser.add_filter(Box::new(filter));
        })
    }

    /// Seeds one accepted stamped beacon for an image URL.
    fn seed_beacon(&self, image_url: &str) {
        use ps_html::beacon_finder as finder;
        let backend = finder::BeaconFinder::new(self.server.metadata_cache());
        let oh = self
            .server
            .hasher()
            .hash(self.options.borrow().signature().as_bytes());
        let mut evidence = finder::BeaconEvidence::default();
        let nonce = finder::prepare_beacon(&mut evidence, 1000, 5, 11).expect("due");
        let hash = finder::url_hash(image_url);
        finder::update_candidates(
            &mut evidence,
            &[hash.clone()].into_iter().collect(),
            false,
        );
        let report = finder::BeaconReport {
            html_hashes: Some([hash].into_iter().collect()),
            lcp_kind: finder::LCP_KIND_IMG.to_string(),
            lcp_image_url: String::new(),
            nonce,
        };
        assert_eq!(
            finder::accept_beacon(&mut evidence, &report, 1000),
            finder::AcceptOutcome::Accepted
        );
        backend.store(PAGE_URL, &oh, "@Desktop", &evidence);
    }

    /// Seeds one accepted beacon with explicit hashes, LCP kind,
    /// and LCP URL (background LCP reports an empty set).
    fn seed_lcp_report(&self, hashes: &[String], kind: &str, url: &str) {
        use ps_html::beacon_finder as finder;
        let backend = finder::BeaconFinder::new(self.server.metadata_cache());
        let oh = self
            .server
            .hasher()
            .hash(self.options.borrow().signature().as_bytes());
        let mut evidence = finder::BeaconEvidence::default();
        let nonce = finder::prepare_beacon(&mut evidence, 1000, 5, 11).expect("due");
        finder::update_candidates(
            &mut evidence,
            &hashes.iter().cloned().collect(),
            false,
        );
        let report = finder::BeaconReport {
            html_hashes: Some(hashes.iter().cloned().collect()),
            lcp_kind: kind.to_string(),
            lcp_image_url: url.to_string(),
            nonce,
        };
        assert_eq!(
            finder::accept_beacon(&mut evidence, &report, 1000),
            finder::AcceptOutcome::Accepted
        );
        backend.store(PAGE_URL, &oh, "@Desktop", &evidence);
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn heuristic_hero_preloads_and_rest_lazy() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>",
    );
    // Exactly one preload, pointing at the first image.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"a.png\"") >= 1);
    // High priority lands on both the hero and its preload.
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
    // The hero is marked no-defer; the second image lazy-loads.
    assert_eq!(1, count(&out, b"data-pagespeed-no-defer=\"1\""));
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}

#[test]
fn learned_evidence_overrides_position() {
    let harness = Harness::new();
    harness.seed_beacon("http://lcp.test/b.png");
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>",
    );
    // The learned hero preloads even though it is second.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"b.png\"") >= 1);
    // The contradicted first image is left alone: no lazy hint.
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn author_preload_suppresses_duplicate_hint() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><link rel=\"preload\" as=\"image\" href=\"a.png\"></head><body><img src=\"a.png\"></body></html>",
    );
    // The author's hint stands; no duplicate is inserted.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    // Element priority still applies to the hero.
    assert_eq!(1, count(&out, b"fetchpriority=\"high\""));
}

#[test]
fn hidden_trackers_and_data_urls_skip() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"hide.png\" style=\"display:none\"><img src=\"t.png\" width=\"1\" height=\"1\"><img src=\"data:image/png;base64,AA==\"><img src=\"real.png\"></body></html>",
    );
    // Only the visible, non-tracker, non-data image is the hero.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"real.png\"") >= 1);
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn explicit_high_priority_is_never_lazy() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\" fetchpriority=\"high\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

const BG_URL: &str = "http://lcp.test/bg.avif";

#[test]
fn background_lcp_preloads_head_hint() {
    let harness = Harness::new();
    harness.seed_lcp_report(&[], ps_html::beacon_finder::LCP_KIND_BACKGROUND, BG_URL);
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>",
    );
    // Exactly one preload, pointing at the background URL with
    // high fetch priority; no image element is touched for
    // priority (the contradicted first image stays plain).
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, BG_URL.as_bytes()) >= 1);
    assert_eq!(1, count(&out, b"fetchpriority=\"high\""));
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}

#[test]
fn background_lcp_yields_to_image_hero() {
    // A learned-critical image spends the single-preload budget,
    // so the background URL gets no second hint.
    let harness = Harness::new();
    let hash = ps_html::beacon_finder::url_hash("http://lcp.test/b.png");
    harness.seed_lcp_report(
        &[hash],
        ps_html::beacon_finder::LCP_KIND_BACKGROUND,
        BG_URL,
    );
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"b.png\"") >= 1);
    assert_eq!(0, count(&out, BG_URL.as_bytes()));
}

const LEGACY_UA: &str = "Mozilla/5.0 (compatible; MSIE 9.0; Windows NT 6.1; Trident/5.0)";

#[test]
fn background_lcp_format_mismatch_skips() {
    let harness = Harness::new();
    harness.seed_lcp_report(&[], ps_html::beacon_finder::LCP_KIND_BACKGROUND, BG_URL);
    let driver = harness.driver_for(LEGACY_UA);
    assert!(
        !driver
            .request_properties()
            .supports_avif_rewritten_urls(),
        "legacy UA must lack AVIF support"
    );
    // Learned AVIF bytes are not hinted to a non-AVIF browser.
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"></body></html>",
    );
    assert_eq!(0, count(&out, b"rel=\"preload\""));
    // Universal formats still preload for the same client.
    let jpg = Harness::new();
    jpg.seed_lcp_report(
        &[],
        ps_html::beacon_finder::LCP_KIND_BACKGROUND,
        "http://lcp.test/bg.jpg",
    );
    let legacy = jpg.driver_for(LEGACY_UA);
    let out = jpg.render(
        &legacy,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
}

#[test]
fn background_lcp_author_preload_wins() {
    let harness = Harness::new();
    harness.seed_lcp_report(&[], ps_html::beacon_finder::LCP_KIND_BACKGROUND, BG_URL);
    let driver = harness.driver();
    let html = format!(
        "<html><head><link rel=\"preload\" as=\"image\" href=\"{BG_URL}\"></head><body><img src=\"a.png\"></body></html>"
    );
    let out = harness.render(&driver, html.as_bytes());
    // The author's hint stands; no duplicate is inserted.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
}
