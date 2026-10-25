// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 7501 program lifecycle state reports ([`ProgramStateReport::turns`]).
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

/// The engine's record of OSC 7501.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramStateReport {
    /// Moves with every accepted report, `clear` and full reset included.
    pub seq: u64,
    /// The `seq` of the report that began the one standing now: a report equal to the one
    /// before it (a writer's re-send) keeps it, so one box re-sent is still one box.
    pub began: u64,
    /// The source episode a checkpoint restored into this local report. A changed
    /// report or reset clears this lineage; a re-send of the same report keeps it.
    pub restored_from: Option<u64>,
    /// The report standing now; `None` after a `clear`, a reset, or before any.
    pub state: Option<ProgramState>,
    /// How many turns the accepted reports began: a `working` or `blocked` report begins one unless
    /// the report before it (a `clear` or a full reset looked past) was `working` or `blocked` too.
    pub turns: u64,
    /// The state of the latest report taken, a `clear` or a full reset looked past.
    last: Option<ProgramStateWord>,
}

impl ProgramStateReport {
    /// Take one OSC 7501 body (the text after `7501;`).
    pub(super) fn accept(&mut self, body: &str) -> bool {
        match parse(body) {
            Some(next) => {
                self.seq = self.seq.wrapping_add(1);
                let state = match next {
                    Body::Clear => None,
                    Body::Report(state) => {
                        if begins_a_turn(self.last, state.state) {
                            self.turns = self.turns.wrapping_add(1);
                        }
                        self.last = Some(state.state);
                        Some(state)
                    }
                };
                if state != self.state {
                    self.began = self.seq;
                    self.restored_from = None;
                }
                self.state = state;
                true
            }
            None => false,
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
    /// take, leaves none standing. The sequence only moves forward.
    pub(super) fn restore(&mut self, body: Option<&str>, source_began: Option<u64>) {
        let state = match body.and_then(parse) {
            Some(Body::Report(state)) => Some(state),
            Some(Body::Clear) | None => None,
        };
        if state == self.state {
            self.restored_from = source_began.filter(|began| *began > 0 && state.is_some());
            return;
        }
        self.seq = self.seq.wrapping_add(1);
        self.began = self.seq;
        self.state = state;
        self.restored_from = source_began.filter(|began| *began > 0 && state.is_some());
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
            self.restored_from = None;
        }
    }
}

/// Whether a report of `next` begins a turn after `last`.
#[inline]
const fn begins_a_turn(last: Option<ProgramStateWord>, next: ProgramStateWord) -> bool {
    use ProgramStateWord as W;
    matches!(next, W::Working | W::Blocked) && !matches!(last, Some(W::Working | W::Blocked))
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
mod tests {
    use super::*;

    fn state(s: ProgramStateWord, k: Option<ProgramBlockedKind>) -> Option<Body> {
        Some(Body::Report(ProgramState {
            state: s,
            kind: k,
            app: ProgramApp::Codex,
        }))
    }

    #[test]
    fn every_state_codex_sends_is_read() {
        use ProgramBlockedKind::*;
        use ProgramStateWord::*;
        assert_eq!(parse("state=idle:app=codex"), state(Idle, None));
        assert_eq!(parse("state=working:app=codex"), state(Working, None));
        assert_eq!(parse("state=blocked:app=codex"), state(Blocked, None));
        assert_eq!(parse("state=done:app=codex"), state(Done, None));
        assert_eq!(parse("state=error:app=codex"), state(Error, None));
        assert_eq!(
            parse("state=blocked:app=codex:kind=permission"),
            state(Blocked, Some(Permission))
        );
        assert_eq!(
            parse("state=blocked:app=codex:kind=question"),
            state(Blocked, Some(Question))
        );
        assert_eq!(
            parse("state=blocked:app=codex:kind=auth"),
            state(Blocked, Some(Auth))
        );
        assert_eq!(parse("state=clear"), Some(Body::Clear));
    }

    #[test]
    fn a_body_this_reader_does_not_take_changes_nothing() {
        for body in [
            "",
            "state=",
            "state=sleeping:app=codex",
            "state=idle",
            "state=idle:app=claude",
            "state=working:app=grok",
            "state=idle:app=codex:state=working",
            "state=blocked:app=codex:kind=coffee",
            "app=codex",
            "garbage",
        ] {
            assert_eq!(parse(body), None, "{body:?}");
            let mut report = ProgramStateReport::default();
            assert!(!report.accept(body), "{body:?}");
            assert_eq!(report, ProgramStateReport::default(), "{body:?}");
        }
        assert_eq!(parse(&"x".repeat(MAX_BODY_BYTES + 1)), None);
    }

    #[test]
    fn a_kind_is_kept_only_on_blocked_and_an_unknown_key_is_left_alone() {
        assert_eq!(
            parse("state=idle:app=codex:kind=permission"),
            state(ProgramStateWord::Idle, None)
        );
        assert_eq!(
            parse("state=working:app=codex:v=2"),
            state(ProgramStateWord::Working, None)
        );
    }

    #[test]
    fn the_sequence_moves_with_every_report_a_clear_and_a_reset() {
        let mut report = ProgramStateReport::default();
        assert!(report.accept("state=working:app=codex"));
        assert_eq!(report.seq, 1);
        assert!(report.accept("state=working:app=codex"));
        assert_eq!(report.seq, 2, "a repeated report is still a report");
        assert!(report.accept("state=clear"));
        assert_eq!((report.seq, report.state), (3, None));
        report.reset();
        assert_eq!(report.seq, 3, "a reset with nothing standing moves nothing");
        assert!(report.accept("state=idle:app=codex"));
        report.reset();
        assert_eq!((report.seq, report.state), (5, None));
    }

    /// WHERE THE STANDING REPORT BEGAN: a report equal to the one standing (a writer's re-send)
    /// keeps `began`; a different report, a kind that moved, a `clear` and a reset each begin
    /// anew. NEGATIVE CONTROL: the re-sends, whose `seq` moves while `began` does not.
    #[test]
    fn a_report_sent_again_keeps_where_it_began() {
        let mut report = ProgramStateReport::default();
        let mut take = |body: &str| {
            assert!(report.accept(body), "{body:?}");
            (report.seq, report.began)
        };
        assert_eq!(take("state=blocked:app=codex:kind=permission"), (1, 1));
        assert_eq!(
            take("state=blocked:app=codex:kind=permission"),
            (2, 1),
            "a re-send"
        );
        assert_eq!(
            take("state=blocked:app=codex:kind=question"),
            (3, 3),
            "the kind moved"
        );
        assert_eq!(take("state=working:app=codex"), (4, 4));
        assert_eq!(take("state=working:app=codex"), (5, 4), "a re-send");
        assert_eq!(take("state=clear"), (6, 6));
        assert_eq!(take("state=clear"), (7, 6), "a clear sent again");
        assert_eq!(take("state=blocked:app=codex"), (8, 8));
        report.reset();
        assert_eq!(
            (report.seq, report.began),
            (9, 9),
            "a reset begins none standing"
        );
    }

    /// A CARRIED REPORT: the standing report's body reads back as the same report, which
    /// begins no turn, and a re-send of it keeps where it began; a body this reader does not
    /// take, a `clear` and none leave none standing. NEGATIVE CONTROL: the same report taken
    /// as a first report begins a turn.
    #[test]
    fn a_carried_report_stands_again_and_begins_no_turn() {
        let mut live = ProgramStateReport::default();
        assert!(live.accept("state=blocked:app=codex:kind=permission"));
        assert_eq!(live.turns, 1, "CONTROL: a first report begins a turn");
        let body = live.carried();
        assert_eq!(
            body.as_deref(),
            Some("state=blocked:app=codex:kind=permission")
        );
        let mut restored = ProgramStateReport::default();
        restored.restore(body.as_deref(), Some(live.began));
        assert_eq!(restored.state, live.state);
        assert_eq!((restored.seq, restored.began, restored.turns), (1, 1, 0));
        assert!(restored.accept("state=blocked:app=codex:kind=permission"));
        assert_eq!(
            (restored.began, restored.turns),
            (1, 0),
            "a re-send of the carried box is the same box"
        );
        let numbering = (restored.seq, restored.began, restored.turns);
        restored.restore(body.as_deref(), Some(42));
        assert_eq!(
            restored.restored_from,
            Some(42),
            "an in-place restore replaces source lineage too"
        );
        assert_eq!((restored.seq, restored.began, restored.turns), numbering);
        restored.restore(body.as_deref(), None);
        assert_eq!(
            restored.restored_from, None,
            "an old checkpoint leaves no inherited identity"
        );
        assert_eq!((restored.seq, restored.began, restored.turns), numbering);
        assert!(restored.accept("state=working:app=codex"));
        assert_eq!(restored.turns, 0, "the carried turn goes on");
        for body in [None, Some("state=clear"), Some("state=sleeping:app=codex")] {
            let mut none = ProgramStateReport::default();
            none.restore(body, None);
            assert_eq!(none, ProgramStateReport::default(), "{body:?}");
        }
        let mut idle = ProgramStateReport::default();
        assert!(idle.accept("state=idle:app=codex"));
        assert_eq!(idle.carried().as_deref(), Some("state=idle:app=codex"));
        idle.restore(None, None);
        assert_eq!((idle.seq, idle.began, idle.state), (2, 2, None));
    }

    /// THE TURN COUNT: a turn begins at `working` or `blocked` after a report
    /// at rest (`idle`, `done`, `error`) or as the first report; a report
    /// written again, a move within a turn, and a report written again after
    /// a `clear` or a full reset (the program took the terminal back) begin
    /// none, while a turn that began across a `clear` does. NEGATIVE
    /// CONTROLS: every line that begins none.
    #[test]
    fn the_turn_count_moves_only_where_a_turn_begins() {
        let mut report = ProgramStateReport::default();
        let mut turns = |body: &str| {
            assert!(report.accept(body), "{body:?}");
            report.turns
        };
        let rows: [(&str, u64, &str); 17] = [
            ("state=working:app=codex", 1, "the first report, in a turn"),
            ("state=working:app=codex", 1, "written again: no move"),
            (
                "state=blocked:app=codex:kind=permission",
                1,
                "asked within it",
            ),
            ("state=working:app=codex", 1, "answered: the same turn"),
            ("state=done:app=codex", 1, "the turn ended"),
            ("state=working:app=codex", 2, "a turn after done"),
            ("state=idle:app=codex", 2, "ended"),
            (
                "state=blocked:app=codex:kind=question",
                3,
                "a turn that asked at once",
            ),
            ("state=error:app=codex", 3, "ended on an error"),
            ("state=working:app=codex", 4, "a turn after an error"),
            ("state=clear", 4, "a handoff mid-turn"),
            (
                "state=working:app=codex",
                4,
                "the terminal taken back: resumed",
            ),
            ("state=done:app=codex", 4, "ended"),
            ("state=clear", 4, "a handoff at rest"),
            ("state=done:app=codex", 4, "taken back: still at rest"),
            ("state=clear", 4, "a handoff at rest"),
            ("state=working:app=codex", 5, "a turn begun across it"),
        ];
        for (body, want, why) in rows {
            assert_eq!(turns(body), want, "{why}: {body}");
        }
        report.reset();
        assert!(report.accept("state=working:app=codex"));
        assert_eq!(report.turns, 5, "resumed after a reset");
        assert!(report.accept("state=idle:app=codex"));
        report.reset();
        assert!(report.accept("state=working:app=codex"));
        assert_eq!(report.turns, 6, "begun across a reset");
        assert!(!report.accept("state=idle:app=claude"));
        assert!(!report.accept("state=working:app=claude"));
        assert_eq!(report.turns, 6);
    }

    /// THE COUNT THROUGH THE PARSER, the bytes codex-alab writes: a turn
    /// that begins and ends between two reads of the engine leaves the same
    /// report standing (`done`) and the count one higher; a handoff mid-turn
    /// (`clear`, then `working` again) leaves it as it was.
    #[test]
    fn a_turn_between_two_reads_moves_the_count_and_a_handoff_does_not() {
        use crate::terminal::Terminal;
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7501;state=done:app=codex\x1b\\");
        let before = term.program_state();
        term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
        term.process(b"\x1b]7501;state=done:app=codex\x1b\\");
        let after = term.program_state();
        assert_eq!(after.state, before.state, "the same report stands");
        assert_eq!(after.turns, before.turns + 1, "and a turn ran");
        term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
        term.process(b"\x1b]7501;state=clear\x1b\\");
        term.process(b"\x1b]7501;state=working:app=codex\x1b\\");
        assert_eq!(term.program_state().turns, after.turns + 1, "one turn");
    }

    #[test]
    fn the_wire_word_carries_the_kind() {
        let blocked = ProgramState {
            state: ProgramStateWord::Blocked,
            kind: Some(ProgramBlockedKind::Permission),
            app: ProgramApp::Codex,
        };
        assert_eq!(blocked.wire(), "blocked:permission");
        let idle = ProgramState {
            state: ProgramStateWord::Idle,
            kind: None,
            app: ProgramApp::GrokBuild,
        };
        assert_eq!(idle.wire(), "idle");
    }

    /// The bytes as the two codex builds write them, through the real parser:
    /// vendor 0.163's format string (`\x1b]7501;state={}:app=codex\x1b\\`,
    /// read out of its shipped binary: idle, working and blocked, no kind)
    /// and codex-alab main's `SetTerminalProgramStatus` (adds done, error and
    /// a blocked report's kind), then `ClearTerminalProgramStatus`. A full
    /// reset clears what stands; a body with a `;` in it is no report.
    #[test]
    fn the_bytes_codex_writes_are_read_through_the_parser() {
        use crate::terminal::Terminal;
        use ProgramBlockedKind::*;
        use ProgramStateWord::*;
        let mut term = Terminal::new(24, 80);
        let seen = |term: &mut Terminal, bytes: &[u8]| {
            term.process(bytes);
            term.program_state().state
        };
        let vendor: [(&[u8], ProgramStateWord); 3] = [
            (b"\x1b]7501;state=idle:app=codex\x1b\\", Idle),
            (b"\x1b]7501;state=working:app=codex\x1b\\", Working),
            (b"\x1b]7501;state=blocked:app=codex\x1b\\", Blocked),
        ];
        for (bytes, want) in vendor {
            assert_eq!(
                seen(&mut term, bytes),
                Some(ProgramState {
                    state: want,
                    kind: None,
                    app: ProgramApp::Codex,
                })
            );
        }
        let alab: [(&[u8], ProgramStateWord, Option<ProgramBlockedKind>); 5] = [
            (b"\x1b]7501;state=done:app=codex\x1b\\", Done, None),
            (b"\x1b]7501;state=error:app=codex\x1b\\", Error, None),
            (
                b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\",
                Blocked,
                Some(Permission),
            ),
            (
                b"\x1b]7501;state=blocked:app=codex:kind=question\x1b\\",
                Blocked,
                Some(Question),
            ),
            (
                b"\x1b]7501;state=blocked:app=codex:kind=auth\x07",
                Blocked,
                Some(Auth),
            ),
        ];
        for (bytes, want, kind) in alab {
            assert_eq!(
                seen(&mut term, bytes),
                Some(ProgramState {
                    state: want,
                    kind,
                    app: ProgramApp::Codex,
                })
            );
        }
        let before = term.program_state().seq;
        assert_eq!(seen(&mut term, b"\x1b]7501;state=clear\x1b\\"), None);
        assert_eq!(term.program_state().seq, before + 1);
        seen(
            &mut term,
            b"\x1b]21337;status=Working;indicator=#ff9500;status-color=;detail=\x07",
        );
        assert_eq!(term.program_state().seq, before + 1);
        assert_eq!(
            seen(&mut term, b"\x1b]7501;state=idle;app=codex\x1b\\"),
            None
        );
        assert_eq!(term.program_state().seq, before + 1);
        seen(&mut term, b"\x1b]7501;state=working:app=codex\x1b\\");
        assert_eq!(seen(&mut term, b"\x1bc"), None);
        assert_eq!(term.program_state().seq, before + 3);
    }

    /// The bytes Grok Build 1.0.70 writes, through the real parser. Its binary holds the pieces
    /// of one report, `ESC ] 7501;state=` `:kind=permission` `:app=grok-build` `:title=`, its
    /// state words `working`, `blocked` and `error`, a child record's clear
    /// (`state=clear:id=`) and, at exit, `state=clear`; its question and sign-in waits name no
    /// kind (it holds no other). Its title is base64, with or without padding.
    #[test]
    fn the_root_record_grok_build_writes_is_read_through_the_parser() {
        use crate::terminal::Terminal;
        use ProgramStateWord::*;
        let grok = |state, kind| {
            Some(ProgramState {
                state,
                kind,
                app: ProgramApp::GrokBuild,
            })
        };
        let mut term = Terminal::new(24, 80);
        let rows: [(&[u8], Option<ProgramState>); 6] = [
            (
                b"\x1b]7501;state=working:app=grok-build:title=Rml4IHRoZSBwYXJzZXI\x1b\\",
                grok(Working, None),
            ),
            (
                b"\x1b]7501;state=blocked:kind=permission:app=grok-build:title=Rml4IHRoZSBwYXJzZXI=\x1b\\",
                grok(Blocked, Some(ProgramBlockedKind::Permission)),
            ),
            (
                b"\x1b]7501;state=blocked:app=grok-build:title=Rml4IHRoZSBwYXJzZXI\x1b\\",
                grok(Blocked, None),
            ),
            (
                b"\x1b]7501;state=done:app=grok-build:title=Rml4IHRoZSBwYXJzZXI\x07",
                grok(Done, None),
            ),
            (
                b"\x1b]7501;state=error:app=grok-build\x1b\\",
                grok(Error, None),
            ),
            (b"\x1b]7501;state=clear\x1b\\", None),
        ];
        for (bytes, want) in rows {
            term.process(bytes);
            assert_eq!(
                term.program_state().state,
                want,
                "{}",
                String::from_utf8_lossy(bytes)
            );
        }
        assert_eq!(term.program_state().turns, 1, "one turn, working to done");
    }

    /// A CHILD RECORD SAYS NOTHING OF THE SESSION: Grok Build gives each running subagent its own
    /// record (`id=<subagent>`), says it done when it finishes, and clears it with
    /// `state=clear:id=<subagent>`, all while the session's own turn runs. None of those is
    /// taken: the root `working` stands, its sequence does not move, and no turn ends or begins.
    /// NEGATIVE CONTROL: the same reports with no `id` are taken.
    #[test]
    fn a_child_records_report_and_clear_leave_the_root_record_standing() {
        use crate::terminal::Terminal;
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b]7501;state=working:app=grok-build\x1b\\");
        let root = term.program_state();
        for child in [
            &b"\x1b]7501;state=working:app=grok-build:id=sub_1:title=cmVhZA\x1b\\"[..],
            b"\x1b]7501;state=blocked:kind=permission:app=grok-build:id=sub_1\x1b\\",
            b"\x1b]7501;state=done:app=grok-build:id=sub_1\x1b\\",
            b"\x1b]7501;state=clear:id=sub_1\x1b\\",
            b"\x1b]7501;state=clear:id=s/sub_2\x1b\\",
        ] {
            term.process(child);
            assert_eq!(
                term.program_state(),
                root,
                "{}",
                String::from_utf8_lossy(child)
            );
        }
        term.process(b"\x1b]7501;state=done:app=grok-build\x1b\\");
        assert_eq!(
            term.program_state().state.map(|s| s.state),
            Some(ProgramStateWord::Done),
            "the root's own report is taken"
        );
        term.process(b"\x1b]7501;state=clear\x1b\\");
        assert_eq!(term.program_state().state, None, "the root's own clear is");
    }

    /// A CHECKPOINT CARRIES THE REPORT'S OWN PROGRAM: Grok Build's standing report is carried as
    /// `app=grok-build` and restored as the same report, a re-send of it keeping where it began.
    /// NEGATIVE CONTROL: codex's carries `app=codex`.
    #[test]
    fn a_carried_grok_build_report_restores_as_grok_builds() {
        let mut live = ProgramStateReport::default();
        assert!(live.accept("state=blocked:kind=permission:app=grok-build:title=cGFyc2Vy"));
        let body = live.carried();
        assert_eq!(
            body.as_deref(),
            Some("state=blocked:app=grok-build:kind=permission")
        );
        let mut restored = ProgramStateReport::default();
        restored.restore(body.as_deref(), Some(live.began));
        assert_eq!(restored.state, live.state);
        assert!(restored.accept("state=blocked:kind=permission:app=grok-build"));
        assert_eq!((restored.began, restored.turns), (1, 0), "the same box");
        let mut codex = ProgramStateReport::default();
        assert!(codex.accept("state=working:app=codex"));
        assert_eq!(codex.carried().as_deref(), Some("state=working:app=codex"));
    }

    /// A report as long as the protocol allows is a report: Grok Build's carries its title, up
    /// to 256 base64 bytes, beside a `msg` of up to 2,732. One byte past the cap is none.
    #[test]
    fn a_report_with_a_title_and_message_at_the_protocols_caps_is_taken() {
        let title = "A".repeat(256);
        let msg = "B".repeat(2732);
        let body = format!(
            "state=blocked:kind=permission:app=grok-build:progress=40:title={title}:msg={msg}"
        );
        assert_eq!(
            parse(&body),
            Some(Body::Report(ProgramState {
                state: ProgramStateWord::Blocked,
                kind: Some(ProgramBlockedKind::Permission),
                app: ProgramApp::GrokBuild,
            }))
        );
        let pad = MAX_BODY_BYTES - "state=done:app=grok-build:msg=".len();
        let edge = format!("state=done:app=grok-build:msg={}", "C".repeat(pad));
        assert_eq!(edge.len(), MAX_BODY_BYTES);
        assert!(parse(&edge).is_some(), "at the cap");
        assert_eq!(parse(&format!("{edge}C")), None, "past it");
    }
}
