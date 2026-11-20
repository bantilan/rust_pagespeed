//! Request option scanning (`RewriteQuery::Scan`).
//!
//! Ports the cookies → query → request-header → response-header
//! option scan: related URL options, Ajax disables, the custom-option
//! pre-scan, cookie authentication, query consumption with URL
//! rebuild, header scans with consumption, and the CoreFilters
//! default. Later sources override earlier ones, except filter
//! disables always win. Outputs are the surviving query params,
//! the consumed PageSpeed params and cookies, and the built request
//! options (`None` when nothing applied).
//!
//! Seams: `RequestProperties` is omitted (its only consumer,
//! `SetEffectiveImageQualities`, is dead in C++ — declared, never
//! called); sticky tokens record into the passed request context.

use crate::google_url::{escape_query_param, GoogleUrl};
use crate::headers::Headers;
use crate::names::header;
use crate::query_params::QueryParams;
use crate::request::{RequestContext, RequestHeaders};
use crate::response::ResponseHeaders;
use ps_cache::namer::ResourceNamer;
use ps_config::filter::{related_tables_for_decoding_id, Filter, RewriteLevel};
use ps_config::message_handler::{MessageHandler, MessageLevel};
use ps_config::options::{trim_url_quotes, QueryStatus, RewriteOptions};
use ps_config::property::lookup_by_id;
use ps_config::scope::Scope;

/// Query-param prefixes (`kModPagespeed`, `kPageSpeed`).
pub const MOD_PAGESPEED: &str = "ModPagespeed";
/// Short query-param prefix.
pub const PAGE_SPEED: &str = "PageSpeed";
/// Sticky-parameters suffix (`kStickyQueryParameters`).
const STICKY_QUERY_PARAMETERS: &str = "StickyQueryParameters";
/// Resource-option separators (`kResourceFilterSeparator`,
/// `kResourceOptionValueSeparator`).
const FILTER_SEPARATOR: char = '+';
/// Option name/value separator inside resource segments.
const OPTION_VALUE_SEPARATOR: char = '=';
/// Dummy base for cookie-value sanitizing (`SanitizeValueAsQP`).
const SANITIZE_BASE: &str = "http://www.example.com/?x=";

/// Whether a scan outcome is usable (`IsOK`).
#[must_use]
pub fn status_is_ok(status: QueryStatus) -> bool {
    matches!(status, QueryStatus::NoneFound | QueryStatus::Success)
}

/// Whether a name might carry options (`MightBeCustomOption`):
/// case-sensitive `ModPagespeed`/`PageSpeed` prefix, or the
/// client-options header (case-insensitive).
#[must_use]
pub fn might_be_custom_option(name: &str) -> bool {
    name.starts_with(MOD_PAGESPEED)
        || name.starts_with(PAGE_SPEED)
        || name.eq_ignore_ascii_case(header::X_PSA_CLIENT_OPTIONS)
}

/// Whether a name addresses sticky parameters (the
/// `StickyQueryParameters` suffix, case-insensitive).
fn is_sticky_name(name: &str) -> bool {
    name.strip_prefix(MOD_PAGESPEED)
        .or_else(|| name.strip_prefix(PAGE_SPEED))
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(STICKY_QUERY_PARAMETERS))
}

/// Removes the override token from response headers
/// (`RemoveRequestOptionOverride`).
pub fn remove_request_option_override(headers: &mut Headers) {
    headers.remove_all("ModPagespeedRequestOptionOverride");
    headers.remove_all("PageSpeedRequestOptionOverride");
}

/// Sanitizes an untrusted value as a query value
/// (`SanitizeValueAsQP`): runs it through URL parsing and keeps
/// the surviving suffix.
fn sanitize_value_as_qp(untrusted_value: &str) -> String {
    let probe = GoogleUrl::new(&format!("{SANITIZE_BASE}{untrusted_value}"));
    probe
        .spec()
        .get(SANITIZE_BASE.len()..)
        .unwrap_or("")
        .to_string()
}

