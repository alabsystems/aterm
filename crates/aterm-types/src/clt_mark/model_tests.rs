// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance of the account's record of setup's administrator-password attempt to
//! `CltEpisode` (`aterm_models_setup::clt_episode_model`).
//!
//! The walk drives every reachable step of the model on the real record, its file and its
//! askers' processes held in memory, the window's ends decided by its own decision
//! ([`tab_end`]) from the exit its tab's program would have, and at every reachable state each
//! automatic claim and lift the model refuses, which the record must refuse too, leaving it as
//! it was. A terminal `aterm init`'s ends are decided by its own decision ([`term_end`]) from
//! how its run ended. The window keeps only the claim [`hand`] gives back, as the window does.
//! The negative control plants each defect in the code under test ([`Defect`]), never in the
//! rig, and shows a law of the model catches it there.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use aterm_models_setup::clt_episode_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::*;

/// The record's place: the rig's files are its own, in memory.
const PATH: &str = "/nowhere/aterm-clt-episode/command-line-tools-tab";

/// The rig's first clock (unix seconds).
const START: u64 = 2_000_000_000;

/// The model's `Tick`: past every pause, a decline's longest hold ([`DECLINE_AGAIN`]) included.
const TICK: u64 = 200 * 86_400;

/// The askers' pids: the window, its Mac Setup tab's program, a terminal `aterm init` and a
/// person's own retry. A process that ends and starts again is the same pid born again.
const WINDOW: i32 = 501;
const PROGRAM: i32 = 502;
const TERMINAL: i32 = 601;
const PERSON: i32 = 701;

const PROJECTED: [&str; 25] = [
    "rec", "held", "handed", "live", "win", "term", "own", "quitting", "tries", "fin", "prior",
    "pheld", "phanded", "plive", "due", "pdue", "fdue", "sd", "psd", "said", "ends", "ever",
    "early", "cutend", "cutno",
];

const ANCHORED: [&str; 20] = [
    "ClaimT",
    "ClaimW",
    "ClosedW",
    "DeclinedO",
    "DeclinedT",
    "DeclinedW",
    "DoneO",
    "DoneT",
    "DoneW",
    "ExplicitO",
    "Hand",
    "InterruptedT",
    "Lift",
    "Read",
    "ReleasedO",
    "ReleasedT",
    "ReleasedW",
    "UnfinishedO",
    "UnfinishedT",
    "UnfinishedW",
];

const MUTANTS: [&str; 12] = [
    "CutOffReadsDeclined",
    "ClaimBesideLive",
    "ClaimThroughDecline",
    "HandLeavesWindow",
    "HandDropsClaim",
    "StaleEndWrites",
    "RetryEndDropsAsk",
    "ReleaseCountsATry",
    "EndForgetsFinish",
    "InterruptionDeclines",
    "QuitDropsFinish",
    "HangupDeclinesT",
];

/// The record's file and the processes that run, in memory.
#[derive(Clone, Default)]
struct Mem {
    text: RefCell<Option<String>>,
    running: RefCell<BTreeSet<(i32, u64)>>,
}

impl Files for Mem {
    fn read(&self, _: &Path, _: usize) -> io::Result<Option<String>> {
        Ok(self.text.borrow().clone())
    }
    fn write(&self, _: &Path, text: &str) -> io::Result<()> {
        *self.text.borrow_mut() = Some(text.to_owned());
        Ok(())
    }
    fn remove(&self, _: &Path) -> io::Result<()> {
        *self.text.borrow_mut() = None;
        Ok(())
    }
    fn lock(&self, _: &Path) -> io::Result<Held> {
        Ok(Held::new(()))
    }
    fn runs(&self, pid: i32, birth: u64) -> bool {
        self.running.borrow().contains(&(pid, birth))
    }
}

/// The real record and its askers, and what the model reads of how the record behaved.
#[derive(Clone)]
struct Rig {
    files: Mem,
    now: u64,
    /// Each asker's birth now: a process that starts again is born again.
    window: u64,
    program: u64,
    terminal: u64,
    person: u64,
    /// The claim the window holds for its attempt, as [`hand`] gave it back, whether its tab's
    /// program asks it (the model's `win == 2`), and whether that program asks under no claim the
    /// window holds (`win == 3`).
    win: Option<Claim>,
    asks_in_tab: bool,
    orphan: bool,
    /// aterm's quit is noted: the window decides its tab's end as a quit's.
    quitting: bool,
    term: Option<Claim>,
    own: Option<Claim>,
    said: bool,
    ends: i64,
    ever: bool,
    early: bool,
    cutend: bool,
    /// A terminal's end decided a decline for what its hangup or a `SIGTERM` stopped it with.
    cutno: bool,
}

