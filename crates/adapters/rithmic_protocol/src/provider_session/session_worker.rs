//! Session worker.

use super::{
    AtomicBool, AuthenticationState, CanonicalSessionState, CatalogCommandState,
    CollectionProgress, CollectorError, DecodedCatalogMessage, DecodedControlMessage,
    DecodedMarketMessage, DepthByOrderSnapshotMessage, DirectSessionState, Duration,
    InitialSubscriptionAck, Instant, InstrumentDescriptor, InstrumentReference,
    InstrumentReferenceRequest, MAXIMUM_INITIAL_DBO_SNAPSHOT_MESSAGES,
    MAXIMUM_INITIAL_SUBSCRIPTION_MESSAGES, MAXIMUM_MBO_ORDERS, MarketDataSubscription,
    MarketIdentity, NonZeroUsize, Ordering, PendingCatalogCommand, PendingSubscription,
    ProviderInvalidationReason, ProviderSessionEvent, Receiver, RetryDisposition,
    RithmicAuthorizedSilenceEvidenceFault, RithmicCatalogEvent, RithmicCatalogRejection,
    RithmicInstrumentSelection, RithmicProviderConfig, RithmicProviderInstrument,
    RithmicSelectionMode, RithmicSessionCommand, RithmicSessionError, RithmicSessionMessage,
    SESSION_COMMAND_BATCH, SESSION_COMMAND_POLL_INTERVAL, SessionEmitter, SessionGeneration,
    SubscriptionAcknowledgement, SubscriptionAction, SubscriptionPhase, SymbolSearchCollector,
    TryRecvError, VecDeque, malformed, session_failure, thread, unix_nanos_now,
    validate_provider_instruments,
};

pub(super) fn emit_connection_failure(
    emitter: &SessionEmitter,
    generation: SessionGeneration,
    error: RithmicSessionError,
) {
    if error == RithmicSessionError::LoginRejected {
        let _ = emitter.send(ProviderSessionEvent::AuthenticationChanged {
            generation,
            state: AuthenticationState::Rejected,
        });
    }
    let (reason, retry) = session_failure(error);
    emitter.invalid(reason, retry);
}

pub(super) fn install_subscriptions(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    stop: &AtomicBool,
) -> Result<VecDeque<RithmicSessionMessage>, RithmicSessionError> {
    let mut initial_messages = VecDeque::new();
    let mut messages_read = 0_usize;
    for instrument in &config.instruments {
        connection.update_market_data(instrument.as_request())?;
        if stop.load(Ordering::Acquire) {
            return Err(RithmicSessionError::Cancelled);
        }
        await_initial_subscription_ack(
            connection,
            config.session_limits.response_timeout,
            InitialSubscriptionAck::MarketData,
            &mut messages_read,
            &mut initial_messages,
        )?;
        if instrument.order_book {
            connection
                .update_depth_by_order(instrument.depth_request(SubscriptionAction::Subscribe))?;
            await_initial_subscription_ack(
                connection,
                config.session_limits.response_timeout,
                InitialSubscriptionAck::DepthByOrder,
                &mut messages_read,
                &mut initial_messages,
            )?;
            connection.request_depth_by_order_snapshot(instrument.depth_snapshot_request())?;
            await_initial_depth_snapshot(
                connection,
                config.session_limits.response_timeout,
                instrument,
                &mut initial_messages,
            )?;
        }
    }
    Ok(initial_messages)
}

