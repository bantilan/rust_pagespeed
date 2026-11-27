# One critical/LCP learning decision — 2026-11-27

Implemented locally, not deployed or pushed. Baseline source c5e610c; this is a
behavioral change, not a measured production latency improvement.

- Default `pagespeed BeaconSingleDecision on;` stops critical/LCP nonce issuance
  and HTML sampling bypass after one accepted report in the current evidence
  cohort. Existing filters, resource processing and RUM timing remain enabled.
- Effective options participate in the configuration signature. The issuer
  persists its policy as an optional evidence-codec extension, so the receiver
  does not guess the directory configuration. Legacy evidence still decodes;
  legacy readers ignore the extension. HTML lookup applies effective options
  even to a legacy learned record without the extension.
- Existing transactional evidence updates serialize issuance and acceptance.
  First acceptance clears competing pending nonces; later reports cannot
  replace that decision. Invalid/expired reports do not settle learning.
- Settled candidate bookkeeping cannot reopen learning. Missing/evicted
  evidence and new effective configurations can learn again. Purging HTML alone need not purge evidence.
- `pagespeed BeaconSingleDecision off;` restores periodic sampling. The old
  cadence regression remains enabled with that explicit option.
- This trades periodic adaptation for fewer learning requests: layout-only LCP
  and content changes will not be relearned until evidence resets.
  A nonce-validated unknown-LCP acknowledgement still settles sampling using
  existing fallback behavior. It does not prove an image LCP was measured.
- Independent font reports retain their existing session-storage duplicate
  suppression, and RUM timing reports remain per load. This is not a promise
  of zero beacon HTTP traffic or permanent HITs regardless of expiry/privacy.

## Verification

Working directory: repository root. Initial finder baseline: 15 tests pass.
Config: 96 pass. HTML library: 220 pass. Critical-image integration: 6 pass;
cookie integration: 7 pass; Vary integration: 8 pass. NGINX library: 294 pass.
Matched NGINX 1.31.10 release build passes. Full HTML suite result recorded below.

Regressions exercise first valid acceptance, invalid-report retry, competing
outstanding reports, persisted policy/legacy codec, timestamps through i64::MAX,
settled candidate updates, browser HITs after the old sampling deadline, collector suppression,
receiver rejection and periodic opt-out. Existing multiprocess transactional
nonce/report test still passes.

Exact commands:

```sh
cargo test --locked -j 2 -p ps-html --lib beacon_finder
cargo test --locked -j 2 -p ps-config -p ps-html --lib
cargo test --locked -j 2 -p ps-html --test critical_images_beacon --test html_cache_cookies --test html_cache_vary
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
cargo test --locked -j 2 -p ps-html
git diff --check
```

Raw logs are alongside this file. Initial implementation test compile failed
because a test used a private Scope import; corrected to ps_config::scope::Scope.
Initial Vary regression expected periodic sampling under defaults; it now
explicitly selects periodic mode and passes. No production profiling, latency
benchmark, or live-server behavior claim is made for this change.

## Exact public-group refinement (user clarification)

The public critical/LCP cohort now uses only the same six format/device groups
as HTML identity. A shared helper serves the collector, HTML lookup, lazy-image
finder and LCP prioritization. The collector endpoint carries a bounded `bvg`
value because the POST's Accept header cannot identify the original HTML group.
The receiver accepts only six named groups and still requires a nonce issued in
that exact cohort. Invalid groups and cross-group nonces cannot alter a decision.
Legacy collector requests without bvg retain their old device-cohort route.
Default Chrome/Firefox equality, six distinct groups, tablet/mobile equality,
issuer URL routing and receiver cohort isolation are regression tested. Existing
legacy format-mismatch tests explicitly select detailed mode so their guard
remains exercised. This changes evidence namespaces and requires warming each
public group once after installation. Font confirmations are still independent.

The earlier full HTML suite passed 1,787 cases across 64 result groups before
this refinement; final refinement verification is recorded below when complete.

Final targeted refinement checks: config96, HTML221 library, critical6, cookie7,
Vary8, LCP26 and NGINX294 all pass. Matched release build: 35.01s. Actual
isolated two-worker accepted-before-reload/replay oracle passes (reload.json):
valid count/generation stay 1, replay cannot replace accepted evidence.

```sh
cargo test --locked -j 2 -p ps-config -p ps-html --lib --test critical_images_beacon --test html_cache_cookies --test html_cache_vary --test prioritize_lcp_images
cargo test --locked -j 2 -p ps-nginx --lib
NGINX_SOURCE_DIR=/home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10 cargo build --locked --release -j 2 -p ps-nginx
python3 tools/verify_beacon_reload.py --nginx /home/gamingph/Projects/Rust/rust_pagespeed_compiler/nginx-build/nginx-1.31.10/objs/nginx --module target/release/libps_nginx.so --output docs/performance/single-beacon-decision-20261127/reload.json --accept-before-reload
cargo test --locked -j 2 -p ps-html
```

Milestone checklist:

- [x] Persist plan and finder baseline before source edits.
- [x] Default option, signature separation and periodic opt-out.
- [x] First accepted decision, nonce replay/competition protection, persistence.
- [x] Exactly six public critical/LCP cohorts shared by all consumers.
- [x] Candidate bookkeeping cannot reopen settled learning.
- [x] Targeted regressions and real overlapping-worker reload/replay oracle.
- [x] Matched release module and user documentation.
- [x] Final full HTML suite.
- [x] Commit tested milestone locally, no push.

Final full HTML suite: 1788 passing cases across 64 result groups; exit 0.
`git diff --check` passes. Next action: install through the normal deployment
workflow and verify live group sharing. No installation has been performed.
