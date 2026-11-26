# Analytics early HTML-cache HIT — 2026-11-26

Implemented at user request. early_key now evaluates the existing configured cookie allowlist rather than rejecting every Cookie header. No new cookie exceptions or directives: approved built-in/custom ignored cookies use the same cookie-independent identity as ordinary HTML lookup. Unknown/private/login/session/mixed/malformed/Cookie2/forced-bypass cookies remain blocked. HtmlCacheIgnoreCommonCookies off remains respected. Successful approved-cookie early HITs increment existing html_cache_cookie_allowlisted and HTML HIT counters once. All origin-policy/evidence/TTL/purge/request gates remain in lookup_early and eligibility_key; no cache read or filter removed.

## Before/after origin counter oracle

Matched NGINX1.31.10 isolated FastCGI fixture with25ms artificial origin delay,50KiBHTML,20measured analytics requests after publication, fresh HTTP connections on loopback; no live changes. Each request uses different `_ga` values plus `_gid`. Baseline source6496a45 runtime; matched baseline release built before edits. Final module SHA256 `e396622fa5f9eb6d1545efb37f88ddb6ddb7b23898216136f56c4602b36f3e83`.

| RUM fixture | Before | After |
| --- | ---: | ---: |
| Analytics requests reporting HIT | 20/20 | 20/20 |
| Origin calls for those requests | 20 | **0** |
| Median response | 26.774 ms | **0.685 ms** |
| RUM browser-clock initializer retained | Yes | Yes |

Core-filter fixture after: analytics median0.662ms,20HITs with0origin calls. For each build, all HIT body hashes match its own published cold body (beacon options can make across-build hashes differ). Earlier development fixed runs were0.25–0.29ms; final recorded run0.66–0.69ms illustrates host variation, but origin counter conclusion is exact. Live previous57–71ms analytics lateHITs are baseline evidence, not an after deployment measurement. No production speedup claim until installed/retested.

Cold first publication still invokes origin. Unknown, mixed analytics+session, malformed cookies, Cookie2, login, Authorization, explicit no-cache and Range each invoke origin. Private Set-Cookie origin invokes origin every request including approved analytics cookies. Unit tests cover prepublication fallback, publication from analytics request, cookie-free/different/duplicate-analytics sharing, exact name case, ignore-common-off, custom ignore/forced bypass, and existing full cache gates. README now describes early allowlist parity rather than unconditional Cookie bypass. Cache authorization still depends on the existing contract that ignored cookies do not change public HTML.

## Verification

- `cargo test --locked -p ps-html --lib --test html_cache_cookies --test preload_critical_fonts`:204library+4cookieintegration+19fonttests pass (regressions.log).
- `NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx`: baseline28.56s; final27.91s; build logs retained.
- `tools/verify_early_html_cache.py`: before/after counter oracle extended via --analytics-early baseline/fixed, private controls and RUM retained; both final fixed runs passed.
- `tools/verify_beacon_reload.py --early-html-cache`: passed old-worker overlap, issued nonce/accept/replay, cache evidence/reload checks (beacon-reload.json/log). This preexisting reload oracle tests normal browser request, while new counter oracle tests approved-cookie request gating. No new browser runtime instrumentation test claimed.
- `git diff --check`, Python syntax: pass. No live config edit, reload, deployment, purge or push. Isolated processes stopped by harness finally blocks. Full HTML suite was1765passed at previous allocation milestone; not claimed rerun for this change.

Exact reproduction from repository root (NGINX binary at /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx):

```sh
python3 tools/verify_early_html_cache.py --nginx <matched-nginx> --module "$PWD/target/release/libps_nginx.so" --output docs/performance/analytics-early-hit/after-rum.json --rum-only --analytics-early fixed
python3 tools/verify_early_html_cache.py --nginx <matched-nginx> --module "$PWD/target/release/libps_nginx.so" --output docs/performance/analytics-early-hit/after-core.json --analytics-early fixed
python3 tools/verify_beacon_reload.py --nginx <matched-nginx> --module "$PWD/target/release/libps_nginx.so" --early-html-cache --output docs/performance/analytics-early-hit/beacon-reload.json
```

Baseline uses --rum-only --analytics-early baseline against module built from original runtime. Existing NGINX default error-log permission alert is retained in run logs; harness prefixes have their own active log and all probes/test assertions passed. No runtime error or failed config test hidden.
