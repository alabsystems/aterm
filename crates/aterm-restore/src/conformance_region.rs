// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for `JournalWriter` and `recover` against `CrashRestoreJournal`.

use std::collections::{BTreeMap, VecDeque};

use aterm_core::terminal::{Terminal, TerminalCheckpoint};
use aterm_models_session::crash_restore_journal_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};

use crate::layout::{
    Geometry, OFF_HIST_IDENTITY, OFF_STREAM_GATHERED, OFF_TAIL_COMMIT, OFF_TAIL_FLOOR,
    kf_commit_word, kf_len_word,
};
use crate::records::{HistoryIdentity, Keyframe, decode_keyframe};
use crate::recover::{RecoverPolicy, Recovery, Restored, recover};
use crate::store::{CountdownStore, RegionView, VecStore};
use crate::writer::{JournalWriter, KeyframeInput, WriterFault, WriterIdentity};

/// The PTY bytes of one unit.
const UNIT: u64 = 5;
/// One unit's tail record: a 16-byte header and five payload bytes padded to eight.
const RECORD: u64 = 24;
/// The live engine's size: the units print on its second row, under the prefill.
const ROWS: u16 = 2;
const COLS: u16 = 12;
/// an early keyframe clears something.
const BRIDGE: usize = 8;
/// The history ring's modulus.
const HIST_CAP: usize = 2048;
/// The config every keyframe and every restorer runs under.
const CONFIG: u64 = 0xC0FF;

const WRITER: WriterIdentity<'static> = WriterIdentity {
    pid: 1,
    birth_us: 1,
    local_id: 1,
    build: 1,
    sid: "tier-1",
};

/// the two poisons are ghosts of the restorer, set on the dead state.
const LIVE: [&str; 7] = [
    "Gather",
    "Slice",
    "SliceClear",
    "Commit",
    "RingReset",
    "BeginKf",
    "FinishKf",
];

fn unit(k: u64, clear: bool) -> Vec<u8> {
    let mut bytes = if clear {
        b"\x1b[3J".to_vec()
    } else {
        b"\x1b[0m".to_vec()
    };
    bytes.push(b'a' + u8::try_from(k.saturating_sub(1)).unwrap_or(0));
    bytes
}

fn int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// The tail ring's capacity for the model's `Cap`.
fn geometry(model: &Model) -> Geometry {
    let cap = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Cap")
        .map_or(2, |(_, v)| *v);
    let tail = usize::try_from(cap).unwrap_or(2) * RECORD as usize;
    Geometry::custom(16 * 1024, tail, 4096).expect("the Tier-1 layout")
}

/// The units a restored screen shows, as their letters, in screen order.
fn letters(checkpoint: &TerminalCheckpoint) -> String {
    Terminal::from_checkpoint(checkpoint)
        .visible_content()
        .chars()
        .filter(|c| ('a'..='c').contains(c))
        .collect()
}

/// die, and what the model counts that no word holds (units processed, the one clear).
pub(crate) struct Rig {
    writer: JournalWriter<CountdownStore>,
    term: Terminal,
    /// Units gathered and processed.
    g: u64,
    p: u64,
    /// The bytes of each processed unit, for the tail append.
    units: Vec<Vec<u8>>,
    /// Units committed to the tail.
    t: u64,
    /// The unit (1-based) that held the clear, 0 for none.
    clear_at: u64,
    /// The engine's clear generation when the rig began.
    base_gen: u64,
}

