//! Axiusflow's native GPUI terminal entry point.

mod live_market_worker;
mod market_worker;
mod windowed_benchmark;

use axiusflow_application::ReplayStreamUpdate;
use axiusflow_chart_integration::{ChartBridgeMetrics, OriginChartView};
use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor};
use axiusflow_observability::{DiagnosticsQueue, FeedDiagnosticsSnapshot, LocalLatencyMetric};
use gpui::{
    App, Bounds, Context, Entity, Hsla, Render, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, StyledExt,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    theme::{Theme as ComponentTheme, ThemeMode as ComponentThemeMode, ThemeTokens},
};
use gpui_platform::application;
use market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup, PendingUiDiagnostics,
    UiDiagnosticsFeedback,
};
use std::{sync::mpsc::TrySendError, time::Duration};

const MARKET_POLL_INTERVAL: Duration = Duration::from_millis(50);

fn generation_status(
    worker_label: &str,
    subscription_id: &str,
    generation: &DesktopMarketGeneration,
) -> String {
    let (first_sequence, last_sequence) = generation.sequence_range();
    format!(
        "{worker_label} · {subscription_id} · model g{} · {} retained · seq {first_sequence}–{last_sequence}",
        generation.generation(),
        generation.items().len(),
    )
}

fn bridge_status(metrics: ChartBridgeMetrics) -> String {
    format!(
        "bridge q{} · overflows {} · recoveries {}/{}{}",
        metrics.queued_updates,
        metrics.queue_overflows,
        metrics.completed_recoveries,
        metrics.failed_recoveries,
        if metrics.recovery_pending {
            " · snapshot pending"
        } else {
            ""
        }
    )
}

fn publication_chart_state(accepted: bool, recovery_pending: bool) -> ChartState {
    if accepted && !recovery_pending {
        ChartState::Ready
    } else {
        ChartState::Recovering
    }
}

fn reconciled_bridge_state(current: ChartState, recovery_pending: bool) -> ChartState {
    if current == ChartState::Ready && recovery_pending {
        ChartState::Recovering
    } else {
        current
    }
}

struct TerminalApp {
    chart: Option<Entity<OriginChartView>>,
    chart_state: ChartState,
    chart_state_message: String,
    theme: AxiusflowTheme,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    bridge_label: String,
    market_worker: MarketDataWorker,
    diagnostics: Option<FeedDiagnosticsSnapshot>,
    diagnostics_expanded: bool,
    pending_ui_diagnostics: Option<PendingUiDiagnostics>,
}

struct HeaderState {
    theme: AxiusflowTheme,
    replay_label: String,
    bridge_label: String,
    chart_state: ChartState,
    diagnostics_expanded: bool,
}

