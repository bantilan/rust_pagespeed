//! Page corpus (Phase 4b): inputs, normalization, record/compare.
//!
//! [`CASES`] is the hand-maintained input list. Record mode
//! (`PS_COMPAT_RECORD=1`) serves each case through the C++
//! stack and writes normalized expectations to
//! `tests/corpus_data/pages.rs` (committed, reviewable).
//! Compare mode (default) replays the recorded cases against
//! the Rust stack. [`normalize`] keeps both sides honest: only
//! pagespeed behavior is compared, never framing, clocks, or
//! version strings.

use ps_compat::harness::Assertion;
use std::collections::HashMap;

/// Fixed origin-file mtime (epoch seconds) so ETag and
/// Last-Modified agree across stacks and runs.
pub const FIXED_MTIME_SECS: u64 = 1_700_000_000;

/// Cache-dir placeholder in directives, replaced per case.
pub const CACHE_PLACEHOLDER: &str = "{CACHE}";

/// One corpus input (hand-maintained).
pub struct CaseDef {
    /// Case name (also the recorded file key).
    pub name: &'static str,
    /// Request path (`/ws.html`).
    pub path: &'static str,
    /// Origin bytes (`None` serves nothing: 404 cases).
    pub html: Option<&'static [u8]>,
    /// Directives as (name, value); empty value renders bare.
    pub directives: &'static [(&'static str, &'static str)],
    /// Known divergence reason (reported, never fails).
    pub known_gap: Option<&'static str>,
}

/// Whitespace/comments fixture.
const WS_HTML: &[u8] = b"<html>  <head>  <title>t</title>  <!-- comment -->  </head>  <body>  <p>  hi  </p>  </body>  </html>";

/// Richer fixture (links, images, scripts, styles).
const RICH_HTML: &[u8] = b"<!DOCTYPE html><html><head><title>Bench page</title><link rel=\"stylesheet\" href=\"a.css\"><style>body { color: red; }</style><script src=\"a.js\"></script><script>function f() { return 1 + 2; }</script></head><body><div class=\"a\" id=\"x\"><p>Hello <b>world</b> <a href=\"/p?q=1\">link</a></p><img src=\"i.png\" width=\"10\" height=\"10\"></div></body></html>";

/// The input corpus.
pub const CASES: &[CaseDef] = &[
    CaseDef {
        name: "smoke-html",
        path: "/page.html",
        html: Some(b"<html><head><title>t</title></head><body><p>hi</p></body></html>"),
        directives: &[("on", ""), ("FileCachePath", CACHE_PLACEHOLDER)],
        known_gap: None,
    },
    CaseDef {
        name: "whitespace-comments",
        path: "/ws.html",
        html: Some(WS_HTML),
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("EnableFilters", "collapse_whitespace,remove_comments"),
        ],
        known_gap: None,
    },
    CaseDef {
        name: "core-rich",
        path: "/rich.html",
        html: Some(RICH_HTML),
        directives: &[("on", ""), ("FileCachePath", CACHE_PLACEHOLDER)],
        known_gap: Some("inline CSS/JS minify wiring unported (slots skipped)"),
    },
    CaseDef {
        name: "elide-attributes",
        path: "/elide.html",
        html: Some(RICH_HTML),
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("EnableFilters", "elide_attributes"),
        ],
        known_gap: Some("inline CSS/JS minify wiring unported (slots skipped)"),
    },
    CaseDef {
        name: "missing-file",
        path: "/no-such-file.html",
        html: None,
        directives: &[("on", ""), ("FileCachePath", CACHE_PLACEHOLDER)],
        known_gap: Some("error s-maxage rides unported recorder arming"),
    },
];

/// A recorded case: inputs plus normalized C++ expectations.
#[derive(Debug, Clone)]
pub struct RecordedCase {
    /// Case name.
    pub name: String,
    /// Request path.
    pub path: String,
    /// Origin bytes (empty when the case serves nothing).
    pub html: Vec<u8>,
    /// Directives with `{CACHE}` already substituted at record
    /// time (compare re-substitutes per case).
    pub directives: Vec<(String, String)>,
    /// Expected status.
    pub status: u16,
    /// Expected headers (normalized, sorted).
    pub headers: Vec<(String, String)>,
    /// Expected body (normalized).
    pub body: Vec<u8>,
    /// Known divergence reason, if any.
    pub known_gap: Option<String>,
}

/// Headers dropped from comparison (framing, clocks, server
/// identity — no pagespeed signal).
const DROPPED_HEADERS: &[&str] = &[
    "date",
    "server",
    "connection",
    "transfer-encoding",
    "content-length",
];

