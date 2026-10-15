//! Single owner-side session and quote eligibility policy.
use aeris_instruments::{InstrumentClass, SessionPhase};
use aeris_trading::RejectReason;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionPolicyError {
    InvalidMaxQuoteAge,
}

impl std::fmt::Display for ExecutionPolicyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("maximum quote age must be positive")
    }
}

impl std::error::Error for ExecutionPolicyError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionPolicy {
    max_quote_age_ms: [i64; 7],
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            max_quote_age_ms: [60_000, 60_000, 60_000, 60_000, 10_000, 10_000, 60_000],
        }
    }
}

impl ExecutionPolicy {
    #[must_use]
    pub const fn max_quote_age(self, class: InstrumentClass) -> i64 {
        self.max_quote_age_ms[class_index(class)]
    }

    /// Overrides exactly one instrument class without changing the other classes.
    ///
    /// # Errors
    /// Rejects a non-positive age limit.
    pub fn with_max_quote_age(
        mut self,
        class: InstrumentClass,
        age_ms: i64,
    ) -> Result<Self, ExecutionPolicyError> {
        if age_ms <= 0 {
            return Err(ExecutionPolicyError::InvalidMaxQuoteAge);
        }
        self.max_quote_age_ms[class_index(class)] = age_ms;
        Ok(self)
    }

    pub(crate) fn validate(self) -> Result<Self, ExecutionPolicyError> {
        if self.max_quote_age_ms.iter().any(|age| *age <= 0) {
            return Err(ExecutionPolicyError::InvalidMaxQuoteAge);
        }
        Ok(self)
    }
}

const fn class_index(class: InstrumentClass) -> usize {
    match class {
        InstrumentClass::Equity => 0,
        InstrumentClass::Option => 1,
        InstrumentClass::Future => 2,
        InstrumentClass::ForeignExchange => 3,
        InstrumentClass::CryptoSpot => 4,
        InstrumentClass::CryptoPerpetual => 5,
        InstrumentClass::Other => 6,
    }
}

pub(crate) fn tradable_phase(
    phase: SessionPhase,
    next_open: Option<i64>,
) -> Result<(), RejectReason> {
    match phase {
        SessionPhase::Open | SessionPhase::Extended | SessionPhase::AlwaysOpen => Ok(()),
        SessionPhase::Unknown => Err(RejectReason::SessionUnknown),
        SessionPhase::PreOpen | SessionPhase::Break | SessionPhase::Closed => {
            Err(RejectReason::MarketClosed { phase, next_open })
        }
    }
}

pub(crate) fn fresh_quote(
    now: i64,
    received: i64,
    class: InstrumentClass,
    policy: ExecutionPolicy,
) -> Result<(), RejectReason> {
    let age_ms = now.saturating_sub(received).max(0) / 1_000_000;
    let limit_ms = policy.max_quote_age(class);
    if age_ms > limit_ms {
        Err(RejectReason::QuoteStale { age_ms, limit_ms })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_uses_inclusive_class_boundaries_and_overrides() {
        let policy = ExecutionPolicy::default();
        let now = 100_000_000_000;
        for (class, limit) in [
            (InstrumentClass::CryptoPerpetual, 10_000),
            (InstrumentClass::CryptoSpot, 10_000),
            (InstrumentClass::Future, 60_000),
            (InstrumentClass::Equity, 60_000),
        ] {
            assert_eq!(
                fresh_quote(now, now - limit * 1_000_000, class, policy),
                Ok(())
            );
            assert_eq!(
                fresh_quote(now, now - (limit + 1) * 1_000_000, class, policy),
                Err(RejectReason::QuoteStale {
                    age_ms: limit + 1,
                    limit_ms: limit,
                })
            );
        }
        let shorter = policy
            .with_max_quote_age(InstrumentClass::CryptoSpot, 2_000)
            .unwrap();
        assert_eq!(
            shorter.max_quote_age(InstrumentClass::CryptoPerpetual),
            10_000
        );
        assert_eq!(shorter.max_quote_age(InstrumentClass::Future), 60_000);
        assert!(
            fresh_quote(
                now,
                now - 2_001_000_000,
                InstrumentClass::CryptoSpot,
                shorter
            )
            .is_err()
        );
    }
}
