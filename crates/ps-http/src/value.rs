//! Cache value format (`http_value.{h,cc}`).
//!
//! Wire shape: `[type:1][first_len:u32le][first_chunk][second_chunk]`
//! with `type` = `h` (headers first) or `b` (body first). Headers
//! serialize via the protobuf-compatible codec; bodies stream verbatim.
//! `Decode` is total: malformed blobs fail instead of panicking.
#![forbid(unsafe_code)]

use crate::response::ResponseHeaders;

/// Value with headers first (`kHeaders` = `h`).
pub const HEADERS_FIRST: u8 = b'h';
/// Value with body first (`kBody` = `b`).
pub const BODY_FIRST: u8 = b'b';

/// A cacheable headers+body pair (`HTTPValue`).
#[derive(Debug, Clone, Default)]
pub struct HttpValue {
    headers: ResponseHeaders,
    body: Vec<u8>,
}

impl HttpValue {
    /// Creates an empty value.
    #[must_use]
    pub fn new() -> HttpValue {
        HttpValue::default()
    }

    /// Borrows the headers.
    #[must_use]
    pub fn headers(&self) -> &ResponseHeaders {
        &self.headers
    }

    /// Mutably borrows the headers.
    pub fn headers_mut(&mut self) -> &mut ResponseHeaders {
        &mut self.headers
    }

    /// Borrows the body.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Appends body bytes (`Write`).
    pub fn write_body(&mut self, bytes: &[u8]) {
        self.body.extend_from_slice(bytes);
    }

    /// Sets headers wholesale (`SetHeaders`).
    pub fn set_headers(&mut self, headers: ResponseHeaders) {
        self.headers = headers;
    }

    /// Encodes with headers first (`Encode`).
    #[must_use]
    pub fn encode(&mut self) -> Vec<u8> {
        let first = self.headers.write_as_binary();
        frame(HEADERS_FIRST, &first, &self.body)
    }

    /// Encodes with the body first (`Encode` + `body_first`).
    #[must_use]
    pub fn encode_body_first(&mut self) -> Vec<u8> {
        let second = self.headers.write_as_binary();
        frame(BODY_FIRST, &self.body, &second)
    }

    /// Decodes a blob (`Decode`); malformed input errors.
    pub fn decode(&mut self, blob: &[u8]) -> Result<(), String> {
        if blob.len() < 5 {
            return Err("blob too short".to_string());
        }
        let kind = blob[0];
        if kind != HEADERS_FIRST && kind != BODY_FIRST {
            return Err(format!("bad type byte: {kind}"));
        }
        let first_len = u32::from_le_bytes(
            blob[1..5]
                .try_into()
                .map_err(|_| "short length".to_string())?,
        ) as usize;
        if blob.len() < 5 + first_len {
            return Err("blob truncated".to_string());
        }
        let first = &blob[5..5 + first_len];
        let second = &blob[5 + first_len..];
        if kind == HEADERS_FIRST {
            self.headers.read_from_binary(first)?;
            self.body = second.to_vec();
        } else {
            self.body = first.to_vec();
            self.headers.read_from_binary(second)?;
        }
        Ok(())
    }

    /// Extracts into headers + body sink (`Extract`).
    pub fn extract_into(
        blob: &[u8],
        headers: &mut ResponseHeaders,
        body: &mut Vec<u8>,
    ) -> Result<(), String> {
        let mut value = HttpValue::new();
        value.decode(blob)?;
        *headers = value.headers;
        body.clear();
        body.extend_from_slice(&value.body);
        Ok(())
    }
}

fn frame(kind: u8, first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + first.len() + second.len());
    out.push(kind);
    out.extend_from_slice(&(first.len() as u32).to_le_bytes());
    out.extend_from_slice(first);
    out.extend_from_slice(second);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::HttpOptions;

    fn sample() -> HttpValue {
        let mut value = HttpValue::new();
        value.headers = crate::response::ResponseHeaders::new(HttpOptions::default_for_tests());
        value.headers.set_first_line(1, 1, 200, "OK");
        value.headers.headers_mut().add("Content-Type", "text/html");
        value.write_body(b"<html>");
        value
    }

    #[test]
    fn round_trips_both_orders() {
        for body_first in [false, true] {
            let mut value = sample();
            let blob = if body_first {
                value.encode_body_first()
            } else {
                value.encode()
            };
            assert_eq!(
                blob[0],
                if body_first {
                    BODY_FIRST
                } else {
                    HEADERS_FIRST
                }
            );
            let mut back = HttpValue::new();
            back.decode(&blob).unwrap();
            assert_eq!(back.headers().status_code(), 200);
            assert_eq!(back.body(), b"<html>");
            // Re-encoding the decoded value reproduces the blob.
            let mut back2 = back.clone();
            let blob2 = if body_first {
                back2.encode_body_first()
            } else {
                back2.encode()
            };
            assert_eq!(blob, blob2);
        }
    }

    #[test]
    fn malformed_blobs_fail() {
        let mut value = HttpValue::new();
        assert!(value.decode(b"").is_err());
        assert!(value.decode(b"h\x01\x00").is_err()); // short length
        assert!(value.decode(b"x\x00\x00\x00\x00").is_err()); // bad type
        assert!(value.decode(b"h\xff\x00\x00\x00").is_err()); // truncated
        assert!(value.decode(b"h\x01\x00\x00\x00\xff").is_err()); // bad proto
    }
}
