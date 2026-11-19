//! Embedded admin stylesheets and scripts (the `data2c` blobs).

/// Stylesheet bytes by leaf name (`admin_site_css`, ...).
#[must_use]
pub fn stylesheet(name: &str) -> &'static str {
    match name {
        "caches" => include_str!("../assets/caches.css"),
        "console" => include_str!("../assets/console.css"),
        "graphs" => include_str!("../assets/graphs.css"),
        "statistics" => include_str!("../assets/statistics.css"),
        _ => include_str!("../assets/admin_site.css"),
    }
}

/// Script bytes by leaf name (`caches_js`, ...); `debug`
/// selects the unminified builds.
#[must_use]
pub fn script(name: &str, debug: bool) -> &'static str {
    match (name, debug) {
        ("caches", false) => include_str!("../assets/caches_opt.js"),
        ("caches", true) => include_str!("../assets/caches_dbg.js"),
        ("config", _) => include_str!("../assets/config_dbg.js"),
        ("console", false) => include_str!("../assets/console_opt.js"),
        ("console", true) => include_str!("../assets/console_dbg.js"),
        ("graphs", false) => include_str!("../assets/graphs_opt.js"),
        ("graphs", true) => include_str!("../assets/graphs_dbg.js"),
        ("messages", false) => include_str!("../assets/messages_opt.js"),
        ("messages", true) => include_str!("../assets/messages_dbg.js"),
        ("statistics", false) => include_str!("../assets/statistics_opt.js"),
        ("statistics", true) => include_str!("../assets/statistics_dbg.js"),
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_nonempty() {
        for name in ["caches", "console", "graphs", "statistics", "admin"] {
            assert!(!stylesheet(name).is_empty(), "{name}");
        }
        for name in [
            "caches",
            "config",
            "console",
            "graphs",
            "messages",
            "statistics",
        ] {
            assert!(!script(name, false).is_empty(), "{name}");
            assert!(!script(name, true).is_empty(), "{name}");
        }
    }
}
