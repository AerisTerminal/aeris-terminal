#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::{
    collections::BTreeMap,
    num::NonZeroUsize,
    path::PathBuf,
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, TrySendError},
    },
    thread,
};

use axiusflow_application::{ReplayProvenance, ReplaySnapshot, ReplayStreamUpdate};
use axiusflow_coinbase_coordinator::market_worker::{
    ChartState, MarketDataWorker, MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup,
};
use axiusflow_coinbase_coordinator::{rithmic_market_worker, rithmic_series::RithmicSeries};
use axiusflow_coinbase_market_adapter::{
    CoinbaseHttpsHistoryTransport, CoinbaseProductCatalog, CoinbaseSpotProduct,
};
use axiusflow_engine::{
    ENGINE_SOCKET_NAME, EnginePublicationHub, EngineState, bind_listener,
    default_engine_state_root, load_coinbase_catalog, native_installation_token,
    persist_coinbase_catalog, serve_client_with_publications,
};
use axiusflow_local_engine_protocol::{
    CatalogEntry, ChartDelta, ChartProvenance, ChartSnapshot, DomBookState, DomLevel,
    DomRecoveryReason, DomRow as WireDomRow, DomSnapshot, EngineFaultCode, Fault,
    ProviderConnectionState, ProviderState, ResourceMode, RithmicCatalog, RithmicChart,
    RithmicSymbol, ViewKind, envelope, split_catalog,
};
use axiusflow_market_data::{
    ChartAggregation, ChartInterval, DomColumnLevel, OrderBookRecoveryReason, OrderBookState,
};
use axiusflow_market_protocol_adapter::{
    DecimalConvention, encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes,
};
use axiusflow_rithmic_protocol_adapter::{
    InstrumentType, RithmicCatalogEvent, RithmicCatalogRejection, RithmicInstrumentSelection,
    RithmicReadOnlySubscription, RithmicSymbolSearch, SearchPattern,
};
use interprocess::local_socket::traits::Listener as _;

struct ActiveSelection {
    product: CoinbaseSpotProduct,
    interval: ChartInterval,
    market: String,
    interval_seconds: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceTransition {
    KeepRunning,
    Suspend,
    Resume,
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        if argument != "--coinbase-live-smoke" {
            eprintln!(
                "unsupported engine argument: {}",
                argument.to_string_lossy()
            );
            process::exit(2);
        }
        if let Err(error) = run_coinbase_live_smoke_command(arguments) {
            eprintln!("Axiusflow engine live smoke failed: {error}");
            process::exit(1);
        }
        return;
    }
    if let Err(error) = run() {
        eprintln!("Axiusflow engine failed: {error}");
        process::exit(1);
    }
}

fn run_coinbase_live_smoke_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_engine --coinbase-live-smoke <BTC-USD|ETH-USD> <history-root>";
    let product = arguments.next().ok_or_else(|| usage.to_string())?;
    let history_root = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    run_coinbase_live_smoke(
        &product.to_string_lossy(),
        std::path::PathBuf::from(history_root),
    )
}

fn run_coinbase_live_smoke(product_id: &str, history_root: PathBuf) -> Result<(), String> {
    let (startup, mut worker) = MarketDataWorker::start_coinbase(
        product_id.to_string(),
        history_root,
        thread::current().id(),
        false,
        true,
    )?;
    if !matches!(startup, MarketWorkerStartup::Loading(_)) {
        return Err("Coinbase resident worker bypassed the loading state".to_string());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    let mut local_cache_observed = false;
    let mut last_state = None;
    let mut last_state_message = None;
    let mut last_snapshot_provenance = None;
    loop {
        let (messages, disconnected) = worker.drain_messages();
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication) => match publication.update {
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LiveProvider =>
                    {
                        drop(worker);
                        println!(
                            "coinbase_shipping_live_smoke=passed product={product_id} loading=true local_cache_observed={local_cache_observed} covering_snapshot=true clean_shutdown=true"
                        );
                        return Ok(());
                    }
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LocalCache =>
                    {
                        local_cache_observed = true;
                        last_snapshot_provenance = Some(snapshot.provenance());
                    }
                    ReplayStreamUpdate::Snapshot(snapshot) => {
                        last_snapshot_provenance = Some(snapshot.provenance());
                    }
                    ReplayStreamUpdate::Delta(_) => {}
                },
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message,
                } => {
                    return Err(format!(
                        "{message} (previous_state_message={last_state_message:?})"
                    ));
                }
                MarketWorkerMessage::State { state, message } => {
                    last_state = Some(state);
                    last_state_message = Some(message);
                }
                _ => {}
            }
        }
        if disconnected {
            return Err("Coinbase resident worker disconnected before its snapshot".to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Coinbase resident worker timed out before its snapshot (local_cache_observed={local_cache_observed}, last_snapshot_provenance={last_snapshot_provenance:?}, last_state={last_state:?}, last_state_message={last_state_message:?})"
            ));
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn run() -> Result<(), String> {
    const MAXIMUM_CLIENTS: usize = 4;

    let token = Arc::new(native_installation_token()?);
    let listener = bind_listener(ENGINE_SOCKET_NAME).map_err(|error| error.to_string())?;
    register_engine_autostart();
    let mut epoch_bytes = [0_u8; 8];
    getrandom::fill(&mut epoch_bytes).map_err(|error| error.to_string())?;
    let engine_epoch = u64::from_le_bytes(epoch_bytes).max(1);
    let state_root = default_engine_state_root()?;
    let state = EngineState::open(&state_root)?;
    let publications = EnginePublicationHub::default();
    let set_resource_mode = start_market_runtime(
        state.clone(),
        publications.clone(),
        engine_epoch,
        state_root,
    )?;
    let active_clients = Arc::new(AtomicUsize::new(0));
    loop {
        let stream = listener.accept().map_err(|error| error.to_string())?;
        let Ok(active_before) =
            active_clients.fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAXIMUM_CLIENTS).then_some(active + 1)
            })
        else {
            drop(stream);
            continue;
        };
        if active_before == 0 {
            set_resource_mode(ResourceMode::Interactive);
        }
        let token = Arc::clone(&token);
        let state = state.clone();
        let publications = publications.clone();
        let active_clients = Arc::clone(&active_clients);
        let set_resource_mode = Arc::clone(&set_resource_mode);
        thread::Builder::new()
            .name("axiusflow-engine-client".to_string())
            .spawn(move || {
                if let Err(error) = serve_client_with_publications(
                    stream,
                    token.as_slice(),
                    engine_epoch,
                    &state,
                    &publications,
                ) {
                    eprintln!("Axiusflow engine rejected a local client: {error}");
                }
                if active_clients.fetch_sub(1, Ordering::AcqRel) == 1 {
                    set_resource_mode(ResourceMode::Warm);
                }
            })
            .map_err(|error| error.to_string())?;
    }
}

