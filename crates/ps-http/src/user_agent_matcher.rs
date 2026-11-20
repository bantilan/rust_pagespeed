//! User-agent capability matching (`UserAgentMatcher`).
//!
//! Wildcard allow/block lists plus version sniffing for image
//! inlining, webp/avif support, device type, and JS deferral.
//!
//! NOTE: the known-screen-dimensions table is write-only in C++
//! (built, never read); it has no equivalent here.

use ps_config::wildcard::WildcardGroup;
use regex::Regex;

/// Webp test user agent (`kTestUserAgentWebP`).
pub const TEST_USER_AGENT_WEBP: &str = "test-user-agent-webp";
/// Non-webp test user agent (`kTestUserAgentNoWebP`; must not
/// contain the substring "webp").
pub const TEST_USER_AGENT_NO_WEBP: &str = "test-user-agent-no";

/// Device class (`UserAgentMatcher::DeviceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeviceType {
    /// Desktop.
    #[default]
    Desktop,
    /// Tablet.
    Tablet,
    /// Mobile.
    Mobile,
}

/// UA capability matcher (`UserAgentMatcher`).
#[derive(Debug)]
pub struct UserAgentMatcher {
    supports_image_inlining: WildcardGroup,
    supports_lazyload_images: WildcardGroup,
    defer_js_allowlist: WildcardGroup,
    defer_js_mobile_allowlist: WildcardGroup,
    legacy_webp: WildcardGroup,
    supports_webp_lossless_alpha: WildcardGroup,
    supports_webp_animated: WildcardGroup,
    supports_dns_prefetch: WildcardGroup,
    mobile_user_agents: WildcardGroup,
    tablet_user_agents: WildcardGroup,
    ie_user_agents: WildcardGroup,
    mobilization_user_agents: WildcardGroup,
    chrome_version_pattern: Regex,
}

const GOOGLE_PLUS_USER_AGENT: &str = "*Google (+https://developers.google.com/+/web/snippet/)*";

const IMAGE_INLINING_ALLOWLIST: &[&str] = &[
    "*Android*",
    "*Chrome/*",
    "*CriOS/*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*Firefox/*",
    "*iPad*",
    "*iPhone*",
    "*iPod*",
    "*itouch*",
    "*Opera*",
    "*Safari*",
    "*Wget*",
    // Allow in ads policy checks to match usual UA behavior.
    "AdsBot-Google*",
    // Plus IE, see use in the code.
    // The following user agents are used only for internal testing
    "google command line rewriter",
    "webp",
    "webp-la",
];
const IMAGE_INLINING_BLOCKEDLIST: &[&str] = &[
    "*Firefox/1.*",
    "*Firefox/2.*",
    "*MSIE 5.*",
    "*MSIE 6.*",
    "*MSIE 7.*",
    "*Opera?5*",
    "*Opera?6*",
    GOOGLE_PLUS_USER_AGENT,
];

// Exclude BlackBerry OS 5.0 and older. Exclude all Opera Mini.
const LAZYLOAD_IMAGES_BLOCKEDLIST: &[&str] =
    &["BlackBerry*CLDC*", "*Opera Mini*", GOOGLE_PLUS_USER_AGENT];

// For defer js we only allow Firefox4+, IE8+, safari and Chrome.
// Note: None of the following should match a mobile UA.
const DEFER_JS_ALLOWLIST: &[&str] = &[
    "*Chrome/*",
    "*Firefox/*",
    "*Safari*",
    "*CriOS/*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    // Plus IE, see code below.
    "*Wget*",
    "*Googlebot*",
    "*Mediapartners-Google*",
];
const DEFER_JS_BLOCKEDLIST: &[&str] = &[
    "*Firefox/1.*",
    "*Firefox/2.*",
    "*Firefox/3.*",
    "*MSIE 5.*",
    "*MSIE 6.*",
    "*MSIE 7.*",
    "*MSIE 8.*",
];
const DEFER_JS_MOBILE_ALLOWLIST: &[&str] = &["*AppleWebKit/*"];

// For legacy webp rewriting, we allowlist Android, but blockedlist
// older versions and Firefox.
const LEGACY_WEBP_ALLOWLIST: &[&str] = &["*Android *"];
const LEGACY_WEBP_BLOCKEDLIST: &[&str] = &[
    "*Android 0.*",
    "*Android 1.*",
    "*Android 2.*",
    "*Android 3.*",
    "*Firefox/*",
    "*Edge/*",
    "*Trident/*",
    "*Windows Phone*",
    "*Chrome/*", // Genuine Chrome always sends Accept: webp.
    "*CriOS/*",  // Paranoia: we should not see Android and CriOS together.
];

// To determine lossless webp support and animated webp support, we
// must examine the UA.
const WEBP_LOSSLESS_ALPHA_ALLOWLIST: &[&str] = &[
    "*Chrome/??.*",
    "*Chrome/???.*",
    "*CriOS/??.*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    // User agent used only for internal testing.
    "webp-la",
    "webp-animated",
    "*Firefox/*", // Do this way to permit Firefox webcapable to convert png
];
const WEBP_LOSSLESS_ALPHA_BLOCKEDLIST: &[&str] = &[
    "*Chrome/?.*",
    "*Chrome/1?.*",
    "*Chrome/20.*",
    "*Chrome/21.*",
    "*Chrome/22.*",
    "*CriOS/1?.*",
    "*CriOS/20.*",
    "*CriOS/21.*",
    "*CriOS/22.*",
    "*CriOS/23.*",
    "*CriOS/24.*",
    "*CriOS/25.*",
    "*CriOS/26.*",
    "*CriOS/27.*",
    "*CriOS/28.*",
    "*Firefox/?.*",
    "*Firefox/1?.*",
    "*Firefox/2?.*",
    "*Firefox/3?.*",
    "*Firefox/4?.*",
    "*Firefox/5?.*",
    "*Firefox/60.*",
    "*Firefox/61.*",
    "*Firefox/62.*",
    "*Firefox/63.*",
    "*Firefox/64.*", // Black list Firefox not webp capable
];

// Animated WebP is supported by browsers based on Chromium v32+,
// including Chrome 32+ and Opera 19+.
const WEBP_ANIMATED_ALLOWLIST: &[&str] = &[
    "*Chrome/??.*",
    "*CriOS/??.*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    "webp-animated", // User agent for internal testing.
    "*Firefox/*",
];
const WEBP_ANIMATED_BLOCKEDLIST: &[&str] = &[
    "*Chrome/?.*",
    "*Chrome/1?.*",
    "*Chrome/2?.*",
    "*Chrome/30.*",
    "*Chrome/31.*",
    "*CriOS/?.*",
    "*CriOS/1?.*",
    "*CriOS/2?.*",
    "*CriOS/30.*",
    "*CriOS/31.*",
    "*Firefox/?.*",
    "*Firefox/1?.*",
    "*Firefox/2?.*",
    "*Firefox/3?.*",
    "*Firefox/4?.*",
    "*Firefox/5?.*",
    "*Firefox/60.*",
    "*Firefox/61.*",
    "*Firefox/62.*",
    "*Firefox/63.*",
    "*Firefox/64.*",
];

