# Remaining refinements after ef9aeb5

Source review at b3fdb21; this is not a fresh runtime or full correctness audit. F1–F7 implementation fixes are committed in ef9aeb5. Phase 10 remains open.

## 1. Resource-serving polling — refinement implemented and verified

Call trace confirmed: NGINX module.rs PagespeedResource content handler directly calls terminal::fetch_resource_body -> ResourceDriver::fetch_resource -> RewriteContext::fetch. Polling therefore ran on the worker event loop.

The request driver now exposes a bounded wait budget (default 250 ms retained for existing callers, maximum 250 ms). NGINX selects zero: one completed-output lookup, then the existing local reconstruction fallback. Claims/publication fencing remain active. This removes added polling sleeps, not synchronous cache lookup or image/CSS/JS reconstruction cost. A competing request may reconstruct in parallel instead of waiting; fully asynchronous resource serving would require a separate NGINX event-loop integration milestone.

Targeted serial debug test measured 248.634 ms with zero polling versus 519.508 ms with the existing 250 ms budget; both returned successful image output. Single mocked-contender measurements, not production or end-to-end NGINX benchmarks. Full image suite (124 tests), NGINX terminal suite (12 tests), and ordinary release build passed; exact commands/logs are recorded in the implementation ledger.

## 2. Operational recovery tests — requested coverage completed

`tools/verify_beacon_reload.py` runs real two-worker NGINX with explicit beacon POSTs across SIGHUP. Both pre-reload acceptance and post-reload acceptance of an old nonce pass. Each verifies a genuinely new worker, an overlapping old worker, exact persisted evidence counts/generation, nonce consumption and replay rejection. Three repeated runs per ordering passed; raw JSON/logs are in beacon-reload-repetitions.*. The fixture uses a small page and a supported browser UA; it does not claim additional HTML cache HIT coverage.

`tracked_coherence::tests::offline_domain_replacement_restores_admission_and_preserves_disk` verifies writer-slot exhaustion, persistent memory bypass, disk replacement/delete correctness, then a full drop of all old handles before offline tracker replacement. New-domain reads preserve disk state and actually repopulate memory. All 74 cache tests pass. This models the offline cache-domain procedure, not live replacement of NGINX mappings.

The separate end-to-end u64 epoch-exhaustion case remains optional coverage work; it is not the same condition as exhausted writer slots. Never delete live mappings.

## 3. Complete performance acceptance

Current five-run default/shared cold/HIT/MISS data is saved under ef9aeb5-*.json. Single-worker shared hits show no consistent win; keep opt-in. Existing concurrency evidence is one workload/order and stress-test throughput; broader process-count, queue saturation, eviction/memory, duplicate-fetch/encode measurements remain useful before recommending wider rollout.

Use matched binaries/configuration and randomized repeated modes for causal performance claims. No new optimization should be justified solely by historic numbers from different clients/NGINX versions.

The historical production ~13 ms remains unattributed: the supplied localhost endpoint previously timed out. Isolated stage residual includes NGINX, uninstrumented Rust, sockets, scheduling and client overhead. A functioning endpoint and suitable tracing are required to isolate production and NGINX costs. perf kernel events were denied; sanitizers/model checking were not run.

## 4. Reconcile audit and progress presentation

SHARED_MEMORY_AUDIT.md retains pre-fix executive summary/missing-feature sections alongside a completed checklist and implementation log. Treat these as historical findings; add a clear supersession/status table rather than deleting the evidence. The progress ledger's older chronological entries also describe work pending at that time; current phase checklist and Next Session Resume Point take precedence.

No source change or deployment was made during this review. Next task is remaining performance acceptance (item 3); asynchronous resource reconstruction remains a separately scoped improvement.
