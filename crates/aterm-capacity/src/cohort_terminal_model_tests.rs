// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Terminal traces use actual child birth/body/group lifetime and covered physical receipts.
//! Cover evidence is recorded only when the real after-cover interruption was consumed.
//! The final decision is sampled while the actual query still holds its original Slot.

use super::*;
use crate::{
    Held, Ledger, Need,
    cohort::{Cohort, Lifetime},
    ledger::Laid,
};
use aterm_spec::{
    derive::Model,
    interp::{self, State},
    wait::{MUST_HAPPEN, until_within},
};
use aterm_uds::spawnfd::{self, Child, Stdio};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::{cell::RefCell, collections::BTreeSet, path::PathBuf, rc::Rc};

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryPoint {
    Enter,
    Closed,
}
type DecisionObserver = Box<dyn FnMut(QueryPoint, &Namespace, &CohortClosureRef, &Slot)>;
thread_local! {static DECISION: RefCell<Option<DecisionObserver>> = const {RefCell::new(None)};}

/// Called only at the actual successful decision, before the original Slot/gate drops.
pub(super) fn query_decision(namespace: &Namespace, reference: &CohortClosureRef, slot: &Slot) {
    query_event(QueryPoint::Closed, namespace, reference, slot);
}
pub(super) fn query_entry(namespace: &Namespace, reference: &CohortClosureRef, slot: &Slot) {
    query_event(QueryPoint::Enter, namespace, reference, slot);
}
fn query_event(
    point: QueryPoint,
    namespace: &Namespace,
    reference: &CohortClosureRef,
    slot: &Slot,
) {
    DECISION.with(|observer| {
        if let Some(observer) = observer.borrow_mut().as_mut() {
            observer(point, namespace, reference, slot);
        }
    });
}

