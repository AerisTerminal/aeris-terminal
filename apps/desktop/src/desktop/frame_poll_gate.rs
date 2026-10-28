/// Coalesces market-poll frame requests so at most one is in flight.
///
/// The gate exists to keep a burst of provider wakes from queueing a frame
/// callback each; it deliberately does **not** consider window focus. Polling
/// used to stop entirely while the window was unfocused, which froze the price
/// and the bar countdown for as long as the trader looked at another window,
/// then replayed the backlog in a rush when focus came back. GPUI already
/// throttles unfocused windows to roughly 30fps, so energy is bounded there,
/// where it can be measured against actual frame demand.
#[derive(Default)]
pub(crate) struct FramePollGate {
    scheduled: bool,
}

impl FramePollGate {
    pub(crate) const fn try_schedule(&mut self) -> bool {
        if self.scheduled {
            return false;
        }
        self.scheduled = true;
        true
    }

    pub(crate) const fn complete(&mut self) {
        self.scheduled = false;
    }
}
