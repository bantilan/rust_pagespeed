//! Deterministic fixture vectors, one family per crate.
//!
//! Each vector is data only; the Phase 4 suite evaluates it
//! through [`crate::harness`]. The unit tests below pin every
//! entry against the live crates so the vectors cannot drift.
//!
//! Provenance: every expected value is transcribed from the C++
//! ground truth (source named per vector) or mirrored from a
//! C++-ported suite in the owning crate. Novel pairs are fixed
//! points or whitespace collapses verifiable by inspection.

// -- ps-config: directives, levels, filters, client options --

/// Config directive vectors: (directive, value, parses).
///
/// Value grammar from `rewrite_options.cc` (`SetFromString`
/// family); the `ModPagespeed` prefix strip and the rename map
/// come from `property.rs` (`LookupOptionByName`).
#[must_use]
pub fn config_parse_vectors() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        ("RewriteLevel", "CoreFilters", true),
        ("RewriteLevel", "corefilters", true),
        ("RewriteLevel", "BogusLevel", false),
        ("ModPagespeedRewriteLevel", "CoreFilters", true),
        ("ImageWebpRecompressionQuality", "75", true),
        ("EnableCachePurge", "on", true),
        ("EnableCachePurge", "off", true),
        ("EnableCachePurge", "maybe", false),
        ("HtmlCache", "true", true),
        ("HtmlCacheTtlSec", "60", true),
        ("HtmlCacheTtlSec", "-1", true),
        ("HtmlCacheTtlSec", "sixty", false),
        ("NoSuchDirective", "x", false),
        ("ModPagespeed", "x", false),
    ]
}

/// Rewrite-level spelling vectors: (spelling, parses).
///
/// Canonical names from `RewriteLevel::parse`
/// (`ParseRewriteLevel`, ASCII case-insensitive).
#[must_use]
pub fn rewrite_level_vectors() -> Vec<(&'static str, bool)> {
    vec![
        ("PassThrough", true),
        ("OptimizeForBandwidth", true),
        ("CoreFilters", true),
        ("MobilizeFilters", true),
        ("TestingCoreFilters", true),
        ("AllFilters", true),
        ("corefilters", true),
        ("BogusLevel", false),
        ("", false),
    ]
}

/// Filter identity vectors: (URL id, public name).
///
/// Ids from `rewrite_options.cc` (`kFilterVectorStaticInitializer`,
/// all 95 diffed byte-identical); names from
/// `rewrite_filter_names.gperf`. The round-trip test also sweeps
/// all 95 filters programmatically.
#[must_use]
pub fn filter_identity_vectors() -> Vec<(&'static str, &'static str)> {
    vec![
        ("cw", "collapse_whitespace"),
        ("cc", "combine_css"),
        ("jc", "combine_javascript"),
        ("ea", "elide_attributes"),
        ("ci", "inline_css"),
        ("ji", "inline_javascript"),
        ("rj", "recompress_jpeg"),
        ("rq", "remove_quotes"),
        ("cf", "rewrite_css"),
        ("jm", "rewrite_javascript_external"),
        ("is", "sprite_images"),
    ]
}

/// Client-options vectors: (header, parses, disables all filters,
/// disables image filters).
///
/// Grammar from `rewrite_query.cc` (`ParseClientOptions`,
/// version-gated, unknown pairs ignored); application from
/// `UpdateRewriteOptionsWithClientOptions`.
#[must_use]
pub fn client_options_vectors() -> Vec<(&'static str, bool, bool, bool)> {
    vec![
        ("v=1,m=2", true, true, true),
        ("v=1,m=1", true, false, true),
        ("v=1,m=0", true, false, false),
        ("v=1,iqp=2,m=0", true, false, false),
        ("m=0,iqp=3,v=1,xyz=100,zyx=,yzx", true, false, false),
        ("iqp=2,m=1,v=2", false, false, false),
        ("iqp=2,m=1", false, false, false),
        ("v=1,v=1,m=0", false, false, false),
    ]
}

