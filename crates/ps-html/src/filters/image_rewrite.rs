//! Image rewriting (`ImageRewriteFilter`, `ic`).
//!
//! Recompresses, resizes, and converts images (JPEG/PNG/GIF/WebP),
//! inlines small ones as data URLs, and records dimensions. Fetch
//! reconstruction, srcset candidates, and CSS-nested rewrites share
//! the same single-input context machinery.
//!
//! NOTE: CSP gating, critical-images/beacon integration, the
//! property cache (rendered dimensions, inlinable URLs), logging,
//! and the central-controller worker pool wait for their epics.

use std::rc::Rc;

use ps_config::filter::Filter;
use ps_http::content_type::{mime_to_content_type, ContentType};
use ps_http::google_url::GoogleUrl;

use crate::element::NodeId;
use crate::filter::HtmlFilter;
use crate::image_url_encoder::{
    has_valid_dimension, has_valid_dimensions, ImageDim, ImageResourceContext, ImageUrlEncoder,
    LibWebpLevel,
};
use crate::lexer::Lexer;
use crate::metadata::CachedResult;
use crate::name::Keyword;
use crate::parser::Dom;
use crate::resource::{InputResource, Resource};
use crate::resource_slot::{relativize_or_passthrough, HtmlResourceSlot};
use crate::rewrite_context::{
    ContextSlot, RewriteResult, SingleRewriteBehavior, SingleRewriteContext,
};
use crate::server_context::{ImageRewriteRecord, InputRole, ResourceDriver};
use crate::url_segment_encoder::UrlSegmentEncoder;

/// Filter id (`RewriteOptions::kImageCompressionId`).
pub const FILTER_ID: &str = "ic";

/// Successful rewrites (`kImageRewrites`).
pub const IMAGE_REWRITES: &str = "image_rewrites";
/// High-resolution drops (`kImageNoRewritesHighResolution`).
pub const IMAGE_NOREWRITES_HIGH_RESOLUTION: &str = "image_norewrites_high_resolution";
/// Intentional drops (`kImageRewritesDroppedIntentionally`).
pub const IMAGE_REWRITES_DROPPED_INTENTIONALLY: &str = "image_rewrites_dropped_intentionally";
/// Name-decode drops (`kImageRewritesDroppedDecodeFailure`).
pub const IMAGE_REWRITES_DROPPED_DECODE_FAILURE: &str = "image_rewrites_dropped_decode_failure";
/// Write drops (`kImageRewritesDroppedServerWriteFail`).
pub const IMAGE_REWRITES_DROPPED_SERVER_WRITE_FAIL: &str =
    "image_rewrites_dropped_server_write_fail";
/// Unknown-MIME drops (`kImageRewritesDroppedMIMETypeUnknown`).
pub const IMAGE_REWRITES_DROPPED_MIME_TYPE_UNKNOWN: &str =
    "image_rewrites_dropped_mime_type_unknown";
/// No-saving resize drops (`kImageRewritesDroppedNoSavingResize`).
pub const IMAGE_REWRITES_DROPPED_NOSAVING_RESIZE: &str = "image_rewrites_dropped_nosaving_resize";
/// No-saving recompress drops (`kImageRewritesDroppedNoSavingNoResize`).
pub const IMAGE_REWRITES_DROPPED_NOSAVING_NORESIZE: &str =
    "image_rewrites_dropped_nosaving_noresize";
/// Load drops (`kImageRewritesDroppedDueToLoad`).
pub const IMAGE_REWRITES_DROPPED_DUE_TO_LOAD: &str = "image_rewrites_dropped_due_to_load";
/// Mobile squashes (`kImageRewritesSquashingForMobileScreen`).
pub const IMAGE_REWRITES_SQUASHING_FOR_MOBILE_SCREEN: &str =
    "image_rewrites_squashing_for_mobile_screen";
/// Bytes saved (`kImageRewriteTotalBytesSaved`).
pub const IMAGE_REWRITE_TOTAL_BYTES_SAVED: &str = "image_rewrite_total_bytes_saved";
/// Original bytes (`kImageRewriteTotalOriginalBytes`).
pub const IMAGE_REWRITE_TOTAL_ORIGINAL_BYTES: &str = "image_rewrite_total_original_bytes";
/// Rewrite uses (`kImageRewriteUses`).
pub const IMAGE_REWRITE_USES: &str = "image_rewrite_uses";
/// Inline count (`kImageInline`).
pub const IMAGE_INLINE: &str = "image_inline";
/// In-flight rewrites (`kImageOngoingRewrites`).
pub const IMAGE_ONGOING_REWRITES: &str = "image_ongoing_rewrites";
/// Rendered-dimension resizes (`kImageResizedUsingRenderedDimensions`).
pub const IMAGE_RESIZED_USING_RENDERED_DIMENSIONS: &str = "image_resized_using_rendered_dimensions";
/// Webp rewrites (`kImageWebpRewrites`).
pub const IMAGE_WEBP_REWRITES: &str = "image_webp_rewrites";
/// OK latency (`kImageRewriteLatencyOkMs`).
pub const IMAGE_REWRITE_LATENCY_OK_MS: &str = "image_rewrite_latency_ok_ms";
/// Failed latency (`kImageRewriteLatencyFailedMs`).
pub const IMAGE_REWRITE_LATENCY_FAILED_MS: &str = "image_rewrite_latency_failed_ms";
/// Total latency (`kImageRewriteLatencyTotalMs`).
pub const IMAGE_REWRITE_LATENCY_TOTAL_MS: &str = "image_rewrite_latency_total_ms";

/// Rewrite place (`Context::Place`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewritePlace {
    /// Inside CSS.
    Css,
    /// Serving fetch.
    Fetch,
    /// HTML attribute.
    HtmlAttr,
    /// Srcset candidate.
    Srcset,
    /// Non-CSS nested rewrite.
    NonCssNested,
}

/// Inline outcome (`InlineResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineResult {
    /// Inlined.
    Success,
    /// No inlined bytes cached.
    NoData,
    /// Too large for the threshold.
    TooLarge,
    /// Device cannot inline (or webp forbidden).
    UnsupportedDevice,
    /// Not a critical image.
    NotCritical,
    /// Small images stay unrewritten (property cache).
    CacheSmallImagesUnrewritten,
    /// Responsive virtual image.
    Responsive,
    /// Shortcut icon (link tag).
    Shortcut,
    /// Corrupt inline data.
    InternalError,
}

/// Parses a dimension attribute (`ParseDimensionAttribute`):
/// optional whitespace/plus, digits ( nonzero, no overflow),
/// optional fraction (rounded), optional `px`, nothing else.
#[must_use]
pub fn parse_dimension_attribute(value: &[u8]) -> Option<i32> {
    let mut position = skip_ascii_whitespace(value);
    // Skip leading plus.
    if position.first() == Some(&b'+') {
        position = &position[1..];
    }
    let mut result: u32 = 0;
    let mut digits = 0;
    while let Some(&byte) = position.first() {
        if !byte.is_ascii_digit() {
            break;
        }
        result = result
            .checked_mul(10)?
            .checked_add(u32::from(byte - b'0'))?;
        digits += 1;
        position = &position[1..];
    }
    // Reject if no digits or only zeroes, or conversion to signed
    // will fail.
    if digits == 0 || result < 1 || result > i32::MAX as u32 {
        return None;
    }
    // Process fraction (including 45. with nothing after the .).
    if position.first() == Some(&b'.') {
        position = &position[1..];
        if let Some(&byte) = position.first() {
            if (b'5'..=b'9').contains(&byte) && result < i32::MAX as u32 {
                // Round based on leading fraction digit, avoiding
                // overflow.
                result += 1;
                position = &position[1..];
            }
        }
        // Discard all fraction digits.
        while position.first().is_some_and(u8::is_ascii_digit) {
            position = &position[1..];
        }
    }
    // Skip whitespace before a possible trailing px. The spec allows
    // other junk, or a trailing percent, but we can't resize
    // percentages and older browsers don't resize on junk.
    position = skip_ascii_whitespace(position);
    if position.len() >= 2 && position[0] == b'p' && position[1] == b'x' {
        position = skip_ascii_whitespace(&position[2..]);
    }
    // Reject if there's trailing junk.
    if !position.is_empty() {
        return None;
    }
    Some(result as i32)
}

/// Skips ASCII whitespace (`SkipAsciiWhitespace`).
fn skip_ascii_whitespace(mut value: &[u8]) -> &[u8] {
    while let Some(&byte) = value.first() {
        if byte == b' ' || byte == b'\x09' || byte == b'\x0A' || byte == b'\x0C' || byte == b'\x0D'
        {
            value = &value[1..];
        } else {
            break;
        }
    }
    value
}

/// Single-image behavior (context owned per element/candidate/fetch).
pub struct ImageRewriteBehavior {
    // Unread C++ `ImageRewriteFilter` state (driver, candidate
    // index, resize mode), stored for parity with the C++ filter.
    #[allow(dead_code)]
    driver: Rc<ResourceDriver>,
    resource_context: ImageResourceContext,
    place: RewritePlace,
    #[allow(dead_code)]
    image_index: i32,
    in_noscript_element: bool,
    css_image_inline_max_bytes: i64,
    #[allow(dead_code)]
    is_resized_using_rendered_dimensions: bool,
    base_url: GoogleUrl,
    parent_debug: Option<Rc<std::cell::RefCell<Vec<String>>>>,
}

