use gpui::{
    App, Div, Hsla, IntoElement, Pixels, RenderOnce, ScrollHandle, Stateful, Window, div,
    prelude::*, px,
};

const SCROLLBAR_WIDTH: Pixels = px(6.0);
const MIN_THUMB_LENGTH: f32 = 18.0;

/// Adds raw GPUI vertical scrolling without changing the content geometry.
/// The Axiusflow scrollbar is painted as an overlay, so reserving a second
/// gutter here would shift centered controls away from their container center.
#[track_caller]
pub(crate) fn tracked_overflow_y_scrollbar(body: Div, handle: &ScrollHandle) -> Stateful<Div> {
    body.id(std::panic::Location::caller())
        .overflow_y_scroll()
        .track_scroll(handle)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThumbMetrics {
    start: f32,
    length: f32,
}

fn thumb_metrics(viewport: f32, content: f32, offset: f32) -> Option<ThumbMetrics> {
    if !viewport.is_finite()
        || !content.is_finite()
        || !offset.is_finite()
        || viewport <= 0.0
        || content <= viewport
    {
        return None;
    }

    let length = (viewport * viewport / content)
        .max(MIN_THUMB_LENGTH)
        .min(viewport);
    let travel = viewport - length;
    let scrollable = content - viewport;
    Some(ThumbMetrics {
        start: (offset.clamp(0.0, scrollable) / scrollable) * travel,
        length,
    })
}

/// A non-interactive, Axiusflow-painted scroll position indicator. Scrolling
/// itself remains owned by GPUI's `ScrollHandle`, so wheel/touchpad behavior
/// and clipping have one state owner.
#[derive(IntoElement)]
pub(crate) struct ThinScrollbar {
    handle: ScrollHandle,
    color: Hsla,
}

impl ThinScrollbar {
    pub(crate) fn new(handle: &ScrollHandle, color: Hsla) -> Self {
        Self {
            handle: handle.clone(),
            color,
        }
    }
}

impl RenderOnce for ThinScrollbar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let viewport = self.handle.bounds().size.height.as_f32();
        let max_offset = self.handle.max_offset().y.as_f32().min(0.0);
        let content = viewport - max_offset;
        let offset = (-self.handle.offset().y.as_f32()).max(0.0);
        let metrics = thumb_metrics(viewport, content, offset);

        div()
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(SCROLLBAR_WIDTH)
            .children(metrics.map(|metrics| {
                div()
                    .absolute()
                    .top(px(metrics.start))
                    .right(px(1.0))
                    .w(px(3.0))
                    .h(px(metrics.length))
                    .rounded_full()
                    .bg(self.color)
            }))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{ScrollHandle, Styled, div};

    use super::{MIN_THUMB_LENGTH, ThumbMetrics, thumb_metrics, tracked_overflow_y_scrollbar};

    #[test]
    fn overlay_scrollbar_does_not_reserve_a_layout_gutter() {
        let handle = ScrollHandle::new();
        let mut body = tracked_overflow_y_scrollbar(div(), &handle);
        assert!(body.style().scrollbar_width.is_none());
    }

    #[test]
    fn thumb_is_hidden_when_content_fits() {
        assert_eq!(thumb_metrics(100.0, 100.0, 0.0), None);
        assert_eq!(thumb_metrics(100.0, 80.0, 0.0), None);
    }

    #[test]
    fn thumb_tracks_clamped_scroll_progress() {
        assert_eq!(
            thumb_metrics(100.0, 400.0, 150.0),
            Some(ThumbMetrics {
                start: 37.5,
                length: 25.0,
            })
        );
        assert_eq!(
            thumb_metrics(100.0, 1_000.0, 10_000.0),
            Some(ThumbMetrics {
                start: 100.0 - MIN_THUMB_LENGTH,
                length: MIN_THUMB_LENGTH,
            })
        );
    }
}
