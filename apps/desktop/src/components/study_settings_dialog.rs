use super::*;

fn constraint_hint(control: &StudySettingControl) -> Option<String> {
    match control {
        StudySettingControl::Integer {
            minimum,
            maximum,
            step,
        } => {
            let mut parts = Vec::new();
            if let Some(value) = minimum {
                parts.push(format!("Min {value}"));
            }
            if let Some(value) = maximum {
                parts.push(format!("Max {value}"));
            }
            if let Some(value) = step {
                parts.push(format!("Step {value}"));
            }
            (!parts.is_empty()).then(|| parts.join(" · "))
        }
        StudySettingControl::Decimal {
            minimum,
            maximum,
            step,
        } => {
            let mut parts = Vec::new();
            if let Some(value) = minimum {
                parts.push(format!(
                    "Min {}",
                    workspace_surface::study_decimal_text(*value)
                ));
            }
            if let Some(value) = maximum {
                parts.push(format!(
                    "Max {}",
                    workspace_surface::study_decimal_text(*value)
                ));
            }
            if let Some(value) = step {
                parts.push(format!(
                    "Step {}",
                    workspace_surface::study_decimal_text(*value)
                ));
            }
            (!parts.is_empty()).then(|| parts.join(" · "))
        }
        StudySettingControl::Boolean
        | StudySettingControl::Text
        | StudySettingControl::Choice { .. } => None,
    }
}

fn boolean_control(
    app: &Entity<WorkspaceSurface>,
    control_id: usize,
    identifier: &str,
    selected: bool,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let identifier = identifier.to_string();
    let update = app.clone();
    Button::new(("study_setting_boolean", control_id))
        .theme(theme)
        .resting_fill(theme.colors.surface_secondary)
        .selected(selected)
        .disabled(!enabled)
        .label(if selected { "On" } else { "Off" })
        .on_click(move |_, _, cx| {
            update.update(cx, |surface, surface_cx| {
                surface.set_study_setting_boolean(&identifier, !selected, surface_cx);
            });
        })
        .into_any_element()
}

fn choice_control(
    app: &Entity<WorkspaceSurface>,
    control_id: usize,
    identifier: &str,
    current: Option<&str>,
    options: &[axiusflow_market_runtime::study::StudySettingChoiceOption],
    enabled: bool,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let mut row = div().flex().flex_wrap().gap_2();
    for (index, option) in options.iter().enumerate() {
        let update = app.clone();
        let setting_identifier = identifier.to_string();
        let option_identifier = option.identifier.clone();
        row = row.child(
            Button::new((
                "study_setting_choice",
                control_id.saturating_mul(65).saturating_add(index),
            ))
            .theme(theme)
            .resting_fill(theme.colors.surface_secondary)
            .selected(current == Some(option.identifier.as_str()))
            .disabled(!enabled)
            .label(option.label.clone())
            .on_click(move |_, _, cx| {
                update.update(cx, |surface, surface_cx| {
                    surface.select_study_setting_choice(
                        &setting_identifier,
                        &option_identifier,
                        surface_cx,
                    );
                });
            }),
        );
    }
    row.into_any_element()
}

fn text_control(
    input: Option<&Entity<InputState>>,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    input.map_or_else(
        || {
            div()
                .h(px(32.0))
                .flex()
                .items_center()
                .px_2()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border))
                .text_sm()
                .text_color(gpui_color(colors.danger))
                .child("Editor unavailable")
                .into_any_element()
        },
        |input| {
            div()
                .h(px(32.0))
                .opacity(if enabled { 1.0 } else { 0.55 })
                .child(
                    Input::new(input)
                        .fill(gpui_color(colors.surface_secondary))
                        .border_color(gpui_color(colors.border))
                        .focus_border_color(gpui_color(colors.ring))
                        .flex_1(),
                )
                .into_any_element()
        },
    )
}

fn setting_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    control_id: usize,
    spec: &StudySettingSpec,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> AnyElement {
    match &spec.presentation.control {
        StudySettingControl::Boolean => boolean_control(
            app,
            control_id,
            &spec.identifier,
            matches!(
                dialog.draft_values.get(&spec.identifier),
                Some(StudySettingValue::Boolean(true))
            ),
            enabled,
            theme,
        ),
        StudySettingControl::Choice { options } => {
            let current = match dialog.draft_values.get(&spec.identifier) {
                Some(StudySettingValue::Choice(value)) => Some(value.as_str()),
                _ => None,
            };
            choice_control(
                app,
                control_id,
                &spec.identifier,
                current,
                options,
                enabled,
                theme,
            )
        }
        StudySettingControl::Integer { .. }
        | StudySettingControl::Decimal { .. }
        | StudySettingControl::Text => {
            text_control(dialog.inputs.get(&spec.identifier), enabled, theme)
        }
    }
}

