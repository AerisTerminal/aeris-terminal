//! Replays of local market recordings on a chart. The market runtime owns each replay: its
//! clock, the recorded events it delivers and the instrument a chart selects to show it.
//! A pane only opens one from the Market recording dialog, sends its transport controls and
//! restores the instrument it showed before when the replay closes. A replay chart shows
//! the past, so no order entry on it reaches an account, and the replay is never saved
//! with the workspace.

use super::*;
use aeris_chart_integration::ChartReplayCommand;
use aeris_localization::{panels, tr};
use aeris_market_runtime::{
    MAXIMUM_RECORDING_REPLAYS, MarketService, RecordedInstrument, RecordingReplayControl,
    RecordingReplayError, RecordingReplayPlayback, RecordingReplaySpeed, RecordingReplayStatus,
    is_recording_replay_provider,
};
use chrono::{Datelike, Timelike};
use gpui::{Bounds, MouseButton, canvas};
use std::{cell::Cell, rc::Rc};

/// How often the panel reads the replay clock again while a replay is open.
const STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const REPLAY_PANEL_HEIGHT: f32 = 40.0;
const SPEED_MENU_WIDTH: f32 = 120.0;
const SEEK_BAR_WIDTH: f32 = 240.0;
const SEEK_BAR_HEIGHT: f32 = 6.0;
const PERMILLE: u16 = 1_000;

/// One replay the runtime opened for this pane. Dropping it closes the runtime's replay, so
/// a replay never outlives the pane or the choice that opened it.
pub(super) struct ReplayLease {
    status: RecordingReplayStatus,
    market: MarketService,
}

impl Drop for ReplayLease {
    fn drop(&mut self) {
        match self.market.close_recording_replay(self.status.id) {
            Ok(()) | Err(RecordingReplayError::UnknownReplay) => {}
            Err(error) => diagnostic!("A recording replay could not close: {error}"),
        }
    }
}

struct ActiveReplay {
    lease: ReplayLease,
    /// The instrument the pane showed before the replay; it is selected again when the
    /// replay closes and is what the workspace saves while the replay runs.
    restore: Option<InstallProviderInstrument>,
    /// The chart showed the replay instrument at least once.
    shown: bool,
}

/// A surface's recording replay: at most one per pane.
#[derive(Default)]
pub(super) struct RecordingReplayHost {
    replay: Option<ActiveReplay>,
    /// Reads the recording's index off the UI thread; dropping it abandons the open.
    opening: Option<gpui::Task<()>>,
    /// Repaints the panel with the replay clock; dropping it stops the refresh.
    refresh: Option<gpui::Task<()>>,
    speed_menu_open: bool,
}

impl WorkspaceSurface {
    /// Whether this pane shows, or is about to show, a recording replay. Order entry checks
    /// it at the click or key: a replay chart never trades.
    pub(super) fn recording_replay_on(&self) -> bool {
        self.recording_replay.replay.is_some()
            || self
                .product
                .as_ref()
                .is_some_and(|product| is_recording_replay_provider(&product.provider))
    }

    /// Refuses an order sent from a replay chart on the pane's feedback line.
    pub(super) fn refuse_recording_replay_order(&self) {
        self.lifecycle.trading_feedback().record_for_pane(
            &self.trading_pane_token,
            Err(tr!(panels::RECORDING_REPLAY_NO_TRADING).to_string()),
        );
    }

    /// The instrument the workspace saves for this pane: the one shown before a replay, since
    /// a replay instrument names a source that ends with the process.
    pub(super) fn persisted_product(&self) -> Option<InstallProviderInstrument> {
        match &self.product {
            Some(product) if is_recording_replay_provider(&product.provider) => self
                .recording_replay
                .replay
                .as_ref()
                .and_then(|replay| replay.restore.clone()),
            product => product.clone(),
        }
    }

    /// Opens one recorded session on this pane's chart, paused at its first event. Opening
    /// reads the recording's index, so it runs on a background thread.
    pub(super) fn open_recording_replay(
        &mut self,
        source: RecordedInstrument,
        session_started_unix_millis: u64,
        cx: &mut Context<Self>,
    ) {
        let market = self.lifecycle.market().clone();
        self.recording_replay.opening = Some(cx.spawn(async move |surface, cx| {
            let opened = cx
                .background_executor()
                .spawn(async move {
                    market
                        .open_recording_replay(&source, session_started_unix_millis)
                        .map(|status| ReplayLease { status, market })
                })
                .await;
            // A pane closed meanwhile drops the lease here, which closes the replay.
            if surface
                .update(cx, |surface, surface_cx| {
                    surface.adopt_recording_replay(opened, surface_cx);
                })
                .is_err()
            {
                diagnostic!("A recording replay opened after its pane closed");
            }
        }));
    }

