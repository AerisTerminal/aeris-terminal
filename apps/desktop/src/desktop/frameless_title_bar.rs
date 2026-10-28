//! Frameless window chrome. The workspace title bar collapses to nothing and the workspace
//! fills the window. While the pointer rests at the top edge the bar's row grows back, sliding
//! the bar down and pushing the workspace down with it; after the pointer leaves, the row
//! collapses again and the workspace stretches back up. Fullscreen keeps the bar collapsed.

use super::*;
use gpui::{Animation, AnimationExt, ease_in_out};

/// Strip along the top edge that reveals the title bar. On Windows it overlaps the native
/// top resize border, whose pointer moves GPUI still delivers as ordinary moves.
const REVEAL_ZONE_HEIGHT: f32 = 6.0;
const REVEAL_DURATION: Duration = Duration::from_millis(200);
const CONCEAL_DURATION: Duration = Duration::from_millis(180);
/// Grace period after the pointer leaves, so brushing past the edge does not flicker the bar.
pub(super) const CONCEAL_DELAY: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TitleBarPlacement {
    /// The title bar always takes its row in the layout.
    Docked,
    /// The title bar's row is collapsed and grows back on hover at the top edge.
    Frameless,
    /// Fullscreen: the title bar's row is collapsed and never grows back.
    Hidden,
}

pub(super) const fn title_bar_placement(
    fullscreen: bool,
    window_frame: chart_chrome::WindowFrame,
) -> TitleBarPlacement {
    if fullscreen {
        TitleBarPlacement::Hidden
    } else if window_frame.frameless() {
        TitleBarPlacement::Frameless
    } else {
        TitleBarPlacement::Docked
    }
}

impl TitleBarPlacement {
    /// Height the title bar row currently takes above the workspace, given how far a
    /// frameless bar is revealed (0 collapsed, 1 fully shown).
    pub(super) fn row_height(self, frameless_visibility: f32) -> f32 {
        match self {
            Self::Docked => WORKSPACE_TITLE_BAR_HEIGHT,
            Self::Frameless => WORKSPACE_TITLE_BAR_HEIGHT * frameless_visibility.clamp(0.0, 1.0),
            Self::Hidden => 0.0,
        }
    }
}

/// One slide between two visibility fractions (0 hidden, 1 revealed).
#[derive(Clone, Copy, Debug)]
struct Slide {
    from: f32,
    to: f32,
    started: Instant,
    duration: Duration,
}

impl Slide {
    fn visibility_at(self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let fraction = (elapsed / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        self.visibility_for(ease_in_out(fraction))
    }

    fn visibility_for(self, eased: f32) -> f32 {
        self.from + (self.to - self.from) * eased
    }
}

/// Reveal state of the frameless title bar, owned by the terminal window.
#[derive(Debug, Default)]
pub(super) struct FramelessTitleBar {
    revealed: bool,
    slide: Option<Slide>,
    /// Restarts the slide animation each time the direction changes.
    generation: u64,
    /// Fences delayed conceal requests; any newer reveal retires older ones.
    conceal_ticket: u64,
}

impl FramelessTitleBar {
    pub(super) const fn revealed(&self) -> bool {
        self.revealed
    }

    /// Whether a pointer at `pointer_y` (window coordinates, `None` when the pointer is
    /// outside the window) rests on the bar. Hover events only fire on pointer motion, and
    /// the bar grows under a pointer that is standing still, so the conceal decision reads
    /// the pointer position instead of the last hover event.
    pub(super) fn holds_pointer(&self, pointer_y: Option<f32>, now: Instant) -> bool {
        let row_height = TitleBarPlacement::Frameless.row_height(self.visibility_at(now));
        pointer_y.is_some_and(|y| (0.0..row_height).contains(&y))
    }

    /// Starts sliding in. Returns whether anything changed.
    pub(super) fn reveal(&mut self, now: Instant) -> bool {
        self.retire_conceal_requests();
        if self.revealed {
            return false;
        }
        self.slide_to(1.0, REVEAL_DURATION, now);
        true
    }

    /// Starts sliding out. Returns whether anything changed.
    pub(super) fn conceal(&mut self, now: Instant) -> bool {
        self.retire_conceal_requests();
        if !self.revealed {
            return false;
        }
        self.slide_to(0.0, CONCEAL_DURATION, now);
        true
    }

    /// Ticket for a conceal that runs after [`CONCEAL_DELAY`] unless retired first.
    pub(super) fn request_conceal(&mut self) -> u64 {
        self.retire_conceal_requests();
        self.conceal_ticket
    }

