// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::Principal;
use crate::session_timeline::scoped_input_tests::{install, scoped, ticket};
use crate::session_timeline::{self, FencedWrite};
use aterm_agent::harness::native_act::Reply;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Barrier, mpsc};

#[cfg(unix)]
#[path = "native_deferred_restart_tests.rs"]
mod deferred_restart;
#[cfg(unix)]
#[path = "native_restart_slots_tests.rs"]
mod restart_slots;

#[path = "native_closed_restart_tests.rs"]
mod closed_restart;
#[path = "native_closed_restart_model_tests.rs"]
mod closed_restart_model;
#[path = "native_continuation_phase_tests.rs"]
mod continuation_phase;

#[path = "native_account_continuation_tests.rs"]
mod account_continuation_tests;
#[cfg(unix)]
#[path = "native_resume_only_tests.rs"]
mod resume_only;

#[path = "native_cancel_prepare_tests.rs"]
mod initial_cancel;

#[path = "native_constructor_capability_tests.rs"]
mod constructor_capabilities;

#[cfg(unix)]
#[path = "native_account_execution_tests.rs"]
mod account_execution;
#[cfg(unix)]
#[path = "native_issuer_tests.rs"]
mod issuer;
#[cfg(unix)]
#[path = "native_phase_receipt_tests.rs"]
mod phase_receipts;
#[cfg(unix)]
#[path = "native_publication_tests.rs"]
mod publication;
#[cfg(unix)]
#[path = "native_receipt_tests.rs"]
mod receipts;

const PATIENCE: Duration = Duration::from_secs(60);

