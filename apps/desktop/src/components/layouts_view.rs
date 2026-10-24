//! The layouts start page, the chart header's layout menu and the layout dialogs: renaming,
//! copying, creating and deleting saved layouts, and the prompts that ask whether to save a
//! layout with unsaved changes before its tab or the window closes.

use super::*;
use aeris_desktop::command_registry::CommandId;
use layout_library::{MAXIMUM_SAVED_LAYOUTS, UNTITLED_LAYOUT_NAME};

const START_PAGE_WIDTH: f32 = 720.0;
const START_PAGE_PADDING_Y: f32 = 48.0;
const START_PAGE_PADDING_X: f32 = 24.0;
const START_PAGE_GAP: f32 = 12.0;
const START_PAGE_SEARCH_WIDTH: f32 = 260.0;
const START_PAGE_SEARCH_HEIGHT: f32 = 28.0;
const LAYOUT_CARD_ICON_SIZE: f32 = 36.0;
const LAYOUT_MENU_WIDTH: f32 = 260.0;
const LAYOUT_CARD_MENU_WIDTH: f32 = 180.0;
/// Layouts the header menu offers under "Recently used".
const RECENT_LAYOUT_LIMIT: usize = 5;
pub(super) const UNSAVED_LAYOUT_DOT_SIZE: f32 = 6.0;

/// The start page, the layout menus and the one layout dialog open at a time.
#[derive(Default)]
pub(super) struct LayoutsUi {
    start_page: Option<StartPage>,
    layout_menu_open: bool,
    card_menu: Option<u64>,
    name_dialog: Option<LayoutNameDialog>,
    delete_confirmation: Option<u64>,
    close_prompt: Option<ClosePrompt>,
}

/// The transient "New tab" placeholder. It is not a workspace and is never persisted; while
/// `shown` it is the selected tab and the page replaces the active workspace.
struct StartPage {
    shown: bool,
    search: Entity<InputState>,
    _search_changes: gpui::Subscription,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LayoutNamePurpose {
    Rename(u64),
    Copy,
    Create,
}

impl LayoutNamePurpose {
    const fn title(self) -> &'static str {
        match self {
            Self::Rename(_) => "Rename layout",
            Self::Copy => "Make a copy of layout",
            Self::Create => "Create new layout",
        }
    }

    const fn confirm_label(self) -> &'static str {
        match self {
            Self::Rename(_) => "Rename",
            Self::Copy => "Make a copy",
            Self::Create => "Create",
        }
    }
}

struct LayoutNameDialog {
    purpose: LayoutNamePurpose,
    input: Entity<InputState>,
    error: Option<String>,
    _input_events: gpui::Subscription,
}

/// What asks whether to save unsaved layouts before it closes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ClosePrompt {
    Tab(u64),
    Window,
}

impl LayoutsUi {
    pub(super) fn start_page_open(&self) -> bool {
        self.start_page.is_some()
    }

    pub(super) fn start_page_shown(&self) -> bool {
        self.start_page.as_ref().is_some_and(|page| page.shown)
    }

    pub(super) fn modal_open(&self) -> bool {
        self.name_dialog.is_some()
            || self.delete_confirmation.is_some()
            || self.close_prompt.is_some()
    }

    pub(super) fn menu_open(&self) -> bool {
        self.layout_menu_open || self.card_menu.is_some()
    }

    /// Whether the start page or a layout dialog takes the keyboard from the workspace.
    pub(super) fn covers_workspace(&self) -> bool {
        self.start_page_shown() || self.modal_open()
    }

    #[cfg(test)]
    pub(super) fn close_prompt(&self) -> Option<ClosePrompt> {
        self.close_prompt
    }

    #[cfg(test)]
    pub(super) fn name_dialog(&self) -> Option<(LayoutNamePurpose, Option<&str>)> {
        self.name_dialog
            .as_ref()
            .map(|dialog| (dialog.purpose, dialog.error.as_deref()))
    }

    #[cfg(test)]
    pub(super) fn name_input(&self) -> Option<&Entity<InputState>> {
        self.name_dialog.as_ref().map(|dialog| &dialog.input)
    }

    #[cfg(test)]
    pub(super) fn delete_confirmation(&self) -> Option<u64> {
        self.delete_confirmation
    }
}

