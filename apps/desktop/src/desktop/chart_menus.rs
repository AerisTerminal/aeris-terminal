//! The chart menus the terminal shows over its workspaces: a chart pane's right-click context
//! menu and the floating chart settings panel with its popovers. The settings panel opens from
//! the context menu, so at most one of the two is open. Inside the panel one popover at a time
//! is shown: a dropdown, a color picker, the template menu or a template dialog. The terminal
//! applies menu actions to the target pane and decides what to repaint and persist.

use gpui::{Entity, Pixels, Point};

use super::chart_context_menus::FloatingPanelPlacement;
use super::{
    ChartColorPickerState, ChartColorSetting, ChartContextAction, ChartContextKind,
    ChartContextMenu, ChartCopyFeedback, ChartSettingsSection, InputState, PriceAxisMenuFlyout,
};

/// What the chart settings panel shows of its saved templates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ChartSettingsTemplateOverlay {
    #[default]
    Closed,
    Menu,
    /// The menu with its saved-templates flyout beside it.
    TemplatesFlyout,
    SaveDialog,
    /// Confirming deletion of the saved template at this index.
    DeleteDialog(usize),
}

impl ChartSettingsTemplateOverlay {
    pub(super) const fn menu_open(self) -> bool {
        matches!(self, Self::Menu | Self::TemplatesFlyout)
    }
}

/// The template popovers that carry no input of their own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TemplateList {
    Menu,
    Flyout,
    DeleteDialog(usize),
}

enum SettingsPopover {
    None,
    /// The settings dropdown that is open, by its row id.
    Select(&'static str),
    ColorPicker(ChartColorPickerState),
    Templates(TemplateList),
    SaveTemplate {
        name: Entity<InputState>,
        error: Option<String>,
    },
}

/// The open chart settings panel: the pane it edits, where it was dragged, its section and
/// its popover.
pub(super) struct ChartSettingsPanel {
    target: ChartContextMenu,
    placement: FloatingPanelPlacement,
    section: ChartSettingsSection,
    popover: SettingsPopover,
}

impl ChartSettingsPanel {
    /// The context menu the panel was opened from, which names the pane it edits.
    pub(super) const fn target(&self) -> &ChartContextMenu {
        &self.target
    }

    pub(super) const fn section(&self) -> ChartSettingsSection {
        self.section
    }

    /// Where the viewer dragged the panel; `None` keeps it centered.
    pub(super) const fn origin(&self) -> Option<Point<Pixels>> {
        self.placement.origin
    }

    pub(super) const fn color_picker(&self) -> Option<&ChartColorPickerState> {
        match &self.popover {
            SettingsPopover::ColorPicker(picker) => Some(picker),
            _ => None,
        }
    }

    pub(super) const fn open_select(&self) -> Option<&'static str> {
        match self.popover {
            SettingsPopover::Select(id) => Some(id),
            _ => None,
        }
    }

    pub(super) const fn template_overlay(&self) -> ChartSettingsTemplateOverlay {
        match self.popover {
            SettingsPopover::Templates(TemplateList::Menu) => ChartSettingsTemplateOverlay::Menu,
            SettingsPopover::Templates(TemplateList::Flyout) => {
                ChartSettingsTemplateOverlay::TemplatesFlyout
            }
            SettingsPopover::Templates(TemplateList::DeleteDialog(index)) => {
                ChartSettingsTemplateOverlay::DeleteDialog(index)
            }
            SettingsPopover::SaveTemplate { .. } => ChartSettingsTemplateOverlay::SaveDialog,
            SettingsPopover::None
            | SettingsPopover::Select(_)
            | SettingsPopover::ColorPicker(_) => ChartSettingsTemplateOverlay::Closed,
        }
    }

    /// The name field of the open save-template dialog.
    pub(super) const fn template_name(&self) -> Option<&Entity<InputState>> {
        match &self.popover {
            SettingsPopover::SaveTemplate { name, .. } => Some(name),
            _ => None,
        }
    }

