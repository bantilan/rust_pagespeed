# Progressive Asynchronous Resource Rewriting

C++-style progressive optimization for the Rust port: serve cached
rewrites immediately, bound the wait for uncached resources by a
rewrite deadline, and warm the cache in the background so later
requests reuse completed work.

## How it works

`ServerContext` optionally owns a `BackgroundPool`
(`crates/ps-html/src/background.rs`): N worker threads
(`NumRewriteThreads`, default 1), a bounded pending queue (1024),
in-flight dedup by fetch key, and shared statistics. The nginx
factory creates one pool per worker at child init and installs it
on the worker's `ServerContext`; every request driver then shares
that server's caches and pool.

On an HTTP-cache miss, `CacheableResource::load` submits a
fetch+store job and waits up to the driver's remaining flush-window
budget (`RewriteDeadlinePerFlushMs` per flush, rearmed by
`ps-nginx` on every flush, default 10ms NDEBUG parity):

- Result in time: link it exactly like a sync completion. Fetch,
  store, stats, and failure paths are one shared implementation
  (`FetchPlan`/`execute_fetch`/`store_fetch_outcome`), so sync,
  async, and in-place fetches cannot diverge.
- Deadline exceeded: serve the fallback (original references for
  HTML, failed fetch for IPRO origins so the server falls back to
  its native method). The job keeps running and warms the cache.
  Timeouts never touch failure memory and mark inputs pending, so
  contexts skip writing partitions — C++ drops uncompleted
  contexts the same way.
- Pool missing, shut down, or the URL unmappable: run the
  direct sync path. Degraded, never failed. Held fetches stay
  inconclusive (`pending`, never stored).
- Pool saturated: shed the fetch (`QueueFull`, counted) and serve
  the fallback immediately without waiting; a later request
  retries once workers drain. Saturation never parks past the
  deadline.

Simultaneous identical fetches collapse onto one job (dedup key:
fetch URL + serialized request headers + fallback shape), so a
thundering herd pays for one origin fetch.

## Deadline selection

`ResourceDriver::fetch_wait_budget_ms` mirrors C++
`RewriteContext::GetRewriteDeadlineAlarmMs` with the
`InPlaceRewriteContext` override:

- Serving in place (`fetch_in_place_resource` scope) with
  `InPlaceWaitForOptimized` on: `InPlaceRewriteDeadlineMs`.
- Otherwise: `RewriteDeadlinePerFlushMs`.
- Non-positive configured value: unbounded wait (deadline
  disabled, C++ parity).

In-place origin fetches (`reconstruct`, `revalidate_input`,
proxy `serve_head`) run through the same bounded path. Without a
pool they fetch direct, exactly as before.

## Statistics

`background_jobs_submitted`, `background_jobs_completed`,
`background_jobs_timeouts`, `background_jobs_dedup_hits`,
`background_jobs_queue_full`, `background_jobs_shutdown`
(`crates/ps-html/src/rewrite_stats.rs`), plus
`html_async_rewrite_scheduled` (HTML misses detached to the pool)
and `html_async_rewrite_completed` (worker-side rewrites run to
partitions).

## Lazy image bytes

HTML-embedded images (`HtmlAttr`, `Srcset` places) defer pixel
work out of HTML render: the first view mints an unversioned
(hash-`0`) URL from the input hash after one decode (gates, debug
messages, dimensions, and input-based inlining all mirror eager),
and the first `.pagespeed.` request reconstructs through the
existing fetch path, stores, and upgrades the partition to the
versioned URL. Later renders are eager-identical. Inlinable
images still optimize eagerly (bounded by the inline budget, so
no URL ever exists to trigger their serve). CSS images stay eager
(amortized by cached CSS output).

First-view transients (all self-healing via serve): hash `0`
instead of the content hash, input extension instead of converted
(`.jpg` where eager shows `.webp`), and `.ic.0` claims where eager
shows `.ce.` (no-saving images converge to `.ce.` after the serve
records the decline). Unversioned serves bank bytes briefly under
the requested URL (fallback-shortened headers bound the entry),
so slow-to-rerender references never re-optimize per request.
Failures serve original bytes and converge partitions to
unoptimizable, exactly like eager.

New counter: `image_rewrites_deferred`. Deferred successes bill
`image_rewrite_latency_total_ms` only (OK-latency still measures
completed optimizations, at serve time).

Benchmark (`cold_image_page/8_jpegs_no_pool`, 8× 236 KB JPEGs,
cold server per iteration): eager 180.5 ms, lazy 45.3 ms (4.0×).

## Fire-and-forget HTML rewrites