impl TerminalApp {
    fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
    ) -> Self {
        let theme = AxiusflowTheme::dark();
        let (chart, chart_state, chart_state_message, replay_label, worker_label, subscription_id) =
            match startup {
                MarketWorkerStartup::Loading {
                    instrument,
                    subscription_id,
                    worker_label,
                } => {
                    let _ = instrument;
                    (
                        None,
                        ChartState::Loading,
                        "waiting for a covering market snapshot".to_string(),
                        "waiting for a covering market snapshot".to_string(),
                        worker_label,
                        subscription_id,
                    )
                }
                MarketWorkerStartup::Ready(bootstrap) => {
                    let replay_label = generation_status(
                        &bootstrap.worker_label,
                        &bootstrap.subscription_id,
                        &bootstrap.generation,
                    );
                    let snapshot = bootstrap.snapshot;
                    let chart =
                        cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot));
                    (
                        Some(chart),
                        ChartState::Ready,
                        "market snapshot is current".to_string(),
                        replay_label,
                        bootstrap.worker_label,
                        bootstrap.subscription_id,
                    )
                }
            };
        let bridge_label = chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        let poll_executor = cx.background_executor().clone();
        cx.spawn(async move |app, cx| {
            loop {
                poll_executor.timer(MARKET_POLL_INTERVAL).await;
                if app.update(cx, TerminalApp::poll_market_worker).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            chart,
            chart_state,
            chart_state_message,
            theme,
            replay_label,
            worker_label,
            subscription_id,
            bridge_label,
            market_worker,
            diagnostics: None,
            diagnostics_expanded: false,
            pending_ui_diagnostics: None,
        }
    }

    fn apply_publication(
        &mut self,
        mut publication: MarketWorkerPublication,
        cx: &mut Context<Self>,
    ) {
        let ui_diagnostics = publication.ui_diagnostics.take();
        self.worker_label = publication.worker_label;
        self.subscription_id = publication.subscription_id;
        self.replay_label = generation_status(
            &self.worker_label,
            &self.subscription_id,
            &publication.generation,
        );
        let (next_state, accepted) = match (&self.chart, publication.update) {
            (None, axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot)) => {
                let theme = self.theme;
                self.chart =
                    Some(cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot)));
                (ChartState::Ready, true)
            }
            (Some(chart), update) => {
                let (accepted, recovery_pending) = chart.update(cx, |chart, chart_cx| {
                    let accepted = chart.try_queue_replay_update(update).is_ok();
                    if !accepted {
                        eprintln!("bounded chart queue overflowed; fixture resnapshot required");
                    }
                    chart_cx.notify();
                    (accepted, chart.replay_bridge_metrics().recovery_pending)
                });
                (
                    publication_chart_state(accepted, recovery_pending),
                    accepted,
                )
            }
            (None, axiusflow_application::ReplayStreamUpdate::Delta(_)) => {
                self.finish_ui_diagnostics(ui_diagnostics, false);
                self.set_chart_state(
                    ChartState::Error,
                    "market delta arrived before the initial covering snapshot".to_string(),
                    cx,
                );
                return;
            }
        };
        self.finish_ui_diagnostics(ui_diagnostics, accepted);
        if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
        cx.notify();
    }

    fn finish_ui_diagnostics(&mut self, incoming: Option<PendingUiDiagnostics>, accepted: bool) {
        let Some(incoming) = incoming else {
            return;
        };
        if !accepted {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: incoming.generation(),
                });
            return;
        }
        if let Some(replaced) = self.pending_ui_diagnostics.replace(incoming) {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: replaced.generation(),
                });
        }
    }

    fn apply_recovery(
        &mut self,
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        chart.mark_replay_recovery_failed(request_id);
                        chart_cx.notify();
                    });
                }
                self.set_chart_state(ChartState::Error, error.clone(), cx);
                eprintln!("fixture recovery {request_id} failed: {error}");
                return;
            }
        };
        let Some(chart) = &self.chart else {
            self.set_chart_state(
                ChartState::Error,
                "recovery response arrived before the initial snapshot".to_string(),
                cx,
            );
            return;
        };
        let install = chart.update(cx, |chart, chart_cx| {
            let installed = chart.install_replay_recovery(request_id, &bootstrap.snapshot);
            chart_cx.notify();
            installed
        });
        match install {
            Ok(true) => {
                self.replay_label = generation_status(
                    &self.worker_label,
                    &bootstrap.subscription_id,
                    &bootstrap.generation,
                );
                self.chart_state = ChartState::Ready;
                self.chart_state_message = "market snapshot is current".to_string();
                cx.notify();
            }
            Ok(false) => eprintln!("ignored stale fixture recovery response {request_id}"),
            Err(error) => {
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                eprintln!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.mark_replay_stream_invalid();
                chart_cx.notify();
            });
        }
        eprintln!("fixture market worker invalidated the stream: {message}");
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
    }

    fn dispatch_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.market_worker.is_connected() {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let worker = &self.market_worker;
        let dispatch = chart.update(cx, |chart, chart_cx| {
            let result =
                chart.try_dispatch_replay_recovery(|command| worker.try_send_recovery(command));
            if result.as_ref().is_ok_and(|dispatched| *dispatched) {
                chart_cx.notify();
            }
            result
        });
        match dispatch {
            Ok(_) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(command)) => {
                self.market_worker.mark_disconnected();
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(command.request_id);
                    chart_cx.notify();
                });
            }
        }
    }

    fn poll_market_worker(&mut self, cx: &mut Context<Self>) {
        let (messages, disconnected) = self.market_worker.drain_messages();
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication) => {
                    self.apply_publication(publication, cx);
                }
                MarketWorkerMessage::Diagnostics(snapshot) => {
                    self.diagnostics = Some(*snapshot);
                    cx.notify();
                }
                MarketWorkerMessage::Recovery { request_id, result } => {
                    self.apply_recovery(request_id, result, cx);
                }
                MarketWorkerMessage::State { state, message } => {
                    self.set_chart_state(state, message, cx);
                }
            }
        }
        if disconnected && self.chart_state != ChartState::Error {
            self.set_chart_state(
                ChartState::Error,
                "worker channel disconnected".to_string(),
                cx,
            );
        }
        self.dispatch_recovery(cx);

        let status = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| {
                let metrics = chart.read(cx).replay_bridge_metrics();
                let reconciled =
                    reconciled_bridge_state(self.chart_state, metrics.recovery_pending);
                if reconciled != self.chart_state {
                    self.chart_state = reconciled;
                    self.chart_state_message =
                        "chart validation requires a correlated covering snapshot".to_string();
                    cx.notify();
                }
                bridge_status(metrics)
            },
        );
        if self.bridge_label != status {
            self.bridge_label = status;
            cx.notify();
        }
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme.toggled();
        sync_component_theme(&theme, Some(window), cx);
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(theme);
                chart_cx.notify();
            });
        }
        self.theme = theme;
        cx.notify();
    }

    fn toggle_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.diagnostics_expanded = !self.diagnostics_expanded;
        cx.notify();
    }

    fn schedule_diagnostics_frame(&mut self, window: &mut Window) {
        if let Some(mut diagnostics) = self.pending_ui_diagnostics.take()
            && let Some(sender) = self.market_worker.ui_diagnostics_sender()
        {
            diagnostics.mark_frame_submit();
            window.on_next_frame(move |_window, _cx| {
                let _ = sender.send(diagnostics.into_presented());
            });
        }
    }

    fn chart_state_label(&self) -> String {
        format!(
            "{} · {}",
            self.chart_state.label(),
            self.chart_state_message
        )
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_diagnostics_frame(window);
        let theme = self.theme;
        let colors = theme.colors;
        let app = cx.entity();
        let button_radius = px(f32::from(RadiusToken::Default.logical_pixels()));
        let chart_state_label = self.chart_state_label();
        let diagnostics_panel = self
            .diagnostics_expanded
            .then(|| diagnostics_panel(self.diagnostics.as_ref(), &colors, button_radius));
        let header = terminal_header(
            cx,
            app,
            HeaderState {
                theme,
                replay_label: self.replay_label.clone(),
                bridge_label: self.bridge_label.clone(),
                chart_state: self.chart_state,
                diagnostics_expanded: self.diagnostics_expanded,
            },
        );

        div()
            .v_flex()
            .size_full()
            .bg(gpui_color(colors.background))
            .text_color(gpui_color(colors.foreground))
            .child(header)
            .child(
                div()
                    .id("primary_chart")
                    .flex_1()
                    .m(px(12.0))
                    .rounded(button_radius)
                    .overflow_hidden()
                    .border_1()
                    .border_color(gpui_color(colors.border))
                    .bg(gpui_color(colors.card))
                    .children(self.chart.clone())
                    .children(self.chart.is_none().then(|| {
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(gpui_color(colors.muted_foreground))
                            .child(chart_state_label)
                    })),
            )
            .children(diagnostics_panel)
    }
}

