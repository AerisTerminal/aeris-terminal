use super::native_ui::input::{SyntaxHighlighter, TextArea, TextareaState};
use super::study_editor_highlight::{
    STUDY_API_QUICK_REF, STUDY_SNIPPETS, line_column_to_byte_range, rust_syntax_highlighter,
};
use super::study_editor_store::{
    CompiledStudyArtifact, StudyProjectStore, compile_study_source_to_wasm,
    export_project_bundle_json, import_project_bundle_json, install_wasm_sandbox_target,
    parse_study_source_spec, probe_study_dev_pack, probe_study_dev_pack_fresh,
    run_offline_preview_check,
};
use super::*;
use aeris_localization::{panels, tr};
use aeris_study_sdk::project::{
    StudyDiagnostic, StudyDiagnosticSeverity, StudyProjectLibrary, StudyProjectTemplate,
};
use aeris_study_sdk::sandbox::StudySandboxLimits;
use aeris_study_sdk::toolchain::{PreviewFixturePreset, StudyDevPackStatus};
use aeris_study_sdk::wasm_runtime::CompiledWasmStudy;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum StudyEditorDock {
    #[default]
    Right,
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum StudyEditorPopover {
    #[default]
    None,
    Scripts,
    More,
}

#[derive(Clone)]
pub(super) struct StudyEditorWidthDrag;

impl Render for StudyEditorWidthDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
pub(super) struct StudyEditorHeightDrag;

impl Render for StudyEditorHeightDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

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
    pub(super) dock: StudyEditorDock,
    pub(super) width: f32,
    pub(super) height: f32,
    pub(super) popover: StudyEditorPopover,
    pub(super) find_open: bool,
    pub(super) snippets_open: bool,
    pub(super) console_open: bool,
    pub(super) line_numbers: bool,
    pub(super) word_wrap: bool,
    pub(super) font_size: f32,
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
        source_input.update(cx, |input, input_cx| {
            input.set_soft_wrap(false, window, input_cx);
            if !initial_source.is_empty() {
                input.set_value(initial_source, window, input_cx);
            }
        });
        let highlighter = rust_syntax_highlighter(theme);
        let status_message = if !warnings.is_empty() {
            Some(warnings.join(" · "))
        } else {
            Some("Ready".to_string())
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
                if let Some(dialog) = surface.study_editor.as_mut() {
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
            dock: StudyEditorDock::Right,
            width: 560.0,
            height: 340.0,
            popover: StudyEditorPopover::None,
            find_open: false,
            snippets_open: false,
            console_open: false,
            line_numbers: true,
            word_wrap: false,
            font_size: 13.0,
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
                if let Some(dialog) = surface.study_editor.as_mut() {
                    if dialog.active_job_id != job_id {
                        return;
                    }
                    dialog.busy_label = None;
                    dialog.active_task = None;
                    dialog.finish_build_and_save(&identifier, build_result);
                    if !dialog.diagnostics.is_empty() {
                        dialog.console_open = true;
                    }
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
                if let Some(dialog) = surface.study_editor.as_mut() {
                    if dialog.active_job_id != job_id {
                        return;
                    }
                    dialog.busy_label = None;
                    dialog.active_task = None;
                    dialog.console_open = !diagnostics.is_empty();
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
        let spec = parse_study_source_spec(&source, &display_name);
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
                let Some(dialog) = surface.study_editor.as_mut() else {
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
                            identifier, revision, spec, compiled, surface_cx,
                        );
                    }
                    Err(diagnostics) => {
                        let count = diagnostics.len();
                        dialog.diagnostics = diagnostics;
                        dialog.console_open = true;
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
                if let Some(dialog) = surface.study_editor.as_mut() {
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
                            dialog.console_open = true;
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
                    let status = probe_study_dev_pack_fresh();
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
                if let Some(dialog) = surface.study_editor.as_mut() {
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

fn popover_menu_row(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    icon: Option<HugeIcon>,
    destructive: bool,
    disabled: bool,
    theme: &AerisTheme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let colors = theme.colors;
    let fg = if disabled {
        gpui_color(colors.text_muted)
    } else if destructive {
        gpui_color(colors.text_danger)
    } else {
        gpui_color(colors.text_default)
    };
    let hover_bg = gpui_color(colors.hover_bg);
    div()
        .id(id)
        .w_full()
        .h(px(30.0))
        .px_2p5()
        .rounded(px(6.0))
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(fg)
        .when(!disabled, |row| {
            row.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .on_click(move |_, window, cx| {
                    on_click(window, cx);
                    cx.stop_propagation();
                })
        })
        .when_some(icon, |row, ic| {
            row.child(header_icon(ic).with_size(px(14.0)))
        })
        .child(label.into())
        .into_any_element()
}

fn editor_top_bar(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dock_app = app.clone();
    let expand_app = app.clone();
    let close_app = app.clone();
    let console_app = app.clone();
    let dot_color = if editor.busy_label.is_some() {
        gpui_color(colors.ring_primary)
    } else {
        match &editor.dev_pack_status {
            Some(StudyDevPackStatus::Ready { .. }) => gpui_color(colors.positive),
            Some(StudyDevPackStatus::MissingSandboxTarget { .. }) => gpui_color(colors.warning),
            Some(StudyDevPackStatus::Unavailable) => gpui_color(colors.text_danger),
            None => gpui_color(colors.text_muted),
        }
    };
    let dock_icon = match editor.dock {
        StudyEditorDock::Right => HugeIcon::SplitStacked,
        StudyEditorDock::Bottom => HugeIcon::SplitSideBySide,
    };
    let dock_tip = match editor.dock {
        StudyEditorDock::Right => "Dock editor to bottom",
        StudyEditorDock::Bottom => "Dock editor to right",
    };
    let expanded = match editor.dock {
        StudyEditorDock::Right => editor.width >= 760.0,
        StudyEditorDock::Bottom => editor.height >= 480.0,
    };
    let expand_icon = if expanded {
        HugeIcon::WindowRestore
    } else {
        HugeIcon::WindowMaximize
    };

    div()
        .h(px(34.0))
        .px_3()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(gpui_color(colors.border_subtle))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_default))
                .child(header_icon(HugeIcon::Template).with_size(px(14.0)))
                .child(tr!(panels::STUDY_EDITOR))
                .child(
                    div()
                        .id("study_editor_dev_dot")
                        .size(px(7.0))
                        .rounded_full()
                        .bg(dot_color)
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            console_app.update(cx, WorkspaceSurface::toggle_study_editor_console);
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("study_editor_dock_toggle", theme)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Sm)
                        .icon(header_icon(dock_icon))
                        .aria_label(dock_tip)
                        .tooltip(TooltipSpec::new(dock_tip, theme).show_delay(TOOLTIP_OPEN_DELAY))
                        .on_click(move |_, _, cx| {
                            dock_app.update(cx, WorkspaceSurface::toggle_study_editor_dock);
                        }),
                )
                .child(
                    Button::new("study_editor_expand_toggle", theme)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Sm)
                        .icon(header_icon(expand_icon))
                        .aria_label("Toggle editor size")
                        .tooltip(
                            TooltipSpec::new("Toggle editor size", theme)
                                .show_delay(TOOLTIP_OPEN_DELAY),
                        )
                        .on_click(move |_, _, cx| {
                            expand_app.update(cx, WorkspaceSurface::toggle_study_editor_expanded);
                        }),
                )
                .child(
                    Button::new("study_editor_close_btn", theme)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Sm)
                        .icon(header_icon(HugeIcon::Close))
                        .aria_label("Close")
                        .tooltip(TooltipSpec::new("Close", theme).show_delay(TOOLTIP_OPEN_DELAY))
                        .on_click(move |_, _, cx| {
                            close_app.update(cx, WorkspaceSurface::close_study_editor_dialog);
                        }),
                ),
        )
        .into_any_element()
}

fn editor_action_bar(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let has_selection = editor.selected_id.is_some();
    let is_busy = editor.busy_label.is_some();
    let script_title = editor
        .selected_id
        .clone()
        .unwrap_or_else(|| "Select script".to_string());
    let scripts_open = editor.popover == StudyEditorPopover::Scripts;
    let more_open = editor.popover == StudyEditorPopover::More;

    let toggle_scripts = app.clone();
    let add_chart = app.clone();
    let preview = app.clone();
    let save = app.clone();
    let toggle_more = app.clone();
    let hover_bg = gpui_color(colors.hover_bg);

    let script_pill = div()
        .id("study_editor_script_menu_btn")
        .h(px(30.0))
        .max_w(px(230.0))
        .px_2p5()
        .rounded(px(6.0))
        .flex()
        .items_center()
        .gap_1p5()
        .cursor_pointer()
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(gpui_color(colors.text_default))
        .when(scripts_open, |el| el.bg(gpui_color(colors.active_bg)))
        .hover(move |style| style.bg(hover_bg))
        .on_click(move |_, _, cx| {
            toggle_scripts.update(cx, |surface, surface_cx| {
                surface.toggle_study_editor_popover(StudyEditorPopover::Scripts, surface_cx);
            });
            cx.stop_propagation();
        })
        .child(div().truncate().child(script_title))
        .child(
            header_icon(if scripts_open {
                HugeIcon::ChevronUp
            } else {
                HugeIcon::ChevronDown
            })
            .with_size(px(13.0)),
        );

    div()
        .h(px(44.0))
        .px_3()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(gpui_color(colors.border_subtle))
        .child(script_pill)
        .child(
            Button::new("study_editor_add_chart", theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .icon(header_icon(HugeIcon::Play))
                .label("Add to chart")
                .disabled(!has_selection || is_busy)
                .on_click(move |_, _, cx| {
                    add_chart.update(cx, WorkspaceSurface::add_study_editor_project_to_chart);
                }),
        )
        .child(
            Button::new("study_editor_preview", theme)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Sm)
                .label("Preview")
                .disabled(!has_selection || is_busy)
                .on_click(move |_, _, cx| {
                    preview.update(cx, WorkspaceSurface::run_study_editor_preview_test);
                }),
        )
        .child(div().flex_1())
        .child(
            Button::new("study_editor_save", theme)
                .variant(ButtonVariant::Primary)
                .button_size(ButtonSize::Sm)
                .label("Save")
                .disabled(!has_selection || is_busy)
                .on_click(move |_, _, cx| {
                    save.update(cx, WorkspaceSurface::validate_and_save_study_editor_project);
                }),
        )
        .child(
            Button::new("study_editor_more_btn", theme)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Sm)
                .icon(header_icon(HugeIcon::More))
                .aria_label("Editor options")
                .selected(more_open)
                .on_click(move |_, _, cx| {
                    toggle_more.update(cx, |surface, surface_cx| {
                        surface.toggle_study_editor_popover(StudyEditorPopover::More, surface_cx);
                    });
                    cx.stop_propagation();
                }),
        )
        .into_any_element()
}