Metadata HITs render optimized URLs inline with no fetching. On a
metadata MISS, an HTML request with a pool installed snapshots the
rewrite's Send-owned inputs (options, URLs, behavior parameters)
and submits a deduped `html-rewrite:` job
(`crates/ps-html/src/html_async.rs`), then serves original URLs
immediately — no waiting, no deadline involved. The render marks
itself detached, so the HTML cache never publishes unoptimized
bytes. The pool worker rebuilds an equivalent driver from the
snapshot (same options, shared caches/fetcher/statistics/hasher)
and runs the exact sync miss core — fetch, partition, rewrite,
partition write — without rendering. The next HTML request HITs.
Stale (revalidatable) metadata serves the stale optimized URLs now
and refreshes in the background the same way.

Coverage: CSS, JavaScript, image, cache-extender, and the three
inline behaviors. Combiners, font-service inputs, shutdown pools,
and the fetch/IPRO serving paths keep the sync behavior, so no
optimization is ever skipped — only deferred off the critical
path. Saturation sheds to fallback (`QueueFull`, counted); a later
request retries.

## Intentional differences from C++

- **Detached HTML rewrites; bounded resource waits.** HTML metadata
  misses never wait: they schedule whole-rewrite jobs and serve
  fallback URLs at once (C++ deadline-detach parity). Blocking
  waits remain only where bytes are owed — fetch/IPRO serving and
  the sync-fallback behaviors — bounded by the per-flush deadline
  (default 10ms). Worst case per HTML flush is the deadline only
  when a fallback behavior runs; scheduled rewrites add nothing.
- **Disabled deadlines mean no HTML waiting.** With
  `RewriteDeadlinePerFlushMs <= 0` the HTML path is pure
  fire-and-forget; only in-place serving with waiting enabled
  keeps the C++ unbounded wait.
- **Deadline-bounded IPRO origins fail fast.** A slow origin past
  the deadline behaves like a failed origin fetch (existing
  failure path); the server/proxy layer serves the original by its
  native method, and the background completion warms the cache.
- **IPRO keeps its direct authorization posture.** URLs the
  `DomainLawyer` won't map still fetch direct (as before) rather
  than failing; only mappable URLs take the bounded path.
- **Dedup keys include request headers.** HTML-embedded and
  in-place fetches for one URL dedup together only when their
  request headers match; same-path concurrent fetches always
  collapse.
- **Per-worker pools.** Each nginx worker has its own pool and
  memory caches; there is no cross-worker dedup yet (shared-memory
  caches still need server integration, as before).
- **NDEBUG defaults.** Both deadline options default to 10ms
  (C++ release); C++ debug builds use 20ms.

## Benchmarks

`cargo bench -p ps-html --bench background` (criterion; sync legs
register no pool, i.e. pre-change behavior). Indicative numbers
from the dev machine:

| Benchmark | Sync | Async |
|---|---|---|
| `cold_first_request` (50ms origin, 10ms deadline) | 50.6ms | 10.5ms |
| `warm_hit` (steady-state metadata hit) | 68.9µs | 70.0µs |
| `concurrent_cold` (8 threads, 100ms origin) | 101.8ms | 11.3ms |

(Pre-detach numbers: HTML metadata misses now schedule
fire-and-forget instead of waiting out the deadline, so the async
cold legs serve in microseconds. Re-run `cargo bench` to refresh.)

Bulk workload
(`crates/ps-html/examples/async_workload.rs`, 50 pages,
20ms origin, 10ms deadline, 1 pool thread):

| | Sync (no pool) | Async (pool) |
|---|---|---|
| Cold pass (50 requests) | 1022ms | 3ms |
| Warm pass (50 requests) | 3ms | 4ms |
| Origin fetches | 50 | 50 |
| Timeouts / completions | 0 / 0 | 0 / 50 |

Stage breakdown
(`crates/ps-html/examples/timing_breakdown.rs`, release):

| Stage | Before | After |
|---|---|---|
| Options signature (per call) | 1220ns | 59ns |
| Filter-chain construction (`Driver::new`) | 1937µs | 136µs |
| HTML rewrite, cold metadata (10 resources) | 2498µs | 557µs |
| HTML rewrite, warm metadata | 2449µs | 531µs |
| HTML-cache MISS + store | — | 45µs |
| HTML-cache HIT lookup (no filters/parse) | — | 42µs |

Run it with:

```sh
cargo build --release -p ps-html --example async_workload
/usr/bin/time -v ./target/release/examples/async_workload --pool --pages 50
/usr/bin/time -v ./target/release/examples/async_workload --no-pool --pages 50
```

## Limitations

- Optimization compute has no deadline: a slow *rewrite* (not
  fetch) still blocks the worker inline. Only origin latency is
  bounded.
- No mid-stream failure staging or async staleness freshening yet
  (`serve_stale_rewritten` revalidates inline, bounded by the
  deadline when a pool is registered).
- Shutdown degrades to inline execution (visible via
  `background_jobs_shutdown`); queue-full instead sheds fast
  (visible via `background_jobs_queue_full`), so only a server
  going away ever waits past the deadline.
