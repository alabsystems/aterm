// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bindings for the harness core's three bounded machines: the lossy
//! ledger ring (`harness::ring`, design §11 item 5), the limit-recovery
//! ladder (`harness::limits`, design §11 item 7) and the grid spine's turn
//! machine (`harness::observe`, design §4.2/§5.8.1).
//!
//! A `ty`-green model on its own is a statement about the DESCRIPTION of the
//! code. These tests are what make each one a statement about the code that
//! compiled: they drive the real `Ring` against real files, the real
//! classifier, table and engine against real evidence, and the real
//! `Observer` against real screens, project what those produce onto the
//! model's variables, and check every observed transition against the same
//! derived model checked at Tier 0 — by the in-process interpreter always,
//! and additionally by `ty trace validate` wherever that binary is installed.
//!
//! Every machine also carries FORGED successors the model must refuse, so a
//! green binding is never vacuous: a model that admits everything would pass
//! the positive half of every test here and fail every negative one.
//!
//! What is MEASURED and what is not. Measured: the ring's transitions against
//! files in a temp directory, with each crash shape fabricated on disk (no test
//! can cut power); the engine's decisions for real classifications at injected
//! clock values. Not measured: anything about power loss, and any evidence the
//! design records as unconfirmed (§5.8.9) — a hook payload this suite hands in
//! is a value the classifier may receive, never one it has been seen to
//! receive.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aterm_agent::harness::limits::{
    Action, ActionTable, Carry, Class, Classification, Decision, Evidence, Guards, LimitsConfig,
    Refusal, State as Engine, Step, TableError, Terminal, Verdict, WindowKind, classify, step,
};
use aterm_agent::harness::observe::{Event, Observer, Sample, StatusSample};
use aterm_agent::harness::ring::{Ring, RingConfig};
use aterm_agent::harness::source::Source;
use aterm_agent::supervise::phase::{Phase, worker_phase};
use aterm_agent::supervise::prompt::fixtures;
use aterm_agent::supervise::screen::Screen;
use aterm_spec::derive::{
    Model, harness_failure_recovery_model, harness_ledger_ring_model,
    harness_turn_observation_model,
};
use aterm_spec::verify;

/// The model's variables, as the interpreter and `ty` both take them.
type Vars = BTreeMap<&'static str, i64>;

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

/// A unique scratch root; the ring's own directory is created BY the ring
/// below it, so directory admission runs in every test that uses one.
struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aterm-harness-conformance-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create the scratch root");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn small(value: u64) -> i64 {
    i64::try_from(value).expect("a bounded test quantity")
}

/// Check one observed transition against the model's named action, on every
/// tier that is installed.
fn assert_transition(model: &Model, before: &Vars, after: &Vars, action: &str, label: &str) {
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some(action),
        label,
    );
    assert!(
        accepted,
        "the real {label} is not the model's `{action}`: {before:?} -> {after:?}\n{diagnostics}"
    );
}

/// A successor the real code did NOT produce must be refused as `action`.
///
/// `base` is what the real code DID produce; the forgery is that state with
/// `changes` applied, and it is offered as a successor of `before`. Keeping
/// the two apart is what makes the control a near-miss of the truth rather
/// than of the state before it.
fn assert_forged_rejected(
    model: &Model,
    before: &Vars,
    base: &Vars,
    action: &str,
    changes: &[(&'static str, i64)],
    label: &str,
) {
    let mut forged = base.clone();
    for (name, value) in changes {
        forged.insert(name, *value);
    }
    assert_ne!(
        &forged, base,
        "{label}: the forgery must differ from the truth"
    );
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        &forged,
        Some(action),
        label,
    );
    assert!(
        !accepted,
        "a forged `{action}` was admitted, so the bind is vacuous: {before:?} -> {forged:?}\n\
         {diagnostics}"
    );
}

// ---------------------------------------------------------------------------
// §11 item 5 — the ledger ring
// ---------------------------------------------------------------------------

/// Records per segment and segments per ring: the model's `SegCap` and
/// `Segments`, spelled here as the real config they come from.
const SEG_CAP: u64 = 2;
const SEGMENTS: usize = 3;
/// Every framed uniform record is this long, whatever its id (checked against
/// the real file on the first append, so fixture drift fails loudly).
const RECORD: u64 = 48;

fn uniform_cfg() -> RingConfig {
    RingConfig {
        max_bytes: RECORD * SEG_CAP * (SEGMENTS as u64),
        segments: SEGMENTS,
    }
}

/// A row whose framed record is exactly [`RECORD`] bytes for any id below 100.
fn uniform_line(id: u64) -> String {
    let width = 26 - id.to_string().len();
    format!("{:.<width$}", format!("row-{id}"))
}

/// The ring's segment files as the DIRECTORY shows them, parsed here rather
/// than by the code under test: `(first id, path)`, oldest first.
fn segment_files(dir: &Path, name: &str) -> Vec<(u64, PathBuf)> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries {
        let entry = entry.expect("read a directory entry");
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = file_name.strip_prefix(&format!("{name}.")) else {
            continue;
        };
        let Some(digits) = rest.strip_suffix(".jsonl") else {
            continue;
        };
        if digits.len() == 20 && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            found.push((digits.parse().expect("twenty digits"), entry.path()));
        }
    }
    found.sort();
    found
}

/// The real ring, projected onto the model's variables. Everything but
/// `nextid` and `live` is read from the DIRECTORY by this test's own parser,
/// so the projection never asks the code under test what it believes.
struct RingBound {
    dir: PathBuf,
    model: Model,
    ring: Option<Ring>,
    nextid: u64,
    vars: Vars,
    transitions: usize,
}

impl RingBound {
    const NAME: &'static str = "rm";

