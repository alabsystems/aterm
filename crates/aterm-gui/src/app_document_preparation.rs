// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-URI ownership from file admission through durable journal preparation.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::{AdmittedDocument, App, DocumentGrantStore, DocumentId, GrantAccess, InstallFailure};
use crate::document_store::DocumentSnapshot;
use crate::native_document_host::{GrantedDocument, PreparedDocumentGrant};
use crate::native_document_journal::{
    DraftJournalHost, InitializedJournal, JournalFault, JournalOpenDecision, RecoveryNotice,
};

#[cfg(test)]
std::thread_local! {
    static WORK_OBSERVER: std::cell::RefCell<Option<WorkObserver>> = const { std::cell::RefCell::new(None) };
    static INITIALIZE_DELIVERY_GATE: std::cell::RefCell<Option<DeliveryGate>> = const { std::cell::RefCell::new(None) };
    static INITIALIZE_GATE: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug)]
struct PreparedValue {
    granted: GrantedDocument,
    document: DocumentId,
    existed: bool,
    recovery_notice: Option<RecoveryNotice>,
}

#[derive(Clone, Copy, Debug)]
enum OriginalRequestEffect {
    NoDirectoryRefresh,
    DirectoryRefreshed,
    DirectoryRefreshRefused,
}
impl OriginalRequestEffect {
    fn refusal(self) -> Option<&'static str> {
        matches!(self, Self::DirectoryRefreshRefused)
            .then_some("The directory could not be refreshed for this request")
    }
}

#[derive(Debug)]
struct PreparedResult {
    readiness: PreparedValue,
    effect: OriginalRequestEffect,
}

/// The original request's immutable decision, separate from borrower readiness.
#[derive(Debug)]
pub(super) struct PreparedRequest {
    readiness: PreparedDocument,
    effect: OriginalRequestEffect,
}
impl PreparedRequest {
    pub(super) fn refusal(&self) -> Option<&'static str> {
        self.effect.refusal()
    }
    pub(super) fn into_readiness(self) -> PreparedDocument {
        self.readiness
    }
}

/// A prepared open retains its queue position until its view is installed or abandoned.
#[derive(Clone, Debug)]
pub(crate) struct PreparedDocument {
    pub(super) granted: GrantedDocument,
    pub(super) document: DocumentId,
    existed: bool,
    pub(super) recovery_notice: Option<RecoveryNotice>,
    _lease: PreparationLease,
}

impl PreparedDocument {
    pub(super) fn retain_preparation(&self) -> PreparationLease {
        self._lease.clone()
    }
}

pub(super) enum DocumentOpening {
    Admitted(AdmittedDocument),
    Prepared(Box<PreparedDocument>),
}

impl DocumentOpening {
    pub(super) fn canonical_uri(&self) -> &str {
        match self {
            Self::Admitted(admitted) => admitted.canonical_uri(),
            Self::Prepared(prepared) => &prepared.granted.grant.canonical_uri,
        }
    }

    pub(super) fn directory(&self) -> bool {
        match self {
            Self::Admitted(admitted) => admitted.is_directory(),
            Self::Prepared(prepared) => prepared.granted.is_directory(),
        }
    }

    pub(super) fn existing(
        &self,
        documents: &crate::document_store::DocumentStore,
    ) -> Option<DocumentId> {
        match self {
            Self::Admitted(admitted) => documents.id_for_uri(admitted.canonical_uri()),
            Self::Prepared(prepared) => (prepared.existed
                || documents
                    .view_count(prepared.document)
                    .is_some_and(|count| count > 0))
            .then_some(prepared.document),
        }
    }
}

#[derive(Debug)]
struct Shared {
    leases: AtomicUsize,
    result: Mutex<Option<Result<PreparedResult, InstallFailure>>>,
    proxy: Option<aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
}

impl Shared {
    fn wake(&self) {
        if let Some(proxy) = &self.proxy {
            let _ = proxy.send_event(crate::Wake::RestoreDocumentReady);
        }
    }
}

type RefreshGuard = Box<dyn Fn(&App) -> bool>;
/// Only the original requesting lease owns refresh authority. Readiness borrowers
/// retain Shared without acquiring or extending that requester's lifetime.
#[derive(Debug)]
struct RefreshOwner(Arc<AtomicBool>);
impl Drop for RefreshOwner {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// App predicates live only in the UI coordinator, never in sendable readiness.
struct RefreshWitness {
    live: Arc<AtomicBool>,
    guard: Option<RefreshGuard>,
}
impl RefreshWitness {
    fn owned(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
    fn live(&self, app: &App) -> bool {
        self.owned() && self.guard.as_ref().is_none_or(|guard| guard(app))
    }
}
#[derive(Debug)]
pub(super) struct PreparationLease(Arc<Shared>, Option<RefreshOwner>);

impl Clone for PreparationLease {
    fn clone(&self) -> Self {
        Self::borrow(&self.0)
    }
}

impl Drop for PreparationLease {
    fn drop(&mut self) {
        let owner = self.1.take();
        let invalidated_refresh = owner.is_some();
        drop(owner);
        let last = self.0.leases.fetch_sub(1, Ordering::AcqRel) == 1;
        if last || invalidated_refresh {
            self.0.wake();
        }
    }
}

impl PreparationLease {
    fn borrow(shared: &Arc<Shared>) -> Self {
        shared.leases.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(shared), None)
    }

