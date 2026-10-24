//! Chart drawings of one saved layout.
//!
//! Aeris Charts owns the drawings of the instrument a chart shows. Drawings are anchored to that
//! instrument's prices, so a symbol switch replaces the chart with an empty one that is filled
//! from the layout's drawing set. A saved layout keeps one drawing set per instrument, shared by
//! every pane of every tab that shows the layout: each pane publishes its chart's committed edits
//! into the set and imports the set's newer revisions, so a pane that switches instrument, is
//! split off or closed never takes drawings with it. The exchanged document anchors drawings by
//! exchange time, so panes on different timeframes stay aligned.

use aeris_chart_integration::{AerisChartView, saved_drawings_document};
use aeris_contracts::{InstallProviderInstrument, WorkspaceInstrumentDrawingsState};
use aeris_observability::diagnostic;

/// Instruments one layout keeps drawings for.
pub(super) const MAXIMUM_DRAWING_INSTRUMENTS_PER_LAYOUT: usize = 32;
/// Drawing-document bytes one layout keeps beyond its most recently used instrument. That
/// instrument is bounded by the chart's own drawing limit instead, so it is never evicted.
pub(super) const MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_LAYOUT: usize = 1_048_576;

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

    fn drawings(&self, document: String) -> WorkspaceInstrumentDrawingsState {
        WorkspaceInstrumentDrawingsState {
            provider: self.provider.clone(),
            instrument_id: self.instrument_id.clone(),
            chart_state_json: document,
            locked_drawing_ids: Vec::new(),
        }
    }
}

/// Saved drawings as an Aeris Charts drawings document, or `None` when they are unreadable.
fn upgraded(
    mut drawings: WorkspaceInstrumentDrawingsState,
) -> Option<WorkspaceInstrumentDrawingsState> {
    match saved_drawings_document(&drawings.chart_state_json, &drawings.locked_drawing_ids) {
        Ok(document) => {
            drawings.chart_state_json = document;
            drawings.locked_drawing_ids.clear();
            Some(drawings)
        }
        Err(error) => {
            diagnostic!(
                "Aeris dropped unreadable saved drawings of {}: {error}",
                drawings.instrument_id
            );
            None
        }
    }
}

#[derive(Debug)]
struct LayoutEntry {
    instrument: DrawingInstrument,
    /// `None` records that the instrument's drawings were cleared, so panes still showing them
    /// remove them too. A cleared entry is never persisted.
    document: Option<String>,
    revision: u64,
}

/// The working drawing set of one saved layout: the drawings of every instrument shown on it.
#[derive(Debug, Default)]
pub(super) struct LayoutDrawings {
    /// Most recently changed or shown first.
    entries: Vec<LayoutEntry>,
    /// Advances on every change; each entry records the revision that last changed it.
    revision: u64,
    /// The revision last written to the saved layout.
    saved_revision: u64,
}

impl LayoutDrawings {
    /// The working set of a layout opened from its saved drawings.
    pub(super) fn from_saved(saved: &[WorkspaceInstrumentDrawingsState]) -> Self {
        let mut drawings = Self::default();
        for entry in
            bounded_instrument_drawings(saved.iter().cloned().filter_map(upgraded).collect())
        {
            drawings.revision += 1;
            drawings.entries.push(LayoutEntry {
                instrument: DrawingInstrument {
                    provider: entry.provider,
                    instrument_id: entry.instrument_id,
                },
                document: Some(entry.chart_state_json),
                revision: drawings.revision,
            });
        }
        drawings.saved_revision = drawings.revision;
        drawings
    }

