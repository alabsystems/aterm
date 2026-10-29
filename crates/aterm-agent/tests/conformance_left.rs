// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `LeftWrite` (`aterm_models_harness::left_write_model`) to the facts'
//! account of a line left in the composer (the slice-2 plan's 2.9b): [`FactsState::leave`], every
//! look's settle on [`FactsState::look`], the door's told line on [`FactsState::told`], and the
//! policy's draft step judged by [`FactsState::draft_may_go`]. The model's one episode is a
//! wake's line at a break, written under `prompt`; at every state the account is probed on
//! copies, so a look settles exactly where the model says, told or released, and the draft step
//! may send exactly where the model's `DraftSends` may. The session (its composer, its submit
//! count or its turns), the person and the owner's mode are the model's environment. Each walk
//! runs on a host that serves the submit count and on one that serves none, where a turn seen
//! running stands for the count risen, and makes the model's `Recount` each way the code meets
//! it: a successor reading the line back from its predecessor's row, the host back from an
//! outage, and (where a count is served) the host's count started again from 0. At every state a
//! second probe reads the composer unread with any turn running before it reads it as the model
//! has it, so a turn seen under a dialog is kept (`Left::ran`).

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_agent::supervise::facts::{
    CLEARED, EpisodeKey, FactKind, FactsState, GONE, Left, LeftLook, SUBMITTED, SeedOutcome,
    Settled, TURN, UNCOUNTED,
};
use aterm_agent::supervise::occupancy::WakeDoor;
use aterm_agent::supervise::occupancy::door::{LeftRow, LeftSaid, left_reason, left_rows_in};
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_models_harness::left_write_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, StepEvidence};

type State = BTreeMap<&'static str, i64>;

const T0: i64 = 1_000_000_000;

/// The loop's clock between two steps, ms.
const STEP_MS: i64 = 1_000;

/// The model's episode: a wake's.
const KEY: EpisodeKey = EpisodeKey {
    kind: FactKind::Wake,
    id: 0x0051,
};

/// The door that left it: a wake's line at a break, under `prompt`.
const ROW: LeftRow = LeftRow::Wake {
    ord: 1,
    held_ms: 0,
    door: WakeDoor::Break,
};

/// Its line.
const LINE: &str = "aterm: wake #1 (build) holds: the file is there.";

/// The model's mutants, which no real step mirrors.
const MUTANTS: [&str; 5] = [
    "KeepOnClear",
    "ReleaseOnLeft",
    "ReleaseUncounted",
    "SubmitWithoutPrompt",
    "ToldOnClear",
];

/// The steps the code performs, each on its anchored function.
const ANCHORED: [&str; 7] = [
    "DraftSends",
    "GoneReleased",
    "Leaves",
    "LookReleased",
    "LookTold",
    "LookUncounted",
    "Types",
];

/// The session's, the person's and the owner's steps, which the account only sees.
const ENVIRONMENT: [&str; 6] = [
    "Ends",
    "PersonClears",
    "PersonSends",
    "Recount",
    "Regrant",
    "Withdraw",
];

/// The session the model's ledger rows are booked for.
const SID: &str = "s-51";

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Real {
    /// The loop's clock, ms.
    now: i64,
    facts: FactsState,
    /// The composer holds the line.
    comp: bool,
    /// The session's submit count.
    submits: u64,
    /// A turn runs, the line's or the draft step's.
    live: bool,
    /// The owner's `prompt` is in force.
    prompt: bool,
}

impl Real {
    fn new() -> Self {
        Self {
            now: T0,
            facts: FactsState::default(),
            comp: false,
            submits: 7,
            live: false,
            prompt: true,
        }
    }

    /// The loop's read of the session as `run`'s host serves it.
    fn read(&self, run: Run) -> LeftLook {
        LeftLook::Read {
            draft: Some(self.comp),
            submits: run.served.then_some(self.submits),
            live: self.live,
        }
    }
}

