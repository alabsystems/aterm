// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `WakeRoom` (`aterm_models_harness::wake_room_model`) to the pace's
//! reservation table: [`Pace::ask`] (a slot line, from either pool) and [`Pace::reserve`] (an
//! `await wake` answer) reserve and refuse, and [`Pace::retire`] lapses a reservation whose tab
//! was seen holding a slot. A holder's re-ask must keep its reservation as taken and draw no
//! line.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_models_harness::wake_room_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, StepEvidence};

use super::fault::{self, Fault};
use super::{Ask, Pace, PaceGrant, Pool, Reserve};

type State = BTreeMap<&'static str, i64>;

const T0: i64 = 1_792_656_000_000;

/// The clock between two steps, ms.
const STEP_MS: i64 = 1_000;

/// One of the model's sessions: its variables' prefix, its actions' suffix, and its hold's start.
struct Session {
    var: &'static str,
    action: &'static str,
    since_ms: i64,
}

/// The sessions, oldest hold first.
const SESSIONS: [Session; 3] = [
    Session {
        var: "a",
        action: "A",
        since_ms: T0 - 3 * 60 * 60_000,
    },
    Session {
        var: "b",
        action: "B",
        since_ms: T0 - 2 * 60 * 60_000,
    },
    Session {
        var: "c",
        action: "C",
        since_ms: T0 - 60 * 60_000,
    },
];

/// The steps the table performs, each on its anchored function.
const ANCHORED: [&str; 9] = [
    "LapseA", "LapseB", "LapseC", "RefuseA", "RefuseB", "RefuseC", "ReserveA", "ReserveB",
    "ReserveC",
];

/// The loop's and the world's steps, which the table only sees.
const ENVIRONMENT: [&str; 11] = [
    "BuildTakes",
    "ReadyA",
    "ReadyB",
    "ReadyC",
    "SkipA",
    "SkipB",
    "SkipC",
    "SlotFrees",
    "WriteA",
    "WriteB",
    "WriteC",
];

/// The model's mutants.
const MUTANTS: [&str; 5] = [
    "CountOwn",
    "HerdTell",
    "RenewOnAsk",
    "RenewWhileWaited",
    "YoungestFirst",
];

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
pub(super) struct Real {
    now: i64,
    pace: Pace,
    /// The build slots the board proves usable.
    usable: u16,
}

fn var(s: &Session, name: &str) -> String {
    format!("{}_{name}", s.var)
}

/// The model's reservations, waits and usable slots as the table says them.
pub(super) fn project(r: &Real) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::from([(String::from("usable"), i64::from(r.usable))]);
    for s in &SESSIONS {
        let res = r.pace.reserved.iter().any(|x| x.sid == s.var);
        let wait = r.pace.waiting.iter().any(|x| x.sid == s.var);
        out.insert(var(s, "res"), i64::from(res));
        out.insert(var(s, "wait"), i64::from(wait));
    }
    out
}

fn agrees(st: &State, r: &Real) -> bool {
    project(r).iter().all(|(name, v)| st[name.as_str()] == *v)
}

/// Which call stands for a session's ask (cycled): a slot line of the facts pool, one of the
/// wake pool, or an `await wake` answer's reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asker {
    Facts,
    Wake,
    Answer,
}

#[derive(Clone, Copy)]
struct Run {
    asker: Asker,
    fault: Option<Fault>,
}

/// The evidence windows of an audited walk, or none.
struct Ev(Option<StepEvidence>);

impl Ev {
    fn step<T>(&mut self, action: &'static str, drive: impl FnOnce() -> T) -> T {
        match &mut self.0 {
            Some(ev) => ev.step(action, drive),
            None => drive(),
        }
    }
}

