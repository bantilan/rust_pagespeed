//! In-place resource optimization (`InPlaceRewriteContext`, id `aj`).
//!
//! Serves optimized bytes at origin URLs. The first fetch records
//! the original, rewrites it, and (unless waiting) streams the
//! original with an injected s-maxage window; later fetches serve
//! the optimized bytes. Conditional requests short-circuit with
//! 304s against the rewrite etag.
//!
//! Sync-model notes: the background rewrite runs inline before
//! responding, but the served bytes match C++ (originals stream
//! on miss; waiting mode serves optimized at once). Rewrite
//! deadlines, mid-stream failure staging, staleness freshening,
//! and load-from-file inputs have no sync equivalent and wait
//! for those subsystems. Origin (non-proxy) fallback waits for
//! the server-integration layer; serving here always proxies.
//! Only test servants implement nested rewrites (see
//! `RewriteFilter::rewrite_in_place`).

use std::rc::Rc;

use ps_cache::cache::KeyState;
use ps_http::content_type::{extension_to_content_type, ContentType};
use ps_http::fetch::AsyncFetch;
use ps_http::google_url::GoogleUrl;
use ps_http::http_cache::{FindStatus, ETAG_PREFIX};
use ps_http::names::header;
use ps_http::request::Method;
use ps_http::response::{RequestProperties, ResponseHeaders, ValidatorOption, VaryOption};

use crate::image_url_encoder::{ImageResourceContext, ImageUrlEncoder, LibWebpLevel};
use crate::metadata::{decode_partitions, encode_partitions, CachedResult, OutputPartitions};
use crate::rewrite_filter::RewriteFilter;
use crate::server_context::ResourceDriver;

/// Filter id (`kInPlaceRewriteId`).
pub const FILTER_ID: &str = "aj";
/// Oversized streams (`kInPlaceOversizedOptStream`).
pub const IN_PLACE_OVERSIZED_OPT_STREAM: &str = "in_place_oversized_opt_stream";
/// Uncacheable rewrites (`kInPlaceUncacheableRewrites`).
pub const IN_PLACE_UNCACHEABLE_REWRITES: &str = "in_place_uncacheable_rewrites";

/// Serves an origin URL in place (`FetchInPlaceResource`).
/// Non-proxy callers (nginx) fail content IPRO doesn't
/// rewrite so the server falls back to its native method
/// (notably HTML, which the HTML pipeline owns); proxy
/// callers serve such bytes through.
pub fn fetch_in_place_resource(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    proxy_mode: bool,
) -> bool {
    let gurl = GoogleUrl::new(url);
    if !gurl.is_web_valid() {
        return false;
    }
    if fetch.request_headers().method() == Method::Head {
        // A HEAD response carries no body to record or rewrite.
        return serve_head(driver, url, fetch, proxy_mode);
    }
    // The UA key feeds the metadata key, so servant UA encoding
    // runs on every fetch, hit or miss.
    let ua_key = user_agent_cache_key(driver, url);
    let mkey = metadata_key(driver, url, &ua_key);
    if let Some(cached) = lookup_metadata(driver, &mkey) {
        return serve_from_metadata(driver, url, fetch, &cached, &mkey, proxy_mode);
    }
    reconstruct(driver, url, fetch, &mkey, proxy_mode)
}

/// Serves headers without a body or rewrite (HEAD requests).
fn serve_head(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    proxy_mode: bool,
) -> bool {
    if !proxy_mode {
        // HEAD never rewrites: non-proxy callers fall back to
        // the server's native method.
        return fail_fetch(driver, fetch);
    }
    if let Some((headers, _)) = find_original(driver, url) {
        fetch.response_headers_mut().copy_from(&headers);
        fetch.headers_complete();
        fetch.handle_done(true);
        return true;
    }
    let mut scratch = scratch_fetch_for_origin(driver, url);
    driver.server_context().fetcher().fetch(url, &mut scratch);
    if !scratch.success() {
        return fail_fetch(driver, fetch);
    }
    let mut headers = scratch.response_headers().clone();
    fix_fetched_headers(driver, &mut headers);
    fetch.response_headers_mut().copy_from(&headers);
    fetch.headers_complete();
    fetch.handle_done(true);
    true
}

/// Fails a fetch, stamping the attempt time when dateless
/// (C++ failed fetches surface the attempt date; the stamping
/// lives in its async failure plumbing).
fn fail_fetch(driver: &Rc<ResourceDriver>, fetch: &mut dyn AsyncFetch) -> bool {
    if fetch.response_headers().date_ms() == 0 {
        fetch
            .response_headers_mut()
            .set_date(driver.server_context().now_ms());
    }
    fetch.handle_done(false);
    true
}

/// Normalizes freshly fetched headers (`CachePutFetch`).
fn fix_fetched_headers(driver: &Rc<ResourceDriver>, headers: &mut ResponseHeaders) {
    headers.fix_date_headers(driver.server_context().now_ms());
    headers.compute_caching();
}

/// Metadata key: filter id, signature, URL, UA key.
fn metadata_key(driver: &Rc<ResourceDriver>, url: &str, ua_key: &str) -> String {
    format!(
        "rname/aj_{}/{url}@{ua_key}",
        driver.partition_signature_hash()
    )
}

/// Reads one cached partition, if present and well-formed.
fn lookup_metadata(driver: &Rc<ResourceDriver>, mkey: &str) -> Option<CachedResult> {
    let lookup = driver.server_context().metadata_cache().get(mkey);
    if lookup.state != KeyState::Available {
        return None;
    }
    let bytes = lookup.value?;
    let partitions = decode_partitions(&bytes)?;
    if partitions.partition.len() != 1 {
        return None;
    }
    partitions.partition.into_iter().next()
}

