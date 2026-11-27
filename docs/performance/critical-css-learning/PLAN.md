# Automatic critical CSS learning — resumable plan

Baseline a81740e, 2026-11-27. User requested continuation after HTML-cache
compatibility. Current live deployment is not changed by this implementation.
Read this file, SHARED_MEMORY_IMPLEMENTATION.md, git status and recent commits
before resuming. Commit only tested, independently functional milestones.

## Decisions and dependencies

- Preserve exactly one accepted critical/LCP decision per page/options and six
  public format/device groups. CSS joins that same nonce/report/transaction;
  do not add an independent recurring CSS collector or finalize LCP prematurely.
- Candidate selector summaries must come from server-authorized CSS inputs.
  Bound counts, individual length, aggregate bytes and encoded payload. Never
  accept arbitrary non-candidate selector sets into shared evidence.
- HTML requests read metadata only for external stylesheets. Missing summaries
  schedule existing bounded/deduplicated background work, retaining full CSS and
  original URLs. No synchronous external resource fetch or unbounded waits.
- Inline CSS and stylesheets use existing parser, media checks and
  css_util::js_detectable_selector. Original C++ critical_css_beacon_filter.cc
  skips alternate/noscript/non-screen CSS, waits for pending summaries, queries
  candidate selectors in browser, retains unparseable CSS, and keeps full CSS
  in the delayed loader. Do not mistake querySelector DOM usage for a measured
  above-the-fold selector set.
- Incomplete, over-limit, inaccessible or failed summaries must keep full CSS;
  never truncate evidence and then strip unsampled rules. An accepted empty
  selector result is distinct from no CSS report. Existing selector filter
  conservatively retains full CSS for empty results.
- Preserve HTML-cache selector identity/publication checks from a81740e, private
  eligibility, RUM, font learning, responsive/LCP/image/CSS/JS processing.
- Version optional evidence extensions compatibly. Old records decode unchanged.
  Production activation occurs only after full end-to-end tests; unconnected
  phases must not claim automatic learning is available.

## Milestones and acceptance

- [x] Phase 0: audit source/assets/receiver and original C++ reference; persist plan.
- [x] Phase 1: bounded CSS candidate/decision codec, existing evidence extension,
  atomic combined-report receiver, same six cohorts, nonce/replay/first-result
  enforcement; no active collector yet. Tests legacy/corrupt/limits, comma-containing
  selectors, malformed/unknown/missing reports, fresh/replayed nonces and cache
  identity. Run baseline and final relevant suites, build, record and commit.
- [ ] Phase 2: authorized async stylesheet summaries and inline selector capture;
  imports/media/noscript/alternate/charset/limits; background cache miss dedup,
  retry/failure behavior, expiration, no synchronous resource reads on HTML path.
- [ ] Phase 3: one browser collector completes both CSS and LCP evidence in one
  report; bounded selector batches, safe DOM querying, navigation-safe transport,
  no duplicate POST; CSS-only option also works; prepare nonce only once all
  required inputs are ready. Do not finalize an LCP-only report for a CSS-required
  cohort. Cached unlearned HTML must not suppress eventual sampling.
- [ ] Phase 4: enable metadata-backed selector lookup and summaries in actual
  normal/early caching; dirty input invalidation, output snapshot races, one
  accepted combined result, full stylesheet fallback, no repeated learning BYPASS.
- [ ] Phase 5: actual Chrome + real NGINX/FastCGI oracle, multiprocess/reload/replay,
  private/unknown cookie and unsupported-UA controls, full regression, cold/warm
  MISS/HIT benchmarks, README, matched release and final commit.

## Current state / Next Session Resume Point

Phase 0 and Phase1 complete and tested. No automatic browser collector enabled.
Next implement Phase2 metadata summaries/async authorized CSS discovery; then
Phase3 combined collector and protocol signature generation, Phase4 lookup and
Phase5 real browser/NGINX checks. Phase1 commands/results in PHASE1.md.
Baseline commands/results and modified files will be recorded below after each
completed task. Exact source changes in git; do not infer verification from presence.

Phase1 progress: bounded CssEvidence candidate/decision codec, optional shared
record extension, combined report completeness/whitelist validation and strict
encoded-component cs parsing implemented. 225 HTML library tests pass, including
legacy codec, valid empty decision vs absent, malformed/limits, encoded comma,
candidate generations/nonces and immutable first result. Receiver tests pending.
No active producer registers CSS candidates yet, so existing image/LCP behavior
remains unchanged. Baseline cache-API fixture uses retained release executable
from a81740e (not rebuilt from modified source), 2000HITs/cell, stored CSV.
Activation requirement for Phase3: effective configuration signature must version
the CSS protocol so mixed old/new workers cannot drop the optional extension or
accept partial legacy reports into an active combined cohort.

Phase1 verification progress: HTML225library, criticalimage6/cacheCSS4/Vary8/LCP26
integration, strict query5, NGINX295 tests pass. New receiver integration proves
missing/malformed/unknown/duplicate cs cannot consume nonce or finalize; escaped
comma survives; accepted combined report increments generation/count once; replay
cannot change record; no write to other format group. Matched release build
running; no automatic producer activation or production changes.

Phase1 matched1.31.10 release PASS25.17s; finalcachefixture2000HITs/cell. All commands complete, gitdiffcheckpass. Commit tested backend milestone locally without push.
