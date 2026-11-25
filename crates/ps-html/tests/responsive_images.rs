//! Fixed-width responsive srcsets (`responsive_images` + `resize_images`).
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_cache::hash::MockHasher;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::chain::{build_chain, ChainContext, ChainFilter};
use ps_html::driver::Driver;
use ps_html::filters::image_rewrite::ImageRewriteFilter;
use ps_html::filters::responsive_images::{
    ResponsiveImageFirstFilter, EAGER_SIZES, LAZY_SIZES, RESPONSIVE_SRCSET_WIDTHS,
};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::fetch::BufferingFetch;
use ps_http::google_url::GoogleUrl;
use ps_http::mock::MockUrlFetcher;

const PAGE: &str = "https://test.com/page";
const START_MS: i64 = 1_700_000_000_000;

fn fixture_with_fetcher() -> (
    Rc<ServerContext>,
    Rc<RefCell<RewriteOptions>>,
    Arc<MockUrlFetcher>,
) {
    let fetcher = Arc::new(MockUrlFetcher::new());
    let mut server = ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        fetcher.clone(),
        START_MS,
    );
    server.set_hasher(Arc::new(MockHasher::new()));
    let server = Rc::new(server);
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options.borrow_mut().add_domain("http://test.com/");
    options.borrow_mut().add_domain("https://test.com/");
    (server, options, fetcher)
}

fn fixture() -> (Rc<ServerContext>, Rc<RefCell<RewriteOptions>>) {
    let (server, options, _) = fixture_with_fetcher();
    (server, options)
}

fn driver_for(
    server: &Rc<ServerContext>,
    options: &Rc<RefCell<RewriteOptions>>,
) -> Rc<ResourceDriver> {
    Rc::new(ResourceDriver::new(server.clone(), options.clone(), PAGE))
}

fn render_filter(driver: &Rc<ResourceDriver>, html: &str) -> String {
    let out = harness::run_with_filters(html.as_bytes(), |parser: &mut Parser| {
        parser
            .add_early_pre_render_filter(Box::new(ResponsiveImageFirstFilter::new(driver.clone())));
    });
    String::from_utf8(out).unwrap()
}

fn render_driver(options: &RewriteOptions, driver: Rc<ResourceDriver>, html: &str) -> String {
    let sink = harness::SharedSink::default();
    let mut driver = Driver::new(
        options,
        &ChainContext::new(true, true),
        sink.clone(),
        Some(driver),
    );
    driver.start_parse(PAGE.as_bytes(), false);
    driver.write(html.as_bytes());
    driver.finish();
    String::from_utf8(sink.bytes()).unwrap()
}

fn enable_responsive(options: &mut RewriteOptions) {
    options.filters_mut().enable(Filter::ResponsiveImages);
    options.filters_mut().enable(Filter::ResizeImages);
}

#[test]
fn widths_and_sizes_strings() {
    assert_eq!(RESPONSIVE_SRCSET_WIDTHS, [480, 640]);
    assert_eq!(LAZY_SIZES, "auto, 100vw");
    assert_eq!(EAGER_SIZES, "(max-width: 640px) 480px, 640px");
}

#[test]
fn eager_img_gets_srcset_and_static_sizes() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let out = render_filter(&driver, "<img src='photo.jpg'>");
    assert!(out.contains("srcset="), "{out}");
    assert!(out.contains("480w"), "{out}");
    assert!(out.contains("640w"), "{out}");
    assert!(out.contains(".pagespeed.ic.0.jpg"), "{out}");
    assert!(out.contains(&format!("sizes=\"{EAGER_SIZES}\"")), "{out}");
    // Original src stays as the fallback.
    assert!(out.contains("src='photo.jpg'"), "{out}");
}

#[test]
fn lazy_img_gets_auto_sizes() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let out = render_filter(
        &driver,
        "<img src='photo.jpg' loading='lazy' width='600' height='400'>",
    );
    assert!(out.contains(&format!("sizes=\"{LAZY_SIZES}\"")), "{out}");
    assert!(out.contains("480w") && out.contains("640w"), "{out}");
}

#[test]
fn existing_srcset_and_sizes_are_preserved() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    // Author srcset: untouched (owned by the sizes machinery).
    let input = "<img src='a.jpg' srcset='a.jpg 300w, b.jpg 800w'>";
    assert_eq!(render_filter(&driver, input), input);
    // Author sizes without srcset: kept, srcset still generated.
    let out = render_filter(&driver, "<img src='a.jpg' sizes='50vw'>");
    assert!(out.contains("sizes='50vw'"), "{out}");
    assert!(out.contains("480w"), "{out}");
}

