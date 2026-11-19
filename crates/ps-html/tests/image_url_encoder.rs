//! Image-URL-encoding tests ported from `image_url_encoder_test.cc`.

use std::collections::HashSet;

use ps_html::image_url_encoder::{
    ImageDim, ImageResourceContext, ImageUrlEncoder, LibWebpLevel, has_valid_dimension,
    has_valid_dimensions,
};
use ps_http::google_url::GoogleUrl;

const DIMS_URL: &str = "17x33x,hencoded.url,_with,_various.stuff";
const NO_DIMS_URL: &str = "x,hencoded.url,_with,_various.stuff";
const ACTUAL_URL: &str = "http://encoded.url/with/various.stuff";

/// Encodes a URL with dimensions (`EncodeUrlAndDimensions`).
fn encode_url_and_dimensions(origin_url: &str, dim: &ImageDim) -> String {
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    context.desired_image_dims = *dim;
    String::from_utf8(encoder.encode(origin_url.as_bytes(), &context)).unwrap()
}

/// Decodes into dimensions plus URL (`DecodeUrlAndDimensions`).
fn decode_url_and_dimensions(encoded: &str) -> Option<(ImageDim, String)> {
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    let url = encoder.decode(encoded.as_bytes(), &mut context)?;
    let url = String::from_utf8(url).unwrap();
    Some((context.desired_image_dims, url))
}

/// Expects a dimension decode failure (`ExpectBadDim`).
fn expect_bad_dim(url: &str) {
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    assert!(
        encoder.decode(url.as_bytes(), &mut context).is_none(),
        "{url}"
    );
    assert!(!has_valid_dimension(&context.desired_image_dims));
}

fn is_pagespeed_webp(url: &str) -> bool {
    ImageUrlEncoder::is_webp_rewritten_url(&GoogleUrl::new(url))
}

#[test]
fn test_encoding_and_decoding() {
    let original_url = "a.jpg";
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    context.libwebp_level = LibWebpLevel::LossyOnly;
    context.mobile_user_agent = true;
    context.desired_image_dims = ImageDim {
        width: Some(1024),
        height: Some(768),
    };

    let encoded_url = encoder.encode(original_url.as_bytes(), &context);
    assert_eq!("1024x768xa.jpg", String::from_utf8_lossy(&encoded_url));

    let decoded_url = encoder.decode(&encoded_url, &mut context).expect("decode");

    // Check the resource context returned.
    assert_eq!(LibWebpLevel::LossyOnly, context.libwebp_level);
    assert!(context.mobile_user_agent);

    // Check the decoded url after encoding is the same as original.
    assert_eq!(original_url.as_bytes(), decoded_url.as_slice());
}

#[test]
fn test_encoding_and_decoding_without_webp_and_mobile_ua() {
    let original_url = "a.jpg";
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    context.libwebp_level = LibWebpLevel::None;
    context.mobile_user_agent = false;
    context.desired_image_dims = ImageDim {
        width: Some(1024),
        height: Some(768),
    };

    let encoded_url = encoder.encode(original_url.as_bytes(), &context);
    assert_eq!("1024x768xa.jpg", String::from_utf8_lossy(&encoded_url));

    let decoded_url = encoder.decode(&encoded_url, &mut context).expect("decode");

    // Check the resource context returned.
    assert_eq!(LibWebpLevel::None, context.libwebp_level);
    assert!(!context.mobile_user_agent);

    // Check the decoded url after encoding is the same as original.
    assert_eq!(original_url.as_bytes(), decoded_url.as_slice());
}

#[test]
fn test_legacy_mobile_webp_decoding() {
    let encoded_url = "1024x768mwa.jpg";
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    context.libwebp_level = LibWebpLevel::LossyOnly;
    let decoded_url = encoder
        .decode(encoded_url.as_bytes(), &mut context)
        .expect("decode");
    assert_eq!(LibWebpLevel::LossyOnly, context.libwebp_level);
    assert!(context.mobile_user_agent);
    assert_eq!(b"a.jpg", decoded_url.as_slice());
}

#[test]
fn test_legacy_mobile_decoding() {
    let encoded_url = "1024x768mxa.jpg";
    let encoder = ImageUrlEncoder;
    let mut context = ImageResourceContext::default();
    // Set webp lossy similar to the ImageRewriteFilter flow.
    context.libwebp_level = LibWebpLevel::LossyOnly;
    let decoded_url = encoder
        .decode(encoded_url.as_bytes(), &mut context)
        .expect("decode");
    // While decoding in ImageUrlEncoder, we don't unset libwebp_level if
    // already set and no encoding char("w"/"v") is found, we get a false
    // ResourceContext, but its alright since the UA is a webp-capable one.
    assert_eq!(LibWebpLevel::LossyOnly, context.libwebp_level);
    assert!(context.mobile_user_agent);
    assert_eq!(b"a.jpg", decoded_url.as_slice());
}

