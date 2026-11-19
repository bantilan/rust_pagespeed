//! Conversion statistics sinks.
//!
//! Ports the `ConversionVariables` / `AvifConversionVariables` recording
//! rules (`UpdateWebpStats`, `UpdateAvifStats`): timeouts bump a counter,
//! completions land in success/failure duration histograms, shed AVIF work
//! bumps its own counter, and reaped child CPU is always accounted. The
//! sinks are trait objects so drivers stay testable; the `ps-stats`-backed
//! implementation arrives with the filter layer.

/// WebP conversion variable (`ConversionVariables::VariableType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebpVar {
    FromUnknownFormat,
    FromGif,
    FromPng,
    FromJpeg,
    Opaque,
    NonOpaque,
    FromGifAnimated,
}

/// Recorder for conversion outcomes. All methods have no-op defaults so
/// partial implementations only override what they persist.
pub trait Recorder {
    /// Records one WebP conversion against one variable. Drivers call this
    /// twice per conversion (source + opacity class), like the C++.
    fn webp_conversion(&self, _var: WebpVar, _ok: bool, _timed_out: bool, _elapsed_ms: i64) {}

    /// Records one AVIF encoder run. `child_cpu_ms` is `Some` when the child
    /// was reaped (including killed runaways).
    fn avif_conversion(
        &self,
        _ok: bool,
        _timed_out: bool,
        _wall_elapsed_ms: i64,
        _child_cpu_ms: Option<i64>,
    ) {
    }

    /// Records one AVIF conversion refused by admission control.
    fn avif_shed(&self) {}
}

/// Recorder that drops everything (for `NULL` variable blocks in the C++).
#[derive(Debug, Default)]
pub struct NullRecorder;

impl Recorder for NullRecorder {}

/// One recorded WebP conversion: (variable, ok, timed_out, elapsed_ms).
pub type WebpEvent = (WebpVar, bool, bool, i64);
/// One recorded AVIF run: (ok, timed_out, wall_ms, child_cpu_ms).
pub type AvifEvent = (bool, bool, i64, Option<i64>);

/// In-memory recorder for tests.
#[derive(Debug, Default)]
pub struct TestRecorder {
    /// WebP recordings in call order.
    pub webp_events: std::sync::Mutex<Vec<WebpEvent>>,
    /// AVIF recordings in call order.
    pub avif_events: std::sync::Mutex<Vec<AvifEvent>>,
    /// Shed AVIF conversions.
    pub avif_sheds: std::sync::atomic::AtomicU32,
}

impl TestRecorder {
    #[must_use]
    pub fn new() -> Self {
        TestRecorder::default()
    }
}

impl Recorder for TestRecorder {
    fn webp_conversion(&self, var: WebpVar, ok: bool, timed_out: bool, elapsed_ms: i64) {
        self.webp_events
            .lock()
            .unwrap()
            .push((var, ok, timed_out, elapsed_ms));
    }

    fn avif_conversion(
        &self,
        ok: bool,
        timed_out: bool,
        wall_elapsed_ms: i64,
        child_cpu_ms: Option<i64>,
    ) {
        self.avif_events
            .lock()
            .unwrap()
            .push((ok, timed_out, wall_elapsed_ms, child_cpu_ms));
    }

    fn avif_shed(&self) {
        self.avif_sheds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_recorder_accepts_everything() {
        let recorder = NullRecorder;
        recorder.webp_conversion(WebpVar::FromJpeg, true, false, 3);
        recorder.avif_conversion(true, false, 9, Some(4));
        recorder.avif_shed();
    }

    #[test]
    fn test_recorder_captures_events() {
        let recorder = TestRecorder::new();
        recorder.webp_conversion(WebpVar::Opaque, true, false, 3);
        recorder.avif_conversion(false, true, 9, None);
        recorder.avif_shed();
        assert_eq!(
            *recorder.webp_events.lock().unwrap(),
            [(WebpVar::Opaque, true, false, 3)]
        );
        assert_eq!(
            *recorder.avif_events.lock().unwrap(),
            [(false, true, 9, None)]
        );
        assert_eq!(
            recorder
                .avif_sheds
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
}