/// "Saved Jan 5, 2026" in UTC, or `None` for a layout never saved explicitly.
fn saved_date_label(saved_at_unix_ms: i64) -> Option<String> {
    if saved_at_unix_ms <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(saved_at_unix_ms)
        .map(|saved| saved.format("Saved %b %-d, %Y").to_string())
}

/// The first chart's symbol and interval, the pane count and the saved date of a layout.
fn layout_card_detail(layout: &WorkspaceSavedLayoutState) -> String {
    let panes = layout
        .tab
        .as_ref()
        .map_or(&[][..], |tab| tab.panes.as_slice());
    let mut parts = Vec::new();
    if let Some(pane) = panes.first() {
        let symbol = pane
            .instrument
            .as_ref()
            .map_or("No market", |instrument| instrument.display_symbol.as_str());
        parts.push(if pane.interval.is_empty() {
            symbol.to_string()
        } else {
            format!("{symbol} {}", pane.interval)
        });
    }
    if panes.len() > 1 {
        parts.push(format!("{} charts", panes.len()));
    }
    parts.extend(saved_date_label(layout.saved_at_unix_ms));
    parts.join(" \u{b7} ")
}

/// The saved layouts the start page lists for `query`, most recently opened first.
fn matching_layouts<'a>(
    library: &'a LayoutLibrary,
    query: &str,
) -> Vec<&'a WorkspaceSavedLayoutState> {
    let query = query.trim().to_lowercase();
    library
        .recently_used(MAXIMUM_SAVED_LAYOUTS)
        .into_iter()
        .filter(|layout| query.is_empty() || layout.name.to_lowercase().contains(&query))
        .collect()
}

pub(super) fn unsaved_layout_dot(theme: &AerisTheme) -> Div {
    div()
        .flex_none()
        .size(px(UNSAVED_LAYOUT_DOT_SIZE))
        .rounded_full()
        .bg(gpui_color(theme.colors.primary))
}

