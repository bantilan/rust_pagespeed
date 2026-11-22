//! File-load policy: which URLs load straight from disk.
//!
//! Ports `FileLoadPolicy` (`file_load_policy.cc`): `LoadFromFile`
//! literal prefix mappings and `LoadFromFileMatch` regexp mappings
//! (youngest declaration wins across both kinds), gated by
//! `LoadFromFileRule[Match]` allow/disallow rules (youngest match
//! wins, default allow). Matched URLs map to a filename the caller
//! reads directly instead of fetching over HTTP.
//!
//! Matching semantics mirror C++ exactly: mappings see the URL
//! without query or ref; `%20` becomes a space after substitution
//! (URLs keep it encoded); slashes collapse before rules see the
//! path; regexp results with `.`/`..` segments are rejected
//! (backreferences can reintroduce what URL canonicalization
//! removed). Literal mappings need no dot check: both URL parsers
//! canonicalize dot segments away (including `%2e` spellings), so a
//! canonical URL cannot smuggle one through a prefix mapping.
//!
//! Regexps use RE2-compatible syntax via the `regex` crate (already
//! in the workspace for `ps-http`); rewrites spell backreferences
//! C++-style (`\1`) and are translated once at config time. Patterns
//! compile at config time, never per request. Windows UNC handling
//! from C++ is omitted: this port targets Linux nginx only.
//!
//! The content-type gate (`ShouldLoadFromFile`'s static-resource
//! check) lives with the caller, which owns content types
//! (`ps-http` depends on this crate, so the policy returns the
//! mapped filename and the caller judges its extension).
#![forbid(unsafe_code)]

use regex::Regex;

/// Literal URL-prefix → directory mapping (`FileLoadMappingLiteral`,
/// prefixes slash-terminated at registration).
#[derive(Debug, Clone)]
pub struct FileLoadMapping {
    url_prefix: String,
    filename_prefix: String,
}

/// Regexp URL mapping with backreference rewrite
/// (`FileLoadMappingRegexp`, pattern `^`-anchored at registration).
#[derive(Debug, Clone)]
pub struct FileLoadMappingRegexp {
    /// Pattern as configured (display and signatures).
    pattern: String,
    /// Compiled pattern.
    regex: Regex,
    /// Rewrite translated to `$N` form.
    rewrite: String,
}

/// One URL mapping, literal or regexp, in declaration order.
#[derive(Debug, Clone)]
pub enum FileLoadMappingKind {
    /// Literal prefix mapping.
    Literal(FileLoadMapping),
    /// Regexp mapping.
    Regexp(FileLoadMappingRegexp),
}

/// Allow/disallow rule matcher (`FileLoadRule*`).
#[derive(Debug, Clone)]
enum FileLoadRuleMatcher {
    /// Filename-prefix match (slashes collapsed at registration).
    Literal(String),
    /// Unanchored regexp match over the collapsed filename.
    Regexp {
        /// Pattern as configured (display and signatures).
        pattern: String,
        /// Compiled pattern.
        regex: Regex,
    },
}

/// One `LoadFromFileRule[Match]` entry.
#[derive(Debug, Clone)]
pub struct LoadRule {
    /// Whether the rule allows (vs disallows) loading.
    pub allow: bool,
    matcher: FileLoadRuleMatcher,
}

/// Decides which URLs load from which files (`FileLoadPolicy`).
#[derive(Debug, Clone, Default)]
pub struct FileLoadPolicy {
    mappings: Vec<FileLoadMappingKind>,
    rules: Vec<LoadRule>,
}

impl FileLoadPolicy {
    /// Empty policy (maps nothing).
    #[must_use]
    pub fn new() -> Self {
        FileLoadPolicy::default()
    }

