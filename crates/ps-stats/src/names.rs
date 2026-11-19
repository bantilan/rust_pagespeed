//! Audited statistic names and per-owner registration.
//!
//! Every name below is byte-identical to its C++ registration (including
//! historical typos such as `low-priority-worked-queue-depth` and the
//! truncated `show_ads_snippets_not_converte`). Each `init_*` function
//! mirrors one `::InitStats` body; [`init_all`] wires the full set in
//! factory order (`RewriteDriverFactory::InitStats` +
//! `SystemCaches::InitStats` + fetcher/recorder/context stats).
//!
//! Kinds: `V` = `AddVariable`, `U` = `AddUpDownCounter`, `G` =
//! `AddGlobalUpDownCounter`, `H` = `AddHistogram`, `T` =
//! `AddTimedVariable` (default group).
#![forbid(unsafe_code)]

use crate::core::{Statistics, DEFAULT_TIMED_GROUP};

/// Registers one variable.
fn var(stats: &Statistics, name: &str) {
    stats.add_variable(name);
}

/// Registers one up/down counter.
fn updown(stats: &Statistics, name: &str) {
    stats.add_up_down_counter(name);
}

/// Registers one global up/down counter.
fn global(stats: &Statistics, name: &str) {
    stats.add_global_up_down_counter(name);
}

/// Registers one histogram.
fn hist(stats: &Statistics, name: &str) {
    stats.add_histogram(name);
}

/// Registers one timed variable in the default group.
fn timed(stats: &Statistics, name: &str) {
    stats.add_timed_variable(name, DEFAULT_TIMED_GROUP);
}

/// `HTTPCache::InitStats` (9 variables).
pub fn init_http_cache(stats: &Statistics) {
    for name in [
        "cache_time_us",
        "cache_hits",
        "cache_misses",
        "cache_backend_hits",
        "cache_backend_misses",
        "cache_fallbacks",
        "cache_expirations",
        "cache_inserts",
        "cache_deletes",
    ] {
        var(stats, name);
    }
}

/// `RewriteDriver::InitStats`: 5 HTML-cache variables plus the 27-filter
/// fan-out (closes admin-gap-1: every fan-out target enumerated).
pub fn init_rewrite_driver(stats: &Statistics) {
    for name in [
        "html_cache_hits",
        "html_cache_misses",
        "html_cache_bypasses",
        "html_cache_inserts",
        "html_cache_bytes_served",
    ] {
        var(stats, name);
    }
    init_add_instrumentation(stats);
    init_cache_extender(stats);
    init_critical_css_beacon(stats);
    init_critical_images_beacon(stats);
    init_css_combine(stats);
    init_css_filter(stats);
    init_css_inline(stats);
    init_css_inline_import_to_link(stats);
    init_css_move_to_head(stats);
    init_css_summarizer_base(stats);
    init_dedup_inlined_images(stats);
    init_domain_rewrite(stats);
    init_google_analytics(stats);
    init_google_font_css_inline(stats);
    init_image_combine(stats);
    init_image_rewrite(stats);
    init_in_place_rewrite_context(stats);
    init_insert_ga(stats);
    init_javascript(stats);
    init_js_combine(stats);
    init_js_inline(stats);
    init_local_storage_cache(stats);
    init_make_show_ads_async(stats);
    init_meta_tag(stats);
    init_rewrite_context(stats);
    init_url_input_resource(stats);
    init_url_left_trim(stats);
}

