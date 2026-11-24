//! Streaming HTML parsing and rewriting engine.
//!
//! Rust port of the `pagespeed/kernel/html` tokenizer/parser plus the
//! `net/instaweb/rewriter` filter chain. The pipeline is event-driven like
//! the C++: [`lexer`] tokenizes bytes into events, [`parser`] buffers them
//! into flush windows with a live DOM slice, filters mutate events and
//! elements, and [`writer`] serializes.
//!
//! Everything is byte-oriented (`&[u8]`/`Vec<u8]`), matching the C++
//! `GoogleString`/`StringPiece` semantics including Latin-1 attribute
//! values that are not valid UTF-8.
#![forbid(unsafe_code)]

pub mod ads_attribute;
pub mod ads_util;
pub mod background;
pub mod base_tracker;
pub mod beacon_finder;
pub mod cacheable_resource;
pub mod chain;
pub mod critical_selector_finder;
pub mod css_tag_scanner;
pub mod css_util;
pub mod decode_url;
pub mod device_properties;
pub mod doctype;
pub mod domain_lawyer;
pub mod downstream_caching_directives;
pub mod driver;
pub mod element;
pub mod event;
pub mod fake_filter;
pub mod file_input_resource;
pub mod filter;
pub mod filters;
pub mod flush_early;
pub mod font_service_input_resource;
pub mod html_async;
pub mod html_cache;
pub mod image_url_encoder;
pub mod ipro;
pub mod js_escape;
pub mod keywords;
pub mod lexer;
pub mod metadata;
pub mod name;
pub mod namer;
pub mod output_resource;
pub mod parser;
pub mod request_properties;
pub mod resource;
pub mod resource_slot;
pub mod resource_tag_scanner;
pub mod rewrite_context;
pub mod rewrite_filter;
pub mod rewrite_stats;
pub mod script_scanner;
pub mod server_context;
pub mod show_ads_snippet_parser;
pub mod srcset;
pub mod static_assets;
pub mod url_input_resource;
pub mod url_namer;
pub mod url_partnership;
pub mod url_segment_encoder;
pub mod writer;

mod combine_metadata;
