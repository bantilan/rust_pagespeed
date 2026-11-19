//! Option scopes, mirroring `RewriteOptions::OptionScope`.
//!
//! Source: `mod_pagespeed/net/instaweb/rewriter/public/rewrite_options.h`.
//! Ordering is load-bearing: a directive set at a narrower scope overrides
//! wider scopes, and some directives are rejected below a minimum scope.
#![forbid(unsafe_code)]

/// Scope at which an option may be set, narrowest first.
///
/// C++ order: `kQueryScope < kDirectoryScope < kServerScope <
/// kLegacyProcessScope < kProcessScopeStrict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Per-request only (`?PageSpeed=...` query params / cookies).
    Query,
    /// Directory / location context and narrower.
    Directory,
    /// Server / vhost context and narrower.
    Server,
    /// Legacy process-wide options still honored per-server in some paths.
    LegacyProcess,
    /// Main / http context only.
    ProcessStrict,
}

impl Scope {
    /// Parses the C++ enumerator name (e.g. `"kDirectoryScope"`).
    #[must_use]
    pub fn from_cpp_name(name: &str) -> Option<Scope> {
        match name {
            "kQueryScope" => Some(Scope::Query),
            "kDirectoryScope" => Some(Scope::Directory),
            "kServerScope" => Some(Scope::Server),
            "kLegacyProcessScope" => Some(Scope::LegacyProcess),
            "kProcessScopeStrict" => Some(Scope::ProcessStrict),
            _ => None,
        }
    }

    /// The C++ enumerator name.
    #[must_use]
    pub const fn cpp_name(self) -> &'static str {
        match self {
            Scope::Query => "kQueryScope",
            Scope::Directory => "kDirectoryScope",
            Scope::Server => "kServerScope",
            Scope::LegacyProcess => "kLegacyProcessScope",
            Scope::ProcessStrict => "kProcessScopeStrict",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_matches_cpp() {
        assert!(
            Scope::Query < Scope::Directory
                && Scope::Directory < Scope::Server
                && Scope::Server < Scope::LegacyProcess
                && Scope::LegacyProcess < Scope::ProcessStrict
        );
    }

    #[test]
    fn cpp_names_round_trip() {
        for scope in [
            Scope::Query,
            Scope::Directory,
            Scope::Server,
            Scope::LegacyProcess,
            Scope::ProcessStrict,
        ] {
            assert_eq!(Scope::from_cpp_name(scope.cpp_name()), Some(scope));
        }
        assert_eq!(Scope::from_cpp_name("kBogus"), None);
    }
}