/// `RewriteStats::InitStats`.
pub fn init_rewrite_stats(stats: &Statistics) {
    for name in [
        "resource_url_domain_acceptances",
        "resource_url_domain_rejections",
        "rewrite_cached_output_missed_deadline",
        "rewrite_cached_output_hits",
        "rewrite_cached_output_misses",
        "resource_404_count",
        "slurp_404_count",
        "total_page_load_ms",
        "page_load_count",
        "resource_fetches_cached",
        "resource_fetch_construct_successes",
        "resource_fetch_construct_failures",
        "num_cache_control_rewritable_resources",
        "num_cache_control_not_rewritable_resources",
        "num_flushes",
    ] {
        var(stats, name);
    }
    for name in [
        "Beacon Reported Load Time (ms)",
        "Pagespeed Resource Latency Histogram",
        "Rewrite Latency Histogram",
        "Backend Fetch First Byte Latency Histogram",
    ] {
        hist(stats, name);
    }
    for name in [
        "num_fallback_responses_served",
        "num_proactively_freshen_user_facing_request",
        "num_fallback_responses_served_while_revalidate",
        "num_conditional_refreshes",
        "ipro_served",
        "ipro_not_in_cache",
        "ipro_not_rewritable",
        "downstream_cache_purge_attempts",
        "successful_downstream_cache_purges",
    ] {
        var(stats, name);
    }
    for name in [
        "total_fetch_count",
        "total_rewrite_count",
        "num_rewrites_executed",
        "num_rewrites_dropped",
    ] {
        timed(stats, name);
    }
    for name in [
        "num_resource_fetch_successes",
        "num_resource_fetch_failures",
    ] {
        var(stats, name);
    }
    // Waveform counters (`kWaveFormCounters`; note "worked" is verbatim).
    for name in [
        "html-worker-queue-depth",
        "rewrite-worker-queue-depth",
        "low-priority-worked-queue-depth",
    ] {
        updown(stats, name);
    }
}

/// `CacheBatcher::InitStats`.
pub fn init_cache_batcher(stats: &Statistics) {
    for name in [
        "cache_batcher_dropped_gets",
        "cache_batcher_coalesced_gets",
        "cache_batcher_queued_gets",
    ] {
        var(stats, name);
    }
}

/// `CriticalImagesFinder::InitStats`.
pub fn init_critical_images_finder(stats: &Statistics) {
    for name in [
        "critical_images_valid_count",
        "critical_images_expired_count",
        "critical_images_not_found_count",
    ] {
        var(stats, name);
    }
}

/// `CriticalSelectorFinder::InitStats` (timed).
pub fn init_critical_selector_finder(stats: &Statistics) {
    for name in [
        "critical_selectors_valid_count",
        "critical_selectors_expired_count",
        "critical_selectors_not_found_count",
    ] {
        timed(stats, name);
    }
}

/// `PropertyStoreGetCallback::InitStats`.
pub fn init_property_store(stats: &Statistics) {
    hist(stats, "PropertyStoreLatencyAfterFastFinishCalledMs");
}

/// `InProcessCentralController::InitStats` (four sub-controllers).
pub fn init_central_controller(stats: &Statistics) {
    for name in [
        "named-lock-rewrite-scheduler-granted",
        "named-lock-rewrite-scheduler-denied",
        "named-lock-rewrite-scheduler-stolen",
        "named-lock-rewrite-scheduler-released-not-held",
    ] {
        timed(stats, name);
    }
    updown(stats, "named-lock-rewrite-scheduler-locks-held");
    for name in [
        "popularity-contest-num-rewrites-requested",
        "popularity-contest-num-rewrites-succeeded",
        "popularity-contest-num-rewrites-failed",
        "popularity-contest-num-rewrites-rejected-queue-full",
        "popularity-contest-num-rewrites-rejected-already-running",
    ] {
        timed(stats, name);
    }
    for name in [
        "popularity-contest-queue-size",
        "popularity-contest-num-rewrites-running",
        "popularity-contest-num-rewrites-awaiting-retry",
    ] {
        updown(stats, name);
    }
    global(stats, "active-expensive-operations");
    global(stats, "queued-expensive-operations");
    timed(stats, "permitted-expensive-operations");
    global(stats, "current-expensive-operations");
}

/// `CacheStats::InitStats(prefix)`: 5 histograms + 4 variables.
pub fn init_cache_stats(prefix: &str, stats: &Statistics) {
    for suffix in [
        "_get_count",
        "_hit_latency_us",
        "_insert_latency_us",
        "_insert_size_bytes",
        "_lookup_size_bytes",
    ] {
        hist(stats, &format!("{prefix}{suffix}"));
    }
    for suffix in ["_deletes", "_hits", "_inserts", "_misses"] {
        var(stats, &format!("{prefix}{suffix}"));
    }
}

/// Cache-backend prefixes (`SystemCaches::InitStats`).
pub const CACHE_BACKEND_PREFIXES: &[&str] = &[
    "file_cache",
    "lru_cache",
    "shm_cache",
    "memcached_async",
    "memcached_blocking",
    "redis_async",
    "redis_blocking",
];

