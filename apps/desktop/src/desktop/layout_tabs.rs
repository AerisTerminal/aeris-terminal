//! The saved layouts open tabs show: the working drawing set the tabs of one layout share,
//! autosave, and the layout commands.

use super::*;
use aeris_desktop::command_registry::CommandId;
use aeris_observability::diagnostic;
use layout_library::{
    MAXIMUM_LAYOUT_NAME_CHARS, MAXIMUM_SAVED_LAYOUTS, normalized_layout_tab, opened_layout_tab,
};

const COPY_SUFFIX: &str = " copy";

pub(super) fn now_unix_ms(lifecycle: &DesktopLifecycle) -> i64 {
    lifecycle.clock().wall_unix_nanos().div_euclid(1_000_000)
}

/// The library of a starting shell and the working drawing sets of the layouts its tabs show.
/// A tab showing no layout of the library is given a new one.
pub(super) fn restored_layouts(
    saved: Vec<WorkspaceSavedLayoutState>,
    tabs: &mut TabStrip,
    now_unix_ms: i64,
    cx: &App,
) -> (LayoutLibrary, BTreeMap<u64, LayoutDrawings>) {
    let mut library = LayoutLibrary::new(saved);
    let mut drawings = BTreeMap::new();
    for workspace in tabs.iter_mut() {
        let snapshot = normalized_layout_tab(&workspace_tab_state(workspace, cx));
        if library.get(workspace.saved_layout_id).is_none() {
            let name = library.unique_name(layout_library::UNTITLED_LAYOUT_NAME);
            match library.create(&name, &snapshot, Vec::new(), now_unix_ms) {
                Ok(layout_id) => {
                    workspace.saved_layout_id = layout_id;
                    workspace.label = name;
                }
                Err(error) => {
                    diagnostic!("Aeris workspace tab could not be given a layout: {error}");
                    continue;
                }
            }
        }
        workspace.autosaved_snapshot = Some(snapshot);
        if let Some(layout) = library.get(workspace.saved_layout_id) {
            drawings
                .entry(layout.layout_id)
                .or_insert_with(|| LayoutDrawings::from_saved(&layout.instrument_drawings));
        }
    }
    (library, drawings)
}

pub(super) fn open_layout_ids(workspaces: &[WorkspaceTab]) -> Vec<u64> {
    workspaces
        .iter()
        .map(|workspace| workspace.saved_layout_id)
        .collect()
}

/// Saves one tab of a layout with autosave on into the library. The tab saves its arrangement
/// only when it changed since the tab last saved it (`autosaved`), so two tabs showing one
/// layout differently do not overwrite each other on every save: the last change wins.
/// Changed drawings are saved with the layout's saved arrangement otherwise. A layout with
/// autosave off is left alone.
fn autosave_tab(
    library: &mut LayoutLibrary,
    drawings: Option<&mut LayoutDrawings>,
    autosaved: &mut Option<WorkspaceTabState>,
    state: &WorkspaceTabState,
    now_unix_ms: i64,
) -> Result<(), String> {
    let Some(layout) = library
        .get(state.saved_layout_id)
        .filter(|layout| layout.autosave)
    else {
        return Ok(());
    };
    let snapshot = normalized_layout_tab(state);
    let arrangement_changed = autosaved.as_ref() != Some(&snapshot);
    let drawings_changed = drawings
        .as_ref()
        .is_some_and(|drawings| drawings.has_unsaved_changes());
    if !arrangement_changed && !drawings_changed {
        return Ok(());
    }
    let tab = if arrangement_changed {
        snapshot.clone()
    } else {
        layout.tab.clone().unwrap_or_default()
    };
    let entries = drawings.as_ref().map_or_else(
        || layout.instrument_drawings.clone(),
        |drawings| drawings.entries(),
    );
    library.save(state.saved_layout_id, &tab, entries, now_unix_ms)?;
    *autosaved = Some(snapshot);
    if let Some(drawings) = drawings {
        drawings.mark_saved();
    }
    Ok(())
}

