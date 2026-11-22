//! Admin console (`AdminSite`).
//!
//! Serves `/pagespeed_admin/*` leaves and legacy statistics
//! queries: dashboard, history, images, HTML cache,
//! diagnostics, statistics, config, histograms, caches,
//! console, message history, and graphs, plus JSON endpoints
//! and cache purge/delete actions.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ps_cache::purge::PurgeSet;
use ps_cache::timer::MockTimer;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_html::keywords::escape as html_escape_bytes;
use ps_http::fetch::AsyncFetch;
use ps_http::google_url::{GoogleUrl, PORT_UNSPECIFIED};
use ps_http::query_params::QueryParams;
use ps_stats::core::Statistics;

use crate::assets::{script, stylesheet};
use crate::graph_query::parse_graph_query;
use crate::history::{browse_with_live, filter_log, matches as history_matches};
use crate::log_buffer::LogBuffer;
use crate::purge::PurgeBackend;

/// Leaf identifiers for admin routing.
const LEAF_DASHBOARD: &str = "dashboard";
const LEAF_HISTORY: &str = "history";
const LEAF_IMAGES: &str = "images";
const LEAF_HTML_CACHE: &str = "html_cache";
const LEAF_JAVASCRIPT: &str = "javascript";
const LEAF_CSS: &str = "css";
const LEAF_STATISTICS: &str = "statistics";
const LEAF_STATS_JSON: &str = "stats_json";
const LEAF_GRAPHS: &str = "graphs";
const LEAF_CONFIG: &str = "config";
const LEAF_CONSOLE: &str = "console";
const LEAF_MESSAGE_HISTORY: &str = "message_history";
const LEAF_CACHE: &str = "cache";
const LEAF_HISTOGRAMS: &str = "histograms";

/// How long a request waits for a busy slot (`kExpensiveSlotWaitMs`).
pub const EXPENSIVE_SLOT_WAIT_MS: u64 = 100;

/// Costly admin reads, each admitted one at a time
/// (`ExpensiveLeaf`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpensiveLeaf {
    /// Graphs/console JSON.
    Graphs,
    /// Configuration dump.
    Config,
    /// Plain message history.
    Messages,
    /// Cache metadata lookup.
    CacheLookup,
    /// Global cache listing.
    CacheStatus,
}

impl ExpensiveLeaf {
    /// Slot index.
    fn index(self) -> usize {
        match self {
            ExpensiveLeaf::Graphs => 0,
            ExpensiveLeaf::Config => 1,
            ExpensiveLeaf::Messages => 2,
            ExpensiveLeaf::CacheLookup => 3,
            ExpensiveLeaf::CacheStatus => 4,
        }
    }
}

/// Entry point kind (`AdminSource`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminSource {
    /// `/pagespeed_admin` console.
    PageSpeedAdmin,
    /// Legacy `/*_pagespeed_statistics` page.
    Statistics,
    /// Anything else.
    Other,
}

/// System-level admin knobs (`SystemRewriteOptions` subset).
#[derive(Debug, Clone, Default)]
pub struct AdminConfig {
    /// Retain message history on disk.
    pub history_archive: bool,
    /// File cache root holding `history/`.
    pub file_cache_path: String,
    /// Snapshot interval, seconds.
    pub history_archive_interval: i64,
    /// Retention, days.
    pub history_archive_retention_days: i64,
    /// Storage cap, MiB.
    pub history_archive_max_size_mb: i64,
    /// Statistics collection on.
    pub statistics_enabled: bool,
    /// Statistics logging on.
    pub statistics_logging_enabled: bool,
    /// Statistics log directory.
    pub log_dir: String,
}

/// Cache backends for the caches leaf.
#[derive(Debug, Default)]
pub struct CacheInventory {
    /// HTTP cache descriptor.
    pub http_cache_name: String,
    /// Metadata cache descriptor.
    pub metadata_cache_name: String,
    /// Property-store descriptor.
    pub property_store_name: String,
    /// Filesystem metadata cache descriptor, if any.
    pub filesystem_metadata_cache_name: Option<String>,
    /// Pre-rendered backend statistics, if any.
    pub backend_stats: Option<String>,
}

/// Metadata lookup result for `cache?url=` (`ShowCacheHandler`).
#[derive(Debug, Clone)]
pub struct CacheLookup {
    /// HTTP status to serve.
    pub status: u16,
    /// Content type to serve.
    pub content_type: String,
    /// Response body.
    pub body: String,
}

/// Per-request admin collaborators.
pub struct AdminRequest<'a> {
    /// Rewrite options (purge/delete gating, debug assets, HTML flags).
    pub options: &'a RewriteOptions,
    /// System admin knobs.
    pub global_options: &'a AdminConfig,
    /// Live statistics.
    pub statistics: &'a Statistics,
    /// Display statistics (usually the same object).
    pub stats: &'a Statistics,
    /// Cache backends, when the server wires them.
    pub inventory: Option<&'a CacheInventory>,
    /// Metadata lookup for `cache?url=` (renders a notice when
    /// absent): `(url, user_agent, delete, as_json)`.
    pub show_cache: Option<&'a dyn Fn(&str, &str, bool, bool) -> CacheLookup>,
    /// Purge sink.
    pub purge: &'a mut dyn PurgeBackend,
    /// Purge-set records for `new_set=`.
    pub purge_set: &'a PurgeSet,
}

/// Admin console (`AdminSite`).
pub struct AdminSite {
    timer: Arc<Mutex<MockTimer>>,
    log: Arc<Mutex<LogBuffer>>,
    start_time_ms: i64,
    expensive_in_flight: [AtomicBool; 5],
}

impl AdminSite {
    /// Builds a console over a clock and a message buffer.
    #[must_use]
    pub fn new(timer: Arc<Mutex<MockTimer>>, log: Arc<Mutex<LogBuffer>>) -> Self {
        let start_time_ms = timer.lock().map(|timer| timer.now_ms()).unwrap_or(0);
        AdminSite {
            timer,
            log,
            start_time_ms,
            expensive_in_flight: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
        }
    }

    /// Borrows the message buffer for tests
    /// (`MessageHandlerForTesting`).
    #[must_use]
    pub fn log_for_testing(&self) -> Arc<Mutex<LogBuffer>> {
        self.log.clone()
    }

    /// Current time.
    fn now_ms(&self) -> i64 {
        self.timer.lock().map(|timer| timer.now_ms()).unwrap_or(0)
    }

    /// Frees a slot (`ReleaseExpensiveSlot`).
    pub fn release_expensive_slot(&self, leaf: ExpensiveLeaf) {
        self.expensive_in_flight[leaf.index()].store(false, Ordering::Release);
    }

    /// Marks a slot taken or free (tests).
    pub fn set_expensive_slot_busy_for_testing(&self, leaf: ExpensiveLeaf, busy: bool) {
        self.expensive_in_flight[leaf.index()].store(busy, Ordering::Release);
    }

    /// Reads a dashboard counter, live plus recovered
    /// (`DashboardCounter`).
    fn dashboard_counter(stats: &Statistics, name: &str, previous: &BTreeMap<String, i64>) -> i64 {
        let live = stats
            .get_variable(name)
            .map(|variable| variable.get())
            .or_else(|| stats.get_up_down_counter(name).map(|counter| counter.get()))
            .unwrap_or(0)
            .max(0);
        live + previous.get(name).copied().unwrap_or(0)
    }

    /// Recovers previous-instance counters
    /// (`PreviousDashboardCounters`).
    fn previous_dashboard_counters(
        stats: &Statistics,
        options: Option<&AdminConfig>,
    ) -> BTreeMap<String, i64> {
        let mut previous = BTreeMap::new();
        if options.is_some_and(|options| options.history_archive) {
            if let Some(logger) = stats.console_logger() {
                let mut recovered = std::collections::HashMap::new();
                logger.previous_dashboard_counters(&mut recovered);
                previous.extend(recovered);
            }
        }
        previous
    }