// -- ps-cache: segments, namer --

/// Cache-key vectors: resource names whose encoded segments
/// must round-trip byte-identically (`EncodeToUrlSegment`).
#[must_use]
pub fn cache_key_vectors() -> Vec<&'static str> {
    vec![
        "styles/site.css",
        "images/a b.png",
        "a+b/c.d.js",
        "100%.css",
        "q?.css",
        "a.pagespeed.b",
        "options/with/slashes",
    ]
}

/// Resource-namer vectors: (encoded URL, name, id, ext, options,
/// experiment).
///
/// Audited shapes from the `ResourceNamer` goldens
/// (experiment, options incl. slash escapes, cache-extender).
#[must_use]
pub fn resource_namer_vectors(
) -> Vec<(&'static str, &'static str, &'static str, &'static str, &'static str, &'static str)> {
    vec![
        (
            "photo.jpg.pagespeed.q.ic.ABCDEF1234.jpg",
            "photo.jpg",
            "ic",
            "jpg",
            "",
            "q",
        ),
        (
            "a.css.pagespeed.a=b.cf.HASH1234.css",
            "a.css",
            "cf",
            "css",
            "a=b",
            "",
        ),
        (
            "n.css.pagespeed.options,_with,_slashes.cf.HASH.css",
            "n.css",
            "cf",
            "css",
            "options/with/slashes",
            "",
        ),
        (
            "x.html.pagespeed.ce.HASHVALUE1.html",
            "x.html",
            "ce",
            "html",
            "",
            "",
        ),
    ]
}

// -- ps-http: URLs --

/// URL resolution vectors: (base, reference, resolved spec).
///
/// WHATWG `url` behavior through the `GoogleUrl` facade;
/// exotic-input deviations from Chromium GURL are documented
/// in `ps-http/src/google_url.rs`.
#[must_use]
pub fn url_resolve_vectors() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        (
            "https://pagespeed.invalid/",
            "/polyfills.min.js",
            "https://pagespeed.invalid/polyfills.min.js",
        ),
        (
            "https://pagespeed.invalid/",
            "https://polyfill.io/v3/polyfill.min.js?f=default",
            "https://polyfill.io/v3/polyfill.min.js?f=default",
        ),
        (
            "http://test.com/a/b/c.html",
            "../d.html",
            "http://test.com/a/d.html",
        ),
    ]
}

/// URL validity vectors: (spec, is valid, is web-valid).
#[must_use]
pub fn url_validity_vectors() -> Vec<(&'static str, bool, bool)> {
    vec![
        ("https://h/p/a.js", true, true),
        ("http://h:8080/p", true, true),
        ("data:text/plain,hi", true, false),
        ("javascript:alert(1)", true, false),
        ("not a url", false, false),
        ("", false, false),
        ("bogus", false, false),
    ]
}

// -- ps-image: negotiation --

/// Accept-header vectors: (header, MIME type, accepts, rejects).
///
/// Quality grammar from `device_properties.cc`: accepts takes the
/// last q (`q=0;q=1` accepts) while rejects fires on ANY zero q
/// (`q=0;q=1` also rejects); absent MIME is neutral.
#[must_use]
pub fn accept_vectors() -> Vec<(&'static str, &'static str, bool, bool)> {
    vec![
        ("text/html, image/webp;q=0.8", "image/webp", true, false),
        ("text/html, image/avif", "image/avif", true, false),
        ("image/webp;q=0", "image/webp", false, true),
        ("text/html", "image/webp", false, false),
        ("image/webp;q=1;q=0", "image/webp", false, true),
        ("image/webp;q=0;q=1", "image/webp", true, true),
    ]
}

