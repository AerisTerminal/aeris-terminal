use super::*;

fn active_header_state(
    workspace: &WorkspaceSurface,
    theme: &AxiusflowTheme,
    chart_has_market_data: bool,
    cx: &App,
) -> HeaderState {
    HeaderState {
        theme: *theme,
        provider: workspace.provider,
        instrument_label: terminal_instrument_label(workspace),
        series_label: series_selector_label(workspace.selected_interval()),
        chart_type: workspace.chart_type(cx),
        chart_type_label: workspace.chart_type(cx).label().to_string(),
        instruments: workspace.instrument_entries(cx),
        symbol_input: workspace.symbol_input.clone(),
        indicator_input: workspace.indicator_input.clone(),
        indicator_message: workspace.indicator_message.clone(),
        series_message: workspace.series_message.clone(),
        pending: HeaderPendingState {
            symbol_selection: workspace.market_state.symbol_selection_pending,
            series: workspace.rithmic_switch.in_progress(),
        },
        drawing_history: workspace.drawing_history_state(cx),
        controls: HeaderControls::from_state(
            workspace.symbol_input.is_some() || !workspace.symbol_browser.results().is_empty(),
            workspace.has_market_selection(),
        )
        .with_chart_controls(chart_has_market_data),
        order_book_visible: workspace.side_panels.contains(SidePanel::OrderBook),
        watchlist_visible: workspace.side_panels.contains(SidePanel::Watchlist),
        connection_state: workspace
            .connection_state
            .unwrap_or(FeedConnectionState::Disconnected),
        transport_rtt_nanos: workspace.provider_transport_rtt_nanos,
        instrument_scroll: workspace.scrolls.instrument.clone(),
    }
}

impl TerminalApp {
    fn absorb_render_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.absorb_pane_activate_requests(cx);
        self.absorb_watchlist_requests(cx);
        self.absorb_chart_context_menu_requests(cx);
        self.absorb_study_settings_requests(window, cx);
        self.absorb_study_remove_requests(cx);
    }

    fn rendered_title_bar(
        &self,
        terminal: &Entity<Self>,
        window: &Window,
        fullscreen: bool,
        cx: &App,
    ) -> Option<impl IntoElement + use<>> {
        workspace_title_bar_visible(fullscreen).then(|| {
            workspace_title_bar(
                terminal,
                &WorkspaceTabBarState {
                    workspaces: &self.workspaces,
                    active: self.active,
                    enabled: self.workspace_factory.is_some(),
                    error: self.workspace_error.as_deref(),
                    workspace_drag: self.workspace_drag,
                    theme: self.theme,
                },
                window,
                cx,
            )
        })
    }

    fn rendered_about_dialog(&self, terminal: &Entity<Self>) -> Option<AnyElement> {
        self.about_dialog_open
            .then(|| about_dialog_layer(terminal, self.update_presentation(), &self.theme))
    }

    fn rendered_header(
        &self,
        terminal: &Entity<Self>,
        surface: &Entity<WorkspaceSurface>,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let workspace = surface.read(cx);
        let has_data = workspace
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        terminal_header(
            terminal,
            surface,
            active_header_state(workspace, &self.theme, has_data, cx),
        )
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_market_wake_listener(window, cx);
        self.schedule_market_frame(window, cx);
        if !axiusflow_desktop::account::DesktopAccount::shared()
            .is_some_and(|account| account.authenticated())
        {
            return onboarding::onboarding_surface(window, &self.theme, None);
        }
        if self.workspace_drag.is_some() && !cx.has_active_drag() {
            self.workspace_drag = None;
        }
        self.track_window_activation(window, cx);
        self.absorb_render_requests(window, cx);
        let terminal = cx.entity();
        let pane_count = self.workspaces[self.active].panes.len();
        let active = self.active_surface();
        let workspace = active.read(cx);
        let chart_has_market_data = workspace
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        let fullscreen = window.is_fullscreen();
        let overlay = chrome_overlay_layer(
            workspace,
            &active,
            &self.theme,
            chart_chrome::CHART_CHROME_HEIGHT
                + if fullscreen {
                    0.0
                } else {
                    WORKSPACE_TITLE_BAR_HEIGHT
                },
            window.viewport_size(),
            cx,
        );
        let (context_menu, settings_menu) = self.chart_surface_menus(
            &terminal,
            pane_count,
            chart_has_market_data,
            window.viewport_size(),
            cx,
        );
        let account_menu = self.account_menu_overlay(&terminal, window.viewport_size());
        let about_dialog = self.rendered_about_dialog(&terminal);
        let title_bar = self.rendered_title_bar(&terminal, window, fullscreen, cx);
        let header = self.rendered_header(&terminal, &active, cx);
        let watchlist = self.watchlist_rows(cx);
        let market = workspace_market_area(
            &terminal,
            &self.workspaces[self.active],
            &active,
            self.drawing_toolbar.is_collapsed(),
            watchlist,
            &self.theme,
            cx,
        );
        let fullscreen_focus = self.chrome_focus.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.chrome_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|terminal, _, window, cx| {
                    terminal.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
                    terminal.end_workspace_drag(cx);
                }),
            )
            .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
            .on_action(|_: &ZoomWindow, window, _| {
                WindowCommand::MaximizeOrRestore.execute(window);
            })
            .map(|root| workspace_action_handlers(root, cx))
            .on_action(move |_: &ToggleFullscreen, window, cx| {
                window.toggle_fullscreen();
                fullscreen_focus.focus(window, cx);
            })
            .on_action(cx.listener(Self::close_window))
            .bg(gpui_color(self.theme.colors.surface))
            .text_color(gpui_color(self.theme.colors.text_primary))
            .font_family(axiusflow_design_system::platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .children(title_bar)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui_color(self.theme.colors.surface))
                    .child(market),
            )
            .children(overlay)
            .children(context_menu)
            .children(settings_menu)
            .children(account_menu)
            .children(about_dialog)
    }
}