    /// Displays the runtime/savings summary (`DashboardHandler`).
    pub fn dashboard_handler(
        &self,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
        archive_options: Option<&AdminConfig>,
        recent_images: Option<&str>,
    ) {
        open_admin_html("dashboard", "", source, self.now_ms(), fetch);
        let previous = Self::previous_dashboard_counters(stats, archive_options);
        let javascript_saved =
            Self::dashboard_counter(stats, "javascript_total_bytes_saved", &previous).max(0);
        let css_saved =
            Self::dashboard_counter(stats, "css_filter_total_bytes_saved", &previous).max(0);
        let image_saved =
            Self::dashboard_counter(stats, "image_rewrite_total_bytes_saved", &previous).max(0);
        let total_saved = javascript_saved + css_saved + image_saved;
        let javascript_rewrites =
            Self::dashboard_counter(stats, "javascript_blocks_minified", &previous);
        let css_rewrites = Self::dashboard_counter(stats, "css_filter_blocks_rewritten", &previous);
        let image_rewrites = Self::dashboard_counter(stats, "image_rewrites", &previous);
        let total_rewrites = javascript_rewrites + css_rewrites + image_rewrites;
        let html_publications = Self::dashboard_counter(stats, "html_cache_inserts", &previous);
        let html_served = Self::dashboard_counter(stats, "html_cache_bytes_served", &previous);
        let activity_total = total_rewrites + html_publications;
        let uptime = format_dashboard_uptime(self.now_ms() - self.start_time_ms);
        let javascript_percent = dashboard_percent(javascript_saved, total_saved);
        let css_percent = dashboard_percent(css_saved, total_saved);
        let image_percent = dashboard_percent(image_saved, total_saved);

        fetch.handle_write(
            b"<section class='pagespeed-dashboard'><div class='pagespeed-dashboard-heading'><div><p class='pagespeed-dashboard-eyebrow'>LIVE OVERVIEW</p><h1>Performance dashboard</h1><p class='pagespeed-dashboard-description'>Live optimization counters and recent image history for this PageSpeed instance.</p></div><div class='pagespeed-dashboard-controls'><span class='pagespeed-dashboard-live'><i></i> INSTANCE ACTIVE</span><a class='pagespeed-history-compact-link' href='history'>Archive status</a><button type='button' class='pagespeed-purge-button' id='pagespeed-purge-cache'>Purge cache</button></div></div><p class='pagespeed-purge-status' id='pagespeed-purge-status' role='status' aria-live='polite'></p><div class='pagespeed-dashboard-grid'><article class='pagespeed-dashboard-card pagespeed-dashboard-total'><div class='pagespeed-dashboard-card-label'>TOTAL DATA SAVED</div>",
        );
        if !previous.is_empty() {
            fetch.handle_write(
                b"<div class='pagespeed-dashboard-card-note'>Recovered saved statistics + current run. Sampled history excludes unsaved activity; legacy reset boundaries may be incomplete.</div>",
            );
        }
        fetch.handle_write(
            format!(
                "<div class='pagespeed-dashboard-total-value'>{}</div>",
                format_dashboard_bytes(total_saved)
            )
            .as_bytes(),
        );
        fetch.handle_write(
            b"<div class='pagespeed-dashboard-card-note'>Combined savings from JavaScript, CSS, and image rewrites</div></article><article class='pagespeed-dashboard-card pagespeed-dashboard-uptime'><div class='pagespeed-dashboard-card-label'>PAGESPEED UPTIME</div>",
        );
        fetch.handle_write(
            format!("<div class='pagespeed-dashboard-total-value'>{uptime}</div>").as_bytes(),
        );
        fetch.handle_write(
            format!(
                "<div class='pagespeed-dashboard-card-note'>Since this admin instance initialized</div></article><article class='pagespeed-dashboard-card'><div class='pagespeed-dashboard-card-label'>SUCCESSFUL REWRITES</div><div class='pagespeed-dashboard-value'>{total_rewrites}</div><div class='pagespeed-dashboard-card-note'>Across image, CSS, and JavaScript resources</div></article><article class='pagespeed-dashboard-card'><div class='pagespeed-dashboard-card-label'>IMAGE REWRITES</div><a class='pagespeed-dashboard-image-link' href='images'><div class='pagespeed-dashboard-value'>{image_rewrites}</div><span>Inspect recent optimized images</span></a></article></div>"
            )
            .as_bytes(),
        );

        fetch.handle_write(
            b"<div class='pagespeed-dashboard-panels'><section class='pagespeed-dashboard-panel'><div class='pagespeed-dashboard-panel-heading'><div><h2>Savings by resource</h2><p>Available saved statistics plus current activity</p></div><span class='pagespeed-dashboard-period'>RECORDED</span></div><div class='pagespeed-dashboard-chart' role='img' aria-label='Savings by resource: JavaScript, CSS, and images'><div class='pagespeed-dashboard-stack'><span class='pagespeed-dashboard-stack-js' style='width:",
        );
        fetch.handle_write(
            format!(
                "{javascript_percent}%'></span><span class='pagespeed-dashboard-stack-css' style='width:{css_percent}%'></span><span class='pagespeed-dashboard-stack-image' style='width:{image_percent}%'></span></div>"
            )
            .as_bytes(),
        );
        for (label, class, value, percent) in [
            ("JavaScript", "js", javascript_saved, javascript_percent),
            ("CSS", "css", css_saved, css_percent),
            ("Images", "image", image_saved, image_percent),
        ] {
            fetch.handle_write(
                format!(
                    "<div class='pagespeed-dashboard-breakdown-row'><span class='pagespeed-dashboard-legend-dot pagespeed-dashboard-legend-{class}'></span><span>{label}</span><strong>{}</strong><small>{percent}%</small></div>",
                    format_dashboard_bytes(value),
                )
                .as_bytes(),
            );
        }
        fetch.handle_write(
            b"<div class='pagespeed-dashboard-breakdown-row'><span class='pagespeed-dashboard-legend-dot pagespeed-dashboard-legend-html'></span><span>HTML</span><strong>Not measured</strong><small aria-label='Savings percentage unavailable'>&mdash;</small></div>",
        );
        fetch.handle_write(
            format!(
                "<p class='pagespeed-dashboard-footnote'>Cached HTML served: {}. HTML input/output savings are not recorded; cache-served bytes are excluded from savings totals and percentages.</p>",
                format_dashboard_bytes(html_served),
            )
            .as_bytes(),
        );
        fetch.handle_write(
            b"<p class='pagespeed-dashboard-footnote'>Savings include available saved statistics and current counters. They represent optimized resource bytes, not a measurement of all network traffic.</p></section><section class='pagespeed-dashboard-panel'><div class='pagespeed-dashboard-panel-heading'><div><h2>Rewrite activity</h2><p>Resource rewrites and HTML cache publications</p></div></div><div class='pagespeed-dashboard-activity'>",
        );
        for (label, class, icon, value, unit) in [
            ("Images", "image", "IM", image_rewrites, "rewrites"),
            ("JavaScript", "js", "JS", javascript_rewrites, "rewrites"),
            ("CSS", "css", "#", css_rewrites, "rewrites"),
            ("HTML", "html", "HT", html_publications, "publications"),
        ] {
            let percent = dashboard_percent(value, activity_total);
            fetch.handle_write(
                format!(
                    "<div class='pagespeed-dashboard-activity-row'><span class='pagespeed-dashboard-activity-icon pagespeed-dashboard-activity-{class}'>{icon}</span><div class='pagespeed-dashboard-activity-main'><div><strong>{label}</strong><span>{value} {unit}</span></div><span class='pagespeed-dashboard-activity-track'><i style='width:{percent}%'></i></span></div></div>"
                )
                .as_bytes(),
            );
        }
        fetch.handle_write(
            b"</div></section></div><section class='pagespeed-dashboard-panel pagespeed-dashboard-recent'><div class='pagespeed-dashboard-panel-heading'><div><h2>Recent image optimization</h2><p>",
        );
        fetch.handle_write(
            if recent_images.is_some() {
                "Latest image records from live memory and retained archive, when available."
            } else {
                "Latest image assets rewritten by PageSpeed."
            }
            .as_bytes(),
        );
        fetch.handle_write(b"</p></div><a href='images'>Browse image history &rarr;</a></div>");
        let log;
        if let Some(stored) = recent_images {
            log = stored.to_string();
        } else {
            log = self.log.lock().map(|log| log.dump()).unwrap_or_default();
        }
        let mut recent_rows = Vec::new();
        read_recent_image_rewrites(&log, 4, &mut recent_rows);
        if recent_rows.is_empty() {
            fetch.handle_write(
                if recent_images.is_some() {
                    "<div class='pagespeed-dashboard-recent-empty'>No image rewrite records were found in the current buffer or retained archive.</div>"
                } else {
                    "<div class='pagespeed-dashboard-recent-empty'>Successful image rewrites will appear here while their entries remain in the bounded message buffer.</div>"
                }
                .as_bytes(),
            );
        } else {
            fetch.handle_write(
                b"<div class='pagespeed-dashboard-recent-wrap'><table class='pagespeed-admin-data-table pagespeed-dashboard-recent-table'><thead><tr><th>IMAGE</th><th>FORMAT</th><th>BEFORE</th><th>AFTER</th><th>SAVED</th><th>TIME</th></tr></thead><tbody>",
            );
            for row in &recent_rows {
                let saved = row.original_bytes - row.optimized_bytes;
                fetch.handle_write(
                    format!(
                        "<tr><td class='pagespeed-dashboard-recent-url' title='{}'>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td class='pagespeed-dashboard-recent-saved'>{}</td><td><time class='pagespeed-image-time' data-timestamp-ms='{}'>Loading date...</time></td></tr>",
                        escape_html(&row.original_url),
                        escape_html(&row.original_url),
                        escape_html(&row.output_mime_type),
                        format_dashboard_bytes(row.original_bytes),
                        format_dashboard_bytes(row.optimized_bytes),
                        format_dashboard_bytes(saved),
                        row.timestamp_ms,
                    )
                    .as_bytes(),
                );
            }
            fetch.handle_write(b"</tbody></table></div>");
        }
        fetch.handle_write(
            b"</section></section><script>(function(){var e=document.querySelectorAll('.pagespeed-image-time');for(var i=0;i<e.length;i++){var t=Number(e[i].getAttribute('data-timestamp-ms'));if(t>0){e[i].textContent=new Date(t).toLocaleString();}}var b=document.getElementById('pagespeed-purge-cache'),s=document.getElementById('pagespeed-purge-status');b.addEventListener('click',function(){if(!window.confirm('Purge the entire PageSpeed cache? Cached resources will be regenerated as pages are requested.'))return;b.disabled=true;s.textContent='Purging cache...';fetch('cache?purge=*',{credentials:'same-origin',cache:'no-store'}).then(function(r){return r.text().then(function(t){if(!r.ok)throw new Error(t||'HTTP '+r.status);return t;});}).then(function(t){s.textContent=t;if(t.replace(/^\\s+|\\s+$/g,'')!=='Purge successful'){s.setAttribute('data-result','error');}else{s.setAttribute('data-result','success');}}).catch(function(){s.textContent='Cache purge request failed.';s.setAttribute('data-result','error');}).then(function(){b.disabled=false;});});})();</script>",
        );
        close_admin_html(fetch);
    }

    /// Displays archive configuration and health
    /// (`HistoryArchiveHandler`).
    pub fn history_archive_handler(
        &self,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        options: &AdminConfig,
    ) {
        open_admin_html("history", "", source, self.now_ms(), fetch);
        fetch
            .response_headers_mut()
            .set_date_and_caching(self.now_ms(), 0, ", private, no-store");
        write_heading(
            fetch,
            "STORAGE & RETENTION",
            "Historical Archive",
            "Monitor snapshots, archive capacity, and any gaps in retained history.",
        );
        self.write_history_health(fetch, options);
        close_admin_html(fetch);
    }

    /// Writes archive health cards (`WriteHistoryHealth`). Live
    /// shared-buffer health stays with the server layer; without
    /// it the page reports configuration plus the unavailable
    /// notice, like C++.
    fn write_history_health(&self, fetch: &mut dyn AsyncFetch, options: &AdminConfig) {
        if options.history_archive {
            fetch.handle_write(
                b"<section class='pagespeed-history-controls' role='alert'>Archive source health is unavailable. Verify MessageBufferSize and that the module and PSOL builds match.</section>",
            );
        }
        fetch.handle_write(b"<section class='pagespeed-dashboard-grid pagespeed-history-cards'>");
        let interval = if options.history_archive_interval == 1 {
            "1 second".to_string()
        } else {
            format!("{} seconds", options.history_archive_interval)
        };
        for (label, value) in [
            (
                "Archive status",
                if options.history_archive {
                    "Enabled".to_string()
                } else {
                    "Disabled".to_string()
                },
            ),
            ("Snapshot interval", interval),
            (
                "Retention",
                format!("{} days", options.history_archive_retention_days),
            ),
            (
                "Storage limit",
                format!("{} MiB", options.history_archive_max_size_mb),
            ),
        ] {
            fetch.handle_write(
                format!(
                    "<article class='pagespeed-dashboard-card'><div class='pagespeed-dashboard-card-label'>{label}</div><div class='pagespeed-dashboard-value'>{value}</div></article>"
                )
                .as_bytes(),
            );
        }
        let next = if options.history_archive {
            "Awaiting scheduler"
        } else {
            "Not scheduled"
        };
        fetch.handle_write(
            format!(
                "</section><section class='pagespeed-dashboard-panel'><div class='pagespeed-dashboard-panel-heading'><div><h2>Archive health</h2><p>Shared-buffer snapshots run in the background</p></div></div><div class='pagespeed-history-status'><p>Last successful snapshot: No successful snapshot yet</p><p>Next scheduled snapshot: {next}</p><p>Archive disk usage at last successful snapshot: 0 bytes. Archived records (cumulative): 0. Currently retained records: 0. Detected missing bytes: 0. Uncertain restart/reset gaps: 0. Archive failures: 0.</p><p>Gap counts are source-wide; the types and number of overwritten events cannot be reconstructed. Restarts may lose an unknown unsaved tail. These are cumulative classifications, not counts of currently retained records. A single message may match more than one history view.</p><table class='pagespeed-admin-data-table'><thead><tr><th>View</th><th>Cumulative archived matches</th></tr></thead><tbody><tr><td>Messages</td><td>0</td></tr><tr><td>Images</td><td>0</td></tr><tr><td>HTML cache</td><td>0</td></tr><tr><td>JavaScript diagnostics</td><td>0</td></tr><tr><td>CSS diagnostics</td><td>0</td></tr></tbody></table></div></section>"
            )
            .as_bytes(),
        );
    }