    /// Whether the set changed since it was last written to the saved layout.
    pub(super) const fn has_unsaved_changes(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub(super) const fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// The durable drawings, most recently used first.
    pub(super) fn entries(&self) -> Vec<WorkspaceInstrumentDrawingsState> {
        self.entries
            .iter()
            .filter_map(|entry| {
                entry
                    .document
                    .clone()
                    .map(|document| entry.instrument.drawings(document))
            })
            .collect()
    }

    fn entry(&self, instrument: &DrawingInstrument) -> Option<&LayoutEntry> {
        self.entries
            .iter()
            .find(|entry| entry.instrument == *instrument)
    }

    fn revision_of(&self, instrument: &DrawingInstrument) -> Option<u64> {
        self.entry(instrument).map(|entry| entry.revision)
    }

    /// Replaces the drawings of `instrument`; `None` clears them. Returns whether the set
    /// changed.
    fn set(&mut self, instrument: &DrawingInstrument, document: Option<String>) -> bool {
        let existing = self
            .entries
            .iter()
            .position(|entry| entry.instrument == *instrument);
        match existing {
            Some(index) if self.entries[index].document == document => {
                self.touch(instrument);
                return false;
            }
            Some(index) => {
                self.entries.remove(index);
            }
            None if document.is_none() => return false,
            None => {}
        }
        self.revision += 1;
        self.entries.insert(
            0,
            LayoutEntry {
                instrument: instrument.clone(),
                document,
                revision: self.revision,
            },
        );
        self.bound();
        true
    }

    /// Marks `instrument` most recently used, so drawings a pane shows are evicted last.
    fn touch(&mut self, instrument: &DrawingInstrument) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.instrument == *instrument)
            && index > 0
        {
            let entry = self.entries.remove(index);
            self.entries.insert(0, entry);
        }
    }

    fn bound(&mut self) {
        let mut kept = Vec::with_capacity(self.entries.len());
        let (mut documents, mut clears, mut remembered_bytes, mut evicted) = (0, 0, 0_usize, 0);
        for entry in std::mem::take(&mut self.entries) {
            match &entry.document {
                None if clears >= MAXIMUM_DRAWING_INSTRUMENTS_PER_LAYOUT => continue,
                None => clears += 1,
                Some(document) => {
                    if documents > 0 {
                        if documents >= MAXIMUM_DRAWING_INSTRUMENTS_PER_LAYOUT
                            || remembered_bytes.saturating_add(document.len())
                                > MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_LAYOUT
                        {
                            evicted += 1;
                            continue;
                        }
                        remembered_bytes += document.len();
                    }
                    documents += 1;
                }
            }
            kept.push(entry);
        }
        if evicted > 0 {
            diagnostic!(
                "Aeris dropped the drawings of {evicted} least recently used instruments of a layout"
            );
        }
        self.entries = kept;
    }
}

/// How one pane's chart is kept on its layout's drawing set.
#[derive(Debug, Default)]
pub(super) struct PaneDrawingSync {
    /// The instrument the pane's chart was built for. During a symbol switch the surface already
    /// names the next instrument while the old chart stays on screen, so drawings key by this.
    chart_instrument: Option<DrawingInstrument>,
    /// The chart's drawing revision last published or imported, so imported drawings are never
    /// published back.
    synced_chart_revision: u64,
    /// The layout revision of `chart_instrument` the chart shows.
    applied_revision: u64,
    /// The chart was built without drawings and is filled from the layout during the next sync.
    pending_import: bool,
    /// Edits of a replaced chart that were not yet published.
    pending_publication: Option<(DrawingInstrument, Option<String>)>,
}

impl PaneDrawingSync {
    /// Starts a chart built for `instrument`, after `capture_outgoing` recorded the replaced one.
    pub(super) fn begin_chart(&mut self, instrument: Option<&InstallProviderInstrument>) {
        self.chart_instrument = instrument.map(DrawingInstrument::of);
        self.applied_revision = 0;
        self.pending_import = self.chart_instrument.is_some();
    }

    /// Records the drawing revision a newly built chart starts with.
    pub(super) fn settle_chart(&mut self, chart: &AerisChartView) {
        self.synced_chart_revision = chart.drawing_revision();
    }