/// Writes one cached partition.
fn store_metadata(driver: &Rc<ResourceDriver>, mkey: &str, cached: &CachedResult) {
    let mut partitions = OutputPartitions::default();
    partitions.partition.push(cached.clone());
    driver
        .server_context()
        .metadata_cache()
        .put(mkey, &encode_partitions(&partitions));
}

/// UA-dependent cache fragment (`UserAgentCacheKey`).
fn user_agent_cache_key(driver: &Rc<ResourceDriver>, url: &str) -> String {
    if !in_place_optimize_for_browser_enabled(driver) {
        return String::new();
    }
    let mut context = ImageResourceContext::default();
    // We risk intentionally increasing metadata cache
    // fragmentation when request URL extensions are wrong or
    // inconclusive: unknown extensions encode every capacity.
    match extension_content_type(url) {
        None => {
            encode_for_type(driver, &k_jpeg_like(), &mut context);
            encode_for_type(driver, &k_css_like(), &mut context);
        }
        Some(content_type) if content_type.is_image() || content_type.is_css() => {
            encode_for_type(driver, &content_type, &mut context);
        }
        Some(_) => {}
    }
    // In IPRO, we cannot use mobile quality if we're not
    // allowed to vary on user agent.
    let properties = driver.request_properties();
    let vary_on_user_agent =
        ImageUrlEncoder::allow_vary_on_user_agent(&driver.options(), &properties);
    if !vary_on_user_agent {
        context.may_use_small_screen_quality = false;
        if ImageUrlEncoder::allow_vary_on_accept(&driver.options(), &properties) {
            if context.libwebp_level != LibWebpLevel::None {
                context.libwebp_level = LibWebpLevel::LossyOnly;
            }
        } else {
            context.libwebp_level = LibWebpLevel::None;
        }
    }
    ImageUrlEncoder::cache_key_from_resource_context(&context)
}

/// Content type from the URL leaf extension
/// (`NameExtensionToContentType`).
fn extension_content_type(url: &str) -> Option<ContentType> {
    let leaf = url.rsplit('/').next().unwrap_or(url);
    let leaf = leaf.split(['?', '#']).next().unwrap_or(leaf);
    if !leaf.contains('.') {
        return None;
    }
    extension_to_content_type(leaf)
}

/// Runs servant UA encoding for a content type.
fn encode_for_type(
    driver: &Rc<ResourceDriver>,
    content_type: &ContentType,
    context: &mut ImageResourceContext,
) {
    if let Some(filter) = get_rewrite_filter(driver, content_type) {
        filter.encode_user_agent_into_resource_context(driver, context);
    }
}

/// Whether browser-dependent IPRO applies
/// (`InPlaceOptimizeForBrowserEnabled`).
fn in_place_optimize_for_browser_enabled(driver: &Rc<ResourceDriver>) -> bool {
    use ps_config::filter::Filter;
    let options = driver.options();
    options.enabled(Filter::InPlaceOptimizeForBrowser)
        && (options.enabled(Filter::ConvertJpegToWebp) || options.enabled(Filter::ConvertToAvif))
}

/// Picks the servant filter for a content type
/// (`GetRewriteFilter`). Servants register lazily on first
/// lookup (C++ constructs all filters in the driver ctor;
/// lazy construction is observationally identical).
fn get_rewrite_filter(
    driver: &Rc<ResourceDriver>,
    content_type: &ContentType,
) -> Option<Rc<dyn RewriteFilter>> {
    use ps_config::filter::Filter;
    let options = driver.options();
    if content_type.is_css() && options.enabled(Filter::RewriteCss) {
        if driver.find_filter("cf").is_none() {
            driver.add_filter(Rc::new(super::filters::css::CssFilter::new(driver.clone())));
        }
        return driver.find_filter("cf");
    }
    if content_type.is_js_like() && options.enabled(Filter::RewriteJavascriptExternal) {
        if driver.find_filter("jm").is_none() {
            driver.add_filter(Rc::new(super::filters::JavascriptFilter::new(
                driver.clone(),
            )));
        }
        return driver.find_filter("jm");
    }
    if content_type.is_image() && options.image_optimization_enabled() {
        if driver.find_filter("ic").is_none() {
            driver.add_filter(Rc::new(super::filters::ImageRewriteFilter::new(
                driver.clone(),
            )));
        }
        return driver.find_filter("ic");
    }
    None
}

/// JPEG stand-in for extension-less URLs (`kContentTypeJpeg`).
fn k_jpeg_like() -> ContentType {
    ps_http::content_type::mime_to_content_type("image/jpeg").expect("jpeg known")
}

/// CSS stand-in for extension-less URLs (`kContentTypeCss`).
fn k_css_like() -> ContentType {
    ps_http::content_type::mime_to_content_type("text/css").expect("css known")
}

/// Serves from a metadata hit (`Fetch` on cached partitions).
fn serve_from_metadata(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    cached: &CachedResult,
    mkey: &str,
    proxy_mode: bool,
) -> bool {
    let now_ms = driver.server_context().now_ms();
    let (date_ms, expire_ms) = input_window(&cached.input);
    let stale = expire_ms <= now_ms;
    if stale {
        let threshold = driver.options().metadata_cache_staleness_threshold_ms();
        if cached.optimizable && now_ms < expire_ms + threshold {
            return serve_stale_rewritten(
                driver, url, fetch, cached, date_ms, expire_ms, proxy_mode,
            );
        }
        // Beyond the threshold: revalidate synchronously (the
        // async rewrite would refresh in the background).
        return revalidate(driver, url, fetch, mkey, Some(cached), proxy_mode);
    }
    if !cached.optimizable {
        return serve_original_cached(driver, url, fetch, mkey, proxy_mode);
    }
    serve_rewritten(
        driver, url, fetch, cached, mkey, date_ms, expire_ms, false, None, proxy_mode,
    )
}

