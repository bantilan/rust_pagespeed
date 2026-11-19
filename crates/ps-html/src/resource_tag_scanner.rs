//! Resource attribute scanning (`resource_tag_scanner::ScanElement`).
//!
//! Finds URL-valued attributes on an element and classifies them
//! (stylesheet/script/image/hyperlink/...) by tag structure plus
//! operator-configured `UrlValuedAttribute` overrides. URL resolution
//! itself belongs to the pipelines; this module never parses URLs.

use ps_config::options::Category;

use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Attribute cap per element (`kMaxScannedAttributes`).
const MAX_SCANNED_ATTRIBUTES: usize = 256;

/// Operator URL-valued attributes (`UrlValuedAttribute` triples).
pub type UrlValuedAttributes<'a> = &'a [(String, String, Category)];

/// Scans an element's attributes (`ScanElement`), returning (attribute
/// index, category) for each URL-valued one, in attribute order.
pub fn scan_element(
    dom: &mut Dom,
    id: NodeId,
    url_valued: UrlValuedAttributes<'_>,
) -> Vec<(usize, Category)> {
    let count = dom.element_attributes(id).len();
    // Absurdly attributed elements are left alone entirely.
    if count > MAX_SCANNED_ATTRIBUTES {
        return Vec::new();
    }
    let mut found = Vec::new();
    for index in 0..count {
        // Validity reads the decoding flag as-is: fresh attributes count
        // as valid until something decodes them, exactly like the C++.
        if dom.element_attributes(id)[index].decoding_error() {
            continue;
        }
        if let Some(category) = categorize_attribute(dom, id, index, url_valued) {
            found.push((index, category));
        }
    }
    found
}

/// Classifies one attribute (`CategorizeAttribute`); user overrides win
/// over spec-defined pairs (`None` is `kUndefined`).
fn categorize_attribute(
    dom: &mut Dom,
    id: NodeId,
    index: usize,
    url_valued: UrlValuedAttributes<'_>,
) -> Option<Category> {
    for (element, attribute, category) in url_valued {
        let element_match = dom
            .element_name(id)
            .eq_ignore_ascii_case(element.as_bytes());
        let attribute_match = dom.element_attributes(id)[index]
            .name_str()
            .eq_ignore_ascii_case(attribute.as_bytes());
        if element_match && attribute_match {
            return Some(*category);
        }
        // Inlining turns links into styles keeping attributes, so link
        // overrides interpret on style elements too.
        if dom.element_keyword(id) == Keyword::Style
            && element.eq_ignore_ascii_case("link")
            && attribute_match
        {
            return Some(*category);
        }
    }
    categorize_by_spec(dom, id, index)
}

/// Spec-defined pairs (`CategorizeAttributeBySpec`).
fn categorize_by_spec(dom: &mut Dom, id: NodeId, index: usize) -> Option<Category> {
    let keyword = dom.element_keyword(id);
    let attribute = dom.element_attributes(id)[index].keyword();
    match keyword {
        Keyword::Link => categorize_link(dom, id, attribute),
        Keyword::Script if attribute == Keyword::Src => Some(Category::Script),
        Keyword::Img => match attribute {
            Keyword::Src | Keyword::DataSrc => Some(Category::Image),
            Keyword::Longdesc => Some(Category::Hyperlink),
            _ => None,
        },
        Keyword::Body => match attribute {
            Keyword::Background => Some(Category::Image),
            Keyword::Cite => Some(Category::Hyperlink),
            _ => None,
        },
        Keyword::Td
        | Keyword::Th
        | Keyword::Table
        | Keyword::Tbody
        | Keyword::Tfoot
        | Keyword::Thead => {
            if attribute == Keyword::Background {
                Some(Category::Image)
            } else {
                None
            }
        }
        Keyword::Input => {
            if attribute == Keyword::Formaction {
                Some(Category::Hyperlink)
            } else if attribute == Keyword::Src && is_image_input(dom, id) {
                Some(Category::Image)
            } else {
                None
            }
        }
        Keyword::Command if attribute == Keyword::Icon => Some(Category::Image),
        Keyword::A | Keyword::Area if attribute == Keyword::Href => Some(Category::Hyperlink),
        Keyword::Form if attribute == Keyword::Action => Some(Category::Hyperlink),
        Keyword::Video => match attribute {
            Keyword::Src => Some(Category::OtherResource),
            Keyword::Poster => Some(Category::Image),
            _ => None,
        },
        Keyword::Audio | Keyword::Source | Keyword::Track | Keyword::Embed
            if attribute == Keyword::Src =>
        {
            Some(Category::OtherResource)
        }
        Keyword::Frame | Keyword::Iframe => match attribute {
            Keyword::Src => Some(Category::OtherResource),
            Keyword::Longdesc => Some(Category::Hyperlink),
            _ => None,
        },
        Keyword::Html if attribute == Keyword::Manifest => Some(Category::OtherResource),
        Keyword::Blockquote | Keyword::Q | Keyword::Ins | Keyword::Del
            if attribute == Keyword::Cite =>
        {
            Some(Category::Hyperlink)
        }
        Keyword::Button if attribute == Keyword::Formaction => Some(Category::Hyperlink),
        _ => None,
    }
}

/// Link `href` classification: stylesheets, icons, and prefetches.
fn categorize_link(dom: &mut Dom, id: NodeId, attribute: Keyword) -> Option<Category> {
    if attribute != Keyword::Href {
        return None;
    }
    let Some(rel_index) = dom.find_attribute(id, Keyword::Rel) else {
        // Bare links are plain hyperlinks.
        return Some(Category::Hyperlink);
    };
    let (rel, _) = dom.decoded_attribute(id, rel_index);
    let rel = rel.as_deref().unwrap_or(b"");
    if crate::css_tag_scanner::is_stylesheet_or_alternate(rel) {
        return Some(Category::Stylesheet);
    }
    // Unrecognized tokens are ignored (`shortcut icon` reads as icon).
    let tokens: Vec<&[u8]> = rel
        .split(|byte| *byte == b' ')
        .filter(|token| !token.is_empty())
        .collect();
    // Images take precedence over prefetches.
    if tokens.iter().any(|token| {
        token.eq_ignore_ascii_case(b"icon")
            || token.eq_ignore_ascii_case(b"apple-touch-icon")
            || token.eq_ignore_ascii_case(b"apple-touch-icon-precomposed")
            || token.eq_ignore_ascii_case(b"apple-touch-startup-image")
    }) {
        return Some(Category::Image);
    }
    if tokens.iter().any(|token| {
        token.eq_ignore_ascii_case(b"prefetch") || token.eq_ignore_ascii_case(b"dns-prefetch")
    }) {
        return Some(Category::Prefetch);
    }
    None
}

/// True for `<input type=image ...>` (decoded, case-insensitive).
fn is_image_input(dom: &mut Dom, id: NodeId) -> bool {
    match dom.find_attribute(id, Keyword::Type) {
        Some(index) => {
            let (decoded, _) = dom.decoded_attribute(id, index);
            decoded
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(b"image"))
        }
        None => false,
    }
}
