//! External AVIF encoding through the `avifenc` binary.
//!
//! Ports `ConvertToAvifExternal`: admission control on a process-wide encoder
//! slot, temp-file staging with an extension-carrying input name, the
//! `avifenc --jobs 1 --min CQ --max CQ` invocation (CQ from quality),
//! deadline enforcement by polling wait with kill, output validation, temp
//! cleanup, and statistics on every return path.
//!
//! Two deliberate simplifications vs the C++: child CPU time is not
//! collected (`std::process` exposes no rusage; the stat stays silent), and
//! the deadline uses a 1ms poll loop instead of a watchdog thread — same
//! outcomes, no extra threads or dependencies. `std::process` spawns
//! without fork, preserving the C++ WG-07 fix.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::error::{Error, ErrorSource, Result};

/// Process-wide count of running external AVIF encoders.
static ENCODERS_IN_FLIGHT: AtomicI32 = AtomicI32::new(0);
/// Temp-file uniqueness counter.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Minimum plausible AVIF output (`kMinAvifBytes`): ftyp box + brand + payload.
const MIN_AVIF_BYTES: usize = 32;

/// One external AVIF conversion request.
#[derive(Debug, Clone)]
pub struct AvifRequest {
    /// Input file suffix (".jpg" or ".png") so avifenc sniffs the format.
    pub input_suffix: &'static str,
    /// Quality 0..=100, mapped to a constant-Q level.
    pub quality: i64,
    /// Deadline per run in ms; negative disables, zero always times out.
    pub timeout_ms: i64,
    /// Process-wide concurrency cap; <= 0 means unbounded.
    pub max_concurrent: i32,
}

/// RAII admission slot; releases the in-flight count on drop.
struct EncoderSlot {
    held: bool,
}

impl EncoderSlot {
    fn try_acquire(max_concurrent: i32) -> Option<Self> {
        loop {
            let in_flight = ENCODERS_IN_FLIGHT.load(Ordering::SeqCst);
            if max_concurrent > 0 && in_flight >= max_concurrent {
                return None;
            }
            if ENCODERS_IN_FLIGHT
                .compare_exchange(in_flight, in_flight + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(EncoderSlot { held: true });
            }
        }
    }
}

