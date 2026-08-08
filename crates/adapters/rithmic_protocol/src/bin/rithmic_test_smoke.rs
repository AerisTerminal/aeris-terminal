use axiusflow_desktop_provider_runtime::{
    AuthenticationState, ProviderInvalidationReason, ProviderSessionDriver, ProviderSessionEvent,
    SessionGeneration,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_rithmic_protocol_adapter::{
    CollectionProgress, DecodedCatalogMessage, DecodedControlMessage, DecodedMarketMessage,
    DecodedTimeBarType, HistoryBars, HistoryCollectionRequest, HistoryCollector, HistorySeries,
    InstrumentReferenceRequest, MarketDataSubscription, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicApplication, RithmicAuthorizedSilenceEvidenceFault,
    RithmicCallbackLimits, RithmicCredentialBytes, RithmicProviderConfig, RithmicProviderDriver,
    RithmicProviderEvents, RithmicSessionLimits, RithmicSessionMessage, RithmicTestSession,
    SearchPattern, SubscriptionAction, SymbolSearchCollectionRequest, SymbolSearchCollector,
    SymbolSearchRequest, TickBarReplayRequest, TimeBarReplayRequest, TimeBarType,
    collect_rithmic_covering_recovery_evidence,
};
use std::{
    io::{self, Write},
    num::{NonZeroU64, NonZeroUsize},
    sync::mpsc::{self, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

const SYMBOL: &str = "MNQ";
const AUTHORIZED_SILENCE_RUN_TIMEOUT: Duration = Duration::from_mins(2);
const HISTORY_LOOKBACK_MINUTES: i32 = 4 * 24 * 60 + 300;
const MAXIMUM_HISTORY_BARS: usize = 6_063;

fn main() -> Result<(), String> {
    let authorized_silence_recovery = parse_authorized_silence_recovery(std::env::args().skip(1))?;
    let credentials = load_credentials()?;
    if authorized_silence_recovery {
        run_authorized_silence_recovery(&credentials)?;
        return Ok(());
    }
    let application = application();
    let (selected, subscription_rejected) = run_ticker(&credentials, application)?;
    run_history(&credentials, application, &selected)?;
    if subscription_rejected {
        return Err("rithmic_test_smoke=failed live_stream=provider_rejected".to_string());
    }
    let (reconnected, reconnect_rejected) = run_ticker(&credentials, application)?;
    if reconnect_rejected
        || reconnected.symbol != selected.symbol
        || reconnected.exchange != selected.exchange
    {
        return Err("rithmic_test_smoke=failed reconnect=identity_or_subscription".to_string());
    }
    println!("rithmic_reconnect=passed");
    println!(
        "rithmic_test_smoke=passed symbol={} exchange={}",
        selected.symbol, selected.exchange
    );
    Ok(())
}

fn parse_authorized_silence_recovery(
    arguments: impl IntoIterator<Item = String>,
) -> Result<bool, String> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(false),
        [argument] if argument == "--authorized-silence-recovery" => Ok(true),
        _ => Err(
            "usage: rithmic_test_smoke [--authorized-silence-recovery] (credentials are loaded only from the native vault)"
                .to_string(),
        ),
    }
}

#[derive(Clone)]
struct SelectedInstrument {
    symbol: String,
    exchange: String,
}

const fn application() -> RithmicApplication<'static> {
    RithmicApplication {
        name: "Axiusflow",
        version: env!("CARGO_PKG_VERSION"),
    }
}

fn run_ticker(
    credentials: &RithmicCredentialBytes,
    application: RithmicApplication<'_>,
) -> Result<(SelectedInstrument, bool), String> {
    let borrowed = credentials
        .credentials()
        .map_err(|_| "credentials_invalid")?;
    let mut ticker = RithmicTestSession::discover_and_login(
        borrowed,
        application,
        RithmicSessionLimits::default(),
        None,
    )
    .map_err(|error| format!("ticker_login_failed={error}"))?;
    println!("rithmic_ticker_login=passed");
    let selected = search_and_reference(&mut ticker)?;
    let rejected = test_subscription(&mut ticker, &selected)?;
    test_heartbeat(&mut ticker)?;
    ticker
        .close()
        .map_err(|error| format!("ticker_close_failed={error}"))?;
    println!("rithmic_ticker_close=passed");
    Ok((selected, rejected))
}

