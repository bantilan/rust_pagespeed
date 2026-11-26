# Ordinary HTML HIT publication-policy overhead — 2026-11-27

Source baseline: e5f238e. Local release microbenchmark; no live deployment, purge, configuration changes or reduction in optimization quality.

The ordinary HTML lookup previously built the early-lookup hint key and cloned/serialized origin headers before reading cached HTML. These are needed only when publishing a rewritten MISS. The new path returns after successful validation; absent and invalid entries retain publication preparation and arming. Early lookup itself is unchanged. This cannot explain or fix pending-resource BYPASS delays, and does not accelerate an already successful early HIT.

## Reproduction

```bash
cargo test --locked -p ps-html --lib html_cache
# Before source changes, enable HtmlCacheEarlyLookup in the existing benchmark fixture:
cargo run --locked --release -p ps-html --example font_lookup_latency
# Preserve the resulting baseline executable locally for alternating trials.
cargo test --locked -j 2 -p ps-html
cargo test --locked -j 2 -p ps-html --lib html_cache
cargo run --locked --release -j 2 -p ps-html --example font_lookup_latency
```

The example uses an in-memory cache, 50 warmups and 1,000 measured ordinary HITs per font-option setting, records median elapsed microseconds and global allocator calls, and asserts every lookup is a HIT. Its extra font-discovery fixture timings are unrelated to this change. Font on/off means configuration flags on the cached lookup; filters do not run on HIT. Early lookup must explicitly be enabled to exercise the removed hint preparation. It remains off by default in RewriteOptions.

The initial full-suite run was interrupted during compilation; `after-tests.log` is incomplete and is not passing evidence. `after-tests-resumed.log` is the resumed full suite. Additional cache regression covers a corrupt HTML entry followed by repair and early HIT publication. The initial unpaired benchmark may include host-load variation; prefer alternating retained executables for timing comparison. Timing/allocation numbers here cover driver creation + ordinary lookup, not NGINX, TLS, disk/shared caches, resource fetch or total production latency.

## Results

Three serial paired runs, alternating order (before/after, after/before, before/after), after compilation completed. Each table timing is the median of three per-run medians; allocation counts were identical in all three runs. Raw per-run CSVs and alternating.json are adjacent.

| Ordinary HIT fixture | Before | After | Reduction | Allocator calls before → after |
| --- | ---: | ---: | ---: | ---: |
| Font preload disabled | 16.511 µs | 10.410 µs | 37.0% | 571 → 406 |
| Font preload enabled | 17.402 µs | 10.990 µs | 36.8% | 576 → 411 |

Savings are approximately 0.006 ms per fixture HIT. These do not predict the same percentage improvement in full HTTP latency. Absolute times vary across individual trials; the consistent 165 fewer allocator calls are stronger evidence of work removed. No filters execute in either cached case, and no output/quality changes were made.

- Baseline targeted cache tests: 25 passed.
- Resumed complete `ps-html` suite: 1,775 passed across 64 test summaries, no failures. The new regression was added during compilation and is verified separately below, not claimed to be present in this full-suite run.
- Final targeted cache tests: 26 passed, including corrupt-entry repair followed by early HIT.
- Release example built and all HIT/output assertions passed.
- Matching NGINX 1.31.10 release module built successfully:

```bash
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
python3 docs/performance/html-hit-policy-20261127/alternate.py
```

To rerun alternating trials, preserve a baseline executable named `before-bench-local` beside the script: build e5f238e with the four-line benchmark-fixture change enabling HtmlCacheEarlyLookup, then copy its release example executable before building the new source. This temporary binary is intentionally excluded from the commit; raw measurements remain persisted.

No production measurements, filter-stage CPU profiling, NGINX integration timing, or cold-cache comparison were performed for this milestone. The latest live baseline remains ../live-20261127/RESULTS.md. Recurring `resource-optimization-pending` BYPASS needs separate resource-level investigation. The proposed redundant NGINX body copy change was not implemented in this milestone.
