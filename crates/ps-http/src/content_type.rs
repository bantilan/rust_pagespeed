//! Content types (`pagespeed/kernel/http/content_type.{h,cc}`).
//!
//! MIME table with canonical extensions, plus the classification
//! predicates the caching and rewriting layers consult.
#![forbid(unsafe_code)]

/// Content kind (`ContentType::Kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentTypeKind {
    Html,
    Xhtml,
    CeHtml,
    Javascript,
    Css,
    Text,
    Xml,
    Png,
    Gif,
    Jpeg,
    Swf,
    Webp,
    Avif,
    Ico,
    Json,
    SourceMap,
    Pdf,
    Video,
    Audio,
    OctetStream,
    Other,
}

/// One MIME table row: canonical MIME, extension, kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentType {
    mime: &'static str,
    extension: &'static str,
    kind: ContentTypeKind,
}

impl ContentType {
    /// Canonical MIME type.
    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        self.mime
    }

    /// Canonical file extension (with dot).
    #[must_use]
    pub const fn extension(self) -> &'static str {
        self.extension
    }

    /// Content kind.
    #[must_use]
    pub const fn kind(self) -> ContentTypeKind {
        self.kind
    }

    /// HTML or XHTML (`IsHtmlLike`).
    #[must_use]
    pub const fn is_html_like(self) -> bool {
        matches!(
            self.kind,
            ContentTypeKind::Html | ContentTypeKind::Xhtml | ContentTypeKind::CeHtml
        )
    }

    /// XML flavors (`IsXmlLike`).
    #[must_use]
    pub const fn is_xml_like(self) -> bool {
        matches!(
            self.kind,
            ContentTypeKind::Xhtml | ContentTypeKind::CeHtml | ContentTypeKind::Xml
        )
    }

    /// JavaScript or JSON (`IsJsLike`).
    #[must_use]
    pub const fn is_js_like(self) -> bool {
        matches!(
            self.kind,
            ContentTypeKind::Javascript | ContentTypeKind::Json
        )
    }

    /// CSS (`IsCss`).
    #[must_use]
    pub const fn is_css(self) -> bool {
        matches!(self.kind, ContentTypeKind::Css)
    }

    /// Raster images (`IsImage`).
    #[must_use]
    pub const fn is_image(self) -> bool {
        matches!(
            self.kind,
            ContentTypeKind::Png
                | ContentTypeKind::Gif
                | ContentTypeKind::Jpeg
                | ContentTypeKind::Webp
                | ContentTypeKind::Avif
        )
    }

    /// Video (`IsVideo`).
    #[must_use]
    pub const fn is_video(self) -> bool {
        matches!(self.kind, ContentTypeKind::Video)
    }

    /// Audio (`IsAudio`).
    #[must_use]
    pub const fn is_audio(self) -> bool {
        matches!(self.kind, ContentTypeKind::Audio)
    }

    /// Flash (`IsFlash`).
    #[must_use]
    pub const fn is_flash(self) -> bool {
        matches!(self.kind, ContentTypeKind::Swf)
    }

    /// Compressible text-ish types (`IsCompressible`).
    #[must_use]
    pub const fn is_compressible(self) -> bool {
        self.is_xml_like()
            || self.is_html_like()
            || self.is_js_like()
            || self.is_css()
            || matches!(self.kind, ContentTypeKind::Text)
    }

    /// Types heuristics treat as static (`IsLikelyStaticResource`).
    #[must_use]
    pub const fn is_likely_static_resource(self) -> bool {
        matches!(
            self.kind,
            ContentTypeKind::Css
                | ContentTypeKind::Gif
                | ContentTypeKind::Ico
                | ContentTypeKind::Javascript
                | ContentTypeKind::Jpeg
                | ContentTypeKind::Pdf
                | ContentTypeKind::Png
                | ContentTypeKind::Swf
                | ContentTypeKind::Video
                | ContentTypeKind::Audio
                | ContentTypeKind::Webp
                | ContentTypeKind::Avif
        )
    }
}

