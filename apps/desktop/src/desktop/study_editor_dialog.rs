use super::native_ui::input::{TextArea, TextareaState};
use super::study_editor_store::{
    StudyProjectStore, compile_study_source_to_wasm, export_project_bundle_json,
    import_project_bundle_json, probe_study_dev_pack, run_offline_preview_check,
};
use super::*;
use aeris_localization::{common, panels, tr};
use aeris_study_sdk::project::{
    StudyDiagnostic, StudyDiagnosticSeverity, StudyProjectLibrary, StudyProjectTemplate,
};
use aeris_study_sdk::sandbox::StudySandboxLimits;
use aeris_study_sdk::toolchain::{PreviewFixturePreset, StudyDevPackStatus};
use aeris_study_sdk::wasm_runtime::CompiledWasmStudy;
use std::path::PathBuf;

pub(super) struct StudyEditorDialogState {
    pub(super) store: StudyProjectStore,
    pub(super) library: StudyProjectLibrary,
    pub(super) selected_id: Option<String>,
    pub(super) new_id_input: Entity<InputState>,
    pub(super) source_input: Entity<TextareaState>,
    pub(super) diagnostics: Vec<StudyDiagnostic>,
    pub(super) status_message: Option<String>,
    pub(super) dev_pack_status: StudyDevPackStatus,
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
        let dev_pack_status = probe_study_dev_pack();
        if let StudyDevPackStatus::Ready { rustc_version, .. }
        | StudyDevPackStatus::MissingSandboxTarget { rustc_version, .. } = &dev_pack_status
        {
            let _ = store.prune_stale_artifact_cache(rustc_version);
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
            dev_pack_status,
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
        match compile_study_source_to_wasm(&source) {
            Ok(artifact) => {
                self.diagnostics.clear();
                if let Err(error) = self.store.save_last_good_wasm(
                    &identifier,
                    &artifact.source_sha256,
                    &artifact.toolchain_identity,
                    &artifact.wasm_bytes,
                ) {
                    self.status_message = Some(error);
                    return;
                }
                let _ = self.library.record_build_success(
                    &identifier,
                    artifact.source_sha256,
                    artifact.artifact_sha256.clone(),
                );
                let kib = artifact.wasm_bytes.len().div_ceil(1024);
                self.status_message = match self
                    .library
                    .get(&identifier)
                    .map(|project| self.store.save_project(project))
                {
                    Some(Ok(())) => Some(format!(
                        "Built WASM ({kib} KiB · {})",
                        &artifact.artifact_sha256[..12]
                    )),
                    Some(Err(error)) => Some(error),
                    None => None,
                };
            }
            Err(diagnostics) => {
                self.diagnostics = diagnostics.clone();
                let _ = self.library.record_build_failure(&identifier, diagnostics);
                if let Some(project) = self.library.get(&identifier) {
                    let _ = self.store.save_project(project);
                }
                self.status_message = Some(format!(
                    "Build failed ({} diagnostic(s)); last-good artifact preserved",
                    self.diagnostics.len()
                ));
            }
        }
    }