    fn new(dir: PathBuf) -> Self {
        let model = harness_ledger_ring_model();
        let ring = Ring::open(&dir, Self::NAME, uniform_cfg()).expect("open the bound ring");
        let bound = RingBound {
            dir,
            vars: model.init_state(),
            model,
            nextid: ring.next_id(),
            ring: Some(ring),
            transitions: 0,
        };
        assert_eq!(
            bound.project(),
            bound.vars,
            "a fresh ring must be the model's Init"
        );
        bound
    }

    fn project(&self) -> Vars {
        let files = segment_files(&self.dir, Self::NAME);
        let (top, fill, torn) = match files.last() {
            None => (1, 0, false),
            Some((first, path)) => {
                let bytes = fs::read(path).expect("read the newest segment");
                let torn = bytes.last().is_some_and(|byte| *byte != b'\n');
                let lines = bytes.iter().filter(|byte| **byte == b'\n').count() + usize::from(torn);
                let fill = small(u64::try_from(lines).expect("small"));
                (small(*first) + fill, fill, torn)
            }
        };
        if let Some(ring) = &self.ring {
            // The ring's own accessors must agree with what the directory says.
            assert_eq!(ring.segment_count(), files.len());
            assert_eq!(
                ring.floor_id(),
                files.first().map_or(ring.next_id(), |(id, _)| *id)
            );
            assert!(
                ring.bytes() <= uniform_cfg().max_bytes,
                "the ring must hold at most one ring's worth of bytes"
            );
        }
        BTreeMap::from([
            ("nextid", small(self.nextid)),
            ("top", top),
            ("lo", small(files.first().map_or(1, |(id, _)| *id))),
            ("segs", small(u64::try_from(files.len()).expect("small"))),
            ("fill", fill),
            ("torn", i64::from(torn)),
            ("live", i64::from(self.ring.is_some())),
            // The real ring has no defect-witness to project: the model's two
            // rotation mutants change nothing at the committed `Buggy = 0`.
            ("forged", 0),
        ])
    }

    fn settle(&mut self, action: &str) {
        let after = self.project();
        let label = format!("ledger ring step {} ({action})", self.transitions);
        let before = self.vars.clone();
        assert_transition(&self.model, &before, &after, action, &label);
        self.vars = after;
        self.transitions += 1;
    }

    fn forged_rejected(&self, before: &Vars, action: &str, changes: &[(&'static str, i64)]) {
        assert_forged_rejected(
            &self.model,
            before,
            &self.vars,
            action,
            changes,
            "ledger ring forgery",
        );
    }

    /// Append one uniform row; the MODEL says which action that must be.
    fn write(&mut self) -> &'static str {
        let action = if self.model.action_enabled("Emit", &self.vars) {
            "Emit"
        } else {
            "Rotate"
        };
        assert!(
            self.model.action_enabled(action, &self.vars),
            "the model refuses a write at {:?}; the real ring never refuses one",
            self.vars
        );
        let ring = self.ring.as_mut().expect("a live ring");
        let id = ring
            .append(&uniform_line(self.nextid))
            .expect("the ring never refuses a well-formed row");
        assert_eq!(id, self.nextid, "append returns the announced id");
        self.nextid = ring.next_id();
        self.settle(action);
        action
    }

    /// Die mid-write: a record's first bytes reach the newest segment.
    fn tear(&mut self) {
        self.ring = None;
        let (_, newest) = segment_files(&self.dir, Self::NAME)
            .pop()
            .expect("the ring has a segment to tear");
        let dirty = fs::read(&newest)
            .expect("read the newest segment")
            .last()
            .is_some_and(|byte| *byte != b'\n');
        let mut torn = Vec::new();
        if dirty {
            torn.push(b'\n'); // the seal a real append writes in front
        }
        torn.extend_from_slice(b"{\"id\":99,\"");
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&newest)
            .expect("open the newest segment to tear it");
        std::io::Write::write_all(&mut file, &torn).expect("write the fragment");
        self.settle("Tear");
    }

    /// Die after creating the next segment, before unlinking or writing.
    fn crash_after_create(&mut self) {
        self.ring = None;
        let path = self
            .dir
            .join(format!("{}.{:020}.jsonl", Self::NAME, self.nextid));
        fs::write(path, b"").expect("fabricate the created segment");
        self.settle("CrashAfterCreate");
    }

    fn crash(&mut self) {
        self.ring = None;
        self.settle("Crash");
    }

    fn reopen(&mut self) {
        let ring = Ring::open(&self.dir, Self::NAME, uniform_cfg()).expect("reopen the ring");
        self.nextid = ring.next_id();
        self.ring = Some(ring);
        self.settle("Reopen");
    }
}

