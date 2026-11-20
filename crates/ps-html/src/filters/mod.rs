//! Built-in HTML filters.
//!
//! Each module ports one C++ filter class; [`instantiate`] maps implemented
//! [`ChainFilter`](crate::chain::ChainFilter) slots to constructors so the
//! driver can build live chains from a [`ChainPlan`](crate::chain::ChainPlan).
//! Slots without an implementation return `None` until their filter lands.

pub mod add_head;
pub mod add_ids;
pub mod add_instrumentation;
pub mod amp_document;
pub mod base_tag;
pub mod cache_extender;
pub mod collapse_whitespace;
pub mod common;
pub mod critical_selector;
pub mod css;
pub mod css_combine;
pub mod css_hierarchy;
pub mod css_image_rewriter;
pub mod css_inline;
pub mod css_inline_import_to_link;
pub mod css_minify;
pub mod debug;
pub mod decode_rewritten_urls;
pub mod dedup_inlined_images;
pub mod defer_iframe;
pub mod delay_images;
pub mod deterministic_js;
pub mod dom_stats;
pub mod domain_rewrite;
pub mod elide_attributes;
pub mod fix_reflow;
pub mod flush_html;
pub mod google_analytics;
pub mod google_font_css_inline;
pub mod handle_noscript_redirect;
pub mod image_combine;
pub mod image_rewrite;
pub mod insert_amp_link;
pub mod insert_dns_prefetch;
pub mod insert_ga;
pub mod javascript;
pub mod js_combine;
pub mod js_defer_disabled;
pub mod js_disable;
pub mod js_inline;
pub mod js_minify;
pub mod js_outline;
pub mod lazyload_images;
pub mod local_storage_cache;
pub mod make_show_ads_async;
pub mod meta_tag;
pub mod move_css_to_head;
pub mod pedantic;
pub mod quote_removal;
pub mod redirect_on_size_limit;
pub mod remove_comments;
pub mod rewritten_content_scanning;
pub mod strip_legacy_polyfills;
pub mod strip_scripts;
pub mod strip_subresource_hints;
pub mod support_noscript;
pub mod url_left_trim;

