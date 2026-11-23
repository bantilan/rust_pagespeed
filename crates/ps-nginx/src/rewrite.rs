//! Rewrite bridge (nginx ↔ ps-html sync drivers).
//!
//! Builds per-server HTML contexts over the assembled caches:
//! backends shared, statistics shared with the worker registry,
//! clock reset per request. Origin inputs load through the
//! injected fetcher (the file fetcher on mapped roots); the
//! native fetcher stays dormant until async drivers exist, and
//! unmapped origins fail the way network failures do.

use ps_cache::cache::Cache;
use ps_html::server_context::ServerContext as HtmlServerContext;
use ps_http::fetch::{AsyncFetch, AsyncFetcher};
use std::rc::Rc;
use std::sync::Arc;

/// Origin fetcher that always fails (`Done(false)`): unmapped
/// origins (proxy, aliases, unmapped roots) fail closed and
/// rewrites proceed from cache.
#[derive(Debug, Default)]
pub struct UnavailableFetcher;

impl AsyncFetcher for UnavailableFetcher {
    fn fetch(&self, _url: &str, fetch: &mut dyn AsyncFetch) {
        fetch.handle_done(false);
    }

    fn supports_https(&self) -> bool {
        false
    }
}

/// Builds an HTML context over assembled caches.
/// `http_backend`/`metadata` come from `setup_caches`;
/// `statistics` is the worker registry (shared, like C++);
/// `fetcher` serves origin inputs (the file fetcher on
/// static roots, unavailable otherwise).
#[must_use]
pub fn html_context(
    http_backend: Arc<dyn Cache>,
    metadata: Arc<dyn Cache>,
    statistics: Arc<ps_stats::core::Statistics>,
    start_ms: i64,
    fetcher: Arc<dyn AsyncFetcher>,
) -> Rc<HtmlServerContext> {
    let mut context =
        HtmlServerContext::new_with_metadata(http_backend, metadata, fetcher, start_ms);
    context.set_statistics(statistics);
    Rc::new(context)
}

/// Resets a context's clock to a request time (the shared wall
/// clock, per request).
pub fn reset_context_clock(context: &Rc<HtmlServerContext>, now_ms: i64) {
    context.timer().borrow_mut().set_ms(now_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::cache::MemoryCache;
    use ps_http::fetch::BufferingFetch;
    use ps_http::request::RequestContext;

    #[test]
    fn unavailable_fetcher_fails() {
        let fetcher = UnavailableFetcher;
        assert!(!fetcher.supports_https());
        let mut fetch = BufferingFetch::new(
            "http://origin/x.css",
            RequestContext::new(ps_http::response::HttpOptions::default_for_tests()),
        );
        fetcher.fetch("http://origin/x.css", &mut fetch);
        assert!(fetch.done());
        assert!(!fetch.success());
    }

    #[test]
    fn bridge_shares_caches_stats_and_clock() {
        let backend: Arc<dyn Cache> = Arc::new(MemoryCache::new());
        let metadata: Arc<dyn Cache> = Arc::new(MemoryCache::new());
        let statistics = Arc::new(ps_stats::core::Statistics::new());
        let context = html_context(
            backend,
            metadata,
            Arc::clone(&statistics),
            1000,
            Arc::new(UnavailableFetcher),
        );
        assert_eq!(1000, context.now_ms());
        reset_context_clock(&context, 2000);
        assert_eq!(2000, context.now_ms());
        // Stats flow to the shared registry.
        context.variable("bridge_probe").add(1);
        assert_eq!(
            1,
            statistics
                .get_variable("bridge_probe")
                .expect("shared")
                .get()
        );
    }
}