fn terminal_header(
    cx: &mut Context<TerminalApp>,
    app: Entity<TerminalApp>,
    state: HeaderState,
) -> impl IntoElement {
    let colors = state.theme.colors;
    let radius = px(f32::from(RadiusToken::Default.logical_pixels()));
    let passive_button = ButtonCustomVariant::new(cx)
        .color(gpui_color(colors.card))
        .foreground(gpui_color(colors.muted_foreground))
        .hover(gpui_color(colors.card))
        .active(gpui_color(colors.card));
    let active_button = ButtonCustomVariant::new(cx)
        .color(gpui_color(colors.secondary))
        .foreground(gpui_color(colors.secondary_foreground))
        .hover(gpui_color(colors.accent))
        .active(gpui_color(colors.muted));
    let diagnostics_app = app.clone();
    div()
        .h(px(state.theme.dimensions.app_header_height.logical_pixels))
        .flex_none()
        .items_center()
        .justify_between()
        .px_4()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .child("Axiusflow")
        .child(
            div()
                .items_center()
                .gap_2()
                .child(
                    Button::new("market_data_source")
                        .label(format!(
                            "{} · {}",
                            state.chart_state.label(),
                            state.replay_label
                        ))
                        .rounded(radius)
                        .custom(passive_button),
                )
                .child(
                    Button::new("market_data_bridge")
                        .label(state.bridge_label)
                        .rounded(radius)
                        .custom(passive_button),
                )
                .child(
                    Button::new("feed_health_toggle")
                        .label(if state.diagnostics_expanded {
                            "Hide feed health"
                        } else {
                            "Feed health"
                        })
                        .rounded(radius)
                        .custom(active_button)
                        .on_click(move |_, _, cx| {
                            diagnostics_app.update(cx, TerminalApp::toggle_diagnostics);
                        }),
                )
                .child(
                    Button::new("theme_toggle")
                        .label(format!("Switch to {}", state.theme.mode.toggled().label()))
                        .rounded(radius)
                        .custom(active_button)
                        .on_click(move |_, window, cx| {
                            app.update(cx, |app, cx| app.toggle_theme(window, cx));
                        }),
                ),
        )
}

