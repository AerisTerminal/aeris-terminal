//! Bounded provider reconnect scheduling; the owner supplies the time source.

use std::{num::NonZeroU32, time::Duration};

use super::clock::Clock;

#[derive(Clone, Copy)]
pub(crate) enum Jitter {
    Full,
    Equal,
}

#[derive(Clone, Copy)]
pub(crate) struct ReconnectPolicy {
    initial: Duration,
    multiplier: u32,
    cap: Duration,
    jitter: Jitter,
    budget: Option<NonZeroU32>,
    healthy_reset_after: Duration,
}

impl ReconnectPolicy {
    pub(crate) const fn initial_delay(self) -> Duration {
        self.initial
    }
    pub(crate) const fn new(
        initial: Duration,
        multiplier: u32,
        cap: Duration,
        jitter: Jitter,
        budget: Option<NonZeroU32>,
        healthy_reset_after: Duration,
    ) -> Self {
        Self {
            initial,
            multiplier,
            cap,
            jitter,
            budget,
            healthy_reset_after,
        }
    }

    pub(crate) const fn ctrader() -> Self {
        Self::new(
            Duration::from_secs(3),
            2,
            Duration::from_secs(48),
            Jitter::Equal,
            NonZeroU32::new(5),
            Duration::from_mins(1),
        )
    }

    pub(crate) const fn tastytrade() -> Self {
        Self::new(
            Duration::from_secs(6),
            2,
            Duration::from_secs(48),
            Jitter::Equal,
            None,
            Duration::from_mins(1),
        )
    }

    pub(crate) const fn hyperliquid() -> Self {
        Self::new(
            Duration::from_millis(500),
            2,
            Duration::from_secs(30),
            Jitter::Full,
            None,
            Duration::from_mins(1),
        )
    }

    pub(crate) const fn rithmic_recovery() -> Self {
        Self::new(
            Duration::from_secs(1),
            2,
            Duration::from_secs(30),
            Jitter::Equal,
            None,
            Duration::from_mins(1),
        )
    }

