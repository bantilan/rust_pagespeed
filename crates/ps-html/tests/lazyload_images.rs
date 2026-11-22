//! Lazyload tests ported from `lazyload_images_filter_test.cc`.
//!
//! NOTE: log assertions read the filter's recorded statuses instead
//! of a log channel, which does not exist yet. `LazyloadImagesPreserveURLsOn`
//! ports as the options veto it really is (the chain omits the filter,
//! so the parse runs bare). One hand test covers the abort script, an
//! upstream-untested branch.
#[path = "harness.rs"]
mod harness;

use std::collections::HashSet;

use harness::{SharedFilter, SharedSink};
use ps_cache::namer::ResourceNamer;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_config::RewriteLevel;
use ps_html::filters::lazyload_images::{
    FinderAvailability, LazyloadHtmlStatus, LazyloadImagesConfig, LazyloadImagesFilter,
    LazyloadLogRecord, LazyloadStatus, BEACON_IMAGE_ONLOAD_CODE, IMAGE_ONLOAD_CODE,
    LOAD_ALL_IMAGES, OVERRIDE_ATTRIBUTE_FUNCTIONS,
};
use ps_html::parser::Parser;
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::url_namer::TestUrlNamer;
use ps_html::writer::WriterFilter;

type LlFilter = LazyloadImagesFilter<TestUrlNamer>;

/// Test blank-image URL (the `BLANK_GIF` asset URL in tests).
const BLANK: &str = "/psajs/1.0.gif";

fn onload_js() -> &'static str {
    std::str::from_utf8(IMAGE_ONLOAD_CODE).unwrap()
}

/// Fixture options: troublesome resources disallowed.
fn options() -> RewriteOptions {
    let mut options = RewriteOptions::new();
    options.disallow_troublesome_resources();
    options
}

fn config(debug: bool) -> LazyloadImagesConfig {
    LazyloadImagesConfig {
        options: options(),
        blank_image_url: BLANK.to_string(),
        debug,
        ..LazyloadImagesConfig::default()
    }
}

fn run_case(
    case: &str,
    html: &str,
    config: LazyloadImagesConfig,
) -> (String, SharedFilter<LlFilter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(LazyloadImagesFilter::new(config, TestUrlNamer::new()));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(html.as_bytes());
    parser.finish();
    (String::from_utf8(sink.bytes()).unwrap(), filter)
}

/// Expected runtime script (`GetLazyloadScriptHtml`).
fn lazyload_script_html(after_onload: bool, blank: &str, debug: bool) -> String {
    let mut html = String::from("<script type=\"text/javascript\" data-pagespeed-no-defer>");
    html.push_str(std::str::from_utf8(get_asset(StaticAsset::LazyloadImages, debug)).unwrap());
    html.push_str(&format!(
        "\npagespeed.lazyLoadInit({after_onload}, \"{blank}\");\n"
    ));
    html.push_str("</script>");
    html
}

/// Expected override postscript (`GetLazyloadPostscriptHtml`).
fn postscript_html() -> String {
    format!(
        "<script type=\"text/javascript\" data-pagespeed-no-defer>{}</script>",
        std::str::from_utf8(OVERRIDE_ATTRIBUTE_FUNCTIONS).unwrap()
    )
}

/// Expected rewritten image (`GenerateRewrittenImageTag`).
fn rewritten_img(url: &str, extra: &str, blank: &str) -> String {
    format!(
        "<img data-pagespeed-lazy-src=\"{url}\" {extra}src=\"{blank}\" \
        onload=\"{code}\" onerror=\"this.onerror=null;{code}\"/>",
        code = onload_js(),
    )
}

fn applied() -> LazyloadLogRecord {
    LazyloadLogRecord {
        status: LazyloadStatus::AppliedOk,
        blacklisted: false,
        critical: false,
    }
}

fn not_applied(blacklisted: bool, critical: bool) -> LazyloadLogRecord {
    LazyloadLogRecord {
        status: LazyloadStatus::NotApplied,
        blacklisted,
        critical,
    }
}

