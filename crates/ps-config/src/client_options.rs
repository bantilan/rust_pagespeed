//! Downstream proxy client options (`X-PSA-Client-Options`).
//!
//! A downstream proxy announces its preferences as comma-separated
//! `name=value` pairs (`v=1,m=2,iqp=1`): a version plus an optional
//! proxy mode and image-quality preference. Only version `1` is
//! understood; unknown pairs are ignored.

use crate::filter::{Filter, FilterSet, RewriteLevel};
use crate::options::RewriteOptions;

/// Proxy mode requested by the downstream client (`ProxyMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// The server operates in its default mode.
    Default,
    /// No image is transformed.
    NoImageTransform,
    /// No resource is transformed (like `?PageSpeedFilters=`).
    NoTransform,
}

impl ProxyMode {
    /// Parses a mode number (`ParseProxyMode`).
    fn parse(text: Option<&str>) -> Option<ProxyMode> {
        let text = text?;
        if text.is_empty() {
            return None;
        }
        match text.parse::<i64>() {
            Ok(0) => Some(ProxyMode::Default),
            Ok(1) => Some(ProxyMode::NoImageTransform),
            Ok(2) => Some(ProxyMode::NoTransform),
            _ => None,
        }
    }
}

/// Image-quality preference (`ImageQualityPreference`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageQualityPreference {
    /// The server uses its own default image quality.
    Default,
    /// The request asks for low image quality.
    Low,
    /// The request asks for medium image quality.
    Medium,
    /// The request asks for high image quality.
    High,
}

impl ImageQualityPreference {
    /// Parses a preference number (`ParseImageQualityPreference`).
    fn parse(text: Option<&str>) -> Option<ImageQualityPreference> {
        let text = text?;
        if text.is_empty() {
            return None;
        }
        match text.parse::<i64>() {
            Ok(0) => Some(ImageQualityPreference::Default),
            Ok(1) => Some(ImageQualityPreference::Low),
            Ok(2) => Some(ImageQualityPreference::Medium),
            Ok(3) => Some(ImageQualityPreference::High),
            _ => None,
        }
    }
}

/// Parses one header value (`ParseClientOptions`): the proxy
/// mode plus the quality preference, or `None` when the value
/// is absent, repeated, or carries an unsupported version.
/// A valid version with bad options still parses, leaving the
/// offending preference at its default.
#[must_use]
pub fn parse_client_options(header: Option<&str>) -> Option<(ProxyMode, ImageQualityPreference)> {
    let header = header?;
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for pair in header.split(',').filter(|pair| !pair.is_empty()) {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        pairs.push((name, value));
    }
    let lookup1 = |name: &str| -> Option<&str> {
        let mut found = None;
        for (key, value) in &pairs {
            if *key == name {
                if found.is_some() {
                    return None;
                }
                found = Some(*value);
            }
        }
        found
    };
    // Only version 1 is supported for now. New supported versions
    // might be added later.
    if lookup1("v") != Some("1") {
        return None;
    }
    let mut mode = ProxyMode::Default;
    let mut quality = ImageQualityPreference::Default;
    if let Some(parsed) = ProxyMode::parse(lookup1("m")) {
        mode = parsed;
    }
    if mode == ProxyMode::Default {
        if let Some(parsed) = ImageQualityPreference::parse(lookup1("iqp")) {
            quality = parsed;
        }
    }
    Some((mode, quality))
}

/// Filters disabled for image-conservative clients
/// (`ImageRewriteFilter::kRelatedFilters`).
pub const IMAGE_RELATED_FILTERS: &[Filter] = &[
    Filter::ConvertGifToPng,
    Filter::ConvertJpegToProgressive,
    Filter::ConvertJpegToWebp,
    Filter::ConvertToAvif,
    Filter::ConvertPngToJpeg,
    Filter::ConvertToWebpAnimated,
    Filter::ConvertToWebpLossless,
    Filter::JpegSubsampling,
    Filter::RecompressJpeg,
    Filter::RecompressPng,
    Filter::RecompressWebp,
    Filter::ResizeImages,
    Filter::ResizeMobileImages,
    Filter::StripImageColorProfile,
    Filter::StripImageMetaData,
];

