use crate::desktop::BuiltinIndicator;
use aeris_desktop::chart_chrome;
use aeris_localization::{tr, workspace};

/// Order-flow indicators the indicator menu lists. Each is one per chart, so choosing one already
/// on the chart shows it again rather than adding a second.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::desktop) enum OrderFlowMenuStudy {
    CumulativeDelta,
    Delta,
    BigTrades,
    OrderBookDepth,
    DepthHeatmap,
    OpenInterest,
    Liquidations,
}

impl OrderFlowMenuStudy {
    pub(in crate::desktop) const ALL: [Self; 7] = [
        Self::CumulativeDelta,
        Self::Delta,
        Self::BigTrades,
        Self::OrderBookDepth,
        Self::DepthHeatmap,
        Self::OpenInterest,
        Self::Liquidations,
    ];

    pub(in crate::desktop) fn label(self) -> &'static str {
        match self {
            Self::CumulativeDelta => tr!(workspace::ORDER_FLOW_CUMULATIVE_DELTA),
            Self::Delta => tr!(workspace::ORDER_FLOW_DELTA_HISTOGRAM),
            Self::BigTrades => tr!(workspace::ORDER_FLOW_BIG_TRADES),
            Self::OrderBookDepth => tr!(workspace::ORDER_FLOW_BOOK_ON_CHART),
            Self::DepthHeatmap => tr!(workspace::ORDER_FLOW_DEPTH_HEATMAP),
            Self::OpenInterest => tr!(workspace::ORDER_FLOW_OPEN_INTEREST),
            Self::Liquidations => tr!(workspace::ORDER_FLOW_LIQUIDATIONS),
        }
    }

    /// Whether the study is built from trade prints rather than the resting book or exchange stats.
    pub(in crate::desktop) const fn needs_trades(self) -> bool {
        !matches!(
            self,
            Self::OrderBookDepth | Self::DepthHeatmap | Self::OpenInterest | Self::Liquidations
        )
    }

    const fn favorite_identifier(self) -> &'static str {
        let index = match self {
            Self::CumulativeDelta => 0,
            Self::Delta => 1,
            Self::BigTrades => 2,
            Self::OrderBookDepth => 3,
            Self::DepthHeatmap => 4,
            Self::OpenInterest => 5,
            Self::Liquidations => 6,
        };
        chart_chrome::ORDER_FLOW_FAVORITE_IDENTIFIERS[index]
    }
}

/// One addable row of the indicator menu. Rendering, arrow keys, Enter and scrolling all index
/// the same list: the starred rows first, then the matching order-flow studies, then the
/// matching native indicators.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::desktop) enum IndicatorMenuEntry {
    OrderFlow(OrderFlowMenuStudy),
    Indicator(&'static chart_chrome::IndicatorSpec),
    Builtin(BuiltinIndicator),
}

/// The indicator menu's groups in display order; each non-empty one has a heading.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::desktop) enum IndicatorMenuGroup {
    Favorites,
    OrderFlow,
    Indicators,
}

impl IndicatorMenuEntry {
    pub(in crate::desktop) fn label(self) -> &'static str {
        match self {
            Self::OrderFlow(study) => study.label(),
            Self::Indicator(spec) => spec.label,
            Self::Builtin(indicator) => indicator.label(),
        }
    }

    /// The identifier the chart chrome stars the row under.
    pub(in crate::desktop) const fn favorite_identifier(self) -> &'static str {
        match self {
            Self::OrderFlow(study) => study.favorite_identifier(),
            Self::Indicator(spec) => spec.kind.identifier(),
            Self::Builtin(indicator) => indicator.identifier(),
        }
    }

    pub(in crate::desktop) fn group(
        self,
        favorites: chart_chrome::IndicatorFavorites,
    ) -> IndicatorMenuGroup {
        if favorites.contains(self.favorite_identifier()) {
            IndicatorMenuGroup::Favorites
        } else if matches!(self, Self::OrderFlow(_)) {
            IndicatorMenuGroup::OrderFlow
        } else {
            IndicatorMenuGroup::Indicators
        }
    }
}

/// Scroll-body child of a menu entry. Each non-empty group is preceded by its heading.
pub(in crate::desktop) fn indicator_menu_scroll_item(
    entries: &[IndicatorMenuEntry],
    favorites: chart_chrome::IndicatorFavorites,
    selection: usize,
) -> Option<usize> {
    let group = entries.get(selection)?.group(favorites);
    let mut groups = entries
        .iter()
        .map(|entry| entry.group(favorites))
        .filter(|candidate| *candidate <= group)
        .collect::<Vec<_>>();
    groups.dedup();
    Some(selection + groups.len())
}