fn await_initial_depth_snapshot(
    connection: &mut crate::RithmicTickerConnection,
    response_timeout: Duration,
    instrument: &RithmicProviderInstrument,
    initial_messages: &mut VecDeque<RithmicSessionMessage>,
) -> Result<(), RithmicSessionError> {
    let deadline = Instant::now() + response_timeout;
    let mut messages_read = 0_usize;
    let mut retained_orders = 0_usize;
    loop {
        if messages_read >= MAXIMUM_INITIAL_DBO_SNAPSHOT_MESSAGES {
            return Err(RithmicSessionError::Protocol);
        }
        let mut message = connection.read_next_until(deadline)?;
        messages_read = messages_read.saturating_add(1);
        match &mut message {
            RithmicSessionMessage::Market(DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::Level(level),
            )) => {
                if !instrument.identity_matches(&level.identity) {
                    return Err(RithmicSessionError::Protocol);
                }
                retained_orders = retained_orders
                    .checked_add(level.orders.len())
                    .filter(|orders| *orders <= MAXIMUM_MBO_ORDERS)
                    .ok_or(RithmicSessionError::Protocol)?;
                initial_messages.push_back(message);
            }
            RithmicSessionMessage::Market(DecodedMarketMessage::DepthByOrderSnapshot(
                DepthByOrderSnapshotMessage::Complete {
                    accepted, identity, ..
                },
            )) => {
                if !*accepted {
                    return Err(RithmicSessionError::Protocol);
                }
                if identity.is_none() {
                    *identity = Some(MarketIdentity {
                        symbol: instrument.descriptor.provider_symbol.clone(),
                        exchange: instrument.descriptor.venue_id.clone(),
                    });
                } else if identity
                    .as_ref()
                    .is_some_and(|identity| !instrument.identity_matches(identity))
                {
                    return Err(RithmicSessionError::Protocol);
                }
                initial_messages.push_back(message);
                return Ok(());
            }
            RithmicSessionMessage::Control(
                DecodedControlMessage::Reject | DecodedControlMessage::ForcedLogout,
            ) => return Err(RithmicSessionError::Protocol),
            _ => initial_messages.push_back(message),
        }
    }
}

fn await_initial_subscription_ack(
    connection: &mut crate::RithmicTickerConnection,
    response_timeout: Duration,
    expected: InitialSubscriptionAck,
    messages_read: &mut usize,
    initial_messages: &mut VecDeque<RithmicSessionMessage>,
) -> Result<(), RithmicSessionError> {
    let deadline = Instant::now() + response_timeout;
    while *messages_read < MAXIMUM_INITIAL_SUBSCRIPTION_MESSAGES {
        let message = connection.read_next_until(deadline)?;
        *messages_read = messages_read.saturating_add(1);
        let matching = match &message {
            RithmicSessionMessage::Control(DecodedControlMessage::MarketDataSubscription {
                accepted,
            }) if expected == InitialSubscriptionAck::MarketData => Some(*accepted),
            RithmicSessionMessage::Control(DecodedControlMessage::DepthByOrderSubscription {
                accepted,
            }) if expected == InitialSubscriptionAck::DepthByOrder => Some(*accepted),
            RithmicSessionMessage::Control(
                DecodedControlMessage::Reject | DecodedControlMessage::ForcedLogout,
            ) => return Err(RithmicSessionError::Protocol),
            _ => None,
        };
        if let Some(accepted) = matching {
            return accepted.then_some(()).ok_or(RithmicSessionError::Protocol);
        }
        initial_messages.push_back(message);
    }
    Err(RithmicSessionError::Deadline)
}

fn catalog_command_deadline(state: &CatalogCommandState) -> Option<Instant> {
    match state.pending_catalog.as_ref() {
        Some(
            PendingCatalogCommand::Search { deadline, .. }
            | PendingCatalogCommand::Reference { deadline, .. },
        ) => Some(*deadline),
        None => state
            .pending_subscription
            .as_ref()
            .map(|subscription| subscription.deadline),
    }
}

pub(super) fn catalog_command_timed_out(state: &CatalogCommandState, now: Instant) -> bool {
    catalog_command_deadline(state).is_some_and(|deadline| now >= deadline)
}

pub(super) fn heartbeat_due(
    state: &DirectSessionState,
    now: Instant,
    initial_messages_pending: bool,
) -> bool {
    !initial_messages_pending && state.heartbeat_deadline.is_none() && now >= state.next_heartbeat
}

