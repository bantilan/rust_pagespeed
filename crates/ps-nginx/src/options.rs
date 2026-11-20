//! Request option determination (`ps_determine_options`).
//!
//! Ports the request-time option assembly: the remote-config plan,
//! query/header/cookie scanning, directory/request merging, script
//! execution, and the experiment gate. The remote fetch, purge-set
//! inheritance, experiment matcher, and cookie write execute in
//! Slice 6; this module pins the assembly order and decisions,
//! failing closed (serve unoptimized) wherever a deferred input
//! is required but unavailable.

use crate::conf::NgxOptions;
use crate::script::execute_script_lines;
use ps_config::message_handler::MessageHandler;
use ps_config::options::NginxFactoryOptions;
use ps_config::scope::Scope;
use ps_http::google_url::GoogleUrl;
use ps_http::query::{get_query_options, RewriteQuery};
use ps_http::request::{RequestContext, RequestHeaders};
use ps_http::response::ResponseHeaders;
use ps_http::time::format_http_date;

/// Planned remote-configuration fetch
/// (`ps_determine_remote_options` past the fetch).
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Cloned globals the fetch applies onto.
    pub options: NgxOptions,
    /// Configuration URL to fetch.
    pub fetch_url: String,
    /// Fetch wait in ms (zeroed when the wait would deadlock).
    pub timeout_ms: i64,
}

/// Plans the remote-configuration fetch: clone globals, zero the
/// wait when a native-fetcher fetch would block its own event loop
/// (`use_native_fetcher` with a blocking HTTP-cache backend), and
/// hand the fetch to Slice 6. `None` when no remote URL is
/// configured (the C++ null guards cannot fire on references; the
/// Slice 6 wrapper checks context presence before calling).
#[must_use]
pub fn plan_remote_options(
    global: &NgxOptions,
    use_native_fetcher: bool,
    cache_blocking: bool,
) -> Option<RemoteConfig> {
    let fetch_url = global.core.remote_configuration_url();
    if fetch_url.is_empty() {
        return None;
    }
    let mut options = global.clone_with();
    let mut timeout_ms = options.core.remote_configuration_timeout_ms();
    if use_native_fetcher && cache_blocking {
        timeout_ms = 0;
        options
            .core
            .set_option("RemoteConfigurationTimeoutMs", "0", Scope::ProcessStrict)
            .expect("literal parses");
    }
    Some(RemoteConfig { options, fetch_url, timeout_ms })
}

/// Request-scan outcome (`ps_determine_request_options` outputs).
pub struct RequestOptionsOutcome {
    /// Scanned request options, if any applied.
    pub options: Option<ps_config::options::RewriteOptions>,
    /// Consumed query params (escaped).
    pub params: String,
    /// Consumed option cookies (escaped).
    pub cookies: String,
    /// The scan failed to parse (the FFI logs and treats the
    /// request as option-free).
    pub parse_failed: bool,
}

/// Scans request options from URL, headers, and cookies: `None`
/// options (with `parse_failed` noted) when parsing fails.
pub fn determine_request_options(
    domain_options: &ps_config::options::RewriteOptions,
    request_context: Option<&mut RequestContext>,
    request_url: &mut GoogleUrl,
    request_headers: Option<&mut RequestHeaders>,
    response_headers: Option<&mut ResponseHeaders>,
    handler: &mut impl MessageHandler,
) -> RequestOptionsOutcome {
    let mut query = RewriteQuery::new();
    if !get_query_options(
        domain_options,
        request_context,
        request_url,
        request_headers,
        response_headers,
        &mut query,
        handler,
    ) {
        return RequestOptionsOutcome {
            options: None,
            params: String::new(),
            cookies: String::new(),
            parse_failed: true,
        };
    }
    RequestOptionsOutcome {
        options: query.release_options(),
        params: query.pagespeed_query_params().to_escaped_string(),
        cookies: query.pagespeed_option_cookies().to_escaped_string(),
        parse_failed: false,
    }
}

