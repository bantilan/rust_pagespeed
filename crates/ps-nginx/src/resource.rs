//! Resource-handler decisions (`ps_resource_handler` branch selection).
//!
//! Ports the handler's gating and branch selection as pure
//! decisions over extracted inputs. Options resolution
//! (`ps_determine_remote_options` / `ps_determine_options`) sits
//! between the two decision halves and lands in Slice 3b; branch
//! bodies (fetches, drivers, admin renders) belong to Slices
//! 4-6. The FFI wrapper (Slice 6) runs: gates → options →
//! branch → body, declining until each body lands.

use crate::routing::Route;
use ps_config::options::{EnabledState, RewriteOptions};
use ps_http::google_url::GoogleUrl;
use ps_http::request::Method;

/// Whether a route is an admin-style handler (`is_an_admin_handler`).
#[must_use]
pub const fn is_admin_route(route: Route) -> bool {
    matches!(
        route,
        Route::Statistics
            | Route::GlobalStatistics
            | Route::Console
            | Route::Admin
            | Route::GlobalAdmin
            | Route::CachePurge
    )
}

/// Pre-options gates (`ps_resource_handler` head).
pub struct GateInputs<'a> {
    /// Main request (not a subrequest).
    pub is_main: bool,
    /// Proxy-fetch factory initialized.
    pub factory_present: bool,
    /// Worker is shutting down.
    pub shutting_down: bool,
    /// HTML path (`html_rewrite` argument).
    pub html_rewrite: bool,
    /// Context exists and has HTML armed
    /// (`ctx != NULL && ctx->html_rewrite`).
    pub ctx_armed: bool,
    /// Request method.
    pub method: Method,
    /// Routed category.
    pub route: Route,
    /// Final request URL (post options-resolution rewrite).
    pub url_text: &'a str,
}

/// Gate outcome: proceed or decline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Continue into options resolution.
    Proceed,
    /// Serve without optimization (`NGX_DECLINED`). On the
    /// shutdown path the FFI also releases an attached base fetch.
    Decline,
}

/// Runs the pre-options gates: subrequest, factory, shutdown,
/// method (non-HTML only, purge excepted), and URL validity.
#[must_use]
pub fn decide_gates(inputs: &GateInputs) -> Gate {
    // The HTML path always arrives with an armed context (`CHECK`).
    assert!(
        !(inputs.html_rewrite && !inputs.ctx_armed),
        "html path without an armed context"
    );
    if !inputs.is_main {
        return Gate::Decline;
    }
    if !inputs.factory_present {
        return Gate::Decline;
    }
    if inputs.shutting_down {
        return Gate::Decline;
    }
    if !inputs.html_rewrite
        && !matches!(inputs.method, Method::Get | Method::Head | Method::Post)
        && inputs.route != Route::CachePurge
    {
        return Gate::Decline;
    }
    if !GoogleUrl::new(inputs.url_text).is_web_valid() {
        return Gate::Decline;
    }
    Gate::Proceed
}

/// Post-options branch inputs (resolved custom or global options).
pub struct BranchInputs {
    /// Fully passive (`unplugged()`).
    pub unplugged: bool,
    /// Master switch on (`enabled()`).
    pub enabled: bool,
    /// URL names a `.pagespeed.` resource.
    pub pagespeed_resource: bool,
    /// HTML path.
    pub html_rewrite: bool,
    /// Routed category.
    pub route: Route,
    /// Request method.
    pub method: Method,
    /// Origin mapping names a proxy fetch.
    pub proxy_mapped: bool,
    /// URL passes the allow list (`IsAllowed`).
    pub is_allowed: bool,
    /// In-place rewriting on (`in_place_rewriting_enabled()`).
    pub in_place_enabled: bool,
    /// Server and module read the URL alike (IPRO agreement).
    pub ipro_url_agrees: bool,
}

impl BranchInputs {
    /// Fills the option-derived inputs from resolved options (the
    /// `determine_options` custom output, or globals when it yields
    /// none): switch state, allow verdict, and the in-place gate.
    pub fn resolve_from(&mut self, options: &RewriteOptions, url_spec: &str) {
        self.unplugged = options.unplugged();
        self.enabled = options.enabled_state() == EnabledState::On;
        self.is_allowed = options.is_allowed(url_spec);
        self.in_place_enabled = options.in_place_rewriting_enabled();
    }
}

