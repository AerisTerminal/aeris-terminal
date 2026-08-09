use super::{
    CoinbaseDesktopWorker, HISTORY_BARS, LiveLoopState, ProductProfile, cached_history_provenance,
    fence_failed_history, history_provenance, nonzero, publish_cached_update, publish_update,
    unix_nanos,
};
use crate::market_worker::MarketWorkerSender;
use axiusflow_application::{
    MarketBarClientModel, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot,
    ReplayStreamUpdate,
};
use axiusflow_coinbase_market_adapter::{
    COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseHistoryCapabilityAdapter, CoinbaseHistoryTransport,
    CoinbaseHttpsHistoryTransport, CoinbaseInterval, CoinbaseSpotProduct, ENTITLEMENT_CLASS,
    aggregate_coinbase_bars, decode_history_bar, decode_history_segment, encode_history_bar,
    encode_history_segment, history_segment_item_count,
};
use axiusflow_desktop_history::{
    ControlPlaneState, HistoryDecoder, HydrationOutcome, HydrationRequest, ProviderConnectionState,
    StartupCacheState,
};
use axiusflow_desktop_provider_runtime::{HistoryCompletionInstall, SessionGeneration};
use axiusflow_desktop_storage::{
    DataKind, HistoryScope, HistorySeriesIdentity, PublicationRequest, RecoveryAction,
    RetainedRange, RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_provider_history::{
    Completion, DataClass, HistoryItem, HistoryPage, HistoryPageRequest, HistoryRange,
    HistoryScheduler, RequestInterest, RequestPriority, SchedulerConfig,
};
use std::{
    collections::{BTreeMap, VecDeque},
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    sync::{Arc, atomic::AtomicBool},
};

pub(super) struct PreparedHistory {
    pub(super) identity: SegmentIdentity,
    pub(super) completion: Completion,
    pub(super) received_unix_nanos: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FetchPhase {
    Recent,
    Full,
}

pub(super) trait HistorySource: Send {
    fn now_unix_nanos(&self) -> Result<i64, String>;

    fn fetch(
        &mut self,
        profile: &ProductProfile,
        now_unix_nanos: i64,
        cancel: Arc<AtomicBool>,
        phase: FetchPhase,
    ) -> Result<PreparedHistory, String>;
}

pub(super) struct DirectHistorySource;

impl HistorySource for DirectHistorySource {
    fn now_unix_nanos(&self) -> Result<i64, String> {
        unix_nanos()
    }

    fn fetch(
        &mut self,
        profile: &ProductProfile,
        now_unix_nanos: i64,
        cancel: Arc<AtomicBool>,
        phase: FetchPhase,
    ) -> Result<PreparedHistory, String> {
        let transport = CoinbaseHttpsHistoryTransport::with_stop(Arc::clone(&cancel));
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(transport)
            .map_err(|error| error.to_string())?;
        adapter.set_stop(cancel);
        fetch_history_with_adapter(profile, &mut adapter, now_unix_nanos, phase)
    }
}

struct CoinbaseSegmentDecoder;

impl HistoryDecoder<MarketBar> for CoinbaseSegmentDecoder {
    fn retained_decoded_bytes(&mut self, payload: &[u8]) -> Result<usize, String> {
        history_segment_item_count(payload)?
            .checked_mul(size_of::<
                axiusflow_provider_history::SequencedHistory<MarketBar>,
            >())
            .ok_or_else(|| "Coinbase retained history size overflow".to_string())
    }

    fn decode(
        &mut self,
        payload: &[u8],
        maximum_decoded_bytes: usize,
    ) -> Result<
        (
            Vec<axiusflow_provider_history::SequencedHistory<MarketBar>>,
            usize,
        ),
        String,
    > {
        let retained_bytes = self.retained_decoded_bytes(payload)?;
        if retained_bytes > maximum_decoded_bytes {
            return Err("Coinbase retained history exceeds its decoded bound".to_string());
        }
        decode_history_segment(payload).map(|values| (values, retained_bytes))
    }
}

pub(super) struct StreamingSeriesContext<'a> {
    pub(super) profile: &'a ProductProfile,
    pub(super) segment_key: &'a SegmentEncryptionKey,
    pub(super) instrument: &'a InstrumentRevision,
    pub(super) bar_definition: &'a BarDefinition,
    pub(super) worker_label: &'a str,
}

struct CachedHistoryRequest<'a> {
    identities: &'a [SegmentIdentity],
    segment_key: &'a SegmentEncryptionKey,
    provider_state: ProviderConnectionState,
    now_seconds: i64,
    received_unix_nanos: i64,
    instrument: &'a InstrumentRevision,
    bar_definition: &'a BarDefinition,
    worker_label: &'a str,
}

