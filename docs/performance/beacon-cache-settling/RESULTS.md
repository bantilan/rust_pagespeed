# Image-free beacon/cache settling — 2026-11-26

## Evidence and cause addressed

The six supplied captures target /sample-page/, unlike the earlier homepage replay. All omit document response text, so individual HIT/BYPASS comments cannot be independently read from them. They show successful font reports; those do not acknowledge critical-image sampling. Prior live logs contained cancelled critical POSTs (499). No live configuration/deployment changes were made.

Code-level browser reproduction: with lazy loading (Run send_at_onload=false), an image-free page had no lazy image callback to trigger reporting. It waited for pagehide. The sender used XHR, which can be cancelled by navigation. Thus repeated short visits can leave learning incomplete and retain high-frequency sampling. Baseline browser regression reproduces absent load report and XHR-only hidden delivery for both shipped assets. This establishes those defects, not a claim that every reported live BYPASS had the same cause.

## Repair

Both optimized/debug collectors queue critical reports using navigator.sendBeacon with an application/x-www-form-urlencoded Blob; false admission or exceptions retain legacy XHR fallback. Image-free pages trigger collection on load and on subsequently arriving LCP observations. An active observer with no usable observation continues observing until evidence or hide; no arbitrary timeout invents LCP evidence. Pages with images retain lazyload-driven timing. Existing nonce validation, replay protection, evidence generations, privacy gates and periodic sampling remain active. No HTML critical-path waits or resource fetches added.

## Baseline and verification

| Check | Before | After |
| --- | --- | --- |
| Image-free lazyload load report, both assets | No report | One nonce-bound report |
| Hidden report with native beacon available | Cancellable XHR | Native beacon queue |
| Native queue false/throws | Legacy fallback | XHR fallback retained |
| Sampling to shared evidence to cache | Not previously verified together | 4 accepted real-browser reports, 4/4 HTTP HIT probes |

Commands run from repository root:

```bash
python3 tools/verify_beacon_navigation.py
python3 tools/verify_image_free_beacon_js.py
cargo test --locked -p ps-html --lib --test critical_images_beacon --test html_cache_cookies
cargo test --locked -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
python3 tools/verify_early_html_cache.py --nginx ../rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --sampled-image-free --output docs/performance/beacon-cache-settling/browser-cache.json
python3 tools/verify_early_html_cache.py --nginx ../rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --rum-only --analytics-early fixed --output docs/performance/beacon-cache-settling/analytics-rum.json
python3 tools/verify_beacon_reload.py --nginx ../rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --image-free --early-html-cache --output docs/performance/beacon-cache-settling/reload.json
```

Results: HTML207 + critical filter6 + cookie4, NGINX294 pass; release build passes. Browser lifecycle/native admission/fallback tests pass for both assets; baseline failures preserved. Real Chrome submits four accepted samples to isolated NGINX/FastCGI. Fourth issuance sees three accepted samples and selects the existing low-frequency interval. After publication, four HTTP probes stay HIT including beyond five seconds, with no additional origin requests. Reload test verifies acceptance/replay across overlapping workers. RUM/analytics/private-origin oracle passes.

Limits: Chrome dump-DOM omitted diagnostic status comments in this fixture, so the cache HIT oracle is a separate HTTP variant sharing the actual browser-learned page/device evidence, backed by an origin request counter. Do not claim browser-DOM HIT or resolution of the earlier static-alias fixture. No production latency improvement measured or live deployment performed. Initial learning and periodic samples still legitimately bypass; this patch repairs sampling completion rather than disabling it.

Synthetic 25 ms origin fixture, separate RUM/analytics regression: warm HIT median 0.244 ms; analytics HIT median 0.238 ms. These are local fixture numbers, not production before/after latency.