impl ImageRewriteBehavior {
    /// Builds a behavior over a driver and resource context.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        driver: Rc<ResourceDriver>,
        resource_context: ImageResourceContext,
        place: RewritePlace,
        image_index: i32,
        in_noscript_element: bool,
        css_image_inline_max_bytes: i64,
        is_resized_using_rendered_dimensions: bool,
        base_url: GoogleUrl,
    ) -> Self {
        ImageRewriteBehavior {
            driver,
            resource_context,
            place,
            image_index,
            in_noscript_element,
            css_image_inline_max_bytes,
            is_resized_using_rendered_dimensions,
            base_url,
            parent_debug: None,
        }
    }

    /// Builds a nested CSS behavior (`MakeNestedRewriteContextForCss`
    /// behavior half): UA-encoded context, no dimensions, and the CSS
    /// inline budget. Parent debug collects the inline verdict plus
    /// the partition messages at harvest.
    #[must_use]
    pub fn for_css(
        driver: Rc<ResourceDriver>,
        css_image_inline_max_bytes: i64,
        parent_debug: Rc<std::cell::RefCell<Vec<String>>>,
    ) -> Self {
        let mut resource_context = ImageResourceContext::default();
        encode_user_agent_into_resource_context(&driver, &mut resource_context);
        ImageRewriteBehavior {
            driver,
            resource_context,
            place: RewritePlace::Css,
            image_index: 0,
            in_noscript_element: false,
            css_image_inline_max_bytes,
            is_resized_using_rendered_dimensions: false,
            base_url: GoogleUrl::new(""),
            parent_debug: Some(parent_debug),
        }
    }
}

/// Context-free image codec adapter (`encoder()`): decodes names
/// leniently for serving validation; encoding without dimensions
/// only feeds partition-key fragments.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageUrlSegmentEncoder;

impl UrlSegmentEncoder for ImageUrlSegmentEncoder {
    fn encode(&self, urls: &[Vec<u8>]) -> Vec<u8> {
        let encoder = ImageUrlEncoder;
        let url = urls.first().map(Vec::as_slice).unwrap_or(b"");
        encoder.encode(url, &ImageResourceContext::default())
    }

    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>> {
        let encoder = ImageUrlEncoder;
        let mut context = ImageResourceContext::default();
        encoder.decode(segment, &mut context).map(|url| vec![url])
    }
}

/// Shared image codec.
static IMAGE_ENCODER: ImageUrlSegmentEncoder = ImageUrlSegmentEncoder;

impl SingleRewriteBehavior for ImageRewriteBehavior {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &IMAGE_ENCODER
    }

    fn encode_output_name(&self, leaf: &[u8]) -> Vec<u8> {
        ImageUrlEncoder.encode(leaf, &self.resource_context)
    }

    fn decode_output_name(&self, name: &[u8]) -> Option<Vec<Vec<u8>>> {
        let mut context = self.resource_context.clone();
        ImageUrlEncoder
            .decode(name, &mut context)
            .map(|url| vec![url])
    }

    fn key_suffix_extra(&self) -> String {
        let encoder = ImageUrlEncoder;
        let dims = encoder.encode(b"", &self.resource_context);
        format!(
            "{}@{}",
            String::from_utf8_lossy(&dims),
            ImageUrlEncoder::cache_key_from_resource_context(&self.resource_context)
        )
    }

    fn policy_permits_rendering(
        &self,
        _driver: &Rc<ResourceDriver>,
        _outputs: &[Option<crate::output_resource::OutputResource>],
    ) -> bool {
        // NOTE: `AreOutputsAllowedByCsp(kImgSrc)` waits for the CSP
        // subsystem.
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn render_partition(
        &self,
        driver: &Rc<ResourceDriver>,
        dom: &mut Dom,
        _base: &GoogleUrl,
        options: &ps_config::options::RewriteOptions,
        partition: &CachedResult,
        slots: &[ContextSlot],
        _outputs: &[Option<crate::output_resource::OutputResource>],
        slot_indices: &[usize],
    ) -> bool {
        match self.place {
            RewritePlace::HtmlAttr => {
                let Some(&slot_index) = slot_indices.first() else {
                    return false;
                };
                let ContextSlot::Html(slot) = &slots[slot_index] else {
                    return false;
                };
                let mut inline_result = InlineResult::NoData;
                let rewrote = finish_rewrite_image_url(
                    driver,
                    self,
                    partition,
                    dom,
                    &self.base_url,
                    options,
                    slot,
                    &mut inline_result,
                );
                if options.enabled(Filter::InlineImages) {
                    let message = message_for_inline_result(inline_result);
                    if let Some(element) = slot.borrow().element() {
                        driver.insert_debug_comment(dom, message, element);
                    }
                }
                if rewrote {
                    // We wrote out the URL ourselves; don't let the
                    // default handling mess it up.
                    slots[slot_index].set_disable_rendering();
                }
                rewrote
            }
            RewritePlace::Srcset => false,
            RewritePlace::Css => {
                let Some(&slot_index) = slot_indices.first() else {
                    return false;
                };
                let mut inline_result = InlineResult::NoData;
                let rewrote = finish_rewrite_css_image_url(
                    driver,
                    self.css_image_inline_max_bytes,
                    partition,
                    &slots[slot_index],
                    &mut inline_result,
                );
                if driver.debug_mode() && options.enabled(Filter::InlineImages) {
                    let message = message_for_inline_result(inline_result);
                    // The stylesheet's context keeps the message, so
                    // the HTML that references the stylesheet can show
                    // it, within the document's debug budget.
                    if let Some(parent) = &self.parent_debug {
                        if !message.is_empty() {
                            parent.borrow_mut().push(message.to_string());
                        }
                    }
                }
                if rewrote {
                    // We wrote out the URL ourselves; don't let the
                    // default handling mess it up (in particular
                    // replacing data: with out-of-line version).
                    slots[slot_index].set_disable_rendering();
                }
                rewrote
            }
            RewritePlace::Fetch | RewritePlace::NonCssNested => false,
        }
    }

    fn rewrite_single(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &InputResource,
        output: &mut crate::output_resource::OutputResource,
        partition: &mut CachedResult,
    ) -> RewriteResult {
        rewrite_loaded_resource(self, driver, input, output, partition)
    }
}

/// Stats-backed conversion recorder.
struct StatsRecorder {
    server: Rc<crate::server_context::ServerContext>,
}

impl StatsRecorder {
    fn webp_var_names(var: ps_image::stats::WebpVar) -> (&'static str, &'static str, &'static str) {
        match var {
            ps_image::stats::WebpVar::FromGif => (
                "image_webp_conversion_gif_timeouts",
                "image_webp_conversion_gif_success_ms",
                "image_webp_conversion_gif_failure_ms",
            ),
            ps_image::stats::WebpVar::FromPng => (
                "image_webp_conversion_png_timeouts",
                "image_webp_conversion_png_success_ms",
                "image_webp_conversion_png_failure_ms",
            ),
            ps_image::stats::WebpVar::FromJpeg => (
                "image_webp_conversion_jpeg_timeouts",
                "image_webp_conversion_jpeg_success_ms",
                "image_webp_conversion_jpeg_failure_ms",
            ),
            ps_image::stats::WebpVar::FromGifAnimated => (
                "image_webp_conversion_gif_animated_timeouts",
                "image_webp_conversion_gif_animated_success_ms",
                "image_webp_conversion_gif_animated_failure_ms",
            ),
            ps_image::stats::WebpVar::Opaque => (
                "image_webp_opaque_timeouts",
                "image_webp_opaque_success_ms",
                "image_webp_opaque_failure_ms",
            ),
            ps_image::stats::WebpVar::NonOpaque => (
                "image_webp_alpha_timeouts",
                "image_webp_alpha_success_ms",
                "image_webp_alpha_failure_ms",
            ),
            ps_image::stats::WebpVar::FromUnknownFormat => (
                "image_webp_conversion_unknown_timeouts",
                "image_webp_conversion_unknown_success_ms",
                "image_webp_conversion_unknown_failure_ms",
            ),
        }
    }
}

impl ps_image::stats::Recorder for StatsRecorder {
    fn webp_conversion(
        &self,
        var: ps_image::stats::WebpVar,
        ok: bool,
        timed_out: bool,
        elapsed_ms: i64,
    ) {
        let (timeouts, success_ms, failure_ms) = Self::webp_var_names(var);
        if timed_out {
            self.server.variable(timeouts).add(1);
        } else if ok {
            self.server.histogram(success_ms).add(elapsed_ms as f64);
        } else {
            self.server.histogram(failure_ms).add(elapsed_ms as f64);
        }
    }