impl Rig {
    /// (seq 1, slot 1: the model's slot `a`) at tail position 0.
    fn new(model: &Model, budget: Option<usize>, fault: Option<WriterFault>) -> Self {
        let geometry = geometry(model);
        let store = CountdownStore::new(geometry.region_len, budget);
        let mut writer = JournalWriter::create(store, geometry, &WRITER).expect("created");
        if let Some(fault) = fault {
            writer.plant(fault);
        }
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"pre0\r\npre1\r\npre2\r\n");
        let fence = term.history_fence();
        writer
            .reset_history(HistoryIdentity::of(&fence), fence.oldest, HIST_CAP, false)
            .expect("the ring");
        let lines = term
            .history_lines_since_fence(&fence, fence.oldest, 16)
            .expect("the prefill's history");
        assert!(!lines.is_empty(), "the prefill made history");
        writer
            .append_history(fence.oldest, &lines)
            .expect("one chunk");
        let mut rig = Self {
            writer,
            term,
            g: 0,
            p: 0,
            units: Vec::new(),
            t: 0,
            clear_at: 0,
            base_gen: fence.clear_gen,
        };
        rig.begin();
        rig.writer.finish_keyframe().expect("the first keyframe");
        rig
    }

    fn begin(&mut self) {
        let checkpoint = self.term.checkpoint_journal(BRIDGE).expect("ground");
        let input = KeyframeInput {
            checkpoint: &checkpoint,
            fence: self.term.history_fence(),
            resize_lifetime: self.term.resize_lifetime(),
            resize_ordinal: self.term.resize_ordinal(),
            bridge_lines: u32::try_from(BRIDGE).unwrap_or(0),
            config_fp: CONFIG,
            integration_on: self.term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        };
        self.writer.begin_keyframe(&input).expect("a keyframe fits");
    }

    /// The real operation behind one of the model's live actions.
    fn apply(&mut self, action: &str) {
        match action {
            "Gather" => {
                self.g += 1;
                self.writer.set_gathered(self.g * UNIT).expect("gathered");
            }
            "Slice" | "SliceClear" => {
                self.p += 1;
                let clear = action == "SliceClear";
                let bytes = unit(self.p, clear);
                self.term.process(&bytes);
                self.units.push(bytes);
                if clear {
                    self.clear_at = self.p;
                }
            }
            "Commit" => {
                let bytes = self.units[usize::try_from(self.t).expect("small")].clone();
                self.writer.append_slice(&bytes).expect("appended");
                self.t += 1;
            }
            "RingReset" => {
                let fence = self.term.history_fence();
                self.writer
                    .reset_history(HistoryIdentity::of(&fence), fence.oldest, HIST_CAP, true)
                    .expect("reset");
            }
            "BeginKf" => self.begin(),
            "FinishKf" => {
                self.writer.finish_keyframe().expect("finished");
            }
            other => panic!("{other} is not a live action"),
        }
    }

    fn word(&self, at: usize) -> u64 {
        self.writer
            .store()
            .load_u64_acquire(at)
            .expect("a header word")
    }

    /// numbers start at 0 where the code's start at 1).
    fn slot(&self, slot: usize) -> [i64; 6] {
        let commit = self.word(kf_commit_word(slot));
        let len = usize::try_from(self.word(kf_len_word(slot))).unwrap_or(0);
        let geometry = *self.writer.geometry();
        let mut record = vec![0u8; len.min(geometry.kf_cap)];
        self.writer
            .store()
            .read(geometry.kf_slot_off(slot), &mut record)
            .expect("the slot");
        let decode = |seq: u64| decode_keyframe(&record, seq).ok();
        let whole = (commit != 0).then(|| decode(commit)).flatten();
        let held: Option<Keyframe> = whole.clone().or_else(|| (1..=8).find_map(decode));
        let seq = if commit != 0 {
            commit - 1
        } else {
            held.as_ref().map_or(0, |k| k.head.seq - 1)
        };
        [
            i64::from(commit != 0),
            i64::from(whole.is_some()),
            int(seq),
            held.as_ref().map_or(0, |k| int(k.head.tail_from / RECORD)),
            held.as_ref().map_or(0, |k| int(k.head.stream_at / UNIT)),
            held.as_ref()
                .map_or(0, |k| int(k.head.fence.clear_gen - self.base_gen)),
        ]
    }

    /// writer lives (the restore variables are at their initial values).
    pub(crate) fn project(&self) -> State {
        let commits = [self.word(kf_commit_word(0)), self.word(kf_commit_word(1))];
        let seq = commits[0].max(commits[1]);
        let [a_ok, a_full, a_seq, a_at, a_state, a_gen] = self.slot(1);
        let [b_ok, b_full, b_seq, b_at, b_state, b_gen] = self.slot(0);
        let mut s: State = BTreeMap::new();
        for (name, value) in [
            ("g", int(self.word(OFF_STREAM_GATHERED) / UNIT)),
            ("p", int(self.p)),
            ("t", int(self.word(OFF_TAIL_COMMIT) / RECORD)),
            ("floor", int(self.word(OFF_TAIL_FLOOR) / RECORD)),
            ("seq", int(seq)),
            ("to_b", int(seq & 1)),
            ("writing", i64::from(self.writer.keyframe_in_progress())),
            ("a_ok", a_ok),
            ("a_full", a_full),
            ("a_seq", a_seq),
            ("a_at", a_at),
            ("a_state", a_state),
            ("a_gen", a_gen),
            ("b_ok", b_ok),
            ("b_full", b_full),
            ("b_seq", b_seq),
            ("b_at", b_at),
            ("b_state", b_state),
            ("b_gen", b_gen),
            (
                "gen",
                int(self.term.history_fence().clear_gen - self.base_gen),
            ),
            (
                "ring_gen",
                int(self.word(OFF_HIST_IDENTITY + 8) - self.base_gen),
            ),
            ("clear_at", int(self.clear_at)),
            ("live", 1),
        ] {
            s.insert(name, value);
        }
        for ghost in [
            "poison",
            "kpoison",
            "attempts",
            "full_tries",
            "restored",
            "r",
            "rgen",
            "exact",
            "imported",
            "torn",
            "wrong",
            "resurrected",
        ] {
            s.insert(ghost, 0);
        }
        s
    }
}

