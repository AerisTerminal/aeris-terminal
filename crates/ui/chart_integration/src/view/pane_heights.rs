//! Separate-pane heights the user chose, kept by what each pane shows so a chart rebuilt for
//! another interval or instrument, or restored from a workspace, opens them at the same size.

use super::{AerisChartView, ChartIndicator, LegendItem, OrderFlowStudy};
use aeris_charts_engine::IndicatorBindingInfo;
use std::collections::BTreeMap;

/// Most remembered pane heights. A new entry evicts the least recently chosen one.
const MAX_PANE_HEIGHTS: usize = 32;

/// What a separate chart pane shows, independent of engine-local series identities.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChartPaneContent {
    Volume,
    CumulativeDelta,
    Delta,
    /// The `occurrence`-th native indicator of this kind, in creation order.
    Indicator {
        indicator: ChartIndicator,
        occurrence: u8,
    },
    /// An external study, by the durable key its host gave through
    /// [`AerisChartView::set_study_pane_key`].
    Study(u64),
}

impl ChartPaneContent {
    /// Returns the durable workspace identifier of this pane content.
    #[must_use]
    pub fn identifier(self) -> String {
        match self {
            Self::Volume => "volume".to_string(),
            Self::CumulativeDelta => "order_flow.cumulative_delta".to_string(),
            Self::Delta => "order_flow.delta".to_string(),
            Self::Indicator {
                indicator,
                occurrence,
            } => format!("indicator.{}.{occurrence}", indicator.identifier()),
            Self::Study(key) => format!("study.{key}"),
        }
    }

    /// Parses a durable workspace pane content identifier.
    #[must_use]
    pub fn from_identifier(value: &str) -> Option<Self> {
        match value {
            "volume" => return Some(Self::Volume),
            "order_flow.cumulative_delta" => return Some(Self::CumulativeDelta),
            "order_flow.delta" => return Some(Self::Delta),
            _ => {}
        }
        if let Some(key) = value.strip_prefix("study.") {
            return key.parse().ok().map(Self::Study);
        }
        let (indicator, occurrence) = value.strip_prefix("indicator.")?.rsplit_once('.')?;
        Some(Self::Indicator {
            indicator: ChartIndicator::from_identifier(indicator)?,
            occurrence: occurrence.parse().ok()?,
        })
    }
}

/// The height of one separate pane as a multiple of the main price pane's height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartPaneHeight {
    pub content: ChartPaneContent,
    pub share: f64,
}

#[derive(Debug, Default)]
pub(super) struct PaneHeights {
    /// Heights the user chose, least recently chosen first.
    chosen: Vec<ChartPaneHeight>,
    /// Content of each pane at the last sync, so only a pane that newly appears is resized and
    /// a pane the user is looking at never jumps.
    seen: Vec<Option<ChartPaneContent>>,
    /// Durable host keys of external studies, by engine study id.
    study_keys: BTreeMap<u64, u64>,
}

fn remember(chosen: &mut Vec<ChartPaneHeight>, height: ChartPaneHeight) {
    chosen.retain(|entry| entry.content != height.content);
    if chosen.len() == MAX_PANE_HEIGHTS {
        chosen.remove(0);
    }
    chosen.push(height);
}

impl AerisChartView {
    /// Heights the user chose for separate panes, including panes not on the chart right now.
    /// Empty until the user resizes a pane, so untouched charts keep the default heights.
    #[must_use]
    pub fn pane_heights(&self) -> Vec<ChartPaneHeight> {
        self.pane_heights.chosen.clone()
    }

    /// Replaces the remembered pane heights. Each pane on the chart, and each pane that appears
    /// later, opens at the height chosen for its content.
    pub fn restore_pane_heights(&mut self, heights: &[ChartPaneHeight]) {
        let mut chosen = Vec::new();
        for height in heights {
            if height.share.is_finite() && height.share > 0.0 {
                remember(&mut chosen, *height);
            }
        }
        self.pane_heights.chosen = chosen;
        self.pane_heights.seen.clear();
        self.invalidate_series_layout();
    }

