// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Markdown/editor host integration.
//!
//! Local files enter through one host-minted grant. Canonical bytes then live in
//! `DocumentStore`; Markdown and editor app instances retain only `DocumentId`,
//! while each presentation gets its own stable view state.

use std::collections::{BTreeMap, BTreeSet};

use crate::app_edit::Placement;
use crate::document_store::{DocumentId, DocumentViewId};
use crate::native_app::{
    AppEvent, AppKind, AppViewState, EditorApp, EventResult, MarkdownApp, MarkdownViewState,
    NativeApp, TextInputEvent,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigEditorRevealDecision {
    Select {
        requested: String,
        range: std::ops::Range<usize>,
    },
    /// The key is not in the file: add it (or, where no value is known,
    /// say how), never a Search that cannot match.
    AddAbsent { requested: String },
}

/// Resolve only an in-document Settings → Manual target. Filesystem authority
/// is deliberately absent from [`crate::native_app::ConfigEditorTarget`]; the
/// host has already opened its canonical `aterm.toml` before calling this.
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
    /// At the value that resolves exactly as its absence does, so the file
    /// means what it meant.
    AtDefault,
    /// With no value yet, the caret where it goes: no value resolves as the
    /// key's absence does (a shell, a font family, a list, a path), and its
    /// unset words describe a behaviour rather than spell a value. The line
    /// does not parse until a value is typed, so Save waits for one; `record`
    /// names the `[[table]]` record the line was added in.
    NeedsValue { record: Option<&'static str> },
    /// A table or map key (`keybindings`, `harness`) as its empty `[key]`
    /// table, the caret on the line under it where its entries go: an empty
    /// table reads as an absent one.
    Table,
}

/// The edit that adds `key`, absent from `source`, where TOML reads it — a
/// top-level key among the root keys, a dotted key in its table, a member of
/// a list of records in a new `[[record]]` — at the value that resolves as
/// the key's absence does ([`crate::prefs::unset_literal`] for a Settings
/// row, the schema's documented default otherwise), checked to change
/// nothing; else with no value, the caret where it goes. Never the words a
/// Settings row shows for an unset value ("platform default", "Built-in
/// sprite"), never an example someone did not choose. Formatting elsewhere
/// is kept: the edit is the smallest span the two texts differ in. A table
/// or a map comes in as its empty `[key]` table. `None` for a list of
/// records itself and for a member of one the file already has (which
/// record is the person's call).
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
    // No value means "unset": the line without one, `key = `. A list of
    // records takes no line (it has no example value either).
    crate::native_config_language::config_key_example_literal(key)?;
    let placeholder = aterm_toml::edit::Item::Value(aterm_toml::edit::Value::from(""));
    let mut added = crate::prefs::with_toml_item(source, key, placeholder).ok()?;
    let value = crate::native_config_language::config_key_source_range(&added, key)?;
    added.replace_range(value.clone(), "");
    let (edit, _) = smallest_edit(source, &added, key, Some(value.start))?;
    Some((edit, AbsentKeyAdded::NeedsValue { record: None }))
}

/// The value `key` resolves to when it is absent from `source`, as an item to
/// write: a Settings row's [`crate::prefs::unset_literal`] (read with the
/// document as it stands, so a default that follows another setting is that
/// setting's), else the schema's documented default for a key only aterm.toml
/// holds. `None` where no value resolves as absence does.
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

/// Whether `added` (`source` with a key added at its default) means what
/// `source` meant, as far as aterm can tell: it loads, and Manual finds
/// nothing in it that it did not find in `source`. A default the reader
/// refuses, or one that draws a warning, is not added as one.
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

/// A table or map key (`keybindings`, `harness`) absent from `source`: its
/// empty `[key]` table at the end of the file, the caret on the line under
/// it, where its entries go. Never "a line of its own" left wherever the
/// caret was — at the end of a file that is inside its last table, and a
/// line typed there would be that table's (`machine.keybindings`). An empty
/// table reads as an absent one, checked to change nothing; `None` for any
/// other key, or where the header would change something.
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

/// A member of a list of records (`sparkle_words.custom`)
/// absent from `source`: a new `[[root]]` record at the end of the file with
/// the member's line and no value, the caret where it goes. A record is a
/// list element, never a plain table — `[sparkle_words.custom.graphic]` is a
/// map where the reader wants a list, and the file stops loading. `None`
/// where `source` already has records: which one to add it to is the
/// person's call.
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

/// The smallest edit that turns `source` into `added`, the caret after it,
/// or at byte `caret` of `added` when given.
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
struct PendingDocumentClose {
    window: WindowId,
    tab: crate::tab_model::TabId,
    views: Vec<DocumentViewId>,
    whole_tab: bool,
}

/// Latest explicit durability intent received while an older generation is in
/// flight. The document owns it, not the initiating view, so a repeated Save or
/// a close/Quit barrier survives focus changes; it is pumped as soon as the
/// current atomic save reduces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingDocumentSaveIntent {
    seq: aterm_buffer::Seq,
    source_view: crate::tab_model::ViewId,
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
        plan: crate::native_document_io::JournalAppendPlan,
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
    fn proxy(&self) -> &aterm_window::event_loop::EventLoopProxy<crate::Wake> {
        match self {
            Self::Save { proxy, .. }
            | Self::JournalAppend { proxy, .. }
            | Self::JournalRewrite { proxy, .. }
            | Self::JournalReseat { proxy, .. } => proxy,
        }
    }
}

/// Jobs can retain a complete 32 MiB document image. The channel holds at most
/// four waiting jobs and the single disk worker owns at most one executing job:
/// five retained full images total. Journal saturation retains only a coalesced
/// document ID and replans from the reducer after a slot-release wake, so the UI
/// neither blocks nor grows another queue of encoded images.
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
}

/// An append the headless test seam held back from inline execution
/// ([`DocumentHostRuntime::held_journal_appends`]). Unix, as the draft-carry
/// tests that drive it (`editor_carry_conformance`) and every journal seam here.
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) struct HeldJournalAppend {
    pub(crate) document: DocumentId,
    path: std::path::PathBuf,
    key: crate::native_document_io::JournalDocumentKey,
    pub(crate) plan: crate::native_document_io::JournalAppendPlan,
}

/// A re-seat the headless test seam held back from inline execution
/// ([`DocumentHostRuntime::held_journal_reseats`]).
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) struct HeldJournalReseat {
    pub(crate) document: DocumentId,
    path: std::path::PathBuf,
    plan: JournalReseatPlan,
}

/// The status a save shows when its completion could not be recorded: the
/// write may or may not have landed, so the person saves again. The cause is
/// logged, never painted.
const SAVE_UNCONFIRMED: &str = "Save could not be confirmed; choose Save again";

/// The status a successor gives a document whose unsaved draft it restored
/// during an update: the draft is the buffer, and the file is as it was.
pub(crate) const CARRIED_DRAFT_NOTICE: &str =
    "Unsaved draft carried through the update — the file on disk is unchanged";

/// Whether one document's unsaved draft rides an update to the successor
/// ([`App::draft_carry`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftCarry {
    /// Durable in its journal and verified on disk: the successor's restore
    /// replays exactly this text. Carries its size, which the update's cap sums.
    Carried { bytes: usize },
    /// Its journal is writing the latest edit (or re-establishing itself):
    /// asking again shortly will tell, and nobody needs to act.
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

/// Admit an already-planned journal effect without blocking the UI. `Full` is
/// an ordinary scheduling outcome: roll the reducer back to replannable state
/// and retain only its document ID. A disconnected worker preserves the reducer
/// intent too, but reports the permanent error instead of arming a wake-less
/// retry.
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

/// Shared admission primitive. Its non-blocking `Full` verdict is Tier-1-bound
/// to `native_document_queue_model` below; production uses the same generic
/// function with `NativeDocumentJob`.
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

fn native_document_queue_slot_available_wake() -> crate::Wake {
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
                        // `recv` released exactly one of the four waiting slots.
                        // Notify the UI before doing disk I/O so one coalesced
                        // journal intent can claim it without waiting for this
                        // potentially slow job to finish.
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
                                    // The worker thread: no frame to drop, so
                                    // ride out the whole fork window rather
                                    // than lose the append.
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

/// The draft-journal store, or why there is none, settled at its first look.
///
/// Opening it reads the per-install journal secret, and a FIRST launch mints
/// that secret behind two `F_FULLFSYNC` barriers (`load_or_mint_journal_secret`):
/// 15.7 ms of the UI thread before the first window on an idle disk, and far more
/// under the first launch's package writes. Nothing before a document needs the
/// store, so the running App opens it on a worker ([`Self::opening`]) and the
/// first look (`as_ref`, `as_mut`, `available_mut`, `unavailable`) joins that
/// worker if it is still opening.
struct JournalSlot {
    settled: std::cell::OnceCell<Result<DocumentJournalStore, String>>,
    opening: std::cell::Cell<Option<std::thread::JoinHandle<Result<DocumentJournalStore, String>>>>,
    /// What the first look runs inline when no worker is opening the store
    /// ([`Self::deferred`]).
    deferred: std::cell::Cell<Option<JournalOpener>>,
}

/// How a [`JournalSlot`] opens its store: `DocumentJournalStore::system_default`.
type JournalOpener = fn() -> Result<DocumentJournalStore, String>;

impl JournalSlot {
    /// A slot already settled to `store`.
    fn open(store: Result<DocumentJournalStore, String>) -> Self {
        Self {
            settled: std::cell::OnceCell::from(store),
            opening: std::cell::Cell::new(None),
            deferred: std::cell::Cell::new(None),
        }
    }

    /// A slot `open` settles inline at its first look: [`Self::opening`]
    /// without the worker. The headless App's (`App::headless_for_test`): nine
    /// in ten of the test binary's Apps never look, and so never mint a secret
    /// behind its two barriers or make a root for the exit sweep to remove
    /// (2026-10-20, `-p aterm-gui --lib`: 4,176 stores opened a run, 325
    /// deferred; the sweep after the last test 0.9-4.6 s, 0.16-0.53 s deferred).
    fn deferred(open: JournalOpener) -> Self {
        Self {
            settled: std::cell::OnceCell::new(),
            opening: std::cell::Cell::new(None),
            deferred: std::cell::Cell::new(Some(open)),
        }
    }

    /// A slot `open` settles on an `aterm-journal-open` worker, or inline when no
    /// thread can start. `Responsive`, the floor for work the UI thread may wait
    /// on (`qos`).
    fn opening<F>(open: F) -> Self
    where
        F: FnOnce() -> Result<DocumentJournalStore, String> + Send + 'static,
    {
        // Handed over only once the worker exists, so a failed spawn still has
        // `open` to run inline.
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

    /// The store, or the reason there is none.
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
    /// The file changed on disk under a dirty buffer, or a save found it
    /// changed.
    Version,
    /// A save could not be confirmed: the disk may or may not hold it.
    SaveUnconfirmed,
    /// aterm.toml's file binding (its target or logical path) changed; the
    /// document must be reopened, whatever the buffer holds.
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
    /// successor may have read its restore image. The Commit check fails
    /// (`Wait:`) on a landing it cannot order against that read; the rollback
    /// clears it. Set from every completion path
    /// ([`App::note_document_journal_landing`]).
    journal_landed_while_parked: bool,
    /// Test seam: the headless App has no document worker, so journal effects
    /// run inline. When `Some`, appends are held here for a test to run later as
    /// the worker would (`App::run_held_journal_append_for_test`), putting an
    /// append in flight across a park.
    #[cfg(all(test, unix))]
    held_journal_appends: Option<Vec<HeldJournalAppend>>,
    /// Test seam: [`Self::held_journal_appends`] for re-seats
    /// (`App::run_held_journal_reseat_for_test`).
    #[cfg(all(test, unix))]
    held_journal_reseats: Option<Vec<HeldJournalReseat>>,
    /// Test seam: a headless successor restores no document tab
    /// ([`App::successor_restores_document_tabs`]). Set, the App answers as a
    /// windowed one, so a test can carry drafts.
    #[cfg(all(test, unix))]
    carry_as_windowed_for_test: bool,
    /// Documents whose on-disk generation no longer matches the saver baseline,
    /// and why. This host-owned latch blocks every save entry point until an
    /// explicit reload/reconciliation installs a fresh stable observation — or
    /// the buffer returns to the version aterm admitted, which holds no local
    /// edits ([`App::adopt_disk_when_buffer_is_base`]).
    disk_conflicts: BTreeMap<DocumentId, DiskConflictCause>,
    inflight: BTreeSet<DocumentId>,
    pending_saves: BTreeMap<DocumentId, PendingDocumentSaveIntent>,
    pending_closes: BTreeMap<DocumentId, Vec<PendingDocumentClose>>,
    /// OS-window closes are all-or-nothing across every document leaf in that
    /// window: no tab or view is detached while any member still lacks its exact
    /// durable checkpoint.
    pending_window_closes: BTreeMap<WindowId, DocumentClosePlan>,
    /// Whole-app Quit waits on one process-wide plan before update application or
    /// event-loop exit. It supersedes (without partially applying) window plans.
    pending_quit: Option<DocumentClosePlan>,
    /// Admissions running on an `aterm-document-admit` worker (the two-phase
    /// open; see [`App::begin_document_admission`]). Keyed by requesting window
    /// and request: a repeat in the same window coalesces onto the read in
    /// flight, while another window's request is its own read, so a completion
    /// for a closed window never swallows a live window's tab. A completion is
    /// matched by ticket and applied at most once.
    admissions: BTreeMap<(WindowId, DocumentAdmissionKey), PendingDocumentAdmission>,
    next_admission: u64,
    /// The line each pending open reveals once its tab is installed
    /// ([`App::open_document_tab_at`]): keyed like
    /// [`Self::admissions`], the latest request's line winning, and taken by
    /// the install (or dropped with a failed one).
    reveals: BTreeMap<(WindowId, DocumentAdmissionKey), LineReveal>,
    /// The `edit` verb's tickets ([`crate::app_edit`]).
    pub(crate) edits: crate::app_edit::EditTickets,
    /// A document view closed, or a Suspended document's save or journal write
    /// landed, since the last [`App::release_unheld_documents`]: the next park
    /// looks for documents nothing holds any more.
    pub(crate) release_due: bool,
    /// When the last recently-closed record that alone kept a document expires
    /// (`App::lat_epoch` milliseconds): the first park at or past it looks
    /// again ([`App::release_documents_at_park`]).
    pub(crate) release_deadline_ms: Option<u64>,
}

/// Where a document open lands its caret: a 1-based line and, optionally, a
/// 1-based column (a file citation's `path:12:3`, `crate::path_link`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineReveal {
    pub(crate) line: u32,
    pub(crate) col: Option<u32>,
}

impl Default for DocumentHostRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentHostRuntime {
    /// The headless App's runtime: its journal store opens at the first look
    /// ([`JournalSlot::deferred`]), as the running App's ([`Self::opening`])
    /// is joined there, with no worker.
    pub(crate) fn new() -> Self {
        Self::with_journals(JournalSlot::deferred(DocumentJournalStore::system_default))
    }

    /// The running App's runtime: its journal store opens on a worker
    /// ([`JournalSlot::opening`]), so the boot never waits on the secret's mint.
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
            inflight: BTreeSet::new(),
            pending_saves: BTreeMap::new(),
            pending_closes: BTreeMap::new(),
            pending_window_closes: BTreeMap::new(),
            pending_quit: None,
            admissions: BTreeMap::new(),
            next_admission: 1,
            reveals: BTreeMap::new(),
            edits: crate::app_edit::EditTickets::default(),
            release_due: false,
            release_deadline_ms: None,
        }
    }
}

/// Identity of one two-phase open in flight: the exact URI and the hosting app.
/// The same key in the same window coalesces; Markdown and Editor opens of one
/// file, or requests from two windows, do not. Only documents take the
/// two-phase path (the Manual config editor, which binds config symlinks, stays
/// synchronous), so the symlink rule is not part of the key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DocumentAdmissionKey {
    pub(crate) uri: String,
    pub(crate) kind: AppKind,
    /// The `edit` ticket this admission opens a fresh view for
    /// ([`crate::app_edit`]); `None` for every gesture. Part of the key, so an
    /// edit never coalesces with a gesture's open of the same file, or with
    /// another edit.
    pub(crate) edit: Option<crate::app_edit::EditTicketId>,
}

/// One in-flight admission (see [`DocumentHostRuntime::admissions`]).
#[derive(Debug)]
struct PendingDocumentAdmission {
    ticket: DocumentAdmissionTicket,
    started: std::time::Instant,
}

/// Monotonic identity of one admission request. A completion carries the
/// ticket it was started with; the main thread applies it only while the
/// pending entry still holds that ticket, so a superseded or already-finished
/// admission's result is dropped rather than applied twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DocumentAdmissionTicket(u64);

/// An `aterm-document-admit` worker silent this long counts as abandoned for
/// coalescing only: a new request for the file starts a fresh read rather than
/// join a wait that may never end (an unreachable volume, a stalled download).
/// A late result no longer matches its ticket and is dropped.
const DOCUMENT_ADMISSION_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// What the worker hands back through `Wake::DocumentAdmitted`: the request it
/// was given, verbatim, plus the admitted file or why the read refused. Plain
/// owned data; it crosses from the worker to the main thread by move.
#[derive(Debug)]
pub(crate) struct DocumentAdmissionOutcome {
    pub(crate) ticket: DocumentAdmissionTicket,
    pub(crate) wid: WindowId,
    pub(crate) key: DocumentAdmissionKey,
    pub(crate) result: Result<Box<AdmittedDocument>, DocumentOpenFailure>,
}

/// Why an admission's read refused: the host's error, kept typed so an `edit`
/// ticket can name it on the wire (`ERR refused <word>`,
/// [`DocumentHostError::refusal_word`]). Its text is what a gesture shows.
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

/// The tab an open landed in: the canonical document, the view showing it and
/// that view's tab.
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
    /// No event-loop proxy exists to report back to (the headless test harness),
    /// so the admission ran inline and the tab is already open. Carries the
    /// same `app <kind> <uri> view=<n>` line the synchronous opener returns.
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

