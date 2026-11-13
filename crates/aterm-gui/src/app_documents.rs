// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Markdown/editor host integration.

use std::collections::{BTreeMap, BTreeSet};

use crate::app_edit::Placement;
use crate::document_store::{DocumentId, DocumentViewId};
use crate::native_app::{
    AppEvent, AppKind, AppViewState, EditorApp, EventResult, MarkdownWorkspaceMode,
    MarkdownWorkspaceView, NativeApp, TextInputEvent,
};
use crate::native_document_host::{
    AdmittedDocument, DEFAULT_DOCUMENT_LIMIT, DocumentGrant, DocumentGrantStore, DocumentHostError,
    DocumentPersistenceStore, GrantAccess,
};
use crate::native_document_journal::{
    DocumentJournalStore, DraftCarryRefusal, JournalCompletion, JournalEffect, JournalLockPatience,
    JournalReseatPlan, JournalReseatResult, JournalRewriteGeneration, JournalRewritePlan,
    JournalRewriteResult, execute_journal_append, execute_journal_reseat, execute_journal_rewrite,
    verify_carried_draft,
};
use crate::native_editor::{CloseWhy, EditorCommand, EditorEffect, Selection};
use crate::{App, WindowId};
use amacs::input::{EditorInput, Follow, Reduction};

#[path = "app_document_watch.rs"]
mod watch;

#[cfg(test)]
#[path = "owner_keys_save_tests.rs"]
mod owner_keys_save_tests;

#[cfg(all(test, unix))]
#[path = "app_documents_private_tests.rs"]
mod private_tests;

#[cfg(all(test, unix))]
#[path = "app_documents_conceal_tests.rs"]
mod conceal_tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigEditorRevealDecision {
    Select {
        requested: String,
        range: std::ops::Range<usize>,
    },
    /// The key is not in the file.
    AddAbsent { requested: String },
}

/// Resolve only an in-document Settings → Manual target.
pub(crate) fn config_editor_reveal_decision(
    source: &str,
    target: &crate::native_app::ConfigEditorTarget,
) -> Result<ConfigEditorRevealDecision, String> {
    const MAX_TARGET_BYTES: usize = 512;

    let crate::native_app::ConfigEditorTarget::Key(key) = target;
    let requested = key.trim();
    if requested.is_empty() || requested.len() > MAX_TARGET_BYTES {
        return Err("Manual reveal target is empty or too long".to_string());
    }
    let direct = crate::native_config_language::config_key_source_range(source, key);
    Ok(direct.map_or_else(
        || ConfigEditorRevealDecision::AddAbsent {
            requested: requested.to_string(),
        },
        |range| ConfigEditorRevealDecision::Select {
            requested: requested.to_string(),
            range,
        },
    ))
}

/// How an absent key was added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbsentKeyAdded {
    /// At the value that resolves exactly as its absence does, so the file means what it meant.
    AtDefault,
    /// With no value yet, the caret where it goes.
    NeedsValue { record: Option<&'static str> },
    /// A table or map key (`keybindings`, `harness`) as its empty `[key]` table, the caret on the line under it where its entries go.
    Table,
}

fn absent_key_edit(
    source: &str,
    key: &str,
) -> Option<(
    crate::native_config_language::ConfigCompletionEdit,
    AbsentKeyAdded,
)> {
    if let Some(root) = crate::native_config_language::config_key_record_root(key) {
        return absent_record_member_edit(source, root, key);
    }
    let at_default = absent_key_default_item(source, key)
        .and_then(|item| crate::prefs::with_toml_item(source, key, item).ok())
        .filter(|added| adding_changes_nothing(source, added));
    if let Some(added) = at_default {
        let (edit, _) = smallest_edit(source, &added, key, None)?;
        return Some((edit, AbsentKeyAdded::AtDefault));
    }
    if let Some(table) = absent_table_edit(source, key) {
        return Some(table);
    }
    crate::native_config_language::config_key_example_literal(key)?;
    let placeholder = aterm_toml::edit::Item::Value(aterm_toml::edit::Value::from(""));
    let mut added = crate::prefs::with_toml_item(source, key, placeholder).ok()?;
    let value = crate::native_config_language::config_key_source_range(&added, key)?;
    added.replace_range(value.clone(), "");
    let (edit, _) = smallest_edit(source, &added, key, Some(value.start))?;
    Some((edit, AbsentKeyAdded::NeedsValue { record: None }))
}

fn absent_key_default_item(source: &str, key: &str) -> Option<aterm_toml::edit::Item> {
    let config = crate::app_config::Config::parse(source).unwrap_or_default();
    let fields = crate::prefs::editable_fields(&config);
    if let Some(field) = fields.iter().find(|field| field.key == key) {
        let literal = crate::prefs::unset_literal(field)?;
        return crate::prefs::typed_toml_item(key, &literal).ok();
    }
    let literal = crate::native_config_language::config_key_default_literal(key)?;
    let value = literal.parse::<aterm_toml::edit::Value>().ok()?;
    Some(aterm_toml::edit::Item::Value(value))
}

fn adding_changes_nothing(source: &str, added: &str) -> bool {
    if crate::app_config::Config::parse(added).is_err() {
        return false;
    }
    let findings = |text: &str| {
        let mut findings = crate::native_config_language::analyze(text)
            .diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.severity, diagnostic.message))
            .collect::<Vec<_>>();
        findings.sort_by(|a, b| a.1.cmp(&b.1));
        findings
    };
    let before = findings(source);
    let mut after = findings(added);
    after.retain(|finding| !before.contains(finding));
    after.is_empty()
}

fn absent_table_edit(
    source: &str,
    key: &str,
) -> Option<(
    crate::native_config_language::ConfigCompletionEdit,
    AbsentKeyAdded,
)> {
    use crate::native_config_language::ConfigSchemaKind;
    let entry = crate::native_config_language::config_schema_entry(key)?;
    if !matches!(
        entry.kind,
        ConfigSchemaKind::Table | ConfigSchemaKind::DynamicStringMap
    ) {
        return None;
    }
    let mut added = source.to_string();
    if !added.is_empty() && !added.ends_with('\n') {
        added.push('\n');
    }
    if !added.is_empty() {
        added.push('\n');
    }
    added.push_str(&format!("[{key}]\n"));
    let caret = added.len();
    if !adding_changes_nothing(source, &added) {
        return None;
    }
    let (edit, _) = smallest_edit(source, &added, key, Some(caret))?;
    Some((edit, AbsentKeyAdded::Table))
}

fn absent_record_member_edit(
    source: &str,
    root: &'static str,
    key: &str,
) -> Option<(
    crate::native_config_language::ConfigCompletionEdit,
    AbsentKeyAdded,
)> {
    let document = source.parse::<aterm_toml::edit::DocumentMut>().ok()?;
    let mut node = document.as_item();
    let mut present = true;
    for part in root.split('.') {
        match node.as_table_like().and_then(|table| table.get(part)) {
            Some(child) => node = child,
            None => {
                present = false;
                break;
            }
        }
    }
    if present {
        return None;
    }
    let local = key.strip_prefix(root)?.strip_prefix('.')?;
    let mut added = source.to_string();
    if !added.is_empty() && !added.ends_with('\n') {
        added.push('\n');
    }
    if !added.is_empty() {
        added.push('\n');
    }
    added.push_str(&format!("[[{root}]]\n{local} = "));
    let caret = added.len();
    added.push('\n');
    let (edit, _) = smallest_edit(source, &added, key, Some(caret))?;
    Some((edit, AbsentKeyAdded::NeedsValue { record: Some(root) }))
}

fn smallest_edit(
    source: &str,
    added: &str,
    key: &str,
    caret: Option<usize>,
) -> Option<(crate::native_config_language::ConfigCompletionEdit, ())> {
    let prefix = source
        .bytes()
        .zip(added.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let mut prefix = prefix;
    while !source.is_char_boundary(prefix) || !added.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let room = (source.len() - prefix).min(added.len() - prefix);
    let mut suffix = source
        .bytes()
        .rev()
        .zip(added.bytes().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while !source.is_char_boundary(source.len() - suffix)
        || !added.is_char_boundary(added.len() - suffix)
    {
        suffix -= 1;
    }
    let replacement = prefix..source.len() - suffix;
    let insertion = added[prefix..added.len() - suffix].to_string();
    if insertion.is_empty() {
        return None;
    }
    let caret = caret
        .and_then(|caret| caret.checked_sub(prefix))
        .filter(|caret| *caret <= insertion.len() && insertion.is_char_boundary(*caret))
        .unwrap_or(insertion.len());
    Some((
        crate::native_config_language::ConfigCompletionEdit {
            expected: source[replacement.clone()].to_string(),
            replacement,
            insertion,
            post_insert_selection: caret..caret,
            display: key.to_string(),
            help: String::new(),
        },
        (),
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One queued tab or leaf close. Its window is wherever its tab is now
/// ([`App::tab_window`]): a tab keeps its process-unique id when it moves.
struct PendingDocumentClose {
    tab: crate::tab_model::TabId,
    views: Vec<DocumentViewId>,
    whole_tab: bool,
}

/// Latest explicit durability intent received while an older generation is in
/// flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingDocumentSaveIntent {
    seq: aterm_buffer::Seq,
    source_view: crate::tab_model::ViewId,
    /// Whether control clients, accessibility clients or other processes' input alone asked for
    /// it, so its replay is a control client's ([`crate::owner_keys_save`]); a person's request
    /// merged in makes it the person's.
    controller: bool,
}

/// One document's leaves inside a larger window/quit close plan. The plan is
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

impl DocumentClosePlan {
    /// Whether every leaf this plan closes is still one of `live`'s.
    fn within(&self, live: &Self) -> bool {
        let live = live
            .documents
            .iter()
            .flat_map(|item| item.views.iter().map(move |view| (item.document, *view)))
            .collect::<BTreeSet<_>>();
        self.documents.iter().all(|item| {
            item.views
                .iter()
                .all(|view| live.contains(&(item.document, *view)))
        })
    }
}

/// One OS window's armed close ([`App::prepare_window_document_shutdown`]): the
/// documents it waits on, and the window it was asked over.
#[derive(Clone, Debug)]
struct PendingWindowClose {
    plan: DocumentClosePlan,
    asked: crate::app_window::WindowCloseConsent,
    /// Ready, consented and handed to a pending update handoff, which replays
    /// the close once it stands down ([`App::replayed_window_close_consented`]).
    deferred: bool,
}

enum NativeDocumentJob {
    Save {
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        grant: DocumentGrant,
        plan: crate::native_document_io::SavePlan,
        config_themes: Option<std::sync::Arc<crate::app_config::ThemeCatalog>>,
        proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    },
    JournalAppend {
        document: DocumentId,
        path: std::path::PathBuf,
        key: crate::native_document_io::JournalDocumentKey,
        plan: crate::native_document_journal::JournalAppendPlan,
        proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    },
    JournalRewrite {
        document: DocumentId,
        path: std::path::PathBuf,
        plan: JournalRewritePlan,
        proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    },
    JournalReseat {
        document: DocumentId,
        path: std::path::PathBuf,
        plan: JournalReseatPlan,
        proxy: aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    },
}

impl NativeDocumentJob {
    const fn proxy(&self) -> &aterm_window::event_loop::EventLoopProxy<crate::Wake> {
        match self {
            Self::Save { proxy, .. }
            | Self::JournalAppend { proxy, .. }
            | Self::JournalRewrite { proxy, .. }
            | Self::JournalReseat { proxy, .. } => proxy,
        }
    }
}

const NATIVE_DOCUMENT_QUEUE_CAPACITY: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentQueueAdmissionError {
    Saturated,
    WorkerStopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalDriveOutcome {
    Idle,
    Enqueued,
    Deferred,
}

#[derive(Default)]
struct JournalRetrySet {
    documents: BTreeSet<DocumentId>,
}

impl JournalRetrySet {
    fn retain(&mut self, document: DocumentId) -> bool {
        self.documents.insert(document)
    }

    fn remove(&mut self, document: DocumentId) -> bool {
        self.documents.remove(&document)
    }

    fn pop_first(&mut self) -> Option<DocumentId> {
        self.documents.pop_first()
    }

    fn contains(&self, document: DocumentId) -> bool {
        self.documents.contains(&document)
    }

    fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

/// An append the headless test seam held back from inline execution.
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) struct HeldJournalAppend {
    pub(crate) document: DocumentId,
    path: std::path::PathBuf,
    key: crate::native_document_io::JournalDocumentKey,
    pub(crate) plan: crate::native_document_journal::JournalAppendPlan,
}

/// A re-seat the headless test seam held back from inline execution.
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) struct HeldJournalReseat {
    pub(crate) document: DocumentId,
    path: std::path::PathBuf,
    plan: JournalReseatPlan,
}

const SAVE_UNCONFIRMED: &str = "Save could not be confirmed; choose Save again";

/// The status a successor gives a document whose unsaved draft it restored during an update.
pub(crate) const CARRIED_DRAFT_NOTICE: &str =
    "Unsaved draft carried through the update — the file on disk is unchanged";

/// Whether one document's unsaved draft rides an update to the successor ([`App::draft_carry`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftCarry {
    /// Durable in its journal and verified on disk.
    Carried { bytes: usize },
    /// Its journal is writing the latest edit (or re-establishing itself).
    Writing,
    /// It cannot ride this update; the words say why.
    Refused(String),
}

#[derive(Clone, Copy)]
enum JournalAdmissionEffect {
    Append(crate::native_document_io::JournalGeneration),
    Rewrite(JournalRewriteGeneration),
    Reseat(JournalRewriteGeneration),
}

fn try_enqueue_document_journal_job<T>(
    sender: &std::sync::mpsc::SyncSender<T>,
    job: T,
    document: DocumentId,
    effect: JournalAdmissionEffect,
    journals: &mut DocumentJournalStore,
    retries: &mut JournalRetrySet,
) -> Result<JournalDriveOutcome, String> {
    match try_enqueue_document_job(sender, job) {
        Ok(()) => {
            retries.remove(document);
            Ok(JournalDriveOutcome::Enqueued)
        }
        Err(error) => {
            let (deferred, name) = match effect {
                JournalAdmissionEffect::Append(generation) => {
                    (journals.defer_append(document, generation), "append")
                }
                JournalAdmissionEffect::Rewrite(generation) => {
                    (journals.defer_rewrite(document, generation), "rewrite")
                }
                JournalAdmissionEffect::Reseat(generation) => {
                    (journals.defer_reseat(document, generation), "re-seat")
                }
            };
            if !deferred {
                return Err(format!(
                    "document journal {name} changed during queue admission"
                ));
            }
            if error == DocumentQueueAdmissionError::Saturated {
                retries.retain(document);
                Ok(JournalDriveOutcome::Deferred)
            } else {
                retries.remove(document);
                Err(format!("document journal {error}"))
            }
        }
    }
}

impl std::fmt::Display for DocumentQueueAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saturated => formatter.write_str(
                "document worker is saturated; retry after the current disk operations finish",
            ),
            Self::WorkerStopped => formatter.write_str("document worker stopped"),
        }
    }
}

fn try_enqueue_document_job<T>(
    sender: &std::sync::mpsc::SyncSender<T>,
    job: T,
) -> Result<(), DocumentQueueAdmissionError> {
    match sender.try_send(job) {
        Ok(()) => Ok(()),
        Err(std::sync::mpsc::TrySendError::Full(_)) => Err(DocumentQueueAdmissionError::Saturated),
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            Err(DocumentQueueAdmissionError::WorkerStopped)
        }
    }
}

const fn native_document_queue_slot_available_wake() -> crate::Wake {
    crate::Wake::NativeDocumentQueueSlotAvailable
}

