//! Request routing and content dispatch (`ps_route_request`, `ps_content_handler`).
//!
//! Ports the sync-decide contract from `ngx_pagespeed.cc`: request-URL
//! construction (`ps_determine_url`/`ps_determine_port` past header
//! extraction), the subrequest check, the 15-way route decision, the
//! content-handler gates (disabled server, uninitialized context),
//! and the route-to-handler dispatch. Terminal handlers (beacon,
//! simple, resource) are later slices: dispatch records their
//! selection and the FFI wrapper declines until they land.

use crate::conf::NgxOptions;
use ps_http::google_url::GoogleUrl;

/// Subrequest marker (`kModPagespeedSubrequestUserAgent`): internal
/// fetches carry it in their User-Agent.
pub const SUBREQUEST_USER_AGENT: &str = "mod_pagespeed";

/// Default static-asset prefix (`SystemRewriteDriverFactory`;
/// settable through the factory directive, Slice 6).
pub const STATIC_ASSET_PREFIX: &str = "/pagespeed_static/";

/// Default beacon endpoint (`NgxRewriteDriverFactory` ctor).
pub const NGINX_BEACON_URL: &str = "/ngx_pagespeed_beacon";

/// No port specified (`ps_determine_port` sentinel).
pub const NO_PORT: i32 = -1;

/// Route decision (`RequestRouting::Response`), same order as C++.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Worker is shutting down (`kError`).
    Error,
    /// Static-asset path (`kStaticContent`).
    StaticContent,
    /// URL would not parse (`kInvalidUrl`).
    InvalidUrl,
    /// Pagespeed off for this server (`kPagespeedDisabled`).
    PagespeedDisabled,
    /// Beacon endpoint (`kBeacon`).
    Beacon,
    /// Statistics path (`kStatistics`).
    Statistics,
    /// Global statistics path (`kGlobalStatistics`).
    GlobalStatistics,
    /// Console path (`kConsole`).
    Console,
    /// Message-history path (`kMessages`).
    Messages,
    /// Admin path (`kAdmin`).
    Admin,
    /// Purge method (`kCachePurge`).
    CachePurge,
    /// Global admin path (`kGlobalAdmin`).
    GlobalAdmin,
    /// Internal subrequest (`kPagespeedSubrequest`).
    PagespeedSubrequest,
    /// Request already carries an error (`kErrorResponse`).
    ErrorResponse,
    /// Anything else: resource/HTML path (`kResource`).
    Resource,
}

/// Whether a request is an internal subrequest
/// (`is_pagespeed_subrequest`): the User-Agent merely contains
/// the marker.
#[must_use]
pub fn is_pagespeed_subrequest(user_agent: Option<&str>) -> bool {
    user_agent.is_some_and(|ua| ua.contains(SUBREQUEST_USER_AGENT))
}

/// Extracts the port from a Host header value (`ps_determine_port`
/// header half): the text after the final colon, when it parses
/// as an integer (`StringToInt`: optional sign, all digits, no
/// whitespace, range-checked — exactly `str::parse::<i32>`).
/// Missing headers, colon-less values, IPv6 brackets, and
/// malformed ports all read as unspecified.
#[must_use]
pub fn parse_host_port(host: Option<&str>) -> i32 {
    let Some(host) = host else {
        return NO_PORT;
    };
    let Some(colon) = host.rfind(':') else {
        return NO_PORT;
    };
    host[colon + 1..].parse::<i32>().unwrap_or(NO_PORT)
}

/// Builds the request URL (`ps_determine_url`) from extracted
/// parts: default ports (and unspecified ones) carry no `:port`
/// suffix.
#[must_use]
pub fn build_url(https: bool, port: i32, host: &str, unparsed_uri: &str) -> String {
    let port_string = if (https && (port == 443 || port == NO_PORT))
        || (!https && (port == 80 || port == NO_PORT))
    {
        String::new()
    } else {
        format!(":{port}")
    };
    format!(
        "{}://{host}{port_string}{unparsed_uri}",
        if https { "https" } else { "http" }
    )
}

