//! HTML-cache variant e2e: named-`Vary` entries key per request
//! variant, `Vary: *` still bypasses, unlearned beacon variants
//! publish and rekey on evidence, sampling-due requests bypass,
//! and detached (deadline-pending) rewrites never publish. All
//! assertions use counters and bodies, never latency.

#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_cache::purge::PurgeSet;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::html_cache::HtmlCacheSession;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::mock::MockUrlFetcher;
use ps_http::request::{Method, RequestHeaders};
use ps_http::response::{HttpOptions, ResponseHeaders};

const PAGE_URL: &str = "http://vary.test/page.html";
const START_MS: i64 = 1_700_000_000_000;

/// Configured driver harness over memory caches.
struct VaryHarness {
    options: Rc<RefCell<RewriteOptions>>,
    server: Rc<ServerContext>,
    driver: Rc<ResourceDriver>,
    purge: PurgeSet,
}

impl VaryHarness {
    fn new() -> Self {
        let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let http: Arc<dyn ps_cache::cache::Cache> = Arc::new(MemoryCache::new());
        let server = Rc::new(ServerContext::new_with_metadata(
            http,
            metadata,
            Arc::new(MockUrlFetcher::new()),
            START_MS,
        ));
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options
            .borrow_mut()
            .set_option("HtmlCache", "on", Scope::Server)
            .unwrap();
        options
            .borrow_mut()
            .set_option("HtmlCacheTtlSec", "60", Scope::Server)
            .unwrap();
        let driver = Rc::new(ResourceDriver::new(
            Rc::clone(&server),
            Rc::clone(&options),
            PAGE_URL,
        ));
        VaryHarness {
            options,
            server,
            driver,
            purge: PurgeSet::new(25_000),
        }
    }

    fn session(&self) -> HtmlCacheSession {
        HtmlCacheSession::new(Rc::clone(&self.driver), PAGE_URL)
    }

    fn counter(&self, name: &str) -> i64 {
        self.server.variable(name).get()
    }
}

fn get_request() -> RequestHeaders {
    let mut request = RequestHeaders::new();
    request.set_method(Method::Get);
    request.set_version(1, 1);
    request.headers_mut().add("Host", "vary.test");
    request
}

fn ok_origin() -> ResponseHeaders {
    let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
    origin.set_status_and_reason(200);
    origin.headers_mut().add("Content-Type", "text/html");
    origin
        .headers_mut()
        .add("Date", &ps_http::time::format_http_date(START_MS));
    origin
        .headers_mut()
        .add("Cache-Control", "public, max-age=300");
    origin.compute_caching();
    origin
}