#[test]
fn harness_ledger_ring_real_rotation_and_crash_recovery_match_the_model() {
    let root = TestDir::new("ring");
    let mut bound = RingBound::new(root.0.join("wrap"));

    // The fixture: one uniform record is exactly RECORD bytes on disk. Drift
    // here would silently change what "a full segment" means.
    assert_eq!(bound.write(), "Rotate", "the first row starts a segment");
    let live = bound.ring.as_ref().expect("live");
    assert_eq!(live.bytes(), RECORD, "uniform-record fixture drift");

    let actions: Vec<&str> = (0..5).map(|_| bound.write()).collect();
    assert_eq!(actions, ["Emit", "Rotate", "Emit", "Rotate", "Emit"]);
    assert_eq!(bound.vars["segs"], small(SEGMENTS as u64));
    assert_eq!(bound.vars["top"] - bound.vars["lo"], 6, "exactly full");

    // Crash shape: the new segment exists and the oldest was not unlinked.
    bound.crash_after_create();
    assert_eq!(bound.vars["segs"], 4, "one file too many, on disk");
    bound.reopen();
    assert_eq!((bound.vars["segs"], bound.vars["lo"]), (3, 3));
    assert_eq!(bound.vars["nextid"], 7);

    // Crash shape: a record's first bytes landed and the newline did not.
    let before_tear = bound.vars.clone();
    bound.tear();
    assert_eq!(bound.vars["torn"], 1);
    // NEGATIVE CONTROL: a torn write that reads back as a clean one.
    bound.forged_rejected(&before_tear, "Tear", &[("torn", 0)]);

    let before_reopen = bound.vars.clone();
    bound.reopen();
    assert_eq!(bound.vars["nextid"], 8, "the torn slot 7 is burned");
    // NEGATIVE CONTROL: the defect the ring exists to refuse — a reopen that
    // does not count the fragment's slot and hands id 7 out a second time.
    bound.forged_rejected(&before_reopen, "Reopen", &[("nextid", 7)]);

    assert_eq!(bound.write(), "Emit", "the sealed fragment and one row fit");
    assert_eq!(bound.vars["torn"], 0);

    let before_rotate = bound.vars.clone();
    assert_eq!(bound.write(), "Rotate");
    assert_eq!((bound.vars["segs"], bound.vars["lo"]), (3, 5));
    // NEGATIVE CONTROLS: a rotation that forgot the second-oldest segment too,
    // and one that forgot nothing.
    bound.forged_rejected(&before_rotate, "Rotate", &[("lo", 7), ("segs", 2)]);
    bound.forged_rejected(&before_rotate, "Rotate", &[("lo", 3), ("segs", 4)]);

    bound.crash();
    bound.reopen();
    assert_eq!(bound.vars["nextid"], 10);

    // What survived: the torn slot 7 is junk and never returned, 1..=4 were
    // rotated out, and every id below the floor is reported as dropped.
    let ring = bound.ring.as_ref().expect("live");
    let ids: Vec<u64> = ring
        .read_since(0, 64)
        .expect("read the whole ring")
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, [5, 6, 8, 9], "slot 7 is junk; 1..=4 rotated out");
    assert_eq!(ring.floor_id(), 5);
    assert_eq!(ring.len().expect("count rows"), 4);
}

// ---------------------------------------------------------------------------
// §11 item 7 — the limit-recovery ladder
// ---------------------------------------------------------------------------

/// Every clock value in this half is a literal; nothing reads a wall clock.
const NOW: i64 = 1_789_660_000;
const GEN: u64 = 25;
/// The reset the five-hour window reports, inside the shipped `max_wait_h`.
const RESET: i64 = NOW + 7200;

fn stop_failure(error: &str) -> Evidence {
    Evidence::StopFailure {
        error: error.to_string(),
        details: None,
    }
}

fn window(which: WindowKind, pct: f64, resets_at: Option<i64>) -> Evidence {
    Evidence::Window {
        which,
        used_pct: Some(pct),
        resets_at,
        source: Source::StatusLine,
        age_s: Some(8),
    }
}

fn classified(evidence: &[Evidence]) -> Classification {
    classify(evidence, NOW).unwrap_or_else(|| panic!("no classification for {evidence:?}"))
}

fn action_of(decided: &Step) -> Option<Action> {
    match &decided.decision {
        Decision::Act { action, .. } => Some(*action),
        _ => None,
    }
}

/// The action coding of the model's `act`: the closed vocabulary of §5.8.4.
fn act_code(action: Option<Action>) -> i64 {
    match action {
        None => 0,
        Some(Action::LetVendorRetry) => 1,
        Some(Action::Wait) => 2,
        Some(Action::Retry) => 3,
        Some(Action::SwitchModel | Action::SwitchAccount) => 4,
        Some(Action::Relogin) => 5,
        Some(Action::Escalate) => 6,
        Some(other) => panic!("{other} is outside the ladder this bind drives"),
    }
}

/// The real engine, projected onto the model's variables.
struct RecoveryBound {
    cfg: LimitsConfig,
    guards: Guards,
    model: Model,
    engine: Option<Engine>,
    /// The last action the ENGINE decided in the live generation (its own
    /// output, not this test's opinion); a settled generation has none.
    act: i64,
    now: i64,
    rows: u64,
    vars: Vars,
    transitions: usize,
}

impl RecoveryBound {
    fn new(cfg: LimitsConfig, guards: Guards) -> Self {
        let model = harness_failure_recovery_model();
        let bound = RecoveryBound {
            cfg,
            guards,
            vars: model.init_state(),
            model,
            engine: None,
            act: 0,
            now: NOW,
            rows: 1,
            transitions: 0,
        };
        assert_eq!(
            bound.project(),
            bound.vars,
            "no classification yet must be the model's Init"
        );
        bound
    }

    fn project(&self) -> Vars {
        let (class, phase, inflight, spent, dwell, generation) = match &self.engine {
            None => (0, 0, 0, 0, 0, 0),
            Some(engine) => {
                let phase = match engine.terminal {
                    Some(Terminal::Escalated) => 4,
                    // Settled or Generation: the generation is over and the
                    // host holds no live classification any more.
                    Some(_) => 0,
                    None if engine.in_flight.is_some() => 2,
                    None => 1,
                };
                // Cross-checks: the two phases that ARE readable off the real
                // state must agree with the phase this projection reports.
                assert_eq!(
                    phase == 2,
                    engine.in_flight.is_some(),
                    "phase 2 is exactly one action awaiting its verdict"
                );
                assert_eq!(
                    phase == 4,
                    engine.terminal == Some(Terminal::Escalated),
                    "phase 4 is exactly the escalated terminal"
                );
                let class = Class::ALL
                    .iter()
                    .position(|candidate| *candidate == engine.class)
                    .expect("every class is in Class::ALL");
                let dwell = engine
                    .last_switch_at
                    .is_some_and(|at| self.now < at.saturating_add(small(self.cfg.min_dwell_s)));
                (
                    i64::try_from(class).expect("eight classes"),
                    phase,
                    i64::from(engine.in_flight.is_some()),
                    i64::from(engine.budget.used(self.cfg.budget, self.now)),
                    i64::from(dwell),
                    small(engine.generation.saturating_sub(GEN)),
                )
            }
        };
        BTreeMap::from([
            ("class", class),
            ("phase", phase),
            ("inflight", inflight),
            ("act", if phase == 0 { 0 } else { self.act }),
            ("spent", spent),
            ("dwell", dwell),
            ("gen", generation),
            // The three witnesses the shipping engine never sets. That it
            // never does is the claim; the forged successors below are the
            // control.
            ("stale", 0),
            ("auto", 0),
            ("silent", 0),
            ("broke", 0),
        ])
    }

