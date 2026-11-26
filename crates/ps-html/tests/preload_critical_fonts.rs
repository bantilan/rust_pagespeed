//! Automatic critical font preloads (`preload_critical_fonts`).
#[path = "harness.rs"]
mod harness;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use ps_cache::cache::MemoryCache;
use ps_cache::hash::MockHasher;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::chain::{build_chain, ChainContext, ChainFilter};
use ps_html::driver::Driver;
use ps_html::filters::preload_critical_fonts::{
    PreloadCriticalFontsFilter, FONT_PRELOAD_BEACON_CONFIRMED, FONT_PRELOAD_CACHE_HIT,
    FONT_PRELOAD_CACHE_MISS, FONT_PRELOAD_DISCOVERED, FONT_PRELOAD_PRELOADED, FONT_PRELOAD_SKIPPED,
};
use ps_html::parser::Parser;
use ps_html::server_context::{ResourceDriver, ServerContext};
use ps_http::mock::MockUrlFetcher;

const PAGE: &str = "https://test.com/page";
const START_MS: i64 = 1_700_000_000_000;

const MANROPE_CSS: &str = "@font-face {\n  font-family: 'Manrope';\n  font-style: normal;\n  font-weight: 400;\n  src: url(https://cdn.test/manrope-400.woff2) format('woff2');\n}\nbody { font-family: Manrope, sans-serif; }\n";

fn fixture_with_fetcher() -> (
    Rc<ServerContext>,
    Rc<RefCell<RewriteOptions>>,
    Arc<MockUrlFetcher>,
) {
    let fetcher = Arc::new(MockUrlFetcher::new());
    let mut server = ServerContext::new_with_metadata(
        Arc::new(MemoryCache::new()),
        Arc::new(MemoryCache::new()),
        fetcher.clone(),
        START_MS,
    );
    server.set_hasher(Arc::new(MockHasher::new()));
    let server = Rc::new(server);
    let options = Rc::new(RefCell::new(RewriteOptions::new()));
    options.borrow_mut().add_domain("http://test.com/");
    options.borrow_mut().add_domain("https://test.com/");
    (server, options, fetcher)
}

fn fixture() -> (Rc<ServerContext>, Rc<RefCell<RewriteOptions>>) {
    let (server, options, _) = fixture_with_fetcher();
    (server, options)
}

fn driver_for(
    server: &Rc<ServerContext>,
    options: &Rc<RefCell<RewriteOptions>>,
) -> Rc<ResourceDriver> {
    Rc::new(ResourceDriver::new(server.clone(), options.clone(), PAGE))
}

fn render_filter(driver: &Rc<ResourceDriver>, html: &str) -> String {
    let out = harness::run_with_filters(html.as_bytes(), |parser: &mut Parser| {
        parser
            .add_early_pre_render_filter(Box::new(PreloadCriticalFontsFilter::new(driver.clone())));
    });
    String::from_utf8(out).unwrap()
}

fn stat(server: &Rc<ServerContext>, name: &str) -> i64 {
    server.variable(name).get()
}

#[test]
fn inline_face_emits_preload_first_in_head() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let html = format!(
        "<html><head><title>t</title><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let out = render_filter(&driver, &html);
    assert!(out.contains("rel=\"preload\""), "{out}");
    assert!(out.contains("as=\"font\""), "{out}");
    assert!(out.contains("type=\"font/woff2\""), "{out}");
    assert!(
        out.contains("href=\"https://cdn.test/manrope-400.woff2\""),
        "{out}"
    );
    assert!(out.contains("crossorigin=\"anonymous\""), "{out}");
    // Earliest in head: ahead of the title.
    let preload = out.find("rel=\"preload\"").unwrap();
    let title = out.find("<title>").unwrap();
    assert!(preload < title, "{out}");
    assert_eq!(stat(&server, FONT_PRELOAD_DISCOVERED), 1);
    assert_eq!(stat(&server, FONT_PRELOAD_PRELOADED), 1);
}

