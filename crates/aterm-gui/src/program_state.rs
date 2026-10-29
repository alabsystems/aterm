// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROGRAM'S OWN STATE (OSC 7501, the native harness contract §6).

use std::time::Instant;

use aterm_core::terminal::program_state::{
    ProgramApp, ProgramState, ProgramStateReport, ProgramStateWord,
};

/// One edge of a codex's turn that no shown move claims ([`ProgramStateGate::observe_fed`]), with
/// the wall clock of the parse batch it arrived in (epoch ms; `None` where that batch read none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FedEdge {
    /// A turn began.
    Began(Option<u64>),
    /// A turn ended at this rest (`idle`, `done` or `error`).
    Ended(ProgramStateWord, Option<u64>),
}

/// One session's gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramStateGate {
    /// A sweep has read the engine; the first reading is a baseline ([`Self::observe_fed`] feeds
    /// none of the edges it finds).
    read: bool,
    /// The engine sequence the last sweep saw.
    seen_seq: u64,
    /// Reports at or below this sequence were already seen before the latest
    /// observed foreground move: none of them shows.
    floor_seq: u64,
    /// The report shown, and since when.
    shown: Option<(ProgramState, Instant)>,
    /// Where the report shown began in the engine (`ProgramStateReport::began`): a `blocked`
    /// report that began anew is a new box though its words are the same.
    shown_began: Option<u64>,
    /// When the report shown began on the clock that counts sleep
    /// (`aterm_uds::process::continuous_ms`), which the survey's `agent_ms=`
    /// runs on too: `program_state_ms=` ([`Self::shown_age_ms`]).
    shown_continuous_ms: Option<u64>,
    /// How many shown `working` reports were followed by a shown `done` or
    /// `idle`: the program's own turn ends, which presence tells as a story
    /// point and, where nobody watched, as the tab's `•`
    /// (`Facts::program_turn_ends`).
    turn_ends: u64,
    /// How many shown `working` reports began a turn ([`starts_a_turn`]):
    /// what pairs one turn's end with the ledger settle of the same turn, so
    /// presence tells it once (`Facts::program_turn_starts`).
    turn_starts: u64,
    /// When the gate counted the latest of those starts.
    turn_started: Option<Instant>,
    /// The engine's count of turns begun (`ProgramStateReport::turns`) at the
    /// last sweep's reading, honoured or not.
    turns: u64,
    /// How many times the report shown moved ([`Self::observe`] returning true): what the tab
    /// hover's cached activity line keys on (`App::session_status_revision`).
    moves: u64,
}

impl ProgramStateGate {
    /// Fold one sweep's reading.
    pub(crate) fn observe(
        &mut self,
        report: ProgramStateReport,
        honoured: bool,
        group_moved: bool,
        now: Instant,
    ) -> bool {
        self.read = true;
        self.turns = report.turns;
        if group_moved {
            // Discard reports already seen under the previous group. An
            // unseen report remains eligible; its writer is not attributable.
            self.floor_seq = self.seen_seq;
        }
        self.seen_seq = report.seq;
        let next = report
            .state
            .filter(|_| honoured && report.seq > self.floor_seq);
        // A box, then a `working` this sweep missed, then the next box: the same words, a new box.
        let new_box = next.is_some_and(|state| {
            state.state == aterm_core::terminal::program_state::ProgramStateWord::Blocked
                && self.shown_began != Some(report.began)
        });
        if self.shown.map(|(state, _)| state) == next
            && (!group_moved || next.is_none())
            && !new_box
        {
            return false;
        }
        let before = if group_moved {
            None
        } else {
            self.shown.map(|(state, _)| state)
        };
        if ends_a_turn(before, next) {
            self.turn_ends += 1;
        }
        if starts_a_turn(before, next) {
            self.turn_starts += 1;
            self.turn_started = Some(now);
        }
        self.shown = next.map(|state| (state, now));
        self.shown_began = next.map(|_| report.began);
        self.shown_continuous_ms = next.and_then(|_| aterm_uds::process::continuous_ms());
        self.moves += 1;
        true
    }

    /// Fold one sweep's reading as [`Self::observe`] does, and answer with it the edges of a
    /// codex's turns the engine kept (`ProgramStateReport::turn_edges`) that no shown move claims,
    /// oldest first: each beginning and end past the last reading, an end only for a turn begun
    /// after the latest foreground move (one begun at or before it, or one kept with no beginning
    /// once a move was seen, is the run's that left), and not the beginning this reading's own
    /// move shows, which its program mark claims. None is fed
    /// on the first reading (a baseline), on a foreground move's reading (unseen output then is
    /// not attributable to its writer), with no codex in front (`codex_front`), or from a report
    /// whose sequence went back.
    pub(crate) fn observe_fed(
        &mut self,
        report: ProgramStateReport,
        honoured: bool,
        codex_front: bool,
        group_moved: bool,
        now: Instant,
    ) -> (bool, Vec<FedEdge>) {
        let mut edges = Vec::new();
        // An edge's sequence is at most its report's, so a report whose sequence went back (another
        // engine) has none past the last reading.
        if self.read && !group_moved && codex_front {
            for turn in report.turn_edges().filter(|t| t.app == ProgramApp::Codex) {
                if let Some(began) = turn.began.filter(|b| b.seq > self.seen_seq) {
                    edges.push(FedEdge::Began(began.wall_ms));
                }
                let after_move = turn
                    .began
                    .map_or(self.floor_seq == 0, |b| b.seq > self.floor_seq);
                if let Some((word, ended)) = turn.ended.filter(|(_, e)| e.seq > self.seen_seq)
                    && after_move
                {
                    edges.push(FedEdge::Ended(word, ended.wall_ms));
                }
            }
        }
        let starts = self.turn_starts;
        let moved = self.observe(report, honoured, group_moved, now);
        // The beginning this move shows is its mark's. Only the running turn's beginning comes
        // last: an end's sequence is above its beginning's.
        if self.turn_starts > starts && matches!(edges.last(), Some(FedEdge::Began(_))) {
            edges.pop();
        }
        (moved, edges)
    }

