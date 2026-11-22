//! Ports of `AdminSiteTest`, `GraphQueryTest`, and
//! `AdminSiteExpensiveReadsTest` (`admin_site_test.cc`), plus
//! route/JSON/purge-shape coverage for the admin leaves.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ps_admin::access::{AdminAccess, AdminEndpoint};
use ps_admin::graph_query::{
    parse_graph_query, MAX_GRAPH_GRANULARITY_MS, MAX_GRAPH_VAR_TITLES, MIN_GRAPH_GRANULARITY_MS,
};
use ps_admin::history::{browse, filter_log, matches as history_matches};
use ps_admin::log_buffer::LogBuffer;
use ps_admin::purge::{PurgeBackend, PurgeSetBackend, PURGE_SET_FULL};
use ps_admin::site::{
    AdminConfig, AdminRequest, AdminSite, AdminSource, CacheInventory, CacheLookup, ExpensiveLeaf,
};
use ps_cache::base64::{web64_decode, web64_encode};
use ps_cache::purge::PurgeSet;
use ps_cache::timer::MockTimer;
use ps_config::filter::Filter;
use ps_config::message_handler::MessageLevel;
use ps_config::options::RewriteOptions;
use ps_config::scope::Scope;
use ps_http::fetch::{AsyncFetch, BufferingFetch};
use ps_http::google_url::GoogleUrl;
use ps_http::query_params::QueryParams;
use ps_http::request::RequestContext;
use ps_http::response::HttpOptions;
use ps_stats::core::{Statistics, StatisticsLogger};

const NOW_MS: i64 = 1_700_000_000_000;

fn test_fetch(url: &str) -> BufferingFetch {
    BufferingFetch::new(url, RequestContext::new(HttpOptions::default()))
}

fn body_of(fetch: &BufferingFetch) -> String {
    String::from_utf8_lossy(fetch.body()).into_owned()
}

fn query_of(url: &str) -> QueryParams {
    let mut query = QueryParams::new();
    query.parse_from_url(&GoogleUrl::new(url));
    query
}

/// Fixture mirroring `AdminSiteTest::SetUp`.
struct Fixture {
    site: AdminSite,
    log: Arc<Mutex<LogBuffer>>,
    stats: Statistics,
    options: RewriteOptions,
    global: AdminConfig,
    purge_set: PurgeSet,
}

impl Fixture {
    fn new() -> Fixture {
        let timer = Arc::new(Mutex::new(MockTimer::new(NOW_MS)));
        let log = Arc::new(Mutex::new(LogBuffer::new(1 << 20)));
        // Message-dump parsing discards the first potentially partial line.
        log.lock().unwrap().message(
            MessageLevel::Info,
            "Start admin test message buffer.".to_string(),
        );
        let site = AdminSite::new(timer, log.clone());
        Fixture {
            site,
            log,
            stats: Statistics::new(),
            options: RewriteOptions::new(),
            global: AdminConfig::default(),
            purge_set: PurgeSet::new(1 << 20),
        }
    }

    fn message(&self, level: MessageLevel, text: &str) {
        self.log.lock().unwrap().message(level, text.to_string());
    }

    fn request<'a>(&'a self, purge: &'a mut dyn PurgeBackend) -> AdminRequest<'a> {
        AdminRequest {
            options: &self.options,
            global_options: &self.global,
            statistics: &self.stats,
            stats: &self.stats,
            inventory: None,
            show_cache: None,
            purge,
            purge_set: &self.purge_set,
        }
    }

    /// Fetches an admin leaf through `AdminPage`; returns the status code.
    fn fetch_leaf(
        &self,
        path: &str,
        query_string: &str,
        body: &mut String,
        purge: &mut dyn PurgeBackend,
        is_global: bool,
    ) -> i32 {
        let stripped = GoogleUrl::new(&format!("http://localhost{path}"));
        let mut query = QueryParams::new();
        if !query_string.is_empty() {
            query.parse_from_untrusted_string(query_string);
        }
        let mut fetch = test_fetch(&format!("http://localhost{path}"));
        let mut request = self.request(purge);
        self.site.admin_page(
            is_global,
            &stripped,
            &query,
            &self.options,
            &mut fetch,
            &mut request,
        );
        *body = body_of(&fetch);
        fetch.response_headers().status_code()
    }
}

/// Canned statistics-log reader for console/JSON tests.
#[derive(Debug, Default)]
struct FakeLogger {
    json: String,
    recovered: HashMap<String, i64>,
}

impl StatisticsLogger for FakeLogger {
    fn dump_json(
        &self,
        _for_graphs: bool,
        _var_titles: &[String],
        _start_ms: i64,
        _end_ms: i64,
        _granularity_ms: i64,
    ) -> String {
        self.json.clone()
    }

    fn previous_dashboard_counters(&self, out: &mut HashMap<String, i64>) {
        out.extend(self.recovered.clone());
    }
}

/// Records how the console asked for a purge instead of making it.
#[derive(Debug, Default)]
struct RecordingBackend {
    num_purges: usize,
    last_url: String,
    last_allow_eviction: bool,
}

impl PurgeBackend for RecordingBackend {
    fn purge(&mut self, url: &str, _now_ms: i64, allow_eviction: bool) -> Result<(), String> {
        self.num_purges += 1;
        self.last_url = url.to_string();
        self.last_allow_eviction = allow_eviction;
        Ok(())
    }
}

#[test]
fn structured_messages_in_history_page() {
    let fixture = Fixture::new();
    assert!(Arc::ptr_eq(&fixture.log, &fixture.site.log_for_testing()));
    // Due to the size limit to the SharedCircularBuffer, the earliest message
    // in the buffer may be incomplete. In order to always display complete
    // messages on the history page, we simply ignore all the things before the
    // first new line. So here we inject a useless line at the beginning to show
    // that we throw out the first (possibly) incomplete line.
    fixture.message(MessageLevel::Info, "Ignore the first line.");
    fixture.message(MessageLevel::Error, "Test for Errors");
    fixture.message(MessageLevel::Warning, "Test for Warnings");
    fixture.message(MessageLevel::Info, "Test for Infos");
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/message_history");
    fixture.site.message_history_handler(
        &fixture.options,
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        None,
        "",
        "messages",
    );
    let body = body_of(&fetch);
    assert!(
        body.contains("class='pagespeed-message-entry' data-severity='error'"),
        "{body}"
    );
    assert!(
        body.contains("class='pagespeed-message-entry' data-severity='warning'"),
        "{body}"
    );
    assert!(body.contains("Test for Errors"), "{body}");
    assert!(body.contains("Test for Warnings"), "{body}");
}

