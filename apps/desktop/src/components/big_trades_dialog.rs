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

fn choice_button(
    id: (&'static str, usize),
    label: &'static str,
    selected: bool,
    theme: &AerisTheme,
) -> Button {
    Button::new(id, theme)
        .resting_fill(theme.colors.surface_secondary)
        .selected(selected)
        .label(label)
}

fn filter_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let mut choices = div().flex().flex_wrap().gap_2();
    for (index, (label, intensity)) in FILTER_CHOICES.into_iter().enumerate() {
        let update = app.clone();
        choices = choices.child(
            choice_button(
                ("big_trades_filter", index),
                label,
                dialog.intensity == intensity,
                theme,
            )
            .on_click(move |_, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_big_trades_dialog_filter(intensity, surface_cx);
                });
            }),
        );
    }
    let description = if dialog.intensity.is_some() {
        "Adapts to recent order sizes. Weak shows the most bubbles, Strong only the largest orders."
    } else {
        "Every order of at least the minimum volume."
    };
    setting_row("Filter", description, choices.into_any_element(), theme)
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
    let mut choices = div().flex().flex_wrap().gap_2();
    for (index, (label, size)) in SIZE_CHOICES.into_iter().enumerate() {
        let update = app.clone();
        choices = choices.child(
            choice_button(
                ("big_trades_size", index),
                label,
                dialog.size == size,
                theme,
            )
            .on_click(move |_, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.set_big_trades_dialog_size(size, surface_cx);
                });
            }),
        );
    }
    setting_row(
        "Bubble size",
        "Bubbles grow with order volume within this range.",
        choices.into_any_element(),
        theme,
    )
}

fn show_volume_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let update = app.clone();
    let show_volume = dialog.show_volume;
    setting_row(
        "Show volume",
        "Print the order volume inside bubbles large enough to hold it.",
        choice_button(
            ("big_trades_show_volume", 0),
            if show_volume { "On" } else { "Off" },
            show_volume,
            theme,
        )
        .on_click(move |_, _, cx| {
            update.update(cx, |surface, surface_cx| {
                surface.set_big_trades_dialog_show_volume(!show_volume, surface_cx);
            });
        })
        .into_any_element(),
        theme,
    )
}

fn big_trades_body(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .id("big_trades_dialog_body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
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
        .into_any_element()
}

fn big_trades_footer(app: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> Div {
    let reset = app.clone();
    let cancel = app.clone();
    let apply = app.clone();
    modal_footer(theme)
        .justify_between()
        .child(
            Button::new("big_trades_reset", theme)
                .variant(ButtonVariant::Destructive)
                .label("Reset to defaults")
                .on_click(move |_, window, cx| {
                    reset.update(cx, |surface, surface_cx| {
                        surface.reset_big_trades_dialog(window, surface_cx);
                    });
                }),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("big_trades_cancel", theme)
                        .variant(ButtonVariant::Secondary)
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, WorkspaceSurface::close_big_trades_dialog);
                        }),
                )
                .child(
                    Button::new("big_trades_apply", theme)
                        .variant(ButtonVariant::Positive)
                        .label("Apply")
                        .on_click(move |_, _, cx| {
                            apply.update(cx, WorkspaceSurface::apply_big_trades_dialog);
                        }),
                ),
        )
}

pub(super) fn big_trades_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &BigTradesDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let dismiss = app.clone();
    let close = app.clone();
    let header = modal_header(
        "big_trades_close",
        "Indicator settings",
        Some("Big Trades".into_any_element()),
        theme,
        move |_, cx| {
            close.update(cx, WorkspaceSurface::close_big_trades_dialog);
        },
    );
    ModalLayer::new("big_trades_dialog", px(460.0), theme, move |_, cx| {
        dismiss.update(cx, WorkspaceSurface::close_big_trades_dialog);
    })
    .max_height(px(640.0))
    .radius(RadiusToken::Medium)
    .child(header)
    .child(big_trades_body(app, dialog, theme))
    .child(big_trades_footer(app, theme))
    .into_any_element()
}
