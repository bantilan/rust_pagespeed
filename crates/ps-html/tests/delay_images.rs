//! Delay tests ported from `delay_images_filter_test.cc`.
//!
//! NOTE: the C++ inputs name image files and rely on the image
//! pipeline to generate low-res previews (tagging
//! `data-pagespeed-low-res-src`) before this filter runs. That
//! pipeline is not ported, so inputs here carry the tag directly —
//! the delay filter only ever reads those — with canned low-res
//! sentinels standing in for encoder bytes. The `?PageSpeed=noscript`
//! wrappers come from a sibling filter and are dropped. Flush-window,
//! noscript-tagging, preview-sizing, and resize tests are skipped:
//! they exercise the pipeline, not this filter.
#[path = "harness.rs"]
mod harness;

use harness::{SharedFilter, SharedSink};
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_config::RewriteLevel;
use ps_html::filters::delay_images::{
    DelayImagesConfig, DelayImagesFilter, DelayImagesHtmlStatus, DELAY_IMAGES_INLINE_SUFFIX,
    DELAY_IMAGES_SUFFIX, IMAGE_ONLOAD_JS_SNIPPET,
};
use ps_html::filters::lazyload_images::{FinderAvailability, BEACON_IMAGE_ONLOAD_CODE};
use ps_html::parser::Parser;
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::writer::WriterFilter;

use ps_html::filters::delay_images::IMAGE_ONLOAD_CODE as DELAY_ONLOAD;

/// Canned low-res bytes standing in for encoder output.
const LOWRES_JPEG: &str = "data:image/jpeg;base64,LOWRESJPEG";
const LOWRES_WEBP: &str = "data:image/webp;base64,LOWRESWEBP";

fn onload_js() -> &'static str {
    std::str::from_utf8(DELAY_ONLOAD).unwrap()
}

fn config() -> DelayImagesConfig {
    DelayImagesConfig::default()
}

/// Mobile aggressive fixture (`SetupUserAgentTest` + aggressive on).
fn mobile_config() -> DelayImagesConfig {
    let mut config = DelayImagesConfig::default();
    config.is_mobile = true;
    config
        .options
        .set_option(
            "EnableAggressiveRewritersForMobile",
            "on",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    config
}

fn run_case(
    case: &str,
    html: &str,
    config: DelayImagesConfig,
) -> (String, SharedFilter<DelayImagesFilter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(DelayImagesFilter::new(config));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(html.as_bytes());
    parser.finish();
    (String::from_utf8(sink.bytes()).unwrap(), filter)
}

/// Expected swap-helper block (`GetImageOnloadScriptBlock`).
fn onload_block() -> String {
    format!(
        "<script data-pagespeed-no-defer type=\"text/javascript\">{}</script>",
        std::str::from_utf8(IMAGE_ONLOAD_JS_SNIPPET).unwrap()
    )
}

/// Expected inplace rewrite (`GenerateRewrittenImageTag`).
fn rewritten_img(url: &str, lowres: &str) -> String {
    format!(
        "<img data-pagespeed-high-res-src=\"{url}\" src=\"{lowres}\" \
        onload=\"{code}\" onerror=\"this.onerror=null;{code}\"/>",
        code = onload_js(),
    )
}

/// Expected delay libraries (`GetInlineScript`).
fn inline_script(debug: bool) -> String {
    let mut html = String::from("<script data-pagespeed-no-defer type=\"text/javascript\">");
    html.push_str(std::str::from_utf8(get_asset(StaticAsset::DelayImagesInline, debug)).unwrap());
    html.push_str(std::str::from_utf8(DELAY_IMAGES_INLINE_SUFFIX).unwrap());
    html.push_str(std::str::from_utf8(get_asset(StaticAsset::DelayImages, debug)).unwrap());
    html.push_str(std::str::from_utf8(DELAY_IMAGES_SUFFIX).unwrap());
    html.push_str("</script>");
    html
}

/// Expected low-res data script (`GenerateAddLowResScript`).
fn add_lowres_script(url: &str, data: &str) -> String {
    format!(
        "<script data-pagespeed-no-defer type=\"text/javascript\">\
        \nif(window.pagespeed){{if(pagespeed.delayImagesInline){{\
        pagespeed.delayImagesInline.addLowResImages('{url}', '{data}');\
        \npagespeed.delayImagesInline.replaceWithLowRes();}}}}\n</script>"
    )
}

/// Expected high-res trigger (`GetHighResScript` / `GetLazyHighResScript`).
fn highres_script(lazy: bool) -> String {
    let call = if lazy {
        "registerLazyLoadHighRes"
    } else {
        "replaceWithHighRes"
    };
    format!(
        "<script data-pagespeed-no-defer type=\"text/javascript\">\
        \nif(window.pagespeed){{if(pagespeed.delayImages){{\
        pagespeed.delayImages.{call}();}}}}\n</script>"
    )
}

/// Tags an image with canned low-res bytes (the pipeline's output).
fn tagged(src_attr: &str, lowres: &str) -> String {
    format!("<img {src_attr} data-pagespeed-low-res-src=\"{lowres}\"/>")
}

#[test]
fn basic() {
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let expected = format!(
        "<head></head><body>{}{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/1.webp", LOWRES_WEBP),
        ""
    );
    let (output, filter) = run_case("inline_preview_images", &input, config());
    assert_eq!(output, expected);
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DelayImagesHtmlStatus::Active), 1)
    );
}

