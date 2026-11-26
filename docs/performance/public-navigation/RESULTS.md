# Public navigation cache sharing — 2026-11-26

At user request, public HTML keys omit Referer, Sec-Fetch-Site and request Priority unless origin Vary names those fields. All other header variants, approved-cookie policy, private/authorization/no-cache gates and beacon evidence remain unchanged. Request identity and early-policy namespaces versioned to public-html-v2/early-html-v2 so old entries are not reused. No live configuration/deployment changes.

Explicit Vary fields remain in final HTML identity using the cached origin policy, even though the candidate early-policy key normalizes navigation provenance. Origin policy is still trusted only for its existing configured TTL/invalidation contract. Applications that change public HTML based on these headers must send appropriate Vary or disable public caching; unannounced personalization cannot safely be inferred. Accept-Language/Accept/User-Agent/other request variants not broadly collapsed.

Verification:205HTMLlibrary+4cookieintegration+294NGINXtests pass; matched1.31.10release build24.87s. New unitregression warms a direct request then verifies ordinary/early internal-navigation HIT; Vary:Referer/Sec-Fetch-Site/Priority each isolate entries; Accept-Language remains isolated. Existing cookie/private/sampling/expiration/filemtime tests run in librarysuite. Extended isolated25msFastCGIoracle proves returningfromanotherpage with changednavigationheaders servesHIT with0additionalorigincalls, exactpublishedbodypreserved; analytics20HITs stillskiporigin,RUMclockretained,privatecookie/auth/no-cache/range/SetCookie controlsstillreachorigin. No browserruntimeJSexecution test claimed. No all-header normalization, no disabledfilters.

Commands:

```sh
cargo test --locked -p ps-html --lib --test html_cache_cookies
cargo test --locked -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
python3 tools/verify_early_html_cache.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module "$PWD/target/release/libps_nginx.so" --rum-only --analytics-early fixed --output docs/performance/public-navigation/oracle.json
```

Logs/JSON retained. No productionlatency claim, live install/retest required; sampling/expiry/learning can still intentionally MISS/BYPASS. Harness processes stopped. First visits under new key format need warming; old disk entries can expire normally, no purge needed for key migration. README contract updated.
