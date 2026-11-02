// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the one turn routine (`composer_turn`): scripted looks, sends and Enters, on an
//! injected clock.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use super::*;

/// What the scripted target was asked to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ev {
    Look,
    Send(String),
    Enter,
}

/// What the scripted target has seen so far, for its screen to depend on.
#[derive(Debug, Default)]
struct State {
    /// Looks before anything was sent.
    before: usize,
    /// The text sent.
    sent: Option<String>,
    /// Enters pressed.
    enters: u32,
    /// Looks since the last Enter.
    since_enter: u32,
}

/// A scripted target: `screen` draws each look (`None`: the screen could not be read), and each
/// look is read by the real [`stand_of`] for `agent` over `cell`'s replies (`ctl cell <row> <col>`
/// as the target stood at that look).
struct Script<F: Fn(&State) -> Option<String>> {
    agent: &'static str,
    screen: F,
    cell: fn(&State, usize, usize) -> Option<String>,
    state: RefCell<State>,
    events: RefCell<Vec<Ev>>,
    ms: Cell<u64>,
    t0: Instant,
}

impl<F: Fn(&State) -> Option<String>> Script<F> {
    fn new(screen: F) -> Self {
        Self {
            agent: "claude",
            screen,
            cell: |_, row, col| plain(row, col),
            state: RefCell::new(State::default()),
            events: RefCell::new(Vec::new()),
            ms: Cell::new(0),
            t0: Instant::now(),
        }
    }

    /// The same target running `agent`, its cells drawn by `cell`.
    fn with(
        mut self,
        agent: &'static str,
        cell: fn(&State, usize, usize) -> Option<String>,
    ) -> Self {
        self.agent = agent;
        self.cell = cell;
        self
    }

    fn events(&self) -> Vec<Ev> {
        self.events.borrow().clone()
    }

    fn sends(&self) -> usize {
        self.events()
            .iter()
            .filter(|e| matches!(e, Ev::Send(_)))
            .count()
    }

    fn enters(&self) -> usize {
        self.events().iter().filter(|e| **e == Ev::Enter).count()
    }
}

impl<F: Fn(&State) -> Option<String>> Steps for Script<F> {
    fn look(&self) -> Look {
        self.events.borrow_mut().push(Ev::Look);
        let seen = {
            let s = self.state.borrow();
            (self.screen)(&s).map(|screen| {
                let stand = stand_of(self.agent, &screen, |row, col| (self.cell)(&s, row, col));
                (screen, stand)
            })
        };
        {
            let mut s = self.state.borrow_mut();
            if s.sent.is_none() {
                s.before += 1;
            }
            if s.enters > 0 {
                s.since_enter += 1;
            }
        }
        seen
    }

    fn send(&self, text: &str) -> Result<bool, Fail> {
        self.events.borrow_mut().push(Ev::Send(text.to_string()));
        self.state.borrow_mut().sent = Some(text.to_string());
        Ok(true)
    }

    fn enter(&self) -> Result<bool, Fail> {
        self.events.borrow_mut().push(Ev::Enter);
        let mut s = self.state.borrow_mut();
        s.enters += 1;
        s.since_enter = 0;
        Ok(true)
    }

    fn now(&self) -> Instant {
        self.t0 + Duration::from_millis(self.ms.get())
    }

