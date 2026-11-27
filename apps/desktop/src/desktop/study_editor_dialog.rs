use super::native_ui::input::{SyntaxHighlighter, TextArea, TextareaState};
use super::study_editor_highlight::{
    STUDY_API_QUICK_REF, STUDY_SNIPPETS, line_column_to_byte_range, rust_syntax_highlighter,
};
use super::study_editor_store::{
    CompiledStudyArtifact, StudyProjectStore, compile_study_source_to_wasm,
    export_project_bundle_json, import_project_bundle_json, install_wasm_sandbox_target,
    probe_study_dev_pack, run_offline_preview_check,
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
    pub(super) search_input: Entity<InputState>,
    pub(super) source_input: Entity<TextareaState>,
    pub(super) highlighter: SyntaxHighlighter,
    pub(super) diagnostics: Vec<StudyDiagnostic>,
    pub(super) status_message: Option<String>,
    pub(super) busy_label: Option<String>,
    pub(super) dev_pack_status: Option<StudyDevPackStatus>,
    pub(super) active_job_id: u64,
    pub(super) active_task: Option<gpui::Task<()>>,
    pub(super) probe_task: Option<gpui::Task<()>>,
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
        theme: &AerisTheme,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) -> Self {
        let store = StudyProjectStore::new(&data_root);
        let (mut library, warnings) = store.load_library();

        // Always open with a live, ready-to-edit Rust indicator project so the code
        // editor is immediately visible and usable on the very first open.
        if library.is_empty()
            && let Ok(project) = library.create_from_template(
                "user.my_study",
                "My Study",
                StudyProjectTemplate::StatelessOverlay,
            )
        {
            let _ = store.save_project(project);
        }

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
        let search_input =
            cx.new(|input_cx| InputState::new(window, input_cx).placeholder("Find in code..."));
        let source_input = cx.new(|input_cx| {
            TextareaState::new(window, input_cx).placeholder("// Rust study source")
        });
        if !initial_source.is_empty() {
            source_input.update(cx, |input, input_cx| {
                input.set_value(initial_source, window, input_cx);
            });
        }
        let highlighter = rust_syntax_highlighter(theme);
        let status_message = if !warnings.is_empty() {
            Some(warnings.join(" · "))
        } else {
            Some("Ready — edit Rust code, run Preview test, or Add / Reload on chart".to_string())
        };

        // Probe rustc + wasm32-unknown-unknown asynchronously off the GPUI thread so
        // opening the Study Editor never stalls the live chart.
        let probe_store = store.clone();
        let probe_task = cx.spawn(async move |surface, cx| {
            let status = cx
                .background_executor()
                .spawn(async move {
                    let status = probe_study_dev_pack();
                    if let StudyDevPackStatus::Ready { rustc_version, .. }
                    | StudyDevPackStatus::MissingSandboxTarget { rustc_version, .. } = &status
                    {
                        let _ = probe_store.prune_stale_artifact_cache(rustc_version);
                    }
                    status
                })
                .await;
            let _ = surface.update(cx, |surface, surface_cx| {
                if let Some(IndicatorSettingsDialog::StudyEditor(dialog)) =
                    surface.indicator_settings_dialog.as_mut()
                {
                    dialog.dev_pack_status = Some(status);
                    dialog.probe_task = None;
                    surface_cx.notify();
                }
            });
        });

        Self {
            store,
            library,
            selected_id,
            new_id_input,
            search_input,
            source_input,
            highlighter,
            diagnostics: Vec::new(),
            status_message,
            busy_label: None,
            dev_pack_status: None,
            active_job_id: 0,
            active_task: None,
            probe_task: Some(probe_task),
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
            let mut n = self.library.len() + 1;
            loop {
                let candidate = format!("user.study_{n}");
                if self.library.get(&candidate).is_none() {
                    break candidate;
                }
                n += 1;
            }
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

    pub(super) fn start_build_and_save(&mut self, cx: &mut Context<WorkspaceSurface>) {
        let Some(identifier) = self.selected_id.clone() else {
            return;
        };
        let source = self.source_input.read(cx).value().to_string();
        if let Err(error) = self.library.update_source(&identifier, source.clone()) {
            self.status_message = Some(format!("Update failed: {error:?}"));
            return;
        }
        self.active_job_id = self.active_job_id.wrapping_add(1);
        let job_id = self.active_job_id;
        self.busy_label = Some(format!("Compiling `{identifier}` to WASM..."));
        self.status_message = None;

        self.active_task = Some(cx.spawn(async move |surface, cx| {
            let build_result = cx
                .background_executor()
                .spawn(async move { compile_study_source_to_wasm(&source) })
                .await;
            let _ = surface.update(cx, |surface, surface_cx| {
                if let Some(IndicatorSettingsDialog::StudyEditor(dialog)) =
                    surface.indicator_settings_dialog.as_mut()
                {
                    if dialog.active_job_id != job_id {
                        return;
                    }
                    dialog.busy_label = None;
                    dialog.active_task = None;
                    dialog.finish_build_and_save(&identifier, build_result);
                    surface_cx.notify();
                }
            });
        }));
    }

    fn finish_build_and_save(
        &mut self,
        identifier: &str,
        result: Result<CompiledStudyArtifact, Vec<StudyDiagnostic>>,
    ) {
        match result {
            Ok(artifact) => {
                self.diagnostics.clear();
                if let Err(error) = self.store.save_last_good_wasm(
                    identifier,
                    &artifact.source_sha256,
                    &artifact.toolchain_identity,
                    &artifact.wasm_bytes,
                ) {
                    self.status_message = Some(error);
                    return;
                }
                let _ = self.library.record_build_success(
                    identifier,
                    artifact.source_sha256,
                    artifact.artifact_sha256.clone(),
                );
                let kib = artifact.wasm_bytes.len().div_ceil(1024);
                self.status_message = match self
                    .library
                    .get(identifier)
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
                let _ = self.library.record_build_failure(identifier, diagnostics);
                if let Some(project) = self.library.get(identifier) {
                    let _ = self.store.save_project(project);
                }
                self.status_message = Some(format!(
                    "Build failed ({} diagnostic(s)); last-good artifact preserved",
                    self.diagnostics.len()
                ));
            }
        }
    }

    pub(super) fn start_preview_test(&mut self, cx: &mut Context<WorkspaceSurface>) {
        if self.selected_id.is_none() {
            return;
        }
        let source = self.source_input.read(cx).value().to_string();
        self.active_job_id = self.active_job_id.wrapping_add(1);
        let job_id = self.active_job_id;
        self.busy_label = Some("Compiling & running WASM preview...".to_string());
        self.status_message = None;

        self.active_task = Some(cx.spawn(async move |surface, cx| {
            let (diagnostics, summary) =
                cx.background_executor()
                    .spawn(async move {
                        run_offline_preview_check(&source, PreviewFixturePreset::Uptrend)
                    })
                    .await;
            let _ = surface.update(cx, |surface, surface_cx| {
                if let Some(IndicatorSettingsDialog::StudyEditor(dialog)) =
                    surface.indicator_settings_dialog.as_mut()
                {
                    if dialog.active_job_id != job_id {
                        return;
                    }
                    dialog.busy_label = None;
                    dialog.active_task = None;
                    dialog.diagnostics = diagnostics;
                    dialog.status_message = Some(summary);
                    surface_cx.notify();
                }
            });
        }));
    }

    pub(super) fn start_build_for_chart(&mut self, cx: &mut Context<WorkspaceSurface>) {
        let Some(identifier) = self.selected_id.clone() else {
            return;
        };
        let source = self.source_input.read(cx).value().to_string();
        if let Err(error) = self.library.update_source(&identifier, source.clone()) {
            self.status_message = Some(format!("Update failed: {error:?}"));
            return;
        }
        let (display_name, revision) = self.library.get(&identifier).map_or_else(
            || (identifier.clone(), 1),
            |p| (p.manifest.name.clone(), p.manifest.implementation_revision),
        );
        self.active_job_id = self.active_job_id.wrapping_add(1);
        let job_id = self.active_job_id;
        self.busy_label = Some(format!("Building `{identifier}` for chart..."));
        self.status_message = None;
        let fallback_store = self.store.clone();
        let bg_id = identifier.clone();

        self.active_task = Some(cx.spawn(async move |surface, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    match compile_study_source_to_wasm(&source) {
                        Ok(artifact) => Ok((Some(artifact.clone()), artifact.compiled)),
                        Err(diagnostics) => {
                            let fallback = fallback_store
                                .load_last_good_wasm(&bg_id)
                                .ok()
                                .flatten()
                                .and_then(|bytes| {
                                    CompiledWasmStudy::from_bytes(
                                        &bytes,
                                        StudySandboxLimits::default(),
                                    )
                                    .ok()
                                });
                            match fallback {
                                Some(compiled) => Ok((None, compiled)),
                                None => Err(diagnostics),
                            }
                        }
                    }
                })
                .await;

            let _ = surface.update(cx, |surface, surface_cx| {
                let Some(IndicatorSettingsDialog::StudyEditor(dialog)) =
                    surface.indicator_settings_dialog.as_mut()
                else {
                    return;
                };
                if dialog.active_job_id != job_id {
                    return;
                }
                dialog.busy_label = None;
                dialog.active_task = None;
                match outcome {
                    Ok((maybe_artifact, compiled)) => {
                        if let Some(artifact) = maybe_artifact {
                            dialog.diagnostics.clear();
                            let _ = dialog.store.save_last_good_wasm(
                                &identifier,
                                &artifact.source_sha256,
                                &artifact.toolchain_identity,
                                &artifact.wasm_bytes,
                            );
                            let _ = dialog.library.record_build_success(
                                &identifier,
                                artifact.source_sha256,
                                artifact.artifact_sha256,
                            );
                            if let Some(project) = dialog.library.get(&identifier) {
                                let _ = dialog.store.save_project(project);
                            }
                        }
                        surface.attach_compiled_wasm_study_to_chart(
                            identifier,
                            display_name,
                            revision,
                            compiled,
                            surface_cx,
                        );
                    }
                    Err(diagnostics) => {
                        let count = diagnostics.len();
                        dialog.diagnostics = diagnostics;
                        dialog.status_message = Some(format!(
                            "Build failed ({count} diagnostic(s)) and no last-good WASM exists"
                        ));
                        surface_cx.notify();
                    }
                }
            });
        }));
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
        let mut n = self.library.len() + 1;
        let new_id = loop {
            let candidate = format!("{source_id}_copy_{n}");
            if self.library.get(&candidate).is_none() {
                break candidate;
            }
            n += 1;
        };
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

    pub(super) fn insert_snippet(
        &mut self,
        snippet: &'static str,
        doc: &'static str,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        if self.selected_id.is_none() {
            return;
        }
        self.source_input.update(cx, |input, input_cx| {
            input.insert(snippet, window, input_cx);
        });
        self.status_message = Some(format!("Inserted snippet: {doc}"));
    }

    pub(super) fn find_next_in_source(
        &mut self,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            return;
        }
        let source = self.source_input.read(cx).value().to_string();
        let cursor = self.source_input.read(cx).cursor().min(source.len());
        let found = source[cursor..]
            .find(&query)
            .map(|rel| cursor + rel)
            .or_else(|| source.find(&query));
        if let Some(start) = found {
            let end = start + query.len();
            self.source_input.update(cx, |input, input_cx| {
                input.select_byte_range(start, end, window, input_cx);
            });
            self.status_message = Some(format!("Found `{query}`"));
        } else {
            self.status_message = Some(format!("`{query}` not found"));
        }
    }

    pub(super) fn jump_to_diagnostic(
        &mut self,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<WorkspaceSurface>,
    ) {
        let source = self.source_input.read(cx).value().to_string();
        let (start, end) = line_column_to_byte_range(&source, line, column);
        self.source_input.update(cx, |input, input_cx| {
            input.select_byte_range(start, end, window, input_cx);
        });
    }

    pub(super) fn start_install_dev_pack_target(&mut self, cx: &mut Context<WorkspaceSurface>) {
        self.busy_label = Some("Installing `wasm32-unknown-unknown` target...".to_string());
        self.probe_task = Some(cx.spawn(async move |surface, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { install_wasm_sandbox_target() })
                .await;
            let _ = surface.update(cx, |surface, surface_cx| {
                if let Some(IndicatorSettingsDialog::StudyEditor(dialog)) =
                    surface.indicator_settings_dialog.as_mut()
                {
                    dialog.busy_label = None;
                    dialog.probe_task = None;
                    match result {
                        Ok(status) => {
                            dialog.dev_pack_status = Some(status);
                            dialog.status_message = Some(
                                "Installed `wasm32-unknown-unknown` sandbox target".to_string(),
                            );
                        }
                        Err(error) => {
                            dialog.status_message = Some(error);
                        }
                    }
                    surface_cx.notify();
                }
            });
        }));
    }

    pub(super) fn start_clean_dev_pack_cache(&mut self, cx: &mut Context<WorkspaceSurface>) {
        self.busy_label = Some("Re-checking Rust Dev Pack...".to_string());
        let store = self.store.clone();
        self.probe_task = Some(cx.spawn(async move |surface, cx| {
            let (status, removed) = cx
                .background_executor()
                .spawn(async move {
                    let status = probe_study_dev_pack();
                    let host_id = match &status {
                        StudyDevPackStatus::Ready { rustc_version, .. }
                        | StudyDevPackStatus::MissingSandboxTarget { rustc_version, .. } => {
                            rustc_version.as_str()
                        }
                        StudyDevPackStatus::Unavailable => "",
                    };
                    let removed = store.prune_stale_artifact_cache(host_id);
                    (status, removed)
                })
                .await;
            let _ = surface.update(cx, |surface, surface_cx| {
                if let Some(IndicatorSettingsDialog::StudyEditor(dialog)) =
                    surface.indicator_settings_dialog.as_mut()
                {
                    dialog.busy_label = None;
                    dialog.probe_task = None;
                    dialog.dev_pack_status = Some(status);
                    dialog.status_message = Some(format!(
                        "Dev Pack re-checked; pruned {removed} stale cache receipt(s)"
                    ));
                    surface_cx.notify();
                }
            });
        }));
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
    let has_selection = dialog.selected_id.is_some();
    let revert = app.clone();
    let duplicate = app.clone();
    let export_bundle = app.clone();
    let import_bundle = app.clone();
    let delete = app.clone();

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

    let mut project_list = div()
        .id("study_editor_project_list")
        .flex_1()
        .min_h(px(140.0))
        .max_h(px(260.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1();
    for (idx, project) in dialog.library.iter().enumerate() {
        let id = project.manifest.identifier.clone();
        let selected = dialog.selected_id.as_deref() == Some(id.as_str());
        let select_app = app.clone();
        let click_id = id.clone();
        project_list = project_list.child(
            Button::new(("study_editor_proj", idx), theme)
                .variant(if selected {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                })
                .button_size(ButtonSize::Sm)
                .w_full()
                .label(id)
                .on_click(move |_, window, cx| {
                    select_app.update(cx, |surface, surface_cx| {
                        surface.select_study_editor_project(click_id.clone(), window, surface_cx);
                    });
                }),
        );
    }

    let project_actions = div()
        .flex()
        .flex_wrap()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            Button::new("study_editor_revert", theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
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
                .button_size(ButtonSize::Sm)
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
                .button_size(ButtonSize::Sm)
                .label("Copy")
                .disabled(!has_selection)
                .on_click(move |_, _, cx| {
                    export_bundle.update(cx, WorkspaceSurface::export_study_editor_bundle);
                }),
        )
        .child(
            Button::new("study_editor_import", theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .label("Import")
                .on_click(move |_, window, cx| {
                    import_bundle.update(cx, |surface, surface_cx| {
                        surface.import_study_editor_bundle(window, surface_cx);
                    });
                }),
        )
        .child(
            Button::new("study_editor_delete", theme)
                .variant(ButtonVariant::Destructive)
                .button_size(ButtonSize::Sm)
                .label(tr!(panels::REMOVE))
                .disabled(!has_selection)
                .on_click(move |_, window, cx| {
                    delete.update(cx, |surface, surface_cx| {
                        surface.delete_study_editor_project(window, surface_cx);
                    });
                }),
        );

    div()
        .flex()
        .flex_col()
        .gap_2()
        .w(px(230.0))
        .flex_shrink_0()
        .pr_3()
        .border_r_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_secondary))
                .child(tr!(panels::STUDY_EDITOR_NEW_PROJECT)),
        )
        .child(
            div()
                .h(px(30.0))
                .child(Input::new(&dialog.new_id_input).platform(theme)),
        )
        .child(template_row)
        .child(
            div()
                .pt_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_secondary))
                .child("Projects"),
        )
        .child(project_list)
        .child(project_actions)
        .into_any_element()
}