fn workspace_action_handlers(root: Div, cx: &mut Context<TerminalApp>) -> Div {
    root.on_action(cx.listener(TerminalApp::new_workspace))
        .on_action(cx.listener(TerminalApp::select_next_workspace))
        .on_action(cx.listener(TerminalApp::select_previous_workspace))
        .on_action(cx.listener(TerminalApp::move_workspace_left))
        .on_action(cx.listener(TerminalApp::move_workspace_right))
        .on_action(cx.listener(TerminalApp::close_active_workspace))
        .on_action(cx.listener(TerminalApp::split_pane_horizontal))
        .on_action(cx.listener(TerminalApp::split_pane_vertical))
        .on_action(cx.listener(TerminalApp::close_active_pane))
}

#[derive(Clone)]
struct WorkspaceTabDrag {
    tab_id: u64,
}

impl Render for WorkspaceTabDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
pub(super) struct WorkspaceSplitDrag {
    pub(super) workspace_id: u64,
    pub(super) left_pane_id: u64,
    pub(super) right_pane_id: u64,
    pub(super) direction: ChartSplitDirection,
}

impl Render for WorkspaceSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone, Copy)]
struct WorkspaceTabRenderState {
    index: usize,
    active: usize,
    workspace_count: usize,
    drag_enabled: bool,
    drag_translation: Option<f32>,
    theme: AxiusflowTheme,
}

const fn workspace_tab_close_drag_enabled(workspace_count: usize) -> bool {
    workspace_count > 1
}

