//! URL parsing/resolution (`GoogleUrl`) over the WHATWG `url` crate.
//!
//! Covers the rewrite surface (parse, resolve, web-validity, leaf
//! extraction); the facade grows method-by-method as consumers land.
//!
//! GURL-vs-WHATWG deviations: canonicalization engines differ, so exotic
//! inputs (drive letters, backslashes on non-special schemes, unusual
//! auth/port forms, IDNA versions) may resolve differently than Chromium
//! GURL; the Step 8 compat corpus arbitrates real-world impact. The leaf
//! rule replicates GURL explicitly (unreserved escapes decode, survivors
//! uppercase) so allowlist comparisons keep their fail-closed shape, and
//! invalid specs yield empty strings instead of logging DFATALs.
//!
//! Two forms are kept: `spec()` preserves input bytes verbatim (GURL keeps
//! bytes like `'` that WHATWG would encode), while structural methods
//! slice the canonical parse.

/// Absolute or resolved URL; invalid when parsing failed. The spec keeps
/// the input bytes verbatim (GURL preserves bytes WHATWG would encode,
/// notably `'` in queries); the parsed form drives structure and validity.
/// Canonicalization differences on exotic inputs are documented at the top.
#[derive(Debug, Clone, Default)]
pub struct GoogleUrl {
    url: Option<url::Url>,
    spec: String,
}

/// URL identity compares canonical specs (GURL `operator==`); two
/// invalid URLs compare unequal.
impl PartialEq for GoogleUrl {
    fn eq(&self, other: &Self) -> bool {
        match (&self.url, &other.url) {
            (Some(left), Some(right)) => left.as_str() == right.as_str(),
            _ => false,
        }
    }
}

impl GoogleUrl {
    /// Parses an absolute URL.
    #[must_use]
    pub fn new(spec: &str) -> Self {
        match url::Url::parse(spec) {
            Ok(url) => GoogleUrl {
                url: Some(url),
                spec: spec.to_string(),
            },
            Err(_) => GoogleUrl {
                url: None,
                spec: String::new(),
            },
        }
    }

    /// Percent-decodes escapes, leaving `+` alone
    /// (`UnescapeIgnorePlus`): malformed `%` sequences pass
    /// through verbatim.
    #[must_use]
    pub fn unescape_ignore_plus(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%'
                && index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit()
            {
                let hex = |byte: u8| (byte as char).to_digit(16).unwrap_or(0) as u8;
                out.push(hex(bytes[index + 1]) << 4 | hex(bytes[index + 2]));
                index += 3;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        out
    }

    /// Resolves a reference against a base (`GoogleUrl(base, ref)`).
    #[must_use]
    pub fn resolve(base: &GoogleUrl, reference: &str) -> Self {
        match base.url.as_ref().and_then(|root| root.join(reference).ok()) {
            Some(url) => {
                let canonical = url.as_str().to_string();
                GoogleUrl {
                    url: Some(url),
                    spec: canonical,
                }
            }
            None => GoogleUrl {
                url: None,
                spec: String::new(),
            },
        }
    }

    /// Whether parsing succeeded.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.url.is_some()
    }

    /// Fetchable web URL (`IsWebValid`): parsed, http(s), host present.
    #[must_use]
    pub fn is_web_valid(&self) -> bool {
        match &self.url {
            Some(url) => matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            None => false,
        }
    }

    /// Canonical spec (`Spec`; empty when invalid).
    #[must_use]
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// Last path segment minus query/fragment (`LeafSansQuery`), with
    /// GURL's path rule applied: percent-escapes of unreserved bytes
    /// decode, remaining escapes uppercase. Empty when invalid or
    /// pathless.
    #[must_use]
    pub fn leaf_sans_query(&self) -> String {
        let Some(url) = &self.url else {
            return String::new();
        };
        let Some(leaf) = url.path_segments().and_then(|mut parts| parts.next_back()) else {
            return String::new();
        };
        decode_unreserved(leaf)
    }

