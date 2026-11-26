# Homepage BYPASS after855c1a2 deployment — 2026-11-27

Confirmed live server header nginx/1.31.10+855c1a2. Read-only curl replay using direct resolve157.245.148.68, original homepage, Accept text/html,image/avif,image/webp and compression: threeFirefox147 Ubuntu desktop requests allBYPASS with no CriticalImages.Run collector; threeChrome158 desktop requests allHIT withoutcollector. InitialChromeprobeMISS. This contradicts treating all reported bypasses as productive sampling. Live config unchanged. No purge/reload/install performed.

Five real Chrome sessions were also attempted with tools/chrome_cdp.py; their DOM snapshots did not expose diagnostic comments and getBeaconData was empty. Do not count these as HIT/BYPASS evidence. Read-only SSH logs show font204s; this alone does not establish criticalreportacceptance. Disk evidence scan found differing valid/pending counts in fourrecords but cannotattribute eachrecordtopage fromvaluealone; no claim that all belongtohomepage. Raw server logs/private headers not saved.

## Diagnostic change, not root-cause fix

ShowCacheTimestamp now adds a secondary PageSpeed Cache Detail comment when an armed rewrite skips storage. Static reasons: empty-body, request-policy, beacon-evidence-changed, resource-optimization-pending, beacon-sampling, expired-or-unencodable. Existing status comment stays intact; no keys/cookies/nonces exposed. Gate behavior unchanged. Request rejects before arming retain existing diagnostics and may not show this secondary comment. Regression proves a detached rewrite staysuncached and reports resource-optimization-pending.

Commands: cargo test --locked -p ps-html --lib --test html_cache_cookies --test html_cache_vary; cargo test --locked -p ps-nginx --lib; NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx. Latest results in adjacent logs. HTML211+cookie4+Vary8 pass. NGINX294 and matched release build passed.

Next: install matching diagnostic build, replay failing Firefox request, inspect PageSpeed Cache Detail. If no secondary comment, inspect lookup eligibility rather than storepath. This milestone does not claim to fix repeatedBYPASS, no production latency improvement, no disabled resourceoptimization or sampling. Keep prior all-source changes and public grouping. Need measured reason before next repair.
