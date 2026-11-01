use super::big_trades_dialog::{
    SettingDropdown, control_strip, dropdown_choices, setting_input, setting_row, settings_group,
    settings_groups,
};
use super::*;
use aeris_localization::{Text, common, panels, tr};

const PLACEMENT_CHOICES: [(Text, VolumePlacement); 2] = [
    (
        panels::VOLUME_PLACEMENT_PRICE_PANE,
        VolumePlacement::PricePane,
    ),
    (panels::VOLUME_PLACEMENT_OWN_PANE, VolumePlacement::OwnPane),
];

const AVERAGE_KIND_CHOICES: [(Text, VolumeAverageKind); 2] = [
    (panels::VOLUME_AVERAGE_SMA, VolumeAverageKind::Simple),
    (panels::VOLUME_AVERAGE_EMA, VolumeAverageKind::Exponential),
];

/// A swatch that opens the color picker for one volume color. Picks preview at once. A
/// `named` swatch shows the color's name instead of its hex code, for paired colors.
fn color_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    target: VolumeColorTarget,
    named: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let value = target.color(dialog);
    let open_picker = dialog
        .color_picker
        .as_ref()
        .filter(|picker| picker.target == target);
    let toggle = app.clone();
    let id = ("volume_color", target as usize);
    let button = if named {
        named_color_button(id, target.label(), &value, open_picker.is_some(), theme)
    } else {
        color_swatch_button(id, target.label(), &value, open_picker.is_some(), theme)
    };
    let mut control = div()
        .relative()
        .flex()
        .child(button.on_press(move |_, window, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_volume_dialog_color_picker(target, window, surface_cx);
            });
        }));
    if let Some(picker) = open_picker {
        let select = app.clone();
        let dismiss = app.clone();
        control = control.child(gpui::deferred(
            ColorPicker::new(
                ("volume_color_popover", target as usize),
                value,
                &picker.input,
                theme,
            )
            .error(picker.error.as_deref())
            .on_select(move |value, _, cx| {
                select.update(cx, |surface, surface_cx| {
                    surface.set_volume_dialog_color(target, &value, surface_cx);
                });
            })
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, WorkspaceSurface::close_volume_dialog_color_picker);
            }),
        ));
    }
    control.into_any_element()
}

fn columns_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    setting_row(
        tr!(panels::VOLUME_COLUMNS),
        tr!(panels::VOLUME_COLUMNS_DESCRIPTION),
        control_strip([
            color_control(app, dialog, VolumeColorTarget::Up, true, theme),
            color_control(app, dialog, VolumeColorTarget::Down, true, theme),
        ]),
        theme,
    )
}

fn placement_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let choices = SettingDropdown {
        menu: "volume_placement",
        label: tr!(panels::VOLUME_PLACEMENT),
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: dropdown_choices(&PLACEMENT_CHOICES, |placement| {
            dialog.draft.placement == placement
        }),
    }
    .render(app, WorkspaceSurface::set_volume_dialog_placement, theme);
    let description = match dialog.draft.placement {
        VolumePlacement::PricePane => tr!(panels::VOLUME_PLACEMENT_PRICE_DESCRIPTION),
        VolumePlacement::OwnPane => tr!(panels::VOLUME_PLACEMENT_OWN_DESCRIPTION),
    };
    setting_row(tr!(panels::VOLUME_PLACEMENT), description, choices, theme)
}

fn average_switch_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let update = app.clone();
    div()
        .py_1()
        .child(
            SwitchRow::new(
                "volume_average_enabled",
                tr!(panels::VOLUME_MOVING_AVERAGE),
                dialog.draft.average.enabled,
                theme,
            )
            .description(tr!(panels::VOLUME_MOVING_AVERAGE_DESCRIPTION))
            .on_change(move |enabled, _, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_volume_dialog_average_enabled(enabled, surface_cx);
                });
            }),
        )
        .into_any_element()
}

fn average_kind_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let choices = SettingDropdown {
        menu: "volume_average_kind",
        label: tr!(panels::VOLUME_AVERAGE_TYPE),
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: dropdown_choices(&AVERAGE_KIND_CHOICES, |kind| {
            dialog.draft.average.kind == kind
        }),
    }
    .render(app, WorkspaceSurface::set_volume_dialog_average_kind, theme);
    let description = match dialog.draft.average.kind {
        VolumeAverageKind::Simple => tr!(panels::VOLUME_AVERAGE_SMA_DESCRIPTION),
        VolumeAverageKind::Exponential => tr!(panels::VOLUME_AVERAGE_EMA_DESCRIPTION),
    };
    setting_row(
        tr!(panels::VOLUME_AVERAGE_TYPE),
        description,
        choices,
        theme,
    )
}

fn average_length_row(dialog: &VolumeDialogState, theme: &AerisTheme) -> AnyElement {
    setting_row(
        tr!(panels::VOLUME_LENGTH),
        tr!(panels::VOLUME_LENGTH_DESCRIPTION),
        setting_input(&dialog.average_length, theme),
        theme,
    )
}

fn average_color_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    setting_row(
        tr!(panels::VOLUME_AVERAGE_COLOR),
        tr!(panels::VOLUME_AVERAGE_COLOR_DESCRIPTION),
        color_control(app, dialog, VolumeColorTarget::Average, false, theme),
        theme,
    )
}

pub(super) fn volume_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let reset = app.clone();
    let cancel = app.clone();
    let apply = app.clone();
    Dialog::new("volume_dialog", DialogSize::Md, theme, move |_, cx| {
        dismiss.update(cx, WorkspaceSurface::close_volume_dialog);
    })
    .title(tr!(panels::INDICATOR_SETTINGS))
    .subtitle(tr!(panels::VOLUME))
    .max_height(px(680.0))
    .child(settings_groups(
        [
            settings_group(
                tr!(panels::VOLUME_COLUMNS),
                [
                    columns_row(app, dialog, theme),
                    placement_row(app, dialog, theme),
                ],
                theme,
            ),
            settings_group(
                tr!(panels::VOLUME_MOVING_AVERAGE),
                std::iter::once(average_switch_row(app, dialog, theme)).chain(
                    dialog
                        .draft
                        .average
                        .enabled
                        .then(|| {
                            [
                                average_kind_row(app, dialog, theme),
                                average_length_row(dialog, theme),
                                average_color_row(app, dialog, theme),
                            ]
                        })
                        .into_iter()
                        .flatten(),
                ),
                theme,
            ),
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
        Button::new("volume_reset", theme)
            .variant(ButtonVariant::Destructive)
            .label(tr!(panels::RESET_TO_DEFAULTS))
            .on_click(move |_, window, cx| {
                reset.update(cx, |surface, surface_cx| {
                    surface.reset_volume_dialog(window, surface_cx);
                });
            }),
    )
    .action(
        Button::new("volume_cancel", theme)
            .variant(ButtonVariant::Secondary)
            .label(tr!(common::CANCEL))
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_volume_dialog);
            }),
    )
    .action(
        Button::new("volume_apply", theme)
            .variant(ButtonVariant::Primary)
            .label(tr!(panels::APPLY))
            .on_click(move |_, _, cx| {
                apply.update(cx, WorkspaceSurface::apply_volume_dialog);
            }),
    )
    .into_any_element()
}