    /// Canonical parsed form, for structural slicing.
    fn canonical(&self) -> &str {
        self.url.as_ref().map_or("", url::Url::as_str)
    }

    /// Byte offsets `(path_start, leaf_end)` in the canonical spec; `None`
    /// when invalid or pathless. `leaf_end` is the path end
    /// (query/fragment excluded).
    fn path_bounds(&self) -> Option<(usize, usize)> {
        let url = self.url.as_ref()?;
        // Opaque paths (data:, javascript:, ...) have no leaves.
        if url.cannot_be_a_base() {
            return None;
        }
        let spec = url.as_str();
        let path = url.path();
        if path.is_empty() {
            return None;
        }
        // The path is the run ending at the first '?'/'#' (or spec end).
        let path_end = spec.find(['?', '#']).unwrap_or(spec.len());
        if !spec[..path_end].ends_with(path) {
            return None;
        }
        let path_start = path_end - path.len();
        Some((path_start, path_start + path.len()))
    }

    /// Offset one past the last path slash (`LeafStartPosition` + 1).
    fn after_last_slash(&self) -> Option<usize> {
        let (path_start, leaf_end) = self.path_bounds()?;
        let spec = self.canonical();
        let slash = spec[..leaf_end.min(spec.len())].rfind('/')?;
        let after = slash + 1;
        if after < path_start {
            return None;
        }
        Some(after)
    }

    /// Query without `?` (`Query`; empty when absent).
    #[must_use]
    pub fn query(&self) -> &str {
        self.url.as_ref().and_then(url::Url::query).unwrap_or("")
    }

    /// Whether a query is present (`has_query`), even an empty one.
    #[must_use]
    pub fn has_query(&self) -> bool {
        self.url.as_ref().is_some_and(|url| url.query().is_some())
    }

    /// Web or `data:` validity (`IsWebOrDataValid`).
    #[must_use]
    pub fn is_web_or_data_valid(&self) -> bool {
        self.is_web_valid() || (self.url.is_some() && self.scheme() == "data")
    }

    /// Lowercase scheme (`Scheme`; empty when invalid or schemeless).
    #[must_use]
    pub fn scheme(&self) -> &str {
        self.url.as_ref().map(|url| url.scheme()).unwrap_or("")
    }

    /// Lowercase host without port or brackets (`Host`; empty when absent).
    #[must_use]
    pub fn host(&self) -> String {
        match self.url.as_ref().and_then(|url| url.host_str()) {
            Some(host) => host
                .strip_prefix('[')
                .and_then(|inner| inner.strip_suffix(']'))
                .unwrap_or(host)
                .to_string(),
            None => String::new(),
        }
    }

    /// Host plus explicit port (`HostAndPort`; empty without a host).
    /// Default ports never appear (both engines strip them), matching
    /// the C++ slice arithmetic.
    #[must_use]
    pub fn host_and_port(&self) -> String {
        match self
            .url
            .as_ref()
            .and_then(|url| url.host_str().map(|host| (host, url.port())))
        {
            Some((host, Some(port))) => format!("{host}:{port}"),
            Some((host, None)) => host.to_string(),
            None => String::new(),
        }
    }

    /// Explicit port, or `PORT_UNSPECIFIED` when absent or default
    /// for the scheme (`IntPort`).
    #[must_use]
    pub fn int_port(&self) -> i32 {
        self.url
            .as_ref()
            .and_then(url::Url::port)
            .map_or(PORT_UNSPECIFIED, |port| i32::from(port))
    }

    /// Effective port, defaulting per scheme (`EffectiveIntPort`).
    #[must_use]
    pub fn effective_int_port(&self) -> i32 {
        self.url
            .as_ref()
            .and_then(url::Url::port_or_known_default)
            .map_or(PORT_UNSPECIFIED, |port| i32::from(port))
    }

