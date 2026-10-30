//! Bar replay controls: the header's Replay toggle and the toolbar over a replaying chart.
//! Aeris Charts owns the replay mode (trim cursor, start click, stepping, playback cadence and
//! the Shift+arrow keys); these controls only send its commands and show its state. While a
//! chart replays history, its own order entry is paused so a chart showing the past never
//! places an order at today's prices.

use super::*;
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
/// Keeps the toolbar clear of the chart's time axis.
const REPLAY_TOOLBAR_BOTTOM_INSET: f32 = 44.0;
const REPLAY_CLOCK_MIN_WIDTH: f32 = 132.0;
const REPLAY_NOTICE_LIFETIME: Duration = Duration::from_secs(6);

/// The replay toolbar menu a surface has open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BarReplayMenu {
    Speed,
    Step,
}

impl WorkspaceSurface {
    /// Whether this surface's chart is selecting a replay start bar or replaying.
    pub(super) fn bar_replay_on(&self, cx: &App) -> bool {
        self.chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).bar_replay_on())
    }

    /// The header toggle: arms the trim cursor, or leaves replay when it is on.
    pub(super) fn toggle_bar_replay(&mut self, cx: &mut Context<Self>) {
        let command = if self.bar_replay_on(cx) {
            ChartReplayCommand::Exit
        } else {
            ChartReplayCommand::SelectStart
        };
        self.run_bar_replay(command, cx);
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

    /// A replay change from the chart; a menu left open when replay ends would reopen with the
    /// next replay.
    pub(super) fn observe_bar_replay_change(&mut self, cx: &mut Context<Self>) {
        if self.bar_replay_menu.is_some() && !self.bar_replay_on(cx) {
            self.bar_replay_menu = None;
            cx.notify();
        }
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

/// The header's Replay toggle for the active chart.
pub(super) fn bar_replay_header_control(
    app: Entity<WorkspaceSurface>,
    on: bool,
    chart_ready: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let tooltip = if on { "Exit bar replay" } else { "Bar replay" };
    Button::new("bar_replay_toggle", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .icon(header_icon(HugeIcon::Scissor))
        .aria_label(tooltip)
        .tooltip(TooltipSpec::new(tooltip, theme).show_delay(TOOLTIP_OPEN_DELAY))
        .selected(on)
        .disabled(!on && !chart_ready)
        .on_click(move |_, _, cx| {
            app.update(cx, WorkspaceSurface::toggle_bar_replay);
        })
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

fn replay_button(id: &'static str, label: &'static str, theme: &AerisTheme) -> Button {
    Button::new(id, theme)
        .debug_selector(|| id.into())
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Default)
        .aria_label(label)
        .tooltip(TooltipSpec::new(label, theme).show_delay(TOOLTIP_OPEN_DELAY))
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
    replay_button(id, label, theme)
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

/// A toolbar trigger and, while open, its menu above it.
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
    let trigger = replay_button(trigger_id, tooltip, theme)
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

/// The replay toolbar of a pane whose chart is replaying, centered above its time axis.
pub(super) fn bar_replay_toolbar(
    surface_entity: &Entity<WorkspaceSurface>,
    surface: &WorkspaceSurface,
    theme: &AerisTheme,
    cx: &App,
) -> Option<AnyElement> {
    let chart = surface.chart.as_ref()?.read(cx);
    let state = chart.bar_replay_state();
    if state.phase == ChartReplayPhase::Off {
        return None;
    }
    let controls = if state.phase == ChartReplayPhase::Selecting {
        selecting_controls(surface_entity, theme)
    } else {
        replay_controls(
            surface_entity,
            &state,
            chart.bar_replay_clock_label(),
            surface.bar_replay_menu,
            theme,
        )
    };
    let radius = px(f32::from(RadiusToken::Compact.logical_pixels()));
    let panel = popover_surface(div().id("bar_replay_toolbar"), theme, radius)
        .debug_selector(|| "bar_replay_toolbar".into())
        .flex()
        .items_center()
        .gap(px(2.0))
        .p_1()
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(controls);
    Some(
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(REPLAY_TOOLBAR_BOTTOM_INSET))
            .flex()
            .justify_center()
            .child(panel)
            .into_any_element(),
    )
}

fn selecting_controls(surface: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> Vec<AnyElement> {
    vec![
        div()
            .px_2()
            .text_sm()
            .text_color(gpui_color(theme.colors.text_secondary))
            .child("Click a bar to start replay. Esc cancels.")
            .into_any_element(),
        command_button(
            surface,
            "bar_replay_cancel",
            HugeIcon::Close,
            "Cancel bar replay",
            ChartReplayCommand::CancelSelection,
            theme,
        )
        .into_any_element(),
    ]
}

fn replay_controls(
    surface: &Entity<WorkspaceSurface>,
    state: &ChartReplayState,
    clock: Option<String>,
    open: Option<BarReplayMenu>,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    let mut controls = transport_controls(surface, state, theme);
    controls.push(speed_menu(surface, state.speed, open, theme).into_any_element());
    controls.push(step_menu(surface, state.step, open, theme).into_any_element());
    controls.extend(realtime_controls(surface, clock, theme));
    controls
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
            "bar_replay_select",
            HugeIcon::Scissor,
            "Select a new start bar",
            ChartReplayCommand::SelectStart,
            theme,
        )
        .into_any_element(),
        separator(theme).into_any_element(),
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
    let realtime = surface.clone();
    vec![
        separator(theme).into_any_element(),
        div()
            .min_w(px(REPLAY_CLOCK_MIN_WIDTH))
            .px_1()
            .text_sm()
            .text_center()
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(clock.unwrap_or_default())
            .into_any_element(),
        separator(theme).into_any_element(),
        replay_button(
            "bar_replay_realtime",
            "Leave replay and show all data",
            theme,
        )
        .label("Jump to real time")
        .on_click(move |_, _, cx| {
            realtime.update(cx, |surface, surface_cx| {
                surface.run_bar_replay(ChartReplayCommand::JumpToRealtime, surface_cx);
            });
        })
        .into_any_element(),
        command_button(
            surface,
            "bar_replay_exit",
            HugeIcon::Close,
            "Exit bar replay",
            ChartReplayCommand::Exit,
            theme,
        )
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
}
