// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 7501 program lifecycle state reports ([`ProgramStateReport::turns`],
//! [`ProgramTurns`]).
//!
//! The program status protocol's wire grammar is `OSC 7501 ; key=value(:key=value)* ST`: `state`
//! required, `app` the program, `kind` on a `blocked` report, `id` naming a child record (absent:
//! the root record), base64 `title` and `msg`, and `progress`. This reader takes the root record
//! of any program by its `app` ([`ProgramApp`]) and leaves every child record alone: a
//! subagent's report, or the `clear` of one, says nothing of the session. A program it does not
//! know by name ([`ProgramApp::Other`]) has its report stand apart
//! ([`ProgramStateReport::other`]), to be shown: it moves none of the known programs' record,
//! which is what every reader that decides reads. A program asks whether the terminal speaks the
//! protocol with `OSC 7501 ; ?`, which the engine answers (`handle_osc_7501`); Claude Code, Grok
//! Build, Muse Code, agy and Kimi Code write no report until it is answered.

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

/// The program an OSC 7501 report names in its `app`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProgramApp {
    /// Claude Code (`app=claude-code`), measured on 2.1.323 once its query is answered:
    /// `state=idle` at once, `working` before the frame that shows a turn begun, `done` before the
    /// frame that draws the reply's last line and its done row, `error` with a base64 `msg` for a
    /// failed turn, `blocked:kind=permission` with the box's `msg`, and `state=clear` at exit. With
    /// its query answered its window title carries no run glyph.
    ClaudeCode,
    /// Codex (`app=codex`): the vendor build and codex-alab, which write reports unasked.
    Codex,
    /// xAI's Grok Build: `app=grok-build`, read out of its 1.0.70 binary (its root report
    /// `state=<word>[:kind=permission]:app=grok-build[:title=<base64>]`, its child records'
    /// `:id=<id>`, and `state=clear` at exit), and `app=grok`, which 1.0.77 writes once its query
    /// is answered (`state=idle:app=grok`, then `state=clear` at exit).
    GrokBuild,
    /// Any other program, by the word it names itself with: its report stands apart, to be
    /// shown, and draws no turn ([`ProgramStateReport::other`]).
    Other(AppWord),
}

impl ProgramApp {
    /// Its wire word: `claude-code`, `codex`, `grok-build`, or the [`AppWord`] it was named by.
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::GrokBuild => "grok-build",
            Self::Other(word) => word.as_str(),
        }
    }

    /// One of the programs this reader knows by name, whose reports draw turns: not
    /// [`Self::Other`].
    #[must_use]
    #[inline]
    pub const fn is_known(&self) -> bool {
        !matches!(self, Self::Other(_))
    }

    /// A known program's wire word.
    #[inline]
    fn known(word: &str) -> Option<Self> {
        Some(match word {
            "claude-code" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "grok-build" | "grok" => Self::GrokBuild,
            _ => return None,
        })
    }

    /// The program `word` names: a known one, else an [`AppWord`]; `None` for a word outside
    /// that grammar.
    #[inline]
    fn parse(word: &str) -> Option<Self> {
        Self::known(word).or_else(|| AppWord::new(word).map(Self::Other))
    }
}

/// An `app=` word none of the known programs uses: 1 to [`AppWord::MAX`] bytes, each of
/// `[a-z0-9._-]`. Kept inline, so a report stays `Copy`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppWord {
    /// How many of `bytes` are the word.
    len: u8,
    /// The word, then zeros.
    bytes: [u8; AppWord::MAX],
}

impl AppWord {
    /// The longest word.
    pub const MAX: usize = 32;