type PublicationGate = (mpsc::SyncSender<()>, Arc<(Mutex<bool>, std::sync::Condvar)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalEvent {
    Prepare,
    MayExecute,
    NoExec(ImagePhase),
    Seal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalState {
    Prepared,
    MayExecute,
    NoExec,
    Sealed,
}

struct JournalEntry {
    prepare: Prepare,
    binding: Binding,
    state: JournalState,
    predecessor: Option<String>,
    successor: Option<String>,
    closed_phase: Option<ImagePhase>,
    account_phase: Option<ImagePhase>,
}

struct Facts {
    #[cfg(unix)]
    native_journal: Option<aterm_agent::harness::native_receipts::testing::Journal>,
    journal: Mutex<BTreeMap<String, JournalEntry>>,
    prepared_history: Mutex<Vec<(Prepare, Binding)>>,
    prepare_journal_allowed: AtomicBool,
    prepare_journal_ack: AtomicBool,
    prepare_journal_panic: AtomicBool,
    prepare_journal_gate: Option<PublicationGate>,
    grant_journal_allowed: AtomicBool,
    grant_journal_gate: Option<PublicationGate>,
    grant_journal_calls: AtomicU32,
    sealed_history: Mutex<Vec<(Prepare, Binding)>>,
    seal_journal_allowed: AtomicBool,
    receipt_allowed: AtomicBool,
    receipt_readable: AtomicBool,
    historical_pin_probe: Option<(std::sync::Weak<crate::SessionCtx>, Arc<AtomicBool>)>,
    receipt_history: Mutex<Vec<(Prepare, Binding)>>,
    receipt_gate: Option<PublicationGate>,
    #[cfg(unix)]
    fence_allowed: AtomicBool,
    #[cfg(unix)]
    fence_ack: AtomicBool,
    #[cfg(unix)]
    fence_gate: Option<PublicationGate>,
    processes: Arc<Mutex<BTreeMap<u32, Process>>>,
    foreground: AtomicU32,
    signals: AtomicU32,
    constructors: AtomicBool,
    physical_target: Option<Vec<u8>>,
    account_allowed: AtomicBool,
    reservation: Option<(Arc<Barrier>, Arc<Barrier>)>,
    completion: Option<(Arc<Barrier>, Arc<Barrier>)>,
    completion_seen: AtomicBool,
    outcome_gate: Option<PublicationGate>,
    prepare_gate: Option<PublicationGate>,
    reservation_gate: Option<PublicationGate>,
    conversation_valid: Arc<AtomicBool>,
    clock: Arc<AtomicU64>,
    watch_available: AtomicBool,
    watch_unknown: AtomicBool,
    watch_count: AtomicU32,
    watched_after_signals: AtomicU32,
    change_birth_on_watch: AtomicBool,
}

impl Facts {
    fn receipt_finished(&self) {
        if let Some((entered, proceed)) = &self.receipt_gate {
            entered.send(()).unwrap();
            let (lock, ready) = &**proceed;
            let (guard, waited) = ready
                .wait_timeout_while(lock.lock().unwrap(), PATIENCE, |go| !*go)
                .unwrap();
            assert!(
                *guard && !waited.timed_out(),
                "receipt worker was not released"
            );
        }
    }
    /// The host double rejects the same illegal transitions as the real issuer
    /// journal. History vectors are observations, not permission to advance.
    fn journal_event(
        &self,
        prepare: &Prepare,
        binding: &Binding,
        event: JournalEvent,
    ) -> Option<bool> {
        let mut journal = self.journal.lock().unwrap();
        let previous = journal.get(&prepare.id);
        if previous.is_some_and(|old| old.prepare != *prepare || old.binding != *binding) {
            return None;
        }
        let prior = previous.map(|old| old.state);
        let predecessor = previous.and_then(|old| old.predecessor.clone());
        let successor = previous.and_then(|old| old.successor.clone());
        let closed_phase = match event {
            JournalEvent::NoExec(phase) => Some(phase),
            _ => previous.and_then(|old| old.closed_phase),
        };
        let account_phase = previous.and_then(|old| old.account_phase);
        if account_phase == Some(ImagePhase::Rollback)
            && matches!(event, JournalEvent::NoExec(ImagePhase::Forward))
        {
            return None;
        }
        if matches!(prior, Some(JournalState::NoExec))
            && matches!(event, JournalEvent::NoExec(_))
            && previous.and_then(|old| old.closed_phase) != closed_phase
        {
            return None;
        }
        let state = match (prior, event) {
            (None, JournalEvent::Prepare) => JournalState::Prepared,
            (Some(JournalState::Prepared), JournalEvent::Prepare)
            | (Some(JournalState::MayExecute), JournalEvent::MayExecute)
            | (Some(JournalState::NoExec), JournalEvent::NoExec(_))
            | (Some(JournalState::Sealed | JournalState::NoExec), JournalEvent::Seal) => {
                return Some(false);
            }
            (Some(JournalState::Prepared), JournalEvent::MayExecute) => JournalState::MayExecute,
            (Some(JournalState::Prepared), JournalEvent::NoExec(_)) => JournalState::NoExec,
            (Some(JournalState::Prepared | JournalState::MayExecute), JournalEvent::Seal) => {
                JournalState::Sealed
            }
            _ => return None,
        };
        journal.insert(
            prepare.id.clone(),
            JournalEntry {
                prepare: prepare.clone(),
                binding: binding.clone(),
                state,
                predecessor,
                successor,
                closed_phase,
                account_phase,
            },
        );
        Some(true)
    }

    fn prepare_record_allowed(&self) -> bool {
        if let Some((entered, proceed)) = &self.prepare_journal_gate {
            entered.send(()).unwrap();
            let (lock, ready) = &**proceed;
            let (guard, waited) = ready
                .wait_timeout_while(lock.lock().unwrap(), PATIENCE, |go| !*go)
                .unwrap();
            assert!(
                *guard && !waited.timed_out(),
                "prepare journal worker was not released"
            );
        }
        self.prepare_journal_allowed.load(Ordering::SeqCst)
    }

    fn prepare_record_acknowledged(&self) -> bool {
        assert!(
            !self.prepare_journal_panic.load(Ordering::SeqCst),
            "injected panic after a committed preparation"
        );
        self.prepare_journal_ack.load(Ordering::SeqCst)
    }

    fn new() -> Self {
        let actor = process(4242, 1, 4242, 400, 2);
        // A separate child session/group is legal for a supervisor's inner PTY.
        let child = process(500, 4242, 500, 500, 3);
        Self {
            #[cfg(unix)]
            native_journal: None,
            journal: Mutex::new(BTreeMap::new()),
            prepared_history: Mutex::new(Vec::new()),
            prepare_journal_allowed: AtomicBool::new(true),
            prepare_journal_ack: AtomicBool::new(true),
            prepare_journal_panic: AtomicBool::new(false),
            prepare_journal_gate: None,
            grant_journal_allowed: AtomicBool::new(true),
            grant_journal_gate: None,
            grant_journal_calls: AtomicU32::new(0),
            sealed_history: Mutex::new(Vec::new()),
            seal_journal_allowed: AtomicBool::new(true),
            receipt_allowed: AtomicBool::new(true),
            receipt_readable: AtomicBool::new(true),
            historical_pin_probe: None,
            receipt_history: Mutex::new(Vec::new()),
            receipt_gate: None,
            #[cfg(unix)]
            fence_allowed: AtomicBool::new(true),
            #[cfg(unix)]
            fence_ack: AtomicBool::new(true),
            #[cfg(unix)]
            fence_gate: None,
            processes: Arc::new(Mutex::new(BTreeMap::from([
                (actor.pid, actor),
                (child.pid, child),
                (900, process(900, 1, 900, 400, 20)),
            ]))),
            foreground: AtomicU32::new(4242),
            signals: AtomicU32::new(0),
            constructors: AtomicBool::new(true),
            physical_target: None,
            account_allowed: AtomicBool::new(true),
            reservation: None,
            completion: None,
            completion_seen: AtomicBool::new(false),
            outcome_gate: None,
            prepare_gate: None,
            reservation_gate: None,
            conversation_valid: Arc::new(AtomicBool::new(true)),
            watch_available: AtomicBool::new(true),
            watch_unknown: AtomicBool::new(false),
            watch_count: AtomicU32::new(0),
            watched_after_signals: AtomicU32::new(0),
            change_birth_on_watch: AtomicBool::new(false),
            clock: Arc::new(AtomicU64::new(
                aterm_agent::harness::native_act::monotonic_us().unwrap_or(1),
            )),
        }
    }

    fn enter_target(&self) {
        self.processes.lock().unwrap().get_mut(&4242).unwrap().image = image(9);
    }
}

impl Kernel for Facts {
    fn fence_account_claim(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
    ) -> Option<account_fence::FenceRecord> {
        #[cfg(unix)]
        {
            if !self.fence_allowed.load(Ordering::SeqCst) {
                return None;
            }
            let record = self
                .native_journal
                .as_ref()?
                .fence_account(aterm_agent::harness::native_receipts::AccountRelation {
                    predecessor: relation.predecessor().prepare(),
                    predecessor_binding: relation.predecessor().binding(),
                    phase: relation.phase(),
                    candidate: relation.candidate(),
                })
                .ok()?;
            if let Some((entered, proceed)) = &self.fence_gate {
                entered.send(()).unwrap();
                let (lock, ready) = &**proceed;
                let (guard, waited) = ready
                    .wait_timeout_while(lock.lock().unwrap(), PATIENCE, |go| !*go)
                    .unwrap();
                assert!(
                    *guard && !waited.timed_out(),
                    "fence worker was not released"
                );
            }
            if !self.fence_ack.load(Ordering::SeqCst)
                || record.outcome != aterm_agent::harness::native_receipts::AccountOutcome::Fenced
            {
                return None;
            }
            Some(account_fence::FenceRecord {
                sequence: record.sequence,
                recorded_ms: record.recorded_ms,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = relation;
            None
        }
    }
    fn record_account_closure(
        &self,
        prepare: &Prepare,
        binding: &Binding,
        phase: ImagePhase,
    ) -> Option<account_observation::ClosureRecord> {
        #[cfg(unix)]
        {
            if !self.record_noexec(prepare, binding, phase) {
                return None;
            }
            let record = self
                .native_journal
                .as_ref()?
                .observe_phase(prepare, binding)
                .ok()??;
            Some(account_observation::ClosureRecord {
                phase: record.phase,
                sequence: record.sequence,
                recorded_ms: record.recorded_ms,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (prepare, binding, phase);
            None
        }
    }
    fn recover_account_observation(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
        binding: &Binding,
    ) -> Option<account_observation::ClaimRecord> {
        #[cfg(unix)]
        {
            if !self.receipt_readable.load(Ordering::SeqCst)
                || !self.prepare_journal_ack.load(Ordering::SeqCst)
            {
                return None;
            }
            let record = self
                .native_journal
                .as_ref()?
                .recover_account(
                    aterm_agent::harness::native_receipts::AccountRelation {
                        predecessor: relation.predecessor().prepare(),
                        predecessor_binding: relation.predecessor().binding(),
                        phase: relation.phase(),
                        candidate: relation.candidate(),
                    },
                    binding,
                )
                .ok()??;
            self.receipt_finished();
            Some(account_observation::ClaimRecord {
                state: account_continuation::recorded_state(record.outcome)?,
                sequence: record.sequence,
                recorded_ms: record.recorded_ms,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (relation, binding);
            None
        }
    }
    fn recover_account_claim(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
        binding: &Binding,
    ) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
        use aterm_agent::harness::native_account_wire::ChildState;
        if !self.prepare_journal_ack.load(Ordering::SeqCst) {
            return None;
        }
        #[cfg(unix)]
        if let Some(journal) = &self.native_journal {
            return account_continuation::recorded_state(
                journal
                    .recover_account(
                        aterm_agent::harness::native_receipts::AccountRelation {
                            predecessor: relation.predecessor().prepare(),
                            predecessor_binding: relation.predecessor().binding(),
                            phase: relation.phase(),
                            candidate: relation.candidate(),
                        },
                        binding,
                    )
                    .ok()??
                    .outcome,
            );
        }
        let journal = self.journal.lock().unwrap();
        let parent = journal.get(&relation.predecessor().prepare().id)?;
        let child = journal.get(&relation.candidate().id)?;
        if parent.prepare != *relation.predecessor().prepare()
            || parent.binding != *relation.predecessor().binding()
            || parent.successor.as_deref() != Some(&relation.candidate().id)
            || child.prepare != *relation.candidate()
            || child.binding != *binding
            || child.account_phase != Some(relation.phase())
        {
            return None;
        }
        match child.state {
            JournalState::Prepared => Some(ChildState::Prepared),
            JournalState::MayExecute => Some(ChildState::MayExecute),
            JournalState::NoExec => child.closed_phase.map(ChildState::NoExec),
            JournalState::Sealed => Some(ChildState::Sealed { may_execute: true }),
        }
    }
    fn record_account_claim(
        &self,
        relation: &aterm_agent::harness::native_account_wire::Relation,
        binding: &Binding,
    ) -> Option<aterm_agent::harness::native_account_wire::ChildState> {
        use aterm_agent::harness::native_account_wire::ChildState;
        if !self.prepare_record_allowed() {
            return None;
        }
        let predecessor = relation.predecessor().prepare();
        let predecessor_binding = relation.predecessor().binding();
        let prepare = relation.candidate();
        #[cfg(unix)]
        let from_disk = if let Some(journal) = &self.native_journal {
            let observed = journal
                .claim_account(
                    aterm_agent::harness::native_receipts::AccountRelation {
                        predecessor,
                        predecessor_binding,
                        phase: relation.phase(),
                        candidate: prepare,
                    },
                    binding,
                )
                .ok()?;
            Some(account_continuation::recorded_state(observed.outcome)?)
        } else {
            None
        };
        #[cfg(not(unix))]
        let from_disk: Option<ChildState> = None;
        let mut journal = self.journal.lock().unwrap();
        let parent = journal.get(&predecessor.id)?;
        if parent.prepare != *predecessor
            || parent.binding != *predecessor_binding
            || parent.state != JournalState::NoExec
            || parent.closed_phase != Some(relation.phase())
        {
            return None;
        }
        if let Some(old) = journal.get(&prepare.id) {
            let exact = old.prepare == *prepare
                && old.binding == *binding
                && old.account_phase == Some(relation.phase())
                && old.predecessor.as_ref() == Some(&predecessor.id)
                && parent.successor.as_ref() == Some(&prepare.id);
            let observed = from_disk.or_else(|| match old.state {
                JournalState::Prepared => Some(ChildState::Prepared),
                JournalState::MayExecute => Some(ChildState::MayExecute),
                JournalState::NoExec => old.closed_phase.map(ChildState::NoExec),
                JournalState::Sealed => Some(ChildState::Sealed { may_execute: true }),
            });
            drop(journal);
            return if exact && self.prepare_record_acknowledged() {
                observed
            } else {
                None
            };
        }
        if parent.successor.is_some() {
            return None;
        }
        journal.get_mut(&predecessor.id).unwrap().successor = Some(prepare.id.clone());
        journal.insert(
            prepare.id.clone(),
            JournalEntry {
                prepare: prepare.clone(),
                binding: binding.clone(),
                state: JournalState::Prepared,
                predecessor: Some(predecessor.id.clone()),
                successor: None,
                closed_phase: None,
                account_phase: Some(relation.phase()),
            },
        );
        drop(journal);
        self.prepared_history
            .lock()
            .unwrap()
            .push((prepare.clone(), binding.clone()));
        self.prepare_record_acknowledged()
            .then_some(from_disk.unwrap_or(ChildState::Prepared))
    }

    fn account_admission(
        &self,
        _: &aterm_agent::harness::accounts_pool::ExecutionIntent,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        self.account_allowed
            .load(Ordering::SeqCst)
            .then_some(None)
            .ok_or(())
    }
    fn record_successor(
        &self,
        predecessor: &Prepare,
        predecessor_binding: &Binding,
        prepare: &Prepare,
        binding: &Binding,
    ) -> bool {
        if !self.prepare_record_allowed() {
            return false;
        }
        #[cfg(unix)]
        if self.native_journal.as_ref().is_some_and(|journal| {
            journal
                .successor(predecessor, predecessor_binding, prepare, binding)
                .is_err()
        }) {
            return false;
        }
        let mut journal = self.journal.lock().unwrap();
        let Some(parent) = journal.get(&predecessor.id) else {
            return false;
        };
        if parent.prepare != *predecessor
            || parent.binding != *predecessor_binding
            || parent.state != JournalState::NoExec
            || predecessor.host_epoch != prepare.host_epoch
            || predecessor.id == prepare.id
        {
            return false;
        }
        if let Some(old) = journal.get(&prepare.id) {
            let exact = old.state == JournalState::Prepared
                && old.account_phase.is_none()
                && old.prepare == *prepare
                && old.binding == *binding
                && old.predecessor.as_ref() == Some(&predecessor.id)
                && parent.successor.as_ref() == Some(&prepare.id);
            drop(journal);
            return exact && self.prepare_record_acknowledged();
        }
        if parent.successor.is_some() {
            return false;
        }
        journal.get_mut(&predecessor.id).unwrap().successor = Some(prepare.id.clone());
        journal.insert(
            prepare.id.clone(),
            JournalEntry {
                prepare: prepare.clone(),
                binding: binding.clone(),
                state: JournalState::Prepared,
                predecessor: Some(predecessor.id.clone()),
                successor: None,
                closed_phase: None,
                account_phase: None,
            },
        );
        drop(journal);
        self.prepared_history
            .lock()
            .unwrap()
            .push((prepare.clone(), binding.clone()));
        self.prepare_record_acknowledged()
    }
    fn record_prepared(&self, prepare: &Prepare, binding: &Binding) -> bool {
        if !self.prepare_record_allowed() {
            return false;
        }
        #[cfg(unix)]
        if self.native_journal.as_ref().is_some_and(|journal| {
            journal
                .record(
                    prepare,
                    binding,
                    aterm_agent::harness::native_receipts::testing::Append::Prepare,
                )
                .is_err()
        }) {
            return false;
        }
        let Some(changed) = self.journal_event(prepare, binding, JournalEvent::Prepare) else {
            return false;
        };
        if changed {
            self.prepared_history
                .lock()
                .unwrap()
                .push((prepare.clone(), binding.clone()));
        }
        self.prepare_record_acknowledged()
    }
    fn record_may_execute(&self, prepare: &Prepare, binding: &Binding) -> bool {
        self.grant_journal_calls.fetch_add(1, Ordering::SeqCst);
        if let Some((entered, proceed)) = &self.grant_journal_gate {
            entered.send(()).unwrap();
            let (lock, ready) = &**proceed;
            let (guard, waited) = ready
                .wait_timeout_while(lock.lock().unwrap(), PATIENCE, |go| !*go)
                .unwrap();
            assert!(
                *guard && !waited.timed_out(),
                "grant journal worker was not released"
            );
        }
        self.grant_journal_allowed.load(Ordering::SeqCst)
            && self
                .journal_event(prepare, binding, JournalEvent::MayExecute)
                .is_some()
    }
    fn record_sealed(&self, prepare: &Prepare, binding: &Binding) -> bool {
        if !self.seal_journal_allowed.load(Ordering::SeqCst) {
            return false;
        }
        #[cfg(unix)]
        if self.native_journal.as_ref().is_some_and(|journal| {
            journal
                .record(
                    prepare,
                    binding,
                    aterm_agent::harness::native_receipts::testing::Append::Seal,
                )
                .is_err()
        }) {
            return false;
        }
        let Some(changed) = self.journal_event(prepare, binding, JournalEvent::Seal) else {
            return false;
        };
        if changed {
            self.sealed_history
                .lock()
                .unwrap()
                .push((prepare.clone(), binding.clone()));
        }
        true
    }
    fn recorded_operation(&self, id: &str) -> Result<bool, ()> {
        if !self.receipt_readable.load(Ordering::SeqCst) {
            return Err(());
        }
        #[cfg(unix)]
        if let Some(journal) = &self.native_journal {
            return journal.contains_operation(id).map_err(|_| ());
        }
        Ok(self
            .prepared_history
            .lock()
            .unwrap()
            .iter()
            .any(|(prepare, _)| prepare.id == id)
            || self
                .receipt_history
                .lock()
                .unwrap()
                .iter()
                .any(|(prepare, _)| prepare.id == id))
    }
    fn record_noexec(&self, prepare: &Prepare, binding: &Binding, phase: ImagePhase) -> bool {
        if !self.receipt_allowed.load(Ordering::SeqCst)
            || (phase == ImagePhase::Rollback
                && !matches!(
                    &prepare.plan,
                    NativePlan::Restart {
                        rollback: Some(_),
                        ..
                    }
                ))
        {
            return false;
        }
        #[cfg(unix)]
        if self.native_journal.as_ref().is_some_and(|journal| {
            journal
                .record(
                    prepare,
                    binding,
                    aterm_agent::harness::native_receipts::testing::Append::NoExecInPhase(phase),
                )
                .is_err()
        }) {
            return false;
        }
        self.receipt_finished();
        let Some(changed) = self.journal_event(prepare, binding, JournalEvent::NoExec(phase))
        else {
            return false;
        };
        if changed {
            self.receipt_history
                .lock()
                .unwrap()
                .push((prepare.clone(), binding.clone()));
        }
        true
    }
    fn historical_noexec(&self, request: &Request) -> bool {
        if let Some((ctx, probe)) = &self.historical_pin_probe {
            let id = match request {
                Request::Prepare(prepare) => &prepare.id,
                Request::Query(binding) | Request::Reconcile(binding) => &binding.id,
                _ => unreachable!(),
            };
            probe.store(
                ctx.upgrade().is_some_and(|ctx| {
                    ctx.fenced_writes
                        .native
                        .lock()
                        .unwrap()
                        .receipt_flights
                        .get(id)
                        .copied()
                        .unwrap_or(0)
                        > 0
                }),
                Ordering::SeqCst,
            );
        }
        if !self.receipt_readable.load(Ordering::SeqCst) {
            return false;
        }
        self.receipt_history
            .lock()
            .unwrap()
            .iter()
            .any(|(prepare, binding)| match request {
                Request::Prepare(actual) => actual.as_ref() == prepare,
                Request::Query(actual) | Request::Reconcile(actual) => actual == binding,
                _ => false,
            })
    }
    fn account_execution(
        &self,
        _: &RestartCommand,
        _: u32,
        _: Option<&ColdEnvironmentFacts>,
    ) -> Result<Option<aterm_agent::harness::accounts_pool::ExecutionGuard>, ()> {
        self.account_allowed
            .load(Ordering::SeqCst)
            .then_some(None)
            .ok_or(())
    }
    fn constructor_enabled(&self, _: Constructor) -> bool {
        self.constructors.load(Ordering::Acquire)
    }
    fn clock(&self) -> Clock {
        Clock::Facts(Arc::clone(&self.clock))
    }
    fn process(&self, pid: u32) -> Option<Process> {
        self.processes.lock().unwrap().get(&pid).cloned()
    }
    fn image(&self, image: &Image) -> bool {
        self.physical_target
            .as_ref()
            .is_none_or(|path| *path != image.path || image_exists(image))
    }
    fn foreground(&self, _: i32) -> Option<u32> {
        Some(self.foreground.load(Ordering::SeqCst))
    }
    fn own_target(&self, _: &str, _: &Image) -> bool {
        true
    }
    fn own_launcher(&self, image: &Image) -> bool {
        image.inode == 20
    }
    fn birth_alive(&self, original: &Process) -> Option<bool> {
        Some(
            self.process(original.pid)
                .is_some_and(|actual| actual.birth == original.birth),
        )
    }
    fn signal(&self, _: i32, _: &Process, _: &crate::SessionCtx) -> String {
        self.signals.fetch_add(1, Ordering::SeqCst);
        "OK signalled\n".into()
    }
    fn watch(&self, process: &Process) -> Option<DeathWatch> {
        self.watch_count.fetch_add(1, Ordering::SeqCst);
        self.watched_after_signals
            .store(self.signals.load(Ordering::SeqCst), Ordering::SeqCst);
        if !self.watch_available.load(Ordering::SeqCst) {
            return None;
        }
        if self.change_birth_on_watch.swap(false, Ordering::SeqCst) {
            self.processes.lock().unwrap().get_mut(&process.pid)?.birth += 1;
        }
        if self.watch_unknown.load(Ordering::SeqCst) {
            return Some(DeathWatch::Unknown);
        }
        Some(DeathWatch::Facts(
            process.clone(),
            Arc::clone(&self.processes),
        ))
    }
    fn group_gone(&self, group: u32) -> Option<bool> {
        Some(
            self.processes
                .lock()
                .unwrap()
                .values()
                .all(|process| process.group != group),
        )
    }
    fn conversation(&self, _: &crate::SessionCtx, _: &Process) -> Option<Conversation> {
        Some(Conversation::Facts(Arc::clone(&self.conversation_valid)))
    }
    fn record_key(&self, _: &crate::SessionCtx) -> Option<String> {
        self.conversation_valid
            .load(Ordering::SeqCst)
            .then(|| "captured-record-root/conversation".into())
    }
    fn record_guard(&self, _: &crate::SessionCtx) -> Option<Conversation> {
        Some(Conversation::Facts(Arc::clone(&self.conversation_valid)))
    }
    fn reserved(&self) {
        if let Some((arrived, gate)) = &self.reservation_gate {
            arrived.send(()).unwrap();
            let (lock, changed) = &**gate;
            let permit = lock.lock().unwrap();
            let (permit, _) = changed
                .wait_timeout_while(permit, PATIENCE, |permit| !*permit)
                .unwrap();
            assert!(*permit, "queued cancellation gate hang detector");
        }
        if let Some((reserved, proceed)) = &self.reservation {
            reserved.wait();
            proceed.wait();
        }
    }
    fn preparing(&self) {
        if let Some((arrived, gate)) = &self.prepare_gate {
            arrived.send(()).unwrap();
            let (lock, changed) = &**gate;
            let permit = lock.lock().unwrap();
            let (permit, _) = changed
                .wait_timeout_while(permit, PATIENCE, |permit| !*permit)
                .unwrap();
            assert!(*permit, "initial prepare publication gate hang detector");
        }
    }
    fn completion_sealed(&self) {
        if let Some((arrived, gate)) = &self.outcome_gate
            && !self.completion_seen.swap(true, Ordering::SeqCst)
        {
            arrived.send(()).unwrap();
            let (lock, changed) = &**gate;
            let permit = lock.lock().unwrap();
            let (permit, _) = changed
                .wait_timeout_while(permit, PATIENCE, |permit| !*permit)
                .unwrap();
            assert!(*permit, "outcome publication gate hang detector");
            return;
        }
        if let Some((sealed, proceed)) = &self.completion
            && !self.completion_seen.swap(true, Ordering::SeqCst)
        {
            sealed.wait();
            proceed.wait();
        }
    }
}

fn image(inode: u64) -> Image {
    Image {
        path: format!("/image-{inode}").into_bytes(),
        device: 1,
        inode,
    }
}

fn restart_command(inode: u64) -> RestartCommand {
    RestartCommand {
        settles: None,
        account_intent: None,
        image: image(inode),
        program: format!("/image-{inode}"),
        argv: vec!["--resume".into(), "captured-conversation".into()],
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: aterm_agent::harness::upgrade::Dialect::Zsh,
    }
}

fn process(pid: u32, parent: u32, group: u32, session: u32, inode: u64) -> Process {
    Process {
        pid,
        birth: u64::from(pid) + 100,
        parent,
        group,
        session,
        image: image(inode),
    }
}

fn caller() -> SupervisorInputCaller {
    SupervisorInputCaller {
        principal: Principal::Instance,
        connection: Some(71),
        peer: Some(4242),
        proof: Some(scoped(&[Scope::Upgrade]).proven),
    }
}

fn plan(ctx: &crate::SessionCtx, facts: &Facts) -> Prepare {
    Prepare {
        asked: None,
        id: "ab".repeat(16),
        host_epoch: host_epoch().unwrap().to_string(),
        holder: "grok-alab@4242".into(),
        revision: ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
        expected_human_seq: None,
        deadline_us: aterm_agent::harness::native_act::monotonic_us().unwrap() + 60_000_000,
        duty: Scope::Upgrade,
        plan: NativePlan::OwnImage {
            child: facts.process(500).unwrap(),
            target: image(9),
            version: "0.1.0+g123".into(),
        },
    }
}

fn run(
    ctx: &Arc<crate::SessionCtx>,
    facts: &Facts,
    request: Request,
    actor: SupervisorInputCaller,
) -> String {
    serve_by(ctx, -1, request, actor, facts, &mut |_| {
        panic!("no terminal input in OwnImage")
    })
}

fn admitted_binding(reply: &str) -> Binding {
    let Some(Reply::Admitted(binding)) = Reply::parse(reply) else {
        panic!("not admitted: {reply}");
    };
    binding
}

#[test]
fn native_actual_admission_rejects_wrong_duty_revision_actor_connection_and_plan() {
    let handle = crate::session_store::test_handle_on(7910, -1);
    let ctx = &handle.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let original = plan(ctx, &facts);
    for duty in [
        Scope::Relaunch,
        Scope::Approvals,
        Scope::TurnEnd,
        Scope::Questions,
        Scope::Walls,
    ] {
        let mut wrong = original.clone();
        wrong.duty = duty;
        assert_eq!(run(ctx, &facts, Request::prepare(wrong), caller()), DENIED);
    }
    let mut stale = original.clone();
    stale.revision += 1;
    assert_eq!(run(ctx, &facts, Request::prepare(stale), caller()), DENIED);
    for actor in [
        SupervisorInputCaller {
            connection: Some(72),
            ..caller()
        },
        SupervisorInputCaller {
            peer: Some(4243),
            ..caller()
        },
        SupervisorInputCaller {
            proof: None,
            ..caller()
        },
        SupervisorInputCaller {
            principal: Principal::Harness,
            ..caller()
        },
    ] {
        assert_eq!(
            run(ctx, &facts, Request::prepare(original.clone()), actor),
            DENIED
        );
    }
    let mut restart = original.clone();
    restart.plan = NativePlan::Restart {
        cold: None,
        observation: None,
        process: facts.process(4242).unwrap(),
        shell: process(400, 1, 400, 400, 4),
        launcher: image(20),
        socket: "/fake-native.sock".into(),
        forward: Box::new(restart_command(9)),
        rollback: Some(Box::new(restart_command(2))),
    };
    assert_eq!(
        run(ctx, &facts, Request::prepare(restart), caller()),
        DENIED
    );
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[test]
fn replacing_the_captured_physical_restart_target_refuses_actual_admission() {
    use std::os::unix::fs::PermissionsExt as _;
    let world = aterm_tempfile::tempdir().unwrap();
    let path = world.path().join("claude");
    // Data-only executable entries: no process is started from either file.
    std::fs::write(&path, b"target-before").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let captured = Image::at(&path).unwrap();
    let (handle, mut facts, mut request, actor) = closed_restart::fixture(79991);
    facts.physical_target = Some(captured.path.clone());
    let NativePlan::Restart { forward, .. } = &mut request.plan else {
        unreachable!();
    };
    forward.image = captured;
    forward.program = path.to_str().unwrap().into();
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request.clone()),
        actor.clone(),
    ));
    assert_eq!(
        run(&handle.ctx, &facts, Request::Abort(binding), actor.clone()),
        ROLLBACK_SAFE
    );
    std::fs::rename(&path, world.path().join("original-target")).unwrap();
    std::fs::write(&path, b"target-after").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    request.id = "ac".repeat(16);
    assert_eq!(
        run(
            &handle.ctx,
            &facts,
            Request::prepare(request.clone()),
            actor.clone()
        ),
        DENIED
    );
    assert!(
        !handle
            .ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .by_id
            .contains_key(&request.id)
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 0);
    request.id = "ad".repeat(16);
    let NativePlan::Restart { forward, .. } = &mut request.plan else {
        unreachable!();
    };
    forward.image = Image::at(&path).unwrap();
    let binding = admitted_binding(&run(
        &handle.ctx,
        &facts,
        Request::prepare(request),
        actor.clone(),
    ));
    assert_eq!(
        run(&handle.ctx, &facts, Request::Abort(binding), actor),
        ROLLBACK_SAFE
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn sealed_cold_restart_admission_requires_builtin_relaunch_without_rollback() {
    use aterm_agent::harness::native_act::{ColdBilling, ColdIntent};
    for case in 0..4 {
        let (handle, facts, mut request, mut actor) = closed_restart::fixture(7980 + case);
        let NativePlan::Restart { cold, rollback, .. } = &mut request.plan else {
            unreachable!();
        };
        *cold = Some(Box::new(ColdIntent {
            login: None,
            home: None,
            auth_observed: false,
            route: aterm_agent::harness::accounts::AuthRoute::Stored,
            conversation: Some("captured-conversation".into()),
            profile: None,
            mode: aterm_agent::harness::accounts::Mode::Wall,
            billing: ColdBilling::Keep,
        }));
        *rollback = None;
        assert_eq!(request.plan.constructor(), Constructor::ColdRestart);
        match case {
            1 => actor.principal = Principal::Instance,
            2 => {
                let NativePlan::Restart { rollback, .. } = &mut request.plan else {
                    unreachable!();
                };
                *rollback = Some(Box::new(restart_command(2)));
            }
            3 => request.duty = Scope::Upgrade,
            _ => {}
        }
        let reply = run(
            &handle.ctx,
            &facts,
            Request::prepare(request.clone()),
            actor,
        );
        if case == 0 {
            assert!(
                matches!(Reply::parse(reply.trim_end()), Some(Reply::Admitted(_))),
                "{reply}"
            );
            let table = handle.ctx.fenced_writes.native.lock().unwrap();
            let operation = table.by_id.get(&request.id).unwrap().lock().unwrap();
            assert_eq!(operation.request, request);
        } else {
            assert_eq!(reply, DENIED, "case {case}");
            assert!(
                handle
                    .ctx
                    .fenced_writes
                    .native
                    .lock()
                    .unwrap()
                    .by_id
                    .is_empty()
            );
        }
        assert_eq!(
            facts.signals.load(Ordering::SeqCst),
            0,
            "prepare is not an effect"
        );
    }
}

#[test]
fn native_queue_input_drain_transition_and_lost_ack_conform_to_model() {
    let model = aterm_models_harness::scoped_native_handoff_model();
    let mut state = model.init_state();
    let h = crate::session_store::test_handle_on(7911, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let old = FencedWrite::enter_fence(
        &ctx,
        &ticket(&ctx, true, Scope::Approvals),
        crate::metrics::now_us(),
    )
    .unwrap();
    let reserved = Arc::new(Barrier::new(2));
    let proceed = Arc::new(Barrier::new(2));
    let facts = Arc::new(Facts {
        reservation: Some((Arc::clone(&reserved), Arc::clone(&proceed))),
        ..Facts::new()
    });
    let request = plan(&ctx, &facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let worker_request = request.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(worker_request),
            caller(),
        )
    });
    reserved.wait();
    assert!(model.fire("Reserve", &mut state));
    assert!(model.fire("Overtake", &mut state));
    let operation = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .by_id
        .get(&request.id)
        .unwrap()
        .clone();
    assert_eq!(operation.lock().unwrap().phase, Phase::Queued);
    assert!(
        FencedWrite::enter_fence(
            &ctx,
            &ticket(&ctx, true, Scope::Approvals),
            crate::metrics::now_us()
        )
        .is_err()
    );
    drop(old);
    assert!(model.fire("OldInputFinish", &mut state));
    proceed.wait();
    let binding = admitted_binding(&worker.join().unwrap());
    assert_eq!(operation.lock().unwrap().phase, Phase::Started);
    assert!(model.fire("NativeStart", &mut state));
    let transition = session_timeline::publish_claim_supervisor_scoped(
        &ctx,
        "grok-alab@4242",
        Some(71),
        scoped(&[Scope::Approvals]),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    assert!(model.fire("Publish", &mut state));
    let published_revision = transition.published_ref().revision;
    let (done, ack) = mpsc::channel();
    let at_ack = Arc::new(Barrier::new(2));
    let worker_barrier = Arc::clone(&at_ack);
    let acknowledger = std::thread::spawn(move || {
        worker_barrier.wait();
        done.send(transition.acknowledge().revision).unwrap();
    });
    at_ack.wait();
    assert!(model.fire("EarlyAck", &mut state));
    assert!(ack.try_recv().is_err());
    assert!(model.fire("NewRevisionInput", &mut state));
    assert!(
        FencedWrite::enter_fence(
            &ctx,
            &ticket(&ctx, false, Scope::Approvals),
            crate::metrics::now_us()
        )
        .is_err()
    );
    connection_closed_by(&ctx, 71, facts.as_ref());
    assert!(model.fire("LoseAck", &mut state));
    assert!(operation.lock().unwrap().custody.is_some());
    let reconnected = SupervisorInputCaller {
        connection: Some(72),
        ..caller()
    };
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Recover(binding.clone()),
            reconnected.clone()
        ),
        admitted(&binding)
    );
    assert!(model.fire("Recover", &mut state));
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::prepare(Prepare {
                asked: None,
                host_epoch: "00".repeat(16),
                ..request.clone()
            }),
            caller()
        ),
        "ERR native host epoch changed\n"
    );
    assert!(model.fire("NewHost", &mut state));
    facts.enter_target();
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            reconnected.clone()
        ),
        "OK native-act state=precommitted\n"
    );
    // The commit's own receipt reads the drain under `try_lock` (an observation never parks a
    // request lane), and releasing the custody wakes the acknowledger onto that same lock: the
    // reply reads committed, or unknown while the woken acknowledger holds it. The operation
    // committed either way, as the query below reads once the acknowledger has returned.
    let commit = run(
        &ctx,
        &facts,
        Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0+g123".into()),
        reconnected.clone(),
    );
    assert!(commit == COMMITTED || commit == UNKNOWN, "{commit}");
    assert!(model.fire("KernelOutcome", &mut state));
    assert_eq!(ack.recv_timeout(PATIENCE).unwrap(), published_revision);
    acknowledger.join().unwrap();
    // The replaced claim is untouched by the old operation's completion/reproof.
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us())
            .revision(),
        published_revision
    );
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Query(binding.clone()),
            reconnected.clone()
        ),
        COMMITTED
    );
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::Commit(binding, ImagePhase::Forward, "0.1.0+g123".into()),
            reconnected
        ),
        COMMITTED
    );
    assert!(model.fire("DuplicateCompletion", &mut state));
    assert_eq!(
        ctx.fenced_writes.native.lock().unwrap().completed.len() as i64,
        state["records"]
    );
    for law in &model.invariants {
        assert!(model.check_invariant(law.name, &state), "{}", law.name);
    }
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
}

