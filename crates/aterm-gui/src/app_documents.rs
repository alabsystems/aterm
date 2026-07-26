// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Markdown/editor host integration.
//!
//! Local files enter through one host-minted grant. Canonical bytes then live in
//! `DocumentStore`; Markdown and editor app instances retain only `DocumentId`,
//! while each presentation gets its own stable view state.

use std::collections::{BTreeMap, BTreeSet};

use crate::document_store::{DocumentId, DocumentViewId};
use crate::native_app::{
    AppEvent, AppKind, AppViewState, EditorApp, EventResult, MarkdownApp, MarkdownViewState,
    NativeApp, TextInputEvent,
};
use crate::native_document_host::{
    DEFAULT_DOCUMENT_LIMIT, DocumentGrant, DocumentGrantStore, DocumentPersistenceStore,
    GrantAccess,
};
use crate::native_document_journal::{
    DocumentJournalStore, JournalCompletion, JournalEffect, JournalRewriteGeneration,
    JournalRewritePlan, JournalRewriteResult, execute_journal_append, execute_journal_rewrite,
};
use crate::native_editor::{EditorCommand, EditorEffect, Selection};
use crate::{App, WindowId};

fn config_search_range(source: &str, query: &str) -> Option<std::ops::Range<usize>> {
    if query.is_ascii() {
        let needle = query.as_bytes();
        return (!needle.is_empty())
            .then(|| {
                source
                    .as_bytes()
                    .windows(needle.len())
                    .position(|candidate| candidate.eq_ignore_ascii_case(needle))
            })
            .flatten()
            .map(|start| start..start + needle.len());
    }
    source
        .find(query)
        .map(|start| start..start.saturating_add(query.len()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigEditorRevealDecision {
    Select {
        requested: String,
        range: std::ops::Range<usize>,
    },
    SeedSearch {
        requested: String,
    },
}

/// Resolve only an in-document Settings → Manual target. Filesystem authority
/// is deliberately absent from [`crate::native_app::ConfigEditorTarget`]; the
/// host has already opened its canonical `aterm.toml` before calling this.
pub(crate) fn config_editor_reveal_decision(
    source: &str,
    target: &crate::native_app::ConfigEditorTarget,
) -> Result<ConfigEditorRevealDecision, String> {
    const MAX_TARGET_BYTES: usize = 512;

    let requested = match target {
        crate::native_app::ConfigEditorTarget::Key(key)
        | crate::native_app::ConfigEditorTarget::Search(key) => key.trim(),
    };
    if requested.is_empty() || requested.len() > MAX_TARGET_BYTES {
        return Err("Manual reveal target is empty or too long".to_string());
    }
    let direct = match target {
        crate::native_app::ConfigEditorTarget::Key(key) => {
            crate::native_config_language::config_key_source_range(source, key)
        }
        crate::native_app::ConfigEditorTarget::Search(query) => config_search_range(source, query),
    };
    Ok(direct.map_or_else(
        || ConfigEditorRevealDecision::SeedSearch {
            requested: requested.to_string(),
        },
        |range| ConfigEditorRevealDecision::Select {
            requested: requested.to_string(),
            range,
        },
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingDocumentClose {
    window: WindowId,
    tab: crate::tab_model::TabId,
    views: Vec<DocumentViewId>,
    whole_tab: bool,
}

/// Latest explicit durability intent received while an older generation is in
/// flight.  The document owns this latch rather than the initiating view: a
/// repeated Save or a close/Quit barrier must survive view focus changes and be
/// pumped immediately after the current atomic save reduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingDocumentSaveIntent {
    seq: aterm_buffer::Seq,
    source_view: crate::tab_model::ViewId,
}

/// One document's leaves inside a larger window/quit close plan.  The plan is
/// immutable once armed: `DocumentStore::prepare_close` freezes the exact head
/// sequence and editing is rejected until durability either succeeds or the
/// close is explicitly retried.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedDocumentClose {
    document: DocumentId,
    views: Vec<DocumentViewId>,
    source_view: crate::tab_model::ViewId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DocumentClosePlan {
    documents: Vec<PlannedDocumentClose>,
}

enum NativeDocumentJob {
    Save {
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        grant: DocumentGrant,
        plan: crate::native_document_io::SavePlan,
        config_themes: Option<std::sync::Arc<crate::app_config::ThemeCatalog>>,
        proxy: winit::event_loop::EventLoopProxy<crate::Wake>,
    },
    JournalAppend {
        document: DocumentId,
        path: std::path::PathBuf,
        key: crate::native_document_io::JournalDocumentKey,
        plan: crate::native_document_io::JournalAppendPlan,
        proxy: winit::event_loop::EventLoopProxy<crate::Wake>,
    },
    JournalRewrite {
        document: DocumentId,
        path: std::path::PathBuf,
        plan: JournalRewritePlan,
        proxy: winit::event_loop::EventLoopProxy<crate::Wake>,
    },
}

fn native_document_queue() -> Result<&'static std::sync::mpsc::Sender<NativeDocumentJob>, String> {
    static QUEUE: std::sync::OnceLock<Result<std::sync::mpsc::Sender<NativeDocumentJob>, String>> =
        std::sync::OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::channel::<NativeDocumentJob>();
            std::thread::Builder::new()
                .name("aterm-native-document".to_string())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        match job {
                            NativeDocumentJob::Save {
                                document,
                                source_view,
                                grant,
                                plan,
                                config_themes,
                                proxy,
                            } => {
                                let result = crate::native_document_host::execute_granted_save(
                                    &grant, &plan,
                                );
                                let config_observation = prepare_config_save_observation(
                                    &grant,
                                    &plan,
                                    &result,
                                    config_themes,
                                );
                                let _ = proxy.send_event(crate::Wake::NativeDocumentSaved {
                                    document,
                                    source_view,
                                    generation: plan.generation,
                                    saved_bytes: plan.bytes,
                                    result,
                                    config_observation,
                                });
                            }
                            NativeDocumentJob::JournalAppend {
                                document,
                                path,
                                key,
                                plan,
                                proxy,
                            } => {
                                let result = execute_journal_append(&path, key, &plan);
                                let _ = proxy.send_event(crate::Wake::NativeDocumentJournaled {
                                    document,
                                    generation: plan.generation,
                                    result,
                                });
                            }
                            NativeDocumentJob::JournalRewrite {
                                document,
                                path,
                                plan,
                                proxy,
                            } => {
                                let result = execute_journal_rewrite(&path, &plan);
                                let _ =
                                    proxy.send_event(crate::Wake::NativeDocumentJournalRewritten {
                                        document,
                                        generation: plan.generation,
                                        result,
                                    });
                            }
                        }
                    }
                })
                .map_err(|error| format!("could not start document worker: {error}"))?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn prepare_config_save_observation(
    grant: &DocumentGrant,
    plan: &crate::native_document_io::SavePlan,
    result: &crate::native_document_io::AtomicSaveResult,
    themes: Option<std::sync::Arc<crate::app_config::ThemeCatalog>>,
) -> Option<Result<crate::native_config_service::PreparedConfigObservation, String>> {
    let themes = themes?;
    let crate::native_document_io::AtomicSaveResult::Committed(proof) = result else {
        return None;
    };
    let text = std::str::from_utf8(&plan.bytes)
        .map(str::to_owned)
        .map_err(|_| "saved config bytes were not UTF-8".to_string());
    Some(text.and_then(|text| {
        crate::native_config_service::VersionedConfigService::prepare_observation(
            crate::native_config_service::ConfigDiskObservation {
                text,
                baseline: crate::native_document_host::AtomicFileBaseline {
                    target: grant.target().clone(),
                    observed: proof.observed,
                },
            },
            themes,
        )
    }))
}

pub(crate) struct DocumentHostRuntime {
    pub(crate) grants: DocumentGrantStore,
    pub(crate) persistence: DocumentPersistenceStore,
    journals: Option<DocumentJournalStore>,
    journal_unavailable: Option<String>,
    recovery_status: BTreeMap<DocumentId, String>,
    /// Documents whose on-disk generation no longer matches the saver baseline.
    /// This host-owned latch blocks every save entry point until an explicit
    /// reload/reconciliation installs a fresh stable observation.
    disk_conflicts: BTreeSet<DocumentId>,
    inflight: BTreeSet<DocumentId>,
    pending_saves: BTreeMap<DocumentId, PendingDocumentSaveIntent>,
    pending_closes: BTreeMap<DocumentId, Vec<PendingDocumentClose>>,
    /// OS-window closes are all-or-nothing across every document leaf in that
    /// window.  No tab/view is detached while any member still lacks its exact
    /// durable checkpoint.
    pending_window_closes: BTreeMap<WindowId, DocumentClosePlan>,
    /// Whole-app Quit waits on one process-wide plan before update application or
    /// event-loop exit. It supersedes (without partially applying) window plans.
    pending_quit: Option<DocumentClosePlan>,
}

impl Default for DocumentHostRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentHostRuntime {
    pub(crate) fn new() -> Self {
        let (journals, journal_unavailable) = match DocumentJournalStore::system_default() {
            Ok(store) => (Some(store), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            grants: DocumentGrantStore::new(),
            persistence: DocumentPersistenceStore::default(),
            journals,
            journal_unavailable,
            recovery_status: BTreeMap::new(),
            disk_conflicts: BTreeSet::new(),
            inflight: BTreeSet::new(),
            pending_saves: BTreeMap::new(),
            pending_closes: BTreeMap::new(),
            pending_window_closes: BTreeMap::new(),
            pending_quit: None,
        }
    }
}

impl App {
    /// Resolve and ensure the one process config file, then focus it in the
    /// existing native Editor app. This host seam owns path authority; Settings
    /// reducers emit only `OpenConfigEditor` and never receive a filesystem path.
    pub(crate) fn ensure_and_open_config_editor_in_window(
        &mut self,
        wid: WindowId,
    ) -> Result<String, String> {
        self.ensure_and_open_config_editor_at_in_window(wid, None)
    }

    pub(crate) fn ensure_and_open_config_editor_at_in_window(
        &mut self,
        wid: WindowId,
        target: Option<&crate::native_app::ConfigEditorTarget>,
    ) -> Result<String, String> {
        let path = crate::app_config::config_path().ok_or_else(|| {
            "could not resolve the config path (HOME/XDG_CONFIG_HOME unset)".to_string()
        })?;
        self.ensure_and_open_config_editor_path_at_in_window(wid, &path, target)
    }

    /// Path-injected form for deterministic host tests. Production callers use
    /// `ensure_and_open_config_editor_in_window`, which alone resolves process
    /// configuration; both paths cross the same grant and tab-host boundaries.
    #[cfg(test)]
    pub(crate) fn ensure_and_open_config_editor_path_in_window(
        &mut self,
        wid: WindowId,
        path: &std::path::Path,
    ) -> Result<String, String> {
        self.ensure_and_open_config_editor_path_at_in_window(wid, path, None)
    }

    pub(crate) fn ensure_and_open_config_editor_path_at_in_window(
        &mut self,
        wid: WindowId,
        path: &std::path::Path,
        target: Option<&crate::native_app::ConfigEditorTarget>,
    ) -> Result<String, String> {
        let logical_path = ensure_config_file(path)?;
        let uri = crate::native_document_host::path_to_file_uri(&logical_path)
            .map_err(|error| format!("could not address aterm.toml: {error}"))?;
        self.open_document_tab_in_window_with_config_symlinks(wid, AppKind::Editor, &uri, true)?;
        let document = self
            .active_native_view(wid)
            .and_then(|(instance, _)| self.native_runtime.document_id(instance))
            .ok_or_else(|| {
                "native config editor did not retain its canonical document".to_string()
            })?;
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "native config document disappeared after open".to_string())?;
        let revision = self
            .document_store
            .revision(document)
            .ok_or_else(|| "native config document revision disappeared after open".to_string())?;
        if self.native_config_service.bound_logical_path().is_none() {
            let grant_id = self
                .native_documents
                .persistence
                .grant(document)
                .ok_or_else(|| "native config document grant disappeared after open".to_string())?;
            let grant =
                self.native_documents.grants.get(grant_id).ok_or_else(|| {
                    "native config document grant disappeared after open".to_string()
                })?;
            let observed = self
                .native_documents
                .persistence
                .observed(document)
                .ok_or_else(|| {
                    "native config document baseline disappeared after open".to_string()
                })?;
            self.native_config_service.bind_unparsed_disk_baseline(
                crate::native_document_host::AtomicFileBaseline {
                    target: grant.target().clone(),
                    observed,
                },
            )?;
        }
        if !self
            .native_runtime
            .enable_config_editor(document, snapshot.text.as_ref(), revision)
        {
            return Err("native config editor controller disappeared after open".to_string());
        }
        if let Some(target) = target {
            self.reveal_config_editor_target(wid, document, target)?;
        }
        self.request_config_host_diagnostics(document);
        for (window, instance, view) in self.document_native_views(document) {
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
        Ok(uri)
    }

    fn reveal_config_editor_target(
        &mut self,
        wid: WindowId,
        document: DocumentId,
        target: &crate::native_app::ConfigEditorTarget,
    ) -> Result<(), String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "native config editor is not focused after open".to_string())?;
        if self.native_runtime.document_id(instance) != Some(document) {
            return Err("focused native editor does not own aterm.toml".to_string());
        }
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "native config document disappeared during reveal".to_string())?;
        let decision = config_editor_reveal_decision(&snapshot.text, target)?;
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return Err("native config editor view disappeared during reveal".to_string());
        };
        let buffer = state
            .buffer
            .as_mut()
            .ok_or_else(|| "native config editor buffer disappeared during reveal".to_string())?;
        state.config_completion_selected = 0;
        state.config_completion_interaction = None;
        state.config_completion_dismissed = None;
        match decision {
            ConfigEditorRevealDecision::Select { requested, range } => {
                buffer.selections = vec![crate::native_editor::Selection {
                    anchor: range.start,
                    head: range.end,
                }];
                buffer.primary = 0;
                buffer.minibuffer = crate::native_editor::Minibuffer::Inactive;
                buffer.ensure_primary_visible(&snapshot.text, buffer.viewport_lines());
                state.status = Some(format!("Revealed {requested} in aterm.toml"));
            }
            ConfigEditorRevealDecision::SeedSearch { requested } => {
                let origin = buffer.primary_selection().head.min(snapshot.text.len());
                buffer.minibuffer = crate::native_editor::Minibuffer::Search {
                    query: requested.clone(),
                    origin,
                };
                state.status = Some(format!(
                    "{requested} is not authored yet · search is ready; completion can insert it"
                ));
            }
        }
        Ok(())
    }

    /// Apply the exact stable disk observation that the reload host will parse
    /// and offer to the config service. Malformed-but-UTF-8 TOML still reaches a
    /// clean Manual buffer (and its diagnostics) while live Config rejects it.
    /// Dirty Manual bytes remain untouched. A symlink target change is surfaced
    /// as a reopen-required conflict rather than rebinding an existing saver.
    pub(crate) fn refresh_open_config_editor_observation(
        &mut self,
        observation: &crate::native_config_service::ConfigDiskObservation,
    ) -> Result<(), String> {
        let Some(document) = self.native_runtime.config_editor_document() else {
            return Ok(());
        };
        let grant_id = self
            .native_documents
            .persistence
            .grant(document)
            .ok_or_else(|| "config document persistence grant disappeared".to_string())?;
        let grant = self
            .native_documents
            .grants
            .get(grant_id)
            .ok_or_else(|| "config document grant disappeared".to_string())?;
        let binding_valid = grant.validate_current_binding().is_ok();
        let same_target = grant.target().target_path() == observation.baseline.target.target_path();
        let same_logical = grant.targets_logical_path(observation.baseline.target.logical_path());
        if !binding_valid || !same_target || !same_logical {
            let message =
                "aterm.toml's file binding changed; Save is blocked until Manual is reopened"
                    .to_string();
            self.set_document_disk_conflict(document, true);
            self.set_document_recovery_status(document, Some(message.clone()));
            return Err(message);
        }
        self.reduce_document_observation(
            document,
            &observation.text,
            observation.baseline.observed,
        )?;
        Ok(())
    }

    pub(crate) fn editor_config_completion(
        &self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        action: &str,
    ) -> Option<crate::native_config_language::ConfigCompletionEdit> {
        if !self.native_runtime.config_editor_enabled(instance) {
            return None;
        }
        let context = self.editor_config_completion_context(instance, view)?;
        let document = self.native_runtime.document_id(instance)?;
        let snapshot = self.document_store.snapshot(document)?;
        let analysis = self.native_runtime.config_editor_analysis(instance)?;
        crate::native_config_language::resolve_config_completion_action_with_analysis(
            &snapshot.text,
            context,
            action,
            analysis,
        )
    }

    pub(crate) fn editor_config_completion_context(
        &self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::native_config_language::ConfigCompletionContext> {
        if !self.native_runtime.config_editor_enabled(instance) {
            return None;
        }
        let document = self.native_runtime.document_id(instance)?;
        let snapshot = self.document_store.snapshot(document)?;
        let caret = match self.native_runtime.view_state(view)? {
            AppViewState::Editor(state) => state.buffer.as_ref()?.primary_selection().head,
            _ => return None,
        };
        Some(crate::native_config_language::ConfigCompletionContext::new(
            document.get(),
            snapshot.seq.0,
            caret,
        ))
    }

    /// Exact current Manual assistance for keyboard dispatch. This resolves
    /// from the worker-authored analysis index and fills the same bounded cache
    /// paint uses; no intervening render is required for Tab, Ctrl-Space, or
    /// Escape to observe the current caret.
    pub(crate) fn editor_config_assist(
        &self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
    ) -> Option<(
        crate::native_config_language::ConfigCompletionContext,
        crate::native_config_language::ConfigAssist,
    )> {
        if !self.native_runtime.config_editor_enabled(instance) {
            return None;
        }
        let document = self.native_runtime.document_id(instance)?;
        let snapshot = self.document_store.snapshot(document)?;
        let caret = match self.native_runtime.view_state(view)? {
            AppViewState::Editor(state) => state.buffer.as_ref()?.primary_selection().head,
            _ => return None,
        };
        self.native_runtime
            .config_editor_assist(instance, &snapshot, caret)
    }

    /// Route editor-native input into the canonical document workspace. Returns
    /// `None` for non-editor apps so their ordinary reducer remains authoritative.
    pub(crate) fn dispatch_editor_event(
        &mut self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        event: &AppEvent,
    ) -> Result<Option<EventResult>, String> {
        let document = match self.native_runtime.app(instance) {
            Some(NativeApp::Editor(editor)) => editor.document,
            _ => return Ok(None),
        };
        if !matches!(
            event,
            AppEvent::InsertText(_)
                | AppEvent::TextInput(_)
                | AppEvent::EditorChord(_)
                | AppEvent::EditorCommand(_)
                | AppEvent::EditorCompletion(_)
                | AppEvent::EditorConfigCompletion(_)
                | AppEvent::EditorConfigCompletionRejected
                | AppEvent::EditorConfigDiagnosticNavigate { .. }
                | AppEvent::EditorSetSelection { .. }
                | AppEvent::EditorViewportChanged { .. }
                | AppEvent::ScrollLines(_)
        ) {
            return Ok(None);
        }

        let config_diagnostic_navigation = match event {
            AppEvent::EditorConfigDiagnosticNavigate { previous } => {
                let analysis = self
                    .native_runtime
                    .config_editor_analysis(instance)
                    .ok_or_else(|| "config diagnostics are unavailable".to_string())?;
                let count = analysis.diagnostic_count();
                let current = match self.native_runtime.view_state(view) {
                    Some(AppViewState::Editor(state)) => state.config_diagnostic_selected,
                    _ => return Err("editor view state disappeared".to_string()),
                };
                let selected = crate::native_app::config_diagnostic_selection_transition(
                    current, count, *previous,
                );
                analysis
                    .diagnostic_at(selected)
                    .map(|diagnostic| (selected, diagnostic.bytes.start))
            }
            _ => None,
        };

        let effects = {
            let (runtime, workspace, store) = (
                &mut self.native_runtime,
                &mut self.editor_workspace,
                &mut self.document_store,
            );
            let Some(AppViewState::Editor(state)) = runtime.view_state_mut(view) else {
                return Err("editor view state disappeared".to_string());
            };
            let Some(buffer) = state.buffer.as_mut() else {
                return Err("editor buffer is not attached".to_string());
            };
            state.preedit.clear();
            if !matches!(
                event,
                AppEvent::ScrollLines(_)
                    | AppEvent::EditorViewportChanged { .. }
                    | AppEvent::TextInput(TextInputEvent::Preedit(_))
            ) {
                state.config_completion_selected = 0;
                state.config_completion_interaction = None;
            }
            let reduced = match event {
                AppEvent::InsertText(text) | AppEvent::TextInput(TextInputEvent::Commit(text)) => {
                    workspace.insert_text(store, buffer, text)
                }
                AppEvent::TextInput(TextInputEvent::Preedit(text)) => {
                    state.preedit.clone_from(text);
                    Ok(Vec::new())
                }
                AppEvent::TextInput(TextInputEvent::Backspace) => {
                    if buffer.minibuffer_active() {
                        workspace.minibuffer_backspace(store, buffer)
                    } else {
                        workspace.execute(store, buffer, EditorCommand::DeleteBackward)
                    }
                }
                AppEvent::TextInput(TextInputEvent::Delete) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        workspace.execute(store, buffer, EditorCommand::DeleteForward)
                    }
                }
                AppEvent::TextInput(TextInputEvent::Left { extend }) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        let anchors = buffer
                            .selections
                            .iter()
                            .map(|selection| selection.anchor)
                            .collect::<Vec<_>>();
                        let result = workspace.execute(store, buffer, EditorCommand::MoveBackward);
                        if *extend {
                            for (selection, anchor) in buffer.selections.iter_mut().zip(anchors) {
                                selection.anchor = anchor;
                            }
                        }
                        result
                    }
                }
                AppEvent::TextInput(TextInputEvent::Right { extend }) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        let anchors = buffer
                            .selections
                            .iter()
                            .map(|selection| selection.anchor)
                            .collect::<Vec<_>>();
                        let result = workspace.execute(store, buffer, EditorCommand::MoveForward);
                        if *extend {
                            for (selection, anchor) in buffer.selections.iter_mut().zip(anchors) {
                                selection.anchor = anchor;
                            }
                        }
                        result
                    }
                }
                AppEvent::TextInput(TextInputEvent::SelectAll) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        let len = store
                            .snapshot(document)
                            .ok_or_else(|| "editor document disappeared".to_string())?
                            .text
                            .len();
                        buffer.selections = vec![Selection {
                            anchor: 0,
                            head: len,
                        }];
                        buffer.primary = 0;
                        Ok(Vec::new())
                    }
                }
                AppEvent::TextInput(TextInputEvent::Undo) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        workspace.execute(store, buffer, EditorCommand::Undo)
                    }
                }
                AppEvent::TextInput(TextInputEvent::Redo) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        workspace.execute(store, buffer, EditorCommand::Redo)
                    }
                }
                AppEvent::TextInput(TextInputEvent::Submit) => {
                    if buffer.minibuffer_active() {
                        workspace.submit_minibuffer(store, buffer)
                    } else {
                        workspace.insert_text(store, buffer, "\n")
                    }
                }
                AppEvent::TextInput(TextInputEvent::Cancel) => {
                    workspace.execute(store, buffer, EditorCommand::Abort)
                }
                AppEvent::EditorChord(chord) => {
                    workspace.handle_chord(store, buffer, chord.clone())
                }
                AppEvent::EditorCommand(command) => workspace.execute(store, buffer, *command),
                AppEvent::EditorCompletion(action) => {
                    workspace.command_completion_action(store, buffer, *action)
                }
                AppEvent::EditorConfigCompletion(completion) => {
                    let snapshot = store
                        .snapshot(document)
                        .ok_or_else(|| "editor document disappeared".to_string())?;
                    let replacement = completion.replacement.clone();
                    let current = snapshot.text.get(replacement.clone());
                    if current != Some(completion.expected.as_str()) {
                        Ok(vec![EditorEffect::Status(
                            "Config completion became stale; edit and try again".to_string(),
                        )])
                    } else {
                        let post_insert_selection = completion.post_insert_selection.clone();
                        let insertion_len = completion.insertion.len();
                        let post_insert_selection_valid = post_insert_selection.start
                            <= post_insert_selection.end
                            && post_insert_selection.end <= insertion_len
                            && completion
                                .insertion
                                .is_char_boundary(post_insert_selection.start)
                            && completion
                                .insertion
                                .is_char_boundary(post_insert_selection.end);
                        if post_insert_selection_valid {
                            buffer.selections = vec![Selection {
                                anchor: replacement.start,
                                head: replacement.end,
                            }];
                            buffer.primary = 0;
                            match workspace.insert_text(store, buffer, &completion.insertion) {
                                Ok(effects) => {
                                    buffer.selections = vec![Selection {
                                        anchor: replacement.start + post_insert_selection.start,
                                        head: replacement.start + post_insert_selection.end,
                                    }];
                                    buffer.primary = 0;
                                    Ok(effects)
                                }
                                Err(error) => Err(error),
                            }
                        } else {
                            Err(crate::native_editor::EditorError::InvalidSelections)
                        }
                    }
                }
                AppEvent::EditorConfigCompletionRejected => Ok(vec![EditorEffect::Status(
                    "Config completion became stale; edit and try again".to_string(),
                )]),
                AppEvent::EditorConfigDiagnosticNavigate { .. } => {
                    let Some((selected, target)) = config_diagnostic_navigation else {
                        return Ok(Some(EventResult::Handled));
                    };
                    let snapshot = store
                        .snapshot(document)
                        .ok_or_else(|| "editor document disappeared".to_string())?;
                    if target > snapshot.text.len() || !snapshot.text.is_char_boundary(target) {
                        Err(crate::native_editor::EditorError::InvalidSelections)
                    } else {
                        state.config_diagnostic_selected = selected;
                        buffer.selections = vec![Selection {
                            anchor: target,
                            head: target,
                        }];
                        buffer.primary = 0;
                        Ok(Vec::new())
                    }
                }
                AppEvent::EditorSetSelection { anchor, head } => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        let Some(snapshot) = store.snapshot(document) else {
                            return Err("editor document disappeared".to_string());
                        };
                        if *anchor > snapshot.text.len()
                            || *head > snapshot.text.len()
                            || !snapshot.text.is_char_boundary(*anchor)
                            || !snapshot.text.is_char_boundary(*head)
                        {
                            Err(crate::native_editor::EditorError::InvalidSelections)
                        } else {
                            buffer.selections = vec![Selection {
                                anchor: *anchor,
                                head: *head,
                            }];
                            buffer.primary = 0;
                            Ok(Vec::new())
                        }
                    }
                }
                AppEvent::EditorViewportChanged { visible_lines } => {
                    let snapshot = store
                        .snapshot(document)
                        .ok_or_else(|| "editor document disappeared".to_string())?;
                    buffer.reconcile_viewport(&snapshot.text, *visible_lines);
                    Ok(Vec::new())
                }
                AppEvent::ScrollLines(lines) => {
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
                        let snapshot = store
                            .snapshot(document)
                            .ok_or_else(|| "editor document disappeared".to_string())?;
                        buffer.scroll_lines(&snapshot.text, *lines);
                        Ok(Vec::new())
                    }
                }
                _ => unreachable!("editor event filter above is exhaustive"),
            };
            if !matches!(
                event,
                AppEvent::ScrollLines(_)
                    | AppEvent::EditorViewportChanged { .. }
                    | AppEvent::TextInput(TextInputEvent::Preedit(_))
            ) && let Some(snapshot) = store.snapshot(document)
            {
                let visible_lines = buffer.viewport_lines();
                buffer.ensure_primary_visible(&snapshot.text, visible_lines);
            }
            match reduced {
                Ok(effects) => {
                    // EditorWorkspace owns view-local selection, viewport,
                    // chord, preedit, and minibuffer state outside the generic
                    // NativeRuntime reducer. Publish the same monotonic view
                    // revision that ordinary native reducers advance so cache
                    // stamps and control-socket inspection never retain an old
                    // caret/modeline after a handled editor event.
                    state.common.presentation_revision =
                        state.common.presentation_revision.saturating_add(1);
                    effects
                }
                Err(error) => {
                    state.status = Some(editor_error_message(&error));
                    state.common.presentation_revision =
                        state.common.presentation_revision.saturating_add(1);
                    Vec::new()
                }
            }
        };

        self.native_runtime.set_editor_history_availability(
            document,
            self.editor_workspace.can_undo(document),
            self.editor_workspace.can_redo(document),
        );

        if !matches!(event, AppEvent::EditorConfigDiagnosticNavigate { .. }) {
            self.publish_editor_commit(document, view);
        }
        for effect in effects {
            self.execute_editor_effect(view, effect)?;
        }
        Ok(Some(EventResult::Handled))
    }

    fn publish_editor_commit(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) {
        if let Some((seq, deltas)) = self.editor_workspace.take_last_commit(document) {
            let other_views = self
                .view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if view != source_view
                            && self
                                .native_runtime
                                .document_id(native.instance)
                                .is_some_and(|candidate| candidate == document) =>
                    {
                        Some(view)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for view in other_views {
                if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
                    && let Some(buffer) = state.buffer.as_mut()
                {
                    buffer.observe_external(seq, &deltas);
                }
            }
        }

        let Some(snapshot) = self.document_store.snapshot(document) else {
            return;
        };
        let journal_result = self
            .native_documents
            .journals
            .as_mut()
            .ok_or_else(|| {
                self.native_documents
                    .journal_unavailable
                    .clone()
                    .unwrap_or_else(|| "document journal is unavailable".to_string())
            })
            .and_then(|journals| journals.observe_commit(&snapshot));
        if let Err(message) = journal_result.and_then(|()| self.drive_document_journal(document)) {
            self.set_document_recovery_status(document, Some(message));
        }
        let dirty = self.document_store.dirty(document).unwrap_or(false);
        let revision = self.document_store.revision(document).unwrap_or(0);
        self.native_runtime
            .publish_document(document, snapshot.text.as_ref(), revision, dirty);
        self.request_config_host_diagnostics(document);
        let views = self.document_native_views(document);
        for (wid, instance, view) in views {
            let _ = self.native_runtime.dispatch(
                instance,
                view,
                AppEvent::DocumentChanged { document, revision },
            );
            self.refresh_native_presentation(wid, instance, view);
            self.request_native_redraw(wid);
        }
    }

    fn drive_document_journal(&mut self, document: DocumentId) -> Result<(), String> {
        let proxy = self.proxy.clone();
        let queue = if proxy.is_some() {
            Some(native_document_queue()?)
        } else {
            None
        };
        loop {
            let effect = self
                .native_documents
                .journals
                .as_mut()
                .ok_or_else(|| "document journal is unavailable".to_string())?
                .next_effect(document)?;
            let Some(effect) = effect else {
                return Ok(());
            };
            match effect {
                JournalEffect::Append { path, key, plan } => {
                    if let (Some(proxy), Some(queue)) = (proxy.clone(), queue) {
                        if queue
                            .send(NativeDocumentJob::JournalAppend {
                                document,
                                path,
                                key,
                                plan: plan.clone(),
                                proxy,
                            })
                            .is_err()
                        {
                            let completion = self
                                .native_documents
                                .journals
                                .as_mut()
                                .expect("journal store checked above")
                                .complete_append(
                                    document,
                                    plan.generation,
                                    crate::native_document_io::JournalAppendResult::Cancelled,
                                );
                            return Err(journal_completion_error(completion)
                                .unwrap_or_else(|| "document journal worker stopped".to_string()));
                        }
                        return Ok(());
                    }
                    let result = execute_journal_append(&path, key, &plan);
                    let completion = self
                        .native_documents
                        .journals
                        .as_mut()
                        .expect("journal store checked above")
                        .complete_append(document, plan.generation, result);
                    if let Some(message) = journal_completion_error(completion) {
                        return Err(message);
                    }
                }
                JournalEffect::Rewrite { path, plan } => {
                    if let (Some(proxy), Some(queue)) = (proxy.clone(), queue) {
                        if queue
                            .send(NativeDocumentJob::JournalRewrite {
                                document,
                                path,
                                plan: plan.clone(),
                                proxy,
                            })
                            .is_err()
                        {
                            let completion = self
                                .native_documents
                                .journals
                                .as_mut()
                                .expect("journal store checked above")
                                .complete_rewrite(
                                    document,
                                    plan.generation,
                                    JournalRewriteResult::Failed(
                                        "document journal worker stopped".to_string(),
                                    ),
                                );
                            return Err(journal_completion_error(completion)
                                .unwrap_or_else(|| "document journal worker stopped".to_string()));
                        }
                        return Ok(());
                    }
                    let result = execute_journal_rewrite(&path, &plan);
                    let completion = self
                        .native_documents
                        .journals
                        .as_mut()
                        .expect("journal store checked above")
                        .complete_rewrite(document, plan.generation, result);
                    if let Some(message) = journal_completion_error(completion) {
                        return Err(message);
                    }
                }
            }
        }
    }

    pub(crate) fn finish_native_document_journal(
        &mut self,
        document: DocumentId,
        generation: crate::native_document_io::JournalGeneration,
        result: crate::native_document_io::JournalAppendResult,
    ) {
        let completion = self
            .native_documents
            .journals
            .as_mut()
            .map_or(JournalCompletion::Stale, |journals| {
                journals.complete_append(document, generation, result)
            });
        if matches!(&completion, JournalCompletion::Durable { .. }) {
            for (_, _, view) in self.document_native_views(document) {
                self.set_editor_view_status(view, "Draft autosaved");
            }
        }
        if let Some(message) = journal_completion_error(completion) {
            self.set_document_recovery_status(document, Some(message));
            return;
        }
        if let Err(message) = self.drive_document_journal(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    pub(crate) fn finish_native_document_journal_rewrite(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
        result: JournalRewriteResult,
    ) {
        let completion = self
            .native_documents
            .journals
            .as_mut()
            .map_or(JournalCompletion::Stale, |journals| {
                journals.complete_rewrite(document, generation, result)
            });
        if matches!(&completion, JournalCompletion::Durable { .. }) {
            for (_, _, view) in self.document_native_views(document) {
                self.set_editor_view_status(view, "Draft autosaved");
            }
        }
        if let Some(message) = journal_completion_error(completion) {
            self.set_document_recovery_status(document, Some(message));
            return;
        }
        if let Err(message) = self.drive_document_journal(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    fn set_document_recovery_status(&mut self, document: DocumentId, message: Option<String>) {
        if let Some(message) = &message {
            self.native_documents
                .recovery_status
                .insert(document, message.clone());
        } else {
            self.native_documents.recovery_status.remove(&document);
        }
        self.native_runtime
            .set_document_recovery_status(document, message);
        for (window, _, _) in self.document_native_views(document) {
            self.request_native_redraw(window);
        }
    }

    fn set_document_disk_conflict(&mut self, document: DocumentId, conflicted: bool) {
        if conflicted {
            self.native_documents.disk_conflicts.insert(document);
        } else {
            self.native_documents.disk_conflicts.remove(&document);
        }
        self.native_runtime
            .set_document_disk_conflict(document, conflicted);
        for (window, _, _) in self.document_native_views(document) {
            self.request_native_redraw(window);
        }
    }

    fn execute_editor_effect(
        &mut self,
        source_view: crate::tab_model::ViewId,
        effect: EditorEffect,
    ) -> Result<(), String> {
        match effect {
            EditorEffect::SaveDocument { document, seq: _ } => {
                self.save_document_checkpoint(document, source_view)?;
            }
            EditorEffect::Status(message) => {
                if let Some(AppViewState::Editor(state)) =
                    self.native_runtime.view_state_mut(source_view)
                {
                    state.status = Some(message);
                }
            }
            EditorEffect::Bell => {
                if let Some(AppViewState::Editor(state)) =
                    self.native_runtime.view_state_mut(source_view)
                {
                    state.status = Some("No command is bound to that key".to_string());
                }
            }
            EditorEffect::SwitchBuffer { query } => {
                self.switch_to_open_editor_buffer(source_view, &query);
            }
            EditorEffect::ShowCommands => {
                if let Some(AppViewState::Editor(state)) =
                    self.native_runtime.view_state_mut(source_view)
                {
                    state.status = Some("Type a command name".to_string());
                }
            }
            EditorEffect::RevertDocument { document } => {
                self.discard_document_changes(document, source_view)?;
            }
        }
        Ok(())
    }

    /// Resolve an exact minibuffer name to an already-open Editor tab in the
    /// source window. This deliberately changes tab focus rather than retargeting
    /// an `EditorApp`/`DocumentViewId`, so persistence grants, journals, and save
    /// generations remain attached to their original canonical documents.
    fn switch_to_open_editor_buffer(&mut self, source_view: crate::tab_model::ViewId, query: &str) {
        let source_window = self.windows.iter().find_map(|(wid, state)| {
            state
                .tab_set
                .tabs()
                .iter()
                .any(|tab| tab.root.leaves().contains(&source_view))
                .then_some(*wid)
        });
        let Some(wid) = source_window else {
            self.set_editor_view_status(source_view, "Editor window is no longer available");
            return;
        };
        if query.is_empty() {
            self.set_editor_view_status(
                source_view,
                "Buffer name is required; no document changed",
            );
            return;
        }

        let matches = self.windows.get(&wid).map_or_else(Vec::new, |state| {
            state
                .tab_set
                .tabs()
                .iter()
                .enumerate()
                .filter_map(|(index, tab)| {
                    tab.root.leaves().into_iter().find_map(|view| {
                        let crate::tab_model::View::Native(native) =
                            self.view_store.get(view).copied()?
                        else {
                            return None;
                        };
                        let NativeApp::Editor(editor) = self.native_runtime.app(native.instance)?
                        else {
                            return None;
                        };
                        let uri = self.document_store.canonical_uri(editor.document)?;
                        (editor.title == query || uri == query).then_some((
                            index,
                            native.instance,
                            view,
                            editor.title.clone(),
                        ))
                    })
                })
                .collect::<Vec<_>>()
        });
        let [(index, instance, target_view, title)] = matches.as_slice() else {
            let status = if matches.is_empty() {
                let open_elsewhere = self.windows.iter().any(|(candidate_window, state)| {
                    *candidate_window != wid
                        && state.tab_set.tabs().iter().any(|tab| {
                            tab.root.leaves().into_iter().any(|view| {
                                let Some(crate::tab_model::View::Native(native)) =
                                    self.view_store.get(view).copied()
                                else {
                                    return false;
                                };
                                let Some(NativeApp::Editor(editor)) =
                                    self.native_runtime.app(native.instance)
                                else {
                                    return false;
                                };
                                self.document_store
                                    .canonical_uri(editor.document)
                                    .is_some_and(|uri| editor.title == query || uri == query)
                            })
                        })
                });
                if open_elsewhere {
                    format!(
                        "Buffer `{query}` is open in another window; cross-window switching is unavailable"
                    )
                } else {
                    format!("No open editor buffer named `{query}`; no document changed")
                }
            } else {
                format!("Buffer name `{query}` is ambiguous; use its full file URI")
            };
            self.set_editor_view_status(source_view, &status);
            return;
        };

        let (index, instance, target_view, title) =
            (*index, *instance, *target_view, title.clone());
        self.switch_tab_in(wid, index);
        self.set_editor_view_status(target_view, &format!("Switched to buffer {title}"));
        self.refresh_native_presentation(wid, instance, target_view);
        self.request_native_redraw(wid);
    }

    fn set_editor_view_status(&mut self, view: crate::tab_model::ViewId, message: &str) {
        if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
            state.status = Some(message.to_string());
        }
    }

    /// Re-observe one document through its existing file capability. Platform
    /// watch notifications, explicit refresh, and repeated open all enter this
    /// deterministic reducer; none may replace dirty bytes implicitly.
    pub(crate) fn refresh_native_document(
        &mut self,
        document: DocumentId,
    ) -> Result<crate::native_document_io::FileWatchReduction, String> {
        let grant = self
            .native_documents
            .persistence
            .grant(document)
            .ok_or_else(|| "document persistence grant disappeared".to_string())?;
        let observed = self
            .native_documents
            .grants
            .refresh_local(grant, DEFAULT_DOCUMENT_LIMIT)
            .map_err(|error| format!("document refresh failed: {error}"))?;
        self.reduce_document_observation(document, &observed.text, observed.observed)
    }

    fn reduce_document_observation(
        &mut self,
        document: DocumentId,
        observed_text: &str,
        observed: crate::native_document_io::ObservedFileVersion,
    ) -> Result<crate::native_document_io::FileWatchReduction, String> {
        let baseline = self
            .native_documents
            .persistence
            .observed(document)
            .ok_or_else(|| "document persistence baseline disappeared".to_string())?;
        let reduction = crate::native_document_io::reduce_file_watch(
            crate::native_document_io::FileWatchInput {
                baseline,
                observed,
                document_dirty: self.document_store.dirty(document).unwrap_or(false),
                save_in_flight: self.native_documents.persistence.save_in_flight(document),
            },
        );
        match reduction {
            crate::native_document_io::FileWatchReduction::Unchanged => {
                if self.native_documents.disk_conflicts.contains(&document) {
                    // The target returned to the admitted baseline. Explicitly
                    // re-accept it so a reducer-level save conflict is cleared
                    // together with the host/UI latch.
                    self.native_documents
                        .persistence
                        .accept_observation(document, observed)
                        .map_err(|error| {
                            format!("could not clear resolved disk conflict: {error:?}")
                        })?;
                    self.set_document_disk_conflict(document, false);
                    self.set_document_recovery_status(document, None);
                    for (_, _, view) in self.document_native_views(document) {
                        self.set_editor_view_status(
                            view,
                            "Disk conflict cleared · Save is available",
                        );
                    }
                }
            }
            crate::native_document_io::FileWatchReduction::RebindEquivalent { .. } => {
                self.native_documents
                    .persistence
                    .accept_observation(document, observed)
                    .map_err(|error| {
                        format!("could not refresh equivalent disk baseline: {error:?}")
                    })?;
                let cleared_conflict = self.native_documents.disk_conflicts.contains(&document);
                self.set_document_disk_conflict(document, false);
                if cleared_conflict {
                    self.set_document_recovery_status(document, None);
                }
                let status = if self.document_store.dirty(document).unwrap_or(false) {
                    "Disk version refreshed · local edits preserved · Save is available"
                } else {
                    "Disk version refreshed · content is unchanged"
                };
                for (_, _, view) in self.document_native_views(document) {
                    self.set_editor_view_status(view, status);
                }
            }
            crate::native_document_io::FileWatchReduction::ReloadClean { .. } => {
                self.install_stable_file_observation(
                    document,
                    observed_text,
                    observed,
                    "Reloaded changes from disk",
                )?;
            }
            crate::native_document_io::FileWatchReduction::ConflictDirty { .. } => {
                self.set_document_disk_conflict(document, true);
                self.set_document_recovery_status(
                    document,
                    Some(
                        "File changed on disk; local edits are preserved and Save is blocked to prevent an overwrite. Copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’"
                            .to_string(),
                    ),
                );
            }
            crate::native_document_io::FileWatchReduction::DeferredSaving { .. } => {
                for (_, _, view) in self.document_native_views(document) {
                    self.set_editor_view_status(view, "Disk refresh deferred until save completes");
                }
            }
        }
        Ok(reduction)
    }

    fn discard_document_changes(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        if self.native_documents.persistence.save_in_flight(document) {
            return Err("cannot discard while a save is in flight".to_string());
        }
        let grant = self
            .native_documents
            .persistence
            .grant(document)
            .ok_or_else(|| "document persistence grant disappeared".to_string())?;
        let observed = self
            .native_documents
            .grants
            .refresh_local(grant, DEFAULT_DOCUMENT_LIMIT)
            .map_err(|error| format!("could not reload document for discard: {error}"))?;
        if matches!(
            self.document_store.phase(document),
            Some(
                crate::document_store::DocumentPhase::Closing { .. }
                    | crate::document_store::DocumentPhase::Blocked { .. }
            )
        ) {
            // A failed final checkpoint freezes ordinary edits. Discard/reload
            // is itself the explicit recovery decision, so reopen the reducer
            // before atomically replacing the draft. Any pending tab/window/
            // quit plan remains installed and revalidates against the new
            // durable head below.
            self.document_store
                .abort_close(document)
                .map_err(|error| format!("could not reopen document for discard: {error:?}"))?;
        }
        self.install_stable_file_observation(
            document,
            &observed.text,
            observed.observed,
            "Discarded changes and reloaded from disk",
        )?;
        self.set_editor_view_status(source_view, "Discarded changes and reloaded from disk");
        self.finish_pending_document_closes(document)?;
        Ok(())
    }

    fn install_stable_file_observation(
        &mut self,
        document: DocumentId,
        observed_text: &str,
        observed: crate::native_document_io::ObservedFileVersion,
        status: &str,
    ) -> Result<(), String> {
        let before = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "document disappeared during refresh".to_string())?;
        let (seq, deltas) = if before.text.as_ref() == observed_text {
            (before.seq, Vec::new())
        } else {
            match self.document_store.transact(
                document,
                before.seq,
                vec![crate::document_store::TextEdit {
                    range: 0..before.text.len(),
                    insert: observed_text.to_string(),
                }],
            ) {
                crate::document_store::DocumentTxnOutcome::Committed { seq, deltas, .. } => {
                    (seq, deltas)
                }
                crate::document_store::DocumentTxnOutcome::Conflict { .. } => {
                    return Err("document changed while installing disk refresh".to_string());
                }
                crate::document_store::DocumentTxnOutcome::Rejected(error) => {
                    return Err(format!("disk refresh was rejected: {error:?}"));
                }
            }
        };
        self.native_documents
            .persistence
            .accept_observation(document, observed)
            .map_err(|error| format!("could not accept refreshed file version: {error:?}"))?;
        self.document_store
            .checkpoint_ack(document, seq)
            .map_err(|error| format!("could not mark refreshed bytes durable: {error:?}"))?;

        let views = self.document_native_views(document);
        for (_, _, view) in &views {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(*view)
                && let Some(buffer) = state.buffer.as_mut()
            {
                buffer.observe_external(seq, &deltas);
                state.status = Some(status.to_string());
            }
        }
        let current = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "refreshed document disappeared".to_string())?;
        if let Some(journals) = self.native_documents.journals.as_mut() {
            journals.observe_commit(&current)?;
            journals.request_checkpoint(
                crate::native_document_io::DurableCheckpoint {
                    document,
                    seq,
                    source: crate::native_document_io::DurableSource::StableFileObservation,
                },
                current.text.clone(),
            )?;
        }
        let revision = self.document_store.revision(document).unwrap_or(0);
        self.native_runtime
            .publish_document(document, current.text.as_ref(), revision, false);
        self.request_config_host_diagnostics(document);
        for (window, instance, view) in views {
            let _ = self.native_runtime.dispatch(
                instance,
                view,
                AppEvent::DocumentChanged { document, revision },
            );
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
        self.set_document_disk_conflict(document, false);
        self.set_document_recovery_status(document, None);
        self.drive_document_journal(document)?;
        Ok(())
    }

    pub(crate) fn save_document_checkpoint(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        if self.native_documents.disk_conflicts.contains(&document) {
            let message = "Save blocked · file changed on disk; copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’".to_string();
            self.set_editor_view_status(source_view, &message);
            return Err(message);
        }
        if let Some(message) = self.native_runtime.config_editor_save_error(document) {
            let message = format!("Save blocked · {message}");
            self.set_editor_view_status(source_view, &message);
            return Err(message);
        }
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "document disappeared before save".to_string())?;
        if self.native_documents.inflight.contains(&document) {
            let intent = PendingDocumentSaveIntent {
                seq: snapshot.seq,
                source_view,
            };
            self.native_documents
                .pending_saves
                .entry(document)
                .and_modify(|pending| {
                    if intent.seq >= pending.seq {
                        *pending = intent;
                    }
                })
                .or_insert(intent);
            self.set_editor_view_status(source_view, "Save queued after current checkpoint");
            return Ok(());
        }
        if let Some(proxy) = self.proxy.clone() {
            // Acquire both fallible capabilities before moving the persistence
            // reducer into `Saving`; a worker startup failure cannot wedge it.
            let queue = native_document_queue()?;
            let pending = self
                .native_documents
                .persistence
                .begin(&snapshot)
                .map_err(|error| format!("could not begin document save: {error:?}"))?;
            let grant = self
                .native_documents
                .grants
                .cloned_grant(pending.grant)
                .ok_or_else(|| "document grant disappeared before save".to_string())?;
            let config_themes = self
                .native_config_service
                .bound_logical_path()
                .filter(|path| grant.targets_logical_path(path))
                .map(|_| {
                    std::sync::Arc::clone(&self.native_config_service.snapshot().assets.themes)
                });
            if queue
                .send(NativeDocumentJob::Save {
                    document,
                    source_view,
                    grant,
                    plan: pending.plan.clone(),
                    config_themes,
                    proxy,
                })
                .is_err()
            {
                let _ = self.native_documents.persistence.complete(
                    document,
                    pending.plan.generation,
                    crate::native_document_io::AtomicSaveResult::Cancelled,
                );
                return Err("document worker stopped".to_string());
            }
            self.native_documents.inflight.insert(document);
            self.native_runtime.set_document_saving(document, true);
            return Ok(());
        }

        // Headless tests have no event-loop proxy. Run the identical capability
        // and reducer path inline so they can prove the complete durable protocol.
        let pending = self
            .native_documents
            .persistence
            .begin(&snapshot)
            .map_err(|error| format!("could not begin document save: {error:?}"))?;
        let grant = self
            .native_documents
            .grants
            .cloned_grant(pending.grant)
            .ok_or_else(|| "document grant disappeared before save".to_string())?;
        let config_themes = self
            .native_config_service
            .bound_logical_path()
            .filter(|path| grant.targets_logical_path(path))
            .map(|_| std::sync::Arc::clone(&self.native_config_service.snapshot().assets.themes));
        let result = crate::native_document_host::execute_granted_save(&grant, &pending.plan);
        let config_observation =
            prepare_config_save_observation(&grant, &pending.plan, &result, config_themes);
        self.finish_native_document_save(
            document,
            source_view,
            pending.plan.generation,
            pending.plan.bytes,
            result,
            config_observation,
        )
    }

    pub(crate) fn finish_native_document_save(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        generation: crate::native_document_io::SaveGeneration,
        saved_bytes: std::sync::Arc<[u8]>,
        result: crate::native_document_io::AtomicSaveResult,
        config_observation: Option<
            Result<crate::native_config_service::PreparedConfigObservation, String>,
        >,
    ) -> Result<(), String> {
        self.native_documents.inflight.remove(&document);
        let reduction = match self
            .native_documents
            .persistence
            .complete(document, generation, result)
        {
            Ok(reduction) => reduction,
            Err(error) => {
                let message = format!("could not finish document save: {error:?}");
                let _ = self.document_store.checkpoint_fail(document);
                self.native_documents.pending_saves.remove(&document);
                self.native_runtime.set_document_saving(document, false);
                self.publish_editor_commit(document, source_view);
                self.mark_document_shutdown_failure(document, &message);
                return Err(message);
            }
        };
        let mut prepared_config = None;
        let outcome = match reduction {
            crate::native_document_io::SaveReduction::Durable(checkpoint) => {
                (|| -> Result<(), String> {
                    self.document_store
                        .checkpoint_ack(document, checkpoint.seq)
                        .map_err(|error| {
                            format!("could not publish durable checkpoint: {error:?}")
                        })?;
                    let saved_text = std::str::from_utf8(&saved_bytes)
                        .map_err(|_| "saved document bytes were not UTF-8".to_string())?;
                    if let Some(observation) = config_observation {
                        match observation {
                            Ok(observation) => prepared_config = Some(observation),
                            Err(error) => {
                                self.native_config_service.mark_reconciliation_required();
                                aterm_log::warn!(
                                    "manual config save was durable but its prepared config generation failed: {error}"
                                );
                            }
                        }
                    }
                    if let Some(journals) = self.native_documents.journals.as_mut() {
                        journals
                            .request_checkpoint(checkpoint, std::sync::Arc::from(saved_text))?;
                    }
                    Ok(())
                })()
            }
            crate::native_document_io::SaveReduction::ReboundEquivalent(_) => {
                let _ = self.document_store.checkpoint_fail(document);
                let cleared_conflict = self.native_documents.disk_conflicts.contains(&document);
                self.set_document_disk_conflict(document, false);
                if cleared_conflict {
                    self.set_document_recovery_status(document, None);
                }
                Err(
                    "Disk version refreshed after an identical on-disk change; local edits are preserved and Save is available. Choose Save again"
                        .to_string(),
                )
            }
            crate::native_document_io::SaveReduction::Conflict(conflict) => {
                let _ = self.document_store.checkpoint_fail(document);
                self.set_document_disk_conflict(document, true);
                let message = format!(
                    "File changed on disk ({conflict:?}); local edits are preserved and Save is blocked. Copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’"
                );
                self.set_document_recovery_status(document, Some(message.clone()));
                Err(message)
            }
            crate::native_document_io::SaveReduction::Failed { stage, message } => {
                let _ = self.document_store.checkpoint_fail(document);
                Err(format!("document save failed at {stage:?}: {message}"))
            }
            crate::native_document_io::SaveReduction::ReconcileRequired {
                stage,
                observed,
                message,
                ..
            } => {
                let _ = self.document_store.checkpoint_fail(document);
                self.set_document_disk_conflict(document, true);
                let recovery = format!(
                    "Document publication is indeterminate at {stage:?}; Save is blocked. Copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’: {message} (observed: {observed:?})"
                );
                self.set_document_recovery_status(document, Some(recovery.clone()));
                Err(recovery)
            }
            crate::native_document_io::SaveReduction::Cancelled => {
                let _ = self.document_store.checkpoint_fail(document);
                Err("document save was cancelled".to_string())
            }
            crate::native_document_io::SaveReduction::Stale => {
                Err("stale document save completion".to_string())
            }
        };

        if outcome.is_ok()
            && let Some(intent) = self.native_documents.pending_saves.remove(&document)
        {
            let Some(durable) = self.document_store.checkpoint_seq(document) else {
                let message = "document checkpoint disappeared before queued save".to_string();
                let _ = self.document_store.checkpoint_fail(document);
                self.native_runtime.set_document_saving(document, false);
                self.publish_editor_commit(document, source_view);
                self.mark_document_shutdown_failure(document, &message);
                return Err(message);
            };
            if intent.seq > durable {
                // Keep the Editor in its Saving state across the hand-off. In a
                // headless run this call completes inline (and performs the final
                // publication); with a real event loop it launches the next worker
                // generation and the later completion owns final publication.
                if let Err(message) = self.save_document_checkpoint(document, intent.source_view) {
                    self.native_runtime.set_document_saving(document, false);
                    self.publish_editor_commit(document, source_view);
                    self.mark_document_shutdown_failure(document, &message);
                    return Err(message);
                }
                return Ok(());
            }
        }

        if outcome.is_err() {
            // A conflict/failure invalidates every intent based on that unproven
            // generation. A new stable observation must be explicitly accepted;
            // no save may launch automatically from stale state.
            self.native_documents.pending_saves.remove(&document);
        }
        self.native_runtime.set_document_saving(document, false);
        self.publish_editor_commit(document, source_view);
        if outcome.is_ok()
            && let Err(message) = self.drive_document_journal(document)
        {
            self.set_document_recovery_status(document, Some(message));
        }
        if let Err(message) = &outcome {
            self.mark_document_shutdown_failure(document, message);
        } else {
            self.mark_document_save_success(document);
            self.finish_pending_document_closes(document)?;
        }
        if let Some(prepared) = prepared_config {
            self.admit_manual_config_observation(prepared);
        }
        outcome
    }

    /// Whole-tab multi-document barrier. Every document leaf is prepared first; only a
    /// single successful batch verdict removes reference edges, so a later blocked leaf
    /// can never leave an earlier sibling half-detached.
    pub(crate) fn prepare_document_tab_close_batch(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
    ) -> Result<bool, String> {
        let state = self
            .windows
            .get(&wid)
            .ok_or_else(|| "document tab window disappeared".to_string())?;
        let tab = state
            .tab_set
            .get(tab_id)
            .ok_or_else(|| "document tab disappeared".to_string())?;
        let mut grouped =
            BTreeMap::<DocumentId, (BTreeSet<DocumentViewId>, crate::tab_model::ViewId)>::new();
        for view in tab.root.leaves() {
            let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
            else {
                continue;
            };
            let Some(document) = self.native_runtime.document_id(native.instance) else {
                continue;
            };
            let entry = grouped
                .entry(document)
                .or_insert_with(|| (BTreeSet::new(), view));
            entry.0.insert(DocumentViewId(view.get()));
        }
        if grouped.is_empty() {
            return Ok(true);
        }
        let plan = DocumentClosePlan {
            documents: grouped
                .into_iter()
                .map(|(document, (views, source_view))| PlannedDocumentClose {
                    document,
                    views: views.into_iter().collect(),
                    source_view,
                })
                .collect(),
        };
        if !self.drive_document_close_plan(&plan, true)? {
            return Ok(false);
        }
        let batch = plan
            .documents
            .iter()
            .map(|item| (item.document, item.views.clone()))
            .collect::<Vec<_>>();
        self.document_store
            .commit_detach_batch(&batch)
            .map_err(|error| format!("could not commit atomic tab close: {error:?}"))?;
        Ok(true)
    }

    fn queue_pending_document_close(
        &mut self,
        document: DocumentId,
        window: WindowId,
        tab: crate::tab_model::TabId,
        views: Vec<DocumentViewId>,
        whole_tab: bool,
    ) {
        let pending = PendingDocumentClose {
            window,
            tab,
            views,
            whole_tab,
        };
        let closes = self
            .native_documents
            .pending_closes
            .entry(document)
            .or_default();
        if !closes.contains(&pending) {
            closes.push(pending);
        }
    }

    /// Per-leaf variant of the document close barrier. A mixed tab survives;
    /// only the named native view is detached after the exact checkpoint proof.
    pub(crate) fn prepare_document_view_close(
        &mut self,
        wid: WindowId,
        tab_id: crate::tab_model::TabId,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<bool, String> {
        let belongs = self
            .windows
            .get(&wid)
            .and_then(|window| window.tab_set.get(tab_id))
            .is_some_and(|tab| tab.root.contains(source_view));
        if !belongs {
            return Err("document view disappeared from its split".to_string());
        }
        let closing = vec![DocumentViewId(source_view.get())];
        let readiness = self
            .document_store
            .prepare_close(document, &closing)
            .map_err(|error| format!("could not prepare document view close: {error:?}"))?;
        match readiness {
            crate::document_store::DocumentCloseReadiness::Ready { .. } => {
                self.document_store
                    .commit_detach(document, &closing)
                    .map_err(|error| format!("could not detach document view: {error:?}"))?;
                Ok(true)
            }
            crate::document_store::DocumentCloseReadiness::Blocked { .. } => {
                self.document_store
                    .checkpoint_retry(document)
                    .map_err(|error| format!("could not retry document checkpoint: {error:?}"))?;
                self.queue_pending_document_close(document, wid, tab_id, closing, false);
                self.save_document_checkpoint(document, source_view)?;
                Ok(false)
            }
            crate::document_store::DocumentCloseReadiness::Pending { .. } => {
                self.queue_pending_document_close(document, wid, tab_id, closing, false);
                self.save_document_checkpoint(document, source_view)?;
                Ok(false)
            }
        }
    }

    fn finish_pending_document_closes(&mut self, document: DocumentId) -> Result<(), String> {
        let pending = self
            .native_documents
            .pending_closes
            .remove(&document)
            .unwrap_or_default();
        let mut deferred = Vec::new();
        for close in pending {
            // A larger OS-window/whole-app plan owns teardown atomically.  Letting
            // an older tab-close completion detach here would visibly apply only
            // part of that plan before the other documents are durable.
            if self.native_documents.pending_quit.is_some()
                || self
                    .native_documents
                    .pending_window_closes
                    .contains_key(&close.window)
            {
                deferred.push(close);
                continue;
            }
            let unchanged = self
                .windows
                .get(&close.window)
                .and_then(|ws| ws.tab_set.tabs().iter().find(|tab| tab.id == close.tab))
                .is_some_and(|tab| {
                    let live = tab
                        .root
                        .leaves()
                        .into_iter()
                        .map(|view| DocumentViewId(view.get()))
                        .collect::<BTreeSet<_>>();
                    close.views.iter().all(|view| live.contains(view))
                });
            if !unchanged {
                continue;
            }
            self.document_store
                .commit_detach(document, &close.views)
                .map_err(|error| format!("could not finish document close: {error:?}"))?;
            if close.whole_tab {
                let removed = self
                    .windows
                    .get_mut(&close.window)
                    .and_then(|ws| ws.tab_set.remove(close.tab));
                if let Some(tab) = removed {
                    self.remove_tab_views(&tab);
                    self.resync_active_or_window(close.window);
                    self.request_native_redraw(close.window);
                }
            } else {
                for document_view in close.views {
                    let view = crate::tab_model::ViewId::from_stored(document_view.0);
                    let removed = self
                        .windows
                        .get_mut(&close.window)
                        .and_then(|window| {
                            let index = window
                                .tab_set
                                .tabs()
                                .iter()
                                .position(|candidate| candidate.id == close.tab)?;
                            window.tab_set.tab_at_mut(index)
                        })
                        .map(|tab| tab.remove_view(view));
                    if removed == Some(crate::tab_model::RemoveLeaf::Removed) {
                        self.remove_view_link(view);
                    }
                }
                self.resize_panes(close.window);
                self.resync_active_or_window(close.window);
                self.request_native_redraw(close.window);
            }
        }
        if !deferred.is_empty() {
            self.native_documents
                .pending_closes
                .insert(document, deferred);
        }
        Ok(())
    }

    /// Arm (or re-drive) the all-or-nothing document barrier for one OS window.
    /// `true` means every document is durable and its document-view edges were
    /// detached as one batch, so the caller may now tear down the window. `false`
    /// means the complete window remains installed while saves are in flight or a
    /// failed save is surfaced for retry.
    pub(crate) fn prepare_window_document_shutdown(
        &mut self,
        window: WindowId,
    ) -> Result<bool, String> {
        self.prepare_window_document_shutdown_inner(window, true)
    }

    fn prepare_window_document_shutdown_inner(
        &mut self,
        window: WindowId,
        start_saves: bool,
    ) -> Result<bool, String> {
        if !self.windows.contains_key(&window) {
            return Ok(true);
        }
        if !self
            .native_documents
            .pending_window_closes
            .contains_key(&window)
        {
            let plan = self.document_close_plan_for_window(window)?;
            self.native_documents
                .pending_window_closes
                .insert(window, plan);
        }
        let plan = self
            .native_documents
            .pending_window_closes
            .get(&window)
            .cloned()
            .expect("window close plan was just installed");
        let ready = self.drive_document_close_plan(&plan, start_saves)?;
        if ready {
            self.commit_pending_window_document_shutdown(window)?;
        }
        Ok(ready)
    }

    /// Arm the process-wide clean-Quit barrier.  This enumerates documents from
    /// `DocumentStore`, not only visible tabs, so a dirty suspended document can
    /// never be skipped.  A Ready verdict permits deferred-update application and
    /// event-loop exit; Waiting keeps every window/view alive.
    pub(crate) fn prepare_quit_document_shutdown(&mut self) -> Result<bool, String> {
        self.prepare_quit_document_shutdown_inner(true)
    }

    /// A failed Quit save is retryable, not a latent exit request. The event-loop
    /// bridge uses this to release its generation latch and require a fresh user
    /// confirmation rather than exiting later after an unrelated save.
    pub(crate) fn quit_document_shutdown_blocked(&self) -> bool {
        self.native_documents
            .pending_quit
            .as_ref()
            .is_some_and(|plan| {
                plan.documents.iter().any(|item| {
                    matches!(
                        self.document_store.phase(item.document),
                        Some(crate::document_store::DocumentPhase::Blocked { .. })
                    )
                })
            })
    }

    /// Disarm a failed whole-app Quit while preserving any independently pending
    /// window close. This prevents a late completion from turning a failed Quit
    /// into a surprise process exit. Returns whether a plan was removed.
    pub(crate) fn cancel_failed_quit_document_shutdown(&mut self) -> bool {
        let Some(plan) = self.native_documents.pending_quit.take() else {
            return false;
        };
        let protected = self
            .native_documents
            .pending_window_closes
            .values()
            .flat_map(|plan| plan.documents.iter().map(|item| item.document))
            .collect::<BTreeSet<_>>();
        for item in plan.documents {
            if !protected.contains(&item.document) {
                let _ = self.document_store.abort_close(item.document);
            }
        }
        true
    }

    fn prepare_quit_document_shutdown_inner(&mut self, start_saves: bool) -> Result<bool, String> {
        if self.native_documents.pending_quit.is_none() {
            self.native_documents.pending_quit = Some(self.document_close_plan_for_quit());
        }
        let plan = self
            .native_documents
            .pending_quit
            .clone()
            .expect("quit close plan was just installed");
        let ready = self.drive_document_close_plan(&plan, start_saves)?;
        if ready {
            self.native_documents.pending_quit = None;
        }
        Ok(ready)
    }

    /// Called after an async save reduction.  A pending Quit wins over individual
    /// window closes.  Returning `true` consumes a now-ready Quit plan; ready window
    /// ids are batch-detached and consumed, ready for `close_window_logical`.
    pub(crate) fn take_ready_document_shutdowns(
        &mut self,
    ) -> Result<(bool, Vec<WindowId>), String> {
        if let Some(plan) = self.native_documents.pending_quit.clone() {
            if self.document_close_plan_ready(&plan)? {
                // The first native-app preflight happened before the asynchronous
                // checkpoint. A Settings draft (or another native blocker) may
                // have appeared while the save worker was running, so obtain fresh
                // reducer-owned readiness at the irreversible exit boundary.
                if !self.prepare_quit_native_shutdown()? {
                    return Ok((false, Vec::new()));
                }
                self.native_documents.pending_quit = None;
                return Ok((true, Vec::new()));
            }
            return Ok((false, Vec::new()));
        }

        let windows = self
            .native_documents
            .pending_window_closes
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut ready = Vec::new();
        for window in windows {
            let Some(plan) = self
                .native_documents
                .pending_window_closes
                .get(&window)
                .cloned()
            else {
                continue;
            };
            if self.document_close_plan_ready(&plan)? {
                // Do not detach the now-durable document edges until every native
                // leaf is still ready. The UI remains live while checkpoints run,
                // and a newly-created Settings draft must retain the complete
                // window topology and surface its exact recovery payload.
                if !self
                    .prepare_window_native_shutdown(window, crate::native_app::CloseScope::Window)?
                {
                    continue;
                }
                self.commit_pending_window_document_shutdown(window)?;
                ready.push(window);
            }
        }
        Ok((false, ready))
    }

    fn document_close_plan_for_window(
        &self,
        window: WindowId,
    ) -> Result<DocumentClosePlan, String> {
        let state = self
            .windows
            .get(&window)
            .ok_or_else(|| "window disappeared before document close planning".to_string())?;
        let mut grouped =
            BTreeMap::<DocumentId, (BTreeSet<DocumentViewId>, crate::tab_model::ViewId)>::new();
        for view in state
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| tab.root.leaves())
        {
            let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
            else {
                continue;
            };
            let Some(document) = self.native_runtime.document_id(native.instance) else {
                continue;
            };
            let entry = grouped
                .entry(document)
                .or_insert_with(|| (BTreeSet::new(), view));
            entry.0.insert(DocumentViewId(view.get()));
        }
        Ok(DocumentClosePlan {
            documents: grouped
                .into_iter()
                .map(|(document, (views, source_view))| PlannedDocumentClose {
                    document,
                    views: views.into_iter().collect(),
                    source_view,
                })
                .collect(),
        })
    }

    fn document_close_plan_for_quit(&self) -> DocumentClosePlan {
        DocumentClosePlan {
            documents: self
                .document_store
                .document_ids()
                .into_iter()
                .map(|document| {
                    let views = self.document_store.view_ids(document).unwrap_or_default();
                    let source_view = self.document_native_views(document).first().map_or_else(
                        || crate::tab_model::ViewId::from_stored(0),
                        |(_, _, view)| *view,
                    );
                    PlannedDocumentClose {
                        document,
                        views,
                        source_view,
                    }
                })
                .collect(),
        }
    }

    fn drive_document_close_plan(
        &mut self,
        plan: &DocumentClosePlan,
        start_saves: bool,
    ) -> Result<bool, String> {
        let mut waiting = Vec::new();
        for item in &plan.documents {
            let readiness = self
                .document_store
                .prepare_close(item.document, &item.views)
                .map_err(|error| format!("could not prepare document shutdown: {error:?}"))?;
            match readiness {
                crate::document_store::DocumentCloseReadiness::Ready { .. } => {}
                crate::document_store::DocumentCloseReadiness::Pending { .. } => {
                    waiting.push((item.clone(), false));
                }
                crate::document_store::DocumentCloseReadiness::Blocked { .. } => {
                    waiting.push((item.clone(), true));
                }
            }
        }
        if waiting.is_empty() {
            return Ok(true);
        }
        if !start_saves {
            return Ok(false);
        }

        for (item, retry) in waiting {
            if retry {
                self.document_store
                    .checkpoint_retry(item.document)
                    .map_err(|error| format!("could not retry document shutdown: {error:?}"))?;
            }
            if let Err(message) = self.save_document_checkpoint(item.document, item.source_view) {
                self.mark_document_shutdown_failure(item.document, &message);
            }
        }
        self.document_close_plan_ready(plan)
    }

    fn document_close_plan_ready(&mut self, plan: &DocumentClosePlan) -> Result<bool, String> {
        for item in &plan.documents {
            if !matches!(
                self.document_store
                    .prepare_close(item.document, &item.views)
                    .map_err(|error| format!("could not verify document shutdown: {error:?}"))?,
                crate::document_store::DocumentCloseReadiness::Ready { .. }
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn commit_pending_window_document_shutdown(&mut self, window: WindowId) -> Result<(), String> {
        let plan = self
            .native_documents
            .pending_window_closes
            .get(&window)
            .cloned()
            .ok_or_else(|| "pending window document plan disappeared".to_string())?;
        let batch = plan
            .documents
            .iter()
            .map(|item| (item.document, item.views.clone()))
            .collect::<Vec<_>>();
        self.document_store
            .commit_detach_batch(&batch)
            .map_err(|error| format!("could not commit atomic window close: {error:?}"))?;
        self.native_documents.pending_window_closes.remove(&window);
        for closes in self.native_documents.pending_closes.values_mut() {
            closes.retain(|close| close.window != window);
        }
        self.native_documents
            .pending_closes
            .retain(|_, closes| !closes.is_empty());
        Ok(())
    }

    fn mark_document_shutdown_failure(&mut self, document: DocumentId, message: &str) {
        let _ = self.document_store.checkpoint_fail(document);
        for (window, _, view) in self.document_native_views(document) {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
                state.status = Some(message.to_string());
            }
            if let Some(tab) = self.windows.get_mut(&window).and_then(|state| {
                let index = state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.root.leaves().contains(&view))?;
                state.tab_set.tab_at_mut(index)
            }) {
                tab.presentation.indicators.attention = true;
                tab.presentation.tooltip = Some(message.to_string());
            }
            self.request_native_redraw(window);
        }
    }

    fn mark_document_save_success(&mut self, document: DocumentId) {
        self.set_document_disk_conflict(document, false);
        self.set_document_recovery_status(document, None);
        let dirty = self.document_store.dirty(document).unwrap_or(false);
        for (window, _, view) in self.document_native_views(document) {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
                state.status = Some(if dirty {
                    "Modified · newer changes are not saved".to_string()
                } else {
                    "Saved".to_string()
                });
            }
            if let Some(tab) = self.windows.get_mut(&window).and_then(|state| {
                let index = state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.root.leaves().contains(&view))?;
                state.tab_set.tab_at_mut(index)
            }) {
                tab.presentation.indicators.attention = false;
            }
            self.request_native_redraw(window);
        }
    }

    pub(crate) fn document_native_views(
        &self,
        document: DocumentId,
    ) -> Vec<(
        WindowId,
        crate::tab_model::AppInstanceId,
        crate::tab_model::ViewId,
    )> {
        self.windows
            .iter()
            .flat_map(|(wid, ws)| {
                ws.tab_set.tabs().iter().flat_map(move |tab| {
                    tab.root.leaves().into_iter().filter_map(move |view| {
                        let crate::tab_model::View::Native(native) =
                            self.view_store.get(view).copied()?
                        else {
                            return None;
                        };
                        (self.native_runtime.document_id(native.instance) == Some(document))
                            .then_some((*wid, native.instance, view))
                    })
                })
            })
            .collect()
    }

    /// Recompute duplicate-basename labels and publish every affected tab,
    /// including inactive tabs and views in other windows. Canonical document
    /// identity remains in the runtime/store; this only refreshes presentation.
    pub(crate) fn refresh_disambiguated_document_titles(&mut self) {
        let changed = self
            .native_runtime
            .disambiguate_document_titles()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if changed.is_empty() {
            return;
        }
        let view_store = &self.view_store;
        let changed_instances = &changed;
        let views = self
            .windows
            .iter()
            .flat_map(|(window, state)| {
                state.tab_set.tabs().iter().flat_map(move |tab| {
                    tab.root.leaves().into_iter().filter_map(move |view| {
                        let crate::tab_model::View::Native(native) =
                            view_store.get(view).copied()?
                        else {
                            return None;
                        };
                        changed_instances.contains(&native.instance).then_some((
                            *window,
                            native.instance,
                            view,
                        ))
                    })
                })
            })
            .collect::<Vec<_>>();
        for (window, instance, view) in views {
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
    }

    /// Open or focus one canonical local document in the requesting window.
    /// Ordinary repeated opens never pull focus to another window.
    pub(crate) fn open_document_tab(&mut self, kind: AppKind, uri: &str) -> Result<String, String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no requesting window".to_string())?;
        self.open_document_tab_in_window(wid, kind, uri)
    }

    /// Open or focus a canonical local document in one exact host window.
    ///
    /// Window-system gestures such as file drops carry the window they landed on.
    /// Keeping that identity through grant acquisition and tab installation prevents
    /// a late focus change from redirecting the document into another window.
    pub(crate) fn open_document_tab_in_window(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
    ) -> Result<String, String> {
        self.open_document_tab_in_window_with_config_symlinks(wid, kind, uri, false)
    }

    fn open_document_tab_in_window_with_config_symlinks(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
        allow_config_symlinks: bool,
    ) -> Result<String, String> {
        if !matches!(kind, AppKind::Markdown | AppKind::Editor) {
            return Err("document tabs must be Markdown or Editor".to_string());
        }
        if allow_config_symlinks && kind != AppKind::Editor {
            return Err("only the Manual config editor may bind config symlinks".to_string());
        }
        if !self.windows.contains_key(&wid) {
            return Err("requesting window disappeared".to_string());
        }
        let access = if kind == AppKind::Editor {
            GrantAccess::ReadWrite
        } else {
            GrantAccess::ReadOnly
        };
        let granted = if allow_config_symlinks {
            self.native_documents
                .grants
                .open_local_config(uri, access, DEFAULT_DOCUMENT_LIMIT)
        } else {
            self.native_documents
                .grants
                .open_local(uri, access, DEFAULT_DOCUMENT_LIMIT)
        }
        .map_err(|error| format!("document open failed: {error}"))?;
        let existing = self.document_store.id_for_uri(&granted.grant.canonical_uri);
        let mut recovery_notice = None;
        let document = if let Some(document) = existing {
            document
        } else {
            let decision = self
                .native_documents
                .journals
                .as_ref()
                .ok_or_else(|| {
                    self.native_documents
                        .journal_unavailable
                        .clone()
                        .unwrap_or_else(|| "document recovery is unavailable".to_string())
                })?
                .inspect_open(&granted.grant.canonical_uri, granted.text.as_bytes())?;
            let recovered = decision.recovered_text.clone();
            let document = self
                .document_store
                .open(granted.grant.canonical_uri.clone(), granted.text.clone());
            let disk_snapshot = self
                .document_store
                .snapshot(document)
                .ok_or_else(|| "new document baseline disappeared".to_string())?;
            if let Some(recovered) = recovered
                && recovered.as_str() != disk_snapshot.text.as_ref()
            {
                let outcome = self.document_store.transact(
                    document,
                    disk_snapshot.seq,
                    vec![crate::document_store::TextEdit {
                        range: 0..disk_snapshot.text.len(),
                        insert: recovered,
                    }],
                );
                if !matches!(
                    outcome,
                    crate::document_store::DocumentTxnOutcome::Committed { .. }
                ) {
                    let _ = self.document_store.remove_if_unattached(document);
                    return Err("recovered draft could not be installed atomically".to_string());
                }
            }
            let current = self
                .document_store
                .snapshot(document)
                .ok_or_else(|| "recovered document disappeared".to_string())?;
            let initialized = self
                .native_documents
                .journals
                .as_mut()
                .expect("journal availability checked before opening")
                .initialize(decision, &disk_snapshot, &current);
            let initialized = match initialized {
                Ok(initialized) => initialized,
                Err(error) => {
                    let _ = self.document_store.remove_if_unattached(document);
                    return Err(format!("document recovery initialization failed: {error}"));
                }
            };
            recovery_notice = initialized.notice;
            document
        };
        self.native_documents
            .persistence
            .register(document, granted.grant.id, granted.observed)
            .map_err(|error| format!("document persistence failed: {error:?}"))?;
        if existing.is_some() {
            let _ = self.refresh_native_document(document)?;
        }

        if let Some((tab, view)) = self.document_view_in_window(wid, kind, document) {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_set.switch_to(tab);
                ws.last_present = None;
            }
            self.resync_active_or_window(wid);
            self.request_native_redraw(wid);
            return Ok(format!(
                "app {} {} view={}",
                kind.as_str(),
                granted.grant.canonical_uri,
                view.get()
            ));
        }

        let title = granted
            .grant
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("Untitled")
            .to_string();
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "canonical document disappeared".to_string())?;
        let presentation = crate::tab_model::TabPresentation {
            title: title.clone(),
            icon: Some(match kind {
                AppKind::Markdown => crate::tab_model::TabIconKind::Markdown,
                AppKind::Editor => crate::tab_model::TabIconKind::Editor,
                AppKind::Settings => crate::tab_model::TabIconKind::Settings,
                AppKind::Recovery => crate::tab_model::TabIconKind::Recovery,
            }),
            indicators: crate::tab_model::TabIndicators::default(),
            closable: true,
            // The tooltip carries the EXACT URI this tab was opened with. The
            // grant's canonical form (symlinks resolved — `/private/var/…` on
            // macOS) stays the internal identity for dedup and persistence,
            // but the user is shown the location they actually asked for.
            tooltip: Some(format!("{} · {}", kind.display_name(), uri)),
        };
        let state = match kind {
            AppKind::Markdown => AppViewState::Markdown(MarkdownViewState::default()),
            AppKind::Editor => AppViewState::Editor(Box::default()),
            AppKind::Settings | AppKind::Recovery => {
                return Err("requested app is not a document app".to_string());
            }
        };
        let install =
            if let Some(instance) = self.native_runtime.instance_for_document(kind, document) {
                self.install_native_tab(wid, instance, state, presentation)
                    .map(|(tab, view)| (instance, tab, view))
            } else {
                // The app presents (and restores through) the exact URI the
                // user opened; the resolved grant URI remains the store key.
                let app = match kind {
                    AppKind::Markdown => NativeApp::Markdown(MarkdownApp::new_with_uri(
                        document,
                        title,
                        uri.to_string(),
                        snapshot.text.as_ref(),
                    )),
                    AppKind::Editor => {
                        NativeApp::Editor(EditorApp::new_with_uri(document, title, uri.to_string()))
                    }
                    AppKind::Settings | AppKind::Recovery => {
                        return Err("requested app is not a document app".to_string());
                    }
                };
                self.install_new_native_tab(wid, app, state, presentation)
            };
        let (instance, tab, view) =
            install.map_err(|error| format!("could not install native document tab: {error:?}"))?;

        if let Err(error) = self.attach_document_view(kind, document, view) {
            self.rollback_document_tab(wid, tab);
            return Err(error);
        }
        if let Some(notice) = recovery_notice {
            self.set_document_recovery_status(document, Some(notice.message()));
        } else if let Some(status) = self
            .native_documents
            .recovery_status
            .get(&document)
            .cloned()
        {
            self.native_runtime
                .set_document_recovery_status(document, Some(status));
        }
        self.native_runtime.set_document_disk_conflict(
            document,
            self.native_documents.disk_conflicts.contains(&document),
        );
        self.refresh_disambiguated_document_titles();
        self.refresh_native_presentation(wid, instance, view);
        self.resync_active_or_window(wid);
        self.request_native_redraw(wid);
        Ok(format!(
            "app {} {} view={}",
            kind.as_str(),
            granted.grant.canonical_uri,
            view.get()
        ))
    }

    fn document_view_in_window(
        &self,
        wid: WindowId,
        kind: AppKind,
        document: DocumentId,
    ) -> Option<(crate::tab_model::TabId, crate::tab_model::ViewId)> {
        self.windows
            .get(&wid)?
            .tab_set
            .tabs()
            .iter()
            .find_map(|tab| {
                tab.root.leaves().into_iter().find_map(|view| {
                    let crate::tab_model::View::Native(native) =
                        self.view_store.get(view).copied()?
                    else {
                        return None;
                    };
                    let app = self.native_runtime.app(native.instance)?;
                    (app.kind() == kind && app.document_id() == Some(document))
                        .then_some((tab.id, view))
                })
            })
    }

    pub(crate) fn attach_document_view(
        &mut self,
        kind: AppKind,
        document: DocumentId,
        view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        let document_view = DocumentViewId(view.get());
        match kind {
            AppKind::Markdown => self
                .document_store
                .attach_view(document, document_view)
                .map_err(|error| format!("could not attach Markdown view: {error:?}")),
            AppKind::Editor => {
                let buffer = self
                    .editor_workspace
                    .attach(&mut self.document_store, document, document_view)
                    .map_err(|error| format!("could not attach editor view: {error:?}"))?;
                let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
                else {
                    return Err("editor runtime view disappeared during attach".to_string());
                };
                state.buffer = Some(buffer);
                Ok(())
            }
            AppKind::Settings | AppKind::Recovery => {
                Err("requested app is not a document app".to_string())
            }
        }
    }

    fn rollback_document_tab(&mut self, wid: WindowId, tab: crate::tab_model::TabId) {
        let removed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.tab_set.remove(tab));
        if let Some(tab) = removed {
            self.remove_tab_views(&tab);
        }
        self.resync_active_or_window(wid);
    }

    fn request_native_redraw(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_present = None;
        }
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }
}