impl TerminalApp {
    /// Shows the start page in the "New tab" placeholder, creating the placeholder when there
    /// is none. There is at most one.
    pub(super) fn open_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layouts_ui.layout_menu_open = false;
        self.layouts_ui.card_menu = None;
        self.show_terminal_view(cx);
        if self.layouts_ui.start_page.is_none() {
            let search =
                cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Search layouts"));
            let changes = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
            self.layouts_ui.start_page = Some(StartPage {
                shown: false,
                search,
                _search_changes: changes,
            });
        }
        let Some(page) = self.layouts_ui.start_page.as_mut() else {
            return;
        };
        let newly_shown = !page.shown;
        page.shown = true;
        let search = page.search.clone();
        search.update(cx, |search, search_cx| search.focus(window, search_cx));
        if newly_shown {
            self.set_workspace_resource_class(self.active, ConsumerResourceClass::Background, cx);
        }
        self.workspace_error = None;
        cx.notify();
    }

    /// Leaves the placeholder in the tab strip and shows the active workspace again.
    pub(super) fn hide_start_page(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self
            .layouts_ui
            .start_page
            .as_mut()
            .filter(|page| page.shown)
        else {
            return;
        };
        page.shown = false;
        self.layouts_ui.card_menu = None;
        self.set_workspace_resource_class(self.active, ConsumerResourceClass::Foreground, cx);
        cx.notify();
    }

    /// Removes the placeholder.
    pub(super) fn close_start_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.layouts_ui.start_page.take() else {
            return;
        };
        self.layouts_ui.card_menu = None;
        if page.shown {
            self.set_workspace_resource_class(self.active, ConsumerResourceClass::Foreground, cx);
            self.focus_workspace(window, cx);
        }
        cx.notify();
    }

    /// Replaces the placeholder with a new tab holding one chart and a new layout.
    pub(super) fn create_layout_from_start_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.layout_library.unique_name(UNTITLED_LAYOUT_NAME);
        match self.add_workspace(&name, window, cx) {
            Ok(()) => self.close_start_page(window, cx),
            Err(error) => {
                self.workspace_error = Some(error);
                cx.notify();
            }
        }
    }

    /// Replaces the placeholder with a new tab showing a saved layout. A layout another tab
    /// already shows opens again; both tabs then edit the same layout.
    pub(super) fn open_layout_from_start_page(
        &mut self,
        layout_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layouts_ui.card_menu = None;
        match self.open_saved_layout(layout_id, window, cx) {
            Ok(()) => self.close_start_page(window, cx),
            Err(error) => {
                self.workspace_error = Some(error);
                cx.notify();
            }
        }
    }

    fn open_layout_in_new_tab(
        &mut self,
        layout_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layouts_ui.layout_menu_open = false;
        if let Err(error) = self.open_saved_layout(layout_id, window, cx) {
            self.workspace_error = Some(error);
        }
        cx.notify();
    }

    fn copy_layout_from_start_page(&mut self, layout_id: u64, cx: &mut Context<Self>) {
        self.layouts_ui.card_menu = None;
        if let Err(error) = self.copy_saved_layout(layout_id, cx) {
            self.workspace_error = Some(error);
        }
        cx.notify();
    }

    pub(super) fn toggle_layout_menu(&mut self, cx: &mut Context<Self>) {
        self.layouts_ui.layout_menu_open = !self.layouts_ui.layout_menu_open;
        self.layouts_ui.card_menu = None;
        cx.notify();
    }

    fn toggle_layout_card_menu(&mut self, layout_id: u64, cx: &mut Context<Self>) {
        self.layouts_ui.card_menu =
            (self.layouts_ui.card_menu != Some(layout_id)).then_some(layout_id);
        self.layouts_ui.layout_menu_open = false;
        cx.notify();
    }

    pub(super) fn close_layout_menus(&mut self, cx: &mut Context<Self>) {
        self.layouts_ui.layout_menu_open = false;
        self.layouts_ui.card_menu = None;
        cx.notify();
    }

    /// Opens the name dialog, its field holding the layout's name (rename) or a free name.
    pub(super) fn open_layout_name_dialog(
        &mut self,
        purpose: LayoutNamePurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = match purpose {
            LayoutNamePurpose::Rename(layout_id) => self
                .layout_library
                .get(layout_id)
                .map(|layout| layout.name.clone()),
            LayoutNamePurpose::Copy => {
                Some(self.copy_layout_name(&self.workspaces[self.active].label))
            }
            LayoutNamePurpose::Create => {
                Some(self.layout_library.unique_name(UNTITLED_LAYOUT_NAME))
            }
        };
        let Some(name) = name else {
            self.workspace_error = Some("The layout no longer exists".to_string());
            cx.notify();
            return;
        };
        self.layouts_ui.layout_menu_open = false;
        self.layouts_ui.card_menu = None;
        let input = cx.new(|input_cx| {
            InputState::new(window, input_cx)
                .placeholder("Layout name")
                .default_value(name)
        });
        input.update(cx, |input, input_cx| {
            input.focus(window, input_cx);
            input.select_all(window, input_cx);
        });
        let input_events = cx.subscribe_in(
            &input,
            window,
            |terminal, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => terminal.confirm_layout_name(window, cx),
                InputEvent::Change => {
                    let name = input.read(cx).value();
                    let error = terminal.layout_name_problem(&name);
                    if let Some(dialog) = terminal.layouts_ui.name_dialog.as_mut() {
                        dialog.error = error;
                    }
                    cx.notify();
                }
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        self.layouts_ui.name_dialog = Some(LayoutNameDialog {
            purpose,
            input,
            error: None,
            _input_events: input_events,
        });
        cx.notify();
    }

    /// Why the open name dialog cannot use `name`, if it cannot.
    fn layout_name_problem(&self, name: &str) -> Option<String> {
        let purpose = self.layouts_ui.name_dialog.as_ref()?.purpose;
        match purpose {
            LayoutNamePurpose::Rename(layout_id) => {
                self.layout_library.check_rename(layout_id, name).err()
            }
            LayoutNamePurpose::Copy | LayoutNamePurpose::Create => {
                self.layout_library.check_new_layout(name).err()
            }
        }
    }

    /// Applies the name dialog's name, or shows why it cannot be used.
    pub(super) fn confirm_layout_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.layouts_ui.name_dialog.as_ref() else {
            return;
        };
        let purpose = dialog.purpose;
        let name = dialog.input.read(cx).value().to_string();
        let result = match purpose {
            LayoutNamePurpose::Rename(layout_id) => self.rename_layout(layout_id, &name, cx),
            LayoutNamePurpose::Copy => self.copy_active_layout(&name, cx).map(|_| ()),
            LayoutNamePurpose::Create => self.add_workspace(&name, window, cx),
        };
        match result {
            Ok(()) => {
                self.layouts_ui.name_dialog = None;
                self.focus_workspace(window, cx);
            }
            Err(error) => {
                if let Some(dialog) = self.layouts_ui.name_dialog.as_mut() {
                    dialog.error = Some(error);
                }
            }
        }
        cx.notify();
    }

    /// Asks to confirm deleting a layout, or says why it cannot be deleted now.
    pub(super) fn request_layout_delete(&mut self, layout_id: u64, cx: &mut Context<Self>) {
        self.layouts_ui.card_menu = None;
        match self
            .layout_library
            .check_delete(layout_id, &layout_tabs::open_layout_ids(&self.workspaces))
        {
            Ok(_) => {
                self.workspace_error = None;
                self.layouts_ui.delete_confirmation = Some(layout_id);
            }
            Err(error) => self.workspace_error = Some(error),
        }
        cx.notify();
    }

    pub(super) fn confirm_layout_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(layout_id) = self.layouts_ui.delete_confirmation.take() else {
            return;
        };
        if let Err(error) = self.delete_saved_layout(layout_id, cx) {
            self.workspace_error = Some(error);
        }
        self.focus_start_page_search(window, cx);
        cx.notify();
    }

    fn focus_start_page_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = self
            .layouts_ui
            .start_page
            .as_ref()
            .filter(|page| page.shown)
        {
            page.search
                .update(cx, |search, search_cx| search.focus(window, search_cx));
        } else {
            self.chrome_focus.focus(window, cx);
        }
    }

    /// Asks whether to save unsaved layouts before a tab or the window closes.
    pub(super) fn prompt_layout_close(
        &mut self,
        prompt: ClosePrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layouts_ui.layout_menu_open = false;
        self.layouts_ui.card_menu = None;
        self.layouts_ui.name_dialog = None;
        self.layouts_ui.delete_confirmation = None;
        self.layouts_ui.close_prompt = Some(prompt);
        self.chrome_focus.focus(window, cx);
        cx.notify();
    }

    /// Finishes the close the prompt asked about, saving every unsaved layout it covers
    /// first when `save`. The window close is claimed once, here.
    pub(super) fn finish_layout_close(
        &mut self,
        save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(prompt) = self.layouts_ui.close_prompt.take() else {
            return;
        };
        let indices = match prompt {
            ClosePrompt::Tab(tab_id) => self
                .workspaces
                .iter()
                .position(|workspace| workspace.id == tab_id)
                .into_iter()
                .collect::<Vec<_>>(),
            ClosePrompt::Window => (0..self.workspaces.len()).collect(),
        };
        if save {
            for index in indices {
                if let Err(error) = self.save_layout_tab(index, cx) {
                    self.workspace_error = Some(error);
                    cx.notify();
                    return;
                }
            }
        }
        match prompt {
            ClosePrompt::Tab(tab_id) => self.close_workspace_now(tab_id, window, cx),
            ClosePrompt::Window => {
                if self.claim_close(cx) {
                    window.remove_window();
                }
            }
        }
        cx.notify();
    }

    /// Closes the open layout dialog without acting.
    pub(super) fn cancel_layout_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.layouts_ui.name_dialog = None;
        self.layouts_ui.delete_confirmation = None;
        self.layouts_ui.close_prompt = None;
        if self.layouts_ui.start_page_shown() {
            self.focus_start_page_search(window, cx);
        } else {
            self.focus_workspace(window, cx);
        }
        cx.notify();
    }

    pub(super) fn save_active_layout_shortcut(
        &mut self,
        _: &SaveActiveLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.layouts_ui.start_page_shown() {
            return;
        }
        self.run_layout_command(CommandId::SaveLayout, window, cx);
    }

    /// Whether each open tab, in order, differs from its layout's saved version.
    pub(super) fn layout_unsaved_flags(&self, cx: &App) -> Vec<bool> {
        (0..self.workspaces.len())
            .map(|index| self.layout_tab_has_unsaved_changes(index, cx))
            .collect()
    }

    /// The start page in place of the active workspace, while the placeholder is shown.
    pub(super) fn rendered_start_page(
        &self,
        terminal: &Entity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        let page = self
            .layouts_ui
            .start_page
            .as_ref()
            .filter(|page| page.shown)?;
        let theme = self.theme;
        let colors = theme.colors;
        let query = page.search.read(cx).value();
        let layouts = matching_layouts(&self.layout_library, &query);
        let empty = (layouts.is_empty() && !query.trim().is_empty()).then(|| {
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_muted))
                .child(format!("No layouts match \u{201c}{}\u{201d}", query.trim()))
        });
        let cards = layouts
            .into_iter()
            .map(|layout| self.layout_card(terminal, layout, &theme))
            .collect::<Vec<_>>();
        let header = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .text_xl()
                    .font_weight(platform_font_weight(TypographyRole::Strong))
                    .text_color(gpui_color(colors.text_default))
                    .child("Layouts"),
            )
            .child(
                div()
                    .w(px(START_PAGE_SEARCH_WIDTH))
                    .h(px(START_PAGE_SEARCH_HEIGHT))
                    .flex()
                    .child(Input::new(&page.search).platform(&theme).flex_1()),
            );
        let error = self.workspace_error.clone().map(|error| {
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_danger))
                .child(error)
        });
        Some(
            div()
                .id("layouts_start_page")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .bg(gpui_color(colors.surface))
                .child(
                    div()
                        .mx_auto()
                        .w_full()
                        .max_w(px(START_PAGE_WIDTH))
                        .flex()
                        .flex_col()
                        .gap(px(START_PAGE_GAP))
                        .py(px(START_PAGE_PADDING_Y))
                        .px(px(START_PAGE_PADDING_X))
                        .child(header)
                        .children(error)
                        .child(Self::create_layout_card(terminal, &theme))
                        .children(cards)
                        .children(empty),
                )
                .into_any_element(),
        )
    }

    fn create_layout_card(terminal: &Entity<Self>, theme: &AerisTheme) -> AnyElement {
        let create = terminal.clone();
        layout_card_frame(("create_layout_card", 0), theme)
            .child(
                layout_card_body(
                    ("create_layout_card_body", 0),
                    HugeIcon::Add,
                    "Create new layout",
                    "A new tab with one chart".to_string(),
                    theme,
                )
                .on_click(move |_, window, cx| {
                    create.update(cx, |terminal, cx| {
                        terminal.create_layout_from_start_page(window, cx);
                    });
                }),
            )
            .into_any_element()
    }

    fn layout_card(
        &self,
        terminal: &Entity<Self>,
        layout: &WorkspaceSavedLayoutState,
        theme: &AerisTheme,
    ) -> AnyElement {
        let layout_id = layout.layout_id;
        let open = terminal.clone();
        let menu_open = self.layouts_ui.card_menu == Some(layout_id);
        let toggle = terminal.clone();
        let dismiss = terminal.clone();
        let trigger = Button::new(("layout_card_more", layout_id), theme)
            .variant(ButtonVariant::Ghost)
            .icon(header_icon(HugeIcon::More))
            .aria_label(format!("More actions for {}", layout.name))
            .open(menu_open)
            .on_click(move |_, _, cx| {
                toggle.update(cx, |terminal, cx| {
                    terminal.toggle_layout_card_menu(layout_id, cx);
                });
            });
        let menu = MenuAnchor::new(("layout_card_menu_anchor", layout_id), trigger)
            .menu(menu_open.then(|| layout_card_menu(terminal, layout_id, theme)))
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, TerminalApp::close_layout_menus);
            });
        layout_card_frame(("layout_card", layout_id), theme)
            .child(
                layout_card_body(
                    ("layout_card_body", layout_id),
                    HugeIcon::Template,
                    layout.name.clone(),
                    layout_card_detail(layout),
                    theme,
                )
                .on_click(move |_, window, cx| {
                    open.update(cx, |terminal, cx| {
                        terminal.open_layout_from_start_page(layout_id, window, cx);
                    });
                }),
            )
            .child(div().flex_none().pr_2().child(menu))
            .into_any_element()
    }

    /// The layout button of the chart header and, while open, its menu.
    pub(super) fn layout_menu_button(&self, terminal: &Entity<Self>, unsaved: bool) -> AnyElement {
        let theme = self.theme;
        let workspace = &self.workspaces[self.active];
        let open = self.layouts_ui.layout_menu_open;
        let toggle = terminal.clone();
        let dismiss = terminal.clone();
        let label = if unsaved {
            format!("Layout {}, unsaved changes", workspace.label)
        } else {
            format!("Layout {}", workspace.label)
        };
        let button = Button::new("layout_menu", &theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Lg)
            .label(workspace.label.clone())
            .children(unsaved.then(|| unsaved_layout_dot(&theme)))
            .caret(header_icon(HugeIcon::ChevronDown))
            .open(open)
            .aria_label(label)
            .on_click(move |_, _, cx| {
                toggle.update(cx, TerminalApp::toggle_layout_menu);
            });
        let menu = MenuAnchor::new("layout_menu_anchor", button)
            .menu(open.then(|| self.layout_menu(terminal, unsaved)))
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, TerminalApp::close_layout_menus);
            });
        chrome_tooltip("layout_menu", "Layouts", menu, &theme).into_any_element()
    }

    /// The header menu's rows for the most recently opened layouts other than the active one.
    fn recent_layout_rows(&self, terminal: &Entity<Self>, active_layout: u64) -> Vec<MenuRow> {
        self.layout_library
            .recently_used(RECENT_LAYOUT_LIMIT + 1)
            .into_iter()
            .filter(|layout| layout.layout_id != active_layout)
            .take(RECENT_LAYOUT_LIMIT)
            .map(|layout| {
                let layout_id = layout.layout_id;
                let terminal = terminal.clone();
                MenuRow::compact(
                    ("layout_menu_recent", layout_id),
                    layout.name.clone(),
                    &self.theme,
                )
                .on_click(move |_, window, cx| {
                    terminal.update(cx, |terminal, cx| {
                        terminal.open_layout_in_new_tab(layout_id, window, cx);
                    });
                })
            })
            .collect()
    }

    fn layout_menu(&self, terminal: &Entity<Self>, unsaved: bool) -> MenuPanel {
        let theme = self.theme;
        let active_layout = self.workspaces[self.active].saved_layout_id;
        let autosave = self
            .layout_library
            .get(active_layout)
            .is_some_and(|layout| layout.autosave);
        let action = |id: &'static str,
                      label: &'static str,
                      run: fn(&mut Self, &mut Window, &mut Context<Self>)| {
            let terminal = terminal.clone();
            MenuRow::compact(id, label, &theme).on_click(move |_, window, cx| {
                terminal.update(cx, |terminal, cx| run(terminal, window, cx));
            })
        };
        let recent = self.recent_layout_rows(terminal, active_layout);
        let mut menu = MenuPanel::new(
            "layout_menu_panel",
            MenuPlacement::Anchored {
                side: MenuSide::Below,
                align: MenuAlign::End,
            },
            &theme,
        )
        .width(px(LAYOUT_MENU_WIDTH))
        .animate_from(PopupAnimationOrigin::new(1.0, 0.0))
        .child(
            action("layout_menu_save", "Save layout", |terminal, window, cx| {
                terminal.layouts_ui.layout_menu_open = false;
                terminal.run_layout_command(CommandId::SaveLayout, window, cx);
            })
            .detail("Ctrl+S")
            .disabled(!unsaved),
        )
        .child(
            action(
                "layout_menu_autosave",
                "Autosave",
                |terminal, window, cx| {
                    terminal.run_layout_command(CommandId::ToggleLayoutAutosave, window, cx);
                },
            )
            .checked(autosave),
        )
        .separator()
        .child(action(
            "layout_menu_rename",
            "Rename\u{2026}",
            |terminal, window, cx| {
                let layout_id = terminal.workspaces[terminal.active].saved_layout_id;
                terminal.open_layout_name_dialog(LayoutNamePurpose::Rename(layout_id), window, cx);
            },
        ))
        .child(action(
            "layout_menu_copy",
            "Make a copy\u{2026}",
            |terminal, window, cx| {
                terminal.open_layout_name_dialog(LayoutNamePurpose::Copy, window, cx);
            },
        ))
        .child(action(
            "layout_menu_create",
            "Create new layout\u{2026}",
            |terminal, window, cx| {
                terminal.open_layout_name_dialog(LayoutNamePurpose::Create, window, cx);
            },
        ))
        .separator();
        if !recent.is_empty() {
            menu = menu
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(gpui_color(theme.colors.text_muted))
                        .child("Recently used"),
                )
                .children(recent)
                .separator();
        }
        menu.child(action(
            "layout_menu_open",
            "Open layout\u{2026}",
            |terminal, window, cx| {
                terminal.open_start_page(window, cx);
            },
        ))
    }

    /// The open layout dialog, if any.
    pub(super) fn layouts_dialog_layer(
        &self,
        terminal: &Entity<Self>,
        cx: &App,
    ) -> Option<AnyElement> {
        let theme = self.theme;
        if let Some(prompt) = self.layouts_ui.close_prompt {
            return Some(self.close_prompt_dialog(terminal, prompt, cx));
        }
        if let Some(layout_id) = self.layouts_ui.delete_confirmation {
            let name = self
                .layout_library
                .get(layout_id)
                .map_or_else(String::new, |layout| layout.name.clone());
            let cancel = terminal.clone();
            let confirm = terminal.clone();
            return Some(
                ConfirmationDialog::new(
                    "layout_delete_dialog",
                    "Delete layout?",
                    ConfirmationTone::Destructive,
                    &theme,
                    move |window, cx| {
                        cancel.update(cx, |terminal, cx| terminal.cancel_layout_modal(window, cx));
                    },
                    move |window, cx| {
                        confirm.update(cx, |terminal, cx| terminal.confirm_layout_delete(window, cx));
                    },
                )
                .confirm_label("Delete")
                .message(format!(
                    "Delete \u{201c}{name}\u{201d}? Its arrangement and drawings cannot be recovered."
                ))
                .into_any_element(),
            );
        }
        let dialog = self.layouts_ui.name_dialog.as_ref()?;
        let cancel = terminal.clone();
        let confirm = terminal.clone();
        Some(
            ConfirmationDialog::new(
                "layout_name_dialog",
                dialog.purpose.title(),
                ConfirmationTone::Positive,
                &theme,
                move |window, cx| {
                    cancel.update(cx, |terminal, cx| terminal.cancel_layout_modal(window, cx));
                },
                move |window, cx| {
                    confirm.update(cx, |terminal, cx| terminal.confirm_layout_name(window, cx));
                },
            )
            .confirm_label(dialog.purpose.confirm_label())
            .child(
                Input::new(&dialog.input)
                    .platform(&theme)
                    .invalid(dialog.error.is_some()),
            )
            .children(dialog.error.clone().map(|error| {
                div()
                    .text_xs()
                    .text_color(gpui_color(theme.colors.text_danger))
                    .child(error)
            }))
            .into_any_element(),
        )
    }

    fn close_prompt_dialog(
        &self,
        terminal: &Entity<Self>,
        prompt: ClosePrompt,
        cx: &App,
    ) -> AnyElement {
        let theme = self.theme;
        let names = match prompt {
            ClosePrompt::Tab(tab_id) => self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == tab_id)
                .map(|workspace| vec![workspace.label.clone()])
                .unwrap_or_default(),
            ClosePrompt::Window => self.unsaved_layout_names(cx),
        };
        let (title, message) = match names.as_slice() {
            [name] => (
                format!("Save changes to \u{201c}{name}\u{201d}?"),
                "Your layout has unsaved changes.".to_string(),
            ),
            names => (
                format!("Save changes to {} layouts?", names.len()),
                format!(
                    "Your layouts have unsaved changes: {}.",
                    names
                        .iter()
                        .map(|name| format!("\u{201c}{name}\u{201d}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
        };
        let dismiss = terminal.clone();
        let cancel = terminal.clone();
        let discard = terminal.clone();
        let save = terminal.clone();
        Dialog::new(
            "layout_close_prompt",
            DialogSize::Sm,
            &theme,
            move |window, cx| {
                dismiss.update(cx, |terminal, cx| terminal.cancel_layout_modal(window, cx));
            },
        )
        .title(title)
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(message),
        )
        .action(
            Button::new("layout_close_cancel", &theme)
                .variant(ButtonVariant::Outline)
                .label("Cancel")
                .on_press(move |_, window, cx| {
                    cancel.update(cx, |terminal, cx| terminal.cancel_layout_modal(window, cx));
                }),
        )
        .action(
            Button::new("layout_close_discard", &theme)
                .variant(ButtonVariant::Destructive)
                .label("Discard and close")
                .on_press(move |_, window, cx| {
                    discard.update(cx, |terminal, cx| {
                        terminal.finish_layout_close(false, window, cx);
                    });
                }),
        )
        .action(
            Button::new("layout_close_save", &theme)
                .variant(ButtonVariant::Primary)
                .label("Save and close")
                .on_press(move |_, window, cx| {
                    save.update(cx, |terminal, cx| {
                        terminal.finish_layout_close(true, window, cx);
                    });
                }),
        )
        .into_any_element()
    }
}

fn layout_card_frame(id: (&'static str, u64), theme: &AerisTheme) -> Stateful<Div> {
    let colors = theme.colors;
    div()
        .id(id)
        .w_full()
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
}

fn layout_card_body(
    id: (&'static str, u64),
    icon: HugeIcon,
    title: impl Into<SharedString>,
    detail: String,
    theme: &AerisTheme,
) -> Button {
    let colors = theme.colors;
    Button::new(id, theme)
        .variant(ButtonVariant::Ghost)
        .flex_1()
        .min_w_0()
        .h_auto()
        .justify_start()
        .gap_3()
        .px_4()
        .py_3()
        .child(
            div()
                .flex_none()
                .size(px(LAYOUT_CARD_ICON_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .bg(gpui_color(colors.surface))
                .child(header_icon(icon)),
        )
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_default))
                        .child(title.into()),
                )
                .children((!detail.is_empty()).then(|| {
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(gpui_color(colors.text_secondary))
                        .child(detail)
                })),
        )
}

fn layout_card_menu(
    terminal: &Entity<TerminalApp>,
    layout_id: u64,
    theme: &AerisTheme,
) -> MenuPanel {
    let open = terminal.clone();
    let rename = terminal.clone();
    let copy = terminal.clone();
    let delete = terminal.clone();
    MenuPanel::new(
        ("layout_card_menu", layout_id),
        MenuPlacement::Anchored {
            side: MenuSide::Below,
            align: MenuAlign::End,
        },
        theme,
    )
    .width(px(LAYOUT_CARD_MENU_WIDTH))
    .animate_from(PopupAnimationOrigin::new(1.0, 0.0))
    .child(
        MenuRow::compact(("layout_card_open", layout_id), "Open", theme).on_click(
            move |_, window, cx| {
                open.update(cx, |terminal, cx| {
                    terminal.open_layout_from_start_page(layout_id, window, cx);
                });
            },
        ),
    )
    .child(
        MenuRow::compact(("layout_card_rename", layout_id), "Rename\u{2026}", theme).on_click(
            move |_, window, cx| {
                rename.update(cx, |terminal, cx| {
                    terminal.open_layout_name_dialog(
                        LayoutNamePurpose::Rename(layout_id),
                        window,
                        cx,
                    );
                });
            },
        ),
    )
    .child(
        MenuRow::compact(("layout_card_copy", layout_id), "Make a copy", theme).on_click(
            move |_, _, cx| {
                copy.update(cx, |terminal, cx| {
                    terminal.copy_layout_from_start_page(layout_id, cx);
                });
            },
        ),
    )
    .separator()
    .child(
        MenuRow::compact(("layout_card_delete", layout_id), "Delete\u{2026}", theme)
            .destructive(true)
            .on_click(move |_, _, cx| {
                delete.update(cx, |terminal, cx| {
                    terminal.request_layout_delete(layout_id, cx);
                });
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_dates_read_as_calendar_days_and_unsaved_layouts_have_none() {
        assert_eq!(saved_date_label(0), None);
        assert_eq!(
            saved_date_label(1_767_571_200_000).as_deref(),
            Some("Saved Jan 5, 2026")
        );
    }

    fn one_chart() -> WorkspaceTabState {
        WorkspaceTabState {
            workspace_id: 7,
            panes: vec![WorkspacePaneState {
                pane_id: 3,
                consumer_id: 4,
                interval: "5m".to_string(),
                ..WorkspacePaneState::default()
            }],
            active_pane_id: 3,
            generation: 1,
            layout: Some(WorkspaceLayoutState {
                pane_id: 3,
                ..WorkspaceLayoutState::default()
            }),
            ..WorkspaceTabState::default()
        }
    }

    #[test]
    fn the_start_page_lists_matching_layouts_most_recently_opened_first() {
        let mut library = LayoutLibrary::default();
        let scalping = library
            .create("Scalping", &one_chart(), Vec::new(), 1)
            .expect("layout");
        let swing = library
            .create("Swing", &one_chart(), Vec::new(), 2)
            .expect("layout");
        let futures = library
            .create("Futures scalping", &one_chart(), Vec::new(), 3)
            .expect("layout");
        library.touch_opened(scalping, 4).expect("opened");
        let ids = |query: &str| {
            matching_layouts(&library, query)
                .into_iter()
                .map(|layout| layout.layout_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(""), [scalping, futures, swing]);
        assert_eq!(ids("  SCALP "), [scalping, futures], "search ignores case");
        assert_eq!(ids("day"), Vec::<u64>::new());
        let detail = layout_card_detail(library.get(swing).expect("layout"));
        assert!(detail.starts_with("No market 5m"), "{detail}");
    }
}
