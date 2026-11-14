// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of the pane task to `AosPaneFaithfulReplay`
//! (`aterm_models_aos::aos_pane_faithful_replay_model`). Every chain of the
//! model's actions up to its bound is recorded from a live engine, `PaneCode::ThisBuild` is stepped
//! input by input beside it, and after each input the chain is checked as `rewind check` checks it
//! ([`PaneRecording::check`]): the projection of all of it is the model's own successor, and each
//! dial's successor is one the real check does not reach. The faults are written by this harness
//! into the recording, or come from the debug-only [`PaneCode::DemoClockSkew`]; no shipped code
//! carries a seam for them.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use aterm_aos::contract::Verdict;
use aterm_aos::effects::{EffectRecord, ShadowPort};
use aterm_aos::task::{Code, state_digest};
use aterm_core::config::TerminalConfig;
use aterm_core::grid::ResizePolicy;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_models_aos::aos_pane_faithful_replay_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use super::trace::{PaneCheck, PaneRecording};
use super::*;

const BUILD: &str = "model-conformance";
/// How much later than the live engine parsed it a `SkewedSlice` records its slice.
const SKEW_MS: u64 = 3_600_000;
/// A digest that names no screen of any chain.
const FORGED: Digest = Digest([0xa5; 32]);
/// Milliseconds between two inputs of a chain.
const STEP_MS: u64 = 10;
/// A slice whose screen shows the clock it was parsed at: a prompt mark.
const MARK: &[u8] = b"\x1b]133;A\x07$ ";
/// A device-attributes query, which the live engine answers.
const DA: &[u8] = b"\x1b[c";
/// The reply an altered recording holds for [`DA`].
const ALTERED_DA: &[u8] = b"\x1b[?1;2c";

/// The model's verdicts, as `verdict` numbers them.
const PASS: i64 = 1;
const FAIL_WITNESS: i64 = 2;
const FAIL_REPLIES: i64 = 3;
const FAIL_CLOCK: i64 = 4;

/// The model's faults, as `first` numbers them.
const WITNESS_FAULT: i64 = 1;
const REPLY_FAULT: i64 = 2;
const CLOCK_FAULT: i64 = 3;

fn env() -> PaneEnv {
    PaneEnv {
        config: TerminalConfig::default(),
        policy: ResizePolicy::Native,
        host_resyncs_alt_screen: true,
        host_alternate_scroll: true,
        cell_px: None,
        security_policy: None,
        hyperlink_schemes: Vec::new(),
        kitty_media_admitted: false,
    }
}

fn index(n: usize) -> i64 {
    i64::try_from(n).expect("a chain's counts are small")
}

fn cursor(n: u64) -> i64 {
    i64::try_from(n).expect("a chain's cursors are small")
}

fn clock(ms: u64) -> PaneClock {
    PaneClock {
        offset_ns: ms * 1_000_000,
        wall_ms: Some(1_800_000_000_000 + ms),
    }
}

/// The model's verdict and cursor for a real one.
fn verdict_of(v: &Verdict) -> (i64, i64) {
    match v {
        Verdict::Pass => (PASS, 0),
        Verdict::Fail {
            law, cursor: at, ..
        } => {
            let named = match *law {
                WITNESS_MATCHES => FAIL_WITNESS,
                REPLIES_MATCH => FAIL_REPLIES,
                CLOCK_RECORDED => FAIL_CLOCK,
                other => panic!("the contract has no law `{other}`"),
            };
            (named, cursor(*at))
        }
    }
}

/// A live engine, the recording a host makes of it, and `PaneCode::ThisBuild` stepped through the
/// recording input by input as the check's own replay steps it.
struct Real {
    rec: PaneRecording,
    live: Terminal,
    replay: PaneState,
    code: PrebuiltPane,
    /// The stepped replay's screen differs from the live one, as of the last input.
    div: bool,
    /// The first fault the stepped replay met, and its cursor.
    first: Option<(i64, i64)>,
    /// The live clock of the next input.
    ms: u64,
}

impl Real {
    fn new() -> Real {
        let env = env();
        let mut seed = Terminal::new(6, 30);
        seed.set_host_alternate_scroll(env.host_alternate_scroll);
        let start = PaneState::new(project(&seed).expect("a blank screen projects"), env);
        let rec = PaneRecording::new(ClockReading::now(), start.clone());
        let live = start.engine_on(rec.schema());
        Real {
            rec,
            live,
            replay: start,
            code: PrebuiltPane::new(PaneCode::ThisBuild, BUILD),
            div: false,
            first: None,
            ms: 0,
        }
    }