pub(super) fn collect_market(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    generation: SessionGeneration,
    stop: &AtomicBool,
    commands: &Receiver<RithmicSessionCommand>,
    emitter: &SessionEmitter,
    mut initial_messages: VecDeque<RithmicSessionMessage>,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let mut canonical = CanonicalSessionState::new(config, generation);
    let heartbeat_interval = connection.heartbeat_interval();
    let silence_evidence_fault = config.silence_evidence_fault(generation);
    if let Some(fault) = silence_evidence_fault {
        return collect_authorized_silence_evidence(connection, config, stop, fault);
    }
    let started = Instant::now();
    let mut state = DirectSessionState {
        last_message: started,
        // Take the first RTT sample as soon as buffered subscription messages
        // are drained. Later heartbeats retain the negotiated provider cadence.
        next_heartbeat: started,
        heartbeat_deadline: None,
        response_timeout: config.session_limits.response_timeout,
        source_ordinal: 0,
        catalog: CatalogCommandState::default(),
    };
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        if state.catalog.pending_catalog.is_none() && state.catalog.pending_subscription.is_none() {
            process_session_commands(
                connection,
                commands,
                generation,
                emitter,
                &mut state.catalog,
                &mut canonical,
                config.session_limits.response_timeout,
            )?;
        }
        let now = Instant::now();
        if catalog_command_timed_out(&state.catalog, now) {
            return Err(session_failure(RithmicSessionError::Deadline));
        }
        if let Some(invalidation) = silence_invalidation(&state, config, now) {
            return Err(invalidation);
        }
        if heartbeat_due(&state, now, !initial_messages.is_empty()) {
            connection.send_heartbeat().map_err(session_failure)?;
            state.heartbeat_deadline = Some(now + config.session_limits.response_timeout);
            state.next_heartbeat = now + heartbeat_interval;
        }
        let mut deadline = (state.last_message + config.message_silence_timeout)
            .min(now + SESSION_COMMAND_POLL_INTERVAL);
        if let Some(pending) = state.heartbeat_deadline {
            deadline = deadline.min(pending);
        } else {
            deadline = deadline.min(state.next_heartbeat);
        }
        let message = match initial_messages.pop_front() {
            Some(message) => message,
            None => match connection.read_next_until(deadline) {
                Ok(message) => message,
                Err(RithmicSessionError::Deadline) => continue,
                Err(RithmicSessionError::Cancelled) if stop.load(Ordering::Acquire) => {
                    return Ok(());
                }
                Err(error) => return Err(session_failure(error)),
            },
        };
        state.last_message = Instant::now();
        state.source_ordinal = state.source_ordinal.checked_add(1).ok_or_else(malformed)?;
        if !handle_session_message(
            connection,
            message,
            generation,
            emitter,
            &mut state,
            &mut canonical,
            stop,
        )? {
            return Ok(());
        }
    }
}

fn collect_authorized_silence_evidence(
    connection: &mut crate::RithmicTickerConnection,
    config: &RithmicProviderConfig,
    stop: &AtomicBool,
    fault: RithmicAuthorizedSilenceEvidenceFault,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    const POLL_INTERVAL: Duration = Duration::from_millis(10);
    let started = Instant::now();
    let mut state = DirectSessionState {
        last_message: started,
        next_heartbeat: started + config.message_silence_timeout,
        heartbeat_deadline: None,
        response_timeout: config.session_limits.response_timeout,
        source_ordinal: 0,
        catalog: CatalogCommandState::default(),
    };
    if fault == RithmicAuthorizedSilenceEvidenceFault::Heartbeat {
        connection.send_heartbeat().map_err(session_failure)?;
        state.heartbeat_deadline = Some(started + config.session_limits.response_timeout);
    }
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let now = Instant::now();
        if let Some(invalidation) = silence_invalidation(&state, config, now) {
            return Err(invalidation);
        }
        let deadline = match fault {
            RithmicAuthorizedSilenceEvidenceFault::Heartbeat => state
                .heartbeat_deadline
                .unwrap_or(started + config.session_limits.response_timeout),
            RithmicAuthorizedSilenceEvidenceFault::Message => {
                started + config.message_silence_timeout
            }
        };
        thread::sleep(deadline.saturating_duration_since(now).min(POLL_INTERVAL));
    }
}

pub(super) fn silence_invalidation(
    state: &DirectSessionState,
    config: &RithmicProviderConfig,
    now: Instant,
) -> Option<(ProviderInvalidationReason, RetryDisposition)> {
    if now.duration_since(state.last_message) >= config.message_silence_timeout {
        return Some((
            ProviderInvalidationReason::MessageSilence,
            RetryDisposition::Transient,
        ));
    }
    state
        .heartbeat_deadline
        .filter(|deadline| now >= *deadline)
        .map(|_| {
            (
                ProviderInvalidationReason::HeartbeatSilence,
                RetryDisposition::Transient,
            )
        })
}

pub(super) fn heartbeat_transport_rtt_nanos(
    state: &DirectSessionState,
    received_at: Instant,
) -> Option<u64> {
    let deadline = state.heartbeat_deadline?;
    let sent_at = deadline.checked_sub(state.response_timeout)?;
    let nanos = received_at.saturating_duration_since(sent_at).as_nanos();
    Some(u64::try_from(nanos).unwrap_or(u64::MAX).max(1))
}

