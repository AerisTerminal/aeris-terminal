//! Monotonic and wall-clock boundary plus the portable standard-library implementation.

use std::time::{Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

/// Monotonic and wall-clock source with explicit units.
pub trait RuntimeClock {
    type Error;

    fn monotonic_nanos(&self) -> u64;

    /// Returns Unix wall time in nanoseconds.
    ///
    /// # Errors
    ///
    /// Returns an error when the system clock is earlier than the Unix epoch.
    fn wall_unix_nanos(&self) -> Result<i128, Self::Error>;
}

/// Process-relative standard-library clock implementation.
#[derive(Clone, Debug)]
pub struct StandardRuntimeClock {
    monotonic_origin: Instant,
}

impl StandardRuntimeClock {
    #[must_use]
    pub fn new() -> Self {
        Self {
            monotonic_origin: Instant::now(),
        }
    }
}

impl Default for StandardRuntimeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeClock for StandardRuntimeClock {
    type Error = SystemTimeError;

    fn monotonic_nanos(&self) -> u64 {
        u64::try_from(self.monotonic_origin.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn wall_unix_nanos(&self) -> Result<i128, Self::Error> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
        Ok(i128::try_from(elapsed.as_nanos()).unwrap_or(i128::MAX))
    }
}