#[test]
fn caps_two_preloads_and_prunes() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let css = "@font-face { font-family: A; src: url(https://x.test/a.woff2); }\n\
         @font-face { font-family: B; src: url(https://x.test/b.woff2); }\n\
         @font-face { font-family: C; src: url(https://x.test/c.woff2); }\n\
         body { font-family: A; } h1 { font-family: B; } p { font-family: C; }";
    // h1/p elements exist so all three usages verify; the cap then
    // admits a+b and skips c.
    let html =
        format!("<html><head><style>{css}</style></head><body><h1>t</h1><p>x</p></body></html>");
    let out = render_filter(&driver, &html);
    assert_eq!(out.matches("rel=\"preload\"").count(), 2, "{out}");
    assert!(out.contains("https://x.test/a.woff2"), "{out}");
    assert!(out.contains("https://x.test/b.woff2"), "{out}");
    // c.woff2 appears only in the style block, never preloaded.
    assert_eq!(out.matches("c.woff2").count(), 1, "{out}");
    // Priority order preserved (a before b).
    assert!(
        out.find("https://x.test/a.woff2").unwrap() < out.find("https://x.test/b.woff2").unwrap(),
        "{out}"
    );
    // Cap overflow counts as skipped.
    assert_eq!(stat(&server, FONT_PRELOAD_SKIPPED), 1);
    // Icon-only and subset faces never emit.
    let css = "@font-face { font-family: Icons; src: url(https://x.test/i.woff2); \
         unicode-range: U+E000-F8FF; }\n\
         @font-face { font-family: Cyr; src: url(https://x.test/c.woff2); \
         unicode-range: U+0400-04FF; }\n\
         body { font-family: Icons; } p { font-family: Cyr; }";
    let html = format!("<html><head><style>{css}</style></head><body></body></html>");
    let out = render_filter(&driver, &html);
    assert!(!out.contains("rel=\"preload\""), "{out}");
}

#[test]
fn skips_unsafe_and_duplicate_markup() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    // Author preload wins: no duplicate.
    let html = format!(
        "<html><head><link rel=\"preload\" as=\"font\" type=\"font/woff2\" \
         href=\"https://cdn.test/manrope-400.woff2\" crossorigin=\"anonymous\">\
         <style>{MANROPE_CSS}</style></head><body></body></html>"
    );
    let out = render_filter(&driver, &html);
    assert_eq!(out.matches("rel=\"preload\"").count(), 1, "{out}");
    // No head: nothing emitted, nothing panics.
    let out = render_filter(&driver, "<p>no head here</p>");
    assert!(!out.contains("rel=\"preload\""), "{out}");
    // Print-only style ignored.
    let html = format!(
        "<html><head><style media=\"print\">{MANROPE_CSS}</style></head>\
         <body></body></html>"
    );
    let out = render_filter(&driver, &html);
    assert!(!out.contains("rel=\"preload\""), "{out}");
}

#[test]
fn external_stylesheet_loads_and_caches() {
    let (server, options, fetcher) = fixture_with_fetcher();
    fetcher.set_simple_response(
        "https://test.com/theme.css",
        "text/css",
        MANROPE_CSS.as_bytes(),
    );
    let driver = driver_for(&server, &options);
    let html = "<html><head><link rel=\"stylesheet\" href=\"/theme.css\"></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>";
    let out = render_filter(&driver, html);
    assert!(out.contains("manrope-400.woff2"), "{out}");
    assert!(out.contains("type=\"font/woff2\""), "{out}");
    // Second render reuses the cached decision (no re-parse).
    let driver = driver_for(&server, &options);
    let out = render_filter(&driver, html);
    assert!(out.contains("manrope-400.woff2"), "{out}");
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_MISS), 1);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_HIT), 1);
}

#[test]
fn failed_sheet_decision_revalidates_past_failure_ttl() {
    let (server, options, fetcher) = fixture_with_fetcher();
    // No fetcher response: the sheet load fails outright.
    let driver = driver_for(&server, &options);
    let html = "<html><head><link rel=\"stylesheet\" href=\"/theme.css\"></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>";
    let first = render_filter(&driver, &html);
    assert!(!first.contains("manrope-400.woff2"), "{first}");
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_MISS), 1);
    // Within the window the partial decision replays.
    let driver = driver_for(&server, &options);
    let second = render_filter(&driver, &html);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_HIT), 1);
    assert!(!second.contains("manrope-400.woff2"), "{second}");
    // The sheet lands; past the 300s default failure TTL plus the
    // retry margin the decision recomputes and really re-fetches.
    fetcher.set_simple_response(
        "https://test.com/theme.css",
        "text/css",
        MANROPE_CSS.as_bytes(),
    );
    server.timer().borrow_mut().advance_ms(400_000);
    let driver = driver_for(&server, &options);
    let third = render_filter(&driver, &html);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_MISS), 2);
    assert!(third.contains("manrope-400.woff2"), "{third}");
}