    pub(super) fn prepare_chart_artifact(
        &mut self,
        cx: &mut Context<WorkspaceSurface>,
    ) -> Result<(String, String, u32, CompiledWasmStudy), String> {
        let identifier = self
            .selected_id
            .clone()
            .ok_or_else(|| "Select a study project first".to_string())?;
        let source = self.source_input.read(cx).value().to_string();
        if let Err(error) = self.library.update_source(&identifier, source.clone()) {
            return Err(format!("Update failed: {error:?}"));
        }
        let compiled = match compile_study_source_to_wasm(&source) {
            Ok(artifact) => {
                self.diagnostics.clear();
                let _ = self.store.save_last_good_wasm(
                    &identifier,
                    &artifact.source_sha256,
                    &artifact.toolchain_identity,
                    &artifact.wasm_bytes,
                );
                let _ = self.library.record_build_success(
                    &identifier,
                    artifact.source_sha256,
                    artifact.artifact_sha256,
                );
                if let Some(project) = self.library.get(&identifier) {
                    let _ = self.store.save_project(project);
                }
                artifact.compiled
            }
            Err(diagnostics) => {
                self.diagnostics = diagnostics;
                // Fall back to previously built `last_good.wasm` if available on disk.
                let fallback = self
                    .store
                    .load_last_good_wasm(&identifier)
                    .ok()
                    .flatten()
                    .and_then(|bytes| {
                        CompiledWasmStudy::from_bytes(&bytes, StudySandboxLimits::default()).ok()
                    });
                let Some(compiled) = fallback else {
                    let msg = format!(
                        "Build failed ({} diagnostic(s)) and no last-good WASM exists",
                        self.diagnostics.len()
                    );
                    self.status_message = Some(msg.clone());
                    return Err(msg);
                };
                compiled
            }
        };
        let (name, revision) = self.library.get(&identifier).map_or_else(
            || (identifier.clone(), 1),
            |p| (p.manifest.name.clone(), p.manifest.implementation_revision),
        );
        Ok((identifier, name, revision, compiled))
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

    pub(super) fn duplicate_selected(
        &mut self,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let Some(source_id) = self.selected_id.clone() else {
            return;
        };
        let new_id = format!("{source_id}_copy_{}", self.library.len() + 1);
        let new_name = new_id
            .rsplit('.')
            .next()
            .unwrap_or(&new_id)
            .replace('_', " ");
        match self.library.duplicate(&source_id, &new_id, &new_name) {
            Ok(project) => {
                let save_result = self.store.save_project(project);
                let source = project.source.clone();
                self.selected_id = Some(new_id.clone());
                self.diagnostics.clear();
                self.status_message = match save_result {
                    Ok(()) => Some(format!("Duplicated to `{new_id}`")),
                    Err(error) => Some(error),
                };
                self.source_input.update(cx, |input, input_cx| {
                    input.set_value(source, window, input_cx);
                });
            }
            Err(error) => {
                self.status_message = Some(format!("Duplicate failed: {error:?}"));
            }
        }
    }

    pub(super) fn export_selected_bundle(&mut self, cx: &mut Context<WorkspaceSurface>) {
        let Some(identifier) = self.selected_id.as_deref() else {
            return;
        };
        let Some(project) = self.library.get(identifier) else {
            return;
        };
        match export_project_bundle_json(project) {
            Ok(json) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(json));
                self.status_message = Some(format!("Copied `{identifier}` bundle to clipboard"));
            }
            Err(error) => {
                self.status_message = Some(format!("Export failed: {error}"));
            }
        }
    }

    pub(super) fn run_preview_test(&mut self, cx: &mut Context<WorkspaceSurface>) {
        if self.selected_id.is_none() {
            return;
        }
        let source = self.source_input.read(cx).value().to_string();
        let (diagnostics, summary) =
            run_offline_preview_check(&source, PreviewFixturePreset::Uptrend);
        self.diagnostics = diagnostics;
        self.status_message = Some(summary);
    }

    pub(super) fn import_bundle_from_clipboard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let Some(raw) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.status_message = Some("Clipboard does not contain a study bundle".to_string());
            return;
        };
        match import_project_bundle_json(&raw) {
            Ok((manifest, source)) => {
                let id = manifest.identifier.clone();
                let name = manifest.name.clone();
                match self.library.create_from_template(
                    &id,
                    &name,
                    StudyProjectTemplate::StatelessOverlay,
                ) {
                    Ok(_) => {
                        let _ = self.library.update_source(&id, source.clone());
                        if let Some(project) = self.library.get(&id) {
                            let _ = self.store.save_project(project);
                        }
                        self.selected_id = Some(id.clone());
                        self.diagnostics.clear();
                        self.status_message = Some(format!("Imported `{id}` from clipboard"));
                        self.source_input.update(cx, |input, input_cx| {
                            input.set_value(source, window, input_cx);
                        });
                    }
                    Err(error) => {
                        self.status_message = Some(format!("Import failed: {error:?}"));
                    }
                }
            }
            Err(error) => {
                self.status_message = Some(error);
            }
        }
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
    let toolchain_line = match &dialog.dev_pack_status {
        StudyDevPackStatus::Ready {
            rustc_version,
            target_triple,
        } => format!("Dev Pack: Ready ({rustc_version} · {target_triple})"),
        StudyDevPackStatus::MissingSandboxTarget {
            rustc_version,
            required_target,
        } => format!(
            "Dev Pack: {rustc_version} (install `{required_target}` target for WASM builds)"
        ),
        StudyDevPackStatus::Unavailable => {
            "Dev Pack: Static sandbox validation mode (no local rustc detected)".to_string()
        }
    };
    panel = panel.child(
        div()
            .text_xs()
            .text_color(gpui_color(colors.text_secondary))
            .child(toolchain_line),
    );
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
    let duplicate = app.clone();
    let export_bundle = app.clone();
    let import_bundle = app.clone();
    let preview = app.clone();
    let add_to_chart = app.clone();
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
                Button::new("study_editor_duplicate", theme)
                    .variant(ButtonVariant::Secondary)
                    .label("Duplicate")
                    .disabled(!has_selection)
                    .on_click(move |_, window, cx| {
                        duplicate.update(cx, |surface, surface_cx| {
                            surface.duplicate_study_editor_project(window, surface_cx);
                        });
                    }),
            )
            .child(
                Button::new("study_editor_export", theme)
                    .variant(ButtonVariant::Secondary)
                    .label("Copy bundle")
                    .disabled(!has_selection)
                    .on_click(move |_, _, cx| {
                        export_bundle.update(cx, WorkspaceSurface::export_study_editor_bundle);
                    }),
            )
            .child(
                Button::new("study_editor_import", theme)
                    .variant(ButtonVariant::Secondary)
                    .label("Import bundle")
                    .on_click(move |_, window, cx| {
                        import_bundle.update(cx, |surface, surface_cx| {
                            surface.import_study_editor_bundle(window, surface_cx);
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
        Button::new("study_editor_preview", theme)
            .variant(ButtonVariant::Secondary)
            .label("Preview test")
            .disabled(!has_selection)
            .on_click(move |_, _, cx| {
                preview.update(cx, WorkspaceSurface::run_study_editor_preview_test);
            }),
    )
    .action(
        Button::new("study_editor_add_chart", theme)
            .variant(ButtonVariant::Secondary)
            .label("Add to chart")
            .disabled(!has_selection)
            .on_click(move |_, _, cx| {
                add_to_chart.update(cx, WorkspaceSurface::add_study_editor_project_to_chart);
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