    /// `word`, when it is 1 to [`Self::MAX`] bytes of `[a-z0-9._-]` and no known program's
    /// word (that is [`ProgramApp::ClaudeCode`], [`ProgramApp::Codex`] or
    /// [`ProgramApp::GrokBuild`]).
    #[must_use]
    pub fn new(word: &str) -> Option<Self> {
        let raw = word.as_bytes();
        let grammar = |b: &u8| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-');
        if raw.is_empty()
            || raw.len() > Self::MAX
            || !raw.iter().all(grammar)
            || ProgramApp::known(word).is_some()
        {
            return None;
        }
        let mut bytes = [0; Self::MAX];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(Self {
            len: u8::try_from(raw.len()).ok()?,
            bytes,
        })
    }

    /// The word.
    #[must_use]
    #[inline]
    pub fn as_str(&self) -> &str {
        // Every byte is ASCII ([`Self::new`]), so this never falls back.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

impl std::fmt::Debug for AppWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AppWord").field(&self.as_str()).finish()
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

/// How many turns the engine keeps ([`ProgramTurns`]): the newest, an older one dropped as a newer
/// one is kept. A reader's turn bucket (aterm-gui's `PROGRAM_BURST`) is held to no more than this,
/// so a reading opens no more turns than the ring keeps.
pub const PROGRAM_TURNS_KEPT: usize = 32;

/// Where a report that began, resumed or ended a turn was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnEdge {
    /// The report's [`ProgramStateReport::seq`].
    pub seq: u64,
    /// The wall clock of the parse batch it arrived in, in epoch milliseconds (the
    /// [`ClockReading::wall_ms`](super::ClockReading::wall_ms) that batch was processed at);
    /// `None` when that reading had none (a restore's).
    pub wall_ms: Option<u64>,
}

/// One turn the accepted reports drew ([`ProgramTurns`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramTurn {
    /// Its place in the ring: how many turns the ring had kept when it kept this one, counting
    /// it (the first is 1). A reader's cursor ([`TurnCursor`]) counts past it.
    pub n: u64,
    /// The program whose report began it, or ended it when none kept began it.
    pub app: ProgramApp,
    /// The report that began it, or resumed it ([`Self::resumed`]); `None` for a turn whose only
    /// kept edge is its end.
    pub began: Option<TurnEdge>,
    /// `began` resumed a turn rather than beginning one: an in-turn report taken while no report
    /// stood (a `clear` or a full reset took an in-turn report down, as codex-alab's handoff to
    /// a job-control stop or an external program does), or one a restore installed. A resumed
    /// beginning is the same turn's: [`ProgramStateReport::turns`] does not count it.
    pub resumed: bool,
    /// The rest that ended it (`idle`, `done` or `error`) and its edge; `None` while it runs.
    pub ended: Option<(ProgramStateWord, TurnEdge)>,
}

/// Where a reader of [`ProgramTurns`] has read to ([`ProgramTurns::after`]): the newest report
/// read and the newest turn consumed or acknowledged lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnCursor {
    /// The [`ProgramStateReport::seq`] read: an edge above it is new.
    pub seq: u64,
    /// The [`ProgramTurn::n`] consumed or acknowledged lost: a newer absent turn was lost unread.
    pub n: u64,
}

/// THE TURNS THE REPORTS DREW: the newest [`PROGRAM_TURNS_KEPT`], each numbered
/// ([`ProgramTurn::n`]). A report that begins a turn ([`ProgramStateReport::turns`]) keeps one with
/// its edge; an in-turn report taken while no report stands keeps a resumed one
/// ([`ProgramTurn::resumed`]); the next rest (`idle`, `done`, `error`) after a report from the
/// same app in a turn (a `clear` or a full reset looked past) ends the newest, or keeps one with
/// no beginning when the newest has ended. A re-send, a move within a turn, a `clear`, a full reset and a child
/// record's report keep and end none. A restore that installs a different report drops every
/// kept turn, then keeps a resumed one when that report is in a turn; a checkpoint carries none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProgramTurns {
    /// The kept turns, turn `n` in slot `(n - 1) % PROGRAM_TURNS_KEPT`.
    slots: [Option<ProgramTurn>; PROGRAM_TURNS_KEPT],
    /// How many turns were ever kept: the newest's `n`.
    kept: u64,
    /// The first `n` a restore left kept: older ones were dropped.
    from: u64,
}

