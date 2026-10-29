// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `WakeBook` (`aterm_models_harness::wake_book_model`) to a session's
//! book of declared wakes: [`WakeBook`]'s steps, [`ParkBook::park`], and [`prove`] for a peer
//! outside the tree. The model's one wake is the book's wake #1, and at every state the book is
//! probed on copies, so a claim is granted, an `await wake` answered and a park taken exactly
//! where the model says. The need, `unless=` at the tell, a heard turn and the loop's guards at a
//! claim are the model's environment, and so are a line a door left in the composer and what
//! the person or the policy does with it: the book keeps the claim (a successor too, from the
//! left row), and the look that settles the line tells it (`Write`) or gives it back (`Release`).

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_agent::supervise::facts::SeedOutcome;
use aterm_agent::supervise::occupancy::WakeDoor;
use aterm_agent::supervise::occupancy::declarer::{AgentId, ProcLink, prove};
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_agent::supervise::occupancy::wakes::{
    DeclaredWake, Levels, ParkBook, ParkRefusal, Reconciled, SourceAtom, SysWait, UNHEARD_MS,
    WakeBook, WakeReq,
};
use aterm_agent::supervise::occupancy::wakes_seen::{Declarer, WakeState};
use aterm_models_harness::wake_book_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, StepEvidence};

type State = BTreeMap<&'static str, i64>;

const T0: i64 = 1_000_000_000;

/// The loop's clock between two steps, ms.
const STEP_MS: i64 = 1_000;

/// The tab's PTY child.
const PTY: AgentId = AgentId {
    pid: 500,
    birth_us: 1_000,
};

/// The agent it hosts.
const AGENT: AgentId = AgentId {
    pid: 501,
    birth_us: 2_000,
};

/// The agent's own `aterm ctl`, up to the tab.
const OWN: [ProcLink; 3] = [
    ProcLink {
        pid: 601,
        birth_us: 3_000,
        ppid: 501,
    },
    ProcLink {
        pid: 501,
        birth_us: 2_000,
        ppid: 500,
    },
    ProcLink {
        pid: 500,
        birth_us: 1_000,
        ppid: 1,
    },
];

/// Peers outside the tree: none (the bridge, a relay), another session's agent, and the agent's
/// own chain up to the tab's pid reused by another process (only the tab's birth tells it).
const FOREIGN: [&[ProcLink]; 3] = [
    &[],
    &[
        ProcLink {
            pid: 701,
            birth_us: 3_000,
            ppid: 700,
        },
        ProcLink {
            pid: 700,
            birth_us: 1_500,
            ppid: 1,
        },
    ],
    &[
        ProcLink {
            pid: 601,
            birth_us: 3_000,
            ppid: 501,
        },
        ProcLink {
            pid: 501,
            birth_us: 2_000,
            ppid: 500,
        },
        ProcLink {
            pid: 500,
            birth_us: 500,
            ppid: 1,
        },
    ],
];

/// The model's mutants, which no real step mirrors.
const MUTANTS: [&str; 17] = [
    "ParkDeaf",
    "DeclareFromOutside",
    "AwaitFromAnyone",
    "TypeTwice",
    "AnswerThenType",
    "ParkSecond",
    "TellOnStaleNeed",
    "TellExpired",
    "TypeIntoManaged",
    "TypeUnderObserve",
    "LevelOnly",
    "ToldBeforeWrite",
    "ReleaseAfterTyped",
    "RetellAfterHeard",
    "AnswerTwice",
    "ReleaseOnLeft",
    "KeepOnClear",
];

/// The steps the code performs, each on its anchored function.
const ANCHORED: [&str; 21] = [
    "Answer",
    "Claim",
    "CrashMidWrite",
    "DeclareOther",
    "DeclareOwn",
    "Edge",
    "Expire",
    "ForeignAwait",
    "ForeignTake",
    "Heard",
    "KeepLeft",
    "Park",
    "Reconcile",
    "RefuseSecond",
    "Release",
    "Rm",
    "Take",
    "Unless",
    "Write",
    "WriteFails",
    "WriteOk",
];

/// The steps of the loop, the levels and the agent's turns, which the book only sees.
const ENVIRONMENT: [&str; 14] = [
    "Blip",
    "BrakeOff",
    "BrakeOn",
    "Break",
    "ClearLeft",
    "Fails",
    "Holds",
    "Leave",
    "NeedFails",
    "NeedHolds",
    "SubmitLeft",
    "TurnRuns",
    "UnlessHolds",
    "WriteLeft",
];