#[allow(clippy::too_many_lines)]
fn handle_session_message(
    connection: &mut crate::RithmicTickerConnection,
    message: RithmicSessionMessage,
    generation: SessionGeneration,
    emitter: &SessionEmitter,
    state: &mut DirectSessionState,
    canonical: &mut CanonicalSessionState,
    stop: &AtomicBool,
) -> Result<bool, (ProviderInvalidationReason, RetryDisposition)> {
    let response_timeout = state.response_timeout;
    match message {
        RithmicSessionMessage::Catalog(message) => {
            handle_catalog_message(
                connection,
                message,
                generation,
                emitter,
                &mut state.catalog,
                canonical,
                response_timeout,
            )?;
        }
        RithmicSessionMessage::Control(DecodedControlMessage::MarketDataSubscription {
            accepted,
        }) if state.catalog.pending_subscription.is_some() => {
            advance_subscription(
                connection,
                generation,
                emitter,
                SubscriptionAcknowledgement::MarketData(accepted),
                &mut state.catalog.pending_subscription,
                canonical,
                response_timeout,
            )?;
        }
        RithmicSessionMessage::Control(DecodedControlMessage::DepthByOrderSubscription {
            accepted,
        }) if state.catalog.pending_subscription.is_some() => {
            advance_subscription(
                connection,
                generation,
                emitter,
                SubscriptionAcknowledgement::DepthByOrder(accepted),
                &mut state.catalog.pending_subscription,
                canonical,
                response_timeout,
            )?;
        }
        RithmicSessionMessage::Market(mut message) => {
            let snapshot_completion = match &mut message {
                DecodedMarketMessage::DepthByOrderSnapshot(
                    DepthByOrderSnapshotMessage::Complete {
                        accepted, identity, ..
                    },
                ) => {
                    if let Some(plan) = state.catalog.pending_subscription.as_ref()
                        && let SubscriptionPhase::SubscribeDepthSnapshot(index) = plan.phase
                    {
                        let instrument = plan.added.get(index).ok_or_else(malformed)?;
                        if let Some(identity) = identity.as_ref()
                            && !instrument.identity_matches(identity)
                        {
                            return Err(malformed());
                        }
                        if identity.is_none() {
                            *identity = Some(MarketIdentity {
                                symbol: instrument.descriptor.provider_symbol.clone(),
                                exchange: instrument.descriptor.venue_id.clone(),
                            });
                        }
                        if !*accepted {
                            if let Some((selection, _)) = plan.selection.as_ref() {
                                send_rejection(
                                    emitter,
                                    generation,
                                    selection.selection_generation,
                                    RithmicCatalogRejection::SubscriptionRejected,
                                );
                            }
                            return Err((
                                ProviderInvalidationReason::Transport,
                                RetryDisposition::Transient,
                            ));
                        }
                    }
                    // Only a completion owed to a pending subscription advances
                    // it; any other completion answers an in-session resnapshot.
                    state
                        .catalog
                        .pending_subscription
                        .as_ref()
                        .is_some_and(|plan| {
                            matches!(plan.phase, SubscriptionPhase::SubscribeDepthSnapshot(_))
                        })
                        .then_some(*accepted)
                }
                _ => None,
            };
            let received_unix_nanos = unix_nanos_now()?;
            if let Some(event) =
                canonical.convert(message, state.source_ordinal, received_unix_nanos)?
                && !emitter.send(ProviderSessionEvent::Market { generation, event })
            {
                stop.store(true, Ordering::Release);
                return Ok(false);
            }
            for instrument in canonical.take_depth_resnapshots() {
                connection
                    .request_depth_by_order_snapshot(instrument.depth_snapshot_request())
                    .map_err(session_failure)?;
            }
            if snapshot_completion.is_some_and(|accepted| accepted) {
                let complete = {
                    let Some(plan) = state.catalog.pending_subscription.as_mut() else {
                        return Err(malformed());
                    };
                    let SubscriptionPhase::SubscribeDepthSnapshot(index) = plan.phase else {
                        return Err(malformed());
                    };
                    advance_added_subscription(
                        connection,
                        plan,
                        index,
                        canonical,
                        response_timeout,
                    )?
                };
                if complete {
                    finish_subscription(
                        &mut state.catalog.pending_subscription,
                        generation,
                        emitter,
                    )?;
                }
            }
        }
        RithmicSessionMessage::Control(DecodedControlMessage::Heartbeat {
            accepted: true, ..
        }) => {
            let received_at = Instant::now();
            let transport_rtt_nanos = heartbeat_transport_rtt_nanos(state, received_at);
            state.heartbeat_deadline = None;
            if !emitter.send(ProviderSessionEvent::Heartbeat {
                generation,
                received_unix_nanos: unix_nanos_now()?,
                transport_rtt_nanos,
            }) {
                stop.store(true, Ordering::Release);
                return Ok(false);
            }
        }
        RithmicSessionMessage::Control(DecodedControlMessage::ForcedLogout) => {
            return Err((
                ProviderInvalidationReason::Authentication,
                RetryDisposition::Terminal,
            ));
        }
        _ => return Err(malformed()),
    }
    Ok(true)
}

