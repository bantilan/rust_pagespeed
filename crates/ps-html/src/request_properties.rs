//! Request capability properties (`RequestProperties`).
//!
//! Device sniffing gated by downstream-cache directives. Distinct
//! from the cache-oriented `ps_http` properties of the same C++
//! family name.

use ps_http::request::RequestHeaders;
use ps_http::user_agent_matcher::DeviceType;

use super::device_properties::DeviceProperties;
use super::downstream_caching_directives::DownstreamCachingDirectives;

/// Request capabilities (`RequestProperties`).
#[derive(Debug)]
pub struct RequestProperties {
    device: DeviceProperties,
    downstream: DownstreamCachingDirectives,
}

impl RequestProperties {
    /// Builds empty properties.
    #[must_use]
    pub fn new() -> Self {
        RequestProperties {
            device: DeviceProperties::new(),
            downstream: DownstreamCachingDirectives::new(),
        }
    }

    /// Installs the user agent.
    pub fn set_user_agent(&mut self, user_agent: &str) {
        self.device.set_user_agent(user_agent);
    }

    /// Parses request headers.
    pub fn parse_request_headers(&mut self, headers: &RequestHeaders) {
        self.device.parse_request_headers(headers);
        self.downstream
            .parse_capability_list_from_request_headers(headers);
    }

    /// Whether image inlining is supported.
    #[must_use]
    pub fn supports_image_inlining(&mut self) -> bool {
        self.downstream.supports_image_inlining() && self.device.supports_image_inlining()
    }

    /// Whether lazyloaded images are supported.
    #[must_use]
    pub fn supports_lazyload_images(&mut self) -> bool {
        self.downstream.supports_lazyload_images() && self.device.supports_lazyload_images()
    }

    /// Whether DNS prefetch is supported (device-only, like C++).
    #[must_use]
    pub fn supports_dns_prefetch(&self) -> bool {
        self.device.supports_dns_prefetch()
    }

    /// Whether DNS prefetch uses `rel=prefetch` (IE9, device-only).
    #[must_use]
    pub fn supports_dns_prefetch_using_rel_prefetch(&self) -> bool {
        self.device.supports_dns_prefetch_using_rel_prefetch()
    }

    /// Whether critical CSS is supported.
    #[must_use]
    pub fn supports_critical_css(&self) -> bool {
        self.device.supports_critical_css()
    }

    /// Whether gzip is accepted.
    #[must_use]
    pub fn accepts_gzip(&self) -> bool {
        self.device.accepts_gzip()
    }

    /// Whether the critical-CSS beacon is supported.
    #[must_use]
    pub fn supports_critical_css_beacon(&self) -> bool {
        // For bots, we don't allow instrumentation, but we do allow
        // bots to use previous instrumentation results collected by
        // non-bots.
        self.supports_critical_css() && !self.is_bot()
    }

    /// Whether the critical-images beacon is supported.
    #[must_use]
    pub fn supports_critical_images_beacon(&self) -> bool {
        self.device.supports_critical_images_beacon()
    }

    /// Whether deferred JS is supported.
    #[must_use]
    pub fn supports_js_defer(&mut self, allow_mobile: bool) -> bool {
        self.downstream.supports_js_defer() && self.device.supports_js_defer(allow_mobile)
    }

    /// Whether in-place webp is supported.
    #[must_use]
    pub fn supports_webp_in_place(&mut self) -> bool {
        self.downstream.supports_webp() && self.device.supports_webp_in_place()
    }

    /// Whether webp rewritten URLs are supported.
    #[must_use]
    pub fn supports_webp_rewritten_urls(&mut self) -> bool {
        self.downstream.supports_webp() && self.device.supports_webp_rewritten_urls()
    }

    /// Whether AVIF rewritten URLs are supported.
    #[must_use]
    pub fn supports_avif_rewritten_urls(&self) -> bool {
        self.device.supports_avif_rewritten_urls()
    }

    /// Whether lossless-alpha webp is supported.
    #[must_use]
    pub fn supports_webp_lossless_alpha(&mut self) -> bool {
        self.downstream.supports_webp_lossless_alpha() && self.device.supports_webp_lossless_alpha()
    }

    /// Whether animated webp is supported.
    #[must_use]
    pub fn supports_webp_animated(&mut self) -> bool {
        self.downstream.supports_webp_animated() && self.device.supports_webp_animated()
    }

    /// Whether the agent is a bot.
    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.device.is_bot()
    }

    /// Whether the agent is mobile.
    #[must_use]
    pub fn is_mobile(&self) -> bool {
        self.device.is_mobile()
    }

    /// Whether the agent is a tablet.
    #[must_use]
    pub fn is_tablet(&self) -> bool {
        self.device.is_tablet()
    }

    /// Classifies the device.
    #[must_use]
    pub fn device_type(&self) -> DeviceType {
        self.device.device_type()
    }

    /// Whether webp inlining is forbidden.
    #[must_use]
    pub fn forbid_webp_inlining(&self) -> bool {
        self.device.forbid_webp_inlining()
    }

    /// Whether Save-Data was requested.
    #[must_use]
    pub fn requests_save_data(&self) -> bool {
        self.device.requests_save_data()
    }

    /// Whether a Via header is present.
    #[must_use]
    pub fn has_via_header(&self) -> bool {
        self.device.has_via_header()
    }
}

impl Default for RequestProperties {
    fn default() -> Self {
        Self::new()
    }
}
