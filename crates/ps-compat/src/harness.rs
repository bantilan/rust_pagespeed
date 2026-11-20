//! Harness interface: corpora, assertion kinds, and the
//! runner trait the Phase 4 suite implements.

/// One comparable expectation (`assertion kinds`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assertion {
    /// Response bytes must equal `expected` exactly.
    ByteEqual {
        /// Expected bytes.
        expected: Vec<u8>,
        /// Actual bytes.
        actual: Vec<u8>,
    },
    /// A response header must equal `expected`.
    HeaderEqual {
        /// Header name.
        name: String,
        /// Expected value.
        expected: String,
        /// Actual value.
        actual: String,
    },
    /// A statistic must move by exactly `expected_delta`.
    StatDeltaEqual {
        /// Statistic name.
        name: String,
        /// Value before the case.
        before: i64,
        /// Value after the case.
        after: i64,
        /// Required delta.
        expected_delta: i64,
    },
    /// A cache key must equal `expected`.
    CacheKeyEqual {
        /// Expected key.
        expected: String,
        /// Actual key.
        actual: String,
    },
    /// The response status must equal `expected` (Phase 4
    /// extension for route smokes).
    StatusEqual {
        /// Expected status.
        expected: u16,
        /// Actual status.
        actual: u16,
    },
    /// Two images must decode to identical pixels (encoders
    /// differ, so bytes never match; lossless only).
    ImagePixelsEqual {
        /// Expected bytes.
        expected: Vec<u8>,
        /// Actual bytes.
        actual: Vec<u8>,
    },
}

/// The verdict for one assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertionResult {
    /// Assertion kind name.
    pub kind: &'static str,
    /// Whether it held.
    pub passed: bool,
    /// Human-readable mismatch, empty when passed.
    pub detail: String,
}

/// Evaluates one assertion.
#[must_use]
pub fn evaluate(assertion: &Assertion) -> AssertionResult {
    match assertion {
        Assertion::ByteEqual { expected, actual } => {
            let passed = expected == actual;
            AssertionResult {
                kind: "byte-equal",
                passed,
                detail: if passed {
                    String::new()
                } else {
                    format!(
                        "byte mismatch: expected {} bytes, got {}",
                        expected.len(),
                        actual.len()
                    )
                },
            }
        }
        Assertion::HeaderEqual {
            name,
            expected,
            actual,
        } => {
            let passed = expected == actual;
            AssertionResult {
                kind: "header-equal",
                passed,
                detail: if passed {
                    String::new()
                } else {
                    format!("{name}: expected {expected:?}, got {actual:?}")
                },
            }
        }
        Assertion::StatDeltaEqual {
            name,
            before,
            after,
            expected_delta,
        } => {
            let passed = after - before == *expected_delta;
            AssertionResult {
                kind: "stat-delta-equal",
                passed,
                detail: if passed {
                    String::new()
                } else {
                    format!(
                        "{name}: expected delta {expected_delta}, got {}",
                        after - before
                    )
                },
            }
        }
        Assertion::CacheKeyEqual { expected, actual } => {
            let passed = expected == actual;
            AssertionResult {
                kind: "cache-key-equal",
                passed,
                detail: if passed {
                    String::new()
                } else {
                    format!("key: expected {expected:?}, got {actual:?}")
                },
            }
        }
        Assertion::StatusEqual { expected, actual } => {
            let passed = expected == actual;
            AssertionResult {
                kind: "status-equal",
                passed,
                detail: if passed {
                    String::new()
                } else {
                    format!("status: expected {expected}, got {actual}")
                },
            }
        }
        Assertion::ImagePixelsEqual { expected, actual } => image_pixels_equal(expected, actual),
    }
}

