use super::*;

const FILTER_CHOICES: [(&str, Option<BigTradesIntensity>); 4] = [
    ("Weak", Some(BigTradesIntensity::Weak)),
    ("Medium", Some(BigTradesIntensity::Medium)),
    ("Strong", Some(BigTradesIntensity::Strong)),
    ("Fixed", None),
];

const SIZE_CHOICES: [(&str, BigTradesSize); 3] = [
    ("Small", BigTradesSize::Small),
    ("Medium", BigTradesSize::Medium),
    ("Large", BigTradesSize::Large),
];

/// A titled group of indicator settings on one borderless `surface-secondary` card with no
/// lines between its rows, matching the chart settings panel's groups.
pub(super) fn settings_group(
    title: impl Into<SharedString>,
    rows: impl IntoIterator<Item = AnyElement>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_secondary))
                .child(title.into()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .px_3()
                .py_1()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .bg(gpui_color(colors.surface_secondary))
                .children(rows),
        )
        .into_any_element()
}

/// One setting inside a [`settings_group`] card: label and description above its control.
pub(super) fn setting_row(
    label: &'static str,
    description: &'static str,
    control: AnyElement,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .py_2()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_default))
                        .child(label),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(description),
                ),
        )
        .child(control)
        .into_any_element()
}

pub(super) struct DropdownChoice<T> {
    pub(super) label: SharedString,
    pub(super) value: T,
    pub(super) selected: bool,
}

/// A single-choice indicator setting as a dropdown. `menu` keys the dropdown within its dialog.
pub(super) struct SettingDropdown<'a, T> {
    pub(super) menu: &'a str,
    pub(super) label: &'a str,
    pub(super) open_menu: Option<&'a str>,
    pub(super) enabled: bool,
    pub(super) choices: Vec<DropdownChoice<T>>,
}

impl<T: Clone + 'static> SettingDropdown<'_, T> {
    /// A full-width secondary trigger showing the current choice and, while open, its menu.
    /// Picking a choice closes the dropdown before `choose` applies it.
    pub(super) fn render(
        self,
        app: &Entity<WorkspaceSurface>,
        choose: impl Fn(&mut WorkspaceSurface, T, &mut Context<WorkspaceSurface>) + Clone + 'static,
        theme: &AerisTheme,
    ) -> AnyElement {
        let open = self.enabled && self.open_menu == Some(self.menu);
        let current = self
            .choices
            .iter()
            .find(|choice| choice.selected)
            .map_or_else(SharedString::default, |choice| choice.label.clone());
        let toggle = app.clone();
        let toggle_menu = self.menu.to_string();
        let trigger = Button::new(
            SharedString::from(format!("indicator_setting_{}", self.menu)),
            theme,
        )
        .variant(ButtonVariant::Secondary)
        .button_size(ButtonSize::Lg)
        .trigger()
        .full_width()
        .open(open)
        .disabled(!self.enabled)
        .aria_label(self.label.to_string())
        .leading(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_default))
                .child(current),
        )
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_press(move |_, _, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_indicator_settings_menu(&toggle_menu, surface_cx);
            });
        });
        let menu = self.menu;
        let panel = open.then(|| {
            MenuPanel::new(
                SharedString::from(format!("indicator_setting_{menu}_menu")),
                MenuPlacement::Anchored {
                    side: MenuSide::Below,
                    align: MenuAlign::Stretch,
                },
                theme,
            )
            .animate_from(PopupAnimationOrigin::TOP_LEFT)
            .children(self.choices.into_iter().enumerate().map(|(index, choice)| {
                let pick = app.clone();
                let choose = choose.clone();
                let value = choice.value;
                MenuRow::compact(
                    SharedString::from(format!("indicator_setting_{menu}_{index}")),
                    choice.label,
                    theme,
                )
                .checked(choice.selected)
                .on_click(move |_, _, cx| {
                    pick.update(cx, |surface, surface_cx| {
                        surface.close_indicator_settings_menu(surface_cx);
                        choose(surface, value.clone(), surface_cx);
                    });
                })
            }))
        });
        let dismiss = app.clone();
        MenuAnchor::new(
            SharedString::from(format!("indicator_setting_{menu}_anchor")),
            trigger,
        )
        .full_width()
        .menu(panel)
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_indicator_settings_menu);
        })
        .into_any_element()
    }
}

