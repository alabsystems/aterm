// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for the harness tape models.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use astream_effects::EffectRecord;
use aterm_models_harness::{
    supervise_replay_cursor_model, supervise_tape_recovery_model, supervisor_raised_latch_model,
};
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::rng::XorShift;
use aterm_spec::xref::StepEvidence;

use super::codec::{ctl_reply, ctl_request};
use super::ctl::is_act;
use super::doors;
use super::frame::{Hint, MAGIC, TapeWriter};
use super::frame_tests::{HEADER, Memory, Said, arranged, mono, noise, screen_read};
use super::header::VERBATIM;
use super::recover::{Check, Keep, Limits, Recovered, Tail, read, recover};
use super::replay::ReplayCore;
use crate::supervise::fx::Fx;
use crate::supervise::{Ctl, CtlReply};

const MACHINE: &str = "SuperviseTapeRecovery";

/// The variables a reading decides, as the real reading has them.
pub(crate) fn project(read: &Recovered) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("done", 1),
        ("recovered", i64::try_from(read.frames).unwrap_or(i64::MAX)),
        (
            "refused",
            i64::from(matches!(read.tail, Tail::Fault { .. })),
        ),
        (
            "said_torn",
            i64::from(matches!(read.tail, Tail::Torn { .. })),
        ),
    ])
}

fn walk(model: &Model, schedule: &[&str]) -> State {
    let mut state = model.init_state();
    for action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
    }
    state
}

struct Tape {
    bytes: Vec<u8>,
    ends: Vec<usize>,
    records: Vec<Vec<crate::supervise::tape::codec::Item>>,
}

fn write_tape(model: &Model, ev: &mut StepEvidence, frames: u64) -> Tape {
    let sink = Memory::default();
    let mut state = model.init_state();
    let written_now = |sink: &Memory| {
        let read = recover(&sink.bytes());
        assert_eq!(read.tail, Tail::Clean, "a writer's frames are whole");
        i64::try_from(read.frames).unwrap_or(i64::MAX)
    };
    let mut w = ev.step("Write", || {
        TapeWriter::start(
            Box::new(sink.clone()),
            "/t/bind.atape",
            HEADER,
            Box::new(Said::default()),
        )
    });
    assert!(model.fire("Write", &mut state));
    assert_eq!(written_now(&sink), state["written"], "the header frame");
    let mut rng = XorShift::new(29);
    let mut records = Vec::new();
    for frame in 0..frames - 1 {
        let items = vec![
            mono(frame * 9),
            screen_read(frame as usize),
            noise(&mut rng, 24),
        ];
        for item in &items {
            w.append(item);
        }
        ev.step("Write", || w.flush_hint(Hint::Act));
        assert!(model.fire("Write", &mut state));
        assert_eq!(
            written_now(&sink),
            state["written"],
            "records frame {frame}"
        );
        records.push(items);
    }
    assert_eq!(
        model.action_enabled("Write", &state),
        frames < 3,
        "the model holds at most three frames: the longest tape written here"
    );
    let ends = sink
        .taken()
        .writes
        .iter()
        .scan(0, |end, len| {
            *end += len;
            Some(*end)
        })
        .collect();
    Tape {
        bytes: sink.bytes(),
        ends,
        records,
    }
}

struct Case {
    what: String,
    bytes: Vec<u8>,
    schedule: Vec<&'static str>,
}

fn cases(tape: &Tape) -> Vec<Case> {
    let mut out = Vec::new();
    for cut in 0..=tape.bytes.len() {
        let whole = tape.ends.iter().filter(|e| **e <= cut).count();
        let mut schedule = vec!["Write"; whole];
        schedule.push(if cut == 0 || tape.ends.contains(&cut) {
            "Crash"
        } else {
            "Tear"
        });
        schedule.push("Recover");
        out.push(Case {
            what: format!("{}-frame tape, cut at {cut}", tape.ends.len()),
            bytes: tape.bytes[..cut].to_vec(),
            schedule,
        });
    }
    let n = tape.ends.len();
    let mut of_len: Vec<Vec<usize>> = vec![Vec::new()];
    let mut orders = Vec::new();
    for _ in 0..=n {
        of_len = of_len
            .iter()
            .flat_map(|o| (0..n).map(move |i| [o.as_slice(), &[i]].concat()))
            .collect();
        orders.extend(of_len.iter().cloned());
    }
    for order in orders {
        if order
            .iter()
            .enumerate()
            .all(|(place, frame)| place == *frame)
        {
            continue;
        }
        let mut schedule = vec!["Write"; n];
        schedule.extend(["Crash", "Misplace", "Recover"]);
        out.push(Case {
            what: format!("{n}-frame tape, frames {order:?}"),
            bytes: arranged(&tape.bytes, &order),
            schedule,
        });
    }
    let last_start = match tape.ends.as_slice() {
        [.., before, _] => *before,
        _ => MAGIC.len(),
    };
    for at in 0..tape.bytes.len() {
        for mask in [0x01u8, 0x80, 0xff] {
            let mut bytes = tape.bytes.clone();
            bytes[at] ^= mask;
            let damage = if at < MAGIC.len() {
                "RotMagic"
            } else if at < last_start {
                "Rot"
            } else {
                "RotLast"
            };
            let mut schedule = vec!["Write"; tape.ends.len()];
            schedule.extend(["Crash", damage, "Recover"]);
            out.push(Case {
                what: format!("{}-frame tape, byte {at} ^ {mask:#04x}", tape.ends.len()),
                bytes,
                schedule,
            });
        }
    }
    out
}

