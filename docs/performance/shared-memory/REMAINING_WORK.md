# Remaining refinements after ef9aeb5

Source review at b3fdb21; this is not a fresh runtime or full correctness audit. F1–F7 implementation fixes are committed in ef9aeb5. Phase 10 remains open.

## 1. Investigate resource-serving polling (highest priority)

`crates/ps-html/src/rewrite_context.rs`: FETCH_DEDUP_WAIT is fixed at 250 ms; await_claimed_output repeatedly calls HttpCache::find and thread::sleep. This is the optimized-resource fetch/reconstruction path, not HTML rewrite delivery. The bound is a polling budget, not a strict end-to-end deadline: cache calls and scheduling can extend elapsed time, and local reconstruction follows timeout.

Next: trace actual NGINX caller/thread context, profile contended resource requests and unrelated concurrent HTML delivery, then decide whether asynchronous waiting or a configurable budget is warranted. Do not assume observed HTML gains prove resource delivery latency improved. Existing fake-coordinator tests cover claiming/timeouts; add actual competing-worker publication and client responsiveness coverage if changing this path.

## 2. Complete operational recovery tests

The audit explicitly leaves beacon nonce issuance/report merging paced across real graceful reload uncovered. Current lifecycle harness exercises HTML overlap, crashes, configuration changes and admin metrics, but does not send explicit beacon requests. Add a test retaining valid evidence identity across generations and rejecting nonce replay without losing accepted evidence.

Primitive saturation and adapter disk-preservation tests exist. Remaining gaps: end-to-end epoch exhaustion and the documented offline domain-replacement procedure after permanent shard bypass. Verify replacement only with all workers stopped; never delete live mappings. These are coverage gaps, not demonstrated corruption.

## 3. Complete performance acceptance

Current five-run default/shared cold/HIT/MISS data is saved under ef9aeb5-*.json. Single-worker shared hits show no consistent win; keep opt-in. Existing concurrency evidence is one workload/order and stress-test throughput; broader process-count, queue saturation, eviction/memory, duplicate-fetch/encode measurements remain useful before recommending wider rollout.

Use matched binaries/configuration and randomized repeated modes for causal performance claims. No new optimization should be justified solely by historic numbers from different clients/NGINX versions.

The historical production ~13 ms remains unattributed: the supplied localhost endpoint previously timed out. Isolated stage residual includes NGINX, uninstrumented Rust, sockets, scheduling and client overhead. A functioning endpoint and suitable tracing are required to isolate production and NGINX costs. perf kernel events were denied; sanitizers/model checking were not run.

## 4. Reconcile audit and progress presentation

SHARED_MEMORY_AUDIT.md retains pre-fix executive summary/missing-feature sections alongside a completed checklist and implementation log. Treat these as historical findings; add a clear supersession/status table rather than deleting the evidence. The progress ledger's older chronological entries also describe work pending at that time; current phase checklist and Next Session Resume Point take precedence.

No source change or deployment was made during this review. Next implementation task is item 1's caller/thread trace and contention measurement, followed by the smallest evidence-supported fix.