#[cfg(all(target_os = "windows", not(debug_assertions)))]
fn register_engine_autostart() {
    use std::os::windows::process::CommandExt as _;

    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let _ = thread::Builder::new()
        .name("axiusflow-engine-autostart".to_string())
        .spawn(move || {
            let command = format!("\"{}\"", executable.display());
            let mut process = std::process::Command::new("reg.exe");
            process.args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "AxiusflowEngine",
                "/t",
                "REG_SZ",
                "/d",
                &command,
                "/f",
            ]);
            process.creation_flags(0x0800_0000);
            if !matches!(process.status(), Ok(status) if status.success()) {
                eprintln!("Axiusflow engine login startup registration failed");
            }
        });
}

#[cfg(not(all(target_os = "windows", not(debug_assertions))))]
fn register_engine_autostart() {}

fn start_market_runtime(
    state: EngineState,
    publications: EnginePublicationHub,
    engine_epoch: u64,
    state_root: PathBuf,
) -> Result<Arc<dyn Fn(ResourceMode) + Send + Sync>, String> {
    let (selection_tx, selection_rx) = mpsc::channel();
    let (provider_command_tx, provider_command_rx) = mpsc::sync_channel(16);
    let (resource_mode_tx, resource_mode_rx) = mpsc::sync_channel(4);
    let resource_state = state.clone();
    let handle = thread::Builder::new()
        .name("axiusflow-engine-market".to_string())
        .spawn(move || {
            let market_thread = thread::current();
            state.set_selection_callback(Arc::new(move |workspace| {
                if selection_tx.send(workspace).is_ok() {
                    market_thread.unpark();
                }
            }));
            let command_thread = thread::current();
            let command_faults = publications.clone();
            state.set_provider_command_callback(Arc::new(move |command| match provider_command_tx
                .try_send(command)
            {
                Ok(()) => command_thread.unpark(),
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => publish_fault(
                    &command_faults,
                    "Resident provider command queue is unavailable",
                ),
            }));
            run_market_runtime(
                &state,
                &publications,
                engine_epoch,
                &state_root,
                &selection_rx,
                &provider_command_rx,
                &resource_mode_rx,
            );
        })
        .map_err(|error| error.to_string())?;
    let market_thread = handle.thread().clone();
    Ok(Arc::new(move |mode| {
        resource_state.set_resource_mode(mode);
        if resource_mode_tx.try_send(mode).is_ok() {
            market_thread.unpark();
        }
    }))
}

fn run_market_runtime(
    state: &EngineState,
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    state_root: &std::path::Path,
    selections: &Receiver<axiusflow_local_engine_protocol::WorkspaceState>,
    provider_commands: &Receiver<envelope::Payload>,
    resource_modes: &Receiver<ResourceMode>,
) {
    loop {
        if state.workspace().provider == "rithmic" {
            run_rithmic_market_runtime(
                state,
                publications,
                engine_epoch,
                selections,
                provider_commands,
                resource_modes,
            );
        } else {
            run_coinbase_market_runtime(
                state,
                publications,
                engine_epoch,
                state_root,
                selections,
                provider_commands,
                resource_modes,
            );
        }
    }
}

fn run_coinbase_market_runtime(
    state: &EngineState,
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    state_root: &std::path::Path,
    selections: &Receiver<axiusflow_local_engine_protocol::WorkspaceState>,
    provider_commands: &Receiver<envelope::Payload>,
    resource_modes: &Receiver<ResourceMode>,
) {
    let workspace = state.workspace();
    let history_root = default_coinbase_history_root();
    let Some(interval) = interval_from_seconds(workspace.interval_seconds) else {
        publish_fault(publications, "Workspace interval is unsupported");
        return;
    };
    let (products, refresh_catalog) = match bootstrap_catalog(state_root) {
        Ok(products) => products,
        Err(error) => {
            publish_fault(publications, &format!("Catalog bootstrap failed: {error}"));
            return;
        }
    };
    let Some(product) = products
        .iter()
        .find(|product| product.product_id == workspace.market)
        .cloned()
    else {
        publish_fault(
            publications,
            "Workspace market is unavailable in the active catalog",
        );
        return;
    };
    publish_catalog(publications, &products);
    let mut active = ActiveSelection {
        product,
        interval,
        market: workspace.market,
        interval_seconds: workspace.interval_seconds,
    };
    let include_level2 = true;
    let mut worker = match start_resident_worker(
        &active.product,
        active.interval,
        &history_root,
        include_level2,
    ) {
        Ok(worker) => Some(worker),
        Err(error) => {
            publish_fault(
                publications,
                &format!("Market runtime could not start: {error}"),
            );
            return;
        }
    };
    let catalog_refresh = spawn_catalog_refresh(refresh_catalog);
    let Ok(convention) = DecimalConvention::try_new("price_mantissa", "quantity_mantissa") else {
        return;
    };
    let mut chart_context: Option<ReplaySnapshot> = None;
    let mut products = products
        .into_iter()
        .map(|product| (product.product_id.clone(), product))
        .collect::<BTreeMap<_, _>>();
    let mut pending = None;
    loop {
        if let Err(error) = apply_resource_mode(
            resource_modes,
            &mut worker,
            &active.product,
            active.interval,
            &history_root,
            &mut chart_context,
        ) {
            publish_fault(
                publications,
                &format!("Market resource transition failed: {error}"),
            );
            return;
        }
        let Some(worker) = worker.as_mut() else {
            thread::park();
            continue;
        };
        let disconnected = drain_market_messages(
            worker,
            publications,
            engine_epoch,
            &convention,
            &mut chart_context,
            &mut products,
        );
        if apply_selections(selections, worker, &products, &mut active, &mut pending) {
            return;
        }
        reject_non_coinbase_commands(provider_commands, publications);
        apply_catalog_refresh(
            catalog_refresh.as_ref(),
            state_root,
            publications,
            &mut products,
        );
        if disconnected {
            return;
        }
        thread::park();
    }
}