fn workspace_tab_close_button(
    terminal: Entity<TerminalApp>,
    tab_id: u64,
    index: usize,
    label: &str,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let key_terminal = terminal.clone();
    div()
        .id(("close_workspace", tab_id))
        .occlude()
        .size(px(WORKSPACE_TAB_ICON_HIT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(format!("Close {label}"))
        .tab_index(isize::try_from(index.saturating_mul(2).saturating_add(1)).unwrap_or(isize::MAX))
        .hover(move |close| {
            close
                .bg(gpui_color(colors.danger))
                .text_color(gpui_color(colors.danger_foreground))
        })
        .focus_visible(move |close| close.border_2().border_color(gpui_color(colors.ring)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            terminal.update(cx, |terminal, cx| {
                terminal.close_workspace(tab_id, window, cx);
            });
            cx.stop_propagation();
        })
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                key_terminal.update(cx, |terminal, cx| {
                    terminal.close_workspace(tab_id, window, cx);
                });
                cx.stop_propagation();
            }
        })
        .child(header_icon(HugeIcon::CancelIcon01).with_size(px(WORKSPACE_TAB_ICON_GLYPH)))
}

fn handle_workspace_tab_key(
    terminal: &Entity<TerminalApp>,
    tab_id: u64,
    index: usize,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let key = event.keystroke.key.as_str();
    if !matches!(
        key,
        "left" | "right" | "home" | "end" | "enter" | "space" | "delete"
    ) {
        return;
    }
    terminal.update(cx, |terminal, cx| match key {
        "left" => terminal.select_relative_workspace(index, -1, window, cx),
        "right" => terminal.select_relative_workspace(index, 1, window, cx),
        "home" => terminal.select_and_focus_workspace(0, window, cx),
        "end" => {
            let last = terminal.workspaces.len().saturating_sub(1);
            terminal.select_and_focus_workspace(last, window, cx);
        }
        "enter" | "space" => terminal.select_workspace_id(tab_id, cx),
        "delete" => terminal.close_workspace(tab_id, window, cx),
        _ => {}
    });
    cx.stop_propagation();
}

fn workspace_add_button(
    terminal: Entity<TerminalApp>,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let key_terminal = terminal.clone();
    div()
        .id("add_workspace")
        .occlude()
        .size(px(WORKSPACE_TAB_ICON_HIT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(if enabled {
            colors.icon
        } else {
            colors.text_muted
        }))
        .role(Role::Button)
        .aria_label("Create workspace")
        .tab_index(isize::MAX)
        .tab_stop(enabled)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| {
                    button.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
                })
                .focus_visible(move |button| {
                    button.border_2().border_color(gpui_color(colors.ring))
                })
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    terminal.update(cx, |terminal, cx| terminal.add_workspace(window, cx));
                    cx.stop_propagation();
                })
                .on_key_down(move |event, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        key_terminal.update(cx, |terminal, cx| terminal.add_workspace(window, cx));
                        cx.stop_propagation();
                    }
                })
        })
        .child(header_icon(HugeIcon::AddIcon01).with_size(px(WORKSPACE_TAB_ICON_GLYPH)))
}

fn workspace_tab_content(
    workspace: &WorkspaceTab,
    theme: &AxiusflowTheme,
    cx: &App,
) -> (String, String, Div) {
    let surface = workspace.panes[workspace.active_pane].surface.read(cx);
    let label = terminal_instrument_label(surface);
    let latest = (!surface.showing_superseded_series())
        .then(|| {
            surface
                .chart
                .as_ref()
                .and_then(|chart| chart.read(cx).latest_price_summary())
        })
        .flatten();
    let price = latest.map(|summary| {
        format!(
            "{:.precision$}",
            summary.last,
            precision = usize::from(summary.precision)
        )
    });
    let change = latest.and_then(|summary| summary.change_percent);
    let change_label = change.map(|value| format!("{value:+.2}%"));
    let aria_label = match (&price, &change_label) {
        (Some(price), Some(change)) => format!("{label}, last {price}, change {change}"),
        (Some(price), None) => format!("{label}, last {price}"),
        (None, _) => label.clone(),
    };
    let change_color = change.map_or(theme.colors.text_muted, |value| {
        if value < 0.0 {
            theme.colors.danger
        } else if value > 0.0 {
            theme.colors.primary
        } else {
            theme.colors.text_secondary
        }
    });
    let exchange = match surface.provider {
        TerminalProvider::Rithmic => assets::ExchangeLogo::Rithmic,
        TerminalProvider::Hyperliquid => assets::ExchangeLogo::Hyperliquid,
    };
    let content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .child(exchange_mark(exchange, px(16.0), false, &theme.colors))
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(label.clone()),
        )
        .children(price.map(|price| {
            div()
                .flex_none()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(price)
        }))
        .children(change_label.map(|change| {
            div()
                .flex_none()
                .text_xs()
                .text_color(gpui_color(change_color))
                .child(change)
        }));
    (label, aria_label, content)
}

