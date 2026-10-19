//! Runtime.

use super::{
    ChartState, EndpointRecord, EndpointRole, FeedConnectionState, MarketRuntimeEvent,
    MarketService, MarketWorkerCommand, MarketWorkerMessage, PushedEventContext, WorkerEndpoint,
    apply_pushed_event, classify_provider_catalog_event, complete_pending_recovery,
    initialize_catalog_endpoint, initialize_endpoint, initialize_market_screen_endpoint, mpsc,
    process_command,
};
use aeris_desktop::market_worker::{
    MarketWorkerSendError, MarketWorkerSender, ProviderCatalogEvent,
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
                            report_state(&record.endpoint.messages, ChartState::Error, error);
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
                    report_state(&record.endpoint.messages, ChartState::Error, error);
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
        match apply_received_event(market, client_id, event, endpoints, clock) {
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

/// Sends each chart its market session status when its product or the runtime calendar
/// changed since the last send, and on every `tick_due` so phase boundaries are crossed.
fn publish_session_statuses(
    market: &MarketService,
    endpoints: &mut [EndpointRecord],
    tick_due: bool,
    clock: &dyn Clock,
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
        let now = *now.get_or_insert_with(|| clock.wall_unix_nanos());
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
        report_state(
            &record.endpoint.messages,
            ChartState::Error,
            error.to_string(),
        );
    }
}

pub(super) fn report_state(messages: &MarketWorkerSender, state: ChartState, message: String) {
    match messages.send(MarketWorkerMessage::State { state, message }) {
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
    _client_id: u64,
    (consumer_id, event): (u64, aeris_market_runtime::MarketConsumerEvent),
    endpoints: &mut [EndpointRecord],
    clock: &dyn Clock,
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
            report_state(
                &endpoint.messages,
                ChartState::Error,
                fault.redacted_detail.clone(),
            );
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
                let status = market.market_session_status(instrument, clock.wall_unix_nanos());
                endpoint
                    .messages
                    .send(MarketWorkerMessage::MarketSessionStatus(status))
                    .map_err(|error| error.to_string())?;
            }
            match endpoint
                .messages
                .send(MarketWorkerMessage::ProviderCatalog(event))
            {
                Ok(()) => return Ok(changed),
                Err(MarketWorkerSendError::Disconnected) => {
                    // A released pane cannot receive this catalog result; its
                    // command channel closure retires the runtime consumer.
                    return Ok(false);
                }
                Err(MarketWorkerSendError::Full) => {
                    return Err("market catalog presentation mailbox is full".to_string());
                }
            }
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
    if complete_pending_recovery(&event, &record.product, endpoint, clock.wall_unix_nanos())? {
        return Ok(false);
    }
    let result = apply_pushed_event(
        event,
        &PushedEventContext {
            consumer_id: endpoint.consumer_id,
            active_generation: endpoint.active_generation,
            realtime: true,
            instrument: &record.product,
            received_unix_nanos: clock.wall_unix_nanos(),
        },
        &mut endpoint.live,
        &endpoint.messages,
    );
    if let Err(error) = result {
        report_state(&endpoint.messages, ChartState::Error, error);
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
    use aeris_trading_runtime::clock::ManualClock;
    use std::num::NonZeroUsize;

    #[test]
    fn runtime_failure_uses_bounded_pane_mailbox_and_tolerates_retirement() {
        let (messages, receiver) = super::super::market_worker_channel(NonZeroUsize::MIN);
        report_state(&messages, ChartState::Error, "market unavailable".into());
        let (queued, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            queued.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Error,
                message,
            }] if message == "market unavailable"
        ));
        for _ in 0..9 {
            report_state(&messages, ChartState::Error, "market unavailable".into());
        }
        report_state(&messages, ChartState::Error, "saturated pane".into());
        assert_eq!(messages.occupancy().0, 9, "control reserve remains bounded");
        drop(receiver);
        report_state(&messages, ChartState::Error, "retired pane".into());
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
        let MarketWorkerMessage::MarketSessionStatus(first) = first else {
            panic!("expected first session status");
        };
        assert_eq!(first, market.market_session_status(&product, first_time));

        clock.set_wall_unix_nanos(second_time);
        publish_session_statuses(&market, &mut records, true, &clock);
        let (messages, disconnected) = pane.worker.drain_messages();
        assert!(!disconnected);
        let second = messages.into_iter().next().expect("second session status");
        let MarketWorkerMessage::MarketSessionStatus(second) = second else {
            panic!("expected second session status");
        };
        assert_eq!(second, market.market_session_status(&product, second_time));
        assert_ne!(
            first, second,
            "different sessions must follow the injected clock"
        );
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
