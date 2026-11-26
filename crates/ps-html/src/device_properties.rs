//! Device capability properties (`DeviceProperties`).
//!
//! User-agent sniffing plus request-header parsing (Accept,
//! Save-Data, Via) answering what the client supports. Values are
//! computed eagerly when the UA/headers are set (C++ caches them
//! lazily; same observable).

use ps_http::bot_checker::is_bot_user_agent;
use ps_http::names::header;
use ps_http::request::RequestHeaders;
use ps_http::user_agent_matcher::{DeviceType, UserAgentMatcher};

/// Device capabilities (`DeviceProperties`).
#[derive(Debug, Clone)]
pub struct DeviceProperties {
    matcher: &'static UserAgentMatcher,
    user_agent: String,
    supports_image_inlining: bool,
    supports_lazyload_images: bool,
    supports_dns_prefetch: bool,
    supports_dns_prefetch_using_rel_prefetch: bool,
    supports_critical_css: bool,
    supports_js_defer_mobile: bool,
    supports_js_defer_desktop: bool,
    requests_save_data: bool,
    accepts_webp: bool,
    accepts_avif: bool,
    avif_explicitly_rejected: bool,
    webp_explicitly_rejected: bool,
    accepts_gzip: bool,
    has_via_header: bool,
    headers_parsed: bool,
}

impl DeviceProperties {
    /// Builds empty properties.
    #[must_use]
    pub fn new() -> Self {
        DeviceProperties {
            matcher: UserAgentMatcher::shared(),
            user_agent: String::new(),
            supports_image_inlining: false,
            supports_lazyload_images: false,
            supports_dns_prefetch: false,
            supports_dns_prefetch_using_rel_prefetch: false,
            supports_critical_css: false,
            supports_js_defer_mobile: false,
            supports_js_defer_desktop: false,
            requests_save_data: false,
            accepts_webp: false,
            accepts_avif: false,
            avif_explicitly_rejected: false,
            webp_explicitly_rejected: false,
            accepts_gzip: false,
            has_via_header: false,
            headers_parsed: false,
        }
    }

    /// Installs the user agent, recomputing UA-derived values.
    pub fn set_user_agent(&mut self, user_agent: &str) {
        self.user_agent = user_agent.to_string();
        let ua = self.user_agent.as_str();
        self.supports_image_inlining = self.matcher.supports_image_inlining(ua);
        // Currently CriticalSelectorFilter can't deal with IE
        // conditional comments, so we disable ourselves for IE.
        self.supports_critical_css = !self.matcher.is_ie(ua);
        self.supports_js_defer_mobile = self.matcher.supports_js_defer(ua, true);
        self.supports_js_defer_desktop = self.matcher.supports_js_defer(ua, false);
        self.supports_lazyload_images = !self.is_bot() && self.matcher.supports_lazyload_images(ua);
        self.supports_dns_prefetch = self.matcher.supports_dns_prefetch(ua);
        self.supports_dns_prefetch_using_rel_prefetch =
            self.matcher.supports_dns_prefetch_using_rel_prefetch(ua);
    }

    /// Parses request headers (`ParseRequestHeaders`).
    pub fn parse_request_headers(&mut self, headers: &RequestHeaders) {
        self.accepts_webp = accepts_image_format(headers, "image/webp");
        self.accepts_avif = accepts_image_format(headers, "image/avif");
        self.avif_explicitly_rejected = explicitly_rejects_image_format(headers, "image/avif");
        self.webp_explicitly_rejected = explicitly_rejects_image_format(headers, "image/webp");
        self.accepts_gzip = headers.headers().has_value(header::ACCEPT_ENCODING, "gzip");
        self.requests_save_data = headers
            .headers()
            .lookup1(header::SAVE_DATA)
            .is_some_and(|value| value.eq_ignore_ascii_case("on"));
        self.has_via_header = headers.headers().has(header::VIA);
        self.headers_parsed = true;
    }

