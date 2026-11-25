//! Eligibility of existing width lists for an explicit layout sizes policy.
use ps_html::srcset::{supports_width_sizes, MAX_SIZES_CANDIDATES, MAX_SIZES_SRCSET_BYTES};

#[test]
fn accepts_width_lists_without_changing_author_text() {
    for input in [
        "large.jpg 800w, small.jpg 300w, middle.jpg 768w",
        "  /small.jpg\t0300w ,\n/large.jpg 800w\r\n",
        "https://example.org/a%20b.jpg?a=1&b=2 640w",
        "data:image/png;base64,AAAA 300w, other.jpg 600w",
        "data:image/svg+xml,%3Csvg%3E,%3C/svg%3E 300w",
        "café.jpg 400w",
        "one.jpg 4294967295w",
    ] {
        let before = input.as_bytes().to_vec();
        assert!(supports_width_sizes(input.as_bytes()), "{input:?}");
        assert_eq!(input.as_bytes(), before);
    }
}

#[test]
fn rejects_unsupported_or_ambiguous_lists() {
    for input in [
        "",
        " \n",
        "one.jpg",
        "one.jpg 1x",
        "one.jpg 2x, two.jpg 3x",
        "one.jpg 300w, two.jpg 2x",
        "one.jpg 300w, two.jpg",
        "one.jpg 300w, two.jpg 300w",
        "one.jpg 0300w, two.jpg 300w",
        "one.jpg 0w",
        "one.jpg -1w",
        "one.jpg +1w",
        "one.jpg 1.5w",
        "one.jpg 4294967296w",
        "one.jpg 300W",
        "one.jpg 300w 200h",
        "one.jpg 300w 1x",
        ",one.jpg 300w",
        "one.jpg 300w,",
        "one.jpg 300w,, two.jpg 600w",
        "one.jpg, 300w",
        "one.jpg 300w two.jpg 600w",
        "one.jpg (300w)",
        "one\0.jpg 300w",
        "one\u{b}.jpg 300w",
        "<img> 300w",
        "\"one.jpg\" 300w",
        "one.jpg 300w;",
        "one.jpg 300w, ,",
    ] {
        assert!(!supports_width_sizes(input.as_bytes()), "{input:?}");
    }
    assert!(!supports_width_sizes(b"\xff.jpg 300w"));
}

#[test]
fn limits_bytes_and_candidates_before_allocation() {
    let list = (1..=MAX_SIZES_CANDIDATES)
        .map(|w| format!("image{w}.jpg {w}w"))
        .collect::<Vec<_>>()
        .join(", ");
    assert!(supports_width_sizes(list.as_bytes()));
    assert!(!supports_width_sizes(
        format!("{list}, extra.jpg 1000w").as_bytes()
    ));
    let at_limit = format!("{} 1w", "x".repeat(MAX_SIZES_SRCSET_BYTES - 3));
    assert_eq!(at_limit.len(), MAX_SIZES_SRCSET_BYTES);
    assert!(supports_width_sizes(at_limit.as_bytes()));
    assert!(!supports_width_sizes(format!("x{at_limit}").as_bytes()));
    assert!(!supports_width_sizes(&vec![
        b' ';
        MAX_SIZES_SRCSET_BYTES + 1
    ]));
}

#[allow(dead_code)]
#[path = "harness.rs"]
mod harness;

use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_html::chain::{build_chain, ChainContext, ChainFilter};
use ps_html::driver::Driver;
use std::rc::Rc;

const SIZES: &str = "(max-width: 800px) calc(100vw - 60px), 645px";

fn policy() -> RewriteOptions {
    let mut options = RewriteOptions::new();
    for (name, value) in [
        ("SrcsetSizesEnabled", "on"),
        ("SrcsetSizesClass", "hero"),
        ("SrcsetSizes", SIZES),
    ] {
        options.set_option(name, value, Scope::Server).unwrap();
    }
    options
}

fn render(options: &RewriteOptions, html: &str) -> String {
    let sink = harness::SharedSink::default();
    let mut driver = Driver::new(options, &ChainContext::new(true, true), sink.clone(), None);
    assert!(!driver
        .skipped_slots()
        .iter()
        .any(|(_, slot)| *slot == ChainFilter::SrcsetSizes));
    driver.start_parse(b"https://test.com/page", false);
    driver.write(html.as_bytes());
    driver.finish();
    String::from_utf8(sink.bytes()).unwrap()
}

