use crate::{DataClass, HistoryCapabilities, ProviderHistoryError, model::rate_limit};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Default)]
struct RateWindow {
    start_nanos: u64,
    dispatched: u32,
}

#[derive(Default)]
pub(crate) struct RateGates {
    windows: BTreeMap<DataClass, RateWindow>,
}

impl RateGates {
    pub fn allowed(
        &self,
        capabilities: &HistoryCapabilities,
        data_class: DataClass,
        class_inflight: usize,
        now_monotonic_nanos: u64,
    ) -> Result<bool, ProviderHistoryError> {
        let limit = rate_limit(capabilities, data_class)?;
        if class_inflight >= limit.maximum_inflight.get() {
            return Ok(false);
        }
        let window = self
            .windows
            .get(&data_class)
            .copied()
            .unwrap_or(RateWindow {
                start_nanos: now_monotonic_nanos,
                dispatched: 0,
            });
        let elapsed = now_monotonic_nanos.saturating_sub(window.start_nanos);
        Ok(elapsed >= limit.window_nanos.get() || window.dispatched < limit.requests.get())
    }

    pub fn consume(
        &mut self,
        capabilities: &HistoryCapabilities,
        data_class: DataClass,
        now_monotonic_nanos: u64,
    ) -> Result<(), ProviderHistoryError> {
        let limit = rate_limit(capabilities, data_class)?;
        let duration = limit.window_nanos.get();
        let window = self.windows.entry(data_class).or_insert(RateWindow {
            start_nanos: now_monotonic_nanos,
            dispatched: 0,
        });
        if now_monotonic_nanos.saturating_sub(window.start_nanos) >= duration {
            *window = RateWindow {
                start_nanos: now_monotonic_nanos,
                dispatched: 0,
            };
        }
        window.dispatched = window.dispatched.saturating_add(1);
        Ok(())
    }
}