    /// Whether gzip is accepted.
    #[must_use]
    pub fn accepts_gzip(&self) -> bool {
        self.accepts_gzip
    }

    /// Whether image inlining is supported.
    #[must_use]
    pub fn supports_image_inlining(&self) -> bool {
        self.supports_image_inlining
    }

    /// Whether lazyloaded images are supported.
    #[must_use]
    pub fn supports_lazyload_images(&self) -> bool {
        self.supports_lazyload_images
    }

    /// Whether DNS prefetch is supported.
    #[must_use]
    pub fn supports_dns_prefetch(&self) -> bool {
        self.supports_dns_prefetch
    }

    /// Whether DNS prefetch uses `rel=prefetch` (IE9).
    #[must_use]
    pub fn supports_dns_prefetch_using_rel_prefetch(&self) -> bool {
        self.supports_dns_prefetch_using_rel_prefetch
    }

    /// Whether critical CSS is supported.
    #[must_use]
    pub fn supports_critical_css(&self) -> bool {
        self.supports_critical_css
    }

    /// Whether the critical-images beacon is supported.
    #[must_use]
    pub fn supports_critical_images_beacon(&self) -> bool {
        // For now this script has the same user agent requirements as
        // image inlining. Also disable for bots.
        self.supports_image_inlining() && !self.is_bot()
    }

    /// Whether deferred JS is supported.
    #[must_use]
    pub fn supports_js_defer(&self, allow_mobile: bool) -> bool {
        if allow_mobile {
            self.supports_js_defer_mobile
        } else {
            self.supports_js_defer_desktop
        }
    }

    /// Whether in-place webp is supported.
    #[must_use]
    pub fn supports_webp_in_place(&self) -> bool {
        self.accepts_webp
    }

    /// Whether webp rewritten URLs are supported.
    #[must_use]
    pub fn supports_webp_rewritten_urls(&self) -> bool {
        !self.webp_explicitly_rejected
            && (self.accepts_webp || self.matcher.supports_webp_rewritten_urls(&self.user_agent))
    }

    /// Whether AVIF rewritten URLs are supported.
    #[must_use]
    pub fn supports_avif_rewritten_urls(&self) -> bool {
        !self.avif_explicitly_rejected
            && !self.matcher.is_known_avif_unsupported(&self.user_agent)
            && (self.accepts_avif || self.matcher.supports_avif_rewritten_urls(&self.user_agent))
    }

    /// Whether lossless-alpha webp is supported.
    #[must_use]
    pub fn supports_webp_lossless_alpha(&self) -> bool {
        self.supports_webp_rewritten_urls()
            && self.matcher.supports_webp_lossless_alpha(&self.user_agent)
    }

    /// Whether animated webp is supported.
    #[must_use]
    pub fn supports_webp_animated(&self) -> bool {
        self.accepts_webp && self.matcher.supports_webp_animated(&self.user_agent)
    }

    /// Whether the agent is a bot.
    #[must_use]
    pub fn is_bot(&self) -> bool {
        is_bot_user_agent(&self.user_agent)
    }

    /// Whether the agent is mobile.
    #[must_use]
    pub fn is_mobile(&self) -> bool {
        self.device_type() == DeviceType::Mobile
    }

    /// Whether the agent is a tablet.
    #[must_use]
    pub fn is_tablet(&self) -> bool {
        self.device_type() == DeviceType::Tablet
    }

    /// Classifies the device.
    #[must_use]
    pub fn device_type(&self) -> DeviceType {
        self.matcher.device_type_for_ua(&self.user_agent)
    }

    /// Whether webp inlining is forbidden (Chrome 36/37 on iOS).
    #[must_use]
    pub fn forbid_webp_inlining(&self) -> bool {
        // Chrome 36 on iOS devices failed to display inlined WebP
        // image, so inlining WebP on these devices is forbidden.
        if self.matcher.is_ios_user_agent(&self.user_agent) {
            if let Some((major, _, _, _)) = self.matcher.chrome_build_number(&self.user_agent) {
                return major == 36 || major == 37;
            }
        }
        false
    }