/// The session an action of the table's is about, and whether it reserves (else refuses).
fn of(action: &str) -> Option<(&'static Session, &'static str)> {
    let (verb, who) = action.split_at(action.len().checked_sub(1)?);
    let session = SESSIONS.iter().find(|s| s.action == who)?;
    match verb {
        "Reserve" => Some((session, "Reserve")),
        "Refuse" => Some((session, "Refuse")),
        "Lapse" => Some((session, "Lapse")),
        _ => None,
    }
}

/// The real step for `action` at `st`; false where the table answers otherwise than the step
/// says.
fn mirror(action: &'static str, st: &State, r: &mut Real, run: Run, ev: &mut Ev) -> bool {
    r.now += STEP_MS;
    let now = r.now;
    match (action, of(action)) {
        ("BuildTakes", _) => r.usable = 0,
        ("SlotFrees", _) => r.usable = 1,
        (_, Some((s, "Lapse"))) => {
            let held = BTreeMap::from([(s.var.to_string(), now)]);
            ev.step(action, || r.pace.retire(&held));
        }
        (_, Some((s, verb))) => {
            let holder = st[var(s, "res").as_str()] == 1;
            let kept = r.pace.reserved.clone();
            let lines = (r.pace.lines.len(), r.pace.wake_lines.len());
            let usable = r.usable;
            let pool = match run.asker {
                Asker::Wake => Pool::Wake,
                Asker::Facts | Asker::Answer => Pool::Facts,
            };
            let granted = match run.asker {
                Asker::Answer => {
                    let reserve =
                        ev.step(action, || r.pace.reserve(s.var, s.since_ms, usable, now));
                    reserve == Reserve::Granted
                }
                Asker::Facts | Asker::Wake => {
                    let ask = Ask {
                        sid: s.var,
                        slot: true,
                        hold_since_ms: s.since_ms,
                        usable,
                        pool,
                    };
                    match ev.step(action, || r.pace.ask(&ask, now)) {
                        PaceGrant::Granted => true,
                        PaceGrant::Reserved => false,
                        _ => return false,
                    }
                }
            };
            if granted != (verb == "Reserve") {
                return false;
            }
            let drawn = (r.pace.lines.len(), r.pace.wake_lines.len());
            let want = match (holder || !granted, run.asker) {
                (true, _) | (false, Asker::Answer) => lines,
                (false, Asker::Facts) => (lines.0 + 1, lines.1),
                (false, Asker::Wake) => (lines.0, lines.1 + 1),
            };
            if drawn != want || (holder && r.pace.reserved != kept) {
                return false;
            }
        }
        _ => {}
    }
    true
}

struct Walked {
    states: usize,
    off: Vec<State>,
    fired: BTreeSet<&'static str>,
}

fn walk(m: &Model, strict: bool, run: Run, ev: &mut Ev) -> Walked {
    let mut go = || {
        let mut w = Walked {
            states: 0,
            off: Vec::new(),
            fired: BTreeSet::new(),
        };
        let init = Real {
            now: T0,
            pace: Pace::default(),
            usable: 1,
        };
        assert!(
            agrees(&m.init_state(), &init),
            "the empty table is the model's start"
        );
        let mut seen: HashSet<Vec<i64>> = HashSet::new();
        let mut queue = VecDeque::from([(m.init_state(), init)]);
        while let Some((st, real)) = queue.pop_front() {
            if !seen.insert(st.values().copied().collect()) {
                continue;
            }
            let env = m.eval_env(&st);
            for action in &m.actions {
                let Some(next) = m.successors_in(action, &env, &st).into_iter().next() else {
                    continue;
                };
                let mut r = real.clone();
                let stepped = mirror(action.name, &st, &mut r, run, ev);
                w.fired.insert(action.name);
                if stepped && agrees(&next, &r) {
                    queue.push_back((next, r));
                    continue;
                }
                assert!(
                    !strict,
                    "the table disagrees after {} ({:?}) from {st:?}: the model's {next:?}, \
                     the table's {:?}",
                    action.name,
                    run.asker,
                    project(&r),
                );
                w.off.push(next);
            }
        }
        w.states = seen.len();
        w
    };
    match run.fault {
        Some(f) => fault::with(f, go),
        None => go(),
    }
}