impl Rig {
    fn new() -> Self {
        let rig = Self {
            files: Mem::default(),
            now: START,
            window: 1,
            program: 0,
            terminal: 0,
            person: 0,
            win: None,
            asks_in_tab: false,
            orphan: false,
            quitting: false,
            term: None,
            own: None,
            said: false,
            ends: 0,
            ever: false,
            early: false,
            cutend: false,
            cutno: false,
        };
        rig.start(WINDOW, rig.window);
        rig
    }

    fn path() -> &'static Path {
        Path::new(PATH)
    }

    fn asker(pid: i32, birth: u64) -> Asker {
        Asker {
            build: "release".to_owned(),
            pid,
            birth,
        }
    }

    fn start(&self, pid: i32, birth: u64) {
        self.files.running.borrow_mut().insert((pid, birth));
    }

    fn stop(&self, pid: i32, birth: u64) {
        self.files.running.borrow_mut().remove(&(pid, birth));
    }

    /// The record's one line, as the code reads it.
    fn line(&self) -> Option<Line> {
        self.files
            .text
            .borrow()
            .as_deref()
            .map(|text| state(text).expect("the record reads"))
    }

    /// Whether a reading now says a person's decline holds.
    fn declined(&self) -> bool {
        matches!(
            record(&self.files, Self::path(), self.now).asked,
            Asked::Declined { .. }
        )
    }

    /// The window's process ends and the next one starts.
    fn window_restarts(&mut self) {
        self.stop(WINDOW, self.window);
        self.window += 1;
        self.start(WINDOW, self.window);
    }

    /// End `claim`'s attempt as `how`, and read back what the model reads of it.
    fn end_as(&mut self, claim: &Claim, how: End) {
        let wrote =
            end(&self.files, Self::path(), claim, how, self.now).expect("the end is recorded");
        self.ended(wrote, how);
    }

    /// What the model reads of an end the record took where it held the claim (`took`).
    fn ended(&mut self, took: Took, how: End) {
        if how == End::Done {
            self.ever = true;
        }
        if took.held() {
            match how {
                End::Unfinished => self.ends += 1,
                End::Done | End::Declined => self.ends = 0,
                End::Released => {}
            }
        }
    }

    /// The window's attempt ends as its decision reads the exit `exit` of its tab's program
    /// (closed while it ran where `closed`), inside `action`'s window.
    fn window_ends(
        &mut self,
        exit: Option<TabExit>,
        closed: bool,
        action: &'static str,
        ev: &mut StepEvidence,
    ) {
        let claim = self.win.clone().expect("the window asks");
        let quit = self.quitting;
        let (how, wrote) = ev.step(action, || {
            let how = tab_end(exit, closed, quit).expect("the window writes this end");
            let wrote =
                end(&self.files, Self::path(), &claim, how, self.now).expect("the end is recorded");
            (how, wrote)
        });
        self.ended(wrote, how);
        self.window_ended();
    }

    /// The window's attempt ended: its tab's program, where it asked, ended with it.
    fn window_ended(&mut self) {
        if self.asks_in_tab {
            self.stop(PROGRAM, self.program);
        }
        self.win = None;
        self.asks_in_tab = false;
        self.orphan = false;
    }

    /// The terminal's attempt ends as its decision reads how its run ended (`exit`), inside
    /// `action`'s window, and its process with it.
    fn terminal_ends(&mut self, exit: TermExit, action: &'static str, ev: &mut StepEvidence) {
        let claim = self.term.clone().expect("the terminal asks");
        let (how, wrote) = ev.step(action, || {
            let how = term_end(exit);
            let wrote =
                end(&self.files, Self::path(), &claim, how, self.now).expect("the end is recorded");
            (how, wrote)
        });
        if exit
            == (TermExit::Unfinished {
                stop: Some(Stop::Interruption),
            })
            && how == End::Declined
        {
            self.cutno = true;
        }
        self.ended(wrote, how);
        self.terminal_ended();
    }

    /// The terminal's attempt ended, and its process with it.
    fn terminal_ended(&mut self) {
        self.stop(TERMINAL, self.terminal);
        self.term = None;
    }

    /// The person's attempt ended, and its process with it.
    fn person_ended(&mut self) {
        self.stop(PERSON, self.person);
        self.own = None;
    }

    /// aterm ends with the window and its tab's program: what the quit's hangup ends a program
    /// asking under a quit with is decided by the window, which writes no end for any of it.
    fn aterm_quits(&mut self) {
        if let Some(claim) = self.win.clone()
            && self.asks_in_tab
            && self.quitting
        {
            for exit in [
                Some(TabExit::Signal),
                None,
                Some(TabExit::Code(i32::from(
                    crate::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
                ))),
            ] {
                if let Some(how) = tab_end(exit, false, true) {
                    self.cutend = true;
                    self.end_as(&claim, how);
                }
            }
        }
        if self.asks_in_tab {
            self.stop(PROGRAM, self.program);
        }
        self.window_restarts();
        self.win = None;
        self.asks_in_tab = false;
        self.orphan = false;
        self.quitting = false;
    }

    /// An automatic claim by the process `pid` born `birth`.
    fn automatic(&mut self, pid: i32, birth: u64, action: &'static str, ev: &mut StepEvidence) {
        let declined = self.declined();
        let asker = Self::asker(pid, birth);
        let claimed = ev.step(action, || {
            claim(&self.files, Self::path(), &asker, self.now).expect("the claim is recorded")
        });
        let claimed = claimed.unwrap_or_else(|| panic!("{action} claims"));
        self.early |= declined;
        if pid == WINDOW {
            self.win = Some(claimed);
        } else {
            self.term = Some(claimed);
        }
    }

    /// The window hands its claim to its tab's program, which starts: the window keeps what
    /// the hand gives back, as the window does (`Mark::hand`), so a refused hand leaves it
    /// holding none while the program asks.
    fn hand_to_tab(&mut self, action: &'static str, ev: &mut StepEvidence) -> Option<Claim> {
        let claim = self.win.take().expect("the window asks");
        self.program += 1;
        self.start(PROGRAM, self.program);
        let program = self.program;
        let handed = ev.step(action, || {
            hand(&self.files, Self::path(), &claim, PROGRAM, program).expect("the hand is recorded")
        });
        self.asks_in_tab = true;
        self.orphan = handed.is_none();
        self.win.clone_from(&handed);
        handed
    }
}

