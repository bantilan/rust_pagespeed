//! Ad attribute names (`ads_attribute`).
//!
//! ShowAds (`google_*`) and AdSense (`data-*`) attribute vocabularies plus
//! the `google_x_y` → `data-x-y` rename rule.

/// AdSense attribute names.
pub const DATA_AD_CLIENT: &[u8] = b"data-ad-client";
/// AdSense channel attribute.
pub const DATA_AD_CHANNEL: &[u8] = b"data-ad-channel";
/// AdSense slot attribute.
pub const DATA_AD_SLOT: &[u8] = b"data-ad-slot";
/// AdSense format attribute.
pub const DATA_AD_FORMAT: &[u8] = b"data-ad-format";

/// ShowAds client attribute.
pub const GOOGLE_AD_CLIENT: &[u8] = b"google_ad_client";
/// ShowAds channel attribute.
pub const GOOGLE_AD_CHANNEL: &[u8] = b"google_ad_channel";
/// ShowAds slot attribute.
pub const GOOGLE_AD_SLOT: &[u8] = b"google_ad_slot";
/// ShowAds format attribute.
pub const GOOGLE_AD_FORMAT: &[u8] = b"google_ad_format";
/// ShowAds width attribute.
pub const GOOGLE_AD_WIDTH: &[u8] = b"google_ad_width";
/// ShowAds height attribute.
pub const GOOGLE_AD_HEIGHT: &[u8] = b"google_ad_height";
/// ShowAds output attribute.
pub const GOOGLE_AD_OUTPUT: &[u8] = b"google_ad_output";

/// Renames a showads attribute to its adsbygoogle form
/// (`LookupAdsByGoogleAttributeName`): `google` + `_`-separated parts
/// become `data` + `-`-separated parts; anything else maps to empty.
/// Empty parts are kept (no empty-omission in the split).
#[must_use]
pub fn lookup_ads_by_google_attribute_name(show_ads_name: &[u8]) -> Vec<u8> {
    let parts: Vec<&[u8]> = show_ads_name.split(|byte| *byte == b'_').collect();
    if parts.len() < 2 || parts[0] != b"google" {
        return Vec::new();
    }
    let mut out = b"data".to_vec();
    for part in &parts[1..] {
        out.push(b'-');
        out.extend_from_slice(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_basic() {
        let lookup = |name: &[u8]| lookup_ads_by_google_attribute_name(name);
        assert_eq!(lookup(b"google_ad_client"), b"data-ad-client");
        assert_eq!(lookup(b"google_ad_slot"), b"data-ad-slot");
        assert_eq!(lookup(b"google_ad_channel"), b"data-ad-channel");
        assert_eq!(lookup(b"google_language"), b"data-language");
        assert_eq!(lookup(b"google_color_border"), b"data-color-border");
    }

    #[test]
    fn lookup_not_matched() {
        assert!(lookup_ads_by_google_attribute_name(b"elgoog_foo_bar").is_empty());
        assert!(lookup_ads_by_google_attribute_name(b"google").is_empty());
    }
}