impl Default for ProgramTurns {
    fn default() -> Self {
        ProgramTurns {
            slots: [None; PROGRAM_TURNS_KEPT],
            kept: 0,
            from: 1,
        }
    }
}

impl ProgramTurns {
    /// The first `n` still kept.
    const fn first(&self) -> u64 {
        let rotated = self.kept.saturating_sub(PROGRAM_TURNS_KEPT as u64) + 1;
        if self.from > rotated {
            self.from
        } else {
            rotated
        }
    }

    /// The slot of turn `n` (at least 1).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a remainder below PROGRAM_TURNS_KEPT fits a usize"
    )]
    const fn slot(n: u64) -> usize {
        ((n - 1) % PROGRAM_TURNS_KEPT as u64) as usize
    }

    /// The kept turns, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = ProgramTurn> + '_ {
        (self.first()..=self.kept).filter_map(|n| self.slots[Self::slot(n)])
    }

    /// The newest kept turn.
    #[must_use]
    pub fn newest(&self) -> Option<ProgramTurn> {
        (self.kept >= self.first())
            .then(|| self.slots[Self::slot(self.kept)])
            .flatten()
    }

    /// Where a reader that read everything kept now stands, past the report `seq`.
    #[must_use]
    pub const fn head(&self, seq: u64) -> TurnCursor {
        TurnCursor { seq, n: self.kept }
    }

    /// THE TURNS PAST `after` into `out`, oldest first: each kept turn with an edge whose report is
    /// newer than `after.seq` (every kept turn when `after` is `None`, a reader's first reading),
    /// answering how many turns kept after `after.n` are no longer kept (lost to the ring before
    /// the reader read them, or dropped by a restore). `out` is cleared first.
    pub fn after(&self, after: Option<TurnCursor>, out: &mut Vec<ProgramTurn>) -> u64 {
        out.clear();
        let seq = after.map_or(0, |c| c.seq);
        let newer = |edge: Option<TurnEdge>| edge.is_some_and(|e| e.seq > seq);
        out.extend(
            self.iter()
                .filter(|t| after.is_none() || newer(t.began) || newer(t.ended.map(|(_, e)| e))),
        );
        let read = after.map_or(0, |c| c.n);
        self.first().min(self.kept + 1).saturating_sub(read + 1)
    }

    /// Keep `turn` as the newest (its `n` set here), dropping the oldest when
    /// [`PROGRAM_TURNS_KEPT`] are kept.
    const fn push(&mut self, mut turn: ProgramTurn) {
        self.kept += 1;
        turn.n = self.kept;
        self.slots[Self::slot(self.kept)] = Some(turn);
    }

    /// End the newest turn at the rest `state` took at `edge`, or keep a turn with no beginning
    /// when the newest has ended (or none is kept).
    fn end(&mut self, state: ProgramState, edge: TurnEdge) {
        let ended = Some((state.state, edge));
        let newest = (self.kept >= self.first())
            .then(|| self.slots[Self::slot(self.kept)].as_mut())
            .flatten();
        match newest {
            Some(newest) if newest.ended.is_none() => newest.ended = ended,
            _ => self.push(ProgramTurn {
                n: 0,
                app: state.app,
                began: None,
                resumed: false,
                ended,
            }),
        }
    }

    /// Drop every kept turn ([`ProgramStateRecord::restore`]); the numbering goes on.
    const fn clear(&mut self) {
        self.from = self.kept + 1;
    }
}

/// Another program's report ([`ProgramApp::Other`]), which stands apart from the known
/// programs' ([`ProgramStateReport::other`]): it is shown, and nothing that decides reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OtherReport {
    /// Moves with every such report, and with each `clear`, known program's report, shell
    /// prompt, full reset and restore that takes one away.
    pub seq: u64,
    /// The report standing; `None` before any, and after a `clear`, a known program's report, a
    /// shell prompt ([`ProgramStateRecord::prompt_drawn`]), a full reset or a restore of
    /// another.
    pub state: Option<ProgramState>,
}