    pub(super) const fn conceal_request_is_current(&self, ticket: u64) -> bool {
        self.conceal_ticket == ticket
    }

    /// Shows the bar at rest without motion, used when frameless mode turns on while the
    /// pointer is already working in the bar's menus.
    pub(super) fn settle_revealed(&mut self) {
        self.retire_conceal_requests();
        self.revealed = true;
        self.slide = None;
    }

    /// Forgets all reveal state, used when frameless mode turns off.
    pub(super) fn reset(&mut self) {
        let conceal_ticket = self.conceal_ticket.wrapping_add(1);
        *self = Self {
            conceal_ticket,
            ..Self::default()
        };
    }

    fn retire_conceal_requests(&mut self) {
        self.conceal_ticket = self.conceal_ticket.wrapping_add(1);
    }

    fn settled_visibility(&self) -> f32 {
        if self.revealed { 1.0 } else { 0.0 }
    }

    /// How far the bar is revealed at `now`: 0 collapsed, 1 fully shown.
    pub(super) fn visibility_at(&self, now: Instant) -> f32 {
        self.slide.map_or_else(
            || self.settled_visibility(),
            |slide| slide.visibility_at(now),
        )
    }

    /// A reversal starts from wherever the bar currently is, so it never jumps, and takes
    /// only the share of the full duration that remains to travel.
    fn slide_to(&mut self, to: f32, duration: Duration, now: Instant) {
        let from = self.visibility_at(now);
        self.revealed = to > 0.5;
        self.generation = self.generation.wrapping_add(1);
        self.slide = Some(Slide {
            from,
            to,
            started: now,
            duration: duration
                .mul_f32((to - from).abs())
                .max(Duration::from_millis(1)),
        });
    }

    /// Whether the bar is on screen at all: revealed, or still sliding away.
    fn on_screen_at(&self, now: Instant) -> bool {
        self.revealed || self.visibility_at(now) > 0.0
    }
}

/// The frameless title bar row, in the layout above the workspace. Its height follows the
/// reveal, and the full-height bar inside stays pinned to the row's bottom edge, so the bar
/// slides down from above the window while the workspace below moves with the row.
pub(super) fn frameless_title_bar_row(
    terminal: &Entity<TerminalApp>,
    state: &FramelessTitleBar,
    title_bar: impl FnOnce() -> AnyElement,
    now: Instant,
) -> Option<AnyElement> {
    if !state.on_screen_at(now) {
        return None;
    }
    let hover_terminal = terminal.clone();
    let row = div()
        .id("frameless_title_bar")
        .w_full()
        .flex_none()
        .flex()
        .flex_col()
        .justify_end()
        .overflow_hidden()
        .on_hover(move |hovered, window, cx| {
            if !*hovered {
                hover_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.release_frameless_title_bar(window, terminal_cx);
                });
            }
        })
        .child(
            div()
                .w_full()
                .h(px(WORKSPACE_TITLE_BAR_HEIGHT))
                .flex_none()
                .child(title_bar()),
        );
    let row_height = |visibility: f32| px(TitleBarPlacement::Frameless.row_height(visibility));
    Some(match state.slide {
        Some(slide) => row
            .with_animation(
                ("frameless_title_bar_slide", state.generation),
                Animation::new(slide.duration).with_easing(ease_in_out),
                move |row, eased| row.h(row_height(slide.visibility_for(eased))),
            )
            .into_any_element(),
        None => row.h(row_height(1.0)).into_any_element(),
    })
}