#[test]
fn pending_sheet_skips_store_and_retries_next_visit() {
    let (server, options, fetcher) = fixture_with_fetcher();
    fetcher.set_waiting(true);
    let driver = driver_for(&server, &options);
    let html = "<html><head><link rel=\"stylesheet\" href=\"/theme.css\"></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>";
    let first = render_filter(&driver, &html);
    assert!(!first.contains("manrope-400.woff2"), "{first}");
    // Still pending: no decision stored, every visit recomputes.
    let driver = driver_for(&server, &options);
    let second = render_filter(&driver, &html);
    assert!(!second.contains("manrope-400.woff2"), "{second}");
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_MISS), 2);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_HIT), 0);
    // The fetch completes; the next visit discovers and stores.
    fetcher.set_simple_response(
        "https://test.com/theme.css",
        "text/css",
        MANROPE_CSS.as_bytes(),
    );
    fetcher.call_callbacks();
    let driver = driver_for(&server, &options);
    let third = render_filter(&driver, &html);
    assert!(third.contains("manrope-400.woff2"), "{third}");
    let driver = driver_for(&server, &options);
    render_filter(&driver, &html);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_HIT), 1);
}

#[test]
fn nested_imports_resolve() {
    let (server, options, fetcher) = fixture_with_fetcher();
    fetcher.set_simple_response(
        "https://test.com/a.css",
        "text/css",
        b"@import url(\"b.css\");\nbody { font-family: Manrope; }",
    );
    fetcher.set_simple_response("https://test.com/b.css", "text/css", MANROPE_CSS.as_bytes());
    let driver = driver_for(&server, &options);
    let html = "<html><head><link rel=\"stylesheet\" href=\"/a.css\"></head><body></body></html>";
    let out = render_filter(&driver, html);
    assert!(out.contains("manrope-400.woff2"), "{out}");
}

#[test]
fn beacon_confirmation_lifts_pruning() {
    use ps_html::font_discovery::FontStyle;
    use ps_html::font_store;
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    // Confirm the icon family via the metadata blob directly.
    let mut confirmed = std::collections::HashSet::new();
    confirmed.insert(("icons".to_string(), 400, FontStyle::Normal));
    let page_url = driver.base_url().spec().to_string();
    let oh = server.hasher().hash(driver.options_signature().as_bytes());
    let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
        driver.request_properties().device_type(),
    );
    server.metadata_cache().put(
        &font_store::confirmed_key(&page_url, &oh, &suffix),
        font_store::encode_confirmed(&confirmed).as_bytes(),
    );
    let css = "@font-face { font-family: Icons; src: url(https://x.test/i.woff2); \
         unicode-range: U+E000-F8FF; }\nbody { font-family: Icons; }";
    let html = format!("<html><head><style>{css}</style></head><body></body></html>");
    let out = render_filter(&driver, &html);
    assert!(out.contains("https://x.test/i.woff2"), "{out}");
    assert_eq!(stat(&server, FONT_PRELOAD_BEACON_CONFIRMED), 1);
    assert_eq!(stat(&server, FONT_PRELOAD_PRELOADED), 1);
}

#[test]
fn chain_plans_slot_when_enabled() {
    let (server, options) = fixture();
    options
        .borrow_mut()
        .filters_mut()
        .enable(ps_config::filter::Filter::PreloadCriticalFonts);
    let plan = ps_html::chain::build_chain(
        &options.borrow(),
        &ps_html::chain::ChainContext::new(true, true),
    );
    assert!(plan.post.contains(&ChainFilter::PreloadCriticalFonts));
    let _ = server;
}

#[test]
fn beacon_js_injected_while_unconfirmed() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let html = format!(
        "<html><head><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let out = render_filter(&driver, &html);
    assert!(out.contains("pagespeed.FontPreloads.Run("), "{out}");
    assert!(out.contains("data-pagespeed-no-defer"), "{out}");
    // A nonce was issued into the page's outstanding set.
    let page_url = driver.base_url().spec().to_string();
    let oh = server.hasher().hash(driver.options_signature().as_bytes());
    let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
        driver.request_properties().device_type(),
    );
    let blob = match server
        .metadata_cache()
        .get(&ps_html::font_store::nonces_key(&page_url, &oh, &suffix))
    {
        ps_cache::cache::Lookup {
            state: ps_cache::cache::KeyState::Available,
            value: Some(blob),
        } => blob,
        _ => panic!("nonces blob missing"),
    };
    let nonces = ps_html::font_store::decode_nonces(&String::from_utf8(blob).unwrap());
    assert_eq!(nonces.pending.len(), 1);
}

