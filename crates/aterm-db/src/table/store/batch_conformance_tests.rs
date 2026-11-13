// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two real checkpoint groups projected onto JournalGroupCustody. File/entry phases
//! compose the atomic writer's file-before-rename, directory-after-rename contract;
//! its completed writes supply actual flushes and independently reread endpoints.
//! The journal's callback supplies its commit boundary, checked against the existing
//! before-commit seam. Fault controls omit the permit file flush or publish the
//! witness before that boundary. No process cut here claims to simulate power loss.

use super::*;
use aterm_durable::{Flush, seam::Recording};
use aterm_spec::interp::State;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

type Taken = Vec<(&'static str, State)>;
pub(super) type PermissionWriter = fn(&Path, &str, &[u8]) -> std::io::Result<()>;

struct Trace {
    state: State,
    taken: Taken,
    recording: Recording,
    dir: PathBuf,
    anchor: Vec<u8>,
    checkpoint_seen: bool,
    before_commit: bool,
}

impl Trace {
    fn step(&mut self, action: &'static str, updates: &[(&'static str, i64)]) {
        for (name, value) in updates {
            self.state.insert(name, *value);
        }
        self.taken.push((action, self.state.clone()));
    }

    fn checkpoint(&mut self) {
        let flushes = self.recording.take();
        assert_eq!(flushes.len(), 2, "one checkpoint file and directory");
        let aterm_journal::Committed::Rebuilt { records, .. } = aterm_journal::read_committed(
            &self.dir,
            "table",
            &self.anchor,
            Capacity::default().journal(),
            None,
        )
        .unwrap() else {
            panic!("a full read rebuilds the checkpoint");
        };
        let count = records.len() as i64;
        assert_eq!(count, self.state["permit_end"]);
        let durable = if flushes.iter().all(|flush| flush.is_durable()) {
            count
        } else {
            self.state["durable_checkpoint"]
        };
        self.step(
            "Checkpoint",
            &[
                ("checkpoint", count),
                ("durable_checkpoint", durable),
                ("phase", 5),
            ],
        );
        self.checkpoint_seen = true;
    }
}

thread_local! {
    static ACTIVE: RefCell<Option<Rc<RefCell<Trace>>>> = const { RefCell::new(None) };
    static OMIT_FILE_FLUSH: Cell<bool> = const { Cell::new(false) };
    static RUNS: RefCell<BTreeMap<(bool, bool), Option<Taken>>> = const { RefCell::new(BTreeMap::new()) };
}

fn with_trace(f: impl FnOnce(&mut Trace)) {
    let trace = ACTIVE.with(|active| active.borrow().clone());
    if let Some(trace) = trace {
        f(&mut trace.borrow_mut());
    }
}

struct Observing {
    outer: Option<Rc<RefCell<Trace>>>,
    omit_file_flush: bool,
}
impl Drop for Observing {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.outer.take());
        OMIT_FILE_FLUSH.with(|omit| omit.set(self.omit_file_flush));
    }
}

pub(super) fn starting_permission(before: Witness, end: Witness) {
    with_trace(|trace| {
        assert_eq!(end.count - before.count, 2);
        assert_eq!(before.count as i64, trace.state["witness"]);
        assert!(trace.recording.take().is_empty());
        if before.count == 2 {
            trace.step("NextGroup", &[("generation", 1), ("phase", 0)]);
        }
        trace.before_commit = false;
        trace.checkpoint_seen = false;
        trace.step("Prepare", &[("temp_durable", 0), ("phase", 1)]);
    });
}

/// The negative control still flushes the directory: it cannot stand in for the
/// omitted temporary-file flush, even when the new entry survives a crash.
fn directory_only_permission(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    private_file::check_private_dir(dir)?;
    aterm_uds::atomic_file::write_flushed(&dir.join(name), bytes, 0o600, None)?;
    Flush::Durable.dir(dir)?;
    Ok(())
}

pub(super) fn permission_writer(original: PermissionWriter) -> PermissionWriter {
    if OMIT_FILE_FLUSH.with(Cell::get) {
        directory_only_permission
    } else {
        original
    }
}