/// Case-insensitive prefix match (`StringCaseStartsWith`),
/// byte-safe for non-ASCII text past the prefix.
fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// Endpoint families for the access check (`AllowDomain`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Statistics,
    GlobalStatistics,
    Messages,
    Console,
    Admin,
    GlobalAdmin,
}

/// Whether a host may use an endpoint (`AllowDomain`): empty hosts
/// never pass, empty groups allow all, otherwise the host must
/// match (default deny).
fn access_allowed(options: &NgxOptions, endpoint: Endpoint, host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    let group = match endpoint {
        Endpoint::Statistics => &options.system.statistics_domains,
        Endpoint::GlobalStatistics => &options.system.global_statistics_domains,
        Endpoint::Messages => &options.system.messages_domains,
        Endpoint::Console => &options.system.console_domains,
        Endpoint::Admin => &options.system.admin_domains,
        Endpoint::GlobalAdmin => &options.system.global_admin_domains,
    };
    if group.is_empty() {
        return true;
    }
    group.is_match(host, false)
}

/// Inputs to the route decision (`ps_route_request` past extraction).
pub struct RoutingInputs<'a> {
    /// Server disabled (`ps_disabled`).
    pub disabled: bool,
    /// Worker is shutting down (`ngx_terminate || ngx_exiting`).
    pub shutting_down: bool,
    /// Request already carries an error (`r->err_status != 0`).
    pub err_status: bool,
    /// Built request URL (`ps_determine_url`).
    pub url_text: &'a str,
    /// Internal subrequest (`is_pagespeed_subrequest`).
    pub subrequest: bool,
    /// Static-asset prefix (factory-owned; defaults to
    /// `STATIC_ASSET_PREFIX` until Slice 6 wires it).
    pub static_asset_prefix: &'a str,
    /// Server global options (`config()`).
    pub options: &'a NgxOptions,
    /// HTTPS request (`ps_is_https`).
    pub https: bool,
    /// Request method name (`r->method_name`).
    pub method: &'a str,
}

/// Routes a request (`ps_route_request`), branch for branch.
#[must_use]
pub fn route_request(inputs: &RoutingInputs) -> Route {
    if inputs.disabled {
        return Route::PagespeedDisabled;
    }
    if inputs.shutting_down {
        return Route::Error;
    }
    if inputs.err_status {
        return Route::ErrorResponse;
    }
    let url = GoogleUrl::new(inputs.url_text);
    if !url.is_web_valid() {
        return Route::InvalidUrl;
    }
    if inputs.subrequest {
        return Route::PagespeedSubrequest;
    }
    if url.path_sans_leaf() == inputs.static_asset_prefix {
        return Route::StaticContent;
    }
    let system = &inputs.options.system;
    let path = url.path_sans_query();
    if path.eq_ignore_ascii_case(&system.statistics_path)
        && access_allowed(inputs.options, Endpoint::Statistics, &url.host())
    {
        return Route::Statistics;
    }
    if path.eq_ignore_ascii_case(&system.global_statistics_path)
        && access_allowed(inputs.options, Endpoint::GlobalStatistics, &url.host())
    {
        return Route::GlobalStatistics;
    }
    if path.eq_ignore_ascii_case(&system.console_path)
        && access_allowed(inputs.options, Endpoint::Console, &url.host())
    {
        return Route::Console;
    }
    if path.eq_ignore_ascii_case(&system.messages_path)
        && access_allowed(inputs.options, Endpoint::Messages, &url.host())
    {
        return Route::Messages;
    }
    // The admin handlers get everything under a path (`/path/*`)
    // while the rest take exact matches (`/path`).
    if !system.admin_path.is_empty()
        && starts_with_ignore_case(path, &system.admin_path)
        && access_allowed(inputs.options, Endpoint::Admin, &url.host())
    {
        return Route::Admin;
    }
    if !system.global_admin_path.is_empty()
        && starts_with_ignore_case(path, &system.global_admin_path)
        && access_allowed(inputs.options, Endpoint::GlobalAdmin, &url.host())
    {
        return Route::GlobalAdmin;
    }
    if inputs.options.core.enable_cache_purge()
        && !system.purge_method.is_empty()
        && system.purge_method == inputs.method
    {
        return Route::CachePurge;
    }
    let beacon = inputs.options.core.beacon_url();
    let endpoint = match beacon.as_ref() {
        Some(beacon) if inputs.https => beacon.https.as_str(),
        Some(beacon) => beacon.http.as_str(),
        None if inputs.https => NGINX_BEACON_URL,
        None => NGINX_BEACON_URL,
    };
    if path == endpoint {
        return Route::Beacon;
    }
    Route::Resource
}

