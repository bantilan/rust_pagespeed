//! Cache variant keys, rewritten URL extensions, and Vary rules.
//!
//! Ports `ImageUrlEncoder::CacheKeyFromResourceContext` (the per-UA variant
//! key: webp level + `.avif` + mobile + save-data/small-screen quality
//! tiers), the output extension mapping behind rewritten image URLs, and the
//! `InPlaceRewriteContext::AddVaryIfRequired` image branch (Vary: User-Agent
//! vs Vary: Accept selection, Save-Data augmentation, and the IE
//! cache-private rule).

use crate::format::ImageType;
use crate::negotiate::LibWebpLevel;

/// Variant-key fragments (`kWebp*UserAgentKey`, `kMobileUserAgentKey`, ...).
pub const WEBP_NONE_KEY: &str = ".";
pub const WEBP_LOSSY_KEY: &str = "w";
pub const WEBP_LOSSLESS_ALPHA_KEY: &str = "v";
pub const WEBP_ANIMATED_KEY: &str = "a";
pub const AVIF_KEY: &str = ".avif";
pub const MOBILE_KEY: &str = "m";
pub const SAVE_DATA_KEY: &str = "d";
pub const SMALL_SCREEN_KEY: &str = "ss";

/// Inputs to the variant cache key.
#[derive(Debug, Clone, Copy, Default)]
pub struct VariantContext {
    pub libwebp_level: LibWebpLevel,
    pub supports_avif: bool,
    pub mobile_user_agent: bool,
    /// Save-Data quality may apply (wins over small-screen).
    pub may_use_save_data_quality: bool,
    pub may_use_small_screen_quality: bool,
}

/// Builds the UA variant cache key (`CacheKeyFromResourceContext`).
#[must_use]
pub fn variant_cache_key(ctx: VariantContext) -> String {
    let mut key = String::new();
    key.push_str(match ctx.libwebp_level {
        LibWebpLevel::None => WEBP_NONE_KEY,
        LibWebpLevel::LossyOnly => WEBP_LOSSY_KEY,
        LibWebpLevel::LossyLosslessAlpha => WEBP_LOSSLESS_ALPHA_KEY,
        LibWebpLevel::Animated => WEBP_ANIMATED_KEY,
    });
    if ctx.supports_avif {
        key.push_str(AVIF_KEY);
    }
    if ctx.mobile_user_agent {
        key.push_str(MOBILE_KEY);
    }
    if ctx.may_use_save_data_quality {
        key.push_str(SAVE_DATA_KEY);
    } else if ctx.may_use_small_screen_quality {
        key.push_str(SMALL_SCREEN_KEY);
    }
    key
}

/// Rewritten image URL extension for an output type.
///
/// Matches the `.pagespeed.ic.<hash>.<ext>` suffix scheme: JPEG output keeps
/// `jpg`, WebP outputs (lossy, lossless, animated) share `webp`, AVIF uses
/// `avif`. Unknown types have no rewritten form.
#[must_use]
pub fn rewritten_extension(image_type: ImageType) -> Option<&'static str> {
    image_type.extension()
}

/// Filter id of the image compression filter (`ic`), used to recognize
/// image-rewritten URLs.
pub const IMAGE_COMPRESSION_ID: &str = "ic";

/// Inputs to the Vary decision (the option and request predicates behind
/// `AddVaryIfRequired` for images).
#[derive(Debug, Clone, Copy, Default)]
pub struct VaryInputs {
    /// In-place browser optimization with a single output partition.
    pub in_place_single_partition: bool,
    /// The optimized image type (unknown = not optimized).
    pub optimized_type: Option<ImageType>,
    /// Any conversion/small-screen filter that makes output UA-dependent.
    pub conversion_enabled: bool,
    /// Lossy conversion specifically (jpeg→webp or →avif).
    pub lossy_conversion_enabled: bool,
    /// Save-Data quality support is on.
    pub support_save_data: bool,
    /// Vary-on-User-Agent allowed (option or auto without Via header).
    pub allow_vary_on_user_agent: bool,
    /// Vary-on-Accept allowed (option or auto with Via header).
    pub allow_vary_on_accept: bool,
    /// Serve cache-private to IE instead of Varying.
    pub private_not_vary_for_ie: bool,
    /// Request came from IE.
    pub is_ie: bool,
    /// Existing Vary header values (for the merge rule).
    pub existing_vary: Option<VaryPresent>,
}

/// Which Vary coverage already exists on the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaryPresent {
    /// `Vary: *` or `Vary: User-Agent` (covers everything).
    StarOrUserAgent,
    /// `Vary: Accept` (covers image needs).
    Accept,
    /// Anything else (or absent).
    Other,
}

/// Classifies existing Vary header values for the merge rule.
///
/// Each VALUE is compared whole (case-insensitive); comma-joined values do
/// not split, exactly like the C++ loop.
#[must_use]
pub fn classify_existing_vary(values: &[&str]) -> VaryPresent {
    let mut found_accept = false;
    for value in values {
        if *value == "*" || value.eq_ignore_ascii_case("User-Agent") {
            return VaryPresent::StarOrUserAgent;
        }
        if value.eq_ignore_ascii_case("Accept") {
            found_accept = true;
        }
    }
    if found_accept {
        VaryPresent::Accept
    } else {
        VaryPresent::Other
    }
}

/// Vary decision outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaryDecision {
    /// No header change needed.
    None,
    /// Add this Vary value.
    Vary(String),
    /// IE rule: mark cache-private instead of varying.
    PrivateForIe,
}