/// Named `Vary` fields get per-variant entries: distinct request
/// headers miss, store, and hit independently with isolated bodies.
#[test]
fn named_vary_caches_per_variant_with_isolated_bodies() {
    let harness = VaryHarness::new();
    let mut origin = ok_origin();
    origin.headers_mut().add("Vary", "Accept-Encoding");
    origin.headers_mut().add("Vary", "User-Agent");
    origin.compute_caching();

    // Variant A (gzip desktop) misses and publishes.
    let mut request_a = get_request();
    request_a.headers_mut().add("Accept-Encoding", "gzip");
    request_a.headers_mut().add("User-Agent", "Desktop");
    let mut first = harness.session();
    assert!(first
        .lookup(&origin, &request_a, &harness.purge, None, START_MS)
        .is_none());
    assert_eq!(1, harness.counter("html_cache_misses"));
    first.write(b"<html>desktop</html>");
    let finish = first.finish(&request_a, START_MS + 1000);
    assert!(finish.detail_line.is_some());
    assert_eq!(1, harness.counter("html_cache_inserts"));

    // Same variant hits with its own body.
    let mut hit = harness.session();
    let entry = hit
        .lookup(&origin, &request_a, &harness.purge, None, START_MS + 2000)
        .expect("variant A hits");
    assert_eq!(b"<html>desktop</html>", entry.body.as_slice());
    assert_eq!(1, harness.counter("html_cache_hits"));

    // Variant B (br mobile) is a different entry, not a hit.
    let mut request_b = get_request();
    request_b.headers_mut().add("Accept-Encoding", "br");
    request_b.headers_mut().add("User-Agent", "Mobile");
    let mut second = harness.session();
    assert!(second
        .lookup(&origin, &request_b, &harness.purge, None, START_MS + 3000)
        .is_none());
    assert_eq!(2, harness.counter("html_cache_misses"));
    second.write(b"<html>mobile</html>");
    let finish_b = second.finish(&request_b, START_MS + 4000);
    assert!(finish_b.detail_line.is_some());

    // Each variant serves its own bytes (isolation both ways).
    let mut hit_a = harness.session();
    let entry_a = hit_a
        .lookup(&origin, &request_a, &harness.purge, None, START_MS + 5000)
        .expect("variant A still hits");
    assert_eq!(b"<html>desktop</html>", entry_a.body.as_slice());
    let mut hit_b = harness.session();
    let entry_b = hit_b
        .lookup(&origin, &request_b, &harness.purge, None, START_MS + 6000)
        .expect("variant B hits");
    assert_eq!(b"<html>mobile</html>", entry_b.body.as_slice());
    assert_eq!(3, harness.counter("html_cache_hits"));
    assert_eq!(0, harness.counter("html_cache_bypasses"));
}

/// Seeds one accepted beacon for the harness page, returning the
/// stored evidence with sampling settled in the future (mirrors the
/// real merge path: sample, report, accept).
fn seed_learned_beacon(harness: &VaryHarness) -> ps_html::beacon_finder::BeaconEvidence {
    use ps_html::beacon_finder as finder;
    let server = Rc::clone(&harness.server);
    let oh = server
        .hasher()
        .hash(harness.options.borrow().signature().as_bytes());
    let cohort = finder::BeaconFinder::new(server.metadata_cache());
    let mut evidence = cohort.load(PAGE_URL, &oh, "@Desktop").into_record();
    evidence.next_beacon_ms = 0;
    let nonce = finder::prepare_beacon(&mut evidence, START_MS, 5, 77).expect("due");
    let hash = finder::url_hash("http://vary.test/hero.png");
    finder::update_candidates(&mut evidence, &[hash.clone()].into_iter().collect(), false);
    let report = finder::BeaconReport {
        html_hashes: Some([hash].into_iter().collect()),
        lcp_kind: finder::LCP_KIND_IMG.to_string(),
        lcp_image_url: String::new(),
        nonce,
        font_families: Vec::new(),
    };
    assert_eq!(
        finder::accept_beacon(&mut evidence, &report, START_MS),
        finder::AcceptOutcome::Accepted
    );
    evidence.next_beacon_ms = START_MS + 1_000_000;
    cohort.store(PAGE_URL, &oh, "@Desktop", &evidence);
    evidence
}

/// Snapshots the current evidence identity into the driver,
/// mirroring the beacon filter's end-of-document write.
fn settle_driver_identity(harness: &VaryHarness) {
    use ps_html::beacon_finder as finder;
    let server = Rc::clone(&harness.server);
    let oh = server
        .hasher()
        .hash(harness.options.borrow().signature().as_bytes());
    let cohort = finder::BeaconFinder::new(server.metadata_cache());
    let evidence = cohort.load(PAGE_URL, &oh, "@Desktop").into_record();
    harness
        .driver
        .set_beacon_evidence_identity(finder::encode_evidence(&evidence));
}

fn enable_beaconing(harness: &VaryHarness) {
    harness
        .options
        .borrow_mut()
        .soft_enable_filter_for_testing(Filter::PrioritizeLcpImages);
    assert!(ps_html::beacon_finder::beaconing_active(
        &harness.driver.options()
    ));
}