/// Serves a stale-but-usable rewrite while revalidating
/// (`stale_rewrite`).
fn serve_stale_rewritten(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    cached: &CachedResult,
    date_ms: i64,
    expire_ms: i64,
    proxy_mode: bool,
) -> bool {
    // Revalidate inline (the background refresh): unchanged
    // inputs reuse the rewrite without recomputing.
    revalidate_input(driver, url, cached);
    let ua_key = user_agent_cache_key(driver, url);
    let mkey = metadata_key(driver, url, &ua_key);
    serve_rewritten(
        driver, url, fetch, cached, &mkey, date_ms, expire_ms, true, None, proxy_mode,
    )
}

/// Revalidates inputs against the origin; refreshes caches
/// when unchanged and reports the outcome with the response.
fn revalidate_input(
    driver: &Rc<ResourceDriver>,
    url: &str,
    cached: &CachedResult,
) -> Option<(ResponseHeaders, Vec<u8>, bool)> {
    let mut scratch = scratch_fetch_for_origin(driver, url);
    driver.server_context().fetcher().fetch(url, &mut scratch);
    if !scratch.success() {
        return None;
    }
    let body = scratch.body().to_vec();
    let hash = driver.server_context().hasher().hash(&body);
    let unchanged = cached
        .input
        .first()
        .and_then(|info| info.input_content_hash.as_deref())
        == Some(hash.as_str());
    let mut headers = scratch.response_headers().clone();
    fix_fetched_headers(driver, &mut headers);
    if unchanged {
        // Contents identical: refresh the HTTP entry, the
        // rewritten entry, and the metadata window without
        // rewriting.
        put_original(driver, url, &headers, &body);
        let refreshed = refresh_metadata_window(driver, url, cached, &headers);
        if cached.optimizable {
            reput_rewritten(driver, &refreshed);
        }
    }
    Some((headers, body, unchanged))
}

/// Revalidates past the staleness threshold: unchanged inputs
/// refresh metadata and serve the original; changed inputs
/// rewrite fully.
fn revalidate(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    mkey: &str,
    cached: Option<&CachedResult>,
    proxy_mode: bool,
) -> bool {
    let Some(previous) = cached else {
        return reconstruct(driver, url, fetch, mkey, proxy_mode);
    };
    match revalidate_input(driver, url, previous) {
        None => {
            // Refetch failed: serve whatever is cached, else fail.
            if let Some((headers, body)) = find_original(driver, url) {
                serve_response(fetch, &headers, &body)
            } else {
                fail_fetch(driver, fetch)
            }
        }
        Some((headers, body, unchanged)) => {
            if unchanged {
                // Contents identical: metadata refreshed without
                // rewriting; serve the revalidated original with
                // the injected window, like a miss.
                serve_with_window(driver, fetch, &headers, &body)
            } else {
                reconstruct_from_response(driver, url, fetch, mkey, headers, body, proxy_mode)
            }
        }
    }
}

/// Refreshes a metadata entry's input window in place,
/// returning the refreshed entry.
fn refresh_metadata_window(
    driver: &Rc<ResourceDriver>,
    url: &str,
    cached: &CachedResult,
    headers: &ResponseHeaders,
) -> CachedResult {
    let mut refreshed = cached.clone();
    if let Some(info) = refreshed.input.first_mut() {
        info.date_ms = Some(headers.date_ms());
        info.expiration_time_ms = Some(headers.cache_expiration_time_ms());
    }
    let ua_key = user_agent_cache_key(driver, url);
    store_metadata(driver, &metadata_key(driver, url, &ua_key), &refreshed);
    refreshed
}

/// Re-stores the rewritten bytes under the refreshed window.
fn reput_rewritten(driver: &Rc<ResourceDriver>, refreshed: &CachedResult) {
    let Some((headers, body)) = find_entry_incl_stale(driver, &refreshed.url) else {
        return;
    };
    let Some(content_type) = headers.determine_content_type() else {
        return;
    };
    let (date_ms, expire_ms) = input_window(&refreshed.input);
    let original_content_length = headers
        .headers()
        .lookup1(header::X_ORIGINAL_CONTENT_LENGTH)
        .and_then(|value| value.parse::<i64>().ok());
    put_response(
        driver,
        &refreshed.url,
        date_ms,
        expire_ms,
        &content_type,
        &body,
        original_content_length,
        Some(&headers),
    );
}

