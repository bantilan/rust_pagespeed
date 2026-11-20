//! Terminal handlers (simple/resource/admin bodies).
//!
//! Ports the in-memory response path: static-asset and message
//! bodies with fixed headers (`ps_simple_handler`), admin pages
//! through the admin site, and `.pagespeed.` resources through
//! the sync rewrite bridge. Serving (headers-out, chains, final
//! codes) stays FFI over the outcomes here.

use crate::assets::{get_asset_by_name, AssetMiss};
use ps_admin::purge::PurgeSetBackend;
use ps_admin::site::{AdminConfig, AdminRequest, AdminSite, CacheInventory};
use ps_cache::purge::PurgeSet;
use ps_html::keywords::escape as html_escape_bytes;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::query_params::QueryParams;
use ps_http::request::RequestContext;
use ps_http::response::HttpOptions;
use std::sync::{Arc, Mutex};

/// Messages-disabled body (verbatim, trailing space included).
pub const MESSAGES_DISABLED_BODY: &str =
    "Writing to ngx_pagespeed_message failed. \nPlease check if it's enabled in pagespeed.conf.\n";
/// Invalid-asset log format (`GetAsset` error).
pub const INVALID_ASSET_LOG: &str = "Invalid url requested: ";
/// Messages content type (HTML, like C++).
pub const HTML_MIME: &str = "text/html";

/// In-memory response: status, content type, cache header, body.
/// The FFI adds Date/Last-Modified/nosniff, discards the request
/// body, and serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleResponse {
    /// HTTP status.
    pub status: u16,
    /// Content-Type value.
    pub content_type: String,
    /// Cache-Control value.
    pub cache_control: String,
    /// Response body.
    pub body: Vec<u8>,
}

/// Simple-asset outcome (`ps_simple_handler` static branch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimpleOutcome {
    /// Serve the response.
    Serve(SimpleResponse),
    /// Decline to nginx (unknown asset).
    Decline,
    /// Log the message, then decline (malformed name).
    LogDecline(String),
}

/// Serves a static asset by hashed filename.
#[must_use]
pub fn simple_asset(file_name: &str) -> SimpleOutcome {
    match get_asset_by_name(file_name) {
        Ok(served) => SimpleOutcome::Serve(SimpleResponse {
            status: 200,
            content_type: served.content_type.to_string(),
            cache_control: served.cache_header.to_string(),
            body: served.content.to_vec(),
        }),
        Err(AssetMiss::InvalidName) => {
            SimpleOutcome::LogDecline(format!("{INVALID_ASSET_LOG}{file_name}."))
        }
        Err(AssetMiss::Unknown) => SimpleOutcome::Decline,
    }
}

/// Serves the message history (`ps_simple_handler` messages
/// branch): `None` renders the disabled notice.
#[must_use]
pub fn simple_messages(dump: Option<&str>) -> SimpleResponse {
    let body = match dump {
        Some(log) => {
            let mut escaped = Vec::with_capacity(log.len());
            html_escape_bytes(log.as_bytes(), &mut escaped);
            let mut body = b"<pre>".to_vec();
            body.extend_from_slice(&escaped);
            body.extend_from_slice(b"</pre>\n");
            body
        }
        None => MESSAGES_DISABLED_BODY.as_bytes().to_vec(),
    };
    SimpleResponse {
        status: 200,
        content_type: HTML_MIME.to_string(),
        cache_control: ps_http::names::header::NO_CACHE.to_string(),
        body,
    }
}

/// Fetched resource: status, ordered headers, body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceResponse {
    /// HTTP status (0 when the fetch never completed headers).
    pub status: u16,
    /// Response headers in order.
    pub headers: Vec<(String, String)>,
    /// Response body.
    pub body: Vec<u8>,
}

