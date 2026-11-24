# HTML request performance measurements

Measured on 2026-11-24 (Asia/Manila), using the supplied
`/test/aerosky-real-estate-html-responsive-website-template/index.html` page.
The matched Rust comparison uses the workspace's staged Nginx 1.31.8, one
foreground worker, the same page files and rewrite configuration, and release
modules. The baseline is commit `cef5556`, with only the same opt-in profiling
hooks added. The deployed localhost Nginx 1.31.9 was queried but not modified.

## Delivery latency

Times are milliseconds. Cold samples use five independently created, empty cache
prefixes and fresh Nginx processes per version. OS filesystem/page caches remain
warm: this is application-cache cold, not a machine reboot. Warm values are medians
of 100 sequential loopback HTTP requests after three consecutive actual HTML HITs.
Both modules have `pipeline-profile` enabled. Normal builds have no profiling clocks
or logs.

| Request path | Before | After | Speedup |
| --- | ---: | ---: | ---: |
| Cold HTML delivery, fresh application caches | 30.793 | 6.318 | 4.9× |
| Warm resources, HTML cache MISS | 14.952 | 1.905 | 7.8× |
| Warm HTML cache HIT | 0.591 | 0.522 | 1.1× |
| PageSpeed-off Nginx/transport control | 0.162 | 0.160 | effectively unchanged |

MISS probes vary only `X-PS-Benchmark`, a whole-HTML cache identity header. Resource
URLs, effective rewrite options, Host, and page content stay fixed. Every measured
warm HIT was marked HIT and every measured warm MISS was marked MISS. Cold delivery
serves valid fallback URLs while work continues; it does **not** measure the time
to finish all background optimizations. Warmup waits for completed rewrites before
measuring either warm path.

Early incremental measurements, before the final matched run:

| Implementation stage | Warm HTML MISS median |
| --- | ---: |
| Initial checkout, early run | 15.19 ms |
| Async CSS/JS combinations, font scheduling, clone/plan reuse | 4.65 ms |
| Final shared UA/capability/LCP state, signature generations and coherent LRU | 1.90 ms |

The intermediate result used 20 samples in a separate run; it is not an isolated
A/B measurement of each individual edit. Use the matched 100-sample table above
for the overall comparison. Deadline and panic fixes primarily protect tail
latency and correctness; no separate numerical speedup is claimed for them.

## Memory-cache default

The existing 8 MiB-per-worker LRU default was also compared against
`LRUCacheKbPerProcess 0` with the optimized module and 100 samples per path.
Disk-only warm MISSes measured **2.205 ms**, versus **1.905 ms** with the LRU
(about 14% lower delivery time). HITs measured 0.542 versus 0.522 ms; that smaller
difference should not be treated as a strong result from one sequential run.
Output stayed identical. This supports retaining the existing bounded default on
this workload; explicit zero remains supported. Raw samples are in
[disk-only.json](performance/disk-only.json). The LRU changes in this patch add
cross-worker coherence and fix stale replacement/backfill races rather than
silently enabling a previously absent default.

## Where the approximately 13 ms went

The initial Callgrind profile attributed roughly 42% of recorded instructions to
JS combination and 31% to CSS combination. These paths loaded cached resource
bodies, reparsed CSS, and minified JS again on HTML misses. The intermediate
profile then exposed repeated UA matcher/regex construction and repeated LCP
evidence/capability work. These were measurements from the isolated child, not
inferences from elapsed time alone.

The matched warm-MISS stage timers show:

| Stage | Before, µs | After, µs |
| --- | ---: | ---: |
| Filter construction and parse startup | 9.257 | 9.011 |
| Whole-HTML cache eligibility/lookup | 97.433 | 36.617 |
| HTML parsing and filter processing | 13,846.343 | 1,120.168 |
| Whole-HTML cache publication | 214.647 | 219.326 |
| Remaining request time, median of per-request residuals | 771.660 | 516.034 |

HTML processing alone fell by **12.73 ms**. Filter initialization was about 9 µs,
not the dominant source of the reported gap. HITs already skipped filter
construction in the baseline, and continue to do so: both initialization and
HTML processing measured zero on HITs. HIT eligibility/lookup fell from
108.758 µs to 52.511 µs.

