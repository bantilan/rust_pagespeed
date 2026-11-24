//! User-agent normalization (`UserAgentNormalizer`).
//!
//! Collapses UA variety that does not affect rendering (device
//! model noise, installed .NET versions) so UA-keyed cache entries
//! stay few.

use regex::Regex;

/// Normalizes user agents (`UserAgentNormalizer`).
pub trait UserAgentNormalizer: Send + Sync {
    /// Normalizes one UA string (`Normalize`).
    fn normalize(&self, ua: &str) -> String;
}

/// Normalizes with every normalizer in order (`NormalizeWithAll`).
#[must_use]
pub fn normalize_with_all(
    normalizers: &[std::rc::Rc<dyn UserAgentNormalizer>],
    ua: &str,
) -> String {
    let mut out = ua.to_string();
    for normalizer in normalizers {
        out = normalizer.normalize(&out);
    }
    out
}

/// Strips Android device-model noise (`AndroidUserAgentNormalizer`).
#[derive(Clone)]
pub struct AndroidUserAgentNormalizer {
    dalvik_ua: Regex,
    chrome_android_ua: Regex,
    android_browser_ua: Regex,
}

impl AndroidUserAgentNormalizer {
    /// Builds the normalizer (patterns must compile).
    #[must_use]
    pub fn new() -> Self {
        static NORMALIZER: std::sync::OnceLock<AndroidUserAgentNormalizer> =
            std::sync::OnceLock::new();
        NORMALIZER.get_or_init(Self::build).clone()
    }

    fn build() -> Self {
        AndroidUserAgentNormalizer {
            dalvik_ua: Regex::new(
                r"\A(Dalvik/[0-9.]+ \(Linux; U; Android [^ \t\n\f\r;]+)[ \t\n\f\r;][^)]+\)\z",
            )
            .expect("dalvik pattern"),
            chrome_android_ua: Regex::new(
                r"\A(Mozilla/5\.0 \(Linux; Android [0-9.]+; )[^)]+(\) AppleWebKit/[0-9.]+ \(KHTML, like Gecko\) )(?:Version/[0-9.]+ )?(Chrome/[0-9.]+(?: Mobile)?[ ]+Safari/[0-9.]+)\z",
            )
            .expect("chrome android pattern"),
            android_browser_ua: Regex::new(
                r"\A(Mozilla/5\.0 \(Linux;(?: U;)? Android [0-9.]+; )[^)]+(\) AppleWebKit/[0-9.+]+ \(KHTML, like Gecko\) Version/[0-9.]+(?: Mobile)? Safari/[0-9.]+)\z",
            )
            .expect("android browser pattern"),
        }
    }
}

impl Default for AndroidUserAgentNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl UserAgentNormalizer for AndroidUserAgentNormalizer {
    fn normalize(&self, ua: &str) -> String {
        if let Some(caps) = self.dalvik_ua.captures(ua) {
            return format!("{})", &caps[1]);
        }
        if let Some(caps) = self.chrome_android_ua.captures(ua) {
            return format!("{}{}{}", &caps[1], &caps[2], &caps[3]);
        }
        if let Some(caps) = self.android_browser_ua.captures(ua) {
            return format!("{}{}", &caps[1], &caps[2]);
        }
        ua.to_string()
    }
}

/// Strips IE add-on noise (`IEUserAgentNormalizer`).
#[derive(Clone)]
pub struct IEUserAgentNormalizer {
    ie_ua: Regex,
}

impl IEUserAgentNormalizer {
    /// Builds the normalizer (pattern must compile).
    #[must_use]
    pub fn new() -> Self {
        static NORMALIZER: std::sync::OnceLock<IEUserAgentNormalizer> = std::sync::OnceLock::new();
        NORMALIZER.get_or_init(Self::build).clone()
    }

    fn build() -> Self {
        IEUserAgentNormalizer {
            ie_ua: Regex::new(r"\A(Mozilla/[0-9].0 \(compatible; MSIE [0-9.]+)([^)]+)\)\z")
                .expect("ie pattern"),
        }
    }
}