pub(super) struct InitialHistoryContext<'a> {
    pub(super) profile: &'a ProductProfile,
    pub(super) segment_key: &'a SegmentEncryptionKey,
    pub(super) instrument: &'a InstrumentRevision,
    pub(super) bar_definition: &'a BarDefinition,
    pub(super) worker_label: &'a str,
    pub(super) initial_network: Option<axiusflow_platform_runtime::NetworkEvent>,
}

pub(super) fn install_recent_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    context: &StreamingSeriesContext<'_>,
    history: &PreparedHistory,
    state: &mut LiveLoopState,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    if install_covering_snapshot(worker, generation, context, history, model, message_tx).is_err() {
        fence_failed_history(
            worker,
            generation,
            &mut state.retained,
            &mut state.recovery_announced,
            message_tx,
        )?;
    }
    Ok(())
}

pub(super) fn install_fetched_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    context: &StreamingSeriesContext<'_>,
    history: &PreparedHistory,
    state: &mut LiveLoopState,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    let result = install_covering_snapshot(worker, generation, context, history, model, message_tx);
    if result.is_err() {
        fence_failed_history(
            worker,
            generation,
            &mut state.retained,
            &mut state.recovery_announced,
            message_tx,
        )?;
        return Ok(());
    }
    let bars = worker.coinbase_bar_history(&context.profile.product_id)?;
    state.retained = bars
        .into_iter()
        .map(|bar| history_provenance(bar, generation, history.received_unix_nanos))
        .collect::<Result<VecDeque<_>, _>>()?;
    state.streaming_generation = Some(generation);
    state.reconnect_backoff.reset();
    state.recovery_announced = false;
    Ok(())
}

fn install_covering_snapshot<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    context: &StreamingSeriesContext<'_>,
    history: &PreparedHistory,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<(), String> {
    install_history(
        worker,
        generation,
        context.profile,
        history,
        context.segment_key,
        history.received_unix_nanos,
    )?;
    let bars = worker.coinbase_bar_history(&context.profile.product_id)?;
    let retained = bars
        .into_iter()
        .map(|bar| history_provenance(bar, generation, history.received_unix_nanos))
        .collect::<Result<VecDeque<_>, _>>()?;
    let snapshot_generation = model
        .current_generation()
        .map_or(1, |current| current.generation().saturating_add(1));
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        context.instrument.clone(),
        ReplayProvenance::LiveProvider,
        context.bar_definition.clone(),
        snapshot_generation,
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    publish_update(
        worker,
        generation,
        model,
        ReplayStreamUpdate::Snapshot(snapshot),
        context.worker_label,
        message_tx,
    )
}

