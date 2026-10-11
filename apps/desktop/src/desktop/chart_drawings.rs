//! Per-instrument chart drawings remembered by one workspace pane.
//!
//! Aeris Charts owns the drawings of the instrument a chart shows. Drawings are anchored to
//! that instrument's prices, so a symbol switch replaces the chart with an empty one; the pane
//! keeps what it drew on every other instrument so switching back restores it, within and
//! across sessions.
//!
//! Panes of one workspace that show the same instrument share one drawing set: a committed edit
//! is published to the sibling panes, which apply it to their chart or to the entry they
//! remember, and a chart that opens without drawings adopts a sibling's. The exchanged document
//! anchors drawings by exchange time, so panes on different timeframes stay aligned.

use aeris_chart_integration::AerisChartView;
use aeris_contracts::{InstallProviderInstrument, WorkspaceInstrumentDrawingsState};
use aeris_observability::diagnostic;

/// Instruments one pane remembers drawings for, including the one it shows.
pub(super) const MAXIMUM_DRAWING_INSTRUMENTS_PER_PANE: usize = 32;
/// Drawing-document bytes one pane remembers for instruments it does not show. The shown
/// instrument is bounded by the chart's own drawing limit instead, so it is never evicted.
pub(super) const MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_PANE: usize = 1_048_576;

/// Stable identity drawings are keyed by. Session and selection generations are deliberately
/// excluded: they fence one runtime process, not the market the drawings belong to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DrawingInstrument {
    provider: String,
    instrument_id: String,
}

impl DrawingInstrument {
    fn of(instrument: &InstallProviderInstrument) -> Self {
        Self {
            provider: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
        }
    }

    fn keys(&self, drawings: &WorkspaceInstrumentDrawingsState) -> bool {
        drawings.provider == self.provider && drawings.instrument_id == self.instrument_id
    }

    fn drawings(
        &self,
        chart_state_json: String,
        locked_drawing_ids: Vec<u32>,
    ) -> WorkspaceInstrumentDrawingsState {
        WorkspaceInstrumentDrawingsState {
            provider: self.provider.clone(),
            instrument_id: self.instrument_id.clone(),
            chart_state_json,
            locked_drawing_ids,
        }
    }
}

/// The complete drawing set of one instrument as exchanged between the panes of a workspace;
/// `None` drawings means the instrument has none.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SharedDrawings {
    instrument: DrawingInstrument,
    drawings: Option<WorkspaceInstrumentDrawingsState>,
}

/// The drawings of every instrument one pane has shown.
#[derive(Debug, Default)]
pub(super) struct PaneDrawings {
    /// The instrument the pane's chart was built for. During a symbol switch the surface already
    /// names the next instrument while the old chart stays on screen, so drawings key by this.
    chart_instrument: Option<DrawingInstrument>,
    /// Most recently used first.
    remembered: Vec<WorkspaceInstrumentDrawingsState>,
    /// The shown chart's drawing revision last published or applied, so drawings applied from a
    /// sibling pane are never published back.
    shared_revision: u64,
    /// The shown chart opened without drawings and may adopt a sibling pane's.
    awaiting_peer: bool,
}

impl PaneDrawings {
    pub(super) fn restored(remembered: &[WorkspaceInstrumentDrawingsState]) -> Self {
        Self {
            remembered: bounded_instrument_drawings(remembered.to_vec()),
            ..Self::default()
        }
    }

    /// Records the drawings a newly built chart was restored with, after `begin_chart`.
    pub(super) fn settle_chart(&mut self, chart: &AerisChartView) {
        self.shared_revision = chart.drawing_revision();
        self.awaiting_peer = self.chart_instrument.is_some() && chart.drawing_count() == 0;
    }

    fn shows(&self, instrument: &DrawingInstrument, chart: &AerisChartView) -> bool {
        self.chart_instrument.as_ref() == Some(instrument) && chart.has_market_data()
    }

    /// The shown chart's drawings when they changed since last shared.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts cannot serialize the drawings.
    pub(super) fn take_publication(
        &mut self,
        chart: &AerisChartView,
    ) -> Result<Option<SharedDrawings>, String> {
        let revision = chart.drawing_revision();
        if revision == self.shared_revision {
            return Ok(None);
        }
        self.shared_revision = revision;
        self.awaiting_peer = false;
        let Some(instrument) = &self.chart_instrument else {
            return Ok(None);
        };
        self.live(instrument, chart)
    }

