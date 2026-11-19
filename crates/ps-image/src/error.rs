//! Status codes for image decoding and encoding.
//!
//! Port of `pagespeed/kernel/image/scanline_status.h`: the same status
//! vocabulary (`Success`, `ParseError`, `Timeout`, ...), expressed as a Rust
//! error type instead of an out-param struct.

use std::fmt;

/// Where an image pipeline failure originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    Unknown,
    PngReader,
    GifReader,
    JpegReader,
    WebpReader,
    Resizer,
    PngWriter,
    JpegWriter,
    WebpWriter,
    Util,
    AvifEncoder,
}

impl fmt::Display for ErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            ErrorSource::Unknown => "unknown",
            ErrorSource::PngReader => "png_reader",
            ErrorSource::GifReader => "gif_reader",
            ErrorSource::JpegReader => "jpeg_reader",
            ErrorSource::WebpReader => "webp_reader",
            ErrorSource::Resizer => "resizer",
            ErrorSource::PngWriter => "png_writer",
            ErrorSource::JpegWriter => "jpeg_writer",
            ErrorSource::WebpWriter => "webp_writer",
            ErrorSource::Util => "util",
            ErrorSource::AvifEncoder => "avif_encoder",
        };
        f.write_str(name)
    }
}

/// Image pipeline failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    source: ErrorSource,
    details: String,
}

/// Failure kinds, matching `ScanlineStatusType` one-for-one (minus the
/// `Success`/`Uninitialized` sentinel states, which have no error form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Codec or container the pipeline cannot handle.
    UnsupportedFormat,
    /// Valid container, feature the pipeline cannot handle (e.g. CMYK JPEG).
    UnsupportedFeature,
    /// Truncated or corrupt input bytes.
    ParseError,
    /// Allocation or output-buffer failure.
    MemoryError,
    /// Internal invariant violation (a bug, never bad input).
    InternalError,
    /// Conversion exceeded its time budget.
    Timeout,
    /// Caller passed inconsistent arguments (empty image, zero size...).
    InvocationError,
    /// External encoder (avifenc) missing, failed, or killed.
    EncoderError,
}

impl Error {
    #[must_use]
    pub fn new(kind: ErrorKind, source: ErrorSource, details: impl Into<String>) -> Self {
        Error {
            kind,
            source,
            details: details.into(),
        }
    }

    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub fn source(&self) -> ErrorSource {
        self.source
    }

    #[must_use]
    pub fn details(&self) -> &str {
        &self.details
    }

    pub(crate) fn parse(source: ErrorSource, details: impl Into<String>) -> Self {
        Error::new(ErrorKind::ParseError, source, details)
    }

    pub(crate) fn unsupported_format(details: impl Into<String>) -> Self {
        Error::new(ErrorKind::UnsupportedFormat, ErrorSource::Util, details)
    }

    pub(crate) fn unsupported_feature(source: ErrorSource, details: impl Into<String>) -> Self {
        Error::new(ErrorKind::UnsupportedFeature, source, details)
    }

    pub(crate) fn internal(source: ErrorSource, details: impl Into<String>) -> Self {
        Error::new(ErrorKind::InternalError, source, details)
    }

    pub(crate) fn invocation(source: ErrorSource, details: impl Into<String>) -> Self {
        Error::new(ErrorKind::InvocationError, source, details)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} in {}: {}", self.kind, self.source, self.details)
    }
}

impl std::error::Error for Error {}

/// Pipeline result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_source_and_kind() {
        let err = Error::parse(ErrorSource::JpegReader, "truncated SOF");
        assert_eq!(err.kind(), ErrorKind::ParseError);
        assert_eq!(err.source(), ErrorSource::JpegReader);
        let text = err.to_string();
        assert!(text.contains("jpeg_reader"), "{text}");
        assert!(text.contains("truncated SOF"), "{text}");
    }
}