/// Serves the optimized bytes (`FetchTryFallback` hit path).
#[allow(clippy::too_many_arguments)]
fn serve_rewritten(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    cached: &CachedResult,
    mkey: &str,
    date_ms: i64,
    expire_ms: i64,
    stale: bool,
    input_headers: Option<ResponseHeaders>,
    proxy_mode: bool,
) -> bool {
    let aj_etag = format!("{ETAG_PREFIX}aj-{}\"", cached.hash);
    if let Some(request_etag) = fetch
        .request_headers()
        .headers()
        .lookup1(header::IF_NONE_MATCH)
    {
        if request_etag == aj_etag {
            // Serve out a 304.
            fetch.response_headers_mut().clear();
            fetch.response_headers_mut().set_status_and_reason(304);
            fetch.handle_done(true);
            return true;
        }
    }
    let Some((mut headers, body)) = find_rewritten(driver, &cached.url) else {
        // Metadata without bytes: reconstruct with a forced
        // nested rewrite.
        return reconstruct(driver, url, fetch, mkey, proxy_mode);
    };
    fix_fetch_fallback_headers(driver, url, cached, date_ms, expire_ms, stale, &mut headers);
    add_vary_if_required(driver, cached, &mut headers);
    let found;
    let input_headers = match input_headers {
        Some(headers) => Some(headers),
        None => {
            found = find_entry_incl_stale(driver, url);
            found.as_ref().map(|(headers, _)| headers.clone())
        }
    };
    if let Some(input_headers) = input_headers {
        apply_input_cache_control(&input_headers, &mut headers);
    }
    let now_ms = driver.server_context().now_ms();
    if !stale
        && imminently_expiring(
            date_ms,
            expire_ms,
            now_ms,
            driver.options().implicit_cache_ttl_ms(),
        )
    {
        // Proactively freshen entries near expiry (the
        // background refresh, run inline): unchanged inputs
        // reuse the rewrite without recomputing.
        if let Some((fresh_headers, fresh_body, unchanged)) = revalidate_input(driver, url, cached)
        {
            if !unchanged {
                if let Some(content_type) = fresh_headers.determine_content_type() {
                    if let Some(servant) = get_rewrite_filter(driver, &content_type) {
                        let _ = rewrite_and_store(
                            driver,
                            url,
                            mkey,
                            &fresh_headers,
                            &fresh_body,
                            &servant,
                            &content_type,
                        );
                    }
                }
            }
        }
    }
    fetch.response_headers_mut().copy_from(&headers);
    fetch.headers_complete();
    let ok = fetch.handle_write(&body);
    fetch.handle_done(ok);
    true
}

/// Serves the original from the HTTP cache (unoptimizable hit).
fn serve_original_cached(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    mkey: &str,
    proxy_mode: bool,
) -> bool {
    if let Some((headers, body)) = find_original(driver, url) {
        fetch.response_headers_mut().copy_from(&headers);
        fetch.headers_complete();
        let ok = fetch.handle_write(&body);
        fetch.handle_done(ok);
        return true;
    }
    reconstruct(driver, url, fetch, mkey, proxy_mode)
}

/// Serves a response with the injected s-maxage window.
fn serve_with_window(
    driver: &Rc<ResourceDriver>,
    fetch: &mut dyn AsyncFetch,
    headers: &ResponseHeaders,
    body: &[u8],
) -> bool {
    let mut headers = headers.clone();
    inject_s_maxage(driver, &mut headers);
    headers.compute_caching();
    serve_response(fetch, &headers, body)
}

/// Fetches the original and rewrites it (`StartFetchReconstruction`).
fn reconstruct(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    mkey: &str,
    proxy_mode: bool,
) -> bool {
    if let Some((headers, body)) = find_original(driver, url) {
        return reconstruct_from_response(driver, url, fetch, mkey, headers, body, proxy_mode);
    }
    let mut scratch = scratch_fetch_for_origin(driver, url);
    driver.server_context().fetcher().fetch(url, &mut scratch);
    if !scratch.success() {
        return fail_fetch(driver, fetch);
    }
    let mut headers = scratch.response_headers().clone();
    fix_fetched_headers(driver, &mut headers);
    let body = scratch.body().to_vec();
    // Oversized responses stream through unrecorded (the
    // mid-stream overflow, observed whole) — but only
    // rewritable ones stream; anything else fails in
    // non-proxy mode like below.
    if !driver
        .server_context()
        .http_cache()
        .is_cacheable_body_size(body.len() as i64)
    {
        driver
            .server_context()
            .variable(IN_PLACE_OVERSIZED_OPT_STREAM)
            .add(1);
        if !proxy_mode && can_in_place_rewrite(driver, fetch, &headers).is_none() {
            return fail_fetch(driver, fetch);
        }
        fetch.response_headers_mut().copy_from(&headers);
        fetch.headers_complete();
        let ok = fetch.handle_write(&body);
        fetch.handle_done(ok);
        return true;
    }
    reconstruct_from_response(driver, url, fetch, mkey, headers, body, proxy_mode)
}

/// Records, rewrites, and serves a fetched response.
fn reconstruct_from_response(
    driver: &Rc<ResourceDriver>,
    url: &str,
    fetch: &mut dyn AsyncFetch,
    mkey: &str,
    headers: ResponseHeaders,
    body: Vec<u8>,
    proxy_mode: bool,
) -> bool {
    // The HTTP layer records cacheable origin responses (except
    // codings it cannot adapt) regardless of rewriting.
    if !headers.has_content_coding_other_than_single_gzip() {
        put_original(driver, url, &headers, &body);
    }
    let Some(content_type) = can_in_place_rewrite(driver, fetch, &headers) else {
        // Proxy mode serves the bytes through; non-proxy
        // callers fail so the server falls back to its native
        // method (notably HTML, which the HTML pipeline owns).
        if proxy_mode {
            return serve_response(fetch, &headers, &body);
        }
        return fail_fetch(driver, fetch);
    };
    // Servant lookup happens at rewrite time, after the
    // streaming serve already committed to the window.
    let servant = get_rewrite_filter(driver, &content_type);
    if servant.is_none() || !is_safe_to_rewrite(driver, &headers, &body) {
        store_metadata(driver, mkey, &unoptimizable_result(&headers, &body, driver));
        // NOTE: waiting mode held the headers, so only
        // streaming serves inject the window; no-transform
        // additionally refuses injection in `SetSMaxAge`.
        if driver.options().in_place_wait_for_optimized() {
            return serve_response(fetch, &headers, &body);
        }
        return serve_with_window(driver, fetch, &headers, &body);
    }
    let servant = servant.expect("servant checked above");
    match rewrite_and_store(driver, url, mkey, &headers, &body, &servant, &content_type) {
        None => {
            // Rewrite failed: remember so later fetches skip it.
            store_metadata(driver, mkey, &unoptimizable_result(&headers, &body, driver));
            if driver.options().in_place_wait_for_optimized() {
                // Waiting mode held the headers back, so no
                // s-maxage was ever injected: serve plain.
                serve_response(fetch, &headers, &body)
            } else {
                serve_with_window(driver, fetch, &headers, &body)
            }
        }
        Some((cached, output)) => {
            if driver.options().in_place_wait_for_optimized() {
                let (date_ms, expire_ms) = input_window(&cached.input);
                if expire_ms <= date_ms {
                    // No usable input window (uncacheable
                    // inputs): the immediate serve uses the
                    // generated output headers, whose TTL is
                    // just a proto value.
                    return serve_generated(
                        driver,
                        fetch,
                        &cached,
                        &headers,
                        &output.bytes,
                        &output.content_type,
                    );
                }
                serve_rewritten(
                    driver,
                    url,
                    fetch,
                    &cached,
                    mkey,
                    date_ms,
                    expire_ms,
                    false,
                    Some(headers.clone()),
                    proxy_mode,
                )
            } else {
                serve_with_window(driver, fetch, &headers, &body)
            }
        }
    }
}