    /// Lists recently rewritten images (`ImagesHandler`).
    pub fn images_handler(
        &self,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        stored: Option<&str>,
        controls: &str,
    ) {
        let log;
        if let Some(stored) = stored {
            log = stored.to_string();
        } else {
            log = self.log.lock().map(|log| log.dump()).unwrap_or_default();
        }
        let mut rows = Vec::new();
        read_recent_image_rewrites(&log, 100, &mut rows);
        let total_saved: i64 = rows
            .iter()
            .map(|row| row.original_bytes - row.optimized_bytes)
            .sum();

        open_admin_html("images", "", source, self.now_ms(), fetch);
        fetch.handle_write(controls.as_bytes());
        fetch.handle_write(
            format!(
                "<section class='pagespeed-images-page'><div class='pagespeed-dashboard-heading'><p class='pagespeed-dashboard-eyebrow'>IMAGE OPTIMIZATION</p><h1>Recently optimized images</h1><p class='pagespeed-dashboard-description'>{}</p></div><div class='pagespeed-images-summary'>",
                if stored.is_some() {
                    "Recent rewrites merged from memory with retained history."
                } else {
                    "Recent successful rewrites recorded in the PageSpeed message buffer."
                }
            )
            .as_bytes(),
        );
        fetch.handle_write(
            format!(
                "<strong>{}</strong><span>recent rewrites</span><strong>{}</strong><span>saved in this list</span></div>",
                rows.len(),
                format_dashboard_bytes(total_saved),
            )
            .as_bytes(),
        );

        if rows.is_empty() {
            fetch.handle_write(
                if stored.is_some() {
                    "<div class='pagespeed-images-empty'><h2>No archived images match</h2><p>Try another date range or an older page.</p></div></section>"
                } else {
                    "<div class='pagespeed-images-empty'><h2>No image rewrites found</h2><p>Successful image rewrites will appear here. The message buffer must be enabled, and this list only includes entries still retained in that bounded buffer.</p></div></section>"
                }
                .as_bytes(),
            );
        } else {
            fetch.handle_write(
                b"<div class='pagespeed-images-toolbar'><label class='pagespeed-images-search-wrap'><input type='search' id='pagespeed-images-search' placeholder='Search image URL or format' aria-label='Search image URL or format'></label><div><label class='pagespeed-images-filter-label'>Format <select id='pagespeed-images-format' aria-label='Filter by format'><option value='all'>All formats</option><option value='image/avif'>AVIF</option><option value='image/webp'>WebP</option><option value='image/jpeg'>JPEG</option><option value='image/png'>PNG</option></select></label><label class='pagespeed-images-filter-label'>Sort <select id='pagespeed-images-sort' aria-label='Sort images'><option value='newest'>Newest first</option><option value='saved'>Most saved</option></select></label></div></div><div class='pagespeed-images-table-wrap'><table class='pagespeed-admin-data-table pagespeed-images-table'><thead><tr><th>Date</th><th>Original</th><th>Format</th><th>Before</th><th>After</th><th>Saved</th><th>Preview</th></tr></thead><tbody id='pagespeed-images-rows'>",
            );
            for row in &rows {
                let saved = row.original_bytes - row.optimized_bytes;
                fetch.handle_write(
                    format!(
                        "<tr data-format='{}' data-timestamp-ms='{}' data-saved-bytes='{}'><td><time class='pagespeed-image-time' data-timestamp-ms='{}'>Loading date...</time></td><td class='pagespeed-image-url'><a href='{}' target='_blank' rel='noopener noreferrer' referrerpolicy='no-referrer' title='Open original image'>{}</a></td><td><code>{}</code></td><td>{}</td><td>{}</td><td class='pagespeed-image-saved'>{}</td><td><button type='button' class='pagespeed-image-preview-button' data-original-url='{}' data-optimized-url='{}' data-format='{}' data-original-bytes='{}' data-optimized-bytes='{}'>Compare</button></td></tr>",
                        escape_html(&row.output_mime_type),
                        row.timestamp_ms,
                        saved,
                        row.timestamp_ms,
                        escape_html(&row.original_url),
                        escape_html(&row.original_url),
                        escape_html(&row.output_mime_type),
                        format_dashboard_bytes(row.original_bytes),
                        format_dashboard_bytes(row.optimized_bytes),
                        format_dashboard_bytes(saved),
                        escape_html(&row.original_url),
                        escape_html(&row.optimized_url),
                        escape_html(&row.output_mime_type),
                        row.original_bytes,
                        row.optimized_bytes,
                    )
                    .as_bytes(),
                );
            }
            fetch.handle_write(
                b"</tbody></table></div><div class='pagespeed-images-list-footer'><span id='pagespeed-images-result-count'></span><span>Filters apply to the records loaded in this page.</span></div><div id='pagespeed-images-empty-filter' class='pagespeed-images-empty-filter' style='display:none'>No images match those filters.</div><div class='pagespeed-image-modal' id='pagespeed-image-modal' role='dialog' aria-modal='true' aria-labelledby='pagespeed-image-modal-title' aria-hidden='true'><div class='pagespeed-image-modal-card'><div class='pagespeed-image-modal-header'><div><h2 id='pagespeed-image-modal-title'>Image comparison</h2><p id='pagespeed-image-modal-meta'></p></div><button type='button' class='pagespeed-image-modal-close' aria-label='Close image comparison'>&times;</button></div><div class='pagespeed-image-modal-compare'><figure><img id='pagespeed-image-modal-original' alt='Original image' referrerpolicy='no-referrer'><figcaption><span>Original</span><span><strong id='pagespeed-image-modal-original-size'></strong> <a id='pagespeed-image-modal-original-link' target='_blank' rel='noopener noreferrer' referrerpolicy='no-referrer'>Open</a></span></figcaption></figure><figure><img id='pagespeed-image-modal-optimized' alt='Optimized image' referrerpolicy='no-referrer'><figcaption><span><span id='pagespeed-image-modal-format'></span><br><strong id='pagespeed-image-modal-optimized-size'></strong></span><a id='pagespeed-image-modal-optimized-link' target='_blank' rel='noopener noreferrer' referrerpolicy='no-referrer'>Open</a></figcaption></figure></div><p class='pagespeed-image-modal-saved' id='pagespeed-image-modal-saved'></p></div></div></section>",
            );
            fetch.handle_write(
                b"<script>(function(){var times=document.querySelectorAll('.pagespeed-image-time');for(var i=0;i<times.length;i++){var t=Number(times[i].getAttribute('data-timestamp-ms'));if(t>0){times[i].textContent=new Date(t).toLocaleString();}}var modal=document.getElementById('pagespeed-image-modal');var rows=document.getElementById('pagespeed-images-rows');if(rows){var search=document.getElementById('pagespeed-images-search');var format=document.getElementById('pagespeed-images-format');var sort=document.getElementById('pagespeed-images-sort');var entries=Array.prototype.slice.call(rows.querySelectorAll('tr'));var count=document.getElementById('pagespeed-images-result-count');var empty=document.getElementById('pagespeed-images-empty-filter');function filterRows(){var query=search.value.toLowerCase();var selected=format.value;entries.sort(function(a,b){return sort.value==='saved'?Number(b.getAttribute('data-saved-bytes'))-Number(a.getAttribute('data-saved-bytes')):Number(b.getAttribute('data-timestamp-ms'))-Number(a.getAttribute('data-timestamp-ms'));});var visible=0;for(var j=0;j<entries.length;j++){var match=(selected==='all'||entries[j].getAttribute('data-format')===selected)&&entries[j].textContent.toLowerCase().indexOf(query)!==-1;entries[j].style.display=match?'':'none';if(match)visible++;rows.appendChild(entries[j]);}count.textContent='Showing '+visible+' of '+entries.length+' rewrites';empty.style.display=visible?'none':'block';}search.addEventListener('input',filterRows);format.addEventListener('change',filterRows);sort.addEventListener('change',filterRows);filterRows();}if(!modal)return;var close=modal.querySelector('.pagespeed-image-modal-close');function hide(){modal.classList.remove('is-open');modal.setAttribute('aria-hidden','true');document.body.style.overflow='';if(lastTrigger)lastTrigger.focus();}var lastTrigger=null;document.addEventListener('click',function(event){var button=event.target.closest('.pagespeed-image-preview-button');if(button){var original=button.getAttribute('data-original-url');var optimized=button.getAttribute('data-optimized-url');var format=button.getAttribute('data-format');var before=Number(button.getAttribute('data-original-bytes'));var after=Number(button.getAttribute('data-optimized-bytes'));document.getElementById('pagespeed-image-modal-title').textContent=original;document.getElementById('pagespeed-image-modal-meta').textContent='Original and optimized image';document.getElementById('pagespeed-image-modal-original').src=original;document.getElementById('pagespeed-image-modal-optimized').src=optimized;document.getElementById('pagespeed-image-modal-original-link').href=original;document.getElementById('pagespeed-image-modal-optimized-link').href=optimized;document.getElementById('pagespeed-image-modal-format').textContent=format;document.getElementById('pagespeed-image-modal-original-size').textContent=before.toLocaleString()+' bytes';document.getElementById('pagespeed-image-modal-optimized-size').textContent=after.toLocaleString()+' bytes';document.getElementById('pagespeed-image-modal-saved').textContent='Saved '+Math.max(0,before-after).toLocaleString()+' bytes ('+(before>0?Math.round((before-after)*100/before):0)+'% smaller)';modal.classList.add('is-open');modal.setAttribute('aria-hidden','false');document.body.style.overflow='hidden';lastTrigger=button;close.focus();return;}if(event.target===modal||event.target===close){hide();}});document.addEventListener('keydown',function(event){if(!modal.classList.contains('is-open'))return;if(event.key==='Escape'){hide();return;}if(event.key!=='Tab')return;var focusable=modal.querySelectorAll('button,a[href],input,select,textarea,[tabindex]:not([tabindex=\"-1\"])');focusable=Array.prototype.filter.call(focusable,function(el){return !el.disabled&&el.offsetParent!==null;});if(!focusable.length)return;var first=focusable[0],last=focusable[focusable.length-1];if(event.shiftKey&&document.activeElement===first){event.preventDefault();last.focus();}else if(!event.shiftKey&&document.activeElement===last){event.preventDefault();first.focus();}});})();</script>",
            );
        }
        close_admin_html(fetch);
    }

    /// Lists recent HTML publications (`HtmlCacheHandler`).
    #[allow(clippy::too_many_arguments)]
    pub fn html_cache_handler(
        &self,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
        options: &RewriteOptions,
        stored: Option<&str>,
        controls: &str,
        archive_options: Option<&AdminConfig>,
    ) {
        fetch
            .response_headers_mut()
            .set_date_and_caching(self.now_ms(), 0, ", private, no-store");
        open_admin_html("html_cache", "", source, self.now_ms(), fetch);
        fetch.handle_write(controls.as_bytes());
        let log;
        if let Some(stored) = stored {
            log = stored.to_string();
        } else {
            log = self.log.lock().map(|log| log.dump()).unwrap_or_default();
        }
        let mut rows = Vec::new();
        read_recent_html_rewrites(&log, &mut rows);
        fetch.handle_write(
            b"<section class='pagespeed-html-cache-page'><div class='pagespeed-dashboard-heading'><p class='pagespeed-dashboard-eyebrow'>HTML OPTIMIZATION</p><h1>HTML rewrite cache</h1><p class='pagespeed-dashboard-description'>Recent completed HTML rewrites, isolated by request headers and browser capabilities.</p></div>",
        );
        let previous = Self::previous_dashboard_counters(stats, archive_options);
        Self::write_html_cache_summary(stats, fetch, &previous);
        Self::write_html_cache_cookie_policy(options, fetch);
        let mut bypasses = Vec::new();
        read_recent_cookie_bypasses(&log, &mut bypasses);
        Self::write_html_cache_cookie_bypasses(&bypasses, fetch);
        fetch.handle_write(
            format!(
                "<section class='pagespeed-dashboard-panel'><div class='pagespeed-dashboard-panel-heading'><div><h2>Recent HTML publications</h2><p>HTML cache: <strong>{}</strong>. Configured TTL cap: {} seconds.</p></div><a href='config'>View configuration &rarr;</a></div>{}This is publication history, not a live cache inventory: eviction, purge, policy changes, or new beacon evidence can invalidate an entry before its TTL. No HTML bodies or raw request headers are recorded. Opening a URL makes a new request using your browser's headers; it is not a snapshot of the listed variant.</p>",
                if options.html_cache() {
                    "Enabled"
                } else {
                    "Disabled"
                },
                options.html_cache_ttl_sec(),
                if stored.is_some() {
                    "<p class='pagespeed-dashboard-footnote'>Recent publications merged from memory with retained history. "
                } else {
                    "<p class='pagespeed-dashboard-footnote'>Up to 100 recently recorded variants from the bounded message buffer (MessageBufferSize must be nonzero). "
                },
            )
            .as_bytes(),
        );
        if rows.is_empty() {
            fetch.handle_write(
                if stored.is_some() {
                    "<div class='pagespeed-images-empty'><h2>No HTML publications match</h2><p>Try another date range or an older page.</p></div></section></section>"
                } else {
                    "<div class='pagespeed-images-empty'><h2>No HTML rewrites recorded</h2><p>Enable HtmlCache and MessageBufferSize, then visit a publicly cacheable HTML page. Entries appear after beacon learning and resource rewrites settle. Private requests and sampled beacon responses are not recorded.</p></div></section></section>"
                }
                .as_bytes(),
            );
            close_admin_html(fetch);
            return;
        }
        fetch.handle_write(
            b"<div class='pagespeed-images-toolbar'><label class='pagespeed-images-search-wrap'><input type='search' id='pagespeed-html-search' placeholder='Search page URL or variant' aria-label='Search page URL or variant'></label><div><label class='pagespeed-images-filter-label'>Device <select id='pagespeed-html-device' aria-label='Filter by device'><option value='all'>All devices</option><option value='desktop'>Desktop</option><option value='mobile'>Mobile</option><option value='tablet'>Tablet</option></select></label><label class='pagespeed-images-filter-label'>Capabilities <select id='pagespeed-html-format' aria-label='Filter by image capability'><option value='all'>All capabilities</option><option>AVIF capable</option><option>WebP capable</option><option>Original formats</option></select></label></div></div><div class='pagespeed-images-table-wrap'><table class='pagespeed-admin-data-table pagespeed-html-cache-table'><thead><tr><th>Created</th><th>Page URL</th><th>Device</th><th>Image capability</th><th>Variant</th><th>HTML size</th><th>Age</th><th>Expires / TTL</th></tr></thead><tbody id='pagespeed-html-rows'>",
        );
        let now_ms = self.now_ms();
        for row in &rows {
            let escaped_url = escape_html(&row.url);
            let escaped_variant = escape_html(&row.variant);
            let tail = row.variant.len().saturating_sub(12);
            let short_variant =
                escape_html(&String::from_utf8_lossy(&row.variant.as_bytes()[tail..]));
            let age_secs = (now_ms - row.created_ms).max(0) / 1000;
            let within_ttl = row.expires_ms > now_ms;
            fetch.handle_write(
                format!(
                    "<tr data-device='{}' data-format='{}' data-variant='{}'><td><time data-html-time='{}'>{}</time></td><td class='pagespeed-image-url'><a href='{}' target='_blank' rel='noopener noreferrer' referrerpolicy='no-referrer'>{}</a></td><td>{}</td><td>{}</td><td><code title='{}'>{}</code></td><td>{}</td><td data-html-age='{}'>{}s</td><td><time data-html-time='{}'>{}</time><span class='pagespeed-html-ttl' data-html-expiry='{}'>{}</span></td></tr>",
                    row.device,
                    row.formats,
                    escaped_variant,
                    row.created_ms,
                    row.created_ms,
                    escaped_url,
                    escaped_url,
                    row.device,
                    row.formats,
                    escaped_variant,
                    short_variant,
                    format_dashboard_bytes(row.bytes),
                    row.created_ms,
                    age_secs,
                    row.expires_ms,
                    row.expires_ms,
                    row.expires_ms,
                    if within_ttl {
                        "Within TTL"
                    } else {
                        "TTL elapsed"
                    },
                )
                .as_bytes(),
            );
        }
        fetch.handle_write(
            b"</tbody></table></div><p id='pagespeed-html-results' class='pagespeed-dashboard-footnote' role='status' aria-live='polite'></p></section></section><script>(function(){var search=document.getElementById('pagespeed-html-search'),device=document.getElementById('pagespeed-html-device'),format=document.getElementById('pagespeed-html-format'),rows=document.getElementById('pagespeed-html-rows').rows,result=document.getElementById('pagespeed-html-results');function filter(){var q=search.value.toLowerCase(),n=0;for(var i=0;i<rows.length;i++){var r=rows[i],show=(r.textContent+' '+r.getAttribute('data-variant')).toLowerCase().indexOf(q)!==-1&&(device.value==='all'||r.getAttribute('data-device')===device.value)&&(format.value==='all'||r.getAttribute('data-format')===format.value);r.hidden=!show;if(show)n++;}result.textContent=n+' of '+rows.length+' recorded variants shown. Refresh to load new publications.';}function times(){var now=Date.now(),nodes=document.querySelectorAll('[data-html-time]');for(var i=0;i<nodes.length;i++){var t=Number(nodes[i].getAttribute('data-html-time'));nodes[i].textContent=new Date(t).toLocaleString();}nodes=document.querySelectorAll('[data-html-age]');for(var j=0;j<nodes.length;j++)nodes[j].textContent=Math.max(0,Math.floor((now-Number(nodes[j].getAttribute('data-html-age')))/1000))+'s';nodes=document.querySelectorAll('[data-html-expiry]');for(var k=0;k<nodes.length;k++){var n=nodes[k],valid=now<Number(n.getAttribute('data-html-expiry'));n.textContent=valid?'Within TTL':'TTL elapsed';n.setAttribute('data-elapsed',valid?'false':'true');}}search.addEventListener('input',filter);device.addEventListener('change',filter);format.addEventListener('change',filter);times();filter();window.setInterval(times,1000);})();</script>",
        );
        close_admin_html(fetch);
    }

