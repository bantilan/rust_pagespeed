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
    merge_font_beacon(&options_hash, &report, page_url, user_agent, server, now_ms);
    if report.html_hashes.is_none() {
        return None;
    }
    let matcher = ps_http::user_agent_matcher::UserAgentMatcher::new();
    let device = matcher.device_type_for_ua(user_agent);
    let group = query_params.lookup1_unescaped("bvg");
    let suffix = match group.as_deref() {
        Some(group) => beacon_finder::public_evidence_suffix(group)?,
        None => ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(device),
    };
    let finder = BeaconFinder::new(server.metadata_cache());
    finder.update(page_url, &options_hash, suffix, |evidence| {
        beacon_finder::accept_beacon(evidence, &report, now_ms)
    })
}

/// Merges font triples from a beacon report: validates the font
/// nonce against the page's outstanding set, then unions triples
/// into the confirmed blob (capped). Independent of the image half.
fn merge_font_beacon(
    options_hash: &str,
    report: &beacon_finder::BeaconReport,
    page_url: &str,
    user_agent: &str,
    server: &ps_html::server_context::ServerContext,
    now_ms: i64,
) {
    use ps_cache::cache::KeyState;
    use ps_html::filters::preload_critical_fonts::{
        FONT_PRELOAD_BEACON_ACCEPTED, FONT_PRELOAD_BEACON_REJECTED,
    };
    if report.font_families.is_empty() {
        return;
    }
    let matcher = ps_http::user_agent_matcher::UserAgentMatcher::new();
    let device = matcher.device_type_for_ua(user_agent);
    let suffix = ps_http::user_agent_matcher::UserAgentMatcher::device_type_suffix(device);
    let cache = server.metadata_cache();
    let nonces_key = ps_html::font_store::nonces_key(page_url, options_hash, suffix);
    let mut nonces = match cache.get(&nonces_key) {
        ps_cache::cache::Lookup {
            state: KeyState::Available,
            value: Some(blob),
        } => String::from_utf8(blob)
            .map(|text| ps_html::font_store::decode_nonces(&text))
            .unwrap_or_default(),
        _ => ps_html::font_store::FontNonces::default(),
    };
    if !ps_html::font_store::consume_nonce(&mut nonces, &report.nonce, now_ms) {
        server.variable(FONT_PRELOAD_BEACON_REJECTED).add(1);
        return;
    }
    cache.put(
        &nonces_key,
        ps_html::font_store::encode_nonces(&nonces).as_bytes(),
    );
    let confirmed_key = ps_html::font_store::confirmed_key(page_url, options_hash, suffix);
    let mut confirmed = match cache.get(&confirmed_key) {
        ps_cache::cache::Lookup {
            state: KeyState::Available,
            value: Some(blob),
        } => String::from_utf8(blob)
            .map(|text| ps_html::font_store::decode_confirmed(&text))
            .unwrap_or_default(),
        _ => std::collections::HashSet::new(),
    };
    let before = confirmed.len();
    for triple in &report.font_families {
        if confirmed.len() >= 64 {
            break;
        }
        confirmed.insert(triple.clone());
    }
    if confirmed.len() != before {
        cache.put(
            &confirmed_key,
            ps_html::font_store::encode_confirmed(&confirmed).as_bytes(),
        );
    }
    server.variable(FONT_PRELOAD_BEACON_ACCEPTED).add(1);
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
    if let Some(escaped) = query_params.lookup1_escaped("cs") {
        report.css_selectors = ps_html::critical_css_evidence::parse_report(escaped);
    }
    if let Some(nonce) = query_params.lookup1_unescaped(beacon_finder::PARAM_NONCE) {
        report.nonce = nonce;
    }
    if let Some(fonts) = query_params.lookup1_unescaped(beacon_finder::PARAM_FONT_FAMILIES) {
        if !fonts.is_empty() {
            report.font_families = beacon_finder::parse_font_families(&fonts);
        }
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
            std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
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
        evidence.single_decision = true;
        let nonce =
            ps_html::beacon_finder::prepare_beacon(&mut evidence, 1000, 5, 42).expect("due");
        let hash = ps_html::beacon_finder::url_hash("http://h/hero.png");
        let keys: std::collections::HashSet<String> = [hash.clone()].into_iter().collect();
        ps_html::beacon_finder::update_candidates(&mut evidence, &keys, false);
        evidence.pending_nonces.push(("competing".into(), 1000));
        finder.store("http://h/p", "oh1", "avif-desktop", &evidence);
        // Bad nonces reject without merging.
        assert_eq!(
            handle_beacon_evidence(
                &format!("url=http://h/p&bvg=avif-desktop&oh=oh1&lcp={hash}&n=wrong"),
                "http://h/p",
                "Test/1.0",
                &server,
                1000
            ),
            Some(AcceptOutcome::BadNonce)
        );
        // A valid nonce for AVIF/desktop cannot write a different group.
        assert_eq!(handle_beacon_evidence(
            &format!("bvg=webp-desktop&oh=oh1&lcp={hash}&n={nonce}"),
            "http://h/p", "Test/1.0", &server, 1000,
        ), Some(AcceptOutcome::BadNonce));
        assert!(handle_beacon_evidence(
            &format!("bvg=invalid&oh=oh1&lcp={hash}&n={nonce}"),
            "http://h/p", "Test/1.0", &server, 1000,
        ).is_none());
        // The issued nonce accepts and stores LCP evidence.
        assert_eq!(
            handle_beacon_evidence(
                &format!("url=http://h/p&bvg=avif-desktop&oh=oh1&lcp={hash}&n={nonce}"),
                "http://h/p",
                "Test/1.0",
                &server,
                1000
            ),
            Some(AcceptOutcome::Accepted)
        );
        let stored = finder.load("http://h/p", "oh1", "avif-desktop");
        let record = stored.record().expect("stored record");
        assert_eq!(1, record.valid_beacons);
        assert!(record.single_decision);
        assert!(record.pending_nonces.is_empty());
        assert_eq!(
            handle_beacon_evidence("bvg=avif-desktop&oh=oh1&lcpk=text&n=competing", "http://h/p", "Test/1.0", &server, 1001),
            Some(AcceptOutcome::BadNonce)
        );
        assert_eq!(finder.load("http://h/p", "oh1", "avif-desktop").record().unwrap(), record);

        assert_eq!(ps_html::beacon_finder::LCP_KIND_IMG, record.lcp_kind);
        assert!(ps_html::beacon_finder::is_html_critical_image(
            "http://h/hero.png",
            record
        ));
    }

    #[test]
    fn combined_css_evidence_uses_one_nonce_and_original_html_group() {
        use ps_html::beacon_finder::{BeaconEvidence, CssCandidateError};
        let metadata: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let server = crate::rewrite::html_context(
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new()), metadata.clone(),
            std::sync::Arc::new(ps_stats::core::Statistics::new()), 0,
            std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
        );
        let finder = BeaconFinder::new(&metadata);
        let mut evidence = BeaconEvidence::default();
        evidence.single_decision = true;
        assert_eq!(beacon_finder::register_css_candidates(&mut evidence,
            ["div".into(), ":is(.a,.b)".into()].into()), Ok(true));
        let nonce = beacon_finder::prepare_beacon(&mut evidence, 1000, 5, 42).unwrap();
        finder.store("https://h/p", "combined", "avif-desktop", &evidence);
        let send = |tail: &str| handle_beacon_evidence(
            &format!("bvg=avif-desktop&oh=combined&n={nonce}&lcpk=unknown{tail}"),
            "https://h/p", "Firefox/147", &server, 1001,
        );
        for rejected in ["", "&cs=unknown", "&cs=%FF", "&cs=div&cs=div", "&cs=div,"] {
            assert_eq!(send(rejected), Some(AcceptOutcome::InvalidReport));
            assert_eq!(finder.load("https://h/p", "combined", "avif-desktop").record().unwrap(), &evidence);
        }
        assert_eq!(send("&cs=%3Ais%28.a%2C.b%29"), Some(AcceptOutcome::Accepted));
        let stored = finder.load("https://h/p", "combined", "avif-desktop").into_record();
        assert_eq!(stored.valid_beacons, 1);
        assert_eq!(stored.generation, 1);
        assert_eq!(stored.critical_css.as_ref().unwrap().matched().unwrap(), &[":is(.a,.b)".into()].into());
        assert!(stored.pending_nonces.is_empty());
        assert_eq!(send("&cs=div"), Some(AcceptOutcome::BadNonce));
        assert_eq!(finder.load("https://h/p", "combined", "avif-desktop").into_record(), stored);
        let mut updated = stored.clone();
        assert_eq!(beacon_finder::register_css_candidates(&mut updated, ["other".into()].into()), Err(CssCandidateError::Settled));
        assert_eq!(updated, stored);
        assert!(finder.load("https://h/p", "combined", "webp-desktop").record().is_none());
    }

    #[test]
    fn font_half_merges_confirmed_triples() {
        use ps_cache::cache::{Cache, KeyState};
        let metadata: std::sync::Arc<dyn ps_cache::cache::Cache> =
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new());
        let server = crate::rewrite::html_context(
            std::sync::Arc::new(ps_cache::cache::MemoryCache::new()),
            std::sync::Arc::clone(&metadata),
            std::sync::Arc::new(ps_stats::core::Statistics::new()),
            0,
            std::sync::Arc::new(crate::rewrite::UnavailableFetcher),
        );
        // Seed a pending font nonce directly.
        let mut nonces = ps_html::font_store::FontNonces::default();
        ps_html::font_store::issue_nonce(&mut nonces, "fontnonce1".to_string(), 1000);
        metadata.put(
            &ps_html::font_store::nonces_key("http://h/p", "oh1", "@Desktop"),
            ps_html::font_store::encode_nonces(&nonces).as_bytes(),
        );
        // Bad nonce rejects without merging.
        handle_beacon_evidence(
            "url=http://h/p&oh=oh1&n=wrong&ff=Manrope%7C700%7Cn",
            "http://h/p",
            "Test/1.0",
            &server,
            1000,
        );
        assert_eq!(
            server
                .statistics()
                .get_variable("preload_critical_fonts_beacon_rejected")
                .map(|variable| variable.get()),
            Some(1)
        );
        // Issued nonce accepts and unions triples (lowercased).
        handle_beacon_evidence(
            "url=http://h/p&oh=oh1&n=fontnonce1&ff=Manrope%7C700%7Cn,Icons%7C400%7Cn",
            "http://h/p",
            "Test/1.0",
            &server,
            1000,
        );
        let blob = match metadata.get(&ps_html::font_store::confirmed_key(
            "http://h/p",
            "oh1",
            "@Desktop",
        )) {
            ps_cache::cache::Lookup {
                state: KeyState::Available,
                value: Some(blob),
            } => String::from_utf8(blob).unwrap(),
            _ => panic!("confirmed blob missing"),
        };
        let confirmed = ps_html::font_store::decode_confirmed(&blob);
        assert_eq!(confirmed.len(), 2);
        assert!(confirmed
            .iter()
            .any(|(family, weight, _)| { family == "manrope" && *weight == 700 }));
        assert_eq!(
            server
                .statistics()
                .get_variable("preload_critical_fonts_beacon_accepted")
                .map(|variable| variable.get()),
            Some(1)
        );
        // Consumed nonces do not validate twice.
        handle_beacon_evidence(
            "url=http://h/p&oh=oh1&n=fontnonce1&ff=Other%7C400%7Cn",
            "http://h/p",
            "Test/1.0",
            &server,
            1000,
        );
        assert_eq!(
            server
                .statistics()
                .get_variable("preload_critical_fonts_beacon_rejected")
                .map(|variable| variable.get()),
            Some(2)
        );
    }
}