/// Create a missing config file without truncating an existing file, then return
/// the exact logical path used by the native document-grant boundary.
///
/// Authority is resolved before the first filesystem mutation. Creation then
/// goes through the same locked, compare-and-swap host as ordinary saves. Manual
/// binds every existing symlink component before any mutation, so dotfiles layouts
/// may use parent-directory or final-file links without allowing a later swap to
/// redirect creation or publication. Ordinary document grants remain symlink-free.
fn ensure_config_file(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("config path must be absolute: {}", path.display()));
    }
    let contents = crate::native_document_host::read_config_atomic_file(
        path,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        true,
    )
    .map_err(|error| format!("could not validate {}: {error}", path.display()))?;
    if !contents.baseline.observed.exists {
        match crate::native_document_host::commit_atomic_bytes(&contents.baseline, b"") {
            crate::native_document_host::AtomicCommitResult::Committed(_) => {}
            crate::native_document_host::AtomicCommitResult::Conflict { .. } => {
                // Another cooperating opener may have won the missing-file CAS.
                // Re-mint from the now-visible path instead of treating that safe
                // convergence as a user-facing failure. A symlink/non-file winner
                // is rejected by this exact authority check.
                crate::native_document_host::read_config_atomic_file(
                    path,
                    crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
                    false,
                )
                .map_err(|error| {
                    format!(
                        "could not validate concurrently created {}: {error}",
                        path.display()
                    )
                })?;
            }
            crate::native_document_host::AtomicCommitResult::Failed { stage, message } => {
                return Err(format!(
                    "could not create {} at {stage:?}: {message}",
                    path.display()
                ));
            }
            crate::native_document_host::AtomicCommitResult::PublishedUnverified {
                stage,
                message,
                ..
            } => {
                return Err(format!(
                    "{} may have been created but could not be verified at {stage:?}: {message}; reopen Manual to reconcile",
                    path.display()
                ));
            }
        }
    }
    Ok(path.to_path_buf())
}