/// Serves generated output headers for windowless immediate
/// serves: year TTL plus the input's conservative directives.
fn serve_generated(
    driver: &Rc<ResourceDriver>,
    fetch: &mut dyn AsyncFetch,
    cached: &CachedResult,
    input_headers: &ResponseHeaders,
    body: &[u8],
    content_type: &ContentType,
) -> bool {
    let mut headers = ResponseHeaders::new(http_options(driver));
    headers.set_status_and_reason(200);
    headers
        .headers_mut()
        .replace(header::CONTENT_TYPE, content_type.mime_type());
    let now_ms = driver.server_context().now_ms();
    headers.set_date_and_caching(now_ms, ps_cache::timer::YEAR_MS, "");
    headers.compute_caching();
    // Use the most conservative Cache-Control considering the
    // input (`ApplyInputCacheControl`). The recompute keeps the
    // year TTL stale while clearing cacheability, exactly like
    // C++ (whose `else` branch never resets it).
    for directive in ["no-cache", "no-store", "private"] {
        if input_headers.has_cache_control_directive(directive) {
            headers.headers_mut().add(header::CACHE_CONTROL, directive);
        }
    }
    headers
        .headers_mut()
        .replace(header::ETAG, &format!("{ETAG_PREFIX}aj-{}\"", cached.hash));
    headers.compute_caching();
    serve_response(fetch, &headers, body)
}

/// Runs the nested servant rewrite and stores its outputs.
fn rewrite_and_store(
    driver: &Rc<ResourceDriver>,
    url: &str,
    mkey: &str,
    headers: &ResponseHeaders,
    body: &[u8],
    servant: &Rc<dyn RewriteFilter>,
    content_type: &ContentType,
) -> Option<(CachedResult, crate::rewrite_filter::InPlaceOutput)> {
    let output = servant.rewrite_in_place(driver, body, content_type)?;
    let rewritten = rewritten_url(
        url,
        servant.id(),
        &output.content_type,
        driver,
        &output.bytes,
    );
    let hash = driver.server_context().hasher().hash(&output.bytes);
    #[allow(clippy::cast_possible_wrap)]
    let original_len = body.len() as i64;
    let cached = optimizable_result(headers, body, driver, &rewritten, &hash, &output);
    put_rewritten(driver, &rewritten, &cached, &output, original_len, headers);
    store_metadata(driver, mkey, &cached);
    Some((cached, output))
}

/// Serves headers and body as-is.
fn serve_response(fetch: &mut dyn AsyncFetch, headers: &ResponseHeaders, body: &[u8]) -> bool {
    fetch.response_headers_mut().copy_from(headers);
    fetch.headers_complete();
    let ok = fetch.handle_write(body);
    fetch.handle_done(ok);
    true
}

/// Decides rewritability (`RecordingFetch::CanInPlaceRewrite`).
fn can_in_place_rewrite(
    driver: &Rc<ResourceDriver>,
    fetch: &dyn AsyncFetch,
    headers: &ResponseHeaders,
) -> Option<ContentType> {
    // We are rewriting only 200 responses.
    if headers.status_code() != 200 {
        return None;
    }
    let content_type = headers.determine_content_type()?;
    // The rewriters can only see through gzip, and their output
    // is served without the input's Content-Encoding.
    if headers.has_content_coding_other_than_single_gzip() {
        return None;
    }
    if !(content_type.is_css() || content_type.is_js_like() || content_type.is_image()) {
        return None;
    }
    let vary = if driver.options().respect_vary() {
        VaryOption::RespectVaryOnResources
    } else {
        VaryOption::IgnoreVaryOnResources
    };
    let http_cache = driver.server_context().http_cache();
    if headers.is_proxy_cacheable(
        fetch.request_headers().properties(),
        vary,
        ValidatorOption::NoValidator,
    ) && !http_cache.is_expired(headers, driver.server_context().now_ms())
    {
        return Some(content_type);
    }
    if driver.options().rewrite_uncacheable_resources()
        && driver.options().in_place_wait_for_optimized()
    {
        driver
            .server_context()
            .variable(IN_PLACE_UNCACHEABLE_REWRITES)
            .add(1);
        return Some(content_type);
    }
    None
}

