//! Compat harness hooks (Step 8).
//!
//! Deterministic fixture vectors from each crate plus the
//! assertion/corpus interface the reusable Phase 4 suite
//! plugs into. No suite lives here — only the shapes.
#![forbid(unsafe_code)]

pub mod fixtures;
pub mod harness;