#[test]
fn native_history_preserves_independent_operator_and_legacy_rights() {
    let h = crate::session_store::test_handle_on(7912, -1);
    let ctx = &h.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    assert!(
        !untyped_required(ctx, Principal::Harness, Some(10), None, None),
        "legacy v3 is unchanged before actual native opt-in"
    );
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(plan(ctx, &facts)),
        caller(),
    ));
    connection_closed_by(ctx, 71, &facts);
    assert!(untyped_required(
        ctx,
        Principal::Instance,
        Some(72),
        Some(4242),
        Some(4342)
    ));
    assert!(untyped_required(
        ctx,
        Principal::Instance,
        Some(72),
        Some(4242),
        None
    ));
    assert!(
        !untyped_required(ctx, Principal::Instance, Some(72), Some(4242), Some(9999)),
        "reused pid is an independent credential"
    );
    assert!(
        !untyped_required(ctx, Principal::Instance, Some(99), Some(999), Some(123)),
        "independent operator keeps its authority"
    );
    assert_eq!(
        run(ctx, &facts, Request::Abort(binding), caller()),
        ROLLBACK_SAFE
    );
    assert!(
        untyped_required(ctx, Principal::Instance, Some(72), Some(4242), Some(4342)),
        "abort/release cannot erase scoped actor history"
    );
}

