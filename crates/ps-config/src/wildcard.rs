//! Wildcard patterns and ordered allow/disallow groups.
//!
//! Ports `pagespeed/kernel/base/wildcard.{h,cc}` and the match ordering of
//! `wildcard_group.cc` (`WildcardGroup::Match`: the last-inserted matching
//! pattern wins; otherwise the caller-supplied default applies).
//!
//! Syntax: `*` matches any (possibly empty) run, `?` matches exactly one
//! byte. Matching is case-sensitive and byte-oriented, as in C++.
#![forbid(unsafe_code)]

/// A compiled wildcard pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wildcard {
    spec: String,
}

impl Wildcard {
    /// Matches-any marker (`*`).
    pub const MATCH_ANY: char = '*';
    /// Matches-one marker (`?`).
    pub const MATCH_ONE: char = '?';

    /// Compiles a pattern.
    #[must_use]
    pub fn new(spec: &str) -> Wildcard {
        Wildcard {
            spec: spec.to_string(),
        }
    }

    /// The original pattern text (`spec()`).
    #[must_use]
    pub fn spec(&self) -> &str {
        &self.spec
    }

    /// Whether the pattern has no wildcards (`IsSimple`).
    #[must_use]
    pub fn is_simple(&self) -> bool {
        !self.spec.contains(['*', '?'])
    }

    /// Tests a candidate (`Wildcard::Match`).
    ///
    /// This is a direct port of the C++ block algorithm: the pattern is
    /// viewed as literal/`?` blocks separated by `*`; first and last blocks
    /// anchor at the string ends, middle blocks match leftmost.
    #[must_use]
    pub fn matches(&self, actual: &str) -> bool {
        // Fast path for patterns without wildcards (`is_simple_`).
        if !self.spec.contains(['*', '?']) {
            return self.spec == actual;
        }
        let pat = self.spec.as_bytes();
        let text = actual.as_bytes();
        // Split into blocks separated by `*`, dropping empty middle blocks
        // (`**` behaves as `*`).
        let blocks: Vec<&[u8]> = pat
            .split(|c| *c == b'*')
            .filter(|b| !b.is_empty())
            .collect();
        if blocks.is_empty() {
            // Pattern is all `*`: matches everything.
            return true;
        }
        let leading_star = pat.first() == Some(&b'*');
        let trailing_star = pat.last() == Some(&b'*');
        // Minimum required length: every block char consumes one byte.
        let min_len: usize = blocks.iter().map(|b| b.len()).sum();
        if text.len() < min_len {
            return false;
        }
        // Single block: anchoring depends on the surrounding stars.
        if blocks.len() == 1 {
            let block = blocks[0];
            return match (leading_star, trailing_star) {
                (false, false) => text.len() == block.len() && match_block(block, text),
                (false, true) => match_block(block, &text[..block.len()]),
                (true, false) => match_block(block, &text[text.len() - block.len()..]),
                (true, true) => find_block(block, text, 0, text.len()).is_some(),
            };
        }
        let mut pos = 0usize;
        // First block anchors at the start unless a leading `*` frees it.
        let mut start_block = 0usize;
        if !leading_star {
            if !match_block(blocks[0], &text[..blocks[0].len()]) {
                return false;
            }
            pos = blocks[0].len();
            start_block = 1;
        }
        // Last block anchors at the end unless a trailing `*` frees it.
        let mut end_limit = text.len();
        let mut end_block = blocks.len();
        if !trailing_star {
            let last = blocks[blocks.len() - 1];
            let start = text.len() - last.len();
            if !match_block(last, &text[start..]) {
                return false;
            }
            end_limit = start;
            end_block -= 1;
        }
        // Middle blocks, leftmost match.
        for block in &blocks[start_block..end_block] {
            // Block must start with a literal for the memchr-style search in
            // C++; leading `?`s are handled by scanning every position.
            match find_block(block, text, pos, end_limit) {
                Some(found) => pos = found + block.len(),
                None => return false,
            }
        }
        // Remaining text must fit before the anchored tail.
        pos <= end_limit
    }
}

/// Matches one block (`?` = any byte) against a same-length slice.
fn match_block(block: &[u8], text: &[u8]) -> bool {
    block.len() == text.len()
        && block
            .iter()
            .zip(text.iter())
            .all(|(p, t)| *p == b'?' || p == t)
}

/// Leftmost position in `text[pos..limit]` where `block` matches.
fn find_block(block: &[u8], text: &[u8], pos: usize, limit: usize) -> Option<usize> {
    if block.is_empty() {
        return Some(pos);
    }
    let end = limit.saturating_sub(block.len());
    let mut i = pos;
    // C++ uses memchr on the first literal char; leading `?` forces a
    // positional scan. Scanning every position is equivalent.
    while i <= end && i + block.len() <= text.len() {
        if match_block(block, &text[i..i + block.len()]) {
            return Some(i);
        }
        // memchr-style skip: if the block starts with a literal, jump to
        // its next occurrence.
        if block[0] != b'?' {
            let skip = text[i + 1..end + 1].iter().position(|c| *c == block[0])?;
            i += 1 + skip;
        } else {
            i += 1;
        }
    }
    None
}