/// Selected branch (bodies in Slices 4-6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branch {
    /// Serve without optimization (`NGX_DECLINED`).
    Decline,
    /// Fetch a `.pagespeed.` resource, then wait (`ResourceFetch`).
    PagespeedResource,
    /// Render an admin page, then wait.
    Admin,
    /// Proxy the mapped origin, then wait.
    Proxy,
    /// Transform HTML inline (`NGX_OK`).
    Html,
    /// Look up / record the in-place resource, then wait.
    InPlace,
    /// Non-pagespeed content: arm HTML for the filter pass and
    /// decline (sets `html_rewrite`, `NGX_DECLINED`).
    PassThrough,
}

/// Selects the branch: standby gate, then resource / admin /
/// proxy / HTML / in-place / pass-through, in C++ order.
#[must_use]
pub fn decide_branch(inputs: &BranchInputs) -> Branch {
    if inputs.unplugged || (!inputs.enabled && !inputs.pagespeed_resource) {
        return Branch::Decline;
    }
    if inputs.pagespeed_resource {
        return Branch::PagespeedResource;
    }
    if is_admin_route(inputs.route) {
        return Branch::Admin;
    }
    if !inputs.html_rewrite
        && inputs.route == Route::Resource
        && matches!(inputs.method, Method::Get | Method::Head)
        && inputs.proxy_mapped
    {
        return Branch::Proxy;
    }
    if inputs.html_rewrite && inputs.is_allowed {
        return Branch::Html;
    }
    if inputs.in_place_enabled && inputs.enabled && inputs.is_allowed && inputs.ipro_url_agrees {
        return Branch::InPlace;
    }
    Branch::PassThrough
}

