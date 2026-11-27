# Extended default HTML-cache cookie policy — 2026-11-27

Baseline source: 3a7bfbb. No production deployment, NGINX configuration changes, purge or vendor/CMS research. User-supplied defaults/optional list implemented in the existing cookie-policy infrastructure.

## Behavior

- Common cookie allowance remains on by default, for every website/CMS. Added every supplied default pattern; optional entries remain off unless individually enabled with existing HtmlCacheIgnoreCookie. Existing HtmlCacheBypassCookie excludes site-specific personalizing names.
- Built-in exact names compile into a Rust match; static narrow prefix/infix/numeric suffix tables avoid request-time setup. Custom exact/wildcard/numeric suffix rules compile at configuration changes, merge through existing configuration, and participate in cached options signatures. Policy generation `public-cookies-v2` also separates new effective cache keys from old policy.
- `wp-settings-[0-9]+`/`wp-settings-time-[0-9]+` accept ASCII digits only. Mixpanel/PostHog suffixes are anchored. Google Ads `_gcl_*` is narrowed to the five supplied exact names; broad old Hotjar prefixes are narrowed to supplied names/prefixes. Non-conflicting existing legacy exact analytics names remain.
- Known protected login/auth/session/cart/CSRF families win against any custom ignore, including `*`. Exact predefined analytics/bookkeeping session names retain their intended non-login meaning. Unknown names bypass unless explicitly allowed by the administrator. Site-specific personalization semantics require exclusions; no cookie-name policy can discover arbitrary application behavior.
- Request scanning borrows names without a name Vec, bounds the cookie count, validates full headers, and allocates nothing on accepted requests. Bypass reasons can allocate an owned name (never values). No new logging, I/O, regex compilation or header mutation. Original upstream cookies, existing HTTP eligibility/access controls, private response checks and RUM remain.

Full lists and directives: [HTML_CACHE_COOKIES.md](../../HTML_CACHE_COOKIES.md).

## Verification

Before changes:

```bash
cargo test --locked -j 2 -p ps-config cookies
cargo test --locked -j 2 -p ps-html --test html_cache_cookies
```

Baseline: 8 cookie-policy unit tests and 4 HTML cookie integrations passed.

Final:

```bash
cargo test --locked -j 2 -p ps-config
cargo test --locked -j 2 -p ps-html --lib --test html_cache_cookies --test html_cache_vary
cargo test --locked -j 2 -p ps-nginx -p ps-admin --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
python3 docs/performance/cookie-policy-20261127/benchmark.py
```

- Config: **93 passed**. New fixtures exercise every supplied default and every optional entry (bypass → explicit opt-in → exclusion). Tests cover mixed cookies, protected names even with wildcard allowances, near misses, case sensitivity, ASCII numeric suffixes, infix matching, merge/signature behavior and invalid-pattern rejection.
- HTML: **212 unit + 6 cookie + 8 Vary tests passed**. New integration cases prove value-free ordinary/early sharing, unchanged upstream Cookie header, unknown/private mixtures bypassing without insertion, and login/logout restoring the original public body. Configuration changes/exclusions and protected-over-custom rules are exercised through RewriteOptions, not a duplicate policy.
- NGINX: **294 passed**; admin: **5 passed**. Deployment release module built successfully against 1.31.10.
- Existing full-config property-count test expected 169 while baseline table already had 170 (confirmed directly at 3a7bfbb). Corrected stale count/comment; no property was added by this milestone. First failing log retained.
- Initial NGINX unit run with deployment 1.31.10 bindings failed its existing assertion requiring pinned 1.31.8; 293 other tests passed. Final unit run uses default pinned bindings and passes all 294; deployment build uses explicit 1.31.10. Original mismatch log retained. No runtime compatibility test was weakened.

## Matcher microbenchmark

Actual old/current cookies.rs modules compiled separately with `rustc --edition=2021 -O`; no copied matcher logic. The script retrieves baseline via git show, compiles both before measurements, then runs three alternating serial pairs. Each run measures five batches of 100,000 requests after 1,000 warmups; table is median of per-run medians. Global allocator counts cover the matcher only. Final-source raw trials, commands and summary.json are adjacent. The initial exploratory benchmark.log predates final session-prefix checks; use benchmark-final.log and the final per-trial CSVs.

| Fixture | Old matcher | New matcher | Allocations old → new | Eligibility old → new |
| --- | ---: | ---: | ---: | --- |
| Existing analytics, 4 cookies | 0.243 µs | 0.402 µs | 1 → 0 | allowed → allowed |
| Expanded public, 6 cookies | 0.552 µs | 1.003 µs | 3 → 0 | bypass → allowed |
| Analytics + PHPSESSID | 0.159 µs | 0.216 µs | 2 → 1 | bypass → bypass |

The expanded policy adds CPU checks; this is **not** a matcher speedup claim. Existing analytics overhead increased about 0.158 µs (0.000158 ms) in this fixture while removing its allocation. The expanded-public row changes functionality, so its timings are not an equal-work comparison. The script asserts accepted new cases allocate zero, intended eligibility changes occur, and protected requests remain denied. No production request latency, cold/warm HTML latency or profiling attribution was measured here.

No features/optimization quality or HTTP privacy/beacon checks were disabled to increase cache eligibility. Newly eligible public visitors can reuse shared HTML when other gates permit. On installation the versioned policy warms a new options/cache namespace. Latest live baseline and pending-resource BYPASS investigation remain separate in ../live-20261127/RESULTS.md.