fn drain_market_messages(
    worker: &mut MarketDataWorker,
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    convention: &DecimalConvention,
    chart_context: &mut Option<ReplaySnapshot>,
    products: &mut BTreeMap<String, CoinbaseSpotProduct>,
) -> bool {
    let (messages, disconnected) = worker.drain_messages();
    for message in messages {
        publish_market_message(
            publications,
            engine_epoch,
            convention,
            chart_context,
            products,
            message,
        );
    }
    disconnected
}

fn reject_non_coinbase_commands(
    commands: &Receiver<envelope::Payload>,
    publications: &EnginePublicationHub,
) {
    for command in commands.try_iter() {
        let command_name = match command {
            envelope::Payload::RithmicSearch(_) => "rithmic-search",
            envelope::Payload::RithmicSelect(_) => "rithmic-selection",
            envelope::Payload::RithmicHistory(_) => "rithmic-history",
            _ => "unknown",
        };
        publish_fault(
            publications,
            &format!("Provider command is unavailable for workspace provider {command_name}"),
        );
    }
}

fn run_rithmic_market_runtime(
    state: &EngineState,
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    selections: &Receiver<axiusflow_local_engine_protocol::WorkspaceState>,
    provider_commands: &Receiver<envelope::Payload>,
    resource_modes: &Receiver<ResourceMode>,
) {
    let history_root = default_rithmic_history_root();
    let mut worker = match start_rithmic_worker(&history_root) {
        Ok(worker) => Some(worker),
        Err(error) => {
            publish_fault(
                publications,
                &format!("Rithmic resident runtime could not start: {error}"),
            );
            thread::park();
            return;
        }
    };
    let Ok(convention) = DecimalConvention::try_new("price_mantissa", "quantity_mantissa") else {
        return;
    };
    let mut chart_context = None;
    let mut empty_catalog = BTreeMap::new();
    loop {
        if selections
            .try_iter()
            .last()
            .is_some_and(|selection| selection.provider != "rithmic")
        {
            return;
        }
        if let Some(mode) = resource_modes.try_iter().last() {
            match resource_transition(mode, worker.is_some()) {
                ResourceTransition::KeepRunning => {}
                ResourceTransition::Suspend => {
                    drop(worker.take());
                    chart_context = None;
                }
                ResourceTransition::Resume => match start_rithmic_worker(&history_root) {
                    Ok(resumed) => worker = Some(resumed),
                    Err(error) => {
                        publish_fault(
                            publications,
                            &format!("Rithmic resident runtime could not resume: {error}"),
                        );
                    }
                },
            }
        }
        if let Some(active_worker) = worker.as_mut() {
            for command in provider_commands.try_iter() {
                if let Err(error) = dispatch_rithmic_command(active_worker, command) {
                    publish_fault(publications, &error);
                }
            }
            let (messages, disconnected) = active_worker.drain_messages();
            for message in messages {
                publish_market_message(
                    publications,
                    engine_epoch,
                    &convention,
                    &mut chart_context,
                    &mut empty_catalog,
                    message,
                );
            }
            if disconnected {
                publish_fault(publications, "Rithmic resident runtime disconnected");
                return;
            }
        } else {
            for _ in provider_commands.try_iter() {
                publish_fault(publications, "Rithmic provider is offline-suspended");
            }
        }
        if state.workspace().provider != "rithmic" {
            return;
        }
        thread::park();
    }
}

fn start_rithmic_worker(history_root: &std::path::Path) -> Result<MarketDataWorker, String> {
    let (_startup, worker) = rithmic_market_worker::start(
        history_root.to_path_buf(),
        thread::current().id(),
        false,
        None,
    )?;
    let market_thread = thread::current();
    worker.set_message_wake(Arc::new(move || market_thread.unpark()));
    Ok(worker)
}

fn dispatch_rithmic_command(
    worker: &MarketDataWorker,
    command: envelope::Payload,
) -> Result<(), String> {
    match command {
        envelope::Payload::RithmicSearch(search) => worker
            .try_search_rithmic(parse_rithmic_search(search)?)
            .map_err(|_| "Rithmic search command queue is unavailable".to_string()),
        envelope::Payload::RithmicSelect(selection) => worker
            .try_select_rithmic(parse_rithmic_selection(selection)?)
            .map_err(|_| "Rithmic selection command queue is unavailable".to_string()),
        envelope::Payload::RithmicHistory(request) => worker
            .try_request_rithmic_history(parse_rithmic_history(&request)?)
            .map_err(|_| "Rithmic history command queue is unavailable".to_string()),
        _ => Err("Unsupported resident provider command".to_string()),
    }
}