/// Decodes two images and compares dimensions, pixel format,
/// and pixels.
fn image_pixels_equal(expected: &[u8], actual: &[u8]) -> AssertionResult {
    use ps_image::format::{sniff, ImageType};
    let fail = |detail: String| AssertionResult {
        kind: "image-pixels-equal",
        passed: false,
        detail,
    };
    let decode = |bytes: &[u8], side: &str| -> Result<ps_image::frame::Frame, String> {
        match sniff(bytes) {
            ImageType::Png => ps_image::png::decode(bytes),
            ImageType::Jpeg => ps_image::jpeg::decode(bytes),
            ImageType::Gif => ps_image::gif::first_frame(bytes),
            ImageType::Webp | ImageType::WebpLosslessOrAlpha => ps_image::webp::decode(bytes),
            other => {
                return Err(format!("{side}: unsupported format {other:?}"));
            }
        }
        .map_err(|error| format!("{side}: decode failed: {error:?}"))
    };
    let expected_lossy = matches!(
        sniff(expected),
        ImageType::Jpeg | ImageType::Webp | ImageType::WebpLosslessOrAlpha
    );
    let actual_lossy = matches!(
        sniff(actual),
        ImageType::Jpeg | ImageType::Webp | ImageType::WebpLosslessOrAlpha
    );
    let expected_frame = match decode(expected, "expected") {
        Ok(frame) => frame,
        Err(detail) => return fail(detail),
    };
    let actual_frame = match decode(actual, "actual") {
        Ok(frame) => frame,
        Err(detail) => return fail(detail),
    };
    if (expected_frame.width(), expected_frame.height())
        != (actual_frame.width(), actual_frame.height())
    {
        return fail(format!(
            "dimensions: expected {}x{}, got {}x{}",
            expected_frame.width(),
            expected_frame.height(),
            actual_frame.width(),
            actual_frame.height()
        ));
    }
    // Normalize layouts (encoders choose RGB vs RGBA freely).
    let expected_rgba = match expected_frame.expand(ps_image::frame::PixelFormat::Rgba8888) {
        Ok(frame) => frame,
        Err(error) => return fail(format!("expected: expand failed: {error:?}")),
    };
    let actual_rgba = match actual_frame.expand(ps_image::frame::PixelFormat::Rgba8888) {
        Ok(frame) => frame,
        Err(error) => return fail(format!("actual: expand failed: {error:?}")),
    };
    if expected_rgba.pixels() == actual_rgba.pixels() {
        return AssertionResult {
            kind: "image-pixels-equal",
            passed: true,
            detail: String::new(),
        };
    }
    // Lossy formats never match exactly across encoders: require
    // structural fidelity (PSNR floor) instead. Lossless pairs
    // must match bit-exact after layout normalization.
    if !expected_lossy && !actual_lossy {
        return fail(format!(
            "pixels differ: {}x{} lossless mismatch",
            expected_frame.width(),
            expected_frame.height(),
        ));
    }
    let psnr = psnr_db(expected_rgba.pixels(), actual_rgba.pixels());
    if psnr >= 30.0 {
        return AssertionResult {
            kind: "image-pixels-equal",
            passed: true,
            detail: String::new(),
        };
    }
    fail(format!(
        "pixels differ: {}x{} PSNR {psnr:.1} dB",
        expected_frame.width(),
        expected_frame.height(),
    ))
}

/// Peak signal-to-noise ratio in dB (lossy cross-encoder
/// fidelity; mirrors the ps-image codec floor).
fn psnr_db(a: &[u8], b: &[u8]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut sum_squares: u64 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        let diff = (*x as i32 - *y as i32) as i64;
        sum_squares += (diff * diff) as u64;
    }
    if sum_squares == 0 {
        return f64::INFINITY;
    }
    let mse = sum_squares as f64 / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

/// One page-corpus case: fetch `url`, rewrite `html` under
/// `directives`, then check `assertions`.
#[derive(Debug, Clone)]
pub struct PageCase {
    /// Case name.
    pub name: String,
    /// Request URL.
    pub url: String,
    /// Origin HTML.
    pub html: Vec<u8>,
    /// Extra origin files as (path, bytes) pairs.
    pub extra: Vec<(String, Vec<u8>)>,
    /// Pin the origin mtime (stable ETag/Last-Modified).
    pub fixed_mtime: bool,
    /// Extra request headers as (name, value) pairs.
    pub request_headers: Vec<(String, String)>,
    /// Directives as (name, value) pairs.
    pub directives: Vec<(String, String)>,
    /// Expectations over the rewritten response.
    pub assertions: Vec<Assertion>,
}

/// One config-corpus case: apply `directives`, then check
/// `assertions` (usually stat or key vectors).
#[derive(Debug, Clone)]
pub struct ConfigCase {
    /// Case name.
    pub name: String,
    /// Directives as (name, value) pairs.
    pub directives: Vec<(String, String)>,
    /// Expectations over the configured build.
    pub assertions: Vec<Assertion>,
}

/// A corpus: page cases plus config cases.
#[derive(Debug, Clone, Default)]
pub struct Corpus {
    /// Page-corpus cases, run in order.
    pub pages: Vec<PageCase>,
    /// Config-corpus cases, run in order after the pages.
    pub configs: Vec<ConfigCase>,
}

/// Runs corpus cases (`harness interface`). Phase 4 provides
/// the nginx-driving implementation; this trait pins the
/// shapes it must satisfy.
pub trait Runner {
    /// Runs one page case, returning one result per assertion.
    fn run_page(&self, case: &PageCase) -> Vec<AssertionResult>;
    /// Runs one config case, returning one result per assertion.
    fn run_config(&self, case: &ConfigCase) -> Vec<AssertionResult>;
    /// Runs a whole corpus, pages then configs, concatenating
    /// the per-case results in order.
    fn run_corpus(&self, corpus: &Corpus) -> Vec<AssertionResult> {
        let mut results = Vec::new();
        for case in &corpus.pages {
            results.extend(self.run_page(case));
        }
        for case in &corpus.configs {
            results.extend(self.run_config(case));
        }
        results
    }
}

