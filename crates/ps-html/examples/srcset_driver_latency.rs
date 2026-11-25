//! Same-build filter-off/on setup and parsing cost, not NGINX/request latency.
use ps_config::{options::RewriteOptions, scope::Scope};
use ps_html::{
    chain::ChainContext,
    driver::Driver,
    server_context::{ResourceDriver, ServerContext},
    writer::ByteSink,
};
use std::{cell::RefCell, hint::black_box, rc::Rc, sync::Arc, time::Instant};

struct Sink;
impl ByteSink for Sink {
    fn write_bytes(&mut self, bytes: &[u8]) -> bool {
        black_box(bytes.len());
        true
    }
}

fn request(
    server: &Rc<ServerContext>,
    options: &Rc<RefCell<RewriteOptions>>,
    html: &[u8],
) -> [f64; 4] {
    let start = Instant::now();
    let resources = Rc::new(ResourceDriver::new(
        server.clone(),
        options.clone(),
        "http://fixture.test/page",
    ));
    let resources_done = Instant::now();
    let mut driver = Driver::new(
        &options.borrow(),
        &ChainContext::new(true, true),
        Sink,
        Some(resources),
    );
    let filters_done = Instant::now();
    driver.start_parse(b"http://fixture.test/page", false);
    driver.write(black_box(html));
    driver.finish();
    drop(driver);
    let end = Instant::now();
    [
        resources_done.duration_since(start).as_nanos() as f64,
        filters_done.duration_since(resources_done).as_nanos() as f64,
        end.duration_since(filters_done).as_nanos() as f64,
        end.duration_since(start).as_nanos() as f64,
    ]
}

fn main() {
    // Optional mode/iteration arguments allow separate-process peak-RSS samples.
    let args: Vec<_> = std::env::args().collect();
    let selected = args.get(1).map(String::as_str).unwrap_or("both");
    assert!(["both", "off", "on"].contains(&selected));
    let iterations: usize = args.get(2).map(|n| n.parse().unwrap()).unwrap_or(500);
    assert!(iterations > 0);
    let active = [selected != "on", selected != "off"];

    let html = format!("<html><head></head><body>{}<img class='hero' src='800.jpg' srcset='300.jpg 300w, 768.jpg 768w, 800.jpg 800w' sizes='100vw'></body></html>", "<p>Some realistic text content.</p>".repeat(1000));
    println!("{{\"scope\":\"in-process driver only; timers include overhead; no HTML cache or network\",\"batches\":9,\"iterations_per_batch\":{},\"html_bytes\":{},\"cases\":[",iterations,html.len());
    // Alternate order across batches; distinct cold plans for initial samples.
    let fetcher = Arc::new(ps_http::mock::MockUrlFetcher::new());
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(ps_cache::cache::MemoryCache::new()),
        Arc::new(ps_cache::cache::MemoryCache::new()),
        fetcher.clone(),
        1_700_000_000_000,
    ));
    let options: Vec<_> = ["off", "on"]
        .iter()
        .map(|mode| {
            let mut o = RewriteOptions::new();
            for (key, value) in [
                ("SrcsetSizesEnabled", *mode),
                ("SrcsetSizesClass", "hero"),
                (
                    "SrcsetSizes",
                    "(max-width: 800px) calc(100vw - 60px), 645px",
                ),
            ] {
                o.set_option(key, value, Scope::Server).unwrap();
            }
            Rc::new(RefCell::new(o))
        })
        .collect();
    let cold: Vec<_> = options
        .iter()
        .enumerate()
        .map(|(mode, o)| {
            if active[mode] {
                request(&server, o, html.as_bytes())
            } else {
                [0.0; 4]
            }
        })
        .collect();
    for _ in 0..100 {
        for (mode, o) in options.iter().enumerate() {
            if active[mode] {
                request(&server, o, html.as_bytes());
            }
        }
    }
    let mut batches: [Vec<[f64; 4]>; 2] = [Vec::new(), Vec::new()];
    for batch in 0..9 {
        for mode in if batch % 2 == 0 { [0, 1] } else { [1, 0] } {
            if !active[mode] {
                continue;
            }
            let mut sums = [0.0; 4];
            for _ in 0..iterations {
                let sample = request(&server, &options[mode], html.as_bytes());
                for i in 0..4 {
                    sums[i] += sample[i] / iterations as f64;
                }
            }
            batches[mode].push(sums);
        }
    }
    assert_eq!(fetcher.fetch_count(), 0);
    let mut first = true;
    for (mode, name) in ["off", "on"].iter().enumerate() {
        if !active[mode] {
            continue;
        }
        let medians: Vec<_> = (0..4)
            .map(|i| {
                let mut values: Vec<_> = batches[mode].iter().map(|b| b[i]).collect();
                values.sort_by(f64::total_cmp);
                values[4]
            })
            .collect();
        println!("{}{{\"mode\":\"{}\",\"cold_first_sample_ns\":{:?},\"warm_medians_ns\":{:?},\"warm_batches_ns\":{:?}}}", if first {""} else {","},name,cold[mode],medians,batches[mode]);
        first = false;
    }
    println!("],\"stage_order\":[\"resource_driver_setup\",\"filter_setup_with_cached_plan\",\"html_processing\",\"total\"],\"resource_fetches\":0}}");
}