fn editor_error_message(error: &crate::native_editor::EditorError) -> String {
    use crate::native_editor::EditorError;
    match error {
        EditorError::NothingToUndo => "Nothing to undo".to_string(),
        EditorError::NothingToRedo => "Nothing to redo".to_string(),
        EditorError::NoKill => "Kill ring is empty".to_string(),
        EditorError::StaleView { .. }
        | EditorError::TransactionConflict { .. }
        | EditorError::HistoryDiverged { .. } => {
            "The document changed; this view was safely stopped before editing stale text"
                .to_string()
        }
        EditorError::UnknownDocument => "The document is no longer available".to_string(),
        EditorError::InvalidSelections | EditorError::TransactionRejected => {
            "The edit could not be applied".to_string()
        }
        EditorError::MacroRecursion => "A keyboard macro cannot invoke itself".to_string(),
    }
}

fn journal_completion_error(completion: JournalCompletion) -> Option<String> {
    match completion {
        JournalCompletion::Durable { .. } | JournalCompletion::Stale => None,
        JournalCompletion::Failed { message, .. } => Some(message),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::input::InputEvent;
    use crate::native_editor::Minibuffer;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    fn file_uri(path: &std::path::Path) -> String {
        format!("file://{}", path.to_string_lossy().replace(' ', "%20"))
    }

    fn key(key: Key, mods: Modifiers) -> InputEvent {
        InputEvent::Key {
            key,
            mods,
            base_layout: None,
            event_type: KeyEventType::Press,
        }
    }

    fn drive_native(app: &mut App, event: InputEvent) {
        assert!(app.native_input_event(WindowId(0), &event));
    }

    fn enter_settings_draft(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new(format!(
                "settings/control/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )))),
        )
        .unwrap();
        app.dispatch_native_view_event(wid, view, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::TextInput(TextInputEvent::Commit("Async Close Draft Mono".to_string())),
        )
        .unwrap();
    }

    fn discard_all_settings_drafts(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
        for _ in 0..2 {
            app.dispatch_native_view_event(
                wid,
                view,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                    value: None,
                }),
            )
            .unwrap();
        }
    }

    fn editor_buffer(
        app: &App,
        view: crate::tab_model::ViewId,
    ) -> &crate::native_editor::EditorBufferView {
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("expected editor view");
        };
        state.buffer.as_ref().expect("attached editor buffer")
    }

    #[test]
    fn editor_reducer_overscrolls_to_a_presentable_anchor_and_reverses_immediately() {
        let dir =
            std::env::temp_dir().join(format!("aterm-editor-scroll-anchor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        // More lines than the renderer's bounded maximum keep this reducer
        // exercise scrollable under every headless/window geometry.
        let source = (0..300)
            .map(|line| format!("# config line {line}\n"))
            .collect::<String>();
        fs::write(&path, &source).unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::EditorViewportChanged { visible_lines: 4 },
        )
        .unwrap();
        app.dispatch_native_view_event(wid, view, AppEvent::ScrollLines(i32::MAX))
            .unwrap();

        let snapshot = app.document_store.snapshot(document).unwrap();
        let bottom = crate::native_editor::project_viewport(
            snapshot.text.as_ref(),
            editor_buffer(&app, view),
            editor_buffer(&app, view).viewport_lines(),
            80,
        );
        let visible_lines = editor_buffer(&app, view).viewport_lines();
        assert_eq!(bottom.total_lines, 301);
        assert_eq!(
            bottom.first_line,
            bottom.total_lines.saturating_sub(visible_lines),
            "stored anchor is the last full page for the installed renderer capacity"
        );
        let bottom_anchor = editor_buffer(&app, view).viewport_anchor;

        app.dispatch_native_view_event(wid, view, AppEvent::ScrollLines(-1))
            .unwrap();
        let one_line_up = crate::native_editor::project_viewport(
            snapshot.text.as_ref(),
            editor_buffer(&app, view),
            visible_lines,
            80,
        );
        assert_eq!(one_line_up.first_line + 1, bottom.first_line);
        assert!(
            editor_buffer(&app, view).viewport_anchor < bottom_anchor,
            "one reverse reducer event must move the stored and painted anchor"
        );

        let _ = fs::remove_dir_all(dir);
    }

    fn clear_exact_config_assist_cache(
        app: &App,
        instance: crate::tab_model::AppInstanceId,
        context: crate::native_config_language::ConfigCompletionContext,
    ) {
        assert!(
            app.native_runtime
                .config_editor_analysis(instance)
                .is_some(),
            "the exact worker analysis is complete"
        );
        app.native_runtime.clear_config_assist_cache(instance);
        assert_eq!(
            app.native_runtime
                .cached_config_completion_count(instance, context),
            0
        );
        assert!(
            !app.native_runtime
                .cached_config_assist_present(instance, context),
            "no render may pre-populate the input-path fixture"
        );
    }

    fn begin_inflight_save_for_test(
        app: &mut App,
        document: DocumentId,
    ) -> crate::native_document_host::PendingDocumentSave {
        let snapshot = app.document_store.snapshot(document).unwrap();
        let pending = app.native_documents.persistence.begin(&snapshot).unwrap();
        assert!(app.native_documents.inflight.insert(document));
        app.native_runtime.set_document_saving(document, true);
        pending
    }

    fn finish_inflight_save_for_test(
        app: &mut App,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        pending: crate::native_document_host::PendingDocumentSave,
    ) -> Result<(), String> {
        let grant = app
            .native_documents
            .grants
            .cloned_grant(pending.grant)
            .unwrap();
        let config_themes = app
            .native_config_service
            .bound_logical_path()
            .filter(|path| grant.targets_logical_path(path))
            .map(|_| std::sync::Arc::clone(&app.native_config_service.snapshot().assets.themes));
        let result = crate::native_document_host::execute_granted_save(&grant, &pending.plan);
        let config_observation =
            prepare_config_save_observation(&grant, &pending.plan, &result, config_themes);
        app.finish_native_document_save(
            document,
            source_view,
            pending.plan.generation,
            pending.plan.bytes,
            result,
            config_observation,
        )
    }

    #[cfg(unix)]
    #[test]
    fn manual_open_refuses_writerless_journal_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir =
            std::env::temp_dir().join(format!("aterm-manual-journal-fifo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config = dir.join("aterm.toml");
        fs::write(&config, "window_theme = \"dark\"\n").unwrap();
        let journal_root = dir.join("private-drafts");
        let mut app = App::headless_for_test();
        app.native_documents.journals =
            Some(DocumentJournalStore::for_test(journal_root.clone()).unwrap());

        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(&config).expect("canonical config path"),
        )
        .unwrap();
        let key = crate::native_document_io::JournalDocumentKey::for_canonical_uri(&canonical);
        let journal = journal_root.join(format!("{:016x}.atdj", key.0));
        let journal_c =
            std::ffi::CString::new(journal.as_os_str().as_bytes()).expect("journal FIFO path");
        // SAFETY: `journal_c` is a live NUL-terminated pathname and `mkfifo`
        // retains no pointer. The fixture directory is private to this test.
        assert_eq!(unsafe { libc::mkfifo(journal_c.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        let error = app
            .ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
            .expect_err("Manual must refuse a special recovery-journal target");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.contains("recovery journal"), "{error}");
        assert!(app.active_native_view(WindowId(0)).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manual_open_reports_busy_held_journal_lock_and_retries_cleanly() {
        let dir =
            std::env::temp_dir().join(format!("aterm-manual-journal-held-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config = dir.join("aterm.toml");
        fs::write(&config, "window_theme = \"dark\"\n").unwrap();
        let journal_root = dir.join("private-drafts");
        let mut app = App::headless_for_test();
        app.native_documents.journals =
            Some(DocumentJournalStore::for_test(journal_root.clone()).unwrap());
        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(&config).expect("canonical config path"),
        )
        .unwrap();
        let key = crate::native_document_io::JournalDocumentKey::for_canonical_uri(&canonical);
        let journal = journal_root.join(format!("{:016x}.atdj", key.0));
        let lock_path = journal_root.join(format!(
            ".{}.lock",
            journal.file_name().unwrap().to_string_lossy()
        ));
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        held.lock().unwrap();

        let started = std::time::Instant::now();
        let error = app
            .ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
            .expect_err("held journal lock must report busy");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.contains("busy"), "{error}");
        assert!(error.contains("retry opening Manual"), "{error}");
        assert!(app.active_native_view(WindowId(0)).is_none());

        drop(held);
        app.ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
            .expect("Manual retry succeeds after the lock is released");
        assert!(app.active_native_view(WindowId(0)).is_some());
        let _ = fs::remove_dir_all(dir);
    }

    fn assert_focused_config_completion_activates(named_key: NamedKey, suffix: &str) {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-focused-completion-{}-{suffix}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "win").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();

        assert!(app.prepare_native_input_scratch(wid));
        let focus = app
            .retained_native_leaf_artifact(wid, view, false)
            .expect("compiled native config editor")
            .compiled
            .semantics
            .iter()
            .find(|node| {
                node.key.as_str().starts_with("editor/config-completion/")
                    && node.label.starts_with("Insert window_theme")
            })
            .expect("window theme completion semantic button")
            .key
            .clone();
        let context = app
            .editor_config_completion_context(instance, view)
            .expect("exact rendered completion context");
        let state = app
            .native_runtime
            .view_state_mut(view)
            .expect("config editor view state");
        state.common_mut().last_focus = Some(focus);
        let AppViewState::Editor(state) = state else {
            panic!("config editor view state");
        };
        state.config_completion_interaction = Some(context);

        drive_native(&mut app, key(Key::Named(named_key), Modifiers::empty()));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "window_theme = \"auto\""
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn repeated_open_reuses_current_window_view_and_document() {
        let dir = std::env::temp_dir().join(format!("aterm-app-doc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("README test.md");
        fs::write(&path, "# Hello\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        let first = app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        let tabs = app.windows[&WindowId(0)].tab_set.len();
        let first_document = app
            .native_runtime
            .document_id(app.active_native_view(WindowId(0)).unwrap().0)
            .unwrap();
        let second = app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        assert_eq!(app.windows[&WindowId(0)].tab_set.len(), tabs);
        assert_eq!(first, second);
        assert_eq!(app.document_store.view_count(first_document), Some(1));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_file_creation_is_race_safe_and_never_truncates_existing_bytes() {
        let dir =
            std::env::temp_dir().join(format!("aterm-config-editor-ensure-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("nested/aterm.toml");

        let first = ensure_config_file(&path).unwrap();
        assert!(first.is_absolute());
        assert_eq!(fs::read(&path).unwrap(), b"");

        let authored = b"# keep this comment\nfont_px = 15.0\n";
        fs::write(&path, authored).unwrap();
        let second = ensure_config_file(&path).unwrap();
        assert_eq!(second, first);
        assert_eq!(fs::read(&path).unwrap(), authored);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_file_creation_rejects_relative_authority_before_mutating() {
        let path = std::path::PathBuf::from(format!(
            ".aterm-relative-config-ensure-{}/aterm.toml",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());

        let error = ensure_config_file(&path).unwrap_err();

        assert!(error.contains("must be absolute"), "{error}");
        assert!(
            !path.parent().unwrap().exists(),
            "authority validation must precede directory creation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_file_creation_through_bound_parent_symlink_preserves_the_alias() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-ensure-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let real_parent = dir.join("real");
        let alias = dir.join("alias");
        fs::create_dir_all(&real_parent).unwrap();
        symlink(&real_parent, &alias).unwrap();
        let path = alias.join("missing/aterm.toml");

        let opened = ensure_config_file(&path).unwrap();

        assert_eq!(opened, path);
        assert_eq!(
            fs::read(real_parent.join("missing/aterm.toml")).unwrap(),
            b""
        );
        assert!(
            fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&alias).unwrap(), real_parent);
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn manual_final_config_symlink_opens_saves_target_and_preserves_link() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-final-link-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let config_dir = dir.join("config");
        let dotfiles_dir = dir.join("dotfiles");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&dotfiles_dir).unwrap();
        let target = dotfiles_dir.join("aterm.toml");
        let logical = config_dir.join("aterm.toml");
        let link_destination = std::path::PathBuf::from("../dotfiles/aterm.toml");
        fs::write(&target, "theme = \"Default\"\n").unwrap();
        symlink(&link_destination, &logical).unwrap();

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let opened = app
            .ensure_and_open_config_editor_path_in_window(wid, &logical)
            .unwrap();
        assert_eq!(opened, file_uri(&logical));
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "theme = \"Default\"\n"
        );
        let grant = app
            .native_documents
            .persistence
            .grant(document)
            .and_then(|grant| app.native_documents.grants.get(grant))
            .unwrap();
        assert_eq!(grant.logical_path(), logical);
        assert_eq!(grant.target().target_path(), target.canonicalize().unwrap());
        assert_eq!(
            app.native_config_service.bound_logical_path(),
            Some(logical.as_path())
        );

        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("theme = \"Nord\"\n".to_string())),
        )
        .unwrap();
        app.save_document_checkpoint(document, view).unwrap();

        assert!(
            fs::symlink_metadata(&logical)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&logical).unwrap(), link_destination);
        assert_eq!(fs::read_to_string(&target).unwrap(), "theme = \"Nord\"\n");
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "theme = \"Nord\"\n"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn manual_final_config_symlink_swap_conflicts_without_redirecting_save() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-final-link-swap-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.toml");
        let second = dir.join("second.toml");
        let logical = dir.join("aterm.toml");
        fs::write(&first, "theme = \"Default\"\n").unwrap();
        fs::write(&second, "theme = \"Dracula\"\n").unwrap();
        symlink("first.toml", &logical).unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&logical).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &logical)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("theme = \"Nord\"\n".to_string())),
        )
        .unwrap();

        fs::remove_file(&logical).unwrap();
        symlink("second.toml", &logical).unwrap();
        let error = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(error.contains("changed on disk"), "{error}");
        assert_eq!(fs::read_to_string(&first).unwrap(), "theme = \"Default\"\n");
        assert_eq!(
            fs::read_to_string(&second).unwrap(),
            "theme = \"Dracula\"\n"
        );
        assert_eq!(
            fs::read_link(&logical).unwrap(),
            std::path::PathBuf::from("second.toml")
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("Manual editor remains installed after link conflict");
        };
        assert!(
            state
                .status
                .as_deref()
                .is_some_and(|status| status.contains("changed on disk")),
            "conflict must be visible: {:?}",
            state.status
        );
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "theme = \"Default\"\n",
            "a conflicted draft must leave the admitted live config unchanged"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn manual_parent_config_symlink_swap_conflicts_without_redirecting_save() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-parent-link-swap-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first_dir = dir.join("first");
        let second_dir = dir.join("second");
        let alias = dir.join("config");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let first = first_dir.join("aterm.toml");
        let second = second_dir.join("aterm.toml");
        let logical = alias.join("aterm.toml");
        fs::write(&first, "theme = \"Default\"\n").unwrap();
        fs::write(&second, "theme = \"Dracula\"\n").unwrap();
        symlink("first", &alias).unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&logical).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &logical)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("theme = \"Nord\"\n".to_string())),
        )
        .unwrap();

        fs::remove_file(&alias).unwrap();
        symlink("second", &alias).unwrap();
        let error = app.save_document_checkpoint(document, view).unwrap_err();

        assert!(error.contains("changed on disk"), "{error}");
        assert_eq!(fs::read_to_string(&first).unwrap(), "theme = \"Default\"\n");
        assert_eq!(
            fs::read_to_string(&second).unwrap(),
            "theme = \"Dracula\"\n"
        );
        assert_eq!(
            fs::read_link(&alias).unwrap(),
            std::path::PathBuf::from("second")
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("Manual editor remains installed after parent-link conflict");
        };
        assert!(
            state
                .status
                .as_deref()
                .is_some_and(|status| status.contains("changed on disk")),
            "conflict must be visible: {:?}",
            state.status
        );
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "theme = \"Default\"\n",
            "a conflicted draft must leave the admitted live config unchanged"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_editor_uses_one_native_document_tab_without_spawning_a_session() {
        let dir =
            std::env::temp_dir().join(format!("aterm-config-editor-native-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("config/aterm/aterm.toml");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sessions = app.pool.sessions.len();

        let first_uri = app
            .ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let first = app.active_native_view(wid).expect("native config editor");
        let document = app
            .native_runtime
            .document_id(first.0)
            .expect("config document");
        let tabs = app.windows[&wid].tab_set.len();
        assert!(app.native_runtime.config_editor_enabled(first.0));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            ""
        );
        assert_eq!(app.pool.sessions.len(), sessions);

        let second_uri = app
            .ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        assert_eq!(second_uri, first_uri);
        assert_eq!(app.active_native_view(wid), Some(first));
        assert_eq!(app.windows[&wid].tab_set.len(), tabs);
        assert_eq!(app.pool.sessions.len(), sessions);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_editor_blocks_malformed_save_and_keeps_disk_bytes_durable() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-invalid-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("aterm.toml");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit(
                "font_px = \"definitely not a number\"\n".to_string(),
            )),
        )
        .unwrap();
        assert!(
            app.native_runtime
                .config_editor_save_error(document)
                .is_some()
        );
        let save = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .expect("editor save command");
        assert!(!save.enabled);

        let error = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(error.contains("Save blocked"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("config editor view must remain installed");
        };
        assert!(
            state
                .status
                .as_deref()
                .is_some_and(|status| status.contains("Save blocked"))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manual_config_save_synchronizes_the_service_before_watcher_delivery() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-immediate-sync-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "").unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("theme = \"Dracula\"\n".to_string())),
        )
        .unwrap();

        app.save_document_checkpoint(document, view).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Dracula\"\n");
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "theme = \"Dracula\"\n",
            "durable Manual completion synchronizes the process service inline"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manual_save_reports_busy_held_file_lock_preserves_draft_and_retries() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-held-save-lock-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("theme = \"Dracula\"\n".to_string())),
        )
        .unwrap();
        let lock_path = dir.join(".aterm.toml.aterm-write.lock");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        held.lock().unwrap();

        let started = std::time::Instant::now();
        let error = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(error.contains("busy"), "{error}");
        assert!(error.contains("retry Save"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"");
        assert_eq!(app.document_store.dirty(document), Some(true));
        let save = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .expect("Manual Save command");
        assert!(save.enabled, "busy failure remains explicitly retryable");

        drop(held);
        app.save_document_checkpoint(document, view)
            .expect("Manual Save retry succeeds after lock release");
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Dracula\"\n");
        assert_eq!(app.document_store.dirty(document), Some(false));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_startup_manual_save_binds_and_recovers_the_config_service() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-malformed-startup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "theme = [\n").unwrap();
        let mut app = App::headless_for_test();
        assert!(app.native_config_service.bound_logical_path().is_none());
        let wid = WindowId(0);

        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();

        assert_eq!(
            app.native_config_service.bound_logical_path(),
            Some(path.as_path())
        );
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::TextInput(TextInputEvent::SelectAll))
            .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("theme = \"Nord\"\n".to_string())),
        )
        .unwrap();
        app.save_document_checkpoint(document, view).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Nord\"\n");
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "theme = \"Nord\"\n",
            "valid Manual recovery is admitted immediately despite an unbound startup service"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_external_observation_refreshes_clean_manual_and_service_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-exact-observation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "font_px = 12.0\n").unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let document = app.native_runtime.config_editor_document().unwrap();

        let external = "# external comment only\nfont_px = 12.0\n";
        fs::write(&path, external).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&observation)
            .unwrap();
        let snapshot = app
            .sync_native_config_external_observation(observation)
            .unwrap()
            .expect("external observation is admitted outside a write");

        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            external
        );
        assert_eq!(snapshot.text.as_ref(), external);
        assert_eq!(snapshot.values().unwrap().get("font_px").unwrap(), "12");
        assert_eq!(app.document_store.dirty(document), Some(false));
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn byte_identical_observations_rebind_dirty_manual_without_losing_draft() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-equivalent-generation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let replacement = dir.join("replacement.toml");
        let baseline_text = "font_px = 12.0\n";
        fs::write(&path, baseline_text).unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let old_disk = app.native_documents.persistence.observed(document).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("# local draft\n".to_string())),
        )
        .unwrap();
        let draft = app.document_store.snapshot(document).unwrap();
        assert!(app.document_store.dirty(document).unwrap());

        fs::write(&replacement, baseline_text).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let watch_observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        assert_eq!(
            crate::native_document_io::detect_version_conflict(
                old_disk,
                watch_observation.baseline.observed
            ),
            Some(crate::native_document_io::VersionConflict::Identity)
        );

        app.refresh_open_config_editor_observation(&watch_observation)
            .unwrap();
        let after_watch = app.document_store.snapshot(document).unwrap();
        assert_eq!(after_watch.seq, draft.seq);
        assert_eq!(
            after_watch.text, draft.text,
            "watch rebind must not touch the dirty Manual draft"
        );
        assert_eq!(
            app.native_documents.persistence.observed(document),
            Some(watch_observation.baseline.observed)
        );
        assert!(!app.native_documents.disk_conflicts.contains(&document));

        // Race the next watcher cycle: Save's host preflight receives another
        // same-byte generation directly and must make the same safe decision.
        fs::write(&replacement, baseline_text).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let save_observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        assert_eq!(
            crate::native_document_io::detect_version_conflict(
                watch_observation.baseline.observed,
                save_observation.baseline.observed
            ),
            Some(crate::native_document_io::VersionConflict::Identity)
        );

        let first_save = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(
            first_save.contains("Disk version refreshed"),
            "{first_save}"
        );
        assert!(first_save.contains("Save is available"), "{first_save}");

        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!(after.seq, draft.seq);
        assert_eq!(
            after.text, draft.text,
            "the dirty Manual draft is untouched"
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(
            app.native_documents.persistence.observed(document),
            Some(save_observation.baseline.observed),
            "the save baseline advances to the byte-equivalent disk generation"
        );
        assert!(!app.native_documents.disk_conflicts.contains(&document));
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("Manual editor remains installed");
        };
        assert!(!editor.disk_conflict);
        let save = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .expect("Save command");
        assert!(save.enabled, "Manual Save is available after safe rebind");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert!(
            state.status.as_deref().is_some_and(|status| {
                status.contains("local edits are preserved") && status.contains("Save is available")
            }),
            "unexpected Manual status: {:?}",
            state.status
        );

        app.save_document_checkpoint(document, view).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), draft.text.as_ref());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn byte_identical_symlink_recreation_does_not_rebind_manual_capability() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-equivalent-relink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.toml");
        let logical = dir.join("aterm.toml");
        let old_link = dir.join("aterm.old-link");
        let baseline_text = "font_px = 12.0\n";
        fs::write(&target, baseline_text).unwrap();
        symlink("target.toml", &logical).unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&logical).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &logical)
            .unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("# local draft\n".to_string())),
        )
        .unwrap();
        let draft = app.document_store.snapshot(document).unwrap();

        fs::rename(&logical, &old_link).unwrap();
        symlink("target.toml", &logical).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&logical, false)
                .unwrap();
        let error = app
            .refresh_open_config_editor_observation(&observation)
            .unwrap_err();

        assert!(error.contains("file binding changed"), "{error}");
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text,
            draft.text
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert!(app.native_documents.disk_conflicts.contains(&document));
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("Manual editor remains installed");
        };
        assert!(editor.disk_conflict);
        assert_eq!(fs::read_to_string(&target).unwrap(), baseline_text);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_toml_observation_reaches_manual_but_not_live_config() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-malformed-observation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let initial = "font_px = 12.0\n";
        fs::write(&path, initial).unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let before = app.native_config_service.snapshot();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.config_editor_document().unwrap();

        let malformed = "font_px = [\n";
        fs::write(&path, malformed).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        app.note_config_watch_candidate(observation.baseline.clone());
        app.prepare_native_config_external_observation(observation);

        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            malformed
        );
        assert!(
            app.native_runtime
                .config_editor_save_error(document)
                .is_some()
        );
        let analysis = app
            .native_runtime
            .config_editor_analysis(instance)
            .expect("exact malformed bytes are analyzed in Manual");
        assert!(analysis.has_errors());
        assert!(
            analysis
                .summary()
                .is_some_and(|summary| summary.contains("Ln 1"))
        );
        app.dispatch_native_event(
            wid,
            AppEvent::EditorConfigDiagnosticNavigate { previous: false },
        )
        .unwrap();
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("Manual editor view remains installed");
        };
        let buffer = state
            .buffer
            .as_ref()
            .expect("Manual buffer remains attached");
        assert_eq!(
            buffer.primary_selection().head,
            malformed.len() - 1,
            "F8 targets the authored line end, not the synthetic blank line after its newline"
        );
        assert_eq!(buffer.viewport_anchor, 0);
        let after = app.native_config_service.snapshot();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.text, before.text);
        assert!(app.native_config_service.reconciliation_required());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_config_observation_leaves_manual_and_live_service_unchanged() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-unreadable-observation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let initial = "font_px = 12.0\n";
        fs::write(&path, initial).unwrap();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let document = app.native_runtime.config_editor_document().unwrap();
        let before_document = app.document_store.snapshot(document).unwrap();
        let before_service = app.native_config_service.snapshot();

        fs::write(&path, [0xff]).unwrap();
        let error =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap_err();

        assert!(error.contains("not valid UTF-8"), "{error}");
        let after_document = app.document_store.snapshot(document).unwrap();
        assert_eq!(after_document.seq, before_document.seq);
        assert_eq!(after_document.text, before_document.text);
        assert_eq!(app.native_config_service.snapshot(), before_service);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_publication_reloads_clean_manual_bytes_and_preserves_dirty_drafts() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-publication-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "font_px = 12.0\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        let clean_external = "# changed by native Settings\nfont_px = 14.0\n";
        fs::write(&path, clean_external).unwrap();
        let clean_observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&clean_observation)
            .unwrap();
        let clean_snapshot =
            crate::native_config_service::VersionedConfigService::new(clean_external.to_string())
                .unwrap()
                .snapshot();
        app.publish_native_config_snapshot(&clean_snapshot);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            clean_external
        );
        assert_eq!(app.document_store.dirty(document), Some(false));

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("# local draft\n".to_string())),
        )
        .unwrap();
        let local = app.document_store.snapshot(document).unwrap().text.clone();
        let dirty_external = "# changed externally again\nfont_px = 16.0\n";
        fs::write(&path, dirty_external).unwrap();
        let dirty_observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&dirty_observation)
            .unwrap();
        let dirty_snapshot =
            crate::native_config_service::VersionedConfigService::new(dirty_external.to_string())
                .unwrap()
                .snapshot();
        app.publish_native_config_snapshot(&dirty_snapshot);

        assert_eq!(app.document_store.snapshot(document).unwrap().text, local);
        assert_eq!(app.document_store.dirty(document), Some(true));
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("config editor must remain installed");
        };
        assert!(editor.recovery_status.as_deref().is_some_and(|status| {
            status.contains("changed on disk") && status.contains("preserved")
        }));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn config_completion_round_trips_through_the_native_editor_workspace() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-completion-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("aterm.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "win").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();

        let assist = crate::native_config_language::assist("win", 3);
        let index = assist
            .completions
            .iter()
            .position(|candidate| candidate.insertion.starts_with("window_theme ="))
            .expect("metadata-derived window theme completion");
        let document = app.native_runtime.document_id(instance).unwrap();
        let snapshot = app.document_store.snapshot(document).unwrap();
        let action = crate::native_config_language::config_completion_action(
            crate::native_config_language::ConfigCompletionContext::new(
                document.get(),
                snapshot.seq.0,
                3,
            ),
            index,
            &assist.completions[index],
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state_mut(view).unwrap() else {
            panic!("config editor view");
        };
        state.common.presentation_revision = state.common.presentation_revision.saturating_add(1);
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(action),
                value: None,
            }),
        )
        .unwrap();

        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "window_theme = \"auto\""
        );
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &crate::native_editor::Selection {
                anchor: 16,
                head: 20,
            },
            "the stale-checked action carries the editable sample selection"
        );
        assert!(app.editor_workspace.can_undo(document));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn accepted_config_samples_select_the_editable_payload_for_immediate_typing() {
        for (case, prefix, insertion, typed, expected) in [
            (
                "shell",
                "shell",
                "shell = \"\"",
                "/bin/zsh",
                "shell = \"/bin/zsh\"",
            ),
            (
                "color",
                "foreground",
                "foreground = \"#ffffff\"",
                "#112233",
                "foreground = \"#112233\"",
            ),
            (
                "list",
                "shell_args",
                "shell_args = []",
                "\"--login\"",
                "shell_args = [\"--login\"]",
            ),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "aterm-config-editor-post-completion-{case}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("aterm.toml");
            fs::write(&path, prefix).unwrap();
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.ensure_and_open_config_editor_path_in_window(wid, &path)
                .unwrap();
            let (instance, view) = app.active_native_view(wid).unwrap();
            app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
                .unwrap();

            let assist = crate::native_config_language::assist(prefix, prefix.len());
            let (index, completion) = assist
                .completions
                .iter()
                .enumerate()
                .find(|(_, completion)| completion.insertion == insertion)
                .expect("shape-specific key completion");
            let document = app.native_runtime.document_id(instance).unwrap();
            let snapshot = app.document_store.snapshot(document).unwrap();
            let context = crate::native_config_language::ConfigCompletionContext::new(
                document.get(),
                snapshot.seq.0,
                prefix.len(),
            );
            let action =
                crate::native_config_language::config_completion_action(context, index, completion);
            let expected_selection = crate::native_editor::Selection {
                anchor: completion.replacement.start + completion.post_insert_selection.start,
                head: completion.replacement.start + completion.post_insert_selection.end,
            };
            app.dispatch_native_event(
                wid,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new(action),
                    value: None,
                }),
            )
            .unwrap();
            assert_eq!(
                editor_buffer(&app, view).primary_selection(),
                &expected_selection
            );

            app.dispatch_native_event(
                wid,
                AppEvent::TextInput(TextInputEvent::Commit(typed.to_string())),
            )
            .unwrap();
            let completed = app.document_store.snapshot(document).unwrap();
            assert_eq!(completed.text.as_ref(), expected);
            assert!(
                !crate::native_config_language::analyze(&completed.text).has_errors(),
                "immediate typing after {case} completion must retain valid TOML"
            );
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn focused_config_completion_enter_activates_its_semantic_button() {
        assert_focused_config_completion_activates(NamedKey::Enter, "enter");
    }

    #[test]
    fn focused_config_completion_space_activates_its_semantic_button() {
        assert_focused_config_completion_activates(NamedKey::Space, "space");
    }

    #[test]
    fn presented_config_assist_does_not_steal_enter_or_vertical_motion() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-ordinary-keys-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "theme = \"Default\"\ncursor_blink = true\n";
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
        );
        let after_down = editor_buffer(&app, view).primary_selection().head;
        assert!(after_down > source.find('\n').unwrap());
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert!(state.config_completion_interaction.is_none());

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        assert_eq!(
            app.document_store
                .snapshot(document)
                .unwrap()
                .text
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count(),
            3,
            "Enter remains a newline even while assistance is presented"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn escape_dismisses_only_the_exact_config_completion_context() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-dismiss-completion-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "win").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        let context = app
            .editor_config_completion_context(instance, view)
            .unwrap();

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_dismissed, Some(context));
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantics
                .iter()
                .all(|node| !node.key.as_str().starts_with("editor/config-completion/"))
        );

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "win\n"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn escape_dismisses_help_only_config_assistance() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-dismiss-help-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "matrix_rain.density = 12").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        let context = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        assert_eq!(
            app.native_runtime
                .cached_config_completion_count(instance, context),
            0
        );
        assert!(
            app.native_runtime
                .cached_config_assist_present(instance, context)
        );

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_dismissed, Some(context));
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                .is_none()
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_analysis_drives_first_tab_before_any_assist_render() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-unrendered-tab-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "window_theme = ";
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        let context = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        clear_exact_config_assist_cache(&app, instance, context);
        let before = app.document_store.snapshot(document).unwrap();
        let caret = editor_buffer(&app, view).primary_selection().head;

        drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));

        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, &after.text), (before.seq, &before.text));
        assert_eq!(editor_buffer(&app, view).primary_selection().head, caret);
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_interaction, Some(context));
        assert_eq!(state.config_completion_selected, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_analysis_drives_ctrl_space_before_any_assist_render() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-unrendered-ctrl-space-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "window_theme = ";
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        let context = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        clear_exact_config_assist_cache(&app, instance, context);
        let before = app.document_store.snapshot(document).unwrap();
        let caret = editor_buffer(&app, view).primary_selection().head;

        drive_native(&mut app, key(Key::Named(NamedKey::Space), Modifiers::CTRL));

        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, &after.text), (before.seq, &before.text));
        assert_eq!(editor_buffer(&app, view).primary_selection().head, caret);
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_interaction, Some(context));
        assert_eq!(state.config_completion_selected, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn exact_help_only_analysis_dismisses_on_escape_without_an_assist_render() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-unrendered-help-escape-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "matrix_rain.density = 12";
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        let context = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        clear_exact_config_assist_cache(&app, instance, context);
        let before = app.document_store.snapshot(document).unwrap();

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        );

        let after = app.document_store.snapshot(document).unwrap();
        assert_eq!((after.seq, &after.text), (before.seq, &before.text));
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_dismissed, Some(context));
        assert!(state.config_completion_interaction.is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ctrl_space_reopens_dismissed_candidate_and_help_only_contexts() {
        for (suffix, source) in [
            ("candidates", "window_theme = "),
            ("help", "matrix_rain.density = 12"),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "aterm-config-editor-reopen-{suffix}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("aterm.toml");
            fs::write(&path, source).unwrap();
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.ensure_and_open_config_editor_path_in_window(wid, &path)
                .unwrap();
            let (instance, view) = app.active_native_view(wid).unwrap();
            let document = app.native_runtime.document_id(instance).unwrap();
            app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
                .unwrap();
            let context = app
                .editor_config_completion_context(instance, view)
                .unwrap();
            app.native_runtime.clear_config_assist_cache(instance);
            let before = app.document_store.snapshot(document).unwrap();

            drive_native(
                &mut app,
                key(Key::Named(NamedKey::Escape), Modifiers::empty()),
            );
            let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
                panic!("config editor view");
            };
            assert_eq!(state.config_completion_dismissed, Some(context));
            app.native_runtime.clear_config_assist_cache(instance);

            drive_native(&mut app, key(Key::Named(NamedKey::Space), Modifiers::CTRL));
            let after = app.document_store.snapshot(document).unwrap();
            assert_eq!((after.seq, &after.text), (before.seq, &before.text));
            let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
                panic!("config editor view");
            };
            assert_eq!(state.config_completion_dismissed, None);
            assert_eq!(state.config_completion_interaction, Some(context));
            assert_eq!(state.config_completion_selected, 0);
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn tab_and_arrows_reach_a_nonfirst_config_completion_without_a_pointer() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-keyboard-completion-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "window_theme = ";
        fs::write(&path, source).unwrap();
        let assist = crate::native_config_language::assist(source, source.len());
        assert!(assist.completions.len() > 1);
        let second = &assist.completions[1];
        let mut expected = source.to_string();
        expected.replace_range(second.replacement.clone(), &second.insertion);

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));

        drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::SHIFT));
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert!(state.config_completion_interaction.is_none());
        drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
        );
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_selected, 1);
        assert!(state.config_completion_interaction.is_some());
        drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::SHIFT));
        let document = app.native_runtime.config_editor_document().unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            source,
            "Shift-Tab remains reverse focus traversal while completions are active"
        );
        drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));

        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            expected
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn caret_only_context_change_resets_stale_completion_selection_for_paint_and_input() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-caret-context-selection-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "window_theme = ";
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        let stale = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        let snapshot = app
            .document_store
            .snapshot(app.native_runtime.document_id(instance).unwrap())
            .unwrap();
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state_mut(view) else {
            panic!("config editor view");
        };
        state.config_completion_interaction = Some(stale);
        state.config_completion_selected = 7;
        assert!(
            state
                .buffer
                .as_mut()
                .unwrap()
                .pointer_select(&snapshot.text, 0, false, 4),
            "the fixture changes only the caret"
        );
        let current = app
            .editor_config_completion_context(instance, view)
            .unwrap();
        assert_ne!(current, stale);

        assert!(app.prepare_native_input_scratch(wid));
        let compiled = &app
            .retained_native_leaf_artifact(wid, view, false)
            .unwrap()
            .compiled;
        let selected = compiled
            .semantics
            .iter()
            .filter(|node| {
                node.key.as_str().starts_with("editor/config-completion/")
                    && node.state.is_some_and(|state| state.selected)
            })
            .map(|node| node.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(selected, ["editor/config-completion/0"]);

        drive_native(&mut app, key(Key::Named(NamedKey::Tab), Modifiers::empty()));
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_interaction, Some(current));
        assert_eq!(state.config_completion_selected, 0);
        assert_eq!(editor_buffer(&app, view).primary_selection().head, 0);
        assert_eq!(
            app.document_store
                .snapshot(snapshot.id)
                .unwrap()
                .text
                .as_ref(),
            source
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn pending_editor_chord_owns_escape_tab_and_ctrl_space_before_config_assist() {
        for (suffix, terminator) in [
            (
                "escape",
                key(Key::Named(NamedKey::Escape), Modifiers::empty()),
            ),
            ("tab", key(Key::Named(NamedKey::Tab), Modifiers::empty())),
            (
                "ctrl-space",
                key(Key::Named(NamedKey::Space), Modifiers::CTRL),
            ),
        ] {
            let dir = std::env::temp_dir().join(format!(
                "aterm-config-editor-chord-assist-{suffix}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("aterm.toml");
            let source = "win";
            fs::write(&path, source).unwrap();
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            app.ensure_and_open_config_editor_path_in_window(wid, &path)
                .unwrap();
            let (_, view) = app.active_native_view(wid).unwrap();
            app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
                .unwrap();
            assert!(app.prepare_native_input_scratch(wid));
            assert!(
                app.retained_native_leaf_artifact(wid, view, false)
                    .unwrap()
                    .compiled
                    .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                    .is_some(),
                "the regression starts with visible config assistance"
            );

            drive_native(&mut app, key(Key::Character('x'), Modifiers::CTRL));
            assert!(editor_buffer(&app, view).chord_pending());
            assert!(app.prepare_native_input_scratch(wid));
            assert!(
                app.retained_native_leaf_artifact(wid, view, false)
                    .unwrap()
                    .compiled
                    .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                    .is_none(),
                "a pending editor chord suppresses config assistance"
            );

            drive_native(&mut app, terminator);
            assert!(
                !editor_buffer(&app, view).chord_pending(),
                "{suffix} is reduced as the second chord stroke"
            );
            let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
                panic!("config editor view");
            };
            assert!(state.config_completion_interaction.is_none());
            assert_eq!(
                app.document_store
                    .snapshot(app.native_runtime.config_editor_document().unwrap())
                    .unwrap()
                    .text
                    .as_ref(),
                source
            );

            if suffix == "escape" {
                drive_native(&mut app, key(Key::Character('s'), Modifiers::CTRL));
                assert!(matches!(
                    &editor_buffer(&app, view).minibuffer,
                    Minibuffer::Search { .. }
                ));
            }
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn config_assist_yields_to_search_and_goto_minibuffers() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-minibuffer-assist-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        fs::write(&path, "win").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (_, view) = app.active_native_view(wid).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        assert!(app.prepare_native_input_scratch(wid));
        assert!(
            app.retained_native_leaf_artifact(wid, view, false)
                .unwrap()
                .compiled
                .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                .is_some()
        );

        for command in [EditorCommand::IncrementalSearch, EditorCommand::GotoLine] {
            app.dispatch_native_event(wid, AppEvent::EditorCommand(command))
                .unwrap();
            assert!(app.prepare_native_input_scratch(wid));
            assert!(
                app.retained_native_leaf_artifact(wid, view, false)
                    .unwrap()
                    .compiled
                    .semantic(&crate::native_ui::UiKey::new("editor/config-assist"))
                    .is_none()
            );
            drive_native(
                &mut app,
                key(Key::Named(NamedKey::Escape), Modifiers::empty()),
            );
            let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
                panic!("config editor view");
            };
            assert!(
                state
                    .buffer
                    .as_ref()
                    .is_some_and(|buffer| !buffer.minibuffer_active()),
                "one Escape closes the active editor minibuffer"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn f8_reveals_a_single_offscreen_config_problem() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-single-problem-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = format!("{}future_single_setting = 1\n", "# spacer\n".repeat(500));
        fs::write(&path, &source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let analysis = app
            .native_runtime
            .config_editor_analysis(instance)
            .unwrap()
            .clone();
        assert_eq!(analysis.diagnostic_count(), 1);
        let target = analysis.diagnostic_at(0).unwrap().bytes.start;

        drive_native(&mut app, key(Key::Named(NamedKey::F8), Modifiers::empty()));
        let buffer = editor_buffer(&app, view);
        assert_eq!(buffer.primary_selection().head, target);
        assert!(
            buffer.viewport_anchor > 0,
            "the off-screen problem must be revealed"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_manual_handoff_reveals_an_existing_key_and_seeds_absent_search() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-target-handoff-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = format!("{}shell = \"/bin/zsh\"\n", "# spacer\n".repeat(500));
        fs::write(&path, &source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (_, original_view) = app.active_native_view(wid).unwrap();

        app.ensure_and_open_config_editor_path_at_in_window(
            wid,
            &path,
            Some(&crate::native_app::ConfigEditorTarget::Key(
                "shell".to_string(),
            )),
        )
        .unwrap();
        let (_, reopened_view) = app.active_native_view(wid).unwrap();
        assert_eq!(reopened_view, original_view, "the canonical tab is reused");
        let value_start = source.find("\"/bin/zsh\"").unwrap();
        let buffer = editor_buffer(&app, reopened_view);
        assert_eq!(
            buffer.primary_selection().range(),
            value_start..value_start + "\"/bin/zsh\"".len()
        );
        assert!(buffer.viewport_anchor > 0, "the distant target is revealed");

        app.ensure_and_open_config_editor_path_at_in_window(
            wid,
            &path,
            Some(&crate::native_app::ConfigEditorTarget::Key(
                "gpu".to_string(),
            )),
        )
        .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view")
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().minibuffer,
            crate::native_editor::Minibuffer::Search {
                query: "gpu".to_string(),
                origin: value_start + "\"/bin/zsh\"".len(),
            }
        );
        assert!(
            state
                .status
                .as_deref()
                .unwrap()
                .contains("not authored yet"),
            "absent target explains the ready search/completion fallback"
        );
        assert!(app.native_runtime.config_editor_enabled(instance));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn f8_cycles_offscreen_problems_and_keeps_the_full_semantic_message() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-problem-cycle-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let long_key = format!("future_{}", "diagnostic_".repeat(24));
        let source = format!(
            "future_first_setting = 1\n{}{} = 2\n",
            "# spacer\n".repeat(500),
            long_key
        );
        fs::write(&path, &source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let analysis = app
            .native_runtime
            .config_editor_analysis(instance)
            .unwrap()
            .clone();
        assert_eq!(analysis.diagnostic_count(), 2);
        let first = analysis.diagnostic_at(0).unwrap().bytes.start;
        let second = analysis.diagnostic_at(1).unwrap().bytes.start;

        assert!(app.prepare_native_input_scratch(wid));
        let controls = app
            .retained_native_leaf_artifact(wid, view, false)
            .unwrap()
            .compiled;
        for (key, action) in [
            (
                "editor/config-problem-previous-button",
                "editor/config-problem-previous",
            ),
            (
                "editor/config-problem-next-button",
                "editor/config-problem-next",
            ),
        ] {
            let semantic = controls
                .semantic(&crate::native_ui::UiKey::new(key))
                .expect("visible config problem navigation");
            assert_eq!(
                semantic
                    .action
                    .as_ref()
                    .map(crate::native_ui::ActionId::as_str),
                Some(action)
            );
            assert!(semantic.state.is_some_and(|state| state.enabled));
        }

        drive_native(&mut app, key(Key::Named(NamedKey::F8), Modifiers::empty()));
        assert_eq!(editor_buffer(&app, view).primary_selection().head, second);
        assert!(app.prepare_native_input_scratch(wid));
        let artifact = app.retained_native_leaf_artifact(wid, view, false).unwrap();
        let status = artifact
            .compiled
            .paint
            .iter()
            .find_map(|node| match &node.content {
                crate::native_ui::UiContent::TextViewport(spec) => Some(spec),
                _ => None,
            })
            .unwrap();
        let semantic = status.semantic_status.as_deref().unwrap();
        assert!(semantic.contains("Problem 2 of 2"));
        assert!(semantic.contains(&long_key));
        assert_ne!(status.status.as_deref(), Some(semantic));

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/config-problem-previous"),
                value: None,
            }),
        )
        .unwrap();
        assert_eq!(
            editor_buffer(&app, view).primary_selection().head,
            first,
            "Previous Problem button converges with Shift-F8"
        );

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/config-problem-next"),
                value: None,
            }),
        )
        .unwrap();
        assert_eq!(
            editor_buffer(&app, view).primary_selection().head,
            second,
            "Next Problem button converges with F8"
        );

        drive_native(&mut app, key(Key::Named(NamedKey::F8), Modifiers::SHIFT));
        assert_eq!(editor_buffer(&app, view).primary_selection().head, first);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ordinary_editor_buffer_enter_still_inserts_a_newline() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-editor-ordinary-enter-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        fs::write(&path, "abc").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();
        let state = app.native_runtime.view_state_mut(view).unwrap();
        state.common_mut().last_focus = Some(crate::native_ui::UiKey::new("editor/buffer"));

        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );

        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "abc\n"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_set_selection_replaces_the_primary_without_mutating_document() {
        let dir =
            std::env::temp_dir().join(format!("aterm-editor-set-selection-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unicode.txt");
        fs::write(&path, "aéz").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let before = app.document_store.snapshot(document).unwrap();
        let AppViewState::Editor(state) = app.native_runtime.view_state_mut(view).unwrap() else {
            panic!("editor view");
        };
        let buffer = state.buffer.as_mut().unwrap();
        buffer.selections = vec![Selection::caret(0), Selection::caret(before.text.len())];
        buffer.primary = 1;

        app.dispatch_native_event(wid, AppEvent::EditorSetSelection { anchor: 1, head: 3 })
            .unwrap();

        let buffer = editor_buffer(&app, view);
        assert_eq!(buffer.selections, vec![Selection { anchor: 1, head: 3 }]);
        assert_eq!(buffer.primary, 0);
        let selected = app.document_store.snapshot(document).unwrap();
        assert_eq!(selected.seq, before.seq);
        assert_eq!(selected.text, before.text);
        assert_eq!(app.document_store.dirty(document), Some(false));

        app.dispatch_native_event(wid, AppEvent::EditorSetSelection { anchor: 1, head: 2 })
            .unwrap();
        assert_eq!(
            editor_buffer(&app, view).selections,
            vec![Selection { anchor: 1, head: 3 }],
            "a non-UTF-8-boundary accessibility selection is rejected"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_rendered_config_completion_cannot_retarget_a_new_caret() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-stale-completion-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("aterm.toml");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "win\nthe").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.ensure_and_open_config_editor_path_in_window(wid, &path)
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineEnd))
            .unwrap();

        assert!(app.prepare_native_input_scratch(wid));
        let stale_action = app
            .retained_native_leaf_artifact(wid, view, false)
            .expect("compiled native config editor")
            .compiled
            .semantics
            .iter()
            .find(|node| node.label.starts_with("Insert window_theme"))
            .and_then(|node| node.action.clone())
            .expect("rendered window-theme completion action");
        let stale_index = stale_action
            .as_str()
            .strip_prefix(crate::native_config_language::CONFIG_COMPLETION_ACTION_PREFIX)
            .and_then(|suffix| suffix.split('/').next())
            .and_then(|index| index.parse::<usize>().ok())
            .expect("bound completion index");
        let before_move = app.document_store.snapshot(document).unwrap();

        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::MoveLineDown))
            .unwrap();
        let moved = app.document_store.snapshot(document).unwrap();
        assert_eq!(moved.seq, before_move.seq, "caret movement does not edit");
        let caret = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Editor(state) => state.buffer.as_ref().unwrap().primary_selection().head,
            _ => panic!("config editor view"),
        };
        let current = crate::native_config_language::assist(&moved.text, caret);
        assert!(
            current.completions[stale_index]
                .insertion
                .starts_with("theme ="),
            "the same index now denotes a different visible candidate"
        );

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: stale_action,
                value: None,
            }),
        )
        .unwrap();
        let rejected = app.document_store.snapshot(document).unwrap();
        assert_eq!(rejected.seq, moved.seq);
        assert_eq!(rejected.text, moved.text);
        let status = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Editor(state) => state.status.as_deref(),
            _ => None,
        };
        assert!(status.is_some_and(|message| message.contains("completion became stale")));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_basenames_refresh_inactive_tab_titles_and_keep_exact_uri_tooltips() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-app-document-identities-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let one_dir = dir.join("one");
        let two_dir = dir.join("two");
        fs::create_dir_all(&one_dir).unwrap();
        fs::create_dir_all(&two_dir).unwrap();
        let one = one_dir.join("README.md");
        let two = two_dir.join("README.md");
        fs::write(&one, "# One\n").unwrap();
        fs::write(&two, "# Two\n").unwrap();
        let one_uri = file_uri(&one);
        let two_uri = file_uri(&two);
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Markdown, &one_uri).unwrap();
        app.open_document_tab(AppKind::Editor, &one_uri).unwrap();
        app.open_document_tab(AppKind::Markdown, &two_uri).unwrap();

        let documents = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .filter(|tab| tab.presentation.title.starts_with("README.md"))
            .map(|tab| {
                (
                    tab.presentation.title.clone(),
                    tab.presentation.tooltip.clone().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(documents.len(), 3);
        assert_eq!(
            documents
                .iter()
                .filter(|(title, _)| title == "README.md — one")
                .count(),
            2,
            "Markdown and Editor for one canonical document share one suffix"
        );
        assert_eq!(
            documents
                .iter()
                .filter(|(title, _)| title == "README.md — two")
                .count(),
            1
        );
        assert!(
            documents
                .iter()
                .any(|(_, tooltip)| tooltip == &format!("Markdown · {one_uri}"))
        );
        assert!(
            documents
                .iter()
                .any(|(_, tooltip)| tooltip == &format!("Editor · {one_uri}"))
        );
        assert!(
            documents
                .iter()
                .any(|(_, tooltip)| tooltip == &format!("Markdown · {two_uri}"))
        );
        app.refresh_window_tabs(wid);
        assert_eq!(
            app.windows[&wid].tab_set.tabs()[1].presentation.title,
            "README.md — one"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_undo_redo_command_availability_tracks_shipping_history_after_every_reduction() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-editor-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.txt");
        fs::write(&path, "abc").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let enabled = |app: &App, action: &str| {
            app.native_runtime
                .commands(instance, view)
                .unwrap()
                .into_iter()
                .find(|command| command.id.as_str() == action)
                .expect("editor history command")
                .enabled
        };
        assert!(!enabled(&app, "editor/undo"));
        assert!(!enabled(&app, "editor/redo"));

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("x".to_string())),
        )
        .unwrap();
        assert!(enabled(&app, "editor/undo"));
        assert!(!enabled(&app, "editor/redo"));

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/undo"),
                value: None,
            }),
        )
        .unwrap();
        assert!(!enabled(&app, "editor/undo"));
        assert!(enabled(&app, "editor/redo"));

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/redo"),
                value: None,
            }),
        )
        .unwrap();
        assert!(enabled(&app, "editor/undo"));
        assert!(!enabled(&app, "editor/redo"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn markdown_and_editor_share_document_but_not_view_state() {
        let dir = std::env::temp_dir().join(format!("aterm-app-cross-mode-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared.md");
        fs::write(&path, "shared\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        let markdown = app
            .native_runtime
            .document_id(app.active_native_view(WindowId(0)).unwrap().0)
            .unwrap();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (editor_instance, editor_view) = app.active_native_view(WindowId(0)).unwrap();
        assert_eq!(
            app.native_runtime.document_id(editor_instance),
            Some(markdown)
        );
        assert!(matches!(
            app.native_runtime.view_state(editor_view),
            Some(AppViewState::Editor(state)) if state.buffer.is_some()
        ));
        assert_eq!(app.document_store.view_count(markdown), Some(2));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn markdown_edit_action_completes_its_reader_status_after_editor_focus() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-edit-handoff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("handoff.md");
        fs::write(&path, "# Handoff\n\nBody\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        let (markdown_instance, markdown_view) = app.active_native_view(wid).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("markdown/edit"),
                value: None,
            }),
        )
        .unwrap();

        let (active_instance, _) = app.active_native_view(wid).unwrap();
        assert_eq!(
            app.native_runtime.app(active_instance).map(NativeApp::kind),
            Some(AppKind::Editor)
        );
        assert_eq!(
            app.native_runtime.document_id(active_instance),
            app.native_runtime.document_id(markdown_instance)
        );
        assert!(matches!(
            app.native_runtime.view_state(markdown_view),
            Some(AppViewState::Markdown(state))
                if state.notice.as_deref() == Some("Editor opened")
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_input_mutates_shared_document_and_save_advances_checkpoint() {
        let dir = std::env::temp_dir().join(format!("aterm-app-edit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("editable.md");
        fs::write(&path, "shared\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let wid = WindowId(0);
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        assert_eq!(
            app.dispatch_native_event(
                wid,
                AppEvent::TextInput(TextInputEvent::Commit("native ".to_string())),
            )
            .unwrap(),
            EventResult::Handled
        );
        let edited = app.document_store.snapshot(document).unwrap();
        assert_eq!(edited.text.as_ref(), "native shared\n");
        assert_eq!(app.document_store.dirty(document), Some(true));

        app.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::Save))
            .unwrap();
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert_eq!(fs::read_to_string(&path).unwrap(), "native shared\n");
        assert_eq!(
            app.document_store.checkpoint_seq(document),
            Some(app.document_store.snapshot(document).unwrap().seq)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn completion_does_not_report_saved_when_newer_edits_were_not_requested() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-app-save-newer-modified-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("modified.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("saved ".into())),
        )
        .unwrap();
        let first = begin_inflight_save_for_test(&mut app, document);
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("newer ".into())),
        )
        .unwrap();

        finish_inflight_save_for_test(&mut app, document, view, first).unwrap();

        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(fs::read_to_string(&path).unwrap(), "saved base\n");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view must remain installed");
        };
        assert_eq!(
            state.status.as_deref(),
            Some("Modified · newer changes are not saved")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn repeated_save_during_inflight_checkpoint_persists_the_latest_sequence() {
        let dir = std::env::temp_dir().join(format!("aterm-app-save-latch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latched.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("first ".into())),
        )
        .unwrap();
        let first = begin_inflight_save_for_test(&mut app, document);

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("second ".into())),
        )
        .unwrap();
        app.save_document_checkpoint(document, view).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("third ".into())),
        )
        .unwrap();
        app.save_document_checkpoint(document, view).unwrap();
        let latest = app.document_store.snapshot(document).unwrap();
        assert_eq!(
            app.native_documents
                .pending_saves
                .get(&document)
                .map(|intent| intent.seq),
            Some(latest.seq)
        );

        finish_inflight_save_for_test(&mut app, document, view, first).unwrap();

        assert!(!app.native_documents.inflight.contains(&document));
        assert!(!app.native_documents.pending_saves.contains_key(&document));
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert_eq!(
            app.document_store.checkpoint_seq(document),
            Some(latest.seq)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view must remain installed");
        };
        assert_eq!(state.status.as_deref(), Some("Saved"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn close_during_inflight_save_pumps_the_frozen_close_sequence() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-close-save-latch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("close.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("first ".into())),
        )
        .unwrap();
        let first = begin_inflight_save_for_test(&mut app, document);
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("latest ".into())),
        )
        .unwrap();
        let latest = app.document_store.snapshot(document).unwrap();

        assert!(
            !app.prepare_window_document_shutdown_inner(wid, true)
                .unwrap()
        );
        assert!(app.native_documents.pending_saves.contains_key(&document));
        assert!(
            app.native_documents
                .pending_window_closes
                .contains_key(&wid)
        );
        assert_eq!(app.document_store.view_count(document), Some(1));

        finish_inflight_save_for_test(&mut app, document, view, first).unwrap();

        assert_eq!(app.document_store.view_count(document), Some(1));
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![wid])
        );
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert!(
            !app.native_documents
                .pending_window_closes
                .contains_key(&wid)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn async_window_close_revalidates_settings_drafts_before_document_detach() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-window-close-native-revalidate-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("close.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (editor, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(editor).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("durable ".into())),
        )
        .unwrap();
        assert!(
            !app.prepare_window_document_shutdown_inner(wid, false)
                .unwrap()
        );

        // The window stays interactive while its checkpoint runs. A native-app
        // draft created after the initial close gesture must therefore be part of
        // the completion-time transaction, not silently bypassed.
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings_view) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings_view);
        app.save_document_checkpoint(document, editor_view).unwrap();

        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![])
        );
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert!(
            app.native_documents
                .pending_window_closes
                .contains_key(&wid)
        );
        let controls = app.windows[&wid]
            .palette()
            .expect("blocked completion surfaces reducer recovery")
            .controls_lines();
        for action in ["settings/drafts/review", "settings/drafts/discard-all"] {
            assert!(
                controls.iter().any(|line| line.contains(action)),
                "missing {action}: {controls:?}"
            );
        }

        discard_all_settings_drafts(&mut app, wid, settings_view);
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![wid])
        );
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert!(
            !app.native_documents
                .pending_window_closes
                .contains_key(&wid)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn async_quit_revalidates_settings_drafts_before_exit_verdict() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-quit-native-revalidate-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quit.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (editor, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(editor).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("durable ".into())),
        )
        .unwrap();
        assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings_view) = app.active_native_view(wid).unwrap();
        enter_settings_draft(&mut app, wid, settings_view);
        app.save_document_checkpoint(document, editor_view).unwrap();

        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![])
        );
        assert!(app.native_documents.pending_quit.is_some());
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert!(app.windows[&wid].palette().is_some());

        discard_all_settings_drafts(&mut app, wid, settings_view);
        assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
        assert!(app.native_documents.pending_quit.is_none());
        // Whole-app quit exits with live document edges; process teardown owns
        // them only after this fresh all-native Ready verdict.
        assert_eq!(app.document_store.view_count(document), Some(1));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn real_save_latch_and_close_completion_refine_the_derived_model() {
        #[derive(Clone, Copy)]
        struct Controller {
            baseline: u64,
            target: i64,
            requested: i64,
            close_seq: i64,
            settled: bool,
        }

        fn relative(seq: aterm_buffer::Seq, baseline: u64) -> i64 {
            i64::try_from(seq.0.saturating_sub(baseline)).unwrap()
        }

        fn project(
            model: &aterm_spec::derive::Model,
            app: &App,
            document: DocumentId,
            controller: Controller,
        ) -> aterm_spec::interp::State {
            let mut state = model.init_state();
            let snapshot = app.document_store.snapshot(document).unwrap();
            let durable = app.document_store.checkpoint_seq(document).unwrap();
            state.insert("head", relative(snapshot.seq, controller.baseline));
            state.insert("durable", relative(durable, controller.baseline));
            state.insert(
                "inflight",
                i64::from(app.native_documents.inflight.contains(&document)),
            );
            state.insert("target", controller.target);
            state.insert("requested", controller.requested);
            state.insert(
                "latched",
                i64::from(app.native_documents.pending_saves.contains_key(&document)),
            );
            state.insert(
                "close_waiting",
                i64::from(
                    app.native_documents
                        .pending_window_closes
                        .values()
                        .any(|plan| plan.documents.iter().any(|item| item.document == document)),
                ),
            );
            state.insert("close_seq", controller.close_seq);
            state.insert(
                "closed",
                i64::from(app.document_store.view_count(document) == Some(0)),
            );
            state.insert("settled", i64::from(controller.settled));
            state
        }

        fn assert_action(
            model: &aterm_spec::derive::Model,
            before: &aterm_spec::interp::State,
            after: &aterm_spec::interp::State,
            action: &'static str,
        ) {
            assert_eq!(
                model.successors(action, before).as_slice(),
                std::slice::from_ref(after),
                "shipping document transition must refine {action}"
            );
            assert_eq!(
                aterm_spec::interp::admits(model, before, after),
                Some(action)
            );
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, after),
                    "post-state violates {}::{}: {after:?}",
                    model.name,
                    invariant.name
                );
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "aterm-app-save-latch-conformance-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("close.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let baseline = app.document_store.snapshot(document).unwrap().seq.0;
        let model = aterm_spec::derive::native_save_intent_latch_model();
        let mut controller = Controller {
            baseline,
            target: 0,
            requested: 0,
            close_seq: 0,
            settled: true,
        };
        let mut state = project(&model, &app, document, controller);
        assert_eq!(state, model.init_state());

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("first ".into())),
        )
        .unwrap();
        controller.settled = false;
        let after = project(&model, &app, document, controller);
        assert_action(&model, &state, &after, "Edit");
        state = after;

        let first = begin_inflight_save_for_test(&mut app, document);
        controller.target = state["head"];
        controller.requested = state["head"];
        let after = project(&model, &app, document, controller);
        assert_action(&model, &state, &after, "BeginSave");
        state = after;

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("latest ".into())),
        )
        .unwrap();
        let after = project(&model, &app, document, controller);
        assert_action(&model, &state, &after, "Edit");
        state = after;

        assert!(
            !app.prepare_window_document_shutdown_inner(wid, true)
                .unwrap()
        );
        controller.requested = state["head"];
        controller.close_seq = state["head"];
        let after = project(&model, &app, document, controller);
        assert_action(&model, &state, &after, "BeginCloseInflight");
        state = after;

        // Negative control: dropping the newer latch would both claim a false
        // final save and close below the frozen sequence.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let dropped = buggy.successors("CompleteChain", &state)[0].clone();
        assert_eq!(aterm_spec::interp::admits(&model, &state, &dropped), None);
        assert!(!buggy.check_invariant("SettledCoversLatestRequest", &dropped));
        assert!(!buggy.check_invariant("WaitingCloseHasCompletionPump", &dropped));

        finish_inflight_save_for_test(&mut app, document, view, first).unwrap();
        controller.target = controller.requested;
        controller.settled = true;
        let after = project(&model, &app, document, controller);
        assert_action(&model, &state, &after, "CompleteChain");
        state = after;

        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![wid])
        );
        let after = project(&model, &app, document, controller);
        assert_action(&model, &state, &after, "CommitClose");
        assert_eq!(fs::read_to_string(&path).unwrap(), "first latest base\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn quit_during_inflight_save_pumps_the_frozen_quit_sequence() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-quit-save-latch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quit.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("first ".into())),
        )
        .unwrap();
        let first = begin_inflight_save_for_test(&mut app, document);
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("latest ".into())),
        )
        .unwrap();
        let latest = app.document_store.snapshot(document).unwrap();

        assert!(!app.prepare_quit_document_shutdown_inner(true).unwrap());
        assert!(app.native_documents.pending_saves.contains_key(&document));
        finish_inflight_save_for_test(&mut app, document, view, first).unwrap();

        assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
        assert!(!app.native_documents.pending_saves.contains_key(&document));
        assert_eq!(
            app.document_store.checkpoint_seq(document),
            Some(latest.seq)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn file_refresh_reloads_clean_conflicts_dirty_and_explicit_discard_is_atomic() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-refresh-discard-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watched.md");
        fs::write(&path, "disk one\n").unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        fs::write(&path, "disk two\n").unwrap();
        assert!(matches!(
            app.refresh_native_document(document).unwrap(),
            crate::native_document_io::FileWatchReduction::ReloadClean { .. }
        ));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "disk two\n"
        );
        assert_eq!(app.document_store.dirty(document), Some(false));

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("local ".to_string())),
        )
        .unwrap();
        let local = app.document_store.snapshot(document).unwrap();
        fs::write(&path, "disk three\n").unwrap();
        assert!(matches!(
            app.refresh_native_document(document).unwrap(),
            crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
        ));
        // Negative control: a blind watcher reload would lose this exact draft.
        let preserved = app.document_store.snapshot(document).unwrap();
        assert_eq!(preserved.seq, local.seq);
        assert_eq!(preserved.text, local.text);
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert!(app.native_documents.disk_conflicts.contains(&document));
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("conflicted document remains an Editor");
        };
        assert!(editor.disk_conflict);
        let save_enabled = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .expect("Save command")
            .enabled;
        assert!(!save_enabled, "Save UI is disabled during disk conflict");
        let error = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(error.contains("Save blocked"), "{error}");
        assert!(
            error.contains("Discard Changes and Reload from Disk"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk three\n");
        let recovery = app
            .native_documents
            .recovery_status
            .get(&document)
            .expect("conflict recovery guidance");
        assert!(recovery.contains("Save is blocked"), "{recovery}");
        assert!(recovery.contains("Discard Changes and Reload from Disk"));
        assert!(!recovery.to_ascii_lowercase().contains("retry"));

        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/revert"),
                value: None,
            }),
        )
        .unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "disk three\n"
        );
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert!(!app.native_documents.disk_conflicts.contains(&document));
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            unreachable!();
        };
        assert!(!editor.disk_conflict);
        assert!(!app.native_documents.recovery_status.contains_key(&document));
        let selection = editor_buffer(&app, app.active_native_view(wid).unwrap().1)
            .primary_selection()
            .range();
        assert!(selection.is_empty());
        assert!(selection.end <= "disk three\n".len());

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("resolved ".to_string())),
        )
        .unwrap();
        let save_enabled = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .expect("Save command")
            .enabled;
        assert!(
            save_enabled,
            "fresh edits are saveable after explicit reload"
        );
        app.save_document_checkpoint(document, view).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk three\nresolved ");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn shipping_chords_keep_mark_active_through_motion_and_kill_region() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-editor-mark-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mark.txt");
        fs::write(&path, "alpha beta").unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        drive_native(&mut app, key(Key::Named(NamedKey::Space), Modifiers::CTRL));
        for _ in 0..5 {
            drive_native(&mut app, key(Key::Character('f'), Modifiers::CTRL));
        }
        assert!(editor_buffer(&app, view).mark_active);
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &Selection { anchor: 0, head: 5 }
        );
        drive_native(&mut app, key(Key::Character('w'), Modifiers::CTRL));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            " beta"
        );
        assert!(!editor_buffer(&app, view).mark_active);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn shipping_search_and_m_x_own_text_without_document_leakage() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-app-editor-minibuffer-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("modal.txt");
        fs::write(&path, "zero needle tail").unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();

        drive_native(&mut app, key(Key::Character('s'), Modifiers::CTRL));
        drive_native(&mut app, InputEvent::Text("needle".to_string()));
        let searched = app.document_store.snapshot(document).unwrap();
        assert_eq!(searched.seq, original.seq);
        assert_eq!(searched.text, original.text);
        assert_eq!(editor_buffer(&app, view).primary_selection().range(), 5..11);

        // Destructive/navigation gestures are modal too; none may fall through
        // into the document while I-search owns input.
        for event in [
            key(Key::Named(NamedKey::Delete), Modifiers::empty()),
            key(Key::Named(NamedKey::ArrowLeft), Modifiers::empty()),
            key(Key::Named(NamedKey::ArrowDown), Modifiers::empty()),
        ] {
            drive_native(&mut app, event);
        }
        assert_eq!(
            app.document_store.snapshot(document).unwrap().seq,
            original.seq
        );
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Escape), Modifiers::empty()),
        );
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &Selection::caret(0)
        );

        drive_native(&mut app, key(Key::Character('x'), Modifiers::ALT));
        drive_native(&mut app, InputEvent::Text("forward-char".to_string()));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().seq,
            original.seq
        );
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &Selection::caret(1)
        );

        drive_native(&mut app, key(Key::Character('x'), Modifiers::ALT));
        drive_native(&mut app, InputEvent::Text("not-a-command".to_string()));
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor remains live");
        };
        assert!(matches!(
            state.buffer.as_ref().map(|buffer| &buffer.minibuffer),
            Some(Minibuffer::Command { query, .. }) if query == "not-a-command"
        ));
        assert!(
            state
                .status
                .as_deref()
                .is_some_and(|status| status.contains("No editor command"))
        );
        assert_eq!(
            app.document_store.snapshot(document).unwrap().seq,
            original.seq
        );
        drive_native(&mut app, key(Key::Character('g'), Modifiers::CTRL));
        assert!(matches!(
            &editor_buffer(&app, view).minibuffer,
            Minibuffer::Inactive
        ));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn shipping_c_x_b_switches_only_to_an_exact_open_editor_buffer() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-editor-buffer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first_path = dir.join("first.txt");
        let second_path = dir.join("second.txt");
        fs::write(&first_path, "first bytes").unwrap();
        fs::write(&second_path, "second bytes").unwrap();
        let first_uri = file_uri(&first_path);
        let second_uri = file_uri(&second_path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &first_uri).unwrap();
        let first_document = app
            .native_runtime
            .document_id(app.active_native_view(WindowId(0)).unwrap().0)
            .unwrap();
        app.open_document_tab(AppKind::Editor, &second_uri).unwrap();
        let second_document = app
            .native_runtime
            .document_id(app.active_native_view(WindowId(0)).unwrap().0)
            .unwrap();
        let first_before = app.document_store.snapshot(first_document).unwrap();
        let second_before = app.document_store.snapshot(second_document).unwrap();

        // The unmodified second key is intentionally driven through the shipping
        // InputEvent seam: pending C-x must route it to the chord trie, not text.
        drive_native(&mut app, key(Key::Character('x'), Modifiers::CTRL));
        drive_native(&mut app, key(Key::Character('b'), Modifiers::empty()));
        drive_native(&mut app, InputEvent::Text("first.txt".to_string()));
        assert_eq!(
            app.document_store.snapshot(second_document).unwrap().seq,
            second_before.seq
        );
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        let (active, _) = app.active_native_view(WindowId(0)).unwrap();
        assert_eq!(app.native_runtime.document_id(active), Some(first_document));
        assert_eq!(
            app.document_store.snapshot(first_document).unwrap().text,
            first_before.text
        );
        assert_eq!(
            app.document_store.snapshot(second_document).unwrap().text,
            second_before.text
        );

        app.open_document_tab(AppKind::Editor, &second_uri).unwrap();
        drive_native(&mut app, key(Key::Character('x'), Modifiers::CTRL));
        drive_native(&mut app, key(Key::Character('b'), Modifiers::empty()));
        drive_native(&mut app, InputEvent::Text("missing.txt".to_string()));
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        let (active, active_view) = app.active_native_view(WindowId(0)).unwrap();
        assert_eq!(
            app.native_runtime.document_id(active),
            Some(second_document)
        );
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(active_view) else {
            panic!("second editor remains active");
        };
        assert!(
            state
                .status
                .as_deref()
                .is_some_and(|status| status.contains("No open editor buffer"))
        );
        assert_eq!(
            app.document_store.snapshot(second_document).unwrap().seq,
            second_before.seq
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn production_open_recovers_durable_draft_before_publishing_editor_view() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-crash-recovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recover.md");
        let journal_root = dir.join("private-drafts");
        fs::write(&path, "disk\n").unwrap();
        let uri = file_uri(&path);

        {
            let mut crashed = App::headless_for_test();
            crashed.native_documents.journals =
                Some(DocumentJournalStore::for_test(journal_root.clone()).unwrap());
            crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
            crashed
                .dispatch_native_event(
                    WindowId(0),
                    AppEvent::TextInput(TextInputEvent::Commit("unsaved ".into())),
                )
                .unwrap();
            let (instance, _) = crashed.active_native_view(WindowId(0)).unwrap();
            let document = crashed.native_runtime.document_id(instance).unwrap();
            assert_eq!(
                crashed
                    .document_store
                    .snapshot(document)
                    .unwrap()
                    .text
                    .as_ref(),
                "unsaved disk\n"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");
            // Drop without invoking the atomic file-save lane: this is the crash.
        }

        let mut reopened = App::headless_for_test();
        reopened.native_documents.journals =
            Some(DocumentJournalStore::for_test(journal_root.clone()).unwrap());
        reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, _) = reopened.active_native_view(WindowId(0)).unwrap();
        let document = reopened.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            reopened
                .document_store
                .snapshot(document)
                .unwrap()
                .text
                .as_ref(),
            "unsaved disk\n"
        );
        assert_eq!(reopened.document_store.dirty(document), Some(true));
        let Some(NativeApp::Editor(editor)) = reopened.native_runtime.app(instance) else {
            panic!("recovered view must be Editor")
        };
        assert!(
            editor
                .recovery_status
                .as_deref()
                .is_some_and(|status| status.contains("Recovered an unsaved draft"))
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn production_open_surfaces_disk_conflict_and_preserves_draft() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-app-recovery-conflict-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("conflict.md");
        let journal_root = dir.join("private-drafts");
        fs::write(&path, "base\n").unwrap();
        let uri = file_uri(&path);

        {
            let mut crashed = App::headless_for_test();
            crashed.native_documents.journals =
                Some(DocumentJournalStore::for_test(journal_root.clone()).unwrap());
            crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
            crashed
                .dispatch_native_event(
                    WindowId(0),
                    AppEvent::TextInput(TextInputEvent::Commit("draft ".into())),
                )
                .unwrap();
        }
        fs::write(&path, "external\n").unwrap();

        let mut reopened = App::headless_for_test();
        reopened.native_documents.journals =
            Some(DocumentJournalStore::for_test(journal_root.clone()).unwrap());
        reopened.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, _) = reopened.active_native_view(WindowId(0)).unwrap();
        let document = reopened.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            reopened
                .document_store
                .snapshot(document)
                .unwrap()
                .text
                .as_ref(),
            "external\n",
            "conflicting recovery never silently replaces newer disk bytes"
        );
        let Some(NativeApp::Editor(editor)) = reopened.native_runtime.app(instance) else {
            panic!("conflict view must be Editor")
        };
        assert!(
            editor
                .recovery_status
                .as_deref()
                .is_some_and(|status| status.contains("conflicts with newer disk content"))
        );
        assert!(
            fs::read_dir(&journal_root).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".preserved-")),
            "conflicting draft material remains in the private recovery directory"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn final_dirty_document_view_is_saved_before_its_tab_detaches() {
        let dir = std::env::temp_dir().join(format!("aterm-app-close-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("close.md");
        fs::write(&path, "before\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let wid = WindowId(0);
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("safe ".to_string())),
        )
        .unwrap();
        let before_tabs = app.windows[&wid].tab_set.len();

        // A real event-loop save may report pending. The headless lane executes the
        // identical durability proof inline, then commits the all-document detach batch.
        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.windows[&wid].tab_set.len(), before_tabs - 1);
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn final_dirty_document_leaf_is_saved_before_only_that_split_leaf_detaches() {
        let dir = std::env::temp_dir().join(format!("aterm-app-leaf-close-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("leaf-close.md");
        fs::write(&path, "before\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let wid = WindowId(0);
        let (editor_instance, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(editor_instance).unwrap();

        let settings_instance = app
            .native_runtime
            .insert_instance(NativeApp::Settings(
                crate::native_settings::SettingsApp::new(
                    crate::update_screen::UpdateState::from_status(1, "test", None, false),
                ),
            ))
            .unwrap();
        let settings_view = app
            .split_active_with_native(
                wid,
                crate::tab_model::SplitAxis::Horizontal,
                settings_instance,
                AppViewState::Settings(Box::new(crate::native_settings::SettingsViewState::new(
                    &app.config,
                ))),
            )
            .unwrap();
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(editor_view);
        app.sync_window(wid);
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("safe ".to_string())),
        )
        .unwrap();

        app.close_active_tab();

        let active = app.windows[&wid].tab_set.active().unwrap();
        assert_eq!(active.root.leaves(), vec![settings_view]);
        assert_eq!(active.focus, settings_view);
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert_eq!(fs::read_to_string(&path).unwrap(), "safe before\n");
        assert!(app.structural_invariants_ok());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn nonfinal_document_close_never_forces_a_checkpoint() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-share-close-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shared-close.md");
        fs::write(&path, "before\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let wid = WindowId(0);
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("dirty ".to_string())),
        )
        .unwrap();

        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");

        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert_eq!(fs::read_to_string(&path).unwrap(), "dirty before\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn window_close_waits_for_every_document_before_atomic_detach() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-window-batch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first_path = dir.join("first.md");
        let second_path = dir.join("second.md");
        fs::write(&first_path, "first\n").unwrap();
        fs::write(&second_path, "second\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        app.open_document_tab(AppKind::Editor, &file_uri(&first_path))
            .unwrap();
        let (first_instance, first_view) = app.active_native_view(wid).unwrap();
        let first = app.native_runtime.document_id(first_instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("safe ".into())),
        )
        .unwrap();

        app.open_document_tab(AppKind::Editor, &file_uri(&second_path))
            .unwrap();
        let (second_instance, second_view) = app.active_native_view(wid).unwrap();
        let second = app.native_runtime.document_id(second_instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("also ".into())),
        )
        .unwrap();
        let tabs_before = app.windows[&wid].tab_set.len();

        assert!(
            !app.prepare_window_document_shutdown_inner(wid, false)
                .unwrap()
        );
        assert_eq!(app.document_store.view_count(first), Some(1));
        assert_eq!(app.document_store.view_count(second), Some(1));

        // One genuine atomic-file Durable reduction is insufficient: the batch
        // coordinator must leave *both* leaves and the window intact.
        app.save_document_checkpoint(first, first_view).unwrap();
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![])
        );
        assert!(app.windows.contains_key(&wid));
        assert_eq!(app.windows[&wid].tab_set.len(), tabs_before);
        assert_eq!(app.document_store.view_count(first), Some(1));
        assert_eq!(app.document_store.view_count(second), Some(1));

        app.save_document_checkpoint(second, second_view).unwrap();
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![wid])
        );
        assert!(app.windows.contains_key(&wid));
        assert_eq!(app.document_store.view_count(first), Some(0));
        assert_eq!(app.document_store.view_count(second), Some(0));
        assert_eq!(fs::read_to_string(&first_path).unwrap(), "safe first\n");
        assert_eq!(fs::read_to_string(&second_path).unwrap(), "also second\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn whole_app_quit_waits_for_all_durable_documents_without_partial_teardown() {
        let dir = std::env::temp_dir().join(format!("aterm-app-quit-batch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let first_path = dir.join("quit-first.md");
        let second_path = dir.join("quit-second.md");
        fs::write(&first_path, "first\n").unwrap();
        fs::write(&second_path, "second\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        app.open_document_tab(AppKind::Editor, &file_uri(&first_path))
            .unwrap();
        let (first_instance, first_view) = app.active_native_view(wid).unwrap();
        let first = app.native_runtime.document_id(first_instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("safe ".into())),
        )
        .unwrap();
        app.open_document_tab(AppKind::Editor, &file_uri(&second_path))
            .unwrap();
        let (second_instance, second_view) = app.active_native_view(wid).unwrap();
        let second = app.native_runtime.document_id(second_instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("also ".into())),
        )
        .unwrap();

        assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
        app.save_document_checkpoint(first, first_view).unwrap();
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![])
        );
        assert!(app.windows.contains_key(&wid));
        assert_eq!(app.document_store.view_count(first), Some(1));
        assert_eq!(app.document_store.view_count(second), Some(1));

        app.save_document_checkpoint(second, second_view).unwrap();
        assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
        assert!(app.windows.contains_key(&wid));
        assert_eq!(app.document_store.dirty(first), Some(false));
        assert_eq!(app.document_store.dirty(second), Some(false));
        // Quit exits with live windows; teardown belongs to process exit, not an
        // incremental close that could expose a half-destroyed UI.
        assert_eq!(app.document_store.view_count(first), Some(1));
        assert_eq!(app.document_store.view_count(second), Some(1));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_window_save_keeps_the_complete_plan_and_surfaces_status() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-app-window-save-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("conflict.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("ours ".into())),
        )
        .unwrap();
        assert!(
            !app.prepare_window_document_shutdown_inner(wid, false)
                .unwrap()
        );

        // Negative control: an external generation change makes the genuine save
        // reducer return Conflict rather than a Durable proof.
        fs::write(&path, "theirs\n").unwrap();
        let error = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(error.contains("changed on disk"), "{error}");
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![])
        );
        assert!(app.windows.contains_key(&wid));
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert!(matches!(
            app.document_store.phase(document),
            Some(crate::document_store::DocumentPhase::Blocked { .. })
        ));
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view must remain installed");
        };
        assert!(
            state
                .status
                .as_deref()
                .is_some_and(|status| status.contains("changed on disk"))
        );
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/revert"),
                value: None,
            }),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![wid]),
            "discard/reload is a complete recovery for a conflict-blocked window close"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_quit_save_is_disarmed_and_cannot_late_exit() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-app-quit-save-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quit-conflict.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("ours ".into())),
        )
        .unwrap();
        assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());

        fs::write(&path, "theirs\n").unwrap();
        assert!(
            app.save_document_checkpoint(document, view)
                .unwrap_err()
                .contains("changed on disk")
        );
        assert!(app.quit_document_shutdown_blocked());
        assert!(app.cancel_failed_quit_document_shutdown());
        assert!(!app.cancel_failed_quit_document_shutdown());
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![])
        );
        assert!(app.windows.contains_key(&wid));
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert_eq!(
            app.document_store.phase(document),
            Some(crate::document_store::DocumentPhase::Active)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dropped_file_opens_and_reuses_the_active_document_app_kind() {
        let dir =
            std::env::temp_dir().join(format!("aterm-app-document-drop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.md");
        let dropped = dir.join("dropped file.md");
        fs::write(&source, "source\n").unwrap();
        fs::write(&dropped, "dropped\n").unwrap();

        for kind in [AppKind::Markdown, AppKind::Editor] {
            let mut app = App::headless_for_test();
            app.open_document_tab(kind, &file_uri(&source)).unwrap();
            let wid = WindowId(0);

            app.drop_file(wid, &dropped);
            let (instance, _) = app.active_native_view(wid).expect("dropped document view");
            assert_eq!(
                app.native_runtime.app(instance).map(NativeApp::kind),
                Some(kind)
            );
            let document = app
                .native_runtime
                .document_id(instance)
                .expect("dropped document identity");
            assert_eq!(
                app.document_store.snapshot(document).unwrap().text.as_ref(),
                "dropped\n"
            );

            let tabs_after_first_drop = app.windows[&wid].tab_set.len();
            app.drop_file(wid, &dropped);
            let (reused_instance, _) = app.active_native_view(wid).expect("reused document view");
            assert_eq!(reused_instance, instance);
            assert_eq!(app.windows[&wid].tab_set.len(), tabs_after_first_drop);
        }
        let _ = fs::remove_dir_all(dir);
    }
}