    /// Canonical parsed spec (default ports stripped, like GURL).
    #[must_use]
    pub fn canonical_spec(&self) -> &str {
        self.canonical()
    }

    /// Scheme plus authority without trailing slash (`Origin`), as a
    /// Escapes non-URL characters (`Sanitize`): alphanumerics,
    /// `.~_-%`, and reserved chars pass through; the rest become
    /// `%XX`.
    #[must_use]
    pub fn sanitize(url: &str) -> String {
        const RESERVED: &str = ":/?#[]@!$&'()*+,;=";
        let mut escaped = String::new();
        for byte in url.bytes() {
            let c = byte as char;
            if c.is_ascii_alphanumeric()
                || matches!(c, '.' | '~' | '_' | '-' | '%')
                || RESERVED.bytes().any(|reserved| reserved == byte)
            {
                escaped.push(c);
            } else {
                escaped.push_str(&format!("%{byte:02X}"));
            }
        }
        escaped
    }

    /// byte-prefix of [`spec`](Self::spec) so trimmers can index into
    /// the spec by its length. Empty when invalid. Unlike the
    /// structural slices above this scans the stored spec (verbatim
    /// for [`new`](Self::new)), so non-canonical inputs compare
    /// fail-closed instead of trimming.
    #[must_use]
    pub fn origin(&self) -> &str {
        if self.url.is_none() {
            return "";
        }
        let Some(after_scheme) = self.spec.find("://").map(|index| index + 3) else {
            return "";
        };
        match self.spec[after_scheme..].find('/') {
            Some(relative) => &self.spec[..after_scheme + relative],
            None => self.spec.as_str(),
        }
    }

    /// Spec through the path end (`AllExceptQuery`; drops query and
    /// fragment alike).
    #[must_use]
    pub fn all_except_query(&self) -> &str {
        match self.path_bounds() {
            Some((_, leaf_end)) => &self.canonical()[..leaf_end.min(self.canonical().len())],
            None => "",
        }
    }

    /// Spec through the last path slash (`AllExceptLeaf`).
    #[must_use]
    pub fn all_except_leaf(&self) -> &str {
        match self.after_last_slash() {
            Some(after) => &self.canonical()[..after.min(self.canonical().len())],
            None => "",
        }
    }

    /// Leaf plus query and fragment (`LeafWithQuery`).
    #[must_use]
    pub fn leaf_with_query(&self) -> &str {
        match self.after_last_slash() {
            Some(after) => &self.canonical()[after.min(self.canonical().len())..],
            None => "",
        }
    }

    /// Path through the last slash (`PathSansLeaf`).
    #[must_use]
    pub fn path_sans_leaf(&self) -> &str {
        match (self.path_bounds(), self.after_last_slash()) {
            (Some((path_start, _)), Some(after)) => &self.canonical()[path_start..after],
            _ => "",
        }
    }

    /// Path with query and fragment (`PathAndLeaf`), leading slash
    /// included; empty when invalid or pathless.
    #[must_use]
    pub fn path_and_leaf(&self) -> &str {
        match self.path_bounds() {
            Some((path_start, _)) => &self.canonical()[path_start.min(self.canonical().len())..],
            None => "",
        }
    }

    /// Path without query or fragment (`PathSansQuery`); empty when
    /// invalid or pathless.
    #[must_use]
    pub fn path_sans_query(&self) -> &str {
        match self.url.as_ref() {
            Some(url) => url.path(),
            None => "",
        }
    }

