use super::big_trades_dialog::{DropdownChoice, SettingDropdown, setting_row as described_row};
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
    spec: &StudySettingSpec,
    selected: bool,
    enabled: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let label = spec.presentation.label.clone();
    let identifier = spec.identifier.clone();
    let update = app.clone();
    Switch::new(
        ("study_setting_boolean", control_id),
        label,
        selected,
        theme,
    )
    .disabled(!enabled)
    .on_change(move |checked, _, _, cx| {
        update.update(cx, |surface, surface_cx| {
            surface.set_study_setting_boolean(&identifier, checked, surface_cx);
        });
    })
    .into_any_element()
}

fn choice_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    control_id: usize,
    spec: &StudySettingSpec,
    options: &[aeris_market_runtime::study::StudySettingChoiceOption],
    enabled: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let current = match dialog.draft_values.get(&spec.identifier) {
        Some(StudySettingValue::Choice(value)) => Some(value.as_str()),
        _ => None,
    };
    // Study authors name settings freely, so the dropdown key uses the control index.
    let menu = format!("study_choice_{control_id}");
    let setting_identifier = spec.identifier.clone();
    SettingDropdown {
        menu: &menu,
        label: &spec.presentation.label,
        open_menu: dialog.menu.as_deref(),
        enabled,
        choices: options
            .iter()
            .map(|option| DropdownChoice {
                label: option.label.clone().into(),
                value: option.identifier.clone(),
                selected: current == Some(option.identifier.as_str()),
            })
            .collect(),
    }
    .render(
        app,
        move |surface, option: String, cx| {
            surface.select_study_setting_choice(&setting_identifier, &option, cx);
        },
        theme,
    )
}

fn text_control(
    input: Option<&Entity<InputState>>,
    enabled: bool,
    theme: &AerisTheme,
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
                .child(Input::new(input).platform(theme).flex_1())
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
    theme: &AerisTheme,
) -> AnyElement {
    match &spec.presentation.control {
        StudySettingControl::Boolean => boolean_control(
            app,
            control_id,
            spec,
            matches!(
                dialog.draft_values.get(&spec.identifier),
                Some(StudySettingValue::Boolean(true))
            ),
            enabled,
            theme,
        ),
        StudySettingControl::Choice { options } => {
            choice_control(app, dialog, control_id, spec, options, enabled, theme)
        }
        StudySettingControl::Integer { .. }
        | StudySettingControl::Decimal { .. }
        | StudySettingControl::Text => {
            text_control(dialog.inputs.get(&spec.identifier), enabled, theme)
        }
    }
}

/// Host-owned style row shown for every study, independent of its SDK settings.
fn line_thickness_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let choices = SettingDropdown {
        menu: "study_line_width",
        label: "Line thickness",
        open_menu: dialog.menu.as_deref(),
        enabled: true,
        choices: (1..=MAXIMUM_STUDY_LINE_WIDTH)
            .map(|width| DropdownChoice {
                label: format!("{width} px").into(),
                value: width,
                selected: dialog.line_width == width,
            })
            .collect(),
    }
    .render(app, WorkspaceSurface::set_study_settings_line_width, theme);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .bg(gpui_color(field_surface(theme)))
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
                        .child("Line thickness"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.text_muted))
                        .child("Stroke width of every line this study draws"),
                ),
        )
        .child(choices)
        .into_any_element()
}

/// A swatch that opens the color picker for one study line. Picks change only the draft.
fn output_color_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    draft: &StudyOutputColorDraft,
    theme: &AerisTheme,
) -> AnyElement {
    let output_index = draft.output_index;
    let value = draft.effective_color().to_string();
    let open_picker = dialog
        .color_picker
        .as_ref()
        .filter(|picker| picker.target == output_index);
    let toggle = app.clone();
    let mut control = div().relative().flex().child(
        color_swatch_button(
            ("study_output_color", output_index),
            draft.label.clone(),
            &value,
            open_picker.is_some(),
            theme,
        )
        .on_press(move |_, window, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_study_dialog_color_picker(output_index, window, surface_cx);
            });
        }),
    );
    if let Some(picker) = open_picker {
        let select = app.clone();
        let dismiss = app.clone();
        control = control.child(gpui::deferred(
            ColorPicker::new(
                ("study_output_color_popover", output_index),
                value,
                &picker.input,
                theme,
            )
            .error(picker.error.as_deref())
            .on_select(move |value, _, cx| {
                select.update(cx, |surface, surface_cx| {
                    surface.set_study_dialog_color(output_index, &value, surface_cx);
                });
            })
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, WorkspaceSurface::close_study_dialog_color_picker);
            }),
        ));
    }
    control.into_any_element()
}

/// Host-owned color row: one labeled swatch per line the study draws.
fn output_colors_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let swatches = div()
        .flex()
        .flex_col()
        .gap_2()
        .children(dialog.output_colors.iter().map(|draft| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(theme.colors.text_secondary))
                        .child(draft.label.clone()),
                )
                .child(output_color_control(app, dialog, draft, theme))
        }))
        .into_any_element();
    described_row(
        "Line colors",
        "Color of each line this study draws.",
        swatches,
        theme,
    )
}

fn setting_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    control_id: usize,
    spec: &StudySettingSpec,
    theme: &AerisTheme,
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
            .bg(gpui_color(field_surface(theme)))
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
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    // The dialog body owns padding and scrolling; this column only spaces the settings rows.
    let mut body = div().flex().flex_col().gap_2();
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
    body = body
        .child(
            div()
                .mt_1()
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_muted))
                .child("Style"),
        )
        .child(line_thickness_row(app, dialog, theme));
    if !dialog.output_colors.is_empty() {
        body = body.child(output_colors_row(app, dialog, theme));
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

pub(super) fn study_settings_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let dismiss = app.clone();
    let reset = app.clone();
    let cancel = app.clone();
    let save = app.clone();
    let busy = app
        .read(cx)
        .studies
        .reinitializing
        .contains_key(&dialog.study_id);

    Dialog::new(
        "study_settings_dialog",
        DialogSize::Md,
        theme,
        move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_study_settings_dialog);
        },
    )
    .title("Study settings")
    .subtitle(dialog.title.clone())
    .max_height(px(640.0))
    .child(study_settings_body(app, dialog, theme, cx))
    .footer_leading(
        Button::new("study_settings_reset", theme)
            .variant(ButtonVariant::Destructive)
            .label("Reset to defaults")
            .disabled(busy)
            .on_click(move |_, window, cx| {
                reset.update(cx, |surface, surface_cx| {
                    surface.reset_study_settings_dialog(window, surface_cx);
                });
            }),
    )
    .action(
        Button::new("study_settings_cancel", theme)
            .variant(ButtonVariant::Secondary)
            .label("Cancel")
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_study_settings_dialog);
            }),
    )
    .action(
        Button::new("study_settings_save", theme)
            .variant(ButtonVariant::Primary)
            .label(if busy { "Applying…" } else { "Apply" })
            .loading(busy)
            .disabled(busy)
            .on_click(move |_, _, cx| {
                save.update(cx, WorkspaceSurface::save_study_settings_dialog);
            }),
    )
    .into_any_element()
}