/// Why a worker's completion installed no tab. A dropped completion is
/// bookkeeping and only logged; a failed one answers the user's gesture and is
/// shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocumentAdmissionRejected {
    /// Nothing to tell the user: the ticket was superseded or already consumed,
    /// or the requesting window closed while the worker was reading. The result
    /// is discarded without registering anything.
    Dropped(String),
    /// The read or the grant refused — an evicted iCloud Drive file, a missing
    /// or oversize file, a non-UTF-8 one. The user asked for this file and gets
    /// the reason in words they can act on.
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
    /// Resolve and ensure the one process config file, then focus it in the
    /// existing native Editor app. This host seam owns path authority; Settings
    /// reducers emit only `OpenConfigEditor` and never receive a filesystem path.
    pub(crate) fn ensure_and_open_config_editor_in_window(
        &mut self,
        wid: WindowId,
    ) -> Result<String, String> {
        self.ensure_and_open_config_editor_at_in_window(wid, None)
    }

    /// The App menu's "Open aterm.toml": the native editor on the one config
    /// file, in the frontmost window — the file itself, not a Settings page.
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
            ConfigEditorRevealDecision::AddAbsent { requested } => {
                buffer.minibuffer = crate::native_editor::Minibuffer::Inactive;
                let Some((edit, added_as)) = absent_key_edit(&snapshot.text, &requested) else {
                    // No line to add (a list of records), or a member of a
                    // list of records the file already has: say how, on a
                    // footer nothing else covers.
                    let note = match crate::native_config_language::config_key_record_root(
                        &requested,
                    ) {
                        Some(root) => format!(
                            "{requested} is not in aterm.toml yet. Add it inside a [[{root}]] \
                             record; completion offers its values."
                        ),
                        // A list of records itself is written record by
                        // record, never as one line.
                        None => format!(
                            "{requested} is not in aterm.toml yet. Add a [[{requested}]] record \
                             at the end of the file; completion offers its fields."
                        ),
                    };
                    state.status = Some(note.clone());
                    state.reveal_status = Some(note);
                    return Ok(());
                };
                // One undoable, unsaved edit, through the same path an
                // accepted completion takes.
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
                    buffer.minibuffer = crate::native_editor::Minibuffer::Inactive;
                    buffer.ensure_primary_visible(&text, buffer.viewport_lines());
                }
                let note = match added_as {
                    // Short enough for the 80x24 footer.
                    AbsentKeyAdded::AtDefault => format!(
                        "Added {requested} at its default, which changes nothing. Undo removes it."
                    ),
                    // No value spells "unset": the caret waits where one
                    // goes, and the line does not parse until it is there.
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
        let editor_event = match event {
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(_) | AppEvent::EditorSetSelection { .. } => true,
            // Keys reach an editor as `EditorInput`, which amacs's input
            // reducer reads. Of the native pages' text-field events an editor
            // takes only what no key spells: an input method's commit and
            // preedit, the menu's Select All, and a cancel.
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
        // A buffer read from a file whose every line break is CRLF holds LF,
        // and its save writes each LF back as CRLF: text typed or pasted into
        // it with CRLF line ends joins it as LF, or the save would write
        // CR CR LF (design §4.7).
        let held_line_ends = self
            .native_documents
            .persistence
            .observed(document)
            .is_some_and(|observed| {
                observed.encoding().eol == crate::native_document_io::LineEnding::CrLf
            })
            .then(|| crlf_held_insertion(event))
            .flatten();
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

        // The reducer checks every command it runs, from a key, M-x, a row
        // or a macro, against what this gesture carries
        // (`command_registry::editor_grant`). Set on every reduction, so a
        // control client's narrower grant never outlives its own input.
        self.editor_workspace.granted =
            crate::command_registry::editor_grant(self.person_gesture_refused());
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
            // A scroll, a viewport change or a preedit leaves the caret where
            // it is, and every other host event reveals it; an input's own
            // reduction says which it was, or that the reducer ignored it.
            let host_scroll = matches!(
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
            // An input resets the host state below once it is reduced, and
            // only when the reducer took it.
            let reduced_by_input = matches!(event, AppEvent::EditorInput(_));
            if !reduced_by_input {
                state.preedit.clear();
                state.preedit_caret = None;
                if !host_scroll {
                    state.config_completion_selected = 0;
                    state.config_completion_interaction = None;
                    // An edit or caret move: the reveal's note has been read.
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
                        let snapshot = store
                            .snapshot(document)
                            .ok_or_else(|| "editor document disappeared".to_string())?;
                        buffer.scroll_lines(&snapshot.text, *lines);
                        Ok(Vec::new())
                    }
                }
                _ => unreachable!("editor event filter above is exhaustive"),
            };
            // An input the reducer declined changed nothing, and goes back
            // to the face, which may give the key a meaning of its own.
            if follow == Follow::Ignored {
                return Ok(Some(EventResult::Bubble));
            }
            if reduced_by_input {
                state.preedit.clear();
                state.preedit_caret = None;
                if follow == Follow::Reveal {
                    state.config_completion_selected = 0;
                    state.config_completion_interaction = None;
                    // An edit or caret move: the reveal's note has been read.
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
                    // EditorWorkspace owns view-local editor state outside the
                    // generic NativeRuntime reducer, so advance the view revision
                    // here too: cache stamps and control-socket inspection must
                    // not keep an old caret/modeline.
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
        if let Some((seq, deltas)) = self.editor_workspace.take_last_commit(document) {
            self.edit_document_committed(document, source_view, seq);
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
        // After the commit is published: a reload installs newer text, which
        // must not be followed by this commit's stale one.
        if let Err(message) = self.adopt_disk_when_buffer_is_base(document) {
            self.set_document_recovery_status(document, Some(message));
        }
    }

    /// A COMMIT THAT PUTS BACK THE BYTES ON DISK IS CLEAN: an Undo (or any
    /// edit) whose text is exactly what aterm last saw in the file moves the
    /// checkpoint to it, as an unchanged refresh does
    /// ([`Self::install_stable_file_observation`]), so the title loses its
    /// dot, Save goes quiet, the tab reads clean, a close needs no save and
    /// the journal drops the draft instead of autosaving it. Only with no
    /// save in flight or owed and no disk conflict: an older save still
    /// landing could put other bytes on disk after the buffer read clean.
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

    /// [`Self::checkpoint_commit_back_on_disk`] for `document`'s current text,
    /// outside an edit: a buffer that is clean now says so to its views (the
    /// tab's mark, Save) as an edit's publication would.
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

    /// Whether a save of `document` is in flight or owed: an older save still
    /// landing could put other bytes on disk after the buffer read clean.
    pub(crate) fn document_save_owed(&self, document: DocumentId) -> bool {
        self.native_documents.persistence.save_in_flight(document)
            || self.native_documents.inflight.contains(&document)
            || self.native_documents.pending_saves.contains_key(&document)
    }

    /// Whether `snapshot`'s text is exactly the version of the file aterm
    /// admitted (loaded, saved, or accepted from disk): a buffer that is
    /// holds no local edits, conflict or not. The text is compared as the
    /// bytes a save of it writes (`encode_for_disk` with the document's
    /// mapping), so a file read with a byte order mark or CRLF line ends taken
    /// off is clean again once an Undo returns its text. Every commit of a
    /// dirty buffer asks, so the length a save would write is checked first,
    /// and only a text of the file's length is encoded and fingerprinted.
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

    /// Move `snapshot`'s document checkpoint to it: its text is the admitted
    /// file ([`Self::buffer_is_admitted_baseline`]), so it is clean.
    fn mark_buffer_the_admitted_file(
        &mut self,
        snapshot: &crate::document_store::DocumentSnapshot,
    ) -> Result<(), String> {
        let document = snapshot.id;
        self.document_store
            .checkpoint_ack(document, snapshot.seq)
            .map_err(|error| format!("could not mark the unchanged file durable: {error:?}"))?;
        // What the footer said of the edit ("Draft autosaved", "Added … Undo
        // removes it") is over: it says the buffer is the file again.
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

    /// A BUFFER BACK AT THE VERSION ATERM ADMITTED HAS NOTHING TO LOSE: when
    /// an edit (an Undo, most often) returns a document whose file changed on
    /// disk to the text aterm loaded or last saved, it has no local edits, so
    /// the disk's current version is taken as a clean document's is — the
    /// same fresh read Discard Changes and Reload from Disk makes — and the
    /// conflict, its "copy any local edits" guidance and the dirty mark go.
    /// Not while a save is in flight or owed (it owns what lands next), and
    /// not for a changed file binding, which only a reopen settles.
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
        // The re-seat fence follows the handoff attempt: no journal is
        // re-seated under a successor that may be restoring from it
        // (`DocumentJournalStore::set_reseat_fenced`).
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
                    // No worker (`proxy` is None, so the queue was never built)
                    // and this thread owns `App`: take the frame-sized budget. A
                    // refusal reaches `set_document_recovery_status` like any
                    // other journal failure.
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

    /// Pump one coalesced journal intent for the one channel slot just released
    /// by the worker. Idle/stale IDs cost no slot and are skipped; a successful
    /// enqueue or another `Full` verdict ends this wake, keeping UI work bounded.
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
        // A clean document holds no draft: its landing is the checkpoint's
        // pruning, never news for the footer.
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

    /// An append or checkpoint that failed while a re-seat was owed most likely
    /// failed because of it (a successor's image on disk): drive the re-seat now
    /// rather than at the next keystroke. A re-seat's own failure is never
    /// re-driven from here, so a journal that keeps refusing cannot spin.
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
            // Whatever the successor's image made fail while it stood there
            // (an append's "journal image changed") is over: say nothing stale.
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

    /// Whether one document's unsaved draft rides an update to the successor:
    /// the per-document half of the Relaunch preflight
    /// (`App::native_update_close_preflight`), asked when the update starts and
    /// again at its Commit.
    ///
    /// Carried means the journal store proved the head durable (file and
    /// directory fsync'd) with nothing writing or owed, the file is the one the
    /// draft was made against as far as this process has seen, and the image on
    /// disk, read back and checksummed, replays to exactly the editor's text
    /// ([`verify_carried_draft`]). The successor reopens the file, which replays
    /// the image; the file itself is never written.
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
        // The journal's base is the text the file was admitted as, a byte
        // order mark or CRLF line ends taken off: the successor's open
        // compares the same reading (`inspect_open` with the admitted text).
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

    /// Whether this process's update successor reopens its document tabs (and
    /// its Settings tabs, from the same layout, with their field drafts): the one
    /// predicate every carried piece of unsaved native state rides by. The
    /// successor restores the handed-over layout only when it has a window
    /// (`main_entry` takes the handoff layout only when not headless), so a
    /// headless process carries no draft. Nor does one off unix: the Windows lane
    /// replaces the app cold (spawn, then exit; `apply_staged_update_now`) and
    /// reopens nothing.
    pub(crate) fn successor_restores_document_tabs(&self) -> bool {
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

    /// Whether any draft-journal write is on the worker now (an update may not
    /// start or commit over one — `DocumentJournalStore::writes_in_flight`).
    pub(crate) fn document_journal_writes_in_flight(&self) -> bool {
        self.native_documents
            .journals
            .as_ref()
            .is_some_and(DocumentJournalStore::writes_in_flight)
    }

    /// Record a journal write that landed while an update handoff was parked
    /// ([`DocumentHostRuntime::journal_landed_while_parked`]). Every path a
    /// completion reduces through calls this — the worker's wakes and the
    /// inline lane alike.
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

    /// Whether a journal write landed after the parked successor may have read
    /// its image: the Commit check cannot vouch for what the successor restored.
    pub(crate) fn document_journal_landed_while_parked(&self) -> bool {
        self.native_documents.journal_landed_while_parked
    }

    /// After an update handoff rolls back, every journal the successor may
    /// have republished while it restored its documents is owed a re-seat, and
    /// each is driven now. With a worker this only enqueues (the rollback's
    /// event-loop half does no filesystem I/O); a re-seat still fenced by the
    /// attempt record runs at the next drive.
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
        // The attempt that was parked is over: a later landing orders against
        // no successor.
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

    /// Drive every journal owing work nothing is doing (a re-seat, or a head a
    /// failed append left behind) before an update asks whether each unsaved
    /// draft can be carried, so the update itself retries a transient failure.
    ///
    /// Only an unsaved draft's failed write is retried here; the update reports
    /// a journal that keeps refusing (`DraftCarryRefusal::AppendFailed` /
    /// `ReseatFailed`). Retrying any other journal (a saved document's re-seat
    /// after a rollback, say) would only put a write in flight when the
    /// preflight asks, holding the update ("Wait:") silently over a document
    /// with nothing to carry; that one retries at its document's next drive or
    /// the next rollback's re-seat.
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

    /// Point this App's draft journals at `root` — how a test gives an
    /// outgoing App and its successor the one journal directory two processes
    /// of one user share.
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

    /// Hold every journal append from now on instead of running it inline
    /// ([`DocumentHostRuntime::held_journal_appends`]).
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

    /// Run the oldest held append exactly as the document worker does — the
    /// worker's executor and patience, then the completion the worker's wake
    /// delivers (`finish_native_document_journal`).
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

    /// Hold every journal re-seat from now on instead of running it inline
    /// ([`DocumentHostRuntime::held_journal_reseats`]).
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

    /// Run the oldest held re-seat exactly as the document worker does — the
    /// worker's executor and patience, then the completion its wake delivers
    /// (`finish_native_document_journal_reseat`).
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

    /// Test seam: start a save of `document`'s head as the event loop's worker
    /// would, and hold it in flight; [`Self::land_held_document_save_for_test`]
    /// runs it. Puts a checkpoint between a close and its detach.
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

    /// Test seam: run a save [`Self::hold_document_save_for_test`] held and
    /// reduce its completion as `Wake` delivery does.
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
        // A clean document holds no draft: its landing is the checkpoint's
        // pruning, never news for the footer.
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

    fn set_document_disk_conflict(
        &mut self,
        document: DocumentId,
        conflict: Option<DiskConflictCause>,
    ) {
        if let Some(cause) = conflict {
            self.native_documents.disk_conflicts.insert(document, cause);
        } else {
            self.native_documents.disk_conflicts.remove(&document);
        }
        self.native_runtime
            .set_document_disk_conflict(document, conflict.is_some());
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
            EditorEffect::CloseView {
                why: CloseWhy::Plain,
            } => {
                if let Err(why) = self.close_editor_view(source_view) {
                    let status = self.editor_close_status(source_view, None, &why);
                    self.set_editor_view_status(source_view, &status);
                }
            }
            // An edit ticket's answer (`app_edit`): done saves, then closes
            // once the save is durable; abort discards, then closes.
            EditorEffect::CloseView {
                why: CloseWhy::Finish,
            } => self.finish_edit_view(source_view),
            EditorEffect::CloseView {
                why: CloseWhy::Abort,
            } => self.abort_edit_view(source_view),
            EditorEffect::KillBuffer { query } => {
                self.kill_open_editor_buffer(source_view, &query);
            }
            // The engine hands over a kill only for a person's input (the
            // Clipboard class `editor_grant` withholds from a control client).
            EditorEffect::KillToClipboard { text } => {
                if !crate::control::pbcopy(&text) {
                    aterm_log::warn!("editor: the kill did not reach the clipboard");
                }
            }
        }
        Ok(())
    }

    /// Why the editor command `event` names may not run on this gesture:
    /// a class it needs that the gesture's grant lacks
    /// (`command_registry::editor_grant`), said in the view's status line
    /// too. A completion row names the command it would run. `None` when it
    /// may run, or when `event` names no command.
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

    /// Close one Editor view as ⌘W closes it: focused first, then the leaf
    /// of a split or the whole tab, through the same document barrier (a
    /// changed draft is saved before the view goes; the close finishes when
    /// the save lands). The reason when it did not close.
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
        let focused = self.windows.get_mut(&wid).is_some_and(|ws| {
            ws.tab_set.switch_to(tab)
                && ws
                    .tab_set
                    .active_mut()
                    .is_some_and(|active| active.set_focus(view))
        });
        if !focused {
            return Err("Editor view could not take focus to close".to_string());
        }
        self.resync_active_or_window(wid);
        let split = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|active| active.root.len() > 1);
        let closed = if split {
            self.close_focused_mixed_leaf(wid)
        } else {
            self.close_active_native_tab(wid)
        };
        self.request_native_redraw(wid);
        closed
    }

    /// What to tell the person when closing the Editor `view` (whose buffer
    /// is `title`, or "this buffer") did not close it at once. A changed
    /// draft is saved before its view goes, and the close started that save:
    /// a split's leaf then closes by itself once the save lands, while a
    /// whole tab stays for the person to close again. Any other `why` is a
    /// refusal, said as it is.
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

    /// `kill-buffer` of an open buffer named as `switch-to-buffer` names one:
    /// its view closes as [`Self::close_editor_view`] closes it, and the
    /// person stays where they were.
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
        // Back to the view the command ran in.
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

    /// Resolve an exact minibuffer name to an already-open Editor tab in the
    /// source window. This deliberately changes tab focus rather than retargeting
    /// an `EditorApp`/`DocumentViewId`, so persistence grants, journals, and save
    /// generations remain attached to their original canonical documents.
    fn switch_to_open_editor_buffer(&mut self, source_view: crate::tab_model::ViewId, query: &str) {
        let (wid, index, instance, target_view, title) =
            match self.resolve_open_editor_buffer(source_view, query) {
                Ok(found) => found,
                Err(status) => {
                    self.set_editor_view_status(source_view, &status);
                    return;
                }
            };
        self.switch_tab_in(wid, index);
        self.set_editor_view_status(target_view, &format!("Switched to buffer {title}"));
        self.refresh_native_presentation(wid, instance, target_view);
        self.request_native_redraw(wid);
    }

    /// The one open Editor buffer in `source_view`'s window that `query`
    /// names exactly, by title or file URI: its window, tab index, app
    /// instance, view and title. The status that says why when none or more
    /// than one does.
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
                .any(|tab| tab.root.leaves().contains(&source_view))
                .then_some(*wid)
        });
        let Some(wid) = source_window else {
            return Err("Editor window is no longer available".to_string());
        };
        if query.is_empty() {
            return Err("Buffer name is required; no document changed".to_string());
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
            return Err(if matches.is_empty() {
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
            });
        };
        Ok((wid, *index, *instance, *target_view, title.clone()))
    }

    pub(crate) fn set_editor_view_status(&mut self, view: crate::tab_model::ViewId, message: &str) {
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
        let observed = self
            .refresh_document_file(document)
            .map_err(|error| format!("document refresh failed: {error}"))?;
        self.reduce_document_observation(document, &observed.text, observed.observed)
    }

    /// Read `document`'s file again through its grant. A document never saved
    /// whose file does not exist yet (an `edit … create=1`) reads "still
    /// missing" as no change rather than as a failure; once it has been saved,
    /// a missing file fails as for any document.
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
        // A fingerprint match is only a candidate for unchanged content. A
        // fresh read with different exact bytes follows the ordinary external
        // change path even when its fingerprint and metadata collide. Unknown
        // exact baselines also fail closed until explicitly re-observed. A
        // prior exact image may confirm this fresh read after a save rebind,
        // but a live save still owns admission of the current version.
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
                    // The target returned to the admitted baseline. Explicitly
                    // re-accept the fresh exact read, including one confirming
                    // the prior image after a fingerprint-only save rebind.
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
                    // A buffer that is the file again is clean, not "local
                    // edits" waiting for a Save.
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
                // The refreshed baseline may be the buffer's own text: then
                // there are no local edits to preserve.
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
                )?;
            }
            crate::native_document_io::FileWatchReduction::ConflictDirty { change } => {
                self.set_document_disk_conflict(document, Some(DiskConflictCause::Version));
                // The same words as the save refusal for the same conflict.
                let line = match change {
                    crate::native_document_io::VersionConflict::Existence => {
                        crate::native_app::DISK_EXISTENCE_RECOVERY
                    }
                    _ => crate::native_app::DISK_CONFLICT_RECOVERY,
                };
                self.set_document_recovery_status(document, Some(line.to_string()));
            }
            // The in-flight save's "Saving…" and then its own outcome are what
            // the person sees; the deferred refresh runs after it.
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
        // The fresh read is the buffer's text now, so its mapping is too
        // (unless it is exactly the old reading): a discard of a buffer held
        // as it was writes the file's own line ends back.
        self.native_documents
            .persistence
            .accept_reloaded_observation(document, observed, observed_text)
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
        self.set_document_disk_conflict(document, None);
        self.set_document_recovery_status(document, None);
        self.drive_document_journal(document)?;
        Ok(())
    }

    /// "Save failed: " and why, in the save words
    /// ([`crate::save_words`]), for a save of `document`'s file.
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

    pub(crate) fn save_document_checkpoint(
        &mut self,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
    ) -> Result<(), String> {
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

        // Headless tests have no event-loop proxy. Run the identical capability
        // and reducer path inline so they can prove the complete durable protocol.
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

    /// A save's completion, from its worker (`Wake::NativeDocumentSaved`) or
    /// inline: land it ([`Self::land_native_document_save`]), then let every
    /// `edit` view finishing on the document close once its done is durable,
    /// or stop finishing when the save did not land (`app_edit`).
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
            Result<crate::native_config_service::PreparedConfigObservation, String>,
        >,
    ) -> Result<(), String> {
        let landed = self.land_native_document_save(
            document,
            source_view,
            generation,
            saved_bytes,
            result,
            config_observation,
        );
        self.edit_document_saved(document, landed.as_ref().err().map(String::as_str));
        landed
    }

    fn land_native_document_save(
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
        self.note_document_settling(document);
        self.native_documents.inflight.remove(&document);
        let reduction = match self
            .native_documents
            .persistence
            .complete(document, generation, result)
        {
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
                Err(message)
            }
            // The log keeps the step's own account; the person reads why
            // in the words a failed save of aterm.toml gives
            // ([`crate::save_words`]).
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
                // The file holds the save's bytes, the buffer's text encoded
                // as the document maps it, not the text itself.
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
            // A newer save replaced this one; nothing failed for the person,
            // so the completion is logged by the caller and never painted.
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

    /// Whole-tab multi-document barrier. Every document leaf is prepared first; only a
    /// single successful batch verdict removes reference edges, so a later blocked leaf
    /// can never leave an earlier sibling half-detached.
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
            // A pending window or Quit plan owns teardown atomically; detaching
            // an older tab close here would apply part of it before its other
            // documents are durable.
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
                // As `close_focused_mixed_leaf` does for an immediate close: a
                // split left with terminals only gets its terminal projection
                // back, or the window's tabs and layouts disagree.
                self.refresh_aggregate_tab_presentation(close.window, close.tab);
                self.promote_terminal_projection_if_needed(close.window, close.tab);
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
        // Re-derive the plan on every gesture: the armed window stays
        // interactive, so a repeated close can follow a document opened into it.
        // A reused plan would report ready while that document is dirty and
        // strand its view in `view_store` with no tab. An unchanged window
        // yields an equal plan.
        let plan = self.document_close_plan_for_window(window)?;
        self.native_documents
            .pending_window_closes
            .insert(window, plan.clone());
        let ready = self.drive_document_close_plan(&plan, start_saves)?;
        if ready {
            self.commit_pending_window_document_shutdown(window)?;
        }
        Ok(ready)
    }

    /// Arm the process-wide clean-Quit barrier. It enumerates documents from
    /// `DocumentStore`, not only visible tabs, so a dirty suspended document is
    /// never skipped. Ready permits deferred-update application and event-loop
    /// exit; Waiting keeps every window and view alive.
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
        // Re-derived on every gesture, as the window barrier is: the app stays
        // interactive while a Quit's saves run, so a repeated Quit can find a
        // document opened since the first one, which a reused plan would skip.
        let plan = self.document_close_plan_for_quit();
        self.native_documents.pending_quit = Some(plan.clone());
        let ready = self.drive_document_close_plan(&plan, start_saves)?;
        if ready {
            self.native_documents.pending_quit = None;
        }
        Ok(ready)
    }

    /// Called after an async save reduction. A pending Quit wins over window
    /// closes. Returning `true` consumes a now-ready Quit plan; ready window ids
    /// are batch-detached and consumed, ready for `close_window_logical`.
    pub(crate) fn take_ready_document_shutdowns(
        &mut self,
    ) -> Result<(bool, Vec<WindowId>), String> {
        if let Some(plan) = self.native_documents.pending_quit.clone() {
            if self.document_close_plan_ready(&plan)? {
                // The native-app preflight ran before the async checkpoint; a
                // Settings draft (or another native blocker) may have appeared
                // since, so re-ask at the irreversible exit boundary.
                if !self.prepare_quit_native_shutdown()? {
                    return Ok((false, Vec::new()));
                }
                // Re-plan against the store's current documents, as the window
                // arm below does: the frozen `plan` misses any opened since.
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
            let Some(plan) = self
                .native_documents
                .pending_window_closes
                .get(&window)
                .cloned()
            else {
                continue;
            };
            if self.document_close_plan_ready(&plan)? {
                // Detach nothing until every native leaf is still ready: the UI
                // stays live while checkpoints run, and a new Settings draft must
                // keep the whole window and surface its recovery payload.
                if !self
                    .prepare_window_native_shutdown(window, crate::native_app::CloseScope::Window)?
                {
                    continue;
                }
                // `plan` froze when the barrier armed, so a document opened
                // since is absent from it; the editor reducer answers `Ready`
                // unconditionally, so the native sweep above misses its edits
                // too. Committing the frozen plan would strand that document's
                // view in `view_store` with no tab or window, dirty for good and
                // refusing every update apply. Re-plan against the window's
                // current leaves.
                let fresh = self.document_close_plan_for_window(window)?;
                if fresh != plan {
                    self.native_documents
                        .pending_window_closes
                        .insert(window, fresh.clone());
                    if !self.drive_document_close_plan(&fresh, true)? {
                        continue;
                    }
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
            // A save lifts a failure's mark, never an `edit` view's: the
            // program that asked still waits on it (`app_edit`).
            let edit_mark = self.edit_attention_in_tab_of(window, view);
            if let Some(tab) = self.windows.get_mut(&window).and_then(|state| {
                let index = state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| tab.root.leaves().contains(&view))?;
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

    /// Every native leaf, in every tab of every window, whose app instance
    /// `keep` accepts.
    fn native_views_where(
        &self,
        keep: impl Fn(crate::tab_model::AppInstanceId) -> bool,
    ) -> Vec<(
        WindowId,
        crate::tab_model::AppInstanceId,
        crate::tab_model::ViewId,
    )> {
        let keep = &keep;
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
                        keep(native.instance).then_some((*wid, native.instance, view))
                    })
                })
            })
            .collect()
    }

    /// At a park: let go of the documents nothing holds any more when a look
    /// is due — a document view closed or a Suspended document's write landed
    /// since the last one, or the recently-closed record that alone kept a
    /// document has expired. A `bool` and an `Option` test on every other park.
    /// The `edit` tickets' orphan clocks are read here too
    /// (`App::sweep_orphaned_edits_at_park`).
    pub(crate) fn release_documents_at_park(&mut self) {
        self.sweep_orphaned_edits_at_park();
        let host = &self.native_documents;
        if host.release_due || host.release_deadline_ms.is_some() {
            let now_ms = self.lat_epoch.elapsed().as_millis() as u64;
            self.release_documents_due(now_ms);
        }
    }

    /// [`Self::release_unheld_documents`] when a look is due at `now_ms`
    /// ([`Self::release_documents_at_park`]). Returns how many it let go.
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

    /// Let go of every document nothing holds any more (§5.10): no view and no
    /// app controller, its text the file's, no save or journal write in
    /// flight, owed or queued, no close or Quit plan in progress, and no
    /// recently-closed record a reopen could bring it back for — the first
    /// park after the last such record expires looks again. Each leaves with
    /// its saver, grant, journal entry and editor history, and this process
    /// lets go of its journal image (`DocumentJournalStore::forget`); opening
    /// the file again starts a new document from disk. Every Suspended
    /// document is clean (closing a dirty one's last view saves it first), so
    /// this drops no edit. Returns how many it let go.
    pub(crate) fn release_unheld_documents(&mut self, now_ms: u64) -> usize {
        if self.native_documents.pending_quit.is_some()
            || !self.native_documents.pending_window_closes.is_empty()
        {
            // The plan names its documents: look again once it is settled.
            return 0;
        }
        let reopenable = self.closed_recovery.document_uris(now_ms);
        let mut deadline: Option<u64> = None;
        let mut released = Vec::new();
        for document in self.document_store.document_ids() {
            if !self.document_unheld(document) {
                continue;
            }
            let uri = self.document_store.canonical_uri(document);
            // Records name a document by its canonical URI, which never changes.
            let kept_until = reopenable
                .iter()
                .filter(|(named, _)| Some(*named) == uri)
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
        let host = &mut self.native_documents;
        for document in &released {
            if let Some(grant) = host.persistence.forget(*document) {
                host.grants.forget(grant);
            }
            host.recovery_status.remove(document);
            host.disk_conflicts.remove(document);
            host.journal_retries.remove(*document);
            self.editor_workspace.forget(*document);
        }
        if let Some(journals) = host.journals.as_mut() {
            journals.forget(&released);
        }
        released.len()
    }

    /// Nothing but a recently-closed record could still hold `document`
    /// ([`Self::release_unheld_documents`]).
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

    /// Arm [`Self::release_unheld_documents`] for the next park when
    /// `document` has no view left: a write that landed may have been all
    /// that still held it.
    fn note_document_settling(&mut self, document: DocumentId) {
        if self.document_store.phase(document)
            == Some(crate::document_store::DocumentPhase::Suspended)
        {
            self.native_documents.release_due = true;
        }
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
        for (window, instance, view) in
            self.native_views_where(|instance| changed.contains(&instance))
        {
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
    }

    /// Open a local document in window `wid` with its caret on `line` (and
    /// `col`): the internal form of `open app editor` that a file citation's
    /// Cmd-click takes (`crate::path_link`). The wire verb keeps refusing a `#`
    /// fragment; the line travels here, beside the URI. A gesture's open: the
    /// two-phase admission ([`Self::request_document_tab_in_window`]), so the
    /// event loop never waits on the read, with the line carried to the install
    /// ([`DocumentHostRuntime::reveals`]). An already-open document is focused
    /// and its caret moved. Returns the admission's status line.
    pub(crate) fn open_document_tab_at(
        &mut self,
        wid: WindowId,
        kind: AppKind,
        uri: &str,
        line: Option<u32>,
        col: Option<u32>,
    ) -> Result<String, String> {
        let slot = (
            wid,
            DocumentAdmissionKey {
                uri: uri.to_string(),
                kind,
                edit: None,
            },
        );
        match line {
            Some(line) => {
                self.native_documents
                    .reveals
                    .insert(slot.clone(), LineReveal { line, col });
            }
            None => {
                self.native_documents.reveals.remove(&slot);
            }
        }
        let admission = self.request_document_tab_in_window(wid, kind, uri);
        match &admission {
            Ok(DocumentAdmission::Opened(_)) => {
                if let Some(reveal) = self.native_documents.reveals.remove(&slot) {
                    self.reveal_document_line(wid, reveal)?;
                }
            }
            Ok(DocumentAdmission::InFlight(_) | DocumentAdmission::Coalesced(_)) => {}
            Err(_) => {
                self.native_documents.reveals.remove(&slot);
            }
        }
        admission.map(|admission| admission.message(&slot.1))
    }

    /// Put the caret of `wid`'s focused editor on `reveal`'s line and column,
    /// scrolled into view: what a just-installed (or just-focused) document tab
    /// does for a file citation. A line past the end lands on the last line.
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

    /// [`Self::reveal_document_line`] for one exact view, focused or not: an
    /// `edit` ticket's view, which may open behind the tab the person is on.
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
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            // A Markdown view has no caret to move.
            return Ok(());
        };
        let buffer = state
            .buffer
            .as_mut()
            .ok_or_else(|| "the editor buffer disappeared".to_string())?;
        buffer.selections = vec![crate::native_editor::Selection::caret(offset)];
        buffer.primary = 0;
        buffer.minibuffer = crate::native_editor::Minibuffer::Inactive;
        buffer.ensure_primary_visible(&snapshot.text, buffer.viewport_lines());
        state.status = Some(format!("Line {}", reveal.line));
        for (window, instance, view) in self.document_native_views(document) {
            self.invalidate_native_view_cache(window, view, crate::native_app::DamageRegion::All);
            self.refresh_native_presentation(window, instance, view);
            self.request_native_redraw(window);
        }
        Ok(())
    }

    /// Two-phase open of one local document in the frontmost window (File ▸ Open
    /// Markdown / Open File in Editor). See [`Self::request_document_tab_in_window`].
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

    /// Two-phase open of one local document in one exact host window, the form
    /// every main-thread gesture uses (file drop, the Open… menu items).
    ///
    /// Phase one validates the window, kind and URI and hands the read to an
    /// `aterm-document-admit` worker; phase two (`Wake::DocumentAdmitted` →
    /// [`Self::complete_document_admission`]) mints the grant and installs the
    /// tab. The event loop never blocks on the read: an evicted iCloud Drive
    /// item downloads, and an unreachable volume times out, on the worker.
    /// Without an event-loop proxy (headless tests) the admission runs inline
    /// and returns [`DocumentAdmission::Opened`].
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

    /// The two-phase open of an `edit` ticket's file in window `wid`
    /// ([`crate::app_edit`]): the gesture path, under a key no gesture shares,
    /// so the read is never coalesced and the install always makes a fresh view.
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

    /// Synchronous open in the frontmost window: the control socket's `app open
    /// markdown|editor` verb (`app_control::open_app`), which answers its client
    /// with the installed view. Same caller rules as
    /// [`Self::open_document_tab_in_window`]; user gestures use
    /// [`Self::request_document_tab`].
    pub(crate) fn open_document_tab(&mut self, kind: AppKind, uri: &str) -> Result<String, String> {
        let wid = self
            .frontmost_window
            .ok_or_else(|| "no requesting window".to_string())?;
        self.open_document_tab_in_window(wid, kind, uri)
    }

    /// Open or focus a canonical local document in one exact host window,
    /// synchronously: the bounded read runs on the calling thread. The window
    /// identity is kept through grant and tab install, so a late focus change
    /// cannot redirect the document.
    ///
    /// For callers that need the installed tab to continue: session restore, the
    /// Markdown ▸ Edit effect, the Recovery tab's reopen, tests, and the Manual
    /// config editor (which binds the config service to the document in the
    /// same call). User gestures (`drop_file`, `choose_and_open_document`) go
    /// through [`Self::request_document_tab_in_window`] instead. On the main
    /// thread a dataless file fails fast as `NotDownloaded` (materialization is
    /// off there; `aterm_diskwalk::iopol`), but a network volume's timeout is
    /// waited out.
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
        Self::validate_document_request(kind, allow_config_symlinks)?;
        if !self.windows.contains_key(&wid) {
            return Err("requesting window disappeared".to_string());
        }
        let admitted = Self::admit_document_now(uri, allow_config_symlinks)?;
        self.install_admitted_document(wid, kind, uri, admitted, Placement::FocusOrNewTab)
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

    /// The blocking half of an open, on the calling thread: resolve, read,
    /// validate. Pure with respect to `App`, so a worker can run it.
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

    /// [`Self::admit_document_now`] with every admission option: an `edit …
    /// create=1` admits a file that does not exist yet as an empty text, and
    /// its first save creates it (`AdmitOptions::allow_missing`).
    fn admit_document_with(
        uri: &str,
        options: crate::native_document_host::AdmitOptions,
    ) -> Result<AdmittedDocument, DocumentOpenFailure> {
        crate::native_document_host::admit_local_file_opts(uri, options)
            .map_err(DocumentOpenFailure)
    }

    /// The admission options of an open under `key`: an ordinary document's,
    /// or, for an `edit` ticket that asked for `create=1`, one that admits a
    /// missing file.
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

    /// Phase one of the two-phase open. The caller has validated `wid` and the
    /// key. Returns without touching the filesystem when a proxy exists.
    fn begin_document_admission(
        &mut self,
        wid: WindowId,
        key: DocumentAdmissionKey,
    ) -> Result<DocumentAdmission, String> {
        // Coalescing is per window: a hit is this live window's own read in
        // flight. Another window's read of the file is a separate entry and
        // installs there (or is dropped if that window closed).
        if let Some(pending) = self.native_documents.admissions.get(&(wid, key.clone()))
            && pending.started.elapsed() < DOCUMENT_ADMISSION_STALE_AFTER
        {
            return Ok(DocumentAdmission::Coalesced(pending.ticket));
        }
        let options = self.admission_options(&key);
        let Some(proxy) = self.proxy.clone() else {
            // No event loop to report back to (headless tests): admit inline so
            // tests that read the tab straight back stay synchronous;
            // `complete_document_admission` has its own tests.
            let admitted = Self::admit_document_with(&key.uri, options);
            if key.edit.is_some() {
                let reply = self.finish_edit_admission(wid, key, admitted.map(Box::new));
                return Ok(DocumentAdmission::Opened(reply.unwrap_or_else(|why| why)));
            }
            return self
                .install_admitted_document(
                    wid,
                    key.kind,
                    &key.uri,
                    admitted?,
                    Placement::FocusOrNewTab,
                )
                .map(|installed| DocumentAdmission::Opened(installed.line()));
        };
        let ticket = self.mint_document_admission(wid, key.clone());
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
                let result = Self::admit_document_with(&worker_key.uri, options).map(Box::new);
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

    /// Record one admission as pending and mint its ticket. Split from
    /// [`Self::begin_document_admission`] so the completion state machine can be
    /// tested without a worker thread or an event loop.
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
            },
        );
        ticket
    }

    /// Phase two of the two-phase open, on the main thread, from
    /// `Wake::DocumentAdmitted`. Dropped when the pending entry holds another
    /// ticket (superseded) or the requesting window closed (the entry is
    /// retired, nothing registered); otherwise mints the grant and installs the
    /// tab as the synchronous opener does. `Err` says whether the outcome was
    /// dropped silently or failed for a reason the user should see.
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
        // Retire the entry before any early return, so a later open of the same
        // file never coalesces onto a result already consumed; its line reveal
        // goes with it.
        self.native_documents.admissions.remove(&slot);
        let reveal = self.native_documents.reveals.remove(&slot);
        let (wid, key) = slot;
        // An `edit` ticket's admission answers its control client, whatever
        // became of the ticket or the window (`app_edit`), and is never shown.
        if key.edit.is_some() {
            return self
                .finish_edit_admission(wid, key, result)
                .map_err(DocumentAdmissionRejected::Dropped);
        }
        // A window that closed while the worker read drops the result: nothing
        // is minted or registered.
        if !self.windows.contains_key(&wid) {
            return Err(DocumentAdmissionRejected::Dropped(format!(
                "requesting window closed before {} was admitted",
                key.uri
            )));
        }
        let admitted =
            *result.map_err(|failure| DocumentAdmissionRejected::Failed(failure.to_string()))?;
        let installed = self
            .install_admitted_document(wid, key.kind, &key.uri, admitted, Placement::FocusOrNewTab)
            .map_err(DocumentAdmissionRejected::Failed)?;
        if let Some(reveal) = reveal {
            self.reveal_document_line(wid, reveal)
                .map_err(DocumentAdmissionRejected::Failed)?;
        }
        Ok(installed.line())
    }

    /// Whether an admission for exactly this request is still pending (test /
    /// diagnostic seam).
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

    /// The main-thread tail of every open: mint (or upgrade) the grant, register
    /// the document with recovery and persistence, then create or focus the tab.
    /// No filesystem read happens here beyond the cheap binding revalidation of
    /// an already-minted grant.
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
        let access = if kind == AppKind::Editor {
            GrantAccess::ReadWrite
        } else {
            GrantAccess::ReadOnly
        };
        let granted = self
            .native_documents
            .grants
            .grant_admitted(admitted, access)
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
                        .journals
                        .unavailable()
                        .unwrap_or("document recovery is unavailable")
                        .to_string()
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
            .register(document, granted.grant.id, granted.observed, &granted.text)
            .map_err(|error| format!("document persistence failed: {error:?}"))?;
        if existing.is_some() {
            let _ = self.refresh_native_document(document)?;
        }

        // A gesture focuses the view already open; an `edit` ticket always owns
        // a fresh view of its own (`app_edit`), so a view the person opened is
        // never reused or focused.
        if placement == Placement::FocusOrNewTab
            && let Some((tab, view)) = self.document_view_in_window(wid, kind, document)
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.tab_set.switch_to(tab);
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
        // The tab active before the install, which a background placement
        // re-selects (`App::place_admitted_view`).
        let previously_active = self.windows.get(&wid).and_then(|ws| ws.tab_set.active_id());

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
            icon: Some(kind.icon()),
            indicators: crate::tab_model::TabIndicators::default(),
            conn: None,
            closable: true,
            // The tooltip carries the exact URI this tab was opened with; the
            // grant's canonical form (symlinks resolved, `/private/var/…` on
            // macOS) stays the internal identity for dedup and persistence.
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
        let (instance, tab, view) = install.map_err(|error| {
            aterm_log::warn!("could not install native document tab: {error:?}");
            "This document could not be reopened".to_string()
        })?;

        if let Err(error) = self.attach_document_view(kind, document, view) {
            self.rollback_document_tab(wid, tab);
            return Err(error);
        }
        if let Some(notice) = recovery_notice {
            // An update's successor recovering a draft is the update carrying
            // it (the outgoing process certified this journal before the update
            // started), so say that rather than suggest a crash.
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
        self.resync_active_or_window(wid);
        self.request_native_redraw(wid);
        if let Placement::NewTabAfter { ticket } = placement {
            self.place_admitted_view(wid, tab, ticket, previously_active);
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

    pub(crate) fn request_native_redraw(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_present = None;
        }
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
    }
}