The residual includes Nginx integration, headers/configuration work outside these
spans, output delivery, and loopback/client overhead. It is **not** an exclusive
measurement of Nginx CPU time. The PageSpeed-off control separately measures the
static Nginx/transport floor. Stage medians need not sum to the median total.

The resource-fetch logs recorded zero foreground origin fetches in **both** matched
warm scenarios. The earlier implementation's main problem on this workload was
processing already-cached bytes synchronously, rather than waiting for network
fetches. Across startup, warmup, and all warm samples, baseline/optimized runs
recorded 257/256 background fetch calls, totaling 19.225/9.254 ms respectively.
These are aggregate background durations, not additions to HTML delivery time;
the different warmup executions make them unsuitable as a fetcher speedup claim.
The optimized combination-HIT regression additionally asserts **zero HTTP resource
cache reads**, catching cached-byte loading that an origin-fetch counter misses.

Attaching perf/strace to the deployed root-owned workers was unavailable:
`perf_event_paranoid=4`, and passwordless sudo was unavailable. An isolated child
allowed Callgrind and syscall tracing without changing the live service. Initial
read-only live probes measured approximately 0.99 ms for HTML HITs and 15.85 ms for
unique-query BYPASS requests; those are separate from the controlled MISS tests.
No production call-stack attribution or production-after-deployment result is
claimed.

## Optimization quality and validation

The baseline and optimized warm-HIT HTML are byte-identical after removing only
the cache timestamp comment: 18,182 bytes, 17 `.pagespeed.` references, SHA-256
`5d758a78fa306671ce98b7ed3ef35fa21e8c19f85fc6f91749f6f628ebd7e509`.
The improvement does not come from omitting resources or emitting cheaper output.

`cargo test --workspace --quiet` passed **2,850 tests** with zero failures and zero
ignored tests. Additional focused checks cover:

- MISS → background completion → metadata HIT progression and queue saturation;
- CSS/JS asynchronous combinations matching the synchronous output;
- no HTTP resource-cache reads on combination metadata HITs;
- successful partitions surviving a neighboring failed candidate;
- expiration, option generation changes, and request-capability invalidation;
- cross-process LRU replacement/deletion coherence and cleaner exclusions;
- panic/shutdown waiter release and elapsed-work deadlines;
- RUM bootstrap retention on HTML HITs while filters stay unconstructed;
- existing image, responsive-image, lazyload, LCP/preload and beacon regressions.

The small standalone CoreFilters microbenchmark remained statistically unchanged:
21.25 µs before versus 21.32 µs in the measured repeat (p=0.36). It does not exercise
the resource-driver/Nginx path and is not a substitute for the request measurements.

## Reproduction and artifacts

Raw samples: [before](performance/before.json), [after](performance/after.json),
[cold repetitions](performance/cold.json), [test totals](performance/validation.json),
[CPU attribution](performance/cpu-summary.json), and
[optimized syscall totals](performance/syscalls-after.json).
CPU profiles include startup and asynchronous warmup; their totals are diagnostic
sessions, not normalized per-request instruction counts. Syscall trace elapsed
times include tracing overhead and are not request-latency measurements.
The `cold` field in the warm-run JSON means the first request in that process;
only the separate fresh-prefix repetitions are used as cold results above.

Build the instrumented module:

```sh
cargo build -p ps-nginx --release --features pipeline-profile
```

Prepare an isolated prefix with writable `cache`, `logs`, and `conf` directories.
Use `daemon off; master_process off; worker_processes 1;`, a pid file at
`<prefix>/nginx.pid`, an error log at `<prefix>/logs/error.log`, and an unprivileged
listen port. Configure the same root and filters, HTML caching, TTL 300, and
`ShowCacheTimestamp on`. The supplied-page comparison enabled CoreFilters,
`prioritize_lcp_images`, `inline_css`, `rewrite_images`, `convert_to_avif`,
`convert_jpeg_to_webp`, `strip_legacy_polyfills`, and `lazyload_images`, with
`SupportNoScriptEnabled off` and `CriticalImagesBeaconEnabled true`.

```sh
python3 tools/profile_html.py \
  --nginx /absolute/path/to/nginx \
  --prefix /tmp/isolated-probe --port 18088 \
  --path /test/aerosky-real-estate-html-responsive-website-template/index.html \
  --samples 100 --output /tmp/measurements.json
```

