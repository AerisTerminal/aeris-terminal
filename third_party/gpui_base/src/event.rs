use gpui::{
    App, ClickEvent, InteractiveElement, OngoingScroll, Pixels, Point, Stateful,
    StatefulInteractiveElement, TouchPhase, Window,
};

pub trait OngoingScrollExt {
    /// Locks a wheel delta to the axis its gesture started on.
    fn lock_axis(&mut self, delta: &mut Point<Pixels>, touch_phase: TouchPhase);
}

impl OngoingScrollExt for OngoingScroll {
    fn lock_axis(&mut self, delta: &mut Point<Pixels>, touch_phase: TouchPhase) {
        self.filter(delta, touch_phase);
    }
}

pub trait InteractiveElementExt: InteractiveElement {
    /// Locks scrolling to the gesture's dominant axis.
    fn lock_scroll_axis(self) -> Self
    where
        Self: Sized + StatefulInteractiveElement,
    {
        self.restrict_scroll_to_axis()
    }

    /// Set the listener for a double click event.
    fn on_double_click(
        mut self,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self
    where
        Self: Sized,
    {
        self.interactivity().on_click(move |event, window, cx| {
            if event.click_count() == 2 {
                listener(event, window, cx);
            }
        });
        self
    }
}

impl<E: InteractiveElement> InteractiveElementExt for Stateful<E> {}