impl Default for IEUserAgentNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl UserAgentNormalizer for IEUserAgentNormalizer {
    fn normalize(&self, ua: &str) -> String {
        let Some(caps) = self.ie_ua.captures(ua) else {
            return ua.to_string();
        };
        // IE UA strings enumerate things like installed .NET versions
        // which blow up their variety. We keep only parts that talk
        // about the renderer or platform.
        let mut out = caps[1].to_string();
        for fragment in caps[2].split("; ") {
            if fragment.starts_with("Trident")
                || fragment.starts_with("Windows ")
                || fragment.starts_with("WOW64 ")
                || fragment.starts_with("chromeframe")
                || fragment.starts_with("IEMobile")
                || fragment.starts_with("Media Center PC")
            {
                out.push_str("; ");
                out.push_str(fragment);
            }
        }
        out.push(')');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHROME: &str = "Mozilla/5.0 (Windows NT 6.1; WOW64) AppleWebKit/537.31 \
        (KHTML, like Gecko) Chrome/26.0.1410.64 Safari/537.31";

    #[test]
    fn ie() {
        let normalize_ie = IEUserAgentNormalizer::new();
        // Shouldn't affect a totally different UA
        assert_eq!(CHROME, normalize_ie.normalize(CHROME));

        // Various plugins get stripped
        assert_eq!(
            "Mozilla/4.0 (compatible; MSIE 8.0; Windows NT 5.1; Trident/4.0)",
            normalize_ie.normalize(
                "Mozilla/4.0 (compatible; MSIE 8.0; \
                Windows NT 5.1; Trident/4.0; .NET CLR 2.0.50727; \
                OfficeLiveConnector.1.3; OfficeLivePatch.0.0)"
            )
        );

        // We do keep 'chromeframe', though, as it can mean a different renderer
        // might be in used.
        assert_eq!(
            "Mozilla/4.0 (compatible; MSIE 6.0; Windows NT 5.1; chromeframe/26.0.1410.64)",
            normalize_ie.normalize(
                "Mozilla/4.0 (compatible; MSIE 6.0; Windows NT 5.1; SV1; chromeframe/26.0.1410.64)"
            )
        );

        // Make sure Windows Phone doesn't get bundled in, too, since it may
        // be useful to tell it apart. Also makes sure we do handle Mozilla/5.0
        // strings variants as well.
        assert_eq!(
            "Mozilla/5.0 (compatible; MSIE 9.0; Windows Phone OS 7.5; Trident/5.0; IEMobile/9.0)",
            normalize_ie.normalize(
                "Mozilla/5.0 (compatible; MSIE 9.0; Windows Phone OS 7.5; \
                Trident/5.0; IEMobile/9.0; NOKIA; Lumia 800)"
            )
        );
    }

    #[test]
    fn android_chrome_unaffected() {
        let normalize_android = AndroidUserAgentNormalizer::new();
        // Shouldn't affect a totally different UA
        assert_eq!(CHROME, normalize_android.normalize(CHROME));
    }

    #[test]
    fn dalvik() {
        let normalize_android = AndroidUserAgentNormalizer::new();
        // Lots of fetches from android seem to be identified as being from the VM
        // rather than the browser.
        assert_eq!(
            "Dalvik/1.4.0 (Linux; U; Android 2.3.4)",
            normalize_android
                .normalize("Dalvik/1.4.0 (Linux; U; Android 2.3.4; GT-N7000 Build/GRJ22)")
        );

        assert_eq!(
            "Dalvik/1.6.0 (Linux; U; Android 4.2.2)",
            normalize_android
                .normalize("Dalvik/1.6.0 (Linux; U; Android 4.2.2; Nexus 7 Build/JDQ39)")
        );

        assert_eq!(
            "Dalvik/1.4.0 (Linux; U; Android 2.3.4)",
            normalize_android.normalize(
                "Dalvik/1.4.0 (Linux; U; Android 2.3.4; \
                BlueStacks-00000000-0000-0000-0000-000000000000 Build/GRJ22)"
            )
        );
    }

    #[test]
    fn browser() {
        let normalize_android = AndroidUserAgentNormalizer::new();
        // Test for how we normalize the pre-Chrome Android Browser
        assert_eq!(
            "Mozilla/5.0 (Linux; U; Android 2.2; ) AppleWebKit/533.1 (KHTML, like Gecko) \
            Version/4.0 Mobile Safari/533.1",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; U; Android 2.2; en-us; Desire_A8181 Build/FRF91) \
                AppleWebKit/533.1 (KHTML, like Gecko) Version/4.0 Mobile Safari/533.1"
            )
        );

