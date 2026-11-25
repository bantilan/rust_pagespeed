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
- [x] A1b model/codec: bounded observations, exact-viewport rules, confidence/conflict/fallback and corruption tests.
- [x] A1b identity primitives: bounded server-scope placement digest, streaming original-document hash and bounded wire/manifest checks.
- [ ] A3 integration prerequisites: runtime identity producer, original-document completeness/streaming fallback, stylesheet epoch/TTL invalidation, nonce-bound issued manifest (identity helpers alone do not implement these).
- [x] A2 implement native automatic sizing for eligible existing lazy images with browser fallback tests, no loading/LCP changes. Separate benchmark/commit.
- [x] A3a nonce-bound transactional evidence store, codec/caps/TTL/epoch/effectivehintgeneration; memorythread and Linuxmultiprocess replay/update tests.
- [x] A3b bounded browser collector asset and actual NGINX receiver route; realChrome transport checks and registerednonce receiver/legacybeacon tests.
- [ ] A3c trustedruntimeidentity/manifest producer, sampling/nonceentropy/stamping/injection/cachegates; actualmetadataadapter/crash/reload integration. No markup inference yet.
- [ ] A4 conservative validated evidence consumer for eager images with explicit observed-range fallbacks; duplicate-image placement isolation, LCP preload alignment and HTML-cache tests.
- [ ] A5 desktop/mobile/orientation/DPR browser tests, repeated same URL in different slots, changing CSS/stale evidence, first visit, unsupported browser/noJS, multiworker/reload/RUM. Ensure no extra downloads or variants.
- [ ] A6 controlled off/on cold/resourcewarm HTMLMISS/HTMLHIT timings, JS/beacon bytes/CPU, queue/resource counts, README and release delivery. No production speed claims without measured evidence.

## Audit evidence

crates/ps-html/assets/critical_images_beacon_dbg.js:getImageRenderedMap emits rw/rh/ow/oh and merges duplicate URL hashes. critical_images_beacon.rs:init_call enables rendered map only for ResizeToRenderedImageDimensions. beacon_finder.rs explicitly documents rd ignored; BeaconReport has no rendered data. ps-nginx/src/beacon.rs:parse_beacon_report accepts LCP/nonce only. SrcsetSizesPolicy currently requires manual class+sizes. Existing manual Phases0–3 and Phase4 evidence remain valid for that feature; remaining manual latency tasks are carried into automatic-mode validation, not marked done.

## Next Session Resume Point

A3b collector+receiver checkpoint verified. New3056byte automatic_sizes_collector.js exportedviaCOLLECTOR_JS, actualNGINXanswer_beacon callsboundedhandle_automatic_sizes(asm/asi/asn/url)->singlecohortaccept. Serverregistrationonly; noHTTPregister. 294NGINX+184HTMLlibrary+12srcsettests pass;8actualChromeHTTPsinkcases pass,oneoriginalimagefetch,attrsunchanged,duplicate/hidden/picture/no-transform/cap skips,sendBeaconfalse/throwfetchfallback,crossoriginrefusal. Lockedcheck1.15s. Producer/injection STILLUNWIRED,automaticLCPinactive. Next A3c: trustedcompleteoriginaldocument/styleidentityproducer/streamflushfallback,requestcohortisolation,nonceentropy+manifestregistration/stamping/injection gatedonmetadata success andcache sampling eligibility. Mustincludeepoch/generation/expiryinlearnedHTMLkeys/earlyHIT validation andensure nonce-bearingcollectorHTMLnotcachedorreused,whileexistingRUMretained. Existingcriticalbeaconprotocol unchanged. Then A4consumelearnedsizesbeforeLCPpreloads. No manualbreakpoints/no variants/deployment;allcommandsfinished.

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

## A1b automatic breakpoint model — before edits

