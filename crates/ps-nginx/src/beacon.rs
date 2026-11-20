//! Beacon handling (`ps_beacon_handler`, `HandleBeacon` stats half).
//!
//! Combines query and POST beacon data, validates the URL,
//! folds onload timings into the worker statistics, and answers
//! 204. The property-cache half (critical-image sets) waits for
//! the property store; without it the handler returns after the
//! stats, exactly like C++ with a disabled pcache.

use ps_http::google_url::GoogleUrl;
use ps_http::query_params::QueryParams;
use ps_stats::core::Statistics;

/// Beacon URL parameter (`kBeaconUrlQueryParam`).
pub const BEACON_URL_PARAM: &str = "url";
/// Beacon timing parameter (`kBeaconEtsQueryParam`).
pub const BEACON_ETS_PARAM: &str = "ets";
/// Mobilization beacon id (short-circuits).
pub const MOBILIZATION_ID: &str = "psmob";

/// Onload total (`RewriteStats::total_page_load_ms`).
pub const TOTAL_PAGE_LOAD_MS: &str = "total_page_load_ms";
/// Onload count (`RewriteStats::page_load_count`).
pub const PAGE_LOAD_COUNT: &str = "page_load_count";
/// Onload timing histogram (`beacon_timings_ms_histogram`).
pub const BEACON_TIMINGS_HISTOGRAM: &str = "Beacon Reported Load Time (ms)";

/// Combines query params with an optional POST body (`StrCat`
/// in `ps_beacon_body_handler`; GET beacons pass an empty body).
#[must_use]
pub fn combine_beacon_data(query: &str, body: &str) -> String {
    format!("{query}&{body}")
}

/// Handles a beacon (`HandleBeacon` stats half): returns
/// success plus any warning to log. Stats fold into the worker
/// registry; the pcache half is skipped (no property store).
pub fn handle_beacon(params: &str, statistics: &Statistics) -> (bool, Option<String>) {
    let mut query_params = QueryParams::new();
    query_params.parse_from_untrusted_string(params);
    if query_params.lookup1_unescaped("id").as_deref() == Some(MOBILIZATION_ID) {
        return (true, None);
    }
    let Some(url_param) = query_params.lookup1_unescaped(BEACON_URL_PARAM) else {
        return (
            false,
            Some(format!(
                "Missing URL parameter in beacon: {}",
                cescape(params)
            )),
        );
    };
    let url = GoogleUrl::new(&url_param);
    if !url.is_web_valid() {
        return (
            false,
            Some(format!(
                "Invalid URL parameter in beacon: {}",
                cescape(&url_param)
            )),
        );
    }
    let mut status = true;
    if let Some(ets) = query_params.lookup1_unescaped(BEACON_ETS_PARAM) {
        if let Some(index) = ets.find(':') {
            if index < ets.len() {
                let load_time = &ets[index + 1..];
                match load_time.parse::<i32>() {
                    Ok(ms) if ms >= 0 => {
                        statistics.add_variable(TOTAL_PAGE_LOAD_MS).add(ms as i64);
                        statistics.add_variable(PAGE_LOAD_COUNT).add(1);
                        statistics
                            .add_histogram(BEACON_TIMINGS_HISTOGRAM)
                            .add(ms as f64);
                    }
                    _ => status = false,
                }
            }
        }
    }
    (status, None)
}

/// Escapes client data for single-line logs (`CEscape`): keeps
/// printable ASCII, C-escapes quotes/backslash/controls, octal
/// escapes the rest.
#[must_use]
pub fn cescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\{byte:03o}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_like_cescape() {
        assert_eq!(cescape("plain"), "plain");
        assert_eq!(cescape("a\nb\rc\td\"e\\f"), "a\\nb\\rc\\td\\\"e\\\\f");
        assert_eq!(cescape("\x01\x7f "), "\\001\\177\\302\\240");
    }

    #[test]
    fn beacon_matrix() {
        let stats = Statistics::new();
        // Mobilization short-circuits.
        assert_eq!(handle_beacon("id=psmob", &stats), (true, None));
        // Missing URL warns with escaped params.
        let (status, warning) = handle_beacon("ets=load:10", &stats);
        assert!(!status);
        assert_eq!(
            warning.unwrap(),
            "Missing URL parameter in beacon: ets=load:10"
        );
        // Bad URL warns with the escaped value.
        let (status, warning) = handle_beacon("url=::bad::", &stats);
        assert!(!status);
        assert!(warning
            .unwrap()
            .starts_with("Invalid URL parameter in beacon: "));
        // Onload timings fold into worker stats.
        let (status, warning) = handle_beacon("url=http://h/p&ets=load:120", &stats);
        assert_eq!((status, warning), (true, None));
        assert_eq!(stats.get_variable(TOTAL_PAGE_LOAD_MS).unwrap().get(), 120);
        assert_eq!(stats.get_variable(PAGE_LOAD_COUNT).unwrap().get(), 1);
        assert!(stats.get_histogram(BEACON_TIMINGS_HISTOGRAM).is_some());
        // Bad timings fail without stats; missing colons skip.
        let (status, _) = handle_beacon("url=http://h/p&ets=load:lots", &stats);
        assert!(!status);
        assert_eq!(stats.get_variable(PAGE_LOAD_COUNT).unwrap().get(), 1);
        let (status, _) = handle_beacon("url=http://h/p&ets=load", &stats);
        assert!(status);
        // Data combination always joins with `&`, even when empty.
        assert_eq!(combine_beacon_data("a=1", "b=2"), "a=1&b=2");
        assert_eq!(combine_beacon_data("", ""), "&");
    }
}
