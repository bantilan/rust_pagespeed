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
            font_families: Vec::new(),
            css_selectors: None,
        };
        assert_eq!(
            finder::accept_beacon(&mut evidence, &report, 1000),
            finder::AcceptOutcome::Accepted
        );
        backend.store(PAGE_URL, &oh, ps_html::beacon_finder::evidence_suffix(&self.driver()), &evidence);
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
            font_families: Vec::new(),
            css_selectors: None,
        };
        assert_eq!(
            finder::accept_beacon(&mut evidence, &report, 1000),
            finder::AcceptOutcome::Accepted
        );
        backend.store(PAGE_URL, &oh, ps_html::beacon_finder::evidence_suffix(&self.driver()), &evidence);
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
    // Exercise the legacy shared-device evidence format guard explicitly.
    harness.options.borrow_mut().set_option("HtmlCacheSimpleVariants", "off", ps_config::scope::Scope::Server).unwrap();
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
    jpg.options.borrow_mut().set_option("HtmlCacheSimpleVariants", "off", ps_config::scope::Scope::Server).unwrap();
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

#[test]
fn wordpress_custom_logo_skipped_hero_passes_to_content() {
    // Classic WP markup: cropped filename carries no "logo" word, so
    // class + branding-ancestor + home-link signals must carry it.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><div class=\"site-branding\"><a class=\"custom-logo-link\" rel=\"home\" href=\"/\"><img class=\"custom-logo\" src=\"cropped-x-270x270.png\" width=\"270\" height=\"70\" alt=\"Acme Corp\"></a></div></header><main><img src=\"content.png\"></main></body></html>",
    );
    // The content image (not the first-element logo) is the hero.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"content.png\"") >= 1);
    assert_eq!(0, count(&out, b"href=\"cropped-x-270x270.png\""));
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
    // The skipped logo is left alone: no lazy hint either.
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn wordpress_block_logo_skipped() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><figure class=\"wp-block-site-logo\"><a href=\"/\"><img src=\"logo-300x100.png\"></a></figure></header><main><img src=\"content.png\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"content.png\"") >= 1);
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn custom_header_svg_logo_skipped() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><nav><img id=\"site-logo\" src=\"/assets/logo.svg\" alt=\"Acme logo\"></nav></header><main><img src=\"content.png\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"content.png\"") >= 1);
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn logo_inside_picture_skipped() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><picture><source srcset=\"logo.webp\" type=\"image/webp\"><img src=\"logo.png\" class=\"logo\"></picture></header><main><img src=\"content.png\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"content.png\"") >= 1);
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn schema_org_itemprop_logo_skipped() {
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><div itemscope itemtype=\"https://schema.org/Organization\"><img itemprop=\"logo\" src=\"mark.png\"></div><main><img src=\"content.png\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"content.png\"") >= 1);
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn bare_itemprop_without_context_stays_candidate() {
    // A single attribute never decides alone: itemprop with zero
    // corroboration keeps the image eligible.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img itemprop=\"logo\" src=\"mark.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"mark.png\"") >= 1);
}

#[test]
fn filename_only_logo_stays_candidate() {
    // A bare filename mention is not enough to suppress.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"logo.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"logo.png\"") >= 1);
}

#[test]
fn hero_mentioning_logo_still_prioritized() {
    // Hero counter-signals veto the URL mention.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><main><img class=\"hero-banner\" src=\"hero-logo-reveal.jpg\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"hero-logo-reveal.jpg\"") >= 1);
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
}

#[test]
fn article_logo_image_not_confused() {
    // A logo-named file in article content keeps candidacy (weak
    // single signal, content discount): it still becomes the hero.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><article><img src=\"company-logo.png\"><img src=\"photo.jpg\"></article></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"company-logo.png\"") >= 1);
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}

#[test]
fn logo_with_author_priority_left_byte_identical() {
    // Explicit developer priorities survive: the author's preload
    // stands alone and the author's fetchpriority is untouched.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><link rel=\"preload\" as=\"image\" href=\"logo.png\"></head><body><header><img class=\"logo\" src=\"logo.png\" fetchpriority=\"high\"></header></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert_eq!(1, count(&out, b"fetchpriority=\"high\""));
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
    assert_eq!(0, count(&out, b"data-pagespeed-no-defer"));
}