    fn avif_conversion(
        &self,
        ok: bool,
        timed_out: bool,
        wall_elapsed_ms: i64,
        child_cpu_ms: Option<i64>,
    ) {
        if timed_out {
            self.server
                .variable("image_avif_conversion_timeouts")
                .add(1);
        } else if ok {
            self.server
                .histogram("image_avif_conversion_success_ms")
                .add(wall_elapsed_ms as f64);
        } else {
            self.server
                .histogram("image_avif_conversion_failure_ms")
                .add(wall_elapsed_ms as f64);
        }
        // Child CPU is reported for every reaped encoder, including
        // killed ones.
        if let Some(child_cpu_ms) = child_cpu_ms {
            self.server
                .histogram("image_avif_conversion_child_cpu_ms")
                .add(child_cpu_ms as f64);
        }
    }

    fn avif_shed(&self) {
        self.server.variable("image_avif_conversion_shed").add(1);
    }
}

/// Maps an image type to its content type (`Image::TypeToContentType`).
fn image_type_to_content_type(image_type: ps_image::format::ImageType) -> Option<ContentType> {
    use ps_image::format::ImageType;
    let mime = match image_type {
        ImageType::Jpeg => "image/jpeg",
        ImageType::Png => "image/png",
        ImageType::Gif => "image/gif",
        ImageType::Webp | ImageType::WebpLosslessOrAlpha | ImageType::WebpAnimated => "image/webp",
        ImageType::Avif => "image/avif",
        ImageType::Unknown => return None,
    };
    mime_to_content_type(mime)
}

/// Sets webp conversion options (`SetWebpCompressionOptions`).
fn set_webp_compression_options(
    resource_context: &ImageResourceContext,
    options: &ps_config::options::RewriteOptions,
    image_options: &mut ps_image::image::CompressionOptions,
) {
    use ps_image::image::PreferredWebp;
    match resource_context.libwebp_level {
        LibWebpLevel::None => {
            image_options.preferred_webp = PreferredWebp::None;
            image_options.allow_webp_alpha = false;
        }
        LibWebpLevel::LossyOnly => {
            image_options.preferred_webp = PreferredWebp::Lossy;
            image_options.allow_webp_alpha = false;
        }
        LibWebpLevel::Animated => {
            if options.enabled(Filter::ConvertToWebpAnimated) {
                image_options.preferred_webp = PreferredWebp::Animated;
                image_options.allow_webp_animated = true;
                image_options.allow_webp_alpha = true;
            } else {
                image_options.allow_webp_alpha = true;
                set_lossy_or_lossless(options, image_options);
            }
        }
        LibWebpLevel::LossyLosslessAlpha => {
            image_options.allow_webp_alpha = true;
            set_lossy_or_lossless(options, image_options);
        }
    }
}

/// Picks lossy vs lossless webp for alpha-capable agents.
fn set_lossy_or_lossless(
    options: &ps_config::options::RewriteOptions,
    image_options: &mut ps_image::image::CompressionOptions,
) {
    use ps_image::image::PreferredWebp;
    if options.enabled(Filter::ConvertToWebpLossless) {
        image_options.preferred_webp = PreferredWebp::Lossless;
    } else {
        image_options.preferred_webp = PreferredWebp::Lossy;
    }
}

/// Resolves qualities into compression options (`DetermineQualities`).
fn determine_qualities(
    options: &ps_config::options::RewriteOptions,
    resource_context: &ImageResourceContext,
    image_options: &mut ps_image::image::CompressionOptions,
) {
    if resource_context.may_use_save_data_quality {
        // Use Save-Data qualities.
        image_options.webp_quality = options.image_webp_quality_save_data();
        image_options.webp_animated_quality = options.image_webp_quality_save_data();
        image_options.jpeg_quality = options.image_jpeg_quality_save_data();
        image_options.jpeg_num_progressive_scans = options.image_jpeg_num_progressive_scans();
    } else if resource_context.may_use_small_screen_quality {
        // Use small screen qualities.
        image_options.webp_quality = options.image_webp_quality_for_small_screen();
        image_options.webp_animated_quality = options.image_webp_animated_quality();
        image_options.jpeg_quality = options.image_jpeg_quality_for_small_screen();
        image_options.jpeg_num_progressive_scans =
            options.image_jpeg_num_progressive_scans_for_small_screen();
    } else {
        // Use regular (desktop) qualities.
        image_options.webp_quality = options.image_webp_quality();
        image_options.webp_animated_quality = options.image_webp_animated_quality();
        image_options.jpeg_quality = options.image_jpeg_quality();
        image_options.jpeg_num_progressive_scans = options.image_jpeg_num_progressive_scans();
    }
}

/// Builds compression options for a loaded resource
/// (`ImageOptionsForLoadedResource`).
pub fn image_options_for_loaded_resource(
    resource_context: &ImageResourceContext,
    input_size: i64,
    options: &ps_config::options::RewriteOptions,
) -> ps_image::image::CompressionOptions {
    let mut image_options = ps_image::image::CompressionOptions::default();
    if resource_context.libwebp_level != LibWebpLevel::None {
        set_webp_compression_options(resource_context, options, &mut image_options);
    }
    determine_qualities(options, resource_context, &mut image_options);
    image_options.progressive_jpeg = options.enabled(Filter::ConvertJpegToProgressive)
        && input_size >= options.progressive_jpeg_min_bytes();
    image_options.progressive_jpeg_min_bytes = options.progressive_jpeg_min_bytes();
    image_options.convert_png_to_jpeg = options.enabled(Filter::ConvertPngToJpeg);
    image_options.convert_gif_to_png = options.enabled(Filter::ConvertGifToPng);
    image_options.convert_jpeg_to_webp = options.enabled(Filter::ConvertJpegToWebp);
    image_options.convert_to_avif =
        options.enabled(Filter::ConvertToAvif) && resource_context.supports_avif;
    image_options.recompress_jpeg = options.enabled(Filter::RecompressJpeg);
    image_options.recompress_png = options.enabled(Filter::RecompressPng);
    image_options.recompress_webp = options.enabled(Filter::RecompressWebp);
    image_options.retain_color_profile = !options.enabled(Filter::StripImageColorProfile);
    image_options.retain_exif_data = !options.enabled(Filter::StripImageMetaData);
    image_options.retain_color_sampling = !options.enabled(Filter::JpegSubsampling);
    image_options.webp_conversion_timeout_ms = options.image_webp_timeout_ms();
    image_options.avif_conversion_timeout_ms = options.image_avif_timeout_ms();
    image_options.avif_max_concurrent_conversions =
        options.image_avif_max_concurrent_conversions() as i32;
    image_options
}

/// Computes a width preserving aspect ratio (`GetPageWidth`).
fn get_page_width(page_height: i64, image_width: i64, image_height: i64) -> i64 {
    if image_height > 0 {
        (page_height * image_width + image_height / 2) / image_height
    } else {
        // The client should ensure that "image_height > 0". If this
        // condition is not met, we protect against division by 0 by
        // returning 0 so that resize attempts will fail.
        0
    }
}

/// Computes a height preserving aspect ratio (`GetPageHeight`).
fn get_page_height(page_width: i64, image_height: i64, image_width: i64) -> i64 {
    if image_height > 0 {
        (page_width * image_height + image_width / 2) / image_width
    } else {
        // The client should ensure that "image_width > 0". If this
        // condition is not met, we protect against division by 0 by
        // returning 0 so that resize attempts will fail.
        0
    }
}

/// Fills a missing side preserving aspect ratio
/// (`SetDesiredDimensionsIfRequired`).
fn set_desired_dimensions_if_required(desired_dim: &mut ImageDim, image_dim: &ImageDim) {
    if !has_valid_dimension(desired_dim) {
        return;
    }
    let image_width = image_dim.width.unwrap_or(0) as i64;
    let image_height = image_dim.height.unwrap_or(0) as i64;
    if desired_dim.width.is_none() {
        // Fill in a missing page width, rounding the result.
        if let Some(page_height) = desired_dim.height {
            let width = get_page_width(page_height as i64, image_width, image_height);
            desired_dim.width = Some(width.max(0) as u32);
        }
    } else if desired_dim.height.is_none() {
        if let Some(page_width) = desired_dim.width {
            let height = get_page_height(page_width as i64, image_height, image_width);
            desired_dim.height = Some(height.max(0) as u32);
        }
    }
}

/// Whether the image should resize (`ShouldResize`), filling the
/// desired dimensions.
fn should_resize(
    resource_context: &ImageResourceContext,
    image_type: ps_image::format::ImageType,
    image_dim: &ImageDim,
    options: &ps_config::options::RewriteOptions,
    desired_dim: &mut ImageDim,
) -> bool {
    use ps_image::format::ImageType;
    if !options.enabled(Filter::ResizeImages)
        && !options.enabled(Filter::ResizeToRenderedImageDimensions)
    {
        return false;
    }
    if image_type != ImageType::Gif
        || options.enabled(Filter::ConvertGifToPng)
        || options.enabled(Filter::DelayImages)
    {
        *desired_dim = resource_context.desired_image_dims;
        if options.enabled(Filter::ResizeToRenderedImageDimensions) {
            // Respect the aspect ratio of the image when doing the resize.
            set_desired_dimensions_if_required(desired_dim, image_dim);
        } else {
            // UpdateDesiredImageDimsIfNecessary is a no-op override point.
            if options.enabled(Filter::ResizeImages)
                && has_valid_dimension(desired_dim)
                && has_valid_dimensions(image_dim)
            {
                set_desired_dimensions_if_required(desired_dim, image_dim);
            }
        }
        if has_valid_dimension(desired_dim) && has_valid_dimensions(image_dim) {
            let page_area =
                desired_dim.width.unwrap_or(0) as i64 * desired_dim.height.unwrap_or(0) as i64;
            let image_area =
                image_dim.width.unwrap_or(0) as i64 * image_dim.height.unwrap_or(0) as i64;
            if page_area * 100 < image_area * options.image_limit_resize_area_percent() {
                return true;
            }
        }
    }
    false
}