/// `FileCache::InitStats`.
pub fn init_file_cache(stats: &Statistics) {
    for name in [
        "file_cache_bytes_freed_in_cleanup",
        "file_cache_cleanups",
        "file_cache_disk_checks",
        "file_cache_evictions",
        "file_cache_skipped_cleanups",
        "file_cache_started_cleanups",
        "file_cache_write_errors",
    ] {
        var(stats, name);
    }
}

/// `CompressedCache::InitStats` (the savings histogram is
/// `INCLUDE_HISTOGRAMS`-gated in C++; registered here unconditionally and
/// documented, since the logger never reads it).
pub fn init_compressed_cache(stats: &Statistics) {
    hist(stats, "compressed_cache_savings");
    for name in [
        "compressed_cache_corrupt_payloads",
        "compressed_cache_original_size",
        "compressed_cache_compressed_size",
    ] {
        var(stats, name);
    }
}

/// `PurgeContext::InitStats` (note the leading underscore on the poll
/// timestamp, verbatim).
pub fn init_purge_context(stats: &Statistics) {
    for name in [
        "purge_cancellations",
        "purge_contentions",
        "purge_file_parse_failures",
        "purge_file_stats",
        "purge_file_writes",
        "purge_file_write_failures",
        "purge_index",
    ] {
        var(stats, name);
    }
    updown(stats, "_purge_poll_timestamp_ms");
}

/// `RedisCache::InitStats`.
pub fn init_redis_cache(stats: &Statistics) {
    for name in ["redis_cluster_redirections", "redis_cluster_slots_fetches"] {
        var(stats, name);
    }
}

/// `AprMemCache::InitStats`.
pub fn init_mem_cache(stats: &Statistics) {
    var(stats, "memcache_timeouts");
    updown(stats, "memcache_last_error_checkpoint_ms");
    updown(stats, "memcache_error_burst_size");
}

/// `SerfUrlAsyncFetcher::InitStats` (release set; the `num_calls_to_read`
/// variable is `#ifndef NDEBUG`-only in C++ and omitted here).
pub fn init_serf_fetcher(stats: &Statistics) {
    for name in [
        "serf_fetch_request_count",
        "serf_fetch_bytes_count",
        "serf_fetch_time_duration_ms",
        "serf_fetch_cancel_count",
        "serf_fetch_timeout_count",
        "serf_fetch_failure_count",
        "serf_fetch_cert_errors",
        "serf_fetch_ultimate_success",
        "serf_fetch_ultimate_failure",
    ] {
        var(stats, name);
    }
    updown(stats, "serf_fetch_active_count");
    updown(stats, "serf_fetch_last_check_timestamp_ms");
}

/// `InPlaceResourceRecorder::InitStats`.
pub fn init_ipro_recorder(stats: &Statistics) {
    for name in [
        "ipro_recorder_resources",
        "ipro_recorder_inserted_into_cache",
        "ipro_recorder_not_cacheable",
        "ipro_recorder_failed",
        "ipro_recorder_dropped_due_to_load",
        "ipro_recorder_dropped_due_to_size",
    ] {
        var(stats, name);
    }
}

/// `SystemServerContext` flush stats (lazy-registered in C++).
pub fn init_cache_flush(stats: &Statistics) {
    var(stats, "cache_flush_count");
    updown(stats, "cache_flush_timestamp_ms");
}

/// `UrlAsyncFetcherStats::InitStats("http")`.
pub fn init_http_fetcher(stats: &Statistics) {
    let histogram = stats.add_histogram("http_fetch_latency_us");
    histogram.set_max_value(500_000.0); // 500ms in us, as in C++.
    for name in [
        "http_fetches",
        "http_bytes_fetched",
        "http_approx_header_bytes_fetched",
    ] {
        var(stats, name);
    }
}

/// Property-cache cohort stats: full `CacheStats` family per
/// `pcache-cohorts-<cohort>` prefix.
pub fn init_property_cache_cohort(cohort: &str, stats: &Statistics) {
    init_cache_stats(&format!("pcache-cohorts-{cohort}"), stats);
}

/// Registers the full audited set in factory order.
pub fn init_all(stats: &Statistics) {
    init_http_cache(stats);
    init_rewrite_driver(stats);
    init_rewrite_stats(stats);
    init_cache_batcher(stats);
    init_central_controller(stats);
    init_critical_images_finder(stats);
    init_critical_selector_finder(stats);
    init_property_store(stats);
    for prefix in CACHE_BACKEND_PREFIXES {
        init_cache_stats(prefix, stats);
    }
    init_mem_cache(stats);
    init_file_cache(stats);
    init_compressed_cache(stats);
    init_purge_context(stats);
    init_redis_cache(stats);
    init_serf_fetcher(stats);
    init_ipro_recorder(stats);
    init_cache_flush(stats);
    init_http_fetcher(stats);
    // Known cohorts (see graphs var list).
    init_property_cache_cohort("dom", stats);
    init_property_cache_cohort("beacon_cohort", stats);
}