struct Disagreement {
    what: String,
    schedule: Vec<&'static str>,
    model: State,
    real: State,
    broken: Vec<&'static str>,
}

impl std::fmt::Debug for Disagreement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({:?}): model {:?}, real {:?}, breaking {:?}",
            self.what, self.schedule, self.model, self.real, self.broken
        )
    }
}

fn bind(
    model: &Model,
    tape: &Tape,
    reader: &dyn Fn(&[u8]) -> Recovered,
    mut ev: Option<&mut StepEvidence>,
) -> Vec<Disagreement> {
    let mut out = Vec::new();
    for case in cases(tape) {
        let expected = walk(model, &case.schedule);
        let reading = match ev.as_deref_mut() {
            Some(ev) => ev.step("Recover", || reader(&case.bytes)),
            None => reader(&case.bytes),
        };
        let mut real = expected.clone();
        real.extend(project(&reading));
        let broken: Vec<&'static str> = model
            .invariants
            .iter()
            .map(|inv| inv.name)
            .filter(|name| !model.check_invariant(name, &real))
            .collect();
        let frames = reading.frames.saturating_sub(1).min(tape.records.len());
        let prefix: Vec<_> = tape.records[..frames].iter().flatten().cloned().collect();
        if real != expected || !broken.is_empty() || reading.items != prefix {
            out.push(Disagreement {
                what: case.what,
                schedule: case.schedule,
                model: expected,
                real,
                broken,
            });
        }
    }
    out
}