        assert_eq!(
            "Mozilla/5.0 (Linux; U; Android 4.1.2; ) AppleWebKit/534.30 (KHTML, like Gecko) \
            Version/4.0 Mobile Safari/534.30",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; U; Android 4.1.2; en-gb; GT-I9300 Build/JZO54K) \
                AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Mobile Safari/534.30"
            )
        );

        assert_eq!(
            "Mozilla/5.0 (Linux; U; Android 1.5; ) AppleWebKit/528.5+ \
            (KHTML, like Gecko) Version/3.1.2 Mobile Safari/525.20.1",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; U; Android 1.5; en-us) AppleWebKit/528.5+ \
                (KHTML, like Gecko) Version/3.1.2 Mobile Safari/525.20.1"
            )
        );

        // Tablet UA string as well --- it lacks 'Mobile'.
        assert_eq!(
            "Mozilla/5.0 (Linux; U; Android 4.0.4; ) AppleWebKit/534.30 (KHTML, like Gecko) \
            Version/4.0 Safari/534.30",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; U; Android 4.0.4; en-gb; GT-N8000 Build/IMM76D) \
                AppleWebKit/534.30 (KHTML, like Gecko) Version/4.0 Safari/534.30"
            )
        );
    }

    #[test]
    fn mobile_chrome() {
        let normalize_android = AndroidUserAgentNormalizer::new();
        // Normalization of Android Chrome UA strings. (Desktop Chrome UA strings
        // don't need it)
        assert_eq!(
            "Mozilla/5.0 (Linux; Android 4.1.2; ) AppleWebKit/537.31 (KHTML, like Gecko) \
            Chrome/26.0.1410.58 Mobile Safari/537.31",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; Android 4.1.2; GT-I9300 Build/JZO54K) \
                AppleWebKit/537.31 (KHTML, like Gecko) Chrome/26.0.1410.58 Mobile Safari/537.31"
            )
        );

        assert_eq!(
            "Mozilla/5.0 (Linux; Android 4.2.2; ) AppleWebKit/537.31 (KHTML, like Gecko) \
            Chrome/26.0.1410.58 Safari/537.31",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; Android 4.2.2; Nexus 7 Build/JDQ39) \
                AppleWebKit/537.31 (KHTML, like Gecko) Chrome/26.0.1410.58 Safari/537.31"
            )
        );

        // Some Samsung devices also have an extra Version/1.0 thrown in for
        // some reason.
        assert_eq!(
            "Mozilla/5.0 (Linux; Android 4.2.2; ) AppleWebKit/535.19 (KHTML, like Gecko) \
            Chrome/18.0.1025.308 Mobile Safari/535.19",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; Android 4.2.2; en-au; SAMSUNG GT-I9505 Build/JDQ39) \
                AppleWebKit/535.19 (KHTML, like Gecko) Version/1.0 \
                Chrome/18.0.1025.308 Mobile Safari/535.19"
            )
        );

        // For some reason Chrome 18 on tablets had a doubled space
        // between Chrome/ and Safari (perhaps in place of 'Mobile'), make sure
        // we properly drop the device/build name on these, too.
        assert_eq!(
            "Mozilla/5.0 (Linux; Android 4.2.2; ) AppleWebKit/535.19 (KHTML, like Gecko) \
            Chrome/18.0.1025.166  Safari/535.19",
            normalize_android.normalize(
                "Mozilla/5.0 (Linux; Android 4.2.2; Nexus 7 Build/JDQ39) \
                AppleWebKit/535.19 (KHTML, like Gecko) Chrome/18.0.1025.166  Safari/535.19"
            )
        );
    }

    #[test]
    fn with_all() {
        use std::rc::Rc;
        let normalizers: Vec<Rc<dyn UserAgentNormalizer>> = vec![
            Rc::new(IEUserAgentNormalizer::new()),
            Rc::new(AndroidUserAgentNormalizer::new()),
        ];

        assert_eq!(
            "Mozilla/5.0 (Linux; Android 4.1.2; ) AppleWebKit/537.31 (KHTML, like Gecko) \
            Chrome/26.0.1410.58 Mobile Safari/537.31",
            normalize_with_all(
                &normalizers,
                "Mozilla/5.0 (Linux; Android 4.1.2; GT-I9300 Build/JZO54K) \
                AppleWebKit/537.31 (KHTML, like Gecko) Chrome/26.0.1410.58 Mobile Safari/537.31"
            )
        );

        assert_eq!(
            "Mozilla/4.0 (compatible; MSIE 8.0; Windows NT 5.1; Trident/4.0)",
            normalize_with_all(
                &normalizers,
                "Mozilla/4.0 (compatible; MSIE 8.0; \
                Windows NT 5.1; Trident/4.0; .NET CLR 2.0.50727; \
                OfficeLiveConnector.1.3; OfficeLivePatch.0.0)"
            )
        );
    }
}
