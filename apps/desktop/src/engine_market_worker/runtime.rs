//! Runtime.

use super::{
    ChartState, EndpointRecord, EndpointRole, FeedConnectionState, MarketRuntimeEvent,
    MarketService, MarketWorkerCommand, MarketWorkerMessage, PushedEventContext, WorkerEndpoint,
    apply_pushed_event, classify_provider_catalog_event, complete_pending_recovery,
    initialize_catalog_endpoint, initialize_endpoint, initialize_market_screen_endpoint, mpsc,
    process_command, shared_market_runtime,
};
use aeris_desktop::market_worker::ProviderCatalogEvent;
use aeris_observability::diagnostic;
use aeris_trading::market::MarketRoute;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub(super) fn run_workers(
    client_id: u64,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
) -> Result<(), String> {
    for record in endpoints.iter() {
        let _ = record
            .endpoint
            .messages
            .send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Discovering,
                message: "Connecting to market data".to_string(),
                transport_rtt_nanos: None,
            });
    }
    let market = shared_market_runtime()?;
    market.attach(client_id)?;
    let owns_ticket_interest = additions.is_some();
    let result = run_attached_workers(&market, client_id, endpoints, additions);
    if owns_ticket_interest
        && let Some(trading) = aeris_desktop::trading::handle()
        && let Err(error) = trading.set_ticket_interest(Vec::new())
    {
        diagnostic!("Aeris ticket quote interest could not be released: {error}");
    }
    for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
        let _ = market.remove_consumer(client_id, record.endpoint.consumer_id);
        record.endpoint.active = false;
        let _ = record.endpoint.shutdown.try_send(());
    }
    let detach_result = market.detach(client_id);
    result.and(detach_result)
}

fn run_attached_workers(
    market: &MarketService,
    client_id: u64,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
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
                            let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                                state: ChartState::Error,
                                message: error,
                            });
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
                    let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                        state: ChartState::Error,
                        message: error,
                    });
                }
            }
            if disconnected {
                retire_endpoint(market, client_id, &mut record.endpoint);
                ticket_interest_dirty = true;
            }
        }
        endpoints.retain(|record| record.endpoint.active);
        if owns_ticket_interest && ticket_interest_dirty {
            update_ticket_interest(endpoints, &mut ticket_routes);
            ticket_interest_dirty = false;
        }
        let session_tick_due = Instant::now() >= next_session_status_at;
        publish_session_statuses(market, endpoints, session_tick_due);
        if session_tick_due {
            next_session_status_at = Instant::now() + Duration::from_secs(60);
        }
        ticket_interest_dirty |=
            drain_market_events(market, client_id, endpoints, next_session_status_at)?;
        if owns_ticket_interest && ticket_interest_dirty {
            update_ticket_interest(endpoints, &mut ticket_routes);
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
        match apply_received_event(market, client_id, event, endpoints) {
            Ok(selection_changed) => changed |= selection_changed,
            Err(error) => {
                report_runtime_failure(endpoints, &error);
                return Err(error);
            }
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

fn update_ticket_interest(endpoints: &[EndpointRecord], published: &mut Vec<MarketRoute>) {
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
    let Some(trading) = aeris_desktop::trading::handle() else {
        return;
    };
    if let Err(error) = trading.set_ticket_interest(desired.clone()) {
        diagnostic!("Aeris ticket quote interest could not be updated: {error}");
    } else {
        *published = desired;
    }
}

/// Sends each chart its market session status when its product or the runtime calendar
/// changed since the last send, and on every `tick_due` so phase boundaries are crossed.
fn publish_session_statuses(
    market: &MarketService,
    endpoints: &mut [EndpointRecord],
    tick_due: bool,
) {
    let revision = market.market_session_revision();
    let mut now = None;
    for record in endpoints
        .iter_mut()
        .filter(|record| record.endpoint.active && record.role.demands_series())
    {
        if !tick_due
            && !session_status_stale(
                record.session_status_sent.as_ref(),
                &record.product.instrument_id,
                revision,
            )
        {
            continue;
        }
        let now = *now.get_or_insert_with(super::now_unix_nanos);
        let status = market.market_session_status(&record.product, now);
        if let Err(error) = record
            .endpoint
            .messages
            .send(MarketWorkerMessage::MarketSessionStatus(status))
        {
            diagnostic!("Aeris market session status was not delivered: {error}");
        }
        record.session_status_sent = Some((record.product.instrument_id.clone(), revision));
    }
}

fn session_status_stale(sent: Option<&(String, u64)>, instrument_id: &str, revision: u64) -> bool {
    sent.is_none_or(|(sent_instrument, sent_revision)| {
        sent_instrument != instrument_id || *sent_revision != revision
    })
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
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error.to_string(),
        });
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
    _client_id: u64,
    (consumer_id, event): (u64, aeris_market_runtime::MarketConsumerEvent),
    endpoints: &mut [EndpointRecord],
) -> Result<bool, String> {
    let event = event.value;
    if consumer_id == 0 {
        let MarketRuntimeEvent::Fault(fault) = event else {
            return Err("engine pushed an unrouted market message".to_string());
        };
        for endpoint in endpoints
            .iter()
            .filter(|record| record.endpoint.active)
            .map(|record| &record.endpoint)
        {
            let _ = endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: fault.redacted_detail.clone(),
            });
        }
        return Ok(false);
    }
    let Some(record) = endpoints
        .iter_mut()
        .find(|record| record.endpoint.active && record.endpoint.consumer_id == consumer_id)
    else {
        return Ok(false);
    };
    if let MarketRuntimeEvent::SelectionOutcome(outcome) = event {
        apply_selection_outcome(record, outcome)?;
        return Ok(true);
    }
    let endpoint = &mut record.endpoint;
    let (catalog, event) = classify_provider_catalog_event(event);
    let event = match catalog {
        Some(event) => {
            let mut changed = false;
            if let ProviderCatalogEvent::SelectionInstalled { instrument, .. } = &event
                && record.role == EndpointRole::Market
                && record.product.provider == instrument.provider
                && record.product.instrument_id == instrument.instrument_id
                && instrument.selection_generation >= record.product.selection_generation
                && instrument.session_generation >= record.product.session_generation
            {
                // Verified Terms supersede the provisional selected descriptor, not
                // a different pane selection. Ticket demand uses the same identity.
                record.product = instrument.clone();
                changed = true;
            }
            if let ProviderCatalogEvent::SelectionInstalled { instrument, .. } = &event
                && record.role != EndpointRole::MarketScreen
            {
                let status = market.market_session_status(instrument, super::now_unix_nanos());
                endpoint
                    .messages
                    .send(MarketWorkerMessage::MarketSessionStatus(status))
                    .map_err(|error| error.to_string())?;
            }
            let _ = endpoint
                .messages
                .send(MarketWorkerMessage::ProviderCatalog(event));
            return Ok(changed);
        }
        None => event,
    };
    let Some(event) = event else { return Ok(false) };
    // A screen consumer owns no series, so any other event is not addressed to its presentation.
    if record.role == EndpointRole::MarketScreen {
        return Ok(false);
    }
    if let MarketRuntimeEvent::ProviderState(state) = &event
        && state.provider != record.product.provider
    {
        return Err("engine provider state identity mismatched".to_string());
    }
    if complete_pending_recovery(&event, &record.product, endpoint)? {
        return Ok(false);
    }
    let result = apply_pushed_event(
        event,
        &PushedEventContext {
            consumer_id: endpoint.consumer_id,
            active_generation: endpoint.active_generation,
            realtime: true,
            instrument: &record.product,
        },
        &mut endpoint.live,
        &endpoint.messages,
    );
    if let Err(error) = result {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
    Ok(false)
}

