// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `LineCustody` (`aterm_models_harness::line_custody_model`) to the pure
//! core of a line held in custody (`aterm_agent::supervise::custody`, ATERM-KI-1101-3): the
//! custody formed on [`Line::formed`], every look's settle on [`Line::look`], its program's end
//! on [`Line::ended`], a turn's on [`Line::took`], [`Line::turned`] and [`Line::outlasted`],
//! the gate's hold and loss on [`Line::gate_read`] and [`Rest::gate`], the loop's answer to a box
//! over the composer on [`answer_goes`], the restart or the wait on [`Line::lose`] and
//! [`Restarts`], the host's word that it made the restart on [`Restarts::answered`] (the restart
//! made, the line ended with its program, only where another process is named after it; with the
//! line's own process named, it stands held and the restart stands asked), its deferral on
//! [`Restarts::deferred`], a restart standing spent by its line's settle on [`Restarts::settled`],
//! and what the host is told on [`line_held`]. The worker's program, the person, the session's
//! rest, the process the host and the restart's report name, and the clock are the model's
//! environment, rendered into the reads, `status` words, rests, lanes, processes and instants the
//! core takes; the host's word after a restart names the process it left by the restart's report
//! with the host's own naming still the process before (a kept snapshot lagging), or by the host
//! alone, the report naming none. At every state the
//! core is probed on copies, so a look settles, a turn settles, a write is held or lost, a box's
//! answer goes, a loss is restarted or waits, and the host is let type or move exactly where the
//! model says. Each walk runs in the committed configuration and in each scenario (a host that
//! serves no input word, no restart lane), under six renderings: the unread word (`pending`,
//! `typeahead`), for the line queued or input queued behind it, or the unserved one (none, `-`); a
//! draw whole, arriving, joined to a person's words, or garbled with no person's key; each read
//! under the model's box over the composer (`dlg`), the line drawn under it or not, as one that
//! cannot say what the composer holds: a dialog's (no composer text), a glyph's cell unread (the
//! composer's text, no say of what it holds), or no composer drawn (no text, no key count); each
//! cause of a read not at rest; each cause of no restart lane; and a clear read dated by the
//! write's held-text note or not. Every read the turn's settle takes is probed, so a turn settles,
//! outlasts or keeps the line exactly where the model says, under the box too.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::time::{Duration, Instant};

use aterm_agent::supervise::custody::{
    At, CustodyGate, ECHO_HANG, GateRead, Held, Lane, LeftStands, Line, LineHeld, Look, Loss,
    LostRestart, Process, Rest, Restarted, Restarts, Settles, Shows, Turned, answer_goes,
    line_held,
};
use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_models_harness::line_custody_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, StepEvidence};

type State = BTreeMap<&'static str, i64>;

/// The line the loop wrote.
const LINE: &str = "Continue with stage 2 of the plan.";

/// A part of it arriving.
const ARRIVING: &str = "Continue with st";

/// Text that is neither the line, a part of it, nor holds it: a person's, or a garble.
const OTHER: &str = "hold on, I am typing";

/// The person's grace.
const GRACE: Duration = Duration::from_secs(5);

/// The loop's clock between two steps.
const STEP: Duration = Duration::from_millis(1);

/// How long before a write's give-up its held-text note read the input clear.
const NOTED: Duration = Duration::from_secs(50);

/// The model's mutants, which no real step mirrors.
const MUTANTS: [&str; 15] = [
    "AnswersComposer",
    "AnswersWithoutRead",
    "HostOverStands",
    "LostWithinMinute",
    "RestartAgain",
    "RestartBehind",
    "RestartTakenOnWord",
    "RestartUnread",
    "SettleKeepsAsked",
    "SettleOnMove",
    "SettleWithoutTurn",
    "TurnSettlesUnderDialog",
    "TurnSettlesUnread",
    "UpgradeOverStands",
    "WriteWhileHeld",
];