#[test]
fn preserve_urls_on() {
    // Preserve-URLs vetoes the filter at the options level, so the
    // chain omits it and the parse runs bare.
    let mut options = RewriteOptions::new();
    options
        .set_option("ImagePreserveURLs", "on", ps_config::Scope::ProcessStrict)
        .unwrap();
    options
        .filters_mut()
        .set_level(RewriteLevel::TestingCoreFilters);
    assert!(!options.filters().enabled(Filter::DelayImages));

    let input = "<html><head></head><body><img src=\"http://test.com/1.jpeg\"/></body></html>";
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/inline_preview_images.html", false);
    parser.write(input.as_bytes());
    parser.finish();
    assert_eq!(String::from_utf8(sink.bytes()).unwrap(), input);
}

#[test]
fn unsupported_user_agent() {
    let mut fixture = config();
    fixture.supports_image_inlining = false;
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let (output, filter) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, input);
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DelayImagesHtmlStatus::UserAgentNotSupported), 0)
    );
}

#[test]
fn query_param() {
    // Escaped entities survive the in-place rename byte-identically.
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.webp?a=b&amp;c=d\"", LOWRES_WEBP)
    );
    let expected = format!(
        "<head></head><body>{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/1.webp?a=b&amp;c=d", LOWRES_WEBP),
    );
    let (output, _) = run_case("inline_preview_images", &input, config());
    assert_eq!(output, expected);
}

#[test]
fn unescaped_query_param() {
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.webp?a=b&c=d\"", LOWRES_WEBP)
    );
    let expected = format!(
        "<head></head><body>{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/1.webp?a=b&c=d", LOWRES_WEBP),
    );
    let (output, _) = run_case("inline_preview_images", &input, config());
    assert_eq!(output, expected);
}

#[test]
fn only_url_valued_attribute() {
    // No `src` means no image category: the tag stays put, low-res
    // tag included (the early return skips the tail delete, like C++).
    let mut fixture = config();
    fixture.url_valued.push((
        "img".to_string(),
        "data-src".to_string(),
        ps_config::options::Category::Image,
    ));
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("data-src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let (output, filter) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, input);
    assert_eq!(filter.with(|filter| filter.log_status().1), 0);
}

#[test]
fn src_and_url_valued_attribute() {
    let mut fixture = config();
    fixture.url_valued.push((
        "img".to_string(),
        "data-src".to_string(),
        ps_config::options::Category::Image,
    ));
    let input = format!(
        "<head></head><body>{}</body>",
        tagged(
            "src=\"http://test.com/1.webp\" data-src=\"http://test.com/2.jpeg\"",
            LOWRES_WEBP
        )
    );
    let expected = format!(
        "<head></head><body>{}<img data-pagespeed-high-res-src=\"http://test.com/1.webp\" \
        data-src=\"http://test.com/2.jpeg\" src=\"{LOWRES_WEBP}\" \
        onload=\"{code}\" onerror=\"this.onerror=null;{code}\"/></body>",
        onload_block(),
        code = onload_js(),
    );
    let (output, _) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, expected);
}