#[test]
fn the_real_writer_and_reader_conform_to_the_model() {
    let model = interp::with_buggy(&supervise_tape_recovery_model(), 0);
    let mut ev = StepEvidence::new(MACHINE);
    for frames in 1..=3 {
        let tape = write_tape(&model, &mut ev, frames);
        assert_eq!(tape.ends.len() as u64, frames);
        let misplaced = cases(&tape)
            .iter()
            .filter(|c| c.schedule.contains(&"Misplace"))
            .count();
        assert_eq!(
            misplaced,
            [1, 12, 117][tape.ends.len() - 1],
            "{frames}-frame tape"
        );
        let wrong = bind(&model, &tape, &recover, Some(&mut ev));
        assert!(
            wrong.is_empty(),
            "{} reading(s) of the {frames}-frame tape the model does not admit, first: {:?}",
            wrong.len(),
            wrong.first()
        );
    }
    let audit = ev.audit();
    assert_eq!(
        audit.actions.iter().copied().collect::<Vec<_>>(),
        ["Recover", "Write"]
    );
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

#[test]
fn a_reader_that_ignores_xxh64_is_rejected() {
    let model = interp::with_buggy(&supervise_tape_recovery_model(), 0);
    let blind_model = interp::with_buggy(&supervise_tape_recovery_model(), 1);
    let tape = write_tape(&model, &mut StepEvidence::new(MACHINE), 3);
    let blind = |bytes: &[u8]| read(bytes, Keep::Refuse, Check::Nothing, Limits::TAPE);
    let wrong = bind(&model, &tape, &blind, None);
    assert!(
        !wrong.is_empty(),
        "the bind rejects a checksum-blind reader"
    );
    let read_through: Vec<&Disagreement> = wrong
        .iter()
        .filter(|d| d.broken.contains(&"RotRefused"))
        .collect();
    assert!(
        !read_through.is_empty(),
        "a damaged frame is read as written: {:?}",
        wrong.first()
    );
    let predicted = read_through.iter().any(|d| {
        let blind_state = walk(&blind_model, &d.schedule);
        ["recovered", "refused", "said_torn"]
            .iter()
            .all(|v| blind_state[v] == d.real[v])
    });
    assert!(predicted, "the Buggy = 1 machine predicts a read-through");
    assert!(bind(&model, &tape, &recover, None).is_empty());
}

#[test]
fn a_reader_that_ignores_places_is_rejected() {
    let model = interp::with_buggy(&supervise_tape_recovery_model(), 0);
    let unplaced_model = interp::with_buggy(&supervise_tape_recovery_model(), 6);
    let tape = write_tape(&model, &mut StepEvidence::new(MACHINE), 3);
    let unplaced = |bytes: &[u8]| read(bytes, Keep::Refuse, Check::Unplaced, Limits::TAPE);
    let wrong = bind(&model, &tape, &unplaced, None);
    assert!(
        wrong.iter().all(|d| d.schedule.contains(&"Misplace")),
        "only arrangements: {:?}",
        wrong.iter().find(|d| !d.schedule.contains(&"Misplace"))
    );
    let read_through: Vec<&Disagreement> = wrong
        .iter()
        .filter(|d| d.broken.contains(&"MisplacedRefused"))
        .collect();
    assert!(
        !read_through.is_empty(),
        "frames out of place are read as written: {:?}",
        wrong.first()
    );
    let predicted = read_through.iter().any(|d| {
        let unplaced_state = walk(&unplaced_model, &d.schedule);
        ["recovered", "refused", "said_torn"]
            .iter()
            .all(|v| unplaced_state[v] == d.real[v])
    });
    assert!(predicted, "the Buggy = 6 machine predicts a read-through");
}

const CURSOR: &str = "SuperviseReplayCursor";

const TAPE_WORDS: [&[&str]; 4] = [&["status"], &["key", "1"], &["meta"], &["key", "2"]];

const CURSOR_MOVES: [&str; 7] = [
    "AskNext",
    "AskOtherRead",
    "AskRepeat",
    "AskClosingAct",
    "AskOtherAct",
    "AskPastEnd",
    "AskAfterDivergence",
];

const STRICT_MOVES: [&str; 5] = [
    "AskNext",
    "AskOtherRead",
    "AskOtherAct",
    "AskPastEnd",
    "AskAfterDivergence",
];

fn ok_reply(what: &str) -> Result<CtlReply, String> {
    Ok(CtlReply {
        code: 0,
        stdout: format!("OK {what}\n"),
        stderr: String::new(),
    })
}

fn cursor_tape() -> Vec<EffectRecord> {
    TAPE_WORDS
        .iter()
        .map(|words| EffectRecord::Exchange {
            door: doors::CTL.to_string(),
            request: ctl_request(words),
            reply: ctl_reply(&ok_reply(&words.join(" "))),
        })
        .collect()
}

fn ask_for(action: &str, pos: i64) -> Vec<&'static str> {
    let at = |p: i64| TAPE_WORDS[usize::try_from(p.clamp(0, 3)).unwrap_or(0)].to_vec();
    match action {
        "AskNext" | "AskAfterDivergence" => at(pos),
        "AskOtherRead" if pos <= 1 => vec!["meta"],
        "AskOtherRead" => vec!["status"],
        "AskRepeat" => at(pos - 1),
        "AskClosingAct" => at(pos + 1),
        "AskOtherAct" => vec!["key", "9"],
        "AskPastEnd" => vec!["status"],
        other => panic!("no move {other}"),
    }
}

fn record_of(reply: &CtlReply) -> Option<usize> {
    TAPE_WORDS
        .iter()
        .position(|w| reply.stdout == format!("OK {}\n", w.join(" ")))
}

fn closing_at(pos: usize) -> usize {
    (pos..TAPE_WORDS.len())
        .find(|&p| is_act(TAPE_WORDS[p]))
        .unwrap_or(TAPE_WORDS.len())
}

/// What a cursor did, as the model's variables see it.
#[derive(Debug, Default, Clone)]
pub(crate) struct CursorSeen {
    pos: i64,
    diverged: bool,
    live: i64,
    skipped_act: i64,
    served_after: i64,
    served_future: i64,
}

/// The variables a cursor decides, as it has them.
pub(crate) fn project_cursor(seen: &CursorSeen) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("pos", seen.pos),
        ("diverged", i64::from(seen.diverged)),
        ("live", seen.live),
        ("skipped_act", seen.skipped_act),
        ("served_after", seen.served_after),
        ("served_future", seen.served_future),
    ])
}

trait Cursor {
    fn ask(&mut self, words: &[&str]) -> bool;
    fn seen(&self) -> CursorSeen;
}

struct RealCursor {
    core: Arc<ReplayCore>,
    fx: Fx,
    live: Cell<i64>,
    served_acts: i64,
    served_after: i64,
    served_future: i64,
}

