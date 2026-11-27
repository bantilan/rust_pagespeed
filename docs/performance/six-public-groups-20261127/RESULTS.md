# Six public HTML client groups — 2026-11-27

User requests AVIF/WebP/JPEG-compatible fallback × desktop/mobile, public eligibility and beacon result only; explicitly approves ignoring wp_lang/retina values under equivalent-public-HTML contract. Baseline source3ae700a. No production deployment or config changes.

## Contract and architecture

New default-on Directory option HtmlCacheSimpleVariants (not query-settable), active with existing HtmlCachePublicBrowserGrouping. Public request identity becomes `public-six-v1:{avif|webp|fallback}:{desktop|mobile}`. Existing negotiation infers supported image format from browser/Accept with explicit q=0 rejection; AVIF first, then WebP, otherwise original JPEG/PNG/generic fallback. Non-desktop/tablets map to mobile. Browser families/versions, Accept-Language, transport encoding, Save-Data/Via/privacy/arbitrary headers, allowed tracking and language/retina values no longer add request dimensions. Compression still negotiates independently on output. Original request headers/cookies are forwarded unchanged.

Response headers and origin TTL no longer fragment identity in simple mode. Cache eligibility still verifies response privacy/Set-Cookie/freshness/Vary-star, authorization/cookie/request constraints; TTL/expiration/purge remain validation, not user variants. URL/query, effective rewrite configuration/cache namespace/debug/file modification and effective beacon/font learning decisions remain correctness inputs. Existing evidence cohorts may differ for source devices, but client grouping adds only six categories and evidence decisions remain as requested. This does not force pending-resource or sampled-nonce HTML into shared storage.

This mode deliberately assumes equivalent PUBLIC origin HTML within each group: origin named Vary no longer requests extra client variants. Applications serving language/custom-header/preference-dependent HTML must use `HtmlCacheSimpleVariants off` to retain previous detailed header/capability/opaque preference identity, or `HtmlCachePublicBrowserGrouping off` for exact request grouping. The user explicitly approved ignoring language/retina values; no such contract is assumed for unknown/auth/session/cart/CSRF cookies, which still bypass. Arbitrary application authentication implemented only through an unrecognized custom header is incompatible with this public contract; access control must be enforced before early cache serving or public mode disabled for that route.

WP Super Cache comparison: https://github.com/Automattic/wp-super-cache/blob/trunk/wp-cache-phase1.php registers wp_cache_check_mobile for cache naming; https://github.com/Automattic/wp-super-cache/blob/trunk/wp-cache-phase2.php defines UA-based mobile detection and `-mobile` suffix, with configuration/hooks/custom group support. Used as a coarse-grouping reference, not copied detector or exact behavioral parity. Rust keeps existing negotiation/device classifier, collapsing its tablet class into mobile.

## Verification

```bash
cargo test --locked -j 2 -p ps-html --lib --test html_cache_cookies --test html_cache_vary
cargo test --locked -j 2 -p ps-config
cargo test --locked -j 2 -p ps-html --lib --test html_cache_cookies --test html_cache_vary --test critical_images_beacon
cargo test --locked -j 2 -p ps-nginx -p ps-admin --lib
cargo test --locked -j 2 -p ps-admin --lib --test admin_site
cargo test --locked -j 2 -p ps-html --lib six_public_groups
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
cargo run --locked --release -j 2 -p ps-html --example cookie_preference_latency
```

Final config95, HTML217+critical6+cookies7+Vary8, pinned NGINX294+admin5, admin integration49 passed. Matching1.31.10release build passed after final admin changes. Tablet extension passed targeted test after full suite; last source edit was test-only. Baseline HTML216+cookie7+Vary8 passed.

New actual ordinary/early test verifies exactly six keys, all six body variants isolated; Chrome/Firefox share despite language/encoding/arbitraryheader/DNT/preference/tracking differences and named origin Vary; q0 AVIF/WebP reject maintains fallback separation; tablet maps mobile. Auth/session/unknown mixtures cannot hit early or normal cache. New config test verifies default, query rejection, override, signature separation and publicmodeoff. Previous detailed-mode tests explicitly opt out, preserving and testing their old privacy/Vary/preference behavior rather than silently deleting assertions. Existing sampling/nonce/detachedresource/beacon/expiry/purge tests pass.

## Local lookup measurements

Actual existing cookie_preference_latency release fixture: reused driver, memory cache,32KiBbody,50warmups+2,000measured/assertedHITs per fixture/path. Three alternating serial before/after pairs; median of per-run medians below. Sixteen fixture/path/version cells each have6,000assertedHITs. Raw trial CSVs/summary adjacent.

Baseline executable was retained from the prior9941683 preference milestone (before later sampling-only changes); it exercises the earlier detailed public lookup. Thus this is not a fresh isolated3ae700a rebuild/code-only causal benchmark. Source baseline tests are3ae700a. No production timing, filter/resource/NGINX-stage profiling or fully cold-cache benchmark performed. Temporary retained executable removed after measurements.

| Fixture/path | Detailed earlier build | Six groups |
| --- | ---: | ---: |
| Cookie-free ordinary | 7.494 µs | 6.613 µs |
| Cookie-free early | 12.784 µs | 10.560 µs |
| Tracking ordinary | 8.606 µs | 7.484 µs |
| Tracking early | 13.926 µs | 11.031 µs |
| Preferences ordinary | 9.207 µs | 7.674 µs |
| Preferences early | 15.239 µs | 11.412 µs |
| Full screenshot jar ordinary | 9.518 µs | 7.865 µs |
| Full screenshot jar early | 15.729 µs | 11.823 µs |

No permanent HIT promise: config/URL/format/device/effectivelearning changes, expiry/purge, sampled nonce requests, private requests or pending background work can still MISS/BYPASS. Install build through normal workflow, then verify crossbrowser sharing and actual application public-content equivalence. No push/deployment performed.
