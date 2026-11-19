//! Filter-chain builder (`RewriteDriver::AddFilters`).
//!
//! Ports `AddPreRenderFilters`/`AddPostRenderFilters` (plus event-listener
//! setup and writer attachment) as a pure function of [`RewriteOptions`]:
//! [`build_chain`] returns the exact ordered [`ChainPlan`] — listeners,
//! early pre-render, pre-render, and post-render filters with the terminal
//! writer — for any options + [`ChainContext`].
//!
//! ORDER NOTE (mirrors the C++ comment on `AddPreRenderFilters`): the order
//! built here is the order filters actually apply. A change here must also
//! update `install/common/pagespeed.conf.template` and the config-filters
//! docs, which list filters in application order for site owners.
//!
//! Filter implementations land incrementally; this module pins identity,
//! order, and gating so each filter plugs into a verified slot.

use ps_config::filter::{Filter, RewriteLevel};
use ps_config::options::{RewriteOptions, Value};

/// One filter slot in the chain, identified by its C++ class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChainFilter {
    // Event listeners.
    FlushHtml,
    AmpDocument,
    // Early pre-render.
    DomStats,
    StripSubresourceHints,
    DecodeRewrittenUrls,
    ResponsiveImageFirst,
    AddHead,
    BaseTag,
    AddIds,
    // Pre-render.
    StripScripts,
    StripLegacyPolyfills,
    CriticalImagesBeacon,
    MakeShowAdsAsync,
    CssInlineImportToLink,
    CssOutline,
    GoogleFontCssInline,
    CssMoveToHead,
    CssCombine,
    CssFilter,
    CriticalCssBeacon,
    CriticalSelector,
    CssInline,
    JsOutline,
    GoogleAnalytics,
    InsertGA,
    JsCombine,
    Javascript,
    JsInline,
    ImageRewrite,
    RemoveComments,
    ElideAttributes,
    CacheExtender,
    ImageCombine,
    LocalStorageCache,
    CollectDependencies,
    PushPreload,
    // Post-render.
    ResponsiveImageSecond,
    RewrittenContentScanning,
    InsertDnsPrefetch,
    PrioritizeLcpImages,
    InsertAmpLink,
    AddInstrumentation,
    DeferIframe,
    JsDisable,
    JsDeferDisabled,
    FixReflow,
    DeterministicJs,
    MetaTag,
    DelayImages,
    DedupInlinedImages,
    LazyloadImages,
    SupportNoscript,
    HandleNoscriptRedirect,
    RedirectOnSizeLimit,
    Pedantic,
    DomainRewrite,
    UrlLeftTrim,
    RemoveQuotes,
    CollapseWhitespace,
    Debug,
    HtmlWriter,
}