impl RealCursor {
    fn new(acts: bool) -> Self {
        let core = if acts {
            ReplayCore::start_acts(cursor_tape(), Vec::new(), VERBATIM, VERBATIM, "test", None)
        } else {
            ReplayCore::start(cursor_tape(), VERBATIM, VERBATIM, "test", None)
        };
        Self {
            fx: Fx::replay(Arc::clone(&core)),
            core,
            live: Cell::new(0),
            served_acts: 0,
            served_after: 0,
            served_future: 0,
        }
    }
}

impl Cursor for RealCursor {
    fn ask(&mut self, words: &[&str]) -> bool {
        let was_diverged = self.seen().diverged;
        let closing = closing_at(self.core.consumed().0);
        let live = &self.live;
        let fx = &self.fx;
        let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fx.exchange(
                doors::CTL,
                || ctl_request(words),
                || {
                    live.set(live.get() + 1);
                    ok_reply("from the world")
                },
            )
        }));
        let Ok(reply) = served else {
            return false;
        };
        self.served_acts += i64::from(is_act(words));
        self.served_after += i64::from(was_diverged);
        let from = reply.as_ref().ok().and_then(record_of);
        self.served_future += i64::from(from.is_some_and(|r| r > closing));
        true
    }

    fn seen(&self) -> CursorSeen {
        let (pos, _) = self.core.consumed();
        let consumed_acts = TAPE_WORDS[..pos].iter().filter(|w| is_act(w)).count();
        CursorSeen {
            pos: i64::try_from(pos).unwrap_or(i64::MAX),
            diverged: self.core.diverged().is_some(),
            live: self.live.get(),
            skipped_act: i64::try_from(consumed_acts).unwrap_or(0) - self.served_acts,
            served_after: self.served_after,
            served_future: self.served_future,
        }
    }
}

/// THE STRICT ARM'S NEGATIVE CONTROL: a cursor that resyncs a request it
/// does not hold to the next record that holds it, across whatever lies
/// between — an act included.
#[derive(Default)]
struct LenientCursor {
    pos: usize,
    diverged: bool,
    skipped_act: i64,
}

impl Cursor for LenientCursor {
    fn ask(&mut self, words: &[&str]) -> bool {
        if self.diverged {
            return false;
        }
        match (self.pos..TAPE_WORDS.len()).find(|&p| TAPE_WORDS[p] == words) {
            Some(found) => {
                self.skipped_act += i64::try_from(
                    TAPE_WORDS[self.pos..found]
                        .iter()
                        .filter(|w| is_act(w))
                        .count(),
                )
                .unwrap_or(0);
                self.pos = found + 1;
                true
            }
            None => {
                self.diverged = true;
                false
            }
        }
    }

    fn seen(&self) -> CursorSeen {
        CursorSeen {
            pos: i64::try_from(self.pos).unwrap_or(i64::MAX),
            diverged: self.diverged,
            skipped_act: self.skipped_act,
            ..CursorSeen::default()
        }
    }
}

/// THE ACTS ARM'S NEGATIVE CONTROL: the acts cursor with no segment bound —
/// a read is served again from its segment and an act closes it, but a read
/// the segment does not hold is resynced to wherever ahead the tape holds
/// it, past an act included.
#[derive(Default)]
struct UnboundedActsCursor {
    pos: usize,
    seg_start: usize,
    diverged: bool,
    skipped_act: i64,
}

impl Cursor for UnboundedActsCursor {
    fn ask(&mut self, words: &[&str]) -> bool {
        if self.diverged {
            return false;
        }
        let acts = |range: std::ops::Range<usize>| {
            i64::try_from(TAPE_WORDS[range].iter().filter(|w| is_act(w)).count()).unwrap_or(0)
        };
        if is_act(words) {
            let closing = closing_at(self.pos);
            if closing < TAPE_WORDS.len() && TAPE_WORDS[closing] == words {
                self.pos = closing + 1;
                self.seg_start = self.pos;
                return true;
            }
            self.diverged = true;
            return false;
        }
        if let Some(found) = (self.pos..TAPE_WORDS.len()).find(|&p| TAPE_WORDS[p] == words) {
            self.skipped_act += acts(self.pos..found);
            self.pos = found + 1;
            if self.skipped_act > 0 {
                self.seg_start = self.pos;
            }
            return true;
        }
        if (self.seg_start..self.pos).any(|p| TAPE_WORDS[p] == words) {
            return true;
        }
        self.diverged = true;
        false
    }

    fn seen(&self) -> CursorSeen {
        CursorSeen {
            pos: i64::try_from(self.pos).unwrap_or(i64::MAX),
            diverged: self.diverged,
            skipped_act: self.skipped_act,
            ..CursorSeen::default()
        }
    }
}