/// Whether rewriting is safe (`IsSafeToRewrite` subset that
/// matters here: no-transform and empty bodies).
fn is_safe_to_rewrite(driver: &Rc<ResourceDriver>, headers: &ResponseHeaders, body: &[u8]) -> bool {
    if driver.options().disable_rewrite_on_no_transform()
        && headers.has_cache_control_directive("no-transform")
    {
        return false;
    }
    !body.is_empty()
}

/// Finds the cached original, when fresh.
fn find_original(driver: &Rc<ResourceDriver>, url: &str) -> Option<(ResponseHeaders, Vec<u8>)> {
    find_entry(driver, url)
}

/// Finds the cached rewritten bytes, when fresh.
fn find_rewritten(driver: &Rc<ResourceDriver>, url: &str) -> Option<(ResponseHeaders, Vec<u8>)> {
    find_entry(driver, url)
}

/// Looks up one HTTP entry.
fn find_entry(driver: &Rc<ResourceDriver>, url: &str) -> Option<(ResponseHeaders, Vec<u8>)> {
    let mut ctx = driver.request_context().borrow().clone();
    let found = http_find(driver, url, &mut ctx);
    if found.status == FindStatus::Found {
        Some((found.headers.unwrap_or_default(), found.body))
    } else {
        None
    }
}

/// Looks up one HTTP entry, accepting stale fallbacks.
fn find_entry_incl_stale(
    driver: &Rc<ResourceDriver>,
    url: &str,
) -> Option<(ResponseHeaders, Vec<u8>)> {
    let mut ctx = driver.request_context().borrow().clone();
    let found = http_find(driver, url, &mut ctx);
    if found.status == FindStatus::Found {
        Some((found.headers.unwrap_or_default(), found.body))
    } else {
        found
            .fallback_headers
            .map(|headers| (headers, found.fallback_body))
    }
}

/// Runs one HTTP lookup.
fn http_find(
    driver: &Rc<ResourceDriver>,
    url: &str,
    ctx: &mut ps_http::request::RequestContext,
) -> ps_http::http_cache::FindResult {
    driver.server_context().http_cache().find(
        url,
        driver.cache_fragment(),
        RequestProperties::default(),
        VaryOption::IgnoreVaryOnResources,
        &|_| true,
        -1,
        ctx,
        driver.server_context().now_ms(),
    )
}

/// Stores the origin response.
fn put_original(driver: &Rc<ResourceDriver>, url: &str, headers: &ResponseHeaders, body: &[u8]) {
    let mut value = ps_http::value::HttpValue::new();
    value.set_headers(headers.clone());
    value.write_body(body);
    driver.server_context().http_cache().put_value(
        url,
        driver.cache_fragment(),
        RequestProperties::default(),
        http_options(driver),
        &value,
        driver.server_context().now_ms(),
    );
}

/// Stores the rewritten response under its synthetic URL.
/// Windowless (uncacheable) inputs are never stored (their
/// conservative directives would refuse the store anyway).
fn put_rewritten(
    driver: &Rc<ResourceDriver>,
    rewritten_url: &str,
    cached: &CachedResult,
    output: &crate::rewrite_filter::InPlaceOutput,
    original_len: i64,
    seed: &ResponseHeaders,
) {
    if cached.input.first().is_none() {
        return;
    }
    let (date_ms, expire_ms) = input_window(&cached.input);
    if expire_ms <= date_ms {
        return;
    }
    put_response(
        driver,
        rewritten_url,
        date_ms,
        expire_ms,
        &output.content_type,
        &output.bytes,
        Some(original_len),
        Some(seed),
    );
}

/// Stores bytes under a URL with a TTL window. `seed` carries
/// input headers the output keeps (notably `Accept-Ranges`;
/// length, type, and caching recompute below).
fn put_response(
    driver: &Rc<ResourceDriver>,
    url: &str,
    date_ms: i64,
    expire_ms: i64,
    content_type: &ContentType,
    body: &[u8],
    original_content_length: Option<i64>,
    seed: Option<&ResponseHeaders>,
) {
    let mut headers = seed.cloned().unwrap_or_else(|| {
        let mut fresh = ResponseHeaders::new(http_options(driver));
        fresh.set_status_and_reason(200);
        fresh
    });
    headers.set_status_and_reason(200);
    headers
        .headers_mut()
        .replace(header::CONTENT_TYPE, content_type.mime_type());
    headers.headers_mut().remove_all(header::CONTENT_LENGTH);
    headers.set_date_and_caching(date_ms, (expire_ms - date_ms).max(0), "");
    if let Some(length) = original_content_length {
        headers.set_original_content_length(length);
    }
    headers.compute_caching();
    let mut value = ps_http::value::HttpValue::new();
    value.set_headers(headers);
    value.write_body(body);
    driver.server_context().http_cache().put_value(
        url,
        driver.cache_fragment(),
        RequestProperties::default(),
        http_options(driver),
        &value,
        driver.server_context().now_ms(),
    );
}

/// HTTP options for cache operations.
fn http_options(driver: &Rc<ResourceDriver>) -> ps_http::response::HttpOptions {
    ps_http::response::HttpOptions {
        respect_vary: driver.options().respect_vary(),
        implicit_cache_ttl_ms: driver.options().implicit_cache_ttl_ms(),
    }
}

/// Whether the window nears expiry (`IsImminentlyExpiring`).
fn imminently_expiring(date_ms: i64, expire_ms: i64, now_ms: i64, implicit_ttl_ms: i64) -> bool {
    let ttl_ms = expire_ms - date_ms;
    if ttl_ms < implicit_ttl_ms {
        return false;
    }
    let threshold = implicit_ttl_ms.min(20 * ttl_ms / 100);
    expire_ms - now_ms < threshold
}

