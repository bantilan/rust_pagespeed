//! Chain-builder tests: level order goldens plus one test per
//! conditional/ordering dependency (each forced on and off).
use ps_config::filter::{Filter, RewriteLevel};
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::chain::{build_chain, ChainContext, ChainFilter, ChainPlan, Phase};

fn prod_ctx() -> ChainContext {
    ChainContext::new(true, true)
}

fn no_beacon_ctx() -> ChainContext {
    ChainContext::new(false, false)
}

fn options_with_level(level: RewriteLevel) -> RewriteOptions {
    let mut options = RewriteOptions::new();
    options.filters_mut().set_level(level);
    options
}

fn enable(options: &mut RewriteOptions, filters: &[Filter]) {
    for filter in filters {
        options.filters_mut().enable(*filter);
    }
}

fn forbid(options: &mut RewriteOptions, filter: Filter) {
    options.filters_mut().forbidden_set_mut().insert(filter);
}

fn set_bool(options: &mut RewriteOptions, name: &str, value: bool) {
    options
        .set_option(
            name,
            if value { "true" } else { "false" },
            Scope::ProcessStrict,
        )
        .unwrap();
}

fn names(plan: &ChainPlan, phase: Phase) -> Vec<&'static str> {
    plan.phase_names(phase)
}

fn has(plan: &ChainPlan, phase: Phase, filter: ChainFilter) -> bool {
    let list = match phase {
        Phase::Listener => &plan.listeners,
        Phase::EarlyPreRender => &plan.early_pre,
        Phase::PreRender => &plan.pre,
        Phase::PostRender => &plan.post,
    };
    list.contains(&filter)
}

fn index(plan: &ChainPlan, phase: Phase, filter: ChainFilter) -> usize {
    let list = match phase {
        Phase::Listener => &plan.listeners,
        Phase::EarlyPreRender => &plan.early_pre,
        Phase::PreRender => &plan.pre,
        Phase::PostRender => &plan.post,
    };
    list.iter().position(|f| *f == filter).unwrap()
}

#[test]
fn inline_import_forced_by_critical_css() {
    // Forced by PrioritizeCriticalCss unless forbidden.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::PrioritizeCriticalCss]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssInlineImportToLink
    ));
    forbid(&mut options, Filter::InlineImportToLink);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssInlineImportToLink
    ));
    // Forced by ComputeCriticalCss too.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::ComputeCriticalCss]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssInlineImportToLink
    ));
    // Off by default.
    let options = RewriteOptions::new();
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssInlineImportToLink
    ));
}

#[test]
fn css_filter_forced_by_flatten() {
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::FlattenCssImports]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssFilter
    ));
    forbid(&mut options, Filter::RewriteCss);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssFilter
    ));
    // PrioritizeCriticalCss forces flattening, which forces cf.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::PrioritizeCriticalCss]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssFilter
    ));
}

#[test]
fn critical_css_beacon_arms() {
    // Prioritize arm needs beacon results.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::PrioritizeCriticalCss]);
    assert!(!has(
        &build_chain(&options, &no_beacon_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalCssBeacon
    ));
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalCssBeacon
    ));
    // Compute arm needs no beacon.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::ComputeCriticalCss]);
    assert!(has(
        &build_chain(&options, &no_beacon_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalCssBeacon
    ));
}

#[test]
fn preserve_urls_skip_resource_filters() {
    // cf skipped under css-preserve unless preemptive rewrite is on.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::RewriteCss]);
    set_bool(&mut options, "CssPreserveURLs", true);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssFilter
    ));
    set_bool(&mut options, "InPlacePreemptiveRewriteCss", false);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssFilter
    ));
    // jm skipped under js-preserve unless preemptive rewrite is on.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::RewriteJavascriptExternal]);
    set_bool(&mut options, "JsPreserveURLs", true);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::Javascript
    ));
    set_bool(&mut options, "InPlacePreemptiveRewriteJavascript", false);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::Javascript
    ));
    // Inline JS is always allowed through for jm.
    enable(&mut options, &[Filter::RewriteJavascriptInline]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::Javascript
    ));
    // ic skipped under image-preserve unless preemptive rewrite is on.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::RecompressJpeg]);
    set_bool(&mut options, "ImagePreserveURLs", true);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::ImageRewrite
    ));
    set_bool(&mut options, "InPlacePreemptiveRewriteImages", false);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::ImageRewrite
    ));
}