struct ObserveDecision;
impl ObserveDecision {
    fn set(observer: DecisionObserver) -> Self {
        DECISION.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(observer);
        });
        Self
    }
}
impl Drop for ObserveDecision {
    fn drop(&mut self) {
        DECISION.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

struct World {
    scratch: TestWorld,
    namespace: Namespace,
    ledger: Ledger,
}
impl World {
    fn new() -> Self {
        let scratch = TestWorld::new("terminal-model");
        let home = scratch.path().join("home");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
        let ledger = Ledger::at(namespace.fence());
        Self {
            scratch,
            namespace,
            ledger,
        }
    }

    fn admitted(&self) -> Controller {
        // Exercise the real raw admission/journal API. These figures allocate no memory.
        let (mut record, mut laid) = self.ledger.lay(&Need::new("terminal model"), 0).unwrap();
        self.ledger
            .admit(
                &mut laid,
                &mut record,
                Held {
                    admitted_ms: 1,
                    cores: 1,
                    alone: false,
                },
            )
            .unwrap();
        Controller(Some(laid))
    }

    fn slot(&self, reference: &CohortClosureRef) -> PathBuf {
        self.namespace.retired().join(reference.key())
    }
}

/// Preserve the physical failure before the ordinary controller Drop can archive it.
/// Successful traces still use its ordinary destructor. This owns only private test data.
struct Controller(Option<Laid>);
impl std::ops::Deref for Controller {
    type Target = Laid;
    fn deref(&self) -> &Laid {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for Controller {
    fn deref_mut(&mut self) -> &mut Laid {
        self.0.as_mut().unwrap()
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        let gate = if std::thread::panicking() {
            self.0
                .as_ref()
                .and_then(|laid| laid.custody.as_ref())
                .and_then(|custody| open(&custody.directory, MUTATION, false, false).ok())
                .and_then(|file| RecordLock::try_lock(file, 1).ok())
        } else {
            None
        };
        // If another transaction holds the gate, the real destructor already refuses it.
        drop(self.0.take());
        drop(gate);
    }
}

struct Running {
    child: Child,
    input: Option<std::process::ChildStdin>,
}
impl Running {
    fn start(laid: &mut Laid) -> (Self, Cohort) {
        let attempt = laid.arm_cohort().unwrap();
        let cat = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/cat"));
        let mut spawned = spawnfd::command(cat)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn_cohort(attempt)
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
        let cohort = laid.bind_cohort(&mut spawned).unwrap();
        assert_eq!(cohort.lifetime(), Lifetime::Present);
        (
            Self {
                child: spawned.into_child(),
                input: Some(input),
            },
            cohort,
        )
    }
    fn finish(&mut self, cohort: &Cohort) {
        self.input.take();
        let status = until_within(MUST_HAPPEN, "actual cat exit", || {
            self.child.try_wait().unwrap()
        });
        assert!(status.success());
        until_within(MUST_HAPPEN, "actual self-led cat group drain", || {
            (cohort.lifetime() == Lifetime::Drained).then_some(())
        });
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.input.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct Trace {
    model: Model,
    state: State,
    covered: BTreeSet<&'static str>,
}
impl Trace {
    fn new() -> Self {
        let model = aterm_models_capacity::capacity_terminal_model();
        let state = model.init_state();
        Self {
            model,
            state,
            covered: BTreeSet::new(),
        }
    }
    fn step(&mut self, action: &str) {
        let mut next = self.model.successors(action, &self.state);
        assert_eq!(
            next.len(),
            1,
            "actual terminal transition {action}: {:?}",
            self.state
        );
        self.state = next.pop().unwrap();
    }
    fn consumed(&mut self, stage: &'static str) {
        assert_eq!(
            INTERRUPT.with(std::cell::Cell::take),
            None,
            "the actual production boundary must have run"
        );
        self.covered.insert(stage);
        self.step(match stage {
            "terminal-covered" => "CoverTerminal",
            "archive-covered" => "CoverArchive",
            "archive-moved" => "Move",
            "compaction-covered" => "CoverInventory",
            _ => panic!("unmapped terminal boundary"),
        });
    }
    fn mutant(&self, bug: i64, action: &str, invariant: &str) {
        let model = interp::with_buggy(&self.model, bug);
        let bad = model
            .successors(action, &self.state)
            .pop()
            .expect("counterfactual must be enabled");
        assert!(
            !model.check_invariant(invariant, &bad),
            "mutant {bug}: {invariant}"
        );
    }
}

#[derive(Clone, Copy)]
enum Gate<'a> {
    Controller(&'a Transaction),
    Archive(&'a Slot),
}
struct Context<'a> {
    namespace: &'a Namespace,
    reference: &'a CohortClosureRef,
    cohort: &'a Cohort,
    gate: Gate<'a>,
}

fn blocked(file: File) -> bool {
    match RecordLock::try_lock(file, 1) {
        Err(std::fs::TryLockError::WouldBlock) => true,
        Ok(lock) => {
            drop(lock);
            false
        }
        Err(error) => panic!("fresh lock probe: {error}"),
    }
}

fn read_optional(parent: &File, name: &str) -> Option<(File, String, Stamp)> {
    match held_immutable(parent, name) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("reading actual terminal metadata {name}: {error}"),
    }
}

fn inventory_original(root: &File, value: &Value) -> bool {
    if value["claim"] != identity(root, true).unwrap() {
        return false;
    }
    let inventory = &value["inventory"];
    let directories = inventory["directories"].as_array().unwrap();
    let files = inventory["files"].as_array().unwrap();
    let check_file = |parent: &File, directory: &str, name: &str| {
        let expected = files.iter().find(|entry| {
            entry["directory"].as_str() == Some(directory) && entry["name"].as_str() == Some(name)
        });
        let Some(expected) = expected else {
            return false;
        };
        let held = open(parent, name, false, false).unwrap();
        Stamp::read(&held).unwrap() == Stamp::decode(&expected["stamp"]).unwrap()
    };
    for item in fd::Dir::read_from(root).unwrap() {
        let item = item.unwrap();
        let name = item.file_name().to_str().unwrap();
        if matches!(name, "." | "..") {
            continue;
        }
        if let Some(expected) = directories
            .iter()
            .find(|entry| entry["name"].as_str() == Some(name))
        {
            let directory = open(root, name, false, true).unwrap();
            if expected["identity"] != identity(&directory, true).unwrap() {
                return false;
            }
            for item in fd::Dir::read_from(&directory).unwrap() {
                let item = item.unwrap();
                let child = item.file_name().to_str().unwrap();
                if !matches!(child, "." | "..") && !check_file(&directory, name, child) {
                    return false;
                }
            }
        } else if !check_file(root, "", name) {
            return false;
        }
    }
    true
}

/// Historical move/cover facts come from consumed production boundaries, not file presence.
/// Current paths, full bytes/origins, group lifetime and lock exclusion are sampled directly.
fn project(trace: &Trace, context: &Context<'_>) -> State {
    let mut actual = trace.state.clone();
    let active = context.namespace.open_active().unwrap();
    let retired = context.namespace.open_retired().unwrap();
    let key = context.reference.key();
    let active_claim = match open(&active, &key, false, true) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("active claim: {error}"),
    };
    let slot_dir = match open(&retired, &key, false, true) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => panic!("archive slot: {error}"),
    };
    let moved = active_claim.is_none()
        && slot_dir
            .as_ref()
            .is_some_and(|slot| read_optional(slot, RECEIPT).is_some());
    let history = slot_dir
        .as_ref()
        .and_then(|slot| match open(slot, HISTORY, false, true) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => panic!("history: {error}"),
        });
    let terminal_covered = trace.covered.contains("terminal-covered");
    let archive_covered = trace.covered.contains("archive-covered");
    let inventory_covered = trace.covered.contains("compaction-covered");
    let compacted = moved && inventory_covered && history.is_none();
    actual.insert(
        "drained",
        i64::from(context.cohort.lifetime() == Lifetime::Drained),
    );
    actual.insert("terminal_covered", i64::from(terminal_covered));
    actual.insert("archive_covered", i64::from(archive_covered));
    actual.insert("inventory_covered", i64::from(inventory_covered));
    actual.insert("moved", i64::from(moved));
    actual.insert("compacted", i64::from(compacted));
    actual.insert(
        "phase",
        if compacted {
            5
        } else if inventory_covered {
            4
        } else if moved {
            3
        } else if archive_covered {
            2
        } else if terminal_covered {
            1
        } else {
            0
        },
    );
    let serialized = match context.gate {
        Gate::Controller(transaction) => {
            blocked(open(&transaction.anchor.directory, MUTATION, false, false).unwrap())
        }
        Gate::Archive(slot) => {
            slot.check().unwrap();
            blocked(open(slot.file(), ".slot.lock", false, false).unwrap())
        }
    };
    actual.insert("mutation_held", i64::from(serialized));
    let receipt = slot_dir
        .as_ref()
        .and_then(|slot| read_optional(slot, RECEIPT));
    // Probe the actual original guard while its record still exists. After known
    // compaction, this historical fact stays bound to the covered terminal receipt;
    // missing payload alone never turns into a fresh free-guard observation.
    if let Some(root) = active_claim.as_ref().or(history.as_ref()) {
        let name = format!("{}{}", context.reference.marker(), CLAIM);
        let record = open(root, &name, false, false).unwrap();
        actual.insert("guards_free", i64::from(!blocked(record)));
    }
    if let (Some(history), Some(slot)) = (&history, &slot_dir)
        && let Some((_, text, _)) = read_optional(slot, COMPACTED)
    {
        let value: Value = aterm_json::from_str(&text).unwrap();
        actual.insert(
            "inventory_original",
            i64::from(inventory_original(history, &value)),
        );
    }
    let closed = active_claim
        .as_ref()
        .or(history.as_ref())
        .and_then(|root| read_optional(root, CLOSED));
    actual.insert(
        "receipts_present",
        i64::from(if moved {
            receipt.is_some()
        } else {
            terminal_covered && closed.is_some()
        }),
    );
    if let Some((_, text, _)) = receipt {
        let value: Value = aterm_json::from_str(&text).unwrap();
        actual.insert(
            "reference_matches",
            i64::from(value["reference"] == context.reference.to_json()),
        );
        actual.insert(
            "slot_original",
            i64::from(value["slot"] == identity(slot_dir.as_ref().unwrap(), true).unwrap()),
        );
    }
    if trace.state["reported_closed"] == 1 {
        actual.insert("query_reference_matches", actual["reference_matches"]);
        actual.insert("query_slot_original", actual["slot_original"]);
        actual.insert("query_mutation_held", i64::from(serialized));
    }
    actual
}

fn observe(
    trace: &Trace,
    world: &World,
    reference: &CohortClosureRef,
    cohort: &Cohort,
    transaction: &Transaction,
) {
    assert_eq!(
        project(
            trace,
            &Context {
                namespace: &world.namespace,
                reference,
                cohort,
                gate: Gate::Controller(transaction)
            }
        ),
        trace.state
    );
}

fn retire_owned(laid: &Laid) -> Transaction {
    let custody = laid.custody.as_ref().unwrap();
    assert_eq!(custody.journal.lifetime(), Lifetime::Drained);
    let mut transaction = laid.transaction().unwrap();
    let name = laid.path().file_name().unwrap().to_str().unwrap();
    let text = transaction
        .observe(&custody.directory, &custody.writer, name)
        .unwrap();
    let capture = transaction
        .capture(&custody.directory, &custody.writer, name, &text, true)
        .unwrap();
    transaction
        .commit(&custody.directory, &custody.writer, &capture)
        .unwrap();
    transaction
}

fn publish_to(
    trace: &mut Trace,
    transaction: &Transaction,
    world: &World,
    laid: &Laid,
    stage: &'static str,
) {
    interrupt(stage);
    assert!(transaction.archive(&world.namespace, &laid.lock).is_err());
    trace.consumed(stage);
}

fn moved_owner() -> (
    World,
    Controller,
    Cohort,
    Transaction,
    Trace,
    CohortClosureRef,
) {
    let world = World::new();
    let mut laid = world.admitted();
    let reference = laid.cohort_closure_ref().unwrap();
    let (mut running, cohort) = Running::start(&mut laid);
    assert_eq!(
        laid.finish_cohort().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    running.finish(&cohort);
    laid.finish_cohort().unwrap();
    let mut transaction = retire_owned(&laid);
    let mut trace = Trace::new();
    trace.step("ActualDrain");
    trace.step("HoldMutation");
    trace.step("RetainedOwnerTerminal");
    observe(&trace, &world, &reference, &cohort, &transaction);
    // The actual named FILE failure precedes the real after-cover witness.
    super::super::refuse_named_file_flush(CLOSED);
    interrupt("terminal-covered");
    assert!(transaction.archive(&world.namespace, &laid.lock).is_err());
    assert_eq!(
        INTERRUPT.with(std::cell::Cell::take),
        Some("terminal-covered")
    );
    observe(&trace, &world, &reference, &cohort, &transaction);
    publish_to(&mut trace, &transaction, &world, &laid, "terminal-covered");
    observe(&trace, &world, &reference, &cohort, &transaction);
    trace.mutant(1, "MoveUncovered", "TransferFollowsDurableCover");
    publish_to(&mut trace, &transaction, &world, &laid, "archive-covered");
    observe(&trace, &world, &reference, &cohort, &transaction);
    drop(transaction);
    trace.step("ReleaseMutation");
    let directory = &laid.custody.as_ref().unwrap().directory;
    assert!(
        !blocked(open(directory, MUTATION, false, false).unwrap()),
        "the actual original transaction was released"
    );
    trace.mutant(9, "MoveWithoutMutation", "TransferAndClosureAreSerialized");
    transaction = laid.transaction().unwrap();
    trace.step("HoldMutation");
    publish_to(&mut trace, &transaction, &world, &laid, "archive-moved");
    observe(&trace, &world, &reference, &cohort, &transaction);
    (world, laid, cohort, transaction, trace, reference)
}

fn cover_inventory_after_move(world: &World, reference: &CohortClosureRef, trace: &mut Trace) {
    interrupt("compaction-covered");
    assert!(world.ledger.reap_cohort_claim(reference).is_err());
    trace.consumed("compaction-covered");
    assert!(world.slot(reference).join(HISTORY).is_dir());
    assert!(world.slot(reference).join(COMPACTED).is_file());
}

fn finish_and_query(
    world: &World,
    reference: &CohortClosureRef,
    cohort: &Cohort,
    trace: Trace,
) -> Trace {
    let trace = Rc::new(RefCell::new(trace));
    let seen = Rc::new(RefCell::new(false));
    let target = trace.clone();
    let called = seen.clone();
    let expected = reference.clone();
    let group = cohort.clone();
    let observer = ObserveDecision::set(Box::new(move |point, namespace, offered, slot| {
        assert_eq!(offered, &expected);
        let mut trace = target.borrow_mut();
        if point == QueryPoint::Enter {
            if trace.state["mutation_held"] == 0 {
                trace.step("HoldMutation");
            }
            return;
        }
        if trace.state["phase"] == 4 {
            trace.step("Compact");
        }
        assert!(blocked(
            open(slot.file(), ".slot.lock", false, false).unwrap()
        ));
        trace.step("Query");
        let actual = project(
            &trace,
            &Context {
                namespace,
                reference: offered,
                cohort: &group,
                gate: Gate::Archive(slot),
            },
        );
        assert_eq!(actual, trace.state);
        *called.borrow_mut() = true;
    }));
    assert_eq!(
        world.ledger.reap_cohort_claim(reference).unwrap(),
        CohortClaimState::Closed
    );
    assert!(
        *seen.borrow(),
        "actual Closed decision must sample its retained Slot gate"
    );
    drop(observer);
    match Rc::try_unwrap(trace) {
        Ok(trace) => trace.into_inner(),
        Err(_) => panic!("the actual query observer must release its trace"),
    }
}

#[test]
fn actual_owner_terminal_transfer_and_cold_compaction_follow_the_model() {
    let (world, laid, cohort, transaction, mut trace, reference) = moved_owner();
    // The owner can transfer while its ORIGINAL guard is still held; cold conversion cannot.
    assert_eq!(trace.state["guards_free"], 0);
    assert_eq!(trace.state["move_owner"], 1);
    cover_inventory_after_move(&world, &reference, &mut trace);
    observe(&trace, &world, &reference, &cohort, &transaction);
    let trace = finish_and_query(&world, &reference, &cohort, trace);
    trace.mutant(7, "DeleteFinalEvidence", "FinalEvidenceWaitsForHostPort");
    for name in [RECEIPT, COMPACTED, ".slot.lock"] {
        assert!(world.slot(&reference).join(name).is_file());
    }
    assert!(!world.slot(&reference).join(HISTORY).exists());
    assert!(
        world.ledger.list().unwrap().is_empty(),
        "retired bodies are outside hot admission"
    );
    // Explicit external evidence-loss control, not an implemented deletion/Host-ACK port.
    std::fs::rename(
        world.slot(&reference).join(RECEIPT),
        world.scratch.path().join("preserved-terminal-receipt"),
    )
    .unwrap();
    assert_eq!(
        world.ledger.cohort_claim_state(&reference).unwrap(),
        CohortClaimState::Unknown
    );
    drop(transaction);
    drop(laid);
}

#[test]
fn cold_terminal_conversion_needs_real_group_drain_and_a_fresh_free_guard() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "ledger::cohort_ledger::checkpoint::archive::terminal_model_tests::cold_terminal_conversion_needs_real_group_drain_and_a_fresh_free_guard",
    ) {
        return;
    }
    let world = World::new();
    let mut laid = world.admitted();
    let reference = laid.cohort_closure_ref().unwrap();
    let path = laid.path().to_owned();
    let (mut running, cohort) = Running::start(&mut laid);
    drop(laid);
    assert_eq!(
        world.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Retained
    );
    let mut trace = Trace::new();
    let free = RecordLock::try_lock(File::open(&path).unwrap(), RECORD_MAX).unwrap();
    trace.step("GuardsClose");
    drop(free);
    let active = world.namespace.open_active().unwrap();
    let anchor = Anchor::open_name(&active, &reference.key(), world.namespace.nonce()).unwrap();
    let directory = open(&active, &reference.key(), false, true).unwrap();
    let live_transaction = anchor.lock(&directory).unwrap();
    trace.step("HoldMutation");
    assert!(blocked(open(&directory, MUTATION, false, false).unwrap()));
    assert_eq!(cohort.lifetime(), Lifetime::Present);
    observe(&trace, &world, &reference, &cohort, &live_transaction);
    trace.mutant(
        2,
        "MoveLiveGroup",
        "TransferHasActualDrainAndTerminalCustody",
    );
    drop(live_transaction);
    trace.step("ReleaseMutation");
    running.finish(&cohort);
    trace.step("ActualDrain");
    // A real separate lock holder can keep the cold path from establishing free custody.
    let guard = RecordLock::try_lock(File::open(&path).unwrap(), RECORD_MAX).unwrap();
    let mut held_trace = Trace::new();
    held_trace.step("ActualDrain");
    let held_transaction = anchor.lock(&directory).unwrap();
    held_trace.step("HoldMutation");
    assert!(blocked(open(&directory, MUTATION, false, false).unwrap()));
    observe(&held_trace, &world, &reference, &cohort, &held_transaction);
    held_trace.mutant(
        8,
        "MoveHeldGuards",
        "TransferHasActualDrainAndTerminalCustody",
    );
    drop(held_transaction);
    assert_eq!(
        world.ledger.reap_cohort_claim(&reference).unwrap(),
        CohortClaimState::Retained
    );
    assert!(path.is_file());
    drop(guard);
    let directory = open(
        &world.namespace.open_active().unwrap(),
        &reference.key(),
        false,
        true,
    )
    .unwrap();
    let anchor = Anchor::open_name(
        &world.namespace.open_active().unwrap(),
        &reference.key(),
        world.namespace.nonce(),
    )
    .unwrap();
    let mut transaction = anchor.lock_recover(&directory).unwrap();
    trace.step("HoldMutation");
    for stage in ["terminal-covered", "archive-covered", "archive-moved"] {
        interrupt(stage);
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            crate::ledger::cohort_ledger::probe_under(
                &world.namespace,
                &directory,
                name,
                name.ends_with(CLAIM),
                true,
                &mut transaction
            )
            .is_err()
        );
        trace.consumed(stage);
        observe(&trace, &world, &reference, &cohort, &transaction);
    }
    assert_eq!(trace.state["move_guard_free"], 1);
    assert_eq!(trace.state["move_owner"], 0);
    cover_inventory_after_move(&world, &reference, &mut trace);
    finish_and_query(&world, &reference, &cohort, trace);
}