/// Experiment cookie name (`experiment::kExperimentCookie`).
pub const EXPERIMENT_COOKIE: &str = "PageSpeedExperiment";

/// Experiment matcher verdict (deferred matcher seam): the assigned
/// state plus whether the client needs the cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentVerdict {
    /// Assigned experiment state id.
    pub experiment_id: i32,
    /// Classification needs a cookie set.
    pub need_cookie: bool,
}

/// Whether the experiment applies (`ps_determine_options`
/// experiment gate): running, rewriting HTML, and either free of
/// request options or explicitly enrolled.
#[must_use]
pub fn experiment_applies(
    running_experiment: bool,
    html_rewrite: bool,
    have_request_options: bool,
    enroll_experiment: bool,
) -> bool {
    running_experiment && html_rewrite && (!have_request_options || enroll_experiment)
}

/// Builds the experiment cookie value
/// (`ps_set_experiment_state_and_cookie` value half).
#[must_use]
pub fn experiment_cookie_value(experiment_id: i32, expires_ms: i64, host: &str) -> String {
    format!(
        "{EXPERIMENT_COOKIE}={experiment_id}; Expires={}; Domain=.{host}; Path=/",
        format_http_date(expires_ms)
    )
}

/// Determined request options (`ps_determine_options` outputs, of
/// which the caller takes ownership).
pub struct DeterminedOptions {
    /// Custom options (`None` = use globals).
    pub options: Option<NgxOptions>,
    /// Consumed query params (escaped).
    pub pagespeed_query_params: String,
    /// Consumed option cookies (escaped).
    pub pagespeed_option_cookies: String,
    /// Assigned experiment state, if classified.
    pub experiment_id: Option<i32>,
    /// Experiment cookie to set, if classification needs one (the
    /// Slice 6 write may still fail the request on pool error).
    pub experiment_cookie: Option<String>,
}

/// Inputs to option determination (all extracted; `remote` arrives
/// from the Slice 6 remote fetch, `experiment_verdict` from the
/// deferred matcher).
pub struct DetermineInputs<'a> {
    /// Server global options (never null).
    pub global: &'a NgxOptions,
    /// Location options, already rebased (usually null).
    pub directory: Option<&'a NgxOptions>,
    /// Remote-config base, if a remote URL is configured.
    pub remote: Option<NgxOptions>,
    /// Factory options (script lines may set factory names).
    pub factory: &'a mut NginxFactoryOptions,
    /// Request context for sticky parameters.
    pub request_context: Option<&'a mut RequestContext>,
    /// Request URL (query params stripped in place).
    pub request_url: &'a mut GoogleUrl,
    /// Request headers (options consumed in place).
    pub request_headers: Option<&'a mut RequestHeaders>,
    /// Response headers (options consumed in place).
    pub response_headers: Option<&'a mut ResponseHeaders>,
    /// Script argument substituter (Slice 6 runs programs).
    pub substitute: &'a dyn Fn(usize, usize, &str) -> Option<String>,
    /// Matcher verdict, when the experiment gate needs one.
    pub experiment_verdict: Option<ExperimentVerdict>,
    /// Current time in ms (injected clock).
    pub now_ms: i64,
    /// Request host for the cookie domain.
    pub host: &'a str,
    /// HTML path (experiments only enroll on HTML).
    pub html_rewrite: bool,
}

