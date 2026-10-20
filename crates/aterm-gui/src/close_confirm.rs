// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The in-window close/quit confirmation banner (Windows, and Linux, which has no native dialog):
//! the destructive-close confirm composed
//! into the window's own top rows, as [`crate::paste_banner`] composes the multi-line-paste
//! question. A native task-modal dialog would run its own message loop inside the call and stop
//! winit's event loop, so every main-thread ctl verb would time out until a person clicked.
//!
//! The question is parked on `App::close_banner` ([`PendingClose`]) and the gesture refused for
//! now (`confirm_destructive_close` answers `false`): the event loop keeps turning, and
//! `controls front` reports the pending confirm with the wire way to answer it
//! ([`PendingClose::controls_line`]). Enter proceeds by replaying the gesture ([`CloseReplay`])
//! under the `Programmatic` close policy, so the question is never asked twice; Escape (or a
//! click on the band) cancels. The keystroke decision is [`crate::alert_keys::confirm_key`].
//!
//! The wire answers with its own verb, `confirm yes|no` ([`WIRE_ACCEPT`] / [`WIRE_CANCEL`],
//! served by `App::answer_close_confirm_from_wire`), never a keystroke: `key enter` is the most
//! common key an agent sends and is always the PTY's, and a flagless key lands in the front
//! window, which may not be the one asking. There is one question per instance, so the verb
//! needs no aim. The wire answers only a question the wire asked
//! ([`PendingClose::wire_may_answer`]); one a person asked at the window (Ctrl-W, Alt+F4, the
//! caption ✕, the Quit menu) waits for them.
//!
//! No TTL and no auto-dismiss: the banner stands until a key, a click, `confirm`, or the window
//! closing answers it. One shows at a time. A person's gesture over another window while one
//! stands (the taskbar's "Close all windows") waits its turn, one per window, and is asked once
//! the standing question is answered or dropped (`App::ask_next_queued_close`); any other second
//! gesture is refused with the standing question named. The window a question stands over is
//! raised and focused for a person's gesture (`App::raise_close_question`), so a question over a
//! background or minimized window is seen.
//!
//! The macOS `NSAlert` path is not immune — `menu::confirm` answers inside `runModal`, and
//! control wakes queue behind that nested run loop (`menu::confirm_owner`) — but is out of this
//! module's scope.

use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band;
use crate::paste_banner::ellipsized;
use crate::quit_safety::ConfirmPrompt;
use crate::session_store::ExitActor;
use crate::settings::{blank_row, write_str};

/// What Enter replays: the gesture the banner stood in for, re-run under the
/// `Programmatic` close policy (see `App::replay_confirmed_close`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReplay {
    /// Close window `wid` (`App::close_window`, which exits the app iff it was the last
    /// window). Every last-tab close escalates to this once its confirm passes, and Alt+F4 /
    /// the caption ✕ are this, so replaying it replays the gesture.
    Window(crate::WindowId),
    /// Quit the whole program (the Quit action): `App::on_quit_requested`, whose
    /// quit-scope barriers a window close does not run.
    Quit,
    /// Close tab `tab` of window `wid`, a busy tab whose close leaves the window
    /// standing (the strip ✕, a middle click, the tab menu):
    /// `App::replay_deferred_handoff_tab_close`.
    Tab {
        wid: crate::WindowId,
        tab: crate::tab_model::TabId,
    },
    /// Close pane `view` of tab `tab`, a busy pane whose close leaves the window
    /// standing (Ctrl+Shift+W, Close Tab on a split or a non-last tab):
    /// `App::replay_deferred_handoff_view_close`.
    Pane {
        wid: crate::WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
    },
}

/// One parked close/quit confirmation awaiting an answer. Held by `App` in
/// `Option<PendingClose>`: `Some` ⇔ the banner is up over `wid`'s top rows and the
/// gesture's answer is still owed.
pub(crate) struct PendingClose {
    /// The window the banner splices into: the one the gesture would close (or, for a quit, the
    /// one it was anchored to). Only its keys answer it, and the entry dies with it.
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

/// The banner is two rows: the question with the answer keys, then the body (three
/// when the keys do not fit beside the question and take a row of their own).
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

/// The wire `kind=` of a prompt: what the affirmative answer does. A last-tab close of the last
/// window quits the app, and on Windows its prompt says so (`proceed == "Quit"`), so the kind
/// follows the prompt rather than the gesture that raised it.
pub(crate) fn kind(prompt: &ConfirmPrompt) -> &'static str {
    match prompt.proceed {
        "Quit" => "quit",
        crate::quit_safety::CLOSE_TAB => "close-tab",
        crate::quit_safety::CLOSE_PANE => "close-pane",
        _ => "close-window",
    }
}