#[test]
fn no_dims_webp_or_mobile() {
    let (dim, origin_url) = decode_url_and_dimensions(NO_DIMS_URL).expect("decode");
    assert!(!has_valid_dimensions(&dim));
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(NO_DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn no_dims_webp_la() {
    let legacy = "v,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(!has_valid_dimensions(&dim));
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(NO_DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn no_dims_webp_la_mobile() {
    let legacy = "mv,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(!has_valid_dimensions(&dim));
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(NO_DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_dims() {
    let (dim, origin_url) = decode_url_and_dimensions(DIMS_URL).expect("decode");
    assert!(has_valid_dimensions(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_dims_webp() {
    let legacy = "17x33w,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimensions(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_dims_webp_la() {
    let legacy = "17x33v,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimensions(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_dims_mobile() {
    let legacy = "17x33mx,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimensions(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_dims_webp_mobile() {
    let legacy = "17x33mw,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimensions(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_dims_webp_la_mobile() {
    let legacy = "17x33mv,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimensions(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(DIMS_URL, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_width() {
    let width_url = "17xNx,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(width_url).expect("decode");
    assert!(has_valid_dimension(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(None, dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(width_url, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_width_webp() {
    let legacy = "17xNw,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimension(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(None, dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    let width_url = "17xNx,hencoded.url,_with,_various.stuff";
    assert_eq!(width_url, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_width_webp_la() {
    let legacy = "17xNv,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimension(&dim));
    assert_eq!(Some(17), dim.width);
    assert_eq!(None, dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    let width_url = "17xNx,hencoded.url,_with,_various.stuff";
    assert_eq!(width_url, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_height() {
    let height_url = "Nx33x,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(height_url).expect("decode");
    assert!(has_valid_dimension(&dim));
    assert_eq!(None, dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    assert_eq!(height_url, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_height_webp() {
    let legacy = "Nx33w,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimension(&dim));
    assert_eq!(None, dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    let height_url = "Nx33x,hencoded.url,_with,_various.stuff";
    assert_eq!(height_url, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn has_height_webp_la() {
    let legacy = "Nx33v,hencoded.url,_with,_various.stuff";
    let (dim, origin_url) = decode_url_and_dimensions(legacy).expect("decode");
    assert!(has_valid_dimension(&dim));
    assert_eq!(None, dim.width);
    assert_eq!(Some(33), dim.height);
    assert_eq!(ACTUAL_URL, origin_url);
    let height_url = "Nx33x,hencoded.url,_with,_various.stuff";
    assert_eq!(height_url, encode_url_and_dimensions(&origin_url, &dim));
}

#[test]
fn cache_key() {
    let key = ImageUrlEncoder::cache_key_from_resource_context;
    let context = ImageResourceContext::default();
    assert_eq!(".", key(&context));

    let mut context = ImageResourceContext::default();
    context.may_use_small_screen_quality = true;
    assert_eq!(".ss", key(&context));

    let mut context = ImageResourceContext::default();
    context.may_use_save_data_quality = true;
    context.libwebp_level = LibWebpLevel::LossyOnly;
    assert_eq!("wd", key(&context));

    let mut context = ImageResourceContext::default();
    context.mobile_user_agent = true;
    context.libwebp_level = LibWebpLevel::LossyLosslessAlpha;
    assert_eq!("vm", key(&context));

    let mut context = ImageResourceContext::default();
    context.may_use_save_data_quality = true;
    context.mobile_user_agent = true;
    context.libwebp_level = LibWebpLevel::Animated;
    assert_eq!("amd", key(&context));

    // When both may_use_small_screen_quality and may_use_save_data_quality are
    // set, may_use_save_data_quality takes precedence.
    let mut context = ImageResourceContext::default();
    context.may_use_small_screen_quality = true;
    context.may_use_save_data_quality = true;
    assert_eq!(".d", key(&context));

    let mut context = ImageResourceContext::default();
    context.supports_avif = true;
    assert_eq!("..avif", key(&context));
    context.libwebp_level = LibWebpLevel::LossyOnly;
    assert_eq!("w.avif", key(&context));
    context.supports_avif = false;
    assert_eq!("w", key(&context));
    context.libwebp_level = LibWebpLevel::None;
    assert_eq!(".", key(&context));
}

#[test]
fn different_webp_levels() {
    // Make sure different levels of WebP support get different cache keys.
    let mut seen = HashSet::new();
    for level in [
        LibWebpLevel::None,
        LibWebpLevel::LossyOnly,
        LibWebpLevel::LossyLosslessAlpha,
        LibWebpLevel::Animated,
    ] {
        let mut context = ImageResourceContext::default();
        context.libwebp_level = level;
        let cache_key = ImageUrlEncoder::cache_key_from_resource_context(&context);
        assert!(!seen.contains(&cache_key), "{cache_key}");
        seen.insert(cache_key);
    }
}

#[test]
fn bad_first() {
    expect_bad_dim("badx33x,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_first_webp() {
    expect_bad_dim("badx33w,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_first_webp_la() {
    expect_bad_dim("badx33v,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_first_mobile() {
    expect_bad_dim("badx33mx,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_first_webp_mobile() {
    expect_bad_dim("badx33mw,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_first_webp_la_mobile() {
    expect_bad_dim("badx33mv,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_second() {
    expect_bad_dim("17xbadx,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_second_webp() {
    expect_bad_dim("17xbadw,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_second_webp_la() {
    expect_bad_dim("17xbadv,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_second_mobile() {
    expect_bad_dim("17xbadmx,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_second_webp_mobile() {
    expect_bad_dim("17xbadmw,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_second_webp_la_mobile() {
    expect_bad_dim("17xbadmv,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_leading_n() {
    expect_bad_dim("Nxw,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_middle_n() {
    expect_bad_dim("17xN,hencoded.url,_with,_various.stuff");
}

#[test]
fn no_xs() {
    expect_bad_dim(",hencoded.url,_with,_various.stuff");
}

#[test]
fn no_xs_mobile() {
    expect_bad_dim("m,hencoded.url,_with,_various.stuff");
}

#[test]
fn blank_second() {
    expect_bad_dim("17xx,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_size_check() {
    // Catch case where url size check was inverted.
    expect_bad_dim("17xx");
}

#[test]
fn blank_second_webp() {
    expect_bad_dim("17xw,hencoded.url,_with,_various.stuff");
}

#[test]
fn blank_second_mobile() {
    expect_bad_dim("17xmx,hencoded.url,_with,_various.stuff");
}

#[test]
fn blank_second_webp_mobile() {
    expect_bad_dim("17xmw,hencoded.url,_with,_various.stuff");
}

#[test]
fn blank_second_webp_la_mobile() {
    expect_bad_dim("17xmv,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_trail_char() {
    expect_bad_dim("17x33u,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_init_char() {
    expect_bad_dim("u,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_width_char() {
    expect_bad_dim("17t,hencoded.url,_with,_various.stuff");
}

#[test]
fn bad_height_char() {
    expect_bad_dim("Nx33t,hencoded.url,_with,_various.stuff");
}

#[test]
fn short_both_dims() {
    expect_bad_dim("17x33");
}

#[test]
fn short_width() {
    expect_bad_dim("Nx33");
}

#[test]
fn short_height() {
    expect_bad_dim("17xN");
}

#[test]
fn both_dims_missing() {
    expect_bad_dim("NxNx,hencoded.url,_with,_various.stuff");
}

#[test]
fn very_short_url() {
    expect_bad_dim("7x3");
}

#[test]
fn truncated_after_first_dim() {
    expect_bad_dim("175x");
}

#[test]
fn truncated_before_sep() {
    expect_bad_dim("12500");
}

#[test]
fn webp_detection() {
    // Valid WebP URL.
    assert!(is_pagespeed_webp(
        "http://example.com/xa.jpg.pagespeed.ic.0.webp"
    ));
    assert!(is_pagespeed_webp(
        "http://example.com/xa.png.pagespeed.ic.0.webp"
    ));
    assert!(is_pagespeed_webp(
        "http://example.com/xa.gif.pagespeed.ic.0.webp"
    ));
    assert!(is_pagespeed_webp(
        "http://example.com/xa.webp.pagespeed.ic.0.webp"
    ));
    assert!(is_pagespeed_webp(
        "http://example.com/17x33a.jpg.pagespeed.ic.0.webp"
    ));

    // Invalid WebP URL.
    assert!(!is_pagespeed_webp(
        "http://example.com/xa.jpg.XXXXXXXX.ic.0.webp"
    ));
    assert!(!is_pagespeed_webp(
        "http://example.com/xa.gif.pagespeed.ic.0.png"
    ));
    assert!(!is_pagespeed_webp(
        "http://example.com/xa.png.pagespeed.ic.0.jpeg"
    ));
    assert!(!is_pagespeed_webp(
        "http://example.com/xa.jpg.pagespeed.ic.0.jpeg"
    ));
    assert!(!is_pagespeed_webp("http://example.com/foo.webp"));
    assert!(!is_pagespeed_webp("http://example.com/foo.jpg"));
    assert!(!is_pagespeed_webp(
        "http://example.com/x.jpg.pagespeed.cd.0.jpeg"
    ));
    assert!(!is_pagespeed_webp(
        "http://example.com/x.jpg.pagespeed.ce.0.webp"
    ));
}