    /// Writes HTML-cache counters (`WriteHtmlCacheSummary`).
    fn write_html_cache_summary(
        stats: &Statistics,
        fetch: &mut dyn AsyncFetch,
        previous: &BTreeMap<String, i64>,
    ) {
        fetch.handle_write(b"<div class='pagespeed-dashboard-grid'>");
        for (name, label, note) in [
            (
                "html_cache_hits",
                "HTML CACHE HITS",
                "Parsing and filters skipped",
            ),
            (
                "html_cache_misses",
                "LOOKUP MISSES",
                "Entry absent or stale",
            ),
            (
                "html_cache_bypasses",
                "BYPASSED LOOKUPS",
                "Request or rewrite not eligible",
            ),
            (
                "html_cache_inserts",
                "HTML PUBLICATIONS",
                "Best-effort cache writes",
            ),
            (
                "html_cache_cookie_allowlisted",
                "COOKIE ALLOWLIST ACCEPTS",
                "Lookups admitted with ignored cookies",
            ),
        ] {
            fetch.handle_write(
                format!(
                    "<article class='pagespeed-dashboard-card'><div class='pagespeed-dashboard-card-label'>{label}</div><div class='pagespeed-dashboard-value'>{}</div><div class='pagespeed-dashboard-card-note'>{note}</div></article>",
                    Self::dashboard_counter(stats, name, previous),
                )
                .as_bytes(),
            );
        }
        let hits = Self::dashboard_counter(stats, "html_cache_hits", previous);
        let misses = Self::dashboard_counter(stats, "html_cache_misses", previous);
        fetch.handle_write(
            format!(
                "</div><p class='pagespeed-dashboard-footnote'>Hit rate: {}% of lookups. HTML served from cache: {}. Counters include available saved samples plus current activity; served bytes are not compression savings.</p>",
                dashboard_percent(hits, hits + misses),
                format_dashboard_bytes(Self::dashboard_counter(
                    stats,
                    "html_cache_bytes_served",
                    previous
                )),
            )
            .as_bytes(),
        );
    }