/// Conservative input window (`UpdateDateAndExpiry`).
fn input_window(input: &[crate::cacheable_resource::InputInfo]) -> (i64, i64) {
    let mut date_ms = i64::MAX;
    let mut expire_ms = i64::MAX;
    for dependency in input {
        if let (Some(date), Some(expire)) = (dependency.date_ms, dependency.expiration_time_ms) {
            date_ms = date_ms.min(date);
            expire_ms = expire_ms.min(expire);
        }
    }
    (date_ms, expire_ms)
}

/// Builds the unoptimizable partition.
fn unoptimizable_result(
    headers: &ResponseHeaders,
    body: &[u8],
    driver: &Rc<ResourceDriver>,
) -> CachedResult {
    let mut cached = CachedResult::new();
    cached.optimizable = false;
    cached.input.push(input_info(headers, body, driver));
    cached
}

/// Builds the optimized partition.
fn optimizable_result(
    headers: &ResponseHeaders,
    body: &[u8],
    driver: &Rc<ResourceDriver>,
    rewritten_url: &str,
    hash: &str,
    output: &crate::rewrite_filter::InPlaceOutput,
) -> CachedResult {
    let mut cached = CachedResult::new();
    cached.optimizable = true;
    cached.url = rewritten_url.to_string();
    cached.hash = hash.to_string();
    cached.extension = output
        .content_type
        .extension()
        .trim_start_matches('.')
        .to_string();
    cached.optimized_image_type = output.optimized_image_type;
    cached.input.push(input_info(headers, body, driver));
    cached
}

/// Records one input's watermarks.
fn input_info(
    headers: &ResponseHeaders,
    body: &[u8],
    driver: &Rc<ResourceDriver>,
) -> crate::cacheable_resource::InputInfo {
    let mut info = crate::cacheable_resource::InputInfo::default();
    info.date_ms = Some(headers.date_ms());
    info.expiration_time_ms = Some(headers.cache_expiration_time_ms());
    info.input_content_hash = Some(driver.server_context().hasher().hash(body));
    info
}

/// Synthetic rewritten URL (`{url}.pagespeed.{id}.{hash}.{ext}`).
fn rewritten_url(
    url: &str,
    id: &str,
    content_type: &ContentType,
    driver: &Rc<ResourceDriver>,
    bytes: &[u8],
) -> String {
    let hash = driver.server_context().hasher().hash(bytes);
    let extension = content_type.extension().trim_start_matches('.');
    format!("{url}.pagespeed.{id}.{hash}.{extension}")
}

/// Injects the s-maxage window on streaming serves.
fn inject_s_maxage(driver: &Rc<ResourceDriver>, headers: &mut ResponseHeaders) {
    let s_maxage = driver.options().effective_in_place_s_maxage_sec();
    if s_maxage != -1 && headers.status_code() == 200 {
        headers.set_s_max_age(s_maxage);
    }
}

/// Scratch origin fetch.
fn scratch_fetch_for_origin(
    driver: &Rc<ResourceDriver>,
    url: &str,
) -> ps_http::fetch::BufferingFetch {
    let ctx = driver.request_context().borrow().clone();
    ps_http::fetch::BufferingFetch::new(url, ctx)
}

/// Finalizes optimized-serve headers (`FixFetchFallbackHeaders`).
#[allow(clippy::too_many_arguments)]
fn fix_fetch_fallback_headers(
    driver: &Rc<ResourceDriver>,
    url: &str,
    cached: &CachedResult,
    date_ms: i64,
    expire_ms: i64,
    stale: bool,
    headers: &mut ResponseHeaders,
) {
    if !cached.hash.is_empty() {
        headers
            .headers_mut()
            .replace(header::ETAG, &format!("{ETAG_PREFIX}aj-{}\"", cached.hash));
    }
    // NOTE: load-from-file inputs wait for file resources; the
    // implicit TTL always applies here.
    let implicit_ttl_ms = driver.options().implicit_cache_ttl_ms();
    headers.headers_mut().remove_all(header::LAST_MODIFIED);
    headers.set_implicit_cache_ttl_ms(implicit_ttl_ms);
    headers.compute_caching();
    let mut expire_at_ms = expire_ms;
    let now_ms = driver.server_context().now_ms();
    if expire_at_ms == i64::MAX {
        // If expire_at_ms is not set, set the cache ttl to the
        // implicit ttl value specified in the response headers.
        expire_at_ms = now_ms + headers.implicit_cache_ttl_ms();
    } else if stale {
        // If we are serving a stale rewrite, set the cache ttl
        // to the minimum of the implicit cache TTL and the
        // original ttl.
        expire_at_ms = now_ms + headers.implicit_cache_ttl_ms().min(expire_at_ms - date_ms);
    }
    headers.set_date_and_caching(now_ms, expire_at_ms - now_ms, "");
    headers.compute_caching();
    remove_redundant_rel_canonical_header(url, headers);
}