/// How the model's `Recount` meets the code.
#[derive(Clone, Copy, Debug)]
enum Recount {
    /// A successor loop: the account read back from the left row the loop booked
    /// (`left_rows_in`, `FactsState::seed_left`), the host's count going on.
    Seed,
    /// The host back from an outage (`FactsState::recount`), its count going on.
    Outage,
    /// The host's count started again from 0 under the same loop, nothing told it (a count
    /// lower than the line was left at).
    Restart,
}

/// How a walk departs from the committed real code, for the negative controls.
#[derive(Clone, Copy)]
struct Run {
    /// The host serves the session's submit count; else a turn seen running stands for it.
    served: bool,
    /// How `Recount` is made.
    recount: Recount,
    /// The defect planted, if any.
    fault: Option<Fault>,
}

/// The model's `st` as the account says it: 1 left, 2 told, 0 neither.
fn project(r: &Real) -> State {
    let st = if r.facts.is_left(KEY) {
        1
    } else if r.facts.is_told(KEY) {
        2
    } else {
        0
    };
    State::from([("st", st)])
}

/// What one look settled of the episode: told, released, or nothing.
fn settled(out: &[(EpisodeKey, Left, Settled)]) -> Option<Settled> {
    match out {
        [] => None,
        [(key, _, how)] if *key == KEY => Some(*how),
        more => panic!("one episode, one settle: {more:?}"),
    }
}

/// What a look at `st` settles in the model where the session lives: told where the count it may
/// weigh rose (`LookTold`, the word the host's evidence gives) or where it may weigh none
/// (`LookUncounted`), released where it may and did not rise (`LookReleased`).
fn settles(st: &State, run: Run) -> Option<Settled> {
    if st["st"] != 1 || st["comp"] != 0 {
        return None;
    }
    Some(if st["known"] == 0 {
        Settled::Told(UNCOUNTED)
    } else if st["sent"] == 1 {
        Settled::Told(if run.served { SUBMITTED } else { TURN })
    } else {
        Settled::Released(CLEARED)
    })
}

