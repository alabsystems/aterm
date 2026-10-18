// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROGRAM'S OWN STATE (OSC 7501, the native harness contract §6): what a
//! `codex` in front says about itself, shown as `status program_state=` and in
//! the presence row, and NOTHING ELSE.
//!
//! The engine keeps each session's last report with a sequence
//! (`aterm_core::terminal::program_state`). This gate, run by the status sweep
//! beside the agent verdict, decides which report a session shows:
//!
//! - only while the program in front names `codex` (the sweep's published
//!   program, its argv0's word: [`aterm_phase::program_of`]), or is a runtime
//!   that hosts one (`node`, `bun`: npm's `codex`; [`honoured_for`]);
//! - after a foreground move, only a report newer than the previous sweep's
//!   reading. Already observed reports are cleared; an unseen report can show
//!   immediately. The PTY does not attribute bytes to a writer, so an old
//!   program's final unseen report and a new program's first report between
//!   sweeps cannot be distinguished. `state=clear` and a full reset clear the
//!   engine's report.
//!
//! It is a claim for display. It never reaches `agent=`, `agent_since_ms=`,
//! `phase_authoritative`, the zone hash an act reads, nor any `await`: nothing
//! here is read by a decision, and the verdict those carry stays the screen
//! reader's. A report the sweep did not see (one superseded between two
//! sweeps) is simply not shown; the next one is.

use std::time::Instant;

use aterm_core::terminal::program_state::{ProgramState, ProgramStateReport};

/// One session's gate (see the module doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramStateGate {
    /// The engine sequence the last sweep saw.
    seen_seq: u64,
    /// Reports at or below this sequence were already seen before the latest
    /// observed foreground move: none of them shows.
    floor_seq: u64,
    /// The report shown, and since when.
    shown: Option<(ProgramState, Instant)>,
    /// How many shown `working` reports were followed by a shown `done` or
    /// `idle`: the program's own turn ends, which the presence band tells as
    /// a story point (`Facts::program_turn_ends`). Display only.
    turn_ends: u64,
    /// How many shown `working` reports began a turn ([`starts_a_turn`]):
    /// what pairs one turn's end with the ledger settle of the same turn, so
    /// the presence band tells it once (`Facts::program_turn_starts`).
    turn_starts: u64,
}

impl ProgramStateGate {
    /// Fold one sweep's reading: the engine's `report` (read under the sweep's
    /// terminal guard), whether the program in front names `codex`
    /// (`honoured`), and whether the foreground group moved since the last
    /// sweep (`group_moved`). Whether what is shown changed.
    pub(crate) fn observe(
        &mut self,
        report: ProgramStateReport,
        honoured: bool,
        group_moved: bool,
        now: Instant,
    ) -> bool {
        if group_moved {
            // Discard reports already seen under the previous group. An
            // unseen report remains eligible; its writer is not attributable.
            self.floor_seq = self.seen_seq;
        }
        self.seen_seq = report.seq;
        let next = report
            .state
            .filter(|_| honoured && report.seq > self.floor_seq);
        if self.shown.map(|(state, _)| state) == next && (!group_moved || next.is_none()) {
            return false;
        }
        if ends_a_turn(self.shown.map(|(state, _)| state), next) {
            self.turn_ends += 1;
        }
        if starts_a_turn(self.shown.map(|(state, _)| state), next) {
            self.turn_starts += 1;
        }
        self.shown = next.map(|state| (state, now));
        true
    }

    /// How many turn ends this session's program has reported
    /// ([`ends_a_turn`]), as shown.
    pub(crate) fn turn_ends(&self) -> u64 {
        self.turn_ends
    }

    /// How many turns this session's program has begun ([`starts_a_turn`]),
    /// as shown.
    pub(crate) fn turn_starts(&self) -> u64 {
        self.turn_starts
    }

    /// The report shown, and since when.
    pub(crate) fn shown(&self) -> Option<(ProgramState, Instant)> {
        self.shown
    }
}

/// Whether the shown report moving from `before` to `after` is the program
/// finishing a turn: `working`, then `done` or `idle`. A foreground move
/// that shows nothing, a `blocked` or an `error` is not one.
fn ends_a_turn(before: Option<ProgramState>, after: Option<ProgramState>) -> bool {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    matches!(
        (before.map(|s| s.state), after.map(|s| s.state)),
        (Some(W::Working), Some(W::Done | W::Idle))
    )
}

/// Whether the shown report moving from `before` to `after` is the program
/// beginning a turn: entering a turn in progress (`working` or `blocked`)
/// from no report, `idle`, `done` or `error`. A turn the gate first sees
/// `blocked` (a permission asked before a sweep saw it work) begins there;
/// `working` after `blocked` is the same turn resuming, and a move within the
/// two is no start. So every turn end the gate counts ([`ends_a_turn`]) has
/// its own start: a turn the person drove themselves moves the count, and
/// its end never pairs with a ledger settle read at the count before it.
fn starts_a_turn(before: Option<ProgramState>, after: Option<ProgramState>) -> bool {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    let in_turn = |state: Option<ProgramState>| {
        matches!(state.map(|s| s.state), Some(W::Working | W::Blocked))
    };
    in_turn(after) && !in_turn(before)
}

/// Whether a report is honoured under the program in front, its published
/// name (`program=`): under a `codex` ([`aterm_phase::program_of`]), and
/// under a runtime that hosts one ([`aterm_phase::may_host_agent`]: `node`,
/// `bun`) — npm's `codex` is `node …/codex.js`, and since aterm supervises no
/// Codex (the Codex screen lane's retirement, 2026-10-16) its own report is
/// all a vendor `codex` shows of itself. Only a report naming `app=codex`
/// is a report at all (the engine's parser), so a runtime running anything
/// else shows none. Never under another agent's name or a shell's. An
/// exhaustive match, so a new agent decides here (the agent-gate guard).
pub(crate) fn honoured_for(program: Option<&str>) -> bool {
    use aterm_phase::Program;
    let Some(name) = program else {
        return false;
    };
    match aterm_phase::program_of(name) {
        Some(Program::Codex) => true,
        Some(
            Program::Claude | Program::Generic | Program::Agy | Program::Muse | Program::Gemini,
        ) => false,
        None => aterm_phase::may_host_agent(name),
    }
}

