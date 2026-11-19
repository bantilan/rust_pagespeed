//! Stylesheet link detection (`CssTagScanner`, element half).
//!
//! Ports the element-inspection helpers: stylesheet-link recognition
//! (`ParseCssElement`), `@import` sniffing (`HasImport`), and the
//! `rel`-token predicates. The streaming URL transformer
//! (`TransformUrls`) rides the resource pipelines and lands with them.

use crate::element::NodeId;
use crate::name::Keyword;
use crate::parser::Dom;

/// Recognized stylesheet link (`ParseCssElement` success).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssLink {
    /// Index of the `href` attribute.
    pub href: usize,
    /// Decoded `media` value (empty when absent).
    pub media: Vec<u8>,
    /// Nonstandard attribute names, in order.
    pub nonstandard: Vec<Vec<u8>>,
}

/// True for `<link rel=stylesheet href=...>` with optional
/// `type=text/css` and `media` (`ParseCssElement`).
pub fn parse_css_element(dom: &mut Dom, id: NodeId) -> Option<CssLink> {
    if dom.element_keyword(id) != Keyword::Link {
        return None;
    }
    let mut href = None;
    let mut media = Vec::new();
    let mut has_rel_stylesheet = false;
    let mut nonstandard = Vec::new();
    let count = dom.element_attributes(id).len();
    for index in 0..count {
        let keyword = dom.element_attributes(id)[index].keyword();
        match keyword {
            Keyword::Href => {
                let (decoded, error) = decoded_attr(dom, id, index);
                if href.is_some() || error {
                    // Duplicate or undecipherable href.
                    let _ = decoded;
                    return None;
                }
                href = Some(index);
            }
            Keyword::Rel => {
                let (decoded, _) = decoded_attr(dom, id, index);
                let rel = decoded.as_deref().unwrap_or(b"");
                if !rel.trim_ascii().eq_ignore_ascii_case(b"stylesheet") {
                    // Includes alternate stylesheets.
                    return None;
                }
                has_rel_stylesheet = true;
            }
            Keyword::Media => {
                let (decoded, _) = decoded_attr(dom, id, index);
                match decoded {
                    // Bare `media` (or a decoding error) fails.
                    None => return None,
                    Some(value) => media = value,
                }
            }
            Keyword::Type => {
                let (decoded, _) = decoded_attr(dom, id, index);
                let value = decoded.as_deref().unwrap_or(b"");
                if !value.trim_ascii().eq_ignore_ascii_case(b"text/css") {
                    return None;
                }
            }
            Keyword::Title | Keyword::DataPagespeedNoTransform | Keyword::PagespeedNoTransform => {
                return None;
            }
            _ => {
                nonstandard.push(dom.element_attributes(id)[index].name_str().to_vec());
            }
        }
    }
    let href = href?;
    if !has_rel_stylesheet {
        return None;
    }
    Some(CssLink {
        href,
        media,
        nonstandard,
    })
}

/// Decoded value plus decoding-error flag for one attribute.
fn decoded_attr(dom: &mut Dom, id: NodeId, index: usize) -> (Option<Vec<u8>>, bool) {
    let attribute = &mut dom.element_attributes_mut(id)[index];
    let decoded = attribute.decoded_value_or_null().map(<[u8]>::to_vec);
    (decoded, attribute.decoding_error())
}

/// True when the text contains a case-insensitive `@import` (`HasImport`).
/// Deliberately naive: matches inside comments, strings, and longer words
/// (`@importinvalid`) exactly like the C++; short tails never match.
#[must_use]
pub fn has_import(contents: &[u8]) -> bool {
    contents
        .windows(b"@import".len())
        .any(|window| window.eq_ignore_ascii_case(b"@import"))
}

/// True when a `rel` token list names `stylesheet` (`IsStylesheetOrAlternate`).
#[must_use]
pub fn is_stylesheet_or_alternate(value: &[u8]) -> bool {
    split_tokens(value)
        .iter()
        .any(|token| token.eq_ignore_ascii_case(b"stylesheet"))
}

/// True when a `rel` token list names both stylesheets (`IsAlternateStylesheet`).
#[must_use]
pub fn is_alternate_stylesheet(value: &[u8]) -> bool {
    let tokens = split_tokens(value);
    tokens
        .iter()
        .any(|token| token.eq_ignore_ascii_case(b"stylesheet"))
        && tokens
            .iter()
            .any(|token| token.eq_ignore_ascii_case(b"alternate"))
}

