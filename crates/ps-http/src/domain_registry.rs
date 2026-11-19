//! Public-suffix hostname analysis (`domain_registry`).
//!
//! Computes minimal private suffixes (the registrable domain: one label
//! above the public suffix) over the vendored Public Suffix List,
//! following the list's matching algorithm: longest exception rule wins,
//! then longest exact rule, then longest wildcard rule; unknown TLDs and
//! malformed hostnames fail secure to the whole input. Matching runs on
//! an ASCII-lowercased scratch copy while results slice the original
//! (length-preserving, so offsets transfer).

use std::collections::HashSet;
use std::sync::OnceLock;

const LIST: &str = include_str!("../assets/public_suffix_list.dat");

/// Parsed rule sets (exact, wildcard parents, exceptions).
struct Rules {
    exact: HashSet<String>,
    wildcard: HashSet<String>,
    exception: HashSet<String>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut exact = HashSet::new();
        let mut wildcard = HashSet::new();
        let mut exception = HashSet::new();
        for line in LIST.lines().map(str::trim) {
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            if let Some(rule) = line.strip_prefix('!') {
                exception.insert(rule.to_string());
            } else if let Some(rule) = line.strip_prefix("*.") {
                wildcard.insert(rule.to_string());
            } else {
                exact.insert(line.to_string());
            }
        }
        Rules {
            exact,
            wildcard,
            exception,
        }
    })
}

/// No-op initializer (`Init`); parsing is lazy here.
pub fn init() {}

/// Minimal private suffix (`MinimalPrivateSuffix`): the registrable
/// domain, or the whole hostname when unknown or malformed.
#[must_use]
pub fn minimal_private_suffix(hostname: &str) -> &str {
    if hostname.is_empty() {
        return "";
    }
    // One trailing dot marks a FQDN and survives; anything odder fails
    // secure to the whole input.
    let (work, had_dot) = match hostname.strip_suffix('.') {
        Some(work) => (work, true),
        None => (hostname, false),
    };
    if work.is_empty() || work.starts_with('.') || work.ends_with('.') || work.contains("..") {
        return hostname;
    }
    let lower = work.to_ascii_lowercase();
    let labels: Vec<&str> = lower.split('.').collect();
    let rules = rules();
    // Longest exception rule wins outright: the exception itself
    // becomes registrable, so the private suffix starts at the match.
    for start in 0..labels.len() {
        if rules.exception.contains(&labels[start..].join(".")) {
            return slice_from_label(hostname, &labels, start, had_dot);
        }
    }
    // Longest exact rule, then longest wildcard parent.
    let mut exact_at = None;
    let mut wildcard_at = None;
    for start in 0..labels.len() {
        let suffix = labels[start..].join(".");
        if exact_at.is_none() && rules.exact.contains(&suffix) {
            exact_at = Some(start);
        }
        if wildcard_at.is_none() && start + 1 < labels.len() {
            let parent = labels[start + 1..].join(".");
            if rules.wildcard.contains(&parent) {
                wildcard_at = Some(start);
            }
        }
    }
    if let Some(start) = exact_at.or(wildcard_at) {
        // The private suffix adds one label above the public suffix;
        // when the host is that short it is already minimal.
        let cut = if start > 0 { start - 1 } else { 0 };
        return slice_from_label(hostname, &labels, cut, had_dot);
    }
    hostname
}

/// Slices the original hostname from a label index, restoring one
/// trailing dot when the input had it.
fn slice_from_label<'a>(hostname: &'a str, labels: &[&str], cut: usize, had_dot: bool) -> &'a str {
    let mut offset: usize = labels[..cut].iter().map(|label| label.len() + 1).sum();
    offset = offset.min(hostname.len());
    let mut end = hostname.len();
    if had_dot {
        end = end.saturating_sub(1);
    }
    // Re-append the single trailing dot the input carried.
    let end = if had_dot { end + 1 } else { end };
    &hostname[offset.min(end)..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_private_suffix_cases() {
        init();
        assert_eq!(minimal_private_suffix("google.com"), "google.com");
        assert_eq!(minimal_private_suffix("www.google.com"), "google.com");
        assert_eq!(minimal_private_suffix("www.google.com."), "google.com.");
        assert_eq!(minimal_private_suffix("google.com."), "google.com.");
        assert_eq!(
            minimal_private_suffix("www.google.com.."),
            "www.google.com.."
        );
        assert_eq!(minimal_private_suffix("google.co.uk"), "google.co.uk");
        assert_eq!(minimal_private_suffix("www.google.co.uk"), "google.co.uk");
        assert_eq!(
            minimal_private_suffix("foo.bar.google.co.uk"),
            "google.co.uk"
        );
        assert_eq!(
            minimal_private_suffix("a.b.c.d.e.f.g.h.i.j.k.l.co.uk"),
            "l.co.uk"
        );
        assert_eq!(
            minimal_private_suffix("example.appspot.com"),
            "example.appspot.com"
        );
        assert_eq!(
            minimal_private_suffix("www.example.appspot.com"),
            "example.appspot.com"
        );
        assert_eq!(
            minimal_private_suffix("a.b.c.this.doesntexist"),
            "a.b.c.this.doesntexist"
        );
        assert_eq!(minimal_private_suffix("com"), "com");
        assert_eq!(minimal_private_suffix(""), "");
        assert_eq!(minimal_private_suffix("."), ".");
        assert_eq!(minimal_private_suffix(".."), "..");
        assert_eq!(minimal_private_suffix("..doesntexist."), "..doesntexist.");
    }
}