/// Fetches a `.pagespeed.` resource through the sync bridge
/// (`ResourceFetch::Start` shape): resets the context clock,
/// builds a driver over the request options, drives
/// `fetch_resource`, and collects the outcome.
pub fn fetch_resource_body(
    html: &std::rc::Rc<ps_html::server_context::ServerContext>,
    options: &ps_config::options::RewriteOptions,
    url: &str,
    now_ms: i64,
) -> ResourceResponse {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::rewrite::reset_context_clock(html, now_ms);
    let mut fetch = BufferingFetch::new(url, RequestContext::new(HttpOptions::default()));
    let driver = Rc::new(ps_html::server_context::ResourceDriver::new(
        Rc::clone(html),
        Rc::new(RefCell::new(options.clone())),
        url,
    ));
    ps_html::server_context::ResourceDriver::fetch_resource(&driver, url, &mut fetch);
    collect_fetch(&fetch)
}

/// Serves an origin URL in place through the sync bridge
/// (the nginx content handler calls `FetchInPlaceResource`
/// directly with proxy mode off): unrewritable content
/// fails back to the server's native method.
pub fn fetch_in_place_body(
    html: &std::rc::Rc<ps_html::server_context::ServerContext>,
    options: &ps_config::options::RewriteOptions,
    url: &str,
    now_ms: i64,
) -> ResourceResponse {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::rewrite::reset_context_clock(html, now_ms);
    let mut fetch = BufferingFetch::new(url, RequestContext::new(HttpOptions::default()));
    let driver = Rc::new(ps_html::server_context::ResourceDriver::new(
        Rc::clone(html),
        Rc::new(RefCell::new(options.clone())),
        url,
    ));
    ps_html::ipro::fetch_in_place_resource(&driver, url, &mut fetch, false);
    collect_fetch(&fetch)
}

/// Collects a buffered fetch into a servable response.
fn collect_fetch(fetch: &BufferingFetch) -> ResourceResponse {
    let headers = fetch
        .response_headers()
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    ResourceResponse {
        status: fetch.response_headers().status_code().max(0) as u16,
        headers,
        body: fetch.body().to_vec(),
    }
}

/// Collects an admin render: the leaves always set a status
/// (the opener stamps 200), so a 0 here would send nginx a
/// garbage status line and defaults to 200 instead.
fn collect_admin(fetch: &BufferingFetch) -> ResourceResponse {
    let mut response = collect_fetch(fetch);
    if response.status == 0 {
        response.status = 200;
    }
    response
}

/// Admin page inputs.
pub struct AdminInputs<'a> {
    /// Request options (core).
    pub options: &'a ps_config::options::RewriteOptions,
    /// System options (admin knobs).
    pub system: &'a ps_config::options::NginxSystemOptions,
    /// Worker statistics (live + display).
    pub statistics: &'a ps_stats::core::Statistics,
    /// Shared message buffer.
    pub log: &'a Arc<Mutex<ps_admin::log_buffer::LogBuffer>>,
    /// Shared purge set (locked by the caller).
    pub purge_set: &'a mut PurgeSet,
    /// Cache inventory for the caches leaf.
    pub inventory: CacheInventory,
    /// Full admin URL.
    pub url: &'a str,
    /// Global (vs per-vhost) console.
    pub is_global: bool,
    /// Request time.
    pub now_ms: i64,
}

/// Renders an admin page (`AdminPage`) through a per-request
/// site: purges land in the shared set (fetch-flow validity is a
/// follow-up), metadata lookup renders its notice.
pub fn admin_response(inputs: AdminInputs) -> ResourceResponse {
    let site = admin_site_for(inputs.log, inputs.now_ms);
    let config = admin_config_for(inputs.system);
    let display_set = inputs.purge_set.clone();
    let mut backend = PurgeSetBackend::new(std::mem::replace(
        &mut *inputs.purge_set,
        PurgeSet::new(25_000),
    ));
    let stripped_url = GoogleUrl::new(inputs.url);
    let mut query = QueryParams::new();
    if let Some(index) = inputs.url.find('?') {
        query.parse_from_untrusted_string(&inputs.url[index + 1..]);
    }
    let mut fetch = BufferingFetch::new(inputs.url, RequestContext::new(HttpOptions::default()));
    let mut request = AdminRequest {
        options: inputs.options,
        global_options: &config,
        statistics: inputs.statistics,
        stats: inputs.statistics,
        inventory: Some(&inputs.inventory),
        show_cache: None,
        purge: &mut backend,
        purge_set: &display_set,
    };
    site.admin_page(
        inputs.is_global,
        &stripped_url,
        &query,
        inputs.options,
        &mut fetch,
        &mut request,
    );
    *inputs.purge_set = backend.purge_set;
    collect_admin(&fetch)
}

