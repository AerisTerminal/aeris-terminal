//! Bar replay commands for the host toolbar. Aeris Charts owns the replay mode: the trim cursor
//! and its start click, stepping, the playback cadence and the Shift+arrow keys. The host only
//! sends these commands and renders [`ChartReplayState`].

use super::AerisChartView;
use aeris_charts_engine::{
    ChartError, ReplayPhase, ReplayResume, ReplaySession, ReplaySnapshot, ReplayStep,
};
use gpui::Context;
use num_traits::ToPrimitive;
use std::fmt;

const MICROS_PER_SECOND: i64 = 1_000_000;

/// One bar replay toolbar command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChartReplayCommand {
    /// Arm the trim cursor; from an active replay this re-selects the start bar.
    SelectStart,
    /// Leave the trim cursor without choosing a bar.
    CancelSelection,
    TogglePlay,
    StepForward,
    StepBack,
    /// Playback steps per second, within the engine's replay speed bounds.
    SetSpeed(f64),
    SetStep(ReplayStep),
    /// Leave replay showing all data, live updates included.
    JumpToRealtime,
    /// Trim at the bar containing this moment (Unix microseconds), the first bar when it
    /// precedes the data, and pause there.
    SelectTime {
        micros: i64,
    },
    Exit,
}

/// Aeris Charts refused a bar replay command: the chart has no bars on a time axis, the command
/// does not apply to the current replay phase, or its value is out of range.
#[derive(Debug)]
pub struct ChartReplayError(ChartError);

impl fmt::Display for ChartReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bar replay refused the command: {}", self.0)
    }
}

impl std::error::Error for ChartReplayError {}

/// What the bar replay toolbar renders.
pub type ChartReplayState = ReplaySnapshot;
/// A bar replay carried from one chart to the chart that replaces it.
pub type ChartReplaySession = ReplaySession;
/// Where a carried bar replay landed on its new chart.
pub type ChartReplayResume = ReplayResume;

/// One price bar a replay has revealed, in UTC seconds and chart prices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartReplayBar {
    pub open_time: f64,
    /// The next bar's open; `None` for the newest bar the chart holds.
    pub end_time: Option<f64>,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

impl AerisChartView {
    #[must_use]
    pub fn bar_replay_state(&self) -> ChartReplayState {
        self.engine.replay_state()
    }

    /// Whether bar replay is selecting a start bar or replaying.
    #[must_use]
    pub fn bar_replay_on(&self) -> bool {
        self.engine.replay_state().phase != ReplayPhase::Off
    }

    /// The replay to carry to the chart that replaces this one; `None` while replay is off.
    #[must_use]
    pub fn bar_replay_session(&self) -> Option<ChartReplaySession> {
        self.engine.replay_session()
    }

    /// Continues a replay carried from the chart this one replaces, then emits the resulting
    /// host events like a toolbar command.
    ///
    /// # Errors
    ///
    /// Returns [`ChartReplayError`] when this chart has no bars on a time axis or the session
    /// has no start bar.
    pub fn resume_bar_replay(
        &mut self,
        session: ChartReplaySession,
        cx: &mut Context<Self>,
    ) -> Result<ChartReplayResume, ChartReplayError> {
        let result = self.engine.resume_replay(session);
        self.after_input(cx);
        result.map_err(ChartReplayError)
    }

    /// Whether replay sits at a moment before every bar this chart holds, so it shows none.
    #[must_use]
    pub fn bar_replay_waits_for_history(&self) -> bool {
        let state = self.engine.replay_state();
        matches!(state.phase, ReplayPhase::Paused | ReplayPhase::Playing)
            && state.clock_micros.is_some_and(|clock| {
                self.product_bars
                    .first_time()
                    .is_none_or(|first| first > replay_seconds(clock))
            })
    }

    /// The price bars replay has revealed that open at or after `from` (UTC seconds); with no
    /// `from`, only the newest revealed bar. Empty while replay has no clock.
    #[must_use]
    pub fn bar_replay_revealed_bars(&self, from: Option<f64>) -> Vec<ChartReplayBar> {
        let Some(clock) = self.engine.replay_state().clock_micros else {
            return Vec::new();
        };
        let through = replay_seconds(clock);
        let bar =
            |(open_time, end_time, [open, high, low, close]): (f64, Option<f64>, [f64; 4])| {
                ChartReplayBar {
                    open_time,
                    end_time,
                    open,
                    high,
                    low,
                    close,
                }
            };
        match from {
            Some(from) => self
                .product_bars
                .bars_between(from, through)
                .map(bar)
                .collect(),
            None => self
                .product_bars
                .bars_between(f64::NEG_INFINITY, through)
                .last()
                .map(bar)
                .into_iter()
                .collect(),
        }
    }

    /// The replay clock in the chart's time zone, when replay has chosen a start bar.
    #[must_use]
    pub fn bar_replay_clock_label(&self) -> Option<String> {
        let state = self.engine.replay_state();
        if !matches!(state.phase, ReplayPhase::Paused | ReplayPhase::Playing) {
            return None;
        }
        state
            .clock_micros
            .map(|clock| self.time_zone_date_time_label_at(clock.div_euclid(MICROS_PER_SECOND)))
    }

    /// Runs one toolbar command, then emits the resulting host events and arms the playback
    /// wake exactly like a pointer or key listener.
    ///
    /// # Errors
    ///
    /// Returns [`ChartReplayError`] when Aeris Charts refuses the command: the chart has no
    /// bars on a time axis, the command does not apply to the current replay phase, or its
    /// value is out of range.
    pub fn apply_bar_replay(
        &mut self,
        command: ChartReplayCommand,
        cx: &mut Context<Self>,
    ) -> Result<(), ChartReplayError> {
        let engine = &mut self.engine;
        let result = match command {
            ChartReplayCommand::SelectStart => engine.replay_select_start(),
            ChartReplayCommand::CancelSelection => {
                engine.replay_cancel_selection();
                Ok(())
            }
            ChartReplayCommand::TogglePlay => engine.replay_toggle_play(),
            ChartReplayCommand::StepForward => engine.replay_step_forward().map(|_| ()),
            ChartReplayCommand::StepBack => engine.replay_step_back().map(|_| ()),
            ChartReplayCommand::SetSpeed(speed) => engine.set_replay_speed(speed),
            ChartReplayCommand::SetStep(step) => engine.set_replay_step(step),
            ChartReplayCommand::JumpToRealtime => engine.replay_jump_to_realtime(),
            ChartReplayCommand::SelectTime { micros } => engine.replay_select_time(micros),
            ChartReplayCommand::Exit => engine.replay_exit(),
        };
        self.after_input(cx);
        result.map_err(ChartReplayError)
    }
}

/// The revealed bars open at or before the clock's whole second.
fn replay_seconds(clock_micros: i64) -> f64 {
    clock_micros
        .div_euclid(MICROS_PER_SECOND)
        .to_f64()
        .unwrap_or(f64::INFINITY)
}
