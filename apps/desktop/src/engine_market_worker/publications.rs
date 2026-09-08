//! Publications.

use super::{
    ChartInterval, ChartState, DemandError, EngineFaultCode, EngineSupervisor, FeedConnectionState,
    InstallProviderInstrument, MarketPublicationGeneration, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerSender, OrderBookIdentity,
    ProviderConnectionState, ProviderState, PushedEventContext, ReplayRecoveryCommand,
    ReplayStreamUpdate, SeriesLoadState, SeriesSnapshot, SeriesState, SeriesUpdate, WorkerEndpoint,
    demand_error, envelope, generation_from_snapshot, order_book_from_snapshot,
    provider_display_name, replay_snapshot, replay_tail_update, series_key, tail_publication,
    worker_identity,
};

/// Applies one series-readiness transition, reporting a live handoff to the UI.
/// Turns the engine's load state into the state the chart presents.
///
/// The engine is explicit that a series serving retained local history is
/// `Partial`, not ready. Dropping that on the floor is what showed a stale chart
/// as current for the seconds before provider coverage landed, and then jumped.
/// Once the series has gone live the same `Partial` means something else — a
/// backfill repairing history behind a chart that is streaming — so it stops
/// being a loading state at that point.
pub(super) fn apply_series_state(
    state: SeriesState,
    provider: &str,
    realtime: bool,
    published: bool,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let provider_name = provider_display_name(provider);
    let load_state = SeriesLoadState::try_from(state.state)
        .map_err(|_| "engine returned an invalid realtime state".to_string())?;
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
            if !published {
                return Err("engine marked history live without a covering snapshot".to_string());
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
        SeriesLoadState::Ready if !published => {
            Err("engine marked history ready without a covering snapshot".to_string())
        }
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
                    format!("Resident engine is loading current {provider_name} coverage")
                }),
            )?;
            Ok(())
        }
        SeriesLoadState::Empty
        | SeriesLoadState::Resolving
        | SeriesLoadState::Partial
        | SeriesLoadState::Superseded => Ok(()),
    }
}

pub(super) fn apply_pushed_event(
    event: envelope::Payload,
    context: &PushedEventContext<'_>,
    publication: &mut Option<MarketPublicationGeneration>,
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
        envelope::Payload::SeriesSnapshot(snapshot) => apply_realtime_snapshot(
            &snapshot,
            consumer_id,
            active_generation,
            instrument.session_generation,
            publication,
            messages,
        ),
        envelope::Payload::SeriesUpdate(update) => apply_realtime_update(
            &update,
            consumer_id,
            active_generation,
            instrument.session_generation,
            publication,
            messages,
        ),
        envelope::Payload::ProviderState(state) => {
            // Provider reconnects advance independently of a chart selection.
            if state.generation < instrument.session_generation {
                return Ok(());
            }
            apply_provider_state(&state, instrument.provider.as_str(), realtime, messages)?;
            Ok(())
        }
        envelope::Payload::SeriesState(state) => {
            apply_realtime_series_state(&state, context, publication.is_some(), live, messages)
        }
        envelope::Payload::DemandError(error) => {
            apply_realtime_demand_error(&error, consumer_id, active_generation, messages)
        }
        envelope::Payload::OrderBookSnapshot(snapshot) => {
            if snapshot.consumer_id != consumer_id {
                return Err("engine order-book consumer mismatched".to_string());
            }
            if snapshot.provider_generation < instrument.session_generation {
                return Ok(());
            }
            let Ok(frame) = order_book_from_snapshot(
                &OrderBookIdentity {
                    instrument,
                    series_generation: active_generation,
                },
                &snapshot,
            ) else {
                // Depth is an ancillary stream. A stale or malformed book
                // image must never transition the price chart into a fatal
                // state; retain the last valid Order Book frame and wait for the next
                // canonical snapshot.
                return Ok(());
            };
            messages
                .send(MarketWorkerMessage::OrderBook(frame))
                .map_err(|error| error.to_string())?;
            Ok(())
        }
        envelope::Payload::OrderFlowSnapshot(_) | envelope::Payload::OrderFlowUpdate(_) => Ok(()),
        envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected pushed market event".to_string()),
    }
}