/// The pending gesture in words, for the wire's refusals: what [`kind`] says as
/// a token.
fn gesture(prompt: &ConfirmPrompt) -> &'static str {
    match prompt.proceed {
        "Quit" => "a quit",
        crate::quit_safety::CLOSE_TAB => "a tab close",
        crate::quit_safety::CLOSE_PANE => "a pane close",
        _ => "a window close",
    }
}

/// The question's words: the prompt's title. The painted warn mark is added by
/// [`banner_rows`], so the accessible alert does not read it aloud.
pub(crate) fn question(prompt: &ConfirmPrompt) -> String {
    prompt.title.to_string()
}

/// The two keys that answer the question, right-aligned on the title row and spoken as the a11y
/// question's description. The affirmative key names the prompt's verb: "Enter quits" over a
/// quit, "Enter closes" over a close.
pub(crate) fn answer_keys(prompt: &ConfirmPrompt) -> String {
    let verb = match prompt.proceed {
        "Quit" => "quits",
        _ => "closes",
    };
    format!("Enter {verb} \u{00b7} Esc cancels")
}

/// [`answer_keys`] for a window too narrow to carry the verbs beside the question.
const ANSWER_KEYS_SHORT: &str = "Enter \u{00b7} Esc";

/// Row 0's title and the answer keys that fit beside it in `cols`: the verbs
/// first, then the short keys; `None` when neither fits, and the keys take a row of
/// their own. [`banner_rows`] paints by it and [`PendingClose::wanted_rows`] sizes
/// by it, so the two agree.
fn title_row(prompt: &ConfirmPrompt, cols: usize) -> (String, Option<String>) {
    let title = format!("!  {}", question(prompt));
    let fits = |keys: &str| {
        cols.saturating_sub(keys.chars().count() + MARGIN) > MARGIN + title.chars().count() + 2
    };
    let keys = [answer_keys(prompt), ANSWER_KEYS_SHORT.to_string()]
        .into_iter()
        .find(|keys| fits(keys));
    (title, keys)
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

    /// Rows the banner wants in a window `cols` wide (the splice clamps to the
    /// frame): one more when the answer keys need a row of their own.
    pub(crate) fn wanted_rows(&self, cols: usize) -> usize {
        BANNER_ROWS + usize::from(title_row(&self.prompt, cols).1.is_none())
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

    /// The `controls front` report of this pending confirm: what is asked, over which window, who
    /// asked, and how to answer it from the wire (`answer=-` when the person who asked answers
    /// it). One line of `k=v` tokens; the title is quoted because it carries spaces.
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

    /// The `invoke` reply of the gesture that parked this question (`App::invoke_reply`): an `OK`
    /// (asking is what the action does), worded so no driver reads it as a close that happened.
    pub(crate) fn wire_pending(&self) -> String {
        format!(
            "confirm pending kind={} window={}; `confirm yes|no` answers it",
            kind(&self.prompt),
            self.wid.0
        )
    }

    /// The wire refusal of a gesture this question stands in the way of, and the `confirm` verb's
    /// refusal of a question it may not answer. The parking gesture also leaves it in
    /// `pending_action_refusal` for every other reply-minting consumer; `invoke` answers that
    /// gesture with [`Self::wire_pending`] instead.
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

/// The `confirm` verb's argument: `yes` proceeds, `no` keeps everything. The `Err` is the whole
/// usage line, so the refusal teaches it.
pub(crate) fn parse_wire_answer(rest: &str) -> Result<bool, String> {
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some("yes"), None) => Ok(true),
        (Some("no"), None) => Ok(false),
        _ => Err("usage: confirm yes|no".to_string()),
    }
}