/// WebP user-agent vectors: (user agent, supports WebP).
///
/// Boundaries from the C++ user-agent matcher tests.
#[must_use]
pub fn ua_webp_vectors() -> Vec<(&'static str, bool)> {
    vec![
        (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            true,
        ),
        (
            "Mozilla/5.0 (Windows NT 6.1) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/31.0 Safari/537.36",
            false,
        ),
        (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:64.0) Gecko/20100101 Firefox/64.0",
            false,
        ),
        (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:65.0) Gecko/20100101 Firefox/65.0",
            true,
        ),
    ]
}

/// AVIF user-agent vectors: (user agent, supports AVIF).
///
/// Boundaries from the C++ AVIF test table.
#[must_use]
pub fn ua_avif_vectors() -> Vec<(&'static str, bool)> {
    vec![
        ("Mozilla/5.0 Chrome/85.0.0.0 Safari/537.36", true),
        ("Mozilla/5.0 Chrome/84.0.0.0 Safari/537.36", false),
        ("Mozilla/5.0 Firefox/93.0", true),
        ("Mozilla/5.0 Firefox/92.0", false),
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0) Version/16.4 Safari/605.1",
            true,
        ),
        (
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0) Version/16.3 Safari/605.1",
            false,
        ),
    ]
}

/// Capability-list vectors: (header, webp, lossless-alpha, animated).
///
/// `PS-CapabilityList` gating over a WebP-capable Chrome 32
/// device; the trailing-bare-token quirk (`jw,ws` gates
/// lossless) is ported exactly from the C++ matcher.
#[must_use]
pub fn capability_vectors() -> Vec<(&'static str, bool, bool, bool)> {
    vec![
        ("ii,di", false, false, false),
        ("jw,ws,xx", true, true, false),
        ("jw,ws", true, false, false),
    ]
}

/// Variant-cache-key vectors: (libwebp level 0-3, avif, mobile,
/// save-data, small-screen, expected key).
///
/// Levels: 0 none (`.`), 1 lossy (`w`), 2 lossless-alpha (`v`),
/// 3 animated (`a`); Save-Data wins over small-screen
/// (`CacheKeyFromResourceContext`).
#[must_use]
pub fn variant_key_vectors() -> Vec<(u8, bool, bool, bool, bool, &'static str)> {
    vec![
        (0, false, false, false, false, "."),
        (1, true, true, false, true, "w.avifmss"),
        (0, false, false, true, true, ".d"),
        (3, false, false, false, false, "a"),
    ]
}

// -- ps-css / ps-js: golden pairs --

/// CSS golden vectors: (input, minified output).
///
/// Fixed points and whitespace collapses through `minify_css`
/// (`RewriteCss` shape, fail-closed); outputs verified against
/// the minification rules by inspection.
#[must_use]
pub fn css_golden_vectors() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a{color:red}", "a{color:red}"),
        ("a { color : red ; }", "a{color:red}"),
        ("a{margin:0px}", "a{margin:0}"),
    ]
}

/// CSS inputs `minify_css` must reject (fail closed).
///
/// `{` is the ported `CssMinifyTest.Invalid` input; the unclosed
/// rule fails the same `parse_stylesheet` gate.
#[must_use]
pub fn css_reject_vectors() -> Vec<&'static str> {
    vec!["a{color:red", "{"]
}

/// JS golden vectors: (input, minified output).
///
/// Pairs from the ported `js_minify_test.cc` corpus; both the
/// tokenizer minifier and the legacy JSMin port must agree.
#[must_use]
pub fn js_golden_vectors() -> Vec<(&'static str, &'static str)> {
    vec![
        ("var x = 1;\r\nvar y = 2;", "var x=1;var y=2;"),
        (
            "{  \"foo\": {\"bar\": 1}, \"baz\": 2 } ",
            "{\"foo\":{\"bar\":1},\"baz\":2}",
        ),
        ("", ""),
    ]
}

/// JS inputs both minifiers must reject (fail closed).
#[must_use]
pub fn js_reject_vectors() -> Vec<&'static str> {
    vec![
        "/* not valid javascript",
        "\"not valid javascript",
        "/not_valid_javascript",
    ]
}