/// A history join's admission: the real one, or a negative control's.
type Admit = fn(&Restored) -> bool;

/// One transition the sweep replays: from `state` (reached by `path`), `action`.
struct Transition {
    path: Vec<&'static str>,
    state: State,
    action: &'static str,
    next: State,
}

/// path, in breadth-first order.
fn transitions(model: &Model) -> Vec<Transition> {
    let init = model.init_state();
    let mut seen: BTreeMap<State, Vec<&'static str>> = BTreeMap::new();
    seen.insert(init.clone(), Vec::new());
    let mut queue = VecDeque::from([init]);
    let mut out = Vec::new();
    while let Some(state) = queue.pop_front() {
        let path = seen[&state].clone();
        for action in LIVE {
            let mut next = state.clone();
            if !model.fire(action, &mut next) {
                continue;
            }
            if !seen.contains_key(&next) {
                let mut longer = path.clone();
                longer.push(action);
                seen.insert(next.clone(), longer);
                queue.push_back(next.clone());
            }
            out.push(Transition {
                path: path.clone(),
                state: state.clone(),
                action,
                next,
            });
        }
    }
    out
}

/// Replay `path` on a fresh rig with `budget` stores.
fn replay(model: &Model, path: &[&str], budget: Option<usize>, fault: Option<WriterFault>) -> Rig {
    let mut rig = Rig::new(model, budget, fault);
    for action in path {
        rig.apply(action);
    }
    rig
}

