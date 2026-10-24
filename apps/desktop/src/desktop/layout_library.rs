//! The library of user-named saved layouts.
//!
//! Every workspace tab shows one saved layout. A layout keeps the pane arrangement it was last
//! saved with and the drawings of every instrument shown on it; the tabs showing one layout
//! share its working drawing set (`chart_drawings::LayoutDrawings`). A layout with autosave on
//! is saved on every change. One with autosave off changes only on an explicit save, and a
//! restart reopens its last saved version.

use std::collections::BTreeSet;
use std::sync::Arc;

use aeris_contracts::{
    WorkspaceInstrumentDrawingsState, WorkspaceLayoutState, WorkspacePaneState,
    WorkspaceSavedLayoutState, WorkspaceState, WorkspaceTabState,
};
use aeris_observability::diagnostic;

use super::chart_drawings::{bounded_instrument_drawings, legacy_pane_drawings};

pub(super) const MAXIMUM_SAVED_LAYOUTS: usize = 100;
pub(super) const MAXIMUM_LAYOUT_NAME_CHARS: usize = 64;
/// Drawing-document bytes the whole library keeps, so it stays inside the workspace file bound.
pub(super) const MAXIMUM_SAVED_LAYOUT_DRAWING_BYTES: usize = 8 * 1_048_576;
pub(super) const UNTITLED_LAYOUT_NAME: &str = "Untitled";

/// The saved layouts, in creation order. Each layout is shared with pending persistence
/// requests, so a save clones only the layout it changes.
#[derive(Clone, Debug, Default)]
pub(super) struct LayoutLibrary {
    layouts: Vec<Arc<WorkspaceSavedLayoutState>>,
    /// Advances on every change, so persistence notices one without comparing layouts.
    revision: u64,
}

impl LayoutLibrary {
    /// The library of a sanitized workspace.
    pub(super) fn new(layouts: Vec<WorkspaceSavedLayoutState>) -> Self {
        Self {
            layouts: layouts.into_iter().map(Arc::new).collect(),
            revision: 0,
        }
    }

    pub(super) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn states(&self) -> Vec<Arc<WorkspaceSavedLayoutState>> {
        self.layouts.clone()
    }

    pub(super) fn get(&self, layout_id: u64) -> Option<&WorkspaceSavedLayoutState> {
        self.layouts
            .iter()
            .find(|layout| layout.layout_id == layout_id)
            .map(AsRef::as_ref)
    }

    fn index(&self, layout_id: u64) -> Result<usize, String> {
        self.layouts
            .iter()
            .position(|layout| layout.layout_id == layout_id)
            .ok_or_else(|| "The layout no longer exists".to_string())
    }

    fn changed(&mut self, index: usize) -> &mut WorkspaceSavedLayoutState {
        self.revision = self.revision.wrapping_add(1);
        Arc::make_mut(&mut self.layouts[index])
    }

    fn name_taken(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.layouts
            .iter()
            .any(|layout| layout.name.to_lowercase() == name)
    }

    /// Checks that a new layout named `name` can be created.
    ///
    /// # Errors
    ///
    /// Returns an error when the library is full or the name is invalid or taken.
    pub(super) fn check_new_layout(&self, name: &str) -> Result<(), String> {
        if self.layouts.len() >= MAXIMUM_SAVED_LAYOUTS {
            return Err(full_library_message());
        }
        self.validated_name(name).map(|_| ())
    }

    /// `name` trimmed, when it is a valid name no layout uses.
    fn validated_name(&self, name: &str) -> Result<String, String> {
        let name = trimmed_name(name)?;
        if self.name_taken(name) {
            return Err(format!("A layout named \"{name}\" already exists"));
        }
        Ok(name.to_string())
    }

    /// `base` when no layout uses it, otherwise `base` numbered from 2. An invalid base is
    /// replaced by `UNTITLED_LAYOUT_NAME`.
    pub(super) fn unique_name(&self, base: &str) -> String {
        let base = trimmed_name(base).unwrap_or(UNTITLED_LAYOUT_NAME);
        if !self.name_taken(base) {
            return base.to_string();
        }
        // One more candidate than there are layouts, so one is always free.
        for number in 2..=self.layouts.len().saturating_add(2) {
            let suffix = format!(" {number}");
            let stem = base
                .chars()
                .take(MAXIMUM_LAYOUT_NAME_CHARS.saturating_sub(suffix.len()))
                .collect::<String>();
            let candidate = format!("{}{suffix}", stem.trim_end());
            if !self.name_taken(&candidate) {
                return candidate;
            }
        }
        format!("{UNTITLED_LAYOUT_NAME} {}", self.next_id())
    }

