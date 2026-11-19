//! Minimal protobuf wire codec for `HttpResponseHeaders`
//! (`pagespeed/kernel/http/http.proto`).
//!
//! Supports exactly what the message needs: varint fields (int32/int64/
//! bool), length-delimited strings and embedded `NameValue` messages,
//! field presence, and unknown-field skipping. Byte-compatible with
//! protobuf serialization in both directions, so Rust and C++ nodes
//! share cache blobs.
#![forbid(unsafe_code)]

/// Wire types used by the schema.
const WIRE_VARINT: u8 = 0;
const WIRE_LENGTH_DELIMITED: u8 = 2;

/// Field numbers of `HttpResponseHeaders`.
pub const F_STATUS_CODE: u32 = 1;
pub const F_REASON_PHRASE: u32 = 2;
pub const F_MINOR_VERSION: u32 = 3;
pub const F_MAJOR_VERSION: u32 = 4;
pub const F_EXPIRATION_TIME_MS: u32 = 5;
pub const F_DATE_MS: u32 = 6;
pub const F_BROWSER_CACHEABLE: u32 = 7;
pub const F_PROXY_CACHEABLE: u32 = 8;
pub const F_HEADER: u32 = 9;
pub const F_LAST_MODIFIED_TIME_MS: u32 = 10;
pub const F_CACHE_TTL_MS: u32 = 11;
pub const F_IS_IMPLICITLY_CACHEABLE: u32 = 12;
pub const F_REQUIRES_BROWSER_REVALIDATION: u32 = 13;
pub const F_REQUIRES_PROXY_REVALIDATION: u32 = 14;

/// Field numbers of `NameValue`.
pub const F_NV_NAME: u32 = 1;
pub const F_NV_VALUE: u32 = 2;

/// Decoded `HttpResponseHeaders` with presence bits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResponseProto {
    /// Field 1, int32.
    pub status_code: Option<i32>,
    /// Field 2.
    pub reason_phrase: Option<String>,
    /// Field 3, int32.
    pub minor_version: Option<i32>,
    /// Field 4, int32 (default 1 when absent).
    pub major_version: Option<i32>,
    /// Field 5, int64.
    pub expiration_time_ms: Option<i64>,
    /// Field 6, int64.
    pub date_ms: Option<i64>,
    /// Field 7.
    pub browser_cacheable: Option<bool>,
    /// Field 8.
    pub proxy_cacheable: Option<bool>,
    /// Field 9, repeated, in order.
    pub headers: Vec<(String, String)>,
    /// Field 10, int64.
    pub last_modified_time_ms: Option<i64>,
    /// Field 11, int64.
    pub cache_ttl_ms: Option<i64>,
    /// Field 12.
    pub is_implicitly_cacheable: Option<bool>,
    /// Field 13.
    pub requires_browser_revalidation: Option<bool>,
    /// Field 14.
    pub requires_proxy_revalidation: Option<bool>,
}

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn tag(field: u32, wire: u8, out: &mut Vec<u8>) {
    encode_varint(u64::from(field) << 3 | u64::from(wire), out);
}

fn encode_int32(field: u32, value: i32, out: &mut Vec<u8>) {
    // int32 encodes as sign-extended 64-bit varint.
    tag(field, WIRE_VARINT, out);
    encode_varint(value as i64 as u64, out);
}

fn encode_int64(field: u32, value: i64, out: &mut Vec<u8>) {
    tag(field, WIRE_VARINT, out);
    encode_varint(value as u64, out);
}

fn encode_bool(field: u32, value: bool, out: &mut Vec<u8>) {
    tag(field, WIRE_VARINT, out);
    out.push(u8::from(value));
}

fn encode_string(field: u32, value: &str, out: &mut Vec<u8>) {
    tag(field, WIRE_LENGTH_DELIMITED, out);
    encode_varint(value.len() as u64, out);
    out.extend_from_slice(value.as_bytes());
}