    fn settle(&mut self, action: &str) {
        let after = self.project();
        let label = format!("recovery step {} ({action})", self.transitions);
        let before = self.vars.clone();
        assert_transition(&self.model, &before, &after, action, &label);
        self.vars = after;
        self.transitions += 1;
    }

    /// The real code changed nothing the model names — a refusal, or evidence
    /// folded into a live classification.
    fn assert_unchanged(&self, why: &str) {
        assert_eq!(self.project(), self.vars, "{why} must change nothing");
    }

    fn forged_rejected(&self, action: &str, changes: &[(&'static str, i64)]) {
        assert_forged_rejected(
            &self.model,
            &self.vars,
            &self.vars,
            action,
            changes,
            "recovery ladder forgery",
        );
    }

    /// Fold one event into the live generation. The engine's own `act` is
    /// cleared where the class moved, because the decision the old row made
    /// went with the row.
    fn observe(&mut self, event: &aterm_agent::harness::limits::Event, action: &str) {
        let before = self
            .engine
            .as_ref()
            .map(|engine| engine.class)
            .expect("a classified generation");
        let engine = self.engine.as_mut().expect("a classified generation");
        engine.observe(event, self.now);
        if engine.class != before {
            self.act = 0;
        }
        self.settle(action);
    }

    /// The host classifies and builds the state for one generation.
    fn classify_as(&mut self, c: &Classification, action: &str) {
        let carry = self
            .engine
            .as_ref()
            .map_or_else(Carry::default, Engine::carry);
        let generation = self.engine.as_ref().map_or(GEN, |engine| engine.generation);
        self.engine = Some(Engine::new(c, self.now, generation, carry));
        self.act = 0;
        self.settle(action);
    }

    /// Ask the engine for one decision. Returns what it decided, unrecorded.
    fn decide(&self) -> Step {
        let engine = self.engine.as_ref().expect("a classified generation");
        step(&self.cfg, engine, &self.guards, self.now, engine.generation)
    }

    /// Ask, then record what it decided, then check the model admits it.
    fn act_now(&mut self, expected: Action, action: &str) -> Step {
        let decided = self.decide();
        assert_eq!(
            action_of(&decided),
            Some(expected),
            "the engine decided {:?}, not {expected}",
            decided.decision
        );
        let engine = self.engine.as_mut().expect("a classified generation");
        engine.commit(&decided, self.rows, self.now);
        self.rows += 1;
        self.act = act_code(Some(expected));
        self.settle(action);
        decided
    }

    fn verdict(&mut self, verdict: Verdict) {
        let engine = self.engine.as_mut().expect("a classified generation");
        engine.verdict(verdict, self.now);
        self.settle("Verdict");
    }

    fn advance_to(&mut self, now: i64, action: Option<&str>) {
        assert!(now >= self.now, "the injected clock only moves forward");
        self.now = now;
        match action {
            Some(action) => self.settle(action),
            None => self.assert_unchanged("time passing with no timer due"),
        }
    }
}

fn level_three_guards(cfg: &LimitsConfig) -> Guards {
    Guards::from_config(cfg, false)
}

#[test]
fn harness_recovery_real_five_hour_ladder_matches_the_model() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    // A real pair: the hook value plus an exhausted readable window. Nothing
    // above L1 may run on a single source, so the pair is what unlocks the
    // switch below.
    let five_hour = classified(&[
        stop_failure("rate_limit"),
        window(WindowKind::FiveHour, 96.0, Some(RESET)),
    ]);
    assert_eq!(five_hour.class, Class::Session5hLimit);
    assert!(!five_hour.unpaired, "StopFailure + window is a pair");
    bound.classify_as(&five_hour, "ClassifySession5h");

    // The row is [switch-account, switch-model, wait, retry, escalate]:
    // switch-account is L4 and this level is 3, so it is skipped, journaled.
    let switched = bound.act_now(Action::SwitchModel, "StartSwitch");
    assert!(
        switched
            .skipped
            .iter()
            .any(|k| k.action == Action::SwitchAccount && k.why.contains("level 4")),
        "the L4 candidate must be skipped and journaled: {:?}",
        switched.skipped
    );
    assert_eq!(bound.vars["inflight"], 1);

    // A second automatic action, while one awaits its verdict, is REFUSED and
    // queued. This is the model's `SecondActionWhileInFlight` at Buggy = 0.
    assert!(matches!(
        bound.decide().decision,
        Decision::Refused {
            why: Refusal::InFlight
        }
    ));
    bound.assert_unchanged("a refused second action");
    bound.settle("SecondActionWhileInFlight");
    // NEGATIVE CONTROL: the defect this law exists to refuse.
    bound.forged_rejected("SecondActionWhileInFlight", &[("inflight", 2)]);

    bound.verdict(Verdict::Executed);
    assert_eq!(bound.vars["spent"], 1, "the switch spent one budget slot");
    assert_eq!(bound.vars["dwell"], 1, "and started the dwell");

    // The ladder walks on from the candidate after the one that ran: `wait`
    // keyed on the window's own reset, then `retry` once that wait has ended.
    bound.act_now(Action::Wait, "ArmWait");
    assert!(
        matches!(bound.decide().decision, Decision::Wait { until } if until == RESET + 60),
        "a retry before its wait has ended is a wait, not an action"
    );

    // Time alone carries the dwell out: nothing else the model names moves.
    bound.advance_to(RESET + 120, Some("DwellElapses"));
    bound.act_now(Action::Retry, "StartRetry");
    bound.verdict(Verdict::Refused);
    assert_eq!(
        bound.vars["spent"], 1,
        "a retry is not a switch: the shared budget is untouched"
    );

    // The last candidate is `escalate`, and it too is conditional: while the
    // window's reset is inside `max_wait_h` and the budget has room, the
    // engine observes rather than calling a human. Nothing is committed, so
    // nothing the model names changes.
    let exhausted = bound.decide();
    assert!(matches!(
        exhausted.decision,
        Decision::Refused {
            why: Refusal::Exhausted
        }
    ));
    assert!(
        exhausted
            .skipped
            .iter()
            .any(|k| k.action == Action::Escalate && k.why.contains("observing")),
        "the skipped escalate must say why: {:?}",
        exhausted.skipped
    );
    bound.assert_unchanged("an exhausted ladder");
}

#[test]
fn harness_recovery_real_stale_generation_and_handover_match_the_model() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    let five_hour = classified(&[
        stop_failure("rate_limit"),
        window(WindowKind::FiveHour, 96.0, Some(RESET)),
    ]);
    bound.classify_as(&five_hour, "ClassifySession5h");

