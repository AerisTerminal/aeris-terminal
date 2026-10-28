//! Keeps a trigger's popup inside the window.
//!
//! [`fit_to_window`] covers its positioned parent, the trigger, and holds one absolutely
//! positioned popup. After layout it moves the popup only when it would leave the window: a
//! popup that opened below (or above) its trigger without room on that side flips to the other
//! side, keeping the same gap, when that side fits or has more room; then it slides inside the
//! window edges.
//!
//! With [`FitToWindow::on_dismiss`], any press outside both the trigger and the popup closes
//! it. Presses on the trigger are left to the trigger, so pressing it again toggles the popup
//! closed instead of closing and reopening it.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Edges, Element, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Length, MouseDownEvent, Pixels, Point, Position, Size, Style, Window,
    point, px,
};

/// Space a fitted popup keeps from the window edges.
const WINDOW_MARGIN: Pixels = px(8.0);

type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// Wraps `popup`, which positions itself against the trigger with absolute insets, so it stays
/// inside the window. Place it in the trigger's positioned wrapper, usually through `deferred`.
pub(crate) fn fit_to_window(popup: impl IntoElement) -> FitToWindow {
    FitToWindow {
        popup: popup.into_any_element(),
        on_dismiss: None,
    }
}

pub(crate) struct FitToWindow {
    popup: AnyElement,
    on_dismiss: Option<Dismiss>,
}

impl FitToWindow {
    /// Runs `handler` when a press lands outside both the trigger and the popup.
    pub(crate) fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

pub(crate) struct FitToWindowLayout {
    popup: LayoutId,
}

impl Element for FitToWindow {
    type RequestLayoutState = FitToWindowLayout;
    /// Where the popup painted, after any move into the window.
    type PrepaintState = Bounds<Pixels>;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let popup = self.popup.request_layout(window, cx);
        let style = Style {
            position: Position::Absolute,
            inset: Edges::all(Length::from(px(0.0))),
            ..Style::default()
        };
        (
            window.request_layout(style, [popup], cx),
            FitToWindowLayout { popup },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        trigger: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let popup = window.layout_bounds(layout.popup);
        let offset = popup_offset_in_window(trigger, popup, window.viewport_size());
        window.with_element_offset(offset, |window| self.popup.prepaint(window, cx));
        Bounds::new(popup.origin + offset, popup.size)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        trigger: Bounds<Pixels>,
        _layout: &mut Self::RequestLayoutState,
        popup: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(dismiss) = self.on_dismiss.clone() {
            let popup = *popup;
            // Capture phase, so surfaces that stop a press from bubbling still close the popup.
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture
                    && !trigger.contains(&event.position)
                    && !popup.contains(&event.position)
                {
                    dismiss(window, cx);
                }
            });
        }
        self.popup.paint(window, cx);
    }
}