#[test]
fn no_delay_with_onload_attribute() {
    // A foreign onload vetoes the rewrite but the low-res tag still
    // goes (fall-through to the tail delete).
    let input = format!(
        "<head></head><body><img src=\"http://test.com/2.jpeg\" onload=\"do_something();\" \
        data-pagespeed-low-res-src=\"{LOWRES_JPEG}\"/></body>"
    );
    let expected = "<head></head><body>\
        <img src=\"http://test.com/2.jpeg\" onload=\"do_something();\"/></body>";
    let (output, filter) = run_case("inline_preview_images", &input, config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.log_status().1), 0);
}

#[test]
fn delay_with_pagespeed_added_onload() {
    let beacon = std::str::from_utf8(BEACON_IMAGE_ONLOAD_CODE).unwrap();
    let input = format!(
        "<head></head><body><img src=\"http://test.com/2.jpeg\" onload=\"{beacon}\" \
        data-pagespeed-low-res-src=\"{LOWRES_JPEG}\"/></body>"
    );
    let expected = format!(
        "<head></head><body>{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/2.jpeg", LOWRES_JPEG),
    );
    let (output, _) = run_case("inline_preview_images", &input, config());
    assert_eq!(output, expected);
}

#[test]
fn blank_image() {
    let mut fixture = config();
    fixture
        .options
        .set_option(
            "UseBlankImageForInlinePreview",
            "on",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let expected = format!(
        "<head></head><body>{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/1.webp", LOWRES_WEBP),
    );
    let (output, _) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, expected);
}

#[test]
fn blank_image_on_mobile() {
    // Blank previews stay inplace even for aggressive mobile.
    let mut fixture = mobile_config();
    fixture
        .options
        .set_option(
            "UseBlankImageForInlinePreview",
            "on",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.jpeg\"", LOWRES_JPEG)
    );
    let expected = format!(
        "<head></head><body>{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/1.jpeg", LOWRES_JPEG),
    );
    let (output, _) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, expected);
}

#[test]
fn mobile_aggressive() {
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.jpeg\"", LOWRES_JPEG)
    );
    let expected = format!(
        "<head></head><body>\
        <img data-pagespeed-high-res-src=\"http://test.com/1.jpeg\"/>\
        {}\
        {}\
        {}</body>",
        inline_script(false),
        add_lowres_script("http://test.com/1.jpeg", LOWRES_JPEG),
        highres_script(false),
    );
    let (output, filter) = run_case("inline_preview_images", &input, mobile_config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.log_status().1), 1);
}

#[test]
fn mobile_with_url_valued_attribute() {
    let mut fixture = mobile_config();
    fixture.url_valued.push((
        "img".to_string(),
        "data-src".to_string(),
        ps_config::options::Category::Image,
    ));
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("data-src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let (output, _) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, input);
}

#[test]
fn mobile_lazyload() {
    let mut fixture = mobile_config();
    fixture
        .options
        .set_option(
            "EnableLazyLoadHighResImages",
            "on",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.jpeg\"", LOWRES_JPEG)
    );
    let expected = format!(
        "<head></head><body>\
        <img data-pagespeed-high-res-src=\"http://test.com/1.jpeg\"/>\
        {}\
        {}\
        {}</body>",
        inline_script(false),
        add_lowres_script("http://test.com/1.jpeg", LOWRES_JPEG),
        highres_script(true),
    );
    let (output, _) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, expected);
}

#[test]
fn jpeg_on_input_element() {
    let input = format!(
        "<head></head><body>\
        <input type=\"image\" src=\"http://test.com/1.jpeg\" \
        data-pagespeed-low-res-src=\"{LOWRES_JPEG}\" /></body>"
    );
    let expected = format!(
        "<head></head><body>\
        <input type=\"image\" data-pagespeed-high-res-src=\"http://test.com/1.jpeg\"/>\
        {}\
        {}\
        {}</body>",
        inline_script(false),
        add_lowres_script("http://test.com/1.jpeg", LOWRES_JPEG),
        highres_script(false),
    );
    let (output, _) = run_case("inline_preview_images", &input, mobile_config());
    assert_eq!(output, expected);
}

