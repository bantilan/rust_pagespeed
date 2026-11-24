# PageSpeed Rust (`pagespeed-rust`)

A Rust port of [PageSpeed](https://github.com/apache/incubator-pagespeed-mod)
(PSOL optimization engine + `ngx_pagespeed` server module): HTML/CSS/JS/image
optimization, multi-tier caching, beacon-driven learning, admin pages, and a
dynamically-loaded NGINX module — memory-safe, fast, and behavior-compatible
with the C++ original.

## Status: serving production traffic

The module is installed and serving rewritten traffic (HTML, WebP/AVIF images,
proxy mode). The full workspace suite (97 suites) and live compatibility
checks against the C++ module pass. It is still maturing: keep backups of
your C++ configuration when switching over.

## Performance

Linux shared caching is available through explicit `CreateSharedMemoryMetadataCache FileCachePath size_kb` configuration. See [configuration, migration, and measured tradeoffs](docs/performance/shared-memory/CONFIGURATION.md). Initial activation from legacy workers requires a full stop/start; the shared tier is not enabled automatically. See the [measured latency results and profiling limits](docs/performance/shared-memory/RESULTS.md).

Measured on localhost (`curl %{time_total}`), Moto/Aroma template pages,
same filter config throughout. Full methodology and raw samples:
[`../benchmark.md`](../benchmark.md).

| Test case | C++ 1.15 (cold) | Rust before (`f03a58a`) | Rust now (`9063416`) |
|---|---|---|---|
| Moto first load | 2 ms | 86 ms | 6 ms |
| Moto repeat (HTML hit) | 3 ms | 7 ms | 2 ms |
| Aroma first load | 4 ms | 154 ms | 8 ms |
| Aroma repeat (HTML hit) | 3 ms | 7 ms | 2 ms |

Rewriting verified intact on warmed pages (26/32 `pagespeed` URLs, matching
the C++ 25–32 range): the speedup comes from deferred work, not disabled
filters. Component-level before/after numbers (filter init 1937→136µs,
signature 1220→59ns, 50-page cold pass with 20ms origins 515→3ms) live in
[`docs/ASYNC_REWRITING.md`](docs/ASYNC_REWRITING.md) and the
`timing_breakdown` / `async_workload` examples.

## Features vs C++ PageSpeed

Legend: ✅ supported · 🔶 partial · 📋 planned · ❌ not supported (out of scope)

| Area | C++ 1.15 | Rust | Notes |
|---|---|---|---|
| HTML rewrite engine | ✅ | ✅ | Streaming parser, plan-driven filter chain |
| Rewrite filters | ~100 classes | ✅ 60+ modules live | Full chain plan; unbuilt slots pass through (see `skipped_slots`) |
| Rewrite options & directives | ✅ | ✅ | `RewriteOptions`, scopes, nginx directive parsing |
| CSS minify / rewrite / combine / inline | ✅ | ✅ | Hand-written parser, no regex |
| JS minify / combine / inline / source maps | ✅ | ✅ | Lossless tokenizer, shared pattern table |
| Images: JPEG/PNG/GIF resize & recompress | ✅ | ✅ | |
| Images: WebP / AVIF conversion + negotiation | ✅ | ✅ | Format checks per request variant |
| Lazy-loaded images, deferred iframes | ✅ | ✅ | |
| Responsive images (`srcset`, `<picture>`) | ✅ | ✅ | |
| CSS/JS/HTML cache extension | ✅ | ✅ | |
| Domain rewriting, sharding, mapping | ✅ | ✅ | |
| Resource file cache + HTTP cache + purge | ✅ | ✅ | Byte-compatible entries, purge endpoint |
| HTML (whole-page) cache | ✅ | ✅ | Variant-aware identity, cookie allowlists |
| In-memory LRU tier | ✅ opt-in | ✅ **on by default** | 8 MiB/process, 16 KiB cap; explicit zeros restore C++ shape |
| IPRO (in-place resource optimization) | ✅ | ✅ | Recording, waiting, deadlines |
| Memcached / Redis backends | ✅ | 🔶 falls back | Warns once per path, uses file cache |
| Shared-memory metadata cache | ✅ | ❌ fails closed | Per-process LRU covers the hot path instead |
| Critical-image beaconing + LCP learning | ✅ | ✅ | Beacon finder, evidence store, LCP prioritization |
| Full RUM instrumentation filter | ✅ | 📋 code present | Filter implemented, chain wiring pending |
| **Automatic logo detection for LCP** | ❌ | ✅ **Rust-only** | Confidence-based (WP/custom/SVG/CSS-bg); keeps logos out of auto-preload |
| **LCP fallback hero** | ❌ | ✅ **Rust-only** | Empty/stale evidence never leaves the visual hero bare |
| Async HTML (deadline-detach) | ✅ threads | ✅ adapted | Fire-and-forget rewrite jobs; first view serves originals, next view hits |
| **Async CSS/JS combination** | ✅ threads | ✅ adapted | Background combiners with failure memory; uncombinable members never block HTML caching |
| **Background admin reporting** | ✅ | ✅ | Worker completions land on the admin images page |
| Admin / console / statistics / graphs | ✅ | ✅ | `ps-admin` + cross-process shared counters |
| Gzip handling, Vary, downstream caching | ✅ | ✅ | |
| Apache module | ✅ | ❌ | NGINX only |
| Envoy integration | ✅ | ❌ | |

### Deliberate differences from C++

- **LRU on by default** (C++: off). Same knobs, safer out-of-box latency.
- **Disabled HTML deadlines mean no waiting** (C++: unbounded wait). Only
  in-place serving with waiting keeps the unbounded path.
- **First cold view serves original URLs** while optimization runs in the
  background (C++ deadline-detach parity under load); warmed views are
  identical. Nothing is ever skipped — only deferred.
- **Logo-aware LCP** is new functionality with no C++ counterpart.
- **Combine failure memory**: uncombinable candidates (404s, uncacheable
  origins) rest on the input-errors TTL instead of re-detaching every
  render, so one bad asset can't disable HTML caching.

## Resource usage

Measured live (12 nginx workers): ~213 MB total RSS, 11–25 MB per worker
depending on cache warmth, against 15 GB system RAM. Per-worker memory is
dominated by the bounded 8 MiB LRU tier plus message buffers and the
rewrite thread pool; the file cache lives on disk. Regex tables compile
once at worker startup and serving filters construct lazily, so steady
requests carry no initialization overhead.

## Layout

Cargo workspace (`resolver = "2"`):

| Crate | Purpose |
|---|---|
| `ps-html` | Streaming HTML engine, filter chain, rewrite contexts, LCP/logo logic |
| `ps-css` | CSS parsing, minification, rewriting, combining |
| `ps-js` | JS tokenizing, minification, combining, source maps |
| `ps-image` | Image decode, analysis, optimization, WebP/AVIF conversion, sprites |
| `ps-cache` | Hashing, resource naming, LRU/file/compressed backends, TTL, purge |
| `ps-http` | Fetchers, HTTP cache, headers, cookies, downstream purge |
| `ps-stats` | Thread-safe statistics, histograms, shared-counter backend |
| `ps-config` | Rewrite options, filter state, scopes, directive parsing |
| `ps-admin` | Admin site, console JSON, statistics graphs, purge endpoint |
| `ps-nginx` | NGINX module: hooks, filters, config, cache assembly (FFI) |
| `ps-compat` | Compatibility harness: corpus + live checks vs C++ |
| `ps-shm` | Cross-process shared counters (mmap) |

## Safety policy

Every crate carries `#![forbid(unsafe_code)]` **except** `ps-nginx`, which is
the documented FFI-only exception: it alone may contain `unsafe` code, and
only for `extern "C"` declarations and minimal wrappers around the NGINX C
API. No optimization logic may live behind `unsafe` in `ps-nginx`.

Release builds use `opt-level = 3`, full LTO, and `codegen-units = 1`
(see the workspace `Cargo.toml`).

## Building and testing

Requires a Rust stable toolchain:

```sh
cargo test --workspace          # full suite (unit + integration + compat)
cargo build --workspace --release
```

Benchmarks and profiling harnesses:

```sh
cargo run --release -p ps-html --example timing_breakdown   # stage latencies
cargo run --release -p ps-html --example async_workload -- --pool --pages 50
cargo bench -p ps-html --bench background   # criterion async suite
```

NGINX module packaging (`.deb` build against nginx sources) lives outside
this repo in `../rust_pagespeed_compiler/build_from_source_to_deb.sh`.

## Docs

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — workspace crate designs
- [`docs/ASYNC_REWRITING.md`](docs/ASYNC_REWRITING.md) — async model, stats, benchmarks
- [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md) + [`docs/performance/`](docs/performance/) — request-path measurements and raw JSON
- [`docs/AUDIT.md`](docs/AUDIT.md) — inventory of the C++ originals
- [`docs/MAPPING.md`](docs/MAPPING.md) — which C++ owner feeds which crate
- [`docs/PLAN.md`](docs/PLAN.md) — build order
- [`../benchmark.md`](../benchmark.md) — production latency table vs C++

## Original sources are preserved

The existing checkouts are untouched and remain the source of truth:

- `../mod_pagespeed` (read-only)
- `../ngx_pagespeed` (read-only)
- `../ngx_psol` (read-only)
- `../ngx_compiler` (read-only)

Nothing in this repository modifies those directories. Port work here must
be verified against them.
