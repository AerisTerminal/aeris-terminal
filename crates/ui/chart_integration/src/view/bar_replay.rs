//! Bar replay commands for the host toolbar. Aeris Charts owns the replay mode: the trim cursor
//! and its start click, stepping, the playback cadence and the Shift+arrow keys. The host only
//! sends these commands and renders [`ChartReplayState`].

use super::AerisChartView;
use aeris_charts_engine::{ChartError, ReplayPhase, ReplaySnapshot, ReplayStep};
use gpui::Context;
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
            ChartReplayCommand::Exit => engine.replay_exit(),
        };
        self.after_input(cx);
        result.map_err(ChartReplayError)
    }
}
