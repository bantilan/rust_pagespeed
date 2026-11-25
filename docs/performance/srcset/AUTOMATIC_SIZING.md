# Automatic responsive image sizing — implementation contract

Status: audit/design checkpoint only; automatic mode is NOT implemented. User scope changes from manually configured sizes to automatic desktop/mobile sizing. Existing manual implementation remains available; no live changes.

## Decisions

- Reuse existing image candidates; no variant generation, directory scanning, synchronous image fetch/decode or descriptor relabeling.
- Learn rendered CSS slot widths in the browser. Device classification alone cannot select sizes: viewport, orientation, CSS layout and image placement matter. Browser still chooses candidate according to viewport/DPR.
- Existing critical-images browser asset emits rd keyed by URL hash, but Rust ignores rd; current payload lacks viewport and merges repeated same-URL elements. Extend protocol rather than interpret existing URL-only measurements as element-specific layout evidence.
- Measurements must include viewport CSS width, rendered fractional CSS width (getBoundingClientRect), image identity stable across optimized URLs and placement, plus bounded candidates/layout identity. Do not confuse naturalWidth (density-corrected browser width) with encoded intrinsic pixels.
- Preserve current author sizes on first visit, missing/stale/ambiguous evidence, unsupported picture/art direction, no-transform, hidden/zero-width elements and uncertain layouts. Do not apply one mobile width globally or infer unobserved CSS breakpoints from sparse points.
- Derive conservative media-conditioned hints only for validated observed viewport ranges; retain original sizes as fallback outside learned ranges. Explicitly specify confidence/range semantics before mutation. A single observation does not prove a fluid/fixed layout function.
- Consider native sizes=auto for already-author-lazy images as an independent capability-gated milestone; browser evaluates actual layout. Preserve fallback sizes and existing loading priorities. Do not make eager/LCP images lazy to enable auto, do not apply auto to unsupported cases, and test browser support/fallback directly before delivery. This may provide immediate automatic lazy-image sizing while eager/LCP learning matures.
- Per-request device evidence must not leak into a shared HTML variant. Prefer one viewport-conditioned sizes expression; preserve existing options/generation/cache coherence, per-request nonces and RUM. Layout evidence changes invalidate dependent HTML/preloads via generation, with bounded updates rather than every report.
- No client post-load srcset/sizes rewriting that causes duplicate hero downloads. Preloads must use the same learned sizes expression as img.
- New automatic mode defaults off and needs one documented switch, not mandatory image class/custom CSS expressions. Exact option name to be finalized in configuration milestone; do not advertise a nonexistent directive.

## Independently verified milestones

- [x] A0 audit existing rd collection/receiver and record protocol gaps/design constraints.
- [ ] A1 specify exact automatic option/mode and native-lazy support matrix, defaultoff/signature tests; bounded measurement data model/codec tests. Decide precise image placement identity and observed-range confidence rules.
- [ ] A2 implement native automatic sizing for eligible existing lazy images with browser fallback tests, no loading/LCP changes. Separate benchmark/commit.
- [ ] A3 extend beacon JS/receiver with viewport+element identity/rendered widths, strict payload/count/numeric bounds, nonce validation, generation and TTL. Shared metadata transaction/crash/reload tests; preserve old beacon decoding. No markup inference yet.
- [ ] A4 conservative validated evidence consumer for eager images with explicit observed-range fallbacks; duplicate-image placement isolation, LCP preload alignment and HTML-cache tests.
- [ ] A5 desktop/mobile/orientation/DPR browser tests, repeated same URL in different slots, changing CSS/stale evidence, first visit, unsupported browser/noJS, multiworker/reload/RUM. Ensure no extra downloads or variants.
- [ ] A6 controlled off/on cold/resourcewarm HTMLMISS/HTMLHIT timings, JS/beacon bytes/CPU, queue/resource counts, README and release delivery. No production speed claims without measured evidence.

## Audit evidence

crates/ps-html/assets/critical_images_beacon_dbg.js:getImageRenderedMap emits rw/rh/ow/oh and merges duplicate URL hashes. critical_images_beacon.rs:init_call enables rendered map only for ResizeToRenderedImageDimensions. beacon_finder.rs explicitly documents rd ignored; BeaconReport has no rendered data. ps-nginx/src/beacon.rs:parse_beacon_report accepts LCP/nonce only. SrcsetSizesPolicy currently requires manual class+sizes. Existing manual Phases0–3 and Phase4 evidence remain valid for that feature; remaining manual latency tasks are carried into automatic-mode validation, not marked done.

## Next Session Resume Point

Read this document, SHARED_MEMORY_IMPLEMENTATION.md, git status/log and verify9srcset tests. Begin A1 by selecting the automatic switch/native-lazy eligibility and a bounded measurement model. Consult official browser/HTML documentation for sizes=auto support/semantics (not verified this session), then test actual installedChrome/Firefox behavior. No need to repeat old manual audit or benchmarks. Do not implement URL-onlyrd as per-element layout, guess CSS breakpoints, deploy incomplete mode or change the live site. Keep each tested milestone committed locally and update ledger after each task.