fn diagnostics_console(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let mut panel = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .min_h(px(76.0))
        .max_h(px(130.0))
        .rounded(px(6.0))
        .bg(gpui_color(colors.surface_secondary))
        .border_1()
        .border_color(gpui_color(colors.border_secondary));

    if let Some(busy) = &dialog.busy_label {
        panel = panel.child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.ring_primary))
                .child(busy.clone()),
        );
    } else if let Some(status) = &dialog.status_message {
        panel = panel.child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(status.clone()),
        );
    }

    for (idx, diag) in dialog.diagnostics.iter().take(6).enumerate() {
        let code = diag.code.as_deref().unwrap_or("STUDY");
        let label = format!("{}:{} [{code}] {}", diag.line, diag.column, diag.message);
        let text_color = match diag.severity {
            StudyDiagnosticSeverity::Error => gpui_color(colors.text_danger),
            StudyDiagnosticSeverity::Warning => gpui_color(colors.text_warning),
            StudyDiagnosticSeverity::Note => gpui_color(colors.text_secondary),
        };
        let jump_app = app.clone();
        let line = diag.line;
        let column = diag.column;
        panel = panel.child(
            div()
                .id(("study_editor_diag", idx))
                .text_xs()
                .text_color(text_color)
                .cursor_pointer()
                .on_click(move |_, window, cx| {
                    jump_app.update(cx, |surface, surface_cx| {
                        surface.jump_to_study_editor_diagnostic(line, column, window, surface_cx);
                    });
                })
                .child(label),
        );
    }
    panel.into_any_element()
}