impl OtherReport {
    /// `state` stands now: a report always moves the sequence, taking one away only when one
    /// stood.
    const fn take(&mut self, state: Option<ProgramState>) {
        if state.is_some() || self.state.is_some() {
            self.seq = self.seq.wrapping_add(1);
        }
        self.state = state;
    }
}

/// The engine's report of OSC 7501: what stands now and how many turns began, read by value on
/// every status sweep ([`ProgramStateRecord::report`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramStateReport {
    /// Moves with every accepted report of a known program ([`ProgramApp::is_known`]), `clear`
    /// and full reset included.
    pub seq: u64,
    /// The `seq` of the report that began the one standing now: a report equal to the one
    /// before it (a writer's re-send) keeps it, so one box re-sent is still one box.
    pub began: u64,
    /// The known programs' report standing now, which every reader that decides reads; `None`
    /// after a `clear`, a reset, or before any. Another program's report never stands here, nor
    /// takes this one away ([`Self::other`]).
    pub state: Option<ProgramState>,
    /// Another program's report, shown in words without moving the known program's record.
    pub other: OtherReport,
    /// How many turns the accepted reports began: a `working` or `blocked` report begins one unless
    /// the previous accepted report names the same app and was `working` or `blocked` too
    /// (a `clear` or a full reset is looked past).
    pub turns: u64,
    /// The `seq` of the latest `clear`, or of the latest full reset that took a report down (0
    /// before any): a reader that sees the foreground move splits the turn edges it had not read
    /// there, those below it the leaving program's.
    pub cleared: u64,
    /// The latest report taken, a `clear` or a full reset looked past: its app owns the turn.
    last: Option<ProgramState>,
    /// The process group whose bytes the host said it parses ([`ProgramStateRecord::front_is`]);
    /// `0` before it said any.
    front: i32,
    /// The group whose bytes carried the known programs' report standing: [`Self::front`] when it
    /// was taken, or the first group said where none was said by then (`0` until then). The
    /// report is a run sign only while that group holds the front ([`Self::run_sign`]).
    writer: i32,
}

impl ProgramStateReport {
    /// The known programs' report standing now as the OSC 7501 body that says it
    /// (`state=<word>:app=<app>[:kind=<kind>]`); `None` while none stands. The screen readers
    /// take it only as [`Self::run_sign`] gives it.
    #[must_use]
    pub fn carried(&self) -> Option<String> {
        self.state.map(body_of)
    }

    /// The known programs' report standing as a run sign of the program in front
    /// ([`Self::carried`]), which `text --json` sends as `"program_report"` and the readers take
    /// ahead of the title: `None` while none stands, and while its writer's group is not in front
    /// ([`Terminal::program_front_is`](super::Terminal::program_front_is)). A program killed
    /// mid-turn, or one that exited with no `clear`, leaves its report standing, and nothing the
    /// shell writes overwrites it; it is no run sign under the shell, nor under a later program
    /// until that program writes a report of its own, which stands in its place as that
    /// program's. A program stopped and resumed (`^Z`, then `fg`) holds the front again, and its
    /// report is its run sign again. With no group said the report standing is the sign. Another
    /// program's report ([`Self::other`]) is never one.
    #[must_use]
    pub fn run_sign(&self) -> Option<String> {
        self.sign().map(body_of)
    }

    /// [`Self::run_sign`] by value: the known programs' report standing while its writer's group
    /// is in front, else `None`. A reader that runs with each parse (the session's reader, which
    /// keeps it off the term lock) takes it with no allocation.
    #[must_use]
    pub const fn sign(&self) -> Option<ProgramState> {
        if self.writer != self.front {
            return None;
        }
        self.state
    }
}

/// THE ENGINE'S RECORD OF OSC 7501: the report standing ([`Self::report`]) and the turns the
/// reports drew ([`Self::turns`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramStateRecord {
    report: ProgramStateReport,
    ring: ProgramTurns,
}

