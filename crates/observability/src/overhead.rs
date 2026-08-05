use core::fmt;
use std::error::Error;

/// Maximum allowed p99 detailed-diagnostics regression in basis points (5%).
pub const MAXIMUM_P99_REGRESSION_BASIS_POINTS: u64 = 500;
/// Maximum allowed p99.9 detailed-diagnostics regression in basis points (10%).
pub const MAXIMUM_P99_9_REGRESSION_BASIS_POINTS: u64 = 1_000;

const MAXIMUM_BENCHMARK_LABEL_BYTES: usize = 256;

/// Named environment required before diagnostics overhead evidence is comparable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsBenchmarkContext {
    pub hardware: String,
    pub operating_system: String,
    pub workload: String,
    pub warm_up_samples: u64,
    pub measured_samples: u64,
}

impl DiagnosticsBenchmarkContext {
    /// Validates named, bounded benchmark context and nonzero sample windows.
    ///
    /// # Errors
    ///
    /// Returns a redacted field-class error for incomplete benchmark evidence.
    pub fn validate(&self) -> Result<(), DiagnosticsOverheadError> {
        for (field, value) in [
            ("hardware", self.hardware.as_str()),
            ("operating_system", self.operating_system.as_str()),
            ("workload", self.workload.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(DiagnosticsOverheadError::EmptyContext(field));
            }
            if value.len() > MAXIMUM_BENCHMARK_LABEL_BYTES {
                return Err(DiagnosticsOverheadError::ContextTooLong {
                    field,
                    maximum: MAXIMUM_BENCHMARK_LABEL_BYTES,
                });
            }
            if value.chars().any(char::is_control) {
                return Err(DiagnosticsOverheadError::InvalidContext(field));
            }
        }
        if self.warm_up_samples == 0 || self.measured_samples == 0 {
            return Err(DiagnosticsOverheadError::EmptySampleWindow);
        }
        Ok(())
    }
}

/// Required percentile and semantic-outcome evidence for one benchmark arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticsBenchmarkArm {
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    pub p99_nanos: u64,
    pub p99_9_nanos: u64,
    pub maximum_nanos: u64,
    pub sample_count: u64,
    pub gaps: u64,
    pub overflows: u64,
    pub recoveries: u64,
}

impl DiagnosticsBenchmarkArm {
    fn validate(self, expected_samples: u64) -> Result<(), DiagnosticsOverheadError> {
        if self.sample_count != expected_samples {
            return Err(DiagnosticsOverheadError::SampleCountMismatch {
                expected: expected_samples,
                actual: self.sample_count,
            });
        }
        if self.p50_nanos > self.p95_nanos
            || self.p95_nanos > self.p99_nanos
            || self.p99_nanos > self.p99_9_nanos
            || self.p99_9_nanos > self.maximum_nanos
        {
            return Err(DiagnosticsOverheadError::InvalidPercentileOrder);
        }
        Ok(())
    }
}

/// Complete baseline and detailed-diagnostics benchmark evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsOverheadEvidence {
    pub context: DiagnosticsBenchmarkContext,
    pub baseline: DiagnosticsBenchmarkArm,
    pub detailed: DiagnosticsBenchmarkArm,
}

impl DiagnosticsOverheadEvidence {
    /// Validates comparable evidence and calculates the declared p99 budgets.
    ///
    /// # Errors
    ///
    /// Returns an error for incomplete context, malformed percentiles, different
    /// sample counts, semantic divergence, or an unusable zero baseline.
    pub fn assess(&self) -> Result<DiagnosticsOverheadAssessment, DiagnosticsOverheadError> {
        self.context.validate()?;
        self.baseline.validate(self.context.measured_samples)?;
        self.detailed.validate(self.context.measured_samples)?;
        if (
            self.baseline.gaps,
            self.baseline.overflows,
            self.baseline.recoveries,
        ) != (
            self.detailed.gaps,
            self.detailed.overflows,
            self.detailed.recoveries,
        ) {
            return Err(DiagnosticsOverheadError::SemanticOutcomeMismatch);
        }
        let p99_regression_basis_points =
            regression_basis_points(self.baseline.p99_nanos, self.detailed.p99_nanos, "p99")?;
        let p99_9_regression_basis_points = regression_basis_points(
            self.baseline.p99_9_nanos,
            self.detailed.p99_9_nanos,
            "p99.9",
        )?;
        Ok(DiagnosticsOverheadAssessment {
            p99_regression_basis_points,
            p99_9_regression_basis_points,
            passes: p99_regression_basis_points <= MAXIMUM_P99_REGRESSION_BASIS_POINTS
                && p99_9_regression_basis_points <= MAXIMUM_P99_9_REGRESSION_BASIS_POINTS,
        })
    }
}

/// Exact integer assessment against the roadmap's detailed-diagnostics budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticsOverheadAssessment {
    pub p99_regression_basis_points: u64,
    pub p99_9_regression_basis_points: u64,
    pub passes: bool,
}