    /// Whether the policy holds no mappings or rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty() && self.rules.is_empty()
    }

    /// Maps a URL prefix onto a directory (`Associate`): both sides
    /// gain a terminal slash when missing, so associations always
    /// cover directories. Later associations win over earlier ones.
    pub fn associate(&mut self, url_prefix: &str, filename_prefix: &str) {
        let mut url = url_prefix.to_string();
        let mut filename = filename_prefix.to_string();
        ensure_ends_in_slash(&mut url);
        ensure_ends_in_slash(&mut filename);
        self.mappings.push(FileLoadMappingKind::Literal(FileLoadMapping {
            url_prefix: url,
            filename_prefix: filename,
        }));
    }

    /// Maps URLs by regexp with backreference rewrite
    /// (`AssociateRegexp`, e.g. `^https?://example.com/~([^/]*)/static/`
    /// → `/var/static/\1/`). Fails without mutating unless the
    /// pattern is `^`-anchored, compiles, and every `\N` in the
    /// rewrite names a captured group.
    pub fn associate_regexp(
        &mut self,
        url_regexp: &str,
        filename_prefix: &str,
    ) -> Result<(), String> {
        if !url_regexp.starts_with('^') {
            return Err("File mapping regular expression must match beginning \
                        of string. (Must start with '^'.)"
                .to_string());
        }
        let regex =
            Regex::new(url_regexp).map_err(|error| format!("Invalid pattern: {error}"))?;
        let rewrite = translate_rewrite(filename_prefix, regex.captures_len())?;
        self.mappings
            .push(FileLoadMappingKind::Regexp(FileLoadMappingRegexp {
                pattern: url_regexp.to_string(),
                regex,
                rewrite,
            }));
        Ok(())
    }

    /// Adds an allow/disallow rule (`AddRule`): literal prefixes
    /// match by prefix, regexps match unanchored; patterns see
    /// slash-collapsed filenames, so they are collapsed the same
    /// way. Fails without mutating on uncompilable regexps.
    pub fn add_rule(
        &mut self,
        rule: &str,
        is_regexp: bool,
        allow: bool,
    ) -> Result<(), String> {
        if is_regexp {
            let mut pattern = rule.to_string();
            collapse_slashes(&mut pattern);
            let regex =
                Regex::new(&pattern).map_err(|error| format!("Invalid pattern: {error}"))?;
            self.rules.push(LoadRule {
                allow,
                matcher: FileLoadRuleMatcher::Regexp { pattern, regex },
            });
        } else {
            let mut prefix = rule.to_string();
            collapse_slashes(&mut prefix);
            self.rules.push(LoadRule {
                allow,
                matcher: FileLoadRuleMatcher::Literal(prefix),
            });
        }
        Ok(())
    }

    /// Maps a URL (without query or ref) to a filename
    /// (`ShouldLoadFromFileHelper`): youngest matching mapping wins,
    /// youngest matching rule decides, default allow. Returns `None`
    /// when nothing maps.
    #[must_use]
    pub fn map_url_to_filename(&self, url: &str) -> Option<String> {
        if url.is_empty() {
            return None;
        }
        for mapping in self.mappings.iter().rev() {
            let substituted = match mapping {
                FileLoadMappingKind::Literal(mapping) => substitute_literal(mapping, url),
                FileLoadMappingKind::Regexp(mapping) => substitute_regexp(mapping, url),
            };
            let Some(mut filename) = substituted else {
                continue;
            };
            // URLs keep "%20" encoded; the filesystem sees a space.
            if filename.contains("%20") {
                filename = filename.replace("%20", " ");
            }
            // Rules see the path the filesystem will see.
            collapse_slashes(&mut filename);
            for rule in self.rules.iter().rev() {
                if let Some(allow) = rule.classify(&filename) {
                    return allow.then_some(filename);
                }
            }
            return Some(filename);
        }
        None
    }

    /// Merges another policy's mappings and rules (`Merge`): the
    /// merged entries are younger, so they win on overlap.
    pub fn append_from(&mut self, other: &FileLoadPolicy) {
        self.mappings.extend(other.mappings.iter().cloned());
        self.rules.extend(other.rules.iter().cloned());
    }

    /// Canonical signature fragment (declaration order): mapping and
    /// rule changes shift cache keys.
    #[must_use]
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for mapping in &self.mappings {
            match mapping {
                FileLoadMappingKind::Literal(mapping) => {
                    out.push_str(&mapping.url_prefix);
                    out.push('=');
                    out.push_str(&mapping.filename_prefix);
                    out.push(';');
                }
                FileLoadMappingKind::Regexp(mapping) => {
                    out.push_str(&mapping.pattern);
                    out.push_str("=>");
                    // The translated rewrite is a pure function of the
                    // configured text, so either form keys identically.
                    out.push_str(&mapping.rewrite);
                    out.push(';');
                }
            }
        }
        out.push('|');
        for rule in &self.rules {
            out.push(if rule.allow { 'A' } else { 'D' });
            match &rule.matcher {
                FileLoadRuleMatcher::Literal(prefix) => out.push_str(prefix),
                FileLoadRuleMatcher::Regexp { pattern, .. } => out.push_str(pattern),
            }
            out.push(';');
        }
        out
    }
}

