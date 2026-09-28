// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows in-window CLOSE / QUIT CONFIRMATION banner — the destructive-close
//! confirm composed into the window's own top rows, the way [`crate::paste_banner`]
//! composes the multi-line-paste question.
//!
//! # The defect this replaces (measured 2026-09-22, Windows audit)
//!
//! `invoke CloseTab` on a last tab with a running job raised a NATIVE task-modal
//! dialog on the main thread (`TaskDialogIndirect`, window class #32770, caption
//! "aterm", buttons Quit / Cancel). A modal dialog runs its own message loop
//! INSIDE the call, so winit's event loop — the one thread every control verb's
//! `call_main` hop waits on — did not turn until a human clicked. While the dialog
//! stood, EVERY main-thread ctl verb answered `ERR main-thread reply did not
//! arrive within 30s` (`window: timed out`), nothing reported that a dialog was
//! pending, and `controls` could not show it. For a human it was a modal; for an
//! agent driving the window it was a wedge — and the agent's own `invoke` was what
//! raised it, so it could not even know to click.
//!
//! # The shape
//!
//! The question is PARKED on `App::close_banner` ([`PendingClose`]) and the gesture
//! is REFUSED for now (`confirm_destructive_close` answers `false`): the event loop
//! keeps turning, so ctl keeps answering, and `controls front` reports the pending
//! confirm with the wire way to answer it ([`PendingClose::controls_line`]). Enter
//! proceeds — by REPLAYING the gesture ([`CloseReplay`]) under the `Programmatic`
//! close policy, so the question is never asked twice — and Escape (or a click on
//! the band) cancels. The keystroke decision is [`crate::alert_keys::confirm_key`],
//! the same pure router the macOS sheet and the Linux paste banner answer through.
//!
//! The wire answers with its own verb, `confirm yes|no` ([`WIRE_ACCEPT`] /
//! [`WIRE_CANCEL`], served by `App::answer_close_confirm_from_wire`), and never with
//! a keystroke: a wire `key enter` is always the PTY's. An earlier cut answered the
//! question with the wire's `key enter` / `key escape`, and review found two ways
//! that failed. `key enter` is the most common keystroke an agent sends, so one that
//! carried on with its routine `send <command>` + `key enter` after missing the
//! `invoke` refusal quit aterm and killed the job, with a reply (`OK`) identical to
//! typing Enter. And a flagless key lands in the FRONT window, so once the person
//! brought another window forward, the printed `answer="key enter"` ran whatever
//! was typed at that window's prompt instead. There is one question per instance, so
//! the verb needs no aim and its reply says what it did.
//!
//! The wire answers only a question the WIRE asked ([`PendingClose::wire_may_answer`]):
//! a control client's `invoke CloseTab` or `invoke Quit`. A question a person asked
//! at the window (Ctrl-W, Alt+F4, the caption ✕, the Quit menu) waits for that
//! person: `confirm` refuses it, so no driver answers a question it was never asked.
//!
//! Pure + geometry-injected like `paste_banner`: the row builder takes explicit
//! `cols`/`panel_rows` so it unit-tests without a window. No TTL and no
//! auto-dismiss — a destructive question does not answer itself; the banner stands
//! until a key, a click, the wire's `confirm`, or the window closing (which drops
//! it: that close was the thing being asked about) answers it. ONE at a time: a second destructive gesture
//! while one stands is refused and the standing question keeps its answer, which is
//! what the disabled owner window of the old modal enforced.
//!
//! The macOS `NSAlert` path is untouched, deliberately and not because it is
//! immune: `menu::confirm` answers inside `runModal`, and control wakes queue
//! behind that nested run loop the same way (`menu::confirm_owner` says so). This
//! change is scoped to the platform where the wedge was measured; the Linux
//! titlebar-warning confirm never blocks.

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;

use crate::chrome_band;
use crate::quit_safety::ConfirmPrompt;
use crate::session_store::ExitActor;
use crate::settings::{blank_row, write_str};