#[test]
fn scoped_native_negative_controls_falsify_each_law() {
    let base = aterm_models_harness::scoped_native_handoff_model();
    for (bug, law) in [
        (1, "ExactActorScopeRevision"),
        (2, "NativeCannotOvertakeInput"),
        (3, "TransitionAckWaitsActualCustody"),
        (4, "NewRevisionCannotStarveHandoff"),
        (5, "LostAckRetainsCustody"),
        (6, "RecoveryNeverMutatesTwice"),
        (7, "NewHostCannotRecreateOperation"),
        (8, "CompletionIsRecordedOnce"),
        (9, "TerminalOutcomeIsImmutable"),
        (10, "ConstructorCapabilityPrecedesAdmission"),
    ] {
        let model = aterm_spec::interp::with_buggy(&base, bug);
        let mut state = model.init_state();
        let actions: &[&str] = if bug == 1 {
            &["WrongActor"]
        } else if bug == 10 {
            &["DisabledConstructor"]
        } else {
            &[
                "Reserve",
                "Overtake",
                "OldInputFinish",
                "NativeStart",
                "Publish",
                "EarlyAck",
                "NewRevisionInput",
                "LoseAck",
                "Recover",
                "NewHost",
                "KernelOutcome",
                "DuplicateCompletion",
            ]
        };
        let mut caught = false;
        for action in actions {
            assert!(model.fire(action, &mut state));
            caught |= !base.check_invariant(law, &state);
        }
        assert!(caught, "negative control {bug}: {law}");
    }
}

