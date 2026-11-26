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

## Feature quick reference

| Feature | Configuration / behavior | Details |
|---|---|---|
| **Zstd compression** | `pagespeed EnableFilters compress_zstd;` — negotiated with supporting clients | [Compression and caching](#zstd-output-compression) |
| **Automatic font lookup and preloading** | `preload_critical_fonts` is on in CoreFilters; disable with `pagespeed DisableFilters preload_critical_fonts;` | [Discovery, learning, and controls](#automatic-critical-font-preloads) |
| **HTML cache before PHP** | `pagespeed HtmlCache on;` plus `pagespeed HtmlCacheEarlyLookup on;` | [Early HITs and analytics cookies](#serve-html-cache-hits-before-php) |
| **WebP / AVIF images** | Image conversion and per-client format negotiation; AVIF needs the runtime encoder | [AVIF dependency](#avif-runtime-dependency) |
| **LCP and critical-image learning** | Beacon evidence, priority/preload hints, logo detection, and fallback hero selection | [Supported features](#features-vs-c-pagespeed) |
| **Shared metadata caching** | Optional bounded cross-worker tier, alongside default local LRU and disk persistence | [Shared caching](#shared-caching) |
| **RUM instrumentation** | `pagespeed EnableFilters add_instrumentation;` — navigation timing remains active in cached HTML | [HTML-cache behavior](#serve-html-cache-hits-before-php) |

Example inside an existing PageSpeed-enabled NGINX `http {}` or `server {}` block:

```nginx
pagespeed EnableFilters compress_zstd;
# Font lookup/preloading already runs with CoreFilters.
# pagespeed EnableFilters preload_critical_fonts;

# Optional, for public routes with suitable cache invalidation:
pagespeed HtmlCache on;
pagespeed HtmlCacheEarlyLookup on;
```

Read each feature's eligibility and cache-policy requirements below before
changing an existing deployment.

## Performance

The following controlled static-page baseline uses **`ef9aeb5`**, tested on 2026-11-24 with the default
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
evaluated under the intended workload. The [worker-scaling and cache-pressure
report](docs/performance/shared-memory/WORKER_MATRIX.md) covers 1/2/4-worker
comparisons, eviction, memory use, and bounded queue rejection.

Initial activation over legacy workers requires a **full stop/start**.
Subsequent same-protocol graceful reloads are tested. See
[configuration, recovery, and admin diagnostics](docs/performance/shared-memory/CONFIGURATION.md)
for deployment details.

Earlier component benchmarks and asynchronous-workload examples are documented
in [the async rewriting guide](docs/ASYNC_REWRITING.md).

### Zstd output compression

Enable negotiated Zstandard output with `pagespeed EnableFilters compress_zstd;`.
It keeps the original level 3 compression and streaming flush behavior. Clients
that do not accept zstd retain the existing response path.

Completed codec workspaces are reused per thread. Repeated complete bodies use
a bounded 512 KiB/eight-entry payload cache with exact-byte validation; streaming
HTML frames are never cached, preserving per-request RUM/beacon behavior.
[Zstd benchmarks and memory tradeoffs](docs/performance/zstd/RESULTS.md) cover
HTML HIT/MISS, cached resources, unique bodies, and disabled-filter controls.

### Automatic critical font preloads

`preload_critical_fonts` is included in **CoreFilters**, so it is enabled by
default at that rewrite level. It adds at most two font preload hints near the
start of `<head>`, with the font MIME type and `crossorigin="anonymous"`.

Discovery uses inline and external CSS, CSS custom properties (`var()`), variable
font weight ranges, and font usages verified against document tags/classes/IDs.
Unused utility classes do not trigger preloads. Existing author font preloads
are respected; unsafe sources and unconfirmed icon/subset-only fonts are pruned.
This is bounded CSS analysis, not a browser layout engine.

```nginx
# Optional: explicitly enable when using a different rewrite level.
pagespeed EnableFilters preload_critical_fonts;

# Alternatively, disable font preloading:
# pagespeed DisableFilters preload_critical_fonts;

# Optional diagnostics explaining font selection/rejection:
# pagespeed EnableFilters debug;
```

Parsed stylesheet discoveries and page decisions are cached. Pending remote CSS
loads are left for a later render instead of waiting for network completion.
Browser font beacons learn loaded families and version dependent HTML entries.
The beacon endpoint supports empty POSTs with report fields in the query string
(the `navigator.sendBeacon(url)` transport); issued-nonce and replay validation
still apply. Existing POST-body beacons remain supported.

Recent selector collection work reduced allocations from 7,987 to 1,987 in the
repeated-class local fixture. This is an instrumented component result, not a
production request-latency claim. See the [font lookup benchmark](docs/performance/font-lookup-optimization/RESULTS.md)
and [font beacon verification](docs/performance/font-empty-beacon/RESULTS.md).

### Serve HTML cache hits before PHP

`HtmlCacheEarlyLookup` is an opt-in extension for routes whose anonymous output
may be reused for the configured HTML-cache lifetime:

```nginx
pagespeed HtmlCache on;
pagespeed HtmlCacheTtlSec 300;
pagespeed HtmlCacheEarlyLookup on;
pagespeed ShowCacheTimestamp on;  # Optional HIT/MISS/BYPASS comment
```

After an eligible rewrite publishes its body and origin-policy metadata, warm
GET requests can return before PHP or another origin handler runs. Missing,
expired, purged, corrupt, or differently keyed entries fall through to the
existing origin/rewrite path. Options, request variants, file modification time,
and beacon evidence remain validated; sampling-due beacon requests fall through.
HTML bodies containing sampled beacon nonces are not published. Existing RUM
JavaScript remains in the cached HTML and runs on each browser navigation.

The default is **off**. Enabling this trusts the cached origin policy until expiry:
changes to application content, response headers, or access rules that are not
represented in the cache identity require a purge or expiry. Use it only on
public routes with suitable invalidation. The early path uses the same configured cookie allowlist as ordinary HTML caching:
requests containing only approved analytics cookies can HIT before PHP runs.
Private, login, session, unknown, malformed, or forced-bypass cookies still bypass,
as do Authorization, conditional/range headers, or explicit Cache-Control/Pragma.
`HtmlCacheIgnoreCommonCookies off` also disables the built-in analytics allowance
for early lookup; custom ignore/bypass cookie rules remain respected. Ignored
cookies must not affect server-generated HTML. Origin Set-Cookie/private/no-store
prohibitions still prevent publication. Dynamic applications that require PHP to
run on every request should leave this option off. NGINX access phases still run
before the early handler; the setting does not replace application authorization.
Returned HTML uses private/no-store downstream policy and negotiates compression
for the current request. Resource optimization, ordinary HTML cache lookup, and
all enabled rewrite filters remain active on misses.

Approved analytics cookies such as `_ga` and `_gid` need no new directive when
`HtmlCacheEarlyLookup on` is enabled. They can share the public cookie-free
variant without invoking PHP on a warm HIT. Private/login/session cookies are
not covered by that allowance. The [analytics early-HIT regression](docs/performance/analytics-early-hit/RESULTS.md)
verified zero origin calls across 20 analytics HITs; median latency changed from
26.77 ms to 0.69 ms with a synthetic 25 ms origin delay. These are isolated local
measurements, not a live-site speedup claim.

A HIT does not guarantee every later navigation will HIT. Beacon sampling,
newly accepted learning evidence, request variants, expiration, and invalidation
can cause a later BYPASS or MISS. Sampled responses stay uncached so their
per-request nonces are not reused. A successful beacon transport normally
returns HTTP 204; that status alone does not prove its nonce/evidence was
accepted. Font learning and replay checks are covered by the linked verification
record. Deployment availability depends on the installed module version; the
latest local fixes are not evidence that a production server has been updated.

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
| Shared-memory metadata cache | ✅ | ✅ opt-in | Bounded Linux shared tier; persistent disk fallback and tested reload/recovery |
| Critical-image beaconing + LCP learning | ✅ | ✅ | Beacon finder, evidence store, LCP prioritization |
| Full RUM instrumentation filter | ✅ | ✅ | `add_instrumentation`; cached HTML retains browser-side navigation timing |
| **Automatic critical font preloads** | — | ✅ **CoreFilters default** | Up to two hints; CSS usage analysis, variable fonts, cached decisions and beacon learning |
| **Automatic logo detection for LCP** | ❌ | ✅ **Rust-only** | Confidence-based (WP/custom/SVG/CSS-bg); keeps logos out of auto-preload |
| **LCP fallback hero** | ❌ | ✅ **Rust-only** | Empty/stale evidence never leaves the visual hero bare |
| Async HTML (deadline-detach) | ✅ threads | ✅ adapted | Fire-and-forget rewrite jobs; first view serves originals, next view hits |
| **Async CSS/JS combination** | ✅ threads | ✅ adapted | Background combiners with failure memory; uncombinable members never block HTML caching |
| **Background admin reporting** | ✅ | ✅ | Worker completions land on the admin images page |
| Admin / console / statistics / graphs | ✅ | ✅ | `ps-admin` + cross-process shared counters |
| **Zstd output compression** | — | ✅ | `compress_zstd`; negotiated, streaming, bounded payload cache |
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
once at worker startup. Immutable filter plans are reused; mutable parser/filter
state remains request-local. Early HTML-cache HITs bypass parsing and rewrite
filter construction entirely.

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

## AVIF runtime dependency

**AVIF conversion requires the external `avifenc` executable on every server
running the module.** Enabling `convert_to_avif` or installing the PageSpeed
module alone does not install this encoder. Without it, AVIF conversion fails
and images can remain in another format, such as WebP or JPEG.

On Ubuntu/Debian, install and verify it:

```bash
sudo apt update
sudo apt install libavif-bin
command -v avifenc
avifenc --version
```

The executable must be available in the NGINX worker environment's `PATH` and
executable by its user. A successful AVIF test on a development machine does
not verify this dependency on the production server.

After installing the encoder, purge the affected PageSpeed resource and HTML
cache entries, or allow them to expire. Existing cached WebP/JPEG results may
otherwise continue to be served. Request the page again to warm background
optimization; verify the image response's **`Content-Type: image/avif`**, rather
than relying on its URL extension. The client must advertise AVIF support, and
existing eligibility, conversion limits, and fallback rules still apply—installing
the encoder does not force every image to become AVIF.

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
