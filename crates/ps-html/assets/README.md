# Vendored static JS assets

Support scripts served by `StaticAssetManager::GetAsset`,
both byte-exact Closure-compiled genfiles: `*_opt.js` is the
minified build, `*_dbg.js` the whitespace pretty-print C++
serves for `kDebug` (the data2c rule feeds on the Closure
`closure_out` debug build, not the cleartext source):

- `deterministic_opt.js` / `deterministic_dbg.js`
- `defer_iframe_opt.js` / `defer_iframe_dbg.js`
- `add_instrumentation_opt.js` / `add_instrumentation_dbg.js`
- `extended_instrumentation_opt.js` / `extended_instrumentation_dbg.js`
- `client_domain_rewriter_opt.js` / `client_domain_rewriter_dbg.js`
- `dedup_inlined_images_opt.js` / `dedup_inlined_images_dbg.js`
- `lazyload_images_opt.js` / `lazyload_images_dbg.js`
- `delay_images_opt.js` / `delay_images_dbg.js`
- `delay_images_inline_opt.js` / `delay_images_inline_dbg.js`
- `local_storage_cache_opt.js` / `local_storage_cache_dbg.js`
- `critical_css_loader_opt.js` / `critical_css_loader_dbg.js`
- `critical_css_beacon_opt.js` / `critical_css_beacon_dbg.js`
- `critical_images_beacon_opt.js` / `critical_images_beacon_dbg.js`
- `js_defer_opt.js` / `js_defer_dbg.js`
- `responsive_opt.js` / `responsive_dbg.js`

Source: `mod_pagespeed/net/instaweb/genfiles/rewriter/` (checked-in build
outputs of the Closure Compiler over `net/instaweb/rewriter/*.js`).
Regenerate by rebuilding those genfiles and re-copying; `static_assets`
tests pin distinguishing markers so a stale copy fails loudly.