pub(super) fn permission_written(
    dir: &Path,
    name: &str,
    claim: &[u8],
    before: Witness,
    end: Witness,
) {
    with_trace(|trace| {
        let flushes = trace.recording.take();
        assert!(flushes.len() <= 2);
        let temp_full = flushes.len() == 2 && flushes[0].is_durable();
        let entry_full = flushes.last().is_some_and(|flush| flush.is_durable());
        let actual = group::read(dir, name, claim).unwrap();
        assert!(actual.acknowledged() == before && actual.end() == Some(end));
        trace.step(
            "FlushTemporary",
            &[
                ("temp_durable", i64::from(temp_full)),
                (
                    "durable_witness",
                    if temp_full {
                        trace.state["witness"]
                    } else {
                        trace.state["durable_witness"]
                    },
                ),
                ("phase", 2),
            ],
        );
        trace.step(
            "RenamePermit",
            &[
                ("old_start", trace.state["permit_start"]),
                ("old_end", trace.state["permit_end"]),
                ("old_data", trace.state["permit_data"]),
                ("permit_start", before.count as i64),
                ("permit_end", end.count as i64),
                ("permit_data", trace.state["temp_durable"]),
                ("entry_durable", 0),
                ("phase", 3),
            ],
        );
        trace.step(
            "FlushPermitDirectory",
            &[
                ("entry_durable", i64::from(entry_full)),
                (
                    "durable_witness",
                    if entry_full {
                        trace.state["witness"]
                    } else {
                        trace.state["durable_witness"]
                    },
                ),
                ("phase", 4),
            ],
        );
    });
}

pub(super) fn journal_committed() {
    with_trace(|trace| {
        if !trace.checkpoint_seen {
            trace.checkpoint();
        }
        // WitnessFirst reaches the callback before the journal's before-commit seam.
        // Do not invent a completed commit in that trace.
        if trace.before_commit {
            assert!(trace.recording.take().is_empty());
            trace.step(
                "Commit",
                &[
                    ("durable_checkpoint", trace.state["checkpoint"]),
                    ("phase", 6),
                ],
            );
        }
    });
}

pub(super) fn witness_written(dir: &Path, name: &str, expected: Witness, flush: Flush) {
    with_trace(|trace| {
        let flushes = trace.recording.take();
        assert_eq!(flushes, [flush; 2]);
        let claim = private_file::read_private(dir, name, CLAIM_LEN).unwrap();
        let actual = Witness::decode(&claim[WITNESS_AT..]).unwrap();
        assert!(actual == expected);
        let count = actual.count as i64;
        trace.step("Witness", &[("witness", count), ("phase", 7)]);
        let durable = if flushes.iter().all(|flush| flush.is_durable()) {
            count
        } else {
            trace.state["durable_witness"]
        };
        trace.step(
            "FinishWitness",
            &[("durable_witness", durable), ("phase", 8)],
        );
    });
}

pub(super) fn acknowledged(witness: Witness) {
    with_trace(|trace| {
        assert!(trace.recording.take().is_empty());
        trace.step(
            "Ack",
            &[("acknowledged", witness.count as i64), ("phase", 9)],
        );
    });
}

fn run(strict: bool, apart: bool, omit_file: bool, early_witness: bool) -> Option<Taken> {
    let _strict = strict.then(aterm_journal::seam::serve_strict);
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, &lab._home.path().join("conformance")).unwrap();
    let actual_strict = store.log.as_ref().unwrap().served().served == astream_broker::Tier::Strict;
    if actual_strict != strict {
        return None;
    }
    let wires = super::batch_tests::chain(&session, store.scope, 4);
    let _apart = apart.then(aterm_journal::seam::apart);
    let _early = early_witness
        .then(|| aterm_journal::seam::mutate(aterm_journal::seam::Mutant::WitnessFirst));
    let model = aterm_models_swarm::journal_group_custody_model();
    let mut state = model.init_state();
    state.insert("strict", i64::from(strict));
    state.insert("apart", i64::from(apart));
    let trace = Rc::new(RefCell::new(Trace {
        state,
        taken: Vec::new(),
        recording: Recording::start(),
        dir: store.data_dir.clone(),
        anchor: store.claim[..ANCHOR_LEN].to_vec(),
        checkpoint_seen: false,
        before_commit: false,
    }));
    let _observing = Observing {
        outer: ACTIVE.with(|active| active.replace(Some(Rc::clone(&trace)))),
        omit_file_flush: OMIT_FILE_FLUSH.with(|omit| omit.replace(omit_file)),
    };
    let _hook = aterm_journal::seam::before_each_commit(|| {
        with_trace(|trace| {
            if !trace.checkpoint_seen {
                trace.checkpoint();
            }
            trace.before_commit = true;
        })
    });
    for group in wires.chunks(2) {
        store
            .ingest_batch(&session, &[&group[0], &group[1]])
            .unwrap();
    }
    let taken = trace.borrow().taken.clone();
    Some(taken)
}

