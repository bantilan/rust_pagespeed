//! Critical-CSS option compatibility with ordinary/early HTML cache lookup.
//! No network, disk, production traffic or filter execution.
use ps_cache::{cache::MemoryCache, purge::PurgeSet};
use ps_config::{options::RewriteOptions, scope::Scope};
use ps_html::{
    html_cache::HtmlCacheSession,
    server_context::{ResourceDriver, ServerContext},
};
use ps_http::{
    mock::MockUrlFetcher,
    request::{Method, RequestHeaders},
    response::{HttpOptions, ResponseHeaders},
};
use std::{cell::RefCell, rc::Rc, sync::Arc, time::Instant};
fn main() {
    const NOW: i64 = 1_700_000_000_000;
    const URL: &str = "https://cookies.test/page";
    println!("fixture,path,median_us,p95_us,measured_hits");
    for (fixture, critical_css, known) in [
        ("css-off", false, 0),
        ("css-on-unlearned", true, 0),
        ("css-on-known-1", true, 1),
        ("css-on-known-128", true, 128),
    ] {
        let server = Rc::new(ServerContext::new_with_metadata(
            Arc::new(MemoryCache::new()),
            Arc::new(MemoryCache::new()),
            Arc::new(MockUrlFetcher::new()),
            NOW,
        ));
        let mut options = RewriteOptions::new();
        for name in ["HtmlCache", "HtmlCacheEarlyLookup"] {
            options.set_option(name, "on", Scope::Server).unwrap();
        }
        if critical_css {
            options
                .filters_mut()
                .enable(ps_config::filter::Filter::PrioritizeCriticalCss);
        }
        let driver = Rc::new(ResourceDriver::new(
            server,
            Rc::new(RefCell::new(options)),
            URL,
        ));
        for i in 0..known {
            assert!(ps_html::critical_css_blocking::remember(
                &driver,
                &format!("https://cookies.test/{i}.css"),
                b"div{color:red}",
                NOW + 300000,
            ));
        }
        let mut request = RequestHeaders::new();
        request.set_method(Method::Get);
        request.set_version(1, 1);
        request.headers_mut().add("Host", "cookies.test");
        request.headers_mut().add("Accept", "text/html");
        let mut origin = ResponseHeaders::new(HttpOptions::default_for_tests());
        origin.set_status_and_reason(200);
        origin.headers_mut().add("Content-Type", "text/html");
        origin
            .headers_mut()
            .add("Cache-Control", "public, max-age=300");
        origin
            .headers_mut()
            .add("Date", &ps_http::time::format_http_date(NOW));
        origin.compute_caching();
        let purge = PurgeSet::new(25000);
        let body = vec![b'x'; 32768];
        let mut first = HtmlCacheSession::new(driver.clone(), URL);
        assert!(first.lookup(&origin, &request, &purge, None, NOW).is_none());
        first.write(&body);
        first.finish(&request, NOW);
        for early in [false, true] {
            let mut hits = 0;
            let mut samples = Vec::new();
            for i in 0..2050 {
                let start = Instant::now();
                let mut session = HtmlCacheSession::new(driver.clone(), URL);
                let entry = if early {
                    session
                        .lookup_early(&request, &purge, None, NOW + 1)
                        .map(|(e, _)| e)
                } else {
                    session.lookup(&origin, &request, &purge, None, NOW + 1)
                };
                let ns = start.elapsed().as_nanos();
                if let Some(entry) = entry {
                    assert_eq!(entry.body, body);
                    if i >= 50 {
                        hits += 1;
                    }
                    std::hint::black_box(entry);
                }
                if i >= 50 {
                    samples.push(ns);
                }
            }
            samples.sort_unstable();
            println!(
                "{fixture},{},{:.3},{:.3},{}",
                if early { "early" } else { "ordinary" },
                samples[1000] as f64 / 1000.,
                samples[1899] as f64 / 1000.,
                hits
            );
        }
    }
}
