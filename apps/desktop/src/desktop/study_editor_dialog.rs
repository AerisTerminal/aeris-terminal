use super::native_ui::input::{TextArea, TextareaState};
use super::study_editor_store::{
    StudyProjectStore, source_sha256_hex, validate_study_source_static,
};
use super::*;
use aeris_localization::{common, panels, tr};
use aeris_study_sdk::project::{
    StudyDiagnostic, StudyDiagnosticSeverity, StudyProjectLibrary, StudyProjectTemplate,
};
use std::path::PathBuf;

pub(super) struct StudyEditorDialogState {
    pub(super) store: StudyProjectStore,
    pub(super) library: StudyProjectLibrary,
    pub(super) selected_id: Option<String>,
    pub(super) new_id_input: Entity<InputState>,
    pub(super) source_input: Entity<TextareaState>,
    pub(super) diagnostics: Vec<StudyDiagnostic>,
    pub(super) status_message: Option<String>,
}

const TEMPLATES: [(StudyProjectTemplate, &str); 5] = [
    (StudyProjectTemplate::StatelessOverlay, "Overlay"),
    (StudyProjectTemplate::StatefulRecursive, "Recursive"),
    (StudyProjectTemplate::MultiTimeframe, "MTF"),
    (StudyProjectTemplate::DrawingTableDashboard, "Scene"),
    (StudyProjectTemplate::AlertStudy, "Alerts"),
];