impl ChainFilter {
    /// C++ class name (golden-test identity).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ChainFilter::FlushHtml => "FlushHtmlFilter",
            ChainFilter::AmpDocument => "AmpDocumentFilter",
            ChainFilter::DomStats => "DomStatsFilter",
            ChainFilter::StripSubresourceHints => "StripSubresourceHintsFilter",
            ChainFilter::DecodeRewrittenUrls => "DecodeRewrittenUrlsFilter",
            ChainFilter::ResponsiveImageFirst => "ResponsiveImageFirstFilter",
            ChainFilter::AddHead => "AddHeadFilter",
            ChainFilter::BaseTag => "BaseTagFilter",
            ChainFilter::AddIds => "AddIdsFilter",
            ChainFilter::StripScripts => "StripScriptsFilter",
            ChainFilter::StripLegacyPolyfills => "StripLegacyPolyfillsFilter",
            ChainFilter::CriticalImagesBeacon => "CriticalImagesBeaconFilter",
            ChainFilter::MakeShowAdsAsync => "MakeShowAdsAsyncFilter",
            ChainFilter::CssInlineImportToLink => "CssInlineImportToLinkFilter",
            ChainFilter::CssOutline => "CssOutlineFilter",
            ChainFilter::GoogleFontCssInline => "GoogleFontCssInlineFilter",
            ChainFilter::CssMoveToHead => "CssMoveToHeadFilter",
            ChainFilter::CssCombine => "CssCombineFilter",
            ChainFilter::CssFilter => "CssFilter",
            ChainFilter::CriticalCssBeacon => "CriticalCssBeaconFilter",
            ChainFilter::CriticalSelector => "CriticalSelectorFilter",
            ChainFilter::CssInline => "CssInlineFilter",
            ChainFilter::JsOutline => "JsOutlineFilter",
            ChainFilter::GoogleAnalytics => "GoogleAnalyticsFilter",
            ChainFilter::InsertGA => "InsertGAFilter",
            ChainFilter::JsCombine => "JsCombineFilter",
            ChainFilter::Javascript => "JavascriptFilter",
            ChainFilter::JsInline => "JsInlineFilter",
            ChainFilter::ImageRewrite => "ImageRewriteFilter",
            ChainFilter::RemoveComments => "RemoveCommentsFilter",
            ChainFilter::ElideAttributes => "ElideAttributesFilter",
            ChainFilter::CacheExtender => "CacheExtenderFilter",
            ChainFilter::ImageCombine => "ImageCombineFilter",
            ChainFilter::LocalStorageCache => "LocalStorageCacheFilter",
            ChainFilter::CollectDependencies => "CollectDependenciesFilter",
            ChainFilter::PushPreload => "PushPreloadFilter",
            ChainFilter::ResponsiveImageSecond => "ResponsiveImageSecondFilter",
            ChainFilter::RewrittenContentScanning => "RewrittenContentScanningFilter",
            ChainFilter::InsertDnsPrefetch => "InsertDnsPrefetchFilter",
            ChainFilter::PrioritizeLcpImages => "PrioritizeLcpImagesFilter",
            ChainFilter::InsertAmpLink => "InsertAmpLinkFilter",
            ChainFilter::AddInstrumentation => "AddInstrumentationFilter",
            ChainFilter::DeferIframe => "DeferIframeFilter",
            ChainFilter::JsDisable => "JsDisableFilter",
            ChainFilter::JsDeferDisabled => "JsDeferDisabledFilter",
            ChainFilter::FixReflow => "FixReflowFilter",
            ChainFilter::DeterministicJs => "DeterministicJsFilter",
            ChainFilter::MetaTag => "MetaTagFilter",
            ChainFilter::DelayImages => "DelayImagesFilter",
            ChainFilter::DedupInlinedImages => "DedupInlinedImagesFilter",
            ChainFilter::LazyloadImages => "LazyloadImagesFilter",
            ChainFilter::SupportNoscript => "SupportNoscriptFilter",
            ChainFilter::HandleNoscriptRedirect => "HandleNoscriptRedirectFilter",
            ChainFilter::RedirectOnSizeLimit => "RedirectOnSizeLimitFilter",
            ChainFilter::Pedantic => "PedanticFilter",
            ChainFilter::DomainRewrite => "DomainRewriteFilter",
            ChainFilter::UrlLeftTrim => "UrlLeftTrimFilter",
            ChainFilter::RemoveQuotes => "HtmlAttributeQuoteRemoval",
            ChainFilter::CollapseWhitespace => "CollapseWhitespaceFilter",
            ChainFilter::Debug => "DebugFilter",
            ChainFilter::HtmlWriter => "HtmlWriterFilter",
        }
    }

    /// Resource-filter id (`EnableRewriteFilter`, e.g. `cf`), if any.
    #[must_use]
    pub fn resource_id(self) -> Option<&'static str> {
        match self {
            ChainFilter::CacheExtender => Some("ce"),
            ChainFilter::CssCombine => Some("cc"),
            ChainFilter::CssFilter => Some("cf"),
            ChainFilter::ImageCombine => Some("is"),
            ChainFilter::ImageRewrite => Some("ic"),
            ChainFilter::JsCombine => Some("jc"),
            ChainFilter::Javascript => Some("jm"),
            ChainFilter::LocalStorageCache => Some("ls"),
            _ => None,
        }
    }
}