#[test]
fn single_head() {
    let input = "<head></head><body>\
        <img />\
        <img src=\"\" />\
        <noscript><img src=\"noscript.jpg\" /></noscript>\
        <noembed><img src=\"noembed.jpg\" /></noembed>\
        <marquee><img src=\"marquee.jpg\" /></marquee>\
        <img src=\"1.jpg\" />\
        <img src=\"1.jpg\" data-pagespeed-no-defer/>\
        <img src=\"1.jpg\" pagespeed_no_defer/>\
        <img src=\"1.jpg\" data-src=\"2.jpg\"/>\
        <img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhE\"/>\
        <img src=\"2's.jpg\" height=\"300\" width=\"123\" />\
        <input src=\"12.jpg\"type=\"image\" />\
        <input src=\"12.jpg\" />\
        <img src=\"1.jpg\" onload=\"blah();\" />\
        <img src=\"1.jpg\" class=\"123 dfcg-metabox\" />\
        </body>";
    let expected = format!(
        "<head>{}</head><body>\
        <img/>\
        <img src=\"\"/>\
        <noscript><img src=\"noscript.jpg\"/></noscript>\
        <noembed><img src=\"noembed.jpg\"/></noembed>\
        <marquee><img src=\"marquee.jpg\"/></marquee>\
        {}\
        <img src=\"1.jpg\" data-pagespeed-no-defer />\
        <img src=\"1.jpg\" pagespeed_no_defer />\
        <img src=\"1.jpg\" data-src=\"2.jpg\"/>\
        <img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhE\"/>\
        {}\
        <input src=\"12.jpg\" type=\"image\"/>\
        <input src=\"12.jpg\"/>\
        <img src=\"1.jpg\" onload=\"blah();\"/>\
        <img src=\"1.jpg\" class=\"123 dfcg-metabox\"/>\
        {}</body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("1.jpg", "", BLANK),
        rewritten_img("2's.jpg", "height=\"300\" width=\"123\" ", BLANK),
        postscript_html(),
    );
    let (output, filter) = run_case("lazyload_images", input, config(false));
    assert_eq!(output, expected);
    assert_eq!(
        filter.with(|filter| filter.log_status().1.to_vec()),
        vec![
            applied(),
            not_applied(false, false),
            applied(),
            not_applied(false, false)
        ]
    );
}

#[test]
fn blacklist() {
    let mut fixture = config(false);
    fixture
        .options
        .allow_resources_mut()
        .disallow("*blacklist*");
    let input = "<head></head><body>\
        <img src=\"http://www.1.com/blacklist.jpg\"/>\
        <img src=\"http://www.1.com/img1\"/>\
        <img src=\"img2\"/>\
        </body>";
    let expected = format!(
        "<head>{}</head><body>\
        <img src=\"http://www.1.com/blacklist.jpg\"/>\
        {}\
        {}\
        {}</body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("http://www.1.com/img1", "", BLANK),
        rewritten_img("img2", "", BLANK),
        postscript_html(),
    );
    let (output, filter) = run_case("lazyload_images", input, fixture);
    assert_eq!(output, expected);
    assert_eq!(
        filter.with(|filter| filter.log_status().1.to_vec()),
        vec![not_applied(true, false), applied(), applied()]
    );
}

#[test]
fn critical_images() {
    let mut namer = ResourceNamer::new();
    namer.set_id("ce");
    namer.set_hash("HASH");
    namer.set_name("critical4.jpg");
    namer.set_ext("jpg");
    let rewritten_url = format!("http://test.com/{}", namer.encode());

    let mut fixture = config(false);
    fixture.finder_availability = FinderAvailability::Available;
    fixture.critical_images = [
        "http://www.1.com/critical",
        "www.1.com/critical2",
        "http://test.com/critical3",
        "http://test.com/critical4.jpg",
    ]
    .into_iter()
    .map(ps_html::beacon_finder::url_hash)
    .collect::<HashSet<_>>();
    fixture.hash_length = 1;

    let input = format!(
        "<head></head><body>\
        <img src=\"http://www.1.com/critical\"/>\
        <img src=\"http://www.1.com/critical2\"/>\
        <img src=\"critical3\"/>\
        <img src=\"{rewritten_url}\"/>\
        </body>"
    );
    // The schemeless set entry never matches, so critical2 rewrites;
    // the ce URL decodes to critical4.jpg, so it stays.
    let expected = format!(
        "<head>{}</head><body>\
        <img src=\"http://www.1.com/critical\"/>\
        {}\
        <img src=\"critical3\"/>\
        <img src=\"{rewritten_url}\"/>\
        {}</body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("http://www.1.com/critical2", "", BLANK),
        postscript_html(),
    );
    let (output, filter) = run_case("lazyload_images", &input, fixture);
    assert_eq!(output, expected);
    let (html, records) = filter.with(|filter| {
        let (html, records) = filter.log_status();
        (html, records.to_vec())
    });
    assert_eq!(html, Some(LazyloadHtmlStatus::Active));
    assert_eq!(
        records,
        vec![
            not_applied(false, true),
            applied(),
            not_applied(false, true),
            not_applied(false, true),
        ]
    );
    let applied_count = records
        .iter()
        .filter(|r| r.status == LazyloadStatus::AppliedOk)
        .count();
    assert_eq!((applied_count, records.len() - applied_count), (1, 3));
}