fn process_session_commands(
    connection: &mut crate::RithmicTickerConnection,
    commands: &Receiver<RithmicSessionCommand>,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    state: &mut CatalogCommandState,
    canonical: &mut CanonicalSessionState,
    response_timeout: Duration,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    for _ in 0..SESSION_COMMAND_BATCH {
        let command = match commands.try_recv() {
            Ok(command) => command,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
        };
        match command {
            RithmicSessionCommand::Search(request) => {
                let collector = SymbolSearchCollector::try_new(request.collection_request())
                    .map_err(|_| malformed())?;
                if let Err(error) = connection.search_symbols(request.protocol_request()) {
                    let rejection = search_command_failure(error)?;
                    if !send_rejection(
                        emitter,
                        session_generation,
                        request.search_generation,
                        rejection,
                    ) {
                        return Err((
                            ProviderInvalidationReason::QueueOverflow,
                            RetryDisposition::Transient,
                        ));
                    }
                    continue;
                }
                state.pending_catalog = Some(PendingCatalogCommand::Search {
                    generation: request.search_generation,
                    collector,
                    deadline: Instant::now() + response_timeout,
                });
                return Ok(());
            }
            RithmicSessionCommand::Select(selection) => {
                let available =
                    state
                        .latest_search
                        .as_ref()
                        .is_some_and(|(generation, symbols)| {
                            *generation == selection.search_generation
                                && symbols.contains_key(&(
                                    selection.exchange.clone(),
                                    selection.symbol.clone(),
                                ))
                        });
                if !available {
                    let reason = state.latest_search.as_ref().map_or(
                        RithmicCatalogRejection::SupersededSearch,
                        |(generation, _)| {
                            if *generation == selection.search_generation {
                                RithmicCatalogRejection::InstrumentUnavailable
                            } else {
                                RithmicCatalogRejection::SupersededSearch
                            }
                        },
                    );
                    if !send_rejection(
                        emitter,
                        session_generation,
                        selection.selection_generation,
                        reason,
                    ) {
                        return Ok(());
                    }
                    continue;
                }
                connection
                    .request_instrument_reference(InstrumentReferenceRequest {
                        symbol: &selection.symbol,
                        exchange: market_data_exchange(&selection.exchange),
                    })
                    .map_err(session_failure)?;
                state.pending_catalog = Some(PendingCatalogCommand::Reference {
                    selection,
                    deadline: Instant::now() + response_timeout,
                });
                return Ok(());
            }
            RithmicSessionCommand::ReplaceSubscriptions(instruments) => {
                state.pending_subscription = start_subscription_replacement(
                    connection,
                    &instruments,
                    None,
                    canonical,
                    session_generation,
                    emitter,
                    response_timeout,
                )?;
                return Ok(());
            }
        }
    }
    Ok(())
}

pub(super) fn search_command_failure(
    error: RithmicSessionError,
) -> Result<RithmicCatalogRejection, (ProviderInvalidationReason, RetryDisposition)> {
    if error == RithmicSessionError::RequestInFlight {
        Ok(RithmicCatalogRejection::SearchRejected)
    } else {
        Err(session_failure(error))
    }
}

