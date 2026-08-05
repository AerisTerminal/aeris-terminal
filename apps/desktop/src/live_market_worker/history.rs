use super::{
    CoinbaseDesktopWorker, HISTORY_BARS, LiveLoopState, ProductProfile, fence_failed_history,
    history_provenance, nonzero, publish_update, unix_nanos,
};
use crate::market_worker::MarketWorkerSender;
use axiusflow_application::{
    MarketBarClientModel, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot,
    ReplayStreamUpdate,
};
use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, ENTITLEMENT_CLASS,
    decode_history_bar,
};
use axiusflow_desktop_history::StartupCacheState;
use axiusflow_desktop_provider_runtime::{
    DesktopProviderState, HistoryCompletionInstall, SessionGeneration,
};
use axiusflow_desktop_storage::{DataKind, HistoryScope, SegmentEncryptionKey, SegmentIdentity};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_provider_history::{
    Completion, DataClass, HistoryPageRequest, HistoryRange, HistoryScheduler,
    ProviderHistoryAdapter, RequestInterest, RequestPriority, SchedulerConfig,
};
use std::{
    collections::VecDeque,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
};

pub(super) struct PreparedHistory {
    pub(super) identity: SegmentIdentity,
    completion: Completion,
    pub(super) received_unix_nanos: i64,
}

pub(super) struct StreamingSeriesContext<'a> {
    pub(super) profile: &'a ProductProfile,
    pub(super) segment_key: &'a SegmentEncryptionKey,
    pub(super) instrument: &'a InstrumentRevision,
    pub(super) bar_definition: &'a BarDefinition,
    pub(super) worker_label: &'a str,
}

struct StreamingHistoryRequest<'a> {
    generation: SessionGeneration,
    profile: &'a ProductProfile,
    segment_key: &'a SegmentEncryptionKey,
    instrument: &'a InstrumentRevision,
    bar_definition: &'a BarDefinition,
    worker_label: &'a str,
}

