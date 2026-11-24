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
use ps_http::fetch::{AsyncFetch, AsyncFetcher, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::query_params::QueryParams;
use ps_http::request::{Method, RequestContext, RequestHeaders};
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
    /// Image rewrite records for the admin images page (the
    /// caller messages each line into the message buffer).
    pub image_records: Vec<ps_html::server_context::ImageRewriteRecord>,
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
    request_headers: &ps_http::request::RequestHeaders,
) -> ResourceResponse {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::rewrite::reset_context_clock(html, now_ms);
    let mut fetch = BufferingFetch::new(url, RequestContext::new(HttpOptions::default()));
    *fetch.request_headers_mut() = request_headers.clone();
    let driver = Rc::new(ps_html::server_context::ResourceDriver::new(
        Rc::clone(html),
        Rc::new(RefCell::new(options.clone())),
        url,
    ));
    install_request_context(&driver, request_headers);
    // This is called directly by the NGINX content handler. Never sleep
    // on its event loop waiting for a background resource owner.
    driver.set_fetch_dedup_wait(std::time::Duration::ZERO);
    driver.register_serving_filters();
    ps_html::server_context::ResourceDriver::fetch_resource(&driver, url, &mut fetch);
    let mut response = collect_fetch(&fetch);
    response.image_records = driver.take_image_records();
    response
}

/// Installs request headers + UA on a driver (`SetRequestHeaders`
/// shape): capabilities (webp/avif/mobile) and UA cache keys
/// derive from the live request.
pub fn install_request_context(
    driver: &std::rc::Rc<ps_html::server_context::ResourceDriver>,
    request_headers: &ps_http::request::RequestHeaders,
) {
    driver.set_request_headers(request_headers.clone());
    if let Some(user_agent) = request_headers
        .headers()
        .lookup1(ps_http::names::header::USER_AGENT)
    {
        driver.set_user_agent(&user_agent);
    }
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
    request_headers: &ps_http::request::RequestHeaders,
) -> ResourceResponse {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::rewrite::reset_context_clock(html, now_ms);
    let mut fetch = BufferingFetch::new(url, RequestContext::new(HttpOptions::default()));
    *fetch.request_headers_mut() = request_headers.clone();
    let driver = Rc::new(ps_html::server_context::ResourceDriver::new(
        Rc::clone(html),
        Rc::new(RefCell::new(options.clone())),
        url,
    ));
    install_request_context(&driver, request_headers);
    driver.register_serving_filters();
    ps_html::ipro::fetch_in_place_resource(&driver, url, &mut fetch, false);
    let mut response = collect_fetch(&fetch);
    response.image_records = driver.take_image_records();
    response
}

/// Builds a lawyer over the proxy mappings (`domain_lawyer()`
/// reads live options in C++; the sync bridge snapshots per
/// request, like `ResourceDriver::rebuild_lawyer`).
#[must_use]
pub fn proxy_lawyer(
    options: &ps_config::options::RewriteOptions,
) -> ps_html::domain_lawyer::DomainLawyer {
    use ps_config::options::DomainMappingKind;
    let mut lawyer = ps_html::domain_lawyer::DomainLawyer::new();
    for mapping in options.domain_mappings() {
        if mapping.kind == DomainMappingKind::Proxy {
            // The third `MapProxyDomain` arg re-bases fetches onto
            // a CDN (C++ `AddProxyDomainMapping` third leg);
            // two-arg forms carry an empty target.
            lawyer.add_proxy_domain_mapping(&mapping.from, &mapping.to, &mapping.host_header, None);
        }
    }
    lawyer
}