/// The steps the core performs, each on its anchored function.
const ANCHORED: [&str; 22] = [
    "AnswersBox",
    "AsksRestart",
    "BreakKeepsDraw",
    "Enters",
    "GateHolds",
    "HostTypes",
    "HostUpgrades",
    "LookDrawn",
    "LookPerson",
    "RestartDeferred",
    "RestartMade",
    "RestartMadeDeferred",
    "RestartStands",
    "RestartUnmade",
    "SeesEnded",
    "TurnOutlasted",
    "TurnRuns",
    "TurnSettles",
    "Types",
    "WaitsLost",
    "WritesQueued",
    "WritesRead",
];

/// The program's, the person's, the session's and the clock's steps, which the core only sees.
const ENVIRONMENT: [&str; 16] = [
    "DialogGoes",
    "DialogShows",
    "Minute",
    "NextAct",
    "PersonClears",
    "PersonDraws",
    "ProgramDraws",
    "ProgramDrops",
    "ProgramEnds",
    "ProgramReads",
    "Queues",
    "ReadsAndDraws",
    "ReadsAndDrops",
    "ReadsBehind",
    "Rests",
    "Stirs",
];

/// The real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Real {
    /// The loop's clock.
    now: Instant,
    /// The line held, if one is.
    line: Option<Line>,
    /// The restart asked since the last act submitted.
    restarts: Restarts,
    /// The person's key count.
    human: u64,
    /// The agent's process id.
    pid: u32,
}

impl Real {
    fn new(t0: Instant) -> Self {
        Self {
            now: t0,
            line: None,
            restarts: Restarts::default(),
            human: 40,
            pid: 7_100,
        }
    }

    /// The agent's process as the host names it.
    fn process(&self) -> Option<Process> {
        Some((Some(self.pid), Some(1_000 + i64::from(self.pid))))
    }
}

/// One walk's configuration and rendering, and the defect planted, if any.
#[derive(Clone, Copy, Debug)]
struct Run {
    /// The host serves an input word (`Served = 1`).
    served: bool,
    /// A restart lane is there (`Lane = 1`).
    lane: bool,
    /// Which rendering each choice takes.
    v: usize,
    /// The defect planted.
    fault: Option<Fault>,
}

impl Run {
    /// The model in this run's configuration.
    fn model(self) -> Model {
        interp::with_consts(
            &line_custody_model(),
            &[
                ("Served", i64::from(self.served)),
                ("Lane", i64::from(self.lane)),
            ],
        )
    }
}

/// What `status input=` says at `st`: clear unless the line, or input behind it, is queued, on
/// a host that serves the word; nothing a read can weigh on one that serves none.
fn word(st: &State, run: Run) -> Option<&'static str> {
    if !run.served {
        return [None, Some("-")][run.v % 2];
    }
    Some(if st["q"] == 4 || st["bq"] == 1 {
        ["pending", "typeahead"][run.v % 2]
    } else {
        "clear"
    })
}

/// What a read at `st` shows of the line: a person's draft over it, its draw (whole, arriving,
/// joined to a person's words, or a garble with no person's key since the write), or none of it;
/// under the box over the composer, a read that cannot say what it holds, the line drawn under
/// it or not (a dialog's, a glyph's cell unread, no composer drawn), which is no evidence.
fn shows(st: &State, r: &Real, run: Run) -> Shows {
    let human = Some(r.human);
    if st["dlg"] == 1 {
        let (composer, human) = match run.v % 3 {
            0 => (String::new(), human),
            1 => {
                let text = if st["q"] == 1 {
                    LINE
                } else if st["pk"] == 1 {
                    OTHER
                } else {
                    ""
                };
                (text.to_string(), human)
            }
            _ => (String::new(), None),
        };
        return Shows {
            stands: None,
            composer,
            human,
        };
    }
    let (stands, composer, human) = if st["pk"] == 1 {
        (LeftStands::Part, OTHER.to_string(), human)
    } else if st["q"] == 1 {
        match run.v % 4 {
            0 => (LeftStands::Whole, LINE.to_string(), human),
            1 => (LeftStands::Part, ARRIVING.to_string(), human),
            2 => (
                LeftStands::Part,
                format!("{LINE} {OTHER}"),
                Some(r.human + 1),
            ),
            _ => (LeftStands::Part, OTHER.to_string(), human),
        }
    } else {
        (LeftStands::Nothing, String::new(), human)
    };
    Shows {
        stands: Some(stands),
        composer,
        human,
    }
}

