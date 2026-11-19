//! PageSpeed configuration: filters, options, scopes, and parsing.
//!
//! This crate ports `RewriteOptions` and its parsing/dispatch surface:
//! the 95-filter enum with names, URL ids, sets, and enablement
//! ([`filter`]), option scopes ([`scope`]), the 167-row property table
//! with lookups and scope gates ([`property`]), wildcard groups
//! ([`wildcard`]), and the value store with config-file, nginx-directive,
//! merge, and query-param entry points ([`options`]).
//!
//! System/factory options (`FileCachePath`, `MemcachedServers`, …),
//! `DomainLawyer`, `ExperimentSpec` validation, and signature computation
//! land with their owning steps; see each item's docs.
#![forbid(unsafe_code)]

pub mod client_options;
pub mod filter;
pub mod message_handler;
pub mod options;
pub mod property;
pub mod scope;
pub mod wildcard;

pub use filter::{Filter, FilterSet, FilterState, RewriteLevel};
pub use message_handler::{MessageHandler, MessageLevel, MockMessageHandler, NullMessageHandler};
pub use options::{RewriteOptions, SetError, SetOutcome};
pub use property::{lookup_by_id, lookup_by_name, Property};
pub use scope::Scope;
