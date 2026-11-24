# Remaining refinements after ef9aeb5

Source review at b3fdb21; this is not a fresh runtime or full correctness audit. F1–F7 implementation fixes are committed in ef9aeb5. Phase 10 remains open.

## 1. Resource-serving polling — refinement implemented and verified

Call trace confirmed: NGINX module.rs PagespeedResource content handler directly calls terminal::fetch_resource_body -> ResourceDriver::fetch_resource -> RewriteContext::fetch. Polling therefore ran on the worker event loop.

The request driver now exposes a bounded wait budget (default 250 ms retained for existing callers, maximum 250 ms). NGINX selects zero: one completed-output lookup, then the existing local reconstruction fallback. Claims/publication fencing remain active. This removes added polling sleeps, not synchronous cache lookup or image/CSS/JS reconstruction cost. A competing request may reconstruct in parallel instead of waiting; fully asynchronous resource serving would require a separate NGINX event-loop integration milestone.

Targeted serial debug test measured 248.634 ms with zero polling versus 519.508 ms with the existing 250 ms budget; both returned successful image output. Single mocked-contender measurements, not production or end-to-end NGINX benchmarks. Full image suite (124 tests), NGINX terminal suite (12 tests), and ordinary release build passed; exact commands/logs are recorded in the implementation ledger.

## 2. Complete operational recovery tests

The audit explicitly leaves beacon nonce issuance/report merging paced across real graceful reload uncovered. Current lifecycle harness exercises HTML overlap, crashes, configuration changes and admin metrics, but does not send explicit beacon requests. Add a test retaining valid evidence identity across generations and rejecting nonce replay without losing accepted evidence.

Primitive saturation and adapter disk-preservation tests exist. Remaining gaps: end-to-end epoch exhaustion and the documented offline domain-replacement procedure after permanent shard bypass. Verify replacement only with all workers stopped; never delete live mappings. These are coverage gaps, not demonstrated corruption.

## 3. Complete performance acceptance

Current five-run default/shared cold/HIT/MISS data is saved under ef9aeb5-*.json. Single-worker shared hits show no consistent win; keep opt-in. Existing concurrency evidence is one workload/order and stress-test throughput; broader process-count, queue saturation, eviction/memory, duplicate-fetch/encode measurements remain useful before recommending wider rollout.

Use matched binaries/configuration and randomized repeated modes for causal performance claims. No new optimization should be justified solely by historic numbers from different clients/NGINX versions.

The historical production ~13 ms remains unattributed: the supplied localhost endpoint previously timed out. Isolated stage residual includes NGINX, uninstrumented Rust, sockets, scheduling and client overhead. A functioning endpoint and suitable tracing are required to isolate production and NGINX costs. perf kernel events were denied; sanitizers/model checking were not run.

## 4. Reconcile audit and progress presentation

SHARED_MEMORY_AUDIT.md retains pre-fix executive summary/missing-feature sections alongside a completed checklist and implementation log. Treat these as historical findings; add a clear supersession/status table rather than deleting the evidence. The progress ledger's older chronological entries also describe work pending at that time; current phase checklist and Next Session Resume Point take precedence.

No source change or deployment was made during this review. Next implementation task after this refinement is operational recovery coverage (item 2); asynchronous resource reconstruction remains a separately scoped improvement.
