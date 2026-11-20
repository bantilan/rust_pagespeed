//! Static JS assets (`StaticAssetManager::GetAsset`).
//!
//! Byte-exact support scripts injected by filters, selected by asset and
//! debug flag exactly like `GetAsset` (`kDebug` serves the debug build).
//! Bytes live in `assets/` (see its README for provenance).

/// Supplied static asset (`StaticAssetEnum`, partial — grows with the
/// filters that need assets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StaticAsset {
    DeterministicJs,
    DeferIframe,
    DeferJs,
    ResponsiveJs,
    AddInstrumentation,
    ExtendedInstrumentation,
    ClientDomainRewriter,
    CriticalCssBeaconJs,
    CriticalImagesBeaconJs,
    DedupInlinedImages,
    LazyloadImages,
    DelayImages,
    DelayImagesInline,
    LocalStorageCacheJs,
    CriticalCssLoaderJs,
}

/// Serves an asset's bytes (`GetAsset`).
#[must_use]
pub fn get_asset(asset: StaticAsset, debug: bool) -> &'static [u8] {
    match (asset, debug) {
        (StaticAsset::DeterministicJs, false) => {
            include_bytes!("../assets/deterministic_opt.js").as_slice()
        }
        (StaticAsset::DeterministicJs, true) => {
            include_bytes!("../assets/deterministic_dbg.js").as_slice()
        }
        (StaticAsset::DeferIframe, false) => {
            include_bytes!("../assets/defer_iframe_opt.js").as_slice()
        }
        (StaticAsset::DeferIframe, true) => {
            include_bytes!("../assets/defer_iframe_dbg.js").as_slice()
        }
        (StaticAsset::DeferJs, false) => include_bytes!("../assets/js_defer_opt.js").as_slice(),
        (StaticAsset::DeferJs, true) => include_bytes!("../assets/js_defer_dbg.js").as_slice(),
        (StaticAsset::ResponsiveJs, false) => {
            include_bytes!("../assets/responsive_opt.js").as_slice()
        }
        (StaticAsset::ResponsiveJs, true) => {
            include_bytes!("../assets/responsive_dbg.js").as_slice()
        }
        (StaticAsset::CriticalCssBeaconJs, false) => {
            include_bytes!("../assets/critical_css_beacon_opt.js").as_slice()
        }
        (StaticAsset::CriticalCssBeaconJs, true) => {
            include_bytes!("../assets/critical_css_beacon_dbg.js").as_slice()
        }
        (StaticAsset::CriticalImagesBeaconJs, false) => {
            include_bytes!("../assets/critical_images_beacon_opt.js").as_slice()
        }
        (StaticAsset::CriticalImagesBeaconJs, true) => {
            include_bytes!("../assets/critical_images_beacon_dbg.js").as_slice()
        }
        (StaticAsset::AddInstrumentation, false) => {
            include_bytes!("../assets/add_instrumentation_opt.js").as_slice()
        }
        (StaticAsset::AddInstrumentation, true) => {
            include_bytes!("../assets/add_instrumentation_dbg.js").as_slice()
        }
        (StaticAsset::ExtendedInstrumentation, false) => {
            include_bytes!("../assets/extended_instrumentation_opt.js").as_slice()
        }
        (StaticAsset::ExtendedInstrumentation, true) => {
            include_bytes!("../assets/extended_instrumentation_dbg.js").as_slice()
        }
        (StaticAsset::ClientDomainRewriter, false) => {
            include_bytes!("../assets/client_domain_rewriter_opt.js").as_slice()
        }
        (StaticAsset::ClientDomainRewriter, true) => {
            include_bytes!("../assets/client_domain_rewriter_dbg.js").as_slice()
        }
        (StaticAsset::DedupInlinedImages, false) => {
            include_bytes!("../assets/dedup_inlined_images_opt.js").as_slice()
        }
        (StaticAsset::DedupInlinedImages, true) => {
            include_bytes!("../assets/dedup_inlined_images_dbg.js").as_slice()
        }
        (StaticAsset::LazyloadImages, false) => {
            include_bytes!("../assets/lazyload_images_opt.js").as_slice()
        }
        (StaticAsset::LazyloadImages, true) => {
            include_bytes!("../assets/lazyload_images_dbg.js").as_slice()
        }
        (StaticAsset::DelayImages, false) => {
            include_bytes!("../assets/delay_images_opt.js").as_slice()
        }
        (StaticAsset::DelayImages, true) => {
            include_bytes!("../assets/delay_images_dbg.js").as_slice()
        }
        (StaticAsset::DelayImagesInline, false) => {
            include_bytes!("../assets/delay_images_inline_opt.js").as_slice()
        }
        (StaticAsset::DelayImagesInline, true) => {
            include_bytes!("../assets/delay_images_inline_dbg.js").as_slice()
        }
        (StaticAsset::LocalStorageCacheJs, false) => {
            include_bytes!("../assets/local_storage_cache_opt.js").as_slice()
        }
        (StaticAsset::LocalStorageCacheJs, true) => {
            include_bytes!("../assets/local_storage_cache_dbg.js").as_slice()
        }
        (StaticAsset::CriticalCssLoaderJs, false) => {
            include_bytes!("../assets/critical_css_loader_opt.js").as_slice()
        }
        (StaticAsset::CriticalCssLoaderJs, true) => {
            include_bytes!("../assets/critical_css_loader_dbg.js").as_slice()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn deterministic_js_markers() {
        // Mirrors the C++ test's population checks on both variants.
        for debug in [false, true] {
            let js = get_asset(StaticAsset::DeterministicJs, debug);
            assert!(contains(js, b"Date"));
            assert!(contains(js, b"Math.random"));
        }
        assert_ne!(
            get_asset(StaticAsset::DeterministicJs, false),
            get_asset(StaticAsset::DeterministicJs, true)
        );
    }

    #[test]
    fn defer_iframe_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::DeferIframe, debug);
            assert!(contains(js, b"pagespeed_iframe"));
        }
        assert_ne!(
            get_asset(StaticAsset::DeferIframe, false),
            get_asset(StaticAsset::DeferIframe, true)
        );
    }

    #[test]
    fn instrumentation_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::AddInstrumentation, debug);
            assert!(contains(js, b"addInstrumentationInit"));
            let js = get_asset(StaticAsset::ExtendedInstrumentation, debug);
            assert!(contains(js, b"getResourceTimingData"));
        }
        assert_ne!(
            get_asset(StaticAsset::AddInstrumentation, false),
            get_asset(StaticAsset::AddInstrumentation, true)
        );
        assert_ne!(
            get_asset(StaticAsset::ExtendedInstrumentation, false),
            get_asset(StaticAsset::ExtendedInstrumentation, true)
        );
    }

    #[test]
    fn client_domain_rewriter_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::ClientDomainRewriter, debug);
            assert!(contains(js, b"clientDomainRewriter"));
        }
        assert_ne!(
            get_asset(StaticAsset::ClientDomainRewriter, false),
            get_asset(StaticAsset::ClientDomainRewriter, true)
        );
    }

    #[test]
    fn lazyload_images_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::LazyloadImages, debug);
            assert!(contains(js, b"lazyLoad"));
            assert!(contains(js, b"loadIfVisibleAndMaybeBeacon"));
        }
        assert_ne!(
            get_asset(StaticAsset::LazyloadImages, false),
            get_asset(StaticAsset::LazyloadImages, true)
        );
    }

    #[test]
    fn delay_images_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::DelayImages, debug);
            assert!(contains(js, b"delayImages"));
            let inline = get_asset(StaticAsset::DelayImagesInline, debug);
            assert!(contains(inline, b"delayImagesInline"));
            assert!(contains(inline, b"addLowResImages"));
        }
        assert_ne!(
            get_asset(StaticAsset::DelayImages, false),
            get_asset(StaticAsset::DelayImages, true)
        );
        assert_ne!(
            get_asset(StaticAsset::DelayImagesInline, false),
            get_asset(StaticAsset::DelayImagesInline, true)
        );
    }

    #[test]
    fn local_storage_cache_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::LocalStorageCacheJs, debug);
            assert!(contains(js, b"localStorageCache"));
        }
        assert_ne!(
            get_asset(StaticAsset::LocalStorageCacheJs, false),
            get_asset(StaticAsset::LocalStorageCacheJs, true)
        );
    }

    #[test]
    fn critical_css_loader_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::CriticalCssLoaderJs, debug);
            assert!(contains(js, b"CriticalCssLoader"));
        }
        assert_ne!(
            get_asset(StaticAsset::CriticalCssLoaderJs, false),
            get_asset(StaticAsset::CriticalCssLoaderJs, true)
        );
    }

    #[test]
    fn dedup_inlined_images_markers() {
        for debug in [false, true] {
            let js = get_asset(StaticAsset::DedupInlinedImages, debug);
            assert!(contains(js, b"dedupInlinedImages"));
            assert!(contains(js, b"inlineImg"));
        }
        assert_ne!(
            get_asset(StaticAsset::DedupInlinedImages, false),
            get_asset(StaticAsset::DedupInlinedImages, true)
        );
    }

    #[test]
    fn beacon_and_defer_markers() {
        for debug in [false, true] {
            assert!(contains(
                get_asset(StaticAsset::CriticalCssBeaconJs, debug),
                b"Beacon"
            ));
            assert!(contains(
                get_asset(StaticAsset::CriticalImagesBeaconJs, debug),
                b"Beacon"
            ));
            assert!(contains(
                get_asset(StaticAsset::DeferJs, debug),
                b"pagespeed"
            ));
            // The genuine Closure output renames every identifier,
            // so no "responsive" string survives in the opt half;
            // `srcset` is the shared distinguishing marker.
            assert!(contains(
                get_asset(StaticAsset::ResponsiveJs, debug),
                b"srcset"
            ));
        }
        assert_ne!(
            get_asset(StaticAsset::DeferJs, false),
            get_asset(StaticAsset::DeferJs, true)
        );
    }
}