/// Rewrites one loaded image (`RewriteLoadedResourceImpl`).
fn rewrite_loaded_resource(
    behavior: &ImageRewriteBehavior,
    driver: &Rc<ResourceDriver>,
    input: &InputResource,
    output: &mut crate::output_resource::OutputResource,
    partition: &mut CachedResult,
) -> RewriteResult {
    use ps_image::format::{ImageFormat, ImageType};
    // If requested, drop random image rewrites. Eventually,
    // frequently requested images will get optimized but the long
    // tail won't be optimized much.
    let drop_percentage = driver.options().rewrite_random_drop_percentage();
    if drop_percentage > 0 && behavior.place != RewritePlace::Css {
        // Note that we don't randomly drop if this is a nested
        // context of the CSS filter as we don't want to partially
        // rewrite a CSS file.
        let roll = driver.server_context().simple_random_next() % 100;
        if drop_percentage > roll as i64 {
            return RewriteResult::TooBusy;
        }
    }
    crate::rewrite_context::add_link_rel_canonical(driver, input, output.response_headers_mut());
    let options = driver.options().clone();
    let mut resource_context = behavior.resource_context.clone();
    let name = output.full_name().name().to_string();
    if ImageUrlEncoder
        .decode(name.as_bytes(), &mut resource_context)
        .is_none()
    {
        driver
            .server_context()
            .variable(IMAGE_REWRITES_DROPPED_INTENTIONALLY)
            .add(1);
        driver
            .server_context()
            .variable(IMAGE_REWRITES_DROPPED_DECODE_FAILURE)
            .add(1);
        return RewriteResult::Failed;
    }

    let contents = input.contents();
    let image_options =
        image_options_for_loaded_resource(&resource_context, contents.len() as i64, &options);
    let original_image_type = ps_image::format::sniff(&contents);
    if original_image_type == ImageType::Unknown {
        driver
            .server_context()
            .variable(IMAGE_REWRITES_DROPPED_INTENTIONALLY)
            .add(1);
        driver
            .server_context()
            .variable(IMAGE_REWRITES_DROPPED_MIME_TYPE_UNKNOWN)
            .add(1);
        return RewriteResult::Failed;
    }
    let original_size = contents.len();
    let mut optimized_size = original_size;
    let mut is_resized = false;

    let analysis = match ps_image::image::analyze(&contents) {
        Ok(analysis) => analysis,
        Err(_) => {
            driver
                .server_context()
                .variable(IMAGE_REWRITES_DROPPED_INTENTIONALLY)
                .add(1);
            driver
                .server_context()
                .variable(IMAGE_REWRITES_DROPPED_DECODE_FAILURE)
                .add(1);
            return RewriteResult::Failed;
        }
    };
    let image_dim = ImageDim {
        width: Some(analysis.width),
        height: Some(analysis.height),
    };
    if (analysis.width as i64) * (analysis.height as i64) * 4
        > options.image_resolution_limit_bytes()
    {
        driver
            .server_context()
            .variable(IMAGE_REWRITES_DROPPED_INTENTIONALLY)
            .add(1);
        driver
            .server_context()
            .variable(IMAGE_NOREWRITES_HIGH_RESOLUTION)
            .add(1);
        return RewriteResult::Failed;
    }

    driver
        .server_context()
        .up_down_counter(IMAGE_ONGOING_REWRITES)
        .add(1);
    let rewrite_start = std::time::Instant::now();
    let mut rewrite_result = RewriteResult::Failed;

    // Begin by resizing the image if necessary.
    let debug_url = url_for_debug_messages(driver, input.url());
    let mut working_bytes = contents.clone();
    let mut working_type = original_image_type;
    let mut desired_dim = ImageDim::default();
    if analysis.width < 1 || analysis.height < 1 {
        if driver.debug_mode() {
            partition.debug_message.push(format!(
                "Cannot resize {}: Image must be at least 1x1",
                debug_url
            ));
        }
    } else if should_resize(
        &resource_context,
        original_image_type,
        &image_dim,
        &options,
        &mut desired_dim,
    ) {
        let width = desired_dim.width.unwrap_or(0);
        let height = desired_dim.height.unwrap_or(0);
        match ps_image::image::resize_image(&contents, width, height, &image_options) {
            Ok((resized, format)) => {
                working_bytes = resized;
                // GIF input converts through its first frame (treated
                // as PNG from here on).
                if working_type == ImageType::Gif && format == ImageFormat::Png {
                    working_type = ImageType::Png;
                }
                is_resized = true;
                if driver.debug_mode() {
                    partition.debug_message.push(format!(
                        "Resized image {} from {}x{} to {}x{}",
                        &debug_url, analysis.width, analysis.height, width, height,
                    ));
                }
            }
            Err(_) => {
                desired_dim = ImageDim::default();
            }
        }
    } else if driver.debug_mode() {
        partition.debug_message.push(format!(
            "Image {} does not appear to need resizing.",
            debug_url
        ));
    }
    // Cache image dimensions, including any resizing we did. This
    // happens regardless of whether we rewrite the image contents.
    let post_resize_dim = if is_resized {
        ImageDim {
            width: desired_dim.width,
            height: desired_dim.height,
        }
    } else {
        image_dim
    };
    if has_valid_dimensions(&post_resize_dim) {
        partition.image_file_dims = Some((
            post_resize_dim.width.unwrap_or(0),
            post_resize_dim.height.unwrap_or(0),
        ));
    }

    // The analysis message lands after any resize message, as in
    // C++ (where the selector runs during output computation).
    if (original_image_type == ImageType::Gif || original_image_type == ImageType::Png)
        && driver.debug_mode()
    {
        partition.debug_message.push(format!(
            "Image {} has{} transparent pixels, is{} sensitive to compression noise, and has{} animation.",
            &debug_url,
            if analysis.transparency { "" } else { " no" },
            if analysis.photo { " not" } else { "" },
            if analysis.animated { "" } else { " no" },
        ));
    }

    // When resizing was expected but failed, drop the dimensions
    // from the output name: re-encode the URL decoded above, rather
    // than the leaf of the input URL.
    if !is_resized {
        resource_context.desired_image_dims = ImageDim::default();
        let decoded = ImageUrlEncoder.decode(name.as_bytes(), &mut ImageResourceContext::default());
        if let Some(url) = decoded {
            let renamed = ImageUrlEncoder.encode(&url, &resource_context);
            output.set_name(&String::from_utf8_lossy(&renamed));
        }
    }

    // Now re-compress the (possibly resized) image, and decide if
    // it's saved us anything.
    if is_resized || options.image_optimization_enabled() {
        let natural_dims = Some((analysis.width, analysis.height));
        let resized_dims = if is_resized {
            Some((
                desired_dim.width.unwrap_or(analysis.width),
                desired_dim.height.unwrap_or(analysis.height),
            ))
        } else {
            None
        };
        let ctx = ps_image::image::TranscodeContext {
            resized: is_resized,
            resized_dims,
            original_type: Some(original_image_type),
        };
        let mut budget = ps_image::image::ConversionBudget::new();
        let recorder = StatsRecorder {
            server: driver.server_context().clone(),
        };
        let transcoded = ps_image::image::transcode(
            &working_bytes,
            natural_dims,
            ctx,
            &image_options,
            &mut budget,
            false,
            &recorder,
        );
        // A failed recompress falls back to the (possibly resized)
        // working bytes, like C++ `Contents()`.
        let (out_bytes, out_type) = match transcoded {
            Ok(transcoded) => (transcoded.bytes, transcoded.image_type),
            Err(_) => (working_bytes.clone(), working_type),
        };
        {
            optimized_size = out_bytes.len();
            if optimized_size * 100
                < contents.len() * options.image_limit_optimized_percent() as usize
            {
                // Here output image type could potentially be
                // different from input type.
                let output_type = image_type_to_content_type(out_type);
                // Consider inlining output image (no need to check
                // input, it's bigger). This needs to happen before
                // Write to persist.
                save_if_inlinable(&out_bytes, out_type, &options, partition);
                driver.server_context().merge_non_caching_response_headers(
                    input.response_headers(),
                    output.response_headers_mut(),
                );
                if options.no_transform_optimized_images() {
                    output.set_cache_control_suffix(",no-transform");
                }
                if driver.write_output(
                    std::slice::from_ref(input),
                    &out_bytes,
                    output_type,
                    "",
                    output,
                    partition,
                ) {
                    // Update stats.
                    driver.server_context().variable(IMAGE_REWRITES).add(1);
                    // Record the rewrite for the admin images page.
                    driver.record_image_rewrite(ImageRewriteRecord {
                        timestamp_ms: driver.server_context().now_ms(),
                        original_bytes: original_size as i64,
                        optimized_bytes: optimized_size as i64,
                        output_mime_type: output_type.map_or_else(
                            || "application/octet-stream".to_string(),
                            |content_type| content_type.mime_type().to_string(),
                        ),
                        original_url: input.url().to_string(),
                        optimized_url: output.url().to_string(),
                    });
                    driver
                        .server_context()
                        .variable(IMAGE_REWRITE_TOTAL_BYTES_SAVED)
                        .add(original_size as i64 - optimized_size as i64);
                    driver
                        .server_context()
                        .variable(IMAGE_REWRITE_TOTAL_ORIGINAL_BYTES)
                        .add(original_size as i64);
                    if output_type
                        .is_some_and(|content_type| content_type.mime_type() == "image/webp")
                    {
                        driver.server_context().variable(IMAGE_WEBP_REWRITES).add(1);
                    }
                    rewrite_result = RewriteResult::Ok;
                } else {
                    // Server fails to write merged files.
                    driver
                        .server_context()
                        .variable(IMAGE_REWRITES_DROPPED_SERVER_WRITE_FAIL)
                        .add(1);
                }
            } else if is_resized {
                // Eliminate any image dimensions from a resize
                // operation that succeeded, but yielded overly-large
                // output.
                driver
                    .server_context()
                    .variable(IMAGE_REWRITES_DROPPED_NOSAVING_RESIZE)
                    .add(1);
                partition.image_file_dims = None;
            } else if options.image_optimization_enabled() {
                // Fails due to overly-large output without resize.
                driver
                    .server_context()
                    .variable(IMAGE_REWRITES_DROPPED_NOSAVING_NORESIZE)
                    .add(1);
            }
        }
    }

    partition.optimized_size = if rewrite_result == RewriteResult::Ok {
        optimized_size as i64
    } else {
        original_size as i64
    };

    // Try inlining input image if output hasn't been inlined already.
    if partition.inlined_data.is_none() {
        save_if_inlinable(&contents, original_image_type, &options, partition);
    }

    maybe_generate_low_res_preview(
        behavior,
        driver,
        &options,
        &resource_context,
        &working_bytes,
        optimized_size,
        input,
        partition,
    );
    driver
        .server_context()
        .up_down_counter(IMAGE_ONGOING_REWRITES)
        .add(-1);

    let latency_ms = rewrite_start.elapsed().as_millis() as f64;
    if rewrite_result == RewriteResult::Ok {
        driver
            .server_context()
            .histogram(IMAGE_REWRITE_LATENCY_OK_MS)
            .add(latency_ms);
    } else {
        driver
            .server_context()
            .histogram(IMAGE_REWRITE_LATENCY_FAILED_MS)
            .add(latency_ms);
    }
    // We track the total latency (including failed & OK) in its own
    // variable so it can be easily scraped.
    driver
        .server_context()
        .histogram(IMAGE_REWRITE_LATENCY_TOTAL_MS)
        .add(latency_ms);

    // All other conditions were updated in other code paths above.
    if rewrite_result == RewriteResult::Failed {
        driver
            .server_context()
            .variable(IMAGE_REWRITES_DROPPED_INTENTIONALLY)
            .add(1);
    }
    // NOTE: background-rewrite activity logging waits for the log record.
    rewrite_result
}