pub(super) fn prepare_initial_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    history_source: &impl HistorySource,
    context: &InitialHistoryContext<'_>,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<VecDeque<ProvenancedMarketBar>, String> {
    let now = history_source.now_unix_nanos()?;
    let now_seconds = now / 1_000_000_000;
    let scope = HistoryScope {
        provider_id: "coinbase".to_string(),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: ENTITLEMENT_CLASS.to_string(),
    };
    let series = HistorySeriesIdentity {
        scope: &scope,
        instrument_id: &context.profile.instrument_id,
        data_kind: DataKind::Bars,
        resolution: context.profile.interval.label(),
        source_revision: 1,
        schema_revision: 1,
        calendar_revision: 1,
        adjustment_revision: 1,
        correction_revision: 1,
    };
    let requested = history_request_range(context.profile, now, FetchPhase::Full)?;
    let identities = worker
        .retained_history_identities_in_range(
            series,
            RetainedRange {
                start_unix_nanos: requested.start_unix_nanos,
                end_unix_nanos: requested.end_unix_nanos,
            },
            now_seconds,
        )
        .map_err(|error| error.to_string())?;
    let provider_state =
        if context.initial_network == Some(axiusflow_platform_runtime::NetworkEvent::Unavailable) {
            ProviderConnectionState::Offline
        } else {
            ProviderConnectionState::Online
        };
    let retained = if identities.is_empty() {
        if provider_state == ProviderConnectionState::Offline {
            let _ = message_tx.send(crate::market_worker::MarketWorkerMessage::State {
                state: crate::market_worker::ChartState::Stale,
                message: "Coinbase is offline and no retained history is available".to_string(),
            });
        }
        VecDeque::new()
    } else {
        hydrate_cached_history(
            worker,
            &CachedHistoryRequest {
                identities: &identities,
                segment_key: context.segment_key,
                provider_state,
                now_seconds,
                received_unix_nanos: now,
                instrument: context.instrument,
                bar_definition: context.bar_definition,
                worker_label: context.worker_label,
            },
            model,
            message_tx,
        )?
    };
    let connection = worker
        .request_connection()
        .map_err(|error| error.to_string())?;
    if connection.is_some() && !retained.is_empty() {
        let _ = message_tx.send(crate::market_worker::MarketWorkerMessage::State {
            state: crate::market_worker::ChartState::Recovering,
            message: "Showing authenticated local Coinbase history while connecting for a fresh covering snapshot"
                .to_string(),
        });
    }
    Ok(retained)
}

fn hydrate_cached_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    request: &CachedHistoryRequest<'_>,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<VecDeque<ProvenancedMarketBar>, String> {
    let mut bars = BTreeMap::new();
    let mut cache_generation = 1;
    let mut unavailable = false;
    for identity in request.identities {
        let outcome = worker
            .hydrate_visible(
                HydrationRequest {
                    identity,
                    encryption_key: request.segment_key,
                    now_unix_seconds: request.now_seconds,
                    startup_cache_state: StartupCacheState::Cold,
                    provider_state: request.provider_state,
                    control_plane_state: ControlPlaneState::Unavailable,
                    missing_recovery: RecoveryAction::ProviderRefetch,
                },
                &mut CoinbaseSegmentDecoder,
            )
            .map_err(|error| error.to_string())?;
        let HydrationOutcome::Ready { publication, .. } = outcome else {
            unavailable = true;
            continue;
        };
        cache_generation = cache_generation.max(publication.generation);
        for item in &publication.values {
            bars.insert(item.value.exchange_timestamp_seconds, item.value);
        }
    }
    if bars.is_empty() {
        let state = if request.provider_state == ProviderConnectionState::Offline {
            crate::market_worker::ChartState::Stale
        } else {
            crate::market_worker::ChartState::Recovering
        };
        let _ = message_tx.send(crate::market_worker::MarketWorkerMessage::State {
            state,
            message:
                "Coinbase retained history is unavailable; a covering provider snapshot is required"
                    .to_string(),
        });
        return Ok(VecDeque::new());
    }
    while bars.len() > HISTORY_BARS {
        let _ = bars.pop_first();
    }
    let retained = bars
        .into_values()
        .enumerate()
        .map(|(index, mut bar)| {
            bar.source_sequence = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| "Coinbase retained history sequence overflow".to_string())?;
            cached_history_provenance(bar, cache_generation, request.received_unix_nanos)
        })
        .collect::<Result<VecDeque<_>, _>>()?;
    let snapshot = ReplaySnapshot::try_from_provenanced_values(
        request.instrument.clone(),
        ReplayProvenance::LocalCache,
        request.bar_definition.clone(),
        model
            .current_generation()
            .map_or(1, |current| current.generation().saturating_add(1)),
        retained.iter().cloned().collect(),
    )
    .map_err(|error| error.to_string())?;
    publish_cached_update(
        model,
        ReplayStreamUpdate::Snapshot(snapshot),
        request.worker_label,
        message_tx,
    )?;
    let _ = message_tx.send(crate::market_worker::MarketWorkerMessage::State {
        state: crate::market_worker::ChartState::Stale,
        message: if unavailable {
            "Showing partial authenticated local Coinbase history while repairing gaps"
        } else {
            "Showing authenticated local Coinbase history while awaiting live reconciliation"
        }
        .to_string(),
    });
    Ok(retained)
}