#[test]
fn archive_read_does_not_require_live_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "ps-admin-restart-{}-{}",
        std::process::id(),
        NOW_MS
    ));
    let history = root.join("history");
    std::fs::create_dir_all(&history).unwrap();
    let payload = "I[Mon, 05 Apr 2010 18:51:26 GMT] [Info] [987] \
         Script https://archive.example/no-live.js didn't shrink.\n";
    let payload = payload.replace("  ", " ");
    let header = format!(
        "PSH1 1 123456789 0 {} 1270493486000 0 0 1 0 0 1270493486000 1 0 0 1 0\n",
        payload.len()
    );
    std::fs::write(history.join("checkpoint"), &header).unwrap();
    std::fs::write(
        history.join("segment-00000000000000000001.psh"),
        format!("{header}{payload}"),
    )
    .unwrap();

    let mut fixture = Fixture::new();
    fixture.global.history_archive = true;
    fixture.global.file_cache_path = root.to_string_lossy().into_owned();
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/javascript");
    let query = QueryParams::new();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/javascript");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut request = fixture.request(&mut backend);
    fixture.site.admin_page(
        true,
        &stripped,
        &query,
        &fixture.options,
        &mut fetch,
        &mut request,
    );
    let body = body_of(&fetch);
    assert!(
        body.contains("https://archive.example/no-live.js"),
        "{body}"
    );
    assert!(
        !body.contains("<section class='pagespeed-history-controls'>"),
        "{body}"
    );
    assert!(!body.contains("Browse history"), "{body}");

    std::fs::remove_file(history.join("checkpoint")).unwrap();
    std::fs::remove_file(history.join("segment-00000000000000000001.psh")).unwrap();
    std::fs::remove_dir(&history).unwrap();
    std::fs::remove_dir(&root).unwrap();
}

fn image_detail(timestamp_ms: i64) -> String {
    format!(
        "PS_IMAGE_DETAIL\t{timestamp_ms}\t10000\t4000\timage/webp\t\
         https://example.com/recent.jpg\t\
         https://example.com/recent.jpg.pagespeed.ic.abc.webp"
    )
}

#[test]
fn dashboard_shows_savings_and_uptime() {
    let fixture = Fixture::new();
    fixture.message(MessageLevel::Info, &image_detail(NOW_MS));
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/dashboard");
    fixture.site.dashboard_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        None,
        None,
    );
    let body = body_of(&fetch);
    for expected in [
        "Performance dashboard",
        "TOTAL DATA SAVED",
        "PAGESPEED UPTIME",
        "JavaScript</span>",
        ">CSS</span>",
        "IMAGE REWRITES",
        "Inspect recent optimized images",
        "Savings by resource",
        "Rewrite activity",
        "Purge cache",
        "cache?purge=*",
        "pagespeed-purge-status",
        "Recent image optimization",
        "https://example.com/recent.jpg",
        "pagespeed-dashboard-stack-image",
        "pagespeed-admin-sidebar",
    ] {
        assert!(body.contains(expected), "missing {expected}\n{body}");
    }
}

#[test]
fn dashboard_shows_html_activity_without_inventing_savings() {
    let fixture = Fixture::new();
    fixture.stats.add_variable("html_cache_inserts").add(4);
    fixture
        .stats
        .add_variable("html_cache_bytes_served")
        .add(4096);
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/dashboard");
    fixture.site.dashboard_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        None,
        None,
    );
    let body = body_of(&fetch);
    assert!(
        body.contains("<span>HTML</span><strong>Not measured</strong>"),
        "{body}"
    );
    assert!(body.contains("Cached HTML served: 4.0 KB"), "{body}");
    assert!(
        body.contains("<strong>HTML</strong><span>4 publications</span>"),
        "{body}"
    );
    assert!(body.contains("pagespeed-dashboard-activity-html"), "{body}");
    assert!(
        body.contains("activity-track'><i style='width:100%"),
        "{body}"
    );
    assert!(
        body.contains("pagespeed-dashboard-total-value'>0 B</div>"),
        "{body}"
    );
    assert!(!body.contains("HTML CACHE HITS"), "{body}");
}

#[test]
fn images_page_parses_emitted_record_format() {
    use ps_html::server_context::ImageRewriteRecord;
    let fixture = Fixture::new();
    // The filter's rendering is the format's single source of truth;
    // the admin parser must consume it verbatim (drift guard).
    let record = ImageRewriteRecord {
        timestamp_ms: NOW_MS,
        original_bytes: 10000,
        optimized_bytes: 4000,
        output_mime_type: "image/avif".to_string(),
        original_url: "https://example.com/a.jpg".to_string(),
        optimized_url: "https://example.com/a.jpg.pagespeed.ic.h.avif".to_string(),
    };
    fixture.message(MessageLevel::Info, &record.message_line());
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/images");
    fixture
        .site
        .images_handler(AdminSource::PageSpeedAdmin, &mut fetch, None, "");
    let body = body_of(&fetch);
    for expected in [
        "https://example.com/a.jpg",
        "image/avif",
        "1</strong><span>recent rewrites</span>",
    ] {
        assert!(body.contains(expected), "missing {expected}\n{body}");
    }
}

#[test]
fn images_page_shows_recorded_rewrite_and_comparison() {
    let fixture = Fixture::new();
    fixture.message(
        MessageLevel::Info,
        &format!(
            "PS_IMAGE_DETAIL\t{NOW_MS}\t10000\t4000\timage/webp\t\
             https://example.com/photo.jpg\t\
             https://example.com/photo.jpg.pagespeed.ic.abc.webp"
        ),
    );
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/images");
    fixture
        .site
        .images_handler(AdminSource::PageSpeedAdmin, &mut fetch, None, "");
    let body = body_of(&fetch);
    for expected in [
        "Recently optimized images",
        "https://example.com/photo.jpg",
        "image/webp",
        "5.8 KB",
        "pagespeed-image-preview-button",
        "pagespeed-image-modal",
        "Image comparison",
    ] {
        assert!(body.contains(expected), "missing {expected}\n{body}");
    }
}

