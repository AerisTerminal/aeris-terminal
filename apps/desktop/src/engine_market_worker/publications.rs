//! Publications.

use super::{
    ChartInterval, ChartState, EngineFaultCode, FeedConnectionState, InstallProviderInstrument,
    MarketDemandError, MarketOrderBookSnapshot, MarketPublicationGeneration, MarketRuntimeEvent,
    MarketSeriesSnapshot, MarketSeriesState, MarketSeriesUpdate, MarketService,
    MarketWorkerBootstrap, MarketWorkerMessage, MarketWorkerPublication, MarketWorkerSender,
    ProviderConnectionState, ProviderState, PushedEventContext, ReplayRecoveryCommand,
    ReplayStreamUpdate, SeriesLoadState, WorkerEndpoint, chart_streams, demand_error,
    provider_display_name, replay_runtime_snapshot, replay_runtime_tail_update,
    runtime_generation_from_snapshot, runtime_order_book_frame, series_key, worker_identity,
};
use aeris_observability::diagnostic;

/// Applies one series-readiness transition, reporting a live handoff to the UI.
/// Turns the engine's load state into the state the chart presents.
///
/// The engine is explicit that a series serving retained canonical history is
/// `Partial`, not ready. Dropping that on the floor is what showed a stale chart
/// as current for the seconds before provider coverage landed, and then jumped.
/// Once the series has gone live the same `Partial` means something else — a
/// backfill repairing history behind a chart that is streaming — so it stops
/// being a loading state at that point.
pub(super) fn apply_series_state(
    state: MarketSeriesState,
    provider: &str,
    realtime: bool,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let provider_name = provider_display_name(provider);
    let load_state = state.state;
    let announce = |chart_state: ChartState, message: String| {
        messages
            .send(MarketWorkerMessage::State {
                state: chart_state,
                message,
            })
            .map_err(|error| error.to_string())
    };
    match load_state {
        SeriesLoadState::Live => {
            if !realtime {
                return Err(format!(
                    "engine marked a {provider_name} calendar-history series live"
                ));
            }
            *live = true;
            announce(
                ChartState::Ready,
                format!("{provider_name} history/live handoff is current"),
            )?;
            Ok(())
        }
        SeriesLoadState::Failed => Err(state
            .detail
            .unwrap_or_else(|| format!("{provider_name} realtime failed"))),
        // Provider history is installed; a realtime series is still loading
        // until its trade handoff promotes it to Live. Revealing it at Ready
        // exposes the history/live seam as a stalled or disconnected chart.
        SeriesLoadState::Ready if realtime => {
            announce(
                ChartState::Loading,
                format!("{provider_name} history is loaded; connecting the live edge"),
            )?;
            Ok(())
        }
        SeriesLoadState::Ready => {
            announce(
                ChartState::Ready,
                format!("{provider_name} provider history is current"),
            )?;
            Ok(())
        }
        SeriesLoadState::Resolving | SeriesLoadState::Partial if !*live => {
            announce(
                ChartState::Loading,
                state.detail.unwrap_or_else(|| {
                    format!("Market runtime is loading current {provider_name} coverage")
                }),
            )?;
            Ok(())
        }
        // The provider answered with no history and the runtime keeps the live
        // subscription up. This is a settled state, not a fault: presenting it
        // as recovering would invalidate the replay stream and re-request the
        // same empty history.
        SeriesLoadState::Empty => {
            *live = false;
            announce(
                ChartState::AwaitingData,
                state.detail.unwrap_or_else(|| {
                    format!("{provider_name} has no market history for this period yet")
                }),
            )?;
            Ok(())
        }
        SeriesLoadState::Resolving | SeriesLoadState::Partial | SeriesLoadState::Superseded => {
            Ok(())
        }
    }
}

fn apply_order_book_snapshot(
    snapshot: &MarketOrderBookSnapshot,
    context: &PushedEventContext<'_>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let &PushedEventContext {
        consumer_id,
        active_generation,
        instrument,
        ..
    } = context;
    if snapshot.consumer_id.0.get() != consumer_id {
        return Err("engine order-book consumer mismatched".to_string());
    }
    let Some(frame) = runtime_order_book_frame(snapshot, instrument, active_generation) else {
        // Depth is an ancillary stream. A stale or malformed book
        // image must never transition the price chart into a fatal
        // state; retain the last valid Order Book frame and wait for the next
        // canonical snapshot.
        return Ok(());
    };
    aeris_desktop::trading::publish_simulated_market_observation(&frame);
    messages
        .send(MarketWorkerMessage::OrderBook(frame))
        .map_err(|error| error.to_string())?;
    Ok(())
}

