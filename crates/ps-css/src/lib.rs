//! CSS parsing, minification, and rewriting helpers.
//!
//! A port of the vendored `webutil/css` parser plus the PageSpeed CSS
//! filters' pure cores (`css_minify`, URL/`@import` rewriting,
//! flattening, combining). Async fetching and HTML integration live
//! in later steps; this crate parses and rewrites CSS text given
//! already-fetched inputs.
#![forbid(unsafe_code)]

pub mod absolutify;
pub mod ast;
pub mod color;
pub mod combine;
pub mod escapes;
pub mod flatten;
pub mod identifier;
pub mod inline;
pub mod media;
pub mod minify;
pub mod parser;
pub mod property;
pub mod rewrite;
pub mod scanner;
pub mod selector;
pub mod stats;
pub mod string_util;
pub mod tags;
pub mod unicode_text;
pub mod util;
pub mod value;