/// Whether `state`, an open tab, differs from its layout's saved version.
fn differs_from_saved(
    library: &LayoutLibrary,
    drawings: Option<&LayoutDrawings>,
    state: &WorkspaceTabState,
) -> bool {
    let Some(layout) = library.get(state.saved_layout_id) else {
        return true;
    };
    layout.tab.as_ref() != Some(&normalized_layout_tab(state))
        || drawings.is_some_and(LayoutDrawings::has_unsaved_changes)
}

/// `name` followed by `suffix`, shortened to fit a layout name.
fn suffixed_layout_name(name: &str, suffix: &str) -> String {
    let stem = name
        .chars()
        .take(MAXIMUM_LAYOUT_NAME_CHARS.saturating_sub(suffix.chars().count()))
        .collect::<String>();
    format!("{}{suffix}", stem.trim_end())
}

impl TerminalApp {
    fn now_unix_ms(&self) -> i64 {
        now_unix_ms(&self.lifecycle)
    }

    /// Keeps every pane of every tab showing a layout on the layout's drawing set, and
    /// persists the workspace when a set changed.
    pub(super) fn sync_layout_drawings(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for (layout_id, drawings) in self.saved.layout_drawings_mut() {
            let panes = self
                .tabs
                .iter()
                .filter(|workspace| workspace.saved_layout_id == *layout_id)
                .flat_map(|workspace| workspace.panes.iter().map(|pane| &pane.surface));
            changed |= workspace_surface::sync_layout_drawings(&panes, drawings, cx);
        }
        if changed {
            self.persist_workspace_layout_if_changed(cx);
        }
    }

    /// Drops the working drawing sets of layouts no tab shows any more.
    pub(super) fn retain_open_layout_drawings(&mut self) {
        self.saved.layout_drawings_mut().retain(|layout_id, _| {
            self.tabs
                .iter()
                .any(|workspace| workspace.saved_layout_id == *layout_id)
        });
    }

    /// Saves every tab whose layout has autosave on. `tabs` is the state of
    /// `self.tabs`, in order.
    pub(super) fn autosave_layouts(&mut self, tabs: &[WorkspaceTabState]) {
        let now_unix_ms = self.now_unix_ms();
        for (workspace, state) in self.tabs.iter_mut().zip(tabs) {
            let (library, drawings) = self
                .saved
                .layout_with_drawings_mut(workspace.saved_layout_id);
            if let Err(error) = autosave_tab(
                library,
                drawings,
                &mut workspace.autosaved_snapshot,
                state,
                now_unix_ms,
            ) {
                self.notices.show_workspace_error(error);
            }
        }
    }

    /// Whether the tab at `index` differs from its layout's saved version. A layout with
    /// autosave on is saved with every change, so it never reports unsaved changes.
    pub(super) fn layout_tab_has_unsaved_changes(&self, index: usize, cx: &App) -> bool {
        self.tabs.get(index).is_some_and(|workspace| {
            !self
                .saved
                .layouts()
                .get(workspace.saved_layout_id)
                .is_some_and(|layout| layout.autosave)
                && differs_from_saved(
                    self.saved.layouts(),
                    self.saved.layout_drawings().get(&workspace.saved_layout_id),
                    &workspace_tab_state(workspace, cx),
                )
        })
    }

    /// Gives a new tab a new layout named `name` with autosave on, holding the tab's
    /// arrangement and no drawings.
    ///
    /// # Errors
    ///
    /// Returns an error when the library has no room for another layout or the name is
    /// invalid or taken.
    pub(super) fn assign_new_layout(
        &mut self,
        workspace: &mut WorkspaceTab,
        name: &str,
        cx: &App,
    ) -> Result<(), String> {
        let snapshot = normalized_layout_tab(&workspace_tab_state(workspace, cx));
        let now_unix_ms = self.now_unix_ms();
        let layout_id =
            self.saved
                .layouts_mut()
                .create(name, &snapshot, Vec::new(), now_unix_ms)?;
        workspace.saved_layout_id = layout_id;
        workspace.label = self
            .saved
            .layouts()
            .get(layout_id)
            .map_or_else(|| name.trim().to_string(), |layout| layout.name.clone());
        workspace.autosaved_snapshot = Some(snapshot);
        self.saved
            .layout_drawings_mut()
            .insert(layout_id, LayoutDrawings::default());
        Ok(())
    }

