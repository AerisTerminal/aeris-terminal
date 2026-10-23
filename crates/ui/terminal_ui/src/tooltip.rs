//! Theme-token tooltip shared by desktop chrome and chart product glue.
//!
//! The trigger owns the tooltip: hover starts a delayed show, and leaving, pressing or
//! scrolling fades it out. The tooltip sits beside the trigger on the requested side rather
//! than at the cursor, so it never covers the pointer or the control it describes.

use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family, platform_typography,
};
use gpui::{
    Anchor, AnimationExt, AnyElement, App, Bounds, BoxShadow, Context, Div, ElementId, Hsla,
    IntoElement, Pixels, Point, SharedString, Size, SpringAnimation, SpringConfig, Task, Window,
    anchored, canvas, deferred, div, point, prelude::*, px,
};

/// Space between the trigger edge and the tooltip.
const TOOLTIP_GAP: f32 = 6.0;
/// Space kept between a tooltip and the window edge.
const TOOLTIP_WINDOW_MARGIN: f32 = 4.0;
/// A single-line tooltip plus its gap; a vertical side with less room flips to the other side.
const TOOLTIP_FLIP_EXTENT: f32 = 32.0;
/// How far the tooltip travels toward its resting place while it fades in.
const TOOLTIP_SLIDE: f32 = 4.0;
/// Critically damped (`damping = 2 * sqrt(stiffness * mass)`), so the fade never overshoots.
const TOOLTIP_SPRING: SpringConfig = SpringConfig::new(900.0, 60.0, 1.0);
/// Long enough for [`TOOLTIP_SPRING`] to fade below 3% opacity before the layer is removed.
const TOOLTIP_EXIT: Duration = Duration::from_millis(180);
/// Deferred menus and popovers draw at the default priority 0; tooltips stay above them.
const TOOLTIP_LAYER_PRIORITY: usize = 1;

fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}

/// The trigger side a tooltip appears on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TooltipPlacement {
    Top,
    #[default]
    Bottom,
    Left,
    Right,
}

impl TooltipPlacement {
    /// Flips a vertical placement that has no room at the window edge. Left and right keep
    /// their side; the window snap keeps an unusually wide tooltip on screen.
    fn resolve(self, trigger: Bounds<Pixels>, viewport: Size<Pixels>) -> Self {
        let extent = px(TOOLTIP_FLIP_EXTENT);
        let room_above = trigger.top();
        let room_below = viewport.height - trigger.bottom();
        match self {
            Self::Top if room_above < extent && room_below >= extent => Self::Bottom,
            Self::Bottom if room_below < extent && room_above >= extent => Self::Top,
            placement => placement,
        }
    }

    /// The window point the tooltip attaches to and which of its edges touches that point.
    fn attachment(self, trigger: Bounds<Pixels>) -> (Point<Pixels>, Anchor) {
        let gap = px(TOOLTIP_GAP);
        let center = trigger.center();
        match self {
            Self::Top => (point(center.x, trigger.top() - gap), Anchor::BottomCenter),
            Self::Bottom => (point(center.x, trigger.bottom() + gap), Anchor::TopCenter),
            Self::Left => (point(trigger.left() - gap, center.y), Anchor::RightCenter),
            Self::Right => (point(trigger.right() + gap, center.y), Anchor::LeftCenter),
        }
    }

    /// The unit direction from the tooltip toward its trigger, where the slide starts.
    const fn toward_trigger(self) -> (f32, f32) {
        match self {
            Self::Top => (0.0, 1.0),
            Self::Bottom => (0.0, -1.0),
            Self::Left => (1.0, 0.0),
            Self::Right => (-1.0, 0.0),
        }
    }
}

/// Data needed to build a tooltip without process-global theme state.
#[derive(Clone)]
pub struct TooltipSpec {
    content: TooltipContent,
    theme: AerisTheme,
    show_delay: Duration,
    placement: TooltipPlacement,
}

#[derive(Clone)]
enum TooltipContent {
    Static(SharedString),
    Dynamic(Arc<dyn Fn(&App) -> SharedString>),
    Rich(Arc<dyn Fn(&AerisTheme) -> AnyElement>),
}

impl TooltipSpec {
    #[must_use]
    pub fn new(label: impl Into<SharedString>, theme: &AerisTheme) -> Self {
        Self::with_content(TooltipContent::Static(label.into()), theme)
    }

    /// Recomputes text while the tooltip is visible; its one-second wake is owned by the
    /// trigger's tooltip state and cancels when the tooltip hides.
    #[must_use]
    pub fn dynamic(label: impl Fn(&App) -> SharedString + 'static, theme: &AerisTheme) -> Self {
        Self::with_content(TooltipContent::Dynamic(Arc::new(label)), theme)
    }

