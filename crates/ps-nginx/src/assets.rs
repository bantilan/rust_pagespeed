//! Static-asset serving (`StaticAssetManager::GetAsset`).
//!
//! Serves the vendored support scripts by hashed filename:
//! `<name>[_debug].<HASH>.js` selects debug/opt bytes, the hash
//! decides the long or private cache header. Debug content is the
//! cleartext sources (C++ serves those, not the Closure
//! pretty-prints). Hashes are default-length MD5 like C++.

use ps_cache::hash::{Hasher, Md5Hasher};
use ps_html::static_assets::{get_asset, StaticAsset};

/// Long cache header (year TTL, hash verified).
pub const LONG_CACHE_HEADER: &str = "max-age=31536000";
/// Mismatch header (5-minute TTL, private).
pub const PRIVATE_CACHE_HEADER: &str = "max-age=300,private";
/// Served content type (all assets are JavaScript but the gif).
pub const JAVASCRIPT_MIME: &str = "application/javascript";
/// Blank-gif content type.
pub const GIF_MIME: &str = "image/gif";

/// 1x1 transparent gif (`GIF_blank`, 53 bytes).
pub const BLANK_GIF: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0x21, 0xfe, 0x06, 0x70, 0x73, 0x61, 0x5f, 0x6c, 0x6c, 0x00, 0x21, 0xf9, 0x04,
    0x01, 0x0a, 0x00, 0x01, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02,
    0x02, 0x4c, 0x01, 0x00, 0x3b,
];

/// Served asset: content, MIME type, and cache header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedAsset {
    /// Response bytes.
    pub content: &'static [u8],
    /// MIME type.
    pub content_type: &'static str,
    /// Cache-Control value (long TTL on hash match, private on
    /// mismatch).
    pub cache_header: &'static str,
}

/// Looks up an asset by hashed filename (`GetAsset`). Returns the
/// asset plus whether the name was malformed (C++ logs
/// `Invalid url requested: {name}.` for those).
pub fn get_asset_by_name(file_name: &str) -> Result<ServedAsset, AssetMiss> {
    let parts: Vec<&str> = file_name
        .split('.')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() != 3 {
        return Err(AssetMiss::InvalidName);
    }
    let (stem, debug) = match parts[0].strip_suffix("_debug") {
        Some(stripped) => (stripped, true),
        None => (parts[0], false),
    };
    let (content, is_gif) = match asset_bytes(stem, debug) {
        Some(found) => found,
        None => return Err(AssetMiss::Unknown),
    };
    let content_type = if is_gif { GIF_MIME } else { JAVASCRIPT_MIME };
    let hash = Md5Hasher::new().hash(content);
    let cache_header = if hash == parts[1] {
        LONG_CACHE_HEADER
    } else {
        PRIVATE_CACHE_HEADER
    };
    Ok(ServedAsset {
        content,
        content_type,
        cache_header,
    })
}

/// Asset lookup miss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetMiss {
    /// Malformed name (logged by the caller).
    InvalidName,
    /// Well-formed but unknown (silent, like C++).
    Unknown,
}

/// Looks up servable bytes by plain file name: content plus
/// whether it is the gif (deprecated entries serve empty
/// content, like their unset C++ strings).
fn asset_bytes(name: &str, debug: bool) -> Option<(&'static [u8], bool)> {
    if name == "1" {
        return Some((BLANK_GIF, true));
    }
    if matches!(
        name,
        "deprecated_split_html_beacon" | "deprecated_ghost_click_buster" | "deprecated_blink"
    ) {
        return Some((b"", false));
    }
    let asset = match name {
        "add_instrumentation" => StaticAsset::AddInstrumentation,
        "extended_instrumentation" => StaticAsset::ExtendedInstrumentation,
        "client_domain_rewriter" => StaticAsset::ClientDomainRewriter,
        "critical_css_beacon" => StaticAsset::CriticalCssBeaconJs,
        "critical_css_loader" => StaticAsset::CriticalCssLoaderJs,
        "critical_images_beacon" => StaticAsset::CriticalImagesBeaconJs,
        "dedup_inlined_images" => StaticAsset::DedupInlinedImages,
        "defer_iframe" => StaticAsset::DeferIframe,
        "js_defer" => StaticAsset::DeferJs,
        "delay_images" => StaticAsset::DelayImages,
        "delay_images_inline" => StaticAsset::DelayImagesInline,
        "lazyload_images" => StaticAsset::LazyloadImages,
        "deterministic" => StaticAsset::DeterministicJs,
        "local_storage_cache" => StaticAsset::LocalStorageCacheJs,
        "responsive" => StaticAsset::ResponsiveJs,
        _ => return None,
    };
    Some((get_asset(asset, debug), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashed(stem: &str, debug: bool) -> String {
        let (content, _) = asset_bytes(stem, debug).expect("known");
        format!(
            "{}.{}.js",
            if debug {
                format!("{stem}_debug")
            } else {
                stem.to_string()
            },
            Md5Hasher::new().hash(content)
        )
    }

    #[test]
    fn serves_named_assets() {
        let served = get_asset_by_name(&hashed("deterministic", false)).expect("served");
        assert_eq!(served.content_type, JAVASCRIPT_MIME);
        assert_eq!(served.cache_header, LONG_CACHE_HEADER);
        assert!(!served.content.is_empty());
        // Debug selects the pretty-print genfiles (readable
        // identifiers, no license header).
        let served = get_asset_by_name(&hashed("deterministic", true)).expect("served");
        assert!(!served.content.starts_with(b"/*"));
        assert!(served.content.windows(9).any(|w| w == b"orig_date"));
        // The gif serves bytes with its MIME type.
        let served = get_asset_by_name(&hashed("1", false)).expect("served");
        assert_eq!(served.content_type, GIF_MIME);
        assert_eq!(served.content.len(), 53);
        // Deprecated entries serve empty content.
        let served = get_asset_by_name(&hashed("deprecated_blink", false)).expect("served");
        assert!(served.content.is_empty());
    }

    #[test]
    fn hash_mismatch_goes_private() {
        let served = get_asset_by_name("deterministic.deadbeef00.js").expect("served");
        assert_eq!(served.cache_header, PRIVATE_CACHE_HEADER);
    }

    #[test]
    fn malformed_and_unknown_miss() {
        assert_eq!(
            get_asset_by_name("deterministic.js"),
            Err(AssetMiss::InvalidName)
        );
        assert_eq!(get_asset_by_name("a.b.c.d"), Err(AssetMiss::InvalidName));
        assert_eq!(
            get_asset_by_name("bogus.deadbeef00.js"),
            Err(AssetMiss::Unknown)
        );
    }
}
