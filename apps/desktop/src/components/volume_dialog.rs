use super::big_trades_dialog::{choice_tab, setting_row};
use super::*;

const PLACEMENT_CHOICES: [(&str, VolumePlacement); 2] = [
    ("Price pane", VolumePlacement::PricePane),
    ("Own pane", VolumePlacement::OwnPane),
];

const AVERAGE_KIND_CHOICES: [(&str, VolumeAverageKind); 2] = [
    ("SMA", VolumeAverageKind::Simple),
    ("EMA", VolumeAverageKind::Exponential),
];

/// A swatch that opens the color picker for one volume color. Picks change only the draft.
fn color_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    target: VolumeColorTarget,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let value = target.color(&dialog.draft);
    let open_picker = dialog
        .color_picker
        .as_ref()
        .filter(|picker| picker.target == target);
    let toggle = app.clone();
    let mut control = div().relative().flex().child(
        Button::new(("volume_color", target as usize), theme)
            .variant(ButtonVariant::Outline)
            .open(open_picker.is_some())
            .aria_label(target.label())
            .leading(
                div()
                    .size(design_rems(16.0))
                    .flex_none()
                    .rounded_full()
                    .border(platform_border_width(theme))
                    .border_color(gpui_color(colors.border_secondary))
                    .bg(transparency_backdrop(theme))
                    .child(
                        div()
                            .size_full()
                            .rounded_full()
                            .bg(chart_css_color(&value, colors.text_secondary)),
                    ),
            )
            .child(
                div()
                    .w(design_rems(62.0))
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(value.to_ascii_uppercase()),
            )
            .on_press(move |_, window, cx| {
                toggle.update(cx, |surface, surface_cx| {
                    surface.toggle_volume_dialog_color_picker(target, window, surface_cx);
                });
            }),
    );
    if let Some(picker) = open_picker {
        let select = app.clone();
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
            }),
        ));
    }
    control.into_any_element()
}

fn labeled_color(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    target: VolumeColorTarget,
    theme: &AerisTheme,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(target.label()),
        )
        .child(color_control(app, dialog, target, theme))
        .into_any_element()
}

fn columns_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    setting_row(
        "Columns",
        "Each column takes the color of the bar that traded it.",
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(labeled_color(app, dialog, VolumeColorTarget::Up, theme))
            .child(labeled_color(app, dialog, VolumeColorTarget::Down, theme))
            .into_any_element(),
        theme,
    )
}

fn placement_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let choices = TabList::new("volume_placement", "Placement", theme).children(
        PLACEMENT_CHOICES
            .into_iter()
            .enumerate()
            .map(|(index, (label, placement))| {
                let update = app.clone();
                choice_tab(
                    ("volume_placement", index),
                    label,
                    dialog.draft.placement == placement,
                    theme,
                )
                .on_click(move |_, _, cx| {
                    update.update(cx, |surface, surface_cx| {
                        surface.set_volume_dialog_placement(placement, surface_cx);
                    });
                })
            }),
    );
    let description = match dialog.draft.placement {
        VolumePlacement::PricePane => "Columns sit along the bottom of the price chart.",
        VolumePlacement::OwnPane => "Columns get a pane and scale of their own below the price.",
    };
    setting_row(
        "Placement",
        description,
        div().flex().child(choices).into_any_element(),
        theme,
    )
}

fn average_switch_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let update = app.clone();
    div()
        .px_3()
        .child(
            SwitchRow::new(
                "volume_average_enabled",
                "Moving average",
                dialog.draft.average.enabled,
                theme,
            )
            .description("Draw the average volume over the columns.")
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
    let choices = TabList::new("volume_average_kind", "Average type", theme).children(
        AVERAGE_KIND_CHOICES
            .into_iter()
            .enumerate()
            .map(|(index, (label, kind))| {
                let update = app.clone();
                choice_tab(
                    ("volume_average_kind", index),
                    label,
                    dialog.draft.average.kind == kind,
                    theme,
                )
                .on_click(move |_, _, cx| {
                    update.update(cx, |surface, surface_cx| {
                        surface.set_volume_dialog_average_kind(kind, surface_cx);
                    });
                })
            }),
    );
    let description = match dialog.draft.average.kind {
        VolumeAverageKind::Simple => "Simple: every bar in the length counts equally.",
        VolumeAverageKind::Exponential => "Exponential: recent bars count more.",
    };
    setting_row(
        "Average type",
        description,
        div().flex().child(choices).into_any_element(),
        theme,
    )
}

fn average_length_row(dialog: &VolumeDialogState, theme: &AerisTheme) -> AnyElement {
    setting_row(
        "Length",
        "Number of bars averaged, from 1 to 500.",
        div()
            .h(px(32.0))
            .child(Input::new(&dialog.average_length).platform(theme).flex_1())
            .into_any_element(),
        theme,
    )
}

fn average_color_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &VolumeDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    setting_row(
        "Average color",
        "Color of the moving-average line.",
        labeled_color(app, dialog, VolumeColorTarget::Average, theme),
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
    .title("Indicator settings")
    .subtitle("Volume")
    .max_height(px(680.0))
    .child(columns_row(app, dialog, theme))
    .child(placement_row(app, dialog, theme))
    .child(average_switch_row(app, dialog, theme))
    .when(dialog.draft.average.enabled, |body| {
        body.child(average_kind_row(app, dialog, theme))
            .child(average_length_row(dialog, theme))
            .child(average_color_row(app, dialog, theme))
    })
    .children(dialog.message.as_ref().map(|message| {
        div()
            .text_sm()
            .text_color(gpui_color(colors.danger))
            .child(message.clone())
    }))
    .footer_leading(
        Button::new("volume_reset", theme)
            .variant(ButtonVariant::Destructive)
            .label("Reset to defaults")
            .on_click(move |_, window, cx| {
                reset.update(cx, |surface, surface_cx| {
                    surface.reset_volume_dialog(window, surface_cx);
                });
            }),
    )
    .action(
        Button::new("volume_cancel", theme)
            .variant(ButtonVariant::Outline)
            .label("Cancel")
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_volume_dialog);
            }),
    )
    .action(
        Button::new("volume_apply", theme)
            .variant(ButtonVariant::Secondary)
            .label("Apply")
            .on_click(move |_, _, cx| {
                apply.update(cx, WorkspaceSurface::apply_volume_dialog);
            }),
    )
    .into_any_element()
}