fn parse_rithmic_search(
    search: axiusflow_local_engine_protocol::RithmicSearch,
) -> Result<RithmicSymbolSearch, String> {
    RithmicSymbolSearch::try_new(
        nonzero_usize(search.generation)?,
        search.query,
        search.exchange,
        search.product_code,
        search
            .instrument_type
            .as_deref()
            .map(parse_instrument_type)
            .transpose()?,
        if search.contains {
            SearchPattern::Contains
        } else {
            SearchPattern::Equals
        },
        NonZeroUsize::new(search.maximum_results as usize)
            .ok_or_else(|| "Rithmic search result bound is invalid".to_string())?,
    )
    .map_err(|_| "Rithmic search command is invalid".to_string())
}

fn parse_rithmic_selection(
    selection: axiusflow_local_engine_protocol::RithmicSelect,
) -> Result<RithmicInstrumentSelection, String> {
    let subscription = RithmicReadOnlySubscription::try_new(
        selection.trades,
        selection.quotes,
        selection.order_book,
    )
    .map_err(|_| "Rithmic subscription is invalid".to_string())?;
    RithmicInstrumentSelection::try_new(
        nonzero_usize(selection.selection_generation)?,
        nonzero_usize(selection.search_generation)?,
        selection.symbol,
        selection.exchange,
        selection.entitlement_id,
        subscription,
    )
    .map_err(|_| "Rithmic selection command is invalid".to_string())
}

fn parse_rithmic_history(
    request: &axiusflow_local_engine_protocol::RithmicHistory,
) -> Result<axiusflow_coinbase_coordinator::rithmic_series::RithmicSeriesRequest, String> {
    let series = RithmicSeries::ALL
        .into_iter()
        .find(|series| series.label() == request.series)
        .ok_or_else(|| "Rithmic chart series is unsupported".to_string())?;
    Ok(
        axiusflow_coinbase_coordinator::rithmic_series::RithmicSeriesRequest {
            selection_generation: nonzero_usize(request.selection_generation)?,
            series_generation: nonzero_usize(request.series_generation)?,
            series,
        },
    )
}

fn nonzero_usize(value: u64) -> Result<NonZeroUsize, String> {
    usize::try_from(value)
        .ok()
        .and_then(NonZeroUsize::new)
        .ok_or_else(|| "Rithmic command generation is invalid".to_string())
}

fn parse_instrument_type(value: &str) -> Result<InstrumentType, String> {
    match value {
        "FUTURE" => Ok(InstrumentType::Future),
        "FUTURE_OPTION" => Ok(InstrumentType::FutureOption),
        "FUTURE_STRATEGY" => Ok(InstrumentType::FutureStrategy),
        "EQUITY" => Ok(InstrumentType::Equity),
        "EQUITY_OPTION" => Ok(InstrumentType::EquityOption),
        "EQUITY_STRATEGY" => Ok(InstrumentType::EquityStrategy),
        "INDEX" => Ok(InstrumentType::Index),
        "INDEX_OPTION" => Ok(InstrumentType::IndexOption),
        "SPREAD" => Ok(InstrumentType::Spread),
        "SYNTHETIC" => Ok(InstrumentType::Synthetic),
        _ => Err("Rithmic instrument type is invalid".to_string()),
    }
}

fn apply_selections(
    selections: &Receiver<axiusflow_local_engine_protocol::WorkspaceState>,
    worker: &MarketDataWorker,
    products: &BTreeMap<String, CoinbaseSpotProduct>,
    active: &mut ActiveSelection,
    pending: &mut Option<axiusflow_local_engine_protocol::WorkspaceState>,
) -> bool {
    for selection in selections.try_iter() {
        if selection.provider != "coinbase" {
            return true;
        }
        *pending = Some(selection);
    }
    let Some(selection) = pending.as_ref() else {
        return false;
    };
    if selection.market == active.market && selection.interval_seconds == active.interval_seconds {
        *pending = None;
        return false;
    }
    let Some(product) = products.get(&selection.market).cloned() else {
        return false;
    };
    let Some(interval) = interval_from_seconds(selection.interval_seconds) else {
        return false;
    };
    if worker
        .try_select_coinbase(product.clone(), interval)
        .is_ok()
    {
        active.product = product;
        active.interval = interval;
        active.market.clone_from(&selection.market);
        active.interval_seconds = selection.interval_seconds;
        *pending = None;
    }
    false
}

fn apply_resource_mode(
    resource_modes: &Receiver<ResourceMode>,
    worker: &mut Option<MarketDataWorker>,
    active_product: &CoinbaseSpotProduct,
    active_interval: ChartInterval,
    history_root: &std::path::Path,
    chart_context: &mut Option<ReplaySnapshot>,
) -> Result<(), String> {
    let Some(mode) = resource_modes.try_iter().last() else {
        return Ok(());
    };
    match resource_transition(mode, worker.is_some()) {
        ResourceTransition::KeepRunning => {}
        ResourceTransition::Suspend => {
            drop(worker.take());
            *chart_context = None;
        }
        ResourceTransition::Resume => {
            *worker = Some(start_resident_worker(
                active_product,
                active_interval,
                history_root,
                true,
            )?);
            *chart_context = None;
        }
    }
    Ok(())
}

const fn resource_transition(mode: ResourceMode, running: bool) -> ResourceTransition {
    match (mode, running) {
        (ResourceMode::OfflineSuspended, true) => ResourceTransition::Suspend,
        (ResourceMode::OfflineSuspended, false) | (_, true) => ResourceTransition::KeepRunning,
        (_, false) => ResourceTransition::Resume,
    }
}

