//! Cache-key construction (`rewrite_context.cc`, `http_cache.{h,cc}`,
//! `server_context.cc`, `output_resource.cc`).
//!
//! - Metadata partition keys: `rname/<id>_<signature>/<url>@<suffix>`
//!   (`RewriteContext::SetPartitionKey`).
//! - Long/structureless URL keys are hash-split as `<c1>/<rest>`
//!   (`HashSplit`).
//! - HTTP-cache keys are `version/fragment/key` (`HTTPCache::CompositeKey`,
//!   currently `v5/`).
//! - Generated-resource names use `id.name` (`ResourceNamer::EncodeIdName`).
#![forbid(unsafe_code)]

use crate::hash::Hasher;

/// Metadata key prefix (`ServerContext::kCacheKeyResourceNamePrefix`).
pub const RESOURCE_NAME_PREFIX: &str = "rname/";
/// HTTP-cache key version (`kHttpCacheVersion`).
pub const HTTP_CACHE_VERSION: u32 = 5;
/// Degenerate zero-slot URL key (`SetPartitionKey`).
pub const EMPTY_URL_KEY: &str = "empty";

/// Version prefix for HTTP-cache keys (`SetVersion`: `v<N>/`).
#[must_use]
pub fn http_version_prefix() -> String {
    format!("v{HTTP_CACHE_VERSION}/")
}

/// Splits a hash as `<first-char>/<rest>` (`HashSplit`).
///
/// Used when a URL key lacks natural `/` hierarchy (data: URLs, combined
/// encodings) so filesystem caches stay shallow.
#[must_use]
pub fn hash_split(hasher: &dyn Hasher, text: &str) -> String {
    let hash = hasher.hash(text.as_bytes());
    format!("{}/{}", &hash[..1], &hash[1..])
}

/// Whether a URL is a `data:` URL (`IsDataUrl`).
#[must_use]
pub fn is_data_url(url: &str) -> bool {
    url.starts_with("data:")
}

/// Which `SetPartitionKey` branch builds the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotKey<'a> {
    /// Zero-slot degenerate form (fixed `empty` key, suffix cleared).
    Zero,
    /// Single resource: natural key, `encoding@ua_suffix` suffix.
    Single {
        /// Resource cache key (hash-split when a `data:` URL).
        url_key: &'a str,
        /// URL-encoder output for the resource context.
        encoding: &'a str,
        /// User-agent cache-key fragment.
        user_agent_key: &'a str,
        /// Context suffix (`CacheKeySuffix`).
        suffix: &'a str,
    },
    /// Combined resources: pre-encoded key, always hash-split.
    Multi {
        /// Encoder output over all slot keys.
        combined: &'a str,
        /// Context suffix (kept, unlike the zero-slot form).
        suffix: &'a str,
    },
}

/// Builds a metadata partition key (`SetPartitionKey`).
#[must_use]
pub fn partition_key(id: &str, signature: &str, slots: SlotKey<'_>, hasher: &dyn Hasher) -> String {
    // One format for all branches, as in C++ (`StrCat(prefix, id, "_",
    // signature, "/", url_key, "@", suffix)`).
    let (key, full_suffix) = match slots {
        SlotKey::Zero => (EMPTY_URL_KEY.to_string(), String::new()),
        SlotKey::Single {
            url_key,
            encoding,
            user_agent_key,
            suffix,
        } => {
            let key = if is_data_url(url_key) {
                hash_split(hasher, url_key)
            } else {
                url_key.to_string()
            };
            let full_suffix = format!("{encoding}@{user_agent_key}_{suffix}");
            (key, full_suffix)
        }
        SlotKey::Multi { combined, suffix } => (hash_split(hasher, combined), suffix.to_string()),
    };
    format!("{RESOURCE_NAME_PREFIX}{id}_{signature}/{key}@{full_suffix}")
}

/// Builds an HTTP-cache composite key (`HTTPCache::CompositeKey`):
/// `version/fragment/key`, or `version/key` when the fragment is empty.
///
/// The fragment carries the cache-fragment shard (anti-poisoning); it must
/// not contain `/`.
#[must_use]
pub fn composite_key(key: &str, fragment: &str) -> String {
    debug_assert!(
        !fragment.contains('/'),
        "fragment must not contain '/' (C++ DCHECK)"
    );
    if fragment.is_empty() {
        format!("{}{key}", http_version_prefix())
    } else {
        format!("{}{fragment}/{key}", http_version_prefix())
    }
}

/// Builds an output-resource name key (`OutputResource::name_key`):
/// `resolved_base` + `EncodeIdName()`.
#[must_use]
pub fn output_name_key(resolved_base: &str, id: &str, name: &str) -> String {
    format!("{resolved_base}{id}.{name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Md5Hasher;

    #[test]
    fn golden_partition_keys() {
        let hasher = Md5Hasher::new();
        // Single-slot form: rname/<id>_<sig>/<url>@<enc>@<ua>_<suffix>.
        assert_eq!(
            partition_key(
                "ic",
                "0",
                SlotKey::Single {
                    url_key: "http://x/a.png",
                    encoding: "enc",
                    user_agent_key: "ua",
                    suffix: "s",
                },
                &hasher
            ),
            "rname/ic_0/http://x/a.png@enc@ua_s"
        );
        // Zero-slot degenerate form shares one entry.
        assert_eq!(
            partition_key("is", "0", SlotKey::Zero, &hasher),
            "rname/is_0/empty@"
        );
        // Data URLs hash-split: first hash char becomes a directory.
        let key = partition_key(
            "ic",
            "0",
            SlotKey::Single {
                url_key: "data:image/png;base64,xx",
                encoding: "",
                user_agent_key: "",
                suffix: "",
            },
            &hasher,
        );
        let hash = hasher.hash(b"data:image/png;base64,xx");
        assert_eq!(key, format!("rname/ic_0/{}/{}@@_", &hash[..1], &hash[1..]));
        // Multi-slot: combined key always hash-splits; suffix kept.
        let key = partition_key(
            "cc",
            "0",
            SlotKey::Multi {
                combined: "combined",
                suffix: "",
            },
            &hasher,
        );
        let hash = hasher.hash(b"combined");
        assert_eq!(key, format!("rname/cc_0/{}/{}@", &hash[..1], &hash[1..]));
    }

    #[test]
    fn golden_composite_keys() {
        assert_eq!(http_version_prefix(), "v5/");
        assert_eq!(composite_key("http://x/a", ""), "v5/http://x/a");
        assert_eq!(composite_key("http://x/a", "frag"), "v5/frag/http://x/a");
        assert_eq!(
            output_name_key("http://x/", "cf", "a.css"),
            "http://x/cf.a.css"
        );
        assert!(is_data_url("data:,x"));
        assert!(!is_data_url("http://x/"));
    }
}