/// Splits on ASCII spaces, dropping empties (`SplitStringPieceToVector`).
fn split_tokens(value: &[u8]) -> Vec<&[u8]> {
    value
        .split(|byte| *byte == b' ')
        .filter(|token| !token.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh `<link rel=stylesheet href=URL>` element (the C++ fixture).
    fn fixture_link(dom: &mut Dom) -> NodeId {
        let id = dom.new_element(None, b"link");
        dom.add_attribute(id, b"rel", b"stylesheet");
        dom.add_attribute(id, b"href", b"http://www.myhost.com/static/mycss.css");
        id
    }

    fn decoded_href(dom: &mut Dom, id: NodeId, link: &CssLink) -> Option<Vec<u8>> {
        let attribute = &mut dom.element_attributes_mut(id)[link.href];
        attribute.decoded_value_or_null().map(<[u8]>::to_vec)
    }

    #[test]
    fn minimal_ok() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        let link = parse_css_element(&mut dom, id).unwrap();
        assert!(link.media.is_empty());
        assert_eq!(
            decoded_href(&mut dom, id, &link).as_deref(),
            Some(b"http://www.myhost.com/static/mycss.css".as_slice())
        );
        assert!(link.nonstandard.is_empty());
    }

    #[test]
    fn nonstandard_attribute_ok() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        dom.add_attribute(id, b"other", b"value");
        let link = parse_css_element(&mut dom, id).unwrap();
        assert_eq!(link.nonstandard, vec![b"other".to_vec()]);
    }

    #[test]
    fn with_type_ok() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        dom.add_attribute(id, b"type", b"text/css");
        assert!(parse_css_element(&mut dom, id).is_some());
    }

    #[test]
    fn bad_type_fail() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        dom.add_attribute(id, b"type", b"text/plain");
        assert!(parse_css_element(&mut dom, id).is_none());
    }

    #[test]
    fn with_media_ok() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        dom.add_attribute(id, b"media", b"print");
        let link = parse_css_element(&mut dom, id).unwrap();
        assert_eq!(link.media, b"print");
    }

    #[test]
    fn doubled_href_fail() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        let href = dom.element_attributes(id)[1].clone();
        dom.element_attributes_mut(id).push(href);
        assert!(parse_css_element(&mut dom, id).is_none());
    }

    #[test]
    fn missing_rel_fail() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        assert!(dom.delete_attribute(id, Keyword::Rel));
        assert!(parse_css_element(&mut dom, id).is_none());
    }

    #[test]
    fn alternate_rel_fail() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        assert!(dom.delete_attribute(id, Keyword::Rel));
        dom.add_attribute(id, b"rel", b"alternate stylesheet");
        assert!(parse_css_element(&mut dom, id).is_none());
    }

    #[test]
    fn missing_rel_doubled_href_fail() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        assert!(dom.delete_attribute(id, Keyword::Rel));
        let href = dom.element_attributes(id)[0].clone();
        dom.element_attributes_mut(id).push(href);
        assert!(parse_css_element(&mut dom, id).is_none());
    }

    #[test]
    fn doubled_rel_ok() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        let rel = dom.element_attributes(id)[0].clone();
        dom.element_attributes_mut(id).push(rel);
        assert!(parse_css_element(&mut dom, id).is_some());
    }

    #[test]
    fn missing_href_doubled_rel_fail() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        assert!(dom.delete_attribute(id, Keyword::Href));
        let rel = dom.element_attributes(id)[0].clone();
        dom.element_attributes_mut(id).push(rel);
        assert!(parse_css_element(&mut dom, id).is_none());
    }

    #[test]
    fn rel_case_insensitive_ok() {
        let mut dom = Dom::new();
        let id = fixture_link(&mut dom);
        assert!(dom.delete_attribute(id, Keyword::Rel));
        dom.add_attribute(id, b"rel", b"StyleSheet");
        assert!(parse_css_element(&mut dom, id).is_some());
    }

    #[test]
    fn has_import_matrix() {
        assert!(has_import(b"@import"));
        assert!(has_import(b"@Import"));
        assert!(has_import(
            b"@charset 'iso-8859-1';\n@import url('http://foo.com');\n"
        ));
        assert!(has_import(
            b"@charset 'iso-8859-1';\n@iMPorT url('http://foo.com');\n"
        ));
        assert!(!has_import(b""));
        assert!(!has_import(b"@impor"));
        assert!(!has_import(b"@charset 'iso-8859-1';\n@impor"));
        assert!(!has_import(b"@impor"));
        // Documented false positives: longer words and comments match.
        assert!(has_import(
            b"@charset 'iso-8859-1';\n@importinvalid url('http://foo.com');\n"
        ));
        assert!(has_import(
            b"@charset 'iso-8859-1';\n/* @import url('http://foo.com'); */\n"
        ));
        assert!(has_import(
            b"@charset 'iso-8859-1';\na { color: pink; }\n/* @import after rulesets is invalid */\n@import url('http://foo.com');\n"
        ));
    }

    #[test]
    fn stylesheet_or_alternate_matrix() {
        for value in [
            "stylesheet",
            "canonical stylesheet",
            " stylesheet",
            " styleSheet",
            "alternate stylesheet",
            "stylesheet alternate",
            "stylesheet alternate canonical",
            "StyleshEet alternAte canoNical ",
        ] {
            assert!(is_stylesheet_or_alternate(value.as_bytes()), "{value}");
        }
        for value in ["alternate", "prev", ""] {
            assert!(!is_stylesheet_or_alternate(value.as_bytes()), "{value}");
        }
    }

    #[test]
    fn alternate_stylesheet_matrix() {
        for value in [
            "alternate stylesheet",
            "stylesheet alternate",
            "stylesheet alternate canonical",
            "StyleshEet alternAte canoNical ",
        ] {
            assert!(is_alternate_stylesheet(value.as_bytes()), "{value}");
        }
        for value in [
            "stylesheet",
            "canonical stylesheet",
            " stylesheet",
            " styleSheet",
            "alternate",
            "prev",
            "",
        ] {
            assert!(!is_alternate_stylesheet(value.as_bytes()), "{value}");
        }
    }
}
