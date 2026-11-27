# Phase 1 — combined CSS/LCP evidence foundation

2026-11-27, source baseline a81740e. No deployment, push, automatic collector or
resource-fetch behavior change. The backend is intentionally inactive until a
server-side producer registers complete CSS candidates and a combined browser
report is implemented in subsequent phases (PLAN.md).

Implemented:

- Bounded sorted candidate set and optional matched decision. None differs from
  accepted empty. Limits: 2,048 selectors, 1,024 bytes each, 32KiB aggregate; codec
  and wire are capped. Invalid data rejects the whole input, never truncates.
- CSS stored inside existing critical/LCP evidence as an optional codec extension.
  No extra cache variant, nonce store or independent collection timer.
- Server candidate registration retires old nonces on changed inputs; a settled
  single decision cannot be reopened. Old records with no CSS decode unchanged.
- Report validation requires CSS when the registered cohort requires it, and
  every reported selector must be a registered candidate. Incomplete CSS cannot
  finalize an LCP-only result. One existing transaction accepts both together.
- Strict URL-component decoding splits original PageSpeed's cs comma delimiters
  before decoding components, preserving a comma inside :is(.a,.b). Malformed
  encoding/UTF-8, duplicates of the cs field, empty components and limits fail
  closed. Empty cs is valid and distinct from absent.
- Receiver retains original HTML bvg cohort and nonce validation; one accepted
  result clears pending competitors, and replay cannot replace it.

Compatibility: optional extension only; old binary readers ignore extensions,
so activation MUST version effective options/signature before any producer uses
it across reloads. That prerequisite remains in Phase3 of PLAN.md. The existing
CSS asset alone sends no LCP, so it cannot finalize a combined report; Phase3
must implement one combined collector, rather than simply inserting that asset.
The metadata-backed finder and HTML evidence activation are intentionally pending.

Tests passed:

- Baseline finder 16; final HTML library 225 (four new tests).
- Critical-image6, HTML CSS cache4, Vary8, LCP26 integrations.
- Additional cookie7 and selector24 integrations.
- Strict query codec5; NGINX library295 (one new receiver test).
- Includes existing multiprocess transactional evidence test.

Commands, working directory repository root:

```sh
cargo test --locked -j 2 -p ps-html --lib beacon_finder
# retained release executable built at a81740e, before this implementation:
target/release/examples/critical_css_cache_latency
cargo test --locked -j 2 -p ps-html --lib
cargo test --locked -j 2 -p ps-html --test critical_images_beacon --test html_cache_vary --test prioritize_lcp_images --test html_cache_critical_css
cargo test --locked -j 2 -p ps-html --test html_cache_cookies --test critical_selector
cargo test --locked -j 2 -p ps-http --lib query_params
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
cargo run --locked --release -j 2 -p ps-html --example critical_css_cache_latency
git diff --check
```

Raw logs/CSV in this directory. Initial codec compile used .ok() on an Option;
corrected and all listed tests rerun successfully. Full workspace not rerun;
this milestone does not activate the producer/filter. No production latency or
rendering improvement is claimed. Cache-API fixture has 50 warmups +2,000 measured
lookups each path, 32KiBbody, memory cache and reused driver; excludes parser,
fetching and network. Final build/fixture completion recorded below.

Next actionable task: Phase2 complete authorized stylesheet selector summaries,
using metadata-only HTML reads and bounded background dedup/retry. Keep full CSS
until all inputs required for a combined decision are ready. Do not activate the
collector or claim automatic learning before Phases3–5 are verified.

Completion: matched NGINX1.31.10 release PASS (25.17s), final cache-API run
completed with 2,000/2,000 HITs in all four cells; raw final CSV retained. This
receiver-only foundation does not alter the enabled HTML-cache algorithm, so
these single-run lookup timings are a regression check, not a speedup claim.
All commands complete and diff check passes. Milestone ready for local commit.