pub(super) fn install_ready_history(
    worker: &mut CoinbaseDesktopWorker,
    context: &StreamingSeriesContext<'_>,
    state: &mut LiveLoopState,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<bool, String> {
    if state.streaming_generation.is_some() {
        return Ok(false);
    }
    let DesktopProviderState::Streaming { generation } =
        worker.provider_state().map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let history = install_streaming_history(
        worker,
        &StreamingHistoryRequest {
            generation,
            profile: context.profile,
            segment_key: context.segment_key,
            instrument: context.instrument,
            bar_definition: context.bar_definition,
            worker_label: context.worker_label,
        },
        &mut state.prepared,
        model,
        message_tx,
    );
    match history {
        Ok(history) => {
            state.retained = history;
            state.streaming_generation = Some(generation);
            state.reconnect_backoff.reset();
            state.recovery_announced = false;
            Ok(false)
        }
        Err(error) => {
            fence_failed_history(
                worker,
                generation,
                &mut state.retained,
                &mut state.recovery_announced,
                message_tx,
                &error,
            )?;
            Ok(true)
        }
    }
}

fn install_streaming_history(
    worker: &mut CoinbaseDesktopWorker,
    request: &StreamingHistoryRequest<'_>,
    prepared: &mut Option<PreparedHistory>,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<VecDeque<ProvenancedMarketBar>, String> {
    let history = match prepared.take() {
        Some(history) => history,
        None => fetch_history(request.profile)?,
    };
    install_history(
        worker,
        request.generation,
        request.profile,
        &history,
        request.segment_key,
    )?;
    let bars = worker.coinbase_bar_history(&request.profile.product_id)?;
    let retained = bars
        .into_iter()
        .map(|bar| history_provenance(bar, request.generation, history.received_unix_nanos))
        .collect::<Result<VecDeque<_>, _>>()?;
    let snapshot_generation = model
        .current_generation()
        .map_or(1, |current| current.generation().saturating_add(1));
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        request.instrument.clone(),
        ReplayProvenance::LiveProvider,
        request.bar_definition.clone(),
        snapshot_generation,
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    publish_update(
        worker,
        request.generation,
        model,
        ReplayStreamUpdate::Snapshot(snapshot),
        request.worker_label,
        message_tx,
    )?;
    Ok(retained)
}

pub(super) fn prepare_initial_history(
    worker: &mut CoinbaseDesktopWorker,
) -> Result<Option<PreparedHistory>, String> {
    worker
        .request_connection()
        .map_err(|error| error.to_string())?;
    Ok(None)
}

fn install_history(
    worker: &mut CoinbaseDesktopWorker,
    generation: SessionGeneration,
    profile: &ProductProfile,
    history: &PreparedHistory,
    segment_key: &SegmentEncryptionKey,
) -> Result<(), String> {
    let interest = RequestInterest::new(NonZeroU64::MIN);
    let now_seconds =
        history_installation_time(history.identity.range_end_unix_nanos, unix_nanos()?)?;
    let binding = worker
        .begin_scheduled_history_handoff(
            generation,
            history.identity.clone(),
            interest,
            segment_key,
            now_seconds,
        )
        .map_err(|error| error.to_string())?;
    worker
        .install_history_completion(
            generation,
            &history.identity,
            &history.completion,
            HistoryCompletionInstall {
                binding,
                snapshot_generation: NonZeroU64::new(generation.get()).unwrap_or(NonZeroU64::MIN),
                empty_cutover_watermark: None,
                startup_cache_state: StartupCacheState::Cold,
            },
            |_| Ok(size_of::<MarketBar>()),
            |item, _| decode_history_bar(item).map(|bar| (bar, size_of::<MarketBar>())),
        )
        .map_err(|error| error.to_string())?;
    worker.seed_coinbase_bar_history(
        generation,
        &profile.product_id,
        &history.identity,
        segment_key,
        now_seconds,
    )?;
    Ok(())
}

pub(super) fn history_installation_time(
    range_end_unix_nanos: i64,
    now_unix_nanos: i64,
) -> Result<i64, String> {
    let current_minute = now_unix_nanos.div_euclid(60_000_000_000) * 60_000_000_000;
    if range_end_unix_nanos != current_minute {
        return Err("Coinbase history became stale before installation".to_string());
    }
    Ok(now_unix_nanos / 1_000_000_000)
}

fn fetch_history(profile: &ProductProfile) -> Result<PreparedHistory, String> {
    let now = unix_nanos()?;
    let minute_nanos = 60_000_000_000_i64;
    let end = now.div_euclid(minute_nanos) * minute_nanos;
    let start = end
        .checked_sub(minute_nanos * i64::try_from(HISTORY_BARS).map_err(|error| error.to_string())?)
        .ok_or_else(|| "Coinbase history range underflow".to_string())?;
    let identity = history_identity(profile, start, end);
    let request = HistoryPageRequest {
        provider_id: "coinbase".to_string(),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        instrument_id: profile.instrument_id.clone(),
        data_class: DataClass::Bars,
        resolution: "1m".to_string(),
        range: HistoryRange {
            start_unix_nanos: start,
            end_unix_nanos: end,
        },
        maximum_items: nonzero(HISTORY_BARS),
        continuation: None,
    };
    let mut adapter =
        CoinbaseHistoryCapabilityAdapter::try_new().map_err(|error| error.to_string())?;
    let mut scheduler = HistoryScheduler::try_new(
        adapter.capabilities().clone(),
        SchedulerConfig {
            maximum_queued_requests: NonZeroUsize::MIN,
            maximum_total_inflight: NonZeroUsize::MIN,
            maximum_interests_per_request: NonZeroUsize::MIN,
            maximum_continuations_per_request: NonZeroUsize::MIN,
            maximum_fetch_attempts: NonZeroUsize::MIN,
            adjacent_prefetch_windows: 0,
        },
    )
    .map_err(|error| error.to_string())?;
    let interest = RequestInterest::new(NonZeroU64::MIN);
    scheduler
        .submit(request, interest, RequestPriority::Visible, now)
        .map_err(|error| error.to_string())?;
    let dispatch = scheduler
        .dispatch_next(now, 0)
        .map_err(|error| error.to_string())?
        .dispatch
        .ok_or_else(|| "Coinbase history request was not dispatchable".to_string())?;
    let page = adapter.fetch_page(&dispatch.request)?;
    if page.items.is_empty() {
        return Err("Coinbase returned no completed history bars".to_string());
    }
    let received_unix_nanos = unix_nanos()?;
    let completion = scheduler
        .complete(dispatch.dispatch_id, page, received_unix_nanos)
        .map_err(|error| error.to_string())?;
    Ok(PreparedHistory {
        identity,
        completion,
        received_unix_nanos,
    })
}

fn history_identity(profile: &ProductProfile, start: i64, end: i64) -> SegmentIdentity {
    SegmentIdentity {
        scope: HistoryScope {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        },
        instrument_id: profile.instrument_id.clone(),
        data_kind: DataKind::Bars,
        resolution: "1m".to_string(),
        range_start_unix_nanos: start,
        range_end_unix_nanos: end,
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::history_installation_time;

    #[test]
    fn history_installation_rejects_a_crossed_minute_boundary() {
        let requested_end = 120_000_000_000;
        assert_eq!(
            history_installation_time(requested_end, requested_end + 30_000_000_000),
            Ok(150)
        );
        assert!(history_installation_time(requested_end, requested_end + 60_000_000_000).is_err());
    }
}