fn cursor_schedule(
    model: &Model,
    moves: &[&'static str],
    rng: &mut XorShift,
    len: usize,
) -> Vec<&'static str> {
    let mut state = model.init_state();
    let mut out = Vec::new();
    while out.len() < len {
        let enabled: Vec<&'static str> = moves
            .iter()
            .copied()
            .filter(|m| model.action_enabled(m, &state))
            .collect();
        let Some(&pick) = enabled.get(rng.below(enabled.len().max(1) as u64) as usize) else {
            break;
        };
        assert!(model.fire(pick, &mut state));
        out.push(pick);
    }
    out
}

fn bind_cursor(
    model: &Model,
    schedule: &[&'static str],
    cursor: &mut dyn Cursor,
    mut ev: Option<&mut StepEvidence>,
) -> Option<(usize, State, State)> {
    let mut state = model.init_state();
    for (i, action) in schedule.iter().enumerate() {
        let words = ask_for(action, state["pos"]);
        match ev.as_deref_mut() {
            Some(ev) => ev.step(action, || cursor.ask(&words)),
            None => cursor.ask(&words),
        };
        assert!(
            model.fire(action, &mut state),
            "{action} after {schedule:?}"
        );
        let mut real = state.clone();
        real.extend(project_cursor(&cursor.seen()));
        if real != state {
            return Some((i, state, real));
        }
    }
    None
}

fn cursor_machine(acts: i64, buggy: i64) -> Model {
    interp::with_consts(
        &supervise_replay_cursor_model(),
        &[("Acts", acts), ("Buggy", buggy)],
    )
}

fn conforms(acts: bool) {
    let model = cursor_machine(i64::from(acts), 0);
    let mut ev = StepEvidence::new(CURSOR);
    let mut rng = XorShift::new(61);
    let whole: Vec<&'static str> = ["AskNext"; 4]
        .into_iter()
        .chain(["AskPastEnd", "AskAfterDivergence"])
        .collect();
    let drawn = (0..200).map(|_| cursor_schedule(&model, &CURSOR_MOVES, &mut rng, 8));
    for schedule in std::iter::once(whole).chain(drawn) {
        let mut real = RealCursor::new(acts);
        let wrong = bind_cursor(&model, &schedule, &mut real, Some(&mut ev));
        assert!(wrong.is_none(), "acts={acts} {schedule:?}: {wrong:?}");
        assert_eq!(real.live.get(), 0, "no live call, ever");
    }
    let audit = ev.audit();
    let mut moves = CURSOR_MOVES.to_vec();
    moves.sort_unstable();
    assert_eq!(audit.actions.iter().copied().collect::<Vec<_>>(), moves);
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

#[test]
fn the_real_replay_cursor_conforms_to_the_model() {
    conforms(false);
}

#[test]
fn the_real_acts_cursor_conforms_to_the_model() {
    conforms(true);
}

fn rejected_for_passing_an_act<C: Cursor>(acts: i64, moves: &[&'static str], make: impl Fn() -> C) {
    let model = cursor_machine(acts, 0);
    let lenient_model = cursor_machine(acts, 2);
    let mut rng = XorShift::new(61);
    let mut rejected = 0;
    for _ in 0..200 {
        let schedule = cursor_schedule(&model, moves, &mut rng, 8);
        if let Some((_, _, real)) = bind_cursor(&model, &schedule, &mut make(), None) {
            rejected += 1;
            assert!(
                !model.check_invariant("NoActSkipped", &real),
                "acts={acts}: rejected for passing an act: {real:?}"
            );
        }
    }
    assert!(
        rejected > 0,
        "acts={acts}: the bind rejects a cursor that resyncs across acts"
    );
    for schedule in [["AskOtherRead"].as_slice(), &["AskNext", "AskOtherRead"]] {
        let mut cursor = make();
        let mut state = lenient_model.init_state();
        for action in schedule {
            cursor.ask(&ask_for(action, state["pos"]));
            assert!(lenient_model.fire(action, &mut state));
        }
        let mut real = state.clone();
        real.extend(project_cursor(&cursor.seen()));
        assert_eq!(real, state, "acts={acts}: {schedule:?}");
        assert_eq!(state["skipped_act"], 1);
    }
}

#[test]
fn a_cursor_that_resyncs_across_an_act_is_rejected() {
    rejected_for_passing_an_act(0, &STRICT_MOVES, LenientCursor::default);
}

#[test]
fn an_acts_cursor_unbounded_by_its_segment_is_rejected() {
    rejected_for_passing_an_act(1, &CURSOR_MOVES, UnboundedActsCursor::default);
}

const LATCH: &str = "SupervisorRaisedLatch";

/// What a recorded loop and its replay did, as the model's variables see them.
#[derive(Debug, Clone)]
pub(crate) struct LatchSeen {
    marked: bool,
    mark: i64,
    diverged: bool,
}

/// The variables a latch run decides.
pub(crate) fn project_latch(seen: &LatchSeen) -> BTreeMap<&'static str, i64> {
    BTreeMap::from([
        ("marked", i64::from(seen.marked)),
        ("mark", seen.mark),
        ("diverged", i64::from(seen.diverged)),
    ])
}

struct Answers;

impl Ctl for Answers {
    fn call(&mut self, _: &[&str]) -> Result<CtlReply, String> {
        ok_reply("seq=1")
    }
}

fn door_words(op: usize) -> Vec<String> {
    vec![
        "await".to_string(),
        "seq".to_string(),
        op.to_string(),
        "timeout".to_string(),
        "20000".to_string(),
    ]
}

fn latch_run(
    raise_at: Option<usize>,
    setter_tapes: bool,
    mut ev: Option<&mut StepEvidence>,
) -> (LatchSeen, Vec<bool>, Vec<bool>) {
    use super::core::TapeCore;
    use super::ctl::{ReplayCtl, TapedCtl};
    use crate::supervise::fx::ClockSource;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    let mut step = |action: &'static str, f: &mut dyn FnMut()| match ev.as_deref_mut() {
        Some(ev) => ev.step(action, f),
        None => f(),
    };
    let sink = Memory::default();
    let writer = TapeWriter::start(
        Box::new(sink.clone()),
        "latch.atape",
        HEADER,
        Box::new(Said::default()),
    );
    let clock = Fx::over(ClockSource::Scripted(Arc::default()));
    let (origin, _) = clock.peek();
    let core = TapeCore::start(writer, VERBATIM, origin, "test");
    let fx = clock.recording(Arc::clone(&core));
    let flag = Arc::new(AtomicBool::new(false));
    let (tell, told) = mpsc::channel::<()>();
    let (done, raised) = mpsc::channel::<()>();
    let host = {
        let flag = Arc::clone(&flag);
        let core = Arc::clone(&core);
        std::thread::spawn(move || {
            if told.recv().is_ok() {
                flag.store(true, Ordering::SeqCst);
                if setter_tapes {
                    core.raised(doors::RAISED_STOP, true, Default::default);
                }
                let _ = done.send(());
            }
        })
    };
    let mut answers = Answers;
    let mut taped = TapedCtl::new(&mut answers, fx.clone(), doors::CTL);
    let mut mark = None;
    let mut reads = Vec::new();
    for op in 0..=4 {
        if raise_at == Some(op) {
            let before = core.effects();
            tell.send(()).expect("the host thread");
            raised
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the host thread raises within the hang detector");
            if core.effects() > before {
                mark.get_or_insert(op);
            }
        }
        if op == 4 {
            break;
        }
        let before = core.effects();
        step("LoopOp", &mut || {
            if op % 2 == 0 {
                reads.push(fx.raised(doors::RAISED_STOP, || flag.load(Ordering::SeqCst)));
            } else {
                let words = door_words(op);
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                let _ = taped.call(&words);
            }
        });
        if op % 2 == 0 && core.effects() > before {
            mark.get_or_insert(op);
        }
    }
    drop(tell);
    host.join().expect("the host thread");
    step("EndRecord", &mut || {
        fx.finish(super::codec::End::Returned {
            how: "done".to_string(),
        });
    });
    let read = recover(&sink.bytes());
    assert_eq!(read.tail, Tail::Clean);
    let records: Vec<EffectRecord> = read
        .items
        .iter()
        .filter_map(|i| i.record().cloned())
        .collect();
    let replay_core = ReplayCore::start(records, VERBATIM, VERBATIM, "test", None);
    let rfx = Fx::replay(Arc::clone(&replay_core));
    let mut lane = ReplayCtl::new(rfx.clone(), doors::CTL);
    let mut replayed = Vec::new();
    let mut diverged = false;
    for op in 0..4 {
        if diverged {
            break;
        }
        let mut went = true;
        step("ReplayOp", &mut || {
            went = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if op % 2 == 0 {
                    replayed.push(rfx.raised(doors::RAISED_STOP, || false));
                } else {
                    let words = door_words(op);
                    let words: Vec<&str> = words.iter().map(String::as_str).collect();
                    let _ = lane.call(&words);
                }
            }))
            .is_ok();
        });
        diverged = !went;
    }
    if !diverged {
        let mut left = 0;
        step("ReplayEnd", &mut || left = replay_core.left());
        diverged = left > 0;
    }
    let seen = LatchSeen {
        marked: mark.is_some(),
        mark: mark.map_or(0, |m| i64::try_from(m).unwrap_or(0)),
        diverged,
    };
    (seen, reads, replayed)
}