/// Parses a resource option segment (`ParseResourceOption`): filter
/// ids enable related filters, `id:value` pairs set related
/// options; anything else invalidates. The level drops to
/// PassThrough with only explicitly enabled filters kept.
fn parse_resource_option(
    value: &str,
    options: &mut RewriteOptions,
    related_filters: &[Filter],
    related_options: &[&str],
) -> QueryStatus {
    let mut status = QueryStatus::NoneFound;
    for segment in value.split(FILTER_SEPARATOR).filter(|s| !s.is_empty()) {
        let parts: Vec<&str> =
            segment.split(OPTION_VALUE_SEPARATOR).filter(|s| !s.is_empty()).collect();
        match parts.len() {
            1 => match Filter::from_id(parts[0]) {
                Some(filter) if related_filters.binary_search(&filter).is_ok() => {
                    options.filters_mut().enable(filter);
                    status = QueryStatus::Success;
                }
                _ => status = QueryStatus::Invalid,
            },
            2 => {
                let name = lookup_by_id(parts[0]).map(|prop| prop.name);
                match name {
                    Some(name)
                        if related_options.binary_search(&name).is_ok()
                            && options
                                .set_option(name, parts[1], Scope::Directory)
                                .is_ok() =>
                    {
                        status = QueryStatus::Success;
                    }
                    _ => status = QueryStatus::Invalid,
                }
            }
            _ => status = QueryStatus::Invalid,
        }
    }
    options.set_rewrite_level(RewriteLevel::PassThrough);
    options.filters_mut().disable_all_not_explicitly_enabled();
    status
}

/// Scans one header list (`ScanHeader`): override-gate, per-pair
/// scan with sticky recording, consumption of successful
/// `ModPagespeed*`/`PageSpeed*` headers, unconditional `X-PSA`
/// removal. `remove_token` additionally strips the override token
/// (response headers).
pub fn scan_header_list(
    headers: &mut Headers,
    allow_options: bool,
    request_option_override: &str,
    request_context: &mut Option<&mut RequestContext>,
    options: &mut RewriteOptions,
    remove_token: bool,
) -> QueryStatus {
    let mut allow_options = allow_options;
    let mut status = QueryStatus::NoneFound;
    if !allow_options && !request_option_override.is_empty() {
        for (name, value) in headers.iter() {
            if name == "ModPagespeedRequestOptionOverride"
                || name == "PageSpeedRequestOptionOverride"
            {
                allow_options = value == request_option_override;
                break;
            }
        }
    }
    if remove_token {
        remove_request_option_override(headers);
    }
    let pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let mut to_remove: Vec<(String, String)> = Vec::new();
    for (name, value) in &pairs {
        if is_sticky_name(name) && request_context.is_none() {
            continue;
        }
        match options.scan_query(name, value, allow_options) {
            QueryStatus::NoneFound => {}
            QueryStatus::Success => {
                if is_sticky_name(name) {
                    if let Some(context) = request_context {
                        context.set_sticky_query_parameters_token(trim_url_quotes(value));
                    }
                }
                if name.starts_with(MOD_PAGESPEED) || name.starts_with(PAGE_SPEED) {
                    to_remove.push((name.clone(), value.clone()));
                }
                status = QueryStatus::Success;
            }
            QueryStatus::Invalid => return QueryStatus::Invalid,
        }
    }
    for (name, value) in &to_remove {
        headers.remove(name, value);
    }
    headers.remove_all(header::X_PSA_CLIENT_OPTIONS);
    status
}

/// Request option scan state (`RewriteQuery`).
#[derive(Debug, Default)]
pub struct RewriteQuery {
    query_params: QueryParams,
    pagespeed_query_params: QueryParams,
    pagespeed_option_cookies: QueryParams,
    options: Option<RewriteOptions>,
}

impl RewriteQuery {
    /// Creates an empty scan.
    #[must_use]
    pub fn new() -> Self {
        RewriteQuery::default()
    }