// -- 27-filter fan-out bodies (RewriteDriver::InitStats order) --

/// `AddInstrumentationFilter::InitStats`.
pub fn init_add_instrumentation(stats: &Statistics) {
    var(stats, "instrumentation_filter_script_added_count");
}

/// `CacheExtender::InitStats`.
pub fn init_cache_extender(stats: &Statistics) {
    var(stats, "cache_extensions");
    var(stats, "not_cacheable");
}

/// `CriticalCssBeaconFilter::InitStats`.
pub fn init_critical_css_beacon(stats: &Statistics) {
    for name in [
        "critical_css_beacon_filter_script_added_count",
        "critical_css_no_beacon_due_to_missing_data",
        "critical_css_skipped_due_to_charset",
    ] {
        var(stats, name);
    }
}

/// `CriticalImagesBeaconFilter::InitStats`.
pub fn init_critical_images_beacon(stats: &Statistics) {
    var(stats, "critical_images_beacon_filter_script_added_count");
}

/// `CssCombineFilter::InitStats`.
pub fn init_css_combine(stats: &Statistics) {
    var(stats, "css_combine_opportunities");
    var(stats, "css_file_count_reduction");
}

/// `CssFilter::InitStats` (bytes-saved is an up/down counter).
pub fn init_css_filter(stats: &Statistics) {
    for name in [
        "css_filter_blocks_rewritten",
        "css_filter_parse_failures",
        "css_filter_parse_max_bytes_exceeded",
        "css_filter_fallback_rewrites",
        "css_filter_fallback_failures",
        "css_filter_rewrites_dropped",
    ] {
        var(stats, name);
    }
    updown(stats, "css_filter_total_bytes_saved");
    for name in [
        "css_filter_total_original_bytes",
        "css_filter_uses",
        "flatten_imports_charset_mismatch",
        "flatten_imports_invalid_url",
        "flatten_imports_limit_exceeded",
        "flatten_imports_minify_failed",
        "flatten_imports_recursion",
        "flatten_imports_complex_queries",
    ] {
        var(stats, name);
    }
}

/// `CssInlineFilter::InitStats`.
pub fn init_css_inline(stats: &Statistics) {
    var(stats, "num_css_inlined");
}

/// `CssInlineImportToLinkFilter::InitStats`.
pub fn init_css_inline_import_to_link(stats: &Statistics) {
    var(stats, "css_imports_to_links");
}

/// `CssMoveToHeadFilter::InitStats`.
pub fn init_css_move_to_head(stats: &Statistics) {
    var(stats, "css_elements_moved");
}

/// `CssSummarizerBase::InitStats`.
pub fn init_css_summarizer_base(stats: &Statistics) {
    var(stats, "num_css_used_for_critical_css_computation");
    var(stats, "num_css_not_used_for_critical_css_computation");
}

/// `DedupInlinedImagesFilter::InitStats`.
pub fn init_dedup_inlined_images(stats: &Statistics) {
    var(stats, "num_dedup_inlined_images_candidates_found");
    var(stats, "num_dedup_inlined_images_candidates_replaced");
}

/// `DomainRewriteFilter::InitStats`.
pub fn init_domain_rewrite(stats: &Statistics) {
    var(stats, "domain_rewrites");
}

/// `GoogleAnalyticsFilter::InitStats`.
pub fn init_google_analytics(stats: &Statistics) {
    var(stats, "google_analytics_page_load_count");
    var(stats, "google_analytics_rewritten_count");
}

/// `GoogleFontCssInlineFilter::InitStats` via
/// `CacheableResourceBase::InitStats("font_service_input_resource")`.
pub fn init_google_font_css_inline(stats: &Statistics) {
    init_cacheable_resource("font_service_input_resource", stats);
}

/// `ImageCombineFilter::InitStats`.
pub fn init_image_combine(stats: &Statistics) {
    var(stats, "image_file_count_reduction");
}

