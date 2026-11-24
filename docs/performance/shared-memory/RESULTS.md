# Shared-memory performance checkpoint

Phase 10 remains in progress. These are isolated local measurements, not production attribution. Source implementation: `848fc15`; subsequent `f4f8b6a` records tests. Module hashes and temporary paths are in `phase10-ordinary-module.json` and `phase10-profile-module.json`. Matched staged Nginx 1.31.8 was used; no live service changes.

## Ordinary module: total request latency

Median milliseconds, pooled across three alternating default/shared pairs, 300 HIT and 300 MISS samples per page/mode, 15 cold requests per page/mode. Application caches fresh for cold runs; OS caches may be warm. Single worker; 16 MiB shared tier.

| Page | Mode | Cold | Warm HTML HIT | Warm HTML MISS |
|---|---|---:|---:|---:|
| Moto | Default | 4.159 | 0.488 | 1.825 |
| Moto | Shared | 4.363 | 0.472 | 1.891 |
| Aroma | Default | 5.472 | 0.483 | 2.655 |
| Aroma | Shared | 5.705 | 0.500 | 2.681 |

No consistent single-worker advantage: shared cache remains opt-in. Raw data: phase10-default-1/2/3.json and phase10-shared-1/2/3.json. Historical baseline at d0f055c was Moto 6.185/1.322/4.768 ms and Aroma 10.026/1.463/7.293 ms (cold/HIT/MISS); runtime/load differed, so decreases cannot be assigned to one implementation phase. Earlier primitive-only phases did not expose an enabled production request path; no invented per-phase request timings are supplied.

Two workers, eight clients, 400 Moto HIT requests per mode: shared median 1.676 ms, default 1.987 ms. This is one workload/order (shared followed by full-stop default), not a general throughput conclusion. See phase10-concurrency.json.

Ordinary isolated Aerosky: cold 3.263 ms, HIT 0.470 ms, MISS 1.945 ms, PageSpeed off 0.159 ms. See phase10-aerosky-ordinary.json.

## Instrumented Aerosky breakdown

Pipeline-profile module, one worker, 100 warm samples. Instrumented totals differ from ordinary runs and are not a before/after comparison.

| Stage | Warm HIT | Warm MISS |
|---|---:|---:|
| Total request, ms | 0.515 | 1.914 |
| Filter initialization, µs | 0 | 8.822 |
| HTML cache lookup, µs | 44.023 | 36.408 |
| HTML processing, µs | 0 | 1122.094 |
| HTML cache store, µs | 0 | 211.123 |
| Outside instrumented stages, ms | 0.471 | 0.531 |

Cold total 4.116 ms. PageSpeed-off total 0.161 ms includes loopback/client/Nginx. Stage medians do not sum exactly to total median; residual is calculated per request before taking its median. Residual includes uninstrumented Rust, Nginx, sockets, client and scheduling; it is not a measurement of pure Nginx overhead.

All 257 recorded resource fetches were background; zero foreground fetches. Aggregate resource fetch time 19.150 ms spans the workload and must not be added to HTML request latency. See phase10-profile-plain.json.

## Profiling evidence and cache operations

Strace perturbed latency (HIT 2.379 ms, MISS 3.847 ms). Its whole-workload syscall totals include overlapping/idle waits: epoll_wait 101.473 ms, futex 23.232 ms, openat 14.802 ms. They do not identify a 13 ms request bottleneck. Raw compressed trace and JSON summary are retained.

Callgrind recorded 2,590,272,038 instructions including startup/background jobs: run_rewrite_job accounts for ~74.78% inclusive instructions; deflate_slow alone ~18.85% self instructions. Image encode/decode and CSS optimization remain active. Inclusive counts overlap; instruction shares are not wall-time shares. Raw compressed profile and annotations are retained.

Final idle cache microbenchmark medians: file 4859.222 ns, shared L2/file 272.150 ns, L1/file 147.822 ns, L1/shared/file 190.377 ns (seven batches of 10,000 4 KiB gets). Shared L2 avoids repeated filesystem cost, while coherence adds ~42.6 ns on this hot L1 workload. This is not an HTML request benchmark. See phase10-cache-probe-repeat.json.

## Correctness, validation and remaining work

Normalized optimized HTML hashes match across all final default/shared repeats: Moto `889b7c10af1d2ce7c7c158cfe6700347dc8e5d8169fd0df84ce58ae125386298` (15,031 bytes, 15 PageSpeed references); Aroma `c691571e4644b28bebd771d586a7625b62b3bdcb8a3848cd87b5216a4847fcf6` (34,068 bytes, 23 references). No filters were disabled to obtain these results.

Final workspace tests: 2,922 reported passes including two nested subprocess reports, zero failures, 99 result groups. Final ordinary release build passed. Logs persisted as phase10-final-*.log. Earlier ten-repeat shared/cache fault stress also passed; no sanitizer run claimed. Kernel perf events are unavailable on this host.

The supplied localhost production URL timed out after 10 seconds, yielding no latency samples (phase10-localhost-observation.json). The historical approximately 13 ms remains unattributed. Next session: review this report against raw artifacts, complete Phase 10 documentation/acceptance checks, and obtain functioning production access for production and Nginx-specific attribution. Do not deploy staged ABI-mismatched modules or claim the production issue resolved.
