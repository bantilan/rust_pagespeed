//! Stage-timing breakdown for HTML requests (profiling harness).
//!
//! Measures per-stage latency so optimizations report before/after
//! numbers from execution, not assumptions: options signature,
//! filter-chain construction (`Driver::new`), metadata cache
//! lookup, full HTML rewrite cold (metadata miss) and warm
//! (metadata hit), and HTML-cache HIT lookup.
//!
//! Usage: `timing_breakdown [--pages N] [--resources N]`.
use ps_cache::cache::{Cache, MemoryCache};
use ps_config::filter::{Filter, RewriteLevel};
use ps_config::options::RewriteOptions;
use ps_html::chain::{build_chain, ChainContext};
use ps_html::driver::Driver;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::response::{HttpOptions, ResponseHeaders};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

const START_MS: i64 = 1_700_000_000_000;

fn drive_once(
    server: &Rc<ServerContext>,
    options: &Rc<RefCell<RewriteOptions>>,
    html: &[u8],
) -> Vec<u8> {
    let resource_driver = Rc::new(ResourceDriver::new(
        server.clone(),
        options.clone(),
        "http://test.com/page.html",
    ));
    let ctx = ChainContext::new(true, true);
    let mut driver = Driver::new(&options.borrow(), &ctx, Vec::new(), Some(resource_driver));
    driver.start_parse(b"http://test.com/page.html", false);
    driver.write(html);
    driver.finish();
    Vec::new()
}

