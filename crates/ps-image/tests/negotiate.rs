//! Negotiation tests: Accept parsing, UA vectors ported from the C++ matcher
//! tests, capability-list gates, libwebp level selection, and the mapping
//! into compression options.

use ps_image::image::{CompressionOptions, PreferredWebp};
use ps_image::negotiate::{
    accepts_image_format, apply_webp_level, device_support, explicitly_rejects_image_format, is_ie,
    is_known_avif_unsupported, legacy_webp, request_support, select_libwebp_level,
    ua_supports_avif, ua_supports_webp, ua_supports_webp_animated, ua_supports_webp_lossless_alpha,
    CapabilityList, LibWebpLevel, WebpFilterFlags,
};

#[test]
fn accept_matrix() {
    // Present with positive quality (explicit or default 1.0).
    assert!(accepts_image_format(
        &["text/html, image/webp;q=0.8"],
        "image/webp"
    ));
    assert!(accepts_image_format(
        &["text/html, image/avif"],
        "image/avif"
    ));
    // Multiple header values are all consulted.
    assert!(accepts_image_format(
        &["text/html", "image/webp"],
        "image/webp"
    ));
    // Zero quality is not acceptance; it is explicit rejection.
    assert!(!accepts_image_format(&["image/webp;q=0"], "image/webp"));
    assert!(explicitly_rejects_image_format(
        &["image/webp;q=0"],
        "image/webp"
    ));
    // Absent MIME: neither accepted nor rejected.
    assert!(!accepts_image_format(&["text/html"], "image/webp"));
    assert!(!explicitly_rejects_image_format(
        &["text/html"],
        "image/webp"
    ));
    // Last q parameter wins, like the overwriting loop.
    assert!(!accepts_image_format(&["image/webp;q=1;q=0"], "image/webp"));
    assert!(accepts_image_format(&["image/webp;q=0;q=1"], "image/webp"));
}

const CHROME_120: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const CHROME_32: &str =
    "Mozilla/5.0 (Windows NT 6.2; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/32.0.1667.0 Safari/537.36";
const FIREFOX_92: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:92.0) Gecko/20100101 Firefox/92.0";

#[test]
fn webp_ua_vectors() {
    // Firefox boundary at 65.
    assert!(!ua_supports_webp(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:64.0) Gecko/20100101 Firefox/64.0"
    ));
    assert!(ua_supports_webp(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:65.0) Gecko/20100101 Firefox/65.0"
    ));
    // Chrome boundary at 32.
    assert!(ua_supports_webp(CHROME_120));
    assert!(!ua_supports_webp(
        "Mozilla/5.0 (Windows NT 6.1) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/31.0 Safari/537.36"
    ));
    // Legacy Android browser without Accept.
    assert!(legacy_webp(
        "Mozilla/5.0 (Linux; U; Android 4.0.1; en-us; Galaxy Nexus Build/ICL27) AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Mobile Safari/534.30"
    ));
    assert!(!legacy_webp(
        "Mozilla/5.0 (Linux; U; Android 3.2; en-us; Sony Tablet S Build/THMAS11000) AppleWebKit/534.13 (KHTML, like Gecko) Version/4.0 Safari/534.13"
    ));
    assert!(!legacy_webp(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_6_4) AppleWebKit/534.30 (KHTML, like Gecko) Chrome/12.0.742.100 Safari/534.30"
    ));
    // Lossless/animated need modern Chrome (32+) or allowlisted test UAs.
    assert!(ua_supports_webp_lossless_alpha(CHROME_120));
    assert!(ua_supports_webp_animated(
        "Mozilla/5.0 (Windows NT 6.2; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/32.0.1667.0 Safari/537.36"
    ));
    assert!(!ua_supports_webp_animated(
        "Mozilla/5.0 (Windows NT 5.1) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/31.0.1650.16 Safari/537.36"
    ));
    // Faithful C++ table gap: the animated allowlist has no 3-digit Chrome
    // pattern (unlike the lossless table), so Chrome 100+ matches nothing
    // and reports false. (Found inconsistency, reported.)
    assert!(!ua_supports_webp_animated(CHROME_120));
    assert!(ua_supports_webp_lossless_alpha("webp-la"));
    // The lossless table is pure wildcards: Firefox 65+ matches the allow
    // rule with no block rule above 64, so modern Firefox reports true.
    assert!(ua_supports_webp_lossless_alpha(FIREFOX_92));
    assert!(!ua_supports_webp_lossless_alpha(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:64.0) Gecko/20100101 Firefox/64.0"
    ));
}

