//! Bar replay controls for the active chart: the header's Replay button and the replay panel
//! docked below the chart layout. Aeris Charts owns the replay mode (trim cursor, start click,
//! stepping, playback cadence and the Shift+arrow keys); these controls only send its commands
//! and show its state. While a chart replays history, its own order entry is paused so a chart
//! showing the past trades the replay's own practice session (`bar_replay_trading`) instead
//! of an account.
//!
//! A replay outlives the chart showing it: a timeframe or symbol switch carries it to the
//! replacement chart at the same moment. When that chart's history does not reach back to the
//! replay clock yet, the pane asks the market runtime for the history before the clock and, if
//! the provider has none, moves replay to the earliest bar with a notice.

use super::*;
use crate::desktop::native_ui::tooltip::TooltipPlacement;
use aeris_chart_integration::{
    ChartReplayCommand, ChartReplayPhase, ChartReplayResume, ChartReplaySession, ChartReplayState,
    ChartReplayStep,
};
use aeris_localization::{Text, common, panels, tr};
use aeris_trading_runtime::BarReplayTrading;

const MICROS_PER_SECOND: i64 = 1_000_000;
/// Playback steps per second the speed menu offers.
const REPLAY_SPEEDS: [f64; 6] = [0.5, 1.0, 2.0, 3.0, 5.0, 10.0];
/// Fixed-interval steps the step menu offers after "1 bar", in seconds.
const REPLAY_INTERVALS: [(i64, Text); 5] = [
    (60, panels::REPLAY_INTERVAL_1_MINUTE),
    (300, panels::REPLAY_INTERVAL_5_MINUTES),
    (900, panels::REPLAY_INTERVAL_15_MINUTES),
    (3_600, panels::REPLAY_INTERVAL_1_HOUR),
    (14_400, panels::REPLAY_INTERVAL_4_HOURS),
];
const REPLAY_MENU_WIDTH: f32 = 140.0;
const REPLAY_PANEL_HEIGHT: f32 = 40.0;
const REPLAY_NOTICE_LIFETIME: Duration = Duration::from_secs(6);
const NANOS_PER_MICRO: i64 = 1_000;
/// The history span requested before the replay clock when the replaced chart showed no range.
const DEFAULT_HISTORY_SPAN_NANOS: i64 = 24 * 60 * 60 * 1_000_000_000;

/// A surface's bar replay state beside the replay the chart owns.
#[derive(Default)]
pub(super) struct BarReplayHost {
    menu: Option<BarReplayMenu>,
    history_wait: Option<HistoryWait>,
    /// The practice session the replay trades; `None` until the first replay order.
    pub(super) trading: Option<BarReplayTrading>,
}

/// A carried replay whose clock precedes every bar the chart holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HistoryWait {
    clock_micros: i64,
    span_nanos: i64,
}

/// The replay of a chart being replaced, with the time span that chart showed.
pub(super) struct CarriedBarReplay {
    session: ChartReplaySession,
    span_nanos: Option<i64>,
}

/// The replay menu a surface has open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BarReplayMenu {
    Speed,
    Step,
}

/// What the replay panel shows for the active chart.
pub(super) struct BarReplayView {
    /// `None` while the pane has no chart.
    state: Option<ChartReplayState>,
    clock: Option<String>,
    /// The replay trading session's balance, once it exists.
    balance: Option<String>,
    menu: Option<BarReplayMenu>,
}