    /// Surviving query params.
    #[must_use]
    pub fn query_params(&self) -> &QueryParams {
        &self.query_params
    }

    /// Consumed PageSpeed query params.
    #[must_use]
    pub fn pagespeed_query_params(&self) -> &QueryParams {
        &self.pagespeed_query_params
    }

    /// Consumed PageSpeed option cookies.
    #[must_use]
    pub fn pagespeed_option_cookies(&self) -> &QueryParams {
        &self.pagespeed_option_cookies
    }

    /// Built request options, if any.
    #[must_use]
    pub fn options(&self) -> Option<&RewriteOptions> {
        self.options.as_ref()
    }

    /// Takes the built request options (`ReleaseOptions`).
    pub fn release_options(&mut self) -> Option<RewriteOptions> {
        self.options.take()
    }

    /// Caps request-built thresholds (`LimitSizeThresholdsTo`).
    pub fn limit_size_thresholds_to(&mut self, ceiling: &RewriteOptions) {
        if let Some(options) = self.options.as_mut() {
            options.limit_size_thresholds_to(ceiling);
        }
    }

    /// Whether anything looks like an option (`MayHaveCustomOptions`).
    fn may_have_custom_options(
        params: &QueryParams,
        request_headers: Option<&RequestHeaders>,
        response_headers: Option<&ResponseHeaders>,
        cookies: &[(String, String)],
    ) -> bool {
        for index in 0..params.len() {
            if might_be_custom_option(params.name(index)) {
                return true;
            }
        }
        for headers in [request_headers.map(|h| h.headers()), response_headers.map(|h| h.headers())]
            .into_iter()
            .flatten()
        {
            if headers.iter().any(|(name, _)| might_be_custom_option(name)) {
                return true;
            }
        }
        if cookies.iter().any(|(name, _)| might_be_custom_option(name)) {
            return true;
        }
        if request_headers.is_some_and(|headers| {
            headers.headers().has(header::X_PSA_CLIENT_OPTIONS)
                || headers.headers().has_cache_control_directive("no-transform")
        }) {
            return true;
        }
        if response_headers.is_some_and(|headers| {
            headers.headers().has_cache_control_directive("no-transform")
        }) {
            return true;
        }
        false
    }

    /// Full option scan (`Scan`).
    #[allow(clippy::too_many_arguments)]
    pub fn scan(
        &mut self,
        allow_related_options: bool,
        allow_options_by_cookies: bool,
        request_option_override: &str,
        request_context: Option<&mut RequestContext>,
        request_url: &mut GoogleUrl,
        request_headers: Option<&mut RequestHeaders>,
        mut response_headers: Option<&mut ResponseHeaders>,
        handler: &mut impl MessageHandler,
    ) -> QueryStatus {
        let status = self.scan_inner(
            allow_related_options,
            allow_options_by_cookies,
            request_option_override,
            request_context,
            request_url,
            request_headers,
            &mut response_headers,
            handler,
        );
        // The override token never reaches the client, on every
        // return path (`ScopedRemoveRequestOptionOverride`).
        if let Some(headers) = response_headers {
            remove_request_option_override(headers.headers_mut());
        }
        status
    }