#[test]
fn html_cache_metrics_stay_on_dedicated_page() {
    let fixture = Fixture::new();
    fixture.stats.add_variable("html_cache_hits").add(3);
    fixture.stats.add_variable("html_cache_misses").add(1);
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("No HTML rewrites recorded"), "{body}");
    assert!(body.contains("Hit rate: 75%"), "{body}");
    assert!(body.contains("HTML cache: <strong>Disabled"), "{body}");
    assert!(body.contains("not a live cache inventory"), "{body}");

    let mut dashboard_fetch = test_fetch("http://localhost/pagespeed_admin/dashboard");
    fixture.site.dashboard_handler(
        AdminSource::PageSpeedAdmin,
        &mut dashboard_fetch,
        &fixture.stats,
        None,
        None,
    );
    let dashboard = body_of(&dashboard_fetch);
    assert!(dashboard.contains("href='html_cache'"), "{dashboard}");
    assert!(!dashboard.contains("HTML CACHE HITS"), "{dashboard}");
    assert!(
        !dashboard.contains("<h2>HTML rewrite cache</h2>"),
        "{dashboard}"
    );
    assert!(dashboard.contains("Savings by resource"), "{dashboard}");
}

fn html_detail(bytes: i64, variant: &str, encoded_url: &str) -> String {
    format!(
        "PS_HTML_DETAIL\t1000\t5000\t{bytes}\tmobile\tWebP capable\t\
         optimized-html-v1:{variant}\t{encoded_url}"
    )
}

fn html_bypass(timestamp_ms: i64, reason: &str, encoded_url: &str) -> String {
    format!("PS_HTML_BYPASS\t{timestamp_ms}\t{reason}\t{encoded_url}")
}

#[test]
fn html_cache_records_are_escaped_and_deduplicated() {
    let fixture = Fixture::new();
    let encoded = web64_encode("https://example.com/blog/?x=1&y=<script>".as_bytes());
    for bytes in [1000, 2000] {
        fixture.message(
            MessageLevel::Info,
            &html_detail(bytes, "abcdefghijklmnopqrst", &encoded),
        );
    }
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("data-device='mobile'"), "{body}");
    assert!(body.contains("WebP capable"), "{body}");
    assert!(body.contains("1.9 KB"), "{body}");
    assert!(!body.contains("&y=<script>"), "{body}");
    assert!(body.contains("&amp;y=&lt;script&gt;"), "{body}");
    let first = body.find("<tr data-device=").expect("row");
    assert_eq!(None, body[first + 1..].find("<tr data-device="));
    assert!(body.contains("pagespeed-html-search"), "{body}");
    assert!(body.contains("pagespeed-html-format"), "{body}");
}

#[test]
fn html_cache_cookie_policy_panel_lists_effective_rules() {
    let mut fixture = Fixture::new();
    fixture
        .options
        .parse_and_set_1("HtmlCacheIgnoreCookie", "tracker", Scope::Server)
        .unwrap();
    fixture
        .options
        .parse_and_set_1("HtmlCacheBypassCookie", "session*", Scope::Server)
        .unwrap();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    // Effective policy: built-in list, custom entries, bypass rules.
    for expected in [
        "Cookie allowlist",
        "Common cookies: <strong>Ignored</strong>",
        "<code>_ga</code>",
        "<code>_ga_*</code>",
        "<code>_fbp</code>",
        "<code>_clck</code>",
        "<code>_hjSession*</code>",
        "<code>tracker</code>",
        "<code>session*</code>",
        "must not influence server-generated HTML",
    ] {
        assert!(body.contains(expected), "missing {expected}\n{body}");
    }
    // Disabling the common list shows through.
    fixture
        .options
        .set_option("HtmlCacheIgnoreCommonCookies", "off", Scope::Server)
        .unwrap();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(
        body.contains("Common cookies: <strong>Not ignored</strong>"),
        "{body}"
    );
}

#[test]
fn html_cache_cookie_bypasses_render_newest_first() {
    let fixture = Fixture::new();
    let page = web64_encode("https://example.com/page".as_bytes());
    let shop = web64_encode("https://example.com/shop/?x=1&y=2".as_bytes());
    fixture.message(
        MessageLevel::Info,
        &html_bypass(1000, "cookie:sessionid", &page),
    );
    fixture.message(
        MessageLevel::Info,
        &html_bypass(2000, "malformed-cookie", &shop),
    );
    // Malformed lines never render.
    fixture.message(
        MessageLevel::Info,
        "PS_HTML_BYPASS\t3000\tcookie:broken",
    );
    fixture.message(
        MessageLevel::Info,
        &html_bypass(3000, "bogus-reason", &page),
    );
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("Recent cookie bypasses"), "{body}");
    assert!(body.contains("<code>cookie:sessionid</code>"), "{body}");
    assert!(body.contains("<code>malformed-cookie</code>"), "{body}");
    assert!(!body.contains("bogus-reason"), "{body}");
    assert!(!body.contains("cookie:broken"), "{body}");
    assert!(body.contains("&amp;y=2"), "{body}");
    // Newest first: the malformed row precedes the cookie row.
    let malformed = body.find("malformed-cookie").expect("row");
    let cookie = body.find("cookie:sessionid").expect("row");
    assert!(malformed < cookie, "{body}");
}

#[test]
fn html_cache_bypass_reasons_escape_html() {
    let fixture = Fixture::new();
    let page = web64_encode("https://example.com/".as_bytes());
    fixture.message(
        MessageLevel::Info,
        &html_bypass(1000, "cookie:x<y", &page),
    );
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("<code>cookie:x&lt;y</code>"), "{body}");
    assert!(!body.contains("cookie:x<y"), "{body}");
}

#[test]
fn html_cache_summary_counts_allowlist_accepts() {
    let fixture = Fixture::new();
    fixture
        .stats
        .add_variable("html_cache_cookie_allowlisted")
        .add(7);
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("COOKIE ALLOWLIST ACCEPTS"), "{body}");
    assert!(body.contains("pagespeed-dashboard-value'>7</div>"), "{body}");
}

#[test]
fn history_matches_html_cache_bypass_lines() {
    assert!(history_matches(
        "html_cache",
        "I[date] PS_HTML_BYPASS\t1\tcookie:a\txyz"
    ));
    assert!(history_matches(
        "html_cache",
        "I[date] PS_HTML_DETAIL\t1\t2\t3\td\tf\tk\tu"
    ));
    assert!(!history_matches("html_cache", "I[date] unrelated"));
    assert!(history_matches(
        "messages",
        "PS_HTML_BYPASS\t1\tcookie:a\txyz"
    ));
}

