//! Admin access control (`SystemRewriteOptions` allow-lists).
//!
//! Each admin endpoint family carries a wildcard host group:
//! an empty group allows every host, otherwise the request
//! host must match, with later rules overriding earlier ones.
//! This closes admin-gap-2 (traced from the Apache/nginx
//! handler dispatch through `AllowDomain`).

/// Glob matcher (`Wildcard::Match`, case-sensitive): `*`
/// matches any run, `?` matches one byte.
#[must_use]
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut px, mut tx) = (0usize, 0usize);
    let (mut star, mut backtrack) = (None, 0usize);
    while tx < text.len() {
        if px < pattern.len() && (pattern[px] == b'?' || pattern[px] == text[tx]) {
            px += 1;
            tx += 1;
        } else if px < pattern.len() && pattern[px] == b'*' {
            star = Some(px);
            px += 1;
            backtrack = tx;
        } else if let Some(star_pos) = star {
            px = star_pos + 1;
            backtrack += 1;
            tx = backtrack;
        } else {
            return false;
        }
    }
    while px < pattern.len() && pattern[px] == b'*' {
        px += 1;
    }
    px == pattern.len()
}

/// Ordered allow/deny host rules (`FastWildcardGroup`).
#[derive(Debug, Default, Clone)]
pub struct WildcardGroup {
    rules: Vec<(String, bool)>,
}

impl WildcardGroup {
    /// Creates an empty group (allows everything).
    #[must_use]
    pub fn new() -> Self {
        WildcardGroup::default()
    }

    /// Appends an allow rule (`Allow`).
    pub fn allow(&mut self, pattern: &str) {
        self.rules.push((pattern.to_string(), true));
    }

    /// Appends a deny rule (`Disallow`).
    pub fn disallow(&mut self, pattern: &str) {
        self.rules.push((pattern.to_string(), false));
    }

    /// Whether the group holds no rules (`empty`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Matches a host (`Match`): the most recent matching rule
    /// wins, else `default_allow`.
    #[must_use]
    pub fn matches(&self, host: &str, default_allow: bool) -> bool {
        for (pattern, allow) in self.rules.iter().rev() {
            if wildcard_match(pattern, host) {
                return *allow;
            }
        }
        default_allow
    }
}

/// Per-endpoint access lists (the six admin domain groups).
#[derive(Debug, Default)]
pub struct AdminAccess {
    /// `mod_pagespeed_statistics` hosts.
    pub statistics: WildcardGroup,
    /// Global statistics hosts.
    pub global_statistics: WildcardGroup,
    /// Message-history hosts.
    pub messages: WildcardGroup,
    /// Console hosts.
    pub console: WildcardGroup,
    /// Admin console hosts.
    pub admin: WildcardGroup,
    /// Global admin console hosts.
    pub global_admin: WildcardGroup,
}

/// Admin endpoint families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminEndpoint {
    /// Per-vhost statistics page.
    Statistics,
    /// Whole-server statistics page.
    GlobalStatistics,
    /// Message history page.
    Messages,
    /// Console page.
    Console,
    /// Per-vhost admin console.
    Admin,
    /// Whole-server admin console.
    GlobalAdmin,
}

impl AdminAccess {
    /// Whether a host may use an endpoint (`AllowDomain`):
    /// empty groups allow all, otherwise the host must match
    /// (default deny).
    #[must_use]
    pub fn allowed(&self, endpoint: AdminEndpoint, host: &str) -> bool {
        if host.is_empty() {
            return false;
        }
        let group = match endpoint {
            AdminEndpoint::Statistics => &self.statistics,
            AdminEndpoint::GlobalStatistics => &self.global_statistics,
            AdminEndpoint::Messages => &self.messages,
            AdminEndpoint::Console => &self.console,
            AdminEndpoint::Admin => &self.admin,
            AdminEndpoint::GlobalAdmin => &self.global_admin,
        };
        if group.is_empty() {
            // Allow unless they disallowed anything.
            return true;
        }
        group.matches(host, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_basics() {
        assert!(wildcard_match("Hello", "Hello"));
        assert!(!wildcard_match("Hello", "xHello"));
        assert!(wildcard_match("*.example.com", "www.example.com"));
        assert!(!wildcard_match("*.example.com", "example.com"));
        assert!(wildcard_match("10.0.?.*", "10.0.1.2"));
        assert!(!wildcard_match("10.0.?.*", "10.0.12.2"));
        assert!(wildcard_match("*", "anything"));
    }

    #[test]
    fn empty_group_allows_all() {
        let access = AdminAccess::default();
        assert!(access.allowed(AdminEndpoint::Admin, "example.com"));
        assert!(!access.allowed(AdminEndpoint::Admin, ""));
    }

    #[test]
    fn later_rules_override() {
        let mut access = AdminAccess::default();
        access.admin.allow("*.example.com");
        access.admin.disallow("evil.example.com");
        assert!(access.allowed(AdminEndpoint::Admin, "www.example.com"));
        assert!(!access.allowed(AdminEndpoint::Admin, "evil.example.com"));
        assert!(!access.allowed(AdminEndpoint::Admin, "other.org"));
        // Other families stay open.
        assert!(access.allowed(AdminEndpoint::Statistics, "other.org"));
    }
}