fn scripts_popover(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let has_selection = editor.selected_id.is_some();
    let is_busy = editor.busy_label.is_some();

    let save_app = app.clone();
    let dup_app = app.clone();
    let rev_app = app.clone();
    let copy_app = app.clone();
    let import_app = app.clone();
    let del_app = app.clone();

    let mut templates = div().flex().flex_wrap().gap_1();
    for (idx, (template, label)) in TEMPLATES.iter().enumerate() {
        let create_app = app.clone();
        let template = *template;
        templates = templates.child(
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

    let mut scripts_list = div()
        .id("study_editor_scripts_list")
        .max_h(px(200.0))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_0p5();
    let hover_bg = gpui_color(colors.hover_bg);
    for (idx, project) in editor.library.iter().enumerate() {
        let id = project.manifest.identifier.clone();
        let selected = editor.selected_id.as_deref() == Some(id.as_str());
        let select_app = app.clone();
        let click_id = id.clone();
        scripts_list = scripts_list.child(
            div()
                .id(("study_editor_script_item", idx))
                .w_full()
                .h(px(30.0))
                .px_2p5()
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_between()
                .cursor_pointer()
                .text_xs()
                .when(selected, |row| {
                    row.bg(gpui_color(colors.primary_subtle))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(gpui_color(colors.text_default))
                })
                .when(!selected, |row| {
                    row.text_color(gpui_color(colors.text_secondary))
                        .hover(move |style| style.bg(hover_bg))
                })
                .on_click(move |_, window, cx| {
                    select_app.update(cx, |surface, surface_cx| {
                        surface.select_study_editor_project(click_id.clone(), window, surface_cx);
                    });
                    cx.stop_propagation();
                })
                .child(div().truncate().child(id))
                .when(selected, |row| {
                    row.child(header_icon(HugeIcon::CheckIcon).with_size(px(13.0)))
                }),
        );
    }

    div()
        .id("study_editor_scripts_popover")
        .absolute()
        .top(px(76.0))
        .left(px(12.0))
        .w(px(292.0))
        .max_h(px(460.0))
        .overflow_y_scroll()
        .p_2()
        .rounded(px(10.0))
        .bg(gpui_color(colors.surface))
        .border_1()
        .border_color(gpui_color(colors.border))
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(popover_menu_row(
            "se_menu_save",
            "Save & compile",
            Some(HugeIcon::CheckIcon),
            false,
            !has_selection || is_busy,
            theme,
            move |_, cx| {
                save_app.update(cx, WorkspaceSurface::validate_and_save_study_editor_project);
            },
        ))
        .child(popover_menu_row(
            "se_menu_dup",
            "Make a copy...",
            Some(HugeIcon::Copy),
            false,
            !has_selection,
            theme,
            move |window, cx| {
                dup_app.update(cx, |surface, surface_cx| {
                    surface.duplicate_study_editor_project(window, surface_cx);
                });
            },
        ))
        .child(popover_menu_row(
            "se_menu_revert",
            tr!(panels::STUDY_EDITOR_REVERT),
            Some(HugeIcon::Refresh),
            false,
            !has_selection,
            theme,
            move |window, cx| {
                rev_app.update(cx, |surface, surface_cx| {
                    surface.revert_study_editor_project(window, surface_cx);
                });
            },
        ))
        .child(popover_menu_row(
            "se_menu_copy_bundle",
            "Copy bundle to clipboard",
            Some(HugeIcon::Copy),
            false,
            !has_selection,
            theme,
            move |_, cx| {
                copy_app.update(cx, WorkspaceSurface::export_study_editor_bundle);
            },
        ))
        .child(popover_menu_row(
            "se_menu_import",
            "Import bundle from clipboard",
            Some(HugeIcon::Download),
            false,
            false,
            theme,
            move |window, cx| {
                import_app.update(cx, |surface, surface_cx| {
                    surface.import_study_editor_bundle(window, surface_cx);
                });
            },
        ))
        .child(popover_menu_row(
            "se_menu_delete",
            tr!(panels::REMOVE),
            Some(HugeIcon::Trash),
            true,
            !has_selection,
            theme,
            move |window, cx| {
                del_app.update(cx, |surface, surface_cx| {
                    surface.delete_study_editor_project(window, surface_cx);
                });
            },
        ))
        .child(
            div()
                .my_1()
                .border_t_1()
                .border_color(gpui_color(colors.border_subtle)),
        )
        .child(
            div()
                .px_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_muted))
                .child("CREATE NEW"),
        )
        .child(
            div()
                .h(px(28.0))
                .px_1()
                .child(Input::new(&editor.new_id_input).platform(theme)),
        )
        .child(div().px_1().pt_0p5().child(templates))
        .child(
            div()
                .my_1()
                .border_t_1()
                .border_color(gpui_color(colors.border_subtle)),
        )
        .child(
            div()
                .px_1()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_muted))
                .child("SCRIPTS"),
        )
        .child(scripts_list)
        .into_any_element()
}