    // A request bound to a generation the state does not have is refused
    // before anything else is read — before the table, the timers or a guard.
    let engine = bound.engine.as_ref().expect("classified");
    let stale = step(
        &bound.cfg,
        engine,
        &bound.guards,
        bound.now,
        engine.generation + 1,
    );
    assert!(matches!(
        stale.decision,
        Decision::Refused {
            why: Refusal::StaleGeneration
        }
    ));
    assert!(stale.skipped.is_empty(), "nothing was even considered");
    bound.assert_unchanged("a stale-generation request");
    bound.settle("StaleGenerationRequest");
    // NEGATIVE CONTROL: acting for a generation the state no longer has.
    bound.forged_rejected("StaleGenerationRequest", &[("stale", 1)]);

    // The generation changes under the state: whatever was pending is
    // dropped, and the budget and dwell carry into the next one.
    bound.act_now(Action::SwitchModel, "StartSwitch");
    assert_eq!(bound.vars["inflight"], 1);
    let engine = bound.engine.as_mut().expect("classified");
    engine.observe(
        &aterm_agent::harness::limits::Event::GenerationChanged(GEN + 1),
        bound.now,
    );
    bound.act = 0;
    bound.settle("GenerationChanged");
    assert_eq!(bound.vars["inflight"], 0, "the pending action was dropped");
    assert_eq!(bound.vars["gen"], 1);

    // `unknown` is the fail-closed class: its row is `["escalate"]` and the
    // table REFUSES any attempt to give it a retry or a switch.
    let unknown = classified(&[stop_failure("invalid_request")]);
    assert_eq!(unknown.class, Class::Unknown);
    assert!(unknown.unpaired, "unknown is never a pair");
    bound.classify_as(&unknown, "ClassifyUnknown");

    let mut table = ActionTable::default();
    assert_eq!(table.get(Class::Unknown), [Action::Escalate]);
    assert!(matches!(
        table.set_names(Class::Unknown, &["retry", "escalate"]),
        Err(TableError::RetryNotAllowed {
            class: Class::Unknown
        })
    ));
    assert!(matches!(
        table.set_names(Class::Unknown, &["switch-model", "escalate"]),
        Err(TableError::SwitchNotAllowed {
            class: Class::Unknown,
            action: Action::SwitchModel
        })
    ));
    assert!(matches!(
        table.set_names(Class::NetworkOffline, &["switch-account", "escalate"]),
        Err(TableError::SwitchNotAllowed {
            class: Class::NetworkOffline,
            action: Action::SwitchAccount
        })
    ));
    assert_eq!(
        table.get(Class::Unknown),
        [Action::Escalate],
        "a refused row leaves the table unchanged"
    );
    // NEGATIVE CONTROLS: the two rows the table just refused, forged as
    // transitions of the live `unknown` generation.
    bound.forged_rejected("RetryInUnknownClass", &[("act", 3)]);
    bound.forged_rejected("SwitchInForbiddenClass", &[("act", 4)]);

    // Escalation waits for the third `unknown` inside the window, so the
    // engine's first answer is a wait, not an action.
    assert!(matches!(bound.decide().decision, Decision::Wait { .. }));
    bound.assert_unchanged("a wait for the escalate-after budget");
    let engine = bound.engine.as_mut().expect("classified");
    for at in [bound.now + 10, bound.now + 20] {
        engine.observe(
            &aterm_agent::harness::limits::Event::Reclassified(unknown.clone()),
            at,
        );
    }
    bound.assert_unchanged("more evidence for the same class");
    bound.act_now(Action::Escalate, "Escalate");
}