/// `ImageRewriteFilter::InitStats`.
pub fn init_image_rewrite(stats: &Statistics) {
    for name in [
        "image_rewrites",
        "image_resized_using_rendered_dimensions",
        "image_norewrites_high_resolution",
        "image_rewrites_dropped_intentionally",
        "image_rewrites_dropped_decode_failure",
        "image_rewrites_dropped_mime_type_unknown",
        "image_rewrites_dropped_server_write_fail",
        "image_rewrites_dropped_nosaving_resize",
        "image_rewrites_dropped_nosaving_noresize",
    ] {
        var(stats, name);
    }
    timed(stats, "image_rewrites_dropped_due_to_load");
    timed(stats, "image_rewrites_squashing_for_mobile_screen");
    for name in [
        "image_rewrite_total_bytes_saved",
        "image_rewrite_total_original_bytes",
        "image_rewrite_uses",
        "image_inline",
        "image_webp_rewrites",
        "image_rewrite_latency_total_ms",
    ] {
        var(stats, name);
    }
    updown(stats, "image_ongoing_rewrites");
    hist(stats, "image_rewrite_latency_ok_ms");
    hist(stats, "image_rewrite_latency_failed_ms");
    for name in [
        "image_webp_conversion_gif_timeouts",
        "image_webp_conversion_png_timeouts",
        "image_webp_conversion_jpeg_timeouts",
        "image_webp_conversion_gif_animated_timeouts",
    ] {
        var(stats, name);
    }
    for name in [
        "image_webp_conversion_gif_success_ms",
        "image_webp_conversion_png_success_ms",
        "image_webp_conversion_jpeg_success_ms",
        "image_webp_conversion_gif_animated_success_ms",
        "image_webp_conversion_gif_failure_ms",
        "image_webp_conversion_png_failure_ms",
        "image_webp_conversion_jpeg_failure_ms",
        "image_webp_conversion_gif_animated_failure_ms",
    ] {
        hist(stats, name);
    }
    var(stats, "image_webp_alpha_timeouts");
    hist(stats, "image_webp_alpha_success_ms");
    hist(stats, "image_webp_alpha_failure_ms");
    var(stats, "image_webp_opaque_timeouts");
    hist(stats, "image_webp_opaque_success_ms");
    hist(stats, "image_webp_opaque_failure_ms");
    var(stats, "image_avif_conversion_timeouts");
    var(stats, "image_avif_conversion_shed");
    hist(stats, "image_avif_conversion_success_ms");
    hist(stats, "image_avif_conversion_failure_ms");
    hist(stats, "image_avif_conversion_child_cpu_ms");
}

/// `InPlaceRewriteContext::InitStats`.
pub fn init_in_place_rewrite_context(stats: &Statistics) {
    var(stats, "in_place_oversized_opt_stream");
    var(stats, "in_place_uncacheable_rewrites");
}

/// `InsertGAFilter::InitStats`.
pub fn init_insert_ga(stats: &Statistics) {
    var(stats, "inserted_ga_snippets");
}

/// `JavascriptFilter::InitStats` via `JavascriptRewriteConfig`.
pub fn init_javascript(stats: &Statistics) {
    for name in [
        "javascript_blocks_minified",
        "javascript_libraries_identified",
        "javascript_minification_failures",
        "javascript_total_bytes_saved",
        "javascript_total_original_bytes",
        "javascript_minify_uses",
        "javascript_reducing_minifications",
        "javascript_minification_disabled",
        "javascript_did_not_shrink",
        "javascript_failed_to_write",
    ] {
        var(stats, name);
    }
}

/// `JsCombineFilter::InitStats`.
pub fn init_js_combine(stats: &Statistics) {
    var(stats, "js_file_count_reduction");
}

/// `JsInlineFilter::InitStats`.
pub fn init_js_inline(stats: &Statistics) {
    var(stats, "num_js_inlined");
}

/// `LocalStorageCacheFilter::InitStats`.
pub fn init_local_storage_cache(stats: &Statistics) {
    for name in [
        "num_local_storage_cache_candidates_found",
        "num_local_storage_cache_stored_total",
        "num_local_storage_cache_stored_images",
        "num_local_storage_cache_stored_css",
        "num_local_storage_cache_candidates_added",
        "num_local_storage_cache_candidates_removed",
    ] {
        var(stats, name);
    }
}

