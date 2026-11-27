# Critical CSS and HTML cache — 2026-11-27

Source baseline: 58a1c92, clean working tree. Local implementation only; no live
configuration/module changes or pushes. The user reported repeated BYPASS after
enabling prioritize_critical_css. The old eligibility matrix rejected that option
unconditionally, independently of cookies, collector sampling or actual output.

## Implemented contract

- Supported clients can cache completed output with prioritize_critical_css on.
- Canonical sorted, length-framed selector evidence joins the existing HTML key,
  encoded as web64 to keep key fields unambiguous. It adds no browser group or
  header dimension beyond the existing evidence component.
- A request-local selector snapshot is used by the actual filter; a changed set
  at publication time rejects the store. New sessions clear old snapshots.
- Changed selectors invalidate ordinary and early lookup; set order does not.
- The normal driver now instantiates the existing CriticalSelectorFilter.
  Critical rules inline; the full stylesheet and its loader remain in output.
- Missing/empty evidence preserves the full original stylesheet and remains
  cacheable. Automatic selector collection is still not connected: this change
  does not claim to implement production automatic critical CSS learning.
- Unsupported clients cannot reuse another client's transformed CSS. Private,
  authentication, expiry, purge, detached-resource and sampled-collector gates
  remain active. RUM scripts retain their per-load browser clock on HIT.

Compatibility reference: local original C++ source
../mod_pagespeed/net/instaweb/rewriter/critical_selector_filter.cc:397–424,
DetermineEnabled: missing selectors or unsupported UA leave original styles
unchanged. The prior Rust blanket HTML-cache filter prohibition was not needed
for that behavior. This work preserves the C++ filter's CSS-selection/full-style
fallback behavior, not a claim of completed C++ beacon-collector parity.

## Baseline

Before implementation: cache library tests 30 pass. Cache-API fixture (32KiB
body, memory backend, reused driver, no parser/network/disk; 50 warmups and 2,000
measured lookups per cell):

| Option | Ordinary median | Early median | HITs per path |
|---|---:|---:|---:|
| CSS off | 6.473 µs | 15.499 µs | 2,000/2,000 |
| CSS on, no selectors | 4.949 µs | 4.057 µs | 0/2,000 |

The old BYPASS lookup is less work than delivering a cached body; this table is
not a whole-request latency comparison. Baseline raw files: baseline.csv and
baseline-build.log. Existing cookie benchmark was adapted into a separate
critical_css_cache_latency example without changing existing implementation.

The same isolated real NGINX/FastCGI oracle with the old release module
reproduced BYPASS: cold 32.52ms, full styles and RUM present. It intentionally
failed its expected-MISS assertion (nginx-baseline.log). Origin delay is a
synthetic 25ms. A startup alert about /var/log/nginx permission occurs before
the isolated config opens its own log; the process served the measured response.

## Validation

- HTML library: 221 pass; existing selector filter: 24 pass.
- New real-driver CSS/cache integrations: 4 pass (ordinary/early output, missing
  to learned evidence, input order, mid-render change, sampling/private/UA gates).
- Critical-image integrations 6, cookies 7, Vary 8 all pass.
- Cache library 74; pinned NGINX library 294 pass.
- Final matched release build, real NGINX final oracle, cache-API comparison and
  full HTML suite: see completion record below when finished.

Initial test fixture fixes: byte-vector show_comment decoded before string
matching; response Cache-Control suffix corrected from public to ', public';
expected minified blue is #00f. Logs retain initial failures and passing reruns.

Commands from repository root:

```sh
cargo test --locked -j 2 -p ps-html --lib html_cache
cargo run --locked --release -j 2 -p ps-html --example critical_css_cache_latency
cargo test --locked -j 2 -p ps-html --lib --test html_cache_critical_css --test critical_selector --test html_cache_cookies --test html_cache_vary --test critical_images_beacon
cargo test --locked -j 2 -p ps-cache --lib
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
python3 tools/verify_early_html_cache.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/critical-css-html-cache-20261127/nginx-final.json --rum-only --critical-css --browser-groups --analytics-early fixed
cargo test --locked -j 2 -p ps-html
python3 -m py_compile tools/verify_early_html_cache.py
git diff --check
```

Checklist:

- [x] Persist plan and baseline before implementation.
- [x] Evidence-aware cache and actual filter integration.
- [x] Focused output, evidence, privacy and compatibility regressions.
- [x] Documentation explains absent automatic collector honestly.
- [x] Matched build, final real NGINX origin-counter/RUM checks and comparison.
- [x] Full HTML suite, final persisted resume state; tested milestone ready for local commit.

## Final measured results

| Cache-API fixture | Ordinary median | Early median | HITs per path |
|---|---:|---:|---:|
| CSS off | 6.612 µs | 9.849 µs | 2,000/2,000 |
| CSS on, no selectors | 6.822 µs | 10.129 µs | 2,000/2,000 |

The feature changes from 0/2,000 HITs to 2,000/2,000. The on/off lookup difference
in the final run is 0.210µs ordinary / 0.280µs early. Single runs, same-process
fixture, no production attribution; do not compare the old cheaper BYPASS API
call directly against the new cached body delivery as an end-to-end speedup.

Matched NGINX 1.31.10 release: PASS, 40.77s. Full HTML suite: **1,792 passing
cases across 65 result groups**, exit 0. Scoped new-file rustfmt, Python compile
and git diff --check pass. Existing fixture/compiler warnings remain.

Actual isolated FastCGI checks (synthetic 25ms origin delay):

| Final fixture | Cold | Warm median (20 HITs) | Analytics median (20 HITs) |
|---|---:|---:|---:|
| CSS + RUM | MISS, 27.841ms | 0.286ms | 0.307ms |
| CSS + CoreFilters | MISS, 28.294ms | 0.355ms | 0.361ms |

Warm/analytics HITs do not increment origin count. RUM-mode additionally verifies
10 Chrome/Firefox HITs without origin calls, retained browser clock and complete
CSS; both modes verify navigation, private cookies, auth, origin Set-Cookie,
no-cache and range protections. These measurements are local, not live codefaq
latency. Raw JSON and logs are alongside this file.

An extra CoreFilters + browser-groups run correctly encountered critical-image
collector BYPASS on the fresh Chrome page: that harness does not submit the
browser's report. Its expected-MISS assertion was unsuitable for that sampling
case; no collector or filter was disabled to force the assertion. Cross-browser
sharing is covered by the RUM-mode test, and CoreFilters by the separate final
run. The failed run's graceful shutdown also timed out; only its isolated
process group was killed after matching its exact /tmp prefix. The oracle now
has a bounded force-stop fallback so this cannot mask the original assertion.

Additional final CoreFilters command:

```sh
python3 tools/verify_early_html_cache.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/critical-css-html-cache-20261127/nginx-core-final.json --critical-css --analytics-early fixed
```

Next action: install the tested source through the normal deployment workflow;
verify codefaq with its existing filter configuration and an accepted LCP report.
No production configuration/module change or push was performed here. Automatic
critical-selector collection remains unfinished; HTML-cache compatibility is
implemented and verified, including actual transformed output with an installed
finder. Do not describe that as completed automatic CSS learning.
