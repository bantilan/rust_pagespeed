//! Downstream capability directives
//! (`DownstreamCachingDirectives`).
//!
//! Reads the `PS-CapabilityList` request header naming which
//! rewrites a downstream cache supports; absent means everything.

use ps_config::filter::Filter;
use ps_http::request::RequestHeaders;

/// Capability-list header (`kPsaCapabilityList`).
pub const CAPABILITY_LIST_HEADER: &str = "PS-CapabilityList";

/// Unset marker (`kNoCapabilitiesSpecified`).
const NO_CAPABILITIES_SPECIFIED: &str = "NoCapabilitiesSpecified";

/// Tri-state flag (`LazyBool`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LazyBool {
    /// Uncomputed.
    #[default]
    NotSet,
    /// True.
    True,
    /// False.
    False,
}

/// Downstream capability gates (`DownstreamCachingDirectives`).
#[derive(Debug, Clone, Default)]
pub struct DownstreamCachingDirectives {
    supports_image_inlining: LazyBool,
    supports_js_defer: LazyBool,
    supports_lazyload_images: LazyBool,
    supports_webp: LazyBool,
    supports_webp_lossless_alpha: LazyBool,
    supports_webp_animated: LazyBool,
    capabilities_to_be_supported: String,
}

impl DownstreamCachingDirectives {
    /// Builds unset directives.
    #[must_use]
    pub fn new() -> Self {
        DownstreamCachingDirectives {
            capabilities_to_be_supported: NO_CAPABILITIES_SPECIFIED.to_string(),
            ..DownstreamCachingDirectives::default()
        }
    }

    /// Parses the capability list (`ParseCapabilityListFromRequestHeaders`).
    pub fn parse_capability_list_from_request_headers(&mut self, headers: &RequestHeaders) {
        if let Some(capabilities) = headers.headers().lookup1(CAPABILITY_LIST_HEADER) {
            self.capabilities_to_be_supported = capabilities;
        }
        // Reset everything.
        self.supports_image_inlining = LazyBool::NotSet;
        self.supports_js_defer = LazyBool::NotSet;
        self.supports_lazyload_images = LazyBool::NotSet;
        self.supports_webp = LazyBool::NotSet;
        self.supports_webp_lossless_alpha = LazyBool::NotSet;
        self.supports_webp_animated = LazyBool::NotSet;
    }

    /// Whether a capability is supported (`IsPropertySupported`).
    fn is_property_supported(
        stored: &mut LazyBool,
        capability: &str,
        supported_capabilities: &str,
    ) -> bool {
        if *stored == LazyBool::NotSet {
            *stored = if supported_capabilities == NO_CAPABILITIES_SPECIFIED
                || supported_capabilities == capability
                || supported_capabilities.starts_with(&format!("{capability}:"))
                || supported_capabilities.contains(&format!(",{capability}:"))
                || supported_capabilities.starts_with(&format!("{capability},"))
                || supported_capabilities.contains(&format!(",{capability},"))
            {
                LazyBool::True
            } else {
                LazyBool::False
            };
        }
        *stored == LazyBool::True
    }

    /// Whether image inlining is supported.
    #[must_use]
    pub fn supports_image_inlining(&mut self) -> bool {
        Self::is_property_supported(
            &mut self.supports_image_inlining,
            Filter::InlineImages.id(),
            self.capabilities_to_be_supported.as_str(),
        )
    }

    /// Whether lazyloaded images are supported.
    #[must_use]
    pub fn supports_lazyload_images(&mut self) -> bool {
        Self::is_property_supported(
            &mut self.supports_lazyload_images,
            Filter::LazyloadImages.id(),
            self.capabilities_to_be_supported.as_str(),
        )
    }

    /// Whether deferred JS is supported.
    #[must_use]
    pub fn supports_js_defer(&mut self) -> bool {
        Self::is_property_supported(
            &mut self.supports_js_defer,
            Filter::DeferJavascript.id(),
            self.capabilities_to_be_supported.as_str(),
        )
    }

    /// Whether webp is supported.
    #[must_use]
    pub fn supports_webp(&mut self) -> bool {
        Self::is_property_supported(
            &mut self.supports_webp,
            Filter::ConvertJpegToWebp.id(),
            self.capabilities_to_be_supported.as_str(),
        )
    }

    /// Whether lossless-alpha webp is supported.
    #[must_use]
    pub fn supports_webp_lossless_alpha(&mut self) -> bool {
        Self::is_property_supported(
            &mut self.supports_webp_lossless_alpha,
            Filter::ConvertToWebpLossless.id(),
            self.capabilities_to_be_supported.as_str(),
        )
    }

    /// Whether animated webp is supported.
    #[must_use]
    pub fn supports_webp_animated(&mut self) -> bool {
        Self::is_property_supported(
            &mut self.supports_webp_animated,
            Filter::ConvertToWebpAnimated.id(),
            self.capabilities_to_be_supported.as_str(),
        )
    }
}