/// The steps only a loop's door under `prompt`, in a hosted session with no manager, takes.
const DOOR_STEPS: [&str; 9] = [
    "Claim",
    "Write",
    "Release",
    "Reconcile",
    "CrashMidWrite",
    "WriteLeft",
    "SubmitLeft",
    "ClearLeft",
    "KeepLeft",
];

/// The wake: a file to appear, slots to tell it, and a lock whose going ends it untold.
fn req() -> WakeReq {
    let text = "label=build when=file:~/scratch.noindex/run/done need=slots>=1 \
                unless=gone:~/scratch.noindex/run/lock";
    WakeReq::parse(text).unwrap_or_else(|why| panic!("{text}: {why:?}"))
}

/// The levels a look reads: the condition's file, the `unless=` atom, the agent alive.
#[derive(Clone, Copy)]
struct Lv {
    cond: bool,
    unl: bool,
    alive: bool,
}

impl Levels for Lv {
    fn source(&self, atom: &SourceAtom) -> Option<bool> {
        match atom {
            SourceAtom::File { .. } => Some(self.cond),
            SourceAtom::Gone { .. } => Some(self.unl),
            SourceAtom::Pid { .. } => None,
        }
    }

    fn sys(&self, _: &SysWait) -> Option<bool> {
        None
    }

    fn agent_alive(&self, _: AgentId) -> bool {
        self.alive
    }
}

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Real {
    /// The loop's clock, ms.
    now: i64,
    book: WakeBook,
    parks: ParkBook,
    lv: Lv,
    /// The ledger's fact rows, oldest first.
    rows: Vec<(u64, SeedOutcome)>,
    /// A wake was booked (gone from the book is `st = 7`, not none).
    declared: bool,
    /// The last connection that parked.
    conns: u64,
}

impl Real {
    fn new() -> Self {
        Self {
            now: T0,
            book: WakeBook::default(),
            parks: ParkBook::default(),
            lv: Lv {
                cond: false,
                unl: false,
                alive: true,
            },
            rows: Vec::new(),
            declared: false,
            conns: 0,
        }
    }

    fn episode(&self) -> u64 {
        self.book
            .get(1)
            .map(DeclaredWake::episode)
            .expect("the model's wake is booked")
    }
}

/// How a walk departs from the committed real code, for the negative controls.
#[derive(Clone, Copy)]
struct Run {
    /// Which real call stands for a step with more than one (cycled): an edge through a look or
    /// through its own event, an expiry by `by=` or by the agent gone, a break's door or a point's.
    variant: usize,
    /// The model's `Hosted`.
    hosted: bool,
    /// The defect planted, if any.
    fault: Option<Fault>,
}

impl Run {
    const fn door(self) -> WakeDoor {
        if self.variant.is_multiple_of(2) {
            WakeDoor::Break
        } else {
            WakeDoor::Point
        }
    }
}

/// The model's `st` and `parked` as the book and the parks say them; `None` for a book the model
/// has no state of (a second wake).
fn project(r: &Real) -> Option<State> {
    if r.book.wakes.len() > 1 {
        return None;
    }
    let st = match r.book.get(1).map(|w| w.state) {
        None if r.declared => 7,
        None => 0,
        Some(WakeState::Pending) => 1,
        Some(WakeState::Ready) => 2,
        Some(WakeState::Answering { .. }) => 3,
        Some(WakeState::Answered { .. }) => 4,
        Some(WakeState::Claimed { .. }) => 5,
        Some(WakeState::Told { .. }) => 6,
        Some(WakeState::Taken | WakeState::Expired { .. }) => 7,
    };
    let parked = i64::try_from(r.parks.parked().len()).ok()?;
    Some(State::from([("st", st), ("parked", parked)]))
}