#[cfg(unix)]
#[test]
fn private_constructor_executes_only_its_signal_and_retained_continuation() {
    use crate::control_input_ack::{InputMark, testing};
    use crate::input::{Delivery, EgressReceipt};
    let (h, mut reader) = testing::owned_handle(7913);
    let ctx = &h.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let (arrived, at_seal) = mpsc::sync_channel(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let facts = Arc::new(Facts {
        outcome_gate: Some((arrived, Arc::clone(&gate))),
        ..Facts::new()
    });
    let old = process(4242, 400, 4242, 400, 2);
    let shell = process(400, 1, 400, 400, 4);
    facts.processes.lock().unwrap().insert(old.pid, old.clone());
    facts
        .processes
        .lock()
        .unwrap()
        .insert(shell.pid, shell.clone());
    let mut request = plan(ctx, &facts);
    request.holder = crate::harness_host::holder();
    request.expected_human_seq = Some(ctx.human_input.seq());
    request.duty = Scope::Relaunch;
    request.plan = NativePlan::Restart {
        cold: None,
        observation: None,
        process: old,
        shell,
        launcher: image(20),
        socket: "/fake-native.sock".into(),
        forward: Box::new(restart_command(9)),
        rollback: Some(Box::new(restart_command(2))),
    };
    let builtin = SupervisorInputCaller {
        principal: Principal::Harness,
        connection: Some(8),
        peer: Some(900),
        proof: None,
    };
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(request),
        builtin.clone(),
    ));
    for _ in 0..2 {
        assert_eq!(
            run(
                ctx,
                &facts,
                Request::Signal(binding.clone()),
                builtin.clone()
            ),
            "OK native-act state=signalled\n"
        );
    }
    assert_eq!(
        facts.signals.load(Ordering::SeqCst),
        1,
        "idempotent signal never reaches raw-kill fallback"
    );
    assert_eq!(
        run(ctx, &facts, Request::Signal(binding.clone()), caller()),
        DENIED
    );
    facts.processes.lock().unwrap().remove(&4242);
    facts.foreground.store(400, Ordering::SeqCst);
    let result = serve_by(
        ctx,
        -1,
        Request::Resume(
            binding.clone(),
            ImagePhase::Forward,
            "1.1".into(),
            ctx.human_input.seq(),
        ),
        builtin.clone(),
        &*facts,
        &mut |body| {
            let expected = restart_command(9)
                .launcher_line(
                    &image(20),
                    "/fake-native.sock",
                    ctx.self_id.as_str(),
                    &binding,
                    ImagePhase::Forward,
                )
                .unwrap();
            assert_eq!(body, format!("if-gen=1.1 idle=1 timeout=1000 {expected}"));
            let mark = InputMark::at_epoch(
                ctx,
                h.local_id,
                aterm_slog::Actor::Ctl,
                crate::session_log::InputVerb::Turn,
                None,
                true,
            );
            let producer = mark.native_producer();
            let completion = mark.completion();
            assert!(completion.start_supervised());
            let receipt = ctx.sink.write_frame_with_prefix(b"R").unwrap();
            assert!(
                ctx.sink
                    .wait_prefix_to_kernel(receipt.kernel_prefix().unwrap())
            );
            completion.finish(EgressReceipt::from_reported_delivery(
                Delivery::Full,
                receipt.order(),
            ));
            drop(producer);
            "OK typed=1\n".into()
        },
    );
    assert_eq!(result, "OK native-act state=resumed phase=forward\n");
    assert_eq!(testing::byte(&mut reader), b'R');
    facts
        .processes
        .lock()
        .unwrap()
        .insert(600, process(600, 400, 600, 400, 20));
    facts.foreground.store(600, Ordering::SeqCst);
    let launcher = SupervisorInputCaller {
        principal: Principal::Instance,
        connection: Some(80),
        peer: Some(600),
        proof: None,
    };
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Attach(binding.clone(), ImagePhase::Forward),
            launcher.clone()
        )),
        Some(Reply::Launch { .. })
    ));
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Execute(binding.clone(), ImagePhase::Forward),
            launcher
        )),
        Some(Reply::Execute { .. })
    ));
    facts.processes.lock().unwrap().get_mut(&600).unwrap().image = image(9);
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            builtin.clone()
        ),
        "OK native-act state=precommitted\n"
    );
    let worker_ctx = Arc::clone(ctx);
    let worker_facts = Arc::clone(&facts);
    let worker_binding = binding.clone();
    let worker_builtin = builtin.clone();
    let committer = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::Commit(worker_binding, ImagePhase::Forward, "1.0.70".into()),
            worker_builtin,
        )
    });
    let reached_seal = at_seal.recv_timeout(PATIENCE);
    // The exact new mapping was captured with its original watch before terminal seal.
    // Death before deferred publication must preserve it, without requiring a vendor record.
    let before_publish = if reached_seal.is_ok() {
        facts.processes.lock().unwrap().remove(&600);
        facts.foreground.store(400, Ordering::SeqCst);
        Some(run(ctx, &facts, Request::Query(binding), builtin.clone()))
    } else {
        None
    };
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let completed = committer.join().unwrap();
    reached_seal.expect("positive native completion reaches its publication gate");
    assert_eq!(
        before_publish.as_deref(),
        Some(UNKNOWN),
        "the marker still owns outcome publication"
    );
    let Some(Reply::RestartCommitted(observed)) = Reply::parse(&completed) else {
        panic!("private passive handoff: {completed}");
    };
    assert_eq!(observed.registration.vendor, process(600, 400, 600, 400, 9));
    assert_eq!(observed.registration.shell, facts.process(400).unwrap());
    let retained = {
        let meta = ctx.meta.lock().unwrap();
        let context = observe_control::context(ctx, &meta, &builtin, &*facts).unwrap();
        observe_control::store(ctx).lock().unwrap().prove_with(
            &observed.binding,
            &context,
            -1,
            facts.clock.load(Ordering::SeqCst),
            &ObservationKernel(&*facts),
        )
    };
    assert!(
        retained.is_some(),
        "post-seal death retains the captured new image"
    );
    drop(retained);
    facts
        .processes
        .lock()
        .unwrap()
        .insert(601, process(601, 400, 601, 400, 9));
    facts.foreground.store(601, Ordering::SeqCst);
    let unrelated = {
        let meta = ctx.meta.lock().unwrap();
        let context = observe_control::context(ctx, &meta, &builtin, &*facts).unwrap();
        observe_control::store(ctx).lock().unwrap().prove_with(
            &observed.binding,
            &context,
            -1,
            facts.clock.load(Ordering::SeqCst),
            &ObservationKernel(&*facts),
        )
    };
    assert!(
        unrelated.is_none(),
        "an active new vendor never receives the old crash witness"
    );
    assert!(
        FencedWrite::enter_fence(
            ctx,
            &ticket(ctx, true, Scope::Approvals),
            crate::metrics::now_us()
        )
        .is_ok()
    );
}