/// The asker `asker` is, as the model names it: 1 the window or its tab's program, 2 the
/// terminal, 3 the person.
fn role(asker: &Asker) -> i64 {
    match asker.pid {
        WINDOW | PROGRAM => 1,
        TERMINAL => 2,
        PERSON => 3,
        other => panic!("no asker is pid {other}"),
    }
}

/// The kind of line `line` is, as the model numbers it.
fn kind(line: &Line) -> i64 {
    match line {
        Line::Asking { .. } => 1,
        Line::Unfinished { .. } => 2,
        Line::Done { .. } => 3,
        Line::Declined { .. } => 4,
        Line::Opened => panic!("the rig writes no historical line"),
    }
}

fn project(rig: &Rig, model_state: &State) -> State {
    let mut state = model_state.clone();
    let now = rig.now;
    let over = |due: u64| i64::from(due <= now);
    let runs = |claim: &Claim| i64::from(rig.files.runs(claim.asker.pid, claim.asker.birth));
    // When the line's own pause is over: a cut-off claim's, as it reads once its asker is gone.
    let due = |line: &Line| match line {
        Line::Asking {
            claim,
            tries,
            since,
            ..
        } => over(ASK_AGAIN.due_by(claim.at.max(*since), *tries, now)),
        Line::Unfinished { at, tries, .. } => over(ASK_AGAIN.due_by(*at, *tries, now)),
        Line::Done { at, .. } => over(ASK_AGAIN.due_by(*at, 1, now)),
        Line::Declined { at, declines, .. } => over(DECLINE_AGAIN.due_by(*at, *declines, now)),
        Line::Opened => panic!("the rig writes no historical line"),
    };
    // When the schedule of the unfinished tries an asking line carries is over.
    let schedule = |line: &Line| match line {
        Line::Asking { tries, since, .. } if *tries > 0 => {
            over(ASK_AGAIN.due_by(*since, *tries, now))
        }
        _ => 1,
    };
    let line = rig.line();
    let prior = match &line {
        Some(Line::Asking { prior, .. }) => prior.as_deref().cloned(),
        _ => None,
    };
    let asking = |line: Option<&Line>| match line {
        Some(Line::Asking { claim, .. }) => (
            role(&claim.asker),
            i64::from(claim.asker.pid == PROGRAM),
            runs(claim),
        ),
        _ => (0, 0, 0),
    };
    let (held, handed, live) = asking(line.as_ref());
    let (pheld, phanded, plive) = asking(prior.as_ref());
    let finished = line.as_ref().map_or(0, Line::finished);
    state.insert("rec", line.as_ref().map_or(0, kind));
    state.insert("held", held);
    state.insert("handed", handed);
    state.insert("live", live);
    state.insert(
        "win",
        match (&rig.win, rig.asks_in_tab, rig.orphan) {
            (None, true, true) => 3,
            (None, _, _) => 0,
            (Some(_), false, _) => 1,
            (Some(_), true, _) => 2,
        },
    );
    state.insert("term", i64::from(rig.term.is_some()));
    state.insert("own", i64::from(rig.own.is_some()));
    state.insert("quitting", i64::from(rig.quitting));
    state.insert(
        "tries",
        match &line {
            Some(Line::Asking { tries, .. } | Line::Unfinished { tries, .. }) => i64::from(*tries),
            _ => 0,
        },
    );
    state.insert("fin", i64::from(finished > 0));
    state.insert("prior", prior.as_ref().map_or(0, kind));
    state.insert("pheld", pheld);
    state.insert("phanded", phanded);
    state.insert("plive", plive);
    state.insert("due", line.as_ref().map_or(1, due));
    state.insert("pdue", prior.as_ref().map_or(1, due));
    state.insert(
        "fdue",
        if finished == 0 {
            1
        } else {
            over(ASK_AGAIN.due_by(finished, 1, now))
        },
    );
    state.insert("sd", line.as_ref().map_or(1, schedule));
    state.insert("psd", prior.as_ref().map_or(1, schedule));
    state.insert("said", i64::from(rig.said));
    state.insert("ends", rig.ends);
    state.insert("ever", i64::from(rig.ever));
    state.insert("early", i64::from(rig.early));
    state.insert("cutend", i64::from(rig.cutend));
    state.insert("cutno", i64::from(rig.cutno));
    state
}