fn diagnostics_panel(
    snapshot: Option<&FeedDiagnosticsSnapshot>,
    colors: &axiusflow_design_system::ThemeColors,
    radius: gpui::Pixels,
) -> impl IntoElement {
    let lines = snapshot.map_or_else(
        || vec!["Waiting for the first bounded feed-health snapshot".to_string()],
        |snapshot| {
            let ui_queue = snapshot.queues[DiagnosticsQueue::UiUpdate as usize];
            let model_to_ui = latency_p99(snapshot, LocalLatencyMetric::ModelPublishToUiEnqueue);
            let ui_to_frame = latency_p99(snapshot, LocalLatencyMetric::UiEnqueueToFrameSubmit);
            let frame_to_present = latency_p99(snapshot, LocalLatencyMetric::FrameSubmitToPresent);
            vec![
                format!(
                    "{} / {} / {} · {:?} · generation {}",
                    snapshot.identity.provider(),
                    snapshot.identity.system(),
                    snapshot.identity.environment(),
                    snapshot.connection_state,
                    snapshot.session_generation.map_or(0, std::num::NonZeroU64::get),
                ),
                format!(
                    "heartbeat {} · message {} · recovery {:?} · reconnects {}",
                    format_age(snapshot.heartbeat_age_nanos),
                    format_age(snapshot.last_message_age_nanos),
                    snapshot.recovery_reason,
                    snapshot.reconnect_count,
                ),
                format!(
                    "trades {}/s · publications {}/s · gaps {} · malformed {} · stale {}",
                    format_rate_milli(snapshot.rates.trades_per_second_milli),
                    format_rate_milli(snapshot.rates.publications_per_second_milli),
                    snapshot.counters.gaps,
                    snapshot.counters.malformed_messages,
                    snapshot.counters.stale_callbacks,
                ),
                format!(
                    "UI queue {}/{} · high-water {} · conflated {} · memory {}/{} bytes",
                    ui_queue.current_items,
                    ui_queue.item_capacity,
                    ui_queue.high_water_items,
                    snapshot.counters.coalesced_ui_updates,
                    snapshot.memory.current_bytes,
                    snapshot.memory.configured_bound_bytes,
                ),
                format!(
                    "p99 model→UI {model_to_ui} · UI→frame {ui_to_frame} · frame→present {frame_to_present}",
                ),
            ]
        },
    );
    div()
        .flex_none()
        .mx(px(12.0))
        .mb(px(12.0))
        .p_3()
        .rounded(radius)
        .border_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.card))
        .text_xs()
        .text_color(gpui_color(colors.muted_foreground))
        .children(lines.into_iter().map(|line| div().child(line)))
}

