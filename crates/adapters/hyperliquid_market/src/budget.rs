//! Process-wide request weight budget for the public info endpoint.
//!
//! Hyperliquid limits REST traffic per IP address to an aggregated 1,200
//! weight per minute: the info requests this adapter sends weigh 20 each and
//! `candleSnapshot` adds one per 60 candles returned. Charts, the catalog and
//! the market screen all draw from this one budget, so together they wait for
//! capacity instead of being rejected with HTTP 429 and retrying into the limit.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The provider's rate-limit window.
const WINDOW: Duration = Duration::from_secs(60);
/// Weight spent in any window: below the documented 1,200 so other software
/// on the same address keeps headroom.
const WINDOW_WEIGHT: u32 = 1_000;
/// Weight of every info request this adapter sends.
const INFO_REQUEST_WEIGHT: u32 = 20;
/// `candleSnapshot` adds one weight per this many returned candles.
const CANDLES_PER_WEIGHT: usize = 60;
/// Longest a request waits for budget before failing as rate limited.
const MAXIMUM_BUDGET_WAIT: Duration = Duration::from_secs(10);
/// Longest single sleep, so cancellation is observed promptly.
const BUDGET_POLL: Duration = Duration::from_millis(100);

static BUDGET: Mutex<WeightWindow> = Mutex::new(WeightWindow::new());

/// Spends one info request's weight, waiting while earlier requests leave no
/// capacity in the window.
///
/// # Errors
///
/// Returns an error when `stop` is raised while waiting, or when the budget
/// cannot cover the request within the bounded wait.
pub(crate) fn spend_info_request(stop: Option<&AtomicBool>) -> Result<(), String> {
    spend(&BUDGET, INFO_REQUEST_WEIGHT, stop, MAXIMUM_BUDGET_WAIT)
}

/// Charges the additional weight of a returned candle page.
pub(crate) fn charge_candles(count: usize) {
    lock(&BUDGET).record(candle_weight(count), Instant::now());
}

/// Rounded up so the charge never undercounts the provider's.
fn candle_weight(count: usize) -> u32 {
    u32::try_from(count.div_ceil(CANDLES_PER_WEIGHT)).unwrap_or(u32::MAX)
}

/// The provider rejected a request for exceeding its limit, which other
/// software on the address may have spent: treat the window as full from now.
pub(crate) fn provider_limited() {
    lock(&BUDGET).fill(Instant::now());
}

fn spend(
    budget: &Mutex<WeightWindow>,
    weight: u32,
    stop: Option<&AtomicBool>,
    maximum_wait: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let now = Instant::now();
        let Err(wait) = lock(budget).try_spend(weight, now) else {
            return Ok(());
        };
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            return Err("hyperliquid info request was cancelled".to_string());
        }
        if now.saturating_duration_since(started) + wait > maximum_wait {
            return Err("hyperliquid info request budget is exhausted".to_string());
        }
        std::thread::sleep(wait.min(BUDGET_POLL));
    }
}

fn lock(budget: &Mutex<WeightWindow>) -> std::sync::MutexGuard<'_, WeightWindow> {
    // The window holds only counters, so a panic elsewhere leaves it usable.
    budget.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Weight spent during the trailing window, oldest first.
struct WeightWindow {
    spent: VecDeque<(Instant, u32)>,
    total: u32,
}

impl WeightWindow {
    const fn new() -> Self {
        Self {
            spent: VecDeque::new(),
            total: 0,
        }
    }

    /// Spends `weight` at `now` when it fits in the window, or returns how
    /// long until enough earlier spending expires.
    fn try_spend(&mut self, weight: u32, now: Instant) -> Result<(), Duration> {
        self.expire(now);
        if self.total.saturating_add(weight) <= WINDOW_WEIGHT {
            self.record(weight, now);
            return Ok(());
        }
        let mut remaining = self.total;
        for &(at, spent) in &self.spent {
            remaining = remaining.saturating_sub(spent);
            if remaining.saturating_add(weight) <= WINDOW_WEIGHT {
                return Err((at + WINDOW).saturating_duration_since(now));
            }
        }
        Err(WINDOW)
    }

    fn record(&mut self, weight: u32, now: Instant) {
        if weight > 0 {
            self.spent.push_back((now, weight));
            self.total = self.total.saturating_add(weight);
        }
    }

    fn fill(&mut self, now: Instant) {
        self.expire(now);
        self.record(WINDOW_WEIGHT.saturating_sub(self.total), now);
    }

    fn expire(&mut self, now: Instant) {
        while let Some(&(at, weight)) = self.spent.front() {
            if now.saturating_duration_since(at) < WINDOW {
                break;
            }
            self.spent.pop_front();
            self.total = self.total.saturating_sub(weight);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_window_waits_for_its_oldest_spend_to_expire() {
        let start = Instant::now();
        let mut window = WeightWindow::new();
        for _ in 0..WINDOW_WEIGHT / INFO_REQUEST_WEIGHT {
            assert_eq!(window.try_spend(INFO_REQUEST_WEIGHT, start), Ok(()));
        }
        let later = start + Duration::from_secs(10);
        assert_eq!(
            window.try_spend(INFO_REQUEST_WEIGHT, later),
            Err(Duration::from_secs(50))
        );
        assert_eq!(
            window.try_spend(INFO_REQUEST_WEIGHT, start + WINDOW),
            Ok(())
        );
        assert_eq!(window.total, INFO_REQUEST_WEIGHT);
    }

    #[test]
    fn candle_pages_and_provider_rejections_count_against_the_window() {
        let start = Instant::now();
        let mut window = WeightWindow::new();
        assert_eq!(candle_weight(1_599), 27);
        assert_eq!(candle_weight(5_000), 84);
        assert_eq!(window.try_spend(INFO_REQUEST_WEIGHT, start), Ok(()));
        window.record(candle_weight(5_000), start);
        assert_eq!(window.total, 104);

        let rejected = start + Duration::from_secs(30);
        window.fill(rejected);
        assert_eq!(window.total, WINDOW_WEIGHT);
        assert_eq!(
            window.try_spend(INFO_REQUEST_WEIGHT, rejected),
            Err(Duration::from_secs(30)),
            "the rejection holds the window until earlier spending expires"
        );
        assert_eq!(
            window.try_spend(INFO_REQUEST_WEIGHT, start + WINDOW),
            Ok(())
        );
        assert_eq!(window.total, WINDOW_WEIGHT - 104 + INFO_REQUEST_WEIGHT);

        let mut idle = WeightWindow::new();
        idle.fill(start);
        assert_eq!(
            idle.try_spend(INFO_REQUEST_WEIGHT, start),
            Err(WINDOW),
            "a rejection Aeris did not cause blocks the whole window"
        );
    }

    #[test]
    fn waiting_ends_on_cancellation_or_when_the_wait_exceeds_its_bound() {
        let budget = Mutex::new(WeightWindow::new());
        lock(&budget).fill(Instant::now());
        let stop = AtomicBool::new(true);
        assert_eq!(
            spend(&budget, INFO_REQUEST_WEIGHT, Some(&stop), WINDOW),
            Err("hyperliquid info request was cancelled".to_string())
        );
        assert_eq!(
            spend(&budget, INFO_REQUEST_WEIGHT, None, Duration::from_secs(1)),
            Err("hyperliquid info request budget is exhausted".to_string())
        );
    }
}