    pub(super) fn template_error(&self) -> Option<&str> {
        match &self.popover {
            SettingsPopover::SaveTemplate { error, .. } => error.as_deref(),
            _ => None,
        }
    }
}

enum OpenChartMenu {
    None,
    Context(ChartContextMenu),
    Settings(ChartSettingsPanel),
}

pub(super) struct ChartMenus {
    open: OpenChartMenu,
    /// Fences the timer that closes the context menu after a "Copied" confirmation.
    copy_feedback_generation: u64,
}

impl Default for ChartMenus {
    fn default() -> Self {
        Self {
            open: OpenChartMenu::None,
            copy_feedback_generation: 0,
        }
    }
}

impl ChartMenus {
    pub(super) const fn is_open(&self) -> bool {
        !matches!(self.open, OpenChartMenu::None)
    }

    pub(super) const fn context(&self) -> Option<&ChartContextMenu> {
        match &self.open {
            OpenChartMenu::Context(menu) => Some(menu),
            _ => None,
        }
    }

    pub(super) const fn settings(&self) -> Option<&ChartSettingsPanel> {
        match &self.open {
            OpenChartMenu::Settings(panel) => Some(panel),
            _ => None,
        }
    }

    const fn settings_mut(&mut self) -> Option<&mut ChartSettingsPanel> {
        match &mut self.open {
            OpenChartMenu::Settings(panel) => Some(panel),
            _ => None,
        }
    }

    /// The workspace whose chart pointers an open settings panel suspended.
    fn settings_workspace(&self) -> Option<u64> {
        self.settings().map(|panel| panel.target.workspace_id)
    }

    /// Shows `menu` as the context menu. Returns the workspace of a settings panel it closed,
    /// whose chart pointers resume.
    pub(super) fn open_context(&mut self, menu: ChartContextMenu) -> Option<u64> {
        let closed = self.settings_workspace();
        self.open = OpenChartMenu::Context(menu);
        closed
    }

    /// Closes the context menu, reporting whether it was open.
    pub(super) fn close_context(&mut self) -> bool {
        if self.context().is_some() {
            self.open = OpenChartMenu::None;
            true
        } else {
            false
        }
    }

    /// Whether the open context menu is the one opened at `menu`'s pane and position.
    pub(super) fn context_shows(&self, menu: &ChartContextMenu) -> bool {
        self.context().is_some_and(|open| {
            open.workspace_id == menu.workspace_id
                && open.pane_id == menu.pane_id
                && open.position == menu.position
        })
    }

    /// Opens the settings panel on `target`'s pane, centered on its first section with no
    /// popover, in place of the context menu. Returns the workspace of a settings panel it
    /// replaced, whose chart pointers resume.
    pub(super) fn open_settings(&mut self, target: ChartContextMenu) -> Option<u64> {
        let replaced = self.settings_workspace();
        self.open = OpenChartMenu::Settings(ChartSettingsPanel {
            target,
            placement: FloatingPanelPlacement::default(),
            section: ChartSettingsSection::Symbol,
            popover: SettingsPopover::None,
        });
        replaced
    }

    /// Closes the settings panel, returning the workspace whose chart pointers resume.
    pub(super) fn close_settings(&mut self) -> Option<u64> {
        let closed = self.settings_workspace();
        if closed.is_some() {
            self.open = OpenChartMenu::None;
        }
        closed
    }

    /// Shows or hides the capture submenu, reporting whether it changed.
    pub(super) const fn set_capture_flyout(&mut self, open: bool) -> bool {
        if let OpenChartMenu::Context(menu) = &mut self.open
            && menu.capture_flyout_open != open
        {
            menu.capture_flyout_open = open;
            return true;
        }
        false
    }

    /// Opens a price axis submenu, or closes it when it is open. Reports whether a context
    /// menu is open to show it.
    pub(super) fn toggle_price_axis_flyout(&mut self, flyout: PriceAxisMenuFlyout) -> bool {
        let OpenChartMenu::Context(menu) = &mut self.open else {
            return false;
        };
        menu.flyout = if menu.flyout == flyout {
            PriceAxisMenuFlyout::None
        } else {
            flyout
        };
        true
    }

