//! Image URL encoding (`ImageUrlEncoder`).
//!
//! Prefixes rewritten image names with desired dimensions
//! (`<width>x<height>x<url>`, `N` for a missing side) and decodes
//! them back, including legacy webp/mobile terminator codes.

use ps_http::google_url::GoogleUrl;

/// Dimension separator and terminator (`kCodeSeparator`).
const CODE_SEPARATOR: u8 = b'x';
/// Legacy lossy-webp terminator (`kCodeWebpLossy`).
const CODE_WEBP_LOSSY: u8 = b'w';
/// Legacy lossy+alpha-webp terminator
/// (`kCodeWebpLossyLosslessAlpha`).
const CODE_WEBP_LOSSY_LOSSLESS_ALPHA: u8 = b'v';
/// Legacy mobile-UA marker (`kCodeMobileUserAgent`).
const CODE_MOBILE_USER_AGENT: u8 = b'm';
/// Missing-dimension marker (`kMissingDimension`).
const MISSING_DIMENSION: u8 = b'N';

/// Desired image dimensions (`ImageDim`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImageDim {
    /// Desired width, when set.
    pub width: Option<u32>,
    /// Desired height, when set.
    pub height: Option<u32>,
}

/// Webp capability level (`ResourceContext::LibWebpLevel`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibWebpLevel {
    /// No webp.
    #[default]
    None,
    /// Lossy only.
    LossyOnly,
    /// Lossy with lossless alpha.
    LossyLosslessAlpha,
    /// Animated webp.
    Animated,
}

/// Image request context (`ResourceContext`, image subset).
#[derive(Debug, Clone, Default)]
pub struct ImageResourceContext {
    /// Desired dimensions.
    pub desired_image_dims: ImageDim,
    /// Webp capability level.
    pub libwebp_level: LibWebpLevel,
    /// Mobile user agent.
    pub mobile_user_agent: bool,
    /// AVIF-capable user agent.
    pub supports_avif: bool,
    /// Save-Data quality allowed.
    pub may_use_save_data_quality: bool,
    /// Small-screen quality allowed.
    pub may_use_small_screen_quality: bool,
}

/// Whether the context names a dimension (`HasDimension`).
#[must_use]
pub fn has_dimension(context: &ImageResourceContext) -> bool {
    let dims = &context.desired_image_dims;
    dims.width.is_some() || dims.height.is_some()
}

/// Whether either dimension is set (`HasValidDimension`).
#[must_use]
pub fn has_valid_dimension(dims: &ImageDim) -> bool {
    dims.width.is_some() || dims.height.is_some()
}

/// Whether both dimensions are set (`HasValidDimensions`).
#[must_use]
pub fn has_valid_dimensions(dims: &ImageDim) -> bool {
    dims.width.is_some() && dims.height.is_some()
}

/// Image name codec (`ImageUrlEncoder`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageUrlEncoder;

impl ImageUrlEncoder {
    /// Encodes one URL with its dimensions (`Encode`).
    #[must_use]
    pub fn encode(&self, url: &[u8], context: &ImageResourceContext) -> Vec<u8> {
        let mut out = Vec::new();
        if has_dimension(context) {
            let dims = &context.desired_image_dims;
            match dims.width {
                Some(width) => out.extend_from_slice(width.to_string().as_bytes()),
                None => out.push(MISSING_DIMENSION),
            }
            out.push(CODE_SEPARATOR);
            match dims.height {
                Some(height) => out.extend_from_slice(height.to_string().as_bytes()),
                None => out.push(MISSING_DIMENSION),
            }
        }
        out.push(CODE_SEPARATOR);
        out.extend_from_slice(ps_cache::namer::encode_url_segment_bytes(url).as_bytes());
        out
    }