/// The account agrees with the model at `st`: its projection, and what its probes on copies say
/// (a look's settle, the draft step's leave to send, a door's leave to write).
fn agrees(st: &State, r: &Real, run: Run) -> bool {
    let real = project(r);
    let look = if st["gone"] == 1 {
        LeftLook::Gone
    } else {
        r.read(run)
    };
    let probe = settled(&r.facts.clone().look(look, r.now));
    let want = if st["gone"] == 1 {
        (st["st"] == 1).then_some(Settled::Released(GONE))
    } else {
        settles(st, run)
    };
    // The composer unread first (a dialog over it) with whatever turn runs, then read as the
    // model has it with none running: the turn seen under the dialog stands for one seen now.
    let under_dialog = if st["gone"] == 1 {
        probe
    } else {
        let mut copy = r.facts.clone();
        let submits = run.served.then_some(r.submits);
        let unread = LeftLook::Read {
            draft: None,
            submits,
            live: r.live,
        };
        let kept = copy.look(unread, r.now);
        let read = LeftLook::Read {
            draft: Some(r.comp),
            submits,
            live: false,
        };
        if kept.is_empty() {
            settled(&copy.look(read, r.now))
        } else {
            Some(Settled::Released("an unread composer settled a line"))
        }
    };
    let may_go = r.facts.draft_may_go(r.prompt, false);
    let untold = !r.facts.is_told(KEY) && !r.facts.is_left(KEY);
    real["st"] == st["st"]
        && probe == want
        && under_dialog == want
        && may_go == (st["pr"] == 1 || st["st"] != 1)
        && untold == (st["st"] == 0)
        && r.comp == (st["comp"] == 1)
        && r.prompt == (st["pr"] == 1)
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

/// The real step for `action` at `st`; false where the code answers otherwise than the step
/// says. A step's own call runs in its evidence window, and nothing else does.
fn mirror(action: &'static str, st: &State, r: &mut Real, run: Run, ev: &mut Ev) -> bool {
    r.now += STEP_MS;
    let now = r.now;
    match action {
        "Types" => {
            ev.step(action, || r.facts.told(KEY, now));
            true
        }
        "Leaves" => {
            let left = Left {
                row: ROW,
                text: LINE.to_string(),
                at_ms: now,
                submits: run.served.then_some(r.submits),
                ran: false,
                seeded: false,
                counted: true,
            };
            r.comp = true;
            ev.step(action, || r.facts.leave(KEY, left));
            true
        }
        "PersonSends" => {
            r.comp = false;
            r.submits += 1;
            r.live = true;
            true
        }
        "PersonClears" => {
            r.comp = false;
            true
        }
        "DraftSends" => {
            if !r.facts.draft_may_go(r.prompt, false) {
                return false;
            }
            r.comp = false;
            r.submits += 1;
            r.live = true;
            let out = ev.step(action, || r.facts.look(LeftLook::Submitted, now));
            let want = (st["st"] == 1).then_some(Settled::Told(SUBMITTED));
            settled(&out) == want
        }
        "LookTold" | "LookReleased" | "LookUncounted" => {
            let look = r.read(run);
            let out = ev.step(action, || r.facts.look(look, now));
            r.live = false;
            match settled(&out) {
                Some(Settled::Told(UNCOUNTED)) => action == "LookUncounted",
                Some(Settled::Told(_)) => action == "LookTold",
                Some(Settled::Released(why)) => action == "LookReleased" && why == CLEARED,
                None => false,
            }
        }
        "Recount" => recount(r, run, now),
        "GoneReleased" => {
            let out = ev.step(action, || r.facts.look(LeftLook::Gone, now));
            settled(&out) == Some(Settled::Released(GONE))
        }
        "Withdraw" => {
            r.prompt = false;
            true
        }
        "Regrant" => {
            r.prompt = true;
            true
        }
        "Ends" => {
            r.comp = false;
            r.live = false;
            true
        }
        other => panic!("no real step for `{other}`"),
    }
}

/// The model's `Recount` as `run` makes it: a successor's account read back from the left row
/// the loop booked, the host back from an outage, or its count started again from 0.
fn recount(r: &mut Real, run: Run, now: i64) -> bool {
    match run.recount {
        Recount::Seed => {
            let Some(left) = r
                .facts
                .left_lines()
                .find(|(k, _)| **k == KEY)
                .map(|(_, l)| l)
            else {
                return false;
            };
            let reason = left_reason(
                ROW,
                KEY,
                LeftSaid::Left {
                    submits: left.submits,
                },
            );
            let row = format!(
                "{{\"ts\":{},\"rule_id\":\"{}\",\"decision\":\"waiting\",\"command\":\"{LINE}\",\
                 \"reason\":\"{reason}\",\"sid\":\"{SID}\"}}",
                left.at_ms,
                KEY.kind.rule(),
            );
            let lefts = left_rows_in(&row, Some(SID));
            if lefts.len() != 1 {
                return false;
            }
            let mut successor = FactsState::default();
            successor.seed_left(&[(KEY, now, SeedOutcome::Left)], lefts);
            r.facts = successor;
            r.live = false;
        }
        Recount::Outage => r.facts.recount(),
        Recount::Restart => {
            if !run.served {
                return false;
            }
            r.submits = 0;
        }
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
        let init = Real::new();
        assert!(
            agrees(&m.init_state(), &init, run),
            "the empty account is the model's start"
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
                let mut r = real.clone();
                let stepped = mirror(action.name, &st, &mut r, run, ev);
                w.fired.insert(action.name);
                if stepped && agrees(&next, &r, run) {
                    queue.push_back((next, r));
                    continue;
                }
                assert!(
                    !strict,
                    "the account disagrees after {} from {st:?}: the model's {next:?}, the \
                     account's {:?}",
                    action.name,
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

/// Both hosts: one that serves the submit count, one that serves none.
const HOSTS: [bool; 2] = [true, false];

/// Each host with each way `Recount` meets the code (a count started again only where one is
/// served).
fn runs(fault: Option<Fault>) -> Vec<Run> {
    HOSTS
        .into_iter()
        .flat_map(|served| {
            [Recount::Seed, Recount::Outage, Recount::Restart]
                .into_iter()
                .filter(move |recount| served || !matches!(recount, Recount::Restart))
                .map(move |recount| Run {
                    served,
                    recount,
                    fault,
                })
        })
        .collect()
}

#[test]
fn tier1_the_account_settles_exactly_where_the_model_does() {
    let runs = runs(None);
    let walked = aterm_spec::check::each(&runs, |&run| {
        walk(&left_write_model(), true, run, &mut Ev(None))
    });
    let reachable = interp::reachable_states(&left_write_model()).len();
    for (run, w) in runs.iter().zip(walked) {
        let want: BTreeSet<&str> = ANCHORED.into_iter().chain(ENVIRONMENT).collect();
        assert_eq!(
            w.fired, want,
            "served {} {:?}: the steps mirrored",
            run.served, run.recount
        );
        assert_eq!(
            w.states, reachable,
            "served {} {:?}: every state the model reaches is walked",
            run.served, run.recount
        );
    }
}

type Shows = fn(&State) -> bool;

#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Shows); 5] = [
        // The account keeps nothing at a left write: the model's line stands left.
        (Fault::ReleaseOnLeft, |st| st["st"] == 1),
        // A cleared composer keeps the line for good: the model gives it back.
        (Fault::LeftNeverSettles, |st| {
            st["st"] == 0 || (st["st"] == 1 && st["comp"] == 0 && st["sent"] == 0)
        }),
        // A cleared composer books the line told: the model gives it back.
        (Fault::ToldOnClear, |st| {
            st["st"] == 0 || (st["st"] == 1 && st["comp"] == 0 && st["sent"] == 0)
        }),
        // The draft step may send what `prompt` left once `prompt` is gone.
        (Fault::SubmitWithoutPrompt, |st| {
            st["st"] == 1 && st["pr"] == 0
        }),
        // A count no look may weigh is weighed: the model tells the clear composer's line.
        (Fault::ReleaseUncounted, |st| {
            st["st"] == 1 && st["comp"] == 0 && st["known"] == 0
        }),
    ];
    let runs: Vec<(Fault, Shows, Run)> = controls
        .into_iter()
        .flat_map(|(fault, shows)| {
            runs(Some(fault))
                .into_iter()
                .map(move |run| (fault, shows, run))
        })
        .collect();
    let walked = aterm_spec::check::each(&runs, |&(_, _, run)| {
        walk(&left_write_model(), false, run, &mut Ev(None))
    });
    for (&(fault, shows, run), w) in runs.iter().zip(walked) {
        assert!(
            !w.off.is_empty(),
            "{fault:?} (served {} {:?}) agreed everywhere",
            run.served,
            run.recount
        );
        assert!(
            w.off.iter().all(shows),
            "{fault:?} (served {} {:?}): {:?}",
            run.served,
            run.recount,
            w.off
        );
    }
}

#[test]
fn tier1_every_anchored_step_enters_its_own_function() {
    let m = left_write_model();
    let mut ev = Ev(Some(StepEvidence::new("LeftWrite")));
    for run in runs(None) {
        let w = walk(&m, true, run, &mut ev);
        assert_eq!(
            w.states,
            interp::reachable_states(&m).len(),
            "served {} {:?}",
            run.served,
            run.recount
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
        .filter(|r| r.machine == "LeftWrite")
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, ANCHORED.into_iter().collect(), "{anchored:?}");
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "LeftWrite")
        .map(|w| w.action)
        .collect();
    let want: BTreeSet<&str> = ENVIRONMENT.into_iter().chain(MUTANTS).collect();
    assert_eq!(waived, want, "{waived:?}");
    let actions: BTreeSet<&str> = left_write_model().actions.iter().map(|a| a.name).collect();
    let bound: BTreeSet<&str> = anchored.union(&waived).copied().collect();
    assert_eq!(bound, actions, "every action anchored or waived");
}