#[test]
fn outline_css_skipped_by_prioritize() {
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::OutlineCss]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssOutline
    ));
    enable(&mut options, &[Filter::PrioritizeCriticalCss]);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CssOutline
    ));
}

#[test]
fn responsive_images_needs_both() {
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::ResponsiveImages]);
    let plan = build_chain(&options, &prod_ctx());
    assert!(!has(
        &plan,
        Phase::EarlyPreRender,
        ChainFilter::ResponsiveImageFirst
    ));
    assert!(!has(
        &plan,
        Phase::PostRender,
        ChainFilter::ResponsiveImageSecond
    ));
    enable(&mut options, &[Filter::ResizeImages]);
    let plan = build_chain(&options, &prod_ctx());
    assert!(has(
        &plan,
        Phase::EarlyPreRender,
        ChainFilter::ResponsiveImageFirst
    ));
    // Second phase registers first in the post list.
    assert_eq!(plan.post[0], ChainFilter::ResponsiveImageSecond);
}

#[test]
fn insert_ga_needs_id() {
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::InsertGA]);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::InsertGA
    ));
    options
        .set_option("AnalyticsID", "UA-1", Scope::ProcessStrict)
        .unwrap();
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::InsertGA
    ));
    // Experiments insert GA too (with an id set).
    let mut options = RewriteOptions::new();
    options
        .set_option("AnalyticsID", "UA-1", Scope::ProcessStrict)
        .unwrap();
    set_bool(&mut options, "RunExperiment", true);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::InsertGA
    ));
}

#[test]
fn rewritten_content_scanning_needs_preconnect() {
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::FlushSubresources]);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::RewrittenContentScanning
    ));
    options
        .set_request_property("pcu", "https://x.test/")
        .unwrap();
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::RewrittenContentScanning
    ));
}

#[test]
fn size_limit_wiring() {
    let options = RewriteOptions::new();
    let plan = build_chain(&options, &prod_ctx());
    assert!(!has(
        &plan,
        Phase::PostRender,
        ChainFilter::RedirectOnSizeLimit
    ));
    assert_eq!(plan.size_limit, None);
    let mut options = RewriteOptions::new();
    options
        .set_option("MaxHtmlParseBytes", "100", Scope::ProcessStrict)
        .unwrap();
    let plan = build_chain(&options, &prod_ctx());
    assert!(has(
        &plan,
        Phase::PostRender,
        ChainFilter::RedirectOnSizeLimit
    ));
    assert_eq!(plan.size_limit, Some(100));
}

#[test]
fn domain_rewrite_needs_mappings() {
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::RewriteDomains]);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::DomainRewrite
    ));
    options
        .parse_and_set_2("MapRewriteDomain", "a.test", "b.test")
        .unwrap();
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::DomainRewrite
    ));
    // Shards count too.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::RewriteDomains]);
    options
        .parse_and_set_2("ShardDomain", "a.test", "s.test")
        .unwrap();
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::DomainRewrite
    ));
}