fn more_popover(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let is_busy = editor.busy_label.is_some();
    let find_app = app.clone();
    let snip_app = app.clone();
    let cons_app = app.clone();
    let ln_app = app.clone();
    let wrap_app = app.clone();
    let recheck_app = app.clone();

    let mut font_sizes = div().flex().items_center().gap_1().px_2().py_1();
    for (px_label, size) in [(12usize, 12.0_f32), (13, 13.0), (14, 14.0)] {
        let active = (editor.font_size - size).abs() < 0.5;
        let f_app = app.clone();
        font_sizes = font_sizes.child(
            Button::new(("se_font_sz", px_label), theme)
                .variant(if active {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Secondary
                })
                .button_size(ButtonSize::Sm)
                .label(format!("{px_label}px"))
                .on_click(move |_, _, cx| {
                    f_app.update(cx, |surface, surface_cx| {
                        surface.set_study_editor_font_size(size, surface_cx);
                    });
                }),
        );
    }

    let mut menu = div()
        .id("study_editor_more_popover")
        .absolute()
        .top(px(76.0))
        .right(px(12.0))
        .w(px(252.0))
        .max_h(px(420.0))
        .overflow_y_scroll()
        .p_2()
        .rounded(px(10.0))
        .bg(gpui_color(colors.surface))
        .border_1()
        .border_color(gpui_color(colors.border))
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .px_2()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_muted))
                .child("TOOLS"),
        )
        .child(popover_menu_row(
            "se_more_find",
            if editor.find_open {
                "Hide Find bar"
            } else {
                "Find in code"
            },
            Some(HugeIcon::Search),
            false,
            false,
            theme,
            move |_, cx| {
                find_app.update(cx, WorkspaceSurface::toggle_study_editor_find);
            },
        ))
        .child(popover_menu_row(
            "se_more_snippets",
            if editor.snippets_open {
                "Hide Snippets bar"
            } else {
                "Show Snippets & API"
            },
            Some(HugeIcon::Add),
            false,
            false,
            theme,
            move |_, cx| {
                snip_app.update(cx, WorkspaceSurface::toggle_study_editor_snippets);
            },
        ))
        .child(popover_menu_row(
            "se_more_console",
            if editor.console_open {
                "Hide Diagnostics console"
            } else {
                "Show Diagnostics console"
            },
            Some(HugeIcon::Info),
            false,
            false,
            theme,
            move |_, cx| {
                cons_app.update(cx, WorkspaceSurface::toggle_study_editor_console);
            },
        ))
        .child(
            div()
                .my_1()
                .border_t_1()
                .border_color(gpui_color(colors.border_subtle)),
        )
        .child(
            div()
                .px_2()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_muted))
                .child("EDITOR SETTINGS"),
        )
        .child(popover_menu_row(
            "se_more_ln",
            if editor.line_numbers {
                "✓ Line numbers"
            } else {
                "   Line numbers"
            },
            None,
            false,
            false,
            theme,
            move |_, cx| {
                ln_app.update(cx, WorkspaceSurface::toggle_study_editor_line_numbers);
            },
        ))
        .child(popover_menu_row(
            "se_more_wrap",
            if editor.word_wrap {
                "✓ Word wrap"
            } else {
                "   Word wrap"
            },
            None,
            false,
            false,
            theme,
            move |window, cx| {
                wrap_app.update(cx, |surface, surface_cx| {
                    surface.toggle_study_editor_word_wrap(window, surface_cx);
                });
            },
        ))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_2()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child("Font size")
                .child(font_sizes),
        )
        .child(
            div()
                .my_1()
                .border_t_1()
                .border_color(gpui_color(colors.border_subtle)),
        )
        .child(
            div()
                .px_2()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.text_muted))
                .child("DEV PACK"),
        );

    if matches!(
        editor.dev_pack_status,
        Some(StudyDevPackStatus::MissingSandboxTarget { .. })
    ) {
        let install_app = app.clone();
        menu = menu.child(popover_menu_row(
            "se_more_install_target",
            "Install wasm32 target",
            Some(HugeIcon::Download),
            false,
            is_busy,
            theme,
            move |_, cx| {
                install_app.update(cx, WorkspaceSurface::install_study_editor_dev_pack_target);
            },
        ));
    }

    menu.child(popover_menu_row(
        "se_more_recheck",
        "Re-check Rust toolchain",
        Some(HugeIcon::Refresh),
        false,
        is_busy,
        theme,
        move |_, cx| {
            recheck_app.update(cx, WorkspaceSurface::clean_study_editor_cache);
        },
    ))
    .into_any_element()
}

