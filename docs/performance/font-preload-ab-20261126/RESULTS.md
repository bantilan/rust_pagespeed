# Live preload_critical_fonts OFF/ON — 2026-11-26

User authorized live OFF/ON benchmark. Existing filter default is ON. Temporarily inserted exactly one EnableFilters or DisableFilters directive in the HTTPS site, validated each config with nginx -t, gracefully reloaded, and restored exact original config in finally. For separate uncached rewrite runs, also temporarily set HtmlCache and HtmlCacheEarlyLookup off. No deploy, cache purge or module change. Original configuration SHA256: `08a7aa02c0a74c8c7764180d57af0fa2eca5bb50e2ef70c72e093f28755ebd01`. Both harnesses report restored=true; independent remote SHA check matches and post-restore homepage HTTP200. Original core/default filter state remains enabled.

Server-local loopback HTTPS with SNI/Host rustpagespeed.com, persistent HTTP/1.1 connections, identity encoding, desktop Chrome-like UA. OFF/ON/ON/OFF order; 5 warmup +30 measured requests per page/phase/round, 60 measured per mode per row. Full HTML delivery time includes Nginx and, for uncached rewrites, PHP/origin and all enabled rewrite filters. Does not isolate filter CPU time or measure browser rendering/font requests. No public-network timing added because it would obscure these small differences.

| Case | OFF median | ON median | ON − OFF | OFF p95 | ON p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| HTML HIT / | 0.966 ms | 0.979 ms | +0.013 ms | 1.368 ms | 1.136 ms |
| HTML HIT /hello-world/ | 1.078 ms | 1.235 ms | +0.157 ms | 1.386 ms | 1.406 ms |
| HTML HIT /sample-page/ | 1.070 ms | 1.042 ms | -0.028 ms | 1.481 ms | 1.253 ms |
| Uncached rewrite / | 68.188 ms | 65.761 ms | -2.426 ms | 92.503 ms | 74.721 ms |
| Uncached rewrite /hello-world/ | 76.414 ms | 75.618 ms | -0.797 ms | 84.091 ms | 100.486 ms |
| Uncached rewrite /sample-page/ | 60.714 ms | 58.668 ms | -2.046 ms | 73.507 ms | 65.701 ms |

All retained cached samples: HTTP200/HIT. All retained uncached samples: HTTP200/BYPASS, not true cold resource-cache misses. Each page emitted zero `as=font` preload links in both modes; this workload therefore measures the enabled discovery/lookup path without demonstrating successful font emission or browser FCP/LCP improvement. A font-bearing fixture/browser test is required to quantify those benefits. Do not interpret negative ON−OFF medians as a proven optimization: live-origin scheduling, process caches and warmup vary. Cache-HIT results remain near1ms; hello-world ON HIT varied1.326ms to1.067ms across rounds (see summary), illustrating run variation. No controlled before/after claim versus earlier live checks.

Important failed-method correction: raw.json phase `rewrite_no_html_cache` used request header PageSpeedHtmlCache:off, but every retained response was actually HIT. Those rows are retained as diagnostic evidence and EXCLUDED from rewrite results. Early cache lookup did not honor that attempted bypass in this test; root cause was not investigated or fixed here. Correct rewrite results come only from uncached.json with both cache paths disabled in config and assertions rejecting HITs. Initial exploratory PageSpeedFilters header probes likewise BYPASS, so config toggles were used for the fair HIT comparison.

Artifacts: raw.json/run.log for first pass; uncached.json/uncached-run.log for validated uncached pass; summary.json for selected groups and individual round medians. Scripts contain exact tested directives and restoration logic.

Reproduction (mutates live config temporarily; requires the same authorized maintenance scope):

```sh
ssh -o BatchMode=yes -o ConnectTimeout=8 root@157.245.148.68 'python3 -' < docs/performance/font-preload-ab-20261126/probe.py > docs/performance/font-preload-ab-20261126/raw.json 2> docs/performance/font-preload-ab-20261126/run.log
ssh -o BatchMode=yes -o ConnectTimeout=8 root@157.245.148.68 'python3 -' < docs/performance/font-preload-ab-20261126/probe_uncached.py > docs/performance/font-preload-ab-20261126/uncached.json 2> docs/performance/font-preload-ab-20261126/uncached-run.log
python3 docs/performance/font-preload-ab-20261126/summarize.py
```

Both remote scripts completed successfully. Every config test succeeded; existing listen/http2 deprecation warnings retained in logs. summarize.py validates HTTP200, HIT/BYPASS classification, original config hash agreement and restoration. No application code changed or build required. No profiler, concurrent-load/cold-start test or browser font benefit claimed.