/// Folds input directives into output headers
/// (`ApplyInputCacheControl`, single input).
fn apply_input_cache_control(input_headers: &ResponseHeaders, headers: &mut ResponseHeaders) {
    headers.compute_caching();
    let mut input_headers = input_headers.clone();
    input_headers.compute_caching();
    let mut proxy_cacheable = headers.is_proxy_cacheable(
        RequestProperties::default(),
        VaryOption::IgnoreVaryOnResources,
        ValidatorOption::HasValidator,
    );
    let mut browser_cacheable = headers.is_browser_cacheable();
    let mut no_store = headers.has_cache_control_directive("no-store");
    let mut max_age = headers.cache_ttl_ms();
    let mut is_public = true;
    let mut saw_input = false;
    if input_headers.status_code() == 200 {
        if input_headers.cache_ttl_ms() < max_age {
            max_age = input_headers.cache_ttl_ms();
        }
        proxy_cacheable &= input_headers.is_proxy_cacheable(
            RequestProperties::default(),
            VaryOption::IgnoreVaryOnResources,
            ValidatorOption::HasValidator,
        );
        browser_cacheable &= input_headers.is_browser_cacheable();
        no_store |= input_headers.has_cache_control_directive("no-store");
        is_public &= input_headers
            .headers()
            .has_value_case_insensitive(header::CACHE_CONTROL, "public");
        saw_input = true;
    }
    if proxy_cacheable {
        if is_public && saw_input {
            headers.set_cache_control_public(max_age / 1000);
        }
    } else {
        let directives;
        if browser_cacheable {
            directives = ",private";
        } else {
            max_age = 0;
            directives = if no_store {
                ",no-cache,no-store"
            } else {
                ",no-cache"
            };
        }
        headers.set_date_and_caching(headers.date_ms(), max_age, directives);
        headers.headers_mut().remove(header::ETAG, "W/\"0\"");
        headers.compute_caching();
    }
}

/// Drops the self-canonical link (`RemoveRedundantRelCanonicalHeader`).
fn remove_redundant_rel_canonical_header(url: &str, headers: &mut ResponseHeaders) {
    let canonical = ResponseHeaders::rel_canonical_header_value(url);
    headers.headers_mut().remove(header::LINK, &canonical);
}

/// Adds Vary for browser-dependent rewrites (`AddVaryIfRequired`).
fn add_vary_if_required(
    driver: &Rc<ResourceDriver>,
    cached: &CachedResult,
    headers: &mut ResponseHeaders,
) {
    use ps_config::filter::Filter;
    use ps_image::format::ImageType;
    if !in_place_optimize_for_browser_enabled(driver) {
        // No browser-dependent rewrites => no need for vary
        return;
    }
    let Some(content_type) = headers.determine_content_type() else {
        return;
    };
    let options = driver.options();
    let properties = driver.request_properties();
    let mut new_vary = String::new();
    let mut depends_on_save_data = false;
    if content_type.is_image() {
        let image_type = cached.optimized_image_type.unwrap_or(ImageType::Unknown);
        if ImageUrlEncoder::allow_vary_on_user_agent(&options, &properties)
            && image_type != ImageType::Unknown
            && (options.enabled(Filter::ConvertJpegToWebp)
                || options.enabled(Filter::ConvertToAvif)
                || options.enabled(Filter::ConvertToWebpLossless)
                || options.enabled(Filter::ConvertToWebpAnimated)
                || options.has_valid_small_screen_qualities())
        {
            // If we are allowed to vary on user-agent and the
            // image has been successfully optimized, we need to
            // add "vary: user-agent", since we might have used
            // user-agent for determining image format and/or
            // quality.
            new_vary = header::USER_AGENT.to_string();
        } else if ImageUrlEncoder::allow_vary_on_accept(&options, &properties)
            && (image_type == ImageType::Jpeg
                || image_type == ImageType::Webp
                || image_type == ImageType::Avif)
            && (options.enabled(Filter::ConvertJpegToWebp)
                || options.enabled(Filter::ConvertToAvif))
        {
            // If we are allowed to vary on Accept header and the
            // image has been successfully optimized to lossy
            // format, we need to add "vary: accept", since we
            // might have used the Accept header for determining
            // image quality and whether WebP lossy could be used.
            new_vary = header::ACCEPT.to_string();
        }
        depends_on_save_data = image_type == ImageType::Jpeg
            || image_type == ImageType::Webp
            || image_type == ImageType::Avif
            || image_type == ImageType::WebpAnimated;
    } else if content_type.is_css() {
        // If it's CSS, constituent images can be rewritten in a
        // UA-dependent manner. But we don't necessarily see
        // Accept:image/webp on the request, so we must Vary:
        // User-Agent.
        if options.enabled(Filter::RewriteCss)
            && (options.enabled(Filter::ConvertJpegToWebp)
                || options.enabled(Filter::ConvertToAvif)
                || options.enabled(Filter::ConvertToWebpAnimated)
                || options.enabled(Filter::ConvertToWebpLossless))
        {
            new_vary = header::USER_AGENT.to_string();
            depends_on_save_data = true;
        }
    }
    // If Save-Data is allowed, add it to the Vary header.
    if depends_on_save_data && options.support_save_data() {
        if !new_vary.is_empty() {
            new_vary.push(',');
        }
        new_vary.push_str(header::SAVE_DATA);
    }
    if new_vary.is_empty() {
        return;
    }
    if options.private_not_vary_for_ie()
        && ps_http::user_agent_matcher::UserAgentMatcher::new().is_ie(&driver.user_agent())
    {
        // IE stores Vary: Accept resources in its cache, but
        // must revalidate them every single time they're
        // fetched (except for older IE, which doesn't cache
        // them at all). To avoid the re-validation cost we by
        // default serve these resource cache-control: private
        // to IE.
        headers.headers_mut().add(header::CACHE_CONTROL, "private");
        return;
    }
    if let Some(varies) = headers.headers().lookup(header::VARY) {
        // Need to add to the existing Vary header. But first,
        // check that the vary header doesn't already encompass
        // new_vary.
        for vary in varies {
            if vary == "*"
                || vary.eq_ignore_ascii_case(header::USER_AGENT)
                || (content_type.is_image() && vary.eq_ignore_ascii_case(header::ACCEPT))
            {
                // Current Vary: header captures necessary vary
                // information.
                return;
            }
        }
    }
    headers.headers_mut().add(header::VARY, &new_vary);
}