impl StudyEditorDialogState {
    pub(super) fn open(
        data_root: PathBuf,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) -> Self {
        let store = StudyProjectStore::new(&data_root);
        let (library, warnings) = store.load_library();
        let selected_id = library
            .iter()
            .next()
            .map(|project| project.manifest.identifier.clone());
        let initial_source = selected_id
            .as_deref()
            .and_then(|id| library.get(id))
            .map(|project| project.source.clone())
            .unwrap_or_default();
        let new_id_input =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("user.my_study"));
        let source_input = cx.new(|input_cx| {
            TextareaState::new(window, input_cx).placeholder("// Rust study source")
        });
        if !initial_source.is_empty() {
            source_input.update(cx, |input, input_cx| {
                input.set_value(initial_source, window, input_cx);
            });
        }
        let status_message = (!warnings.is_empty()).then(|| warnings.join(" · "));
        Self {
            store,
            library,
            selected_id,
            new_id_input,
            source_input,
            diagnostics: Vec::new(),
            status_message,
        }
    }

    pub(super) fn select_project(
        &mut self,
        identifier: String,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let Some(project) = self.library.get(&identifier) else {
            return;
        };
        let source = project.source.clone();
        self.diagnostics = project.diagnostics.clone();
        self.selected_id = Some(identifier);
        self.status_message = None;
        self.source_input.update(cx, |input, input_cx| {
            input.set_value(source, window, input_cx);
        });
    }

    pub(super) fn create_from_template(
        &mut self,
        template: StudyProjectTemplate,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let raw_id = self.new_id_input.read(cx).value().trim().to_string();
        let identifier = if raw_id.is_empty() {
            format!("user.study_{}", self.library.len() + 1)
        } else {
            raw_id
        };
        let display_name = identifier
            .rsplit('.')
            .next()
            .unwrap_or(&identifier)
            .replace('_', " ");
        match self
            .library
            .create_from_template(&identifier, &display_name, template)
        {
            Ok(project) => {
                let save_result = self.store.save_project(project);
                let source = project.source.clone();
                self.selected_id = Some(identifier.clone());
                self.diagnostics.clear();
                self.status_message = match save_result {
                    Ok(()) => Some(format!("Created `{identifier}`")),
                    Err(error) => Some(error),
                };
                self.new_id_input.update(cx, |input, input_cx| {
                    input.set_value(String::new(), window, input_cx);
                });
                self.source_input.update(cx, |input, input_cx| {
                    input.set_value(source, window, input_cx);
                });
            }
            Err(error) => {
                self.status_message = Some(format!("Create failed: {error:?}"));
            }
        }
    }

    pub(super) fn validate_and_save(&mut self, cx: &mut Context<WorkspaceSurface>) {
        let Some(identifier) = self.selected_id.clone() else {
            return;
        };
        let source = self.source_input.read(cx).value().to_string();
        if let Err(error) = self.library.update_source(&identifier, source.clone()) {
            self.status_message = Some(format!("Update failed: {error:?}"));
            return;
        }
        let diagnostics = validate_study_source_static(&source);
        let has_errors = diagnostics
            .iter()
            .any(|diag| diag.severity == StudyDiagnosticSeverity::Error);
        self.diagnostics = diagnostics.clone();
        if has_errors {
            let _ = self.library.record_build_failure(&identifier, diagnostics);
            if let Some(project) = self.library.get(&identifier) {
                let _ = self.store.save_project(project);
            }
            self.status_message = Some(format!(
                "Validation failed ({} diagnostic(s)); last-good artifact preserved",
                self.diagnostics.len()
            ));
        } else {
            let sha = source_sha256_hex(&source);
            let _ = self
                .library
                .record_build_success(&identifier, sha.clone(), sha.clone());
            self.status_message = match self
                .library
                .get(&identifier)
                .map(|project| self.store.save_project(project))
            {
                Some(Ok(())) => Some(format!("Validated & saved ({})", &sha[..12])),
                Some(Err(error)) => Some(error),
                None => None,
            };
        }
    }

    pub(super) fn revert_to_last_good(
        &mut self,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let Some(identifier) = self.selected_id.clone() else {
            return;
        };
        match self.library.revert_to_last_good(&identifier) {
            Ok(()) => {
                let restored = self
                    .library
                    .get(&identifier)
                    .map(|project| project.source.clone())
                    .unwrap_or_default();
                if let Some(project) = self.library.get(&identifier) {
                    let _ = self.store.save_project(project);
                }
                self.diagnostics.clear();
                self.status_message = Some("Reverted to last known good source".to_string());
                self.source_input.update(cx, |input, input_cx| {
                    input.set_value(restored, window, input_cx);
                });
            }
            Err(error) => {
                self.status_message = Some(format!("Revert unavailable: {error:?}"));
            }
        }
    }

    pub(super) fn delete_selected(
        &mut self,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let Some(identifier) = self.selected_id.clone() else {
            return;
        };
        let _ = self.library.delete(&identifier);
        let _ = self.store.delete_project(&identifier);
        let next_id = self
            .library
            .iter()
            .next()
            .map(|project| project.manifest.identifier.clone());
        self.selected_id = next_id.clone();
        self.diagnostics.clear();
        self.status_message = Some(format!("Removed `{identifier}`"));
        let next_source = next_id
            .as_deref()
            .and_then(|id| self.library.get(id))
            .map(|project| project.source.clone())
            .unwrap_or_default();
        self.source_input.update(cx, |input, input_cx| {
            input.set_value(next_source, window, input_cx);
        });
    }
}

fn project_sidebar(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let mut list = div().flex().flex_col().gap_1().w(px(210.0)).flex_shrink_0();
    list = list.child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(gpui_color(colors.text_secondary))
            .child(tr!(panels::STUDY_EDITOR_NEW_PROJECT)),
    );
    list = list.child(Input::new(&dialog.new_id_input).platform(theme));
    let mut template_row = div().flex().flex_wrap().gap_1();
    for (idx, (template, label)) in TEMPLATES.iter().enumerate() {
        let create_app = app.clone();
        let template = *template;
        template_row = template_row.child(
            Button::new(("study_editor_tpl", idx), theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .label(*label)
                .on_click(move |_, window, cx| {
                    create_app.update(cx, |surface, surface_cx| {
                        surface.create_study_editor_project(template, window, surface_cx);
                    });
                }),
        );
    }
    list = list.child(template_row);
    for (idx, project) in dialog.library.iter().enumerate() {
        let id = project.manifest.identifier.clone();
        let selected = dialog.selected_id.as_deref() == Some(id.as_str());
        let select_app = app.clone();
        let click_id = id.clone();
        list = list.child(
            Button::new(("study_editor_proj", idx), theme)
                .variant(if selected {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                })
                .button_size(ButtonSize::Sm)
                .label(id)
                .on_click(move |_, window, cx| {
                    select_app.update(cx, |surface, surface_cx| {
                        surface.select_study_editor_project(click_id.clone(), window, surface_cx);
                    });
                }),
        );
    }
    list.into_any_element()
}