    /// Keeps the chart's unpublished edits for the next sync before the chart is replaced.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts cannot serialize the drawings.
    pub(super) fn capture_outgoing(&mut self, chart: &AerisChartView) -> Result<(), String> {
        let Some(instrument) = &self.chart_instrument else {
            return Ok(());
        };
        let revision = chart.drawing_revision();
        if self.pending_import || !chart.has_market_data() || revision == self.synced_chart_revision
        {
            return Ok(());
        }
        self.synced_chart_revision = revision;
        self.pending_publication = Some((instrument.clone(), chart_document(chart)?));
        Ok(())
    }

    /// Publishes the chart's committed edits, and any captured from a replaced chart, into the
    /// layout. Returns whether the layout changed.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts cannot serialize the drawings.
    pub(super) fn publish(
        &mut self,
        chart: Option<&AerisChartView>,
        layout: &mut LayoutDrawings,
    ) -> Result<bool, String> {
        let mut changed = false;
        if let Some((instrument, document)) = self.pending_publication.take() {
            changed |= layout.set(&instrument, document);
        }
        let (Some(instrument), Some(chart)) = (&self.chart_instrument, chart) else {
            return Ok(changed);
        };
        if self.pending_import || !chart.has_market_data() {
            return Ok(changed);
        }
        let revision = chart.drawing_revision();
        if revision == self.synced_chart_revision {
            layout.touch(instrument);
            return Ok(changed);
        }
        self.synced_chart_revision = revision;
        changed |= layout.set(instrument, chart_document(chart)?);
        self.applied_revision = layout.revision_of(instrument).unwrap_or(0);
        Ok(changed)
    }

    /// Brings the chart to the layout's drawings of its instrument when they changed since the
    /// chart last showed them. Returns whether the chart changed.
    ///
    /// # Errors
    ///
    /// Returns an error when Aeris Charts rejects the drawings.
    pub(super) fn import(
        &mut self,
        chart: &mut AerisChartView,
        layout: &LayoutDrawings,
    ) -> Result<bool, String> {
        let Some(instrument) = &self.chart_instrument else {
            return Ok(false);
        };
        if !chart.has_market_data() {
            return Ok(false);
        }
        let entry = layout.entry(instrument);
        let revision = entry.map(|entry| entry.revision);
        if !self.pending_import && revision.is_none_or(|revision| revision == self.applied_revision)
        {
            return Ok(false);
        }
        self.pending_import = false;
        self.applied_revision = revision.unwrap_or(0);
        let imported = match entry.and_then(|entry| entry.document.as_deref()) {
            Some(document) => chart.import_drawings_json(document).map(|()| true),
            None if chart.drawing_count() > 0 => {
                chart.clear_drawings();
                Ok(true)
            }
            None => Ok(false),
        };
        self.synced_chart_revision = chart.drawing_revision();
        imported
    }

    /// Adopts `layout` as the set the chart already shows, after the tab moved to another layout
    /// holding the same drawings.
    pub(super) fn rebind(&mut self, layout: &LayoutDrawings) {
        if let Some(instrument) = &self.chart_instrument {
            self.applied_revision = layout.revision_of(instrument).unwrap_or(0);
        }
    }
}

fn chart_document(chart: &AerisChartView) -> Result<Option<String>, String> {
    if chart.drawing_count() == 0 {
        return Ok(None);
    }
    chart.export_drawings_json().map(Some)
}

/// Drawings a workspace saved before per-instrument drawings, which belong to the pane's
/// persisted instrument.
pub(super) fn legacy_pane_drawings(
    chart_state_json: &str,
    locked_drawing_ids: &[u32],
    instrument: Option<&InstallProviderInstrument>,
) -> Option<WorkspaceInstrumentDrawingsState> {
    let instrument = DrawingInstrument::of(instrument?);
    if chart_state_json.is_empty() {
        return None;
    }
    upgraded(WorkspaceInstrumentDrawingsState {
        locked_drawing_ids: locked_drawing_ids.to_vec(),
        ..instrument.drawings(chart_state_json.to_string())
    })
}