#[test]
fn html_cache_rejects_malformed_and_non_web_records() {
    let fixture = Fixture::new();
    let encoded = web64_encode("javascript:alert(1)".as_bytes());
    fixture.message(
        MessageLevel::Info,
        &format!(
            "PS_HTML_DETAIL\t1000\t5000\t100\tdesktop\tAVIF capable\t\
             optimized-html-v1:abcdefghijklmnopqrst\t{encoded}"
        ),
    );
    fixture.message(MessageLevel::Info, "PS_HTML_DETAIL\tbroken");
    fixture.message(
        MessageLevel::Info,
        "PS_HTML_DETAIL\t1000\t500\t-1\tdesktop\tAVIF capable\t\
         optimized-html-v1:abcdefghijklmnopqrst\tbad-base64",
    );
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("No HTML rewrites recorded"), "{body}");
    assert!(!body.contains("javascript:alert"), "{body}");
}

#[test]
fn html_cache_history_is_bounded_and_newest_first() {
    let fixture = Fixture::new();
    let encoded = web64_encode("https://example.com/blog/".as_bytes());
    for index in 0..105 {
        fixture.message(
            MessageLevel::Info,
            &html_detail(100, &format!("variant-{index:03}"), &encoded),
        );
    }
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/html_cache");
    fixture.site.html_cache_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        &fixture.options,
        None,
        "",
        None,
    );
    let body = body_of(&fetch);
    assert_eq!(100, body.matches("<tr data-device=").count(), "{body}");
    let newest = body.find("optimized-html-v1:variant-104").expect("newest");
    let older = body.find("optimized-html-v1:variant-005").expect("older");
    assert!(newest < older);
    assert!(!body.contains("optimized-html-v1:variant-004"));
}

#[test]
fn existing_diagnostics_are_filtered_without_inventing_rewrite_fields() {
    let fixture = Fixture::new();
    fixture.message(
        MessageLevel::Info,
        "Script https://diagnostic.example/fail.js didn't shrink.",
    );
    fixture.message(
        MessageLevel::Warning,
        "CSS parsing error in https://diagnostic.example/fail.css",
    );
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/javascript");
    fixture.site.message_history_handler(
        &fixture.options,
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        None,
        "",
        "javascript",
    );
    let body = body_of(&fetch);
    assert!(
        body.contains("https://diagnostic.example/fail.js"),
        "{body}"
    );
    assert!(
        !body.contains("https://diagnostic.example/fail.css"),
        "{body}"
    );
    assert!(
        body.contains("not a complete per-resource rewrite history"),
        "{body}"
    );
}

#[test]
fn delete_refused_when_purge_disabled() {
    // Cache purging is disabled by default, so a cache-entry delete is refused
    // rather than performed.
    let fixture = Fixture::new();
    assert!(!fixture.options.enable_cache_purge());
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
    let query =
        query_of("http://localhost/pagespeed_admin/cache?url=http://example.com/x.css&Delete");
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut request = fixture.request(&mut backend);
    fixture.site.admin_page(
        false,
        &stripped,
        &query,
        &fixture.options,
        &mut fetch,
        &mut request,
    );
    assert_eq!(403, fetch.response_headers().status_code());
    let body = body_of(&fetch);
    assert!(body.contains("Deleting not enabled"), "{body}");
    assert!(body.contains("EnableCachePurge"), "{body}");
}

fn parse_query(query: &str) -> ps_admin::graph_query::GraphQuery {
    let mut params = QueryParams::new();
    params.parse_from_untrusted_string(query);
    parse_graph_query(&params, NOW_MS)
}

#[test]
fn graph_defaults_ask_for_the_graphs_over_the_whole_log() {
    let query = parse_query("json=1");
    assert_eq!(0, query.start_time);
    assert_eq!(NOW_MS, query.end_time);
    assert_eq!(3000, query.granularity_ms);
    assert!(query.dump_for_graphs);
    assert!(query.var_titles.is_empty());
}

#[test]
fn graph_granularity_is_held_to_the_bounds() {
    assert_eq!(
        MIN_GRAPH_GRANULARITY_MS,
        parse_query("granularity=0").granularity_ms
    );
    assert_eq!(
        MIN_GRAPH_GRANULARITY_MS,
        parse_query("granularity=1").granularity_ms
    );
    assert_eq!(
        MIN_GRAPH_GRANULARITY_MS,
        parse_query("granularity=-5").granularity_ms
    );
    assert_eq!(
        MIN_GRAPH_GRANULARITY_MS,
        parse_query("granularity=999").granularity_ms
    );
    assert_eq!(1000, parse_query("granularity=1000").granularity_ms);
    assert_eq!(60000, parse_query("granularity=60000").granularity_ms);
    assert_eq!(
        MAX_GRAPH_GRANULARITY_MS,
        parse_query("granularity=999999999999999").granularity_ms
    );
    // Not a number reads as 0, which is held to the lower bound.
    assert_eq!(
        MIN_GRAPH_GRANULARITY_MS,
        parse_query("granularity=abc").granularity_ms
    );
}

#[test]
fn graph_times_are_non_negative_and_not_far_in_the_future() {
    assert_eq!(0, parse_query("start_time=-100").start_time);
    assert_eq!(0, parse_query("end_time=-100").end_time);
    assert_eq!(12345, parse_query("start_time=12345").start_time);
    assert_eq!(
        NOW_MS + 86_400_000,
        parse_query("end_time=99999999999999999").end_time
    );
    assert_eq!(
        NOW_MS + 86_400_000,
        parse_query("start_time=99999999999999999").start_time
    );
    assert_eq!(
        NOW_MS - 1000,
        parse_query("end_time=1699999999000").end_time
    );
}

