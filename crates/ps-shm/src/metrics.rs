//! Saturating shared counters for bounded cache/ownership diagnostics.
use crate::region::{RegionLayout, SharedRegion};
use std::{io, path::Path};

/// Counter indices are fixed by the caller's versioned filename/schema. Values
/// persist until the domain is replaced offline or a new boot domain is used.
#[derive(Debug)]
pub struct SharedMetrics(SharedRegion);
impl SharedMetrics {
    /// Open 1–256 atomic counters; no live reset or resize is permitted.
    pub fn open(path: &Path, count: usize) -> io::Result<Self> {
        Ok(Self(SharedRegion::open(
            path,
            RegionLayout::new(count, 8)?,
        )?))
    }
    /// Increment without wrapping; indices are checked at runtime.
    pub fn increment(&self, index: usize) -> io::Result<()> {
        self.0.invalidate(index)
    }
    /// Read one process-shared counter without locking or filesystem access.
    pub fn get(&self, index: usize) -> io::Result<u64> {
        self.0.epoch(index)
    }
    /// Exact mapping allocation including headers/alignment.
    pub fn mapped_bytes(&self) -> usize {
        self.0.layout().mapped_bytes()
    }
}
