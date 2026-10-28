//! Pane and screener projection of coordinator-fenced catalog callbacks.

use aeris_contracts::InstallProviderInstrument;
use aeris_market_runtime::MarketRuntimeEvent;
use aeris_market_runtime::{
    ProviderCatalogRejected, ProviderInstrumentSearchResult, ProviderMarketScreen,
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
}