impl WorkspaceSurface {
    /// Whether this surface's chart is selecting a replay start bar or replaying.
    pub(super) fn bar_replay_on(&self, cx: &App) -> bool {
        self.chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).bar_replay_on())
    }

    pub(super) fn bar_replay_phase(&self, cx: &App) -> ChartReplayPhase {
        self.chart.as_ref().map_or(ChartReplayPhase::Off, |chart| {
            chart.read(cx).bar_replay_state().phase
        })
    }

    fn bar_replay_view(&self, cx: &App) -> BarReplayView {
        let chart = self.chart.as_ref().map(|chart| chart.read(cx));
        BarReplayView {
            state: chart.map(AerisChartView::bar_replay_state),
            clock: chart.and_then(AerisChartView::bar_replay_clock_label),
            balance: self.bar_replay_balance_label(),
            menu: self.bar_replay.menu,
        }
    }

    /// The replay to carry from the current chart to the one replacing it.
    pub(super) fn carry_bar_replay(&self, cx: &App) -> Option<CarriedBarReplay> {
        let chart = self.chart.as_ref()?.read(cx);
        Some(CarriedBarReplay {
            session: chart.bar_replay_session()?,
            span_nanos: chart
                .visible_time_range_unix_nanos()
                .map(|(start, end)| end.saturating_sub(start))
                .filter(|span| *span > 0),
        })
    }

    /// Continues a carried replay on the newly installed chart. A clock before the chart's
    /// history waits for older history; see [`Self::bar_replay_history_viewport`].
    pub(super) fn resume_carried_bar_replay(
        &mut self,
        carried: Option<CarriedBarReplay>,
        cx: &mut Context<Self>,
    ) {
        self.bar_replay.history_wait = None;
        if let (Some(carried), Some(chart)) = (carried, self.chart.clone()) {
            let clock = carried.session.clock_micros();
            let span_nanos = carried.span_nanos.unwrap_or(DEFAULT_HISTORY_SPAN_NANOS);
            match chart.update(cx, |chart, chart_cx| {
                chart.resume_bar_replay(carried.session, chart_cx)
            }) {
                Ok(ChartReplayResume::AtClock) => {}
                Ok(ChartReplayResume::BeforeHistory) => {
                    self.bar_replay.history_wait = clock.map(|clock_micros| HistoryWait {
                        clock_micros,
                        span_nanos,
                    });
                }
                Err(error) => diagnostic!("A carried bar replay could not resume: {error}"),
            }
        }
        self.sync_bar_replay_trading(cx);
    }

    /// The viewport to report while a carried replay waits for history before its clock: the
    /// span the replaced chart showed, ending at the clock. The chart itself shows no bars
    /// then, so it has no viewport of its own to report.
    pub(super) fn bar_replay_history_viewport(&mut self, cx: &App) -> Option<(i64, i64)> {
        let wait = self.bar_replay.history_wait?;
        if !self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).bar_replay_waits_for_history())
        {
            self.bar_replay.history_wait = None;
            return None;
        }
        let end = wait.clock_micros.saturating_mul(NANOS_PER_MICRO);
        Some((end.saturating_sub(wait.span_nanos), end))
    }

    /// The runtime loads no history older than the chart holds. A replay still waiting for
    /// history before its clock moves to the earliest bar instead.
    pub(super) fn bar_replay_history_exhausted(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = self.bar_replay.history_wait.take() else {
            return;
        };
        if !self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).bar_replay_waits_for_history())
        {
            return;
        }
        self.run_bar_replay(
            ChartReplayCommand::SelectTime {
                micros: wait.clock_micros,
            },
            cx,
        );
        post_bar_replay_notice(
            "bar_replay_history_start",
            tr!(panels::REPLAY_HISTORY_START_TITLE),
            tr!(panels::REPLAY_HISTORY_START_DETAIL),
            cx,
        );
    }

    /// The header's Replay button: arms the trim cursor, cancels an armed one, or leaves a
    /// running replay.
    pub(super) fn toggle_bar_replay(&mut self, cx: &mut Context<Self>) {
        let phase = self.bar_replay_phase(cx);
        self.run_bar_replay(header_command(phase), cx);
    }

    pub(super) fn run_bar_replay(&mut self, command: ChartReplayCommand, cx: &mut Context<Self>) {
        let Some(chart) = self.chart.clone() else {
            return;
        };
        self.bar_replay.menu = None;
        let result = chart.update(cx, |chart, chart_cx| {
            chart.apply_bar_replay(command, chart_cx)
        });
        if let Err(error) = result {
            diagnostic!("Bar replay command {command:?} was refused: {error}");
            if command == ChartReplayCommand::SelectStart {
                post_bar_replay_notice(
                    "bar_replay_unavailable",
                    tr!(panels::REPLAY_UNAVAILABLE_TITLE),
                    tr!(panels::REPLAY_UNAVAILABLE_DETAIL),
                    cx,
                );
            }
        }
        cx.notify();
    }

    /// A replay change from the chart. The header and the replay panel show the replay state,
    /// a menu left open when replay ends would reopen with the next replay, and the replay's
    /// trading session follows the clock.
    pub(super) fn observe_bar_replay_change(&mut self, cx: &mut Context<Self>) {
        if !self.bar_replay_on(cx) {
            self.bar_replay.menu = None;
        }
        self.sync_bar_replay_trading(cx);
        cx.notify();
    }

    fn toggle_bar_replay_menu(&mut self, menu: BarReplayMenu, cx: &mut Context<Self>) {
        self.bar_replay.menu = (self.bar_replay.menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn close_bar_replay_menu(&mut self, cx: &mut Context<Self>) {
        if self.bar_replay.menu.take().is_some() {
            cx.notify();
        }
    }
}

const fn header_command(phase: ChartReplayPhase) -> ChartReplayCommand {
    match phase {
        ChartReplayPhase::Off => ChartReplayCommand::SelectStart,
        ChartReplayPhase::Selecting => ChartReplayCommand::CancelSelection,
        ChartReplayPhase::Paused | ChartReplayPhase::Playing => ChartReplayCommand::Exit,
    }
}

fn header_tooltip(phase: ChartReplayPhase) -> &'static str {
    match phase {
        ChartReplayPhase::Off => tr!(panels::REPLAY_HEADER_TOOLTIP),
        ChartReplayPhase::Selecting => tr!(panels::REPLAY_CANCEL_SELECTION),
        ChartReplayPhase::Paused | ChartReplayPhase::Playing => tr!(panels::REPLAY_EXIT),
    }
}

