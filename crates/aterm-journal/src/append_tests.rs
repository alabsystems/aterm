// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The append's conformance with `JournalAppend`: a real [`Log::append_then`], its flushes
//! projected onto the model's variables step by step, for each tier and each place of the
//! dependent write, and three code mutants the projection rejects.

use super::*;
use aterm_spec::interp::State;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

fn limits() -> Limits {
    Limits {
        records: 8,
        bytes: 8192,
        record_bytes: 1024,
    }
}

/// A write's level in the model, by the flushes that finished it: `2` on stable media,
/// `1` behind a barrier, `0` neither.
fn level(flushes: &[Flush]) -> i64 {
    match flushes {
        [] => 0,
        _ if flushes.iter().all(|flush| flush.is_durable()) => 2,
        _ => 1,
    }
}

/// Each step an append took, by its model action, with the model state it left.
type Taken = Vec<(&'static str, State)>;

/// The dependent write of an `append_then` into `witness`: a private file there.
fn witness_write(
    witness: &Path,
    bytes: &'static [u8],
) -> impl FnOnce(Offset, Flush) -> io::Result<()> {
    move |_, flush| private_file::write_private_flushed(witness, "witness", bytes, flush)
}

/// Two `append_then`s on a fresh journal: the first one's dependent write is the second's
/// `prior`, at the level its flushes left it; and each step the second took, with the model
/// state it leaves, projected from what was flushed: astream's counts for the record and the
/// commit, aterm-durable's recording for the checkpoint and the dependent write. A full flush
/// on the log's device makes every write issued there before it durable; a write on another
/// device (`apart`) it leaves as it was. `None` where this host serves an un-injected log
/// Strict, so an Ordered configuration cannot be run.
fn run(strict: bool, apart: bool, mutant: seam::Mutant) -> Option<Taken> {
    let _strict = strict.then(seam::serve_strict);
    let _apart = apart.then(seam::apart);
    let _mutant = seam::mutate(mutant);
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = Log::open(dir.path(), "test", b"pinned", false, limits()).unwrap();
    log.finish_open().unwrap();
    if !strict && mutant != seam::Mutant::UnorderedRecord && !log.served_ordered() {
        return None;
    }
    let witness = dir.path().join("witness");
    private_file::create_private_dir(&witness).unwrap();
    let recording = aterm_durable::seam::Recording::start();
    let steps = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&steps);
    tests::on_append(move |step, counts| {
        seen.borrow_mut().push((step, counts, recording.take()));
    });
    log.append_then(
        "app/0",
        vec![0; 32],
        &witness,
        witness_write(&witness, b"prior"),
    )
    .unwrap();
    let prior = steps
        .borrow()
        .iter()
        .find(|(step, ..)| *step == tests::Step::Witnessed)
        .map(|(.., flushes)| level(flushes))
        .unwrap();
    steps.borrow_mut().clear();
    let from = log.log.flush_counts();
    let appended = log.append_then(
        "app/1",
        vec![1; 32],
        &witness,
        witness_write(&witness, b"seen"),
    );
    tests::clear_append_hook();
    appended.unwrap();
    let model = aterm_models_swarm::journal_append_model();
    let mut state = model.init_state();
    state.insert("strict", i64::from(strict));
    state.insert("apart", i64::from(apart));
    state.insert("prior", prior);
    let mut taken = Vec::new();
    let mut before = from;
    let drained = |state: &mut State, var: &'static str| {
        if state[var] == 1 {
            state.insert(var, 2);
        }
    };
    for (step, counts, flushes) in steps.borrow().iter() {
        let full = counts.full > before.full;
        let barrier = counts.barrier > before.barrier;
        before = *counts;
        let action = match step {
            tests::Step::Published => {
                state.insert("published", 1);
                state.insert("record", if full { 2 } else { i64::from(barrier) });
                if full && !apart {
                    drained(&mut state, "prior");
                }
                "Publish"
            }
            tests::Step::Checkpointed => {
                state.insert("checkpointed", 1);
                state.insert("checkpoint", level(flushes));
                "Checkpoint"
            }
            tests::Step::Committed => {
                state.insert("committed", 1);
                if full {
                    drained(&mut state, "checkpoint");
                    if !apart {
                        drained(&mut state, "witness");
                        drained(&mut state, "prior");
                    }
                    state.insert("record", 2);
                }
                "Commit"
            }
            tests::Step::Witnessed => {
                state.insert("witnessed", 1);
                state.insert("witness", level(flushes));
                if level(flushes) == 2 && !apart {
                    drained(&mut state, "checkpoint");
                    drained(&mut state, "prior");
                }
                "Witness"
            }
        };
        taken.push((action, state.clone()));
    }
    state.insert("acked", 1);
    taken.push(("Ack", state));
    Some(taken)
}