#[test]
fn debug_and_writer_terminals() {
    // Writer terminal on by default; debug off.
    let options = RewriteOptions::new();
    let plan = build_chain(&options, &prod_ctx());
    assert_eq!(plan.post.last(), Some(&ChainFilter::HtmlWriter));
    assert!(!has(&plan, Phase::PostRender, ChainFilter::Debug));
    // Debug second-to-last when on.
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::Debug]);
    let plan = build_chain(&options, &prod_ctx());
    let tail = &plan.post[plan.post.len() - 2..];
    assert_eq!(tail, &[ChainFilter::Debug, ChainFilter::HtmlWriter]);
    // Writer off when disabled.
    let mut options = RewriteOptions::new();
    options
        .filters_mut()
        .disabled_set_mut()
        .insert(Filter::HtmlWriterFilter);
    let plan = build_chain(&options, &prod_ctx());
    assert!(!has(&plan, Phase::PostRender, ChainFilter::HtmlWriter));
}

#[test]
fn scalar_gates() {
    // StripSubresourceHints unless preserved.
    let options = RewriteOptions::new();
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::EarlyPreRender,
        ChainFilter::StripSubresourceHints
    ));
    let mut options = RewriteOptions::new();
    set_bool(&mut options, "PreserveSubresourceHints", true);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::EarlyPreRender,
        ChainFilter::StripSubresourceHints
    ));
    // SupportNoscript on by default.
    let options = RewriteOptions::new();
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::SupportNoscript
    ));
    let mut options = RewriteOptions::new();
    set_bool(&mut options, "SupportNoScriptEnabled", false);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PostRender,
        ChainFilter::SupportNoscript
    ));
    // FlushHtml listener only when enabled.
    let options = RewriteOptions::new();
    assert_eq!(
        names(&build_chain(&options, &prod_ctx()), Phase::Listener),
        ["AmpDocumentFilter"]
    );
    let mut options = RewriteOptions::new();
    set_bool(&mut options, "FlushHtml", true);
    assert_eq!(
        names(&build_chain(&options, &prod_ctx()), Phase::Listener),
        ["FlushHtmlFilter", "AmpDocumentFilter"]
    );
}

#[test]
fn critical_images_beacon_gate() {
    // Needs an image trigger plus beacon enabled plus factory/pcache.
    let options = RewriteOptions::new();
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalImagesBeacon
    ));
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::LazyloadImages]);
    assert!(has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalImagesBeacon
    ));
    assert!(!has(
        &build_chain(&options, &no_beacon_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalImagesBeacon
    ));
    let mut options = RewriteOptions::new();
    enable(&mut options, &[Filter::LazyloadImages]);
    set_bool(&mut options, "CriticalImagesBeaconEnabled", false);
    assert!(!has(
        &build_chain(&options, &prod_ctx()),
        Phase::PreRender,
        ChainFilter::CriticalImagesBeacon
    ));
}

