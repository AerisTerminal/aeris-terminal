//! Workspace tabs.

use super::*;

pub(super) fn move_item<T>(items: &mut Vec<T>, source: usize, destination: usize) -> bool {
    if items.is_empty() {
        return false;
    }
    let destination = destination.min(items.len() - 1);
    if source >= items.len() || source == destination {
        return false;
    }
    let item = items.remove(source);
    items.insert(destination, item);
    true
}

fn restore_watchlist(entries: Vec<WorkspaceWatchlistEntryState>) -> Vec<InstallProviderInstrument> {
    entries
        .into_iter()
        .filter_map(|entry| entry.instrument)
        .collect()
}

fn start_market_summary(
    factory: Option<&engine_market_worker::WorkspaceMarketFactory>,
    workspace_id: u64,
    instrument: InstallProviderInstrument,
    resource_class: ConsumerResourceClass,
    wake: &UiWake,
) -> MarketSummaryEntry {
    let worker = factory.and_then(|factory| {
        match factory.create_summary_worker(
            workspace_id,
            instrument.clone(),
            ChartInterval::Day1,
            resource_class,
        ) {
            Ok(worker) => {
                worker.set_message_wake(wake.callback());
                Some(worker)
            }
            Err(error) => {
                eprintln!("market summary demand could not start: {error}");
                None
            }
        }
    });
    let message = worker
        .is_none()
        .then(|| "Market data unavailable".to_string());
    MarketSummaryEntry::new(instrument, worker, message, resource_class)
}

pub(super) fn market_summary_instrument_for_switch(
    product: Option<&InstallProviderInstrument>,
    previous_selection: Option<&(Option<InstallProviderInstrument>, ChartInterval)>,
    showing_superseded: bool,
) -> Option<InstallProviderInstrument> {
    if showing_superseded {
        return previous_selection
            .and_then(|(instrument, _)| instrument.clone())
            .or_else(|| product.cloned());
    }
    product.cloned()
}

fn market_summary_tab_instrument(surface: &WorkspaceSurface) -> Option<InstallProviderInstrument> {
    market_summary_instrument_for_switch(
        surface.product.as_ref(),
        surface.rithmic_previous_selection.as_ref(),
        surface.showing_superseded_series(),
    )
}

pub(super) fn market_summary_requirements(
    tab_instruments: impl IntoIterator<Item = InstallProviderInstrument>,
    watchlist: &[InstallProviderInstrument],
    watchlist_visible: bool,
) -> BTreeMap<MarketSummaryKey, (InstallProviderInstrument, bool)> {
    let mut desired = BTreeMap::new();
    for instrument in tab_instruments {
        desired.insert(
            MarketSummaryKey::from_instrument(&instrument),
            (instrument, true),
        );
    }
    for instrument in watchlist {
        let key = MarketSummaryKey::from_instrument(instrument);
        desired
            .entry(key)
            .and_modify(|(_, foreground)| *foreground |= watchlist_visible)
            .or_insert_with(|| (instrument.clone(), watchlist_visible));
    }
    desired
}