    pub(crate) const fn venue() -> Self {
        Self::new(
            Duration::from_secs(5),
            2,
            Duration::from_secs(300),
            Jitter::Equal,
            None,
            Duration::from_mins(1),
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RetryClass {
    Transient,
    RateLimited { retry_after: Duration },
    Fatal,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RetryDecision {
    Wait(Duration),
    Failed { attempts: u32 },
    Fatal,
}

pub(crate) struct Backoff {
    policy: ReconnectPolicy,
    attempts: u32,
    random: u64,
}

impl Backoff {
    pub(crate) fn new(policy: ReconnectPolicy, clock: &dyn Clock, seed: u64) -> Self {
        // Wall clock and caller-supplied identity make separate workers independent
        // while a replayed clock and seed always reproduce the same sequence.
        let random = clock.wall_nanos().unwrap_or_default().cast_unsigned() ^ seed;
        Self {
            policy,
            attempts: 0,
            random,
        }
    }

    pub(crate) fn attempts(&self) -> u32 {
        self.attempts
    }

    pub(crate) fn reset(&mut self) {
        self.attempts = 0;
    }

    pub(crate) fn healthy_for(&mut self, since: std::time::Instant, clock: &dyn Clock) {
        if clock.monotonic().saturating_duration_since(since) >= self.policy.healthy_reset_after {
            self.reset();
        }
    }

    pub(crate) fn failure(&mut self, class: RetryClass) -> RetryDecision {
        if matches!(class, RetryClass::Fatal) {
            return RetryDecision::Fatal;
        }
        if let RetryClass::RateLimited { retry_after } = class {
            return RetryDecision::Wait(retry_after);
        }
        self.attempts = self.attempts.saturating_add(1);
        if self
            .policy
            .budget
            .is_some_and(|budget| self.attempts >= budget.get())
        {
            return RetryDecision::Failed {
                attempts: self.attempts,
            };
        }
        let mut ceiling = self.policy.initial;
        for _ in 1..self.attempts.min(32) {
            ceiling = ceiling
                .saturating_mul(self.policy.multiplier)
                .min(self.policy.cap);
        }
        ceiling = ceiling.min(self.policy.cap);
        // SplitMix64, with rejection-free mapping into an inclusive nanosecond
        // interval. Modulo bias is immaterial to a retry scheduler, but all
        // arithmetic stays bounded even at the duration cap.
        self.random = self.random.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.random;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        let upper = u64::try_from(ceiling.as_nanos()).unwrap_or(u64::MAX);
        let lower = match self.policy.jitter {
            Jitter::Full => 0,
            Jitter::Equal => upper / 2,
        };
        let span = upper - lower;
        RetryDecision::Wait(Duration::from_nanos(lower + value % (span + 1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_service::clock::ScriptedClock;

    #[test]
    fn reconnect_schedules_respect_bounds_and_replay_seed() {
        let clock = ScriptedClock::new(42);
        for (policy, initial, cap, equal) in [
            (ReconnectPolicy::hyperliquid(), 500_u64, 30_000_u64, false),
            (ReconnectPolicy::ctrader(), 3_000, 48_000, true),
            (ReconnectPolicy::tastytrade(), 6_000, 48_000, true),
        ] {
            let mut first = Backoff::new(policy, &clock, 7);
            let mut replay = Backoff::new(policy, &clock, 7);
            let mut different = Backoff::new(policy, &clock, 8);
            let mut changed = false;
            for attempt in 1..=if equal && initial == 3_000 { 4 } else { 20 } {
                let upper = initial.saturating_mul(1_u64 << (attempt - 1)).min(cap);
                let RetryDecision::Wait(delay) = first.failure(RetryClass::Transient) else {
                    panic!("attempt should be scheduled");
                };
                assert!(delay.as_millis() >= if equal { u128::from(upper / 2) } else { 0 });
                assert!(delay.as_millis() <= u128::from(upper));
                assert_eq!(
                    replay.failure(RetryClass::Transient),
                    RetryDecision::Wait(delay)
                );
                changed |= different.failure(RetryClass::Transient) != RetryDecision::Wait(delay);
            }
            assert!(changed, "seed must affect jitter");
        }
    }

    #[test]
    fn reconnect_budget_rate_limit_fatal_and_healthy_reset() {
        let clock = ScriptedClock::new(1);
        let mut backoff = Backoff::new(ReconnectPolicy::ctrader(), &clock, 2);
        assert_eq!(
            backoff.failure(RetryClass::RateLimited {
                retry_after: Duration::from_secs(123)
            }),
            RetryDecision::Wait(Duration::from_secs(123))
        );
        let healthy_since = clock.monotonic();
        clock.advance(Duration::from_mins(1));
        backoff.healthy_for(healthy_since, &clock);
        assert_eq!(backoff.attempts(), 0);
        for _ in 0..4 {
            assert!(matches!(
                backoff.failure(RetryClass::Transient),
                RetryDecision::Wait(_)
            ));
        }
        assert_eq!(
            backoff.failure(RetryClass::Transient),
            RetryDecision::Failed { attempts: 5 }
        );
        assert_eq!(backoff.failure(RetryClass::Fatal), RetryDecision::Fatal);
    }

    #[test]
    fn hyperliquid_healthy_minute_restores_the_first_half_second_step() {
        let clock = ScriptedClock::new(123);
        let mut backoff = Backoff::new(ReconnectPolicy::hyperliquid(), &clock, 9);
        for _ in 0..20 {
            assert!(matches!(
                backoff.failure(RetryClass::Transient),
                RetryDecision::Wait(_)
            ));
        }
        assert_eq!(backoff.attempts(), 20);
        let healthy_since = clock.monotonic();
        clock.advance(Duration::from_mins(1));
        backoff.healthy_for(healthy_since, &clock);
        assert_eq!(backoff.attempts(), 0);
        let RetryDecision::Wait(delay) = backoff.failure(RetryClass::Transient) else {
            panic!("Hyperliquid reconnect has no attempt budget");
        };
        assert!(delay <= Duration::from_millis(500));
    }
}