/// Saves inline bytes when under the largest threshold
/// (`SaveIfInlinable`).
fn save_if_inlinable(
    contents: &[u8],
    image_type: ps_image::format::ImageType,
    options: &ps_config::options::RewriteOptions,
    cached: &mut CachedResult,
) {
    // We retain inlining information if the image size is < the
    // largest possible inlining threshold, as an image might be used
    // in both html and css and we may see it first from the one with
    // a smaller threshold.
    if (contents.len() as i64) < options.max_image_inline_max_bytes() {
        cached.inlined_data = Some(contents.to_vec());
        cached.inlined_image_mime = image_type_to_content_type(image_type)
            .map(|content_type| content_type.mime_type().to_string());
    }
}

/// Generates a low-resolution preview for delay-images.
#[allow(clippy::too_many_arguments)]
fn maybe_generate_low_res_preview(
    behavior: &ImageRewriteBehavior,
    driver: &Rc<ResourceDriver>,
    options: &ps_config::options::RewriteOptions,
    resource_context: &ImageResourceContext,
    working_bytes: &[u8],
    optimized_size: usize,
    _input: &InputResource,
    cached: &mut CachedResult,
) {
    use ps_image::format::ImageType;
    let image_size = optimized_size as i64;
    if !options.enabled(Filter::DelayImages)
        || behavior.in_noscript_element
        || cached.low_resolution_inlined_data.is_some()
        || image_size < options.min_image_size_low_resolution_bytes()
        || image_size > options.max_image_size_low_resolution_bytes()
    {
        return;
    }
    let mut image_options = ps_image::image::CompressionOptions::default();
    set_webp_compression_options(resource_context, options, &mut image_options);
    image_options.jpeg_quality = options.image_jpeg_quality();
    image_options.webp_quality = options.image_webp_quality();
    image_options.webp_animated_quality = options.image_webp_animated_quality();
    image_options.progressive_jpeg = false;
    image_options.convert_png_to_jpeg = options.enabled(Filter::ConvertPngToJpeg);
    // Set to true since we optimize a gif to png before resize.
    image_options.convert_gif_to_png = true;
    image_options.recompress_jpeg = true;
    image_options.recompress_png = true;
    image_options.recompress_webp = true;
    // Since these are replaced with their high res versions,
    // stripping them off for low res images will further reduce
    // bytes.
    image_options.retain_color_profile = false;
    image_options.retain_exif_data = false;
    image_options.retain_color_sampling = false;
    image_options.jpeg_num_progressive_scans = options.image_jpeg_num_progressive_scans();

    let low_bytes = if options.use_blank_image_for_inline_preview() {
        let (width, height) = cached
            .image_file_dims
            .map(|(width, height)| (width, height))
            .unwrap_or((1, 1));
        match ps_image::frame::generate_blank_image(width, height, true) {
            Ok(bytes) => bytes,
            Err(_) => return,
        }
    } else {
        working_bytes.to_vec()
    };
    let mut budget = ps_image::image::ConversionBudget::new();
    let recorder = StatsRecorder {
        server: driver.server_context().clone(),
    };
    let transcoded = ps_image::image::transcode(
        &low_bytes,
        None,
        ps_image::image::TranscodeContext::default(),
        &image_options,
        &mut budget,
        true,
        &recorder,
    );
    let (low_resolved, low_type) = match transcoded {
        Ok(transcoded) => (transcoded.bytes, transcoded.image_type),
        Err(_) => (low_bytes, ps_image::format::sniff(working_bytes)),
    };
    let low_res_is_small = options.max_low_res_image_size_bytes() < 0
        || low_resolved.len() as i64 <= options.max_low_res_image_size_bytes();
    let low_res_smaller_than_full_res = low_resolved.len() * 100
        < working_bytes.len() * options.max_low_res_to_full_res_image_size_percentage() as usize;
    if !(low_res_is_small && low_res_smaller_than_full_res) {
        return;
    }
    // NOTE: mobile low-res downscaling (`ResizeLowQualityImage`)
    // waits for the delay-images filter integration.
    if low_type == ImageType::Unknown {
        return;
    }
    cached.low_resolution_inlined_data = Some(low_resolved);
    cached.low_resolution_inlined_mime = image_type_to_content_type(low_type)
        .map(|content_type| content_type.mime_type().to_string());
}

/// Explains an inline outcome (`MessageForInlineResult`).
fn message_for_inline_result(inline_result: InlineResult) -> &'static str {
    match inline_result {
        InlineResult::Success | InlineResult::Responsive => "",
        InlineResult::UnsupportedDevice => {
            "The image was not inlined because device does not support inlinling."
        }
        InlineResult::NotCritical => {
            "The image was not inlined because you have chosen to only inline the critical images but this image is not critical."
        }
        InlineResult::NoData | InlineResult::TooLarge => {
            "The image was not inlined because it has too many bytes."
        }
        InlineResult::CacheSmallImagesUnrewritten => {
            "The image was not inlined because CacheSmallImagesUnrewritten has been set."
        }
        InlineResult::Shortcut => {
            "The image was not inlined because it is a shortcut icon."
        }
        InlineResult::InternalError => {
            "The image was not inlined because the internal data was corrupted."
        }
    }
}

