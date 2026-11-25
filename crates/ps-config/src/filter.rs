//! Rewrite filters: enum, names, URL ids, sets, and enablement.
//!
//! Sources (all under `mod_pagespeed/net/instaweb/rewriter/`):
//! - `public/rewrite_options.h` (`enum Filter`, `enum RewriteLevel`)
//! - `rewrite_filter_names.gperf` (public name map, case-insensitive)
//! - `rewrite_options.cc` (URL ids, display names, filter sets,
//!   `Enabled()`, `AdjustFiltersByCommaSeparatedList`)
//!
//! Discriminants match the C++ enum order (`kAddBaseTag == 0`, ...,
//! `kStripScripts == 94`). Set slices keep C++ array order.
#![forbid(unsafe_code)]

/// A rewrite filter. Discriminants match the C++ `Filter` enum order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Filter {
    AddBaseTag = 0,
    AddHead = 1,
    AddIds = 2,
    AddInstrumentation = 3,
    ComputeStatistics = 4,
    CachePartialHtmlDeprecated = 5,
    CanonicalizeJavascriptLibraries = 6,
    CollapseWhitespace = 7,
    CombineCss = 8,
    CombineHeads = 9,
    CombineJavascript = 10,
    ComputeCriticalCss = 11,
    ComputeVisibleTextDeprecated = 12,
    ConvertGifToPng = 13,
    ConvertJpegToProgressive = 14,
    ConvertJpegToWebp = 15,
    ConvertToAvif = 16,
    ConvertMetaTags = 17,
    ConvertPngToJpeg = 18,
    ConvertToWebpAnimated = 19,
    ConvertToWebpLossless = 20,
    Debug = 21,
    DecodeRewrittenUrls = 22,
    DedupInlinedImages = 23,
    DeferIframe = 24,
    DeferJavascript = 25,
    DelayImages = 26,
    DeterministicJs = 27,
    DisableJavascript = 28,
    DivStructure = 29,
    ElideAttributes = 30,
    ExperimentCollectMobImageInfo = 31,
    ExperimentHttp2 = 32,
    ExplicitCloseTags = 33,
    ExtendCacheCss = 34,
    ExtendCacheImages = 35,
    ExtendCachePdfs = 36,
    ExtendCacheScripts = 37,
    FallbackRewriteCssUrls = 38,
    FixReflows = 39,
    FlattenCssImports = 40,
    FlushSubresources = 41,
    HandleNoscriptRedirect = 42,
    HintPreloadSubresources = 43,
    HtmlWriterFilter = 44,
    IncludeJsSourceMaps = 45,
    InlineCss = 46,
    InlineGoogleFontCss = 47,
    InlineImages = 48,
    InlineImportToLink = 49,
    InlineJavascript = 50,
    InPlaceOptimizeForBrowser = 51,
    InsertAmpLink = 52,
    InsertDnsPrefetch = 53,
    InsertGA = 54,
    InsertImageDimensions = 55,
    JpegSubsampling = 56,
    LazyloadImages = 57,
    LeftTrimUrls = 58,
    LocalStorageCache = 59,
    MakeGoogleAnalyticsAsync = 60,
    MakeShowAdsAsync = 61,
    Mobilize = 62,
    MobilizePrecompute = 63,
    MoveCssAboveScripts = 64,
    MoveCssToHead = 65,
    OutlineCss = 66,
    OutlineJavascript = 67,
    Pedantic = 68,
    PrioritizeLcpImages = 69,
    PrioritizeCriticalCss = 70,
    RecompressJpeg = 71,
    RecompressPng = 72,
    RecompressWebp = 73,
    RemoveComments = 74,
    RemoveQuotes = 75,
    ResizeImages = 76,
    ResizeMobileImages = 77,
    ResizeToRenderedImageDimensions = 78,
    ResponsiveImages = 79,
    ResponsiveImagesZoom = 80,
    RewriteCss = 81,
    RewriteDomains = 82,
    RewriteJavascriptExternal = 83,
    RewriteJavascriptInline = 84,
    RewriteStyleAttributes = 85,
    RewriteStyleAttributesWithUrl = 86,
    ServeDeprecationNotice = 87,
    SplitHtml = 88,
    SplitHtmlHelper = 89,
    SpriteImages = 90,
    StripImageColorProfile = 91,
    StripImageMetaData = 92,
    StripLegacyPolyfills = 93,
    StripScripts = 94,
    // Rust-port extension past the C++ set: appended, never inserted,
    // so every C++ discriminant (and every stored bit position) keeps
    // its value.
    CompressZstd = 95,
    PreloadCriticalFonts = 96,
}

/// Number of filters (`kEndOfFilters`).
pub const NUM_FILTERS: usize = 97;

impl Filter {
    /// All filters in enum order.
    pub const ALL: [Filter; NUM_FILTERS] = [
        Filter::AddBaseTag,
        Filter::AddHead,
        Filter::AddIds,
        Filter::AddInstrumentation,
        Filter::ComputeStatistics,
        Filter::CachePartialHtmlDeprecated,
        Filter::CanonicalizeJavascriptLibraries,
        Filter::CollapseWhitespace,
        Filter::CombineCss,
        Filter::CombineHeads,
        Filter::CombineJavascript,
        Filter::ComputeCriticalCss,
        Filter::ComputeVisibleTextDeprecated,
        Filter::ConvertGifToPng,
        Filter::ConvertJpegToProgressive,
        Filter::ConvertJpegToWebp,
        Filter::ConvertToAvif,
        Filter::ConvertMetaTags,
        Filter::ConvertPngToJpeg,
        Filter::ConvertToWebpAnimated,
        Filter::ConvertToWebpLossless,
        Filter::Debug,
        Filter::DecodeRewrittenUrls,
        Filter::DedupInlinedImages,
        Filter::DeferIframe,
        Filter::DeferJavascript,
        Filter::DelayImages,
        Filter::DeterministicJs,
        Filter::DisableJavascript,
        Filter::DivStructure,
        Filter::ElideAttributes,
        Filter::ExperimentCollectMobImageInfo,
        Filter::ExperimentHttp2,
        Filter::ExplicitCloseTags,
        Filter::ExtendCacheCss,
        Filter::ExtendCacheImages,
        Filter::ExtendCachePdfs,
        Filter::ExtendCacheScripts,
        Filter::FallbackRewriteCssUrls,
        Filter::FixReflows,
        Filter::FlattenCssImports,
        Filter::FlushSubresources,
        Filter::HandleNoscriptRedirect,
        Filter::HintPreloadSubresources,
        Filter::HtmlWriterFilter,
        Filter::IncludeJsSourceMaps,
        Filter::InlineCss,
        Filter::InlineGoogleFontCss,
        Filter::InlineImages,
        Filter::InlineImportToLink,
        Filter::InlineJavascript,
        Filter::InPlaceOptimizeForBrowser,
        Filter::InsertAmpLink,
        Filter::InsertDnsPrefetch,
        Filter::InsertGA,
        Filter::InsertImageDimensions,
        Filter::JpegSubsampling,
        Filter::LazyloadImages,
        Filter::LeftTrimUrls,
        Filter::LocalStorageCache,
        Filter::MakeGoogleAnalyticsAsync,
        Filter::MakeShowAdsAsync,
        Filter::Mobilize,
        Filter::MobilizePrecompute,
        Filter::MoveCssAboveScripts,
        Filter::MoveCssToHead,
        Filter::OutlineCss,
        Filter::OutlineJavascript,
        Filter::Pedantic,
        Filter::PrioritizeLcpImages,
        Filter::PrioritizeCriticalCss,
        Filter::RecompressJpeg,
        Filter::RecompressPng,
        Filter::RecompressWebp,
        Filter::RemoveComments,
        Filter::RemoveQuotes,
        Filter::ResizeImages,
        Filter::ResizeMobileImages,
        Filter::ResizeToRenderedImageDimensions,
        Filter::ResponsiveImages,
        Filter::ResponsiveImagesZoom,
        Filter::RewriteCss,
        Filter::RewriteDomains,
        Filter::RewriteJavascriptExternal,
        Filter::RewriteJavascriptInline,
        Filter::RewriteStyleAttributes,
        Filter::RewriteStyleAttributesWithUrl,
        Filter::ServeDeprecationNotice,
        Filter::SplitHtml,
        Filter::SplitHtmlHelper,
        Filter::SpriteImages,
        Filter::StripImageColorProfile,
        Filter::StripImageMetaData,
        Filter::StripLegacyPolyfills,
        Filter::StripScripts,
        Filter::CompressZstd,
        Filter::PreloadCriticalFonts,
    ];