/// Ordered allow/disallow pattern list (`WildcardGroup`).
///
/// `is_match` returns the verdict of the last-inserted matching pattern, or
/// the supplied default when nothing matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WildcardGroup {
    entries: Vec<(String, bool)>,
}

impl WildcardGroup {
    /// Empty group.
    #[must_use]
    pub fn new() -> WildcardGroup {
        WildcardGroup::default()
    }

    /// Appends an allow pattern (`Allow`).
    pub fn allow(&mut self, pattern: &str) {
        self.entries.push((pattern.to_string(), true));
    }

    /// Appends a disallow pattern (`Disallow`).
    pub fn disallow(&mut self, pattern: &str) {
        self.entries.push((pattern.to_string(), false));
    }

    /// Number of patterns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the group holds no patterns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends another group's patterns (`AppendFrom`; used by merge).
    pub fn append_from(&mut self, other: &WildcardGroup) {
        self.entries.extend(other.entries.iter().cloned());
    }

    /// Clears all patterns (`Clear`).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Matches (`Match`): last-inserted match wins, else `default_allow`.
    #[must_use]
    pub fn is_match(&self, text: &str, default_allow: bool) -> bool {
        for (pattern, allow) in self.entries.iter().rev() {
            if Wildcard::new(pattern).matches(text) {
                return *allow;
            }
        }
        default_allow
    }

    /// Signature (`Signature`: `spec + ("A"|"D") + ","` per entry, in order).
    #[must_use]
    pub fn signature(&self) -> String {
        let mut out = String::new();
        for (pattern, allow) in &self.entries {
            out.push_str(pattern);
            out.push(if *allow { 'A' } else { 'D' });
            out.push(',');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_and_simple() {
        assert!(Wildcard::new("abc").matches("abc"));
        assert!(!Wildcard::new("abc").matches("abd"));
        assert!(!Wildcard::new("abc").matches("abcd"));
        assert!(!Wildcard::new("abc").matches("ABC")); // case-sensitive
        assert!(Wildcard::new("").matches(""));
        assert!(!Wildcard::new("").matches("a"));
    }

    #[test]
    fn star_forms() {
        assert!(Wildcard::new("*").matches(""));
        assert!(Wildcard::new("*").matches("anything at all"));
        assert!(Wildcard::new("**").matches("x"));
        assert!(Wildcard::new("a*").matches("a"));
        assert!(Wildcard::new("a*").matches("abc"));
        assert!(!Wildcard::new("a*").matches("ba"));
        assert!(Wildcard::new("*a").matches("a"));
        assert!(Wildcard::new("*a").matches("bba"));
        assert!(!Wildcard::new("*a").matches("aab"));
        assert!(Wildcard::new("*a*").matches("a"));
        assert!(Wildcard::new("*a*").matches("xxaxx"));
        assert!(!Wildcard::new("*a*").matches("xxx"));
        // Real config shapes from DisallowTroublesomeResources.
        assert!(Wildcard::new("*js_tinyMCE*").matches("http://x/js_tinyMCE.js"));
        assert!(!Wildcard::new("*js_tinyMCE*").matches("http://x/other.js"));
        assert!(Wildcard::new("*://l.yimg.com/*").matches("https://l.yimg.com/foo"));
    }

    #[test]
    fn question_mark() {
        assert!(Wildcard::new("a?c").matches("abc"));
        assert!(!Wildcard::new("a?c").matches("ac"));
        assert!(!Wildcard::new("a?c").matches("abbc"));
        assert!(Wildcard::new("?").matches("x"));
        assert!(!Wildcard::new("?").matches(""));
        assert!(Wildcard::new("a*?").matches("ab"));
        assert!(Wildcard::new("a*?").matches("axyz"));
        assert!(!Wildcard::new("a*?").matches("a"));
    }

    #[test]
    fn middle_blocks_leftmost() {
        assert!(Wildcard::new("a*b*c").matches("axxbyyc"));
        assert!(!Wildcard::new("a*b*c").matches("axxbyy"));
        assert!(Wildcard::new("*a*b*").matches("xaaybb"));
        assert!(!Wildcard::new("*a*b*").matches("xbay"));
        // Repeated block content needs rightmost retry, not first-candidate.
        assert!(Wildcard::new("*ab*ab").matches("abxab"));
        assert!(Wildcard::new("a?a*a").matches("abaXa"));
    }

    #[test]
    fn group_last_match_wins() {
        let mut group = WildcardGroup::new();
        assert!(group.is_empty());
        assert!(group.is_match("x", true));
        assert!(!group.is_match("x", false));
        group.allow("*.example.com/*");
        group.disallow("*.example.com/private/*");
        assert!(group.is_match("http://a.example.com/x", false));
        assert!(!group.is_match("http://a.example.com/private/x", true));
        // Later allow overrides the earlier disallow.
        group.allow("*.example.com/private/ok/*");
        assert!(group.is_match("http://a.example.com/private/ok/x", false));
        assert_eq!(group.len(), 3);
        assert_eq!(
            group.signature(),
            "*.example.com/*A,*.example.com/private/*D,\
             *.example.com/private/ok/*A,"
        );
        let mut other = WildcardGroup::new();
        other.append_from(&group);
        assert_eq!(other, group);
        other.clear();
        assert!(other.is_empty());
    }
}