#[test]
fn beacon_js_wrapped_inside_script_element() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let html = format!(
        "<html><head><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let out = render_filter(&driver, &html);
    // The beacon payload must sit inside its script element: raw JS
    // after </html> renders as visible page text at the bottom.
    let open = out
        .find("<script data-pagespeed-no-defer")
        .expect("beacon script tag");
    let after_open = &out[open..];
    let run = after_open
        .find("pagespeed.FontPreloads.Run(")
        .expect("beacon run call");
    let close = after_open.find("</script>").expect("script close");
    assert!(run < close, "{out}");
    // The whole script closes before </body>; nothing beacon-shaped
    // leaks past </html>.
    let body_end = out.find("</body>").expect("body close");
    let html_end = out.find("</html>").expect("html close");
    assert!(open + close < body_end && body_end < html_end, "{out}");
    assert!(!out[html_end..].contains("FontPreloads"), "{out}");
    let _ = server;
}

#[test]
fn beacon_js_withheld_once_confirmed() {
    use ps_html::font_discovery::FontStyle;
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let mut confirmed = std::collections::HashSet::new();
    confirmed.insert(("manrope".to_string(), 400, FontStyle::Normal));
    let page_url = driver.base_url().spec().to_string();
    let oh = server.hasher().hash(driver.options_signature().as_bytes());
    let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
        driver.request_properties().device_type(),
    );
    server.metadata_cache().put(
        &ps_html::font_store::confirmed_key(&page_url, &oh, &suffix),
        ps_html::font_store::encode_confirmed(&confirmed).as_bytes(),
    );
    let html = format!(
        "<html><head><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let out = render_filter(&driver, &html);
    assert!(!out.contains("FontPreloads.Run"), "{out}");
}

#[test]
fn decision_hit_keeps_beacon_while_learnable() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let html = format!(
        "<html><head><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let first = render_filter(&driver, &html);
    assert!(first.contains("pagespeed.FontPreloads.Run("), "{first}");
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_MISS), 1);
    // Same inputs: the cached decision replays, but learning is
    // still open so the beacon must ship again.
    let second = render_filter(&driver, &html);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_HIT), 1);
    assert!(second.contains("rel=\"preload\""), "{second}");
    assert!(second.contains("pagespeed.FontPreloads.Run("), "{second}");
}

#[test]
fn decision_hit_stays_quiet_once_learned() {
    use ps_html::font_discovery::FontStyle;
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let mut confirmed = std::collections::HashSet::new();
    confirmed.insert(("manrope".to_string(), 400, FontStyle::Normal));
    let page_url = driver.base_url().spec().to_string();
    let oh = server.hasher().hash(driver.options_signature().as_bytes());
    let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(
        driver.request_properties().device_type(),
    );
    server.metadata_cache().put(
        &ps_html::font_store::confirmed_key(&page_url, &oh, &suffix),
        ps_html::font_store::encode_confirmed(&confirmed).as_bytes(),
    );
    let html = format!(
        "<html><head><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let first = render_filter(&driver, &html);
    assert!(!first.contains("FontPreloads.Run"), "{first}");
    let second = render_filter(&driver, &html);
    assert_eq!(stat(&server, FONT_PRELOAD_CACHE_HIT), 1);
    assert!(second.contains("rel=\"preload\""), "{second}");
    assert!(!second.contains("FontPreloads.Run"), "{second}");
}

/// Real `wp-fonts-local` block from rustpagespeed.com (Twenty
/// Twenty-Five): variable Manrope + Fira Code, weight ranges.
const TT5_FONTS_LOCAL: &str = "@font-face{font-family:Manrope;font-style:normal;font-weight:200 800;font-display:fallback;src:url(https://rustpagespeed.com/wp-content/themes/twentytwentyfive/assets/fonts/manrope/Manrope-VariableFont_wght.woff2) format('woff2')}@font-face{font-family:\"Fira Code\";font-style:normal;font-weight:300 700;font-display:fallback;src:url(https://rustpagespeed.com/wp-content/themes/twentytwentyfive/assets/fonts/fira-code/FiraCode-VariableFont_wght.woff2) format('woff2')}";

/// Trimmed `global-styles-inline-css` from the same page: preset
/// families, `var()`-wired body, per-family utility classes.
const TT5_GLOBAL: &str = ":root{--wp--preset--font-family--manrope:Manrope , sans-serif;--wp--preset--font-family--fira-code:\"Fira Code\" , monospace;}body{font-family: var(--wp--preset--font-family--manrope);font-weight:300;}.has-manrope-font-family{font-family: var(--wp--preset--font-family--manrope) !important;}.has-fira-code-font-family{font-family: var(--wp--preset--font-family--fira-code) !important;}";

const TT5_DOM: &str = "<header><nav><a>Home</a></nav><p class=\"site-title\">Rust PageSpeed</p></header><main><h1>Hello</h1><p>Body copy.</p></main>";

#[test]
fn twentytwentyfive_preloads_manrope_excludes_fira_code() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let html = format!(
        "<html><head><style class=\"wp-fonts-local\">{TT5_FONTS_LOCAL}</style>\
         <style id=\"global-styles-inline-css\">{TT5_GLOBAL}</style></head>\
         <body>{TT5_DOM}</body></html>"
    );
    let out = render_filter(&driver, &html);
    assert_eq!(out.matches("rel=\"preload\"").count(), 1, "{out}");
    assert!(out.contains("as=\"font\""), "{out}");
    assert!(out.contains("Manrope-VariableFont_wght.woff2"), "{out}");
    assert!(out.contains("type=\"font/woff2\""), "{out}");
    assert!(out.contains("crossorigin=\"anonymous\""), "{out}");
    // Fira Code stays declared-only: its utility class matches no
    // element, so no second preload is emitted for it.
    assert!(
        !out.contains("as=\"font\" type=\"font/woff2\" href=\"https://rustpagespeed.com/wp-content/themes/twentytwentyfive/assets/fonts/fira-code"),
        "{out}"
    );
}