    /// Converts a C++ enum value (`0..97`) to a filter.
    #[must_use]
    pub const fn from_repr(value: u8) -> Option<Filter> {
        // `as` casts are not allowed on enums in const fn here, so index ALL.
        if value as usize >= NUM_FILTERS {
            return None;
        }
        Some(Filter::ALL[value as usize])
    }

    /// Short resource-URL id (`kFilterVectorStaticInitializer`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        const IDS: [&str; NUM_FILTERS] = [
            "ab",
            "ah",
            "ad",
            "ai",
            "ca",
            "ct",
            "ij",
            "cw",
            "cc",
            "ch",
            "jc",
            "bc",
            "bp",
            "gp",
            "jp",
            "jw",
            "av",
            "mc",
            "pj",
            "wa",
            "ws",
            "db",
            "du",
            "dd",
            "df",
            "dj",
            "di",
            "mj",
            "jd",
            "ds",
            "ea",
            "xi",
            "x2",
            "xc",
            "ec",
            "ei",
            "ep",
            "es",
            "fc",
            "fr",
            "if",
            "fs",
            "hn",
            "hpsr",
            "hw",
            "sm",
            "ci",
            "gf",
            "ii",
            "il",
            "ji",
            "io",
            "ial",
            "idp",
            "ig",
            "id",
            "js",
            "ll",
            "tu",
            "ls",
            "ga",
            "gaa",
            "mob",
            "mob_precompute",
            "cj",
            "cm",
            "co",
            "jo",
            "pc",
            "pli",
            "pr",
            "rj",
            "rp",
            "rw",
            "rc",
            "rq",
            "ri",
            "rm",
            "ir",
            "rx",
            "rz",
            "cf",
            "rd",
            "jm",
            "jj",
            "cs",
            "cu",
            "sd",
            "sh",
            "se",
            "is",
            "cp",
            "md",
            "slp",
            "ss",
            "cz",
            "pf",
        ];
        IDS[self as usize]
    }

    /// Display name (`kFilterVectorStaticInitializer`, byte-identical,
    /// including the trailing space in `"Fallback Rewrite Css "`).
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        const NAMES: [&str; NUM_FILTERS] = [
            "Add Base Tag",
            "Add Head",
            "Add Ids",
            "Add Instrumentation",
            "Compute HTML statistics",
            "Deprecated.",
            "Canonicalize Javascript library URLs",
            "Collapse Whitespace",
            "Combine Css",
            "Combine Heads",
            "Combine Javascript",
            "Background Compute Critical css",
            "Deprecated.",
            "Convert Gif to Png",
            "Convert Jpeg to Progressive",
            "Convert Jpeg To Webp",
            "Convert images to AVIF",
            "Convert Meta Tags",
            "Convert Png to Jpeg",
            "Convert animated images to WebP",
            "When converting images to WebP, prefer lossless conversions",
            "Debug",
            "Decode Rewritten URLs",
            "Dedup Inlined Images",
            "Defer Iframe",
            "Defer Javascript",
            "Delay Images",
            "Deterministic Js",
            "Disables scripts by placing them inside noscript tags",
            "Div Structure",
            "Elide Attributes",
            "Experiment: collect image info to help mobilization",
            "Experiment: http2-specific features in development",
            "Explicit Close Tags",
            "Cache Extend Css",
            "Cache Extend Images",
            "Cache Extend PDFs",
            "Cache Extend Scripts",
            "Fallback Rewrite Css ",
            "Fix Reflows",
            "Flatten CSS Imports",
            "Flush Subresources",
            "Handles Noscript Redirects",
            "Hint Preload of Subresources",
            "Flushes html",
            "Include JS Source Maps",
            "Inline Css",
            "Inline Google Font CSS",
            "Inline Images",
            "Inline @import to Link",
            "Inline Javascript",
            "In-place optimize for browser",
            "Insert AMP link",
            "Insert DNS Prefetch",
            "Insert Google Analytics",
            "Insert Image Dimensions",
            "Jpeg Subsampling",
            "Lazyload Images",
            "Left Trim Urls",
            "Local Storage Cache",
            "Make Google Analytics Async",
            "Convert showads.js use to async adsbygoogle.js",
            "Mobilize Webpage",
            "Deprecated.",
            "Move Css Above Scripts",
            "Move Css To Head",
            "Outline Css",
            "Outline Javascript",
            "Add pedantic types",
            "Prioritize LCP Images",
            "Prioritize Critical Css",
            "Recompress Jpeg",
            "Recompress Png",
            "Recompress Webp",
            "Remove Comments",
            "Remove Quotes",
            "Resize Images",
            "Resize Mobile Images",
            "Resize to Rendered Image Dimensions",
            "Responsive Images",
            "Responsive Images Zoom",
            "Rewrite Css",
            "Rewrite Domains",
            "Rewrite External Javascript",
            "Rewrite Inline Javascript",
            "Rewrite Style Attributes",
            "Rewrite Style Attributes With Url",
            "Serve Deprecation Notice",
            "Deprecated",
            "Deprecated",
            "Sprite Images",
            "Strip Image Color Profiles",
            "Strip Image Meta Data",
            "Strip Legacy Polyfills",
            "Strip Scripts",
            "Compress Zstd",
            "Preload Critical Fonts",
        ];
        NAMES[self as usize]
    }

    /// Looks up a filter by URL id (`LookupFilterById`; case-sensitive).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Filter> {
        // Linear scan over 97 entries; the C++ binary search is O(log n)
        // but both are trivial here. Sorted-by-id order is irrelevant.
        Filter::ALL.iter().find(|f| f.id() == id).copied()
    }

    /// Looks up a filter by public name (`FilterMapper::Lookup`).
    ///
    /// Matching is ASCII case-insensitive (`%ignore-case` in the gperf
    /// file). Internal-only filters (`DelayImages` except via its alias,
    /// `HandleNoscriptRedirect`, `HtmlWriterFilter`,
    /// `ServeDeprecationNotice`) have no public name and return `None`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Filter> {
        // 95 entries in gperf order.
        const MAP: [(&str, Filter); 95] = [
            ("add_base_tag", Filter::AddBaseTag),
            ("add_head", Filter::AddHead),
            ("add_ids", Filter::AddIds),
            ("add_instrumentation", Filter::AddInstrumentation),
            ("cache_partial_html", Filter::CachePartialHtmlDeprecated),
            (
                "canonicalize_javascript_libraries",
                Filter::CanonicalizeJavascriptLibraries,
            ),
            ("collapse_whitespace", Filter::CollapseWhitespace),
            ("combine_css", Filter::CombineCss),
            ("combine_javascript", Filter::CombineJavascript),
            ("combine_heads", Filter::CombineHeads),
            ("compress_zstd", Filter::CompressZstd),
            ("compute_statistics", Filter::ComputeStatistics),
            ("compute_critical_css", Filter::ComputeCriticalCss),
            ("convert_gif_to_png", Filter::ConvertGifToPng),
            (
                "convert_jpeg_to_progressive",
                Filter::ConvertJpegToProgressive,
            ),
            ("convert_jpeg_to_webp", Filter::ConvertJpegToWebp),
            ("convert_to_avif", Filter::ConvertToAvif),
            ("convert_meta_tags", Filter::ConvertMetaTags),
            ("convert_png_to_jpeg", Filter::ConvertPngToJpeg),
            ("convert_to_webp_animated", Filter::ConvertToWebpAnimated),
            ("convert_to_webp_lossless", Filter::ConvertToWebpLossless),
            ("debug", Filter::Debug),
            ("decode_rewritten_urls", Filter::DecodeRewrittenUrls),
            ("dedup_inlined_images", Filter::DedupInlinedImages),
            ("defer_iframe", Filter::DeferIframe),
            ("defer_javascript", Filter::DeferJavascript),
            ("deterministic_js", Filter::DeterministicJs),
            ("disable_javascript", Filter::DisableJavascript),
            ("div_structure", Filter::DivStructure),
            ("elide_attributes", Filter::ElideAttributes),
            (
                "experiment_collect_mob_image_info",
                Filter::ExperimentCollectMobImageInfo,
            ),
            ("experiment_http2", Filter::ExperimentHttp2),
            ("explicit_close_tags", Filter::ExplicitCloseTags),
            ("extend_cache_css", Filter::ExtendCacheCss),
            ("extend_cache_images", Filter::ExtendCacheImages),
            ("extend_cache_pdfs", Filter::ExtendCachePdfs),
            ("extend_cache_scripts", Filter::ExtendCacheScripts),
            ("fallback_rewrite_css_urls", Filter::FallbackRewriteCssUrls),
            ("fix_reflows", Filter::FixReflows),
            ("flatten_css_imports", Filter::FlattenCssImports),
            ("flush_subresources", Filter::FlushSubresources),
            ("hint_preload_subresources", Filter::HintPreloadSubresources),
            (
                "in_place_optimize_for_browser",
                Filter::InPlaceOptimizeForBrowser,
            ),
            ("include_js_source_maps", Filter::IncludeJsSourceMaps),
            ("inline_css", Filter::InlineCss),
            ("inline_google_font_css", Filter::InlineGoogleFontCss),
            ("inline_images", Filter::InlineImages),
            ("inline_import_to_link", Filter::InlineImportToLink),
            ("inline_javascript", Filter::InlineJavascript),
            ("inline_preview_images", Filter::DelayImages),
            ("insert_amp_link", Filter::InsertAmpLink),
            ("insert_dns_prefetch", Filter::InsertDnsPrefetch),
            ("insert_ga", Filter::InsertGA),
            ("insert_img_dimensions", Filter::InsertImageDimensions),
            ("insert_image_dimensions", Filter::InsertImageDimensions),
            ("jpeg_subsampling", Filter::JpegSubsampling),
            ("lazyload_images", Filter::LazyloadImages),
            ("local_storage_cache", Filter::LocalStorageCache),
            ("left_trim_urls", Filter::LeftTrimUrls),
            (
                "make_google_analytics_async",
                Filter::MakeGoogleAnalyticsAsync,
            ),
            ("make_show_ads_async", Filter::MakeShowAdsAsync),
            ("mobilize", Filter::Mobilize),
            ("mobilize_precompute", Filter::MobilizePrecompute),
            ("move_css_above_scripts", Filter::MoveCssAboveScripts),
            ("move_css_to_head", Filter::MoveCssToHead),
            ("outline_css", Filter::OutlineCss),
            ("outline_javascript", Filter::OutlineJavascript),
            ("pedantic", Filter::Pedantic),
            ("preload_critical_fonts", Filter::PreloadCriticalFonts),
            ("prioritize_lcp_images", Filter::PrioritizeLcpImages),
            ("prioritize_critical_css", Filter::PrioritizeCriticalCss),
            ("recompress_jpeg", Filter::RecompressJpeg),
            ("recompress_png", Filter::RecompressPng),
            ("recompress_webp", Filter::RecompressWebp),
            ("remove_comments", Filter::RemoveComments),
            ("remove_quotes", Filter::RemoveQuotes),
            ("resize_images", Filter::ResizeImages),
            ("resize_mobile_images", Filter::ResizeMobileImages),
            (
                "resize_rendered_image_dimensions",
                Filter::ResizeToRenderedImageDimensions,
            ),
            ("responsive_images", Filter::ResponsiveImages),
            ("responsive_images_zoom", Filter::ResponsiveImagesZoom),
            ("rewrite_css", Filter::RewriteCss),
            ("rewrite_domains", Filter::RewriteDomains),
            (
                "rewrite_javascript_external",
                Filter::RewriteJavascriptExternal,
            ),
            ("rewrite_javascript_inline", Filter::RewriteJavascriptInline),
            ("rewrite_style_attributes", Filter::RewriteStyleAttributes),
            (
                "rewrite_style_attributes_with_url",
                Filter::RewriteStyleAttributesWithUrl,
            ),
            ("split_html", Filter::SplitHtml),
            ("split_html_helper", Filter::SplitHtmlHelper),
            ("sprite_images", Filter::SpriteImages),
            ("strip_image_color_profile", Filter::StripImageColorProfile),
            ("strip_image_meta_data", Filter::StripImageMetaData),
            ("strip_legacy_polyfills", Filter::StripLegacyPolyfills),
            ("strip_scripts", Filter::StripScripts),
            ("trim_urls", Filter::LeftTrimUrls),
        ];
        MAP.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, f)| *f)
    }
}

