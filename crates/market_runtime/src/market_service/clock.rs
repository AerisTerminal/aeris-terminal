//! Time source for coordinator-owned deadlines and market session decisions.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) trait Clock: Send + Sync {
    fn monotonic(&self) -> Instant;
    fn wall_nanos(&self) -> Option<i64>;
}

pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn monotonic(&self) -> Instant {
        Instant::now()
    }

    fn wall_nanos(&self) -> Option<i64> {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos(),
        )
        .ok()
    }
}

#[cfg(test)]
pub(super) struct ScriptedClock {
    origin: Instant,
    elapsed: std::sync::Mutex<std::time::Duration>,
    wall_nanos: std::sync::atomic::AtomicI64,
}

#[cfg(test)]
impl ScriptedClock {
    pub(super) fn new(wall_nanos: i64) -> Self {
        Self {
            origin: Instant::now(),
            elapsed: std::sync::Mutex::new(std::time::Duration::ZERO),
            wall_nanos: std::sync::atomic::AtomicI64::new(wall_nanos),
        }
    }

    pub(super) fn advance(&self, by: std::time::Duration) {
        *self.elapsed.lock().expect("scripted clock lock") += by;
        self.wall_nanos.fetch_add(
            i64::try_from(by.as_nanos()).expect("scripted interval fits i64"),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
impl Clock for ScriptedClock {
    fn monotonic(&self) -> Instant {
        self.origin + *self.elapsed.lock().expect("scripted clock lock")
    }

    fn wall_nanos(&self) -> Option<i64> {
        Some(self.wall_nanos.load(std::sync::atomic::Ordering::Relaxed))
    }
}