/// What Enter REPLAYS: the gesture the banner stood in for, re-run under the
/// `Programmatic` close policy (see `App::replay_confirmed_close`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReplay {
    /// Close window `wid`: `App::close_window`, which exits the app iff it was the
    /// last window. Every spelling of a last-tab close (Ctrl-W, the strip's ✕,
    /// `invoke CloseTab`, the tab menu) escalates to exactly this once its confirm
    /// passes, and Alt+F4 / the caption ✕ ARE this — so replaying the window close
    /// is replaying the gesture, with the window's own native/document barriers.
    Window(crate::WindowId),
    /// Quit the whole program (the Quit action): `App::on_quit_requested`, whose
    /// quit-scope barriers a window close does not run.
    Quit,
}

/// ONE parked close/quit confirmation awaiting an answer. Held by `App` in
/// `Option<PendingClose>`: `Some` ⇔ the banner is up over `wid`'s top rows and the
/// gesture's answer is still owed.
pub(crate) struct PendingClose {
    /// The window the banner splices into — the one the gesture would close (or,
    /// for a quit, the one it was anchored to); only ITS keys answer it, and the
    /// entry dies with it.
    pub(crate) wid: crate::WindowId,
    /// The copy the native dialog would have shown — title, body, and the verb
    /// on the affirmative button.
    pub(crate) prompt: ConfirmPrompt,
    /// What Enter replays.
    pub(crate) replay: CloseReplay,
    /// Who asked, as the exit ledger's `by=` names it: the control client whose
    /// gesture raised the question (`ctl`, or its sid), or [`ExitActor::Human`]
    /// for a gesture made at the window. Decides whether the wire may answer.
    pub(crate) asked_by: ExitActor,
}

/// The banner is two rows: the question with the answer keys, then the body.
pub(crate) const BANNER_ROWS: usize = 2;
/// Left margin, in cells (mirrors `paste_banner`).
const MARGIN: usize = 2;
/// The body's hanging indent: under the title text, past the `!  ` mark.
const BODY_INDENT: usize = MARGIN + 3;

/// The wire spellings that answer the question, printed by [`PendingClose::controls_line`].
/// Named once so the report and the verb that honours them cannot drift apart.
pub(crate) const WIRE_ACCEPT: &str = "confirm yes";
pub(crate) const WIRE_CANCEL: &str = "confirm no";
/// The `controls front` line when no confirm is pending — the same `open=false`
/// shape the other surfaces report (`overlay open=false`, `tab-menu open=false`).
pub(crate) const CONTROLS_CLOSED: &str = "confirm open=false";
/// The `confirm` verb's refusal when no question stands.
pub(crate) const NOTHING_PENDING: &str = "nothing is waiting for an answer";

/// The wire `kind=` of a prompt: what the affirmative answer DOES. A last-tab close
/// of the last window quits the app, and on Windows its prompt says so (`proceed ==
/// "Quit"`), so the kind follows the prompt rather than the gesture that raised it.
pub(crate) fn kind(prompt: &ConfirmPrompt) -> &'static str {
    match prompt.proceed {
        "Quit" => "quit",
        _ => "close-window",
    }
}

/// The pending gesture in words, for the wire's refusals: what [`kind`] says as
/// a token.
fn gesture(prompt: &ConfirmPrompt) -> &'static str {
    match prompt.proceed {
        "Quit" => "a quit",
        _ => "a window close",
    }
}

/// The QUESTION row's text: the prompt's title behind the warn mark.
pub(crate) fn question(prompt: &ConfirmPrompt) -> String {
    format!("!  {}", prompt.title)
}

/// The two keys that answer the question, right-aligned on the title row and
/// spoken as the a11y question's description: the affirmative key names the
/// prompt's REAL verb (the reason the native dialog was a `TaskDialog` and not a
/// Yes/No `MessageBoxW`), so "Enter quits" over a quit prompt, "Enter closes"
/// over a close.
pub(crate) fn answer_keys(prompt: &ConfirmPrompt) -> String {
    let verb = match prompt.proceed {
        "Quit" => "quits",
        _ => "closes",
    };
    format!("Enter {verb} \u{00b7} Esc cancels")
}

impl PendingClose {
    /// `asked_by` is the close attribution open when the gesture ran
    /// (`session_store::current_close_attribution`). A gesture at the window
    /// that opens no scope of its own (the Quit menu, the palette) arrives
    /// [`ExitActor::Unknown`], and is a person's all the same.
    pub(crate) fn new(
        wid: crate::WindowId,
        prompt: ConfirmPrompt,
        replay: CloseReplay,
        asked_by: ExitActor,
    ) -> Self {
        let asked_by = match asked_by {
            ExitActor::Unknown => ExitActor::Human,
            other => other,
        };
        Self {
            wid,
            prompt,
            replay,
            asked_by,
        }
    }