/// Each singly-gated filter appears in its phase when enabled alone.
#[test]
fn single_gates() {
    let cases: &[(Filter, Phase, ChainFilter)] = &[
        (
            Filter::ComputeStatistics,
            Phase::EarlyPreRender,
            ChainFilter::DomStats,
        ),
        (
            Filter::DecodeRewrittenUrls,
            Phase::EarlyPreRender,
            ChainFilter::DecodeRewrittenUrls,
        ),
        (
            Filter::AddBaseTag,
            Phase::EarlyPreRender,
            ChainFilter::BaseTag,
        ),
        (Filter::AddIds, Phase::EarlyPreRender, ChainFilter::AddIds),
        (
            Filter::StripScripts,
            Phase::PreRender,
            ChainFilter::StripScripts,
        ),
        (
            Filter::StripLegacyPolyfills,
            Phase::PreRender,
            ChainFilter::StripLegacyPolyfills,
        ),
        (
            Filter::MakeShowAdsAsync,
            Phase::PreRender,
            ChainFilter::MakeShowAdsAsync,
        ),
        (
            Filter::InlineImportToLink,
            Phase::PreRender,
            ChainFilter::CssInlineImportToLink,
        ),
        (
            Filter::OutlineCss,
            Phase::PreRender,
            ChainFilter::CssOutline,
        ),
        (
            Filter::InlineGoogleFontCss,
            Phase::PreRender,
            ChainFilter::GoogleFontCssInline,
        ),
        (
            Filter::MoveCssToHead,
            Phase::PreRender,
            ChainFilter::CssMoveToHead,
        ),
        (
            Filter::MoveCssAboveScripts,
            Phase::PreRender,
            ChainFilter::CssMoveToHead,
        ),
        (
            Filter::CombineCss,
            Phase::PreRender,
            ChainFilter::CssCombine,
        ),
        (Filter::RewriteCss, Phase::PreRender, ChainFilter::CssFilter),
        (
            Filter::PrioritizeCriticalCss,
            Phase::PreRender,
            ChainFilter::CriticalSelector,
        ),
        (Filter::InlineCss, Phase::PreRender, ChainFilter::CssInline),
        (
            Filter::OutlineJavascript,
            Phase::PreRender,
            ChainFilter::JsOutline,
        ),
        (
            Filter::MakeGoogleAnalyticsAsync,
            Phase::PreRender,
            ChainFilter::GoogleAnalytics,
        ),
        (
            Filter::CombineJavascript,
            Phase::PreRender,
            ChainFilter::JsCombine,
        ),
        (
            Filter::RewriteJavascriptExternal,
            Phase::PreRender,
            ChainFilter::Javascript,
        ),
        (
            Filter::RewriteJavascriptInline,
            Phase::PreRender,
            ChainFilter::Javascript,
        ),
        (
            Filter::CanonicalizeJavascriptLibraries,
            Phase::PreRender,
            ChainFilter::Javascript,
        ),
        (
            Filter::InlineJavascript,
            Phase::PreRender,
            ChainFilter::JsInline,
        ),
        (
            Filter::RecompressJpeg,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::ResizeImages,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::ResizeToRenderedImageDimensions,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::InlineImages,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::InsertImageDimensions,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::JpegSubsampling,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::StripImageColorProfile,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::StripImageMetaData,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::DelayImages,
            Phase::PreRender,
            ChainFilter::ImageRewrite,
        ),
        (
            Filter::RemoveComments,
            Phase::PreRender,
            ChainFilter::RemoveComments,
        ),
        (
            Filter::ElideAttributes,
            Phase::PreRender,
            ChainFilter::ElideAttributes,
        ),
        (
            Filter::ExtendCacheCss,
            Phase::PreRender,
            ChainFilter::CacheExtender,
        ),
        (
            Filter::ExtendCacheImages,
            Phase::PreRender,
            ChainFilter::CacheExtender,
        ),
        (
            Filter::ExtendCachePdfs,
            Phase::PreRender,
            ChainFilter::CacheExtender,
        ),
        (
            Filter::ExtendCacheScripts,
            Phase::PreRender,
            ChainFilter::CacheExtender,
        ),
        (
            Filter::SpriteImages,
            Phase::PreRender,
            ChainFilter::ImageCombine,
        ),
        (
            Filter::LocalStorageCache,
            Phase::PreRender,
            ChainFilter::LocalStorageCache,
        ),
        (
            Filter::ExperimentHttp2,
            Phase::PreRender,
            ChainFilter::CollectDependencies,
        ),
        (
            Filter::HintPreloadSubresources,
            Phase::PreRender,
            ChainFilter::PushPreload,
        ),
        (
            Filter::InsertDnsPrefetch,
            Phase::PostRender,
            ChainFilter::InsertDnsPrefetch,
        ),
        (
            Filter::PrioritizeLcpImages,
            Phase::PostRender,
            ChainFilter::PrioritizeLcpImages,
        ),
        (
            Filter::InsertAmpLink,
            Phase::PostRender,
            ChainFilter::InsertAmpLink,
        ),
        (
            Filter::AddInstrumentation,
            Phase::PostRender,
            ChainFilter::AddInstrumentation,
        ),
        (
            Filter::DeferJavascript,
            Phase::PostRender,
            ChainFilter::JsDeferDisabled,
        ),
        (
            Filter::FixReflows,
            Phase::PostRender,
            ChainFilter::FixReflow,
        ),
        (
            Filter::DeterministicJs,
            Phase::PostRender,
            ChainFilter::DeterministicJs,
        ),
        (
            Filter::ConvertMetaTags,
            Phase::PostRender,
            ChainFilter::MetaTag,
        ),
        (
            Filter::DisableJavascript,
            Phase::PostRender,
            ChainFilter::JsDisable,
        ),
        (
            Filter::DelayImages,
            Phase::PostRender,
            ChainFilter::DelayImages,
        ),
        (
            Filter::DedupInlinedImages,
            Phase::PostRender,
            ChainFilter::DedupInlinedImages,
        ),
        (
            Filter::LazyloadImages,
            Phase::PostRender,
            ChainFilter::LazyloadImages,
        ),
        (
            Filter::HandleNoscriptRedirect,
            Phase::PostRender,
            ChainFilter::HandleNoscriptRedirect,
        ),
        (Filter::Pedantic, Phase::PostRender, ChainFilter::Pedantic),
        (
            Filter::LeftTrimUrls,
            Phase::PostRender,
            ChainFilter::UrlLeftTrim,
        ),
        (
            Filter::RemoveQuotes,
            Phase::PostRender,
            ChainFilter::RemoveQuotes,
        ),
        (
            Filter::CollapseWhitespace,
            Phase::PostRender,
            ChainFilter::CollapseWhitespace,
        ),
        (Filter::Debug, Phase::PostRender, ChainFilter::Debug),
    ];
    for (filter, phase, slot) in cases {
        let mut options = RewriteOptions::new();
        enable(&mut options, &[*filter]);
        assert!(
            has(&build_chain(&options, &prod_ctx()), *phase, *slot),
            "{filter:?} must add {}",
            slot.name()
        );
    }
}

