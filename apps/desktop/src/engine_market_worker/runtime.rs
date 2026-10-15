//! Runtime.

use super::{
    ChartState, EndpointRecord, EndpointRole, FeedConnectionState, MarketRuntimeEvent,
    MarketService, MarketWorkerCommand, MarketWorkerMessage, PushedEventContext, WorkerEndpoint,
    apply_pushed_event, classify_provider_catalog_event, complete_pending_recovery,
    handle_startup_catalog_event, initialize_catalog_endpoint, initialize_endpoint,
    initialize_market_screen_endpoint, mpsc, process_command, shared_market_runtime,
};
use aeris_desktop::market_worker::ProviderCatalogEvent;
use aeris_observability::diagnostic;
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
    let result = run_attached_workers(&market, client_id, endpoints, additions);
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

    let mut additions = additions;
    let mut next_session_status_at = Instant::now();
    while additions.is_some() || endpoints.iter().any(|record| record.endpoint.active) {
        if let Some(receiver) = additions.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(mut record) => match initialize_record(market, client_id, &mut record) {
                        Ok(()) => endpoints.push(record),
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
            }
        }
        endpoints.retain(|record| record.endpoint.active);
        let session_tick_due = Instant::now() >= next_session_status_at;
        publish_session_statuses(market, endpoints, session_tick_due);
        if session_tick_due {
            next_session_status_at = Instant::now() + Duration::from_secs(60);
        }
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
            continue;
        }
        let events = match market.drain_events(client_id, &consumer_budgets) {
            Ok(events) => events,
            Err(error) => {
                report_runtime_failure(endpoints, &error);
                return Err(error);
            }
        };
        let empty = events.is_empty();
        for event in events {
            if let Err(error) = apply_received_event(market, client_id, event, endpoints) {
                report_runtime_failure(endpoints, &error);
                return Err(error);
            }
        }
        if empty {
            let _ready = market.wait_for_events(
                client_id,
                next_session_status_at.saturating_duration_since(Instant::now()),
            );
        }
    }
    Ok(())
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
    client_id: u64,
    (consumer_id, event): (u64, aeris_market_runtime::MarketConsumerEvent),
    endpoints: &mut [EndpointRecord],
) -> Result<(), String> {
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
        return Ok(());
    }
    let Some(record) = endpoints
        .iter_mut()
        .find(|record| record.endpoint.active && record.endpoint.consumer_id == consumer_id)
    else {
        return Ok(());
    };
    match handle_startup_catalog_event(market, client_id, record, &event) {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                state: ChartState::Error,
                message: error,
            });
            return Ok(());
        }
    }
    let endpoint = &mut record.endpoint;
    let (catalog, event) = classify_provider_catalog_event(event);
    let event = match catalog {
        Some(event) => {
            if let ProviderCatalogEvent::SelectionInstalled { instrument, .. } = &event
                && let Err(error) = aeris_desktop::trading::register_provider_instrument(instrument)
            {
                aeris_desktop::trading::report_practice_registration_error(error);
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
            return Ok(());
        }
        None => event,
    };
    let Some(event) = event else { return Ok(()) };
    // A screen consumer owns no series, so any other event is not addressed to its presentation.
    if record.role == EndpointRole::MarketScreen {
        return Ok(());
    }
    if let MarketRuntimeEvent::ProviderState(state) = &event
        && state.provider != record.product.provider
    {
        return Err("engine provider state identity mismatched".to_string());
    }
    if complete_pending_recovery(&event, &record.product, endpoint)? {
        return Ok(());
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
