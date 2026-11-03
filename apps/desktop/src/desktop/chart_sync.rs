//! Which chart changes the panes of one workspace tab share. The tab owns the choice and
//! `TerminalApp::synchronize_linked_charts` relays each shared change to the tab's other panes.

use aeris_contracts::WorkspaceTabState;
use aeris_localization::{Text, shell};

/// One kind of change a tab can share between its charts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChartSyncKind {
    Symbol,
    Timeframe,
    Crosshair,
    Time,
}

impl ChartSyncKind {
    pub(super) const ALL: [Self; 4] = [Self::Symbol, Self::Timeframe, Self::Crosshair, Self::Time];

    const fn bit(self) -> u32 {
        match self {
            Self::Symbol => 0b0001,
            Self::Timeframe => 0b0010,
            Self::Crosshair => 0b0100,
            Self::Time => 0b1000,
        }
    }

    pub(super) const fn id(self) -> &'static str {
        match self {
            Self::Symbol => "chart_sync_symbol",
            Self::Timeframe => "chart_sync_timeframe",
            Self::Crosshair => "chart_sync_crosshair",
            Self::Time => "chart_sync_time",
        }
    }

    pub(super) const fn label(self) -> Text {
        match self {
            Self::Symbol => shell::CHART_SYNC_SYMBOL,
            Self::Timeframe => shell::CHART_SYNC_TIMEFRAME,
            Self::Crosshair => shell::CHART_SYNC_CROSSHAIR,
            Self::Time => shell::CHART_SYNC_TIME,
        }
    }
}

/// The set of shared changes, stored as `WorkspaceTabState::chart_sync` bits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ChartSync(u32);

const ALL_BITS: u32 = 0b1111;

impl ChartSync {
    pub(super) const fn from_bits(bits: u32) -> Self {
        Self(bits & ALL_BITS)
    }

    pub(super) const fn bits(self) -> u32 {
        self.0
    }

    pub(super) const fn contains(self, kind: ChartSyncKind) -> bool {
        self.0 & kind.bit() != 0
    }

    #[must_use]
    pub(super) const fn toggled(self, kind: ChartSyncKind) -> Self {
        Self(self.0 ^ kind.bit())
    }

    /// Whether the tab shares this engine sync event. Terminal links financial charts only,
    /// which never publish general-chart events.
    pub(super) const fn shares(self, kind: &aeris_chart_integration::ChartSyncEventKind) -> bool {
        match kind {
            aeris_chart_integration::ChartSyncEventKind::Crosshair { .. }
            | aeris_chart_integration::ChartSyncEventKind::ClearCrosshair => {
                self.contains(ChartSyncKind::Crosshair)
            }
            aeris_chart_integration::ChartSyncEventKind::VisibleTimeRange { .. } => {
                self.contains(ChartSyncKind::Time)
            }
            aeris_chart_integration::ChartSyncEventKind::GeneralAxisView { .. }
            | aeris_chart_integration::ChartSyncEventKind::GeneralCrosshair { .. } => false,
        }
    }
}

/// Gives a tab written before `chart_sync` existed the sync its panes' link groups implied:
/// a pane in any group shared its symbol, and its link flags (bit 0 crosshair, bit 1 time
/// range) chose the rest. The panes' legacy fields are cleared either way, so they are never
/// read again.
pub(super) fn adopt_legacy_link_groups(tab: &mut WorkspaceTabState) {
    if tab.chart_sync.is_none() {
        let linked = tab
            .panes
            .iter()
            .filter_map(|pane| pane.chart.as_ref())
            .filter(|chart| chart.chart_link_group != 0);
        let mut sync = 0;
        for chart in linked {
            sync |= ChartSyncKind::Symbol.bit();
            if chart.chart_link_flags & 0b01 != 0 {
                sync |= ChartSyncKind::Crosshair.bit();
            }
            if chart.chart_link_flags & 0b10 != 0 {
                sync |= ChartSyncKind::Time.bit();
            }
        }
        tab.chart_sync = Some(sync);
    }
    tab.chart_sync = tab.chart_sync.map(|bits| bits & ALL_BITS);
    for chart in tab.panes.iter_mut().filter_map(|pane| pane.chart.as_mut()) {
        chart.chart_link_group = 0;
        chart.chart_link_flags = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_contracts::{WorkspaceChartState, WorkspacePaneState};

    fn pane(group: u32, flags: u32) -> WorkspacePaneState {
        WorkspacePaneState {
            chart: Some(WorkspaceChartState {
                chart_link_group: group,
                chart_link_flags: flags,
                ..WorkspaceChartState::default()
            }),
            ..WorkspacePaneState::default()
        }
    }

    #[test]
    fn each_kind_toggles_on_its_own() {
        let sync = ChartSync::default().toggled(ChartSyncKind::Timeframe);
        assert!(sync.contains(ChartSyncKind::Timeframe));
        assert!(!sync.contains(ChartSyncKind::Symbol));
        let sync = sync.toggled(ChartSyncKind::Crosshair);
        assert!(sync.contains(ChartSyncKind::Crosshair));
        assert!(
            !sync
                .toggled(ChartSyncKind::Timeframe)
                .contains(ChartSyncKind::Timeframe)
        );
        assert_eq!(ChartSync::from_bits(u32::MAX).bits(), ALL_BITS);
    }

    #[test]
    fn a_legacy_link_group_becomes_the_tab_sync_and_is_cleared() {
        let mut tab = WorkspaceTabState {
            panes: vec![pane(1, 0b01), pane(1, 0b10), pane(0, 0b11)],
            ..WorkspaceTabState::default()
        };
        adopt_legacy_link_groups(&mut tab);
        let sync = ChartSync::from_bits(tab.chart_sync.expect("derived"));
        assert!(sync.contains(ChartSyncKind::Symbol));
        assert!(sync.contains(ChartSyncKind::Crosshair));
        assert!(sync.contains(ChartSyncKind::Time));
        assert!(!sync.contains(ChartSyncKind::Timeframe));
        assert!(tab.panes.iter().all(|pane| {
            pane.chart
                .as_ref()
                .is_some_and(|chart| chart.chart_link_group == 0 && chart.chart_link_flags == 0)
        }));
    }

    #[test]
    fn an_unlinked_legacy_tab_syncs_nothing_and_a_saved_choice_is_kept() {
        let mut legacy = WorkspaceTabState {
            panes: vec![pane(0, 0b11)],
            ..WorkspaceTabState::default()
        };
        adopt_legacy_link_groups(&mut legacy);
        assert_eq!(legacy.chart_sync, Some(0));

        let mut current = WorkspaceTabState {
            panes: vec![pane(2, 0b11)],
            chart_sync: Some(ChartSyncKind::Timeframe.bit()),
            ..WorkspaceTabState::default()
        };
        adopt_legacy_link_groups(&mut current);
        assert_eq!(current.chart_sync, Some(ChartSyncKind::Timeframe.bit()));
    }
}