    fn readiness(&self, value: &PreparedValue) -> PreparedDocument {
        PreparedDocument {
            granted: value.granted.clone(),
            document: value.document,
            existed: value.existed,
            recovery_notice: value.recovery_notice.clone(),
            _lease: self.clone(),
        }
    }
    /// Borrow canonical readiness without acquiring the original request's effect.
    ///
    /// A refusal is typed ([`InstallFailure`]): a draft another aterm process kept busy, or
    /// changed, through [`WAITED_FAILURES`] attempts says so apart from every other failure.
    pub(super) fn poll(&self) -> Option<Result<PreparedDocument, InstallFailure>> {
        self.0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|result| {
                result
                    .as_ref()
                    .map(|value| self.readiness(&value.readiness))
                    .map_err(Clone::clone)
            })
    }
    fn poll_request(&self) -> Option<Result<PreparedRequest, InstallFailure>> {
        self.0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|result| {
                result
                    .as_ref()
                    .map(|value| PreparedRequest {
                        readiness: self.readiness(&value.readiness),
                        effect: value.effect,
                    })
                    .map_err(Clone::clone)
            })
    }
    pub(super) fn request_refusal(&self) -> Option<&'static str> {
        self.0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|value| value.effect.refusal())
    }
}

struct Inspected {
    grant: PreparedDocumentGrant,
    host: DraftJournalHost,
    decision: Option<JournalOpenDecision>,
    recovery: Option<amacs::editor::PreparedRecovery>,
    directory: Option<InspectedDirectory>,
}
struct InspectedDirectory {
    update: amacs::store::PreparedDirectoryUpdate,
    views: Vec<crate::tab_model::ViewId>,
}

enum Stage {
    Queued,
    Inspecting(mpsc::Receiver<Result<Inspected, String>>),
    Waiting,
    Ready,
}

pub(super) struct Preparation {
    admitted: AdmittedDocument,
    access: GrantAccess,
    refresh_requester: Option<RefreshWitness>,
    request_effect: OriginalRequestEffect,
    required_document: Option<DocumentId>,
    shared: Arc<Shared>,
    stage: Stage,
    expected: Option<DocumentId>,
    provisional: Option<DocumentId>,
    granted: Option<GrantedDocument>,
    /// [`RecoveryOwner::failures`] when this request began to wait for its document.
    waited_from: u32,
}

// A request owns a prospective view. This owner survives every request and owns
// recovery of the exact provisional document until its current head is durable.
struct RecoveryOwner {
    disk: DocumentSnapshot,
    granted: GrantedDocument,
    host: DraftJournalHost,
    recovered_text: Option<Arc<str>>,
    witnessed_text: Option<Arc<str>>,
    attempted_text: Arc<str>,
    retry_delay: Duration,
    wake: Arc<Shared>,
    phase: RecoveryPhase,
    /// How many of this owner's attempts have failed, and the latest one's fault: what a
    /// request that waited through [`WAITED_FAILURES`] of them is refused with.
    failures: u32,
    fault: Option<JournalFault>,
}

enum RecoveryPhase {
    Initializing {
        receiver: mpsc::Receiver<Result<InitializationOutcome, String>>,
        captured: DocumentSnapshot,
    },
    NeedJournal(Instant),
    Journaling {
        retry_at: Option<Instant>,
    },
}

const RECOVERY_RETRY: Duration = Duration::from_secs(1);

/// How many failed attempts of its document's [`RecoveryOwner`] a request waits through before
/// it is refused with the latest one's fault. The owner keeps retrying after that, for the
/// provisional document it still holds and for the next request, which waits afresh.
///
/// Against a draft lock another aterm process holds this is two worker lock waits (2 s each,
/// `JournalLockPatience::Worker`) and the one [`RECOVERY_RETRY`] between them, about five
/// seconds: under the `edit` verb's 30 s admission deadline, so a busy draft answers
/// `ERR busy drafts` instead of timing out.
const WAITED_FAILURES: u32 = 2;

fn preparation_now() -> Instant {
    #[cfg(all(test, unix))]
    if let Some(now) = tests::clock_now() {
        return now;
    }
    Instant::now()
}

#[derive(Default)]
pub(super) struct PreparationGroup {
    jobs: VecDeque<Preparation>,
    requested_uris: BTreeSet<String>,
    provisional: Option<DocumentId>,
    provisional_ready: bool,
    recovery: Option<RecoveryOwner>,
    recovery_notice: Option<RecoveryNotice>,
    published: Arc<AtomicBool>,
}

#[cfg(test)]
type WorkObserver = Arc<dyn Fn(&str, amacs::store::PreparationWork) + Send + Sync>;

type DeliveryGate = Box<dyn FnOnce() -> Result<(), String> + Send>;

struct InitializationOutcome {
    // Inspection and initialization are one attempt. Even a failed publication
    // returns the predecessor it actually inspected, plus the attempted snapshot
    // held by Initializing. Two heads bound the uncertainty across any retries.
    witnessed_text: Option<Arc<str>>,
    result: Result<InitializedJournal, JournalFault>,
}

fn initialize_attempt(
    host: &DraftJournalHost,
    decision: JournalOpenDecision,
    disk: &DocumentSnapshot,
    current: &DocumentSnapshot,
    delivery: Option<DeliveryGate>,
) -> InitializationOutcome {
    let witnessed_text = decision.recovered_text.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<InitializedJournal, JournalFault> {
            let initialized = host.initialize_on_worker(decision, disk, current)?;
            if let Some(delivery) = delivery {
                delivery()?;
            }
            Ok(initialized)
        },
    ))
    .unwrap_or_else(|_| {
        Err(JournalFault::Other(
            "document initialization worker panicked".to_string(),
        ))
    });
    InitializationOutcome {
        witnessed_text,
        result,
    }
}

impl RecoveryOwner {
    fn retry_at(&mut self, now: Instant) -> Instant {
        let at = now + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(Duration::from_secs(60));
        at
    }

    /// Record a failed attempt, and answer when the next is due.
    fn failed(&mut self, fault: JournalFault, now: Instant) -> Instant {
        self.failures = self.failures.saturating_add(1);
        self.fault = Some(fault);
        self.retry_at(now)
    }

