//! Catalog-shaped instrument fixtures used only by desktop tests.

use aeris_contracts::InstallProviderInstrument;

#[must_use]
pub fn fixture_futures_contract(symbol: &str) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "rithmic".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: format!("instrument:rithmic:CME:{symbol}"),
        provider_symbol: symbol.to_string(),
        display_symbol: symbol.to_string(),
        venue_id: "CME".to_string(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: format!("rithmic-test:CME:{symbol}"),
        price_increment: None,
        ..Default::default()
    }
}

#[must_use]
pub fn fixture_public_crypto() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "hyperliquid".to_string(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        provider_symbol: "BTC".to_string(),
        display_symbol: "BTC-USDC".to_string(),
        venue_id: "Hyperliquid".to_string(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: "hyperliquid-public".to_string(),
        price_increment: None,
        ..Default::default()
    }
}

pub fn fixture_futures_contracts() -> Vec<InstallProviderInstrument> {
    ["MNQ", "ES"]
        .into_iter()
        .map(fixture_futures_contract)
        .collect()
}
