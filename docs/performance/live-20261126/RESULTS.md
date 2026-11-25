# Live latency check — 2026-11-26 (Asia/Manila)

Target: https://rustpagespeed.com/. Reported server `nginx/1.31.10+183c879`; this header is not proof of the loaded module binary. No deployment, cache purge, configuration edit, or restart. Existing production caches; anonymous GET, desktop Chrome-like UA, identity encoding, HTTP/1.1. Each case has 3 warmups and 20 measured sequential requests. Public and loopback runs overlapped, so these are light live checks, not isolated capacity benchmarks.

| Page | Server-local keepalive median / p95 | Public keepalive median / p95 | Fresh local TLS median total |
| --- | ---: | ---: | ---: |
| `/` | 0.90 / 0.99 ms | 71.93 / 73.63 ms | 14.40 ms |
| `/hello-world/` | 1.08 / 1.27 ms | 75.17 / 76.18 ms | 14.18 ms |
| `/sample-page/` | 1.00 / 1.05 ms | 71.64 / 72.54 ms | 14.09 ms |

All measured persistent-connection samples were HTTP200 HTML-cache HITs (20 per page per location). Times above include full HTML body delivery; no images/CSS/JS subrequests or browser rendering. Python keepalive timings include response header parsing. Public numbers include the route from the development machine and cannot be interpreted as Rust execution time.

Fresh loopback curl requests spent median12.70–13.03ms reaching completed TLS setup; total median14.09–14.40ms. This demonstrates connection setup dominates this test, but does not attribute historical production residuals or internal filter/cache CPU time. No profiler or stage tracing was run here.

Fresh public HTTPS HIT total medians were420.14ms(home),407.02ms(hello-world),404.71ms(sample-page), each18HITs. There was also1BYPASS and1MISS per page after warmup: retain them separately in public.json, do not label them HITs or infer stable cold/MISS performance from n=1. No deliberately cold/resource-miss samples were requested because live caches were preserved. Persistent public HITs were substantially faster than repeated fresh connections. No controlled before/after regression claim; historical results use different builds/methods.

Reproduction from repository root:

```sh
python3 docs/performance/live-20261126/probe.py > docs/performance/live-20261126/public.json
ssh -o BatchMode=yes root@157.245.148.68 'python3 - --local' < docs/performance/live-20261126/probe.py > docs/performance/live-20261126/server-local.json
python3 docs/performance/live-20261126/keepalive.py > docs/performance/live-20261126/public-keepalive.json
ssh -o BatchMode=yes root@157.245.148.68 'python3 - --local' < docs/performance/live-20261126/keepalive.py > docs/performance/live-20261126/server-local-keepalive.json
```

Raw rows include cache status and warmup flags. Existing SSH authorization used only to execute read-only loopback probes; no remote files created. Scripts ran successfully, all persistent samples status200; no application build/tests needed for this measurement-only checkpoint.