/// How the terminal's run ended for each of its ends: a finish read back, a failure (a refused
/// password, a download), a person's key at its terminal, a `sudo` that never started, and its
/// hangup or a `SIGTERM`.
fn terminal_exit(action: &str) -> TermExit {
    match action {
        "DoneT" => TermExit::Finished { settled: true },
        "UnfinishedT" => TermExit::Unfinished { stop: None },
        "DeclinedT" => TermExit::Unfinished {
            stop: Some(Stop::Person),
        },
        "ReleasedT" => TermExit::NotStarted,
        "InterruptedT" => TermExit::Unfinished {
            stop: Some(Stop::Interruption),
        },
        other => panic!("`{other}` is no end of the terminal's"),
    }
}

/// The exit of the window's tab's program each window end is played with: its own status for
/// each, and none seen for a tab closed while it ran.
fn window_exit(action: &str) -> (Option<TabExit>, bool) {
    use crate::macos_setup::{COMMAND_LINE_TOOLS_TAB_NOT_STARTED, COMMAND_LINE_TOOLS_TAB_STOPPED};
    match action {
        "DoneW" => (Some(TabExit::Code(0)), false),
        "UnfinishedW" => (Some(TabExit::Code(1)), false),
        "DeclinedW" => (
            Some(TabExit::Code(i32::from(COMMAND_LINE_TOOLS_TAB_STOPPED))),
            false,
        ),
        "ClosedW" => (None, true),
        "ReleasedW" => (
            Some(TabExit::Code(i32::from(COMMAND_LINE_TOOLS_TAB_NOT_STARTED))),
            false,
        ),
        other => panic!("`{other}` is no end of the window's"),
    }
}