#[test]
fn compaction_refuses_unknown_and_restored_original_inventory() {
    for restore in [false, true] {
        let (world, _laid, cohort, transaction, mut trace, reference) = moved_owner();
        cover_inventory_after_move(&world, &reference, &mut trace);
        let history = world.slot(&reference).join(HISTORY);
        let victim = if restore {
            history.join(HEADER)
        } else {
            history.join("another-actors-data")
        };
        let original = if restore {
            let held = File::open(&victim).unwrap();
            let before = Stamp::read(&held).unwrap();
            super::super::past_the_clock_tick(&world.namespace.open_backend().unwrap(), &held);
            std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
            let bytes = std::fs::read(&victim).unwrap();
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(&victim)
                .unwrap();
            writer.write_all_at(b"!", 0).unwrap();
            writer.write_all_at(&bytes, 0).unwrap();
            std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o400)).unwrap();
            assert_ne!(Stamp::read(&held).unwrap(), before);
            bytes
        } else {
            let bytes = b"unknown work stays exactly here".to_vec();
            std::fs::write(&victim, &bytes).unwrap();
            bytes
        };
        trace.step("RestoreInventory");
        assert!(world.ledger.reap_cohort_claim(&reference).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), original);
        assert!(history.is_dir());
        observe(&trace, &world, &reference, &cohort, &transaction);
        trace.mutant(3, "CompactUnknown", "CompactionPreservesUnknownData");
        assert_eq!(cohort.lifetime(), Lifetime::Drained);
        assert!(world.slot(&reference).join(RECEIPT).is_file());
        drop(transaction);
    }
}