// -- ps-admin: routes --

/// Admin route vectors: (path, query, global, status).
///
/// All 14 `AdminSite` leaves over an empty stats snapshot plus
/// the redirect, the plain 404, and the purge auth gate.
#[must_use]
pub fn admin_route_vectors() -> Vec<(&'static str, &'static str, bool, i32)> {
    vec![
        ("/pagespeed_admin/dashboard", "", false, 200),
        ("/pagespeed_admin/history", "", false, 200),
        ("/pagespeed_admin/images", "", false, 200),
        ("/pagespeed_admin/html_cache", "", false, 200),
        ("/pagespeed_admin/javascript", "", false, 200),
        ("/pagespeed_admin/css", "", false, 200),
        ("/pagespeed_admin/statistics", "", false, 200),
        ("/pagespeed_admin/stats_json", "", false, 200),
        ("/pagespeed_admin/graphs", "", false, 200),
        ("/pagespeed_admin/config", "", false, 200),
        ("/pagespeed_admin/console", "", false, 200),
        ("/pagespeed_admin/message_history", "", false, 200),
        ("/pagespeed_admin/cache", "", false, 200),
        ("/pagespeed_admin/histograms", "", false, 200),
        ("/pagespeed_admin/cache", "purge=*", true, 200),
        ("/pagespeed_admin/cache", "purge=*", false, 403),
        ("/pagespeed_admin/nope", "", false, 404),
        ("/pagespeed_admin", "", false, 301),
    ]
}

// -- ps-stats: names --

/// Statistics-name vectors every build must register.
///
/// One pin per family: HTTP cache, HTML cache, rewrite stats,
/// image, CSS, JavaScript, backend prefixes, fetchers, IPRO,
/// histograms, and timed variables.
#[must_use]
pub fn stat_name_vectors() -> Vec<&'static str> {
    vec![
        "cache_hits",
        "cache_misses",
        "cache_time_us",
        "html_cache_hits",
        "html_cache_misses",
        "html_cache_inserts",
        "html_cache_bytes_served",
        "resource_404_count",
        "num_flushes",
        "ipro_served",
        "image_rewrites",
        "image_webp_rewrites",
        "image_rewrite_total_bytes_saved",
        "css_filter_blocks_rewritten",
        "css_filter_total_bytes_saved",
        "javascript_blocks_minified",
        "javascript_total_bytes_saved",
        "file_cache_hits",
        "lru_cache_inserts",
        "serf_fetch_request_count",
    ]
}

// -- ps-html: golden pairs --

/// HTML golden pairs: (filters, input, output).
///
/// Driver-level rewrites mirrored from the C++-ported
/// per-filter suites.
#[must_use]
pub fn html_golden_vectors() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("", "<p>a</p>", "<p>a</p>"),
        ("remove_comments", "<!-- x --><p>a</p>", "<p>a</p>"),
        ("collapse_whitespace", "<p>a  b</p>", "<p>a b</p>"),
        (
            "elide_attributes",
            "<head></head><body><form method=get></form></body>",
            "<head></head><body><form></form></body>",
        ),
        (
            "remove_quotes",
            "<div class=\"foo\">x</div>",
            "<div class=foo>x</div>",
        ),
    ]
}

// -- ps-nginx: pure helpers --

/// In-place URL agreement vectors: (server URI, server host,
/// module path, module host, merge slashes, agrees).
///
/// `NgxInPlaceUrlAgrees`: decoded paths compared byte for byte,
/// hosts case-insensitively.
#[must_use]
pub fn nginx_agreement_vectors() -> Vec<(&'static str, &'static str, &'static str, &'static str, bool, bool)> {
    vec![
        ("/", "h", "/", "h", true, true),
        ("/a+b/c.d.js", "h", "/a+b/c.d.js", "h", true, true),
        ("/100%.css", "h", "/100%25.css", "h", true, true),
        ("/q?.css", "h", "/q%3F.css", "h", true, true),
        ("/a.css", "h", "/b.css", "h", true, false),
        ("/a.css", "h", "/a.css", "other", true, false),
        ("/a.css", "EXAMPLE.com", "/a.css", "example.COM", true, true),
        ("/a//b.css", "h", "/a//b.css", "h", false, true),
    ]
}