fn handle_catalog_message(
    connection: &mut crate::RithmicTickerConnection,
    message: DecodedCatalogMessage,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    state: &mut CatalogCommandState,
    canonical: &mut CanonicalSessionState,
    response_timeout: Duration,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let Some(pending) = state.pending_catalog.as_mut() else {
        return Err(malformed());
    };
    match pending {
        PendingCatalogCommand::Search {
            generation,
            collector,
            ..
        } => match collector.accept(message) {
            Ok(CollectionProgress::Pending) => Ok(()),
            Ok(CollectionProgress::Complete(symbols)) => {
                let search_generation = *generation;
                state.latest_search = Some((
                    search_generation,
                    symbols
                        .results
                        .iter()
                        .cloned()
                        .map(|symbol| ((symbol.exchange.clone(), symbol.symbol.clone()), symbol))
                        .collect(),
                ));
                state.pending_catalog = None;
                if !emitter.send_catalog(RithmicCatalogEvent::SearchCompleted {
                    session_generation,
                    search_generation,
                    symbols,
                }) {
                    return Err((
                        ProviderInvalidationReason::QueueOverflow,
                        RetryDisposition::Transient,
                    ));
                }
                Ok(())
            }
            Err(CollectorError::Rejected) => {
                let search_generation = *generation;
                state.pending_catalog = None;
                send_rejection(
                    emitter,
                    session_generation,
                    search_generation,
                    RithmicCatalogRejection::SearchRejected,
                );
                Ok(())
            }
            Ok(CollectionProgress::Unhandled(_)) | Err(_) => Err(malformed()),
        },
        PendingCatalogCommand::Reference { selection, .. } => {
            let DecodedCatalogMessage::InstrumentReference(reference) = message else {
                return Err(malformed());
            };
            let selection = selection.clone();
            state.pending_catalog = None;
            let Some(reference) = reference else {
                send_rejection(
                    emitter,
                    session_generation,
                    selection.selection_generation,
                    RithmicCatalogRejection::InstrumentUnavailable,
                );
                return Ok(());
            };
            let instrument = selected_instrument(&selection, reference)?;
            if selection.mode == RithmicSelectionMode::ReferenceOnly {
                if !emitter.send_catalog(RithmicCatalogEvent::SelectionInstalled {
                    session_generation,
                    selection_generation: selection.selection_generation,
                    instrument: instrument.descriptor,
                    entitlement_id: instrument.entitlement_id,
                }) {
                    return Err((
                        ProviderInvalidationReason::QueueOverflow,
                        RetryDisposition::Transient,
                    ));
                }
                return Ok(());
            }
            let target = [instrument];
            state.pending_subscription = start_subscription_replacement(
                connection,
                &target,
                Some(selection),
                canonical,
                session_generation,
                emitter,
                response_timeout,
            )?;
            Ok(())
        }
    }
}

fn start_subscription_replacement(
    connection: &mut crate::RithmicTickerConnection,
    target: &[RithmicProviderInstrument],
    selection: Option<RithmicInstrumentSelection>,
    canonical: &mut CanonicalSessionState,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    response_timeout: Duration,
) -> Result<Option<PendingSubscription>, (ProviderInvalidationReason, RetryDisposition)> {
    validate_provider_instruments(target).map_err(|_| malformed())?;
    let removed = canonical
        .instruments()
        .iter()
        .filter(|current| !target.contains(current))
        .cloned()
        .collect::<Vec<_>>();
    let added = target
        .iter()
        .filter(|requested| !canonical.instruments().contains(requested))
        .cloned()
        .collect::<Vec<_>>();
    if removed.is_empty() && added.is_empty() {
        if let Some(selection) = selection.as_ref() {
            let instrument = target.first().cloned().ok_or_else(malformed)?;
            emit_selection_installed(emitter, session_generation, selection, instrument)?;
        }
        return Ok(None);
    }
    let phase = if let Some(instrument) = removed.first() {
        connection
            .update_market_data(subscription_request(
                instrument,
                SubscriptionAction::Unsubscribe,
            ))
            .map_err(session_failure)?;
        SubscriptionPhase::UnsubscribeMarket(0)
    } else {
        begin_added_subscription(connection, &added, 0, canonical)?;
        SubscriptionPhase::SubscribeMarket(0)
    };
    let selection = selection.zip(target.first().cloned());
    Ok(Some(PendingSubscription {
        selection,
        removed,
        added,
        phase,
        deadline: Instant::now() + response_timeout,
    }))
}