/// What the read at `st` says of rest: at rest where the model's `rest` is, else one cause of
/// not (the agent's own background work running under the turn's end among them), and the
/// composer clear where it shows none of the line. Under the box over the composer each read is
/// as the shell judges it: a dialog drawn over the composer, or no composer drawn, is no turn's
/// end; the composer's glyph cell unread is the turn's end the rest's cause says, its composer
/// never clear, since the read cannot say what it holds ([`shows`]).
fn rest(st: &State, shows: &Shows, run: Run) -> Rest {
    let mut rest = Rest {
        turn_ended: true,
        working: false,
        clear: false,
        halted: false,
        autopilot: true,
        person: [None, Some(GRACE + Duration::from_secs(1))][run.v % 2],
        grace: GRACE,
        stuck: false,
    };
    if st["rest"] == 0 {
        match run.v % 6 {
            0 => rest.turn_ended = false,
            1 => rest.halted = true,
            2 => rest.autopilot = false,
            3 => rest.person = Some(GRACE - Duration::from_secs(1)),
            4 => rest.working = true,
            _ => rest.stuck = true,
        }
    }
    if st["dlg"] == 1 && run.v % 3 != 1 {
        rest.turn_ended = false;
    }
    rest.clear = rest.turn_ended && shows.stands == Some(LeftStands::Nothing);
    rest
}

/// Where a restart could be made: everywhere under `Lane = 1`, else for one cause not.
fn lane(run: Run) -> Lane {
    let mut lane = Lane {
        relaunch: true,
        host: true,
        can_restart: true,
    };
    if !run.lane {
        match run.v % 3 {
            0 => lane.relaunch = false,
            1 => lane.host = false,
            _ => lane.can_restart = false,
        }
    }
    lane
}

/// The model's variables the core holds: the custody, what `status` said of the line (and its
/// minute), a turn read running, a break's kept draw, the line lost, and the restart asked.
fn project(r: &Real) -> State {
    let restart = match r.restarts.get() {
        None if r.restarts.standing() => 3,
        None => 0,
        Some(LostRestart::Made | LostRestart::Unseen) => 1,
        Some(LostRestart::Unmade(_)) => 2,
    };
    let Some(line) = &r.line else {
        return State::from([
            ("cust", 0),
            ("held", 0),
            ("ripe", 0),
            ("ran", 0),
            ("drawn", 0),
            ("lost", 0),
            ("restart", restart),
        ]);
    };
    let (held, ripe) = match line.held() {
        Held::Unread => (0, 0),
        Held::Read { since } => (1, i64::from(r.now >= since + ECHO_HANG)),
        Held::Unknown => (2, 0),
    };
    State::from([
        ("cust", 1),
        ("held", held),
        ("ripe", ripe),
        ("ran", i64::from(line.ran())),
        ("drawn", i64::from(line.drawn().is_some())),
        ("lost", i64::from(line.is_lost())),
        ("restart", restart),
    ])
}