impl LoadRule {
    /// Whether the rule matches, and its verdict (`Classify`).
    fn classify(&self, filename: &str) -> Option<bool> {
        let matched = match &self.matcher {
            FileLoadRuleMatcher::Literal(prefix) => filename.starts_with(prefix),
            FileLoadRuleMatcher::Regexp { regex, .. } => regex.is_match(filename),
        };
        matched.then_some(self.allow)
    }
}

/// Literal substitution: prefix swap, if the URL carries the prefix.
fn substitute_literal(mapping: &FileLoadMapping, url: &str) -> Option<String> {
    url.strip_prefix(&mapping.url_prefix).map(|suffix| {
        let mut filename = String::with_capacity(mapping.filename_prefix.len() + suffix.len());
        filename.push_str(&mapping.filename_prefix);
        filename.push_str(suffix);
        filename
    })
}

/// Regexp substitution: first-match rewrite, rejected when the
/// result carries a `.`/`..` segment.
fn substitute_regexp(mapping: &FileLoadMappingRegexp, url: &str) -> Option<String> {
    let replaced = mapping.regex.replace(url, mapping.rewrite.as_str());
    match replaced {
        std::borrow::Cow::Borrowed(_) => None,
        std::borrow::Cow::Owned(filename) => {
            if has_dot_segment(&filename) {
                return None;
            }
            Some(filename)
        }
    }
}

/// Whether any `/`-separated segment is `.` or `..`.
fn has_dot_segment(path: &str) -> bool {
    path.split('/').any(|segment| segment == "." || segment == "..")
}

/// Collapses repeated slashes in place.
fn collapse_slashes(text: &mut String) {
    if !text.contains("//") {
        return;
    }
    let mut collapsed = String::with_capacity(text.len());
    let mut previous_slash = false;
    for byte in text.bytes() {
        if byte == b'/' {
            if previous_slash {
                continue;
            }
            previous_slash = true;
        } else {
            previous_slash = false;
        }
        collapsed.push(byte as char);
    }
    *text = collapsed;
}

/// Appends a terminal slash unless present.
fn ensure_ends_in_slash(text: &mut String) {
    if !text.ends_with('/') {
        text.push('/');
    }
}