/// The book agrees with the model at `st`: its projection, and what its probes on copies say (a
/// door's claim at an unheard answer's retell, an `await wake`'s answer, a park, an answer heard,
/// an answer begun).
fn agrees(st: &State, r: &Real) -> bool {
    let Some(real) = project(r) else {
        return false;
    };
    let (s, parked) = (st["st"], st["parked"]);
    let granted = r.book.get(1).map(DeclaredWake::episode).is_some_and(|ep| {
        let mut book = r.book.clone();
        book.claim(1, ep, WakeDoor::Break, r.now + UNHEARD_MS)
            .lets()
    });
    let answers = r.book.for_await().contains(&1);
    let parks = r.parks.clone().park(r.conns + 1, &r.book, r.now).is_ok();
    let heard = r.book.clone().heard(1, r.now);
    let begins = r.book.clone().answer_begin(1, r.now);
    real["st"] == s
        && real["parked"] == parked
        && granted == (s == 2 || s == 4)
        && answers == (s == 2)
        && parks == (parked == 0 && (s == 1 || s == 2))
        && heard == (s == 4)
        && begins == (s == 2)
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

/// The model's book holds one wake: a second declaration, the model's `DeclareOther` past its
/// first, is outside it.
fn realizable(st: &State, action: &str) -> bool {
    action != "DeclareOther" || st["st"] == 0
}

/// The real step for `action` at `st`; false where the code answers otherwise than the step
/// says. A step's own call runs in its evidence window, and nothing else does (the agent's own
/// proof before a declaration or a park is not the step's).
fn mirror(action: &'static str, st: &State, r: &mut Real, run: Run, ev: &mut Ev) -> bool {
    r.now += STEP_MS;
    let now = r.now;
    match action {
        "DeclareOwn" => {
            let agent = prove(&OWN, PTY, Some(AGENT)).expect("the agent's own peer is of its tree");
            r.declared = true;
            let ord = ev.step(action, || {
                r.book.declare(req(), Declarer::Agent, agent, now, 0)
            });
            ord == Ok(1)
        }
        "DeclareOther" | "ForeignAwait" | "ForeignTake" => {
            let admitted: Vec<AgentId> = ev.step(action, || {
                FOREIGN
                    .iter()
                    .filter_map(|chain| prove(chain, PTY, Some(AGENT)).ok())
                    .collect()
            });
            for agent in admitted {
                match action {
                    "DeclareOther" => {
                        r.declared = true;
                        let _booked = r.book.declare(req(), Declarer::Agent, agent, now, 0);
                    }
                    "ForeignAwait" => {
                        r.conns += 1;
                        let _parked = r.parks.park(r.conns, &r.book, now);
                    }
                    _ => {
                        let _took = r.book.taken(1);
                    }
                }
            }
            true
        }
        "Holds" => {
            r.lv.cond = true;
            true
        }
        "Fails" => {
            r.lv.cond = false;
            true
        }
        "UnlessHolds" => {
            r.lv.unl = true;
            true
        }
        "Blip" | "NeedHolds" | "NeedFails" | "Break" | "Leave" | "BrakeOn" | "BrakeOff"
        | "TurnRuns" | "SubmitLeft" | "ClearLeft" => true,
        // The door's write left its line: its left row, and no call of the book's.
        "WriteLeft" => {
            let ep = r.episode();
            r.rows.push((ep, SeedOutcome::Left));
            true
        }
        "Edge" => {
            let lv = r.lv;
            if st["blip"] == 1 || st["unl"] == 1 || !run.variant.is_multiple_of(2) {
                ev.step(action, || r.book.on_source(1, 0, now));
            } else {
                ev.step(action, || r.book.on_look(&lv, now));
            }
            true
        }
        "Expire" => {
            if run.variant.is_multiple_of(2) {
                let due = r.book.get(1).expect("the model's wake").due_by_ms;
                r.now = r.now.max(due);
            } else {
                r.lv.alive = false;
            }
            let (lv, now) = (r.lv, r.now);
            ev.step(action, || r.book.on_look(&lv, now));
            true
        }
        "Unless" => {
            let lv = r.lv;
            ev.step(action, || r.book.on_look(&lv, now));
            true
        }
        "Rm" => ev.step(action, || r.book.rm(1)),
        "Take" => ev.step(action, || r.book.taken(1)),
        "Park" | "RefuseSecond" => {
            let _agent = prove(&OWN, PTY, Some(AGENT)).expect("the agent's own peer");
            r.conns += 1;
            let conn = r.conns;
            let parked = ev.step(action, || r.parks.park(conn, &r.book, now));
            if action == "Park" {
                parked.is_ok()
            } else {
                parked == Err(ParkRefusal::Busy)
            }
        }
        "Answer" => ev.step(action, || r.book.answer_begin(1, now)),
        "WriteOk" | "WriteFails" => {
            let ok = action == "WriteOk";
            ev.step(action, || r.book.answer_done(1, ok, run.hosted, now));
            if !ok {
                return true;
            }
            let conn = r.parks.parked().first().map(|p| p.conn);
            conn.is_some_and(|conn| r.parks.unpark(conn))
        }
        "Heard" => ev.step(action, || r.book.heard(1, now)),
        "Claim" => {
            if st["st"] == 4 {
                let at = r
                    .book
                    .get(1)
                    .and_then(|w| w.answered_ms)
                    .expect("an answered wake");
                r.now = r.now.max(at + UNHEARD_MS);
            }
            let (ep, now, door) = (r.episode(), r.now, run.door());
            let claim = ev.step(action, || r.book.claim(1, ep, door, now));
            r.rows.push((ep, SeedOutcome::Intent));
            claim.lets()
        }
        "Write" => {
            let ep = r.episode();
            r.rows.push((ep, SeedOutcome::Typed));
            ev.step(action, || r.book.told(1, ep, run.door(), now))
        }
        "Release" => {
            let ep = r.episode();
            r.rows.push((ep, SeedOutcome::Skipped));
            ev.step(action, || r.book.release(1, ep, run.door()))
        }
        // The loop typed and booked its line, and ended before it told the book: a successor
        // reconciles.
        "Reconcile" => {
            let ep = r.episode();
            r.rows.push((ep, SeedOutcome::Typed));
            let rows = r.rows.clone();
            ev.step(action, || r.book.reconcile(&rows, now)).len() == 1
        }
        // The loop typed its line and ended before it booked it: its intent row is the last.
        "CrashMidWrite" => {
            let rows = r.rows.clone();
            ev.step(action, || r.book.reconcile(&rows, now)).len() == 1
        }
        // The loop left its line and ended before a look settled it: a successor keeps the claim.
        "KeepLeft" => {
            let rows = r.rows.clone();
            ev.step(action, || r.book.reconcile(&rows, now)) == [(1, Reconciled::Left)]
        }
        other => panic!("no real step for `{other}`"),
    }
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
        let init = Real::new();
        assert!(
            agrees(&m.init_state(), &init),
            "the empty book is the model's start"
        );
        let mut seen: HashSet<Vec<i64>> = HashSet::new();
        let mut queue = VecDeque::from([(m.init_state(), init)]);
        while let Some((st, real)) = queue.pop_front() {
            if !seen.insert(crate::common::key(m, &st)) {
                continue;
            }
            let env = m.eval_env(&st);
            for action in &m.actions {
                let Some(next) = m.successors_in(action, &env, &st).into_iter().next() else {
                    continue;
                };
                if !realizable(&st, action.name) {
                    continue;
                }
                let mut r = real.clone();
                let stepped = mirror(action.name, &st, &mut r, run, ev);
                w.fired.insert(action.name);
                if stepped && agrees(&next, &r) {
                    queue.push_back((next, r));
                    continue;
                }
                assert!(
                    !strict,
                    "the book disagrees after {} from {st:?}: the model's {next:?}, the book's \
                     {:?} ({:?})",
                    action.name,
                    project(&r),
                    r.book.get(1).map(|w| w.state),
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

/// The book under `(Prompt, Managed, Hosted)`.
fn under(prompt: i64, managed: i64, hosted: i64) -> Model {
    interp::with_consts(
        &wake_book_model(),
        &[("Prompt", prompt), ("Managed", managed), ("Hosted", hosted)],
    )
}

/// The committed book (`prompt`, no manager, hosted).
const COMMITTED: (i64, i64, i64) = (1, 0, 1);

/// Every configuration the model declares: committed, observe, a managed worker, unhosted.
const CONFIGS: [(i64, i64, i64); 4] = [COMMITTED, (0, 0, 1), (1, 1, 1), (1, 0, 0)];

fn walks(runs: &[((i64, i64, i64), Run)], strict: bool) -> Vec<Walked> {
    aterm_spec::check::each(runs, |&((prompt, managed, hosted), run)| {
        walk(&under(prompt, managed, hosted), strict, run, &mut Ev(None))
    })
}

#[test]
fn tier1_the_book_moves_exactly_where_the_model_does() {
    let runs: Vec<_> = CONFIGS
        .into_iter()
        .flat_map(|config| {
            (0..2).map(move |variant| {
                (
                    config,
                    Run {
                        variant,
                        hosted: config.2 == 1,
                        fault: None,
                    },
                )
            })
        })
        .collect();
    for (&((prompt, managed, hosted), Run { variant, .. }), w) in
        runs.iter().zip(walks(&runs, true))
    {
        let what = format!("Prompt = {prompt}, Managed = {managed}, Hosted = {hosted}, {variant}");
        let doors = (prompt, managed, hosted) == COMMITTED;
        let want: BTreeSet<&str> = ANCHORED
            .into_iter()
            .chain(ENVIRONMENT)
            .filter(|a| doors || !DOOR_STEPS.contains(a))
            .filter(|a| hosted == 1 || !["Heard", "TurnRuns"].contains(a))
            .collect();
        assert_eq!(w.fired, want, "{what}: the steps mirrored");
        let reachable = interp::reachable_states(&under(prompt, managed, hosted)).len();
        assert_eq!(
            w.states, reachable,
            "{what}: every state the model reaches is walked"
        );
    }
}

type Shows = fn(&State) -> bool;

#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Shows, Option<Shows>); 7] = [
        // An edge between two looks is lost: the model's edge lands, the book stays pending.
        (
            Fault::LevelOnly,
            |st| st["st"] == 2,
            Some(|st| st["cond"] == 0),
        ),
        // A told wake is claimed again: every told state's probe.
        (Fault::ClaimTwice, |st| st["st"] == 6, None),
        // A second park stands.
        (
            Fault::ParkSecond,
            |st| st["parked"] == 1,
            Some(|st| st["st"] == 1 || st["st"] == 2),
        ),
        // A failed reply is booked as an answer.
        (
            Fault::ToldBeforeWrite,
            |st| st["st"] == 2 && st["fails"] == 1,
            None,
        ),
        // A successor releases a claim whose line was typed.
        (Fault::ReleaseAfterTyped, |st| st["st"] == 6, None),
        // A successor gives back a claim whose line stands left: the model keeps it.
        (
            Fault::ReleaseOnLeft,
            |st| st["st"] == 5 && st["lk"] == 1,
            None,
        ),
        // A peer outside the tree declares, parks and takes.
        (
            Fault::AwaitFromAnyone,
            |st| st["st"] < 7,
            Some(|st| st["st"] == 0),
        ),
    ];
    let mut runs = Vec::new();
    let mut checks = Vec::new();
    for (fault, shows, among) in controls {
        for variant in 0..2 {
            runs.push((
                COMMITTED,
                Run {
                    variant,
                    hosted: true,
                    fault: Some(fault),
                },
            ));
            checks.push((fault, shows, among));
        }
    }
    for ((&(_, Run { variant, .. }), (fault, shows, among)), w) in
        runs.iter().zip(checks).zip(walks(&runs, false))
    {
        assert!(
            !w.off.is_empty(),
            "{fault:?} (variant {variant}) agreed everywhere"
        );
        assert!(
            w.off.iter().all(shows),
            "{fault:?} (variant {variant}): {:?}",
            w.off
        );
        if let Some(among) = among {
            assert!(
                w.off.iter().any(among),
                "{fault:?} (variant {variant}): {:?}",
                w.off
            );
        }
    }
}

#[test]
fn tier1_every_anchored_step_enters_its_own_function() {
    let m = under(COMMITTED.0, COMMITTED.1, COMMITTED.2);
    let mut ev = Ev(Some(StepEvidence::new("WakeBook")));
    for variant in 0..2 {
        let run = Run {
            variant,
            hosted: true,
            fault: None,
        };
        let w = walk(&m, true, run, &mut ev);
        assert_eq!(
            w.states,
            interp::reachable_states(&m).len(),
            "variant {variant}"
        );
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
        .filter(|r| r.machine == "WakeBook")
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, ANCHORED.into_iter().collect(), "{anchored:?}");
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "WakeBook")
        .map(|w| w.action)
        .collect();
    let want: BTreeSet<&str> = ENVIRONMENT.into_iter().chain(MUTANTS).collect();
    assert_eq!(waived, want, "{waived:?}");
    let actions: BTreeSet<&str> = wake_book_model().actions.iter().map(|a| a.name).collect();
    let bound: BTreeSet<&str> = anchored.union(&waived).copied().collect();
    assert_eq!(bound, actions, "every action anchored or waived");
}
