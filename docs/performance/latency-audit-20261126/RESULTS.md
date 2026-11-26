# Latency audit — 2026-11-26 (Asia/Manila)

User asked to find further latency improvements. Read-only live request audit plus local cache microbenchmark; no runtime application edits, deployment, remote config changes, reload, purge or private-cookie probing. Source inspected at732d534; reported live server header remains nginx/1.31.10+183c879, not proof of module binary/source. The previous selector-allocation change is still local-only unless independently installed. This audit does not claim an implemented speedup.

## Priority 1: allowlisted cookies lose early HTML cache HIT

Source: `crates/ps-html/src/html_cache.rs::early_key` rejects any Cookie header. Ordinary eligibility uses cookie_verdict/evaluate_cookie_headers, accepts configured ignored/analytics cookies, and excludes their values from cache keys. Thus the early and ordinary gates disagree. NGINX module calls lookup_early before the content handler; filters perform ordinary lookup on origin response headers. This explains an apparently fast HIT marker accompanying an origin round trip for analytics-cookie users. Source attribution is supported by paired timing, not a PHP CPU trace.

Read-only paired test: same page/UA/Accept/identity encoding, loopback HTTPS with SNI, persistent HTTP/1.1 connection, phases randomized each iteration,5warmup+20measured per page/phase. Cookie variant sends only `_ga=GA1.1.123.456`. All measured responses HTTP200 and HIT, including cookie variants.

| Page | No-cookie HIT median | `_ga`-only HIT median | Difference |
| --- | ---: | ---: | ---: |
| `/` | 1.92 ms | 62.72 ms | +60.79 ms |
| `/hello-world/` | 2.67 ms | 71.37 ms | +68.70 ms |
| `/sample-page/` | 3.14 ms | 57.14 ms | +54.00 ms |

Recommended next implementation: reuse existing configured cookie_verdict in early_key, with cookie-independent hint key only after that verdict passes. Preserve Cookie2, malformed/unknown/session/login cookies, Authorization, private/Set-Cookie origin policy, request override/no-cache gates, RUM sampling/evidence and origin-policy TTL/invalidation. Add regression tests for early vs ordinary allowlist parity, malicious/mixed/duplicate cookies, private origin and changed options; isolated matched-NGINX delayed-origin fixture must prove content handler bypass and beacon behavior. No automatic blanket cookie ignoring or FastCGI public cache enabled.

## Priority 2: origin/PHP latency dominates uncached requests

Separate randomized5warm+20measured case pairs normal/unique-header/PageSpeed=off. Unique X-PS-Latency-Audit header changes only whole-HTML identity; every measured forced variant verified MISS. All PageSpeed=off responses lacked cache marker;200. No deliberately cold resource cache or full page browser loads.

| Page | Normal HIT | Forced HTML MISS | PageSpeed disabled |
| --- | ---: | ---: | ---: |
| `/` | 1.55 ms | 69.20 ms | 63.39 ms |
| `/hello-world/` | 1.48 ms | 79.99 ms | 73.70 ms |
| `/sample-page/` | 1.36 ms | 57.60 ms | 54.01 ms |

Inference: much of uncached latency lies outside PageSpeed rewriting, since disabling PageSpeed still takes54–74ms. Differences between these interleaved medians (~3.6–6.3ms) are not additive profiled Rust CPU time: origin responses and scheduling vary. Next measure FPM/WordPress request timings, OPcache/object-cache hit rates, actual cache-bypass causes, and origin response generation. Do not attempt to fix54ms of origin processing via microsecond font hash changes. Public network/TLS excluded by loopback persistent connections.

## Priority 3: repeated missing metadata falls through to disk

Code inspection: WriteThroughCache backfills L1 only on successful value reads; absent confirmed-font evidence therefore cannot become a memory hit. Every enabled-font HTML cache identity reads the confirmed key. Local `missing_metadata_cost` wraps FileCache with a read counter plus bounded1MiB LRU.10000repeated missing-key lookups caused10000FileCache::get calls (~2.772µs/lookup);10000seeded memory-hit lookups caused0disk reads (~0.039µs/lookup). This local no-coherence microbenchmark identifies a mechanism, not syscall-profiled production counts or a proven production gain. Coherent branch similarly backfills only values; no production shared backend simulation claimed.

Potential implementation: bounded generation-validated negative entries with expiration, restricted to trueNotFound, invalidated by cross-worker writes/deletes/restart, and safe fallback on coherence uncertainty. Preserve external mutation/purge behavior; no indefinite negative memoization or suppressed new font evidence. Benefit likely microseconds per absent lookup; lower priority than the measured analytics-cookie issue. Existing font evidence lookup must remain in HTML identity to version cached preloads when beacon evidence changes.

## Reproduction and verification

```sh
ssh -o BatchMode=yes -o ConnectTimeout=8 root@157.245.148.68 'python3 -' < docs/performance/latency-audit-20261126/probe.py > docs/performance/latency-audit-20261126/live.json
ssh -o BatchMode=yes root@157.245.148.68 'python3 -' < docs/performance/latency-audit-20261126/cookie_probe.py > docs/performance/latency-audit-20261126/cookies.json
cargo run --locked --release -p ps-cache --example missing_metadata_cost > docs/performance/latency-audit-20261126/missing-metadata.csv 2> docs/performance/latency-audit-20261126/cache-probe-build.log
```

All commands completed successfully; measured live responses200 with classifications above. Local example asserts missing/present states and removes only its private PID-named temporary fixture. Diagnostic Python syntax and git diff checked; no application source change requiring full suite. Raw per-request samples, status/cache flags and build log retained. The requests warm caches and bounded unique-header variants; no cache clearing or persistent server config writes. No profiler attached to live workers. No remote commands intentionally left running.
