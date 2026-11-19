//! Rewritten-URL decoding (`RewriteDriver::DecodeUrl`).
//!
//! Reverses `.pagespeed.` leaves to original URLs: the leaf must parse
//! with a non-empty hash, proxy-mode URLs must re-resolve through the
//! namer, the id must belong to a registered rewrite filter (outline ids
//! decode to no URLs), the filter's encoder must accept the name, the id
//! must not be forbidden, and every decoded URL must absolutize onto the
//! leaf's origin. Anything else fails closed.
//!
//! TODO(pipeline): id `ic` (image compression) needs `ImageUrlEncoder`
//! (dimension codes + libwebp levels); it currently fails decode like an
//! unknown id and lands with the image pipeline slice.

use ps_cache::namer::ResourceNamer;
use ps_config::filter::Filter;
use ps_config::options::RewriteOptions;
use ps_http::google_url::GoogleUrl;

use crate::url_namer::{ProxyExtent, UrlNamer};
use crate::url_segment_encoder::{
    CssUrlEncoder, DefaultEncoder, MultipartEncoder, UrlSegmentEncoder,
};

/// Registered rewrite-filter ids (`resource_filter_map_`) with their
/// segment encoders. Outline ids (`co`, `jo`) decode without an encoder.
fn encoder_for_id(id: &str) -> Option<EncoderKind> {
    match id {
        "ce" | "jm" | "ls" | "sm" => Some(EncoderKind::Default),
        "cc" | "jc" | "is" => Some(EncoderKind::Multipart),
        "cf" => Some(EncoderKind::Css),
        _ => None,
    }
}

/// Segment encoder by registry id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncoderKind {
    Default,
    Multipart,
    Css,
}

impl EncoderKind {
    fn decode(&self, segment: &[u8]) -> Option<Vec<Vec<u8>>> {
        match self {
            EncoderKind::Default => DefaultEncoder.decode(segment),
            EncoderKind::Multipart => MultipartEncoder.decode(segment),
            EncoderKind::Css => CssUrlEncoder.decode(segment),
        }
    }
}

/// Outline-filter ids (decode to no URLs, without an encoder).
fn is_outline_id(id: &str) -> bool {
    id == "co" || id == "jo"
}

/// Driver inputs for URL decoding.
pub struct DecodeConfig<'a> {
    /// Rewrite options (oblivious gate + forbidden list).
    pub options: &'a RewriteOptions,
    /// URL namer (proxy-mode decoding).
    pub namer: &'a dyn UrlNamer,
    /// Hasher length in characters (hash/signature split).
    pub hash_length: usize,
    /// Signature length in characters (0 unless URL signing).
    pub signature_length: usize,
}

/// Decodes a rewritten URL to original absolute URLs (`DecodeUrl`).
#[must_use]
pub fn decode_url(url: &GoogleUrl, config: &DecodeConfig<'_>) -> Option<Vec<String>> {
    if config.options.oblivious_pagespeed_urls() {
        return None;
    }
    if !url.is_web_valid() {
        return None;
    }
    let mut namer = ResourceNamer::new();
    let leaf = url.leaf_sans_query();
    if !namer.decode(
        &leaf,
        config.hash_length as i32,
        config.signature_length as i32,
    ) {
        return None;
    }
    // Hashless names never produce addressable resources (wrong hashes
    // still pass; they arise legitimately under asynchrony).
    if namer.hash().is_empty() {
        return None;
    }
    let proxy_mode = config.namer.proxy_mode();
    let base = if proxy_mode == ProxyExtent::Full
        || (proxy_mode == ProxyExtent::InputOnly && config.namer.is_proxy_encoded(url))
    {
        if !config.namer.is_proxy_encoded(url) {
            return None;
        }
        let decoded = config.namer.decode(url)?;
        let base = GoogleUrl::new(&decoded);
        if !base.is_web_valid() {
            return None;
        }
        base
    } else {
        url.clone()
    };
    let url_base = GoogleUrl::new(base.all_except_leaf());
    let id = namer.id().to_string();
    let mut decoded = Vec::new();
    if is_outline_id(&id) {
        // Outline filters encode cacheable resources without an encoder.
    } else if let Some(encoder) = encoder_for_id(&id) {
        let name = namer.name().as_bytes().to_vec();
        decoded = encoder
            .decode(&name)?
            .iter()
            .map(|url| String::from_utf8_lossy(url).into_owned())
            .collect();
    } else {
        return None;
    }
    if forbidden(config.options, &id) {
        return None;
    }
    let mut absolute = Vec::with_capacity(decoded.len());
    for relative in &decoded {
        let full = GoogleUrl::resolve(&url_base, relative);
        // Decoded names stay on the leaf's origin, like fetch preparation.
        if !full.is_web_valid() || full.origin() != url_base.origin() {
            return None;
        }
        absolute.push(full.spec().to_string());
    }
    Some(absolute)
}