/// Computes the image Vary decision (`AddVaryIfRequired`, image branch).
#[must_use]
pub fn vary_for_image(inputs: VaryInputs) -> VaryDecision {
    if !inputs.in_place_single_partition {
        return VaryDecision::None;
    }
    let Some(image_type) = inputs.optimized_type else {
        return VaryDecision::None;
    };
    if image_type == ImageType::Unknown {
        return VaryDecision::None;
    }
    let mut new_vary = String::new();
    if inputs.allow_vary_on_user_agent && inputs.conversion_enabled {
        // Any UA-dependent conversion (format or small-screen quality).
        new_vary.push_str("User-Agent");
    } else if inputs.allow_vary_on_accept
        && matches!(
            image_type,
            ImageType::Jpeg | ImageType::Webp | ImageType::Avif
        )
        && inputs.lossy_conversion_enabled
    {
        // Lossy output whose quality/format came from the Accept header.
        new_vary.push_str("Accept");
    }
    let depends_on_save_data = matches!(
        image_type,
        ImageType::Jpeg | ImageType::Webp | ImageType::Avif | ImageType::WebpAnimated
    );
    if depends_on_save_data && inputs.support_save_data {
        if !new_vary.is_empty() {
            new_vary.push(',');
        }
        new_vary.push_str("Save-Data");
    }
    if new_vary.is_empty() {
        return VaryDecision::None;
    }
    if inputs.private_not_vary_for_ie && inputs.is_ie {
        return VaryDecision::PrivateForIe;
    }
    match inputs.existing_vary {
        Some(VaryPresent::StarOrUserAgent) | Some(VaryPresent::Accept) => VaryDecision::None,
        _ => VaryDecision::Vary(new_vary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_keys_match_cpp() {
        assert_eq!(variant_cache_key(VariantContext::default()), ".");
        let ctx = VariantContext {
            libwebp_level: LibWebpLevel::LossyOnly,
            supports_avif: true,
            mobile_user_agent: true,
            may_use_save_data_quality: false,
            may_use_small_screen_quality: true,
        };
        assert_eq!(variant_cache_key(ctx), "w.avifmss");
        // Save-Data wins over small-screen.
        let ctx = VariantContext {
            may_use_save_data_quality: true,
            may_use_small_screen_quality: true,
            ..Default::default()
        };
        assert_eq!(variant_cache_key(ctx), ".d");
        assert_eq!(
            variant_cache_key(VariantContext {
                libwebp_level: LibWebpLevel::Animated,
                ..Default::default()
            }),
            "a"
        );
    }

    #[test]
    fn extensions_follow_output_type() {
        assert_eq!(rewritten_extension(ImageType::Jpeg), Some("jpg"));
        assert_eq!(rewritten_extension(ImageType::Png), Some("png"));
        assert_eq!(rewritten_extension(ImageType::Gif), Some("gif"));
        assert_eq!(rewritten_extension(ImageType::WebpAnimated), Some("webp"));
        assert_eq!(rewritten_extension(ImageType::Avif), Some("avif"));
        assert_eq!(rewritten_extension(ImageType::Unknown), None);
    }

    #[test]
    fn vary_selection_rules() {
        let base = VaryInputs {
            in_place_single_partition: true,
            optimized_type: Some(ImageType::Webp),
            conversion_enabled: true,
            lossy_conversion_enabled: true,
            allow_vary_on_user_agent: true,
            ..Default::default()
        };
        // User-Agent wins when allowed.
        assert_eq!(
            vary_for_image(base),
            VaryDecision::Vary("User-Agent".to_string())
        );
        // Otherwise Accept for lossy output.
        let accept = VaryInputs {
            allow_vary_on_user_agent: false,
            allow_vary_on_accept: true,
            ..base
        };
        assert_eq!(
            vary_for_image(accept),
            VaryDecision::Vary("Accept".to_string())
        );
        // PNG output never varies on Accept.
        let png = VaryInputs {
            optimized_type: Some(ImageType::Png),
            ..accept
        };
        assert_eq!(vary_for_image(png), VaryDecision::None);
        // Save-Data appends.
        let save_data = VaryInputs {
            support_save_data: true,
            ..accept
        };
        assert_eq!(
            vary_for_image(save_data),
            VaryDecision::Vary("Accept,Save-Data".to_string())
        );
        // Existing coverage suppresses the header.
        let covered = VaryInputs {
            existing_vary: Some(VaryPresent::Accept),
            ..accept
        };
        assert_eq!(vary_for_image(covered), VaryDecision::None);
        // IE gets cache-private instead.
        let ie = VaryInputs {
            private_not_vary_for_ie: true,
            is_ie: true,
            ..accept
        };
        assert_eq!(vary_for_image(ie), VaryDecision::PrivateForIe);
    }

    #[test]
    fn vary_classification_is_whole_value() {
        assert_eq!(classify_existing_vary(&["*"]), VaryPresent::StarOrUserAgent);
        assert_eq!(
            classify_existing_vary(&["user-agent"]),
            VaryPresent::StarOrUserAgent
        );
        assert_eq!(classify_existing_vary(&["Accept"]), VaryPresent::Accept);
        // Comma-joined values do not split.
        assert_eq!(
            classify_existing_vary(&["Accept, Encoding"]),
            VaryPresent::Other
        );
        assert_eq!(classify_existing_vary(&[]), VaryPresent::Other);
    }
}