impl RewriteOptions {
    /// Disables every filter (`DisableAllFilters`).
    pub fn disable_all_filters(&mut self) {
        self.invalidate_signature();
        *self.filters_mut().enabled_set_mut() = FilterSet::new();
        self.filters_mut().set_level(RewriteLevel::PassThrough);
        for filter in Filter::ALL {
            self.disable_filter(filter);
        }
    }

    /// Disables the image-rewrite filters
    /// (`ImageRewriteFilter::DisableRelatedFilters`).
    pub fn disable_image_related_filters(&mut self) {
        self.invalidate_signature();
        for filter in IMAGE_RELATED_FILTERS {
            self.disable_filter(*filter);
        }
    }

    /// Applies downstream client options
    /// (`UpdateRewriteOptionsWithClientOptions`): false when the
    /// header is absent or asks for default handling.
    pub fn update_with_client_options(&mut self, header: Option<&str>) -> bool {
        self.invalidate_signature();
        let Some((mode, _quality)) = parse_client_options(header) else {
            return false;
        };
        match mode {
            ProxyMode::NoTransform => {
                self.disable_all_filters();
                true
            }
            ProxyMode::NoImageTransform => {
                self.disable_image_related_filters();
                true
            }
            ProxyMode::Default => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Ports of RewriteQueryTest ClientOptions* (parse half).
    #[test]
    fn empty_header_does_not_parse() {
        assert_eq!(None, parse_client_options(None));
    }

    #[test]
    fn pairs_parse_regardless_of_order() {
        assert_eq!(
            Some((ProxyMode::Default, ImageQualityPreference::Medium)),
            parse_client_options(Some("v=1,iqp=2,m=0"))
        );
        // The order of name-value pairs does not matter.
        // Not-supported parts are ignored.
        assert_eq!(
            Some((ProxyMode::Default, ImageQualityPreference::High)),
            parse_client_options(Some("m=0,iqp=3,v=1,xyz=100,zyx=,yzx"))
        );
    }

    #[test]
    fn non_default_mode_ignores_quality() {
        // Image quality is ignored if mode is not Default.
        assert_eq!(
            Some((ProxyMode::NoImageTransform, ImageQualityPreference::Default)),
            parse_client_options(Some("v=1,iqp=2,m=1"))
        );
    }

    #[test]
    fn valid_version_with_bad_options_keeps_defaults() {
        assert_eq!(
            Some((ProxyMode::Default, ImageQualityPreference::Default)),
            parse_client_options(Some("v=1,iqp=2m=1,iqp="))
        );
    }

    #[test]
    fn invalid_version_does_not_parse() {
        assert_eq!(None, parse_client_options(Some("iqp=2,m=1,v=2")));
        assert_eq!(None, parse_client_options(Some("iqp=2,m=1")));
    }

    #[test]
    fn repeated_names_do_not_parse() {
        assert_eq!(None, parse_client_options(Some("v=1,v=1,m=0")));
    }

    #[test]
    fn update_applies_proxy_modes() {
        let mut options = RewriteOptions::new();
        assert!(!options.update_with_client_options(None));
        assert!(!options.update_with_client_options(Some("v=1,m=0")));
        let mut options = RewriteOptions::new();
        assert!(options.update_with_client_options(Some("v=1,m=2")));
        assert!(!options.enabled(Filter::RewriteCss));
        assert!(!options.enabled(Filter::ConvertGifToPng));

        let mut options = RewriteOptions::new();
        assert!(options.update_with_client_options(Some("v=1,m=1")));
        assert!(!options.enabled(Filter::ConvertGifToPng));
        assert!(!options.enabled(Filter::ConvertPngToJpeg));
    }
}