fn test_heartbeat(
    ticker: &mut axiusflow_rithmic_protocol_adapter::RithmicTickerConnection,
) -> Result<(), String> {
    ticker
        .send_heartbeat()
        .map_err(|error| format!("heartbeat_send_failed={error}"))?;
    loop {
        if let RithmicSessionMessage::Control(DecodedControlMessage::Heartbeat {
            accepted, ..
        }) = ticker
            .read_next()
            .map_err(|error| format!("heartbeat_read_failed={error}"))?
        {
            if !accepted {
                return Err("heartbeat_rejected".to_string());
            }
            println!("rithmic_heartbeat=passed");
            return Ok(());
        }
    }
}

fn search_and_reference(
    ticker: &mut axiusflow_rithmic_protocol_adapter::RithmicTickerConnection,
) -> Result<SelectedInstrument, String> {
    ticker
        .search_symbols(SymbolSearchRequest {
            search_text: SYMBOL,
            exchange: None,
            product_code: None,
            instrument_type: None,
            pattern: SearchPattern::Equals,
        })
        .map_err(|error| format!("symbol_search_send_failed={error}"))?;
    let mut collector = SymbolSearchCollector::try_new(SymbolSearchCollectionRequest {
        exchange: None,
        product_code: None,
        instrument_type: None,
        maximum_results: NonZeroUsize::new(128).ok_or("search_limit_invalid")?,
    })
    .map_err(|error| error.to_string())?;
    let results = loop {
        if let RithmicSessionMessage::Catalog(message) = ticker
            .read_next()
            .map_err(|error| format!("symbol_search_read_failed={error}"))?
        {
            match collector
                .accept(message)
                .map_err(|error| error.to_string())?
            {
                CollectionProgress::Pending | CollectionProgress::Unhandled(_) => {}
                CollectionProgress::Complete(results) => break results,
            }
        }
    };
    let selected = results
        .results
        .iter()
        .filter(|result| {
            result.expiration_date.is_some()
                && result.symbol != SYMBOL
                && !result.symbol.contains('-')
        })
        .min_by_key(|result| result.expiration_date.as_deref())
        .ok_or("symbol_search_empty")?;
    let selected = SelectedInstrument {
        symbol: selected.symbol.clone(),
        exchange: selected
            .exchange
            .strip_suffix("-Delayed")
            .unwrap_or(&selected.exchange)
            .to_string(),
    };
    println!(
        "rithmic_symbol_search=passed results={}",
        results.results.len()
    );

    ticker
        .request_instrument_reference(InstrumentReferenceRequest {
            symbol: &selected.symbol,
            exchange: &selected.exchange,
        })
        .map_err(|error| format!("reference_send_failed={error}"))?;
    loop {
        match ticker
            .read_next()
            .map_err(|error| format!("reference_read_failed={error}"))?
        {
            RithmicSessionMessage::Catalog(DecodedCatalogMessage::InstrumentReference(Some(
                reference,
            ))) if reference.symbol == selected.symbol
                && reference.exchange == selected.exchange =>
            {
                break;
            }
            _ => {}
        }
    }
    println!("rithmic_instrument_reference=passed");
    Ok(selected)
}

fn test_subscription(
    ticker: &mut axiusflow_rithmic_protocol_adapter::RithmicTickerConnection,
    selected: &SelectedInstrument,
) -> Result<bool, String> {
    ticker
        .update_market_data(MarketDataSubscription {
            symbol: &selected.symbol,
            exchange: &selected.exchange,
            action: SubscriptionAction::Subscribe,
            trades: true,
            quotes: true,
            order_book: true,
        })
        .map_err(|error| format!("subscription_send_failed={error}"))?;
    let mut subscription_rejected = false;
    let mut trade_observed = false;
    let mut quote_observed = false;
    let mut depth_observed = false;
    while !(subscription_rejected || trade_observed && quote_observed && depth_observed) {
        match ticker
            .read_next()
            .map_err(|error| format!("stream_read_failed={error}"))?
        {
            RithmicSessionMessage::Control(DecodedControlMessage::MarketDataSubscription {
                accepted,
            }) => {
                if !accepted {
                    subscription_rejected = true;
                }
            }
            RithmicSessionMessage::Market(DecodedMarketMessage::Trade(_)) => {
                trade_observed = true;
            }
            RithmicSessionMessage::Market(DecodedMarketMessage::Quote(_)) => {
                quote_observed = true;
            }
            RithmicSessionMessage::Market(DecodedMarketMessage::OrderBook(_)) => {
                depth_observed = true;
            }
            _ => {}
        }
    }
    if subscription_rejected {
        println!("rithmic_live_stream=rejected_by_provider");
    } else {
        println!(
            "rithmic_live_stream=passed trades={trade_observed} quotes={quote_observed} depth={depth_observed}"
        );
    }
    Ok(subscription_rejected)
}