/// Normalizes a response for comparison: drops framing/clock/
/// identity headers, pins the version header, sorts the rest
/// for multiset comparison. Bodies compare byte-exact.
#[must_use]
pub fn normalize(headers: &[(String, String)], body: &[u8]) -> (Vec<(String, String)>, Vec<u8>) {
    let mut kept: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| !DROPPED_HEADERS.contains(&name.as_str()))
        .map(|(name, value)| {
            if name == "x-page-speed" {
                (name.clone(), "VERSION".to_string())
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect();
    kept.sort();
    (kept, body.to_vec())
}

/// Renders bytes as a Rust vec literal.
fn byte_literal(bytes: &[u8]) -> String {
    let mut out = String::from("vec![");
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        if index % 16 == 0 && index > 0 {
            out.push_str("\n        ");
        }
        out.push_str(&byte.to_string());
    }
    out.push(']');
    out
}

/// Renders a string as a Rust string literal.
fn str_literal(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{{{:x}}}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Writes recorded cases as `tests/corpus_data/pages.rs`.
pub fn write_recorded(
    cases: &[RecordedCase],
    manifest_dir: &std::path::Path,
) -> std::io::Result<()> {
    let dir = manifest_dir.join("tests/corpus_data");
    std::fs::create_dir_all(&dir)?;
    let mut out = String::from(
        "// GENERATED by `PS_COMPAT_RECORD=1` — review before committing.\n\
         // Each case: inputs plus normalized C++ expectations.\n\n\
         /// Recorded corpus.\n\
         #[must_use]\n\
         pub fn recorded_cases() -> Vec<RecordedCase> {\n\
         vec![\n",
    );
    for case in cases {
        out.push_str("        RecordedCase {\n");
        out.push_str(&format!(
            "            name: {}.to_string(),\n",
            str_literal(&case.name)
        ));
        out.push_str(&format!(
            "            path: {}.to_string(),\n",
            str_literal(&case.path)
        ));
        out.push_str(&format!(
            "            html: {},\n",
            byte_literal(&case.html)
        ));
        out.push_str("            directives: vec![\n");
        for (name, value) in &case.directives {
            out.push_str(&format!(
                "                ({}.to_string(), {}.to_string()),\n",
                str_literal(name),
                str_literal(value)
            ));
        }
        out.push_str("            ],\n");
        out.push_str(&format!("            status: {},\n", case.status));
        out.push_str("            headers: vec![\n");
        for (name, value) in &case.headers {
            out.push_str(&format!(
                "                ({}.to_string(), {}.to_string()),\n",
                str_literal(name),
                str_literal(value)
            ));
        }
        out.push_str("            ],\n");
        out.push_str(&format!(
            "            body: {},\n",
            byte_literal(&case.body)
        ));
        match &case.known_gap {
            Some(reason) => {
                out.push_str(&format!(
                    "            known_gap: Some({}.to_string()),\n",
                    str_literal(reason)
                ));
            }
            None => out.push_str("            known_gap: None,\n"),
        }
        out.push_str("        },\n");
    }
    out.push_str("    ]\n}\n");
    std::fs::write(dir.join("pages.rs"), out)
}

/// Builds compare assertions for a recorded case.
#[must_use]
pub fn compare_assertions(case: &RecordedCase) -> Vec<Assertion> {
    let mut assertions = vec![Assertion::StatusEqual {
        expected: case.status,
        actual: 0,
    }];
    for (name, value) in &case.headers {
        assertions.push(Assertion::HeaderEqual {
            name: name.clone(),
            expected: value.clone(),
            actual: String::new(),
        });
    }
    assertions.push(Assertion::ByteEqual {
        expected: case.body.clone(),
        actual: Vec::new(),
    });
    assertions
}

/// Looks up recorded headers as a multiset-aware map.
#[must_use]
pub fn header_map(headers: &[(String, String)]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (name, value) in headers {
        map.entry(name.clone()).or_default().push(value.clone());
    }
    for values in map.values_mut() {
        values.sort();
    }
    map
}

include!("corpus_data/pages.rs");

/// One config-corpus input (hand-maintained).
pub struct ConfigDef {
    /// Case name.
    pub name: &'static str,
    /// Probe path (origin holds [`CONFIG_PROBE_HTML`]).
    pub path: &'static str,
    /// Directives as (name, value); empty value renders bare.
    pub directives: &'static [(&'static str, &'static str)],
    /// Known divergence reason (reported, never fails).
    pub known_gap: Option<&'static str>,
}

/// Fixed probe origin for every config case: exercises
/// whitespace, comments, and elidable attributes at once.
pub const CONFIG_PROBE_HTML: &[u8] = b"<html>  <head>  <title>t</title>  <!-- c -->  </head>  <body>  <div class=\"a\" id=\"x\">  <p>  hi  </p>  </div>  </body>  </html>";

/// The config-corpus inputs.
pub const CONFIGS: &[ConfigDef] = &[
    ConfigDef {
        name: "level-passthrough",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("RewriteLevel", "PassThrough"),
        ],
        known_gap: None,
    },
    ConfigDef {
        name: "level-core",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("RewriteLevel", "CoreFilters"),
        ],
        known_gap: None,
    },
    ConfigDef {
        name: "custom-header-value",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("XHeaderValue", "compat-9"),
        ],
        known_gap: None,
    },
    ConfigDef {
        name: "enable-collapse",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("EnableFilters", "collapse_whitespace"),
        ],
        known_gap: None,
    },
    ConfigDef {
        name: "disable-wins",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("EnableFilters", "collapse_whitespace"),
            ("DisableFilters", "collapse_whitespace"),
        ],
        known_gap: None,
    },
    ConfigDef {
        name: "invalid-level",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("RewriteLevel", "Bogus"),
        ],
        known_gap: None,
    },
    ConfigDef {
        name: "unknown-directive",
        path: "/probe.html",
        directives: &[
            ("on", ""),
            ("FileCachePath", CACHE_PLACEHOLDER),
            ("Frobnicator", "yes"),
        ],
        known_gap: None,
    },
];