#[test]
fn avif_ua_vectors() {
    // Boundaries ported from the C++ AVIF test table.
    let supported = [
        "Mozilla/5.0 Chrome/85.0.0.0 Safari/537.36",
        "Mozilla/5.0 Chrome/121.0.0.0 Safari/537.36 Edg/121.0.0.0",
        "Mozilla/5.0 Firefox/93.0",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0) Version/16.4 Safari/605.1",
        "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) Version/16.0 Mobile/15E148 Safari/604.1",
        "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 SamsungBrowser/14.0",
        "Mozilla/5.0 Chrome/121.0.0.0 Safari/537.36 EdgA/121.0.0.0",
        "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 OPR/71.0.0.0",
        "Mozilla/5.0 (Linux; Android 10) UCBrowser/15.5.0.0 Chrome/120.0",
        "Mozilla/5.0 BaiduBrowser/13.52 Chrome/120.0.0.0",
        "Opera/9.80 (Android; Opera Mobi/80.0; U; en)",
    ];
    for ua in supported {
        assert!(ua_supports_avif(ua), "{ua}");
        assert!(!is_known_avif_unsupported(ua), "{ua}");
    }
    let unsupported = [
        "Mozilla/5.0 Chrome/84.0.0.0 Safari/537.36",
        "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
        "Mozilla/5.0 Firefox/92.0",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0) Version/16.3 Safari/605.1",
        "Mozilla/5.0 (iPhone; CPU iPhone OS 15_8 like Mac OS X) Version/15.8 Mobile/15E148 Safari/604.1",
        "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 SamsungBrowser/13.0",
        "Opera Mini/50.0",
        "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/120.0 Safari/537.36 Edge/18.19041",
        "Mozilla/5.0 (Linux; U; Android 4.4.4) Version/4.0 Safari/537.36",
        "Mozilla/5.0 (Mobile; rv:3.0) Gecko/3.0 Firefox/3.0 KaiOS/3.0",
        "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 EdgA/120.0.0.0",
        "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 OPR/70.0.0.0",
        "Mozilla/5.0 (Linux; Android 10) UCBrowser/15.4.0.0 Chrome/120.0",
        "Mozilla/5.0 BaiduBrowser/13.51 Chrome/120.0.0.0",
        "Mozilla/5.0 (Windows NT 10.0; Trident/7.0; rv:11.0) like Gecko",
        "Opera/9.80 (Android; Opera Mobi/12.1; U; en)",
        "Mozilla/5.0 (Linux; U; Android 2.1) Version/4.0 Mobile Safari/530.17",
    ];
    for ua in unsupported {
        assert!(!ua_supports_avif(ua), "{ua}");
    }
    // QQBrowser/14.9 rides on Chrome/120: the Chrome branch fires before the
    // QQBrowser carve-out, so the C++ reports SUPPORTED here — verified by
    // running pagespeed_automatic_test, whose AvifBrowserVersionBoundaries
    // case FAILS on this exact vector (stale expectation). This port matches
    // the executed behavior, not the stale test.
    assert!(ua_supports_avif(
        "Mozilla/5.0 QQBrowser/14.9 Chrome/120.0.0.0 Safari/537.36"
    ));
    // Empty UA is never "known unsupported".
    assert!(!is_known_avif_unsupported(""));
    assert!(is_ie(
        "Mozilla/5.0 (compatible; MSIE 9.0; Windows NT 6.1; Trident/5.0)"
    ));
    assert!(!is_ie(CHROME_120));
}

#[test]
fn device_combines_accept_and_ua() {
    let modern_chrome = CHROME_120;
    // Accept alone suffices for webp URLs.
    let support = device_support(&["image/webp"], "SomeUnknownBrowser/1.0");
    assert!(support.webp_rewritten_urls);
    assert!(!support.avif_rewritten_urls);
    // UA alone suffices when the UA matcher knows it.
    let support = device_support(&["text/html"], modern_chrome);
    assert!(support.webp_rewritten_urls);
    // Explicit rejection vetoes even a capable UA.
    let support = device_support(&["image/webp;q=0"], modern_chrome);
    assert!(!support.webp_rewritten_urls);
    // Lossless/animated need BOTH Accept and UA (Chrome 32 predates the
    // 3-digit animated-table gap, so it exercises the true path).
    let support = device_support(&["image/webp"], CHROME_32);
    assert!(support.webp_lossless_alpha && support.webp_animated);
    let support = device_support(&["text/html"], CHROME_32);
    assert!(!support.webp_lossless_alpha && !support.webp_animated);
    // AVIF via Accept on an unknown UA.
    let support = device_support(&["image/avif"], "SomeUnknownBrowser/1.0");
    assert!(support.avif_rewritten_urls);
    // ... but not on a known-unsupported UA.
    let support = device_support(&["image/avif"], "Opera Mini/50.0");
    assert!(!support.avif_rewritten_urls);
}