fn native_document_queue() -> Result<&'static std::sync::mpsc::SyncSender<NativeDocumentJob>, String>
{
    static QUEUE: std::sync::OnceLock<
        Result<std::sync::mpsc::SyncSender<NativeDocumentJob>, String>,
    > = std::sync::OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) =
                std::sync::mpsc::sync_channel::<NativeDocumentJob>(NATIVE_DOCUMENT_QUEUE_CAPACITY);
            std::thread::Builder::new()
                .name("aterm-native-document".to_string())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        let _ = job
                            .proxy()
                            .send_event(native_document_queue_slot_available_wake());
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
                                let result = execute_journal_append(
                                    &path,
                                    key,
                                    &plan,
                                    JournalLockPatience::Worker,
                                );
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
                                let result = execute_journal_rewrite(
                                    &path,
                                    &plan,
                                    JournalLockPatience::Worker,
                                );
                                let _ =
                                    proxy.send_event(crate::Wake::NativeDocumentJournalRewritten {
                                        document,
                                        generation: plan.generation,
                                        result,
                                    });
                            }
                            NativeDocumentJob::JournalReseat {
                                document,
                                path,
                                plan,
                                proxy,
                            } => {
                                let result = execute_journal_reseat(
                                    &path,
                                    &plan,
                                    JournalLockPatience::Worker,
                                );
                                let _ =
                                    proxy.send_event(crate::Wake::NativeDocumentJournalReseated {
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
) -> Option<
    Result<
        crate::native_config_service::PreparedConfigObservation,
        crate::native_config_service::ConfigFailure,
    >,
> {
    let themes = themes?;
    let crate::native_document_io::AtomicSaveResult::Committed(proof) = result else {
        return None;
    };
    let text = std::str::from_utf8(&plan.bytes)
        .map(str::to_owned)
        .map_err(|_| {
            crate::native_config_service::ConfigFailure::from("saved config bytes were not UTF-8")
        });
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

struct JournalSlot {
    settled: std::cell::OnceCell<Result<DocumentJournalStore, String>>,
    opening: std::cell::Cell<Option<std::thread::JoinHandle<Result<DocumentJournalStore, String>>>>,
    /// What the first look runs inline when no worker is opening the store
    /// ([`Self::deferred`]).
    deferred: std::cell::Cell<Option<JournalOpener>>,
}

type JournalOpener = fn() -> Result<DocumentJournalStore, String>;

impl JournalSlot {
    fn open(store: Result<DocumentJournalStore, String>) -> Self {
        Self {
            settled: std::cell::OnceCell::from(store),
            opening: std::cell::Cell::new(None),
            deferred: std::cell::Cell::new(None),
        }
    }

    fn deferred(open: JournalOpener) -> Self {
        Self {
            settled: std::cell::OnceCell::new(),
            opening: std::cell::Cell::new(None),
            deferred: std::cell::Cell::new(Some(open)),
        }
    }

    fn opening<F>(open: F) -> Self
    where
        F: FnOnce() -> Result<DocumentJournalStore, String> + Send + 'static,
    {
        let (open_tx, open_rx) = std::sync::mpsc::sync_channel::<F>(1);
        let spawned = crate::qos::spawn_lane_thread(
            "aterm-journal-open",
            crate::qos::Role::Responsive,
            move || match open_rx.recv() {
                Ok(open) => open(),
                Err(_) => Err("document recovery is unavailable".to_string()),
            },
        );
        match spawned {
            Ok(worker) => {
                let _ = open_tx.send(open);
                Self {
                    settled: std::cell::OnceCell::new(),
                    opening: std::cell::Cell::new(Some(worker)),
                    deferred: std::cell::Cell::new(None),
                }
            }
            Err(_) => Self::open(open()),
        }
    }

    fn settle(&self) -> &Result<DocumentJournalStore, String> {
        self.settled.get_or_init(|| match self.opening.take() {
            Some(worker) => worker.join().unwrap_or_else(|_| {
                Err("document recovery is unavailable: opening its journal panicked".to_string())
            }),
            None => self.deferred.take().map_or_else(
                || Err("document recovery is unavailable".to_string()),
                |open| open(),
            ),
        })
    }

    fn as_ref(&self) -> Option<&DocumentJournalStore> {
        self.settle().as_ref().ok()
    }

    fn as_mut(&mut self) -> Option<&mut DocumentJournalStore> {
        self.available_mut().ok()
    }

    fn available_mut(&mut self) -> Result<&mut DocumentJournalStore, String> {
        let _ = self.settle();
        match self.settled.get_mut() {
            Some(Ok(store)) => Ok(store),
            Some(Err(reason)) => Err(reason.clone()),
            None => Err("document recovery is unavailable".to_string()),
        }
    }

    fn unavailable(&self) -> Option<&str> {
        self.settle().as_ref().err().map(String::as_str)
    }

    /// A slot settled to a test store at `root`.
    #[cfg(test)]
    fn for_test(root: std::path::PathBuf) -> Self {
        Self::open(Ok(
            DocumentJournalStore::for_test(root).expect("test journal root")
        ))
    }
}

/// Why a document's disk generation no longer matches its saver baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiskConflictCause {
    /// The file changed on disk under a dirty buffer, or a save found it changed.
    Version,
    /// A save could not be confirmed: the disk may or may not hold it.
    SaveUnconfirmed,
    /// aterm.toml's file binding (its target or logical path) changed.
    Binding,
}

pub(crate) struct DocumentHostRuntime {
    pub(crate) grants: DocumentGrantStore,
    pub(crate) persistence: DocumentPersistenceStore,
    journals: JournalSlot,
    recovery_status: BTreeMap<DocumentId, String>,
    /// Journal intents that observed a full worker queue. This is an ID-only,
    /// latest-wins latch: full encoded plans are released before insertion and
    /// rebuilt from `DocumentJournalStore` when a worker receive frees a slot.
    journal_retries: JournalRetrySet,
    /// A journal write landed while an update handoff was parked, after the
    /// successor may have read its restore image.
    journal_landed_while_parked: bool,
    /// Test seam: the headless App has no document worker, so journal effects
    /// run inline.
    #[cfg(all(test, unix))]
    held_journal_appends: Option<Vec<HeldJournalAppend>>,
    /// Test seam: [`Self::held_journal_appends`] for re-seats
    /// (`App::run_held_journal_reseat_for_test`).
    #[cfg(all(test, unix))]
    held_journal_reseats: Option<Vec<HeldJournalReseat>>,
    /// Test seam: a headless successor restores no document tab
    /// ([`App::successor_restores_document_tabs`]).
    #[cfg(all(test, unix))]
    carry_as_windowed_for_test: bool,
    /// Documents whose on-disk generation no longer matches the saver baseline,
    /// and why.
    disk_conflicts: BTreeMap<DocumentId, DiskConflictCause>,
    /// Documents an edit no person at this window is known to have made reached since they
    /// opened or last took the disk's text by a discard: a save of the aterm.toml in force keeps
    /// the owner's keys as the file has them while one is here
    /// ([`crate::owner_keys_save`]).
    unvouched: BTreeMap<DocumentId, crate::owner_keys_save::Unvouched>,
    inflight: BTreeSet<DocumentId>,
    pending_saves: BTreeMap<DocumentId, PendingDocumentSaveIntent>,
    pending_closes: BTreeMap<DocumentId, Vec<PendingDocumentClose>>,
    /// OS-window closes are all-or-nothing across every document leaf in that
    /// window: no tab or view is detached while any member still lacks its exact
    /// durable checkpoint.
    pending_window_closes: BTreeMap<WindowId, PendingWindowClose>,
    /// A document an armed window close waits on was discarded since the last
    /// park, which may have made that close ready with no save to escalate it:
    /// the park escalates it ([`App::escalate_document_shutdown`]).
    pub(crate) close_due: bool,
    /// A window or Quit close went since the last park, so tab and leaf closes
    /// it held may be free to finish: the park finishes each whose document is
    /// saved ([`App::resume_released_document_closes`]).
    closes_resume_due: bool,
    /// Whole-app Quit waits on one process-wide plan before update application or
    /// event-loop exit.
    pending_quit: Option<DocumentClosePlan>,
    /// Admissions running on an `aterm-document-admit` worker (the two-phase
    /// open; see [`App::begin_document_admission`]).
    admissions: BTreeMap<(WindowId, DocumentAdmissionKey), PendingDocumentAdmission>,
    next_admission: u64,
    /// The location each pending open reveals once its tab is installed
    /// ([`App::open_document_tab_at`]): keyed like
    /// [`Self::admissions`], the latest request's location winning, and taken by
    /// the install (or dropped with a failed one).
    reveals: BTreeMap<(WindowId, DocumentAdmissionKey), DocumentReveal>,
    /// The `edit` verb's tickets ([`crate::app_edit`]).
    pub(crate) edits: crate::app_edit::EditTickets,
    /// A document view closed, or a Suspended document's save or journal write landed, since the last [`App::release_unheld_documents`].
    pub(crate) release_due: bool,
    /// When the last recently-closed record that alone kept a document expires.
    pub(crate) release_deadline_ms: Option<u64>,
    /// The file watch over every open document's file (`app_document_watch`).
    pub(crate) watch: watch::DocumentWatch,
}

/// Where a document open lands its caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineReveal {
    pub(crate) line: u32,
    pub(crate) col: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DocumentReveal {
    Line(LineReveal),
    MarkdownFragment(String),
}

impl Default for DocumentHostRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentHostRuntime {
    /// The headless App's runtime.
    pub(crate) fn new() -> Self {
        Self::with_journals(JournalSlot::deferred(DocumentJournalStore::system_default))
    }

    /// The running App's runtime.
    pub(crate) fn opening() -> Self {
        Self::with_journals(JournalSlot::opening(DocumentJournalStore::system_default))
    }

    fn with_journals(journals: JournalSlot) -> Self {
        Self {
            grants: DocumentGrantStore::new(),
            persistence: DocumentPersistenceStore::default(),
            journals,
            recovery_status: BTreeMap::new(),
            journal_retries: JournalRetrySet::default(),
            journal_landed_while_parked: false,
            #[cfg(all(test, unix))]
            held_journal_appends: None,
            #[cfg(all(test, unix))]
            held_journal_reseats: None,
            #[cfg(all(test, unix))]
            carry_as_windowed_for_test: false,
            disk_conflicts: BTreeMap::new(),
            unvouched: BTreeMap::new(),
            inflight: BTreeSet::new(),
            pending_saves: BTreeMap::new(),
            pending_closes: BTreeMap::new(),
            pending_window_closes: BTreeMap::new(),
            close_due: false,
            closes_resume_due: false,
            pending_quit: None,
            admissions: BTreeMap::new(),
            next_admission: 1,
            reveals: BTreeMap::new(),
            edits: crate::app_edit::EditTickets::default(),
            release_due: false,
            release_deadline_ms: None,
            watch: watch::DocumentWatch::default(),
        }
    }
}

/// Identity of one two-phase open in flight: the exact URI and the hosting app.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DocumentAdmissionKey {
    pub(crate) uri: String,
    pub(crate) kind: AppKind,
    /// The `edit` ticket this admission opens a fresh view for ([`crate::app_edit`]).
    pub(crate) edit: Option<crate::app_edit::EditTicketId>,
}

/// One in-flight admission (see [`DocumentHostRuntime::admissions`]).
#[derive(Debug)]
struct PendingDocumentAdmission {
    ticket: DocumentAdmissionTicket,
    started: std::time::Instant,
    purpose: DocumentAdmissionPurpose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocumentAdmissionPurpose {
    Open,
    /// Follow the selected entry's symlinks in the admission worker.
    DirectoryEntry,
    /// Refresh an existing listing without opening a tab or moving focus.
    DirectoryRefresh(DocumentId),
}

/// Monotonic identity of one admission request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentAdmissionTicket(u64);

const DOCUMENT_ADMISSION_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// What the worker hands back through `Wake::DocumentAdmitted`.
#[derive(Debug)]
pub(crate) struct DocumentAdmissionOutcome {
    pub(crate) ticket: DocumentAdmissionTicket,
    pub(crate) wid: WindowId,
    pub(crate) key: DocumentAdmissionKey,
    pub(crate) result: Result<Box<AdmittedDocument>, DocumentOpenFailure>,
}

/// Why an admission's read refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocumentOpenFailure(pub(crate) DocumentHostError);

impl std::fmt::Display for DocumentOpenFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document open failed: {}", self.0)
    }
}

impl From<DocumentOpenFailure> for String {
    fn from(failure: DocumentOpenFailure) -> Self {
        failure.to_string()
    }
}

/// The tab an open landed in: the canonical document, the view showing it and that view's tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstalledDocument {
    pub(crate) kind: AppKind,
    pub(crate) canonical_uri: String,
    pub(crate) document: DocumentId,
    pub(crate) tab: crate::tab_model::TabId,
    pub(crate) view: crate::tab_model::ViewId,
}

impl InstalledDocument {
    /// The `app <kind> <uri> view=<n>` status line every opener reports.
    pub(crate) fn line(&self) -> String {
        format!(
            "app {} {} view={}",
            self.kind.as_str(),
            self.canonical_uri,
            self.view.get()
        )
    }
}

/// How a two-phase open request was disposed of on the calling thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocumentAdmission {
    /// A worker is reading the file; the tab appears when it reports back.
    InFlight(DocumentAdmissionTicket),
    /// The same request was already in flight; this one rides on it.
    Coalesced(DocumentAdmissionTicket),
    /// No event-loop proxy exists to report back to (the headless test harness).
    Opened(String),
}

impl DocumentAdmission {
    /// One status line for callers that report a string either way.
    pub(crate) fn message(&self, key: &DocumentAdmissionKey) -> String {
        match self {
            Self::Opened(line) => line.clone(),
            Self::InFlight(_) | Self::Coalesced(_) => {
                format!("admitting {} {}", key.kind.as_str(), key.uri)
            }
        }
    }
}

/// Why a worker's completion installed no tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocumentAdmissionRejected {
    /// Nothing to tell the user.
    Dropped(String),
    /// The read or the grant refused.
    Failed(String),
}

impl DocumentAdmissionRejected {
    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::Dropped(detail) | Self::Failed(detail) => detail,
        }
    }
}

impl App {
    /// Resolve and ensure the one process config file, then focus it in the existing native Editor app.
    pub(crate) fn ensure_and_open_config_editor_in_window(
        &mut self,
        wid: WindowId,
    ) -> Result<String, String> {
        self.ensure_and_open_config_editor_at_in_window(wid, None)
    }