    fn pause(&self, d: Duration) -> Result<Instant, stop::Stopped> {
        self.ms
            .set(self.ms.get() + u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Ok(self.now())
    }
}

/// Every cell drawn plain, as typed text is.
fn plain(_row: usize, _col: usize) -> Option<String> {
    Some("OK h d0d0d0 111318 none".to_string())
}

/// Claude Code's banner, drawn before its composer (the rows 2.1.300 drew, its version moved on).
fn banner() -> String {
    [
        " ▐▛███▛█   Claude Code v2.1.312",
        "▝▜██████▀  Sonnet 5.5 · Claude API",
        " ▝▝   ▝▝   /private/tmp/x/proj",
        "",
    ]
    .join("\n")
}

/// The banner over Claude Code's composer holding `text` (empty: nothing).
fn composer(text: &str) -> String {
    let rule = "─".repeat(110);
    format!(
        "{}\n\n{rule}\n❯\u{a0}{text}\n{rule}\n  ⏵⏵ auto mode on (shift+tab to cycle)",
        banner()
    )
}

/// Claude Code 2.1.283's folder-trust dialog, as measured (`ctl text` of a fresh pane).
fn trust() -> String {
    include_str!("../../aterm-phase/src/fixtures/claude-2.1.283-trust-fresh-pane.txt")
        .lines()
        .skip_while(|l| l.starts_with("# "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// THE TURN IS TYPED ONLY INTO A COMPOSER THAT STANDS EMPTY, AND ENTERED ONLY ONCE THE TEXT STANDS
/// THERE (E6-g5): `lab claude` returns on the folder-trust dialog or the banner, before any
/// composer; the first two looks find none, the third the empty composer, and only then is the
/// text sent; Enter waits for the look that finds the text in the composer, and the look after it
/// finds the composer empty, the turn taken.
#[test]
fn a_turn_is_sent_only_once_an_empty_composer_stands_and_entered_once_the_text_stands() {
    let (trust, banner, empty, typed) = (trust(), banner(), composer(""), composer("hi"));
    for (what, screen, stands) in [
        ("the folder-trust dialog", &trust, Stand::Starting),
        ("the banner", &banner, Stand::Starting),
        ("the empty composer", &empty, Stand::Composer(String::new())),
        (
            "the composer holding it",
            &typed,
            Stand::Composer("hi".into()),
        ),
    ] {
        assert_eq!(stand_of("claude", screen, plain), stands, "{what}");
    }
    let script = Script::new(|s: &State| {
        Some(match (s.sent.as_deref(), s.enters) {
            (None, _) => [&trust, &banner, &empty][s.before.min(2)].clone(),
            (Some(text), 0) => composer(text),
            (Some(_), _) => empty.clone(),
        })
    });
    let turned = turn(&script, "hi").ok();
    assert_eq!(
        script.events(),
        [
            Ev::Look,
            Ev::Look,
            Ev::Look,
            Ev::Send("hi".into()),
            Ev::Look,
            Ev::Enter,
            Ev::Look,
        ],
        "sent after the third look, Enter once the text stood"
    );
    assert_eq!(turned, Some(Turned::Taken));
}

/// A COMPOSER HOLDING A DRAFT NO ONE TYPED, OR AN EDITOR HOLDING THE TAB, ENDS THE TURN AT ONCE AND
/// NOTHING IS SENT: the failure names the draft or the editor's status row, never the screen.
#[test]
fn a_draft_or_an_editor_no_one_typed_ends_the_turn_at_once_and_nothing_is_sent() {
    let drafted = Script::new(|s: &State| {
        Some(if s.before == 0 {
            banner()
        } else {
            composer("QZP7DRAFT1")
        })
    });
    let turned = turn(&drafted, "hi").ok();
    assert_eq!(
        turned,
        Some(Turned::NotReady {
            wait: ComposerWait::Draft("QZP7DRAFT1".into()),
            last: None,
        })
    );
    assert_eq!(drafted.events(), [Ev::Look, Ev::Look], "ended at the look");
    let why = turned
        .and_then(|t| t.why("claude", "hi"))
        .unwrap_or_default();
    assert!(why.contains("QZP7DRAFT1"), "the draft named: {why}");
    assert!(!why.contains("Claude Code v2.1.312"), "no screen: {why}");

    let status = "\"x.md\" 1L, 4B";
    let vim = Script::new(move |_: &State| {
        let mut rows = vec!["note"];
        rows.extend(["~"; 6]);
        rows.push(status);
        Some(rows.join("\n"))
    });
    assert_eq!(
        turn(&vim, "hi").ok(),
        Some(Turned::NotReady {
            wait: ComposerWait::Editor(status.into()),
            last: None,
        })
    );
    assert_eq!(vim.sends(), 0);
}

/// A SCREEN THAT STANDS STILL A MINUTE WITH NO COMPOSER IS A HANG, NAMED BY THE LAST ROW IT READ: a
/// folder-trust dialog no one answers ends the wait at [`COMPOSER_HANG`] with its key row, nothing
/// sent; a screen that cannot be read at all is waited on the same way.
#[test]
fn a_screen_standing_still_with_no_composer_is_a_hang_named_by_its_last_row() {
    let trust = trust();
    let held = Script::new(|_: &State| Some(trust.clone()));
    let turned = turn(&held, "hi").ok();
    assert_eq!(
        turned,
        Some(Turned::NotReady {
            wait: ComposerWait::Hung,
            last: Some("Enter to confirm · Esc to cancel".into()),
        })
    );
    assert_eq!(held.sends(), 0);
    assert_eq!(held.ms.get(), 60_000, "a minute of a screen standing still");
    let why = turned
        .and_then(|t| t.why("claude", "hi"))
        .unwrap_or_default();
    assert!(
        why.contains("Enter to confirm"),
        "its last row named: {why}"
    );
    assert!(!why.contains("Accessing workspace"), "one row: {why}");

    let unread = Script::new(|_: &State| None);
    assert_eq!(
        turn(&unread, "hi").ok(),
        Some(Turned::NotReady {
            wait: ComposerWait::Hung,
            last: None,
        })
    );
    assert_eq!(unread.sends(), 0);
}

/// ENTER IS PRESSED ONLY ONCE THE TEXT STANDS IN THE COMPOSER: a text the program never shows there
/// within [`DRAFT_HANG`] is never entered, and the failure names the composer it read.
#[test]
fn a_text_that_never_stands_in_the_composer_is_never_entered() {
    let ignored = Script::new(|_: &State| Some(composer("")));
    let turned = turn(&ignored, "hi").ok();
    assert_eq!(turned, Some(Turned::NotStood(Some(String::new()))));
    assert_eq!((ignored.sends(), ignored.enters()), (1, 0));
    assert_eq!(Duration::from_millis(ignored.ms.get()), DRAFT_HANG);
}

/// ENTER AGAIN WHERE THE TEXT STILL STANDS 5 S ON, THEN A HANG DETECTOR FOR THE TAKE: a program
/// that reads its input late takes the text after the second Enter; one that never takes it, or
/// whose screen cannot be read after Enter, is no turn taken.
#[test]
fn enter_is_pressed_again_where_the_text_still_stands_and_a_late_take_is_waited_for() {
    // Taken 30 looks after the second Enter (3 s at 100 ms).
    let late = Script::new(|s: &State| {
        Some(match (s.sent.as_deref(), s.enters) {
            (None, _) => composer(""),
            (Some(_), 2) if s.since_enter >= 30 => composer(""),
            (Some(text), _) => composer(text),
        })
    });
    assert_eq!(turn(&late, "hi").ok(), Some(Turned::Taken));
    assert_eq!((late.sends(), late.enters()), (1, 2));

    let never = Script::new(|s: &State| Some(composer(s.sent.as_deref().unwrap_or_default())));
    assert_eq!(turn(&never, "hi").ok(), Some(Turned::StillStands));
    assert_eq!(never.enters(), 2, "two Enters, never a third");

    let blind = Script::new(|s: &State| match (s.sent.as_deref(), s.enters) {
        (None, _) => Some(composer("")),
        (Some(text), 0) => Some(composer(text)),
        (Some(_), _) => None,
    });
    assert_eq!(turn(&blind, "hi").ok(), Some(Turned::Unread));
}

/// A PLACEHOLDER DRAWN DIM IS AN EMPTY COMPOSER, TYPED TEXT IS NOT: `ctl text` reads Codex's hint
/// and Claude Code's grey suggestion as text, so the composer's first glyph is read by its cell on
/// the composer's own row, past a caret the program draws itself; a cell that says nothing, or an
/// unfinished frame's, is no evidence yet.
#[test]
fn a_placeholder_drawn_dim_is_an_empty_composer_and_typed_text_is_not() {
    let codex = [
        "› QUESTION: plan the database migration",
        "",
        "• Hi! I am the local lab stub, not a model. (reply 3)",
        "",
        "› Ask Codex to do anything",
        "",
        "  gpt-6.1-sol medium · /private/tmp/x/proj",
    ]
    .join("\n");
    let asked = RefCell::new(Vec::new());
    let dim = |row: usize, col: usize| {
        asked.borrow_mut().push((row, col));
        Some("OK A 999999 111318 dim".to_string())
    };
    assert_eq!(
        stand_of("codex", &codex, dim),
        Stand::Composer(String::new())
    );
    assert_eq!(
        *asked.borrow(),
        [(4, 2)],
        "the composer's row, its first glyph"
    );
    assert_eq!(
        stand_of("codex", &codex, plain),
        Stand::Composer("Ask Codex to do anything".into())
    );
    assert_eq!(stand_of("codex", &codex, |_, _| None), Stand::Unknown);
    assert_eq!(
        stand_of("codex", &codex, |_, _| Some(
            "OK A 999999 111318 dim frame=open".into()
        )),
        Stand::Unknown
    );
    // Claude Code 2.1.310 draws its caret inverse over the suggestion's first glyph, the rest dim.
    let suggested = composer("run the tests");
    let caret = |_: usize, col: usize| {
        Some(if col == 2 {
            "OK r 111318 d0d0d0 inverse".to_string()
        } else {
            "OK u 999999 111318 dim".to_string()
        })
    };
    assert_eq!(
        stand_of("claude", &suggested, caret),
        Stand::Composer(String::new())
    );
    assert_eq!(
        stand_of("claude", &suggested, plain),
        Stand::Composer("run the tests".into())
    );
    assert_eq!(
        stand_of("claude", &composer(""), |_, _| None),
        Stand::Composer(String::new()),
        "an empty composer needs no cell"
    );
}

/// Muse Code 1.4.10's idle composer after a turn, as measured: its prompt hint
/// `❯ /export saves the conversation to a text file`, drawn in 676c74 with no `dim` attribute.
fn muse_hint() -> String {
    include_str!(
        "../../aterm-phase/src/fixtures/muse-1.4.10-R6427.1-end-of-turn-footnotes-hint.txt"
    )
    .lines()
    .skip_while(|l| l.starts_with("# "))
    .collect::<Vec<_>>()
    .join("\n")
}

/// The hint's first glyph's cell, as measured: Muse Code's hint colour, no attribute.
const MUSE_HINT_CELL: &str = "OK / 676c74 111318 none";

/// A glyph Muse Code draws as typed text (ccd3db, as measured).
const MUSE_TYPED_CELL: &str = "OK h ccd3db 111318 none";

/// MUSE CODE'S PROMPT HINT IS AN EMPTY COMPOSER, ITS TYPED TEXT IS NOT (review R1): Muse Code
/// draws its idle composer's hint in its hint colour with no `dim` attribute, so the dim rule
/// alone read the hint as a draft no one typed and no Muse turn after a first was typed. Read by
/// the agent's own reader's hint colour, the hint stands empty, on the composer's row, and the
/// turn is typed into it and taken; a glyph drawn as typed text is still a draft, and nothing is
/// sent over it.
#[test]
fn muse_code_s_prompt_hint_is_an_empty_composer_and_its_typed_text_is_not() {
    let hint = muse_hint();
    let row = hint.lines().position(|l| l.starts_with("❯ /export "));
    assert!(row.is_some(), "the measured hint row");
    let asked = RefCell::new(Vec::new());
    let drawn = |row: usize, col: usize| {
        asked.borrow_mut().push((row, col));
        Some(MUSE_HINT_CELL.to_string())
    };
    assert_eq!(
        stand_of("muse", &hint, drawn),
        Stand::Composer(String::new())
    );
    assert_eq!(
        asked.borrow().clone(),
        row.map(|r| (r, 2)).into_iter().collect::<Vec<_>>()
    );
    assert_eq!(
        stand_of("muse", &hint, |_, _| Some(MUSE_TYPED_CELL.into())),
        Stand::Composer("/export saves the conversation to a text file".into())
    );

    let typed = |text: &str| {
        hint.replace(
            "❯ /export saves the conversation to a text file",
            &format!("❯ {text}"),
        )
    };
    let script = Script::new(|s: &State| {
        Some(match (s.sent.as_deref(), s.enters) {
            (Some(text), 0) => typed(text),
            _ => hint.clone(),
        })
    })
    .with("muse", |s, _, _| {
        Some(
            if s.sent.is_some() && s.enters == 0 {
                MUSE_TYPED_CELL
            } else {
                MUSE_HINT_CELL
            }
            .into(),
        )
    });
    assert_eq!(turn(&script, "hi").ok(), Some(Turned::Taken));
    assert_eq!(
        script.events(),
        [
            Ev::Look,
            Ev::Send("hi".into()),
            Ev::Look,
            Ev::Enter,
            Ev::Look
        ],
        "typed over the hint, entered once it stood"
    );

    let drafted = Script::new(|_: &State| Some(hint.clone()))
        .with("muse", |_, _, _| Some(MUSE_TYPED_CELL.into()));
    assert_eq!(
        turn(&drafted, "hi").ok(),
        Some(Turned::NotReady {
            wait: ComposerWait::Draft("/export saves the conversation to a text file".into()),
            last: None,
        })
    );
    assert_eq!(drafted.sends(), 0);
}

/// A COMPOSER WHOSE CELL CANNOT BE READ AFTER ENTER IS NO TAKE (review R2): a look whose glyph cell
/// gave no reply says nothing of what the composer holds, so it is looked past. One such look after
/// the first Enter, the text standing all along, is not the text taken: Enter is pressed again and
/// the text still stands; a composer never read again after Enter is a turn not known to be sent.
#[test]
fn a_composer_cell_unread_after_enter_is_no_take() {
    let standing = |s: &State| Some(composer(s.sent.as_deref().unwrap_or_default()));
    let once = Script::new(standing).with("claude", |s, row, col| {
        (s.enters != 1 || s.since_enter != 0)
            .then(|| plain(row, col))
            .flatten()
    });
    assert_eq!(turn(&once, "hi").ok(), Some(Turned::StillStands));
    assert_eq!(
        once.enters(),
        2,
        "Enter pressed again over the text still standing"
    );

    let never = Script::new(standing).with("claude", |s, row, col| {
        (s.enters == 0).then(|| plain(row, col)).flatten()
    });
    assert_eq!(turn(&never, "hi").ok(), Some(Turned::Unread));
    assert_eq!(never.enters(), 2);
}