pub(super) fn post_bar_replay_notice(
    kind: &'static str,
    title: &'static str,
    detail: &str,
    cx: &mut Context<WorkspaceSurface>,
) {
    cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
        key: notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind,
        },
        severity: notification_center::NoticeSeverity::Warning,
        tone: notification_center::NoticeTone::Warning,
        source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
        title,
        detail: detail.to_string(),
        action: None,
        lifetime: notification_center::NoticeLifetime::Timed(REPLAY_NOTICE_LIFETIME),
    }));
}

/// The header's Replay button for the active chart.
pub(super) fn bar_replay_header_control(
    surface: &Entity<WorkspaceSurface>,
    phase: ChartReplayPhase,
    chart_ready: bool,
    theme: &AerisTheme,
) -> Button {
    let on = phase != ChartReplayPhase::Off;
    let surface = surface.clone();
    replay_button(
        "bar_replay_toggle",
        header_tooltip(phase),
        TooltipPlacement::Bottom,
        theme,
    )
    .icon(header_icon(HugeIcon::Scissor))
    .label(tr!(panels::REPLAY_BUTTON))
    .selected(on)
    .disabled(!on && !chart_ready)
    .on_click(move |_, _, cx| {
        surface.update(cx, WorkspaceSurface::toggle_bar_replay);
    })
}

