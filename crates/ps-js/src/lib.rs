//! JavaScript minification, source maps, and library identification.
//!
//! This crate ports the JS toolchain: keywords ([`keywords`]), the
//! lossless tokenizer ([`tokenizer`]), both minifiers ([`minify`]),
//! source-map encoding ([`sourcemap`]), library identification
//! ([`library`]), and code-block rewrite decisions ([`rewrite`]).
#![forbid(unsafe_code)]

pub mod keywords;
pub mod library;
pub mod minify;
pub mod rewrite;
pub mod sourcemap;
pub mod tokenizer;