fn regression_basis_points(
    baseline: u64,
    detailed: u64,
    percentile: &'static str,
) -> Result<u64, DiagnosticsOverheadError> {
    if baseline == 0 {
        return Err(DiagnosticsOverheadError::ZeroBaseline(percentile));
    }
    if detailed <= baseline {
        return Ok(0);
    }
    let increase = u128::from(detailed.saturating_sub(baseline));
    let rounded_up = increase
        .saturating_mul(10_000)
        .saturating_add(u128::from(baseline).saturating_sub(1))
        / u128::from(baseline);
    Ok(u64::try_from(rounded_up).unwrap_or(u64::MAX))
}

/// Redacted failure classes for diagnostics overhead evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticsOverheadError {
    EmptyContext(&'static str),
    ContextTooLong { field: &'static str, maximum: usize },
    InvalidContext(&'static str),
    EmptySampleWindow,
    SampleCountMismatch { expected: u64, actual: u64 },
    InvalidPercentileOrder,
    SemanticOutcomeMismatch,
    ZeroBaseline(&'static str),
}

impl fmt::Display for DiagnosticsOverheadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "diagnostics overhead evidence rejected: {self:?}"
        )
    }
}

impl Error for DiagnosticsOverheadError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> DiagnosticsBenchmarkContext {
        DiagnosticsBenchmarkContext {
            hardware: "named-test-cpu".to_string(),
            operating_system: "named-test-os".to_string(),
            workload: "one-million-canonical-events".to_string(),
            warm_up_samples: 10_000,
            measured_samples: 1_000_000,
        }
    }

    fn arm(p99: u64, p99_9: u64) -> DiagnosticsBenchmarkArm {
        DiagnosticsBenchmarkArm {
            p50_nanos: 50,
            p95_nanos: 90,
            p99_nanos: p99,
            p99_9_nanos: p99_9,
            maximum_nanos: p99_9.saturating_add(10),
            sample_count: 1_000_000,
            gaps: 2,
            overflows: 1,
            recoveries: 3,
        }
    }

    #[test]
    fn exact_thresholds_pass_and_larger_regressions_fail() {
        let passing = DiagnosticsOverheadEvidence {
            context: context(),
            baseline: arm(100, 100),
            detailed: arm(105, 110),
        }
        .assess()
        .expect("threshold evidence is comparable");
        assert_eq!(passing.p99_regression_basis_points, 500);
        assert_eq!(passing.p99_9_regression_basis_points, 1_000);
        assert!(passing.passes);

        let failing = DiagnosticsOverheadEvidence {
            context: context(),
            baseline: arm(100, 100),
            detailed: arm(106, 111),
        }
        .assess()
        .expect("over-budget evidence remains comparable");
        assert!(!failing.passes);
    }

    #[test]
    fn improvements_report_zero_regression() {
        let assessment = DiagnosticsOverheadEvidence {
            context: context(),
            baseline: arm(200, 300),
            detailed: arm(150, 250),
        }
        .assess()
        .expect("improvement evidence is comparable");
        assert_eq!(assessment.p99_regression_basis_points, 0);
        assert_eq!(assessment.p99_9_regression_basis_points, 0);
        assert!(assessment.passes);
    }

    #[test]
    fn zero_percentile_baselines_are_never_comparable() {
        let zero = DiagnosticsBenchmarkArm {
            p50_nanos: 0,
            p95_nanos: 0,
            p99_nanos: 0,
            p99_9_nanos: 0,
            maximum_nanos: 0,
            ..arm(100, 100)
        };
        assert_eq!(
            DiagnosticsOverheadEvidence {
                context: context(),
                baseline: zero,
                detailed: zero,
            }
            .assess(),
            Err(DiagnosticsOverheadError::ZeroBaseline("p99"))
        );
    }

    #[test]
    fn evidence_requires_equal_semantics_and_sample_windows() {
        let mut mismatched_samples = arm(100, 100);
        mismatched_samples.sample_count -= 1;
        assert_eq!(
            DiagnosticsOverheadEvidence {
                context: context(),
                baseline: arm(100, 100),
                detailed: mismatched_samples,
            }
            .assess(),
            Err(DiagnosticsOverheadError::SampleCountMismatch {
                expected: 1_000_000,
                actual: 999_999,
            })
        );

        let mut mismatched_semantics = arm(100, 100);
        mismatched_semantics.gaps += 1;
        assert_eq!(
            DiagnosticsOverheadEvidence {
                context: context(),
                baseline: arm(100, 100),
                detailed: mismatched_semantics,
            }
            .assess(),
            Err(DiagnosticsOverheadError::SemanticOutcomeMismatch)
        );
    }

    #[test]
    fn malformed_context_and_percentiles_fail_closed_without_echoing_values() {
        let secret = "secret-account\n";
        let mut invalid_context = context();
        invalid_context.hardware = secret.to_string();
        let error = DiagnosticsOverheadEvidence {
            context: invalid_context,
            baseline: arm(100, 100),
            detailed: arm(100, 100),
        }
        .assess()
        .expect_err("control-bearing context is rejected");
        assert!(!format!("{error:?} {error}").contains(secret));

        let malformed = DiagnosticsBenchmarkArm {
            p50_nanos: 100,
            p95_nanos: 90,
            ..arm(100, 100)
        };
        assert_eq!(
            DiagnosticsOverheadEvidence {
                context: context(),
                baseline: malformed,
                detailed: arm(100, 100),
            }
            .assess(),
            Err(DiagnosticsOverheadError::InvalidPercentileOrder)
        );
    }
}
