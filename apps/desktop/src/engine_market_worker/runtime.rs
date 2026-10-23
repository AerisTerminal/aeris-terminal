//! Runtime.

use super::{
    EndpointRecord, EndpointRole, FeedConnectionState, MarketRuntimeEvent, MarketService,
    MarketWorkerCommand, MarketWorkerMessage, WorkerEndpoint, classify_provider_catalog_event,
    complete_pending_recovery, initialize_catalog_endpoint, initialize_endpoint,
    initialize_market_screen_endpoint, mpsc, process_command,
};
use aeris_desktop::market_worker::{
    MarketWorkerSendError, MarketWorkerSender, ProviderCatalogEvent, verified_terms_supersede,
};
use aeris_observability::diagnostic;
use aeris_trading::market::MarketRoute;
use aeris_trading_runtime::clock::Clock;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub(super) fn run_workers(
    market: &MarketService,
    client_id: u64,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
    trading: &aeris_trading_runtime::TradingService,
    clock: &dyn Clock,
) -> Result<(), String> {
    for record in endpoints.iter() {
        match record
            .endpoint
            .messages
            .send(MarketWorkerMessage::Connection {
                generation: record.endpoint.active_generation,
                state: FeedConnectionState::Discovering,
                message: "Connecting to market data".to_string(),
                transport_rtt_nanos: None,
            }) {
            Ok(()) | Err(MarketWorkerSendError::Disconnected) => {
                // The UI may have retired the pane before the worker attaches.
            }
            Err(MarketWorkerSendError::Full) => {
                diagnostic!("Aeris market connection status could not enter the pane mailbox");
            }
        }
    }
    market.attach(client_id)?;
    let owns_ticket_interest = additions.is_some();
    let result = run_attached_workers(market, client_id, endpoints, additions, trading, clock);
    if owns_ticket_interest && let Err(error) = trading.set_ticket_interest(Vec::new()) {
        diagnostic!("Aeris ticket quote interest could not be released: {error}");
    }
    for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
        retire_endpoint(market, client_id, &mut record.endpoint);
    }
    let detach_result = market.detach(client_id);
    result.and(detach_result)
}

fn run_attached_workers(
    market: &MarketService,
    client_id: u64,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
    trading: &aeris_trading_runtime::TradingService,
    clock: &dyn Clock,
) -> Result<(), String> {
    for record in endpoints.iter_mut() {
        initialize_record(market, client_id, record)?;
    }

    // The workspace group owns tickets; standalone catalog clients must not
    // replace its interest set with an empty one.
    let owns_ticket_interest = additions.is_some();
    let mut additions = additions;
    let mut next_session_status_at = Instant::now();
    let mut ticket_routes = Vec::<MarketRoute>::new();
    let mut ticket_interest_dirty = true;
    while additions.is_some() || endpoints.iter().any(|record| record.endpoint.active) {
        if let Some(receiver) = additions.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(mut record) => match initialize_record(market, client_id, &mut record) {
                        Ok(()) => {
                            endpoints.push(record);
                            ticket_interest_dirty = true;
                        }
                        Err(error) => {
                            report_failure(
                                &record.endpoint.messages,
                                record
                                    .role
                                    .demands_series()
                                    .then_some(record.endpoint.active_generation),
                                error,
                            );
                            retire_endpoint(market, client_id, &mut record.endpoint);
                        }
                    },
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        additions = None;
                        break;
                    }
                }
            }
        }
        for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
            let (commands, disconnected) = drain_command_batch(&record.endpoint.commands);
            for command in commands {
                if let Err(error) = process_command(market, client_id, record, command) {
                    report_failure(
                        &record.endpoint.messages,
                        record
                            .role
                            .demands_series()
                            .then_some(record.endpoint.active_generation),
                        error,
                    );
                }
            }
            if disconnected {
                retire_endpoint(market, client_id, &mut record.endpoint);
                ticket_interest_dirty = true;
            }
        }
        endpoints.retain(|record| record.endpoint.active);
        if owns_ticket_interest && ticket_interest_dirty {
            update_ticket_interest(endpoints, &mut ticket_routes, trading);
            ticket_interest_dirty = false;
        }
        let session_tick_due = Instant::now() >= next_session_status_at;
        publish_session_statuses(market, endpoints, session_tick_due, clock);
        if session_tick_due {
            next_session_status_at = Instant::now() + Duration::from_secs(60);
        }
        ticket_interest_dirty |=
            drain_market_events(market, client_id, endpoints, next_session_status_at, clock)?;
        if owns_ticket_interest && ticket_interest_dirty {
            update_ticket_interest(endpoints, &mut ticket_routes, trading);
            ticket_interest_dirty = false;
        }
    }
    Ok(())
}

fn drain_market_events(
    market: &MarketService,
    client_id: u64,
    endpoints: &mut [EndpointRecord],
    next_session_status_at: Instant,
    clock: &dyn Clock,
) -> Result<bool, String> {
    let consumer_budgets = endpoints
        .iter()
        .filter(|record| record.endpoint.active)
        .filter_map(|record| {
            let (queued, capacity) = record.endpoint.messages.occupancy();
            let available = capacity.saturating_sub(queued);
            (available > 0).then_some((record.endpoint.consumer_id, available))
        })
        .collect::<Vec<_>>();
    if consumer_budgets.is_empty() {
        let _ready = market.wait_for_events(
            client_id,
            next_session_status_at.saturating_duration_since(Instant::now()),
        );
        return Ok(false);
    }
    let events = market
        .drain_events(client_id, &consumer_budgets)
        .inspect_err(|error| {
            report_runtime_failure(endpoints, error);
        })?;
    let empty = events.is_empty();
    let mut changed = false;
    for event in events {
        let consumer_id = event.0;
        match apply_received_event(market, client_id, event, endpoints, clock) {
            Ok(selection_changed) => changed |= selection_changed,
            Err(error) if consumer_id == 0 => {
                report_runtime_failure(endpoints, &error);
                return Err(error);
            }
            // One pane's rejected event fails only that pane; the other panes
            // share this worker and keep their healthy series.
            Err(error) => report_consumer_failure(endpoints, consumer_id, error),
        }
    }
    if empty {
        let _ready = market.wait_for_events(
            client_id,
            next_session_status_at.saturating_duration_since(Instant::now()),
        );
    }
    Ok(changed)
}

