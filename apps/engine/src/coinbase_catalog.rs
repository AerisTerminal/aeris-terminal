//! Engine-owned Coinbase public product catalog.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use axiusflow_coinbase_market_adapter::{
    CoinbaseHttpsHistoryTransport, CoinbaseProductCatalog, CoinbaseSpotProduct, ENTITLEMENT_CLASS,
};
use axiusflow_engine_protocol::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};

pub(crate) const PROVIDER_GENERATION: u64 = 1;
const COMMAND_CAPACITY: usize = 64;
type CatalogWorker = (
    CoinbaseCatalogControl,
    Receiver<Event>,
    thread::JoinHandle<()>,
);

pub(crate) enum CoinbaseCatalogEvent {
    SearchCompleted {
        result: ProviderInstrumentSearchResult,
        authorization_generation: u64,
    },
    SelectionResolved {
        consumer_id: u64,
        instrument: InstallProviderInstrument,
        authorization_generation: u64,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
        authorization_generation: u64,
    },
}

type Event = CoinbaseCatalogEvent;

#[derive(Clone)]
pub(crate) struct CoinbaseCatalogControl {
    commands: SyncSender<CoinbaseCatalogCommand>,
    state: Arc<Mutex<CatalogState>>,
}

enum CoinbaseCatalogCommand {
    Search {
        request: axiusflow_engine_protocol::SearchProviderInstruments,
        authorization_generation: u64,
    },
    Select {
        request: axiusflow_engine_protocol::SelectProviderInstrument,
        authorization_generation: u64,
    },
}

#[derive(Default)]
struct CatalogState {
    next_generation: u64,
    active: BTreeMap<u64, u64>,
    searches: BTreeMap<u64, CompletedSearch>,
}

#[derive(Clone, Copy)]
pub(crate) enum CoinbaseCatalogDispatchError {
    Unauthorized,
    Full,
    Disconnected,
}

struct CompletedSearch {
    generation: u64,
    authorization_generation: u64,
    products: Vec<CoinbaseSpotProduct>,
}

impl CoinbaseCatalogControl {
    pub(crate) fn authorize_consumer(&self, consumer_id: u64) -> Result<(), String> {
        let mut authorizations = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        authorizations.next_generation = authorizations
            .next_generation
            .checked_add(1)
            .ok_or_else(|| "Coinbase catalog authorization space is exhausted".to_string())?;
        let generation = authorizations.next_generation;
        authorizations.active.insert(consumer_id, generation);
        Ok(())
    }

    pub(crate) fn release_consumer(&self, consumer_id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.remove(&consumer_id);
        state.searches.remove(&consumer_id);
    }

    pub(crate) fn is_authorized(&self, consumer_id: u64, generation: u64) -> bool {
        is_authorized(
            &self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            consumer_id,
            generation,
        )
    }

    pub(crate) fn try_search(
        &self,
        request: axiusflow_engine_protocol::SearchProviderInstruments,
    ) -> Result<(), CoinbaseCatalogDispatchError> {
        let authorization_generation = self.authorization_generation(request.consumer_id)?;
        self.commands
            .try_send(CoinbaseCatalogCommand::Search {
                request,
                authorization_generation,
            })
            .map_err(|error| dispatch_error(&error))
    }

    pub(crate) fn try_select(
        &self,
        request: axiusflow_engine_protocol::SelectProviderInstrument,
    ) -> Result<(), CoinbaseCatalogDispatchError> {
        let authorization_generation = self.authorization_generation(request.consumer_id)?;
        self.commands
            .try_send(CoinbaseCatalogCommand::Select {
                request,
                authorization_generation,
            })
            .map_err(|error| dispatch_error(&error))
    }

    fn authorization_generation(
        &self,
        consumer_id: u64,
    ) -> Result<u64, CoinbaseCatalogDispatchError> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .get(&consumer_id)
            .copied()
            .ok_or(CoinbaseCatalogDispatchError::Unauthorized)
    }

    #[cfg(test)]
    pub(crate) fn test_control() -> Self {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        thread::spawn(move || while receiver.recv().is_ok() {});
        Self {
            commands,
            state: Arc::new(Mutex::new(CatalogState::default())),
        }
    }

    #[cfg(test)]
    pub(crate) fn is_consumer_authorized(&self, consumer_id: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .contains_key(&consumer_id)
    }
}

