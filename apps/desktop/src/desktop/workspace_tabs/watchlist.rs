use super::super::market_watch::{DragMove, FlagPress, PickerColor};
use super::*;
use aeris_localization::{shell, tr};

impl TerminalApp {
    pub(in crate::desktop) fn watchlist_entries(&self) -> Vec<WorkspaceWatchlistEntryState> {
        self.market_watch.durable_entries()
    }

    pub(in crate::desktop) fn watchlist_rows(&self, cx: &App) -> Vec<WatchlistRow> {
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
                                .then(|| tr!(shell::MARKET_DATA_UNAVAILABLE).to_string())
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

    pub(in crate::desktop) fn watchlist_panel_state(&self, cx: &App) -> WatchlistPanelState {
        WatchlistPanelState {
            rows: self.watchlist_rows(cx),
            drag: self.market_watch.drag().cloned(),
            scroll: self.market_watch.scroll().clone(),
            focus: self.market_watch.focus().clone(),
            flags: self.market_watch.flags_view(),
        }
    }

    pub(in crate::desktop) fn reconcile_active_drags(&mut self, cx: &mut Context<Self>) {
        if cx.has_active_drag() {
            return;
        }
        self.tabs.end_drag();
        self.end_watchlist_drag(cx);
    }

    pub(in crate::desktop) fn absorb_watchlist_requests(&mut self, cx: &mut Context<Self>) {
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

    pub(in crate::desktop) fn sync_market_summaries(&mut self, cx: &App) {
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
                    self.frames.dirty(),
                ),
            );
        }
    }

    pub(in crate::desktop) fn add_watchlist_instrument(
        &mut self,
        instrument: InstallProviderInstrument,
        cx: &mut Context<Self>,
    ) -> market_screener::WatchlistAddOutcome {
        if self
            .market_watch
            .lists(&instrument.provider, &instrument.instrument_id)
        {
            return market_screener::WatchlistAddOutcome::AlreadyListed;
        }
        if !self.market_watch.add(instrument) {
            return market_screener::WatchlistAddOutcome::Full;
        }
        self.sync_market_summaries(cx);
        self.saved.mark_unsaved(UnsavedEdit::Watchlist);
        self.record_layout_change(cx);
        cx.notify();
        market_screener::WatchlistAddOutcome::Added
    }

    pub(in crate::desktop) fn remove_watchlist_instrument(
        &mut self,
        provider: &str,
        instrument_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.market_watch.remove(provider, instrument_id) {
            self.sync_market_summaries(cx);
            self.saved.mark_unsaved(UnsavedEdit::Watchlist);
            self.record_layout_change(cx);
            cx.notify();
        }
    }

    pub(in crate::desktop) fn begin_watchlist_drag(
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

    pub(in crate::desktop) fn move_watchlist_drag(
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

    pub(in crate::desktop) fn end_watchlist_drag(&mut self, cx: &mut Context<Self>) {
        if !self.market_watch.end_drag() {
            return;
        }
        self.record_layout_change(cx);
        cx.notify();
    }

    /// A left press on a row's flag: an unflagged row takes the default flag and opens its
    /// color picker; a flagged row loses its flag.
    pub(in crate::desktop) fn press_watchlist_flag(
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
    pub(in crate::desktop) fn toggle_watchlist_flag_picker(
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
    pub(in crate::desktop) fn apply_watchlist_flag_color(
        &mut self,
        color: &str,
        cx: &mut Context<Self>,
    ) {
        match self.market_watch.apply_picker_color(color) {
            PickerColor::Closed => return,
            PickerColor::Applied => self.saved.mark_unsaved(UnsavedEdit::Watchlist),
            PickerColor::Rejected | PickerColor::Accepted => {}
        }
        cx.notify();
    }

    pub(in crate::desktop) fn close_watchlist_flag_picker(&mut self, cx: &mut Context<Self>) {
        if self.market_watch.close_picker() {
            self.save_watchlist_flags(cx);
        }
    }

    fn save_watchlist_flags(&mut self, cx: &mut Context<Self>) {
        self.saved.mark_unsaved(UnsavedEdit::Watchlist);
        self.record_layout_change(cx);
        cx.notify();
    }

    pub(in crate::desktop) fn toggle_watchlist_flag_filter_menu(&mut self, cx: &mut Context<Self>) {
        self.market_watch.toggle_filter_menu();
        cx.notify();
    }

    pub(in crate::desktop) fn close_watchlist_flag_filter_menu(&mut self, cx: &mut Context<Self>) {
        if self.market_watch.close_filter_menu() {
            cx.notify();
        }
    }

    /// Shows only the rows flagged with `filter`, or every row for `None`.
    pub(in crate::desktop) fn set_watchlist_flag_filter(
        &mut self,
        filter: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.market_watch.set_filter(filter);
        cx.notify();
    }
}