#[test]
fn native_input_seals_producer_but_pending_work_keeps_exact_custody() {
    for attempted in [false, true] {
        let h = crate::session_store::test_handle_on(7914 + u64::from(attempted), -1);
        let ctx = &h.ctx;
        install(ctx, 71, &[Scope::Upgrade]);
        let facts = Facts::new();
        let binding = admitted_binding(&run(
            ctx,
            &facts,
            Request::prepare(plan(ctx, &facts)),
            caller(),
        ));
        let operation = lookup(ctx, &binding).unwrap();
        let custody = operation.lock().unwrap().custody.as_ref().unwrap().clone();
        let snapshot = ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_authority(crate::metrics::now_us());
        let input = Arc::new(NativeInput::new(
            ctx,
            custody,
            ctx.human_input.seq(),
            snapshot,
            aterm_agent::harness::native_act::monotonic_us().unwrap() + MAX_PREPARE_US,
            Arc::new(AtomicBool::new(false)),
            Some(Conversation::Facts(Arc::new(AtomicBool::new(true)))),
            Clock::Facts(Arc::clone(&facts.clock)),
        ));
        let pending = input.reserve().unwrap();
        if attempted {
            assert!(input.start(false));
            pending.attempted();
        }
        input.seal();
        let completion = operation
            .lock()
            .unwrap()
            .seal_completion(Decision::Committed);
        assert_eq!(finish(ctx, &operation, completion), UNKNOWN);
        let duplicate = operation
            .lock()
            .unwrap()
            .seal_completion(Decision::ProvenNotCommittedRollbackSafe);
        assert!(
            duplicate.is_none(),
            "terminal outcome cannot be replaced by a late callback"
        );
        assert_eq!(finish(ctx, &operation, duplicate), UNKNOWN);
        for _ in 0..2 {
            assert_eq!(
                run(ctx, &facts, Request::Query(binding.clone()), caller()),
                UNKNOWN,
                "an observation returns Unknown without parking behind actual work"
            );
        }
        assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
        assert!(
            !input.unwritten(),
            "pending GUI/worker work is not proof of zero bytes"
        );
        assert!(
            FencedWrite::enter_fence(
                ctx,
                &ticket(ctx, true, Scope::Approvals),
                crate::metrics::now_us()
            )
            .is_err()
        );
        drop(pending);
        assert_eq!(
            run(ctx, &facts, Request::Query(binding.clone()), caller()),
            COMMITTED
        );
        assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
        assert_eq!(
            input.unwritten(),
            !attempted,
            "an attempted write lost without receipt remains unknown"
        );
        assert!(
            FencedWrite::enter_fence(
                ctx,
                &ticket(ctx, true, Scope::Approvals),
                crate::metrics::now_us()
            )
            .is_ok(),
            "read-only observation Arc retains no lease after actual work drains"
        );
    }
}