const INSERT_DNS_PREFETCH_ALLOWLIST: &[&str] = &[
    "*Chrome/*",
    "*CriOS/*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    "*Firefox/*",
    "*Safari/*",
    // Plus IE, see code below.
    "*Wget*",
];
const INSERT_DNS_PREFETCH_BLOCKEDLIST: &[&str] = &[
    "*Firefox/1.*",
    "*Firefox/2.*",
    "*Firefox/3.*",
    "*Version/3.*Safari/*",
    "*Version/4.*Safari/*",
    "*Version/5.*Safari/*",
    "*MSIE 5.*",
    "*MSIE 6.*",
    "*MSIE 7.*",
    "*MSIE 8.*",
];

// Tablet allowlist; check mobile lists first for correct results.
const TABLET_USER_AGENT_ALLOWLIST: &[&str] = &[
    "*Android*",
    "*iPad*",
    "*TouchPad*",
    "*Silk-Accelerated*",
    "*Kindle Fire*",
    "*Android*Tablet*",
    "*SM-T*",
];
const MOBILE_USER_AGENT_ALLOWLIST: &[&str] = &[
    "*Mozilla*Android*Mobile*",
    "*Android*Mobile*",
    "*iPhone*",
    "*BlackBerry*",
    "*Opera Mobi*",
    "*Opera Mini*",
    "*SymbianOS*",
    "*UP.Browser*",
    "*J-PHONE*",
    "*Profile/MIDP*",
    "*profile/MIDP*",
    "*portalmmm*",
    "*DoCoMo*",
    "*Obigo*",
    "*FxiOS/*",
    "*EdgiOS/*",
    "*EdgA/*",
    "*SamsungBrowser*Mobile*",
    "AdsBot-Google-Mobile",
];
const MOBILE_USER_AGENT_BLOCKEDLIST: &[&str] = &[
    "*Mozilla*Android*Silk*Mobile*",
    "*Mozilla*Android*Kindle Fire*Mobile*",
    "*iPad*",
];

const MOBILIZATION_USER_AGENT_ALLOWLIST: &[&str] = &[
    "*Android*",
    "*Chrome/*",
    "*Firefox/*",
    "*iPad*",
    "*iPhone*",
    "*iPod*",
    "*Opera*",
    "*Safari*",
    "*Wget*",
    "*Edg/*",
    "*EdgA/*",
    "*EdgiOS/*",
    "*OPR/*",
    "*FxiOS/*",
    "*CriOS/*",
    "*Android *",
    "*iPhone*",
    "AdsBot-Google*",
];
const MOBILIZATION_USER_AGENT_BLOCKEDLIST: &[&str] = &[
    "*Android 0.*",
    "*Android 1.*",
    "*Android 2.*",
    "*BlackBerry*",
    "*Mozilla*Android*Silk*Mobile*",
    "*Mozilla*Android*Kindle Fire*Mobile*",
    "*Opera Mobi*",
    "*Opera Mini*",
    "*SymbianOS*",
    "*UP.Browser*",
    "*J-PHONE*",
    "*Profile/MIDP*",
    "*profile/MIDP*",
    "*portalmmm*",
    "*DoCoMo*",
    "*Obigo*",
    "*CriOS/*",
    "*GSA*Safari*",
    "*U; Android 3.*",
    "*U; Android 4.*",
];

// IE 11+ strings are deliberately difficult; spot them anyway.
const IE_USER_AGENTS: &[&str] = &[
    "*MSIE *",               // Should match any IE before 11.
    "*rv:11.?) like Gecko*", // Other revisions (eg 12.0) are FireFox
    "*IE 1*",                // Initial numeral avoids Samsung UA
    "*Trident/7*",           // Opera sometimes pretends to be earlier Trident
];

/// Chromium-family 4-number version pattern.
const CHROME_VERSION_PATTERN: &str =
    r"(?:Chrome|CriOS|Edg|EdgA|EdgiOS|OPR)/([0-9]+)\.([0-9]+)\.([0-9]+)\.([0-9]+)";

impl UserAgentMatcher {
    /// Builds the matcher with all pattern tables.
    #[must_use]
    pub fn new() -> Self {
        let mut matcher = UserAgentMatcher {
            supports_image_inlining: WildcardGroup::new(),
            supports_lazyload_images: WildcardGroup::new(),
            defer_js_allowlist: WildcardGroup::new(),
            defer_js_mobile_allowlist: WildcardGroup::new(),
            legacy_webp: WildcardGroup::new(),
            supports_webp_lossless_alpha: WildcardGroup::new(),
            supports_webp_animated: WildcardGroup::new(),
            supports_dns_prefetch: WildcardGroup::new(),
            mobile_user_agents: WildcardGroup::new(),
            tablet_user_agents: WildcardGroup::new(),
            ie_user_agents: WildcardGroup::new(),
            mobilization_user_agents: WildcardGroup::new(),
            chrome_version_pattern: Regex::new(CHROME_VERSION_PATTERN)
                .expect("chrome version pattern"),
        };
        for pattern in IMAGE_INLINING_ALLOWLIST {
            matcher.supports_image_inlining.allow(pattern);
        }
        for pattern in IE_USER_AGENTS {
            matcher.supports_image_inlining.allow(pattern);
        }
        for pattern in IMAGE_INLINING_BLOCKEDLIST {
            matcher.supports_image_inlining.disallow(pattern);
        }
        for pattern in LAZYLOAD_IMAGES_BLOCKEDLIST {
            matcher.supports_lazyload_images.disallow(pattern);
        }
        matcher.defer_js_allowlist.allow(IE_USER_AGENTS[0]);
        for pattern in DEFER_JS_ALLOWLIST {
            matcher.defer_js_allowlist.allow(pattern);
        }
        // https://github.com/apache/incubator-pagespeed-mod/issues/982
        matcher.defer_js_allowlist.disallow("* MSIE 9.*");
        for pattern in DEFER_JS_BLOCKEDLIST {
            matcher.defer_js_allowlist.disallow(pattern);
        }
        for pattern in DEFER_JS_MOBILE_ALLOWLIST {
            matcher.defer_js_mobile_allowlist.allow(pattern);
        }
        for pattern in LEGACY_WEBP_ALLOWLIST {
            matcher.legacy_webp.allow(pattern);
        }
        for pattern in LEGACY_WEBP_BLOCKEDLIST {
            matcher.legacy_webp.disallow(pattern);
        }
        for pattern in WEBP_LOSSLESS_ALPHA_ALLOWLIST {
            matcher.supports_webp_lossless_alpha.allow(pattern);
        }
        for pattern in WEBP_LOSSLESS_ALPHA_BLOCKEDLIST {
            matcher.supports_webp_lossless_alpha.disallow(pattern);
        }
        for pattern in WEBP_ANIMATED_ALLOWLIST {
            matcher.supports_webp_animated.allow(pattern);
        }
        for pattern in WEBP_ANIMATED_BLOCKEDLIST {
            matcher.supports_webp_animated.disallow(pattern);
        }
        for pattern in INSERT_DNS_PREFETCH_ALLOWLIST {
            matcher.supports_dns_prefetch.allow(pattern);
        }
        for pattern in IE_USER_AGENTS {
            matcher.supports_dns_prefetch.allow(pattern);
        }
        for pattern in INSERT_DNS_PREFETCH_BLOCKEDLIST {
            matcher.supports_dns_prefetch.disallow(pattern);
        }
        for pattern in MOBILE_USER_AGENT_ALLOWLIST {
            matcher.mobile_user_agents.allow(pattern);
        }
        for pattern in MOBILE_USER_AGENT_BLOCKEDLIST {
            matcher.mobile_user_agents.disallow(pattern);
        }
        for pattern in TABLET_USER_AGENT_ALLOWLIST {
            matcher.tablet_user_agents.allow(pattern);
        }
        for pattern in MOBILIZATION_USER_AGENT_ALLOWLIST {
            matcher.mobilization_user_agents.allow(pattern);
        }
        for pattern in MOBILIZATION_USER_AGENT_BLOCKEDLIST {
            matcher.mobilization_user_agents.disallow(pattern);
        }
        for pattern in IE_USER_AGENTS {
            matcher.ie_user_agents.allow(pattern);
        }
        matcher
    }

    /// Whether the agent is IE (`IsIe`).
    #[must_use]
    pub fn is_ie(&self, user_agent: &str) -> bool {
        self.ie_user_agents.is_match(user_agent, false)
    }

    /// Whether the agent is IE9 (`IsIe9`).
    #[must_use]
    pub fn is_ie9(&self, user_agent: &str) -> bool {
        user_agent.contains(" MSIE 9.")
    }

    /// Whether the agent supports image inlining.
    #[must_use]
    pub fn supports_image_inlining(&self, user_agent: &str) -> bool {
        if user_agent.is_empty() {
            return true;
        }
        self.supports_image_inlining.is_match(user_agent, false)
    }

    /// Whether the agent supports lazyloaded images.
    #[must_use]
    pub fn supports_lazyload_images(&self, user_agent: &str) -> bool {
        self.supports_lazyload_images.is_match(user_agent, true)
    }

    /// Whether the agent supports DNS prefetch.
    #[must_use]
    pub fn supports_dns_prefetch(&self, user_agent: &str) -> bool {
        self.supports_dns_prefetch.is_match(user_agent, false)
    }

    /// Whether the agent supports deferred JS.
    #[must_use]
    pub fn supports_js_defer(&self, user_agent: &str, allow_mobile: bool) -> bool {
        if self.device_type_for_ua(user_agent) != DeviceType::Desktop {
            return allow_mobile && self.defer_js_mobile_allowlist.is_match(user_agent, false);
        }
        user_agent.is_empty() || self.defer_js_allowlist.is_match(user_agent, false)
    }

    /// Whether the agent is legacy-webp (Android 4.0+, sans
    /// Firefox).
    #[must_use]
    pub fn legacy_webp(&self, user_agent: &str) -> bool {
        self.legacy_webp.is_match(user_agent, false)
    }

    /// Whether the agent decodes webp without an Accept advertisement.
    #[must_use]
    pub fn supports_webp_rewritten_urls(&self, user_agent: &str) -> bool {
        if self.legacy_webp(user_agent) {
            return true;
        }
        if user_agent.contains("Firefox/") {
            return parse_version_after(user_agent, "Firefox/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 65, 0));
        }
        if user_agent.contains("Chrome/") || user_agent.contains("Chromium/") {
            let token = if user_agent.contains("Chrome/") {
                "Chrome/"
            } else {
                "Chromium/"
            };
            return parse_version_after(user_agent, token)
                .is_some_and(|(major, minor)| version_at_least(major, minor, 32, 0));
        }
        if user_agent.contains("OPR/") {
            return parse_version_after(user_agent, "OPR/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 19, 0));
        }
        if user_agent.contains("Version/")
            && user_agent.contains("Safari/")
            && is_ios_user_agent(user_agent)
        {
            return (parse_version_after(user_agent, "iPhone OS ")
                .or_else(|| parse_version_after(user_agent, "CPU OS ")))
            .is_some_and(|(major, minor)| version_at_least(major, minor, 14, 0));
        }
        if user_agent.contains("Version/")
            && user_agent.contains("Safari/")
            && !user_agent.contains("Chrome/")
            && !user_agent.contains("CriOS/")
            && !user_agent.contains("Edg")
        {
            let modern_macos = user_agent.contains("Mac OS X 11_")
                || user_agent.contains("Mac OS X 12_")
                || user_agent.contains("Mac OS X 13_")
                || user_agent.contains("Mac OS X 14_")
                || user_agent.contains("Mac OS X 15_")
                || user_agent.contains("Mac OS X 26_");
            return modern_macos
                && parse_version_after(user_agent, "Version/")
                    .is_some_and(|(major, minor)| version_at_least(major, minor, 14, 0));
        }
        false
    }

    /// Whether the agent reports lossless/alpha webp support.
    #[must_use]
    pub fn supports_webp_lossless_alpha(&self, user_agent: &str) -> bool {
        self.supports_webp_lossless_alpha
            .is_match(user_agent, false)
    }

    /// Whether the agent reports animated-webp support.
    #[must_use]
    pub fn supports_webp_animated(&self, user_agent: &str) -> bool {
        self.supports_webp_animated.is_match(user_agent, false)
    }

    /// Whether the agent is known to decode AVIF.
    #[must_use]
    pub fn supports_avif_rewritten_urls(&self, user_agent: &str) -> bool {
        if self.is_known_avif_unsupported(user_agent) {
            return false;
        }
        if user_agent.contains("Opera Mini") || user_agent.contains("KaiOS") {
            return false;
        }
        if user_agent.contains("Opera Mobi") || user_agent.contains("Opera Mobile") {
            return (parse_version_after(user_agent, "Opera Mobile/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 80, 0)))
                || (parse_version_after(user_agent, "Opera Mobi/")
                    .is_some_and(|(major, minor)| version_at_least(major, minor, 80, 0)));
        }
        if user_agent.contains("EdgiOS/")
            || (is_ios_user_agent(user_agent)
                && (user_agent.contains("CriOS/") || user_agent.contains("FxiOS/")))
        {
            return ios_version_at_least_16(user_agent);
        }
        if user_agent.contains("SamsungBrowser/") {
            return parse_version_after(user_agent, "SamsungBrowser/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 14, 0));
        }
        if user_agent.contains("UCBrowser/") {
            return parse_version_after(user_agent, "UCBrowser/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 15, 5));
        }
        if user_agent.contains("BaiduBrowser/") {
            return parse_version_after(user_agent, "BaiduBrowser/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 13, 52));
        }
        if user_agent.contains("Version/")
            && user_agent.contains("Safari/")
            && !user_agent.contains("Chrome/")
            && !user_agent.contains("CriOS/")
            && !user_agent.contains("Edg")
        {
            if is_ios_user_agent(user_agent) {
                return ios_version_at_least_16(user_agent);
            }
            return parse_version_after(user_agent, "Version/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 16, 4));
        }
        if user_agent.contains("Firefox/") {
            return parse_version_after(user_agent, "Firefox/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 93, 0));
        }
        if user_agent.contains("OPR/") {
            return parse_version_after(user_agent, "OPR/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 71, 0));
        }
        if user_agent.contains("Edg/") {
            return parse_version_after(user_agent, "Edg/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 121, 0));
        }
        if user_agent.contains("EdgA/") {
            return parse_version_after(user_agent, "EdgA/")
                .is_some_and(|(major, minor)| version_at_least(major, minor, 121, 0));
        }
        if user_agent.contains("EdgiOS/") {
            return ios_version_at_least_16(user_agent);
        }
        // Chrome iOS is WebKit-based; use the iOS platform version,
        // not its Chrome marketing version. Chromium-based Android
        // browsers/WebView follow the engine's AVIF support threshold.
        if user_agent.contains("CriOS/") {
            return if is_ios_user_agent(user_agent) {
                ios_version_at_least_16(user_agent)
            } else {
                parse_version_after(user_agent, "CriOS/")
                    .is_some_and(|(major, minor)| version_at_least(major, minor, 85, 0))
            };
        }
        if user_agent.contains("Chrome/") || user_agent.contains("Chromium/") {
            let token = if user_agent.contains("Chrome/") {
                "Chrome/"
            } else {
                "Chromium/"
            };
            return parse_version_after(user_agent, token)
                .is_some_and(|(major, minor)| version_at_least(major, minor, 85, 0));
        }
        false
    }

    /// Whether the agent is known to lack (full) AVIF support.
    #[must_use]
    pub fn is_known_avif_unsupported(&self, user_agent: &str) -> bool {
        if user_agent.is_empty() {
            return false;
        }
        if self.is_ie(user_agent)
            || user_agent.contains("Edge/")
            || user_agent.contains("Opera Mini")
            || user_agent.contains("KaiOS")
        {
            return true;
        }
        if user_agent.contains("AndroidBrowser/") || user_agent.contains("Android Browser") {
            return true;
        }
        if user_agent.contains("Android ")
            && !user_agent.contains("Chrome/")
            && !user_agent.contains("Chromium/")
        {
            if let Some((major, minor)) = parse_version_after(user_agent, "Android ") {
                if major < 5 || (major == 4 && minor <= 4) {
                    return true;
                }
            }
        }
        if user_agent.contains("Opera Mobi") || user_agent.contains("Opera Mobile") {
            if let Some((major, minor)) = parse_version_after(user_agent, "Opera Mobi/")
                .or_else(|| parse_version_after(user_agent, "Opera Mobile/"))
            {
                return !version_at_least(major, minor, 80, 0);
            }
            if !user_agent.contains("OPR/") {
                if let Some((major, minor)) = parse_version_after(user_agent, "Version/") {
                    return major < 13 || (major == 12 && minor <= 1);
                }
            }
        }
        if user_agent.contains("Opera/") && !user_agent.contains("OPR/") {
            if let Some((major, minor)) = parse_version_after(user_agent, "Version/") {
                return major < 13 || (major == 12 && minor <= 1);
            }
            return true;
        }
        if (user_agent.contains("EdgiOS/")
            || (is_ios_user_agent(user_agent)
                && (user_agent.contains("CriOS/") || user_agent.contains("FxiOS/"))))
            && !ios_version_at_least_16(user_agent)
        {
            return true;
        }
        if user_agent.contains("Version/")
            && user_agent.contains("Safari/")
            && is_ios_user_agent(user_agent)
        {
            return !ios_version_at_least_16(user_agent);
        }
        if user_agent.contains("Version/")
            && user_agent.contains("Safari/")
            && !user_agent.contains("Chrome/")
            && !user_agent.contains("CriOS/")
            && !user_agent.contains("Edg")
        {
            return parse_version_after(user_agent, "Version/")
                .is_none_or(|(major, minor)| !version_at_least(major, minor, 16, 4));
        }
        if user_agent.contains("Safari/")
            && !user_agent.contains("Chrome/")
            && !user_agent.contains("CriOS/")
            && !user_agent.contains("Edg")
            && !user_agent.contains("OPR/")
            && !user_agent.contains("Version/")
        {
            return true;
        }
        if user_agent.contains("Edg/") {
            if let Some((major, minor)) = parse_version_after(user_agent, "Edg/") {
                return !version_at_least(major, minor, 121, 0);
            }
        }
        if user_agent.contains("EdgA/") {
            if let Some((major, minor)) = parse_version_after(user_agent, "EdgA/") {
                return !version_at_least(major, minor, 121, 0);
            }
        }
        if user_agent.contains("Firefox/") {
            if let Some((major, minor)) = parse_version_after(user_agent, "Firefox/") {
                return !version_at_least(major, minor, 93, 0);
            }
        }
        if user_agent.contains("OPR/") {
            if let Some((major, minor)) = parse_version_after(user_agent, "OPR/") {
                return !version_at_least(major, minor, 71, 0);
            }
        }
        // NOTE: the C++ test marks QQBrowser/14.9 unsupported even
        // beside a modern Chrome token, so this check runs ahead of
        // the Chrome one (where the C++ order would shadow it).
        if user_agent.contains("QQBrowser/") {
            return parse_version_after(user_agent, "QQBrowser/")
                .is_some_and(|(major, minor)| major == 14 && minor == 9);
        }
        if user_agent.contains("Chrome/") || user_agent.contains("Chromium/") {
            let token = if user_agent.contains("Chrome/") {
                "Chrome/"
            } else {
                "Chromium/"
            };
            if let Some((major, minor)) = parse_version_after(user_agent, token) {
                return !version_at_least(major, minor, 85, 0);
            }
        }
        if user_agent.contains("UCBrowser/") {
            return parse_version_after(user_agent, "UCBrowser/")
                .is_none_or(|(major, minor)| !version_at_least(major, minor, 15, 5));
        }
        if user_agent.contains("BaiduBrowser/") {
            return parse_version_after(user_agent, "BaiduBrowser/")
                .is_none_or(|(major, minor)| !version_at_least(major, minor, 13, 52));
        }
        if user_agent.contains("SamsungBrowser/") {
            return parse_version_after(user_agent, "SamsungBrowser/")
                .is_none_or(|(major, minor)| !version_at_least(major, minor, 14, 0));
        }
        false
    }

    /// Whether the agent is Android.
    #[must_use]
    pub fn is_android_user_agent(&self, user_agent: &str) -> bool {
        user_agent.contains("Android")
    }

    /// Whether the agent is iOS.
    #[must_use]
    pub fn is_ios_user_agent(&self, user_agent: &str) -> bool {
        user_agent.contains("iPhone") || user_agent.contains("iPad")
    }

    /// Parses a Chromium build number, if present.
    ///
    /// NOTE: the C++ test requires the LAST Chromium token to win
    /// (page-embedded `Chrome/` before `Edg/`/`OPR/`), i.e. a greedy
    /// `.*` prefix; a plain leftmost search would return the earlier
    /// `Chrome/` token instead.
    pub fn chrome_build_number(&self, user_agent: &str) -> Option<(i32, i32, i32, i32)> {
        let caps = self
            .chrome_version_pattern
            .captures_iter(user_agent)
            .last()?;
        let number = |index: usize| caps[index].parse::<i32>().unwrap_or(-1);
        Some((number(1), number(2), number(3), number(4)))
    }

    /// Whether the agent prefetches DNS via `rel=prefetch` (IE9).
    #[must_use]
    pub fn supports_dns_prefetch_using_rel_prefetch(&self, user_agent: &str) -> bool {
        self.is_ie9(user_agent)
    }

    /// Classifies the agent's device (`GetDeviceTypeForUA`).
    #[must_use]
    pub fn device_type_for_ua(&self, user_agent: &str) -> DeviceType {
        if self.mobile_user_agents.is_match(user_agent, false) {
            return DeviceType::Mobile;
        }
        if self.tablet_user_agents.is_match(user_agent, false) {
            return DeviceType::Tablet;
        }
        DeviceType::Desktop
    }

    /// Names a device type (`DeviceTypeString`).
    #[must_use]
    pub fn device_type_string(device_type: DeviceType) -> &'static str {
        match device_type {
            DeviceType::Mobile => "mobile",
            DeviceType::Tablet => "tablet",
            DeviceType::Desktop => "desktop",
        }
    }

    /// Suffixes a device type (`DeviceTypeSuffix`).
    #[must_use]
    pub fn device_type_suffix(device_type: DeviceType) -> &'static str {
        match device_type {
            DeviceType::Mobile => "@Mobile",
            DeviceType::Tablet => "@Tablet",
            DeviceType::Desktop => "@Desktop",
        }
    }

    /// Whether a Chrome-iOS build clears build/patch floors.
    #[must_use]
    pub fn user_agent_exceeds_chrome_ios_build_and_patch(
        &self,
        user_agent: &str,
        required_build: i32,
        required_patch: i32,
    ) -> bool {
        // Verify if this is an iOS user agent.
        if !self.is_ios_user_agent(user_agent) {
            return false;
        }
        self.user_agent_exceeds_chrome_build_and_patch(user_agent, required_build, required_patch)
    }

    /// Whether a Chrome-Android build clears build/patch floors.
    #[must_use]
    pub fn user_agent_exceeds_chrome_android_build_and_patch(
        &self,
        user_agent: &str,
        required_build: i32,
        required_patch: i32,
    ) -> bool {
        // Verify if this is an Android user agent.
        if !self.is_android_user_agent(user_agent) {
            return false;
        }
        self.user_agent_exceeds_chrome_build_and_patch(user_agent, required_build, required_patch)
    }

    /// Whether a Chromium build clears build/patch floors.
    #[must_use]
    pub fn user_agent_exceeds_chrome_build_and_patch(
        &self,
        user_agent: &str,
        required_build: i32,
        required_patch: i32,
    ) -> bool {
        // By default user agent sniffing is disabled.
        if required_build == -1 && required_patch == -1 {
            return false;
        }
        let Some((_, _, parsed_build, parsed_patch)) = self.chrome_build_number(user_agent) else {
            return false;
        };
        if parsed_build < required_build {
            return false;
        } else if parsed_build == required_build && parsed_patch < required_patch {
            return false;
        }
        true
    }

    /// Whether the agent supports mobilization.
    #[must_use]
    pub fn supports_mobilization(&self, user_agent: &str) -> bool {
        self.mobilization_user_agents.is_match(user_agent, false)
    }
}

impl Default for UserAgentMatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Parses `major.minor` after a token (`ParseVersionAfter`).
fn parse_version_after(user_agent: &str, token: &str) -> Option<(i32, i32)> {
    let mut version = user_agent.split_once(token)?.1;
    let first = version.as_bytes().first()?;
    if !first.is_ascii_digit() {
        return None;
    }
    let mut major: i32 = 0;
    let mut minor: i32 = 0;
    while let Some(digit) = version.as_bytes().first().filter(|b| b.is_ascii_digit()) {
        major = major
            .saturating_mul(10)
            .saturating_add(i32::from(digit - b'0'));
        version = &version[1..];
    }
    if let Some(separator) = version.as_bytes().first() {
        if *separator != b'.' && *separator != b'_' {
            return Some((major, minor));
        }
        version = &version[1..];
        while let Some(digit) = version.as_bytes().first().filter(|b| b.is_ascii_digit()) {
            minor = minor
                .saturating_mul(10)
                .saturating_add(i32::from(digit - b'0'));
            version = &version[1..];
        }
    }
    Some((major, minor))
}

/// Whether a version clears a floor (`VersionAtLeast`).
fn version_at_least(major: i32, minor: i32, required_major: i32, required_minor: i32) -> bool {
    major > required_major || (major == required_major && minor >= required_minor)
}

/// Whether the iOS version clears 16.0 (`IOSVersionAtLeast16`).
fn ios_version_at_least_16(user_agent: &str) -> bool {
    (parse_version_after(user_agent, "iPhone OS ")
        .or_else(|| parse_version_after(user_agent, "CPU OS ")))
    .is_some_and(|(major, minor)| version_at_least(major, minor, 16, 0))
}

/// Whether the agent names iOS hardware (`IsIOSUserAgent`).
fn is_ios_user_agent(user_agent: &str) -> bool {
    user_agent.contains("iPhone")
        || user_agent.contains("iPad")
        || user_agent.contains("iPod")
        || user_agent.contains("CPU OS ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCEPT_HEADER_VALUE_MOBILE: &str = "text/html,application/vnd.wap.xhtml+xml";
    const ACCEPT_HEADER_VALUE_NON_MOBILE: &str = "text/html";
    const ALCATEL_MOBILE_USER_AGENT: &str = concat!(
        "ALCATEL_one_touch_310A/1.0 Profile/MIDP-2.0 ",
        "Configuration/CLDC-1.1 ObigoInternetBrowser/Q03C"
    );
    const ALCATEL_USER_AGENT: &str = "Alcatel_one_touch_214/1.0 ObigoInternetBrowser/Q03C";
    const AMOI_USER_AGENT: &str = "Amoi 8512/R18.0 NF-Browser/3.3";
    const ANDROID_CHROME18_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 4.0.4; Galaxy Nexus Build/IMM76B) ",
        "AppleWebKit/535.19 (KHTML, like Gecko) Chrome/18.0.1025.133 Mobile ",
        "Safari/535.19"
    );
    const ANDROID_CHROME21_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 4.1.4; Galaxy Nexus Build/IMM76B) ",
        "AppleWebKit/535.19 (KHTML, like Gecko) Chrome/21.0.1025.133 Mobile ",
        "Safari/535.19"
    );
    const ANDROID_HC_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; U; Android 3.2; en-us; Sony Tablet S Build/THMAS11000)",
        " AppleWebKit/534.13 (KHTML, like Gecko) Version/4.0 Safari/534.13"
    );
    const ANDROID_ICS_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; U; Android 4.0.1; en-us; Galaxy Nexus Build/ICL27) ",
        "AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Mobile Safari/534.30"
    );
    const PAGESPEED_INSIGHTS_MOBILE_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 6.0.1; Nexus 5X Build/MMB29P) ",
        "AppleWebKit/537.36 (KHTML, like Gecko; Google Page Speed Insights) ",
        "Chrome/27.0.1453 Mobile Safari/537.36"
    );
    const PAGESPEED_INSIGHTS_DESKTOP_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko; ",
        "Google Page Speed Insights) Chrome/27.0.1453 Safari/537.36"
    );
    const ANDROID_NEXUS_S_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; U; Android 2.3.3; en-gb; Nexus S Build/GRI20)",
        "AppleWebKit/533.1 (KHTML, like Gecko) Version/4.0 Mobile Safari/533.1"
    );
    const BENQ_USER_AGENT: &str = "BENQ-A500";
    const BLACK_BERRY_OS5_USER_AGENT: &str = concat!(
        "BlackBerry9000/5.0.0.93 Profile/MIDP-2.0 Configuration/CLDC-1.1 ",
        "VendorID/179"
    );
    const BLACK_BERRY_OS6_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (BlackBerry; U; BlackBerry 9800; en-US) AppleWebKit/534.11+ ",
        "(KHTML, like Gecko) Version/6.0.0.141 Mobile Safari/534.11+"
    );
    const CHROME12_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_6_4) ",
        "AppleWebKit/534.30 (KHTML, like Gecko) Chrome/12.0.742.100 Safari/534.30"
    );
    const CHROME15_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows; U; Windows NT 6.1; en-US) ",
        "AppleWebKit/534.13 (KHTML, like Gecko) Chrome/15.0.597.19 Safari/534.13"
    );
    const CHROME18_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows; U; Windows NT 6.1; en-US) ",
        "AppleWebKit/534.13 (KHTML, like Gecko) Chrome/18.0.597.19 Safari/534.13"
    );
    const CHROME9_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows; U; Windows NT 6.1; en-US) ",
        "AppleWebKit/534.13 (KHTML, like Gecko) Chrome/9.0.597.19 Safari/534.13"
    );
    const CHROME31_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows NT 5.1) AppleWebKit/537.36 (KHTML, like Gecko) ",
        "Chrome/31.0.1650.16 Safari/537.36"
    );
    const CHROME32_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows NT 6.2; Win64; x64) AppleWebKit/537.36 ",
        "(KHTML, like Gecko) Chrome/32.0.1667.0 Safari/537.36"
    );
    const CHROME37_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_9_4) ",
        "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/37.0.2062.122 Safari/537.36"
    );
    const CHROME41_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko)",
        " Chrome/41.0.2227.0 Safari/537.36"
    );
    const CHROME42_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko)",
        " Chrome/42.0.2302.4 Safari/537.36"
    );
    const CHROME_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows; U; Windows NT 5.1; en-US) ",
        "AppleWebKit/525.13 (KHTML, like Gecko) Chrome/0.A.B.C Safari/525.13"
    );
    const COMPAL_USER_AGENT: &str = "Compal-A618";
    const CRI_OS31_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 7_0_3 like Mac OS X) ",
        "AppleWebKit/537.51.1 (KHTML, like Gecko) CriOS/31.0.1650.18 Mobile/11B511 ",
        "Safari/8536.25 (37014222-1F1D-4117-89B3-699D5C10628A)"
    );
    const CRI_OS32_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 7_0_4 like Mac OS X) ",
        "AppleWebKit/537.51.1 (KHTML, like Gecko) CriOS/32.0.1700.21 ",
        "Mobile/11B554a Safari/9537.53 (A7BED55D-B09E-484F-B2FE-5E4952E9B87E)"
    );
    const CRI_OS48_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 9_2 like Mac OS X) ",
        "AppleWebKit/601.1 (KHTML, like Gecko) CriOS/48.0.2564.87 ",
        "Mobile/13C75 Safari/601.1.46"
    );
    const DO_CO_MO_MOBILE_USER_AGENT: &str = "DoCoMo/1.0/D505iS/c20/TB/W20H10";
    const FIREFOX1_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (X11; U; Linux i686; en-US; rv:1.8.0.7) ",
        "Gecko/20060909 Firefox/1.5.0.7 MG (Novarra-Vision/6.1)"
    );
    const FIREFOX3_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (X11; U; Linux x86_64; zh-CN; rv:1.9.2.10) ",
        "Gecko/20100922 Ubuntu/10.10 (maverick) Firefox/3.6.10"
    );
    const FIREFOX42_ANDROID_USER_AGENT: &str =
        "Mozilla/5.0 (Android 5.0.1; Mobile; rv:42.0) Gecko/42.0 Firefox/42.0";
    const FIREFOX5_USER_AGENT: &str =
        "Mozilla/5.0 (X11; U; Linux i586; de; rv:5.0) Gecko/20100101 Firefox/5.0";
    const FIREFOX_MOBILE_USER_AGENT: &str =
        "Mozilla/5.0 (Android; Mobile; rv:38.0) Gecko/38.0 Firefox/38.0";
    const FIREFOX_NOKIA_N800: &str = concat!(
        "Mozilla/5.0 (X11; U; Linux armv6l; en-US; rv:1.9a6pre) Gecko/20070810 ",
        "Firefox/3.0a1 Tablet browser 0.1.16 RX-34_2007SE_4.2007.38-2"
    );
    const FIREFOX_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:27.0) ",
        "Gecko/20100101 Firefox/27.0"
    );
    const FLY_USER_AGENT: &str = "FLY-2040i/BSI AU.Browser/2.0 QO3C1 MMP/1.0";
    const GENERIC_ANDROID_USER_AGENT: &str = "Android";
    const GOOGLEBOT_USER_AGENT: &str =
        "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
    const GOOGLE_ADS_BOT_USER_AGENT: &str = "AdsBot-Google";
    const GOOGLE_ADS_BOT_MOBILE_USER_AGENT: &str = "AdsBot-Google-Mobile";
    const GOOGLE_PLUS_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows NT 6.1; rv:6.0) Gecko/20110814 Firefox/6.0 Google ",
        "(+https://developers.google.com/+/web/snippet/)"
    );
    const IE6_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows; U; MSIE 6.0; Windows NT 5.1; SV1;",
        " .NET CLR 2.0.50727)"
    );
    const IE7_USER_AGENT: &str = "Mozilla/5.0 (Windows; U; MSIE 7.0; Windows NT 6.0; en-US)";
    const IE8_USER_AGENT: &str = concat!(
        "Mozilla/4.0 (compatible; MSIE 8.0; Windows NT 6.1; WOW64;",
        " Trident/4.0; SLCC2; .NET CLR 2.0.50727; .NET CLR 3.5.30729;",
        " .NET CLR 3.0.30729; Media Center PC 6.0; InfoPath.2;",
        " .NET4.0C; .NET4.0E; FDM)"
    );
    const IE9_USER_AGENT: &str = "Mozilla/5.0 (Windows; U; MSIE 9.0; WIndows NT 9.0; en-US))";
    const IE10_USER_AGENT: &str = "Mozilla/5.0 (Windows; U; MSIE 10.0; WIndows NT 9.0; en-US))";
    const I_PAD_TABLET_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPad; CPU OS 6_1_3 like Mac OS X) AppleWebKit/536.26 (KHTML, ",
        "like Gecko) Version/6.0 Mobile/10B329 Safari/8536.25"
    );
    const I_PAD_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPad; U; CPU OS 3_2 like Mac OS X; en-us) ",
        "AppleWebKit/531.21.10 (KHTML, like Gecko) Version/4.0.4 ",
        "Mobile/7B334b Safari/531.21.10"
    );
    const I_PAD_CHROME28_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPad; CPU OS 613 like Mac OS X) AppleWebKit/536.26 (KHTML ",
        "like Gecko) CriOS/28.0.1500.12 Mobile/10B329 Safari/8536.25"
    );
    const I_PAD_CHROME29_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPad; CPU OS 613 like Mac OS X) AppleWebKit/536.26 (KHTML ",
        "like Gecko) CriOS/29.0.1547.11 Mobile/10B329 Safari/8536.25"
    );
    const I_PAD_CHROME36_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPad; CPU OS 7_1_1 like Mac OS X) AppleWebKit/537.51.2 ",
        "(KHTML, like Gecko) CriOS/36.0.1985.49 Mobile/11D201 Safari/9537.53"
    );
    const I_PHONE4_SAFARI: &str = concat!(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 5_0_1 like Mac OS X) AppleWebKit/534.46",
        " (KHTML, like Gecko) Version/5.1 Mobile/9A405 Safari/7534.48.3"
    );
    const I_PHONE_CHROME21_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 6_0_1 like Mac OS X; en-us) ",
        "AppleWebKit/534.46.0 (KHTML, like Gecko) CriOS/21.0.1180.82 ",
        "Mobile/10A523 Safari/7534.48.3"
    );
    const I_PHONE_CHROME36_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (iPhone; CPU iPhone OS 7_1_2 like Mac OS X) ",
        "AppleWebKit/537.51.2 (KHTML, like Gecko) CriOS/36.0.1985.49 ",
        "Mobile/11D257 Safari/9537.53"
    );
    const I_PHONE_USER_AGENT: &str = "Apple iPhone OS v2.1.1 CoreMedia v1.0.0.5F138";
    const I_POD_SAFARI: &str = concat!(
        "Mozilla/5.0 (iPod; U; CPU iPhone OS 4_3_3 like Mac OS X; en-us)",
        " AppleWebKit/533.17.9 (KHTML, like Gecko) Version/5.0.2 Mobile/8J2",
        " Safari/6533.18.5"
    );
    const I_USER_AGENT: &str = "i-mobile318";
    const J_MOBILE_USER_AGENT: &str = "J-PHONE/3.0/J-SA05";
    const KDDI_MOBILE_USER_AGENT: &str = "KDDI-CA31 UP.Browser/6.2.0.7.3.129 (GUI) MMP/2.0";
    const KINDLE_TABLET_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; U; Android 2.3.4; en-us; Kindle Fire ",
        "Build/GINGERBREAD) AppleWebKit/533.1 (KHTML, like Gecko) Version/4.0 ",
        "Mobile Safari/533.1"
    );
    const KWC_MOBILE_USER_AGENT: &str = "KWC-E2000/1003 UP.Browser/7.2.6.1.475 (GUI) MMP/2.0";
    const LENOVO_USER_AGENT: &str = "LENOVO-E307_ENG_RUS_FLY/(2006.05.10)S276/WAP1.2.1";
    const LGE_MOBILE_USER_AGENT: &str = "LGE-AX300/1.0 UP.Browser/6.2.3.8 (GUI) MMP/2.0";
    const LGE_USER_AGENT: &str = "LGE-CU8188";
    const LGMIDP_MOBILE_USER_AGENT: &str =
        "LG-A225/V100 Obigo/WAP2.0 Profile/MIDP-2.1 Configuration/CLDC-1.1";
    const LGUP_BROWSER_MOBILE_USER_AGENT: &str = "LG8500/1.0 UP.Browser/6.2.3.9 (GUI) MMP/2.0";
    const LG_USER_AGENT: &str = "LG-B2000";
    const MOT_MOBILE_USER_AGENT: &str = "MOT-1.2.0/11.03 UP.Browser/4.1.27a";
    const MOZILLA_MOBILE_USER_AGENT: &str = concat!(
        "Mozilla/4.0 (compatible; MSIE 4.01; Windows CE; Smartphone; ",
        "SDA/2.0 Profile/MIDP-2.0 Configuration/CLDC-1.1)"
    );
    const MOZILLA_USER_AGENT: &str = "Mozilla/1.22 (compatible; MMEF20; Cellphone; Sony CMD-Z5)";
    const NEC_USER_AGENT: &str = "NEC-E122/1.0 TMT-Mobile-Internet-Browser/1.1.14.20 (GUI)";
    const NEXUS10_CHROME_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 4.2.2; Nexus 10 Build/JDQ39) AppleWebKit/",
        "537.31 (KHTML, like Gecko) Chrome/26.0.1408.0 Safari/537.31"
    );
    const NEXUS6_CHROME44_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 5.1.1; Nexus 6 Build/LVY48E) ",
        "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/44.0.2403.133 Mobile ",
        "Safari/537.36"
    );
    const NEXUS7_CHROME_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 4.2; Nexus 7 Build/JOP32C) AppleWebKit/535.19",
        "(KHTML, like Gecko) Chrome/18.0.1025.166 Safari/535.19"
    );
    const NOKIA_MOBILE_USER_AGENT: &str = concat!(
        "Nokia6600/1.0 (2.33.0) SymbianOS/7.0s Series60/2.0 ",
        "Profile/MIDP-2.0 Configuration/CLDC-1.0"
    );
    const NOKIA_USER_AGENT: &str =
        "Nokia2355/1.0 (JN100V0200.nep) UP.Browser/6.2.2.1.c.1.108 (GUI) MMP/2.0";
    const OPERA1101_USER_AGENT: &str =
        "Opera/9.80 (Windows NT 5.2; U; ru) Presto/2.7.62 Version/11.01";
    const OPERA1110_USER_AGENT: &str =
        "Opera/9.80 (Windows NT 6.0; U; en) Presto/2.8.99 Version/11.10";
    const OPERA5_USER_AGENT: &str = "Opera/5.0 (SunOS 5.8 sun4u; U) [en]";
    const OPERA8_USER_AGENT: &str = "Opera/8.01 (J2ME/MIDP; Opera Mini/1.1.2666/1724; en; U; ssr)";
    const OPERA18_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows NT 6.2; WOW64) AppleWebKit/537.36 ",
        "(KHTML, like Gecko) Chrome/31.0.1650.63 Safari/537.36 OPR/18.0.1284.68"
    );
    const OPERA19_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows NT 6.3; WOW64) AppleWebKit/537.36 ",
        "(KHTML, like Gecko) Chrome/32.0.1700.76 Safari/537.36 OPR/19.0.1326.56"
    );
    const OPERA_MINI_MOBILE_USER_AGENT: &str = concat!(
        "Opera/8.01 (J2ME/MIDP; Opera Mini/1.1.4821/hifi/tmobile/uk; ",
        "Motorola V3; en; U; ssr)"
    );
    const OPERA_MOBI9: &str = "Opera/9.51 Beta (Microsoft Windows; PPC; Opera Mobi/1718; U; en)";
    const OPERA_MOBIL_MOBILE_USER_AGENT: &str = concat!(
        "Opera/9.80 (Android 4.0.4; Linux; Opera Mobi/ADR-1104201100; U; ru) ",
        "Presto/2.7.81 Version/11.00"
    );
    const OPERA_WITH_FIREFOX_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Windows NT 6.0; rv:2.0) Gecko/20100101 Firefox/4.0 Opera ",
        "12.14"
    );
    const PANASONIC_MOBILE_USER_AGENT: &str = concat!(
        "Panasonic-G60/1.0 UP.Browser/6.1.0.6 ",
        "MMP/1.0 UP.Browser/6.1.0.6 (GUI) MMP/1.0"
    );
    const PG_USER_AGENT: &str = "PG-1610/R01";
    const PHILIPS_USER_AGENT: &str = "PHILIPS 330 / Obigo Internet Browser 2.0";
    const PORTALMMM_MOBILE_USER_AGENT: &str = "portalmmm/2.0 6120(c100;TB)";
    const PSP_USER_AGENT: &str = "Mozilla/4.0 (PSP (PlayStation Portable); 2.00)";
    const ROVER_USER_AGENT: &str = "Rover 3.5 (RIM Handheld; Mobitex; OS v. 2.1)";
    const SAFARI_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_6_8) AppleWebKit/534.51.22 ",
        "(KHTML, like Gecko) Version/5.1.1 Safari/534.51.22"
    );
    const SAFARI6_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_7_4) AppleWebKit/536.25 ",
        "(KHTML, like Gecko) Version/6.0 Safari/536.25"
    );
    const SAFARI9_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_11_5) AppleWebKit/601.6.17 ",
        "(KHTML, like Gecko) Version/9.1.1 Safari/601.6.17"
    );
    const SAGEM_MOBILE_USER_AGENT: &str = "SAGEM-my202C/Orange1.0 UP.Browser/5.0.5.6 (GUI)";
    const SAGEM_USER_AGENT: &str = "SAGEM-942";
    const SAMSUNG_MOBILE_USER_AGENT: &str = concat!(
        "SAMSUNG-B2700/SXIB1 SHP/VPP/R5 NetFront/3.4 SMM-MMS/1.2.0 ",
        "profile/MIDP-2.0 configuration/CLDC-1.1"
    );
    const SCH_MOBILE_USER_AGENT: &str = "SCH-A850 UP.Browser/6.2.3.2 (GUI) MMP/2.0";
    const SCH_USER_AGENT: &str = "SCH-U350/1.0 NetFront/3.0.22.2.18 (GUI) MMP/2.0";
    const SEC_MOBILE_USER_AGENT: &str = "SEC-scha310 UP.Browser/4.1.26c3";
    const SGH_USER_AGENT: &str = "SGH-Z230";
    const SHARP_MOBILE_USER_AGENT: &str = concat!(
        "SHARP-TQ-GX10/0.0 Profile/MIDP-1.0 Configuration/CLDC-1.0 ",
        "UP.Browser/6.1.0.2.129 (GUI) MMP/1.0"
    );
    const SHARP_USER_AGENT: &str = "SHARP-TQ-GX15";
    const SIE_MOBILE_USER_AGENT: &str = "SIE-2128/24 UP.Browser/5.0.3.3 (GUI)";
    const SIE_USER_AGENT: &str = "SIE-A40";
    const SILK_DESKTOP_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (PlayStation Vita 2.10) AppleWebKit/536.26 (KHTML, ",
        "like Gecko) Silk/3.2"
    );
    const SILK_TABLET_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; U; Android 2.3.4; en-us; Silk/1.0.22.153_10033210) ",
        "AppleWebKit/533.1 (KHTML, like Gecko) Version/4.0 Mobile Safari/533.1 ",
        "Silk-Accelerated=true"
    );
    const SOFT_BANK_MOBILE_USER_AGENT: &str = concat!(
        "SoftBank/1.0/705NK/NKJ001 Series60/3.0 NokiaN73/3.0650 ",
        "Profile/MIDP-2.0 Configuration/CLDC-1.1"
    );
    const SPICE_USER_AGENT: &str = concat!(
        "Spice M6800  Opera/9.80 (MTK; Nucleus; U; en-US) Presto/2.4.18 ",
        "Version/10.00"
    );
    const TIANYU_USER_AGENT: &str = "TIANYU-KTOUCH/B2012";
    const VODAFONE_MOBILE_USER_AGENT: &str = concat!(
        "Vodafone/1.0/0Vodafone710/B616 Browser/Obigo-Browser/Q04A ",
        "MMS/Obigo-MMS/Q04A SyncML/HW-SyncML/1.0 Java/QVM/4.1 Profile/MIDP-2.0 ",
        "Configuration/CLDC-1.1"
    );
    const WINDOWS_PHONE_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Mobile; Windows Phone 8.1; Android 4.0; ARM; Trident/7.0; ",
        "Touch; rv:11.0; IEMobile/11.0; NOKIA; Lumia 928) like iPhone OS 7_0_3 ",
        "Mac OS X AppleWebKit/537 (KHTML, like Gecko) Mobile Safari/537"
    );
    const WIN_WAP_USER_AGENT: &str = "WinWAP/1.3 (1.3.0.0;WinCE;PPC2003)";
    const X_WAP_PROFILE_HEADER_VALUE: &str = "http://foo.bar.xml";
    const X_WAP_PROFILE: &str = "x-wap-profile";
    const YOUR_WAP_USER_AGENT: &str = "YourWap Ericsson 380/2.63";
    const ZTE_MOBILE_USER_AGENT: &str = "ZTE-C705/1.0 UP.Browser/4.1.27a2";
    const XT907_USER_AGENT: &str = concat!(
        "Mozilla/5.0 (Linux; Android 4.1.1; XT907 Build/9.8.1Q_27-2) AppleWebKit",
        "/537.25 (KHTML, like Gecko) Chrome/26.0.1376.1 Mobile Safari/537.25"
    );
    const TESTING_WEBP: &str = "webp";
    const TESTING_WEBP_LOSSLESS_ALPHA: &str = "webp-la";
    const TESTING_WEBP_ANIMATED: &str = "webp-animated";
    const IE11_USER_AGENTS: &[&str] = &[
        "Windows-RSS-Platform/2.0 (IE 11.0; Windows NT 6.1)",
        "Mozilla/5.0 (compatible; IE 11.0; Win32; Trident/7.0)",
        "Mozilla/5.0 (Windows NT 6.1; WOW64; ***********; rv:11.0) like Gecko",
        "Mozilla/5.0 (Windows NT 6.1; Win64; x64; Trident/7.0; rv:11.0) like Gecko",
    ];
    const MOBILE_USER_AGENTS: &[&str] = &[
        ALCATEL_MOBILE_USER_AGENT,
        ALCATEL_USER_AGENT,
        ANDROID_CHROME21_USER_AGENT,
        ANDROID_ICS_USER_AGENT,
        ANDROID_ICS_USER_AGENT,
        ANDROID_NEXUS_S_USER_AGENT,
        DO_CO_MO_MOBILE_USER_AGENT,
        I_PHONE4_SAFARI,
        I_PHONE_CHROME21_USER_AGENT,
        I_PHONE_USER_AGENT,
        I_POD_SAFARI,
        J_MOBILE_USER_AGENT,
        KDDI_MOBILE_USER_AGENT,
        KWC_MOBILE_USER_AGENT,
        LGE_MOBILE_USER_AGENT,
        LGMIDP_MOBILE_USER_AGENT,
        LGUP_BROWSER_MOBILE_USER_AGENT,
        MOT_MOBILE_USER_AGENT,
        MOZILLA_MOBILE_USER_AGENT,
        NOKIA_MOBILE_USER_AGENT,
        OPERA_MINI_MOBILE_USER_AGENT,
        OPERA_MOBI9,
        OPERA_MOBIL_MOBILE_USER_AGENT,
        PANASONIC_MOBILE_USER_AGENT,
        PHILIPS_USER_AGENT,
        PORTALMMM_MOBILE_USER_AGENT,
        SAGEM_MOBILE_USER_AGENT,
        SAMSUNG_MOBILE_USER_AGENT,
        SCH_MOBILE_USER_AGENT,
        SEC_MOBILE_USER_AGENT,
        SHARP_MOBILE_USER_AGENT,
        SIE_MOBILE_USER_AGENT,
        SOFT_BANK_MOBILE_USER_AGENT,
        VODAFONE_MOBILE_USER_AGENT,
        ZTE_MOBILE_USER_AGENT,
        GOOGLE_ADS_BOT_MOBILE_USER_AGENT,
    ];
    const DESKTOP_USER_AGENTS: &[&str] = &[
        "not a mobile",
        AMOI_USER_AGENT,
        BENQ_USER_AGENT,
        COMPAL_USER_AGENT,
        FIREFOX_NOKIA_N800,
        FLY_USER_AGENT,
        I_USER_AGENT,
        LENOVO_USER_AGENT,
        LGE_USER_AGENT,
        LG_USER_AGENT,
        MOZILLA_USER_AGENT,
        NEC_USER_AGENT,
        OPERA1101_USER_AGENT,
        PG_USER_AGENT,
        ROVER_USER_AGENT,
        SAFARI_USER_AGENT,
        SAGEM_USER_AGENT,
        SCH_USER_AGENT,
        SGH_USER_AGENT,
        SHARP_USER_AGENT,
        SIE_USER_AGENT,
        SPICE_USER_AGENT,
        TIANYU_USER_AGENT,
        WIN_WAP_USER_AGENT,
        YOUR_WAP_USER_AGENT,
        GOOGLE_ADS_BOT_USER_AGENT,
    ];
    const TABLET_USER_AGENTS: &[&str] = &[
        GENERIC_ANDROID_USER_AGENT,
        I_PAD_TABLET_USER_AGENT,
        I_PAD_USER_AGENT,
        KINDLE_TABLET_USER_AGENT,
        NEXUS7_CHROME_USER_AGENT,
        SILK_TABLET_USER_AGENT,
    ];
    const IMAGE_INLINING_SUPPORTED_USER_AGENTS: &[&str] = &[
        "",
        ANDROID_CHROME21_USER_AGENT,
        ANDROID_HC_USER_AGENT,
        ANDROID_ICS_USER_AGENT,
        CHROME_USER_AGENT,
        FIREFOX_USER_AGENT,
        IE9_USER_AGENT,
        IE10_USER_AGENT,
        I_PHONE_USER_AGENT,
        OPERA8_USER_AGENT,
        SAFARI_USER_AGENT,
        IE11_USER_AGENTS[0],
        IE11_USER_AGENTS[1],
        IE11_USER_AGENTS[2],
        IE11_USER_AGENTS[3],
        GOOGLE_ADS_BOT_USER_AGENT,
        GOOGLE_ADS_BOT_MOBILE_USER_AGENT,
    ];

    fn verify_get_device_type_for_ua() {
        let matcher = UserAgentMatcher::new();
        for ua in MOBILE_USER_AGENTS {
            assert_eq!(DeviceType::Mobile, matcher.device_type_for_ua(ua), "{ua}");
        }
        for ua in DESKTOP_USER_AGENTS {
            assert_eq!(DeviceType::Desktop, matcher.device_type_for_ua(ua), "{ua}");
        }
        for ua in TABLET_USER_AGENTS {
            assert_eq!(DeviceType::Tablet, matcher.device_type_for_ua(ua), "{ua}");
        }
    }

    fn verify_image_inlining_support() {
        let matcher = UserAgentMatcher::new();
        for ua in IMAGE_INLINING_SUPPORTED_USER_AGENTS {
            assert!(matcher.supports_image_inlining(ua), "{ua}");
        }
        assert!(!matcher.supports_image_inlining("random user agent"));
    }

    fn verify_mobilization_support() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.supports_mobilization(ANDROID_CHROME18_USER_AGENT));
        assert!(!matcher.supports_mobilization(ANDROID_HC_USER_AGENT));
        assert!(matcher.supports_mobilization(I_PHONE4_SAFARI));
        assert!(!matcher.supports_mobilization(I_PHONE_CHROME21_USER_AGENT));
        assert!(!matcher.supports_mobilization(BLACK_BERRY_OS6_USER_AGENT));
        assert!(matcher.supports_mobilization(FIREFOX_USER_AGENT));
        assert!(matcher.supports_mobilization(FIREFOX_MOBILE_USER_AGENT));
        assert!(matcher.supports_mobilization(CHROME42_USER_AGENT));
        assert!(!matcher.supports_mobilization(OPERA_MINI_MOBILE_USER_AGENT));
        assert!(matcher.supports_mobilization(GOOGLE_ADS_BOT_USER_AGENT));
        assert!(matcher.supports_mobilization(GOOGLE_ADS_BOT_MOBILE_USER_AGENT));
    }

    #[test]
    fn is_ie_test() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.is_ie(IE6_USER_AGENT));
        assert!(matcher.is_ie(IE7_USER_AGENT));
        assert!(matcher.is_ie(IE8_USER_AGENT));
        assert!(matcher.is_ie(IE9_USER_AGENT));
        for ua in IE11_USER_AGENTS {
            assert!(matcher.is_ie(ua), "{ua}");
        }
        assert!(!matcher.is_ie(FIREFOX_USER_AGENT));
        assert!(!matcher.is_ie(CHROME_USER_AGENT));
    }

    #[test]
    fn supports_image_inlining() {
        verify_image_inlining_support();
    }

    #[test]
    fn supports_lazyload_images() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.supports_lazyload_images(CHROME_USER_AGENT));
        assert!(matcher.supports_lazyload_images(FIREFOX_USER_AGENT));
        assert!(matcher.supports_lazyload_images(I_PHONE_USER_AGENT));
        assert!(matcher.supports_lazyload_images(BLACK_BERRY_OS6_USER_AGENT));
        assert!(!matcher.supports_lazyload_images(BLACK_BERRY_OS5_USER_AGENT));
        assert!(!matcher.supports_lazyload_images(GOOGLE_PLUS_USER_AGENT));
    }

    #[test]
    fn not_supports_image_inlining() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_image_inlining(IE6_USER_AGENT));
        assert!(!matcher.supports_image_inlining(FIREFOX1_USER_AGENT));
        assert!(!matcher.supports_image_inlining(NOKIA_USER_AGENT));
        assert!(!matcher.supports_image_inlining(OPERA5_USER_AGENT));
        assert!(!matcher.supports_image_inlining(PSP_USER_AGENT));
        assert!(!matcher.supports_image_inlining(GOOGLE_PLUS_USER_AGENT));
        assert!(matcher.supports_image_inlining(ANDROID_CHROME18_USER_AGENT));
    }

    #[test]
    fn supports_js_defer() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_js_defer(IE9_USER_AGENT, false));
        for ua in IE11_USER_AGENTS {
            assert!(!matcher.supports_js_defer(ua, false), "{ua}");
        }
        assert!(matcher.supports_js_defer(CHROME_USER_AGENT, false));
        assert!(matcher.supports_js_defer(FIREFOX_USER_AGENT, false));
        assert!(matcher.supports_js_defer(SAFARI_USER_AGENT, false));
    }

    #[test]
    fn supports_js_defer_allow_mobile() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.supports_js_defer(ANDROID_HC_USER_AGENT, true));
        assert!(matcher.supports_js_defer(I_PHONE4_SAFARI, true));
        // Desktop is also supported.
        assert!(matcher.supports_js_defer(CHROME_USER_AGENT, true));
    }

    #[test]
    fn not_supports_js_defer() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_js_defer(IE6_USER_AGENT, false));
        assert!(!matcher.supports_js_defer(IE8_USER_AGENT, false));
        assert!(!matcher.supports_js_defer(FIREFOX1_USER_AGENT, false));
        assert!(!matcher.supports_js_defer(FIREFOX3_USER_AGENT, false));
        assert!(!matcher.supports_js_defer(NOKIA_USER_AGENT, false));
        assert!(!matcher.supports_js_defer(OPERA5_USER_AGENT, false));
        assert!(!matcher.supports_js_defer(PSP_USER_AGENT, false));
        // Mobile is not supported too.
        assert!(!matcher.supports_js_defer(I_PHONE4_SAFARI, false));
    }

    #[test]
    fn not_supports_js_defer_allow_mobile() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_js_defer(OPERA_MOBI9, true));
    }

    // Googlebot for mobile generally includes the UA for the mobile
    // device being impersonated.
    const GOOGLEBOT_MOBILE: &str =
        "(compatible; Googlebot-Mobile/2.1; +http://www.google.com/bot.html)";

    #[test]
    fn mobile_bot_supports_js_defer() {
        let matcher = UserAgentMatcher::new();
        // Reference: https://developers.google.com/webmasters/smartphone-sites/
        // detecting-user-agents
        let google_bot_iphone_ua = format!(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 6_0 like Mac OS X) AppleWebKit/536.26 (KHTML, like Gecko) Version/6.0 Mobile/10A5376e Safari/8536.25 {GOOGLEBOT_MOBILE}"
        );
        assert!(matcher.supports_js_defer(&google_bot_iphone_ua, true));

        let google_bot_android_ua = format!(
            "Mozilla/5.0 (Linux; Android 4.3; Nexus 4 Build/JWR66Y) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/32.0.1666.0 Mobile Safari/537.36 {GOOGLEBOT_MOBILE}"
        );
        assert!(matcher.supports_js_defer(&google_bot_android_ua, true));

        // Feature-phones don't support JS Defer.
        let samsung_feature_bot = format!(
            "SAMSUNG-SGH-E250/1.0 Profile/MIDP-2.0 Configuration/CLDC-1.1 UP.Browser/6.2.3.3.c.1.101 (GUI) MMP/2.0 {GOOGLEBOT_MOBILE}"
        );
        let do_co_mo_bot = format!("DoCoMo/2.0 N905i(c100;TB;W24H16) {GOOGLEBOT_MOBILE}");
        assert!(!matcher.supports_js_defer(&samsung_feature_bot, true));
        assert!(!matcher.supports_js_defer(&do_co_mo_bot, true));
    }

    // Googlebot for desktop generally does not includes a specific
    // browser UA.
    const GOOGLEBOT_DESKTOP: &str = "(compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

    #[test]
    fn desktop_bot_supports_js_defer() {
        let matcher = UserAgentMatcher::new();
        // https://support.google.com/webmasters/answer/1061943?hl=en
        let googlebot_normal = format!("Mozilla/5.0 {GOOGLEBOT_DESKTOP}");
        assert!(matcher.supports_js_defer(&googlebot_normal, true));

        let googlebot_rare = "Googlebot/2.1 (+http://www.google.com/bot.html)";
        assert!(matcher.supports_js_defer(googlebot_rare, true));

        assert!(matcher.supports_js_defer("Googlebot-News", true));
        assert!(matcher.supports_js_defer("Googlebot-Image/1.0", true));
        assert!(matcher.supports_js_defer("Googlebot-Video/1.0", true));
        assert!(matcher.supports_js_defer("Mediapartners-Google", true));
        assert!(matcher.supports_js_defer("Googlebot-AdsBot/1.0", true));

        // This UA was extrapolated from the snippet when google-searching for
        // "what's my user agent" from Firefox.
        let google_bot_firefox_ua = format!(
            "Mozilla/5.0 {GOOGLEBOT_DESKTOP} Mozilla/5.0 (Windows NT 6.1; WOW64; rv:24.0) Gecko/20100101 Firefox/24.0"
        );
        assert!(matcher.supports_js_defer(&google_bot_firefox_ua, true));
    }

    #[test]
    fn webp_capable_lacking_accept_header() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.legacy_webp(TESTING_WEBP));
        assert!(!matcher.legacy_webp(TESTING_WEBP_LOSSLESS_ALPHA));

        assert!(matcher.legacy_webp(ANDROID_ICS_USER_AGENT));
        assert!(!matcher.legacy_webp(CHROME12_USER_AGENT));
        assert!(!matcher.legacy_webp(CHROME18_USER_AGENT));
        assert!(!matcher.legacy_webp(OPERA1110_USER_AGENT));
        assert!(!matcher.legacy_webp(I_PAD_CHROME29_USER_AGENT));
        assert!(!matcher.legacy_webp(I_PAD_CHROME36_USER_AGENT));
        assert!(!matcher.legacy_webp(I_PHONE_CHROME36_USER_AGENT));
        assert!(!matcher.legacy_webp(OPERA_WITH_FIREFOX_USER_AGENT));
    }

    #[test]
    fn modern_firefox_webp_without_accept_header() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_webp_rewritten_urls(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:64.0) ",
            "Gecko/20100101 Firefox/64.0",
        )));
        assert!(matcher.supports_webp_rewritten_urls(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:65.0) ",
            "Gecko/20100101 Firefox/65.0",
        )));
        assert!(matcher.supports_webp_rewritten_urls(concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:92.0) ",
            "Gecko/20100101 Firefox/92.0",
        )));
    }

    #[test]
    fn doesnt_support_webp() {
        let matcher = UserAgentMatcher::new();
        // The most interesting tests here are the recent but slightly older versions
        // of Chrome and Opera that can't display webp.
        assert!(!matcher.legacy_webp(ANDROID_HC_USER_AGENT));
        assert!(!matcher.legacy_webp(CHROME_USER_AGENT));
        assert!(!matcher.legacy_webp(CHROME9_USER_AGENT));
        assert!(!matcher.legacy_webp(CHROME15_USER_AGENT));
        assert!(!matcher.legacy_webp(OPERA1101_USER_AGENT));
        assert!(!matcher.legacy_webp(FIREFOX_USER_AGENT));
        assert!(!matcher.legacy_webp(FIREFOX1_USER_AGENT));
        assert!(!matcher.legacy_webp(FIREFOX42_ANDROID_USER_AGENT));
        assert!(!matcher.legacy_webp(IE6_USER_AGENT));
        assert!(!matcher.legacy_webp(IE7_USER_AGENT));
        assert!(!matcher.legacy_webp(IE8_USER_AGENT));
        assert!(!matcher.legacy_webp(IE9_USER_AGENT));
        for ua in IE11_USER_AGENTS {
            assert!(!matcher.legacy_webp(ua), "{ua}");
        }
        assert!(!matcher.legacy_webp(I_PHONE_USER_AGENT));
        assert!(!matcher.legacy_webp(NOKIA_USER_AGENT));
        assert!(!matcher.legacy_webp(OPERA5_USER_AGENT));
        assert!(!matcher.legacy_webp(OPERA8_USER_AGENT));
        assert!(!matcher.legacy_webp(PSP_USER_AGENT));
        assert!(!matcher.legacy_webp(SAFARI_USER_AGENT));
        assert!(!matcher.legacy_webp(I_PHONE_CHROME21_USER_AGENT));
        assert!(!matcher.legacy_webp(I_PAD_CHROME28_USER_AGENT));
        assert!(!matcher.legacy_webp(WINDOWS_PHONE_USER_AGENT));
    }

    #[test]
    fn is_android_user_agent_test() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.is_android_user_agent(ANDROID_HC_USER_AGENT));
        assert!(!matcher.is_android_user_agent(IE6_USER_AGENT));
    }

    #[test]
    fn is_ios_user_agent_test() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.is_ios_user_agent(I_PHONE_USER_AGENT));
        assert!(matcher.is_ios_user_agent(I_PAD_USER_AGENT));
        assert!(matcher.is_ios_user_agent(I_POD_SAFARI));
        assert!(matcher.is_ios_user_agent(I_PHONE_CHROME21_USER_AGENT));
        assert!(matcher.is_ios_user_agent(I_PAD_CHROME28_USER_AGENT));
        assert!(matcher.is_ios_user_agent(I_PAD_CHROME29_USER_AGENT));
        assert!(matcher.is_ios_user_agent(I_PAD_CHROME36_USER_AGENT));
        assert!(matcher.is_ios_user_agent(I_PHONE_CHROME36_USER_AGENT));
        assert!(!matcher.is_ios_user_agent(IE6_USER_AGENT));
    }

    #[test]
    fn chrome_build_number_test() {
        let matcher = UserAgentMatcher::new();
        assert_eq!(
            Some((9, 0, 597, 19)),
            matcher.chrome_build_number(CHROME9_USER_AGENT)
        );

        // On iOS it's "CriOS", not "Chrome".
        assert_eq!(
            Some((21, 0, 1180, 82)),
            matcher.chrome_build_number(I_PHONE_CHROME21_USER_AGENT)
        );

        assert_eq!(None, matcher.chrome_build_number(ANDROID_HC_USER_AGENT));
        assert_eq!(None, matcher.chrome_build_number(CHROME_USER_AGENT));
        assert_eq!(None, matcher.chrome_build_number("Chrome/10.0"));
        assert_eq!(None, matcher.chrome_build_number("Chrome/10.0.1."));
    }

    #[test]
    fn exceeds_chrome_build_and_patch_test() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.user_agent_exceeds_chrome_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1000,
            0
        ));
        assert!(matcher.user_agent_exceeds_chrome_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1000,
            999
        ));
        assert!(matcher.user_agent_exceeds_chrome_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1180,
            82
        ));
        assert!(!matcher.user_agent_exceeds_chrome_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1180,
            83
        ));
        assert!(!matcher.user_agent_exceeds_chrome_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1181,
            0
        ));
        assert!(!matcher.user_agent_exceeds_chrome_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1181,
            83
        ));

        assert!(matcher.user_agent_exceeds_chrome_android_build_and_patch(
            ANDROID_CHROME21_USER_AGENT,
            1000,
            0
        ));
        assert!(!matcher.user_agent_exceeds_chrome_android_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1000,
            0
        ));

        assert!(matcher.user_agent_exceeds_chrome_ios_build_and_patch(
            I_PHONE_CHROME21_USER_AGENT,
            1000,
            0
        ));
        assert!(!matcher.user_agent_exceeds_chrome_ios_build_and_patch(
            ANDROID_CHROME21_USER_AGENT,
            1000,
            0
        ));
    }

    #[test]
    fn supports_dns_prefetch() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.supports_dns_prefetch(CHROME_USER_AGENT));
        assert!(matcher.supports_dns_prefetch(IE9_USER_AGENT));
        assert!(matcher.supports_dns_prefetch(FIREFOX5_USER_AGENT));
        assert!(matcher.supports_dns_prefetch(SAFARI6_USER_AGENT));
        assert!(matcher.supports_dns_prefetch(SAFARI9_USER_AGENT));
        for ua in IE11_USER_AGENTS {
            assert!(matcher.supports_dns_prefetch(ua), "{ua}");
        }
    }

    #[test]
    fn doesnt_support_dns_prefetch() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_dns_prefetch(FIREFOX1_USER_AGENT));
        assert!(!matcher.supports_dns_prefetch(IE6_USER_AGENT));
        assert!(!matcher.supports_dns_prefetch(IE7_USER_AGENT));
        assert!(!matcher.supports_dns_prefetch(IE8_USER_AGENT));
        assert!(!matcher.supports_dns_prefetch(SAFARI_USER_AGENT));
    }

    #[test]
    fn supports_webp_lossless_alpha() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.supports_webp_lossless_alpha(TESTING_WEBP_LOSSLESS_ALPHA));
        assert!(matcher.supports_webp_lossless_alpha(I_PAD_CHROME29_USER_AGENT));
        assert!(matcher.supports_webp_lossless_alpha(I_PAD_CHROME36_USER_AGENT));
        assert!(matcher.supports_webp_lossless_alpha(I_PHONE_CHROME36_USER_AGENT));
        assert!(matcher.supports_webp_lossless_alpha(NEXUS10_CHROME_USER_AGENT));
        assert!(matcher.supports_webp_lossless_alpha(XT907_USER_AGENT));
        assert!(matcher.supports_webp_lossless_alpha(PAGESPEED_INSIGHTS_MOBILE_USER_AGENT));
        assert!(matcher.supports_webp_lossless_alpha(PAGESPEED_INSIGHTS_DESKTOP_USER_AGENT));
    }

    #[test]
    fn doesnt_support_webp_lossless_alpha() {
        let matcher = UserAgentMatcher::new();
        // The most interesting tests here are the recent but slightly older versions
        // of Chrome and Opera that can't display webp.
        assert!(!matcher.supports_webp_lossless_alpha(TESTING_WEBP));
        assert!(!matcher.supports_webp_lossless_alpha(ANDROID_ICS_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(CHROME12_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(CHROME18_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(OPERA1110_USER_AGENT));

        assert!(!matcher.supports_webp_lossless_alpha(ANDROID_HC_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(CHROME_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(CHROME9_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(CHROME15_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(OPERA1101_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(FIREFOX_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(FIREFOX1_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(IE6_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(IE7_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(IE8_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(IE9_USER_AGENT));
        for ua in IE11_USER_AGENTS {
            assert!(!matcher.supports_webp_lossless_alpha(ua), "{ua}");
        }
        assert!(!matcher.supports_webp_lossless_alpha(I_PHONE_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(NOKIA_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(OPERA5_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(OPERA8_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(PSP_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(SAFARI_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(I_PAD_CHROME28_USER_AGENT));
        assert!(!matcher.supports_webp_lossless_alpha(WINDOWS_PHONE_USER_AGENT));
    }

    #[test]
    fn supports_dns_prefetch_using_rel_prefetch() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_dns_prefetch_using_rel_prefetch(IE6_USER_AGENT));
        assert!(!matcher.supports_dns_prefetch_using_rel_prefetch(IE7_USER_AGENT));
        assert!(!matcher.supports_dns_prefetch_using_rel_prefetch(IE8_USER_AGENT));
        assert!(matcher.supports_dns_prefetch_using_rel_prefetch(IE9_USER_AGENT));
    }

    #[test]
    fn get_device_type_for_ua() {
        verify_get_device_type_for_ua();
    }

    #[test]
    fn ie11_no_defer_js() {
        let matcher = UserAgentMatcher::new();
        for ua in IE11_USER_AGENTS {
            assert!(!matcher.supports_js_defer(ua, true), "{ua}");
        }
    }

    #[test]
    fn mobilization() {
        verify_mobilization_support();
    }

    #[test]
    fn supports_animated_webp() {
        let matcher = UserAgentMatcher::new();
        assert!(matcher.supports_webp_animated(TESTING_WEBP_ANIMATED));

        assert!(matcher.supports_webp_animated(CHROME32_USER_AGENT));
        assert!(matcher.supports_webp_animated(CRI_OS32_USER_AGENT));
        assert!(matcher.supports_webp_animated(OPERA19_USER_AGENT));
        assert!(matcher.supports_webp_animated(CHROME37_USER_AGENT));
    }

    #[test]
    fn doesnt_support_animated_webp() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_webp_animated(CHROME31_USER_AGENT));
        assert!(!matcher.supports_webp_animated(CRI_OS31_USER_AGENT));
        assert!(!matcher.supports_webp_animated(OPERA18_USER_AGENT));
        assert!(!matcher.supports_webp_animated(CHROME18_USER_AGENT));
        assert!(!matcher.supports_webp_animated(OPERA1110_USER_AGENT));
        assert!(!matcher.supports_webp_animated(ANDROID_ICS_USER_AGENT));
        assert!(!matcher.supports_webp_animated(FIREFOX_USER_AGENT));
        assert!(!matcher.supports_webp_animated(IE10_USER_AGENT));
        assert!(!matcher.supports_webp_animated(I_PHONE4_SAFARI));
        assert!(!matcher.supports_webp_animated(WINDOWS_PHONE_USER_AGENT));
        assert!(!matcher.supports_webp_animated(PAGESPEED_INSIGHTS_MOBILE_USER_AGENT));
        assert!(!matcher.supports_webp_animated(PAGESPEED_INSIGHTS_DESKTOP_USER_AGENT));
    }

    #[test]
    fn modern_chromium_build_number_parsing() {
        let matcher = UserAgentMatcher::new();
        let edge_user_agent = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/123.0.6312.86 Safari/537.36 ",
            "Edg/123.0.2420.81",
        );
        let opera_user_agent = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/122.0.6261.95 Safari/537.36 ",
            "OPR/108.0.0.0",
        );
        let edge_ios_user_agent = concat!(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_3 like Mac OS X) ",
            "AppleWebKit/605.1.15 (KHTML, like Gecko) EdgiOS/122.0.2365.92 ",
            "Version/17.0 Mobile/15E148 Safari/604.1",
        );

        assert_eq!(
            Some((123, 0, 2420, 81)),
            matcher.chrome_build_number(edge_user_agent)
        );
        assert_eq!(
            Some((108, 0, 0, 0)),
            matcher.chrome_build_number(opera_user_agent)
        );
        assert_eq!(
            Some((122, 0, 2365, 92)),
            matcher.chrome_build_number(edge_ios_user_agent)
        );
    }

    #[test]
    fn modern_mobile_and_tablet_detection() {
        let matcher = UserAgentMatcher::new();
        let pixel_mobile = concat!(
            "Mozilla/5.0 (Linux; Android 14; Pixel 7) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/123.0.6312.99 Mobile Safari/537.36",
        );
        let galaxy_tablet = concat!(
            "Mozilla/5.0 (Linux; Android 13; SM-T870) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/123.0.6312.99 Safari/537.36",
        );
        let desktop_edge = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/123.0.6312.86 Safari/537.36 ",
            "Edg/123.0.2420.81",
        );

        assert_eq!(DeviceType::Mobile, matcher.device_type_for_ua(pixel_mobile));
        assert_eq!(
            DeviceType::Tablet,
            matcher.device_type_for_ua(galaxy_tablet)
        );
        assert_eq!(
            DeviceType::Desktop,
            matcher.device_type_for_ua(desktop_edge)
        );
    }

    #[test]
    fn supports_avif_rewritten_urls() {
        let matcher = UserAgentMatcher::new();
        let chrome123 = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/123.0.6312.86 Safari/537.36",
        );
        let edge123 = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/123.0.6312.86 Safari/537.36 ",
            "Edg/123.0.2420.81",
        );
        let firefox123 = "Mozilla/5.0 (X11; Linux x86_64; rv:123.0) Gecko/20100101 Firefox/123.0";
        let safari16 = concat!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_5) AppleWebKit/605.1.15 ",
            "(KHTML, like Gecko) Version/16.6 Safari/605.1.15",
        );

        assert!(matcher.supports_avif_rewritten_urls(chrome123));
        assert!(matcher.supports_avif_rewritten_urls(edge123));
        assert!(matcher.supports_avif_rewritten_urls(firefox123));
        assert!(matcher.supports_avif_rewritten_urls(safari16));
    }

    #[test]
    fn avif_browser_version_boundaries() {
        let matcher = UserAgentMatcher::new();
        assert!(!matcher.supports_avif_rewritten_urls("Mozilla/5.0 Chrome/84.0.0.0 Safari/537.36"));
        assert!(matcher.supports_avif_rewritten_urls("Mozilla/5.0 Chrome/85.0.0.0 Safari/537.36"));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/84.0.0.0 Android 10 Mobile Safari/537.36"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/85.0.0.0 Android 10 Mobile Safari/537.36"
        ));

        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/121.0.0.0 Safari/537.36 Edg/121.0.0.0"
        ));
        assert!(!matcher.supports_avif_rewritten_urls("Mozilla/5.0 Firefox/92.0"));
        assert!(matcher.supports_avif_rewritten_urls("Mozilla/5.0 Firefox/93.0"));

        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0) Version/16.3 Safari/605.1"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 13_0) Version/16.4 Safari/605.1"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 15_8 like Mac OS X) Version/15.8 ",
            "Mobile/15E148 Safari/604.1",
        )));
        assert!(matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) Version/16.0 ",
            "Mobile/15E148 Safari/604.1",
        )));

        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 SamsungBrowser/13.0"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 SamsungBrowser/14.0"
        ));
        assert!(!matcher.supports_avif_rewritten_urls("Opera Mini/50.0"));
        assert!(!matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (Windows NT 10.0) AppleWebKit/537.36 Chrome/120.0 ",
            "Safari/537.36 Edge/18.19041",
        )));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Linux; Android 4.4.4) Version/4.0 Safari/537.36"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Mobile; rv:3.0) Gecko/3.0 Firefox/3.0 KaiOS/3.0"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 QQBrowser/14.9 Chrome/120.0.0.0 Safari/537.36"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 EdgA/120.0.0.0"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/121.0.0.0 Safari/537.36 EdgA/121.0.0.0"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 Chrome/120.0.0.0 ",
            "Mobile Safari/537.36 SamsungBrowser/13.0",
        )));
        assert!(matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 Chrome/120.0.0.0 ",
            "Mobile Safari/537.36 SamsungBrowser/14.0",
        )));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 OPR/70.0.0.0"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 Chrome/120.0.0.0 Safari/537.36 OPR/71.0.0.0"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Linux; Android 10) UCBrowser/15.4.0.0 Chrome/120.0"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Linux; Android 10) UCBrowser/15.5.0.0 Chrome/120.0"
        ));
        assert!(!matcher
            .supports_avif_rewritten_urls("Mozilla/5.0 BaiduBrowser/13.51 Chrome/120.0.0.0"));
        assert!(
            matcher.supports_avif_rewritten_urls("Mozilla/5.0 BaiduBrowser/13.52 Chrome/120.0.0.0")
        );
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Windows NT 10.0; Trident/7.0; rv:11.0) like Gecko"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 15_8 like Mac OS X) EdgiOS/120.0"
        ));
        assert!(matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) EdgiOS/120.0"
        ));
        assert!(
            !matcher.supports_avif_rewritten_urls("Opera/9.80 (Android; Opera Mobi/12.1; U; en)")
        );
        assert!(
            matcher.supports_avif_rewritten_urls("Opera/9.80 (Android; Opera Mobi/80.0; U; en)")
        );
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Mobile; rv:3.0) Gecko/3.0 Firefox/3.0 KaiOS/2.5"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Linux; U; Android 2.1) Version/4.0 Mobile Safari/530.17"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Linux; U; Android 4.4.4) Version/4.0 Mobile Safari/537.36"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(
            "Mozilla/5.0 (Mobile; rv:3.0) Gecko/3.0 Firefox/3.0 KaiOS/3.0"
        ));
        assert!(!matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 (KHTML, like Gecko) ",
            "Version/4.0 Chrome/84.0.0.0 Mobile Safari/537.36 wv",
        )));
        assert!(matcher.supports_avif_rewritten_urls(concat!(
            "Mozilla/5.0 (Linux; Android 10) AppleWebKit/537.36 (KHTML, like Gecko) ",
            "Version/4.0 Chrome/85.0.0.0 Mobile Safari/537.36 wv",
        )));
    }

    #[test]
    fn doesnt_support_avif_rewritten_urls() {
        let matcher = UserAgentMatcher::new();
        let chrome84 = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/84.0.4147.135 Safari/537.36",
        );
        let edge84 = concat!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
            "(KHTML, like Gecko) Chrome/84.0.4147.135 Safari/537.36 ",
            "Edg/84.0.522.63",
        );
        let firefox92 = "Mozilla/5.0 (X11; Linux x86_64; rv:92.0) Gecko/20100101 Firefox/92.0";
        let safari15 = concat!(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 ",
            "(KHTML, like Gecko) Version/15.6 Safari/605.1.15",
        );

        assert!(!matcher.supports_avif_rewritten_urls(chrome84));
        assert!(!matcher.supports_avif_rewritten_urls(edge84));
        assert!(!matcher.supports_avif_rewritten_urls(firefox92));
        assert!(!matcher.supports_avif_rewritten_urls(safari15));
    }
}
