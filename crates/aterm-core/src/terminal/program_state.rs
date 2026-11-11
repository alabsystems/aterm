// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 7501 program lifecycle state reports ([`ProgramStateReport::turns`],
//! [`ProgramStateReport::turn_edges`]).
//!
//! The program status protocol's wire grammar is `OSC 7501 ; key=value(:key=value)* ST`: `state`
//! required, `app` the program, `kind` on a `blocked` report, `id` naming a child record (absent:
//! the root record), base64 `title` and `msg`, and `progress`. This reader takes the root record
//! of the two programs it knows by their `app` ([`ProgramApp`]) and leaves every child record
//! alone: a subagent's report, or the `clear` of one, says nothing of the session.

/// The state word of an OSC 7501 report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgramStateWord {
    /// Waiting for input, nothing running.
    Idle,
    /// A turn is running.
    Working,
    /// Stopped on something only a person (or a supervisor) can answer.
    Blocked,
    /// A turn ended with its work done.
    Done,
    /// A turn ended on an error.
    Error,
}

impl ProgramStateWord {
    /// Its wire word: `idle`, `working`, `blocked`, `done`, `error`.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    #[inline]
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// What a `blocked` report is blocked on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgramBlockedKind {
    /// A permission (approval) box.
    Permission,
    /// A question to the user.
    Question,
    /// A login.
    Auth,
}

impl ProgramBlockedKind {
    /// Its wire word: `permission`, `question`, `auth`.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    #[inline]
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "permission" => Self::Permission,
            "question" => Self::Question,
            "auth" => Self::Auth,
            _ => return None,
        })
    }
}

/// The program an OSC 7501 report names in its `app`, of the ones this reader takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgramApp {
    /// Codex (`app=codex`): the vendor build and codex-alab.
    Codex,
    /// xAI's Grok Build (`app=grok-build`), read out of its 1.0.70 binary: its root report
    /// (`state=<word>[:kind=permission]:app=grok-build[:title=<base64>]`), its child records'
    /// (`:id=<id>`), and `state=clear` at exit.
    GrokBuild,
}

impl ProgramApp {
    /// Its wire word: `codex`, `grok-build`.
    #[must_use]
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::GrokBuild => "grok-build",
        }
    }

    #[inline]
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "codex" => Self::Codex,
            "grok-build" => Self::GrokBuild,
            _ => return None,
        })
    }
}

/// One accepted OSC 7501 report (not a `clear`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProgramState {
    /// The state.
    pub state: ProgramStateWord,
    /// What a `blocked` report is blocked on, when it said; `None` on every other state.
    pub kind: Option<ProgramBlockedKind>,
    /// The program the report names.
    pub app: ProgramApp,
}

impl ProgramState {
    /// `state[:kind]`, as `status program_state=` prints it.
    #[inline]
    #[must_use]
    pub fn wire(self) -> String {
        match self.kind {
            Some(kind) => format!("{}:{}", self.state.as_str(), kind.as_str()),
            None => self.state.as_str().to_string(),
        }
    }
}

/// How many turns a report keeps the edges of ([`ProgramStateReport::turn_edges`]): the newest,
/// an older one dropped as a newer one begins.
pub const TURN_EDGES_KEPT: usize = 4;

/// Where a report that began or ended a turn was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnEdge {
    /// The report's [`ProgramStateReport::seq`].
    pub seq: u64,
    /// The wall clock of the parse batch it arrived in, in epoch milliseconds (the
    /// [`ClockReading::wall_ms`](super::ClockReading::wall_ms) that batch was processed at);
    /// `None` when that reading had none.
    pub wall_ms: Option<u64>,
}

/// One turn the accepted reports drew ([`ProgramStateReport::turn_edges`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramTurn {
    /// The program whose report began it, or ended it when none kept began it.
    pub app: ProgramApp,
    /// The report that began it (one [`ProgramStateReport::turns`] counts); `None` for a turn
    /// already running at a restore, which keeps no edge from before it.
    pub began: Option<TurnEdge>,
    /// The rest that ended it (`idle`, `done` or `error`) and its edge; `None` while it runs.
    pub ended: Option<(ProgramStateWord, TurnEdge)>,
}

