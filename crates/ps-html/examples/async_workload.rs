//! Async-rewrite workload probe: drives `pages` cold-then-warm HTML
//! requests through one shared server with a slow origin, with or
//! without the background pool. Run under `/usr/bin/time -v` for
//! max RSS and CPU; the program prints latency and fetch counts.
//!
//! Usage: `async_workload [--pool|--no-pool] [--pages N]
//! [--origin-ms N] [--deadline-ms N]`.
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
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

const START_MS: i64 = 1_700_000_000_000;

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

fn drive(server: &Rc<ServerContext>, options: &Rc<RefCell<RewriteOptions>>, css_url: &str) {
    let resource_driver = Rc::new(ResourceDriver::new(
        server.clone(),
        options.clone(),
        "http://test.com/page.html",
    ));
    let ctx = ChainContext::new(true, true);
    let html = format!("<head><link rel='stylesheet' href='{css_url}'></head><body></body>");
    let mut driver = Driver::new(&options.borrow(), &ctx, Vec::new(), Some(resource_driver));
    driver.start_parse(b"http://test.com/page.html", false);
    driver.write(html.as_bytes());
    driver.finish();
}

fn stat(server: &ServerContext, name: &str) -> i64 {
    server
        .statistics()
        .get_variable(name)
        .map(|v| v.get())
        .unwrap_or(-1)
}

fn main() {
    let mut with_pool = true;
    let mut pages: usize = 50;
    let mut origin_ms: u64 = 20;
    let mut deadline_ms: i64 = 10;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pool" => with_pool = true,
            "--no-pool" => with_pool = false,
            "--pages" => pages = args.next().unwrap_or_default().parse().unwrap_or(pages),
            "--origin-ms" => {
                origin_ms = args.next().unwrap_or_default().parse().unwrap_or(origin_ms)
            }
            "--deadline-ms" => {
                deadline_ms = args
                    .next()
                    .unwrap_or_default()
                    .parse()
                    .unwrap_or(deadline_ms);
            }
            other => eprintln!("ignoring unknown arg: {other}"),
        }
    }

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
        delay: Duration::from_millis(origin_ms),
        inner: MockUrlFetcher::new(),
    });
    for page in 0..pages {
        let mut headers = ResponseHeaders::new(HttpOptions::default_for_tests());
        headers.set_first_line(1, 1, 200, "OK");
        headers.headers_mut().add(header::CONTENT_TYPE, "text/css");
        headers.set_date_and_caching(START_MS, 300_000, "");
        headers.compute_caching();
        fetcher.inner.set_response(
            &format!("http://test.com/p{page}.css"),
            headers,
            b"div{display:block}",
        );
    }
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        fetcher.clone() as Arc<dyn AsyncFetcher>,
        START_MS,
    ));
    let pool = if with_pool {
        let pool = BackgroundPool::new(
            PoolConfig {
                threads: 1,
                queue_bound: 1024,
            },
            &server.statistics_shared(),
        );
        server.set_background_pool(Arc::clone(&pool));
        Some(pool)
    } else {
        None
    };

    // Cold pass: every page misses; async waits only the deadline.
    let start = Instant::now();
    for page in 0..pages {
        drive(&server, &options, &format!("http://test.com/p{page}.css"));
    }
    let cold_elapsed = start.elapsed();
    // Let background completions land, then serve everything warm.
    if with_pool {
        let deadline = Instant::now() + Duration::from_secs(60);
        while stat(&server, "background_jobs_completed") < pages as i64 {
            assert!(Instant::now() < deadline, "background jobs stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let start = Instant::now();
    for page in 0..pages {
        drive(&server, &options, &format!("http://test.com/p{page}.css"));
    }
    let warm_elapsed = start.elapsed();
    if let Some(pool) = &pool {
        pool.shutdown();
    }
    println!("pool={with_pool} pages={pages} origin_ms={origin_ms} deadline_ms={deadline_ms}");
    println!(
        "cold_pass_ms={} warm_pass_ms={} origin_fetches={} timeouts={} completed={}",
        cold_elapsed.as_millis(),
        warm_elapsed.as_millis(),
        fetcher.inner.fetch_count(),
        stat(&server, "background_jobs_timeouts"),
        stat(&server, "background_jobs_completed"),
    );
}
