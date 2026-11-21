//! Beacon handling (`ps_beacon_handler`, `HandleBeacon`).
//!
//! Combines query and POST beacon data, validates the URL, folds
//! onload timings into the worker statistics, merges critical
//! image and LCP evidence into the metadata-backed beacon cohort,
//! and answers 204.

use ps_html::beacon_finder::{self, AcceptOutcome, BeaconFinder, BeaconReport};
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

/// Evidence-store half of a beacon (`HandleBeacon` pcache half):
/// parses the LCP/nonce report and merges it into the variant's
/// record. Returns the merge outcome, or `None` when the beacon
/// carries no options hash (evidence keys require it).
pub fn handle_beacon_evidence(
    params: &str,
    page_url: &str,
    user_agent: &str,
    server: &ps_html::server_context::ServerContext,
    now_ms: i64,
) -> Option<AcceptOutcome> {
    let mut query_params = QueryParams::new();
    query_params.parse_from_untrusted_string(params);
    let options_hash = query_params.lookup1_unescaped(beacon_finder::PARAM_OPTIONS_HASH)?;
    let report = parse_beacon_report(&query_params);
    if report.html_hashes.is_none() {
        return None;
    }
    let matcher = ps_http::user_agent_matcher::UserAgentMatcher::new();
    let device = matcher.device_type_for_ua(user_agent);
    let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(device);
    let finder = BeaconFinder::new(server.metadata_cache());
    let mut evidence = finder.load(page_url, &options_hash, suffix).into_record();
    let before = evidence.clone();
    let outcome = beacon_finder::accept_beacon(&mut evidence, &report, now_ms);
    // The cohort writes back unconditionally (even nonce
    // failures), preserving expiry feedback.
    if evidence != before {
        finder.store(page_url, &options_hash, suffix, &evidence);
    }
    Some(outcome)
}

/// Parses the LCP/nonce evidence report: a stamped LCP hash joins
/// the critical set with img kind; otherwise a valid kind (with
/// an empty set, so the beacon still counts) plus the loaded URL
/// for unstamped image LCP.
fn parse_beacon_report(query_params: &QueryParams) -> BeaconReport {
    let mut report = BeaconReport::default();
    if let Some(hash) = query_params.lookup1_unescaped(beacon_finder::PARAM_LCP_IMAGE) {
        if !hash.is_empty() {
            report
                .html_hashes
                .get_or_insert_with(std::collections::HashSet::new)
                .insert(hash);
        }
    }
    if report.html_hashes.is_some() {
        report.lcp_kind = beacon_finder::LCP_KIND_IMG.to_string();
    } else if let Some(kind) = query_params.lookup1_unescaped(beacon_finder::PARAM_LCP_KIND) {
        if beacon_finder::valid_lcp_kind(&kind) {
            if kind != beacon_finder::LCP_KIND_TEXT {
                if let Some(url) = query_params.lookup1_unescaped(beacon_finder::PARAM_LCP_URL) {
                    if !url.is_empty() && url.len() <= beacon_finder::LCP_URL_MAX_LENGTH {
                        report.lcp_image_url = url;
                    }
                }
            }
            report.lcp_kind = kind;
            report.html_hashes = Some(std::collections::HashSet::new());
        }
    }
    if let Some(nonce) = query_params.lookup1_unescaped(beacon_finder::PARAM_NONCE) {
        report.nonce = nonce;
    }
    report
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

    #[test]
    fn evidence_half_stores_lcp() {
        use ps_html::beacon_finder::{BeaconEvidence, BeaconFinder};
        // One metadata cache backs the finder (like one server).
        let metadata: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let html = crate::rewrite::html_context(
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new()),
            std::sync::Arc::clone(&metadata),
            std::sync::Arc::new(ps_stats::core::Statistics::new()),
            0,
            std::rc::Rc::new(crate::rewrite::UnavailableFetcher),
        );
        let server = html;
        // No options hash: stats-only, nothing stored.
        assert!(handle_beacon_evidence(
            "url=http://h/p&lcp=1&n=x",
            "http://h/p",
            "Test/1.0",
            &server,
            1000
        )
        .is_none());
        // Seed a pending nonce through the finder.
        let finder = BeaconFinder::new(server.metadata_cache());
        let mut evidence = BeaconEvidence::default();
        let nonce =
            ps_html::beacon_finder::prepare_beacon(&mut evidence, 1000, 5, 42)
                .expect("due");
        let hash = ps_html::beacon_finder::url_hash("http://h/hero.png");
        let keys: std::collections::HashSet<String> =
            [hash.clone()].into_iter().collect();
        ps_html::beacon_finder::update_candidates(&mut evidence, &keys, false);
        finder.store("http://h/p", "oh1", "@Desktop", &evidence);
        // Bad nonces reject without merging.
        assert_eq!(
            handle_beacon_evidence(
                &format!("url=http://h/p&oh=oh1&lcp={hash}&n=wrong"),
                "http://h/p",
                "Test/1.0",
                &server,
                1000
            ),
            Some(AcceptOutcome::BadNonce)
        );
        // The issued nonce accepts and stores LCP evidence.
        assert_eq!(
            handle_beacon_evidence(
                &format!("url=http://h/p&oh=oh1&lcp={hash}&n={nonce}"),
                "http://h/p",
                "Test/1.0",
                &server,
                1000
            ),
            Some(AcceptOutcome::Accepted)
        );
        let stored = finder.load("http://h/p", "oh1", "@Desktop");
        let record = stored.record().expect("stored record");
        assert_eq!(1, record.valid_beacons);
        assert_eq!(ps_html::beacon_finder::LCP_KIND_IMG, record.lcp_kind);
        assert!(ps_html::beacon_finder::is_html_critical_image(
            "http://h/hero.png",
            record
        ));
    }
}