/// Relative order within each phase, per the audited C++ sequence.
#[test]
fn ordering_rules() {
    // Early pre-render skeleton.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::ComputeStatistics,
            Filter::DecodeRewrittenUrls,
            Filter::ResponsiveImages,
            Filter::ResizeImages,
            Filter::MoveCssToHead, // triggers RequiresAddHead
            Filter::AddBaseTag,
            Filter::AddIds,
        ],
    );
    let plan = build_chain(&options, &prod_ctx());
    let early = |f| index(&plan, Phase::EarlyPreRender, f);
    assert!(
        early(ChainFilter::DomStats) < early(ChainFilter::StripSubresourceHints)
            && early(ChainFilter::StripSubresourceHints) < early(ChainFilter::DecodeRewrittenUrls)
            && early(ChainFilter::DecodeRewrittenUrls) < early(ChainFilter::ResponsiveImageFirst)
            && early(ChainFilter::ResponsiveImageFirst) < early(ChainFilter::AddHead)
            && early(ChainFilter::AddHead) < early(ChainFilter::BaseTag)
            && early(ChainFilter::BaseTag) < early(ChainFilter::AddIds)
    );

    // CSS pipeline order.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::InlineImportToLink,
            Filter::OutlineCss,
            Filter::InlineGoogleFontCss,
            Filter::MoveCssToHead,
            Filter::CombineCss,
            Filter::RewriteCss,
            Filter::InlineCss,
        ],
    );
    let plan = build_chain(&options, &prod_ctx());
    let pre = |f| index(&plan, Phase::PreRender, f);
    assert!(
        pre(ChainFilter::CssInlineImportToLink) < pre(ChainFilter::CssOutline)
            && pre(ChainFilter::CssOutline) < pre(ChainFilter::GoogleFontCssInline)
            && pre(ChainFilter::GoogleFontCssInline) < pre(ChainFilter::CssMoveToHead)
            && pre(ChainFilter::CssMoveToHead) < pre(ChainFilter::CssCombine)
            && pre(ChainFilter::CssCombine) < pre(ChainFilter::CssFilter)
            && pre(ChainFilter::CssFilter) < pre(ChainFilter::CssInline)
    );

    // JS pipeline order (analytics before rewrite, combine after minify).
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::OutlineJavascript,
            Filter::MakeGoogleAnalyticsAsync,
            Filter::InsertGA,
            Filter::CombineJavascript,
            Filter::RewriteJavascriptExternal,
            Filter::InlineJavascript,
        ],
    );
    options
        .set_option("AnalyticsID", "UA-1", Scope::ProcessStrict)
        .unwrap();
    let plan = build_chain(&options, &prod_ctx());
    let pre = |f| index(&plan, Phase::PreRender, f);
    assert!(
        pre(ChainFilter::JsOutline) < pre(ChainFilter::GoogleAnalytics)
            && pre(ChainFilter::GoogleAnalytics) < pre(ChainFilter::InsertGA)
            && pre(ChainFilter::InsertGA) < pre(ChainFilter::JsCombine)
            && pre(ChainFilter::JsCombine) < pre(ChainFilter::Javascript)
            && pre(ChainFilter::Javascript) < pre(ChainFilter::JsInline)
    );

    // Resource pipeline order.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::RecompressJpeg,
            Filter::RemoveComments,
            Filter::ElideAttributes,
            Filter::ExtendCacheCss,
            Filter::SpriteImages,
            Filter::LocalStorageCache,
            Filter::ExperimentHttp2,
        ],
    );
    let plan = build_chain(&options, &prod_ctx());
    let pre = |f| index(&plan, Phase::PreRender, f);
    assert!(
        pre(ChainFilter::ImageRewrite) < pre(ChainFilter::RemoveComments)
            && pre(ChainFilter::RemoveComments) < pre(ChainFilter::ElideAttributes)
            && pre(ChainFilter::ElideAttributes) < pre(ChainFilter::CacheExtender)
            && pre(ChainFilter::CacheExtender) < pre(ChainFilter::ImageCombine)
            && pre(ChainFilter::ImageCombine) < pre(ChainFilter::LocalStorageCache)
            && pre(ChainFilter::LocalStorageCache) < pre(ChainFilter::CollectDependencies)
    );

    // PushPreload closes the pre list, after CollectDependencies.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[Filter::HintPreloadSubresources, Filter::RewriteCss],
    );
    let plan = build_chain(&options, &prod_ctx());
    assert_eq!(
        plan.pre[plan.pre.len() - 2..],
        [ChainFilter::CollectDependencies, ChainFilter::PushPreload]
    );

    // Post-render head: scanning, discovery, instrumentation, defer.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::FlushSubresources,
            Filter::InsertDnsPrefetch,
            Filter::PrioritizeLcpImages,
            Filter::InsertAmpLink,
            Filter::AddInstrumentation,
            Filter::DeferJavascript,
            Filter::FixReflows,
            Filter::DeterministicJs,
            Filter::ConvertMetaTags,
        ],
    );
    options
        .set_request_property("pcu", "https://x.test/")
        .unwrap();
    let plan = build_chain(&options, &prod_ctx());
    let post = |f| index(&plan, Phase::PostRender, f);
    assert!(
        post(ChainFilter::RewrittenContentScanning) < post(ChainFilter::InsertDnsPrefetch)
            && post(ChainFilter::InsertDnsPrefetch) < post(ChainFilter::PrioritizeLcpImages)
            && post(ChainFilter::PrioritizeLcpImages) < post(ChainFilter::InsertAmpLink)
            && post(ChainFilter::InsertAmpLink) < post(ChainFilter::AddInstrumentation)
            && post(ChainFilter::AddInstrumentation) < post(ChainFilter::DeferIframe)
            && post(ChainFilter::DeferIframe) < post(ChainFilter::FixReflow)
            && post(ChainFilter::FixReflow) < post(ChainFilter::DeterministicJs)
            && post(ChainFilter::DeterministicJs) < post(ChainFilter::MetaTag)
    );
    // Defer triple adjacency.
    let at = post(ChainFilter::DeferIframe);
    assert_eq!(
        plan.post[at..at + 3],
        [
            ChainFilter::DeferIframe,
            ChainFilter::JsDisable,
            ChainFilter::JsDeferDisabled
        ]
    );

    // Post-render tail: images, noscript, size, pedantic, domains, trim.
    let mut options = RewriteOptions::new();
    enable(
        &mut options,
        &[
            Filter::DisableJavascript,
            Filter::DelayImages,
            Filter::DedupInlinedImages,
            Filter::LazyloadImages,
            Filter::HandleNoscriptRedirect,
            Filter::Pedantic,
            Filter::RewriteDomains,
            Filter::LeftTrimUrls,
            Filter::RemoveQuotes,
            Filter::CollapseWhitespace,
        ],
    );
    options
        .set_option("MaxHtmlParseBytes", "10", Scope::ProcessStrict)
        .unwrap();
    options
        .parse_and_set_2("MapRewriteDomain", "a.test", "b.test")
        .unwrap();
    let plan = build_chain(&options, &prod_ctx());
    let post = |f| index(&plan, Phase::PostRender, f);
    assert!(
        post(ChainFilter::JsDisable) < post(ChainFilter::DelayImages)
            && post(ChainFilter::DelayImages) < post(ChainFilter::DedupInlinedImages)
            && post(ChainFilter::DedupInlinedImages) < post(ChainFilter::LazyloadImages)
            && post(ChainFilter::LazyloadImages) < post(ChainFilter::SupportNoscript)
            && post(ChainFilter::SupportNoscript) < post(ChainFilter::HandleNoscriptRedirect)
            && post(ChainFilter::HandleNoscriptRedirect) < post(ChainFilter::RedirectOnSizeLimit)
            && post(ChainFilter::RedirectOnSizeLimit) < post(ChainFilter::Pedantic)
            && post(ChainFilter::Pedantic) < post(ChainFilter::DomainRewrite)
            && post(ChainFilter::DomainRewrite) < post(ChainFilter::UrlLeftTrim)
            && post(ChainFilter::UrlLeftTrim) < post(ChainFilter::RemoveQuotes)
            && post(ChainFilter::RemoveQuotes) < post(ChainFilter::CollapseWhitespace)
            && post(ChainFilter::CollapseWhitespace) < post(ChainFilter::HtmlWriter)
    );
}