impl IntoElement for FitToWindow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// How far `popup`, laid out against `trigger`, must move to stay inside a `viewport`-sized
/// window.
fn popup_offset_in_window(
    trigger: Bounds<Pixels>,
    popup: Bounds<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let height = popup.size.height;
    let width = popup.size.width;
    let window_top = WINDOW_MARGIN;
    let window_bottom = viewport.height - WINDOW_MARGIN;
    let opens_below = popup.center().y >= trigger.center().y;
    let mut top = popup.top();
    if opens_below && popup.bottom() > window_bottom {
        let gap = (popup.top() - trigger.bottom()).max(px(0.0));
        let flipped = trigger.top() - gap - height;
        let room_below = window_bottom - popup.top();
        let room_above = trigger.top() - gap - window_top;
        if flipped >= window_top || room_above > room_below {
            top = flipped;
        }
    } else if !opens_below && popup.top() < window_top {
        let gap = (trigger.top() - popup.bottom()).max(px(0.0));
        let flipped = trigger.bottom() + gap;
        let room_above = popup.bottom() - window_top;
        let room_below = window_bottom - flipped;
        if flipped + height <= window_bottom || room_below > room_above {
            top = flipped;
        }
    }
    // A popup taller than the window keeps its top edge visible.
    top = top.min(window_bottom - height).max(window_top);
    let left = popup
        .left()
        .min(viewport.width - WINDOW_MARGIN - width)
        .max(WINDOW_MARGIN);
    point(left - popup.left(), top - popup.top())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        Bounds, Context, InteractiveElement, IntoElement, Modifiers, ParentElement, Pixels, Render,
        Styled, TestAppContext, VisualTestContext, Window, deferred, div, point, px, size,
    };

    use super::{WINDOW_MARGIN, fit_to_window, popup_offset_in_window};

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    const VIEWPORT: (f32, f32) = (1000.0, 800.0);

    fn offset(trigger: Bounds<Pixels>, popup: Bounds<Pixels>) -> gpui::Point<Pixels> {
        popup_offset_in_window(trigger, popup, size(px(VIEWPORT.0), px(VIEWPORT.1)))
    }

    #[test]
    fn a_popup_that_fits_stays_where_it_opened() {
        let trigger = bounds(100.0, 100.0, 80.0, 32.0);
        let popup = bounds(100.0, 136.0, 320.0, 400.0);
        assert_eq!(offset(trigger, popup), point(px(0.0), px(0.0)));
    }

    #[test]
    fn a_popup_without_room_below_flips_above_its_trigger_with_the_same_gap() {
        let trigger = bounds(400.0, 600.0, 80.0, 32.0);
        let popup = bounds(400.0, 636.0, 320.0, 400.0);
        let moved = offset(trigger, popup);
        assert_eq!(popup.bottom() + moved.y, trigger.top() - px(4.0));
        assert_eq!(moved.x, px(0.0));
    }

    #[test]
    fn a_popup_without_room_above_flips_below_its_trigger() {
        let trigger = bounds(400.0, 60.0, 80.0, 32.0);
        let popup = bounds(400.0, -344.0, 320.0, 400.0);
        let moved = offset(trigger, popup);
        assert_eq!(popup.top() + moved.y, trigger.bottom() + px(4.0));
    }

    #[test]
    fn a_popup_that_fits_on_neither_side_takes_the_roomier_side_and_slides_inside() {
        let trigger = bounds(400.0, 500.0, 80.0, 32.0);
        let popup = bounds(400.0, 536.0, 320.0, 600.0);
        let moved = offset(trigger, popup);
        assert_eq!(popup.top() + moved.y, WINDOW_MARGIN);
    }

    #[test]
    fn a_popup_slides_back_inside_the_side_edges() {
        let trigger = bounds(900.0, 100.0, 80.0, 32.0);
        let popup = bounds(900.0, 136.0, 320.0, 200.0);
        let moved = offset(trigger, popup);
        assert_eq!(popup.right() + moved.x, px(VIEWPORT.0) - WINDOW_MARGIN);
        assert_eq!(moved.y, px(0.0));
    }

    /// A trigger near the bottom-left of the window with an open popup that counts dismissals.
    struct BottomTriggerHarness {
        dismissals: Rc<Cell<usize>>,
    }

    impl Render for BottomTriggerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let dismissals = self.dismissals.clone();
            div().size_full().relative().child(
                div()
                    .absolute()
                    .bottom(px(40.0))
                    .left(px(40.0))
                    .w(px(80.0))
                    .h(px(32.0))
                    .debug_selector(|| "trigger".into())
                    .child(deferred(
                        fit_to_window(
                            div()
                                .id("popup")
                                .absolute()
                                .top_full()
                                .mt(px(4.0))
                                .left_0()
                                .w(px(320.0))
                                .h(px(300.0))
                                .occlude()
                                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                .debug_selector(|| "popup".into()),
                        )
                        .on_dismiss(move |_, _| dismissals.set(dismissals.get() + 1)),
                    )),
            )
        }
    }

    fn open_harness(cx: &mut TestAppContext) -> (Rc<Cell<usize>>, &mut VisualTestContext) {
        let dismissals = Rc::new(Cell::new(0));
        let harness_dismissals = dismissals.clone();
        let (_, cx) = cx.add_window_view(move |_, _| BottomTriggerHarness {
            dismissals: harness_dismissals,
        });
        cx.run_until_parked();
        (dismissals, cx)
    }

    #[gpui::test]
    fn a_popup_opened_near_the_bottom_of_the_window_renders_above_its_trigger(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = open_harness(cx);
        let trigger = cx.debug_bounds("trigger").expect("trigger bounds");
        let popup = cx.debug_bounds("popup").expect("popup bounds");
        assert_eq!(popup.bottom(), trigger.top() - px(4.0));
        assert_eq!(popup.left(), trigger.left());
    }

    #[gpui::test]
    fn only_a_press_outside_the_trigger_and_the_popup_dismisses_it(cx: &mut TestAppContext) {
        let (dismissals, cx) = open_harness(cx);
        let trigger = cx.debug_bounds("trigger").expect("trigger bounds");
        let popup = cx.debug_bounds("popup").expect("popup bounds");

        cx.simulate_click(popup.center(), Modifiers::none());
        cx.simulate_click(trigger.center(), Modifiers::none());
        assert_eq!(
            dismissals.get(),
            0,
            "presses on the popup or trigger keep it open"
        );

        cx.simulate_click(
            point(popup.right() + px(40.0), popup.top() + px(10.0)),
            Modifiers::none(),
        );
        assert_eq!(dismissals.get(), 1, "a press on empty space closes it");
    }
}