/// Chain phase (`FlushAsync` runs early-pre, then pre, then — after rewrite
/// initiation — the post-render parse chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Listener,
    EarlyPreRender,
    PreRender,
    PostRender,
}

/// Ordered filter plan for one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainPlan {
    /// Event listeners (run per event, not per flush window).
    pub listeners: Vec<ChainFilter>,
    /// Early pre-render filters (before resource scanning).
    pub early_pre: Vec<ChainFilter>,
    /// Pre-render filters (resource rewrites initiate after these).
    pub pre: Vec<ChainFilter>,
    /// Post-render filters (the parse chain; writer terminal when on).
    pub post: Vec<ChainFilter>,
    /// Parser byte cap (`set_size_limit(MaxHtmlParseBytes)`), when capped.
    pub size_limit: Option<i64>,
}

impl ChainPlan {
    /// Names per phase, for golden tests.
    #[must_use]
    pub fn phase_names(&self, phase: Phase) -> Vec<&'static str> {
        let list = match phase {
            Phase::Listener => &self.listeners,
            Phase::EarlyPreRender => &self.early_pre,
            Phase::PreRender => &self.pre,
            Phase::PostRender => &self.post,
        };
        list.iter().map(|filter| filter.name()).collect()
    }
}

/// Non-option inputs to chain building (server/factory state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainContext {
    /// `factory()->UseBeaconResultsInFilters()`.
    pub use_beacon_results: bool,
    /// `page_property_cache()->enabled()`.
    pub property_cache_enabled: bool,
}

impl ChainContext {
    #[must_use]
    pub fn new(use_beacon_results: bool, property_cache_enabled: bool) -> Self {
        ChainContext {
            use_beacon_results,
            property_cache_enabled,
        }
    }
}

// -- Option readers -----------------------------------------------------

fn opt_bool(options: &RewriteOptions, id: &str) -> bool {
    match options.get_by_id(id) {
        Some(Value::Bool(value)) => value,
        other => panic!("option {id} missing or not a bool: {other:?}"),
    }
}

fn opt_int(options: &RewriteOptions, id: &str) -> i64 {
    match options.get_by_id(id) {
        Some(Value::Int(value)) => value,
        other => panic!("option {id} missing or not an int: {other:?}"),
    }
}

fn opt_str(options: &RewriteOptions, id: &str) -> String {
    match options.get_by_id(id) {
        Some(Value::Str(value)) => value,
        other => panic!("option {id} missing or not a string: {other:?}"),
    }
}

/// Level-implied boolean (`CheckLevelSpecificOption`): true when the
/// level matches and the option was never explicitly set.
fn check_level_option(options: &RewriteOptions, level: RewriteLevel, id: &str) -> bool {
    if options.was_set(id) || options.filters().level() != level {
        opt_bool(options, id)
    } else {
        true
    }
}

fn check_bandwidth_option(options: &RewriteOptions, id: &str) -> bool {
    check_level_option(options, RewriteLevel::OptimizeForBandwidth, id)
}

fn check_mobilize_option(options: &RewriteOptions, id: &str) -> bool {
    check_level_option(options, RewriteLevel::MobilizeFilters, id)
}

fn css_preserve_urls(options: &RewriteOptions) -> bool {
    check_bandwidth_option(options, "cpu") || check_mobilize_option(options, "cpu")
}

fn js_preserve_urls(options: &RewriteOptions) -> bool {
    check_bandwidth_option(options, "jpu")
}

fn image_preserve_urls(options: &RewriteOptions) -> bool {
    check_bandwidth_option(options, "ipu")
}

fn enabled(options: &RewriteOptions, filter: Filter) -> bool {
    options.filters().enabled(filter)
}

fn forbidden(options: &RewriteOptions, filter: Filter) -> bool {
    options.filters().forbidden(filter)
}