/// Drops malformed and duplicate entries, then the least recently used ones past the layout's
/// bounds. The first entry is always kept so the most recently used instrument never loses its
/// drawings.
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
            if kept.len() >= MAXIMUM_DRAWING_INSTRUMENTS_PER_LAYOUT
                || remembered_bytes.saturating_add(bytes)
                    > MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_LAYOUT
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

    fn instrument(instrument_id: &str) -> DrawingInstrument {
        DrawingInstrument {
            provider: "hyperliquid".to_string(),
            instrument_id: instrument_id.to_string(),
        }
    }

    const DOCUMENT: &str = r#"{"schema":"aeris_charts-drawings","schema_version":1,"drawings":[]}"#;

    #[test]
    fn the_layout_set_is_bounded_least_recently_used_first() {
        let entries = [drawings("unreadable", "[x]")]
            .into_iter()
            .chain(
                (0..MAXIMUM_DRAWING_INSTRUMENTS_PER_LAYOUT + 3)
                    .map(|index| drawings(&index.to_string(), DOCUMENT)),
            )
            .chain([drawings("0", "{}"), drawings("", DOCUMENT)])
            .collect::<Vec<_>>();
        let restored = LayoutDrawings::from_saved(&entries).entries();
        assert_eq!(restored.len(), MAXIMUM_DRAWING_INSTRUMENTS_PER_LAYOUT);
        assert_eq!(restored[0].instrument_id, "0");
        assert_eq!(restored[0].chart_state_json, DOCUMENT);

        let large = "x".repeat(MAXIMUM_REMEMBERED_DRAWING_BYTES_PER_LAYOUT);
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
    fn changes_advance_the_revision_until_saved_and_clears_are_not_persisted() {
        let mut layout = LayoutDrawings::from_saved(&[drawings("A", DOCUMENT)]);
        assert!(!layout.has_unsaved_changes());
        assert!(!layout.set(&instrument("A"), Some(DOCUMENT.to_string())));
        assert!(
            !layout.has_unsaved_changes(),
            "an unchanged document is no edit"
        );
        assert!(!layout.set(&instrument("B"), None), "nothing to clear");

        assert!(layout.set(&instrument("B"), Some(DOCUMENT.to_string())));
        assert!(layout.has_unsaved_changes());
        assert_eq!(ids(&layout.entries()), ["B", "A"]);
        layout.mark_saved();
        assert!(!layout.has_unsaved_changes());

        assert!(layout.set(&instrument("A"), None));
        assert_eq!(ids(&layout.entries()), ["B"]);
        assert!(layout.revision_of(&instrument("A")).is_some());
        assert!(layout.has_unsaved_changes());
    }

    #[test]
    fn legacy_drawings_need_the_panes_instrument_and_become_a_document() {
        let instrument = InstallProviderInstrument {
            provider: "hyperliquid".to_string(),
            instrument_id: "BTC".to_string(),
            ..InstallProviderInstrument::default()
        };
        let legacy = r#"[{"id":1,"kind":"horizontal_line","pane_index":0,"points":[{"logical":1.0,"price":2.0}]}]"#;
        let upgraded = legacy_pane_drawings(legacy, &[1], Some(&instrument))
            .expect("legacy drawings belong to the pane's instrument");
        assert_eq!(upgraded.instrument_id, "BTC");
        assert_eq!(upgraded.locked_drawing_ids, Vec::<u32>::new());
        let document: serde_json::Value =
            serde_json::from_str(&upgraded.chart_state_json).expect("document is JSON");
        assert_eq!(document["schema"], "aeris_charts-drawings");
        assert_eq!(document["drawings"][0]["locked"], true);
        assert_eq!(legacy_pane_drawings(legacy, &[], None), None);
        assert_eq!(legacy_pane_drawings("", &[], Some(&instrument)), None);
        assert_eq!(legacy_pane_drawings("[x]", &[], Some(&instrument)), None);
    }
}