User explicitly rejects manualbreakpointconfiguration. No SrcsetBreakpointSizesoptionwillbeadded. Learn measuredCSSwidth at viewportwidth; never infer arbitraryCSSbreakpointintervals from sparse samples. Firstconsumer emits exact observedviewport clauses, e.g.(width:412px)352px, followed by originalauthorfallback. Require3acceptedindependentlynoncevalidatedreports; confidence is stability check, not proof allpersonalizedlayouts equal. Persist min/maxwidth at1/64CSSpxprecision; conflictingobservations>1CSSpx disable thatviewport until newlayoutidentity/expiry. Ceilmaxwidth to integerCSSpx so hints neverroundbelowmeasurement. Cap16viewports/record,2048encodedbytes,viewport1..16384,width1..16384CSSpx,1024fallbackbytes. One record scoped to hashed page/options/layout/candidate identity AND elementplacement; URLs alone are insufficient. Epoch/versioncodec rejects invalid/duplicate/oversizedinput; namespace distinct from existingbeaconevidence. A1b implements puremodel/codec only, no fakeautomaticLCPconsumer; A3mustderive/checkidentityserver-side, validate nonce and per-pagecaps/TTL/sharedcachetransactions, then A4 applies in initialHTML beforepreloads. Generation mustchangeonlywhen effectivelearnedhintschange, not count-onlysampling, toavoidHTMLcachechurn. NativeA2remainsunchanged.

## A1b model/codec verified checkpoint

Newautomatic_sizes.rs exportsMeasuredSizes; uncalledpuremodel with noI/O/filtercallbacks/queues/stats. Identitymustbeverifiedserverdigest; producer/hashderivationandlayoutinvalidationNOTimplemented. Collector/receiver mustauthenticateindependentnonces; observe aloneisNOTreplayprotection. Eachrecord16viewports, positiveviewport≤16384/CSSwidth≤16384, fixed64ths,min/max,countcap3. Conflicts>1CSSpxpermanentlydisableviewportunderthatidentityuntilTTL/epochreset; no extrapolation. Outputceilmaxwidth,exact(width:Npx) mediaqueries only,authorfallback verbatim (max1024bytes). Three samples are a conservative heuristic, notproofagainstpersonalizedlayouts; producerisolationmusthandlethat. Codecmeasured-sizes/1,strictdecode≤2048bytes,duplicate/invalidfield/count/versionreject. Cachetransaction/TTL/generation still A3. No nonce-bearingexistingcodecmodified.

Tests: cold/missingconfidence returnsNone, stablemobile+desktop widthclauses, fractionalwidthroundsup, wrongidentity/oversizefallbackreject, conflict+latersamples remainssuppressed, capacity/rangerejectbeforemutation,cache roundtrip/corruptversion/duplicate/singleinconsistentobservation rejected.

```bash
cargo test -p ps-html --lib --quiet
cargo test -p ps-html --test srcset_sizes --quiet
rustfmt --edition 2021 crates/ps-html/src/automatic_sizes.rs
git diff --check
```
174librarypass0.29s(previous171+3newtests),12srcsetpass0.01s; auto-a1b-tests.log/auto-a1b-srcset-tests.log. No releasebuild/browserbenchmark needed for uncalledmodel; nativeautomaticfilter behavior unchanged. No totalrequestimprovementclaimed. Modifiedautomatic_sizes.rs/libmodule,ledger/plan/testlogs. No livechanges; exactnextaction above.

## Placement identity + wire validation milestone — before edits

A1b integration groundwork: SHA256 domain-separated length-framed digest includes fullpageURL/effectiveoptionssignature, originaldocumentdigest, stylesheet/layout epoch, document-ordereligible-imageordinal, originalsrc/srcset/fallbacksizes. Inputsboundedbeforehash;64placements/page. Hash is identity, NOTauthorization; only server-issued manifest tied to authenticatednonce may permitreports. Originalpreoptimizationbytes avoidformatURLdrift. Changesinlayout/candidates/position/optionscannotsharemeasurements. A3mustcomputeoriginaldocumentdigest incrementally (no buffering/fullHTMLcopy), resolve stylesheet epoch/invalidation and streamingfallback beforeapplyingmeasurements. Purehelperrequiresservertrustedcontext; no arbitrarybrowsercontextaccepted. Newboundedversionedwirebatch≤8192bytes/64uniqueidentities,viewport1..16384,width64ths1..1048576;strictnumeric/noextras/duplicates. Unknownissuedidentityrejectwholebatch. Parserdoesnotconsumenonceorwritecache; actualsharedtransactionintegrationnext, existingbeaconcodecunchanged. SHA256dependencyadded explicitly, no collision-prone truncation or URL-onlyhashreuse.

## Placement identity/wire validation — verified checkpoint

Newhelpers are pureanduncalled; no collector/receiver/driver/HTMLcache changes. placement_identity rejects oversizedfields/invalidordinal/unsupportedwidthlist beforehashing. SHA256domain+64bitlength framing separates inputs; options/page/document/styleepoch/elementposition/originalsrc/srcset/sizes allcovered. URLhashcannotidentifyduplicateplacements; tests confirmdistinctordinal givesdistinctdigest. A digest is NOTauthentication: receiver MUSTverifyissuedmanifest+nonceexpiry/replaytransaction. matches_manifestalsorevalidatespublicbatchfields.