#[test]
fn measured_lcp_logo_still_prioritized() {
    // Reliable LCP measurements override logo exclusion.
    let harness = Harness::new();
    harness.seed_lcp_report(
        &[],
        ps_html::beacon_finder::LCP_KIND_IMG,
        "http://lcp.test/logo.png",
    );
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><img class=\"logo\" src=\"logo.png\"></header><main><img src=\"content.png\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"logo.png\"") >= 1);
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
    // The non-critical content image still lazy-loads.
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}

#[test]
fn text_lcp_evidence_falls_back_to_visual_hero() {
    // Measurements naming no image (text LCP, empty critical set)
    // must not veto the visual hero: the first eligible image is
    // promoted at end of document instead of leaving the budget
    // unspent.
    let harness = Harness::new();
    harness.seed_lcp_report(&[], ps_html::beacon_finder::LCP_KIND_TEXT, "");
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"a.png\"") >= 1);
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}

#[test]
fn stale_evidence_naming_absent_image_falls_back() {
    // Evidence for an image that is not on this page (stale variant,
    // renamed asset) likewise falls back to the visual hero.
    let harness = Harness::new();
    harness.seed_beacon("http://lcp.test/gone.png");
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"a.png\"") >= 1);
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
}

#[test]
fn fallback_hero_skips_logo_candidate() {
    // The fallback promotes the first *non-logo* eligible image.
    let harness = Harness::new();
    harness.seed_lcp_report(&[], ps_html::beacon_finder::LCP_KIND_TEXT, "");
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><img class=\"logo\" src=\"logo.png\" alt=\"Acme logo\"></header><main><img src=\"content.png\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"content.png\"") >= 1);
    assert_eq!(0, count(&out, b"href=\"logo.png\""));
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn fallback_does_not_double_spend_learned_budget() {
    // A learned hero still spends the only budget; the fallback
    // stays quiet.
    let harness = Harness::new();
    harness.seed_beacon("http://lcp.test/b.png");
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><img src=\"a.png\"><img src=\"b.png\"><img src=\"c.png\"></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"b.png\"") >= 1);
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
    // Contradicted first image bare, post-hero image lazy.
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}

#[test]
fn logo_inside_banner_region_still_skipped() {
    // Ancestor region classes (`mainBanner`) wrap the whole header
    // and must not veto the logo: only the element's own class/id
    // discriminates. Shape mirrors the mobile-app landing template.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header class=\"mainBanner\"><div class=\"container\"><figure><a href=\"./index.html\"><img class=\"img-logo\" src=\"images/logo.png\" alt=\"\"></a></figure></div></header><main><img class=\"img-iPhone\" src=\"images/iphone/2.png\" alt=\"\"></main></body></html>",
    );
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"images/iphone/2.png\"") >= 1);
    assert_eq!(0, count(&out, b"href=\"images/logo.png\""));
    assert_eq!(2, count(&out, b"fetchpriority=\"high\""));
    assert_eq!(0, count(&out, b"loading=\"lazy\""));
}

#[test]
fn element_hero_class_still_vetoes_despite_header() {
    // The veto survives where it belongs: on the element itself,
    // even inside a header with a logo-named file.
    let harness = Harness::new();
    let driver = harness.driver();
    let out = harness.render(
        &driver,
        b"<html><head><title>t</title></head><body><header><img class=\"hero-banner\" src=\"logo-reveal.jpg\"></header><main><img src=\"content.png\"></main></body></html>",
    );
    // Hero-banner image keeps candidacy (vetoed as logo) and takes
    // the inline hero slot as first eligible.
    assert_eq!(1, count(&out, b"rel=\"preload\""));
    assert!(count(&out, b"href=\"logo-reveal.jpg\"") >= 1);
    assert_eq!(1, count(&out, b"loading=\"lazy\""));
}