thread_local! {
    /// Each configuration's run, by `(strict, apart)`, made once on this thread.
    static RUNS: RefCell<BTreeMap<(bool, bool), Option<Taken>>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// The state the code reaches from `state` by `action`: the environment's actions as the
/// model has them, and each of the append's from the run of `state`'s configuration.
pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "Latch" => {
            next.insert("strict", 1);
        }
        "Apart" => {
            next.insert("apart", 1);
            next.insert("prior", 2);
        }
        "Crash" => {
            next.insert("crashed", 1);
            next.insert("kept_prior", 1);
            next.insert("kept_record", state["published"]);
            next.insert("kept_checkpoint", state["checkpointed"]);
            next.insert("kept_witness", state["witnessed"]);
        }
        "DropRecord" => {
            next.insert("kept_record", 0);
        }
        "DropCheckpoint" => {
            next.insert("kept_checkpoint", 0);
        }
        "DropWitness" => {
            next.insert("kept_witness", 0);
        }
        "DropPrior" => {
            next.insert("kept_prior", 0);
        }
        _ => {
            let config = (state["strict"] == 1, state["apart"] == 1);
            return RUNS.with(|runs| {
                runs.borrow_mut()
                    .entry(config)
                    .or_insert_with(|| run(config.0, config.1, seam::Mutant::None))
                    .as_ref()
                    .and_then(|taken| taken.iter().find(|(name, _)| *name == action))
                    .map(|(_, state)| state.clone())
                    .unwrap_or_else(|| panic!("{config:?} takes no {action}"))
            });
        }
    }
    next
}