/// `MakeShowAdsAsyncFilter::InitStats` (the truncated
/// `..._not_converte` spelling is verbatim C++).
pub fn init_make_show_ads_async(stats: &Statistics) {
    var(stats, "show_ads_snippets_converted");
    var(stats, "show_ads_snippets_not_converte");
    var(stats, "show_ads_api_replaced_for_async");
}

/// `MetaTagFilter::InitStats`.
pub fn init_meta_tag(stats: &Statistics) {
    var(stats, "converted_meta_tags");
}

/// `RewriteContext::InitStats` + `FetchContext::InitStats`.
pub fn init_rewrite_context(stats: &Statistics) {
    var(stats, "num_rewrites_abandoned_for_lock_contention");
    var(stats, "num_deadline_alarm_invocations");
}

/// `UrlInputResource::InitStats` via
/// `CacheableResourceBase::InitStats("url_input_resource")`.
pub fn init_url_input_resource(stats: &Statistics) {
    init_cacheable_resource("url_input_resource", stats);
}

/// `UrlLeftTrimFilter::InitStats`.
pub fn init_url_left_trim(stats: &Statistics) {
    var(stats, "url_trims");
    var(stats, "url_trim_saved_bytes");
}

/// `CacheableResourceBase::InitStats(prefix)`: 5 suffixed variables.
pub fn init_cacheable_resource(prefix: &str, stats: &Statistics) {
    for suffix in [
        "_hit",
        "_recent_fetch_failure",
        "_recent_uncacheable_miss",
        "_recent_uncacheable_failure",
        "_miss",
    ] {
        var(stats, &format!("{prefix}{suffix}"));
    }
}

/// Variables logged for `/pagespeed_console` (`kConsoleVars`, C++ order,
/// including the duplicated `image_norewrites_high_resolution`).
pub const CONSOLE_VARS: &[&str] = &[
    "serf_fetch_failure_count",
    "serf_fetch_request_count",
    "resource_url_domain_rejections",
    "resource_url_domain_acceptances",
    "num_cache_control_not_rewritable_resources",
    "num_cache_control_rewritable_resources",
    "cache_backend_misses",
    "cache_backend_hits",
    "cache_expirations",
    "css_filter_parse_failures",
    "css_filter_blocks_rewritten",
    "javascript_minification_failures",
    "javascript_blocks_minified",
    "image_rewrites",
    "image_rewrites_dropped_nosaving_resize",
    "image_rewrites_dropped_nosaving_noresize",
    "image_norewrites_high_resolution",
    "image_rewrites_dropped_decode_failure",
    "image_rewrites_dropped_server_write_fail",
    "image_rewrites_dropped_mime_type_unknown",
    "image_norewrites_high_resolution",
    "css_combine_opportunities",
    "css_file_count_reduction",
];

/// Other logged variables (`kOtherLoggedVars`, C++ order).
pub const OTHER_LOGGED_VARS: &[&str] = &[
    "num_flushes",
    "cache_hits",
    "cache_misses",
    "num_fallback_responses_served",
    "slurp_404_count",
    "page_load_count",
    "total_page_load_ms",
    "num_rewrites_executed",
    "num_rewrites_dropped",
    "resource_404_count",
    "serf_fetch_request_count",
    "serf_fetch_bytes_count",
    "image_ongoing_rewrites",
    "javascript_total_bytes_saved",
    "css_filter_total_bytes_saved",
    "image_rewrite_total_bytes_saved",
    "image_norewrites_high_resolution",
    "image_rewrites_dropped_due_to_load",
    "image_rewrites_dropped_intentionally",
    "flatten_imports_charset_mismatch",
    "flatten_imports_invalid_url",
    "flatten_imports_limit_exceeded",
    "flatten_imports_minify_failed",
    "flatten_imports_recursion",
    "css_filter_parse_failures",
    "converted_meta_tags",
    "javascript_minification_failures",
    "html_cache_hits",
    "html_cache_misses",
    "html_cache_bypasses",
    "html_cache_inserts",
    "html_cache_bytes_served",
];

/// Dashboard counters recovered across restarts (`kDashboardCounters`).
pub const DASHBOARD_COUNTERS: &[&str] = &[
    "javascript_total_bytes_saved",
    "css_filter_total_bytes_saved",
    "image_rewrite_total_bytes_saved",
    "javascript_blocks_minified",
    "css_filter_blocks_rewritten",
    "image_rewrites",
    "html_cache_hits",
    "html_cache_misses",
    "html_cache_bypasses",
    "html_cache_inserts",
    "html_cache_bytes_served",
];