fn assert_level(
    level: RewriteLevel,
    listeners: &[&str],
    early: &[&str],
    pre: &[&str],
    post: &[&str],
) {
    let options = options_with_level(level);
    let plan = build_chain(&options, &prod_ctx());
    assert_eq!(names(&plan, Phase::Listener), listeners, "listeners");
    assert_eq!(names(&plan, Phase::EarlyPreRender), early, "early-pre");
    assert_eq!(names(&plan, Phase::PreRender), pre, "pre");
    assert_eq!(names(&plan, Phase::PostRender), post, "post");
    assert_eq!(plan.size_limit, None);
}

#[test]
fn level_golden_passthrough() {
    assert_level(
        RewriteLevel::PassThrough,
        &["AmpDocumentFilter"],
        &["StripSubresourceHintsFilter"],
        &[],
        &["SupportNoscriptFilter", "HtmlWriterFilter"],
    );
}

#[test]
fn level_golden_core() {
    assert_level(
        RewriteLevel::CoreFilters,
        &["AmpDocumentFilter"],
        &["StripSubresourceHintsFilter", "AddHeadFilter"],
        &[
            "CriticalImagesBeaconFilter",
            "CssInlineImportToLinkFilter",
            "CssCombineFilter",
            "CssFilter",
            "CssInlineFilter",
            "JsCombineFilter",
            "JavascriptFilter",
            "JsInlineFilter",
            "ImageRewriteFilter",
            "CacheExtenderFilter",
        ],
        &[
            "PreloadCriticalFontsFilter",
            "MetaTagFilter",
            "SupportNoscriptFilter",
            "HtmlWriterFilter",
        ],
    );
}