/// Reports every case as unevaluated. Useful as a harness
/// smoke before Phase 4 lands a real runner.
#[derive(Debug, Default)]
pub struct NullRunner;

impl Runner for NullRunner {
    fn run_page(&self, case: &PageCase) -> Vec<AssertionResult> {
        case.assertions
            .iter()
            .map(|_| AssertionResult {
                kind: "unevaluated",
                passed: false,
                detail: format!("no runner for page case {}", case.name),
            })
            .collect()
    }

    fn run_config(&self, case: &ConfigCase) -> Vec<AssertionResult> {
        case.assertions
            .iter()
            .map(|_| AssertionResult {
                kind: "unevaluated",
                passed: false,
                detail: format!("no runner for config case {}", case.name),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One-pixel PNG with the given channels (via our own
    /// encoder, so fixtures never rot).
    fn tiny_png(r: u8, g: u8, b: u8) -> Vec<u8> {
        let frame = ps_image::frame::Frame::from_parts(
            1,
            1,
            ps_image::frame::PixelFormat::Rgb888,
            vec![r, g, b],
        )
        .expect("1x1 frame");
        ps_image::png::encode(&frame, &ps_image::png::PngEncodeOptions::default())
            .expect("png encodes")
    }

    #[test]
    fn assertions_evaluate_both_ways() {
        let passing = vec![
            Assertion::ByteEqual {
                expected: vec![1, 2],
                actual: vec![1, 2],
            },
            Assertion::HeaderEqual {
                name: "Content-Type".to_string(),
                expected: "text/html".to_string(),
                actual: "text/html".to_string(),
            },
            Assertion::StatDeltaEqual {
                name: "image_rewrites".to_string(),
                before: 3,
                after: 5,
                expected_delta: 2,
            },
            Assertion::CacheKeyEqual {
                expected: "k".to_string(),
                actual: "k".to_string(),
            },
            Assertion::ImagePixelsEqual {
                expected: tiny_png(1, 0, 0),
                actual: tiny_png(1, 0, 0),
            },
        ];
        for assertion in &passing {
            let result = evaluate(assertion);
            assert!(result.passed, "{assertion:?}");
            assert!(result.detail.is_empty());
        }
        let failing = vec![
            Assertion::ByteEqual {
                expected: vec![1],
                actual: vec![2],
            },
            Assertion::StatDeltaEqual {
                name: "image_rewrites".to_string(),
                before: 3,
                after: 4,
                expected_delta: 2,
            },
            Assertion::ImagePixelsEqual {
                expected: tiny_png(255, 0, 0),
                actual: tiny_png(0, 0, 255),
            },
            // Lossless pairs reject even one LSB of drift.
            Assertion::ImagePixelsEqual {
                expected: tiny_png(1, 0, 0),
                actual: tiny_png(2, 0, 0),
            },
            Assertion::ImagePixelsEqual {
                expected: vec![0, 1, 2],
                actual: vec![0, 1, 2],
            },
        ];
        for assertion in &failing {
            let result = evaluate(assertion);
            assert!(!result.passed, "{assertion:?}");
            assert!(!result.detail.is_empty(), "{assertion:?}");
        }
    }

    #[test]
    fn null_runner_reports_every_case() {
        let runner = NullRunner;
        let page = PageCase {
            name: "p".to_string(),
            url: "http://x/".to_string(),
            html: vec![],
            extra: vec![],
            fixed_mtime: true,
            request_headers: vec![],
            directives: vec![],
            assertions: vec![Assertion::ByteEqual {
                expected: vec![],
                actual: vec![],
            }],
        };
        let results = runner.run_page(&page);
        assert_eq!(1, results.len());
        assert!(!results[0].passed);
        assert_eq!("unevaluated", results[0].kind);
    }

    #[test]
    fn corpus_runs_pages_then_configs() {
        let runner = NullRunner;
        let byte = Assertion::ByteEqual {
            expected: vec![],
            actual: vec![],
        };
        let key = Assertion::CacheKeyEqual {
            expected: "k".to_string(),
            actual: "k".to_string(),
        };
        let corpus = Corpus {
            pages: vec![PageCase {
                name: "p".to_string(),
                url: "http://x/".to_string(),
                html: vec![],
                extra: vec![],
                fixed_mtime: true,
                request_headers: vec![],
                directives: vec![],
                assertions: vec![byte],
            }],
            configs: vec![ConfigCase {
                name: "c".to_string(),
                directives: vec![],
                assertions: vec![key],
            }],
        };
        let results = runner.run_corpus(&corpus);
        assert_eq!(2, results.len());
        assert!(results.iter().all(|r| r.kind == "unevaluated"));
        assert!(results[0].detail.contains('p'));
        assert!(results[1].detail.contains('c'));
    }
}