/// Serves a proxy-mapped URL through the sync bridge (the nginx
/// proxy branch, `StartNewProxyFetch` with `is_proxy_mapped`):
/// maps the request URL onto its origin, fetches it with the
/// client's headers forwarded transparently (the `Host` stays
/// the proxy's — C++ computes the mapped host header but never
/// sends it), and collects the origin's response verbatim.
/// `HEAD` runs as the `GET` it answers (nginx drops the body
/// itself, and the fetched length answers correctly); transport
/// failures yield status 0 (decline), while origin error
/// statuses serve as-is.
pub fn fetch_proxy_body(
    options: &ps_config::options::RewriteOptions,
    timeout_ms: i64,
    url: &str,
    request_headers: &RequestHeaders,
) -> ResourceResponse {
    let failed = ResourceResponse {
        status: 0,
        headers: Vec::new(),
        body: Vec::new(),
        image_records: Vec::new(),
    };
    let lawyer = proxy_lawyer(options);
    let gurl = GoogleUrl::new(url);
    let Some((mapped, _, is_proxy)) = lawyer.map_origin_url(&gurl) else {
        return failed;
    };
    if !is_proxy {
        return failed;
    }
    let mut outgoing = request_headers.clone();
    outgoing.set_method(Method::Get);
    let mut fetch = BufferingFetch::new(&mapped, RequestContext::new(HttpOptions::default()));
    *fetch.request_headers_mut() = outgoing;
    let fetcher = crate::origin_fetch::OriginFetcher::new(
        timeout_ms,
        crate::origin_fetch::DEFAULT_MAX_BODY_BYTES,
    );
    fetcher.fetch(&mapped, &mut fetch);
    if !fetch.done() || !fetch.success() {
        return failed;
    }
    collect_proxy(&fetch)
}

