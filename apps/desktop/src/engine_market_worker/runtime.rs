//! Runtime.

use super::{
    ChartState, EVENT_WAIT, EndpointRecord, FeedConnectionState, MarketRuntimeEvent, MarketService,
    MarketWorkerCommand, MarketWorkerMessage, PushedEventContext, WorkerEndpoint,
    apply_pushed_event, classify_provider_catalog_event, complete_pending_recovery,
    handle_startup_catalog_event, initialize_catalog_endpoint, initialize_endpoint, mpsc,
    process_command, set_resource_class, shared_market_runtime, thread,
};
use aeris_desktop::market_worker::ProviderCatalogEvent;
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
            process_pending_foreground_selection(market, client_id, record);
            process_pending_resource_class(market, client_id, &mut record.endpoint);
            process_pending_depth_visibility(market, client_id, record);
            process_pending_price_alerts(market, client_id, record);
            match record.endpoint.commands.try_recv() {
                Ok(command) => {
                    if let Err(error) = process_command(market, client_id, record, command) {
                        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                            state: ChartState::Error,
                            message: error,
                        });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    retire_endpoint(market, client_id, &mut record.endpoint);
                }
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
            thread::sleep(EVENT_WAIT);
            continue;
        }
        let events = match market.poll_events(client_id, &consumer_budgets) {
            Ok(events) => events,
            Err(error) => {
                report_runtime_failure(endpoints, &error);
                return Err(error);
            }
        };
        if events.is_empty() {
            thread::sleep(EVENT_WAIT);
        }
        for event in events {
            if let Err(error) = apply_received_event(market, client_id, event, endpoints) {
                report_runtime_failure(endpoints, &error);
                return Err(error);
            }
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
        .filter(|record| record.endpoint.active && !record.catalog_only)
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
            eprintln!("Aeris market session status was not delivered: {error}");
        }
        record.session_status_sent = Some((record.product.instrument_id.clone(), revision));
    }
}

fn session_status_stale(sent: Option<&(String, u64)>, instrument_id: &str, revision: u64) -> bool {
    sent.is_none_or(|(sent_instrument, sent_revision)| {
        sent_instrument != instrument_id || *sent_revision != revision
    })
}

fn process_pending_foreground_selection(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) {
    let provider_selection = record
        .endpoint
        .pending_provider_selection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(selection) = provider_selection
        && let Err(error) = process_command(
            market,
            client_id,
            record,
            MarketWorkerCommand::ProviderSelect(selection),
        )
    {
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }

    let engine_selection = record
        .endpoint
        .pending_engine_selection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(selection) = engine_selection
        && let Err(error) = process_command(
            market,
            client_id,
            record,
            MarketWorkerCommand::EngineSelect(selection),
        )
    {
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
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
    if record.catalog_only {
        initialize_catalog_endpoint(market, client_id, record.workspace_id, &mut record.endpoint)
    } else {
        initialize_endpoint(market, client_id, record)
    }
}

/// Applies one event drained directly from the runtime-owned consumer outbox.
fn apply_received_event(
    market: &MarketService,
    client_id: u64,
    (consumer_id, event): (u64, MarketRuntimeEvent),
    endpoints: &mut [EndpointRecord],
) -> Result<(), String> {
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
            if let ProviderCatalogEvent::SelectionInstalled { instrument, .. } = &event {
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

fn process_pending_resource_class(
    market: &MarketService,
    client_id: u64,
    endpoint: &mut WorkerEndpoint,
) {
    let pending = endpoint
        .pending_resource_class
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(resource_class) = pending
        && let Err(error) = set_resource_class(market, client_id, endpoint, resource_class)
    {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
}

fn process_pending_depth_visibility(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) {
    let pending = record
        .endpoint
        .pending_depth_visible
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(visible) = pending else { return };
    if let Err(error) = super::selection_commands::set_depth_visible(
        market,
        client_id,
        &mut record.endpoint,
        &record.product.provider,
        record.startup_resolution.is_some(),
        visible,
    ) {
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
}

fn process_pending_price_alerts(
    market: &MarketService,
    client_id: u64,
    record: &mut EndpointRecord,
) {
    let pending = record
        .endpoint
        .pending_price_alerts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(alerts) = pending {
        let _ = process_command(
            market,
            client_id,
            record,
            MarketWorkerCommand::ReplacePriceAlerts(alerts),
        );
    }
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
