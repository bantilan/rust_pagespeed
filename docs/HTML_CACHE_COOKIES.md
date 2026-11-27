# HTML-cache cookie policy

The existing `HtmlCacheIgnoreCommonCookies` option defaults to **on**. With HTML caching enabled, no extra NGINX directive is required. This policy is CMS-independent: it applies equally to WordPress, Drupal, Joomla, Magento/Adobe Commerce, TYPO3, Craft CMS, October CMS, PrestaShop, OpenCart, Shopware, Moodle, MediaWiki, phpBB, Laravel, Symfony, CodeIgniter, CakePHP, Yii, Grav, Concrete CMS, MODX, ProcessWire and Statamic, and other websites.

Default cookies are considered non-personalizing under normal intended use. Site-specific exceptions belong in `HtmlCacheBypassCookie`. Unknown cookies continue to bypass. Recognized login/authentication, session, shopping-cart and CSRF cookies bypass even when a custom allowance such as `*` matches them. Exact predefined analytics session/bookkeeping names (for example `ai_session` and `mage-cache-sessid`) retain their intended non-login meaning. No name detector can determine an application's arbitrary personalization semantics; exclude names your application uses to vary public HTML.

Ignored tracking-cookie values are excluded from cache identity. In default six-group public mode, language/retina preference values are also ignored under the equivalent-public-HTML contract. With HtmlCacheSimpleVariants off, those two cookies select separate opaque preference variants. Cookies are never stripped from upstream requests. Authorization, response privacy/Set-Cookie, access control, freshness, Vary, purge and per-request beacon eligibility remain unchanged. Allowed cookies do not guarantee a HIT if another eligibility check rejects the request.

## Public language/retina cookies

No extra configuration is needed for these exact names when common-cookie support and HTML caching are on:

| Cookie | Core handling |
| --- | --- |
| `tk_qs` | Ignored tracking value; shares the cookie-free entry if no preferences are present. |
| `wordpress_test_cookie` | Already ignored by the default list. |
| `wp_lang` | Default: ignored within the six format/device groups; detailed mode: separate language variant. |
| `trx_addons_is_retina` | Default: ignored within the six format/device groups; detailed mode: separate retina variant. |

Default `HtmlCacheSimpleVariants on` ignores these values in both ordinary and early identities; the site must serve equivalent public HTML. In detailed mode (`off`), both ordinary and early HIT identities include the same opaque preference selectors. Same values share across cookie/header order and tracking values; different values, empty and absent cookies stay separate. Values are kept byte-distinct rather than guessing application normalization. Duplicate preference cookies bypass because upstream first/last-value handling can be ambiguous. Login/session/cart/CSRF, unknown cookies and other HTTP/privacy/beacon gates still take precedence.

`HtmlCacheBypassCookie "wp_lang";` (or the retina name) excludes a preference; `HtmlCacheIgnoreCommonCookies off;` disables its default acceptance. Explicitly allowing either name with `HtmlCacheIgnoreCookie` still preserves preference variants rather than ignoring its value. This targeted preference behavior is separate from the ordinary tracking-cookie allowlist.

## Default ignored entries

`*` is a wildcard; `[0-9]+` means one or more ASCII digits at the end, with a literal prefix. `wp-settings-12` matches; `wp-settings-abc` does not. Mixpanel/PostHog suffixes are anchored. Google Ads `_gcl_` and Hotjar rules are narrowed to the names/prefixes below rather than arbitrary vendor prefixes.