/// Content-handler selection (`ps_content_handler` switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAction {
    /// Worker is shutting down (`NGX_ERROR`).
    Error,
    /// Not ours (`NGX_DECLINED`).
    Decline,
    /// Beacon handler (later slice; declines until it lands).
    Beacon,
    /// Static-asset/message handler (later slice; declines until
    /// it lands).
    Simple,
    /// Resource/admin handler (later slice; declines until it
    /// lands).
    Resource,
}

/// Selects the handler for a route (`ps_content_handler` past gating).
#[must_use]
pub const fn content_action(route: Route) -> ContentAction {
    match route {
        Route::Error => ContentAction::Error,
        Route::PagespeedDisabled
        | Route::InvalidUrl
        | Route::PagespeedSubrequest
        | Route::ErrorResponse => ContentAction::Decline,
        Route::Beacon => ContentAction::Beacon,
        Route::StaticContent | Route::Messages => ContentAction::Simple,
        Route::Statistics
        | Route::GlobalStatistics
        | Route::Console
        | Route::Admin
        | Route::GlobalAdmin
        | Route::CachePurge
        | Route::Resource => ContentAction::Resource,
    }
}

/// Full content decision: disabled or uninitialized servers decline
/// before routing (the flush poll runs between gating and routing
/// in the FFI wrapper); otherwise the route selects the handler.
#[must_use]
pub fn content_decision(disabled: bool, initialized: bool, route: Route) -> ContentAction {
    if disabled || !initialized {
        return ContentAction::Decline;
    }
    content_action(route)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::NgxOptions;
    use ps_config::scope::Scope;

    fn inputs<'a>(options: &'a NgxOptions, url_text: &'a str) -> RoutingInputs<'a> {
        RoutingInputs {
            disabled: false,
            shutting_down: false,
            err_status: false,
            url_text,
            subrequest: false,
            static_asset_prefix: STATIC_ASSET_PREFIX,
            options,
            https: false,
            method: "GET",
        }
    }

    #[test]
    fn subrequest_token_is_substring() {
        assert!(!is_pagespeed_subrequest(None));
        assert!(!is_pagespeed_subrequest(Some("")));
        assert!(!is_pagespeed_subrequest(Some("Mozilla/5.0")));
        assert!(is_pagespeed_subrequest(Some("mod_pagespeed")));
        // The serf fetcher embeds it mid-token.
        assert!(is_pagespeed_subrequest(Some(
            "ngx-pagespeed (mod_pagespeed/1.0)"
        )));
    }

    #[test]
    fn host_port_corners() {
        assert_eq!(NO_PORT, parse_host_port(None));
        assert_eq!(NO_PORT, parse_host_port(Some("www.example.com")));
        assert_eq!(8080, parse_host_port(Some("www.example.com:8080")));
        assert_eq!(8080, parse_host_port(Some("127.0.0.1:8080")));
        // IPv6: brackets defeat the final-colon rule.
        assert_eq!(NO_PORT, parse_host_port(Some("[::1]")));
        assert_eq!(8080, parse_host_port(Some("[::1]:8080")));
        // Malformed tails read as unspecified.
        assert_eq!(NO_PORT, parse_host_port(Some("example.com:")));
        assert_eq!(NO_PORT, parse_host_port(Some("example.com:http")));
        assert_eq!(NO_PORT, parse_host_port(Some("example.com:80 ")));
        assert_eq!(NO_PORT, parse_host_port(Some("example.com:99999999999")));
        // Sign-prefixed integers parse, like `StringToInt`.
        assert_eq!(80, parse_host_port(Some("example.com:+80")));
    }

    #[test]
    fn url_build_ports() {
        assert_eq!("http://h/a?b", build_url(false, NO_PORT, "h", "/a?b"));
        assert_eq!("http://h/a", build_url(false, 80, "h", "/a"));
        assert_eq!("http://h:8080/a", build_url(false, 8080, "h", "/a"));
        assert_eq!("https://h/a", build_url(true, NO_PORT, "h", "/a"));
        assert_eq!("https://h/a", build_url(true, 443, "h", "/a"));
        assert_eq!("https://h:8443/a", build_url(true, 8443, "h", "/a"));
        // Cross-scheme defaults keep their suffix.
        assert_eq!("http://h:443/a", build_url(false, 443, "h", "/a"));
        assert_eq!("https://h:80/a", build_url(true, 80, "h", "/a"));
    }

    #[test]
    fn route_early_branches() {
        let options = NgxOptions::new();
        let mut early = inputs(&options, "http://h/a.css");
        early.disabled = true;
        assert_eq!(Route::PagespeedDisabled, route_request(&early));
        early.disabled = false;
        early.shutting_down = true;
        assert_eq!(Route::Error, route_request(&early));
        early.shutting_down = false;
        early.err_status = true;
        assert_eq!(Route::ErrorResponse, route_request(&early));
        early.err_status = false;
        early.url_text = "not a url";
        assert_eq!(Route::InvalidUrl, route_request(&early));
        early.url_text = "http://h/a.css";
        early.subrequest = true;
        assert_eq!(Route::PagespeedSubrequest, route_request(&early));
        // Subrequests win over the static prefix (order matters).
        early.url_text = "http://h/pagespeed_static/x.js";
        assert_eq!(Route::PagespeedSubrequest, route_request(&early));
        early.subrequest = false;
        assert_eq!(Route::StaticContent, route_request(&early));
    }

    #[test]
    fn route_admin_matrix() {
        let mut options = NgxOptions::new();
        options.system.statistics_path = "/ngx_pagespeed_statistics".to_string();
        options.system.global_statistics_path = "/ngx_pagespeed_global_statistics".to_string();
        options.system.console_path = "/pagespeed_console".to_string();
        options.system.messages_path = "/ngx_pagespeed_message".to_string();
        options.system.admin_path = "/pagespeed_admin".to_string();
        options.system.global_admin_path = "/pagespeed_global_admin".to_string();
        for (path, route) in [
            ("/ngx_pagespeed_statistics", Route::Statistics),
            ("/ngx_pagespeed_global_statistics", Route::GlobalStatistics),
            ("/pagespeed_console", Route::Console),
            ("/ngx_pagespeed_message", Route::Messages),
            ("/pagespeed_admin", Route::Admin),
            ("/pagespeed_admin/cache_flush", Route::Admin),
            ("/pagespeed_global_admin", Route::GlobalAdmin),
            ("/pagespeed_global_admin/x", Route::GlobalAdmin),
        ] {
            let url = format!("http://h{path}");
            assert_eq!(route, route_request(&inputs(&options, &url)), "{path}");
        }
        // Exact paths are case-insensitive; query strings ignored.
        assert_eq!(
            Route::Statistics,
            route_request(&inputs(&options, "http://h/NGX_PAGESPEED_STATISTICS?x=1"))
        );
        assert_eq!(
            Route::Admin,
            route_request(&inputs(&options, "http://h/Pagespeed_Admin/x"))
        );
        // Non-admin paths take exact matches only.
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&options, "http://h/ngx_pagespeed_statistics/x"))
        );
        // Empty admin paths never match (the guard matters: every path
        // trivially starts with "").
        let bare = NgxOptions::new();
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&bare, "http://h/pagespeed_admin"))
        );
    }

    #[test]
    fn route_access_gates() {
        let mut options = NgxOptions::new();
        options.system.statistics_path = "/s".to_string();
        options.system.statistics_domains.allow("*.example.com");
        options
            .system
            .statistics_domains
            .disallow("evil.example.com");
        assert_eq!(
            Route::Statistics,
            route_request(&inputs(&options, "http://www.example.com/s"))
        );
        // Denied hosts fall through to the resource path.
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&options, "http://evil.example.com/s"))
        );
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&options, "http://other.org/s"))
        );
    }

    #[test]
    fn route_purge_and_beacon() {
        let mut options = NgxOptions::new();
        options
            .core
            .set_option("EnableCachePurge", "true", Scope::ProcessStrict)
            .unwrap();
        options.system.purge_method = "PURGE".to_string();
        let mut purge = inputs(&options, "http://h/a.css");
        purge.method = "PURGE";
        assert_eq!(Route::CachePurge, route_request(&purge));
        purge.method = "GET";
        assert_eq!(Route::Resource, route_request(&purge));
        // Empty method or disabled flag: no purge route.
        options.system.purge_method.clear();
        let plain = inputs(&options, "http://h/a.css");
        assert_eq!(Route::Resource, route_request(&plain));
        // Default beacon endpoint, per scheme (exact, case-sensitive).
        assert_eq!(
            Route::Beacon,
            route_request(&inputs(&options, "http://h/ngx_pagespeed_beacon"))
        );
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&options, "http://h/NGX_PAGESPEED_BEACON"))
        );
        let mut https = inputs(&options, "https://h/ngx_pagespeed_beacon");
        https.https = true;
        assert_eq!(Route::Beacon, route_request(&https));
        // Configured beacon pair.
        options
            .core
            .set_option("BeaconUrl", "http://a/b https://c/d", Scope::ProcessStrict)
            .unwrap();
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&options, "http://h/ngx_pagespeed_beacon"))
        );
        // The comparison is path-vs-configured-endpoint, verbatim: an
        // absolute configured URL never equals a path.
        assert_eq!(
            Route::Resource,
            route_request(&inputs(&options, "http://a/b"))
        );
    }

    #[test]
    fn dispatch_table() {
        // Gates decline before routing.
        assert_eq!(
            ContentAction::Decline,
            content_decision(true, true, Route::Beacon)
        );
        assert_eq!(
            ContentAction::Decline,
            content_decision(false, false, Route::Beacon)
        );
        assert_eq!(
            ContentAction::Decline,
            content_decision(true, false, Route::Error)
        );
        // The switch, all fifteen routes.
        for (route, action) in [
            (Route::Error, ContentAction::Error),
            (Route::PagespeedDisabled, ContentAction::Decline),
            (Route::InvalidUrl, ContentAction::Decline),
            (Route::PagespeedSubrequest, ContentAction::Decline),
            (Route::ErrorResponse, ContentAction::Decline),
            (Route::Beacon, ContentAction::Beacon),
            (Route::StaticContent, ContentAction::Simple),
            (Route::Messages, ContentAction::Simple),
            (Route::Statistics, ContentAction::Resource),
            (Route::GlobalStatistics, ContentAction::Resource),
            (Route::Console, ContentAction::Resource),
            (Route::Admin, ContentAction::Resource),
            (Route::GlobalAdmin, ContentAction::Resource),
            (Route::CachePurge, ContentAction::Resource),
            (Route::Resource, ContentAction::Resource),
        ] {
            assert_eq!(action, content_action(route), "{route:?}");
            assert_eq!(action, content_decision(false, true, route), "{route:?}");
        }
    }
}
