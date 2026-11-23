//! Confidence-based website logo detection (`logo_detection`).
//!
//! Scores how likely an image is a site logo from multiple independent
//! signal groups — URL/filename, class/id (including WordPress
//! conventions), alt text, `itemprop="logo"` microdata, ancestor
//! placement (headers, navigation, site-branding containers,
//! schema.org scopes), explicit dimensions, and image format —
//! minus hero/content counter-signals. No single attribute decides
//! alone: a positive verdict needs both a score of at least
//! [`LOGO_THRESHOLD`] and at least two distinct signal groups, so a
//! hero or article image that merely mentions "logo" stays a
//! candidate.
//!
//! The LCP filter uses this to keep ordinary logos out of automatic
//! prioritization (preload hints, `fetchpriority="high"`, and the
//! heuristic hero slot) without touching author-defined priorities:
//! skipped logos are left byte-identical, and learned beacon
//! evidence still promotes a logo that measurements prove is the
//! LCP element. CSS-background and evidence-URL scoring ride the
//! same weights through [`score_logo_url`] and
//! [`css_background_urls`]; element-less backgrounds only ever
//! preload from evidence (measurements win), so those entry points
//! exist for unit coverage and future filter wiring.
#![forbid(unsafe_code)]

use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Positive verdict threshold (with ≥2 signal groups).
pub const LOGO_THRESHOLD: f32 = 0.50;

/// WordPress logo markers (`custom-logo*`, `site-logo*`).
const WP_LOGO_MARKERS: &[&str] = &[
    "custom-logo",
    "site-logo",
    "wp-block-site-logo",
    "custom-logo-link",
];

/// Branding container markers (class/id, element or ancestor).
const BRANDING_MARKERS: &[&str] = &[
    "site-branding",
    "brand-logo",
    "header-logo",
    "nav-logo",
    "navbar-brand",
    "site-title",
];

/// Hero/content counter-signal markers (class/id, element or ancestor).
const NON_LOGO_MARKERS: &[&str] = &[
    "hero", "banner", "featured", "slider", "carousel", "jumbotron", "billboard", "cover-image",
    "coverimage", "article-image", "post-thumbnail",
];

/// Schema.org scopes whose logo context corroborates (`itemtype`).
const ORG_SCOPES: &[&str] = &[
    "organization",
    "localbusiness",
    "brand",
    "corporation",
    "ngo",
    "store",
    "restaurant",
];

/// Logo verdict for one image.
#[derive(Debug, Clone, PartialEq)]
pub struct LogoVerdict {
    /// Confident website logo (score ≥ threshold, ≥2 groups).
    pub is_logo: bool,
    /// Combined confidence in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Distinct positive signal groups observed.
    pub groups: u32,
}

/// Detects whether an `<img>` is a website logo.
///
/// `src` is the (possibly rewritten) image source; names survive
/// rewriting, so rewritten URLs score like originals. Runs on every
/// LCP-eligible image: attribute reads plus one ancestor walk, no
/// allocation beyond lowercased copies of small values.
#[must_use]
pub fn detect_logo(dom: &mut Dom, img_id: NodeId, src: &[u8]) -> LogoVerdict {
    let mut score = 0.0f32;
    let mut groups = 0u32;

    // URL group: filename hit outranks a path-only hit.
    let (url_score, _) = score_logo_url(src);
    if url_score > 0.0 {
        score += url_score;
        groups += 1;
    }

    // Class/id group: exact "logo" token, WP markers, branding, substring.
    let class_score = class_id_score(&element_values(dom, img_id));
    if class_score > 0.0 {
        score += class_score;
        groups += 1;
    }

    // Alt group.
    if let Some(alt) = attribute_value(dom, img_id, b"alt") {
        if contains_word(&alt, "logo") {
            score += 0.25;
            groups += 1;
        }
    }

    // Itemprop group: explicit schema.org assertion (strongest single
    // signal, still needs corroboration).
    if let Some(itemprop) = attribute_value(dom, img_id, b"itemprop") {
        if has_token(&itemprop, "logo") {
            score += 0.45;
            groups += 1;
        }
    }

    // Ancestor group (capped): placement + containers + org scope.
    let (ancestor_score, ancestor_veto) = ancestor_signals(dom, img_id);
    if ancestor_score > 0.0 {
        score += ancestor_score;
        groups += 1;
    }
    if ancestor_veto {
        score -= 0.60;
    }

    // Element-level hero/content veto.
    if has_non_logo_marker(&element_values(dom, img_id)) {
        score -= 0.60;
    }
    // Content-context discount (article/figure without branding).
    if in_content_context(dom, img_id) {
        score -= 0.15;
    }

    // Dimensions group: small-and-wide corroborates, huge vetoes.
    match logo_dimensions(dom, img_id) {
        DimensionSignal::LogoLike => {
            score += 0.10;
            groups += 1;
        }
        DimensionSignal::HeroLike => {
            score -= 0.35;
        }
        DimensionSignal::Unknown => {}
    }

    // Format group: SVG corroborates weakly.
    if is_svg_url(src) {
        score += 0.10;
        groups += 1;
    }

    let confidence = score.clamp(0.0, 1.0);
    LogoVerdict {
        is_logo: confidence >= LOGO_THRESHOLD && groups >= 2,
        confidence,
        groups,
    }
}