#[test]
fn level_golden_bandwidth() {
    assert_level(
        RewriteLevel::OptimizeForBandwidth,
        &["AmpDocumentFilter"],
        &["StripSubresourceHintsFilter"],
        &["CssFilter", "JavascriptFilter", "ImageRewriteFilter"],
        &["SupportNoscriptFilter", "HtmlWriterFilter"],
    );
}

#[test]
fn level_golden_testing() {
    assert_level(
        RewriteLevel::TestingCoreFilters,
        &["AmpDocumentFilter"],
        &[
            "StripSubresourceHintsFilter",
            "ResponsiveImageFirstFilter",
            "AddHeadFilter",
        ],
        &[
            "CriticalImagesBeaconFilter",
            "CssInlineImportToLinkFilter",
            "CssCombineFilter",
            "CssFilter",
            "CriticalCssBeaconFilter",
            "CriticalSelectorFilter",
            "CssInlineFilter",
            "GoogleAnalyticsFilter",
            "JsCombineFilter",
            "JavascriptFilter",
            "JsInlineFilter",
            "ImageRewriteFilter",
            "CacheExtenderFilter",
            "ImageCombineFilter",
        ],
        &[
            "ResponsiveImageSecondFilter",
            "PreloadCriticalFontsFilter",
            "InsertAmpLinkFilter",
            "DeferIframeFilter",
            "JsDisableFilter",
            "JsDeferDisabledFilter",
            "MetaTagFilter",
            "DelayImagesFilter",
            "LazyloadImagesFilter",
            "SupportNoscriptFilter",
            "UrlLeftTrimFilter",
            "DebugFilter",
            "HtmlWriterFilter",
        ],
    );
}

