# Automatic responsive image sizing — implementation contract

Status: A0/A1a/A2 verified; native automatic sizing implemented for eligible author-lazy images. A1b and learned eager/LCP path pending. User scope changes from manually configured sizes to automatic desktop/mobile sizing. Existing manual implementation remains available; no live changes.

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
- [x] A1a automatic switch/native-lazy published compatibility, defaultoff/scope/merge/signature tests and actual Chrome fixture probe.
- [ ] A1b bounded measurement data model/codec tests, precise image placement identity and observed-range confidence rules (required before A3, independent of A2 native path).
- [x] A2 implement native automatic sizing for eligible existing lazy images with browser fallback tests, no loading/LCP changes. Separate benchmark/commit.
- [ ] A3 extend beacon JS/receiver with viewport+element identity/rendered widths, strict payload/count/numeric bounds, nonce validation, generation and TTL. Shared metadata transaction/crash/reload tests; preserve old beacon decoding. No markup inference yet.
- [ ] A4 conservative validated evidence consumer for eager images with explicit observed-range fallbacks; duplicate-image placement isolation, LCP preload alignment and HTML-cache tests.
- [ ] A5 desktop/mobile/orientation/DPR browser tests, repeated same URL in different slots, changing CSS/stale evidence, first visit, unsupported browser/noJS, multiworker/reload/RUM. Ensure no extra downloads or variants.
- [ ] A6 controlled off/on cold/resourcewarm HTMLMISS/HTMLHIT timings, JS/beacon bytes/CPU, queue/resource counts, README and release delivery. No production speed claims without measured evidence.

## Audit evidence

crates/ps-html/assets/critical_images_beacon_dbg.js:getImageRenderedMap emits rw/rh/ow/oh and merges duplicate URL hashes. critical_images_beacon.rs:init_call enables rendered map only for ResizeToRenderedImageDimensions. beacon_finder.rs explicitly documents rd ignored; BeaconReport has no rendered data. ps-nginx/src/beacon.rs:parse_beacon_report accepts LCP/nonce only. SrcsetSizesPolicy currently requires manual class+sizes. Existing manual Phases0–3 and Phase4 evidence remain valid for that feature; remaining manual latency tasks are carried into automatic-mode validation, not marked done.

## Next Session Resume Point

A2 native automatic mode verified. SrcsetAutoSizes on now emits auto+originalfallback for eligible existing native-lazy dimensioned widthsrcset img, without manual class/rule. Manualmatchingwins; picture/no-transform/highpriority/unknownlayout/customlazy skips; activeJS LazyloadImages disables only nativeauto path, preservesexistingJSfilter. Fullrequestlatency/RUM/cache/nativeunsupportedbrowser checks remain A5/A6. Next A1b before A3: bounded viewport+per-element measurement model/codec, placement identity and conservative observed-range rules for eager/LCP learning. Existing rd URLhashmerges duplicateplacements; cannot consumeas-is. Nativeauto does NOT solve eagerhero. Read A2verification commands/artifacts below; inspectgitstatus/log and rerun12srcsettests before edits. No deployment/livechanges; no commands running. Do not repeat native24caseprobe unless sourcechanges.

## A1a contract before source edits

Split A1 into small checkpoints: A1a config+native browser probe; A1b bounded measurement model/identity/range rules remains before A3. Option SrcsetAutoSizes Bool=false, Directory scope, no query override, table-backed merge/signature isolation. No runtime consumer in A1a; do not advertise switch as functional output yet. It is independent of manual class/sizes settings. A2 native eligibility: existing author loading=lazy, bounded valid width-srcset, dimension/layout safeguards, preserve fallback sizes, skip picture/no-transform/explicit high priority until tested. Do not change eager loading; manual policy precedence must be explicit (manual matching wins). Nativeauto can alter intrinsic-size containment; require dimensions/layout regression testing rather than assume no layout impact. Existing PageSpeed lazyload is JSplaceholder deferral, not automatically native loading=lazy.

Primary sources checked2026-11-25: https://html.spec.whatwg.org/multipage/images.html#sizes-attributes (auto uses rendered objectwidth, fallback list, intrinsiccontainment dimensions); https://raw.githubusercontent.com/mdn/browser-compat-data/main/html/elements/img.json (sizes.auto:Chrome126+,Firefox150+,Safari27 partial SVGobject-fit issue). This is published compatibility data, not local browser verification. ActualChrome probe will use supplied fixtureimages/knownCSSwidth; Firefox/Safari not locally verified in this checkpoint.

## A1a verification

Configuration groundwork only, no HTML behavior change. Newtableoption/getter + tests and NGINXdirective parser regression.91ps-configtests pass vs90baseline,1NGINXdirective,9existing srcset. Existingwarnings retained. New optional --native-auto-probe in verify_srcset_browser.py generates native loadinglazy/auto staticfixture while both manualpolicies off; clearly labels results as browserprobe, not automaticfilter. Normal defaultmode unchanged and24case regressionpasses. Bothfixtures use actualisolatedNGINX+Chrome158.0.8127.88, existing release moduleSHA recorded inJSON (predates newconfig, intentionally no newdirectiveused byprobe). No release/deployment claimed.

Chromeprobe24cases viewport352/412/1024×DPR1/1.75/2/3×off/on: CSSwidthsunchanged, actualfilewidth≥needed up to largestcandidate, oneimage request each, enabledbytesnevergreater.3pairs smaller,9same;412×1.75still768w. No Safari/Firefox localverification; MDNBCD support table checked separately, no claim unsupportedbrowser fallback verified. NoJSdisabled test, intrinsic-layout-unknown or eagerauto test yet; required for later milestones.