/// The replay panel docked below the chart layout while the active chart replays: the
/// selection hint, or the start bar, transport, speed, step, clock, real time and exit
/// controls.
pub(super) fn bar_replay_panel(
    surface: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
    cx: &App,
) -> Option<AnyElement> {
    let BarReplayView {
        state,
        clock,
        balance,
        menu,
    } = surface.read(cx).bar_replay_view(cx);
    let state = state.filter(|state| state.phase != ChartReplayPhase::Off)?;
    let controls = if state.phase == ChartReplayPhase::Selecting {
        selecting_controls(surface, theme)
    } else {
        let mut controls = vec![
            command_button(
                surface,
                "bar_replay_select",
                HugeIcon::Scissor,
                tr!(panels::REPLAY_CHOOSE_START),
                ChartReplayCommand::SelectStart,
                theme,
            )
            .label(tr!(panels::REPLAY_SELECT_BAR))
            .into_any_element(),
            separator(theme).into_any_element(),
        ];
        controls.extend(transport_controls(surface, &state, theme));
        controls.push(separator(theme).into_any_element());
        controls.push(speed_menu(surface, state.speed, menu, theme).into_any_element());
        controls.push(step_menu(surface, state.step, menu, theme).into_any_element());
        if let Some(balance) = balance {
            controls.push(separator(theme).into_any_element());
            controls.push(panel_text(balance, theme).into_any_element());
        }
        controls.extend(realtime_controls(surface, clock, theme));
        controls
    };
    Some(
        div()
            .id("bar_replay_panel")
            .debug_selector(|| "bar_replay_panel".into())
            .h(px(REPLAY_PANEL_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .px_2()
            .overflow_hidden()
            .border_t_1()
            .border_color(gpui_color(theme.colors.border))
            .bg(gpui_color(theme.colors.surface))
            .children(controls)
            .into_any_element(),
    )
}

fn selecting_controls(surface: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> Vec<AnyElement> {
    vec![
        div()
            .px_1()
            .text_sm()
            .whitespace_nowrap()
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(tr!(panels::REPLAY_CLICK_BAR))
            .into_any_element(),
        command_button(
            surface,
            "bar_replay_cancel",
            HugeIcon::CancelCircle,
            tr!(panels::REPLAY_CANCEL_SELECTION),
            ChartReplayCommand::CancelSelection,
            theme,
        )
        .label(tr!(common::CANCEL))
        .into_any_element(),
    ]
}

fn speed_label(speed: f64) -> String {
    tr!(panels::REPLAY_SPEED_FORMAT, speed = speed)
}

fn step_label(step: ChartReplayStep) -> String {
    match step {
        ChartReplayStep::Bar => tr!(panels::REPLAY_STEP_ONE_BAR).to_string(),
        ChartReplayStep::Interval { micros } => REPLAY_INTERVALS
            .iter()
            .find(|(seconds, _)| seconds.saturating_mul(MICROS_PER_SECOND) == micros)
            .map_or_else(
                || {
                    tr!(
                        panels::REPLAY_INTERVAL_SECONDS,
                        seconds = micros.div_euclid(MICROS_PER_SECOND)
                    )
                },
                |(_, label)| label.get().to_string(),
            ),
    }
}

fn interval_step(seconds: i64) -> ChartReplayStep {
    ChartReplayStep::Interval {
        micros: seconds.saturating_mul(MICROS_PER_SECOND),
    }
}

fn replay_button(
    id: &'static str,
    label: &'static str,
    tooltip_placement: TooltipPlacement,
    theme: &AerisTheme,
) -> Button {
    Button::new(id, theme)
        .debug_selector(|| id.into())
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .aria_label(label)
        .tooltip(
            TooltipSpec::new(label, theme)
                .show_delay(TOOLTIP_OPEN_DELAY)
                .placement(tooltip_placement),
        )
}

fn command_button(
    surface: &Entity<WorkspaceSurface>,
    id: &'static str,
    icon: HugeIcon,
    label: &'static str,
    command: ChartReplayCommand,
    theme: &AerisTheme,
) -> Button {
    let surface = surface.clone();
    replay_button(id, label, TooltipPlacement::Top, theme)
        .icon(header_icon(icon))
        .on_click(move |_, _, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface.run_bar_replay(command, surface_cx);
            });
        })
}

