# Late image-LCP collector repair — 2026-11-26

## Reproduction and repair

The testing-world HAR diagnosed in ../testing-world-cache shows sampling collectors on BYPASS responses but no critical reports captured for that page. It ends with four HITs; it does not establish that all later requests bypass. Source reproduction confirmed a collection gap: an onload/lazyload check could run before the LCP observer delivers any entry. The collector returned without sending, then its observer retried only for image-free pages. Image-bearing pages waited for hide/navigation.

Baseline Chrome asset tests reproduce zero reports in image-late-onload and image-late-lazy cases for both shipped assets (four failures). The repair marks checkRequested only when collection is actually invoked, and the observer retries once usable evidence arrives. This preserves lazyload readiness; an image page without a requested check still does not report early. Existing beaconSent guard ensures one send despite subsequent callbacks/hide. Active observers still await evidence rather than inventing LCP; hidden/unavailable fallback, nonce transport, validation and replay behavior unchanged. No change to sampling cadence, HTML-cache privacy/expiration rules, resource workers, format or quality options.

## Verification

- 18 lifecycle/transport browser cases pass across debug and optimized assets, including late image onload, late lazyload-ready, duplicate callbacks/hide, unchanged lazy-image readiness exclusion, image-free cases and native admission fallback. Legacy image-free collector test passes.
- HTML210 + critical filter6 + cache Vary8 pass.
- NGINX initially293/294: existing restart_namespace_prevents_old_shared_generation_hits failed uniqueness assertion. Original log retained; isolated rerun and full294 passed without source/test changes to that area. This remains a separate intermittent namespace-test finding, not attributed to this JS repair.
- Matched NGINX1.31.10 release build passed. Local artifact SHA256: 6a5e04a80915193da49f4ee429723c0e9b3d0f5fdf5e2f2ad522f67d4bf886ef.
- Real-time Chrome + isolated NGINX/FastCGI: four accepted critical reports and persisted LCP kind=img, then MISS publication followed by four HIT HTTP probes, including past five seconds, with no additional origin calls. Original-image fixture is served normally from its own static route. Header replay RUM/analytics/browser grouping and private-origin controls pass separately.

Fixture corrections are retained in intermediate logs: synthetic favicon initially returned another HTML page and created a second evidence record; fixture now returns204 for favicon. Virtual-time dump-DOM acknowledged unknown LCP but did not reliably paint image LCP, so the image oracle uses a small stdlib DevTools-pipe helper with real wall-time rendering and graceful browser shutdown. Temporary profile cleanup race was fixed by Browser.close before teardown. The final assertion requires actual img evidence; it was not weakened to accept unknown. Earlier image-free dump-DOM behavior remains unchanged.

## Reproduction commands

From repository root:

```bash
python3 tools/verify_beacon_navigation.py
python3 tools/verify_image_free_beacon_js.py
cargo test --locked -p ps-html --lib --test critical_images_beacon --test html_cache_vary
cargo test --locked -p ps-nginx --lib restart_namespace_prevents_old_shared_generation_hits
cargo test --locked -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
PYTHONDONTWRITEBYTECODE=1 python3 tools/verify_early_html_cache.py --nginx ../rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --sampled-image --output docs/performance/late-lcp-cache/image-cache.json
python3 tools/verify_early_html_cache.py --nginx ../rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --rum-only --analytics-early fixed --browser-groups --output docs/performance/late-lcp-cache/rum-browser-groups.json
```

## Limits / next deployment step

No live installation/reload, no production before/after latency claim, no promise of permanent HIT: legitimate resampling, accepted evidence changes, TTL/purge and private responses still apply. Stable-cache assertions use a separate HTTP request identity sharing actual browser-learned page/device evidence and an origin counter; browser DOM omits diagnostic comments, so do not claim DOM-observed HITs. The collector gap is proven repaired; whether every historical testing-world MISS had this cause is not established. Remaining unexplained MISS identities would need internal diagnostics if they recur after deployment.

Install a module matching live NGINX, then verify testing-world reports image evidence and stays HIT between legitimate resampling intervals. No new configuration needed, no sampling disabled and no cache purge performed.