fn editor_toolbar(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let mut snippets = div().flex().flex_wrap().items_center().gap_1();
    for (idx, snippet) in STUDY_SNIPPETS.iter().enumerate() {
        let insert_app = app.clone();
        let body = snippet.body;
        let doc = snippet.doc;
        snippets = snippets.child(
            Button::new(("study_editor_snip", idx), theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .label(snippet.label)
                .on_click(move |_, window, cx| {
                    insert_app.update(cx, |surface, surface_cx| {
                        surface.insert_study_editor_snippet(body, doc, window, surface_cx);
                    });
                }),
        );
    }
    let find_app = app.clone();
    let find_row = div()
        .flex()
        .items_center()
        .gap_1()
        .w(px(220.0))
        .h(px(28.0))
        .child(Input::new(&dialog.search_input).platform(theme).flex_1())
        .child(
            Button::new("study_editor_find_next", theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .label("Find")
                .on_click(move |_, window, cx| {
                    find_app.update(cx, |surface, surface_cx| {
                        surface.find_in_study_editor(window, surface_cx);
                    });
                }),
        );
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(snippets)
                .child(find_row),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(STUDY_API_QUICK_REF),
        )
        .into_any_element()
}

fn dev_pack_footer(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let is_busy = dialog.busy_label.is_some();
    let toolchain_line = match &dialog.dev_pack_status {
        None => "Dev Pack: Checking Rust toolchain...".to_string(),
        Some(StudyDevPackStatus::Ready {
            rustc_version,
            target_triple,
        }) => format!("Dev Pack: Ready ({rustc_version} · {target_triple})"),
        Some(StudyDevPackStatus::MissingSandboxTarget {
            rustc_version,
            required_target,
        }) => format!("Dev Pack: {rustc_version} (missing `{required_target}`)"),
        Some(StudyDevPackStatus::Unavailable) => {
            "Dev Pack: No local `rustc` detected (install Rust + wasm32-unknown-unknown)"
                .to_string()
        }
    };
    let mut row = div().flex().items_center().gap_2().child(
        div()
            .text_xs()
            .truncate()
            .text_color(gpui_color(colors.text_secondary))
            .child(toolchain_line),
    );
    if matches!(
        dialog.dev_pack_status,
        Some(StudyDevPackStatus::MissingSandboxTarget { .. })
    ) {
        let install_app = app.clone();
        row = row.child(
            Button::new("study_editor_install_target", theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .label("Install WASM target")
                .disabled(is_busy)
                .on_click(move |_, _, cx| {
                    install_app.update(cx, WorkspaceSurface::install_study_editor_dev_pack_target);
                }),
        );
    }
    let clean_app = app.clone();
    row = row.child(
        Button::new("study_editor_recheck_pack", theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Sm)
            .label("Re-check")
            .disabled(is_busy)
            .on_click(move |_, _, cx| {
                clean_app.update(cx, WorkspaceSurface::clean_study_editor_cache);
            }),
    );
    row.into_any_element()
}

pub(super) fn study_editor_dialog_layer(
    app: &Entity<WorkspaceSurface>,
    dialog: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = app.clone();
    let preview = app.clone();
    let add_to_chart = app.clone();
    let cancel = app.clone();
    let save = app.clone();
    let has_selection = dialog.selected_id.is_some();
    let is_busy = dialog.busy_label.is_some();

    let editor_pane = if has_selection {
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(editor_toolbar(app, dialog, theme))
            .child(
                TextArea::new(&dialog.source_input, px(360.0), theme)
                    .syntax_highlighter(dialog.highlighter.clone()),
            )
            .child(diagnostics_console(app, dialog, theme))
            .into_any_element()
    } else {
        div()
            .flex_1()
            .min_w_0()
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
        DialogSize::Xl,
        theme,
        move |_, cx| {
            dismiss.update(cx, WorkspaceSurface::close_study_editor_dialog);
        },
    )
    .title(tr!(panels::STUDY_EDITOR))
    .subtitle(tr!(panels::STUDY_EDITOR_SUBTITLE))
    .max_height(px(720.0))
    .child(
        div()
            .flex()
            .gap_3()
            .w_full()
            .min_h(px(520.0))
            .child(project_sidebar(app, dialog, theme))
            .child(editor_pane),
    )
    .footer_leading(dev_pack_footer(app, dialog, theme))
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
            .disabled(!has_selection || is_busy)
            .on_click(move |_, _, cx| {
                preview.update(cx, WorkspaceSurface::run_study_editor_preview_test);
            }),
    )
    .action(
        Button::new("study_editor_add_chart", theme)
            .variant(ButtonVariant::Secondary)
            .label("Add / Reload on chart")
            .disabled(!has_selection || is_busy)
            .on_click(move |_, _, cx| {
                add_to_chart.update(cx, WorkspaceSurface::add_study_editor_project_to_chart);
            }),
    )
    .action(
        Button::new("study_editor_save", theme)
            .variant(ButtonVariant::Primary)
            .label(tr!(panels::STUDY_EDITOR_VALIDATE_SAVE))
            .disabled(!has_selection || is_busy)
            .on_click(move |_, _, cx| {
                save.update(cx, WorkspaceSurface::validate_and_save_study_editor_project);
            }),
    )
    .into_any_element()
}