/// The engine's record of OSC 7501.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramStateReport {
    /// Moves with every accepted report, `clear` and full reset included.
    pub seq: u64,
    /// The `seq` of the report that began the one standing now: a report equal to the one
    /// before it (a writer's re-send) keeps it, so one box re-sent is still one box.
    pub began: u64,
    /// The report standing now; `None` after a `clear`, a reset, or before any.
    pub state: Option<ProgramState>,
    /// How many turns the accepted reports began: a `working` or `blocked` report begins one unless
    /// the report before it (a `clear` or a full reset looked past) was `working` or `blocked` too.
    pub turns: u64,
    /// The state of the latest report taken, a `clear` or a full reset looked past.
    last: Option<ProgramStateWord>,
    /// The newest [`TURN_EDGES_KEPT`] turns the reports drew, oldest first, `None` past the
    /// newest ([`Self::turn_edges`]).
    edges: [Option<ProgramTurn>; TURN_EDGES_KEPT],
}

impl ProgramStateReport {
    /// Take one OSC 7501 body (the text after `7501;`) from a batch with no wall clock
    /// ([`Self::accept_at`] with `None`).
    #[cfg(test)]
    pub(super) fn accept(&mut self, body: &str) -> bool {
        self.accept_at(body, None)
    }

    /// Take one OSC 7501 body (the text after `7501;`) from a parse batch whose wall clock read
    /// `wall_ms`, which dates the turn edge it draws ([`Self::turn_edges`]).
    pub(super) fn accept_at(&mut self, body: &str, wall_ms: Option<u64>) -> bool {
        match parse(body) {
            Some(next) => {
                self.seq = self.seq.wrapping_add(1);
                let state = match next {
                    Body::Clear => None,
                    Body::Report(state) => {
                        let edge = TurnEdge {
                            seq: self.seq,
                            wall_ms,
                        };
                        if begins_a_turn(self.last, state.state) {
                            self.turns = self.turns.wrapping_add(1);
                            self.push_turn(ProgramTurn {
                                app: state.app,
                                began: Some(edge),
                                ended: None,
                            });
                        } else if ends_a_turn(self.last, state.state) {
                            self.end_turn(state, edge);
                        }
                        self.last = Some(state.state);
                        Some(state)
                    }
                };
                if state != self.state {
                    self.began = self.seq;
                }
                self.state = state;
                true
            }
            None => false,
        }
    }

    /// The turns the accepted reports drew, oldest first: the newest [`TURN_EDGES_KEPT`]. A report
    /// that begins a turn ([`Self::turns`]) adds one with its edge, and the next rest (`idle`,
    /// `done`, `error`) after a report in a turn (a `clear` or a full reset looked past) ends the
    /// newest, or adds one with no beginning when the newest has ended. A re-send, a move within a
    /// turn, a `clear`, a full reset and a child record's report add and end none. A restore that
    /// installs a different report clears them, and a checkpoint carries none.
    #[inline]
    pub fn turn_edges(&self) -> impl Iterator<Item = ProgramTurn> + '_ {
        self.edges.iter().flatten().copied()
    }

    /// Keep `turn` as the newest, dropping the oldest when [`TURN_EDGES_KEPT`] are kept.
    fn push_turn(&mut self, turn: ProgramTurn) {
        if let Some(free) = self.edges.iter_mut().find(|slot| slot.is_none()) {
            *free = Some(turn);
        } else {
            self.edges.rotate_left(1);
            self.edges[TURN_EDGES_KEPT - 1] = Some(turn);
        }
    }

    /// End the newest turn at the rest `state` took at `edge`, or keep a turn with no beginning
    /// when the newest has ended (or none is kept).
    fn end_turn(&mut self, state: ProgramState, edge: TurnEdge) {
        let ended = Some((state.state, edge));
        match self.edges.iter_mut().rev().find_map(Option::as_mut) {
            Some(newest) if newest.ended.is_none() => newest.ended = ended,
            _ => self.push_turn(ProgramTurn {
                app: state.app,
                began: None,
                ended,
            }),
        }
    }

    /// The report standing now as the OSC 7501 body that says it
    /// (`state=<word>:app=<app>[:kind=<kind>]`), which a checkpoint carries ([`Self::restore`]);
    /// `None` while none stands.
    pub(super) fn carried(&self) -> Option<String> {
        self.state.map(|state| match state.kind {
            Some(kind) => format!(
                "state={}:app={}:kind={}",
                state.state.as_str(),
                state.app.as_str(),
                kind.as_str()
            ),
            None => format!("state={}:app={}", state.state.as_str(), state.app.as_str()),
        })
    }

    /// Install the report a checkpoint carried (`body`, [`Self::carried`]), read by this
    /// reader's own parser, as a new report that begins no turn: a later report is judged
    /// against it (a re-send of it keeps where it began). `None`, or a body this reader does not
    /// take, leaves none standing. The sequence only moves forward. Installing a different report
    /// clears the turn edges ([`Self::turn_edges`]); restoring the report standing keeps them.
    pub(super) fn restore(&mut self, body: Option<&str>) {
        let state = match body.and_then(parse) {
            Some(Body::Report(state)) => Some(state),
            Some(Body::Clear) | None => None,
        };
        if state == self.state {
            return;
        }
        self.seq = self.seq.wrapping_add(1);
        self.began = self.seq;
        self.state = state;
        self.edges = [None; TURN_EDGES_KEPT];
        if let Some(state) = state {
            self.last = Some(state.state);
        }
    }

    /// A full reset (RIS): no report stands.
    #[inline]
    pub(super) const fn reset(&mut self) {
        if self.state.is_some() {
            self.seq = self.seq.wrapping_add(1);
            self.began = self.seq;
            self.state = None;
        }
    }
}