/// Core filters (`kCoreFilterSet`, 31 entries, C++ order plus the
/// Rust-port `PreloadCriticalFonts`, on by default).
pub const CORE_FILTERS: &[Filter] = &[
    Filter::AddHead,
    Filter::CombineCss,
    Filter::CombineJavascript,
    Filter::ConvertGifToPng,
    Filter::ConvertJpegToProgressive,
    Filter::ConvertJpegToWebp,
    Filter::ConvertToAvif,
    Filter::ConvertMetaTags,
    Filter::ConvertPngToJpeg,
    Filter::ConvertToWebpLossless,
    Filter::ExtendCacheCss,
    Filter::ExtendCacheImages,
    Filter::ExtendCacheScripts,
    Filter::FallbackRewriteCssUrls,
    Filter::FlattenCssImports,
    Filter::InlineCss,
    Filter::InlineImages,
    Filter::InlineImportToLink,
    Filter::InlineJavascript,
    Filter::JpegSubsampling,
    Filter::RecompressJpeg,
    Filter::RecompressPng,
    Filter::RecompressWebp,
    Filter::ResizeImages,
    Filter::RewriteCss,
    Filter::RewriteJavascriptExternal,
    Filter::RewriteJavascriptInline,
    Filter::RewriteStyleAttributesWithUrl,
    Filter::StripImageColorProfile,
    Filter::StripImageMetaData,
    // Rust-port extension appended last (enum order, not alphabetical).
    Filter::PreloadCriticalFonts,
];

/// Optimize-for-bandwidth filters (`kOptimizeForBandwidthFilterSet`, 15).
pub const BANDWIDTH_FILTERS: &[Filter] = &[
    Filter::ConvertGifToPng,
    Filter::ConvertJpegToProgressive,
    Filter::ConvertJpegToWebp,
    Filter::ConvertToAvif,
    Filter::ConvertPngToJpeg,
    Filter::InPlaceOptimizeForBrowser,
    Filter::JpegSubsampling,
    Filter::RecompressJpeg,
    Filter::RecompressPng,
    Filter::RecompressWebp,
    Filter::RewriteCss,
    Filter::RewriteJavascriptExternal,
    Filter::RewriteJavascriptInline,
    Filter::StripImageColorProfile,
    Filter::StripImageMetaData,
];

/// Test filters (`kTestFilterSet`, 17; Core is added separately by the
/// level switch, as in C++).
pub const TEST_FILTERS: &[Filter] = &[
    Filter::ConvertJpegToWebp,
    Filter::Debug,
    Filter::DeferIframe,
    Filter::DeferJavascript,
    Filter::DelayImages,
    Filter::IncludeJsSourceMaps,
    Filter::InsertAmpLink,
    Filter::InsertGA,
    Filter::InsertImageDimensions,
    Filter::LazyloadImages,
    Filter::LeftTrimUrls,
    Filter::MakeGoogleAnalyticsAsync,
    Filter::PrioritizeCriticalCss,
    Filter::ResizeToRenderedImageDimensions,
    Filter::ResponsiveImages,
    Filter::RewriteDomains,
    Filter::SpriteImages,
];

/// Dangerous filters (`kDangerousFilterSet`, 14), excluded even from All.
pub const DANGEROUS_FILTERS: &[Filter] = &[
    Filter::CachePartialHtmlDeprecated,
    Filter::CanonicalizeJavascriptLibraries,
    Filter::ComputeVisibleTextDeprecated,
    Filter::DeterministicJs,
    Filter::DisableJavascript,
    Filter::DivStructure,
    Filter::ExperimentCollectMobImageInfo,
    Filter::ExplicitCloseTags,
    Filter::FixReflows,
    Filter::Mobilize,
    Filter::MobilizePrecompute,
    Filter::ServeDeprecationNotice,
    Filter::StripLegacyPolyfills,
    Filter::StripScripts,
];

/// Filters requiring script execution (`kRequiresScriptExecutionFilterSet`).
pub const REQUIRES_SCRIPT_EXECUTION: &[Filter] = &[
    Filter::CachePartialHtmlDeprecated,
    Filter::DedupInlinedImages,
    Filter::DeferIframe,
    Filter::DeferJavascript,
    Filter::DelayImages,
    Filter::FlushSubresources,
    Filter::LazyloadImages,
    Filter::LocalStorageCache,
    Filter::Mobilize,
];

/// Filters requiring a `head` element (`kAddHeadFilters`, 10).
pub const ADD_HEAD_FILTERS: &[Filter] = &[
    Filter::AddBaseTag,
    Filter::AddHead,
    Filter::AddInstrumentation,
    Filter::CombineHeads,
    Filter::DeterministicJs,
    Filter::HandleNoscriptRedirect,
    Filter::MakeGoogleAnalyticsAsync,
    Filter::Mobilize,
    Filter::MoveCssAboveScripts,
    Filter::MoveCssToHead,
];

/// Mobilize level filters (`kMobilizeFilterSet`).
pub const MOBILIZE_FILTERS: &[Filter] =
    &[Filter::Mobilize, Filter::RewriteCss, Filter::RewriteDomains];