/// A recorded config case.
#[derive(Debug, Clone)]
pub struct RecordedConfig {
    /// Case name.
    pub name: String,
    /// Probe path.
    pub path: String,
    /// Directives with `{CACHE}` intact (compare re-substitutes).
    pub directives: Vec<(String, String)>,
    /// Whether the stack booted.
    pub boot_ok: bool,
    /// Expected status (0 when boot failed).
    pub status: u16,
    /// Expected headers (normalized, sorted).
    pub headers: Vec<(String, String)>,
    /// Expected body (normalized).
    pub body: Vec<u8>,
    /// Known divergence reason, if any.
    pub known_gap: Option<String>,
}

/// Writes recorded config cases as `tests/corpus_data/configs.rs`.
pub fn write_recorded_configs(
    cases: &[RecordedConfig],
    manifest_dir: &std::path::Path,
) -> std::io::Result<()> {
    let dir = manifest_dir.join("tests/corpus_data");
    std::fs::create_dir_all(&dir)?;
    let mut out = String::from(
        "// GENERATED by `PS_COMPAT_RECORD=1` — review before committing.\n\
         // Each case: directives plus normalized C++ expectations.\n\n\
         /// Recorded config corpus.\n\
         #[must_use]\n\
         pub fn recorded_configs() -> Vec<RecordedConfig> {\n\
         vec![\n",
    );
    for case in cases {
        out.push_str("        RecordedConfig {\n");
        out.push_str(&format!(
            "            name: {}.to_string(),\n",
            str_literal(&case.name)
        ));
        out.push_str(&format!(
            "            path: {}.to_string(),\n",
            str_literal(&case.path)
        ));
        out.push_str("            directives: vec![\n");
        for (name, value) in &case.directives {
            out.push_str(&format!(
                "                ({}.to_string(), {}.to_string()),\n",
                str_literal(name),
                str_literal(value)
            ));
        }
        out.push_str("            ],\n");
        out.push_str(&format!("            boot_ok: {},\n", case.boot_ok));
        out.push_str(&format!("            status: {},\n", case.status));
        out.push_str("            headers: vec![\n");
        for (name, value) in &case.headers {
            out.push_str(&format!(
                "                ({}.to_string(), {}.to_string()),\n",
                str_literal(name),
                str_literal(value)
            ));
        }
        out.push_str("            ],\n");
        out.push_str(&format!(
            "            body: {},\n",
            byte_literal(&case.body)
        ));
        match &case.known_gap {
            Some(reason) => {
                out.push_str(&format!(
                    "            known_gap: Some({}.to_string()),\n",
                    str_literal(reason)
                ));
            }
            None => out.push_str("            known_gap: None,\n"),
        }
        out.push_str("        },\n");
    }
    out.push_str("    ]\n}\n");
    std::fs::write(dir.join("configs.rs"), out)
}

include!("corpus_data/configs.rs");