fn update_ticket_interest(
    endpoints: &[EndpointRecord],
    published: &mut Vec<MarketRoute>,
    trading: &aeris_trading_runtime::TradingService,
) {
    let mut routes = BTreeMap::new();
    for record in endpoints
        .iter()
        .filter(|record| record.endpoint.active && record.role == EndpointRole::Market)
    {
        if let Ok(terms) =
            aeris_market_runtime::market_service::instrument_terms_from_install(&record.product)
        {
            routes.insert(terms.route.instrument_id.clone(), terms.route);
        }
    }
    // Only 64 tickets can hold quote leases; orders and positions retain their
    // independent owner demand even when more panes are open.
    if routes.len() > 64 {
        diagnostic!(
            "Aeris ticket quote interest omitted {} instruments above the 64-route limit",
            routes.len() - 64
        );
    }
    let desired = routes.into_values().take(64).collect::<Vec<_>>();
    if *published == desired {
        return;
    }
    if let Err(error) = trading.set_ticket_interest(desired.clone()) {
        diagnostic!("Aeris ticket quote interest could not be updated: {error}");
    } else {
        *published = desired;
    }
}

/// Sends each chart its market session status when its product changed since the last
/// send, and on every `tick_due` so phase boundaries are crossed.
fn publish_session_statuses(
    market: &MarketService,
    endpoints: &mut [EndpointRecord],
    tick_due: bool,
    clock: &dyn Clock,
) {
    let mut now = None;
    for record in endpoints
        .iter_mut()
        .filter(|record| record.endpoint.active && record.role.demands_series())
    {
        if !tick_due
            && !session_status_stale(
                record.session_status_sent.as_deref(),
                &record.product.instrument_id,
            )
        {
            continue;
        }
        let now = *now.get_or_insert_with(|| clock.wall_unix_nanos());
        let status = market.market_session_status(&record.product, now);
        match record
            .endpoint
            .messages
            .send(MarketWorkerMessage::MarketSessionStatus {
                generation: record.endpoint.active_generation,
                status,
            }) {
            Ok(()) => {
                record.session_status_sent = Some(record.product.instrument_id.clone());
            }
            Err(error) => {
                // Keep the last acknowledged instrument so a full mailbox retries
                // on the next worker iteration without waiting for the minute tick.
                diagnostic!("Aeris market session status was not delivered: {error}");
            }
        }
    }
}

fn session_status_stale(sent: Option<&str>, instrument_id: &str) -> bool {
    sent != Some(instrument_id)
}

