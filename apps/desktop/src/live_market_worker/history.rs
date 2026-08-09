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
    CoinbaseInterval, CoinbaseSpotProduct, ENTITLEMENT_CLASS, aggregate_coinbase_bars,
    decode_history_bar, decode_history_segment, encode_history_bar, encode_history_segment,
    history_segment_item_count,
};
use axiusflow_desktop_history::{
    ControlPlaneState, HistoryDecoder, HydrationOutcome, HydrationRequest, ProviderConnectionState,
    StartupCacheState,
};
use axiusflow_desktop_provider_runtime::{
    DesktopProviderState, HistoryCompletionInstall, SessionGeneration,
};
use axiusflow_desktop_storage::{
    DataKind, HistoryScope, HistorySeriesIdentity, PublicationRequest, RecoveryAction,
    RetentionPolicy, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_instruments::InstrumentRevision;
use axiusflow_market_data::{BarDefinition, MarketBar};
use axiusflow_provider_history::{
    Completion, DataClass, HistoryItem, HistoryPage, HistoryPageRequest, HistoryRange,
    HistoryScheduler, RequestInterest, RequestPriority, SchedulerConfig,
};
use std::{
    collections::VecDeque,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
};

pub(super) struct PreparedHistory {
    pub(super) identity: SegmentIdentity,
    pub(super) completion: Completion,
    pub(super) received_unix_nanos: i64,
}

pub(super) trait HistorySource: Send {
    fn now_unix_nanos(&self) -> Result<i64, String>;

    fn fetch(
        &mut self,
        profile: &ProductProfile,
        now_unix_nanos: i64,
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
    ) -> Result<PreparedHistory, String> {
        let mut adapter =
            CoinbaseHistoryCapabilityAdapter::try_new().map_err(|error| error.to_string())?;
        fetch_history_with_adapter(profile, &mut adapter, now_unix_nanos)
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

struct StreamingHistoryRequest<'a> {
    generation: SessionGeneration,
    profile: &'a ProductProfile,
    segment_key: &'a SegmentEncryptionKey,
    instrument: &'a InstrumentRevision,
    bar_definition: &'a BarDefinition,
    worker_label: &'a str,
}

struct CachedHistoryRequest<'a> {
    identity: &'a SegmentIdentity,
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

pub(super) fn install_ready_history<
    V: axiusflow_platform_runtime::CredentialVault,
    H: HistorySource,
>(
    worker: &mut CoinbaseDesktopWorker<V>,
    history_source: &mut H,
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
        history_source,
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
    if let Ok(history) = history {
        state.retained = history;
        state.streaming_generation = Some(generation);
        state.reconnect_backoff.reset();
        state.recovery_announced = false;
        Ok(false)
    } else {
        fence_failed_history(
            worker,
            generation,
            &mut state.retained,
            &mut state.recovery_announced,
            message_tx,
        )?;
        Ok(true)
    }
}

fn install_streaming_history<V: axiusflow_platform_runtime::CredentialVault, H: HistorySource>(
    worker: &mut CoinbaseDesktopWorker<V>,
    history_source: &mut H,
    request: &StreamingHistoryRequest<'_>,
    prepared: &mut Option<PreparedHistory>,
    model: &mut MarketBarClientModel,
    message_tx: &MarketWorkerSender,
) -> Result<VecDeque<ProvenancedMarketBar>, String> {
    let history = if let Some(history) = prepared.take() {
        history
    } else {
        let now = history_source.now_unix_nanos()?;
        history_source.fetch(request.profile, now)?
    };
    let now = history_source.now_unix_nanos()?;
    install_history(
        worker,
        request.generation,
        request.profile,
        &history,
        request.segment_key,
        now,
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
    let latest = worker
        .latest_history_identity(
            HistorySeriesIdentity {
                scope: &scope,
                instrument_id: &context.profile.instrument_id,
                data_kind: DataKind::Bars,
                resolution: context.profile.interval.label(),
                source_revision: 1,
                schema_revision: 1,
                calendar_revision: 1,
                adjustment_revision: 1,
                correction_revision: 1,
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
    let retained = if let Some(identity) = latest {
        hydrate_cached_history(
            worker,
            &CachedHistoryRequest {
                identity: &identity,
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
    } else {
        if provider_state == ProviderConnectionState::Offline {
            let _ = message_tx.send(crate::market_worker::MarketWorkerMessage::State {
                state: crate::market_worker::ChartState::Stale,
                message: "Coinbase is offline and no retained history is available".to_string(),
            });
        }
        VecDeque::new()
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
    let outcome = worker
        .hydrate_visible(
            HydrationRequest {
                identity: request.identity,
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
    };
    let retained = publication
        .values
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut bar = item.value;
            bar.source_sequence = u64::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| "Coinbase retained history sequence overflow".to_string())?;
            cached_history_provenance(bar, publication.generation, request.received_unix_nanos)
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
        message:
            "Showing authenticated local Coinbase history while awaiting a fresh covering snapshot"
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
    if profile.interval == axiusflow_market_data::ChartInterval::Minute1 {
        worker.seed_coinbase_bar_history(
            generation,
            &profile.product_id,
            &history.identity,
            segment_key,
            now_seconds,
        )?;
    } else {
        worker.reset_aggregation();
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

pub(super) fn fetch_history_with_adapter<T: CoinbaseHistoryTransport>(
    profile: &ProductProfile,
    adapter: &mut CoinbaseHistoryCapabilityAdapter<T>,
    now: i64,
) -> Result<PreparedHistory, String> {
    let interval = CoinbaseInterval::try_from(profile.interval).map_err(str::to_string)?;
    let source_seconds = coinbase_history_source_seconds(interval);
    let source_nanos = source_seconds
        .checked_mul(1_000_000_000)
        .ok_or_else(|| "Coinbase history source interval overflow".to_string())?;
    let source_items = coinbase_history_source_items(interval, HISTORY_BARS)?;
    let end = now.div_euclid(source_nanos) * source_nanos;
    let start = end
        .checked_sub(
            source_nanos
                .checked_mul(i64::try_from(source_items).map_err(|error| error.to_string())?)
                .ok_or_else(|| "Coinbase history range overflow".to_string())?,
        )
        .ok_or_else(|| "Coinbase history range underflow".to_string())?;
    register_history_product(adapter, profile);
    let identity = history_identity(profile, start, end);
    let request = HistoryPageRequest {
        provider_id: "coinbase".to_string(),
        account_id: COINBASE_PUBLIC_ACCOUNT_ID.to_string(),
        entitlement_revision: ENTITLEMENT_CLASS.to_string(),
        instrument_id: profile.instrument_id.clone(),
        data_class: DataClass::Bars,
        resolution: profile.interval.label().to_string(),
        range: HistoryRange {
            start_unix_nanos: start,
            end_unix_nanos: end,
        },
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
    if bars.len() > HISTORY_BARS {
        bars.drain(..bars.len() - HISTORY_BARS);
    }
    for (index, bar) in bars.iter_mut().enumerate() {
        bar.source_sequence = u64::try_from(index)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Coinbase history sequence overflow".to_string())?;
    }
    let page = HistoryPage {
        request: dispatch.request.clone(),
        items: bars
            .into_iter()
            .map(|bar| HistoryItem {
                sequence: bar.source_sequence,
                event_time_unix_nanos: bar.exchange_timestamp_seconds * 1_000_000_000,
                payload: encode_history_bar(bar),
            })
            .collect(),
        next: None,
    };
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
    let source_nanos = coinbase_history_source_seconds(interval) * 1_000_000_000;
    let current_source = now_unix_nanos.div_euclid(source_nanos) * source_nanos;
    if range_end_unix_nanos != current_source {
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
        DirectHistorySource, InitialHistoryContext, history_identity, history_installation_time,
        prepare_initial_history,
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
    use std::{fs, path::PathBuf, thread};

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
        let payload = encode_history_segment(&page.items).expect("segment encodes");
        let mut store = HistoryStore::open(&root.0, catalog_key(), 4).expect("store opens");
        let outcome = store
            .publish(PublicationRequest {
                identity: &identity,
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
        drop(store);
        if corrupt {
            corrupt_file(&root.0.join("segments").join(receipt.file_name));
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
        let history_source = DirectHistorySource;
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