/// Statistics inputs: a subset of [`AdminInputs`] (no purge
/// set, no inventory; the caches leaf renders its notice).
pub struct StatisticsInputs<'a> {
    /// Request options (core).
    pub options: &'a ps_config::options::RewriteOptions,
    /// System options (admin knobs).
    pub system: &'a ps_config::options::NginxSystemOptions,
    /// Statistics registry to render.
    pub statistics: &'a ps_stats::core::Statistics,
    /// Shared message buffer.
    pub log: &'a Arc<Mutex<ps_admin::log_buffer::LogBuffer>>,
    /// Full statistics URL (query selects the leaf).
    pub url: &'a str,
    /// Global (vs per-vhost) statistics.
    pub is_global: bool,
    /// Request time.
    pub now_ms: i64,
}

/// Builds a per-request admin site over the shared log.
fn admin_site_for(log: &Arc<Mutex<ps_admin::log_buffer::LogBuffer>>, now_ms: i64) -> AdminSite {
    use ps_cache::timer::MockTimer;
    AdminSite::new(
        Arc::new(Mutex::new(MockTimer::new(now_ms))),
        Arc::clone(log),
    )
}

/// Builds the system admin knobs from system options.
fn admin_config_for(system: &ps_config::options::NginxSystemOptions) -> AdminConfig {
    AdminConfig {
        history_archive: false,
        file_cache_path: system.file_cache_path.clone(),
        history_archive_interval: 300,
        history_archive_retention_days: 7,
        history_archive_max_size_mb: 100,
        statistics_enabled: system.statistics_enabled,
        statistics_logging_enabled: false,
        log_dir: system.log_dir.clone(),
    }
}

/// Renders a statistics page (`StatisticsPage`): the query
/// selects JSON, config, histograms, graphs, caches (notice),
/// or the statistics leaf.
pub fn statistics_response(inputs: StatisticsInputs) -> ResourceResponse {
    use ps_admin::site::AdminSource;
    let site = admin_site_for(inputs.log, inputs.now_ms);
    let mut query = QueryParams::new();
    if let Some(index) = inputs.url.find('?') {
        query.parse_from_untrusted_string(&inputs.url[index + 1..]);
    }
    let mut fetch = BufferingFetch::new(inputs.url, RequestContext::new(HttpOptions::default()));
    fetch
        .response_headers_mut()
        .set_date_and_caching(inputs.now_ms, 0, ", no-cache");
    if query.has("json") {
        site.console_json_handler(&query, &mut fetch, inputs.statistics);
    } else if query.has("config") {
        site.print_config(AdminSource::Statistics, &mut fetch, inputs.options);
    } else if query.has("histograms") {
        site.print_histograms(AdminSource::Statistics, &mut fetch, inputs.statistics);
    } else if query.has("graphs") {
        site.graphs_handler(
            inputs.options,
            AdminSource::Statistics,
            &query,
            &mut fetch,
            inputs.statistics,
        );
    } else if query.has("cache") {
        let config = admin_config_for(inputs.system);
        let mut backend = PurgeSetBackend::new(PurgeSet::new(25_000));
        let display_set = PurgeSet::new(25_000);
        let stripped_url = GoogleUrl::new(inputs.url);
        let mut request = AdminRequest {
            options: inputs.options,
            global_options: &config,
            statistics: inputs.statistics,
            stats: inputs.statistics,
            inventory: None,
            show_cache: None,
            purge: &mut backend,
            purge_set: &display_set,
        };
        site.print_caches(
            AdminSource::Statistics,
            inputs.is_global,
            &stripped_url,
            &query,
            inputs.options,
            &mut fetch,
            &mut request,
        );
    } else {
        site.statistics_handler(
            inputs.options,
            AdminSource::Statistics,
            &mut fetch,
            inputs.statistics,
        );
    }
    let _ = inputs.is_global;
    collect_admin(&fetch)
}

