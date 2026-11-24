# PageSpeed Rust (`pagespeed-rust`)

A Rust port of [PageSpeed](https://github.com/apache/incubator-pagespeed-mod)
(PSOL optimization engine + `ngx_pagespeed` server module): HTML/CSS/JS/image
optimization, multi-tier caching, beacon-driven learning, admin pages, and a
dynamically-loaded NGINX module — memory-safe, fast, and behavior-compatible
with the C++ original.

## Status: serving production traffic

The module is installed and serving rewritten traffic (HTML, WebP/AVIF images,
proxy mode). The latest recorded full workspace run passed 2,920 primary tests across
99 result groups, including C++ compatibility coverage (see the
[verification record](SHARED_MEMORY_IMPLEMENTATION.md)). It is still maturing: keep backups of
your C++ configuration when switching over.

## Performance

Latest measured build: **`ef9aeb5`**, tested on 2026-11-24 with the default
cache configuration and an ordinary release module against NGINX 1.31.8.

| Page | First HTML delivery | Warm HTML cache HIT | Warm metadata, HTML cache MISS |
|---|---:|---:|---:|
| Moto | **3.64 ms** | **0.46 ms** | **1.86 ms** |
| Aroma | **4.93 ms** | **0.49 ms** | **2.70 ms** |

First-delivery values are medians of five requests with fresh processes and
empty application caches. Each warm value pools 500 requests across five runs;
all HIT samples were confirmed HTML cache hits. Tests used an isolated local
NGINX worker and a Python HTTP client; OS filesystem caches were not flushed.
These measurements exclude process startup and do not measure completion of
background resource optimization.

HTML rewriting uses cached resource metadata. Missing metadata schedules
background optimization and returns the original URL; later requests use the
optimized URL. Warmed HTML retained the same normalized output across every
run and both cache modes, including 15 optimized resource references on Moto
and 23 on Aroma.

The [current benchmark report](docs/performance/shared-memory/ef9aeb5-comparison.md)
contains raw-data links, exact commands, the historical comparison table, and
results with shared caching enabled. Older measurements used different clients
and NGINX builds, so they do not establish a controlled speedup ratio. The
[profiling checkpoint](docs/performance/shared-memory/RESULTS.md) records stage
costs and remaining attribution limits, including the unresolved historical
production latency.

### Shared caching

Linux supports an optional bounded shared-memory metadata tier between the
local LRU and persistent disk cache:

```nginx
# Inside http {}; use the same FileCachePath for both directives.
pagespeed FileCachePath /var/cache/pagespeed;
pagespeed CreateSharedMemoryMetadataCache /var/cache/pagespeed 16384;
```

The example allocates a 16 MiB byte-cache budget; bounded coordination mappings
are additional. Shared caching remains **opt-in**: the latest single-worker
run measured Moto/Aroma HTML hits at 0.47/0.50 ms, with no consistent advantage
over the default stack. Its cross-worker reuse and coordination should be
evaluated under the intended workload.

Initial activation over legacy workers requires a **full stop/start**.
Subsequent same-protocol graceful reloads are tested. See
[configuration, recovery, and admin diagnostics](docs/performance/shared-memory/CONFIGURATION.md)
for deployment details.

Earlier component benchmarks and asynchronous-workload examples are documented
in [the async rewriting guide](docs/ASYNC_REWRITING.md).

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
- [`docs/performance/shared-memory/ef9aeb5-comparison.md`](docs/performance/shared-memory/ef9aeb5-comparison.md) — latest benchmark, raw results, and historical comparison
- [`docs/performance/shared-memory/CONFIGURATION.md`](docs/performance/shared-memory/CONFIGURATION.md) — shared-cache configuration, lifecycle, and admin metrics
- [`../benchmark.md`](../benchmark.md) — historical localhost latency comparisons

## Original sources are preserved

The existing checkouts are untouched and remain the source of truth:

- `../mod_pagespeed` (read-only)
- `../ngx_pagespeed` (read-only)
- `../ngx_psol` (read-only)
- `../ngx_compiler` (read-only)

Nothing in this repository modifies those directories. Port work here must
be verified against them.