/// Translates a C++-style rewrite (`\1` backreferences) into the
/// crate's `$1` form, validating group numbers against
/// `group_count` (including group 0). `$` escapes to `$$`; anything
/// else after a backslash, or a dangling backslash, fails.
fn translate_rewrite(rewrite: &str, group_count: usize) -> Result<String, String> {
    let mut out = String::with_capacity(rewrite.len());
    let mut chars = rewrite.chars().peekable();
    while let Some(char) = chars.next() {
        if char == '$' {
            out.push_str("$$");
        } else if char != '\\' {
            out.push(char);
        } else {
            match chars.peek() {
                None => return Err("Rewrite ends with a dangling backslash.".to_string()),
                Some('\\') => {
                    chars.next();
                    out.push('\\');
                }
                Some(digit) if digit.is_ascii_digit() => {
                    let mut number = String::new();
                    while let Some(digit) = chars.peek() {
                        if !digit.is_ascii_digit() {
                            break;
                        }
                        number.push(*digit);
                        chars.next();
                    }
                    let group: usize = number
                        .parse()
                        .map_err(|_| format!("Bad backreference \\{number}."))?;
                    if group >= group_count {
                        return Err(format!(
                            "Backreference \\{group} names no captured group."
                        ));
                    }
                    out.push('$');
                    out.push_str(&number);
                }
                Some(other) => {
                    return Err(format!("Bad escape \\{other} in rewrite."));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps a URL string through the policy (C++ `LoadFromFile`
    /// shape, without the content-type gate, which lives with the
    /// caller).
    fn map(policy: &FileLoadPolicy, url: &str) -> Option<String> {
        policy.map_url_to_filename(url)
    }

    #[test]
    fn empty_policy_maps_nothing() {
        let policy = FileLoadPolicy::new();
        assert!(policy.is_empty());
        for url in [
            "http://www.example.com/static/foo.png",
            "http://www.example.com/static/bar/",
            "http://www.example.com/static/some/more/dirs/b.css",
            "http://www.example.com/static/foo.png",
            "http://www.example.com/images/another.gif",
            "http://www.other-site.com/foo.png",
            "",
        ] {
            assert_eq!(map(&policy, url), None, "{url}");
        }
    }

    #[test]
    fn one_prefix_vectors() {
        let mut policy = FileLoadPolicy::new();
        policy.associate("http://www.example.com/static/", "/example/1/");
        assert!(!policy.is_empty());
        assert_eq!(
            map(&policy, "http://www.example.com/static/foo.png"),
            Some("/example/1/foo.png".to_string())
        );
        // Directories map too (the content-type gate, not the
        // mapping, rejects extensionless names).
        assert_eq!(
            map(&policy, "http://www.example.com/static/bar/"),
            Some("/example/1/bar/".to_string())
        );
        assert_eq!(
            map(&policy, "http://www.example.com/static/some/more/dirs/b.css"),
            Some("/example/1/some/more/dirs/b.css".to_string())
        );
        // Query strings never reach matching (callers pass the
        // query-free URL).
        assert_eq!(
            map(&policy, "http://www.example.com/static/foo.png"),
            Some("/example/1/foo.png".to_string())
        );
        // "%20" becomes a space; "%2F" stays encoded.
        assert_eq!(
            map(&policy, "http://www.example.com/static/foo%20bar.png"),
            Some("/example/1/foo bar.png".to_string())
        );
        assert_eq!(
            map(&policy, "http://www.example.com/static/foo%2Fbar.png"),
            Some("/example/1/foo%2Fbar.png".to_string())
        );
        for url in [
            "http://www.example.com/images/another.gif",
            "http://www.some-site.com/with/many/dirs/a/b.js",
            "http://www.other-site.com/foo.png",
            "http://www.example.com/foo.png",
            // Canonical URLs cannot smuggle dot segments through a
            // prefix mapping (both parsers resolve them away).
            "http://www.example.com/foo.png",
        ] {
            assert_eq!(map(&policy, url), None, "{url}");
        }
    }

    #[test]
    fn prefixes_gain_terminal_slashes() {
        let mut policy = FileLoadPolicy::new();
        policy.associate("http://www.example.com/images/", "/example/images/static");
        policy.associate(
            "http://www.some-site.com/with/many/dirs",
            "/var/www/some-site.com/",
        );
        assert_eq!(
            map(&policy, "http://www.example.com/images/another.gif"),
            Some("/example/images/static/another.gif".to_string())
        );
        assert_eq!(
            map(&policy, "http://www.some-site.com/with/many/dirs/a/b.js"),
            Some("/var/www/some-site.com/a/b.js".to_string())
        );
        // Without the added slash, "dirsX/..." would have matched.
        assert_eq!(
            map(&policy, "http://www.some-site.com/with/many/dirsX/a.js"),
            None
        );
    }

    #[test]
    fn overlapping_prefixes_youngest_wins() {
        let mut policy = FileLoadPolicy::new();
        policy.associate("http://www.example.com/static/", "/1/");
        policy.associate("http://www.example.com/", "/2/");
        policy.associate("http://www.example.com/static/sub/dir/", "/3/");
        assert_eq!(
            map(&policy, "http://www.example.com/foo.png"),
            Some("/2/foo.png".to_string())
        );
        assert_eq!(
            map(&policy, "http://www.example.com/static/foo.png"),
            Some("/2/static/foo.png".to_string())
        );
        assert_eq!(
            map(&policy, "http://www.example.com/static/sub/dir/foo.png"),
            Some("/3/foo.png".to_string())
        );
        assert_eq!(
            map(&policy, "http://www.example.com/static/sub/dir/plus/foo.png"),
            Some("/3/plus/foo.png".to_string())
        );
    }

    #[test]
    fn regexp_backreferences() {
        let mut policy = FileLoadPolicy::new();
        policy
            .associate_regexp(
                "^https?://example.com/~([^/]*)/static/",
                "/var/static/\\1/",
            )
            .unwrap();
        assert_eq!(
            map(&policy, "http://example.com/~pat/static/cat.jpg"),
            Some("/var/static/pat/cat.jpg".to_string())
        );
        assert_eq!(
            map(&policy, "http://example.com/~sam/static/dog.jpg"),
            Some("/var/static/sam/dog.jpg".to_string())
        );
        assert_eq!(
            map(&policy, "https://example.com/~al/static/ie.css"),
            Some("/var/static/al/ie.css".to_string())
        );
        // Group 0 (whole match) rewrites too.
        let mut whole = FileLoadPolicy::new();
        whole
            .associate_regexp("^http://example.com/(.*)", "/mirror/\\0")
            .unwrap();
        assert_eq!(
            map(&whole, "http://example.com/a/b.js"),
            // Slashes collapse after substitution, like C++.
            Some("/mirror/http:/example.com/a/b.js".to_string())
        );
    }

    #[test]
    fn regexp_dot_segments_fall_through() {
        let mut policy = FileLoadPolicy::new();
        policy.associate("http://example.com/", "/var/www/");
        policy
            .associate_regexp(
                "^https?://example.com/~([^/]*)/static/",
                "/var/static/\\1/",
            )
            .unwrap();
        policy
            .associate_regexp("^https?://example.com/-([^/]*)/static/", "/var/static/\\1")
            .unwrap();
        policy
            .associate_regexp("^https?://([^/]*)/vhost/", "/var/vhosts/\\1/")
            .unwrap();
        // Dotted regexp results fall through to the whole-site mapping.
        assert_eq!(
            map(&policy, "http://example.com/~../static/cat.jpg"),
            Some("/var/www/~../static/cat.jpg".to_string())
        );
        assert_eq!(
            map(&policy, "http://example.com/~./static/cat.jpg"),
            Some("/var/www/~./static/cat.jpg".to_string())
        );
        assert_eq!(
            map(&policy, "http://example.com/-../static//cat.jpg"),
            Some("/var/www/-../static/cat.jpg".to_string())
        );
        assert_eq!(map(&policy, "https://example.com/~../static/cat.jpg"), None);
        // Dots short of a whole segment are fine.
        assert_eq!(
            map(&policy, "http://example.com/~..pat/static/cat.jpg"),
            Some("/var/static/..pat/cat.jpg".to_string())
        );
        assert_eq!(
            map(&policy, "http://example.com/~.../static/cat.jpg"),
            Some("/var/static/.../cat.jpg".to_string())
        );
        assert_eq!(
            map(&policy, "http://example.com/-../static/cat.jpg"),
            Some("/var/static/..cat.jpg".to_string())
        );
    }

    #[test]
    fn regexp_registration_validates() {
        let mut policy = FileLoadPolicy::new();
        // Unanchored patterns reject.
        assert!(policy
            .associate_regexp("http://example.com/[^/]*/static", "/var/static/")
            .is_err());
        // Excess backreferences reject.
        assert!(policy
            .associate_regexp(
                "^http://([^/]*).com/([^/]*)/static",
                "/var/\\1/\\2/\\3/static/"
            )
            .is_err());
        // Uncompilable patterns reject.
        assert!(policy.associate_regexp("^http://([/", "/var/").is_err());
        // Bad rewrite escapes reject.
        assert!(policy
            .associate_regexp("^http://example.com/", "/var/\\x/")
            .is_err());
        assert!(policy
            .associate_regexp("^http://example.com/", "/var/dir\\")
            .is_err());
        // Nothing stuck.
        assert!(policy.is_empty());
        // Literal dollars survive translation untouched.
        let mut dollars = FileLoadPolicy::new();
        dollars
            .associate_regexp("^http://example.com/(.*)", "/var/$price/\\1")
            .unwrap();
        assert_eq!(
            map(&dollars, "http://example.com/a.js"),
            Some("/var/$price/a.js".to_string())
        );
        // Escaped backslashes collapse to one.
        let mut slashes = FileLoadPolicy::new();
        slashes
            .associate_regexp("^http://example.com/(.*)", "/var\\\\\\1")
            .unwrap();
        assert_eq!(
            map(&slashes, "http://example.com/a.js"),
            Some("/var\\a.js".to_string())
        );
    }

    #[test]
    fn rules_youngest_match_wins() {
        let mut policy = FileLoadPolicy::new();
        policy.associate("http://example.com/", "/www/");
        assert_eq!(
            map(&policy, "http://example.com/cgi-bin/guestbook.pl.js"),
            Some("/www/cgi-bin/guestbook.pl.js".to_string())
        );
        policy.add_rule("/www/cgi-bin/", false, false).unwrap();
        assert_eq!(
            map(&policy, "http://example.com/cgi-bin/guestbook.pl.js"),
            None
        );
        policy.add_rule("\\.js$", true, true).unwrap();
        assert_eq!(
            map(&policy, "http://example.com/cgi-bin/guestbook.js"),
            Some("/www/cgi-bin/guestbook.js".to_string())
        );
        policy.add_rule("\\.ssi.js$", true, false).unwrap();
        assert_eq!(
            map(&policy, "http://example.com/cgi-bin/guestbook.ssi.js"),
            None
        );
        policy
            .add_rule("/www/cgi-bin/allow", false, true)
            .unwrap();
        assert_eq!(
            map(&policy, "http://example.com/cgi-bin/allow.ssi.js"),
            Some("/www/cgi-bin/allow.ssi.js".to_string())
        );
        // Bad regexp rules reject without sticking.
        assert!(policy.add_rule("([/", true, false).is_err());
        assert_eq!(
            map(&policy, "http://example.com/cgi-bin/allow.ssi.js"),
            Some("/www/cgi-bin/allow.ssi.js".to_string())
        );
    }

    #[test]
    fn rules_see_collapsed_names() {
        let mut policy = FileLoadPolicy::new();
        policy.associate("http://example.com/", "/www/");
        policy.add_rule("/www/cgi-bin/", false, false).unwrap();
        policy.add_rule("^/www/private/", true, false).unwrap();
        for url in [
            "http://example.com/cgi-bin/a.js",
            "http://example.com//cgi-bin/a.js",
            "http://example.com////cgi-bin//a.js",
            "http://example.com/private/a.js",
            "http://example.com//private/a.js",
            "http://example.com///private/a.js",
        ] {
            assert_eq!(map(&policy, url), None, "{url}");
        }
        assert_eq!(
            map(&policy, "http://example.com//js///a.js"),
            Some("/www/js/a.js".to_string())
        );
        // Doubled slashes in configuration collapse the same way.
        let mut doubled = FileLoadPolicy::new();
        doubled.associate("http://example.com/", "/var//www/");
        doubled
            .add_rule("/var//www/cgi-bin/", false, false)
            .unwrap();
        assert_eq!(map(&doubled, "http://example.com/cgi-bin/a.js"), None);
        assert_eq!(
            map(&doubled, "http://example.com/js/a.js"),
            Some("/var/www/js/a.js".to_string())
        );
        let mut doubled_regexp = FileLoadPolicy::new();
        doubled_regexp.associate("http://example.com/", "/var//www/");
        doubled_regexp
            .add_rule("^/var//www/private/", true, false)
            .unwrap();
        assert_eq!(
            map(&doubled_regexp, "http://example.com/private/a.js"),
            None
        );
        assert_eq!(
            map(&doubled_regexp, "http://example.com/js/a.js"),
            Some("/var/www/js/a.js".to_string())
        );
    }

    #[test]
    fn merge_appends_youngest() {
        let mut first = FileLoadPolicy::new();
        first.associate("http://www.example.com/1/", "/1/");
        first
            .associate_regexp("^http://www\\.example\\.com/([^/]*)/", "/\\1/a/")
            .unwrap();
        first.add_rule("/5/", false, false).unwrap();
        first.add_rule("\\.jpg$", true, false).unwrap();
        assert_eq!(
            map(&first, "http://www.example.com/1/foo.png"),
            Some("/1/a/foo.png".to_string())
        );
        assert_eq!(
            map(&first, "http://www.example.com/2/foo.png"),
            Some("/2/a/foo.png".to_string())
        );
        assert_eq!(map(&first, "http://www.example.com/3/foo.jpg"), None);
        assert_eq!(map(&first, "http://www.example.com/5/foo.png"), None);
        let mut second = FileLoadPolicy::new();
        second.associate("http://www.example.com/3/", "/3/");
        second.associate("http://www.example.com/4/", "/4/");
        second.add_rule("exception\\.jpg$", true, true).unwrap();
        first.append_from(&second);
        // Merged mappings win on overlap ...
        assert_eq!(
            map(&first, "http://www.example.com/3/foo.png"),
            Some("/3/foo.png".to_string())
        );
        assert_eq!(
            map(&first, "http://www.example.com/4/foo.png"),
            Some("/4/foo.png".to_string())
        );
        // ... and merged rules win too.
        assert_eq!(map(&first, "http://www.example.com/5/foo.png"), None);
        assert_eq!(map(&first, "http://www.example.com/4/foo.jpg"), None);
        // Unknown extensions map fine here (via the merged
        // literal, youngest); the caller's content-type gate
        // rejects them.
        assert_eq!(
            map(&first, "http://www.example.com/4/foo.notjpg"),
            Some("/4/foo.notjpg".to_string())
        );
        assert_eq!(
            map(&first, "http://www.example.com/4/exception.jpg"),
            Some("/4/exception.jpg".to_string())
        );
        assert_eq!(
            map(&first, "http://www.example.com/4/anexception.jpg"),
            Some("/4/anexception.jpg".to_string())
        );
        assert_eq!(
            map(&first, "http://www.example.com/5/exception.jpg"),
            Some("/5/a/exception.jpg".to_string())
        );
        // The donor is unchanged.
        assert_eq!(
            map(&second, "http://www.example.com/3/foo.png"),
            Some("/3/foo.png".to_string())
        );
        // Signatures track every input.
        assert_ne!(FileLoadPolicy::new().signature(), first.signature());
        assert_eq!(first.signature(), first.clone().signature());
    }
}
