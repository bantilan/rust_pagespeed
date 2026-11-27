# Cross-page known-important CSS — 2026-11-27

Baseline5d15a83. User explicitly requested positive stylesheet classifications
propagate to other pages. Local only, no deployment or push.

## Implemented behavior

Default-on CriticalCssKeepBlocking (Directory, no query override) is effective-
options-signature-bearing. Existing selector extraction and other filters remain
available via off. Only positive results from the trusted installed selector
finder can register a file; no arbitrary URL/browser list bypasses validation.

A stylesheet with a nonempty critical subset remains a full ordinary blocking
link on the learning page. Later pages with the same URL and validated response
contents leave it in situ, outside delayed bundles, before parsing its selectors.
No safe-to-defer classification is propagated. Inline styles are unchanged.

Shared metadata table is scoped to page origin/effective options. Entries are
content+URL fingerprints and expiry, bounded to128entries/16KiB, with origin
response expiry and1h maximum. Eviction chooses earliest expiry. Missing, corrupt,
expired or changed-version entries cannot authorize reuse. Transactions use the
existing backend update protection; contention skips persistence without waiting.
No additional resource fetching/logging or new background queues were added.

The active bounded registry snapshot joins existing CSS HTML evidence identity
and publication checks: normal and early caches rekey when another page learns,
when entries expire or are evicted/purged. This conservatively rekeys all pages
with the same origin/options using critical CSS, including pages without that
file. First learning during render may reject that publication; next request
settles under updated identity. This does not remove sampling/privacy/expiry/RUM
or pending-resource guards. No extra browser header variant is introduced.

Important remaining limit: automatic selector discovery/combined browser learning
is unfinished (critical-css-learning/PLAN.md). This registry is functional when
trusted selector evidence is installed; it cannot infer importance from absent
evidence. External resource validation/loading is the existing path, not a new
metadata-only implementation. Same-version recognition skips selector reparsing
and deferral, but no claim of zero CSS-byte reads is made. The new positive-file
policy deliberately defers fewer sheets than original C++ per-page extraction.

## Verification / resumable checklist

- [x] Plan and baseline before edits: selector24+HTMLCSS4 pass; baseline cache-API
  fixture retained previous5d15a83 release executable; raw CSV/log persisted.
- [x] Config97, HTML228 library, selector25, HTMLCSS5, criticalimage6, cookie7,
  Vary8 pass; NGINX295 pass.
- [x] Shared registry tests: URL/content/origin/config isolation, expiry/purge,
  malformed/over-limit entries, cap eviction, concurrent64positiveupdates.
- [x] Actual filter two-page test: known file stays blocking, independent file
  still follows its own page decision, explicit optout restores old behavior.
- [x] HTML normal/early cache rekey and mid-render registry change protection.
- [x] Same-URL changed-version filter extension test.
- [x] Matched release, fullHTML, actualNGINX/RUM and final cache-API measurements.
- [x] Final ledger/docs/results; verified milestone ready for local commit.

Commands (repository root):

```sh
cargo test --locked -j 2 -p ps-html --test critical_selector --test html_cache_critical_css
target/release/examples/critical_css_cache_latency
cargo test --locked -j 2 -p ps-config -p ps-html --lib --test critical_selector --test html_cache_critical_css --test critical_images_beacon --test html_cache_cookies --test html_cache_vary
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
cargo run --locked --release -j 2 -p ps-html --example critical_css_cache_latency
cargo test --locked -j 2 -p ps-html
cargo test --locked -j 2 -p ps-html --test critical_selector known_important
git diff --check
```

Legacy exact-output selector fixtures explicitly turn this policy off to continue
verifying original C++ extraction semantics. New integration tests exercise its
default-enabled implementation. New files scoped-rustfmt; no whole-tree formatting.
Final results and current next action recorded below when commands complete.

Latency refinement: initial full128registry API lookups measured110/115us while
compilation ran concurrently. Repeating parse/sort/encode and adding large
registry data to every HTML key was unnecessary. Added bounded8record
worker-local parsing cache using exact shared bytes+key+expiry, retaining a
shared read every lookup (no coherence polling window). Registry digest now
compact in HTML identity. Expiry, changedsharedbytes, purge andeviction invalidate
the local parse immediately on the next lookup. Focusedtests pass afterchange.
Finalfullsuite/matchedbuild/idlebenchmark pending; see resumeledger.

Optimized focused tests228library/selector25/cacheCSS5 pass. Under concurrent
compilation, 128entry lookup medians decreased from110.125→17.442µs ordinary
and114.774→24.055µs early; no whole-request/production speedup claim. Final
idle measurements are required below. Final matchedrelease passes74s; final
NGINX tests/oracle wait on fullsuite buildlock. Existing initial matchedbuild
38.07s/realNGINXoracle passed before the local parsing optimization.

## Final verification and measured limits

Full finalHTML **1,801pass across65resultgroups**, exit0. Config97 and finalNGINX295
pass. Final matchingNGINX1.31.10release74s pass (compile ran concurrently with
other checks). Actual finalNGINX origin-counter/RUM oracle passes20warmHITs,
20analyticsHITs and10ChromeFirefoxHITs with no extraorigin calls, fullstyles and
RUMbrowserclock intact; auth/cookie/SetCookie/range/no-cache controls pass.
Warmmedian0.751ms, analytics0.717ms in this synthetic25msorigin fixture; no live
latencyclaim. Full tests include final sameURLchangedbytes filter guard.

Final cache-API executable run with build jobs finished (idle-final.csv):

| Fixture | Ordinary median | Early median | HITs/path |
|---|---:|---:|---:|
| CSS off |13.194µs|19.847µs|2000/2000|
| CSS on, empty registry |17.022µs|23.544µs|2000/2000|
| CSS on, 1 known sheet |16.871µs|23.464µs|2000/2000|
| CSS on, 128 known sheets |17.332µs|23.854µs|2000/2000|

128vs1entry costs0.461µs ordinary/0.390µs early in this run. Compared CSS-off,
CSS-on adds3.8/3.7µs (includes selector identity and registry work). Historical
baseline css-off6.232/9.408µs differs substantially from final13.194/19.847µs,
so raw before/after wall timings are noisy and cannot support causal production
claims. Matched final fixtures and before/after fullregistry parsing costs are
reported with that limitation. Measurements exclude network/parser/filter work,
use reuseddriver/memorycache32KiBbody,50warmups+2000lookups/path, no livepurges.

Final oracle command:

```sh
python3 tools/verify_early_html_cache.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/critical-css-keep-blocking/nginx-final.json --rum-only --critical-css --browser-groups --analytics-early fixed
target/release/examples/critical_css_cache_latency > docs/performance/critical-css-keep-blocking/idle-final.csv
```

Initial sameURLversion test extension borrowed a temporarydriver; corrected by
binding thedriver and selector25test rerun passed. No production alterations,
remote push or automatic collector activation. All commands complete. Next
resume automatic CSS learning Phase2 using critical-css-learning/PLAN.md; reuse
this positive registry when authoritative learned CSS evidence becomes available.