    /// The instrument whose drawings the shown chart should adopt from a sibling pane, once.
    pub(super) fn take_peer_request(
        &mut self,
        chart: &AerisChartView,
    ) -> Option<DrawingInstrument> {
        if !self.awaiting_peer || !chart.has_market_data() {
            return None;
        }
        self.awaiting_peer = false;
        self.chart_instrument.clone()
    }

    /// The shown chart's drawings when it shows `instrument`.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts cannot serialize the drawings.
    pub(super) fn live(
        &self,
        instrument: &DrawingInstrument,
        chart: &AerisChartView,
    ) -> Result<Option<SharedDrawings>, String> {
        if !self.shows(instrument, chart) {
            return Ok(None);
        }
        Ok(Some(SharedDrawings {
            instrument: instrument.clone(),
            drawings: Self::chart_drawings(instrument, chart)?,
        }))
    }

    /// The drawings remembered for `instrument` while another one is shown.
    pub(super) fn remembered_for(&self, instrument: &DrawingInstrument) -> Option<SharedDrawings> {
        self.remembered
            .iter()
            .find(|entry| instrument.keys(entry))
            .map(|entry| SharedDrawings {
                instrument: instrument.clone(),
                drawings: Some(entry.clone()),
            })
    }

    /// Applies a sibling pane's drawings onto the shown chart when it shows their instrument,
    /// otherwise onto the remembered entry, so a later switch back restores the current set.
    /// Returns whether anything changed.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts rejects the drawings.
    pub(super) fn receive(
        &mut self,
        shared: &SharedDrawings,
        chart: Option<&mut AerisChartView>,
    ) -> Result<bool, String> {
        if let Some(chart) = chart.filter(|chart| self.shows(&shared.instrument, chart)) {
            let imported = if let Some(drawings) = &shared.drawings {
                chart.import_semantic_state_json(
                    &drawings.chart_state_json,
                    &drawings.locked_drawing_ids,
                )
            } else {
                chart.clear_drawings();
                Ok(())
            };
            self.shared_revision = chart.drawing_revision();
            self.awaiting_peer = false;
            return imported.map(|()| true);
        }
        let Some(index) = self
            .remembered
            .iter()
            .position(|entry| shared.instrument.keys(entry))
        else {
            return Ok(false);
        };
        match &shared.drawings {
            Some(drawings) => self.remembered[index] = drawings.clone(),
            None => {
                self.remembered.remove(index);
            }
        }
        self.remembered = bounded_instrument_drawings(std::mem::take(&mut self.remembered));
        Ok(true)
    }

    /// Records the shown chart's drawings before the chart is replaced.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts cannot serialize the drawings; the
    /// previously remembered drawings are then kept.
    pub(super) fn remember(&mut self, chart: &AerisChartView) -> Result<(), String> {
        let Some(instrument) = &self.chart_instrument else {
            return Ok(());
        };
        if !chart.has_market_data() {
            return Ok(());
        }
        let drawings = Self::chart_drawings(instrument, chart)?;
        self.remembered.retain(|entry| !instrument.keys(entry));
        if let Some(drawings) = drawings {
            self.remembered.insert(0, drawings);
        }
        self.remembered = bounded_instrument_drawings(std::mem::take(&mut self.remembered));
        Ok(())
    }

    /// Starts a chart built for `instrument` and returns the drawings to restore onto it.
    pub(super) fn begin_chart(
        &mut self,
        instrument: Option<&InstallProviderInstrument>,
    ) -> Option<WorkspaceInstrumentDrawingsState> {
        self.chart_instrument = instrument.map(DrawingInstrument::of);
        let instrument = self.chart_instrument.as_ref()?;
        self.remembered
            .iter()
            .find(|entry| instrument.keys(entry))
            .cloned()
    }

    /// The drawings remembered from charts the pane has already replaced.
    pub(super) fn remembered(&self) -> Vec<WorkspaceInstrumentDrawingsState> {
        self.remembered.clone()
    }

    /// The durable drawings, with the shown chart's live drawings taking precedence.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts cannot serialize the shown chart's drawings.
    pub(super) fn persisted(
        &self,
        chart: &AerisChartView,
    ) -> Result<Vec<WorkspaceInstrumentDrawingsState>, String> {
        let Some(instrument) = &self.chart_instrument else {
            return Ok(self.remembered());
        };
        if !chart.has_market_data() {
            return Ok(self.remembered());
        }
        let entries = Self::chart_drawings(instrument, chart)?
            .into_iter()
            .chain(
                self.remembered
                    .iter()
                    .filter(|entry| !instrument.keys(entry))
                    .cloned(),
            )
            .collect();
        Ok(bounded_instrument_drawings(entries))
    }