    /// A request began to wait for this owner's document: its next attempt is due now and its
    /// retries start from [`RECOVERY_RETRY`] again, so a person who asks again (a failed open's
    /// Retry) is answered by a fresh attempt, not by an earlier request's backoff.
    fn hurry(&mut self, now: Instant) {
        self.retry_delay = RECOVERY_RETRY;
        match &mut self.phase {
            RecoveryPhase::NeedJournal(at) | RecoveryPhase::Journaling { retry_at: Some(at) } => {
                *at = (*at).min(now);
            }
            RecoveryPhase::Initializing { .. } | RecoveryPhase::Journaling { retry_at: None } => {}
        }
    }

    /// The fault a request that began to wait at `waited_from` failures is refused with, once
    /// it has waited through [`WAITED_FAILURES`] more.
    fn refusal(&self, waited_from: u32) -> Option<JournalFault> {
        (self.failures.saturating_sub(waited_from) >= WAITED_FAILURES)
            .then(|| self.fault.clone())
            .flatten()
    }
}

pub(super) type Preparations = BTreeMap<String, PreparationGroup>;

fn worker<T: Send + 'static>(
    name: &str,
    shared: &Arc<Shared>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> mpsc::Receiver<Result<T, String>> {
    let (sender, receiver) = mpsc::channel();
    let wake = Arc::clone(shared);
    #[cfg(test)]
    let observer = WORK_OBSERVER.with(|observer| observer.borrow().clone());
    #[cfg(test)]
    let observed_name = name.to_owned();
    let started = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            #[cfg(test)]
            if observer.is_some() {
                amacs::store::take_preparation_work();
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("document preparation worker panicked".into()));
            #[cfg(test)]
            if let Some(observer) = observer {
                observer(&observed_name, amacs::store::take_preparation_work());
            }
            let _ = sender.send(result);
            wake.wake();
        });
    if let Err(error) = started {
        let (sender, failed) = mpsc::channel();
        let _ = sender.send(Err(format!(
            "could not start document preparation worker: {error}"
        )));
        shared.wake();
        return failed;
    }
    receiver
}