#[test]
fn graph_variable_names_are_listed_and_bounded() {
    let named = parse_query("var_titles=a,b,,c");
    assert!(!named.dump_for_graphs);
    assert_eq!(3, named.var_titles.len());
    let many = format!(
        "var_titles={}",
        (0..3 * MAX_GRAPH_VAR_TITLES)
            .map(|index| format!("v{index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let capped = parse_query(&many);
    assert!(!capped.dump_for_graphs);
    assert_eq!(MAX_GRAPH_VAR_TITLES, capped.var_titles.len());
    // Several var_titles parameters share the same bound.
    let repeated = (0..3 * MAX_GRAPH_VAR_TITLES)
        .map(|index| format!("var_titles=w{index}"))
        .collect::<Vec<_>>()
        .join("&");
    assert_eq!(
        MAX_GRAPH_VAR_TITLES,
        parse_query(&repeated).var_titles.len()
    );
}

#[test]
fn config_admits_one_read_at_a_time() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut body = String::new();
    assert_eq!(
        200,
        fixture.fetch_leaf(
            "/pagespeed_admin/config",
            "",
            &mut body,
            &mut backend,
            false
        )
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Config, true);
    assert_eq!(
        429,
        fixture.fetch_leaf(
            "/pagespeed_admin/config",
            "",
            &mut body,
            &mut backend,
            false
        )
    );
    assert!(body.contains("busy"), "{body}");
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Config, false);
    assert_eq!(
        200,
        fixture.fetch_leaf(
            "/pagespeed_admin/config",
            "",
            &mut body,
            &mut backend,
            false
        )
    );
}

#[test]
fn message_history_admits_one_read_at_a_time() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut body = String::new();
    assert_eq!(
        200,
        fixture.fetch_leaf(
            "/pagespeed_admin/message_history",
            "",
            &mut body,
            &mut backend,
            false
        )
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Messages, true);
    assert_eq!(
        429,
        fixture.fetch_leaf(
            "/pagespeed_admin/message_history",
            "",
            &mut body,
            &mut backend,
            false
        )
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Messages, false);
}

#[test]
fn graphs_json_admits_one_read_at_a_time() {
    // No console logger in the fixture, so an admitted read answers 404;
    // a refused one answers 429 either way.
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut body = String::new();
    assert_eq!(
        404,
        fixture.fetch_leaf(
            "/pagespeed_admin/graphs",
            "json=1",
            &mut body,
            &mut backend,
            false
        )
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Graphs, true);
    assert_eq!(
        429,
        fixture.fetch_leaf(
            "/pagespeed_admin/graphs",
            "json=1",
            &mut body,
            &mut backend,
            false
        )
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Graphs, false);
}

#[test]
fn cache_lookup_admits_one_read_at_a_time() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::CacheLookup, true);
    let mut body = String::new();
    assert_eq!(
        429,
        fixture.fetch_leaf(
            "/pagespeed_admin/cache",
            "url=http://example.com/x.css",
            &mut body,
            &mut backend,
            false
        )
    );
    assert!(body.contains("busy"), "{body}");
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::CacheLookup, false);
}

#[test]
fn a_completed_cache_lookup_frees_its_slot() {
    // A lookup that runs to completion releases its slot: the next one is
    // admitted, not answered 429.
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut first = String::new();
    assert_ne!(
        429,
        fixture.fetch_leaf(
            "/pagespeed_admin/cache",
            "url=http://example.com/x.css",
            &mut first,
            &mut backend,
            false
        )
    );
    let mut second = String::new();
    assert_ne!(
        429,
        fixture.fetch_leaf(
            "/pagespeed_admin/cache",
            "url=http://example.com/y.css",
            &mut second,
            &mut backend,
            false
        )
    );
}

#[test]
fn cache_status_admits_one_read_at_a_time() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut body = String::new();
    assert_eq!(
        200,
        fixture.fetch_leaf("/pagespeed_admin/cache", "", &mut body, &mut backend, true)
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::CacheStatus, true);
    assert_eq!(
        429,
        fixture.fetch_leaf("/pagespeed_admin/cache", "", &mut body, &mut backend, true)
    );
    // The per-vhost listing takes no slot.
    assert_eq!(
        200,
        fixture.fetch_leaf("/pagespeed_admin/cache", "", &mut body, &mut backend, false)
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::CacheStatus, false);
}

#[test]
fn leaves_do_not_block_each_other() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Config, true);
    let mut body = String::new();
    assert_eq!(
        200,
        fixture.fetch_leaf(
            "/pagespeed_admin/message_history",
            "",
            &mut body,
            &mut backend,
            false
        )
    );
    fixture
        .site
        .set_expensive_slot_busy_for_testing(ExpensiveLeaf::Config, false);
}

#[test]
fn a_busy_slot_is_waited_for_briefly() {
    let fixture = Fixture::new();
    let shared = Arc::new(fixture.site);
    // Move the rest of the fixture out for the fetch below.
    let Fixture {
        stats,
        options,
        global,
        purge_set,
        ..
    } = fixture;
    shared.set_expensive_slot_busy_for_testing(ExpensiveLeaf::Config, true);
    let releaser = shared.clone();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        releaser.release_expensive_slot(ExpensiveLeaf::Config);
    });
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/config");
    let query = QueryParams::new();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/config");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut request = AdminRequest {
        options: &options,
        global_options: &global,
        statistics: &stats,
        stats: &stats,
        inventory: None,
        show_cache: None,
        purge: &mut backend,
        purge_set: &purge_set,
    };
    shared.admin_page(false, &stripped, &query, &options, &mut fetch, &mut request);
    assert_eq!(200, fetch.response_headers().status_code());
    handle.join().unwrap();
}

fn enable_purge(options: &mut RewriteOptions) {
    options
        .set_option("EnableCachePurge", "on", Scope::Server)
        .unwrap();
}

#[test]
fn delete_allowed_when_purge_enabled() {
    let mut fixture = Fixture::new();
    enable_purge(&mut fixture.options);
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
    let query =
        query_of("http://localhost/pagespeed_admin/cache?url=http://example.com/x.css&Delete");
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut request = fixture.request(&mut backend);
    // With purging enabled the delete runs a metadata lookup, not a refusal.
    fixture.site.admin_page(
        false,
        &stripped,
        &query,
        &fixture.options,
        &mut fetch,
        &mut request,
    );
    assert_ne!(403, fetch.response_headers().status_code());
    assert!(!body_of(&fetch).contains("Deleting not enabled"));
}