fn workspace_tab(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    state: &WorkspaceTabRenderState,
    cx: &App,
) -> AnyElement {
    let (label, aria_label, content) = workspace_tab_content(workspace, &state.theme, cx);
    let index = state.index;
    let drag_enabled = state.drag_enabled;
    let theme = state.theme;
    let tab_id = workspace.id;
    let selected = index == state.active;
    let select_terminal = terminal.clone();
    let key_terminal = terminal.clone();
    let middle_click_terminal = terminal.clone();
    let drag_terminal = terminal.clone();
    let drag = WorkspaceTabDrag { tab_id };
    let tab_focus = workspace.focus.clone();
    let mouse_focus = workspace.focus.clone();
    Tab::new(("workspace_tab", tab_id), &theme)
        .selected(selected)
        .w(px(WORKSPACE_TAB_WIDTH))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .pl_3()
        .pr_1()
        .text_sm()
        .track_focus(&tab_focus)
        .aria_label(aria_label)
        .aria_position_in_set(index + 1)
        .aria_size_of_set(state.workspace_count)
        .when_some(state.drag_translation, |tab, translation| {
            tab.relative().left(px(translation)).shadow_md()
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            mouse_focus.focus(window, cx);
            select_terminal.update(cx, |terminal, cx| {
                terminal.select_workspace_id(tab_id, cx);
            });
        })
        .on_aux_click(move |event, window, cx| {
            if event.is_middle_click() {
                middle_click_terminal.update(cx, |terminal, cx| {
                    terminal.close_workspace(tab_id, window, cx);
                });
                cx.stop_propagation();
            }
        })
        .on_key_down(move |event, window, cx| {
            handle_workspace_tab_key(&key_terminal, tab_id, index, event, window, cx);
        })
        .when(drag_enabled, |tab| {
            tab.on_drag(drag, move |drag, cursor_offset, _, cx| {
                drag_terminal.update(cx, |terminal, cx| {
                    terminal.begin_workspace_drag(drag.tab_id, f32::from(cursor_offset.x), cx);
                });
                cx.new(|_| drag.clone())
            })
        })
        .child(content)
        .children(
            workspace_tab_close_drag_enabled(state.workspace_count).then(|| {
                workspace_tab_close_button(terminal.clone(), tab_id, index, &label, &theme)
            }),
        )
        .into_any_element()
}