fn install_history<V: axiusflow_platform_runtime::CredentialVault>(
    worker: &mut CoinbaseDesktopWorker<V>,
    generation: SessionGeneration,
    profile: &ProductProfile,
    history: &PreparedHistory,
    segment_key: &SegmentEncryptionKey,
    now_unix_nanos: i64,
) -> Result<(), String> {
    let interest = RequestInterest::new(NonZeroU64::MIN);
    let now_seconds = history_installation_time_for(
        profile,
        history.identity.range_end_unix_nanos,
        now_unix_nanos,
    )?;
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
    worker.reset_aggregation();
    if profile.interval == axiusflow_market_data::ChartInterval::Minute1 {
        worker.seed_coinbase_bar_history(
            generation,
            &profile.product_id,
            &history.identity,
            segment_key,
            now_seconds,
        )?;
    }
    let payload = encode_history_segment(&history.completion.page().items)?;
    worker
        .persist_history_segment(PublicationRequest {
            identity: &history.identity,
            payload: &payload,
            encryption_key: segment_key,
            retention: RetentionPolicy::UntilRevoked,
            recovery: RecoveryAction::ProviderRefetch,
            now_unix_seconds: now_seconds,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
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

const RECENT_PHASE_SOURCE_ITEMS: usize = 350;

pub(super) fn needs_recent_phase(interval: axiusflow_market_data::ChartInterval) -> bool {
    let Ok(interval) = CoinbaseInterval::try_from(interval) else {
        return false;
    };
    coinbase_history_source_items(interval, HISTORY_BARS)
        .is_ok_and(|items| items > RECENT_PHASE_SOURCE_ITEMS)
}

pub(super) fn fetch_history_with_adapter<T: CoinbaseHistoryTransport>(
    profile: &ProductProfile,
    adapter: &mut CoinbaseHistoryCapabilityAdapter<T>,
    now: i64,
    phase: FetchPhase,
) -> Result<PreparedHistory, String> {
    let interval = CoinbaseInterval::try_from(profile.interval).map_err(str::to_string)?;
    let source_seconds = coinbase_history_source_seconds(interval);
    let range = history_request_range(profile, now, phase)?;
    let start = range.start_unix_nanos;
    let end = range.end_unix_nanos;
    register_history_product(adapter, profile);
    let identity = history_identity(profile, start, end);
    let request = HistoryPageRequest {
        provider_id: "coinbase".to_string(),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        instrument_id: profile.instrument_id.clone(),
        data_class: DataClass::Bars,
        resolution: profile.interval.label().to_string(),
        range,
        maximum_items: nonzero(350),
        continuation: None,
    };
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
    let batch = adapter
        .fetch_paginated(&dispatch.request)
        .map_err(|error| format!("Coinbase history provider fetch failed: {error}"))?;
    let page = aggregated_history_page(&dispatch.request, &batch, interval, source_seconds, start)?;
    let received_unix_nanos = now;
    let completion = scheduler
        .complete(dispatch.dispatch_id, page, received_unix_nanos)
        .map_err(|error| error.to_string())?;
    Ok(PreparedHistory {
        identity,
        completion,
        received_unix_nanos,
    })
}

pub(super) fn history_request_range(
    profile: &ProductProfile,
    now: i64,
    phase: FetchPhase,
) -> Result<HistoryRange, String> {
    let interval = CoinbaseInterval::try_from(profile.interval).map_err(str::to_string)?;
    let source_nanos = coinbase_history_source_seconds(interval)
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase history source interval overflow".to_string())?;
    let full_source_items = coinbase_history_source_items(interval, HISTORY_BARS)?;
    let source_items = match phase {
        FetchPhase::Recent => full_source_items.min(RECENT_PHASE_SOURCE_ITEMS),
        FetchPhase::Full => full_source_items,
    };
    let end_unix_nanos = now.div_euclid(source_nanos) * source_nanos;
    let start_unix_nanos = end_unix_nanos
        .checked_sub(
            source_nanos
                .checked_mul(i64::try_from(source_items).map_err(|error| error.to_string())?)
                .ok_or_else(|| "Coinbase history range overflow".to_string())?,
        )
        .ok_or_else(|| "Coinbase history range underflow".to_string())?;
    Ok(HistoryRange {
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn aggregated_history_page(
    request: &HistoryPageRequest,
    batch: &axiusflow_coinbase_market_adapter::CoinbaseHistoryBatch,
    interval: CoinbaseInterval,
    source_seconds: i64,
    start: i64,
) -> Result<HistoryPage, String> {
    if batch.items.is_empty() {
        return Err("Coinbase returned no completed history bars".to_string());
    }
    let source = batch
        .items
        .iter()
        .map(decode_history_bar)
        .collect::<Result<Vec<_>, _>>()?;
    let source = materialize_coinbase_continuity(source, source_seconds)?;
    let (mut bars, _) = aggregate_coinbase_bars(&source, interval)?;
    bars.retain(|bar| {
        bar.exchange_timestamp_seconds
            .checked_mul(1_000_000_000)
            .is_some_and(|nanos| nanos >= start)
    });
    if bars.len() > HISTORY_BARS {
        bars.drain(..bars.len() - HISTORY_BARS);
    }
    for (index, bar) in bars.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Coinbase history sequence overflow".to_string())?;
    }
    Ok(HistoryPage {
        request: request.clone(),
        items: bars
            .into_iter()
            .map(|bar| HistoryItem {
                sequence: bar.source_sequence,
                event_time_unix_nanos: bar.exchange_timestamp_seconds * 1_000_000_000,
                payload: encode_history_bar(bar),
            })
            .collect(),
        next: None,
    })
}

fn register_history_product<T>(
    adapter: &mut CoinbaseHistoryCapabilityAdapter<T>,
    profile: &ProductProfile,
) {
    adapter.register_product(&CoinbaseSpotProduct {
        product_id: profile.product_id.clone(),
        instrument_id: profile.instrument_id.clone(),
        display_symbol: profile.symbol.clone(),
        base_currency: profile.base_currency.clone(),
        quote_currency: profile.quote_currency.clone(),
        price_scale: profile.price_scale,
        quantity_scale: profile.quantity_scale,
    });
}

fn history_installation_time_for(
    profile: &ProductProfile,
    range_end_unix_nanos: i64,
    now_unix_nanos: i64,
) -> Result<i64, String> {
    let interval = CoinbaseInterval::try_from(profile.interval).map_err(str::to_string)?;
    let source_seconds = coinbase_history_source_seconds(interval);
    let source_nanos = source_seconds * 1_000_000_000;
    let current_source = now_unix_nanos.div_euclid(source_nanos) * source_nanos;
    // One-minute history must end at the live boundary because the live
    // aggregator seeds from the newest retained minute. Coarser sources may
    // lag one bucket when a long paginated fetch crosses a boundary.
    let tolerance = if source_seconds == 60 {
        0
    } else {
        source_nanos
    };
    if range_end_unix_nanos > current_source || current_source - range_end_unix_nanos > tolerance {
        return Err("Coinbase history became stale before installation".to_string());
    }
    Ok(now_unix_nanos / 1_000_000_000)
}

fn coinbase_history_source_seconds(interval: CoinbaseInterval) -> i64 {
    match interval {
        CoinbaseInterval::Minute1 | CoinbaseInterval::Minute3 => 60,
        CoinbaseInterval::Minute5 => 300,
        CoinbaseInterval::Minute15 => 900,
        CoinbaseInterval::Minute30 => 1_800,
        CoinbaseInterval::Hour1 => 3_600,
        CoinbaseInterval::Hour2 | CoinbaseInterval::Hour4 | CoinbaseInterval::Hour8 => 7_200,
        CoinbaseInterval::Hour12 => 21_600,
        CoinbaseInterval::Day1
        | CoinbaseInterval::Day3
        | CoinbaseInterval::Week1
        | CoinbaseInterval::Month1 => 86_400,
    }
}

fn coinbase_history_source_items(
    interval: CoinbaseInterval,
    output_items: usize,
) -> Result<usize, String> {
    let source_seconds = coinbase_history_source_seconds(interval);
    let target_seconds = interval.fixed_seconds().unwrap_or(match interval {
        CoinbaseInterval::Week1 => 7 * 86_400,
        CoinbaseInterval::Month1 => 31 * 86_400,
        _ => source_seconds,
    });
    let per_output =
        usize::try_from(target_seconds / source_seconds).map_err(|error| error.to_string())?;
    output_items
        .checked_mul(per_output)
        .ok_or_else(|| "Coinbase history source item count overflow".to_string())
}

fn materialize_coinbase_continuity(
    mut bars: Vec<MarketBar>,
    source_seconds: i64,
) -> Result<Vec<MarketBar>, String> {
    bars.sort_by_key(|bar| bar.exchange_timestamp_seconds);
    let mut output: Vec<MarketBar> = Vec::with_capacity(bars.len());
    for bar in bars {
        if let Some(previous) = output.last().copied() {
            let mut timestamp = previous.exchange_timestamp_seconds + source_seconds;
            while timestamp < bar.exchange_timestamp_seconds {
                output.push(MarketBar {
                    source_sequence: 1,
                    exchange_timestamp_seconds: timestamp,
                    open: previous.close,
                    high: previous.close,
                    low: previous.close,
                    close: previous.close,
                    volume: 0,
                });
                timestamp = timestamp
                    .checked_add(source_seconds)
                    .ok_or_else(|| "Coinbase continuity timestamp overflow".to_string())?;
            }
        }
        output.push(bar);
    }
    Ok(output)
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
        resolution: profile.interval.label().to_string(),
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
    use super::{
        FetchPhase, HistorySource, InitialHistoryContext, PreparedHistory, history_identity,
        history_installation_time, prepare_initial_history,
    };
    use crate::{
        live_market_worker::{
            composition::{
                bar_definition, client_model, instrument, nonzero, open_test_worker,
                product_profile,
            },
            lifecycle::apply_initial_network,
        },
        market_worker::{ChartState, MarketWorkerMessage, market_worker_channel},
    };
    use axiusflow_application::{ReplayProvenance, ReplayStreamUpdate};
    use axiusflow_coinbase_market_adapter::{
        COINBASE_PUBLIC_ACCOUNT_ID, CoinbaseConfig, CoinbaseHistoryCapabilityAdapter,
        CoinbaseHistoryTransport, CoinbaseProviderDriver, ENTITLEMENT_CLASS,
        encode_history_segment,
    };
    use axiusflow_desktop_storage::{
        CatalogKey, HistoryStore, PublicationOutcome, PublicationRequest, RecoveryAction,
        RetentionPolicy, SegmentEncryptionKey,
    };
    use axiusflow_platform_runtime::{CredentialVault, NetworkEvent};
    use axiusflow_provider_history::{
        DataClass, HistoryPageRequest, HistoryRange, ProviderHistoryAdapter,
    };
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, atomic::AtomicBool},
        thread,
    };

    #[derive(Clone)]
    struct MemoryVault;

    impl CredentialVault for MemoryVault {
        type Error = ();

        fn store(&self, _key: &str, _secret: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
            Ok(None)
        }

        fn delete(&self, _key: &str) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    struct FixtureTransport;

    impl CoinbaseHistoryTransport for FixtureTransport {
        fn get(&mut self, _path: &str) -> Result<Vec<u8>, String> {
            Ok(br#"{"candles":[
                {"start":"1700000100","low":"37020.00","high":"37080.00","open":"37020.00","close":"37070.00","volume":"0.75000000"},
                {"start":"1700000040","low":"36950.00","high":"37050.00","open":"37000.00","close":"37020.00","volume":"0.50000000"}
            ]}"#
                .to_vec())
        }
    }

    struct FixtureClock(i64);

    impl HistorySource for FixtureClock {
        fn now_unix_nanos(&self) -> Result<i64, String> {
            Ok(self.0)
        }

        fn fetch(
            &mut self,
            _profile: &super::ProductProfile,
            _now_unix_nanos: i64,
            _cancel: Arc<AtomicBool>,
            _phase: FetchPhase,
        ) -> Result<PreparedHistory, String> {
            unreachable!("offline startup does not fetch provider history")
        }
    }

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn create(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "axiusflow-shipping-cache-{name}-{}-{:?}",
                std::process::id(),
                thread::current().id()
            ));
            fs::create_dir(&path).expect("test root creates");
            Self(path)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn history_installation_rejects_a_crossed_minute_boundary() {
        let requested_end = 120_000_000_000;
        assert_eq!(
            history_installation_time(requested_end, requested_end + 30_000_000_000),
            Ok(150)
        );
        assert!(history_installation_time(requested_end, requested_end + 60_000_000_000).is_err());
    }

    #[test]
    fn coarse_intervals_tolerate_one_source_bucket_of_fetch_lag() {
        let mut profile = product_profile("BTC-USD".to_string()).expect("profile validates");
        profile.interval = axiusflow_market_data::ChartInterval::Minute5;
        let current_bucket = 1_700_000_100_i64.div_euclid(300) * 300;
        let now = (current_bucket + 30) * 1_000_000_000;
        let lagging_end = (current_bucket - 300) * 1_000_000_000;
        assert_eq!(
            super::history_installation_time_for(&profile, lagging_end, now),
            Ok(now / 1_000_000_000)
        );
        let stale_end = (current_bucket - 600) * 1_000_000_000;
        assert!(super::history_installation_time_for(&profile, stale_end, now).is_err());

        profile.interval = axiusflow_market_data::ChartInterval::Minute1;
        let minute_now = 1_700_000_090_i64 * 1_000_000_000;
        let lagging_minute = 1_699_999_980_i64 * 1_000_000_000;
        assert!(
            super::history_installation_time_for(&profile, lagging_minute, minute_now).is_err()
        );
    }

    #[test]
    fn shipping_startup_publishes_authenticated_cache_offline_and_rejects_corruption() {
        for corrupt in [false, true] {
            assert_offline_cache_startup(corrupt);
        }
    }

    fn assert_offline_cache_startup(corrupt: bool) {
        let profile = product_profile("BTC-USD".to_string()).expect("profile validates");
        let root = seed_cached_history(&profile, corrupt);
        let ui_thread = thread::current().id();
        let path = root.0.clone();
        thread::spawn(move || {
            assert_cached_startup_messages(&profile, path, ui_thread, corrupt);
        })
        .join()
        .expect("shipping cache fixture completes");
    }

    fn seed_cached_history(profile: &super::ProductProfile, corrupt: bool) -> TestRoot {
        let root = TestRoot::create(if corrupt { "corrupt" } else { "ready" });
        let end = 1_700_000_160_000_000_000;
        let identity = history_identity(profile, 1_700_000_040_000_000_000, end);
        let request = HistoryPageRequest {
            provider_id: "coinbase".to_string(),
            account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
            entitlement_revision: ENTITLEMENT_CLASS.to_string(),
            instrument_id: profile.instrument_id.clone(),
            data_class: DataClass::Bars,
            resolution: "1m".to_string(),
            range: HistoryRange {
                start_unix_nanos: identity.range_start_unix_nanos,
                end_unix_nanos: identity.range_end_unix_nanos,
            },
            maximum_items: nonzero(2),
            continuation: None,
        };
        let mut adapter = CoinbaseHistoryCapabilityAdapter::try_with_transport(FixtureTransport)
            .expect("fixture adapter validates");
        let page = adapter.fetch_page(&request).expect("fixture page decodes");
        assert_eq!(page.items.len(), 2);
        let mut older = identity.clone();
        older.range_end_unix_nanos = 1_700_000_100_000_000_000;
        let mut newer = identity;
        newer.range_start_unix_nanos = older.range_end_unix_nanos;
        let mut store = HistoryStore::open(&root.0, catalog_key(), 4).expect("store opens");
        let receipts = [older, newer]
            .iter()
            .zip(page.items.iter())
            .map(|(identity, item)| {
                let payload =
                    encode_history_segment(std::slice::from_ref(item)).expect("segment encodes");
                let outcome = store
                    .publish(PublicationRequest {
                        identity,
                        payload: &payload,
                        encryption_key: &segment_key(),
                        retention: RetentionPolicy::UntilRevoked,
                        recovery: RecoveryAction::ProviderRefetch,
                        now_unix_seconds: 1_700_000_160,
                    })
                    .expect("segment publishes");
                let PublicationOutcome::Published(receipt) = outcome else {
                    panic!("retained fixture became memory-only");
                };
                receipt
            })
            .collect::<Vec<_>>();
        drop(store);
        if corrupt {
            for receipt in receipts {
                corrupt_file(&root.0.join("segments").join(receipt.file_name));
            }
        }
        root
    }

    fn assert_cached_startup_messages(
        profile: &super::ProductProfile,
        path: PathBuf,
        ui_thread: thread::ThreadId,
        corrupt: bool,
    ) {
        let (driver, _events) = CoinbaseProviderDriver::new(
            CoinbaseConfig::try_new(vec![profile.product_id.clone()])
                .expect("provider config validates"),
            nonzero(8),
        );
        let mut worker =
            open_test_worker(profile, path, ui_thread, MemoryVault, driver, catalog_key())
                .expect("shipping worker opens");
        apply_initial_network(&mut worker, Some(NetworkEvent::Unavailable))
            .expect("offline state applies");
        let (sender, receiver) = market_worker_channel(nonzero(8));
        let instrument = instrument(profile).expect("instrument validates");
        let definition = bar_definition();
        let mut model = client_model();
        let history_source = FixtureClock(1_700_000_190_000_000_000);
        let retained = prepare_initial_history(
            &mut worker,
            &history_source,
            &InitialHistoryContext {
                profile,
                segment_key: &segment_key(),
                instrument: &instrument,
                bar_definition: &definition,
                worker_label: "Coinbase deterministic shipping cache",
                initial_network: Some(NetworkEvent::Unavailable),
            },
            &mut model,
            &sender,
        )
        .expect("offline cache preparation is redacted and bounded");
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        if corrupt {
            assert!(retained.is_empty());
            assert!(
                messages
                    .iter()
                    .all(|message| !matches!(message, MarketWorkerMessage::Update(_)))
            );
        } else {
            assert_eq!(retained.len(), 2);
            assert_eq!(
                retained
                    .iter()
                    .map(|item| item.value().source_sequence)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
            let cached_snapshot = messages.iter().find_map(|message| match message {
                MarketWorkerMessage::Update(publication) => match &publication.update {
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LocalCache =>
                    {
                        Some(snapshot)
                    }
                    _ => None,
                },
                _ => None,
            });
            let cached_snapshot = cached_snapshot.expect("local cache snapshot publishes");
            assert_eq!(cached_snapshot.evidence().ownership_epoch, 1);
            assert_eq!(cached_snapshot.evidence().first_sequence, 1);
            assert_eq!(cached_snapshot.evidence().last_sequence, 2);
        }
        assert!(messages.iter().any(|message| matches!(
            message,
            MarketWorkerMessage::State {
                state: ChartState::Stale,
                ..
            }
        )));
        worker.stop().expect("offline worker stops");
    }

    fn catalog_key() -> CatalogKey {
        CatalogKey::try_new("shipping-cache-catalog".to_string(), [7; 32])
            .expect("catalog key validates")
    }

    fn segment_key() -> SegmentEncryptionKey {
        SegmentEncryptionKey::try_new("shipping-cache-segment".to_string(), [9; 32])
            .expect("segment key validates")
    }

    fn corrupt_file(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path)
                .expect("segment metadata reads")
                .permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(path, permissions).expect("segment becomes writable");
        }
        fs::write(path, b"corrupt").expect("segment corruption writes");
    }
}