/// The core agrees with the model at `st`: its projection, what the host is told, and what its
/// probes on copies say of a look at a point and at a break, the program's end, a turn's
/// settle, a box's answer, the gate's decision for a line's write and for any other, and the
/// loss.
fn agrees(m: &Model, st: &State, r: &Real, run: Run) -> bool {
    if project(r).iter().any(|(k, v)| st[k] != *v) {
        return false;
    }
    let on = |action: &str| m.action_enabled(action, st);
    let told = line_held(r.line.as_ref());
    let want_told = (st["cust"] == 1).then_some(if st["lost"] == 1 {
        LineHeld::Lost
    } else {
        LineHeld::Stands
    });
    let free = told != Some(LineHeld::Stands);
    if told != want_told
        || (st["asked"] == 0 && (on("HostTypes") != free || on("HostUpgrades") != free))
    {
        return false;
    }
    let Some(line) = r.line.as_ref().filter(|_| st["asked"] == 0) else {
        return true;
    };
    let shows = shows(st, r, run);
    let point = match line.clone().look(&shows, At::Point) {
        Look::Settled(settles, _) => Some(settles),
        Look::Stands => None,
        Look::Kept => return false,
    };
    let want_point = if on("LookPerson") {
        Some(Settles::Person)
    } else {
        on("LookDrawn").then_some(Settles::Drawn)
    };
    let mut at_break = line.clone();
    let brk = at_break.look(&shows, At::Break);
    let want_brk = if on("LookPerson") {
        Look::Settled(Settles::Person, LeftStands::Nothing)
    } else if st["q"] == 1 && st["dlg"] == 0 {
        Look::Kept
    } else {
        Look::Stands
    };
    if point != want_point
        || brk != want_brk
        || (brk == Look::Kept && at_break.drawn().is_none())
        || line.ended(r.process()) != on("SeesEnded")
    {
        return false;
    }
    let w = word(st, run);
    let want_turned = if on("TurnSettles") {
        Turned::Settles
    } else if on("TurnOutlasted") {
        Turned::Outlasts
    } else {
        Turned::Stands
    };
    let mut answer = line.clone();
    let _ = answer.gate_read(w, false, r.now);
    if line.turned(w, &shows) != want_turned
        || answer_goes(answer.held(), &shows) != on("AnswersBox")
    {
        return false;
    }
    if st["drawn"] == 1 || st["moved"] == 1 || (st["dlg"] == 0 && (st["q"] == 1 || st["pk"] == 1)) {
        // The look settles the line before the gate reads it.
        return true;
    }
    let mut copy = line.clone();
    let gate = match copy.gate_read(w, true, r.now) {
        GateRead::Holds => CustodyGate::Hold,
        GateRead::Again => rest(st, &shows, run).gate(),
    };
    let lost = on("AsksRestart") || on("WaitsLost");
    if (gate == CustodyGate::Lost) != lost || (gate == CustodyGate::Hold) != on("GateHolds") {
        return false;
    }
    if gate == CustodyGate::Lost {
        let restarts = match copy.lose(&r.restarts, &lane(run), r.now) {
            Loss::Restart { .. } => true,
            Loss::Wait { .. } => false,
        };
        if restarts != on("AsksRestart") {
            return false;
        }
    }
    // A write that is no line's (a key, a menu's choice) never takes the restart.
    line.clone().gate_read(w, false, r.now) == GateRead::Holds
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

/// The real step for `action` from `st` to `next`; false where the core answers otherwise than
/// the step says. A step's own calls run in its evidence window, and nothing else does.
fn mirror(
    action: &'static str,
    st: &State,
    next: &State,
    r: &mut Real,
    run: Run,
    ev: &mut Ev,
) -> bool {
    r.now += STEP;
    let now = r.now;
    match action {
        "WritesQueued" | "WritesRead" => {
            // The first `status` read after the write reads the line where the model has it.
            let w = word(next, run);
            let noted = (action == "WritesRead" && run.served && run.v % 2 == 1)
                .then(|| now.checked_sub(NOTED))
                .flatten();
            let (human, process) = (r.human, r.process());
            let line = ev.step(action, || Line::formed(LINE, human, w, noted, process, now));
            r.line = Some(line);
            true
        }
        "Types" | "Enters" => {
            ev.step(action, || r.restarts.submitted());
            true
        }
        "PersonDraws" => {
            r.human += 1;
            true
        }
        "ProgramEnds" => {
            r.pid += 1;
            true
        }
        "Minute" => {
            r.now += ECHO_HANG;
            true
        }
        "RestartMade" | "RestartStands" => {
            // The host's word that it made the restart, and the process named after it: another
            // one where the restart ended the line's program, the line's own where it stands. On
            // odd renderings the restart's own report names it while the host still names the
            // process before (a kept snapshot lagging the restart); on even ones the report names
            // none and the host names it. What the word comes to is the core's to say, and the
            // line goes only where it says so.
            let was = r.process();
            if action == "RestartMade" {
                r.pid += 1;
            }
            let (left, named) = if run.v % 2 == 1 {
                (Some((Some(r.pid), None)), was)
            } else {
                (None, r.process())
            };
            let (line, restarts) = (r.line.as_ref(), &mut r.restarts);
            let restarted = ev.step(action, || restarts.answered(line, left, named));
            if restarted == Restarted::Ended {
                r.line = None;
            }
            true
        }
        "RestartDeferred" | "RestartMadeDeferred" => {
            // A deferral's word; under the second, the restart was made all the same (its signal
            // went, the process it started not yet registered): the host names another process.
            if action == "RestartMadeDeferred" {
                r.pid += 1;
            }
            ev.step(action, || r.restarts.deferred());
            true
        }
        "RestartUnmade" => {
            let step = [Some("refused"), None][run.v % 2];
            ev.step(action, || r.restarts.unmade(step));
            true
        }
        "HostTypes" => {
            let told = ev.step(action, || line_held(r.line.as_ref()));
            told != Some(LineHeld::Stands)
        }
        "HostUpgrades" => {
            // The upgrade's move ends the agent's process: the host names another after it.
            let told = ev.step(action, || line_held(r.line.as_ref()));
            r.pid += 1;
            told != Some(LineHeld::Stands)
        }
        "DialogGoes" | "DialogShows" | "NextAct" | "PersonClears" | "ProgramDraws"
        | "ProgramDrops" | "ProgramReads" | "Queues" | "ReadsAndDraws" | "ReadsAndDrops"
        | "ReadsBehind" | "Rests" | "Stirs" => true,
        _ => held_step(action, st, r, run, ev),
    }
}

/// The real step for `action`, one of the custody's own on the line held, from `st`.
fn held_step(action: &'static str, st: &State, r: &mut Real, run: Run, ev: &mut Ev) -> bool {
    let shows = shows(st, r, run);
    let process = r.process();
    let now = r.now;
    let w = word(st, run);
    let restarts = &mut r.restarts;
    let Some(line) = r.line.as_mut() else {
        return false;
    };
    // A settle spends a restart that stood asked for the line, in the settle's own window.
    let spend = |settles: bool, restarts: &mut Restarts| {
        if settles {
            restarts.settled();
        }
        settles
    };
    let settled = match action {
        "TurnRuns" => {
            ev.step(action, || line.took());
            return true;
        }
        "BreakKeepsDraw" => {
            let look = ev.step(action, || line.look(&shows, At::Break));
            return look == Look::Kept && line.drawn().is_some();
        }
        "LookDrawn" => {
            let kept = line.drawn();
            let stands = if st["q"] == 1 && st["dlg"] == 0 {
                Some(LeftStands::Nothing)
            } else {
                kept
            };
            ev.step(action, || {
                let look = line.look(&shows, At::Point);
                let drawn = matches!(look, Look::Settled(Settles::Drawn, s) if Some(s) == stands);
                spend(drawn, restarts)
            })
        }
        "LookPerson" => {
            let at = [At::Point, At::Break][run.v % 2];
            ev.step(action, || {
                let look = line.look(&shows, at);
                spend(matches!(look, Look::Settled(Settles::Person, _)), restarts)
            })
        }
        "SeesEnded" => ev.step(action, || spend(line.ended(process), restarts)),
        "AnswersBox" => {
            // The loop reads `status` before testing whether a dialog answer passes custody. A
            // fresh clear word can therefore prove the held line read in this same step.
            let _ = line.gate_read(w, false, now);
            return ev.step(action, || answer_goes(line.held(), &shows));
        }
        "TurnSettles" => ev.step(action, || {
            spend(line.turned(w, &shows) == Turned::Settles, restarts)
        }),
        "TurnOutlasted" => {
            let turned = ev.step(action, || {
                let turned = line.turned(w, &shows);
                if turned == Turned::Outlasts {
                    line.outlasted(w, now);
                }
                turned
            });
            return turned == Turned::Outlasts;
        }
        "GateHolds" => {
            let rest = rest(st, &shows, run);
            let gate = ev.step(action, || {
                let gate = match line.gate_read(w, true, now) {
                    GateRead::Holds => CustodyGate::Hold,
                    GateRead::Again => rest.gate(),
                };
                if gate == CustodyGate::Hold {
                    line.rearm(now);
                }
                gate
            });
            return gate == CustodyGate::Hold;
        }
        "AsksRestart" | "WaitsLost" => {
            let (rest, lane) = (rest(st, &shows, run), lane(run));
            let restarts = &*restarts;
            let loss = ev.step(action, || match line.gate_read(w, true, now) {
                GateRead::Again if rest.gate() == CustodyGate::Lost => {
                    Some(line.lose(restarts, &lane, now))
                }
                GateRead::Again | GateRead::Holds => None,
            });
            return matches!(
                (action, loss),
                ("AsksRestart", Some(Loss::Restart { .. }))
                    | ("WaitsLost", Some(Loss::Wait { .. }))
            );
        }
        other => panic!("no real step for `{other}`"),
    };
    r.line = None;
    settled
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
        let init = Real::new(Instant::now());
        assert!(
            agrees(m, &m.init_state(), &init, run),
            "no line held is the model's start"
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
                let stepped = mirror(action.name, &st, &next, &mut r, run, ev);
                w.fired.insert(action.name);
                if stepped && agrees(m, &next, &r, run) {
                    queue.push_back((next, r));
                    continue;
                }
                assert!(
                    !strict,
                    "the core disagrees after {} from {st:?} ({run:?}): the model's {next:?}, \
                     the core's {:?}",
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

/// The renderings each configuration is walked under: one for each cause of a read not at rest.
const RENDERINGS: usize = 6;

/// The committed configuration and each scenario, under every rendering.
fn runs(fault: Option<Fault>) -> Vec<Run> {
    [(true, true), (false, true), (true, false)]
        .into_iter()
        .flat_map(|(served, lane)| {
            (0..RENDERINGS).map(move |v| Run {
                served,
                lane,
                v,
                fault,
            })
        })
        .collect()
}

#[test]
fn tier1_the_core_holds_and_settles_exactly_where_the_model_does() {
    let runs = runs(None);
    let walked =
        aterm_spec::check::each(&runs, |&run| walk(&run.model(), true, run, &mut Ev(None)));
    for (run, w) in runs.iter().zip(walked) {
        let m = run.model();
        assert_eq!(
            w.fired,
            interp::fired_actions(&m),
            "{run:?}: the steps mirrored"
        );
        assert_eq!(
            w.states,
            interp::reachable_states(&m).len(),
            "{run:?}: every state the model reaches is walked"
        );
    }
}

/// Where a planted defect may disagree, in the model's terms.
type Where = fn(&State, Run) -> bool;

/// Which configurations a planted defect shows in.
type Applies = fn(Run) -> bool;

#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    let controls: [(Fault, Applies, Where); 14] = [
        // A point's look settles a line no read shows, or none can say of: the model holds it.
        (
            Fault::CustodySettlesUnshown,
            |_| true,
            |st, _| {
                st["cust"] == 1
                    && st["drawn"] == 0
                    && (st["dlg"] == 1 || (st["q"] != 1 && st["pk"] == 0))
            },
        ),
        // A turn settles a line on an input unread or unknown: the model outlasts the turn.
        (
            Fault::CustodyTurnAnyWord,
            |_| true,
            |st, run| st["cust"] == 1 && (st["q"] == 4 || st["bq"] == 1 || !run.served),
        ),
        // A line no read found clear is lost at the gate: the model holds the write.
        (
            Fault::CustodyLostUnread,
            |_| true,
            |st, _| st["cust"] == 1 && st["held"] != 1,
        ),
        // A line read clear is lost within its minute: the model holds the write.
        (
            Fault::CustodyLostWithinMinute,
            |run| run.served,
            |st, _| st["cust"] == 1 && st["ripe"] == 0,
        ),
        // A second loss is restarted again: the model waits.
        (
            Fault::CustodyRestartAgain,
            |run| run.served && run.lane,
            |st, _| st["cust"] == 1 && st["restart"] > 0,
        ),
        // The host is told nothing of a line that stands: the model holds the host.
        (
            Fault::CustodyHostUntold,
            |_| true,
            |st, _| st["cust"] == 1 && st["lost"] == 0,
        ),
        // A read that cannot say is taken for the line whole: the model holds it.
        (
            Fault::CustodySettlesUnreadable,
            |_| true,
            |st, _| st["cust"] == 1 && st["dlg"] == 1,
        ),
        // A turn settles or outlasts a line on a read that cannot say what the composer holds:
        // the model keeps the turn's evidence for the read after the box.
        (
            Fault::CustodyTurnUnreadable,
            |_| true,
            |st, _| st["cust"] == 1 && st["dlg"] == 1 && st["ran"] == 1,
        ),
        // A key that is no line's goes where no box is over the composer: the model holds it.
        (
            Fault::CustodyAnswersComposer,
            |run| run.served,
            |st, run| {
                st["cust"] == 1
                    && st["dlg"] == 0
                    && st["asked"] == 0
                    && (st["held"] == 1 || (run.served && st["q"] != 4 && st["bq"] == 0))
            },
        ),
        // A box's key follows a line that neither an earlier nor this gate's input read proved
        // read: the model holds it.
        (
            Fault::CustodyAnswersWithoutRead,
            |_| true,
            |st, run| {
                st["cust"] == 1
                    && st["dlg"] == 1
                    && st["asked"] == 0
                    && (!run.served || (st["held"] != 1 && (st["q"] == 4 || st["bq"] == 1)))
            },
        ),
        // A read line is lost though input behind it reads unread: the model holds the write.
        (
            Fault::CustodyLostBehind,
            |run| run.served,
            |st, _| st["cust"] == 1 && st["bq"] == 1,
        ),
        // The host's word that it made the restart settles the line and books the restart made
        // though the process the line was written to stands: the model holds the line, its
        // restart standing unanswered, to be asked again.
        (
            Fault::CustodyRestartOnWord,
            |run| run.served && run.lane,
            |st, _| st["cust"] == 1 && st["asked"] == 0 && st["restart"] == 3 && st["ripe"] == 1,
        ),
        // The host's naming is taken before the restart's own report: a restart made while the
        // host still names the process it ended reads as nothing ended, where the model ends the
        // line with its program and books the restart made.
        (
            Fault::CustodyRestartBySnapshot,
            |run| run.served && run.lane && run.v % 2 == 1,
            |st, _| st["cust"] == 0 && st["asked"] == 0 && st["restart"] == 1,
        ),
        // A settle leaves the restart that stood asked for its line unspent: the model spends it.
        (
            Fault::CustodySettleKeepsAsked,
            |run| run.served && run.lane,
            |st, _| st["cust"] == 0 && st["restart"] == 1,
        ),
    ];
    let runs: Vec<(Fault, Where, Run)> = controls
        .into_iter()
        .flat_map(|(fault, applies, at)| {
            runs(Some(fault))
                .into_iter()
                .filter(move |&run| applies(run))
                .map(move |run| (fault, at, run))
        })
        .collect();
    let walked = aterm_spec::check::each(&runs, |&(_, _, run)| {
        walk(&run.model(), false, run, &mut Ev(None))
    });
    for (&(fault, at, run), w) in runs.iter().zip(walked) {
        assert!(!w.off.is_empty(), "{fault:?} ({run:?}) agreed everywhere");
        assert!(
            w.off.iter().all(|st| at(st, run)),
            "{fault:?} ({run:?}): {:?}",
            w.off
        );
        // The box is rendered over a line drawn under it too, where a turn settled would leave
        // the line drawn with no writer.
        assert!(
            fault != Fault::CustodyTurnUnreadable || w.off.iter().any(|st| st["q"] == 1),
            "{fault:?} ({run:?}) never disagreed over a line drawn under the box"
        );
    }
}

#[test]
fn tier1_every_anchored_step_enters_its_own_function() {
    let mut ev = Ev(Some(StepEvidence::new("LineCustody")));
    for run in runs(None) {
        let m = run.model();
        let w = walk(&m, true, run, &mut ev);
        assert_eq!(w.states, interp::reachable_states(&m).len(), "{run:?}");
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
        .filter(|r| r.machine == "LineCustody")
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, ANCHORED.into_iter().collect(), "{anchored:?}");
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == "LineCustody")
        .map(|w| w.action)
        .collect();
    let want: BTreeSet<&str> = ENVIRONMENT.into_iter().chain(MUTANTS).collect();
    assert_eq!(waived, want, "{waived:?}");
    let actions: BTreeSet<&str> = line_custody_model()
        .actions
        .iter()
        .map(|a| a.name)
        .collect();
    let bound: BTreeSet<&str> = anchored.union(&waived).copied().collect();
    assert_eq!(bound, actions, "every action anchored or waived");
}