fn find_strip(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let find_app = app.clone();
    let close_app = app.clone();
    div()
        .h(px(36.0))
        .px_3()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .bg(gpui_color(colors.surface_secondary))
        .border_b_1()
        .border_color(gpui_color(colors.border_subtle))
        .child(
            div()
                .flex_1()
                .h(px(26.0))
                .child(Input::new(&editor.search_input).platform(theme).flex_1()),
        )
        .child(
            Button::new("study_editor_find_next", theme)
                .variant(ButtonVariant::Secondary)
                .button_size(ButtonSize::Sm)
                .label("Next")
                .on_click(move |_, window, cx| {
                    find_app.update(cx, |surface, surface_cx| {
                        surface.find_in_study_editor(window, surface_cx);
                    });
                }),
        )
        .child(
            Button::new("study_editor_find_close", theme)
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::Sm)
                .icon(header_icon(HugeIcon::Close))
                .on_click(move |_, _, cx| {
                    close_app.update(cx, WorkspaceSurface::toggle_study_editor_find);
                }),
        )
        .into_any_element()
}

fn snippets_strip(app: &Entity<WorkspaceSurface>, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let mut row = div().flex().flex_wrap().items_center().gap_1();
    for (idx, snippet) in STUDY_SNIPPETS.iter().enumerate() {
        let insert_app = app.clone();
        let body = snippet.body;
        let doc = snippet.doc;
        row = row.child(
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
    div()
        .px_3()
        .py_1p5()
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .bg(gpui_color(colors.surface_secondary))
        .border_b_1()
        .border_color(gpui_color(colors.border_subtle))
        .child(row)
        .child(
            div()
                .text_xs()
                .truncate()
                .text_color(gpui_color(colors.text_muted))
                .child(STUDY_API_QUICK_REF),
        )
        .into_any_element()
}

fn diagnostics_drawer(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let close_app = app.clone();
    let mut list = div()
        .id("study_editor_diag_list")
        .max_h(px(120.0))
        .overflow_y_scroll()
        .px_3()
        .py_1p5()
        .flex()
        .flex_col()
        .gap_1();

    if let Some(busy) = &editor.busy_label {
        list = list.child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui_color(colors.ring_primary))
                .child(busy.clone()),
        );
    } else if let Some(status) = &editor.status_message {
        list = list.child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(status.clone()),
        );
    }

    for (idx, diag) in editor.diagnostics.iter().take(12).enumerate() {
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
        list = list.child(
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

    div()
        .flex_none()
        .flex()
        .flex_col()
        .bg(gpui_color(colors.surface_secondary))
        .border_t_1()
        .border_color(gpui_color(colors.border_subtle))
        .child(
            div()
                .h(px(26.0))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(gpui_color(colors.border_subtle))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(gpui_color(colors.text_secondary))
                        .child(if editor.diagnostics.is_empty() {
                            "Console".to_string()
                        } else {
                            format!("Console ({})", editor.diagnostics.len())
                        }),
                )
                .child(
                    Button::new("study_editor_console_close", theme)
                        .variant(ButtonVariant::Ghost)
                        .button_size(ButtonSize::Sm)
                        .icon(header_icon(HugeIcon::Close))
                        .on_click(move |_, _, cx| {
                            close_app.update(cx, WorkspaceSurface::toggle_study_editor_console);
                        }),
                ),
        )
        .child(list)
        .into_any_element()
}

