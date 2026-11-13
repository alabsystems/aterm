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
#[path = "program_state_tests.rs"]
mod tests;