fn start_resident_worker(
    product: &CoinbaseSpotProduct,
    interval: ChartInterval,
    history_root: &std::path::Path,
    include_level2: bool,
) -> Result<MarketDataWorker, String> {
    let (_startup, worker) = MarketDataWorker::start_coinbase_product(
        product.clone(),
        interval,
        history_root.to_path_buf(),
        thread::current().id(),
        false,
        false,
        include_level2,
    )?;
    let market_thread = thread::current();
    worker.set_message_wake(Arc::new(move || market_thread.unpark()));
    Ok(worker)
}

fn bootstrap_catalog(
    state_root: &std::path::Path,
) -> Result<(Vec<CoinbaseSpotProduct>, bool), String> {
    if let Some(products) = load_coinbase_catalog(state_root)? {
        return Ok((products, true));
    }
    let products = fetch_coinbase_catalog()?;
    if let Err(error) = persist_coinbase_catalog(state_root, &products) {
        eprintln!("Axiusflow engine catalog persistence failed: {error}");
    }
    Ok((products, false))
}

fn spawn_catalog_refresh(
    enabled: bool,
) -> Option<Receiver<Result<Vec<CoinbaseSpotProduct>, String>>> {
    enabled.then(|| {
        let (sender, receiver) = mpsc::sync_channel(1);
        let market_thread = thread::current();
        let _ = thread::Builder::new()
            .name("axiusflow-engine-catalog-refresh".to_string())
            .spawn(move || {
                let _ = sender.send(fetch_coinbase_catalog());
                market_thread.unpark();
            });
        receiver
    })
}

fn apply_catalog_refresh(
    refresh: Option<&Receiver<Result<Vec<CoinbaseSpotProduct>, String>>>,
    state_root: &std::path::Path,
    publications: &EnginePublicationHub,
    products: &mut BTreeMap<String, CoinbaseSpotProduct>,
) {
    let Some(refresh) = refresh else {
        return;
    };
    for refreshed in refresh.try_iter() {
        let Ok(refreshed) = refreshed else {
            continue;
        };
        if let Err(error) = persist_coinbase_catalog(state_root, &refreshed) {
            eprintln!("Axiusflow engine catalog persistence failed: {error}");
        }
        *products = refreshed
            .iter()
            .cloned()
            .map(|product| (product.product_id.clone(), product))
            .collect();
        publish_catalog(publications, &refreshed);
    }
}

fn fetch_coinbase_catalog() -> Result<Vec<CoinbaseSpotProduct>, String> {
    let mut catalog = CoinbaseProductCatalog::with_transport(CoinbaseHttpsHistoryTransport::new());
    catalog
        .fetch_active_spot_products()
        .map_err(|error| error.to_string())
}

fn publish_market_message(
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    convention: &DecimalConvention,
    chart_context: &mut Option<ReplaySnapshot>,
    product_catalog: &mut BTreeMap<String, CoinbaseSpotProduct>,
    message: MarketWorkerMessage,
) {
    match message {
        MarketWorkerMessage::Update(publication) => publish_chart_update(
            publications,
            engine_epoch,
            convention,
            chart_context,
            publication,
        ),
        MarketWorkerMessage::CoinbaseCatalog(Ok(products)) => {
            product_catalog.extend(
                products
                    .iter()
                    .cloned()
                    .map(|product| (product.product_id.clone(), product)),
            );
            publish_catalog(publications, &products);
        }
        MarketWorkerMessage::State { state, message } => {
            eprintln!("Axiusflow engine market state {state:?}: {message}");
            let state = match state {
                ChartState::Loading | ChartState::Recovering => ProviderConnectionState::Connecting,
                ChartState::Ready => ProviderConnectionState::Connected,
                ChartState::Stale | ChartState::Error => ProviderConnectionState::Disconnected,
            };
            publications.publish(
                ViewKind::Diagnostics,
                &envelope::Payload::ProviderState(ProviderState {
                    state: state as i32,
                    generation: 0,
                }),
            );
        }
        MarketWorkerMessage::Connection { state, .. } => {
            use axiusflow_observability::FeedConnectionState;
            let state = match state {
                FeedConnectionState::Discovering
                | FeedConnectionState::Authenticating
                | FeedConnectionState::Recovering => ProviderConnectionState::Connecting,
                FeedConnectionState::Streaming => ProviderConnectionState::Connected,
                FeedConnectionState::Disconnected | FeedConnectionState::Stopped => {
                    ProviderConnectionState::Disconnected
                }
            };
            publications.publish(
                ViewKind::Diagnostics,
                &envelope::Payload::ProviderState(ProviderState {
                    state: state as i32,
                    generation: 0,
                }),
            );
        }
        MarketWorkerMessage::CoinbaseDom(frame) => {
            let market = product_catalog
                .values()
                .find(|product| product.instrument_id == frame.instrument_id)
                .map_or_else(
                    || frame.instrument_id.clone(),
                    |product| product.product_id.clone(),
                );
            publish_dom_frame_with_market(publications, engine_epoch, market, frame);
        }
        MarketWorkerMessage::RithmicCatalog(event) => publish_rithmic_catalog(publications, event),
        MarketWorkerMessage::RithmicHistory {
            selection_generation,
            series_generation,
            result,
        } => match result {
            Ok(bootstrap) => publish_rithmic_snapshot(
                publications,
                convention,
                selection_generation,
                series_generation,
                false,
                &bootstrap.snapshot,
            ),
            Err(error) => publish_fault(publications, &error),
        },
        MarketWorkerMessage::RithmicLive {
            selection_generation,
            series_generation,
            snapshot,
        } => publish_rithmic_snapshot(
            publications,
            convention,
            selection_generation,
            series_generation,
            true,
            &snapshot,
        ),
        MarketWorkerMessage::RithmicDom(frame) => {
            let market = frame.instrument_id.clone();
            publish_dom_frame_with_market(publications, engine_epoch, market, frame);
        }
        MarketWorkerMessage::Diagnostics(_)
        | MarketWorkerMessage::Recovery { .. }
        | MarketWorkerMessage::CoinbaseSwitchMarker { .. }
        | MarketWorkerMessage::ChartViewport { .. }
        | MarketWorkerMessage::CoinbaseCatalog(Err(_)) => {}
    }
}

