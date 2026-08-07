use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use axiusflow_rithmic_protocol_adapter::{
    CollectionProgress, DecodedCatalogMessage, DecodedControlMessage, DecodedTimeBarType,
    HistoryBars, HistoryCollectionRequest, HistoryCollector, HistorySeries,
    InstrumentReferenceRequest, MarketDataSubscription, RITHMIC_TEST_VAULT_KEY,
    RITHMIC_TEST_VAULT_SERVICE, RithmicApplication, RithmicCredentialBytes, RithmicSessionLimits,
    RithmicSessionMessage, RithmicTestSession, SearchPattern, SubscriptionAction,
    SymbolSearchCollectionRequest, SymbolSearchCollector, SymbolSearchRequest,
    TimeBarReplayRequest, TimeBarType,
};
use std::{
    num::NonZeroUsize,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroize;

const SYMBOL: &str = "MNQ";

fn main() -> Result<(), String> {
    let credentials = load_credentials()?;
    let application = application();
    let (selected, subscription_rejected) = run_ticker(&credentials, application)?;
    run_history(&credentials, application, &selected)?;
    if subscription_rejected {
        return Err("rithmic_test_smoke=failed live_stream=provider_rejected".to_string());
    }
    println!(
        "rithmic_test_smoke=passed symbol={} exchange={}",
        selected.symbol, selected.exchange
    );
    Ok(())
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
    ticker
        .close()
        .map_err(|error| format!("ticker_close_failed={error}"))?;
    Ok((selected, rejected))
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
            quotes: false,
            order_book: false,
        })
        .map_err(|error| format!("subscription_send_failed={error}"))?;
    let mut subscription_rejected = false;
    let mut market_messages = 0usize;
    while market_messages < 1 && !subscription_rejected {
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
            RithmicSessionMessage::Market(_) => market_messages += 1,
            _ => {}
        }
    }
    if subscription_rejected {
        println!("rithmic_live_stream=rejected_by_provider");
    } else {
        println!("rithmic_live_stream=passed messages={market_messages}");
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
    let start = finish.checked_sub(60 * 300).ok_or("time_underflow")?;
    history
        .replay_time_bars(TimeBarReplayRequest {
            symbol: &selected.symbol,
            exchange: &selected.exchange,
            bar_type: TimeBarType::Minute,
            period: 1,
            start_seconds: start,
            finish_seconds: finish,
            maximum_bars: 300,
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
        maximum_bars: NonZeroUsize::new(300).ok_or("history_limit_invalid")?,
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
    println!("rithmic_history=passed bars={bar_count}");
    history
        .close()
        .map_err(|error| format!("history_close_failed={error}"))?;
    Ok(())
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