#[test]
fn vhost_purge_set_lists_own_host_only() {
    // The purge set of a cache path holds every site's records. A per-vhost
    // console lists its own host's records and the whole-cache line; the
    // global console lists them all.
    let mut fixture = Fixture::new();
    assert!(fixture.purge_set.update_global_timestamp(1000));
    assert!(fixture.purge_set.put("http://example.com/own.css", 2000));
    assert!(fixture.purge_set.put("http://EXAMPLE.com/upper.css", 2000));
    assert!(fixture
        .purge_set
        .put("http://example.com:8081/other-port.css", 2000));
    assert!(fixture
        .purge_set
        .put("http://other.example.org/theirs.css?k=v", 2000));

    let stripped = GoogleUrl::new("http://example.com/pagespeed_admin/cache");
    let query = query_of("http://example.com/pagespeed_admin/cache?new_set=");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut vhost_fetch = test_fetch("http://example.com/pagespeed_admin/cache");
    let mut request = fixture.request(&mut backend);
    fixture.site.admin_page(
        false,
        &stripped,
        &query,
        &fixture.options,
        &mut vhost_fetch,
        &mut request,
    );
    assert_eq!(200, vhost_fetch.response_headers().status_code());
    let vhost_body = body_of(&vhost_fetch);
    assert!(vhost_body.contains("Global@"), "{vhost_body}");
    assert!(
        vhost_body.contains("http://example.com/own.css@"),
        "{vhost_body}"
    );
    assert!(vhost_body.contains("upper.css@"), "{vhost_body}");
    assert!(!vhost_body.contains("other-port"), "{vhost_body}");
    assert!(!vhost_body.contains("theirs.css"), "{vhost_body}");

    let mut global_fetch = test_fetch("http://example.com/pagespeed_admin/cache");
    let mut request = fixture.request(&mut backend);
    fixture.site.admin_page(
        true,
        &stripped,
        &query,
        &fixture.options,
        &mut global_fetch,
        &mut request,
    );
    let global_body = body_of(&global_fetch);
    assert!(global_body.contains("Global@"), "{global_body}");
    assert!(global_body.contains("own.css@"), "{global_body}");
    assert!(global_body.contains("other-port.css@"), "{global_body}");
    assert!(global_body.contains("theirs.css"), "{global_body}");
}

#[test]
fn vhost_console_purge_may_not_evict() {
    // A single-URL purge from a per-vhost console must not evict another
    // site's purge record; the whole-server console may.
    let mut fixture = Fixture::new();
    enable_purge(&mut fixture.options);
    let mut recording = RecordingBackend::default();

    let vhost_url = "http://example.com/pagespeed_admin/cache?purge=http://example.com/x.css";
    let vhost_stripped = GoogleUrl::new("http://example.com/pagespeed_admin/cache");
    let vhost_query = query_of(vhost_url);
    let mut vhost_fetch = test_fetch(vhost_url);
    let mut request = fixture.request(&mut recording);
    fixture.site.admin_page(
        false,
        &vhost_stripped,
        &vhost_query,
        &fixture.options,
        &mut vhost_fetch,
        &mut request,
    );
    assert_eq!(1, recording.num_purges);
    assert_eq!("http://example.com/x.css", recording.last_url);
    assert!(!recording.last_allow_eviction);

    let global_url =
        "http://example.com/pagespeed_global_admin/cache?purge=http://example.com/x.css";
    let global_stripped = GoogleUrl::new("http://example.com/pagespeed_global_admin/cache");
    let global_query = query_of(global_url);
    let mut global_fetch = test_fetch(global_url);
    let mut request = fixture.request(&mut recording);
    fixture.site.admin_page(
        true,
        &global_stripped,
        &global_query,
        &fixture.options,
        &mut global_fetch,
        &mut request,
    );
    assert_eq!(2, recording.num_purges);
    assert!(recording.last_allow_eviction);
}

// Route table, JSON shapes, and purge scoping extras below.

#[test]
fn every_leaf_serves_and_unknown_pages_404() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    for leaf in [
        "dashboard",
        "history",
        "images",
        "html_cache",
        "javascript",
        "css",
        "statistics",
        "stats_json",
        "graphs",
        "config",
        "console",
        "message_history",
        "cache",
        "histograms",
    ] {
        let mut body = String::new();
        let status = fixture.fetch_leaf(
            &format!("/pagespeed_admin/{leaf}"),
            "",
            &mut body,
            &mut backend,
            false,
        );
        assert_eq!(200, status, "leaf {leaf}: {body}");
    }
    // The bare handler path redirects so relative links keep working.
    let mut redirect_body = String::new();
    let status = fixture.fetch_leaf(
        "/pagespeed_admin",
        "",
        &mut redirect_body,
        &mut backend,
        false,
    );
    assert_eq!(301, status, "{redirect_body}");
    assert!(
        redirect_body.contains("Redirecting to URL"),
        "{redirect_body}"
    );
    // Unknown leaves name the miss and guess the console root.
    let mut missing_body = String::new();
    let status = fixture.fetch_leaf(
        "/pagespeed_admin/nope",
        "",
        &mut missing_body,
        &mut backend,
        false,
    );
    assert_eq!(404, status, "{missing_body}");
    assert!(
        missing_body.contains("Unknown admin page: "),
        "{missing_body}"
    );
    assert!(
        missing_body.contains("Did you mean to visit: "),
        "{missing_body}"
    );
}

#[test]
fn statistics_page_serves_its_query_shapes() {
    let fixture = Fixture::new();
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let shapes = [
        ("json=1", 404, "console_logger must be enabled"),
        ("config", 200, "ACTIVE SETTINGS"),
        ("histograms", 200, "DISTRIBUTIONS"),
        ("graphs", 200, "Graphs need a little setup"),
        ("cache", 200, "CACHE INSPECTION"),
        ("", 200, "LIVE METRICS"),
    ];
    for (query_string, status, marker) in shapes {
        let mut query = QueryParams::new();
        if !query_string.is_empty() {
            query.parse_from_untrusted_string(query_string);
        }
        let mut fetch = test_fetch("http://localhost/mod_pagespeed_statistics");
        let mut request = fixture.request(&mut backend);
        fixture
            .site
            .statistics_page(false, &query, &fixture.options, &mut fetch, &mut request);
        assert_eq!(
            status,
            fetch.response_headers().status_code(),
            "shape {query_string}"
        );
        assert!(body_of(&fetch).contains(marker), "shape {query_string}");
    }
}

#[test]
fn purge_scope_checks_run_ahead_of_the_enabled_check() {
    let fixture = Fixture::new();
    assert!(!fixture.options.enable_cache_purge());
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    // A whole-cache purge from a per-vhost console is forbidden, not a
    // soft "purging not enabled" failure.
    for (query_string, marker) in [
        (
            "purge=*",
            "global purge is available from the global admin console",
        ),
        (
            "purge=http://other.example.org/x.css",
            "URL belongs to another host; use that host's console",
        ),
        (
            "purge=http://example.com/everything*",
            "global purge is available from the global admin console",
        ),
    ] {
        let stripped = GoogleUrl::new("http://example.com/pagespeed_admin/cache");
        let query = query_of(&format!(
            "http://example.com/pagespeed_admin/cache?{query_string}"
        ));
        let mut fetch = test_fetch("http://example.com/pagespeed_admin/cache");
        let mut request = fixture.request(&mut backend);
        fixture.site.admin_page(
            false,
            &stripped,
            &query,
            &fixture.options,
            &mut fetch,
            &mut request,
        );
        assert_eq!(
            403,
            fetch.response_headers().status_code(),
            "{query_string}"
        );
        assert!(body_of(&fetch).contains(marker), "{query_string}");
    }
}

