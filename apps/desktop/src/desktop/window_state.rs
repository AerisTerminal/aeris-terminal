//! The terminal window's own state: the shell focus, whether the window is active, open or
//! closing, the frameless title bar, the fullscreen hint and chart-only fullscreen. The terminal
//! acts on what each transition reports (starting a window move, refreshing the profile,
//! toggling window fullscreen); nothing here touches GPUI.

use gpui::FocusHandle;

use super::frameless_title_bar::FramelessTitleBar;
use super::fullscreen_hint::FullscreenHint;
use super::terminal_chrome::{WindowMoveGestureEvent, window_move_gesture_transition};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowActivity {
    Active,
    Inactive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowPhase {
    Open,
    /// The close was claimed: workspace durability is handed off and markets are retiring.
    Closing,
}

/// A profile refresh waits for the window to come back from the browser that edits it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProfileRefresh {
    Idle,
    OnNextActivation,
}

/// Shift+F chart-only fullscreen: only the workspace's charts show.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ChartFullscreen {
    /// Entering put the window into fullscreen, so leaving takes it out again.
    restores_window: bool,
}

/// What a change of the window's active state means for the terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ActivityChange {
    Unchanged,
    /// The window came back to the front; `refresh_profile` is set when a refresh was armed.
    Activated {
        refresh_profile: bool,
    },
    Deactivated,
}

pub(super) struct WindowState {
    chrome_focus: FocusHandle,
    activity: WindowActivity,
    phase: WindowPhase,
    profile_refresh: ProfileRefresh,
    /// A title-bar press that becomes a window move once the pointer drags.
    move_pending: bool,
    title_bar: FramelessTitleBar,
    fullscreen_hint: FullscreenHint,
    chart_fullscreen: Option<ChartFullscreen>,
}

impl WindowState {
    pub(super) fn new(chrome_focus: FocusHandle) -> Self {
        Self {
            chrome_focus,
            activity: WindowActivity::Active,
            phase: WindowPhase::Open,
            profile_refresh: ProfileRefresh::Idle,
            move_pending: false,
            title_bar: FramelessTitleBar::default(),
            fullscreen_hint: FullscreenHint::default(),
            chart_fullscreen: None,
        }
    }

    /// The shell's focus handle, which takes the keyboard when no text field or chart holds it.
    pub(super) const fn chrome_focus(&self) -> &FocusHandle {
        &self.chrome_focus
    }

    pub(super) fn is_active(&self) -> bool {
        self.activity == WindowActivity::Active
    }

    pub(super) fn is_closing(&self) -> bool {
        self.phase == WindowPhase::Closing
    }

    /// Marks the window closing, reporting whether this call claimed the close.
    pub(super) fn begin_closing(&mut self) -> bool {
        if self.is_closing() {
            return false;
        }
        self.phase = WindowPhase::Closing;
        true
    }

    /// Follows the window's active state; an armed profile refresh is claimed on activation.
    pub(super) fn observe_activity(&mut self, active: bool) -> ActivityChange {
        let next = if active {
            WindowActivity::Active
        } else {
            WindowActivity::Inactive
        };
        if next == self.activity {
            return ActivityChange::Unchanged;
        }
        self.activity = next;
        match next {
            WindowActivity::Inactive => ActivityChange::Deactivated,
            WindowActivity::Active => {
                let refresh_profile = self.profile_refresh == ProfileRefresh::OnNextActivation;
                self.profile_refresh = ProfileRefresh::Idle;
                ActivityChange::Activated { refresh_profile }
            }
        }
    }

    /// Refreshes the account profile the next time the window becomes active.
    pub(super) fn arm_profile_refresh(&mut self) {
        self.profile_refresh = ProfileRefresh::OnNextActivation;
    }

    /// Follows a title-bar move gesture, returning whether the window move starts now.
    pub(super) fn move_gesture(&mut self, event: WindowMoveGestureEvent) -> bool {
        let transition = window_move_gesture_transition(self.move_pending, event);
        self.move_pending = transition.pending;
        transition.start_move
    }

    pub(super) const fn move_pending(&self) -> bool {
        self.move_pending
    }

    pub(super) const fn title_bar(&self) -> &FramelessTitleBar {
        &self.title_bar
    }

    pub(super) const fn title_bar_mut(&mut self) -> &mut FramelessTitleBar {
        &mut self.title_bar
    }

    pub(super) const fn fullscreen_hint(&self) -> &FullscreenHint {
        &self.fullscreen_hint
    }