fn main() {
    let mut pages: usize = 20;
    let mut resources: usize = 10;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pages" => pages = args.next().unwrap_or_default().parse().unwrap_or(pages),
            "--resources" => {
                resources = args.next().unwrap_or_default().parse().unwrap_or(resources)
            }
            other => eprintln!("ignoring unknown arg: {other}"),
        }
    }

    let mut opts = RewriteOptions::new();
    opts.filters_mut().set_level(RewriteLevel::CoreFilters);
    opts.filters_mut().enable(Filter::LazyloadImages);
    opts.filters_mut().enable(Filter::InlineCss);
    opts.filters_mut().enable(Filter::InlineJavascript);

    // Stage 1: options signature cost (per-call, uncached baseline).
    let start = Instant::now();
    let iters = 1000;
    for _ in 0..iters {
        std::hint::black_box(opts.signature());
    }
    let sig_total = start.elapsed();
    println!(
        "signature: per_call_ns={} ({} iters)",
        sig_total.as_nanos() / iters as u128,
        iters
    );

    // Stage 2: chain plan + filter construction cost.
    let ctx = ChainContext::new(true, true);
    let start = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(build_chain(&opts, &ctx));
    }
    let plan_total = start.elapsed();
    println!(
        "build_chain: per_call_ns={} ({} iters)",
        plan_total.as_nanos() / iters as u128,
        iters
    );

    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Arc::new(MockUrlFetcher::new()),
        START_MS,
    ));
    let options = Rc::new(RefCell::new(opts));
    let start = Instant::now();
    let driver_iters = 200;
    for _ in 0..driver_iters {
        let rd = Rc::new(ResourceDriver::new(
            server.clone(),
            options.clone(),
            "http://test.com/page.html",
        ));
        std::hint::black_box(Driver::new(&options.borrow(), &ctx, Vec::new(), Some(rd)));
    }
    let driver_total = start.elapsed();
    println!(
        "driver_new_filter_init: per_call_us={} ({} iters)",
        driver_total.as_micros() / driver_iters as u128,
        driver_iters
    );

    // Stage 3: full HTML rewrite, cold metadata then warm.
    let fetcher = MockUrlFetcher::new();
    let mut html = String::from("<html><head>");
    for i in 0..resources {
        let url = format!("http://test.com/r{i}.css");
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.set_date_and_caching(START_MS, 300_000, "");
        headers.compute_caching();
        fetcher.set_response(&url, headers, b"div{display:block}/*x*/");
        html.push_str(&format!("<link rel='stylesheet' href='{url}'>"));
    }
    html.push_str("</head><body><p>hi</p></body></html>");
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        Arc::new(fetcher),
        START_MS,
    ));
    // Cold: first render misses metadata for every resource.
    let start = Instant::now();
    for _ in 0..pages {
        std::hint::black_box(drive_once(&server, &options, html.as_bytes()));
    }
    let cold_total = start.elapsed();
    println!(
        "html_rewrite_first_view: per_page_us={} ({} pages, {} resources each)",
        cold_total.as_micros() / pages as u128,
        pages,
        resources
    );
    // Warm: metadata cached; measures hit-path HTML cost.
    let start = Instant::now();
    for _ in 0..pages {
        std::hint::black_box(drive_once(&server, &options, html.as_bytes()));
    }
    let warm_total = start.elapsed();
    println!(
        "html_rewrite_warm_metadata: per_page_us={} ({} pages)",
        warm_total.as_micros() / pages as u128,
        pages
    );

    // Stage 4: HTML-cache MISS (lookup + buffer + store) vs HIT
    // (lookup + serve, no filters, no parse).
    {
        use ps_cache::purge::PurgeSet;
        use ps_html::html_cache::HtmlCacheSession;
        use ps_http::request::{Method, RequestHeaders};
        let mut hopts = RewriteOptions::new();
        hopts
            .set_option("HtmlCache", "on", ps_config::scope::Scope::Server)
            .unwrap();
        hopts
            .set_option("HtmlCacheTtlSec", "60", ps_config::scope::Scope::Server)
            .unwrap();
        let hoptions = Rc::new(RefCell::new(hopts));
        let hserver = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(MockUrlFetcher::new()),
            START_MS,
        ));
        let mut request = RequestHeaders::new();
        request.set_method(Method::Get);
        request.set_version(1, 1);
        request.headers_mut().add("Host", "test.com");
        let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        origin.set_date_and_caching(START_MS, 300_000, "");
        origin.compute_caching();
        let purge = PurgeSet::new(25_000);
        let body = html.as_bytes().to_vec();
        // Prime one entry.
        let hdriver = Rc::new(ResourceDriver::new(
            hserver.clone(),
            hoptions.clone(),
            "http://test.com/page.html",
        ));
        let mut prime = HtmlCacheSession::new(hdriver, "http://test.com/page.html");
        assert!(prime
            .lookup(&origin, &request, &purge, None, START_MS)
            .is_none());
        prime.write(&body);
        prime.finish(&request, START_MS + 1000);
        // MISS on a fresh URL (lookup + buffer + store).
        let start = Instant::now();
        let hiters = 2000;
        for i in 0..hiters {
            let url = format!("http://test.com/miss{i}.html");
            let d = Rc::new(ResourceDriver::new(
                hserver.clone(),
                hoptions.clone(),
                &url,
            ));
            let mut s = HtmlCacheSession::new(d, &url);
            std::hint::black_box(s.lookup(&origin, &request, &purge, None, START_MS + 1000));
            s.write(&body);
            std::hint::black_box(s.finish(&request, START_MS + 1000));
        }
        println!(
            "html_cache_miss_store: per_call_us={} ({} iters)",
            start.elapsed().as_micros() / hiters as u128,
            hiters
        );
        // HIT on the primed URL (lookup only: the HIT path).
        let start = Instant::now();
        let mut hits = 0;
        for _ in 0..hiters {
            let d = Rc::new(ResourceDriver::new(
                hserver.clone(),
                hoptions.clone(),
                "http://test.com/page.html",
            ));
            let mut s = HtmlCacheSession::new(d, "http://test.com/page.html");
            if s.lookup(&origin, &request, &purge, None, START_MS + 1000).is_some() {
                hits += 1;
            }
        }
        println!(
            "html_cache_hit_lookup: per_call_us={} ({} iters, {} hits)",
            start.elapsed().as_micros() / hiters as u128,
            hiters,
            hits
        );
    }

    // Stage 5: raw metadata cache op cost (memory backend).
    let meta = MemoryCache::new();
    meta.put("k", b"value-bytes-for-timing");
    let start = Instant::now();
    let cache_iters = 100_000;
    for _ in 0..cache_iters {
        std::hint::black_box(meta.get("k"));
    }
    let cache_total = start.elapsed();
    println!(
        "metadata_get_memory: per_call_ns={} ({} iters)",
        cache_total.as_nanos() / cache_iters as u128,
        cache_iters
    );
}
