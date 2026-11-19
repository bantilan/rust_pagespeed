//! PageSpeed HTTP layer: headers, parsing, fetchers, and HTTP cache.
//!
//! This crate ports the fetching core: header names/statuses ([`names`]),
//! HTTP dates ([`time`]), the ordered header list ([`headers`]), content
//! types ([`content_type`]), the protobuf-compatible headers codec
//! ([`proto`]), response headers with caching computation ([`response`]),
//! response parsing ([`parser`]), request headers/context ([`request`]),
//! the cache value format ([`value`]), fetch traits and inflating fetches
//! ([`fetch`]), the mock fetcher ([`mock`]), the HTTP cache
//! ([`http_cache`]), and downstream purge requests ([`purger`]).
#![forbid(unsafe_code)]

pub mod bot_checker;
pub mod content_type;
pub mod data_url;
pub mod domain_registry;
pub mod fetch;
pub mod google_url;
pub mod headers;
pub mod http_cache;
pub mod mock;
pub mod names;
pub mod parser;
pub mod proto;
pub mod purger;
pub mod query_params;
pub mod request;
pub mod response;
pub mod time;
pub mod user_agent_matcher;
pub mod user_agent_normalizer;
pub mod value;

pub use response::{HttpOptions, ResponseHeaders};