fn dispatch_error(
    error: &mpsc::TrySendError<CoinbaseCatalogCommand>,
) -> CoinbaseCatalogDispatchError {
    match error {
        mpsc::TrySendError::Full(_) => CoinbaseCatalogDispatchError::Full,
        mpsc::TrySendError::Disconnected(_) => CoinbaseCatalogDispatchError::Disconnected,
    }
}

pub(crate) fn start(stop: Arc<AtomicBool>) -> Result<CatalogWorker, String> {
    let (control_tx, control_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (event_tx, event_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let state = Arc::new(Mutex::new(CatalogState::default()));
    let worker_state = Arc::clone(&state);
    let worker = thread::Builder::new()
        .name("axiusflow-coinbase-catalog".to_string())
        .spawn(move || run(&control_rx, &event_tx, &worker_state, &stop))
        .map_err(|error| error.to_string())?;
    Ok((
        CoinbaseCatalogControl {
            commands: control_tx,
            state,
        },
        event_rx,
        worker,
    ))
}

fn run(
    control_rx: &Receiver<CoinbaseCatalogCommand>,
    events: &SyncSender<Event>,
    state: &Mutex<CatalogState>,
    stop: &Arc<AtomicBool>,
) {
    let mut products = None::<Vec<CoinbaseSpotProduct>>;
    while !stop.load(Ordering::Acquire) {
        let control = match control_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(control) => control,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if products.is_none() {
            products = load_products(stop);
        }
        if events
            .send(handle_control(products.as_deref(), state, control))
            .is_err()
        {
            return;
        }
    }
}

fn handle_control(
    products: Option<&[CoinbaseSpotProduct]>,
    state: &Mutex<CatalogState>,
    control: CoinbaseCatalogCommand,
) -> Event {
    match control {
        CoinbaseCatalogCommand::Search {
            request: search,
            authorization_generation,
        } => handle_search(products, state, &search, authorization_generation),
        CoinbaseCatalogCommand::Select {
            request: selection,
            authorization_generation,
        } => handle_select(state, &selection, authorization_generation),
    }
}

fn handle_search(
    products: Option<&[CoinbaseSpotProduct]>,
    state: &Mutex<CatalogState>,
    search: &axiusflow_engine_protocol::SearchProviderInstruments,
    authorization_generation: u64,
) -> Event {
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !is_authorized(&state, search.consumer_id, authorization_generation) {
        return rejection(
            search.consumer_id,
            search.search_generation,
            ProviderCatalogRejectionReason::SearchRejected,
            false,
            authorization_generation,
        );
    }
    if state
        .searches
        .get(&search.consumer_id)
        .is_some_and(|current| current.generation > search.search_generation)
    {
        return rejection(
            search.consumer_id,
            search.search_generation,
            ProviderCatalogRejectionReason::SupersededSearch,
            false,
            authorization_generation,
        );
    }
    let Some(products) = products else {
        return rejection(
            search.consumer_id,
            search.search_generation,
            ProviderCatalogRejectionReason::SearchRejected,
            false,
            authorization_generation,
        );
    };
    let maximum_results = usize::try_from(search.maximum_results).unwrap_or(usize::MAX);
    let query = search.query.trim().to_ascii_uppercase();
    let products = products
        .iter()
        .filter(|product| query.is_empty() || product_matches(product, &query))
        .take(maximum_results)
        .cloned()
        .collect::<Vec<_>>();
    let instruments = products.iter().map(summary).collect();
    state.searches.insert(
        search.consumer_id,
        CompletedSearch {
            generation: search.search_generation,
            authorization_generation,
            products,
        },
    );
    Event::SearchCompleted {
        result: ProviderInstrumentSearchResult {
            consumer_id: search.consumer_id,
            provider: "coinbase".to_string(),
            provider_generation: PROVIDER_GENERATION,
            search_generation: search.search_generation,
            instruments,
        },
        authorization_generation,
    }
}

fn handle_select(
    state: &Mutex<CatalogState>,
    selection: &axiusflow_engine_protocol::SelectProviderInstrument,
    authorization_generation: u64,
) -> Event {
    let mut state = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let instrument = state
        .searches
        .get(&selection.consumer_id)
        .filter(|search| {
            search.generation == selection.search_generation
                && search.authorization_generation == authorization_generation
                && is_authorized(&state, selection.consumer_id, authorization_generation)
        })
        .and_then(|search| {
            search.products.iter().find(|product| {
                selection.provider == "coinbase"
                    && product.product_id == selection.symbol
                    && selection.exchange.eq_ignore_ascii_case("coinbase")
                    && selection.entitlement_id == ENTITLEMENT_CLASS
            })
        })
        .map(|product| install_product(product, selection.selection_generation));
    let Some(instrument) = instrument else {
        return rejection(
            selection.consumer_id,
            selection.selection_generation,
            ProviderCatalogRejectionReason::InstrumentUnavailable,
            true,
            authorization_generation,
        );
    };
    state.searches.remove(&selection.consumer_id);
    Event::SelectionResolved {
        consumer_id: selection.consumer_id,
        instrument,
        authorization_generation,
    }
}

fn is_authorized(state: &CatalogState, consumer_id: u64, generation: u64) -> bool {
    state
        .active
        .get(&consumer_id)
        .is_some_and(|active| *active == generation)
}

fn load_products(stop: &Arc<AtomicBool>) -> Option<Vec<CoinbaseSpotProduct>> {
    let mut catalog = CoinbaseProductCatalog::with_transport_and_stop(
        CoinbaseHttpsHistoryTransport::with_stop(Arc::clone(stop)),
        Arc::clone(stop),
    );
    catalog
        .fetch_active_spot_products()
        .ok()
        .map(|products| products.into_iter().filter(supported_product).collect())
}

fn product_matches(product: &CoinbaseSpotProduct, query: &str) -> bool {
    product.product_id.contains(query)
        || product.display_symbol.to_ascii_uppercase().contains(query)
        || product.base_currency.contains(query)
        || product.quote_currency.contains(query)
}

fn supported_product(product: &CoinbaseSpotProduct) -> bool {
    product.price_scale <= 18 && product.quantity_scale <= 18
}

fn summary(product: &CoinbaseSpotProduct) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        symbol: product.product_id.clone(),
        exchange: "coinbase".to_string(),
        name: Some(product.display_symbol.clone()),
        product_code: Some(product.product_id.clone()),
        instrument_type: Some("spot".to_string()),
        expiration_date: None,
    }
}

