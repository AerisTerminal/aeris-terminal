//! Typed, bounded study alert events emitted during calculation and owned by the host.
//!
//! Study calculations emit pure semantic alert records; they never trigger OS notifications,
//! sound, webhooks, or UI side effects directly. Alerts are committed atomically with study
//! outputs and deduplicated by `(condition_id, bar_timestamp_unix_nanos)` according to their
//! declared [`StudyAlertFrequency`].

use super::MAXIMUM_STUDY_IDENTIFIER_BYTES;
use std::collections::BTreeSet;

/// Maximum alert events emitted by one study in a single calculation wave.
pub const MAXIMUM_STUDY_ALERTS_PER_WAVE: usize = 16;
/// Maximum UTF-8 bytes in one study alert message payload.
pub const MAXIMUM_STUDY_ALERT_MESSAGE_BYTES: usize = 256;
/// Maximum deduplication keys retained per study for `OncePerBar` / `OncePerBarClose` alerts.
const MAXIMUM_DEDUP_KEYS_PER_STUDY: usize = 256;

/// Firing cadence for a study-emitted alert condition.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum StudyAlertFrequency {
    /// Fires at most once per `(condition_id, bar_timestamp_unix_nanos)`.
    #[default]
    OncePerBar,
    /// Fires at most once per bar, and only when the bar is confirmed (not forming).
    OncePerBarClose,
    /// Fires on every committed execution wave that emits the alert.
    EveryTick,
}

/// One typed semantic alert event emitted by a native study calculation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyAlertEvent {
    /// Stable study-local condition identifier (e.g. `"bullish_cross"`).
    pub condition_id: String,
    /// Human-readable bounded message payload.
    pub message: String,
    /// Bar exchange timestamp in Unix nanoseconds when the condition fired.
    pub bar_timestamp_unix_nanos: i64,
    /// Firing cadence requested by the study.
    pub frequency: StudyAlertFrequency,
}

impl StudyAlertEvent {
    /// Validates identifier and message bounds.
    ///
    /// # Errors
    /// Returns an error if `condition_id` is empty/untrimmed/too long or `message` exceeds bounds.
    pub fn validate(&self) -> Result<(), String> {
        let id = self.condition_id.trim();
        if id.is_empty()
            || id != self.condition_id
            || self.condition_id.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
        {
            return Err(
                "study alert condition_id must be a non-empty bounded identifier".to_string(),
            );
        }
        if self.message.len() > MAXIMUM_STUDY_ALERT_MESSAGE_BYTES {
            return Err(format!(
                "study alert message exceeds {MAXIMUM_STUDY_ALERT_MESSAGE_BYTES} bytes"
            ));
        }
        Ok(())
    }
}

/// Runtime-owned deduplication tracker for one study's committed alert emissions.
#[derive(Clone, Debug, Default)]
pub(super) struct StudyAlertLedger {
    fired_keys: BTreeSet<(i64, String)>,
}

impl StudyAlertLedger {
    /// Filters and records candidate alerts from one committed wave.
    pub(super) fn commit_wave(
        &mut self,
        candidates: Vec<StudyAlertEvent>,
        is_confirmed_bar: bool,
    ) -> Vec<StudyAlertEvent> {
        let mut accepted = Vec::new();
        for event in candidates {
            match event.frequency {
                StudyAlertFrequency::EveryTick => accepted.push(event),
                StudyAlertFrequency::OncePerBar => {
                    let key = (event.bar_timestamp_unix_nanos, event.condition_id.clone());
                    if self.record_key(key) {
                        accepted.push(event);
                    }
                }
                StudyAlertFrequency::OncePerBarClose => {
                    if !is_confirmed_bar {
                        continue;
                    }
                    let key = (event.bar_timestamp_unix_nanos, event.condition_id.clone());
                    if self.record_key(key) {
                        accepted.push(event);
                    }
                }
            }
        }
        accepted
    }

    fn record_key(&mut self, key: (i64, String)) -> bool {
        if !self.fired_keys.insert(key) {
            return false;
        }
        while self.fired_keys.len() > MAXIMUM_DEDUP_KEYS_PER_STUDY {
            let _ = self.fired_keys.pop_first();
        }
        true
    }
}