#[test]
fn multiple_same_image() {
    // One map entry serves both copies.
    let img = tagged("src=\"http://test.com/1.jpeg\"", LOWRES_JPEG);
    let input = format!("<head></head><body>{img}{img}</body>");
    let expected = format!(
        "<head></head><body>\
        <img data-pagespeed-high-res-src=\"http://test.com/1.jpeg\"/>\
        <img data-pagespeed-high-res-src=\"http://test.com/1.jpeg\"/>\
        {}\
        {}\
        {}</body>",
        inline_script(false),
        add_lowres_script("http://test.com/1.jpeg", LOWRES_JPEG),
        highres_script(false),
    );
    let (output, filter) = run_case("inline_preview_images", &input, mobile_config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.log_status().1), 2);
}

#[test]
fn no_head_tag() {
    let input = format!(
        "<body>{}</body>",
        tagged("src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let expected = format!(
        "<body>{}{}</body>",
        onload_block(),
        rewritten_img("http://test.com/1.webp", LOWRES_WEBP),
    );
    let (output, _) = run_case("inline_preview_images", &input, config());
    assert_eq!(output, expected);
}

#[test]
fn pcache_miss() {
    let mut fixture = config();
    fixture.finder_availability = FinderAvailability::NoDataYet;
    let input = format!(
        "<head></head><body>{}</body>",
        tagged("src=\"http://test.com/1.webp\"", LOWRES_WEBP)
    );
    let (output, filter) = run_case("inline_preview_images", &input, fixture);
    assert_eq!(output, input);
    assert_eq!(
        filter.with(|filter| filter.log_status()),
        (Some(DelayImagesHtmlStatus::PropertyCacheMiss), 0)
    );
}

#[test]
fn multiple_bodies() {
    let input = format!(
        "<head></head>\
        <body>{}</body>\
        <body>{}</body>",
        tagged("src=\"http://test.com/1.webp\"", LOWRES_WEBP),
        tagged("src=\"http://test.com/2.jpeg\"", LOWRES_JPEG),
    );
    // The libraries land once; each body gets its data + trigger.
    let expected = format!(
        "<head></head><body>\
        <img data-pagespeed-high-res-src=\"http://test.com/1.webp\"/>\
        {}\
        {}\
        {}</body><body>\
        <img data-pagespeed-high-res-src=\"http://test.com/2.jpeg\"/>\
        {}\
        {}</body>",
        inline_script(false),
        add_lowres_script("http://test.com/1.webp", LOWRES_WEBP),
        highres_script(false),
        add_lowres_script("http://test.com/2.jpeg", LOWRES_JPEG),
        highres_script(false),
    );
    let (output, _) = run_case("inline_preview_images", &input, mobile_config());
    assert_eq!(output, expected);
}

#[test]
fn script_optimized() {
    let (output, _) = run_case(
        "optimized",
        &format!(
            "<head></head><body>{}</body>",
            tagged("src=\"http://test.com/1.jpeg\"", LOWRES_JPEG)
        ),
        mobile_config(),
    );
    assert!(!output.contains("/*"), "no comments in optimized code");
}

#[test]
fn script_debug() {
    let mut fixture = mobile_config();
    fixture.debug = true;
    let (output, _) = run_case(
        "debug",
        &format!(
            "<head></head><body>{}</body>",
            tagged("src=\"http://test.com/1.jpeg\"", LOWRES_JPEG)
        ),
        fixture,
    );
    assert!(!output.contains("/*"), "no comments in debug code");
}

#[test]
fn onload_code_guarded() {
    // The literal must stay free of quote and & characters so it
    // serializes into the attribute byte-identically.
    assert_eq!(
        onload_js(),
        "if(window.pagespeed){if(pagespeed.switchToHighResAndMaybeBeacon){\
        pagespeed.switchToHighResAndMaybeBeacon(this);}}"
    );
}