fn apply_selection_outcome(
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
            if record
                .pending_selection
                .as_ref()
                .is_none_or(|(pending, _, _)| *pending == generation.0.get())
            {
                record.product = selected.clone();
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
                            .send(MarketWorkerMessage::EngineSwitchMarker {
                                sequence: generation.0.get(),
                            })
                            .map_err(|error| error.to_string())?;
                    }
                } else {
                    record
                        .endpoint
                        .messages
                        .send(MarketWorkerMessage::SelectedInstrument(selected))
                        .map_err(|error| error.to_string())?;
                }
                record.endpoint.active_generation = generation.0.get();
                record.endpoint.live = false;
                record.session_status_sent = None;
            }
        }
        aeris_market_runtime::SelectionOutcome::Failed { generation, error } => {
            if record
                .pending_selection
                .as_ref()
                .is_some_and(|(pending, _, _)| *pending == generation.0.get())
            {
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
                }
                record
                    .endpoint
                    .messages
                    .send(MarketWorkerMessage::State {
                        state: if record.endpoint.live {
                            ChartState::Ready
                        } else {
                            ChartState::Loading
                        },
                        message: format!(
                            "Market selection failed; retaining previous chart: {error:?}"
                        ),
                    })
                    .map_err(|error| error.to_string())?;
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
    let _ = market.remove_consumer(client_id, endpoint.consumer_id);
    endpoint.active = false;
    let _ = endpoint.shutdown.try_send(());
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn session_status_is_resent_when_the_calendar_or_product_changes() {
        let sent = ("tastytrade:Future:/GCZ6".to_string(), 3);
        assert!(session_status_stale(None, "tastytrade:Future:/GCZ6", 0));
        assert!(!session_status_stale(
            Some(&sent),
            "tastytrade:Future:/GCZ6",
            3
        ));
        assert!(
            session_status_stale(Some(&sent), "tastytrade:Future:/GCZ6", 4),
            "a status computed before the calendar loaded must be replaced"
        );
        assert!(
            session_status_stale(Some(&sent), "tastytrade:Future:/ESZ6", 3),
            "a chart that switched instrument needs its own status"
        );
    }
}
