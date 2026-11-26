# HAR main-document and image audit — 2026-11-26

User supplied two captures of the same navigation (11entries; main document975ms). Inspection confirms deployed server header5381c22 and both beacons204, including previously failing emptyfontPOST. Main documentBYPASS carries a newCriticalImages initialization; cache sampling can legitimately bypass, and no Cookie is present. This is not the analytics cookie issue. One navigation cannot prove a repeated bypass loop.

Firefox main timings: blocked355ms,DNS123ms,connect124ms,SSL127ms,wait246ms,receive0. These are browser-reported timing fields; do not interpret975ms as serverCPU or add overlappingHAR phases as an independent decomposition. Warm connection reuse and network/DNS/connection behavior matter. No network setting changed.

Image1 HAR2065ms:1020wait+1045receive,168868bytes. Image2HAR3800ms:1552wait+2248receive,181199bytes. SmallJS3033decodedbytes had1551mswait too. Headers show image/avif despite.jpg URL suffix; AVIF conversion is working. Source uses existing1536w image candidate. Renderedwidth/DPR not recorded, so no speculative narrowing of sizes or relabeling image descriptors.

Read-only SSH loopbackHTTPS probes, FirefoxUA+AVIFAccept,identityencoding,persistentconnection,12samples perresource: warm median(first2excluded) image1~0.715ms,image2~0.714ms,JS~0.417ms; initial observations7.65/5.46/5.06ms includingfreshTLS. All200, exactsame168868/181199imagebytes asHAR. Existing caches preserved, noforcedcoldencode or CPU profiling claimed. This establishes warmed resource delivery is fast at measurementtime, not that cold/concurrent AVIF encoding can never block. HAR multi-second waits cannot be attributed to image encoding from this evidence alone.

## Verified configuration improvement

Read-only nginx -T shows no enabled prioritize_lcp_images. Per-request PageSpeedFilters -/+prioritize_lcp_images (3requests/mode, no globalconfigedit) confirms:

| Output | OFF | ON |
| --- | --- | --- |
| Image preload | None | One matching hero srcset+sizes |
| Hero loading | Eager/high | Eager/high |
| Secondary image | No loading hint | loading=lazy |
| Existing font preload | Present | Retained unchanged |
| src/srcset/sizes | Existing | Unchanged |

The proposed existing-filter configuration is in recommended.conf. No new code/filter/quality reduction; no liveconfig/deployment/reload/purge. Request-level tests changed effectiveoptions only for those probe requests. ResponsesBYPASS; timings cannot be compared as filterCPU because of origin and sampling. HTTP200, exact src/srcset/sizes preservation, preloadalignment and fontretention asserted against savedJSON. Native lazy hint may still load an image near theviewport; no promise it eliminates that request.

Apply `pagespeed EnableFilters prioritize_lcp_images;` in the existing server block, test nginx configuration and gracefullyreload through the user's normal deployment process. This makes the hero discoverable in head and lets browserdefersecondaryimage; it does not reduceimagebytes or eliminateDNS/TLS/networktransfer. Browser LCP gain has NOT been benchmarked. Further sizing work requires actual renderedCSSwidth+viewport+DPR evidence; do not hardcode645px or force768w based on this HAR alone.

Artifacts har-summary.json/local.json/lcp-control.json retain the measured rows without privatecookies or fullbeaconnonces. Local probes ran via Pythonhttp.client over SSH and origin127.0.0.1:443 with rustpagespeed.comSNI; no remote scriptfiles persisted. No appsource modifications/full tests needed for this audit; output assertions andgitdiffcheck pass. All requests complete; no processes left running.