    /// Decodes a name into its URL plus context (`Decode`).
    pub fn decode(&self, encoded: &[u8], context: &mut ImageResourceContext) -> Option<Vec<u8>> {
        if encoded.is_empty() {
            return None;
        }
        let mut remaining = encoded;
        let mut terminator = remaining[0];
        if is_valid_code(terminator) {
            // No dimensions.  x... or w... or mx... or mw...
            // Do nothing.
        } else if decode_image_dimensions(&mut remaining, &mut context.desired_image_dims) {
            // We've parsed the dimensions and they've been stripped
            // from remaining. Now set terminator properly.
            terminator = remaining[0];
        } else {
            return None;
        }
        // Remove the terminator
        remaining = &remaining[1..];

        // Set mobile user agent & set webp only if its a legacy encoding.
        if terminator == CODE_MOBILE_USER_AGENT {
            context.mobile_user_agent = true;
            // There must be a final lossy, lossless-alpha, or plain
            // separator. Otherwise, invalid. Check and strip it.
            if remaining.is_empty() {
                return None;
            }
            terminator = remaining[0];
            if terminator != CODE_WEBP_LOSSY
                && terminator != CODE_WEBP_LOSSY_LOSSLESS_ALPHA
                && terminator != CODE_SEPARATOR
            {
                return None;
            }
            remaining = &remaining[1..];
        }
        // Following terminator check is for Legacy Url Encoding.
        // If it's a legacy "x" encoding, we don't overwrite the
        // libwebp_level. In case it doesn't hit either of the
        // following two conditions, the libwebp level is taken as
        // the one set previously (non-legacy URLs).
        if terminator == CODE_WEBP_LOSSY {
            context.libwebp_level = LibWebpLevel::LossyOnly;
        } else if terminator == CODE_WEBP_LOSSY_LOSSLESS_ALPHA {
            context.libwebp_level = LibWebpLevel::LossyLosslessAlpha;
        }

        let text = std::str::from_utf8(remaining).ok()?;
        ps_cache::namer::decode_url_segment_bytes(text)
    }

    /// UA cache-key fragment for a context
    /// (`CacheKeyFromResourceContext`).
    #[must_use]
    pub fn cache_key_from_resource_context(context: &ImageResourceContext) -> String {
        let mut key = String::new();
        match context.libwebp_level {
            LibWebpLevel::None => key.push('.'),
            LibWebpLevel::LossyLosslessAlpha => key.push('v'),
            LibWebpLevel::LossyOnly => key.push('w'),
            LibWebpLevel::Animated => key.push('a'),
        }
        if context.supports_avif {
            key.push_str(".avif");
        }
        if context.mobile_user_agent {
            key.push('m');
        }
        // If the image will be compressed to a quality different than
        // the regular one, add a key to cache. The quality for
        // Save-Data has higher precedence than that for mobile, so
        // does the key.
        if context.may_use_save_data_quality {
            key.push('d');
        } else if context.may_use_small_screen_quality {
            key.push_str("ss");
        }
        key
    }

    /// Whether the URL is a webp image rewrite (`IsWebpRewrittenUrl`).
    #[must_use]
    pub fn is_webp_rewritten_url(gurl: &GoogleUrl) -> bool {
        let mut namer = ps_cache::namer::ResourceNamer::new();
        if !namer.decode_ignore_hash_and_signature(&gurl.leaf_sans_query()) {
            return false;
        }
        // We only convert images to WebP whose URLs were created by
        // ImageRewriteFilter, whose ID is "ic".
        if namer.id() != "ic" {
            return false;
        }
        namer.ext() == "webp"
    }

    /// Sets the webp level from options + capabilities
    /// (`SetLibWebpLevel`).
    pub fn set_libwebp_level(
        options: &ps_config::options::RewriteOptions,
        properties: &mut crate::request_properties::RequestProperties,
        context: &mut ImageResourceContext,
    ) {
        use ps_config::filter::Filter;
        let mut level = LibWebpLevel::None;
        // We do enabled checks before Setting the Webp Level, since
        // it avoids writing two metadata cache keys for same output
        // if webp rewriting is disabled.
        if properties.supports_webp_animated()
            && (options.enabled(Filter::RecompressWebp)
                || options.enabled(Filter::ConvertToWebpAnimated))
        {
            level = LibWebpLevel::Animated;
        } else if properties.supports_webp_lossless_alpha()
            && (options.enabled(Filter::RecompressWebp)
                || options.enabled(Filter::ConvertToWebpLossless))
        {
            level = LibWebpLevel::LossyLosslessAlpha;
        } else if properties.supports_webp_rewritten_urls()
            && (options.enabled(Filter::RecompressWebp)
                || options.enabled(Filter::ConvertToWebpLossless)
                || options.enabled(Filter::ConvertJpegToWebp))
        {
            level = LibWebpLevel::LossyOnly;
        }
        context.libwebp_level = level;
    }