    /// How many times the report shown has moved.
    pub(crate) const fn moves(&self) -> u64 {
        self.moves
    }

    /// How long the report shown has stood (`status program_state_ms=`), counted through sleep
    /// as the survey's `agent_ms=` is; the sweep's clock where this host has no such clock.
    /// `status`'s own `agent_since_ms=` and `human_ms=` stop while the Mac sleeps, so across a
    /// sleep this age is not comparable with them. `None` while none is shown.
    pub(crate) fn shown_age_ms(&self) -> Option<u64> {
        let (_, since) = self.shown?;
        Some(
            match (
                self.shown_continuous_ms,
                aterm_uds::process::continuous_ms(),
            ) {
                (Some(at), Some(now)) => now.saturating_sub(at),
                _ => u64::try_from(Instant::now().saturating_duration_since(since).as_millis())
                    .unwrap_or(u64::MAX),
            },
        )
    }

    /// When the latest turn the gate counted began ([`Self::turn_starts`]), on the sweep's clock.
    pub(crate) const fn turn_started(&self) -> Option<Instant> {
        self.turn_started
    }

    /// How many turn ends this session's program has reported ([`ends_a_turn`]), as shown.
    pub(crate) const fn turn_ends(&self) -> u64 {
        self.turn_ends
    }

    /// How many turns this session's program has begun ([`starts_a_turn`]), as shown.
    pub(crate) const fn turn_starts(&self) -> u64 {
        self.turn_starts
    }

    /// The report shown, and since when.
    pub(crate) const fn shown(&self) -> Option<(ProgramState, Instant)> {
        self.shown
    }

    /// The engine's count of turns begun as the last sweep read it, whatever the gate shows.
    pub(crate) const fn turns(&self) -> u64 {
        self.turns
    }
}

/// Whether the shown report moving from `before` to `after` is the program
/// finishing a turn: `working`, then `done` or `idle`.
fn ends_a_turn(before: Option<ProgramState>, after: Option<ProgramState>) -> bool {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    matches!(
        (before.map(|s| s.state), after.map(|s| s.state)),
        (Some(W::Working), Some(W::Done | W::Idle))
    )
}

/// Whether the shown report moving from `before` to `after` is the program
/// beginning a turn: entering a turn in progress (`working` or `blocked`)
/// from no report, `idle`, `done` or `error`.
fn starts_a_turn(before: Option<ProgramState>, after: Option<ProgramState>) -> bool {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let in_turn = |state: Option<ProgramState>| {
        matches!(state.map(|s| s.state), Some(W::Working | W::Blocked))
    };
    in_turn(after) && !in_turn(before)
}

/// Whether a report naming `app` is honoured under the program in front, its published name
/// (`program=`): a `codex` report under a codex (or a script runtime one runs under, as an npm
/// install does), a `grok-build` report under a Grok Build, and nothing else. A report is the
/// claim of the program it names, so one never stands for another program in front.
pub(crate) fn honoured_for(program: Option<&str>, app: Option<ProgramApp>) -> bool {
    use aterm_phase::Program;
    let (Some(name), Some(app)) = (program, app) else {
        return false;
    };
    match (aterm_phase::program_of(name), app) {
        (Some(Program::Codex), ProgramApp::Codex)
        | (Some(Program::Grok), ProgramApp::GrokBuild) => true,
        (None, ProgramApp::Codex) => aterm_phase::may_host_agent(name),
        (
            Some(
                Program::Claude
                | Program::Codex
                | Program::Grok
                | Program::Kimi
                | Program::Generic
                | Program::Agy
                | Program::Muse,
            )
            | None,
            ProgramApp::Codex | ProgramApp::GrokBuild,
        ) => false,
    }
}

/// `status program_state=<state>[:<kind>]`, `-` while none is shown.
pub(crate) fn wire(shown: Option<(ProgramState, Instant)>) -> String {
    shown.map_or_else(|| "-".to_string(), |(state, _)| state.wire())
}

/// `status program_state=<state>[:<kind>] program_state_ms=<ms>`, both `-` while none is
/// shown: the gate's report and its age, read from the one gate so they agree.
pub(crate) fn status_fields(gate: Option<&ProgramStateGate>) -> String {
    let shown = gate.and_then(ProgramStateGate::shown);
    let age = gate
        .and_then(ProgramStateGate::shown_age_ms)
        .map_or_else(|| "-".to_string(), |ms| ms.to_string());
    format!("program_state={} program_state_ms={age}", wire(shown))
}

/// The `program_state=` column of `sessions`, `task` and `await task`: the report the
/// timeline holds as shown ([`crate::session_timeline::SessionTimeline::program_shown`], which
/// every gate move sets in the same sweep), `-` while none is. A claim in its own column:
/// nothing that decides reads it, and it never moves `agent=`.
pub(crate) fn column(shown: Option<crate::session_timeline::ShownProgram>) -> String {
    shown.map_or_else(|| "-".to_string(), |shown| shown.state.wire())
}