#[test]
fn purge_edge_cases_answer_plainly() {
    let mut fixture = Fixture::new();
    enable_purge(&mut fixture.options);
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    for (query_string, marker) in [
        ("purge=", "Empty URL"),
        ("purge=http://[invalid", "Invalid URL: "),
    ] {
        let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
        let query = query_of(&format!(
            "http://localhost/pagespeed_admin/cache?{query_string}"
        ));
        let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
        let mut request = fixture.request(&mut backend);
        fixture.site.admin_page(
            true,
            &stripped,
            &query,
            &fixture.options,
            &mut fetch,
            &mut request,
        );
        assert_eq!(
            200,
            fetch.response_headers().status_code(),
            "{query_string}"
        );
        assert!(body_of(&fetch).contains(marker), "{query_string}");
    }
    // A disabled purge names the option that enables it.
    let plain = Fixture::new();
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
    let query = query_of("http://localhost/pagespeed_admin/cache?purge=http://x/y.css");
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
    let mut request = plain.request(&mut backend);
    plain.site.admin_page(
        true,
        &stripped,
        &query,
        &plain.options,
        &mut fetch,
        &mut request,
    );
    let body = body_of(&fetch);
    assert!(body.contains("Purging not enabled"), "{body}");
    assert!(body.contains("EnableCachePurge"), "{body}");
}

#[test]
fn whole_cache_purge_stamps_the_global_timestamp() {
    let mut fixture = Fixture::new();
    enable_purge(&mut fixture.options);
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
    let query = query_of("http://localhost/pagespeed_admin/cache?purge=*");
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
    let mut request = fixture.request(&mut backend);
    fixture.site.admin_page(
        true,
        &stripped,
        &query,
        &fixture.options,
        &mut fetch,
        &mut request,
    );
    assert_eq!(200, fetch.response_headers().status_code());
    assert!(body_of(&fetch).contains("Purge successful"));
    assert_eq!(NOW_MS, backend.purge_set.global_timestamp_ms());
}

#[test]
fn full_purge_set_reports_the_refusal_reason() {
    let mut fixture = Fixture::new();
    enable_purge(&mut fixture.options);
    let mut backend = PurgeSetBackend::new(PurgeSet::new(64));
    assert!(backend.purge_set.put("http://example.com/full.css", NOW_MS));
    // A second record cannot fit without eviction.
    let stripped = GoogleUrl::new("http://example.com/pagespeed_admin/cache");
    let query =
        query_of("http://example.com/pagespeed_admin/cache?purge=http://example.com/other.css");
    let mut fetch = test_fetch("http://example.com/pagespeed_admin/cache");
    let mut request = fixture.request(&mut backend);
    fixture.site.admin_page(
        false,
        &stripped,
        &query,
        &fixture.options,
        &mut fetch,
        &mut request,
    );
    assert_eq!(200, fetch.response_headers().status_code());
    assert!(body_of(&fetch).contains(PURGE_SET_FULL));
}

#[test]
fn console_json_serves_logged_samples() {
    let fixture = Fixture::new();
    fixture.stats.set_console_logger(Arc::new(FakeLogger {
        json: "{\"series\":[1,2]}".to_string(),
        recovered: HashMap::new(),
    }));
    let mut query = QueryParams::new();
    query.parse_from_untrusted_string("json=1");
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/console?json=1");
    fixture
        .site
        .console_json_handler(&query, &mut fetch, &fixture.stats);
    assert_eq!(200, fetch.response_headers().status_code());
    assert_eq!("{\"series\":[1,2]}", body_of(&fetch));
    let content_type = fetch
        .response_headers()
        .headers()
        .lookup1("Content-Type")
        .unwrap();
    assert_eq!("application/json", content_type);
}

#[test]
fn console_gating_names_every_missing_prerequisite() {
    let fixture = Fixture::new();
    let query = QueryParams::new();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/console");
    fixture.site.console_handler(
        &fixture.global,
        &fixture.options,
        AdminSource::PageSpeedAdmin,
        &query,
        &mut fetch,
        &fixture.stats,
    );
    let body = body_of(&fetch);
    assert!(body.contains("Console needs a little setup"), "{body}");
    assert!(body.contains("Statistics is not enabled."), "{body}");
    assert!(body.contains("StatisticsLogging is not enabled."), "{body}");
    assert!(body.contains("LogDir is not set."), "{body}");

    let mut ready = Fixture::new();
    ready.global.statistics_enabled = true;
    ready.global.statistics_logging_enabled = true;
    ready.global.log_dir = "/tmp/logs".to_string();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/console");
    ready.site.console_handler(
        &ready.global,
        &ready.options,
        AdminSource::PageSpeedAdmin,
        &query,
        &mut fetch,
        &ready.stats,
    );
    let body = body_of(&fetch);
    assert!(body.contains("pagespeed.startConsole();"), "{body}");
}

#[test]
fn graphs_page_gates_on_the_console_logger() {
    let fixture = Fixture::new();
    let query = QueryParams::new();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/graphs");
    fixture.site.graphs_handler(
        &fixture.options,
        AdminSource::PageSpeedAdmin,
        &query,
        &mut fetch,
        &fixture.stats,
    );
    assert!(body_of(&fetch).contains("Graphs need a little setup"));

    fixture
        .stats
        .set_console_logger(Arc::new(FakeLogger::default()));
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/graphs");
    fixture.site.graphs_handler(
        &fixture.options,
        AdminSource::PageSpeedAdmin,
        &query,
        &mut fetch,
        &fixture.stats,
    );
    let body = body_of(&fetch);
    assert!(body.contains("pagespeed.Graphs.Start();"), "{body}");
}

#[test]
fn dashboard_recovers_previous_instance_counters() {
    let fixture = Fixture::new();
    let mut recovered = HashMap::new();
    recovered.insert("image_rewrite_total_bytes_saved".to_string(), 2048);
    fixture.stats.set_console_logger(Arc::new(FakeLogger {
        json: String::new(),
        recovered,
    }));
    let global = AdminConfig {
        history_archive: true,
        ..AdminConfig::default()
    };
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/dashboard");
    fixture.site.dashboard_handler(
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
        Some(&global),
        None,
    );
    let body = body_of(&fetch);
    assert!(body.contains("Recovered saved statistics"), "{body}");
    assert!(body.contains("2.0 KB"), "{body}");
}

