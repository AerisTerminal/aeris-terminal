//! Provider-neutral presentation metadata published by `market_runtime`.

/// How a provider session is established from the desktop's perspective.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderConnectionKind {
    /// The provider uses credentials held by the native vault.
    Credentials,
    /// The provider is available without account credentials.
    Public,
}

/// Which persisted instrument identity is accepted by a provider catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCatalogSymbol {
    ProviderSymbol,
    DisplaySymbol,
}

/// How a provider's one-day chart bars are bucketed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderDailyBarCadence {
    /// Fixed 24-hour buckets.
    FixedDay,
    /// One trading session per bar; the provider serves no fixed 24-hour daily series.
    TradingSession,
}

/// How deep a provider's live market feed goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFeedLevel {
    /// Bars only, without a live quote.
    Bars,
    /// Level 1: the best bid and offer.
    TopOfBook,
    /// Level 2: aggregated size at each price level.
    PriceLevels,
    /// Level 3: every resting order at each price level.
    OrderLevels,
}

impl ProviderFeedLevel {
    /// Whether the feed carries a book beyond the best bid and offer.
    #[must_use]
    pub const fn depth_available(self) -> bool {
        matches!(self, Self::PriceLevels | Self::OrderLevels)
    }

    /// Whether the feed carries a live best bid and offer.
    #[must_use]
    pub const fn quotes_available(self) -> bool {
        !matches!(self, Self::Bars)
    }

    /// The conventional feed-level name, or `None` for a bars-only feed.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Bars => None,
            Self::TopOfBook => Some("L1 data"),
            Self::PriceLevels => Some("L2 data"),
            Self::OrderLevels => Some("L3 data"),
        }
    }
}

/// Licensing artwork and About notices supplied by a provider, not inferred from its ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderAttribution {
    pub market_data_mark_key: &'static str,
    pub powered_by_mark_key: &'static str,
    /// Lines may contain `{year}`, replaced with the current UTC year at display time.
    pub notices: &'static [&'static str],
    pub powered_by_notice: &'static str,
}

/// Stable provider metadata for catalog and chart presentation.// Each flag is an independent, static provider capability, not encoded state.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderPresentationDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub chart_interval_labels: &'static [&'static str],
    pub daily_bars: ProviderDailyBarCadence,
    pub search_hint: &'static str,
    pub logo_key: &'static str,
    /// Trading venue served by this provider's broker relay, if any.
    pub broker_venue_id: Option<&'static str>,
    pub attribution: Option<ProviderAttribution>,
    pub catalog_symbol: ProviderCatalogSymbol,
    pub selection_entitlement_id: &'static str,
    pub catalog_refresh_on_startup: bool,
    pub ready_label_suffix: &'static str,
    pub feed_level: ProviderFeedLevel,
    /// Whether search honours `InstrumentSearchCategories` (it serves more than one category).
    pub search_categories_available: bool,
    /// Whether the provider answers `ScreenProviderMarkets` with listed-market statistics.
    pub market_screen_available: bool,
    /// Whether the provider streams individual trade prints (time and sales).
    pub trades_available: bool,
    pub connection_kind: ProviderConnectionKind,
}

#[cfg(test)]
mod tests {
    use super::ProviderFeedLevel;

    #[test]
    fn feed_levels_name_their_depth() {
        assert_eq!(ProviderFeedLevel::Bars.label(), None);
        assert_eq!(ProviderFeedLevel::TopOfBook.label(), Some("L1 data"));
        assert_eq!(ProviderFeedLevel::PriceLevels.label(), Some("L2 data"));
        assert_eq!(ProviderFeedLevel::OrderLevels.label(), Some("L3 data"));
        assert!(!ProviderFeedLevel::TopOfBook.depth_available());
        assert!(ProviderFeedLevel::PriceLevels.depth_available());
        assert!(ProviderFeedLevel::OrderLevels.depth_available());
        assert!(!ProviderFeedLevel::Bars.quotes_available());
        assert!(ProviderFeedLevel::TopOfBook.quotes_available());
    }
}
