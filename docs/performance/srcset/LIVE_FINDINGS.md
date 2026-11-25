# Installed automatic sizing: report diagnosis

Read-only check; no configuration/cache/server edits. User installed automatic sizing and explicitly requests LCP coverage.

Report: https://pagespeed.web.dev/analysis/https-rustpagespeed-com/0rdq50fd0wns7ma76f1hwd?form_factor=mobile
Report mobile fetchTime2026-11-25T09:09:58.222Z. Saved selected audits in live-psi-20261125.json, desktop included for comparison. Fetched report HTML via curl because web reader returned internal error; decoded embedded Lighthouse JSON directly, no new Lighthouse run claimed.

- MobileFCP0.917s,LCP1.201s,SpeedIndex2.980s,TBT0ms,CLS0. Image estimatedwaste46234bytes (45KiB):header23592+squareLCP22642.
- Both displayed352CSSpx; report target616physicalpx. Selected768x538 and768x768 respectively. Existingmobilecandidategap300->768means correctsizes alone may stillselect768. No resizing/variantgeneration authorized in srcset-only scope.
- LCP is wp-image-10, explicitly highpriority, eager and initialdocumentdiscoverable (all reportLCPdiscoverychecks pass). Keep that behavior.
- DesktopFCP0.242s,LCP0.522s,CLS0. Display645CSSpx; selectedsquare1024 andheader800. Betterhints could select supplied768candidate whenDPRpermits; no claim exact645candidateexists.
- LiveanonymousmobileHTML check: bothimgwidth/height/srcset present, no loadinglazy on either. LCPfetchpriorityhigh. Author sizes remain(max-width:1024px)100vw,1024px and(max-width:800px)100vw,800px. This is correct A2skipbehavior, not evidence the new switch is ignored. Currentliveforms also containAVIFcandidates vs JPEGnamedreportcandidates, so do not equate currentformatbytes with reporttimebytes.
- ReadonlySSH nginx-T filtered output confirms SrcsetAutoSizeson and EnableFilterscompress_zstd; no explicitmanualSrcsetrule. Didnotinspect/outputcredentials or changeconfiguration.

## Required implementation extension

Include eager/LCP img in learned sizing A1b/A3/A4. Nativeauto alone cannot do this safely without lazyloading; never delay LCP or rewrite sizes afterimagealreadydownloads. Learn bounded viewportCSSwidth+per-elementrenderedwidth through existingnoncevalidatedbeacon, preserveexactpage/options/layout/candidateidentity and generationcachecoherence. Do not collapse duplicate sameURLplacements. Apply conservative supportedviewportevidence in HTML beforepreloadgeneration; originalauthorfallback outsideknownviewports. Keepfetchpriority, eagerloading, src/srcset, responsivecandidatequality, RUM and initialdiscovery. Firstvisitwithoutlearnings remainsoriginal.

Acceptance: firstvisitfallback; secondvisitwithtrustedmatchingviewport/elementevidence updatesbothimgsizes+generatedimagesizes; no originimagefetch/decodeonHTML; no doubleLCPdownload; unknownviewport/stale/ambiguousevidence unchanged; multiworkerreload/cacheHIT/MISS/RUMsafe; viewport/DPRbrowserevidence. Mobile616candidatewarning may remain with only300/768 suppliedfiles; be explicit. No new variantgeneration or lower-qualityselection to silenceaudit.
