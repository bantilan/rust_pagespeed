# Pre-origin HTML-cache lookup

Implemented as opt-in `pagespeed HtmlCacheEarlyLookup on;` and enabled on rustpagespeed.com on 2026-11-25. Default remains off elsewhere. The ordinary header-stage lookup and every enabled filter remain active on misses.

## Architecture and correctness

The existing body key depends on origin response headers. A bounded (<=64KiB) origin-policy hint now supplies those inputs before origin execution. Hints are published only after a completed eligible body publication, use the existing metadata cache/backend, and carry the same creation/expiry envelope. Their keys include URL, cache fragment, effective options signature, request headers, and current file mtime. Only regular files provide mtime identity; directories no longer stand in for a resource.

An early request must be a main GET. Cookies (including analytics cookies), authorization, conditional/range headers, request Vary, Cache-Control and Pragma bypass early serving. Experiment cookie creation also bypasses. NGINX PRECONTENT runs after its access phases. The feature cannot be enabled by query options. Existing eligibility, origin cookie/privacy prohibitions, TTL, purge, body validation, beacon evidence/versioning and sampling gates are rerun using current request/options/evidence. Missing, corrupt, expired, unavailable or differently keyed metadata falls through. A hint cannot independently authorize serving. Body and hint are separate writes; partial writes safely fall through rather than requiring atomic cross-entry publication.

Only bodies that already passed existing publication checks can be served. Detached/incomplete rewrites and sampled beacon nonce bodies do not publish. Early hits construct no rewrite filters, replay origin representation headers with stale encoding/framing/validators removed, add private/no-store downstream policy, append a fresh diagnostic when enabled, and negotiate Zstd for this request. Cached RUM JS remains executable per navigation; browser-side `Number(new Date())` is retained rather than freezing a server timestamp.

**Freshness contract:** early serving trusts the saved origin policy within its lifetime instead of rechecking PHP headers every request. Application content/header/access-policy changes not represented in the identity require purge or expiry. This is an opt-in extension to the original header-stage C++ behavior. Do not enable it on application routes whose authorization or side effects require executing PHP every time. Current site's existing HtmlCacheTtlSec3000 was preserved (up to50minutes before expiry). No WordPress automatic invalidation plugin was added.

## Production measurements

Same module, sequential off/on configuration comparison on the server. Three warmup requests per encoding/mode were recorded separately, followed by thirty fresh curl HTTPS connections per encoding/mode, all asserted HIT. Thirty reused TLS requests per mode followed one connection warmup, also asserted HIT. `curl --compressed` verified Zstd decoding; all measured HTTP responses succeeded. Warmup MISS/transition rows are preserved in the raw records and excluded from measured HIT samples. Ordering was off then on, one pair, not randomized or a concurrency/load test.

| Server-local HIT | Early off median | Early on median | Off p95 | On p95 |
|---|---:|---:|---:|---:|
| Fresh TLS, identity |73.876ms|14.383ms|84.472ms|22.716ms|
| Fresh TLS, Zstd |72.363ms|14.497ms|87.386ms|15.782ms|
| Reused TLS, identity |58.026ms|0.878ms|—|—|

Fresh-process curl timings include client/TLS setup and server processing; keepalive elapsed includes full response read. These are not separate filter/metadata CPU profiles. The dominant measured improvement is avoiding PHP on eligible HITs; an independent synthetic FastCGI request-counter oracle proves origin skipping. Production timing alone does not count PHP invocations.

Additional checks:

- New query MISS77.667ms, subsequentHIT22.770ms.
- Fake WordPress logged-in cookie: BYPASS78.654ms.
- Cache-Control:no-cache request reached the ordinary path at73.182ms.
- Public-network10ZstdHITs median339.041ms; network variation prevents attributing its difference from prior385ms solely to this change.
- No lingering FastCGI connections; both services active; no PHP pool-size or timeout increases.

Final measured/installed module SHA256 `452a423e6e992f57d71c5bb3db48f2245d81151b5a20dc580362eaff0e0bcb70`. Final review added explicit Available-state checks for both metadata reads; scoped tests, release, isolated oracles and the full off/on measurement above were rerun on this final build. Final-off.jsonl/final-on.jsonl contain the authoritative controlled comparison. Earlier live-off/on.jsonl are preliminary: identity groups had28HITs, oneMISS and one unmarked transition response; Zstd groups29HITs/oneMISS. Their HIT medians were71.883/73.097ms(off) and14.884/14.587ms(on); these records are retained rather than silently blended with final warm samples.

## Tests and reproducibility