#[test]
fn driver_defaults_and_incomplete_policy_are_byte_equivalent() {
    let input = "<picture><source srcset='a.jpg 300w'><img class='hero' src='a.jpg' srcset='a.jpg 300w, b.jpg 800w' sizes='100vw'></picture><img class='hero' srcset='a.jpg 300w'>";
    for name in ["SrcsetSizesEnabled", "SrcsetSizesClass", "SrcsetSizes"] {
        let mut options = policy();
        options
            .set_option(
                name,
                if name == "SrcsetSizesEnabled" {
                    "off"
                } else {
                    ""
                },
                Scope::Server,
            )
            .unwrap();
        assert!(!build_chain(&options, &ChainContext::new(true, true))
            .early_pre
            .contains(&ChainFilter::SrcsetSizes));
        assert_eq!(render(&options, input), input);
    }
    assert_eq!(render(&RewriteOptions::new(), input), input);
}

#[test]
fn driver_changes_only_explicitly_targeted_sizes() {
    let input = "<img class='extra hero other' src='fallback.jpg' srcset='small.jpg 300w, large.jpg 800w' sizes='100vw' width='800' height='600' loading='eager' fetchpriority='high'><img class='hero' srcset='a.jpg 300w'><img class='heroine' srcset='a.jpg 300w' sizes='100vw'><img class='Hero' srcset='a.jpg 300w' sizes='100vw'>";
    let expected = input
        .replacen("sizes='100vw'", &format!("sizes='{SIZES}'"), 1)
        .replacen(
            "<img class='hero' srcset='a.jpg 300w'>",
            &format!("<img class='hero' srcset='a.jpg 300w' sizes=\"{SIZES}\">"),
            1,
        );
    assert_eq!(render(&policy(), input), expected);
}

