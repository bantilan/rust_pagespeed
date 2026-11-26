# Live browser bypass check — 2026-11-26

Read-only inspection of supplied WebInspector HAR and live rustpagespeed.com running 8e4df99. No server configuration/source/deployment changes.

HAR contains one homepage navigation at 11:48:19 UTC (555.3 ms), no HTML response text and therefore no cache-status comment. No request Cookie or Authorization. Request Cache-Control: max-age=0 does not veto current eligibility. Font beacon returns 204. This capture cannot establish a repeated BYPASS sequence or its specific internal cause.

Live replay results: Chrome158 desktop/simple Accept identity encoding MISS then four HITs. Supplied navigation headers with identity encoding MISS then two HITs. Supplied headers including original compression negotiation via curl --compressed: five consecutive HITs sharing creation timestamp 11:49:22 UTC. No critical collector in these returned bodies. Thus persistent homepage bypass was not reproduced.

Read-only SSH access-log check found multiple critical-image POSTs (query only url; evidence in body) returning 499 around user's navigation/reload activity, while font POSTs return 204. One homepage critical POST returns 204 at 11:48:23 UTC. 499 establishes client cancellation, not whether the cause is navigation timing or server body handling. This is a lead for unsettled critical sampling; not proven causation of the reported BYPASS. Font beacon alone is not the critical-image evidence acknowledgement.

Reproduction commands: Python urllib.request homepage GET with HAR navigation headers (no private cookies), identity encoding, three repetitions; curl -sS --compressed --max-time 20 with supplied HAR headers, five repetitions; ssh root@157.245.148.68 nginx -T and filtered site access logs. Raw private logs/headers are not persisted. PageSpeed=off query showed origin response lacks explicit Cache-Control; rewritten max-age=0,no-cache alone is not sufficient evidence of origin prohibition.

Next: capture consecutive browser navigations including HTML response bodies and critical beacon POST completion, or instrument explicit non-cookie store-skip reasons before making a source fix. Do not disable beacon sampling or private-response gates to force HIT.

## Firefox/Chrome comparison — deployment 5e74c19

New captures at 12:14:18 UTC (Chrome158) and 12:14:35 UTC (Firefox147) target the same /sample-page/. Both desktop Linux, HTTP/2, identical language en-US,en;q=0.9 and encoding gzip, deflate, br, zstd; no request cookies. Firefox body explicitly shows HIT created12:13:35 UTC; Chrome body omitted, so its status cannot be independently read.

Current request_identity removes UA/Accept/client-brand hints and canonicalizes casing/order, but serializes other request headers. Firefox alone sends Sec-GPC:1, Connection:keep-alive and TE:trailers. Chrome alone sends Cache-Control:max-age=0. These retained inputs prevent identical identities independently of UA grouping. Host vs :authority in HAR is transport representation; not claimed as a confirmed internal-key difference. No source fix or new runtime test in this read-only comparison.

Follow-up design: canonicalize transport headers and refresh-only max-age=0 where safe, preserving explicit origin Vary and force-revalidation/no-store behavior. Sec-GPC is intentionally retained as a privacy/content dimension; never suppress the signal to the origin or casually merge privacy-dependent HTML. Existing grouping test used requests matching these other headers, so it did not establish sharing for this actual browser pair.