#[test]
fn harness_recovery_real_pinned_rows_for_spend_billing_and_auth_match_the_model() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    // Money is a human's call: the row is exactly `["escalate"]`, and the
    // table refuses anything else for it.
    let spend = classified(&[
        stop_failure("billing_error"),
        window(WindowKind::SpendLimit, 99.0, None),
    ]);
    assert_eq!(spend.class, Class::SpendBilling);
    assert!(!spend.unpaired, "hook plus spend window is a pair");
    bound.classify_as(&spend, "ClassifySpendBilling");

    let mut table = ActionTable::default();
    assert_eq!(table.get(Class::SpendBilling), [Action::Escalate]);
    assert!(matches!(
        table.set_names(Class::SpendBilling, &["wait", "escalate"]),
        Err(TableError::Pinned {
            class: Class::SpendBilling,
            action: Action::Wait
        })
    ));
    // NEGATIVE CONTROL: an automatic wait armed for a billing failure.
    bound.forged_rejected("ActOutsideThePinnedRow", &[("act", 2)]);

    bound.act_now(Action::Escalate, "Escalate");
    assert!(matches!(
        bound.decide().decision,
        Decision::Refused {
            why: Refusal::Terminal(Terminal::Escalated)
        }
    ));
    bound.assert_unchanged("a refused step on an escalated billing failure");
    bound.settle("ResumeEscalated");
    bound.forged_rejected("ResumeEscalated", &[("phase", 1), ("auto", 1)]);

    // Auth, on ONE source. CHANGED 2026-09-22 with the narrowed two-source
    // rule (§5.8.2): this block used to assert that the lone hook skipped
    // `relogin` "display-only" and escalated instead. `relogin` opens the
    // VENDOR's own sign-in door for a human to walk through (§5.8.10) — it
    // spends no money, no allowance and no account — so one source reaches
    // it, and the model admits the same transition it admits for a pair.
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);
    let auth = classified(&[stop_failure("authentication_failed")]);
    assert_eq!(auth.class, Class::Auth);
    assert!(auth.unpaired, "a hook value alone is one source");
    bound.classify_as(&auth, "ClassifyAuth");
    assert!(
        bound.decide().skipped.is_empty(),
        "nothing on the auth row is withheld from one source: {:?}",
        bound.decide().skipped
    );
    bound.act_now(Action::Relogin, "StartRelogin");
    bound.verdict(Verdict::Executed);

    // The paired half of the same row: `relogin` and nothing else, and the
    // pinned row refuses every other member whatever the evidence says.
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);
    let paired = Classification {
        unpaired: false,
        ..auth
    };
    bound.classify_as(&paired, "ClassifyAuth");
    assert!(matches!(
        table.set_names(Class::Auth, &["switch-model", "escalate"]),
        Err(TableError::Pinned {
            class: Class::Auth,
            action: Action::SwitchModel
        })
    ));
    // NEGATIVE CONTROL: an account or model switch to answer a login failure.
    bound.forged_rejected("ActOutsideThePinnedRow", &[("act", 4)]);

    bound.act_now(Action::Relogin, "StartRelogin");
    assert_eq!(bound.vars["inflight"], 1);
    bound.verdict(Verdict::Executed);
    assert_eq!(
        bound.vars["spent"], 0,
        "a relogin spends the relogin budget, never the switch budget"
    );
}

/// The model's two class-change laws, on the real engine: a hook value the
/// engine cannot place forces `unknown` and its ladder starts over, and a
/// generation reclassified as another class adopts that class's row. Both
/// mutants are offered as forged successors, so neither law is a ghost.
#[test]
fn harness_recovery_a_class_change_moves_the_ladder_cursor_with_it() {
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);

    let five_hour = classified(&[
        stop_failure("rate_limit"),
        window(WindowKind::FiveHour, 96.0, Some(RESET)),
    ]);
    bound.classify_as(&five_hour, "ClassifySession5h");

    // Walk the five-hour ladder one whole step, so the cursor is NOT at zero
    // when the class moves under it.
    bound.act_now(Action::SwitchModel, "StartSwitch");
    bound.verdict(Verdict::Executed);
    let advanced = bound.engine.as_ref().expect("classified").step;
    assert_eq!(advanced, 2, "the cursor is past the candidate that ran");

    // A `StopFailure.error` outside the closed list forces `unknown` — and
    // the cursor goes with the class. `unknown`'s row is ONE entry long, so
    // a cursor of 2 would walk it past its end for ever.
    bound.observe(
        &aterm_agent::harness::limits::Event::Evidence(stop_failure("a_renamed_hook_value")),
        "UnplaceableHookValue",
    );
    let engine = bound.engine.as_ref().expect("classified");
    assert_eq!(engine.class, Class::Unknown);
    assert_eq!(engine.step, 0, "the new row is entered at its beginning");
    // NEGATIVE CONTROL: the class flips and the ladder is left behind.
    bound.forged_rejected("UnplaceableHookValue", &[("silent", 1)]);

    // Two more inside the window make the burst, and the row's one candidate
    // — `escalate` — is reached. That is the whole point of the reset.
    for at in [bound.now + 1, bound.now + 2] {
        let engine = bound.engine.as_mut().expect("classified");
        engine.observe(
            &aterm_agent::harness::limits::Event::Evidence(stop_failure("a_renamed_hook_value")),
            at,
        );
    }
    bound.assert_unchanged("more unplaceable evidence for the same class");
    bound.advance_to(bound.now + 3, None);
    bound.act_now(Action::Escalate, "Escalate");

    // The other half: a live generation reclassified as `spend-billing`
    // adopts the pinned row instead of letting the old one decide.
    let cfg = LimitsConfig::default();
    let guards = level_three_guards(&cfg);
    let mut bound = RecoveryBound::new(cfg, guards);
    bound.classify_as(&five_hour, "ClassifySession5h");
    bound.act_now(Action::SwitchModel, "StartSwitch");
    bound.verdict(Verdict::Executed);

    let spend = classified(&[
        stop_failure("billing_error"),
        window(WindowKind::SpendLimit, 99.0, None),
    ]);
    assert_eq!(spend.class, Class::SpendBilling);
    bound.observe(
        &aterm_agent::harness::limits::Event::Reclassified(spend),
        "ReclassifiedAsSpendBilling",
    );
    let engine = bound.engine.as_ref().expect("classified");
    assert_eq!(engine.class, Class::SpendBilling);
    assert_eq!(engine.step, 0);
    // NEGATIVE CONTROL: the class moves and the old row's decision stands —
    // a model switch left standing for a generation whose latest evidence is
    // `billing_error`.
    bound.forged_rejected("ReclassifiedAsSpendBilling", &[("act", 4)]);

    // Money is a human's call whatever the previous class was doing.
    bound.act_now(Action::Escalate, "Escalate");
}