/// Filters disabled by image-preserve-URLs (`kImagePreserveUrlDisabledFilters`).
pub const IMAGE_PRESERVE_OFF: &[Filter] = &[
    Filter::DelayImages,
    Filter::ExtendCacheImages,
    Filter::InlineImages,
    Filter::LazyloadImages,
    Filter::ResizeImages,
    Filter::ResizeToRenderedImageDimensions,
    Filter::ResponsiveImages,
    Filter::SpriteImages,
];

/// Filters disabled by js-preserve-URLs (`kJsPreserveUrlDisabledFilters`).
pub const JS_PRESERVE_OFF: &[Filter] = &[
    Filter::CanonicalizeJavascriptLibraries,
    Filter::CombineJavascript,
    Filter::DeferJavascript,
    Filter::ExtendCacheScripts,
    Filter::InlineJavascript,
    Filter::OutlineJavascript,
];

/// Filters disabled by css-preserve-URLs (`kCssPreserveUrlDisabledFilters`).
pub const CSS_PRESERVE_OFF: &[Filter] = &[
    Filter::CombineCss,
    Filter::ExtendCacheCss,
    Filter::InlineCss,
    Filter::InlineGoogleFontCss,
    Filter::InlineImportToLink,
    Filter::LeftTrimUrls,
    Filter::OutlineCss,
];

/// Compound alias expansions (`AddByNameToFilterSet`).
pub const COMPOUND_REWRITE_IMAGES: &[Filter] = &[
    Filter::ConvertGifToPng,
    Filter::ConvertJpegToProgressive,
    Filter::ConvertJpegToWebp,
    Filter::ConvertToAvif,
    Filter::ConvertPngToJpeg,
    Filter::ConvertToWebpLossless,
    Filter::InlineImages,
    Filter::JpegSubsampling,
    Filter::RecompressJpeg,
    Filter::RecompressPng,
    Filter::RecompressWebp,
    Filter::ResizeImages,
    Filter::StripImageColorProfile,
    Filter::StripImageMetaData,
];

/// Compound alias `recompress_images` (11 filters).
pub const COMPOUND_RECOMPRESS_IMAGES: &[Filter] = &[
    Filter::ConvertGifToPng,
    Filter::ConvertJpegToProgressive,
    Filter::ConvertJpegToWebp,
    Filter::ConvertToAvif,
    Filter::ConvertPngToJpeg,
    Filter::JpegSubsampling,
    Filter::RecompressJpeg,
    Filter::RecompressPng,
    Filter::RecompressWebp,
    Filter::StripImageColorProfile,
    Filter::StripImageMetaData,
];

/// Compound alias `extend_cache` (css/images/scripts, NOT pdfs).
pub const COMPOUND_EXTEND_CACHE: &[Filter] = &[
    Filter::ExtendCacheCss,
    Filter::ExtendCacheImages,
    Filter::ExtendCacheScripts,
];

/// Compound alias `rewrite_javascript`.
pub const COMPOUND_REWRITE_JAVASCRIPT: &[Filter] = &[
    Filter::RewriteJavascriptExternal,
    Filter::RewriteJavascriptInline,
];

/// Looks up a compound alias by name (case-sensitive, as in C++).
#[must_use]
pub fn compound_alias(name: &str) -> Option<&'static [Filter]> {
    match name {
        "rewrite_images" => Some(COMPOUND_REWRITE_IMAGES),
        "recompress_images" => Some(COMPOUND_RECOMPRESS_IMAGES),
        "extend_cache" => Some(COMPOUND_EXTEND_CACHE),
        "rewrite_javascript" => Some(COMPOUND_REWRITE_JAVASCRIPT),
        // `testing` and `core` expand to static sets; handled by callers via
        // TEST_FILTERS/CORE_FILTERS to keep one definition of each list.
        _ => None,
    }
}

/// Filters whose options a URL segment may enable
/// (`ImageRewriteFilter::kRelatedFilters`, enum order).
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

/// Options a URL segment may set for images
/// (`kRelatedOptions` after the `Initialize()` sort).
pub const IMAGE_RELATED_OPTIONS: &[&str] = &[
    "ImageJpegNumProgressiveScans",
    "ImageJpegNumProgressiveScansForSmallScreens",
    "ImageLimitOptimizedPercent",
    "ImageLimitResizeAreaPercent",
    "ImagePreserveURLs",
    "ImageRecompressionQuality",
    "JpegQualityForSaveData",
    "JpegRecompressionQuality",
    "JpegRecompressionQualityForSmallScreens",
    "ProgressiveJpegMinBytes",
    "WebpAnimatedRecompressionQuality",
    "WebpQualityForSaveData",
    "WebpRecompressionQuality",
    "WebpRecompressionQualityForSmallScreens",
];

/// Filters whose options a CSS URL segment may enable (the
/// `MergeArrays` union of the image and CSS lists, enum order).
pub const CSS_RELATED_FILTERS: &[Filter] = &[
    Filter::ConvertGifToPng,
    Filter::ConvertJpegToProgressive,
    Filter::ConvertJpegToWebp,
    Filter::ConvertToAvif,
    Filter::ConvertPngToJpeg,
    Filter::ConvertToWebpAnimated,
    Filter::ConvertToWebpLossless,
    Filter::ExtendCacheCss,
    Filter::ExtendCacheImages,
    Filter::FallbackRewriteCssUrls,
    Filter::FlattenCssImports,
    Filter::InlineImages,
    Filter::JpegSubsampling,
    Filter::LeftTrimUrls,
    Filter::RecompressJpeg,
    Filter::RecompressPng,
    Filter::RecompressWebp,
    Filter::ResizeImages,
    Filter::ResizeMobileImages,
    Filter::RewriteDomains,
    Filter::SpriteImages,
    Filter::StripImageColorProfile,
    Filter::StripImageMetaData,
];

/// Options a CSS URL segment may set (image list plus the CSS
/// three, after the `Initialize()` sort — note `ImagePreserveURLs`
/// appears twice, verbatim: the concatenation is not deduplicated).
pub const CSS_RELATED_OPTIONS: &[&str] = &[
    "CssImageInlineMaxBytes",
    "CssPreserveURLs",
    "ImageJpegNumProgressiveScans",
    "ImageJpegNumProgressiveScansForSmallScreens",
    "ImageLimitOptimizedPercent",
    "ImageLimitResizeAreaPercent",
    "ImagePreserveURLs",
    "ImagePreserveURLs",
    "ImageRecompressionQuality",
    "JpegQualityForSaveData",
    "JpegRecompressionQuality",
    "JpegRecompressionQualityForSmallScreens",
    "ProgressiveJpegMinBytes",
    "WebpAnimatedRecompressionQuality",
    "WebpQualityForSaveData",
    "WebpRecompressionQuality",
    "WebpRecompressionQualityForSmallScreens",
];

/// Empty related sets (base `RewriteFilter` defaults).
pub const NO_RELATED_FILTERS: &[Filter] = &[];
/// Empty related options (base `RewriteFilter` default).
pub const NO_RELATED_OPTIONS: &[&str] = &[];

/// Related sets for a decoding filter id
/// (`FindFilterForDecoding` + `RelatedFilters`/`RelatedOptions`):
/// the nine ids registered on every decoding driver
/// (`SetServerContext`), of which only `cf` and `ic` carry
/// non-empty tables. Unknown ids yield `None` (no filter).
#[must_use]
pub fn related_tables_for_decoding_id(
    id: &str,
) -> Option<(&'static [Filter], &'static [&'static str])> {
    match id {
        "cf" => Some((CSS_RELATED_FILTERS, CSS_RELATED_OPTIONS)),
        "ic" => Some((IMAGE_RELATED_FILTERS, IMAGE_RELATED_OPTIONS)),
        "ce" | "cc" | "jc" | "jm" | "is" | "ls" | "sm" => {
            Some((NO_RELATED_FILTERS, NO_RELATED_OPTIONS))
        }
        _ => None,
    }
}

/// A set of filters as a 95-bit bitset (mirrors `FilterSet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilterSet {
    words: [u64; 2],
}

impl FilterSet {
    /// Empty set.
    #[must_use]
    pub const fn new() -> FilterSet {
        FilterSet { words: [0, 0] }
    }

    /// Builds a set from a slice.
    #[must_use]
    pub fn from_slice(filters: &[Filter]) -> FilterSet {
        let mut set = FilterSet::new();
        for f in filters {
            set.insert(*f);
        }
        set
    }

    /// Inserts a filter.
    pub fn insert(&mut self, filter: Filter) {
        let bit = filter as usize;
        self.words[bit / 64] |= 1u64 << (bit % 64);
    }

    /// Removes a filter.
    pub fn remove(&mut self, filter: Filter) {
        let bit = filter as usize;
        self.words[bit / 64] &= !(1u64 << (bit % 64));
    }