fn editor_status_bar(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    let (line, col) = editor.source_input.read(cx).cursor_line_col();
    let toggle_console = app.clone();
    let (status_text, status_color) = if let Some(busy) = &editor.busy_label {
        (busy.clone(), gpui_color(colors.ring_primary))
    } else if !editor.diagnostics.is_empty() {
        (
            format!("{} issue(s)", editor.diagnostics.len()),
            gpui_color(colors.text_danger),
        )
    } else if let Some(msg) = &editor.status_message {
        (msg.clone(), gpui_color(colors.text_secondary))
    } else {
        ("Ready".to_string(), gpui_color(colors.text_muted))
    };

    let toolchain = match &editor.dev_pack_status {
        Some(StudyDevPackStatus::Ready { rustc_version, .. }) => rustc_version
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" "),
        Some(StudyDevPackStatus::MissingSandboxTarget { .. }) => "Missing wasm32".to_string(),
        Some(StudyDevPackStatus::Unavailable) => "No rustc".to_string(),
        None => "Checking...".to_string(),
    };

    div()
        .h(px(24.0))
        .px_3()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .bg(gpui_color(colors.surface_secondary))
        .border_t_1()
        .border_color(gpui_color(colors.border_subtle))
        .child(
            div()
                .id("study_editor_status_btn")
                .flex_1()
                .min_w_0()
                .truncate()
                .cursor_pointer()
                .text_xs()
                .text_color(status_color)
                .on_click(move |_, _, cx| {
                    toggle_console.update(cx, WorkspaceSurface::toggle_study_editor_console);
                })
                .child(status_text),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_3()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(format!("Ln {line}, Col {col}"))
                .child(format!("Rust WASM · {toolchain}")),
        )
        .into_any_element()
}