/// Play `action` on the real record, inside its evidence window where the record performs it.
fn drive(rig: &mut Rig, action: &'static str, ev: &mut StepEvidence) {
    rig.said = false;
    match action {
        "ClaimW" => {
            let window = rig.window;
            rig.automatic(WINDOW, window, action, ev);
        }
        "ClaimT" => {
            rig.terminal += 1;
            rig.start(TERMINAL, rig.terminal);
            let terminal = rig.terminal;
            rig.automatic(TERMINAL, terminal, action, ev);
        }
        "ExplicitO" => {
            rig.person += 1;
            rig.start(PERSON, rig.person);
            let asker = Rig::asker(PERSON, rig.person);
            let claimed = ev.step(action, || {
                explicit(&rig.files, Rig::path(), &asker, rig.now).expect("the retry claims")
            });
            rig.own = Some(claimed);
        }
        "Hand" => {
            let handed = rig
                .hand_to_tab(action, ev)
                .expect("the record names the program");
            assert_eq!(handed.asker.pid, PROGRAM);
        }
        "DoneW" | "UnfinishedW" | "DeclinedW" | "ClosedW" | "ReleasedW" => {
            let (exit, closed) = window_exit(action);
            rig.window_ends(exit, closed, action, ev);
        }
        "DoneT" | "UnfinishedT" | "DeclinedT" | "ReleasedT" | "InterruptedT" => {
            rig.terminal_ends(terminal_exit(action), action, ev);
        }
        "DoneO" | "UnfinishedO" | "DeclinedO" | "ReleasedO" => {
            let claim = rig.own.clone().expect("the person asks");
            let how = match action {
                "DoneO" => End::Done,
                "UnfinishedO" => End::Unfinished,
                "DeclinedO" => End::Declined,
                _ => End::Released,
            };
            ev.step(action, || rig.end_as(&claim, how));
            rig.person_ended();
        }
        // An update's handoff: the next window adopts its tab's program, which asks on.
        "WindowGoes" => {
            rig.window_restarts();
            if !rig.asks_in_tab {
                rig.win = None;
            }
        }
        "QuitNoted" => rig.quitting = true,
        "AtermQuits" => rig.aterm_quits(),
        "ProgramGoes" => {
            rig.stop(PROGRAM, rig.program);
            rig.win = None;
            rig.asks_in_tab = false;
            rig.orphan = false;
        }
        "TermGoes" => rig.terminal_ended(),
        "OwnGoes" => rig.person_ended(),
        "Tick" => rig.now += TICK,
        "Read" => {
            let read = ev.step(action, || reading(&rig.files, Rig::path(), rig.now));
            rig.said = matches!(read, Asked::Declined { .. });
        }
        "Lift" => {
            assert!(
                ev.step(action, || lift(&rig.files, Rig::path())
                    .expect("the lift is recorded")),
                "a decline to lift"
            );
        }
        other => panic!("no real step for `{other}`"),
    }
}

/// Run `step` with `defect` planted in the code under test on this thread.
fn planted<T>(defect: Defect, step: impl FnOnce() -> T) -> T {
    PLANTED.with(|planted| planted.set(Some(defect)));
    let out = step();
    PLANTED.with(|planted| planted.set(None));
    out
}

