//! The terminal's app-level overlays: the profile menu and the page menu under the title bar,
//! the keyboard shortcuts dialog and the command palette. Each one covers the window with a
//! scrim or takes the keyboard until it is dismissed, so no two can be in use together: one
//! slot holds the open overlay and opening another replaces it. Chart menus and pane dialogs
//! have their own owners. The terminal decides what to repaint and where focus goes.

use gpui::{Entity, Pixels, Point};

use super::InputState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OverlayKind {
    /// The profile menu under the avatar.
    PlatformMenu,
    /// The page menu under the navigation button.
    AppNavigation,
    KeyboardShortcuts,
    CommandPalette,
}

impl OverlayKind {
    /// Title-bar menus keep a frameless title bar on screen while they are open.
    pub(super) const fn pins_title_bar(self) -> bool {
        matches!(self, Self::PlatformMenu | Self::AppNavigation)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum AppOverlay {
    /// Anchored under the click point that opened it.
    PlatformMenu(Point<Pixels>),
    /// Anchored under the click point that opened it.
    AppNavigation(Point<Pixels>),
    KeyboardShortcuts,
    CommandPalette {
        /// The highlighted result row.
        selection: usize,
        /// Why the last query could not run.
        message: Option<String>,
    },
}

impl AppOverlay {
    const fn kind(&self) -> OverlayKind {
        match self {
            Self::PlatformMenu(_) => OverlayKind::PlatformMenu,
            Self::AppNavigation(_) => OverlayKind::AppNavigation,
            Self::KeyboardShortcuts => OverlayKind::KeyboardShortcuts,
            Self::CommandPalette { .. } => OverlayKind::CommandPalette,
        }
    }
}

pub(super) struct OverlayStack {
    open: Option<AppOverlay>,
    /// Lives as long as the window so its change subscription stays attached between opens.
    command_palette_input: Entity<InputState>,
}

impl OverlayStack {
    pub(super) const fn new(command_palette_input: Entity<InputState>) -> Self {
        Self {
            open: None,
            command_palette_input,
        }
    }

    pub(super) fn kind(&self) -> Option<OverlayKind> {
        self.open.as_ref().map(AppOverlay::kind)
    }

    pub(super) fn is_open(&self, kind: OverlayKind) -> bool {
        self.kind() == Some(kind)
    }

    /// Closes the overlay `kind`, reporting whether it was open. Another open overlay stays.
    pub(super) fn close(&mut self, kind: OverlayKind) -> bool {
        if self.is_open(kind) {
            self.open = None;
            true
        } else {
            false
        }
    }

    /// Shows `overlay`, returning the other overlay it replaced.
    fn replace(&mut self, overlay: AppOverlay) -> Option<OverlayKind> {
        let kind = overlay.kind();
        self.open
            .replace(overlay)
            .map(|previous| previous.kind())
            .filter(|previous| *previous != kind)
    }

    /// Opens the profile menu under `anchor`, or closes it when it is open. Reports whether it
    /// is open afterwards.
    pub(super) fn toggle_platform_menu(&mut self, anchor: Point<Pixels>) -> bool {
        if self.close(OverlayKind::PlatformMenu) {
            return false;
        }
        self.open = Some(AppOverlay::PlatformMenu(anchor));
        true
    }

    pub(super) const fn platform_menu_anchor(&self) -> Option<Point<Pixels>> {
        match self.open {
            Some(AppOverlay::PlatformMenu(anchor)) => Some(anchor),
            _ => None,
        }
    }

    /// Opens the page menu under `anchor`, or closes it when it is open. Reports whether it is
    /// open afterwards.
    pub(super) fn toggle_app_navigation(&mut self, anchor: Point<Pixels>) -> bool {
        if self.close(OverlayKind::AppNavigation) {
            return false;
        }
        self.open = Some(AppOverlay::AppNavigation(anchor));
        true
    }

    pub(super) const fn app_navigation_anchor(&self) -> Option<Point<Pixels>> {
        match self.open {
            Some(AppOverlay::AppNavigation(anchor)) => Some(anchor),
            _ => None,
        }
    }

    /// Opens the keyboard shortcuts dialog, returning the other overlay it replaced.
    pub(super) fn open_keyboard_shortcuts(&mut self) -> Option<OverlayKind> {
        self.replace(AppOverlay::KeyboardShortcuts)
    }

    pub(super) const fn command_palette_input(&self) -> &Entity<InputState> {
        &self.command_palette_input
    }

    /// Opens the command palette on its first result with no message, returning the other
    /// overlay it replaced. The caller clears and focuses the query input.
    pub(super) fn open_command_palette(&mut self) -> Option<OverlayKind> {
        self.replace(AppOverlay::CommandPalette {
            selection: 0,
            message: None,
        })
    }

    /// The highlighted result row of the open command palette.
    pub(super) const fn command_palette_selection(&self) -> Option<usize> {
        match self.open {
            Some(AppOverlay::CommandPalette { selection, .. }) => Some(selection),
            _ => None,
        }
    }

    pub(super) fn command_palette_message(&self) -> Option<&str> {
        match &self.open {
            Some(AppOverlay::CommandPalette { message, .. }) => message.as_deref(),
            _ => None,
        }
    }

    /// Moves the highlight one row up, reporting whether it moved.
    pub(super) const fn select_previous_command(&mut self) -> bool {
        if let Some(AppOverlay::CommandPalette { selection, .. }) = &mut self.open
            && *selection > 0
        {
            *selection -= 1;
            return true;
        }
        false
    }

    /// Moves the highlight one row down within `result_count` rows, reporting whether it moved.
    pub(super) fn select_next_command(&mut self, result_count: usize) -> bool {
        if let Some(AppOverlay::CommandPalette { selection, .. }) = &mut self.open {
            let next = (*selection + 1).min(result_count.saturating_sub(1));
            if next != *selection {
                *selection = next;
                return true;
            }
        }
        false
    }

    /// Shows why the query could not run, reporting whether the palette is open to show it.
    pub(super) fn set_command_palette_message(&mut self, text: impl Into<String>) -> bool {
        if let Some(AppOverlay::CommandPalette { message, .. }) = &mut self.open {
            *message = Some(text.into());
            return true;
        }
        false
    }

    /// An edited query starts again from the first result with no message. Reports whether
    /// the open palette changed.
    pub(super) fn command_query_changed(&mut self) -> bool {
        if let Some(AppOverlay::CommandPalette { selection, message }) = &mut self.open
            && (*selection != 0 || message.is_some())
        {
            *selection = 0;
            *message = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, point, px};

    use super::*;

    fn stack(cx: &mut TestAppContext) -> OverlayStack {
        let (input, _) = cx.add_window_view(|window, cx| {
            gpui_base::init(cx);
            InputState::new(window, cx)
        });
        OverlayStack::new(input)
    }

    #[gpui::test]
    fn one_overlay_is_open_and_opening_another_replaces_it(cx: &mut TestAppContext) {
        let mut overlays = stack(cx);
        assert_eq!(overlays.kind(), None);
        assert!(overlays.toggle_platform_menu(point(px(10.0), px(4.0))));
        assert_eq!(
            overlays.platform_menu_anchor(),
            Some(point(px(10.0), px(4.0)))
        );
        assert_eq!(
            overlays.open_keyboard_shortcuts(),
            Some(OverlayKind::PlatformMenu)
        );
        assert_eq!(overlays.platform_menu_anchor(), None);
        assert!(overlays.is_open(OverlayKind::KeyboardShortcuts));
        assert_eq!(
            overlays.open_command_palette(),
            Some(OverlayKind::KeyboardShortcuts)
        );
        assert_eq!(
            overlays.open_keyboard_shortcuts(),
            Some(OverlayKind::CommandPalette)
        );
        assert_eq!(
            overlays.open_keyboard_shortcuts(),
            None,
            "reopening the same overlay replaces nothing else"
        );
    }

    #[gpui::test]
    fn closing_an_overlay_leaves_a_different_open_one(cx: &mut TestAppContext) {
        let mut overlays = stack(cx);
        assert!(overlays.toggle_app_navigation(point(px(3.0), px(2.0))));
        assert!(!overlays.close(OverlayKind::PlatformMenu));
        assert_eq!(
            overlays.app_navigation_anchor(),
            Some(point(px(3.0), px(2.0)))
        );
        assert!(!overlays.toggle_app_navigation(point(px(9.0), px(9.0))));
        assert_eq!(overlays.kind(), None);
        assert!(!overlays.close(OverlayKind::AppNavigation));
    }

    #[test]
    fn only_title_bar_menus_pin_the_title_bar() {
        assert!(OverlayKind::PlatformMenu.pins_title_bar());
        assert!(OverlayKind::AppNavigation.pins_title_bar());
        assert!(!OverlayKind::KeyboardShortcuts.pins_title_bar());
        assert!(!OverlayKind::CommandPalette.pins_title_bar());
    }

    #[gpui::test]
    fn the_command_palette_highlight_stays_within_its_results(cx: &mut TestAppContext) {
        let mut overlays = stack(cx);
        assert!(
            !overlays.select_next_command(3),
            "a closed palette has no highlight"
        );
        assert_eq!(overlays.open_command_palette(), None);
        assert_eq!(overlays.command_palette_selection(), Some(0));
        assert!(!overlays.select_previous_command());
        assert!(overlays.select_next_command(3));
        assert!(overlays.select_next_command(3));
        assert!(!overlays.select_next_command(3));
        assert_eq!(overlays.command_palette_selection(), Some(2));
        assert!(overlays.select_previous_command());
        assert_eq!(overlays.command_palette_selection(), Some(1));
    }

    #[gpui::test]
    fn an_edited_query_resets_the_highlight_and_message(cx: &mut TestAppContext) {
        let mut overlays = stack(cx);
        assert!(!overlays.set_command_palette_message("No matching command"));
        assert_eq!(overlays.open_command_palette(), None);
        assert!(overlays.select_next_command(4));
        assert!(overlays.set_command_palette_message("No matching command"));
        assert_eq!(
            overlays.command_palette_message(),
            Some("No matching command")
        );
        assert!(overlays.command_query_changed());
        assert_eq!(overlays.command_palette_selection(), Some(0));
        assert_eq!(overlays.command_palette_message(), None);
        assert!(!overlays.command_query_changed());
        assert!(overlays.close(OverlayKind::CommandPalette));
        assert_eq!(overlays.command_palette_selection(), None);
        assert_eq!(overlays.open_command_palette(), None);
        assert_eq!(
            overlays.command_palette_message(),
            None,
            "a reopened palette starts without the old message"
        );
    }
}