/// `status program_state=<state>[:<kind>]`, `-` while none is shown.
pub(crate) fn wire(shown: Option<(ProgramState, Instant)>) -> String {
    shown.map_or_else(|| "-".to_string(), |(state, _)| state.wire())
}

/// The presence row's word for a shown report, when the screen's reader names
/// no phase: `codex working`, `codex blocked`.
pub(crate) fn row_word(state: ProgramState) -> &'static str {
    use aterm_core::terminal::program_state::ProgramStateWord as W;
    match state.state {
        W::Idle => "codex idle",
        W::Working => "codex working",
        W::Blocked => "codex blocked",
        W::Done => "codex done",
        W::Error => "codex error",
    }
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
        // The same report read again is no change.
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
        // The program is named codex on a later sweep: the report stands.
        assert!(gate.observe(term.program_state(), true, false, t0));
        assert_eq!(shown(&gate).as_deref(), Some("working"));
        // …and it stops showing the moment it is not.
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
        // The program exits without a clear; the shell is in front.
        assert!(gate.observe(term.program_state(), true, true, t0));
        assert_eq!(shown(&gate), None, "the exited program's report is gone");
        assert!(!gate.observe(term.program_state(), true, false, t0));
        // A new codex starts and reports before the sweep sees the move.
        term.process(b"\x1b]7501;state=idle:app=codex\x1b\\");
        assert!(gate.observe(term.program_state(), true, true, t0));
        assert_eq!(shown(&gate).as_deref(), Some("idle"));
        // The next program can report the same word before its foreground
        // move is observed. Its age starts here, not at the old program's.
        let next_start = t0 + std::time::Duration::from_secs(1);
        term.process(b"\x1b]7501;state=idle:app=codex\x1b\\");
        assert!(gate.observe(term.program_state(), true, true, next_start));
        assert_eq!(gate.shown().map(|(_, since)| since), Some(next_start));
        assert!(!gate.observe(term.program_state(), true, false, next_start));
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
        // Two turns began: the `working` after `blocked` resumed the first.
        assert_eq!(gate.turn_starts(), 2);
        // A turn first seen `blocked` (it asked before a sweep saw it work)
        // begins there, so its end has a start of its own; NEGATIVE CONTROL:
        // its `working` after the answer is no second start.
        term.process(BLOCKED_PERMISSION);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_starts(), 3, "idle then blocked");
        term.process(WORKING);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_starts(), 3, "blocked then working resumes it");
        term.process(done);
        gate.observe(term.program_state(), true, false, t0);
        assert_eq!(gate.turn_ends(), 3);
        // Negative control: the same bytes under a program that is not codex.
        let mut other = ProgramStateGate::default();
        for bytes in [WORKING, done.as_slice()] {
            term.process(bytes);
            other.observe(term.program_state(), false, false, t0);
        }
        assert_eq!(other.turn_ends(), 0);
        assert_eq!(other.turn_starts(), 0);
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

    #[test]
    fn only_a_codex_in_front_is_honoured() {
        assert!(honoured_for(Some("codex")));
        assert!(honoured_for(Some("/opt/x/bin/codex")));
        // npm's codex: `node …/codex.js` leads the group, and its report
        // stays shown (the runtimes that may host an agent).
        assert!(honoured_for(Some("node")));
        assert!(honoured_for(Some("/usr/local/bin/node")));
        assert!(honoured_for(Some("bun")));
        // NEGATIVE CONTROLS: another agent, a shell, nothing named.
        assert!(!honoured_for(Some("claude")));
        assert!(!honoured_for(Some("zsh")));
        assert!(!honoured_for(Some("-zsh")));
        assert!(!honoured_for(Some("vim")));
        assert!(!honoured_for(None));
    }

    #[test]
    fn the_row_word_names_codex_and_its_state() {
        let blocked = ProgramState {
            state: ProgramStateWord::Blocked,
            kind: Some(ProgramBlockedKind::Question),
        };
        assert_eq!(row_word(blocked), "codex blocked");
    }

    /// THE REAL `status` RECORD AND PRESENCE ROW of a real session, its real
    /// engine fed the bytes codex writes, folded as the sweep folds them: a
    /// shown report is `program_state=` and the row's word, beside an
    /// `agent=` it never moves; the group moving clears both. NEGATIVE
    /// CONTROL: the same bytes under a program that is not `codex` show `-`.
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
            // `agent_since_ms=` is an age, so it is read for a verdict that never moved
            // by the rev and stamp beside it.
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
            // `last_act=` beside it: the act a self-supervising agent recorded.
            assert_eq!(field(&record, "last_act").as_deref(), Some("-"));
            app.pool
                .get(session)
                .expect("pooled")
                .ctx
                .timeline
                .lock()
                .unwrap()
                .note_harness_act("continue", "codex-alab@1");
            let record = app.session_status_record(session).expect("a record");
            assert_eq!(field(&record, "last_act").as_deref(), Some("continue"));
            // The group moves (the program exits): nothing shows.
            app.session_status
                .observe_program_state(session, report, codex, true, t0);
            let record = app.session_status_record(session).expect("a record");
            assert_eq!(
                field(&record, "program_state").as_deref(),
                Some("-"),
                "{record}"
            );
        }
    }
}