LayoutFingerprint streams originalbytes independentofchunkboundaries with no retainedHTMLcopy;8MiBcap disableslearningonly, ordinaryrewriteunaffected. Onlyfinish exposesdigest; caller mustnotstamp/apply beforeoriginaldocumentiscomplete. Parserflushstreaming behavior andtrusted stylesheetepoch producer stillunimplemented. ExternalCSSchangehandling mustbespecified/tested beforeA4; no silentfreshnessassumption.

MeasurementBatchwire:automatic-sizes/1 viewport followedbyuniquehexidentity/width64thslines;≤8192bytes,1..64slots,viewport≤16384,width≤1048576(1/64CSSpx),noempty/duplicate/extra/float/signedfields. Unknownissuedidentityrejectswholebatch; no partialmutation. RawHTTP/queryencodinglimit and noncevalidationremainreceiverobligations. Existingbeaconcodec/measurementcache remainuntouched.

Dependencysha2=0.10addedtops-html,Cargo.lockpins0.10.9;cfg-if/cpufeatures/digestalreadyexisting. No network service calls fromnewcode. Testcommands:
```bash
cargo test -p ps-html --lib automatic_sizes -- --test-threads=1
cargo test --locked -p ps-html --lib --test srcset_sizes -- --test-threads=1
cargo check --locked -p ps-nginx
rustfmt --edition 2021 crates/ps-html/src/automatic_sizes.rs
git diff --check
```
Focusedbaseline3tests passed;firstexpanded5tests passed(auto-identity-tests.log);finalfullsuite177library+12srcsetpassed0.69s/0.01s(auto-identity-regressions.log). NGINXlockeddevcheck4.06s(auto-identity-build.log),notreleasebuild/deployment. Added3tests totalsince174:placementisolation/inputbounds,wireparse/manifest/caps,andstreamhash/chunk/overflow. Nativebrowsertestsnotrerunbecause runtimecallsitesunchanged. No requestbenchmarkaddedforuncalledhelpers; SHA256cost mustbemeasuredwhenintegrated. Allcommandsended,nolivechanges/no push.

## A3a transactional evidence store — before edits

Onekey pertrusted page/options/document/style scope containsimmutableissuedslotset≤64,noncehistory≤128(pending+consumed),MeasuredSizes≤64,5minute fixedrecordTTL,60secondnoncelifetime andeffectivehintgeneration. Registration requires exactsameissuedmanifest underalive scope; conflictfailsclosed. Noncereusedunderalive recordrejected evenafterconsumption. Acceptance validateswholebatch andissuedmembers,mergesallor-none,consumesnonceinSAMEmetadataCache::update;failedbounds/unknownslots do notconsumeit. Generationchangesonlywhen sizes(id,emptyfallback) changes(count-onlysamplesnot),oneincrementperbatch. Contention/errorlookup =>retry/None,not accepted. Availablelookup requiredforvaliddecode; corruptioncannotrestore nonce. Requires same transactional backend as existingbeaconcohort; plain Cache defaultupdate is notcrossprocesssafe. Codec≤256KiB,strictcounts/uniqueids/version/base64/numeric checks;TTLnotextendedbyreports. No HTTP/JSactivation inthismilestone; nextconnecttrustedproducer/collection/receiver, preserveoldbeaconprotocol. Tests memorythreads+LinuxprocesssharedFileCache/TransactionalCache, replay,expiry,capacity,manifestmismatch,conflictandgeneration.

## A3a verified transaction-store checkpoint

New automatic_sizes_store module plus MeasuredSizes.identity getter. No Driver/NGINXhandler/JS/HTMLcachecallbacks modified; this stage is API implementation only, not liveLCP support. Store API requires backend with process-safe Cache::update; default traitimplementation doesnotguaranteecrossprocessatomicity. Tests useMemoryCache(threadlock) andFileCachewrappedinTransactionalCache(Linuxrobusttransaction). Backendwritepersistenceisbest-effort as existingCachecontract; no strongercrashdurability claimed.