pub(super) fn study_editor_docked_panel(
    app: &Entity<WorkspaceSurface>,
    editor: &StudyEditorDialogState,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let colors = theme.colors;
    let has_selection = editor.selected_id.is_some();
    let show_console = editor.console_open || !editor.diagnostics.is_empty();
    let dismiss_popover = app.clone();

    let code_canvas = if has_selection {
        TextArea::code_editor(&editor.source_input, theme)
            .syntax_highlighter(editor.highlighter.clone())
            .line_numbers(editor.line_numbers)
            .font_size(px(editor.font_size))
            .into_any_element()
    } else {
        div()
            .flex_1()
            .w_full()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(gpui_color(colors.text_secondary))
            .child(tr!(panels::STUDY_EDITOR_NO_PROJECTS))
            .into_any_element()
    };

    let popover_el = match editor.popover {
        StudyEditorPopover::None => None,
        StudyEditorPopover::Scripts => Some(scripts_popover(app, editor, theme)),
        StudyEditorPopover::More => Some(more_popover(app, editor, theme)),
    };

    let mut panel = div()
        .id("study_editor_docked_panel")
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .when(editor.popover != StudyEditorPopover::None, |el| {
            el.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                dismiss_popover.update(cx, |surface, surface_cx| {
                    surface.toggle_study_editor_popover(StudyEditorPopover::None, surface_cx);
                });
            })
        });

    panel = match editor.dock {
        StudyEditorDock::Right => {
            let hover_resize = gpui_color(colors.ring_primary).opacity(0.45);
            panel
                .w(px(editor.width))
                .h_full()
                .border_l_1()
                .border_color(gpui_color(colors.border))
                .child(
                    div()
                        .id("study_editor_width_handle")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left_0()
                        .w(px(4.0))
                        .cursor(gpui::CursorStyle::ResizeLeftRight)
                        .hover(move |style| style.bg(hover_resize))
                        .on_drag(StudyEditorWidthDrag, |drag, _, _, cx| {
                            cx.new(|_| drag.clone())
                        }),
                )
        }
        StudyEditorDock::Bottom => {
            let hover_resize = gpui_color(colors.ring_primary).opacity(0.45);
            panel
                .w_full()
                .h(px(editor.height))
                .border_t_1()
                .border_color(gpui_color(colors.border))
                .child(
                    div()
                        .id("study_editor_height_handle")
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(4.0))
                        .cursor(gpui::CursorStyle::ResizeUpDown)
                        .hover(move |style| style.bg(hover_resize))
                        .on_drag(StudyEditorHeightDrag, |drag, _, _, cx| {
                            cx.new(|_| drag.clone())
                        }),
                )
        }
    };

    panel
        .child(editor_top_bar(app, editor, theme))
        .child(editor_action_bar(app, editor, theme))
        .when(editor.find_open, |el| {
            el.child(find_strip(app, editor, theme))
        })
        .when(editor.snippets_open, |el| {
            el.child(snippets_strip(app, theme))
        })
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(code_canvas),
        )
        .when(show_console, |el| {
            el.child(diagnostics_drawer(app, editor, theme))
        })
        .child(editor_status_bar(app, editor, theme, cx))
        .children(popover_el)
        .into_any_element()
}
