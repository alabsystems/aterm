// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! OSC 7501: a program's own report of its lifecycle state.
//!
//! Codex writes `ESC ] 7501 ; state=<idle|working|blocked|done|error> :
//! app=codex [: kind=<permission|question|auth>] ST` as its state moves, and
//! `ESC ] 7501 ; state=clear ST` when it lets the terminal go. Vendor 0.163
//! sends `idle`, `working` and `blocked` with no kind; codex-alab's main also
//! sends `done`, `error` and a blocked report's kind.
//!
//! The engine keeps the last report and a sequence that moves with every report
//! it accepted (a `clear` and a full reset included), and nothing else: it is a
//! program's claim about itself, for display. Which reports a host honours (only
//! while the program in front is a `codex`, and only since the foreground group
//! last moved) is the host's to decide; the sequence is what lets it tell a
//! report made since then from one made before. A report this reader does not
//! take (no `state=`, an unknown state, an app other than `codex`) changes
//! nothing.

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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

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
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "permission" => Self::Permission,
            "question" => Self::Question,
            "auth" => Self::Auth,
            _ => return None,
        })
    }
}

/// One accepted OSC 7501 report (not a `clear`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProgramState {
    /// The state.
    pub state: ProgramStateWord,
    /// What a `blocked` report is blocked on, when it said; `None` on every
    /// other state.
    pub kind: Option<ProgramBlockedKind>,
}

impl ProgramState {
    /// `state[:kind]`, as `status program_state=` prints it.
    #[must_use]
    pub fn wire(self) -> String {
        match self.kind {
            Some(kind) => format!("{}:{}", self.state.as_str(), kind.as_str()),
            None => self.state.as_str().to_string(),
        }
    }
}

/// The engine's record of OSC 7501: the last accepted report, and how many
/// reports (and full resets) it has taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgramStateReport {
    /// Moves with every accepted report, `clear` and full reset included.
    pub seq: u64,
    /// The report standing now; `None` after a `clear`, a reset, or before any.
    pub state: Option<ProgramState>,
}

impl ProgramStateReport {
    /// Take one OSC 7501 body (the text after `7501;`). Whether it was taken.
    pub(super) fn accept(&mut self, body: &str) -> bool {
        match parse(body) {
            Some(next) => {
                self.seq = self.seq.wrapping_add(1);
                self.state = match next {
                    Body::Clear => None,
                    Body::Report(state) => Some(state),
                };
                true
            }
            None => false,
        }
    }

    /// A full reset (RIS): no report stands.
    pub(super) fn reset(&mut self) {
        if self.state.is_some() {
            self.seq = self.seq.wrapping_add(1);
            self.state = None;
        }
    }
}

/// The longest OSC 7501 body this reader looks at; anything longer is not a
/// report.
const MAX_BODY_BYTES: usize = 128;

/// One OSC 7501 body this reader takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Body {
    /// `state=clear`: no report stands.
    Clear,
    /// A report.
    Report(ProgramState),
}

/// What one OSC 7501 body says: [`Body::Clear`] for `state=clear`, [`Body::Report`] for a
/// report, `None` for a body this reader does not take.
fn parse(body: &str) -> Option<Body> {
    if body.len() > MAX_BODY_BYTES {
        return None;
    }
    let (mut state, mut app, mut kind) = (None, None, None);
    for item in body.split(':') {
        let (key, value) = item.split_once('=')?;
        match key {
            "state" if state.is_none() => state = Some(value),
            "app" if app.is_none() => app = Some(value),
            "kind" if kind.is_none() => kind = Some(value),
            // A key named twice is not a report.
            "state" | "app" | "kind" => return None,
            // A key this reader does not know is left alone.
            _ => {}
        }
    }
    if state? == "clear" {
        return Some(Body::Clear);
    }
    if app != Some("codex") {
        return None;
    }
    let state = ProgramStateWord::parse(state?)?;
    let kind = match (state, kind) {
        (ProgramStateWord::Blocked, Some(k)) => Some(ProgramBlockedKind::parse(k)?),
        (_, _) => None,
    };
    Some(Body::Report(ProgramState { state, kind }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(s: ProgramStateWord, k: Option<ProgramBlockedKind>) -> Option<Body> {
        Some(Body::Report(ProgramState { state: s, kind: k }))
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

    #[test]
    fn the_wire_word_carries_the_kind() {
        let blocked = ProgramState {
            state: ProgramStateWord::Blocked,
            kind: Some(ProgramBlockedKind::Permission),
        };
        assert_eq!(blocked.wire(), "blocked:permission");
        let idle = ProgramState {
            state: ProgramStateWord::Idle,
            kind: None,
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
                    kind: None
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
                Some(ProgramState { state: want, kind })
            );
        }
        let before = term.program_state().seq;
        assert_eq!(seen(&mut term, b"\x1b]7501;state=clear\x1b\\"), None);
        assert_eq!(term.program_state().seq, before + 1);
        // iTerm2's OSC 21337 beside it is not read as a report.
        seen(
            &mut term,
            b"\x1b]21337;status=Working;indicator=#ff9500;status-color=;detail=\x07",
        );
        assert_eq!(term.program_state().seq, before + 1);
        // A `;` in the body is no report.
        assert_eq!(
            seen(&mut term, b"\x1b]7501;state=idle;app=codex\x1b\\"),
            None
        );
        assert_eq!(term.program_state().seq, before + 1);
        // A full reset clears the report standing.
        seen(&mut term, b"\x1b]7501;state=working:app=codex\x1b\\");
        assert_eq!(seen(&mut term, b"\x1bc"), None);
        assert_eq!(term.program_state().seq, before + 3);
    }
}