/// Each Buggy=1 mutant's step, played on the real record with its defect planted there.
fn mutant_step(rig: &mut Rig, action: &str, ev: &mut StepEvidence) -> bool {
    rig.said = false;
    match action {
        "CutOffReadsDeclined" => {
            let read = planted(Defect::CutOffReadsDeclined, || {
                reading(&rig.files, Rig::path(), rig.now)
            });
            rig.said = matches!(read, Asked::Declined { .. });
        }
        "ClaimBesideLive" | "ClaimThroughDecline" => {
            rig.terminal += 1;
            rig.start(TERMINAL, rig.terminal);
            let terminal = rig.terminal;
            let defect = if action == "ClaimBesideLive" {
                Defect::ClaimBesideLive
            } else {
                Defect::ClaimThroughDecline
            };
            planted(defect, || rig.automatic(TERMINAL, terminal, "ClaimT", ev));
        }
        "HandLeavesWindow" => {
            let handed = planted(Defect::HandLeavesWindow, || rig.hand_to_tab("Hand", ev));
            assert_eq!(handed.map(|claim| claim.asker.pid), Some(WINDOW));
        }
        "HandDropsClaim" => {
            let handed = planted(Defect::HandDropsClaim, || rig.hand_to_tab("Hand", ev));
            assert_eq!(handed, None, "the hand is refused");
        }
        "StaleEndWrites" | "ReleaseCountsATry" | "EndForgetsFinish" => {
            let (defect, step) = match action {
                "StaleEndWrites" => (Defect::StaleEndWrites, "UnfinishedW"),
                "ReleaseCountsATry" => (Defect::ReleaseCountsATry, "ReleasedW"),
                _ => (Defect::EndForgetsFinish, "UnfinishedW"),
            };
            let (exit, closed) = window_exit(step);
            planted(defect, || rig.window_ends(exit, closed, step, ev));
        }
        "RetryEndDropsAsk" => {
            let claim = rig.own.clone().expect("the person asks");
            planted(Defect::EndDropsRunningAsk, || {
                ev.step("DoneO", || rig.end_as(&claim, End::Done));
            });
            rig.person_ended();
        }
        "InterruptionDeclines" => planted(Defect::InterruptionDeclines, || rig.aterm_quits()),
        "HangupDeclinesT" => planted(Defect::HangupDeclines, || {
            rig.terminal_ends(terminal_exit("InterruptedT"), "InterruptedT", ev);
        }),
        "QuitDropsFinish" => {
            let quit = rig.quitting;
            let how = planted(Defect::QuitDropsOwnEnd, || {
                tab_end(Some(TabExit::Code(0)), false, quit)
            });
            assert_eq!(how, None, "the finish is dropped");
            rig.ever = true;
            rig.window_ended();
        }
        _ => return false,
    }
    true
}