#[test]
fn level_golden_all() {
    assert_level(
        RewriteLevel::AllFilters,
        &["AmpDocumentFilter"],
        &[
            "DomStatsFilter",
            "StripSubresourceHintsFilter",
            "DecodeRewrittenUrlsFilter",
            "ResponsiveImageFirstFilter",
            "AddHeadFilter",
            "BaseTagFilter",
            "AddIdsFilter",
        ],
        &[
            "CriticalImagesBeaconFilter",
            "MakeShowAdsAsyncFilter",
            "CssInlineImportToLinkFilter",
            "GoogleFontCssInlineFilter",
            "CssMoveToHeadFilter",
            "CssCombineFilter",
            "CssFilter",
            "CriticalCssBeaconFilter",
            "CriticalSelectorFilter",
            "CssInlineFilter",
            "JsOutlineFilter",
            "GoogleAnalyticsFilter",
            "JsCombineFilter",
            "JavascriptFilter",
            "JsInlineFilter",
            "ImageRewriteFilter",
            "RemoveCommentsFilter",
            "ElideAttributesFilter",
            "CacheExtenderFilter",
            "ImageCombineFilter",
            "LocalStorageCacheFilter",
            "CollectDependenciesFilter",
            "PushPreloadFilter",
        ],
        &[
            "ResponsiveImageSecondFilter",
            "InsertDnsPrefetchFilter",
            "PrioritizeLcpImagesFilter",
            "PreloadCriticalFontsFilter",
            "InsertAmpLinkFilter",
            "AddInstrumentationFilter",
            "DeferIframeFilter",
            "JsDisableFilter",
            "JsDeferDisabledFilter",
            "MetaTagFilter",
            "DelayImagesFilter",
            "DedupInlinedImagesFilter",
            "LazyloadImagesFilter",
            "SupportNoscriptFilter",
            "HandleNoscriptRedirectFilter",
            "PedanticFilter",
            "UrlLeftTrimFilter",
            "HtmlAttributeQuoteRemoval",
            "CollapseWhitespaceFilter",
            "DebugFilter",
            "HtmlWriterFilter",
        ],
    );
}
