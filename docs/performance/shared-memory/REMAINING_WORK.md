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

## 3. Performance acceptance — expanded evidence; attribution remains open

See [WORKER_MATRIX.md](WORKER_MATRIX.md): 18 randomized default/shared16MiB runs across 1/2/4 workers plus six four-worker default/shared1MiB runs, confirmed HTML phases, output equality, eviction and RSS/PSS/gauges. Seven gated queue-saturation/dedup probes verify bounded rejection and single local execution. No consistent shared-cache latency advantage or completed-job reduction; keep opt-in.

Remaining: directly measure unique fetch/encode work rather than treating local/global dedup counters as bytes/encodes saved; broader workloads and peak-memory/leak profiling before general production claims. Historical ~13 ms remains unattributed. Aerosky now responds with a 0.488 ms warm median in a small read-only sample, but installed module differs and strace attachment was denied. See raw artifacts and report; no security settings were changed.

## 4. Audit and progress presentation — reconciled

[The audit's current status](../../../SHARED_MEMORY_AUDIT.md#current-status--supersedes-the-historical-assessment-below) now separates resolved F1–F7 and verified follow-up coverage from historical findings. Original evidence remains intact, with historical headings/notes. The ledger's stale initial status sections were replaced by a current milestone index and verification scope; older chronological entries remain historical.

Next implementation task: direct per-resource fetch/encode measurement under contention. Production attribution remains open; fully asynchronous resource serving, end-to-end epoch exhaustion, and sanitizer/model verification require separate work. Documentation reconciliation does not mark Phase 10 complete.