    pub(super) fn run_layout_command(
        &mut self,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match command {
            CommandId::SaveLayout => self.save_active_layout(cx),
            CommandId::ToggleLayoutAutosave => {
                let autosave = self
                    .saved
                    .layouts()
                    .get(self.tabs.active().saved_layout_id)
                    .is_some_and(|layout| layout.autosave);
                self.set_active_layout_autosave(!autosave, cx)
            }
            CommandId::CopyLayout => {
                self.open_layout_name_dialog(layouts_view::LayoutNamePurpose::Copy, window, cx);
                Ok(())
            }
            CommandId::OpenRecentLayout => self.open_recent_layout(window, cx),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.notices.show_workspace_error(error);
            cx.notify();
        }
    }

    /// Saves the active tab's arrangement and its layout's drawings as the layout's saved
    /// version.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout no longer exists or its drawings do not fit the
    /// library's storage bound.
    pub(super) fn save_active_layout(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        self.save_layout_tab(self.tabs.active_index(), cx)
    }

    /// Saves the tab at `index` as its layout's saved version.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout no longer exists or its drawings do not fit the
    /// library's storage bound.
    pub(super) fn save_layout_tab(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.sync_layout_drawings(cx);
        let Some(workspace) = self.tabs.get(index) else {
            return Ok(());
        };
        if !differs_from_saved(
            self.saved.layouts(),
            self.saved.layout_drawings().get(&workspace.saved_layout_id),
            &workspace_tab_state(workspace, cx),
        ) {
            return Ok(());
        }
        let layout_id = workspace.saved_layout_id;
        let snapshot = normalized_layout_tab(&workspace_tab_state(workspace, cx));
        let entries = match self.saved.layout_drawings().get(&layout_id) {
            Some(drawings) => drawings.entries(),
            None => self
                .saved
                .layouts()
                .get(layout_id)
                .map(|layout| layout.instrument_drawings.clone())
                .unwrap_or_default(),
        };
        let now_unix_ms = self.now_unix_ms();
        let (library, drawings) = self.saved.layout_with_drawings_mut(layout_id);
        library.save(layout_id, &snapshot, entries, now_unix_ms)?;
        if let Some(drawings) = drawings {
            drawings.mark_saved();
        }
        self.tabs[index].autosaved_snapshot = Some(snapshot);
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(())
    }

    /// Turns autosave of the active tab's layout on or off. Turning it on saves the tab now.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout no longer exists.
    pub(super) fn set_active_layout_autosave(
        &mut self,
        autosave: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let layout_id = self.tabs.active().saved_layout_id;
        self.saved.layouts_mut().set_autosave(layout_id, autosave)?;
        if autosave {
            self.tabs.active_mut().autosaved_snapshot = None;
        }
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(())
    }

    /// The free name a copy of `name` is offered.
    pub(super) fn copy_layout_name(&self, name: &str) -> String {
        self.saved
            .layouts()
            .unique_name(&suffixed_layout_name(name, COPY_SUFFIX))
    }

