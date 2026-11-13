// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The close/quit confirmation: one standing question, its replay, and its wire answers. A Mac
//! window shows it as a native sheet ([`crate::close_sheet`]); everywhere else, and wherever no
//! sheet could be built, it is the in-window band [`banner_rows`] paints.

use aterm_core::terminal::RenderCell;
#[cfg(test)]
use aterm_render::Theme;

use crate::chrome_band;
use crate::paste_banner::ellipsized;
use crate::quit_safety::ConfirmPrompt;
use crate::session_store::ExitActor;
use crate::settings::{blank_row, write_str};

/// What Enter replays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReplay {
    /// Close window `wid` (`App::close_window`, which exits the app iff it was the last window).
    Window(crate::WindowId),
    /// Quit the whole program (the Quit action).
    Quit,
    /// Complete this AppKit termination request, preserving its Quit & Reopen identity.
    #[cfg(unix)]
    NativeTerminate { generation: u64 },
    /// Close tab `tab` of window `wid`.
    Tab {
        wid: crate::WindowId,
        tab: crate::tab_model::TabId,
    },
    /// Close pane `view` of tab `tab`.
    Pane {
        wid: crate::WindowId,
        tab: crate::tab_model::TabId,
        view: crate::tab_model::ViewId,
    },
}

impl CloseReplay {
    /// Cancel only the native request this abandoned question belongs to.
    pub(crate) fn cancel(self) {
        #[cfg(unix)]
        if let Self::NativeTerminate { generation } = self {
            let _ = crate::menu::cancel_native_termination(generation);
        }
    }
}

/// One parked close/quit confirmation awaiting an answer.
pub(crate) struct PendingClose {
    /// The window the banner splices into: the one the gesture would close.
    pub(crate) wid: crate::WindowId,
    /// The confirmation's copy.
    pub(crate) prompt: ConfirmPrompt,
    /// What Enter replays.
    pub(crate) replay: CloseReplay,
    /// Who asked, as the exit ledger's `by=` names it.
    pub(crate) asked_by: ExitActor,
    /// This question's serial, never `0` (`App::question_key`).
    serial: u64,
    /// The native sheet presenting this question; dropping the question ends it.
    #[cfg(target_os = "macos")]
    pub(crate) sheet: Option<crate::close_sheet::CloseSheet>,
}

/// The banner is two rows: the question with the answer keys, then the body.
pub(crate) const BANNER_ROWS: usize = 2;
/// Left margin, in cells (mirrors `paste_banner`).
const MARGIN: usize = 2;
/// The body's hanging indent: under the title text, past the `!  ` mark.
const BODY_INDENT: usize = MARGIN + 3;

/// The wire spellings that answer the question, printed by [`PendingClose::controls_line`].
pub(crate) const WIRE_ACCEPT: &str = "confirm yes";
pub(crate) const WIRE_CANCEL: &str = "confirm no";
/// The `controls front` line when no confirm is pending.
pub(crate) const CONTROLS_CLOSED: &str = "confirm open=false";
/// The `confirm` verb's refusal when no question stands.
pub(crate) const NOTHING_PENDING: &str = "nothing is waiting for an answer";

/// The wire `kind=` of a prompt: what the affirmative answer does.
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

/// The question's words: the prompt's title.
pub(crate) fn question(prompt: &ConfirmPrompt) -> String {
    prompt.title.to_string()
}

/// The two keys that answer the question.
pub(crate) fn answer_keys(prompt: &ConfirmPrompt) -> String {
    let verb = match prompt.proceed {
        "Quit" => "quits",
        _ => "closes",
    };
    format!("Enter {verb} \u{00b7} Esc cancels")
}

/// The accessible alert's description: what the answer ends, then the keys.
#[cfg(a11y_tree)]
pub(crate) fn announcement_detail(prompt: &ConfirmPrompt) -> String {
    format!("{} {}", prompt.body, answer_keys(prompt))
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
    /// `asked_by` is the close attribution open when the gesture ran.
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
            serial: crate::paste_banner::next_question_serial(),
            #[cfg(target_os = "macos")]
            sheet: None,
        }
    }

    /// Whether the window draws this question as its band: no native sheet presents it.
    pub(crate) const fn in_window(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.sheet.is_none()
        }
        #[cfg(not(target_os = "macos"))]
        {
            true
        }
    }

    /// This question's serial (never `0`).
    pub(crate) const fn serial(&self) -> u64 {
        self.serial
    }

    /// Rows the banner wants in a window `cols` wide (the splice clamps to the frame).
    pub(crate) fn wanted_rows(&self, cols: usize) -> usize {
        BANNER_ROWS + usize::from(title_row(&self.prompt, cols).1.is_none())
    }

    /// Whether the wire's `confirm yes|no` answers this question.
    pub(crate) const fn wire_may_answer(&self) -> bool {
        match &self.asked_by {
            ExitActor::Sid(_) | ExitActor::Ctl | ExitActor::Harness | ExitActor::Fleet { .. } => {
                true
            }
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

    /// The `controls front` report of this pending confirm.
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

    /// The `invoke` reply of the gesture that parked this question (`App::invoke_reply`).
    pub(crate) fn wire_pending(&self) -> String {
        format!(
            "confirm pending kind={} window={}; `confirm yes|no` answers it",
            kind(&self.prompt),
            self.wid.0
        )
    }

    /// The wire refusal of a gesture this question stands in the way of.
    pub(crate) fn wire_refusal(&self) -> String {
        let gesture = gesture(&self.prompt);
        if self.wire_may_answer() {
            format!("{gesture} is waiting for an answer in the window; `confirm yes|no` answers it")
        } else {
            format!("{gesture} is waiting for the person at the window")
        }
    }

    /// The `confirm` verb's reply once it has answered.
    pub(crate) fn wire_answered(&self, proceed: bool) -> String {
        format!(
            "answered={} kind={}",
            if proceed { "yes" } else { "no" },
            kind(&self.prompt)
        )
    }
}

/// The `confirm` verb's argument: `yes` proceeds, `no` keeps everything.
pub(crate) fn parse_wire_answer(rest: &str) -> Result<bool, String> {
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some("yes"), None) => Ok(true),
        (Some("no"), None) => Ok(false),
        _ => Err("usage: confirm yes|no".to_string()),
    }
}

/// Pure grid-cell row builder.
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
    chrome_band::seal_band_top(&mut rows[0], c.seam);
    rows
}

#[cfg(test)]
#[path = "close_confirm_tests.rs"]
mod tests;