fn run_history(
    credentials: &RithmicCredentialBytes,
    application: RithmicApplication<'_>,
    selected: &SelectedInstrument,
) -> Result<(), String> {
    let borrowed = credentials
        .credentials()
        .map_err(|_| "credentials_invalid")?;
    let mut history = RithmicTestSession::discover_and_login_history(
        borrowed,
        application,
        RithmicSessionLimits::default(),
        None,
    )
    .map_err(|error| format!("history_login_failed={error}"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock_invalid")?
        .as_secs();
    let finish = i32::try_from(now - (now % 60)).map_err(|_| "time_overflow")?;
    let start = finish
        .checked_sub(60 * HISTORY_LOOKBACK_MINUTES)
        .ok_or("time_underflow")?;
    history
        .replay_time_bars(TimeBarReplayRequest {
            symbol: &selected.symbol,
            exchange: &selected.exchange,
            bar_type: TimeBarType::Minute,
            period: 1,
            start_seconds: start,
            finish_seconds: finish,
            maximum_bars: u16::try_from(MAXIMUM_HISTORY_BARS)
                .map_err(|_| "history_limit_invalid")?,
        })
        .map_err(|error| format!("history_send_failed={error}"))?;
    let mut collector = HistoryCollector::try_new(HistoryCollectionRequest {
        symbol: selected.symbol.clone(),
        exchange: selected.exchange.clone(),
        series: HistorySeries::Time {
            bar_type: DecodedTimeBarType::Minute,
            period: 1,
        },
        start_seconds: start,
        finish_seconds: finish,
        maximum_bars: NonZeroUsize::new(MAXIMUM_HISTORY_BARS).ok_or("history_limit_invalid")?,
    })
    .map_err(|error| error.to_string())?;
    let collected = loop {
        if let RithmicSessionMessage::History(message) = history
            .read_next()
            .map_err(|error| format!("history_read_failed={error}"))?
        {
            match collector
                .accept(message)
                .map_err(|error| error.to_string())?
            {
                CollectionProgress::Pending | CollectionProgress::Unhandled(_) => {}
                CollectionProgress::Complete(result) => break result,
            }
        }
    };
    let bar_count = match collected.bars {
        HistoryBars::Time(bars) => bars.len(),
        HistoryBars::Tick(_) => 0,
    };
    if bar_count == 0 {
        return Err("history_empty".to_string());
    }
    if bar_count > MAXIMUM_HISTORY_BARS {
        return Err("history_bar_bound_exceeded".to_string());
    }
    println!("rithmic_history=passed bars={bar_count}");
    run_tick_history(&mut history, selected, start, finish)?;
    history
        .close()
        .map_err(|error| format!("history_close_failed={error}"))?;
    Ok(())
}

fn run_tick_history(
    history: &mut axiusflow_rithmic_protocol_adapter::RithmicHistoryConnection,
    selected: &SelectedInstrument,
    start: i32,
    finish: i32,
) -> Result<(), String> {
    const TRADES_PER_BAR: u16 = 100;
    history
        .replay_tick_bars(TickBarReplayRequest {
            symbol: &selected.symbol,
            exchange: &selected.exchange,
            trades_per_bar: TRADES_PER_BAR,
            start_seconds: start,
            finish_seconds: finish,
            maximum_bars: 300,
        })
        .map_err(|error| format!("tick_history_send_failed={error}"))?;
    let mut collector = HistoryCollector::try_new(HistoryCollectionRequest {
        symbol: selected.symbol.clone(),
        exchange: selected.exchange.clone(),
        series: HistorySeries::Tick {
            trades_per_bar: TRADES_PER_BAR,
        },
        start_seconds: start,
        finish_seconds: finish,
        maximum_bars: NonZeroUsize::new(300).ok_or("tick_history_limit_invalid")?,
    })
    .map_err(|error| error.to_string())?;
    let collected = loop {
        if let RithmicSessionMessage::History(message) = history
            .read_next()
            .map_err(|error| format!("tick_history_read_failed={error}"))?
        {
            match collector
                .accept(message)
                .map_err(|error| error.to_string())?
            {
                CollectionProgress::Pending | CollectionProgress::Unhandled(_) => {}
                CollectionProgress::Complete(result) => break result,
            }
        }
    };
    let bars = match collected.bars {
        HistoryBars::Tick(bars) => bars,
        HistoryBars::Time(_) => return Err("tick_history_wrong_series".to_string()),
    };
    if bars.is_empty() {
        return Err("tick_history_empty".to_string());
    }
    println!("rithmic_tick_history=passed bars={}", bars.len());
    Ok(())
}

fn run_authorized_silence_recovery(credentials: &RithmicCredentialBytes) -> Result<(), String> {
    let credentials = RithmicCredentialBytes::try_copy_from_vault(credentials.as_bytes())
        .map_err(|_| "credentials_invalid")?;
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    phase("watchdog_started");
    let handle = thread::Builder::new()
        .name("rithmic-authorized-silence-evidence".to_string())
        .spawn(move || {
            let result = run_authorized_silence_recovery_inner(&credentials);
            let _ = result_tx.send(result);
        })
        .map_err(|_| "silence_evidence_thread_unavailable")?;
    match result_rx.recv_timeout(AUTHORIZED_SILENCE_RUN_TIMEOUT) {
        Ok(result) => {
            join_evidence_thread(handle)?;
            result
        }
        Err(RecvTimeoutError::Timeout) => {
            phase("whole_run_deadline_expired");
            Err("authorized_silence_recovery_deadline")
        }
        Err(RecvTimeoutError::Disconnected) => {
            join_evidence_thread(handle)?;
            Err("authorized_silence_recovery_thread_failed")
        }
    }
    .map_err(str::to_string)
}

fn join_evidence_thread(handle: JoinHandle<()>) -> Result<(), &'static str> {
    handle
        .join()
        .map_err(|_| "authorized_silence_recovery_thread_panicked")
}

fn run_authorized_silence_recovery_inner(
    credentials: &RithmicCredentialBytes,
) -> Result<(), &'static str> {
    let generation =
        |value| SessionGeneration::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN));
    let limits = RithmicSessionLimits {
        response_timeout: Duration::from_secs(5),
        ..RithmicSessionLimits::default()
    };
    let config = RithmicProviderConfig::try_new(
        "Axiusflow",
        env!("CARGO_PKG_VERSION"),
        limits,
        Duration::from_secs(10),
        Vec::new(),
    )
    .map_err(|_| "silence_evidence_config_invalid")?
    .with_authorized_silence_evidence_fault(
        generation(1),
        RithmicAuthorizedSilenceEvidenceFault::Message,
    )
    .with_authorized_silence_evidence_fault(
        generation(2),
        RithmicAuthorizedSilenceEvidenceFault::Heartbeat,
    );
    let callback_limits = RithmicCallbackLimits::try_new(
        NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(1_048_576).unwrap_or(NonZeroUsize::MIN),
        NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN),
    )
    .map_err(|_| "silence_evidence_callback_limits_invalid")?;
    let (mut driver, events) = RithmicProviderDriver::new(config, callback_limits);

    for (current, expected) in [
        (generation(1), ProviderInvalidationReason::MessageSilence),
        (generation(2), ProviderInvalidationReason::HeartbeatSilence),
    ] {
        phase(match expected {
            ProviderInvalidationReason::MessageSilence => "message_silence_login_started",
            ProviderInvalidationReason::HeartbeatSilence => "heartbeat_silence_login_started",
            _ => "unexpected_silence_phase",
        });
        driver
            .start_session(current, credentials.as_bytes())
            .map_err(|_| "silence_evidence_start_failed")?;
        wait_for_silence_invalidation(&events, current, expected)?;
        phase(match expected {
            ProviderInvalidationReason::MessageSilence => "message_silence_invalidated",
            ProviderInvalidationReason::HeartbeatSilence => "heartbeat_silence_invalidated",
            _ => "unexpected_silence_phase",
        });
        driver
            .stop_session(current)
            .map_err(|_| "silence_evidence_stop_failed")?;
        println!(
            "rithmic_client_local_inbound_suppression=passed generation={} invalidation={expected:?}",
            current.get()
        );
    }

    let recovered = generation(3);
    phase("fresh_generation_login_started");
    driver
        .start_session(recovered, credentials.as_bytes())
        .map_err(|_| "silence_reconnect_start_failed")?;
    wait_for_established(&events, recovered)?;
    phase("fresh_generation_established");
    driver
        .stop_session(recovered)
        .map_err(|_| "silence_reconnect_stop_failed")?;
    phase("fresh_generation_stopped");
    verify_authorized_clean_close(credentials)?;
    phase("covering_recovery_started");
    let covering =
        collect_rithmic_covering_recovery_evidence().map_err(|_| "covering_recovery_failed")?;
    println!(
        "rithmic_silence_recovery=passed authorized_client_local_silence=true fresh_generation={} covering_recovery=deterministic_production_state_machine covering_generation={} covering_watermark={} confirmed_driver_stop=true",
        recovered.get(),
        covering.recovery_generation,
        covering.recovered_watermark
    );
    phase("complete");
    Ok(())
}