/// The restore variables the real harness produced, onto `base`.
fn with(base: &State, vars: &[(&'static str, i64)]) -> State {
    let mut s = base.clone();
    for (name, value) in vars {
        s.insert(name, *value);
    }
    s
}

/// first step the model does not admit.
fn restore_conforms(
    model: &Model,
    region: &VecStore,
    dead: &State,
    base_gen: u64,
    admit: Admit,
) -> Result<(), String> {
    let run = |tail| {
        recover(
            region,
            RecoverPolicy {
                tail,
                config_fp: CONFIG,
                scratch_nonce: [0xA5; 32],
            },
            Terminal::new,
        )
    };
    let full = run(true);
    let screen = run(false);
    let poison = dead["poison"] == 1;
    let kpoison = dead["kpoison"] == 1;
    let mut state = dead.clone();
    let mut attempts = 0i64;
    let mut full_tries = 0i64;
    loop {
        let before = state.clone();
        let tail = attempts == 0;
        let recovery = if attempts >= 2 {
            None
        } else if tail {
            Some(&full)
        } else {
            Some(&screen)
        };
        let restored = match recovery {
            None | Some(Recovery::Meta(_)) => {
                let after = with(
                    &before,
                    &[("restored", 1), ("r", 0), ("exact", 0), ("imported", 1)],
                );
                return step(model, &before, &after, "RestoreMeta");
            }
            Some(Recovery::Restored(x)) => x,
        };
        let shown = letters(&restored.checkpoint);
        let r = int(shown.len() as u64);
        let wrong = !"abc".starts_with(shown.as_str());
        let rgen = int((restored.keyframe.fence.clear_gen - base_gen)
            + (restored.fence_after.clear_gen - restored.fence_before.clear_gen));
        let died = (tail && poison) || kpoison;
        if tail {
            full_tries += 1;
        }
        if died {
            attempts += 1;
        }
        let after = with(
            &before,
            &[
                ("full_tries", full_tries),
                ("attempts", attempts),
                ("restored", i64::from(!died)),
                ("r", r),
                ("rgen", rgen),
                ("exact", i64::from(restored.exact)),
                ("torn", 0),
                ("wrong", i64::from(wrong)),
            ],
        );
        let label = match (tail, restored.slot) {
            (true, 1) => "RestoreFullA",
            (true, _) => "RestoreFullB",
            (false, 1) => "RestoreScreenA",
            (false, _) => "RestoreScreenB",
        };
        step(model, &before, &after, label)?;
        state = after;
        if died {
            continue;
        }
        let ring_gen = int(restored.ring.identity.clear_gen - base_gen);
        let resurrected = admit(restored) && !restored.ring.chunks.is_empty() && ring_gen < rgen;
        let imported = with(
            &state,
            &[("imported", 1), ("resurrected", i64::from(resurrected))],
        );
        return step(model, &state, &imported, "Import");
    }
}

/// One real step, admitted by the model as exactly `label`, every invariant holding after.
fn step(model: &Model, before: &State, after: &State, label: &str) -> Result<(), String> {
    match admits(model, before, after) {
        Some(found) if found == label => {}
        found => {
            return Err(format!(
                "the real {label} is not the model's (admitted as {found:?}): {before:?} -> {after:?}"
            ));
        }
    }
    for invariant in &model.invariants {
        if !model.check_invariant(invariant.name, after) {
            return Err(format!("{label} breaks {}: {after:?}", invariant.name));
        }
    }
    Ok(())
}

/// model's own `Poison`, `PoisonKf` and `Die`, so it is a state the model reaches.
fn dead_state(model: &Model, state: &State, poison: bool, kpoison: bool) -> State {
    let mut s = state.clone();
    for (on, action) in [(poison, "Poison"), (kpoison, "PoisonKf"), (true, "Die")] {
        if on {
            assert!(model.fire(action, &mut s), "{action} from {state:?}");
        }
    }
    s
}

/// interrupted action (`candidates`), it conforms for every poison combination.
fn death_conforms(
    model: &Model,
    region: &VecStore,
    candidates: &[&State],
    base_gen: u64,
    admit: Admit,
) -> Result<(), String> {
    for (poison, kpoison) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut failures = Vec::new();
        let conforms = candidates.iter().any(|state| {
            let dead = dead_state(model, state, poison, kpoison);
            match restore_conforms(model, region, &dead, base_gen, admit) {
                Ok(()) => true,
                Err(why) => {
                    failures.push(why);
                    false
                }
            }
        });
        if !conforms {
            return Err(format!(
                "poison={poison} kpoison={kpoison}: {}",
                failures.join("\n  or ")
            ));
        }
    }
    Ok(())
}

/// What one sweep covered.
#[derive(Debug, Default)]
struct Covered {
    /// Live states with no poison, each the source of some replayed transition.
    states: usize,
    transitions: usize,
    deaths: usize,
}

/// Replay one transition: the step, then a death after each of its stores.
fn check_transition(
    model: &Model,
    tr: &Transition,
    fault: Option<WriterFault>,
    admit: Admit,
) -> Result<usize, String> {
    let mut rig = replay(model, &tr.path, None, fault);
    let before = rig.project();
    if before != tr.state {
        return Err(format!(
            "the witness {:?} does not reach the model's state: real {before:?}, model {:?}",
            tr.path, tr.state
        ));
    }
    let start = rig.writer.store().stores();
    rig.apply(tr.action);
    let after = rig.project();
    if after != tr.next {
        return Err(format!(
            "{} after {:?}: real {after:?}, model {:?}",
            tr.action, tr.path, tr.next
        ));
    }
    step(model, &before, &after, tr.action)?;
    let base_gen = rig.base_gen;
    let stores = rig.writer.store().stores() - start;
    let region = rig.writer.store().snapshot();
    death_conforms(model, &region, &[&tr.next], base_gen, admit)
        .map_err(|why| format!("a death after {} ({:?}): {why}", tr.action, tr.path))?;
    for j in 1..stores {
        let mut dying = replay(model, &tr.path, Some(start + j), fault);
        dying.apply(tr.action);
        assert!(
            dying.writer.store().died(),
            "the budget fell inside {}",
            tr.action
        );
        let region = dying.writer.store().snapshot();
        death_conforms(model, &region, &[&tr.state, &tr.next], base_gen, admit).map_err(|why| {
            format!(
                "a death after store {j} of {} in {} ({:?}): {why}",
                stores, tr.action, tr.path
            )
        })?;
    }
    Ok(stores.max(1))
}

/// every transition that did not conform.
fn sweep(
    fault: Option<WriterFault>,
    admit: Admit,
    stop_at_first: bool,
) -> Result<Covered, Vec<String>> {
    let model = crash_restore_journal_model();
    let all = transitions(&model);
    let states = all
        .iter()
        .map(|tr| &tr.state)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if stop_at_first {
        for tr in &all {
            check_transition(&model, tr, fault, admit).map_err(|why| vec![why])?;
        }
        return Ok(Covered {
            states,
            transitions: all.len(),
            deaths: 0,
        });
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(16);
    let results: Vec<(usize, Vec<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut deaths = 0;
                    let mut failures = Vec::new();
                    while let Some(tr) =
                        all.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        match check_transition(&model, tr, fault, admit) {
                            Ok(n) => deaths += n,
                            Err(why) => failures.push(why),
                        }
                    }
                    (deaths, failures)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("a sweep worker panicked"))
            .collect()
    });
    let failures: Vec<String> = results.iter().flat_map(|(_, f)| f.clone()).collect();
    if failures.is_empty() {
        Ok(Covered {
            states,
            transitions: all.len(),
            deaths: results.iter().map(|(d, _)| d).sum(),
        })
    } else {
        Err(failures)
    }
}