pub(super) fn workspace_tab_strip(
    terminal: &Entity<TerminalApp>,
    state: &WorkspaceTabBarState<'_>,
    cx: &App,
) -> impl IntoElement + use<> {
    let workspaces = state.workspaces;
    let enabled = state.enabled;
    let theme = state.theme;
    let colors = theme.colors;
    let close_drag_enabled = workspace_tab_close_drag_enabled(workspaces.len());
    let tabs = workspaces.iter().enumerate().map(|(index, workspace)| {
        workspace_tab(
            terminal,
            workspace,
            &WorkspaceTabRenderState {
                index,
                active: state.active,
                workspace_count: workspaces.len(),
                drag_enabled: enabled && close_drag_enabled,
                drag_translation: close_drag_enabled
                    .then(|| workspace_drag_translation(state.workspace_drag, workspace.id, index))
                    .flatten(),
                theme,
            },
            cx,
        )
    });
    let add_terminal = terminal.clone();
    let move_terminal = terminal.clone();
    let end_terminal = terminal.clone();
    let add_enabled = enabled && workspaces.len() < MAXIMUM_OPEN_WORKSPACES;
    div()
        .id("workspace_tab_list")
        .h_full()
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap_0p5()
        .pl_2()
        .overflow_x_hidden()
        .role(Role::TabList)
        .aria_label("Workspaces")
        .aria_orientation(Orientation::Horizontal)
        .tab_group()
        .on_drag_move::<WorkspaceTabDrag>(move |event, _, cx| {
            let tab_id = event.drag(cx).tab_id;
            move_terminal.update(cx, |terminal, cx| {
                terminal.move_workspace_drag(
                    tab_id,
                    f32::from(event.event.position.x),
                    f32::from(event.bounds.left()),
                    cx,
                );
            });
        })
        .on_drop(move |_: &WorkspaceTabDrag, _, cx| {
            end_terminal.update(cx, TerminalApp::end_workspace_drag);
        })
        .children(enabled.then_some(tabs).into_iter().flatten())
        .children(enabled.then(|| {
            chrome_tooltip(
                "add_workspace",
                "Create workspace",
                workspace_add_button(add_terminal, add_enabled, &theme),
                &theme,
            )
        }))
        .children(enabled.then(|| {
            div()
                .id("workspace_tab_drop_target")
                .h(px(chart_chrome::CHART_CONTROL_SIZE))
                .min_w(px(16.0))
        }))
        .children(state.error.map(|error| {
            chrome_tooltip(
                "workspace_creation_error",
                error.to_string(),
                div()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(gpui_color(colors.danger)),
                &theme,
            )
        }))
}

pub(super) fn terminal_root(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let surface = workspace_surface_entity(
        bootstrap,
        market_worker,
        lifecycle,
        chart_chrome,
        WorkspaceSurfaceRestore::default(),
        window,
        cx,
    );
    terminal_shell_root(
        TerminalShellInit {
            workspaces: vec![WorkspaceTab {
                id: 1,
                label: workspace_label(0),
                panes: vec![WorkspacePane {
                    id: 1,
                    consumer_id: 1,
                    surface,
                    focus: cx.focus_handle(),
                }],
                active_pane: 0,
                layout: NucleusWorkspace::new(1, MAXIMUM_PANES_PER_WORKSPACE),
                generation: 1,
                focus: cx.focus_handle(),
            }],
            active_workspace_id: Some(1),
            workspace_revision: 0,
            layout_generation: 1,
            workspace_factory,
            workspace_shell: WorkspaceShellKind::Window,
            chart_chrome,
            chart_settings_templates: Vec::new(),
            default_chart_settings: None,
            watchlist_entries: Vec::new(),
        },
        lifecycle,
        window,
        cx,
    )
}

pub(super) struct TerminalShellInit {
    pub(super) workspaces: Vec<WorkspaceTab>,
    pub(super) active_workspace_id: Option<u64>,
    pub(super) workspace_revision: u64,
    pub(super) layout_generation: u64,
    pub(super) workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    pub(super) workspace_shell: WorkspaceShellKind,
    pub(super) chart_chrome: chart_chrome::ChartChromePreferences,
    pub(super) chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    pub(super) default_chart_settings: Option<WorkspaceChartSettingsTemplateState>,
    pub(super) watchlist_entries: Vec<WorkspaceWatchlistEntryState>,
}

fn terminal_shell_root(
    init: TerminalShellInit,
    lifecycle: &DesktopLifecycle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let terminal_lifecycle = lifecycle.clone();
    let terminal = cx.new(move |cx| TerminalApp::new(init, terminal_lifecycle, cx));
    let closing_terminal = terminal.clone();
    window.on_window_should_close(cx, move |_, cx| {
        closing_terminal.update(cx, |terminal, cx| {
            terminal.claim_close(cx);
        });
        true
    });
    let focus_terminal = terminal.clone();
    window.on_next_frame(move |window, cx| {
        focus_terminal.update(cx, |terminal, cx| {
            terminal.chrome_focus.focus(window, cx);
        });
    });
    terminal
}

