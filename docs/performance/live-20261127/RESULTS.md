# Live latency comparison — 2026-11-27 (Asia/Manila)

Read-only live measurement. Server reports nginx/1.31.10+1eec8fb in every recorded current persistent sample (header is not binary attestation). Existing caches preserved: no purge, deployment, config edit or reload. HTTP/1.1 persistent TLS per page, desktop Chrome131-like UA, Accept:text/html, identity encoding,3warmups+20measured requests. Initial loopback/public probes overlapped each other and the randomized audit; retest loopback/public overlapped each other after audit completed. Light live observations, not isolated load or code-only causal benchmark.

## Confirmed steady HIT comparison

Previous is same-method live-20261126 on reported183c879, not the separate25ms image microbenchmark. Current uses final retest, all60/60measured per location HTTP200/HIT. Initial non-HIT run is retained and reported below.

| Page | Previous local median / p95 | Current local median / p95 | Previous public median / p95 | Current public median / p95 |
| --- | ---: | ---: | ---: | ---: |
| `/` | 0.90 / 0.99 ms | 1.42 / 1.79 ms | 71.93 / 73.63 ms | 73.19 / 73.75 ms |
| `/hello-world/` | 1.08 / 1.27 ms | 1.88 / 2.18 ms | 75.17 / 76.18 ms | 72.41 / 73.26 ms |
| `/sample-page/` | 1.00 / 1.05 ms | 1.87 / 2.30 ms | 71.64 / 72.54 ms | 71.50 / 72.58 ms |

Local confirmed HIT median is0.52–0.87ms slower than historical0.90–1.08ms. Public medians remain around71–73ms; network variation exceeds these local differences. Content sizes, live traffic, caches and intervening changes differ; these observations do not attribute overhead to a particular commit. Timings include complete HTML delivery and Python header parsing, exclude browser rendering and subresources.

## Initial pending-optimization period

| Page | Initial local median | Measured states | Initial public median | Measured states |
| --- | ---: | --- | ---: | --- |
| `/` | 116.70 ms | {'BYPASS': 20} | 195.65 ms | {'BYPASS': 20} |
| `/hello-world/` | 1.92 ms | {'BYPASS': 6, 'MISS': 1, 'HIT': 13} | 69.24 ms | {'BYPASS': 4, 'HIT': 16} |
| `/sample-page/` | 1.46 ms | {'HIT': 20} | 69.29 ms | {'HIT': 20} |

All measured homepage BYPASSs in the initial probes report `resource-optimization-pending`; they do not report sampling. Initial hello-world BYPASSs carry the same pending reason. Later audit and retest settle to HIT. This establishes pending background rewrites are blocking publication during the initial period; it does not identify the resource, prove an indefinite loop, or establish exact settling duration (probe windows differ). No beacon filtering or optimization was disabled to obtain HIT. Do not hide the initial bypass samples by reporting only retest.

## Matched randomized origin/MISS audit

Same historical latency-audit-20261126/probe.py:5warmups+20measured per page/phase, randomized normal/unique-header/PageSpeed=off. Unique header forces a new HTML identity but keeps warm resource caches; not a fully cold resource benchmark. Normal measured20HIT perpage, unique20MISS, off20absent, all200.

| Page | Previous HIT → current | Previous warm-resource HTML MISS → current | Previous PageSpeed off → current |
| --- | ---: | ---: | ---: |
| `/` | 1.55 → 1.88 ms | 69.20 → 86.86 ms | 63.39 → 80.11 ms |
| `/hello-world/` | 1.48 → 1.77 ms | 79.99 → 88.86 ms | 73.70 → 81.75 ms |
| `/sample-page/` | 1.36 → 1.83 ms | 57.60 → 72.44 ms | 54.01 → 68.57 ms |

Origin-disabled timings also rose in this audit, so uncached differences cannot be attributed solely to rewriting. Historical audit server header732d534; current1eec8fb. No full cold-cache benchmark, CPU profiler, filter initialization timing, image encoding timing or NGINX stage attribution in this measurement-only task.

## Commands

```bash
python3 docs/performance/live-20261127/keepalive.py > docs/performance/live-20261127/public-keepalive.json
ssh -o BatchMode=yes -o ConnectTimeout=8 root@157.245.148.68 'python3 - --local' < docs/performance/live-20261127/keepalive.py > docs/performance/live-20261127/server-local-keepalive.json
ssh -o BatchMode=yes -o ConnectTimeout=8 root@157.245.148.68 'python3 -' < docs/performance/latency-audit-20261126/probe.py > docs/performance/live-20261127/audit.json
# Repeat first two commands with output public-retest.json and server-local-retest.json.
```

All commands completed; raw samples/cache/server/detail/bytes and logs adjacent. Probe only modified local reports and warmed bounded live variants. Next investigate which pending resource repeatedly detaches if users still see BYPASS; maintain latest diagnostics and do not change sampling/privacy/quality to force HIT.