pub(super) fn dropdown_choices<T: Copy>(
    choices: &[(&'static str, T)],
    selected: impl Fn(T) -> bool,
) -> Vec<DropdownChoice<T>> {
    choices
        .iter()
        .map(|&(label, value)| DropdownChoice {
            label: label.into(),
            value,
            selected: selected(value),
        })
        .collect()
}

fn filter_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let choices = SettingDropdown {
        menu: "big_trades_filter",
        label: "Filter",
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: dropdown_choices(&FILTER_CHOICES, |intensity| dialog.intensity == intensity),
    }
    .render(app, WorkspaceSurface::set_big_trades_dialog_filter, theme);
    let description = if dialog.intensity.is_some() {
        "Adapts to recent order sizes. Weak shows the most bubbles, Strong only the largest orders."
    } else {
        "Every order of at least the minimum volume."
    };
    setting_row("Filter", description, choices, theme)
}

fn minimum_volume_row(dialog: &BigTradesDialogState, theme: &AerisTheme) -> AnyElement {
    setting_row(
        "Minimum volume",
        "Total volume of one aggressive order, summed across its fills.",
        div()
            .h(px(32.0))
            .child(Input::new(&dialog.minimum_volume).platform(theme).flex_1())
            .into_any_element(),
        theme,
    )
}

fn size_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let choices = SettingDropdown {
        menu: "big_trades_size",
        label: "Bubble size",
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: dropdown_choices(&SIZE_CHOICES, |size| dialog.size == size),
    }
    .render(app, WorkspaceSurface::set_big_trades_dialog_size, theme);
    setting_row(
        "Bubble size",
        "Bubbles grow with order volume within this range.",
        choices,
        theme,
    )
}

fn show_volume_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let update = app.clone();
    div()
        .py_1()
        .child(
            SwitchRow::new(
                "big_trades_show_volume",
                "Show volume",
                dialog.show_volume,
                theme,
            )
            .description("Print the order volume inside bubbles large enough to hold it.")
            .on_change(move |show_volume, _, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_big_trades_dialog_show_volume(show_volume, surface_cx);
                });
            }),
        )
        .into_any_element()
}

pub(super) fn big_trades_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let reset = app.clone();
    let cancel = app.clone();
    let apply = app.clone();
    Dialog::new("big_trades_dialog", DialogSize::Md, theme, move |_, cx| {
        dismiss.update(cx, WorkspaceSurface::close_big_trades_dialog);
    })
    .title("Indicator settings")
    .subtitle("Big Trades")
    .panel_surface()
    .max_height(px(640.0))
    .child(settings_group(
        "Filter",
        std::iter::once(filter_row(app, dialog, theme)).chain(
            dialog
                .intensity
                .is_none()
                .then(|| minimum_volume_row(dialog, theme)),
        ),
        theme,
    ))
    .child(settings_group(
        "Bubbles",
        [
            size_row(app, dialog, theme),
            show_volume_row(app, dialog, theme),
        ],
        theme,
    ))
    .children(dialog.message.as_ref().map(|message| {
        div()
            .text_sm()
            .text_color(gpui_color(colors.danger))
            .child(message.clone())
    }))
    .footer_leading(
        Button::new("big_trades_reset", theme)
            .variant(ButtonVariant::Destructive)
            .label("Reset to defaults")
            .on_click(move |_, window, cx| {
                reset.update(cx, |surface, surface_cx| {
                    surface.reset_big_trades_dialog(window, surface_cx);
                });
            }),
    )
    .action(
        Button::new("big_trades_cancel", theme)
            .variant(ButtonVariant::Outline)
            .label("Cancel")
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_big_trades_dialog);
            }),
    )
    .action(
        Button::new("big_trades_apply", theme)
            .variant(ButtonVariant::Primary)
            .label("Apply")
            .on_click(move |_, _, cx| {
                apply.update(cx, WorkspaceSurface::apply_big_trades_dialog);
            }),
    )
    .into_any_element()
}