/// Pure grid-cell row builder: exactly `panel_rows` rows, each exactly `cols` wide, so the splice
/// overwrites frame rows in place (the `paste_banner` contract). Row 0 carries the question
/// ([`question`]) with the answer keys ([`answer_keys`]) right-aligned, then the prompt's body,
/// ellipsized; in a window too narrow for the keys beside the question they take row 1 and the
/// body row 2.
pub(crate) fn banner_rows(
    prompt: &ConfirmPrompt,
    cols: usize,
    panel_rows: usize,
    palette: impl Into<chrome_band::ChromePalette>,
) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(palette);
    let mut rows: Vec<Vec<RenderCell>> = (0..panel_rows)
        .map(|r| blank_row(cols, c.label, c.bar_bg, r == 0))
        .collect();
    if panel_rows == 0 {
        return rows;
    }
    let (title, row0_keys) = title_row(prompt, cols);
    // `value` (not the dim `label`): the answer keys are not an aside — they are
    // the only way to answer (every other key is swallowed), so they read at full
    // tone and never drop.
    let mut body_row = 1;
    if let Some(keys) = &row0_keys {
        write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
        let keys_col = cols.saturating_sub(keys.chars().count() + MARGIN);
        write_str(&mut rows[0], cols, keys_col, keys, c.value, c.bar_bg, true);
    } else if panel_rows > 1 {
        let title = ellipsized(&title, cols.saturating_sub(2 * MARGIN));
        write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
        let full = answer_keys(prompt);
        let keys = if full.chars().count() + 2 * MARGIN <= cols {
            full.as_str()
        } else {
            ANSWER_KEYS_SHORT
        };
        write_str(&mut rows[1], cols, MARGIN, keys, c.value, c.bar_bg, true);
        body_row = 2;
    } else {
        // One row and no room for both: the keys keep their place at the right
        // and the question is cut to what is left.
        let keys_col = cols.saturating_sub(ANSWER_KEYS_SHORT.chars().count() + MARGIN);
        let title = ellipsized(&title, keys_col.saturating_sub(MARGIN + 1));
        write_str(&mut rows[0], cols, MARGIN, &title, c.warn, c.bar_bg, true);
        write_str(
            &mut rows[0],
            cols,
            keys_col,
            ANSWER_KEYS_SHORT,
            c.value,
            c.bar_bg,
            true,
        );
    }
    if panel_rows > body_row {
        let text_w = cols.saturating_sub(BODY_INDENT + MARGIN);
        write_str(
            &mut rows[body_row],
            cols,
            BODY_INDENT,
            &ellipsized(prompt.body, text_w),
            c.value,
            c.bar_bg,
            false,
        );
    }
    // `write_str` rebuilds every cell it touches without the overline, so the seam (the whole
    // top edge, one tone) is sealed after the words.
    chrome_band::seal_band_top(&mut rows[0], c.seam);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(row: &[RenderCell]) -> String {
        row.iter().map(|cell| cell.ch).collect()
    }

    fn quit_prompt() -> ConfirmPrompt {
        crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit always prompts")
    }

    fn close_prompt() -> ConfirmPrompt {
        crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts")
    }

    /// The banner shows what the native dialog showed: title, body and both answers, the
    /// affirmative key naming the prompt's verb.
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
        assert!(row1.contains("A running process ends"), "{row1}");

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

    /// A narrow window still shows the answer keys: the band swallows every other key, so
    /// without them there is no visible way out.
    #[test]
    fn a_narrow_band_keeps_the_answer_keys() {
        let multi = crate::quit_safety::ConfirmPrompt {
            title: "Close all tabs?",
            body: "A running process ends.",
            proceed: "Close",
        };
        let pending = PendingClose::new(
            crate::WindowId(0),
            multi,
            CloseReplay::Window(crate::WindowId(0)),
            ExitActor::Human,
        );
        assert_eq!(pending.wanted_rows(100), BANNER_ROWS);
        // Short keys beside the question.
        let row0 = text_of(&banner_rows(&multi, 40, BANNER_ROWS, Theme::default())[0]);
        assert!(row0.contains("Enter \u{00b7} Esc"), "{row0:?}");
        // Narrower still: the keys take row 1, the band grows, the body follows.
        assert_eq!(pending.wanted_rows(24), BANNER_ROWS + 1);
        let rows = banner_rows(&multi, 24, BANNER_ROWS + 1, Theme::default());
        assert!(
            text_of(&rows[1]).contains("Enter"),
            "{:?}",
            text_of(&rows[1])
        );
        assert!(
            text_of(&rows[2]).contains("A running"),
            "{:?}",
            text_of(&rows[2])
        );
        // One row: the keys keep their place.
        let row0 = text_of(&banner_rows(&multi, 24, 1, Theme::default())[0]);
        assert!(row0.contains("Enter") && row0.contains("Esc"), "{row0:?}");
    }

    /// The wire kind follows what the answer does: a last-tab close of the last window is a
    /// quit on Windows, and its prompt says so.
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
        // The `invoke` reply of the gesture that asked: `OK`, the state in
        // words, then the one action — never `invoked`, which reads as done.
        assert_eq!(
            pending.wire_pending(),
            "confirm pending kind=quit window=3; `confirm yes|no` answers it"
        );
        // And the refusal of a second gesture while it stands.
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

    /// A question a person asked is theirs to answer: the wire may not, the report says who asked
    /// and that no wire verb answers it, and a gesture with no close scope of its own (the Quit
    /// menu) counts as the person's. A control client's sid asks on the wire like `ctl` does.
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