    /// The chain `actions` names, recorded and stepped from a blank screen.
    fn of(actions: &[&str]) -> Real {
        let mut real = Real::new();
        for action in actions {
            real.act(action);
        }
        real
    }

    fn screen(seen: Option<TerminalCheckpoint>) -> Digest {
        screen_digest(&seen.expect("every chain here is at ground after each input"))
    }

    /// A slice the live engine parses at the next clock, recorded with `recorded` as its clock and
    /// `reply` (or the live engine's own) as its reply.
    fn slice(&mut self, bytes: &[u8], recorded: Option<PaneClock>, reply: Option<&[u8]>) {
        let at = self.rec.schema().reading(clock(self.ms));
        self.live.process_at(bytes, at);
        let own = self.live.take_response().unwrap_or_default();
        self.rec
            .bytes(Arc::from(bytes), recorded, reply.unwrap_or(&own));
    }

    /// The real input `action` names: the live engine's, the recorder's, and the stepped replay's.
    fn act(&mut self, action: &str) {
        let live_clock = clock(self.ms);
        match action {
            "Slice" => self.slice(b"ok ", Some(live_clock), None),
            "SkewedSlice" => self.slice(MARK, Some(clock(self.ms + SKEW_MS)), None),
            "AlteredReply" => self.slice(DA, Some(live_clock), Some(ALTERED_DA)),
            "DroppedClock" => self.slice(b"go ", None, None),
            "Resize" => {
                let (rows, cols) = if self.live.rows() == 6 {
                    (7, 32)
                } else {
                    (6, 30)
                };
                self.live
                    .resize_with_policy(rows, cols, ResizePolicy::Native);
                let _ = self.live.take_reports();
                self.rec.resize(rows, cols);
            }
            "Witness" => {
                let seen = Real::screen(self.live.checkpoint_visible());
                self.rec.witness(seen);
            }
            "ForgedWitness" => self.rec.witness(FORGED),
            other => panic!("no action `{other}` in the pane model"),
        }
        self.ms += STEP_MS;
        self.step();
    }

    /// `PaneCode::ThisBuild`'s step of the input just recorded, its reply shadowed by the recorded
    /// one: the fault it meets, if any, becomes the first one when there was none. Each slice gets
    /// a shadow of its own record, where the check's one shadow stays diverged after a divergence;
    /// the two differ only after the first fault, which is all the projection reads of them.
    fn step(&mut self) {
        let at = self.rec.inputs().len() - 1;
        let input = self.rec.inputs()[at].clone();
        let slices = self.rec.inputs()[..at]
            .iter()
            .filter(|i| matches!(i, PaneInput::Bytes { .. }))
            .count();
        let recorded: &[EffectRecord] = match input {
            PaneInput::Bytes { .. } => &self.rec.effects()[slices..=slices],
            _ => &[],
        };
        let mut port = ShadowPort::new(recorded);
        let stepped = self
            .code
            .step(self.rec.schema(), &mut self.replay, &input, &mut port);
        let fault = match stepped {
            Err(refusal) => panic!("no step of the walk is refused: {refusal:?}"),
            Ok(PaneObservation::Parsed {
                clocked, replies, ..
            }) => match (clocked, replies) {
                (false, _) => Some(CLOCK_FAULT),
                (true, ReplyMatch::Diverged(_)) => Some(REPLY_FAULT),
                (true, ReplyMatch::Matched) => None,
            },
            Ok(PaneObservation::Witnessed { live, replayed }) => {
                (replayed != Some(live)).then_some(WITNESS_FAULT)
            }
            Ok(PaneObservation::Resized { .. }) => None,
            Ok(other) => panic!("the walk records no input that observes {other:?}"),
        };
        if self.first.is_none()
            && let Some(f) = fault
        {
            self.first = Some((f, index(at)));
        }
        let replayed = self
            .replay
            .engine_on(self.rec.schema())
            .checkpoint_visible();
        self.div = Real::screen(replayed) != Real::screen(self.live.checkpoint_visible());
    }

    /// The chain so far, checked as `rewind check` checks it.
    fn check(&self) -> PaneCheck {
        let check = self.rec.check(PaneCode::ThisBuild, BUILD);
        assert_eq!(
            check.state_digest,
            state_digest(self.rec.schema(), &self.replay),
            "the stepped replay is the check's own"
        );
        check
    }