/// The hover's word for a shown report (`presence::state_line`), when the screen's reader names no
/// phase: the program the report names, then its state.
pub(crate) const fn row_word(state: ProgramState) -> &'static str {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    match (state.app, state.state) {
        (ProgramApp::Codex, W::Idle) => "codex idle",
        (ProgramApp::Codex, W::Working) => "codex working",
        (ProgramApp::Codex, W::Blocked) => "codex blocked",
        (ProgramApp::Codex, W::Done) => "codex done",
        (ProgramApp::Codex, W::Error) => "codex error",
        (ProgramApp::GrokBuild, W::Idle) => "grok idle",
        (ProgramApp::GrokBuild, W::Working) => "grok working",
        (ProgramApp::GrokBuild, W::Blocked) => "grok blocked",
        (ProgramApp::GrokBuild, W::Done) => "grok done",
        (ProgramApp::GrokBuild, W::Error) => "grok error",
    }
}

/// The survey's agent word for a shown report, when the screen's reader names no phase
/// (`agent=unknown`): `working` is `busy`; `blocked` is `question` on a question, `wall:auth` on a
/// login and `prompt` otherwise; `idle`, `done` and `error` (a turn that ended) are `idle`.
pub(crate) const fn survey_word(state: ProgramState) -> &'static str {
    survey_word_of(state.state, state.kind)
}

const fn survey_word_of(
    state: aterm_core::terminal::program_state::ProgramStateWord,
    kind: Option<aterm_core::terminal::program_state::ProgramBlockedKind>,
) -> &'static str {
    use aterm_core::terminal::program_state::{ProgramBlockedKind as K, ProgramStateWord as W};
    match (state, kind) {
        (W::Working, _) => "busy",
        (W::Blocked, Some(K::Question)) => "question",
        (W::Blocked, Some(K::Auth)) => "wall:auth",
        (W::Blocked, Some(K::Permission) | None) => "prompt",
        (W::Idle | W::Done | W::Error, _) => "idle",
    }
}