    /// Sets webp/mobile UA state (`SetWebpAndMobileUserAgent`).
    pub fn set_webp_and_mobile_user_agent(
        driver: &crate::server_context::ResourceDriver,
        context: &mut ImageResourceContext,
    ) {
        use ps_config::filter::Filter;
        let options = driver.options();
        // NOTE: the serve-rewritten-webp-to-any-agent branch keys off
        // the fetch URL/base, which only exists on the fetch path;
        // filter.fetch applies it there.
        let mut properties = driver.request_properties();
        context.supports_avif =
            options.enabled(Filter::ConvertToAvif) && properties.supports_avif_rewritten_urls();
        Self::set_libwebp_level(&options, &mut properties, context);
        if options.enabled(Filter::DelayImages)
            && options.enabled(Filter::ResizeMobileImages)
            && properties.is_mobile()
        {
            context.mobile_user_agent = true;
        }
    }

    /// Sets small-screen quality state (`SetSmallScreen`).
    pub fn set_small_screen(
        driver: &crate::server_context::ResourceDriver,
        context: &mut ImageResourceContext,
    ) {
        // We used to do checking based on screen resolution, but we
        // actually care about is physically small screens even if
        // they're high-density.
        context.may_use_small_screen_quality = driver.options().has_valid_small_screen_qualities()
            && driver.request_properties().is_mobile();
    }

    /// Whether Vary: User-Agent is allowed (`AllowVaryOnUserAgent`).
    #[must_use]
    pub fn allow_vary_on_user_agent(
        options: &ps_config::options::RewriteOptions,
        properties: &crate::request_properties::RequestProperties,
    ) -> bool {
        options.allow_vary_on_user_agent()
            || (options.allow_vary_on_auto() && !properties.has_via_header())
    }

    /// Whether Vary: Accept is allowed (`AllowVaryOnAccept`).
    #[must_use]
    pub fn allow_vary_on_accept(
        options: &ps_config::options::RewriteOptions,
        properties: &crate::request_properties::RequestProperties,
    ) -> bool {
        options.allow_vary_on_accept()
            || (options.allow_vary_on_auto() && properties.has_via_header())
    }
}

/// Whether the byte is a dimension terminator (`IsValidCode`).
fn is_valid_code(code: u8) -> bool {
    code == CODE_SEPARATOR
        || code == CODE_WEBP_LOSSY
        || code == CODE_WEBP_LOSSY_LOSSLESS_ALPHA
        || code == CODE_MOBILE_USER_AGENT
}

/// Decodes one dimension, consuming it (`DecodeDimension`).
fn decode_dimension(input: &mut &[u8]) -> Option<(u32, bool)> {
    if input.len() < 2 {
        return None;
    }
    if input[0] == MISSING_DIMENSION {
        *input = &input[1..];
        return Some((0, false));
    }
    let mut result: u32 = 0;
    let mut ok = false;
    while input.len() >= 2 {
        let digit = input[0].wrapping_sub(b'0');
        if digit > 9 {
            break;
        }
        result = result.wrapping_mul(10).wrapping_add(u32::from(digit));
        *input = &input[1..];
        ok = true;
    }
    ok.then_some((result, true))
}

/// Decodes `WxH` dimensions, consuming them
/// (`DecodeImageDimensions`).
fn decode_image_dimensions(remaining: &mut &[u8], dims: &mut ImageDim) -> bool {
    if remaining.len() < 4 {
        // url too short to hold dimensions.
        return false;
    }
    let mut probe = *remaining;
    let Some((width, has_width)) = decode_dimension(&mut probe) else {
        return false;
    };
    if probe.is_empty() || probe[0] != CODE_SEPARATOR {
        return false;
    }
    // Consume the separator
    probe = &probe[1..];
    let Some((height, has_height)) = decode_dimension(&mut probe) else {
        return false;
    };
    if probe.is_empty() || !is_valid_code(probe[0]) {
        return false;
    }
    // Parsed successfully. Now store the dimensions that were present.
    if has_width {
        dims.width = Some(width);
    }
    if has_height {
        dims.height = Some(height);
    } else if !has_width {
        // Both dimensions are missing!  NxN[xw] is not allowed, as
        // it's ambiguous with the shorter encoding.  We should never
        // get here in real life.
        return false;
    }
    *remaining = probe;
    true
}