const ASKERS: [Asker; 3] = [Asker::Facts, Asker::Wake, Asker::Answer];

#[test]
fn tier1_the_table_reserves_exactly_where_the_model_does() {
    let m = wake_room_model();
    let runs: Vec<Run> = ASKERS
        .into_iter()
        .map(|asker| Run { asker, fault: None })
        .collect();
    let walked = aterm_spec::check::each(&runs, |&run| walk(&m, true, run, &mut Ev(None)));
    let reachable = interp::reachable_states(&m).len();
    let want: BTreeSet<&str> = ANCHORED.into_iter().chain(ENVIRONMENT).collect();
    for (run, w) in runs.iter().zip(walked) {
        assert_eq!(w.fired, want, "{:?}: the steps mirrored", run.asker);
        assert_eq!(w.states, reachable, "{:?}: every state walked", run.asker);
    }
}

type Shows = fn(&State) -> bool;

/// A session whose re-ask the model grants against its own reservation.
fn holder_reasked(st: &State) -> bool {
    ["a", "b", "c"]
        .iter()
        .any(|x| st[format!("{x}_res").as_str()] == 1 && st[format!("{x}_grant").as_str()] == 1)
}

#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Shows); 3] = [
        // The holder's reservation is renewed from its re-ask.
        (Fault::RenewOnAsk, holder_reasked),
        // The holder is refused for its own reservation.
        (Fault::CountOwn, holder_reasked),
        // A younger hold takes the slot an older one waits for.
        (Fault::YoungestFirst, |st| {
            (st["b_wait"] == 1 && st["a_wait"] == 1)
                || (st["c_wait"] == 1 && st["a_wait"] + st["b_wait"] > 0)
        }),
    ];
    let m = wake_room_model();
    let runs: Vec<(Run, Shows)> = controls
        .into_iter()
        .flat_map(|(fault, shows)| {
            ASKERS.into_iter().map(move |asker| {
                (
                    Run {
                        asker,
                        fault: Some(fault),
                    },
                    shows,
                )
            })
        })
        .collect();
    let walked = aterm_spec::check::each(&runs, |&(run, _)| walk(&m, false, run, &mut Ev(None)));
    for ((run, shows), w) in runs.iter().zip(walked) {
        let what = format!("{:?} ({:?})", run.fault, run.asker);
        assert!(!w.off.is_empty(), "{what} agreed everywhere");
        assert!(w.off.iter().all(shows), "{what}: {:?}", w.off);
    }
}

#[test]
fn tier1_every_anchored_step_enters_its_own_function() {
    let m = wake_room_model();
    let mut ev = Ev(Some(StepEvidence::new("WakeRoom")));
    for asker in ASKERS {
        let w = walk(&m, true, Run { asker, fault: None }, &mut ev);
        assert_eq!(w.states, interp::reachable_states(&m).len(), "{asker:?}");
    }
    let audit = ev.0.expect("the audited walk's evidence").audit();
    assert_eq!(
        audit.actions,
        ANCHORED.into_iter().collect::<BTreeSet<_>>(),
        "{audit:?}"
    );
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
    assert!(audit.dark_sites.is_empty(), "{audit:?}");
}

#[test]
fn tier1_the_anchors_name_what_the_code_implements() {
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == "WakeRoom")
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, ANCHORED.into_iter().collect(), "{anchored:?}");
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "WakeRoom")
        .map(|w| w.action)
        .collect();
    let want: BTreeSet<&str> = ENVIRONMENT.into_iter().chain(MUTANTS).collect();
    assert_eq!(waived, want, "{waived:?}");
    let actions: BTreeSet<&str> = wake_room_model().actions.iter().map(|a| a.name).collect();
    let bound: BTreeSet<&str> = anchored.union(&waived).copied().collect();
    assert_eq!(bound, actions, "every action anchored or waived");
}