/// Scores bare URL evidence (filename > path), returning
/// `(score, filename_hit)`. Shared by element detection and
/// CSS-background/evidence-URL scoring.
#[must_use]
pub fn score_logo_url(url: &[u8]) -> (f32, bool) {
    let text = String::from_utf8_lossy(url).to_ascii_lowercase();
    let path = text.split(['?', '#']).next().unwrap_or_default();
    let filename = path.rsplit('/').next().unwrap_or_default();
    if filename.contains("logo") {
        (0.30, true)
    } else if path.contains("logo") {
        (0.15, false)
    } else {
        (0.0, false)
    }
}

/// Extracts every `url(...)` target from an inline style value
/// (quote- and whitespace-tolerant; empty targets dropped).
#[must_use]
pub fn css_background_urls(style_value: &[u8]) -> Vec<Vec<u8>> {
    let text = String::from_utf8_lossy(style_value);
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(found) = find_subslice(&bytes[pos..], b"url(") {
        let start = pos + found + 4;
        // Balanced close paren, honoring single/double quotes.
        let mut end = start;
        let mut quote = 0u8;
        while end < bytes.len() {
            let byte = bytes[end];
            if quote != 0 {
                if byte == quote {
                    quote = 0;
                }
            } else if byte == b'\'' || byte == b'"' {
                quote = byte;
            } else if byte == b')' {
                break;
            }
            end += 1;
        }
        if end < bytes.len() {
            let mut target = text[start..end].trim().as_bytes().to_vec();
            if target.len() >= 2
                && (target.starts_with(b"'") || target.starts_with(b"\""))
                && target.ends_with(&[target[0]])
            {
                target = target[1..target.len() - 1].to_vec();
            }
            if !target.is_empty() {
                out.push(target);
            }
            pos = end + 1;
        } else {
            break;
        }
    }
    out
}

/// Reads one attribute's decoded value by raw name.
fn attribute_value(dom: &mut Dom, id: NodeId, name: &[u8]) -> Option<Vec<u8>> {
    let index = if name == b"class" {
        dom.find_attribute(id, Keyword::Class)
    } else if name == b"id" {
        dom.find_attribute(id, Keyword::Id)
    } else {
        dom.find_attribute_by_name(id, name)
    }?;
    dom.decoded_attribute(id, index).0
}

/// Collects class + id values for one element.
fn element_values(dom: &mut Dom, id: NodeId) -> Vec<Vec<u8>> {
    let mut values = Vec::new();
    for name in [b"class".as_slice(), b"id".as_slice()] {
        if let Some(value) = attribute_value(dom, id, name) {
            if !value.is_empty() {
                values.push(value);
            }
        }
    }
    values
}

/// Scores class/id values (max wins; WP markers outrank substrings).
fn class_id_score(values: &[Vec<u8>]) -> f32 {
    let mut best = 0.0f32;
    for value in values {
        let lower = String::from_utf8_lossy(value).to_ascii_lowercase();
        if has_token(lower.as_bytes(), "logo") {
            best = best.max(0.30);
        }
        if WP_LOGO_MARKERS.iter().any(|marker| lower.contains(marker)) {
            best = best.max(0.35);
        }
        if BRANDING_MARKERS.iter().any(|marker| lower.contains(marker)) {
            best = best.max(0.25);
        }
        if lower.contains("logo") {
            best = best.max(0.20);
        }
    }
    best
}

/// Whether any value carries a hero/content counter-signal.
fn has_non_logo_marker(values: &[Vec<u8>]) -> bool {
    values.iter().any(|value| {
        let lower = String::from_utf8_lossy(value).to_ascii_lowercase();
        NON_LOGO_MARKERS.iter().any(|marker| lower.contains(marker))
    })
}

/// Ancestor placement score (capped) plus hero veto.
fn ancestor_signals(dom: &mut Dom, id: NodeId) -> (f32, bool) {
    let mut score = 0.0f32;
    let mut veto = false;
    let mut cursor = dom.arena_node(id).parent();
    while let Some(parent) = cursor {
        if dom.element_name_equals(parent, b"header") {
            score += 0.20;
        } else if dom.element_name_equals(parent, b"nav") {
            score += 0.15;
        }
        let values = element_values(dom, parent);
        if has_non_logo_marker(&values) {
            veto = true;
        }
        let class_score = class_id_score(&values);
        // Ancestor classes corroborate placement (branding-class hit
        // or WP markers); plain "logo" substrings count less here
        // than on the element itself.
        if class_score >= 0.25 {
            score += 0.25;
        } else if class_score > 0.0 {
            score += 0.15;
        }
        // Home-link ancestor (classic WP logo link).
        if dom.element_name_equals(parent, b"a") {
            if let Some(rel) = attribute_value(dom, parent, b"rel") {
                if has_token(&rel, "home") {
                    score += 0.10;
                }
            }
        }
        // Schema.org organization scope.
        if let Some(itemtype) = attribute_value(dom, parent, b"itemtype") {
            let lower = String::from_utf8_lossy(&itemtype).to_ascii_lowercase();
            if ORG_SCOPES.iter().any(|scope| lower.contains(scope)) {
                score += 0.15;
            }
        }
        cursor = dom.arena_node(parent).parent();
    }
    (score.min(0.30), veto)
}

