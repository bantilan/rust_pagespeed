# Live browser bypass check — 2026-11-26

Read-only inspection of supplied WebInspector HAR and live rustpagespeed.com running 8e4df99. No server configuration/source/deployment changes.

HAR contains one homepage navigation at 11:48:19 UTC (555.3 ms), no HTML response text and therefore no cache-status comment. No request Cookie or Authorization. Request Cache-Control: max-age=0 does not veto current eligibility. Font beacon returns 204. This capture cannot establish a repeated BYPASS sequence or its specific internal cause.

Live replay results: Chrome158 desktop/simple Accept identity encoding MISS then four HITs. Supplied navigation headers with identity encoding MISS then two HITs. Supplied headers including original compression negotiation via curl --compressed: five consecutive HITs sharing creation timestamp 11:49:22 UTC. No critical collector in these returned bodies. Thus persistent homepage bypass was not reproduced.

Read-only SSH access-log check found multiple critical-image POSTs (query only url; evidence in body) returning 499 around user's navigation/reload activity, while font POSTs return 204. One homepage critical POST returns 204 at 11:48:23 UTC. 499 establishes client cancellation, not whether the cause is navigation timing or server body handling. This is a lead for unsettled critical sampling; not proven causation of the reported BYPASS. Font beacon alone is not the critical-image evidence acknowledgement.

Reproduction commands: Python urllib.request homepage GET with HAR navigation headers (no private cookies), identity encoding, three repetitions; curl -sS --compressed --max-time 20 with supplied HAR headers, five repetitions; ssh root@157.245.148.68 nginx -T and filtered site access logs. Raw private logs/headers are not persisted. PageSpeed=off query showed origin response lacks explicit Cache-Control; rewritten max-age=0,no-cache alone is not sufficient evidence of origin prohibition.

Next: capture consecutive browser navigations including HTML response bodies and critical beacon POST completion, or instrument explicit non-cookie store-skip reasons before making a source fix. Do not disable beacon sampling or private-response gates to force HIT.
