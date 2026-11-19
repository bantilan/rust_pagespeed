//! Dedup tests ported from `dedup_inlined_images_filter_test.cc`.
//!
//! NOTE: the C++ inputs name files (`Cuppa.png`) and rely on the
//! inline-images pipeline to data-URL them before dedup runs. That
//! pipeline is not ported, so inputs here carry the data URLs
//! directly — the dedup filter only ever sees those. The `?PageSpeed=
//! noscript` URLs and noscript-redirect wrappers likewise come from
//! sibling filters and are dropped. `DedupInlinePreviewImages` is
//! skipped: it needs the delay-images filter.
#[path = "harness.rs"]
mod harness;

/// Inlined Cuppa.png (`kCuppaPngInlineData`, verbatim).
const CUPPA_PNG_INLINE_DATA: &str = concat!(
    "data:image/png;base64,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",
    "5CYII=",
);

use harness::{SharedFilter, SharedSink};
use ps_html::filters::dedup_inlined_images::{
    DedupInlinedImagesConfig, DedupInlinedImagesFilter, DII_INITIALIZER, MINIMUM_IMAGE_CUTOFF,
};
use ps_html::parser::Parser;
use ps_html::static_assets::{get_asset, StaticAsset};
use ps_html::writer::WriterFilter;

fn config() -> DedupInlinedImagesConfig {
    // The default hasher answers "0" to everything, like MockHasher.
    DedupInlinedImagesConfig::default()
}

fn run_case(
    case: &str,
    html: &str,
    config: DedupInlinedImagesConfig,
) -> (String, SharedFilter<DedupInlinedImagesFilter>) {
    let sink = SharedSink::default();
    let filter = SharedFilter::new(DedupInlinedImagesFilter::new(config));
    let mut parser = Parser::new();
    parser.add_filter(Box::new(filter.clone()));
    parser.add_filter(Box::new(WriterFilter::new(sink.clone())));
    parser.start_parse(format!("http://test.com/{case}.html").as_bytes(), false);
    parser.write(html.as_bytes());
    parser.finish();
    (String::from_utf8(sink.bytes()).unwrap(), filter)
}

/// The `<head>`/`<body>` wrapper (`kHtmlWrapperFormat`).
fn wrap(head: &str, body: &str) -> String {
    format!(
        "<head>\n  <title>Dedup Inlined Images Test</title>\n{head}</head>\n<body>{body}</body>\n"
    )
}

/// The utility script block the filter inserts before the first duplicate.
fn utility_script() -> String {
    let mut js = String::from("<script type=\"text/javascript\" data-pagespeed-no-defer>");
    js.push_str(std::str::from_utf8(get_asset(StaticAsset::DedupInlinedImages, false)).unwrap());
    js.push_str(std::str::from_utf8(DII_INITIALIZER).unwrap());
    js.push_str("</script>");
    js
}

/// A restoring snippet (`kInlinedScriptFormat`).
fn inline_script(script: u32, from: u32, to: u32) -> String {
    format!(
        "<script type=\"text/javascript\" id=\"pagespeed_script_{script}\" data-pagespeed-no-defer>\
        if(window.pagespeed){{if(pagespeed.dedupInlinedImages){{pagespeed.dedupInlinedImages.inlineImg(\
        'pagespeed_img_0{from}','pagespeed_img_0{to}','pagespeed_script_{script}');}}}}</script>"
    )
}

#[test]
fn simple() {
    let html = wrap("", "<div/>");
    let (output, filter) = run_case("simple", &html, config());
    assert_eq!(output, html);
    assert_eq!(filter.with(|filter| filter.stats()), (0, 0));
}

#[test]
fn inline_single_small_image() {
    // An id goes on the first occurrence.
    let input = wrap("", &format!("<img src='{CUPPA_PNG_INLINE_DATA}'>"));
    let expected = wrap(
        "",
        &format!("<img src='{CUPPA_PNG_INLINE_DATA}' id=\"pagespeed_img_01\">"),
    );
    let (output, filter) = run_case("inline_single_small_image", &input, config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.stats()), (1, 0));
}

#[test]
fn dont_touch_large_image() {
    // A plain-URL image is not a candidate (the C++ covers the same
    // input via the pipeline declining to inline it).
    let html = wrap("", "<img src='Puzzle.jpg'>");
    let (output, filter) = run_case("dont_inline_large_image", &html, config());
    assert_eq!(output, html);
    assert_eq!(filter.with(|filter| filter.stats()), (0, 0));
}

