use super::big_trades_dialog::{
    DropdownChoice, SettingDropdown, described_setting_row, setting_block, setting_input,
    settings_group, settings_groups,
};
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
                .w(design_rems(SETTINGS_CONTROL_WIDTH))
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
                .opacity(if enabled { 1.0 } else { 0.55 })
                .child(setting_input(input, theme))
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

fn line_thickness_control(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    SettingDropdown {
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
    .render(app, WorkspaceSurface::set_study_settings_line_width, theme)
}

/// A swatch that opens the color picker for one study line. Picks preview at once. A study
/// with several lines shows each line as a bare swatch named by its tooltip, so every swatch fits
/// on the thickness row; a single line shows its hex code.
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
    let id = ("study_output_color", output_index);
    let open = open_picker.is_some();
    let button = if dialog.output_colors.len() > 1 {
        color_dot_button(id, draft.label.clone(), &value, open, theme)
    } else {
        color_swatch_button(id, draft.label.clone(), &value, open, theme)
    };
    let mut control = div()
        .relative()
        .flex()
        .child(button.on_press(move |_, window, cx| {
            toggle.update(cx, |surface, surface_cx| {
                surface.toggle_study_dialog_color_picker(output_index, window, surface_cx);
            });
        }));
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

/// Host-owned style row shown for every study, independent of its SDK settings: the thickness
/// of every line and, beside it, one swatch per line the study draws.
fn lines_row(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let swatches = dialog
        .output_colors
        .iter()
        .map(|draft| output_color_control(app, dialog, draft, theme));
    setting_block(
        "Lines",
        "Thickness of every line and the color of each, in legend order.",
        std::iter::once(line_thickness_control(app, dialog, theme)).chain(swatches),
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
    let details = spec
        .presentation
        .description
        .iter()
        .map(|description| SharedString::from(description.clone()))
        .chain(constraint_hint(&spec.presentation.control).map(SharedString::from));
    Some(described_setting_row(
        spec.presentation.label.clone(),
        details,
        setting_control(app, dialog, control_id, spec, enabled, theme),
        theme,
    ))
}

fn study_settings_body(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudySettingsDialogState,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    // The dialog body owns padding and scrolling; this column only spaces the setting groups.
    let mut body = div().flex().flex_col().gap_3();
    // Consecutive settings that share a presentation group form one group; ungrouped settings
    // are the study's inputs.
    let mut groups: Vec<(SharedString, Vec<AnyElement>)> = Vec::new();
    let mut last_group: Option<Option<&str>> = None;
    for (index, spec) in dialog.specs.iter().enumerate() {
        let Some(row) = setting_row(app, dialog, index, spec, theme, cx) else {
            continue;
        };
        let group = spec.presentation.group.as_deref();
        match groups.last_mut() {
            Some((_, rows)) if last_group == Some(group) => rows.push(row),
            _ => {
                let title = group.map_or_else(
                    || SharedString::from("Inputs"),
                    |group| SharedString::from(group.to_string()),
                );
                groups.push((title, vec![row]));
            }
        }
        last_group = Some(group);
    }
    body = body.child(settings_groups(
        groups
            .into_iter()
            .map(|(title, rows)| settings_group(title, rows, theme))
            .chain(std::iter::once(settings_group(
                "Style",
                [lines_row(app, dialog, theme)],
                theme,
            ))),
        theme,
    ));
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
            .label("Apply")
            .on_click(move |_, _, cx| {
                save.update(cx, WorkspaceSurface::save_study_settings_dialog);
            }),
    )
    .into_any_element()
}