    /// The model's variables, read off the check, the stepped replay and the live engine.
    fn project(&self, check: &PaneCheck) -> State {
        let (first, fat) = self.first.unwrap_or((0, 0));
        let (verdict, at) = verdict_of(&check.verdict);
        let mut s = State::new();
        s.insert("n", index(self.rec.inputs().len()));
        s.insert("z", index(check.counts.resizes));
        s.insert("w", index(check.counts.witnesses));
        s.insert("m", index(check.counts.matched));
        s.insert("div", i64::from(self.div));
        s.insert("first", first);
        s.insert("fat", fat);
        s.insert("verdict", verdict);
        s.insert("at", at);
        s
    }
}

/// The model's verdict and cursor after `actions` from its initial state.
fn verdict(model: &Model, actions: &[&str]) -> (i64, i64) {
    let s = actions
        .iter()
        .fold(model.init_state(), |s, a| successor(model, a, &s));
    (s["verdict"], s["at"])
}

/// One real step: the model's state before it, the action, and the projection after it.
struct Step {
    prev: State,
    action: &'static str,
    got: State,
}

/// The successor `action` takes `state` to under `model`; the action must be enabled.
fn successor(model: &Model, action: &str, state: &State) -> State {
    let mut next = state.clone();
    assert!(model.fire(action, &mut next), "`{action}` is enabled");
    next
}

/// Every chain of the model's actions up to its bound, each recorded and checked after every input,
/// each step checked against the model as it is taken.
fn every_chain(model: &Model) -> Vec<Step> {
    let names: Vec<&'static str> = model.actions.iter().map(|a| a.name).collect();
    let mut steps = Vec::new();
    let mut paths: Vec<Vec<&'static str>> = vec![Vec::new()];
    while let Some(path) = paths.pop() {
        let prev = path
            .iter()
            .fold(model.init_state(), |s, a| successor(model, a, &s));
        for &action in &names {
            if !model.action_enabled(action, &prev) {
                continue;
            }
            let mut next = path.clone();
            next.push(action);
            let real = Real::of(&next);
            let check = real.check();
            let got = real.project(&check);
            let want = successor(model, action, &prev);
            assert_eq!(got, want, "{next:?}: {:?}", check.verdict);
            for inv in &model.invariants {
                assert!(
                    model.check_invariant(inv.name, &got),
                    "{next:?} breaks {}",
                    inv.name
                );
            }
            assert!(interp::admits(model, &prev, &got).is_some(), "{next:?}");
            steps.push(Step {
                prev: prev.clone(),
                action,
                got,
            });
            paths.push(next);
        }
    }
    steps
}

/// [`every_chain`] of the committed model, walked once for every test here.
fn steps() -> &'static [Step] {
    static STEPS: OnceLock<Vec<Step>> = OnceLock::new();
    STEPS.get_or_init(|| every_chain(&aos_pane_faithful_replay_model()))
}

#[test]
fn every_chain_of_the_pane_model_is_a_real_check_in_lock_step() {
    let model = aos_pane_faithful_replay_model();
    let taken: BTreeSet<(State, &str)> =
        steps().iter().map(|s| (s.prev.clone(), s.action)).collect();
    let mut every = BTreeSet::new();
    for state in interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                every.insert((state.clone(), action.name));
            }
        }
    }
    assert_eq!(taken, every, "every transition of the model is a real step");
    let verdicts: BTreeSet<i64> = steps().iter().map(|s| s.got["verdict"]).collect();
    assert_eq!(
        verdicts,
        BTreeSet::from([PASS, FAIL_WITNESS, FAIL_REPLIES, FAIL_CLOCK])
    );
}

