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
    }
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
