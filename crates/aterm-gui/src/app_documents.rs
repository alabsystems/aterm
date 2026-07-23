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
    AppEvent, AppKind, AppViewState, EditorApp, EditorViewState, EventResult, MarkdownApp,
    MarkdownViewState, NativeApp, TextInputEvent,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingDocumentClose {
    window: WindowId,
    tab: crate::tab_model::TabId,
    views: Vec<DocumentViewId>,
    whole_tab: bool,
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
                                proxy,
                            } => {
                                let result = crate::native_document_host::execute_granted_save(
                                    &grant, &plan,
                                );
                                let _ = proxy.send_event(crate::Wake::NativeDocumentSaved {
                                    document,
                                    source_view,
                                    generation: plan.generation,
                                    saved_bytes: plan.bytes,
                                    result,
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

pub(crate) struct DocumentHostRuntime {
    pub(crate) grants: DocumentGrantStore,
    pub(crate) persistence: DocumentPersistenceStore,
    journals: Option<DocumentJournalStore>,
    journal_unavailable: Option<String>,
    recovery_status: BTreeMap<DocumentId, String>,
    inflight: BTreeSet<DocumentId>,
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
            inflight: BTreeSet::new(),
            pending_closes: BTreeMap::new(),
            pending_window_closes: BTreeMap::new(),
            pending_quit: None,
        }
    }
}

impl App {
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
                | AppEvent::EditorViewportChanged { .. }
                | AppEvent::ScrollLines(_)
        ) {
            return Ok(None);
        }

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

        self.publish_editor_commit(document, view);
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
            .publish_document(document, snapshot.text.as_ref(), dirty);
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
            crate::native_document_io::FileWatchReduction::Unchanged => {}
            crate::native_document_io::FileWatchReduction::ReloadClean { .. } => {
                self.install_stable_file_observation(
                    document,
                    observed_text,
                    observed,
                    "Reloaded changes from disk",
                )?;
            }
            crate::native_document_io::FileWatchReduction::ConflictDirty { .. } => {
                self.set_document_recovery_status(
                    document,
                    Some(
                        "File changed on disk; unsaved editor bytes were preserved. Save, discard, or retry after reviewing the conflict"
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
        self.install_stable_file_observation(
            document,
            &observed.text,
            observed.observed,
            "Discarded changes and reloaded from disk",
        )?;
        self.set_editor_view_status(source_view, "Discarded changes and reloaded from disk");
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
        self.native_runtime
            .publish_document(document, current.text.as_ref(), false);
        let revision = self.document_store.revision(document).unwrap_or(0);
        for (window, instance, view) in views {
            let _ = self.native_runtime.dispatch(
                instance,
                view,
                AppEvent::DocumentChanged { document, revision },
            );
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
        self.set_document_recovery_status(document, None);
        self.drive_document_journal(document)?;
        Ok(())
    }

    fn save_document_checkpoint(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        if self.native_documents.inflight.contains(&document) {
            return Ok(());
        }
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "document disappeared before save".to_string())?;
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
            if queue
                .send(NativeDocumentJob::Save {
                    document,
                    source_view,
                    grant,
                    plan: pending.plan.clone(),
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
        let result = self
            .native_documents
            .grants
            .execute_save(pending.grant, &pending.plan);
        self.finish_native_document_save(
            document,
            source_view,
            pending.plan.generation,
            pending.plan.bytes,
            result,
        )
    }

    pub(crate) fn finish_native_document_save(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        generation: crate::native_document_io::SaveGeneration,
        saved_bytes: std::sync::Arc<[u8]>,
        result: crate::native_document_io::AtomicSaveResult,
    ) -> Result<(), String> {
        self.native_documents.inflight.remove(&document);
        let reduction = self
            .native_documents
            .persistence
            .complete(document, generation, result)
            .map_err(|error| format!("could not finish document save: {error:?}"))?;
        let outcome = match reduction {
            crate::native_document_io::SaveReduction::Durable(checkpoint) => {
                self.document_store
                    .checkpoint_ack(document, checkpoint.seq)
                    .map_err(|error| format!("could not publish durable checkpoint: {error:?}"))?;
                let saved_text = std::str::from_utf8(&saved_bytes)
                    .map_err(|_| "saved document bytes were not UTF-8".to_string())?;
                if let Some(journals) = self.native_documents.journals.as_mut() {
                    journals.request_checkpoint(checkpoint, std::sync::Arc::from(saved_text))?;
                }
                if let Some(AppViewState::Editor(state)) =
                    self.native_runtime.view_state_mut(source_view)
                {
                    state.status = Some("Saved".to_string());
                }
                self.finish_pending_document_closes(document)?;
                Ok(())
            }
            crate::native_document_io::SaveReduction::Conflict(conflict) => {
                let _ = self.document_store.checkpoint_fail(document);
                Err(format!("document changed on disk: {conflict:?}"))
            }
            crate::native_document_io::SaveReduction::Failed { stage, message } => {
                let _ = self.document_store.checkpoint_fail(document);
                Err(format!("document save failed at {stage:?}: {message}"))
            }
            crate::native_document_io::SaveReduction::Cancelled => {
                let _ = self.document_store.checkpoint_fail(document);
                Err("document save was cancelled".to_string())
            }
            crate::native_document_io::SaveReduction::Stale => {
                Err("stale document save completion".to_string())
            }
        };
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
        self.native_runtime
            .set_document_recovery_status(document, None);
        for (window, _, view) in self.document_native_views(document) {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
                state.status = Some("Saved".to_string());
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

    fn document_native_views(
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
        if !matches!(kind, AppKind::Markdown | AppKind::Editor) {
            return Err("document tabs must be Markdown or Editor".to_string());
        }
        if !self.windows.contains_key(&wid) {
            return Err("requesting window disappeared".to_string());
        }
        let access = if kind == AppKind::Editor {
            GrantAccess::ReadWrite
        } else {
            GrantAccess::ReadOnly
        };
        let granted = self
            .native_documents
            .grants
            .open_local(uri, access, DEFAULT_DOCUMENT_LIMIT)
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
            AppKind::Editor => AppViewState::Editor(EditorViewState::default()),
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
            Some(AppViewState::Editor(EditorViewState {
                buffer: Some(_),
                ..
            }))
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
        let (instance, _) = app.active_native_view(wid).unwrap();
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
        let selection = editor_buffer(&app, app.active_native_view(wid).unwrap().1)
            .primary_selection()
            .range();
        assert!(selection.is_empty());
        assert!(selection.end <= "disk three\n".len());
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
