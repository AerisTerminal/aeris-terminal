//! The watchlist the terminal shows: its rows with their flags, the session-only flag filter,
//! filter menu and color picker, the row being dragged, and the day summary kept for every
//! listed or tabbed instrument. Mutators report what changed; the terminal decides what to
//! persist and redraw.

use std::collections::BTreeMap;

use gpui::{FocusHandle, ScrollHandle, Window};

use super::local_state::MAXIMUM_WATCHLIST_ENTRIES;
use super::watchlist_panel::{WATCHLIST_ROW_HEIGHT, WatchlistFlagsView};
use super::workspace_tabs::move_item;
use super::{
    InstallProviderInstrument, MarketSummaryEntry, MarketSummaryKey, WatchlistDragState,
    WatchlistEntry, WatchlistFlagPicker, WorkspaceWatchlistEntryState, normalize_hex_color,
    visible_watchlist_indices, watchlist_drag_destination, watchlist_flag_colors,
};

/// Session-only watchlist flag UI: the color filter, its menu and the open flag picker.
#[derive(Default)]
struct WatchlistFlagState {
    /// Shows only the watchlist rows flagged with this color.
    filter: Option<String>,
    filter_menu_open: bool,
    picker: Option<WatchlistFlagPicker>,
}

/// What a left press on a row's flag did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FlagPress {
    /// No row lists the instrument.
    Missing,
    /// The unflagged row took the default color; its picker should open.
    Flagged,
    /// The flagged row lost its flag.
    Cleared,
}

/// What recoloring the open flag picker did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PickerColor {
    /// No picker is open.
    Closed,
    /// The color is not a hex color; the picker shows the error.
    Rejected,
    /// The picker's row took the color.
    Applied,
    /// The color is valid but the picker's row is no longer listed.
    Accepted,
}

/// How a pointer move changed the row drag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DragMove {
    /// No drag of that row, or no listed row under the pointer to place it at.
    Unplaced,
    /// The dragged row is at the pointer's slot; `reordered` when it moved there.
    Placed { reordered: bool },
}

pub(super) struct MarketWatch {
    entries: Vec<WatchlistEntry>,
    flags: WatchlistFlagState,
    drag: Option<WatchlistDragState>,
    scroll: ScrollHandle,
    /// The table's keyboard focus, which a press inside the table takes.
    focus: FocusHandle,
    /// The day summary of every listed instrument and of every tab's shown instrument.
    summaries: BTreeMap<MarketSummaryKey, MarketSummaryEntry>,
}

impl MarketWatch {
    /// Restores the saved rows, dropping rows without an instrument and invalid flag colors.
    pub(super) fn restore(entries: Vec<WorkspaceWatchlistEntryState>, focus: FocusHandle) -> Self {
        Self {
            entries: entries
                .into_iter()
                .filter_map(|entry| {
                    Some(WatchlistEntry {
                        instrument: entry.instrument?,
                        flag: normalize_hex_color(&entry.flag_color),
                    })
                })
                .collect(),
            flags: WatchlistFlagState::default(),
            drag: None,
            scroll: ScrollHandle::new(),
            focus,
            summaries: BTreeMap::new(),
        }
    }

    pub(super) fn entries(&self) -> &[WatchlistEntry] {
        &self.entries
    }

    /// The rows as the workspace file stores them.
    pub(super) fn durable_entries(&self) -> Vec<WorkspaceWatchlistEntryState> {
        self.entries
            .iter()
            .map(|entry| WorkspaceWatchlistEntryState {
                instrument: Some(entry.instrument.clone()),
                flag_color: entry.flag.clone().unwrap_or_default(),
            })
            .collect()
    }

    pub(super) fn instruments(&self) -> impl Iterator<Item = &InstallProviderInstrument> {
        self.entries.iter().map(|entry| &entry.instrument)
    }

    /// Indices of the rows the flag filter shows.
    pub(super) fn visible_indices(&self) -> Vec<usize> {
        visible_watchlist_indices(
            &self.entries,
            self.flags.filter.as_deref(),
            self.flags.picker.as_ref(),
        )
    }

    pub(super) fn flags_view(&self) -> WatchlistFlagsView {
        WatchlistFlagsView {
            picker: self.flags.picker.clone(),
            filter: self.flags.filter.clone(),
            filter_menu_open: self.flags.filter_menu_open,
            colors: watchlist_flag_colors(&self.entries),
        }
    }

    pub(super) const fn drag(&self) -> Option<&WatchlistDragState> {
        self.drag.as_ref()
    }