Use a fresh prefix/cache with `--cold-only` for cold repetitions. Add
`--mode callgrind` or `--mode strace` for attribution, not latency comparisons;
profilers substantially distort elapsed times. `--body-output` saves the warm-HIT
body for output comparison. Restore the ordinary release build with
`cargo build -p ps-nginx --release`.

The C++ source comparison follows `RewriteContext::OutputCacheMiss`,
`RewriteContext::GetRewriteDeadlineAlarmMs`, and `RewriteDriver`'s bounded
wait/detach path in `mod_pagespeed/net/instaweb/rewriter`. Workers reuse the existing
Rust optimizer/partition implementations rather than substituting reduced-quality
operations. C++ can wait within its rewrite budget before detaching; Rust uses the
requested immediate HTML fallback policy. A matched C++ binary benchmark has not
been run, so these results establish the Rust improvement, not a claim of beating
C++ overall. Cross-worker optimization-job deduplication is also not added: jobs
deduplicate within each worker pool, while the new LRU coherence spans workers.


## Follow-up: worker initialization and resource-servant lifetime

Measured after `c984b33`, with the working-tree changes in `server_context.rs`
and the Nginx rewrite bridge. These changes are not deployed.

Callgrind identified 9,671,041 instructions in `TokenizerPatterns::new` under
`ResourceDriver::register_serving_filters` on the first request. Eight tokenizer
regexes were compiled before the HTML-cache lookup. This is an inclusive call
count, not an exclusive request-time percentage: the profile also includes
startup and background optimizers.

The Nginx bridge now prepares the shared tokenizer and capability tables during
worker initialization. Compilation is moved to startup, not removed; request
measurements exclude process startup. Built-in resource servants are enabled by
an inexpensive flag and constructed only when their filter ID is requested.
They are no longer stored inside the driver: the former driver → servant →
driver strong-reference cycle retained a driver even on a pure HTML-cache hit.
Explicit custom servants still take precedence. All seven built-in IDs remain
available and run their existing implementations.

Matched ordinary release builds used the same Nginx 1.31.8 executable, page
files, filter configuration, request client, Host and port. Cold values are
medians of five empty application-cache processes per page and build, alternating
builds; OS caches were warm. Warm values use 100 requests per phase.

| Page / path | c984b33 | Working tree |
|---|---:|---:|
| Moto: Cold HTML | 8.539 ms | 5.471 ms |
| Moto: Warm HTML HIT | 1.369 ms | 1.357 ms |
| Moto: Warm HTML MISS | 4.303 ms | 4.354 ms |
| Aroma: Cold HTML | 10.451 ms | 7.579 ms |
| Aroma: Warm HTML HIT | 1.432 ms | 1.393 ms |
| Aroma: Warm HTML MISS | 6.026 ms | 6.242 ms |

Cold delivery improved about 36% for Moto and 27% for Aroma. Warm timings are
roughly unchanged; MISS values increased slightly in this sample, so this is not
an established steady-state latency improvement. The warmed HTML SHA-256 values
match exactly between builds after removing only the cache timestamp comment:

- Moto: `889b7c10af1d2ce7c7c158cfe6700347dc8e5d8169fd0df84ce58ae125386298` (15031 bytes).
- Aroma: `c691571e4644b28bebd771d586a7625b62b3bdcb8a3848cd87b5216a4847fcf6` (34068 bytes).

A separate own-process test measured Linux VmRSS before and after 1,000 verified
HTML-cache HITs on Moto:

| Build | RSS growth |
|---|---:|
| c984b33 | 11,668 KiB |
| Working tree | 360 KiB |

RSS is allocator-dependent, but the regression test independently proves that
registration and lookup of every built-in servant release their driver reference.
The custom-servant precedence test also passes. All 1,975 tests in
`cargo test -p ps-html -p ps-nginx` passed, including resource serving, HTML cache,
async combination failures, image conversion, and RUM coverage. No filters or
resource processors were disabled. Formatting checks were scoped to the two
modified Rust files; unrelated files already fail workspace-wide formatting.

Raw measurements: [latency](performance/worker-init-lazy-servants.json) and
[retention](performance/worker-init-retention.json). The existing
`tools/profile_html.py` reproduces each phase with isolated configurations;
change `load_module` to the corresponding release artifact and use a fresh
`FileCachePath` for cold repetitions. First-load HTML delivery does not include
completion of background resource optimization.