/// MIME table (`kTypes`), canonical entries first, then synonyms.
pub const TYPES: &[ContentType] = &[
    ContentType {
        mime: "text/html",
        extension: ".html",
        kind: ContentTypeKind::Html,
    },
    ContentType {
        mime: "application/xhtml+xml",
        extension: ".xhtml",
        kind: ContentTypeKind::Xhtml,
    },
    ContentType {
        mime: "application/ce-html+xml",
        extension: ".xhtml",
        kind: ContentTypeKind::CeHtml,
    },
    ContentType {
        mime: "application/javascript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/css",
        extension: ".css",
        kind: ContentTypeKind::Css,
    },
    ContentType {
        mime: "text/plain",
        extension: ".txt",
        kind: ContentTypeKind::Text,
    },
    ContentType {
        mime: "text/xml",
        extension: ".xml",
        kind: ContentTypeKind::Xml,
    },
    ContentType {
        mime: "image/png",
        extension: ".png",
        kind: ContentTypeKind::Png,
    },
    ContentType {
        mime: "image/gif",
        extension: ".gif",
        kind: ContentTypeKind::Gif,
    },
    ContentType {
        mime: "image/jpeg",
        extension: ".jpg",
        kind: ContentTypeKind::Jpeg,
    },
    ContentType {
        mime: "application/x-shockwave-flash",
        extension: ".swf",
        kind: ContentTypeKind::Swf,
    },
    ContentType {
        mime: "image/webp",
        extension: ".webp",
        kind: ContentTypeKind::Webp,
    },
    ContentType {
        mime: "image/avif",
        extension: ".avif",
        kind: ContentTypeKind::Avif,
    },
    ContentType {
        mime: "image/x-icon",
        extension: ".ico",
        kind: ContentTypeKind::Ico,
    },
    ContentType {
        mime: "application/javascript",
        extension: ".json",
        kind: ContentTypeKind::Json,
    },
    ContentType {
        mime: "application/javascript",
        extension: ".map",
        kind: ContentTypeKind::SourceMap,
    },
    ContentType {
        mime: "application/pdf",
        extension: ".pdf",
        kind: ContentTypeKind::Pdf,
    },
    ContentType {
        mime: "application/octet-stream",
        extension: ".bin",
        kind: ContentTypeKind::OctetStream,
    },
    ContentType {
        mime: "image/svg+xml",
        extension: ".svg",
        kind: ContentTypeKind::Xml,
    },
    ContentType {
        mime: "application/x-javascript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/javascript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/x-javascript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/ecmascript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/js",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/jscript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "text/x-js",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "application/ecmascript",
        extension: ".js",
        kind: ContentTypeKind::Javascript,
    },
    ContentType {
        mime: "application/json",
        extension: ".json",
        kind: ContentTypeKind::Json,
    },
    ContentType {
        mime: "application/x-json",
        extension: ".json",
        kind: ContentTypeKind::Json,
    },
    ContentType {
        mime: "image/jpeg",
        extension: ".jpeg",
        kind: ContentTypeKind::Jpeg,
    },
    ContentType {
        mime: "image/jpg",
        extension: ".jpg",
        kind: ContentTypeKind::Jpeg,
    },
    ContentType {
        mime: "image/vnd.microsoft.icon",
        extension: ".ico",
        kind: ContentTypeKind::Ico,
    },
    ContentType {
        mime: "text/html",
        extension: ".htm",
        kind: ContentTypeKind::Html,
    },
    ContentType {
        mime: "application/xml",
        extension: ".xml",
        kind: ContentTypeKind::Xml,
    },
    ContentType {
        mime: "video/mpeg",
        extension: ".mpg",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/mp4",
        extension: ".mp4",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/3gp",
        extension: ".3gp",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/x-flv",
        extension: ".flv",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/ogg",
        extension: ".ogg",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/webm",
        extension: ".webm",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/x-ms-asf",
        extension: ".asf",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/x-ms-wmv",
        extension: ".wmv",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/quicktime",
        extension: ".mov",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "video/mpeg4",
        extension: ".mp4",
        kind: ContentTypeKind::Video,
    },
    ContentType {
        mime: "audio/mpeg",
        extension: ".mp3",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/ogg",
        extension: ".ogg",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/webm",
        extension: ".webm",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/mp4",
        extension: ".mp4",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/x-mpeg",
        extension: ".mp3",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/x-wav",
        extension: ".wav",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/mp3",
        extension: ".mp3",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "audio/wav",
        extension: ".wav",
        kind: ContentTypeKind::Audio,
    },
    ContentType {
        mime: "binary/octet-stream",
        extension: ".bin",
        kind: ContentTypeKind::OctetStream,
    },
];