/// Graph variables (`kGraphsVars`, C++ order).
pub const GRAPHS_VARS: &[&str] = &[
    "pcache-cohorts-dom_deletes",
    "pcache-cohorts-beacon_cohort_misses",
    "pcache-cohorts-dom_inserts",
    "pcache-cohorts-dom_misses",
    "pcache-cohorts-beacon_cohort_deletes",
    "pcache-cohorts-beacon_cohort_hits",
    "pcache-cohorts-beacon_cohort_inserts",
    "pcache-cohorts-dom_hits",
    "rewrite_cached_output_missed_deadline",
    "rewrite_cached_output_hits",
    "rewrite_cached_output_misses",
    "url_input_resource_hit",
    "url_input_resource_recent_fetch_failure",
    "serf_fetch_bytes_count",
    "url_input_resource_recent_uncacheable_miss",
    "url_input_resource_recent_uncacheable_failure",
    "url_input_resource_miss",
    "serf_fetch_request_count",
    "lru_cache_hits",
    "serf_fetch_time_duration_ms",
    "serf_fetch_cancel_count",
    "serf_fetch_timeout_count",
    "serf_fetch_failure_count",
    "http_bytes_fetched",
    "serf_fetch_active_count",
    "lru_cache_deletes",
    "serf_fetch_cert_errors",
    "lru_cache_inserts",
    "lru_cache_misses",
    "file_cache_bytes_freed_in_cleanup",
    "file_cache_cleanups",
    "file_cache_disk_checks",
    "file_cache_evictions",
    "file_cache_write_errors",
    "file_cache_deletes",
    "file_cache_hits",
    "file_cache_inserts",
    "file_cache_misses",
    "http_fetches",
    "http_approx_header_bytes_fetched",
    "image_rewrite_total_bytes_saved",
    "image_rewrite_total_original_bytes",
    "image_rewrite_uses",
    "image_rewrite_latency_total_ms",
    "image_rewrites_dropped_intentionally",
    "image_rewrites_dropped_decode_failure",
    "cache_misses",
    "ipro_not_in_cache",
    "image_rewrites_dropped_mime_type_unknown",
    "cache_fallbacks",
    "image_rewrites_dropped_server_write_fail",
    "cache_inserts",
    "image_rewrites_dropped_nosaving_resize",
    "cache_flush_timestamp_ms",
    "image_rewrites_dropped_nosaving_noresize",
    "ipro_served",
    "ipro_not_rewritable",
    "ipro_recorder_resources",
    "cache_deletes",
    "ipro_recorder_inserted_into_cache",
    "ipro_recorder_not_cacheable",
    "ipro_recorder_failed",
    "ipro_recorder_dropped_due_to_load",
    "ipro_recorder_dropped_due_to_size",
    "shm_cache_deletes",
    "shm_cache_hits",
    "shm_cache_inserts",
    "shm_cache_misses",
    "memcached_async_deletes",
    "memcached_async_hits",
    "memcached_async_inserts",
    "memcached_async_misses",
    "memcached_blocking_deletes",
    "memcached_blocking_hits",
    "cache_expirations",
    "memcached_blocking_inserts",
    "memcached_blocking_misses",
    "cache_time_us",
    "cache_hits",
    "cache_backend_hits",
    "cache_backend_misses",
    "cache_extensions",
    "cache_batcher_dropped_gets",
    "cache_flush_count",
    "redis_async_hits",
    "redis_async_inserts",
    "redis_async_misses",
    "redis_async_deletes",
    "redis_blocking_hits",
    "redis_blocking_inserts",
    "redis_blocking_misses",
    "redis_blocking_deletes",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn stat_exists(stats: &Statistics, name: &str) -> bool {
        // The C++ logger reads variables/counters and silently skips timed
        // vars and histograms; every listed name must still be a real stat
        // of some kind.
        stats.get_variable(name).is_some()
            || stats.get_up_down_counter(name).is_some()
            || stats.get_timed_variable(name).is_some()
            || stats.get_histogram(name).is_some()
    }

    #[test]
    fn all_registered_and_unique() {
        let stats = Statistics::new();
        init_all(&stats);
        let snapshot = stats.snapshot_counters();
        // Every logger-listed name resolves after init_all.
        for name in CONSOLE_VARS
            .iter()
            .chain(OTHER_LOGGED_VARS.iter())
            .chain(DASHBOARD_COUNTERS.iter())
            .chain(GRAPHS_VARS.iter())
        {
            assert!(stat_exists(&stats, name), "unregistered stat: {name}");
        }
        // Registry is non-trivially populated.
        assert!(snapshot.len() > 200, "only {} counters", snapshot.len());
    }

    #[test]
    fn fan_out_count_and_kinds() {
        let stats = Statistics::new();
        init_rewrite_driver(&stats);
        // 5 HTML-cache variables present.
        for name in [
            "html_cache_hits",
            "html_cache_misses",
            "html_cache_bypasses",
            "html_cache_inserts",
            "html_cache_bytes_served",
        ] {
            assert!(stats.get_variable(name).is_some());
        }
        // Spot-check fan-out members and their exact kinds.
        assert!(stats.get_variable("cache_extensions").is_some());
        assert!(stats.get_variable("num_css_inlined").is_some());
        assert!(stats.get_variable("inserted_ga_snippets").is_some());
        assert!(stats.get_variable("converted_meta_tags").is_some());
        assert!(stats
            .get_variable("num_rewrites_abandoned_for_lock_contention")
            .is_some());
        assert!(stats
            .get_variable("num_deadline_alarm_invocations")
            .is_some());
        assert!(stats.get_variable("url_input_resource_hit").is_some());
        assert!(stats
            .get_variable("font_service_input_resource_miss")
            .is_some());
        // Bytes-saved is an up/down counter, not a plain variable.
        assert!(stats
            .get_up_down_counter("css_filter_total_bytes_saved")
            .is_some());
        assert!(stats.get_variable("css_filter_total_bytes_saved").is_none());
        assert!(stats
            .get_up_down_counter("image_ongoing_rewrites")
            .is_some());
        // WebP/AVIF histograms registered as histograms.
        assert!(stats
            .get_histogram("image_webp_conversion_gif_success_ms")
            .is_some());
        assert!(stats
            .get_histogram("image_avif_conversion_child_cpu_ms")
            .is_some());
        // Dropped-due-to-load are timed variables.
        assert!(stats
            .get_timed_variable("image_rewrites_dropped_due_to_load")
            .is_some());
        // Historical spellings preserved byte-identically.
        assert!(stats
            .get_variable("show_ads_snippets_not_converte")
            .is_some());
        // Waveform counters come from init_rewrite_stats, not the driver.
        assert!(stats
            .get_up_down_counter("low-priority-worked-queue-depth")
            .is_none());
        init_rewrite_stats(&stats);
        assert!(stats
            .get_up_down_counter("low-priority-worked-queue-depth")
            .is_some());
        assert!(stats
            .get_up_down_counter("_purge_poll_timestamp_ms")
            .is_none()); // purge init is separate
    }

    #[test]
    fn backend_prefix_families() {
        let stats = Statistics::new();
        for prefix in CACHE_BACKEND_PREFIXES {
            init_cache_stats(prefix, &stats);
        }
        assert_eq!(CACHE_BACKEND_PREFIXES.len(), 7);
        for prefix in CACHE_BACKEND_PREFIXES {
            for suffix in ["_hits", "_misses", "_inserts", "_deletes"] {
                assert!(
                    stats.get_variable(&format!("{prefix}{suffix}")).is_some(),
                    "{prefix}{suffix}"
                );
            }
            for suffix in [
                "_get_count",
                "_hit_latency_us",
                "_insert_latency_us",
                "_insert_size_bytes",
                "_lookup_size_bytes",
            ] {
                assert!(
                    stats.get_histogram(&format!("{prefix}{suffix}")).is_some(),
                    "{prefix}{suffix}"
                );
            }
        }
    }

    #[test]
    fn logger_list_lengths() {
        assert_eq!(CONSOLE_VARS.len(), 23);
        assert_eq!(OTHER_LOGGED_VARS.len(), 32);
        assert_eq!(DASHBOARD_COUNTERS.len(), 11);
        // Console list intentionally repeats one name (verbatim C++).
        assert_eq!(
            CONSOLE_VARS
                .iter()
                .filter(|name| **name == "image_norewrites_high_resolution")
                .count(),
            2
        );
        assert!(GRAPHS_VARS.len() > 80);
    }
}