/// Whether an id is forbidden (`RewriteOptions::Forbidden(id)`): unknown
/// ids (including `ce`) never are; known ones honor the forbidden set
/// plus forbid-all-disabled.
fn forbidden(options: &RewriteOptions, id: &str) -> bool {
    let Some(filter) = Filter::from_id(id) else {
        return false;
    };
    options.filters().forbidden(filter)
        || (options.forbid_all_disabled() && options.filters().disabled_set().contains(filter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url_namer::TestUrlNamer;

    fn options() -> RewriteOptions {
        RewriteOptions::new()
    }

    fn decode(spec: &str) -> Option<Vec<String>> {
        let options = options();
        let namer = TestUrlNamer::new();
        decode_url(
            &GoogleUrl::new(spec),
            &DecodeConfig {
                options: &options,
                namer: &namer,
                hash_length: 1,
                signature_length: 0,
            },
        )
    }

    #[test]
    fn single_url_round_trip() {
        assert_eq!(
            decode("http://test.com/b.css.pagespeed.ce.0.css"),
            Some(vec!["http://test.com/b.css".to_string()])
        );
        assert_eq!(
            decode("http://test.com/c.js.pagespeed.jm.555.js"),
            Some(vec!["http://test.com/c.js".to_string()])
        );
        // Experiment letters ride along.
        assert_eq!(
            decode("http://test.com/d.js.pagespeed.b.jm.0.js"),
            Some(vec!["http://test.com/d.js".to_string()])
        );
    }

    #[test]
    fn combined_url_yields_many() {
        assert_eq!(
            decode("http://www.test.com/I.e.css+f.css.pagespeed.cc.0.css"),
            Some(vec![
                "http://www.test.com/I.e.css".to_string(),
                "http://www.test.com/f.css".to_string(),
            ])
        );
    }

    #[test]
    fn outline_decodes_empty() {
        assert_eq!(
            decode("http://test.com/x.pagespeed.co.0.css"),
            Some(Vec::new())
        );
        assert_eq!(
            decode("http://test.com/x.pagespeed.jo.0.js"),
            Some(Vec::new())
        );
    }

    #[test]
    fn rejects_cleanly() {
        // Not a rewritten leaf.
        assert_eq!(decode("http://test.com/a.css"), None);
        // Unknown filter id.
        assert_eq!(decode("http://test.com/a.pagespeed.zz.0.css"), None);
        // Image id waits for its encoder (pipeline slice).
        assert_eq!(decode("http://test.com/a.pagespeed.ic.0.jpg"), None);
        // Invalid URLs never decode.
        assert_eq!(decode("bogus"), None);
    }

    #[test]
    fn oblivious_and_forbidden_gates() {
        let mut options = RewriteOptions::new();
        options
            .set_option(
                "ObliviousPagespeedUrls",
                "on",
                ps_config::Scope::ProcessStrict,
            )
            .unwrap();
        let namer = TestUrlNamer::new();
        assert_eq!(
            decode_url(
                &GoogleUrl::new("http://test.com/b.css.pagespeed.ce.0.css"),
                &DecodeConfig {
                    options: &options,
                    namer: &namer,
                    hash_length: 1,
                    signature_length: 0,
                },
            ),
            None
        );

        // Forbidding the owning filter vetoes its URLs.
        let mut options = RewriteOptions::new();
        options.filters_mut().forbid(Filter::RewriteCss);
        assert!(forbidden(&options, "cf"));
        assert!(!forbidden(&options, "ce"));
        assert!(!forbidden(&options, "zz"));
        assert_eq!(
            decode_url(
                &GoogleUrl::new("http://test.com/b.css.pagespeed.cf.0.css"),
                &DecodeConfig {
                    options: &options,
                    namer: &namer,
                    hash_length: 1,
                    signature_length: 0,
                },
            ),
            None
        );
    }

    #[test]
    fn proxy_modes() {
        // Full proxy mode requires proxy-encoded URLs.
        let options = options();
        let mut namer = TestUrlNamer::new();
        namer.set_proxy_mode(ProxyExtent::Full);
        assert_eq!(
            decode_url(
                &GoogleUrl::new("http://test.com/b.css.pagespeed.ce.0.css"),
                &DecodeConfig {
                    options: &options,
                    namer: &namer,
                    hash_length: 1,
                    signature_length: 0,
                },
            ),
            None
        );
        // A proxy-encoded leaf decodes against its decoded base.
        assert_eq!(
            decode_url(
                &GoogleUrl::new("http://cdn.com/http/x.com/https/y.com/p/b.css.pagespeed.ce.0.css"),
                &DecodeConfig {
                    options: &options,
                    namer: &namer,
                    hash_length: 1,
                    signature_length: 0,
                },
            ),
            Some(vec!["https://y.com/p/b.css".to_string()])
        );
    }
}