    /// Multi-line content, such as label/value rows, on the same tooltip surface.
    #[must_use]
    pub fn rich(content: impl Fn(&AerisTheme) -> AnyElement + 'static, theme: &AerisTheme) -> Self {
        Self::with_content(TooltipContent::Rich(Arc::new(content)), theme)
    }

    fn with_content(content: TooltipContent, theme: &AerisTheme) -> Self {
        Self {
            content,
            theme: *theme,
            show_delay: Duration::from_millis(500),
            placement: TooltipPlacement::default(),
        }
    }

    #[must_use]
    pub fn show_delay(mut self, delay: Duration) -> Self {
        self.show_delay = delay;
        self
    }

    #[must_use]
    pub fn placement(mut self, placement: TooltipPlacement) -> Self {
        self.placement = placement;
        self
    }

    #[must_use]
    pub const fn delay(&self) -> Duration {
        self.show_delay
    }

    /// The Theme System popover look: `surface-popover` with a `border-popover-highlight`
    /// inner edge and a `border-popover` outer edge.
    /// Single-line labels are pills; rich content keeps a rounded rectangle so rows stay aligned.
    fn surface(&self, cx: &App) -> Div {
        let colors = self.theme.colors;
        let edge = px(self.theme.dimensions.border_width);
        let base = div()
            .border(edge)
            .border_color(gpui_color(colors.border_popover_highlight))
            .bg(gpui_color(colors.surface_popover))
            .shadow(vec![
                BoxShadow::new(px(0.0), px(0.0), gpui_color(colors.border_popover))
                    .spread_radius(edge),
            ])
            .font_family(platform_font_family())
            .font_weight(gpui::FontWeight(f32::from(
                platform_typography().weight(TypographyRole::Normal),
            )))
            .text_xs()
            .text_color(gpui_color(colors.text_default));
        let label = match &self.content {
            TooltipContent::Static(label) => label.clone(),
            TooltipContent::Dynamic(label) => label(cx),
            TooltipContent::Rich(content) => {
                return base
                    .rounded(px(f32::from(RadiusToken::Medium.logical_pixels())))
                    .px_3()
                    .py_2()
                    .child(content(&self.theme));
            }
        };
        base.rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
            .px(px(10.0))
            .py(px(4.0))
            .whitespace_nowrap()
            .child(label)
    }
}
/// Per-trigger tooltip lifecycle; it lives exactly as long as the trigger keeps rendering.
#[derive(Default)]
struct TooltipState {
    phase: TooltipPhase,
    trigger_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    refresh: Option<Task<()>>,
}

/// A pending phase holds its timer task; replacing the phase drops and cancels it.
#[derive(Default)]
enum TooltipPhase {
    #[default]
    Hidden,
    Waiting {
        _show: Task<()>,
    },
    Shown,
    Leaving {
        _hide: Task<()>,
    },
}

impl TooltipState {
    fn hover(&mut self, hovered: bool, delay: Duration, cx: &mut Context<Self>) {
        match (hovered, &self.phase) {
            (true, TooltipPhase::Hidden) => {
                let show = cx.spawn(async move |state, cx| {
                    cx.background_executor().timer(delay).await;
                    // A dropped state means the trigger stopped rendering; nothing to show.
                    let _ = state.update(cx, |state, cx| {
                        state.phase = TooltipPhase::Shown;
                        cx.notify();
                    });
                });
                self.phase = TooltipPhase::Waiting { _show: show };
            }
            (true, TooltipPhase::Leaving { .. }) => {
                self.phase = TooltipPhase::Shown;
                cx.notify();
            }
            (false, _) => self.dismiss(cx),
            (true, TooltipPhase::Waiting { .. } | TooltipPhase::Shown) => {}
        }
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        match self.phase {
            TooltipPhase::Waiting { .. } => self.phase = TooltipPhase::Hidden,
            TooltipPhase::Shown => {
                self.refresh = None;
                let hide = cx.spawn(async move |state, cx| {
                    cx.background_executor().timer(TOOLTIP_EXIT).await;
                    let _ = state.update(cx, |state, cx| {
                        state.phase = TooltipPhase::Hidden;
                        cx.notify();
                    });
                });
                self.phase = TooltipPhase::Leaving { _hide: hide };
                cx.notify();
            }
            TooltipPhase::Hidden | TooltipPhase::Leaving { .. } => {}
        }
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() {
            return;
        }
        self.refresh = Some(cx.spawn(async move |state, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let _ = state.update(cx, |state, cx| {
                state.refresh = None;
                cx.notify();
            });
        }));
    }

    /// `Some(true)` while showing, `Some(false)` while fading out.
    const fn visibility(&self) -> Option<bool> {
        match self.phase {
            TooltipPhase::Shown => Some(true),
            TooltipPhase::Leaving { .. } => Some(false),
            TooltipPhase::Hidden | TooltipPhase::Waiting { .. } => None,
        }
    }
}