    /// Whether Save-Data was requested.
    #[must_use]
    pub fn requests_save_data(&self) -> bool {
        self.requests_save_data
    }

    /// Whether a Via header is present.
    #[must_use]
    pub fn has_via_header(&self) -> bool {
        self.has_via_header
    }
}

impl Default for DeviceProperties {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether Accept advertises a mime type with nonzero quality.
pub(crate) fn accepts_image_format(headers: &RequestHeaders, mime_type: &str) -> bool {
    let Some(values) = headers.headers().lookup(header::ACCEPT) else {
        return false;
    };
    for value in &values {
        for entry in value.split(',') {
            let parts: Vec<&str> = entry.split(';').collect();
            if parts.is_empty() || !trim_http_whitespace(parts[0]).eq_ignore_ascii_case(mime_type) {
                continue;
            }
            let mut quality = 1.0;
            for part in &parts[1..] {
                parse_quality_parameter(part, &mut quality);
            }
            if quality > 0.0 {
                return true;
            }
        }
    }
    false
}

/// Whether Accept explicitly rejects a mime type (`q=0`).
fn explicitly_rejects_image_format(headers: &RequestHeaders, mime_type: &str) -> bool {
    let Some(values) = headers.headers().lookup(header::ACCEPT) else {
        return false;
    };
    for value in &values {
        for entry in value.split(',') {
            let parts: Vec<&str> = entry.split(';').collect();
            if parts.is_empty() || !trim_http_whitespace(parts[0]).eq_ignore_ascii_case(mime_type) {
                continue;
            }
            for part in &parts[1..] {
                let mut quality = 1.0;
                if parse_quality_parameter(part, &mut quality) && quality == 0.0 {
                    return true;
                }
            }
        }
    }
    false
}

/// Trims spaces and tabs (`TrimHttpWhitespace`).
fn trim_http_whitespace(value: &str) -> &str {
    value.trim_matches(|char| char == ' ' || char == '\t')
}

/// Parses a `q=` parameter, storing 0 on any malformation
/// (`ParseQualityParameter`).
fn parse_quality_parameter(parameter: &str, quality: &mut f64) -> bool {
    let Some((name, value)) = parameter.split_once('=') else {
        return false;
    };
    if !trim_http_whitespace(name).eq_ignore_ascii_case("q") {
        return false;
    }
    let parsed: f64 = trim_http_whitespace(value).parse().unwrap_or(f64::NAN);
    if !parsed.is_finite() || parsed < 0.0 || parsed > 1.0 {
        *quality = 0.0;
    } else {
        *quality = parsed;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_navigation_infers_alpha_but_respects_explicit_rejection() {
        let mut device = DeviceProperties::new();
        device.set_user_agent(
            "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:147.0) Gecko/20100101 Firefox/147.0",
        );
        let mut headers = RequestHeaders::new();
        headers
            .headers_mut()
            .add("Accept", "text/html,application/xhtml+xml,*/*;q=0.8");
        device.parse_request_headers(&headers);
        assert!(device.supports_webp_rewritten_urls());
        assert!(device.supports_webp_lossless_alpha());
        // In-place resource negotiation still requires its own explicit Accept.
        assert!(!device.supports_webp_in_place());
        headers
            .headers_mut()
            .replace("Accept", "text/html,image/webp;q=0");
        device.parse_request_headers(&headers);
        assert!(!device.supports_webp_rewritten_urls());
        assert!(!device.supports_webp_lossless_alpha());
        device.set_user_agent("Mozilla/5.0 Firefox/60.0");
        headers.headers_mut().replace("Accept", "text/html,*/*");
        device.parse_request_headers(&headers);
        assert!(!device.supports_webp_lossless_alpha());
    }
}