- Config89library tests passed (including defaultoff and forbiddenqueryenablement).
- HTML171library tests passed, including18cache tests for hint publication, corruption, expiry, mtime, variant/options separation, cookie/auth/range/cache-control/conditional bypass and purge. Final Available-state guard rechecked with all18cache tests.
- NGINX292library tests passed serially.
-34integration tests across add_instrumentation, critical_images_beacon, html_cache_cookies and html_cache_vary passed.
- Real isolated NGINX/FastCGI counter oracle: one MISS then20HITs with zero additional origin calls; bypass requests increment origin counter; Set-Cookie responses never publish. Normalized body hashes match.
- Separate RUM-only fixture enables add_instrumentation with a browser UA; cached clock JS and body hash retained on20HITs with no additional origin calls. This fixture isolates RUM; production filters were unchanged. With CoreFilters/browser UA, nonce-bearing sampling correctly bypasses publication instead of producing a cacheable sample.
- Both actual Zstd beacon reload orderings (accept before/after reload) passed with early lookup enabled, preserving evidence/replay protection.

```bash
cargo test -p ps-config --lib
cargo test -p ps-html --lib -- --test-threads=1
cargo test -p ps-html --lib html_cache::tests -- --test-threads=1
cargo test -p ps-nginx --lib -- --test-threads=1
cargo test -p ps-html --test html_cache_cookies --test html_cache_vary --test add_instrumentation --test critical_images_beacon
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
```

Final release build27.22s. Local NGINX binary for following tools: `/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx`.

```bash
python3 tools/verify_early_html_cache.py --nginx "$NGINX" --module target/release/libps_nginx.so --output /tmp/early-core.json
python3 tools/verify_early_html_cache.py --nginx "$NGINX" --module target/release/libps_nginx.so --output /tmp/early-rum.json --rum-only
python3 tools/verify_beacon_reload.py --nginx "$NGINX" --module target/release/libps_nginx.so --output /tmp/early-beacon.json --zstd --early-html-cache
python3 tools/verify_beacon_reload.py --nginx "$NGINX" --module target/release/libps_nginx.so --output /tmp/early-beacon-pre.json --zstd --early-html-cache --accept-before-reload
```

Set `NGINX` to the binary path above. Isolated startup may emit a harmless default error-log permission alert before reading the private configuration; exit codes/assertions and private error log determine success. Tests do not alter the installed service.

To repeat the production timing probe, run `tools/benchmark_wordpress_html_cache.py` on the server (via SSH stdin or copy); it only reads the site. Controlled results used the same script once with HtmlCacheEarlyLookupoff and once with on, validating/reloading NGINX between modes. Restore on afterward. Raw final-off.jsonl/final-on.jsonl, preliminary live-off.jsonl/live-on.jsonl and live-public.json are saved beside this report.

## Deployment and rollback

Installed NGINX1.31.10 module atomically via new inode/rename after backup. `nginx -t` passed, NGINX restarted to load module. Site enabled HtmlCacheEarlyLookup; all original optimization settings/TTL preserved. Module backup `/usr/lib/nginx/modules/ngx_pagespeed_rust.so.before-early-html-20261125`; site backup `/etc/nginx/rustpagespeed.com.before-early-html-20261125`.

To disable this feature without reverting the earlier FastCGI buffer fix, set HtmlCacheEarlyLookupoff, validate and reload. Full rollback can restore the saved module via a new inode/rename plus saved site configuration, validate, then restart. Existing Debian packages were not rebuilt here; rebuilding from the current source is necessary before future package installation to retain the new feature. No pushes.

## User-installed retest (2026-11-25 04:02 UTC)

Installed diskmoduleSHA unchanged452a423e...; HTTPServer reports1.31.10+a1e2f12 and earlylookup remainson. No server/config changes. Corrected benchmark to warm three requests for the Python keepalive header variant before asserting measuredHITs (initial attempt stopped on firstvariantMISS; its60successfulcurlHIT rows retained in installed-initial-partial.jsonl, excluded from finaltable).

| Confirmed HIT path | Samples | Median | p95 |
|---|---:|---:|---:|
| Local freshTLS identity |30|14.841ms|20.488ms|
| Local freshTLS Zstd |30|14.811ms|21.007ms|
| Local reusedTLS identity |30|0.863ms|—|
| Public freshTLS Zstd |10|302.184ms|—|

All100measuredresponses200/HIT; Zstd negotiated/decoded. Threewarmups perlocalgroupexcluded. FakeWPlogincookieBYPASS200at85.655ms. NoresidualFastCGIconnections, servicesactive, no newerpoolwarnings. LatestNGINXerrors inspected were403blockedscannerrequests, notupstreamtimeouts. Networksample timing is observational, not a controlled network performance comparison. Saved installed-retest.jsonl/installed-retest-public.json. No deployment or runtimecode changes.