Recordnamespaceautomatic-sizes-cohort-v1:<server-scope-digest>, versionautomatic-sizes-store/1, strict≤256KiBdecode, uniqueissued64hexIDs≤64,noncesexact11web64chars≤128, measurements≤64 andvalididentitymatching. Ownseparatecohort, existingbeaconcodecremainsunchanged. Pendingandconsumednoncehistorykeptuntilfixed5minTTL,noreuseunderaliveepoch. Registrationrequiresmatchingimmutablemanifest. Nonceexpires60s,negative/future-issued/expiredtimefailsclosed. registernewcohortresetusesfirstnonceasepoch;dependentHTMLmustincludeepoch+generationandexpiry soTTLresetgen0cannotcollidewitholdgeneration. TTLneverextendedonreports.

Acceptance underonetransaction: validissuednonce+manifest,mergeallrecords,consumeexactnonce,writeoneblob. Unknownidentity/invalidfields/capacity returnswithoutcommit;nopartialmergeorconsumednonce. Firsttwostablesamples persistconfidence only,thirdfirsthintbumpsgeneration;stablefurtherreportsnot,badlayoutconflictsuppresshintbumpsgeneration. Onebumpperbatch,checkedoverflow. CachecontentionanderrorlookupNone=>retry/fallback,notfalseacceptance. loadonlyAvailableandunexpiredstate, exposesepoch/expiry/gen/measurements,no noncehistory.

Tests/commands:
```bash
cargo test --locked -p ps-html --lib automatic_sizes_store -- --test-threads=1
cargo test --locked -p ps-html --lib --test srcset_sizes -- --test-threads=1
cargo check --locked -p ps-nginx
rustfmt --edition 2021 crates/ps-html/src/automatic_sizes.rs crates/ps-html/src/automatic_sizes_store.rs
git diff --check
```
Focused6testpass beforefinalcapacityrollbackaddition(auto-store-tests.log); final184library+12srcsetpass0.72s/0.01s(auto-store-regressions.log). 7newstoretests:8threadsoneacceptednonce,4Linuxchildprocessessharednonce+distinctnonces(all5consumed/measurementsretained),nonceexpiry/replay/capacity/TTLepoch/corruption,unknownmanifest/no mutation,contention/errorlookup,confidence/conflictgeneration,partialmultiimagecapacityrollbackandvalidretry. LockedNGINXcheck1.48s(auto-store-build.log). Initialcheckalso passes(auto-store-initial-build.log). No releasebuild/browserrerunneededforuncalledAPIs; no requestlatencyclaim.

NextA3bmustwiretrustedidentity/manifestregistrationandcollection; originaldocument/styleepochproducerandstreamingfallbackremainunimplemented. No endpointactivation,nonceentropyprovider,HTTPPOSTencodinglimit,JScollector,cross-workerreload/crashpipelineoractualNGINXcacheadaptervalidation claimed. A4mustrecheckcohortexpiryatearlyHTMLcacheHIT beforeusinglearnedhintbody; TTLreset/epoch/measurementgenerationneedsintegration. Alltemporarymultiprocessfixturescleaned,commandsfinished,nolivechanges.

## A3b collector/receiver checkpoint — before edits

ImplementactualNGINXreceiverandbrowsercollectorbeforeproduceractivation. Wireparametersasm(versionedMeasurementBatch),asi(scope64hex),asn(nonce11web64),url(existingpageURL); caprawrequest64KiB/≤16params beforeownquerydecode,decodedbatch8192bytes;singletonallfields/validpageURL/strictwire->MeasurementStore.accept. No registrationfromHTTP. Existingcriticalbeaconstats/handlerunchanged; sizeacceptance usesowncohorttransaction; alwaysnormal204HTTPsemantics,no errorsleakingnonce.

Collector standaloneassetinvokedwithtrustedserverconfiguration: sameoriginendpoint,scope/nonce; scansdocument.images onlyif≤512,stamped64hexIDs≤64,duplicatesexcluded;picture/noscript/template/svg/customcontexts skipped;hidden/zerolayout/nonfinite/oversize skipped. CSSviewportmatchmedia exactinteger; renderedwidthceil64ths, no post-loadsrc/sizesmutationorimagegeneration. Send once afterload viaexistingendpointURLencodedPOST;wire/postcaps;no networkifempty/invalid;sameoriginonly. UseactualChromeharnesswithsyntheticHTTPsink tocheckwire,duplicateplacements/IDs,hiddenandbounds/noimagemutations. Receiverunittestsseedserverissuedmanifestandtestnonce/replay/expiry/unknownslot/payloadcaps/legacybeaconcompatibility. Producertrustedlayout/styleepoch/stamping/nonceentropy/HTMLcacheactivation remainA3c; noautomaticLCPoutputyet.