// ---------------------------------------------------------------------------
// The grid spine's turn machine (design §4.2, §5.8.1)
// ---------------------------------------------------------------------------

/// The real [`Observer`], projected onto the turn model's variables.
///
/// Two of the three variables are read from the observer itself, because a
/// pure in-memory machine has no second place to read them from: `turn` is
/// `Observer::turn_source` (the event stream CANNOT answer it — a status-opened
/// turn upgraded by a later grid read emits nothing) and `exited` is
/// `Observer::exited`. The third, `statusclosed`, is computed BY THIS TEST from
/// the observed transition — a grid-opened turn that went away on a pass which
/// read no grid and saw no exit — so the witness the law is stated over is
/// never something the code under test was asked about.
struct ObserverBound {
    model: Model,
    obs: Observer,
    vars: Vars,
    at_ms: u64,
    transitions: usize,
}

impl ObserverBound {
    fn new() -> Self {
        let model = harness_turn_observation_model();
        let vars = model.init_state();
        ObserverBound {
            model,
            obs: Observer::new(),
            vars,
            at_ms: 0,
            transitions: 0,
        }
    }

    /// The host builds a fresh observer for the next session — the model's
    /// `NewSession`, and the only way out of `exited`.
    fn new_session(&mut self) {
        let before = self.vars.clone();
        self.obs = Observer::new();
        let mut after = before.clone();
        after.insert("turn", 0);
        after.insert("exited", 0);
        self.check(&before, &after, "NewSession");
    }

    /// One pass of the REAL observer over one sample, checked as `action`.
    fn pass(&mut self, sample: &Sample, action: &str) -> Vec<Event> {
        let before = self.vars.clone();
        let had_grid = sample.grid.is_some();
        self.at_ms += 1;
        let events = self.obs.on_sample(sample, self.at_ms);
        // The model's `turn` is `{none, grid, status}`, and the position
        // admits exactly those two sources. Anything else is a PROJECTION
        // FAILURE, not a third state to invent a number for: the bind would
        // stop being about the observer that actually ran.
        let turn = match self.obs.turn_source() {
            None => 0,
            Some(Source::Grid) => 1,
            Some(Source::Status) => 2,
            Some(other) => panic!("a turn opened by {other}, which the model does not model"),
        };
        let exited = i64::from(self.obs.exited());
        // THE WITNESS, computed here and nowhere else: a grid-opened turn is
        // gone, no grid was read this pass, and the program did not exit.
        let closed_blind = before["turn"] == 1 && turn == 0 && !had_grid && exited == 0;
        let mut after = before.clone();
        after.insert("turn", turn);
        after.insert("exited", exited);
        if closed_blind {
            after.insert("statusclosed", 1);
        }
        self.check(&before, &after, action);
        events
    }

    fn check(&mut self, before: &Vars, after: &Vars, action: &str) {
        assert_transition(&self.model, before, after, action, "the harness observer");
        self.vars = after.clone();
        self.transitions += 1;
    }

