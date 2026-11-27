# Stable effective critical-image evidence — 2026-11-27

Baseline:9941683. No deployment, live requests, cache purge or production latency measurement. User reports evidence-change BYPASS/resource-pending cycles and asks whether one valid report suffices for HIT eligibility.

One accepted nonce-validated report already makes critical-image evidence Available (`valid_beacons > 0`). Three accepted reports control transition to lower-frequency sampling, not availability. This milestone preserves that distinction and sampling cadence.

## Cause and repair

HTML keys and the lookup-versus-render publication check previously serialized the entire evidence record: pending nonces, scheduling timestamps, counts, generation, raw support and candidate registrations. Those mutate without changing filters' effective critical-image set or LCP decision. For example adding an unobserved zero-support candidate could prevent publication, and receiving an identical valid report unnecessarily invalidated completed HTML.

Added a separate canonical `html_evidence_identity`: available/unavailable, sorted effective critical-image keys, LCP kind and loaded URL. It uses a distinct version marker. Both HTML identity (therefore ordinary and early lookup) and beacon-filter snapshots use this representation. Persisted evidence codec, nonce validation/replay/expiration, decay/support, scheduling, minimum accepted evidence, learning filters and resource queues remain unchanged. The full record is still stored; only cache invalidation follows effective decisions. Missing/corrupt evidence remains the default unlearned identity. Actual changes in learned output invalidate normally.

A single valid report can support a completed cacheable rewrite between sample requests. This is not a permanent-HIT guarantee. Sampling requests still bypass and never cache nonce-bearing HTML. If effective evidence changes during a render, publication is rejected. `resource-optimization-pending` still blocks publication until background work is ready; this milestone does not identify the repeating detached resource or bypass optimization quality. Pending resources and invalid evidence cannot be fixed by arbitrarily declaring the page cacheable.

## Verification

```bash
cargo test --locked -j 2 -p ps-html --lib --test html_cache_vary
cargo test --locked -j 2 -p ps-html --lib --test html_cache_vary --test critical_images_beacon --test html_cache_cookies
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
```

Baseline213HTML+8Vary pass. Final215HTML+6criticalbeacon+7cookie+8Vary pass. NGINX294pass; matching1.31.10release module built successfully. Logs adjacent.

Deterministic comparisons instead of production-latency claims:

| Mutation | Previous behavior | New verified behavior |
| --- | --- | --- |
| Repeated accepted report, identical effective decisions | HTML MISS | existing HTML HIT |
| Register zero-support candidate / nonce bookkeeping during complete render | evidence snapshot mismatch, no publication | publication and later HIT |
| Effective critical set changes | invalidates | invalidates |
| LCP kind/URL changes | invalidates | invalidates |
| One accepted report | Available | Available |
| Sampling due / sampled nonce HTML | bypass | bypass |
| Detached resource rewrite | no publication | no publication |

Tests explicitly assert unavailable-to-available identity change, counter/schedule/nonce/support-scale stability, threshold/LCP invalidation, correct normal HTML HIT reuse after second accepted report and complete render publication with candidate bookkeeping. Existing Vary/detached/slow-fetch/sampling/non-beacon/private/cookie tests remain passing. No latency benchmark or whole-site BYPASS elimination is claimed. Earlier font evidence identity remains a separate input and can still legitimately invalidate. README/ledger document limits and installation resume point.