## A3b collector/receiver verified checkpoint

ActualNGINX beaconanswer nowcalls handle_automatic_sizes using servingServerContextmetadata. Newfieldsonly; oldcriticalimages/RUMhandlersunchanged. NoHTTPregistrationandnoactualproductioninjectionyet. Incomingownpathchecksraw≤65536bytes/≤16paramsbeforeownQueryParamsdecode,exactlyoneurl/asi/asn/asm(includingvaluelessduplicates),validwebpageURL≤8192,scope64chars/nonce11,escapedwire≤24576/decodedwire≤8192,strictMeasurementBatchandstoreissuednonce/membership. Unknown/expired/replayedfailclosed; cacherejectionNonefollowsordinary204handlerbehavior. Scope+noncefromserverregistryauthorizecohort;pageURLisnottrustedtosynthesizecontext. Otherexistingbeaconconsumersstilluseexistinglimits; this isnotageneralrequest-bodylimitrefactor. ActualreceiverunitseedssameServerContextmetadataandchecksreplay,invalidcaps/duplicates,unknownslots,nolegacybeaconmutation. NoJSONparserdependency.

CollectorassetactualChromevalidated,3056uncompressedbytes. APIpagespeed.collectAutomaticSizes(endpoint,scope,nonce); producerNOTcallingyet. NoURLs/srcset/sizes writes. Onlysameorigin,feature-checkedmodernAPIs;oneloadreport,DOMimages≤512,validuniqueplacementIDs≤64. AmbiguousduplicateIDsdrop,hidden/zero/nonfinite/oversize/picture/no-transformskip;widthceil64ths,max(rect/clientwidth) avoidsunderestimateonscaleddownbox. ExactCSSviewportmatchmediaandresizeguard;URLencodedwire/postcaps. SendsBeaconwithtypedBlob;false/throwfallsbacktosameoriginfetchkeepalivewithcaughtfailure. UnknownAPI/noeligibleimages=>noreport. Firstviewportgiventolayoutmodelbyreceiver; no guessingCSSbreakpoints.

Commands/results:
```bash
cargo test --locked -p ps-nginx --lib beacon -- --test-threads=1
python3 tools/verify_automatic_sizes_collector.py --output docs/performance/srcset/auto-collector-browser.json
cargo test --locked -p ps-nginx --lib -- --test-threads=1
cargo test --locked -p ps-html --lib --test srcset_sizes -- --test-threads=1
cargo check --locked -p ps-nginx
rustfmt --edition 2021 crates/ps-nginx/src/beacon.rs crates/ps-html/src/automatic_sizes.rs
git diff --check
```
5focusedbeacon/routingtests passed(auto-receiver-tests.log); full294NGINX0fail2.49s(auto-receiver-regressions.log),184HTML0fail0.70s+12srcset0fail0.01s(auto-collector-html-tests.log),finalcheck1.15s(auto-receiver-build.log). ActualChrome158 syntheticHTTPsink8cases:640/1024viewporttwodifferentimageplacements(sameURL160/260CSSpx),DOM>512,nslots>64,emptyhidden,beaconfalse,beaconthrow,crossoriginrefusal. PositivecasesoneimageGETandonePOST;allsource/sizesattributesunchanged. Script/JSONhashes/browser/rawpayloads inauto-collector-browser.json;imagesfixtureonly,noproductiontraffic. ChromeharnessstopsHTTPserver/processes;prefixartifactsretainedforreproduction. FullRusttests precededlastJStrycatchrefinement; final8Chromechecksandlockedcheckverifyfinalasset. Noactualproducer->NGINXHTTPendtoendbeaconclaimed: receiver seededRustunit andcollectorsyntheticHTTPsink separatelytested; realwire/NGINXpipeline/reloadoracleremainsA3c.

Filesassets/automatic_sizes_collector.js,automatic_sizes.rs assetexport,ps-nginx/beacon.rs+module.rs receiverwiring,tools/verify_automatic_sizes_collector.py,plan/ledger/testartifacts. NoREADMEfunctionalLCPclaim,noreleasebuild/deploy,norequestlatencybenchmarksinceproducerstilloff. A3cmustresolvecompleteoriginaldoc+stylesheetepochandcohortlayoutisolation,secureentropy/sampling,HTTPcache nonceexclusion/currentRUMandearlyHITexpiry/generationchecks beforeactivation. CollectorHTTPoverhead/JSbytesCPU andcold/warmMISS/HITwillbemeasuredA6.