fn publish_dom_frame_with_market(
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    market: String,
    frame: axiusflow_market_data::DomFrame,
) {
    let (state, recovery_reason) = dom_wire_state(frame.state);
    let rows = frame
        .rows
        .into_iter()
        .map(|row| WireDomRow {
            bid: row.bid.map(dom_wire_level),
            ask: row.ask.map(dom_wire_level),
        })
        .collect();
    publications.publish(
        ViewKind::Dom,
        &envelope::Payload::DomSnapshot(DomSnapshot {
            market,
            engine_epoch,
            selection_generation: frame.selection_generation,
            provider_generation: frame.session_generation,
            payload: Vec::new(),
            provider_id: frame.provider_id,
            instrument_id: frame.instrument_id,
            entitlement_id: frame.entitlement_id,
            revision: frame.revision,
            source_watermark: frame.source_watermark,
            state: state as i32,
            recovery_reason,
            rows,
        }),
    );
}

fn publish_rithmic_catalog(publications: &EnginePublicationHub, event: RithmicCatalogEvent) {
    let catalog = match event {
        RithmicCatalogEvent::SearchCompleted {
            session_generation,
            search_generation,
            symbols,
        } => RithmicCatalog {
            kind: 0,
            session_generation: Some(session_generation.get()),
            command_generation: usize_to_u64(search_generation.get()),
            symbols: symbols
                .results
                .into_iter()
                .map(|symbol| RithmicSymbol {
                    symbol: symbol.symbol,
                    exchange: symbol.exchange,
                    name: symbol.name,
                    product_code: symbol.product_code,
                    instrument_type: symbol.instrument_type,
                    expiration_date: symbol.expiration_date,
                })
                .collect(),
            instrument_id: None,
            provider_symbol: None,
            display_symbol: None,
            venue_id: None,
            price_scale: None,
            quantity_scale: None,
            entitlement_id: None,
            rejection: None,
        },
        RithmicCatalogEvent::SelectionInstalled {
            session_generation,
            selection_generation,
            instrument,
            entitlement_id,
        } => RithmicCatalog {
            kind: 1,
            session_generation: Some(session_generation.get()),
            command_generation: usize_to_u64(selection_generation.get()),
            symbols: Vec::new(),
            instrument_id: Some(instrument.instrument_id),
            provider_symbol: Some(instrument.provider_symbol),
            display_symbol: Some(instrument.display_symbol),
            venue_id: Some(instrument.venue_id),
            price_scale: Some(u32::from(instrument.price_scale)),
            quantity_scale: Some(u32::from(instrument.quantity_scale)),
            entitlement_id: Some(entitlement_id),
            rejection: None,
        },
        RithmicCatalogEvent::CommandRejected {
            session_generation,
            command_generation,
            reason,
        } => RithmicCatalog {
            kind: 2,
            session_generation: session_generation
                .map(axiusflow_desktop_provider_runtime::SessionGeneration::get),
            command_generation: usize_to_u64(command_generation.get()),
            symbols: Vec::new(),
            instrument_id: None,
            provider_symbol: None,
            display_symbol: None,
            venue_id: None,
            price_scale: None,
            quantity_scale: None,
            entitlement_id: None,
            rejection: Some(rithmic_rejection_code(reason)),
        },
    };
    publications.publish(
        ViewKind::Catalog,
        &envelope::Payload::RithmicCatalog(catalog),
    );
}

fn publish_rithmic_snapshot(
    publications: &EnginePublicationHub,
    convention: &DecimalConvention,
    selection_generation: NonZeroUsize,
    series_generation: NonZeroUsize,
    live: bool,
    snapshot: &ReplaySnapshot,
) {
    let Ok(envelopes) = try_encode_replay_snapshot_chunk_envelopes(
        "resident_rithmic_market_bars",
        format!("rithmic-{}", series_generation.get()),
        snapshot,
        convention,
        NonZeroUsize::new(256).unwrap_or(NonZeroUsize::MIN),
    ) else {
        return;
    };
    let maximum_frame = NonZeroUsize::new(900 * 1024).unwrap_or(NonZeroUsize::MIN);
    let payloads = envelopes
        .into_iter()
        .filter_map(|encoded| {
            encode_market_bar_stream_frame(&encoded, maximum_frame)
                .ok()
                .map(|payload| {
                    envelope::Payload::RithmicChart(RithmicChart {
                        selection_generation: usize_to_u64(selection_generation.get()),
                        series_generation: usize_to_u64(series_generation.get()),
                        live,
                        payload,
                    })
                })
        })
        .collect::<Vec<_>>();
    if !payloads.is_empty() {
        publications.publish_covering(ViewKind::Chart, &payloads);
    }
}