/// Renders the console (`ConsoleHandler`).
pub fn console_response(inputs: StatisticsInputs) -> ResourceResponse {
    use ps_admin::site::AdminSource;
    let site = admin_site_for(inputs.log, inputs.now_ms);
    let config = admin_config_for(inputs.system);
    let mut query = QueryParams::new();
    if let Some(index) = inputs.url.find('?') {
        query.parse_from_untrusted_string(&inputs.url[index + 1..]);
    }
    let mut fetch = BufferingFetch::new(inputs.url, RequestContext::new(HttpOptions::default()));
    fetch
        .response_headers_mut()
        .set_date_and_caching(inputs.now_ms, 0, ", no-cache");
    site.console_handler(
        &config,
        inputs.options,
        AdminSource::Statistics,
        &query,
        &mut fetch,
        inputs.statistics,
    );
    collect_admin(&fetch)
}

/// Purge inputs.
pub struct PurgeInputs<'a> {
    /// Shared message buffer.
    pub log: &'a Arc<Mutex<ps_admin::log_buffer::LogBuffer>>,
    /// Shared purge set (locked by the caller).
    pub purge_set: &'a mut PurgeSet,
    /// Purged URL (or `*`-suffixed for the whole cache).
    pub url: &'a str,
    /// Request time.
    pub now_ms: i64,
}