pub(super) fn apply_pushed_event(
    event: MarketRuntimeEvent,
    context: &PushedEventContext<'_>,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let &PushedEventContext {
        consumer_id,
        active_generation,
        realtime,
        instrument,
    } = context;
    match event {
        MarketRuntimeEvent::SeriesSnapshot(snapshot) => apply_realtime_snapshot(
            &snapshot,
            consumer_id,
            active_generation,
            instrument.price_increment,
            Some(instrument.display_symbol.as_str()),
            messages,
        ),
        MarketRuntimeEvent::SeriesUpdate(update) => {
            apply_realtime_update(&update, consumer_id, active_generation, messages)
        }
        MarketRuntimeEvent::ProviderState(state) => {
            apply_provider_state(&state, instrument.provider.as_str(), realtime, messages)?;
            Ok(())
        }
        MarketRuntimeEvent::SeriesState(state) => {
            apply_realtime_series_state(&state, context, live, messages)
        }
        MarketRuntimeEvent::DemandError(error) => {
            apply_realtime_demand_error(&error, consumer_id, active_generation, messages)
        }
        MarketRuntimeEvent::OrderBookSnapshot(snapshot) => {
            apply_order_book_snapshot(&snapshot, context, messages)
        }
        MarketRuntimeEvent::TradeTapeSnapshot(snapshot) => {
            if snapshot.consumer_id.0.get() != consumer_id {
                return Err("engine trade-tape consumer mismatched".to_string());
            }
            if snapshot.generation.0.get() != active_generation {
                return Ok(());
            }
            messages
                .send(MarketWorkerMessage::TradeTape(snapshot))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::DeltaDivergenceTriggered(trigger) => {
            if trigger.consumer_id.0.get() != consumer_id {
                return Err("engine delta-divergence consumer mismatched".to_string());
            }
            if trigger.generation.0.get() != active_generation {
                return Ok(());
            }
            messages
                .send(MarketWorkerMessage::DeltaDivergenceTriggered(trigger))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::StudyOutputSnapshot(snapshot) => {
            if snapshot.consumer_id.0.get() != consumer_id {
                return Err("engine study-output consumer mismatched".to_string());
            }
            messages
                .send(MarketWorkerMessage::StudyOutput(snapshot))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::StudyOutputsInvalidated(invalidated) => {
            if invalidated.consumer_id.0.get() != consumer_id {
                return Err("engine study-invalidation consumer mismatched".to_string());
            }
            messages
                .send(MarketWorkerMessage::StudyOutputsInvalidated(invalidated))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::StudyRemoved(removed) => {
            if removed.consumer_id.0.get() != consumer_id {
                return Err("engine study-removal consumer mismatched".to_string());
            }
            messages
                .send(MarketWorkerMessage::StudyRemoved(removed))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::PriceAlertTriggered(trigger) => {
            if trigger.consumer_id.0.get() != consumer_id {
                return Err("engine price-alert identity mismatched".to_string());
            }
            dispatch_native_price_alert(&trigger);
            messages
                .send(MarketWorkerMessage::PriceAlertTriggered(trigger))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        MarketRuntimeEvent::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected pushed market event".to_string()),
    }
}

fn dispatch_native_price_alert(trigger: &super::MarketPriceAlertTrigger) {
    let scale = trigger.instrument.price_scale;
    let threshold = crate::desktop::market_price_text(trigger.threshold_price, scale);
    let observed = crate::desktop::market_price_text(trigger.observed_price, scale);
    let body = format!(
        "{} {} {}. Last trade: {}.",
        trigger.instrument.display_symbol,
        price_alert_condition_phrase(trigger.condition),
        threshold,
        observed
    );
    let notification =
        aeris_platform_runtime::NativeUserNotification::try_new("Aeris price alert", body);
    match notification.and_then(aeris_platform_runtime::try_send_user_notification) {
        Ok(()) => {}
        Err(error) => diagnostic!("Aeris price alert notification was not delivered: {error}"),
    }
}

const fn price_alert_condition_phrase(
    condition: aeris_contracts::PriceAlertCondition,
) -> &'static str {
    match condition {
        aeris_contracts::PriceAlertCondition::Crossing => "crossed",
        aeris_contracts::PriceAlertCondition::CrossingUp => "crossed up through",
        aeris_contracts::PriceAlertCondition::CrossingDown => "crossed down through",
        aeris_contracts::PriceAlertCondition::GreaterThan => "moved above",
        aeris_contracts::PriceAlertCondition::LessThan => "moved below",
    }
}

fn apply_realtime_snapshot(
    snapshot: &MarketSeriesSnapshot,
    consumer_id: u64,
    active_generation: u64,
    price_increment: Option<i64>,
    display_symbol: Option<&str>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if snapshot.consumer_id.0.get() != consumer_id {
        return Err("engine realtime snapshot identity mismatched".to_string());
    }
    if stale_generation(
        snapshot.generation.0.get(),
        active_generation,
        "engine realtime snapshot generation advanced unexpectedly",
    )? {
        return Ok(());
    }
    let replay = replay_runtime_snapshot(snapshot, price_increment, display_symbol)?;
    let provider = snapshot.snapshot.series.provider_id.as_str();
    send_publication(messages, ReplayStreamUpdate::Snapshot(replay), provider)
}

fn apply_realtime_update(
    update: &MarketSeriesUpdate,
    consumer_id: u64,
    active_generation: u64,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if update.consumer_id.0.get() != consumer_id {
        return Err("engine realtime update identity mismatched".to_string());
    }
    if stale_generation(
        update.generation.0.get(),
        active_generation,
        "engine realtime update generation advanced unexpectedly",
    )? {
        return Ok(());
    }
    let tail = replay_runtime_tail_update(update)?;
    let provider = update.series.provider_id.as_str();
    send_publication(messages, ReplayStreamUpdate::Tail(tail), provider)
}

fn apply_realtime_series_state(
    state: &MarketSeriesState,
    context: &PushedEventContext<'_>,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let consumer_id = context.consumer_id;
    let active_generation = context.active_generation;
    if state.consumer_id.0.get() != consumer_id {
        return Err("engine realtime state identity mismatched".to_string());
    }
    if stale_generation(
        state.generation.0.get(),
        active_generation,
        "engine realtime state generation advanced unexpectedly",
    )? {
        return Ok(());
    }
    apply_series_state(
        state.clone(),
        context.instrument.provider.as_str(),
        context.realtime,
        live,
        messages,
    )
}

pub(super) fn apply_realtime_demand_error(
    error: &MarketDemandError,
    consumer_id: u64,
    active_generation: u64,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if error.consumer_id.0.get() != consumer_id {
        return Err("engine demand-error consumer mismatched".to_string());
    }
    if stale_generation(
        error.generation.0.get(),
        active_generation,
        "engine demand-error generation advanced unexpectedly",
    )? {
        return Ok(());
    }
    let detail = demand_error(error);
    if error.code == EngineFaultCode::Retryable {
        messages
            .send(MarketWorkerMessage::State {
                state: ChartState::Recovering,
                message: detail,
            })
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    Err(detail)
}

fn stale_generation(received: u64, current: u64, future_error: &str) -> Result<bool, String> {
    if received < current {
        return Ok(true);
    }
    if received > current {
        return Err(future_error.to_string());
    }
    Ok(false)
}

pub(super) fn apply_provider_state(
    state: &ProviderState,
    expected_provider: &str,
    realtime: bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if state.provider != expected_provider {
        return Err("engine provider state identity mismatched".to_string());
    }
    if !realtime {
        return Ok(());
    }
    let provider_name = provider_display_name(expected_provider);
    let provider_state = state.state;
    let (connection, detail) = match provider_state {
        ProviderConnectionState::Disconnected => (
            FeedConnectionState::Disconnected,
            format!("{provider_name} realtime is disconnected"),
        ),
        ProviderConnectionState::Connecting => (
            FeedConnectionState::Discovering,
            format!("{provider_name} realtime is connecting"),
        ),
        ProviderConnectionState::Online => (
            FeedConnectionState::Streaming,
            format!("{provider_name} history and realtime are current"),
        ),
        ProviderConnectionState::Recovering => (
            FeedConnectionState::Recovering,
            format!("{provider_name} realtime is recovering; retained history remains visible"),
        ),
        ProviderConnectionState::Failed => (
            FeedConnectionState::Stopped,
            format!("{provider_name} realtime stopped"),
        ),
    };
    messages
        .send(MarketWorkerMessage::Connection {
            state: connection,
            message: state.detail.clone().unwrap_or(detail),
            transport_rtt_nanos: state.transport_rtt_nanos,
        })
        .map_err(|error| error.to_string())
}

pub(super) fn send_recovery(
    market: &MarketService,
    client_id: u64,
    product: &InstallProviderInstrument,
    interval: ChartInterval,
    endpoint: &mut WorkerEndpoint,
    command: ReplayRecoveryCommand,
) -> Result<(), String> {
    if let Some(previous) = endpoint.pending_recovery.replace(command) {
        endpoint
            .messages
            .send(MarketWorkerMessage::Recovery {
                request_id: previous.request_id,
                result: Err("market recovery was superseded by a newer request".to_string()),
            })
            .map_err(|error| error.to_string())?;
    }
    let series = series_key(product, interval)?;
    if let Err(error) = market.set_demand(
        client_id,
        endpoint.consumer_id,
        endpoint.active_generation,
        &series,
        chart_streams(endpoint.depth_visible),
    ) {
        let pending = endpoint
            .pending_recovery
            .take()
            .expect("recovery was installed before demand was issued");
        endpoint
            .messages
            .send(MarketWorkerMessage::Recovery {
                request_id: pending.request_id,
                result: Err(error),
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(super) fn cancel_pending_recovery(
    endpoint: &mut WorkerEndpoint,
    reason: &str,
) -> Result<(), String> {
    let Some(pending) = endpoint.pending_recovery.take() else {
        return Ok(());
    };
    endpoint
        .messages
        .send(MarketWorkerMessage::Recovery {
            request_id: pending.request_id,
            result: Err(reason.to_string()),
        })
        .map_err(|error| error.to_string())
}

pub(super) fn complete_pending_recovery(
    event: &MarketRuntimeEvent,
    product: &InstallProviderInstrument,
    endpoint: &mut WorkerEndpoint,
) -> Result<bool, String> {
    let Some(command) = endpoint.pending_recovery else {
        return Ok(false);
    };
    match event {
        MarketRuntimeEvent::SeriesSnapshot(snapshot)
            if snapshot.consumer_id.0.get() == endpoint.consumer_id
                && snapshot.generation.0.get() == endpoint.active_generation
                && snapshot.snapshot.provider_generation.0.get() >= product.session_generation =>
        {
            let replay = replay_runtime_snapshot(
                snapshot,
                product.price_increment,
                Some(product.display_symbol.as_str()),
            )?;
            let generation = runtime_generation_from_snapshot(snapshot, &replay)?;
            endpoint.pending_recovery = None;
            let (subscription_id, worker_label) = worker_identity(product.provider.as_str());
            endpoint
                .messages
                .send(MarketWorkerMessage::Recovery {
                    request_id: command.request_id,
                    result: Ok(MarketWorkerBootstrap {
                        snapshot: replay,
                        subscription_id: subscription_id.to_string(),
                        generation,
                        worker_label: worker_label.to_string(),
                    }),
                })
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        MarketRuntimeEvent::DemandError(error)
            if error.consumer_id.0.get() == endpoint.consumer_id
                && error.generation.0.get() == endpoint.active_generation =>
        {
            endpoint.pending_recovery = None;
            endpoint
                .messages
                .send(MarketWorkerMessage::Recovery {
                    request_id: command.request_id,
                    result: Err(demand_error(error)),
                })
                .map_err(|error| error.to_string())?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(super) fn send_publication(
    messages: &MarketWorkerSender,
    update: ReplayStreamUpdate,
    provider: &str,
) -> Result<(), String> {
    let generation = match &update {
        ReplayStreamUpdate::Snapshot(snapshot) => {
            let (first_sequence, last_sequence) = snapshot.sequence_range();
            MarketPublicationGeneration::from_tail(
                snapshot.evidence().publication_generation,
                snapshot.bars().len(),
                first_sequence,
                last_sequence,
            )
        }
        ReplayStreamUpdate::Tail(tail) => {
            let sequence = tail.item().value().source_sequence;
            // The worker no longer shadows the chart model's retained range.
            // Tail publications carry only their own canonical evidence.
            MarketPublicationGeneration::from_tail(
                tail.publication_generation(),
                0,
                sequence,
                sequence,
            )
        }
        ReplayStreamUpdate::Delta(_) => {
            return Err(
                "runtime market worker produced an unexpected delta publication".to_string(),
            );
        }
    };
    let (subscription_id, worker_label) = worker_identity(provider);
    messages
        .send(MarketWorkerMessage::Update(MarketWorkerPublication {
            update,
            generation,
            subscription_id: subscription_id.to_string(),
            worker_label: worker_label.to_string(),
            ui_diagnostics: None,
        }))
        .map_err(|error| error.to_string())
}