/// Any image optimization filter on (`ImageOptimizationEnabled`).
fn image_optimization_enabled(options: &RewriteOptions) -> bool {
    const IMAGE_OPT: &[Filter] = &[
        Filter::RecompressJpeg,
        Filter::RecompressPng,
        Filter::RecompressWebp,
        Filter::ConvertGifToPng,
        Filter::ConvertJpegToProgressive,
        Filter::ConvertPngToJpeg,
        Filter::ConvertJpegToWebp,
        Filter::ConvertToAvif,
        Filter::ConvertToWebpAnimated,
        Filter::ConvertToWebpLossless,
    ];
    IMAGE_OPT.iter().any(|filter| enabled(options, *filter))
}

/// Flatten-CSS-imports active (`FlattenCssImportsEnabled`).
fn flatten_css_imports_enabled(options: &RewriteOptions) -> bool {
    enabled(options, Filter::FlattenCssImports)
        || (!forbidden(options, Filter::FlattenCssImports)
            && (enabled(options, Filter::PrioritizeCriticalCss)
                || enabled(options, Filter::ComputeCriticalCss)))
}

/// Dependencies cohort needed (`NeedsDependenciesCohort`).
fn needs_dependencies_cohort(options: &RewriteOptions) -> bool {
    enabled(options, Filter::ExperimentHttp2) || enabled(options, Filter::HintPreloadSubresources)
}

/// Critical-images beacon active (`is_critical_images_beacon_enabled`).
fn critical_images_beacon_enabled(options: &RewriteOptions, ctx: &ChainContext) -> bool {
    (enabled(options, Filter::LazyloadImages)
        || enabled(options, Filter::InlineImages)
        || enabled(options, Filter::DelayImages)
        || enabled(options, Filter::PrioritizeLcpImages)
        || enabled(options, Filter::ResizeToRenderedImageDimensions))
        && opt_bool(options, "cibe")
        && ctx.use_beacon_results
        && ctx.property_cache_enabled
}

/// Domain mappings/shards configured (`can_rewrite_domains`, minus the
/// runtime-only proxy suffix, which is empty at chain-build time).
fn can_rewrite_domains(options: &RewriteOptions) -> bool {
    !options.domain_mappings().is_empty() || !options.shards().is_empty()
}