    /// Rows the banner wants (the splice clamps to the frame).
    pub(crate) fn wanted_rows(&self) -> usize {
        BANNER_ROWS
    }

    /// Whether the wire's `confirm yes|no` answers this question: only when a
    /// control client asked it. A person's question is theirs to answer at the
    /// window. Exhaustive on purpose, so a new kind of caller is classified
    /// before it compiles.
    pub(crate) fn wire_may_answer(&self) -> bool {
        match &self.asked_by {
            ExitActor::Sid(_) | ExitActor::Ctl => true,
            #[cfg(any(unix, test))]
            ExitActor::Bridge => true,
            ExitActor::Human | ExitActor::Unknown => false,
        }
    }

    /// The wire spellings that answer this question, or `-` for a question the
    /// wire may not answer.
    fn wire_answers(&self) -> (&'static str, &'static str) {
        if self.wire_may_answer() {
            (WIRE_ACCEPT, WIRE_CANCEL)
        } else {
            ("-", "-")
        }
    }

    /// The `controls front` report of this pending confirm: what is being asked,
    /// over which window, who asked, and — the part an agent needs — how to
    /// answer it from the wire (`answer=-` when the person who asked answers
    /// it). One line, `k=v` tokens; the title is quoted because it carries spaces.
    pub(crate) fn controls_line(&self) -> String {
        let (answer, cancel) = self.wire_answers();
        format!(
            "confirm open=true kind={} window={} by={} title=\"{}\" proceed={} answer=\"{}\" \
             cancel=\"{}\"",
            kind(&self.prompt),
            self.wid.0,
            crate::control::pct_encode(self.asked_by.as_wire()),
            self.prompt.title,
            self.prompt.proceed,
            answer,
            cancel,
        )
    }

    /// The wire refusal of a gesture this question stands in the way of: the
    /// gesture that PARKED it, or a second one refused while it stands — and the
    /// `confirm` verb's refusal of a question it may not answer. `invoke CloseTab`
    /// mints a reply from `pending_action_refusal`, and `OK` over a tab that is
    /// still there would tell a driver the close happened. The state in words,
    /// then the one action; `controls front` carries the rest.
    pub(crate) fn wire_refusal(&self) -> String {
        let gesture = gesture(&self.prompt);
        if self.wire_may_answer() {
            format!("{gesture} is waiting for an answer in the window; `confirm yes|no` answers it")
        } else {
            format!("{gesture} is waiting for the person at the window")
        }
    }

    /// The `confirm` verb's reply once it has answered: what it answered and what
    /// the question was, so a driver can tell a quit it confirmed from one it
    /// kept (`answered=yes kind=quit`).
    pub(crate) fn wire_answered(&self, proceed: bool) -> String {
        format!(
            "answered={} kind={}",
            if proceed { "yes" } else { "no" },
            kind(&self.prompt)
        )
    }
}

/// The `confirm` verb's argument: `yes` proceeds, `no` keeps everything. PURE, so
/// the grammar is pinned without an event loop; the `Err` is the whole usage
/// line, so the refusal teaches it.
pub(crate) fn parse_wire_answer(rest: &str) -> Result<bool, String> {
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some("yes"), None) => Ok(true),
        (Some("no"), None) => Ok(false),
        _ => Err("usage: confirm yes|no".to_string()),
    }
}