/// Whether a report of `next` begins a turn after `last`.
#[inline]
const fn begins_a_turn(last: Option<ProgramStateWord>, next: ProgramStateWord) -> bool {
    use ProgramStateWord as W;
    matches!(next, W::Working | W::Blocked) && !matches!(last, Some(W::Working | W::Blocked))
}

/// Whether a report of `next` ends the turn running after `last`: a rest after a report in a turn.
#[inline]
const fn ends_a_turn(last: Option<ProgramStateWord>, next: ProgramStateWord) -> bool {
    use ProgramStateWord as W;
    matches!(next, W::Idle | W::Done | W::Error) && matches!(last, Some(W::Working | W::Blocked))
}

/// The longest OSC 7501 body this reader looks at; anything longer is not a report. The
/// protocol caps a `title` at 256 base64 bytes, a `msg` at 2,732 and an `id` at 128, so its
/// longest report, every key at its cap, is about 3,200 bytes: a Grok Build report carries its
/// `title`, which a 128-byte cap would have dropped whole.
const MAX_BODY_BYTES: usize = 4096;

/// One OSC 7501 body this reader takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Body {
    /// `state=clear`: no report stands.
    Clear,
    /// A report.
    Report(ProgramState),
}

/// What one OSC 7501 body says ([`Body::Clear`], [`Body::Report`]): only the root record's, so a
/// body naming an `id` (a child record's report, or its `clear`) is none.
fn parse(body: &str) -> Option<Body> {
    if body.len() > MAX_BODY_BYTES {
        return None;
    }
    let (mut state, mut app, mut kind, mut id) = (None, None, None, None);
    for item in body.split(':') {
        let (key, value) = item.split_once('=')?;
        match key {
            "state" if state.is_none() => state = Some(value),
            "app" if app.is_none() => app = Some(value),
            "kind" if kind.is_none() => kind = Some(value),
            "id" if id.is_none() => id = Some(value),
            "state" | "app" | "kind" | "id" => return None,
            _ => {}
        }
    }
    if id.is_some() {
        return None;
    }
    if state? == "clear" {
        return Some(Body::Clear);
    }
    let app = ProgramApp::parse(app?)?;
    let state = ProgramStateWord::parse(state?)?;
    let kind = match (state, kind) {
        (ProgramStateWord::Blocked, Some(k)) => Some(ProgramBlockedKind::parse(k)?),
        (_, _) => None,
    };
    Some(Body::Report(ProgramState { state, kind, app }))
}

#[cfg(test)]
#[path = "program_state_tests.rs"]
mod tests;