/// Determines request options (`ps_determine_options`): `None`
/// (serve without optimization) on script failure and on
/// experiment application without a matcher verdict.
pub fn determine_options(
    inputs: DetermineInputs<'_>,
    handler: &mut impl MessageHandler,
) -> Option<DeterminedOptions> {
    let domain = inputs.directory.map(|d| &d.core).unwrap_or(&inputs.global.core);
    let scanned = determine_request_options(
        domain,
        inputs.request_context,
        inputs.request_url,
        inputs.request_headers,
        inputs.response_headers,
        handler,
    );
    let have_request_options = scanned.options.is_some();

    // Fast path: globals stand as they are (remote base passes
    // through untouched).
    if !have_request_options
        && inputs.directory.is_none()
        && !inputs.global.core.running_experiment()
        && inputs.global.script_lines.is_empty()
    {
        return Some(DeterminedOptions {
            options: inputs.remote,
            pagespeed_query_params: scanned.params,
            pagespeed_option_cookies: scanned.cookies,
            experiment_id: None,
            experiment_cookie: None,
        });
    }

    let mut assembled = match (inputs.remote, inputs.directory) {
        (Some(mut remote), Some(directory)) => {
            remote.core.merge(&directory.core);
            remote.merge_system(directory);
            remote
        }
        (None, Some(directory)) => directory.clone_with(),
        (remote, None) => remote.unwrap_or_else(|| inputs.global.clone_with()),
    };
    // Purge-set inheritance (`UpdateCachePurgeSet`) is a no-op
    // until Slice 6 owns cache state: there is nothing to inherit
    // from and no cache to invalidate against.

    if !execute_script_lines(
        &mut assembled,
        inputs.factory,
        inputs.substitute,
    ) {
        return None;
    }

    if have_request_options {
        assembled.core.merge(scanned.options.as_ref().expect("have options"));
    }

    let mut experiment_id = None;
    let mut experiment_cookie = None;
    if experiment_applies(
        assembled.core.running_experiment(),
        inputs.html_rewrite,
        have_request_options,
        assembled.core.enroll_experiment(),
    ) {
        let verdict = inputs.experiment_verdict?;
        experiment_id = Some(verdict.experiment_id);
        if verdict.need_cookie && !inputs.host.is_empty() {
            let duration = assembled.core.experiment_cookie_duration_ms();
            experiment_cookie = Some(experiment_cookie_value(
                verdict.experiment_id,
                inputs.now_ms + duration,
                inputs.host,
            ));
        }
    }

    Some(DeterminedOptions {
        options: Some(assembled),
        pagespeed_query_params: scanned.params,
        pagespeed_option_cookies: scanned.cookies,
        experiment_id,
        experiment_cookie,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::ScriptLine;
    use ps_config::filter::RewriteLevel;
    use ps_config::message_handler::{MockMessageHandler, NullMessageHandler};
    use ps_config::options::{EnabledState, RewriteOptions};
    use ps_config::scope::Scope;
    use ps_http::response::HttpOptions;

    fn nginx(core: RewriteOptions) -> NgxOptions {
        NgxOptions {
            core,
            system: Default::default(),
            script_lines: Vec::new(),
            clear_inherited_scripts: false,
        }
    }

    fn core_with(directives: &[(&str, &[&str])]) -> RewriteOptions {
        let mut core = RewriteOptions::new();
        let mut factory = NginxFactoryOptions::default();
        let mut system = Default::default();
        for (directive, args) in directives {
            ps_config::options::apply_nginx_directive(
                &mut core,
                &mut factory,
                &mut system,
                directive,
                args,
                Scope::Server,
            )
            .unwrap();
        }
        core
    }

    #[test]
    fn remote_plan_clones_and_zeroes() {
        let plain = nginx(RewriteOptions::new());
        assert!(plan_remote_options(&plain, false, false).is_none());
        let global = nginx(core_with(&[("RemoteConfigurationUrl", &["http://x/rc"])]));
        let planned = plan_remote_options(&global, false, true).unwrap();
        assert_eq!("http://x/rc", planned.fetch_url);
        assert_eq!(1000, planned.timeout_ms);
        // Native fetcher on a blocking backend cannot wait.
        let planned = plan_remote_options(&global, true, true).unwrap();
        assert_eq!(0, planned.timeout_ms);
        assert_eq!(
            0,
            planned.options.core.remote_configuration_timeout_ms()
        );
        // ...but a non-blocking backend keeps the wait.
        let planned = plan_remote_options(&global, true, false).unwrap();
        assert_eq!(1000, planned.timeout_ms);
    }

    #[test]
    fn fast_path_passes_remote_through() {
        let global = nginx(RewriteOptions::new());
        let mut factory = NginxFactoryOptions::default();
        let mut url = GoogleUrl::new("http://h/a?x=1");
        let mut handler = NullMessageHandler;
        let remote = nginx(core_with(&[("RewriteLevel", &["CoreFilters"])]));
        let determined = determine_options(
            DetermineInputs {
                global: &global,
                directory: None,
                remote: Some(remote),
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
        // Untouched remote base passes through as the custom options.
        assert_eq!(
            RewriteLevel::CoreFilters,
            determined.options.unwrap().core.filters().level()
        );
        assert!(determined.pagespeed_query_params.is_empty());
        // Without remote input the fast path yields no custom options.
        let mut url = GoogleUrl::new("http://h/a?x=1");
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
    }

    #[test]
    fn directory_request_and_script_merge() {
        let global = nginx(core_with(&[("RewriteLevel", &["PassThrough"])]));
        let directory = nginx(core_with(&[("RewriteLevel", &["CoreFilters"])]));
        let mut factory = NginxFactoryOptions::default();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeed=off");
        let mut handler = NullMessageHandler;
        let determined = determine_options(
            DetermineInputs {
                global: &global,
                directory: Some(&directory),
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
        let custom = determined.options.unwrap();
        // Directory level wins over global; the query switch applies.
        assert_eq!(RewriteLevel::CoreFilters, custom.core.filters().level());
        assert_eq!(EnabledState::Off, custom.core.enabled_state());
        assert_eq!("ModPagespeed=off", determined.pagespeed_query_params);
        // Script failure fails the whole determination.
        let mut global = nginx(RewriteOptions::new());
        global.script_lines.push(ScriptLine {
            args: vec!["BogusDirective".to_string(), "x".to_string()],
            scope: Scope::Server,
        });
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
        );
        assert!(determined.is_none());
    }

    #[test]
    fn experiment_gate_and_cookie() {
        assert!(experiment_applies(true, true, false, false));
        assert!(experiment_applies(true, true, true, true));
        assert!(!experiment_applies(true, true, true, false));
        assert!(!experiment_applies(true, false, false, false));
        assert!(!experiment_applies(false, true, false, false));
        assert_eq!(
            "PageSpeedExperiment=2; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Domain=.h; Path=/",
            experiment_cookie_value(2, 0, "h")
        );
        // No verdict while an experiment applies fails closed.
        let global = nginx(core_with(&[("RunExperiment", &["true"])]));
        let mut factory = NginxFactoryOptions::default();
        let mut url = GoogleUrl::new("http://h/a");
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
                html_rewrite: true,
            },
            &mut handler,
        );
        assert!(determined.is_none());
        // With a verdict the id records and the cookie builds.
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
                experiment_verdict: Some(ExperimentVerdict { experiment_id: 3, need_cookie: true }),
                now_ms: 0,
                host: "h",
                html_rewrite: true,
            },
            &mut handler,
        )
        .unwrap();
        assert_eq!(Some(3), determined.experiment_id);
        let cookie = determined.experiment_cookie.unwrap();
        assert!(cookie.starts_with("PageSpeedExperiment=3; Expires="));
        assert!(cookie.ends_with("; Domain=.h; Path=/"));
    }

    #[test]
    fn request_scan_failure_marks_parse_failed() {
        let domain = RewriteOptions::new();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeed=bogus");
        let mut handler = MockMessageHandler::new();
        let outcome = determine_request_options(
            &domain,
            None,
            &mut url,
            None,
            None,
            &mut handler,
        );
        assert!(outcome.options.is_none());
        assert!(outcome.parse_failed);
        let mut context = RequestContext::new(HttpOptions::default_for_tests());
        let mut url = GoogleUrl::new("http://h/a?ModPagespeed=off");
        let outcome = determine_request_options(
            &domain,
            Some(&mut context),
            &mut url,
            None,
            None,
            &mut handler,
        );
        assert_eq!(EnabledState::Off, outcome.options.unwrap().enabled_state());
        assert!(!outcome.parse_failed);
        assert_eq!("ModPagespeed=off", outcome.params);
    }
}