/// Edge strip over the top of the workspace that reveals the collapsed bar. It does not
/// occlude, so the header controls under it keep their own hover and clicks.
pub(super) fn frameless_reveal_zone(
    terminal: &Entity<TerminalApp>,
    state: &FramelessTitleBar,
) -> Option<AnyElement> {
    if state.revealed {
        return None;
    }
    let reveal_terminal = terminal.clone();
    Some(
        div()
            .id("frameless_title_bar_reveal_zone")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(REVEAL_ZONE_HEIGHT))
            .on_hover(move |hovered, window, cx| {
                if *hovered {
                    reveal_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.reveal_frameless_title_bar(window, terminal_cx);
                    });
                }
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_hides_the_title_bar_even_in_frameless_mode() {
        use chart_chrome::WindowFrame::{Framed, Frameless};
        assert_eq!(
            title_bar_placement(false, Framed),
            TitleBarPlacement::Docked
        );
        assert_eq!(
            title_bar_placement(false, Frameless),
            TitleBarPlacement::Frameless
        );
        assert_eq!(title_bar_placement(true, Framed), TitleBarPlacement::Hidden);
        assert_eq!(
            title_bar_placement(true, Frameless),
            TitleBarPlacement::Hidden
        );
    }

    #[test]
    fn frameless_row_grows_with_the_reveal_and_pushes_the_workspace() {
        let full = WORKSPACE_TITLE_BAR_HEIGHT;
        let close = |actual: f32, expected: f32| (actual - expected).abs() < f32::EPSILON;
        assert!(close(TitleBarPlacement::Docked.row_height(0.0), full));
        assert!(close(TitleBarPlacement::Frameless.row_height(0.0), 0.0));
        assert!(close(
            TitleBarPlacement::Frameless.row_height(0.5),
            full / 2.0
        ));
        assert!(close(TitleBarPlacement::Frameless.row_height(1.0), full));
        assert!(close(TitleBarPlacement::Frameless.row_height(2.0), full));
        assert!(close(TitleBarPlacement::Hidden.row_height(1.0), 0.0));
    }

    #[test]
    fn slides_run_to_completion_in_each_direction() {
        let start = Instant::now();
        let mut bar = FramelessTitleBar::default();
        assert!(!bar.on_screen_at(start));
        assert!(bar.reveal(start));
        assert!(!bar.reveal(start), "revealing twice changes nothing");
        assert!(bar.visibility_at(start).abs() < f32::EPSILON);
        let shown = start + REVEAL_DURATION;
        assert!((bar.visibility_at(shown) - 1.0).abs() < f32::EPSILON);
        assert!(bar.conceal(shown));
        let hidden = shown + CONCEAL_DURATION;
        assert!(bar.visibility_at(hidden).abs() < f32::EPSILON);
        assert!(!bar.on_screen_at(hidden));
    }

    #[test]
    fn reversing_mid_slide_continues_from_the_current_position() {
        let start = Instant::now();
        let mut bar = FramelessTitleBar::default();
        bar.reveal(start);
        let midway = start + REVEAL_DURATION / 4;
        let position = bar.visibility_at(midway);
        assert!(position > 0.0 && position < 1.0);
        bar.conceal(midway);
        assert!((bar.visibility_at(midway) - position).abs() < 1e-6);
        let slide = bar.slide.expect("conceal slide");
        assert!(slide.duration < CONCEAL_DURATION);
        assert!(bar.on_screen_at(midway));
    }

    #[test]
    fn newer_requests_retire_pending_conceals() {
        let now = Instant::now();
        let mut bar = FramelessTitleBar::default();
        bar.reveal(now);
        let ticket = bar.request_conceal();
        assert!(bar.conceal_request_is_current(ticket));
        let newer = bar.request_conceal();
        assert!(!bar.conceal_request_is_current(ticket));
        assert!(bar.conceal_request_is_current(newer));
        let ticket = bar.request_conceal();
        bar.reveal(now);
        assert!(!bar.conceal_request_is_current(ticket));
        let ticket = bar.request_conceal();
        bar.reset();
        assert!(!bar.conceal_request_is_current(ticket));
        assert!(!bar.revealed());
    }

    #[test]
    fn a_still_pointer_the_bar_grew_under_holds_it_open() {
        let start = Instant::now();
        let mut bar = FramelessTitleBar::default();
        assert!(
            !bar.holds_pointer(Some(2.0), start),
            "a collapsed bar holds nothing"
        );
        bar.reveal(start);
        let shown = start + REVEAL_DURATION;
        assert!(bar.holds_pointer(Some(2.0), shown));
        assert!(bar.holds_pointer(Some(WORKSPACE_TITLE_BAR_HEIGHT - 1.0), shown));
        assert!(!bar.holds_pointer(Some(WORKSPACE_TITLE_BAR_HEIGHT), shown));
        assert!(!bar.holds_pointer(Some(-1.0), shown));
        assert!(
            !bar.holds_pointer(None, shown),
            "a pointer outside the window"
        );
    }

    #[test]
    fn settling_revealed_skips_the_slide() {
        let now = Instant::now();
        let mut bar = FramelessTitleBar::default();
        bar.settle_revealed();
        assert!(bar.revealed());
        assert!(bar.slide.is_none());
        assert!((bar.visibility_at(now) - 1.0).abs() < f32::EPSILON);
    }
}
