//! Pane and screener projection of coordinator-fenced catalog callbacks.

use aeris_contracts::InstallProviderInstrument;
use aeris_market_runtime::MarketRuntimeEvent;
use aeris_market_runtime::{
    ProviderCatalogRejected, ProviderInstrumentSearchResult, ProviderMarketScan,
    ProviderMarketScreen,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProviderCatalogCommand {
    Search,
    Selection,
}

pub(super) enum ProviderCatalogEvent {
    CatalogReady {
        provider: String,
    },
    SearchCompleted(ProviderInstrumentSearchResult),
    SelectionInstalled {
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    CommandRejected {
        rejection: ProviderCatalogRejected,
        command: ProviderCatalogCommand,
    },
    ScreenCompleted(ProviderMarketScreen),
    ScreenRejected(ProviderCatalogRejected),
    /// Progress or the final answer of the consumer's latest scan pass.
    ScanUpdated(ProviderMarketScan),
    ScanRejected(ProviderCatalogRejected),
}

pub(super) fn project_provider_catalog_event(
    event: MarketRuntimeEvent,
) -> Option<ProviderCatalogEvent> {
    match event {
        MarketRuntimeEvent::CatalogReady { provider } => {
            Some(ProviderCatalogEvent::CatalogReady { provider })
        }
        // The runtime fences consumer ownership, provider and callback generation
        // before delivery. Each view applies its outstanding request generation.
        MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
            Some(ProviderCatalogEvent::SearchCompleted(result))
        }
        MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
            Some(ProviderCatalogEvent::SelectionInstalled {
                command_generation: selection.command_generation,
                instrument: selection.instrument,
            })
        }
        MarketRuntimeEvent::ProviderCatalogRejected {
            rejection,
            selection,
        } => Some(ProviderCatalogEvent::CommandRejected {
            rejection,
            command: if selection {
                ProviderCatalogCommand::Selection
            } else {
                ProviderCatalogCommand::Search
            },
        }),
        MarketRuntimeEvent::ProviderMarketScreen(screen) => {
            Some(ProviderCatalogEvent::ScreenCompleted(screen))
        }
        MarketRuntimeEvent::ProviderMarketScreenRejected(rejection) => {
            Some(ProviderCatalogEvent::ScreenRejected(rejection))
        }
        MarketRuntimeEvent::ProviderMarketScan(scan) => {
            Some(ProviderCatalogEvent::ScanUpdated(scan))
        }
        MarketRuntimeEvent::ProviderMarketScanRejected(rejection) => {
            Some(ProviderCatalogEvent::ScanRejected(rejection))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_market_runtime::ProviderCatalogRejectionReason;

    #[test]
    fn screen_callbacks_project_with_their_request_identity() {
        let screen = ProviderMarketScreen {
            consumer_id: 41,
            provider: "fixture".into(),
            provider_generation: 2,
            screen_generation: 7,
            captured_at_unix_millis: 100,
            markets: Vec::new(),
        };
        assert!(matches!(
            project_provider_catalog_event(MarketRuntimeEvent::ProviderMarketScreen(screen)),
            Some(ProviderCatalogEvent::ScreenCompleted(screen))
                if screen.consumer_id == 41 && screen.screen_generation == 7
        ));
        let rejection = ProviderCatalogRejected {
            consumer_id: 41,
            provider: "fixture".into(),
            provider_generation: Some(2),
            command_generation: 8,
            reason: ProviderCatalogRejectionReason::SearchTimedOut,
        };
        assert!(matches!(
            project_provider_catalog_event(MarketRuntimeEvent::ProviderMarketScreenRejected(
                rejection
            )),
            Some(ProviderCatalogEvent::ScreenRejected(rejection))
                if rejection.consumer_id == 41 && rejection.command_generation == 8
        ));
    }

    #[test]
    fn scan_callbacks_project_with_their_request_identity() {
        let scan = ProviderMarketScan {
            consumer_id: 41,
            provider: "fixture".into(),
            scan_generation: 9,
            complete: true,
            ..ProviderMarketScan::default()
        };
        assert!(matches!(
            project_provider_catalog_event(MarketRuntimeEvent::ProviderMarketScan(scan)),
            Some(ProviderCatalogEvent::ScanUpdated(scan))
                if scan.consumer_id == 41 && scan.scan_generation == 9 && scan.complete
        ));
        let rejection = ProviderCatalogRejected {
            consumer_id: 41,
            provider: "fixture".into(),
            provider_generation: Some(2),
            command_generation: 10,
            reason: ProviderCatalogRejectionReason::DispatchUnavailable,
        };
        assert!(matches!(
            project_provider_catalog_event(MarketRuntimeEvent::ProviderMarketScanRejected(
                rejection
            )),
            Some(ProviderCatalogEvent::ScanRejected(rejection))
                if rejection.command_generation == 10
        ));
    }
}