/// Purges a URL (`PurgeHandler`, route rights: eviction
/// allowed): the record lands in the shared set (fetch-flow
/// validity is a follow-up).
pub fn purge_response(inputs: PurgeInputs) -> ResourceResponse {
    let site = admin_site_for(inputs.log, inputs.now_ms);
    let mut backend = PurgeSetBackend::new(std::mem::replace(
        &mut *inputs.purge_set,
        PurgeSet::new(25_000),
    ));
    let mut fetch = BufferingFetch::new(inputs.url, RequestContext::new(HttpOptions::default()));
    fetch
        .response_headers_mut()
        .set_date_and_caching(inputs.now_ms, 0, ", no-cache");
    site.purge_handler(inputs.url, true, &mut fetch, &mut backend);
    *inputs.purge_set = backend.purge_set;
    collect_admin(&fetch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rewrite::html_context;
    use ps_cache::cache::{Cache, MemoryCache};
    use std::sync::Arc;

    #[test]
    fn simple_asset_outcomes() {
        match simple_asset("deterministic.deadbeef00.js") {
            SimpleOutcome::Serve(response) => {
                assert_eq!(response.status, 200);
                assert_eq!(response.content_type, "application/javascript");
                assert!(!response.body.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(simple_asset("bogus.deadbeef00.js"), SimpleOutcome::Decline);
        assert_eq!(
            simple_asset("bogus.js"),
            SimpleOutcome::LogDecline("Invalid url requested: bogus.js.".to_string())
        );
    }

    #[test]
    fn simple_messages_wraps_or_warns() {
        let response = simple_messages(Some("line1\nline2"));
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "text/html");
        assert_eq!(response.body, b"<pre>line1\nline2</pre>\n");
        let response = simple_messages(Some("<b>"));
        assert_eq!(response.body, b"<pre>&lt;b&gt;</pre>\n");
        let response = simple_messages(None);
        assert_eq!(response.body, MESSAGES_DISABLED_BODY.as_bytes());
    }

    #[test]
    fn resource_garbage_yields_empty() {
        let backend: Arc<dyn Cache> = Arc::new(MemoryCache::new());
        let metadata: Arc<dyn Cache> = Arc::new(MemoryCache::new());
        let html = html_context(
            backend,
            metadata,
            Arc::new(ps_stats::core::Statistics::new()),
            0,
            std::rc::Rc::new(crate::rewrite::UnavailableFetcher),
        );
        let options = ps_config::options::RewriteOptions::new();
        let response = fetch_resource_body(&html, &options, "http://h/not-a-resource", 0);
        assert_eq!(response.status, 0);
        assert!(response.body.is_empty());
    }

    #[test]
    fn statistics_console_and_purge_render() {
        use ps_admin::log_buffer::LogBuffer;
        let options = ps_config::options::RewriteOptions::new();
        let system = ps_config::options::NginxSystemOptions::default();
        let statistics = ps_stats::core::Statistics::new();
        let log = Arc::new(Mutex::new(LogBuffer::new(1000)));
        fn inputs<'a>(
            options: &'a ps_config::options::RewriteOptions,
            system: &'a ps_config::options::NginxSystemOptions,
            statistics: &'a ps_stats::core::Statistics,
            log: &'a Arc<Mutex<ps_admin::log_buffer::LogBuffer>>,
            url: &'a str,
        ) -> StatisticsInputs<'a> {
            StatisticsInputs {
                options,
                system,
                statistics,
                log,
                url,
                is_global: false,
                now_ms: 1_700_000_000_000,
            }
        }
        let response = statistics_response(inputs(
            &options,
            &system,
            &statistics,
            &log,
            "http://h/pagespeed_statistics",
        ));
        assert_eq!(response.status, 200);
        assert!(!response.body.is_empty());
        let response = statistics_response(inputs(
            &options,
            &system,
            &statistics,
            &log,
            "http://h/pagespeed_statistics?histograms",
        ));
        assert_eq!(response.status, 200);
        assert!(!response.body.is_empty());
        let response = console_response(inputs(
            &options,
            &system,
            &statistics,
            &log,
            "http://h/pagespeed_console",
        ));
        assert_eq!(response.status, 200);
        assert!(!response.body.is_empty());
        let mut purge_set = PurgeSet::new(25_000);
        let response = purge_response(PurgeInputs {
            log: &log,
            purge_set: &mut purge_set,
            url: "http://h/a",
            now_ms: 1_700_000_000_000,
        });
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"Purge successful");
    }

    #[test]
    fn admin_pages_render() {
        use ps_admin::log_buffer::LogBuffer;
        let options = ps_config::options::RewriteOptions::new();
        let system = ps_config::options::NginxSystemOptions::default();
        let statistics = ps_stats::core::Statistics::new();
        let log = Arc::new(Mutex::new(LogBuffer::new(1000)));
        let mut purge_set = PurgeSet::new(25_000);
        let inventory = CacheInventory {
            http_cache_name: "HTTPCache(FileCache)".to_string(),
            metadata_cache_name: "Compressed(FileCache)".to_string(),
            property_store_name: "disabled".to_string(),
            filesystem_metadata_cache_name: None,
            backend_stats: None,
        };
        // Bare path redirects for relative links.
        let response = admin_response(AdminInputs {
            options: &options,
            system: &system,
            statistics: &statistics,
            log: &log,
            purge_set: &mut purge_set,
            inventory,
            url: "http://h/pagespeed_admin",
            is_global: true,
            now_ms: 0,
        });
        assert_eq!(response.status, 301);
        // Statistics leaf renders from the worker registry.
        let inventory = CacheInventory {
            http_cache_name: String::new(),
            metadata_cache_name: String::new(),
            property_store_name: String::new(),
            filesystem_metadata_cache_name: None,
            backend_stats: None,
        };
        let response = admin_response(AdminInputs {
            options: &options,
            system: &system,
            statistics: &statistics,
            log: &log,
            purge_set: &mut purge_set,
            inventory,
            url: "http://h/pagespeed_admin/statistics",
            is_global: true,
            now_ms: 0,
        });
        assert_eq!(response.status, 200);
        assert!(!response.body.is_empty());
    }
}
