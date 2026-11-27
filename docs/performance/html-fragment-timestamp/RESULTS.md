# Fragment cache timestamp suppression

Implemented 2026-11-27 against baseline 3ce3c34. No production deployment or configuration changes.

Only explicit complete source HTML documents receive ShowCacheTimestamp diagnostics. Plain text, JSON, fragments and incomplete documents receive neither cache-status timestamp nor cache-detail comments on MISS, BYPASS, ordinary HIT or early HIT. The bounded streaming detector ignores comments/raw text; original source takes precedence over markup synthesized by filters. This is deliberately conservative: documents omitting explicit html tags also omit diagnostics. Fragment caching and rewrite filters remain enabled, with existing privacy, expiry, purge, evidence and authorization gates unchanged.

Source eligibility is persisted in versioned D1 HTML body metadata; policy hints retain their existing format. Legacy body text cannot collide with the new metadata header. The public-html-v3 key namespace prevents old rewritten fragments from acquiring diagnostics; installation causes a one-time HTML cache warm-up. The scanner does no work with timestamps off. No per-URL or query-string exceptions.

## Verification

Commands run from repository root; logs are in this directory.

- Baseline: `cargo test --locked -j2 -p ps-html --lib html_cache` — 30 passed (`baseline.log`).
- `cargo test --locked -j2 -p ps-cache --lib` — 75 passed (`cache-final.log`); versioned/legacy codec, malformed metadata and exact-body preservation regressions included.
- `cargo test --locked -j2 -p ps-html --lib` — 230 passed (`html-final.log`); streamed source detection and fragment MISS/HIT/early/BYPASS, synthesized output and full-document diagnostics regressions included.
- `cargo test --locked -j2 -p ps-nginx --lib` — 296 passed (`nginx-final.log`); actual HtmlSink caches a fragment with diagnostics enabled and preserves non-document eligibility on HIT.
- `cargo test --locked -j2 -p ps-html` — 1,803 passed across 65 result groups (`html-full.log`).
- `NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j2 -p ps-nginx` — passed, 56.94 seconds (`build.log`).
- `python3 tools/verify_early_html_cache.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/html-fragment-timestamp/nginx-oracle.json --rum-only --critical-css --browser-groups --analytics-early fixed` — passed. Real isolated NGINX full-document HIT timestamps, origin skipping, browser-group sharing, RUM clock retention, critical CSS preservation and private-response controls verified (`oracle.log`, `nginx-oracle.json`). Startup default error-log permission alert is harmless; isolated instance and assertions completed successfully.

Initial html.log records the expected old assertion failure when an entry created with diagnostics off was retroactively expected to acquire a comment. Regression now requires explicit producer eligibility; final runs pass. Initial cache.log/nginx.log are intermediate successful runs. This is a diagnostic correctness change; no live latency improvement is claimed.
