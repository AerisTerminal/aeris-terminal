//! The chip that names the way out of fullscreen. It follows the window's fullscreen state, so
//! every path in (F11, Alt+Enter, the window controls or the operating system) shows it: the chip
//! slides down from the top centre, holds, then slides back up. Leaving fullscreen removes it.

use super::*;
use gpui::{Animation, AnimationExt, ease_in_out};

const ENTER_DURATION: Duration = Duration::from_millis(220);
const HOLD_DURATION: Duration = Duration::from_millis(3_000);
const LEAVE_DURATION: Duration = Duration::from_millis(180);
const CHIP_HEIGHT: f32 = 32.0;
/// Gap between the window's top edge and the settled chip.
const CHIP_TOP: f32 = 16.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum HintPhase {
    #[default]
    Hidden,
    Shown,
    Leaving,
}

#[derive(Debug, Default)]
pub(super) struct FullscreenHint {
    fullscreen: bool,
    phase: HintPhase,
    /// Restarts the slide for each entry and fences the timers of earlier entries.
    generation: u64,
}

impl FullscreenHint {
    /// Follows the window's fullscreen state. Returns the generation of a hint that has just
    /// started, whose hold and slide-out the caller schedules.
    pub(super) fn observe(&mut self, fullscreen: bool) -> Option<u64> {
        if fullscreen == self.fullscreen {
            return None;
        }
        self.fullscreen = fullscreen;
        self.generation = self.generation.wrapping_add(1);
        self.phase = if fullscreen {
            HintPhase::Shown
        } else {
            HintPhase::Hidden
        };
        fullscreen.then_some(self.generation)
    }

    /// Starts sliding the chip away after its hold. Returns whether anything changed.
    fn begin_leaving(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.phase != HintPhase::Shown {
            return false;
        }
        self.phase = HintPhase::Leaving;
        true
    }

    /// Removes the chip once its slide-out has run. Returns whether anything changed.
    fn finish(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.phase != HintPhase::Leaving {
            return false;
        }
        self.phase = HintPhase::Hidden;
        true
    }
}

impl TerminalApp {
    pub(super) fn track_fullscreen_hint(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(generation) = self.fullscreen_hint.observe(window.is_fullscreen()) else {
            return;
        };
        cx.spawn(async move |terminal, cx| {
            cx.background_executor()
                .timer(ENTER_DURATION + HOLD_DURATION)
                .await;
            let leaving = terminal.update(cx, |terminal, terminal_cx| {
                let leaving = terminal.fullscreen_hint.begin_leaving(generation);
                if leaving {
                    terminal_cx.notify();
                }
                leaving
            });
            if !matches!(leaving, Ok(true)) {
                return;
            }
            cx.background_executor().timer(LEAVE_DURATION).await;
            let _ = terminal.update(cx, |terminal, terminal_cx| {
                if terminal.fullscreen_hint.finish(generation) {
                    terminal_cx.notify();
                }
            });
        })
        .detach();
    }
}

fn hint_chip(theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .h(px(CHIP_HEIGHT))
        .flex()
        .items_center()
        .gap_1()
        .px_4()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface_secondary))
        .text_sm()
        .text_color(gpui_color(colors.text_secondary))
        .debug_selector(|| "fullscreen_hint_chip".into())
        .child("Press")
        .child(
            div()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_primary))
                .child("Esc"),
        )
        .child("to exit full screen")
}

/// The chip, centred at the top of the window while the hint is on screen. It registers no
/// pointer handlers, so the controls underneath keep their hover and clicks.
pub(super) fn fullscreen_hint_layer(
    hint: &FullscreenHint,
    theme: &AerisTheme,
) -> Option<AnyElement> {
    // `shown` runs from 0 (above the window, transparent) to 1 (settled below the top edge).
    let place = |layer: Div, shown: f32| {
        layer
            .top(px(-CHIP_HEIGHT + (CHIP_TOP + CHIP_HEIGHT) * shown))
            .opacity(shown)
    };
    let layer = div()
        .absolute()
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(hint_chip(theme));
    match hint.phase {
        HintPhase::Hidden => None,
        HintPhase::Shown => Some(
            layer
                .with_animation(
                    ("fullscreen_hint_enter", hint.generation),
                    Animation::new(ENTER_DURATION).with_easing(ease_out_quint()),
                    place,
                )
                .into_any_element(),
        ),
        HintPhase::Leaving => Some(
            layer
                .with_animation(
                    ("fullscreen_hint_leave", hint.generation),
                    Animation::new(LEAVE_DURATION).with_easing(ease_in_out),
                    move |layer, eased| place(layer, 1.0 - eased),
                )
                .into_any_element(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn entering_fullscreen_shows_the_hint_once_and_leaving_removes_it() {
        let mut hint = FullscreenHint::default();
        assert_eq!(hint.observe(false), None);
        let first = hint
            .observe(true)
            .expect("entering fullscreen starts a hint");
        assert_eq!(hint.phase, HintPhase::Shown);
        assert_eq!(
            hint.observe(true),
            None,
            "staying fullscreen does not restart it"
        );

        assert_eq!(hint.observe(false), None);
        assert_eq!(hint.phase, HintPhase::Hidden);
        assert!(
            !hint.begin_leaving(first),
            "the timer of an exited hint is retired"
        );

        let second = hint
            .observe(true)
            .expect("re-entering shows the hint again");
        assert_ne!(first, second);
        assert!(!hint.begin_leaving(first));
        assert!(
            !hint.finish(second),
            "the chip slides out before it is removed"
        );
        assert!(hint.begin_leaving(second));
        assert_eq!(hint.phase, HintPhase::Leaving);
        assert!(!hint.begin_leaving(second));
        assert!(hint.finish(second));
        assert_eq!(hint.phase, HintPhase::Hidden);
    }

    struct HintHarness(FullscreenHint);

    impl Render for HintHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .relative()
                .size_full()
                .children(fullscreen_hint_layer(&self.0, &AerisTheme::dark()))
        }
    }

    #[gpui::test]
    fn the_chip_is_centred_horizontally_at_its_fixed_height(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, cx| {
            gpui_base::init(cx);
            let mut hint = FullscreenHint::default();
            hint.observe(true);
            HintHarness(hint)
        });
        cx.run_until_parked();
        let viewport = cx.update(|window, _| window.viewport_size());
        let chip = cx.debug_bounds("fullscreen_hint_chip").expect("hint chip");
        assert_eq!(chip.size.height, px(CHIP_HEIGHT));
        let left = chip.origin.x;
        let right = viewport.width - (chip.origin.x + chip.size.width);
        assert!(
            (f32::from(left) - f32::from(right)).abs() <= 1.0,
            "chip is centred: {left:?} left, {right:?} right"
        );
    }
}