#[test]
fn twentytwentyfive_code_block_lifts_fira_code() {
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    // A code block carries the Fira utility class: both families are
    // used, so both preload (the cap).
    let html = format!(
        "<html><head><style class=\"wp-fonts-local\">{TT5_FONTS_LOCAL}</style>\
         <style id=\"global-styles-inline-css\">{TT5_GLOBAL}</style></head>\
         <body>{TT5_DOM}<pre class=\"has-fira-code-font-family\">code</pre></body></html>"
    );
    let out = render_filter(&driver, &html);
    assert_eq!(out.matches("rel=\"preload\"").count(), 2, "{out}");
    assert!(out.contains("Manrope-VariableFont_wght.woff2"), "{out}");
    assert!(
        out.contains("as=\"font\" type=\"font/woff2\" href=\"https://rustpagespeed.com/wp-content/themes/twentytwentyfive/assets/fonts/fira-code/FiraCode-VariableFont_wght.woff2\""),
        "{out}"
    );
    assert_eq!(stat(&server, FONT_PRELOAD_PRELOADED), 2);
}

#[test]
fn debug_comment_explains_selection_and_rejection() {
    let (server, options) = fixture();
    options
        .borrow_mut()
        .filters_mut()
        .enable(ps_config::filter::Filter::Debug);
    let driver = driver_for(&server, &options);
    let html = format!(
        "<html><head><style class=\"wp-fonts-local\">{TT5_FONTS_LOCAL}</style>\
         <style id=\"global-styles-inline-css\">{TT5_GLOBAL}</style></head>\
         <body>{TT5_DOM}</body></html>"
    );
    let out = render_filter(&driver, &html);
    assert!(out.contains("<!--font-preloads:"), "{out}");
    assert!(out.contains("manrope=selected(page-wide rule"), "{out}");
    assert!(out.contains("overlaps face 200-800"), "{out}");
    assert!(
        out.contains("fira code=rejected(no matching usage)"),
        "{out}"
    );
    // No Debug filter: no comment.
    let (server, options) = fixture();
    let driver = driver_for(&server, &options);
    let out = render_filter(&driver, &html);
    assert!(!out.contains("font-preloads:"), "{out}");
}

#[test]
fn driver_run_emits_through_chain() {
    let (server, options) = fixture();
    options
        .borrow_mut()
        .filters_mut()
        .enable(ps_config::filter::Filter::PreloadCriticalFonts);
    let html = format!(
        "<html><head><style>{MANROPE_CSS}</style></head>\
         <body><p style=\"font-family: Manrope\">hi</p></body></html>"
    );
    let sink = harness::SharedSink::default();
    let mut driver = Driver::new(
        &options.borrow(),
        &ChainContext::new(true, true),
        sink.clone(),
        Some(driver_for(&server, &options)),
    );
    driver.start_parse(PAGE.as_bytes(), false);
    driver.write(html.as_bytes());
    driver.finish();
    let out = String::from_utf8(sink.bytes()).unwrap();
    assert!(out.contains("rel=\"preload\""), "{out}");
    assert!(out.contains("manrope-400.woff2"), "{out}");
}