/// Whether the image sits in article/figure content (discounted).
/// Figures carrying logo/branding markers (e.g. WordPress
/// `wp-block-site-logo`) are branding wrappers, not content.
fn in_content_context(dom: &mut Dom, id: NodeId) -> bool {
    let mut cursor = dom.arena_node(id).parent();
    while let Some(parent) = cursor {
        if dom.element_name_equals(parent, b"article") {
            return true;
        }
        if dom.element_name_equals(parent, b"figure")
            && class_id_score(&element_values(dom, parent)) < 0.20
        {
            return true;
        }
        cursor = dom.arena_node(parent).parent();
    }
    false
}

/// Explicit-dimension signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DimensionSignal {
    /// Small and wide (logo-like).
    LogoLike,
    /// Huge (hero/article-like).
    HeroLike,
    /// Absent or uninformative.
    Unknown,
}

/// Reads explicit width/height attributes.
fn logo_dimensions(dom: &mut Dom, id: NodeId) -> DimensionSignal {
    let mut number = |name: &[u8]| -> Option<i32> {
        let value = attribute_value(dom, id, name)?;
        std::str::from_utf8(&value)
            .ok()?
            .trim()
            .trim_end_matches("px")
            .trim()
            .parse::<i32>()
            .ok()
    };
    let (Some(width), Some(height)) = (number(b"width"), number(b"height")) else {
        return DimensionSignal::Unknown;
    };
    if width <= 0 || height <= 0 {
        return DimensionSignal::Unknown;
    }
    if width >= 1000 || height >= 700 {
        DimensionSignal::HeroLike
    } else if height <= 160 && width <= 640 {
        DimensionSignal::LogoLike
    } else {
        DimensionSignal::Unknown
    }
}

/// Whether a URL names an SVG payload (query/fragment ignored).
fn is_svg_url(url: &[u8]) -> bool {
    let text = String::from_utf8_lossy(url).to_ascii_lowercase();
    let path = text.split(['?', '#']).next().unwrap_or_default();
    path.ends_with(".svg")
}

/// Whether whitespace-separated tokens contain `word` (ASCII
/// case-insensitive).
fn has_token(value: &[u8], word: &str) -> bool {
    value
        .split(|byte| byte.is_ascii_whitespace())
        .any(|token| token.eq_ignore_ascii_case(word.as_bytes()))
}

/// Whether `word` appears anywhere (ASCII case-insensitive).
fn contains_word(value: &[u8], word: &str) -> bool {
    find_subslice(&value.to_ascii_lowercase(), word.as_bytes()).is_some()
}

/// Byte-substring search.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_scoring_prefers_filenames() {
        assert_eq!(score_logo_url(b"https://h/a/logo.png"), (0.30, true));
        assert_eq!(score_logo_url(b"/img/LOGO-retina.webp?v=2"), (0.30, true));
        assert_eq!(
            score_logo_url(b"https://h/assets/logo/x.png"),
            (0.15, false)
        );
        assert_eq!(score_logo_url(b"https://h/a/hero.jpg"), (0.0, false));
    }

    #[test]
    fn css_url_extraction() {
        assert_eq!(
            css_background_urls(b"background-image:url(/i/logo.png)"),
            vec![b"/i/logo.png".to_vec()]
        );
        assert_eq!(
            css_background_urls(b"background: url( \"/i/a.png\" ) , url('/i/b.svg');"),
            vec![b"/i/a.png".to_vec(), b"/i/b.svg".to_vec()]
        );
        assert!(css_background_urls(b"color: red").is_empty());
        assert!(css_background_urls(b"background: url(").is_empty());
    }

    #[test]
    fn css_background_logo_pipeline() {
        // Element-less backgrounds score through URL extraction +
        // filename weights (entry points for background-capable
        // callers; the LCP filter's only background action is
        // evidence-driven, where measurements win).
        let urls = css_background_urls(
            b".brand{background:url(\"/assets/site-logo.svg\") no-repeat}",
        );
        assert_eq!(urls, vec![b"/assets/site-logo.svg".to_vec()]);
        let (score, filename_hit) = score_logo_url(&urls[0]);
        assert_eq!((score, filename_hit), (0.30, true));
        // Non-logo backgrounds score nothing.
        let urls = css_background_urls(b"background:url(/img/hero-banner.jpg)");
        assert_eq!(urls.len(), 1);
        assert_eq!(score_logo_url(&urls[0]), (0.0, false));
    }

    #[test]
    fn org_scopes_are_ascii() {
        // Non-ASCII in a scope marker would silently never match.
        for scope in ORG_SCOPES {
            assert!(scope.is_ascii(), "{scope}");
        }
    }
}