/// Maximum produced extension length (`.avif`).
pub const MAX_EXTENSION_LEN: usize = 5;

/// Looks up a MIME type, stripping `;...` parameters first
/// (`MimeTypeToContentType`, case-insensitive, first table hit wins).
#[must_use]
pub fn mime_to_content_type(mime: &str) -> Option<ContentType> {
    let bare = mime.split(';').next().unwrap_or(mime);
    TYPES
        .iter()
        .find(|entry| entry.mime.eq_ignore_ascii_case(bare))
        .copied()
}

/// Looks up an extension (`NameExtensionToContentType`, case-insensitive).
#[must_use]
pub fn extension_to_content_type(name: &str) -> Option<ContentType> {
    let dot = name.rfind('.')?;
    let ext = &name[dot..];
    TYPES
        .iter()
        .find(|entry| entry.extension.eq_ignore_ascii_case(ext))
        .copied()
}

/// Splits a Content-Type value into MIME and charset
/// (`ParseContentType`): MIME is the pre-`;` part (untrimmed!), charset
/// the first `charset=` parameter found (trimmed). Fails on empty input
/// and when both parts come out empty.
#[must_use]
pub fn parse_content_type(value: &str) -> Option<(String, String)> {
    if value.is_empty() {
        return None;
    }
    let mut parts = value.split(';');
    let mime = parts.next().unwrap_or("").to_string();
    let mut charset = String::new();
    for part in parts {
        let mut pair = part.splitn(2, '=');
        let (Some(name), Some(val)) = (pair.next(), pair.next()) else {
            continue;
        };
        // C++ requires exactly 2 '='-segments; `splitn(2)` plus a check
        // that the value holds no further '='.
        if val.contains('=') {
            continue;
        }
        if name.trim().eq_ignore_ascii_case("charset") {
            charset = val.trim().to_string();
            break;
        }
    }
    if mime.is_empty() && charset.is_empty() {
        return None;
    }
    Some((mime, charset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_and_extension_lookup() {
        let html = mime_to_content_type("text/html").unwrap();
        assert_eq!(html.kind(), ContentTypeKind::Html);
        assert_eq!(html.extension(), ".html");
        // Parameters stripped, case-insensitive, first hit wins.
        assert_eq!(
            mime_to_content_type("TEXT/HTML; charset=utf-8")
                .unwrap()
                .kind(),
            ContentTypeKind::Html
        );
        assert_eq!(
            mime_to_content_type("text/javascript").unwrap().kind(),
            ContentTypeKind::Javascript
        );
        assert_eq!(mime_to_content_type("bogus/type"), None);
        assert_eq!(
            extension_to_content_type("a.JS").unwrap().kind(),
            ContentTypeKind::Javascript
        );
        assert_eq!(extension_to_content_type("noext"), None);
    }

    #[test]
    fn predicates() {
        let html = mime_to_content_type("text/html").unwrap();
        assert!(html.is_html_like());
        assert!(html.is_compressible());
        assert!(!html.is_likely_static_resource());
        let css = mime_to_content_type("text/css").unwrap();
        assert!(css.is_compressible() && css.is_likely_static_resource());
        let png = mime_to_content_type("image/png").unwrap();
        assert!(png.is_image() && png.is_likely_static_resource());
        assert!(!png.is_compressible());
        let json = mime_to_content_type("application/json").unwrap();
        assert!(json.is_js_like() && !json.is_likely_static_resource());
    }

    #[test]
    fn content_type_parsing() {
        assert_eq!(
            parse_content_type("text/html; charset=UTF-8"),
            Some(("text/html".to_string(), "UTF-8".to_string()))
        );
        // MIME kept verbatim (untrimmed); first charset wins.
        assert_eq!(
            parse_content_type("text/html ; charset=a; charset=b"),
            Some(("text/html ".to_string(), "a".to_string()))
        );
        assert_eq!(
            parse_content_type("text/html"),
            Some(("text/html".to_string(), String::new()))
        );
        assert_eq!(parse_content_type(""), None);
        // Degenerate inputs with neither part fail like the C++.
        assert_eq!(parse_content_type(";"), None);
        assert_eq!(parse_content_type("; charset="), None);
    }
}
