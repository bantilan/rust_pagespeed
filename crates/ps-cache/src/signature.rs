//! HMAC-SHA1 URL signatures (`SHA1Signature`).
#![forbid(unsafe_code)]

use hmac::{Hmac, Mac};
use sha1::Sha1;

use super::base64::web64_encode;

/// Raw HMAC-SHA1 length in bytes (`kSHA1NumBytes`).
pub const SHA1_BYTES: usize = 20;
/// Default signature length in characters (`kDefaultSignatureSize`).
pub const DEFAULT_SIGNATURE_SIZE: usize = 10;

/// HMAC-SHA1 signer (`SHA1Signature`).
#[derive(Debug, Clone, Copy)]
pub struct Sha1Signature {
    max_chars: usize,
}

impl Sha1Signature {
    /// Builds a signer with the default size.
    #[must_use]
    pub fn new() -> Self {
        Sha1Signature {
            max_chars: DEFAULT_SIGNATURE_SIZE,
        }
    }

    /// Builds a signer with an explicit size.
    #[must_use]
    pub fn with_size(max_chars: usize) -> Self {
        Sha1Signature { max_chars }
    }

    /// Encoded length for a byte count (`ComputeSizeFromNumberOfBytes`).
    #[must_use]
    pub fn size_from_bytes(num_bytes: usize) -> usize {
        num_bytes.div_ceil(3) * 4
    }

    /// Raw signature bytes (`RawSign`).
    #[must_use]
    pub fn raw_sign(&self, key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = Hmac::<Sha1>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }

    /// Output length (`SignatureSizeInChars`).
    #[must_use]
    pub fn signature_size_in_chars(&self) -> usize {
        Self::size_from_bytes(SHA1_BYTES).min(self.max_chars)
    }

    /// Web64-encoded signature, truncated (`Sign`).
    #[must_use]
    pub fn sign(&self, key: &[u8], data: &[u8]) -> String {
        let mut out = web64_encode(&self.raw_sign(key, data));
        out.truncate(self.signature_size_in_chars());
        out
    }
}

impl Default for Sha1Signature {
    fn default() -> Self {
        Sha1Signature::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_signature_size() {
        // SHA1 is 160 bit: 27 web64 chars plus padding.
        let key = vec![b'A'; 5000];
        let data = vec![b'Z'; 5000];
        let max = Sha1Signature::size_from_bytes(SHA1_BYTES);
        for size in 0..=max {
            let signature = Sha1Signature::with_size(size);
            assert_eq!(size, signature.sign(b"myKey", b"myData").len());
            assert_eq!(size, signature.sign(&key, &data).len());
        }
    }

    #[test]
    fn signatures_differ() {
        let signature = Sha1Signature::new();
        assert_ne!(
            signature.sign(b"key1", b"data1"),
            signature.sign(b"key2", b"data2")
        );
        assert_ne!(
            signature.sign(b"key1", b"data2"),
            signature.sign(b"key2", b"data2")
        );
        assert_ne!(
            signature.sign(b"key1", b"data1"),
            signature.sign(b"key1", b"data2")
        );
        assert_eq!(
            signature.sign(b"key1", b"data1"),
            signature.sign(b"key1", b"data1")
        );
        // Known vectors (also cross-checked against Python's hmac).
        let short = Sha1Signature::with_size(10);
        assert_eq!("ijqEvNDQBl", short.sign(b"hello", b"world"));
        let long = Sha1Signature::with_size(25);
        assert_eq!("EEFSxb_coHvGM-69RhmfAlXJ9", long.sign(b"key", b"data"));
        assert_eq!("RrTsWGEXFU2s1J1mTl1j_ciO-", long.sign(b"foo", b"bar"));
    }

    #[test]
    fn url_signing_vector() {
        // The fixed vector from `TestValidUrlSignatures`: the check
        // signs the URL through the filter id (hash/signature/ext
        // stripped by length).
        let signature = Sha1Signature::new();
        assert_eq!(
            "Q_aP9rObnq",
            signature.sign(
                b"helloworld",
                b"http://signed-urls.example.com/mod_pagespeed_example/styles/A.all_styles.css.pagespeed.cf"
            )
        );
    }
}