    /// Scan body; the wrapper strips the response token afterwards.
    #[allow(clippy::too_many_arguments)]
    fn scan_inner(
        &mut self,
        allow_related_options: bool,
        allow_options_by_cookies: bool,
        request_option_override: &str,
        mut request_context: Option<&mut RequestContext>,
        request_url: &mut GoogleUrl,
        request_headers: Option<&mut RequestHeaders>,
        response_headers: &mut Option<&mut ResponseHeaders>,
        handler: &mut impl MessageHandler,
    ) -> QueryStatus {
        let mut status = QueryStatus::NoneFound;
        self.query_params.clear();
        self.pagespeed_query_params.clear();
        self.pagespeed_option_cookies.clear();
        self.options = None;

        let mut return_after_parsing = false;
        if allow_related_options {
            let mut namer = ResourceNamer::new();
            if namer.decode_ignore_hash_and_signature(&request_url.leaf_sans_query())
                && namer.has_options()
            {
                if let Some((filters, options)) = related_tables_for_decoding_id(namer.id()) {
                    let mut fresh = RewriteOptions::new();
                    status = parse_resource_option(namer.options(), &mut fresh, filters, options);
                    if status != QueryStatus::Success {
                        return_after_parsing = true;
                    } else {
                        self.options = Some(fresh);
                    }
                }
            }
        }

        let all_cookies: Vec<(String, String)> =
            match (allow_options_by_cookies, request_headers.as_deref()) {
                (true, Some(headers)) => headers.get_all_cookies(),
                _ => Vec::new(),
            };

        if request_headers
            .as_ref()
            .is_some_and(|headers| headers.is_xml_http_request())
        {
            if self.options.is_none() {
                self.options = Some(RewriteOptions::new());
            }
            self.options
                .as_mut()
                .expect("set above")
                .disable_filters_that_cant_run_in_ajax();
            status = QueryStatus::Success;
        }

        self.query_params.parse_from_url(request_url);
        if return_after_parsing
            || !Self::may_have_custom_options(
                &self.query_params,
                request_headers.as_deref(),
                response_headers.as_deref(),
                &all_cookies,
            )
        {
            return status;
        }

        if self.options.is_none() {
            self.options = Some(RewriteOptions::new());
        }

        let mut allow_options = true;
        if !request_option_override.is_empty() {
            allow_options = false;
            let token = self
                .query_params
                .lookup1_unescaped("ModPagespeedRequestOptionOverride")
                .or_else(|| {
                    self.query_params
                        .lookup1_unescaped("PageSpeedRequestOptionOverride")
                });
            if let Some(token) = token {
                allow_options = token == request_option_override;
            }
        }

        for (cookie_name, cookie_value) in &all_cookies {
            if !might_be_custom_option(cookie_name) {
                continue;
            }
            let unescaped = GoogleUrl::unescape_ignore_plus(cookie_value);
            let Ok(unescaped) = String::from_utf8(unescaped) else {
                continue;
            };
            let sanitized = sanitize_value_as_qp(&unescaped);
            let escaped = escape_query_param(&sanitized);
            if unescaped == sanitized && escaped == *cookie_value {
                if is_sticky_name(cookie_name) {
                    continue;
                }
                let options = self.options.as_mut().expect("set above");
                if options.scan_query(cookie_name, &unescaped, allow_options)
                    == QueryStatus::Success
                {
                    self.pagespeed_option_cookies.add_escaped(cookie_name, &unescaped);
                    status = QueryStatus::Success;
                }
            } else {
                handler.message(
                    MessageLevel::Info,
                    format!(
                        "PageSpeed Cookie value seems mangled: name='{cookie_name}', \
                         value='{cookie_value}', escaped value='{escaped}'"
                    ),
                );
            }
        }

        self.pagespeed_query_params.clear();
        let mut temp_query_params = QueryParams::new();
        for index in 0..self.query_params.len() {
            let name = self.query_params.name(index).to_string();
            let Some(unescaped) = self.query_params.unescaped_value_best_effort(index) else {
                temp_query_params.add_escaped(&name, "");
                continue;
            };
            // The unescaper turns `+` into a space, which headers
            // never do: turn spaces back before scanning.
            let fixed = unescaped.replace(' ', "+");
            let sticky_without_context =
                is_sticky_name(&name) && request_context.is_none();
            if sticky_without_context {
                temp_query_params.add_escaped(
                    &name,
                    self.query_params.escaped_value(index).unwrap_or(""),
                );
                continue;
            }
            match self
                .options
                .as_mut()
                .expect("set above")
                .scan_query(&name, &fixed, allow_options)
            {
                QueryStatus::NoneFound => {
                    temp_query_params.add_escaped(
                        &name,
                        self.query_params.escaped_value(index).unwrap_or(""),
                    );
                }
                QueryStatus::Success => {
                    if is_sticky_name(&name) {
                        if let Some(context) = request_context.as_deref_mut() {
                            context.set_sticky_query_parameters_token(trim_url_quotes(&fixed));
                        }
                    }
                    self.pagespeed_query_params.add_escaped(
                        &name,
                        self.query_params.escaped_value(index).unwrap_or(""),
                    );
                    status = QueryStatus::Success;
                }
                QueryStatus::Invalid => {
                    status = QueryStatus::Invalid;
                    self.options = None;
                    return status;
                }
            }
        }
        if status == QueryStatus::Success {
            let temp_params = if temp_query_params.is_empty() {
                String::new()
            } else {
                format!("?{}", temp_query_params.to_escaped_string())
            };
            let rebuilt = format!(
                "{}{}{}",
                request_url.all_except_query(),
                temp_params,
                request_url.all_after_query()
            );
            *request_url = GoogleUrl::new(&rebuilt);
        }

        if let Some(headers) = request_headers {
            match scan_header_list(
                headers.headers_mut(),
                allow_options,
                request_option_override,
                &mut request_context,
                self.options.as_mut().expect("set above"),
                false,
            ) {
                QueryStatus::NoneFound => {}
                QueryStatus::Success => status = QueryStatus::Success,
                QueryStatus::Invalid => {
                    status = QueryStatus::Invalid;
                    self.options = None;
                    return status;
                }
            }
        }

        if let Some(headers) = response_headers.as_deref_mut() {
            match scan_header_list(
                headers.headers_mut(),
                allow_options,
                request_option_override,
                &mut request_context,
                self.options.as_mut().expect("set above"),
                true,
            ) {
                QueryStatus::NoneFound => {}
                QueryStatus::Success => status = QueryStatus::Success,
                QueryStatus::Invalid => {
                    status = QueryStatus::Invalid;
                    self.options = None;
                    return status;
                }
            }
        }

        match status {
            QueryStatus::Success => {
                self.options
                    .as_mut()
                    .expect("success implies options")
                    .set_default_rewrite_level(RewriteLevel::CoreFilters);
            }
            QueryStatus::NoneFound => {
                self.options = None;
            }
            QueryStatus::Invalid => {
                debug_assert!(false, "invalid responses always use early exit");
                self.options = None;
            }
        }
        status
    }
}