/// Attempts inlining (`TryInline`).
fn try_inline(
    driver: &Rc<ResourceDriver>,
    is_html: bool,
    is_critical: bool,
    image_inline_max_bytes: i64,
    cached: &CachedResult,
) -> (InlineResult, Option<String>) {
    // An unset type reads UNKNOWN, which is valid; only a corrupt
    // (unparseable) MIME fails here.
    if let Some(mime) = cached.inlined_image_mime.as_deref() {
        if mime_to_content_type(mime).is_none() {
            return (InlineResult::InternalError, None);
        }
    }
    let mut properties = driver.request_properties();
    let is_webp = cached.inlined_image_mime.as_deref() == Some("image/webp");
    if !properties.supports_image_inlining() || (is_webp && properties.forbid_webp_inlining()) {
        return (InlineResult::UnsupportedDevice, None);
    }
    if is_html && driver.options().inline_only_critical_images() && !is_critical {
        return (InlineResult::NotCritical, None);
    }
    let Some(data) = cached.inlined_data.as_deref() else {
        return (InlineResult::NoData, None);
    };
    if data.len() as i64 >= image_inline_max_bytes {
        return (InlineResult::TooLarge, None);
    }
    // This is the decision point for whether or not an image is
    // suitable for inlining. After this point, we may skip inlining
    // an image, but not because of properties of the image.
    if driver.options().cache_small_images_unrewritten() {
        // NOTE: property-cache URL storage waits for that subsystem.
        return (InlineResult::CacheSmallImagesUnrewritten, None);
    }
    let Some(mime) = cached.inlined_image_mime.as_deref() else {
        return (InlineResult::InternalError, None);
    };
    let content_type = mime_to_content_type(mime).expect("inlined mime");
    let data_url =
        ps_http::data_url::make_data_url(content_type, ps_http::data_url::Encoding::Base64, data);
    (
        InlineResult::Success,
        Some(String::from_utf8_lossy(&data_url).into_owned()),
    )
}

/// Renders one CSS image (`FinishRewriteCssImageUrl`): inlines into
/// the value when small enough, else counts the use and falls back
/// to nested (slot) rendering, which also trims the URL.
fn finish_rewrite_css_image_url(
    driver: &Rc<ResourceDriver>,
    css_image_inline_max_bytes: i64,
    cached: &CachedResult,
    slot: &ContextSlot,
    inline_result: &mut InlineResult,
) -> bool {
    let (result, data_url) = try_inline(driver, false, false, css_image_inline_max_bytes, cached);
    *inline_result = result;
    if result == InlineResult::Success {
        debug_assert!(!driver.options().cache_small_images_unrewritten());
        if let Some(data_url) = data_url {
            let written = match slot {
                ContextSlot::Css(slot) => slot.borrow().direct_set_url(&data_url),
                ContextSlot::Assoc(slot) => slot.borrow().direct_set_url(&data_url),
                _ => false,
            };
            if written {
                driver.server_context().variable(IMAGE_INLINE).add(1);
                return true;
            }
        }
    } else if cached.optimizable {
        driver.server_context().variable(IMAGE_REWRITE_USES).add(1);
    }
    // Fall back to nested rewriting, which will also left trim the
    // url if that is required.
    false
}

/// Renders one HTML image (`FinishRewriteImageUrl`).
#[allow(clippy::too_many_arguments)]
fn finish_rewrite_image_url(
    driver: &Rc<ResourceDriver>,
    behavior: &ImageRewriteBehavior,
    cached: &CachedResult,
    dom: &mut Dom,
    base: &GoogleUrl,
    options: &ps_config::options::RewriteOptions,
    slot: &std::cell::RefCell<HtmlResourceSlot>,
    inline_result: &mut InlineResult,
) -> bool {
    let (element, attribute) = {
        let slot = slot.borrow();
        match (slot.element(), Some(slot.attribute())) {
            (Some(element), Some(attribute)) => (element, attribute),
            _ => return false,
        }
    };
    let (decoded, _) = dom.decoded_attribute(element, attribute);
    let src_value = decoded.unwrap_or_default();
    if src_value.is_empty() {
        return false;
    }
    let mut rewrote_url = false;
    let mut image_inlined = false;
    // Default to all images being critical without meaningful
    // critical-image information (no beacon subsystem here).
    let is_critical_image = true;

    let keyword = dom.element_keyword(element);
    if keyword == Keyword::Link {
        // Don't inline shortcut images. All shortcut images are on
        // link tags, and no non-shortcut images are on link tags.
        *inline_result = InlineResult::Shortcut;
    } else {
        // See if we have a data URL, and if so use it if the browser
        // can handle it.
        let (result, data_url) = try_inline(
            driver,
            true,
            is_critical_image,
            options.image_inline_max_bytes(),
            cached,
        );
        *inline_result = result;
        if result == InlineResult::Success {
            if let Some(data_url) = data_url {
                // Note the use of the ORIGINAL url not the data
                // url. AddLscAttributes uses image dimensions,
                // so must be called before DeleteMatchingImageDims.
                let (original, _) = dom.decoded_attribute(element, attribute);
                let original = original.unwrap_or_default();
                super::local_storage_cache::LocalStorageCacheFilter::add_lsc_attributes(
                    &original, cached, driver, dom, element,
                );
                dom.element_attributes_mut(element)[attribute].set_value(data_url.as_bytes());
                delete_matching_image_dims_after_inline(cached, dom, element);
                driver.server_context().variable(IMAGE_INLINE).add(1);
                rewrote_url = true;
                image_inlined = true;
            }
        }
    }

    // Rewrite URL in case this image was not inlined (and URL
    // rewriting allowed).
    if !image_inlined && !slot.borrow().base().preserve_urls() {
        // Not inlined means we cannot store it in local storage.
        super::local_storage_cache::LocalStorageCacheFilter::remove_lsc_attributes(
            driver, dom, element,
        );
        if cached.optimizable {
            // Rewritten HTTP url
            let rewritten = relativize_or_passthrough(
                options,
                &cached.url,
                slot.borrow().url_relativity(),
                base,
            );
            dom.element_attributes_mut(element)[attribute].set_value(rewritten.as_bytes());
            driver.server_context().variable(IMAGE_REWRITE_USES).add(1);
            rewrote_url = true;
        }
        if options.enabled(Filter::InsertImageDimensions)
            && (keyword == Keyword::Img || keyword == Keyword::Input)
            && !has_any_dimensions(driver, dom, element)
        {
            if let Some((width, height)) = cached.image_file_dims {
                // Add image dimensions. We don't bother to resize if
                // either dimension is specified with units (em, %)
                // rather than as absolute pixels. But note that we DO
                // attempt to include image dimensions even if we
                // otherwise choose not to optimize an image.
                add_attribute(driver, dom, element, b"width", &width.to_string());
                add_attribute(driver, dom, element, b"height", &height.to_string());
            }
        }
    }

    // NOTE: low-res preview insertion waits for the delay-images
    // filter integration; critical-images finder data is unread.
    let _ = behavior;
    // NOTE: rewrite-activity logging waits for the log record.
    rewrote_url
}

/// Deletes width/height matching the file dims after an inline.
fn delete_matching_image_dims_after_inline(cached: &CachedResult, dom: &mut Dom, element: NodeId) {
    // Never strip width= or height= attributes from non-img elements.
    if dom.element_keyword(element) != Keyword::Img {
        return;
    }
    // We go back to the html to determine whether it's safe to strip
    // the width and height attributes, doing so only if all
    // dimensions that are present match the actual post-optimization
    // image dimensions.
    if let Some((file_width, file_height)) = cached.image_file_dims {
        let width_index = dom.find_attribute(element, Keyword::Width);
        let height_index = dom.find_attribute(element, Keyword::Height);
        let width_value = width_index.and_then(|index| {
            let (decoded, _) = dom.decoded_attribute(element, index);
            decoded.and_then(|value| parse_dimension_attribute(&value))
        });
        let height_value = height_index.and_then(|index| {
            let (decoded, _) = dom.decoded_attribute(element, index);
            decoded.and_then(|value| parse_dimension_attribute(&value))
        });
        match (width_value, height_value) {
            (Some(width), _) if width as u32 == file_width => {
                // Width matches, height must either be absent or match.
                match height_value {
                    None if height_index.is_none() => {
                        // No height, just delete width.
                        dom.delete_attribute(element, Keyword::Width);
                    }
                    Some(height) if height as u32 == file_height => {
                        // Both dimensions match, delete both.
                        dom.delete_attribute(element, Keyword::Width);
                        dom.delete_attribute(element, Keyword::Height);
                    }
                    _ => {}
                }
            }
            (None, Some(height)) if width_index.is_none() && height as u32 == file_height => {
                // No width, matching height
                dom.delete_attribute(element, Keyword::Height);
            }
            _ => {}
        }
    }
}

/// Whether the element names any dimensions (`HasAnyDimensions`).
fn has_any_dimensions(driver: &Rc<ResourceDriver>, dom: &mut Dom, element: NodeId) -> bool {
    if dom.find_attribute(element, Keyword::Width).is_some() {
        return true;
    }
    if dom.find_attribute(element, Keyword::Height).is_some() {
        return true;
    }
    let (state, _, _) = style_dimensions(driver, dom, element, false);
    !matches!(state, ps_css::util::DimensionState::NoDimensions)
}