#[test]
fn single_head_load_on_onload() {
    let mut fixture = config(false);
    fixture
        .options
        .set_option(
            "LazyloadImagesAfterOnload",
            "on",
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    let input = "<head></head><body><img src=\"1.jpg\" /></body>";
    let expected = format!(
        "<head>{}</head><body>{}{}</body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("1.jpg", "", BLANK),
        postscript_html(),
    );
    let (output, _) = run_case("lazyload_images", input, fixture);
    assert_eq!(output, expected);
}

#[test]
fn no_lazyload_with_onload_attribute() {
    let input = "<head></head><body><img src=\"1.jpg\" onload=\"do_something();\"/></body>";
    let expected = format!(
        "<head>{}</head><body><img src=\"1.jpg\" onload=\"do_something();\"/></body>",
        lazyload_script_html(true, BLANK, false),
    );
    let (output, _) = run_case("lazyload_images", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn lazyload_with_pagespeed_added_onload() {
    let beacon = std::str::from_utf8(BEACON_IMAGE_ONLOAD_CODE).unwrap();
    let input = format!("<head></head><body><img src=\"1.jpg\" onload=\"{beacon}\"/></body>");
    let expected = format!(
        "<head>{}</head><body>{}{}</body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("1.jpg", "", BLANK),
        postscript_html(),
    );
    let (output, _) = run_case("lazyload_images", &input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn multiple_bodies() {
    let input = "<body><img src=\"1.jpg\" /></body>\
        <body></body>\
        <body><script></script><img src=\"2.jpg\" /><script></script><img src=\"3.jpg\" /><script></script></body>";
    let expected = format!(
        "{}<body>{}{}</body><body></body><body>\
        <script></script>{}{}<script></script>{}{}<script></script></body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("1.jpg", "", BLANK),
        postscript_html(),
        rewritten_img("2.jpg", "", BLANK),
        postscript_html(),
        rewritten_img("3.jpg", "", BLANK),
        postscript_html(),
    );
    let (output, _) = run_case("lazyload_images", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn no_head_tag() {
    let input = "<body><img src=\"1.jpg\" /></body>";
    let expected = format!(
        "{}<body>{}{}</body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("1.jpg", "", BLANK),
        postscript_html(),
    );
    let (output, _) = run_case("lazyload_images", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn preserve_urls_on() {
    // Preserve-URLs vetoes the filter at the options level, so the
    // chain omits it and the parse runs bare.
    let mut options = options();
    options
        .set_option("ImagePreserveURLs", "on", ps_config::Scope::ProcessStrict)
        .unwrap();
    options
        .filters_mut()
        .set_level(RewriteLevel::TestingCoreFilters);
    assert!(!options.filters().enabled(Filter::LazyloadImages));

    let input = "<body><img src=\"1.jpg\"/></body>";
    let sink = SharedSink::default();
    let mut parser = Parser::new();
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(b"http://test.com/lazyload_images.html", false);
    parser.write(input.as_bytes());
    parser.finish();
    assert_eq!(String::from_utf8(sink.bytes()).unwrap(), input);
}

#[test]
fn custom_image_url() {
    let blank = "http://blank.com/1.gif";
    let mut fixture = config(false);
    fixture
        .options
        .set_option(
            "LazyloadImagesBlankUrl",
            blank,
            ps_config::Scope::ProcessStrict,
        )
        .unwrap();
    let input = "<body><img src=\"1.jpg\" /></body>";
    let expected = format!(
        "{}<body>{}{}</body>",
        lazyload_script_html(true, blank, false),
        rewritten_img("1.jpg", "", blank),
        postscript_html(),
    );
    let (output, _) = run_case("lazyload_images", input, fixture);
    assert_eq!(output, expected);
}

#[test]
fn dfcg_class() {
    let input = "<body class=\"dfcg-slideshow\">\
        <img src=\"1.jpg\"/>\
        <div class=\"dfcg\"><img src=\"1.jpg\"/></div>\
        </body>";
    let expected = format!("{}{}", lazyload_script_html(true, BLANK, false), input);
    let (output, _) = run_case("DfcgClass", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn nivo_class() {
    let input = "<body><div class=\"nivo_sl\"><img src=\"1.jpg\"/></div>\
        <img class=\"nivo\" src=\"1.jpg\"/></body>";
    let expected = format!("{}{}", lazyload_script_html(true, BLANK, false), input);
    let (output, _) = run_case("NivoClass", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn class_contains_slider() {
    let input = "<body><div class=\"SliderName2\"><img src=\"1.jpg\"/></div>\
        <img class=\"my_sLiDer\" src=\"1.jpg\"/></body>";
    let expected = format!("{}{}", lazyload_script_html(true, BLANK, false), input);
    let (output, _) = run_case("SliderClass", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn no_images() {
    let input = "<head></head><body></body>";
    let expected = format!(
        "<head>{}</head><body></body>",
        lazyload_script_html(true, BLANK, false)
    );
    let (output, filter) = run_case("NoImages", input, config(false));
    assert_eq!(output, expected);
    assert!(filter.with(|filter| filter.log_status().1.is_empty()));
}

#[test]
fn lazyload_script_optimized() {
    let (output, _) = run_case(
        "optimized",
        "<head></head><body><img src=\"1.jpg\"></body>",
        config(false),
    );
    assert!(!output.contains("/*"), "no comments in optimized code");
}

#[test]
fn lazyload_script_debug() {
    let (output, _) = run_case(
        "debug",
        "<head></head><body><img src=\"1.jpg\"></body>",
        config(true),
    );
    assert!(!output.contains("/*"), "no comments in debug code");
}

#[test]
fn disabled_with_jquery_slider() {
    let input = "<body><head><script src=\"jquery.sexyslider.js\"/></head>\
        <body><img src=\"1.jpg\"/></body>";
    let expected = format!("{}{}", lazyload_script_html(true, BLANK, false), input);
    let (output, _) = run_case("JQuerySlider", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn disabled_with_jquery_slider_after_head() {
    let input = "<head></head><body>\
        <script src=\"jquery.sexyslider.js\"/>\
        <img src=\"1.jpg\"/>\
        </body>";
    let expected = format!(
        "<head>{}</head><body>\
        <script src=\"jquery.sexyslider.js\"/>\
        <img src=\"1.jpg\"/>\
        </body>",
        lazyload_script_html(true, BLANK, false),
    );
    let (output, _) = run_case("abort_script_inserted", input, config(false));
    assert_eq!(output, expected);
}

#[test]
fn abort_script_loads_rewritten_images() {
    // Hand test: images rewritten before the slider script appears get
    // a load-everything fallback; later images stay put and no body
    // postscript follows (the abort path returns before bodies). The
    // override lands before the fallback: after-insertions revisit the
    // inserting filter, in both engines.
    let input = "<head></head><body>\
        <img src=\"1.jpg\"/>\
        <script src=\"jquery.sexyslider.js\"></script>\
        <img src=\"2.jpg\"/>\
        </body>";
    let expected = format!(
        "<head>{}</head><body>\
        {}\
        <script src=\"jquery.sexyslider.js\"></script>\
        {}\
        <script type=\"text/javascript\">{}</script>\
        <img src=\"2.jpg\"/>\
        </body>",
        lazyload_script_html(true, BLANK, false),
        rewritten_img("1.jpg", "", BLANK),
        postscript_html(),
        std::str::from_utf8(LOAD_ALL_IMAGES).unwrap(),
    );
    let (output, filter) = run_case("abort_with_rewrites", input, config(false));
    assert_eq!(output, expected);
    assert_eq!(
        filter.with(|filter| filter.log_status().1.to_vec()),
        vec![applied()]
    );
}

#[test]
fn disabled_for_old_blackberry() {
    let mut fixture = config(false);
    fixture.supports_lazyload_images = false;
    let input = "<head></head><body><img src=\"1.jpg\"/></body>";
    let (output, filter) = run_case("blackberry_useragent", input, fixture);
    assert_eq!(output, input);
    assert_eq!(
        filter.with(|filter| filter.log_status().0),
        Some(LazyloadHtmlStatus::UserAgentNotSupported)
    );
}

#[test]
fn disabled_for_googlebot() {
    let mut fixture = config(false);
    fixture.supports_lazyload_images = false;
    let input = "<head></head><body><img src=\"1.jpg\"/></body>";
    let (output, filter) = run_case("googlebot_useragent", input, fixture);
    assert_eq!(output, input);
    assert_eq!(
        filter.with(|filter| filter.log_status().0),
        Some(LazyloadHtmlStatus::UserAgentNotSupported)
    );
}

#[test]
fn disabled_for_xhr() {
    let mut fixture = config(false);
    fixture.is_xml_http_request = true;
    let input = "<head></head><body><img src=\"1.jpg\"/></body>";
    let (output, filter) = run_case("xhr_requests", input, fixture);
    assert_eq!(output, input);
    assert_eq!(
        filter.with(|filter| filter.log_status().0),
        Some(LazyloadHtmlStatus::Disabled)
    );
}

#[test]
fn image_onload_code_is_guarded() {
    // The literal must stay free of quote and & characters so it
    // serializes into the attribute byte-identically.
    assert_eq!(
        onload_js(),
        "if(window.pagespeed){if(pagespeed.lazyLoadImages){\
        pagespeed.lazyLoadImages.loadIfVisibleAndMaybeBeacon(this);}}"
    );
}
