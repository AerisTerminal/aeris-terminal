/// Operational mode inputs understood by the pure resource policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineResourceMode {
    Interactive,
    Warm,
    Constrained,
    OfflineSuspended,
    MarketsLive,
}

/// Current bounded workload and machine capacity used for one policy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourcePolicyInput {
    pub mode: EngineResourceMode,
    pub available_memory_bytes: u64,
    pub consumer_count: usize,
    pub visible_consumer_count: usize,
    pub provider_series_limit: usize,
    pub hot_set_priority_count: usize,
}

/// Concrete limits applied by the engine coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourcePolicyDecision {
    pub maximum_decoded_bars: usize,
    pub maximum_derived_series: usize,
    pub retain_hidden_depth: bool,
    pub history_prefetch_bars: usize,
    pub warm_retention_seconds: u64,
}

/// Computes one deterministic, bounded resource decision without changing correctness rules.
#[must_use]
pub fn decide(input: ResourcePolicyInput) -> ResourcePolicyDecision {
    const APPROXIMATE_BAR_BYTES: u64 = 96;
    const MINIMUM_ACTIVE_PREFETCH: usize = 100;
    const MAXIMUM_PREFETCH: usize = 350;
    const MAXIMUM_DECODED_BYTES: u64 = 512 * 1024 * 1024;

    if input.mode == EngineResourceMode::OfflineSuspended {
        return ResourcePolicyDecision {
            maximum_decoded_bars: 0,
            maximum_derived_series: 0,
            retain_hidden_depth: false,
            history_prefetch_bars: 0,
            warm_retention_seconds: 0,
        };
    }

    let memory_budget = (input.available_memory_bytes / 16).min(MAXIMUM_DECODED_BYTES);
    let memory_bars = usize::try_from(memory_budget / APPROXIMATE_BAR_BYTES).unwrap_or(usize::MAX);
    let relevant_series = input
        .consumer_count
        .max(input.hot_set_priority_count)
        .max(1)
        .min(input.provider_series_limit.max(1));
    let mode_prefetch = match input.mode {
        EngineResourceMode::Interactive | EngineResourceMode::MarketsLive => MAXIMUM_PREFETCH,
        EngineResourceMode::Warm => 250,
        EngineResourceMode::Constrained => 160,
        EngineResourceMode::OfflineSuspended => 0,
    };
    let per_series_memory = memory_bars / relevant_series;
    let minimum_prefetch = if input.consumer_count > 0 {
        MINIMUM_ACTIVE_PREFETCH
    } else {
        1
    };
    let history_prefetch_bars = mode_prefetch.min(per_series_memory).max(minimum_prefetch);
    let maximum_decoded_bars =
        memory_bars.min(relevant_series.saturating_mul(history_prefetch_bars.saturating_add(1)));
    let maximum_derived_series = match input.mode {
        EngineResourceMode::Interactive => input.hot_set_priority_count.min(16),
        EngineResourceMode::MarketsLive => input.hot_set_priority_count.min(12),
        EngineResourceMode::Warm => input.hot_set_priority_count.min(8),
        EngineResourceMode::Constrained => input.hot_set_priority_count.min(2),
        EngineResourceMode::OfflineSuspended => 0,
    };
    ResourcePolicyDecision {
        maximum_decoded_bars,
        maximum_derived_series,
        retain_hidden_depth: input.mode == EngineResourceMode::MarketsLive,
        history_prefetch_bars,
        warm_retention_seconds: match input.mode {
            EngineResourceMode::Interactive => 15 * 60,
            EngineResourceMode::Warm => 2 * 60 * 60,
            EngineResourceMode::Constrained => 10 * 60,
            EngineResourceMode::MarketsLive => 24 * 60 * 60,
            EngineResourceMode::OfflineSuspended => 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineResourceMode, ResourcePolicyInput, decide};

    #[test]
    fn constrained_and_offline_modes_reduce_work_without_unbounding_active_history() {
        let constrained = decide(ResourcePolicyInput {
            mode: EngineResourceMode::Constrained,
            available_memory_bytes: 8 * 1024 * 1024 * 1024,
            consumer_count: 4,
            visible_consumer_count: 1,
            provider_series_limit: 128,
            hot_set_priority_count: 20,
        });
        assert_eq!(constrained.history_prefetch_bars, 160);
        assert_eq!(constrained.maximum_derived_series, 2);
        assert!(!constrained.retain_hidden_depth);
        assert!(constrained.maximum_decoded_bars >= 4 * 100);

        let offline = decide(ResourcePolicyInput {
            mode: EngineResourceMode::OfflineSuspended,
            available_memory_bytes: u64::MAX,
            consumer_count: 4,
            visible_consumer_count: 4,
            provider_series_limit: 128,
            hot_set_priority_count: 20,
        });
        assert_eq!(offline.maximum_decoded_bars, 0);
        assert_eq!(offline.history_prefetch_bars, 0);
        assert_eq!(offline.warm_retention_seconds, 0);
    }

    #[test]
    fn markets_live_is_bounded_and_explicitly_retains_hidden_depth() {
        let live = decide(ResourcePolicyInput {
            mode: EngineResourceMode::MarketsLive,
            available_memory_bytes: 16 * 1024 * 1024 * 1024,
            consumer_count: 2,
            visible_consumer_count: 1,
            provider_series_limit: 64,
            hot_set_priority_count: 10,
        });
        assert_eq!(live.history_prefetch_bars, 350);
        assert_eq!(live.maximum_derived_series, 10);
        assert!(live.retain_hidden_depth);
        assert_eq!(live.warm_retention_seconds, 24 * 60 * 60);
    }
}