/// Requests without beacon data publish and reuse entries: miss,
/// store under the unlearned marker, then hit with identical bytes.
#[test]
fn unlearned_beacon_requests_publish_and_hit() {
    use ps_html::beacon_finder as finder;
    let harness = VaryHarness::new();
    enable_beaconing(&harness);
    let origin = ok_origin();
    let request = get_request();
    let mut first = harness.session();
    assert!(first
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    assert_eq!(0, harness.counter("html_cache_bypasses"));
    assert_eq!(1, harness.counter("html_cache_misses"));
    assert_eq!(1, harness.counter(finder::NOT_FOUND_COUNT));
    first.write(b"<html>unlearned</html>");
    settle_driver_identity(&harness);
    let finish = first.finish(&request, START_MS + 1000);
    assert!(finish.detail_line.is_some());
    assert_eq!(1, harness.counter("html_cache_inserts"));
    let mut second = harness.session();
    let entry = second
        .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
        .expect("unlearned variant hits");
    assert_eq!(b"<html>unlearned</html>", entry.body.as_slice());
    assert_eq!(1, harness.counter("html_cache_hits"));
}

/// Accepted beacons rekey the variant: the pre-learning entry is
/// orphaned (never served again) and the new variant publishes.
#[test]
fn beacon_arrival_rekeys_variant() {
    let harness = VaryHarness::new();
    enable_beaconing(&harness);
    let origin = ok_origin();
    let request = get_request();
    // Publish the unlearned variant.
    let mut first = harness.session();
    assert!(first
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    first.write(b"<html>unlearned</html>");
    settle_driver_identity(&harness);
    assert!(first
        .finish(&request, START_MS + 1000)
        .detail_line
        .is_some());
    // A real beacon lands through the merge path.
    seed_learned_beacon(&harness);
    // Same request now addresses a fresh variant (miss), which
    // publishes separately; the old bytes are unreachable.
    let mut second = harness.session();
    assert!(second
        .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
        .is_none());
    assert_eq!(2, harness.counter("html_cache_misses"));
    second.write(b"<html>learned</html>");
    settle_driver_identity(&harness);
    assert!(second
        .finish(&request, START_MS + 3000)
        .detail_line
        .is_some());
    assert_eq!(2, harness.counter("html_cache_inserts"));
    let mut third = harness.session();
    let entry = third
        .lookup(&origin, &request, &harness.purge, None, START_MS + 4000)
        .expect("learned variant hits");
    assert_eq!(b"<html>learned</html>", entry.body.as_slice());
}

/// Sampling-due requests from beacon-capable clients bypass so
/// collection keeps running, even with learned evidence on record.
#[test]
fn sampling_due_bypasses_preserving_collection() {
    let harness = VaryHarness::new();
    enable_beaconing(&harness);
    harness.driver.set_user_agent(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
    );
    let mut evidence = seed_learned_beacon(&harness);
    // Force the sampling horizon into the past (due now).
    use ps_html::beacon_finder as finder;
    let server = Rc::clone(&harness.server);
    let oh = server
        .hasher()
        .hash(harness.options.borrow().signature().as_bytes());
    evidence.next_beacon_ms = START_MS - 1;
    finder::BeaconFinder::new(server.metadata_cache()).store(PAGE_URL, &oh, "@Desktop", &evidence);
    let origin = ok_origin();
    let request = get_request();
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    assert_eq!(1, harness.counter("html_cache_bypasses"));
    assert_eq!(0, harness.counter("html_cache_misses"));
    assert_eq!(0, harness.counter("html_cache_inserts"));
}

/// Clients that cannot beacon (curl, bots, latency probes) never
/// bypass for sampling: they could never settle a due record, so
/// due-ness does not apply and they cache normally.
#[test]
fn non_beacon_clients_ignore_sampling_due() {
    let harness = VaryHarness::new();
    enable_beaconing(&harness);
    // Default driver UA cannot beacon; force the record due anyway.
    let mut evidence = seed_learned_beacon(&harness);
    use ps_html::beacon_finder as finder;
    let server = Rc::clone(&harness.server);
    let oh = server
        .hasher()
        .hash(harness.options.borrow().signature().as_bytes());
    evidence.next_beacon_ms = START_MS - 1;
    finder::BeaconFinder::new(server.metadata_cache()).store(
        PAGE_URL,
        &oh,
        "@Desktop",
        &evidence,
    );
    let origin = ok_origin();
    let request = get_request();
    let mut session = harness.session();
    assert!(session
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    assert_eq!(0, harness.counter("html_cache_bypasses"));
    assert_eq!(1, harness.counter("html_cache_misses"));
    session.write(b"<html>probe</html>");
    settle_driver_identity(&harness);
    let finish = session.finish(&request, START_MS + 1000);
    assert!(finish.detail_line.is_some());
    assert_eq!(1, harness.counter("html_cache_inserts"));
}

/// Detached rewrites never publish: a render that served fallback
/// content buffers nothing permanent, while a clean retry stores.
#[test]
fn detached_rewrite_blocks_publication() {
    let harness = VaryHarness::new();
    let origin = ok_origin();
    let request = get_request();
    let mut first = harness.session();
    assert!(first
        .lookup(&origin, &request, &harness.purge, None, START_MS)
        .is_none());
    first.write(b"<html>fallback</html>");
    harness.driver.note_detached_rewrite();
    let finish = first.finish(&request, START_MS + 1000);
    assert!(finish.detail_line.is_none());
    assert_eq!(0, harness.counter("html_cache_inserts"));
    // A clean retry on a fresh driver publishes normally.
    let retry = Rc::new(ResourceDriver::new(
        Rc::clone(&harness.server),
        Rc::clone(&harness.options),
        PAGE_URL,
    ));
    let mut second = HtmlCacheSession::new(retry, PAGE_URL);
    assert!(second
        .lookup(&origin, &request, &harness.purge, None, START_MS + 2000)
        .is_none());
    second.write(b"<html>complete</html>");
    let finish = second.finish(&request, START_MS + 3000);
    assert!(finish.detail_line.is_some());
    assert_eq!(1, harness.counter("html_cache_inserts"));
}

/// `Vary: *` (alone or embedded) still bypasses: no dimension to
/// key, so nothing is stored or served.
#[test]
fn vary_star_still_bypasses() {
    for vary in ["*", "Accept-Encoding, *"] {
        let harness = VaryHarness::new();
        let mut origin = ok_origin();
        origin.headers_mut().add("Vary", vary);
        origin.compute_caching();
        let request = get_request();
        let mut session = harness.session();
        assert!(session
            .lookup(&origin, &request, &harness.purge, None, START_MS)
            .is_none());
        session.write(b"<html>unkeyable</html>");
        let finish = session.finish(&request, START_MS + 1000);
        assert!(finish.detail_line.is_none());
        assert_eq!(1, harness.counter("html_cache_bypasses"));
        assert_eq!(0, harness.counter("html_cache_inserts"));
        assert_eq!(0, harness.counter("html_cache_misses"));
    }
}

/// Slow origin fetcher (overt latency around the mock).
struct SlowFetcher {
    delay: std::time::Duration,
    inner: MockUrlFetcher,
}

impl ps_http::fetch::AsyncFetcher for SlowFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn ps_http::fetch::AsyncFetch) {
        std::thread::sleep(self.delay);
        self.inner.fetch(url, fetch);
    }
}

/// Full async + HTML-cache cycle through the real driver pipeline:
/// a deadline-missed rewrite serves fallback and publishes
/// nothing; once the background fetch lands, the rewrite completes
/// and publishes; later requests hit. All steps assert counters.
#[test]
fn slow_fetch_defers_publication_until_complete() {
    use ps_html::background::{BackgroundPool, PoolConfig};
    use ps_html::chain::ChainContext;
    use ps_html::driver::Driver;
    use std::time::{Duration, Instant};

    const CSS_URL: &str = "http://vary.test/a.css";
    const HTML: &[u8] =
        b"<head><link rel='stylesheet' href='http://vary.test/a.css'></head><body></body>";

    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options.borrow_mut().filters_mut().enable(Filter::InlineCss);
    options
        .borrow_mut()
        .set_option("HtmlCache", "on", Scope::Server)
        .unwrap();
    options
        .borrow_mut()
        .set_option("HtmlCacheTtlSec", "60", Scope::Server)
        .unwrap();
    options
        .borrow_mut()
        .set_option("RewriteDeadlinePerFlushMs", "30", Scope::Server)
        .unwrap();
    let fetcher = Arc::new(SlowFetcher {
        delay: Duration::from_millis(300),
        inner: MockUrlFetcher::new(),
    });
    let mut css_headers = ResponseHeaders::new(HttpOptions::default_for_tests());
    css_headers.set_first_line(1, 1, 200, "OK");
    css_headers
        .headers_mut()
        .add(ps_http::names::header::CONTENT_TYPE, "text/css");
    css_headers.set_date_and_caching(START_MS, 300_000, "");
    css_headers.compute_caching();
    fetcher
        .inner
        .set_response(CSS_URL, css_headers, b"div{display:block}");
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        fetcher as Arc<dyn ps_http::fetch::AsyncFetcher>,
        START_MS,
    ));
    let pool = BackgroundPool::new(
        PoolConfig {
            threads: 2,
            queue_bound: 16,
        },
        &server.statistics_shared(),
    );
    server.set_background_pool(Arc::clone(&pool));
    let purge = PurgeSet::new(25_000);
    let counter = |name: &str| server.variable(name).get();

    // Drive one HTML request through lookup → rewrite → store.
    let drive = |server: &Rc<ServerContext>, options: &Rc<RefCell<RewriteOptions>>, now_ms: i64| {
        let resource_driver = Rc::new(ResourceDriver::new(
            server.clone(),
            options.clone(),
            PAGE_URL,
        ));
        let mut session = HtmlCacheSession::new(resource_driver.clone(), PAGE_URL);
        let mut origin = ok_origin();
        // Same-page origin (cacheable, fresh).
        origin.compute_caching();
        let request = get_request();
        let hit = session
            .lookup(&origin, &request, &purge, None, now_ms)
            .map(|entry| entry.body);
        if let Some(body) = hit {
            return (true, body);
        }
        let sink = harness::SharedSink::default();
        let ctx = ChainContext::new(true, true);
        let mut driver = Driver::new(
            &options.borrow(),
            &ctx,
            sink.clone(),
            Some(resource_driver.clone()),
        );
        driver.start_parse(b"http://vary.test/page.html", false);
        driver.write(HTML);
        driver.finish();
        let output = sink.bytes();
        session.write(&output);
        session.finish(&request, now_ms + 1000);
        (false, output)
    };

    // First request: the CSS fetch misses its deadline, the page
    // serves the original link, and nothing publishes.
    let (hit, first) = drive(&server, &options, START_MS);
    assert!(!hit);
    assert!(first.windows(b"a.css".len()).any(|w| w == b"a.css"));
    assert_eq!(0, counter("html_cache_inserts"));
    assert_eq!(1, counter("html_cache_misses"));

    // Background completion warms the caches; the rewrite now
    // completes and publishes.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if counter("background_jobs_completed") >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "background fetch never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (hit, second) = drive(&server, &options, START_MS + 10_000);
    assert!(!hit);
    assert!(
        second.windows(b"<style>".len()).any(|w| w == b"<style>"),
        "inlined after warmup"
    );
    assert_eq!(1, counter("html_cache_inserts"));

    // Later requests hit the published entry with identical bytes.
    let (hit, third) = drive(&server, &options, START_MS + 20_000);
    assert!(hit);
    assert_eq!(second, third);
    assert_eq!(1, counter("html_cache_hits"));
    pool.shutdown();
}