    pub(super) const fn scroll(&self) -> &ScrollHandle {
        &self.scroll
    }

    pub(super) const fn focus(&self) -> &FocusHandle {
        &self.focus
    }

    /// Whether Up and Down step the rows: the table itself holds keyboard focus (not a field
    /// inside it) and neither the flag picker nor the flag filter menu is open.
    pub(super) fn arrow_keys_step_rows(&self, window: &Window) -> bool {
        self.focus.is_focused(window) && self.flags.picker.is_none() && !self.flags.filter_menu_open
    }

    /// Lists the instrument unflagged; refused when it is listed already or the list is full.
    pub(super) fn add(&mut self, instrument: InstallProviderInstrument) -> bool {
        if self
            .position(&instrument.provider, &instrument.instrument_id)
            .is_some()
            || self.entries.len() >= MAXIMUM_WATCHLIST_ENTRIES
        {
            return false;
        }
        self.entries.push(WatchlistEntry {
            instrument,
            flag: None,
        });
        true
    }

    /// Unlists the instrument, closing its flag picker. Reports whether it was listed.
    pub(super) fn remove(&mut self, provider: &str, instrument_id: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| !entry.is(provider, instrument_id));
        if self.entries.len() == before {
            return false;
        }
        if self.picker_is_for(provider, instrument_id) {
            self.flags.picker = None;
        }
        self.release_unused_filter();
        true
    }

    /// Starts dragging a listed row; `cursor_offset_y` is where the row was grabbed.
    pub(super) fn begin_drag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        cursor_offset_y: f32,
    ) -> bool {
        if self.position(provider, instrument_id).is_none() {
            return false;
        }
        self.drag = Some(WatchlistDragState {
            provider: provider.to_string(),
            instrument_id: instrument_id.to_string(),
            cursor_offset_y: if cursor_offset_y.is_finite() {
                cursor_offset_y.clamp(0.0, WATCHLIST_ROW_HEIGHT)
            } else {
                WATCHLIST_ROW_HEIGHT / 2.0
            },
            pointer_y: None,
            body_top: 0.0,
            scroll_offset_y: 0.0,
        });
        true
    }

    /// Follows the pointer with the dragged row and moves the row to the slot under it.
    pub(super) fn move_drag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        pointer_y: f32,
        body_top: f32,
        scroll_offset_y: f32,
    ) -> DragMove {
        let Some(drag) = self
            .drag
            .as_mut()
            .filter(|drag| drag.provider == provider && drag.instrument_id == instrument_id)
        else {
            return DragMove::Unplaced;
        };
        drag.pointer_y = Some(pointer_y);
        drag.body_top = body_top;
        drag.scroll_offset_y = scroll_offset_y;
        let cursor_offset_y = drag.cursor_offset_y;
        // A flag filter hides rows, so the drop slot is the visible row's place in the list.
        let visible = self.visible_indices();
        let Some(destination) = watchlist_drag_destination(
            pointer_y,
            body_top,
            scroll_offset_y,
            cursor_offset_y,
            visible.len(),
        )
        .and_then(|row| visible.get(row).copied()) else {
            return DragMove::Unplaced;
        };
        let Some(source) = self.position(provider, instrument_id) else {
            return DragMove::Unplaced;
        };
        DragMove::Placed {
            reordered: move_item(&mut self.entries, source, destination),
        }
    }

    /// Ends the row drag; reports whether one was in progress.
    pub(super) fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    pub(super) fn flag(&self, provider: &str, instrument_id: &str) -> Option<&str> {
        self.position(provider, instrument_id)
            .and_then(|index| self.entries[index].flag.as_deref())
    }

    /// An unflagged row takes `default_color`; a flagged row loses its flag and its picker.
    pub(super) fn press_flag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        default_color: String,
    ) -> FlagPress {
        let Some(index) = self.position(provider, instrument_id) else {
            return FlagPress::Missing;
        };
        let entry = &mut self.entries[index];
        if entry.flag.take().is_none() {
            entry.flag = Some(default_color);
            return FlagPress::Flagged;
        }
        if self.picker_is_for(provider, instrument_id) {
            self.flags.picker = None;
        }
        self.release_unused_filter();
        FlagPress::Cleared
    }

    /// Flags an unflagged row with `default_color`; reports whether the row is listed.
    pub(super) fn ensure_flag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        default_color: String,
    ) -> bool {
        let Some(index) = self.position(provider, instrument_id) else {
            return false;
        };
        self.entries[index].flag.get_or_insert(default_color);
        true
    }

    pub(super) fn picker_is_for(&self, provider: &str, instrument_id: &str) -> bool {
        self.flags.picker.as_ref().is_some_and(|picker| {
            picker.provider == provider && picker.instrument_id == instrument_id
        })
    }

    /// Opens a row's flag color picker, closing the filter menu.
    pub(super) fn open_picker(&mut self, picker: WatchlistFlagPicker) {
        self.flags.filter_menu_open = false;
        self.flags.picker = Some(picker);
    }

    /// Recolors the flag whose picker is open.
    pub(super) fn apply_picker_color(&mut self, color: &str) -> PickerColor {
        let Some(picker) = self.flags.picker.as_mut() else {
            return PickerColor::Closed;
        };
        let Some(color) = normalize_hex_color(color) else {
            picker.error = Some("Enter a hex color such as #2962FF or #2962FF80.".to_string());
            return PickerColor::Rejected;
        };
        picker.error = None;
        let picker = picker.clone();
        let Some(entry) = self.entries.iter_mut().find(|entry| picker.is_for(entry)) else {
            return PickerColor::Accepted;
        };
        entry.flag = Some(color);
        PickerColor::Applied
    }

    /// Closes the flag picker; reports whether one was open.
    pub(super) fn close_picker(&mut self) -> bool {
        if self.flags.picker.take().is_none() {
            return false;
        }
        self.release_unused_filter();
        true
    }

    pub(super) const fn toggle_filter_menu(&mut self) {
        self.flags.filter_menu_open = !self.flags.filter_menu_open;
    }

    /// Closes the flag filter menu; reports whether it was open.
    pub(super) const fn close_filter_menu(&mut self) -> bool {
        std::mem::replace(&mut self.flags.filter_menu_open, false)
    }

    /// Shows only the rows flagged with `filter`, or every row for `None`.
    pub(super) fn set_filter(&mut self, filter: Option<String>) {
        self.flags.filter_menu_open = false;
        self.flags.filter = filter;
        self.release_unused_filter();
    }

    pub(super) const fn summaries(&self) -> &BTreeMap<MarketSummaryKey, MarketSummaryEntry> {
        &self.summaries
    }

    pub(super) const fn summaries_mut(
        &mut self,
    ) -> &mut BTreeMap<MarketSummaryKey, MarketSummaryEntry> {
        &mut self.summaries
    }

    /// Removes and returns the summaries `keep` no longer wants, so their workers can retire.
    pub(super) fn take_stale_summaries(
        &mut self,
        keep: impl Fn(&MarketSummaryKey) -> bool,
    ) -> Vec<MarketSummaryEntry> {
        let stale = self
            .summaries
            .keys()
            .filter(|key| !keep(key))
            .cloned()
            .collect::<Vec<_>>();
        stale
            .iter()
            .filter_map(|key| self.summaries.remove(key))
            .collect()
    }

    fn position(&self, provider: &str, instrument_id: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.is(provider, instrument_id))
    }

    /// Ends a flag filter once no row carries its color, so it never leaves the list empty.
    fn release_unused_filter(&mut self) {
        let unused = self.flags.filter.as_ref().is_some_and(|filter| {
            !self
                .entries
                .iter()
                .any(|entry| entry.flag.as_ref() == Some(filter))
        });
        if unused {
            self.flags.filter = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::local_state::fixture_workspace;
    use super::*;

    fn instrument(id: &str) -> InstallProviderInstrument {
        let mut instrument = fixture_workspace().watchlist_entries[0]
            .instrument
            .clone()
            .expect("fixture instrument");
        instrument.instrument_id = id.to_string();
        instrument
    }

    fn listed(cx: &mut gpui::TestAppContext, ids: &[&str]) -> MarketWatch {
        let mut watch = MarketWatch::restore(Vec::new(), cx.update(|cx| cx.focus_handle()));
        for id in ids {
            assert!(watch.add(instrument(id)));
        }
        watch
    }

    fn ids(watch: &MarketWatch) -> Vec<&str> {
        watch
            .entries()
            .iter()
            .map(|entry| entry.instrument.instrument_id.as_str())
            .collect()
    }

    #[gpui::test]
    fn restore_drops_rows_without_an_instrument_and_normalizes_flags(
        cx: &mut gpui::TestAppContext,
    ) {
        let focus = cx.update(|cx| cx.focus_handle());
        let watch = MarketWatch::restore(
            vec![
                WorkspaceWatchlistEntryState {
                    instrument: Some(instrument("a")),
                    flag_color: "#f23645".to_string(),
                },
                WorkspaceWatchlistEntryState {
                    instrument: None,
                    flag_color: String::new(),
                },
                WorkspaceWatchlistEntryState {
                    instrument: Some(instrument("b")),
                    flag_color: "not a color".to_string(),
                },
            ],
            focus,
        );
        assert_eq!(ids(&watch), ["a", "b"]);
        let durable = watch.durable_entries();
        assert_eq!(durable[0].flag_color, "#F23645");
        assert_eq!(durable[1].flag_color, "", "an invalid flag is dropped");
    }

    #[gpui::test]
    fn add_refuses_duplicates_and_a_full_list(cx: &mut gpui::TestAppContext) {
        let mut watch = listed(cx, &["a"]);
        assert!(
            !watch.add(instrument("a")),
            "a listed instrument is not added twice"
        );
        for index in 1..MAXIMUM_WATCHLIST_ENTRIES {
            assert!(watch.add(instrument(&format!("row-{index}"))));
        }
        assert!(
            !watch.add(instrument("overflow")),
            "a full list refuses more rows"
        );
        assert_eq!(watch.entries().len(), MAXIMUM_WATCHLIST_ENTRIES);
    }

    #[gpui::test]
    fn clearing_or_removing_the_last_filtered_flag_ends_the_filter(cx: &mut gpui::TestAppContext) {
        let mut watch = listed(cx, &["a", "b"]);
        let provider = watch.entries()[0].instrument.provider.clone();
        assert_eq!(
            watch.press_flag(&provider, "a", "#335CFF".to_string()),
            FlagPress::Flagged
        );
        watch.set_filter(Some("#335CFF".to_string()));
        assert_eq!(watch.visible_indices(), [0]);
        assert_eq!(
            watch.press_flag(&provider, "a", "#335CFF".to_string()),
            FlagPress::Cleared
        );
        assert!(watch.flags_view().filter.is_none());
        assert_eq!(watch.visible_indices(), [0, 1]);

        assert!(watch.ensure_flag(&provider, "b", "#F23645".to_string()));
        watch.set_filter(Some("#F23645".to_string()));
        assert!(watch.remove(&provider, "b"));
        assert!(watch.flags_view().filter.is_none());
        assert!(
            !watch.remove(&provider, "b"),
            "an unlisted row is not removed"
        );
        assert_eq!(
            watch.press_flag(&provider, "b", "#335CFF".to_string()),
            FlagPress::Missing
        );
    }

    #[gpui::test]
    fn a_drag_reorders_only_its_own_row(cx: &mut gpui::TestAppContext) {
        let mut watch = listed(cx, &["a", "b", "c"]);
        let provider = watch.entries()[0].instrument.provider.clone();
        assert!(!watch.begin_drag(&provider, "missing", 0.0));
        assert!(watch.begin_drag(&provider, "a", 0.0));
        assert_eq!(
            watch.move_drag(&provider, "b", 2.0 * WATCHLIST_ROW_HEIGHT, 0.0, 0.0),
            DragMove::Unplaced,
            "another row's pointer events do not move the dragged row"
        );
        assert_eq!(
            watch.move_drag(&provider, "a", 2.0 * WATCHLIST_ROW_HEIGHT, 0.0, 0.0),
            DragMove::Placed { reordered: true }
        );
        assert_eq!(ids(&watch), ["b", "c", "a"]);
        assert_eq!(
            watch.move_drag(&provider, "a", 2.0 * WATCHLIST_ROW_HEIGHT, 0.0, 0.0),
            DragMove::Placed { reordered: false }
        );
        assert!(watch.end_drag());
        assert!(!watch.end_drag());
    }

    #[gpui::test]
    fn stale_summaries_are_taken_out(cx: &mut gpui::TestAppContext) {
        let mut watch = listed(cx, &[]);
        for id in ["a", "b"] {
            let instrument = instrument(id);
            watch.summaries_mut().insert(
                MarketSummaryKey::from_instrument(&instrument),
                MarketSummaryEntry::new(
                    instrument,
                    None,
                    None,
                    super::super::ConsumerResourceClass::Background,
                ),
            );
        }
        let stale = watch.take_stale_summaries(|key| key.instrument_id == "a");
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].instrument.instrument_id, "b");
        assert_eq!(watch.summaries().len(), 1);
    }
}
