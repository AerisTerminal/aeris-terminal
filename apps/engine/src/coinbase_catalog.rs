//! Engine-owned Coinbase public product catalog.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
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

use crate::rithmic_realtime::{RithmicCatalogControl as Control, RithmicCatalogEvent as Event};

pub(crate) const PROVIDER_GENERATION: u64 = 1;
const COMMAND_CAPACITY: usize = 64;
type CatalogWorker = (SyncSender<Control>, Receiver<Event>, thread::JoinHandle<()>);

pub(crate) fn start(stop: Arc<AtomicBool>) -> Result<CatalogWorker, String> {
    let (control_tx, control_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (event_tx, event_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let worker = thread::Builder::new()
        .name("axiusflow-coinbase-catalog".to_string())
        .spawn(move || run(&control_rx, &event_tx, &stop))
        .map_err(|error| error.to_string())?;
    Ok((control_tx, event_rx, worker))
}

fn run(control_rx: &Receiver<Control>, events: &SyncSender<Event>, stop: &Arc<AtomicBool>) {
    let mut products = None::<Vec<CoinbaseSpotProduct>>;
    let mut searches = BTreeMap::<u64, u64>::new();
    while !stop.load(Ordering::Acquire) {
        let control = match control_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(control) => control,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        if products.is_none() {
            products = load_products(stop);
        }
        match control {
            Control::Search(search) => {
                if searches
                    .get(&search.consumer_id)
                    .is_some_and(|current| *current > search.search_generation)
                {
                    reject(
                        events,
                        search.consumer_id,
                        search.search_generation,
                        ProviderCatalogRejectionReason::SupersededSearch,
                        false,
                    );
                    continue;
                }
                let Some(products) = products.as_ref() else {
                    reject(
                        events,
                        search.consumer_id,
                        search.search_generation,
                        ProviderCatalogRejectionReason::SearchRejected,
                        false,
                    );
                    continue;
                };
                let maximum_results = usize::try_from(search.maximum_results).unwrap_or(usize::MAX);
                let query = search.query.trim().to_ascii_uppercase();
                let instruments = products
                    .iter()
                    .filter(|product| query.is_empty() || product_matches(product, &query))
                    .take(maximum_results)
                    .map(summary)
                    .collect();
                searches.insert(search.consumer_id, search.search_generation);
                let _ = events.send(Event::SearchCompleted(ProviderInstrumentSearchResult {
                    consumer_id: search.consumer_id,
                    provider: "coinbase".to_string(),
                    provider_generation: PROVIDER_GENERATION,
                    search_generation: search.search_generation,
                    instruments,
                }));
            }
            Control::Select(selection) => {
                if searches.get(&selection.consumer_id).copied()
                    != Some(selection.search_generation)
                {
                    reject(
                        events,
                        selection.consumer_id,
                        selection.selection_generation,
                        ProviderCatalogRejectionReason::InstrumentUnavailable,
                        true,
                    );
                    continue;
                }
                let Some(products) = products.as_ref() else {
                    reject(
                        events,
                        selection.consumer_id,
                        selection.selection_generation,
                        ProviderCatalogRejectionReason::InstrumentUnavailable,
                        true,
                    );
                    continue;
                };
                let Some(product) = products.iter().find(|product| {
                    product.product_id == selection.symbol
                        && selection.exchange.eq_ignore_ascii_case("coinbase")
                        && selection.entitlement_id == ENTITLEMENT_CLASS
                }) else {
                    reject(
                        events,
                        selection.consumer_id,
                        selection.selection_generation,
                        ProviderCatalogRejectionReason::InstrumentUnavailable,
                        true,
                    );
                    continue;
                };
                let _ = events.send(Event::SelectionResolved {
                    consumer_id: selection.consumer_id,
                    instrument: install_product(product, selection.selection_generation),
                });
            }
        }
    }
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
    matches!(product.product_id.as_str(), "BTC-USD" | "ETH-USD")
        && product.price_scale == 2
        && product.quantity_scale == 8
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

fn reject(
    events: &SyncSender<Event>,
    consumer_id: u64,
    command_generation: u64,
    reason: ProviderCatalogRejectionReason,
    selection: bool,
) {
    let _ = events.send(Event::Rejected {
        rejection: ProviderCatalogRejected {
            consumer_id,
            provider: "coinbase".to_string(),
            provider_generation: Some(PROVIDER_GENERATION),
            command_generation,
            reason: reason as i32,
        },
        selection,
    });
}

#[cfg(test)]
mod tests {
    use super::{install_product, product_matches, summary, supported_product};
    use axiusflow_coinbase_market_adapter::CoinbaseSpotProduct;
    use axiusflow_coinbase_market_adapter::ENTITLEMENT_CLASS;

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

    #[test]
    fn supported_catalog_products_keep_exact_engine_capabilities() {
        let btc = product("BTC-USD", "instrument:coinbase:btc:usd");
        let sol = product("SOL-USD", "instrument:coinbase:sol:usd");
        let mut changed_precision = btc.clone();
        changed_precision.quantity_scale = 6;

        assert!(supported_product(&btc));
        assert!(!supported_product(&sol));
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