#[test]
fn driver_preserves_unsupported_elements_and_ambiguous_attributes() {
    let cases = [
        "<picture><source media='(min-width:800px)' type='image/avif' srcset='a.avif 800w'><img class='hero' srcset='a.jpg 300w' sizes='100vw'></picture>",
        "<noscript><img class='hero' srcset='a.jpg 300w'></noscript>",
        "<template><img class='hero' srcset='a.jpg 300w'></template>",
        "<svg><img class='hero' srcset='a.jpg 300w'></svg>",
        "<img data-pagespeed-no-transform class='hero' srcset='a.jpg 300w'>",
        "<img pagespeed_no_transform class='hero' srcset='a.jpg 300w'>",
        "<img class='hero' src='a.jpg'>",
        "<img class='hero' srcset='a.jpg 1x, b.jpg 2x'>",
        "<img class='hero' srcset='a.jpg 300w, b.jpg 300w'>",
        "<img class='hero' srcset='a.jpg 300w,'>",
        "<img class='hero' class='other' srcset='a.jpg 300w'>",
        "<img class='hero' srcset='a.jpg 300w' srcset='b.jpg 600w'>",
        "<img class='hero' srcset='a.jpg 300w' sizes='1px' sizes='2px'>",
        "<source class='hero' srcset='a.jpg 300w'>",
    ];
    for input in cases {
        assert_eq!(render(&policy(), input), input, "{input}");
    }
    let long = format!(
        "<img class='hero' srcset='{} 300w'>",
        "x".repeat(MAX_SIZES_SRCSET_BYTES)
    );
    assert_eq!(render(&policy(), &long), long);
    let long_class = format!(
        "<img class='hero {}' srcset='a.jpg 300w'>",
        "x".repeat(1024)
    );
    assert_eq!(render(&policy(), &long_class), long_class);
    let many_attributes = format!(
        "<img class='hero' srcset='a.jpg 300w' {}>",
        (0..64)
            .map(|i| format!("data-x{i}='a'"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(render(&policy(), &many_attributes), many_attributes);
}

#[test]
fn driver_decodes_target_attributes_without_rewriting_srcset_bytes() {
    let input = "<img CLASS='he&#114;o' SRCSET='a.jpg?a=1&amp;b=2 300w, b.jpg 800w' SIZES='100vw'>";
    assert_eq!(
        render(&policy(), input),
        input.replace("SIZES='100vw'", &format!("SIZES='{SIZES}'"))
    );
}

#[test]
fn driver_streaming_and_reuse_reset_context_state() {
    let options = policy();
    let sink = harness::SharedSink::default();
    let mut driver = Driver::new(&options, &ChainContext::new(true, true), sink.clone(), None);
    driver.start_parse(b"https://test.com/page", false);
    driver.write(b"<picture>");
    driver.flush();
    driver.write(b"<img class='hero' srcset='a.jpg 300w'></picture>");
    driver.flush();
    driver.write(b"<img class='hero' srcset='a.jpg 300w'>");
    driver.finish();
    let output = String::from_utf8(sink.bytes()).unwrap();
    assert_eq!(output.matches("sizes=").count(), 1);
    // Reusing the same driver must not inherit a prior blocked context.
    driver.start_parse(b"https://test.com/other", false);
    driver.write(b"<img class='hero' srcset='a.jpg 300w'>");
    driver.finish();
    assert_eq!(
        String::from_utf8(sink.bytes())
            .unwrap()
            .matches("sizes=")
            .count(),
        2
    );
}

#[test]
fn cached_plan_reuses_policy_and_lcp_preload_sees_corrected_sizes() {
    use ps_config::filter::Filter;
    use ps_html::server_context::{ResourceDriver, ServerContext};
    use std::cell::RefCell;
    use std::sync::Arc;
    let fetcher = Arc::new(ps_http::mock::MockUrlFetcher::new());
    let server = Rc::new(ServerContext::new_with_metadata(
        Arc::new(ps_cache::cache::MemoryCache::new()),
        Arc::new(ps_cache::cache::MemoryCache::new()),
        fetcher.clone(),
        1_700_000_000_000,
    ));
    let mut options = policy();
    options.filters_mut().enable(Filter::PrioritizeLcpImages);
    let ctx = ChainContext::new(true, true);
    let first = server.chain_plan(&options, &ctx);
    let second = server.chain_plan(&options, &ctx);
    assert!(Rc::ptr_eq(
        first.srcset_sizes_policy.as_ref().unwrap(),
        second.srcset_sizes_policy.as_ref().unwrap()
    ));
    let mut other = options.clone();
    other
        .set_option("SrcsetSizes", "100px", Scope::Server)
        .unwrap();
    assert_ne!(first, server.chain_plan(&other, &ctx));
    let resource_driver = Rc::new(ResourceDriver::new(
        server.clone(),
        Rc::new(RefCell::new(options.clone())),
        "https://test.com/page",
    ));
    let sink = harness::SharedSink::default();
    let mut driver = Driver::new(&options, &ctx, sink.clone(), Some(resource_driver));
    driver.start_parse(b"https://test.com/page", false);
    driver.write(b"<html><head></head><body><img class='hero' src='a.jpg' srcset='a.jpg 300w, b.jpg 800w' sizes='100vw'></body></html>");
    driver.finish();
    let output = String::from_utf8(sink.bytes()).unwrap();
    assert!(
        output.contains(&format!("imagesizes=\"{SIZES}\"")),
        "{output}"
    );
    assert!(
        output.contains("imagesrcset=\"a.jpg 300w, b.jpg 800w\""),
        "{output}"
    );
    assert_eq!(fetcher.fetch_count(), 0);
    // ImageRewrite removes its no-transform marker. Early placement must
    // have honored that marker before removal, rather than modifying sizes.
    options.filters_mut().enable(Filter::RecompressJpeg);
    let resources = Rc::new(ResourceDriver::new(
        server,
        Rc::new(RefCell::new(options.clone())),
        "https://test.com/page",
    ));
    let sink = harness::SharedSink::default();
    let mut driver = Driver::new(&options, &ctx, sink.clone(), Some(resources));
    assert!(driver.plan().pre.contains(&ChainFilter::ImageRewrite));
    driver.start_parse(b"https://test.com/page", false);
    driver.write(b"<img class='hero' data-pagespeed-no-transform src='a.jpg' srcset='a.jpg 300w' sizes='100vw'>");
    driver.finish();
    let output = String::from_utf8(sink.bytes()).unwrap();
    assert!(output.contains("sizes='100vw'"), "{output}");
    assert!(!output.contains(SIZES), "{output}");
    assert!(!output.contains("data-pagespeed-no-transform"), "{output}");
    assert_eq!(fetcher.fetch_count(), 0);
}

fn automatic() -> RewriteOptions {
    let mut options = RewriteOptions::new();
    options
        .set_option("SrcsetAutoSizes", "on", Scope::Server)
        .unwrap();
    options
}

#[test]
fn automatic_native_lazy_sizing_preserves_fallback_and_needs_no_class() {
    let base = "<img loading='lazy' width='800' height='600' src='fallback.jpg' srcset='a.jpg 300w, b.jpg 800w' sizes='(max-width: 800px) 100vw, 800px'>";
    let expected = base.replace("sizes='", "sizes='auto, ");
    assert_eq!(render(&automatic(), base), expected);
    assert_eq!(render(&RewriteOptions::new(), base), base);
    // Reprocessing already-automatic markup must not duplicate its prefix.
    assert_eq!(render(&automatic(), &expected), expected);
    let absent = base.replace(" sizes='(max-width: 800px) 100vw, 800px'", "");
    assert_eq!(
        render(&automatic(), &absent),
        absent.replace('>', " sizes=\"auto, 100vw\">")
    );
    let encoded = base
        .replace("loading='lazy'", "LOADING='la&#122;y'")
        .replace("sizes='(max-width: 800px) 100vw, 800px'", "SIZES='100vw'");
    assert_eq!(
        render(&automatic(), &encoded),
        encoded.replace("SIZES='100vw'", "SIZES='auto, 100vw'")
    );
}

#[test]
fn automatic_skips_eager_priority_ambiguous_and_missing_layout() {
    let base = "<img loading='lazy' width='800' height='600' srcset='a.jpg 300w, b.jpg 800w' sizes='100vw'>";
    let cases = [
        base.replace("loading='lazy'", "loading='eager'"),
        base.replace(" loading='lazy'", ""),
        base.replace(" width='800'", ""),
        base.replace("height='600'", "height='0'"),
        base.replace("width='800'", "width='100%'"),
        base.replace("width='800'", "width='4294967296'"),
        base.replace("height='600'", "height='600' HEIGHT='300'"),
        base.replace("loading='lazy'", "loading='lazy' loading='eager'"),
        base.replace("sizes='100vw'", "sizes='100vw' SIZES='50vw'"),
        base.replace("<img ", "<img fetchpriority='HIGH' "),
        base.replace("<img ", "<img onload='custom()' "),
        base.replace("<img ", "<img data-pagespeed-lazy-src='original.jpg' "),
        base.replace("<img ", "<img data-src='original.jpg' "),
        base.replace("<img ", "<img data-pagespeed-no-transform "),
        base.replace("<img ", "<img pagespeed_no_transform "),
        base.replace("a.jpg 300w, b.jpg 800w", "a.jpg 1x, b.jpg 2x"),
        format!("<picture>{base}</picture>"),
        format!("<noscript>{base}</noscript>"),
        format!("<template>{base}</template>"),
    ];
    for input in cases {
        assert_eq!(render(&automatic(), &input), input, "{input}");
    }
    let oversized = base.replace("100vw", &"x".repeat(1025));
    assert_eq!(render(&automatic(), &oversized), oversized);
}

#[test]
fn automatic_manual_precedence_and_js_lazy_fallback() {
    use ps_config::filter::Filter;
    let base = "<img class='hero' loading='lazy' width='800' height='600' srcset='a.jpg 300w' sizes='100vw'>";
    let mut options = policy();
    options
        .set_option("SrcsetAutoSizes", "on", Scope::Server)
        .unwrap();
    assert_eq!(
        render(&options, base),
        base.replace("sizes='100vw'", &format!("sizes='{SIZES}'"))
    );
    let mut native = automatic();
    native.filters_mut().enable(Filter::LazyloadImages);
    let plan = build_chain(&native, &ChainContext::new(true, true));
    assert!(!plan.early_pre.contains(&ChainFilter::SrcsetSizes));
    // Existing JS lazy filter stays enabled; the native mode stands down.
    assert!(plan.post.contains(&ChainFilter::LazyloadImages));
    let output = render(&native, base);
    assert!(!output.contains("auto,"), "{output}");
}
