# Public browser HTML-cache grouping — 2026-11-26

Implemented default-on browser grouping when HTML caching is used. Existing HtmlCache enablement is unchanged. Optional `pagespeed HtmlCachePublicBrowserGrouping off;` restores exact UA/Accept keys. This Directory-scoped option participates in signatures and rejects query overrides.

Equivalent Firefox147/Chrome131 desktop rewrite capabilities share early and ordinary HTML entries even with different Accept strings and origin Vary: User-Agent, Accept. Regression tests also cover browser version changes, incompatible AVIF support, mobile, language differences, exact-mode separation and purge. Other privacy/content headers remain keyed; compatible browsers with different such headers may still select separate entries. Public applications must serve equivalent content within a capability group; disable grouping for exact UA/Accept-dependent content.

Commands run from repository root:

```bash
cargo test --locked -p ps-config --lib public_browser_grouping
cargo test --locked -p ps-html --lib --test html_cache_cookies
cargo test --locked -p ps-nginx --lib
cargo test --locked -p ps-html --lib public_browser_grouping
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -p ps-nginx
```

Results: configuration 1 passed; HTML library 207 and cookie integration 4 passed; NGINX library 294 passed; explicit default-on regression 1 passed; release build passed. Logs adjacent. The final test-only assertion was added after starting the release build; runtime source was unchanged.

No deployment, production benchmark or live cross-browser HIT claim. Install a module matching deployment NGINX before live verification. New option signatures require warming new entries. Existing RUM sampling/private-cookie rules remain active. Earlier image-free static-alias fixture limitation is tracked separately in ../image-free-beacon/RESULTS.md.