/// Rewrites HEAD to GET for the HTML pipeline (`ps_resource_handler`
/// HTML branch): the pipeline answers a HEAD like the GET it
/// mirrors, and nginx drops the body itself.
#[must_use]
pub const fn html_branch_method(method: Method) -> Method {
    match method {
        Method::Head => Method::Get,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gates(url: &str) -> GateInputs<'_> {
        GateInputs {
            is_main: true,
            factory_present: true,
            shutting_down: false,
            html_rewrite: false,
            ctx_armed: false,
            method: Method::Get,
            route: Route::Resource,
            url_text: url,
        }
    }

    fn branches() -> BranchInputs {
        BranchInputs {
            unplugged: false,
            enabled: true,
            pagespeed_resource: false,
            html_rewrite: false,
            route: Route::Resource,
            method: Method::Get,
            proxy_mapped: false,
            is_allowed: true,
            in_place_enabled: false,
            ipro_url_agrees: true,
        }
    }

    #[test]
    fn gate_matrix() {
        assert_eq!(Gate::Proceed, decide_gates(&gates("http://h/a")));
        let mut declined = gates("http://h/a");
        declined.is_main = false;
        assert_eq!(Gate::Decline, decide_gates(&declined));
        let mut declined = gates("http://h/a");
        declined.factory_present = false;
        assert_eq!(Gate::Decline, decide_gates(&declined));
        let mut declined = gates("http://h/a");
        declined.shutting_down = true;
        assert_eq!(Gate::Decline, decide_gates(&declined));
        assert_eq!(Gate::Decline, decide_gates(&gates("not a url")));
        // Method gate: non-HTML only, purge excepted.
        let mut put = gates("http://h/a");
        put.method = Method::Put;
        assert_eq!(Gate::Decline, decide_gates(&put));
        put.route = Route::CachePurge;
        assert_eq!(Gate::Proceed, decide_gates(&put));
        let mut html_put = gates("http://h/a");
        html_put.method = Method::Put;
        html_put.html_rewrite = true;
        html_put.ctx_armed = true;
        assert_eq!(Gate::Proceed, decide_gates(&html_put));
    }

    #[test]
    #[should_panic(expected = "html path without an armed context")]
    fn html_path_requires_armed_ctx() {
        let mut inputs = gates("http://h/a");
        inputs.html_rewrite = true;
        let _ = decide_gates(&inputs);
    }

    #[test]
    fn branch_matrix() {
        // Standby gate: unplugged declines; disabled-but-standby
        // serves resources only.
        let mut standby = branches();
        standby.enabled = false;
        assert_eq!(Branch::Decline, decide_branch(&standby));
        standby.pagespeed_resource = true;
        assert_eq!(Branch::PagespeedResource, decide_branch(&standby));
        standby.unplugged = true;
        assert_eq!(Branch::Decline, decide_branch(&standby));
        // Resource and admin branches.
        let mut resource = branches();
        resource.pagespeed_resource = true;
        assert_eq!(Branch::PagespeedResource, decide_branch(&resource));
        for route in [
            Route::Statistics,
            Route::GlobalStatistics,
            Route::Console,
            Route::Admin,
            Route::GlobalAdmin,
            Route::CachePurge,
        ] {
            let mut admin = branches();
            admin.route = route;
            assert!(is_admin_route(route));
            assert_eq!(Branch::Admin, decide_branch(&admin), "{route:?}");
        }
        assert!(!is_admin_route(Route::Resource));
        assert!(!is_admin_route(Route::Beacon));
        // Proxy: non-HTML resource route, GET/HEAD, mapped.
        let mut proxy = branches();
        proxy.proxy_mapped = true;
        assert_eq!(Branch::Proxy, decide_branch(&proxy));
        proxy.method = Method::Post;
        assert_eq!(Branch::PassThrough, decide_branch(&proxy));
        // HTML branch needs the allow list.
        let mut html = branches();
        html.html_rewrite = true;
        assert_eq!(Branch::Html, decide_branch(&html));
        html.is_allowed = false;
        assert_eq!(Branch::PassThrough, decide_branch(&html));
        // In-place needs the full trio plus agreement.
        let mut ipro = branches();
        ipro.in_place_enabled = true;
        assert_eq!(Branch::InPlace, decide_branch(&ipro));
        ipro.ipro_url_agrees = false;
        assert_eq!(Branch::PassThrough, decide_branch(&ipro));
        // Bare fall-through.
        assert_eq!(Branch::PassThrough, decide_branch(&branches()));
    }

    #[test]
    fn head_downgrades_for_html() {
        assert_eq!(Method::Get, html_branch_method(Method::Head));
        assert_eq!(Method::Get, html_branch_method(Method::Get));
        assert_eq!(Method::Post, html_branch_method(Method::Post));
    }

    #[test]
    fn determined_options_drive_branches() {
        use crate::conf::NgxOptions;
        use crate::options::{determine_options, DetermineInputs};
        use ps_config::message_handler::NullMessageHandler;
        use ps_config::options::NginxFactoryOptions;
        use ps_http::google_url::GoogleUrl;

        // A query switch flows through determination into the gate.
        let global = NgxOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeed=off");
        let mut handler = NullMessageHandler;
        let determined = determine_options(
            DetermineInputs {
                global: &global,
                directory: None,
                remote: None,
                factory: &mut factory,
                request_context: None,
                request_url: &mut url,
                request_headers: None,
                response_headers: None,
                substitute: &|_, _, raw| Some(raw.to_string()),
                experiment_verdict: None,
                now_ms: 0,
                host: "h",
                html_rewrite: false,
            },
            &mut handler,
        )
        .unwrap();
        let mut inputs = branches();
        let resolved = determined.options.as_ref().map(|o| &o.core).unwrap_or(&global.core);
        inputs.resolve_from(resolved, "http://h/a");
        assert_eq!(Branch::Decline, decide_branch(&inputs));
        // Without the switch the same request passes through.
        let mut url = GoogleUrl::new("http://h/a");
        let determined = determine_options(
            DetermineInputs {
                global: &global,
                directory: None,
                remote: None,
                factory: &mut factory,
                request_context: None,
                request_url: &mut url,
                request_headers: None,
                response_headers: None,
                substitute: &|_, _, raw| Some(raw.to_string()),
                experiment_verdict: None,
                now_ms: 0,
                host: "h",
                html_rewrite: false,
            },
            &mut handler,
        )
        .unwrap();
        assert!(determined.options.is_none());
        let mut inputs = branches();
        inputs.ipro_url_agrees = false;
        inputs.resolve_from(&global.core, "http://h/a");
        assert_eq!(Branch::PassThrough, decide_branch(&inputs));
    }
}