/// Builds the ordered filter plan (`AddFilters`).
#[must_use]
pub fn build_chain(options: &RewriteOptions, ctx: &ChainContext) -> ChainPlan {
    let mut plan = ChainPlan {
        listeners: Vec::new(),
        early_pre: Vec::new(),
        pre: Vec::new(),
        post: Vec::new(),
        size_limit: None,
    };

    // Event listeners.
    if opt_bool(options, "fh") {
        plan.listeners.push(ChainFilter::FlushHtml);
    }
    plan.listeners.push(ChainFilter::AmpDocument);

    // Early pre-render.
    if enabled(options, Filter::ComputeStatistics) {
        plan.early_pre.push(ChainFilter::DomStats);
    }
    if !opt_bool(options, "psrh") {
        plan.early_pre.push(ChainFilter::StripSubresourceHints);
    }
    if enabled(options, Filter::DecodeRewrittenUrls) {
        plan.early_pre.push(ChainFilter::DecodeRewrittenUrls);
    }
    if enabled(options, Filter::ResponsiveImages) && enabled(options, Filter::ResizeImages) {
        plan.early_pre.push(ChainFilter::ResponsiveImageFirst);
        // Second phase runs post-render, but registers here, so first.
        plan.post.push(ChainFilter::ResponsiveImageSecond);
    }
    if options.filters().requires_add_head() {
        plan.early_pre.push(ChainFilter::AddHead);
    }
    if enabled(options, Filter::AddBaseTag) {
        plan.early_pre.push(ChainFilter::BaseTag);
    }
    if enabled(options, Filter::AddIds) {
        plan.early_pre.push(ChainFilter::AddIds);
    }

    // Pre-render, in application order.
    if enabled(options, Filter::StripScripts) {
        plan.pre.push(ChainFilter::StripScripts);
    }
    if enabled(options, Filter::StripLegacyPolyfills) {
        plan.pre.push(ChainFilter::StripLegacyPolyfills);
    }
    if critical_images_beacon_enabled(options, ctx) {
        plan.pre.push(ChainFilter::CriticalImagesBeacon);
    }
    if enabled(options, Filter::MakeShowAdsAsync) {
        plan.pre.push(ChainFilter::MakeShowAdsAsync);
    }
    if enabled(options, Filter::InlineImportToLink)
        || (!forbidden(options, Filter::InlineImportToLink)
            && (enabled(options, Filter::PrioritizeCriticalCss)
                || enabled(options, Filter::ComputeCriticalCss)))
    {
        plan.pre.push(ChainFilter::CssInlineImportToLink);
    }
    if !enabled(options, Filter::PrioritizeCriticalCss) && enabled(options, Filter::OutlineCss) {
        plan.pre.push(ChainFilter::CssOutline);
    }
    if enabled(options, Filter::InlineGoogleFontCss) {
        plan.pre.push(ChainFilter::GoogleFontCssInline);
    }
    if enabled(options, Filter::MoveCssToHead) || enabled(options, Filter::MoveCssAboveScripts) {
        plan.pre.push(ChainFilter::CssMoveToHead);
    }
    if enabled(options, Filter::CombineCss) {
        plan.pre.push(ChainFilter::CssCombine);
    }
    if (enabled(options, Filter::RewriteCss)
        || (!forbidden(options, Filter::RewriteCss) && flatten_css_imports_enabled(options)))
        && (!css_preserve_urls(options) || check_bandwidth_option(options, "ipprc"))
    {
        plan.pre.push(ChainFilter::CssFilter);
    }
    if (enabled(options, Filter::PrioritizeCriticalCss) && ctx.use_beacon_results)
        || enabled(options, Filter::ComputeCriticalCss)
    {
        plan.pre.push(ChainFilter::CriticalCssBeacon);
    }
    if enabled(options, Filter::PrioritizeCriticalCss) {
        plan.pre.push(ChainFilter::CriticalSelector);
    }
    if enabled(options, Filter::InlineCss) {
        plan.pre.push(ChainFilter::CssInline);
    }
    if enabled(options, Filter::OutlineJavascript) {
        plan.pre.push(ChainFilter::JsOutline);
    }
    if enabled(options, Filter::MakeGoogleAnalyticsAsync) {
        plan.pre.push(ChainFilter::GoogleAnalytics);
    }
    if (enabled(options, Filter::InsertGA) || opt_bool(options, "fur"))
        && !opt_str(options, "ig").is_empty()
    {
        plan.pre.push(ChainFilter::InsertGA);
    }
    if enabled(options, Filter::CombineJavascript) {
        plan.pre.push(ChainFilter::JsCombine);
    }
    if (enabled(options, Filter::RewriteJavascriptExternal)
        || enabled(options, Filter::RewriteJavascriptInline)
        || enabled(options, Filter::CanonicalizeJavascriptLibraries))
        && (!js_preserve_urls(options)
            || check_bandwidth_option(options, "ipprj")
            || enabled(options, Filter::RewriteJavascriptInline))
    {
        plan.pre.push(ChainFilter::Javascript);
    }
    if enabled(options, Filter::InlineJavascript) {
        plan.pre.push(ChainFilter::JsInline);
    }
    if (enabled(options, Filter::ConvertJpegToProgressive)
        || image_optimization_enabled(options)
        || enabled(options, Filter::ResizeImages)
        || enabled(options, Filter::ResizeToRenderedImageDimensions)
        || enabled(options, Filter::InlineImages)
        || enabled(options, Filter::InsertImageDimensions)
        || enabled(options, Filter::JpegSubsampling)
        || enabled(options, Filter::StripImageColorProfile)
        || enabled(options, Filter::StripImageMetaData)
        || enabled(options, Filter::DelayImages))
        && (!image_preserve_urls(options) || check_bandwidth_option(options, "ippri"))
    {
        plan.pre.push(ChainFilter::ImageRewrite);
    }
    if enabled(options, Filter::RemoveComments) {
        plan.pre.push(ChainFilter::RemoveComments);
    }
    if enabled(options, Filter::ElideAttributes) {
        plan.pre.push(ChainFilter::ElideAttributes);
    }
    if enabled(options, Filter::ExtendCacheCss)
        || enabled(options, Filter::ExtendCacheImages)
        || enabled(options, Filter::ExtendCachePdfs)
        || enabled(options, Filter::ExtendCacheScripts)
    {
        plan.pre.push(ChainFilter::CacheExtender);
    }
    if enabled(options, Filter::SpriteImages) {
        plan.pre.push(ChainFilter::ImageCombine);
    }
    if enabled(options, Filter::LocalStorageCache) {
        plan.pre.push(ChainFilter::LocalStorageCache);
    }
    if needs_dependencies_cohort(options) {
        plan.pre.push(ChainFilter::CollectDependencies);
    }

    // Post-render, in application order.
    if enabled(options, Filter::FlushSubresources) && !opt_str(options, "pcu").is_empty() {
        plan.post.push(ChainFilter::RewrittenContentScanning);
    }
    if enabled(options, Filter::InsertDnsPrefetch) {
        plan.post.push(ChainFilter::InsertDnsPrefetch);
    }
    if enabled(options, Filter::PrioritizeLcpImages) {
        plan.post.push(ChainFilter::PrioritizeLcpImages);
    }
    if enabled(options, Filter::InsertAmpLink) {
        plan.post.push(ChainFilter::InsertAmpLink);
    }
    if enabled(options, Filter::AddInstrumentation) {
        plan.post.push(ChainFilter::AddInstrumentation);
    }
    if enabled(options, Filter::DeferJavascript) {
        plan.post.push(ChainFilter::DeferIframe);
        plan.post.push(ChainFilter::JsDisable);
        plan.post.push(ChainFilter::JsDeferDisabled);
    }
    if enabled(options, Filter::FixReflows) {
        plan.post.push(ChainFilter::FixReflow);
    }
    if enabled(options, Filter::DeterministicJs) {
        plan.post.push(ChainFilter::DeterministicJs);
    }
    if enabled(options, Filter::ConvertMetaTags) {
        plan.post.push(ChainFilter::MetaTag);
    }
    if enabled(options, Filter::DisableJavascript) {
        plan.post.push(ChainFilter::DeferIframe);
        plan.post.push(ChainFilter::JsDisable);
    }
    if enabled(options, Filter::DelayImages) {
        plan.post.push(ChainFilter::DelayImages);
    }
    if enabled(options, Filter::DedupInlinedImages) {
        plan.post.push(ChainFilter::DedupInlinedImages);
    }
    if enabled(options, Filter::LazyloadImages) {
        plan.post.push(ChainFilter::LazyloadImages);
    }
    if opt_bool(options, "snse") {
        plan.post.push(ChainFilter::SupportNoscript);
    }
    if enabled(options, Filter::HandleNoscriptRedirect) {
        plan.post.push(ChainFilter::HandleNoscriptRedirect);
    }
    if opt_int(options, "hpb") > 0 {
        plan.post.push(ChainFilter::RedirectOnSizeLimit);
        plan.size_limit = Some(opt_int(options, "hpb"));
    }
    if enabled(options, Filter::Pedantic) {
        plan.post.push(ChainFilter::Pedantic);
    }
    if can_rewrite_domains(options) && enabled(options, Filter::RewriteDomains) {
        plan.post.push(ChainFilter::DomainRewrite);
    }
    if enabled(options, Filter::LeftTrimUrls) {
        plan.post.push(ChainFilter::UrlLeftTrim);
    }
    if enabled(options, Filter::RemoveQuotes) {
        plan.post.push(ChainFilter::RemoveQuotes);
    }
    if enabled(options, Filter::CollapseWhitespace) {
        plan.post.push(ChainFilter::CollapseWhitespace);
    }
    // Oddly appended to the PRE-render list in C++.
    if enabled(options, Filter::HintPreloadSubresources) {
        plan.pre.push(ChainFilter::PushPreload);
    }
    if enabled(options, Filter::Debug) {
        plan.post.push(ChainFilter::Debug);
    }
    if enabled(options, Filter::HtmlWriterFilter) {
        plan.post.push(ChainFilter::HtmlWriter);
    }

    plan
}
