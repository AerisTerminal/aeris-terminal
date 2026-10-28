//! The wakes that bring the terminal back to its frame: the shared market wake that every
//! market worker, the trading store and the context feed signal, the gate that keeps a burst of
//! wakes to one scheduled frame, and the one-time start of the listeners that turn wakes into
//! frames. The terminal schedules the frame and runs the listeners.

use super::UiWake;
use super::frame_poll_gate::FramePollGate;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Listeners {
    NotStarted,
    Started,
}

pub(super) struct FrameWakes {
    market: UiWake,
    poll_gate: FramePollGate,
    listeners: Listeners,
}

impl Default for FrameWakes {
    fn default() -> Self {
        Self {
            market: UiWake::default(),
            poll_gate: FramePollGate::default(),
            listeners: Listeners::NotStarted,
        }
    }
}

impl FrameWakes {
    /// The wake shared by every market worker, the trading store and the context feed.
    pub(super) const fn market(&self) -> &UiWake {
        &self.market
    }

    /// Claims the next market frame, reporting false while one is already scheduled.
    pub(super) const fn try_schedule_frame(&mut self) -> bool {
        self.poll_gate.try_schedule()
    }

    /// The scheduled frame ran, so the next wake may schedule another.
    pub(super) const fn frame_ran(&mut self) {
        self.poll_gate.complete();
    }

    /// Reports true exactly once, for the caller that starts the wake listeners.
    pub(super) fn claim_listener_start(&mut self) -> bool {
        if self.listeners == Listeners::Started {
            return false;
        }
        self.listeners = Listeners::Started;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_wakes_schedules_one_frame_until_it_runs() {
        let mut frames = FrameWakes::default();
        assert!(frames.try_schedule_frame());
        assert!(!frames.try_schedule_frame());
        frames.frame_ran();
        assert!(frames.try_schedule_frame());
    }

    #[test]
    fn the_wake_listeners_start_once() {
        let mut frames = FrameWakes::default();
        assert!(frames.claim_listener_start());
        assert!(!frames.claim_listener_start());
    }
}
