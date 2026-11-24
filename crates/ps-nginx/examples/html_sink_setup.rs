//! Sink setup only: compare legacy option copying with driver-backed options.
use ps_config::options::RewriteOptions;
use ps_http::response::HttpOptions;
use ps_nginx::{
    caching::PreserveCachingHeaders,
    fetch::{BaseFetchType, SharedBaseFetch},
    send::HtmlSink,
};
use std::{
    cell::RefCell,
    hint::black_box,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

fn main() {
    let options = RewriteOptions::new();
    let metadata: Arc<dyn ps_cache::cache::Cache> = Arc::new(ps_cache::cache::MemoryCache::new());
    let server = ps_nginx::rewrite::html_context(
        Arc::new(ps_cache::cache::MemoryCache::new()),
        metadata,
        Arc::new(ps_stats::core::Statistics::new()),
        0,
        Arc::new(ps_nginx::rewrite::UnavailableFetcher),
    );
    let driver = Rc::new(ps_html::server_context::ResourceDriver::new(
        server,
        Rc::new(RefCell::new(options.clone())),
        "http://h/p",
    ));
    let (fetch, _ngx) = SharedBaseFetch::new_pair(
        "http://h/p",
        std::ptr::null_mut(),
        BaseFetchType::HtmlTransform,
        PreserveCachingHeaders::DontPreserve,
        HttpOptions::default(),
        false,
    );
    let slot = Mutex::new(None);
    let mut copying = Vec::new();
    let mut borrowing = Vec::new();
    for round in 0..7 {
        for optimized in if round % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let start = Instant::now();
            for _ in 0..10000 {
                let sink = if optimized {
                    HtmlSink::new_for_resource_driver(
                        "http://h/p",
                        false,
                        &fetch,
                        &slot,
                        driver.clone(),
                    )
                } else {
                    HtmlSink::new(
                        &options,
                        "http://h/p",
                        false,
                        &fetch,
                        &slot,
                        Some(driver.clone()),
                    )
                };
                black_box(sink);
            }
            let ns = start.elapsed().as_nanos() as f64 / 10000.0;
            if optimized {
                borrowing.push(ns);
            } else {
                copying.push(ns);
            }
        }
    }
    let median = |values: &[f64]| {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[3]
    };
    println!("{{\"scope\":\"sink construction/drop only; request driver already allocated; not HTML/request latency\",\"iterations\":10000,\"samples\":7,\"copying_median_ns\":{},\"driver_backed_median_ns\":{},\"copying_samples_ns\":{:?},\"driver_backed_samples_ns\":{:?}}}", median(&copying), median(&borrowing), copying, borrowing);
}
