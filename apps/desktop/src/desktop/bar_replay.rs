//! Bar replay controls for the active chart: the header's Replay button and the replay panel
//! docked below the chart layout. Aeris Charts owns the replay mode (trim cursor, start click,
//! stepping, playback cadence and the Shift+arrow keys); these controls only send its commands
//! and show its state. While a chart replays history, its own order entry is paused so a chart
//! showing the past never places an order at today's prices.

use super::*;
use crate::desktop::native_ui::tooltip::TooltipPlacement;
use aeris_chart_integration::{
    ChartReplayCommand, ChartReplayPhase, ChartReplayState, ChartReplayStep,
};

const MICROS_PER_SECOND: i64 = 1_000_000;
/// Playback steps per second the speed menu offers.
const REPLAY_SPEEDS: [f64; 6] = [0.5, 1.0, 2.0, 3.0, 5.0, 10.0];
/// Fixed-interval steps the step menu offers after "1 bar", in seconds.
const REPLAY_INTERVALS: [(i64, &str); 5] = [
    (60, "1 minute"),
    (300, "5 minutes"),
    (900, "15 minutes"),
    (3_600, "1 hour"),
    (14_400, "4 hours"),
];
const REPLAY_MENU_WIDTH: f32 = 140.0;
const REPLAY_PANEL_HEIGHT: f32 = 40.0;
const REPLAY_NOTICE_LIFETIME: Duration = Duration::from_secs(6);

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
            menu: self.bar_replay_menu,
        }
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
        self.bar_replay_menu = None;
        let result = chart.update(cx, |chart, chart_cx| {
            chart.apply_bar_replay(command, chart_cx)
        });
        if let Err(error) = result {
            diagnostic!("Bar replay command {command:?} was refused: {error}");
            if command == ChartReplayCommand::SelectStart {
                post_bar_replay_unavailable_notice(cx);
            }
        }
        cx.notify();
    }

    /// A replay change from the chart. The header and the replay panel show the replay state,
    /// and a menu left open when replay ends would reopen with the next replay.
    pub(super) fn observe_bar_replay_change(&mut self, cx: &mut Context<Self>) {
        if !self.bar_replay_on(cx) {
            self.bar_replay_menu = None;
        }
        cx.notify();
    }

    fn toggle_bar_replay_menu(&mut self, menu: BarReplayMenu, cx: &mut Context<Self>) {
        self.bar_replay_menu = (self.bar_replay_menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn close_bar_replay_menu(&mut self, cx: &mut Context<Self>) {
        if self.bar_replay_menu.take().is_some() {
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

const fn header_tooltip(phase: ChartReplayPhase) -> &'static str {
    match phase {
        ChartReplayPhase::Off => "Bar replay",
        ChartReplayPhase::Selecting => "Cancel start bar selection (Esc)",
        ChartReplayPhase::Paused | ChartReplayPhase::Playing => "Exit bar replay",
    }
}

fn post_bar_replay_unavailable_notice(cx: &mut Context<WorkspaceSurface>) {
    cx.emit(PaneNoticeEvent::Post(notification_center::UserNotice {
        key: notification_center::NoticeKey {
            scope: cx.entity_id().as_u64(),
            kind: "bar_replay_unavailable",
        },
        severity: notification_center::NoticeSeverity::Warning,
        tone: notification_center::NoticeTone::Warning,
        source: notification_center::NoticeSource::Pane(cx.entity_id().as_u64()),
        title: "Bar replay isn't available",
        detail: "Bar replay needs bars on a time-based chart. Wait for the chart to load, then try again."
            .into(),
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
    .label("Replay")
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
    let BarReplayView { state, clock, menu } = surface.read(cx).bar_replay_view(cx);
    let state = state.filter(|state| state.phase != ChartReplayPhase::Off)?;
    let controls = if state.phase == ChartReplayPhase::Selecting {
        selecting_controls(surface, theme)
    } else {
        let mut controls = vec![
            command_button(
                surface,
                "bar_replay_select",
                HugeIcon::Scissor,
                "Choose a new start bar",
                ChartReplayCommand::SelectStart,
                theme,
            )
            .label("Select bar")
            .into_any_element(),
            separator(theme).into_any_element(),
        ];
        controls.extend(transport_controls(surface, &state, theme));
        controls.push(separator(theme).into_any_element());
        controls.push(speed_menu(surface, state.speed, menu, theme).into_any_element());
        controls.push(step_menu(surface, state.step, menu, theme).into_any_element());
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
            .child("Click a bar on the chart to start replay from it.")
            .into_any_element(),
        command_button(
            surface,
            "bar_replay_cancel",
            HugeIcon::CancelCircle,
            "Cancel start bar selection (Esc)",
            ChartReplayCommand::CancelSelection,
            theme,
        )
        .label("Cancel")
        .into_any_element(),
    ]
}

fn speed_label(speed: f64) -> String {
    format!("{speed}x")
}

fn step_label(step: ChartReplayStep) -> String {
    match step {
        ChartReplayStep::Bar => "1 bar".to_string(),
        ChartReplayStep::Interval { micros } => REPLAY_INTERVALS
            .iter()
            .find(|(seconds, _)| seconds.saturating_mul(MICROS_PER_SECOND) == micros)
            .map_or_else(
                || format!("{}s", micros.div_euclid(MICROS_PER_SECOND)),
                |(_, label)| (*label).to_string(),
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
        (HugeIcon::Pause, "Pause (Shift+Down)")
    } else {
        (HugeIcon::Play, "Play (Shift+Down)")
    };
    vec![
        command_button(
            surface,
            "bar_replay_back",
            HugeIcon::Previous,
            "Step back (Shift+Left)",
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
            "Step forward (Shift+Right)",
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
        "Replay speed",
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
        "Replay step",
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
        div()
            .px_1()
            .text_sm()
            .whitespace_nowrap()
            .text_color(gpui_color(theme.colors.text_default))
            .child(clock.unwrap_or_default())
            .into_any_element(),
        separator(theme).into_any_element(),
        command_button(
            surface,
            "bar_replay_realtime",
            HugeIcon::JumpToEnd,
            "Jump to real time and leave replay",
            ChartReplayCommand::JumpToRealtime,
            theme,
        )
        .label("Jump to real time")
        .into_any_element(),
        command_button(
            surface,
            "bar_replay_exit",
            HugeIcon::CancelCircle,
            "Exit bar replay",
            ChartReplayCommand::Exit,
            theme,
        )
        .label("Exit")
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
            assert_eq!(step_label(interval_step(seconds)), label);
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