/// `s` cut to `max` cells with a trailing ellipsis when it does not fit (the
/// `paste_banner` shape: a hard cut reads like corruption, an ellipsis says "there
/// is more").
fn ellipsized(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// PURE grid-cell row builder: exactly `panel_rows` rows, each exactly `cols` wide,
/// so the splice overwrites frame rows in place (the `config_notice` contract).
/// Row 0 carries the question ([`question`]) with the two answer keys
/// ([`answer_keys`]) right-aligned; row 1 the prompt's body, ellipsized to fit.
pub(crate) fn banner_rows(
    prompt: &ConfirmPrompt,
    cols: usize,
    panel_rows: usize,
    theme: Theme,
) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(theme);
    let mut rows: Vec<Vec<RenderCell>> = (0..panel_rows)
        .map(|r| blank_row(cols, c.label, c.bar_bg, r == 0))
        .collect();
    if panel_rows == 0 {
        return rows;
    }
    let title = ellipsized(&question(prompt), cols.saturating_sub(MARGIN));
    write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
    // `value` (not the dim `label`): the answer keys are not an aside — they are
    // the only way to answer, so they read at full tone.
    let keys = answer_keys(prompt);
    let keys_col = cols.saturating_sub(keys.chars().count() + MARGIN);
    if keys_col > MARGIN + title.chars().count() + 2 {
        write_str(&mut rows[0], cols, keys_col, &keys, c.value, c.bar_bg, true);
    }
    if panel_rows > 1 {
        let text_w = cols.saturating_sub(BODY_INDENT + MARGIN);
        write_str(
            &mut rows[1],
            cols,
            BODY_INDENT,
            &ellipsized(prompt.body, text_w),
            c.value,
            c.bar_bg,
            false,
        );
    }
    // The seam is the whole top edge, in one tone (the `paste_banner` lesson:
    // `write_str` rebuilds every cell it touches without the overline, so the rule
    // has to be sealed after the words are written).
    chrome_band::seal_band_top(&mut rows[0], c.label);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    fn quit_prompt() -> ConfirmPrompt {
        crate::quit_safety::confirm_prompt(true, true).expect("a busy quit always prompts")
    }

    fn close_prompt() -> ConfirmPrompt {
        crate::quit_safety::confirm_prompt(false, true).expect("a busy close prompts")
    }

    /// The banner stands in for the native dialog, so it must show what the dialog
    /// showed: the title, the body, and both answers — with the affirmative key
    /// naming the prompt's REAL verb, the property the `TaskDialog` was chosen for.
    #[test]
    fn banner_shows_question_body_and_verb_named_keys() {
        let rows = banner_rows(&quit_prompt(), 100, BANNER_ROWS, Theme::default());
        assert_eq!(rows.len(), BANNER_ROWS);
        for row in &rows {
            assert_eq!(row.len(), 100, "every row must be exactly cols wide");
        }
        let row0 = text_of(&rows[0]);
        assert!(
            row0.contains("Are you sure you want to quit aterm?"),
            "{row0}"
        );
        assert!(row0.contains("Enter quits"), "{row0}");
        assert!(row0.contains("Esc cancels"), "{row0}");
        let row1 = text_of(&rows[1]);
        assert!(row1.contains("A process is still running."), "{row1}");

        let rows = banner_rows(&close_prompt(), 100, BANNER_ROWS, Theme::default());
        let row0 = text_of(&rows[0]);
        assert!(row0.contains("Enter closes"), "{row0}");
        assert!(!row0.contains("quits"), "{row0}");
    }

    /// The seam is the band's top edge and runs the whole width in one tone (the
    /// `paste_banner` contract); the body row sits inside the band.
    #[test]
    fn the_seam_runs_unbroken_across_the_question_row() {
        let rows = banner_rows(&quit_prompt(), 100, BANNER_ROWS, Theme::default());
        assert!(rows[0].iter().all(|cell| cell.overline));
        let seams: std::collections::BTreeSet<Option<[u8; 3]>> =
            rows[0].iter().map(|cell| cell.overline_color).collect();
        assert_eq!(seams.len(), 1, "{seams:?}");
        assert!(rows[1].iter().all(|cell| !cell.overline));
    }

    /// Degenerate geometry must not panic: zero columns, one row, a window
    /// narrower than the title.
    #[test]
    fn degenerate_geometry_never_panics() {
        for cols in [0_usize, 1, 3, 8, 20, 200] {
            for panel_rows in [0_usize, 1, BANNER_ROWS] {
                let rows = banner_rows(&quit_prompt(), cols, panel_rows, Theme::default());
                assert_eq!(rows.len(), panel_rows);
                for row in &rows {
                    assert_eq!(row.len(), cols);
                }
            }
        }
    }

    /// The wire kind follows what the answer DOES — a last-tab close of the last
    /// window is a quit on Windows and its prompt says so.
    #[test]
    fn kind_follows_the_prompt_verb() {
        assert_eq!(kind(&quit_prompt()), "quit");
        assert_eq!(kind(&close_prompt()), "close-window");
        let multi = crate::quit_safety::ConfirmPrompt {
            title: "Close all tabs?",
            body: "…",
            proceed: "Close",
        };
        assert_eq!(kind(&multi), "close-window");
    }

    /// The `controls front` line is the agent's whole picture: that a confirm is
    /// pending, over which window, what it asks, who asked, and the two wire
    /// spellings that answer it — the `confirm` verb's, never a keystroke, so the
    /// printed answer reaches the question whichever window is in front.
    #[test]
    fn controls_line_reports_the_question_and_how_to_answer_it() {
        let pending = PendingClose::new(
            crate::WindowId(3),
            quit_prompt(),
            CloseReplay::Window(crate::WindowId(3)),
            ExitActor::Ctl,
        );
        assert!(pending.wire_may_answer());
        let line = pending.controls_line();
        assert!(
            line.starts_with("confirm open=true kind=quit window=3 by=ctl "),
            "{line}"
        );
        assert!(
            line.contains("title=\"Are you sure you want to quit aterm?\""),
            "{line}"
        );
        assert!(line.contains("proceed=Quit"), "{line}");
        assert!(line.contains("answer=\"confirm yes\""), "{line}");
        assert!(line.contains("cancel=\"confirm no\""), "{line}");
        assert!(!line.contains("key "), "a keystroke never answers: {line}");
        assert!(!line.contains('\n'), "one line: {line:?}");
        assert_eq!(CONTROLS_CLOSED, "confirm open=false");
        // The `invoke` reply: the state in words, then the one action.
        assert_eq!(
            pending.wire_refusal(),
            "a quit is waiting for an answer in the window; `confirm yes|no` answers it"
        );
        // And the verb's reply says what it answered, over which question.
        assert_eq!(pending.wire_answered(true), "answered=yes kind=quit");
        assert_eq!(pending.wire_answered(false), "answered=no kind=quit");
    }

    /// The `confirm` grammar: exactly `yes` or `no`. A guess — a bare `confirm`,
    /// `confirm y`, a trailing word — is the usage line, never an answer: `yes`
    /// quits, so nothing but `yes` may mean it.
    #[test]
    fn the_confirm_verb_takes_exactly_yes_or_no() {
        assert_eq!(parse_wire_answer("yes"), Ok(true));
        assert_eq!(parse_wire_answer("  no "), Ok(false));
        for bad in ["", "y", "YES", "enter", "yes now", "no yes", "maybe"] {
            assert_eq!(
                parse_wire_answer(bad),
                Err("usage: confirm yes|no".to_string()),
                "{bad:?}"
            );
        }
    }

    /// A question a PERSON asked is theirs to answer: the wire may not, the report
    /// says who asked and that no wire verb answers it, and a gesture made with no
    /// close scope of its own (the Quit menu) counts as the person's. A control
    /// client's sid asks on the wire like `ctl` does.
    #[test]
    fn a_question_a_person_asked_is_not_answered_from_the_wire() {
        for asked_by in [ExitActor::Human, ExitActor::Unknown] {
            let pending = PendingClose::new(
                crate::WindowId(0),
                close_prompt(),
                CloseReplay::Window(crate::WindowId(0)),
                asked_by,
            );
            assert_eq!(pending.asked_by, ExitActor::Human);
            assert!(!pending.wire_may_answer());
            let line = pending.controls_line();
            assert!(
                line.starts_with("confirm open=true kind=close-window window=0 by=human "),
                "{line}"
            );
            assert!(
                line.ends_with("answer=\"-\" cancel=\"-\""),
                "no wire verb answers it: {line}"
            );
            assert_eq!(
                pending.wire_refusal(),
                "a window close is waiting for the person at the window"
            );
        }
        let by_sid = PendingClose::new(
            crate::WindowId(0),
            close_prompt(),
            CloseReplay::Window(crate::WindowId(0)),
            ExitActor::Sid("s-agent".into()),
        );
        assert!(by_sid.wire_may_answer());
        assert!(
            by_sid.controls_line().contains(" by=s-agent "),
            "{}",
            by_sid.controls_line()
        );
        let by_bridge = PendingClose::new(
            crate::WindowId(0),
            close_prompt(),
            CloseReplay::Window(crate::WindowId(0)),
            ExitActor::Bridge,
        );
        assert!(by_bridge.wire_may_answer());
    }
}