/// Coalesces only adjacent latest-value commands. A click is a barrier, so a
/// viewport or alert update queued after it never overtakes that click.
fn drain_command_batch(
    commands: &mpsc::Receiver<MarketWorkerCommand>,
) -> (Vec<MarketWorkerCommand>, bool) {
    let mut batch = Vec::with_capacity(super::COMMAND_CAPACITY);
    let mut disconnected = false;
    for _ in 0..super::COMMAND_CAPACITY {
        match commands.try_recv() {
            Ok(command) => {
                if let Some(previous) = batch.last_mut()
                    && coalesces(previous, &command)
                {
                    *previous = command;
                    continue;
                }
                batch.push(command);
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }
    (batch, disconnected)
}

fn coalesces(previous: &MarketWorkerCommand, next: &MarketWorkerCommand) -> bool {
    match (previous, next) {
        (MarketWorkerCommand::ChartViewport(first), MarketWorkerCommand::ChartViewport(second)) => {
            first.selection_generation == second.selection_generation
        }
        (MarketWorkerCommand::DepthVisible(_), MarketWorkerCommand::DepthVisible(_))
        | (MarketWorkerCommand::ResourceClass(_), MarketWorkerCommand::ResourceClass(_))
        | (
            MarketWorkerCommand::ReplacePriceAlerts(_),
            MarketWorkerCommand::ReplacePriceAlerts(_),
        ) => true,
        _ => false,
    }
}

/// A disconnected in-process runtime is a real terminal failure. Provider
/// reconnects are handled inside the runtime and never arrive through this path.
fn report_runtime_failure(endpoints: &[EndpointRecord], error: &str) {
    for record in endpoints.iter().filter(|record| record.endpoint.active) {
        report_failure(
            &record.endpoint.messages,
            record
                .role
                .demands_series()
                .then_some(record.endpoint.active_generation),
            error.to_string(),
        );
    }
}

fn report_consumer_failure(endpoints: &[EndpointRecord], consumer_id: u64, error: String) {
    diagnostic!("Aeris market pane {consumer_id} event was rejected: {error}");
    if let Some(record) = endpoints
        .iter()
        .find(|record| record.endpoint.active && record.endpoint.consumer_id == consumer_id)
    {
        report_failure(
            &record.endpoint.messages,
            record
                .role
                .demands_series()
                .then_some(record.endpoint.active_generation),
            error,
        );
    }
}

pub(super) fn report_failure(
    messages: &MarketWorkerSender,
    generation: Option<u64>,
    message: String,
) {
    let publication = match generation {
        Some(generation) => MarketWorkerMessage::EndpointFailure {
            generation,
            message,
        },
        None => MarketWorkerMessage::CatalogFailure { message },
    };
    match messages.send(publication) {
        Ok(()) | Err(MarketWorkerSendError::Disconnected) => {
            // The receiver has gone away during pane retirement.
        }
        Err(MarketWorkerSendError::Full) => {
            // A control publication should fit the reserved mailbox capacity.
            diagnostic!("Aeris market chart state could not enter the pane mailbox");
        }
    }
}

fn initialize_record(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) -> Result<(), String> {
    match record.role {
        EndpointRole::Market => initialize_endpoint(market, client_id, record),
        EndpointRole::RithmicCatalog => initialize_catalog_endpoint(
            market,
            client_id,
            record.workspace_id,
            &mut record.endpoint,
        ),
        EndpointRole::MarketScreen => initialize_market_screen_endpoint(
            market,
            client_id,
            record.workspace_id,
            &record.endpoint,
        ),
    }
}

/// Applies one event drained directly from the runtime-owned consumer outbox.
fn apply_received_event(
    market: &MarketService,
    client_id: u64,
    (consumer_id, event): (u64, aeris_market_runtime::MarketConsumerEvent),
    endpoints: &mut [EndpointRecord],
    clock: &dyn Clock,
) -> Result<bool, String> {
    let consumer_epoch = event.epoch;
    let event = event.value;
    if consumer_id == 0 {
        let MarketRuntimeEvent::Fault(fault) = event else {
            return Err("engine pushed an unrouted market message".to_string());
        };
        report_runtime_failure(endpoints, &fault.redacted_detail);
        return Ok(false);
    }
    let Some(record) = endpoints
        .iter_mut()
        .find(|record| record.endpoint.active && record.endpoint.consumer_id == consumer_id)
    else {
        return Ok(false);
    };
    if let MarketRuntimeEvent::SelectionOutcome(outcome) = event {
        adopt_selection_outcome(market, client_id, record, outcome)?;
        return Ok(true);
    }
    let generation = record
        .role
        .demands_series()
        .then_some(record.endpoint.active_generation);
    let (catalog, event) = classify_provider_catalog_event(event);
    if let Some(catalog) = catalog {
        return Ok(deliver_provider_catalog_event(record, catalog));
    }
    let Some(event) = event else { return Ok(false) };
    let endpoint = &mut record.endpoint;
    // A screen consumer owns no series, so any other event is not addressed to its presentation.
    if record.role == EndpointRole::MarketScreen {
        return Ok(false);
    }
    if let MarketRuntimeEvent::ProviderState(state) = &event
        && state.provider != record.product.provider
    {
        // Another provider's state belongs to a selection this pane has not
        // adopted yet; anything else is left over from a replaced demand.
        if record.pending_selection.is_some() || record.initial_selection_pending {
            record.pending_provider_state = Some(aeris_market_runtime::MarketConsumerEvent {
                epoch: consumer_epoch,
                value: event,
            });
        }
        return Ok(false);
    }
    if complete_pending_recovery(&event, &record.product, endpoint, clock.wall_unix_nanos())? {
        return Ok(false);
    }
    if is_series_status(&event)
        || is_bar_publication(&event)
        || matches!(event, MarketRuntimeEvent::OrderBookSnapshot(_))
        || matches!(
            event,
            MarketRuntimeEvent::TradeTapeSnapshot(_)
                | MarketRuntimeEvent::StudyOutputSnapshot(_)
                | MarketRuntimeEvent::StudyOutputsInvalidated(_)
                | MarketRuntimeEvent::StudyRemoved(_)
                | MarketRuntimeEvent::PriceAlertTriggered(_)
                | MarketRuntimeEvent::ProviderState(_)
        )
    {
        deliver_fenced_pane_event(
            endpoint,
            &record.product.provider,
            aeris_market_runtime::MarketConsumerEvent {
                epoch: consumer_epoch,
                value: event,
            },
            clock.wall_unix_nanos(),
        )?;
        return Ok(false);
    }
    let failure = match event {
        MarketRuntimeEvent::Fault(fault) => fault.redacted_detail,
        _ => "engine returned an unexpected pushed market event".into(),
    };
    report_failure(&endpoint.messages, generation, failure);
    Ok(false)
}

/// Delivers one provider catalog result to its pane and reports whether the
/// worker's selected product changed.
fn deliver_provider_catalog_event(
    record: &mut EndpointRecord,
    event: ProviderCatalogEvent,
) -> bool {
    let mut changed = false;
    if let ProviderCatalogEvent::SelectionInstalled { instrument, .. } = &event
        && record.role == EndpointRole::Market
        && verified_terms_supersede(&record.product, instrument)
    {
        // Verified Terms supersede the provisional selected descriptor, not
        // a different pane selection. Ticket demand uses the same identity.
        record.product = instrument.clone();
        // The verified terms may change the calendar without changing the
        // instrument ID. Retry through the single session-status publisher
        // after this catalog reply, including when its mailbox was full.
        record.session_status_sent = None;
        changed = true;
    }
    let endpoint = &record.endpoint;
    match endpoint
        .messages
        .send(MarketWorkerMessage::ProviderCatalog(event))
    {
        Ok(()) => changed,
        // A released pane cannot receive this catalog result; its command
        // channel closure retires the runtime consumer.
        Err(MarketWorkerSendError::Disconnected) => false,
        Err(MarketWorkerSendError::Full) => {
            // This result is lost and the pane's next search or selection
            // supersedes it; one pane's backlog must not stop the workspace
            // market worker.
            diagnostic!("Aeris market catalog result could not enter the pane mailbox");
            changed
        }
    }
}

fn deliver_fenced_pane_event(
    endpoint: &WorkerEndpoint,
    expected_provider: &str,
    event: aeris_market_runtime::MarketConsumerEvent,
    received_unix_nanos: i64,
) -> Result<(), String> {
    match &event.value {
        MarketRuntimeEvent::SeriesState(_) | MarketRuntimeEvent::DemandError(_) => {
            deliver_runtime_series_event(endpoint, event)
        }
        MarketRuntimeEvent::SeriesSnapshot(_) | MarketRuntimeEvent::SeriesUpdate(_) => {
            deliver_runtime_series_publication(endpoint, event, received_unix_nanos)
        }
        MarketRuntimeEvent::OrderBookSnapshot(_) => deliver_runtime_order_book(endpoint, event),
        MarketRuntimeEvent::TradeTapeSnapshot(_) => deliver_runtime_trade_tape(endpoint, event),
        MarketRuntimeEvent::StudyOutputSnapshot(_)
        | MarketRuntimeEvent::StudyOutputsInvalidated(_)
        | MarketRuntimeEvent::StudyRemoved(_) => deliver_runtime_study(endpoint, event),
        MarketRuntimeEvent::PriceAlertTriggered(_) => deliver_runtime_price_alert(endpoint, event),
        MarketRuntimeEvent::ProviderState(_) => {
            deliver_runtime_provider_state(endpoint, expected_provider, event)
        }
        _ => Err("unexpected fenced pane event".into()),
    }
}

pub(super) fn deliver_runtime_provider_state(
    endpoint: &WorkerEndpoint,
    expected_provider: &str,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    let MarketRuntimeEvent::ProviderState(state) = &event.value else {
        return Err("unexpected runtime provider state".into());
    };
    if state.provider != expected_provider {
        return Err("engine provider state identity mismatched".into());
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimeProviderState {
            event,
            generation: endpoint.active_generation,
        })
        .map_err(|error| error.to_string())
}

fn deliver_runtime_price_alert(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    deliver_runtime_price_alert_with_notification(
        endpoint,
        event,
        super::publications::dispatch_native_price_alert,
    )
}

fn deliver_runtime_price_alert_with_notification(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
    notify: impl FnOnce(&super::MarketPriceAlertTrigger),
) -> Result<(), String> {
    let MarketRuntimeEvent::PriceAlertTriggered(trigger) = &event.value else {
        return Err("unexpected runtime price-alert event".into());
    };
    if trigger.consumer_id.0.get() != endpoint.consumer_id {
        return Err("engine price-alert identity mismatched".into());
    }
    // A price alert is consumer-scoped, not bound to the pane's current chart
    // selection. Keep its runtime envelope and let the pane match its alert ID.
    notify(trigger);
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimePriceAlert(event))
        .map_err(|error| error.to_string())
}

fn is_series_status(event: &MarketRuntimeEvent) -> bool {
    matches!(
        event,
        MarketRuntimeEvent::SeriesState(_) | MarketRuntimeEvent::DemandError(_)
    )
}

fn is_bar_publication(event: &MarketRuntimeEvent) -> bool {
    matches!(
        event,
        MarketRuntimeEvent::SeriesSnapshot(_) | MarketRuntimeEvent::SeriesUpdate(_)
    )
}

pub(super) fn deliver_runtime_series_publication(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
    received_unix_nanos: i64,
) -> Result<(), String> {
    let (consumer_id, generation) = match &event.value {
        MarketRuntimeEvent::SeriesSnapshot(snapshot) => (snapshot.consumer_id, snapshot.generation),
        MarketRuntimeEvent::SeriesUpdate(update) => (update.consumer_id, update.generation),
        _ => return Err("unexpected runtime bar event".into()),
    };
    if consumer_id.0.get() != endpoint.consumer_id {
        return Err("engine realtime bar consumer mismatched".into());
    }
    if generation.0.get() < endpoint.active_generation {
        return Ok(());
    }
    if generation.0.get() > endpoint.active_generation {
        return Err("engine realtime bar generation advanced unexpectedly".into());
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimeSeriesPublication {
            event,
            received_unix_nanos,
        })
        .map_err(|error| error.to_string())
}

pub(super) fn deliver_runtime_order_book(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    let MarketRuntimeEvent::OrderBookSnapshot(snapshot) = &event.value else {
        return Err("unexpected runtime order-book event".into());
    };
    if snapshot.consumer_id.0.get() != endpoint.consumer_id {
        return Err("engine order-book consumer mismatched".into());
    }
    if snapshot.generation.0.get() < endpoint.active_generation {
        return Ok(());
    }
    if snapshot.generation.0.get() > endpoint.active_generation {
        return Err("engine order-book generation advanced unexpectedly".into());
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimeOrderBook(event))
        .map_err(|error| error.to_string())
}

pub(super) fn deliver_runtime_trade_tape(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    let MarketRuntimeEvent::TradeTapeSnapshot(snapshot) = &event.value else {
        return Err("unexpected runtime trade-tape event".into());
    };
    if snapshot.consumer_id.0.get() != endpoint.consumer_id {
        return Err("engine trade-tape consumer mismatched".into());
    }
    if snapshot.generation.0.get() < endpoint.active_generation {
        return Ok(());
    }
    if snapshot.generation.0.get() > endpoint.active_generation {
        return Err("engine trade-tape generation advanced unexpectedly".into());
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimeTradeTape(event))
        .map_err(|error| error.to_string())
}

pub(super) fn deliver_runtime_study(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    let (consumer_id, generation) = match &event.value {
        MarketRuntimeEvent::StudyOutputSnapshot(snapshot) => {
            (snapshot.consumer_id, snapshot.generation)
        }
        MarketRuntimeEvent::StudyOutputsInvalidated(invalidated) => {
            (invalidated.consumer_id, invalidated.generation)
        }
        MarketRuntimeEvent::StudyRemoved(removed) => (removed.consumer_id, removed.generation),
        _ => return Err("unexpected runtime study event".into()),
    };
    if consumer_id.0.get() != endpoint.consumer_id {
        return Err("engine study consumer mismatched".into());
    }
    if let Some(generation) = generation {
        if generation.0.get() < endpoint.active_generation {
            return Ok(());
        }
        if generation.0.get() > endpoint.active_generation {
            return Err("engine study generation advanced unexpectedly".into());
        }
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimeStudy(event))
        .map_err(|error| error.to_string())
}

pub(super) fn deliver_runtime_series_event(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    match &event.value {
        MarketRuntimeEvent::SeriesState(_) => deliver_runtime_series_state(endpoint, event),
        MarketRuntimeEvent::DemandError(_) => deliver_runtime_demand_error(endpoint, event),
        _ => Err("unexpected runtime series event".into()),
    }
}

fn deliver_runtime_series_state(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    let MarketRuntimeEvent::SeriesState(state) = &event.value else {
        return Err("unexpected runtime readiness event".into());
    };
    let error = if state.consumer_id.0.get() != endpoint.consumer_id {
        Some("engine realtime state identity mismatched")
    } else if state.generation.0.get() > endpoint.active_generation {
        Some("engine realtime state generation advanced unexpectedly")
    } else {
        None
    };
    if let Some(error) = error {
        report_failure(
            &endpoint.messages,
            Some(endpoint.active_generation),
            error.into(),
        );
    } else if state.generation.0.get() == endpoint.active_generation {
        endpoint
            .messages
            .send(MarketWorkerMessage::RuntimeSeriesStatus(event))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn deliver_runtime_demand_error(
    endpoint: &WorkerEndpoint,
    event: aeris_market_runtime::MarketConsumerEvent,
) -> Result<(), String> {
    let MarketRuntimeEvent::DemandError(error) = &event.value else {
        return Err("unexpected runtime demand error".into());
    };
    if error.consumer_id.0.get() != endpoint.consumer_id {
        return Err("engine demand-error consumer mismatched".into());
    }
    if error.generation.0.get() < endpoint.active_generation {
        return Ok(());
    }
    if error.generation.0.get() > endpoint.active_generation {
        return Err("engine demand-error generation advanced unexpectedly".into());
    }
    endpoint
        .messages
        .send(MarketWorkerMessage::RuntimeSeriesStatus(event))
        .map_err(|error| error.to_string())
}

/// Applies a selection reply, then gives an adopted selection the depth visibility the
/// pane requested while it was still resolving.
fn adopt_selection_outcome(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
    outcome: aeris_market_runtime::SelectionOutcome,
) -> Result<(), String> {
    let adopted = matches!(
        &outcome,
        aeris_market_runtime::SelectionOutcome::Selected { generation, .. }
            if selection_matches_request(record, generation.0.get())
    );
    apply_selection_outcome(record, outcome)?;
    if record.pending_selection.is_none()
        && !record.initial_selection_pending
        && let Some(event) = record.pending_provider_state.take()
        && adopted
        && matches!(&event.value, MarketRuntimeEvent::ProviderState(state) if state.provider == record.product.provider)
    {
        deliver_runtime_provider_state(&record.endpoint, &record.product.provider, event)?;
    }
    if adopted
        && let Err(error) =
            super::selection_commands::apply_depth_visibility(market, client_id, record)
    {
        report_failure(
            &record.endpoint.messages,
            Some(record.endpoint.active_generation),
            error,
        );
    }
    Ok(())
}

fn selection_matches_request(record: &EndpointRecord, generation: u64) -> bool {
    match record.pending_selection.as_ref() {
        Some((pending, _, _)) => *pending == generation,
        None => record.initial_selection_pending && record.endpoint.active_generation == generation,
    }
}

pub(super) fn apply_selection_outcome(
    record: &mut EndpointRecord,
    outcome: aeris_market_runtime::SelectionOutcome,
) -> Result<(), String> {
    match outcome {
        aeris_market_runtime::SelectionOutcome::Selected {
            generation,
            descriptor,
            ..
        } => {
            let selected = descriptor.instrument;
            if selection_matches_request(record, generation.0.get()) {
                record.product = selected.clone();
                record.initial_selection_pending = false;
                if let Some((_, _, interval)) = record.pending_selection.take() {
                    record.interval = interval;
                    if let Some((sequence, browser_generation)) =
                        record.pending_catalog_selection.take()
                    {
                        record
                            .endpoint
                            .messages
                            .send(MarketWorkerMessage::CatalogSwitchCommitted {
                                sequence,
                                browser_generation,
                                instrument: selected,
                                interval,
                            })
                            .map_err(|error| error.to_string())?;
                    } else {
                        record
                            .endpoint
                            .messages
                            .send(MarketWorkerMessage::EngineSelectionCommitted {
                                sequence: generation.0.get(),
                            })
                            .map_err(|error| error.to_string())?;
                    }
                } else {
                    record
                        .endpoint
                        .messages
                        .send(MarketWorkerMessage::SelectedInstrument {
                            generation: generation.0.get(),
                            instrument: selected,
                        })
                        .map_err(|error| error.to_string())?;
                }
                record.endpoint.active_generation = generation.0.get();
                record.session_status_sent = None;
            }
        }
        aeris_market_runtime::SelectionOutcome::Failed { generation, error } => {
            if selection_matches_request(record, generation.0.get()) {
                if record.pending_selection.is_none() {
                    record.initial_selection_pending = false;
                    report_failure(
                        &record.endpoint.messages,
                        Some(generation.0.get()),
                        "Market history could not start. Try again.".to_string(),
                    );
                    return Ok(());
                }
                record.pending_selection = None;
                if let Some((sequence, browser_generation)) =
                    record.pending_catalog_selection.take()
                {
                    record
                        .endpoint
                        .messages
                        .send(MarketWorkerMessage::CatalogSwitchFailed {
                            sequence,
                            browser_generation,
                            detail: format!("{error:?}"),
                        })
                        .map_err(|send| send.to_string())?;
                } else {
                    record
                        .endpoint
                        .messages
                        .send(MarketWorkerMessage::EngineSelectionFailed {
                            sequence: generation.0.get(),
                        })
                        .map_err(|send| send.to_string())?;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn retire_endpoint(
    market: &MarketService,
    client_id: u64,
    endpoint: &mut WorkerEndpoint,
) {
    if let Err(error) = market.remove_consumer(client_id, endpoint.consumer_id) {
        diagnostic!("Aeris market consumer could not be removed during retirement: {error}");
    }
    endpoint.active = false;
    match endpoint.shutdown.try_send(()) {
        Ok(()) | Err(mpsc::TrySendError::Disconnected(())) => {
            // The UI either receives the acknowledgement or has released its waiter.
        }
        Err(mpsc::TrySendError::Full(())) => {
            diagnostic!("Aeris market retirement acknowledgement was already pending");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_runtime::{
        MarketConsumerId, MarketGenerationId, MarketSeriesState,
        fence::{ConsumerTag, EpochSequence, Fenced},
    };
    use aeris_trading_runtime::clock::ManualClock;
    use std::num::{NonZeroU64, NonZeroUsize};

    #[test]
    fn alert_publication_keeps_consumer_fence_and_rejects_wrong_identity() {
        let (mut pane, record) = super::super::worker_endpoint(
            1,
            2,
            super::super::default_hyperliquid_product(),
            41,
            aeris_market_data::ChartInterval::Minute1,
            None,
            7,
        );
        let mut epochs = EpochSequence::<ConsumerTag>::default();
        let epoch = epochs.next();
        let alert = |consumer_id| Fenced {
            epoch,
            value: MarketRuntimeEvent::PriceAlertTriggered(
                aeris_market_runtime::MarketPriceAlertTrigger {
                    consumer_id: MarketConsumerId(NonZeroU64::new(consumer_id).expect("consumer")),
                    alert_id: "fixture-alert".into(),
                    instrument: record.product.clone(),
                    threshold_price: 100,
                    observed_price: 101,
                    condition: aeris_contracts::PriceAlertCondition::CrossingUp,
                    frequency: aeris_contracts::PriceAlertFrequency::OnlyOnce,
                    observed_unix_nanos: 1,
                    remains_active: false,
                },
            ),
        };
        let mut notifications = 0;
        assert!(
            deliver_runtime_price_alert_with_notification(&record.endpoint, alert(42), |_| {
                notifications += 1;
            },)
            .is_err()
        );
        assert_eq!(notifications, 0);
        assert!(pane.worker.drain_messages().0.is_empty());

        deliver_runtime_price_alert_with_notification(&record.endpoint, alert(41), |_| {
            notifications += 1;
        })
        .expect("alert reaches its consumer");
        assert_eq!(notifications, 1);
        let (messages, _) = pane.worker.drain_messages();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimePriceAlert(Fenced {
                epoch: delivered_epoch,
                value: MarketRuntimeEvent::PriceAlertTriggered(trigger),
            })] if *delivered_epoch == epoch
                && trigger.consumer_id.0.get() == 41
                && trigger.alert_id == "fixture-alert"
        ));
    }

    #[test]
    fn failed_runtime_series_keeps_its_consumer_fence_in_the_pane_mailbox() {
        let (mut pane, record) = super::super::worker_endpoint(
            1,
            2,
            super::super::default_hyperliquid_product(),
            41,
            aeris_market_data::ChartInterval::Minute1,
            None,
            7,
        );
        let mut epochs = EpochSequence::<ConsumerTag>::default();
        let epoch = epochs.next();
        let failed = |generation| Fenced {
            epoch,
            value: MarketRuntimeEvent::SeriesState(MarketSeriesState {
                consumer_id: MarketConsumerId(NonZeroU64::new(41).expect("consumer")),
                generation: MarketGenerationId(NonZeroU64::new(generation).expect("generation")),
                series: None,
                state: super::super::SeriesLoadState::Failed,
                detail: Some("market unavailable".into()),
                presentation: None,
            }),
        };
        deliver_runtime_series_state(&record.endpoint, failed(6)).expect("retired failure");
        deliver_runtime_series_state(&record.endpoint, failed(7)).expect("current failure");
        let (queued, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            queued.as_slice(),
            [MarketWorkerMessage::RuntimeSeriesStatus(Fenced {
                epoch: received_epoch,
                value: MarketRuntimeEvent::SeriesState(MarketSeriesState {
                    state: super::super::SeriesLoadState::Failed,
                    detail: Some(detail),
                    ..
                }),
            })] if *received_epoch == epoch && detail == "market unavailable"
        ));
    }

    #[test]
    fn demand_error_keeps_its_consumer_fence_and_drops_retired_generations() {
        let (mut pane, record) = super::super::worker_endpoint(
            1,
            2,
            super::super::default_hyperliquid_product(),
            41,
            aeris_market_data::ChartInterval::Minute1,
            None,
            7,
        );
        let mut epochs = EpochSequence::<ConsumerTag>::default();
        let epoch = epochs.next();
        let failure = |generation| Fenced {
            epoch,
            value: MarketRuntimeEvent::DemandError(super::super::MarketDemandError {
                consumer_id: MarketConsumerId(NonZeroU64::new(41).expect("consumer")),
                generation: MarketGenerationId(NonZeroU64::new(generation).expect("generation")),
                scope: super::super::MarketDemandErrorScope::Series,
                code: super::super::EngineFaultCode::Retryable,
                stage: super::super::FailureStage::ProviderHistory,
                detail: "coverage unavailable".into(),
                series: None,
                cause: "fixture".into(),
                elapsed_millis: None,
            }),
        };
        deliver_runtime_demand_error(&record.endpoint, failure(6)).expect("retired");
        deliver_runtime_demand_error(&record.endpoint, failure(7)).expect("current");
        assert!(deliver_runtime_demand_error(&record.endpoint, failure(8)).is_err());
        let (queued, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        assert!(matches!(
            queued.as_slice(),
            [MarketWorkerMessage::RuntimeSeriesStatus(Fenced {
                epoch: received_epoch,
                value: MarketRuntimeEvent::DemandError(_),
            })] if *received_epoch == epoch
        ));
    }

    #[test]
    fn initial_and_replacement_selections_accept_one_exact_reply_each() {
        let product = super::super::default_hyperliquid_product();
        let (_pane, mut record) = super::super::worker_endpoint(
            1,
            2,
            product.clone(),
            41,
            aeris_market_data::ChartInterval::Minute1,
            None,
            7,
        );
        assert!(selection_matches_request(&record, 7));
        record.initial_selection_pending = false;
        assert!(!selection_matches_request(&record, 7));
        record.pending_selection = Some((8, product, aeris_market_data::ChartInterval::Hour1));
        assert!(!selection_matches_request(&record, 7));
        assert!(selection_matches_request(&record, 8));
        record.pending_selection = None;
        assert!(!selection_matches_request(&record, 8));
        assert!(!selection_matches_request(&record, 9));
    }

    #[test]
    fn selection_outcomes_publish_one_initial_and_one_replacement_reply() {
        use aeris_market_runtime::{
            InstrumentDescriptor, MarketGenerationId, ResolvedSeries, SelectionOutcome,
        };
        use std::num::NonZeroU64;

        let product = super::super::default_hyperliquid_product();
        let (mut pane, mut record) = super::super::worker_endpoint(
            1,
            2,
            product.clone(),
            41,
            aeris_market_data::ChartInterval::Minute1,
            None,
            7,
        );
        let presentation =
            super::super::provider_presentation(&product.provider).expect("fixture provider");
        let outcome = |generation, interval| SelectionOutcome::Selected {
            generation: MarketGenerationId(NonZeroU64::new(generation).expect("generation")),
            descriptor: Box::new(
                InstrumentDescriptor::from_install(&product, presentation, true)
                    .expect("fixture descriptor"),
            ),
            series: ResolvedSeries::resolve(&product, interval, presentation)
                .expect("fixture series"),
        };
        apply_selection_outcome(
            &mut record,
            outcome(7, aeris_market_data::ChartInterval::Minute1),
        )
        .expect("initial outcome");
        apply_selection_outcome(
            &mut record,
            outcome(7, aeris_market_data::ChartInterval::Minute1),
        )
        .expect("duplicate initial outcome");
        assert!(matches!(
            pane.worker.drain_messages().0.as_slice(),
            [MarketWorkerMessage::SelectedInstrument { generation: 7, .. }]
        ));

        record.pending_selection =
            Some((8, product.clone(), aeris_market_data::ChartInterval::Hour1));
        apply_selection_outcome(
            &mut record,
            outcome(8, aeris_market_data::ChartInterval::Hour1),
        )
        .expect("replacement outcome");
        apply_selection_outcome(
            &mut record,
            outcome(8, aeris_market_data::ChartInterval::Hour1),
        )
        .expect("duplicate replacement outcome");
        assert!(matches!(
            pane.worker.drain_messages().0.as_slice(),
            [MarketWorkerMessage::EngineSelectionCommitted { sequence: 8 }]
        ));
        assert_eq!(record.interval, aeris_market_data::ChartInterval::Hour1);
    }

    #[test]
    fn runtime_failure_uses_bounded_pane_mailbox_and_tolerates_retirement() {
        let (messages, receiver) = super::super::market_worker_channel(NonZeroUsize::MIN);
        report_failure(&messages, Some(7), "market unavailable".into());
        let (queued, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            queued.as_slice(),
            [MarketWorkerMessage::EndpointFailure {
                generation: 7,
                message,
            }] if message == "market unavailable"
        ));
        for _ in 0..9 {
            report_failure(&messages, Some(7), "market unavailable".into());
        }
        report_failure(&messages, Some(7), "saturated pane".into());
        assert_eq!(messages.occupancy().0, 9, "control reserve remains bounded");
        drop(receiver);
        report_failure(&messages, Some(7), "retired pane".into());
    }

    #[test]
    fn non_series_endpoint_failure_remains_app_scoped() {
        let (messages, receiver) = super::super::market_worker_channel(NonZeroUsize::MIN);
        report_failure(&messages, None, "catalog unavailable".into());
        let (queued, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            queued.as_slice(),
            [MarketWorkerMessage::CatalogFailure { message }]
                if message == "catalog unavailable"
        ));
    }

    #[test]
    fn market_session_publications_follow_each_desktop_clock() {
        let market = MarketService::start().expect("market runtime");
        let product = super::super::default_product("ES");
        let first_time = 1_781_568_000_000_000_000;
        let second_time = first_time + 5 * 24 * 60 * 60 * 1_000_000_000;
        let clock = ManualClock::new(first_time);
        let (mut pane, record) = super::super::worker_endpoint(
            1,
            2,
            product.clone(),
            3,
            super::super::ChartInterval::Minute1,
            None,
            1,
        );
        let mut records = vec![record];
        publish_session_statuses(&market, &mut records, true, &clock);
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        let first = messages.into_iter().next().expect("first session status");
        let MarketWorkerMessage::MarketSessionStatus {
            generation: first_generation,
            status: first,
        } = first
        else {
            panic!("expected first session status");
        };
        assert_eq!(first_generation, 1);
        assert_eq!(first, market.market_session_status(&product, first_time));

        clock.set_wall_unix_nanos(second_time);
        publish_session_statuses(&market, &mut records, true, &clock);
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        let second = messages.into_iter().next().expect("second session status");
        let MarketWorkerMessage::MarketSessionStatus {
            generation: second_generation,
            status: second,
        } = second
        else {
            panic!("expected second session status");
        };
        assert_eq!(second_generation, 1);
        assert_eq!(second, market.market_session_status(&product, second_time));
        assert_ne!(
            first, second,
            "different sessions must follow the injected clock"
        );
    }

    #[test]
    fn market_session_status_retries_after_mailbox_backpressure() {
        let market = MarketService::start().expect("market runtime");
        let clock = ManualClock::new(1_781_568_000_000_000_000);
        let (_pane, mut record) = super::super::worker_endpoint(
            1,
            2,
            super::super::default_product("ES"),
            3,
            super::super::ChartInterval::Minute1,
            None,
            1,
        );
        let (messages, receiver) = super::super::market_worker_channel(NonZeroUsize::MIN);
        record.endpoint.messages = messages;
        let mut records = vec![record];
        for _ in 0..9 {
            records[0]
                .endpoint
                .messages
                .send(MarketWorkerMessage::EndpointFailure {
                    generation: 1,
                    message: String::new(),
                })
                .expect("fill bounded control reserve");
        }
        publish_session_statuses(&market, &mut records, false, &clock);
        assert_eq!(records[0].session_status_sent, None);
        assert_eq!(receiver.drain().0.len(), 9);

        publish_session_statuses(&market, &mut records, false, &clock);
        assert!(matches!(
            receiver.drain().0.as_slice(),
            [MarketWorkerMessage::MarketSessionStatus { generation: 1, .. }]
        ));
        assert!(records[0].session_status_sent.is_some());
        publish_session_statuses(&market, &mut records, false, &clock);
        assert!(
            receiver.drain().0.is_empty(),
            "do not repeat an acknowledged status"
        );
    }

    #[test]
    fn catalog_terms_status_uses_the_retrying_session_route_after_the_catalog_reply() {
        let market = MarketService::start().expect("market runtime");
        let clock = ManualClock::new(1_781_568_000_000_000_000);
        let (_pane, mut record) = super::super::worker_endpoint(
            1,
            2,
            super::super::default_product("ES"),
            3,
            super::super::ChartInterval::Minute1,
            None,
            1,
        );
        let (messages, receiver) = super::super::market_worker_channel(NonZeroUsize::MIN);
        record.endpoint.messages = messages;
        let mut records = vec![record];
        publish_session_statuses(&market, &mut records, false, &clock);
        assert_eq!(receiver.drain().0.len(), 1);
        for _ in 0..8 {
            records[0]
                .endpoint
                .messages
                .send(MarketWorkerMessage::EndpointFailure {
                    generation: 1,
                    message: String::new(),
                })
                .expect("fill bounded control reserve");
        }
        let mut terms = super::super::default_product("ES");
        terms.session_generation = 2;
        assert!(deliver_provider_catalog_event(
            &mut records[0],
            ProviderCatalogEvent::SelectionInstalled {
                command_generation: 2,
                instrument: terms.clone(),
            },
        ));
        assert_eq!(records[0].session_status_sent, None);
        publish_session_statuses(&market, &mut records, false, &clock);
        assert_eq!(
            records[0].session_status_sent, None,
            "backpressure must not acknowledge the new terms"
        );
        let (queued, _) = receiver.drain();
        assert_eq!(queued.len(), 9);
        assert!(matches!(
            queued.last(),
            Some(MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::SelectionInstalled { .. }
            ))
        ));
        publish_session_statuses(&market, &mut records, false, &clock);
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::MarketSessionStatus { generation: 1, status }]
                if status.instrument_id == terms.instrument_id
        ));
        assert_eq!(records[0].session_status_sent, Some(terms.instrument_id));
    }

    #[test]
    fn a_full_or_closed_pane_mailbox_does_not_stop_the_market_worker() {
        let market = MarketService::start().expect("market runtime");
        let clock = ManualClock::new(1_781_568_000_000_000_000);
        let product = super::super::default_product("ES");
        let (_pane, mut record) = super::super::worker_endpoint(
            1,
            2,
            product.clone(),
            3,
            super::super::ChartInterval::Minute1,
            None,
            1,
        );
        let (messages, receiver) = super::super::market_worker_channel(NonZeroUsize::MIN);
        record.endpoint.messages = messages;
        let mut records = vec![record];
        for _ in 0..9 {
            records[0]
                .endpoint
                .messages
                .send(MarketWorkerMessage::EndpointFailure {
                    generation: 1,
                    message: String::new(),
                })
                .expect("fill bounded control reserve");
        }
        let mut epochs = EpochSequence::<ConsumerTag>::default();
        let epoch = epochs.next();
        let selection = || {
            (
                3,
                Fenced {
                    epoch,
                    value: MarketRuntimeEvent::ProviderInstrumentSelection(
                        aeris_market_runtime::MarketProviderInstrumentSelection {
                            consumer_id: MarketConsumerId(NonZeroU64::new(3).expect("consumer")),
                            instrument: product.clone(),
                            command_generation: 1,
                        },
                    ),
                },
            )
        };
        apply_received_event(&market, 1, selection(), &mut records, &clock)
            .expect("a full pane mailbox is not fatal");
        assert_eq!(receiver.drain().0.len(), 9);
        drop(receiver);
        apply_received_event(&market, 1, selection(), &mut records, &clock)
            .expect("a closed pane mailbox is not fatal");
    }

    #[test]
    fn a_pending_selections_provider_state_waits_for_its_adoption() {
        use aeris_market_runtime::{InstrumentDescriptor, ResolvedSeries, SelectionOutcome};

        let market = MarketService::start().expect("market runtime");
        let clock = ManualClock::new(1_781_568_000_000_000_000);
        let (mut pane, mut record) = super::super::worker_endpoint(
            1,
            2,
            super::super::default_hyperliquid_product(),
            41,
            aeris_market_data::ChartInterval::Minute1,
            None,
            7,
        );
        record.initial_selection_pending = false;
        let selected = super::super::default_product("MNQ");
        record.pending_selection = Some((
            8,
            record.product.clone(),
            aeris_market_data::ChartInterval::Minute1,
        ));
        let mut records = vec![record];
        let mut epochs = EpochSequence::<ConsumerTag>::default();
        let epoch = epochs.next();
        let provider_state = (
            41,
            Fenced {
                epoch,
                value: MarketRuntimeEvent::ProviderState(super::super::ProviderState {
                    provider: selected.provider.clone(),
                    state: super::super::ProviderConnectionState::Online,
                    generation: 1,
                    detail: None,
                    transport_rtt_nanos: None,
                }),
            },
        );
        apply_received_event(&market, 1, provider_state, &mut records, &clock)
            .expect("another provider's state does not stop the worker");
        assert!(pane.worker.drain_messages().0.is_empty());

        let presentation =
            super::super::provider_presentation(&selected.provider).expect("fixture provider");
        let outcome = (
            41,
            Fenced {
                epoch,
                value: MarketRuntimeEvent::SelectionOutcome(SelectionOutcome::Selected {
                    generation: MarketGenerationId(NonZeroU64::new(8).expect("generation")),
                    descriptor: Box::new(
                        InstrumentDescriptor::from_install(&selected, presentation, true)
                            .expect("fixture descriptor"),
                    ),
                    series: ResolvedSeries::resolve(
                        &selected,
                        aeris_market_data::ChartInterval::Minute1,
                        presentation,
                    )
                    .expect("fixture series"),
                }),
            },
        );
        apply_received_event(&market, 1, outcome, &mut records, &clock).expect("adopted");
        let (messages, _) = pane.worker.drain_messages();
        assert!(messages.iter().any(|message| matches!(
            message,
            MarketWorkerMessage::EngineSelectionCommitted { sequence: 8 }
        )));
        assert!(messages.iter().any(|message| matches!(
            message,
            MarketWorkerMessage::RuntimeProviderState {
                generation: 8,
                event: Fenced {
                    value: MarketRuntimeEvent::ProviderState(super::super::ProviderState {
                        state: super::super::ProviderConnectionState::Online,
                        ..
                    }),
                    ..
                },
                ..
            }
        )));
        assert!(records[0].pending_provider_state.is_none());
    }

    #[test]
    fn a_rejected_pane_event_fails_only_that_pane() {
        let product = super::super::default_product("ES");
        let (mut first, first_record) = super::super::worker_endpoint(
            1,
            2,
            product.clone(),
            3,
            super::super::ChartInterval::Minute1,
            None,
            1,
        );
        let (mut second, second_record) = super::super::worker_endpoint(
            1,
            4,
            product,
            5,
            super::super::ChartInterval::Minute1,
            None,
            1,
        );
        let records = vec![first_record, second_record];
        report_consumer_failure(&records, 5, "rejected".into());
        assert!(first.worker.drain_messages().0.is_empty());
        assert!(matches!(
            second.worker.drain_messages().0.as_slice(),
            [MarketWorkerMessage::EndpointFailure { generation: 1, message }]
                if message == "rejected"
        ));
    }

    #[test]
    fn ordered_command_batch_coalesces_only_adjoining_latest_values() {
        let (sender, receiver) = mpsc::sync_channel(8);
        sender
            .send(MarketWorkerCommand::DepthVisible(true))
            .expect("queued");
        sender
            .send(MarketWorkerCommand::DepthVisible(false))
            .expect("queued");
        sender
            .send(MarketWorkerCommand::ProviderSelect(
                aeris_contracts::SelectProviderInstrument {
                    symbol: "ETH".into(),
                    ..Default::default()
                },
            ))
            .expect("click queued");
        sender
            .send(MarketWorkerCommand::DepthVisible(true))
            .expect("queued after click");
        let (batch, disconnected) = drain_command_batch(&receiver);
        assert!(!disconnected);
        assert!(matches!(
            batch.as_slice(),
            [
                MarketWorkerCommand::DepthVisible(false),
                MarketWorkerCommand::ProviderSelect(_),
                MarketWorkerCommand::DepthVisible(true)
            ]
        ));
    }

    #[test]
    fn session_status_is_resent_when_the_product_changes() {
        let sent = Some("rithmic:Future:GCZ6:COMEX");
        assert!(session_status_stale(None, "rithmic:Future:GCZ6:COMEX"));
        assert!(!session_status_stale(sent, "rithmic:Future:GCZ6:COMEX"));
        assert!(
            session_status_stale(sent, "rithmic:Future:ESZ6:CME"),
            "a chart that switched instrument needs its own status"
        );
    }
}