/// Extracts style-attribute dimensions with the parse bound.
fn style_dimensions(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    element: NodeId,
    count_over_bound: bool,
) -> (ps_css::util::DimensionState, i32, i32) {
    use ps_css::util::DimensionState;
    let Some(style_index) = dom.find_attribute(element, Keyword::Style) else {
        return (DimensionState::NoDimensions, -1, -1);
    };
    let (decoded, _) = dom.decoded_attribute(element, style_index);
    let Some(style) = decoded else {
        return (DimensionState::NoDimensions, -1, -1);
    };
    // A style attribute over CssParseMaxBytes is not parsed here either.
    if count_over_bound
        && crate::filters::css::over_parse_max_bytes(
            driver,
            style.len() as i64,
            driver.base_url().spec(),
            "Image style attribute not read",
        )
        .is_some()
    {
        return (DimensionState::NotParsable, -1, -1);
    }
    if style.len() as i64 > driver.options().css_parse_max_bytes()
        && driver.options().css_parse_max_bytes() >= 0
    {
        return (DimensionState::NotParsable, -1, -1);
    }
    let mut parser = ps_css::parser::Parser::new(&style);
    let mut decls = parser.parse_raw_declarations();
    let expanded = ps_css::parser::Parser::expand_declarations(&mut decls);
    ps_css::util::get_dimensions(&expanded)
}

/// Adds an attribute (`driver()->AddAttribute`).
fn add_attribute(
    _driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    element: NodeId,
    name: &[u8],
    value: &str,
) {
    dom.add_attribute(element, name, value.as_bytes());
}

/// Collects page dimensions for resizing (`GetDimensions`).
fn get_dimensions(
    driver: &Rc<ResourceDriver>,
    dom: &mut Dom,
    element: NodeId,
    page_dim: &mut ImageDim,
) {
    use ps_css::util::DimensionState;
    let (state, width, height) = style_dimensions(driver, dom, element, true);
    // NOTE: rendered-dimension lookup waits for the
    // critical-images/property-cache subsystem.
    match state {
        DimensionState::NotParsable => {}
        DimensionState::HasBothDimensions => {
            page_dim.width = Some(width as u32);
            page_dim.height = Some(height as u32);
        }
        DimensionState::HasHeightOnly => {
            page_dim.height = Some(height as u32);
            set_width_from_attribute(dom, element, page_dim);
        }
        DimensionState::HasWidthOnly => {
            page_dim.width = Some(width as u32);
            set_height_from_attribute(dom, element, page_dim);
        }
        DimensionState::NoDimensions => {
            set_width_from_attribute(dom, element, page_dim);
            set_height_from_attribute(dom, element, page_dim);
        }
    }
}

/// Sets width from the attribute, when valid.
fn set_width_from_attribute(dom: &mut Dom, element: NodeId, page_dim: &mut ImageDim) {
    if let Some(index) = dom.find_attribute(element, Keyword::Width) {
        let (decoded, _) = dom.decoded_attribute(element, index);
        if let Some(value) = decoded.and_then(|value| parse_dimension_attribute(&value)) {
            page_dim.width = Some(value as u32);
        }
    }
}

/// Sets height from the attribute, when valid.
fn set_height_from_attribute(dom: &mut Dom, element: NodeId, page_dim: &mut ImageDim) {
    if let Some(index) = dom.find_attribute(element, Keyword::Height) {
        let (decoded, _) = dom.decoded_attribute(element, index);
        if let Some(value) = decoded.and_then(|value| parse_dimension_attribute(&value)) {
            page_dim.height = Some(height_as_u32(value));
        }
    }
}

/// Saturating i32→u32 for parsed dimensions.
fn height_as_u32(value: i32) -> u32 {
    value.max(0) as u32
}

/// Debug-message URL (`UrlForDebugMessages`): domain-mapped when
/// `RewriteDomains` adjusts page URLs too.
fn url_for_debug_messages(driver: &Rc<ResourceDriver>, url: &str) -> String {
    let gurl = GoogleUrl::new(url);
    if !gurl.is_web_valid() {
        return url.to_string();
    }
    // If we're adjusting all the URLs based on domain lawyer, we should do so
    // with comments we add. If unoptimized URLs are left as is, we can do so
    // with debug comments as well.
    if !driver.options().enabled(Filter::RewriteDomains) {
        return url.to_string();
    }
    let options = driver.options();
    let namer = driver.server_context().url_namer();
    let (result, mapped) = crate::filters::domain_rewrite::rewrite_url(
        driver.lawyer(),
        &options,
        &**namer,
        1,
        driver.signature_length(),
        url.as_bytes(),
        &gurl,
        true,
        true,
    );
    if result == crate::filters::domain_rewrite::RewriteResult::RewroteDomain {
        String::from_utf8_lossy(&mapped).into_owned()
    } else {
        url.to_string()
    }
}

/// Encodes UA state into a resource context
/// (`EncodeUserAgentIntoResourceContext`).
pub fn encode_user_agent_into_resource_context(
    driver: &Rc<ResourceDriver>,
    context: &mut ImageResourceContext,
) {
    ImageUrlEncoder::set_webp_and_mobile_user_agent(driver, context);
    // NOTE: `CssUrlEncoder::SetInliningImages` only feeds CSS URL
    // encoding, not image decisions.
    ImageUrlEncoder::set_small_screen(driver, context);
    context.may_use_save_data_quality =
        driver.options().support_save_data() && driver.request_properties().requests_save_data();
}

/// Image-rewriting filter (`ImageRewriteFilter`).
pub struct ImageRewriteFilter {
    driver: Rc<ResourceDriver>,
    image_counter: i32,
    noscript_depth: u32,
    base_url: Option<GoogleUrl>,
}

impl ImageRewriteFilter {
    /// Builds the filter over a driver.
    #[must_use]
    pub fn new(driver: Rc<ResourceDriver>) -> Self {
        ImageRewriteFilter {
            driver,
            image_counter: 0,
            noscript_depth: 0,
            base_url: None,
        }
    }

    /// Effective base URL (first `<base href>` wins, else the page).
    fn effective_base_url(&self) -> GoogleUrl {
        self.base_url
            .clone()
            .unwrap_or_else(|| self.driver.base_url().clone())
    }

    /// Computes URL preservation (`ComputePreserveUrls`).
    fn compute_preserve_urls(
        options: &ps_config::options::RewriteOptions,
        slot: &std::cell::RefCell<HtmlResourceSlot>,
    ) {
        if options.image_preserve_urls()
            && !options.enabled(Filter::ResizeImages)
            && !options.enabled(Filter::ResizeToRenderedImageDimensions)
            && !options.enabled(Filter::InlineImages)
        {
            slot.borrow_mut().base_mut().set_preserve_urls(true);
        }
    }

    /// Starts one image-URL rewrite (`BeginRewriteImageUrl`).
    fn begin_rewrite_image_url(&mut self, dom: &mut Dom, element: NodeId, attribute: usize) {
        let options = self.driver.options().clone();
        let mut resource_context = ImageResourceContext::default();
        // In case of image_preserve_urls we do not want to use image
        // dimension information from HTML/CSS.
        if options.enabled(Filter::ResizeImages)
            || options.enabled(Filter::ResizeToRenderedImageDimensions)
        {
            let mut desired_dim = ImageDim::default();
            get_dimensions(&self.driver, dom, element, &mut desired_dim);
            if desired_dim.width == Some(0)
                || desired_dim.height == Some(0)
                || (desired_dim.width == Some(1) && desired_dim.height == Some(1))
            {
                // This is either a beacon image, or an attempt to
                // prefetch. Drop the desired dimensions so that the
                // image is not resized.
                desired_dim = ImageDim::default();
            }
            resource_context.desired_image_dims = desired_dim;
        }
        encode_user_agent_into_resource_context(&self.driver, &mut resource_context);

        let (decoded, _) = dom.decoded_attribute(element, attribute);
        let Some(href) = decoded else {
            return;
        };
        let base_url = self.effective_base_url();
        let url = GoogleUrl::resolve(&base_url, &String::from_utf8_lossy(&href));
        if !url.is_web_valid() {
            return;
        }
        let (resource, authorized) = self.driver.create_input_resource(
            &url,
            crate::server_context::InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
            crate::server_context::IntendedFor::General,
            InputRole::Img,
        );
        let Some(input) = resource else {
            if !authorized {
                self.driver.insert_unauthorized_domain_debug_comment(
                    dom,
                    &String::from_utf8_lossy(&href),
                    InputRole::Img,
                    element,
                );
            }
            return;
        };

        // If the image will be inlined and the local storage cache is
        // enabled, add the LSC marker attribute to this element so that
        // the LSC filter knows to insert the relevant javascript
        // functions.
        if self.driver.request_properties().supports_image_inlining() {
            let mut state = super::local_storage_cache::InlineState::new();
            super::local_storage_cache::LocalStorageCacheFilter::add_storable_resource(
                &href,
                &self.driver,
                true,
                dom,
                element,
                &mut state,
            );
        }

        let behavior = ImageRewriteBehavior::new(
            self.driver.clone(),
            resource_context,
            RewritePlace::HtmlAttr,
            self.image_counter,
            self.noscript_depth > 0,
            0,
            false,
            base_url,
        );
        self.image_counter += 1;
        let handle: Rc<dyn Resource> = Rc::new(input.clone());
        let slot = self
            .driver
            .get_slot(Some(handle), dom, element, attribute, FILTER_ID);
        let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
        context.add_slot(slot.clone(), input);
        Self::compute_preserve_urls(&options, &slot);
        context.start(dom);
    }