fn latency_p99(snapshot: &FeedDiagnosticsSnapshot, metric: LocalLatencyMetric) -> String {
    snapshot.detailed_latency[metric as usize].map_or_else(
        || "disabled".to_string(),
        |latency| format!("{} µs", latency.p99_upper_bound_nanos / 1_000),
    )
}

fn format_age(age_nanos: Option<u64>) -> String {
    age_nanos.map_or_else(
        || "unknown".to_string(),
        |age| format!("{} ms", age / 1_000_000),
    )
}

fn format_rate_milli(rate: u64) -> String {
    format!("{}.{:03}", rate / 1_000, rate % 1_000)
}

fn sync_component_theme(theme: &AxiusflowTheme, window: Option<&mut Window>, cx: &mut App) {
    let mode = match theme.mode {
        axiusflow_design_system::ThemeMode::Light => ComponentThemeMode::Light,
        axiusflow_design_system::ThemeMode::Dark => ComponentThemeMode::Dark,
    };
    ComponentTheme::change(mode, None, cx);

    let colors = theme.colors;
    let component = ComponentTheme::global_mut(cx);
    component.radius = px(f32::from(RadiusToken::Default.logical_pixels()));
    component.radius_lg = component.radius;
    component.tile_radius = component.radius;

    component.background = gpui_color(colors.background);
    component.foreground = gpui_color(colors.foreground);
    component.border = gpui_color(colors.border);
    component.input = gpui_color(colors.input);
    component.ring = gpui_color(colors.ring);
    component.muted = gpui_color(colors.muted);
    component.muted_foreground = gpui_color(colors.muted_foreground);
    component.accent = gpui_color(colors.accent);
    component.accent_foreground = gpui_color(colors.accent_foreground);
    component.popover = gpui_color(colors.popover);
    component.popover_foreground = gpui_color(colors.popover_foreground);

    component.button = gpui_color(colors.secondary);
    component.button_foreground = gpui_color(colors.secondary_foreground);
    component.button_hover = gpui_color(colors.accent);
    component.button_active = gpui_color(colors.muted);
    component.primary = gpui_color(colors.primary);
    component.primary_foreground = gpui_color(colors.primary_foreground);
    component.primary_hover = gpui_color(colors.primary);
    component.primary_active = gpui_color(colors.primary);
    component.secondary = gpui_color(colors.secondary);
    component.secondary_foreground = gpui_color(colors.secondary_foreground);
    component.secondary_hover = gpui_color(colors.accent);
    component.secondary_active = gpui_color(colors.muted);

    component.chart_1 = gpui_color(colors.chart_palette[0]);
    component.chart_2 = gpui_color(colors.chart_palette[1]);
    component.chart_3 = gpui_color(colors.chart_palette[2]);
    component.chart_4 = gpui_color(colors.chart_palette[3]);
    component.chart_5 = gpui_color(colors.chart_palette[4]);
    component.chart_bullish = gpui_color(colors.chart_candle_up);
    component.chart_bearish = gpui_color(colors.chart_candle_down);
    component.danger = gpui_color(colors.destructive);
    component.danger_foreground = gpui_color(colors.destructive_foreground);
    component.info = gpui_color(colors.info);
    component.success = gpui_color(colors.profit);
    component.warning = gpui_color(colors.warning);

    component.sidebar = gpui_color(colors.card);
    component.sidebar_foreground = gpui_color(colors.card_foreground);
    component.sidebar_border = gpui_color(colors.border);
    component.table = gpui_color(colors.card);
    component.table_head = gpui_color(colors.muted);
    component.table_row_border = gpui_color(colors.border);
    component.title_bar = gpui_color(colors.card);
    component.title_bar_border = gpui_color(colors.border);
    component.status_bar = gpui_color(colors.card);
    component.status_bar_border = gpui_color(colors.border);
    component.tokens = ThemeTokens::from(&component.colors);

    if let Some(window) = window {
        window.refresh();
    }
}

fn gpui_color(color: ThemeColor) -> Hsla {
    rgb(color.rgb_u32()).into()
}

