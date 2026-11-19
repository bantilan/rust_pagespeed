# Vendored static JS assets

Byte-exact copies of the Closure-compiled support scripts served by
`StaticAssetManager::GetAsset` (`*_opt.js` unless `kDebug` selects
`*_dbg.js`):

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

Source: `mod_pagespeed/net/instaweb/genfiles/rewriter/` (checked-in build
outputs of the Closure Compiler over `net/instaweb/rewriter/*.js`).
Regenerate by rebuilding those genfiles and re-copying; `static_assets`
tests pin distinguishing markers so a stale copy fails loudly.