    /// The command palette's "Open aterm.toml".
    pub(crate) fn open_config_editor_in_front(&mut self) -> Result<String, String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "there is no window to open aterm.toml in".to_string())?;
        self.ensure_and_open_config_editor_in_window(wid)
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

    /// Path-injected form for deterministic host tests.
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
        self.open_config_editor_placed(wid, path, target, Placement::FocusOrNewTab)
    }

    /// A restore's config-editor leaf: a fresh view of aterm.toml in a staging tab of its
    /// own, never the window's open aterm.toml tab, which Open aterm.toml would front.
    pub(crate) fn stage_config_editor_restore_view(
        &mut self,
        wid: WindowId,
    ) -> Result<String, String> {
        let path = crate::app_config::config_path().ok_or_else(|| {
            "could not resolve the config path (HOME/XDG_CONFIG_HOME unset)".to_string()
        })?;
        self.open_config_editor_placed(wid, &path, None, Placement::NewTab)
    }

    fn open_config_editor_placed(
        &mut self,
        wid: WindowId,
        path: &std::path::Path,
        target: Option<&crate::native_app::ConfigEditorTarget>,
        placement: Placement,
    ) -> Result<String, String> {
        let logical_path = ensure_config_file(path)?;
        let uri = crate::native_document_host::path_to_file_uri(&logical_path)
            .map_err(|error| format!("could not address aterm.toml: {error}"))?;
        self.open_document_tab_in_window_with_config_symlinks(
            wid,
            AppKind::Editor,
            &uri,
            true,
            placement,
        )?;
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
                buffer.clear_minibuffer();
                buffer.ensure_primary_visible(&snapshot.text, buffer.viewport_lines());
                state.status = Some(format!("Revealed {requested} in aterm.toml"));
            }
            ConfigEditorRevealDecision::AddAbsent { requested } => {
                buffer.clear_minibuffer();
                let Some((edit, added_as)) = absent_key_edit(&snapshot.text, &requested) else {
                    let note = match crate::native_config_language::config_key_record_root(
                        &requested,
                    ) {
                        Some(root) => format!(
                            "{requested} is not in aterm.toml yet. Add it inside a [[{root}]] \
                             record; completion offers its values."
                        ),
                        None => format!(
                            "{requested} is not in aterm.toml yet. Add a [[{requested}]] record \
                             at the end of the file; completion offers its fields."
                        ),
                    };
                    state.status = Some(note.clone());
                    state.reveal_status = Some(note);
                    return Ok(());
                };
                let _ = self.dispatch_editor_event(
                    instance,
                    view,
                    &AppEvent::EditorConfigCompletion(edit),
                )?;
                let text = self
                    .document_store
                    .snapshot(document)
                    .ok_or_else(|| "native config document disappeared during reveal".to_string())?
                    .text;
                let Some(crate::native_app::AppViewState::Editor(state)) =
                    self.native_runtime.view_state_mut(view)
                else {
                    return Err("native config editor view disappeared during reveal".to_string());
                };
                if let Some(buffer) = state.buffer.as_mut() {
                    // A value added at its default is selected, to type over;
                    // a line added without one keeps the caret where the
                    // value goes.
                    if added_as == AbsentKeyAdded::AtDefault
                        && let Some(range) = crate::native_config_language::config_key_source_range(
                            &text, &requested,
                        )
                    {
                        buffer.selections = vec![crate::native_editor::Selection {
                            anchor: range.start,
                            head: range.end,
                        }];
                        buffer.primary = 0;
                    }
                    buffer.clear_minibuffer();
                    buffer.ensure_primary_visible(&text, buffer.viewport_lines());
                }
                let note = match added_as {
                    AbsentKeyAdded::AtDefault => format!(
                        "Added {requested} at its default, which changes nothing. Undo removes it."
                    ),
                    AbsentKeyAdded::NeedsValue { record: None } => format!(
                        "Type a value for {requested}. Save waits for one; Undo removes the line."
                    ),
                    AbsentKeyAdded::NeedsValue { record: Some(root) } => format!(
                        "Added a [[{root}]] record: type a value for {requested}. Save waits for \
                         one; Undo removes it."
                    ),
                    AbsentKeyAdded::Table => format!(
                        "Added [{requested}], which changes nothing. Its entries go under it; \
                         Undo removes it."
                    ),
                };
                state.status = Some(note.clone());
                state.reveal_status = Some(note);
            }
        }
        Ok(())
    }

    /// Apply the exact stable disk observation that the reload host will parse and offer to the config service.
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
            self.set_document_disk_conflict(document, Some(DiskConflictCause::Binding));
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

    /// Exact current Manual assistance for keyboard dispatch.
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

    /// Route editor-native input into the canonical document workspace.
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
        // Read mode and the reader pane own their input. Document commands,
        // including Save/Finish/Abort, still belong to this same editor.
        if matches!(event, AppEvent::TextInput(_) | AppEvent::EditorInput(_))
            && matches!(self.native_runtime.view_state(view), Some(AppViewState::Editor(state))
                if state.markdown.as_ref().is_some_and(|workspace| !workspace.editor_focused()))
        {
            return Ok(None);
        }
        #[cfg(any(a11y_tree, test))]
        if matches!(event, AppEvent::InsertText(_))
            && matches!(self.native_runtime.view_state(view), Some(AppViewState::Editor(state))
                if state.markdown.as_ref().is_some_and(|workspace| !workspace.editor_focused()))
        {
            return Ok(None);
        }
        let editor_event = match event {
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(_) | AppEvent::EditorSetSelection { .. } => true,
            AppEvent::TextInput(
                TextInputEvent::Commit(_)
                | TextInputEvent::Preedit { .. }
                | TextInputEvent::SelectAll
                | TextInputEvent::Cancel,
            )
            | AppEvent::EditorInput(_)
            | AppEvent::EditorCommand(_)
            | AppEvent::EditorCompletion(_)
            | AppEvent::EditorConfigCompletion(_)
            | AppEvent::EditorConfigCompletionRejected
            | AppEvent::EditorConfigDiagnosticNavigate { .. }
            | AppEvent::EditorViewportChanged { .. }
            | AppEvent::ScrollLines(_) => true,
            _ => false,
        };
        if !editor_event {
            return Ok(None);
        }
        let crlf_on_disk = self
            .native_documents
            .persistence
            .observed(document)
            .is_some_and(|observed| {
                observed.encoding().eol == crate::native_document_io::LineEnding::CrLf
            });
        let held_line_ends = crlf_on_disk.then(|| crlf_held_insertion(event)).flatten();
        let event = held_line_ends.as_ref().unwrap_or(event);

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

        let mut granted = crate::command_registry::editor_grant(self.person_gesture_refused());
        // A concealed document's values reach the clipboard only on the person's own gesture: an
        // accessibility client's or another process's posted input copies none of them (one-mesh
        // design §4.8, "Revealed values stay out of the accessibility tree").
        if self.document_store.concealed(document).is_some() && self.gesture_unvouched().is_some() {
            granted.retain(|class| *class != crate::native_editor::CommandAuthority::Clipboard);
        }
        self.editor_workspace.granted = granted;
        // A yank is a key, not text: the engine writes a kill's CRLF as LF into this document.
        self.editor_workspace.crlf_on_disk = crlf_on_disk;
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
            let blocked_diagnostic =
                matches!(event, AppEvent::EditorConfigDiagnosticNavigate { .. })
                    && buffer.minibuffer_active();
            let host_scroll = blocked_diagnostic
                || matches!(
                    event,
                    AppEvent::ScrollLines(_)
                        | AppEvent::EditorViewportChanged { .. }
                        | AppEvent::TextInput(TextInputEvent::Preedit { .. })
                );
            let mut follow = if host_scroll {
                Follow::Scrolled
            } else {
                Follow::Reveal
            };
            let reduced_by_input = matches!(event, AppEvent::EditorInput(_));
            if !reduced_by_input && !blocked_diagnostic {
                state.preedit.clear();
                state.preedit_caret = None;
                if !host_scroll {
                    state.config_completion_selected = 0;
                    state.config_completion_interaction = None;
                    state.reveal_status = None;
                }
            }
            let mut followed = |reduction: Reduction| {
                follow = reduction.follow;
                reduction.effects
            };
            let reduced = match event {
                AppEvent::EditorInput(input) => workspace
                    .handle_input(store, buffer, input.clone())
                    .map(&mut followed),
                #[cfg(any(a11y_tree, test))]
                AppEvent::InsertText(text) => workspace
                    .handle_input(store, buffer, EditorInput::Text(text.clone()))
                    .map(&mut followed),
                AppEvent::TextInput(TextInputEvent::Commit(text)) => workspace
                    .handle_input(store, buffer, EditorInput::Text(text.clone()))
                    .map(&mut followed),
                AppEvent::TextInput(TextInputEvent::Preedit { text, selection }) => {
                    // A composition in a grid with no cell open opens the current cell, as
                    // typing does, and composes at the cell editor's caret.
                    if !text.is_empty() {
                        workspace.grid_compose(store, buffer).map_err(|error| {
                            format!("grid cell editor: {}", editor_error_message(&error))
                        })?;
                    }
                    state.preedit.clone_from(text);
                    state.preedit_caret = crate::native_text_input::normalize_preedit_selection(
                        text,
                        selection.clone(),
                    )
                    .map(|selection| selection.end);
                    Ok(Vec::new())
                }
                AppEvent::TextInput(TextInputEvent::SelectAll) => {
                    workspace.select_whole_document(store, buffer)
                }
                AppEvent::TextInput(TextInputEvent::Cancel) => {
                    workspace.execute(store, buffer, EditorCommand::Abort)
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
                    if let Some(effects) = workspace.minibuffer_blocks_document_input(buffer) {
                        Ok(effects)
                    } else {
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
                }
                #[cfg(any(a11y_tree, test))]
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
                        // Lines of text, or rows of a grid.
                        workspace
                            .scroll_view(store, buffer, *lines)
                            .map(|()| Vec::new())
                    }
                }
                _ => unreachable!("editor event filter above is exhaustive"),
            };
            if follow == Follow::Ignored {
                return Ok(Some(EventResult::Bubble));
            }
            if reduced_by_input {
                state.preedit.clear();
                state.preedit_caret = None;
                if follow == Follow::Reveal {
                    state.config_completion_selected = 0;
                    state.config_completion_interaction = None;
                    state.reveal_status = None;
                }
            }
            if follow == Follow::Reveal
                && let Some(snapshot) = store.snapshot(document)
            {
                let visible_lines = buffer.viewport_lines();
                buffer.ensure_primary_visible(&snapshot.text, visible_lines);
            }
            match reduced {
                Ok(effects) => {
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

        // Neither a step through the diagnostics nor a change of how many rows the
        // viewport shows touches the document: published as a document change, it would
        // reset the view's diagnostic cycle and completion choice.
        if !matches!(
            event,
            AppEvent::EditorConfigDiagnosticNavigate { .. }
                | AppEvent::EditorViewportChanged { .. }
        ) {
            self.publish_editor_commit(document, view);
        }
        for effect in effects {
            self.execute_editor_effect(view, effect)?;
        }
        Ok(Some(EventResult::Handled))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Type",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Edit",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    fn publish_editor_commit(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) {
        let commits = self.editor_workspace.take_commits(document);
        if let Some((seq, _)) = commits.last() {
            if let Some(unvouched) = self.gesture_unvouched() {
                self.native_documents.unvouched.insert(document, unvouched);
            }
            self.edit_document_committed(document, source_view, *seq);
            let document_views = self
                .view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self
                            .native_runtime
                            .document_id(native.instance)
                            .is_some_and(|candidate| candidate == document) =>
                    {
                        Some(view)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for view in document_views {
                if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
                {
                    for (seq, deltas) in &commits {
                        if view != source_view
                            && let Some(buffer) = state.buffer.as_mut()
                        {
                            buffer.observe_external(*seq, deltas);
                        }
                        if let Some(workspace) = state.markdown.as_mut() {
                            workspace.observe_document_edits(deltas);
                        }
                    }
                }
            }
        }

        let Some(snapshot) = self.document_store.snapshot(document) else {
            return;
        };
        let journal_result = self
            .native_documents
            .journals
            .available_mut()
            .and_then(|journals| journals.observe_commit(&snapshot));
        let journal_result =
            journal_result.and_then(|()| self.checkpoint_commit_back_on_disk(&snapshot));
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
        if let Err(message) = self.adopt_disk_when_buffer_is_base(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    fn checkpoint_commit_back_on_disk(
        &mut self,
        snapshot: &crate::document_store::DocumentSnapshot,
    ) -> Result<(), String> {
        let document = snapshot.id;
        if self.document_store.dirty(document) != Some(true)
            || self.document_save_owed(document)
            || self.native_documents.disk_conflicts.contains_key(&document)
            || !self.buffer_is_admitted_baseline(snapshot)
        {
            return Ok(());
        }
        self.mark_buffer_the_admitted_file(snapshot)
    }

    fn checkpoint_current_back_on_disk(&mut self, document: DocumentId) -> Result<(), String> {
        let Some(snapshot) = self.document_store.snapshot(document) else {
            return Ok(());
        };
        if self.document_store.dirty(document) != Some(true) {
            return Ok(());
        }
        self.checkpoint_commit_back_on_disk(&snapshot)?;
        if self.document_store.dirty(document) == Some(false) {
            let revision = self.document_store.revision(document).unwrap_or(0);
            self.native_runtime
                .publish_document(document, snapshot.text.as_ref(), revision, false);
            for (wid, instance, view) in self.document_native_views(document) {
                let _ = self.native_runtime.dispatch(
                    instance,
                    view,
                    AppEvent::DocumentChanged { document, revision },
                );
                self.refresh_native_presentation(wid, instance, view);
                self.request_native_redraw(wid);
            }
        }
        Ok(())
    }

    /// Whether a save of `document` is in flight or owed.
    pub(crate) fn document_save_owed(&self, document: DocumentId) -> bool {
        self.native_documents.persistence.save_in_flight(document)
            || self.native_documents.inflight.contains(&document)
            || self.native_documents.pending_saves.contains_key(&document)
    }

    fn buffer_is_admitted_baseline(
        &self,
        snapshot: &crate::document_store::DocumentSnapshot,
    ) -> bool {
        let document = snapshot.id;
        let Some(observed) = self.native_documents.persistence.observed(document) else {
            return false;
        };
        let text = snapshot.text.as_ref();
        if !observed.exists || !encodes_to_len(text, observed.encoding(), observed.len) {
            return false;
        }
        let bytes = crate::native_document_host::encode_for_disk(text, observed.encoding());
        observed.content == crate::native_document_io::ContentFingerprint::of(&bytes)
            && self
                .native_documents
                .persistence
                .observed_text_matches(document, snapshot.text.as_ref())
    }

    fn mark_buffer_the_admitted_file(
        &mut self,
        snapshot: &crate::document_store::DocumentSnapshot,
    ) -> Result<(), String> {
        let document = snapshot.id;
        self.document_store
            .checkpoint_ack(document, snapshot.seq)
            .map_err(|error| format!("could not mark the unchanged file durable: {error:?}"))?;
        for (_, _, view) in self.document_native_views(document) {
            self.set_editor_view_status(view, "No unsaved changes");
        }
        if let Some(journals) = self.native_documents.journals.as_mut() {
            journals.request_checkpoint(
                crate::native_document_io::DurableCheckpoint {
                    document,
                    seq: snapshot.seq,
                    source: crate::native_document_io::DurableSource::StableFileObservation,
                },
                snapshot.text.clone(),
            )?;
        }
        Ok(())
    }

    fn adopt_disk_when_buffer_is_base(&mut self, document: DocumentId) -> Result<(), String> {
        let Some(cause) = self.native_documents.disk_conflicts.get(&document).copied() else {
            return Ok(());
        };
        if cause == DiskConflictCause::Binding || self.document_save_owed(document) {
            return Ok(());
        }
        let Some(snapshot) = self.document_store.snapshot(document) else {
            return Ok(());
        };
        if !self.buffer_is_admitted_baseline(&snapshot) {
            return Ok(());
        }
        if self.document_store.dirty(document) == Some(true) {
            self.mark_buffer_the_admitted_file(&snapshot)?;
        }
        self.refresh_native_document(document).map(|_| ())
    }

    fn drive_document_journal(&mut self, document: DocumentId) -> Result<(), String> {
        self.drive_document_journal_once(document).map(|_| ())
    }

    fn drive_document_journal_once(
        &mut self,
        document: DocumentId,
    ) -> Result<JournalDriveOutcome, String> {
        let proxy = self.proxy.clone();
        let queue = if proxy.is_some() {
            Some(native_document_queue()?)
        } else {
            None
        };
        let fenced = self.update_handoff_in_flight();
        loop {
            let journals = self
                .native_documents
                .journals
                .as_mut()
                .ok_or_else(|| "document journal is unavailable".to_string())?;
            journals.set_reseat_fenced(fenced);
            let effect = journals.next_effect(document)?;
            let Some(effect) = effect else {
                self.native_documents.journal_retries.remove(document);
                return Ok(JournalDriveOutcome::Idle);
            };
            match effect {
                JournalEffect::Append { path, key, plan } => {
                    if let (Some(proxy), Some(queue)) = (proxy.clone(), queue) {
                        let native_documents = &mut self.native_documents;
                        let journals = native_documents
                            .journals
                            .as_mut()
                            .expect("journal store checked above");
                        return try_enqueue_document_journal_job(
                            queue,
                            NativeDocumentJob::JournalAppend {
                                document,
                                path,
                                key,
                                plan: plan.clone(),
                                proxy,
                            },
                            document,
                            JournalAdmissionEffect::Append(plan.generation),
                            journals,
                            &mut native_documents.journal_retries,
                        );
                    }
                    #[cfg(all(test, unix))]
                    if let Some(held) = self.native_documents.held_journal_appends.as_mut() {
                        held.push(HeldJournalAppend {
                            document,
                            path,
                            key,
                            plan,
                        });
                        return Ok(JournalDriveOutcome::Enqueued);
                    }
                    let result =
                        execute_journal_append(&path, key, &plan, JournalLockPatience::EventLoop);
                    let completion = self
                        .native_documents
                        .journals
                        .as_mut()
                        .expect("journal store checked above")
                        .complete_append(document, plan.generation, result);
                    self.note_document_journal_landing(&completion);
                    if let Some(message) = journal_completion_error(completion) {
                        return Err(message);
                    }
                }
                JournalEffect::Rewrite { path, plan } => {
                    if let (Some(proxy), Some(queue)) = (proxy.clone(), queue) {
                        let native_documents = &mut self.native_documents;
                        let journals = native_documents
                            .journals
                            .as_mut()
                            .expect("journal store checked above");
                        return try_enqueue_document_journal_job(
                            queue,
                            NativeDocumentJob::JournalRewrite {
                                document,
                                path,
                                plan: plan.clone(),
                                proxy,
                            },
                            document,
                            JournalAdmissionEffect::Rewrite(plan.generation),
                            journals,
                            &mut native_documents.journal_retries,
                        );
                    }
                    let result =
                        execute_journal_rewrite(&path, &plan, JournalLockPatience::EventLoop);
                    let completion = self
                        .native_documents
                        .journals
                        .as_mut()
                        .expect("journal store checked above")
                        .complete_rewrite(document, plan.generation, result);
                    self.note_document_journal_landing(&completion);
                    if let Some(message) = journal_completion_error(completion) {
                        return Err(message);
                    }
                }
                JournalEffect::Reseat { path, plan } => {
                    if let (Some(proxy), Some(queue)) = (proxy.clone(), queue) {
                        let native_documents = &mut self.native_documents;
                        let journals = native_documents
                            .journals
                            .as_mut()
                            .expect("journal store checked above");
                        return try_enqueue_document_journal_job(
                            queue,
                            NativeDocumentJob::JournalReseat {
                                document,
                                path,
                                plan: plan.clone(),
                                proxy,
                            },
                            document,
                            JournalAdmissionEffect::Reseat(plan.generation),
                            journals,
                            &mut native_documents.journal_retries,
                        );
                    }
                    #[cfg(all(test, unix))]
                    if let Some(held) = self.native_documents.held_journal_reseats.as_mut() {
                        held.push(HeldJournalReseat {
                            document,
                            path,
                            plan,
                        });
                        return Ok(JournalDriveOutcome::Enqueued);
                    }
                    let result =
                        execute_journal_reseat(&path, &plan, JournalLockPatience::EventLoop);
                    let completion = self
                        .native_documents
                        .journals
                        .as_mut()
                        .expect("journal store checked above")
                        .complete_reseat(document, plan.generation, result);
                    self.note_document_journal_landing(&completion);
                    if let Some(message) = journal_completion_error(completion) {
                        return Err(message);
                    }
                }
            }
        }
    }

    /// Pump one coalesced journal intent for the one channel slot just released by the worker.
    pub(crate) fn native_document_queue_slot_available(&mut self) {
        loop {
            let Some(document) = self.native_documents.journal_retries.pop_first() else {
                return;
            };
            match self.drive_document_journal_once(document) {
                Ok(JournalDriveOutcome::Idle) => {}
                Ok(JournalDriveOutcome::Enqueued | JournalDriveOutcome::Deferred) => return,
                Err(message) => self.set_document_recovery_status(document, Some(message)),
            }
        }
    }

    pub(crate) fn finish_native_document_journal(
        &mut self,
        document: DocumentId,
        generation: crate::native_document_io::JournalGeneration,
        result: crate::native_document_io::JournalAppendResult,
    ) {
        self.note_document_settling(document);
        let completion = self
            .native_documents
            .journals
            .as_mut()
            .map_or(JournalCompletion::Stale, |journals| {
                journals.complete_append(document, generation, result)
            });
        self.note_document_journal_landing(&completion);
        if matches!(&completion, JournalCompletion::Durable { .. })
            && self.document_store.dirty(document) == Some(true)
        {
            for (_, _, view) in self.document_native_views(document) {
                self.set_editor_view_status(view, "Draft autosaved");
            }
        }
        if let Some(message) = journal_completion_error(completion) {
            self.set_document_recovery_status(document, Some(message));
            self.drive_owed_document_journal_reseat(document);
            return;
        }
        if let Err(message) = self.drive_document_journal(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    fn drive_owed_document_journal_reseat(&mut self, document: DocumentId) {
        if self
            .native_documents
            .journals
            .as_ref()
            .is_some_and(|journals| journals.reseat_owed(document))
            && let Err(message) = self.drive_document_journal(document)
        {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    pub(crate) fn finish_native_document_journal_reseat(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
        result: JournalReseatResult,
    ) {
        self.note_document_settling(document);
        let preserved = matches!(
            &result,
            JournalReseatResult::Republished {
                preserved: true,
                ..
            }
        );
        let completion = self
            .native_documents
            .journals
            .as_mut()
            .map_or(JournalCompletion::Stale, |journals| {
                journals.complete_reseat(document, generation, result)
            });
        self.note_document_journal_landing(&completion);
        if matches!(&completion, JournalCompletion::Durable { .. }) {
            aterm_log::info!(
                "document journal: re-established as this process's own after an update \
                 rolled back{}",
                if preserved {
                    "; the image a successor left there was preserved aside first"
                } else {
                    ""
                }
            );
            self.set_document_recovery_status(document, None);
        }
        if let Some(message) = journal_completion_error(completion) {
            self.set_document_recovery_status(document, Some(message));
            return;
        }
        if let Err(message) = self.drive_document_journal(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    /// Whether one document's unsaved draft rides an update to the successor.
    pub(crate) fn draft_carry(&self, document: DocumentId) -> DraftCarry {
        let Some(head) = self.document_store.snapshot(document) else {
            return DraftCarry::Refused("the document is no longer open".to_string());
        };
        if !self.successor_restores_document_tabs() {
            return DraftCarry::Refused(
                if self.headless {
                    "a headless successor reopens no document tab, so the editor would close"
                } else {
                    "an update on this platform reopens no document tab, so the editor would close"
                }
                .to_string(),
            );
        }
        let Some(journals) = self.native_documents.journals.as_ref() else {
            return DraftCarry::Refused(format!(
                "it has no draft journal ({})",
                self.native_documents
                    .journals
                    .unavailable()
                    .unwrap_or("document recovery is unavailable")
            ));
        };
        if self.native_documents.disk_conflicts.contains_key(&document) {
            return DraftCarry::Refused(
                "its file changed on disk after the draft began".to_string(),
            );
        }
        let candidate = match journals.carry_candidate(&head) {
            Ok(candidate) => candidate,
            Err(DraftCarryRefusal::Writing | DraftCarryRefusal::Reseating) => {
                return DraftCarry::Writing;
            }
            Err(DraftCarryRefusal::Behind)
                if self.native_documents.journal_retries.contains(document) =>
            {
                return DraftCarry::Writing;
            }
            Err(DraftCarryRefusal::Behind) => {
                return DraftCarry::Refused(
                    "its latest edit is not in the draft journal (the editor's status says why)"
                        .to_string(),
                );
            }
            Err(DraftCarryRefusal::Unjournaled) => {
                return DraftCarry::Refused("it has no draft journal".to_string());
            }
            Err(DraftCarryRefusal::ReseatFailed) => {
                return DraftCarry::Refused(
                    "its draft journal could not be re-established after an update rolled back \
                     (the editor's status says why; it is retried before every update)"
                        .to_string(),
                );
            }
            Err(DraftCarryRefusal::AppendFailed) => {
                return DraftCarry::Refused(
                    "its latest edit could not be written to the draft journal (the editor's \
                     status says why; it is retried before every update)"
                        .to_string(),
                );
            }
        };
        if !self
            .native_documents
            .persistence
            .observed(document)
            .is_some_and(|observed| observed.exists)
        {
            return DraftCarry::Refused("its file is not on disk".to_string());
        }
        let Some(base) = self
            .native_documents
            .persistence
            .observed_text_fingerprint(document)
        else {
            return DraftCarry::Refused(
                "the file's admitted text is not known to this process".to_string(),
            );
        };
        match verify_carried_draft(&candidate, &head.text, base) {
            Ok(()) => DraftCarry::Carried {
                bytes: head.text.len(),
            },
            Err(why) => DraftCarry::Refused(why),
        }
    }

    /// Whether this process's update successor reopens its document tabs.
    pub(crate) const fn successor_restores_document_tabs(&self) -> bool {
        #[cfg(all(test, unix))]
        if self.native_documents.carry_as_windowed_for_test {
            return true;
        }
        cfg!(unix) && !self.headless
    }

    /// Answer [`Self::successor_restores_document_tabs`] as a windowed App.
    #[cfg(all(test, unix))]
    pub(crate) fn carry_drafts_as_windowed_for_test(&mut self) {
        self.native_documents.carry_as_windowed_for_test = true;
    }

    /// Whether any draft-journal write is on the worker now.
    pub(crate) fn document_journal_writes_in_flight(&self) -> bool {
        self.native_documents
            .journals
            .as_ref()
            .is_some_and(DocumentJournalStore::writes_in_flight)
    }

    /// Whether a document with unsaved text has a draft-journal write on the worker now.
    pub(crate) fn dirty_document_journal_write_in_flight(&self) -> bool {
        self.native_documents
            .journals
            .as_ref()
            .is_some_and(|journals| {
                journals
                    .busy_documents()
                    .any(|document| self.document_store.dirty(document) == Some(true))
            })
    }

    /// The documents an open native view shows.
    pub(crate) fn open_native_documents(&self) -> std::collections::BTreeSet<DocumentId> {
        self.view_store
            .iter()
            .filter_map(|(_, link)| match link {
                crate::tab_model::View::Native(native) => {
                    self.native_runtime.document_id(native.instance)
                }
                crate::tab_model::View::Terminal(_) => None,
            })
            .collect()
    }

    /// Whether aterm's own document work is under way: a journal write on the worker, a journal
    /// write waiting for a worker slot, or a checkpoint an open document's close waits on.
    pub(crate) fn own_document_work_pending(&self) -> bool {
        self.document_journal_writes_in_flight()
            || !self.native_documents.journal_retries.is_empty()
            || self.open_native_documents().into_iter().any(|document| {
                matches!(
                    self.document_store.phase(document),
                    Some(crate::document_store::DocumentPhase::Closing { .. })
                )
            })
    }

    /// Whether aterm's own document work has stood in the automatic update's way for
    /// [`crate::app_native::OWN_WORK_BACKSTOP`] of attempts in a row
    /// ([`crate::native_update_lane_hold::LaneHoldClocks::own_documents`]). Past it, a wait that
    /// protects no unsaved text stops holding the update, and one that does is named as the
    /// person's unsaved work.
    pub(crate) fn own_document_work_overdue(&self, now: std::time::Instant) -> bool {
        self.lane_hold_clocks
            .own_documents
            .overdue(crate::app_native::OWN_WORK_BACKSTOP, now)
    }

    /// Start the own-work clock `ago` before now, as if the lane had seen the work since then.
    #[cfg(all(test, unix))]
    pub(crate) fn backdate_own_document_work_for_test(&mut self, ago: std::time::Duration) {
        self.lane_hold_clocks.own_documents =
            crate::native_update_lane_hold::HoldClock::backdated_for_test(ago);
    }

    /// Record a journal write that landed while an update handoff was parked
    /// ([`DocumentHostRuntime::journal_landed_while_parked`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Land",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    fn note_document_journal_landing(&mut self, completion: &JournalCompletion) {
        if matches!(completion, JournalCompletion::Durable { .. }) && self.update_handoff_parked() {
            self.native_documents.journal_landed_while_parked = true;
        }
    }

    /// Whether a journal write landed after the parked successor may have read its image.
    pub(crate) const fn document_journal_landed_while_parked(&self) -> bool {
        self.native_documents.journal_landed_while_parked
    }

    /// After an update handoff rolls back, every journal the successor may have republished while it restored its documents is owed a re-seat, and each is driven now.
    #[cfg(unix)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Rollback",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    pub(crate) fn reseat_document_journals_after_rollback(&mut self) {
        self.native_documents.journal_landed_while_parked = false;
        let Some(journals) = self.native_documents.journals.as_mut() else {
            return;
        };
        for document in journals.owe_reseat_all() {
            if let Err(message) = self.drive_document_journal(document) {
                self.set_document_recovery_status(document, Some(message));
            }
        }
    }

    /// Drive every journal owing work nothing is doing (a re-seat, a checkpoint, or a head a failed append left behind) before an update asks whether each unsaved draft can be carried.
    pub(crate) fn drive_owed_document_journals(&mut self) {
        let Some(journals) = self.native_documents.journals.as_ref() else {
            return;
        };
        let owed = journals
            .owed_documents()
            .into_iter()
            .filter(|document| {
                !journals.write_failed(*document)
                    || self.document_store.dirty(*document) == Some(true)
            })
            .collect::<Vec<_>>();
        for document in owed {
            if self.native_documents.journal_retries.contains(document) {
                continue;
            }
            if let Err(message) = self.drive_document_journal(document) {
                self.set_document_recovery_status(document, Some(message));
            }
        }
    }

    /// Point this App's draft journals at `root`.
    #[cfg(all(test, unix))]
    pub(crate) fn use_document_journal_root_for_test(&mut self, root: std::path::PathBuf) {
        self.native_documents.journals = JournalSlot::for_test(root);
    }

    /// The journal identity this App's store gives `document`'s URI.
    #[cfg(all(test, unix))]
    pub(crate) fn document_journal_key_for_test(
        &self,
        document: DocumentId,
    ) -> Option<crate::native_document_io::JournalDocumentKey> {
        let uri = self.document_store.canonical_uri(document)?;
        Some(self.native_documents.journals.as_ref()?.key_for(uri))
    }

    #[cfg(all(test, unix))]
    pub(crate) fn document_journal_path_for_test(
        &self,
        document: DocumentId,
    ) -> Option<std::path::PathBuf> {
        self.native_documents
            .journals
            .as_ref()?
            .journal_path(document)
    }

    /// The journal image this App last published for `document`.
    #[cfg(all(test, unix))]
    pub(crate) fn document_journal_image_for_test(
        &self,
        document: DocumentId,
    ) -> Option<crate::native_document_io::ContentFingerprint> {
        self.native_documents
            .journals
            .as_ref()?
            .durable_image(document)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn document_journal_durable_seq_for_test(
        &self,
        document: DocumentId,
    ) -> Option<aterm_buffer::Seq> {
        self.native_documents
            .journals
            .as_ref()?
            .durable_seq(document)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn document_journal_reseat_owed_for_test(&self, document: DocumentId) -> bool {
        self.native_documents
            .journals
            .as_ref()
            .is_some_and(|journals| journals.reseat_owed(document))
    }

    /// Hold every journal append from now on instead of running it inline.
    #[cfg(all(test, unix))]
    pub(crate) fn hold_journal_appends_for_test(&mut self) {
        self.native_documents.held_journal_appends = Some(Vec::new());
    }

    #[cfg(all(test, unix))]
    pub(crate) fn held_journal_appends_for_test(&self) -> &[HeldJournalAppend] {
        self.native_documents
            .held_journal_appends
            .as_deref()
            .unwrap_or_default()
    }

    /// Run the oldest held append exactly as the document worker does.
    #[cfg(all(test, unix))]
    pub(crate) fn run_held_journal_append_for_test(&mut self) -> bool {
        let Some(held) = self
            .native_documents
            .held_journal_appends
            .as_mut()
            .filter(|held| !held.is_empty())
            .map(|held| held.remove(0))
        else {
            return false;
        };
        let result = execute_journal_append(
            &held.path,
            held.key,
            &held.plan,
            JournalLockPatience::Worker,
        );
        self.finish_native_document_journal(held.document, held.plan.generation, result);
        true
    }

    /// Hold every journal re-seat from now on instead of running it inline.
    #[cfg(all(test, unix))]
    pub(crate) fn hold_journal_reseats_for_test(&mut self) {
        self.native_documents.held_journal_reseats = Some(Vec::new());
    }

    #[cfg(all(test, unix))]
    pub(crate) fn held_journal_reseats_for_test(&self) -> &[HeldJournalReseat] {
        self.native_documents
            .held_journal_reseats
            .as_deref()
            .unwrap_or_default()
    }

    /// Run the oldest held re-seat exactly as the document worker does.
    #[cfg(all(test, unix))]
    pub(crate) fn run_held_journal_reseat_for_test(&mut self) -> bool {
        let Some(held) = self
            .native_documents
            .held_journal_reseats
            .as_mut()
            .filter(|held| !held.is_empty())
            .map(|held| held.remove(0))
        else {
            return false;
        };
        let result = execute_journal_reseat(&held.path, &held.plan, JournalLockPatience::Worker);
        self.finish_native_document_journal_reseat(held.document, held.plan.generation, result);
        true
    }

    #[cfg(all(test, unix))]
    pub(crate) fn document_recovery_status_for_test(&self, document: DocumentId) -> Option<&str> {
        self.native_documents
            .recovery_status
            .get(&document)
            .map(String::as_str)
    }

    /// Test seam: start a save of `document`'s head as the event loop's worker would, and hold it in flight.
    #[cfg(test)]
    pub(crate) fn hold_document_save_for_test(
        &mut self,
        document: DocumentId,
    ) -> crate::native_document_host::PendingDocumentSave {
        let snapshot = self.document_store.snapshot(document).expect("document");
        let pending = self
            .native_documents
            .persistence
            .begin(&snapshot)
            .expect("a save begins");
        assert!(self.native_documents.inflight.insert(document));
        self.native_runtime.set_document_saving(document, true);
        pending
    }

    /// Test seam: run a save [`Self::hold_document_save_for_test`] held and reduce its completion as `Wake` delivery does.
    #[cfg(test)]
    pub(crate) fn land_held_document_save_for_test(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        pending: crate::native_document_host::PendingDocumentSave,
    ) -> Result<(), String> {
        let grant = self
            .native_documents
            .grants
            .cloned_grant(pending.grant)
            .expect("the save's grant");
        let result = crate::native_document_host::execute_granted_save(&grant, &pending.plan);
        self.finish_native_document_save(
            document,
            source_view,
            pending.plan.generation,
            pending.plan.bytes,
            result,
            None,
        )
    }

    /// Test seam: whether a close of `document` waits for its checkpoint.
    #[cfg(test)]
    pub(crate) fn document_close_pending_for_test(&self, document: DocumentId) -> bool {
        self.native_documents.pending_closes.contains_key(&document)
    }

    pub(crate) fn finish_native_document_journal_rewrite(
        &mut self,
        document: DocumentId,
        generation: JournalRewriteGeneration,
        result: JournalRewriteResult,
    ) {
        self.note_document_settling(document);
        let completion = self
            .native_documents
            .journals
            .as_mut()
            .map_or(JournalCompletion::Stale, |journals| {
                journals.complete_rewrite(document, generation, result)
            });
        self.note_document_journal_landing(&completion);
        if matches!(&completion, JournalCompletion::Durable { .. })
            && self.document_store.dirty(document) == Some(true)
        {
            for (_, _, view) in self.document_native_views(document) {
                self.set_editor_view_status(view, "Draft autosaved");
            }
        }
        if let Some(message) = journal_completion_error(completion) {
            self.set_document_recovery_status(document, Some(message));
            self.drive_owed_document_journal_reseat(document);
            return;
        }
        if let Err(message) = self.drive_document_journal(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    fn set_document_recovery_status(&mut self, document: DocumentId, message: Option<String>) {
        let changed = self.native_documents.recovery_status.get(&document) != message.as_ref();
        if let Some(message) = &message {
            self.native_documents
                .recovery_status
                .insert(document, message.clone());
        } else {
            self.native_documents.recovery_status.remove(&document);
        }
        self.native_runtime
            .set_document_recovery_status(document, message);
        if changed {
            self.repaint_document_views(document);
        }
    }

    fn set_document_disk_conflict(
        &mut self,
        document: DocumentId,
        conflict: Option<DiskConflictCause>,
    ) {
        let changed = self.native_documents.disk_conflicts.get(&document).copied() != conflict;
        if let Some(cause) = conflict {
            self.native_documents.disk_conflicts.insert(document, cause);
        } else {
            self.native_documents.disk_conflicts.remove(&document);
        }
        self.native_runtime
            .set_document_disk_conflict(document, conflict.is_some());
        // The header row's notice stands only for a change on disk.
        let notice_cleared = conflict != Some(DiskConflictCause::Version)
            && self.native_runtime.set_document_disk_notice(document, None);
        if changed || notice_cleared {
            self.repaint_document_views(document);
        }
    }

    /// Shared document status can change without changing the draft's text sequence.
    fn repaint_document_views(&mut self, document: DocumentId) {
        for (window, instance, view) in self.document_native_views(document) {
            self.native_runtime.change_view_state(view, |state| {
                let common = state.common_mut();
                common.presentation_revision = common.presentation_revision.wrapping_add(1);
                true
            });
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
    }

    fn execute_editor_effect(
        &mut self,
        source_view: crate::tab_model::ViewId,
        effect: EditorEffect,
    ) -> Result<(), String> {
        match effect {
            EditorEffect::OpenPath { uri } => {
                let wid = self
                    .windows
                    .iter()
                    .find_map(|(wid, window)| {
                        window
                            .tab_set
                            .tabs()
                            .iter()
                            .any(|tab| tab.root.contains(source_view))
                            .then_some(*wid)
                    })
                    .ok_or_else(|| "directory window disappeared".to_string())?;
                self.begin_document_admission_for(
                    wid,
                    DocumentAdmissionKey {
                        // Ordinary opens prefer Read when the admitted canonical
                        // document supports Markdown, regardless of an alias's suffix.
                        kind: AppKind::Markdown,
                        uri,
                        edit: None,
                    },
                    DocumentAdmissionPurpose::DirectoryEntry,
                )?;
            }
            EditorEffect::RefreshDirectory { document } => {
                let wid = self
                    .document_native_views(document)
                    .into_iter()
                    .find_map(|(wid, _, view)| (view == source_view).then_some(wid))
                    .ok_or("directory view disappeared")?;
                let uri = self
                    .document_store
                    .canonical_uri(document)
                    .ok_or("directory disappeared")?;
                self.begin_document_admission_for(
                    wid,
                    DocumentAdmissionKey {
                        uri: uri.to_string(),
                        kind: AppKind::Editor,
                        edit: None,
                    },
                    DocumentAdmissionPurpose::DirectoryRefresh(document),
                )?;
            }
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
                    state.status = None;
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
            EditorEffect::KeepDocument { document } => {
                self.keep_document_over_disk(document)?;
            }
            EditorEffect::CloseView {
                why: CloseWhy::Plain,
            } => {
                if let Err(why) = self.close_editor_view(source_view) {
                    let status = self.editor_close_status(source_view, None, &why);
                    self.set_editor_view_status(source_view, &status);
                }
            }
            EditorEffect::CloseView {
                why: CloseWhy::Finish,
            } => self.finish_edit_view(source_view),
            EditorEffect::CloseView {
                why: CloseWhy::Abort,
            } => self.abort_edit_view(source_view),
            EditorEffect::KillBuffer { query } => {
                self.kill_open_editor_buffer(source_view, &query);
            }
            EditorEffect::KillToClipboard { text } => {
                if !crate::control::pbcopy(&text) {
                    aterm_log::warn!("editor: the kill did not reach the clipboard");
                }
            }
        }
        Ok(())
    }

    /// Why the editor command `event` names may not run on this gesture.
    pub(crate) fn refuse_editor_command(
        &mut self,
        view: crate::tab_model::ViewId,
        event: &AppEvent,
    ) -> Option<String> {
        let command = match event {
            AppEvent::EditorCommand(command) => *command,
            AppEvent::EditorCompletion(crate::native_editor::EditorCompletionAction::Choose(
                index,
            )) => {
                let Some(AppViewState::Editor(state)) = self.native_runtime.view_state(view) else {
                    return None;
                };
                let crate::native_editor::Minibuffer::Command { query, .. } =
                    &state.buffer.as_ref()?.minibuffer
                else {
                    return None;
                };
                *crate::native_editor::command_completions(query).get(*index)?
            }
            _ => return None,
        };
        let grant = crate::command_registry::editor_grant(self.person_gesture_refused());
        let refusal = self.editor_workspace.refusal(command, &grant)?;
        aterm_log::info!(
            "editor: {:?} refused: {refusal}",
            crate::command_registry::editor_command(command)
        );
        if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
            state.status = Some(refusal.clone());
            state.common.presentation_revision =
                state.common.presentation_revision.saturating_add(1);
        }
        Some(refusal)
    }

    /// Close one Editor view as ⌘W closes it.
    pub(crate) fn close_editor_view(
        &mut self,
        view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        let (wid, tab) = self
            .windows
            .iter()
            .find_map(|(wid, ws)| {
                ws.tab_set
                    .tabs()
                    .iter()
                    .find(|tab| tab.root.contains(view))
                    .map(|tab| (*wid, tab.id))
            })
            .ok_or_else(|| "Editor window is no longer available".to_string())?;
        let closed = self.close_view_keeping_front(wid, tab, view);
        self.request_native_redraw(wid);
        closed
    }

    /// What to tell the person when closing the Editor `view` (whose buffer is `title`, or "this buffer") did not close it at once.
    pub(crate) fn editor_close_status(
        &self,
        view: crate::tab_model::ViewId,
        title: Option<&str>,
        why: &str,
    ) -> String {
        let buffer = title.unwrap_or("this buffer");
        let document_view = DocumentViewId(view.get());
        let document = match self.view_store.get(view) {
            Some(crate::tab_model::View::Native(native)) => {
                self.native_runtime.document_id(native.instance)
            }
            _ => None,
        };
        let queued = document
            .and_then(|document| self.native_documents.pending_closes.get(&document))
            .is_some_and(|closes| {
                closes
                    .iter()
                    .any(|close| close.views.contains(&document_view))
            });
        if queued {
            return format!("Closing {buffer} once its draft is saved");
        }
        let saving = document.is_some_and(|document| self.document_save_owed(document));
        if saving {
            format!("Saving {buffer} first; close it again once the save lands")
        } else {
            match title {
                Some(title) => format!("Buffer {title} stays open: {why}"),
                None => format!("This buffer stays open: {why}"),
            }
        }
    }

    fn kill_open_editor_buffer(&mut self, source_view: crate::tab_model::ViewId, query: &str) {
        let (_, _, _, target_view, title) =
            match self.resolve_open_editor_buffer(source_view, query) {
                Ok(found) => found,
                Err(status) => {
                    self.set_editor_view_status(source_view, &status);
                    return;
                }
            };
        if let Err(why) = self.close_editor_view(target_view) {
            let status = self.editor_close_status(target_view, Some(&title), &why);
            self.set_editor_view_status(source_view, &status);
        } else {
            self.set_editor_view_status(source_view, &format!("Killed buffer {title}"));
        }
        if target_view == source_view {
            return;
        }
        let source = self.windows.iter_mut().find_map(|(wid, ws)| {
            let tab = ws
                .tab_set
                .tabs()
                .iter()
                .find(|tab| tab.root.contains(source_view))?
                .id;
            (ws.tab_set.switch_to(tab)
                && ws
                    .tab_set
                    .active_mut()
                    .is_some_and(|active| active.set_focus(source_view)))
            .then_some(*wid)
        });
        if let Some(wid) = source {
            self.resync_active_or_window(wid);
            self.request_native_redraw(wid);
        }
    }

    fn switch_to_open_editor_buffer(&mut self, source_view: crate::tab_model::ViewId, query: &str) {
        let (wid, index, instance, target_view, title) =
            match self.resolve_open_editor_buffer(source_view, query) {
                Ok(found) => found,
                Err(status) => {
                    self.set_editor_view_status(source_view, &status);
                    return;
                }
            };
        let focused = self.windows.get_mut(&wid).is_some_and(|window| {
            let focused = window.tab_set.switch_to_index(index)
                && window
                    .tab_set
                    .active_mut()
                    .is_some_and(|tab| tab.set_focus(target_view));
            if focused {
                window.last_present = None;
            }
            focused
        });
        if !focused {
            self.set_editor_view_status(source_view, "The requested buffer could not take focus");
            return;
        }
        self.resync_active_or_window(wid);
        self.palette_sync_native_scope(wid);
        self.set_editor_view_status(target_view, &format!("Switched to buffer {title}"));
        self.refresh_native_presentation(wid, instance, target_view);
        self.request_native_redraw(wid);
    }

    fn resolve_open_editor_buffer(
        &self,
        source_view: crate::tab_model::ViewId,
        query: &str,
    ) -> Result<
        (
            WindowId,
            usize,
            crate::tab_model::AppInstanceId,
            crate::tab_model::ViewId,
            String,
        ),
        String,
    > {
        let source_window = self.windows.iter().find_map(|(wid, state)| {
            state
                .tab_set
                .tabs()
                .iter()
                .any(|tab| tab.root.contains(source_view))
                .then_some(*wid)
        });
        let Some(wid) = source_window else {
            return Err("Editor window is no longer available".to_string());
        };
        if query.is_empty() {
            return Err("Buffer name is required; no document changed".to_string());
        }

        let mut matches = Vec::new();
        if let Some(state) = self.windows.get(&wid) {
            for (index, tab) in state.tab_set.tabs().iter().enumerate() {
                tab.root.visit(&mut |&view| {
                    let Some(crate::tab_model::View::Native(native)) =
                        self.view_store.get(view).copied()
                    else {
                        return;
                    };
                    let Some(NativeApp::Editor(editor)) = self.native_runtime.app(native.instance)
                    else {
                        return;
                    };
                    let Some(uri) = self.document_store.canonical_uri(editor.document) else {
                        return;
                    };
                    if editor.title == query || uri == query {
                        matches.push((index, native.instance, view, editor.title.clone()));
                    }
                });
            }
        }
        let [(index, instance, target_view, title)] = matches.as_slice() else {
            return Err(if matches.is_empty() {
                let open_elsewhere = self.windows.iter().any(|(candidate_window, state)| {
                    *candidate_window != wid
                        && state.tab_set.tabs().iter().any(|tab| {
                            tab.root.any_leaf(&mut |view| {
                                let Some(crate::tab_model::View::Native(native)) =
                                    self.view_store.get(*view).copied()
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
                format!(
                    "Buffer name `{query}` is ambiguous; use a unique file URI or select the intended pane"
                )
            });
        };
        Ok((wid, *index, *instance, *target_view, title.clone()))
    }

    pub(crate) fn set_editor_view_status(&mut self, view: crate::tab_model::ViewId, message: &str) {
        if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) {
            state.status = Some(message.to_string());
        }
    }

    /// Re-observe one document through its existing file capability.
    pub(crate) fn refresh_native_document(
        &mut self,
        document: DocumentId,
    ) -> Result<crate::native_document_io::FileWatchReduction, String> {
        let observed = self
            .refresh_document_file(document)
            .map_err(|error| format!("document refresh failed: {error}"))?;
        self.reduce_document_observation(document, &observed.text, observed.observed)
    }

    fn refresh_document_file(
        &self,
        document: DocumentId,
    ) -> Result<crate::native_document_host::GrantedDocument, String> {
        let grant = self
            .native_documents
            .persistence
            .grant(document)
            .ok_or_else(|| "document persistence grant disappeared".to_string())?;
        let never_saved = self
            .native_documents
            .persistence
            .observed(document)
            .is_some_and(|observed| !observed.exists);
        let grants = &self.native_documents.grants;
        if never_saved {
            grants.refresh_local_allowing_missing(grant, DEFAULT_DOCUMENT_LIMIT)
        } else {
            grants.refresh_local(grant, DEFAULT_DOCUMENT_LIMIT)
        }
        .map_err(|error| error.to_string())
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
        let save_in_flight = self.native_documents.persistence.save_in_flight(document);
        let reduction = crate::native_document_io::reduce_file_watch(
            crate::native_document_io::FileWatchInput {
                baseline,
                observed,
                document_dirty: self.document_store.dirty(document).unwrap_or(false),
                save_in_flight,
            },
        );
        let reduction = if observed.exists
            && matches!(
                reduction,
                crate::native_document_io::FileWatchReduction::Unchanged
                    | crate::native_document_io::FileWatchReduction::RebindEquivalent { .. }
            )
            && (!self
                .native_documents
                .persistence
                .last_observed_text_matches(document, observed_text)
                || (save_in_flight
                    && !self
                        .native_documents
                        .persistence
                        .observed_text_matches(document, observed_text)))
        {
            let change = crate::native_document_io::VersionConflict::Content;
            if save_in_flight {
                crate::native_document_io::FileWatchReduction::DeferredSaving { change }
            } else if self.document_store.dirty(document).unwrap_or(false) {
                crate::native_document_io::FileWatchReduction::ConflictDirty { change }
            } else {
                crate::native_document_io::FileWatchReduction::ReloadClean { change }
            }
        } else {
            reduction
        };
        match reduction {
            crate::native_document_io::FileWatchReduction::Unchanged => {
                let cleared_conflict = self.native_documents.disk_conflicts.contains_key(&document);
                if cleared_conflict
                    || !self
                        .native_documents
                        .persistence
                        .observed_text_matches(document, observed_text)
                {
                    self.native_documents
                        .persistence
                        .accept_observation(document, observed, observed_text)
                        .map_err(|error| {
                            format!("could not clear resolved disk conflict: {error:?}")
                        })?;
                }
                if cleared_conflict {
                    self.set_document_disk_conflict(document, None);
                    self.set_document_recovery_status(document, None);
                    self.checkpoint_current_back_on_disk(document)?;
                    let status = if self.document_store.dirty(document).unwrap_or(false) {
                        "Disk conflict cleared · Save is available"
                    } else {
                        "Disk conflict cleared · content is unchanged"
                    };
                    for (_, _, view) in self.document_native_views(document) {
                        self.set_editor_view_status(view, status);
                    }
                }
            }
            crate::native_document_io::FileWatchReduction::RebindEquivalent { .. } => {
                self.native_documents
                    .persistence
                    .accept_observation(document, observed, observed_text)
                    .map_err(|error| {
                        format!("could not refresh equivalent disk baseline: {error:?}")
                    })?;
                let cleared_conflict = self.native_documents.disk_conflicts.contains_key(&document);
                self.set_document_disk_conflict(document, None);
                if cleared_conflict {
                    self.set_document_recovery_status(document, None);
                }
                self.checkpoint_current_back_on_disk(document)?;
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
                    crate::native_editor::Origin::Disk,
                )?;
            }
            crate::native_document_io::FileWatchReduction::ConflictDirty { change } => {
                self.set_document_disk_conflict(document, Some(DiskConflictCause::Version));
                let line = match change {
                    crate::native_document_io::VersionConflict::Existence => {
                        crate::native_app::DISK_EXISTENCE_RECOVERY
                    }
                    _ => crate::native_app::DISK_CONFLICT_RECOVERY,
                };
                self.set_document_recovery_status(document, Some(line.to_string()));
                self.note_disk_change(document);
            }
            crate::native_document_io::FileWatchReduction::DeferredSaving { .. } => {}
        }
        Ok(reduction)
    }

    pub(crate) fn discard_document_changes(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        if self.native_documents.persistence.save_in_flight(document) {
            return Err("cannot discard while a save is in flight".to_string());
        }
        let observed = self
            .refresh_document_file(document)
            .map_err(|error| format!("could not reload document for discard: {error}"))?;
        if matches!(
            self.document_store.phase(document),
            Some(
                crate::document_store::DocumentPhase::Closing { .. }
                    | crate::document_store::DocumentPhase::Blocked { .. }
            )
        ) {
            self.document_store
                .abort_close(document)
                .map_err(|error| format!("could not reopen document for discard: {error:?}"))?;
        }
        self.install_stable_file_observation(
            document,
            &observed.text,
            observed.observed,
            "Discarded changes and reloaded from disk",
            crate::native_editor::Origin::Revert,
        )?;
        // The buffer holds the file's text now, whoever edited it before; an edit no person
        // here is known to have made must not come back unmarked by C-/ or redo, so its undo
        // starts here.
        if self.native_documents.unvouched.remove(&document).is_some()
            && crate::owner_keys_save::discard_starts_history()
        {
            self.editor_workspace.start_history_here(document);
            self.native_runtime.set_editor_history_availability(
                document,
                self.editor_workspace.can_undo(document),
                self.editor_workspace.can_redo(document),
            );
        }
        self.set_editor_view_status(source_view, "Discarded changes and reloaded from disk");
        self.finish_pending_document_closes(document)?;
        // Discard is how a window close a failed save left waiting recovers: it
        // finishes now, not at some unrelated later save.
        if self
            .native_documents
            .pending_window_closes
            .values()
            .any(|pending| {
                pending
                    .plan
                    .documents
                    .iter()
                    .any(|item| item.document == document)
            })
        {
            self.native_documents.close_due = true;
        }
        Ok(())
    }

    fn install_stable_file_observation(
        &mut self,
        document: DocumentId,
        observed_text: &str,
        observed: crate::native_document_io::ObservedFileVersion,
        status: &str,
        origin: crate::native_editor::Origin,
    ) -> Result<(), String> {
        let before = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "document disappeared during refresh".to_string())?;
        // Only the spans that changed on disk: a caret, selection, mark or
        // scroll anchor outside them stays on its text.
        // One labelled frame through the editor's one writer, so C-/ takes
        // the disk's text back to the buffer's.
        let (seq, deltas) = if before.text.as_ref() == observed_text {
            (before.seq, Vec::new())
        } else {
            match self.editor_workspace.replace_text(
                &mut self.document_store,
                document,
                before.seq,
                observed_text,
                origin,
            ) {
                Ok(committed) => committed,
                Err(crate::native_editor::EditorError::TransactionConflict { .. }) => {
                    return Err("document changed while installing disk refresh".to_string());
                }
                Err(error) => {
                    return Err(format!("disk refresh was rejected: {error:?}"));
                }
            }
        };
        self.native_documents
            .persistence
            .accept_reloaded_observation(document, observed, observed_text)
            .map_err(|error| format!("could not accept refreshed file version: {error:?}"))?;
        self.document_store
            .checkpoint_ack(document, seq)
            .map_err(|error| format!("could not mark refreshed bytes durable: {error:?}"))?;

        let views = self.document_native_views(document);
        for (_, _, view) in &views {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(*view) {
                if let Some(buffer) = state.buffer.as_mut() {
                    buffer.observe_external(seq, &deltas);
                }
                if let Some(workspace) = state.markdown.as_mut() {
                    workspace.observe_document_edits(&deltas);
                }
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
        // The reload is an undo frame: Undo lights up for it.
        self.native_runtime.set_editor_history_availability(
            document,
            self.editor_workspace.can_undo(document),
            self.editor_workspace.can_redo(document),
        );
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
        self.set_document_disk_conflict(document, None);
        self.set_document_recovery_status(document, None);
        self.drive_document_journal(document)?;
        Ok(())
    }

    fn document_save_words(
        &self,
        document: DocumentId,
        failure: &crate::save_words::Failure<'_>,
    ) -> String {
        let path = self
            .native_documents
            .persistence
            .grant(document)
            .and_then(|grant| self.native_documents.grants.get(grant))
            .map(|grant| grant.target().logical_path().to_path_buf());
        format!(
            "Save failed: {}",
            crate::save_words::words(
                crate::save_words::Speaker::Aterm,
                path.as_deref()
                    .unwrap_or_else(|| std::path::Path::new("the file")),
                failure,
            )
        )
    }

    /// File > Save follows the active editor's own Save command.
    pub(crate) fn can_save_document(&self, wid: WindowId) -> bool {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        matches!(
            (
                self.native_runtime.app(instance),
                self.native_runtime.view_state(view)
            ),
            (Some(NativeApp::Editor(editor)), Some(AppViewState::Editor(state)))
                if editor.can_save(state)
        )
    }

    /// Why the save of `document` now is refused ([`crate::owner_keys_save`]): it is the
    /// aterm.toml in force, a control client, an accessibility client or another process's input
    /// made the save or an edit no person here is known to have made reached it, and the owner's
    /// keys in its text differ from the file's.
    fn owner_keys_save_refusal(&self, document: DocumentId) -> Option<String> {
        let provenance = crate::owner_keys_save::Provenance::new(
            self.gesture_unvouched().is_some(),
            self.native_documents.unvouched.get(&document).copied(),
        );
        if !provenance.fenced() {
            return None;
        }
        let host = &self.native_documents;
        let grant = host.grants.get(host.persistence.grant(document)?)?;
        let in_force =
            crate::owner_keys_save::in_force(self.native_config_service.bound_logical_path())?;
        if !crate::owner_keys_save::is_in_force(grant, &in_force) {
            return None;
        }
        let saved = self.document_store.snapshot(document)?.text;
        let disk =
            crate::native_config_service::VersionedConfigService::observe_path(&in_force, true)
                .map(|observation| observation.text);
        provenance.refusal(&saved, disk.as_deref().map_err(|_| ()))
    }

    pub(crate) fn save_front_document(&mut self) -> Result<(), String> {
        let wid = self
            .frontmost_window
            .filter(|&wid| self.can_save_document(wid))
            .ok_or_else(|| "Save is disabled for the active content".to_string())?;
        self.dispatch_native_event(wid, AppEvent::EditorCommand(EditorCommand::Save))
            .map(|_| ())
    }

    pub(crate) fn save_document_checkpoint(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        if let Some(message) = self.owner_keys_save_refusal(document) {
            aterm_log::info!(
                "editor: a save of aterm.toml refused: its owner keys are the owner's"
            );
            self.set_editor_view_status(source_view, &message);
            return Err(message);
        }
        if self.native_documents.disk_conflicts.contains_key(&document) {
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
                controller: crate::owner_keys_save::queued_by_controller(
                    self.gesture_unvouched().is_some(),
                ),
            };
            self.native_documents
                .pending_saves
                .entry(document)
                .and_modify(|pending| {
                    let controller = pending.controller && intent.controller;
                    if intent.seq >= pending.seq {
                        *pending = intent;
                    }
                    pending.controller = controller;
                })
                .or_insert(intent);
            return Ok(());
        }
        if let Some(proxy) = self.proxy.clone() {
            let queue = native_document_queue()?;
            let pending = self
                .native_documents
                .persistence
                .begin(&snapshot)
                .map_err(|error| {
                    self.document_save_words(document, &crate::save_words::Failure::Begin(&error))
                })?;
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
            if let Err(error) = try_enqueue_document_job(
                queue,
                NativeDocumentJob::Save {
                    document,
                    source_view,
                    grant,
                    plan: pending.plan.clone(),
                    config_themes,
                    proxy,
                },
            ) {
                let _ = self.native_documents.persistence.complete(
                    document,
                    pending.plan.generation,
                    crate::native_document_io::AtomicSaveResult::Cancelled,
                );
                return Err(error.to_string());
            }
            self.native_documents.inflight.insert(document);
            self.native_runtime.set_document_saving(document, true);
            return Ok(());
        }

        let pending = self
            .native_documents
            .persistence
            .begin(&snapshot)
            .map_err(|error| {
                self.document_save_words(document, &crate::save_words::Failure::Begin(&error))
            })?;
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

    /// A save's completion, from its worker (`Wake::NativeDocumentSaved`) or inline.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "SaveAck",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn finish_native_document_save(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        generation: crate::native_document_io::SaveGeneration,
        saved_bytes: std::sync::Arc<[u8]>,
        result: crate::native_document_io::AtomicSaveResult,
        config_observation: Option<
            Result<
                crate::native_config_service::PreparedConfigObservation,
                crate::native_config_service::ConfigFailure,
            >,
        >,
    ) -> Result<(), String> {
        let reduction = self
            .native_documents
            .persistence
            .complete(document, generation, result);
        if matches!(
            &reduction,
            Ok(crate::native_document_io::SaveReduction::Stale)
        ) {
            // An obsolete reply owns neither the current save nor its edit or
            // shutdown notifications. An error here would cancel a pending Quit.
            return Ok(());
        }
        let landed = self.land_native_document_save(
            document,
            source_view,
            saved_bytes,
            reduction,
            config_observation,
        );
        self.edit_document_saved(document, landed.as_ref().err().map(String::as_str));
        self.look_again_after_save(document);
        landed
    }

    fn land_native_document_save(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        saved_bytes: std::sync::Arc<[u8]>,
        reduction: Result<
            crate::native_document_io::SaveReduction,
            crate::native_document_host::PersistenceError,
        >,
        config_observation: Option<
            Result<
                crate::native_config_service::PreparedConfigObservation,
                crate::native_config_service::ConfigFailure,
            >,
        >,
    ) -> Result<(), String> {
        self.note_document_settling(document);
        self.native_documents.inflight.remove(&document);
        let reduction = match reduction {
            Ok(reduction) => reduction,
            Err(error) => {
                aterm_log::warn!("could not finish document save: {error:?}");
                let message = SAVE_UNCONFIRMED.to_string();
                let _ = self.document_store.checkpoint_fail(document);
                self.native_documents.pending_saves.remove(&document);
                self.native_runtime.set_document_saving(document, false);
                self.publish_editor_commit(document, source_view);
                self.mark_document_shutdown_failure(document, &message);
                return Err(message);
            }
        };
        let mut prepared_config = None;
        let stale = matches!(reduction, crate::native_document_io::SaveReduction::Stale);
        let outcome = match reduction {
            crate::native_document_io::SaveReduction::Durable(checkpoint) => {
                (|| -> Result<(), String> {
                    self.document_store
                        .checkpoint_ack(document, checkpoint.seq)
                        .map_err(|error| {
                            aterm_log::warn!("could not publish durable checkpoint: {error:?}");
                            SAVE_UNCONFIRMED.to_string()
                        })?;
                    let saved_text = std::str::from_utf8(&saved_bytes)
                        .map_err(|_| "saved document bytes were not UTF-8".to_string())?;
                    if let Some(observation) = config_observation {
                        match observation {
                            Ok(observation) => prepared_config = Some(observation),
                            Err(error) => {
                                self.native_config_service.mark_reconciliation_required();
                                aterm_log::warn!(
                                    "manual config save was durable but its prepared config generation failed: {}",
                                    error.log
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
                let cleared_conflict = self.native_documents.disk_conflicts.contains_key(&document);
                self.set_document_disk_conflict(document, None);
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
                self.set_document_disk_conflict(document, Some(DiskConflictCause::Version));
                aterm_log::warn!("document save refused: file changed on disk ({conflict:?})");
                let message = match conflict {
                    crate::native_document_io::VersionConflict::Existence => {
                        crate::native_app::DISK_EXISTENCE_RECOVERY
                    }
                    _ => crate::native_app::DISK_CONFLICT_RECOVERY,
                }
                .to_string();
                self.set_document_recovery_status(document, Some(message.clone()));
                self.note_disk_change(document);
                Err(message)
            }
            crate::native_document_io::SaveReduction::Failed {
                stage,
                message,
                cause,
            } => {
                let _ = self.document_store.checkpoint_fail(document);
                aterm_log::warn!("document save failed at {stage:?}: {message}");
                Err(self.document_save_words(
                    document,
                    &crate::save_words::Failure::Write { stage, cause },
                ))
            }
            crate::native_document_io::SaveReduction::ReconcileRequired {
                stage,
                observed,
                message,
                ..
            } => {
                let _ = self.document_store.checkpoint_fail(document);
                self.set_document_disk_conflict(document, Some(DiskConflictCause::SaveUnconfirmed));
                aterm_log::warn!(
                    "document save indeterminate at {stage:?}: {message} (observed: {observed:?})"
                );
                let encoding = self
                    .native_documents
                    .persistence
                    .observed(document)
                    .map_or(crate::native_document_io::DiskEncoding::PLAIN, |current| {
                        current.encoding()
                    });
                let written = std::str::from_utf8(&saved_bytes).map_or_else(
                    |_| saved_bytes.to_vec(),
                    |text| crate::native_document_host::encode_for_disk(text, encoding),
                );
                let landed = observed.is_some_and(|observed| {
                    observed.exists
                        && observed.content
                            == crate::native_document_io::ContentFingerprint::of(&written)
                });
                let recovery = if landed {
                    "Saved, but the disk did not confirm it; Save is blocked. Copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’"
                } else {
                    "Save may not have reached the disk; Save is blocked. Copy any local edits you need to keep, then choose ‘Discard Changes and Reload from Disk’"
                }
                .to_string();
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
                // generation and the later completion owns final publication. The
                // replay is made for whoever asked, a control client included.
                let replayed = self.with_controller_mark(intent.controller, |app| {
                    app.save_document_checkpoint(document, intent.source_view)
                });
                if let Err(message) = replayed {
                    self.native_runtime.set_document_saving(document, false);
                    self.publish_editor_commit(document, source_view);
                    self.mark_document_shutdown_failure(document, &message);
                    return Err(message);
                }
                return Ok(());
            }
        }

        if outcome.is_err() {
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
            if !stale {
                self.mark_document_shutdown_failure(document, message);
            }
        } else {
            self.mark_document_save_success(document);
            self.finish_pending_document_closes(document)?;
        }
        if let Some(prepared) = prepared_config {
            self.admit_manual_config_observation(prepared);
        }
        outcome
    }

    /// Whole-tab multi-document barrier.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "RequestClose",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
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
        tab.root.visit(&mut |&view| {
            let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
            else {
                return;
            };
            let Some(document) = self.native_runtime.document_id(native.instance) else {
                return;
            };
            let entry = grouped
                .entry(document)
                .or_insert_with(|| (BTreeSet::new(), view));
            entry.0.insert(DocumentViewId(view.get()));
        });
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
            // A real worker completes after this request returns. Retain the whole-tab
            // intent on every member; the next completion revalidates the atomic plan.
            for item in &plan.documents {
                self.queue_pending_document_close(item.document, tab_id, item.views.clone(), true);
            }
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
        tab: crate::tab_model::TabId,
        views: Vec<DocumentViewId>,
        whole_tab: bool,
    ) {
        let pending = PendingDocumentClose {
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

    /// Per-leaf variant of the document close barrier.
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
                self.queue_pending_document_close(document, tab_id, closing, false);
                self.start_close_checkpoint(document, source_view)?;
                Ok(false)
            }
            crate::document_store::DocumentCloseReadiness::Pending { .. } => {
                self.queue_pending_document_close(document, tab_id, closing, false);
                self.start_close_checkpoint(document, source_view)?;
                Ok(false)
            }
        }
    }

    /// Start the save a view close waits on. A save that could not start leaves no work running
    /// for the close to wait on, so its checkpoint is failed here: the document stands Blocked,
    /// its tab says why, and an update names the person's unsaved text instead of waiting on a
    /// checkpoint nothing is writing.
    fn start_close_checkpoint(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
        self.save_document_checkpoint(document, source_view)
            .inspect_err(|message| self.mark_document_shutdown_failure(document, message))
    }

    /// Commit the open grid cell editor of Editor view `view`, if it has one, before the view
    /// closes, as Return would. `false` when the commit is refused, its reason on the view's
    /// status: the close waits and the person's text stays.
    pub(crate) fn commit_grid_draft_before_close(
        &mut self,
        view: crate::tab_model::ViewId,
    ) -> bool {
        let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
        else {
            return true;
        };
        let Some(document) = self.native_runtime.document_id(native.instance) else {
            return true;
        };
        let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) else {
            return true;
        };
        let Some(buffer) = state.buffer.as_mut() else {
            return true;
        };
        if buffer.grid_draft().is_none() {
            return true;
        }
        let committed = self
            .editor_workspace
            .commit_before_close(&mut self.document_store, buffer);
        state.common.presentation_revision = state.common.presentation_revision.saturating_add(1);
        if let Err(refusal) = &committed {
            state.status = Some(format!("The cell stays open: {refusal}"));
        }
        self.native_runtime.set_editor_history_availability(
            document,
            self.editor_workspace.can_undo(document),
            self.editor_workspace.can_redo(document),
        );
        self.publish_editor_commit(document, view);
        committed.is_ok()
    }

    /// The window tab `tab` is in now, if it is still open.
    pub(crate) fn tab_window(&self, tab: crate::tab_model::TabId) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, state)| state.tab_set.get(tab).is_some())
            .map(|(window, _)| *window)
    }

    fn finish_pending_document_closes(&mut self, document: DocumentId) -> Result<(), String> {
        let pending = self
            .native_documents
            .pending_closes
            .remove(&document)
            .unwrap_or_default();
        let mut deferred = Vec::new();
        let mut dropped = false;
        for close in pending {
            let window = self.tab_window(close.tab);
            // A pending window or Quit plan owns teardown atomically; detaching
            // an older tab close here would apply part of it before its other
            // documents are durable. The window is wherever the tab is now.
            if self.native_documents.pending_quit.is_some()
                || window.is_some_and(|window| {
                    self.native_documents
                        .pending_window_closes
                        .contains_key(&window)
                })
            {
                deferred.push(close);
                continue;
            }
            let unchanged = window
                .and_then(|window| self.windows.get(&window))
                .and_then(|ws| ws.tab_set.get(close.tab))
                .is_some_and(|tab| {
                    close
                        .views
                        .iter()
                        .all(|view| tab.root.any_leaf(&mut |&live| live.get() == view.0))
                });
            let Some(window) = window.filter(|_| unchanged) else {
                // The tab or its leaf is gone: nothing is left to close, and the
                // document must not stay frozen for it.
                dropped = true;
                continue;
            };
            let mutations: Vec<_> = if close.whole_tab {
                vec![crate::DeferredHandoffMutation::CloseTab {
                    window,
                    tab: close.tab,
                }]
            } else {
                close
                    .views
                    .iter()
                    .map(|view| crate::DeferredHandoffMutation::CloseView {
                        window,
                        tab: close.tab,
                        view: crate::tab_model::ViewId::from_stored(view.0),
                    })
                    .collect()
            };
            if self.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::mutations(mutations),
            ) {
                // The handoff now owns the stable close intent and its replay.
                continue;
            }
            let replay_tab = self.windows.get(&window).and_then(|window| {
                window
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.id == close.tab && (close.whole_tab || tab.root.len() == 1))
                    .map(|index| (index, window.tab_set.active_id()))
            });
            if let Some((index, active)) = replay_tab {
                // A whole tab (or a leaf whose sibling closed while saving) must use
                // the full close barrier again. One saved member is no authority to
                // detach another member whose checkpoint is still outstanding.
                self.close_tab_at(window, index);
                if self
                    .windows
                    .get(&window)
                    .is_some_and(|window| window.tab_set.get(close.tab).is_some())
                {
                    if let Some(active) = active
                        && let Some(window) = self.windows.get_mut(&window)
                    {
                        window.tab_set.switch_to(active);
                    }
                    self.resync_active_or_window(window);
                    deferred.push(close);
                } else {
                    self.native_documents.pending_closes.retain(|_, closes| {
                        closes.retain(|pending| pending.tab != close.tab);
                        !closes.is_empty()
                    });
                }
                continue;
            }
            self.document_store
                .commit_detach(document, &close.views)
                .map_err(|error| format!("could not finish document close: {error:?}"))?;
            for document_view in close.views {
                let view = crate::tab_model::ViewId::from_stored(document_view.0);
                let recovery = self.closed_view_record_for(window, close.tab, view);
                let removed = self
                    .windows
                    .get_mut(&window)
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
                    if let Some(recovery) = recovery {
                        self.retain_closed_view(recovery);
                    }
                }
            }
            self.refresh_aggregate_tab_presentation(window, close.tab);
            self.promote_terminal_projection_if_needed(window, close.tab);
            self.resize_panes_after_collapse(window);
            self.resync_active_or_window(window);
            self.request_native_redraw(window);
        }
        for close in deferred {
            if self.tab_window(close.tab).is_none() {
                dropped = true;
                continue;
            }
            let retained = self
                .native_documents
                .pending_closes
                .entry(document)
                .or_default();
            if !retained.contains(&close) {
                retained.push(close);
            }
        }
        if dropped {
            self.release_unprotected_documents(BTreeSet::from([document]));
        }
        Ok(())
    }

    /// Arm (or re-drive) the all-or-nothing document barrier for one OS window.
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
        let plan = self.document_close_plan_for_window(window)?;
        let asked = self.window_close_consent(window);
        if let Some(previous) = self.native_documents.pending_window_closes.remove(&window) {
            // A repeated close re-plans from the window's tabs. A document the
            // replaced plan froze that the new one does not close the same way
            // (it moved to another window, or a leaf of it closed) is reopened
            // first; the new plan freezes again whatever it still closes.
            let released = previous
                .plan
                .documents
                .into_iter()
                .filter(|item| {
                    !plan
                        .documents
                        .iter()
                        .any(|kept| kept.document == item.document && kept.views == item.views)
                })
                .map(|item| item.document)
                .collect();
            self.release_unprotected_documents(released);
        }
        self.native_documents.pending_window_closes.insert(
            window,
            PendingWindowClose {
                plan: plan.clone(),
                asked,
                deferred: false,
            },
        );
        let ready = self.drive_document_close_plan(&plan, start_saves)?;
        if ready {
            self.commit_pending_window_document_shutdown(window)?;
        }
        Ok(ready)
    }

    /// Arm the process-wide clean-Quit barrier.
    pub(crate) fn prepare_quit_document_shutdown(&mut self) -> Result<bool, String> {
        self.prepare_quit_document_shutdown_inner(true)
    }

    /// A failed Quit save is retryable, not a latent exit request.
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

    /// Disarm a failed whole-app Quit while preserving any independently pending window close.
    pub(crate) fn cancel_failed_quit_document_shutdown(&mut self) -> bool {
        let Some(plan) = self.native_documents.pending_quit.take() else {
            return false;
        };
        let protected = self
            .native_documents
            .pending_window_closes
            .values()
            .flat_map(|pending| pending.plan.documents.iter().map(|item| item.document))
            .collect::<BTreeSet<_>>();
        for item in plan.documents {
            if !protected.contains(&item.document) {
                let _ = self.document_store.abort_close(item.document);
            }
        }
        self.native_documents.closes_resume_due = true;
        true
    }

    fn prepare_quit_document_shutdown_inner(&mut self, start_saves: bool) -> Result<bool, String> {
        let plan = self.document_close_plan_for_quit();
        self.native_documents.pending_quit = Some(plan.clone());
        let ready = self.drive_document_close_plan(&plan, start_saves)?;
        if ready {
            self.native_documents.pending_quit = None;
        }
        Ok(ready)
    }

    /// Called after an async save reduction.
    pub(crate) fn take_ready_document_shutdowns(
        &mut self,
    ) -> Result<(bool, Vec<WindowId>), String> {
        if let Some(plan) = self.native_documents.pending_quit.clone() {
            let undriven = self.undriven_documents(&plan);
            if !undriven.documents.is_empty() {
                // A document the Quit found needing no save may need one now; the
                // Quit saves it as it saved every other document.
                self.drive_document_close_plan(&undriven, true)?;
            }
            if self.document_close_plan_ready(&plan)? {
                // The native-app preflight ran before the async checkpoint; a
                // Settings draft (or another native blocker) may have appeared
                // since, so re-ask at the irreversible exit boundary.
                if !self.prepare_quit_native_shutdown()? {
                    return Ok((false, Vec::new()));
                }
                let fresh = self.document_close_plan_for_quit();
                if fresh != plan {
                    self.native_documents.pending_quit = Some(fresh.clone());
                    if !self.drive_document_close_plan(&fresh, true)? {
                        return Ok((false, Vec::new()));
                    }
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
            // One window's broken plan costs only that window's close. Each
            // close is weighed with the ones this pass already committed gone
            // (they close before anything is asked again) and the ones a
            // pending update handoff holds (they close when it replays).
            let gone = ready
                .iter()
                .copied()
                .chain(self.windows_closing_behind_handoff())
                .collect::<Vec<_>>();
            match self.take_ready_window_document_shutdown(window, &gone) {
                Ok(true) => ready.push(window),
                Ok(false) => {}
                Err(error) => {
                    aterm_log::warn!(
                        "window {} document close dropped: {error}; close it again",
                        window.0
                    );
                    self.disarm_window_document_shutdown(window);
                }
            }
        }
        Ok((false, ready))
    }

    /// One armed window close at an escalation, with the windows in `gone`
    /// committed to close before it; `Ok(true)` once it committed and the
    /// window may go.
    fn take_ready_window_document_shutdown(
        &mut self,
        window: WindowId,
        gone: &[WindowId],
    ) -> Result<bool, String> {
        let Some(armed) = self
            .native_documents
            .pending_window_closes
            .get(&window)
            .cloned()
        else {
            return Ok(false);
        };
        if !self.windows.contains_key(&window) {
            self.disarm_window_document_shutdown(window);
            return Ok(false);
        }
        // Readiness is asked of the armed leaves only while they are all still
        // in this window: `prepare_close` freezes a document, and one that moved
        // to another window, or a leaf that closed, is no longer this close's.
        // Such a close no longer describes the window, so it is dropped.
        if !armed
            .plan
            .within(&self.document_close_plan_for_window(window)?)
        {
            self.disarm_window_document_shutdown(window);
            return Ok(false);
        }
        if !self.document_close_plan_ready(&armed.plan)? {
            // A document that needed no save when the close was armed (another
            // window held a view of it) may need one now. The close saves it, as
            // it saved the others, unless the window no longer agrees to close.
            let undriven = self.undriven_documents(&armed.plan);
            if !undriven.documents.is_empty() {
                if !self.window_close_consent_holds(window, &armed.asked, gone) {
                    self.disarm_window_document_shutdown(window);
                    return Ok(false);
                }
                self.drive_document_close_plan(&undriven, true)?;
            }
            return Ok(false);
        }
        // A close a failed save left armed can come ready long after it was
        // asked. It ends only what the person agreed to end then: a terminal
        // the window gained since, a job that was not running then, or the app
        // (the window became the last one, counting the closes this pass
        // committed) drops it, and the person closes the window again.
        if !self.window_close_consent_holds(window, &armed.asked, gone) {
            aterm_log::info!(
                "window {} changed since its close was asked (a terminal, a running job, or \
                 now the last window); the close is dropped",
                window.0
            );
            self.disarm_window_document_shutdown(window);
            return Ok(false);
        }
        // Detach nothing until every native leaf is still ready: the UI stays
        // live while checkpoints run, and a new Settings draft must keep the
        // whole window and surface its recovery payload.
        if !self.prepare_window_native_shutdown(window, crate::native_app::CloseScope::Window)? {
            return Ok(false);
        }
        let fresh = self.document_close_plan_for_window(window)?;
        if fresh != armed.plan {
            if let Some(pending) = self.native_documents.pending_window_closes.get_mut(&window) {
                pending.plan = fresh.clone();
            }
            if !self.drive_document_close_plan(&fresh, true)? {
                return Ok(false);
            }
        }
        // An update handoff in flight takes the window close and replays it
        // (`close_window`) once it is over. Detach nothing before that: the
        // replay re-plans from the window's leaves, which must still be the
        // documents'. The replay weighs the consent again first.
        if let Some(pending) = self.native_documents.pending_window_closes.get_mut(&window) {
            pending.deferred = true;
        }
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseWindow(window),
        )) {
            return Ok(false);
        }
        if let Some(pending) = self.native_documents.pending_window_closes.get_mut(&window) {
            pending.deferred = false;
        }
        self.commit_pending_window_document_shutdown(window)?;
        Ok(true)
    }

    /// Whether a returned update handoff may replay `window`'s close: a close
    /// it took from an armed plan ends only what the person agreed to end when
    /// the close was asked, weighed now that the closes replayed before it are
    /// done. A close that no longer holds is dropped, and the window stays.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn replayed_window_close_consented(&mut self, window: WindowId) -> bool {
        let Some(asked) = self
            .native_documents
            .pending_window_closes
            .get(&window)
            .filter(|pending| pending.deferred)
            .map(|pending| pending.asked.clone())
        else {
            return true;
        };
        if self.window_close_consent_holds(window, &asked, &[]) {
            return true;
        }
        aterm_log::info!(
            "window {} changed while an update handoff held its close (a terminal, a running \
             job, or now the last window); the close is dropped",
            window.0
        );
        self.disarm_window_document_shutdown(window);
        false
    }

    /// A returned update handoff hands back `teardown`: each armed close it had
    /// taken that `teardown` no longer holds (a Quit or an AppKit terminate that
    /// arrived during the handoff replaced it) is disarmed, so its window stays
    /// with its documents editable instead of closing at some later, unrelated
    /// save.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn release_window_closes_the_teardown_dropped(
        &mut self,
        teardown: &crate::DeferredHandoffTeardown,
    ) {
        let dropped: Vec<WindowId> = self
            .native_documents
            .pending_window_closes
            .iter()
            .filter(|(_, pending)| pending.deferred)
            .map(|(window, _)| *window)
            .filter(|window| {
                !matches!(
                    teardown,
                    crate::DeferredHandoffTeardown::Mutations(mutations)
                        if mutations.contains_key(&crate::DeferredHandoffMutation::CloseWindow(*window))
                )
            })
            .collect();
        for window in dropped {
            aterm_log::info!(
                "window {}'s close was replaced by a quit while an update handoff held it; \
                 the close is dropped",
                window.0
            );
            self.disarm_window_document_shutdown(window);
        }
    }

    /// Drop `window`'s armed close, if any, with the tab closes in it that the
    /// close deferred (a commit would have subsumed them; left behind, one
    /// would close its tab at some later save), and reopen for editing each
    /// document of either that no other close still waits on.
    pub(crate) fn disarm_window_document_shutdown(&mut self, window: WindowId) {
        let Some(pending) = self.native_documents.pending_window_closes.remove(&window) else {
            return;
        };
        let mut released = self.forget_window_tab_closes(window);
        released.extend(pending.plan.documents.iter().map(|item| item.document));
        self.release_unprotected_documents(released);
        self.native_documents.closes_resume_due = true;
    }

    /// Reopen for editing each of `documents` that no armed close (a window's,
    /// the Quit's, or a tab's) still waits on.
    fn release_unprotected_documents(&mut self, documents: BTreeSet<DocumentId>) {
        let host = &self.native_documents;
        let protected = host
            .pending_window_closes
            .values()
            .map(|other| &other.plan)
            .chain(host.pending_quit.as_ref())
            .flat_map(|plan| plan.documents.iter().map(|item| item.document))
            .chain(
                host.pending_closes
                    .iter()
                    .filter(|(_, closes)| {
                        closes
                            .iter()
                            .any(|close| self.tab_window(close.tab).is_some())
                    })
                    .map(|(document, _)| *document),
            )
            .collect::<BTreeSet<_>>();
        for document in documents.difference(&protected) {
            let _ = self.document_store.abort_close(*document);
        }
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
        for tab in state.tab_set.tabs() {
            tab.root.visit(&mut |&view| {
                let Some(crate::tab_model::View::Native(native)) =
                    self.view_store.get(view).copied()
                else {
                    return;
                };
                let Some(document) = self.native_runtime.document_id(native.instance) else {
                    return;
                };
                let entry = grouped
                    .entry(document)
                    .or_insert_with(|| (BTreeSet::new(), view));
                entry.0.insert(DocumentViewId(view.get()));
            });
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

    /// Whether every document of `plan` can detach now. Asking freezes nothing:
    /// a document only a close that saves it may freeze.
    fn document_close_plan_ready(&self, plan: &DocumentClosePlan) -> Result<bool, String> {
        for item in &plan.documents {
            if !self
                .document_store
                .close_ready(item.document, &item.views)
                .map_err(|error| format!("could not verify document shutdown: {error:?}"))?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The documents of `plan` that owe a save no close has started: each
    /// needed none when the plan was armed (another view kept it open) and is
    /// still editable.
    fn undriven_documents(&self, plan: &DocumentClosePlan) -> DocumentClosePlan {
        DocumentClosePlan {
            documents: plan
                .documents
                .iter()
                .filter(|item| {
                    matches!(
                        self.document_store.phase(item.document),
                        Some(
                            crate::document_store::DocumentPhase::Active
                                | crate::document_store::DocumentPhase::Suspended
                        )
                    ) && self
                        .document_store
                        .close_ready(item.document, &item.views)
                        .is_ok_and(|ready| !ready)
                })
                .cloned()
                .collect(),
        }
    }

    fn commit_pending_window_document_shutdown(&mut self, window: WindowId) -> Result<(), String> {
        let plan = self
            .native_documents
            .pending_window_closes
            .get(&window)
            .map(|pending| pending.plan.clone())
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
        let forgotten = self.forget_window_tab_closes(window);
        self.release_unprotected_documents(forgotten);
        self.native_documents.closes_resume_due = true;
        Ok(())
    }

    /// Forget the tab and leaf closes in `window` that its window close
    /// subsumed (and any whose tab is gone), answering the documents they
    /// were for. A close whose tab moved to another window is that window's.
    fn forget_window_tab_closes(&mut self, window: WindowId) -> BTreeSet<DocumentId> {
        let held = self
            .native_documents
            .pending_closes
            .values()
            .flatten()
            .map(|close| (close.tab, self.tab_window(close.tab)))
            .collect::<BTreeMap<_, _>>();
        let mut forgotten = BTreeSet::new();
        for (document, closes) in &mut self.native_documents.pending_closes {
            let before = closes.len();
            closes.retain(|close| {
                held.get(&close.tab)
                    .copied()
                    .flatten()
                    .is_some_and(|holder| holder != window)
            });
            if closes.len() != before {
                forgotten.insert(*document);
            }
        }
        self.native_documents
            .pending_closes
            .retain(|_, closes| !closes.is_empty());
        forgotten
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
                    .position(|tab| tab.root.contains(view))?;
                state.tab_set.tab_at_mut(index)
            }) {
                tab.presentation.indicators.attention = true;
                tab.presentation.tooltip = Some(message.to_string());
            }
            self.request_native_redraw(window);
        }
    }

    fn mark_document_save_success(&mut self, document: DocumentId) {
        self.set_document_disk_conflict(document, None);
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
            let edit_mark = self.edit_attention_in_tab_of(window, view);
            if let Some(tab) = self.windows.get_mut(&window).and_then(|state| {
                let index = state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.root.contains(view))?;
                state.tab_set.tab_at_mut(index)
            }) {
                tab.presentation.indicators.attention = edit_mark;
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
        self.native_views_where(|instance| {
            self.native_runtime.document_id(instance) == Some(document)
        })
    }

    fn native_views_where(
        &self,
        keep: impl Fn(crate::tab_model::AppInstanceId) -> bool,
    ) -> Vec<(
        WindowId,
        crate::tab_model::AppInstanceId,
        crate::tab_model::ViewId,
    )> {
        let mut views = Vec::new();
        for (&wid, ws) in &self.windows {
            for tab in ws.tab_set.tabs() {
                tab.root.visit(&mut |&view| {
                    let Some(crate::tab_model::View::Native(native)) =
                        self.view_store.get(view).copied()
                    else {
                        return;
                    };
                    if keep(native.instance) {
                        views.push((wid, native.instance, view));
                    }
                });
            }
        }
        views
    }

    /// At a park: let go of the documents nothing holds any more when a look is due.
    pub(crate) fn release_documents_at_park(&mut self) {
        if std::mem::take(&mut self.native_documents.closes_resume_due) {
            self.resume_released_document_closes();
        }
        self.sweep_orphaned_edits_at_park();
        let host = &self.native_documents;
        if host.release_due || host.release_deadline_ms.is_some() {
            let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
            self.release_documents_due(now_ms);
        }
    }

    /// Finish each queued tab or leaf close that a window or Quit close held
    /// and no longer holds (it went, or the tab moved out of its window),
    /// once its document is saved; one still saving finishes when its save
    /// lands.
    fn resume_released_document_closes(&mut self) {
        let saved = self
            .native_documents
            .pending_closes
            .keys()
            .copied()
            .filter(|document| self.document_store.dirty(*document) == Some(false))
            .collect::<Vec<_>>();
        for document in saved {
            if let Err(error) = self.finish_pending_document_closes(document) {
                aterm_log::warn!("a held document close could not finish: {error}");
            }
        }
    }

    fn release_documents_due(&mut self, now_ms: u64) -> usize {
        let host = &self.native_documents;
        if host.release_due
            || host
                .release_deadline_ms
                .is_some_and(|deadline| now_ms >= deadline)
        {
            self.release_unheld_documents(now_ms)
        } else {
            0
        }
    }

    /// Let go of every document nothing holds any more (§5.10).
    pub(crate) fn release_unheld_documents(&mut self, now_ms: u64) -> usize {
        if self.native_documents.pending_quit.is_some()
            || !self.native_documents.pending_window_closes.is_empty()
        {
            return 0;
        }
        let reopenable = self.closed_recovery.document_uris(now_ms);
        let mut deadline: Option<u64> = None;
        let mut released = Vec::new();
        let mut private_released = false;
        for document in self.document_store.document_ids() {
            if !self.document_unheld(document) {
                continue;
            }
            let uri = self.document_store.canonical_uri(document);
            private_released |= uri.is_some_and(crate::native_document_journal::private_uri);
            // A private edit's document is never kept for a closed tab's reopening.
            let kept_until = reopenable
                .iter()
                .filter(|(named, _)| Some(*named) == uri)
                .filter(|(named, _)| !crate::native_document_journal::private_uri(named))
                .map(|(_, expires_ms)| *expires_ms)
                .max();
            match kept_until {
                Some(expires_ms) => {
                    deadline = Some(deadline.map_or(expires_ms, |soonest| soonest.min(expires_ms)));
                }
                None => released.push(document),
            }
        }
        self.native_documents.release_due = false;
        self.native_documents.release_deadline_ms = deadline;
        released.retain(|document| self.document_store.release(*document).is_ok());
        if private_released {
            sweep_private_scratch();
        }
        let host = &mut self.native_documents;
        for document in &released {
            if let Some(grant) = host.persistence.forget(*document) {
                host.grants.forget(grant);
            }
            host.recovery_status.remove(document);
            host.disk_conflicts.remove(document);
            host.unvouched.remove(document);
            host.journal_retries.remove(*document);
            host.watch.unwatch(*document);
            self.editor_workspace.forget(*document);
        }
        if let Some(journals) = host.journals.as_mut() {
            journals.forget(&released);
        }
        released.len()
    }

    fn document_unheld(&self, document: DocumentId) -> bool {
        let host = &self.native_documents;
        self.document_store.phase(document) == Some(crate::document_store::DocumentPhase::Suspended)
            && self.document_store.dirty(document) == Some(false)
            && !self.native_runtime.holds_document(document)
            && !host.persistence.save_in_flight(document)
            && !host.inflight.contains(&document)
            && !host.pending_saves.contains_key(&document)
            && !host.pending_closes.contains_key(&document)
            && host
                .journals
                .as_ref()
                .is_none_or(|journals| journals.settled(document))
    }

    fn note_document_settling(&mut self, document: DocumentId) {
        if self.document_store.phase(document)
            == Some(crate::document_store::DocumentPhase::Suspended)
        {
            self.native_documents.release_due = true;
        }
    }

    /// Recompute duplicate-basename labels and publish every affected tab, including inactive tabs and views in other windows.
    pub(crate) fn refresh_disambiguated_document_titles(&mut self) {
        let changed = self
            .native_runtime
            .disambiguate_document_titles()
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if changed.is_empty() {
            return;
        }
        for (window, instance, view) in
            self.native_views_where(|instance| changed.contains(&instance))
        {
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
    }

    /// Open a local document in window `wid` with its caret on `line` (and `col`).
    pub(crate) fn open_document_tab_at(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
        line: Option<u32>,
        col: Option<u32>,
    ) -> Result<String, String> {
        self.open_document_tab_revealing(
            wid,
            kind,
            uri,
            line.map(|line| DocumentReveal::Line(LineReveal { line, col })),
        )
    }

    /// Resolve only the clicked Markdown document, then use the same bounded
    /// admission path as File Open. A fragment is applied to the live buffer.
    pub(crate) fn open_markdown_link(
        &mut self,
        wid: WindowId,
        source: DocumentId,
        destination: &str,
    ) -> Result<(), String> {
        let source_uri = self
            .document_store
            .canonical_uri(source)
            .ok_or_else(|| "the Markdown link's source document is gone".to_string())?;
        let (uri, fragment) = crate::native_app::resolve_markdown_link(source_uri, destination)?;
        self.open_document_tab_revealing(
            wid,
            AppKind::Markdown,
            &uri,
            fragment.map(DocumentReveal::MarkdownFragment),
        )
        .map(|_| ())
    }

    fn open_document_tab_revealing(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
        reveal: Option<DocumentReveal>,
    ) -> Result<String, String> {
        let slot = (
            wid,
            DocumentAdmissionKey {
                uri: uri.to_string(),
                kind,
                edit: None,
            },
        );
        match reveal {
            Some(reveal) => {
                self.native_documents.reveals.insert(slot.clone(), reveal);
            }
            None => {
                self.native_documents.reveals.remove(&slot);
            }
        }
        let admission = self.request_document_tab_in_window(wid, kind, uri);
        match &admission {
            Ok(DocumentAdmission::Opened(_)) => {
                if let Some(reveal) = self.native_documents.reveals.remove(&slot) {
                    self.reveal_opened_document(wid, reveal)?;
                }
            }
            Ok(DocumentAdmission::InFlight(_) | DocumentAdmission::Coalesced(_)) => {}
            Err(_) => {
                self.native_documents.reveals.remove(&slot);
            }
        }
        admission.map(|admission| admission.message(&slot.1))
    }

    fn reveal_opened_document(
        &mut self,
        wid: WindowId,
        reveal: DocumentReveal,
    ) -> Result<(), String> {
        let fragment = match reveal {
            DocumentReveal::Line(line) => return self.reveal_document_line(wid, line),
            DocumentReveal::MarkdownFragment(fragment) => fragment,
        };
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "the linked document is not focused".to_string())?;
        let document = self
            .native_runtime
            .document_id(instance)
            .ok_or_else(|| "the linked document is gone".to_string())?;
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "the linked document buffer is gone".to_string())?;
        let offset = if fragment.is_empty() {
            0
        } else {
            let parsed = crate::native_markdown::parse(&snapshot.text);
            crate::native_markdown::local_anchor(&parsed, &format!("#{fragment}"))
                .ok_or_else(|| format!("the linked heading #{fragment} was not found"))?
        };
        let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view) else {
            return Err("the linked document is not an editor workspace".into());
        };
        let workspace = state
            .markdown
            .as_mut()
            .ok_or_else(|| "the linked document does not support Markdown reading".to_string())?;
        if workspace.editor_focused() {
            let buffer = state
                .buffer
                .as_mut()
                .ok_or_else(|| "the linked editor buffer is gone".to_string())?;
            if !state.preedit.is_empty() || buffer.minibuffer_active() {
                return Err("finish the source input before following a heading".into());
            }
            buffer.selections = vec![crate::native_editor::Selection::caret(offset)];
            buffer.primary = 0;
            buffer.ensure_primary_visible(&snapshot.text, buffer.viewport_lines());
        }
        if workspace.reader.history.current().is_none() {
            workspace
                .reader
                .history
                .visit(crate::native_markdown::MarkdownLocation::new(
                    workspace.reader.source_anchor,
                    workspace.reader.visual_row,
                ));
        }
        workspace.reader.source_anchor = offset;
        workspace.reader.visual_row = 0;
        workspace
            .reader
            .history
            .visit(crate::native_markdown::MarkdownLocation::new(offset, 0));
        state.common.retire_input();
        state.common.presentation_revision = state.common.presentation_revision.saturating_add(1);
        self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
        self.refresh_native_presentation(wid, instance, view);
        self.request_native_redraw(wid);
        Ok(())
    }

    /// Put the caret of `wid`'s focused editor on `reveal`'s line and column, scrolled into view.
    pub(crate) fn reveal_document_line(
        &mut self,
        wid: WindowId,
        reveal: LineReveal,
    ) -> Result<(), String> {
        let (_, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "the opened document is not focused".to_string())?;
        self.reveal_document_line_in_view(view, reveal)
    }

    /// [`Self::reveal_document_line`] for one exact view, focused or not.
    pub(crate) fn reveal_document_line_in_view(
        &mut self,
        view: crate::tab_model::ViewId,
        reveal: LineReveal,
    ) -> Result<(), String> {
        let Some(crate::tab_model::View::Native(native)) = self.view_store.get(view).copied()
        else {
            return Err("the opened document's view is gone".to_string());
        };
        let instance = native.instance;
        let document = self
            .native_runtime
            .document_id(instance)
            .ok_or_else(|| "the focused view holds no document".to_string())?;
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "the opened document disappeared".to_string())?;
        let offset = crate::path_link::line_col_offset(&snapshot.text, reveal.line, reveal.col);
        self.native_runtime
            .set_editor_markdown_mode(instance, view, MarkdownWorkspaceMode::Edit);
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return Ok(());
        };
        let buffer = state
            .buffer
            .as_mut()
            .ok_or_else(|| "the editor buffer disappeared".to_string())?;
        buffer.selections = vec![crate::native_editor::Selection::caret(offset)];
        buffer.primary = 0;
        buffer.clear_minibuffer();
        buffer.ensure_primary_visible(&snapshot.text, buffer.viewport_lines());
        state.status = Some(format!("Line {}", reveal.line));
        for (window, instance, view) in self.document_native_views(document) {
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
        Ok(())
    }

    /// Two-phase open of one local document in the frontmost window.
    pub(crate) fn request_document_tab(
        &mut self,
        kind: AppKind,
        uri: &str,
    ) -> Result<DocumentAdmission, String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no requesting window".to_string())?;
        self.request_document_tab_in_window(wid, kind, uri)
    }

    /// Two-phase open of one local document in one exact host window, the form every main-thread gesture uses.
    pub(crate) fn request_document_tab_in_window(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
    ) -> Result<DocumentAdmission, String> {
        Self::validate_document_request(kind, false)?;
        if !self.windows.contains_key(&wid) {
            return Err("requesting window disappeared".to_string());
        }
        self.begin_document_admission(
            wid,
            DocumentAdmissionKey {
                uri: uri.to_string(),
                kind,
                edit: None,
            },
        )
    }

    /// The two-phase open of an `edit` ticket's file in window `wid` ([`crate::app_edit`]).
    pub(crate) fn request_edit_admission(
        &mut self,
        wid: WindowId,
        uri: &str,
        ticket: crate::app_edit::EditTicketId,
    ) -> Result<DocumentAdmission, String> {
        Self::validate_document_request(AppKind::Editor, false)?;
        if !self.windows.contains_key(&wid) {
            return Err("requesting window disappeared".to_string());
        }
        self.begin_document_admission(
            wid,
            DocumentAdmissionKey {
                uri: uri.to_string(),
                kind: AppKind::Editor,
                edit: Some(ticket),
            },
        )
    }

    /// Synchronous open in the frontmost window.
    pub(crate) fn open_document_tab(&mut self, kind: AppKind, uri: &str) -> Result<String, String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no requesting window".to_string())?;
        self.open_document_tab_in_window(wid, kind, uri)
    }

    /// Open or focus a canonical local document in one exact host window, synchronously.
    pub(crate) fn open_document_tab_in_window(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
    ) -> Result<String, String> {
        self.open_document_tab_in_window_with_config_symlinks(
            wid,
            kind,
            uri,
            false,
            Placement::FocusOrNewTab,
        )
    }

    fn open_document_tab_in_window_with_config_symlinks(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
        allow_config_symlinks: bool,
        placement: Placement,
    ) -> Result<String, String> {
        Self::validate_document_request(kind, allow_config_symlinks)?;
        if !self.windows.contains_key(&wid) {
            return Err("requesting window disappeared".to_string());
        }
        let admitted = Self::admit_document_now(uri, allow_config_symlinks)?;
        self.install_admitted_document(wid, kind, uri, admitted, placement)
            .map(|installed| installed.line())
    }

    fn validate_document_request(kind: AppKind, allow_config_symlinks: bool) -> Result<(), String> {
        if !matches!(kind, AppKind::Markdown | AppKind::Editor) {
            return Err("document tabs must be Markdown or Editor".to_string());
        }
        if allow_config_symlinks && kind != AppKind::Editor {
            return Err("only the Manual config editor may bind config symlinks".to_string());
        }
        Ok(())
    }

    fn admit_document_now(
        uri: &str,
        allow_config_symlinks: bool,
    ) -> Result<AdmittedDocument, DocumentOpenFailure> {
        Self::admit_document_with(
            uri,
            crate::native_document_host::AdmitOptions {
                allow_config_symlinks,
                ..crate::native_document_host::AdmitOptions::DOCUMENT
            },
        )
    }

    fn admit_document_with(
        uri: &str,
        options: crate::native_document_host::AdmitOptions,
    ) -> Result<AdmittedDocument, DocumentOpenFailure> {
        crate::native_document_host::admit_local_file_opts(uri, options)
            .map_err(DocumentOpenFailure)
    }

    fn admit_document_request(
        key: &DocumentAdmissionKey,
        purpose: DocumentAdmissionPurpose,
        options: crate::native_document_host::AdmitOptions,
    ) -> Result<AdmittedDocument, DocumentOpenFailure> {
        if purpose == DocumentAdmissionPurpose::DirectoryEntry {
            let path = crate::native_document_host::file_uri_path(&key.uri)
                .map_err(DocumentOpenFailure)?;
            let (_, uri) =
                crate::native_document_host::open_path(&path).map_err(DocumentOpenFailure)?;
            return Self::admit_document_with(&uri, options);
        }
        Self::admit_document_with(&key.uri, options)
    }

    fn admission_options(
        &self,
        key: &DocumentAdmissionKey,
    ) -> crate::native_document_host::AdmitOptions {
        crate::native_document_host::AdmitOptions {
            allow_missing: key
                .edit
                .is_some_and(|ticket| self.edit_admits_missing(ticket)),
            ..crate::native_document_host::AdmitOptions::DOCUMENT
        }
    }

    fn replace_directory_listing(
        &mut self,
        document: DocumentId,
        listing: amacs::store::DirectoryListing,
    ) -> Result<(), String> {
        let before = self
            .document_store
            .snapshot(document)
            .ok_or("directory disappeared")?;
        let deltas = amacs::diff::minimal_edits(&before.text, &listing.text)
            .into_iter()
            .map(|edit| crate::document_store::EditDelta {
                old: edit.range,
                inserted_len: edit.insert.len(),
            })
            .collect::<Vec<_>>();
        let points = self
            .document_native_views(document)
            .into_iter()
            .map(|(_, _, view)| {
                let target = match self.native_runtime.view_state(view) {
                    Some(AppViewState::Editor(state)) => state.buffer.as_ref().and_then(|buffer| {
                        self.document_store
                            .directory(document)?
                            .target_at(buffer.primary_selection().head)
                    }),
                    _ => None,
                };
                (view, listing.point_for(target))
            })
            .collect::<Vec<_>>();
        self.document_store
            .set_directory(document, listing)
            .map_err(|error| format!("directory refresh failed: {error:?}"))?;
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or("directory disappeared")?;
        for (view, point) in points {
            if let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
                && let Some(buffer) = &mut state.buffer
            {
                buffer.observe_external(snapshot.seq, &deltas);
                buffer.selections = vec![crate::native_editor::Selection::caret(point)];
                buffer.primary = 0;
                buffer.viewport_anchor = point;
            }
        }
        self.repaint_document_views(document);
        Ok(())
    }

    fn finish_directory_refresh(
        &mut self,
        document: DocumentId,
        admitted: AdmittedDocument,
    ) -> Result<String, String> {
        if admitted.directory.is_none() {
            return Err("The path is no longer a directory".into());
        }
        if self.document_store.id_for_uri(admitted.canonical_uri()) != Some(document) {
            return Err("The directory changed while refreshing".into());
        }
        let granted = self
            .native_documents
            .grants
            .grant_admitted(admitted, GrantAccess::ReadOnly)
            .map_err(|error| error.to_string())?;
        self.replace_directory_listing(
            document,
            granted.directory.expect("checked directory admission"),
        )?;
        for (_, _, view) in self.document_native_views(document) {
            self.set_editor_view_status(view, "Directory refreshed");
        }
        self.repaint_document_views(document);
        Ok("Directory refreshed".into())
    }

    fn begin_document_admission(
        &mut self,
        wid: WindowId,
        key: DocumentAdmissionKey,
    ) -> Result<DocumentAdmission, String> {
        self.begin_document_admission_for(wid, key, DocumentAdmissionPurpose::Open)
    }

    fn begin_document_admission_for(
        &mut self,
        wid: WindowId,
        key: DocumentAdmissionKey,
        purpose: DocumentAdmissionPurpose,
    ) -> Result<DocumentAdmission, String> {
        // Coalescing is per window: a hit is this live window's own read in
        // flight. Another window's read of the file is a separate entry and
        // installs there (or is dropped if that window closed).
        if let Some(pending) = self.native_documents.admissions.get(&(wid, key.clone()))
            && pending.started.elapsed() < DOCUMENT_ADMISSION_STALE_AFTER
            && pending.purpose == purpose
        {
            return Ok(DocumentAdmission::Coalesced(pending.ticket));
        }
        self.native_documents.admissions.remove(&(wid, key.clone()));
        let options = self.admission_options(&key);
        let Some(proxy) = self.proxy.clone() else {
            let admitted = Self::admit_document_request(&key, purpose, options);
            if let DocumentAdmissionPurpose::DirectoryRefresh(document) = purpose {
                return self
                    .finish_directory_refresh(document, admitted?)
                    .map(DocumentAdmission::Opened);
            }
            if key.edit.is_some() {
                let reply = self.finish_edit_admission(wid, key, admitted.map(Box::new));
                return Ok(DocumentAdmission::Opened(reply.unwrap_or_else(|why| why)));
            }
            let admitted = admitted?;
            let uri = if purpose == DocumentAdmissionPurpose::DirectoryEntry {
                admitted.canonical_uri().to_string()
            } else {
                key.uri.clone()
            };
            return self
                .install_admitted_document(wid, key.kind, &uri, admitted, Placement::FocusOrNewTab)
                .map(|installed| DocumentAdmission::Opened(installed.line()));
        };
        let ticket = self.mint_document_admission(wid, key.clone());
        if let Some(pending) = self
            .native_documents
            .admissions
            .get_mut(&(wid, key.clone()))
        {
            pending.purpose = purpose;
        }
        let worker_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("aterm-document-admit".to_string())
            .spawn(move || {
                // This thread may wait for a download: the user asked for the
                // file and this is nobody's event loop. The policy is
                // thread-scoped; the main thread's stays off.
                if let Err(errno) = aterm_diskwalk::set_thread_materialize_policy(
                    aterm_diskwalk::MaterializePolicy::On,
                ) {
                    crate::logging::stderr_line!(
                        "aterm-gui: document admission worker could not enable \
                         dataless-file materialization (errno {errno}); an evicted \
                         file will be reported as not downloaded"
                    );
                }
                let result =
                    Self::admit_document_request(&worker_key, purpose, options).map(Box::new);
                let _ = proxy.send_event(crate::Wake::DocumentAdmitted(Box::new(
                    DocumentAdmissionOutcome {
                        ticket,
                        wid,
                        key: worker_key,
                        result,
                    },
                )));
            });
        if let Err(error) = spawned {
            self.native_documents.admissions.remove(&(wid, key));
            return Err(format!(
                "could not start the document admission worker: {error}"
            ));
        }
        Ok(DocumentAdmission::InFlight(ticket))
    }

    /// Record one admission as pending and mint its ticket.
    pub(crate) fn mint_document_admission(
        &mut self,
        wid: WindowId,
        key: DocumentAdmissionKey,
    ) -> DocumentAdmissionTicket {
        let raw = self.native_documents.next_admission;
        self.native_documents.next_admission = raw.wrapping_add(1).max(1);
        let ticket = DocumentAdmissionTicket(raw);
        self.native_documents.admissions.insert(
            (wid, key),
            PendingDocumentAdmission {
                ticket,
                started: std::time::Instant::now(),
                purpose: DocumentAdmissionPurpose::Open,
            },
        );
        ticket
    }

    /// Phase two of the two-phase open, on the main thread, from `Wake::DocumentAdmitted`.
    pub(crate) fn complete_document_admission(
        &mut self,
        outcome: DocumentAdmissionOutcome,
    ) -> Result<String, DocumentAdmissionRejected> {
        let DocumentAdmissionOutcome {
            ticket,
            wid,
            key,
            result,
        } = outcome;
        let slot = (wid, key);
        match self.native_documents.admissions.get(&slot) {
            Some(pending) if pending.ticket == ticket => {}
            _ => {
                return Err(DocumentAdmissionRejected::Dropped(format!(
                    "stale document admission for {} ignored",
                    slot.1.uri
                )));
            }
        }
        let purpose = self
            .native_documents
            .admissions
            .remove(&slot)
            .map(|pending| pending.purpose)
            .unwrap_or(DocumentAdmissionPurpose::Open);
        let reveal = self.native_documents.reveals.remove(&slot);
        let (wid, key) = slot;
        if key.edit.is_some() {
            return self
                .finish_edit_admission(wid, key, result)
                .map_err(DocumentAdmissionRejected::Dropped);
        }
        if !self.windows.contains_key(&wid) {
            return Err(DocumentAdmissionRejected::Dropped(format!(
                "requesting window closed before {} was admitted",
                key.uri
            )));
        }
        if let DocumentAdmissionPurpose::DirectoryRefresh(document) = purpose
            && (self.document_store.directory(document).is_none()
                || self.document_store.view_count(document) == Some(0))
        {
            return Err(DocumentAdmissionRejected::Dropped(
                "directory view closed before refresh completed".into(),
            ));
        }
        if let DocumentAdmissionPurpose::DirectoryRefresh(document) = purpose {
            let refreshed = result
                .map_err(|failure| failure.to_string())
                .and_then(|admitted| self.finish_directory_refresh(document, *admitted));
            return Ok(refreshed.unwrap_or_else(|why| {
                let status = format!("Directory refresh failed: {why}");
                for (_, _, view) in self.document_native_views(document) {
                    self.set_editor_view_status(view, &status);
                }
                self.repaint_document_views(document);
                status
            }));
        }
        let admitted =
            *result.map_err(|failure| DocumentAdmissionRejected::Failed(failure.to_string()))?;
        let uri = if purpose == DocumentAdmissionPurpose::DirectoryEntry {
            admitted.canonical_uri().to_string()
        } else {
            key.uri.clone()
        };
        let installed = self
            .install_admitted_document(wid, key.kind, &uri, admitted, Placement::FocusOrNewTab)
            .map_err(DocumentAdmissionRejected::Failed)?;
        if let Some(reveal) = reveal {
            self.reveal_opened_document(wid, reveal)
                .map_err(DocumentAdmissionRejected::Failed)?;
        }
        Ok(installed.line())
    }

    /// Whether an admission for exactly this request is still pending (test / diagnostic seam).
    #[cfg(test)]
    pub(crate) fn document_admission_pending(
        &self,
        wid: WindowId,
        key: &DocumentAdmissionKey,
    ) -> Option<DocumentAdmissionTicket> {
        self.native_documents
            .admissions
            .get(&(wid, key.clone()))
            .map(|pending| pending.ticket)
    }

    /// The main-thread tail of every open.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Restore",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "RestoreIdentical",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "PersonOpens",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn install_admitted_document(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
        admitted: AdmittedDocument,
        placement: Placement,
    ) -> Result<InstalledDocument, String> {
        // Markdown is an opening intent, never a second document application.
        // Its Read/Edit/Split views keep the editor's identity, buffer and grant.
        let read_intent = kind == AppKind::Markdown;
        Self::validate_document_request(kind, false)?;
        let kind = AppKind::Editor;
        let existing = self.document_store.id_for_uri(admitted.canonical_uri());
        if let Some(document) = existing
            && self.document_store.directory(document).is_some() != admitted.directory.is_some()
        {
            return Err("The path changed between a file and a directory; close its existing buffer before reopening it".into());
        }
        // `edit … ro=1`: a fresh buffer nothing changes or saves. A buffer already open for the
        // file may hold changes and a writable grant, so it is never made read-only in place.
        let read_only = match placement {
            Placement::NewTabAfter { ticket } => self.native_documents.edits.read_only(ticket),
            Placement::FocusOrNewTab | Placement::NewTab => false,
        };
        if read_only && existing.is_some() {
            return Err("ERR refused open: a read-only edit opens a file no buffer holds\n".into());
        }
        // `edit … secret=`: the columns of a private grid its readers never get. A private
        // document that names none conceals every cell; any other document takes no `secret=`,
        // since only a private document's text is withheld from every other reader too.
        let private = crate::native_document_journal::private_uri(admitted.canonical_uri());
        let secret = match placement {
            Placement::NewTabAfter { ticket } => self
                .native_documents
                .edits
                .secret(ticket)
                .map(<[String]>::to_vec),
            Placement::FocusOrNewTab | Placement::NewTab => None,
        };
        if secret.is_some() && !private {
            return Err("ERR refused open: secret= names the columns of a private grid\n".into());
        }
        let access = if kind == AppKind::Editor && !read_only {
            GrantAccess::ReadWrite
        } else {
            GrantAccess::ReadOnly
        };
        let granted = self
            .native_documents
            .grants
            .grant_admitted(admitted, access)
            .map_err(|error| format!("document open failed: {error}"))?;
        let mut recovery_notice = None;
        let document = if let Some(document) = existing {
            document
        } else if crate::native_document_journal::private_uri(&granted.grant.canonical_uri) {
            // A private edit's document keeps no draft journal: nothing it holds reaches the
            // drafts folder, and what an earlier open left there goes before the buffer opens.
            let document = self
                .document_store
                .open(granted.grant.canonical_uri.clone(), granted.text.clone());
            if let Some(journals) = self.native_documents.journals.as_mut()
                && let Err(error) = journals.decline(document, &granted.grant.canonical_uri)
            {
                let _ = self.document_store.remove_if_unattached(document);
                return Err(format!(
                    "a private document's earlier draft could not be removed: {error}"
                ));
            }
            document
        } else {
            let decision = self
                .native_documents
                .journals
                .as_ref()
                .ok_or_else(|| {
                    self.native_documents
                        .journals
                        .unavailable()
                        .unwrap_or("document recovery is unavailable")
                        .to_string()
                })?
                .inspect_open(&granted.grant.canonical_uri, granted.text.as_bytes())?;
            let recovered = decision.recovered_text.clone();
            let mut drafted = false;
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
                // A labelled frame, and the floor of the buffer's undo: C-/
                // stops at it (`RECOVERED_FLOOR`), so the draft never goes
                // back to the file's text but by revert-buffer.
                let outcome = self.editor_workspace.replace_text(
                    &mut self.document_store,
                    document,
                    disk_snapshot.seq,
                    &recovered,
                    crate::native_editor::Origin::Recovered,
                );
                if outcome.is_err() {
                    let _ = self.document_store.remove_if_unattached(document);
                    return Err("recovered draft could not be installed atomically".to_string());
                }
                drafted = true;
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
            if drafted {
                self.native_documents
                    .unvouched
                    .insert(document, crate::owner_keys_save::Unvouched::Recovered);
            }
            document
        };
        if read_only && let Err(error) = self.document_store.set_read_only(document) {
            let _ = self.document_store.remove_if_unattached(document);
            return Err(format!(
                "the read-only document could not be marked: {error:?}"
            ));
        }
        // Concealed for the document's life: an earlier open's concealment stands.
        if private
            && let Err(error) = self.document_store.set_concealed(
                document,
                secret.map_or(
                    crate::document_store::Concealed::All,
                    crate::document_store::Concealed::Columns,
                ),
            )
        {
            let _ = self.document_store.remove_if_unattached(document);
            return Err(format!(
                "the private document could not be concealed: {error:?}"
            ));
        }
        if let Some(listing) = granted.directory.clone() {
            self.replace_directory_listing(document, listing)?;
        }
        self.native_documents
            .persistence
            .register(document, granted.grant.id, granted.observed, &granted.text)
            .map_err(|error| format!("document persistence failed: {error:?}"))?;
        if granted.directory.is_none() {
            self.watch_document_file(document);
        }
        if existing.is_some() && granted.directory.is_none() {
            let _ = self.refresh_native_document(document)?;
        }

        if placement == Placement::FocusOrNewTab
            && let Some((tab, view)) = self.document_view_in_window(wid, kind, document)
        {
            if !read_intent
                && let Some(instance) = self.native_runtime.instance_for_document(kind, document)
            {
                self.native_runtime.set_editor_markdown_mode(
                    instance,
                    view,
                    MarkdownWorkspaceMode::Edit,
                );
                self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
            }
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_set.switch_to(tab);
                if let Some(target) = ws.tab_set.active_mut() {
                    target.set_focus(view);
                }
                ws.last_present = None;
            }
            self.resync_active_or_window(wid);
            self.request_native_redraw(wid);
            return Ok(InstalledDocument {
                kind,
                canonical_uri: granted.grant.canonical_uri.clone(),
                document,
                tab,
                view,
            });
        }
        let previously_active = self.windows.get(&wid).and_then(|ws| ws.tab_set.active_id());
        // A view the person does not get in front goes in behind the tab in front with no
        // switch and no sync, so their program is never told it lost focus for it.
        let keep = match placement {
            Placement::NewTabAfter { ticket } => {
                self.admitted_view_kept_front(wid, ticket, previously_active)
            }
            Placement::FocusOrNewTab | Placement::NewTab => None,
        };

        let title = granted
            .grant
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map_or_else(|| granted.grant.path.display().to_string(), str::to_string);
        let presentation = crate::tab_model::TabPresentation {
            title: title.clone(),
            icon: Some(kind.icon()),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: Some(format!("{} · {}", kind.display_name(), uri)),
        };
        let mut state = crate::native_app::EditorViewState::default();
        let instance = self.native_runtime.instance_for_document(kind, document);
        let markdown = instance.map_or_else(
            || crate::native_app::is_markdown_uri(&granted.grant.canonical_uri),
            |instance| matches!(self.native_runtime.app(instance), Some(NativeApp::Editor(editor)) if editor.markdown.is_some()),
        );
        if markdown {
            state.markdown = Some(MarkdownWorkspaceView::new(
                if read_intent && self.document_store.concealed(document).is_none() {
                    MarkdownWorkspaceMode::Read
                } else {
                    MarkdownWorkspaceMode::Edit
                },
            ));
        }
        let state = AppViewState::Editor(Box::new(state));
        let install = if let Some(instance) = instance {
            self.install_native_tab_keeping(wid, instance, state, presentation, keep)
                .map(|(tab, view)| (instance, tab, view))
        } else {
            let mut editor =
                EditorApp::new_with_uri(document, title, granted.grant.canonical_uri.clone());
            editor.concealed = self.document_store.concealed(document).cloned();
            editor.read_only = self.document_store.read_only(document);
            let app = NativeApp::Editor(Box::new(editor));
            self.install_new_native_tab_keeping(wid, app, state, presentation, keep)
        };
        let (instance, tab, view) = install.map_err(|error| {
            aterm_log::warn!("could not install native document tab: {error:?}");
            "This document could not be reopened".to_string()
        })?;

        if let Err(error) = self.attach_document_view(kind, document, view) {
            self.rollback_document_tab(wid, tab);
            return Err(error);
        }
        // Seed derived readers from the canonical buffer before the first paint or
        // semantic action. It may already contain a recovered or unsaved draft.
        let snapshot = self
            .document_store
            .snapshot(document)
            .ok_or_else(|| "the attached document disappeared".to_string())?;
        self.native_runtime.publish_document(
            document,
            snapshot.text.as_ref(),
            self.document_store.revision(document).unwrap_or(0),
            self.document_store.dirty(document).unwrap_or(false),
        );
        if let Some(listing) = self.document_store.directory(document)
            && let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
            && let Some(buffer) = &mut state.buffer
        {
            buffer.selections = vec![crate::native_editor::Selection::caret(
                listing.entries.first().map_or(0, |(range, _)| range.start),
            )];
        }
        if let Some(notice) = recovery_notice {
            let message = match notice {
                crate::native_document_journal::RecoveryNotice::Recovered
                    if self.incoming_handoff_pending =>
                {
                    CARRIED_DRAFT_NOTICE.to_string()
                }
                notice => notice.message(),
            };
            self.set_document_recovery_status(document, Some(message));
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
            self.native_documents.disk_conflicts.contains_key(&document),
        );
        self.refresh_disambiguated_document_titles();
        self.refresh_native_presentation(wid, instance, view);
        if let Placement::NewTabAfter { ticket } = placement {
            self.place_admitted_view(wid, tab, ticket);
        } else {
            self.resync_active_or_window(wid);
            self.request_native_redraw(wid);
        }
        Ok(InstalledDocument {
            kind,
            canonical_uri: granted.grant.canonical_uri.clone(),
            document,
            tab,
            view,
        })
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
                let mut found = None;
                tab.root.any_leaf(&mut |&view| {
                    let Some(crate::tab_model::View::Native(native)) =
                        self.view_store.get(view).copied()
                    else {
                        return false;
                    };
                    let Some(app) = self.native_runtime.app(native.instance) else {
                        return false;
                    };
                    if app.kind() == kind && app.document_id() == Some(document) {
                        found = Some((tab.id, view));
                        true
                    } else {
                        false
                    }
                });
                found
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
            AppKind::Editor => {
                let buffer = self
                    .editor_workspace
                    .attach(&mut self.document_store, document, document_view)
                    .map_err(|error| format!("could not attach editor view: {error:?}"))?;
                let mut buffer = buffer;
                // A table opens as a grid; one too large for it stays text and says why.
                let delimiter = self
                    .document_store
                    .canonical_uri(document)
                    .filter(|_| self.document_store.directory(document).is_none())
                    .zip(self.document_store.snapshot(document))
                    .and_then(|(uri, snapshot)| {
                        amacs::language::Language::detect(
                            uri,
                            amacs::table::first_line(&snapshot.text),
                        )
                        .delimiter()
                    });
                let refusal = delimiter.and_then(|delimiter| {
                    self.editor_workspace
                        .set_face(&self.document_store, &mut buffer, Some(delimiter))
                        .err()
                });
                let Some(AppViewState::Editor(state)) = self.native_runtime.view_state_mut(view)
                else {
                    return Err("editor runtime view disappeared during attach".to_string());
                };
                state.buffer = Some(buffer);
                if refusal.is_some() {
                    state.status = refusal;
                }
                Ok(())
            }
            AppKind::Markdown
            | AppKind::Mail
            | AppKind::About
            | AppKind::Settings
            | AppKind::SystemState
            | AppKind::Recovery
            | AppKind::Allocation
            | AppKind::Workers
            | AppKind::Tables => Err("requested app is not a document app".to_string()),
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

    pub(crate) fn request_native_redraw(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_present = None;
        }
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }
}

fn refused_config_read(
    path: &std::path::Path,
    error: &crate::native_document_host::DocumentHostError,
) -> String {
    aterm_log::warn!("could not validate {}: {error}", path.display());
    crate::save_words::words(
        crate::save_words::Speaker::Aterm,
        path,
        &crate::save_words::Failure::Open(error),
    )
}

fn ensure_config_file(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("config path must be absolute: {}", path.display()));
    }
    let contents = crate::native_document_host::read_config_atomic_file(
        path,
        crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
        true,
    )
    .map_err(|error| refused_config_read(path, &error))?;
    if !contents.baseline.observed.exists {
        match crate::native_document_host::commit_atomic_bytes_ordered(&contents.baseline, b"") {
            crate::native_document_host::AtomicCommitResult::Committed(_) => {}
            crate::native_document_host::AtomicCommitResult::Conflict { .. } => {
                crate::native_document_host::read_config_atomic_file(
                    path,
                    crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
                    false,
                )
                .map_err(|error| refused_config_read(path, &error))?;
            }
            crate::native_document_host::AtomicCommitResult::Failed {
                stage,
                message,
                cause,
            } => {
                aterm_log::warn!(
                    "could not create {} at {stage:?}: {message}",
                    path.display()
                );
                return Err(crate::save_words::words(
                    crate::save_words::Speaker::Aterm,
                    path,
                    &crate::save_words::Failure::Write { stage, cause },
                ));
            }
            crate::native_document_host::AtomicCommitResult::PublishedUnverified {
                stage,
                message,
                cause,
                ..
            } => {
                aterm_log::warn!(
                    "{} may have been created but could not be verified at {stage:?}: {message}",
                    path.display()
                );
                return Err(format!(
                    "{}; open it again to check it",
                    crate::save_words::words(
                        crate::save_words::Speaker::Aterm,
                        path,
                        &crate::save_words::Failure::Unconfirmed { stage, cause },
                    )
                ));
            }
        }
    }
    Ok(path.to_path_buf())
}

fn crlf_held_insertion(event: &AppEvent) -> Option<AppEvent> {
    let as_lf = |text: &str| text.contains("\r\n").then(|| text.replace("\r\n", "\n"));
    match event {
        AppEvent::EditorInput(EditorInput::Text(text)) => {
            as_lf(text).map(|text| AppEvent::EditorInput(EditorInput::Text(text)))
        }
        AppEvent::EditorInput(EditorInput::Paste(text)) => {
            as_lf(text).map(|text| AppEvent::EditorInput(EditorInput::Paste(text)))
        }
        AppEvent::TextInput(TextInputEvent::Commit(text)) => {
            as_lf(text).map(|text| AppEvent::TextInput(TextInputEvent::Commit(text)))
        }
        #[cfg(any(a11y_tree, test))]
        AppEvent::InsertText(text) => as_lf(text).map(AppEvent::InsertText),
        _ => None,
    }
}

fn encodes_to_len(text: &str, encoding: crate::native_document_io::DiskEncoding, len: u64) -> bool {
    let mark = if encoding.bom {
        crate::native_document_io::UTF8_BOM.len()
    } else {
        0
    };
    let least = (text.len() as u64).saturating_add(mark as u64);
    match encoding.eol {
        crate::native_document_io::LineEnding::Lf => least == len,
        crate::native_document_io::LineEnding::CrLf => {
            least <= len
                && least.saturating_add(text.bytes().filter(|&byte| byte == b'\n').count() as u64)
                    == len
        }
    }
}

fn editor_error_message(error: &crate::native_editor::EditorError) -> String {
    use crate::native_editor::EditorError;
    match error {
        EditorError::NothingToUndo => "Nothing to undo".to_string(),
        EditorError::NothingToRedo => "Nothing to redo".to_string(),
        EditorError::NoKill => "Kill ring is empty".to_string(),
        EditorError::UndoRefused { why } => why.clone(),
        EditorError::StaleView { .. } | EditorError::TransactionConflict { .. } => {
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

/// `NativeUpdateEditorCarry`'s two handoff steps, each waived where its Tier-1
/// (`editor_carry_conformance`) binds it.
#[cfg(all(any(test, debug_assertions), unix))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateEditorCarry",
    action = "Park",
    reason = "The handoff's own step: the park installs the attempt record, which this machine \
              reads only through the fences and the late fact. Tier-1 installs the real record \
              (`PendingUpdateHandoff`) the park installs; the park itself is \
              `NativeUpdateSeamlessHandoffOwnership`'s `ParkOutgoingReaders`."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "NativeUpdateEditorCarry",
    action = "StandDown",
    reason = "The handoff's own step: a launched successor stood down before the park leaves \
              nothing restored and nothing owed. Tier-1 retires a real prelaunch record through \
              the real completion reducer (`reduce_returned_handoff_completion`)."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn editor_carry_waivers() {}

/// Once amacs lets go of a private edit's document, sweep aterm's own scratch root
/// ([`aterm_types::private_edit::scratch_root`]) for grid scratch folders whose maker died before
/// removing its own (a live maker removes its own when its edit ends), on a background thread: the
/// release never waits on the disk. The sweep never starts from the document's path, which a
/// controller's `edit` may name anywhere.
fn sweep_private_scratch() {
    if cfg!(test) {
        return;
    }
    let spawned =
        crate::qos::spawn_lane_thread("aterm-private-sweep", crate::qos::Role::Background, || {
            aterm_types::private_edit::sweep_dead(&aterm_types::private_edit::scratch_root(), None);
        });
    if let Err(error) = spawned {
        aterm_log::warn!("private edit sweep not started: {error}");
    }
}

#[cfg(test)]
#[path = "app_documents_tests.rs"]
mod tests;