fn latch_schedule(model: &Model, raise_at: Option<usize>) -> State {
    let mut state = model.init_state();
    let fire = |action: &str, state: &mut State| {
        assert!(model.fire(action, state), "{action}: {state:?}");
    };
    for op in 0..=4 {
        if raise_at == Some(op) {
            fire("Raise", &mut state);
        }
        if op < 4 {
            fire("LoopOp", &mut state);
        }
    }
    fire("EndRecord", &mut state);
    for _ in 0..4 {
        if state["diverged"] == 1 {
            return state;
        }
        fire("ReplayOp", &mut state);
    }
    if state["diverged"] == 0 {
        fire("ReplayEnd", &mut state);
    }
    state
}

#[test]
fn the_real_latch_conforms_to_the_model() {
    let model = interp::with_buggy(&supervisor_raised_latch_model(), 0);
    let mut ev = StepEvidence::new(LATCH);
    for raise_at in [None, Some(0), Some(1), Some(2), Some(3), Some(4)] {
        let (seen, reads, replayed) = latch_run(raise_at, false, Some(&mut ev));
        let expected = latch_schedule(&model, raise_at);
        let mut real = expected.clone();
        real.extend(project_latch(&seen));
        assert_eq!(real, expected, "raised before {raise_at:?}");
        assert_eq!(replayed, reads, "each replayed read answers as recorded");
    }
    let audit = ev.audit();
    assert_eq!(
        audit.actions.iter().copied().collect::<Vec<_>>(),
        ["EndRecord", "LoopOp", "ReplayEnd", "ReplayOp"]
    );
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

#[test]
fn a_setter_that_tapes_the_flag_itself_is_rejected() {
    let model = interp::with_buggy(&supervisor_raised_latch_model(), 0);
    let setter_model = interp::with_buggy(&supervisor_raised_latch_model(), 1);
    let mut rejected = 0;
    for raise_at in [Some(0), Some(1), Some(2), Some(3), Some(4)] {
        let (seen, _, _) = latch_run(raise_at, true, None);
        let committed = latch_schedule(&model, raise_at);
        let mut real = committed.clone();
        real.extend(project_latch(&seen));
        if real != committed {
            rejected += 1;
        }
        let predicted = latch_schedule(&setter_model, raise_at);
        let mut real = predicted.clone();
        real.extend(project_latch(&seen));
        assert_eq!(
            real, predicted,
            "the setter's machine, raised before {raise_at:?}"
        );
    }
    assert!(
        rejected > 0,
        "the bind rejects a setter that tapes the flag"
    );
    let (seen, _, _) = latch_run(Some(1), true, None);
    assert!(
        seen.diverged,
        "raised before a door, the replay diverges there"
    );
}

#[test]
fn a_flag_raised_at_any_moment_replays_exactly() {
    use super::core::TapeCore;
    use super::ctl::{ReplayCtl, TapedCtl};
    use crate::supervise::fx::ClockSource;
    use std::sync::atomic::{AtomicBool, Ordering};

    const OPS: usize = 40;
    for seed in 1..=40u64 {
        let mut rng = XorShift::new(seed);
        let spins = rng.below(400);
        let sink = Memory::default();
        let writer = TapeWriter::start(
            Box::new(sink.clone()),
            "race.atape",
            HEADER,
            Box::new(Said::default()),
        );
        let clock = Fx::over(ClockSource::Scripted(Arc::default()));
        let (origin, _) = clock.peek();
        let fx = clock.recording(TapeCore::start(writer, VERBATIM, origin, "test"));
        let flag = Arc::new(AtomicBool::new(false));
        let host = {
            let flag = Arc::clone(&flag);
            std::thread::spawn(move || {
                for _ in 0..spins {
                    std::thread::yield_now();
                }
                flag.store(true, Ordering::SeqCst);
            })
        };
        let mut answers = Answers;
        let mut taped = TapedCtl::new(&mut answers, fx.clone(), doors::CTL);
        let mut reads = Vec::new();
        for op in 0..OPS {
            if op % 2 == 0 {
                reads.push(fx.raised(doors::RAISED_STOP, || flag.load(Ordering::SeqCst)));
            } else {
                let words = door_words(op);
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                let _ = taped.call(&words);
            }
        }
        host.join().expect("the host thread");
        fx.finish(super::codec::End::Returned {
            how: "done".to_string(),
        });
        let read = recover(&sink.bytes());
        let records: Vec<EffectRecord> = read
            .items
            .iter()
            .filter_map(|i| i.record().cloned())
            .collect();
        let core = ReplayCore::start(records, VERBATIM, VERBATIM, "test", None);
        let rfx = Fx::replay(Arc::clone(&core));
        let mut lane = ReplayCtl::new(rfx.clone(), doors::CTL);
        let replayed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut replayed = Vec::new();
            for op in 0..OPS {
                if op % 2 == 0 {
                    replayed.push(rfx.raised(doors::RAISED_STOP, || false));
                } else {
                    let words = door_words(op);
                    let words: Vec<&str> = words.iter().map(String::as_str).collect();
                    let _ = lane.call(&words);
                }
            }
            replayed
        }))
        .unwrap_or_else(|_| panic!("seed {seed}: the replay diverged: {core:?}"));
        assert_eq!(replayed, reads, "seed {seed}");
        assert_eq!(core.left(), 0, "seed {seed}: nothing left on the tape");
    }
}

