# Beacon traffic reduction — 2026-11-27

Baseline917cbf6. User authorizes avoiding repeated font POSTs/cached-nonce replay and frequent critical sampling. No deployment or live configuration changes.

## Changes

Both font beacon assets remember the last successfully queued nonce per page/options, using sessionStorage across navigation/reload and an in-document map. Replaying that cached nonce in the same tab no longer sends another request. A fresh nonce, page or options identity still reports. Empty font collections do not mark sent. sendBeacon false/throw does not mark sent, so retries remain possible. No server nonce/replay validation changed. Storage errors never break the page: same-document suppression still works, but across reloads cannot be guaranteed when sessionStorage is blocked. A successful queue is not proof of server acceptance; this is best-effort duplicate suppression, not delivery confirmation. No persistent cross-tab/browser identifier added. Session storage retains one last nonce per page/options entry for that tab lifetime.

Critical-image high-frequency threshold changed3→1 accepted reports, as requested by user (one accepted evidence enough). Existing100x low-frequency interval, candidate-change resets, scheduled relearning and nonce safety remain. With5s base, the next sampling after acceptance schedules its successor500s later. This does not immediately move the already-scheduled next5s sample, nor eliminate every future BYPASS. Record counters still count true reports; no fabricated evidence. This deliberately differs from original C++ three-report high-frequency policy, trading fewer redundant samples for earlier low-frequency sampling while retaining effective critical-set/LCP invalidation.

RUM timing scripts/endpoint, resource optimization, private/cookie/HTTP gates and critical nonce-bearing response exclusion are untouched. Pending resource publication remains a separate issue.

## Verification

```bash
cargo test --locked -j 2 -p ps-html --lib beacon_finder
gjs tools/test_font_beacon_dedup.js
cargo test --locked -j 2 -p ps-html --lib --test critical_images_beacon --test html_cache_vary
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
```

Old font asset917cbf6 deterministically fails new executable lifecycle harness: second same-nonce invocation emits2POSTs instead of1. Both final assets pass same-document and simulatedreload dedup, freshnonce, page/options isolation, rejectedqueue retries and blockedstorage fallback. Harness executes actual assets with mocked browser transport/FontFaceSet/storage; it is not a live browser network test.

HTML216 + criticalbeacon6 + Vary8 pass; NGINX294 pass; matching deployment release build passes. New Rust regression accepts a nonce-validated textLCP report, confirms validcount1, and proves following sampling uses100x interval and remains due again eventually. Existing nonce rejection/replay/candidate reset/sampling gates continue passing.

No live traffic improvement or latency benchmark measured. Deterministic before/after network-call test demonstrates redundant POST suppression; schedule test demonstrates lowerfrequency. Follow deployment with HAR across repeated navigation: repeated font nonce should send once pertab/page/options, new nonce remains allowed, timing RUM retained, occasional genuine critical sampling may bypass. Refer existing live baseline rather than claiming permanentHIT.