#[test]
fn capability_list_gates_webp_only() {
    let full = device_support(&["image/webp"], CHROME_32);
    // Absent header: everything passes through.
    let support = request_support(full, &CapabilityList::absent());
    assert!(support.webp_rewritten_urls && support.webp_lossless_alpha && support.webp_animated);
    // Downstream without webp tokens: webp off, AVIF unaffected.
    let full_avif = device_support(&["image/webp,image/avif"], CHROME_120);
    let support = request_support(full_avif, &CapabilityList::parse("ii,di"));
    assert!(!support.webp_rewritten_urls);
    assert!(support.avif_rewritten_urls);
    // Partial tokens gate individually.
    let support = request_support(full, &CapabilityList::parse("jw,ws,xx"));
    assert!(support.webp_rewritten_urls && support.webp_lossless_alpha);
    assert!(!support.webp_animated);
    // C++ matching quirk, ported exactly: a trailing bare token never
    // matches (no exact/prefix/infix form fires), so "jw,ws" gates lossless.
    let support = request_support(full, &CapabilityList::parse("jw,ws"));
    assert!(support.webp_rewritten_urls);
    assert!(!support.webp_lossless_alpha);
}

#[test]
fn level_selection_matrix() {
    let all = WebpFilterFlags {
        recompress_webp: true,
        convert_to_webp_animated: true,
        convert_to_webp_lossless: true,
        convert_jpeg_to_webp: true,
    };
    let animated = ps_image::negotiate::RequestSupport {
        webp_rewritten_urls: true,
        avif_rewritten_urls: true,
        webp_lossless_alpha: true,
        webp_animated: true,
    };
    assert_eq!(select_libwebp_level(animated, all), LibWebpLevel::Animated);
    // Animated support without animated/recompress filters falls through.
    let jpeg_only = WebpFilterFlags {
        convert_jpeg_to_webp: true,
        ..Default::default()
    };
    assert_eq!(
        select_libwebp_level(animated, jpeg_only),
        LibWebpLevel::LossyOnly
    );
    // No support at all: none even with all filters.
    let none = ps_image::negotiate::RequestSupport {
        webp_rewritten_urls: false,
        avif_rewritten_urls: false,
        webp_lossless_alpha: false,
        webp_animated: false,
    };
    assert_eq!(select_libwebp_level(none, all), LibWebpLevel::None);
    // No filters: none even with full support.
    assert_eq!(
        select_libwebp_level(animated, WebpFilterFlags::default()),
        LibWebpLevel::None
    );
}

#[test]
fn level_mapping_into_options() {
    // None: everything off.
    let mut options = CompressionOptions::default();
    apply_webp_level(LibWebpLevel::None, true, true, &mut options);
    assert_eq!(options.preferred_webp, PreferredWebp::None);
    assert!(!options.allow_webp_alpha);
    // Lossy: lossy preferred, no alpha.
    let mut options = CompressionOptions::default();
    apply_webp_level(LibWebpLevel::LossyOnly, true, true, &mut options);
    assert_eq!(options.preferred_webp, PreferredWebp::Lossy);
    assert!(!options.allow_webp_alpha);
    // Animated with the filter: animated + alpha.
    let mut options = CompressionOptions::default();
    apply_webp_level(LibWebpLevel::Animated, true, false, &mut options);
    assert_eq!(options.preferred_webp, PreferredWebp::Animated);
    assert!(options.allow_webp_animated && options.allow_webp_alpha);
    // Animated without the filter: falls through to lossy/lossless choice.
    let mut options = CompressionOptions::default();
    apply_webp_level(LibWebpLevel::Animated, false, true, &mut options);
    assert_eq!(options.preferred_webp, PreferredWebp::Lossless);
    assert!(options.allow_webp_alpha && !options.allow_webp_animated);
    let mut options = CompressionOptions::default();
    apply_webp_level(LibWebpLevel::LossyLosslessAlpha, false, false, &mut options);
    assert_eq!(options.preferred_webp, PreferredWebp::Lossy);
    assert!(options.allow_webp_alpha);
}