    fn adopt_recording_replay(
        &mut self,
        opened: Result<ReplayLease, RecordingReplayError>,
        cx: &mut Context<Self>,
    ) {
        self.recording_replay.opening = None;
        let lease = match opened {
            Ok(lease) => lease,
            Err(error) => {
                super::bar_replay::post_bar_replay_notice(
                    "recording_replay_open_failed",
                    tr!(panels::RECORDING_REPLAY_OPEN_FAILED),
                    &open_failure_detail(error),
                    cx,
                );
                return;
            }
        };
        let restore = match self.recording_replay.replay.take() {
            Some(previous) => previous.restore.clone(),
            None => self
                .product
                .clone()
                .filter(|product| !is_recording_replay_provider(&product.provider)),
        };
        if self.bar_replay_on(cx) {
            self.run_bar_replay(ChartReplayCommand::Exit, cx);
        }
        let instrument = lease.status.instrument.clone();
        self.recording_replay.replay = Some(ActiveReplay {
            lease,
            restore,
            shown: false,
        });
        self.select_installed_instrument(&instrument, cx);
        // The replay belongs to this pane; the tab's symbol sync never relays it.
        self.pending_linked_instrument = None;
        self.start_recording_replay_refresh(cx);
        cx.notify();
    }

    fn start_recording_replay_refresh(&mut self, cx: &mut Context<Self>) {
        self.recording_replay.refresh = Some(cx.spawn(async move |surface, cx| {
            loop {
                cx.background_executor()
                    .timer(STATUS_REFRESH_INTERVAL)
                    .await;
                let open = surface.update(cx, Self::refresh_recording_replay);
                if !matches!(open, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Reads the replay's clock again and ends the replay once the pane no longer shows it:
    /// the user chose another instrument, or its selection failed. Returns whether it stays.
    fn refresh_recording_replay(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(replay) = self.recording_replay.replay.as_mut() else {
            return false;
        };
        let id = replay.lease.status.id;
        let Some(status) = self
            .lifecycle
            .market()
            .recording_replays()
            .into_iter()
            .find(|status| status.id == id)
        else {
            self.end_recording_replay(cx);
            return false;
        };
        let instrument = &status.instrument;
        let shows = |candidate: Option<&InstallProviderInstrument>| {
            candidate.is_some_and(|candidate| {
                candidate.provider == instrument.provider
                    && candidate.instrument_id == instrument.instrument_id
            })
        };
        let pending = self.chart_pane_controller.pending_product();
        if shows(self.product.as_ref()) && pending.is_none() {
            replay.shown = true;
        } else if !shows(pending) {
            let never_shown = !replay.shown;
            self.end_recording_replay(cx);
            if never_shown {
                super::bar_replay::post_bar_replay_notice(
                    "recording_replay_open_failed",
                    tr!(panels::RECORDING_REPLAY_OPEN_FAILED),
                    tr!(panels::RECORDING_REPLAY_NOT_SHOWN),
                    cx,
                );
            }
            return false;
        }
        if replay.lease.status != status {
            replay.lease.status = status;
            cx.notify();
        }
        true
    }

    /// Forgets the replay without changing the chart, which already shows something else.
    fn end_recording_replay(&mut self, cx: &mut Context<Self>) {
        self.recording_replay.replay = None;
        self.recording_replay.refresh = None;
        self.recording_replay.speed_menu_open = false;
        cx.notify();
    }

    /// The panel's Close: ends the replay and shows the instrument from before it again.
    pub(super) fn close_recording_replay(&mut self, cx: &mut Context<Self>) {
        let Some(replay) = self.recording_replay.replay.take() else {
            return;
        };
        let restore = replay.restore.clone();
        drop(replay);
        self.end_recording_replay(cx);
        if let Some(restore) = restore {
            self.select_installed_instrument(&restore, cx);
        }
    }

    fn control_recording_replay(
        &mut self,
        control: RecordingReplayControl,
        cx: &mut Context<Self>,
    ) {
        self.recording_replay.speed_menu_open = false;
        let Some(replay) = self.recording_replay.replay.as_mut() else {
            return;
        };
        match self
            .lifecycle
            .market()
            .control_recording_replay(replay.lease.status.id, control)
        {
            Ok(status) => replay.lease.status = status,
            // The next refresh finds the replay gone and ends it.
            Err(error) => diagnostic!("A recording replay refused {control:?}: {error}"),
        }
        cx.notify();
    }

    /// Play, pause, or play an ended replay again from its first event.
    fn toggle_recording_replay_playback(&mut self, cx: &mut Context<Self>) {
        let Some(status) = self.recording_replay_status() else {
            return;
        };
        match status.playback {
            RecordingReplayPlayback::Playing => {
                self.control_recording_replay(RecordingReplayControl::Pause, cx);
            }
            RecordingReplayPlayback::Paused => {
                self.control_recording_replay(RecordingReplayControl::Play, cx);
            }
            RecordingReplayPlayback::Ended => {
                let unix_nanos = status.start_unix_nanos;
                self.control_recording_replay(RecordingReplayControl::Seek { unix_nanos }, cx);
                self.control_recording_replay(RecordingReplayControl::Play, cx);
            }
        }
    }

    fn toggle_recording_replay_speed_menu(&mut self, cx: &mut Context<Self>) {
        self.recording_replay.speed_menu_open = !self.recording_replay.speed_menu_open;
        cx.notify();
    }

    fn close_recording_replay_speed_menu(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.recording_replay.speed_menu_open) {
            cx.notify();
        }
    }

    fn recording_replay_status(&self) -> Option<RecordingReplayStatus> {
        self.recording_replay
            .replay
            .as_ref()
            .map(|replay| replay.lease.status.clone())
    }
}

fn open_failure_detail(error: RecordingReplayError) -> String {
    match error {
        RecordingReplayError::NotConfigured => {
            tr!(panels::RECORDING_REPLAY_NOT_CONFIGURED).to_string()
        }
        RecordingReplayError::NotFound | RecordingReplayError::UnknownReplay => {
            tr!(panels::RECORDING_REPLAY_NOT_FOUND).to_string()
        }
        RecordingReplayError::Empty => tr!(panels::RECORDING_REPLAY_EMPTY).to_string(),
        RecordingReplayError::Unreadable => tr!(panels::RECORDING_REPLAY_UNREADABLE).to_string(),
        RecordingReplayError::TooManyReplays => tr!(
            panels::RECORDING_REPLAY_TOO_MANY,
            maximum = MAXIMUM_RECORDING_REPLAYS
        ),
    }
}

/// A recorded time as the platform date with `HH:MM:SS UTC`.
pub(super) fn recorded_time_label(unix_nanos: i64) -> String {
    let time = chrono::DateTime::from_timestamp_nanos(unix_nanos);
    format!(
        "{} {:02}:{:02}:{:02} UTC",
        aeris_design_system::platform_date_label(time.year(), time.month(), time.day()),
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// The recording's span in receipt time, and the replay clock within it, as Unix
/// nanoseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReplaySpan {
    start: i64,
    end: i64,
    clock: i64,
}

impl ReplaySpan {
    fn of(status: &RecordingReplayStatus) -> Self {
        Self {
            start: status.start_unix_nanos,
            end: status.end_unix_nanos,
            clock: status.clock_unix_nanos,
        }
    }

    fn length(self) -> i128 {
        i128::from(self.end.saturating_sub(self.start))
    }

    /// How far the clock is through the recording, in thousandths.
    fn progress_permille(self) -> u16 {
        let span = self.length();
        if span <= 0 {
            return PERMILLE;
        }
        let elapsed = i128::from(self.clock.saturating_sub(self.start));
        u16::try_from((elapsed * i128::from(PERMILLE) / span).clamp(0, i128::from(PERMILLE)))
            .unwrap_or(PERMILLE)
    }

    /// The recorded time a press `offset` into a seek bar of `width` asks for.
    fn seek_target(self, offset: f32, width: f32) -> i64 {
        let fraction = if width > 0.0 {
            (offset / width).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let permille = (fraction * f32::from(PERMILLE))
            .round()
            .to_i64()
            .unwrap_or(0);
        let offset_nanos = self.length() * i128::from(permille) / i128::from(PERMILLE);
        self.start
            .saturating_add(i64::try_from(offset_nanos).unwrap_or(i64::MAX))
    }
}

fn speed_label(speed: RecordingReplaySpeed) -> String {
    tr!(panels::REPLAY_SPEED_FORMAT, speed = speed.multiplier())
}

/// The replay panel docked below the chart layout while the active pane replays a recording:
/// the symbol, play and pause, speed, the seek bar, the replay clock, missing data and Close.
pub(super) fn recording_replay_panel(
    surface: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
    cx: &App,
) -> Option<AnyElement> {
    let host = &surface.read(cx).recording_replay;
    let status = host.replay.as_ref()?.lease.status.clone();
    let speed_menu_open = host.speed_menu_open;
    let mut controls = vec![
        panel_text(
            tr!(
                panels::RECORDING_REPLAY_LABEL,
                symbol = status.display_symbol
            ),
            theme.colors.text_default,
        )
        .into_any_element(),
        separator(theme).into_any_element(),
        playback_button(surface, status.playback, theme).into_any_element(),
        speed_menu(surface, status.speed, speed_menu_open, theme).into_any_element(),
        separator(theme).into_any_element(),
        seek_bar(surface, &status, theme).into_any_element(),
        panel_text(
            recorded_time_label(status.clock_unix_nanos),
            theme.colors.text_default,
        )
        .into_any_element(),
    ];
    if status.playback == RecordingReplayPlayback::Ended {
        controls.push(
            panel_text(
                tr!(panels::RECORDING_REPLAY_ENDED).to_string(),
                theme.colors.text_secondary,
            )
            .into_any_element(),
        );
    }
    if status.gaps > 0 {
        controls.push(
            panel_text(
                tr!(panels::RECORDING_REPLAY_GAPS, gaps = status.gaps),
                theme.colors.text_warning,
            )
            .into_any_element(),
        );
    }
    controls.push(separator(theme).into_any_element());
    controls.push(close_button(surface, theme).into_any_element());
    Some(
        div()
            .id("recording_replay_panel")
            .debug_selector(|| "recording_replay_panel".into())
            .h(px(REPLAY_PANEL_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .px_2()
            .overflow_hidden()
            .bg(gpui_color(theme.colors.surface))
            .children(controls)
            .into_any_element(),
    )
}

fn panel_button(id: &'static str, label: &'static str, theme: &AerisTheme) -> Button {
    Button::new(id, theme)
        .debug_selector(|| id.into())
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Lg)
        .aria_label(label)
        .tooltip(
            TooltipSpec::new(label, theme)
                .show_delay(TOOLTIP_OPEN_DELAY)
                .placement(super::native_ui::tooltip::TooltipPlacement::Top),
        )
}

fn panel_text(text: String, color: aeris_design_system::ThemeColor) -> Div {
    div()
        .px_1()
        .text_sm()
        .whitespace_nowrap()
        .text_color(gpui_color(color))
        .map(super::native_ui::numeric_text)
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

fn playback_button(
    surface: &Entity<WorkspaceSurface>,
    playback: RecordingReplayPlayback,
    theme: &AerisTheme,
) -> Button {
    let (icon, label) = match playback {
        RecordingReplayPlayback::Playing => (HugeIcon::Pause, tr!(panels::RECORDING_REPLAY_PAUSE)),
        RecordingReplayPlayback::Paused => (HugeIcon::Play, tr!(panels::RECORDING_REPLAY_PLAY)),
        RecordingReplayPlayback::Ended => (HugeIcon::Play, tr!(panels::RECORDING_REPLAY_RESTART)),
    };
    let surface = surface.clone();
    panel_button("recording_replay_play", label, theme)
        .icon(header_icon(icon))
        .on_click(move |_, _, cx| {
            surface.update(cx, WorkspaceSurface::toggle_recording_replay_playback);
        })
}

fn speed_menu(
    surface: &Entity<WorkspaceSurface>,
    speed: RecordingReplaySpeed,
    open: bool,
    theme: &AerisTheme,
) -> MenuAnchor {
    let toggle = surface.clone();
    let dismiss = surface.clone();
    let rows = surface.clone();
    let trigger = panel_button(
        "recording_replay_speed",
        tr!(panels::RECORDING_REPLAY_SPEED),
        theme,
    )
    .label(speed_label(speed))
    .caret(header_icon(HugeIcon::ChevronDown))
    .open(open)
    .on_click(move |_, _, cx| {
        toggle.update(cx, WorkspaceSurface::toggle_recording_replay_speed_menu);
    });
    MenuAnchor::new("recording_replay_speed_anchor", trigger)
        .menu(open.then(|| {
            MenuPanel::new(
                "recording_replay_speed_menu",
                MenuPlacement::Anchored {
                    side: MenuSide::Above,
                    align: MenuAlign::Start,
                },
                theme,
            )
            .width(px(SPEED_MENU_WIDTH))
            .animate_from(PopupAnimationOrigin::BOTTOM_LEFT)
            .children(
                RecordingReplaySpeed::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        let rows = rows.clone();
                        MenuRow::compact(
                            ("recording_replay_speed_choice", index),
                            speed_label(choice),
                            theme,
                        )
                        .checked(choice == speed)
                        .on_click(move |_, _, cx| {
                            rows.update(cx, |surface, surface_cx| {
                                surface.control_recording_replay(
                                    RecordingReplayControl::Speed(choice),
                                    surface_cx,
                                );
                            });
                        })
                    }),
            )
        }))
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_recording_replay_speed_menu);
        })
}

/// The recording's span with the clock's position; a press moves the clock there.
fn seek_bar(
    surface: &Entity<WorkspaceSurface>,
    status: &RecordingReplayStatus,
    theme: &AerisTheme,
) -> impl IntoElement {
    // The press handler needs the bar's window bounds, which only layout knows.
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
    let record = bounds.clone();
    let seek = surface.clone();
    let span = ReplaySpan::of(status);
    let filled = SEEK_BAR_WIDTH * f32::from(span.progress_permille()) / f32::from(PERMILLE);
    let bar = div()
        .id("recording_replay_seek")
        .debug_selector(|| "recording_replay_seek".into())
        .relative()
        .flex_none()
        .w(px(SEEK_BAR_WIDTH))
        .h(px(SEEK_BAR_HEIGHT))
        .mx_1()
        .rounded_full()
        .bg(gpui_color(theme.colors.border_secondary))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(px(filled))
                .rounded_full()
                .bg(gpui_color(theme.colors.primary)),
        )
        .child(
            canvas(
                move |layout, _, _| record.set(Some(layout)),
                |_, (), _, _| {},
            )
            .absolute()
            .inset_0(),
        )
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let Some(layout) = bounds.get() else {
                return;
            };
            let unix_nanos = span.seek_target(
                f32::from(event.position.x - layout.origin.x),
                f32::from(layout.size.width),
            );
            seek.update(cx, |surface, surface_cx| {
                surface.control_recording_replay(
                    RecordingReplayControl::Seek { unix_nanos },
                    surface_cx,
                );
            });
            cx.stop_propagation();
        });
    let spec =
        TooltipSpec::new(tr!(panels::RECORDING_REPLAY_SEEK), theme).show_delay(TOOLTIP_OPEN_DELAY);
    with_tooltip("recording_replay_seek_tooltip", bar, &spec)
}

fn close_button(surface: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> Button {
    let surface = surface.clone();
    panel_button(
        "recording_replay_close",
        tr!(panels::RECORDING_REPLAY_CLOSE),
        theme,
    )
    .icon(header_icon(HugeIcon::CancelCircle))
    .label(tr!(panels::REPLAY_EXIT_BUTTON))
    .on_click(move |_, _, cx| {
        surface.update(cx, WorkspaceSurface::close_recording_replay);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: i64, end: i64, clock: i64) -> ReplaySpan {
        ReplaySpan { start, end, clock }
    }

    #[test]
    fn progress_and_seek_map_the_recording_span_onto_the_bar() {
        let replay = span(1_000, 11_000, 6_000);
        assert_eq!(replay.progress_permille(), 500);
        assert_eq!(span(1_000, 11_000, 0).progress_permille(), 0);
        assert_eq!(span(1_000, 11_000, 20_000).progress_permille(), 1_000);
        assert_eq!(span(5, 5, 5).progress_permille(), 1_000);
        assert_eq!(replay.seek_target(0.0, 200.0), 1_000);
        assert_eq!(replay.seek_target(50.0, 200.0), 3_500);
        assert_eq!(replay.seek_target(400.0, 200.0), 11_000);
        assert_eq!(replay.seek_target(-10.0, 200.0), 1_000);
        assert_eq!(replay.seek_target(10.0, 0.0), 1_000);
    }

    #[test]
    fn every_offered_speed_has_its_own_label() {
        let labels = RecordingReplaySpeed::ALL.map(speed_label);
        assert_eq!(labels, ["1x", "2x", "5x", "10x", "50x", "100x"]);
    }

    #[test]
    fn recorded_times_show_the_date_and_utc_clock() {
        let label = recorded_time_label(1_767_225_600_000_000_000 + 3_723_000_000_000);
        assert!(label.ends_with("01:02:03 UTC"), "{label}");
    }

    #[test]
    fn every_open_failure_has_a_message() {
        for error in [
            RecordingReplayError::NotConfigured,
            RecordingReplayError::NotFound,
            RecordingReplayError::Empty,
            RecordingReplayError::Unreadable,
            RecordingReplayError::TooManyReplays,
            RecordingReplayError::UnknownReplay,
        ] {
            assert_ne!(open_failure_detail(error), "");
        }
        assert!(
            open_failure_detail(RecordingReplayError::TooManyReplays)
                .contains(&MAXIMUM_RECORDING_REPLAYS.to_string())
        );
    }
}