    /// Saves the active tab as it is now as a new layout named `name`, which the tab then
    /// shows. The original layout keeps its saved version. Returns the new layout's identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the copy cannot be created.
    pub(super) fn copy_active_layout(
        &mut self,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<u64, String> {
        self.sync_layout_drawings(cx);
        let workspace = self.tabs.active();
        let original = workspace.saved_layout_id;
        let snapshot = normalized_layout_tab(&workspace_tab_state(workspace, cx));
        let entries = self
            .saved
            .layout_drawings()
            .get(&original)
            .map(LayoutDrawings::entries)
            .unwrap_or_default();
        let now_unix_ms = self.now_unix_ms();
        let copy =
            self.saved
                .layouts_mut()
                .copy(original, name, &snapshot, entries, now_unix_ms)?;
        let (label, drawings) = self.saved.layouts().get(copy).map_or_else(
            || (name.trim().to_string(), LayoutDrawings::default()),
            |layout| {
                (
                    layout.name.clone(),
                    LayoutDrawings::from_saved(&layout.instrument_drawings),
                )
            },
        );
        let workspace = self.tabs.active_mut();
        workspace.saved_layout_id = copy;
        workspace.label = label;
        workspace.autosaved_snapshot = Some(snapshot);
        for pane in &workspace.panes {
            pane.surface
                .update(cx, |surface, _| surface.rebind_layout_drawings(&drawings));
        }
        self.saved.layout_drawings_mut().insert(copy, drawings);
        self.retain_open_layout_drawings();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(copy)
    }

    /// Saves a copy of a layout's saved version under the next free copy name, without
    /// opening it. Returns the copy's identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or the copy cannot be created.
    pub(super) fn copy_saved_layout(
        &mut self,
        layout_id: u64,
        cx: &mut Context<Self>,
    ) -> Result<u64, String> {
        let layout = self
            .saved
            .layouts()
            .get(layout_id)
            .ok_or_else(|| "The layout no longer exists".to_string())?;
        let name = self.copy_layout_name(&layout.name);
        let (tab, drawings) = (
            layout.tab.clone().unwrap_or_default(),
            layout.instrument_drawings.clone(),
        );
        let now_unix_ms = self.now_unix_ms();
        let copy = self
            .saved
            .layouts_mut()
            .copy(layout_id, &name, &tab, drawings, now_unix_ms)?;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(copy)
    }

    /// Renames a layout and every tab showing it.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or the name is invalid or taken.
    pub(super) fn rename_layout(
        &mut self,
        layout_id: u64,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let name = self.saved.layouts_mut().rename(layout_id, name)?;
        for workspace in self.tabs.iter_mut() {
            if workspace.saved_layout_id == layout_id {
                workspace.label.clone_from(&name);
            }
        }
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(())
    }

    /// Deletes a saved layout no tab shows.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout does not exist or a tab shows it.
    pub(super) fn delete_saved_layout(
        &mut self,
        layout_id: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.saved
            .layouts_mut()
            .delete(layout_id, &open_layout_ids(self.tabs.as_slice()))?;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(())
    }

    /// The names of the layouts open tabs show with unsaved changes, once each, in tab order.
    pub(super) fn unsaved_layout_names(&self, cx: &App) -> Vec<String> {
        let mut layouts = Vec::new();
        let mut names = Vec::new();
        for (index, workspace) in self.tabs.iter().enumerate() {
            if !layouts.contains(&workspace.saved_layout_id)
                && self.layout_tab_has_unsaved_changes(index, cx)
            {
                layouts.push(workspace.saved_layout_id);
                names.push(workspace.label.clone());
            }
        }
        names
    }

    /// Retires market panes started for a tab that did not open.
    fn retire_market_panes(&self, panes: Vec<engine_market_worker::WorkspaceMarketPane>, cx: &App) {
        for mut pane in panes {
            if let Some(retirement) = pane.worker.begin_retirement() {
                self.lifecycle.retire_market_worker(retirement, cx);
            }
        }
    }

    /// Opens the most recently opened layout no tab shows.
    fn open_recent_layout(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let open = open_layout_ids(self.tabs.as_slice());
        let layout_id = self
            .saved
            .layouts()
            .recently_used(MAXIMUM_SAVED_LAYOUTS)
            .into_iter()
            .map(|layout| layout.layout_id)
            .find(|layout_id| !open.contains(layout_id))
            .ok_or_else(|| "Every saved layout is already open".to_string())?;
        self.open_saved_layout(layout_id, window, cx)
    }

    /// Opens a saved layout in a new tab at its saved version. A layout another tab already
    /// shows shares that tab's working drawings.
    ///
    /// # Errors
    ///
    /// Returns an error when no more tabs can open, the layout does not exist or is invalid,
    /// or its charts cannot start.
    pub(super) fn open_saved_layout(
        &mut self,
        layout_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.check_workspace_capacity()?;
        let factory = self
            .services
            .market_factory()
            .cloned()
            .ok_or_else(|| "This window cannot open a layout".to_string())?;
        let layout = self
            .saved
            .layouts()
            .get(layout_id)
            .ok_or_else(|| "The layout no longer exists".to_string())?;
        let (name, saved_drawings) = (layout.name.clone(), layout.instrument_drawings.clone());
        let snapshot = layout
            .tab
            .clone()
            .filter(|tab| !tab.panes.is_empty())
            .ok_or_else(|| format!("Layout \"{name}\" has no charts"))?;
        let charts = snapshot
            .panes
            .iter()
            .map(|pane| {
                let interval = aeris_market_runtime::persisted_interval(&pane.interval)
                    .unwrap_or(ChartInterval::Minute1);
                pane.instrument
                    .clone()
                    .map(|instrument| (instrument, interval))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("A chart of layout \"{name}\" has no market"))?;
        let mut market_panes =
            Vec::<engine_market_worker::WorkspaceMarketPane>::with_capacity(charts.len());
        for (instrument, interval) in charts {
            let created = match market_panes.first() {
                None => factory.create_workspace(instrument, interval),
                Some(first) => factory.create_pane(first.workspace_id, instrument, interval),
            };
            match created {
                Ok(pane) => market_panes.push(pane),
                Err(error) => {
                    self.retire_market_panes(market_panes, cx);
                    return Err(error);
                }
            }
        }
        let workspace_id = market_panes[0].workspace_id;
        let pane_ids = market_panes
            .iter()
            .map(|pane| (pane.pane_id, pane.consumer_id))
            .collect::<Vec<_>>();
        let Some(mut tab) = opened_layout_tab(&snapshot, &pane_ids, workspace_id, 1) else {
            self.retire_market_panes(market_panes, cx);
            return Err(format!("The arrangement of layout \"{name}\" is invalid"));
        };
        tab.saved_layout_id = layout_id;
        tab.label.clone_from(&name);
        let workspace = restored_workspace_tab(
            &tab,
            &mut market_panes,
            &self.lifecycle,
            *self.chrome.preferences(),
            window,
            cx,
        );
        self.retire_market_panes(market_panes, cx);
        let mut workspace = workspace.ok_or_else(|| format!("Layout \"{name}\" could not open"))?;
        workspace.autosaved_snapshot = Some(normalized_layout_tab(&tab));
        workspace.focus = workspace.focus.clone().tab_index(0).tab_stop(true);
        Self::wire_restored_surfaces(
            std::slice::from_ref(&workspace),
            0,
            self.frames.market(),
            cx,
        );
        for pane in &workspace.panes {
            pane.surface.update(cx, |surface, surface_cx| {
                surface.apply_theme(&self.theme, surface_cx);
            });
        }
        self.saved
            .layout_drawings_mut()
            .entry(layout_id)
            .or_insert_with(|| LayoutDrawings::from_saved(&saved_drawings));
        self.show_terminal_view(cx);
        self.set_workspace_resource_class(
            self.tabs.active_index(),
            ConsumerResourceClass::Background,
            cx,
        );
        self.tabs.push_active(workspace);
        let now_unix_ms = self.now_unix_ms();
        self.saved
            .layouts_mut()
            .touch_opened(layout_id, now_unix_ms)?;
        self.notices.clear_workspace_error();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(layout_id: u64, size_basis_points: u32) -> WorkspaceTabState {
        WorkspaceTabState {
            workspace_id: 7,
            label: "A".to_string(),
            panes: vec![WorkspacePaneState {
                pane_id: 3,
                consumer_id: 4,
                size_basis_points,
                interval: "1m".to_string(),
                ..WorkspacePaneState::default()
            }],
            active_pane_id: 3,
            generation: 1,
            layout: Some(WorkspaceLayoutState {
                pane_id: 3,
                ..WorkspaceLayoutState::default()
            }),
            saved_layout_id: layout_id,
            ..WorkspaceTabState::default()
        }
    }

    fn saved_size(library: &LayoutLibrary, layout_id: u64) -> u32 {
        library
            .get(layout_id)
            .and_then(|layout| layout.tab.as_ref())
            .map(|tab| tab.panes[0].size_basis_points)
            .expect("saved arrangement")
    }

    #[test]
    fn autosave_saves_each_tabs_own_changes_and_the_last_change_wins() {
        let mut library = LayoutLibrary::default();
        let layout_id = library
            .create("A", &tab(0, 5_000), Vec::new(), 1)
            .expect("layout");
        let (first, second) = (tab(layout_id, 5_000), tab(layout_id, 7_000));
        let mut first_saved = Some(normalized_layout_tab(&first));
        let mut second_saved = Some(normalized_layout_tab(&second));
        let revision = library.revision();
        autosave_tab(&mut library, None, &mut first_saved, &first, 2).expect("autosave");
        autosave_tab(&mut library, None, &mut second_saved, &second, 2).expect("autosave");
        assert_eq!(library.revision(), revision, "unchanged tabs save nothing");

        let first = tab(layout_id, 6_000);
        autosave_tab(&mut library, None, &mut first_saved, &first, 3).expect("autosave");
        autosave_tab(&mut library, None, &mut second_saved, &second, 3).expect("autosave");
        assert_eq!(saved_size(&library, layout_id), 6_000);
        assert!(!differs_from_saved(&library, None, &first));
        assert!(differs_from_saved(&library, None, &second));

        let second = tab(layout_id, 8_000);
        autosave_tab(&mut library, None, &mut second_saved, &second, 4).expect("autosave");
        autosave_tab(&mut library, None, &mut first_saved, &first, 4).expect("autosave");
        assert_eq!(
            saved_size(&library, layout_id),
            8_000,
            "the last change wins"
        );
        assert_eq!(library.get(layout_id).expect("layout").saved_at_unix_ms, 4);
    }

    #[test]
    fn a_manual_layout_changes_only_when_saved() {
        let mut library = LayoutLibrary::default();
        let layout_id = library
            .create("A", &tab(0, 5_000), Vec::new(), 1)
            .expect("layout");
        library.set_autosave(layout_id, false).expect("manual");
        let state = tab(layout_id, 6_000);
        let mut autosaved = None;
        let revision = library.revision();
        autosave_tab(&mut library, None, &mut autosaved, &state, 2).expect("no autosave");
        assert_eq!(library.revision(), revision);
        assert!(autosaved.is_none());
        assert!(
            differs_from_saved(&library, None, &state),
            "unsaved changes"
        );
        assert!(!differs_from_saved(&library, None, &tab(layout_id, 5_000)));

        library
            .save(layout_id, &state, Vec::new(), 3)
            .expect("explicit save");
        assert!(!differs_from_saved(&library, None, &state));
        assert!(differs_from_saved(
            &library,
            None,
            &tab(layout_id + 1, 6_000)
        ));
    }

    #[test]
    fn copy_names_fit_the_name_bound() {
        assert_eq!(
            suffixed_layout_name("Scalping", COPY_SUFFIX),
            "Scalping copy"
        );
        let long = suffixed_layout_name(&"x".repeat(MAXIMUM_LAYOUT_NAME_CHARS), COPY_SUFFIX);
        assert_eq!(long.chars().count(), MAXIMUM_LAYOUT_NAME_CHARS);
        assert!(long.ends_with(COPY_SUFFIX));
    }
}