    /// Copy with a query parameter appended (`CopyAndAddQueryParam`):
    /// existing pairs kept verbatim (split on `&`, first `=`), the escaped
    /// pair appended, fragment preserved.
    #[must_use]
    pub fn copy_and_add_query_param(&self, name: &str, value: &str) -> GoogleUrl {
        let invalid = || GoogleUrl {
            url: None,
            spec: String::new(),
        };
        if self.url.is_none() {
            return invalid();
        }
        // Operate on the stored spec so chained mutations never re-encode.
        let (head, fragment) = match self.spec.find('#') {
            Some(index) => self.spec.split_at(index),
            None => (self.spec.as_str(), ""),
        };
        let (stem, query) = match head.find('?') {
            Some(index) => head.split_at(index),
            None => (head, ""),
        };
        let query = query.strip_prefix('?').unwrap_or(query);
        let mut pairs: Vec<(String, Option<String>)> = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((name, value)) => (name.to_string(), Some(value.to_string())),
                None => (pair.to_string(), None),
            })
            .collect();
        pairs.push((escape_query_param(name), Some(escape_query_param(value))));
        let query = pairs
            .iter()
            .map(|(name, value)| match value {
                Some(value) => format!("{name}={value}"),
                None => name.clone(),
            })
            .collect::<Vec<_>>()
            .join("&");
        let spliced = format!("{stem}?{query}{fragment}");
        match url::Url::parse(&spliced) {
            Ok(url) => GoogleUrl {
                url: Some(url),
                spec: spliced,
            },
            Err(_) => invalid(),
        }
    }

    /// Scheme-relative form (`NetPath`): `//host/path...`.
    #[must_use]
    pub fn net_path(&self) -> String {
        match self.spec.find("://") {
            Some(index) => format!("//{}", &self.spec[index + 3..]),
            None => self.spec.clone(),
        }
    }

    /// How relative a URL string is (`FindRelativity`).
    #[must_use]
    pub fn find_relativity(url: &str) -> UrlRelativity {
        if GoogleUrl::new(url).is_valid() {
            UrlRelativity::AbsoluteUrl
        } else if url.starts_with("//") {
            UrlRelativity::NetPath
        } else if url.starts_with('/') {
            UrlRelativity::AbsolutePath
        } else {
            UrlRelativity::RelativePath
        }
    }

    /// Shortens this URL against a base (`Relativize`): the naive
    /// shortening stands only when it resolves back identically,
    /// else the absolute spec wins.
    #[must_use]
    pub fn relativize(&self, relativity: UrlRelativity, base: &GoogleUrl) -> String {
        let mut result = self.spec().to_string();
        match relativity {
            UrlRelativity::RelativePath => {
                let prefix = base.all_except_leaf();
                if let Some(stripped) = result.strip_prefix(prefix) {
                    result = stripped.to_string();
                }
            }
            UrlRelativity::AbsolutePath => {
                if self.origin() == base.origin() {
                    result = self.path_and_leaf().to_string();
                }
            }
            UrlRelativity::NetPath => {
                if self.scheme() == base.scheme() {
                    result = self.net_path();
                }
            }
            UrlRelativity::AbsoluteUrl => {}
        }
        if GoogleUrl::resolve(base, &result) != *self {
            result = self.spec().to_string();
        }
        result
    }
}

/// Unspecified port (`url::PORT_UNSPECIFIED`).
pub const PORT_UNSPECIFIED: i32 = -1;

/// URL relativity (`UrlRelativity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRelativity {
    /// `http://example.com/foo/bar/file.ext?k=v#f`.
    AbsoluteUrl,
    /// `//example.com/foo/bar/file.ext?k=v#f`.
    NetPath,
    /// `/foo/bar/file.ext?k=v#f`.
    AbsolutePath,
    /// `bar/file.ext?k=v#f`.
    RelativePath,
}

