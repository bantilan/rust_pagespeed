//! PageSpeed caching: hashing, naming, keys, TTLs, purge, backends.
//!
//! This crate ports the cache foundations: time constants ([`timer`]),
//! base64 codecs ([`base64`]), content hashing ([`hash`]), resource
//! naming ([`namer`]), key construction ([`keys`]), TTL policy ([`ttl`]),
//! purge sets and invalidation ([`purge`]), and the [`Cache`] trait with
//! memory, LRU, and file backends plus adapters ([`cache`]).
#![forbid(unsafe_code)]

pub mod base64;
pub mod cache;
pub mod hash;
pub mod html;
pub mod keys;
pub mod namer;
pub mod purge;
pub mod remote;
/// Optional Linux shared-memory byte backend (factory integration is separate).
#[cfg(target_os = "linux")]
pub mod shared_memory;
pub mod signature;
pub mod system;
pub mod timer;
pub mod ttl;

pub use cache::{Cache, KeyState};
pub use hash::{Hasher, Md5Hasher};