#[test]
fn a_queued_native_constructor_loses_old_revision_before_any_mutation() {
    let h = crate::session_store::test_handle_on(7916, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let old = FencedWrite::enter_fence(
        &ctx,
        &ticket(&ctx, true, Scope::Approvals),
        crate::metrics::now_us(),
    )
    .unwrap();
    let reserved = Arc::new(Barrier::new(2));
    let proceed = Arc::new(Barrier::new(2));
    let facts = Arc::new(Facts {
        reservation: Some((Arc::clone(&reserved), Arc::clone(&proceed))),
        ..Facts::new()
    });
    let request = plan(&ctx, &facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(request),
            caller(),
        )
    });
    reserved.wait();
    // Publication returns while the old actual writer and queued native marker still exist.
    let transition = session_timeline::publish_claim_supervisor_scoped(
        &ctx,
        "grok-alab@4242",
        Some(71),
        scoped(&[Scope::Approvals]),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    drop(old);
    proceed.wait();
    assert_eq!(worker.join().unwrap(), ROLLBACK_SAFE);
    transition.acknowledge();
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert!(
        FencedWrite::enter_fence(
            &ctx,
            &ticket(&ctx, false, Scope::Approvals),
            crate::metrics::now_us()
        )
        .is_ok()
    );
}

#[test]
fn committed_receipt_reports_surviving_claim_version_and_exact_revision() {
    let h = crate::session_store::test_handle_on(7917, -1);
    let ctx = &h.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let request = plan(ctx, &facts);
    let before = request.revision;
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(request), caller()));
    facts.enter_target();
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            caller()
        ),
        "OK native-act state=precommitted\n"
    );
    let receipt = run(
        ctx,
        &facts,
        Request::Commit(binding.clone(), ImagePhase::Forward, "0.1.0+g123".into()),
        caller(),
    );
    assert_eq!(
        Reply::parse(&receipt),
        Some(Reply::Committed(ClaimOutcome::Bound(before + 1)))
    );
    assert_eq!(run(ctx, &facts, Request::Query(binding), caller()), receipt);
    let meta = ctx.meta.lock().unwrap();
    assert_eq!(
        meta.supervisor_authority(crate::metrics::now_us())
            .revision(),
        before + 1
    );
    assert_eq!(
        meta.live_scoped(crate::metrics::now_us())
            .unwrap()
            .claim
            .version
            .as_ref()
            .unwrap()
            .text,
        "0.1.0+g123"
    );
}

#[test]
fn completion_is_sealed_before_unlock_and_duplicate_abort_replays_once() {
    let h = crate::session_store::test_handle_on(7918, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let sealed = Arc::new(Barrier::new(2));
    let proceed = Arc::new(Barrier::new(2));
    let facts = Arc::new(Facts {
        completion: Some((Arc::clone(&sealed), Arc::clone(&proceed))),
        ..Facts::new()
    });
    let binding = admitted_binding(&run(
        &ctx,
        &facts,
        Request::prepare(plan(&ctx, &facts)),
        caller(),
    ));
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let worker_binding = binding.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::Abort(worker_binding),
            caller(),
        )
    });
    sealed.wait();
    let operation = lookup(&ctx, &binding).unwrap();
    assert_eq!(
        operation.lock().unwrap().phase,
        Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
    );
    let (tx, rx) = mpsc::channel();
    let observer_ctx = Arc::clone(&ctx);
    let observer_facts = Arc::clone(&facts);
    let observer_binding = binding.clone();
    let observer = std::thread::spawn(move || {
        tx.send(run(
            &observer_ctx,
            &observer_facts,
            Request::Abort(observer_binding),
            caller(),
        ))
        .unwrap();
    });
    // A minute detects a parked observation, not a response-time budget. Always release the
    // paused owner before checking the result so a failed control leaves no test actor parked.
    let duplicate = rx.recv_timeout(PATIENCE);
    proceed.wait();
    assert_eq!(worker.join().unwrap(), ROLLBACK_SAFE);
    observer.join().unwrap();
    assert_eq!(
        duplicate.unwrap(),
        UNKNOWN,
        "pending exact custody yields no terminal ACK yet"
    );
    assert_eq!(
        run(&ctx, &facts, Request::Abort(binding.clone()), caller()),
        ROLLBACK_SAFE
    );
    assert_eq!(
        run(&ctx, &facts, Request::Query(binding), caller()),
        ROLLBACK_SAFE
    );
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
}