    fn chart_drawings(
        instrument: &DrawingInstrument,
        chart: &AerisChartView,
    ) -> Result<Option<WorkspaceInstrumentDrawingsState>, String> {
        if chart.drawing_count() == 0 {
            return Ok(None);
        }
        let json = chart.export_semantic_state_json()?;
        Ok(Some(instrument.drawings(json, chart.locked_drawing_ids())))
    }
}

/// Drawings a workspace saved before per-instrument drawings, which belong to the pane's
/// persisted instrument.
pub(super) fn legacy_pane_drawings(
    chart_state_json: &str,
    locked_drawing_ids: &[u32],
    instrument: Option<&InstallProviderInstrument>,
) -> Option<WorkspaceInstrumentDrawingsState> {
    let instrument = DrawingInstrument::of(instrument?);
    (!chart_state_json.is_empty())
        .then(|| instrument.drawings(chart_state_json.to_string(), locked_drawing_ids.to_vec()))
}

/// Drops malformed and duplicate entries, then the least recently used ones past the pane's
/// bounds. The first entry is always kept so the shown instrument never loses its drawings.
pub(super) fn bounded_instrument_drawings(
    entries: Vec<WorkspaceInstrumentDrawingsState>,
) -> Vec<WorkspaceInstrumentDrawingsState> {
    let mut kept = Vec::<WorkspaceInstrumentDrawingsState>::new();
    let mut remembered_bytes = 0_usize;
    let mut evicted = 0_usize;
    for entry in entries {
        if entry.provider.is_empty()
            || entry.instrument_id.is_empty()
            || entry.chart_state_json.is_empty()
            || kept.iter().any(|kept| {
                kept.provider == entry.provider && kept.instrument_id == entry.instrument_id
            })
        {
            continue;
        }
        if !kept.is_empty() {
            let bytes = entry.chart_state_json.len();
            if kept.len() >= MAXIMUM_DRAWING_INSTRUMENTS_PER_PANE
                || remembered_bytes.saturating_add(bytes)
                    > MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_PANE
            {
                evicted += 1;
                continue;
            }
            remembered_bytes += bytes;
        }
        kept.push(entry);
    }
    if evicted > 0 {
        diagnostic!(
            "Aeris dropped the saved drawings of {evicted} least recently used instruments"
        );
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawings(instrument_id: &str, json: &str) -> WorkspaceInstrumentDrawingsState {
        WorkspaceInstrumentDrawingsState {
            provider: "hyperliquid".to_string(),
            instrument_id: instrument_id.to_string(),
            chart_state_json: json.to_string(),
            locked_drawing_ids: Vec::new(),
        }
    }

    fn ids(entries: &[WorkspaceInstrumentDrawingsState]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| entry.instrument_id.as_str())
            .collect()
    }

    #[test]
    fn the_library_is_bounded_least_recently_used_first() {
        let entries = (0..MAXIMUM_DRAWING_INSTRUMENTS_PER_PANE + 3)
            .map(|index| drawings(&index.to_string(), "[x]"))
            .chain([drawings("0", "[duplicate]"), drawings("", "[x]")])
            .collect::<Vec<_>>();
        let restored = PaneDrawings::restored(&entries).remembered();
        assert_eq!(restored.len(), MAXIMUM_DRAWING_INSTRUMENTS_PER_PANE);
        assert_eq!(restored[0].chart_state_json, "[x]");

        let large = "x".repeat(MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_PANE);
        let bounded = bounded_instrument_drawings(vec![
            drawings("shown", &large),
            drawings("recent", &large),
            drawings("older", "[x]"),
        ]);
        assert_eq!(ids(&bounded), ["shown", "recent"]);
        let bounded = bounded_instrument_drawings(vec![
            drawings("shown", "[x]"),
            drawings("recent", "[x]"),
            drawings("oversized", &format!("{large}x")),
        ]);
        assert_eq!(ids(&bounded), ["shown", "recent"]);
    }

    #[test]
    fn legacy_drawings_need_the_panes_instrument() {
        let instrument = InstallProviderInstrument {
            provider: "hyperliquid".to_string(),
            instrument_id: "BTC".to_string(),
            ..InstallProviderInstrument::default()
        };
        assert_eq!(
            legacy_pane_drawings("[x]", &[], Some(&instrument)),
            Some(drawings("BTC", "[x]"))
        );
        assert_eq!(legacy_pane_drawings("[x]", &[], None), None);
        assert_eq!(legacy_pane_drawings("", &[], Some(&instrument)), None);
    }
}