/// Percent-decodes unreserved bytes, uppercases surviving escapes.
fn decode_unreserved(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = if bytes[index] == b'%' && index + 2 < bytes.len() {
            match (hex_value(bytes[index + 1]), hex_value(bytes[index + 2])) {
                (Some(high), Some(low)) => Some(high * 16 + low),
                _ => None,
            }
        } else {
            None
        };
        match escape {
            Some(decoded) if is_unreserved(decoded) => {
                out.push(decoded);
                index += 3;
            }
            Some(_) => {
                out.extend_from_slice(&[
                    b'%',
                    bytes[index + 1].to_ascii_uppercase(),
                    bytes[index + 2].to_ascii_uppercase(),
                ]);
                index += 3;
            }
            None => {
                out.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escapes a query name/value (`EscapeQueryParam`): alphanumerics plus
/// `._~-` verbatim, space as `+`, everything else lowercase `%xx`.
fn escape_query_param(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'~' | b'_' | b'-') {
            out.push(byte as char);
        } else if byte == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02x}"));
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validity() {
        assert!(GoogleUrl::new("https://h/p/a.js").is_web_valid());
        assert!(GoogleUrl::new("http://h:8080/p").is_web_valid());
        assert!(!GoogleUrl::new("data:text/plain,hi").is_web_valid());
        assert!(!GoogleUrl::new("javascript:alert(1)").is_web_valid());
        assert!(!GoogleUrl::new("not a url").is_valid());
        assert!(!GoogleUrl::new("").is_valid());
        assert_eq!(GoogleUrl::new("bogus").spec(), "");
    }

    #[test]
    fn resolve_against_base() {
        let base = GoogleUrl::new("https://pagespeed.invalid/");
        assert_eq!(
            GoogleUrl::resolve(&base, "/polyfills.min.js").spec(),
            "https://pagespeed.invalid/polyfills.min.js"
        );
        // Absolute references replace the base.
        assert_eq!(
            GoogleUrl::resolve(&base, "https://polyfill.io/v3/polyfill.min.js?f=default").spec(),
            "https://polyfill.io/v3/polyfill.min.js?f=default"
        );
        // Query-only references keep the base path (no leaf).
        let query_only = GoogleUrl::resolve(&base, "?foo");
        assert!(query_only.is_web_valid());
        assert_eq!(query_only.leaf_sans_query(), "");
        // Invalid bases poison resolution.
        assert!(!GoogleUrl::resolve(&GoogleUrl::new("bogus"), "/x.js").is_valid());
    }

    #[test]
    fn decomposition() {
        let url = GoogleUrl::new("http://test.com/a/b/?p=3#frag");
        assert_eq!(url.path_and_leaf(), "/a/b/?p=3#frag");
        assert_eq!(url.query(), "p=3");
        assert_eq!(url.all_except_query(), "http://test.com/a/b/");
        assert_eq!(url.all_except_leaf(), "http://test.com/a/b/");
        assert_eq!(url.leaf_with_query(), "?p=3#frag");
        assert_eq!(url.path_sans_leaf(), "/a/b/");
        assert_eq!(url.leaf_sans_query(), "");

        let url = GoogleUrl::new("http://test.com/a.html");
        assert_eq!(url.query(), "");
        assert_eq!(url.all_except_query(), "http://test.com/a.html");
        assert_eq!(url.all_except_leaf(), "http://test.com/");
        assert_eq!(url.leaf_with_query(), "a.html");
        assert_eq!(url.path_sans_leaf(), "/");
        assert_eq!(url.leaf_sans_query(), "a.html");

        // Empty query still counts as present for the raw slices.
        let url = GoogleUrl::new("http://test.com/a?");
        assert_eq!(url.query(), "");
        assert_eq!(url.all_except_query(), "http://test.com/a");
        assert_eq!(url.leaf_with_query(), "a?");

        // A bare host still yields its root slash as the directory.
        assert_eq!(GoogleUrl::new("https://h").path_sans_leaf(), "/");
        assert_eq!(GoogleUrl::new("https://h").leaf_with_query(), "");
        // Opaque and invalid URLs yield empties.
        for spec in ["data:text/plain,hi", "bogus"] {
            let url = GoogleUrl::new(spec);
            assert_eq!(url.leaf_with_query(), "", "{spec}");
            assert_eq!(url.path_sans_leaf(), "", "{spec}");
            assert_eq!(url.all_except_query(), "", "{spec}");
            assert_eq!(url.all_except_leaf(), "", "{spec}");
        }
        assert_eq!(GoogleUrl::new("data:text/plain,hi").query(), "");
    }

    #[test]
    fn add_query_param() {
        let with_param = |spec: &str| {
            GoogleUrl::new(spec)
                .copy_and_add_query_param("PageSpeed", "off")
                .spec()
                .to_string()
        };
        assert_eq!(
            with_param("http://test.com/in.html"),
            "http://test.com/in.html?PageSpeed=off"
        );
        assert_eq!(
            with_param("http://test.com/in.html?a=b"),
            "http://test.com/in.html?a=b&PageSpeed=off"
        );
        assert_eq!(
            with_param("http://test.com/in.html?a=b#frag"),
            "http://test.com/in.html?a=b&PageSpeed=off#frag"
        );
        // Existing pairs survive verbatim, even unescaped ones.
        assert_eq!(
            with_param("http://test.com/in.html?'("),
            "http://test.com/in.html?'(&PageSpeed=off"
        );
        // Names and values escape; space becomes `+`.
        let escaped = GoogleUrl::new("http://h/p").copy_and_add_query_param("a b", "c&d=e f");
        assert_eq!(escaped.spec(), "http://h/p?a+b=c%26d%3de+f");
        // Invalid bases stay invalid.
        assert!(!GoogleUrl::new("bogus")
            .copy_and_add_query_param("PageSpeed", "off")
            .is_valid());
    }

    #[test]
    fn origin_and_query_presence() {
        let origin = |spec: &str| GoogleUrl::new(spec).origin().to_string();
        assert_eq!(origin("http://a.com/b/c/d?e=f/g"), "http://a.com");
        assert_eq!(origin("https://h:8443/p"), "https://h:8443");
        assert_eq!(origin("https://h"), "https://h");
        assert_eq!(origin("bogus"), "");
        assert_eq!(origin("data:text/plain,hi"), "");
        // Resolved URLs keep the origin a byte-prefix of the spec.
        let base = GoogleUrl::new("http://foo.bar/baz/");
        let resolved = GoogleUrl::resolve(&base, "//foo.bar/img/img1.jpg");
        assert!(resolved.spec().starts_with(resolved.origin()));
        assert_eq!(resolved.origin(), "http://foo.bar");

        assert!(GoogleUrl::new("http://h/?a=b").has_query());
        assert!(GoogleUrl::new("http://h/?").has_query());
        assert!(!GoogleUrl::new("http://h/").has_query());
        assert!(!GoogleUrl::new("bogus").has_query());

        assert_eq!(GoogleUrl::new("HTTP://H/A").scheme(), "http");
        assert_eq!(GoogleUrl::new("bogus").scheme(), "");
        assert_eq!(GoogleUrl::new("HTTP://Test.COM:8080/p").host(), "test.com");
        assert_eq!(GoogleUrl::new("http://[::1]/p").host(), "::1");
        assert_eq!(GoogleUrl::new("bogus").host(), "");
        assert_eq!(GoogleUrl::new("http://h:8080/p").host_and_port(), "h:8080");
        assert_eq!(GoogleUrl::new("https://h/p").host_and_port(), "h");
        // Default ports strip, so they never render.
        assert_eq!(GoogleUrl::new("http://h:80/p").host_and_port(), "h");
        assert_eq!(GoogleUrl::new("bogus").host_and_port(), "");

        // Identity compares canonical specs.
        assert_eq!(
            GoogleUrl::new("http://h/a"),
            GoogleUrl::resolve(&GoogleUrl::new("http://h/"), "a")
        );
        assert_ne!(GoogleUrl::new("http://h/a"), GoogleUrl::new("http://h/b"));
        assert_ne!(GoogleUrl::new("bogus"), GoogleUrl::new("bogus"));
    }

    #[test]
    fn find_relativity() {
        use super::UrlRelativity::*;
        assert_eq!(
            GoogleUrl::find_relativity("http://example.com/foo/bar/file.ext?k=v#f"),
            AbsoluteUrl
        );
        assert_eq!(
            GoogleUrl::find_relativity("//example.com/foo/bar/file.ext?k=v#f"),
            NetPath
        );
        assert_eq!(
            GoogleUrl::find_relativity("/foo/bar/file.ext?k=v#f"),
            AbsolutePath
        );
        assert_eq!(
            GoogleUrl::find_relativity("bar/file.ext?k=v#f"),
            RelativePath
        );
    }

    #[test]
    fn relativize() {
        use super::UrlRelativity::*;
        let url = GoogleUrl::new("http://example.com/foo/bar/file.ext?k=v#f");
        let good_base = GoogleUrl::new("http://example.com/foo/index.html");
        assert_eq!(
            url.relativize(AbsoluteUrl, &good_base),
            "http://example.com/foo/bar/file.ext?k=v#f"
        );
        assert_eq!(
            url.relativize(NetPath, &good_base),
            "//example.com/foo/bar/file.ext?k=v#f"
        );
        assert_eq!(
            url.relativize(AbsolutePath, &good_base),
            "/foo/bar/file.ext?k=v#f"
        );
        assert_eq!(
            url.relativize(RelativePath, &good_base),
            "bar/file.ext?k=v#f"
        );

        let bad_base = GoogleUrl::new("https://www.example.com/other/path.html");
        assert_eq!(
            url.relativize(AbsoluteUrl, &bad_base),
            "http://example.com/foo/bar/file.ext?k=v#f"
        );
        assert_eq!(
            url.relativize(NetPath, &bad_base),
            "http://example.com/foo/bar/file.ext?k=v#f"
        );
        assert_eq!(
            url.relativize(AbsolutePath, &bad_base),
            "http://example.com/foo/bar/file.ext?k=v#f"
        );
        assert_eq!(
            url.relativize(RelativePath, &bad_base),
            "http://example.com/foo/bar/file.ext?k=v#f"
        );

        let double_slash = GoogleUrl::new("http://example.com//index.html");
        let double_base = GoogleUrl::new("http://example.com/");
        assert_eq!(
            double_slash.relativize(AbsoluteUrl, &double_base),
            "http://example.com//index.html"
        );
        assert_eq!(
            double_slash.relativize(NetPath, &double_base),
            "//example.com//index.html"
        );
        // Cannot shorten to "//index.html": that reads as a net path.
        assert_eq!(
            double_slash.relativize(AbsolutePath, &double_base),
            "http://example.com//index.html"
        );
        // Cannot shorten to "/index.html": that reads as absolute.
        assert_eq!(
            double_slash.relativize(RelativePath, &double_base),
            "http://example.com//index.html"
        );

        let query_url = GoogleUrl::new("http://example.com/?bar");
        let query_base = GoogleUrl::new("http://example.com/foo.html");
        // Cannot shorten to "?bar": that would mean foo.html?bar.
        assert_eq!(
            query_url.relativize(RelativePath, &query_base),
            "http://example.com/?bar"
        );
    }

    #[test]
    fn leaf_extraction() {
        let leaf = |spec: &str| GoogleUrl::new(spec).leaf_sans_query();
        assert_eq!(leaf("https://h/p/a.js?x=1#f"), "a.js");
        assert_eq!(leaf("https://h/p/"), "");
        assert_eq!(leaf("https://h"), "");
        assert_eq!(leaf("bogus"), "");
        // Unreserved escapes decode (GURL rule): `a%41b` reads as `aAb`.
        assert_eq!(leaf("https://h/p/a%41b"), "aAb");
        // Reserved escapes survive, uppercased.
        assert_eq!(leaf("https://h/p/a%20b"), "a%20b");
        assert_eq!(leaf("https://h/p/a%2fb"), "a%2Fb");
        // Incomplete escapes pass through literally.
        assert_eq!(leaf("https://h/p/a%2"), "a%2");
        assert_eq!(leaf("https://h/p/a%zz"), "a%zz");
    }
}