    fn next_id(&self) -> u64 {
        self.layouts
            .iter()
            .map(|layout| layout.layout_id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    fn checked_drawings(
        &self,
        replacing: Option<u64>,
        drawings: Vec<WorkspaceInstrumentDrawingsState>,
    ) -> Result<Vec<WorkspaceInstrumentDrawingsState>, String> {
        let drawings = bounded_instrument_drawings(drawings);
        let others = self
            .layouts
            .iter()
            .filter(|layout| Some(layout.layout_id) != replacing)
            .map(|layout| drawing_bytes(&layout.instrument_drawings))
            .sum::<usize>();
        if others.saturating_add(drawing_bytes(&drawings)) > MAXIMUM_SAVED_LAYOUT_DRAWING_BYTES {
            return Err(format!(
                "Saved layouts can keep at most {} MiB of drawings",
                MAXIMUM_SAVED_LAYOUT_DRAWING_BYTES / 1_048_576
            ));
        }
        Ok(drawings)
    }

    /// Saves a new layout with autosave on and returns its identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the library is full, the name is invalid or taken, or the
    /// drawings do not fit the library's storage bound.
    pub(super) fn create(
        &mut self,
        name: &str,
        snapshot: &WorkspaceTabState,
        drawings: Vec<WorkspaceInstrumentDrawingsState>,
        now_unix_ms: i64,
    ) -> Result<u64, String> {
        self.check_new_layout(name)?;
        let name = self.validated_name(name)?;
        let instrument_drawings = self.checked_drawings(None, drawings)?;
        let layout_id = self.next_id();
        if layout_id == u64::MAX {
            return Err("The layout identity space is exhausted".to_string());
        }
        self.layouts.push(Arc::new(WorkspaceSavedLayoutState {
            layout_id,
            name,
            autosave: true,
            saved_at_unix_ms: now_unix_ms,
            last_opened_unix_ms: now_unix_ms,
            tab: Some(normalized_layout_tab(snapshot)),
            instrument_drawings,
        }));
        self.revision = self.revision.wrapping_add(1);
        Ok(layout_id)
    }

    /// Saves `snapshot` and `drawings`, a working version of the layout, as a new layout with
    /// the same autosave choice.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or the copy cannot be created.
    pub(super) fn copy(
        &mut self,
        layout_id: u64,
        name: &str,
        snapshot: &WorkspaceTabState,
        drawings: Vec<WorkspaceInstrumentDrawingsState>,
        now_unix_ms: i64,
    ) -> Result<u64, String> {
        let autosave = self.layouts[self.index(layout_id)?].autosave;
        let copy = self.create(name, snapshot, drawings, now_unix_ms)?;
        self.set_autosave(copy, autosave)?;
        Ok(copy)
    }

    /// Renames the layout and returns the name it now has.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or the name is invalid or used by
    /// another layout.
    pub(super) fn rename(&mut self, layout_id: u64, name: &str) -> Result<String, String> {
        let index = self.check_rename(layout_id, name)?;
        let name = trimmed_name(name)?.to_string();
        if self.layouts[index].name != name {
            self.changed(index).name.clone_from(&name);
        }
        Ok(name)
    }

    /// Checks that the layout can be renamed to `name` and returns its index.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or the name is invalid or used by
    /// another layout.
    pub(super) fn check_rename(&self, layout_id: u64, name: &str) -> Result<usize, String> {
        let index = self.index(layout_id)?;
        let name = trimmed_name(name)?;
        let lowered = name.to_lowercase();
        if self
            .layouts
            .iter()
            .any(|layout| layout.layout_id != layout_id && layout.name.to_lowercase() == lowered)
        {
            return Err(format!("A layout named \"{name}\" already exists"));
        }
        Ok(index)
    }

    /// Checks that the layout can be deleted and returns its index.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or a tab still shows it.
    pub(super) fn check_delete(
        &self,
        layout_id: u64,
        open_layout_ids: &[u64],
    ) -> Result<usize, String> {
        let index = self.index(layout_id)?;
        if open_layout_ids.contains(&layout_id) {
            return Err(format!(
                "\"{}\" is open in a tab. Close it before deleting the layout.",
                self.layouts[index].name
            ));
        }
        Ok(index)
    }

    /// # Errors
    ///
    /// Returns an error when the layout does not exist or a tab still shows it.
    pub(super) fn delete(&mut self, layout_id: u64, open_layout_ids: &[u64]) -> Result<(), String> {
        let index = self.check_delete(layout_id, open_layout_ids)?;
        self.layouts.remove(index);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Replaces the layout's saved pane arrangement and drawings.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or the drawings do not fit the library's
    /// storage bound.
    pub(super) fn save(
        &mut self,
        layout_id: u64,
        snapshot: &WorkspaceTabState,
        drawings: Vec<WorkspaceInstrumentDrawingsState>,
        now_unix_ms: i64,
    ) -> Result<(), String> {
        let index = self.index(layout_id)?;
        let instrument_drawings = self.checked_drawings(Some(layout_id), drawings)?;
        let tab = Some(normalized_layout_tab(snapshot));
        let layout = &self.layouts[index];
        if layout.tab == tab && layout.instrument_drawings == instrument_drawings {
            return Ok(());
        }
        let layout = self.changed(index);
        layout.tab = tab;
        layout.instrument_drawings = instrument_drawings;
        layout.saved_at_unix_ms = now_unix_ms;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the layout does not exist.
    pub(super) fn set_autosave(&mut self, layout_id: u64, autosave: bool) -> Result<(), String> {
        let index = self.index(layout_id)?;
        if self.layouts[index].autosave != autosave {
            self.changed(index).autosave = autosave;
        }
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the layout does not exist.
    pub(super) fn touch_opened(&mut self, layout_id: u64, now_unix_ms: i64) -> Result<(), String> {
        let index = self.index(layout_id)?;
        self.changed(index).last_opened_unix_ms = now_unix_ms;
        Ok(())
    }

    /// At most `limit` layouts, most recently opened first.
    pub(super) fn recently_used(&self, limit: usize) -> Vec<&WorkspaceSavedLayoutState> {
        let mut layouts = self.layouts.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        layouts.sort_by(|a, b| {
            b.last_opened_unix_ms
                .cmp(&a.last_opened_unix_ms)
                .then(b.layout_id.cmp(&a.layout_id))
        });
        layouts.truncate(limit);
        layouts
    }

    fn into_states(self) -> Vec<WorkspaceSavedLayoutState> {
        self.layouts.into_iter().map(Arc::unwrap_or_clone).collect()
    }
}

pub(super) fn full_library_message() -> String {
    format!(
        "You can keep at most {MAXIMUM_SAVED_LAYOUTS} saved layouts. Delete one to create another."
    )
}

fn trimmed_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A layout needs a name".to_string());
    }
    if name.chars().count() > MAXIMUM_LAYOUT_NAME_CHARS {
        return Err(format!(
            "A layout name can have at most {MAXIMUM_LAYOUT_NAME_CHARS} characters"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("A layout name cannot contain control characters".to_string());
    }
    Ok(name)
}

fn drawing_bytes(drawings: &[WorkspaceInstrumentDrawingsState]) -> usize {
    drawings
        .iter()
        .map(|drawings| drawings.chart_state_json.len())
        .sum()
}

fn layout_leaf_ids(layout: &WorkspaceLayoutState, ids: &mut Vec<u64>) {
    match (&layout.first, &layout.second) {
        (Some(first), Some(second)) => {
            layout_leaf_ids(first, ids);
            layout_leaf_ids(second, ids);
        }
        _ if layout.pane_id != 0 => ids.push(layout.pane_id),
        _ => {}
    }
}

fn remapped_layout(
    layout: &WorkspaceLayoutState,
    pane_id: &impl Fn(u64) -> Option<u64>,
) -> Option<WorkspaceLayoutState> {
    let child = |child: &Option<Box<WorkspaceLayoutState>>| match child.as_deref() {
        None => Some(None),
        Some(child) => remapped_layout(child, pane_id).map(|child| Some(Box::new(child))),
    };
    Some(WorkspaceLayoutState {
        pane_id: if layout.pane_id == 0 {
            0
        } else {
            pane_id(layout.pane_id)?
        },
        first: child(&layout.first)?,
        second: child(&layout.second)?,
        ..layout.clone()
    })
}

/// The tab as a saved layout keeps it: its arrangement and pane presentation without the
/// identities, viewports and drawings that belong to one open tab. Panes are numbered from 1 in
/// layout order, so two tabs with one arrangement compare equal.
pub(super) fn normalized_layout_tab(tab: &WorkspaceTabState) -> WorkspaceTabState {
    let mut order = Vec::new();
    match &tab.layout {
        Some(layout) => layout_leaf_ids(layout, &mut order),
        None => order.extend(tab.panes.iter().map(|pane| pane.pane_id)),
    }
    let ordinal = |pane_id: u64| {
        order
            .iter()
            .position(|id| *id == pane_id)
            .and_then(|index| u64::try_from(index + 1).ok())
    };
    let mut panes = tab
        .panes
        .iter()
        .filter_map(|pane| {
            let mut pane = WorkspacePaneState {
                pane_id: ordinal(pane.pane_id)?,
                consumer_id: 0,
                generation: 0,
                viewport_start_unix_nanos: None,
                viewport_end_unix_nanos: None,
                ..pane.clone()
            };
            if let Some(chart) = &mut pane.chart {
                chart.instrument_drawings.clear();
                chart.chart_state_json.clear();
                chart.locked_drawing_ids.clear();
            }
            Some(pane)
        })
        .collect::<Vec<_>>();
    panes.sort_by_key(|pane| pane.pane_id);
    WorkspaceTabState {
        workspace_id: 0,
        label: String::new(),
        split_axis: tab.split_axis,
        panes,
        active_pane_id: 0,
        generation: 0,
        layout: tab
            .layout
            .as_ref()
            .and_then(|layout| remapped_layout(layout, &ordinal)),
        saved_layout_id: 0,
    }
}

/// A saved layout's arrangement as an open tab whose panes take `pane_ids` (pane and consumer
/// identity, in the snapshot's pane order). `None` when the snapshot's layout names a pane it
/// does not hold or the identities do not match its panes.
pub(super) fn opened_layout_tab(
    snapshot: &WorkspaceTabState,
    pane_ids: &[(u64, u64)],
    workspace_id: u64,
    generation: u64,
) -> Option<WorkspaceTabState> {
    if snapshot.panes.is_empty() || pane_ids.len() != snapshot.panes.len() {
        return None;
    }
    let generation = generation.max(1);
    let mapped = |pane_id: u64| {
        snapshot
            .panes
            .iter()
            .position(|pane| pane.pane_id == pane_id)
            .map(|index| pane_ids[index].0)
    };
    let layout = match &snapshot.layout {
        Some(layout) => Some(remapped_layout(layout, &mapped)?),
        None => None,
    };
    let panes = snapshot
        .panes
        .iter()
        .zip(pane_ids)
        .map(|(pane, (pane_id, consumer_id))| WorkspacePaneState {
            pane_id: *pane_id,
            consumer_id: *consumer_id,
            generation,
            ..pane.clone()
        })
        .collect::<Vec<_>>();
    Some(WorkspaceTabState {
        workspace_id,
        active_pane_id: mapped(snapshot.active_pane_id).unwrap_or(panes[0].pane_id),
        panes,
        generation,
        layout,
        ..snapshot.clone()
    })
}

/// Moves the drawings a pre-library tab kept per pane into one set: the active pane's first,
/// then the others', each pane's legacy drawings after its per-instrument ones.
fn take_pane_drawings(tab: &mut WorkspaceTabState) -> Vec<WorkspaceInstrumentDrawingsState> {
    let mut order = (0..tab.panes.len()).collect::<Vec<_>>();
    if let Some(active) = tab
        .panes
        .iter()
        .position(|pane| pane.pane_id == tab.active_pane_id)
    {
        order.remove(active);
        order.insert(0, active);
    }
    let mut merged = Vec::new();
    for index in order {
        let pane = &mut tab.panes[index];
        let Some(chart) = pane.chart.as_mut() else {
            continue;
        };
        merged.append(&mut chart.instrument_drawings);
        merged.extend(legacy_pane_drawings(
            &chart.chart_state_json,
            &chart.locked_drawing_ids,
            pane.instrument.as_ref(),
        ));
        chart.chart_state_json.clear();
        chart.locked_drawing_ids.clear();
    }
    bounded_instrument_drawings(merged)
}

/// Keeps at most `MAXIMUM_SAVED_LAYOUTS`: every layout a tab shows, then the most recently
/// opened, in their saved order.
fn bound_layout_count(layouts: &mut Vec<WorkspaceSavedLayoutState>, open: &BTreeSet<u64>) {
    if layouts.len() <= MAXIMUM_SAVED_LAYOUTS {
        return;
    }
    let mut ranked = layouts
        .iter()
        .map(|layout| {
            (
                !open.contains(&layout.layout_id),
                std::cmp::Reverse(layout.last_opened_unix_ms),
                layout.layout_id,
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable();
    let kept = ranked
        .into_iter()
        .take(MAXIMUM_SAVED_LAYOUTS)
        .map(|(_, _, layout_id)| layout_id)
        .collect::<BTreeSet<_>>();
    let dropped = layouts.len() - kept.len();
    layouts.retain(|layout| kept.contains(&layout.layout_id));
    diagnostic!("Aeris dropped {dropped} least recently opened saved layouts");
}

/// Clears the drawings of the least recently opened layouts, those no tab shows first, until
/// the library fits `MAXIMUM_SAVED_LAYOUT_DRAWING_BYTES`.
fn bound_library_drawing_bytes(layouts: &mut [WorkspaceSavedLayoutState], open: &BTreeSet<u64>) {
    let mut total = layouts
        .iter()
        .map(|layout| drawing_bytes(&layout.instrument_drawings))
        .sum::<usize>();
    if total <= MAXIMUM_SAVED_LAYOUT_DRAWING_BYTES {
        return;
    }
    let mut order = (0..layouts.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let layout = &layouts[*index];
        (
            open.contains(&layout.layout_id),
            layout.last_opened_unix_ms,
            layout.layout_id,
        )
    });
    for index in order {
        if total <= MAXIMUM_SAVED_LAYOUT_DRAWING_BYTES {
            break;
        }
        let layout = &mut layouts[index];
        total -= drawing_bytes(&layout.instrument_drawings);
        layout.instrument_drawings.clear();
        diagnostic!(
            "Aeris dropped the drawings of saved layout {} to fit the storage bound",
            layout.layout_id
        );
    }
}

/// Repairs the saved layouts of a loaded workspace and gives every tab one: a tab saved before
/// the layout library becomes an untitled layout with autosave on that holds its panes' drawings. Pane
/// drawings are cleared afterwards; the layout owns them. Panes must already be sanitized.
pub(super) fn sanitize_saved_layouts(workspace: &mut WorkspaceState) {
    let open = |tabs: &[WorkspaceTabState]| {
        tabs.iter()
            .map(|tab| tab.saved_layout_id)
            .filter(|layout_id| *layout_id != 0)
            .collect::<BTreeSet<_>>()
    };
    let mut identities = BTreeSet::new();
    let mut layouts = std::mem::take(&mut workspace.saved_layouts)
        .into_iter()
        .filter(|layout| layout.layout_id != 0 && identities.insert(layout.layout_id))
        .collect::<Vec<_>>();
    bound_layout_count(&mut layouts, &open(&workspace.workspace_tabs));
    let mut library = LayoutLibrary::default();
    for mut layout in layouts {
        layout.name = library.unique_name(&layout.name);
        layout.tab = Some(normalized_layout_tab(&layout.tab.unwrap_or_default()));
        layout.instrument_drawings =
            bounded_instrument_drawings(std::mem::take(&mut layout.instrument_drawings));
        library.layouts.push(Arc::new(layout));
    }
    for tab in &mut workspace.workspace_tabs {
        let drawings = take_pane_drawings(tab);
        if let Some(layout) = library.get(tab.saved_layout_id) {
            tab.label.clone_from(&layout.name);
            continue;
        }
        let name = library.unique_name(UNTITLED_LAYOUT_NAME);
        let layout_id = library.next_id();
        library.layouts.push(Arc::new(WorkspaceSavedLayoutState {
            layout_id,
            name: name.clone(),
            autosave: true,
            saved_at_unix_ms: 0,
            last_opened_unix_ms: 0,
            tab: Some(normalized_layout_tab(tab)),
            instrument_drawings: drawings,
        }));
        tab.saved_layout_id = layout_id;
        tab.label = name;
    }
    let mut layouts = library.into_states();
    let open = open(&workspace.workspace_tabs);
    bound_layout_count(&mut layouts, &open);
    bound_library_drawing_bytes(&mut layouts, &open);
    workspace.saved_layouts = layouts;
}

/// Reopens every tab whose layout has autosave off at the layout's last saved version: unsaved
/// changes are discarded on restart, as when the tab is closed without saving. The reopened
/// panes take identities above every identity the workspace holds.
pub(super) fn reopen_saved_manual_layouts(workspace: &mut WorkspaceState) {
    let panes = || {
        workspace
            .workspace_tabs
            .iter()
            .chain(
                workspace
                    .saved_layouts
                    .iter()
                    .filter_map(|layout| layout.tab.as_ref()),
            )
            .flat_map(|tab| &tab.panes)
    };
    let mut next_pane_id = panes().map(|pane| pane.pane_id).max().unwrap_or(0);
    let mut next_consumer_id = panes().map(|pane| pane.consumer_id).max().unwrap_or(0);
    for tab in &mut workspace.workspace_tabs {
        let Some(layout) = workspace
            .saved_layouts
            .iter()
            .find(|layout| layout.layout_id == tab.saved_layout_id && !layout.autosave)
        else {
            continue;
        };
        let Some(snapshot) = layout.tab.as_ref() else {
            continue;
        };
        let Some(pane_ids) = snapshot
            .panes
            .iter()
            .map(|_| {
                next_pane_id = next_pane_id.checked_add(1)?;
                next_consumer_id = next_consumer_id.checked_add(1)?;
                Some((next_pane_id, next_consumer_id))
            })
            .collect::<Option<Vec<_>>>()
        else {
            diagnostic!("Aeris workspace pane identities are exhausted");
            return;
        };
        let Some(mut reopened) =
            opened_layout_tab(snapshot, &pane_ids, tab.workspace_id, tab.generation)
        else {
            diagnostic!(
                "Aeris kept the working version of layout {} because its saved version is invalid",
                layout.layout_id
            );
            continue;
        };
        reopened.saved_layout_id = layout.layout_id;
        reopened.label.clone_from(&layout.name);
        *tab = reopened;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_contracts::{WorkspaceChartState, WorkspaceSplitAxis};

    fn pane(pane_id: u64, consumer_id: u64) -> WorkspacePaneState {
        WorkspacePaneState {
            pane_id,
            consumer_id,
            generation: 3,
            viewport_start_unix_nanos: Some(1),
            viewport_end_unix_nanos: Some(2),
            size_basis_points: 5_000,
            interval: "1m".to_string(),
            chart: Some(WorkspaceChartState {
                chart_type: "bars".to_string(),
                ..WorkspaceChartState::default()
            }),
            ..WorkspacePaneState::default()
        }
    }

    fn leaf(pane_id: u64) -> WorkspaceLayoutState {
        WorkspaceLayoutState {
            pane_id,
            ..WorkspaceLayoutState::default()
        }
    }

    /// Two panes whose layout order (`second`, `first`) differs from their pane order.
    fn split_tab(workspace_id: u64, first: u64, second: u64) -> WorkspaceTabState {
        WorkspaceTabState {
            workspace_id,
            label: format!("Tab {workspace_id}"),
            split_axis: WorkspaceSplitAxis::Vertical as i32,
            panes: vec![pane(first, first + 100), pane(second, second + 100)],
            active_pane_id: first,
            generation: 4,
            layout: Some(WorkspaceLayoutState {
                pane_id: 0,
                split_axis: WorkspaceSplitAxis::Vertical as i32,
                ratio_basis_points: 4_000,
                first: Some(Box::new(leaf(second))),
                second: Some(Box::new(leaf(first))),
            }),
            saved_layout_id: 0,
        }
    }

    #[test]
    fn layout_names_are_trimmed_bounded_and_unique_ignoring_case() {
        let mut library = LayoutLibrary::default();
        let tab = split_tab(1, 1, 2);
        assert_eq!(library.unique_name("  "), "Untitled");
        let first = library
            .create("  Untitled ", &tab, Vec::new(), 1)
            .expect("first");
        assert_eq!(library.get(first).expect("layout").name, "Untitled");
        assert_eq!(library.unique_name("untitled"), "untitled 2");
        library
            .create("Untitled 2", &tab, Vec::new(), 1)
            .expect("second");
        assert_eq!(library.unique_name("Untitled"), "Untitled 3");
        assert!(library.create("UNTITLED", &tab, Vec::new(), 1).is_err());
        assert!(library.create("", &tab, Vec::new(), 1).is_err());
        assert!(library.create("a\tb", &tab, Vec::new(), 1).is_err());
        let long = "x".repeat(MAXIMUM_LAYOUT_NAME_CHARS + 1);
        assert!(library.create(&long, &tab, Vec::new(), 1).is_err());
        let exact = "y".repeat(MAXIMUM_LAYOUT_NAME_CHARS);
        library
            .create(&exact, &tab, Vec::new(), 1)
            .expect("64 characters");
        let numbered = library.unique_name(&exact);
        assert_eq!(numbered.chars().count(), MAXIMUM_LAYOUT_NAME_CHARS);
        assert!(numbered.ends_with(" 2"));
    }

    #[test]
    fn a_full_library_refuses_new_layouts_and_keeps_every_saved_one() {
        let mut library = LayoutLibrary::default();
        let tab = split_tab(1, 1, 2);
        let mut ids = Vec::new();
        for index in 0..MAXIMUM_SAVED_LAYOUTS {
            let opened = i64::try_from(index).expect("small");
            ids.push(
                library
                    .create(&format!("Layout {index}"), &tab, Vec::new(), opened)
                    .expect("within the bound"),
            );
        }
        let revision = library.revision();
        assert_eq!(
            library.create("One more", &tab, Vec::new(), 1_000),
            Err(full_library_message())
        );
        assert_eq!(
            library.copy(ids[0], "A copy", &tab, Vec::new(), 1_000),
            Err(full_library_message())
        );
        assert_eq!(
            library.revision(),
            revision,
            "a refused layout changes nothing"
        );
        assert_eq!(library.states().len(), MAXIMUM_SAVED_LAYOUTS);
        assert!(ids.iter().all(|id| library.get(*id).is_some()));

        library.delete(ids[0], &[]).expect("deleted");
        library
            .create("One more", &tab, Vec::new(), 1_000)
            .expect("room after a delete");
    }

    #[test]
    fn renames_are_validated_and_deletes_refuse_open_layouts() {
        let mut library = LayoutLibrary::default();
        let tab = split_tab(1, 1, 2);
        let first = library.create("Scalping", &tab, Vec::new(), 1).expect("a");
        let second = library.create("Swing", &tab, Vec::new(), 1).expect("b");
        assert_eq!(
            library.rename(first, "  Day trading "),
            Ok("Day trading".to_string())
        );
        assert_eq!(library.get(first).expect("layout").name, "Day trading");
        assert_eq!(
            library.rename(first, "day TRADING"),
            Ok("day TRADING".to_string()),
            "a layout may change the case of its own name"
        );
        assert!(library.rename(second, "Day Trading").is_err());
        assert!(library.rename(second, "   ").is_err());
        assert!(
            library
                .rename(second, &"z".repeat(MAXIMUM_LAYOUT_NAME_CHARS + 1))
                .is_err()
        );
        assert_eq!(library.get(second).expect("layout").name, "Swing");

        let refused = library.delete(second, &[second]).expect_err("open");
        assert!(refused.contains("Swing"), "{refused}");
        assert!(library.get(second).is_some());
        library.delete(second, &[first]).expect("closed");
        assert!(library.get(second).is_none());
    }

    #[test]
    fn copies_keep_the_saved_version_and_recent_layouts_come_first() {
        let mut library = LayoutLibrary::default();
        let drawings = vec![WorkspaceInstrumentDrawingsState {
            provider: "hyperliquid".to_string(),
            instrument_id: "BTC".to_string(),
            chart_state_json: "{}".to_string(),
            locked_drawing_ids: Vec::new(),
        }];
        let original = library
            .create("Scalping", &split_tab(1, 1, 2), Vec::new(), 10)
            .expect("original");
        library.set_autosave(original, false).expect("manual");
        let mut working = split_tab(1, 1, 2);
        working.panes[0].size_basis_points = 6_000;
        let copy = library
            .copy(original, "Scalping copy", &working, drawings.clone(), 20)
            .expect("copy");
        let (saved_original, saved_copy) = (
            library.get(original).expect("original").clone(),
            library.get(copy).expect("copy").clone(),
        );
        assert_ne!(copy, original);
        assert_eq!(saved_copy.tab, Some(normalized_layout_tab(&working)));
        assert_ne!(
            saved_copy.tab, saved_original.tab,
            "the original keeps its version"
        );
        assert_eq!(saved_original.instrument_drawings, []);
        assert_eq!(saved_copy.instrument_drawings, drawings);
        assert!(!saved_copy.autosave);
        assert!(
            library
                .copy(original, "scalping COPY", &working, Vec::new(), 30)
                .is_err()
        );

        let third = library
            .create("Swing", &split_tab(1, 1, 2), Vec::new(), 15)
            .expect("third");
        library.touch_opened(original, 40).expect("opened");
        let recent = library
            .recently_used(2)
            .iter()
            .map(|layout| layout.layout_id)
            .collect::<Vec<_>>();
        assert_eq!(recent, [original, copy]);
        assert_eq!(library.recently_used(10).len(), 3);
        assert!(library.touch_opened(third + 1, 1).is_err());
    }

    #[test]
    fn saving_unchanged_state_is_no_change() {
        let mut library = LayoutLibrary::default();
        let tab = split_tab(1, 1, 2);
        let layout = library.create("A", &tab, Vec::new(), 1).expect("layout");
        let revision = library.revision();
        library
            .save(layout, &split_tab(9, 7, 8), Vec::new(), 2)
            .expect("same arrangement");
        assert_eq!(library.revision(), revision);
        assert_eq!(library.get(layout).expect("layout").saved_at_unix_ms, 1);
        let mut resized = tab;
        resized.panes[0].size_basis_points = 6_000;
        library
            .save(layout, &resized, Vec::new(), 3)
            .expect("changed");
        assert_ne!(library.revision(), revision);
        assert_eq!(library.get(layout).expect("layout").saved_at_unix_ms, 3);
    }

    #[test]
    fn normalized_tabs_drop_open_tab_identity_and_number_panes_in_layout_order() {
        let normalized = normalized_layout_tab(&split_tab(5, 7, 9));
        assert_eq!(normalized, normalized_layout_tab(&split_tab(1, 3, 4)));
        assert_eq!(normalized, normalized_layout_tab(&normalized));
        assert_eq!(
            normalized
                .panes
                .iter()
                .map(|pane| (
                    pane.pane_id,
                    pane.consumer_id,
                    pane.viewport_start_unix_nanos
                ))
                .collect::<Vec<_>>(),
            [(1, 0, None), (2, 0, None)]
        );
        let layout = normalized.layout.as_ref().expect("layout");
        assert_eq!(layout.first.as_ref().expect("first").pane_id, 1);
        assert_eq!(layout.second.as_ref().expect("second").pane_id, 2);
        assert_eq!((normalized.workspace_id, normalized.active_pane_id), (0, 0));
    }

    #[test]
    fn manual_layouts_reopen_their_saved_version_with_fresh_unique_identities() {
        let mut working = split_tab(3, 7, 8);
        working.saved_layout_id = 1;
        let mut autosaved = split_tab(4, 20, 21);
        autosaved.saved_layout_id = 2;
        let mut saved = split_tab(0, 1, 2);
        saved.panes[0].size_basis_points = 7_000;
        let mut workspace = WorkspaceState {
            workspace_tabs: vec![working.clone(), autosaved.clone()],
            saved_layouts: vec![
                WorkspaceSavedLayoutState {
                    layout_id: 1,
                    name: "Manual".to_string(),
                    autosave: false,
                    tab: Some(normalized_layout_tab(&saved)),
                    ..WorkspaceSavedLayoutState::default()
                },
                WorkspaceSavedLayoutState {
                    layout_id: 2,
                    name: "Auto".to_string(),
                    autosave: true,
                    tab: Some(normalized_layout_tab(&split_tab(0, 30, 31))),
                    ..WorkspaceSavedLayoutState::default()
                },
            ],
            ..WorkspaceState::default()
        };
        reopen_saved_manual_layouts(&mut workspace);
        assert_eq!(
            workspace.workspace_tabs[1], autosaved,
            "autosaved tabs stay"
        );
        let reopened = &workspace.workspace_tabs[0];
        assert_eq!(
            (
                reopened.workspace_id,
                reopened.saved_layout_id,
                reopened.label.as_str()
            ),
            (3, 1, "Manual")
        );
        assert_eq!(
            normalized_layout_tab(reopened),
            normalized_layout_tab(&saved),
            "the saved version reopens"
        );
        let ids = reopened
            .panes
            .iter()
            .map(|pane| (pane.pane_id, pane.consumer_id))
            .collect::<Vec<_>>();
        assert_eq!(ids, [(22, 122), (23, 123)], "above every identity held");
        let mut leaves = Vec::new();
        layout_leaf_ids(reopened.layout.as_ref().expect("layout"), &mut leaves);
        assert_eq!(leaves, [22, 23], "the layout follows its panes");
        assert_eq!(reopened.active_pane_id, 22);
        assert!(reopened.panes.iter().all(|pane| pane.generation == 4));
    }
}