/// The module's tests read the report's fields through the record.
#[cfg(test)]
impl std::ops::Deref for ProgramStateRecord {
    type Target = ProgramStateReport;

    fn deref(&self) -> &ProgramStateReport {
        &self.report
    }
}

impl ProgramStateRecord {
    /// The report standing, by value.
    #[inline]
    #[must_use]
    pub const fn report(&self) -> ProgramStateReport {
        self.report
    }

    /// The turns the reports drew.
    #[inline]
    #[must_use]
    pub const fn turns(&self) -> &ProgramTurns {
        &self.ring
    }

    /// Take one OSC 7501 body (the text after `7501;`) from a batch with no wall clock
    /// ([`Self::accept_at`] with `None`).
    #[cfg(test)]
    pub(super) fn accept(&mut self, body: &str) -> bool {
        self.accept_at(body, None)
    }

    /// Take one OSC 7501 body (the text after `7501;`) from a parse batch whose wall clock read
    /// `wall_ms`, which dates the turn edge it draws ([`ProgramTurns`]).
    pub(super) fn accept_at(&mut self, body: &str, wall_ms: Option<u64>) -> bool {
        let Some(next) = parse(body) else {
            return false;
        };
        if let Body::Report(state) = next
            && !state.app.is_known()
        {
            self.report.other.take(Some(state));
            return true;
        }
        let r = &mut self.report;
        r.other.take(None);
        r.seq = r.seq.wrapping_add(1);
        r.writer = r.front;
        let state = match next {
            Body::Clear => {
                r.cleared = r.seq;
                None
            }
            Body::Report(state) => {
                // A different app cannot end or resume the previous app's turn.
                let previous = r
                    .last
                    .filter(|last| last.app == state.app)
                    .map(|last| last.state);
                let edge = TurnEdge {
                    seq: r.seq,
                    wall_ms,
                };
                let began = |resumed| ProgramTurn {
                    n: 0,
                    app: state.app,
                    began: Some(edge),
                    resumed,
                    ended: None,
                };
                if begins_a_turn(previous, state.state) {
                    r.turns = r.turns.wrapping_add(1);
                    self.ring.push(began(false));
                } else if r.state.is_none() && in_a_turn(previous) && in_a_turn(Some(state.state)) {
                    // A `clear` or a full reset took an in-turn report down: the program took the
                    // terminal back mid-turn.
                    self.ring.push(began(true));
                } else if ends_a_turn(previous, state.state) {
                    self.ring.end(state, edge);
                }
                r.last = Some(state);
                Some(state)
            }
        };
        if state != r.state {
            r.began = r.seq;
        }
        r.state = state;
        true
    }

    /// The known programs' report standing now as the OSC 7501 body that says it
    /// (`state=<word>:app=<app>[:kind=<kind>]`), which a checkpoint carries ([`Self::restore`]);
    /// `None` while none stands ([`ProgramStateReport::carried`]).
    pub(super) fn carried(&self) -> Option<String> {
        self.report.carried()
    }

    /// The host says the bytes it parses next are process group `pgid`'s: the terminal's
    /// foreground group as it held the terminal when the host read them. A group of `0` or less
    /// says nothing. A report is its writer's group's, the group whose bytes carried it, and a
    /// report taken before any group was said (one a restore installed) is the first group's
    /// said. A group that relays another host's programs (an `ssh`) is one group here, so a
    /// report written through it stays its sign while it holds the front
    /// ([`ProgramStateReport::run_sign`]).
    #[inline]
    pub(super) const fn front_is(&mut self, pgid: i32) {
        if pgid <= 0 {
            return;
        }
        let r = &mut self.report;
        if r.writer == 0 {
            r.writer = pgid;
        }
        r.front = pgid;
    }

