//! Runtime.

use super::{
    ChartState, Duration, ENGINE_RESTARTED_MESSAGE, EVENT_WAIT, EndpointRecord, EngineSupervisor,
    FeedConnectionState, MARKET_EVENTS_PER_POLL, MarketWorkerCommand, MarketWorkerMessage,
    PushedEventContext, RESTORE_BACKOFF, WorkerEndpoint, apply_pushed_event,
    classify_provider_catalog_event, complete_pending_recovery, envelope, initialize_endpoint,
    mpsc, process_command, set_resource_class, thread,
};

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
                message: "Connecting to the resident market engine".to_string(),
                transport_rtt_nanos: None,
            });
    }
    let mut supervisor = EngineSupervisor::connect(client_id)?;
    let result = run_attached_workers(&mut supervisor, endpoints, additions);
    for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
        let _ = supervisor.remove_market_consumer(record.endpoint.consumer_id);
        record.endpoint.active = false;
        let _ = record.endpoint.shutdown.try_send(());
    }
    let detach_result = supervisor.detach();
    result.and(detach_result)
}

fn run_attached_workers(
    client: &mut EngineSupervisor,
    endpoints: &mut Vec<EndpointRecord>,
    additions: Option<mpsc::Receiver<EndpointRecord>>,
) -> Result<(), String> {
    for record in endpoints.iter_mut() {
        initialize_endpoint(
            client,
            record.workspace_id,
            &mut record.product,
            record.interval,
            &mut record.endpoint,
        )?;
    }

    let mut additions = additions;
    while additions.is_some() || endpoints.iter().any(|record| record.endpoint.active) {
        if let Some(receiver) = additions.as_ref() {
            loop {
                match receiver.try_recv() {
                    Ok(mut record) => {
                        match initialize_endpoint(
                            client,
                            record.workspace_id,
                            &mut record.product,
                            record.interval,
                            &mut record.endpoint,
                        ) {
                            Ok(()) => endpoints.push(record),
                            Err(error) => {
                                let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                                    state: ChartState::Error,
                                    message: error,
                                });
                                retire_endpoint(client, &mut record.endpoint);
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        additions = None;
                        break;
                    }
                }
            }
        }
        for record in endpoints.iter_mut().filter(|record| record.endpoint.active) {
            process_pending_foreground_selection(client, record);
            process_pending_resource_class(client, &mut record.endpoint);
            match record.endpoint.commands.try_recv() {
                Ok(command) => {
                    if let Err(error) = process_command(client, record, command) {
                        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
                            state: ChartState::Error,
                            message: error,
                        });
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    retire_endpoint(client, &mut record.endpoint);
                }
            }
        }
        endpoints.retain(|record| record.endpoint.active);
        // A failed restore must never exit this thread: the engine may be
        // restarting or waiting on sign-in, and exiting here stranded charts
        // in a terminal error until the trader reloaded or changed symbols.
        // Report recovering and retry on the next tick instead.
        match receive_and_apply_event(client, endpoints, EVENT_WAIT) {
            Ok(received) => {
                if received {
                    for _ in 1..MARKET_EVENTS_PER_POLL {
                        match receive_and_apply_event(client, endpoints, Duration::ZERO) {
                            Ok(more) => {
                                if !more {
                                    break;
                                }
                            }
                            Err(error) => {
                                note_engine_restore_failure(endpoints, &error);
                                break;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                note_engine_restore_failure(endpoints, &error);
            }
        }
    }
    Ok(())
}

fn process_pending_foreground_selection(
    client: &mut EngineSupervisor,
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
            client,
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
        && let Err(error) =
            process_command(client, record, MarketWorkerCommand::EngineSelect(selection))
    {
        let _ = record.endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
}

/// Reports a failed engine restore as recovering on every active endpoint
/// and backs off before the next retry. The redacted engine detail stays
/// visible so an unauthenticated engine still tells the trader to sign in,
/// but the worker thread survives to retry once the engine is back.
pub(super) fn note_engine_restore_failure(endpoints: &[EndpointRecord], error: &str) {
    for record in endpoints.iter().filter(|record| record.endpoint.active) {
        let _ = record
            .endpoint
            .messages
            .send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message: error.to_string(),
                transport_rtt_nanos: None,
            });
    }
    thread::sleep(RESTORE_BACKOFF);
}

/// Receives and applies one pushed engine event. Returns whether one arrived.
fn receive_and_apply_event(
    client: &mut EngineSupervisor,
    endpoints: &mut [EndpointRecord],
    timeout: Duration,
) -> Result<bool, String> {
    let received = client.receive_market_event(timeout)?;
    if received.reconnected {
        for endpoint in endpoints
            .iter_mut()
            .filter(|record| record.endpoint.active)
            .map(|record| &mut record.endpoint)
        {
            endpoint.publication = None;
            let _ = endpoint.messages.send(MarketWorkerMessage::Connection {
                state: FeedConnectionState::Recovering,
                message: ENGINE_RESTARTED_MESSAGE.to_string(),
                transport_rtt_nanos: None,
            });
        }
        return Ok(true);
    }
    let (Some(consumer_id), Some(event)) = (received.consumer_id, received.event) else {
        return Ok(false);
    };
    if consumer_id == 0 {
        let envelope::Payload::Fault(fault) = event else {
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
        return Ok(true);
    }
    let Some(record) = endpoints
        .iter_mut()
        .find(|record| record.endpoint.active && record.endpoint.consumer_id == consumer_id)
    else {
        return Ok(true);
    };
    let endpoint = &mut record.endpoint;
    let (catalog, event) =
        classify_provider_catalog_event(event, &record.product.provider, endpoint.consumer_id);
    let event = match catalog {
        Some(event) => {
            let _ = endpoint
                .messages
                .send(MarketWorkerMessage::ProviderCatalog(event));
            return Ok(true);
        }
        None => event,
    };
    let Some(event) = event else { return Ok(true) };
    if complete_pending_recovery(&event, &record.product, endpoint)? {
        return Ok(true);
    }
    let result = apply_pushed_event(
        event,
        &PushedEventContext {
            consumer_id: endpoint.consumer_id,
            active_generation: endpoint.active_generation,
            realtime: true,
            instrument: &record.product,
        },
        &mut endpoint.publication,
        &mut endpoint.live,
        &endpoint.messages,
    );
    if let Err(error) = result {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
    Ok(true)
}

fn process_pending_resource_class(client: &mut EngineSupervisor, endpoint: &mut WorkerEndpoint) {
    let pending = endpoint
        .pending_resource_class
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(resource_class) = pending
        && let Err(error) = set_resource_class(client, endpoint, resource_class)
    {
        let _ = endpoint.messages.send(MarketWorkerMessage::State {
            state: ChartState::Error,
            message: error,
        });
    }
}

pub(super) fn retire_endpoint(client: &mut EngineSupervisor, endpoint: &mut WorkerEndpoint) {
    let _ = client.remove_market_consumer(endpoint.consumer_id);
    endpoint.active = false;
    let _ = endpoint.shutdown.try_send(());
}