fn rithmic_rejection_code(reason: RithmicCatalogRejection) -> u32 {
    match reason {
        RithmicCatalogRejection::SearchRejected => 0,
        RithmicCatalogRejection::SupersededSearch => 1,
        RithmicCatalogRejection::InstrumentUnavailable => 2,
        RithmicCatalogRejection::SubscriptionRejected => 3,
        RithmicCatalogRejection::SearchDispatchUnavailable => 4,
        RithmicCatalogRejection::SelectionDispatchUnavailable => 5,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn dom_wire_level(level: DomColumnLevel) -> DomLevel {
    DomLevel {
        price: level.price,
        quantity: level.quantity,
        order_count: level.order_count,
        price_text: level.price_text,
        quantity_text: level.quantity_text,
        relative_size_bps: u32::from(level.relative_size_bps),
    }
}

fn dom_wire_state(state: OrderBookState) -> (DomBookState, Option<i32>) {
    match state {
        OrderBookState::Ready => (DomBookState::Ready, None),
        OrderBookState::Stale => (DomBookState::Stale, None),
        OrderBookState::Recovering(reason) => (
            DomBookState::Recovering,
            Some(match reason {
                OrderBookRecoveryReason::AwaitingSnapshot => DomRecoveryReason::AwaitingSnapshot,
                OrderBookRecoveryReason::SequenceGap => DomRecoveryReason::SequenceGap,
                OrderBookRecoveryReason::CrossedBook => DomRecoveryReason::CrossedBook,
                OrderBookRecoveryReason::InvalidUpdate => DomRecoveryReason::InvalidUpdate,
            } as i32),
        ),
    }
}

fn publish_catalog(publications: &EnginePublicationHub, products: &[CoinbaseSpotProduct]) {
    let entries = products
        .iter()
        .map(|product| CatalogEntry {
            product_id: product.product_id.clone(),
            base_currency: product.base_currency.clone(),
            quote_currency: product.quote_currency.clone(),
            price_scale: u32::from(product.price_scale),
            quantity_scale: u32::from(product.quantity_scale),
        })
        .collect();
    let payloads: Vec<envelope::Payload> = split_catalog(entries, 1)
        .into_iter()
        .map(envelope::Payload::CatalogSnapshot)
        .collect();
    publications.publish_covering(ViewKind::Catalog, &payloads);
}

fn publish_fault(publications: &EnginePublicationHub, detail: &str) {
    eprintln!("Axiusflow engine market fault: {detail}");
    publications.publish(
        ViewKind::Diagnostics,
        &envelope::Payload::Fault(Fault {
            code: EngineFaultCode::Retryable as i32,
            redacted_detail: detail.to_string(),
        }),
    );
}

fn interval_from_seconds(seconds: u32) -> Option<ChartInterval> {
    ChartInterval::ALL.iter().copied().find(|interval| {
        matches!(
            interval.aggregation(),
            ChartAggregation::FixedSeconds(interval_seconds) if interval_seconds.get() == seconds
        ) || (*interval == ChartInterval::Month1 && seconds == 30 * 24 * 60 * 60)
    })
}

fn publish_chart_update(
    publications: &EnginePublicationHub,
    engine_epoch: u64,
    convention: &DecimalConvention,
    chart_context: &mut Option<ReplaySnapshot>,
    publication: MarketWorkerPublication,
) {
    const MAXIMUM_INNER_FRAME_BYTES: usize = 900 * 1024;
    const SNAPSHOT_CHUNK_ITEMS: usize = 256;

    let maximum_frame = NonZeroUsize::new(MAXIMUM_INNER_FRAME_BYTES).unwrap_or(NonZeroUsize::MIN);
    let provenance = match &publication.update {
        ReplayStreamUpdate::Snapshot(snapshot) => snapshot.provenance(),
        ReplayStreamUpdate::Delta(_) => chart_context
            .as_ref()
            .map_or(ReplayProvenance::LiveProvider, ReplaySnapshot::provenance),
    };
    let wire_provenance = chart_provenance(provenance);
    match publication.update {
        ReplayStreamUpdate::Snapshot(snapshot) => {
            let Ok(envelopes) = try_encode_replay_snapshot_chunk_envelopes(
                publication.subscription_id,
                format!(
                    "engine-{engine_epoch}-{}",
                    publication.generation.generation()
                ),
                &snapshot,
                convention,
                NonZeroUsize::new(SNAPSHOT_CHUNK_ITEMS).unwrap_or(NonZeroUsize::MIN),
            ) else {
                return;
            };
            let payloads = envelopes
                .into_iter()
                .filter_map(|envelope| {
                    encode_market_bar_stream_frame(&envelope, maximum_frame)
                        .ok()
                        .map(|payload| {
                            envelope::Payload::ChartSnapshot(ChartSnapshot {
                                market: snapshot.instrument().instrument_id.as_str().to_string(),
                                interval_seconds: snapshot.bar_definition().interval_seconds,
                                engine_epoch,
                                selection_generation: 0,
                                provider_generation: publication.generation.generation(),
                                payload,
                                provenance: wire_provenance as i32,
                            })
                        })
                })
                .collect::<Vec<_>>();
            if !payloads.is_empty() {
                publications.publish_covering(ViewKind::Chart, &payloads);
                *chart_context = Some(snapshot);
            }
        }
        ReplayStreamUpdate::Delta(delta) => {
            let Some(snapshot) = chart_context.as_ref() else {
                return;
            };
            let Ok(envelope) = try_encode_replay_delta_envelope(
                publication.subscription_id,
                snapshot.instrument(),
                snapshot.bar_definition(),
                &delta,
                convention,
            ) else {
                return;
            };
            let Ok(payload) = encode_market_bar_stream_frame(&envelope, maximum_frame) else {
                return;
            };
            publications.publish_transient(
                ViewKind::Chart,
                &envelope::Payload::ChartDelta(ChartDelta {
                    market: snapshot.instrument().instrument_id.as_str().to_string(),
                    interval_seconds: snapshot.bar_definition().interval_seconds,
                    engine_epoch,
                    selection_generation: 0,
                    provider_generation: publication.generation.generation(),
                    payload,
                    provenance: wire_provenance as i32,
                }),
            );
        }
    }
}

const fn chart_provenance(provenance: ReplayProvenance) -> ChartProvenance {
    match provenance {
        ReplayProvenance::LocalCache => ChartProvenance::LocalCache,
        ReplayProvenance::LiveProvider => ChartProvenance::LiveProvider,
        ReplayProvenance::EmbeddedFixture => ChartProvenance::EmbeddedFixture,
    }
}

fn default_coinbase_history_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("local-data").join("coinbase-history"),
        |root| {
            PathBuf::from(root)
                .join("Axiusflow")
                .join("market-history")
                .join("coinbase")
        },
    )
}