#[test]
fn dedup_second_small_image() {
    let img = format!("<img src='{CUPPA_PNG_INLINE_DATA}'>");
    let input = wrap("", &format!("{img}\n{img}"));
    let expected = wrap(
        "",
        &format!(
            "<img src='{CUPPA_PNG_INLINE_DATA}' id=\"pagespeed_img_01\">\n\
            {util}<img id=\"pagespeed_img_02\">{script}",
            util = utility_script(),
            script = inline_script(3, 1, 2),
        ),
    );
    let (output, filter) = run_case("dedup_second_small_image", &input, config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.stats()), (2, 1));
}

#[test]
fn dedup_many_small_images() {
    let img = format!("<img src='{CUPPA_PNG_INLINE_DATA}'>");
    let input = wrap("", &format!("{img}\n{img}\n{img}"));
    let expected = wrap(
        "",
        &format!(
            "<img src='{CUPPA_PNG_INLINE_DATA}' id=\"pagespeed_img_01\">\n\
            {util}<img id=\"pagespeed_img_02\">{script3}\n\
            <img id=\"pagespeed_img_04\">{script5}",
            util = utility_script(),
            script3 = inline_script(3, 1, 2),
            script5 = inline_script(5, 1, 4),
        ),
    );
    let (output, filter) = run_case("dedup_many_small_images", &input, config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.stats()), (3, 2));
}

#[test]
fn dedup_second_small_image_with_id() {
    // The first occurrence keeps its id; the duplicate is scripted.
    let input = wrap(
        "",
        &format!(
            "<img src='{CUPPA_PNG_INLINE_DATA}' id='xyzzy'>\n<img src='{CUPPA_PNG_INLINE_DATA}'>"
        ),
    );
    let expected = wrap(
        "",
        &format!(
            "<img src='{CUPPA_PNG_INLINE_DATA}' id='xyzzy'>\n\
            {}<img id=\"pagespeed_img_01\">\
            <script type=\"text/javascript\" id=\"pagespeed_script_2\" data-pagespeed-no-defer>\
            if(window.pagespeed){{if(pagespeed.dedupInlinedImages){{pagespeed.dedupInlinedImages.inlineImg(\
            'xyzzy','pagespeed_img_01','pagespeed_script_2');}}}}</script>",
            utility_script(),
        ),
    );
    let (output, filter) = run_case("dedup_second_small_image_with_id", &input, config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.stats()), (2, 1));
}

#[test]
fn dedup_second_small_image_with_attributes() {
    // All other attributes survive; only src goes.
    let input = wrap(
        "",
        &format!(
            "<img src='{CUPPA_PNG_INLINE_DATA}'>\n\
            <img src='{CUPPA_PNG_INLINE_DATA}' alt='xyzzy' id='plugh'>"
        ),
    );
    let expected = wrap(
        "",
        &format!(
            "<img src='{CUPPA_PNG_INLINE_DATA}' id=\"pagespeed_img_01\">\n\
            {}<img alt='xyzzy' id='plugh'>\
            <script type=\"text/javascript\" id=\"pagespeed_script_2\" data-pagespeed-no-defer>\
            if(window.pagespeed){{if(pagespeed.dedupInlinedImages){{pagespeed.dedupInlinedImages.inlineImg(\
            'pagespeed_img_01','plugh','pagespeed_script_2');}}}}</script>",
            utility_script(),
        ),
    );
    let (output, filter) = run_case("dedup_second_small_image_with_attributes", &input, config());
    assert_eq!(output, expected);
    assert_eq!(filter.with(|filter| filter.stats()), (2, 1));
}

#[test]
fn size_cutoff_boundary() {
    // Only images LONGER than the cutoff dedup.
    for size in [MINIMUM_IMAGE_CUTOFF, MINIMUM_IMAGE_CUTOFF + 1] {
        let mut src = String::from("data:image/gif;base64,");
        src.extend(std::iter::repeat('A').take(size - src.len()));
        assert_eq!(src.len(), size);
        let img = format!("<img src='{src}'>");
        let (output, _) = run_case(
            &format!("cutoff_{size}"),
            &format!("<body>{img}\n{img}</body>"),
            config(),
        );
        assert_eq!(
            output.contains("pagespeed_img_01"),
            size > MINIMUM_IMAGE_CUTOFF,
            "size {size}"
        );
    }
}

#[test]
fn disabled_without_lazyload_runtime() {
    // The BlackBerry UA lacks lazyload support, so deduping is off.
    let img = format!("<img src='{CUPPA_PNG_INLINE_DATA}'>");
    let html = wrap("", &format!("{img}\n{img}"));
    let mut disabled = config();
    disabled.supports_lazyload_images = false;
    let (output, filter) = run_case("disabled_for_old_blackberry", &html, disabled);
    assert_eq!(output, html);
    assert_eq!(filter.with(|filter| filter.stats()), (0, 0));
}