/// transition of the model and at a death after every store of each.
#[test]
fn the_real_journal_conforms_to_the_model() {
    let model = crash_restore_journal_model();
    let rig = Rig::new(&model, None, None);
    assert_eq!(rig.project(), model.init_state(), "the real initial state");
    death_conforms(
        &model,
        &rig.writer.store().snapshot(),
        &[&model.init_state()],
        rig.base_gen,
        Restored::history_admissible,
    )
    .unwrap_or_else(|why| panic!("a death at the start: {why}"));

    match sweep(None, Restored::history_admissible, false) {
        Ok(covered) => {
            eprintln!(
                "CrashRestoreJournal Tier-1: {} states, {} transitions, {} death points, each \
                 restored under four poison combinations",
                covered.states, covered.transitions, covered.deaths
            );
            assert!(covered.states > 800, "{covered:?}");
            assert!(covered.transitions > covered.states, "{covered:?}");
            assert!(covered.deaths > covered.transitions, "{covered:?}");
        }
        Err(failures) => panic!(
            "{} transition(s) do not conform:\n  {}",
            failures.len(),
            failures.join("\n  ")
        ),
    }
}

/// reads valid where the model's is being written: this control proves the step half.
#[test]
fn a_writer_that_publishes_early_is_rejected() {
    let failures = sweep(
        Some(WriterFault::PublishEarly),
        Restored::history_admissible,
        true,
    )
    .expect_err("the sweep must reject PublishEarly");
    eprintln!("PublishEarly rejected: {}", failures[0]);
    assert!(
        failures.iter().any(|f| f.contains("BeginKf")),
        "rejected at a keyframe's begin: {failures:?}"
    );
}

/// floor word is the model's `floor`).
#[test]
fn a_writer_with_no_floor_is_rejected() {
    let failures = sweep(
        Some(WriterFault::NoFloor),
        Restored::history_admissible,
        true,
    )
    .expect_err("the sweep must reject NoFloor");
    eprintln!("NoFloor rejected: {}", failures[0]);
    assert!(
        failures.iter().any(|f| f.contains("Commit")),
        "rejected at an overwriting commit: {failures:?}"
    );
}

/// window; a commit word stored before the copy admits bytes not yet written.
#[test]
fn writers_wrong_only_between_stores_are_rejected_by_their_deaths() {
    for fault in [WriterFault::FloorAfterCopy, WriterFault::CommitBeforeCopy] {
        let failures = sweep(Some(fault), Restored::history_admissible, true)
            .expect_err("the death half must reject it");
        eprintln!("{fault:?} rejected: {}", failures[0]);
        assert!(
            failures[0].starts_with("a death after store "),
            "{fault:?}: rejected by a death between two stores, not by a step: {failures:?}"
        );
    }
}

/// holding the lines it cleared) and is rejected at its `Import`.
#[test]
fn a_join_without_the_identity_check_is_rejected() {
    let failures = sweep(None, |_| true, true).expect_err("the sweep must reject it");
    eprintln!(
        "the join without the identity check rejected: {}",
        failures[0]
    );
    assert!(
        failures.iter().any(|f| f.contains("Import")),
        "rejected at the import: {failures:?}"
    );
}