fn diagnostics_panel(dialog: &StudyEditorDialogState, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let mut panel = div().flex().flex_col().gap_1();
    if let Some(status) = &dialog.status_message {
        panel = panel.child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(status.clone()),
        );
    }
    for diag in dialog.diagnostics.iter().take(8) {
        let code = diag.code.as_deref().unwrap_or("STUDY");
        let line = format!("{}:{} [{code}] {}", diag.line, diag.column, diag.message);
        let text_color = match diag.severity {
            StudyDiagnosticSeverity::Error => gpui_color(colors.text_danger),
            StudyDiagnosticSeverity::Warning => gpui_color(colors.text_warning),
            StudyDiagnosticSeverity::Note => gpui_color(colors.text_secondary),
        };
        panel = panel.child(div().text_xs().text_color(text_color).child(line));
    }
    panel.into_any_element()
}

pub(super) fn study_editor_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let revert = app.clone();
    let delete = app.clone();
    let cancel = app.clone();
    let save = app.clone();
    let has_selection = dialog.selected_id.is_some();

    let editor_pane = if has_selection {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(TextArea::new(&dialog.source_input, px(300.0), theme))
            .child(diagnostics_panel(dialog, theme))
            .into_any_element()
    } else {
        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(gpui_color(colors.text_secondary))
            .child(tr!(panels::STUDY_EDITOR_NO_PROJECTS))
            .into_any_element()
    };

    Dialog::new(
        "study_editor_dialog",
        DialogSize::Lg,
        theme,
        move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_study_editor_dialog);
        },
    )
    .title(tr!(panels::STUDY_EDITOR))
    .subtitle(tr!(panels::STUDY_EDITOR_SUBTITLE))
    .max_height(px(640.0))
    .child(
        div()
            .flex()
            .gap_3()
            .w_full()
            .child(project_sidebar(app, dialog, theme))
            .child(editor_pane),
    )
    .footer_leading(
        div()
            .flex()
            .gap_2()
            .child(
                Button::new("study_editor_revert", theme)
                    .variant(ButtonVariant::Secondary)
                    .label(tr!(panels::STUDY_EDITOR_REVERT))
                    .disabled(!has_selection)
                    .on_click(move |_, window, cx| {
                        revert.update(cx, |surface, surface_cx| {
                            surface.revert_study_editor_project(window, surface_cx);
                        });
                    }),
            )
            .child(
                Button::new("study_editor_delete", theme)
                    .variant(ButtonVariant::Destructive)
                    .label(tr!(panels::REMOVE))
                    .disabled(!has_selection)
                    .on_click(move |_, window, cx| {
                        delete.update(cx, |surface, surface_cx| {
                            surface.delete_study_editor_project(window, surface_cx);
                        });
                    }),
            ),
    )
    .action(
        Button::new("study_editor_close", theme)
            .variant(ButtonVariant::Secondary)
            .label(tr!(common::CANCEL))
            .on_click(move |_, _, cx| {
                cancel.update(cx, WorkspaceSurface::close_study_editor_dialog);
            }),
    )
    .action(
        Button::new("study_editor_save", theme)
            .variant(ButtonVariant::Primary)
            .label(tr!(panels::STUDY_EDITOR_VALIDATE_SAVE))
            .disabled(!has_selection)
            .on_click(move |_, _, cx| {
                save.update(cx, WorkspaceSurface::validate_and_save_study_editor_project);
            }),
    )
    .into_any_element()
}
