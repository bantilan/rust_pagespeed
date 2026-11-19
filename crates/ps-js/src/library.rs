//! JavaScript library identification
//! (`javascript_library_identification.{h,cc}`).
//!
//! Registers known libraries by `(size, md5, url)` and recognizes them
//! in rewritten code by size plus a 21-char MD5 hash. Validation rules
//! mirror C++ exactly (including the always-true charset check).
#![forbid(unsafe_code)]

use ps_cache::hash::{Hasher, Md5Hasher};
use std::collections::HashMap;

/// Hash characters compared on lookup (`kNumHashChars` = 21).
pub const NUM_HASH_CHARS: usize = 21;

/// Known-library registry (`JavascriptLibraryIdentification`).
#[derive(Debug, Default)]
pub struct LibraryIdentification {
    libraries: HashMap<i64, HashMap<String, String>>,
    hasher: Md5Hasher,
}

impl LibraryIdentification {
    /// Creates an empty registry (21-char comparison hashes).
    #[must_use]
    pub fn new() -> LibraryIdentification {
        LibraryIdentification {
            libraries: HashMap::new(),
            hasher: Md5Hasher::with_size(NUM_HASH_CHARS),
        }
    }

    /// Registers a library (`RegisterLibrary`).
    ///
    /// The md5 must be ASCII alphanumerics/`-`/`_` (empty allowed, as in
    /// C++) and the URL must resolve against the example base; new
    /// entries override previous ones for the same key.
    pub fn register(&mut self, bytes: i64, md5: &str, url: &str) -> bool {
        if !md5
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return false;
        }
        if !resolves_against_example(url) {
            return false;
        }
        self.libraries
            .entry(bytes)
            .or_default()
            .insert(md5.to_string(), url.to_string());
        true
    }

    /// Recognizes rewritten code (`Find`): size match, then 21-char hash.
    #[must_use]
    pub fn find(&self, rewritten_code: &[u8]) -> Option<String> {
        let candidates = self.libraries.get(&(rewritten_code.len() as i64))?;
        let hash = self.hasher.hash(rewritten_code);
        candidates.get(&hash).cloned()
    }
}

/// Conservative URL check shared with option parsing: absolute
/// `http(s)://host/...` or a relative reference.
fn resolves_against_example(url: &str) -> bool {
    if url.is_empty() || url.bytes().any(|b| b.is_ascii_whitespace()) {
        return false;
    }
    if let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    {
        let host = rest.split('/').next().unwrap_or("");
        return !host.is_empty() && !host.contains([' ', '\t']);
    }
    !url.contains("://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_find_override() {
        let mut libraries = LibraryIdentification::new();
        assert_eq!(NUM_HASH_CHARS, 21);
        // Registration validates md5 charset and URL shape.
        assert!(libraries.register(43567, "5giEj_jl-Ag5G8", "http://www.example.com/url.js"));
        assert!(!libraries.register(43567, "bad md5!", "http://e.com/u.js"));
        assert!(!libraries.register(43567, "abc", "not a url"));
        assert!(libraries.register(1, "", "http://e.com/empty-md5.js"));
        // Unknown code is not recognized.
        assert_eq!(libraries.find(b"var a = 1;"), None);
        // Recognized by exact (size, hash): register-then-find round trip.
        let code = b"/*! jquery-migrate 1.0.0 */var x;";
        let hasher = Md5Hasher::with_size(NUM_HASH_CHARS);
        let hash = hasher.hash(code);
        assert!(libraries.register(code.len() as i64, &hash, "http://cdn.example.com/lib.js"));
        assert_eq!(
            libraries.find(code),
            Some("http://cdn.example.com/lib.js".to_string())
        );
        // Same size, different bytes: no match.
        let mut other = code.to_vec();
        other[0] = b'X';
        assert_eq!(libraries.find(&other), None);
        // Override wins for the same key.
        assert!(libraries.register(code.len() as i64, &hash, "http://cdn.example.com/lib2.js"));
        assert_eq!(
            libraries.find(code),
            Some("http://cdn.example.com/lib2.js".to_string())
        );
    }
}
