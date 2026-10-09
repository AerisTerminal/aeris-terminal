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

fn setting_row(
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
        .px_3()
        .py_2()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_primary))
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

fn choice_tab(
    id: (&'static str, usize),
    label: &'static str,
    selected: bool,
    theme: &AerisTheme,
) -> Tab {
    Tab::new(id, theme).selected(selected).child(label)
}

fn filter_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let choices = TabList::new("big_trades_filter", "Filter", theme).children(
        FILTER_CHOICES
            .into_iter()
            .enumerate()
            .map(|(index, (label, intensity))| {
                let update = app.clone();
                choice_tab(
                    ("big_trades_filter", index),
                    label,
                    dialog.intensity == intensity,
                    theme,
                )
                .on_click(move |_, _, cx| {
                    update.update(cx, |surface, surface_cx| {
                        surface.set_big_trades_dialog_filter(intensity, surface_cx);
                    });
                })
            }),
    );
    let description = if dialog.intensity.is_some() {
        "Adapts to recent order sizes. Weak shows the most bubbles, Strong only the largest orders."
    } else {
        "Every order of at least the minimum volume."
    };
    setting_row(
        "Filter",
        description,
        div().flex().child(choices).into_any_element(),
        theme,
    )
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
    let choices = TabList::new("big_trades_size", "Bubble size", theme).children(
        SIZE_CHOICES
            .into_iter()
            .enumerate()
            .map(|(index, (label, size))| {
                let update = app.clone();
                choice_tab(
                    ("big_trades_size", index),
                    label,
                    dialog.size == size,
                    theme,
                )
                .on_click(move |_, _, cx| {
                    update.update(cx, |surface, surface_cx| {
                        surface.set_big_trades_dialog_size(size, surface_cx);
                    });
                })
            }),
    );
    setting_row(
        "Bubble size",
        "Bubbles grow with order volume within this range.",
        div().flex().child(choices).into_any_element(),
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
        .px_3()
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
    .max_height(px(640.0))
    .child(filter_row(app, dialog, theme))
    .when(dialog.intensity.is_none(), |body| {
        body.child(minimum_volume_row(dialog, theme))
    })
    .child(size_row(app, dialog, theme))
    .child(show_volume_row(app, dialog, theme))
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
            .variant(ButtonVariant::Secondary)
            .label("Cancel")
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_big_trades_dialog);
            }),
    )
    .action(
        Button::new("big_trades_apply", theme)
            .variant(ButtonVariant::Positive)
            .label("Apply")
            .on_click(move |_, _, cx| {
                apply.update(cx, WorkspaceSurface::apply_big_trades_dialog);
            }),
    )
    .into_any_element()
}