    /// Tests membership (`EnumSet::IsSet`).
    #[must_use]
    pub fn contains(self, filter: Filter) -> bool {
        let bit = filter as usize;
        self.words[bit / 64] & (1u64 << (bit % 64)) != 0
    }

    /// Number of members.
    #[must_use]
    pub fn len(self) -> usize {
        (self.words[0].count_ones() + self.words[1].count_ones()) as usize
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.words == [0, 0]
    }

    /// Iterates members in enum order.
    pub fn iter(self) -> impl Iterator<Item = Filter> {
        Filter::ALL.into_iter().filter(move |f| self.contains(*f))
    }
}

/// Base rewrite level (`enum RewriteLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RewriteLevel {
    /// Enable no filters. C++ default (`kPassThrough`).
    #[default]
    PassThrough,
    /// Bandwidth-safe filters; implies preserve-URLs + IPRO.
    OptimizeForBandwidth,
    /// Core set. Most users should specify this.
    CoreFilters,
    /// Mobilize + CSS + domain rewrite.
    MobilizeFilters,
    /// Core plus test filters.
    TestingCoreFilters,
    /// All non-dangerous filters.
    AllFilters,
}

impl RewriteLevel {
    /// Parses a level name (`ParseRewriteLevel`, ASCII case-insensitive).
    #[must_use]
    pub fn parse(name: &str) -> Option<RewriteLevel> {
        if name.eq_ignore_ascii_case("CoreFilters") {
            Some(RewriteLevel::CoreFilters)
        } else if name.eq_ignore_ascii_case("PassThrough") {
            Some(RewriteLevel::PassThrough)
        } else if name.eq_ignore_ascii_case("OptimizeForBandwidth") {
            Some(RewriteLevel::OptimizeForBandwidth)
        } else if name.eq_ignore_ascii_case("MobilizeFilters") {
            Some(RewriteLevel::MobilizeFilters)
        } else if name.eq_ignore_ascii_case("TestingCoreFilters") {
            Some(RewriteLevel::TestingCoreFilters)
        } else if name.eq_ignore_ascii_case("AllFilters") {
            Some(RewriteLevel::AllFilters)
        } else {
            None
        }
    }

    /// Canonical config spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            RewriteLevel::PassThrough => "PassThrough",
            RewriteLevel::OptimizeForBandwidth => "OptimizeForBandwidth",
            RewriteLevel::CoreFilters => "CoreFilters",
            RewriteLevel::MobilizeFilters => "MobilizeFilters",
            RewriteLevel::TestingCoreFilters => "TestingCoreFilters",
            RewriteLevel::AllFilters => "AllFilters",
        }
    }
}

/// Enabled/disabled/forbidden filter sets plus the level and preserve flags.
///
/// Mirrors the corresponding `RewriteOptions` fields; `Enabled()` implements
/// `RewriteOptions::Enabled` exactly, including the TestingCore→Core
/// fallthrough.
#[derive(Debug, Clone)]
pub struct FilterState {
    level: RewriteLevel,
    enabled: FilterSet,
    disabled: FilterSet,
    forbidden: FilterSet,
    css_preserve_urls: bool,
    js_preserve_urls: bool,
    image_preserve_urls: bool,
    forbid_all_disabled: bool,
}

impl Default for FilterState {
    /// Fresh state at `PassThrough` level with the HTML writer
    /// on (C++ enables `kHtmlWriterFilter` in the ctor, so
    /// every options object carries it until disabled).
    fn default() -> Self {
        let mut enabled = FilterSet::new();
        enabled.insert(Filter::HtmlWriterFilter);
        FilterState {
            level: RewriteLevel::PassThrough,
            enabled,
            disabled: FilterSet::new(),
            forbidden: FilterSet::new(),
            css_preserve_urls: false,
            js_preserve_urls: false,
            image_preserve_urls: false,
            forbid_all_disabled: false,
        }
    }
}

impl FilterState {
    /// Creates empty state at `PassThrough` level.
    #[must_use]
    pub fn new() -> FilterState {
        FilterState::default()
    }

    /// Returns the rewrite level.
    #[must_use]
    pub fn level(&self) -> RewriteLevel {
        self.level
    }

    /// Sets the rewrite level (`SetRewriteLevel`).
    pub fn set_level(&mut self, level: RewriteLevel) {
        self.level = level;
    }

    /// Enables a filter (`EnableFilter`).
    pub fn enable(&mut self, filter: Filter) {
        self.enabled.insert(filter);
    }

    /// Disables a filter (`DisableFilter`).
    pub fn disable(&mut self, filter: Filter) {
        self.disabled.insert(filter);
    }

    /// Forbids a filter (`ForbidFilter`).
    pub fn forbid(&mut self, filter: Filter) {
        self.forbidden.insert(filter);
    }

    /// Removes a disable (`disabled_filters_.Erase`).
    pub fn undisable(&mut self, filter: Filter) {
        self.disabled.remove(filter);
    }

    /// Debug-free copy for signatures (`ComputeSignature` ignores
    /// `kDebug`, so cached output stays valid with debug on or off).
    #[must_use]
    pub fn without_debug_for_signature(&self) -> FilterState {
        let mut state = self.clone();
        state.enabled.remove(Filter::Debug);
        state.disabled.remove(Filter::Debug);
        state.forbidden.remove(Filter::Debug);
        state
    }

    /// Removes a forbid (`forbidden_filters_.Erase`).
    pub fn unforbid(&mut self, filter: Filter) {
        self.forbidden.remove(filter);
    }

    /// Sets the css-preserve-urls flag.
    pub fn set_css_preserve_urls(&mut self, value: bool) {
        self.css_preserve_urls = value;
    }

    /// Sets the js-preserve-urls flag.
    pub fn set_js_preserve_urls(&mut self, value: bool) {
        self.js_preserve_urls = value;
    }

    /// Sets the image-preserve-urls flag.
    pub fn set_image_preserve_urls(&mut self, value: bool) {
        self.image_preserve_urls = value;
    }

    /// Reads the css-preserve-urls flag.
    #[must_use]
    pub fn css_preserve_urls(&self) -> bool {
        self.css_preserve_urls
    }

    /// Reads the js-preserve-urls flag.
    #[must_use]
    pub fn js_preserve_urls(&self) -> bool {
        self.js_preserve_urls
    }

    /// Reads the image-preserve-urls flag.
    #[must_use]
    pub fn image_preserve_urls(&self) -> bool {
        self.image_preserve_urls
    }

    /// Sets forbid-all-disabled-filters (`Forbidden()` consults this).
    pub fn set_forbid_all_disabled(&mut self, value: bool) {
        self.forbid_all_disabled = value;
    }

    /// Clears enabled and disabled, keeping forbidden, and
    /// re-enables the HTML writer (`ClearFilters`).
    pub fn clear(&mut self) {
        self.enabled = FilterSet::new();
        self.disabled = FilterSet::new();
        self.enable(Filter::HtmlWriterFilter);
    }

    /// Borrows the enabled set (for merge).
    #[must_use]
    pub fn enabled_set(&self) -> FilterSet {
        self.enabled
    }

    /// Borrows the disabled set (for merge).
    #[must_use]
    pub fn disabled_set(&self) -> FilterSet {
        self.disabled
    }

    /// Borrows the forbidden set (for merge).
    #[must_use]
    pub fn forbidden_set(&self) -> FilterSet {
        self.forbidden
    }

    /// Mutably borrows the enabled set (for merge).
    pub fn enabled_set_mut(&mut self) -> &mut FilterSet {
        &mut self.enabled
    }

    /// Mutably borrows the disabled set (for merge).
    pub fn disabled_set_mut(&mut self) -> &mut FilterSet {
        &mut self.disabled
    }

    /// Mutably borrows the forbidden set (for merge).
    pub fn forbidden_set_mut(&mut self) -> &mut FilterSet {
        &mut self.forbidden
    }

    /// Disables every filter not explicitly enabled
    /// (`DisableAllFiltersNotExplicitlyEnabled`).
    pub fn disable_all_not_explicitly_enabled(&mut self) {
        for filter in Filter::ALL {
            if !self.enabled.contains(filter) {
                self.disabled.insert(filter);
            }
        }
    }

    /// Whether a filter is forbidden (`Forbidden()`).
    #[must_use]
    pub fn forbidden(&self, filter: Filter) -> bool {
        self.forbidden.contains(filter)
            || (self.forbid_all_disabled && self.disabled.contains(filter))
    }