fn advance_subscription(
    connection: &mut crate::RithmicTickerConnection,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
    acknowledgement: SubscriptionAcknowledgement,
    pending: &mut Option<PendingSubscription>,
    canonical: &mut CanonicalSessionState,
    response_timeout: Duration,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let complete = {
        let Some(plan) = pending.as_mut() else {
            return Err(malformed());
        };
        if !acknowledgement.accepted() {
            if let Some((selection, _)) = plan.selection.as_ref() {
                send_rejection(
                    emitter,
                    session_generation,
                    selection.selection_generation,
                    RithmicCatalogRejection::SubscriptionRejected,
                );
            }
            return Err((
                ProviderInvalidationReason::Transport,
                RetryDisposition::Transient,
            ));
        }
        match (plan.phase, acknowledgement) {
            (
                SubscriptionPhase::UnsubscribeMarket(index),
                SubscriptionAcknowledgement::MarketData(_),
            ) => {
                let instrument = plan.removed.get(index).ok_or_else(malformed)?;
                if instrument.order_book {
                    connection
                        .update_depth_by_order(
                            instrument.depth_request(SubscriptionAction::Unsubscribe),
                        )
                        .map_err(session_failure)?;
                    plan.phase = SubscriptionPhase::UnsubscribeDepth(index);
                    plan.deadline = Instant::now() + response_timeout;
                    false
                } else {
                    advance_removed_unsubscribe(
                        connection,
                        plan,
                        index,
                        canonical,
                        response_timeout,
                    )?
                }
            }
            (
                SubscriptionPhase::UnsubscribeDepth(index),
                SubscriptionAcknowledgement::DepthByOrder(_),
            ) => advance_removed_unsubscribe(connection, plan, index, canonical, response_timeout)?,
            (
                SubscriptionPhase::SubscribeMarket(index),
                SubscriptionAcknowledgement::MarketData(_),
            ) => {
                let instrument = plan.added.get(index).ok_or_else(malformed)?;
                if instrument.order_book {
                    connection
                        .update_depth_by_order(
                            instrument.depth_request(SubscriptionAction::Subscribe),
                        )
                        .map_err(session_failure)?;
                    plan.phase = SubscriptionPhase::SubscribeDepth(index);
                    plan.deadline = Instant::now() + response_timeout;
                    false
                } else {
                    advance_added_subscription(
                        connection,
                        plan,
                        index,
                        canonical,
                        response_timeout,
                    )?
                }
            }
            (
                SubscriptionPhase::SubscribeDepth(index),
                SubscriptionAcknowledgement::DepthByOrder(_),
            ) => {
                let instrument = plan.added.get(index).ok_or_else(malformed)?;
                connection
                    .request_depth_by_order_snapshot(instrument.depth_snapshot_request())
                    .map_err(session_failure)?;
                plan.phase = SubscriptionPhase::SubscribeDepthSnapshot(index);
                plan.deadline = Instant::now() + response_timeout;
                false
            }
            _ => return Err(malformed()),
        }
    };
    if complete {
        finish_subscription(pending, session_generation, emitter)?;
    }
    Ok(())
}

fn begin_added_subscription(
    connection: &mut crate::RithmicTickerConnection,
    added: &[RithmicProviderInstrument],
    index: usize,
    canonical: &mut CanonicalSessionState,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let instrument = added.get(index).ok_or_else(malformed)?;
    canonical.add_instrument(instrument.clone())?;
    connection
        .update_market_data(subscription_request(
            instrument,
            SubscriptionAction::Subscribe,
        ))
        .map_err(session_failure)
}

fn advance_removed_unsubscribe(
    connection: &mut crate::RithmicTickerConnection,
    plan: &mut PendingSubscription,
    index: usize,
    canonical: &mut CanonicalSessionState,
    response_timeout: Duration,
) -> Result<bool, (ProviderInvalidationReason, RetryDisposition)> {
    let removed = plan.removed.get(index).ok_or_else(malformed)?;
    canonical.remove_instrument(&removed.descriptor.instrument_id);
    let next = index + 1;
    if let Some(instrument) = plan.removed.get(next) {
        connection
            .update_market_data(subscription_request(
                instrument,
                SubscriptionAction::Unsubscribe,
            ))
            .map_err(session_failure)?;
        plan.phase = SubscriptionPhase::UnsubscribeMarket(next);
        plan.deadline = Instant::now() + response_timeout;
        return Ok(false);
    }
    if !plan.added.is_empty() {
        begin_added_subscription(connection, &plan.added, 0, canonical)?;
        plan.phase = SubscriptionPhase::SubscribeMarket(0);
        plan.deadline = Instant::now() + response_timeout;
        return Ok(false);
    }
    Ok(true)
}

fn advance_added_subscription(
    connection: &mut crate::RithmicTickerConnection,
    plan: &mut PendingSubscription,
    index: usize,
    canonical: &mut CanonicalSessionState,
    response_timeout: Duration,
) -> Result<bool, (ProviderInvalidationReason, RetryDisposition)> {
    let next = index + 1;
    if plan.added.get(next).is_some() {
        begin_added_subscription(connection, &plan.added, next, canonical)?;
        plan.phase = SubscriptionPhase::SubscribeMarket(next);
        plan.deadline = Instant::now() + response_timeout;
        Ok(false)
    } else {
        Ok(true)
    }
}

