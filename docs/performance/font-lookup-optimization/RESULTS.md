# Font lookup allocation profile and latency comparison — 2026-11-26

Baseline source: `0bd1770`, measured before runtime edits. Change: borrow DOM tag spelling and decoded class/id values, and allocate owned selector tokens only when first inserted. Preserve ASCII tag lowercasing, exact class/id matching, decoding/UTF-8 rejection, collection caps, document isolation and all cache/discovery/preload/beacon logic. No filter disabled, no new memory cache or freshness shortcuts, no external resource processing removed. Additive borrowed DOM API leaves existing owned API intact.

## Benchmark

Release-mode `font_lookup_latency` example, local CPU, memory metadata/HTTP caches, synthetic1200-element/600-card font-bearing HTML. Repeated case has common card/title/active classes; unique case adds600different classes. CSS declares/uses Manrope; every cold/warm output asserts exactly one emitted font preload; writer-only baseline asserts zero. Cold creates a fresh ServerContext/cache before each timed render (setup excluded); warm primes decision metadata once. This cold phase is cold font metadata with inline CSS, not a cold network/resource fetch. Both create a request ResourceDriver and parser per render. No fetch/network/I/O; these are not production or Nginx measurements. HTML HIT uses a separately seeded HtmlCacheSession with fresh ResourceDriver each lookup, font option OFF/ON, and asserts HIT; no parser/filter construction on that path.

300measured renders after50warmups per fixture/phase. HTMLHIT1000measured after50warmups. Report medians. A global counting allocator counts allocations/reallocations; callback timers isolate element collection and end-document decision. All reported times include profiler overhead (timer calls and allocator atomics), so do not extrapolate absolute times to production. Raw CSV intentionally retains writer-only OFF rows for noise assessment. These are sequential local runs, not a statistically controlled CPU-pinned A/B trial.

| Workload | Total before → after | Element walk before → after | Walk allocations before → after |
| --- | ---: | ---: | ---: |
| repeated / cold | 1.661 → 1.578 ms | 302.1 → 193.9 µs | 7987 → 1987 |
| repeated / warm | 1.626 → 1.536 ms | 299.8 → 191.6 µs | 7987 → 1987 |
| unique / cold | 1.546 → 1.524 ms | 223.3 → 193.2 µs | 6794 → 2593 |
| unique / warm | 1.503 → 1.481 ms | 224.8 → 192.7 µs | 6794 → 2593 |

Repeated-class warm walk time fell36.1%, walk allocations75.1% (6000 fewer per request); full profiled warm rewrite fell5.5%. Unique-class warm walk time fell14.3%, allocations61.8%; total fell1.5%. These results identify a concrete allocation reduction; whole-request timing remains sensitive to scheduling. Writer-only repeated baseline changed1.450→1.314ms despite no writer change, so whole-pipeline deltas cannot be attributed entirely to this patch. End-document decision replay remains about15µs repeated and50–54µs unique: this patch targets the element walk, not metadata lookup.

HTML-cache HIT OFF6.201→6.252µs, ON7.354→7.524µs. Essentially unchanged; the optimized DOM walk does not execute on a HIT. No cache-HIT speedup claim. The enabled font evidence lookup remains in the HTML-cache key for correctness across beacon updates. Live earlier measurements at font-preload-ab-20261126 use older source and include PHP/TLS/Nginx, and are not an after measurement for this patch. No production installation/config change in this task.

## Commands and verification

```sh
cargo run --locked --release -p ps-html --example font_lookup_latency > docs/performance/font-lookup-optimization/before.csv 2> docs/performance/font-lookup-optimization/baseline-build.log
# Apply runtime patch, then repeat identical harness:
cargo run --locked --release -p ps-html --example font_lookup_latency > docs/performance/font-lookup-optimization/after.csv 2> docs/performance/font-lookup-optimization/after-build.log
cargo test --locked -p ps-html --test preload_critical_fonts
cargo test --locked -p ps-html --lib --test preload_critical_fonts
cargo check --locked -p ps-nginx
cargo test --locked -p ps-html
```

Baseline18fonttests pass. Initial after203library+18fonttests pass; added regression brings font tests to19passing. Regression checks mixed-case known/custom tags, repeated/entity-escaped classes and IDs, exact class/id case, changed selector evidence invalidating positive/negative decisions and subsequent cache replay. Existing tests cover actual TwentyTwentyFive variable fonts/custom properties, beacon script containment, pending loads/imports, preloads cap/pruning and cache replay. Nginx check passed2.09s. Full-suite result recorded in full-html-tests.log and implementation ledger after completion. No new per-request allocations/statistics registrations beyond the profile harness (not linked into production).

Remaining separate work: production stage profiling for PHP/origin vs Rust, isolated matched-Nginx benchmarks and browser font-loading benefit on newly installed build. The earlier request-header HtmlCache:off early-HIT observation is not changed here. Existing automatic image-sizing implementation resume remains separate.

Full `cargo test --locked -p ps-html` passed: 1765 tests, 0 failures, 0 ignored across 64 test-result groups (including doc-test group). Existing compile warnings retained in raw log.