/// Assert the real record, read as the model reads it, is the model's `state` after `action`.
fn conforms(rig: &Rig, state: &State, action: &str, path: &[&'static str]) {
    let real = project(rig, state);
    for var in PROJECTED {
        assert_eq!(
            real[var], state[var],
            "`{var}` after {action} in {path:?}: real {real:?}, model {state:?}"
        );
    }
}

/// Play `schedule` from the start on the model and the real record, with `mutant` playing the
/// steps it knows.
fn replay(
    model: &Model,
    schedule: &[&'static str],
    ev: &mut StepEvidence,
    mutant: &dyn Fn(&mut Rig, &str, &mut StepEvidence) -> bool,
) -> (Rig, State) {
    let mut rig = Rig::new();
    let mut state = model.init_state();
    conforms(&rig, &state, "the start", &[]);
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
        if !mutant(&mut rig, action, ev) {
            drive(&mut rig, action, ev);
        }
        conforms(&rig, &state, action, schedule);
    }
    let real = project(&rig, &state);
    (rig, real)
}

/// At `state`, each automatic claim and lift the model refuses is refused by the record, which
/// it leaves as it was.
fn refusals_conform(model: &Model, state: &State, rig: &Rig, path: &[&'static str]) {
    for (action, idle, pid) in [("ClaimW", "win", WINDOW), ("ClaimT", "term", TERMINAL)] {
        // A window whose quit is noted claims nothing: its own gate, not the record's.
        let busy = state[idle] != 0 || (pid == WINDOW && state["quitting"] != 0);
        if busy || !model.successors(action, state).is_empty() {
            continue;
        }
        let rig = rig.clone();
        let before = rig.files.text.borrow().clone();
        let birth = if pid == WINDOW {
            rig.window
        } else {
            rig.terminal + 1
        };
        let claimed = claim(&rig.files, Rig::path(), &Rig::asker(pid, birth), rig.now)
            .expect("a refusal is no error");
        assert_eq!(
            claimed, None,
            "{action} after {path:?}: the model refuses it at {state:?}"
        );
        assert_eq!(*rig.files.text.borrow(), before, "a refused {action}");
    }
    if model.successors("Lift", state).is_empty() {
        let rig = rig.clone();
        let before = rig.files.text.borrow().clone();
        assert!(
            !lift(&rig.files, Rig::path()).expect("a refusal is no error"),
            "Lift after {path:?}: the model refuses it at {state:?}"
        );
        assert_eq!(*rig.files.text.borrow(), before, "a refused Lift");
    }
}

/// The walk: every transition of every reachable state played on a copy of the real record
/// that reached it, with the invariants checked on the real state, every refusal driven, and the
/// anchors' evidence audited.
fn the_walk_conforms() {
    clt_episode_environment();
    let model = interp::with_buggy(&clt_episode_model(), 0);
    let mut ev = StepEvidence::new("CltEpisode");
    let mut reached: BTreeMap<State, Vec<&'static str>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    let start = Rig::new();
    let init = model.init_state();
    conforms(&start, &init, "the start", &[]);
    reached.insert(init.clone(), Vec::new());
    queue.push_back((init, start));
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    let mut transitions = 0;
    while let Some((state, rig)) = queue.pop_front() {
        let path = reached[&state].clone();
        refusals_conform(&model, &state, &rig, &path);
        for action in model.actions.iter().map(|a| a.name) {
            for next in model.successors(action, &state) {
                let mut after = rig.clone();
                let mut schedule = path.clone();
                schedule.push(action);
                drive(&mut after, action, &mut ev);
                conforms(&after, &next, action, &schedule);
                let real = project(&after, &next);
                for invariant in &model.invariants {
                    assert!(
                        model.check_invariant(invariant.name, &real),
                        "{} after {schedule:?}",
                        invariant.name
                    );
                }
                fired.insert(action);
                transitions += 1;
                if !reached.contains_key(&next) {
                    reached.insert(next.clone(), schedule);
                    queue.push_back((next, after));
                }
            }
        }
    }
    assert_eq!(
        reached.len(),
        interp::reachable_states(&model).len(),
        "the walk covers every reachable state ({transitions} transitions)"
    );
    let committed: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|name| !MUTANTS.contains(name))
        .collect();
    assert_eq!(fired, committed, "every committed action is driven");

    let audit = ev.audit();
    let actions: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(actions, ANCHORED.into_iter().collect::<BTreeSet<_>>());
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

#[test]
fn the_record_conforms_to_the_model() {
    the_walk_conforms();
}

#[test]
fn negative_control_each_mutant_breaks_its_invariant_on_the_real_record() {
    let buggy = interp::with_buggy(&clt_episode_model(), 1);
    let committed = interp::with_buggy(&clt_episode_model(), 0);
    let mut ev = StepEvidence::new("CltEpisode");
    let cases: [(&[&'static str], &str, &[&'static str]); 13] = [
        (
            &["ClaimW", "WindowGoes", "CutOffReadsDeclined"],
            "OnlyADeclineReadsDeclined",
            &["ClaimW", "WindowGoes", "Read"],
        ),
        (
            &["ClaimW", "ClaimBesideLive"],
            "OneAutomaticAsk",
            &["ClaimW", "WindowGoes", "ClaimT"],
        ),
        (
            &["ClaimW", "DeclinedW", "ClaimThroughDecline"],
            "ADeclineHolds",
            &["ClaimW", "DeclinedW", "Tick", "ClaimT"],
        ),
        (
            &["ClaimW", "HandLeavesWindow", "WindowGoes", "ClaimT"],
            "OneAutomaticAsk",
            &["ClaimW", "Hand", "WindowGoes", "Read"],
        ),
        // The stall the review found: a hand the record refused under a person's retry left the
        // window holding no claim, and the retry's release gave back a line naming the window
        // that nobody would end.
        (
            &["ClaimW", "ExplicitO", "HandDropsClaim", "ReleasedO"],
            "EveryNamedAskIsHeld",
            &["ClaimW", "ExplicitO", "Hand", "ReleasedO", "DoneW"],
        ),
        (
            &["ClaimW", "ExplicitO", "StaleEndWrites"],
            "EveryAskIsNamed",
            &["ClaimW", "ExplicitO", "Hand", "UnfinishedW"],
        ),
        // E1: a retry's end over a running automatic ask dropped it from the record, so a
        // second automatic ask went beside it.
        (
            &["ClaimW", "ExplicitO", "RetryEndDropsAsk", "Tick", "ClaimT"],
            "OneAutomaticAsk",
            &["ClaimW", "ExplicitO", "DoneO", "Tick", "Read"],
        ),
        (
            &["ClaimW", "ReleaseCountsATry"],
            "TriesCountUnfinishedEnds",
            &["ClaimW", "ReleasedW"],
        ),
        (
            &["ClaimW", "DoneW", "Tick", "ClaimW", "EndForgetsFinish"],
            "FinishOutlivesLaterTries",
            &["ClaimW", "DoneW", "Tick", "ClaimW", "UnfinishedW"],
        ),
        (
            &["ClaimW", "Hand", "QuitNoted", "InterruptionDeclines"],
            "AnInterruptionWritesNoEnd",
            &["ClaimW", "Hand", "QuitNoted", "AtermQuits", "Read"],
        ),
        // The window wrote nothing for any end at a quit: its program's own finish, read just
        // after the last window's close, was lost.
        (
            &["ClaimW", "Hand", "QuitNoted", "QuitDropsFinish"],
            "FinishOutlivesLaterTries",
            &["ClaimW", "Hand", "QuitNoted", "DoneW"],
        ),
        (
            &["ClaimW", "ExplicitO", "HandDropsClaim"],
            "EveryNamedAskIsHeld",
            &["ClaimW", "ExplicitO", "Hand"],
        ),
        // A terminal `aterm init` read every stop it caught as the person's decline: a logout's
        // SIGTERM or its terminal's hangup at the password prompt held the ask for a month.
        (
            &["ClaimT", "HangupDeclinesT"],
            "AHangupNeverDeclines",
            &["ClaimT", "InterruptedT", "Read"],
        ),
    ];
    let mutants: BTreeSet<&str> = cases
        .iter()
        .flat_map(|(schedule, _, _)| schedule.iter().copied())
        .filter(|action| MUTANTS.contains(action))
        .collect();
    assert_eq!(
        mutants,
        MUTANTS.into_iter().collect::<BTreeSet<_>>(),
        "each mutant is played"
    );
    for (mutant, invariant, shipping) in cases {
        let (_, real) = replay(&buggy, mutant, &mut ev, &mutant_step);
        assert!(
            !buggy.check_invariant(invariant, &real),
            "{mutant:?} breaks {invariant} on the real record: {real:?}"
        );
        let (_, real) = replay(&committed, shipping, &mut ev, &|_, _, _| false);
        assert!(
            committed.check_invariant(invariant, &real),
            "{shipping:?} keeps {invariant}: {real:?}"
        );
    }
}

/// The stall the review found, end to end on the real record: a person's typed `aterm init`
/// claimed over the window's claim before its tab's program started, and gave the record back
/// unused. Before, the hand was refused, the window kept no claim, and the record went back to
/// a line naming the live window, which nobody ended: every reading said another aterm was
/// asking for the window's whole life, and no automatic ask came. Now the line the retry kept
/// names the program, so the window's end is the record's.
/// NEGATIVE CONTROL: with the old hand planted (`HandDropsClaim`), the record reads asking
/// after the program's end.
#[test]
fn a_hand_under_a_persons_retry_keeps_the_attempt_the_windows_to_end() {
    for drops in [false, true] {
        let mut rig = Rig::new();
        let mut ev = StepEvidence::new("CltEpisode");
        drive(&mut rig, "ClaimW", &mut ev);
        drive(&mut rig, "ExplicitO", &mut ev);
        let handed = if drops {
            planted(Defect::HandDropsClaim, || rig.hand_to_tab("Hand", &mut ev))
        } else {
            rig.hand_to_tab("Hand", &mut ev)
        };
        assert_eq!(
            handed.is_some(),
            !drops,
            "the hand of the claim the retry keeps"
        );
        drive(&mut rig, "ReleasedO", &mut ev);
        // The program says its install ended unfinished, and goes.
        match rig.win.clone() {
            Some(claim) => {
                let how = tab_end(Some(TabExit::Code(1)), false, false).expect("an end");
                rig.end_as(&claim, how);
            }
            None => assert!(drops, "the window holds the handed claim"),
        }
        rig.window_ended();
        let read = reading(&rig.files, Rig::path(), rig.now);
        if drops {
            assert!(matches!(read, Asked::Asking), "CONTROL: stalled, {read:?}");
        } else {
            assert!(
                matches!(read, Asked::Unfinished { tries: 1, .. }),
                "asked again by itself: {read:?}"
            );
        }
    }
}