fn default_rithmic_history_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("local-data").join("rithmic-history"),
        |root| {
            PathBuf::from(root)
                .join("Axiusflow")
                .join("market-history")
                .join("rithmic")
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveSelection, ResourceTransition, apply_selections, resource_transition,
        run_coinbase_live_smoke_command,
    };
    use axiusflow_coinbase_coordinator::market_worker::{
        ChartViewportUpdate, MarketDataWorker, MarketWorkerCommand, market_worker_channel,
    };
    use axiusflow_coinbase_market_adapter::CoinbaseSpotProduct;
    use axiusflow_local_engine_protocol::{ResourceMode, WorkspaceState};
    use axiusflow_market_data::ChartInterval;
    use std::{
        collections::BTreeMap,
        num::NonZeroUsize,
        sync::{Arc, atomic::AtomicU64, mpsc},
        thread,
    };

    #[test]
    fn warm_and_interactive_transitions_keep_the_provider_connection_alive() {
        assert_eq!(
            resource_transition(ResourceMode::Warm, true),
            ResourceTransition::KeepRunning
        );
        assert_eq!(
            resource_transition(ResourceMode::Interactive, true),
            ResourceTransition::KeepRunning
        );
        assert_eq!(
            resource_transition(ResourceMode::Constrained, true),
            ResourceTransition::KeepRunning
        );
    }

    #[test]
    fn offline_suspension_is_idempotent_and_resumes_only_when_requested() {
        assert_eq!(
            resource_transition(ResourceMode::OfflineSuspended, true),
            ResourceTransition::Suspend
        );
        assert_eq!(
            resource_transition(ResourceMode::OfflineSuspended, false),
            ResourceTransition::KeepRunning
        );
        assert_eq!(
            resource_transition(ResourceMode::Warm, false),
            ResourceTransition::Resume
        );
    }

    #[test]
    fn resident_live_smoke_command_rejects_incomplete_arguments_before_startup() {
        assert!(run_coinbase_live_smoke_command(std::iter::empty()).is_err());
        assert!(
            run_coinbase_live_smoke_command(
                ["BTC-USD", "history", "extra"]
                    .map(std::ffi::OsString::from)
                    .into_iter()
            )
            .is_err()
        );
    }

    #[test]
    fn full_worker_mailbox_retains_the_latest_selection_for_retry() {
        let btc = spot_product("BTC-USD");
        let eth = spot_product("ETH-USD");
        let mut products = BTreeMap::new();
        products.insert(btc.product_id.clone(), btc.clone());
        products.insert(eth.product_id.clone(), eth.clone());
        let mut active = ActiveSelection {
            product: btc,
            interval: ChartInterval::Minute1,
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
        };
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        command_tx
            .send(MarketWorkerCommand::ChartViewport(ChartViewportUpdate {
                start_unix_nanos: 1,
                end_unix_nanos: 2,
                selection_generation: 1,
            }))
            .expect("fills worker mailbox");
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let worker = MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            None,
            Some(Arc::new(AtomicU64::new(0))),
        );
        let (selection_tx, selection_rx) = mpsc::channel();
        selection_tx
            .send(workspace("ETH-USD"))
            .expect("selection enters engine queue");
        let mut pending = None;

        assert!(!apply_selections(
            &selection_rx,
            &worker,
            &products,
            &mut active,
            &mut pending,
        ));
        assert_eq!(
            pending.as_ref().map(|state| state.market.as_str()),
            Some("ETH-USD")
        );
        assert!(matches!(
            command_rx.recv().expect("prefilled command remains"),
            MarketWorkerCommand::ChartViewport(_)
        ));

        assert!(!apply_selections(
            &selection_rx,
            &worker,
            &products,
            &mut active,
            &mut pending,
        ));
        assert!(pending.is_none());
        assert_eq!(active.market, "ETH-USD");
        let MarketWorkerCommand::CoinbaseSelect(request) =
            command_rx.recv().expect("selection retries")
        else {
            panic!("retry must preserve the selection command");
        };
        assert_eq!(request.product.product_id, "ETH-USD");

        let shutdown = thread::spawn(move || {
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::Shutdown)
            ));
            shutdown_tx.send(()).expect("acknowledges shutdown");
        });
        drop(worker);
        shutdown.join().expect("shutdown thread completes");
    }

    fn spot_product(product_id: &str) -> CoinbaseSpotProduct {
        CoinbaseSpotProduct {
            product_id: product_id.to_string(),
            instrument_id: format!("instrument:coinbase:{}", product_id.to_ascii_lowercase()),
            display_symbol: product_id.replace('-', "/"),
            base_currency: product_id
                .split_once('-')
                .map_or(product_id, |(base, _)| base)
                .to_string(),
            quote_currency: "USD".to_string(),
            price_scale: 2,
            quantity_scale: 8,
        }
    }

    fn workspace(market: &str) -> WorkspaceState {
        WorkspaceState {
            provider: "coinbase".to_string(),
            market: market.to_string(),
            interval_seconds: 60,
            watchlist: Vec::new(),
            workspace_revision: 1,
            warm_mode_enabled: true,
            resource_mode: ResourceMode::Interactive as i32,
            schema_revision: 1,
            cache_manifest_revision: 1,
            hot_series: Vec::new(),
        }
    }
}