    /// Whether a filter is enabled (`Enabled()`).
    ///
    /// Precedence: disabled/forbidden (always lose) > explicitly enabled
    /// (always win) > preserve-urls > level switch.
    #[must_use]
    pub fn enabled(&self, filter: Filter) -> bool {
        if self.disabled.contains(filter) || self.forbidden.contains(filter) {
            return false;
        }
        if self.enabled.contains(filter) {
            return true;
        }
        // Preserve-urls checks consult the off-sets (equivalent to the C++
        // `filter_properties` bits built from the same arrays).
        if self.css_preserve_urls && CSS_PRESERVE_OFF.contains(&filter) {
            return false;
        }
        if self.js_preserve_urls && JS_PRESERVE_OFF.contains(&filter) {
            return false;
        }
        if self.image_preserve_urls && IMAGE_PRESERVE_OFF.contains(&filter) {
            return false;
        }
        match self.level {
            RewriteLevel::TestingCoreFilters => {
                if TEST_FILTERS.contains(&filter) {
                    return true;
                }
                // Fall through to Core, as in C++.
                CORE_FILTERS.contains(&filter)
            }
            RewriteLevel::CoreFilters => CORE_FILTERS.contains(&filter),
            RewriteLevel::OptimizeForBandwidth => BANDWIDTH_FILTERS.contains(&filter),
            RewriteLevel::MobilizeFilters => MOBILIZE_FILTERS.contains(&filter),
            RewriteLevel::AllFilters => !DANGEROUS_FILTERS.contains(&filter),
            RewriteLevel::PassThrough => false,
        }
    }

    /// Whether any add-head filter is enabled (`RequiresAddHead()`).
    #[must_use]
    pub fn requires_add_head(&self) -> bool {
        ADD_HEAD_FILTERS.iter().any(|f| self.enabled(*f))
    }

    /// Adds one name to a set (`AddByNameToFilterSet`).
    ///
    /// Handles public names (case-insensitive), compound aliases, and the
    /// `ResizeMobileImages` → `DelayImages` hard dependency. Returns false
    /// for unknown names (caller reports "Invalid filter name").
    pub fn add_by_name_to_set(&mut self, name: &str, set: FilterSetTarget) -> bool {
        if let Some(filter) = Filter::from_name(name) {
            self.target_mut(set).insert(filter);
            // kResizeMobileImages requires kDelayImages.
            if filter == Filter::ResizeMobileImages {
                self.target_mut(set).insert(Filter::DelayImages);
            }
            return true;
        }
        // Compound names are matched case-sensitively in C++.
        let expanded: Option<&[Filter]> = match name {
            "testing" => None, // handled below (two lists)
            "core" => Some(CORE_FILTERS),
            _ => compound_alias(name),
        };
        if name == "testing" {
            for f in TEST_FILTERS.iter().chain(CORE_FILTERS.iter()) {
                self.target_mut(set).insert(*f);
            }
            return true;
        }
        if let Some(list) = expanded {
            for f in list {
                self.target_mut(set).insert(*f);
            }
            return true;
        }
        false
    }

    fn target_mut(&mut self, set: FilterSetTarget) -> &mut FilterSet {
        match set {
            FilterSetTarget::Enabled => &mut self.enabled,
            FilterSetTarget::Disabled => &mut self.disabled,
            FilterSetTarget::Forbidden => &mut self.forbidden,
        }
    }

    /// Adjusts sets from a comma-separated list
    /// (`AdjustFiltersByCommaSeparatedList`).
    ///
    /// `+x` enables, `-x` disables, bare `x` resets to PassThrough first
    /// (non-incremental). Empty input disables all. Returns false if any
    /// name was unknown (valid names are still applied).
    pub fn adjust_by_comma_list(&mut self, list: &str) -> bool {
        // C++ splits with omit_empty_strings; `names.empty()` is tested on
        // the untrimmed pieces, while whitespace-only tokens are skipped in
        // the loop. So `" "` stays incremental but `""` and `","` reset.
        let pieces: Vec<&str> = list.split(',').filter(|t| !t.is_empty()).collect();
        let mut ok = true;
        let mut non_incremental = pieces.is_empty();
        for piece in pieces {
            let token = piece.trim();
            if token.is_empty() {
                continue;
            }
            if let Some(name) = token.strip_prefix('-') {
                ok &= self.add_by_name_to_set(name, FilterSetTarget::Disabled);
            } else if let Some(name) = token.strip_prefix('+') {
                ok &= self.add_by_name_to_set(name, FilterSetTarget::Enabled);
            } else {
                ok &= self.add_by_name_to_set(token, FilterSetTarget::Enabled);
                non_incremental = true;
            }
        }
        if non_incremental {
            self.set_level(RewriteLevel::PassThrough);
            self.disable_all_not_explicitly_enabled();
        }
        ok
    }

    /// Enables filters from a comma-separated list
    /// (`EnableFiltersByCommaSeparatedList`).
    pub fn enable_by_comma_list(&mut self, list: &str) -> bool {
        Self::apply_comma_list_to_target(self, list, FilterSetTarget::Enabled)
    }

    /// Disables filters from a comma-separated list.
    pub fn disable_by_comma_list(&mut self, list: &str) -> bool {
        Self::apply_comma_list_to_target(self, list, FilterSetTarget::Disabled)
    }

    /// Forbids filters from a comma-separated list.
    pub fn forbid_by_comma_list(&mut self, list: &str) -> bool {
        Self::apply_comma_list_to_target(self, list, FilterSetTarget::Forbidden)
    }

    fn apply_comma_list_to_target(
        state: &mut FilterState,
        list: &str,
        set: FilterSetTarget,
    ) -> bool {
        // `EnableFilters` etc. go through AddCommaSeparatedListToFilterSetState
        // which splits the same way but applies every token to one set.
        let mut ok = true;
        for token in list
            .split(',')
            .filter(|t| !t.is_empty())
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            ok &= state.add_by_name_to_set(token, set);
        }
        ok
    }
}