```text
wordpress_test_cookie
tk_qs
wp-settings-[0-9]+
wp-settings-time-[0-9]+
mage-cache-sessid
_ga
_ga_*
_gid
_gat
_gat_*
_gac_*
__utma
__utmb
__utmc
__utmt
__utmz
__utmv
_gcl_au
_gcl_aw
_gcl_dc
_gcl_gb
_gcl_gs
_gaexp
_opt_awcid
_opt_awmid
_opt_awgid
_opt_awkid
_opt_utmc
_fbp
_fbc
_clck
_clsk
_uetsid
_uetvid
_uetmsclkid
_MicrosoftApplicationInsightsSession
ai_user
ai_session
_hjSession_*
_hjSessionUser_*
_hjFirstSeen
_hjAbsoluteSessionInProgress
_hjIncludedInSessionSample_*
_hjIncludedInPageviewSample
_hjDonePolls
_hjMinimizedPolls
_hjShownFeedbackMessage
_hjRecordingEnabled
_hjRecordingLastActivity
_pk_id.*
_pk_ses.*
_pk_ref.*
_pk_cvar.*
_pk_testcookie.*
_pk_hsr.*
__hstc
__hssc
__hssrc
hubspotutk
_ttp
_tt_enable_cookie
_tt_sessionId
_tt_pixel_session_index
s_cc
s_sq
s_vi
s_fid
AMCV_*
AMCVS_*
ajs_anonymous_id
mp_*_mixpanel
amplitude_id
amplitude_id_*
AMP_*
AMP_MKTG_*
ph_*_posthog
plausible_ignore
__qca
_scid
_scid_r
_pin_unauth
_pinterest_ct_ua
_rdt_uuid
_twclid
AnalyticsSyncHistory
li_fat_id
li_sugr
_ym_uid
_ym_d
_ym_isad
_ym_visorc
_ce.s
_ce.clock_data
_ce.clock_event
_ce.irv
fs_uid
fs_lua
mf_user
mf_*
_lo_uid
_lo_v
__lotl
is_unique
sc_is_visitor_unique
__atuvc
__atuvs
NREUM
NRBA_SESSION
RT
AWSALB
AWSALBCORS
AWSALBTG
AWSALBTGCORS
ARRAffinity
ARRAffinitySameSite
```

Existing non-conflicting legacy analytics exact names remain supported (`__utmx`, `__utmxx`, `__gads`, `__gac`, `__gpi`, `_fplc`, `_FPAU`, `_FPGS`, `_FPGCLAW`, `_FPGCLDC`, `_FPGCLGB`, `_hjCachedUserAttributes`, `_hjViewportId`, `_hjTLDTest`, `hjClosedSurveyInvites`, `hjDonePolls`, `hjMinimizedPolls`, `hjDoneTestUsers`, `hjIncludedInSample`, `hjShownFeedbackMessage`). The admin cookie-policy panel displays the complete effective default list.

## Optional entries — off unless explicitly allowed

Enable individual entries using existing configuration, only when they do not personalize your application's public HTML. These lines are examples, not a recommendation to enable all of them:

```nginx
pagespeed HtmlCacheIgnoreCookie "has_js";
pagespeed HtmlCacheIgnoreCookie "section_data_ids";
pagespeed HtmlCacheIgnoreCookie "mage-cache-storage";
pagespeed HtmlCacheIgnoreCookie "mage-cache-storage-section-invalidation";
pagespeed HtmlCacheIgnoreCookie "__cf_bm";
pagespeed HtmlCacheIgnoreCookie "_cfuvid";
pagespeed HtmlCacheIgnoreCookie "cf_clearance";
pagespeed HtmlCacheIgnoreCookie "cookie_notice_accepted";
pagespeed HtmlCacheIgnoreCookie "cookieyes-consent";
pagespeed HtmlCacheIgnoreCookie "CookieConsent";
pagespeed HtmlCacheIgnoreCookie "OptanonConsent";
pagespeed HtmlCacheIgnoreCookie "OptanonAlertBoxClosed";
pagespeed HtmlCacheIgnoreCookie "cmplz_*";
pagespeed HtmlCacheIgnoreCookie "moove_gdpr_popup";
pagespeed HtmlCacheIgnoreCookie "euCookie";
```

## Additions, exclusions and disabling defaults

```nginx
pagespeed HtmlCache on;
# Default allowances are automatic; these are site-specific examples:
pagespeed HtmlCacheIgnoreCookie "site_metric";
pagespeed HtmlCacheIgnoreCookie "site_*_metric";
pagespeed HtmlCacheIgnoreCookie "counter-[0-9]+";
pagespeed HtmlCacheBypassCookie "_ga";
pagespeed HtmlCacheBypassCookie "wp-settings-[0-9]+";
# To disable the default list:
# pagespeed HtmlCacheIgnoreCommonCookies off;
```

Exclusions win over defaults/custom additions. Protected personalized names win over custom allowances, including a wildcard; avoid broad custom allowances because unknown application cookies may be personalizing. Configuration merges retain additions/exclusions, and cookie policy generation/pattern changes participate in effective options signatures. Installing this policy changes signatures, warming a new cache namespace.

Matching uses static default tables and configuration-compiled custom exact, wildcard and numeric-suffix tables. The request scanner borrows names, bounds cookie count, and does not allocate on successful eligibility. Bypass reasons retain the first offending name only, never its value. No regex compilation, filesystem access, network access or synchronous logging occurs in the cookie matcher.