pub use add_head::AddHeadFilter;
pub use add_ids::AddIdsFilter;
pub use add_instrumentation::{AddInstrumentationConfig, AddInstrumentationFilter};
pub use amp_document::{AmpDocumentFilter, INVALID_AMP_DIRECTIVE_COMMENT, UTF8_LIGHTNING_BOLT};
pub use base_tag::BaseTagFilter;
pub use cache_extender::{CacheExtenderFilter, CACHE_EXTENSIONS, FILTER_ID, NOT_CACHEABLE};
pub use collapse_whitespace::CollapseWhitespaceFilter;
pub use common::{BodyEndTracker, XhtmlStatus};
pub use css_inline::{CssInlineFilter, FILTER_ID as CSS_INLINE_ID, NUM_CSS_INLINED};
pub use css_inline_import_to_link::{CssInlineImportToLinkConfig, CssInlineImportToLinkFilter};
pub use css_minify::CssMinifyFilter;
pub use debug::{DebugConfig, DebugFilter};
pub use decode_rewritten_urls::{DecodeRewrittenUrlsConfig, DecodeRewrittenUrlsFilter};
pub use dedup_inlined_images::{
    DedupInlinedImagesConfig, DedupInlinedImagesFilter, CANDIDATES_FOUND, CANDIDATES_REPLACED,
    DII_INITIALIZER, MINIMUM_IMAGE_CUTOFF,
};
pub use defer_iframe::{DeferIframeFilter, DEFER_IFRAME_IFRAME_JS, DEFER_IFRAME_INIT};
pub use delay_images::{
    DelayImagesConfig, DelayImagesFilter, DelayImagesHtmlStatus, DELAY_IMAGES_INLINE_SUFFIX,
    DELAY_IMAGES_SUFFIX, IMAGE_ONLOAD_JS_SNIPPET,
};
pub use deterministic_js::DeterministicJsFilter;
pub use dom_stats::DomStatsFilter;
pub use domain_rewrite::{DomainRewriteConfig, DomainRewriteFilter, RewriteResult};
pub use elide_attributes::ElideAttributesFilter;
pub use fix_reflow::{FixReflowFilter, ELEMENT_RENDERED_HEIGHT_PROPERTY};
pub use flush_html::FlushHtmlFilter;
pub use google_analytics::GoogleAnalyticsFilter;
pub use google_font_css_inline::{
    GoogleFontCssInlineFilter, FILTER_ID as GOOGLE_FONT_CSS_INLINE_ID,
};
pub use handle_noscript_redirect::HandleNoscriptRedirectFilter;
pub use image_rewrite::{ImageRewriteFilter, FILTER_ID as IMAGE_REWRITE_ID};
pub use insert_amp_link::InsertAmpLinkFilter;
pub use insert_dns_prefetch::{
    DnsPrefetchHtmlStatus, InsertDnsPrefetchConfig, InsertDnsPrefetchFilter, SharedFlushEarlyInfo,
};
pub use insert_ga::{InsertGAConfig, InsertGAFilter};
pub use javascript::{JavascriptFilter, FILTER_ID as JS_MIN_ID, MINIFY_USES, SOURCE_MAP_ID};
pub use js_defer_disabled::{JsDeferDisabledConfig, JsDeferDisabledFilter};
pub use js_disable::{JsDisableConfig, JsDisableFilter, ELEMENT_ONLOAD_CODE};
pub use js_inline::{
    escape_text_element_tags, JsInlineFilter, FILTER_ID as JS_INLINE_ID, NUM_JS_INLINED,
};
pub use js_minify::JsMinifyFilter;
pub use lazyload_images::{
    FinderAvailability, LazyloadHtmlStatus, LazyloadImagesConfig, LazyloadImagesFilter,
    LazyloadLogRecord, LazyloadStatus, BEACON_IMAGE_ONLOAD_CODE, IMAGE_ONLOAD_CODE,
    LOAD_ALL_IMAGES, OVERRIDE_ATTRIBUTE_FUNCTIONS,
};
pub use make_show_ads_async::MakeShowAdsAsyncFilter;
pub use meta_tag::{MetaTagFilter, SharedHeaders};
pub use move_css_to_head::MoveCssToHeadFilter;
pub use pedantic::PedanticFilter;
pub use quote_removal::QuoteRemovalFilter;
pub use redirect_on_size_limit::RedirectOnSizeLimitFilter;
pub use remove_comments::RemoveCommentsFilter;
pub use rewritten_content_scanning::{
    RewrittenContentScanningFilter, NUM_PROXIED_REWRITTEN_RESOURCES_PROPERTY,
};
pub use strip_legacy_polyfills::StripLegacyPolyfillsFilter;
pub use strip_scripts::StripScriptsFilter;
pub use strip_subresource_hints::{StripSubresourceHintsConfig, StripSubresourceHintsFilter};
pub use support_noscript::{should_insert_noscript, SupportNoscriptFilter};
pub use url_left_trim::UrlLeftTrimFilter;

use crate::chain::ChainFilter;
use crate::filter::HtmlFilter;

/// Builds the live filter for an implemented chain slot (`None` while the
/// filter is still unported, or when it needs driver context — `BaseTag`
/// takes the document base URL, `AddHead` the combine flag,
/// `HandleNoscriptRedirect` the document URL, `AmpDocument` the discovery
/// callback — which the driver supplies at construction).
#[must_use]
pub fn instantiate(slot: ChainFilter) -> Option<Box<dyn HtmlFilter>> {
    match slot {
        ChainFilter::CollapseWhitespace => Some(Box::new(CollapseWhitespaceFilter::default())),
        // Minify subsets stand in for the driver-bound filters
        // (external resources need fetch machinery).
        ChainFilter::CssFilter => Some(Box::new(CssMinifyFilter::default())),
        ChainFilter::Javascript => Some(Box::new(JsMinifyFilter::default())),
        ChainFilter::ElideAttributes => Some(Box::new(ElideAttributesFilter)),
        ChainFilter::RemoveQuotes => Some(Box::new(QuoteRemovalFilter::default())),
        ChainFilter::RemoveComments => Some(Box::new(RemoveCommentsFilter::default())),
        ChainFilter::StripScripts => Some(Box::new(StripScriptsFilter)),
        ChainFilter::Pedantic => Some(Box::new(PedanticFilter)),
        ChainFilter::AddIds => Some(Box::new(AddIdsFilter::default())),
        _ => None,
    }
}
