use std::{sync::Arc, time::Duration};

use gpui::{
    Animation, AnimationExt as _, App, ElementId, Hsla, IntoElement, Pixels, RenderOnce,
    SharedString, Window, div, prelude::*,
};

use super::icon::Icon;

/// A looping activity spinner whose SVG is owned by Axiusflow's asset bundle.
#[derive(IntoElement)]
pub(crate) struct Loader {
    id: ElementId,
    icon: Icon,
    color: Option<Hsla>,
    size: Pixels,
    period: Duration,
}

impl Loader {
    pub(crate) fn new(id: impl Into<ElementId>, icon: Icon) -> Self {
        Self {
            id: id.into(),
            icon,
            color: None,
            size: gpui::px(16.0),
            period: Duration::from_millis(700),
        }
    }

    pub(crate) fn from_path(id: impl Into<ElementId>, path: impl Into<SharedString>) -> Self {
        Self::new(id, Icon::new(path))
    }

    pub(crate) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    pub(crate) fn with_size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    fn animation_id(&self) -> ElementId {
        ElementId::NamedChild(Arc::new(self.id.clone()), "rotation".into())
    }
}

impl RenderOnce for Loader {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let animation_id = self.animation_id();
        div().flex_none().child(
            self.icon
                .with_size(self.size)
                .when_some(self.color, Icon::color)
                .with_animation(
                    animation_id,
                    Animation::new(self.period).repeat(),
                    Icon::rotate,
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use gpui::ElementId;

    use super::{Icon, Loader};

    #[test]
    fn loader_animation_is_scoped_below_its_owner() {
        let loader = Loader::new("quote-loader", Icon::new("loader.svg"));
        assert_eq!(
            loader.animation_id(),
            ElementId::NamedChild(
                std::sync::Arc::new(ElementId::from("quote-loader")),
                "rotation".into()
            )
        );
    }
}