/// Response-body vectors: (status, head request, has body).
///
/// HEAD, 204, and 304 responses carry no body.
#[must_use]
pub fn response_body_vectors() -> Vec<(i32, bool, bool)> {
    vec![
        (200, false, true),
        (404, false, true),
        (204, false, false),
        (304, false, false),
        (200, true, false),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps_cache::namer::{decode_url_segment, encode_url_segment, ResourceNamer};
    use ps_config::client_options::parse_client_options;
    use ps_config::filter::{Filter, RewriteLevel, NUM_FILTERS};
    use ps_config::options::{QueryStatus, RewriteOptions, SetOutcome};
    use ps_config::property::PROPERTIES;
    use ps_config::scope::Scope;
    use ps_html::chain::ChainContext;
    use ps_html::driver::Driver;
    use ps_html::writer::ByteSink;
    use ps_stats::core::Statistics;
    use ps_stats::names::init_all;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;

    #[test]
    fn config_vectors_match_the_parser() {
        for (name, value, parses) in config_parse_vectors() {
            let mut options = RewriteOptions::new();
            // Widest scope: the vectors pin value parsing, not scope gates.
            let outcome = options
                .set_option(name, value, Scope::ProcessStrict)
                .is_ok();
            assert_eq!(parses, outcome, "{name}={value}");
        }
    }

    #[test]
    fn rewrite_levels_parse_canonically() {
        for (spelling, parses) in rewrite_level_vectors() {
            assert_eq!(parses, RewriteLevel::parse(spelling).is_some(), "{spelling}");
        }
        // Canonical names round-trip.
        for level in [
            RewriteLevel::PassThrough,
            RewriteLevel::OptimizeForBandwidth,
            RewriteLevel::CoreFilters,
            RewriteLevel::MobilizeFilters,
            RewriteLevel::TestingCoreFilters,
            RewriteLevel::AllFilters,
        ] {
            assert_eq!(Some(level), RewriteLevel::parse(level.name()));
        }
    }

    #[test]
    fn filter_identities_round_trip() {
        for (id, name) in filter_identity_vectors() {
            let by_id = Filter::from_id(id).expect(id);
            let by_name = Filter::from_name(name).expect(name);
            assert_eq!(by_id, by_name, "{id}/{name}");
            assert_eq!(id, by_name.id(), "{name}");
        }
    }

    #[test]
    fn all_filter_ids_are_unique_and_round_trip() {
        assert_eq!(95, NUM_FILTERS);
        assert_eq!(95, Filter::ALL.len());
        let mut seen = HashSet::new();
        for filter in Filter::ALL {
            assert!(seen.insert(filter.id()), "duplicate id {}", filter.id());
            assert_eq!(Some(filter), Filter::from_id(filter.id()));
            assert!(!filter.display_name().is_empty());
        }
    }

    #[test]
    fn client_options_vectors_apply() {
        for (header, parses, kills_all, kills_images) in client_options_vectors() {
            assert_eq!(
                parses,
                parse_client_options(Some(header)).is_some(),
                "{header}"
            );
            let mut options = RewriteOptions::new();
            options.filters_mut().set_level(RewriteLevel::CoreFilters);
            assert!(options.enabled(Filter::RewriteCss), "core enables css");
            assert!(
                options.update_with_client_options(Some(header)) == (kills_all || kills_images),
                "{header}"
            );
            assert_eq!(!kills_all, options.enabled(Filter::RewriteCss), "{header}");
            assert_eq!(
                !kills_images,
                options.enabled(Filter::ConvertGifToPng),
                "{header}"
            );
        }
        assert_eq!(None, parse_client_options(None));
    }

    #[test]
    fn cache_key_vectors_round_trip() {
        for key in cache_key_vectors() {
            let encoded = encode_url_segment(key);
            assert_eq!(Some(key.to_string()), decode_url_segment(&encoded), "{key}");
        }
    }

    #[test]
    fn resource_namer_vectors_decode() {
        for (encoded, name, id, ext, options, experiment) in resource_namer_vectors() {
            let mut namer = ResourceNamer::new();
            assert!(namer.decode_ignore_hash_and_signature(encoded), "{encoded}");
            assert_eq!(name, namer.name(), "{encoded}");
            assert_eq!(id, namer.id(), "{encoded}");
            assert_eq!(ext, namer.ext(), "{encoded}");
            assert_eq!(options, namer.options(), "{encoded}");
            assert_eq!(experiment, namer.experiment(), "{encoded}");
            assert_eq!(encoded, namer.encode(), "{encoded}");
        }
    }

    #[test]
    fn url_vectors_resolve_and_validate() {
        use ps_http::google_url::GoogleUrl;
        for (base, reference, expected) in url_resolve_vectors() {
            let base = GoogleUrl::new(base);
            assert_eq!(expected, GoogleUrl::resolve(&base, reference).spec());
        }
        for (spec, valid, web_valid) in url_validity_vectors() {
            let url = GoogleUrl::new(spec);
            assert_eq!(valid, url.is_valid(), "{spec}");
            assert_eq!(web_valid, url.is_web_valid(), "{spec}");
        }
    }

    #[test]
    fn accept_vectors_classify() {
        use ps_image::negotiate::{accepts_image_format, explicitly_rejects_image_format};
        for (header, mime, accepts, rejects) in accept_vectors() {
            assert_eq!(accepts, accepts_image_format(&[header], mime), "{header}");
            assert_eq!(
                rejects,
                explicitly_rejects_image_format(&[header], mime),
                "{header}"
            );
        }
        // Multiple header values are all consulted.
        assert!(accepts_image_format(&["text/html", "image/webp"], "image/webp"));
    }

    #[test]
    fn ua_vectors_match_matchers() {
        use ps_image::negotiate::{ua_supports_avif, ua_supports_webp};
        for (ua, supports) in ua_webp_vectors() {
            assert_eq!(supports, ua_supports_webp(ua), "{ua}");
        }
        for (ua, supports) in ua_avif_vectors() {
            assert_eq!(supports, ua_supports_avif(ua), "{ua}");
        }
    }

    #[test]
    fn capability_vectors_gate() {
        use ps_image::negotiate::{device_support, request_support, CapabilityList};
        let chrome32 = "Mozilla/5.0 (Windows NT 6.2; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/32.0.1667.0 Safari/537.36";
        // Absent header passes everything through.
        let full = request_support(
            device_support(&["image/webp"], chrome32),
            &CapabilityList::absent(),
        );
        assert!(full.webp_rewritten_urls && full.webp_lossless_alpha && full.webp_animated);
        for (header, webp, lossless, animated) in capability_vectors() {
            let gated = request_support(
                device_support(&["image/webp"], chrome32),
                &CapabilityList::parse(header),
            );
            assert_eq!(webp, gated.webp_rewritten_urls, "{header}");
            assert_eq!(lossless, gated.webp_lossless_alpha, "{header}");
            assert_eq!(animated, gated.webp_animated, "{header}");
        }
    }

    #[test]
    fn variant_keys_match() {
        use ps_image::negotiate::LibWebpLevel;
        use ps_image::variant::{variant_cache_key, VariantContext};
        for (level, avif, mobile, save_data, small_screen, expected) in variant_key_vectors() {
            let libwebp_level = match level {
                0 => LibWebpLevel::None,
                1 => LibWebpLevel::LossyOnly,
                2 => LibWebpLevel::LossyLosslessAlpha,
                3 => LibWebpLevel::Animated,
                _ => panic!("bad level {level}"),
            };
            let ctx = VariantContext {
                libwebp_level,
                supports_avif: avif,
                mobile_user_agent: mobile,
                may_use_save_data_quality: save_data,
                may_use_small_screen_quality: small_screen,
            };
            assert_eq!(expected, variant_cache_key(ctx));
        }
    }

    #[test]
    fn css_goldens_minify_and_rejects_fail_closed() {
        use ps_css::minify::minify_css;
        for (input, expected) in css_golden_vectors() {
            assert_eq!(
                Some(expected.as_bytes().to_vec()),
                minify_css(input.as_bytes()),
                "{input}"
            );
        }
        for input in css_reject_vectors() {
            assert_eq!(None, minify_css(input.as_bytes()), "{input}");
        }
    }

    #[test]
    fn js_goldens_minify_and_rejects_fail_closed() {
        use ps_js::minify::{legacy_minify_js, minify_utf8};
        use ps_js::tokenizer::TokenizerPatterns;
        let patterns = TokenizerPatterns::new();
        for (input, expected) in js_golden_vectors() {
            let (out, ok) = minify_utf8(&patterns, input.as_bytes());
            assert!(ok, "new minifier failed on {input:?}");
            assert_eq!(expected.as_bytes(), out, "new minifier on {input:?}");
            let out = legacy_minify_js(input.as_bytes()).expect("old minifier failed");
            assert_eq!(expected.as_bytes(), out, "old minifier on {input:?}");
        }
        for input in js_reject_vectors() {
            assert!(!minify_utf8(&patterns, input.as_bytes()).1, "{input:?}");
            assert_eq!(None, legacy_minify_js(input.as_bytes()), "{input:?}");
        }
    }

    #[test]
    fn admin_routes_answer_their_statuses() {
        use ps_admin::log_buffer::LogBuffer;
        use ps_admin::purge::PurgeSetBackend;
        use ps_admin::site::{AdminConfig, AdminRequest, AdminSite};
        use ps_cache::purge::PurgeSet;
        use ps_cache::timer::MockTimer;
        use ps_http::fetch::{AsyncFetch, BufferingFetch};
        use ps_http::google_url::GoogleUrl;
        use ps_http::query_params::QueryParams;
        use ps_http::request::RequestContext;
        use ps_http::response::HttpOptions;
        use std::sync::{Arc, Mutex};

        let timer = Arc::new(Mutex::new(MockTimer::new(1_700_000_000_000)));
        let log = Arc::new(Mutex::new(LogBuffer::new(1 << 20)));
        let site = AdminSite::new(timer, log);
        let stats = Statistics::new();
        let mut options = RewriteOptions::new();
        options
            .set_option("EnableCachePurge", "on", Scope::Server)
            .unwrap();
        let global = AdminConfig::default();
        let purge_set = PurgeSet::new(1 << 20);
        for (path, query_string, is_global, status) in admin_route_vectors() {
            let stripped = GoogleUrl::new(&format!("http://localhost{path}"));
            let mut query = QueryParams::new();
            if !query_string.is_empty() {
                query.parse_from_untrusted_string(query_string);
            }
            let mut fetch = BufferingFetch::new(
                &format!("http://localhost{path}"),
                RequestContext::new(HttpOptions::default()),
            );
            let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
            let mut request = AdminRequest {
                options: &options,
                global_options: &global,
                statistics: &stats,
                stats: &stats,
                inventory: None,
                show_cache: None,
                purge: &mut backend,
                purge_set: &purge_set,
            };
            site.admin_page(
                is_global,
                &stripped,
                &query,
                &options,
                &mut fetch,
                &mut request,
            );
            assert_eq!(
                status,
                fetch.response_headers().status_code(),
                "{path}?{query_string}"
            );
        }
    }

    #[test]
    fn stat_names_are_registered() {
        let stats = Statistics::new();
        init_all(&stats);
        for name in stat_name_vectors() {
            assert!(
                stats.get_variable(name).is_some()
                    || stats.get_up_down_counter(name).is_some()
                    || stats.get_histogram(name).is_some()
                    || stats.get_timed_variable(name).is_some(),
                "{name}"
            );
        }
    }

    #[derive(Clone, Default)]
    struct SharedVec(Rc<RefCell<Vec<u8>>>);

    impl ByteSink for SharedVec {
        fn write_bytes(&mut self, bytes: &[u8]) -> bool {
            self.0.borrow_mut().extend_from_slice(bytes);
            true
        }
    }

    #[test]
    fn html_goldens_rewrite_byte_identically() {
        for (filters, input, expected) in html_golden_vectors() {
            let mut options = RewriteOptions::new();
            if !filters.is_empty() {
                assert!(options.filters_mut().enable_by_comma_list(filters));
            }
            let sink = SharedVec::default();
            let mut driver = Driver::new(&options, &ChainContext::new(true, true), sink.clone());
            driver.start_parse(b"http://compat.test/page.html", false);
            driver.write(input.as_bytes());
            driver.finish();
            let actual = String::from_utf8(sink.0.borrow().clone()).unwrap();
            assert_eq!(expected, actual, "{filters} on {input}");
        }
    }

    #[test]
    fn nginx_vectors_match_helpers() {
        use ps_nginx::fetch_body::response_has_no_body;
        use ps_nginx::inplace_url_agreement::url_agrees;
        for (server_uri, server_host, module_path, module_host, merge, agrees)
            in nginx_agreement_vectors()
        {
            assert_eq!(
                agrees,
                url_agrees(server_uri, server_host, module_path, module_host, merge),
                "{server_uri} vs {module_path}"
            );
        }
        for (status, head, has_body) in response_body_vectors() {
            assert_eq!(!has_body, response_has_no_body(status, head));
        }
    }

    #[test]
    fn property_table_shape_pins_config_1() {
        // 167 `AddProperties` registrations, in C++ order.
        assert_eq!(167, PROPERTIES.len());
        let mut ids = HashSet::new();
        let mut named_helpless = 0;
        let mut synthetic = 0;
        for prop in PROPERTIES {
            assert!(ids.insert(prop.id), "duplicate id {}", prop.id);
            if prop.name.is_empty() {
                // `AddRequestProperty` entries: no name, process scope.
                synthetic += 1;
                assert!(prop.help.is_none());
                assert_eq!(Scope::ProcessStrict, prop.scope);
            } else if prop.help.is_none() {
                named_helpless += 1;
            }
        }
        // Exactly C++'s 34 NULL-help registrations plus the 5
        // request-only synthetic entries.
        assert_eq!(34, named_helpless);
        assert_eq!(5, synthetic);
        // filters-5: `RewriteLevel` is a regular base property.
        let level = PROPERTIES.iter().find(|p| p.name == "RewriteLevel").unwrap();
        assert_eq!("l", level.id);
        assert_eq!(Scope::Directory, level.scope);
    }

    #[test]
    fn scope_gate_pins_config_2() {
        // The query path sets only query-scope options.
        let mut options = RewriteOptions::new();
        assert_eq!(
            QueryStatus::Success as u8,
            options.scan_query("ModPagespeedCssInlineMaxBytes", "100", true) as u8
        );
        assert_eq!(
            QueryStatus::NoneFound as u8,
            options.scan_query("ModPagespeedMaxHtmlParseBytes", "100", true) as u8
        );
        // File directives pass the context scope through the same
        // `prop.scope > max_scope` gate, 1:1 with C++.
        let mut options = RewriteOptions::new();
        assert!(options
            .set_option("MaxHtmlParseBytes", "100", Scope::Directory)
            .is_ok());
        let err = options
            .set_option("MaxHtmlParseBytes", "100", Scope::Query)
            .unwrap_err();
        assert_eq!(SetOutcome::NameUnknown as u8, err.outcome as u8);
    }
}