fn completed<T>(receiver: &mpsc::Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match receiver.try_recv() {
        Ok(result) => Some(result),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => {
            Some(Err("document preparation ended without a result".into()))
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) fn set_initialize_gate_for_test(gate: impl FnOnce() + Send + 'static) {
    INITIALIZE_GATE.set(Some(Box::new(gate)));
}

impl App {
    pub(crate) fn document_preparation_pending(&self, uri: &str) -> bool {
        self.native_documents.preparations.contains_key(uri)
            || self.native_documents.preparations.values().any(|group| {
                group
                    .requested_uris
                    .iter()
                    .any(|requested_uri| super::same_file_uri(requested_uri, uri))
            })
    }

    /// These buffers are outside the successor's restore layout, even when their
    /// exact journal head is durable. No update backstop can transfer them.
    pub(crate) fn unpublished_document_preparations(&self) -> bool {
        self.native_documents.preparations.values().any(|group| {
            !group.published.load(Ordering::Acquire)
                && group
                    .provisional
                    .is_some_and(|document| self.document_store.snapshot(document).is_some())
        })
    }

    pub(super) fn document_preparation_work_pending(&self) -> bool {
        self.native_documents
            .preparations
            .values()
            .any(|group| !group.published.load(Ordering::Acquire))
    }

    pub(super) fn document_preparation_owns(&self, document: DocumentId) -> bool {
        self.native_documents.preparations.values().any(|group| {
            group.provisional == Some(document) && !group.published.load(Ordering::Acquire)
        })
    }

    pub(crate) fn document_preparation_allows_attach(&self, document: DocumentId) -> bool {
        self.native_documents
            .preparations
            .values()
            .filter(|group| group.provisional == Some(document))
            .all(|group| group.provisional_ready)
    }

    pub(super) fn mark_document_preparation_published(&mut self, document: DocumentId) {
        self.document_store.publish_prepared(document);
        self.editor_workspace.publish_recovery(document);
        for group in self
            .native_documents
            .preparations
            .values()
            .filter(|group| group.provisional == Some(document))
        {
            group.published.store(true, Ordering::Release);
        }
    }

    /// Detached leaves share the current read/write preparation rather than queue behind
    /// an unpublished sibling whose lease cannot be released until the whole tree publishes.
    pub(super) fn prepare_restore_document_open(
        &mut self,
        admitted: AdmittedDocument,
        requested_uri: &str,
    ) -> PreparationLease {
        if let Some(group) = self
            .native_documents
            .preparations
            .get(admitted.canonical_uri())
            && let Some(owner) = group.jobs.front()
            && owner.access == GrantAccess::ReadWrite
            && owner.shared.leases.load(Ordering::Acquire) != 0
        {
            return PreparationLease::borrow(&owner.shared);
        }
        self.prepare_document_open_with_policy(
            admitted,
            GrantAccess::ReadWrite,
            requested_uri,
            false,
            None,
            None,
        )
    }

    pub(crate) fn ready_restore_document(&self, uri: &str) -> Option<PreparedDocument> {
        let group = self.native_documents.preparations.get(uri)?;
        let owner = group.jobs.front()?;
        if owner.access != GrantAccess::ReadWrite {
            return None;
        }
        let lease = PreparationLease::borrow(&owner.shared);
        let prepared = lease.poll()?.ok()?;
        (self.document_store.id_for_uri(uri) == Some(prepared.document)
            && self.document_preparation_allows_attach(prepared.document))
        .then_some(prepared)
    }

    /// Queue every caller of one canonical URI behind its current preparation owner.
    /// A queued caller holds no provisional document and performs no journal work.
    #[cfg(all(test, unix))]
    pub(super) fn prepare_document_open(
        &mut self,
        admitted: AdmittedDocument,
        access: GrantAccess,
        requested_uri: &str,
    ) -> PreparationLease {
        self.prepare_document_open_with_policy(admitted, access, requested_uri, true, None, None)
    }

    pub(super) fn prepare_document_request(
        &mut self,
        admitted: AdmittedDocument,
        access: GrantAccess,
        requested_uri: &str,
        required_document: Option<DocumentId>,
        guard: impl Fn(&App) -> bool + 'static,
    ) -> PreparationLease {
        let access = if required_document.is_some() {
            GrantAccess::ReadOnly
        } else {
            access
        };
        self.prepare_document_open_with_policy(
            admitted,
            access,
            requested_uri,
            true,
            required_document,
            Some(Box::new(guard)),
        )
    }

    fn prepare_document_open_with_policy(
        &mut self,
        admitted: AdmittedDocument,
        access: GrantAccess,
        requested_uri: &str,
        refresh_directory: bool,
        required_document: Option<DocumentId>,
        guard: Option<RefreshGuard>,
    ) -> PreparationLease {
        let uri = admitted.canonical_uri().to_string();
        let group = self.native_documents.preparations.entry(uri).or_default();
        group.requested_uris.insert(requested_uri.to_owned());
        let shared = Arc::new(Shared {
            leases: AtomicUsize::new(1),
            result: Mutex::new(None),
            proxy: self.proxy.clone(),
        });
        let owner = refresh_directory.then(|| RefreshOwner(Arc::new(AtomicBool::new(true))));
        let refresh_requester = owner.as_ref().map(|owner| RefreshWitness {
            live: Arc::clone(&owner.0),
            guard,
        });
        let lease = PreparationLease(Arc::clone(&shared), owner);
        group.jobs.push_back(Preparation {
            admitted,
            access,
            refresh_requester,
            request_effect: OriginalRequestEffect::NoDirectoryRefresh,
            required_document,
            shared,
            stage: Stage::Queued,
            expected: None,
            provisional: None,
            granted: None,
            waited_from: 0,
        });
        self.advance_document_preparations(false);
        lease
    }

    /// Advance ready worker results only. Filesystem operations and waits stay on workers.
    pub(crate) fn progress_document_preparations(&mut self) {
        self.advance_document_preparations(true);
    }

    pub(super) fn advance_document_preparations(&mut self, dispatch: bool) {
        let mut retry_slots = 4usize.saturating_sub(self.preparation_initializers());
        let keys: Vec<_> = self.native_documents.preparations.keys().cloned().collect();
        for uri in keys {
            let mut group = self
                .native_documents
                .preparations
                .remove(&uri)
                .unwrap_or_default();
            self.advance_preparation_recovery(&uri, &mut group, &mut retry_slots);
            let mut queue = std::mem::take(&mut group.jobs);
            let mut retained = VecDeque::new();
            while let Some(front) = queue.front_mut() {
                let abandoned = front.shared.leases.load(Ordering::Acquire) == 0;
                if abandoned && matches!(front.stage, Stage::Queued | Stage::Waiting | Stage::Ready)
                {
                    queue.pop_front();
                    continue;
                }
                match &front.stage {
                    Stage::Queued => {
                        front.expected = self.document_store.id_for_uri(&uri);
                        front.request_effect = if front.expected.is_some()
                            && front.refresh_requester.is_some()
                            && front.admitted.is_directory()
                        {
                            OriginalRequestEffect::DirectoryRefreshRefused
                        } else {
                            OriginalRequestEffect::NoDirectoryRefresh
                        };
                        if let Some(required) = front.required_document {
                            if !front.admitted.is_directory() {
                                Self::finish_preparation(
                                    front,
                                    Err("The path is no longer a directory".into()),
                                );
                                break;
                            }
                            if front.expected != Some(required)
                                || !self.directory_refresh_live(required)
                                || (group.provisional == Some(required)
                                    && !group.published.load(Ordering::Acquire))
                            {
                                Self::finish_preparation(
                                    front,
                                    Err("Directory view closed before refresh completed".into()),
                                );
                                break;
                            }
                        }
                        let existing = self.native_documents.grants.grant_for_uri(&uri).cloned();
                        let admitted = front.admitted.clone();
                        let access = front.access;
                        let opening = self.native_documents.journals.host_for_worker();
                        let needs_journal = front.expected.is_none();
                        // Preserve intent through reversible action predicates.
                        // Only owner death is monotone; the App guard is checked at commit.
                        let directory = if front
                            .refresh_requester
                            .as_ref()
                            .is_some_and(RefreshWitness::owned)
                            && admitted.is_directory()
                            && admitted.prepared_text.has_graph()
                        {
                            front
                                .expected
                                .filter(|document| {
                                    group.provisional != Some(*document)
                                        || group.published.load(Ordering::Acquire)
                                })
                                .and_then(|document| {
                                    let base = self.document_store.directory_preparation_base(
                                        document,
                                        &admitted.prepared_text,
                                    )?;
                                    let mut views = self
                                        .document_native_views(document)
                                        .into_iter()
                                        .filter_map(|(_, _, view)| {
                                            match self.native_runtime.view_state(view) {
                                                Some(crate::native_app::AppViewState::Editor(
                                                    state,
                                                )) => state.buffer.as_ref().map(|buffer| {
                                                    (view, buffer.directory_coordinates())
                                                }),
                                                _ => None,
                                            }
                                        })
                                        .collect::<Vec<_>>();
                                    // Presentation order is not part of view freshness.
                                    // Sorting retains duplicate identity evidence too.
                                    views.sort_unstable_by_key(|(view, _)| *view);
                                    Some((base, views))
                                })
                        } else {
                            None
                        };
                        front.stage = Stage::Inspecting(worker(
                            "aterm-document-prepare",
                            &front.shared,
                            move || {
                                let grant = DocumentGrantStore::prepare_admitted(
                                    admitted.clone(),
                                    access,
                                    existing,
                                )
                                .map_err(|error| error.to_string())?;
                                #[cfg(all(target_os = "macos", any(test, debug_assertions)))]
                                crate::restore_measurement::wait_for_reopen_journal(
                                    admitted.canonical_uri(),
                                );
                                let host = opening.wait().clone()?;
                                let decision = if !needs_journal {
                                    None
                                } else if crate::native_document_journal::private_uri(
                                    admitted.canonical_uri(),
                                ) {
                                    host.purge(admitted.canonical_uri())?;
                                    None
                                } else {
                                    Some(host.inspect_open(
                                        admitted.canonical_uri(),
                                        admitted.text().as_bytes(),
                                    )?)
                                };
                                let recovery = decision
                                    .as_ref()
                                    .and_then(|decision| decision.recovered_text.as_ref())
                                    .map(|text| {
                                        amacs::editor::PreparedRecovery::new(
                                            &grant.prepared_text(),
                                            text,
                                        )
                                    })
                                    .transpose()
                                    .map_err(|error| format!("{error:?}"))?
                                    .flatten();
                                let directory = directory
                                    .map(|(base, views)| {
                                        let (ids, coordinates): (Vec<_>, Vec<_>) =
                                            views.into_iter().unzip();
                                        base.prepare(grant.prepared_text(), coordinates)
                                            .map(|update| InspectedDirectory { update, views: ids })
                                            .map_err(str::to_owned)
                                    })
                                    .transpose()?;
                                grant.prepared_text().discard_recovery_reservation();
                                Ok(Inspected {
                                    grant,
                                    host,
                                    decision,
                                    recovery,
                                    directory,
                                })
                            },
                        ));
                        break;
                    }
                    Stage::Inspecting(receiver) => {
                        let Some(result) = completed(receiver) else {
                            break;
                        };
                        if abandoned {
                            queue.pop_front();
                            continue;
                        }
                        let inspected = match result {
                            Ok(inspected) => inspected,
                            Err(error) => {
                                let words = self.unrooted_words(&error);
                                Self::finish_preparation(front, Err(words.into()));
                                break;
                            }
                        };
                        if self.document_store.id_for_uri(&uri) != front.expected {
                            if front.required_document.is_some() {
                                Self::finish_preparation(
                                    front,
                                    Err("The directory changed while refreshing".into()),
                                );
                                break;
                            }
                            front.stage = Stage::Queued;
                            continue;
                        }
                        if let Some(required) = front.required_document
                            && !self.directory_refresh_live(required)
                        {
                            Self::finish_preparation(
                                front,
                                Err("Directory view closed before refresh completed".into()),
                            );
                            break;
                        }
                        let granted =
                            match self.native_documents.grants.grant_prepared(inspected.grant) {
                                Ok(granted) => granted,
                                Err(_) => {
                                    front.stage = Stage::Queued;
                                    continue;
                                }
                            };
                        if let Some(document) = front.expected {
                            if let Some(directory) = inspected.directory.filter(|_| {
                                front
                                    .refresh_requester
                                    .as_ref()
                                    .is_some_and(|requester| requester.live(self))
                            }) {
                                if self.native_documents.persistence.grant(document)
                                    != Some(granted.grant.id)
                                {
                                    Self::finish_preparation(
                                        front,
                                        Err("The directory grant changed while opening".into()),
                                    );
                                    break;
                                }
                                let mut current = self
                                    .document_native_views(document)
                                    .into_iter()
                                    .filter_map(|(_, _, view)| {
                                        match self.native_runtime.view_state(view) {
                                            Some(crate::native_app::AppViewState::Editor(
                                                state,
                                            )) if state.buffer.is_some() => Some(view),
                                            _ => None,
                                        }
                                    })
                                    .collect::<Vec<_>>();
                                current.sort_unstable();
                                let matches = current.len() == directory.views.len()
                                    && current
                                        .iter()
                                        .zip(
                                            directory
                                                .views
                                                .iter()
                                                .zip(directory.update.coordinates()),
                                        )
                                        .all(|(view, (expected_view, prepared))| {
                                            view == expected_view
                                                && match self.native_runtime.view_state(*view) {
                                                    Some(
                                                        crate::native_app::AppViewState::Editor(
                                                            state,
                                                        ),
                                                    ) => state.buffer.as_ref().is_some_and(
                                                        |buffer| {
                                                            buffer.matches_directory_coordinates(
                                                                prepared.source(),
                                                            )
                                                        },
                                                    ),
                                                    _ => false,
                                                }
                                        });
                                if !matches {
                                    Self::finish_preparation(
                                        front,
                                        Err("The directory changed while opening".into()),
                                    );
                                    break;
                                }
                                let (seq, coordinates) = match self
                                    .document_store
                                    .adopt_directory_update(document, directory.update)
                                {
                                    Ok(outcome) => outcome,
                                    Err(_) => {
                                        Self::finish_preparation(
                                            front,
                                            Err("The directory changed while opening".into()),
                                        );
                                        break;
                                    }
                                };
                                front.request_effect = OriginalRequestEffect::DirectoryRefreshed;
                                for (view, prepared) in directory.views.into_iter().zip(coordinates)
                                {
                                    let mut route_native_guard =
                                        self.native_runtime.view_state_mut(view);
                                    let Some(crate::native_app::AppViewState::Editor(state)) =
                                        route_native_guard.as_deref_mut()
                                    else {
                                        unreachable!("validated directory view");
                                    };
                                    assert!(
                                        state
                                            .buffer
                                            .as_mut()
                                            .expect("validated directory buffer")
                                            .adopt_directory_coordinates(prepared, seq),
                                        "exclusive directory adoption"
                                    );
                                }
                                self.repaint_document_views(document);
                            }
                            if group.provisional == Some(document) && !group.provisional_ready {
                                front.granted = Some(granted);
                                if let Some(owner) = group.recovery.as_mut() {
                                    front.waited_from = owner.failures;
                                    owner.hurry(preparation_now());
                                }
                                front.stage = Stage::Waiting;
                                break;
                            }
                            let recovery_notice = (group.provisional == Some(document))
                                .then(|| group.recovery_notice.clone())
                                .flatten();
                            Self::finish_preparation(
                                front,
                                Ok(PreparedValue {
                                    granted,
                                    document,
                                    existed: true,
                                    recovery_notice,
                                }),
                            );
                            break;
                        }
                        let document = match self
                            .document_store
                            .open_prepared(uri.clone(), granted.prepared_text.clone())
                        {
                            Ok(document) => document,
                            Err(_) => {
                                Self::finish_preparation(
                                    front,
                                    Err("The document changed while opening".into()),
                                );
                                break;
                            }
                        };
                        if group.provisional != Some(document) {
                            group.provisional_ready = false;
                            group.recovery_notice = None;
                            group.published.store(false, Ordering::Release);
                        }
                        group.provisional = Some(document);
                        front.provisional = Some(document);
                        front.granted = Some(granted.clone());
                        let disk = self
                            .document_store
                            .snapshot(document)
                            .expect("just opened provisional document");
                        let Some(decision) = inspected.decision else {
                            self.native_documents
                                .journals
                                .as_mut()
                                .expect("worker opened journals")
                                .adopt_declined(document);
                            group.provisional_ready = true;
                            Self::finish_preparation(
                                front,
                                Ok(PreparedValue {
                                    granted,
                                    document,
                                    existed: false,
                                    recovery_notice: None,
                                }),
                            );
                            break;
                        };
                        if let Some(recovery) = inspected.recovery {
                            if self
                                .editor_workspace
                                .adopt_recovery(
                                    &mut self.document_store,
                                    document,
                                    disk.seq,
                                    recovery,
                                )
                                .is_err()
                            {
                                // No edit was installed and no worker can publish an image.
                                if self.document_store.snapshot(document).is_some_and(|head| {
                                    head.seq == disk.seq && Arc::ptr_eq(&head.text, &disk.text)
                                }) && self.document_store.view_count(document) == Some(0)
                                {
                                    self.remove_prepared_document_edges(document);
                                    group.provisional = None;
                                    front.provisional = None;
                                } else {
                                    group.recovery = Some(RecoveryOwner {
                                        disk: disk.clone(),
                                        granted: granted.clone(),
                                        host: inspected.host.clone(),
                                        recovered_text: decision.recovered_text.clone(),
                                        witnessed_text: decision.recovered_text.clone(),
                                        attempted_text: disk.text.clone(),
                                        retry_delay: RECOVERY_RETRY,
                                        wake: Arc::clone(&front.shared),
                                        phase: RecoveryPhase::NeedJournal(
                                            preparation_now() + RECOVERY_RETRY,
                                        ),
                                        failures: 0,
                                        fault: None,
                                    });
                                }
                                Self::finish_preparation(
                                    front,
                                    Err("recovered draft could not be installed atomically".into()),
                                );
                                break;
                            }
                            self.native_documents
                                .unvouched
                                .insert(document, crate::owner_keys_save::Unvouched::Recovered);
                        }
                        let current = self
                            .document_store
                            .snapshot(document)
                            .expect("provisional document is retained");
                        let worker_disk = disk.clone();
                        let worker_current = current.clone();
                        let host = inspected.host.clone();
                        let recovered_text = decision.recovered_text.clone();
                        #[cfg(test)]
                        let initialize_gate = INITIALIZE_GATE.take();
                        #[cfg(not(test))]
                        let delivery = None;
                        #[cfg(test)]
                        let delivery = INITIALIZE_DELIVERY_GATE.take();
                        let receiver =
                            worker("aterm-document-initialize", &front.shared, move || {
                                #[cfg(test)]
                                if let Some(gate) = initialize_gate {
                                    gate();
                                }
                                Ok(initialize_attempt(
                                    &inspected.host,
                                    decision,
                                    &worker_disk,
                                    &worker_current,
                                    delivery,
                                ))
                            });
                        group.recovery = Some(RecoveryOwner {
                            disk,
                            granted,
                            host,
                            recovered_text,
                            witnessed_text: None,
                            attempted_text: current.text.clone(),
                            retry_delay: RECOVERY_RETRY,
                            wake: Arc::clone(&front.shared),
                            phase: RecoveryPhase::Initializing {
                                receiver,
                                captured: current,
                            },
                            failures: 0,
                            fault: None,
                        });
                        front.waited_from = 0;
                        front.stage = Stage::Waiting;
                        break;
                    }
                    Stage::Waiting => {
                        let Some(document) = group.provisional else {
                            Self::finish_preparation(
                                front,
                                Err("provisional document disappeared".into()),
                            );
                            break;
                        };
                        if front.provisional.or(front.expected) != Some(document) {
                            Self::finish_preparation(
                                front,
                                Err("provisional document identity changed".into()),
                            );
                            break;
                        }
                        if !group.provisional_ready {
                            // The owner keeps the provisional document and keeps retrying;
                            // only this request stops waiting, and says why.
                            if let Some(fault) = group
                                .recovery
                                .as_ref()
                                .and_then(|owner| owner.refusal(front.waited_from))
                            {
                                let failure = super::journal_failure(
                                    self.native_documents.journals.as_ref(),
                                    fault,
                                );
                                Self::finish_preparation(front, Err(failure));
                            }
                            break;
                        }
                        let granted = front.granted.take().expect("waiting preparation grant");
                        Self::finish_preparation(
                            front,
                            Ok(PreparedValue {
                                granted,
                                document,
                                existed: front.expected.is_some(),
                                recovery_notice: group.recovery_notice.clone(),
                            }),
                        );
                        if abandoned {
                            queue.pop_front();
                            continue;
                        }
                        break;
                    }
                    Stage::Ready => retained.push_back(queue.pop_front().expect("ready owner")),
                }
            }
            retained.append(&mut queue);
            group.jobs = retained;
            if let Some(document) = group.provisional {
                let interested = group.jobs.iter().any(|job| {
                    let same = job.provisional == Some(document)
                        || job.expected == Some(document)
                        || matches!(job.stage, Stage::Queued);
                    same && job.shared.leases.load(Ordering::Acquire) > 0
                });
                if !interested && !group.published.load(Ordering::Acquire) {
                    if self.retire_unpublished_prepared_document(
                        &uri,
                        document,
                        group.provisional_ready,
                    ) {
                        group.provisional = None;
                        group.provisional_ready = false;
                        group.recovery = None;
                        group.recovery_notice = None;
                    } else if group.provisional_ready
                        && self.document_store.view_count(document) == Some(0)
                        && let Some(owner) = group.recovery.as_mut()
                    {
                        // An affirmative forget is the commit point. A refusal
                        // without a future write completion still needs a wake.
                        owner.phase = RecoveryPhase::Journaling {
                            retry_at: Some(owner.retry_at(preparation_now())),
                        };
                        group.provisional_ready = false;
                    }
                }
            }
            if !group.jobs.is_empty()
                || (group.provisional.is_some() && !group.published.load(Ordering::Acquire))
            {
                self.native_documents.preparations.insert(uri, group);
            }
        }
        if self.native_documents.pending_quit.is_some() {
            self.native_documents.close_due = true;
        }
        if !dispatch {
            return;
        }
        self.publish_prepared_admissions();
    }

    /// The journal write of `document`, which a preparation owns, failed for `why`.
    pub(super) fn preparation_journal_failed(&mut self, document: DocumentId, why: &str) {
        for group in self.native_documents.preparations.values_mut() {
            if group.provisional == Some(document)
                && !group.published.load(Ordering::Acquire)
                && let Some(owner) = group.recovery.as_mut()
                && matches!(owner.phase, RecoveryPhase::Journaling { .. })
            {
                group.provisional_ready = false;
                owner.phase = RecoveryPhase::Journaling {
                    retry_at: Some(
                        owner.failed(JournalFault::Other(why.to_string()), preparation_now()),
                    ),
                };
            }
        }
    }

    fn preparation_initializers(&self) -> usize {
        self.native_documents
            .preparations
            .values()
            .filter(|group| {
                group
                    .recovery
                    .as_ref()
                    .is_some_and(|owner| matches!(owner.phase, RecoveryPhase::Initializing { .. }))
            })
            .count()
    }

    /// Pure deadline query. Inflight workers wake the loop when retry capacity frees.
    pub(crate) fn document_preparation_wake(&self) -> Option<Instant> {
        let capacity = self.preparation_initializers() < 4;
        self.native_documents
            .preparations
            .values()
            .filter_map(|group| match &group.recovery.as_ref()?.phase {
                RecoveryPhase::NeedJournal(at) if capacity => Some(*at),
                RecoveryPhase::Journaling { retry_at } => *retry_at,
                _ => None,
            })
            .min()
    }

    /// Service only an expired retry, before the loop plans any close or deadline.
    pub(crate) fn service_due_document_preparations(&mut self) {
        if self
            .document_preparation_wake()
            .is_some_and(|at| at <= preparation_now())
        {
            self.progress_document_preparations();
        }
    }

    fn advance_preparation_recovery(
        &mut self,
        uri: &str,
        group: &mut PreparationGroup,
        retry_slots: &mut usize,
    ) {
        let Some(owner) = group.recovery.as_mut() else {
            return;
        };
        let document = owner.disk.id;
        if owner.granted.grant.canonical_uri != uri
            || self.document_store.canonical_uri(document) != Some(uri)
        {
            group.recovery = None;
            group.provisional = None;
            group.provisional_ready = false;
            return;
        }
        if group.published.load(Ordering::Acquire) {
            // Publication transfers journal/retry authority to the normal document
            // owner. Later shared opens must not await ordinary autosave.
            group.recovery = None;
            group.provisional_ready = true;
            return;
        }
        let Some(head) = self.document_store.snapshot(document) else {
            return;
        };
        let now = preparation_now();
        match &owner.phase {
            RecoveryPhase::Initializing { receiver, captured } => {
                let Some(result) = completed(receiver) else {
                    return;
                };
                let captured = captured.clone();
                let adopted = result
                    .map_err(JournalFault::Other)
                    .and_then(|outcome| {
                        owner.witnessed_text = outcome.witnessed_text;
                        owner.attempted_text = captured.text.clone();
                        outcome.result
                    })
                    .and_then(|initialized| {
                        self.native_documents
                            .journals
                            .as_mut()
                            .ok_or_else(|| "document recovery host disappeared".to_string())?
                            .adopt_initialized(&initialized, uri, &owner.disk, &captured)?;
                        self.native_documents
                            .journals
                            .as_mut()
                            .expect("adopted store")
                            .request_legacy_retirement(document);
                        // The result certifies only captured. Record newest immediately,
                        // before readiness or rollback can become observable.
                        group.recovery_notice = initialized.notice;
                        Ok(())
                    });
                if let Err(fault) = adopted {
                    self.set_document_recovery_status(document, Some(fault.to_string()));
                    owner.phase = RecoveryPhase::NeedJournal(owner.failed(fault, now));
                    return;
                }
                owner.phase = RecoveryPhase::Journaling { retry_at: None };
            }
            RecoveryPhase::NeedJournal(at) => {
                if now < *at || *retry_slots == 0 {
                    return;
                }
                *retry_slots -= 1;
                let host = owner.host.clone();
                let disk = owner.disk.clone();
                let captured = head.clone();
                let current = head.clone();
                let original_recovered = owner.recovered_text.clone();
                let witnessed = owner.witnessed_text.clone();
                let attempted = owner.attempted_text.clone();
                #[cfg(not(test))]
                let delivery = None;
                #[cfg(test)]
                let delivery = INITIALIZE_DELIVERY_GATE.take();
                let uri = uri.to_owned();
                let receiver = worker("aterm-document-recover", &owner.wake, move || {
                    let decision = host.inspect_open(&uri, disk.text.as_bytes())?;
                    // A retry must not replace a different recovered draft. Retain
                    // both owners until admission can establish their relationship.
                    if let Some(recovered) = &decision.recovered_text {
                        if Some(recovered) != original_recovered.as_ref()
                            && Some(recovered) != witnessed.as_ref()
                            && recovered.as_ref() != attempted.as_ref()
                            && recovered.as_ref() != current.text.as_ref()
                        {
                            return Err("recovery retry found an unrelated draft".into());
                        }
                    } else if decision.notice.is_some() {
                        return Err("recovery retry could not validate the existing image".into());
                    }
                    Ok(initialize_attempt(
                        &host, decision, &disk, &current, delivery,
                    ))
                });
                owner.phase = RecoveryPhase::Initializing { receiver, captured };
                return;
            }
            RecoveryPhase::Journaling { retry_at: Some(at) } if now < *at => return,
            RecoveryPhase::Journaling { .. } => {}
        }
        group.provisional_ready = false;
        let result = self
            .native_documents
            .journals
            .as_mut()
            .ok_or_else(|| "document recovery host disappeared".to_owned())
            .and_then(|journals| journals.observe_commit(&head))
            .and_then(|()| self.drive_document_journal(document));
        if let Err(message) = result {
            self.set_document_recovery_status(document, Some(message.clone()));
            owner.phase = RecoveryPhase::Journaling {
                retry_at: Some(owner.failed(JournalFault::Other(message), now)),
            };
            return;
        }
        group.provisional_ready = self
            .native_documents
            .journals
            .as_ref()
            .is_some_and(|journals| {
                journals.settled(document) && journals.carry_candidate(&head).is_ok()
            });
        owner.phase = RecoveryPhase::Journaling { retry_at: None };
        if group.provisional_ready {
            owner.retry_delay = RECOVERY_RETRY;
            owner.fault = None;
            self.set_document_recovery_status(document, None);
        }
    }

    fn publish_prepared_admissions(&mut self) {
        let completed: Vec<_> = self
            .native_documents
            .admissions
            .iter()
            .filter_map(|(key, pending)| {
                Some((key.clone(), pending.preparation.as_ref()?.poll_request()?))
            })
            .collect();
        for ((wid, key), result) in completed {
            let Some(pending) = self
                .native_documents
                .admissions
                .get_mut(&(wid, key.clone()))
            else {
                continue;
            };
            pending.preparation = None;
            let Some(admitted) = pending.admitted.take() else {
                continue;
            };
            // The read stands either way: a preparation that refused is the install's failure,
            // typed, with the file's canonical URI still known.
            pending.prepared = Some(result);
            let outcome = super::DocumentAdmissionOutcome {
                ticket: pending.ticket,
                wid,
                key,
                result: Ok(admitted),
            };
            if let Some(delivery) = self.document_admission_delivery() {
                delivery.send(outcome);
            } else {
                let _ = self.complete_document_admission(outcome);
            }
        }
    }

    fn finish_preparation(job: &mut Preparation, result: Result<PreparedValue, InstallFailure>) {
        let result = result.map(|readiness| PreparedResult {
            readiness,
            effect: job.request_effect,
        });
        *job.shared
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        job.stage = Stage::Ready;
        job.shared.wake();
    }

    /// Retirement is one exclusive memory transition. Missing entries are not
    /// durability evidence; forget must acknowledge before any UI edge disappears.
    fn retire_unpublished_prepared_document(
        &mut self,
        uri: &str,
        document: DocumentId,
        ready: bool,
    ) -> bool {
        if self.document_store.canonical_uri(document) != Some(uri) {
            // The old identity disappeared. Never target its replacement by URI.
            return true;
        }
        let host = &self.native_documents;
        if !ready
            || self.document_store.view_count(document) != Some(0)
            || self.native_runtime.holds_document(document)
            || host.persistence.save_in_flight(document)
            || host.inflight.contains(&document)
            || host.pending_saves.contains_key(&document)
            || host.pending_closes.contains_key(&document)
            || matches!(
                self.document_store.phase(document),
                Some(
                    crate::document_store::DocumentPhase::Closing { .. }
                        | crate::document_store::DocumentPhase::Blocked { .. }
                )
            )
        {
            return false;
        }
        let Some(head) = self.document_store.snapshot(document) else {
            return false;
        };
        let Some(journals) = self.native_documents.journals.as_mut() else {
            return false;
        };
        if !journals.settled(document)
            || (!journals.declined(document) && journals.carry_candidate(&head).is_err())
            || !journals.forget(&[document])
        {
            return false;
        }
        self.remove_prepared_document_edges(document);
        true
    }

    fn remove_prepared_document_edges(&mut self, document: DocumentId) {
        let uri = self
            .document_store
            .canonical_uri(document)
            .map(str::to_owned);
        if self.document_store.remove_if_unattached(document).is_err() {
            return;
        }
        let host = &mut self.native_documents;
        if let Some(grant) = host.persistence.forget(document) {
            host.grants.forget(grant);
        } else if let Some(granted) = uri
            .as_deref()
            .and_then(|uri| host.grants.grant_for_uri(uri))
            .map(|grant| grant.id)
        {
            host.grants.forget(granted);
        }
        host.recovery_status.remove(&document);
        host.disk_conflicts.remove(&document);
        host.unvouched.remove(&document);
        host.journal_retries.remove(document);
        host.watch.unwatch(document);
        self.editor_workspace.forget(document);
    }
}

#[cfg(all(test, unix))]
#[path = "app_document_preparation_tests.rs"]
pub(super) mod tests;