    /// Another program's report standing now ([`ProgramStateReport::other`]) as the body that says it, which a
    /// checkpoint carries beside [`Self::carried`]; `None` while none stands.
    pub(super) fn carried_other(&self) -> Option<String> {
        self.report.other.state.map(body_of)
    }

    /// Install the reports a checkpoint carried, each read by this reader's own parser: the known
    /// programs' (`body`, [`Self::carried`]) as a new report that begins no turn, a later report
    /// judged against it (a re-send of it keeps where it began), and another program's (`other`,
    /// [`Self::carried_other`]) in its own slot ([`ProgramStateReport::other`]). `None`, a body this reader does
    /// not take, or a body of the other kind than its slot's, leaves none standing there. The
    /// sequences only move forward. Installing a different known report drops the old turn
    /// backlog ([`ProgramTurns`]) and keeps a resumed turn with no wall clock when it is in a
    /// turn; restoring the known report standing keeps the backlog. An installed known report is
    /// the group's in front then, or the first group said where none was ([`Self::front_is`]).
    pub(super) fn restore(&mut self, body: Option<&str>, other: Option<&str>) {
        let other = match other.and_then(parse) {
            Some(Body::Report(state)) if !state.app.is_known() => Some(state),
            _ => None,
        };
        if other != self.report.other.state {
            self.report.other.take(other);
        }
        let state = match body.and_then(parse) {
            Some(Body::Report(state)) if state.app.is_known() => Some(state),
            Some(Body::Clear | Body::Report(_)) | None => None,
        };
        let r = &mut self.report;
        if state == r.state {
            return;
        }
        r.seq = r.seq.wrapping_add(1);
        r.began = r.seq;
        r.writer = r.front;
        r.state = state;
        self.ring.clear();
        if let Some(state) = state {
            r.last = Some(state);
            if in_a_turn(Some(state.state)) {
                self.ring.push(ProgramTurn {
                    n: 0,
                    app: state.app,
                    began: Some(TurnEdge {
                        seq: r.seq,
                        wall_ms: None,
                    }),
                    resumed: true,
                    ended: None,
                });
            }
        }
    }

    /// The shell drew its prompt (an OSC 133 or 633 `A` the engine took): no program runs in
    /// front of it, so another program's report ([`ProgramStateReport::other`]) is over, said with a `clear`
    /// or not. The known programs' record is left as it is.
    #[inline]
    pub(super) const fn prompt_drawn(&mut self) {
        self.report.other.take(None);
    }

    /// A full reset (RIS): no report stands, a known program's or another's.
    #[inline]
    pub(super) const fn reset(&mut self) {
        self.report.other.take(None);
        let r = &mut self.report;
        if r.state.is_some() {
            r.seq = r.seq.wrapping_add(1);
            r.began = r.seq;
            r.cleared = r.seq;
            r.state = None;
        }
    }
}

/// Whether `word` is a report in a turn (`working` or `blocked`).
#[inline]
const fn in_a_turn(word: Option<ProgramStateWord>) -> bool {
    matches!(
        word,
        Some(ProgramStateWord::Working | ProgramStateWord::Blocked)
    )
}

/// The OSC 7501 body that says `state` (`state=<word>:app=<app>[:kind=<kind>]`).
fn body_of(state: ProgramState) -> String {
    match state.kind {
        Some(kind) => format!(
            "state={}:app={}:kind={}",
            state.state.as_str(),
            state.app.as_str(),
            kind.as_str()
        ),
        None => format!("state={}:app={}", state.state.as_str(), state.app.as_str()),
    }
}

/// Whether a report of `next` begins a turn after `last`.
#[inline]
const fn begins_a_turn(last: Option<ProgramStateWord>, next: ProgramStateWord) -> bool {
    in_a_turn(Some(next)) && !in_a_turn(last)
}

/// Whether a report of `next` ends the turn running after `last`: a rest after a report in a turn.
#[inline]
const fn ends_a_turn(last: Option<ProgramStateWord>, next: ProgramStateWord) -> bool {
    !in_a_turn(Some(next)) && in_a_turn(last)
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