fn apply_realtime_snapshot(
    snapshot: &SeriesSnapshot,
    consumer_id: u64,
    active_generation: u64,
    minimum_provider_generation: u64,
    publication: &mut Option<MarketPublicationGeneration>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if snapshot.consumer_id != consumer_id {
        return Err("engine realtime snapshot identity mismatched".to_string());
    }
    if stale_generation(
        snapshot.generation,
        active_generation,
        "engine realtime snapshot generation advanced unexpectedly",
    )? || snapshot.provider_generation < minimum_provider_generation
    {
        return Ok(());
    }
    let replay = replay_snapshot(snapshot)?;
    let generation = generation_from_snapshot(snapshot, &replay)?;
    let status = MarketPublicationGeneration::from_generation(&generation);
    *publication = Some(status);
    let provider = snapshot
        .series
        .as_ref()
        .map(|series| series.provider.as_str())
        .ok_or_else(|| "engine snapshot has no series identity".to_string())?;
    send_publication(
        messages,
        ReplayStreamUpdate::Snapshot(replay),
        status,
        provider,
    )
}

fn apply_realtime_update(
    update: &SeriesUpdate,
    consumer_id: u64,
    active_generation: u64,
    minimum_provider_generation: u64,
    publication: &mut Option<MarketPublicationGeneration>,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if update.consumer_id != consumer_id {
        return Err("engine realtime update identity mismatched".to_string());
    }
    if stale_generation(
        update.generation,
        active_generation,
        "engine realtime update generation advanced unexpectedly",
    )? || update.provider_generation < minimum_provider_generation
    {
        return Ok(());
    }
    let tail = replay_tail_update(update)?;
    let status = tail_publication(
        publication
            .ok_or_else(|| "engine sent a Rithmic update before a covering snapshot".to_string())?,
        &tail,
    );
    *publication = Some(status);
    let provider = update
        .series
        .as_ref()
        .map(|series| series.provider.as_str())
        .ok_or_else(|| "engine update has no series identity".to_string())?;
    send_publication(messages, ReplayStreamUpdate::Tail(tail), status, provider)
}

fn apply_realtime_series_state(
    state: &SeriesState,
    context: &PushedEventContext<'_>,
    has_publication: bool,
    live: &mut bool,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    let consumer_id = context.consumer_id;
    let active_generation = context.active_generation;
    if state.consumer_id != consumer_id {
        return Err("engine realtime state identity mismatched".to_string());
    }
    if stale_generation(
        state.generation,
        active_generation,
        "engine realtime state generation advanced unexpectedly",
    )? {
        return Ok(());
    }
    apply_series_state(
        state.clone(),
        context.instrument.provider.as_str(),
        context.realtime,
        has_publication,
        live,
        messages,
    )
}

pub(super) fn apply_realtime_demand_error(
    error: &DemandError,
    consumer_id: u64,
    active_generation: u64,
    messages: &MarketWorkerSender,
) -> Result<(), String> {
    if error.consumer_id != consumer_id {
        return Err("engine demand-error consumer mismatched".to_string());
    }
    if stale_generation(
        error.generation,
        active_generation,
        "engine demand-error generation advanced unexpectedly",
    )? {
        return Ok(());
    }
    let detail = demand_error(error);
    if EngineFaultCode::try_from(error.code) == Ok(EngineFaultCode::Retryable) {
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
    let provider_state = ProviderConnectionState::try_from(state.state)
        .map_err(|_| "engine returned an invalid provider state".to_string())?;
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
    client: &mut EngineSupervisor,
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
    if let Err(error) = client.set_series_demand(
        endpoint.consumer_id,
        endpoint.active_generation,
        series_key(product, interval)?,
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
    event: &envelope::Payload,
    product: &InstallProviderInstrument,
    endpoint: &mut WorkerEndpoint,
) -> Result<bool, String> {
    let Some(command) = endpoint.pending_recovery else {
        return Ok(false);
    };
    match event {
        envelope::Payload::SeriesSnapshot(snapshot)
            if snapshot.consumer_id == endpoint.consumer_id
                && snapshot.generation == endpoint.active_generation
                && snapshot.provider_generation >= product.session_generation =>
        {
            let replay = replay_snapshot(snapshot)?;
            let generation = generation_from_snapshot(snapshot, &replay)?;
            endpoint.publication = Some(MarketPublicationGeneration::from_generation(&generation));
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
        envelope::Payload::DemandError(error)
            if error.consumer_id == endpoint.consumer_id
                && error.generation == endpoint.active_generation =>
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
    generation: MarketPublicationGeneration,
    provider: &str,
) -> Result<(), String> {
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