fn replay_menu_panel(id: &'static str, theme: &AerisTheme) -> MenuPanel {
    MenuPanel::new(
        id,
        MenuPlacement::Anchored {
            side: MenuSide::Above,
            align: MenuAlign::Start,
        },
        theme,
    )
    .width(px(REPLAY_MENU_WIDTH))
    .animate_from(PopupAnimationOrigin::BOTTOM_LEFT)
}

fn replay_menu_row(
    surface: &Entity<WorkspaceSurface>,
    id: (&'static str, usize),
    label: String,
    checked: bool,
    command: ChartReplayCommand,
    theme: &AerisTheme,
) -> MenuRow {
    let surface = surface.clone();
    MenuRow::compact(id, label, theme)
        .checked(checked)
        .on_click(move |_, _, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface.run_bar_replay(command, surface_cx);
            });
        })
}

/// A panel trigger and, while open, its menu above it.
fn replay_menu(
    surface: &Entity<WorkspaceSurface>,
    menu: BarReplayMenu,
    open: Option<BarReplayMenu>,
    label: String,
    tooltip: &'static str,
    panel: impl FnOnce() -> MenuPanel,
    theme: &AerisTheme,
) -> MenuAnchor {
    let (anchor_id, trigger_id) = match menu {
        BarReplayMenu::Speed => ("bar_replay_speed_anchor", "bar_replay_speed"),
        BarReplayMenu::Step => ("bar_replay_step_anchor", "bar_replay_step"),
    };
    let is_open = open == Some(menu);
    let toggle = surface.clone();
    let dismiss = surface.clone();
    let trigger = replay_button(trigger_id, tooltip, TooltipPlacement::Top, theme)
        .label(label)
        .caret(header_icon(HugeIcon::ChevronDown))
        .open(is_open)
        .on_click(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_bar_replay_menu(menu, surface_cx);
            });
        });
    MenuAnchor::new(anchor_id, trigger)
        .menu(is_open.then(panel))
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_bar_replay_menu);
        })
}

fn panel_text(text: String, theme: &AerisTheme) -> Div {
    div()
        .px_1()
        .text_sm()
        .whitespace_nowrap()
        .text_color(gpui_color(theme.colors.text_default))
        .child(text)
}

fn separator(theme: &AerisTheme) -> Div {
    div()
        .flex_none()
        .w(px(theme.dimensions.border_width))
        .h(px(20.0))
        .mx_1()
        .bg(gpui_color(theme.colors.border_secondary))
}

fn transport_controls(
    surface: &Entity<WorkspaceSurface>,
    state: &ChartReplayState,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let playing = state.phase == ChartReplayPhase::Playing;
    let (play_icon, play_label) = if playing {
        (HugeIcon::Pause, tr!(panels::REPLAY_PAUSE))
    } else {
        (HugeIcon::Play, tr!(panels::REPLAY_PLAY))
    };
    vec![
        command_button(
            surface,
            "bar_replay_back",
            HugeIcon::Previous,
            tr!(panels::REPLAY_STEP_BACK),
            ChartReplayCommand::StepBack,
            theme,
        )
        .disabled(!state.can_step_back)
        .into_any_element(),
        command_button(
            surface,
            "bar_replay_play",
            play_icon,
            play_label,
            ChartReplayCommand::TogglePlay,
            theme,
        )
        .disabled(!playing && !state.can_step_forward)
        .into_any_element(),
        command_button(
            surface,
            "bar_replay_forward",
            HugeIcon::Next,
            tr!(panels::REPLAY_STEP_FORWARD),
            ChartReplayCommand::StepForward,
            theme,
        )
        .disabled(!state.can_step_forward)
        .into_any_element(),
    ]
}

