//! Background-fetch benchmarks: first-request latency, warm-hit
//! latency, and concurrent cold requests, sync (no pool) vs async
//! (pool + deadline). The sync leg is the pre-change behavior: no
//! pool registered anywhere, so every load fetches inline.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use ps_cache::cache::MemoryCache;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::background::{BackgroundPool, PoolConfig};
use ps_html::chain::ChainContext;
use ps_html::driver::Driver;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use ps_http::mock::MockUrlFetcher;
use ps_http::names::header;
use ps_http::response::{HttpOptions, ResponseHeaders};
use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const PAGE_URL: &str = "http://test.com/page.html";
const CSS_BODY: &[u8] = b"div{display:block}";
const START_MS: i64 = 1_700_000_000_000;

fn html_for(css_url: &str) -> Vec<u8> {
    format!("<head><link rel='stylesheet' href='{css_url}'></head><body></body>").into_bytes()
}

/// Scripted origin with overt latency.
struct SlowFetcher {
    delay: Duration,
    inner: MockUrlFetcher,
}

impl AsyncFetcher for SlowFetcher {
    fn fetch(&self, url: &str, fetch: &mut dyn AsyncFetch) {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.inner.fetch(url, fetch);
    }
}

struct World {
    server: Rc<ServerContext>,
    options: Rc<RefCell<RewriteOptions>>,
    html: Vec<u8>,
}

impl World {
    /// Cold server: scripted slow CSS origin, optional pool.
    fn new(
        origin_delay: Duration,
        deadline_ms: i64,
        pool: Option<&Arc<BackgroundPool>>,
        css_url: &str,
    ) -> Self {
        let options = Rc::new(RefCell::new(RewriteOptions::new()));
        options.borrow_mut().filters_mut().enable(Filter::InlineCss);
        options
            .borrow_mut()
            .set_option(
                "RewriteDeadlinePerFlushMs",
                &deadline_ms.to_string(),
                Scope::Server,
            )
            .unwrap();
        let fetcher = Arc::new(SlowFetcher {
            delay: origin_delay,
            inner: MockUrlFetcher::new(),
        });
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.set_date_and_caching(START_MS, 300_000, "");
        headers.compute_caching();
        fetcher.inner.set_response(css_url, headers, CSS_BODY);
        let server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            fetcher as Arc<dyn AsyncFetcher>,
            START_MS,
        ));
        if let Some(pool) = pool {
            server.set_background_pool(Arc::clone(pool));
        }
        World {
            server,
            options,
            html: html_for(css_url),
        }
    }

    /// Drives one full HTML request.
    fn drive(&self) {
        let resource_driver = Rc::new(ResourceDriver::new(
            self.server.clone(),
            self.options.clone(),
            PAGE_URL,
        ));
        let ctx = ChainContext::new(true, true);
        let mut driver = Driver::new(
            &self.options.borrow(),
            &ctx,
            Vec::new(),
            Some(resource_driver),
        );
        driver.start_parse(PAGE_URL.as_bytes(), false);
        driver.write(&self.html);
        driver.finish();
    }
}

/// One pool per benchmark arm (per-iteration pools would leak
/// threads, and shutdown joins in-flight work).
fn shared_pool() -> (Arc<BackgroundPool>, Arc<ps_stats::core::Statistics>) {
    let statistics = Arc::new(ps_stats::core::Statistics::new());
    let pool = BackgroundPool::new(
        PoolConfig {
            threads: 2,
            queue_bound: 1024,
        },
        &statistics,
    );
    (pool, statistics)
}

/// Blocks until the completion count advances past `baseline`.
fn await_completion_since(statistics: &ps_stats::core::Statistics, baseline: i64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let completed = statistics
            .get_variable("background_jobs_completed")
            .unwrap()
            .get();
        if completed > baseline {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background fetch never completed"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Cold first request: 50ms origin, 10ms deadline. Each iteration
/// uses a fresh URL so no dedup or cache entry leaks across iters.
fn bench_cold_first_request(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("cold_first_request");
    for (name, with_pool) in [("sync", false), ("async", true)] {
        let shared = with_pool.then(shared_pool);
        let pool = shared.as_ref().map(|(pool, _)| pool);
        let mut iter: u64 = 0;
        group.bench_with_input(
            BenchmarkId::new(name, "50ms_origin_10ms_deadline"),
            &with_pool,
            |bencher, _| {
                bencher.iter_batched(
                    || {
                        iter += 1;
                        World::new(
                            Duration::from_millis(50),
                            10,
                            pool,
                            &format!("http://test.com/a{iter}.css"),
                        )
                    },
                    |world| black_box(world.drive()),
                    criterion::BatchSize::SmallInput,
                );
            },
        );
        if let Some((pool, _)) = shared {
            pool.shutdown();
        }
    }
    group.finish();
}

/// Warm hit: steady-state metadata-hit serve after warmup.
fn bench_warm_hit(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("warm_hit");
    for (name, with_pool) in [("sync", false), ("async", true)] {
        let shared = with_pool.then(shared_pool);
        let pool = shared.as_ref().map(|(pool, _)| pool);
        let statistics = shared.as_ref().map(|(_, statistics)| statistics);
        group.bench_with_input(
            BenchmarkId::new(name, "inline_css"),
            &with_pool,
            |bencher, _| {
                bencher.iter_batched(
                    || {
                        let world = World::new(Duration::ZERO, 10, pool, "http://test.com/a.css");
                        if let Some(statistics) = statistics {
                            // First async drive drops its partition
                            // (pending input); wait for the origin,
                            // then drive again to write it.
                            let baseline = statistics
                                .get_variable("background_jobs_completed")
                                .unwrap()
                                .get();
                            world.drive();
                            await_completion_since(statistics, baseline);
                            world.drive();
                        } else {
                            world.drive();
                        }
                        world
                    },
                    |world| black_box(world.drive()),
                    criterion::BatchSize::SmallInput,
                );
            },
        );
        if let Some((pool, _)) = shared {
            pool.shutdown();
        }
    }
    group.finish();
}

/// Eight concurrent cold requests sharing one pool (async) vs eight
/// independent direct fetches (sync); 100ms origin, 10ms deadline.
fn bench_concurrent_cold(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("concurrent_cold");
    group.sample_size(20);
    for (name, with_pool) in [("sync", false), ("async", true)] {
        let shared = with_pool.then(shared_pool);
        let mut iter: u64 = 0;
        group.bench_with_input(
            BenchmarkId::new(name, "8_threads_100ms_origin"),
            &with_pool,
            |bencher, _| {
                bencher.iter_batched(
                    || {
                        iter += 1;
                        format!("http://test.com/c{iter}.css")
                    },
                    |css_url| {
                        let mut handles = Vec::new();
                        for _ in 0..8 {
                            let css_url = css_url.clone();
                            let pool = shared.as_ref().map(|(pool, _)| Arc::clone(pool));
                            handles.push(std::thread::spawn(move || {
                                let world = World::new(
                                    Duration::from_millis(100),
                                    10,
                                    pool.as_ref(),
                                    &css_url,
                                );
                                world.drive();
                            }));
                        }
                        for handle in handles {
                            handle.join().unwrap();
                        }
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
        if let Some((pool, _)) = shared {
            pool.shutdown();
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_cold_first_request,
    bench_warm_hit,
    bench_concurrent_cold
);
criterion_main!(benches);
