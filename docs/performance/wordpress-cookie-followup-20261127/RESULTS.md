# Core WordPress tracking/public preference cookies — 2026-11-27

Baseline: d7ad0f8. User explicitly approved all three cookies with separate preference variants, superseding the earlier no-cookie-values identity contract only for wp_lang/trx_addons_is_retina. No production changes or deployment. Read-only deployed source inspection located tk_qs in WooCommerce analytics JS and confirmed trx_addons_is_retina controls PHP image multiplier; no additional vendor/CMS research.

## Behavior and design

- `tk_qs`: default exact ignored tracking cookie; its value does not join identity.
- `wordpress_test_cookie`: already default ignored; unchanged.
- `wp_lang` and `trx_addons_is_retina`: exact known public preferences, automatically eligible when common-cookie policy is enabled. Their values select separate public variants using opaque HMAC digests; raw values never join keys or diagnostics.
- Both normal HTML identity and early origin-policy hint identity call the same preference-aware identity wrapper. Fixed selector order makes Cookie ordering/header splitting irrelevant. Missing, empty, quoted and byte-distinct values stay distinct; no assumptions about application normalization. Tracking value changes still share.
- Eligibility and selector extraction reuse one bounded, allocation-free borrowed Cookie pair parser. Duplicate preference names fail closed even when values match, to avoid upstream first/last ambiguity. Cookie names remain strict tokens; trailing value bytes are preserved instead of silently normalizing preference semantics.
- Exclusions win; common defaults can be disabled; explicit custom allowances for either preference preserve variants. Known auth/session/cart/CSRF, unknown cookies, response privacy, authorization/access controls, origin freshness/Vary/purge and RUM eligibility remain unchanged. Upstream Cookie headers remain intact.
- Policy generation v3 updates cached option signatures, warming a new namespace. Preferences are not added to the ignored-values list. Admin panel and README/reference distinguish the two cookie classes.

Configuration examples and full policy: ../../HTML_CACHE_COOKIES.md. No extra NGINX directives required with HTML caching/common-cookie policy enabled.

## Verification

```bash
cargo test --locked -j 2 -p ps-config cookies  # baseline11pass
cargo test --locked -j 2 -p ps-config
cargo test --locked -j 2 -p ps-html --lib --test html_cache_cookies --test html_cache_vary
cargo test --locked -j 2 -p ps-nginx -p ps-admin --lib
cargo test --locked -j 2 -p ps-admin --lib --test admin_site
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
cargo run --locked --release -j 2 -p ps-html --example cookie_preference_latency
python3 docs/performance/wordpress-cookie-followup-20261127/benchmark.py
```

Final source checks: config **94**, HTML unit **213**, cookie integration **7**, Vary **8**, NGINX **294**, admin unit **5** and admin integration **49** pass. Matched deployment release build passed. Initial test-only `.split("; ").rev()` compilation error retained in html-tests.log; fixed with rsplit, and all subsequent HTML runs pass. Admin integration's stale pre-d7 Hotjar prefix expectation updated to the already narrowed `_hjSession_*` while adding visible preference-selector assertions.

Regressions exercise grouped/exact browser modes; seven preference/body variants; cold ordinary/early MISS before publication; correct ordinary/early HIT afterward; tracking-value changes; Cookie order/multiple fields; unknown/auth/session mixtures; login/logout; duplicate preference rejection; exclusions/default-off; opaque preference identity (no raw values) and raw trailing-value distinctions. Existing HTML tests preserve learning/publication/private behavior.

## Local performance observations

Standalone old/current matcher modules compile directly from actual sources (`git show d7ad0f8` versus current cookies.rs). Three alternating serial pairs; each measures five batches of100,000 requests after1,000warmups. No copied policy implementation, regex or service traffic. Accepted paths assert zero allocations and correct eligibility.

| Cookie eligibility fixture | Baseline median of medians | Current | Allocations before → after | Eligibility |
| --- | ---: | ---: | ---: | --- |
| Existing 4 analytics cookies | 0.214 µs | 0.233 µs | 0 → 0 | allowed → allowed |
| Screenshot jar | 0.265 µs | 0.368 µs | 1 → 0 | bypass → allowed |
| Analytics + PHPSESSID | 0.124 µs | 0.132 µs | 1 → 1 | bypass → bypass |

Matcher rows exclude preference digest/key construction. The screenshot row changes functionality and is not equal-work before/after latency. The existing analytics row adds about0.020µs in this standalone fixture; no blanket matcher speedup claim.

Current release warm lookup example: reused driver, memory caches,32KiBbody,50warmups+2,000measuredHITs per fixture/path per run. Three serial repeat runs; below is median of per-run medians,6,000measuredHITs per cell (48,000total). Every response body is asserted. lookup.csv is the initial run; lookup-trial-* and lookup-summary.json contain repeats.

| Current fixture | Ordinary lookup | Early lookup |
| --- | ---: | ---: |
| Cookie-free | 7.925 µs | 12.924 µs |
| Ignored tracking | 8.656 µs | 14.277 µs |
| Two public preferences | 9.528 µs | 15.279 µs |
| Full screenshot jar | 9.488 µs | 15.810 µs |

Different fixtures add matching/scanning/digest work. This is not a matched historical HTML lookup comparison: the old source bypasses the screenshot jar. Example includes session creation, lookup/key work and32KiB cache copies, excludes driver initialization, filters, disk/shared cache, PHP/NGINX, TLS and resource processing. Early path does two lookups/validations; its benefit is skipping origin execution, not lower microbenchmark CPU than a normal lookup. No cold production request/live latency or13ms profiling attribution measured. First requests after install may MISS/BYPASS for ordinary warming, resource or beacon reasons; this change removes the cookie-specific rejection without weakening other gates.