#[test]
fn exact_reference_named_slot_and_fresh_query_gate_cannot_be_substituted() {
    for fault in ["reference", "gate", "slot"] {
        let (world, _laid, cohort, transaction, mut trace, reference) = moved_owner();
        cover_inventory_after_move(&world, &reference, &mut trace);
        let mut trace = finish_and_query(&world, &reference, &cohort, trace);
        let parent = world.namespace.open_retired().unwrap();
        // The successful decision dropped its actual Slot; a fresh OFD now really acquires it.
        drop(Slot::open(&parent, &reference.key()).unwrap());
        trace.step("ReleaseMutation");
        trace.step("NextReference");
        if fault == "gate" {
            let held = Slot::open(&parent, &reference.key()).unwrap();
            assert_eq!(
                world
                    .ledger
                    .reap_cohort_claim(&reference)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
            trace.mutant(6, "QueryWithoutMutation", "TransferAndClosureAreSerialized");
            drop(held);
            finish_and_query(&world, &reference, &cohort, trace);
        } else {
            let offered = if fault == "reference" {
                trace.step("OfferDifferentReference");
                CohortClosureRef::captured(
                    reference.namespace(),
                    reference.marker(),
                    reference.epoch(),
                    crate::ClaimGuardIdentity {
                        device: reference.guard().device,
                        inode: reference.guard().inode + 1,
                    },
                )
                .unwrap()
            } else {
                let detached = replace_after_cover(&world.namespace, &reference, "file").unwrap();
                assert!(world.ledger.cohort_claim_state(&reference).is_err());
                assert!(detached.join(RECEIPT).is_file());
                trace.step("DetachSlot");
                reference.clone()
            };
            let trace = Rc::new(RefCell::new(trace));
            let seen = Rc::new(RefCell::new(false));
            let state = trace.clone();
            let called = seen.clone();
            let group = cohort.clone();
            let _observer =
                ObserveDecision::set(Box::new(move |point, namespace, reference, slot| {
                    assert!(
                        point == QueryPoint::Enter,
                        "a substituted query cannot reach Closed"
                    );
                    let mut trace = state.borrow_mut();
                    trace.step("HoldMutation");
                    assert_eq!(
                        project(
                            &trace,
                            &Context {
                                namespace,
                                reference,
                                cohort: &group,
                                gate: Gate::Archive(slot)
                            }
                        ),
                        trace.state
                    );
                    if fault == "reference" {
                        trace.mutant(4, "QueryDifferentReference", "ClosureNamesExactReference");
                    } else {
                        trace.mutant(5, "QueryDetachedSlot", "ClosureRetainsNamedSlot");
                    }
                    *called.borrow_mut() = true;
                }));
            let result = world.ledger.cohort_claim_state(&offered);
            if fault == "reference" {
                assert_eq!(result.unwrap(), CohortClaimState::Unknown);
            } else {
                assert!(result.is_err());
            }
            assert!(
                *seen.borrow(),
                "refusal must judge the actual freshly held Slot"
            );
        }
        drop(transaction);
    }
}