    /// Writes the effective HTML-cache cookie policy: the built-in
    /// common-cookie allowlist, custom `HtmlCacheIgnoreCookie`
    /// patterns, and explicit `HtmlCacheBypassCookie` rules.
    fn write_html_cache_cookie_policy(options: &RewriteOptions, fetch: &mut dyn AsyncFetch) {
        let builtin = ps_config::cookies::builtin_ignore_patterns();
        let chips = |patterns: &[String]| -> String {
            if patterns.is_empty() {
                return "<span class='pagespeed-dashboard-muted'>None</span>".to_string();
            }
            patterns
                .iter()
                .map(|pattern| format!("<code>{}</code>", escape_html(pattern)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        fetch.handle_write(
            format!(
                "<section class='pagespeed-dashboard-panel'><div class='pagespeed-dashboard-panel-heading'><div><h2>Cookie allowlist</h2><p>Requests carrying only allowlisted cookies cache exactly like cookie-free requests; cookie values never enter cache keys. Common cookies: <strong>{}</strong> ({} built-in patterns).</p></div></div><dl class='pagespeed-admin-data-list'><dt>Built-in common cookies</dt><dd>{}</dd><dt>HtmlCacheIgnoreCookie (custom, extends built-in)</dt><dd>{}</dd><dt>HtmlCacheBypassCookie (forces bypass)</dt><dd>{}</dd></dl><p class='pagespeed-dashboard-footnote'>Allowlisted cookies must not influence server-generated HTML: a cached variant is served to every visitor regardless of their ignored-cookie values. Unknown, authentication, session, cart, and personalization cookies always bypass. Cookie values are never logged; bypass history below names cookies only.</p></section>",
                if options.html_cache_ignore_common_cookies() {
                    "Ignored"
                } else {
                    "Not ignored"
                },
                builtin.len(),
                chips(&builtin),
                chips(options.html_cache_ignore_cookies().patterns()),
                chips(options.html_cache_bypass_cookies().patterns()),
            )
            .as_bytes(),
        );
    }

    /// Writes recent cookie-attributed bypasses (skipped when none
    /// recorded): each row names the responsible cookie, never its
    /// value.
    fn write_html_cache_cookie_bypasses(
        bypasses: &[HtmlCacheBypassRow],
        fetch: &mut dyn AsyncFetch,
    ) {
        if bypasses.is_empty() {
            return;
        }
        fetch.handle_write(
            b"<section class='pagespeed-dashboard-panel'><div class='pagespeed-dashboard-panel-heading'><div><h2>Recent cookie bypasses</h2><p>Lookups bypassed by request cookies, newest first. Remove the cookie, allowlist it with HtmlCacheIgnoreCookie when it cannot change server HTML, or force it with HtmlCacheBypassCookie.</p></div></div><div class='pagespeed-images-table-wrap'><table class='pagespeed-admin-data-table'><thead><tr><th>Time</th><th>Reason</th><th>Page URL</th></tr></thead><tbody>",
        );
        for row in bypasses {
            let escaped_url = escape_html(&row.url);
            fetch.handle_write(
                format!(
                    "<tr><td>{}</td><td><code>{}</code></td><td class='pagespeed-image-url'><a href='{}' target='_blank' rel='noopener noreferrer' referrerpolicy='no-referrer'>{}</a></td></tr>",
                    escape_html(&ps_http::time::format_http_date(row.timestamp_ms)),
                    escape_html(&row.reason),
                    escaped_url,
                    escaped_url,
                )
                .as_bytes(),
            );
        }
        fetch.handle_write(b"</tbody></table></div></section>");
    }

    /// Displays the message log (`MessageHistoryHandler`).
    pub fn message_history_handler(
        &self,
        options: &RewriteOptions,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        stored: Option<&str>,
        controls: &str,
        kind: &str,
    ) {
        // The answer walks the whole retained buffer: one read at a time.
        let guard = ExpensiveGuard::take(self, ExpensiveLeaf::Messages);
        if !guard.held() {
            write_admin_busy(fetch);
            return;
        }
        // Request for page /mod_pagespeed_message.
        let current_link = if kind == "messages" {
            "message_history"
        } else {
            kind
        };
        open_admin_html(current_link, "", source, self.now_ms(), fetch);
        fetch.handle_write(controls.as_bytes());
        write_heading(
            fetch,
            "EVENT LOG",
            if kind == "javascript" {
                "JavaScript diagnostics"
            } else if kind == "css" {
                "CSS diagnostics"
            } else {
                "Message History"
            },
            if kind == "messages" {
                "Review PageSpeed notices, warnings, and rewrite diagnostics."
            } else {
                "Existing diagnostic messages only; not a complete per-resource rewrite history."
            },
        );
        let mut log;
        if let Some(stored) = stored {
            log = stored.to_string();
        } else {
            let dumped = self
                .log
                .lock()
                .map(|log| log.dump_checked())
                .unwrap_or_default();
            let Some(dumped) = dumped else {
                fetch.handle_write(
                    b"<p>Writing to mod_pagespeed_message failed. \nVerify that MessageBufferSize is not set to 0 in pagespeed.conf.</p>\n",
                );
                close_admin_html(fetch);
                return;
            };
            log = dumped;
        }
        if kind != "messages" {
            let mut filtered = String::from("\n");
            for record in LogBuffer::parse_dump(&log) {
                if history_matches(kind, record) {
                    filtered.push_str(record);
                    filtered.push('\n');
                }
            }
            log = filtered;
        }
        fetch.handle_write(b"<div id='log'>");
        // Keep the raw record text in an escaped, machine-readable element. The
        // history UI extracts severity and timestamp without parsing HTML styling.
        for message in LogBuffer::parse_dump(&log) {
            let mut display = message;
            if let Some(detail) = display.find("\tPS_IMAGE_DETAIL\t") {
                display = &display[..detail];
            }
            if !display.is_empty() {
                let severity = match LogBuffer::message_type(display) {
                    ps_config::message_handler::MessageLevel::Error => "error",
                    ps_config::message_handler::MessageLevel::Warning => "warning",
                    ps_config::message_handler::MessageLevel::Fatal => "fatal",
                    ps_config::message_handler::MessageLevel::Info => "info",
                };
                fetch.handle_write(
                    format!(
                        "<div class='pagespeed-message-entry' data-severity='{severity}'>{}</div>\n",
                        escape_html(LogBuffer::reformat(display)),
                    )
                    .as_bytes(),
                );
            }
        }
        fetch.handle_write(b"</div>\n");
        let debug = options.enabled(Filter::Debug);
        fetch.handle_write(
            format!(
                "<script type='text/javascript'>{}\npagespeed.Messages.Start();</script>\n",
                script("messages", debug),
            )
            .as_bytes(),
        );
        close_admin_html(fetch);
    }

    /// Displays the console or its JSON feed (`ConsoleHandler`).
    pub fn console_handler(
        &self,
        global_options: &AdminConfig,
        options: &RewriteOptions,
        source: AdminSource,
        query: &QueryParams,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
    ) {
        if query.has("json") {
            self.console_json_handler(query, fetch, stats);
            return;
        }
        let statistics_enabled = global_options.statistics_enabled;
        let logging_enabled = global_options.statistics_logging_enabled;
        let log_dir_set = !global_options.log_dir.is_empty();
        let debug = options.enabled(Filter::Debug);
        let console_js = script("console", debug);
        let head_markup = format!("<style>{}</style>\n", stylesheet("console"));
        open_admin_html("console", &head_markup, source, self.now_ms(), fetch);
        write_heading(
            fetch,
            "OPERATIONS",
            "Console",
            "Live PageSpeed diagnostics and operational status.",
        );
        if statistics_enabled && logging_enabled && log_dir_set {
            fetch.handle_write(
                b"<div class='pagespeed-console' id='suggestions'>\n  <div class='pagespeed-console-status' id='pagespeed-console-status' role='status'>Loading recent diagnostics...</div>\n  <div class='pagespeed-console-summary' id='pagespeed-console-summary'></div>\n  <div class='pagespeed-console-toolbar'>\n    <div class='pagespeed-console-toolbar-copy'>\n      <h2>Operational signals</h2>\n      <p>Failure and cache trends from the last 24 hours.</p>\n    </div>\n    <input class='pagespeed-console-search' id='pagespeed-console-search' type='search' placeholder='Filter signals...' aria-label='Filter signals'>\n  </div>\n  <div class='pagespeed-console-grid' id='pagespeed-graphs-container'></div>\n</div>\n<script>var pagespeedStatisticsUrl = '';</script>\n<script>",
            );
            // From the admin page, the console JSON is relative, so it can
            // be set to ''.
            fetch.handle_write(console_js.as_bytes());
            fetch.handle_write(b"pagespeed.startConsole();");
            fetch.handle_write(b"</script>\n");
        } else {
            fetch.handle_write(
                b"<section class='pagespeed-console-unavailable'>\n  <h2>Console needs a little setup</h2>\n  <p>PageSpeed cannot load the operational console because:</p>\n  <ul>\n",
            );
            if !statistics_enabled {
                fetch.handle_write(b"  <li>Statistics is not enabled.</li>\n");
            }
            if !logging_enabled {
                fetch.handle_write(b"  <li>StatisticsLogging is not enabled.</li>\n");
            }
            if !log_dir_set {
                fetch.handle_write(b"  <li>LogDir is not set.</li>\n");
            }
            fetch.handle_write(
                b"</ul>\n<p>\n  Configure these options to enable the console. See the\n  <a href='https://developers.google.com/speed/pagespeed/module/console'>console documentation</a>\n  for more details.\n</p>\n</section>\n",
            );
        }
        close_admin_html(fetch);
    }

    /// Serves console/graph log samples (`ConsoleJsonHandler`).
    pub fn console_json_handler(
        &self,
        query: &QueryParams,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
    ) {
        let guard = ExpensiveGuard::take(self, ExpensiveLeaf::Graphs);
        if !guard.held() {
            write_admin_busy(fetch);
            return;
        }
        let Some(logger) = stats.console_logger() else {
            fetch.response_headers_mut().set_status_and_reason(404);
            fetch
                .response_headers_mut()
                .headers_mut()
                .add("Content-Type", "text/plain");
            fetch.handle_write(b"console_logger must be enabled to use '?json' query parameter.");
            fetch.handle_done(true);
            return;
        };
        let query = parse_graph_query(query, self.now_ms());
        let mut var_titles: Vec<String> = query.var_titles.into_iter().collect();
        var_titles.sort();
        fetch.response_headers_mut().set_status_and_reason(200);
        fetch
            .response_headers_mut()
            .headers_mut()
            .add("Content-Type", "application/json");
        fetch.handle_write(
            logger
                .dump_json(
                    query.dump_for_graphs,
                    &var_titles,
                    query.start_time,
                    query.end_time,
                    query.granularity_ms,
                )
                .as_bytes(),
        );
        fetch.handle_done(true);
    }

    /// Displays graphs or their JSON feed (`GraphsHandler`).
    pub fn graphs_handler(
        &self,
        options: &RewriteOptions,
        source: AdminSource,
        query: &QueryParams,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
    ) {
        if query.has("json") {
            self.console_json_handler(query, fetch, stats);
            return;
        }
        let head_markup = format!("<style>{}</style>\n", stylesheet("graphs"));
        open_admin_html("graphs", &head_markup, source, self.now_ms(), fetch);
        write_heading(
            fetch,
            "TRENDS",
            "Graphs",
            "Track cache behavior, image rewrites, and runtime activity over time.",
        );
        if stats.console_logger().is_none() {
            // Same gate as the ?json data fetch below: without a console logger
            // the charts can never load, so say so instead of spinning forever.
            // Also covers no-JavaScript clients, which see this section as-is.
            fetch.handle_write(
                b"<section class='pagespeed-console-unavailable'>\n  <h2>Graphs need a little setup</h2>\n  <p>PageSpeed cannot load trend charts because statistics logging is not enabled. Enable Statistics, StatisticsLogging, and LogDir to use this page.</p>\n</section>\n",
            );
            close_admin_html(fetch);
            return;
        }
        fetch.handle_write(
            b"<div id='cache_applied'>Loading Charts...<noscript>Charts require JavaScript.</noscript></div><div id='cache_type'>Loading Charts...<noscript>Charts require JavaScript.</noscript></div><div id='ipro'>Loading Charts...<noscript>Charts require JavaScript.</noscript></div><div id='image_rewriting'>Loading Charts...<noscript>Charts require JavaScript.</noscript></div><div id='realtime'>Loading Charts...<noscript>Charts require JavaScript.</noscript></div>",
        );
        let debug = options.enabled(Filter::Debug);
        fetch.handle_write(
            format!(
                "<script type='text/javascript'>{}\npagespeed.Graphs.Start();</script>\n",
                script("graphs", debug),
            )
            .as_bytes(),
        );
        close_admin_html(fetch);
    }

    /// Dumps statistics as text (`StatisticsHandler`).
    pub fn statistics_handler(
        &self,
        options: &RewriteOptions,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
    ) {
        let head_markup = format!("<style>{}</style>\n", stylesheet("statistics"));
        open_admin_html("statistics", &head_markup, source, self.now_ms(), fetch);
        write_heading(
            fetch,
            "LIVE METRICS",
            "Statistics",
            "Explore PageSpeed counters grouped by the subsystem they describe.",
        );
        // We have to call Dump() here to write data to sources for
        // system/system_test.sh: Line 79. We use JS to update the data in refreshes.
        fetch.handle_write(b"<pre id='stat'>");
        fetch.handle_write(stats.dump().as_bytes());
        fetch.handle_write(b"</pre>\n");
        let debug = options.enabled(Filter::Debug);
        fetch.handle_write(
            format!(
                "<script type='text/javascript'>{}\npagespeed.Statistics.Start();</script>\n",
                script("statistics", debug),
            )
            .as_bytes(),
        );
        close_admin_html(fetch);
    }

    /// Dumps statistics as JSON (`StatisticsJsonHandler`).
    pub fn statistics_json_handler(&self, fetch: &mut dyn AsyncFetch, stats: &Statistics) {
        fetch.response_headers_mut().set_status_and_reason(200);
        fetch
            .response_headers_mut()
            .headers_mut()
            .add("Content-Type", "application/json");
        fetch.handle_write(stats.dump_json().as_bytes());
        fetch.handle_done(true);
    }

    /// Displays configuration (`PrintConfig`).
    pub fn print_config(
        &self,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        options: &RewriteOptions,
    ) {
        let guard = ExpensiveGuard::take(self, ExpensiveLeaf::Config);
        if !guard.held() {
            write_admin_busy(fetch);
            return;
        }
        open_admin_html("config", "", source, self.now_ms(), fetch);
        write_heading(
            fetch,
            "ACTIVE SETTINGS",
            "Configuration",
            "View the effective PageSpeed options for this server instance.",
        );
        fetch.handle_write(
            b"<div class='pagespeed-config-page'><div id='pagespeed-config-root'></div><div id='pagespeed-config-source' class='pagespeed-config-source'>",
        );
        write_pre(fetch, &options.options_to_string());
        fetch.handle_write(b"</div></div><script type='text/javascript'>");
        fetch.handle_write(script("config", false).as_bytes());
        fetch.handle_write(b"</script>\n");
        close_admin_html(fetch);
    }

    /// Displays histograms (`PrintHistograms`).
    pub fn print_histograms(
        &self,
        source: AdminSource,
        fetch: &mut dyn AsyncFetch,
        stats: &Statistics,
    ) {
        open_admin_html("histograms", "", source, self.now_ms(), fetch);
        write_heading(
            fetch,
            "DISTRIBUTIONS",
            "Histograms",
            "Review the spread and percentiles of measured PageSpeed operations.",
        );
        fetch.handle_write(b"<div id='pagespeed-histograms'>");
        fetch.handle_write(stats.render_histograms().as_bytes());
        fetch.handle_write(b"</div>");
        close_admin_html(fetch);
    }

    /// Displays caches, purge sets, and metadata lookups
    /// (`PrintCaches`).
    #[allow(clippy::too_many_arguments)]
    pub fn print_caches(
        &self,
        source: AdminSource,
        is_global: bool,
        stripped_url: &GoogleUrl,
        query: &QueryParams,
        options: &RewriteOptions,
        fetch: &mut dyn AsyncFetch,
        request: &mut AdminRequest<'_>,
    ) {
        if source == AdminSource::PageSpeedAdmin {
            if let Some(url) = query.lookup1_unescaped("url") {
                // Deleting an entry is a cache-changing action like purge, so it is
                // available only when cache purging is enabled.
                if query.has("Delete") && !options.enable_cache_purge() {
                    fetch.response_headers_mut().set_status_and_reason(403);
                    fetch
                        .response_headers_mut()
                        .headers_mut()
                        .add("Content-Type", "text/html");
                    fetch.handle_write(b"Deleting not enabled: please add\n");
                    write_pre(
                        fetch,
                        &format!(
                            "  {}",
                            RewriteOptions::format_option("EnableCachePurge", "on")
                        ),
                    );
                    fetch.handle_write(b"\nto your configuration file.\n");
                    fetch.handle_done(true);
                    return;
                }
                // Delegate to ShowCacheHandler to get the cached value for that
                // URL, which it may do asynchronously, so we cannot use the
                // AdminHtml abstraction which closes the connection in its
                // destructor.
                let as_json = query.lookup1_unescaped("json").is_some();
                let user_agent = query.lookup1_unescaped("user_agent").unwrap_or_default();
                // Each lookup builds a rewrite driver: one at a time, until the answer
                // has been written (it can complete after this call returns). The slot
                // is taken without waiting: a wait would block this thread, which can be
                // the server's event loop, and the slot may be held by a lookup that is
                // waiting for that same thread.
                let guard = ExpensiveGuard::take_wait(self, ExpensiveLeaf::CacheLookup, 0);
                if !guard.held() {
                    write_admin_busy(fetch);
                    return;
                }
                let delete = query.has("Delete");
                let found = match request.show_cache {
                    Some(show_cache) => show_cache(&url, &user_agent, delete, as_json),
                    None => CacheLookup {
                        status: 200,
                        content_type: "text/html".to_string(),
                        body: format!("Metadata lookup for {url} is unavailable in this build."),
                    },
                };
                // The slot frees when the lookup's response completes, like the
                // C++ `SlotReleasingFetch`; the synchronous hook below always
                // completes before this call returns.
                fetch
                    .response_headers_mut()
                    .set_status_code(i32::from(found.status));
                fetch
                    .response_headers_mut()
                    .headers_mut()
                    .add("Content-Type", &found.content_type);
                fetch.handle_write(found.body.as_bytes());
                fetch.handle_done(true);
                return;
            }
        }

        if source == AdminSource::PageSpeedAdmin && query.lookup1_unescaped("new_set").is_some() {
            fetch.response_headers_mut().set_status_and_reason(200);
            fetch
                .response_headers_mut()
                .headers_mut()
                .add("Content-Type", "text/html");
            let body = if is_global {
                request.purge_set.to_display_string(format_purge_time)
            } else {
                purge_set_string_for_request_host(request.purge_set, stripped_url)
            };
            fetch.handle_write(body.as_bytes());
            fetch.handle_done(true);
            return;
        }

        if source == AdminSource::PageSpeedAdmin {
            if let Some(purge) = query.lookup1_unescaped("purge") {
                // A per-vhost console is scoped to its own host: a whole-cache purge is
                // global-only, and a single-URL purge may only target this host. These
                // scope checks run ahead of the "purge enabled" check below so a
                // disabled purge does not mask a cross-scope request as a soft failure.
                if !is_global {
                    if purge == "*" {
                        fetch.response_headers_mut().set_status_and_reason(403);
                        fetch
                            .response_headers_mut()
                            .headers_mut()
                            .add("Content-Type", "text/html");
                        fetch.handle_write(
                            b"global purge is available from the global admin console",
                        );
                        fetch.handle_done(true);
                        return;
                    }
                    if !purge.is_empty() {
                        let origin = GoogleUrl::new(stripped_url.origin());
                        let resolved = GoogleUrl::resolve(&origin, &purge);
                        if resolved.is_web_valid() {
                            // PurgeHandler reads any target ending in '*' as the whole-cache
                            // purge, whatever host the URL names; judge the resolved form, the
                            // exact string the purge below would act on.
                            if resolved.spec().ends_with('*') {
                                fetch.response_headers_mut().set_status_and_reason(403);
                                fetch
                                    .response_headers_mut()
                                    .headers_mut()
                                    .add("Content-Type", "text/html");
                                fetch.handle_write(
                                    b"global purge is available from the global admin console",
                                );
                                fetch.handle_done(true);
                                return;
                            }
                            if !target_belongs_to_request_host(stripped_url, &resolved) {
                                fetch.response_headers_mut().set_status_and_reason(403);
                                fetch
                                    .response_headers_mut()
                                    .headers_mut()
                                    .add("Content-Type", "text/html");
                                fetch.handle_write(
                                    b"URL belongs to another host; use that host's console",
                                );
                                fetch.handle_done(true);
                                return;
                            }
                        }
                    }
                }
                if !options.enable_cache_purge() {
                    fetch.response_headers_mut().set_status_and_reason(200);
                    fetch
                        .response_headers_mut()
                        .headers_mut()
                        .add("Content-Type", "text/html");
                    fetch.handle_write(b"Purging not enabled: please add\n");
                    write_pre(
                        fetch,
                        &format!(
                            "  {}",
                            RewriteOptions::format_option("EnableCachePurge", "on")
                        ),
                    );
                    fetch.handle_write(
                    b"\nto your configuration file. See the \n<a href='https://developers.google.com/speed/pagespeed/module/system#purge_cache'>documentation</a> for more information about cache purging.",
                );
                    fetch.handle_done(true);
                } else if purge == "*" {
                    self.purge_handler(&purge, true, fetch, request.purge);
                } else if purge.is_empty() {
                    fetch.response_headers_mut().set_status_and_reason(200);
                    fetch
                        .response_headers_mut()
                        .headers_mut()
                        .add("Content-Type", "text/html");
                    fetch.handle_write(b"Empty URL");
                    fetch.handle_done(true);
                } else {
                    let origin = GoogleUrl::new(stripped_url.origin());
                    let resolved = GoogleUrl::resolve(&origin, &purge);
                    if !resolved.is_web_valid() {
                        fetch.response_headers_mut().set_status_and_reason(200);
                        fetch
                            .response_headers_mut()
                            .headers_mut()
                            .add("Content-Type", "text/html");
                        fetch.handle_write(
                            format!("Invalid URL: {}", escape_html(&purge)).as_bytes(),
                        );
                        fetch.handle_done(true);
                    } else {
                        // A purge from a per-vhost console passes allow_eviction = false.
                        self.purge_handler(resolved.spec(), is_global, fetch, request.purge);
                    }
                }
                return;
            }
        }

        // The whole-server listing asks each memcached/redis server for its
        // status, so it runs one at a time.
        if is_global {
            let guard = ExpensiveGuard::take(self, ExpensiveLeaf::CacheStatus);
            if !guard.held() {
                write_admin_busy(fetch);
                return;
            }
        }
        let head_markup = format!("<style>{}</style>\n", stylesheet("caches"));
        open_admin_html("cache", &head_markup, source, self.now_ms(), fetch);
        write_heading(
            fetch,
            "CACHE INSPECTION",
            "Caches",
            "Inspect cache entries, backend structures, and URL invalidations.",
        );
        fetch.handle_write(b"<div id='show_metadata'>");
        // Present a small form to enter a URL.
        if source == AdminSource::PageSpeedAdmin {
            fetch.handle_write(
                b"<div class='pagespeed-cache-section-heading'><h2>Metadata lookup</h2><p>Inspect the cached PageSpeed metadata for a URL and User-Agent.</p></div>",
            );
            let user_agent = fetch
                .request_headers()
                .headers()
                .lookup1("User-Agent")
                .unwrap_or_default();
            fetch.handle_write(show_cache_form(&user_agent).as_bytes());
        }
        fetch.handle_write(b"</div>\n");
        // Display configured cache information.
        if let Some(inventory) = request.inventory {
            // PrintCacheStats flags: kDefaultStatFlags, plus kGlobalView for
            // whole-server listings, kIncludeMemcached, and kIncludeRedis.
            fetch.handle_write(
                b"<div id='cache_struct'><div class='pagespeed-cache-section-heading'><h2>Cache structure</h2><p>Configured storage layers and how PageSpeed composes them.</p></div>",
            );
            fetch.handle_write(CACHE_TABLE_START.as_bytes());
            fetch.handle_write(
                format!(
                    "{}{}{}{}",
                    cache_info_html_snippet("HTTP Cache", &inventory.http_cache_name),
                    cache_info_html_snippet("Metadata Cache", &inventory.metadata_cache_name),
                    cache_info_html_snippet("Property Cache", &inventory.property_store_name),
                    cache_info_html_snippet(
                        "FileSystem Metadata Cache",
                        inventory
                            .filesystem_metadata_cache_name
                            .as_deref()
                            .unwrap_or("none"),
                    ),
                )
                .as_bytes(),
            );
            fetch.handle_write(CACHE_TABLE_END.as_bytes());
            fetch.handle_write(b"</div>");
            fetch.handle_write(
                b"<div id='physical_cache'><div class='pagespeed-cache-section-heading'><h2>Backend statistics</h2><p>Live size and activity reported by configured cache backends.</p></div>",
            );
            if let Some(backend_stats) = &inventory.backend_stats {
                if !backend_stats.is_empty() {
                    write_pre(fetch, backend_stats);
                }
            }
            fetch.handle_write(b"</div>");
            fetch.handle_write(b"<div id='purge_cache'>");
            // Filled in by JS in caches.js: updatePurgeSet().
            fetch.handle_write(
                b"<div class='pagespeed-cache-section-heading'><h2>Purge set</h2><p>Review invalidations and purge a URL or the full PageSpeed cache.</p></div><div id='purge_global' class='pagespeed-caches-purge-global'></div><div id='purge_table' class='pagespeed-caches-purge-table'></div></div>",
            );
        }
        let debug = options.enabled(Filter::Debug);
        // Practice what we preach: put the blocking JS in the tail.
        fetch.handle_write(
            format!(
                "<script type='text/javascript'>{}\npagespeed.Caches.Start();</script>\n",
                script("caches", debug),
            )
            .as_bytes(),
        );
        close_admin_html(fetch);
    }

    /// Purges a URL, or the whole cache when it ends in `*`
    /// (`PurgeHandler` + `Purge`).
    pub fn purge_handler(
        &self,
        url: &str,
        allow_eviction: bool,
        fetch: &mut dyn AsyncFetch,
        purge: &mut dyn PurgeBackend,
    ) {
        match purge.purge(url, self.now_ms(), allow_eviction) {
            Ok(()) => {
                fetch.response_headers_mut().set_status_and_reason(200);
                fetch
                    .response_headers_mut()
                    .headers_mut()
                    .add("Content-Type", "text/html");
                // TODO(xqyin): Currently we may still return 'purge successful' even if
                // the URL does not exist in our cache. Figure out how to solve this case
                // while we don't want to search the whole cache which could be very large.
                fetch.handle_write(b"Purge successful");
            }
            Err(reason) => {
                fetch.response_headers_mut().set_status_and_reason(200);
                fetch
                    .response_headers_mut()
                    .headers_mut()
                    .add("Content-Type", "text/html");
                fetch.handle_write(escape_html(&reason).as_bytes());
                fetch.handle_write(b"\n");
                fetch.handle_write(escape_html("").as_bytes());
            }
        }
        fetch.handle_done(true);
    }

    /// Renders history controls and merged log (`HistoryControls`).
    /// Returns the controls HTML plus the merged stored log, if any.
    /// `live` carries an optional live-snapshot overlap window.
    pub fn history_controls(
        &self,
        kind: &str,
        query: &QueryParams,
        options: &AdminConfig,
        live: Option<(i64, i64, &str)>,
    ) -> (String, Option<String>) {
        if !options.history_archive {
            return (String::new(), None);
        }
        let from = query.lookup1_unescaped("from").unwrap_or_default();
        let until = query.lookup1_unescaped("until").unwrap_or_default();
        let cursor = query.lookup1_unescaped("cursor").unwrap_or_default();
        let mut begin = 0i64;
        let mut end = 253402300800000i64;
        let mut error = String::new();
        if (!from.is_empty() && history_date_ms(&from).is_none())
            || (!until.is_empty() && history_date_ms(&until).is_none())
        {
            error = "Invalid UTC date".to_string();
        }
        if let Some(parsed) = history_date_ms(&from) {
            begin = parsed;
        }
        if let Some(parsed) = history_date_ms(&until) {
            end = parsed;
        }
        if !until.is_empty() {
            end += 86_400_000;
        }
        if begin >= end {
            error = "Invalid date range".to_string();
        }
        let page = browse_with_live(&options.file_cache_path, kind, begin, end, &cursor, live);
        if error.is_empty() && !page.error.is_empty() {
            error = page.error.clone();
        }
        let mut log = if page.log.is_empty() {
            "\n".to_string()
        } else {
            page.log.clone()
        };
        if cursor.is_empty() {
            let dump = self.log.lock().map(|log| log.dump()).unwrap_or_default();
            if let Some(newline) = dump.find('\n') {
                log.push_str(&filter_log(&dump[newline + 1..], begin, end));
            }
        }
        let show_panel =
            kind != "images" && kind != "html_cache" && kind != "javascript" && kind != "css";
        let mut controls = String::new();
        if show_panel {
            controls.push_str("<section class='pagespeed-history-controls'><form method='get'><label>From (UTC) <input type='date' name='from' value='");
            controls.push_str(&escape_html(&from));
            controls
                .push_str("'></label><label>Through (UTC) <input type='date' name='until' value='");
            controls.push_str(&escape_html(&until));
            controls.push_str("'></label><button type='submit'>Browse history</button></form><p>");
            controls.push_str("One chronological history combines current memory with archived records. Archive pages are bounded to 100 matching records; older links continue into retained history. Existing view filters still apply.</p>");
        }
        if !error.is_empty() {
            controls.push_str(&format!("<p role='alert'>{}</p>", escape_html(&error)));
        }
        if !page.next.is_empty() {
            controls.push_str(&format!(
                "<nav aria-label='History pagination'><a href='?from={}&amp;until={}&amp;cursor={}'>Older records &rarr;</a></nav>",
                escape_html(&from),
                escape_html(&until),
                escape_html(&page.next),
            ));
        }
        if show_panel {
            controls.push_str("</section>");
        }
        (controls, Some(log))
    }

    /// Routes one admin URL (`AdminPage`).
    pub fn admin_page(
        &self,
        is_global: bool,
        stripped_url: &GoogleUrl,
        query: &QueryParams,
        options: &RewriteOptions,
        fetch: &mut dyn AsyncFetch,
        request: &mut AdminRequest<'_>,
    ) {
        // The handler is "pagespeed_admin", so we must dispatch off of
        // the remainder of the URL. For
        // "http://example.com/pagespeed_admin/foo?a=b" we want to pull out
        // "foo".
        let path = stripped_url.path_sans_query();
        let tail = path.strip_prefix('/').unwrap_or(path);
        // If there are no slashes at all in the path, e.g. it's "pagespeed_admin",
        // then the relative references to "config" etc will not work. We need
        // to serve the admin pages on "/pagespeed_admin/". So if we got to this
        // point and there are no slashes, then we can just redirect immediately
        // by adding a slash.
        if !tail.contains('/') {
            // If the URL is "/pagespeed_admin", then redirect to "/pagespeed_admin/" so
            // that relative URL references will work.
            fetch.response_headers_mut().set_status_and_reason(301);
            let target = format!("{}/", stripped_url.all_except_query());
            fetch
                .response_headers_mut()
                .headers_mut()
                .add("Location", &target);
            fetch
                .response_headers_mut()
                .headers_mut()
                .add("Content-Type", "text/html");
            let escaped = escape_html(&target);
            fetch.handle_write(format!("Redirecting to URL {escaped}").as_bytes());
            fetch.handle_done(true);
            return;
        }
        let leaf = stripped_url.leaf_sans_query();
        let dashboard_view = leaf == LEAF_DASHBOARD || leaf.is_empty();
        let kind = if dashboard_view {
            "images"
        } else if leaf == LEAF_MESSAGE_HISTORY {
            "messages"
        } else {
            leaf.as_str()
        };
        let has_history = matches!(
            kind,
            "messages" | "images" | "html_cache" | "javascript" | "css"
        ) || dashboard_view;
        let (controls, stored) = if has_history {
            let (controls, stored) =
                self.history_controls(kind, query, request.global_options, None);
            if request.global_options.history_archive {
                fetch.response_headers_mut().set_date_and_caching(
                    self.now_ms(),
                    0,
                    ", private, no-store",
                );
            }
            (controls, stored)
        } else {
            (String::new(), None)
        };
        // Both summaries must use the same logger and matching live counters.
        let mut history_stats = request.stats;
        if (dashboard_view || leaf == LEAF_HTML_CACHE)
            && request.global_options.history_archive
            && request.stats.console_logger().is_none()
            && request.statistics.console_logger().is_some()
        {
            history_stats = request.statistics;
        }
        match leaf.as_str() {
            "" | LEAF_DASHBOARD => self.dashboard_handler(
                AdminSource::PageSpeedAdmin,
                fetch,
                history_stats,
                Some(request.global_options),
                stored.as_deref(),
            ),
            LEAF_HISTORY => self.history_archive_handler(
                AdminSource::PageSpeedAdmin,
                fetch,
                request.global_options,
            ),
            LEAF_IMAGES => self.images_handler(
                AdminSource::PageSpeedAdmin,
                fetch,
                stored.as_deref(),
                &controls,
            ),
            LEAF_HTML_CACHE => self.html_cache_handler(
                AdminSource::PageSpeedAdmin,
                fetch,
                history_stats,
                options,
                stored.as_deref(),
                &controls,
                Some(request.global_options),
            ),
            LEAF_MESSAGE_HISTORY | LEAF_JAVASCRIPT | LEAF_CSS => self.message_history_handler(
                options,
                AdminSource::PageSpeedAdmin,
                fetch,
                stored.as_deref(),
                &controls,
                kind,
            ),
            LEAF_CONSOLE => self.console_handler(
                request.global_options,
                options,
                AdminSource::PageSpeedAdmin,
                query,
                fetch,
                request.statistics,
            ),
            LEAF_GRAPHS => self.graphs_handler(
                options,
                AdminSource::PageSpeedAdmin,
                query,
                fetch,
                request.statistics,
            ),
            LEAF_STATISTICS => {
                self.statistics_handler(options, AdminSource::PageSpeedAdmin, fetch, request.stats)
            }
            LEAF_STATS_JSON => self.statistics_json_handler(fetch, request.stats),
            LEAF_CONFIG => self.print_config(AdminSource::PageSpeedAdmin, fetch, options),
            LEAF_CACHE => self.print_caches(
                AdminSource::PageSpeedAdmin,
                is_global,
                stripped_url,
                query,
                options,
                fetch,
                request,
            ),
            LEAF_HISTOGRAMS => {
                self.print_histograms(AdminSource::PageSpeedAdmin, fetch, request.stats)
            }
            _ => {
                fetch.response_headers_mut().set_status_and_reason(404);
                fetch
                    .response_headers_mut()
                    .headers_mut()
                    .add("Content-Type", "text/html");
                fetch.handle_write(b"Unknown admin page: ");
                write_pre(fetch, &leaf);
                // It's possible that the handler is installed on /a/b/c, and we
                // are now reporting "unknown admin page: c". This is kind of a guess,
                // but provide a nice link here to what might be the correct admin page.
                //
                // This is just a guess, so we don't want to redirect.
                fetch.handle_write(b"<br/>Did you mean to visit: ");
                let guess = format!("{}/", stripped_url.all_except_query());
                let escaped = escape_html(&guess);
                fetch.handle_write(format!("<a href='{escaped}'>{escaped}</a>\n").as_bytes());
                fetch.handle_done(true);
            }
        }
    }

    /// Routes one legacy statistics URL (`StatisticsPage`).
    pub fn statistics_page(
        &self,
        is_global: bool,
        query: &QueryParams,
        options: &RewriteOptions,
        fetch: &mut dyn AsyncFetch,
        request: &mut AdminRequest<'_>,
    ) {
        if query.has("json") {
            self.console_json_handler(query, fetch, request.statistics);
        } else if query.has("config") {
            self.print_config(AdminSource::Statistics, fetch, options);
        } else if query.has("histograms") {
            self.print_histograms(AdminSource::Statistics, fetch, request.stats);
        } else if query.has("graphs") {
            self.graphs_handler(
                options,
                AdminSource::Statistics,
                query,
                fetch,
                request.statistics,
            );
        } else if query.has("cache") {
            let empty_url = GoogleUrl::new("");
            self.print_caches(
                AdminSource::Statistics,
                is_global,
                &empty_url,
                query,
                options,
                fetch,
                request,
            );
        } else {
            self.statistics_handler(options, AdminSource::Statistics, fetch, request.stats);
        }
    }

    /// Takes a slot, waiting briefly (`AcquireExpensiveSlot`).
    fn acquire_expensive_slot(&self, leaf: ExpensiveLeaf, wait_ms: u64) -> bool {
        const POLL_MS: u64 = 5;
        let slot = &self.expensive_in_flight[leaf.index()];
        let mut waited_ms = 0;
        loop {
            if slot
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
            if waited_ms >= wait_ms {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            waited_ms += POLL_MS;
        }
    }
}

/// Whether a purge target stays within the vhost
/// (`TargetBelongsToRequestHost`).
fn target_belongs_to_request_host(request_url: &GoogleUrl, target: &GoogleUrl) -> bool {
    request_url.host().eq_ignore_ascii_case(&target.host())
        && explicit_port(request_url) == explicit_port(target)
}

/// Explicit port, defaulting to 0 (`ExplicitPort`).
fn explicit_port(url: &GoogleUrl) -> i32 {
    let port = url.int_port();
    if port == PORT_UNSPECIFIED {
        0
    } else {
        port
    }
}

/// Parses a UTC history date (`HistoryDate`): strict
/// `YYYY-MM-DD`, year 1970+, normalization round-trips.
fn history_date_ms(date: &str) -> Option<i64> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = date[0..4].parse().ok()?;
    let month: i64 = date[5..7].parse().ok()?;
    let day: i64 = date[8..10].parse().ok()?;
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let (back_year, back_month, back_day) = civil_from_days(days);
    if back_year != year || back_month != month || back_day != day {
        return None;
    }
    let ms = days * 86_400_000;
    if ms < 0 {
        return None;
    }
    Some(ms)
}

/// Days since the Unix epoch (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year.rem_euclid(400);
    let month_prime = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Civil date from days since the epoch (inverse).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719468;
    let era = shifted.div_euclid(146097);
    let day_of_era = shifted.rem_euclid(146097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Formats a purge timestamp (`ConvertTimeToString`).
fn format_purge_time(time_ms: i64) -> Option<String> {
    if (-62135596800000..=253402300799999).contains(&time_ms) {
        Some(ps_http::time::format_http_date(time_ms))
    } else {
        None
    }
}

/// Purge-set records for one vhost (`PurgeSetStringForRequestHost`).
fn purge_set_string_for_request_host(purge_set: &PurgeSet, stripped_url: &GoogleUrl) -> String {
    let mut out = format!(
        "Global@{}",
        format_purge_time(purge_set.global_timestamp_ms())
            .unwrap_or_else(|| purge_set.global_timestamp_ms().to_string())
    );
    for (url, timestamp) in purge_set.iter() {
        let parsed = GoogleUrl::new(url);
        if !parsed.is_web_valid() || !target_belongs_to_request_host(stripped_url, &parsed) {
            continue;
        }
        out.push_str(&format!(
            "\n{url}@{}",
            format_purge_time(timestamp).unwrap_or_else(|| timestamp.to_string())
        ));
    }
    out
}

/// Cache lookup form (`ShowCacheForm`).
fn show_cache_form(user_agent: &str) -> String {
    let mut ua_default = String::new();
    if !user_agent.is_empty() {
        ua_default = format!("value=\"{}\" ", escape_html(user_agent));
    }
    format!(
        "<form class='pagespeed-cache-lookup'>\n  <label for='metadata_text'>Resource URL</label>  <input id='metadata_text' type='url' name='url' placeholder='https://example.com/page' autocomplete='url' />\n  <label for='user_agent'>User-Agent</label>  <input id='user_agent' type='text' name='user_agent' {ua_default}/>\n  <div class='pagespeed-cache-lookup-actions'><button id='metadata_submit' type='button'>Inspect entry</button><button id='metadata_clear' type='reset' class='secondary'>Clear</button></div></form>\n"
    )
}

/// Cache structure table frame (`kTableStart`).
const CACHE_TABLE_START: &str = "<div class='pagespeed-admin-table-wrap'><table class='pagespeed-admin-data-table pagespeed-caches-structure'>\n  <thead>\n    <tr>\n      <th>Cache</th><th>Detail</th><th>Structure</th>\n    </tr>\n  </thead>\n  <tbody>";

/// Cache structure table frame (`kTableEnd`).
const CACHE_TABLE_END: &str = "  </tbody>\n</table></div>";

/// Backend keywords worth surfacing (`HackCacheDescriptor`).
const CACHE_KEYWORDS: &[&str] = &[
    "Compressed",
    "Async",
    "SharedMemCache",
    "LRUCache",
    "AprMemCache",
    "FileCache",
    "RedisCache",
];

/// Strips a cache descriptor to its main keywords
/// (`HackCacheDescriptor`).
fn hack_cache_descriptor(name: &str) -> String {
    let mut out = String::new();
    let mut delimiter = "";
    for keyword in CACHE_KEYWORDS {
        if name.contains(keyword) {
            out.push_str(delimiter);
            out.push_str(keyword);
            delimiter = " ";
        }
    }
    if out.is_empty() {
        out.push_str(name);
    }
    out
}

/// Indents a cache descriptor by parenthesis depth
/// (`IndentCacheDescriptor`).
fn indent_cache_descriptor(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for byte in name.bytes() {
        out.push_str(&escape_html(&String::from_utf8_lossy(&[byte])));
        match byte {
            b'(' => {
                depth += 1;
                out.push_str("<br/>");
                for _ in 0..depth {
                    out.push_str("&nbsp; &nbsp;");
                }
            }
            b',' => {
                out.push_str("<br/>");
                for _ in 0..depth {
                    out.push_str("&nbsp; &nbsp;");
                }
            }
            b')' => {
                depth -= 1;
            }
            _ => {}
        }
    }
    out
}

/// One cache-structure row (`CacheInfoHtmlSnippet`).
fn cache_info_html_snippet(label: &str, descriptor: &str) -> String {
    format!(
        "<tr style='vertical-align:top;'><td>{label}</td><td><input id='{label}_toggle' type='checkbox' onclick=\"pagespeed.Caches.toggleDetail('{label}')\"/></td><td><code id='{label}_summary'>{}</code><code id='{label}_detail' style='display:none;'>{}</code></td></tr>\n",
        escape_html(&hack_cache_descriptor(descriptor)),
        indent_cache_descriptor(descriptor),
    )
}

/// RAII expensive-read guard (`ExpensiveSlot`).
struct ExpensiveGuard<'a> {
    site: &'a AdminSite,
    leaf: ExpensiveLeaf,
    held: bool,
}

impl<'a> ExpensiveGuard<'a> {
    /// Takes a slot with the default wait.
    fn take(site: &'a AdminSite, leaf: ExpensiveLeaf) -> Self {
        Self::take_wait(site, leaf, EXPENSIVE_SLOT_WAIT_MS)
    }

    /// Takes a slot with an explicit wait.
    fn take_wait(site: &'a AdminSite, leaf: ExpensiveLeaf, wait_ms: u64) -> Self {
        let held = site.acquire_expensive_slot(leaf, wait_ms);
        ExpensiveGuard { site, leaf, held }
    }

    /// Whether the slot was taken.
    fn held(&self) -> bool {
        self.held
    }
}

impl Drop for ExpensiveGuard<'_> {
    fn drop(&mut self) {
        if self.held {
            self.site.release_expensive_slot(self.leaf);
        }
    }
}

/// Escapes text for HTML (`HtmlKeywords::Escape`).
fn escape_html(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    html_escape_bytes(text.as_bytes(), &mut out);
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// Writes `<pre>`-wrapped escaped text (`WritePre`).
fn write_pre(fetch: &mut dyn AsyncFetch, text: &str) {
    let body = format!("<pre>{}</pre>\n", escape_html(text));
    fetch.handle_write(body.as_bytes());
}

/// Navigation tabs (`kTabs`): label, title, admin link,
/// statistics link.
const TABS: &[(&str, &str, &str, Option<&str>)] = &[
    ("Dashboard", "Dashboard", "dashboard", None),
    ("Historical Archive", "Historical Archive", "history", None),
    ("Images", "Optimized Images", "images", None),
    ("HTML Cache", "HTML Cache", "html_cache", None),
    ("JavaScript", "JavaScript Diagnostics", "javascript", None),
    ("CSS", "CSS Diagnostics", "css", None),
    ("Statistics", "Statistics", "statistics", Some("?")),
    ("Configuration", "Configuration", "config", Some("?config")),
    (
        "Histograms",
        "Histograms",
        "histograms",
        Some("?histograms"),
    ),
    ("Caches", "Caches", "cache", Some("?cache")),
    ("Console", "Console", "console", None),
    (
        "Message History",
        "Message History",
        "message_history",
        None,
    ),
    ("Graphs", "Graphs", "graphs", None),
];

/// Opens an admin page (`AdminHtml` constructor half): status,
/// headers, banner, and navigation.
fn open_admin_html(
    current_link: &str,
    head_extra: &str,
    source: AdminSource,
    now_ms: i64,
    fetch: &mut dyn AsyncFetch,
) {
    fetch.response_headers_mut().set_status_and_reason(200);
    fetch
        .response_headers_mut()
        .headers_mut()
        .add("Content-Type", "text/html");
    fetch.response_headers_mut().set_last_modified(now_ms);
    // Let PageSpeed dynamically minify the html, css, and javasript
    // generated by the admin page, to the extent it's not done
    // already by the tools.
    fetch.response_headers_mut().headers_mut().add(
        "PageSpeedFilters",
        "rewrite_css,rewrite_javascript,collapse_whitespace",
    );
    fetch.handle_write(
        b"<!DOCTYPE html>\n<html><head><meta name='viewport' content='width=device-width, initial-scale=1'>",
    );
    fetch.handle_write(format!("<style>{}</style>", stylesheet("admin")).as_bytes());
    let mut tabs = String::new();
    for (label, title, admin_link, statistics_link) in TABS {
        let link = match source {
            AdminSource::PageSpeedAdmin => Some(*admin_link),
            AdminSource::Statistics => *statistics_link,
            AdminSource::Other => None,
        };
        if let Some(link) = link {
            let mut style = String::new();
            if *admin_link == current_link {
                style = " class='pagespeed-admin-tab-active' aria-current='page'".to_string();
                fetch.handle_write(format!("<title>PageSpeed {title}</title>").as_bytes());
            }
            tabs.push_str(&format!("<a href='{link}'{style}>{label}</a>\n"));
        }
    }
    fetch.handle_write(format!("{head_extra}</head>").as_bytes());
    fetch.handle_write(
        b"<body class='pagespeed-admin-body'><header class='pagespeed-admin-header'><div class='pagespeed-admin-brand'><span class='pagespeed-admin-brand-mark' aria-hidden='true'></span>mod_pagespeed</div><span class='pagespeed-admin-header-label'>ADMIN CONSOLE</span></header><div class='pagespeed-admin-layout'><aside class='pagespeed-admin-sidebar'><div class='pagespeed-admin-sidebar-label'>WORKSPACE</div><nav class='pagespeed-admin-tabs' aria-label='PageSpeed administration'>",
    );
    fetch.handle_write(tabs.as_bytes());
    fetch.handle_write(b"</nav></aside><main class='pagespeed-admin-content'>\n");
}

/// Closes an admin page (`AdminHtml` destructor half).
fn close_admin_html(fetch: &mut dyn AsyncFetch) {
    fetch.handle_write(b"</main></div></body></html>");
    fetch.handle_done(true);
}

/// Plain-text 429 (`WriteAdminBusy`).
fn write_admin_busy(fetch: &mut dyn AsyncFetch) {
    fetch.response_headers_mut().set_status_code(429);
    fetch
        .response_headers_mut()
        .headers_mut()
        .add("Content-Type", "text/plain");
    fetch
        .response_headers_mut()
        .headers_mut()
        .add("Cache-Control", "no-store, private");
    fetch
        .response_headers_mut()
        .headers_mut()
        .add("X-Content-Type-Options", "nosniff");
    fetch.handle_write(b"busy: another expensive admin read is in flight; retry");
    fetch.handle_done(true);
}

/// Page heading (`WriteAdminPageHeading`).
fn write_heading(fetch: &mut dyn AsyncFetch, eyebrow: &str, title: &str, description: &str) {
    fetch.handle_write(
        format!(
            "<div class='pagespeed-dashboard-heading'><div><p class='pagespeed-dashboard-eyebrow'>{eyebrow}</p><h1>{title}</h1><p class='pagespeed-dashboard-description'>{description}</p></div></div>"
        )
        .as_bytes(),
    );
}

/// Formats byte counts (`FormatDashboardBytes`): `0 B`,
/// `4.0 KB`, `5.8 MB`, ….
fn format_dashboard_bytes(bytes: i64) -> String {
    const KILOBYTE: i64 = 1024;
    const MEGABYTE: i64 = KILOBYTE * 1024;
    const GIGABYTE: i64 = MEGABYTE * 1024;
    let (unit, suffix) = if bytes >= GIGABYTE {
        (GIGABYTE, "GB")
    } else if bytes >= MEGABYTE {
        (MEGABYTE, "MB")
    } else if bytes >= KILOBYTE {
        (KILOBYTE, "KB")
    } else {
        return format!("{bytes} B");
    };
    format!("{}.{} {suffix}", bytes / unit, (bytes % unit) * 10 / unit)
}

/// Formats uptime (`FormatDashboardUptime`).
fn format_dashboard_uptime(elapsed_ms: i64) -> String {
    let elapsed_seconds = (elapsed_ms / 1000).max(0);
    let days = elapsed_seconds / 86400;
    let hours = (elapsed_seconds / 3600) % 24;
    let minutes = (elapsed_seconds / 60) % 60;
    let seconds = elapsed_seconds % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}h {minutes}m {seconds}s")
    }
}

/// Whole percent (`DashboardPercent`).
fn dashboard_percent(value: i64, total: i64) -> i64 {
    if total <= 0 {
        0
    } else {
        value * 100 / total
    }
}

/// One image rewrite record (`ImageRewriteRow`).
#[derive(Debug, Clone, Default)]
struct ImageRewriteRow {
    timestamp_ms: i64,
    original_bytes: i64,
    optimized_bytes: i64,
    output_mime_type: String,
    original_url: String,
    optimized_url: String,
}

/// Reads recent image rewrites newest-first
/// (`ReadRecentImageRewrites`).
fn read_recent_image_rewrites(log: &str, max_rows: usize, rows: &mut Vec<ImageRewriteRow>) {
    for message in LogBuffer::parse_dump(log).iter().rev() {
        if rows.len() >= max_rows {
            break;
        }
        let Some(marker) = message.find("PS_IMAGE_DETAIL\t") else {
            continue;
        };
        let fields: Vec<&str> = message[marker..]
            .split('\t')
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() != 7 {
            continue;
        }
        let (Some(timestamp_ms), Some(original_bytes), Some(optimized_bytes)) = (
            fields[1].parse::<i64>().ok(),
            fields[2].parse::<i64>().ok(),
            fields[3].parse::<i64>().ok(),
        ) else {
            continue;
        };
        if original_bytes < optimized_bytes {
            continue;
        }
        rows.push(ImageRewriteRow {
            timestamp_ms,
            original_bytes,
            optimized_bytes,
            output_mime_type: fields[4].to_string(),
            original_url: fields[5].to_string(),
            optimized_url: fields[6].to_string(),
        });
    }
}

/// One cookie-bypass record (`PS_HTML_BYPASS` history line).
#[derive(Debug, Clone, Default)]
struct HtmlCacheBypassRow {
    timestamp_ms: i64,
    reason: String,
    url: String,
}

/// Reads recent cookie bypasses newest-first, capped at 20.
fn read_recent_cookie_bypasses(log: &str, rows: &mut Vec<HtmlCacheBypassRow>) {
    for message in LogBuffer::parse_dump(log).iter().rev() {
        if rows.len() >= 20 {
            break;
        }
        let Some(marker) = message.find("PS_HTML_BYPASS\t") else {
            continue;
        };
        let fields: Vec<&str> = message[marker..]
            .split('\t')
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() != 4 {
            continue;
        }
        let Some(timestamp_ms) = fields[1].parse::<i64>().ok() else {
            continue;
        };
        if timestamp_ms <= 0 || !valid_bypass_reason(fields[2]) {
            continue;
        }
        if fields[3].len() > 6000 {
            continue;
        }
        let Some(url_bytes) = ps_cache::base64::web64_decode(fields[3]) else {
            continue;
        };
        let Ok(url) = String::from_utf8(url_bytes) else {
            continue;
        };
        if url.len() > 4096 || !GoogleUrl::new(&url).is_web_valid() {
            continue;
        }
        rows.push(HtmlCacheBypassRow {
            timestamp_ms,
            reason: fields[2].to_string(),
            url,
        });
    }
}

/// Whether a bypass reason token is well-formed (`cookie:<name>`,
/// `malformed-cookie`, or `header:Cookie2`).
fn valid_bypass_reason(reason: &str) -> bool {
    if reason == "malformed-cookie" || reason == "header:Cookie2" {
        return true;
    }
    let Some(name) = reason.strip_prefix("cookie:") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= ps_config::cookies::MAX_REASON_NAME_LEN
        && name.bytes().all(|byte| byte.is_ascii_graphic())
}

/// One HTML cache record (`HtmlCacheRow`).
#[derive(Debug, Clone, Default)]
struct HtmlCacheRow {
    created_ms: i64,
    expires_ms: i64,
    bytes: i64,
    device: String,
    formats: String,
    variant: String,
    url: String,
}

/// Reads recent HTML rewrites newest-first, deduplicated
/// (`ReadRecentHtmlRewrites`).
fn read_recent_html_rewrites(log: &str, rows: &mut Vec<HtmlCacheRow>) {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    for message in LogBuffer::parse_dump(log).iter().rev() {
        if rows.len() >= 100 {
            break;
        }
        let Some(marker) = message.find("PS_HTML_DETAIL\t") else {
            continue;
        };
        let fields: Vec<&str> = message[marker..]
            .split('\t')
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() != 8 {
            continue;
        }
        let (Some(created_ms), Some(expires_ms), Some(bytes)) = (
            fields[1].parse::<i64>().ok(),
            fields[2].parse::<i64>().ok(),
            fields[3].parse::<i64>().ok(),
        ) else {
            continue;
        };
        if created_ms <= 0
            || expires_ms <= created_ms
            || bytes <= 0
            || bytes > 1024 * 1024
            || fields[7].len() > 6000
        {
            continue;
        }
        let Some(url_bytes) = ps_cache::base64::web64_decode(fields[7]) else {
            continue;
        };
        let Ok(url) = String::from_utf8(url_bytes) else {
            continue;
        };
        if url.len() > 4096 || !GoogleUrl::new(&url).is_web_valid() {
            continue;
        }
        if !fields[6].starts_with("optimized-html-v1:") || fields[6].len() > 128 {
            continue;
        }
        if fields[4] != "desktop" && fields[4] != "mobile" && fields[4] != "tablet" {
            continue;
        }
        if fields[5] != "AVIF capable"
            && fields[5] != "WebP capable"
            && fields[5] != "Original formats"
        {
            continue;
        }
        if seen.insert(fields[6].to_string()) {
            rows.push(HtmlCacheRow {
                created_ms,
                expires_ms,
                bytes,
                device: fields[4].to_string(),
                formats: fields[5].to_string(),
                variant: fields[6].to_string(),
                url,
            });
        }
    }
}