/// Which filter set a name is added to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterSetTarget {
    Enabled,
    Disabled,
    Forbidden,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn enum_order_and_count() {
        assert_eq!(Filter::ALL.len(), NUM_FILTERS);
        assert_eq!(NUM_FILTERS, 97);
        for (i, f) in Filter::ALL.iter().enumerate() {
            assert_eq!(*f as usize, i, "discriminant drift at {f:?}");
            assert_eq!(Filter::from_repr(i as u8), Some(*f));
        }
        assert_eq!(Filter::from_repr(97), None);
        // Boundary sentinels from C++ (`kFirstFilter`, last filter).
        assert_eq!(Filter::ALL[0], Filter::AddBaseTag);
        assert_eq!(Filter::ALL[94], Filter::StripScripts);
        // Rust-port extensions appended past the C++ set.
        assert_eq!(Filter::ALL[95], Filter::CompressZstd);
        assert_eq!(Filter::CompressZstd.id(), "cz");
        assert_eq!(Filter::CompressZstd.display_name(), "Compress Zstd");
        assert_eq!(Filter::ALL[96], Filter::PreloadCriticalFonts);
        assert_eq!(Filter::PreloadCriticalFonts.id(), "pf");
        assert_eq!(
            Filter::PreloadCriticalFonts.display_name(),
            "Preload Critical Fonts"
        );
    }

    #[test]
    fn related_tables_shape() {
        // Filter lists are enum-ordered (C++ `CHECK`s this).
        assert_enum_ordered("image", IMAGE_RELATED_FILTERS);
        assert_enum_ordered("css", CSS_RELATED_FILTERS);
        assert_eq!(15, IMAGE_RELATED_FILTERS.len());
        assert_eq!(23, CSS_RELATED_FILTERS.len());
        // CSS filters are the image+css union.
        for f in IMAGE_RELATED_FILTERS {
            assert!(CSS_RELATED_FILTERS.contains(f));
        }
        // Option lists are sorted (`Initialize()` sorts after
        // concatenating), so every listed option is found.
        assert_eq!(14, IMAGE_RELATED_OPTIONS.len());
        assert_eq!(17, CSS_RELATED_OPTIONS.len());
        for name in IMAGE_RELATED_OPTIONS {
            assert!(IMAGE_RELATED_OPTIONS.binary_search(name).is_ok(), "{name}");
        }
        for name in CSS_RELATED_OPTIONS {
            assert!(CSS_RELATED_OPTIONS.binary_search(name).is_ok(), "{name}");
        }
        assert!(IMAGE_RELATED_OPTIONS
            .binary_search(&"CssInlineMaxBytes")
            .is_err());
        // Filter lists search correctly.
        assert!(IMAGE_RELATED_FILTERS
            .binary_search(&Filter::RecompressJpeg)
            .is_ok());
        assert!(IMAGE_RELATED_FILTERS
            .binary_search(&Filter::CombineCss)
            .is_err());
        // Decoding ids: cf/ic carry tables, the other seven are
        // registered-but-empty, the rest unknown.
        let (filters, options) = related_tables_for_decoding_id("cf").unwrap();
        assert_eq!(23, filters.len());
        assert_eq!(17, options.len());
        let (filters, options) = related_tables_for_decoding_id("ic").unwrap();
        assert_eq!(15, filters.len());
        assert_eq!(14, options.len());
        for id in ["ce", "cc", "jc", "jm", "is", "ls", "sm"] {
            let (filters, options) = related_tables_for_decoding_id(id).unwrap();
            assert!(filters.is_empty(), "{id}");
            assert!(options.is_empty(), "{id}");
        }
        assert_eq!(None, related_tables_for_decoding_id("if"));
        assert_eq!(None, related_tables_for_decoding_id("zz"));
        assert_eq!(None, related_tables_for_decoding_id(""));
    }

    /// Ports `CheckFilterSetOrdering`: the audited set arrays must list
    /// filters in strictly increasing enum order.
    fn assert_enum_ordered(name: &str, set: &[Filter]) {
        for pair in set.windows(2) {
            assert!(
                (pair[1] as usize) > (pair[0] as usize),
                "{name}: {:?} must precede {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn filter_sets_in_enum_order() {
        assert_enum_ordered("CORE_FILTERS", CORE_FILTERS);
        assert_enum_ordered("TEST_FILTERS", TEST_FILTERS);
        assert_enum_ordered("DANGEROUS_FILTERS", DANGEROUS_FILTERS);
        assert_enum_ordered("IMAGE_PRESERVE_OFF", IMAGE_PRESERVE_OFF);
        assert_enum_ordered("JS_PRESERVE_OFF", JS_PRESERVE_OFF);
        assert_enum_ordered("CSS_PRESERVE_OFF", CSS_PRESERVE_OFF);
        assert_enum_ordered("ADD_HEAD_FILTERS", ADD_HEAD_FILTERS);
    }

    #[test]
    fn ids_unique_and_spot_check() {
        // Mirrors the C++ `DCHECK` for duplicate filter ids.
        let ids: HashSet<&str> = Filter::ALL.iter().map(|f| f.id()).collect();
        assert_eq!(ids.len(), NUM_FILTERS);
        assert_eq!(Filter::CombineCss.id(), "cc");
        assert_eq!(Filter::RewriteCss.id(), "cf");
        assert_eq!(Filter::RewriteJavascriptExternal.id(), "jm");
        assert_eq!(Filter::RewriteJavascriptInline.id(), "jj");
        assert_eq!(Filter::SpriteImages.id(), "is");
        assert_eq!(Filter::InPlaceOptimizeForBrowser.id(), "io");
        assert_eq!(Filter::HtmlWriterFilter.id(), "hw");
        assert_eq!(Filter::MobilizePrecompute.id(), "mob_precompute");
        assert_eq!(Filter::HintPreloadSubresources.id(), "hpsr");
        for f in Filter::ALL {
            assert_eq!(Filter::from_id(f.id()), Some(f));
        }
        assert_eq!(Filter::from_id("zz"), None);
        assert_eq!(Filter::from_id("CC"), None); // case-sensitive, as in C++
    }

    #[test]
    fn display_names_spot_check() {
        assert_eq!(Filter::AddHead.display_name(), "Add Head");
        assert_eq!(
            Filter::CanonicalizeJavascriptLibraries.display_name(),
            "Canonicalize Javascript library URLs"
        );
        // Byte-identical trailing space from the C++ table.
        assert_eq!(
            Filter::FallbackRewriteCssUrls.display_name(),
            "Fallback Rewrite Css "
        );
        assert_eq!(
            Filter::ConvertToWebpLossless.display_name(),
            "When converting images to WebP, prefer lossless conversions"
        );
    }

    #[test]
    fn name_map_aliases_and_case() {
        assert_eq!(
            Filter::from_name("collapse_whitespace"),
            Some(Filter::CollapseWhitespace)
        );
        assert_eq!(
            Filter::from_name("Collapse_Whitespace"),
            Some(Filter::CollapseWhitespace)
        );
        assert_eq!(
            Filter::from_name("compress_zstd"),
            Some(Filter::CompressZstd)
        );
        assert_eq!(
            Filter::from_name("Compress_Zstd"),
            Some(Filter::CompressZstd)
        );
        assert_eq!(
            Filter::from_name("preload_critical_fonts"),
            Some(Filter::PreloadCriticalFonts)
        );
        assert_eq!(
            Filter::from_name("Preload_Critical_Fonts"),
            Some(Filter::PreloadCriticalFonts)
        );
        // Aliases.
        assert_eq!(
            Filter::from_name("inline_preview_images"),
            Some(Filter::DelayImages)
        );
        assert_eq!(
            Filter::from_name("insert_img_dimensions"),
            Some(Filter::InsertImageDimensions)
        );
        assert_eq!(
            Filter::from_name("insert_image_dimensions"),
            Some(Filter::InsertImageDimensions)
        );
        assert_eq!(Filter::from_name("trim_urls"), Some(Filter::LeftTrimUrls));
        // Internal-only filters have no public name.
        assert_eq!(Filter::from_name("delay_images"), None);
        assert_eq!(Filter::from_name("handle_noscript_redirect"), None);
        assert_eq!(Filter::from_name("html_writer_filter"), None);
        assert_eq!(Filter::from_name("serve_deprecation_notice"), None);
        assert_eq!(Filter::from_name("bogus"), None);
    }

    #[test]
    fn set_sizes_and_enum_order() {
        // C++ `CheckFilterSetOrdering` requires enum order; assert sorted.
        for set in [
            CORE_FILTERS,
            BANDWIDTH_FILTERS,
            TEST_FILTERS,
            DANGEROUS_FILTERS,
            REQUIRES_SCRIPT_EXECUTION,
            ADD_HEAD_FILTERS,
            MOBILIZE_FILTERS,
            IMAGE_PRESERVE_OFF,
            JS_PRESERVE_OFF,
            CSS_PRESERVE_OFF,
        ] {
            let keys: Vec<usize> = set.iter().map(|f| *f as usize).collect();
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            assert_eq!(keys, sorted, "set not in enum order");
            assert_eq!(
                keys.iter().collect::<HashSet<_>>().len(),
                keys.len(),
                "duplicate in set"
            );
        }
        assert_eq!(CORE_FILTERS.len(), 31);
        assert!(CORE_FILTERS.contains(&Filter::PreloadCriticalFonts));
        assert_eq!(BANDWIDTH_FILTERS.len(), 15);
        assert_eq!(TEST_FILTERS.len(), 17);
        assert_eq!(DANGEROUS_FILTERS.len(), 14);
        assert_eq!(REQUIRES_SCRIPT_EXECUTION.len(), 9);
        assert_eq!(ADD_HEAD_FILTERS.len(), 10);
        assert_eq!(MOBILIZE_FILTERS.len(), 3);
        assert_eq!(IMAGE_PRESERVE_OFF.len(), 8);
        assert_eq!(JS_PRESERVE_OFF.len(), 6);
        assert_eq!(CSS_PRESERVE_OFF.len(), 7);
    }

    #[test]
    fn compound_alias_containment() {
        assert_eq!(COMPOUND_REWRITE_IMAGES.len(), 14);
        assert_eq!(COMPOUND_RECOMPRESS_IMAGES.len(), 11);
        assert_eq!(COMPOUND_EXTEND_CACHE.len(), 3);
        assert_eq!(COMPOUND_REWRITE_JAVASCRIPT.len(), 2);
        let core: HashSet<Filter> = CORE_FILTERS.iter().copied().collect();
        // Every rewrite_images/extend_cache member is in Core (C++ comments).
        for f in COMPOUND_REWRITE_IMAGES
            .iter()
            .chain(COMPOUND_EXTEND_CACHE.iter())
        {
            assert!(core.contains(f), "{f:?} not in Core");
        }
        let ri: HashSet<Filter> = COMPOUND_REWRITE_IMAGES.iter().copied().collect();
        for f in COMPOUND_RECOMPRESS_IMAGES {
            assert!(ri.contains(f), "{f:?} not in rewrite_images");
        }
        // extend_cache must NOT include pdfs.
        assert!(!COMPOUND_EXTEND_CACHE.contains(&Filter::ExtendCachePdfs));
        assert_eq!(
            compound_alias("rewrite_images"),
            Some(COMPOUND_REWRITE_IMAGES)
        );
        assert_eq!(compound_alias("core"), None); // handled via CORE_FILTERS
        assert_eq!(compound_alias("Rewrite_Images"), None); // case-sensitive
    }

    #[test]
    fn enabled_precedence() {
        // PassThrough enables nothing.
        let state = FilterState::new();
        assert!(!state.enabled(Filter::RewriteCss));
        // Core level.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(state.enabled(Filter::RewriteCss));
        assert!(!state.enabled(Filter::SpriteImages)); // not Core
                                                       // Explicitly enabled always wins (even non-Core at Core level).
        state.enable(Filter::SpriteImages);
        assert!(state.enabled(Filter::SpriteImages));
        // Disabled always loses (even Core at Core level).
        state.disable(Filter::RewriteCss);
        assert!(!state.enabled(Filter::RewriteCss));
        // Forbidden always loses (even explicitly enabled).
        state.enable(Filter::CollapseWhitespace);
        state.forbid(Filter::CollapseWhitespace);
        assert!(!state.enabled(Filter::CollapseWhitespace));
        // Preserve loses to explicit enable, wins over level.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        state.set_css_preserve_urls(true);
        assert!(!state.enabled(Filter::CombineCss)); // Core but css-off-set
        assert!(state.enabled(Filter::RewriteCss)); // Core, not in off-set
        state.enable(Filter::CombineCss);
        assert!(state.enabled(Filter::CombineCss));
        // TestingCore falls through to Core.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::TestingCoreFilters);
        assert!(state.enabled(Filter::RewriteDomains)); // test-only
        assert!(state.enabled(Filter::RewriteCss)); // core via fallthrough
        assert!(!state.enabled(Filter::StripScripts)); // neither
                                                       // All enables everything except dangerous.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::AllFilters);
        assert!(!state.enabled(Filter::StripScripts));
        assert!(!state.enabled(Filter::Mobilize));
        assert!(state.enabled(Filter::CollapseWhitespace));
        // compress_zstd is safe (negotiated encoding): in All, but
        // opt-in everywhere else.
        assert!(state.enabled(Filter::CompressZstd));
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(!state.enabled(Filter::CompressZstd));
        state.enable(Filter::CompressZstd);
        assert!(state.enabled(Filter::CompressZstd));
        state.disable(Filter::CompressZstd);
        assert!(!state.enabled(Filter::CompressZstd));
        // Mobilize level.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::MobilizeFilters);
        assert!(state.enabled(Filter::Mobilize));
        assert!(state.enabled(Filter::RewriteCss));
        assert!(!state.enabled(Filter::CombineCss));
        // Bandwidth excludes URL-mutating filters.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::OptimizeForBandwidth);
        assert!(state.enabled(Filter::InPlaceOptimizeForBrowser));
        assert!(!state.enabled(Filter::CombineCss));
    }

    #[test]
    fn forbid_all_disabled_and_requires_add_head() {
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        state.disable(Filter::RewriteCss);
        assert!(!state.forbidden(Filter::RewriteCss));
        state.set_forbid_all_disabled(true);
        assert!(state.forbidden(Filter::RewriteCss));
        assert!(!state.forbidden(Filter::RewriteDomains));

        // AddHead is Core and still enabled, so head is still required.
        assert!(state.requires_add_head());
        state.disable(Filter::AddHead);
        assert!(!state.requires_add_head());
        // Explicit enable at PassThrough also requires head.
        let mut state = FilterState::new();
        state.enable(Filter::MoveCssToHead);
        assert!(state.requires_add_head());
    }

    #[test]
    fn resize_mobile_auto_inserts_delay_images() {
        let mut state = FilterState::new();
        assert!(state.add_by_name_to_set("resize_mobile_images", FilterSetTarget::Enabled));
        assert!(state.enabled(Filter::ResizeMobileImages));
        assert!(state.enabled(Filter::DelayImages));
        // ... in whichever set was targeted.
        let mut state = FilterState::new();
        assert!(state.add_by_name_to_set("resize_mobile_images", FilterSetTarget::Disabled));
        assert!(!state.enabled(Filter::DelayImages));
    }

    #[test]
    fn add_by_name_compounds_and_testing() {
        let mut state = FilterState::new();
        assert!(state.add_by_name_to_set("core", FilterSetTarget::Enabled));
        assert!(state.enabled(Filter::RewriteCss));
        assert!(!state.enabled(Filter::SpriteImages));
        let mut state = FilterState::new();
        assert!(state.add_by_name_to_set("testing", FilterSetTarget::Enabled));
        assert!(state.enabled(Filter::RewriteDomains)); // test
        assert!(state.enabled(Filter::RewriteCss)); // core included
        let mut state = FilterState::new();
        assert!(state.add_by_name_to_set("rewrite_images", FilterSetTarget::Enabled));
        assert!(state.enabled(Filter::RecompressJpeg));
        assert!(!state.enabled(Filter::RewriteCss));
        assert!(!state.add_by_name_to_set("bogus", FilterSetTarget::Enabled));
    }

    #[test]
    fn comma_list_incremental_vs_reset() {
        // Incremental: level preserved.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(state.adjust_by_comma_list("+sprite_images,-rewrite_css"));
        assert_eq!(state.level(), RewriteLevel::CoreFilters);
        assert!(state.enabled(Filter::SpriteImages));
        assert!(!state.enabled(Filter::RewriteCss));
        assert!(state.enabled(Filter::CombineCss)); // untouched Core member
                                                    // Bare token resets to PassThrough (only listed stay enabled).
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(state.adjust_by_comma_list("+sprite_images,collapse_whitespace"));
        assert_eq!(state.level(), RewriteLevel::PassThrough);
        assert!(state.enabled(Filter::SpriteImages));
        assert!(state.enabled(Filter::CollapseWhitespace));
        assert!(!state.enabled(Filter::RewriteCss));
        // Empty input disables all.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(state.adjust_by_comma_list(""));
        assert_eq!(state.level(), RewriteLevel::PassThrough);
        assert!(!state.enabled(Filter::RewriteCss));
        // "," holds only omitted empties -> same as empty.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(state.adjust_by_comma_list(","));
        assert!(!state.enabled(Filter::RewriteCss));
        // Whitespace-only tokens are skipped, list stays incremental.
        let mut state = FilterState::new();
        state.set_level(RewriteLevel::CoreFilters);
        assert!(state.adjust_by_comma_list(" "));
        assert_eq!(state.level(), RewriteLevel::CoreFilters);
        assert!(state.enabled(Filter::RewriteCss));
        // Unknown names fail but valid ones still apply.
        let mut state = FilterState::new();
        assert!(!state.adjust_by_comma_list("+rewrite_css,+bogus"));
        assert!(state.enabled(Filter::RewriteCss));
    }

    #[test]
    fn enable_disable_forbid_lists() {
        let mut state = FilterState::new();
        assert!(state.enable_by_comma_list("rewrite_css,rewrite_images"));
        assert!(state.enabled(Filter::RewriteCss));
        assert!(state.enabled(Filter::RecompressJpeg));
        assert!(!state.enable_by_comma_list("bogus"));
        assert!(state.disable_by_comma_list("rewrite_css"));
        assert!(!state.enabled(Filter::RewriteCss));
        assert!(state.forbid_by_comma_list("rewrite_css"));
        assert!(state.forbidden(Filter::RewriteCss));
    }

    #[test]
    fn rewrite_level_parse() {
        assert_eq!(
            RewriteLevel::parse("CoreFilters"),
            Some(RewriteLevel::CoreFilters)
        );
        assert_eq!(
            RewriteLevel::parse("corefilters"),
            Some(RewriteLevel::CoreFilters)
        );
        assert_eq!(
            RewriteLevel::parse("OptimizeForBandwidth"),
            Some(RewriteLevel::OptimizeForBandwidth)
        );
        assert_eq!(
            RewriteLevel::parse("TestingCoreFilters"),
            Some(RewriteLevel::TestingCoreFilters)
        );
        assert_eq!(RewriteLevel::parse(""), None);
        assert_eq!(RewriteLevel::parse("bogus"), None);
        assert_eq!(RewriteLevel::CoreFilters.name(), "CoreFilters");
        assert_eq!(RewriteLevel::default(), RewriteLevel::PassThrough);
    }

    #[test]
    fn filter_set_basics() {
        let mut set = FilterSet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
        set.insert(Filter::RewriteCss);
        set.insert(Filter::StripScripts); // bit 94, second word
        assert!(set.contains(Filter::RewriteCss));
        assert!(!set.contains(Filter::CombineCss));
        assert_eq!(set.len(), 2);
        set.remove(Filter::RewriteCss);
        assert_eq!(set.len(), 1);
        let set = FilterSet::from_slice(CORE_FILTERS);
        assert_eq!(set.len(), 31);
        let round: Vec<Filter> = set.iter().collect();
        assert_eq!(round.len(), 31);
        // Iteration is in enum order.
        let mut sorted = round.clone();
        sorted.sort();
        assert_eq!(round, sorted);
    }
}
