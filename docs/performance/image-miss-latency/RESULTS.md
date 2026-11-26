# Image latency on MISS — 2026-11-26

## Measured baseline and scope

Read-only live inspection of rustpagespeed.com on4752cb6: hero already eager/high with a matching head preload and srcset. Existing caches were preserved; no forced cache purge/cold AVIF encode. Five loopback HTTPS requests per image return cached AVIF bytes in roughly1ms (110077 and181199 bytes); server-warm.json retains rounded timings. Two fresh-profile Chrome traces discover images roughly3ms after HTML commit/parser activity, with response/transfer completion around229/299ms in one run and806/1567ms in another. These are not end-to-end navigation/TTFB measurements; trace Document events do not capture the entire navigation connection setup. First trace uses virtual time; second has no virtual-time budget. Raw traces contain irrelevant browser activity and were not committed; sanitized resource summaries retain the measurements. Neither DOM dump exposed the cache diagnostic comment, so do not label these traces confirmed HTML MISS or HIT. This variability and fast loopback do not justify attributing every reported second of delay to encoding.

## Concrete code defect repaired

On image metadata MISS the HTML path already detaches and returns original URLs. However, the background image behavior retained its HTML/srcset place and re-entered the deferred-byte branch, publishing a hash0 URL instead of finishing bytes. The first browser request for that URL then did the actual optimization. Regression baseline reproduced this: Fetch worker generated output/admin record; HtmlAttr worker generated no completed output record.

Added a behavior-local background completion flag. Only pool snapshot execution sets it; HTML/srcset semantics (inlining, resize, format/options) are retained, while the existing full optimizer stores image bytes before publishing versioned metadata. Foreground/no-pool behavior and legacy hash0 reconstruction remain valid. Existing bounded queue, partition dedup/coordinator and saturation fallback unchanged. No waiting/fetch/encode added to HTML request path; no quality/filter disabled. The pool can spend more time per job because it now completes the requested optimization instead of deferring that work again. All srcset candidate jobs remain subject to existing bounds; no new unbounded prefetch queue.

## Controlled release before/after

Same Puzzle.jpg fixture and options,300ms scripted origin delay, five independent test-process samples per version. Baseline compiled from isolated4752cb6 worktree with only the new regression copied in; its failure is expected because it asserts the repaired versioned-URL contract. Tests first measure a successful hash0 reconstruction before that assertion. Worktree was removed after comparison; only owned temporary test edit was discarded. No user changes discarded.

| Median,5 runs | Before | After |
| --- | ---: | ---: |
| HTML original-URL fallback | 0.230ms | 0.364ms |
| First optimized-image serve | 25.144ms | 0.163ms |
| Total fixture elapsed, including origin/background/poll/serve | 335.641ms | 357.605ms |
| Optimized JPEG bytes |118481|118481|

This moves encoding off the first optimized-image request; it does not remove total optimization work or demonstrate a production one-second gain. Total fixture elapsed did not improve in this run. Output byte count is identical, and the same optimization engine/options are used; no claim of a separate AVIF cold-encode benchmark. After completion, first optimized serve performs no origin fetch or new rewrite (counter assertions). HTML remains well below the simulated origin delay. Metadata-ready time was not separately instrumented; total elapsed includes first serve, so do not mislabel it as metadata-ready latency.

## Commands and validation

From repository root:

```bash
cargo test --locked -p ps-html --lib worker_image_rewrite_reports_record_to_sink -- --nocapture
cargo test --locked -p ps-html --test async_progressive image_background_completion -- --nocapture
cargo test --locked --release -p ps-html --test async_progressive image_background_completion -- --nocapture
cargo test --locked -p ps-html --lib --test async_progressive --test image_rewrite --test image_url_encoder
cargo test --locked -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
```

Baseline release command used `CARGO_TARGET_DIR=$PWD/target cargo test --locked --release --manifest-path /tmp/ps-image-before-4752cb6/Cargo.toml -p ps-html --test async_progressive image_background_completion -- --nocapture` after copying the regression test. Five samples execute the built test binaries directly with `image_background_completion --nocapture`; metrics/expected baseline failure retained. Live Chrome flags used fresh temporary --user-data-dir, --headless --no-sandbox --disable-gpu --no-proxy-server, --host-resolver-rules='MAP rustpagespeed.com 157.245.148.68', --trace-startup=devtools.timeline,loading,blink.user_timing, --trace-startup-format=json and --dump-dom. Second run uses --timeout=12000 instead of --virtual-time-budget=6000. Loopback probe uses Python http.client.HTTPSConnection(127.0.0.1), Host rustpagespeed.com, ChromeUA and image/avif,image/webp Accept, persistent connection five requests per original HTML img src.

Passed: HTML library210, async progressive8 (including queue/dedup/fallback), image rewrites124, image URL55, NGINX294; worker test covers Fetch/HTML/srcset and admin records. Matched1.31.10 release module built. Full logs adjacent. No production installation, reload, purge or new live after-build benchmark. After deployment, verify newly missing metadata progresses original→versioned URL with ready bytes; older cached hash0 metadata can use legacy first-serve reconstruction until naturally refreshed. Network/transfer contribution remains to be profiled on user's browser if seconds persist.