fn verify_authorized_clean_close(credentials: &RithmicCredentialBytes) -> Result<(), &'static str> {
    phase("clean_close_login_started");
    let borrowed = credentials
        .credentials()
        .map_err(|_| "credentials_invalid")?;
    let ticker = RithmicTestSession::discover_and_login(
        borrowed,
        application(),
        RithmicSessionLimits::default(),
        None,
    )
    .map_err(|_| "silence_close_login_failed")?;
    ticker.close().map_err(|_| "silence_close_failed")?;
    phase("clean_close_confirmed");
    println!("rithmic_silence_clean_close=passed");
    Ok(())
}

fn wait_for_silence_invalidation(
    events: &RithmicProviderEvents,
    generation: SessionGeneration,
    expected: ProviderInvalidationReason,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut authenticated = false;
    let mut instruments_discovered = false;
    loop {
        if Instant::now() >= deadline {
            return Err("silence_invalidation_timeout");
        }
        if let Some(callback) = events.try_recv() {
            match callback.event {
                ProviderSessionEvent::AuthenticationChanged {
                    generation: callback_generation,
                    state: AuthenticationState::Accepted,
                } if callback_generation == generation => authenticated = true,
                ProviderSessionEvent::InstrumentsDiscovered {
                    generation: callback_generation,
                    ..
                } if callback_generation == generation => instruments_discovered = true,
                ProviderSessionEvent::Invalidated {
                    generation: Some(callback_generation),
                    reason,
                } if callback_generation == generation => {
                    if authenticated
                        && instruments_discovered
                        && reason == expected
                        && callback.retry
                            == Some(axiusflow_rithmic_protocol_adapter::RetryDisposition::Transient)
                    {
                        return Ok(());
                    }
                    return Err("silence_invalidation_mismatch");
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn wait_for_established(
    events: &RithmicProviderEvents,
    generation: SessionGeneration,
) -> Result<(), &'static str> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut authenticated = false;
    let mut instruments_discovered = false;
    while !(authenticated && instruments_discovered) {
        if Instant::now() >= deadline {
            return Err("silence_reconnect_timeout");
        }
        if let Some(callback) = events.try_recv() {
            match callback.event {
                ProviderSessionEvent::AuthenticationChanged {
                    generation: callback_generation,
                    state: AuthenticationState::Accepted,
                } if callback_generation == generation => authenticated = true,
                ProviderSessionEvent::InstrumentsDiscovered {
                    generation: callback_generation,
                    ..
                } if callback_generation == generation => instruments_discovered = true,
                ProviderSessionEvent::Invalidated { reason, .. } => {
                    let _ = reason;
                    return Err("silence_reconnect_invalidated");
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(5));
        }
    }
    Ok(())
}

fn phase(name: &str) {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "rithmic_authorized_silence_phase={name}");
    let _ = stderr.flush();
}

fn load_credentials() -> Result<RithmicCredentialBytes, String> {
    let vault =
        NativeCredentialVault::new(RITHMIC_TEST_VAULT_SERVICE).map_err(|_| "vault_unavailable")?;
    let mut stored = vault
        .load(RITHMIC_TEST_VAULT_KEY)
        .map_err(|_| "vault_load_failed")?
        .ok_or("credentials_missing")?;
    let copied =
        RithmicCredentialBytes::try_copy_from_vault(&stored).map_err(|_| "credentials_invalid");
    stored.zeroize();
    Ok(copied?)
}

#[cfg(test)]
mod tests {
    use super::parse_authorized_silence_recovery;

    #[test]
    fn authorized_silence_recovery_requires_one_exact_nonsecret_flag() {
        assert_eq!(parse_authorized_silence_recovery(Vec::new()), Ok(false));
        assert_eq!(
            parse_authorized_silence_recovery(vec!["--authorized-silence-recovery".to_string()]),
            Ok(true)
        );
        for rejected in [
            vec!["--unknown".to_string()],
            vec![
                "--authorized-silence-recovery".to_string(),
                "extra".to_string(),
            ],
        ] {
            assert!(parse_authorized_silence_recovery(rejected).is_err());
        }
    }
}