#[test]
fn outer_reparent_preserves_own_image_but_actor_and_owned_child_identity_do_not_change() {
    let h = crate::session_store::test_handle_on(7919, -1);
    let ctx = &h.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(plan(ctx, &facts)),
        caller(),
    ));
    facts
        .processes
        .lock()
        .unwrap()
        .get_mut(&4242)
        .unwrap()
        .parent = 999;
    assert_eq!(
        run(ctx, &facts, Request::Abort(binding), caller()),
        ROLLBACK_SAFE,
        "diagnostic outer reparent does not turn safe pre-mutation cancellation into Unknown"
    );

    let h = crate::session_store::test_handle_on(7920, -1);
    let ctx = &h.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let binding = admitted_binding(&run(
        ctx,
        &facts,
        Request::prepare(plan(ctx, &facts)),
        caller(),
    ));
    facts.enter_target();
    facts
        .processes
        .lock()
        .unwrap()
        .get_mut(&4242)
        .unwrap()
        .parent = 999;
    let actor = facts.process(4242).unwrap();
    let child = facts.process(500).unwrap();
    let mut wrong = Vec::new();
    let mut changed = actor.clone();
    changed.birth += 1;
    wrong.push(changed);
    let mut changed = actor.clone();
    changed.group += 1;
    wrong.push(changed);
    let mut changed = actor.clone();
    changed.session += 1;
    wrong.push(changed);
    let mut changed = actor.clone();
    changed.image = image(90);
    wrong.push(changed);
    let mut changed = child.clone();
    changed.birth += 1;
    wrong.push(changed);
    let mut changed = child.clone();
    changed.group += 1;
    wrong.push(changed);
    let mut changed = child.clone();
    changed.session += 1;
    wrong.push(changed);
    let mut changed = child.clone();
    changed.parent += 1;
    wrong.push(changed);
    let mut changed = child.clone();
    changed.image = image(91);
    wrong.push(changed);
    for changed in wrong {
        let pid = changed.pid;
        facts.processes.lock().unwrap().insert(pid, changed);
        assert_ne!(
            run(
                ctx,
                &facts,
                Request::Precommit(binding.clone(), ImagePhase::Forward),
                caller()
            ),
            "OK native-act state=precommitted\n",
            "kernel actor/child change must not reprove"
        );
        facts.processes.lock().unwrap().insert(
            pid,
            if pid == actor.pid {
                actor.clone()
            } else {
                child.clone()
            },
        );
    }
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Precommit(binding.clone(), ImagePhase::Forward),
            caller()
        ),
        "OK native-act state=precommitted\n"
    );
    assert!(matches!(
        Reply::parse(&run(
            ctx,
            &facts,
            Request::Commit(binding, ImagePhase::Forward, "0.1.0+g123".into()),
            caller()
        )),
        Some(Reply::Committed(ClaimOutcome::Bound(_)))
    ));
}

#[test]
fn queued_native_eof_cancels_once_before_any_admission_and_replays_its_safe_outcome() {
    let h = crate::session_store::test_handle_on(7921, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, 71, &[Scope::Upgrade]);
    let old = FencedWrite::enter_fence(
        &ctx,
        &ticket(&ctx, true, Scope::Approvals),
        crate::metrics::now_us(),
    )
    .unwrap();
    let reserved = Arc::new(Barrier::new(2));
    let proceed = Arc::new(Barrier::new(2));
    let facts = Arc::new(Facts {
        reservation: Some((Arc::clone(&reserved), Arc::clone(&proceed))),
        ..Facts::new()
    });
    let request = plan(&ctx, &facts);
    let worker_ctx = Arc::clone(&ctx);
    let worker_facts = Arc::clone(&facts);
    let worker_request = request.clone();
    let worker = std::thread::spawn(move || {
        run(
            &worker_ctx,
            &worker_facts,
            Request::prepare(worker_request),
            caller(),
        )
    });
    reserved.wait();
    // The explicit EOF fact is independent of when metadata cleanup publishes its release.
    connection_closed_by(&ctx, 71, facts.as_ref());
    drop(old);
    proceed.wait();
    assert_eq!(worker.join().unwrap(), ROLLBACK_SAFE);
    assert_eq!(
        run(
            &ctx,
            &facts,
            Request::prepare(request.clone()),
            SupervisorInputCaller {
                connection: Some(72),
                ..caller()
            }
        ),
        ROLLBACK_SAFE
    );
    assert_eq!(
        run(&ctx, &facts, Request::prepare(request), caller()),
        ROLLBACK_SAFE
    );
    assert_eq!(ctx.fenced_writes.native.lock().unwrap().completed.len(), 1);
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
}

#[test]
fn queued_own_image_reproves_actor_identity_while_allowing_diagnostic_reparent() {
    for change in 0..3 {
        let h = crate::session_store::test_handle_on(7922 + change, -1);
        let ctx = Arc::clone(&h.ctx);
        install(&ctx, 71, &[Scope::Upgrade]);
        let old = FencedWrite::enter_fence(
            &ctx,
            &ticket(&ctx, true, Scope::Approvals),
            crate::metrics::now_us(),
        )
        .unwrap();
        let reserved = Arc::new(Barrier::new(2));
        let proceed = Arc::new(Barrier::new(2));
        let facts = Arc::new(Facts {
            reservation: Some((Arc::clone(&reserved), Arc::clone(&proceed))),
            ..Facts::new()
        });
        let request = plan(&ctx, &facts);
        let id = request.id.clone();
        let worker_ctx = Arc::clone(&ctx);
        let worker_facts = Arc::clone(&facts);
        let worker = std::thread::spawn(move || {
            run(
                &worker_ctx,
                &worker_facts,
                Request::prepare(request),
                caller(),
            )
        });
        reserved.wait();
        {
            let mut processes = facts.processes.lock().unwrap();
            let actor = processes.get_mut(&4242).unwrap();
            match change {
                0 => actor.parent = 999,
                1 => actor.birth += 1,
                _ => actor.image = image(88),
            }
        }
        drop(old);
        proceed.wait();
        let reply = worker.join().unwrap();
        if change == 0 {
            let binding = admitted_binding(&reply);
            assert_eq!(
                run(&ctx, &facts, Request::Abort(binding), caller()),
                ROLLBACK_SAFE
            );
        } else {
            assert_eq!(
                reply, ROLLBACK_SAFE,
                "changed actor cannot start a queued native move"
            );
            let op = ctx
                .fenced_writes
                .native
                .lock()
                .unwrap()
                .by_id
                .get(&id)
                .unwrap()
                .clone();
            assert_eq!(
                op.lock().unwrap().phase,
                Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
            );
        }
        assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn codex_renewal_ready_retirement_requires_a_closed_native_capability() {
    let handle = crate::session_store::test_handle_on(8002, -1);
    let ctx = &handle.ctx;
    install(ctx, 71, &[Scope::Upgrade]);
    let facts = Facts::new();
    let request = plan(ctx, &facts);
    let id = request.id.clone();
    assert!(renewal::ready_native_no_go(ctx, &id));
    ctx.fenced_writes
        .native
        .lock()
        .unwrap()
        .reservations
        .insert(id.clone(), None);
    assert!(!renewal::ready_native_no_go(ctx, &id));
    ctx.fenced_writes
        .native
        .lock()
        .unwrap()
        .reservations
        .remove(&id);
    let binding = admitted_binding(&run(ctx, &facts, Request::prepare(request), caller()));
    assert!(
        !renewal::ready_native_no_go(ctx, &id),
        "a live native capability is not absent"
    );
    assert_eq!(
        run(ctx, &facts, Request::Abort(binding.clone()), caller()),
        ROLLBACK_SAFE
    );
    assert!(
        renewal::ready_native_no_go(ctx, &id),
        "actual terminal NoGo closes old authority"
    );
    assert_eq!(
        run(
            ctx,
            &facts,
            Request::Commit(binding, ImagePhase::Forward, "1.2.3".into()),
            caller()
        ),
        ROLLBACK_SAFE
    );
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
}
