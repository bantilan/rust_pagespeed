//! HTML doctype recognition.
//!
//! Ports `DocType`: classification of doctype directives into HTML/XHTML
//! versions, including the shell-like tokenizer and the exact known-doctype
//! strings.

use crate::keywords::is_html_space;

/// Recognized doctype (`DocType::DocTypeEnum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocType {
    #[default]
    Unknown,
    Html5,
    Html4Strict,
    Html4Transitional,
    Xhtml5,
    Xhtml11,
    Xhtml10Strict,
    Xhtml10Transitional,
    OtherXhtml,
}

impl DocType {
    #[must_use]
    pub fn is_xhtml(self) -> bool {
        matches!(
            self,
            DocType::Xhtml5
                | DocType::Xhtml11
                | DocType::Xhtml10Strict
                | DocType::Xhtml10Transitional
                | DocType::OtherXhtml
        )
    }

    #[must_use]
    pub fn is_version_5(self) -> bool {
        matches!(self, DocType::Html5 | DocType::Xhtml5)
    }

    /// Parses a directive's contents (`DocType::Parse`). Returns false (and
    /// leaves `self` untouched) when the directive is not a doctype; returns
    /// true after classifying (possibly as [`DocType::Unknown`]).
    ///
    /// `content_is_xml_like` stands in for `ContentType::IsXmlLike` until
    /// the content-type layer lands.
    pub fn parse(&mut self, directive: &[u8], content_is_xml_like: bool) -> bool {
        if directive.len() < 7 || !directive[..7].eq_ignore_ascii_case(b"doctype") {
            return false;
        }
        let parts = parse_shell_like_string(directive);
        if parts.is_empty() || !parts[0].eq_ignore_ascii_case(b"doctype") {
            return false;
        }
        *self = DocType::Unknown;
        if parts.len() >= 2 && parts[1].eq_ignore_ascii_case(b"html") {
            if parts.len() == 2 {
                *self = if content_is_xml_like {
                    DocType::Xhtml5
                } else {
                    DocType::Html5
                };
            } else if parts.len() == 5 && parts[2].eq_ignore_ascii_case(b"public") {
                let id = &parts[3];
                if id.starts_with(b"-//W3C//DTD XHTML") {
                    *self = if id == b"-//W3C//DTD XHTML 1.1//EN" {
                        DocType::Xhtml11
                    } else if id == b"-//W3C//DTD XHTML 1.0 Strict//EN" {
                        DocType::Xhtml10Strict
                    } else if id == b"-//W3C//DTD XHTML 1.0 Transitional//EN" {
                        DocType::Xhtml10Transitional
                    } else {
                        DocType::OtherXhtml
                    };
                } else if id == b"-//W3C//DTD HTML 4.01//EN" {
                    *self = DocType::Html4Strict;
                } else if id == b"-//W3C//DTD HTML 4.01 Transitional//EN" {
                    *self = DocType::Html4Transitional;
                }
            }
        }
        true
    }
}

/// Splits on HTML whitespace with single/double-quote grouping and
/// backslash escapes (`ParseShellLikeString`).
fn parse_shell_like_string(input: &[u8]) -> Vec<Vec<u8>> {
    let mut parts = Vec::new();
    let mut index = 0;
    while index < input.len() {
        let byte = input[index];
        if byte == b'"' || byte == b'\'' {
            let quote = byte;
            index += 1;
            let mut part = Vec::new();
            while index < input.len() && input[index] != quote {
                if input[index] == b'\\' {
                    index += 1;
                    if index >= input.len() {
                        break;
                    }
                }
                part.push(input[index]);
                index += 1;
            }
            index += 1;
            parts.push(part);
        } else if !is_html_space(byte) {
            let mut part = Vec::new();
            while index < input.len() && !is_html_space(input[index]) {
                part.push(input[index]);
                index += 1;
            }
            parts.push(part);
        } else {
            index += 1;
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html5_doctype() {
        let mut doctype = DocType::default();
        assert!(doctype.parse(b"DOCTYPE html", false));
        assert_eq!(doctype, DocType::Html5);
        assert!(!doctype.is_xhtml() && doctype.is_version_5());
        let mut doctype = DocType::default();
        assert!(doctype.parse(b"DOCTYPE html", true));
        assert_eq!(doctype, DocType::Xhtml5);
    }

    #[test]
    fn legacy_doctypes() {
        let mut doctype = DocType::default();
        assert!(doctype.parse(
            b"DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\"",
            false
        ));
        assert_eq!(doctype, DocType::Xhtml10Strict);
        assert!(doctype.is_xhtml() && !doctype.is_version_5());

        let mut doctype = DocType::default();
        assert!(doctype.parse(
            b"DOCTYPE html PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \"http://www.w3.org/TR/html4/loose.dtd\"",
            false
        ));
        assert_eq!(doctype, DocType::Html4Transitional);

        let mut doctype = DocType::default();
        assert!(doctype.parse(
            b"DOCTYPE html PUBLIC \"-//W3C//DTD XHTML+RDFa 1.0//EN\" \"x\"",
            false
        ));
        assert_eq!(doctype, DocType::OtherXhtml);
    }

    #[test]
    fn non_doctype_untouched() {
        let mut doctype = DocType::Html5;
        assert!(!doctype.parse(b"doctypex html", false));
        assert_eq!(doctype, DocType::Html5);
        assert!(!doctype.parse(b"xml version=\"1.0\"", false));
        // Unknown doctype shape still returns true, classified unknown.
        let mut doctype = DocType::Html5;
        assert!(doctype.parse(b"DOCTYPE something-else", false));
        assert_eq!(doctype, DocType::Unknown);
    }
}