#[test]
fn a_host_racing_the_loop_replays_exactly() {
    use super::core::TapeCore;
    use super::ctl::{ReplayCtl, TapedCtl};
    use crate::supervise::IdleHost;
    use crate::supervise::fx::ClockSource;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A window's host whose wish for a point another thread turns.
    #[derive(Debug, Default)]
    struct Racing {
        wants: AtomicBool,
    }

    impl IdleHost for Racing {
        fn wants(&self) -> bool {
            self.wants.load(Ordering::SeqCst)
        }
        fn at_idle(&self) -> Option<crate::supervise::HostStep> {
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
    }

    const OPS: usize = 60;
    for seed in 1..=40u64 {
        let mut rng = XorShift::new(seed);
        let (stop_at, turn_at) = (rng.below(400), rng.below(400));
        let sink = Memory::default();
        let writer = TapeWriter::start(
            Box::new(sink.clone()),
            "race-host.atape",
            HEADER,
            Box::new(Said::default()),
        );
        let clock = Fx::over(ClockSource::Scripted(Arc::default()));
        let (origin, _) = clock.peek();
        let fx = clock.recording(TapeCore::start(writer, VERBATIM, origin, "test"));
        let flag = Arc::new(AtomicBool::new(false));
        let racing = Arc::new(Racing::default());
        let host = fx.host_door(&(Arc::clone(&racing) as Arc<dyn IdleHost>));
        let thread = {
            let (flag, racing) = (Arc::clone(&flag), Arc::clone(&racing));
            std::thread::spawn(move || {
                for spin in 0..stop_at.max(turn_at) + 1 {
                    if spin == turn_at {
                        racing.wants.store(true, Ordering::SeqCst);
                    }
                    if spin == stop_at {
                        flag.store(true, Ordering::SeqCst);
                    }
                    std::thread::yield_now();
                }
            })
        };
        let mut answers = Answers;
        let mut taped = TapedCtl::new(&mut answers, fx.clone(), doors::CTL);
        let mut reads = Vec::new();
        for op in 0..OPS {
            match op % 3 {
                0 => reads.push(fx.raised(doors::RAISED_STOP, || flag.load(Ordering::SeqCst))),
                1 => reads.push(host.wants()),
                _ => {
                    let words = door_words(op);
                    let words: Vec<&str> = words.iter().map(String::as_str).collect();
                    let _ = taped.call(&words);
                }
            }
        }
        thread.join().expect("the host's thread");
        fx.finish(super::codec::End::Returned {
            how: "done".to_string(),
        });
        let read = recover(&sink.bytes());
        let records: Vec<EffectRecord> = read
            .items
            .iter()
            .filter_map(|i| i.record().cloned())
            .collect();
        let core = ReplayCore::start(records, VERBATIM, VERBATIM, "test", None);
        let rfx = Fx::replay(Arc::clone(&core));
        let rhost = rfx.host_door(&(Arc::new(Racing::default()) as Arc<dyn IdleHost>));
        let mut lane = ReplayCtl::new(rfx.clone(), doors::CTL);
        let replayed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut replayed = Vec::new();
            for op in 0..OPS {
                match op % 3 {
                    0 => replayed.push(rfx.raised(doors::RAISED_STOP, || false)),
                    1 => replayed.push(rhost.wants()),
                    _ => {
                        let words = door_words(op);
                        let words: Vec<&str> = words.iter().map(String::as_str).collect();
                        let _ = lane.call(&words);
                    }
                }
            }
            replayed
        }))
        .unwrap_or_else(|_| panic!("seed {seed}: the replay diverged: {core:?}"));
        assert_eq!(replayed, reads, "seed {seed}");
        assert_eq!(core.left(), 0, "seed {seed}: nothing left on the tape");
    }
}