/// Environment cuts use the model's stable-write observations; protocol steps
/// come from the corresponding real two-group run, never from model successors.
pub(crate) fn transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    match action {
        "Latch" => {
            next.insert("strict", 1);
        }
        "Apart" => {
            next.insert("apart", 1);
        }
        "Crash" => {
            next.insert("crashed", 1);
            for (kept, actual) in [
                ("kept_checkpoint", "checkpoint"),
                ("kept_witness", "witness"),
                ("kept_start", "permit_start"),
                ("kept_end", "permit_end"),
                ("kept_data", "permit_data"),
                ("kept_floor", "permit_start"),
            ] {
                next.insert(kept, state[actual]);
            }
        }
        "DropCheckpoint" => {
            next.insert("kept_checkpoint", state["durable_checkpoint"]);
        }
        "DropWitness" => {
            next.insert("kept_witness", state["durable_witness"]);
        }
        "DropPermit" => {
            for (kept, old) in [
                ("kept_start", "old_start"),
                ("kept_end", "old_end"),
                ("kept_data", "old_data"),
                ("kept_floor", "old_start"),
            ] {
                next.insert(kept, state[old]);
            }
        }
        "DropPermitData" => {
            next.insert("kept_start", 0);
            next.insert("kept_end", 0);
        }
        _ => {
            let config = (state["strict"] == 1, state["apart"] == 1);
            return RUNS.with(|runs| {
                let mut runs = runs.borrow_mut();
                let taken = runs
                    .entry(config)
                    .or_insert_with(|| run(config.0, config.1, false, false));
                let generation = if action == "NextGroup" {
                    1
                } else {
                    state["generation"]
                };
                taken
                    .as_ref()
                    .unwrap()
                    .iter()
                    .find(|(step, next)| *step == action && next["generation"] == generation)
                    .unwrap_or_else(|| panic!("no actual {action} in generation {generation}"))
                    .1
                    .clone()
            });
        }
    }
    next
}

fn first_unmodelled(taken: &Taken, strict: bool, apart: bool) -> Option<(&'static str, State)> {
    let model = aterm_models_swarm::journal_group_custody_model();
    let mut state = model.init_state();
    state.insert("strict", i64::from(strict));
    state.insert("apart", i64::from(apart));
    for (action, next) in taken {
        if !model.successors(action, &state).contains(next) {
            return Some((action, next.clone()));
        }
        state = next.clone();
    }
    None
}

#[test]
fn two_checkpoint_groups_refine_custody_and_reject_missing_file_flush_or_early_witness() {
    let model = aterm_models_swarm::journal_group_custody_model();
    let mut checked = 0;
    let mut ordered = false;
    for strict in [false, true] {
        for apart in [false, true] {
            let taken = run(strict, apart, false, false);
            if taken.is_some() && !strict {
                ordered = true;
            }
            RUNS.with(|runs| runs.borrow_mut().insert((strict, apart), taken));
        }
    }
    for state in aterm_spec::interp::reachable_states(&model) {
        if state["strict"] == 0 && !ordered {
            continue;
        }
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
    let strict = !ordered;
    let unflushed = run(strict, false, true, false).unwrap();
    let (action, state) =
        first_unmodelled(&unflushed, strict, false).expect("missing permit file flush is caught");
    assert_eq!(action, "FlushTemporary");
    assert_eq!(state["temp_durable"], 0);
    let early = run(strict, false, false, true).unwrap();
    let (action, state) =
        first_unmodelled(&early, strict, false).expect("witness before commit is caught");
    assert_eq!(action, "Witness");
    if ordered {
        assert!(!model.check_invariant("WitnessShowsDurable", &state));
    }
}
