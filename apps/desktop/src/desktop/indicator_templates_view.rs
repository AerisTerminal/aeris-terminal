//! The chart header's indicator template menu beside the Indicators button, and its save and
//! delete dialogs. The templates belong to the saved workspace; applying one replaces every
//! indicator of the active chart.

use super::chrome_menu::favorite_star_button;
use super::*;
use aeris_localization::{chart_menu, shell, tr, workspace};
use local_state::MAXIMUM_INDICATOR_TEMPLATES;

const INDICATOR_TEMPLATE_MENU_WIDTH: f32 = 260.0;
const INDICATOR_TEMPLATE_MENU_MAX_HEIGHT: f32 = 360.0;

/// Whether the template menu is open, and the one template dialog open at a time.
#[derive(Default)]
pub(super) struct IndicatorTemplatesUi {
    menu_open: bool,
    save_dialog: Option<IndicatorTemplateSaveDialog>,
    delete_confirmation: Option<usize>,
}

struct IndicatorTemplateSaveDialog {
    input: Entity<InputState>,
    error: Option<String>,
    _input_events: gpui::Subscription,
}

impl IndicatorTemplatesUi {
    pub(super) fn menu_open(&self) -> bool {
        self.menu_open
    }

    pub(super) fn modal_open(&self) -> bool {
        self.save_dialog.is_some() || self.delete_confirmation.is_some()
    }

    #[cfg(test)]
    pub(super) fn save_input(&self) -> Option<&Entity<InputState>> {
        self.save_dialog.as_ref().map(|dialog| &dialog.input)
    }

    #[cfg(test)]
    pub(super) fn save_error(&self) -> Option<&str> {
        self.save_dialog.as_ref()?.error.as_deref()
    }
}

/// Why the save dialog could not store a template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndicatorTemplateSaveError {
    InvalidName,
    NoChartData,
    LibraryFull,
}

impl IndicatorTemplateSaveError {
    fn message(self) -> String {
        match self {
            Self::InvalidName => tr!(shell::TEMPLATE_NAME_INVALID).to_string(),
            Self::NoChartData => tr!(workspace::CHART_DATA_UNAVAILABLE_YET).to_string(),
            Self::LibraryFull => tr!(
                chart_menu::INDICATOR_TEMPLATE_LIMIT,
                maximum = MAXIMUM_INDICATOR_TEMPLATES
            ),
        }
    }
}

/// The saved templates in menu order with their saved index: starred ones first, each group
/// in the order it was saved.
pub(super) fn indicator_templates_in_menu_order(
    templates: &[WorkspaceIndicatorTemplateState],
) -> Vec<(usize, &WorkspaceIndicatorTemplateState)> {
    let mut ordered = templates.iter().enumerate().collect::<Vec<_>>();
    ordered.sort_by_key(|(_, template)| !template.favorite);
    ordered
}

impl TerminalApp {
    pub(super) fn toggle_indicator_template_menu(&mut self, cx: &mut Context<Self>) {
        let ui = &mut self.pages.indicator_templates;
        ui.menu_open = !ui.menu_open;
        cx.notify();
    }

