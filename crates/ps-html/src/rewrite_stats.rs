//! Rewrite statistics roster (`RewriteStats::InitStats` plus every
//! filter/cache/fetcher `InitStats`): pre-creates the console
//! variables so untouched counters still list (creation is
//! idempotent; later bumps attach to these). Names transcribed
//! verbatim from the C++ `Add*` call sites.

use ps_stats::core::Statistics;

/// Pre-creates every known counter.
pub fn init_stats(statistics: &Statistics) {
    statistics.add_histogram("Backend Fetch First Byte Latency Histogram"); // kBackendLatencyHistogram
    statistics.add_histogram("Beacon Reported Load Time (ms)"); // kBeaconTimingsMsHistogram
    statistics.add_histogram("Html Time us Histogram"); // kHtmlRewriteTimeUsHistogram
    statistics.add_histogram("Pagespeed Resource Latency Histogram"); // kFetchLatencyHistogram
    statistics.add_histogram("Rewrite Latency Histogram"); // kRewriteLatencyHistogram
    statistics.add_variable("cache_backend_hits"); // kCacheBackendHits
    statistics.add_variable("cache_backend_misses"); // kCacheBackendMisses
    statistics.add_variable("cache_deletes"); // kCacheDeletes
    statistics.add_variable("cache_expirations"); // kCacheExpirations
    statistics.add_variable("cache_extensions"); // kCacheExtensions
    statistics.add_variable("cache_fallbacks"); // kCacheFallbacks
    statistics.add_variable("cache_flush_count"); // kCacheFlushCount
    statistics.add_up_down_counter("cache_flush_timestamp_ms"); // kCacheFlushTimestampMs
    statistics.add_variable("cache_hits"); // kCacheHits
    statistics.add_variable("cache_inserts"); // kCacheInserts
    statistics.add_variable("cache_misses"); // kCacheMisses
    statistics.add_variable("cache_time_us"); // kCacheTimeUs
    statistics.add_variable("child_shutdown_count"); // kShutdownCount
    statistics.add_variable("converted_meta_tags"); // kConvertedMetaTags
    statistics.add_variable("critical_css_beacon_filter_script_added_count"); // kCriticalCssBeaconAddedCount
    statistics.add_variable("critical_css_no_beacon_due_to_missing_data"); // kCriticalCssNoBeaconDueToMissingData
    statistics.add_variable("critical_css_skipped_due_to_charset"); // kCriticalCssSkippedDueToCharset
    statistics.add_variable("critical_images_beacon_filter_script_added_count"); // kCriticalImagesBeaconAddedCount
    statistics.add_variable("critical_images_expired_count"); // kCriticalImagesExpiredCount
    statistics.add_variable("critical_images_not_found_count"); // kCriticalImagesNotFoundCount
    statistics.add_variable("critical_images_valid_count"); // kCriticalImagesValidCount
    statistics.add_timed_variable("critical_selectors_expired_count", "Statistics"); // kCriticalSelectorsExpiredCount
    statistics.add_timed_variable("critical_selectors_not_found_count", "Statistics"); // kCriticalSelectorsNotFoundCount
    statistics.add_timed_variable("critical_selectors_valid_count", "Statistics"); // kCriticalSelectorsValidCount
    statistics.add_variable("css_combine_opportunities"); // kCssCombineOpportunities
    statistics.add_variable("css_elements_moved"); // kCssElementsMoved
    statistics.add_variable("css_file_count_reduction"); // kCssFileCountReduction
    statistics.add_variable("css_filter_blocks_rewritten"); // kBlocksRewritten
    statistics.add_variable("css_filter_fallback_failures"); // kFallbackFailures
    statistics.add_variable("css_filter_fallback_rewrites"); // kFallbackRewrites
    statistics.add_variable("css_filter_parse_failures"); // kParseFailures
    statistics.add_variable("css_filter_parse_max_bytes_exceeded"); // kParseMaxBytesExceeded
    statistics.add_variable("css_filter_rewrites_dropped"); // kRewritesDropped
    statistics.add_up_down_counter("css_filter_total_bytes_saved"); // kTotalBytesSaved
    statistics.add_variable("css_filter_total_original_bytes"); // kTotalOriginalBytes
    statistics.add_variable("css_filter_uses"); // kUses
    statistics.add_variable("css_imports_to_links"); // kCssImportsToLinks
    statistics.add_up_down_counter("current-fetch-queue-size"); // kCurrentGlobalFetchQueueSize
    statistics.add_variable("domain_rewrites"); // kDomainRewrites
    statistics.add_variable("downstream_cache_purge_attempts"); // kDownstreamCachePurgeAttempts
    statistics.add_timed_variable("dropped-fetch-count", "Statistics"); // kDroppedFetchCount
    statistics.add_variable("flatten_imports_charset_mismatch"); // kCharsetMismatch
    statistics.add_variable("flatten_imports_complex_queries"); // kComplexQueries
    statistics.add_variable("flatten_imports_invalid_url"); // kInvalidUrl
    statistics.add_variable("flatten_imports_limit_exceeded"); // kLimitExceeded
    statistics.add_variable("flatten_imports_minify_failed"); // kMinifyFailed
    statistics.add_variable("flatten_imports_recursion"); // kRecursion
    statistics.add_variable("google_analytics_page_load_count"); // kPageLoadCount
    statistics.add_variable("google_analytics_rewritten_count"); // kRewrittenCount
    statistics.add_up_down_counter("html-worker-queue-depth"); // kWaveFormCounters[0]
    statistics.add_histogram("image_avif_conversion_child_cpu_ms"); // kImageAvifChildCpuMs
    statistics.add_histogram("image_avif_conversion_failure_ms"); // kImageAvifFailureMs
    statistics.add_variable("image_avif_conversion_shed"); // kImageAvifShed
    statistics.add_histogram("image_avif_conversion_success_ms"); // kImageAvifSuccessMs
    statistics.add_variable("image_avif_conversion_timeouts"); // kImageAvifTimeouts
    statistics.add_variable("image_file_count_reduction"); // kImageFileCountReduction
    statistics.add_variable("image_inline"); // kImageInline
    statistics.add_variable("image_norewrites_high_resolution"); // kImageNoRewritesHighResolution
    statistics.add_up_down_counter("image_ongoing_rewrites"); // kImageOngoingRewrites
    statistics.add_variable("image_resized_using_rendered_dimensions"); // kImageResizedUsingRenderedDimensions
    statistics.add_histogram("image_rewrite_latency_failed_ms"); // kImageRewriteLatencyFailedMs
    statistics.add_histogram("image_rewrite_latency_ok_ms"); // kImageRewriteLatencyOkMs
    statistics.add_variable("image_rewrite_latency_total_ms"); // kImageRewriteLatencyTotalMs
    statistics.add_variable("image_rewrite_total_bytes_saved"); // kImageRewriteTotalBytesSaved
    statistics.add_variable("image_rewrite_total_original_bytes"); // kImageRewriteTotalOriginalBytes
    statistics.add_variable("image_rewrite_uses"); // kImageRewriteUses
    statistics.add_variable("image_rewrites"); // kImageRewrites
    statistics.add_variable("image_rewrites_dropped_decode_failure"); // kImageRewritesDroppedDecodeFailure
    statistics.add_timed_variable("image_rewrites_dropped_due_to_load", "Statistics"); // kImageRewritesDroppedDueToLoad
    statistics.add_variable("image_rewrites_deferred"); // lazy HTML render (no C++ counterpart)
    statistics.add_variable("image_rewrites_dropped_intentionally"); // kImageRewritesDroppedIntentionally
    statistics.add_variable("image_rewrites_dropped_mime_type_unknown"); // kImageRewritesDroppedMIMETypeUnknown
    statistics.add_variable("image_rewrites_dropped_nosaving_noresize"); // kImageRewritesDroppedNoSavingNoResize
    statistics.add_variable("image_rewrites_dropped_nosaving_resize"); // kImageRewritesDroppedNoSavingResize
    statistics.add_variable("image_rewrites_dropped_server_write_fail"); // kImageRewritesDroppedServerWriteFail
    statistics.add_timed_variable("image_rewrites_squashing_for_mobile_screen", "Statistics"); // kImageRewritesSquashingForMobileScreen
    statistics.add_histogram("image_webp_alpha_failure_ms"); // kImageWebpWithAlphaFailureMs
    statistics.add_histogram("image_webp_alpha_success_ms"); // kImageWebpWithAlphaSuccessMs
    statistics.add_variable("image_webp_alpha_timeouts"); // kImageWebpWithAlphaTimeouts
    statistics.add_histogram("image_webp_conversion_gif_animated_failure_ms"); // kImageWebpFromGifAnimatedFailureMs
    statistics.add_histogram("image_webp_conversion_gif_animated_success_ms"); // kImageWebpFromGifAnimatedSuccessMs
    statistics.add_variable("image_webp_conversion_gif_animated_timeouts"); // kImageWebpFromGifAnimatedTimeouts
    statistics.add_histogram("image_webp_conversion_gif_failure_ms"); // kImageWebpFromGifFailureMs
    statistics.add_histogram("image_webp_conversion_gif_success_ms"); // kImageWebpFromGifSuccessMs
    statistics.add_variable("image_webp_conversion_gif_timeouts"); // kImageWebpFromGifTimeouts
    statistics.add_histogram("image_webp_conversion_jpeg_failure_ms"); // kImageWebpFromJpegFailureMs
    statistics.add_histogram("image_webp_conversion_jpeg_success_ms"); // kImageWebpFromJpegSuccessMs
    statistics.add_variable("image_webp_conversion_jpeg_timeouts"); // kImageWebpFromJpegTimeouts
    statistics.add_histogram("image_webp_conversion_png_failure_ms"); // kImageWebpFromPngFailureMs
    statistics.add_histogram("image_webp_conversion_png_success_ms"); // kImageWebpFromPngSuccessMs
    statistics.add_variable("image_webp_conversion_png_timeouts"); // kImageWebpFromPngTimeouts
    statistics.add_histogram("image_webp_opaque_failure_ms"); // kImageWebpOpaqueFailureMs
    statistics.add_histogram("image_webp_opaque_success_ms"); // kImageWebpOpaqueSuccessMs
    statistics.add_variable("image_webp_opaque_timeouts"); // kImageWebpOpaqueTimeouts
    statistics.add_variable("image_webp_rewrites"); // kImageWebpRewrites
    statistics.add_variable("in_place_oversized_opt_stream"); // kInPlaceOversizedOptStream
    statistics.add_variable("in_place_uncacheable_rewrites"); // kInPlaceUncacheableRewrites
    statistics.add_variable("inserted_ga_snippets"); // kInsertedGaSnippets
    statistics.add_variable("instrumentation_filter_script_added_count"); // kInstrumentationScriptAddedCount
    statistics.add_variable("ipro_not_in_cache"); // kIproNotInCache
    statistics.add_variable("ipro_not_rewritable"); // kIproNotRewritable
    statistics.add_variable("ipro_recorder_dropped_due_to_load"); // kNumDroppedDueToLoad
    statistics.add_variable("ipro_recorder_dropped_due_to_size"); // kNumDroppedDueToSize
    statistics.add_variable("ipro_recorder_failed"); // kNumFailed
    statistics.add_variable("ipro_recorder_inserted_into_cache"); // kNumInsertedIntoCache
    statistics.add_variable("ipro_recorder_not_cacheable"); // kNumNotCacheable
    statistics.add_variable("ipro_recorder_resources"); // kNumResources
    statistics.add_variable("ipro_served"); // kIproServed
    statistics.add_variable("javascript_blocks_minified"); // kBlocksMinified
    statistics.add_variable("javascript_did_not_shrink"); // kJSDidNotShrink
    statistics.add_variable("javascript_failed_to_write"); // kJSFailedToWrite
    statistics.add_variable("javascript_libraries_identified"); // kLibrariesIdentified
    statistics.add_variable("javascript_minification_disabled"); // kJSMinificationDisabled
    statistics.add_variable("javascript_minification_failures"); // kMinificationFailures
    statistics.add_variable("javascript_minify_uses"); // kMinifyUses
    statistics.add_variable("javascript_reducing_minifications"); // kNumReducingMinifications
    statistics.add_variable("js_file_count_reduction"); // kJsFileCountReduction
    statistics.add_up_down_counter("low-priority-worked-queue-depth"); // kWaveFormCounters[2]
    statistics.add_up_down_counter("memcache_error_burst_size"); // kErrorBurstSize
    statistics.add_up_down_counter("memcache_last_error_checkpoint_ms"); // kLastErrorCheckpointMs
    statistics.add_variable("memcache_timeouts"); // kMemCacheTimeouts
    statistics.add_variable("not_cacheable"); // kNotCacheable
    statistics.add_variable("num_cache_control_not_rewritable_resources"); // kNumCacheControlNotRewritableResources
    statistics.add_variable("num_cache_control_rewritable_resources"); // kNumCacheControlRewritableResources
    statistics.add_variable("num_conditional_refreshes"); // kNumConditionalRefreshes
    statistics.add_variable("num_css_inlined"); // kNumCssInlined
    statistics.add_variable("num_css_not_used_for_critical_css_computation"); // kNumCssNotUsedForCriticalCssComputation
    statistics.add_variable("num_css_used_for_critical_css_computation"); // kNumCssUsedForCriticalCssComputation
    statistics.add_variable("num_deadline_alarm_invocations"); // kNumDeadlineAlarmInvocations
    statistics.add_variable("num_dedup_inlined_images_candidates_found"); // kCandidatesFound
    statistics.add_variable("num_dedup_inlined_images_candidates_replaced"); // kCandidatesReplaced
    statistics.add_variable("num_fallback_responses_served"); // kFallbackResponsesServed
    statistics.add_variable("num_fallback_responses_served_while_revalidate"); // kFallbackResponsesServedWhileRevalidate
    statistics.add_variable("num_flushes"); // kNumFlushes
    statistics.add_variable("num_js_inlined"); // kNumJsInlined
    statistics.add_variable("num_local_storage_cache_candidates_added"); // kCandidatesAdded
    statistics.add_variable("num_local_storage_cache_candidates_removed"); // kCandidatesRemoved
    statistics.add_variable("num_local_storage_cache_stored_css"); // kStoredCss
    statistics.add_variable("num_local_storage_cache_stored_images"); // kStoredImages
    statistics.add_variable("num_local_storage_cache_stored_total"); // kStoredTotal
    statistics.add_variable("num_proactively_freshen_user_facing_request"); // kProactivelyFreshenUserFacingRequest
    statistics.add_variable("num_resource_fetch_failures"); // kNumResourceFetchFailures
    statistics.add_variable("num_resource_fetch_successes"); // kNumResourceFetchSuccesses
    statistics.add_variable("background_jobs_submitted");
    statistics.add_variable("background_jobs_completed");
    statistics.add_variable("background_jobs_failed");
    statistics.add_variable("background_jobs_dedup_hits");
    statistics.add_variable("background_jobs_timeouts");
    statistics.add_variable("background_jobs_queue_full");
    statistics.add_variable("background_jobs_shutdown");
    statistics.add_variable("num_rewrites_abandoned_for_lock_contention"); // kNumRewritesAbandonedForLockContention
    statistics.add_timed_variable("num_rewrites_executed", "Statistics"); // kRewritesExecuted
    statistics.add_timed_variable("queued-fetch-count", "Statistics"); // kQueuedFetchCount
    statistics.add_variable("redis_cluster_redirections"); // kRedisClusterRedirections
    statistics.add_variable("redis_cluster_slots_fetches"); // kRedisClusterSlotsFetches
    statistics.add_variable("resource_404_count"); // kInstawebResource404Count
    statistics.add_variable("resource_fetch_construct_failures"); // kResourceFetchConstructFailures
    statistics.add_variable("resource_fetch_construct_successes"); // kResourceFetchConstructSuccesses
    statistics.add_variable("resource_fetches_cached"); // kResourceFetchesCached
    statistics.add_variable("resource_url_domain_acceptances"); // kResourceUrlDomainAcceptances
    statistics.add_variable("resource_url_domain_rejections"); // kResourceUrlDomainRejections
    statistics.add_up_down_counter("rewrite-worker-queue-depth"); // kWaveFormCounters[1]
    statistics.add_variable("rewrite_cached_output_hits"); // kCachedOutputHits
    statistics.add_variable("rewrite_cached_output_missed_deadline"); // kCachedOutputMissedDeadline
    statistics.add_variable("rewrite_cached_output_misses"); // kCachedOutputMisses
    statistics.add_up_down_counter("serf_fetch_active_count"); // kSerfFetchActiveCount
    statistics.add_variable("serf_fetch_bytes_count"); // kSerfFetchByteCount
    statistics.add_variable("serf_fetch_cancel_count"); // kSerfFetchCancelCount
    statistics.add_variable("serf_fetch_cert_errors"); // kSerfFetchCertErrors
    statistics.add_variable("serf_fetch_failure_count"); // kSerfFetchFailureCount
    statistics.add_up_down_counter("serf_fetch_last_check_timestamp_ms"); // kSerfFetchLastCheckTimestampMs
    statistics.add_variable("serf_fetch_num_calls_to_read"); // kSerfFetchReadCalls
    statistics.add_variable("serf_fetch_request_count"); // kSerfFetchRequestCount
    statistics.add_variable("serf_fetch_time_duration_ms"); // kSerfFetchTimeDurationMs
    statistics.add_variable("serf_fetch_timeout_count"); // kSerfFetchTimeoutCount
    statistics.add_variable("serf_fetch_ultimate_failure"); // kSerfFetchUltimateFailure
    statistics.add_variable("serf_fetch_ultimate_success"); // kSerfFetchUltimateSuccess
    statistics.add_variable("show_ads_api_replaced_for_async"); // kShowAdsApiReplacedForAsync
    statistics.add_variable("show_ads_snippets_converted"); // kShowAdsSnippetsConverted
    statistics.add_variable("show_ads_snippets_not_converte"); // kShowAdsSnippetsNotConverted
    statistics.add_variable("slurp_404_count"); // kInstawebSlurp404Count
    statistics.add_variable("statistics_404_count"); // kStatistics404Count
    statistics.add_variable("successful_downstream_cache_purges"); // kSuccessfulDownstreamCachePurges
    statistics.add_timed_variable("total_fetch_count", "Statistics"); // kTotalFetchCount
    statistics.add_variable("total_page_load_ms"); // kTotalPageLoadMs
    statistics.add_timed_variable("total_rewrite_count", "Statistics"); // kTotalRewriteCount
    statistics.add_variable("url_trim_saved_bytes"); // kUrlTrimSavedBytes
    statistics.add_variable("url_trims"); // kUrlTrims
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_lists_console_names() {
        let statistics = Statistics::new();
        init_stats(&statistics);
        // Spot-check each family plus the fetch pair every
        // context relied on before the roster landed.
        for name in [
            "num_resource_fetch_successes",
            "num_resource_fetch_failures",
            "css_filter_blocks_rewritten",
            "cache_hits",
            "image_rewrites_deferred",
            "rewrite_cached_output_hits",
            "background_jobs_submitted",
            "background_jobs_completed",
            "background_jobs_failed",
            "background_jobs_dedup_hits",
            "background_jobs_timeouts",
            "background_jobs_queue_full",
            "background_jobs_shutdown",
        ] {
            assert!(
                statistics.get_variable(name).is_some(),
                "missing variable {name}"
            );
        }
        assert!(statistics
            .get_histogram("Rewrite Latency Histogram")
            .is_some());
        assert!(statistics
            .get_up_down_counter("cache_flush_timestamp_ms")
            .is_some());
    }
}