    /// An open price axis menu follows its axis to the other side of the chart.
    pub(super) const fn set_price_axis_left(&mut self, left: bool) -> bool {
        if let OpenChartMenu::Context(menu) = &mut self.open
            && let ChartContextKind::PriceAxis {
                left: open_left, ..
            } = &mut menu.kind
        {
            *open_left = left;
            return true;
        }
        false
    }

    /// Shows "Copied" on the open context menu's `action` row. Returns the generation the
    /// closing timer must still match, or `None` when no context menu is open.
    pub(super) const fn begin_copy_feedback(&mut self, action: ChartContextAction) -> Option<u64> {
        let OpenChartMenu::Context(menu) = &mut self.open else {
            return None;
        };
        self.copy_feedback_generation = self.copy_feedback_generation.saturating_add(1);
        let generation = self.copy_feedback_generation;
        menu.copy_feedback = Some(ChartCopyFeedback { action, generation });
        Some(generation)
    }

    /// Whether the open context menu still shows the confirmation of `generation`, so its
    /// timer may close it.
    pub(super) fn copy_feedback_is_current(&self, generation: u64) -> bool {
        self.context()
            .and_then(|menu| menu.copy_feedback)
            .is_some_and(|feedback| feedback.generation == generation)
    }

    /// Moves the settings panel to another section with no popover, reporting whether it
    /// changed.
    pub(super) fn set_settings_section(&mut self, section: ChartSettingsSection) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        if panel.section == section {
            return false;
        }
        panel.section = section;
        panel.popover = SettingsPopover::None;
        true
    }

    /// Closes a dropdown, color picker or template menu; template dialogs stay. Reports
    /// whether one closed.
    pub(super) fn dismiss_settings_popovers(&mut self) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        if matches!(
            panel.popover,
            SettingsPopover::Select(_)
                | SettingsPopover::ColorPicker(_)
                | SettingsPopover::Templates(TemplateList::Menu | TemplateList::Flyout)
        ) {
            panel.popover = SettingsPopover::None;
            true
        } else {
            false
        }
    }

    /// Closes every popover of the settings panel, reporting whether the panel is open.
    pub(super) fn close_settings_popover(&mut self) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        panel.popover = SettingsPopover::None;
        true
    }

    /// Opens the dropdown `id` in place of any other popover, or closes it when it is open.
    /// Reports whether the panel is open.
    pub(super) fn toggle_settings_select(&mut self, id: &'static str) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        panel.popover = if matches!(panel.popover, SettingsPopover::Select(open) if open == id) {
            SettingsPopover::None
        } else {
            SettingsPopover::Select(id)
        };
        true
    }

    /// Closes the open dropdown after its choice, reporting whether one was open.
    pub(super) fn close_settings_select(&mut self) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        if matches!(panel.popover, SettingsPopover::Select(_)) {
            panel.popover = SettingsPopover::None;
            true
        } else {
            false
        }
    }

    /// Whether the color picker of `setting` is open.
    pub(super) fn color_picker_shows(&self, setting: ChartColorSetting) -> bool {
        self.settings()
            .and_then(ChartSettingsPanel::color_picker)
            .is_some_and(|picker| picker.setting == setting)
    }

    /// Opens the color picker of `setting` with its hex `input`, in place of any other
    /// popover. Reports whether the panel is open.
    pub(super) fn open_color_picker(
        &mut self,
        setting: ChartColorSetting,
        input: Entity<InputState>,
    ) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        panel.popover = SettingsPopover::ColorPicker(ChartColorPickerState {
            setting,
            input,
            error: None,
        });
        true
    }

    /// Closes the color picker, reporting whether it was open.
    pub(super) fn close_color_picker(&mut self) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        if matches!(panel.popover, SettingsPopover::ColorPicker(_)) {
            panel.popover = SettingsPopover::None;
            true
        } else {
            false
        }
    }

    /// Shows `error` under the color picker of `setting`, reporting whether it is open.
    pub(super) fn set_color_error(
        &mut self,
        setting: ChartColorSetting,
        error: Option<String>,
    ) -> bool {
        if let Some(panel) = self.settings_mut()
            && let SettingsPopover::ColorPicker(picker) = &mut panel.popover
            && picker.setting == setting
        {
            picker.error = error;
            return true;
        }
        false
    }

    /// Opens the template menu in place of any other popover, or closes it when it is open.
    /// Reports whether the panel is open.
    pub(super) fn toggle_template_menu(&mut self) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        panel.popover = if matches!(
            panel.popover,
            SettingsPopover::Templates(TemplateList::Menu | TemplateList::Flyout)
        ) {
            SettingsPopover::None
        } else {
            SettingsPopover::Templates(TemplateList::Menu)
        };
        true
    }

    /// Shows the saved-templates flyout beside the open template menu, or hides it. Reports
    /// whether it changed.
    pub(super) fn show_templates_flyout(&mut self, open: bool) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        let SettingsPopover::Templates(list @ (TemplateList::Menu | TemplateList::Flyout)) =
            &mut panel.popover
        else {
            return false;
        };
        let next = if open {
            TemplateList::Flyout
        } else {
            TemplateList::Menu
        };
        if *list == next {
            return false;
        }
        *list = next;
        true
    }

    /// Asks to confirm deleting the saved template at `index`, reporting whether the panel is
    /// open. The caller checks that the template exists.
    pub(super) fn confirm_template_delete(&mut self, index: usize) -> bool {
        self.show_templates(TemplateList::DeleteDialog(index))
    }

    /// Returns from a template dialog to the saved-templates flyout, reporting whether the
    /// panel is open.
    pub(super) fn show_saved_templates(&mut self) -> bool {
        self.show_templates(TemplateList::Flyout)
    }

    /// Returns from the save dialog to the template menu, reporting whether the panel is open.
    pub(super) fn show_template_menu(&mut self) -> bool {
        self.show_templates(TemplateList::Menu)
    }

    fn show_templates(&mut self, list: TemplateList) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        panel.popover = SettingsPopover::Templates(list);
        true
    }

    /// Ends the open delete confirmation on the saved-templates flyout, returning the index
    /// of the template the viewer confirmed deleting.
    pub(super) fn take_template_delete(&mut self) -> Option<usize> {
        let panel = self.settings_mut()?;
        let SettingsPopover::Templates(TemplateList::DeleteDialog(index)) = panel.popover else {
            return None;
        };
        panel.popover = SettingsPopover::Templates(TemplateList::Flyout);
        Some(index)
    }

    /// Opens the save-template dialog with its `name` field, reporting whether the panel is
    /// open.
    pub(super) fn open_template_save(&mut self, name: Entity<InputState>) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        panel.popover = SettingsPopover::SaveTemplate { name, error: None };
        true
    }

    /// Shows why the template could not be saved, reporting whether the dialog is open.
    pub(super) fn set_template_error(&mut self, message: impl Into<String>) -> bool {
        if let Some(panel) = self.settings_mut()
            && let SettingsPopover::SaveTemplate { error, .. } = &mut panel.popover
        {
            *error = Some(message.into());
            return true;
        }
        false
    }

    /// Closes the template menu or dialog after a template was applied, reporting whether
    /// one was open. A dropdown or color picker stays.
    pub(super) fn close_templates(&mut self) -> bool {
        let Some(panel) = self.settings_mut() else {
            return false;
        };
        if matches!(
            panel.popover,
            SettingsPopover::Templates(_) | SettingsPopover::SaveTemplate { .. }
        ) {
            panel.popover = SettingsPopover::None;
            true
        } else {
            false
        }
    }

    /// Records where the panel's title bar was grabbed, measured from the panel origin.
    pub(super) fn grab_settings_panel(&mut self, offset: Point<Pixels>) {
        if let Some(panel) = self.settings_mut() {
            panel.placement.grab(offset);
        }
    }

    /// Moves the settings panel under the pointer, reporting whether it moved.
    pub(super) fn move_settings_panel(&mut self, pointer: Point<Pixels>) -> bool {
        self.settings_mut()
            .is_some_and(|panel| panel.placement.follow(pointer))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    fn menu(workspace_id: u64, pane_id: u64, x: f32) -> ChartContextMenu {
        ChartContextMenu {
            workspace_id,
            pane_id,
            position: point(px(x), px(20.0)),
            kind: ChartContextKind::Pane,
            flyout: PriceAxisMenuFlyout::None,
            capture_flyout_open: false,
            copy_price: Some("123.45".into()),
            copy_feedback: None,
        }
    }

    fn price_axis_menu(left: bool) -> ChartContextMenu {
        ChartContextMenu {
            kind: ChartContextKind::PriceAxis { pane: 0, left },
            ..menu(1, 2, 20.0)
        }
    }

    #[test]
    fn the_context_menu_and_settings_panel_replace_each_other() {
        let mut menus = ChartMenus::default();
        assert!(!menus.is_open());
        assert_eq!(menus.open_context(menu(1, 2, 20.0)), None);
        assert!(menus.context_shows(&menu(1, 2, 20.0)));
        assert!(!menus.context_shows(&menu(1, 2, 30.0)));
        assert_eq!(menus.open_settings(menu(1, 2, 20.0)), None);
        assert!(menus.context().is_none());
        assert_eq!(
            menus.settings().map(|panel| panel.target().pane_id),
            Some(2)
        );
        assert_eq!(
            menus.open_context(menu(3, 4, 20.0)),
            Some(1),
            "the closed panel's workspace resumes its chart pointers"
        );
        assert!(menus.close_context());
        assert!(!menus.close_context());
        assert_eq!(menus.close_settings(), None);
        assert!(!menus.is_open());
    }

    #[test]
    fn copy_feedback_generation_fences_stale_close_timers() {
        let mut menus = ChartMenus::default();
        assert_eq!(
            menus.begin_copy_feedback(ChartContextAction::CopyPrice),
            None
        );
        assert_eq!(menus.open_context(menu(1, 2, 20.0)), None);
        let first = menus.begin_copy_feedback(ChartContextAction::CopyCapture);
        let second = menus.begin_copy_feedback(ChartContextAction::CopyPrice);
        assert_eq!((first, second), (Some(1), Some(2)));
        assert!(!menus.copy_feedback_is_current(1));
        assert!(menus.copy_feedback_is_current(2));
        assert_eq!(menus.open_context(menu(1, 2, 20.0)), None);
        assert!(
            !menus.copy_feedback_is_current(2),
            "a reopened menu is not closed by an older timer"
        );
        assert!(menus.close_context());
        assert!(!menus.copy_feedback_is_current(2));
    }

    #[test]
    fn context_submenus_follow_the_pointer_and_the_axis() {
        let mut menus = ChartMenus::default();
        assert!(!menus.set_capture_flyout(true));
        assert_eq!(menus.open_context(price_axis_menu(false)), None);
        assert!(menus.set_capture_flyout(true));
        assert!(!menus.set_capture_flyout(true));
        assert!(menus.toggle_price_axis_flyout(PriceAxisMenuFlyout::Labels));
        assert_eq!(
            menus.context().map(|menu| menu.flyout),
            Some(PriceAxisMenuFlyout::Labels)
        );
        assert!(menus.toggle_price_axis_flyout(PriceAxisMenuFlyout::Labels));
        assert_eq!(
            menus.context().map(|menu| menu.flyout),
            Some(PriceAxisMenuFlyout::None)
        );
        assert!(menus.set_price_axis_left(true));
        assert_eq!(
            menus.context().map(|menu| menu.kind),
            Some(ChartContextKind::PriceAxis {
                pane: 0,
                left: true
            })
        );
    }

    #[test]
    fn the_settings_panel_shows_one_popover_at_a_time() {
        let mut menus = ChartMenus::default();
        assert!(!menus.toggle_settings_select("line_style"));
        assert_eq!(menus.open_settings(menu(1, 2, 20.0)), None);
        let panel = |menus: &ChartMenus| {
            let panel = menus.settings().expect("open panel");
            (panel.open_select(), panel.template_overlay())
        };
        assert!(menus.toggle_settings_select("line_style"));
        assert_eq!(
            panel(&menus),
            (Some("line_style"), ChartSettingsTemplateOverlay::Closed)
        );
        assert!(menus.toggle_template_menu());
        assert_eq!(panel(&menus), (None, ChartSettingsTemplateOverlay::Menu));
        assert!(menus.show_templates_flyout(true));
        assert!(!menus.show_templates_flyout(true));
        assert_eq!(
            panel(&menus),
            (None, ChartSettingsTemplateOverlay::TemplatesFlyout)
        );
        assert!(menus.toggle_settings_select("time_zone"));
        assert_eq!(
            panel(&menus),
            (Some("time_zone"), ChartSettingsTemplateOverlay::Closed)
        );
        assert!(menus.toggle_settings_select("time_zone"));
        assert_eq!(panel(&menus), (None, ChartSettingsTemplateOverlay::Closed));
        assert!(
            !menus.show_templates_flyout(true),
            "no template menu is open"
        );
        assert!(!menus.close_settings_select());
    }

    #[test]
    fn dismissing_popovers_keeps_a_template_dialog() {
        let mut menus = ChartMenus::default();
        assert_eq!(menus.open_settings(menu(1, 2, 20.0)), None);
        assert!(menus.toggle_template_menu());
        assert!(menus.dismiss_settings_popovers());
        assert!(!menus.dismiss_settings_popovers());
        assert!(menus.confirm_template_delete(3));
        assert!(!menus.dismiss_settings_popovers());
        assert_eq!(
            menus.settings().map(ChartSettingsPanel::template_overlay),
            Some(ChartSettingsTemplateOverlay::DeleteDialog(3))
        );
        assert_eq!(menus.take_template_delete(), Some(3));
        assert_eq!(menus.take_template_delete(), None);
        assert_eq!(
            menus.settings().map(ChartSettingsPanel::template_overlay),
            Some(ChartSettingsTemplateOverlay::TemplatesFlyout)
        );
        assert!(menus.close_templates());
        assert!(!menus.close_templates());
    }

    #[test]
    fn a_new_section_or_panel_starts_without_a_popover() {
        let mut menus = ChartMenus::default();
        assert_eq!(menus.open_settings(menu(1, 2, 20.0)), None);
        menus.grab_settings_panel(point(px(5.0), px(5.0)));
        assert!(menus.move_settings_panel(point(px(105.0), px(55.0))));
        assert!(!menus.move_settings_panel(point(px(105.0), px(55.0))));
        assert_eq!(
            menus.settings().and_then(ChartSettingsPanel::origin),
            Some(point(px(100.0), px(50.0)))
        );
        assert!(menus.toggle_template_menu());
        assert!(!menus.set_settings_section(ChartSettingsSection::Symbol));
        assert!(menus.set_settings_section(ChartSettingsSection::Legend));
        let panel = menus.settings().expect("open panel");
        assert_eq!(panel.section(), ChartSettingsSection::Legend);
        assert_eq!(
            panel.template_overlay(),
            ChartSettingsTemplateOverlay::Closed
        );
        assert_eq!(menus.close_settings(), Some(1));
        assert_eq!(menus.open_settings(menu(1, 2, 20.0)), None);
        let panel = menus.settings().expect("reopened panel");
        assert_eq!(panel.section(), ChartSettingsSection::Symbol);
        assert_eq!(panel.origin(), None, "a fresh panel starts centered");
    }
}