/// The first step of `taken` that is no model successor of the state before it.
fn first_unmodelled(taken: &[(&'static str, State)]) -> Option<(&'static str, State)> {
    let model = aterm_models_swarm::journal_append_model();
    let mut state = taken.first().map(|_| model.init_state())?;
    // The environment's actions first: the configuration the run was taken in.
    for (var, action) in [("strict", "Latch"), ("apart", "Apart")] {
        if taken[0].1[var] == 1 {
            state = transition(&state, action);
        }
    }
    for (action, next) in taken {
        if !model.successors(action, &state).contains(next) {
            return Some((action, next.clone()));
        }
        state = next.clone();
    }
    None
}

/// Every append the code takes refines the model, on either tier and with the dependent
/// write on the log's device or apart: each enabled action, from each reachable state.
/// Off Apple, where astream serves every log Strict, the Ordered states are left out.
/// NEGATIVE CONTROLS: a log opened Relaxed leaves its record unflushed ahead of its
/// checkpoint, which no `Publish` successor allows; a barrier-only checkpoint on a Strict log
/// is no `Checkpoint` successor there; and on an Ordered log a dependent write taken before
/// the commit (`WitnessFirst`, the order a reader could see a witness of a record not yet
/// durable in) is no `Witness` successor and breaks `WitnessShowsDurable`. And the law holds
/// something: an acknowledged Strict append a power loss then took the checkpoint of breaks
/// `AcknowledgedSurvives`.
#[test]
fn journal_append_matches_model_and_rejects_unordered_or_unflushed_checkpoints() {
    let model = aterm_models_swarm::journal_append_model();
    let ordered_here = RUNS.with(|runs| {
        runs.borrow_mut()
            .entry((false, false))
            .or_insert_with(|| run(false, false, seam::Mutant::None))
            .is_some()
    });
    let mut checked = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        if state["strict"] == 0 && !ordered_here {
            continue;
        }
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{action:?}: {state:?} -> {actual:?}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "no transition was checked");
    let unordered = run(false, false, seam::Mutant::UnorderedRecord).unwrap();
    let (action, state) = first_unmodelled(&unordered).expect("an unflushed record is caught");
    assert_eq!((action, state["record"]), ("Publish", 0));
    for apart in [false, true] {
        let barrier_only = run(true, apart, seam::Mutant::BarrierCheckpoint).unwrap();
        let (action, state) =
            first_unmodelled(&barrier_only).expect("a barrier-only Strict checkpoint is caught");
        assert_eq!((action, state["checkpoint"]), ("Checkpoint", 1), "{apart}");
    }
    if ordered_here {
        let first = run(false, false, seam::Mutant::WitnessFirst).unwrap();
        let (action, state) =
            first_unmodelled(&first).expect("a witness ahead of its commit is caught");
        assert_eq!((action, state["committed"]), ("Witness", 0));
        assert!(!model.check_invariant("WitnessShowsDurable", &state));
    }
    let mut lost = model.init_state();
    lost.insert("strict", 1);
    for action in ["Publish", "Checkpoint", "Commit", "Witness", "Ack", "Crash"] {
        lost = transition(&lost, action);
    }
    assert!(model.check_invariant("AcknowledgedSurvives", &lost));
    lost.insert("kept_checkpoint", 0);
    assert!(!model.check_invariant("AcknowledgedSurvives", &lost));
}

/// A journal written while astream served its log Strict reopens at the tier this host serves
/// (Ordered on an Apple boot volume), its records and checkpoint intact, and appends there.
#[test]
fn a_journal_written_strict_reopens_ordered() {
    let dir = aterm_tempfile::tempdir().unwrap();
    {
        let _strict = seam::serve_strict();
        let mut log = Log::open(dir.path(), "test", b"pinned", false, limits()).unwrap();
        log.finish_open().unwrap();
        assert!(!log.served_ordered());
        log.append("app/1", vec![1; 16]).unwrap();
        log.append("app/2", vec![2; 16]).unwrap();
    }
    let mut log = Log::open(dir.path(), "test", b"pinned", true, limits()).unwrap();
    assert!(
        log.served_ordered() || cfg!(not(target_vendor = "apple")),
        "{:?}",
        log.served()
    );
    log.append("app/3", vec![3; 16]).unwrap();
    drop(log);
    let log = Log::open(dir.path(), "test", b"pinned", true, limits()).unwrap();
    let records = log.read_range(Offset::ZERO, 8).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].subject, "app/3");
}

/// An append is acknowledged only at its commit's full flush (`Acks::FullFlush`), so a run
/// that stopped without a clean close (every run: the journal never closes its log) lost
/// nothing it acknowledged, and a reboot moves no generation of its astream log, nor has
/// astream announce lost acks. A reboot is stood in by the boot each open is told it is in.
/// NEGATIVE CONTROL: the same run with its log acking at the publish (`AcksAtPublish`) moves
/// the generation once where astream serves it Ordered.
#[test]
fn a_journal_keeps_its_log_identity_across_a_reboot() {
    const BOOT_A: [u8; 16] = [0x0A; 16];
    const BOOT_B: [u8; 16] = [0x0B; 16];
    for mutant in [seam::Mutant::None, seam::Mutant::AcksAtPublish] {
        let _mutant = seam::mutate(mutant);
        let dir = aterm_tempfile::tempdir().unwrap();
        let (before, ordered) = {
            let _boot = seam::boot(BOOT_A);
            let mut log = Log::open(dir.path(), "test", b"pinned", false, limits()).unwrap();
            log.finish_open().unwrap();
            log.append("app/1", vec![1; 16]).unwrap();
            log.append("app/2", vec![2; 16]).unwrap();
            (log.log.identity(), log.served_ordered())
        };
        let _boot = seam::boot(BOOT_B);
        let log = Log::open(dir.path(), "test", b"pinned", true, limits()).unwrap();
        assert_eq!(log.read_range(Offset::ZERO, 8).unwrap().len(), 2);
        let after = log.log.identity();
        let moved = mutant == seam::Mutant::AcksAtPublish && ordered;
        assert_eq!(
            (after.id, after.generation),
            (before.id, before.generation + u64::from(moved)),
            "{mutant:?}, served Ordered: {ordered}"
        );
    }
}
