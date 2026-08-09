#[derive(Default)]
pub(crate) struct FramePollGate {
    scheduled: bool,
}

impl FramePollGate {
    pub(crate) const fn try_schedule(&mut self, window_active: bool) -> bool {
        if !window_active || self.scheduled {
            return false;
        }
        self.scheduled = true;
        true
    }

    pub(crate) const fn complete(&mut self) {
        self.scheduled = false;
    }
}