pub(super) struct RestoredWorkspaceShellPlan<'a> {
    pub(super) active_workspace_id: u64,
    pub(super) workspace_revision: u64,
    pub(super) layout_generation: u64,
    pub(super) workspace_tabs: &'a [WorkspaceTabState],
}

pub(super) fn restored_workspace_shell_plan(
    restored: &WorkspaceState,
) -> RestoredWorkspaceShellPlan<'_> {
    RestoredWorkspaceShellPlan {
        active_workspace_id: restored.active_workspace_id,
        workspace_revision: restored.workspace_revision,
        layout_generation: restored.layout_generation,
        workspace_tabs: &restored.workspace_tabs,
    }
}

pub(super) fn workspace_tabs_root(
    mut market_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored: &WorkspaceState,
    workspace_factory: engine_market_worker::WorkspaceMarketFactory,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let plan = restored_workspace_shell_plan(restored);
    let mut workspaces = Vec::with_capacity(plan.workspace_tabs.len());
    for tab in plan.workspace_tabs {
        let mut panes = Vec::with_capacity(tab.panes.len());
        for persisted in &tab.panes {
            let Some(index) = market_panes.iter().position(|pane| {
                pane.workspace_id == tab.workspace_id && pane.pane_id == persisted.pane_id
            }) else {
                continue;
            };
            let pane = market_panes.remove(index);
            panes.push(WorkspacePane {
                id: pane.pane_id,
                consumer_id: pane.consumer_id,
                surface: workspace_surface_entity(
                    pane.startup,
                    pane.worker,
                    lifecycle,
                    chart_chrome,
                    WorkspaceSurfaceRestore {
                        chart: persisted.chart.clone(),
                        side_panel: Some((
                            persisted.side_panel_visibility,
                            persisted.side_panel_width,
                            persisted.side_panel_split_basis_points,
                        )),
                    },
                    window,
                    cx,
                ),
                focus: cx.focus_handle(),
            });
        }
        if panes.is_empty() {
            continue;
        }
        let Some(layout) = tab.layout.as_ref().and_then(chart_workspace_layout) else {
            continue;
        };
        let pane_order = layout.pane_ids();
        panes.sort_by_key(|pane| {
            pane_order
                .iter()
                .position(|pane_id| *pane_id == pane.id)
                .unwrap_or(usize::MAX)
        });
        let active_pane = panes
            .iter()
            .position(|pane| pane.id == tab.active_pane_id)
            .unwrap_or(0);
        let Ok(layout) = NucleusWorkspace::restore(&layout, MAXIMUM_PANES_PER_WORKSPACE) else {
            continue;
        };
        workspaces.push(WorkspaceTab {
            id: tab.workspace_id,
            label: tab.label.clone(),
            panes,
            active_pane,
            layout,
            generation: tab.generation,
            focus: cx.focus_handle(),
        });
    }
    terminal_shell_root(
        TerminalShellInit {
            workspaces,
            active_workspace_id: Some(plan.active_workspace_id),
            workspace_revision: plan.workspace_revision,
            layout_generation: plan.layout_generation,
            workspace_factory: Some(workspace_factory),
            workspace_shell: WorkspaceShellKind::Tabs,
            chart_chrome,
            chart_settings_templates: restored.chart_settings_templates.clone(),
            default_chart_settings: restored.default_chart_settings.clone(),
            watchlist_entries: restored.watchlist_entries.clone(),
        },
        lifecycle,
        window,
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::workspace_tab_close_drag_enabled;

    #[test]
    fn workspace_tab_close_and_drag_require_multiple_tabs() {
        assert!(!workspace_tab_close_drag_enabled(0));
        assert!(!workspace_tab_close_drag_enabled(1));
        assert!(workspace_tab_close_drag_enabled(2));
        assert!(workspace_tab_close_drag_enabled(3));
    }
}