    /// Starts srcset-candidate rewrites (`BeginRewriteSrcSet`).
    fn begin_rewrite_srcset(&mut self, dom: &mut Dom, element: NodeId, attribute: usize) {
        use crate::srcset::{SrcSetCollection, SrcSetSlot};
        let (decoded, _) = dom.decoded_attribute(element, attribute);
        let Some(value) = decoded else {
            return;
        };
        let collection = Rc::new(std::cell::RefCell::new(SrcSetCollection::new(
            element, attribute, &value,
        )));
        let count = collection
            .borrow()
            .candidates()
            .len()
            .min(crate::srcset::MAX_SLOTS);
        let base_url = self.effective_base_url();
        for index in 0..count {
            let url_text = collection.borrow().candidates()[index].url.clone();
            if url_text.is_empty() {
                continue;
            }
            let url = GoogleUrl::resolve(&base_url, &url_text);
            if !url.is_web_valid() {
                continue;
            }
            let mut resource_context = ImageResourceContext::default();
            encode_user_agent_into_resource_context(&self.driver, &mut resource_context);
            let (resource, _authorized) = self.driver.create_input_resource(
                &url,
                crate::server_context::InlineAuthorizationPolicy::InlineOnlyAuthorizedResources,
                crate::server_context::IntendedFor::General,
                InputRole::Img,
            );
            // Null slots (unparseable or disallowed URLs) are skipped.
            let Some(input) = resource else {
                continue;
            };
            let behavior = ImageRewriteBehavior::new(
                self.driver.clone(),
                resource_context,
                RewritePlace::Srcset,
                self.image_counter,
                self.noscript_depth > 0,
                0,
                false,
                base_url.clone(),
            );
            self.image_counter += 1;
            let handle: Rc<dyn Resource> = Rc::new(input.clone());
            let slot = Rc::new(std::cell::RefCell::new(SrcSetSlot::new(
                handle,
                collection.clone(),
                index,
                dom,
                element,
                FILTER_ID,
            )));
            let mut context = SingleRewriteContext::new(self.driver.clone(), behavior);
            context.add_srcset_slot(slot, input);
            context.start(dom);
        }
        collection.borrow_mut().commit(dom);
    }
}

impl HtmlFilter for ImageRewriteFilter {
    fn name(&self) -> &'static str {
        "ImageRewrite"
    }

    fn start_document(&mut self, _dom: &mut Dom, _lexer: &Lexer) {
        self.image_counter = 0;
        self.noscript_depth = 0;
        self.base_url = None;
    }

    fn start_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript {
            self.noscript_depth += 1;
        }
        // First <base href> wins (`SetBaseUrlIfUnset` parity).
        if dom.element_keyword(id) == Keyword::Base && self.base_url.is_none() {
            if let Some(index) = dom.find_attribute(id, Keyword::Href) {
                let (decoded, _) = dom.decoded_attribute(id, index);
                if let Some(href) = decoded {
                    let base =
                        GoogleUrl::resolve(self.driver.base_url(), &String::from_utf8_lossy(&href));
                    if base.is_web_valid() {
                        self.base_url = Some(base);
                    }
                }
            }
        }
        // Don't rewrite if there is a pagespeed_no_transform or
        // data-pagespeed-no-transform attribute.
        if dom
            .find_attribute(id, Keyword::DataPagespeedNoTransform)
            .is_some()
        {
            // Remove the attribute
            dom.delete_attribute(id, Keyword::DataPagespeedNoTransform);
            return;
        }
        if dom
            .find_attribute(id, Keyword::PagespeedNoTransform)
            .is_some()
        {
            // Remove the attribute
            dom.delete_attribute(id, Keyword::PagespeedNoTransform);
            return;
        }
        // Rewrite any image-valued attributes we find.
        let url_valued = self.driver.options().url_valued_attributes().to_vec();
        let mut image_attrs = Vec::new();
        for (index, category) in crate::resource_tag_scanner::scan_element(dom, id, &url_valued) {
            if category == ps_config::options::Category::Image {
                let (decoded, _) = dom.decoded_attribute(id, index);
                let Some(decoded) = decoded else {
                    continue;
                };
                // The LSC filter only knows how to handle the src
                // attribute. A return value of true means we don't
                // have to rewrite it so can skip that.
                if dom.element_attributes(id)[index].keyword() == Keyword::Src {
                    let mut state = super::local_storage_cache::InlineState::new();
                    if super::local_storage_cache::LocalStorageCacheFilter::add_storable_resource(
                        &decoded,
                        &self.driver,
                        false,
                        dom,
                        id,
                        &mut state,
                    ) {
                        continue;
                    }
                }
                image_attrs.push(index);
            }
        }
        for index in image_attrs {
            self.begin_rewrite_image_url(dom, id, index);
        }
        if dom.element_keyword(id) == Keyword::Img {
            if let Some(srcset) = dom.find_attribute(id, Keyword::Srcset) {
                self.begin_rewrite_srcset(dom, id, srcset);
            }
        }
    }

    fn end_element(&mut self, dom: &mut Dom, _lexer: &Lexer, id: NodeId) {
        if dom.element_keyword(id) == Keyword::Noscript && self.noscript_depth > 0 {
            self.noscript_depth -= 1;
        }
    }
}

impl crate::rewrite_filter::RewriteFilter for ImageRewriteFilter {
    fn id(&self) -> &'static str {
        FILTER_ID
    }

    fn rewrite_in_place(
        &self,
        driver: &Rc<ResourceDriver>,
        input: &[u8],
        content_type: &ps_http::content_type::ContentType,
    ) -> Option<crate::rewrite_filter::InPlaceOutput> {
        // IPRO origin serve: the same transcode core as the
        // `.pagespeed.` path, without resize. UA-derived
        // formats (webp/avif) need `InPlaceOptimizeForBrowser`
        // (bandwidth set): without it the context stays
        // default, like C++'s early return in
        // `EncodeUserAgentIntoResourceContext`. Larger
        // outputs fail back to the original.
        let analysis = ps_image::image::analyze(input).ok()?;
        if analysis.width < 1 || analysis.height < 1 {
            return None;
        }
        let mut resource_context = ImageResourceContext::default();
        if crate::ipro::in_place_optimize_for_browser_enabled(driver) {
            encode_user_agent_into_resource_context(driver, &mut resource_context);
        }
        let options = driver.options().clone();
        let image_options =
            image_options_for_loaded_resource(&resource_context, input.len() as i64, &options);
        let ctx = ps_image::image::TranscodeContext {
            resized: false,
            resized_dims: None,
            original_type: Some(ps_image::format::sniff(input)),
        };
        let mut budget = ps_image::image::ConversionBudget::new();
        let recorder = StatsRecorder {
            server: driver.server_context().clone(),
        };
        let transcoded = ps_image::image::transcode(
            input,
            Some((analysis.width, analysis.height)),
            ctx,
            &image_options,
            &mut budget,
            false,
            &recorder,
        )
        .ok()?;
        if transcoded.bytes.len() >= input.len() {
            return None;
        }
        let content_type =
            image_type_to_content_type(transcoded.image_type).unwrap_or(*content_type);
        Some(crate::rewrite_filter::InPlaceOutput {
            bytes: transcoded.bytes,
            content_type,
            optimized_image_type: Some(transcoded.image_type),
        })
    }

    fn encoder(&self) -> &'static dyn UrlSegmentEncoder {
        &IMAGE_ENCODER
    }

    fn fetch(
        &self,
        driver: &Rc<ResourceDriver>,
        output: crate::output_resource::OutputResource,
        fetch: &mut dyn ps_http::fetch::AsyncFetch,
    ) -> bool {
        // Decode the name for dimensions, then rebuild the UA context
        // around them (fetch URLs carry no UA state of their own).
        let name = output.full_name().name().to_string();
        let mut resource_context = ImageResourceContext::default();
        encode_user_agent_into_resource_context(driver, &mut resource_context);
        if ImageUrlEncoder
            .decode(name.as_bytes(), &mut resource_context)
            .is_none()
        {
            return false;
        }
        if driver.options().serve_rewritten_webp_urls_to_any_agent()
            && ImageUrlEncoder::is_webp_rewritten_url(&GoogleUrl::new(output.url()))
        {
            // See https://developers.google.com/speed/webp/faq :
            // the latest versions of all browsers that support webp,
            // support webp lossless as well.
            resource_context.libwebp_level = LibWebpLevel::LossyLosslessAlpha;
        }
        let base_url = driver.base_url().clone();
        let behavior = ImageRewriteBehavior::new(
            driver.clone(),
            resource_context,
            RewritePlace::Fetch,
            0,
            false,
            0,
            false,
            base_url,
        );
        let mut context = SingleRewriteContext::new(driver.clone(), behavior);
        context.fetch(output, fetch)
    }
}
