# Pending rewritten image fallback

2026-11-28, baseline eb0b4a0. No deployment or production configuration changes.

## Behavior and architecture

Optimized HTTP-cache hits remain first. For a reconstructed `ic` image with an installed background pool, an output MISS schedules the existing background behavior snapshot, with its dimensions/options/capabilities and the same partition/coordination key. The request does not take a foreground lease or run conversion. Existing worker bounds, queue shedding, cross-process coordination and successful-output metadata publication are reused.

An authorized, fresh, cache-eligible original HTTP-cache entry serves directly through the rewritten URL with its original image MIME and bytes. Response headers are `Cache-Control: private, no-store, max-age=0`; source ETag/Last-Modified/Expires, range headers and surrogate/CDN cache directives are removed. The source cache entry is unchanged and temporary bytes never enter the optimized output cache. Optimized output becomes visible after the background worker publishes it.

When source bytes are not in the validated HTTP cache, return an immediate temporary 307 redirect to the decoded, authorized source URL with the same no-store policy. This avoids waiting for source fetch/conversion but adds a browser redirect; original delivery still depends on the origin. Missing or invalid original files cannot be repaired by fallback. No-pool embeddings retain their existing synchronous serving behavior. Queue saturation/shutdown returns fallback rather than running conversion on the request thread.

Image requests no longer get an unconditional If-Modified-Since 304 before cached optimized content is known to exist. Optimized AVIF/WebP cache candidates and metadata fallback candidates honor format acceptance; the existing ServeRewrittenWebpUrlsToAnyAgent option retains its documented override. Other resource filters and HTML cache/RUM eligibility are unchanged.

## Verification commands

From repository root, logs are adjacent to this file:

- `cargo test --locked -j2 -p ps-html --test image_rewrite` baseline — 124 passed, 20.28 seconds (`baseline.log`). This is suite verification, not a request-latency benchmark.
- `cargo test --locked -j2 -p ps-html --lib` — 230 passed (`lib.log`).
- Initial `cargo test --locked -j2 -p ps-html --test image_rewrite` — 126 passed (`images.log`).
- `cargo test --locked -j2 -p ps-html --test async_progressive` — 8 passed (`progressive.log`).
- `cargo test --locked -j2 -p ps-html --test image_rewrite pending_image` — final 2 passed (`regression.log`). Only worker held behind a channel barrier, so original-response checks cannot race conversion. Cold and cached fallback requests must return within 100ms; repeated pending requests, source MIME/body, no-store/validator removal, eventual actual WebP bytes, unsupported-client fallback, and bounded-queue saturation are asserted.
- `cargo test --locked -j2 -p ps-nginx --lib` — initial 296 passed (`nginx.log`).
- `NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j2 -p ps-nginx` — passed, 1m44s (`build.log`).
- `python3 tools/verify_early_html_cache.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/pending-image-fallback/nginx-oracle.json --rum-only --critical-css --browser-groups --analytics-early fixed` — passed (`oracle.log`, `nginx-oracle.json`). Confirms full-document early hits skip origin, Chrome/Firefox sharing, RUM browser clock and private-response controls. This existing oracle does not exercise pending image responses; those use actual ResourceDriver integration tests above.

`cargo test --locked -j2 -p ps-html` — final 1,805 passed across 65 result groups (`html-final.log`). `cargo test --locked -j2 -p ps-nginx --lib` — final 296 passed (`nginx-final.log`). `git diff --check` passed. Initial html-full.log contains an intermediate test failure: the old test helper did not forward Accept headers into BufferingFetch. The new regression helper forwards them; the production bridge already does. Another intermediate assertion used the intentional default WebP-to-any-agent override; the unsupported-client test now explicitly disables that option. No production latency speedup is claimed.