/// Serializes in field-number order (protobuf canonical order).
#[must_use]
pub fn encode_response(proto: &ResponseProto) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(value) = proto.status_code {
        encode_int32(F_STATUS_CODE, value, &mut out);
    }
    if let Some(value) = &proto.reason_phrase {
        encode_string(F_REASON_PHRASE, value, &mut out);
    }
    if let Some(value) = proto.minor_version {
        encode_int32(F_MINOR_VERSION, value, &mut out);
    }
    if let Some(value) = proto.major_version {
        encode_int32(F_MAJOR_VERSION, value, &mut out);
    }
    if let Some(value) = proto.expiration_time_ms {
        encode_int64(F_EXPIRATION_TIME_MS, value, &mut out);
    }
    if let Some(value) = proto.date_ms {
        encode_int64(F_DATE_MS, value, &mut out);
    }
    if let Some(value) = proto.browser_cacheable {
        encode_bool(F_BROWSER_CACHEABLE, value, &mut out);
    }
    if let Some(value) = proto.proxy_cacheable {
        encode_bool(F_PROXY_CACHEABLE, value, &mut out);
    }
    for (name, value) in &proto.headers {
        let mut nested = Vec::new();
        encode_string(F_NV_NAME, name, &mut nested);
        encode_string(F_NV_VALUE, value, &mut nested);
        tag(F_HEADER, WIRE_LENGTH_DELIMITED, &mut out);
        encode_varint(nested.len() as u64, &mut out);
        out.extend_from_slice(&nested);
    }
    if let Some(value) = proto.last_modified_time_ms {
        encode_int64(F_LAST_MODIFIED_TIME_MS, value, &mut out);
    }
    if let Some(value) = proto.cache_ttl_ms {
        encode_int64(F_CACHE_TTL_MS, value, &mut out);
    }
    if let Some(value) = proto.is_implicitly_cacheable {
        encode_bool(F_IS_IMPLICITLY_CACHEABLE, value, &mut out);
    }
    if let Some(value) = proto.requires_browser_revalidation {
        encode_bool(F_REQUIRES_BROWSER_REVALIDATION, value, &mut out);
    }
    if let Some(value) = proto.requires_proxy_revalidation {
        encode_bool(F_REQUIRES_PROXY_REVALIDATION, value, &mut out);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeError {
    Truncated,
    VarintOverflow,
    BadWireType,
    BadUtf8,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let mut shift = 0;
        let mut value: u64 = 0;
        loop {
            if self.pos >= self.bytes.len() {
                return Err(DecodeError::Truncated);
            }
            if shift >= 64 {
                return Err(DecodeError::VarintOverflow);
            }
            let byte = self.bytes[self.pos];
            self.pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < len {
            return Err(DecodeError::Truncated);
        }
        let slice = &self.bytes[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn skip_field(&mut self, wire: u8) -> Result<(), DecodeError> {
        match wire {
            WIRE_VARINT => {
                self.read_varint()?;
                Ok(())
            }
            1 => {
                self.read_bytes(8)?;
                Ok(())
            }
            WIRE_LENGTH_DELIMITED => {
                let len = self.read_varint()? as usize;
                self.read_bytes(len)?;
                Ok(())
            }
            5 => {
                self.read_bytes(4)?;
                Ok(())
            }
            _ => Err(DecodeError::BadWireType),
        }
    }
}

fn decode_nested(bytes: &[u8]) -> Result<(String, String), DecodeError> {
    let mut cursor = Cursor::new(bytes);
    let mut name = None;
    let mut value = None;
    while cursor.remaining() > 0 {
        let tag = cursor.read_varint()?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        if wire != WIRE_LENGTH_DELIMITED {
            cursor.skip_field(wire)?;
            continue;
        }
        let len = cursor.read_varint()? as usize;
        let text =
            std::str::from_utf8(cursor.read_bytes(len)?).map_err(|_| DecodeError::BadUtf8)?;
        if field == F_NV_NAME {
            name = Some(text.to_string());
        } else if field == F_NV_VALUE {
            value = Some(text.to_string());
        }
    }
    // Both fields are `required` in the schema.
    match (name, value) {
        (Some(name), Some(value)) => Ok((name, value)),
        _ => Err(DecodeError::Truncated),
    }
}

/// Decodes, tolerating unknown fields and any field order (last
/// scalar occurrence wins, headers append in order).
pub fn decode_response(bytes: &[u8]) -> Result<ResponseProto, String> {
    let mut cursor = Cursor::new(bytes);
    let mut proto = ResponseProto::default();
    while cursor.remaining() > 0 {
        let tag = cursor.read_varint().map_err(|err| format!("{err:?}"))?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        match (field, wire) {
            (F_STATUS_CODE, WIRE_VARINT) => {
                proto.status_code =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i32);
            }
            (F_REASON_PHRASE, WIRE_LENGTH_DELIMITED) => {
                let len = cursor.read_varint().map_err(|err| format!("{err:?}"))? as usize;
                let text = cursor.read_bytes(len).map_err(|err| format!("{err:?}"))?;
                proto.reason_phrase = Some(
                    std::str::from_utf8(text)
                        .map_err(|_| "BadUtf8".to_string())?
                        .to_string(),
                );
            }
            (F_MINOR_VERSION, WIRE_VARINT) => {
                proto.minor_version =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i32);
            }
            (F_MAJOR_VERSION, WIRE_VARINT) => {
                proto.major_version =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i32);
            }
            (F_EXPIRATION_TIME_MS, WIRE_VARINT) => {
                proto.expiration_time_ms =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i64);
            }
            (F_DATE_MS, WIRE_VARINT) => {
                proto.date_ms =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i64);
            }
            (F_BROWSER_CACHEABLE, WIRE_VARINT)
            | (F_PROXY_CACHEABLE, WIRE_VARINT)
            | (F_IS_IMPLICITLY_CACHEABLE, WIRE_VARINT)
            | (F_REQUIRES_BROWSER_REVALIDATION, WIRE_VARINT)
            | (F_REQUIRES_PROXY_REVALIDATION, WIRE_VARINT) => {
                let value = cursor.read_varint().map_err(|err| format!("{err:?}"))? != 0;
                match field {
                    F_BROWSER_CACHEABLE => proto.browser_cacheable = Some(value),
                    F_PROXY_CACHEABLE => proto.proxy_cacheable = Some(value),
                    F_IS_IMPLICITLY_CACHEABLE => {
                        proto.is_implicitly_cacheable = Some(value);
                    }
                    F_REQUIRES_BROWSER_REVALIDATION => {
                        proto.requires_browser_revalidation = Some(value);
                    }
                    _ => proto.requires_proxy_revalidation = Some(value),
                }
            }
            (F_HEADER, WIRE_LENGTH_DELIMITED) => {
                let len = cursor.read_varint().map_err(|err| format!("{err:?}"))? as usize;
                let nested = cursor.read_bytes(len).map_err(|err| format!("{err:?}"))?;
                proto
                    .headers
                    .push(decode_nested(nested).map_err(|err| format!("{err:?}"))?);
            }
            (F_LAST_MODIFIED_TIME_MS, WIRE_VARINT) => {
                proto.last_modified_time_ms =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i64);
            }
            (F_CACHE_TTL_MS, WIRE_VARINT) => {
                proto.cache_ttl_ms =
                    Some(cursor.read_varint().map_err(|err| format!("{err:?}"))? as i64);
            }
            _ => {
                cursor.skip_field(wire).map_err(|err| format!("{err:?}"))?;
            }
        }
    }
    Ok(proto)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_wire_bytes() {
        // Hand-computed protobuf encoding: status=200 (field 1, varint
        // 0xc8 0x01), one header (field 9, embedded "A: b").
        let proto = ResponseProto {
            status_code: Some(200),
            reason_phrase: Some("OK".to_string()),
            headers: vec![("A".to_string(), "b".to_string())],
            ..ResponseProto::default()
        };
        let bytes = encode_response(&proto);
        assert_eq!(
            bytes,
            vec![
                0x08, 0xc8, 0x01, // field 1, varint 200
                0x12, 0x02, b'O', b'K', // field 2, "OK"
                0x4a, 0x06, // field 9, length 6
                0x0a, 0x01, b'A', // field 1, "A"
                0x12, 0x01, b'b', // field 2, "b"
            ]
        );
        assert_eq!(decode_response(&bytes), Ok(proto.clone()));
    }

    #[test]
    fn scalar_round_trip_and_presence() {
        let proto = ResponseProto {
            minor_version: Some(1),
            major_version: Some(1),
            expiration_time_ms: Some(9_000_000),
            date_ms: Some(8_000_000),
            browser_cacheable: Some(true),
            proxy_cacheable: Some(false),
            requires_browser_revalidation: Some(false),
            requires_proxy_revalidation: Some(true),
            cache_ttl_ms: Some(300_000),
            ..ResponseProto::default()
        };
        let bytes = encode_response(&proto);
        assert_eq!(decode_response(&bytes), Ok(proto.clone()));
        // Absent fields stay absent (presence preserved).
        let empty = decode_response(&[]).unwrap();
        assert_eq!(empty, ResponseProto::default());
        // Unknown fields skipped (field 99, varint + embedded).
        let mut with_unknown = bytes.clone();
        with_unknown.extend_from_slice(&[0x98, 0x06, 0x05]); // field 99 varint
        with_unknown.extend_from_slice(&[0xa2, 0x06, 0x02, 0xaa, 0xbb]);
        assert_eq!(decode_response(&with_unknown), Ok(proto));
        // Truncation fails instead of panicking.
        assert!(decode_response(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode_response(&[0x08]).is_err());
    }
}