#[test]
fn guards_skip_unsafe_markup() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    for input in [
        // No src / empty src.
        "<img>",
        "<img src=''>",
        // Duplicate src is ambiguous.
        "<img src='a.jpg' src='b.jpg'>",
        // Non-image and vector types.
        "<img src='a.txt'>",
        "<img src='a.svg'>",
        "<img src='a.js'>",
        // Data URLs and already-rewritten URLs.
        "<img src='data:image/png;base64,AAAA'>",
        "<img src='xa.png.pagespeed.ic.0.png'>",
        // Blocked contexts and opt-outs.
        "<picture><img src='a.jpg'></picture>",
        "<noscript><img src='a.jpg'></noscript>",
        "<template><img src='a.jpg'></template>",
        "<svg><img src='a.jpg'></svg>",
        "<img data-pagespeed-no-transform src='a.jpg'>",
        "<img pagespeed_no_transform src='a.jpg'>",
    ] {
        assert_eq!(render_filter(&driver, input), input, "{input}");
    }
    // Ambiguous sizes: srcset still generated (100vw default applies).
    let out = render_filter(&driver, "<img src='a.jpg' sizes='1px' sizes='2px'>");
    assert!(out.contains("480w"), "{out}");
    assert!(out.contains("sizes='1px' sizes='2px'"), "{out}");
}

#[test]
fn absolute_and_relative_src_resolve() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    for src in ["photo.jpg", "/img/photo.jpg", "https://test.com/photo.jpg"] {
        let out = render_filter(&driver, &format!("<img src='{src}'>"));
        assert!(out.contains("480w") && out.contains("640w"), "{src}: {out}");
    }
    // Off-domain src follows the shared authorization gate
    // (map_output_base), exactly like image rewriting: default
    // options allow it, restrictive setups refuse it in minting.
    let out = render_filter(&driver, "<img src='https://evil.test/photo.jpg'>");
    assert!(out.contains("480w") && out.contains("640w"), "{out}");
    // Disallowed domains refuse minting: markup untouched.
    let (server, options) = fixture();
    options.borrow_mut().disallow("*evil.test*");
    let driver = driver_for(&server, &options);
    let input = "<img src='https://evil.test/photo.jpg'>";
    assert_eq!(render_filter(&driver, input), input);
}

#[test]
fn minted_variants_serve_at_declared_widths() {
    let (server, options, fetcher) = fixture_with_fetcher();
    let photo = include_bytes!("data/Puzzle.jpg");
    fetcher.set_simple_response("https://test.com/photo.jpg", "image/jpeg", photo);
    options
        .borrow_mut()
        .filters_mut()
        .enable(Filter::ResizeImages);
    let driver = driver_for(&server, &options);
    let out = render_filter(&driver, "<img src='photo.jpg'>");
    let srcset = out
        .split("srcset=\"")
        .nth(1)
        .expect("srcset")
        .split('"')
        .next()
        .unwrap();
    let mut pairs = Vec::new();
    for candidate in srcset.split(", ") {
        let (url, descriptor) = candidate.rsplit_once(' ').expect("descriptor");
        pairs.push((url.to_string(), descriptor.to_string()));
    }
    assert_eq!(pairs.len(), 2);
    for (url, descriptor) in &pairs {
        let absolute = GoogleUrl::resolve(&GoogleUrl::new(PAGE), url)
            .spec()
            .to_string();
        let serve_driver = driver_for(&server, &options);
        serve_driver.add_filter(Rc::new(ImageRewriteFilter::new(serve_driver.clone())));
        let ctx = serve_driver.request_context().borrow().clone();
        let mut fetch = BufferingFetch::new(&absolute, ctx);
        assert!(serve_driver.fetch_resource(&absolute, &mut fetch), "{url}");
        assert!(fetch.success(), "{url}");
        let analysis = ps_image::image::analyze(fetch.body()).expect("decodes");
        let want: u32 = descriptor.trim_end_matches('w').parse().unwrap();
        assert_eq!(analysis.width, want, "{url}");
    }
}

#[test]
fn driver_chain_plans_and_runs_first_pass() {
    let (server, options) = fixture();
    enable_responsive(&mut options.borrow_mut());
    let plan = build_chain(&options.borrow(), &ChainContext::new(true, true));
    assert!(plan.early_pre.contains(&ChainFilter::ResponsiveImageFirst));
    let out = render_driver(
        &options.borrow(),
        driver_for(&server, &options),
        "<img src='photo.jpg'>",
    );
    assert!(out.contains("480w") && out.contains("640w"), "{out}");
    assert!(out.contains(EAGER_SIZES), "{out}");
}
