//! Content hashing (`pagespeed/kernel/base/hasher.{h,cc}`,
//! `md5_hasher.{h,cc}`).
//!
//! `Hash` web64-encodes the raw digest and truncates to
//! `min(max_chars, raw_bytes * 4 / 3)` characters; `hash_to_u64` reads
//! the first 8 raw bytes big-endian. `Md5Hasher` matches the OpenSSL
//! MD5 behind the C++ implementation with the 10-character default.
#![forbid(unsafe_code)]

use crate::base64::web64_encode;

/// Content hasher (`Hasher`).
pub trait Hasher: Send + Sync {
    /// Raw digest bytes (`RawHash`).
    fn raw_hash(&self, content: &[u8]) -> Vec<u8>;

    /// Raw digest length (`RawHashSizeInBytes`).
    fn raw_hash_size(&self) -> usize;

    /// Configured maximum hash length in characters.
    fn max_chars(&self) -> usize;

    /// Encoded, truncated hash (`Hash`).
    fn hash(&self, content: &[u8]) -> String {
        let mut out = web64_encode(&self.raw_hash(content));
        out.truncate(self.hash_size_in_chars());
        out
    }

    /// Encoded hash length (`HashSizeInChars`).
    fn hash_size_in_chars(&self) -> usize {
        self.max_chars().min(self.raw_hash_size() * 4 / 3)
    }

    /// First 8 raw bytes as big-endian u64 (`HashToUint64`).
    fn hash_to_u64(&self, content: &[u8]) -> u64 {
        let raw = self.raw_hash(content);
        assert!(
            raw.len() >= 8,
            "raw hash shorter than 8 bytes (CHECK_LE in C++)"
        );
        let mut value: u64 = 0;
        for byte in raw.iter().take(8) {
            value = (value << 8) | u64::from(*byte);
        }
        value
    }
}

/// MD5 hasher (`MD5Hasher`), defaulting to 10 characters.
#[derive(Debug, Clone, Copy)]
pub struct Md5Hasher {
    max_chars: usize,
}

impl Md5Hasher {
    /// C++ default hash size (`kDefaultHashSize`).
    pub const DEFAULT_HASH_SIZE: usize = 10;
    /// MD5 digest length in bytes.
    pub const DIGEST_BYTES: usize = 16;

    /// Creates a hasher with the default size.
    #[must_use]
    pub const fn new() -> Md5Hasher {
        Md5Hasher {
            max_chars: Md5Hasher::DEFAULT_HASH_SIZE,
        }
    }

    /// Creates a hasher with an explicit size.
    #[must_use]
    pub const fn with_size(max_chars: usize) -> Md5Hasher {
        Md5Hasher { max_chars }
    }
}

impl Default for Md5Hasher {
    fn default() -> Self {
        Md5Hasher::new()
    }
}

impl Hasher for Md5Hasher {
    fn raw_hash(&self, content: &[u8]) -> Vec<u8> {
        use md5::{Digest, Md5};
        Md5::digest(content).to_vec()
    }

    fn raw_hash_size(&self) -> usize {
        Md5Hasher::DIGEST_BYTES
    }

    fn max_chars(&self) -> usize {
        self.max_chars
    }
}

/// Constant test hasher (`MockHasher`): every input hashes to the
/// configured value, `\xd0` (web64 `"0"`) by default.
pub struct MockHasher {
    hash_value: std::sync::Mutex<Vec<u8>>,
    max_chars: usize,
}

impl MockHasher {
    /// Creates the default constant hasher.
    #[must_use]
    pub fn new() -> MockHasher {
        MockHasher {
            hash_value: std::sync::Mutex::new(vec![0xd0]),
            max_chars: i32::MAX as usize,
        }
    }

    /// Creates a hasher returning a fixed value.
    #[must_use]
    pub fn with_value(value: &[u8]) -> MockHasher {
        MockHasher {
            hash_value: std::sync::Mutex::new(value.to_vec()),
            max_chars: value.len(),
        }
    }

    /// Replaces the constant (`set_hash_value`).
    pub fn set_hash_value(&self, value: &[u8]) {
        *self.hash_value.lock().unwrap() = value.to_vec();
    }
}

impl Default for MockHasher {
    fn default() -> Self {
        MockHasher::new()
    }
}

impl std::fmt::Debug for MockHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockHasher")
            .field("hash_value", &self.hash_value.lock().unwrap())
            .finish()
    }
}

impl Hasher for MockHasher {
    fn raw_hash(&self, _content: &[u8]) -> Vec<u8> {
        self.hash_value.lock().unwrap().clone()
    }