    /// A successor the real observer did NOT produce must be refused.
    fn forged_rejected(&self, before: &Vars, action: &str, changes: &[(&'static str, i64)]) {
        assert_forged_rejected(
            &self.model,
            before,
            &self.vars,
            action,
            changes,
            "the harness observer",
        );
    }
}

/// A screen the SHIPPED reader calls busy: a spinner row over the composer.
/// `aterm_phase::phase::worker_phase` is what decides, and this test asserts
/// that directly first, so a fixture that stopped reading busy fails here
/// rather than silently turning every `GridOpensTurn` below into a no-op.
fn busy_rows() -> Vec<String> {
    let mut rows = fixtures::rows(&["⏺ Running the tests.", "", "✻ Synthesizing… (18s)", ""]);
    rows.extend(fixtures::composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt",
    ));
    rows
}

/// A screen the shipped reader calls idle: one thing said, then the composer.
fn idle_rows() -> Vec<String> {
    let mut rows = fixtures::rows(&["⏺ All done."]);
    rows.extend(fixtures::composer("  ? for shortcuts"));
    rows
}

fn screen(rows: Vec<String>, seq: u64) -> Screen {
    Screen {
        rows,
        cursor_row: 0,
        cursor_col: 0,
        seq,
        first: 0,
    }
}

/// A `status` line in the ADOPTED shape this harness is built for
/// (`detail=-`), with the phase and revision each test varies.
fn status(phase: &str, revision: u64) -> StatusSample {
    StatusSample::parse_line(&format!(
        "OK schema=1 sid=s-1 observed=true phase={phase} since_ms=100 outcome=none \
         exit_code=- signal=- detail=- confidence=strong \
         reasons=fg_job,content_activity attribution=adopted conflict=false \
         revision={revision} enabled=true hold=0"
    ))
    .expect("the fixture status line parses")
}

fn sample(phase: &str, revision: u64, grid: Option<Screen>) -> Sample {
    Sample {
        status: status(phase, revision),
        grid,
        offscreen: None,
        search: None,
    }
}

/// §4.2/§5.8.1: the writer/reader pair. A turn opened by the GRID is closed by
/// a later grid read or by the exit, and NEVER by `status` alone — "did not
/// look" is not "not busy". The forged successors are the two defects the
/// model arms, offered against what the real observer actually produced.
#[test]
fn harness_observer_never_closes_a_grid_turn_from_status_alone() {
    assert_eq!(
        worker_phase(&busy_rows()),
        Phase::Busy,
        "the busy fixture must read busy through the SHIPPED reader"
    );
    assert_eq!(
        worker_phase(&idle_rows()),
        Phase::Idle,
        "the idle fixture must read idle through the SHIPPED reader"
    );

    let mut bound = ObserverBound::new();

    // The grid opens the turn.
    let events = bound.pass(
        &sample("running", 1, Some(screen(busy_rows(), 10))),
        "GridOpensTurn",
    );
    assert!(
        events.iter().any(|e| e.kind.name() == "turn-began"),
        "the grid's busy reading begins a turn: {:?}",
        events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
    );
    assert_eq!(bound.obs.turn_source(), Some(Source::Grid));

    // A pass that reads NO grid, with `status` saying the session went quiet.
    // The turn stays open, and no `turn-ended` is emitted.
    let before = bound.vars.clone();
    let events = bound.pass(&sample("quiet", 2, None), "StatusWouldCloseAGridTurn");
    assert!(
        !events.iter().any(|e| e.kind.name() == "turn-ended"),
        "a quiet status with no grid read is not a turn boundary: {:?}",
        events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
    );
    assert_eq!(bound.obs.turn_source(), Some(Source::Grid));
    // NEGATIVE CONTROL: the same pass, closing the turn from `status` alone.
    bound.forged_rejected(
        &before,
        "StatusWouldCloseAGridTurn",
        &[("turn", 0), ("statusclosed", 1)],
    );

    // `status=idle` with no grid is the same answer: still open.
    bound.pass(&sample("idle", 3, None), "StatusWouldCloseAGridTurn");
    assert!(bound.obs.turn_in_flight());

    // The GRID closes what the grid opened.
    let events = bound.pass(
        &sample("idle", 4, Some(screen(idle_rows(), 11))),
        "GridClosesTurn",
    );
    assert!(
        events.iter().any(|e| e.kind.name() == "turn-ended"),
        "the grid's idle reading ends the turn: {:?}",
        events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
    );
    assert_eq!(bound.obs.turn_source(), None);
}

/// The other half of the asymmetry: a turn `status` opened may be closed by
/// `status`, and a grid reading UPGRADES its source — the arm that emits no
/// event at all, which is why this bind reads the observer's own state.
#[test]
fn harness_observer_upgrades_a_status_turn_and_closes_it_where_it_may() {
    let mut bound = ObserverBound::new();

    // No grid this pass: the weaker source may OPEN.
    let events = bound.pass(&sample("running", 1, None), "StatusOpensTurn");
    assert!(events.iter().any(|e| e.kind.name() == "turn-began"));
    assert_eq!(bound.obs.turn_source(), Some(Source::Status));

    // `status` closes what `status` opened. That is not the defect.
    let events = bound.pass(&sample("idle", 2, None), "StatusClosesItsOwnTurn");
    assert!(events.iter().any(|e| e.kind.name() == "turn-ended"));
    assert_eq!(
        bound.vars["statusclosed"], 0,
        "closing its own turn is no witness"
    );

    // Open one from `status` again, then let the grid upgrade it.
    bound.pass(&sample("running", 3, None), "StatusOpensTurn");
    let before = bound.vars.clone();
    let events = bound.pass(
        &sample("running", 4, Some(screen(busy_rows(), 12))),
        "GridUpgradesTheSource",
    );
    assert!(
        !events
            .iter()
            .any(|e| e.kind.name() == "turn-began" || e.kind.name() == "turn-ended"),
        "an upgrade is not a turn boundary: {:?}",
        events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
    );
    assert_eq!(bound.obs.turn_source(), Some(Source::Grid));
    // NEGATIVE CONTROL: an upgrade that closed the turn instead.
    bound.forged_rejected(&before, "GridUpgradesTheSource", &[("turn", 0)]);

    // And now it is the grid's to close — which is the whole point of the
    // upgrade: a status-opened turn does not become uncloseable.
    bound.pass(
        &sample("idle", 5, Some(screen(idle_rows(), 13))),
        "GridClosesTurn",
    );
    assert!(!bound.obs.turn_in_flight());
}

/// The exit closes what it finds, whatever opened it, and the observer says
/// `exited` exactly once. A fresh observer is the model's `NewSession`.
#[test]
fn harness_observer_closes_an_open_turn_under_the_exit() {
    let mut bound = ObserverBound::new();
    bound.pass(
        &sample("running", 1, Some(screen(busy_rows(), 10))),
        "GridOpensTurn",
    );

    let before = bound.vars.clone();
    let events = bound.pass(&sample("exited", 2, None), "SessionExited");
    let names: Vec<&str> = events.iter().map(|e| e.kind.name()).collect();
    assert!(
        names.contains(&"turn-ended") && names.contains(&"exited"),
        "the exit closes the open turn under it: {names:?}"
    );
    assert!(bound.obs.exited());
    assert!(!bound.obs.turn_in_flight());
    // NEGATIVE CONTROL: an exit that leaves the turn in flight.
    bound.forged_rejected(&before, "SessionExited", &[("turn", 1)]);

    // `exited` is emitted once; a second exited sample adds nothing and the
    // machine holds still.
    let events = bound.pass(&sample("exited", 3, None), "ExitedSampleRepeats");
    assert!(
        !events.iter().any(|e| e.kind.name() == "exited"),
        "`exited` is emitted once: {:?}",
        events.iter().map(|e| e.kind.name()).collect::<Vec<_>>()
    );
    assert_eq!(bound.vars["exited"], 1);

    bound.new_session();
    assert!(!bound.obs.exited());
    assert_eq!(
        bound.transitions, 4,
        "every pass was checked against the model"
    );
}
