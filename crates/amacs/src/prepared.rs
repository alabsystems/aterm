// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable preparation, one-shot storage transfer, and reserved retirement.

use crate::store::DirectoryListing;
use crate::text::TextRope;
use std::collections::LinkedList;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreparationWork {
    pub projection_input_bytes: usize,
    pub rope_input_bytes: usize,
    pub rope_leaf_input_bytes: usize,
    pub decoded_input_bytes: usize,
    pub baseline_input_bytes: usize,
    pub diff_input_bytes: usize,
    pub inverse_bytes: usize,
    pub adoptions: usize,
    pub retirement_transfers: usize,
    pub retirement_nodes_reserved: usize,
    pub retired_rope_nodes: usize,
    pub retired_directory_entries: usize,
    pub directory_entry_visits: usize,
    pub retired_history_edits: usize,
}
#[cfg(any(test, debug_assertions))]
std::thread_local! {
    static WORK: std::cell::Cell<PreparationWork> = const { std::cell::Cell::new(PreparationWork {
        projection_input_bytes: 0, rope_input_bytes: 0, rope_leaf_input_bytes: 0, decoded_input_bytes: 0, baseline_input_bytes: 0,
        diff_input_bytes: 0, inverse_bytes: 0, adoptions: 0,
        retirement_transfers: 0, retirement_nodes_reserved: 0, retired_rope_nodes: 0, retired_directory_entries: 0, directory_entry_visits: 0, retired_history_edits: 0,
    }) };
}
#[cfg(any(test, debug_assertions))]
pub fn take_preparation_work() -> PreparationWork {
    WORK.replace(PreparationWork::default())
}
#[cfg(any(test, debug_assertions))]
pub fn record_preparation_work(work: PreparationWork) {
    WORK.with(|counter| {
        let mut total = counter.get();
        macro_rules! add { ($($field:ident),* $(,)?) => { $(total.$field = total.$field.saturating_add(work.$field);)* }; }
        add!(projection_input_bytes, rope_input_bytes, rope_leaf_input_bytes, decoded_input_bytes, baseline_input_bytes, diff_input_bytes, inverse_bytes, adoptions, retirement_transfers, retirement_nodes_reserved, retired_rope_nodes, retired_directory_entries, directory_entry_visits, retired_history_edits);
        counter.set(total);
    });
}
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug)]
pub struct RetirementObservation {
    pub executor: u64,
    pub work: PreparationWork,
}
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, Default)]
pub struct RetirementReceipt(Arc<(Mutex<Option<RetirementObservation>>, Condvar)>);
#[cfg(any(test, debug_assertions))]
impl RetirementReceipt {
    pub fn poll(&self) -> Option<RetirementObservation> {
        self.0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    #[cfg(test)]
    pub fn wait(&self) -> RetirementObservation {
        let observed = self.0.0.lock().unwrap();
        let (observed, _) = self
            .0
            .1
            .wait_timeout_while(observed, aterm_spec::wait::MUST_HAPPEN, |observed| {
                observed.is_none()
            })
            .unwrap();
        observed.clone().expect("retirement did not finish")
    }
}
/// Zero-sized in shipped code; diagnostics never enter its hot paths.
#[derive(Clone, Debug, Default)]
pub(crate) struct Evidence {
    #[cfg(any(test, debug_assertions))]
    receipt: RetirementReceipt,
}
impl Evidence {
    fn release(self, executor: RetirementExecutor, drop_value: impl FnOnce()) {
        #[cfg(not(any(test, debug_assertions)))]
        let _ = executor;
        #[cfg(any(test, debug_assertions))]
        let prior = take_preparation_work();
        drop_value();
        #[cfg(any(test, debug_assertions))]
        {
            let work = take_preparation_work();
            record_preparation_work(prior);
            record_preparation_work(work);
            *self
                .receipt
                .0
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(RetirementObservation {
                executor: executor.identity,
                work,
            });
            self.receipt.0.1.notify_all();
        }
    }
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn receipt(&self) -> RetirementReceipt {
        self.receipt.clone()
    }
}

/// Host-supplied diagnostic identity; no runtime storage in shipped builds.
#[derive(Clone, Copy, Debug, Default)]
pub struct RetirementExecutor {
    #[cfg(any(test, debug_assertions))]
    pub identity: u64,
}

type Job = Box<dyn FnOnce(RetirementExecutor) + Send>;
struct Queue {
    pending: LinkedList<Option<Job>>,
    retiring: bool,
    constructing: usize,
    stopped: bool,
}
struct Pool {
    queue: Mutex<Queue>,
    ready: Condvar,
    available: Condvar,
    concurrency: usize,
}
impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedRetirementPool")
    }
}
/// Pure queue ownership. A host must start its drainer before supplying reservations.
#[derive(Clone, Debug)]
pub struct RetirementService(Arc<Producer>);
/// Non-owning host registry entry; it cannot keep an idle executor alive.
#[derive(Clone, Debug, Default)]
pub struct WeakRetirementService(Weak<Producer>);
impl WeakRetirementService {
    pub fn upgrade(&self) -> Option<RetirementService> {
        self.0.upgrade().map(RetirementService)
    }
}
#[derive(Debug)]
struct Producer {
    pool: Arc<Pool>,
}
impl std::ops::Deref for Producer {
    type Target = Pool;
    fn deref(&self) -> &Pool {
        &self.pool
    }
}
impl Drop for Producer {
    fn drop(&mut self) {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped = true;
        self.ready.notify_all();
    }
}
/// Unique drain authority, retained by the host executor.
pub struct RetirementDrainer(Arc<Pool>);
impl RetirementDrainer {
    pub fn run(self, executor: RetirementExecutor) {
        self.0.run(executor);
    }
    /// Deterministic executor step, also used by pure engine controls.
    pub fn drain_one(&mut self, executor: RetirementExecutor) -> bool {
        let job = {
            let mut queue = self
                .0
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(node) = queue.pending.pop_front() else {
                return false;
            };
            queue.retiring = true;
            node.expect("reserved retirement value")
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(executor)));
        let mut queue = self
            .0
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.retiring = false;
        self.0.available.notify_all();
        true
    }
}
impl RetirementService {
    pub fn channel(concurrency: std::num::NonZeroUsize) -> (Self, RetirementDrainer) {
        let pool = Arc::new(Pool {
            queue: Mutex::new(Queue {
                pending: LinkedList::new(),
                retiring: false,
                constructing: 0,
                stopped: false,
            }),
            ready: Condvar::new(),
            available: Condvar::new(),
            concurrency: concurrency.get(),
        });
        (
            Self(Arc::new(Producer { pool: pool.clone() })),
            RetirementDrainer(pool),
        )
    }
    pub fn reserve(&self) -> PreparationReservation {
        self.0.reserve()
    }
    pub fn downgrade(&self) -> WeakRetirementService {
        WeakRetirementService(Arc::downgrade(&self.0))
    }
}
impl Pool {
    fn begin(self: &Arc<Self>, producer: Arc<Producer>) -> Construction {
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Live unpublished graphs do not hold construction capacity. A complete
        // topology can stage arbitrarily many leaves without waiting on its prefix.
        // Once retirement begins, no new construction enters until it drains.
        // Its backlog can therefore contain only already-owned graphs plus the
        // finite set of constructions that entered before that drain began.
        while queue.constructing == self.concurrency || queue.retiring || !queue.pending.is_empty()
        {
            queue = self
                .available
                .wait(queue)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        queue.constructing += 1;
        Construction(producer)
    }
    fn run(self: Arc<Self>, executor: RetirementExecutor) {
        loop {
            let job = {
                let mut queue = self
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while queue.pending.is_empty() {
                    if queue.stopped {
                        return;
                    }
                    queue = self
                        .ready
                        .wait(queue)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                queue.retiring = true;
                queue
                    .pending
                    .pop_front()
                    .expect("owned retirement node")
                    .expect("retirement value")
            };
            // No queue lock is held while visiting a graph or releasing history.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(executor)));
            let mut queue = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            queue.retiring = false;
            self.available.notify_all();
        }
    }
}
impl Producer {
    fn reserve(self: &Arc<Self>) -> PreparationReservation {
        let work = self.pool.begin(self.clone());
        PreparationReservation {
            initial: Permit::new(self.clone()),
            recovered: Permit::new(self.clone()),
            history: Permit::new(self.clone()),
            work,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Construction(Arc<Producer>);
impl Drop for Construction {
    fn drop(&mut self) {
        let mut queue = self
            .0
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.constructing -= 1;
        self.0.available.notify_all();
    }
}
/// Exactly one preallocated queue node belongs to each graph/history. Cloning
/// prepared handles never clones a node. It is unused, or transfers exactly once
/// with its existing graph, and is freed by the worker after that retirement.
/// There is no channel refusal, queue-capacity wait, or per-release thread.
pub(crate) struct Permit {
    pool: Arc<Producer>,
    node: LinkedList<Option<Job>>,
}
impl std::fmt::Debug for Permit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedRetirementNode")
    }
}
impl Permit {
    fn new(pool: Arc<Producer>) -> Self {
        #[cfg(any(test, debug_assertions))]
        record_preparation_work(PreparationWork {
            retirement_nodes_reserved: 1,
            ..Default::default()
        });
        let mut node = LinkedList::new();
        node.push_back(None);
        Self { pool, node }
    }
    pub(crate) fn begin_construction(&self) -> Construction {
        self.pool.pool.begin(self.pool.clone())
    }
    pub(crate) fn retire(mut self, evidence: Evidence, job: impl FnOnce() + Send + 'static) {
        *self.node.front_mut().expect("owned retirement node") =
            Some(Box::new(move |executor| evidence.release(executor, job)));
        let mut queue = self
            .pool
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.pending.append(&mut self.node);
        #[cfg(any(test, debug_assertions))]
        record_preparation_work(PreparationWork {
            retirement_transfers: 1,
            ..Default::default()
        });
        self.pool.ready.notify_one();
    }
}
/// One-shot recovery nodes plus worker-only construction custody.
pub(crate) struct RecoveryReservation {
    pub(crate) text: Option<Permit>,
    pub(crate) history: Option<Permit>,
    pub(crate) work: Option<Construction>,
}
/// Acquire on the admission worker before reading/materializing content. Only
/// construction holds the concurrency guard; unpublished ownership holds nodes.
#[derive(Debug)]
pub struct PreparationReservation {
    initial: Permit,
    recovered: Permit,
    history: Permit,
    work: Construction,
}

#[derive(Debug)]
struct Data {
    line: Arc<String>,
    projection: Arc<str>,
    rope: TextRope,
    directory: Option<Arc<DirectoryListing>>,
}
/// Storage adopted by an unpublished document. It keeps a last-owner witness
/// until publication or transfers destruction to its pre-reserved worker node.
#[derive(Debug)]
pub(crate) struct PendingStorage {
    data: Option<Data>,
    permit: Option<Permit>,
    evidence: Evidence,
}
impl PendingStorage {
    pub(crate) fn parts(
        &self,
    ) -> (
        Arc<String>,
        Arc<str>,
        TextRope,
        Option<Arc<DirectoryListing>>,
    ) {
        let data = self.data.as_ref().expect("live pending storage");
        (
            data.line.clone(),
            data.projection.clone(),
            data.rope.clone(),
            data.directory.clone(),
        )
    }
    pub(crate) fn publish(
        mut self,
        projection: &Arc<str>,
        directory: Option<&Arc<DirectoryListing>>,
    ) {
        let data = self.data.as_ref().expect("live pending storage");
        let same_directory = match (&data.directory, directory) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if Arc::ptr_eq(&data.projection, projection) && same_directory {
            // Every graph root remains held by the now-published canonical document.
            // Returning this permit visits no content and leaves no prepared owner behind.
            drop(self.data.take());
            drop(self.permit.take());
        }
    }
}
impl Drop for PendingStorage {
    fn drop(&mut self) {
        if let Some(data) = self.data.take() {
            if let Some(permit) = self.permit.take() {
                permit.retire(self.evidence.clone(), move || drop(data));
            } else {
                self.evidence
                    .clone()
                    .release(RetirementExecutor::default(), move || drop(data));
            }
        }
    }
}
#[derive(Debug)]
enum RecoverySlots {
    Immediate,
    Reserved(Permit, Permit),
    Consumed,
}
#[derive(Debug)]
struct Shared {
    line: Arc<String>,
    projection: Arc<str>,
    directory: bool,
    worker_prepared: AtomicBool,
    graph: Mutex<Option<PendingStorage>>,
    recovery: Mutex<RecoverySlots>,
    #[cfg(any(test, debug_assertions))]
    evidence: Evidence,
}
/// Clone-cheap immutable decoded content and a one-shot prepared graph transfer.
/// Consuming the graph changes ownership, never its contents. Stale clones cannot
/// rematerialize it on UI; callers must refuse or requeue on a worker.
#[derive(Clone, Debug)]
pub struct PreparedText(Arc<Shared>);
impl PreparedText {
    /// Synchronous wrapper for actual live String callers.
    pub fn new(text: String) -> Self {
        Self::build(Arc::new(text), None, None)
    }
    pub fn from_shared(text: Arc<String>) -> Self {
        Self::build(text, None, None)
    }
    pub fn from_directory(listing: Arc<DirectoryListing>) -> Self {
        Self::build(Arc::new(String::new()), Some(listing), None)
    }
    pub fn prepare_reserved(
        text: Arc<String>,
        listing: Option<Arc<DirectoryListing>>,
        reservation: PreparationReservation,
    ) -> Self {
        Self::build(text, listing, Some(reservation))
    }
    fn build(
        line: Arc<String>,
        directory: Option<Arc<DirectoryListing>>,
        reservation: Option<PreparationReservation>,
    ) -> Self {
        let line = directory
            .as_ref()
            .map_or(line, |listing| Arc::new(listing.text.clone()));
        #[cfg(any(test, debug_assertions))]
        record_preparation_work(PreparationWork {
            projection_input_bytes: line.len(),
            ..Default::default()
        });
        let projection: Arc<str> = Arc::from(line.as_str());
        let rope = TextRope::from(projection.as_ref());
        let (permit, recovery) = match reservation {
            Some(PreparationReservation {
                initial,
                recovered,
                history,
                work: _work,
            }) => (Some(initial), RecoverySlots::Reserved(recovered, history)),
            None => (None, RecoverySlots::Immediate),
        };
        Self::from_data(line, projection, rope, directory, permit, recovery)
    }
    fn from_data(
        line: Arc<String>,
        projection: Arc<str>,
        rope: TextRope,
        directory: Option<Arc<DirectoryListing>>,
        permit: Option<Permit>,
        recovery: RecoverySlots,
    ) -> Self {
        let evidence = Evidence::default();
        let worker_prepared = AtomicBool::new(permit.is_some());
        Self(Arc::new(Shared {
            line: line.clone(),
            projection: projection.clone(),
            directory: directory.is_some(),
            worker_prepared,
            graph: Mutex::new(Some(PendingStorage {
                data: Some(Data {
                    line,
                    projection,
                    rope,
                    directory,
                }),
                permit,
                evidence: evidence.clone(),
            })),
            recovery: Mutex::new(recovery),
            #[cfg(any(test, debug_assertions))]
            evidence,
        }))
    }
    pub fn shared_line(&self) -> Arc<String> {
        self.0.line.clone()
    }
    pub fn projection(&self) -> Arc<str> {
        self.0.projection.clone()
    }
    pub fn is_worker_prepared(&self) -> bool {
        self.0.worker_prepared.load(Ordering::Acquire)
    }
    pub fn is_directory(&self) -> bool {
        self.0.directory
    }
    /// Upgrade a synchronous admission on its preparation worker. Native async
    /// admission reserves before materialization and needs no upgrade.
    pub fn ensure_worker_owned(
        &self,
        reservation: Option<PreparationReservation>,
    ) -> io::Result<()> {
        {
            let graph = self
                .0
                .graph
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if graph.as_ref().is_some_and(|graph| graph.permit.is_some()) {
                return Ok(());
            }
            if graph.is_none() {
                return Err(io::Error::other("prepared storage was already consumed"));
            }
        }
        let PreparationReservation {
            initial,
            recovered,
            history,
            work: _work,
        } = reservation
            .ok_or_else(|| io::Error::other("worker retirement reservation required"))?;
        let mut graph = self
            .0
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let graph = graph
            .as_mut()
            .ok_or_else(|| io::Error::other("prepared storage was already consumed"))?;
        if graph.permit.is_none() {
            graph.permit = Some(initial);
            *self
                .0
                .recovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                RecoverySlots::Reserved(recovered, history);
            self.0.worker_prepared.store(true, Ordering::Release);
        }
        Ok(())
    }
    /// Explicit directory refresh consumes navigation separately from unused text storage.
    /// Ordinary first-open adoption transfers both together through DocumentStore.
    pub fn take_directory_for_refresh(&self) -> Option<Arc<DirectoryListing>> {
        let mut graph = self.take_graph()?;
        graph.data.as_mut()?.directory.take()
    }
    pub fn has_graph(&self) -> bool {
        self.0
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
    pub(crate) fn take_graph(&self) -> Option<PendingStorage> {
        self.0
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
    pub(crate) fn take_directory_retirement_node(&self) -> Option<Permit> {
        let mut slots = self
            .0
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match std::mem::replace(&mut *slots, RecoverySlots::Consumed) {
            RecoverySlots::Reserved(node, unused) => {
                drop(unused);
                Some(node)
            }
            RecoverySlots::Immediate => {
                *slots = RecoverySlots::Immediate;
                None
            }
            RecoverySlots::Consumed => None,
        }
    }
    pub(crate) fn directory_listing_on_worker(&self) -> Option<Arc<DirectoryListing>> {
        self.0
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()?
            .data
            .as_ref()?
            .directory
            .clone()
    }
    pub(crate) fn recovery_permits(&self) -> Result<RecoveryReservation, ()> {
        let mut slots = self
            .0
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match std::mem::replace(&mut *slots, RecoverySlots::Consumed) {
            RecoverySlots::Immediate => {
                *slots = RecoverySlots::Immediate;
                Ok(RecoveryReservation {
                    text: None,
                    history: None,
                    work: None,
                })
            }
            RecoverySlots::Reserved(text, history) => {
                let work = text.begin_construction();
                Ok(RecoveryReservation {
                    text: Some(text),
                    history: Some(history),
                    work: Some(work),
                })
            }
            RecoverySlots::Consumed => Err(()),
        }
    }
    pub fn discard_recovery_reservation(&self) {
        let mut slots = self
            .0
            .recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(std::mem::replace(&mut *slots, RecoverySlots::Consumed));
    }
    pub(crate) fn recovered(text: String, permit: Option<Permit>) -> Self {
        let line = Arc::new(text);
        #[cfg(any(test, debug_assertions))]
        record_preparation_work(PreparationWork {
            projection_input_bytes: line.len(),
            ..Default::default()
        });
        let projection: Arc<str> = Arc::from(line.as_str());
        let rope = TextRope::from(projection.as_ref());
        Self::from_data(
            line,
            projection,
            rope,
            None,
            permit,
            RecoverySlots::Consumed,
        )
    }
    #[cfg(any(test, debug_assertions))]
    pub fn retirement_receipt(&self) -> RetirementReceipt {
        self.0.evidence.receipt()
    }
}

#[cfg(test)]
#[path = "prepared_tests.rs"]
pub(crate) mod tests;