/// Builds request options from URL, headers, and cookies
/// (`ServerContext::GetQueryOptions`).
#[allow(clippy::too_many_arguments)]
pub fn get_query_options(
    domain_options: &RewriteOptions,
    request_context: Option<&mut RequestContext>,
    request_url: &mut GoogleUrl,
    request_headers: Option<&mut RequestHeaders>,
    response_headers: Option<&mut ResponseHeaders>,
    query: &mut RewriteQuery,
    handler: &mut impl MessageHandler,
) -> bool {
    if !request_url.is_web_valid() {
        handler.message(
            MessageLevel::Error,
            format!("GetQueryOptions: Invalid URL: {}", request_url.spec()),
        );
        return false;
    }
    let ok = status_is_ok(query.scan(
        domain_options.add_options_to_urls(),
        domain_options.allow_options_to_be_set_by_cookies(),
        &domain_options.request_option_override(),
        request_context,
        request_url,
        request_headers,
        response_headers,
        handler,
    ));
    query.limit_size_thresholds_to(domain_options);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::namer::ResourceNamer;
    use ps_config::filter::Filter;
    use ps_config::message_handler::{MockMessageHandler, NullMessageHandler};
    use ps_config::options::{EnabledState, Value};
    use ps_config::scope::Scope;

    fn scan_all(
        query: &mut RewriteQuery,
        url: &mut GoogleUrl,
        headers: Option<&mut RequestHeaders>,
        response: Option<&mut ResponseHeaders>,
    ) -> QueryStatus {
        let mut handler = NullMessageHandler;
        query.scan(false, true, "", None, url, headers, response, &mut handler)
    }

    #[test]
    fn empty_scan_finds_nothing() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?x=1");
        let mut headers = RequestHeaders::new();
        let mut response = ResponseHeaders::new(crate::response::HttpOptions::default_for_tests());
        let status = scan_all(&mut query, &mut url, Some(&mut headers), Some(&mut response));
        assert_eq!(QueryStatus::NoneFound, status);
        assert!(query.options().is_none());
        assert_eq!("http://h/a?x=1", url.spec());
    }

    #[test]
    fn bare_switch_query_and_headers() {
        for (url_query, header) in [
            ("ModPagespeed=off", None),
            ("PageSpeed=off", None),
            ("", Some(("ModPagespeed", "off"))),
            ("", Some(("PageSpeed", "off"))),
        ] {
            let mut query = RewriteQuery::new();
            let mut url = GoogleUrl::new(&format!("http://h/a?{url_query}"));
            let mut headers = RequestHeaders::new();
            if let Some((name, value)) = header {
                headers.headers_mut().add(name, value);
            }
            let status = scan_all(&mut query, &mut url, Some(&mut headers), None);
            assert_eq!(QueryStatus::Success, status);
            let options = query.options().unwrap();
            assert_eq!(EnabledState::Off, options.enabled_state());
        }
        // CoreFilters default enables core filters.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeed=on");
        let status = scan_all(&mut query, &mut url, None, None);
        assert_eq!(QueryStatus::Success, status);
        let options = query.options().unwrap();
        assert!(options.enabled(Filter::CombineCss));
        assert!(options.enabled(Filter::RewriteCss));
        assert!(options.enabled(Filter::RewriteJavascriptExternal));
        assert!(!options.enabled(Filter::ExtendCachePdfs));
    }

    #[test]
    fn filters_list_resets() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeedFilters=remove_quotes");
        let status = scan_all(&mut query, &mut url, None, None);
        assert_eq!(QueryStatus::Success, status);
        let options = query.options().unwrap();
        assert!(options.enabled(Filter::RemoveQuotes));
        assert!(!options.enabled(Filter::CombineCss));
        // The param is consumed from the URL and recorded.
        assert_eq!("http://h/a", url.spec());
        assert_eq!(
            "ModPagespeedFilters=remove_quotes",
            query.pagespeed_query_params().to_escaped_string()
        );
    }

    #[test]
    fn query_strip_keeps_others() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?x=1&ModPagespeed=off&y=2#frag");
        let status = scan_all(&mut query, &mut url, None, None);
        assert_eq!(QueryStatus::Success, status);
        assert_eq!("http://h/a?x=1&y=2#frag", url.spec());
        assert_eq!("ModPagespeed=off", query.pagespeed_query_params().to_escaped_string());
    }

    #[test]
    fn headers_consumed_selectively() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a");
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("ModPagespeed", "off");
        headers.headers_mut().add("X-Unrelated", "keep");
        headers.headers_mut().add("X-PSA-Client-Options", "bogus");
        let status = scan_all(&mut query, &mut url, Some(&mut headers), None);
        assert_eq!(QueryStatus::Success, status);
        assert!(!headers.headers().has("ModPagespeed"));
        assert!(headers.headers().has("X-Unrelated"));
        assert!(!headers.headers().has("X-PSA-Client-Options"));
    }

    #[test]
    fn invalid_short_circuits() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeed=bogus");
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("ModPagespeedFilters", "remove_quotes");
        let status = scan_all(&mut query, &mut url, Some(&mut headers), None);
        assert_eq!(QueryStatus::Invalid, status);
        assert!(query.options().is_none());
        // The header scan never ran: nothing consumed.
        assert!(headers.headers().has("ModPagespeedFilters"));
    }

    #[test]
    fn override_token_gates() {
        // Token configured but absent: the bare switch still works.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?PageSpeed=off");
        let mut handler = NullMessageHandler;
        let status = query.scan(false, true, "sekret", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::Success, status);
        // ...but option params are ignored and left in place.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?PageSpeedFilters=remove_quotes");
        let mut handler = NullMessageHandler;
        let status = query.scan(false, true, "sekret", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::NoneFound, status);
        assert!(query.options().is_none());
        assert_eq!("http://h/a?PageSpeedFilters=remove_quotes", url.spec());
        // A matching token unlocks the params (the token itself stays).
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new(
            "http://h/a?ModPagespeedRequestOptionOverride=sekret&PageSpeedFilters=remove_quotes",
        );
        let mut handler = NullMessageHandler;
        let status = query.scan(false, true, "sekret", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::Success, status);
        assert!(query.options().unwrap().enabled(Filter::RemoveQuotes));
        assert_eq!(
            "http://h/a?ModPagespeedRequestOptionOverride=sekret",
            url.spec()
        );
        // A wrong token denies.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new(
            "http://h/a?ModPagespeedRequestOptionOverride=nope&PageSpeedFilters=remove_quotes",
        );
        let mut handler = NullMessageHandler;
        let status = query.scan(false, true, "sekret", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::NoneFound, status);
        // Repeated tokens deny (exactly-one lookup, like C++).
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new(
            "http://h/a?ModPagespeedRequestOptionOverride=sekret&ModPagespeedRequestOptionOverride=sekret&PageSpeedFilters=remove_quotes",
        );
        let mut handler = NullMessageHandler;
        let status = query.scan(false, true, "sekret", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::NoneFound, status);
    }

    #[test]
    fn xhr_disables_ajax_filters() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a");
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("X-Requested-With", "XMLHttpRequest");
        let status = scan_all(&mut query, &mut url, Some(&mut headers), None);
        assert_eq!(QueryStatus::Success, status);
        let options = query.options().unwrap();
        assert!(!options.enabled(Filter::LazyloadImages));
        assert!(!options.enabled(Filter::AddHead));
        assert!(options.css_preserve_urls());
        assert!(options.image_preserve_urls());
        assert!(options.js_preserve_urls());
    }

    #[test]
    fn cookies_apply_and_record() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a");
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("Cookie", "ModPagespeed=off; other=1");
        let status = scan_all(&mut query, &mut url, Some(&mut headers), None);
        assert_eq!(QueryStatus::Success, status);
        assert_eq!(EnabledState::Off, query.options().unwrap().enabled_state());
        assert_eq!("ModPagespeed=off", query.pagespeed_option_cookies().to_escaped_string());
        // Mangled cookies are ignored with an informational message.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a");
        let mut headers = RequestHeaders::new();
        headers.headers_mut().add("Cookie", "ModPagespeed=a b");
        let mut handler = MockMessageHandler::new();
        let status = query.scan(false, true, "", None, &mut url, Some(&mut headers), None, &mut handler);
        assert_eq!(QueryStatus::NoneFound, status);
        assert_eq!(1, handler.messages().len());
        assert_eq!(MessageLevel::Info, handler.messages()[0].0);
        assert!(handler.messages()[0].1.contains("mangled"));
    }

    #[test]
    fn sticky_needs_context() {
        let mut context =
            RequestContext::new(crate::response::HttpOptions::default_for_tests());
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeedStickyQueryParameters=tok");
        let mut handler = NullMessageHandler;
        let status = query.scan(
            false,
            true,
            "",
            Some(&mut context),
            &mut url,
            None,
            None,
            &mut handler,
        );
        assert_eq!(QueryStatus::Success, status);
        assert_eq!(Some("tok"), context.sticky_query_parameters_token());
        assert_eq!("http://h/a", url.spec());
        // Without a context the param is left alone.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?ModPagespeedStickyQueryParameters=tok");
        let status = scan_all(&mut query, &mut url, None, None);
        assert_eq!(QueryStatus::NoneFound, status);
        assert_eq!("http://h/a?ModPagespeedStickyQueryParameters=tok", url.spec());
    }

    #[test]
    fn response_token_always_stripped() {
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a");
        let mut response =
            ResponseHeaders::new(crate::response::HttpOptions::default_for_tests());
        response.headers_mut().add("PageSpeedRequestOptionOverride", "sekret");
        let status = scan_all(&mut query, &mut url, None, Some(&mut response));
        assert_eq!(QueryStatus::NoneFound, status);
        assert!(!response.headers().has("PageSpeedRequestOptionOverride"));
    }

    fn resource_url_with_options(id: &str, options: &str) -> String {
        let mut namer = ResourceNamer::new();
        namer.set_id(id);
        namer.set_name("image");
        namer.set_hash("0123456789");
        namer.set_options(options);
        namer.set_ext("jpg");
        format!("http://h/{}", namer.encode())
    }

    #[test]
    fn related_options_apply() {
        // Disabled by default: options segments ignored.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new(&resource_url_with_options("ic", "rj+iq=70"));
        let status = scan_all(&mut query, &mut url, None, None);
        assert_eq!(QueryStatus::NoneFound, status);
        // Enabled: the filter enables and the option sets.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new(&resource_url_with_options("ic", "rj+iq=70"));
        let mut handler = NullMessageHandler;
        let status = query.scan(true, true, "", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::Success, status);
        let options = query.options().unwrap();
        assert!(options.enabled(Filter::RecompressJpeg));
        assert!(!options.enabled(Filter::CombineCss));
        assert_eq!(Some(Value::Int(70)), options.get_by_name("JpegRecompressionQuality"));
        // Query params override the embedded options.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new(&format!(
            "{}?PageSpeedJpegRecompressionQuality=71",
            resource_url_with_options("ic", "rj+iq=70")
        ));
        let mut handler = NullMessageHandler;
        let status = query.scan(true, true, "", None, &mut url, None, None, &mut handler);
        assert_eq!(QueryStatus::Success, status);
        assert_eq!(
            Some(Value::Int(71)),
            query.options().unwrap().get_by_name("JpegRecompressionQuality")
        );
    }

    #[test]
    fn resource_option_units() {
        let (filters, related) = related_tables_for_decoding_id("ic").unwrap();
        // Filter enable + option set.
        let mut options = RewriteOptions::new();
        assert_eq!(
            QueryStatus::Success,
            parse_resource_option("rj+iq=70", &mut options, filters, related)
        );
        assert!(options.enabled(Filter::RecompressJpeg));
        assert_eq!(Some(Value::Int(70)), options.get_by_name("JpegRecompressionQuality"));
        // Unknown filter / option / shape invalidate.
        for bad in ["zz", "rj+zz=1", "rj+iq=70+x", "a=b=c"] {
            let mut options = RewriteOptions::new();
            assert_eq!(
                QueryStatus::Invalid,
                parse_resource_option(bad, &mut options, filters, related),
                "{bad}"
            );
        }
        // Empty value parses to nothing.
        let mut options = RewriteOptions::new();
        assert_eq!(
            QueryStatus::NoneFound,
            parse_resource_option("", &mut options, filters, related)
        );
    }

    #[test]
    fn get_query_options_wraps_scan() {
        // Invalid URL fails with an error message.
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("not a url");
        let mut handler = MockMessageHandler::new();
        let domain = RewriteOptions::new();
        assert!(!get_query_options(&domain, None, &mut url, None, None, &mut query, &mut handler));
        assert_eq!(1, handler.messages().len());
        assert_eq!(MessageLevel::Error, handler.messages()[0].0);
        // Thresholds cap at the domain ceiling.
        let mut domain = RewriteOptions::new();
        domain.set_option("CssInlineMaxBytes", "100", Scope::ProcessStrict).unwrap();
        let mut query = RewriteQuery::new();
        let mut url = GoogleUrl::new("http://h/a?PageSpeedCssInlineMaxBytes=99999");
        let mut handler = NullMessageHandler;
        assert!(get_query_options(&domain, None, &mut url, None, None, &mut query, &mut handler));
        assert_eq!(
            Some(Value::Int(100)),
            query.options().unwrap().get_by_name("CssInlineMaxBytes")
        );
    }
}
