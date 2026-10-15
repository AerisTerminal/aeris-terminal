//! Owner clock and deterministic trading-time control.

use std::{
    fmt,
    sync::atomic::{AtomicI64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Wall time used for durable event stamps and monotonic time used for deadlines.
pub trait Clock: Send + Sync + fmt::Debug {
    fn wall_unix_nanos(&self) -> i64;
    fn monotonic(&self) -> Duration;
}

#[derive(Debug)]
pub struct SystemClock {
    started: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn wall_unix_nanos(&self) -> i64 {
        system_unix_nanos()
    }

    fn monotonic(&self) -> Duration {
        self.started.elapsed()
    }
}

pub(crate) fn system_unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

#[derive(Debug)]
pub struct ManualClock {
    wall: AtomicI64,
    monotonic_nanos: AtomicI64,
}

impl ManualClock {
    #[must_use]
    pub fn new(wall_unix_nanos: i64) -> Self {
        Self {
            wall: AtomicI64::new(wall_unix_nanos),
            monotonic_nanos: AtomicI64::new(0),
        }
    }

    pub fn set_wall_unix_nanos(&self, value: i64) {
        self.wall.store(value, Ordering::Release);
    }

    pub fn advance(&self, duration: Duration) {
        let delta = i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX);
        self.wall.fetch_add(delta, Ordering::AcqRel);
        self.monotonic_nanos.fetch_add(delta, Ordering::AcqRel);
    }
}

impl Clock for ManualClock {
    fn wall_unix_nanos(&self) -> i64 {
        self.wall.load(Ordering::Acquire)
    }

    fn monotonic(&self) -> Duration {
        Duration::from_nanos(
            u64::try_from(self.monotonic_nanos.load(Ordering::Acquire).max(0)).unwrap_or(0),
        )
    }
}