/// [`survey_word`] of a `program_state=` column (`<state>[:<kind>]`, [`ProgramState::wire`]), as
/// another aterm's `sessions` line carries it: `None` for `-` or a word this build does not know.
pub(crate) fn survey_word_of_wire(wire: &str) -> Option<&'static str> {
    use aterm_core::terminal::program_state::{ProgramBlockedKind as K, ProgramStateWord as W};
    let (state, kind) = wire
        .split_once(':')
        .map_or((wire, None), |(state, kind)| (state, Some(kind)));
    let state = [W::Idle, W::Working, W::Blocked, W::Done, W::Error]
        .into_iter()
        .find(|word| word.as_str() == state)?;
    let kind = match kind {
        Some(kind) => Some(
            [K::Permission, K::Question, K::Auth]
                .into_iter()
                .find(|word| word.as_str() == kind)?,
        ),
        None => None,
    };
    Some(survey_word_of(state, kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::terminal::Terminal;
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};

    const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
    const BLOCKED_PERMISSION: &[u8] = b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\";
    const CLEAR: &[u8] = b"\x1b]7501;state=clear\x1b\\";

    fn shown(gate: &ProgramStateGate) -> Option<String> {
        gate.shown().map(|(s, _)| s.wire())
    }

    /// The real engine fed the bytes codex writes, read as the sweep reads it.
    #[test]
    fn a_codex_in_front_shows_its_report_and_a_clear_takes_it_away() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        assert!(!gate.observe(term.program_state(), true, false, t0));
        term.process(WORKING);
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert_eq!(shown(&gate).as_deref(), Some("working"));
        assert_eq!(wire(gate.shown()), "working");
        assert!(!gate.observe(term.program_state(), true, false, t0));
        term.process(BLOCKED_PERMISSION);
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert_eq!(wire(gate.shown()), "blocked:permission");
        term.process(CLEAR);
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert_eq!(wire(gate.shown()), "-");
    }

    /// Negative control: the same bytes under a program in front that is not
    /// `codex` show nothing.
    #[test]
    fn a_report_shows_only_while_a_codex_is_in_front() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        term.process(WORKING);
        assert!(!gate.observe(term.program_state(), false, false, t0));
        assert_eq!(shown(&gate), None);
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert_eq!(shown(&gate).as_deref(), Some("working"));
        assert!(gate.observe(term.program_state(), false, false, t0));
        assert_eq!(shown(&gate), None);
    }

    /// A foreground move clears it: the old program's last report never shows
    /// again, while the next program's first one (made before the sweep saw
    /// the move) does.
    #[test]
    fn a_foreground_move_clears_it_and_the_next_programs_report_shows() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        term.process(WORKING);
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert!(gate.observe(term.program_state(), true, true, t0));
        assert_eq!(shown(&gate), None, "the exited program's report is gone");
        assert!(!gate.observe(term.program_state(), true, false, t0));
        term.process(b"\x1b]7501;state=idle:app=codex\x1b\\");
        assert!(gate.observe(term.program_state(), true, true, t0));
        assert_eq!(shown(&gate).as_deref(), Some("idle"));
        let next_start = t0 + std::time::Duration::from_secs(1);
        term.process(b"\x1b]7501;state=idle:app=codex\x1b\\");
        assert!(gate.observe(term.program_state(), true, true, next_start));
        assert_eq!(gate.shown().map(|(_, since)| since), Some(next_start));
        assert!(!gate.observe(term.program_state(), true, false, next_start));
    }

    /// A foreground move judges the next report from no report: codex A exits
    /// mid-turn and codex B is already working when the sweep sees the move,
    /// and B's turn begins (its end pairs with its own start); B's first report
    /// a `done` after A was working ends nothing.
    #[test]
    fn a_group_move_judges_the_next_report_from_none() {
        let mut term = Terminal::new(24, 80);
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_secs(1);
        let done = b"\x1b]7501;state=done:app=codex\x1b\\";
        let mut gate = ProgramStateGate::default();
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!((gate.turn_starts(), gate.turn_started()), (1, Some(t0)));
        term.process(WORKING);
        assert!(gate.observe(term.program_state(), true, true, t1));
        assert_eq!(
            gate.turn_starts(),
            2,
            "B was working when the move was seen"
        );
        assert_eq!(gate.turn_started(), Some(t1), "B's own start");
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t1);
        assert_eq!(gate.turn_starts(), 2, "control: no move, the same turn");
        term.process(done);
        assert!(gate.observe(term.program_state(), true, true, t1));
        assert_eq!(gate.turn_ends(), 0, "A's turn did not end in B's report");
        let mut still = ProgramStateGate::default();
        term.process(WORKING);
        still.observe(term.program_state(), true, false, t0);
        term.process(done);
        still.observe(term.program_state(), true, false, t0);
        assert_eq!(still.turn_ends(), 1, "control: no move, a turn end");
    }

    /// A NEW BOX IS A MOVE: a `blocked` report, then a `working` and a second `blocked` with the
    /// same words between two sweeps, moves the gate (the second box began anew); a re-send of
    /// the box shown does not. Neither counts a turn the gate did not see.
    #[test]
    fn a_second_box_between_two_sweeps_is_a_move() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        term.process(WORKING);
        assert!(gate.observe(term.program_state(), true, false, t0));
        term.process(BLOCKED_PERMISSION);
        assert!(gate.observe(term.program_state(), true, false, t0));
        let turns = (gate.turn_starts(), gate.turn_ends());
        term.process(BLOCKED_PERMISSION);
        assert!(
            !gate.observe(term.program_state(), true, false, t0),
            "CONTROL: the box re-sent is no move"
        );
        term.process(&[WORKING, BLOCKED_PERMISSION].concat());
        assert!(
            gate.observe(term.program_state(), true, false, t0),
            "the next box is a move"
        );
        assert_eq!(shown(&gate).as_deref(), Some("blocked:permission"));
        assert_eq!((gate.turn_starts(), gate.turn_ends()), turns);
        assert!(!gate.observe(term.program_state(), true, false, t0));
    }

    const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";
    /// The wall clock the edge tests' parse batches read, `ms` into the test.
    const WALL0: u64 = 1_760_000_000_000;

    /// `bytes` into `term` in one parse batch whose wall clock reads `ms` into the test.
    fn says_at(term: &mut Terminal, bytes: &[u8], ms: u64) {
        term.process_at(
            bytes,
            aterm_core::terminal::ClockReading {
                wall_ms: Some(WALL0 + ms),
                ..aterm_core::terminal::ClockReading::now()
            },
        );
    }

    /// One codex in front's reading, honoured as the sweep judges it (`honoured_for` of the report
    /// standing), with no foreground move.
    fn fed(gate: &mut ProgramStateGate, term: &Terminal, t: Instant) -> (bool, Vec<FedEdge>) {
        let report = term.program_state();
        let honoured = honoured_for(Some("codex"), report.state.map(|s| s.app));
        gate.observe_fed(report, honoured, true, false, t)
    }

    /// A TURN BETWEEN TWO READINGS IS FED AND SHOWS NOTHING: a codex `working` at 10 ms and its
    /// `done` at 27 ms, both after a reading of `done`, are fed as that turn's beginning and end at
    /// their batches' wall clocks, while the gate shows no move and counts no turn (the turn
    /// counts are the shown moves'); the next reading feeds them no more. The first reading is a
    /// baseline: the turn before it is fed by none.
    #[test]
    fn a_turn_between_two_readings_is_fed_and_shows_nothing() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        says_at(&mut term, WORKING, 0);
        says_at(&mut term, DONE, 5);
        assert_eq!(
            fed(&mut gate, &term, t0),
            (true, vec![]),
            "the first reading is a baseline"
        );
        says_at(&mut term, WORKING, 10);
        says_at(&mut term, DONE, 27);
        assert_eq!(
            fed(&mut gate, &term, t0),
            (
                false,
                vec![
                    FedEdge::Began(Some(WALL0 + 10)),
                    FedEdge::Ended(ProgramStateWord::Done, Some(WALL0 + 27)),
                ]
            )
        );
        assert_eq!((gate.turn_starts(), gate.turn_ends()), (0, 0));
        assert_eq!(fed(&mut gate, &term, t0), (false, vec![]), "fed once");
    }

    /// THE BEGINNING A SHOWN MOVE STARTS IS ITS MARK'S: a `working` shown after `done` feeds
    /// nothing (its mark claims the turn); a `done` and the next `working` read together under a
    /// shown `working` feed the end and that beginning, the gate showing no move; and a turn whole
    /// and the next `working` read together under a shown `done` feed the whole turn, not the
    /// beginning the move shows. Every end is fed, the one a move shows too.
    #[test]
    fn the_begin_a_shown_move_starts_is_the_marks() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        says_at(&mut term, DONE, 0);
        assert_eq!(fed(&mut gate, &term, t0), (true, vec![]));
        says_at(&mut term, WORKING, 10);
        assert_eq!(fed(&mut gate, &term, t0), (true, vec![]), "the mark's");
        assert_eq!(gate.turn_starts(), 1);
        says_at(&mut term, DONE, 20);
        says_at(&mut term, WORKING, 30);
        assert_eq!(
            fed(&mut gate, &term, t0),
            (
                false,
                vec![
                    FedEdge::Ended(ProgramStateWord::Done, Some(WALL0 + 20)),
                    FedEdge::Began(Some(WALL0 + 30)),
                ]
            ),
            "no move shows the beginning"
        );
        says_at(&mut term, DONE, 40);
        assert_eq!(
            fed(&mut gate, &term, t0),
            (
                true,
                vec![FedEdge::Ended(ProgramStateWord::Done, Some(WALL0 + 40))]
            ),
            "a shown end is fed too"
        );
        says_at(&mut term, WORKING, 50);
        says_at(&mut term, DONE, 60);
        says_at(&mut term, WORKING, 70);
        assert_eq!(
            fed(&mut gate, &term, t0),
            (
                true,
                vec![
                    FedEdge::Began(Some(WALL0 + 50)),
                    FedEdge::Ended(ProgramStateWord::Done, Some(WALL0 + 60)),
                ]
            )
        );
        assert_eq!(gate.turn_starts(), 2);
    }

    /// NONE IS FED ON A MOVE, A FIRST READING, FOR A TURN BEGUN BEFORE A MOVE, OR WITH NO CODEX
    /// IN FRONT: a whole turn read on the gate's first reading or on a foreground move's reading
    /// feeds nothing (unseen output then is not attributable to its writer); after a move, the end
    /// of the turn that was running at it is the run's that left, while a turn begun after the move
    /// is fed; a reading with no codex in front, a Grok Build's own turn under a codex, and a
    /// report whose sequence went back (another engine) feed nothing.
    #[test]
    fn a_move_a_first_reading_and_a_turn_begun_before_a_move_feed_nothing() {
        let t0 = Instant::now();
        let none: Vec<FedEdge> = Vec::new();
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        says_at(&mut term, WORKING, 0);
        says_at(&mut term, DONE, 5);
        assert_eq!(fed(&mut gate, &term, t0).1, none, "a first reading");
        says_at(&mut term, WORKING, 10);
        says_at(&mut term, DONE, 20);
        let (_, edges) = gate.observe_fed(term.program_state(), true, true, true, t0);
        assert_eq!(edges, none, "a move's reading");

        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        says_at(&mut term, WORKING, 0);
        fed(&mut gate, &term, t0);
        let (_, edges) = gate.observe_fed(term.program_state(), true, true, true, t0);
        assert_eq!(edges, none);
        says_at(&mut term, DONE, 30);
        assert_eq!(
            fed(&mut gate, &term, t0).1,
            none,
            "the turn running at the move is the run's that left"
        );
        says_at(&mut term, WORKING, 40);
        says_at(&mut term, DONE, 50);
        assert_eq!(
            fed(&mut gate, &term, t0).1,
            [
                FedEdge::Began(Some(WALL0 + 40)),
                FedEdge::Ended(ProgramStateWord::Done, Some(WALL0 + 50)),
            ],
            "CONTROL: a turn begun after the move"
        );
        says_at(&mut term, WORKING, 60);
        says_at(&mut term, DONE, 70);
        let (_, edges) = gate.observe_fed(term.program_state(), false, false, false, t0);
        assert_eq!(edges, none, "no codex in front");
        says_at(
            &mut term,
            b"\x1b]7501;state=working:app=grok-build\x1b\\",
            80,
        );
        says_at(&mut term, b"\x1b]7501;state=done:app=grok-build\x1b\\", 90);
        assert_eq!(fed(&mut gate, &term, t0).1, none, "a Grok Build's turn");
        let mut other = Terminal::new(24, 80);
        says_at(&mut other, WORKING, 100);
        says_at(&mut other, DONE, 110);
        assert_eq!(fed(&mut gate, &other, t0).1, none, "a sequence gone back");
    }

    /// A turn end is `working` then `done`|`idle` as the gate showed them:
    /// not a `done` with no `working` before it, not `working` → `blocked`,
    /// and not a report the gate does not honour.
    #[test]
    fn only_working_then_done_or_idle_counts_as_a_turn_end() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        let done = b"\x1b]7501;state=done:app=codex\x1b\\";
        let idle = b"\x1b]7501;state=idle:app=codex\x1b\\";
        term.process(done);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_ends(), 0, "done with no working before it");
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        term.process(BLOCKED_PERMISSION);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_ends(), 0, "working then blocked");
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        term.process(done);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_ends(), 1);
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        term.process(idle);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_ends(), 2);
        assert_eq!(gate.turn_starts(), 2);
        term.process(BLOCKED_PERMISSION);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_starts(), 3, "idle then blocked");
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_starts(), 3, "blocked then working resumes it");
        term.process(done);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_ends(), 3);
        let mut other = ProgramStateGate::default();
        for bytes in [WORKING, done.as_slice()] {
            term.process(bytes);
            other.observe(term.program_state(), false, false, t0);
        }
        assert_eq!(other.turn_ends(), 0);
        assert_eq!(other.turn_starts(), 0);
    }

    /// GROK BUILD'S OWN REPORT THROUGH THE REAL SWEEP (`App::observe_session_statuses`): a real
    /// engine fed the bytes Grok Build 1.0.70 writes (its root record, a subagent's child record
    /// beside it, and its exit's `clear`) under a published `grok` is `status program_state=` and
    /// the hover's `grok …` word; a child record moves neither.
    /// NEGATIVE CONTROLS: the same bytes under a published `codex` show nothing (a report never
    /// stands for another program), and so do codex's own bytes under a published `grok`.
    #[test]
    fn grok_builds_own_report_is_shown_only_under_a_grok_in_front() {
        const G_WORKING: &[u8] = b"\x1b]7501;state=working:app=grok-build:title=cGFyc2Vy\x1b\\";
        const G_BLOCKED: &[u8] =
            b"\x1b]7501;state=blocked:kind=permission:app=grok-build:title=cGFyc2Vy\x1b\\";
        const G_CHILD_DONE: &[u8] = b"\x1b]7501;state=done:app=grok-build:id=sub_1\x1b\\";
        const G_CHILD_CLEAR: &[u8] = b"\x1b]7501;state=clear:id=sub_1\x1b\\";
        const G_DONE: &[u8] = b"\x1b]7501;state=done:app=grok-build:title=cGFyc2Vy\x1b\\";
        let shown_through = |program: &str, stream: &[&[u8]]| -> Vec<String> {
            let mut app = crate::App::headless_for_test();
            let sid = app.next_session_id;
            app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
            let (term, timeline) = {
                let pooled = app.pool.get(sid).expect("pooled");
                (
                    std::sync::Arc::clone(&pooled.term),
                    std::sync::Arc::clone(&pooled.ctx.timeline),
                )
            };
            {
                let mut tl = timeline.lock().unwrap();
                tl.note_foreground_group(4242);
                tl.set_program(4242, Some(program.into()));
            }
            let step = std::time::Duration::from_secs(1);
            let mut t = Instant::now();
            let mut seen = Vec::new();
            for bytes in stream {
                crate::term_lock(&term).process(bytes);
                let _ = app.observe_session_statuses(t);
                t += step;
                let shown = app.session_status.program_state(sid);
                seen.push(format!(
                    "{} {}",
                    wire(shown),
                    shown.map_or("-", |(state, _)| row_word(state))
                ));
            }
            seen
        };
        let grok = [
            G_WORKING,
            G_CHILD_DONE,
            G_BLOCKED,
            G_CHILD_CLEAR,
            G_WORKING,
            G_DONE,
            CLEAR,
        ];
        assert_eq!(
            shown_through("grok", &grok),
            [
                "working grok working",
                "working grok working",
                "blocked:permission grok blocked",
                "blocked:permission grok blocked",
                "working grok working",
                "done grok done",
                "- -",
            ]
        );
        assert!(
            shown_through("codex", &grok).iter().all(|s| s == "- -"),
            "control: Grok Build's report under a codex"
        );
        assert!(
            shown_through("grok", &[WORKING, BLOCKED_PERMISSION, CLEAR])
                .iter()
                .all(|s| s == "- -"),
            "control: codex's report under a Grok Build"
        );
        assert_eq!(
            shown_through("codex", &[WORKING, CLEAR]),
            ["working codex working", "- -"],
            "control: codex's under a codex still shows"
        );
    }

    /// THE TURN LEDGER AND THE `done` EDGE THROUGH THE REAL SWEEP
    /// (`App::observe_session_statuses`), a real engine fed the bytes codex
    /// writes under a published `codex`: each honoured move is one
    /// display-only `program-state` row, in order, and the first `done`
    /// fires an `fx celebrate on=done` arm bound to the session, once (a
    /// later `done` finds it spent).
    #[test]
    fn each_honoured_move_is_a_program_state_row_and_done_fires_an_on_done_arm() {
        use crate::session_timeline::PROGRAM_STATE;
        use aterm_effects::kitty_sing::{CelebrateOn, song_signature};
        const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";
        for codex in [true, false] {
            let mut app = crate::App::headless_for_test();
            app.config = aterm_toml::from_str(
                "cursor_trail = true\ncursor_trail_style = \"rainbow kitty pet\"",
            )
            .expect("valid toml");
            let sid = app.next_session_id;
            app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
            let (term, timeline) = {
                let pooled = app.pool.get(sid).expect("pooled");
                (
                    std::sync::Arc::clone(&pooled.term),
                    std::sync::Arc::clone(&pooled.ctx.timeline),
                )
            };
            {
                let mut tl = timeline.lock().unwrap();
                tl.note_foreground_group(4242);
                tl.set_program(4242, Some(if codex { "codex" } else { "claude" }.into()));
            }
            let armed = app
                .fx_control(crate::FxCtlOp::Celebrate {
                    sig: song_signature('C'),
                    bars: 1,
                    on: CelebrateOn::Done,
                })
                .expect("admitted");
            assert!(armed.starts_with("armed=done "), "{armed}");
            assert!(
                app.fx_control(crate::FxCtlOp::Celebrate {
                    sig: song_signature('C'),
                    bars: 1,
                    on: CelebrateOn::Done,
                })
                .is_err(),
                "a second arm is refused"
            );
            let step = std::time::Duration::from_secs(1);
            let mut t = std::time::Instant::now();
            let mut after: Vec<String> = Vec::new();
            for bytes in [
                WORKING,
                BLOCKED_PERMISSION,
                WORKING,
                DONE,
                WORKING,
                DONE,
                CLEAR,
            ] {
                crate::term_lock(&term).process(bytes);
                let _ = app.observe_session_statuses(t);
                t += step;
                after.push(app.fx_control(crate::FxCtlOp::Status).expect("reads"));
            }
            let waiting = |s: &String| s.starts_with("armed=done ") && s.contains(" live=false ");
            let fired = |s: &String| s.starts_with("armed=none ") && s.contains(" live=true ");
            let rows: Vec<String> = timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == PROGRAM_STATE)
                .map(|e| e.payload.to_string())
                .collect();
            let status = app.fx_control(crate::FxCtlOp::Status).expect("reads");
            if codex {
                assert_eq!(
                    rows,
                    [
                        "state=working kind=-",
                        "state=blocked kind=permission",
                        "state=working kind=-",
                        "state=done kind=-",
                        "state=working kind=-",
                        "state=done kind=-",
                        "state=- kind=-",
                    ],
                );
                assert!(
                    status.starts_with("armed=none ") && status.contains(" live=true "),
                    "the first done fired the arm: {status}"
                );
                assert!(after[..3].iter().all(waiting), "{after:#?}");
                assert!(after[3..].iter().all(fired), "{after:#?}");
            } else {
                assert!(after.iter().all(waiting), "control: {after:#?}");
                assert!(rows.is_empty(), "control: no row under claude: {rows:?}");
                assert!(
                    status.starts_with("armed=done ") && status.contains(" live=false "),
                    "control: the arm still waits: {status}"
                );
            }
        }
    }

    #[test]
    fn a_full_reset_clears_it() {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        term.process(b"\x1bc");
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert_eq!(shown(&gate), None);
    }

    /// A REPORT IS HONOURED ONLY UNDER THE PROGRAM IT NAMES: a `codex` report under a codex or a
    /// runtime one runs under, a `grok-build` report under a Grok Build. NEGATIVE CONTROLS: each
    /// app's report under the other program, under a runtime (no Grok Build runs under one), under
    /// a shell or a supervised agent, and under no program.
    #[test]
    fn a_report_is_honoured_only_under_the_program_it_names() {
        let (codex, grok) = (Some(ProgramApp::Codex), Some(ProgramApp::GrokBuild));
        for program in [
            "codex",
            "/opt/x/bin/codex",
            "node",
            "/usr/local/bin/node",
            "bun",
        ] {
            assert!(honoured_for(Some(program), codex), "{program}");
            assert!(!honoured_for(Some(program), grok), "{program}: not Grok's");
        }
        for program in ["grok", "/Users//x/.local/share/aterm/agents/grok"] {
            assert!(honoured_for(Some(program), grok), "{program}");
            assert!(
                !honoured_for(Some(program), codex),
                "{program}: not codex's"
            );
        }
        for program in ["claude", "muse", "agy", "kimi", "zsh", "-zsh", "vim"] {
            assert!(!honoured_for(Some(program), codex), "{program}");
            assert!(!honoured_for(Some(program), grok), "{program}");
        }
        assert!(!honoured_for(None, codex));
        assert!(!honoured_for(None, grok));
        assert!(
            !honoured_for(Some("codex"), None),
            "no report names no program"
        );
    }

    #[test]
    fn the_row_word_names_the_reports_program_and_its_state() {
        let blocked = |app| ProgramState {
            state: ProgramStateWord::Blocked,
            kind: Some(ProgramBlockedKind::Question),
            app,
        };
        assert_eq!(row_word(blocked(ProgramApp::Codex)), "codex blocked");
        assert_eq!(row_word(blocked(ProgramApp::GrokBuild)), "grok blocked");
    }

    /// THE REAL `status` RECORD AND PRESENCE FACTS of a real session, its real
    /// engine fed the bytes codex writes, folded as the sweep folds them: a
    /// shown report is `program_state=` and the hover's word, beside an
    /// `agent=` it never moves; the group moving clears both.
    #[test]
    fn a_shown_report_is_status_program_state_and_never_agent() {
        let field = |record: &str, key: &str| {
            record
                .split(' ')
                .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_string)
        };
        for codex in [true, false] {
            let mut app = crate::App::headless_for_test();
            let shown = app
                .front_terminal(crate::WindowId(0))
                .expect("a front terminal");
            let (session, term) = (shown.session, std::sync::Arc::clone(&shown.term));
            let before = app.session_status_record(session).expect("a record");
            crate::term_lock(&term).process(WORKING);
            let report = crate::term_lock(&term).program_state();
            let t0 = Instant::now();
            assert_eq!(
                app.session_status
                    .observe_program_state(session, report, codex, false, t0),
                codex
            );
            let record = app.session_status_record(session).expect("a record");
            let want = if codex { "working" } else { "-" };
            assert_eq!(
                field(&record, "program_state").as_deref(),
                Some(want),
                "{record}"
            );
            let age = field(&record, "program_state_ms");
            if codex {
                assert!(
                    age.as_deref()
                        .and_then(|ms| ms.parse::<u64>().ok())
                        .is_some(),
                    "the shown report's age: {record}"
                );
            } else {
                assert_eq!(age.as_deref(), Some("-"), "none shown, no age: {record}");
            }
            for key in [
                "agent",
                "agent_detail",
                "agent_rev",
                "agent_gen",
                "agent_fp",
            ] {
                assert_eq!(
                    field(&record, key),
                    field(&before, key),
                    "{key} moved: {record}"
                );
            }
            let row = app
                .presence_facts(session)
                .and_then(|facts| facts.program)
                .map(|(word, _)| word);
            assert_eq!(row, codex.then_some("codex working"));
            assert_eq!(field(&record, "last_act").as_deref(), Some("-"));
            app.pool
                .get(session)
                .expect("pooled")
                .ctx
                .timeline
                .lock()
                .unwrap()
                .note_harness_act("continue", "codex-alab@1", aterm_slog::Actor::Ctl);
            let record = app.session_status_record(session).expect("a record");
            assert_eq!(field(&record, "last_act").as_deref(), Some("continue"));
            app.session_status
                .observe_program_state(session, report, codex, true, t0);
            let record = app.session_status_record(session).expect("a record");
            assert_eq!(
                field(&record, "program_state").as_deref(),
                Some("-"),
                "{record}"
            );
            assert_eq!(
                field(&record, "program_state_ms").as_deref(),
                Some("-"),
                "{record}"
            );
        }
    }

    /// `program_state_ms=` counts through sleep, as the survey's `agent_ms=` does: the gate stamps
    /// a shown report on the continuous clock (`aterm_uds::process::continuous_ms`) and ages it
    /// there, never by the sweep's `Instant` nor by the process's `Instant` epoch
    /// (`aterm_gui_timing::now_ms`, the clock of `status agent_since_ms=`), which both stop while
    /// the Mac sleeps. The stamp lies between continuous readings taken around the sweep, far
    /// above the process epoch's count; a stamp a minute back that no `Instant` here saw pass
    /// reads a minute; a repeat of the shown report keeps its age and a move restarts it.
    #[test]
    fn the_shown_reports_age_counts_on_the_clock_that_counts_sleep() {
        const MINUTE: u64 = 60_000;
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        assert_eq!(gate.shown_age_ms(), None);
        assert_eq!(
            status_fields(Some(&gate)),
            "program_state=- program_state_ms=-"
        );
        assert_eq!(status_fields(None), "program_state=- program_state_ms=-");
        let swept = Instant::now()
            .checked_sub(std::time::Duration::from_secs(10))
            .expect("an instant ten seconds back");
        term.process(WORKING);
        let before = aterm_uds::process::continuous_ms();
        assert!(gate.observe(term.program_state(), true, false, swept));
        let after = aterm_uds::process::continuous_ms();
        let Some((before, after)) = before.zip(after) else {
            let age = gate.shown_age_ms().expect("a shown report has an age");
            assert!(
                age >= 10_000,
                "no continuous clock here: the sweep's Instant: {age}"
            );
            return;
        };
        let stamp = gate.shown_continuous_ms.expect("a shown report is stamped");
        assert!(
            (before..=after).contains(&stamp),
            "stamped on the continuous clock: {before} <= {stamp} <= {after} (the process \
             epoch reads {})",
            aterm_gui_timing::now_ms()
        );
        let age = gate.shown_age_ms().expect("a shown report has an age");
        assert!(
            age < 10_000,
            "aged from the continuous stamp, not the sweep's Instant: {age}"
        );
        let fields = status_fields(Some(&gate));
        assert!(
            fields.starts_with("program_state=working program_state_ms="),
            "{fields}"
        );
        gate.shown_continuous_ms = Some(stamp.checked_sub(MINUTE).expect("a machine up a minute"));
        let slept = gate.shown_age_ms().expect("a shown report has an age");
        assert!(
            (MINUTE..MINUTE + 10_000).contains(&slept),
            "a minute only the continuous clock saw reads a minute: {slept}"
        );
        assert!(
            !gate.observe(term.program_state(), true, false, Instant::now()),
            "PRECONDITION: a repeat is no move"
        );
        let kept = gate.shown_age_ms().expect("a shown report has an age");
        assert!(kept >= MINUTE, "a repeat keeps the age: {kept}");
        term.process(BLOCKED_PERMISSION);
        assert!(gate.observe(term.program_state(), true, false, Instant::now()));
        let moved = gate.shown_age_ms().expect("a shown report has an age");
        assert!(moved < 10_000, "a move restarts the age: {moved}");
        term.process(CLEAR);
        assert!(gate.observe(term.program_state(), true, false, Instant::now()));
        assert_eq!(gate.shown_age_ms(), None, "a clear takes the age with it");
    }

    /// THE CLAIM'S OWN COLUMN ON `ls`, through the real sweep (`App::observe_session_statuses`)
    /// and the real `sessions` reply: a codex's shown report is its row's `program_state=`, beside
    /// `agent=` and its siblings, which it never moves; under claude the same bytes show `-`.
    #[test]
    fn a_shown_report_is_the_ls_rows_program_state_and_never_agent() {
        let field = |row: &str, key: &str| {
            row.split(' ')
                .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_string)
        };
        for codex in [true, false] {
            let mut app = crate::App::headless_for_test();
            let sid = app.next_session_id;
            app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
            let (term, timeline) = {
                let pooled = app.pool.get(sid).expect("pooled");
                (
                    std::sync::Arc::clone(&pooled.term),
                    std::sync::Arc::clone(&pooled.ctx.timeline),
                )
            };
            {
                let mut tl = timeline.lock().unwrap();
                tl.note_foreground_group(4242);
                tl.set_program(4242, Some(if codex { "codex" } else { "claude" }.into()));
            }
            let row = |app: &crate::App| {
                crate::control::cmd_sessions_store(&app.store, None)
                    .lines()
                    .find(|line| line.split(' ').next() == Some(sid.to_string().as_str()))
                    .expect("the session's row")
                    .to_string()
            };
            let mut t = Instant::now();
            let _ = app.observe_session_statuses(t);
            let before = row(&app);
            assert_eq!(
                field(&before, "program_state").as_deref(),
                Some("-"),
                "{before}"
            );
            for bytes in [WORKING, BLOCKED_PERMISSION] {
                t += std::time::Duration::from_secs(1);
                crate::term_lock(&term).process(bytes);
                let _ = app.observe_session_statuses(t);
            }
            let after = row(&app);
            let want = if codex { "blocked:permission" } else { "-" };
            assert_eq!(
                field(&after, "program_state").as_deref(),
                Some(want),
                "{after}"
            );
            for key in [
                "agent",
                "agent_detail",
                "agent_rev",
                "agent_gen",
                "agent_fp",
            ] {
                assert_eq!(
                    field(&after, key),
                    field(&before, key),
                    "{key} moved: {after}"
                );
            }
            assert!(
                after.contains(" program_state=") && after.contains(" human_ms="),
                "{after}"
            );
            let (agent_cols, rest) = after.split_once(" program_state=").expect("the column");
            assert!(
                agent_cols.contains(" agent_fp=") && rest.contains(" human_ms="),
                "after the agent fields, before human_ms=: {after}"
            );
        }
    }
}
