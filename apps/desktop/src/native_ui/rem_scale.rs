//! Screen-aware sizing for whole panels.
//!
//! [`RemScale`] lays out, prepaints and paints its child with the window's rem size
//! multiplied by a [`MenuScale`]. Every rem-based length inside it (text sizes, the
//! `p_*`/`gap_*` spacing helpers and lengths from [`design_rems`]) grows together, so a
//! panel scales as one unit without threading a factor through each of its builders.
//! Lengths given in `px` stay fixed; use that deliberately, e.g. for icon hit targets
//! whose geometry must stay aligned to device pixels.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Rems, Window, rems,
};

use super::menu::MenuScale;

/// Logical pixels in one rem at the window root. Aeris keeps GPUI's default root rem, so a
/// fixed design converted with [`design_rems`] renders at its exact pixel size outside a
/// [`RemScale`] subtree.
pub(crate) const ROOT_REM_PX: f32 = 16.0;

/// Expresses a logical-pixel design length in rems so it follows an enclosing [`RemScale`].
pub(crate) fn design_rems(logical: f32) -> Rems {
    rems(logical / ROOT_REM_PX)
}

/// Renders `child` with every rem-based length scaled by `scale`.
pub(crate) fn rem_scaled(scale: MenuScale, child: impl IntoElement) -> RemScale {
    RemScale {
        scale,
        child: child.into_any_element(),
    }
}

pub(crate) struct RemScale {
    scale: MenuScale,
    child: AnyElement,
}

impl RemScale {
    fn rem_size(&self, window: &Window) -> Pixels {
        window.rem_size() * self.scale.factor()
    }
}

impl IntoElement for RemScale {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RemScale {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
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
        let rem_size = self.rem_size(window);
        let layout_id = window.with_rem_size(Some(rem_size), |window| {
            self.child.request_layout(window, cx)
        });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let rem_size = self.rem_size(window);
        window.with_rem_size(Some(rem_size), |window| {
            self.child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let rem_size = self.rem_size(window);
        window.with_rem_size(Some(rem_size), |window| self.child.paint(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use gpui::rems;

    use super::{ROOT_REM_PX, design_rems};

    #[test]
    fn design_lengths_round_trip_through_the_root_rem() {
        assert_eq!(design_rems(ROOT_REM_PX), rems(1.0));
        assert_eq!(design_rems(38.0), rems(38.0 / 16.0));
    }
}