fn speed_menu(
    surface: &Entity<WorkspaceSurface>,
    speed: f64,
    open: Option<BarReplayMenu>,
    theme: &AerisTheme,
) -> MenuAnchor {
    let rows_surface = surface.clone();
    replay_menu(
        surface,
        BarReplayMenu::Speed,
        open,
        speed_label(speed),
        tr!(panels::REPLAY_SPEED),
        move || {
            replay_menu_panel("bar_replay_speed_menu", theme).children(
                REPLAY_SPEEDS
                    .into_iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        replay_menu_row(
                            &rows_surface,
                            ("bar_replay_speed_choice", index),
                            speed_label(choice),
                            (speed - choice).abs() < f64::EPSILON,
                            ChartReplayCommand::SetSpeed(choice),
                            theme,
                        )
                    }),
            )
        },
        theme,
    )
}

fn step_menu(
    surface: &Entity<WorkspaceSurface>,
    step: ChartReplayStep,
    open: Option<BarReplayMenu>,
    theme: &AerisTheme,
) -> MenuAnchor {
    let rows_surface = surface.clone();
    replay_menu(
        surface,
        BarReplayMenu::Step,
        open,
        step_label(step),
        tr!(panels::REPLAY_STEP),
        move || {
            let choices = std::iter::once(ChartReplayStep::Bar).chain(
                REPLAY_INTERVALS
                    .into_iter()
                    .map(|(seconds, _)| interval_step(seconds)),
            );
            replay_menu_panel("bar_replay_step_menu", theme).children(choices.enumerate().map(
                |(index, choice)| {
                    replay_menu_row(
                        &rows_surface,
                        ("bar_replay_step_choice", index),
                        step_label(choice),
                        step == choice,
                        ChartReplayCommand::SetStep(choice),
                        theme,
                    )
                },
            ))
        },
        theme,
    )
}

fn realtime_controls(
    surface: &Entity<WorkspaceSurface>,
    clock: Option<String>,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    vec![
        separator(theme).into_any_element(),
        panel_text(clock.unwrap_or_default(), theme).into_any_element(),
        separator(theme).into_any_element(),
        command_button(
            surface,
            "bar_replay_realtime",
            HugeIcon::JumpToEnd,
            tr!(panels::REPLAY_JUMP_LEAVE),
            ChartReplayCommand::JumpToRealtime,
            theme,
        )
        .label(tr!(panels::REPLAY_JUMP))
        .into_any_element(),
        command_button(
            surface,
            "bar_replay_exit",
            HugeIcon::CancelCircle,
            tr!(panels::REPLAY_EXIT),
            ChartReplayCommand::Exit,
            theme,
        )
        .label(tr!(panels::REPLAY_EXIT_BUTTON))
        .into_any_element(),
    ]
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_replay_choice_has_its_own_label() {
        assert_eq!(speed_label(0.5), "0.5x");
        assert_eq!(speed_label(1.0), "1x");
        assert_eq!(speed_label(10.0), "10x");
        assert_eq!(step_label(ChartReplayStep::Bar), "1 bar");
        for (seconds, label) in REPLAY_INTERVALS {
            assert_eq!(step_label(interval_step(seconds)), label.get());
        }
        assert_eq!(step_label(interval_step(90)), "90s");
    }

    #[test]
    fn offered_replay_speeds_are_within_the_engine_bounds() {
        for speed in REPLAY_SPEEDS {
            assert!((0.1..=10.0).contains(&speed), "{speed}");
        }
    }

    #[test]
    fn the_header_button_starts_cancels_or_exits_by_phase() {
        assert_eq!(
            header_command(ChartReplayPhase::Off),
            ChartReplayCommand::SelectStart
        );
        assert_eq!(
            header_command(ChartReplayPhase::Selecting),
            ChartReplayCommand::CancelSelection
        );
        assert_eq!(
            header_command(ChartReplayPhase::Paused),
            ChartReplayCommand::Exit
        );
        assert_eq!(
            header_command(ChartReplayPhase::Playing),
            ChartReplayCommand::Exit
        );
    }
}