#[test]
fn cache_listing_renders_inventory_and_purge_placeholders() {
    let fixture = Fixture::new();
    let inventory = CacheInventory {
        http_cache_name: "HTTPCache(Fallback(small=Async(AprMemCache)))".to_string(),
        metadata_cache_name: "LRUCache".to_string(),
        property_store_name: "FileCache".to_string(),
        filesystem_metadata_cache_name: None,
        backend_stats: Some("backend: ok".to_string()),
    };
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
    let query = QueryParams::new();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    let mut request = AdminRequest {
        options: &fixture.options,
        global_options: &fixture.global,
        statistics: &fixture.stats,
        stats: &fixture.stats,
        inventory: Some(&inventory),
        show_cache: None,
        purge: &mut backend,
        purge_set: &fixture.purge_set,
    };
    fixture.site.admin_page(
        true,
        &stripped,
        &query,
        &fixture.options,
        &mut fetch,
        &mut request,
    );
    let body = body_of(&fetch);
    for expected in [
        "CACHE INSPECTION",
        "Metadata lookup",
        "pagespeed-cache-lookup",
        "Async AprMemCache",
        "FileSystem Metadata Cache",
        "backend: ok",
        "id='purge_global'",
        "id='purge_table'",
        "pagespeed.Caches.Start();",
    ] {
        assert!(body.contains(expected), "missing {expected}\n{body}");
    }
}

#[test]
fn cache_lookup_reports_through_the_hook() {
    let fixture = Fixture::new();
    let show_cache = |url: &str, user_agent: &str, delete: bool, as_json: bool| {
        assert_eq!("http://example.com/x.css", url);
        assert_eq!("TestAgent/1.0", user_agent);
        assert!(delete);
        assert!(as_json);
        CacheLookup {
            status: 200,
            content_type: "application/json".to_string(),
            body: "{\"entry\":\"x\"}".to_string(),
        }
    };
    let stripped = GoogleUrl::new("http://localhost/pagespeed_admin/cache");
    let query = query_of(
        "http://localhost/pagespeed_admin/cache?url=http://example.com/x.css\
         &user_agent=TestAgent/1.0&Delete&json=1",
    );
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/cache");
    let mut backend = PurgeSetBackend::new(PurgeSet::new(1 << 20));
    // Deleting requires purge rights even though a hook answers the lookup.
    let mut options = RewriteOptions::new();
    enable_purge(&mut options);
    let mut request = AdminRequest {
        options: &options,
        global_options: &fixture.global,
        statistics: &fixture.stats,
        stats: &fixture.stats,
        inventory: None,
        show_cache: Some(&show_cache),
        purge: &mut backend,
        purge_set: &fixture.purge_set,
    };
    fixture
        .site
        .admin_page(false, &stripped, &query, &options, &mut fetch, &mut request);
    assert_eq!(200, fetch.response_headers().status_code());
    assert_eq!("{\"entry\":\"x\"}", body_of(&fetch));
}

#[test]
fn history_controls_render_the_browse_form() {
    let mut fixture = Fixture::new();
    fixture.global.history_archive = true;
    fixture.global.file_cache_path = std::env::temp_dir().to_string_lossy().into_owned();
    let mut query = QueryParams::new();
    query.parse_from_untrusted_string("from=2024-01-01&until=not-a-date");
    let (controls, stored) =
        fixture
            .site
            .history_controls("messages", &query, &fixture.global, None);
    assert!(controls.contains("Browse history"), "{controls}");
    assert!(controls.contains("Invalid UTC date"), "{controls}");
    assert!(stored.is_some());
}

#[test]
fn message_history_reports_a_disabled_buffer() {
    let timer = Arc::new(Mutex::new(MockTimer::new(NOW_MS)));
    let log = Arc::new(Mutex::new(LogBuffer::disabled()));
    let site = AdminSite::new(timer, log);
    let options = RewriteOptions::new();
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/message_history");
    site.message_history_handler(
        &options,
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        None,
        "",
        "messages",
    );
    let body = body_of(&fetch);
    assert!(
        body.contains("Writing to mod_pagespeed_message failed."),
        "{body}"
    );
    assert!(body.contains("MessageBufferSize is not set to 0"), "{body}");
}

#[test]
fn history_archive_helpers_match_their_contracts() {
    assert!(history_matches("messages", "anything"));
    assert!(history_matches("images", "x PS_IMAGE_DETAIL\t rest"));
    assert!(!history_matches("images", "nothing here"));
    assert!(history_matches("css", "CSS parsing error"));
    assert!(!history_matches("bogus", "CSS parsing error"));
    // Unbounded ranges pass the log through untouched.
    assert_eq!("abc", filter_log("abc", 0, 253402300800000));
    // A missing checkpoint reports no snapshots yet.
    let page = browse(
        "/nonexistent-cache-path",
        "messages",
        0,
        253402300800000,
        "",
    );
    assert_eq!("No snapshots have been created yet", page.error);
    let missing = web64_decode("bad-base64!!");
    assert!(missing.is_none());
}

#[test]
fn admin_access_groups_gate_each_endpoint_family() {
    let open = AdminAccess::default();
    assert!(open.allowed(AdminEndpoint::Console, "example.com"));
    assert!(!open.allowed(AdminEndpoint::Console, ""));
    let mut gated = AdminAccess::default();
    gated.global_admin.allow("ops.example.com");
    assert!(gated.allowed(AdminEndpoint::GlobalAdmin, "ops.example.com"));
    assert!(!gated.allowed(AdminEndpoint::GlobalAdmin, "example.com"));
    assert!(gated.allowed(AdminEndpoint::Admin, "example.com"));
}

#[test]
fn debug_assets_serve_unminified_builds() {
    let mut fixture = Fixture::new();
    fixture.options.filters_mut().enable(Filter::Debug);
    let mut fetch = test_fetch("http://localhost/pagespeed_admin/statistics");
    fixture.site.statistics_handler(
        &fixture.options,
        AdminSource::PageSpeedAdmin,
        &mut fetch,
        &fixture.stats,
    );
    // The debug bundle is the large unminified closure output.
    assert!(body_of(&fetch).len() > 100_000);
}
