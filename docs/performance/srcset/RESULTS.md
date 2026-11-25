# Srcset Phase 4 measured checkpoint

No live server changes or deployment. Runtime source4f21fde. This checkpoint verifies post-guard browser/cache/beacon behavior; Phase4 remains incomplete for realistic core-resource MISS profiling and adversarial-input latency. Exact runnable commands are retained in PLAN.md.

## Same-build NGINX off/on

Five alternating rounds, each fresh isolated NGINX process/cache. Startup excluded. Each mode has5first-request samples,100unique-path warm HTML MISS samples and100early HTML HIT samples. Fresh loopback HTTP connections, canonical Hostlocalhost, PassThrough+RUM, fixed25ms synthetic FastCGI delay, no resource optimizers. These are not production or full-resource-cold measurements.

| Request | Filter off median | Filter on median | Off p95 | On p95 |
|---|---:|---:|---:|---:|
| Cold first request | 30.449 ms | 30.632 ms | 30.637 ms | 31.011 ms |
| Warm HTML MISS | 28.918 ms | 28.920 ms | 29.496 ms | 29.742 ms |
| HTML HIT | 0.723 ms | 0.693 ms | 0.976 ms | 0.935 ms |

Batch median ranges overlap. No causal speedup or regression claim. Cold p95 is based on only5samples. The fixed origin delay dominates MISS time; subtracting it does not isolate NGINX overhead. Raw samples and batch ranges:phase4-final/summary.json and per-runJSON. SameURL/shared persistent disk cache off/on key isolation passed, with expected sizes and first MISS asserted. HITs retained normalized body and browser RUM clock while issuing0additional origin calls. Cookie/auth/cachecontrol/range fallbacks and SetCookie exclusion passed.

## Driver-stage diagnostic

9alternating batches×500requests per mode,100warmups,34KB/1000paragraph fixture, no HTML cache/network. Timers include overhead; HTML stage includes driver destruction. Stage medians are independent and need not sum exactly.

| Stage | Off | On |
|---|---:|---:|
| Resource driver setup | 2.125 µs | 2.395 µs |
| Filter setup/cached plan | 11.875 µs | 13.158 µs |
| HTML processing and drop | 1029.895 µs | 1095.861 µs |
| Total | 1042.508 µs | 1111.646 µs |

Zero fetches asserted. Both modes are substantially slower than the prior session, so this run cannot demonstrate a before/after guard improvement. Same-run enabled overhead is observable, not a production latency estimate. No kernel/profile attribution of the earlier production13ms is claimed.

Separate-process peak RSS (single off/on run,100iterations/batch; not allocation counts):

- off: 6976 KiB.
- on: 6952 KiB.

Single process peaks are noisy and do not establish a memory regression. Raw output includes timing and exit status.

## Browser and beacon correctness

Chrome24cases:352/412/1024CSSviewport×DPR1/1.75/2/3×off/on. Actual NGINX HTML, supplied300/768/800JPEG fixture; no browser cache. CSSwidth invariant, selected file sufficient up to largest supplied size, transfer body size matches source file, enabled bytes never larger in checked cases.3pairs select smaller supplied image,9unchanged;412×1.75still768w. This is synthetic layout evidence, not production byte savings. Natural dimensions, real file dimensions/currentSrc and transfers retained in phase4-browser-after-guards.json.

Both actual core+Zstd+early-cache beacon reload acceptance orderings pass with sizes enabled, nonce replay rejected and old/new workers overlapping. Existing tools retain defaults; new flags enable policy fixtures. Policy-enabled cacheable RUM oracle is intentionally separate from core nonce-bearing HTML. ModuleSHA:

5777c16827917839fd60b05814906a1dbefe4c0e224e2d695b4942aba406c93f

Remaining:realistic core/resourcewarmMISS versus coldresource handling, large/malformed/defaultoff enabled-filter latency matrix, complete final docs/release review. No need to rerun the completed browser/reload checks unless source changes. No active background fixtures left.