fn finish_subscription(
    pending: &mut Option<PendingSubscription>,
    session_generation: SessionGeneration,
    emitter: &SessionEmitter,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    let plan = pending.take().ok_or_else(malformed)?;
    if let Some((selection, instrument)) = plan.selection {
        emit_selection_installed(emitter, session_generation, &selection, instrument)?;
    }
    Ok(())
}

fn emit_selection_installed(
    emitter: &SessionEmitter,
    session_generation: SessionGeneration,
    selection: &RithmicInstrumentSelection,
    instrument: RithmicProviderInstrument,
) -> Result<(), (ProviderInvalidationReason, RetryDisposition)> {
    if emitter.send_catalog(RithmicCatalogEvent::SelectionInstalled {
        session_generation,
        selection_generation: selection.selection_generation,
        instrument: instrument.descriptor,
        entitlement_id: instrument.entitlement_id,
    }) {
        Ok(())
    } else {
        Err((
            ProviderInvalidationReason::QueueOverflow,
            RetryDisposition::Transient,
        ))
    }
}

pub(super) fn selected_instrument(
    selection: &RithmicInstrumentSelection,
    reference: InstrumentReference,
) -> Result<RithmicProviderInstrument, (ProviderInvalidationReason, RetryDisposition)> {
    if reference.symbol != selection.symbol
        || reference.exchange != market_data_exchange(&selection.exchange)
    {
        return Err(malformed());
    }
    let instrument_id = format!(
        "instrument:rithmic:{}:{}",
        reference.exchange, reference.symbol
    );
    let price_scale = reference.price_precision.unwrap_or(0);
    let price_increment = reference
        .minimum_price_change
        .filter(|value| *value > 0.0)
        .and_then(|value| fixed_reference_increment(value, price_scale));
    let descriptor = InstrumentDescriptor {
        instrument_id,
        provider_symbol: reference.symbol.clone(),
        display_symbol: reference.name.unwrap_or(reference.symbol),
        venue_id: reference.exchange,
        price_scale,
        quantity_scale: 0,
        price_increment,
    };
    if descriptor.validate().is_err() {
        return Err(malformed());
    }
    Ok(RithmicProviderInstrument {
        descriptor,
        entitlement_id: selection.entitlement_id.clone(),
        trades: selection.subscription.trades,
        quotes: selection.subscription.quotes,
        order_book: selection.subscription.order_book,
    })
}

/// Converts provider reference-data increments to the exact fixed-point price
/// units used by canonical depth. If the provider value cannot round-trip at
/// its declared precision, omit the presentation grid rather than guessing.
fn fixed_reference_increment(value: f64, scale: u8) -> Option<i64> {
    if !value.is_finite() || value <= 0.0 || scale > 18 {
        return None;
    }
    let rendered = format!("{value:.precision$}", precision = usize::from(scale));
    let round_trip = rendered.parse::<f64>().ok()?;
    let tolerance = f64::EPSILON * value.abs().max(1.0) * 4.0;
    if (round_trip - value).abs() > tolerance {
        return None;
    }
    let increment = rendered.replace('.', "").parse::<i64>().ok()?;
    (increment > 0).then_some(increment)
}

pub(super) fn market_data_exchange(catalog_exchange: &str) -> &str {
    catalog_exchange
        .strip_suffix("-Delayed")
        .unwrap_or(catalog_exchange)
}

fn subscription_request(
    instrument: &RithmicProviderInstrument,
    action: SubscriptionAction,
) -> MarketDataSubscription<'_> {
    MarketDataSubscription {
        symbol: &instrument.descriptor.provider_symbol,
        exchange: &instrument.descriptor.venue_id,
        action,
        trades: instrument.trades,
        quotes: instrument.quotes,
        order_book: instrument.order_book,
    }
}

fn send_rejection(
    emitter: &SessionEmitter,
    session_generation: SessionGeneration,
    command_generation: NonZeroUsize,
    reason: RithmicCatalogRejection,
) -> bool {
    emitter.send_catalog(RithmicCatalogEvent::CommandRejected {
        session_generation: Some(session_generation),
        command_generation,
        reason,
    })
}