    pub(super) fn close_indicator_template_menu(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.pages.indicator_templates.menu_open) {
            cx.notify();
        }
    }

    pub(super) fn open_indicator_template_save_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pages.indicator_templates.menu_open = false;
        let input = cx.new(|input_cx| {
            InputState::new(window, input_cx).placeholder(tr!(shell::TEMPLATE_NAME_PLACEHOLDER))
        });
        input.update(cx, |input, input_cx| input.focus(window, input_cx));
        let input_events = cx.subscribe_in(
            &input,
            window,
            |terminal, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    terminal.save_indicator_template(window, cx);
                }
            },
        );
        self.pages.indicator_templates.save_dialog = Some(IndicatorTemplateSaveDialog {
            input,
            error: None,
            _input_events: input_events,
        });
        cx.notify();
    }

    /// Saves the active chart's indicators under the dialog's name, replacing the template of
    /// that name, or shows why it cannot.
    pub(super) fn save_indicator_template(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.pages.indicator_templates.save_dialog.as_ref() else {
            return;
        };
        let name = dialog.input.read(cx).value().trim().to_string();
        match self.store_indicator_template(name, cx) {
            Ok(()) => {
                self.pages.indicator_templates.save_dialog = None;
                self.record_layout_change(cx);
                self.focus_workspace(window, cx);
            }
            Err(error) => {
                if let Some(dialog) = self.pages.indicator_templates.save_dialog.as_mut() {
                    dialog.error = Some(error.message());
                }
            }
        }
        cx.notify();
    }

    fn store_indicator_template(
        &mut self,
        name: String,
        cx: &App,
    ) -> Result<(), IndicatorTemplateSaveError> {
        if !local_state::valid_template_name(&name) {
            return Err(IndicatorTemplateSaveError::InvalidName);
        }
        let template = self
            .active_surface()
            .read(cx)
            .indicator_template(name, cx)
            .ok_or(IndicatorTemplateSaveError::NoChartData)?;
        if !self.saved.store_indicator_template(template) {
            return Err(IndicatorTemplateSaveError::LibraryFull);
        }
        Ok(())
    }
    /// Replaces every indicator of the active chart with the template at `index`.
    pub(super) fn apply_indicator_template(&mut self, index: usize, cx: &mut Context<Self>) {
        self.pages.indicator_templates.menu_open = false;
        let Some(template) = self.saved.indicator_templates().get(index).cloned() else {
            return;
        };
        self.active_surface().update(cx, |surface, surface_cx| {
            surface.apply_indicator_template(&template, surface_cx);
        });
        cx.notify();
    }

    pub(super) fn toggle_indicator_template_favorite(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if self.saved.toggle_indicator_template_favorite(index) {
            self.record_layout_change(cx);
            cx.notify();
        }
    }

    pub(super) fn request_indicator_template_delete(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if index < self.saved.indicator_templates().len() {
            self.pages.indicator_templates.menu_open = false;
            self.pages.indicator_templates.delete_confirmation = Some(index);
            cx.notify();
        }
    }

    pub(super) fn delete_indicator_template(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.pages.indicator_templates.delete_confirmation.take() else {
            return;
        };
        if self.saved.remove_indicator_template(index) {
            self.record_layout_change(cx);
        }
        self.focus_workspace(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_indicator_template_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pages.indicator_templates.save_dialog = None;
        self.pages.indicator_templates.delete_confirmation = None;
        self.focus_workspace(window, cx);
        cx.notify();
    }

    /// The chevron beside the header's Indicators button and, while open, its template menu.
    pub(super) fn indicator_template_menu_button(
        &self,
        terminal: &Entity<Self>,
        enabled: bool,
    ) -> AnyElement {
        let theme = self.theme;
        let open = enabled && self.pages.indicator_templates.menu_open;
        let toggle = terminal.clone();
        let dismiss = terminal.clone();
        let button = Button::new("indicator_template_menu", &theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Lg)
            .w(design_rems(HEADER_CHEVRON_WIDTH))
            .icon(header_icon(HugeIcon::ChevronDown).with_size(px(HEADER_CHEVRON_ICON)))
            .aria_label(tr!(chart_menu::INDICATOR_TEMPLATES))
            .open(open)
            .disabled(!enabled)
            .on_click(move |_, _, cx| {
                toggle.update(cx, TerminalApp::toggle_indicator_template_menu);
            });
        let menu = MenuAnchor::new("indicator_template_menu_anchor", button)
            .menu(open.then(|| self.indicator_template_menu(terminal)))
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, TerminalApp::close_indicator_template_menu);
            });
        chrome_tooltip(
            "indicator_template_menu",
            tr!(chart_menu::INDICATOR_TEMPLATES),
            menu,
            &theme,
        )
    }

    fn indicator_template_menu(&self, terminal: &Entity<Self>) -> MenuPanel {
        let theme = self.theme;
        let save = terminal.clone();
        let rows = indicator_templates_in_menu_order(self.saved.indicator_templates())
            .into_iter()
            .map(|(index, template)| indicator_template_menu_row(terminal, index, template, &theme))
            .collect::<Vec<_>>();
        let empty = rows.is_empty().then(|| {
            MenuRow::compact(
                "indicator_template_none",
                tr!(chart_menu::NO_SAVED_INDICATOR_TEMPLATES),
                &theme,
            )
            .disabled(true)
        });
        MenuPanel::new(
            "indicator_template_menu_panel",
            MenuPlacement::Anchored {
                side: MenuSide::Below,
                align: MenuAlign::Start,
            },
            &theme,
        )
        .width(px(INDICATOR_TEMPLATE_MENU_WIDTH))
        .max_height(design_rems(INDICATOR_TEMPLATE_MENU_MAX_HEIGHT))
        .animate_from(PopupAnimationOrigin::new(0.0, 0.0))
        .child(
            MenuRow::compact(
                "indicator_template_save",
                tr!(chart_menu::SAVE_INDICATOR_TEMPLATE),
                &theme,
            )
            .on_click(move |_, window, cx| {
                save.update(cx, |terminal, cx| {
                    terminal.open_indicator_template_save_dialog(window, cx);
                });
            }),
        )
        .separator()
        .children(rows)
        .children(empty)
    }

    /// The open template dialog, if any.
    pub(super) fn indicator_templates_dialog_layer(
        &self,
        terminal: &Entity<Self>,
    ) -> Option<AnyElement> {
        let theme = self.theme;
        let cancel = terminal.clone();
        let confirm = terminal.clone();
        let ui = &self.pages.indicator_templates;
        if let Some(index) = ui.delete_confirmation {
            let name = self
                .saved
                .indicator_templates()
                .get(index)
                .map_or_else(String::new, |template| template.name.clone());
            return Some(
                ConfirmationDialog::new(
                    "indicator_template_delete_dialog",
                    tr!(chart_menu::DELETE_INDICATOR_TEMPLATE_TITLE),
                    ConfirmationTone::Destructive,
                    &theme,
                    move |window, cx| {
                        cancel.update(cx, |terminal, cx| {
                            terminal.cancel_indicator_template_dialog(window, cx);
                        });
                    },
                    move |window, cx| {
                        confirm.update(cx, |terminal, cx| {
                            terminal.delete_indicator_template(window, cx);
                        });
                    },
                )
                .confirm_label(tr!(chart_menu::DELETE))
                .message(tr!(
                    chart_menu::DELETE_INDICATOR_TEMPLATE_MESSAGE,
                    name = name
                ))
                .into_any_element(),
            );
        }
        let dialog = ui.save_dialog.as_ref()?;
        Some(
            ConfirmationDialog::new(
                "indicator_template_save_dialog",
                tr!(chart_menu::SAVE_INDICATOR_TEMPLATE_TITLE),
                ConfirmationTone::Positive,
                &theme,
                move |window, cx| {
                    cancel.update(cx, |terminal, cx| {
                        terminal.cancel_indicator_template_dialog(window, cx);
                    });
                },
                move |window, cx| {
                    confirm.update(cx, |terminal, cx| {
                        terminal.save_indicator_template(window, cx);
                    });
                },
            )
            .confirm_label(tr!(chart_menu::SAVE))
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
}

/// A row applies its template; its star keeps the template at the top and its trash button
/// asks to delete it.
fn indicator_template_menu_row(
    terminal: &Entity<TerminalApp>,
    index: usize,
    template: &WorkspaceIndicatorTemplateState,
    theme: &AerisTheme,
) -> MenuRow {
    let apply = terminal.clone();
    let star = terminal.clone();
    let delete = terminal.clone();
    MenuRow::compact(("indicator_template", index), template.name.clone(), theme)
        .trailing(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    favorite_star_button(
                        ("indicator_template_star", index),
                        template.favorite,
                        theme,
                    )
                    .on_click(move |_, _, cx| {
                        star.update(cx, |terminal, terminal_cx| {
                            terminal.toggle_indicator_template_favorite(index, terminal_cx);
                        });
                        cx.stop_propagation();
                    }),
                )
                .child(
                    Button::new(("indicator_template_delete", index), theme)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Sm)
                        .icon(header_icon(HugeIcon::Trash))
                        .aria_label(tr!(chart_menu::DELETE_TEMPLATE, name = template.name))
                        .danger_on_hover()
                        .tab_stop(false)
                        .on_click(move |_, _, cx| {
                            delete.update(cx, |terminal, terminal_cx| {
                                terminal.request_indicator_template_delete(index, terminal_cx);
                            });
                            cx.stop_propagation();
                        }),
                ),
        )
        .on_click(move |_, _, cx| {
            apply.update(cx, |terminal, terminal_cx| {
                terminal.apply_indicator_template(index, terminal_cx);
            });
        })
}