/// Gives `trigger` the shared tooltip. `id` keys the tooltip state and must be stable for the
/// trigger across frames; the trigger's own element id is the natural choice.
pub fn attach_tooltip<E>(
    trigger: E,
    id: impl Into<ElementId>,
    spec: &TooltipSpec,
    window: &mut Window,
    cx: &mut App,
) -> E
where
    E: StatefulInteractiveElement + ParentElement,
{
    let state = window.use_keyed_state(id, cx, |_, _| TooltipState::default());
    let (visibility, trigger_bounds) = {
        let state = state.read(cx);
        (state.visibility(), state.trigger_bounds.clone())
    };
    let layer = visibility
        .zip(trigger_bounds.get())
        .map(|(showing, bounds)| {
            if showing && matches!(spec.content, TooltipContent::Dynamic(_)) {
                state.update(cx, TooltipState::schedule_refresh);
            }
            tooltip_layer(spec, bounds, window.viewport_size(), showing, cx)
        });
    let delay = spec.show_delay;
    let hover = state.clone();
    let press = state.clone();
    trigger
        .on_hover(move |hovered, _, cx| {
            hover.update(cx, |state, cx| state.hover(*hovered, delay, cx));
        })
        .capture_any_mouse_down(move |_, _, cx| {
            press.update(cx, TooltipState::dismiss);
        })
        .on_scroll_wheel(move |_, _, cx| {
            state.update(cx, TooltipState::dismiss);
        })
        .child(
            canvas(
                move |bounds, _, _| trigger_bounds.set(Some(bounds)),
                |_, (), _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .children(layer)
}

fn tooltip_layer(
    spec: &TooltipSpec,
    trigger: Bounds<Pixels>,
    viewport: Size<Pixels>,
    showing: bool,
    cx: &App,
) -> AnyElement {
    let placement = spec.placement.resolve(trigger, viewport);
    let (position, anchor) = placement.attachment(trigger);
    let (toward_x, toward_y) = placement.toward_trigger();
    let motion = SpringAnimation::new(TOOLTIP_SPRING)
        .to(if showing { 1.0_f32 } else { 0.0 })
        .from(0.0);
    let surface = spec.surface(cx).relative().with_spring(
        "tooltip_motion",
        motion,
        move |surface, progress| {
            let travel = TOOLTIP_SLIDE * (1.0 - progress);
            surface
                .opacity(progress.clamp(0.0, 1.0))
                .left(px(toward_x * travel))
                .top(px(toward_y * travel))
        },
    );
    deferred(
        anchored()
            .position(position)
            .anchor(anchor)
            .snap_to_window_with_margin(px(TOOLTIP_WINDOW_MARGIN))
            .child(surface),
    )
    .with_priority(TOOLTIP_LAYER_PRIORITY)
    .into_any_element()
}

/// Wraps `trigger` in a flex box that owns the shared tooltip.
pub fn with_tooltip(
    id: impl Into<ElementId>,
    trigger: impl IntoElement + 'static,
    spec: &TooltipSpec,
) -> TooltipTrigger {
    TooltipTrigger {
        id: id.into(),
        trigger: trigger.into_any_element(),
        spec: spec.clone(),
    }
}

#[derive(IntoElement)]
pub struct TooltipTrigger {
    id: ElementId,
    trigger: AnyElement,
    spec: TooltipSpec,
}

impl RenderOnce for TooltipTrigger {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let trigger = div().id(self.id.clone()).flex().child(self.trigger);
        attach_tooltip(trigger, self.id, &self.spec, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{TOOLTIP_EXIT, TOOLTIP_GAP, TooltipPlacement, TooltipSpec, with_tooltip};
    use aeris_design_system::AerisTheme;
    use gpui::{
        Anchor, Bounds, Context, Modifiers, MouseButton, TestAppContext, Window, div, point,
        prelude::*, px, size,
    };
    use std::time::Duration;

    struct TriggerView;

    impl Render for TriggerView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let spec = TooltipSpec::rich(
                |_| {
                    div()
                        .debug_selector(|| "TOOLTIP_BODY".into())
                        .w(px(60.0))
                        .h(px(10.0))
                        .into_any_element()
                },
                &AerisTheme::dark(),
            )
            .show_delay(TEST_DELAY);
            div()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .left(px(100.0))
                        .top(px(100.0))
                        .child(with_tooltip(
                            "trigger",
                            div().w(px(40.0)).h(px(20.0)),
                            &spec,
                        )),
                )
        }
    }

    const TEST_DELAY: Duration = Duration::from_millis(100);

    #[gpui::test]
    fn tooltip_opens_beside_its_trigger_after_the_delay_and_fades_out_after_leaving(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = cx.add_window_view(|_, _| TriggerView);
        cx.run_until_parked();

        cx.simulate_mouse_move(point(px(130.0), px(118.0)), None, Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("TOOLTIP_BODY").is_none());

        cx.executor().advance_clock(TEST_DELAY);
        cx.run_until_parked();
        let body = cx
            .debug_bounds("TOOLTIP_BODY")
            .expect("the tooltip shows after its delay");
        // Bottom placement centres the tooltip under the 40x20 trigger at (100, 100), clear
        // of both the trigger and the cursor, regardless of where the cursor entered. The
        // anchored layer rounds to whole pixels.
        assert!((body.center().x - px(120.0)).abs() <= px(1.0), "{body:?}");
        assert!(body.top() > px(120.0), "{body:?}");

        cx.simulate_mouse_move(point(px(400.0), px(400.0)), None, Modifiers::none());
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("TOOLTIP_BODY").is_some(),
            "the tooltip fades out instead of vanishing"
        );

        cx.executor().advance_clock(TOOLTIP_EXIT);
        cx.run_until_parked();
        assert!(cx.debug_bounds("TOOLTIP_BODY").is_none());
    }

    #[gpui::test]
    fn pressing_the_trigger_dismisses_its_tooltip(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, _| TriggerView);
        cx.run_until_parked();
        cx.simulate_mouse_move(point(px(110.0), px(110.0)), None, Modifiers::none());
        cx.executor().advance_clock(TEST_DELAY);
        cx.run_until_parked();
        assert!(cx.debug_bounds("TOOLTIP_BODY").is_some());

        cx.simulate_mouse_down(
            point(px(110.0), px(110.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.executor().advance_clock(TOOLTIP_EXIT);
        cx.run_until_parked();
        assert!(cx.debug_bounds("TOOLTIP_BODY").is_none());
    }

    #[test]
    fn tooltip_delay_is_explicit_and_bounded_by_the_owner() {
        let tooltip =
            TooltipSpec::new("Close", &AerisTheme::dark()).show_delay(Duration::from_millis(275));
        assert_eq!(tooltip.delay(), Duration::from_millis(275));
    }

    #[test]
    fn each_placement_attaches_beside_the_trigger_edge() {
        let trigger = Bounds::new(point(px(100.0), px(200.0)), size(px(40.0), px(20.0)));
        let gap = px(TOOLTIP_GAP);
        assert_eq!(
            TooltipPlacement::Top.attachment(trigger),
            (point(px(120.0), px(200.0) - gap), Anchor::BottomCenter)
        );
        assert_eq!(
            TooltipPlacement::Bottom.attachment(trigger),
            (point(px(120.0), px(220.0) + gap), Anchor::TopCenter)
        );
        assert_eq!(
            TooltipPlacement::Left.attachment(trigger),
            (point(px(100.0) - gap, px(210.0)), Anchor::RightCenter)
        );
        assert_eq!(
            TooltipPlacement::Right.attachment(trigger),
            (point(px(140.0) + gap, px(210.0)), Anchor::LeftCenter)
        );
    }

    #[test]
    fn vertical_placements_flip_only_when_their_side_has_no_room() {
        let viewport = size(px(800.0), px(600.0));
        let at_top = Bounds::new(point(px(10.0), px(4.0)), size(px(28.0), px(28.0)));
        let at_bottom = Bounds::new(point(px(10.0), px(574.0)), size(px(28.0), px(22.0)));
        let middle = Bounds::new(point(px(10.0), px(300.0)), size(px(28.0), px(28.0)));
        assert_eq!(
            TooltipPlacement::Top.resolve(at_top, viewport),
            TooltipPlacement::Bottom
        );
        assert_eq!(
            TooltipPlacement::Bottom.resolve(at_bottom, viewport),
            TooltipPlacement::Top
        );
        for placement in [
            TooltipPlacement::Top,
            TooltipPlacement::Bottom,
            TooltipPlacement::Left,
            TooltipPlacement::Right,
        ] {
            assert_eq!(placement.resolve(middle, viewport), placement);
        }
        assert_eq!(
            TooltipPlacement::Left.resolve(at_top, viewport),
            TooltipPlacement::Left
        );
    }

    #[test]
    fn the_slide_starts_on_the_trigger_side() {
        assert_eq!(TooltipPlacement::Top.toward_trigger(), (0.0, 1.0));
        assert_eq!(TooltipPlacement::Bottom.toward_trigger(), (0.0, -1.0));
        assert_eq!(TooltipPlacement::Left.toward_trigger(), (1.0, 0.0));
        assert_eq!(TooltipPlacement::Right.toward_trigger(), (-1.0, 0.0));
    }
}
