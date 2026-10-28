//! Workspace tabs.

use super::market_watch::{DragMove, FlagPress, PickerColor};
use super::*;
use aeris_observability::diagnostic;

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
                diagnostic!("market summary demand could not start: {error}");
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
    previous_selection: Option<&InstallProviderInstrument>,
    showing_superseded: bool,
) -> Option<InstallProviderInstrument> {
    if showing_superseded {
        return previous_selection.cloned().or_else(|| product.cloned());
    }
    product.cloned()
}

fn market_summary_tab_instrument(surface: &WorkspaceSurface) -> Option<InstallProviderInstrument> {
    market_summary_instrument_for_switch(
        surface.product.as_ref(),
        surface.chart_pane_controller.restore_product(),
        surface.showing_superseded_series(),
    )
}

pub(super) fn market_summary_requirements<'a>(
    tab_instruments: impl IntoIterator<Item = InstallProviderInstrument>,
    watchlist: impl IntoIterator<Item = &'a InstallProviderInstrument>,
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
    /// Gives every restored pane the shared market wake and its resource class, and persists
    /// the layout when a pane's chart state changes.
    pub(super) fn wire_restored_surfaces(
        workspaces: &[WorkspaceTab],
        active: usize,
        market_frame_wake: &UiWake,
        cx: &mut Context<Self>,
    ) {
        for (index, workspace) in workspaces.iter().enumerate() {
            let resource_class = if index == active {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            };
            for pane in &workspace.panes {
                Self::wire_pane_notices(&pane.surface, cx);
                pane.surface.update(cx, |surface, surface_cx| {
                    surface.set_market_message_wake(market_frame_wake.callback());
                    surface.set_market_resource_class(resource_class, surface_cx);
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
    }

    fn wire_pane_notices(surface: &Entity<WorkspaceSurface>, cx: &mut Context<Self>) {
        cx.observe(surface, |app, pane, cx| {
            app.refresh_trading_pane_for_selection(&pane, cx);
        })
        .detach();
        cx.subscribe(surface, |app, surface, event: &PaneNoticeEvent, cx| {
            let now = Instant::now();
            match event {
                PaneNoticeEvent::Post(notice) => {
                    app.notices.toaster_mut().post_notice(
                        toaster::ToastStack::of(&surface),
                        notice.clone(),
                        now,
                    );
                }
                PaneNoticeEvent::Clear(key) => app.notices.toaster_mut().clear_notice(key, now),
            }
            cx.notify();
        })
        .detach();
        surface.update(cx, |surface, cx| {
            surface.emit_initial_price_alert_restore_notice(cx);
            surface.emit_initial_chart_failure_notice(cx);
            surface.apply_initial_chart_settings(cx);
        });
    }

    fn start_workspace_layout_persistence(
        init: &TerminalShellInit,
        lifecycle: &DesktopLifecycle,
    ) -> Option<WorkspaceLayoutPersistence> {
        (init.workspace_shell == WorkspaceShellKind::Tabs)
            .then(|| {
                WorkspaceLayoutPersistence::new(
                    init.workspace_revision,
                    init.layout_generation,
                    lifecycle.persistence(),
                )
            })
            .transpose()
            .unwrap_or_else(|error| {
                diagnostic!("Aeris workspace persistence could not start: {error}");
                None
            })
    }

    pub(super) fn new(
        init: TerminalShellInit,
        lifecycle: DesktopLifecycle,
        command_palette_input: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let workspace_persistence = Self::start_workspace_layout_persistence(&init, &lifecycle);
        let frames = FrameWakes::default();
        lifecycle.set_context_publication_wake(frames.market().callback());
        let mut tabs = TabStrip::new(init.workspaces, init.active_workspace_id);
        Self::wire_restored_surfaces(tabs.as_slice(), tabs.active_index(), frames.market(), cx);
        let (layout_library, layout_drawings) = layout_tabs::restored_layouts(
            init.saved_layouts,
            &mut tabs,
            layout_tabs::now_unix_ms(&lifecycle),
            cx,
        );
        let restored = WorkspaceSnapshot {
            layout: workspace_layout_tabs(tabs.as_slice(), cx),
            active_workspace_id: tabs.active().id,
            watchlist: init.watchlist_entries.clone(),
        };
        let trading_store = start_trading_store(&lifecycle, frames.market(), cx);
        Self {
            tabs,
            theme: AerisTheme::dark(),
            frames,
            window: WindowState::new(cx.focus_handle().tab_stop(true)),
            lifecycle,
            services: AppServices::new(
                trading_store,
                init.workspace_factory,
                aeris_platform_runtime::active_data_root()
                    .ok()
                    .and_then(|root| {
                        root.run_installed_app_side_effect(
                            aeris_platform_runtime::InstalledAppSideEffect::Update,
                            DesktopUpdater::new,
                        )
                    })
                    .and_then(Result::ok),
                Instant::now(),
            ),
            market_watch: MarketWatch::restore(init.watchlist_entries),
            saved: SavedWorkspace::new(
                workspace_persistence,
                restored,
                layout_library,
                layout_drawings,
                init.chart_settings_templates,
                init.drawing_templates,
            ),
            chart_menus: ChartMenus::default(),
            overlays: OverlayStack::new(command_palette_input),
            pages: app_navigation::AppPages::default(),
            notices: AppNotices::new(init.workspace_notice),
            keyboard_trading: trading_hotkeys::KeyboardTrading::default(),
            provider_login: provider_login::ProviderLoginFlow::default(),
            chrome: AppChrome::new(init.chart_chrome),
        }
    }

    pub(super) fn active_surface(&self) -> Entity<WorkspaceSurface> {
        let workspace = self.tabs.active();
        workspace.panes[workspace.active_pane].surface.clone()
    }

    pub(super) fn synchronize_linked_charts(&mut self, cx: &mut Context<Self>) {
        let mut pending = Vec::new();
        let mut pending_instruments = Vec::new();
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                let (group, events, instrument) = pane.surface.update(cx, |surface, _| {
                    (
                        surface.chart_link_group,
                        surface.take_chart_sync_events(),
                        surface.take_linked_instrument(),
                    )
                });
                if group == 0 {
                    continue;
                }
                if let Some(instrument) = instrument {
                    pending_instruments.push((workspace.id, pane.id, group, instrument));
                }
                pending.extend(
                    events
                        .into_iter()
                        .map(|event| (workspace.id, pane.id, group, event)),
                );
            }
        }
        for (source_workspace, source_pane, group, instrument) in pending_instruments {
            for workspace in self.tabs.iter() {
                for pane in &workspace.panes {
                    if (workspace.id, pane.id) == (source_workspace, source_pane) {
                        continue;
                    }
                    pane.surface.update(cx, |surface, surface_cx| {
                        if surface.chart_link_group == group {
                            surface.apply_linked_instrument(&instrument, surface_cx);
                        }
                    });
                }
            }
        }
        for (source_workspace, source_pane, group, event) in pending {
            if !self.tabs.accept_linked_sync(&event.source, event.revision) {
                continue;
            }
            for workspace in self.tabs.iter() {
                for pane in &workspace.panes {
                    if (workspace.id, pane.id) == (source_workspace, source_pane) {
                        continue;
                    }
                    pane.surface.update(cx, |surface, surface_cx| {
                        if surface.chart_link_group == group {
                            surface.apply_linked_chart_event(&event.kind, surface_cx);
                        }
                    });
                }
            }
        }
    }

    pub(super) fn watchlist_entries(&self) -> Vec<WorkspaceWatchlistEntryState> {
        self.market_watch.durable_entries()
    }

    pub(super) fn watchlist_rows(&self, cx: &App) -> Vec<WatchlistRow> {
        let surface = self.active_surface();
        let surface = surface.read(cx);
        let active = surface
            .chart_pane_controller
            .pending_product()
            .or(surface.product.as_ref());
        let account = surface.trading_pnl.order_entry.selected_account_id.as_ref();
        let store = self.services.trading_store().read(cx);
        let entries = self.market_watch.entries();
        self.market_watch
            .visible_indices()
            .into_iter()
            .filter_map(|index| entries.get(index))
            .map(|entry| {
                let instrument = &entry.instrument;
                let summary = self
                    .market_watch
                    .summaries()
                    .get(&MarketSummaryKey::from_instrument(instrument));
                WatchlistRow {
                    instrument: instrument.clone(),
                    previous_close: summary.and_then(|summary| summary.previous_close),
                    last: summary.and_then(|summary| summary.last),
                    tick_reference: summary.and_then(|summary| summary.tick_reference),
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
                    trading_activity: account.is_some_and(|account| {
                        let Ok(id) = aeris_instruments::InstrumentId::try_new(
                            instrument.instrument_id.clone(),
                        ) else {
                            return false;
                        };
                        store.current().is_some_and(|current| {
                            store
                                .pane_view(account, &id)
                                .orders()
                                .any(|order| order.status.is_open())
                                || current.position_pnl.iter().any(|position| {
                                    &position.position.account_id == account
                                        && position.position.instrument_id == id
                                        && position.position.net_quantity.units() != 0
                                })
                        })
                    }),
                    flag: entry.flag.clone(),
                }
            })
            .collect()
    }

    pub(super) fn watchlist_panel_state(&self, cx: &App) -> WatchlistPanelState {
        WatchlistPanelState {
            rows: self.watchlist_rows(cx),
            drag: self.market_watch.drag().cloned(),
            scroll: self.market_watch.scroll().clone(),
            flags: self.market_watch.flags_view(),
        }
    }

    pub(super) fn reconcile_active_drags(&mut self, cx: &mut Context<Self>) {
        if cx.has_active_drag() {
            return;
        }
        self.tabs.end_drag();
        self.end_watchlist_drag(cx);
    }

    pub(super) fn absorb_watchlist_requests(&mut self, cx: &mut Context<Self>) {
        let requests = self
            .tabs
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .filter_map(|pane| {
                pane.surface.update(cx, |surface, _| {
                    surface.symbol_entry.take_installed_watchlist_instrument()
                })
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
            .chrome
            .side_panels()
            .contains(SidePanel::Watchlist);

        let tab_instruments = self.tabs.iter().filter_map(|workspace| {
            let surface = workspace.panes[workspace.active_pane].surface.read(cx);
            market_summary_tab_instrument(surface)
        });
        let desired = market_summary_requirements(
            tab_instruments,
            self.market_watch.instruments(),
            watchlist_visible,
        );

        for mut entry in self
            .market_watch
            .take_stale_summaries(|key| desired.contains_key(key))
        {
            if let Some(retirement) = entry
                .worker
                .as_mut()
                .and_then(MarketDataWorker::begin_retirement)
            {
                self.lifecycle.retire_market_worker(retirement, cx);
            }
        }

        let workspace_id = self.tabs.active().id;
        let factory = self.services.market_factory().cloned();
        for (key, (instrument, foreground)) in desired {
            let resource_class = if foreground {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            };
            if let Some(entry) = self.market_watch.summaries_mut().get_mut(&key) {
                entry.instrument = instrument;
                entry.set_resource_class(resource_class);
                continue;
            }
            self.market_watch.summaries_mut().insert(
                key,
                start_market_summary(
                    factory.as_ref(),
                    workspace_id,
                    instrument,
                    resource_class,
                    self.frames.market(),
                ),
            );
        }
    }

    fn add_watchlist_instrument(
        &mut self,
        instrument: InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) {
        if !self.market_watch.add(instrument) {
            return;
        }
        self.sync_market_summaries(cx);
        self.saved.mark_unsaved(UnsavedEdit::Watchlist);
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn remove_watchlist_instrument(
        &mut self,
        provider: &str,
        instrument_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.market_watch.remove(provider, instrument_id) {
            self.sync_market_summaries(cx);
            self.saved.mark_unsaved(UnsavedEdit::Watchlist);
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
        if self
            .market_watch
            .begin_drag(provider, instrument_id, cursor_offset_y)
        {
            cx.notify();
        }
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
        let DragMove::Placed { reordered } = self.market_watch.move_drag(
            provider,
            instrument_id,
            pointer_y,
            body_top,
            scroll_offset_y,
        ) else {
            return;
        };
        if reordered {
            self.saved.mark_unsaved(UnsavedEdit::Watchlist);
        }
        cx.notify();
    }

    pub(super) fn end_watchlist_drag(&mut self, cx: &mut Context<Self>) {
        if !self.market_watch.end_drag() {
            return;
        }
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    /// A left press on a row's flag: an unflagged row takes the default flag and opens its
    /// color picker; a flagged row loses its flag.
    pub(super) fn press_watchlist_flag(
        &mut self,
        provider: &str,
        instrument_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default_color = default_watchlist_flag_color(&self.theme);
        match self
            .market_watch
            .press_flag(provider, instrument_id, default_color)
        {
            FlagPress::Missing => {}
            FlagPress::Flagged => {
                self.show_watchlist_flag_picker(provider, instrument_id, window, cx);
            }
            FlagPress::Cleared => self.save_watchlist_flags(cx),
        }
    }

    /// A right press on a row's flag toggles its color picker, flagging the row first.
    pub(super) fn toggle_watchlist_flag_picker(
        &mut self,
        provider: &str,
        instrument_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.market_watch.picker_is_for(provider, instrument_id) {
            self.close_watchlist_flag_picker(cx);
            return;
        }
        let default_color = default_watchlist_flag_color(&self.theme);
        if self
            .market_watch
            .ensure_flag(provider, instrument_id, default_color)
        {
            self.show_watchlist_flag_picker(provider, instrument_id, window, cx);
        }
    }

    fn show_watchlist_flag_picker(
        &mut self,
        provider: &str,
        instrument_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .market_watch
            .flag(provider, instrument_id)
            .map_or_else(|| default_watchlist_flag_color(&self.theme), str::to_string);
        let input = cx.new(|input_cx| InputState::new(window, input_cx));
        input.update(cx, |input, input_cx| {
            input.set_value(current, window, input_cx);
        });
        self.market_watch.open_picker(WatchlistFlagPicker {
            provider: provider.to_string(),
            instrument_id: instrument_id.to_string(),
            input,
            error: None,
        });
        self.save_watchlist_flags(cx);
    }

    /// Recolors the flag whose picker is open. The change is saved when the picker closes.
    pub(super) fn apply_watchlist_flag_color(&mut self, color: &str, cx: &mut Context<Self>) {
        match self.market_watch.apply_picker_color(color) {
            PickerColor::Closed => return,
            PickerColor::Applied => self.saved.mark_unsaved(UnsavedEdit::Watchlist),
            PickerColor::Rejected | PickerColor::Accepted => {}
        }
        cx.notify();
    }

    pub(super) fn close_watchlist_flag_picker(&mut self, cx: &mut Context<Self>) {
        if self.market_watch.close_picker() {
            self.save_watchlist_flags(cx);
        }
    }

    fn save_watchlist_flags(&mut self, cx: &mut Context<Self>) {
        self.saved.mark_unsaved(UnsavedEdit::Watchlist);
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn toggle_watchlist_flag_filter_menu(&mut self, cx: &mut Context<Self>) {
        self.market_watch.toggle_filter_menu();
        cx.notify();
    }

    pub(super) fn close_watchlist_flag_filter_menu(&mut self, cx: &mut Context<Self>) {
        if self.market_watch.close_filter_menu() {
            cx.notify();
        }
    }

    /// Shows only the rows flagged with `filter`, or every row for `None`.
    pub(super) fn set_watchlist_flag_filter(
        &mut self,
        filter: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.market_watch.set_filter(filter);
        cx.notify();
    }

    /// Applies a chart shortcut when the active chart is ready for one. A held key acts once,
    /// so holding Space never streams symbol switches.
    /// Whether keyboard focus rests on the workspace itself (the shell, the active workspace's
    /// tab, or the active pane's chrome or chart) rather than on a text field, so workspace
    /// shortcuts may act.
    pub(super) fn workspace_keyboard_focused(&self, window: &Window, cx: &App) -> bool {
        if self.pages.layouts.covers_workspace() {
            return false;
        }
        self.window.chrome_focus().is_focused(window)
            || self.tabs.active().focus.is_focused(window)
            || self
                .active_surface()
                .read(cx)
                .holds_keyboard_focus(window, cx)
    }

    /// Returns keyboard focus to the visible workspace: the active pane's chart on the terminal
    /// page, otherwise the shell.
    pub(super) fn focus_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pages.view != market_screener::AppView::Terminal
            || self.pages.layouts.start_page_shown()
        {
            self.window.chrome_focus().focus(window, cx);
            return;
        }
        self.active_surface()
            .update(cx, |surface, cx| surface.focus_keyboard(window, cx));
    }

    fn apply_chart_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use aeris_desktop::command_registry::{WorkspaceShortcut, workspace_key_shortcut};
        let Some(shortcut) = workspace_key_shortcut(&event.keystroke) else {
            return false;
        };
        if !self.workspace_keyboard_focused(window, cx)
            || !self.active_surface().read(cx).chart_shortcuts_unblocked()
        {
            return false;
        }
        if event.is_held {
            return true;
        }
        match shortcut {
            WorkspaceShortcut::ToggleChartFullscreen => self.toggle_chart_fullscreen(window, cx),
            WorkspaceShortcut::NextWatchlistSymbol => self.step_watchlist(true, cx),
            WorkspaceShortcut::PreviousWatchlistSymbol => self.step_watchlist(false, cx),
            WorkspaceShortcut::MaximizePane => return false,
        }
        true
    }

    /// Shift+F: the workspace's charts, in their layout, fill the screen (a pane maximized
    /// with Alt+click stays the only one). The window enters fullscreen unless it already is,
    /// and every toolbar and panel hides; leaving restores both.
    pub(super) fn toggle_chart_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.window.toggle_chart_fullscreen(window.is_fullscreen()) {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    /// Leaving window fullscreen another way (F11, Esc, the system) also ends a chart-only
    /// fullscreen that entered it.
    pub(super) fn track_chart_fullscreen(&mut self, window: &Window) {
        self.window.track_window_fullscreen(window.is_fullscreen());
    }

    /// Shows the next or previous watchlist symbol on the active chart.
    fn step_watchlist(&mut self, forward: bool, cx: &mut Context<Self>) {
        let visible = self.market_watch.visible_indices();
        let entries = self.market_watch.entries();
        let current = {
            let surface = self.active_surface().read(cx);
            let active = surface
                .chart_pane_controller
                .pending_product()
                .or(surface.product.as_ref());
            active.and_then(|active| {
                visible
                    .iter()
                    .position(|&index| entries[index].is(&active.provider, &active.instrument_id))
            })
        };
        let Some(instrument) = watchlist_step(current, visible.len(), forward)
            .and_then(|row| visible.get(row))
            .and_then(|&index| entries.get(index))
            .map(|entry| entry.instrument.clone())
        else {
            return;
        };
        self.select_watchlist_instrument(&instrument, cx);
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
        let mut preferences = *self.chrome.preferences();
        preferences.chart_type = self.active_surface().read(cx).chart_type(cx);
        preferences
    }

    pub(super) fn set_workspace_resource_class(
        &self,
        workspace_index: usize,
        resource_class: ConsumerResourceClass,
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self.tabs.get(workspace_index) {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, surface_cx| {
                    surface.set_market_resource_class(resource_class, surface_cx);
                });
            }
        }
    }

    pub(super) fn select_pane(&mut self, workspace_id: u64, pane_id: u64, cx: &mut Context<Self>) {
        let Some(workspace_index) = self
            .tabs
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let workspace = &self.tabs[workspace_index];
        let Some(index) = workspace.panes.iter().position(|pane| pane.id == pane_id) else {
            return;
        };
        if workspace.active_pane == index {
            return;
        }

        let previous = workspace.panes[workspace.active_pane].surface.clone();
        let next = workspace.panes[index].surface.clone();
        let (side_panels, side_panel_size) = previous.read_with(cx, |surface, _| {
            (
                surface.chrome.side_panels(),
                surface.chrome.side_panel_size(),
            )
        });
        previous.update(cx, |surface, surface_cx| {
            surface.apply_side_panels(SidePanelVisibility::default(), surface_cx);
        });
        next.update(cx, |surface, surface_cx| {
            surface.chrome.set_side_panel_size(side_panel_size);
            surface.apply_side_panels(side_panels, surface_cx);
        });

        let workspace = &mut self.tabs[workspace_index];
        workspace.active_pane = index;
        workspace.generation = workspace.generation.saturating_add(1);
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn begin_workspace_pane_alt_click(
        &mut self,
        workspace_id: u64,
        pane_id: u64,
        alt_primary: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(workspace_index) = self
            .tabs
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return false;
        };
        self.tabs[workspace_index].swallow_pane_mouse_up = false;
        if !alt_primary {
            return false;
        }
        let workspace = &self.tabs[workspace_index];
        if !workspace.panes.iter().any(|pane| pane.id == pane_id) {
            return false;
        }
        let WorkspaceMaximizeTransition::Set(maximized_pane) =
            workspace_maximize_transition(workspace.maximized_pane, pane_id, workspace.panes.len())
        else {
            return false;
        };

        self.select_pane(workspace_id, pane_id, cx);
        let workspace = &mut self.tabs[workspace_index];
        workspace.maximized_pane = maximized_pane;
        workspace.swallow_pane_mouse_up = true;
        cx.notify();
        true
    }

    pub(super) fn take_workspace_pane_mouse_up(&mut self, workspace_id: u64) -> bool {
        self.tabs
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .is_some_and(|workspace| std::mem::take(&mut workspace.swallow_pane_mouse_up))
    }

    pub(super) fn absorb_pane_activate_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in self.tabs.iter() {
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
        tool: DrawingToolChoice,
        cx: &mut Context<Self>,
    ) {
        self.chrome.tool_menu_mut().record(tool);
        cx.notify();
        let panes: Vec<_> = self
            .tabs
            .active()
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
        for workspace in self.tabs.iter() {
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
                        capture_flyout_open: false,
                        copy_price: request.copy_price,
                        copy_feedback: None,
                    });
                }
            }
        }
        if let Some(menu) = requested {
            self.open_chart_context_menu(menu, cx);
        }
    }

    pub(super) fn absorb_settings_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                let request = pane
                    .surface
                    .update(cx, |surface, _| surface.pending_settings_request.take());
                if let Some(request) = request {
                    requested = Some((workspace.id, pane.id, pane.surface.clone(), request));
                }
            }
        }
        if let Some((workspace_id, pane_id, surface, request)) = requested {
            self.select_pane(workspace_id, pane_id, cx);
            surface.update(cx, |surface, surface_cx| {
                // An open dialog's previews are restored before the next dialog reads the chart.
                surface.cancel_indicator_settings_dialog(surface_cx);
                match request {
                    LegendSettingsRequest::Study(study_id) => {
                        surface.open_study_settings_dialog(study_id, window, surface_cx);
                    }
                    LegendSettingsRequest::BigTrades => {
                        surface.open_big_trades_dialog(window, surface_cx);
                    }
                    LegendSettingsRequest::Volume => {
                        surface.open_volume_dialog(window, surface_cx);
                    }
                    LegendSettingsRequest::OrderFlow(indicator) => {
                        surface.open_order_flow_dialog(indicator, window, surface_cx);
                    }
                    LegendSettingsRequest::Drawing(drawing) => {
                        surface.open_drawing_settings_dialog(drawing, window, surface_cx);
                    }
                }
            });
        }
    }

    pub(super) fn absorb_study_remove_requests(&mut self, cx: &mut Context<Self>) {
        let mut requested = None;
        for workspace in self.tabs.iter() {
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
        menu.copy_feedback = None;
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        self.show_chart_context_menu(menu, cx);
        cx.notify();
    }

    /// Shows `menu` as the context menu; a settings panel it replaces resumes its charts.
    fn show_chart_context_menu(&mut self, menu: ChartContextMenu, cx: &mut Context<Self>) {
        if let Some(workspace_id) = self.chart_menus.open_context(menu) {
            self.set_workspace_chart_pointers_suspended(workspace_id, false, cx);
        }
    }

    pub(super) fn close_chart_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.chart_menus.close_context() {
            cx.notify();
        }
    }

    pub(super) fn close_chart_settings_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(workspace_id) = self.chart_menus.close_settings() {
            self.set_workspace_chart_pointers_suspended(workspace_id, false, cx);
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
            .tabs
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
        self.tabs
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
            theme_defaults: surface.chart_theme_defaults(cx)?,
            axis: surface.chart_axis_appearance(cx)?,
            series_colors: surface.chart_series_colors(cx)?,
            crosshair_mode: surface.chart_crosshair_mode(cx)?,
            order_flow: surface.chart_order_flow_settings(cx)?,
            time_zone: surface.chart_time_zone_id().to_string(),
            trading_visibility: surface.chart_trading_visibility(),
            chart_trade_buttons_visible: self.chrome.preferences().chart_trade_buttons.shown(),
            legend: surface.chart_legend_options(cx)?,
        })
    }

    pub(super) fn set_chart_settings_section(
        &mut self,
        section: ChartSettingsSection,
        cx: &mut Context<Self>,
    ) {
        if self.chart_menus.set_settings_section(section) {
            cx.notify();
        }
    }

    pub(super) fn dismiss_chart_settings_overlays(&mut self, cx: &mut Context<Self>) {
        if self.chart_menus.dismiss_settings_popovers() {
            cx.notify();
        }
    }

    /// Opens a settings dropdown, closing every other settings popover, or closes it when it
    /// is already open.
    pub(super) fn toggle_chart_settings_select(
        &mut self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.chart_menus.toggle_settings_select(id) {
            cx.notify();
        }
    }

    /// Applies a dropdown choice and closes its dropdown.
    pub(super) fn choose_chart_settings_option(
        &mut self,
        menu: &ChartContextMenu,
        action: ChartSettingsAction,
        cx: &mut Context<Self>,
    ) {
        self.chart_menus.close_settings_select();
        self.apply_chart_settings_action(menu, action, cx);
        cx.notify();
    }

    pub(super) fn toggle_chart_color_picker(
        &mut self,
        color: ChartColorSetting,
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = if self.chart_menus.color_picker_shows(color) {
            self.chart_menus.close_color_picker()
        } else {
            let input = cx.new(|input_cx| InputState::new(window, input_cx));
            input.update(cx, |input, input_cx| {
                input.set_value(current.to_ascii_uppercase(), window, input_cx);
            });
            self.chart_menus.open_color_picker(color, input)
        };
        if changed {
            cx.notify();
        }
    }

    pub(super) fn close_chart_color_picker(&mut self, cx: &mut Context<Self>) {
        if self.chart_menus.close_color_picker() {
            cx.notify();
        }
    }

    pub(super) fn apply_chart_color(
        &mut self,
        menu: &ChartContextMenu,
        setting: ChartColorSetting,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(color) = normalize_hex_color(color) else {
            self.chart_menus.set_color_error(
                setting,
                Some("Enter a hex color such as #2962FF or #2962FF80.".to_string()),
            );
            cx.notify();
            return;
        };
        let Some(surface) = self.chart_settings_surface(menu) else {
            return;
        };
        if matches!(
            setting,
            ChartColorSetting::AxisBorder | ChartColorSetting::AxisText
        ) {
            let Some(mut axis) = surface.read(cx).chart_axis_appearance(cx) else {
                return;
            };
            if setting == ChartColorSetting::AxisBorder {
                axis.border_color = ChartAppearanceColor::Custom(color);
            } else {
                axis.text_color = ChartAppearanceColor::Custom(color);
            }
            surface.update(cx, |surface, surface_cx| {
                surface.set_chart_axis_appearance(&axis, surface_cx);
            });
        } else {
            let Some(appearance) = surface.read(cx).chart_appearance(cx) else {
                return;
            };
            Self::apply_chart_appearance_color(&surface, appearance, setting, color, cx);
        }
        self.chart_menus.set_color_error(setting, None);
        cx.notify();
    }

    fn apply_chart_appearance_color(
        surface: &Entity<WorkspaceSurface>,
        mut appearance: ChartAppearanceSettings,
        setting: ChartColorSetting,
        color: String,
        cx: &mut Context<Self>,
    ) {
        match setting {
            ChartColorSetting::Up => {
                appearance.up_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::Down => {
                appearance.down_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::WickUp => {
                appearance.wick_up_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::WickDown => {
                appearance.wick_down_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::BorderUp => {
                appearance.border_up_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::BorderDown => {
                appearance.border_down_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::Line => {
                appearance.line_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::AreaTop => {
                appearance.area_top_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::AreaBottom => {
                appearance.area_bottom_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::BaselineTop => {
                appearance.baseline_top_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::BaselineBottom => {
                appearance.baseline_bottom_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::Background => {
                appearance.background_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::Text => {
                appearance.text_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::Grid => {
                appearance.grid_color = ChartAppearanceColor::Custom(color.clone());
            }
            ChartColorSetting::Crosshair => {
                appearance.crosshair_color = ChartAppearanceColor::Custom(color);
            }
            ChartColorSetting::AxisBorder | ChartColorSetting::AxisText => return,
        }
        surface.update(cx, |surface, surface_cx| match setting {
            ChartColorSetting::Background
            | ChartColorSetting::Text
            | ChartColorSetting::Grid
            | ChartColorSetting::Crosshair => {
                surface.set_chart_canvas_appearance(&appearance, surface_cx);
            }
            _ => surface.set_chart_series_appearance(&appearance, surface_cx),
        });
    }

    /// Applies chart trading overlay actions through the surface that owns them.
    /// Returns whether `action` was a trading overlay action.
    fn apply_trading_visibility_action(
        surface: &Entity<WorkspaceSurface>,
        action: ChartSettingsAction,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut visibility = surface.read(cx).chart_trading_visibility();
        match action {
            ChartSettingsAction::ToggleOrderManagementLines => {
                visibility.show_order_management_lines = !visibility.show_order_management_lines;
            }
            ChartSettingsAction::ToggleExecutionMarks => {
                visibility.show_execution_marks = !visibility.show_execution_marks;
            }
            ChartSettingsAction::ToggleExtendOrderLinesLeft => {
                visibility.extend_order_lines_left = !visibility.extend_order_lines_left;
            }
            _ => return false,
        }
        surface.update(cx, |surface, surface_cx| {
            surface.set_chart_trading_visibility(visibility, surface_cx);
        });
        true
    }

    /// Applies legend actions through the chart, which owns the legend.
    /// Returns whether `action` was a legend action.
    fn apply_legend_settings_action(
        surface: &Entity<WorkspaceSurface>,
        action: ChartSettingsAction,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(
            action,
            ChartSettingsAction::ToggleSymbolLegend
                | ChartSettingsAction::ToggleSymbolLegendValues
                | ChartSettingsAction::ToggleIndicatorLegend
                | ChartSettingsAction::ToggleIndicatorLegendValues
        ) {
            return false;
        }
        let Some(mut legend) = surface.read(cx).chart_legend_options(cx) else {
            return true;
        };
        match action {
            ChartSettingsAction::ToggleSymbolLegend => legend.symbol = !legend.symbol,
            ChartSettingsAction::ToggleSymbolLegendValues => {
                legend.symbol_values = !legend.symbol_values;
            }
            ChartSettingsAction::ToggleIndicatorLegend => legend.indicators = !legend.indicators,
            ChartSettingsAction::ToggleIndicatorLegendValues => {
                legend.indicator_values = !legend.indicator_values;
            }
            _ => unreachable!(),
        }
        surface.update(cx, |surface, surface_cx| {
            surface.set_chart_legend_options(legend, surface_cx);
        });
        true
    }

    /// Applies footprint order-flow actions through the chart's order-flow settings owner.
    /// Returns whether `action` was an order-flow action.
    fn apply_order_flow_settings_action(
        surface: &Entity<WorkspaceSurface>,
        action: ChartSettingsAction,
        cx: &mut Context<Self>,
    ) -> bool {
        if !matches!(
            action,
            ChartSettingsAction::FootprintMode(_) | ChartSettingsAction::FootprintTicksPerRow(_)
        ) {
            return false;
        }
        let Some(mut settings) = surface.read(cx).chart_order_flow_settings(cx) else {
            return true;
        };
        match action {
            ChartSettingsAction::FootprintMode(mode) => settings.display_mode = mode,
            ChartSettingsAction::FootprintTicksPerRow(ticks) => settings.ticks_per_row = ticks,
            _ => unreachable!(),
        }
        surface.update(cx, |surface, surface_cx| {
            surface.set_chart_order_flow_settings(settings, surface_cx);
        });
        true
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
        if matches!(action, ChartSettingsAction::ToggleChartTradeButtons) {
            let visible = !self.chrome.preferences().chart_trade_buttons.shown();
            self.set_chart_trade_buttons_visible(visible, cx);
            return;
        }
        if Self::apply_trading_visibility_action(&surface, action, cx)
            || Self::apply_legend_settings_action(&surface, action, cx)
            || Self::apply_order_flow_settings_action(&surface, action, cx)
        {
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
            | ChartSettingsAction::FootprintTicksPerRow(_)
            | ChartSettingsAction::ToggleOrderManagementLines
            | ChartSettingsAction::ToggleExecutionMarks
            | ChartSettingsAction::ToggleExtendOrderLinesLeft
            | ChartSettingsAction::ToggleSymbolLegend
            | ChartSettingsAction::ToggleSymbolLegendValues
            | ChartSettingsAction::ToggleIndicatorLegend
            | ChartSettingsAction::ToggleIndicatorLegendValues
            | ChartSettingsAction::ToggleChartTradeButtons => unreachable!(),
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
            | ChartSettingsAction::FootprintTicksPerRow(_)
            | ChartSettingsAction::ToggleOrderManagementLines
            | ChartSettingsAction::ToggleExecutionMarks
            | ChartSettingsAction::ToggleExtendOrderLinesLeft
            | ChartSettingsAction::ToggleSymbolLegend
            | ChartSettingsAction::ToggleSymbolLegendValues
            | ChartSettingsAction::ToggleIndicatorLegend
            | ChartSettingsAction::ToggleIndicatorLegendValues
            | ChartSettingsAction::ToggleChartTradeButtons => unreachable!(),
        });
        cx.notify();
    }

    pub(super) fn reset_chart_settings(&mut self, menu: &ChartContextMenu, cx: &mut Context<Self>) {
        if let Some(surface) = self.chart_settings_surface(menu) {
            surface.update(cx, |surface, surface_cx| {
                surface.reset_chart_appearance(surface_cx);
                surface.set_chart_trading_visibility(
                    ChartTradingVisibilitySettings::default(),
                    surface_cx,
                );
                surface.set_chart_legend_options(ChartLegendOptions::default(), surface_cx);
            });
        }
        self.chart_menus.close_settings_popover();
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
            appearance: Some(workspace_surface::persisted_chart_appearance(
                &snapshot.appearance,
                &snapshot.axis,
            )),
            crosshair_mode: u32::from(snapshot.crosshair_mode),
            time_zone: snapshot.time_zone,
            show_order_management_lines: Some(
                snapshot.trading_visibility.show_order_management_lines,
            ),
            show_execution_marks: Some(snapshot.trading_visibility.show_execution_marks),
            extend_order_lines_left: Some(snapshot.trading_visibility.extend_order_lines_left),
            legend: Some(workspace_surface::persisted_chart_legend(snapshot.legend)),
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
        let Some((appearance, axis)) = template.appearance.as_ref().and_then(|appearance| {
            Some((
                workspace_surface::restored_chart_appearance(appearance)?,
                workspace_surface::restored_chart_axis(appearance),
            ))
        }) else {
            return;
        };
        let crosshair_mode = u8::try_from(template.crosshair_mode).unwrap_or(0).min(3);
        let time_zone = if template.time_zone.is_empty() {
            aeris_chart_integration::DEFAULT_TIME_ZONE
        } else {
            template.time_zone.as_str()
        };
        let trading_visibility = ChartTradingVisibilitySettings {
            show_order_management_lines: template.show_order_management_lines.unwrap_or(true),
            show_execution_marks: template.show_execution_marks.unwrap_or(true),
            extend_order_lines_left: template.extend_order_lines_left.unwrap_or(true),
        };
        let legend = workspace_surface::restored_chart_legend(template.legend);
        surface.update(cx, |surface, surface_cx| {
            surface.set_chart_appearance(&appearance, surface_cx);
            surface.set_chart_axis_appearance(&axis, surface_cx);
            surface.set_chart_crosshair_mode(crosshair_mode, surface_cx);
            surface.set_chart_time_zone(time_zone, surface_cx);
            surface.set_chart_trading_visibility(trading_visibility, surface_cx);
            surface.set_chart_legend_options(legend, surface_cx);
        });
    }

    pub(super) fn toggle_chart_settings_template_menu(&mut self, cx: &mut Context<Self>) {
        if self.chart_menus.toggle_template_menu() {
            cx.notify();
        }
    }

    /// Shows the saved-templates flyout while its row is hovered; other rows hide it.
    pub(super) fn set_chart_settings_templates_flyout(
        &mut self,
        open: bool,
        cx: &mut Context<Self>,
    ) {
        if self.chart_menus.show_templates_flyout(open) {
            cx.notify();
        }
    }

    pub(super) fn confirm_chart_settings_template_delete(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if index < self.saved.chart_settings_templates().len()
            && self.chart_menus.confirm_template_delete(index)
        {
            cx.notify();
        }
    }

    pub(super) fn cancel_chart_settings_template_delete(&mut self, cx: &mut Context<Self>) {
        if self.chart_menus.show_saved_templates() {
            cx.notify();
        }
    }

    pub(super) fn delete_chart_settings_template(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.chart_menus.take_template_delete() else {
            return;
        };
        if self.saved.remove_chart_settings_template(index) {
            self.persist_workspace_layout_if_changed(cx);
        }
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
        if self.chart_menus.open_template_save(input) {
            cx.notify();
        }
    }

    pub(super) fn cancel_chart_settings_template_save(&mut self, cx: &mut Context<Self>) {
        if self.chart_menus.show_template_menu() {
            cx.notify();
        }
    }

    pub(super) fn save_chart_settings_template(
        &mut self,
        menu: &ChartContextMenu,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self
            .chart_menus
            .settings()
            .and_then(ChartSettingsPanel::template_name)
        else {
            return;
        };
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty()
            || name.len() > local_state::MAXIMUM_CHART_SETTINGS_TEMPLATE_NAME_BYTES
            || name.chars().any(char::is_control)
        {
            self.chart_menus
                .set_template_error("Use a name between 1 and 64 characters.");
            cx.notify();
            return;
        }
        let Some(template) = self.current_chart_settings_template(menu, name.clone(), cx) else {
            return;
        };
        if !self.saved.store_chart_settings_template(template) {
            self.chart_menus
                .set_template_error("You can save up to 32 chart templates.");
            cx.notify();
            return;
        }
        self.chart_menus.show_template_menu();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn apply_named_chart_settings_template(
        &mut self,
        menu: &ChartContextMenu,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(template) = self.saved.chart_settings_templates().get(index).cloned() else {
            return;
        };
        self.apply_chart_settings_template_to_surface(menu, &template, cx);
        self.chart_menus.close_templates();
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
            .tabs
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
        self.chart_menus.close_templates();
        cx.notify();
    }

    /// Opens the platform menu under the avatar click point, or closes it when
    /// open. The anchor keeps the panel glued to the avatar's rendered position.
    pub(super) fn toggle_platform_menu_at(
        &mut self,
        anchor: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.overlays.toggle_platform_menu(anchor);
        cx.notify();
    }

    pub(super) fn close_platform_menu(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.overlays.close(OverlayKind::PlatformMenu) {
            self.release_frameless_title_bar(window, cx);
            cx.notify();
        }
    }

    /// Menus and drags that start in the frameless title bar keep it on screen until they end.
    fn frameless_title_bar_pinned(&self) -> bool {
        self.overlays
            .kind()
            .is_some_and(OverlayKind::pins_title_bar)
            || self.tabs.is_dragging()
            || self.window.move_pending()
    }

    /// Grows the frameless bar back. A pointer that only brushes the edge and leaves before
    /// it ever enters the bar produces no hover-out, so the conceal is armed right away.
    pub(super) fn reveal_frameless_title_bar(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.chrome.preferences().window_frame.frameless()
            && self.window.title_bar_mut().reveal(Instant::now())
        {
            self.release_frameless_title_bar(window, cx);
            cx.notify();
        }
    }

    /// Slides the frameless title bar away once the pointer has been off it for the grace
    /// period, unless something pins it first. Hover events depend on pointer motion and on
    /// which child occludes the bar, so each check reads the pointer position, and a pointer
    /// still resting on the bar postpones the conceal by another grace period.
    pub(super) fn release_frameless_title_bar(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.chrome.preferences().window_frame.frameless()
            || !self.window.title_bar().revealed()
            || self.frameless_title_bar_pinned()
        {
            return;
        }
        let ticket = self.window.title_bar_mut().request_conceal();
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                cx.background_executor()
                    .timer(frameless_title_bar::CONCEAL_DELAY)
                    .await;
                let held = terminal.update_in(cx, |terminal, window, terminal_cx| {
                    if !terminal
                        .window
                        .title_bar()
                        .conceal_request_is_current(ticket)
                        || terminal.frameless_title_bar_pinned()
                    {
                        return false;
                    }
                    let now = Instant::now();
                    let pointer_y = window
                        .is_window_hovered()
                        .then(|| f32::from(window.mouse_position().y));
                    if terminal.window.title_bar().holds_pointer(pointer_y, now) {
                        return true;
                    }
                    if terminal.window.title_bar_mut().conceal(now) {
                        terminal_cx.notify();
                    }
                    false
                });
                if !matches!(held, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Turns frameless mode on or off for every workspace and saves it with the shell chrome.
    /// Turning it on from the open platform menu keeps the bar on screen under the pointer.
    pub(super) fn set_window_frame(
        &mut self,
        window_frame: chart_chrome::WindowFrame,
        cx: &mut Context<Self>,
    ) {
        if self.chrome.preferences_mut().window_frame == window_frame {
            return;
        }
        self.chrome.preferences_mut().window_frame = window_frame;
        if window_frame.frameless() {
            self.window.title_bar_mut().settle_revealed();
        } else {
            self.window.title_bar_mut().reset();
        }
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.window_frame = window_frame;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }

    pub(super) fn arm_profile_refresh_after_browser(&mut self) {
        self.window.arm_profile_refresh();
    }

    pub(super) fn retry_update_check(&mut self, cx: &mut Context<Self>) {
        if let Some(updater) = self.services.updater_mut()
            && let Err(_error) = updater.request_check()
        {
            diagnostic!("Aeris update check degraded");
            self.post_workspace_notice(
                "update_check",
                "Update check unavailable",
                notification_center::NoticeLifetime::Timed(Duration::from_secs(6)),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if let Some(updater) = self.services.updater_mut()
            && let Err(_error) = updater.request_restart()
        {
            diagnostic!("Aeris update restart degraded");
            self.post_workspace_notice(
                "update_restart",
                "Update restart unavailable",
                notification_center::NoticeLifetime::UntilDismissed,
                cx,
            );
        }
        cx.notify();
    }

    fn post_workspace_notice(
        &mut self,
        kind: &'static str,
        title: &'static str,
        lifetime: notification_center::NoticeLifetime,
        cx: &mut Context<Self>,
    ) {
        if let Some(host) = toast_host(self.tabs.active()) {
            self.notices.toaster_mut().post_notice(
                host,
                notification_center::UserNotice {
                    key: notification_center::NoticeKey { scope: 0, kind },
                    severity: notification_center::NoticeSeverity::Warning,
                    tone: notification_center::NoticeTone::Warning,
                    source: notification_center::NoticeSource::Workspace,
                    title,
                    detail: "Please try again later.".into(),
                    action: None,
                    lifetime,
                },
                Instant::now(),
            );
            cx.notify();
        }
    }

    fn resume_profile_after_failed_restart(&mut self, cx: &mut Context<Self>) {
        if self
            .lifecycle
            .account()
            .is_some_and(|account| account.request_profile_refresh_if_active().is_err())
        {
            diagnostic!("Aeris account profile refresh could not resume after update rollback");
            self.post_workspace_notice(
                "profile_refresh",
                "Account profile refresh unavailable",
                notification_center::NoticeLifetime::Timed(Duration::from_secs(6)),
                cx,
            );
        }
    }

    fn cancel_update_restart_after_persistence_failure(
        &mut self,
        error: String,
        cx: &mut Context<Self>,
    ) {
        self.services.end_update_restart();
        self.notices.show_workspace_error(error.clone());
        if let Some(cleanup) = self
            .services
            .updater_mut()
            .and_then(|updater| updater.cancel_prepared_restart(error))
        {
            // PreparedRestart::drop only transfers child termination/reaping
            // to its dedicated cleanup worker, so releasing this UI-owned
            // handle cannot block GPUI on process shutdown.
            drop(cleanup);
        }
        self.resume_profile_after_failed_restart(cx);
    }

    fn commit_update_restart_after_persistence(
        &mut self,
        account_refresh: aeris_account_runtime::AccountRefreshQuiesce,
        cx: &mut Context<Self>,
    ) {
        self.services.end_update_restart();
        let Some(updater) = self.services.updater_mut() else {
            drop(account_refresh);
            self.resume_profile_after_failed_restart(cx);
            self.notices
                .show_workspace_error("update client is unavailable");
            cx.notify();
            return;
        };
        if let Err(error) = updater.commit_restart() {
            drop(account_refresh);
            self.cancel_update_restart_after_persistence_failure(error, cx);
            return;
        }
        self.overlays.close(OverlayKind::PlatformMenu);
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
        if self.services.update_restart_pending() || self.window.is_closing() {
            return;
        }
        self.persist_workspace_layout_if_changed(cx);
        let workspace_wait = self
            .saved
            .writer()
            .map(WorkspaceLayoutPersistence::shutdown_wait);
        let chart_chrome_wait = self.lifecycle.persistence().chart_chrome_shutdown_wait();
        let account_refresh = self.lifecycle.account_service().begin_refresh_quiesce();
        let account = self.lifecycle.account().cloned();

        self.services.await_update_restart_durability();
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
                                .saved
                                .writer()
                                .is_none_or(|persistence| {
                                    persistence.shutdown_generation_is_current(generation)
                                })
                        });
                        let chart_chrome_current = terminal
                            .lifecycle
                            .persistence()
                            .chart_chrome_shutdown_generation_is_current(chart_chrome_generation);
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
                && let Some(account) = account
                && account.request_profile_refresh_if_active().is_err()
            {
                // The UI owner is gone; only diagnostics can receive this
                // live-account failure during shutdown.
                diagnostic!("Aeris account profile refresh could not resume after update rollback");
            }
        })
        .detach();
    }

    pub(super) fn update_presentation(&self) -> Option<&UpdatePresentation> {
        self.services.updater().map(DesktopUpdater::presentation)
    }

    /// Attribution follows any pane holding the provider's session, including recovery.
    fn held_provider_attribution(&self, cx: &App) -> Option<aeris_contracts::ProviderAttribution> {
        self.tabs
            .iter()
            .flat_map(|workspace| &workspace.panes)
            .find_map(|pane| {
                let surface = pane.surface.read(cx);
                terminal_chrome::attribution_for_session(
                    aeris_market_runtime::built_in_provider_presentations(),
                    surface.provider,
                    surface
                        .connection_state
                        .unwrap_or(FeedConnectionState::Disconnected),
                )
            })
    }

    pub(super) fn platform_menu_overlay(
        &self,
        terminal: &Entity<Self>,
        viewport: gpui::Size<Pixels>,
        cx: &App,
    ) -> Option<AnyElement> {
        let anchor = self.overlays.platform_menu_anchor()?;
        let account = self
            .lifecycle
            .account()
            .map_or_else(aeris_desktop::account::unavailable_menu_state, |account| {
                account.menu_state()
            });
        Some(platform_menu_layer(
            terminal,
            &account,
            anchor,
            viewport,
            platform_menu::PlatformMenuContent {
                update: self.update_presentation(),
                window_frame: self.chrome.preferences().window_frame,
                drawing_toolbar_visible: self.chrome.preferences().drawing_toolbar.shown(),
                attribution: self.held_provider_attribution(cx),
                now_unix_seconds: self
                    .lifecycle
                    .clock()
                    .wall_unix_nanos()
                    .div_euclid(1_000_000_000),
            },
            &self.theme,
        ))
    }

    /// The floating favorites toolbar; `chart_top` is where the workspace content begins.
    pub(super) fn drawing_favorites_overlay(
        &self,
        terminal: &Entity<Self>,
        chart_top: f32,
        window: &Window,
        cx: &App,
    ) -> Option<AnyElement> {
        drawing_favorites_toolbar_layer(
            terminal,
            &DrawingFavoritesToolbar {
                favorites: self.chrome.preferences().drawing_favorites,
                menu: self.chrome.tool_menu(),
                state: self.active_surface().read(cx).drawing_toolbar_state(cx),
                default_origin: drawing_favorites_default_origin(chart_top),
                viewport: window.viewport_size(),
            },
            &self.theme,
        )
    }

    pub(super) fn drawing_tool_menu_overlay(
        &self,
        terminal: &Entity<Self>,
        window: &Window,
        cx: &App,
    ) -> Option<AnyElement> {
        if !self.chrome.tool_menu().is_open() {
            return None;
        }
        let active_tool = self
            .active_surface()
            .read(cx)
            .drawing_toolbar_state(cx)
            .active_tool;
        drawing_tool_menu_layer(
            terminal,
            self.chrome.tool_menu(),
            &self.chrome.preferences().drawing_favorites,
            active_tool,
            window.viewport_size(),
            window.rem_size(),
            &self.theme,
        )
    }

    pub(super) fn finish_chart_context_menu(
        &mut self,
        mut menu: ChartContextMenu,
        action: ChartContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_pane(menu.workspace_id, menu.pane_id, cx);
        menu.copy_feedback = None;
        match action {
            ChartContextAction::CopyPrice => {
                if let Some(price) = menu.copy_price.as_deref() {
                    cx.write_to_clipboard(ClipboardItem::new_string(price.to_string()));
                }
                self.show_chart_context_menu(menu, cx);
                self.show_chart_copy_feedback(action, window, cx);
                return;
            }
            ChartContextAction::CaptureMenu => {
                menu.capture_flyout_open = true;
                self.show_chart_context_menu(menu, cx);
            }
            ChartContextAction::CopyCapture => {
                let mut copied = None;
                self.update_context_menu_pane(
                    &menu,
                    |surface, surface_cx| copied = surface.copy_chart_capture(surface_cx),
                    cx,
                );
                let Some(copied) = copied else {
                    self.chart_menus.close_context();
                    cx.notify();
                    return;
                };
                // The menu stays open while the capture renders off the UI thread, then confirms
                // on the same row as Copy price does.
                self.show_chart_context_menu(menu.clone(), cx);
                cx.spawn_in(window, async move |terminal, cx| {
                    let copied = copied.await;
                    if let Err(error) = terminal.update_in(cx, |terminal, window, terminal_cx| {
                        if !terminal.chart_menus.context_shows(&menu) {
                            return;
                        }
                        if copied {
                            terminal.show_chart_copy_feedback(action, window, terminal_cx);
                        } else {
                            terminal.close_chart_context_menu(terminal_cx);
                        }
                    }) && terminal.upgrade().is_some()
                    {
                        diagnostic!("Live workspace could not receive chart copy result: {error}");
                    }
                })
                .detach();
            }
            ChartContextAction::SaveCapture => {
                self.chart_menus.close_context();
                self.update_context_menu_pane(&menu, WorkspaceSurface::save_chart_capture, cx);
            }
            ChartContextAction::Reset => {
                self.chart_menus.close_context();
                self.update_context_menu_pane(&menu, WorkspaceSurface::reset_chart_view, cx);
            }
            ChartContextAction::ClearDrawings => {
                self.chart_menus.close_context();
                self.update_context_menu_pane(&menu, WorkspaceSurface::clear_drawings, cx);
            }
            ChartContextAction::ClearIndicators => {
                self.chart_menus.close_context();
                self.update_context_menu_pane(&menu, WorkspaceSurface::clear_indicators, cx);
            }
            ChartContextAction::Split(direction) => {
                self.chart_menus.close_context();
                self.split_active_pane(direction, window, cx);
            }
            ChartContextAction::Close => {
                self.chart_menus.close_context();
                self.close_active_pane(&ClosePane, window, cx);
            }
            ChartContextAction::Settings => {
                let workspace_id = menu.workspace_id;
                if let Some(replaced) = self.chart_menus.open_settings(menu) {
                    self.set_workspace_chart_pointers_suspended(replaced, false, cx);
                }
                self.set_workspace_chart_pointers_suspended(workspace_id, true, cx);
            }
        }
        cx.notify();
    }

    /// Shows "Copied" on the open menu's `action` row, then closes the menu unless a newer copy
    /// or a reopened menu has taken over.
    fn show_chart_copy_feedback(
        &mut self,
        action: ChartContextAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = self.chart_menus.begin_copy_feedback(action) else {
            return;
        };
        cx.notify();
        cx.spawn_in(window, async move |terminal, cx| {
            cx.background_executor()
                .timer(CHART_COPY_FEEDBACK_DURATION)
                .await;
            if let Err(error) = terminal.update_in(cx, |terminal, _, terminal_cx| {
                if terminal.chart_menus.copy_feedback_is_current(generation) {
                    terminal.close_chart_context_menu(terminal_cx);
                }
            }) && terminal.upgrade().is_some()
            {
                diagnostic!("Live workspace chart copy feedback could not close: {error}");
            }
        })
        .detach();
    }

    fn update_context_menu_pane(
        &self,
        menu: &ChartContextMenu,
        update: impl FnOnce(&mut WorkspaceSurface, &mut Context<WorkspaceSurface>),
        cx: &mut Context<Self>,
    ) {
        if let Some(workspace) = self
            .tabs
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            && let Some(pane) = workspace.panes.iter().find(|pane| pane.id == menu.pane_id)
        {
            pane.surface.update(cx, update);
        }
    }

    fn context_menu_chart_objects(&self, menu: &ChartContextMenu, cx: &App) -> (bool, bool) {
        self.tabs
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
        self.tabs
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
        if let PriceAxisMenuAction::SetLeft(next_left) = action {
            self.chart_menus.set_price_axis_left(next_left);
        }
        cx.notify();
    }

    /// Hovering a chart menu row opens the capture chart submenu on its own row and closes it on
    /// any other, so the submenu follows the pointer without a dismiss timer.
    pub(super) fn set_chart_capture_flyout(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.chart_menus.set_capture_flyout(open) {
            cx.notify();
        }
    }

    pub(super) fn toggle_price_axis_flyout(
        &mut self,
        flyout: PriceAxisMenuFlyout,
        cx: &mut Context<Self>,
    ) {
        if self.chart_menus.toggle_price_axis_flyout(flyout) {
            cx.notify();
        }
    }

    fn broadcast_indicator_chrome(&mut self, menu: &ChartContextMenu, cx: &mut Context<Self>) {
        let (names, values, price_lines) = self
            .tabs
            .iter()
            .find(|workspace| workspace.id == menu.workspace_id)
            .and_then(|workspace| workspace.panes.iter().find(|pane| pane.id == menu.pane_id))
            .and_then(|pane| pane.surface.read(cx).chart.clone())
            .map_or(
                (
                    self.chrome.preferences().indicator_name_labels_visible,
                    self.chrome.preferences().indicator_value_labels_visible,
                    self.chrome.preferences().indicator_price_lines_visible,
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
        self.chrome.preferences_mut().indicator_name_labels_visible = names;
        self.chrome.preferences_mut().indicator_value_labels_visible = values;
        self.chrome.preferences_mut().indicator_price_lines_visible = price_lines;
        for workspace in self.tabs.iter() {
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
        self.save_chart_chrome_preferences(cx);
    }

    /// Queues the durable chrome preferences off the UI thread. Chart type and the symbol
    /// search categories and scope change inside a workspace, so they are taken from the
    /// active one first.
    pub(super) fn save_chart_chrome_preferences(&mut self, cx: &mut Context<Self>) {
        let active = self.active_surface();
        let active = active.read(cx);
        self.chrome.preferences_mut().chart_type = active.chart_type(cx);
        self.chrome.preferences_mut().symbol_search_categories =
            active.chart_chrome.symbol_search_categories;
        self.chrome.preferences_mut().symbol_search_scope = active.chart_chrome.symbol_search_scope;
        let preferences = *self.chrome.preferences();
        let persistence = self.lifecycle.persistence().clone();
        match persistence.request_chart_chrome_preferences_save(preferences) {
            Ok(true) => {
                let save = cx
                    .background_executor()
                    .spawn(async move { persistence.run_chart_chrome_preferences_save_worker() });
                cx.spawn(async move |terminal, cx| {
                    let result = save.await;
                    if let Err(error) = terminal.update(cx, |terminal, cx| {
                        match result {
                            Ok(()) => terminal.notices.toaster_mut().clear_notice(
                                &notification_center::NoticeKey {
                                    scope: 0,
                                    kind: "chart_chrome_save",
                                },
                                Instant::now(),
                            ),
                            Err(_error) => terminal.post_chart_chrome_save_notice(cx),
                        }
                        cx.notify();
                    }) {
                        diagnostic!(
                            "Chart preference save result could not reach the desktop: {error}"
                        );
                    }
                })
                .detach();
            }
            Ok(false) => {}
            Err(_error) => {
                self.post_chart_chrome_save_notice(cx);
            }
        }
    }

    fn post_chart_chrome_save_notice(&mut self, cx: &mut Context<Self>) {
        if let Some(host) = toast_host(self.tabs.active()) {
            self.notices.toaster_mut().post_notice(
                host,
                notification_center::UserNotice {
                    key: notification_center::NoticeKey {
                        scope: 0,
                        kind: "chart_chrome_save",
                    },
                    severity: notification_center::NoticeSeverity::Error,
                    tone: notification_center::NoticeTone::Loss,
                    source: notification_center::NoticeSource::Persistence,
                    title: "Chart preferences could not be saved",
                    detail: "Your latest chart preferences may not survive a restart.".into(),
                    action: Some(notification_center::NoticeAction::Retry(
                        notification_center::RetryTarget::ChartChromePreferences,
                    )),
                    lifetime: notification_center::NoticeLifetime::UntilCleared,
                },
                Instant::now(),
            );
            cx.notify();
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
            .tabs
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
        else {
            return;
        };
        let current = workspace.layout.layout();
        if current
            .boundary_ratio(left_pane_id, right_pane_id)
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

    pub(super) fn persist_workspace_layout_if_changed(&mut self, cx: &App) {
        if self.window.is_closing() {
            return;
        }
        let layout = workspace_layout_tabs(self.tabs.as_slice(), cx);
        self.autosave_layouts(&layout);
        let snapshot = WorkspaceSnapshot {
            layout,
            active_workspace_id: self.tabs.active().id,
            watchlist: self.watchlist_entries(),
        };
        match self.saved.save_if_changed(snapshot) {
            WorkspaceSave::Failed(error) => self.notices.show_workspace_error(error),
            WorkspaceSave::Unavailable | WorkspaceSave::Unchanged | WorkspaceSave::Requested => {}
        }
    }

    fn select_workspace(&mut self, next: usize, cx: &mut Context<Self>) {
        if next < self.tabs.len() {
            self.show_terminal_view(cx);
            self.hide_start_page(cx);
        }
        let Some((previous, next)) = self.tabs.select(next) else {
            return;
        };
        self.set_workspace_resource_class(previous, ConsumerResourceClass::Background, cx);
        self.set_workspace_resource_class(next, ConsumerResourceClass::Foreground, cx);
        self.notices.clear_workspace_error();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    pub(super) fn select_workspace_id(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self.tabs.position(tab_id) {
            self.select_workspace(index, cx);
        }
    }

    pub(super) fn select_and_focus_workspace(
        &mut self,
        next: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if next >= self.tabs.len() {
            return;
        }
        self.select_workspace(next, cx);
        self.tabs[next].focus.focus(window, cx);
    }

    pub(super) fn select_relative_workspace(
        &mut self,
        current: usize,
        direction: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(next) = self.tabs.wrapped_index(current, direction) {
            self.select_and_focus_workspace(next, window, cx);
        }
    }

    /// Ctrl+Tab moves between workspaces the way a browser moves between tabs: keyboard focus
    /// lands in the selected workspace, not on its tab, which a frameless window may not show.
    fn cycle_workspace(&mut self, direction: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(next) = self.tabs.wrapped_index(self.tabs.active_index(), direction) {
            self.select_workspace(next, cx);
            self.focus_workspace(window, cx);
        }
    }

    pub(super) fn select_next_workspace(
        &mut self,
        _: &SelectNextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_workspace(1, window, cx);
    }

    pub(super) fn select_previous_workspace(
        &mut self,
        _: &SelectPreviousWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_workspace(-1, window, cx);
    }

    /// Reordering leaves keyboard focus where it was; tab handles are keyed by workspace, so a
    /// focused tab stays focused as it moves.
    fn move_active_workspace(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.tabs.move_active(direction) {
            self.persist_workspace_layout_if_changed(cx);
            cx.notify();
        }
    }

    pub(super) fn move_workspace_left(
        &mut self,
        _: &MoveWorkspaceLeft,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_active_workspace(-1, cx);
    }

    pub(super) fn move_workspace_right(
        &mut self,
        _: &MoveWorkspaceRight,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_active_workspace(1, cx);
    }

    pub(super) fn close_active_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pages.layouts.start_page_shown() {
            self.close_start_page(window, cx);
            return;
        }
        let tab_id = self.tabs.active().id;
        self.close_workspace(tab_id, window, cx);
    }

    pub(super) fn begin_workspace_drag(
        &mut self,
        tab_id: u64,
        cursor_offset_x: f32,
        tab_width: f32,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.begin_drag(tab_id, cursor_offset_x, tab_width) {
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
        match self
            .tabs
            .move_drag(tab_id, pointer_x, strip_left, tab_widths)
        {
            TabDragMove::Unchanged => {}
            TabDragMove::Moved => cx.notify(),
            TabDragMove::Reordered => {
                self.persist_workspace_layout_if_changed(cx);
                cx.notify();
            }
        }
    }

    pub(super) fn end_workspace_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.tabs.end_drag() {
            self.release_frameless_title_bar(window, cx);
            cx.notify();
        }
    }

    /// Closes a tab, first asking whether to save a layout it shows with unsaved changes.
    pub(super) fn close_workspace(
        &mut self,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let unsaved = self
            .tabs
            .position(tab_id)
            .is_some_and(|index| self.layout_tab_has_unsaved_changes(index, cx));
        if unsaved {
            self.prompt_layout_close(layouts_view::ClosePrompt::Tab(tab_id), window, cx);
            return;
        }
        self.close_workspace_now(tab_id, window, cx);
    }

    /// Closes a tab without asking; unsaved changes of a layout no other tab shows are
    /// discarded.
    pub(super) fn close_workspace_now(
        &mut self,
        tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next_active_id = match self.tabs.close_plan(tab_id) {
            TabClose::LastTab => {
                self.retire_market_summaries(cx);
                self.retire_market_screener(cx);
                self.retire_workspaces(cx);
                window.remove_window();
                return;
            }
            TabClose::NotOpen => return,
            TabClose::Remove { next_active_id } => next_active_id,
        };
        // Publishes the closing tab's last drawing edits while its layout still has a tab.
        self.sync_layout_drawings(cx);
        let Some(removed) = self.tabs.remove(tab_id, next_active_id) else {
            return;
        };
        let focus_next_tab = removed.tab.focus.is_focused(window);
        let focus_next_workspace = !focus_next_tab && removed.was_active;
        if removed.drag_cancelled {
            cx.stop_active_drag(window);
        }
        for pane in removed.tab.panes {
            pane.surface.update(cx, |workspace, workspace_cx| {
                workspace.set_market_resource_class(ConsumerResourceClass::Detached, workspace_cx);
                workspace.retire_market_worker(workspace_cx);
            });
        }
        self.retain_open_layout_drawings();
        self.set_workspace_resource_class(
            self.tabs.active_index(),
            ConsumerResourceClass::Foreground,
            cx,
        );
        self.sync_market_summaries(cx);
        if focus_next_tab {
            self.tabs.active().focus.focus(window, cx);
        } else if focus_next_workspace {
            self.focus_workspace(window, cx);
        }
        self.notices.clear_workspace_error();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    /// Refuses another workspace tab past the desktop's bound or the plan's limit.
    pub(super) fn check_workspace_capacity(&self) -> Result<(), String> {
        let open = self.tabs.len();
        if open >= MAXIMUM_OPEN_WORKSPACES {
            return Err(format!(
                "At most {MAXIMUM_OPEN_WORKSPACES} workspaces can be open"
            ));
        }
        if let Some(maximum) = aeris_desktop::account::plan_policy().open_workspaces
            && open >= maximum
        {
            return Err(format!(
                "Your plan supports at most {maximum} open workspaces"
            ));
        }
        Ok(())
    }

    /// Opens a new tab with one chart of the active chart's market and interval, showing a
    /// new layout named `name`.
    ///
    /// # Errors
    ///
    /// Returns an error when no more tabs can open, the name is invalid or taken, the
    /// library is full or the chart cannot start.
    pub(super) fn add_workspace(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.check_workspace_capacity()?;
        let factory = self
            .services
            .market_factory()
            .cloned()
            .ok_or_else(|| "This window cannot open another tab".to_string())?;
        self.saved.layouts().check_new_layout(name)?;
        let active = self.active_surface();
        let (product, interval) = {
            let active = active.read(cx);
            let product = active
                .product
                .clone()
                .ok_or_else(|| "The active workspace has no market to copy".to_string())?;
            (product, active.interval)
        };
        let pane = factory.create_workspace(product, interval)?;
        let workspace_id = pane.workspace_id;
        let pane_id = pane.pane_id;
        let consumer_id = pane.consumer_id;
        let layout = match AerisChartWorkspace::new(pane_id, CHART_PANE_CAPACITY) {
            Ok(layout) => layout,
            Err(error) => {
                let mut pane = pane;
                if let Some(retirement) = pane.worker.begin_retirement() {
                    self.lifecycle.retire_market_worker(retirement, cx);
                }
                return Err(error.to_string());
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
        Self::wire_pane_notices(&surface, cx);
        surface.update(cx, |workspace, workspace_cx| {
            workspace.apply_theme(&self.theme, workspace_cx);
            workspace.set_market_message_wake(self.frames.market().callback());
        });
        let mut workspace = WorkspaceTab {
            id: workspace_id,
            label: String::new(),
            saved_layout_id: 0,
            autosaved_snapshot: None,
            panes: vec![WorkspacePane {
                id: pane_id,
                consumer_id,
                surface: surface.clone(),
            }],
            active_pane: 0,
            layout,
            maximized_pane: None,
            swallow_pane_mouse_up: false,
            generation: 1,
            focus: cx.focus_handle(),
        };
        if let Err(error) = self.assign_new_layout(&mut workspace, name, cx) {
            surface.update(cx, |surface, surface_cx| {
                surface.set_market_resource_class(ConsumerResourceClass::Detached, surface_cx);
                surface.retire_market_worker(surface_cx);
            });
            return Err(error);
        }
        cx.observe(&surface, |app, surface, cx| {
            if surface.update(cx, |surface, _| surface.take_chart_persistence_dirty()) {
                app.persist_workspace_layout_if_changed(cx);
            }
            cx.notify();
        })
        .detach();
        self.show_terminal_view(cx);
        self.set_workspace_resource_class(
            self.tabs.active_index(),
            ConsumerResourceClass::Background,
            cx,
        );
        self.tabs.push_active(workspace);
        self.notices.clear_workspace_error();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
        Ok(())
    }

    pub(super) fn new_workspace(
        &mut self,
        _: &NewWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_start_page(window, cx);
    }

    fn split_active_pane(
        &mut self,
        split_direction: ChartSplitDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(factory) = self.services.market_factory().cloned() else {
            self.notices
                .show_workspace_error("This window cannot open another chart pane");
            cx.notify();
            return;
        };
        let workspace = self.tabs.active();
        if workspace.panes.len() >= CHART_PANE_CAPACITY {
            self.notices.show_workspace_error(format!(
                "At most {CHART_PANE_CAPACITY} chart panes fit in one workspace"
            ));
            cx.notify();
            return;
        }
        let (product, interval, drawing_tool, side_panels, side_panel_size) = {
            let source = workspace.panes[workspace.active_pane].surface.read(cx);
            let Some(product) = source.product.clone() else {
                self.notices
                    .show_workspace_error("The active pane has no market to copy");
                cx.notify();
                return;
            };
            (
                product,
                source.interval,
                self.chrome
                    .tool_menu()
                    .armed_choice(source.drawing_toolbar_state(cx).active_tool),
                source.chrome.side_panels(),
                source.chrome.side_panel_size(),
            )
        };
        let source_surface = workspace.panes[workspace.active_pane].surface.clone();
        let workspace_id = workspace.id;
        let insertion_index = workspace.active_pane.saturating_add(1);
        let pane = match factory.create_pane(workspace_id, product, interval) {
            Ok(pane) => pane,
            Err(error) => {
                self.notices.show_workspace_error(error);
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
        Self::wire_pane_notices(&surface, cx);
        surface.update(cx, |surface, surface_cx| {
            surface.apply_theme(&self.theme, surface_cx);
            surface.set_market_resource_class(ConsumerResourceClass::Foreground, surface_cx);
            surface.set_market_message_wake(self.frames.market().callback());
            surface.select_drawing_tool(drawing_tool, surface_cx);
            surface.chrome.set_side_panel_size(side_panel_size);
            surface.apply_side_panels(side_panels, surface_cx);
        });
        cx.observe(&surface, |app, surface, cx| {
            if surface.update(cx, |surface, _| surface.take_chart_persistence_dirty()) {
                app.persist_workspace_layout_if_changed(cx);
            }
            cx.notify();
        })
        .detach();
        let workspace = self.tabs.active_mut();
        workspace.maximized_pane = None;
        workspace.swallow_pane_mouse_up = false;
        let source_pane_id = workspace.panes[workspace.active_pane].id;
        if let Err(error) = workspace
            .layout
            .split(source_pane_id, split_direction, pane.pane_id)
        {
            surface.update(cx, |surface, surface_cx| {
                surface.set_market_resource_class(ConsumerResourceClass::Detached, surface_cx);
                surface.retire_market_worker(surface_cx);
            });
            self.notices.show_workspace_error(error.to_string());
            cx.notify();
            return;
        }
        source_surface.update(cx, |surface, surface_cx| {
            surface.apply_side_panels(SidePanelVisibility::default(), surface_cx);
        });
        workspace.panes.insert(
            insertion_index,
            WorkspacePane {
                id: pane.pane_id,
                consumer_id: pane.consumer_id,
                surface,
            },
        );
        workspace.active_pane = insertion_index;
        workspace.generation = workspace.generation.saturating_add(1);
        self.notices.clear_workspace_error();
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
        let workspace = self.tabs.active_mut();
        if workspace.panes.len() == 1 {
            return;
        }
        workspace.maximized_pane = None;
        workspace.swallow_pane_mouse_up = false;
        let removed_pane_id = workspace.panes[workspace.active_pane].id;
        if let Err(error) = workspace.layout.remove(removed_pane_id) {
            self.notices.show_workspace_error(error.to_string());
            cx.notify();
            return;
        }
        let (removed_side_panels, removed_side_panel_size) = workspace.panes[workspace.active_pane]
            .surface
            .read_with(cx, |surface, _| {
                (
                    surface.chrome.side_panels(),
                    surface.chrome.side_panel_size(),
                )
            });
        let removed = workspace.panes.remove(workspace.active_pane);
        let pane_order = workspace.layout.layout().leaf_ids();
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
            surface.apply_side_panels(SidePanelVisibility::default(), surface_cx);
            surface.set_market_resource_class(ConsumerResourceClass::Detached, surface_cx);
            surface.retire_market_worker(surface_cx);
        });
        workspace.panes[recipient]
            .surface
            .update(cx, |surface, surface_cx| {
                surface.chrome.set_side_panel_size(removed_side_panel_size);
                surface.apply_side_panels(removed_side_panels, surface_cx);
            });
        self.focus_workspace(window, cx);
        self.notices.clear_workspace_error();
        self.persist_workspace_layout_if_changed(cx);
        cx.notify();
    }

    /// Switches the application theme (palette preset and color mode) everywhere at once.
    pub(super) fn set_theme(
        &mut self,
        theme: &AerisTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.theme == *theme {
            return;
        }
        self.theme = *theme;
        cx.update_global::<gpui_base::Theme, _>(|base, _| *base = base_theme(&self.theme));
        window.refresh();
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |workspace, workspace_cx| {
                    workspace.apply_theme(&self.theme, workspace_cx);
                });
            }
        }
        cx.notify();
    }

    pub(super) fn request_sign_in(&self, cx: &mut Context<Self>) {
        let result = self.lifecycle.account().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            aeris_desktop::account::DesktopAccount::request_sign_in,
        );
        if let Err(error) = result {
            diagnostic!("Aeris sign-in degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn sign_out(&self, cx: &mut Context<Self>) {
        let result = self.lifecycle.account().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            aeris_desktop::account::DesktopAccount::request_sign_out,
        );
        if let Err(error) = result {
            diagnostic!("Aeris sign-out degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn reopen_browser_page(&self, cx: &mut Context<Self>) {
        let result = self.lifecycle.account().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            aeris_desktop::account::DesktopAccount::reopen_browser,
        );
        if let Err(error) = result {
            diagnostic!("Aeris browser reopen degraded: {error}");
        }
        cx.notify();
    }

    pub(super) fn cancel_sign_in(&self, cx: &mut Context<Self>) {
        let result = self.lifecycle.account().map_or_else(
            || Err("sign-in is unavailable".to_string()),
            aeris_desktop::account::DesktopAccount::request_cancel,
        );
        if let Err(error) = result {
            diagnostic!("Aeris sign-in cancellation degraded: {error}");
        }
        cx.notify();
    }

    /// Shows or hides the drawing tools sidebar for every workspace and saves the choice.
    pub(super) fn set_drawing_toolbar_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let visibility = chart_chrome::ChromeVisibility::from_shown(visible);
        if self.chrome.preferences_mut().drawing_toolbar == visibility {
            return;
        }
        self.chrome.preferences_mut().drawing_toolbar = visibility;
        self.chrome.tool_menu_mut().close();
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, _| {
                    surface.chart_chrome.drawing_toolbar = visibility;
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }

    /// Shows or hides the chart Buy / quantity / Sell bar on every chart and saves the choice.
    pub(super) fn set_chart_trade_buttons_visible(
        &mut self,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        let visibility = chart_chrome::ChromeVisibility::from_shown(visible);
        if self.chrome.preferences_mut().chart_trade_buttons == visibility {
            return;
        }
        self.chrome.preferences_mut().chart_trade_buttons = visibility;
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |surface, surface_cx| {
                    surface.set_chart_trade_buttons(visibility, surface_cx);
                });
            }
        }
        self.save_chart_chrome_preferences(cx);
        cx.notify();
    }

    pub(super) fn toggle_drawing_tool_menu(&mut self, group: usize, cx: &mut Context<Self>) {
        self.chrome.tool_menu_mut().toggle(group);
        cx.notify();
    }

    pub(super) fn close_drawing_tool_menu(&mut self, cx: &mut Context<Self>) {
        if self.chrome.tool_menu_mut().close() {
            cx.notify();
        }
    }

    fn retire_workspaces<C: gpui::AppContext>(&mut self, cx: &mut C) {
        for workspace in self.tabs.iter() {
            for pane in &workspace.panes {
                pane.surface.update(cx, |workspace, workspace_cx| {
                    workspace.retire_market_worker(workspace_cx);
                });
            }
        }
    }

    fn retire_market_summaries(&mut self, cx: &App) {
        for entry in self.market_watch.summaries_mut().values_mut() {
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
        if self.window.is_closing() {
            return false;
        }
        self.sync_layout_drawings(cx);
        self.persist_workspace_layout_if_changed(cx);
        self.window.begin_closing();
        if let Some(persistence) = self.saved.writer() {
            self.lifecycle
                .await_workspace_persistence(persistence.shutdown_wait());
        }
        self.retire_market_summaries(cx);
        self.retire_market_screener(cx);
        self.retire_workspaces(cx);
        true
    }

    /// Closes the window, first asking whether to save layouts with unsaved changes.
    pub(super) fn close_window(
        &mut self,
        _: &CloseWindow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.window_should_close(window, cx) {
            window.remove_window();
        }
    }

    /// Whether the window may close now. A window with unsaved layouts asks first and stays
    /// open; otherwise the close is claimed, so it is claimed exactly once.
    pub(super) fn window_should_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.window.is_closing() {
            return true;
        }
        if !self.unsaved_layout_names(cx).is_empty() {
            self.prompt_layout_close(layouts_view::ClosePrompt::Window, window, cx);
            return false;
        }
        self.claim_close(cx)
    }

    /// Handles the open command palette's navigation keys, returning whether the key was used.
    fn command_palette_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event.keystroke.key.as_str() {
            "escape" => self.close_command_palette(window, cx),
            "up" => {
                if self.overlays.select_previous_command() {
                    cx.notify();
                }
            }
            "down" => {
                let query = self.overlays.command_palette_input().read(cx).value();
                let count = aeris_desktop::command_registry::search(&query, 8).len();
                if self.overlays.select_next_command(count) {
                    cx.notify();
                }
            }
            "enter" => self.execute_command_palette_query(window, cx),
            _ => return false,
        }
        true
    }

    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlays.is_open(OverlayKind::CommandPalette) {
            if self.command_palette_key_down(event, window, cx) {
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        if self.overlays.is_open(OverlayKind::KeyboardShortcuts) {
            if event.keystroke.key.as_str() == "escape" {
                self.close_keyboard_shortcuts(window, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        if self.pages.layouts.modal_open() {
            if event.keystroke.key.eq_ignore_ascii_case("escape") {
                self.cancel_layout_modal(window, cx);
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        if self.keyboard_trading.confirming() {
            if self.trading_confirmation_key_down(event, window, cx) {
                window.prevent_default();
                cx.stop_propagation();
            }
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape")
            && self.overlays.is_open(OverlayKind::PlatformMenu)
        {
            self.close_platform_menu(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape") && self.app_navigation_open() {
            self.close_app_navigation(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape") && self.chrome.tool_menu().is_open() {
            self.close_drawing_tool_menu(cx);
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
        if event.keystroke.key.as_str() == "escape" && self.chart_menus.is_open() {
            self.close_chart_context_menu(cx);
            self.close_chart_settings_menu(cx);
            cx.stop_propagation();
            return;
        }
        if self.tabs.is_dragging() && event.keystroke.key.as_str() == "escape" {
            cx.stop_active_drag(window);
            self.end_workspace_drag(window, cx);
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape") && self.pages.layouts.menu_open() {
            self.close_layout_menus(cx);
            cx.stop_propagation();
            return;
        }
        if self.chart_menus.is_open()
            || self.pages.view != market_screener::AppView::Terminal
            || self.pages.layouts.start_page_shown()
        {
            return;
        }
        if event.keystroke.key.eq_ignore_ascii_case("escape") && self.window.chart_fullscreen() {
            self.toggle_chart_fullscreen(window, cx);
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if self.apply_chart_shortcut(event, window, cx) {
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        let workspace_focused = self.workspace_keyboard_focused(window, cx);
        let handled = self.active_surface().update(cx, |workspace, workspace_cx| {
            workspace.on_terminal_key_down(event, workspace_focused, window, workspace_cx)
        });
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    }

    pub(super) fn track_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let window_active = window.is_window_active();
        let change = self.window.observe_activity(window_active);
        if change == ActivityChange::Deactivated {
            // A key released in another window never reaches this one.
            self.keyboard_trading.release_key();
        }
        if !window_active {
            self.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
            if self.tabs.is_dragging() {
                cx.stop_active_drag(window);
                self.end_workspace_drag(window, cx);
            }
        }
        if let ActivityChange::Activated { refresh_profile } = change {
            if refresh_profile
                && let Some(account) = self.lifecycle.account()
                && account.request_profile_refresh_if_active().is_err()
            {
                diagnostic!("Aeris profile refresh degraded");
                self.post_workspace_notice(
                    "profile_refresh",
                    "Account profile refresh unavailable",
                    notification_center::NoticeLifetime::Timed(Duration::from_secs(6)),
                    cx,
                );
            }
            self.schedule_market_frame(window, cx);
        }
    }

    pub(super) fn handle_window_move_gesture(
        &mut self,
        event: WindowMoveGestureEvent,
        window: &mut Window,
    ) {
        if self.window.move_gesture(event) {
            window.start_window_move();
        }
    }

    /// Schedules the single presentation boundary for live market state.
    ///
    /// `on_next_frame` follows the window's compositor cadence, so the bounded
    /// mailbox is drained at the active display's refresh rate without a second
    /// fixed-frequency timer in an individual chart or order-book surface.
    pub(super) fn schedule_market_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.frames.try_schedule_frame() {
            return;
        }
        let terminal = cx.entity();
        window.on_next_frame(move |window, cx| {
            let diagnostics = terminal.update(cx, |terminal, cx| {
                terminal.frames.frame_ran();
                if !terminal.window.is_closing()
                    && terminal
                        .saved
                        .writer()
                        .is_some_and(WorkspaceLayoutPersistence::poll)
                {
                    let error = terminal
                        .saved
                        .writer()
                        .and_then(WorkspaceLayoutPersistence::error);
                    terminal.notices.set_workspace_error(error);
                    cx.notify();
                }
                let authenticated = terminal
                    .lifecycle
                    .account()
                    .is_none_or(aeris_desktop::account::DesktopAccount::authenticated);
                let mut diagnostics = Vec::new();
                let mut summaries_changed = false;
                if authenticated {
                    for entry in terminal.market_watch.summaries_mut().values_mut() {
                        summaries_changed |= entry.poll();
                    }
                }
                if summaries_changed {
                    cx.notify();
                }
                terminal.poll_market_screener(window, cx);
                for workspace in terminal.tabs.iter() {
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

fn start_trading_store(
    lifecycle: &DesktopLifecycle,
    wake: &UiWake,
    cx: &mut Context<TerminalApp>,
) -> Entity<trading_store::TradingStore> {
    let store = cx.new(|_| trading_store::TradingStore::default());
    lifecycle.register_trading_store(&store);
    // GPUI's deterministic test scheduler cannot be woken from the real
    // subscription thread. UI fixtures use an empty store; the bridge and its
    // owner lifecycle are exercised by trading_store's dedicated tests.
    #[cfg(test)]
    if lifecycle.fixture_without_trading_bridge() {
        return store;
    }
    let service = lifecycle.trading().clone();
    let wake = wake.callback();
    let bridge_store = store.clone();
    let subscription = cx
        .background_executor()
        .spawn(async move { service.subscribe() });
    cx.spawn(async move |_, cx| match subscription.await {
        Ok(subscription) => {
            bridge_store.update(cx, |store, _| {
                if let Err(error) = store.start(subscription, wake) {
                    diagnostic!("Trading view bridge unavailable: {error}");
                }
            });
        }
        Err(error) => diagnostic!("Trading view subscription unavailable: {error}"),
    })
    .detach();
    store
}

impl TerminalApp {
    pub(super) fn start_market_wake_listener(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.frames.claim_listener_start() {
            return;
        }
        self.start_account_wake_listener(window, cx);
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        let restart_prepared =
                            if let Some(updater) = terminal.services.updater_mut() {
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
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let wake = self.frames.market().clone();
        self.lifecycle
            .trading_feedback()
            .set_request_wake(wake.callback());
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                wake.notified().await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        if terminal
                            .services
                            .trading_store()
                            .read(terminal_cx)
                            .has_pending()
                        {
                            terminal.apply_trading_publication(terminal_cx);
                            terminal_cx.notify();
                        }
                        if terminal.lifecycle.trading_feedback().has_request_results() {
                            terminal.apply_trading_request_results(terminal_cx);
                        }
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

    /// Re-renders when the account presentation changes. Authentication
    /// must progress even when a suspended provider has no events to wake
    /// this window, so a change also schedules a frame that drains retained
    /// market data.
    fn start_account_wake_listener(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.lifecycle.account() else {
            return;
        };
        let wake = UiWake::default();
        let registration = wake.callback();
        if let Err(error) = account.register_change_wake(&registration) {
            diagnostic!("Aeris account change wake unavailable: {error}");
            return;
        }
        cx.spawn_in(window, async move |terminal, cx| {
            loop {
                wake.notified().await;
                if terminal
                    .update_in(cx, |terminal, window, terminal_cx| {
                        terminal.schedule_market_frame(window, terminal_cx);
                        terminal_cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
            drop(registration);
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
        let context_menu = self.chart_menus.context().cloned().map(|menu| {
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
            if self.services.market_factory().is_some() && pane_count < CHART_PANE_CAPACITY {
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
        let settings_menu = self.chart_menus.settings().and_then(|panel| {
            let menu = panel.target();
            self.chart_settings_snapshot(menu, cx).map(|snapshot| {
                chart_settings_menu_layer(
                    terminal,
                    menu,
                    ChartSettingsView {
                        section: panel.section(),
                        snapshot: &snapshot,
                        color_picker: panel.color_picker(),
                        open_select: panel.open_select(),
                        templates: ChartSettingsTemplateView {
                            overlay: panel.template_overlay(),
                            name_input: panel.template_name(),
                            error: panel.template_error(),
                            templates: self.saved.chart_settings_templates(),
                            apply_to_all: self
                                .tabs
                                .iter()
                                .find(|workspace| workspace.id == menu.workspace_id)
                                .is_some_and(|workspace| workspace.panes.len() > 1),
                        },
                    },
                    panel.origin(),
                    viewport,
                    &self.theme,
                )
            })
        });
        (context_menu, settings_menu)
    }
}
