//! Critical-images beacon insertion tests: sampling, stamping,
//! and candidate registration across visits.
mod harness;

use harness::run_with_filters;
use ps_html::filter::HtmlFilter;
use ps_html::filters::critical_images_beacon::CriticalImagesBeaconFilter;
use ps_html::server_context::{ResourceDriver, ServerContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const PAGE_URL: &str = "http://beacon.test/page.html";
const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";
const BOT_UA: &str = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

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
            Rc::new(ps_http::mock::MockUrlFetcher::new()),
            1_700_000_000_000,
        ));
        Harness {
            server,
            options: Rc::new(RefCell::new(ps_config::options::RewriteOptions::new())),
        }
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
        let filter = CriticalImagesBeaconFilter::new(Rc::clone(driver));
        run_with_filters(html, |parser| {
            parser.add_filter(Box::new(filter));
        })
    }
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[test]
fn first_visit_instruments_stamps_and_registers() {
    let harness = Harness::new();
    let driver = harness.driver_for(CHROME_UA);
    let out = harness.render(
        &driver,
        b"<html><body><img src=\"hero.png\"></body></html>",
    );
    // Beacon JS inserted once before the first image.
    assert_eq!(1, count(&out, b"pagespeed.CriticalImages.Run('"));
    // The image carries its stamped original-URL hash.
    let hash = ps_html::beacon_finder::url_hash("http://beacon.test/hero.png");
    assert!(
        out.windows(hash.len() + 30).any(|window| {
            window.starts_with(b"data-pagespeed-url-hash=\"")
                && &window[25..25 + hash.len()] == hash.as_bytes()
        }),
        "stamped hash for hero.png"
    );
    // End of document registered the candidate and snapshotted
    // the post-render identity for the HTML cache.
    assert!(driver.beacon_evidence_identity().is_some());
    assert_eq!(
        1,
        harness
            .server
            .variable(ps_html::beacon_finder::BEACON_ADDED_COUNT)
            .get()
    );
}

#[test]
fn immediate_revisit_stands_down_without_evidence() {
    let harness = Harness::new();
    let first = harness.driver_for(CHROME_UA);
    harness.render(&first, b"<html><body><img src=\"hero.png\"></body></html>");
    // Sampling just happened: the revisit inserts nothing and
    // stamps nothing (no evidence yet).
    let second = harness.driver_for(CHROME_UA);
    let out = harness.render(
        &second,
        b"<html><body><img src=\"hero.png\"></body></html>",
    );
    assert_eq!(0, count(&out, b"pagespeed.CriticalImages.Run('"));
    assert_eq!(0, count(&out, b"data-pagespeed-url-hash="));
}

#[test]
fn learned_evidence_stamps_without_reinstrumenting() {
    let harness = Harness::new();
    // Seed one accepted beacon out of band.
    let finder = ps_html::beacon_finder::BeaconFinder::new(harness.server.metadata_cache());
    let oh = harness
        .server
        .hasher()
        .hash(harness.options.borrow().signature().as_bytes());
    let mut evidence = ps_html::beacon_finder::BeaconEvidence::default();
    let nonce = ps_html::beacon_finder::prepare_beacon(&mut evidence, 1000, 5, 9)
        .expect("due");
    let hash = ps_html::beacon_finder::url_hash("http://beacon.test/hero.png");
    ps_html::beacon_finder::update_candidates(
        &mut evidence,
        &[hash.clone()].into_iter().collect(),
        false,
    );
    let report = ps_html::beacon_finder::BeaconReport {
        html_hashes: Some([hash].into_iter().collect()),
        lcp_kind: ps_html::beacon_finder::LCP_KIND_IMG.to_string(),
        lcp_image_url: String::new(),
        nonce,
    };
    assert_eq!(
        ps_html::beacon_finder::accept_beacon(&mut evidence, &report, 1000),
        ps_html::beacon_finder::AcceptOutcome::Accepted
    );
    // Sampling is not due for a long while now.
    evidence.next_beacon_ms = i64::MAX / 2;
    finder.store(PAGE_URL, &oh, "@Desktop", &evidence);
    let driver = harness.driver_for(CHROME_UA);
    let out = harness.render(
        &driver,
        b"<html><body><img src=\"hero.png\"></body></html>",
    );
    assert_eq!(0, count(&out, b"pagespeed.CriticalImages.Run('"));
    assert_eq!(1, count(&out, b"data-pagespeed-url-hash="));
}

#[test]
fn bots_never_instrument() {
    let harness = Harness::new();
    let driver = harness.driver_for(BOT_UA);
    let out = harness.render(
        &driver,
        b"<html><body><img src=\"hero.png\"></body></html>",
    );
    assert_eq!(0, count(&out, b"pagespeed.CriticalImages.Run('"));
    assert_eq!(0, count(&out, b"data-pagespeed-url-hash="));
}