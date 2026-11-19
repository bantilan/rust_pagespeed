//! PageSpeed statistics: counters, histograms, timed variables, logging.
//!
//! This crate ports the monitoring core: thread-safe primitives and the
//! named registry ([`core`]), every audited statistic name with per-owner
//! registration ([`names`]), and the append-only statistics log with
//! restart recovery and JSON dumps ([`logger`]).
//!
//! Cross-process shared-memory backing lands with the nginx step; the
//! registry here is in-process (`Send + Sync`).
#![forbid(unsafe_code)]

pub mod core;
pub mod logger;
pub mod names;

pub use core::{
    Histogram, Statistics, StatisticsLogger, TimedLevel, TimedVariable, UpDownCounter, Variable,
};