/// How many of the steps `pick` selects `dial`'s mutant takes elsewhere than the real check did,
/// each to a state that breaks `law`. The last step of `script` is one: the mutant admits no step
/// to where the real check went.
fn caught(dial: i64, law: &str, script: &[&'static str], pick: impl Fn(&Step) -> bool) -> u32 {
    let model = aos_pane_faithful_replay_model();
    let mutant = interp::with_buggy(&model, dial);
    let (last, path) = script.split_last().expect("a script names a step");
    let prev = path
        .iter()
        .fold(model.init_state(), |s, a| successor(&model, a, &s));
    let real = Real::of(script);
    let got = real.project(&real.check());
    assert_eq!(got, successor(&model, last, &prev), "{script:?}");
    let wrong = successor(&mutant, last, &prev);
    assert_ne!(got, wrong, "{script:?}");
    assert!(!mutant.check_invariant(law, &wrong), "{script:?}");
    assert_eq!(interp::admits(&mutant, &prev, &got), None, "{script:?}");

    let mut caught = 0;
    for s in steps().iter().filter(|s| pick(s)) {
        let wrong = successor(&mutant, s.action, &s.prev);
        if wrong == s.got {
            continue;
        }
        assert!(
            !mutant.check_invariant(law, &wrong),
            "Buggy {dial} from {:?} by {}",
            s.prev,
            s.action
        );
        caught += 1;
    }
    caught
}

/// Dial 1 judges a witness the replayed screen differs from a match. A slice recorded an hour past
/// the clock the live engine parsed it at shows its mark's time, and the real check fails the
/// witness after it by name. The debug-only `DemoClockSkew`, which parses every slice an hour late,
/// fails a faithful recording of the same chain the same way, where this build passes it.
#[test]
fn negative_control_skewed_clock_the_witness_after_it_fails_witness_matches() {
    let script = ["Slice", "SkewedSlice", "Witness"];
    let real = Real::of(&script);
    let check = real.check();
    assert!(
        matches!(
            check.verdict,
            Verdict::Fail {
                law: WITNESS_MATCHES,
                cursor: 2,
                ..
            }
        ),
        "{:?}",
        check.verdict
    );
    let n = caught(1, "WitnessMatches", &script, |s| {
        s.action == "Witness" && s.prev["div"] == 1
    });
    assert!(n > 0, "the chains reached a witness after a skew");

    let model = aos_pane_faithful_replay_model();
    let mut faithful = Real::new();
    faithful.slice(MARK, Some(clock(0)), None);
    let seen = Real::screen(faithful.live.checkpoint_visible());
    faithful.rec.witness(seen);
    let under = |code| verdict_of(&faithful.rec.check(code, BUILD).verdict);
    assert_eq!(
        under(PaneCode::ThisBuild),
        verdict(&model, &["Slice", "Witness"])
    );
    assert_eq!(
        under(PaneCode::DemoClockSkew),
        verdict(&model, &["SkewedSlice", "Witness"]),
        "DemoClockSkew parses the mark an hour late"
    );
    let blind = interp::with_buggy(&model, 1);
    assert_eq!(verdict(&blind, &["SkewedSlice", "Witness"]), (PASS, 0));
}

/// Dial 1 again: a witness whose digest is no screen of the chain fails `WitnessMatches` by name.
#[test]
fn negative_control_witness_digest_mismatch_fails_witness_matches() {
    let script = ["Slice", "ForgedWitness"];
    let check = Real::of(&script).check();
    assert!(
        matches!(
            check.verdict,
            Verdict::Fail {
                law: WITNESS_MATCHES,
                cursor: 1,
                ..
            }
        ),
        "{:?}",
        check.verdict
    );
    let n = caught(1, "WitnessMatches", &script, |s| {
        s.action == "ForgedWitness"
    });
    assert!(n > 0);
}

/// Dial 2 does not judge a reply that diverged. A device-attributes query recorded with another
/// reply than the live engine's fails `RepliesMatch` by name at its slice.
#[test]
fn negative_control_altered_da_reply_fails_replies_match() {
    let script = ["Witness", "AlteredReply"];
    let check = Real::of(&script).check();
    assert!(
        matches!(
            check.verdict,
            Verdict::Fail {
                law: REPLIES_MATCH,
                cursor: 1,
                ..
            }
        ),
        "{:?}",
        check.verdict
    );
    let n = caught(2, "RepliesMatch", &script, |s| s.action == "AlteredReply");
    assert!(n > 0);
}

/// Dial 3 does not judge a slice with no clock. A slice recorded without one fails
/// `ClockRecorded` by name at its slice, whatever follows it.
#[test]
fn negative_control_dropped_clock_fails_clock_recorded() {
    let script = ["Slice", "DroppedClock"];
    let check = Real::of(&["Slice", "DroppedClock", "Witness"]).check();
    assert!(
        matches!(
            check.verdict,
            Verdict::Fail {
                law: CLOCK_RECORDED,
                cursor: 1,
                ..
            }
        ),
        "{:?}",
        check.verdict
    );
    let n = caught(3, "ClockRecorded", &script, |s| s.action == "DroppedClock");
    assert!(n > 0);
}

/// Dial 4 takes a witness that matched for no verdict. The real check passes a chain with no
/// fault, and it passes one that no witness checked as well.
#[test]
fn negative_control_a_faithful_chain_passes() {
    let script = ["Slice", "Witness"];
    assert_eq!(Real::of(&script).check().verdict, Verdict::Pass);
    let n = caught(4, "FaithfulPasses", &script, |s| {
        s.action == "Witness" && s.prev["div"] == 0
    });
    assert!(n > 0);

    let unwitnessed = Real::of(&["Resize", "Slice"]).check();
    assert_eq!(unwitnessed.verdict, Verdict::Pass);
    assert_eq!(unwitnessed.counts.witnesses, 0);
}