    pub(super) const fn fullscreen_hint_mut(&mut self) -> &mut FullscreenHint {
        &mut self.fullscreen_hint
    }

    pub(super) const fn chart_fullscreen(&self) -> bool {
        self.chart_fullscreen.is_some()
    }

    /// Enters or leaves chart-only fullscreen. Returns whether the window's own fullscreen
    /// must toggle: entering takes a windowed window fullscreen, and leaving restores it.
    pub(super) fn toggle_chart_fullscreen(&mut self, window_fullscreen: bool) -> bool {
        if let Some(state) = self.chart_fullscreen.take() {
            return state.restores_window && window_fullscreen;
        }
        let restores_window = !window_fullscreen;
        self.chart_fullscreen = Some(ChartFullscreen { restores_window });
        restores_window
    }

    /// Leaving window fullscreen another way also ends a chart-only fullscreen that entered it.
    pub(super) fn track_window_fullscreen(&mut self, window_fullscreen: bool) {
        if self
            .chart_fullscreen
            .is_some_and(|state| state.restores_window && !window_fullscreen)
        {
            self.chart_fullscreen = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window_state(cx: &mut gpui::TestAppContext) -> WindowState {
        WindowState::new(cx.update(|cx| cx.focus_handle()))
    }

    #[gpui::test]
    fn the_close_is_claimed_once(cx: &mut gpui::TestAppContext) {
        let mut state = window_state(cx);
        assert!(!state.is_closing());
        assert!(state.begin_closing());
        assert!(state.is_closing());
        assert!(!state.begin_closing(), "a second close finds it claimed");
    }

    #[gpui::test]
    fn an_armed_profile_refresh_runs_on_the_next_activation_only(cx: &mut gpui::TestAppContext) {
        let mut state = window_state(cx);
        assert_eq!(state.observe_activity(true), ActivityChange::Unchanged);
        state.arm_profile_refresh();
        assert_eq!(
            state.observe_activity(true),
            ActivityChange::Unchanged,
            "an active window waits for the return from the browser"
        );
        assert_eq!(state.observe_activity(false), ActivityChange::Deactivated);
        assert_eq!(state.observe_activity(false), ActivityChange::Unchanged);
        assert_eq!(
            state.observe_activity(true),
            ActivityChange::Activated {
                refresh_profile: true
            }
        );
        assert_eq!(state.observe_activity(false), ActivityChange::Deactivated);
        assert_eq!(
            state.observe_activity(true),
            ActivityChange::Activated {
                refresh_profile: false
            }
        );
    }

    #[gpui::test]
    fn a_title_bar_press_starts_a_move_only_when_dragged(cx: &mut gpui::TestAppContext) {
        let mut state = window_state(cx);
        assert!(!state.move_gesture(WindowMoveGestureEvent::Press));
        assert!(state.move_pending());
        assert!(state.move_gesture(WindowMoveGestureEvent::Move { left_pressed: true }));
        assert!(!state.move_pending());
        assert!(!state.move_gesture(WindowMoveGestureEvent::Press));
        assert!(!state.move_gesture(WindowMoveGestureEvent::Cancel));
        assert!(!state.move_pending());
    }

    #[gpui::test]
    fn chart_fullscreen_restores_the_window_it_changed(cx: &mut gpui::TestAppContext) {
        let mut state = window_state(cx);
        assert!(
            state.toggle_chart_fullscreen(false),
            "a windowed window enters fullscreen"
        );
        assert!(state.chart_fullscreen());
        assert!(
            state.toggle_chart_fullscreen(true),
            "leaving restores the window"
        );
        assert!(!state.chart_fullscreen());

        assert!(
            !state.toggle_chart_fullscreen(true),
            "an already fullscreen window stays as it is"
        );
        assert!(!state.toggle_chart_fullscreen(true));
        assert!(!state.chart_fullscreen());
    }

    #[gpui::test]
    fn leaving_window_fullscreen_ends_the_chart_fullscreen_that_entered_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut state = window_state(cx);
        state.toggle_chart_fullscreen(true);
        state.track_window_fullscreen(false);
        assert!(
            state.chart_fullscreen(),
            "the window was fullscreen before, so chart fullscreen stays"
        );
        state.toggle_chart_fullscreen(true);

        state.toggle_chart_fullscreen(false);
        state.track_window_fullscreen(true);
        assert!(state.chart_fullscreen());
        state.track_window_fullscreen(false);
        assert!(!state.chart_fullscreen());
    }
}