Exactcommands/results:
```bash
cargo test -p ps-config --lib --quiet
cargo test -p ps-nginx --lib directive_lines_dispatch -- --test-threads=1
python3 tools/verify_srcset_browser.py --native-auto-probe --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/auto-a1-chrome.json
python3 tools/verify_srcset_browser.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/auto-a1-manual-regression.json
cargo test -p ps-html --test srcset_sizes --quiet
```
Logs/rawJSON auto-a1-* retained. rustfmtoptions.rs/gitdiffcheck pass. Modifiedconfig options/table/count,NGINXdirective test,browsertool,plan/ledger. No runtimeperformancechange or productionbytesavings claimed. All isolatedprocesses closed.

## A2 contract before edits

Reuse cached SrcsetSizesPolicy with independent auto flag. Manual exact-class match wins; otherwise auto requires existing native loading=lazy, unique positive integer width+height (bounded32bytes), unique bounded widthsrcset/sizes, no high fetchpriority or onload/placeholder lazy attributes. Preserve original fallback sizes by prepending exact auto,; absent/empty sizes gets auto, 100vw. Existing valid auto prefix untouched. Skip automatic path when JS LazyloadImages filter enabled, since its placeholder replacement is incompatible until dedicated interoperability exists; do not disable JS filter. Manual behavior unaffected. No class/manualexpression required. Retain picture/no-transform/context/bounds guards. Release/module browser fixture must exercise actualswitch, not just static nativeauto. No eagerhero learning or native-layout inference beyond supplieddimensions.

## A2 verification

Runtimepolicy nowhas independentautomaticflag, sharedthroughcachedChainPlan; requeststate unchanged. Manualmatching takesprecedence. Nativepath checks unique existingloadinglazy,width/heightpositiveu32, boundedwidthsrcset, optionalnonhighfetchpriority, no onload/customplaceholder. Sourceattributes exceptsizes unchanged; currentauto preserved, authorfallbackretained or100vw. Encoded/caseattrs, duplicates/oversize, eager/no-layout/highpriority/no-transform/picture, manualprecedence, JS-lazyfallback tested. ExistingJSloader remainsenabled; automaticpath disabledwhenconfigured. No beacon/eagerlearning/encoder/resource changes.

Commands/results:
```bash
cargo test -p ps-html --lib --test srcset_sizes --test driver --test prioritize_lcp_images -- --test-threads=1
cargo test -p ps-nginx --lib -- --test-threads=1
cargo test -p ps-html --test image_rewrite img_srcset -- --test-threads=1
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
cargo build --release -p ps-html --example srcset_driver_latency
python3 tools/verify_srcset_browser.py --automatic-filter --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/auto-a2-browser.json
python3 tools/verify_srcset_browser.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/srcset/auto-a2-manual-browser.json
target/release/examples/srcset_driver_latency both 100 auto > docs/performance/srcset/auto-a2-driver.json
```
171HTMLlibrary/29driver/26LCP/12srcset/293NGINX/2existingimagerewrite tests pass. Matchedrelease38.67s; diagnostic50.38s. ActualmoduleautomaticfilterChrome24casespass plusmanual24casespass; oneimage request per automaticcase, CSSwidthunchanged, no greatercheckedbytes, sufficientphysicalcandidateuptolargestavailable. ModuleSHAe268631edf188beea68fd29f41ffc00321c76f18a04a0870415f7ce1f37db743. Fixtureexplicitwidthheightandwidth100%CSS; no universal-layout guarantee, noFirefox/Safarilocaltest. Toolsseparate--native-auto-probe(staticfixture) from --automatic-filter(actualswitch).

Costdiagnostic sequentialafterbuilds/tests/browser:9alternatingbatches×100iterations,100warmups,34KB/1000paragraphfixture; authorlazy dimensions and automaticoption only. Raw auto-a2-driver.json and stderrpolicy label retained. Off/on warmstage medians (microseconds):
- Resource driver setup: 1.377/1.059.
- Filter setup: 7.526/6.900.
- HTML+drop: 760.849/734.010.
- Total: 770.879/741.929.
Zero resourcefetches asserted. Enabledmode measured lower in thisrun; this is noise-sensitive observation, NOT proven speedup or NGINX/requestlatency. Cold samples are single firstdrivercalls, not useful distributions. A6stillneeds controlledcold/warmresourceMISS/HTMLHIT andlargeinputlatency. No deployment or productionbenchmark.

READMEactualswitch/eligibility/conflicts/limitsupdated; rustfmtnewRust/test/example and gitdiffcheck pass. Modifiedfilterpolicy/apply,srcsettests,browsertool,diagnosticexample,README,plan/ledger,rawlogs. Immutableconfigurationreused; no newstatsregistration/fetch/encode/jobs. NextA1bmodel thenA3collection, not eagerauto injection.

## Live report and explicit LCP requirement

User installed A2 and requests LCP inclusion. Read LIVE_FINDINGS.md and live-psi-20261125.json before A1b. Currentmobilebothimagesareeager;LCPfetchpriorityhigh;A2correctlyskipsboth. MobileLCP1.201s/CLS0,TBT0;45KiBestimatedimagesavings,768selectedvs616target. Need learned eager/LCP path, not enablelazy onhero. No actual616/640candidatebetween300/768; correctsizesmaynotremovemobilewarning. Desktop645slotcouldchooseexisting768ratherthan1024. Do not claim userconfigurationwrong. ExplicitA4acceptanceincludesimgsizes/generatedpreloadimagesizesmatched, no duplicaterequests,firstvisit/unknownviewportfallback and no cache/RUMregression.