impl Drop for EncoderSlot {
    fn drop(&mut self) {
        if self.held {
            ENCODERS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Maps a 0..=100 quality to an avifenc constant-Q level 0..=63.
fn quality_to_cq_level(quality: i64) -> i64 {
    let clamped = quality.clamp(0, 100);
    63 - (clamped * 63) / 100
}

/// True when bytes look like a finished AVIF still (`AvifBytesLookComplete`):
/// over 32 bytes with `ftypavif` at the box header.
fn bytes_look_complete(data: &[u8]) -> bool {
    data.len() > MIN_AVIF_BYTES
        && data.get(4..8) == Some(b"ftyp")
        && data.get(8..12) == Some(b"avif")
}

/// Creates a uniquely-named temp file, returning its path. Retries on
/// collision; the file is created empty and exclusive.
fn create_temp_file(prefix: &str, suffix: &str) -> Result<std::path::PathBuf> {
    for _ in 0..100 {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("{prefix}{}_{id}{suffix}", std::process::id(),);
        let path = std::env::temp_dir().join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(_) => continue,
        }
    }
    Err(Error::new(
        crate::error::ErrorKind::EncoderError,
        ErrorSource::AvifEncoder,
        "cannot create AVIF temp file",
    ))
}

/// Converts image bytes to AVIF through an external `avifenc` process.
///
/// Returns the AVIF bytes on success. Every outcome — shed, timeout,
/// spawn failure, encoder failure — records statistics and cleans up.
pub fn convert_to_avif(
    input: &[u8],
    request: &AvifRequest,
    recorder: &dyn crate::stats::Recorder,
) -> Result<Vec<u8>> {
    // Admission control first: a shed conversion costs no files or spawn.
    let _slot = match EncoderSlot::try_acquire(request.max_concurrent) {
        Some(slot) => slot,
        None => {
            recorder.avif_shed();
            return Err(Error::new(
                crate::error::ErrorKind::EncoderError,
                ErrorSource::AvifEncoder,
                "AVIF encoder shed: too many conversions running",
            ));
        }
    };

    // A zero deadline disables AVIF with timeout accounting, without spawn.
    if request.timeout_ms == 0 {
        recorder.avif_conversion(false, true, 0, None);
        return Err(Error::new(
            crate::error::ErrorKind::Timeout,
            ErrorSource::AvifEncoder,
            "AVIF conversion deadline is zero",
        ));
    }
    let started = Instant::now();

    let finish_early = |recorder: &dyn crate::stats::Recorder| {
        recorder.avif_conversion(false, false, 0, None);
    };

    // Stage the input with its expected extension (avifenc sniffs by name).
    let input_base = match create_temp_file("ps_avif_input_", ".bin") {
        Ok(path) => path,
        Err(err) => {
            finish_early(recorder);
            return Err(err);
        }
    };
    let output_path = match create_temp_file("ps_avif_output_", ".avif") {
        Ok(path) => path,
        Err(err) => {
            let _ = std::fs::remove_file(&input_base);
            finish_early(recorder);
            return Err(err);
        }
    };
    let input_path = input_base.with_extension(request.input_suffix.trim_start_matches('.'));
    if std::fs::rename(&input_base, &input_path).is_err() {
        let _ = std::fs::remove_file(&input_base);
        let _ = std::fs::remove_file(&output_path);
        finish_early(recorder);
        return Err(Error::new(
            crate::error::ErrorKind::EncoderError,
            ErrorSource::AvifEncoder,
            "cannot set AVIF input extension",
        ));
    }
    if std::fs::File::create(&input_path)
        .and_then(|mut file| file.write_all(input))
        .is_err()
    {
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_path);
        finish_early(recorder);
        return Err(Error::new(
            crate::error::ErrorKind::EncoderError,
            ErrorSource::AvifEncoder,
            "cannot stage AVIF input",
        ));
    }

    // Spawn: single-threaded encoder, constant quality, quiet stdout/stderr.
    let cq_level = quality_to_cq_level(request.quality).to_string();
    let mut child = match Command::new("avifenc")
        .args([
            "--jobs",
            "1",
            "--min",
            &cq_level,
            "--max",
            &cq_level,
            input_path.to_str().unwrap_or(""),
            output_path.to_str().unwrap_or(""),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let _ = std::fs::remove_file(&input_path);
            let _ = std::fs::remove_file(&output_path);
            let wall_ms = started.elapsed().as_millis() as i64;
            recorder.avif_conversion(false, false, wall_ms, None);
            return Err(Error::new(
                crate::error::ErrorKind::EncoderError,
                ErrorSource::AvifEncoder,
                format!("cannot spawn avifenc: {err}"),
            ));
        }
    };

    // Deadline enforcement: poll for exit, killing past the deadline (a
    // negative timeout blocks until the encoder exits, like the C++ with no
    // watchdog thread). Polling at 1ms granularity keeps this dependency-
    // free; the outcome matches the cond-wait watchdog exactly.
    let deadline = if request.timeout_ms >= 0 {
        Some(started + Duration::from_millis(request.timeout_ms.max(0) as u64))
    } else {
        None
    };
    let mut was_timed_out = false;
    let mut exit_ok = false;
    let mut recovered = false;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_ok = status.success();
                break;
            }
            Ok(None) => {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    was_timed_out = true;
                    let _ = child.kill();
                    // A natural exit racing the kill still counts below.
                    if let Ok(status) = child.wait() {
                        exit_ok = status.success();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => {
                // The host reaped our encoder (its SIGCHLD handler beat us),
                // so no exit status is available: plausible bytes decide.
                recovered = true;
                break;
            }
        }
    }

    let wall_ms = started.elapsed().as_millis() as i64;
    // A completed conversion counts even if the kill raced its natural exit.
    if exit_ok {
        was_timed_out = false;
    }
    if recovered && !was_timed_out {
        exit_ok = true;
    } else {
        recovered = false;
    }

    let mut output = Vec::new();
    let mut ok = exit_ok;
    if ok {
        match std::fs::read(&output_path) {
            Ok(bytes) if !bytes.is_empty() => {
                if recovered && !bytes_look_complete(&bytes) {
                    ok = false;
                } else {
                    output = bytes;
                }
            }
            _ => {
                ok = false;
            }
        }
    }

    recorder.avif_conversion(ok, was_timed_out, wall_ms, None);

    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&output_path);
    if ok {
        Ok(output)
    } else if was_timed_out {
        Err(Error::new(
            crate::error::ErrorKind::Timeout,
            ErrorSource::AvifEncoder,
            "AVIF encoder exceeded its deadline",
        ))
    } else {
        Err(Error::new(
            crate::error::ErrorKind::EncoderError,
            ErrorSource::AvifEncoder,
            "AVIF encoder failed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_maps_to_cq_level() {
        assert_eq!(quality_to_cq_level(100), 0);
        assert_eq!(quality_to_cq_level(0), 63);
        assert_eq!(quality_to_cq_level(85), 63 - (85 * 63) / 100);
        assert_eq!(quality_to_cq_level(200), 0);
        assert_eq!(quality_to_cq_level(-5), 63);
    }

    #[test]
    fn completeness_check_matches_cpp() {
        let mut bytes = vec![0u8; 64];
        bytes[4..8].copy_from_slice(b"ftyp");
        bytes[8..12].copy_from_slice(b"avif");
        assert!(bytes_look_complete(&bytes));
        assert!(!bytes_look_complete(&bytes[..32]));
        bytes[8..12].copy_from_slice(b"avis");
        assert!(!bytes_look_complete(&bytes));
    }
}
