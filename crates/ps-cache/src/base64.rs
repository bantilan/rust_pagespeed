//! Base64 codecs (`third_party/base64/base64.{h,cc}`).
//!
//! Standard padded base64 in two alphabets: MIME (`+/`) and web-safe
//! (`-_`). Decoding stops at the first `=` pad character, rejects bytes
//! outside the alphabet, and tolerates a single trailing dangling char
//! (yielding no output for it), exactly like the C++ `decode()`.
#![forbid(unsafe_code)]

const MIME_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const WEB64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const PAD: u8 = b'=';

/// Encodes bytes with an alphabet, with `=` padding (`encode()`).
fn encode_with(alphabet: &[u8; 64], input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (quanta, remainder) = input.as_chunks::<3>();
    for quantum in quanta {
        let n =
            (u32::from(quantum[0]) << 16) | (u32::from(quantum[1]) << 8) | u32::from(quantum[2]);
        out.push(alphabet[(n >> 18) as usize & 63] as char);
        out.push(alphabet[(n >> 12) as usize & 63] as char);
        out.push(alphabet[(n >> 6) as usize & 63] as char);
        out.push(alphabet[n as usize & 63] as char);
    }
    match remainder {
        [] => {}
        [a] => {
            let n = u32::from(*a) << 16;
            out.push(alphabet[(n >> 18) as usize & 63] as char);
            out.push(alphabet[(n >> 12) as usize & 63] as char);
            out.push(PAD as char);
            out.push(PAD as char);
        }
        [a, b] => {
            let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
            out.push(alphabet[(n >> 18) as usize & 63] as char);
            out.push(alphabet[(n >> 12) as usize & 63] as char);
            out.push(alphabet[(n >> 6) as usize & 63] as char);
            out.push(PAD as char);
        }
        _ => unreachable!("chunks_exact remainder is < 3 bytes"),
    }
    out
}

/// Decodes into bytes, returning `None` on alphabet violations (`decode()`).
///
/// Stops at the first pad character (trailing bytes ignored); a quantum of
/// 2 chars yields 1 byte, 3 chars yield 2 bytes, and a lone trailing char
/// yields nothing but still succeeds.
fn decode_with(alphabet: &[u8; 64], input: &str) -> Option<Vec<u8>> {
    let mut values = [0u8; 256];
    let mut present = [false; 256];
    for (i, byte) in alphabet.iter().enumerate() {
        values[*byte as usize] = i as u8;
        present[*byte as usize] = true;
    }
    let bytes = input.as_bytes();
    // Consume up to the first pad character.
    let mut quads: Vec<u8> = Vec::new();
    for byte in bytes {
        if *byte == PAD {
            break;
        }
        if !present[*byte as usize] {
            return None;
        }
        quads.push(values[*byte as usize]);
    }
    let mut out = Vec::with_capacity(quads.len() / 4 * 3 + 2);
    let (quanta, remainder) = quads.as_chunks::<4>();
    for quantum in quanta {
        let n = (u32::from(quantum[0]) << 18)
            | (u32::from(quantum[1]) << 12)
            | (u32::from(quantum[2]) << 6)
            | u32::from(quantum[3]);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
    }
    match remainder {
        [] => {}
        [_] => {} // dangling char: no output, still success.
        [a, b] => {
            let n = (u32::from(*a) << 18) | (u32::from(*b) << 12);
            out.push((n >> 16) as u8);
        }
        [a, b, c] => {
            let n = (u32::from(*a) << 18) | (u32::from(*b) << 12) | (u32::from(*c) << 6);
            out.push((n >> 16) as u8);
            out.push((n >> 8) as u8);
        }
        _ => unreachable!("chunks_exact remainder is < 4 entries"),
    }
    Some(out)
}

/// MIME base64 encode (`base64_encode`).
#[must_use]
pub fn mime_encode(input: &[u8]) -> String {
    encode_with(MIME_ALPHABET, input)
}

/// MIME base64 decode (`base64_decode`).
#[must_use]
pub fn mime_decode(input: &str) -> Option<Vec<u8>> {
    decode_with(MIME_ALPHABET, input)
}

/// Web-safe base64 encode (`web64_encode`).
#[must_use]
pub fn web64_encode(input: &[u8]) -> String {
    encode_with(WEB64_ALPHABET, input)
}

/// Web-safe base64 decode (`web64_decode`).
#[must_use]
pub fn web64_decode(input: &str) -> Option<Vec<u8>> {
    decode_with(WEB64_ALPHABET, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_vectors() {
        // RFC-style vectors adapted to both alphabets.
        assert_eq!(mime_encode(b""), "");
        assert_eq!(mime_encode(b"f"), "Zg==");
        assert_eq!(mime_encode(b"fo"), "Zm8=");
        assert_eq!(mime_encode(b"foo"), "Zm9v");
        assert_eq!(mime_encode(b"foob"), "Zm9vYg==");
        assert_eq!(mime_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(mime_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(web64_encode(b"foo"), "Zm9v");
        // Bytes 0xfb 0xff select '+/' in MIME, '-_' in web64.
        assert_eq!(mime_encode(&[0xfb, 0xff]), "+/8=");
        assert_eq!(web64_encode(&[0xfb, 0xff]), "-_8=");
        // 16-byte input (MD5 size) pads with "==".
        assert_eq!(web64_encode(&[0u8; 16]).len(), 24);
        assert!(web64_encode(&[0u8; 16]).ends_with("=="));
    }

    #[test]
    fn decode_round_trip_and_rejection() {
        for len in 0..40 {
            let input: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(mime_decode(&mime_encode(&input)), Some(input.clone()));
            assert_eq!(web64_decode(&web64_encode(&input)), Some(input));
        }
        // Cross-alphabet bytes rejected.
        assert_eq!(web64_decode("+/8="), None);
        assert_eq!(mime_decode("-_8="), None);
        assert_eq!(mime_decode("Zm9v!"), None);
        // Trailing garbage after padding is ignored (loop ends at '=').
        assert_eq!(mime_decode("Zg==ignored"), Some(b"f".to_vec()));
        // Lone dangling char yields nothing but succeeds.
        assert_eq!(mime_decode("Z"), Some(Vec::new()));
        assert_eq!(mime_decode(""), Some(Vec::new()));
    }
}