impl TerminalApp {
    pub(super) fn new(
        init: TerminalShellInit,
        lifecycle: DesktopLifecycle,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workspaces = init.workspaces;
        let market_frame_wake = UiWake::default();
        for workspace in &mut workspaces {
            workspace.focus = workspace.focus.clone().tab_index(0).tab_stop(true);
        }
        for workspace in &workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.set_market_message_wake(market_frame_wake.callback());
                });
                cx.observe(&pane.surface, |app, surface, cx| {
                    if surface.update(cx, |surface, _| surface.take_chart_persistence_dirty()) {
                        app.persist_workspace_layout_if_changed(cx);
                    }
                    cx.notify();
                })
                .detach();
            }
        }
        let active = init
            .active_workspace_id
            .and_then(|id| workspaces.iter().position(|workspace| workspace.id == id))
            .unwrap_or(0);
        for (index, workspace) in workspaces.iter().enumerate() {
            let resource_class = if index == active {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            };
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.set_market_resource_class(resource_class);
                });
            }
        }
        let persisted_watchlist = init.watchlist_entries.clone();
        let watchlist = restore_watchlist(init.watchlist_entries);
        let workspace_persistence = (init.workspace_shell == WorkspaceShellKind::Tabs)
            .then(|| {
                WorkspaceLayoutPersistence::new(init.workspace_revision, init.layout_generation)
            })
            .transpose()
            .unwrap_or_else(|error| {
                eprintln!("Aeris workspace persistence could not start: {error}");
                None
            });
        let persisted_layout = workspace_layout_tabs(&workspaces, cx);
        let persisted_active_workspace_id = workspaces[active].id;
        Self {
            workspaces,
            active,
            theme: AerisTheme::dark(),
            drawing_toolbar: DrawingToolbarVisibility::Expanded,
            window_active: true,
            frame_poll_gate: frame_poll_gate::FramePollGate::default(),
            market_frame_wake,
            market_wake_listener_started: None,
            chrome_focus: cx.focus_handle().tab_stop(true),
            lifecycle,
            workspace_factory: init.workspace_factory,
            watchlist,
            market_summaries: BTreeMap::new(),
            persisted_watchlist,
            watchlist_persistence_dirty: false,
            workspace_persistence,
            persisted_layout,
            persisted_active_workspace_id,
            workspace_error: None,
            workspace_drag: None,
            watchlist_drag: None,
            watchlist_scroll: ScrollHandle::new(),
            chart_context_menu: None,
            chart_context_copy_feedback_generation: 0,
            chart_settings_menu: None,
            chart_settings_section: ChartSettingsSection::Series,
            chart_settings_color_picker: None,
            chart_settings_template_overlay: ChartSettingsTemplateOverlay::Closed,
            chart_settings_template_name: None,
            chart_settings_template_error: None,
            chart_settings_templates: init.chart_settings_templates,
            chart_settings_persistence_dirty: false,
            account_menu_open: false,
            account_menu_anchor: None,
            profile_refresh_on_activation: false,
            about_dialog_open: false,
            updater: DesktopUpdater::new().ok(),
            update_restart_persistence_pending: false,
            chart_chrome: init.chart_chrome,
            window_move_pending: false,
            closing: false,
        }
    }

    pub(super) fn active_surface(&self) -> Entity<WorkspaceSurface> {
        let workspace = &self.workspaces[self.active];
        workspace.panes[workspace.active_pane].surface.clone()
    }

    fn watchlist_entries(&self) -> Vec<WorkspaceWatchlistEntryState> {
        self.watchlist
            .iter()
            .map(|instrument| WorkspaceWatchlistEntryState {
                instrument: Some(instrument.clone()),
            })
            .collect()
    }

    pub(super) fn watchlist_rows(&self, cx: &App) -> Vec<WatchlistRow> {
        let surface = self.active_surface();
        let surface = surface.read(cx);
        let active = surface
            .rithmic_pending_product
            .as_ref()
            .or(surface.product.as_ref());
        self.watchlist
            .iter()
            .map(|instrument| {
                let summary = self
                    .market_summaries
                    .get(&MarketSummaryKey::from_instrument(instrument));
                WatchlistRow {
                    instrument: instrument.clone(),
                    previous_close: summary.and_then(|summary| summary.previous_close),
                    last: summary.and_then(|summary| summary.last),
                    message: summary
                        .and_then(|summary| summary.message.clone())
                        .or_else(|| {
                            summary
                                .is_none()
                                .then(|| "Market data unavailable".to_string())
                        }),
                    active: active.is_some_and(|active| {
                        active.provider == instrument.provider
                            && active.instrument_id == instrument.instrument_id
                    }),
                }
            })
            .collect()
    }

    pub(super) fn watchlist_panel_state(&self, cx: &App) -> WatchlistPanelState {
        WatchlistPanelState {
            rows: self.watchlist_rows(cx),
            drag: self.watchlist_drag.clone(),
            scroll: self.watchlist_scroll.clone(),
        }
    }

    pub(super) fn reconcile_active_drags(&mut self, cx: &mut Context<Self>) {
        if cx.has_active_drag() {
            return;
        }
        self.workspace_drag = None;
        self.end_watchlist_drag(cx);
    }

    pub(super) fn absorb_watchlist_requests(&mut self, cx: &mut Context<Self>) {
        let requests = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .filter_map(|pane| {
                pane.surface
                    .update(cx, |surface, _| surface.pending_watchlist_instrument.take())
            })
            .collect::<Vec<_>>();
        for instrument in requests {
            self.add_watchlist_instrument(instrument, cx);
        }
        self.sync_watchlist_resource_class(cx);
    }

    fn sync_watchlist_resource_class(&mut self, cx: &App) {
        self.sync_market_summaries(cx);
    }

    pub(super) fn sync_market_summaries(&mut self, cx: &App) {
        let watchlist_visible = self
            .active_surface()
            .read(cx)
            .side_panels
            .contains(SidePanel::Watchlist);

        let tab_instruments = self.workspaces.iter().filter_map(|workspace| {
            let surface = workspace.panes[workspace.active_pane].surface.read(cx);
            market_summary_tab_instrument(surface)
        });
        let desired =
            market_summary_requirements(tab_instruments, &self.watchlist, watchlist_visible);

        let stale = self
            .market_summaries
            .keys()
            .filter(|key| !desired.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        for key in stale {
            if let Some(mut entry) = self.market_summaries.remove(&key)
                && let Some(retirement) = entry
                    .worker
                    .as_mut()
                    .and_then(MarketDataWorker::begin_retirement)
            {
                self.lifecycle.retire_market_worker(retirement, cx);
            }
        }

        let workspace_id = self.workspaces[self.active].id;
        let factory = self.workspace_factory.clone();
        for (key, (instrument, foreground)) in desired {
            let resource_class = if foreground {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            };
            if let Some(entry) = self.market_summaries.get_mut(&key) {
                entry.instrument = instrument;
                entry.set_resource_class(resource_class);
                continue;
            }
            self.market_summaries.insert(
                key,
                start_market_summary(
                    factory.as_ref(),
                    workspace_id,
                    instrument,
                    resource_class,
                    &self.market_frame_wake,
                ),
            );
        }
    }

    fn add_watchlist_instrument(
        &mut self,
        instrument: InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) {
        if self.watchlist.iter().any(|entry| {
            entry.provider == instrument.provider && entry.instrument_id == instrument.instrument_id
        }) || self.watchlist.len() >= local_state::MAXIMUM_WATCHLIST_ENTRIES
        {
            return;
        }
        self.watchlist.push(instrument);
        self.sync_market_summaries(cx);
        self.watchlist_persistence_dirty = true;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn remove_watchlist_instrument(
        &mut self,
        provider: &str,
        instrument_id: &str,
        cx: &mut Context<Self>,
    ) {
        let before = self.watchlist.len();
        self.watchlist
            .retain(|entry| entry.provider != provider || entry.instrument_id != instrument_id);
        if self.watchlist.len() != before {
            self.sync_market_summaries(cx);
            self.watchlist_persistence_dirty = true;
            self.persist_workspace_layout_if_changed(cx);
            cx.notify();
        }
    }

    pub(super) fn begin_watchlist_drag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        cursor_offset_y: f32,
        cx: &mut Context<Self>,
    ) {
        if !self
            .watchlist
            .iter()
            .any(|entry| entry.provider == provider && entry.instrument_id == instrument_id)
        {
            return;
        }
        self.watchlist_drag = Some(WatchlistDragState {
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
        cx.notify();
    }

    pub(super) fn move_watchlist_drag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        pointer_y: f32,
        body_top: f32,
        scroll_offset_y: f32,
        cx: &mut Context<Self>,
    ) {
        let cursor_offset_y =
            {
                let Some(drag) = self.watchlist_drag.as_mut().filter(|drag| {
                    drag.provider == provider && drag.instrument_id == instrument_id
                }) else {
                    return;
                };
                drag.pointer_y = Some(pointer_y);
                drag.body_top = body_top;
                drag.scroll_offset_y = scroll_offset_y;
                drag.cursor_offset_y
            };
        let Some(destination) = watchlist_drag_destination(
            pointer_y,
            body_top,
            scroll_offset_y,
            cursor_offset_y,
            self.watchlist.len(),
        ) else {
            return;
        };
        let Some(source) = self
            .watchlist
            .iter()
            .position(|entry| entry.provider == provider && entry.instrument_id == instrument_id)
        else {
            return;
        };
        if move_item(&mut self.watchlist, source, destination) {
            self.watchlist_persistence_dirty = true;
        }
        cx.notify();
    }

    pub(super) fn end_watchlist_drag(&mut self, cx: &mut Context<Self>) {
        if self.watchlist_drag.take().is_none() {
            return;
        }
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn select_watchlist_instrument(
        &mut self,
        instrument: &InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) {
        self.active_surface().update(cx, |surface, surface_cx| {
            surface.select_installed_instrument(instrument, surface_cx);
        });
    }

    fn chart_chrome_for_new_surface(&self, cx: &App) -> chart_chrome::ChartChromePreferences {
        let mut preferences = self.chart_chrome;
        preferences.chart_type = self.active_surface().read(cx).chart_type(cx);
        preferences
    }

    fn set_workspace_resource_class(
        &self,
        workspace_index: usize,
        resource_class: ConsumerResourceClass,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.workspaces.get(workspace_index) {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.set_market_resource_class(resource_class);
                });
            }
        }
    }

    fn refresh_workspace_focus_order(&mut self) {
        for workspace in &mut self.workspaces {
            workspace.focus = workspace.focus.clone().tab_index(0).tab_stop(true);
        }
    }

    pub(super) fn select_pane(&mut self, workspace_id: u64, pane_id: u64, cx: &mut Context<Self>) {
        let Some(workspace_index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let workspace = &self.workspaces[workspace_index];
        let Some(index) = workspace.panes.iter().position(|pane| pane.id == pane_id) else {
            return;
        };
        if workspace.active_pane == index {
            return;
        }

        let previous = workspace.panes[workspace.active_pane].surface.clone();
        let next = workspace.panes[index].surface.clone();
        let (side_panels, side_panel_width, side_panel_split) =
            previous.read_with(cx, |surface, _| {
                (
                    surface.side_panels,
                    surface.side_panel_width,
                    surface.side_panel_split_basis_points,
                )
            });
        previous.update(cx, |surface, surface_cx| {
            surface.set_order_book_visible(false, surface_cx);
            surface.set_watchlist_visible(false, surface_cx);
        });
        next.update(cx, |surface, surface_cx| {
            surface.side_panel_width = side_panel_width;
            surface.side_panel_split_basis_points = side_panel_split;
            surface.set_order_book_visible(side_panels.contains(SidePanel::OrderBook), surface_cx);
            surface.set_watchlist_visible(side_panels.contains(SidePanel::Watchlist), surface_cx);
        });

        let workspace = &mut self.workspaces[workspace_index];
        workspace.active_pane = index;
        workspace.generation = workspace.generation.saturating_add(1);
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn absorb_pane_activate_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let activate = pane.surface.update(cx, |surface, _| {
                    let pending = surface.pending_pane_activate == PaneActivationRequest::Pending;
                    surface.pending_pane_activate = PaneActivationRequest::None;
                    pending
                });
                if activate {
                    requested = Some((workspace.id, pane.id));
                }
            }
        }
        if let Some((workspace_id, pane_id)) = requested {
            self.select_pane(workspace_id, pane_id, cx);
        }
    }

    pub(super) fn select_drawing_tool_on_active_workspace(
        &mut self,
        tool: ChartDrawingTool,
        cx: &mut Context<Self>,
    ) {
        let panes: Vec<_> = self.workspaces[self.active]
            .panes
            .iter()
            .map(|pane| pane.surface.clone())
            .collect();
        for surface in panes {
            surface.update(cx, |surface, surface_cx| {
                surface.select_drawing_tool(tool, surface_cx);
            });
        }
    }

    pub(super) fn absorb_chart_context_menu_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let request = pane
                    .surface
                    .update(cx, |surface, _| surface.pending_chart_context_menu.take());
                if let Some(request) = request {
                    requested = Some(ChartContextMenu {
                        workspace_id: workspace.id,
                        pane_id: pane.id,
                        position: request.position,
                        kind: request.kind,
                        flyout: PriceAxisMenuFlyout::None,
                        copy_price: request.copy_price,
                        copy_feedback_generation: None,
                    });
                }
            }
        }
        if let Some(menu) = requested {
            self.open_chart_context_menu(menu, cx);
        }
    }

    pub(super) fn absorb_study_settings_requests(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut requested = None;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let study_id = pane.surface.update(cx, |surface, _| {
                    surface.pending_study_settings_request.take()
                });
                if let Some(study_id) = study_id {
                    requested = Some((workspace.id, pane.id, pane.surface.clone(), study_id));
                }
            }
        }
        if let Some((workspace_id, pane_id, surface, study_id)) = requested {
            self.select_pane(workspace_id, pane_id, cx);
            surface.update(cx, |surface, surface_cx| {
                surface.open_study_settings_dialog(study_id, window, surface_cx);
            });
        }
    }

    pub(super) fn absorb_study_remove_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                let study_id = pane
                    .surface
                    .update(cx, |surface, _| surface.pending_study_remove_request.take());
                if let Some(study_id) = study_id {
                    requested = Some((workspace.id, pane.id, pane.surface.clone(), study_id));
                }
            }
        }
        if let Some((workspace_id, pane_id, surface, study_id)) = requested {
            self.select_pane(workspace_id, pane_id, cx);
            surface.update(cx, |surface, surface_cx| {
                surface.remove_runtime_study(study_id, surface_cx);
            });
        }
    }

    pub(super) fn open_chart_context_menu(
        &mut self,
        mut menu: ChartContextMenu,
        cx: &mut Context<Self>,
    ) {
        menu.copy_feedback_generation = None;
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        self.close_chart_settings_menu(cx);
        self.chart_settings_color_picker = None;
        self.chart_context_menu = Some(menu);
        cx.notify();
    }

    pub(super) fn close_chart_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.chart_context_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn close_chart_settings_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.chart_settings_menu.take() {
            self.set_workspace_chart_pointers_suspended(menu.workspace_id, false, cx);
            self.chart_settings_color_picker = None;
            self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
            self.chart_settings_template_name = None;
            self.chart_settings_template_error = None;
            cx.notify();
        }
    }

    fn set_workspace_chart_pointers_suspended(
        &self,
        workspace_id: u64,
        suspended: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        for pane in &workspace.panes {
            pane.surface.update(cx, |surface, surface_cx| {
                if suspended {
                    surface.suspend_chart_pointer(surface_cx);
                } else {
                    surface.resume_chart_pointer(surface_cx);
                }
            });
        }
    }

    fn chart_settings_surface(&self, menu: &ChartContextMenu) -> Option<Entity<WorkspaceSurface>> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .map(|pane| pane.surface.clone())
    }

    fn chart_settings_snapshot(
        &self,
        menu: &ChartContextMenu,
        cx: &App,
    ) -> Option<ChartSettingsSnapshot> {
        let surface = self.chart_settings_surface(menu)?;
        let surface = surface.read(cx);
        Some(ChartSettingsSnapshot {
            chart_type: surface.chart_type(cx),
            appearance: surface.chart_appearance(cx)?,
            crosshair_mode: surface.chart_crosshair_mode(cx)?,
            order_flow: surface.chart_order_flow_settings(cx)?,
        })
    }

    pub(super) fn set_chart_settings_section(
        &mut self,
        section: ChartSettingsSection,
        cx: &mut Context<Self>,
    ) {
        if self.chart_settings_section != section {
            self.chart_settings_section = section;
            self.chart_settings_color_picker = None;
            self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
            cx.notify();
        }
    }

    pub(super) fn dismiss_chart_settings_overlays(&mut self, cx: &mut Context<Self>) {
        let color_was_open = self.chart_settings_color_picker.take().is_some();
        let template_was_open =
            self.chart_settings_template_overlay == ChartSettingsTemplateOverlay::Menu;
        if template_was_open {
            self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
        }
        if color_was_open || template_was_open {
            cx.notify();
        }
    }

    pub(super) fn toggle_chart_color_picker(
        &mut self,
        color: ChartColorSetting,
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .chart_settings_color_picker
            .as_ref()
            .is_some_and(|picker| picker.setting == color)
        {
            self.chart_settings_color_picker = None;
        } else {
            let input = cx.new(|input_cx| InputState::new(window, input_cx));
            input.update(cx, |input, input_cx| {
                input.set_value(current.to_ascii_uppercase(), window, input_cx);
            });
            self.chart_settings_color_picker = Some(ChartColorPickerState {
                setting: color,
                input,
                error: None,
            });
        }
        cx.notify();
    }

    pub(super) fn apply_chart_color(
        &mut self,
        menu: &ChartContextMenu,
        setting: ChartColorSetting,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(color) = normalize_hex_color(color) else {
            if let Some(picker) = &mut self.chart_settings_color_picker
                && picker.setting == setting
            {
                picker.error = Some("Enter a hex color such as #2962FF or #2962FF80.".to_string());
            }
            cx.notify();
            return;
        };
        let Some(surface) = self.chart_settings_surface(menu) else {
            return;
        };
        let Some(mut appearance) = surface.read(cx).chart_appearance(cx) else {
            return;
        };
        match setting {
            ChartColorSetting::Up => appearance.up_color.clone_from(&color),
            ChartColorSetting::Down => appearance.down_color.clone_from(&color),
            ChartColorSetting::WickUp => appearance.wick_up_color.clone_from(&color),
            ChartColorSetting::WickDown => appearance.wick_down_color.clone_from(&color),
            ChartColorSetting::BorderUp => appearance.border_up_color.clone_from(&color),
            ChartColorSetting::BorderDown => appearance.border_down_color.clone_from(&color),
            ChartColorSetting::Line => appearance.line_color.clone_from(&color),
            ChartColorSetting::AreaTop => appearance.area_top_color.clone_from(&color),
            ChartColorSetting::BaselineTop => {
                appearance.baseline_top_color.clone_from(&color);
            }
            ChartColorSetting::BaselineBottom => {
                appearance.baseline_bottom_color.clone_from(&color);
            }
            ChartColorSetting::Grid => appearance.grid_color.clone_from(&color),
            ChartColorSetting::Crosshair => appearance.crosshair_color = color,
        }
        surface.update(cx, |surface, surface_cx| match setting {
            ChartColorSetting::Grid | ChartColorSetting::Crosshair => {
                surface.set_chart_canvas_appearance(&appearance, surface_cx);
            }
            _ => surface.set_chart_series_appearance(&appearance, surface_cx),
        });
        if let Some(picker) = &mut self.chart_settings_color_picker
            && picker.setting == setting
        {
            picker.error = None;
        }
        cx.notify();
    }

    pub(super) fn apply_chart_settings_action(
        &mut self,
        menu: &ChartContextMenu,
        action: ChartSettingsAction,
        cx: &mut Context<Self>,
    ) {
        let Some(surface) = self.chart_settings_surface(menu) else {
            return;
        };
        if let ChartSettingsAction::CrosshairMode(mode) = action {
            surface.update(cx, |surface, surface_cx| {
                surface.set_chart_crosshair_mode(mode, surface_cx);
            });
            cx.notify();
            return;
        }
        if matches!(
            action,
            ChartSettingsAction::FootprintMode(_)
                | ChartSettingsAction::ToggleCumulativeDelta
                | ChartSettingsAction::ToggleDeltaHistogram
                | ChartSettingsAction::ToggleTradeBubbles
                | ChartSettingsAction::TradeBubbleMinimumVolumeBits(_)
        ) {
            let Some(mut settings) = surface.read(cx).chart_order_flow_settings(cx) else {
                return;
            };
            match action {
                ChartSettingsAction::FootprintMode(mode) => settings.display_mode = mode,
                ChartSettingsAction::ToggleCumulativeDelta => {
                    settings.show_cumulative_delta = !settings.show_cumulative_delta;
                }
                ChartSettingsAction::ToggleDeltaHistogram => {
                    settings.show_delta_histogram = !settings.show_delta_histogram;
                }
                ChartSettingsAction::ToggleTradeBubbles => {
                    settings.show_trade_bubbles = !settings.show_trade_bubbles;
                }
                ChartSettingsAction::TradeBubbleMinimumVolumeBits(bits) => {
                    let value = f64::from_bits(bits);
                    if !value.is_finite() || value < 0.0 {
                        return;
                    }
                    settings.trade_bubble_minimum_volume = value;
                }
                _ => unreachable!(),
            }
            surface.update(cx, |surface, surface_cx| {
                surface.set_chart_order_flow_settings(settings, surface_cx);
            });
            cx.notify();
            return;
        }
        let Some(mut appearance) = surface.read(cx).chart_appearance(cx) else {
            return;
        };
        match action {
            ChartSettingsAction::ToggleGrid => appearance.grid_visible = !appearance.grid_visible,
            ChartSettingsAction::GridStyle(style) => appearance.grid_style = style.min(4),
            ChartSettingsAction::CrosshairMode(_)
            | ChartSettingsAction::FootprintMode(_)
            | ChartSettingsAction::ToggleCumulativeDelta
            | ChartSettingsAction::ToggleDeltaHistogram
            | ChartSettingsAction::ToggleTradeBubbles
            | ChartSettingsAction::TradeBubbleMinimumVolumeBits(_) => unreachable!(),
            ChartSettingsAction::CrosshairWidth(width) => {
                appearance.crosshair_width = width.clamp(1, 4);
            }
            ChartSettingsAction::CrosshairStyle(style) => {
                appearance.crosshair_style = style.min(4);
            }
            ChartSettingsAction::ToggleWicks => appearance.wick_visible = !appearance.wick_visible,
            ChartSettingsAction::ToggleBorders => {
                appearance.border_visible = !appearance.border_visible;
            }
            ChartSettingsAction::ToggleOpen => appearance.open_visible = !appearance.open_visible,
            ChartSettingsAction::ToggleThinBars => appearance.thin_bars = !appearance.thin_bars,
            ChartSettingsAction::LineWidth(width) => appearance.line_width = width.clamp(1, 4),
            ChartSettingsAction::LineStyle(style) => appearance.line_style = style.min(4),
        }
        surface.update(cx, |surface, surface_cx| match action {
            ChartSettingsAction::ToggleGrid
            | ChartSettingsAction::GridStyle(_)
            | ChartSettingsAction::CrosshairWidth(_)
            | ChartSettingsAction::CrosshairStyle(_) => {
                surface.set_chart_canvas_appearance(&appearance, surface_cx);
            }
            ChartSettingsAction::ToggleWicks
            | ChartSettingsAction::ToggleBorders
            | ChartSettingsAction::ToggleOpen
            | ChartSettingsAction::ToggleThinBars
            | ChartSettingsAction::LineWidth(_)
            | ChartSettingsAction::LineStyle(_) => {
                surface.set_chart_series_appearance(&appearance, surface_cx);
            }
            ChartSettingsAction::CrosshairMode(_)
            | ChartSettingsAction::FootprintMode(_)
            | ChartSettingsAction::ToggleCumulativeDelta
            | ChartSettingsAction::ToggleDeltaHistogram
            | ChartSettingsAction::ToggleTradeBubbles
            | ChartSettingsAction::TradeBubbleMinimumVolumeBits(_) => unreachable!(),
        });
        cx.notify();
    }

    pub(super) fn reset_chart_settings(&mut self, menu: &ChartContextMenu, cx: &mut Context<Self>) {
        if let Some(surface) = self.chart_settings_surface(menu) {
            surface.update(cx, |surface, surface_cx| {
                surface.reset_chart_appearance(surface_cx);
            });
        }
        self.chart_settings_color_picker = None;
        self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
        cx.notify();
    }

    fn current_chart_settings_template(
        &self,
        menu: &ChartContextMenu,
        name: String,
        cx: &App,
    ) -> Option<WorkspaceChartSettingsTemplateState> {
        let snapshot = self.chart_settings_snapshot(menu, cx)?;
        Some(WorkspaceChartSettingsTemplateState {
            name,
            chart_type: snapshot.chart_type.identifier().to_string(),
            appearance: Some(workspace_surface::persisted_chart_appearance(
                &snapshot.appearance,
            )),
            crosshair_mode: u32::from(snapshot.crosshair_mode),
        })
    }

    fn apply_chart_settings_template_to_surface(
        &self,
        menu: &ChartContextMenu,
        template: &WorkspaceChartSettingsTemplateState,
        cx: &mut Context<Self>,
    ) {
        let Some(surface) = self.chart_settings_surface(menu) else {
            return;
        };
        let Some(chart_type) = ChartType::from_identifier(&template.chart_type) else {
            return;
        };
        let Some(appearance) = template
            .appearance
            .as_ref()
            .and_then(workspace_surface::restored_chart_appearance)
        else {
            return;
        };
        let crosshair_mode = u8::try_from(template.crosshair_mode).unwrap_or(0).min(3);
        surface.update(cx, |surface, surface_cx| {
            surface.set_chart_type(chart_type, surface_cx);
            surface.set_chart_appearance(&appearance, surface_cx);
            surface.set_chart_crosshair_mode(crosshair_mode, surface_cx);
        });
    }

    pub(super) fn toggle_chart_settings_template_menu(&mut self, cx: &mut Context<Self>) {
        self.chart_settings_color_picker = None;
        self.chart_settings_template_overlay =
            if self.chart_settings_template_overlay == ChartSettingsTemplateOverlay::Menu {
                ChartSettingsTemplateOverlay::Closed
            } else {
                ChartSettingsTemplateOverlay::Menu
            };
        cx.notify();
    }

    pub(super) fn open_chart_settings_template_save_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Template name"));
        input.update(cx, |input, input_cx| input.focus(window, input_cx));
        self.chart_settings_template_name = Some(input);
        self.chart_settings_template_error = None;
        self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::SaveDialog;
        cx.notify();
    }

    pub(super) fn cancel_chart_settings_template_save(&mut self, cx: &mut Context<Self>) {
        self.chart_settings_template_name = None;
        self.chart_settings_template_error = None;
        self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Menu;
        cx.notify();
    }

    pub(super) fn save_chart_settings_template(
        &mut self,
        menu: &ChartContextMenu,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = &self.chart_settings_template_name else {
            return;
        };
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty()
            || name.len() > local_state::MAXIMUM_CHART_SETTINGS_TEMPLATE_NAME_BYTES
            || name.chars().any(char::is_control)
        {
            self.chart_settings_template_error =
                Some("Use a name between 1 and 64 characters.".to_string());
            cx.notify();
            return;
        }
        let Some(template) = self.current_chart_settings_template(menu, name.clone(), cx) else {
            return;
        };
        if let Some(existing) = self
            .chart_settings_templates
            .iter_mut()
            .find(|template| template.name.eq_ignore_ascii_case(&name))
        {
            *existing = template;
        } else if self.chart_settings_templates.len()
            < local_state::MAXIMUM_CHART_SETTINGS_TEMPLATES
        {
            self.chart_settings_templates.push(template);
        } else {
            self.chart_settings_template_error =
                Some("You can save up to 32 chart templates.".to_string());
            cx.notify();
            return;
        }
        self.chart_settings_template_name = None;
        self.chart_settings_template_error = None;
        self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Menu;
        self.chart_settings_persistence_dirty = true;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn apply_named_chart_settings_template(
        &mut self,
        menu: &ChartContextMenu,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(template) = self.chart_settings_templates.get(index).cloned() else {
            return;
        };
        self.apply_chart_settings_template_to_surface(menu, &template, cx);
        self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
        cx.notify();
    }

    pub(super) fn apply_chart_settings_to_all(
        &mut self,
        menu: &ChartContextMenu,
        cx: &mut Context<Self>,
    ) {
        let Some(template) = self.current_chart_settings_template(menu, String::new(), cx) else {
            return;
        };
        let pane_ids = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .map(|workspace| {
                workspace
                    .panes
                    .iter()
                    .map(|pane| pane.id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for pane_id in pane_ids {
            let target = ChartContextMenu {
                pane_id,
                ..menu.clone()
            };
            self.apply_chart_settings_template_to_surface(&target, &template, cx);
        }
        self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
        cx.notify();
    }

    /// Opens the account dropdown under the avatar click point, or closes it
    /// when already open. The stored anchor keeps the panel glued to the
    /// avatar's rendered position instead of a fixed screen corner.
    pub(super) fn toggle_account_menu_at(
        &mut self,
        anchor: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.account_menu_open {
            self.account_menu_open = false;
            self.account_menu_anchor = None;
        } else {
            self.account_menu_open = true;
            self.account_menu_anchor = Some(anchor);
        }
        cx.notify();
    }

    pub(super) fn close_account_menu(&mut self, cx: &mut Context<Self>) {
        if self.account_menu_open {
            self.account_menu_open = false;
            self.account_menu_anchor = None;
            cx.notify();
        }
    }

    pub(super) fn arm_profile_refresh_after_browser(&mut self) {
        self.profile_refresh_on_activation = true;
    }

    pub(super) fn open_about_dialog(&mut self, cx: &mut Context<Self>) {
        self.account_menu_open = false;
        self.account_menu_anchor = None;
        self.about_dialog_open = true;
        cx.notify();
    }

    pub(super) fn close_about_dialog(&mut self, cx: &mut Context<Self>) {
        if self.about_dialog_open {
            self.about_dialog_open = false;
            cx.notify();
        }
    }

    pub(super) fn retry_update_check(&mut self, cx: &mut Context<Self>) {
        if let Some(updater) = self.updater.as_mut()
            && let Err(error) = updater.request_check()
        {
            eprintln!("Aeris update check degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if let Some(updater) = self.updater.as_mut()
            && let Err(error) = updater.request_restart()
        {
            eprintln!("Aeris update restart degraded: {error}");
        }
        cx.notify();
    }

    fn cancel_update_restart_after_persistence_failure(&mut self, error: String, _cx: &App) {
        self.update_restart_persistence_pending = false;
        self.workspace_error = Some(error.clone());
        if let Some(cleanup) = self
            .updater
            .as_mut()
            .and_then(|updater| updater.cancel_prepared_restart(error))
        {
            // PreparedRestart::drop only transfers child termination/reaping
            // to its dedicated cleanup worker, so releasing this UI-owned
            // handle cannot block GPUI on process shutdown.
            drop(cleanup);
        }
        if let Some(account) = aeris_desktop::account::DesktopAccount::shared() {
            let _ = account.request_profile_refresh();
        }
    }

    fn commit_update_restart_after_persistence(
        &mut self,
        account_refresh: aeris_account_runtime::AccountRefreshQuiesce,
        cx: &mut Context<Self>,
    ) {
        self.update_restart_persistence_pending = false;
        let Some(updater) = self.updater.as_mut() else {
            drop(account_refresh);
            if let Some(account) = aeris_desktop::account::DesktopAccount::shared() {
                let _ = account.request_profile_refresh();
            }
            self.workspace_error = Some("update client is unavailable".to_string());
            cx.notify();
            return;
        };
        if let Err(error) = updater.commit_restart() {
            drop(account_refresh);
            self.cancel_update_restart_after_persistence_failure(error, cx);
            return;
        }
        self.about_dialog_open = false;
        self.claim_close(cx);
        self.lifecycle.quit_after_shutdown(cx);
        // `quit_after_shutdown` synchronously installs its own account quiesce
        // claim before this update-specific claim is released.
        drop(account_refresh);
    }

    fn prepare_update_restart_after_workspace_persistence(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.update_restart_persistence_pending || self.closing {
            return;
        }
        self.persist_workspace_layout_if_changed(cx);
        let workspace_wait = self
            .workspace_persistence
            .as_ref()
            .map(WorkspaceLayoutPersistence::shutdown_wait);
        let chart_chrome_wait = chart_chrome::chart_chrome_shutdown_wait();
        let account_refresh = aeris_desktop::account::begin_refresh_quiesce();

        self.update_restart_persistence_pending = true;
        let durability = cx.background_executor().spawn(async move {
            let account_refresh = account_refresh?;
            account_refresh.wait()?;
            let workspace_generation = match workspace_wait {
                Some(wait) => Some(wait.wait(Duration::from_secs(2))?),
                None => None,
            };
            let chart_chrome_generation = chart_chrome_wait.wait(Duration::from_secs(2))?;
            Ok::<_, String>((
                workspace_generation,
                chart_chrome_generation,
                account_refresh,
            ))
        });
        cx.spawn_in(window, async move |terminal, cx| {
            let durable_generations = durability.await;
            let update = terminal.update_in(cx, |terminal, _, terminal_cx| {
                match durable_generations {
                    Ok((workspace_generation, chart_chrome_generation, account_refresh)) => {
                        let workspace_current = workspace_generation.is_none_or(|generation| {
                            terminal
                                .workspace_persistence
                                .as_ref()
                                .is_none_or(|persistence| {
                                    persistence.shutdown_generation_is_current(generation)
                                })
                        });
                        let chart_chrome_current =
                            chart_chrome::chart_chrome_shutdown_generation_is_current(
                                chart_chrome_generation,
                            );
                        if workspace_current && chart_chrome_current {
                            terminal.commit_update_restart_after_persistence(
                                account_refresh,
                                terminal_cx,
                            );
                        } else {
                            drop(account_refresh);
                            terminal.cancel_update_restart_after_persistence_failure(
                                "desktop state changed while update restart was preparing; retry the update"
                                    .to_string(),
                                terminal_cx,
                            );
                        }
                    }
                    Err(error) => {
                        terminal
                            .cancel_update_restart_after_persistence_failure(error, terminal_cx);
                    }
                }
                terminal_cx.notify();
            });
            if update.is_err()
                && let Some(account) = aeris_desktop::account::DesktopAccount::shared()
            {
                let _ = account.request_profile_refresh();
            }
        })
        .detach();
    }

    pub(super) fn update_presentation(&self) -> Option<&UpdatePresentation> {
        self.updater.as_ref().map(DesktopUpdater::presentation)
    }

    pub(super) fn account_menu_overlay(
        &self,
        terminal: &Entity<Self>,
        viewport: gpui::Size<Pixels>,
    ) -> Option<AnyElement> {
        if !self.account_menu_open {
            return None;
        }
        let account = aeris_desktop::account::DesktopAccount::shared()
            .map_or_else(aeris_desktop::account::unavailable_menu_state, |account| {
                account.menu_state()
            });
        Some(account_menu_layer(
            terminal,
            &account,
            self.account_menu_anchor,
            viewport,
            &self.theme,
        ))
    }

    pub(super) fn finish_chart_context_menu(
        &mut self,
        mut menu: ChartContextMenu,
        action: ChartContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        menu.copy_feedback_generation = None;
        match action {
            ChartContextAction::CopyPrice => {
                if let Some(price) = menu.copy_price.as_deref() {
                    cx.write_to_clipboard(ClipboardItem::new_string(price.to_string()));
                }
                self.chart_context_copy_feedback_generation = self
                    .chart_context_copy_feedback_generation
                    .saturating_add(1);
                let generation = self.chart_context_copy_feedback_generation;
                menu.copy_feedback_generation = Some(generation);
                self.chart_context_menu = Some(menu);
                cx.notify();
                cx.spawn_in(window, async move |terminal, cx| {
                    cx.background_executor()
                        .timer(COPY_PRICE_FEEDBACK_DURATION)
                        .await;
                    let _ = terminal.update_in(cx, |terminal, _, terminal_cx| {
                        if Self::chart_context_copy_feedback_is_current(
                            terminal.chart_context_menu.as_ref(),
                            generation,
                        ) {
                            terminal.close_chart_context_menu(terminal_cx);
                        }
                    });
                })
                .detach();
                return;
            }
            ChartContextAction::Reset => {
                self.chart_context_menu = None;
                self.update_context_menu_pane(&menu, WorkspaceSurface::reset_chart_view, cx);
            }
            ChartContextAction::ClearDrawings => {
                self.chart_context_menu = None;
                self.update_context_menu_pane(&menu, WorkspaceSurface::clear_drawings, cx);
            }
            ChartContextAction::ClearIndicators => {
                self.chart_context_menu = None;
                self.update_context_menu_pane(&menu, WorkspaceSurface::clear_indicators, cx);
            }
            ChartContextAction::Split(direction) => {
                self.chart_context_menu = None;
                self.split_active_pane(direction, window, cx);
            }
            ChartContextAction::Close => {
                self.chart_context_menu = None;
                self.close_active_pane(&ClosePane, window, cx);
            }
            ChartContextAction::Settings => {
                self.chart_context_menu = None;
                self.set_workspace_chart_pointers_suspended(menu.workspace_id, true, cx);
                self.chart_settings_section = ChartSettingsSection::Series;
                self.chart_settings_color_picker = None;
                self.chart_settings_template_overlay = ChartSettingsTemplateOverlay::Closed;
                self.chart_settings_template_name = None;
                self.chart_settings_template_error = None;
                self.chart_settings_menu = Some(menu);
            }
        }
        cx.notify();
    }

    pub(super) fn chart_context_copy_feedback_is_current(
        menu: Option<&ChartContextMenu>,
        generation: u64,
    ) -> bool {
        menu.and_then(|menu| menu.copy_feedback_generation) == Some(generation)
    }

    fn update_context_menu_pane(
        &self,
        menu: &ChartContextMenu,
        update: impl FnOnce(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            && let Some(pane) = workspace.panes.iter().find(|pane| pane.id == menu.pane_id)
        {
            pane.surface.update(cx, update);
        }
    }

    fn context_menu_chart_objects(&self, menu: &ChartContextMenu, cx: &App) -> (bool, bool) {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .map_or((false, false), |pane| {
                let surface = pane.surface.read(cx);
                let drawings = surface
                    .chart
                    .as_ref()
                    .is_some_and(|chart| chart.read(cx).drawing_count() > 0);
                (drawings, surface.has_removable_indicators(cx))
            })
    }

    fn context_menu_price_axis_state(
        &self,
        menu: &ChartContextMenu,
        pane: usize,
        left: bool,
        cx: &App,
    ) -> Option<PriceAxisMenuState> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .and_then(|pane| pane.surface.read(cx).chart.as_ref())
            .and_then(|chart| chart.read(cx).price_axis_menu_state(pane, left))
    }

    pub(super) fn apply_price_axis_menu(
        &mut self,
        menu: &ChartContextMenu,
        action: PriceAxisMenuAction,
        cx: &mut Context<Self>,
    ) {
        let ChartContextKind::PriceAxis { pane, left } = menu.kind else {
            return;
        };
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        self.update_context_menu_pane(
            menu,
            |surface, surface_cx| {
                if let Some(chart) = &surface.chart {
                    chart.update(surface_cx, |chart, chart_cx| {
                        chart.apply_price_axis_menu_action(pane, left, action);
                        chart_cx.notify();
                    });
                }
            },
            cx,
        );
        if matches!(
            action,
            PriceAxisMenuAction::ToggleIndicatorNameLabels
                | PriceAxisMenuAction::ToggleIndicatorValueLabels
                | PriceAxisMenuAction::ToggleIndicatorPriceLines
        ) {
            self.broadcast_indicator_chrome(menu, cx);
        }
        if let PriceAxisMenuAction::SetLeft(next_left) = action
            && let Some(open) = &mut self.chart_context_menu
            && let ChartContextKind::PriceAxis {
                left: open_left, ..
            } = &mut open.kind
        {
            *open_left = next_left;
        }
        cx.notify();
    }

    pub(super) fn toggle_price_axis_flyout(
        &mut self,
        flyout: PriceAxisMenuFlyout,
        cx: &mut Context<Self>,
    ) {
        if let Some(menu) = &mut self.chart_context_menu {
            menu.flyout = if menu.flyout == flyout {
                PriceAxisMenuFlyout::None
            } else {
                flyout
            };
            cx.notify();
        }
    }

    fn broadcast_indicator_chrome(&mut self, menu: &ChartContextMenu, cx: &mut Context<Self>) {
        let (names, values, price_lines) = self
            .workspaces
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .and_then(|pane| pane.surface.read(cx).chart.clone())
            .map_or(
                (
                    self.chart_chrome.indicator_name_labels_visible,
                    self.chart_chrome.indicator_value_labels_visible,
                    self.chart_chrome.indicator_price_lines_visible,
                ),
                |chart| {
                    let chart = chart.read(cx);
                    (
                        chart.indicator_name_labels_visible(),
                        chart.indicator_value_labels_visible(),
                        chart.indicator_price_lines_visible(),
                    )
                },
            );
        self.chart_chrome.indicator_name_labels_visible = names;
        self.chart_chrome.indicator_value_labels_visible = values;
        self.chart_chrome.indicator_price_lines_visible = price_lines;
        self.chart_chrome.chart_type = self.active_surface().read(cx).chart_type(cx);
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, surface_cx| {
                    surface.apply_indicator_chrome_preferences(
                        names,
                        values,
                        price_lines,
                        surface_cx,
                    );
                });
            }
        }
        let preferences = self.chart_chrome;
        match chart_chrome::request_chart_chrome_preferences_save(preferences) {
            Ok(true) => cx
                .background_executor()
                .spawn(async move {
                    if let Err(error) = chart_chrome::run_chart_chrome_preferences_save_worker() {
                        eprintln!("Aeris chart chrome could not be saved: {error}");
                    }
                })
                .detach(),
            Ok(false) => {}
            Err(error) => {
                eprintln!("Aeris chart chrome could not be saved: {error}");
            }
        }
    }

    pub(super) fn resize_workspace_split(
        &mut self,
        workspace_id: u64,
        left_pane_id: u64,
        right_pane_id: u64,
        ratio: f64,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let current = workspace.layout.layout();
        if current
            .boundary_ratio_for_panes(left_pane_id, right_pane_id)
            .is_some_and(|current| (current - ratio).abs() < 0.0001)
        {
            return;
        }
        if workspace
            .layout
            .resize_between(left_pane_id, right_pane_id, ratio)
            .is_err()
        {
            return;
        }
        workspace.generation = workspace.generation.saturating_add(1);
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    fn persist_workspace_layout_if_changed(&mut self, cx: &App) {
        if self.closing {
            return;
        }
        let Some(persistence) = self.workspace_persistence.as_ref() else {
            return;
        };
        let layout = workspace_layout_tabs(&self.workspaces, cx);
        let active_workspace_id = self.workspaces[self.active].id;
        let watchlist = self.watchlist_entries();
        if layout == self.persisted_layout
            && active_workspace_id == self.persisted_active_workspace_id
            && !self.chart_settings_persistence_dirty
            && !self.watchlist_persistence_dirty
            && watchlist == self.persisted_watchlist
        {
            return;
        }
        if let Err(error) = persistence.request_with_chart_settings(
            active_workspace_id,
            layout.clone(),
            self.chart_settings_templates.clone(),
            watchlist.clone(),
        ) {
            self.workspace_error = Some(error);
            return;
        }
        self.persisted_layout = layout;
        self.persisted_active_workspace_id = active_workspace_id;
        self.chart_settings_persistence_dirty = false;
        self.persisted_watchlist = watchlist;
        self.watchlist_persistence_dirty = false;
    }

    fn select_workspace(&mut self, next: usize, cx: &mut Context<Self>) {
        let Some((previous, next)) = workspace_switch(self.active, next, self.workspaces.len())
        else {
            return;
        };
        self.set_workspace_resource_class(previous, ConsumerResourceClass::Background, cx);
        self.set_workspace_resource_class(next, ConsumerResourceClass::Foreground, cx);
        self.active = next;
        self.workspace_error = None;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn select_workspace_id(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == tab_id)
        {
            self.select_workspace(index, cx);
        }
    }

    pub(super) fn select_and_focus_workspace(
        &mut self,
        next: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if next >= self.workspaces.len() {
            return;
        }
        self.select_workspace(next, cx);
        self.workspaces[next].focus.focus(window, cx);
    }

    pub(super) fn select_relative_workspace(
        &mut self,
        current: usize,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(next) = wrapped_workspace_index(current, self.workspaces.len(), direction) {
            self.select_and_focus_workspace(next, window, cx);
        }
    }

    pub(super) fn select_next_workspace(
        &mut self,
        _: &SelectNextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_relative_workspace(self.active, 1, window, cx);
    }

    pub(super) fn select_previous_workspace(
        &mut self,
        _: &SelectPreviousWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_relative_workspace(self.active, -1, window, cx);
    }

    fn move_active_workspace(
        &mut self,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(destination) = self.active.checked_add_signed(direction) else {
            return;
        };
        if destination >= self.workspaces.len() {
            return;
        }
        let active_id = self.workspaces[self.active].id;
        if self.reorder_workspace(active_id, destination, cx) {
            self.workspaces[self.active].focus.focus(window, cx);
        }
    }

    pub(super) fn move_workspace_left(
        &mut self,
        _: &MoveWorkspaceLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_active_workspace(-1, window, cx);
    }

    pub(super) fn move_workspace_right(
        &mut self,
        _: &MoveWorkspaceRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_active_workspace(1, window, cx);
    }

    pub(super) fn close_active_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = self.workspaces[self.active].id;
        self.close_workspace(tab_id, window, cx);
    }

    fn reorder_workspace(
        &mut self,
        dragged_id: u64,
        destination_index: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let active_id = self.workspaces[self.active].id;
        let mut ids = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        if !reorder_workspace_ids(&mut ids, dragged_id, destination_index) {
            return false;
        }
        self.workspaces.sort_by_key(|workspace| {
            ids.iter()
                .position(|id| *id == workspace.id)
                .unwrap_or(usize::MAX)
        });
        self.active = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == active_id)
            .unwrap_or(0);
        self.refresh_workspace_focus_order();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        true
    }

    pub(super) fn begin_workspace_drag(
        &mut self,
        tab_id: u64,
        cursor_offset_x: f32,
        tab_width: f32,
        cx: &mut Context<Self>,
    ) {
        if self
            .workspaces
            .iter()
            .any(|workspace| workspace.id == tab_id)
        {
            self.workspace_drag = Some(WorkspaceDragState {
                tab_id,
                cursor_offset_x: if cursor_offset_x.is_finite() {
                    cursor_offset_x.clamp(0.0, tab_width)
                } else {
                    tab_width / 2.0
                },
                pointer_x: None,
                strip_left: 0.0,
            });
            cx.notify();
        }
    }

    pub(super) fn move_workspace_drag(
        &mut self,
        tab_id: u64,
        pointer_x: f32,
        strip_left: f32,
        tab_widths: &[f32],
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self
            .workspace_drag
            .as_mut()
            .filter(|drag| drag.tab_id == tab_id)
        else {
            return;
        };
        let cursor_offset_x = drag.cursor_offset_x;
        drag.pointer_x = Some(pointer_x);
        drag.strip_left = strip_left;
        let Some(destination_index) =
            workspace_drag_destination(pointer_x, strip_left, cursor_offset_x, tab_widths)
        else {
            return;
        };
        if !self.reorder_workspace(tab_id, destination_index, cx) {
            cx.notify();
        }
    }

    pub(super) fn end_workspace_drag(&mut self, cx: &mut Context<Self>) {
        if self.workspace_drag.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn close_workspace(
        &mut self,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        let active_id = self.workspaces[self.active].id;
        if ids.len() == 1 && ids[0] == tab_id {
            self.retire_market_summaries(cx);
            self.retire_workspaces(cx);
            window.remove_window();
            return;
        }
        let Some(next_active_id) = active_workspace_after_close(&ids, active_id, tab_id) else {
            return;
        };
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == tab_id)
        else {
            return;
        };
        let removed = self.workspaces.remove(index);
        let focus_next_tab = active_id == tab_id || removed.focus.is_focused(window);
        if self
            .workspace_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.workspace_drag = None;
            cx.stop_active_drag(window);
        }
        for pane in removed.panes {
            pane.surface.update(cx, |workspace, workspace_cx| {
                workspace.set_market_resource_class(ConsumerResourceClass::Detached);
                workspace.retire_market_worker(workspace_cx);
            });
        }
        self.active = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == next_active_id)
            .unwrap_or(0);
        self.refresh_workspace_focus_order();
        self.set_workspace_resource_class(self.active, ConsumerResourceClass::Foreground, cx);
        self.sync_market_summaries(cx);
        if focus_next_tab {
            self.workspaces[self.active].focus.focus(window, cx);
        }
        self.workspace_error = None;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn add_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let maximum = current_plan_limits().workspaces;
        if self.workspaces.len() >= maximum {
            self.workspace_error = Some(format!(
                "Your plan supports at most {maximum} open workspaces"
            ));
            cx.notify();
            return;
        }
        let Some(factory) = self.workspace_factory.clone() else {
            return;
        };
        let active = self.active_surface();
        let (product, interval) = {
            let active = active.read(cx);
            let Some(product) = active.product.clone() else {
                self.workspace_error =
                    Some("The active workspace has no market to copy".to_string());
                cx.notify();
                return;
            };
            (product, active.interval)
        };
        let pane = match factory.create_workspace(product, interval) {
            Ok(worker) => worker,
            Err(error) => {
                self.workspace_error = Some(error);
                cx.notify();
                return;
            }
        };
        let workspace_id = pane.workspace_id;
        let pane_id = pane.pane_id;
        let consumer_id = pane.consumer_id;
        let surface = workspace_surface_entity(
            pane.startup,
            pane.worker,
            &self.lifecycle,
            self.chart_chrome_for_new_surface(cx),
            WorkspaceSurfaceRestore::default(),
            window,
            cx,
        );
        surface.update(cx, |workspace, workspace_cx| {
            workspace.apply_theme(&self.theme, workspace_cx);
            workspace.set_market_message_wake(self.market_frame_wake.callback());
            let _ = workspace_cx;
        });
        cx.observe(&surface, |app, surface, cx| {
            if surface.update(cx, |surface, _| surface.take_chart_persistence_dirty()) {
                app.persist_workspace_layout_if_changed(cx);
            }
            cx.notify();
        })
        .detach();
        self.set_workspace_resource_class(self.active, ConsumerResourceClass::Background, cx);
        self.workspaces.push(WorkspaceTab {
            id: workspace_id,
            label: format!("Workspace {workspace_id}"),
            panes: vec![WorkspacePane {
                id: pane_id,
                consumer_id,
                surface,
                focus: cx.focus_handle(),
            }],
            active_pane: 0,
            layout: NucleusWorkspace::new(pane_id, MAXIMUM_PANES_PER_WORKSPACE),
            generation: 1,
            focus: cx.focus_handle(),
        });
        self.refresh_workspace_focus_order();
        self.active = self.workspaces.len() - 1;
        self.workspace_error = None;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn new_workspace(
        &mut self,
        _: &NewWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_workspace(window, cx);
    }

    fn split_active_pane(
        &mut self,
        split_direction: ChartSplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(factory) = self.workspace_factory.clone() else {
            self.workspace_error = Some("This window cannot open another chart pane".to_string());
            cx.notify();
            return;
        };
        let workspace = &self.workspaces[self.active];
        let maximum = current_plan_limits().panes_per_workspace;
        if workspace.panes.len() >= maximum {
            self.workspace_error = Some(format!(
                "Your plan supports at most {maximum} charts per workspace"
            ));
            cx.notify();
            return;
        }
        let (product, interval, drawing_tool, side_panels, side_panel_width, side_panel_split) = {
            let source = workspace.panes[workspace.active_pane].surface.read(cx);
            let Some(product) = source.product.clone() else {
                self.workspace_error = Some("The active pane has no market to copy".to_string());
                cx.notify();
                return;
            };
            (
                product,
                source.interval,
                source.drawing_toolbar_state(cx).active_tool,
                source.side_panels,
                source.side_panel_width,
                source.side_panel_split_basis_points,
            )
        };
        let source_surface = workspace.panes[workspace.active_pane].surface.clone();
        let workspace_id = workspace.id;
        let insertion_index = workspace.active_pane.saturating_add(1);
        let pane = match factory.create_pane(workspace_id, product, interval) {
            Ok(pane) => pane,
            Err(error) => {
                self.workspace_error = Some(error);
                cx.notify();
                return;
            }
        };
        let surface = workspace_surface_entity(
            pane.startup,
            pane.worker,
            &self.lifecycle,
            self.chart_chrome_for_new_surface(cx),
            WorkspaceSurfaceRestore::default(),
            window,
            cx,
        );
        surface.update(cx, |surface, surface_cx| {
            surface.apply_theme(&self.theme, surface_cx);
            surface.set_market_resource_class(ConsumerResourceClass::Foreground);
            surface.set_market_message_wake(self.market_frame_wake.callback());
            surface.select_drawing_tool(drawing_tool, surface_cx);
            surface.side_panel_width = side_panel_width;
            surface.side_panel_split_basis_points = side_panel_split;
            surface.set_order_book_visible(side_panels.contains(SidePanel::OrderBook), surface_cx);
            surface.set_watchlist_visible(side_panels.contains(SidePanel::Watchlist), surface_cx);
        });
        cx.observe(&surface, |app, surface, cx| {
            if surface.update(cx, |surface, _| surface.take_chart_persistence_dirty()) {
                app.persist_workspace_layout_if_changed(cx);
            }
            cx.notify();
        })
        .detach();
        let workspace = &mut self.workspaces[self.active];
        let source_pane_id = workspace.panes[workspace.active_pane].id;
        if let Err(error) = workspace
            .layout
            .split(source_pane_id, split_direction, pane.pane_id)
        {
            surface.update(cx, |surface, surface_cx| {
                surface.set_market_resource_class(ConsumerResourceClass::Detached);
                surface.retire_market_worker(surface_cx);
            });
            self.workspace_error = Some(error.to_string());
            cx.notify();
            return;
        }
        source_surface.update(cx, |surface, surface_cx| {
            surface.set_order_book_visible(false, surface_cx);
            surface.set_watchlist_visible(false, surface_cx);
        });
        workspace.panes.insert(
            insertion_index,
            WorkspacePane {
                id: pane.pane_id,
                consumer_id: pane.consumer_id,
                surface,
                focus: cx.focus_handle(),
            },
        );
        workspace.active_pane = insertion_index;
        workspace.generation = workspace.generation.saturating_add(1);
        self.workspace_error = None;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn split_pane_horizontal(
        &mut self,
        _: &SplitPaneHorizontal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active_pane(ChartSplitDirection::Horizontal, window, cx);
    }

    pub(super) fn split_pane_vertical(
        &mut self,
        _: &SplitPaneVertical,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.split_active_pane(ChartSplitDirection::Vertical, window, cx);
    }

    pub(super) fn close_active_pane(
        &mut self,
        _: &ClosePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = &mut self.workspaces[self.active];
        if workspace.panes.len() == 1 {
            return;
        }
        let removed_pane_id = workspace.panes[workspace.active_pane].id;
        if let Err(error) = workspace.layout.remove(removed_pane_id) {
            self.workspace_error = Some(error.to_string());
            cx.notify();
            return;
        }
        let (removed_side_panels, removed_side_panel_width, removed_side_panel_split) = workspace
            .panes[workspace.active_pane]
            .surface
            .read_with(cx, |surface, _| {
                (
                    surface.side_panels,
                    surface.side_panel_width,
                    surface.side_panel_split_basis_points,
                )
            });
        let removed = workspace.panes.remove(workspace.active_pane);
        let pane_order = workspace.layout.layout().pane_ids();
        let recipient_id = pane_order
            .get(
                workspace
                    .active_pane
                    .min(pane_order.len().saturating_sub(1)),
            )
            .copied()
            .unwrap_or(workspace.panes[0].id);
        let recipient = workspace
            .panes
            .iter()
            .position(|pane| pane.id == recipient_id)
            .unwrap_or(0);
        workspace.active_pane = recipient;
        workspace.generation = workspace.generation.saturating_add(1);
        removed.surface.update(cx, |surface, surface_cx| {
            surface.set_order_book_visible(false, surface_cx);
            surface.set_watchlist_visible(false, surface_cx);
            surface.set_market_resource_class(ConsumerResourceClass::Detached);
            surface.retire_market_worker(surface_cx);
        });
        workspace.panes[recipient]
            .surface
            .update(cx, |surface, surface_cx| {
                surface.side_panel_width = removed_side_panel_width;
                surface.side_panel_split_basis_points = removed_side_panel_split;
                surface.set_order_book_visible(
                    removed_side_panels.contains(SidePanel::OrderBook),
                    surface_cx,
                );
                surface.set_watchlist_visible(
                    removed_side_panels.contains(SidePanel::Watchlist),
                    surface_cx,
                );
            });
        workspace.panes[recipient].focus.focus(window, cx);
        self.workspace_error = None;
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = self.theme.toggled();
        cx.update_global::<gpui_base::Theme, _>(|base, _| *base = base_theme(&self.theme));
        window.refresh();
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |workspace, workspace_cx| {
                    workspace.apply_theme(&self.theme, workspace_cx);
                });
            }
        }
        cx.notify();
    }

    pub(super) fn request_sign_in(cx: &mut Context<Self>) {
        let result = aeris_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.request_sign_in(),
        );
        if let Err(error) = result {
            eprintln!("Aeris sign-in degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn sign_out(cx: &mut Context<Self>) {
        let result = aeris_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.request_sign_out(),
        );
        if let Err(error) = result {
            eprintln!("Aeris sign-out degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn reopen_browser_page(cx: &mut Context<Self>) {
        let result = aeris_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.reopen_browser(),
        );
        if let Err(error) = result {
            eprintln!("Aeris browser reopen degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn cancel_sign_in(cx: &mut Context<Self>) {
        let result = aeris_desktop::account::DesktopAccount::shared().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            |account| account.request_cancel(),
        );
        if let Err(error) = result {
            eprintln!("Aeris sign-in cancellation degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn toggle_drawing_toolbar(&mut self, cx: &mut Context<Self>) {
        self.drawing_toolbar.toggle();
        cx.notify();
    }

    fn retire_workspaces<C: gpui::AppContext>(&mut self, cx: &mut C) {
        for workspace in &self.workspaces {
            for pane in &workspace.panes {
                pane.surface.update(cx, |workspace, workspace_cx| {
                    workspace.retire_market_worker(workspace_cx);
                });
            }
        }
    }

    fn retire_market_summaries(&mut self, cx: &App) {
        for entry in self.market_summaries.values_mut() {
            if let Some(retirement) = entry
                .worker
                .as_mut()
                .and_then(MarketDataWorker::begin_retirement)
            {
                self.lifecycle.retire_market_worker(retirement, cx);
            }
        }
    }

    pub(super) fn claim_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.closing {
            return false;
        }
        self.persist_workspace_layout_if_changed(cx);
        self.closing = true;
        if let Some(persistence) = self.workspace_persistence.as_ref() {
            self.lifecycle
                .await_workspace_persistence(persistence.shutdown_wait());
        }
        self.retire_market_summaries(cx);
        self.retire_workspaces(cx);
        true
    }

    pub(super) fn close_window(
        &mut self,
        _: &CloseWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.claim_close(cx) {
            window.remove_window();
        }
    }

    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key.eq_ignore_ascii_case("escape") && self.about_dialog_open {
            self.close_about_dialog(cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key.as_str() == "tab" {
            if event.keystroke.modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key.as_str() == "escape"
            && (self.chart_context_menu.is_some() || self.chart_settings_menu.is_some())
        {
            self.close_chart_context_menu(cx);
            self.close_chart_settings_menu(cx);
            cx.stop_propagation();
            return;
        }
        if self.workspace_drag.is_some() && event.keystroke.key.as_str() == "escape" {
            cx.stop_active_drag(window);
            self.end_workspace_drag(cx);
            cx.stop_propagation();
            return;
        }
        if self.chart_context_menu.is_some() || self.chart_settings_menu.is_some() {
            return;
        }
        let handled = self.active_surface().update(cx, |workspace, workspace_cx| {
            workspace.on_terminal_key_down(event, window, workspace_cx)
        });
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    pub(super) fn track_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_active = window.is_window_active();
        let became_active = window_active && !self.window_active;
        self.window_active = window_active;
        if !window_active {
            self.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
            if self.workspace_drag.is_some() {
                cx.stop_active_drag(window);
                self.end_workspace_drag(cx);
            }
        }
        if became_active {
            if self.profile_refresh_on_activation {
                self.profile_refresh_on_activation = false;
                if let Some(account) = aeris_desktop::account::DesktopAccount::shared()
                    && let Err(error) = account.request_profile_refresh()
                {
                    eprintln!("Aeris profile refresh degraded: {error}");
                }
            }
            self.schedule_market_frame(window, cx);
        }
    }

    pub(super) fn handle_window_move_gesture(
        &mut self,
        event: WindowMoveGestureEvent,
        window: &mut Window,
    ) {
        let transition = window_move_gesture_transition(self.window_move_pending, event);
        self.window_move_pending = transition.pending;
        if transition.start_move {
            window.start_window_move();
        }
    }

    /// Schedules the single presentation boundary for live market state.
    ///
    /// `on_next_frame` follows the window's compositor cadence, so the bounded
    /// mailbox is drained at the active display's refresh rate without a second
    /// fixed-frequency timer in an individual chart or order-book surface.
    pub(super) fn schedule_market_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.frame_poll_gate.try_schedule() {
            return;
        }
        let terminal = cx.entity();
        window.on_next_frame(move |window, cx| {
            let diagnostics = terminal.update(cx, |terminal, cx| {
                terminal.frame_poll_gate.complete();
                if let Some(account) = aeris_desktop::account::DesktopAccount::shared()
                    && account.poll()
                {
                    cx.notify();
                }
                if !terminal.closing
                    && terminal
                        .workspace_persistence
                        .as_ref()
                        .is_some_and(WorkspaceLayoutPersistence::poll)
                {
                    terminal.workspace_error = terminal
                        .workspace_persistence
                        .as_ref()
                        .and_then(WorkspaceLayoutPersistence::error);
                    cx.notify();
                }
                let authenticated = true;
                let mut diagnostics = Vec::new();
                let mut summaries_changed = false;
                if authenticated {
                    for entry in terminal.market_summaries.values_mut() {
                        summaries_changed |= entry.poll();
                    }
                }
                if summaries_changed {
                    cx.notify();
                }
                for workspace in &terminal.workspaces {
                    for pane in &workspace.panes {
                        let surface = pane.surface.clone();
                        let pending = surface.update(cx, |workspace, workspace_cx| {
                            if authenticated && workspace.should_poll_market() {
                                workspace.poll_market_worker(workspace_cx);
                            }
                            workspace.pending_ui_diagnostics.take()
                        });
                        if let Some(pending) = pending {
                            diagnostics.push((surface, pending));
                        }
                    }
                }
                terminal.persist_workspace_layout_if_changed(cx);
                diagnostics
            });
            if !diagnostics.is_empty() {
                window.on_next_frame(move |_, cx| {
                    for (workspace, mut diagnostics) in diagnostics {
                        diagnostics.mark_frame_submit();
                        workspace.update(cx, |workspace, _| {
                            workspace
                                .market_worker
                                .send_ui_diagnostics(diagnostics.into_presented());
                        });
                    }
                });
            }
        });
    }
}

impl TerminalApp {
    pub(super) fn start_market_wake_listener(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.market_wake_listener_started.is_some() {
            return;
        }
        self.market_wake_listener_started = Some(());
        // Authentication must progress even when a suspended provider has
        // no events to wake this window. Poll only account presentation;
        // a changed view schedules a frame that also drains retained data.
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        let restart_prepared = if let Some(updater) = terminal.updater.as_mut() {
                            let update = updater.poll();
                            if update.changed {
                                terminal_cx.notify();
                            }
                            update.restart_prepared
                        } else {
                            false
                        };
                        if restart_prepared {
                            terminal.prepare_update_restart_after_workspace_persistence(
                                window,
                                terminal_cx,
                            );
                            return;
                        }
                        if let Some(account) = aeris_desktop::account::DesktopAccount::shared()
                            && account.poll()
                        {
                            terminal.schedule_market_frame(window, terminal_cx);
                            terminal_cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let wake = self.market_frame_wake.clone();
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                wake.notified().await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        terminal.schedule_market_frame(window, terminal_cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(super) fn chart_surface_menus(
        &self,
        terminal: &Entity<Self>,
        pane_count: usize,
        chart_has_market_data: bool,
        viewport: gpui::Size<Pixels>,
        cx: &App,
    ) -> (Option<AnyElement>, Option<AnyElement>) {
        let context_menu = self.chart_context_menu.clone().map(|menu| {
            if let ChartContextKind::PriceAxis { pane, left } = menu.kind {
                let state = self
                    .context_menu_price_axis_state(&menu, pane, left, cx)
                    .unwrap_or(PriceAxisMenuState {
                        flags: PriceAxisMenuState::PRICE_LINE
                            | PriceAxisMenuState::LAST_VALUE
                            | PriceAxisMenuState::TITLE
                            | PriceAxisMenuState::COUNTDOWN
                            | PriceAxisMenuState::INDICATOR_NAMES
                            | PriceAxisMenuState::INDICATOR_VALUES
                            | PriceAxisMenuState::INDICATOR_PRICE_LINES
                            | PriceAxisMenuState::AUTO_SCALE
                            | PriceAxisMenuState::ALIGN_LABELS,
                        mode: 0,
                        left,
                        precision: None,
                    });
                return price_axis_menu_layer(terminal, &menu, state, viewport, &self.theme);
            }
            let (has_drawings, has_indicators) = self.context_menu_chart_objects(&menu, cx);
            let mut flags = 0;
            if menu.copy_price.is_some() {
                flags |= ChartContextMenuState::COPY_PRICE;
            }
            if chart_has_market_data {
                flags |= ChartContextMenuState::READY;
            }
            if self.workspace_factory.is_some() && pane_count < MAXIMUM_PANES_PER_WORKSPACE {
                flags |= ChartContextMenuState::SPLIT;
            }
            if has_drawings {
                flags |= ChartContextMenuState::DRAWINGS;
            }
            if has_indicators {
                flags |= ChartContextMenuState::INDICATORS;
            }
            chart_context_menu_layer(
                terminal,
                &menu,
                ChartContextMenuState { pane_count, flags },
                viewport,
                &self.theme,
            )
        });
        let settings_menu = self.chart_settings_menu.clone().and_then(|menu| {
            self.chart_settings_snapshot(&menu, cx).map(|snapshot| {
                chart_settings_menu_layer(
                    terminal,
                    &menu,
                    ChartSettingsView {
                        section: self.chart_settings_section,
                        snapshot: &snapshot,
                        color_picker: self.chart_settings_color_picker.as_ref(),
                        templates: ChartSettingsTemplateView {
                            overlay: self.chart_settings_template_overlay,
                            name_input: self.chart_settings_template_name.as_ref(),
                            error: self.chart_settings_template_error.as_deref(),
                            templates: &self.chart_settings_templates,
                            apply_to_all: self
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.id == menu.workspace_id)
                                .is_some_and(|workspace| workspace.panes.len() > 1),
                        },
                    },
                    viewport,
                    &self.theme,
                )
            })
        });
        (context_menu, settings_menu)
    }
}