    fn raw_hash_size(&self) -> usize {
        self.hash_value.lock().unwrap().len()
    }

    fn max_chars(&self) -> usize {
        self.max_chars
    }
}

/// Case-preserving string hash (`HashString<CasePreserve, uint32>`):
/// `result = result * 131 + byte` (wrapping), driving deterministic
/// shard selection identically on every machine.
#[must_use]
pub fn hash_string_case_preserve(data: &[u8]) -> u32 {
    let mut result: u32 = 0;
    for byte in data {
        result = result.wrapping_mul(131).wrapping_add(u32::from(*byte));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_hasher_returns_constant() {
        let hasher = MockHasher::new();
        // Every input hashes to \xd0 (web64 "0").
        assert_eq!(hasher.hash(b""), "0");
        assert_eq!(hasher.hash(b"goodgood"), "0");
        assert_eq!(hasher.raw_hash_size(), 1);
        hasher.set_hash_value(b"custom");
        assert_eq!(hasher.raw_hash(b"anything"), b"custom");
        assert_eq!(MockHasher::with_value(b"xy").raw_hash_size(), 2);
    }

    #[test]
    fn md5_vectors_and_truncation() {
        // RFC 1321 digests via the raw API.
        let hasher = Md5Hasher::new();
        assert_eq!(
            hasher.raw_hash(b""),
            vec![
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
                0x42, 0x7e
            ]
        );
        assert_eq!(hasher.raw_hash(b"abc")[0], 0x90);
        // Default truncates the 24-char web64 encoding to 10 chars.
        assert_eq!(hasher.hash_size_in_chars(), 10);
        assert_eq!(hasher.hash(b"abc").len(), 10);
        // Max chars beyond the 21-char encodable ceiling clamp down.
        let wide = Md5Hasher::with_size(100);
        assert_eq!(wide.hash_size_in_chars(), 16 * 4 / 3);
        assert_eq!(wide.hash(b"abc").len(), 21);
        // Narrow sizes truncate exactly.
        assert_eq!(Md5Hasher::with_size(4).hash(b"abc").len(), 4);
        // Hashes are deterministic and input-sensitive.
        assert_eq!(hasher.hash(b"abc"), hasher.hash(b"abc"));
        assert_ne!(hasher.hash(b"abc"), hasher.hash(b"abd"));
        // Web64 alphabet only (no +/= in the truncated range for md5).
        for byte in hasher.hash(b"The quick brown fox").bytes() {
            assert!(
                byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_',
                "byte {byte}"
            );
        }
    }

    #[test]
    fn hash_to_u64_is_big_endian_prefix() {
        let hasher = Md5Hasher::new();
        let raw = hasher.raw_hash(b"abc");
        let mut expected: u64 = 0;
        for byte in raw.iter().take(8) {
            expected = (expected << 8) | u64::from(*byte);
        }
        assert_eq!(hasher.hash_to_u64(b"abc"), expected);
        // MD5("abc") = 900150983cd24fb0... -> 0x900150983cd24fb0.
        assert_eq!(hasher.hash_to_u64(b"abc"), 0x9001_5098_3cd2_4fb0);
    }

    #[test]
    fn golden_hashes() {
        // Cross-checked against Python hashlib+base64 (with +/ -> -_):
        // web64("")  = "1B2M2Y8AsgTpgAmY7PhCfg=="
        // web64("abc") = "kAFQmDzST7DWlj99KOF_cg==".
        assert_eq!(Md5Hasher::new().hash(b""), "1B2M2Y8Asg");
        assert_eq!(Md5Hasher::with_size(21).hash(b""), "1B2M2Y8AsgTpgAmY7PhCf");
        assert_eq!(Md5Hasher::new().hash(b"abc"), "kAFQmDzST7");
        assert_eq!(
            Md5Hasher::with_size(21).hash(b"abc"),
            "kAFQmDzST7DWlj99KOF_c"
        );
    }

    #[test]
    fn case_preserve_hash_vectors() {
        assert_eq!(hash_string_case_preserve(b""), 0);
        assert_eq!(hash_string_case_preserve(b"a"), 97);
        assert_eq!(hash_string_case_preserve(b"ab"), 97 * 131 + 98);
        assert_eq!(
            hash_string_case_preserve(b"abc"),
            (97 * 131 + 98) * 131 + 99
        );
        // Case is preserved, not folded.
        assert_ne!(
            hash_string_case_preserve(b"abc"),
            hash_string_case_preserve(b"ABC")
        );
    }
}
