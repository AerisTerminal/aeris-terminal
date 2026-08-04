//! Axiusflow's native GPUI terminal entry point.

mod market_worker;
mod windowed_benchmark;

use axiusflow_chart_integration::{ChartBridgeMetrics, OriginChartView};
use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor};
use gpui::{
    App, Bounds, Context, Entity, Hsla, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, StyledExt,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    theme::{Theme as ComponentTheme, ThemeMode as ComponentThemeMode, ThemeTokens},
};
use gpui_platform::application;
use market_worker::{
    DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap, MarketWorkerMessage,
    MarketWorkerPublication,
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

struct TerminalApp {
    chart: Entity<OriginChartView>,
    theme: AxiusflowTheme,
    theme_revision: u64,
    instrument_label: String,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    bridge_label: String,
    market_worker: MarketDataWorker,
}

impl TerminalApp {
    fn new(
        cx: &mut Context<Self>,
        bootstrap: MarketWorkerBootstrap,
        market_worker: MarketDataWorker,
    ) -> Self {
        let theme = AxiusflowTheme::dark();
        let replay_label = generation_status(
            &bootstrap.worker_label,
            &bootstrap.subscription_id,
            &bootstrap.generation,
        );
        let instrument = bootstrap.snapshot.instrument();
        let instrument_label = format!(
            "{} · {} · instrument r{}",
            instrument.symbol, instrument.venue_id, instrument.revision
        );
        let snapshot = bootstrap.snapshot;
        let chart = cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot));
        let bridge_label = bridge_status(chart.read(cx).replay_bridge_metrics());
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
            theme,
            theme_revision: 0,
            instrument_label,
            replay_label,
            worker_label: bootstrap.worker_label,
            subscription_id: bootstrap.subscription_id,
            bridge_label,
            market_worker,
        }
    }

    fn apply_publication(&mut self, publication: MarketWorkerPublication, cx: &mut Context<Self>) {
        self.replay_label = generation_status(
            &self.worker_label,
            &self.subscription_id,
            &publication.generation,
        );
        self.chart.update(cx, |chart, chart_cx| {
            if chart.try_queue_replay_update(publication.update).is_err() {
                eprintln!("bounded chart queue overflowed; fixture resnapshot required");
            }
            chart_cx.notify();
        });
        cx.notify();
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
                self.chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                eprintln!("fixture recovery {request_id} failed: {error}");
                return;
            }
        };
        let install = self.chart.update(cx, |chart, chart_cx| {
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
                cx.notify();
            }
            Ok(false) => eprintln!("ignored stale fixture recovery response {request_id}"),
            Err(error) => {
                self.chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                eprintln!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, message: &str, cx: &mut Context<Self>) {
        self.chart.update(cx, |chart, chart_cx| {
            chart.mark_replay_stream_invalid();
            chart_cx.notify();
        });
        eprintln!("fixture market worker invalidated the stream: {message}");
    }

    fn dispatch_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.market_worker.is_connected() {
            return;
        }
        let worker = &self.market_worker;
        let dispatch = self.chart.update(cx, |chart, chart_cx| {
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
                self.chart.update(cx, |chart, chart_cx| {
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
                MarketWorkerMessage::Recovery { request_id, result } => {
                    self.apply_recovery(request_id, result, cx);
                }
                MarketWorkerMessage::Failed(error) => {
                    self.mark_market_stream_invalid(&error, cx);
                }
            }
        }
        if disconnected {
            self.mark_market_stream_invalid("worker channel disconnected", cx);
        }
        self.dispatch_recovery(cx);

        let status = bridge_status(self.chart.read(cx).replay_bridge_metrics());
        if self.bridge_label != status {
            self.bridge_label = status;
            cx.notify();
        }
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme.toggled();
        sync_component_theme(&theme, Some(window), cx);
        self.chart.update(cx, |chart, chart_cx| {
            chart.set_theme(theme);
            chart_cx.notify();
        });
        self.theme = theme;
        self.theme_revision = self.theme_revision.saturating_add(1);
        cx.notify();
    }
}

impl Render for TerminalApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let colors = theme.colors;
        let app = cx.entity();
        let button_radius = px(f32::from(RadiusToken::Default.logical_pixels()));
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
        let toggle_label = format!("Switch to {}", theme.mode.toggled().label());
        let revision_label = format!("theme r{}", self.theme_revision);
        let instrument_label = self.instrument_label.clone();
        let replay_label = self.replay_label.clone();
        let bridge_label = self.bridge_label.clone();

        div()
            .v_flex()
            .size_full()
            .bg(gpui_color(colors.background))
            .text_color(gpui_color(colors.foreground))
            .child(
                div()
                    .h(px(theme.dimensions.dashboard_header_height.logical_pixels))
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(gpui_color(colors.border))
                    .child(
                        div()
                            .items_center()
                            .gap_3()
                            .child("AXIUSFLOW")
                            .child(
                                div()
                                    .text_color(gpui_color(colors.muted_foreground))
                                    .child(instrument_label),
                            )
                            .child(
                                div()
                                    .text_color(gpui_color(colors.muted_foreground))
                                    .text_xs()
                                    .child(revision_label),
                            ),
                    )
                    .child(
                        div()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("market_data_source")
                                    .label(replay_label)
                                    .rounded(button_radius)
                                    .custom(passive_button),
                            )
                            .child(
                                Button::new("market_data_bridge")
                                    .label(bridge_label)
                                    .rounded(button_radius)
                                    .custom(passive_button),
                            )
                            .child(
                                Button::new("theme_toggle")
                                    .label(toggle_label)
                                    .rounded(button_radius)
                                    .custom(active_button)
                                    .on_click(move |_, window, cx| {
                                        app.update(cx, |app, cx| app.toggle_theme(window, cx));
                                    }),
                            ),
                    ),
            )
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
                    .child(self.chart.clone()),
            )
    }
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

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    if let Some(argument) = arguments.next() {
        if argument == "--windowed-benchmark" {
            let report_path = arguments
                .next()
                .expect("usage: axiusflow_desktop --windowed-benchmark <report-path>");
            windowed_benchmark::run(std::path::Path::new(&report_path))
                .expect("the windowed benchmark completes");
            return;
        }
        eprintln!("unsupported argument: {}", argument.to_string_lossy());
        std::process::exit(2);
    }
    let (bootstrap, market_worker) =
        MarketDataWorker::start().expect("the bounded binary fixture worker bootstraps");
    application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        sync_component_theme(&AxiusflowTheme::dark(), None, cx);
        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
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