/// Why aterm.toml could not be read to open it, in the save words for a
/// refused open ([`crate::save_words`]: a folder that is not there does not
/// exist, it did not go missing while saving); the reader's own account
/// goes to the log.
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

/// Create a missing config file without truncating an existing one, and return
/// the logical path the document-grant boundary uses.
///
/// Authority is resolved before the first mutation; creation goes through the
/// same locked compare-and-swap host as saves. Manual binds every existing
/// symlink component first, so dotfiles layouts may link the parent directory
/// or the file itself without a later swap redirecting creation or publication.
/// Ordinary document grants stay symlink-free.
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
        // Ordered: an empty file; a lost one makes the next open repeat the same
        // missing-file compare-and-swap.
        match crate::native_document_host::commit_atomic_bytes_ordered(&contents.baseline, b"") {
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
                // Said after "Couldn't open aterm.toml: " (Settings) or the
                // menu's own lead: why it could not be made.
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

/// `event`'s inserted text with each CRLF as LF, for a buffer that holds a
/// CRLF file's text with LF ([`App::dispatch_editor_event`]); `None` when it
/// inserts no CRLF.
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

/// Whether the bytes a save of `text` writes for a file held with
/// `encoding` (`encode_for_disk`) are `len` long, worked out without making
/// them: the text, the byte order mark, and one carriage return more per line
/// feed when every line break of the file was CRLF. Only a CRLF text whose
/// length could still match has its line feeds counted.
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

    /// Bound for "refused without blocking": opening a writerless FIFO or
    /// contending a held lock must refuse rather than park the UI thread. A real
    /// failure blocks forever, so this is a one-minute hang detector, never a
    /// latency budget.
    const REFUSES_WITHOUT_BLOCKING: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

    /// The length a save would write is told without making the bytes, and
    /// agrees with the bytes `encode_for_disk` makes for every mapping, a
    /// text one byte off either way included.
    #[test]
    fn encodes_to_len_agrees_with_encode_for_disk() {
        use crate::native_document_io::{DiskEncoding, LineEnding};
        for text in ["", "one", "one\ntwo\n", "\n\n\n", "a\r\nb\n"] {
            for bom in [false, true] {
                for eol in [LineEnding::Lf, LineEnding::CrLf] {
                    let encoding = DiskEncoding { bom, eol };
                    let len =
                        crate::native_document_host::encode_for_disk(text, encoding).len() as u64;
                    assert!(encodes_to_len(text, encoding, len), "{text:?} {encoding:?}");
                    assert!(
                        !encodes_to_len(text, encoding, len + 1),
                        "{text:?} {encoding:?}"
                    );
                    assert!(
                        len == 0 || !encodes_to_len(text, encoding, len - 1),
                        "{text:?} {encoding:?}"
                    );
                }
            }
        }
    }

    /// The running App's journal store opens on a worker: the slot is built
    /// while its opener is still held, and the first look takes the store that
    /// opener made, secret minted.
    #[test]
    fn the_journal_store_opens_on_a_worker_the_boot_does_not_wait_for() {
        let root = std::env::temp_dir().join(format!(
            "aterm-journal-opening-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let opener_root = root.clone();
        let mut slot = JournalSlot::opening(move || {
            release_rx
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .map_err(|_| "the opener was never released".to_string())?;
            DocumentJournalStore::for_test(opener_root)
        });
        assert!(
            !root.join(".journal-key").exists(),
            "nothing is opened before the held opener runs"
        );
        release_tx.send(()).unwrap();
        assert!(
            slot.as_mut().is_some(),
            "the first look settles to the store"
        );
        assert!(
            root.join(".journal-key").exists(),
            "the opener minted the secret"
        );
        assert_eq!(slot.unavailable(), None);
        fs::remove_dir_all(&root).ok();
    }

    /// A store that could not open is unavailable at every look, with the
    /// opener's reason.
    #[test]
    fn a_journal_store_that_could_not_open_says_why() {
        let mut slot = JournalSlot::opening(|| Err("no private state directory".to_string()));
        assert_eq!(slot.unavailable(), Some("no private state directory"));
        assert!(slot.as_ref().is_none());
        assert_eq!(
            slot.available_mut().err().as_deref(),
            Some("no private state directory")
        );
    }

    /// Tier-1 guard binding: the exact generic admission primitive used by the
    /// shipping document worker agrees with the derived bounded-queue model at
    /// every full/not-full decision, including the worker-owned fifth image and
    /// the deferred retry transition. The unbounded channel is the negative
    /// control: it accepts a fifth waiting job where the model refuses it.
    #[test]
    fn native_document_queue_admission_conforms_to_the_derived_model() {
        let model = aterm_models_gui::native_document_queue_model();
        let mut state = model.init_state();
        let mut documents = crate::test_document_store();
        let uri = "file:///queue-retry.md";
        let document = documents.open(uri.to_string(), "draft".to_string());
        let disk = documents.snapshot(document).unwrap();
        let journal_root = std::env::temp_dir().join(format!(
            "aterm-document-queue-retry-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let mut journals = DocumentJournalStore::for_test(journal_root.clone()).unwrap();
        let decision = journals.inspect_open(uri, disk.text.as_bytes()).unwrap();
        journals.initialize(decision, &disk, &disk).unwrap();
        let _ = documents.transact(
            document,
            disk.seq,
            vec![crate::document_store::TextEdit {
                range: disk.text.len()..disk.text.len(),
                insert: " newer".to_string(),
            }],
        );
        let desired = documents.snapshot(document).unwrap();
        journals.observe_commit(&desired).unwrap();
        let JournalEffect::Append {
            plan: first_plan, ..
        } = journals.next_effect(document).unwrap().unwrap()
        else {
            panic!("journal append fixture")
        };
        let mut retries = JournalRetrySet::default();
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<u8>(NATIVE_DOCUMENT_QUEUE_CAPACITY);

        try_enqueue_document_job(&sender, 0).expect("first capacity slot");
        assert!(model.fire("SubmitAccepted", &mut state));
        assert_eq!(receiver.recv().unwrap(), 0);
        assert!(matches!(
            native_document_queue_slot_available_wake(),
            crate::Wake::NativeDocumentQueueSlotAvailable
        ));
        assert!(model.fire("WorkerStarts", &mut state));
        assert!(model.fire("ConsumeDrainEdge", &mut state));

        for value in 1..=NATIVE_DOCUMENT_QUEUE_CAPACITY as u8 {
            assert!(model.action_enabled("SubmitAccepted", &state));
            try_enqueue_document_job(&sender, value).expect("capacity slot");
            assert!(model.fire("SubmitAccepted", &mut state));
        }
        assert!(!model.action_enabled("SubmitAccepted", &state));
        assert!(model.action_enabled("SubmitDeferred", &state));
        assert_eq!(
            try_enqueue_document_journal_job(
                &sender,
                u8::MAX,
                document,
                JournalAdmissionEffect::Append(first_plan.generation),
                &mut journals,
                &mut retries,
            )
            .unwrap(),
            JournalDriveOutcome::Deferred
        );
        assert!(retries.contains(document));
        assert!(model.fire("SubmitDeferred", &mut state));
        assert_eq!(state["queued"], 4);
        assert_eq!(state["executing"], 1);
        assert_eq!(state["retry_pending"], 1);
        assert!(model.check_invariant("BoundedFullImageJobs", &state));
        assert!(model.check_invariant("DeferredIntentRetained", &state));

        assert!(model.fire("WorkerCompletes", &mut state));
        assert_eq!(receiver.recv().unwrap(), 1);
        assert!(model.fire("WorkerStarts", &mut state));
        let JournalEffect::Append {
            plan: retry_plan, ..
        } = journals.next_effect(document).unwrap().unwrap()
        else {
            panic!("capacity release must replan without another edit")
        };
        assert_ne!(retry_plan.generation, first_plan.generation);
        assert_eq!(retry_plan.target_seq, desired.seq);
        assert_eq!(
            try_enqueue_document_journal_job(
                &sender,
                42,
                document,
                JournalAdmissionEffect::Append(retry_plan.generation),
                &mut journals,
                &mut retries,
            )
            .unwrap(),
            JournalDriveOutcome::Enqueued
        );
        assert!(!retries.contains(document));
        assert!(model.fire("RetryAccepted", &mut state));
        assert_eq!(state["queued"], 4);
        assert_eq!(state["executing"], 1);
        assert_eq!(state["retry_pending"], 0);
        assert!(model.check_invariant("BoundedFullImageJobs", &state));
        assert!(model.check_invariant("OpenSlotHasRetryEdge", &state));

        let (unbounded, retained) = std::sync::mpsc::channel();
        unbounded.send(0).unwrap();
        assert_eq!(retained.recv().unwrap(), 0, "one image is executing");
        for value in 1..=NATIVE_DOCUMENT_QUEUE_CAPACITY + 1 {
            unbounded
                .send(value)
                .expect("retired channel never refuses");
        }
        assert!(
            !model.action_enabled("SubmitAccepted", &state),
            "the bounded model refuses where the retired channel accepted"
        );
        let _ = fs::remove_dir_all(journal_root);
    }

    /// A fresh, empty per-process scratch directory named `name`.
    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fresh `name` directory holding `aterm.toml` = `source`, opened in Manual (the
    /// config editor) in window 0 through the shipping path:
    /// `(dir, path, app, (instance, view))`.
    fn open_config(
        name: &str,
        source: &str,
    ) -> (
        std::path::PathBuf,
        std::path::PathBuf,
        App,
        (crate::tab_model::AppInstanceId, crate::tab_model::ViewId),
    ) {
        let dir = fresh_dir(name);
        let path = dir.join("aterm.toml");
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        app.ensure_and_open_config_editor_path_in_window(WindowId(0), &path)
            .unwrap();
        let views = app.active_native_view(WindowId(0)).unwrap();
        (dir, path, app, views)
    }

    /// Type `text` into window `wid`'s focused native view.
    fn commit(app: &mut App, wid: WindowId, text: &str) {
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
        )
        .unwrap();
    }

    fn file_uri(path: &std::path::Path) -> String {
        // The shipping encoder: a hand-rolled `format!` is malformed on Windows
        // (drive letter and backslashes after the authority).
        crate::native_document_host::path_to_file_uri(path).unwrap()
    }

    fn key(key: Key, mods: Modifiers) -> InputEvent {
        InputEvent::Key {
            key,
            mods,
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        }
    }

    /// The chord that opens the editor's incremental search on this platform:
    /// `C-s`, except on Windows, where `C-s` is Save and isearch is `M-s`.
    fn isearch_key() -> InputEvent {
        let mods = if cfg!(windows) {
            Modifiers::ALT
        } else {
            Modifiers::CTRL
        };
        key(Key::Character('s'), mods)
    }

    fn drive_native(app: &mut App, event: InputEvent) {
        assert!(app.native_input_event(WindowId(0), &event));
    }

    fn enter_settings_draft(app: &mut App, wid: WindowId, view: crate::tab_model::ViewId) {
        // The font row's text entry (what "Other…" opens), then a name.
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new(format!(
                    "settings/set/{}",
                    crate::prefs::EDIT_FONT_FAMILY
                )),
                value: None,
            }),
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

    fn admission_fixture(name: &str, bytes: &[u8]) -> (std::path::PathBuf, String) {
        let dir = fresh_dir(&format!("aterm-document-admission-{name}"));
        let path = dir.join("dropped.md");
        fs::write(&path, bytes).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        (dir, uri)
    }

    fn admission_key(uri: &str, kind: AppKind) -> DocumentAdmissionKey {
        DocumentAdmissionKey {
            uri: uri.to_string(),
            kind,
            edit: None,
        }
    }

    /// Exactly what the `aterm-document-admit` worker produces, minus the thread
    /// and the proxy: the outcome for one minted ticket.
    fn worker_outcome(
        ticket: DocumentAdmissionTicket,
        wid: WindowId,
        key: &DocumentAdmissionKey,
    ) -> DocumentAdmissionOutcome {
        DocumentAdmissionOutcome {
            ticket,
            wid,
            key: key.clone(),
            result: App::admit_document_now(&key.uri, false).map(Box::new),
        }
    }

    /// The worker's payload, built the way the worker builds it and applied the
    /// way the `Wake::DocumentAdmitted` arm applies it, installs the same tab
    /// the synchronous opener would — and retires its pending entry.
    #[test]
    fn document_admission_outcome_round_trips_into_an_installed_tab() {
        let (dir, uri) = admission_fixture("round-trip", b"# dropped\n");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = admission_key(&uri, AppKind::Markdown);
        let ticket = app.mint_document_admission(wid, key.clone());
        assert_eq!(app.document_admission_pending(wid, &key), Some(ticket));

        let outcome = worker_outcome(ticket, wid, &key);
        let line = app
            .complete_document_admission(outcome)
            .expect("a live window installs the admitted document");
        assert!(line.starts_with("app markdown file://"), "{line}");
        assert_eq!(app.document_admission_pending(wid, &key), None);
        let (instance, _) = app
            .active_native_view(wid)
            .expect("the Markdown tab is active");
        assert_eq!(
            app.native_runtime.app(instance).map(|app| app.kind()),
            Some(AppKind::Markdown)
        );
        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(dir.join("dropped.md")).unwrap(),
        )
        .unwrap();
        assert!(app.document_store.id_for_uri(&canonical).is_some());
        let _ = fs::remove_dir_all(dir);
    }

    /// A second request for the same file while the first is still reading joins
    /// it: one ticket, one worker, one completion, one document.
    #[test]
    fn duplicate_in_flight_admission_is_coalesced_onto_the_first() {
        let (dir, uri) = admission_fixture("coalesce", b"# once\n");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = admission_key(&uri, AppKind::Editor);
        let ticket = app.mint_document_admission(wid, key.clone());
        assert_eq!(
            app.begin_document_admission(wid, key.clone()),
            Ok(DocumentAdmission::Coalesced(ticket)),
            "same uri, same kind: rides on the in-flight read"
        );
        assert_eq!(
            app.begin_document_admission(wid, key.clone()),
            Ok(DocumentAdmission::Coalesced(ticket))
        );
        // A different kind for the same file is a different tab, not a
        // duplicate; headless, it admits inline.
        let other = admission_key(&uri, AppKind::Markdown);
        assert!(
            matches!(
                app.begin_document_admission(wid, other.clone()),
                Ok(DocumentAdmission::Opened(_))
            ),
            "no proxy: the non-coalesced request admits inline"
        );
        assert_eq!(app.document_admission_pending(wid, &other), None);

        let outcome = worker_outcome(ticket, wid, &key);
        app.complete_document_admission(outcome).unwrap();
        assert_eq!(app.document_admission_pending(wid, &key), None);
        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(dir.join("dropped.md")).unwrap(),
        )
        .unwrap();
        let document = app.document_store.id_for_uri(&canonical).unwrap();
        assert_eq!(
            app.document_native_views(document).len(),
            2,
            "one document, one Editor view and one Markdown view — never a duplicate registration"
        );
        // The coalesced duplicates produced no second completion to apply: the
        // ticket has been consumed, and a replay is refused, not re-installed.
        let replay = worker_outcome(ticket, wid, &key);
        assert!(matches!(
            app.complete_document_admission(replay),
            Err(DocumentAdmissionRejected::Dropped(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }

    /// The requesting window closed while the worker was reading: the result is
    /// dropped cleanly — nothing registered, nothing installed, the pending entry
    /// retired so the file can be opened again afterwards.
    #[test]
    fn admission_completion_for_a_closed_window_is_dropped_cleanly() {
        let (dir, uri) = admission_fixture("closed-window", b"# orphan\n");
        let mut app = App::headless_for_test();
        let gone = WindowId(4242);
        assert!(!app.windows.contains_key(&gone));
        let key = admission_key(&uri, AppKind::Markdown);
        let ticket = app.mint_document_admission(gone, key.clone());
        let outcome = worker_outcome(ticket, gone, &key);
        let error = app
            .complete_document_admission(outcome)
            .expect_err("a closed window cannot receive the tab");
        assert!(
            matches!(error, DocumentAdmissionRejected::Dropped(_)),
            "a closed window drops silently, it does not alert: {error:?}"
        );
        assert_eq!(app.document_admission_pending(gone, &key), None);
        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(dir.join("dropped.md")).unwrap(),
        )
        .unwrap();
        assert_eq!(app.document_store.id_for_uri(&canonical), None);
        assert_eq!(app.native_documents.grants.id_for_uri(&canonical), None);
        // And the file is openable again in a window that exists.
        assert!(matches!(
            app.request_document_tab_in_window(WindowId(0), AppKind::Markdown, &uri),
            Ok(DocumentAdmission::Opened(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }

    /// Coalescing is per window: the same file from a second window while the
    /// first window's read is in flight is its own admission, so the second
    /// window gets its tab even when the first closes before its worker returns
    /// (that drop is `admission_completion_for_a_closed_window_is_dropped_cleanly`).
    #[test]
    fn same_file_from_another_window_is_its_own_admission_not_a_coalesce() {
        let (dir, uri) = admission_fixture("two-windows", b"# twice\n");
        let mut app = App::headless_for_test();
        let live = WindowId(0);
        let gone = WindowId(4243);
        let key = admission_key(&uri, AppKind::Markdown);
        let first = app.mint_document_admission(gone, key.clone());
        // The live window's request does not ride on the closed window's read:
        // in the headless harness it admits inline and installs its own tab.
        let second = app
            .request_document_tab_in_window(live, AppKind::Markdown, &uri)
            .unwrap();
        assert!(
            matches!(second, DocumentAdmission::Opened(_)),
            "a different window is a different admission: {second:?}"
        );
        assert_eq!(
            app.document_admission_pending(gone, &key),
            Some(first),
            "the first window's read is still pending, untouched"
        );
        assert!(app.active_native_view(live).is_some());
        // A duplicate gesture in the live window while its read is in flight is
        // the case that coalesces.
        let ticket = app.mint_document_admission(live, key.clone());
        assert_eq!(
            app.begin_document_admission(live, key.clone()),
            Ok(DocumentAdmission::Coalesced(ticket))
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// A ticket that no longer matches the pending entry (superseded by a fresh
    /// request, or already consumed) is ignored; a failed read retires the entry
    /// and reports the worker's error text.
    #[test]
    fn stale_and_failed_admissions_never_register_a_document() {
        let (dir, uri) = admission_fixture("stale", b"# stale\n");
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let key = admission_key(&uri, AppKind::Markdown);
        let old = app.mint_document_admission(wid, key.clone());
        let fresh = app.mint_document_admission(wid, key.clone());
        assert_ne!(old, fresh);
        let stale = worker_outcome(old, wid, &key);
        let error = app.complete_document_admission(stale).unwrap_err();
        assert!(
            matches!(error, DocumentAdmissionRejected::Dropped(_)),
            "{error:?}"
        );
        assert_eq!(
            app.document_admission_pending(wid, &key),
            Some(fresh),
            "a stale completion leaves the live admission pending"
        );

        let failed = DocumentAdmissionOutcome {
            ticket: fresh,
            wid,
            key: key.clone(),
            result: Err(DocumentOpenFailure(DocumentHostError::Io {
                stage: crate::native_document_io::AtomicSaveStage::Preflight,
                kind: None,
                message:
                    "dropped.md is evicted from iCloud Drive and must be downloaded in Finder first"
                        .to_string(),
            })),
        };
        let error = app.complete_document_admission(failed).unwrap_err();
        assert!(
            matches!(&error, DocumentAdmissionRejected::Failed(detail) if detail.contains("evicted from iCloud Drive")),
            "a read failure is the user's to see: {error:?}"
        );
        assert_eq!(app.document_admission_pending(wid, &key), None);
        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(dir.join("dropped.md")).unwrap(),
        )
        .unwrap();
        assert_eq!(app.document_store.id_for_uri(&canonical), None);
        assert!(app.active_native_view(wid).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_reducer_overscrolls_to_a_presentable_anchor_and_reverses_immediately() {
        let dir = fresh_dir("aterm-editor-scroll-anchor");
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

    /// Replays the in-flight save and panics with the save's own error text, so
    /// a failure names its stage and cause rather than a bare `unwrap` on `Err`.
    fn finish_inflight_save_for_test(
        app: &mut App,
        document: DocumentId,
        source_view: crate::tab_model::ViewId,
        pending: crate::native_document_host::PendingDocumentSave,
    ) {
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
        .unwrap_or_else(|error| panic!("inflight replay save failed: {error}"));
    }

    #[cfg(unix)]
    #[test]
    fn manual_open_refuses_writerless_journal_fifo_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let dir = fresh_dir("aterm-manual-journal-fifo");
        let config = dir.join("aterm.toml");
        fs::write(&config, "window_theme = \"dark\"\n").unwrap();
        let journal_root = dir.join("private-drafts");
        let mut app = App::headless_for_test();
        app.native_documents.journals = JournalSlot::for_test(journal_root.clone());

        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(&config).expect("canonical config path"),
        )
        .unwrap();
        let journal = app
            .native_documents
            .journals
            .as_ref()
            .unwrap()
            .path_for_uri(&canonical);
        let journal_c =
            std::ffi::CString::new(journal.as_os_str().as_bytes()).expect("journal FIFO path");
        // SAFETY: `journal_c` is a live NUL-terminated pathname and `mkfifo`
        // retains no pointer. The fixture directory is private to this test.
        assert_eq!(unsafe { libc::mkfifo(journal_c.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        app.ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
            .expect_err("Manual must refuse a special recovery-journal target");
        assert!(started.elapsed() < REFUSES_WITHOUT_BLOCKING);
        assert!(app.active_native_view(WindowId(0)).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manual_open_reports_busy_held_journal_lock_and_retries_cleanly() {
        let dir = fresh_dir("aterm-manual-journal-held");
        let config = dir.join("aterm.toml");
        fs::write(&config, "window_theme = \"dark\"\n").unwrap();
        let journal_root = dir.join("private-drafts");
        let mut app = App::headless_for_test();
        app.native_documents.journals = JournalSlot::for_test(journal_root.clone());
        let canonical = crate::native_document_host::path_to_file_uri(
            &fs::canonicalize(&config).expect("canonical config path"),
        )
        .unwrap();
        let journal = app
            .native_documents
            .journals
            .as_ref()
            .unwrap()
            .path_for_uri(&canonical);
        let lock_path = journal_root.join(format!(
            ".{}.lock",
            journal.file_name().unwrap().to_string_lossy()
        ));
        let held = crate::native_document_host::HeldAdvisoryLock::lock(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap(),
        )
        .unwrap();

        let started = std::time::Instant::now();
        let error = app
            .ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
            .expect_err("held journal lock must report busy");
        // A liveness bound, not a budget (see `REFUSES_WITHOUT_BLOCKING`); the
        // wait's size is pinned without a clock by the guard test named on
        // `native_document_journal::JournalLockPatience::EventLoop`.
        assert!(started.elapsed() < REFUSES_WITHOUT_BLOCKING);
        assert!(error.contains("busy"), "{error}");
        assert!(app.active_native_view(WindowId(0)).is_none());

        // Released by LOCK_UN (the guard's drop), not by the close alone: a child
        // another test is forking holds this descriptor until it execs, and the
        // retry below has only the event loop's 25 ms journal-lock budget.
        drop(held);
        app.ensure_and_open_config_editor_path_in_window(WindowId(0), &config)
            .expect("Manual retry succeeds after the lock is released");
        assert!(app.active_native_view(WindowId(0)).is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn focused_config_completion_enter_and_space_activate_its_semantic_button() {
        for (suffix, named_key) in [("enter", NamedKey::Enter), ("space", NamedKey::Space)] {
            let (dir, _, mut app, (instance, view)) = open_config(
                &format!("aterm-config-editor-focused-completion-{suffix}"),
                "win",
            );
            let wid = WindowId(0);
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
                "window_theme = \"auto\"",
                "{suffix}"
            );
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn config_file_creation_is_race_safe_and_never_truncates_existing_bytes() {
        let dir = fresh_dir("aterm-config-editor-ensure");
        let path = dir.join("nested/aterm.toml");

        // aterm-durable's counting seam: the empty file a missing config is created
        // as is only ordered, file and entry (the next open repeats a lost create),
        // and an existing one is not written at all.
        let recording = aterm_durable::seam::Recording::start();
        let first = ensure_config_file(&path).unwrap();
        #[cfg(unix)]
        let expected_create = [aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered];
        #[cfg(not(unix))]
        let expected_create = [aterm_durable::Flush::Ordered];
        assert_eq!(
            recording.take(),
            expected_create,
            "the created config file; Unix also flushes its entry"
        );
        assert!(first.is_absolute());
        assert_eq!(fs::read(&path).unwrap(), b"");

        let authored = b"# keep this comment\nfont_px = 15.0\n";
        fs::write(&path, authored).unwrap();
        let second = ensure_config_file(&path).unwrap();
        assert_eq!(
            recording.take(),
            [],
            "an existing config file is not written"
        );
        drop(recording);
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

        ensure_config_file(&path).unwrap_err();
        assert!(
            !path.parent().unwrap().exists(),
            "authority validation must precede directory creation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_file_creation_through_bound_parent_symlink_preserves_the_alias() {
        use std::os::unix::fs::symlink;

        let dir = fresh_dir("aterm-config-editor-ensure-symlink");
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

        let dir = fresh_dir("aterm-config-editor-final-link");
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
        commit(&mut app, wid, "theme = \"Nord\"\n");
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

    /// Swapping the config's final symlink, or its parent directory's, between
    /// open and Save is a conflict: the draft never lands behind the new link.
    #[cfg(unix)]
    #[test]
    fn manual_config_symlink_swap_conflicts_without_redirecting_save() {
        use std::os::unix::fs::symlink;

        // (swapped link, its first and second destinations, the logical path)
        for (link, first_dest, second_dest, logical) in [
            ("aterm.toml", "first.toml", "second.toml", "aterm.toml"),
            ("config", "first", "second", "config/aterm.toml"),
        ] {
            let dir = fresh_dir(&format!("aterm-config-editor-link-swap-{link}"));
            let behind = |dest: &str| dir.join(logical.replacen(link, dest, 1));
            let (first, second) = (behind(first_dest), behind(second_dest));
            for (path, text) in [
                (&first, "theme = \"Default\"\n"),
                (&second, "theme = \"Dracula\"\n"),
            ] {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, text).unwrap();
            }
            let alias = dir.join(link);
            let logical = dir.join(logical);
            symlink(first_dest, &alias).unwrap();

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
            commit(&mut app, wid, "theme = \"Nord\"\n");

            fs::remove_file(&alias).unwrap();
            symlink(second_dest, &alias).unwrap();
            app.save_document_checkpoint(document, view).unwrap_err();

            assert_eq!(fs::read_to_string(&first).unwrap(), "theme = \"Default\"\n");
            assert_eq!(
                fs::read_to_string(&second).unwrap(),
                "theme = \"Dracula\"\n"
            );
            assert_eq!(
                fs::read_link(&alias).unwrap(),
                std::path::PathBuf::from(second_dest)
            );
            assert_eq!(app.document_store.dirty(document), Some(true));
            let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
                panic!("Manual editor remains installed after {link} conflict");
            };
            assert!(state.status.is_some(), "conflict must be visible");
            assert_eq!(
                app.native_config_service.snapshot().text.as_ref(),
                "theme = \"Default\"\n",
                "a conflicted draft must leave the admitted live config unchanged"
            );
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn config_editor_uses_one_native_document_tab_without_spawning_a_session() {
        let dir = fresh_dir("aterm-config-editor-native");
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
        let dir = fresh_dir("aterm-config-editor-invalid");
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

        app.save_document_checkpoint(document, view).unwrap_err();
        assert_eq!(fs::read(&path).unwrap(), b"");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("config editor view must remain installed");
        };
        assert!(state.status.is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn manual_save_reports_busy_held_file_lock_preserves_draft_and_retries() {
        let (dir, path, mut app, (instance, view)) =
            open_config("aterm-config-editor-held-save-lock", "");
        let wid = WindowId(0);
        let document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "theme = \"Dracula\"\n");
        let lock_path = dir.join(".aterm.toml.aterm-write.lock");
        let held = crate::native_document_host::HeldAdvisoryLock::lock(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
                .unwrap(),
        )
        .unwrap();

        let started = std::time::Instant::now();
        let error = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(started.elapsed() < REFUSES_WITHOUT_BLOCKING);
        // In the words a lock another writer holds gets, never the writer's
        // account of it.
        assert_eq!(
            error,
            format!(
                "Save failed: {}",
                crate::save_words::words(
                    crate::save_words::Speaker::Aterm,
                    &path,
                    &crate::save_words::Failure::Write {
                        stage: crate::native_document_io::AtomicSaveStage::Preflight,
                        cause: Some(crate::native_document_io::SaveCause {
                            kind: std::io::ErrorKind::WouldBlock,
                            touch: crate::native_document_io::SaveTouch::Lock,
                        }),
                    },
                )
            )
        );
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

        // Released by LOCK_UN (the guard's drop), not by the close alone: a fork's
        // copy outlives the close until its exec (up to 523 ms measured), and the
        // retry's preflight gives up after 500 ms.
        drop(held);
        app.save_document_checkpoint(document, view)
            .expect("Manual Save retry succeeds after lock release");
        assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Dracula\"\n");
        assert_eq!(app.document_store.dirty(document), Some(false));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_startup_manual_save_binds_and_recovers_the_config_service() {
        let dir = fresh_dir("aterm-config-editor-malformed-startup");
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
        commit(&mut app, wid, "theme = \"Nord\"\n");
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
        let dir = fresh_dir("aterm-config-editor-exact-observation");
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
        let dir = fresh_dir("aterm-config-editor-equivalent-generation");
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

        commit(&mut app, wid, "# local draft\n");
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
        assert!(!app.native_documents.disk_conflicts.contains_key(&document));

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

        app.save_document_checkpoint(document, view).unwrap_err();

        // A watcher can arrive before the person's next Save. Its actual
        // identical read must not turn the safe rebind into a disk conflict.
        app.refresh_open_config_editor_observation(&save_observation)
            .unwrap();

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
        assert!(!app.native_documents.disk_conflicts.contains_key(&document));
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

        app.save_document_checkpoint(document, view).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), draft.text.as_ref());
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn byte_identical_symlink_recreation_does_not_rebind_manual_capability() {
        use std::os::unix::fs::symlink;

        let dir = fresh_dir("aterm-config-editor-equivalent-relink");
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
        commit(&mut app, wid, "# local draft\n");
        let draft = app.document_store.snapshot(document).unwrap();

        fs::rename(&logical, &old_link).unwrap();
        symlink("target.toml", &logical).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&logical, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&observation)
            .unwrap_err();

        assert_eq!(
            app.document_store.snapshot(document).unwrap().text,
            draft.text
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert!(app.native_documents.disk_conflicts.contains_key(&document));
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("Manual editor remains installed");
        };
        assert!(editor.disk_conflict);
        assert_eq!(fs::read_to_string(&target).unwrap(), baseline_text);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_toml_observation_reaches_manual_but_not_live_config() {
        let dir = fresh_dir("aterm-config-editor-malformed-observation");
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
    fn config_publication_preserves_dirty_manual_drafts() {
        // (A clean Manual takes external bytes: `exact_external_observation_…`.)
        let (dir, path, mut app, (instance, _)) =
            open_config("aterm-config-editor-publication", "font_px = 12.0\n");
        let wid = WindowId(0);
        let document = app.native_runtime.document_id(instance).unwrap();

        commit(&mut app, wid, "# local draft\n");
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
        assert_eq!(
            editor.recovery_status.as_deref(),
            Some(crate::native_app::DISK_CONFLICT_RECOVERY)
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// NumLock off, keypad 1 is KP_End. Through the real key path
    /// (`App::on_key` -> `keymap::build_key_input` -> `App::input`) the editor
    /// receives `NumpadEnd` (the identity DECKPAM and kitty need on the PTY
    /// side) and the native reducer folds it onto End.
    #[test]
    fn numlock_off_keypad_end_moves_the_editor_caret_to_the_line_end() {
        let (dir, _, mut app, (_, view)) = open_config("aterm-config-editor-keypad-end", "win");
        let wid = WindowId(0);
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &crate::native_editor::Selection { anchor: 0, head: 0 },
            "a freshly opened document starts at the top"
        );

        app.on_key(
            wid,
            aterm_window::event::KeyEvent::synthetic_for_test(
                aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::Numpad1),
                aterm_window::keyboard::Key::Named(aterm_window::keyboard::NamedKey::End),
                None,
                aterm_window::keyboard::KeyLocation::Numpad,
                aterm_window::event::ElementState::Pressed,
                false,
            ),
        );
        assert_eq!(
            editor_buffer(&app, view).primary_selection(),
            &crate::native_editor::Selection { anchor: 3, head: 3 },
            "KP_End is End to the editor: the caret is at the line end"
        );
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
            let (dir, _, mut app, (instance, view)) = open_config(
                &format!("aterm-config-editor-post-completion-{case}"),
                prefix,
            );
            let wid = WindowId(0);
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

            commit(&mut app, wid, typed);
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
    fn presented_config_assist_does_not_steal_enter_or_vertical_motion() {
        let source = "theme = \"Default\"\ncursor_blink = true\n";
        let (dir, _, mut app, (instance, view)) =
            open_config("aterm-config-editor-ordinary-keys", source);
        let wid = WindowId(0);
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
    fn exact_analysis_drives_tab_before_any_assist_render() {
        let (dir, _, mut app, (instance, view)) =
            open_config("aterm-config-editor-unrendered-tab", "window_theme = ");
        let wid = WindowId(0);
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

        // Tab held down: its auto-repeats neither page the list on nor
        // indent the buffer behind it.
        for _ in 0..3 {
            drive_native(
                &mut app,
                InputEvent::Key {
                    key: Key::Named(NamedKey::Tab),
                    mods: Modifiers::empty(),
                    base_layout: None,
                    layout_text: None,
                    event_type: KeyEventType::Repeat,
                },
            );
        }
        let held = app.document_store.snapshot(document).unwrap();
        assert_eq!((held.seq, &held.text), (before.seq, &before.text));
        assert_eq!(editor_buffer(&app, view).primary_selection().head, caret);
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view");
        };
        assert_eq!(state.config_completion_interaction, Some(context));
        assert_eq!(state.config_completion_selected, 0);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ctrl_space_reopens_dismissed_candidate_and_help_only_contexts() {
        for (suffix, source) in [
            ("candidates", "window_theme = "),
            ("help", "matrix_rain.density = 12"),
        ] {
            let (dir, _, mut app, (instance, view)) =
                open_config(&format!("aterm-config-editor-reopen-{suffix}"), source);
            let wid = WindowId(0);
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
            // Escape dismisses only this exact context: nothing renders for it.
            assert!(app.prepare_native_input_scratch(wid));
            assert!(
                app.retained_native_leaf_artifact(wid, view, false)
                    .unwrap()
                    .compiled
                    .semantics
                    .iter()
                    .all(|node| !node.key.as_str().starts_with("editor/config-completion/")),
                "{suffix}: a dismissed context renders no completions"
            );
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
        let source = "window_theme = ";
        let assist = crate::native_config_language::assist(source, source.len());
        assert!(assist.completions.len() > 1);
        let second = &assist.completions[1];
        let mut expected = source.to_string();
        expected.replace_range(second.replacement.clone(), &second.insertion);
        let (dir, _, mut app, (_, view)) =
            open_config("aterm-config-editor-keyboard-completion", source);
        let wid = WindowId(0);
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
        let source = "window_theme = ";
        let (dir, _, mut app, (instance, view)) =
            open_config("aterm-config-editor-caret-context-selection", source);
        let wid = WindowId(0);
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
        // The caret moved off the context the list was opened for, onto a
        // name nobody is typing: the list waits to be asked for again, and
        // the stale selection paints nowhere.
        assert!(selected.is_empty(), "{selected:?}");

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
            let source = "win";
            let (dir, _, mut app, (_, view)) = open_config(
                &format!("aterm-config-editor-chord-assist-{suffix}"),
                source,
            );
            let wid = WindowId(0);
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
                drive_native(&mut app, isearch_key());
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
        let (dir, _, mut app, (_, view)) =
            open_config("aterm-config-editor-minibuffer-assist", "win");
        let wid = WindowId(0);
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
    fn settings_manual_handoff_reveals_an_existing_key_and_adds_an_absent_one() {
        let source = format!("{}shell = \"/bin/zsh\"\n", "# spacer\n".repeat(500));
        let (dir, path, mut app, (_, original_view)) =
            open_config("aterm-config-editor-target-handoff", &source);
        let wid = WindowId(0);

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
        // Never a Search for a key the file does not have: the key is added
        // where TOML reads it, at its default, as one unsaved edit, with the
        // value selected for typing over.
        let document = app.native_runtime.document_id(instance).unwrap();
        let text = app.document_store.snapshot(document).unwrap().text;
        let added = crate::native_config_language::config_key_source_range(&text, "gpu")
            .expect("gpu is in the buffer now");
        assert!(text.starts_with(&source), "nothing else moved: {text:?}");
        let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
            panic!("config editor view")
        };
        let buffer = state.buffer.as_ref().unwrap();
        assert_eq!(
            buffer.minibuffer,
            crate::native_editor::Minibuffer::Inactive
        );
        assert_eq!(buffer.primary_selection().range(), added);
        assert!(
            state
                .status
                .as_deref()
                .unwrap()
                .starts_with("Added gpu at its default"),
            "{:?}",
            state.status
        );
        assert_eq!(state.reveal_status, state.status, "the footer says it");
        assert!(app.native_runtime.config_editor_enabled(instance));
        let _ = fs::write(&path, "");
        let _ = fs::remove_dir_all(dir);
    }

    /// Settings' "Open aterm.toml at" a key with no value that means unset
    /// (the shell) adds its line with no value, the caret where the value
    /// goes, and says to type one — never "at its default"; Save waits for
    /// the value, and Undo takes the line back out. A member of a list of
    /// records comes in inside a new `[[record]]` the same way.
    #[test]
    fn opening_a_key_with_no_default_awaits_its_value() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-config-editor-needs-value-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        let source = "agents_auto_prime = false\n";
        fs::write(&path, source).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        for (key, line) in [
            ("shell", "shell = "),
            (
                "sparkle_words.custom.graphic.collection",
                "[[sparkle_words.custom]]\ngraphic.collection = ",
            ),
        ] {
            app.ensure_and_open_config_editor_path_at_in_window(
                wid,
                &path,
                Some(&crate::native_app::ConfigEditorTarget::Key(key.to_string())),
            )
            .unwrap();
            let (instance, view) = app.active_native_view(wid).unwrap();
            let document = app.native_runtime.document_id(instance).unwrap();
            let text = app.document_store.snapshot(document).unwrap().text;
            let at = text.find(line).unwrap_or_else(|| panic!("{key}: {text:?}"));
            let AppViewState::Editor(state) = app.native_runtime.view_state(view).unwrap() else {
                panic!("config editor view")
            };
            let buffer = state.buffer.as_ref().unwrap();
            assert_eq!(
                buffer.primary_selection().range(),
                at + line.len()..at + line.len(),
                "{key}: the caret waits at the value"
            );
            let status = state.status.clone().unwrap();
            assert!(
                status.contains(&format!("type a value for {key}"))
                    || status.contains(&format!("Type a value for {key}")),
                "{status}"
            );
            assert!(!status.contains("default"), "{status}");
            assert!(
                app.native_runtime
                    .config_editor_save_error(document)
                    .is_some(),
                "{key}: Save waits for a value"
            );
            app.dispatch_editor_event(
                instance,
                view,
                &AppEvent::EditorCommand(crate::native_editor::EditorCommand::Undo),
            )
            .unwrap();
            assert_eq!(
                &*app.document_store.snapshot(document).unwrap().text,
                source,
                "{key}: Undo takes the line back out"
            );
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// The edit that adds an absent key puts it where TOML reads it: a
    /// top-level key ahead of the first table, a dotted key in its table.
    #[test]
    fn an_absent_key_is_added_where_toml_reads_it() {
        let source = "agents_auto_prime = false\n[update]\nenabled = false\n";
        let apply = |key: &str| {
            let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
            let mut text = source.to_string();
            text.replace_range(edit.replacement.clone(), &edit.insertion);
            (text, added_as)
        };
        let (text, added_as) = apply(crate::prefs::EDIT_SCROLLBACK);
        assert_eq!(added_as, AbsentKeyAdded::AtDefault);
        let key_at = text.find("scrollback").unwrap();
        assert!(
            key_at < text.find("[update]").unwrap(),
            "a root key stays out of [update]: {text}"
        );
        let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        assert!(
            parsed.get(crate::prefs::EDIT_SCROLLBACK).is_some(),
            "{text}"
        );
        let (text, _) = apply("update.auto_apply");
        let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        assert!(parsed["update"]["auto_apply"].as_bool().is_some(), "{text}");
        assert_eq!(parsed["update"]["enabled"].as_bool(), Some(false));
        // A table or a map comes in as its empty table, the caret under its
        // header where its entries go: at the end of the file, never inside
        // the table the file ends with ([update] here, [machine] in a lab's).
        for key in ["harness", "keybindings"] {
            let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
            assert_eq!(added_as, AbsentKeyAdded::Table, "{key}");
            let mut text = source.to_string();
            text.replace_range(edit.replacement.clone(), &edit.insertion);
            let caret = edit.replacement.start + edit.post_insert_selection.start;
            assert!(
                text[..caret].ends_with(&format!("\n[{key}]\n")),
                "{key}: the caret is under the header: {text:?}"
            );
            // What is typed there is the table's, not [update]'s.
            let mut typed = text.clone();
            let entry = if key == "harness" {
                "enabled = false\n"
            } else {
                "\"ctrl+t\" = \"new_tab\"\n"
            };
            typed.insert_str(caret, entry);
            let parsed: aterm_toml::Value = aterm_toml::from_str(&typed).unwrap();
            assert!(
                parsed[key].as_table().is_some_and(|t| t.len() == 1),
                "{typed}"
            );
            assert_eq!(
                parsed["update"].as_table().map(|t| t.len()),
                Some(1),
                "{typed}"
            );
            assert!(
                crate::app_config::Config::parse(&text).is_ok(),
                "{key}: an empty table loads: {text}"
            );
        }
        // A list of records itself is the person's to write, record by record.
        assert!(absent_key_edit(source, "sparkle_words.custom").is_none());
    }

    /// A NUMBER COMES IN AT ITS DEFAULT AS SETTINGS WRITES IT
    /// (`prefs::written_number`): a whole default carries no `.0`, whether
    /// the key has a Settings row (`window_padding`) or only Manual edits it
    /// (`output_streak.idle_secs`, which came in as `10.0`), and a fraction
    /// keeps its digits.
    #[test]
    fn an_absent_number_comes_in_at_its_default_as_settings_writes_it() {
        let source = "agents_auto_prime = false\n";
        for (key, line) in [
            (crate::prefs::EDIT_WINDOW_PADDING, "window_padding = 12"),
            (crate::prefs::EDIT_LINE_HEIGHT, "line_height = 1"),
            ("output_streak.idle_secs", "idle_secs = 10"),
            ("output_streak.intensity", "intensity = 1"),
            (crate::prefs::EDIT_WALLPAPER_DIM, "wallpaper_dim = 0.3"),
        ] {
            let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
            let mut text = source.to_string();
            text.replace_range(edit.replacement.clone(), &edit.insertion);
            assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{key}: {text}");
            assert!(text.lines().any(|at| at == line), "{key}: {text}");
            crate::app_config::Config::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
    }

    /// OPENING AN ABSENT KEY NEVER WRITES WORDS AS A VALUE. Over every key
    /// the schema knows: an edit at the default loads, draws no finding the
    /// file did not have, and writes the value that resolves as the key's
    /// absence does (a Settings row's unset value, else the documented
    /// default: disk.auto_free_gib is 10, disk.apply false, never the consent
    /// to remove files); otherwise the key's line comes in with no value and
    /// the caret where it goes. Negative controls from the lab: the shell,
    /// its arguments, the sprite, the font keys, the lexicon, the wallpaper
    /// and the theme have no such value and never come in "at their default";
    /// a member of a list of records comes in inside a `[[record]]`, never
    /// under plain tables.
    #[test]
    fn an_absent_key_is_added_at_its_default_or_awaits_a_value() {
        let source = "agents_auto_prime = false\n[update]\nenabled = false\n";
        let apply = |key: &str| {
            let (edit, added_as) = absent_key_edit(source, key).expect("an edit");
            let mut text = source.to_string();
            text.replace_range(edit.replacement.clone(), &edit.insertion);
            let caret = edit.replacement.start + edit.post_insert_selection.start;
            (text, added_as, caret)
        };
        let (text, added_as, _) = apply("disk.auto_free_gib");
        assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
        let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["disk"]["auto_free_gib"].as_integer(),
            i64::try_from(aterm_agent::harness::disk::Config::default().auto_free_gib).ok(),
            "{text}"
        );
        let (text, added_as, _) = apply("disk.apply");
        assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
        let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        assert_eq!(parsed["disk"]["apply"].as_bool(), Some(false), "{text}");
        // A text's default is its text, never the empty example the harness
        // refuses ("expected a non-empty text").
        let (text, added_as, _) = apply("harness.continue_text");
        assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{text}");
        let parsed: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["harness"]["continue_text"].as_str(),
            Some(
                aterm_agent::supervise::SupervisorConfig::default()
                    .continue_text
                    .as_str()
            ),
            "{text}"
        );
        for key in [
            crate::prefs::EDIT_GPU,
            crate::prefs::EDIT_SCROLLBACK,
            "matrix_rain.hue",
        ] {
            let (text, added_as, _) = apply(key);
            assert_eq!(added_as, AbsentKeyAdded::AtDefault, "{key}: {text}");
        }
        for key in [
            "shell",
            "shell_args",
            crate::prefs::EDIT_CURSOR_NYAN_SPRITE,
            crate::prefs::EDIT_FONT_FAMILY,
            crate::prefs::EDIT_FONT_PX,
            "font_features",
            crate::prefs::EDIT_FALLBACK_FONTS,
            "font_variation",
            "sparkle_words.lexicon",
            crate::prefs::EDIT_WALLPAPER,
            crate::prefs::EDIT_THEME,
        ] {
            let (text, added_as, caret) = apply(key);
            assert_eq!(
                added_as,
                AbsentKeyAdded::NeedsValue { record: None },
                "{key}: {text}"
            );
            let line_start = text[..caret].rfind('\n').map_or(0, |at| at + 1);
            let line_end = text[caret..].find('\n').map_or(text.len(), |at| caret + at);
            assert!(
                text[line_start..caret].ends_with("= ") && caret == line_end,
                "{key}: the caret waits at the value: {text:?}"
            );
            assert!(
                crate::native_config_language::analyze(&text).has_errors(),
                "{key}: the line does not parse until a value is typed, so Save waits"
            );
        }
        for key in [
            "sparkle_words.custom.graphic.collection",
            "sparkle_words.custom.burst.chance",
            "sparkle_words.custom.words",
        ] {
            let root = crate::native_config_language::config_key_record_root(key).unwrap();
            let (text, added_as, caret) = apply(key);
            assert_eq!(
                added_as,
                AbsentKeyAdded::NeedsValue { record: Some(root) },
                "{key}: {text}"
            );
            assert!(text.contains(&format!("[[{root}]]\n")), "{key}: {text}");
            assert!(text[..caret].ends_with(" = "), "{key}: {text:?}");
            // With a value typed it is a record of the list the reader wants.
            let mut typed = text.clone();
            typed.insert_str(caret, "\"x\"");
            let parsed: aterm_toml::Value = aterm_toml::from_str(&typed).unwrap();
            let mut node = &parsed;
            for part in root.split('.') {
                node = &node[part];
            }
            assert!(
                node.as_array().is_some_and(|records| records.len() == 1),
                "{key}: {typed}"
            );
        }
        // A file that already has records: which one is the person's call.
        assert!(
            absent_key_edit(
                "[[sparkle_words.custom]]\nwords = [\"ship\"]\n",
                "sparkle_words.custom.graphic.collection"
            )
            .is_none()
        );
        // Every key: an edit at the default loads and draws no new finding;
        // any other added line is a value-less line, never a value.
        let findings = |text: &str| {
            crate::native_config_language::analyze(text)
                .diagnostics
                .len()
        };
        for entry in crate::native_config_language::config_schema() {
            let Some((edit, added_as)) = absent_key_edit(source, entry.key) else {
                continue;
            };
            let mut text = source.to_string();
            text.replace_range(edit.replacement.clone(), &edit.insertion);
            match added_as {
                AbsentKeyAdded::AtDefault => {
                    assert!(
                        crate::app_config::Config::parse(&text).is_ok(),
                        "{}: loads\n{text}",
                        entry.key
                    );
                    assert!(
                        findings(&text) <= findings(source),
                        "{}: no new finding\n{text}",
                        entry.key
                    );
                }
                AbsentKeyAdded::NeedsValue { .. } => {
                    let caret = edit.replacement.start + edit.post_insert_selection.start;
                    assert!(
                        text[..caret].ends_with("= "),
                        "{}: no value written\n{text}",
                        entry.key
                    );
                }
                AbsentKeyAdded::Table => {
                    assert!(
                        crate::app_config::Config::parse(&text).is_ok()
                            && findings(&text) <= findings(source),
                        "{}: an empty table changes nothing\n{text}",
                        entry.key
                    );
                }
            }
        }
    }

    #[test]
    fn f8_cycles_offscreen_problems_and_keeps_the_full_semantic_message() {
        let dir = fresh_dir("aterm-config-editor-problem-cycle");
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
        let dir = fresh_dir("aterm-editor-ordinary-enter");
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
        let dir = fresh_dir("aterm-editor-set-selection");
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
        let (dir, _, mut app, (instance, view)) =
            open_config("aterm-config-editor-stale-completion", "win\nthe");
        let wid = WindowId(0);
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
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn duplicate_basenames_refresh_inactive_tab_titles_and_keep_exact_uri_tooltips() {
        let dir = fresh_dir("aterm-app-document-identities");
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
                    tab.presentation.title.as_str(),
                    tab.presentation.tooltip.clone().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        // Markdown and Editor for one canonical document share one suffix, and each
        // tooltip carries its document's exact URI.
        let suffix = |uri: &str| if uri == one_uri { "one" } else { "two" };
        assert_eq!(documents.len(), 3);
        for ((title, tooltip), uri) in documents.iter().zip([&one_uri, &one_uri, &two_uri]) {
            assert_eq!(*title, format!("README.md — {}", suffix(uri)));
            assert!(tooltip.ends_with(uri.as_str()), "{tooltip:?} names {uri}");
        }
        app.refresh_window_tabs(wid);
        assert_eq!(
            app.windows[&wid].tab_set.tabs()[1].presentation.title,
            "README.md — one"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_undo_redo_command_availability_tracks_shipping_history_after_every_reduction() {
        let dir = fresh_dir("aterm-app-editor-history");
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

        commit(&mut app, wid, "x");
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

    /// AN UNDO BACK TO THE BYTES ON DISK IS CLEAN: the document's checkpoint
    /// moves to it, so Save is quiet, the editor and the tab read clean, a
    /// close is ready with no save, and the file is not written. NEGATIVE
    /// CONTROLS: an undo that leaves other text stays dirty, and so does one
    /// made while a save of other bytes is still in flight, which then lands
    /// those bytes and leaves the buffer dirty against them.
    #[test]
    fn an_undo_back_to_the_saved_text_leaves_the_document_clean() {
        let dir = std::env::temp_dir().join(format!("aterm-app-undo-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("undo.txt");
        fs::write(&path, "abc").unwrap();
        let mtime = || fs::metadata(&path).unwrap().modified().unwrap();
        let before = mtime();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let enabled = |app: &App, action: &str| {
            app.native_runtime
                .commands(instance, view)
                .unwrap()
                .into_iter()
                .find(|command| command.id.as_str() == action)
                .expect("editor command")
                .enabled
        };
        let editor_dirty = |app: &App| {
            let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
                panic!("editor app");
            };
            editor.dirty
        };
        let type_text = |app: &mut App, text: &str| {
            app.dispatch_native_event(
                wid,
                AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
            )
            .unwrap();
        };
        let undo = |app: &mut App| {
            app.dispatch_native_event(
                wid,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("editor/undo"),
                    value: None,
                }),
            )
            .unwrap();
        };

        type_text(&mut app, "x");
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert!(enabled(&app, "editor/save") && editor_dirty(&app));
        undo(&mut app);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "abc"
        );
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert!(!enabled(&app, "editor/save"), "Save is quiet");
        assert!(!editor_dirty(&app), "the editor reads clean");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view");
        };
        assert_eq!(state.status.as_deref(), Some("No unsaved changes"));
        let views = [crate::document_store::DocumentViewId(view.get())];
        assert!(matches!(
            app.document_store.prepare_close(document, &views),
            Ok(crate::document_store::DocumentCloseReadiness::Ready { .. })
        ));
        app.document_store.abort_close(document).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "abc");
        assert_eq!(mtime(), before, "nothing was written");

        // An undo that leaves other text than the file's is still a change.
        type_text(&mut app, "x");
        type_text(&mut app, "y");
        undo(&mut app);
        assert_ne!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "abc"
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        undo(&mut app);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "abc"
        );
        assert_eq!(app.document_store.dirty(document), Some(false));

        // A save of other bytes still in flight: the undo is not clean yet,
        // and once those bytes land it is a change against them.
        type_text(&mut app, "z");
        let saving = begin_inflight_save_for_test(&mut app, document);
        undo(&mut app);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "abc"
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        finish_inflight_save_for_test(&mut app, document, view, saving);
        assert_eq!(fs::read_to_string(&path).unwrap(), "zabc");
        assert_eq!(app.document_store.dirty(document), Some(true));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_content_fingerprint_collision_does_not_mark_an_edit_saved() {
        // This measured equal-length FNV collision also exercises the legacy
        // journal-name migration. A fingerprint is not byte equality.
        const SAVED: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
        const EDITED: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
        assert_ne!(SAVED, EDITED);
        assert_eq!(SAVED.len(), EDITED.len());
        assert_eq!(
            crate::native_document_io::ContentFingerprint::of(SAVED.as_bytes()),
            crate::native_document_io::ContentFingerprint::of(EDITED.as_bytes())
        );
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("collision.txt");
        fs::write(&path, SAVED).unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let before = app.document_store.snapshot(document).unwrap();
        let outcome = app.document_store.transact(
            document,
            before.seq,
            vec![crate::document_store::TextEdit {
                range: 0..before.text.len(),
                insert: EDITED.into(),
            }],
        );
        assert!(matches!(
            outcome,
            crate::document_store::DocumentTxnOutcome::Committed { .. }
        ));
        app.publish_editor_commit(document, view);
        assert_eq!(
            app.document_store.dirty(document),
            Some(true),
            "different bytes must retain their unsaved checkpoint"
        );
        assert!(
            app.native_runtime
                .commands(instance, view)
                .unwrap()
                .iter()
                .any(|command| command.id.as_str() == "editor/save" && command.enabled)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), SAVED);
    }

    #[test]
    fn a_saved_image_replaces_the_exact_undo_clean_baseline() {
        const BEFORE: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
        const SAVED: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("saved-collision.txt");
        fs::write(&path, BEFORE).unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(WindowId(0)).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let replace = |app: &mut App, text: &str| {
            let head = app.document_store.snapshot(document).unwrap();
            assert!(matches!(
                app.document_store.transact(
                    document,
                    head.seq,
                    vec![crate::document_store::TextEdit {
                        range: 0..head.text.len(),
                        insert: text.into(),
                    }]
                ),
                crate::document_store::DocumentTxnOutcome::Committed { .. }
            ));
        };
        replace(&mut app, SAVED);
        // Real editor commits publish their new journal head before a save can
        // complete. A direct store transaction alone has not done that yet.
        app.publish_editor_commit(document, view);
        let saving = begin_inflight_save_for_test(&mut app, document);
        finish_inflight_save_for_test(&mut app, document, view, saving);
        assert_eq!(fs::read_to_string(&path).unwrap(), SAVED);
        assert_eq!(app.document_store.dirty(document), Some(false));
        replace(&mut app, BEFORE);
        app.publish_editor_commit(document, view);
        assert_eq!(
            app.document_store.dirty(document),
            Some(true),
            "an older colliding image is not the newly saved file"
        );
        replace(&mut app, SAVED);
        app.publish_editor_commit(document, view);
        assert_eq!(
            app.document_store.dirty(document),
            Some(false),
            "the exact newly saved image is clean"
        );
    }

    #[test]
    fn stable_observations_do_not_treat_colliding_text_as_identical_bytes() {
        const BEFORE: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
        const OBSERVED: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
        for dirty in [false, true] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let path = scratch.path().join("observed-collision.txt");
            fs::write(&path, BEFORE).unwrap();
            let mut app = App::headless_for_test();
            app.open_document_tab(AppKind::Editor, &file_uri(&path))
                .unwrap();
            let (instance, view) = app.active_native_view(WindowId(0)).unwrap();
            let document = app.native_runtime.document_id(instance).unwrap();
            if dirty {
                let head = app.document_store.snapshot(document).unwrap();
                assert!(matches!(
                    app.document_store.transact(
                        document,
                        head.seq,
                        vec![crate::document_store::TextEdit {
                            range: 0..head.text.len(),
                            insert: "private draft".into(),
                        }]
                    ),
                    crate::document_store::DocumentTxnOutcome::Committed { .. }
                ));
                app.publish_editor_commit(document, view);
            }
            fs::write(&path, OBSERVED).unwrap();
            let reduction = app.refresh_native_document(document).unwrap();
            if dirty {
                assert!(
                    matches!(
                        reduction,
                        crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
                    ),
                    "different observed bytes must preserve the draft conflict: {reduction:?}"
                );
                assert_eq!(
                    app.document_store.snapshot(document).unwrap().text.as_ref(),
                    "private draft"
                );
                assert!(app.native_documents.disk_conflicts.contains_key(&document));
            } else {
                assert!(
                    matches!(
                        reduction,
                        crate::native_document_io::FileWatchReduction::ReloadClean { .. }
                    ),
                    "different observed bytes must reload the clean document: {reduction:?}"
                );
                let head = app.document_store.snapshot(document).unwrap();
                assert_eq!(head.text.as_ref(), OBSERVED);
                assert_eq!(app.document_store.dirty(document), Some(false));
                assert!(matches!(
                    app.document_store.transact(
                        document,
                        head.seq,
                        vec![crate::document_store::TextEdit {
                            range: 0..head.text.len(),
                            insert: BEFORE.into(),
                        }]
                    ),
                    crate::document_store::DocumentTxnOutcome::Committed { .. }
                ));
                app.publish_editor_commit(document, view);
                assert_eq!(
                    app.document_store.dirty(document),
                    Some(true),
                    "the pre-refresh image must not remain the exact clean baseline"
                );
            }
            assert_eq!(fs::read_to_string(&path).unwrap(), OBSERVED);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_fresh_colliding_read_after_a_save_rebind_keeps_the_draft_conflict() {
        const BEFORE: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
        const COLLISION: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
        let scratch = aterm_tempfile::tempdir().unwrap();
        let path = scratch.path().join("rebound-collision.txt");
        let replacement = scratch.path().join("replacement.txt");
        fs::write(&path, BEFORE).unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(WindowId(0)).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let head = app.document_store.snapshot(document).unwrap();
        assert!(matches!(
            app.document_store.transact(
                document,
                head.seq,
                vec![crate::document_store::TextEdit {
                    range: 0..head.text.len(),
                    insert: "private draft".into(),
                }]
            ),
            crate::document_store::DocumentTxnOutcome::Committed { .. }
        ));
        app.publish_editor_commit(document, view);
        fs::write(&replacement, BEFORE).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let save = app.save_document_checkpoint(document, view).unwrap_err();
        assert!(save.contains("Disk version refreshed"), "{save}");
        assert!(!app.native_documents.disk_conflicts.contains_key(&document));

        fs::write(&path, COLLISION).unwrap();
        let reduction = app.refresh_native_document(document).unwrap();
        assert!(matches!(
            reduction,
            crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
        ));
        assert!(app.native_documents.disk_conflicts.contains_key(&document));
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "private draft"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), COLLISION);
    }

    #[test]
    fn markdown_edit_action_completes_its_reader_status_after_editor_focus() {
        let dir = fresh_dir("aterm-app-edit-handoff");
        let path = dir.join("handoff.md");
        fs::write(&path, "# Handoff\n\nBody\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        let (markdown_instance, _) = app.active_native_view(wid).unwrap();

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
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_ime_input_route_preserves_document_and_minibuffer_marked_carets() {
        let (dir, uri) = admission_fixture("ime-marked-caret", b"original\n");
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();
        for minibuffer in [false, true] {
            if minibuffer {
                drive_native(&mut app, isearch_key());
            }
            for (selection, expected) in [
                (Some((3, 6)), Some(6)),
                (Some((1, 3)), Some(3)),
                (Some((2, 3)), None),
                (Some((6, 3)), None),
                (Some((0, 99)), None),
                (None, None),
            ] {
                app.on_ime_preedit(wid, "e\u{301}日本".to_string(), selection);
                let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
                    panic!("editor")
                };
                assert_eq!(state.preedit, "e\u{301}日本");
                assert_eq!(
                    state.preedit_caret, expected,
                    "minibuffer={minibuffer}, selection={selection:?}"
                );
                assert_eq!(
                    state.buffer.as_ref().unwrap().minibuffer_active(),
                    minibuffer
                );
                let current = app.document_store.snapshot(document).unwrap();
                assert_eq!(
                    (current.seq, current.text.as_ref()),
                    (original.seq, original.text.as_ref())
                );
            }
            app.on_ime_preedit(wid, String::new(), None);
            let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
                panic!("editor")
            };
            assert!(state.preedit.is_empty());
            assert_eq!(state.preedit_caret, None);
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn editor_input_mutates_shared_document_and_save_advances_checkpoint() {
        let dir = fresh_dir("aterm-app-edit");
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
        let dir = fresh_dir("aterm-app-save-newer-modified");
        let path = dir.join("modified.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        commit(&mut app, wid, "saved ");
        let first = begin_inflight_save_for_test(&mut app, document);
        commit(&mut app, wid, "newer ");

        finish_inflight_save_for_test(&mut app, document, view, first);

        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(fs::read_to_string(&path).unwrap(), "saved base\n");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view must remain installed");
        };
        assert_ne!(state.status.as_deref(), Some("Saved"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn repeated_save_during_inflight_checkpoint_persists_the_latest_sequence() {
        let dir = fresh_dir("aterm-app-save-latch");
        let path = dir.join("latched.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        commit(&mut app, wid, "first ");
        let first = begin_inflight_save_for_test(&mut app, document);

        commit(&mut app, wid, "second ");
        app.save_document_checkpoint(document, view).unwrap();
        commit(&mut app, wid, "third ");
        app.save_document_checkpoint(document, view).unwrap();
        let latest = app.document_store.snapshot(document).unwrap();
        assert_eq!(
            app.native_documents
                .pending_saves
                .get(&document)
                .map(|intent| intent.seq),
            Some(latest.seq)
        );

        finish_inflight_save_for_test(&mut app, document, view, first);

        assert!(!app.native_documents.inflight.contains(&document));
        assert!(!app.native_documents.pending_saves.contains_key(&document));
        assert_eq!(app.document_store.dirty(document), Some(false));
        assert_eq!(
            app.document_store.checkpoint_seq(document),
            Some(latest.seq)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
        let _ = fs::remove_dir_all(dir);
    }

    /// The late-document fixture: `planned.md` dirty in window 0, the shutdown `arm`ed
    /// on it, then — the app staying interactive while the barrier waits — a SECOND
    /// document `late.md` opened and dirtied. Returns the dir, the app, the planned and
    /// late `(document, view)`s, the late file and whatever `arm` returned.
    fn late_document<R>(
        name: &str,
        arm: impl FnOnce(&mut App, DocumentId, crate::tab_model::ViewId) -> R,
    ) -> (
        std::path::PathBuf,
        App,
        (DocumentId, crate::tab_model::ViewId),
        (DocumentId, crate::tab_model::ViewId),
        std::path::PathBuf,
        R,
    ) {
        let dir = fresh_dir(name);
        let planned = dir.join("planned.md");
        let late = dir.join("late.md");
        fs::write(&planned, "planned\n").unwrap();
        fs::write(&late, "late\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&planned))
            .unwrap();
        let (instance, planned_view) = app.active_native_view(wid).unwrap();
        let planned_document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "planned edit ");
        let armed = arm(&mut app, planned_document, planned_view);
        app.open_document_tab(AppKind::Editor, &file_uri(&late))
            .unwrap();
        let (instance, late_view) = app.active_native_view(wid).unwrap();
        let late_document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "late edit ");
        assert_eq!(app.document_store.dirty(late_document), Some(true));
        (
            dir,
            app,
            (planned_document, planned_view),
            (late_document, late_view),
            late,
            armed,
        )
    }

    /// A document opened in a window after its close barrier armed is absent
    /// from the frozen plan; the ready path must re-plan, or the teardown strands
    /// its dirty view in `view_store` with no tab or window.
    #[test]
    fn window_close_plan_frozen_before_a_later_document_cannot_orphan_its_dirty_view() {
        let wid = WindowId(0);
        let (dir, mut app, planned, (late_document, late_view), _, ()) =
            late_document("aterm-window-close-late-document", |app, _, _| {
                assert!(
                    !app.prepare_window_document_shutdown_inner(wid, false)
                        .unwrap()
                );
            });
        app.save_document_checkpoint(planned.0, planned.1).unwrap();
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, vec![wid])
        );
        assert_eq!(app.document_store.view_count(planned.0), Some(0));
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
        assert!(
            !app.view_store.contains(late_view),
            "window teardown left an unreachable native view for a dirty document"
        );
        assert_eq!(
            app.document_store.view_count(late_document),
            Some(0),
            "a retired window must not keep a document reference edge"
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// The same late document through a repeated close gesture (what a user does
    /// when the window does not close). It may re-plan and save the late document
    /// too, but must never report ready while that document is dirty (a ready
    /// verdict lets `close_window` run `close_window_logical` at once), nor leave
    /// its view behind.
    #[test]
    fn repeated_window_close_gesture_cannot_commit_a_stale_document_plan() {
        let wid = WindowId(0);
        let (dir, mut app, _, (late_document, late_view), _, ()) =
            late_document("aterm-window-close-repeat-gesture", |app, _, _| {
                assert!(
                    !app.prepare_window_document_shutdown_inner(wid, false)
                        .unwrap()
                );
            });
        if app
            .prepare_window_document_shutdown_inner(wid, true)
            .unwrap()
        {
            assert_eq!(
                app.document_store.dirty(late_document),
                Some(false),
                "the window cleared while a document opened after the plan armed \
                 was still unsaved"
            );
            assert_eq!(app.document_store.view_count(late_document), Some(0));
            assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
            assert!(!app.view_store.contains(late_view));
        } else {
            // The honest alternative: the window stays, the late document reachable.
            assert!(app.view_store.contains(late_view));
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// The Quit twin of the window-close frozen-plan test: a document opened and
    /// edited while the Quit's save runs withholds the exit until it is saved too.
    #[test]
    fn quit_plan_frozen_before_a_later_document_cannot_exit_over_its_edits() {
        let (dir, mut app, planned, (late_document, late_view), late, ()) =
            late_document("aterm-quit-late-document", |app, _, _| {
                assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
            });
        app.save_document_checkpoint(planned.0, planned.1).unwrap();
        let mut verdict = app.take_ready_document_shutdowns().unwrap();
        if verdict.0 {
            assert_eq!(
                app.document_store.dirty(late_document),
                Some(false),
                "the Quit was granted over a document opened and edited after it armed"
            );
        } else {
            assert_eq!(verdict, (false, Vec::new()));
            app.save_document_checkpoint(late_document, late_view)
                .unwrap();
            verdict = app.take_ready_document_shutdowns().unwrap();
        }
        assert_eq!(
            verdict,
            (true, Vec::new()),
            "the Quit goes once both are saved"
        );
        assert_eq!(fs::read_to_string(&late).unwrap(), "late edit late\n");
        let _ = fs::remove_dir_all(dir);
    }

    /// The same late document, reached by repeating the Quit gesture while the
    /// first save runs: the repeat re-plans and cannot answer ready over the late
    /// document's edits.
    #[test]
    fn repeated_quit_gesture_cannot_exit_over_a_later_document() {
        let (dir, mut app, _, (late_document, late_view), _, ()) =
            late_document("aterm-quit-repeat-gesture", |app, _, _| {
                assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
            });
        if app.prepare_quit_document_shutdown_inner(true).unwrap() {
            assert_eq!(
                app.document_store.dirty(late_document),
                Some(false),
                "a repeated Quit was granted over a document opened after it armed"
            );
        } else {
            assert!(app.view_store.contains(late_view));
        }
        let _ = fs::remove_dir_all(dir);
    }

    /// The same late document, reached through a Quit that was ready when it
    /// deferred behind an update handoff (`DeferredHandoffTeardown::CleanQuitReady`).
    /// The replay asks the document barrier, not only the native one (where the
    /// editor answers `Ready` whatever it holds), so the Quit stays pending and
    /// goes once the late document is saved.
    #[cfg(unix)]
    #[test]
    fn a_quit_ready_behind_an_update_cannot_exit_over_a_document_edited_meanwhile() {
        let _serial = crate::menu::terminate_test_serial();
        let (dir, mut app, _, (late_document, late_view), late, generation) =
            late_document("aterm-quit-replay-late", |app, document, view| {
                let crate::menu::NativeTerminateDecision::Dispatch(generation) =
                    crate::menu::admit_native_terminate(false)
                else {
                    panic!("a first request dispatches");
                };
                assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());
                app.save_document_checkpoint(document, view).unwrap();
                assert_eq!(
                    app.take_ready_document_shutdowns().unwrap(),
                    (true, Vec::new())
                );
                generation
            });
        if app.replay_clean_quit_ready() {
            assert_eq!(
                app.document_store.dirty(late_document),
                Some(false),
                "the replayed Quit exited over a document edited while it was deferred"
            );
        } else {
            assert!(
                crate::menu::native_termination_is_current(generation),
                "a Quit left saving stays pending, neither completed nor cancelled"
            );
            app.save_document_checkpoint(late_document, late_view)
                .unwrap();
            assert_eq!(
                app.take_ready_document_shutdowns().unwrap(),
                (true, Vec::new()),
                "the Quit goes once the late document is saved"
            );
        }
        assert_eq!(fs::read_to_string(&late).unwrap(), "late edit late\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn async_window_close_revalidates_settings_drafts_before_document_detach() {
        let dir = fresh_dir("aterm-window-close-native-revalidate");
        let path = dir.join("close.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (editor, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(editor).unwrap();
        commit(&mut app, wid, "durable ");
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
        let dir = fresh_dir("aterm-quit-native-revalidate");
        let path = dir.join("quit.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (editor, editor_view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(editor).unwrap();
        commit(&mut app, wid, "durable ");
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

        let dir = fresh_dir("aterm-app-save-latch-conformance");
        let path = dir.join("close.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let baseline = app.document_store.snapshot(document).unwrap().seq.0;
        let model = aterm_models_native::native_save_intent_latch_model();
        let mut controller = Controller {
            baseline,
            target: 0,
            requested: 0,
            close_seq: 0,
            settled: true,
        };
        let mut state = project(&model, &app, document, controller);
        assert_eq!(state, model.init_state());

        commit(&mut app, wid, "first ");
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

        commit(&mut app, wid, "latest ");
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

        // Negative control: a close committed the moment it is armed. The
        // genuine plan is not ready — `prepare_close` finds no checkpoint
        // covering the frozen sequence — and nothing the model sees moves.
        assert_eq!(
            app.take_ready_document_shutdowns().unwrap(),
            (false, Vec::new())
        );
        assert_eq!(project(&model, &app, document, controller), state);
        let early = buggy.successors("CommitClose", &state)[0].clone();
        assert_eq!(early["closed"], 1);
        assert_eq!(aterm_spec::interp::admits(&model, &state, &early), None);
        assert!(!model.check_invariant("ClosedSequenceIsDurable", &early));

        finish_inflight_save_for_test(&mut app, document, view, first);
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

    /// A close that has to wait on a save says so, and what comes next:
    /// C-x C-c on a buffer whose save is still landing leaves the tab open,
    /// for the person to close again once the save lands, and the same keys
    /// then close it with the edit on disk.
    #[test]
    fn a_close_waiting_on_its_save_says_so() {
        let dir = fresh_dir("aterm-editor-close-waits");
        let path = dir.join("waits.txt");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let tabs = |app: &App| app.windows.get(&wid).unwrap().tab_set.len();
        let opened = tabs(&app);
        commit(&mut app, wid, "edit ");
        let save = begin_inflight_save_for_test(&mut app, document);
        let close = |app: &mut App| {
            for event in [
                key(Key::Character('x'), Modifiers::CTRL),
                key(Key::Character('c'), Modifiers::CTRL),
            ] {
                assert!(app.native_input_event(wid, &event));
            }
        };
        close(&mut app);
        assert_eq!(tabs(&app), opened);
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view");
        };
        assert_eq!(
            state.status.as_deref(),
            Some("Saving this buffer first; close it again once the save lands")
        );
        finish_inflight_save_for_test(&mut app, document, view, save);
        close(&mut app);
        assert_eq!(tabs(&app), opened - 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "edit base\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_stale_save_completion_is_not_painted_as_a_failure() {
        let dir = fresh_dir("aterm-app-stale-save");
        let path = dir.join("late.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "edit ");
        let first = begin_inflight_save_for_test(&mut app, document);
        let generation = first.plan.generation;
        let bytes = std::sync::Arc::clone(&first.plan.bytes);
        finish_inflight_save_for_test(&mut app, document, view, first);

        // A late completion for the generation that already landed.
        let error = app
            .finish_native_document_save(
                document,
                view,
                generation,
                bytes,
                crate::native_document_io::AtomicSaveResult::Cancelled,
                None,
            )
            .unwrap_err();
        assert!(error.contains("stale"), "{error}");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor view");
        };
        assert_eq!(state.status.as_deref(), Some("Saved"));
        let tab = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.leaves().contains(&view))
            .expect("document tab");
        assert!(!tab.presentation.indicators.attention);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn quit_during_inflight_save_pumps_the_frozen_quit_sequence() {
        let dir = fresh_dir("aterm-app-quit-save-latch");
        let path = dir.join("quit.md");
        fs::write(&path, "base\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        commit(&mut app, wid, "first ");
        let first = begin_inflight_save_for_test(&mut app, document);
        commit(&mut app, wid, "latest ");
        let latest = app.document_store.snapshot(document).unwrap();

        assert!(!app.prepare_quit_document_shutdown_inner(true).unwrap());
        assert!(app.native_documents.pending_saves.contains_key(&document));
        finish_inflight_save_for_test(&mut app, document, view, first);

        assert_eq!(app.take_ready_document_shutdowns().unwrap(), (true, vec![]));
        assert!(!app.native_documents.pending_saves.contains_key(&document));
        assert_eq!(
            app.document_store.checkpoint_seq(document),
            Some(latest.seq)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), latest.text.as_ref());
        let _ = fs::remove_dir_all(dir);
    }

    fn undo(app: &mut App, wid: WindowId) {
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("editor/undo"),
                value: None,
            }),
        )
        .unwrap();
    }

    /// The document is clean, conflict-free and saying nothing about local
    /// edits, its tab and its Save quiet, and holds `text`.
    fn assert_clean_at(
        app: &App,
        (instance, view): (crate::tab_model::AppInstanceId, crate::tab_model::ViewId),
        text: &str,
        context: &str,
    ) {
        let document = app.native_runtime.document_id(instance).unwrap();
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            text,
            "{context}"
        );
        assert_eq!(app.document_store.dirty(document), Some(false), "{context}");
        assert!(
            !app.native_documents.disk_conflicts.contains_key(&document),
            "{context}"
        );
        assert!(
            !app.native_documents.recovery_status.contains_key(&document),
            "{context}"
        );
        let Some(NativeApp::Editor(editor)) = app.native_runtime.app(instance) else {
            panic!("{context}: the editor stays installed");
        };
        assert!(!editor.disk_conflict, "{context}");
        assert!(!editor.dirty, "{context}");
        assert_eq!(editor.recovery_status, None, "{context}");
        let save = app
            .native_runtime
            .commands(instance, view)
            .unwrap()
            .into_iter()
            .find(|command| command.id.as_str() == "editor/save")
            .expect("Save command");
        assert!(!save.enabled, "{context}: nothing to save");
    }

    /// AN UNDO BACK TO THE BASE UNDER A DISK CONFLICT ADOPTS THE DISK TEXT:
    /// once the buffer holds exactly the version aterm loaded, it has no local
    /// edits, so the file's newer version comes in as it would for a clean
    /// document and the "copy any local edits" guidance goes — in Manual and
    /// in a plain Editor tab. (It used to stay dirty, conflicted and blocked
    /// until Discard was pressed by hand.)
    #[test]
    fn an_undo_back_to_the_base_under_a_disk_conflict_adopts_the_disk_text() {
        let wid = WindowId(0);
        let base = "font_px = 12.0\n";
        let external = "cursor_blink = true\nfont_px = 12.0\n";
        let (dir, path, mut app, views) = open_config("aterm-undo-to-base-under-conflict", base);
        let document = app.native_runtime.document_id(views.0).unwrap();
        commit(&mut app, wid, "gpu");
        fs::write(&path, external).unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        app.refresh_open_config_editor_observation(&observation)
            .unwrap();
        assert_eq!(
            app.native_documents.disk_conflicts.get(&document),
            Some(&DiskConflictCause::Version)
        );
        assert_eq!(
            app.native_documents
                .recovery_status
                .get(&document)
                .map(String::as_str),
            Some(crate::native_app::DISK_CONFLICT_RECOVERY)
        );
        undo(&mut app, wid);
        assert_clean_at(&app, views, external, "Manual");
        let _ = fs::remove_dir_all(dir);

        let dir = fresh_dir("aterm-undo-to-base-under-conflict-editor");
        let path = dir.join("notes.md");
        fs::write(&path, "disk one\n").unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let views = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(views.0).unwrap();
        commit(&mut app, wid, "local ");
        fs::write(&path, "disk two\n").unwrap();
        assert!(matches!(
            app.refresh_native_document(document).unwrap(),
            crate::native_document_io::FileWatchReduction::ConflictDirty { .. }
        ));
        undo(&mut app, wid);
        assert_clean_at(&app, views, "disk two\n", "Editor");
        let _ = fs::remove_dir_all(dir);
    }

    /// NEGATIVE CONTROLS for the undo-to-base adoption: an Undo that leaves a
    /// local edit keeps the conflict, its guidance and the edit; a save in
    /// flight owns what lands next, so an Undo to the base waits for it (and
    /// the file's own return to the base then clears the conflict and finds
    /// the buffer clean); and a changed file binding is settled only by a
    /// reopen.
    #[test]
    fn undo_to_base_keeps_local_edits_saves_in_flight_and_binding_conflicts() {
        let wid = WindowId(0);
        let base = "font_px = 12.0\n";
        let external = "cursor_blink = true\nfont_px = 12.0\n";
        let conflicted = |name: &str| {
            let (dir, path, mut app, views) = open_config(name, base);
            commit(&mut app, wid, "gpu");
            commit(&mut app, wid, " = true");
            fs::write(&path, external).unwrap();
            let observation =
                crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                    .unwrap();
            app.refresh_open_config_editor_observation(&observation)
                .unwrap();
            let document = app.native_runtime.document_id(views.0).unwrap();
            assert!(app.native_documents.disk_conflicts.contains_key(&document));
            (dir, path, app, views, document)
        };

        // One of two edits undone: "gpu" is still a local edit.
        let (dir, _, mut app, _, document) = conflicted("aterm-undo-to-base-partial");
        undo(&mut app, wid);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "gpufont_px = 12.0\n"
        );
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert!(app.native_documents.disk_conflicts.contains_key(&document));
        assert_eq!(
            app.native_documents
                .recovery_status
                .get(&document)
                .map(String::as_str),
            Some(crate::native_app::DISK_CONFLICT_RECOVERY)
        );
        let _ = fs::remove_dir_all(dir);

        // A save in flight: the Undo to the base adopts nothing yet.
        let (dir, path, mut app, views, document) = conflicted("aterm-undo-to-base-saving");
        app.native_documents.inflight.insert(document);
        undo(&mut app, wid);
        undo(&mut app, wid);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            base
        );
        assert!(app.native_documents.disk_conflicts.contains_key(&document));
        app.native_documents.inflight.remove(&document);
        // The file goes back to the version aterm admitted: the conflict
        // clears and the buffer, which is that version, is clean.
        fs::write(&path, base).unwrap();
        assert!(matches!(
            app.refresh_native_document(document).unwrap(),
            crate::native_document_io::FileWatchReduction::Unchanged
                | crate::native_document_io::FileWatchReduction::RebindEquivalent { .. }
        ));
        assert_clean_at(&app, views, base, "the file back at the base");
        let _ = fs::remove_dir_all(dir);

        // A changed binding: a reopen, whatever the buffer holds.
        let (dir, _, mut app, _, document) = conflicted("aterm-undo-to-base-binding");
        app.set_document_disk_conflict(document, Some(DiskConflictCause::Binding));
        undo(&mut app, wid);
        undo(&mut app, wid);
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            base
        );
        assert_eq!(
            app.native_documents.disk_conflicts.get(&document),
            Some(&DiskConflictCause::Binding)
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn file_refresh_reloads_clean_conflicts_dirty_and_explicit_discard_is_atomic() {
        let dir = fresh_dir("aterm-app-refresh-discard");
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

        commit(&mut app, wid, "local ");
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
        assert!(app.native_documents.disk_conflicts.contains_key(&document));
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
        app.save_document_checkpoint(document, view).unwrap_err();
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk three\n");
        let recovery = app
            .native_documents
            .recovery_status
            .get(&document)
            .expect("conflict recovery guidance");
        // The watcher's words for the conflict are the save conflict's.
        assert_eq!(recovery, crate::native_app::DISK_CONFLICT_RECOVERY);

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
        assert!(!app.native_documents.disk_conflicts.contains_key(&document));
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

        commit(&mut app, wid, "resolved ");
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
    fn shipping_search_and_m_x_own_text_without_document_leakage() {
        let dir = fresh_dir("aterm-app-editor-minibuffer");
        let path = dir.join("modal.txt");
        fs::write(&path, "zero needle tail").unwrap();
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let original = app.document_store.snapshot(document).unwrap();

        drive_native(&mut app, isearch_key());
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
        // A query no command name contains, even as a subsequence
        // (`not-a-command` became one: indent-for-tab-command).
        drive_native(
            &mut app,
            InputEvent::Text("definitely-not-a-command".to_string()),
        );
        drive_native(
            &mut app,
            key(Key::Named(NamedKey::Enter), Modifiers::empty()),
        );
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view) else {
            panic!("editor remains live");
        };
        assert!(matches!(
            state.buffer.as_ref().map(|buffer| &buffer.minibuffer),
            Some(Minibuffer::Command { query, .. }) if query == "definitely-not-a-command"
        ));
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
        let dir = fresh_dir("aterm-app-editor-buffer");
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
        assert!(matches!(
            app.native_runtime.view_state(active_view),
            Some(AppViewState::Editor(_))
        ));
        assert_eq!(
            app.document_store.snapshot(second_document).unwrap().seq,
            second_before.seq
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn production_open_recovers_durable_draft_before_publishing_editor_view() {
        let dir = fresh_dir("aterm-app-crash-recovery");
        let path = dir.join("recover.md");
        let journal_root = dir.join("private-drafts");
        fs::write(&path, "disk\n").unwrap();
        let uri = file_uri(&path);

        {
            let mut crashed = App::headless_for_test();
            crashed.native_documents.journals = JournalSlot::for_test(journal_root.clone());
            crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
            commit(&mut crashed, WindowId(0), "unsaved ");
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
        reopened.native_documents.journals = JournalSlot::for_test(journal_root.clone());
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
        assert!(editor.recovery_status.is_some());
        assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn production_open_surfaces_disk_conflict_and_preserves_draft() {
        let dir = fresh_dir("aterm-app-recovery-conflict");
        let path = dir.join("conflict.md");
        let journal_root = dir.join("private-drafts");
        fs::write(&path, "base\n").unwrap();
        let uri = file_uri(&path);

        {
            let mut crashed = App::headless_for_test();
            crashed.native_documents.journals = JournalSlot::for_test(journal_root.clone());
            crashed.open_document_tab(AppKind::Editor, &uri).unwrap();
            commit(&mut crashed, WindowId(0), "draft ");
        }
        fs::write(&path, "external\n").unwrap();

        let mut reopened = App::headless_for_test();
        reopened.native_documents.journals = JournalSlot::for_test(journal_root.clone());
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
        assert!(editor.recovery_status.is_some());
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
    fn final_dirty_document_leaf_is_saved_before_only_that_split_leaf_detaches() {
        let dir = fresh_dir("aterm-app-leaf-close");
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
            .insert_instance(NativeApp::Settings(Box::new(
                crate::native_settings::SettingsApp::new(
                    crate::update_screen::UpdateState::from_status(1, "test", None, false),
                ),
            )))
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
        commit(&mut app, wid, "safe ");

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
        let dir = fresh_dir("aterm-app-share-close");
        let path = dir.join("shared-close.md");
        fs::write(&path, "before\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let wid = WindowId(0);
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "dirty ");

        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(1));
        assert_eq!(app.document_store.dirty(document), Some(true));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");

        // The final dirty view: a real event-loop save may report pending; the
        // headless lane runs the identical durability proof inline, then detaches.
        let before_tabs = app.windows[&wid].tab_set.len();
        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.windows[&wid].tab_set.len(), before_tabs - 1);
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert_eq!(fs::read_to_string(&path).unwrap(), "dirty before\n");
        let _ = fs::remove_dir_all(dir);
    }

    /// ATERM-KI-1015-12: a document whose last view closed stays in memory
    /// while a reopen could bring it back — the reopen finds the very document,
    /// its undo history with it — and leaves with every trace once nothing
    /// can: the store's entry, its saver and grant, its journal entry and the
    /// clean journal image no other process holds, its editor history.
    /// Opening the file again then reads it afresh.
    #[test]
    fn a_closed_document_leaves_memory_once_no_reopen_can_bring_it_back() {
        let dir = fresh_dir("aterm-app-release");
        let path = dir.join("released.md");
        fs::write(&path, "before\n").unwrap();
        let uri = file_uri(&path);
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        let canonical = app
            .document_store
            .canonical_uri(document)
            .unwrap()
            .to_string();
        commit(&mut app, wid, "edited ");
        assert!(app.editor_workspace.can_undo(document));
        let journal = app
            .native_documents
            .journals
            .as_ref()
            .unwrap()
            .path_for_uri(&canonical);
        assert!(journal.exists());
        assert_eq!(
            app.release_unheld_documents(0),
            0,
            "a viewed document stays"
        );

        app.close_active_native_tab(wid).unwrap();
        assert_eq!(app.document_store.view_count(document), Some(0));
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited before\n");
        assert!(
            app.native_documents.release_due,
            "a closed view arms the look"
        );
        assert_eq!(
            app.release_unheld_documents(0),
            0,
            "the closed-tab record can still reopen it"
        );
        assert!(!app.native_documents.release_due);
        app.reopen_last_closed_tab().unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        assert_eq!(app.native_runtime.document_id(instance), Some(document));
        assert!(app.editor_workspace.can_undo(document), "undo survived");

        app.close_active_native_tab(wid).unwrap();
        assert_eq!(
            app.release_unheld_documents(u64::MAX),
            1,
            "past the record's age nothing can bring it back"
        );
        assert_eq!(app.document_store.id_for_uri(&canonical), None);
        assert_eq!(app.native_documents.persistence.grant(document), None);
        assert_eq!(app.native_documents.grants.id_for_uri(&canonical), None);
        assert_eq!(
            app.native_documents
                .journals
                .as_ref()
                .unwrap()
                .durable_seq(document),
            None
        );
        assert!(!app.editor_workspace.can_undo(document));
        assert!(!app.editor_workspace.buffers.contains(&document));
        assert!(
            !journal.exists(),
            "its clean journal, which no other process holds, went with it"
        );

        fs::write(&path, "changed on disk\n").unwrap();
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let reopened = app.native_runtime.document_id(instance).unwrap();
        assert_ne!(reopened, document);
        assert_eq!(
            app.document_store.snapshot(reopened).unwrap().text.as_ref(),
            "changed on disk\n"
        );
        assert!(app.structural_invariants_ok());
        let _ = fs::remove_dir_all(dir);
    }

    /// A document kept only by a recently-closed record leaves at the first
    /// park once that record has expired, with no other close or write to
    /// prompt the look.
    #[test]
    fn a_closed_document_leaves_at_the_first_park_past_its_records_expiry() {
        let dir = fresh_dir("aterm-app-release-expiry");
        let path = dir.join("expiring.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.close_active_native_tab(wid).unwrap();
        let closed_ms = app.lat_epoch.elapsed().as_millis() as u64;
        assert_eq!(
            app.release_documents_due(closed_ms),
            0,
            "its record is live"
        );
        assert!(!app.native_documents.release_due);
        let deadline = app
            .native_documents
            .release_deadline_ms
            .expect("the record's expiry is the next look");
        assert!(deadline > closed_ms);
        assert_eq!(app.release_documents_due(deadline - 1), 0);
        assert!(app.document_store.phase(document).is_some(), "not yet");
        assert_eq!(app.release_documents_due(deadline), 1);
        assert_eq!(app.document_store.phase(document), None);
        assert_eq!(app.native_documents.release_deadline_ms, None);
        let _ = fs::remove_dir_all(dir);
    }

    /// A Suspended document whose journal still has work in flight stays, and
    /// the landing of that work arms the next look, which lets it go.
    #[cfg(unix)]
    #[test]
    fn a_closed_document_with_a_journal_write_in_flight_waits_for_its_landing() {
        let dir = fresh_dir("aterm-app-release-inflight");
        let path = dir.join("inflight.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, _) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.close_active_native_tab(wid).unwrap();
        app.hold_journal_reseats_for_test();
        app.reseat_document_journals_after_rollback();
        assert_eq!(app.held_journal_reseats_for_test().len(), 1);

        assert_eq!(app.release_unheld_documents(u64::MAX), 0);
        assert!(app.document_store.phase(document).is_some(), "it stays");
        assert!(app.run_held_journal_reseat_for_test());
        assert!(
            app.native_documents.release_due,
            "the landing arms the look"
        );
        assert_eq!(app.release_unheld_documents(u64::MAX), 1);
        assert_eq!(app.document_store.phase(document), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn window_close_waits_for_every_document_before_atomic_detach() {
        let dir = fresh_dir("aterm-app-window-batch");
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
        commit(&mut app, wid, "safe ");

        app.open_document_tab(AppKind::Editor, &file_uri(&second_path))
            .unwrap();
        let (second_instance, second_view) = app.active_native_view(wid).unwrap();
        let second = app.native_runtime.document_id(second_instance).unwrap();
        commit(&mut app, wid, "also ");
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
        let dir = fresh_dir("aterm-app-quit-batch");
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
        commit(&mut app, wid, "safe ");
        app.open_document_tab(AppKind::Editor, &file_uri(&second_path))
            .unwrap();
        let (second_instance, second_view) = app.active_native_view(wid).unwrap();
        let second = app.native_runtime.document_id(second_instance).unwrap();
        commit(&mut app, wid, "also ");

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
        let dir = fresh_dir("aterm-app-window-save-failure");
        let path = dir.join("conflict.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "ours ");
        assert!(
            !app.prepare_window_document_shutdown_inner(wid, false)
                .unwrap()
        );

        // Negative control: an external generation change makes the genuine save
        // reducer return Conflict rather than a Durable proof.
        fs::write(&path, "theirs\n").unwrap();
        app.save_document_checkpoint(document, view).unwrap_err();
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
        assert!(state.status.is_some());
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
        let dir = fresh_dir("aterm-app-quit-save-failure");
        let path = dir.join("quit-conflict.md");
        fs::write(&path, "before\n").unwrap();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &file_uri(&path))
            .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        commit(&mut app, wid, "ours ");
        assert!(!app.prepare_quit_document_shutdown_inner(false).unwrap());

        fs::write(&path, "theirs\n").unwrap();
        assert!(app.save_document_checkpoint(document, view).is_err());
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
        let dir = fresh_dir("aterm-app-document-drop");
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