    /// Names external study `study_id` by a key that survives chart rebuilds and restarts, so its
    /// pane height can be restored.
    pub fn set_study_pane_key(&mut self, study_id: u64, key: u64) {
        self.pane_heights.study_keys.insert(study_id, key);
    }

    /// Records the current height of every separate pane after the user resized panes.
    pub(super) fn remember_pane_heights(&mut self) {
        let contents = self.pane_contents();
        let mut changed = false;
        for (index, content) in contents.iter().enumerate().skip(1) {
            let (Some(content), Some(share)) = (*content, self.engine.pane_height_share(index))
            else {
                continue;
            };
            remember(
                &mut self.pane_heights.chosen,
                ChartPaneHeight { content, share },
            );
            changed = true;
        }
        self.pane_heights.seen = contents;
        if changed {
            self.mark_user_state_changed();
        }
    }

    /// Gives each newly appearing pane the height chosen for its content before the next layout.
    pub(super) fn sync_pane_heights(&mut self) {
        let pane_count = self.engine.panes.len();
        if !self.layout_dirty && pane_count == self.pane_heights.seen.len() {
            return;
        }
        let engine = &self.engine;
        self.pane_heights
            .study_keys
            .retain(|study_id, _| engine.external_study_visible(*study_id).is_some());
        if self.pane_heights.chosen.is_empty() {
            self.pane_heights.seen.clear();
            self.pane_heights.seen.resize(pane_count, None);
            return;
        }
        let contents = self.pane_contents();
        let mut changed = false;
        for (index, content) in contents.iter().enumerate().skip(1) {
            let Some(content) = *content else {
                continue;
            };
            if self.pane_heights.seen.contains(&Some(content)) {
                continue;
            }
            if let Some(height) = self
                .pane_heights
                .chosen
                .iter()
                .find(|height| height.content == content)
            {
                changed |= self.engine.set_pane_height_share(index, height.share);
            }
        }
        self.pane_heights.seen = contents;
        if changed {
            self.layout_dirty = true;
        }
    }

    /// The content of each pane, named by the first legend row it holds. The main price pane
    /// and panes without a durable name are `None`.
    fn pane_contents(&self) -> Vec<Option<ChartPaneContent>> {
        let mut contents = vec![None; self.engine.panes.len()];
        let bindings = self.engine.indicator_bindings();
        for row in self.legend_rows_at(None) {
            if row.pane == 0 {
                continue;
            }
            let Some(slot) = contents.get_mut(row.pane).filter(|slot| slot.is_none()) else {
                continue;
            };
            *slot = match row.item {
                LegendItem::Volume => Some(ChartPaneContent::Volume),
                LegendItem::OrderFlow(OrderFlowStudy::CumulativeDelta) => {
                    Some(ChartPaneContent::CumulativeDelta)
                }
                LegendItem::OrderFlow(OrderFlowStudy::Delta) => Some(ChartPaneContent::Delta),
                LegendItem::Indicator(binding) => indicator_content(&bindings, binding),
                LegendItem::Study { study_id, .. } => self
                    .pane_heights
                    .study_keys
                    .get(&study_id)
                    .copied()
                    .map(ChartPaneContent::Study),
                LegendItem::Asset
                | LegendItem::VolumeProfile
                | LegendItem::BigTrades
                | LegendItem::OrderBookDepth
                | LegendItem::DepthHeatmap => None,
            };
        }
        contents
    }
}

fn indicator_content(bindings: &[IndicatorBindingInfo], binding: u32) -> Option<ChartPaneContent> {
    let position = bindings
        .iter()
        .position(|candidate| candidate.binding_id == binding)?;
    let indicator = ChartIndicator::from_engine_kind(&bindings[position].kind)?;
    let occurrence = bindings[..position]
        .iter()
        .filter(|candidate| ChartIndicator::from_engine_kind(&candidate.kind) == Some(indicator))
        .count();
    Some(ChartPaneContent::Indicator {
        indicator,
        occurrence: u8::try_from(occurrence).ok()?,
    })
}