/// Collects a proxied fetch, dropping hop-by-hop response
/// headers (the fetcher already renormalized framing; emitting
/// the origin's connection tokens would corrupt ours) and the
/// internal size-accounting header (inputs keep it; the wire
/// must not).
fn collect_proxy(fetch: &BufferingFetch) -> ResourceResponse {
    use ps_http::names::header;
    let headers = fetch
        .response_headers()
        .headers()
        .iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case(header::CONNECTION)
                && !name.eq_ignore_ascii_case(header::KEEP_ALIVE)
                && !name.eq_ignore_ascii_case(header::PROXY_AUTHENTICATE)
                && !name.eq_ignore_ascii_case(header::TRANSFER_ENCODING)
                && !name.eq_ignore_ascii_case(header::UPGRADE)
                && !name.eq_ignore_ascii_case("Trailer")
                && !name.eq_ignore_ascii_case(header::TRAILERS)
                && !name.eq_ignore_ascii_case(header::X_ORIGINAL_CONTENT_LENGTH)
        })
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    ResourceResponse {
        status: fetch.response_headers().status_code().max(0) as u16,
        headers,
        body: fetch.body().to_vec(),
        image_records: Vec::new(),
    }
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
        image_records: Vec::new(),
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
        statistics_logging_enabled: system.statistics_logging_enabled,
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
    use crate::origin_fetch::test_support::{get_headers, Stub};
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
            std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
        );
        let options = ps_config::options::RewriteOptions::new();
        let request_headers = ps_http::request::RequestHeaders::new();
        let response = fetch_resource_body(
            &html,
            &options,
            "http://h/not-a-resource",
            0,
            &request_headers,
        );
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

    /// Options proxying `proxy.test` at the stub origin.
    fn proxy_options_for(stub: &Stub) -> ps_config::options::RewriteOptions {
        let mut options = ps_config::options::RewriteOptions::new();
        let origin = stub.url("/").trim_end_matches('/').to_string();
        options.add_proxy_domain_mapping("http://proxy.test/", &origin);
        options
    }

    /// Client-style headers (`Host` set, like nginx delivers).
    fn proxy_request_headers() -> RequestHeaders {
        let mut headers = get_headers();
        headers
            .headers_mut()
            .add(ps_http::names::header::HOST, "proxy.test");
        headers
    }

    #[test]
    fn proxy_serves_mapped_origin_verbatim() {
        let stub = Stub::once(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 7\r\nX-Origin: yes\r\n\r\n<html>\n",
        );
        let options = proxy_options_for(&stub);
        let lawyer = proxy_lawyer(&options);
        assert!(lawyer.is_proxy_mapped(&GoogleUrl::new("http://proxy.test/page")));
        assert!(!lawyer.is_proxy_mapped(&GoogleUrl::new("http://other.test/page")));
        let response = fetch_proxy_body(
            &options,
            5000,
            "http://proxy.test/page",
            &proxy_request_headers(),
        );
        assert_eq!(response.status, 200);
        assert_eq!(b"<html>\n", response.body.as_slice());
        assert!(response
            .headers
            .iter()
            .any(|(name, value)| name == "X-Origin" && value == "yes"));
        assert!(response.headers.iter().all(|(name, _)| {
            !name.eq_ignore_ascii_case("x-original-content-length")
        }));
        // Transparent proxying: the origin sees the proxy's Host.
        let request = stub.request_text();
        assert!(request.starts_with("GET /page HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("Host: proxy.test\r\n"), "{request}");
    }

    #[test]
    fn proxy_head_runs_as_get() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndata");
        let options = proxy_options_for(&stub);
        let mut headers = proxy_request_headers();
        headers.set_method(Method::Head);
        let response = fetch_proxy_body(&options, 5000, "http://proxy.test/f", &headers);
        assert_eq!(response.status, 200);
        assert_eq!(b"data", response.body.as_slice());
        let request = stub.request_text();
        assert!(request.starts_with("GET /f HTTP/1.1\r\n"), "{request}");
    }

    #[test]
    fn proxy_serves_origin_errors_as_is() {
        let stub = Stub::once(b"HTTP/1.1 404 Gone\r\nContent-Length: 7\r\n\r\nmissing");
        let options = proxy_options_for(&stub);
        let response = fetch_proxy_body(
            &options,
            5000,
            "http://proxy.test/nope",
            &proxy_request_headers(),
        );
        assert_eq!(response.status, 404);
        assert_eq!(b"missing", response.body.as_slice());
    }

    #[test]
    fn proxy_strips_hop_by_hop_response_headers() {
        let stub = Stub::once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nKeep-Alive: timeout=5\r\nUpgrade: h2c\r\nX-Keep: 1\r\n\r\nok",
        );
        let options = proxy_options_for(&stub);
        let response = fetch_proxy_body(
            &options,
            5000,
            "http://proxy.test/x",
            &proxy_request_headers(),
        );
        assert_eq!(response.status, 200);
        for (name, _) in &response.headers {
            assert!(
                !name.eq_ignore_ascii_case("connection")
                    && !name.eq_ignore_ascii_case("keep-alive")
                    && !name.eq_ignore_ascii_case("upgrade"),
                "hop-by-hop leaked: {name}"
            );
        }
        assert!(response
            .headers
            .iter()
            .any(|(name, value)| name == "X-Keep" && value == "1"));
    }

    #[test]
    fn proxy_unmapped_url_fails_to_decline() {
        let stub = Stub::once(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let options = proxy_options_for(&stub);
        let response = fetch_proxy_body(
            &options,
            5000,
            "http://other.test/page",
            &proxy_request_headers(),
        );
        assert_eq!(response.status, 0);
        assert!(response.body.is_empty());
    }

    #[test]
    fn admin_config_maps_logging_switch() {
        let mut system = ps_config::options::NginxSystemOptions::default();
        assert!(!admin_config_for(&system).statistics_logging_enabled);
        system.statistics_logging_enabled = true;
        system.log_dir = "/tmp/logs".to_string();
        let config = admin_config_for(&system);
        assert!(config.statistics_logging_enabled);
        assert_eq!("/tmp/logs", config.log_dir);
    }

    #[test]
    fn proxy_dead_origin_fails_to_decline() {
        let mut options = ps_config::options::RewriteOptions::new();
        // Reserved TEST-NET-1: unroutable, so the fetch fails fast.
        options.add_proxy_domain_mapping("http://proxy.test/", "http://192.0.2.1/");
        let lawyer = proxy_lawyer(&options);
        assert!(lawyer.is_proxy_mapped(&GoogleUrl::new("http://proxy.test/page")));
        let response = fetch_proxy_body(
            &options,
            1000,
            "http://proxy.test/page",
            &proxy_request_headers(),
        );
        assert_eq!(response.status, 0);
    }
}