fn setting_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    control_id: usize,
    spec: &StudySettingSpec,
    theme: &AxiusflowTheme,
    cx: &App,
) -> Option<AnyElement> {
    if !workspace_surface::study_setting_condition_matches(
        dialog,
        spec.presentation.visible_when.as_ref(),
        cx,
    ) {
        return None;
    }
    let enabled = workspace_surface::study_setting_condition_matches(
        dialog,
        spec.presentation.enabled_when.as_ref(),
        cx,
    );
    let colors = theme.colors;
    let hint = constraint_hint(&spec.presentation.control);
    Some(
        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
            .bg(gpui_color(colors.surface))
            .child(
                div().flex().items_start().justify_between().gap_3().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(platform_font_weight(TypographyRole::Strong))
                                .text_color(gpui_color(colors.text_primary))
                                .child(spec.presentation.label.clone()),
                        )
                        .children(spec.presentation.description.as_ref().map(|description| {
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(description.clone())
                        }))
                        .children(hint.map(|hint| {
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(hint)
                        })),
                ),
            )
            .child(setting_control(
                app, dialog, control_id, spec, enabled, theme,
            ))
            .into_any_element(),
    )
}

fn study_settings_body(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AxiusflowTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    let mut body = div()
        .id("study_settings_dialog_body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2()
        .p_3();
    let mut last_group: Option<&str> = None;
    for (index, spec) in dialog.specs.iter().enumerate() {
        if !workspace_surface::study_setting_condition_matches(
            dialog,
            spec.presentation.visible_when.as_ref(),
            cx,
        ) {
            continue;
        }
        let group = spec.presentation.group.as_deref();
        if group != last_group {
            if let Some(group) = group {
                body = body.child(
                    div()
                        .mt_1()
                        .text_xs()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_muted))
                        .child(group.to_string()),
                );
            }
            last_group = group;
        }
        if let Some(row) = setting_row(app, dialog, index, spec, theme, cx) {
            body = body.child(row);
        }
    }
    if let Some(message) = &dialog.message {
        body = body.child(
            div()
                .text_sm()
                .text_color(gpui_color(colors.danger))
                .child(message.clone()),
        );
    }
    body.into_any_element()
}

fn study_settings_header(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AxiusflowTheme,
) -> Div {
    let colors = theme.colors;
    let cancel = app.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child("Study settings"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child(dialog.title.clone()),
                ),
        )
        .child(chrome_close_button(
            "study_settings_close",
            theme,
            move |_, cx| {
                cancel.update(cx, WorkspaceSurface::close_study_settings_dialog);
            },
        ))
}

fn study_settings_footer(
    app: &Entity<WorkspaceSurface>,
    busy: bool,
    theme: &AxiusflowTheme,
) -> Div {
    let colors = theme.colors;
    let reset = app.clone();
    let cancel = app.clone();
    let save = app.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .p_4()
        .border_t_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            Button::new("study_settings_reset")
                .dialog_secondary(theme)
                .label("Reset to defaults")
                .disabled(busy)
                .on_click(move |_, window, cx| {
                    reset.update(cx, |surface, surface_cx| {
                        surface.reset_study_settings_dialog(window, surface_cx);
                    });
                }),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("study_settings_cancel")
                        .dialog_secondary(theme)
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, WorkspaceSurface::close_study_settings_dialog);
                        }),
                )
                .child(
                    Button::new("study_settings_save")
                        .dialog_primary(theme)
                        .label(if busy { "Applying…" } else { "Apply" })
                        .loading(busy)
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            save.update(cx, WorkspaceSurface::save_study_settings_dialog);
                        }),
                ),
        )
}

pub(super) fn study_settings_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AxiusflowTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let busy = app
        .read(cx)
        .studies
        .reinitializing
        .contains_key(&dialog.study_id);
    let body = study_settings_body(app, dialog, theme, cx);
    let header = study_settings_header(app, dialog, theme);
    let footer = study_settings_footer(app, busy, theme);

    div()
        .id("study_settings_dialog_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_color(colors.surface.with_alpha(0.72)))
        .on_any_mouse_down(move |_, _, cx| {
            dismiss.update(cx, WorkspaceSurface::close_study_settings_dialog);
            cx.stop_propagation();
        })
        .child(
            div()
                .id("study_settings_dialog")
                .w(px(500.0))
                .max_h(px(640.0))
                .flex()
                .flex_col()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border_secondary))
                .bg(gpui_color(colors.surface))
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .child(header)
                .child(body)
                .child(footer),
        )
        .into_any_element()
}
