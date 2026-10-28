//! The terminal's chart chrome: the chrome preferences every pane keeps a copy of and saves,
//! and the drawing chrome over the charts, which is the tool group menu, the drag of the
//! favorites toolbar and the selected drawing's toolbar. The terminal pushes preference changes
//! to the panes and decides when to save them.

use gpui::{Pixels, Point};

use super::chart_chrome::ChartChromePreferences;
use super::drawing_context_toolbar::DrawingContextToolbar;
use super::drawing_toolbar::DrawingToolMenu;

pub(super) struct AppChrome {
    preferences: ChartChromePreferences,
    tool_menu: DrawingToolMenu,
    /// Pointer offset from the favorites toolbar origin while its grip is dragged.
    favorites_grab: Option<Point<Pixels>>,
    context_toolbar: DrawingContextToolbar,
}

impl AppChrome {
    pub(super) fn new(preferences: ChartChromePreferences) -> Self {
        Self {
            preferences,
            tool_menu: DrawingToolMenu::default(),
            favorites_grab: None,
            context_toolbar: DrawingContextToolbar::default(),
        }
    }

    pub(super) const fn preferences(&self) -> &ChartChromePreferences {
        &self.preferences
    }

    pub(super) const fn preferences_mut(&mut self) -> &mut ChartChromePreferences {
        &mut self.preferences
    }

    pub(super) const fn tool_menu(&self) -> &DrawingToolMenu {
        &self.tool_menu
    }

    pub(super) const fn tool_menu_mut(&mut self) -> &mut DrawingToolMenu {
        &mut self.tool_menu
    }

    /// Starts dragging the favorites toolbar, `grab` from its origin.
    pub(super) const fn begin_favorites_drag(&mut self, grab: Point<Pixels>) {
        self.favorites_grab = Some(grab);
    }

    pub(super) const fn favorites_grab(&self) -> Option<Point<Pixels>> {
        self.favorites_grab
    }

    /// Ends the favorites toolbar drag, reporting whether one was in progress.
    pub(super) const fn end_favorites_drag(&mut self) -> bool {
        self.favorites_grab.take().is_some()
    }

    pub(super) const fn context_toolbar(&self) -> &DrawingContextToolbar {
        &self.context_toolbar
    }

    pub(super) const fn context_toolbar_mut(&mut self) -> &mut DrawingContextToolbar {
        &mut self.context_toolbar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn the_favorites_toolbar_drag_ends_once() {
        let mut chrome = AppChrome::new(ChartChromePreferences::default());
        assert_eq!(chrome.favorites_grab(), None);
        assert!(!chrome.end_favorites_drag());
        chrome.begin_favorites_drag(point(px(4.0), px(6.0)));
        assert_eq!(chrome.favorites_grab(), Some(point(px(4.0), px(6.0))));
        assert!(chrome.end_favorites_drag());
        assert!(!chrome.end_favorites_drag());
        assert_eq!(chrome.favorites_grab(), None);
    }
}