fn run_coinbase_live_smoke(
    product_id: &str,
    history_root: std::path::PathBuf,
) -> Result<(), String> {
    let (startup, mut worker) = MarketDataWorker::start_coinbase(
        product_id.to_string(),
        history_root,
        std::thread::current().id(),
        false,
    )?;
    if !matches!(startup, MarketWorkerStartup::Loading { .. }) {
        return Err("Coinbase shipping worker bypassed the loading state".to_string());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        let (messages, disconnected) = worker.drain_messages();
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication)
                    if matches!(publication.update, ReplayStreamUpdate::Snapshot(_)) =>
                {
                    drop(worker);
                    println!(
                        "coinbase_shipping_live_smoke=passed product={product_id} loading=true covering_snapshot=true clean_shutdown=true"
                    );
                    return Ok(());
                }
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message,
                } => return Err(message),
                _ => {}
            }
        }
        if disconnected {
            return Err("Coinbase shipping worker disconnected before its snapshot".to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err("Coinbase shipping worker timed out before its snapshot".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn run_coinbase_live_smoke_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --coinbase-live-smoke <BTC-USD|ETH-USD> <history-root>";
    let product = arguments.next().ok_or_else(|| usage.to_string())?;
    let history_root = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    run_coinbase_live_smoke(
        &product.to_string_lossy(),
        std::path::PathBuf::from(history_root),
    )
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let worker = if let Some(argument) = arguments.next() {
        if argument == "--windowed-benchmark" {
            let report_path = arguments
                .next()
                .expect("usage: axiusflow_desktop --windowed-benchmark <report-path>");
            windowed_benchmark::run(std::path::Path::new(&report_path))
                .expect("the windowed benchmark completes");
            return;
        }
        if argument == "--coinbase-live-smoke" {
            run_coinbase_live_smoke_command(arguments)
                .expect("the Coinbase shipping live smoke passes");
            return;
        }
        if argument == "--coinbase-live" {
            let product = arguments.next().unwrap_or_else(|| {
                eprintln!(
                    "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                );
                std::process::exit(2);
            });
            let history_root = arguments.next().unwrap_or_else(|| {
                eprintln!(
                    "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                );
                std::process::exit(2);
            });
            let detailed_diagnostics = match arguments.next() {
                Some(flag) if flag == "--detailed-diagnostics" => true,
                Some(_) => {
                    eprintln!(
                        "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                    );
                    std::process::exit(2);
                }
                None => false,
            };
            if arguments.next().is_some() {
                eprintln!(
                    "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                );
                std::process::exit(2);
            }
            MarketDataWorker::start_coinbase(
                product.to_string_lossy().into_owned(),
                std::path::PathBuf::from(history_root),
                std::thread::current().id(),
                detailed_diagnostics,
            )
            .expect("the bounded direct Coinbase worker starts")
        } else {
            eprintln!("unsupported argument: {}", argument.to_string_lossy());
            std::process::exit(2);
        }
    } else {
        MarketDataWorker::start().expect("the bounded binary fixture worker bootstraps")
    };
    let (bootstrap, market_worker) = worker;
    application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        sync_component_theme(&AxiusflowTheme::dark(), None, cx);
        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Axiusflow".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let terminal = cx.new(move |cx| TerminalApp::new(cx, bootstrap, market_worker));
                cx.new(|cx| Root::new(terminal, window, cx))
            },
        )
        .expect("the Axiusflow terminal window opens");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::{ChartState, publication_chart_state, reconciled_bridge_state};

    #[test]
    fn publication_is_ready_only_after_bridge_acceptance_without_recovery() {
        assert_eq!(publication_chart_state(true, false), ChartState::Ready);
        assert_eq!(publication_chart_state(false, true), ChartState::Recovering);
        assert_eq!(publication_chart_state(true, true), ChartState::Recovering);
    }

    #[test]
    fn bridge_recovery_replaces_ready_after_deferred_gap_validation() {
        assert_eq!(
            reconciled_bridge_state(ChartState::Ready, true),
            ChartState::Recovering
        );
        assert_eq!(
            reconciled_bridge_state(ChartState::Stale, true),
            ChartState::Stale
        );
    }
}