fn install_product(
    product: &CoinbaseSpotProduct,
    selection_generation: u64,
) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "coinbase".to_string(),
        session_generation: PROVIDER_GENERATION,
        selection_generation,
        instrument_id: product.instrument_id.clone(),
        provider_symbol: product.product_id.clone(),
        display_symbol: product.display_symbol.clone(),
        venue_id: "coinbase".to_string(),
        price_scale: u32::from(product.price_scale),
        quantity_scale: u32::from(product.quantity_scale),
        entitlement_id: ENTITLEMENT_CLASS.to_string(),
    }
}

fn rejection(
    consumer_id: u64,
    command_generation: u64,
    reason: ProviderCatalogRejectionReason,
    selection: bool,
    authorization_generation: u64,
) -> Event {
    Event::Rejected {
        rejection: ProviderCatalogRejected {
            consumer_id,
            provider: "coinbase".to_string(),
            provider_generation: Some(PROVIDER_GENERATION),
            command_generation,
            reason: reason as i32,
        },
        selection,
        authorization_generation,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, mpsc};

    use super::{
        CatalogState, CoinbaseCatalogCommand as Control, CoinbaseCatalogControl, Event,
        handle_control, install_product, product_matches, summary, supported_product,
    };
    use axiusflow_coinbase_market_adapter::{CoinbaseSpotProduct, ENTITLEMENT_CLASS};
    use axiusflow_engine_protocol::{
        ProviderCatalogRejectionReason, SearchProviderInstruments, SelectProviderInstrument,
    };

    fn product(product_id: &str, instrument_id: &str) -> CoinbaseSpotProduct {
        CoinbaseSpotProduct {
            product_id: product_id.to_string(),
            instrument_id: instrument_id.to_string(),
            display_symbol: product_id.replace('-', "/"),
            base_currency: product_id
                .split_once('-')
                .map_or("", |(base, _)| base)
                .to_string(),
            quote_currency: "USD".to_string(),
            price_scale: 2,
            quantity_scale: 8,
        }
    }

    fn search(
        state: &Mutex<CatalogState>,
        consumer_id: u64,
        generation: u64,
        query: &str,
        maximum_results: u32,
    ) -> Control {
        let authorization_generation = authorize(state, consumer_id);
        Control::Search {
            request: SearchProviderInstruments {
                consumer_id,
                search_generation: generation,
                provider: "coinbase".to_string(),
                query: query.to_string(),
                maximum_results,
            },
            authorization_generation,
        }
    }

    fn selection(
        state: &Mutex<CatalogState>,
        consumer_id: u64,
        search_generation: u64,
        symbol: &str,
    ) -> Control {
        let authorization_generation = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active[&consumer_id];
        Control::Select {
            request: SelectProviderInstrument {
                consumer_id,
                selection_generation: search_generation + 10,
                search_generation,
                provider: "coinbase".to_string(),
                symbol: symbol.to_string(),
                exchange: "coinbase".to_string(),
                entitlement_id: ENTITLEMENT_CLASS.to_string(),
            },
            authorization_generation,
        }
    }

    fn authorize(state: &Mutex<CatalogState>, consumer_id: u64) -> u64 {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(generation) = state.active.get(&consumer_id) {
            return *generation;
        }
        state.next_generation += 1;
        let generation = state.next_generation;
        state.active.insert(consumer_id, generation);
        generation
    }

    fn products() -> Vec<CoinbaseSpotProduct> {
        vec![
            product("BTC-USD", "instrument:coinbase:btc:usd"),
            product("ETH-USD", "instrument:coinbase:eth:usd"),
            product("SOL-USD", "instrument:coinbase:sol:usd"),
        ]
    }

    #[test]
    fn empty_query_returns_only_the_requested_bound() {
        let products = products();
        let state = Mutex::new(CatalogState::default());

        let Event::SearchCompleted { result, .. } =
            handle_control(Some(&products), &state, search(&state, 41, 1, "  ", 2))
        else {
            panic!("empty Coinbase search completes");
        };

        assert_eq!(result.search_generation, 1);
        assert_eq!(result.instruments.len(), 2);
        assert_eq!(result.instruments[0].symbol, "BTC-USD");
        assert_eq!(result.instruments[1].symbol, "ETH-USD");
    }

    #[test]
    fn typed_query_retains_only_matching_results() {
        let products = products();
        let state = Mutex::new(CatalogState::default());

        let Event::SearchCompleted { result, .. } =
            handle_control(Some(&products), &state, search(&state, 42, 2, "eth", 3))
        else {
            panic!("typed Coinbase search completes");
        };

        assert_eq!(result.instruments.len(), 1);
        assert_eq!(result.instruments[0].symbol, "ETH-USD");
    }

    #[test]
    fn every_product_in_a_full_bounded_result_can_be_selected() {
        let products = products();
        for (generation, symbol) in [(1, "BTC-USD"), (2, "ETH-USD"), (3, "SOL-USD")] {
            let state = Mutex::new(CatalogState::default());
            let _ = handle_control(
                Some(&products),
                &state,
                search(&state, 43, generation, "USD", 3),
            );

            assert!(matches!(
                handle_control(
                    Some(&products),
                    &state,
                    selection(&state, 43, generation, symbol),
                ),
                Event::SelectionResolved { instrument, .. }
                    if instrument.provider_symbol == symbol
            ));
            assert!(
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .searches
                    .is_empty(),
                "completed selection releases search identity"
            );
        }
    }

    #[test]
    fn product_outside_the_latest_bounded_result_is_rejected() {
        let products = products();
        let state = Mutex::new(CatalogState::default());
        let _ = handle_control(Some(&products), &state, search(&state, 44, 4, "USD", 2));

        assert!(matches!(
            handle_control(
                Some(&products),
                &state,
                selection(&state, 44, 4, "SOL-USD"),
            ),
            Event::Rejected {
                rejection,
                selection: true,
                ..
            } if rejection.reason == ProviderCatalogRejectionReason::InstrumentUnavailable as i32
        ));
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .searches
                .get(&44)
                .map(|search| search.generation),
            Some(4)
        );
    }

    #[test]
    fn released_consumer_invalidates_and_removes_completed_search() {
        let products = products();
        let control = CoinbaseCatalogControl::test_control();
        let state = std::sync::Arc::clone(&control.state);
        let _ = handle_control(Some(&products), &state, search(&state, 45, 1, "BTC", 1));
        assert_eq!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .searches
                .len(),
            1
        );

        control.release_consumer(45);

        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.active.is_empty());
        assert!(state.searches.is_empty());
    }

    #[test]
    fn cleanup_bypasses_a_saturated_catalog_command_channel() {
        let products = products();
        let state = std::sync::Arc::new(Mutex::new(CatalogState::default()));
        let (commands, _receiver) = mpsc::sync_channel(1);
        let control = CoinbaseCatalogControl {
            commands,
            state: std::sync::Arc::clone(&state),
        };
        let _ = handle_control(Some(&products), &state, search(&state, 46, 1, "BTC", 1));
        control
            .commands
            .try_send(search(&state, 46, 2, "ETH", 1))
            .expect("bounded command lane fills");
        assert!(matches!(
            control.commands.try_send(search(&state, 46, 3, "SOL", 1)),
            Err(mpsc::TrySendError::Full(_))
        ));

        control.release_consumer(46);

        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.active.is_empty());
        assert!(state.searches.is_empty());
    }

    #[test]
    fn sequential_abandoned_consumers_keep_completed_search_state_bounded() {
        let products = products();
        let control = CoinbaseCatalogControl::test_control();
        let state = std::sync::Arc::clone(&control.state);
        let mut maximum_retained = 0;

        for consumer_id in 1..=1_024 {
            let _ = handle_control(
                Some(&products),
                &state,
                search(&state, consumer_id, 1, "BTC", 1),
            );
            maximum_retained = maximum_retained.max(
                state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .searches
                    .len(),
            );
            control.release_consumer(consumer_id);
            let retained = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .searches
                .len();
            assert_eq!(retained, 0);
        }

        assert_eq!(maximum_retained, 1);
        assert!(
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .searches
                .is_empty()
        );
    }

    #[test]
    fn supported_catalog_products_keep_exact_engine_capabilities() {
        let btc = product("BTC-USD", "instrument:coinbase:btc:usd");
        let sol = product("SOL-USD", "instrument:coinbase:sol:usd");
        let mut changed_precision = btc.clone();
        changed_precision.quantity_scale = 19;

        assert!(supported_product(&btc));
        assert!(supported_product(&sol));
        assert!(!supported_product(&changed_precision));
        assert!(product_matches(&btc, "BTC"));
        assert!(!product_matches(&btc, "ETH"));
    }

    #[test]
    fn selected_product_preserves_catalog_identity_and_precision() {
        let btc = product("BTC-USD", "instrument:coinbase:btc:usd");
        let summary = summary(&btc);
        let installed = install_product(&btc, 7);

        assert_eq!(summary.symbol, "BTC-USD");
        assert_eq!(summary.exchange, "coinbase");
        assert_eq!(installed.selection_generation, 7);
        assert_eq!(installed.instrument_id, btc.instrument_id);
        assert_eq!(installed.price_scale, 2);
        assert_eq!(installed.quantity_scale, 8);
        assert_eq!(installed.entitlement_id, ENTITLEMENT_CLASS);
    }
}
