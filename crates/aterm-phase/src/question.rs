// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE QUESTION DIALOG (AskUserQuestion) as Claude Code 2.1.282 draws it,
//! read whole: its tab bar, its question, each option (its label, its
//! description, its focus, its checkbox, and `(Recommended)` read from the
//! LABEL only), the free-text row and the chat row read by their POSITION,
//! the multi-select button, the Submit (review) tab, and the side-by-side
//! preview form. [`crate::prompt::PromptV2::question_dialog`] carries it.
//!
//! **THE GEOMETRY** (measured 2026-09-25 on 2.1.282 under a headless aterm
//! 0.93.0, 53 captures; the fixtures `claude-2.1.282-question-*.txt`). Top
//! to bottom: a full-width rule at column 0 (its width is the pane's, `W`);
//! the tab bar (` ☐ Colors` for one single-select question; `←  ☐ Button  ☒
//! Grey cursor  ✔ Submit  →` for several, and for ONE multi-select question,
//! whose answers are reviewed too — the active chip is marked by colour
//! only, so the text never says which tab is up); a blank row; the question
//! at column 0 (every row led by the `│ ` gutter when its text has a newline
//! or is wider than 80 cells, else bare); a blank row; the options — `❯ 1.
//! label` / `  2. label` (the number at column 2, the label at column 5, its
//! description rows under it at column 5, dim, which the text cannot tell
//! from a wrapped label) or, multi-select, `  1. [ ] label` (the label and
//! its description at column 9); the free-text row `N+1. Type something.`
//! (`[ ] Type something` in multi-select, whose button row `Next` — `Submit`
//! on the last question — follows at column 5); a full-width divider; the
//! chat row `  N+2. Chat about this`; a blank row; the key-hint footer.
//! The preview form draws its options in a 30-cell column with the preview
//! pane (`┌─┐│└┘`) to their right on the same rows, a blank row, `Notes:
//! press n to add notes` at the pane's column, no descriptions, no
//! free-text row, and an UNNUMBERED chat row. The review tab (the `✔ Submit`
//! chip) draws `Review your answers`, an optional `⚠ You have not answered
//! all questions`, the answers ` ● <question>` / `   → <answer>`, `Ready to
//! submit your answers?` and `❯ 1. Submit answers` / `  2. Cancel`, and no
//! footer. Under a dialog its host may draw the AFK countdown row
//! (`auto-continue in 16s · any key to stay`, right-aligned), one
//! right-aligned plugin notice, and the REPL's `Background task update
//! waiting while this panel is open` (the critique of 2026-09-25, R6).
//!
//! **NEVER READ WHOLE UNLESS SOUND.** A form is read only when every row of
//! it is one of the rows above, the options are numbered `1..=N` from the
//! first without a gap, the free-text row is option `N+1` (by position,
//! never by its words: a person's text replaces them), the chat row is
//! `N+2`, and exactly one row carries the `❯` focus (2.1.282 always draws
//! one: none is a rendering this reader did not understand, R11). Anything
//! else — a scrolled option list (`↑`/`↓` in the pointer column, R9), a
//! numbering gap, an opened notes field, a head cut by the pane, a dialog
//! withheld behind a draft in the composer (R5) — is
//! [`QuestionForm::Unknown`]: still a question, never idle, and never
//! answered.

use std::ops::Range;

use crate::anchors::anchor_text;
use crate::phase::{is_rule, is_transcript_row, leading_spaces};

/// The question dialog, read ([`crate::prompt::PromptV2::question_dialog`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionDialog {
    /// The tab bar's chips in order (the `✔ Submit` chip left out): at least
    /// one on a read tab; empty when the tab bar was not read.
    pub tabs: Vec<QuestionTab>,
    /// A `✔ Submit` chip is drawn: a review step follows the last answer
    /// (several questions, or one multi-select question).
    pub submit_tab: bool,
    pub form: QuestionForm,
    /// The AFK countdown row under the dialog, as drawn (trimmed), when the
    /// host draws one: `auto-continue in 16s · any key to stay`. It ticks.
    pub auto_continue: Option<String>,
    /// The dialog's width in cells: its divider's (its top rule's, on the
    /// review tab); 0 when not read.
    pub width: usize,
}

/// One chip of the tab bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionTab {
    /// The question's header as drawn (`Grey cursor`).
    pub header: String,
    /// `☒`: the question has an answer.
    pub answered: bool,
}

/// The question a tab asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionText {
    /// Its rows on the screen.
    pub rows: Range<usize>,
    /// Its text: the `│ ` gutter stripped, the rows joined with one space.
    pub text: String,
    /// Its rows are led by the `│ ` gutter.
    pub gutter: bool,
}

/// Whether an option's LABEL is marked `(Recommended)` — the tool's own
/// convention ("add (Recommended) at the end of the label"), read from the
/// label only, never from a description row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recommended {
    /// The option's own row ends ` (Recommended)`.
    Label,
    /// A row under the option that may be the label's wrap (the row above
    /// it ran to the dialog's edge) ends ` (Recommended)`: row `label_end`,
    /// the label's last. The text cannot prove it is no description.
    Wrapped {
        label_end: usize,
    },
    No,
}

/// One of the model's options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub n: u8,
    /// The option's numbered row.
    pub row: usize,
    /// Its label (the checkbox stripped): its row's text, and in
    /// [`Recommended::Wrapped`] the rows to `label_end`, joined with one
    /// space. A preview label is every row of it left of the pane.
    pub label: String,
    pub label_rows: Range<usize>,
    /// The rows under the label, joined with one space.
    pub description: String,
    pub recommended: Recommended,
    /// The `❯` focus is on it.
    pub focused: bool,
    /// Multi-select: its checkbox, `[✔]` (`Some(true)`) or `[ ]`.
    pub checked: Option<bool>,
    /// Preview form: the label ends with the selection tick ` ✔` (a tab a
    /// person answered and came back to; stripped from [`Self::label`]).
    pub selected: bool,
}

/// The free-text row, `N+1.` — read by its POSITION (the last numbered row
/// above the divider, or above the multi-select button), never by its
/// words: a person's text replaces the placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeText {
    pub n: u8,
    pub row: usize,
    /// Its text as drawn (the checkbox stripped): the placeholder, or what a
    /// person typed.
    pub text: String,
    /// It shows the placeholder (`Type something.`, `Type something` in
    /// multi-select), nothing typed.
    pub pristine: bool,
    pub focused: bool,
    /// Multi-select: its checkbox (it checks itself when typed into).
    pub checked: Option<bool>,
}

/// The `Chat about this` row under the divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatRow {
    /// `N+2`; `None` in the preview form, which draws no number.
    pub n: Option<u8>,
    pub row: usize,
    pub focused: bool,
}

/// The multi-select button row under the free-text row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonRow {
    pub row: usize,
    /// `Next`, or `Submit` on the last question — read by position.
    pub label: String,
    pub focused: bool,
}

/// One of the review tab's two options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewRow {
    pub row: usize,
    pub focused: bool,
}

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionForm {
    /// One answer: a digit or Enter answers and moves on.
    Single {
        question: QuestionText,
        options: Vec<QuestionOption>,
        free_text: FreeText,
        chat: ChatRow,
    },
    /// Several answers: a digit, Space or Enter toggles; the button moves on.
    Multi {
        question: QuestionText,
        options: Vec<QuestionOption>,
        free_text: FreeText,
        button: ButtonRow,
        chat: ChatRow,
    },
    /// Options with a preview pane at column `pane_col`: a digit only moves
    /// the focus; Enter answers.
    Preview {
        question: QuestionText,
        options: Vec<QuestionOption>,
        chat: ChatRow,
        pane_col: usize,
    },
    /// The Submit tab: `1. Submit answers` / `2. Cancel` (its cancel rejects
    /// the tool call, measured).
    Review {
        /// `⚠ You have not answered all questions` is drawn.
        unanswered: bool,
        submit: ReviewRow,
        cancel: ReviewRow,
    },
    /// Read as a question, not whole (module header): never answered.
    Unknown { why: &'static str },
}

/// Where the dialog's one `❯` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionFocus {
    Option(u8),
    FreeText,
    Chat,
    Button,
    ReviewSubmit,
    ReviewCancel,
    /// No row, or more than one, carries it (and every
    /// [`QuestionForm::Unknown`]).
    None,
}

impl QuestionDialog {
    /// A question named by its footer or its options alone, whose rows were
    /// not read ([`QuestionForm::Unknown`]).
    #[must_use]
    pub(crate) fn unknown(why: &'static str) -> Self {
        Self {
            tabs: Vec::new(),
            submit_tab: false,
            form: QuestionForm::Unknown { why },
            auto_continue: None,
            width: 0,
        }
    }

    /// The question the tab asks; `None` on the review tab and when not read.
    #[must_use]
    pub fn question(&self) -> Option<&QuestionText> {
        match &self.form {
            QuestionForm::Single { question, .. }
            | QuestionForm::Multi { question, .. }
            | QuestionForm::Preview { question, .. } => Some(question),
            QuestionForm::Review { .. } | QuestionForm::Unknown { .. } => None,
        }
    }

    /// The row a key is guarded on: the question's first row as drawn (the
    /// `│` gutter included); on the review tab its `1. Submit answers` row.
    #[must_use]
    pub fn guard_row(&self) -> Option<usize> {
        match &self.form {
            QuestionForm::Review { submit, .. } => Some(submit.row),
            _ => self.question().map(|q| q.rows.start),
        }
    }

    /// Where the `❯` is: exactly one of the dialog's rows carries it, else
    /// [`QuestionFocus::None`].
    #[must_use]
    pub fn focus(&self) -> QuestionFocus {
        use QuestionFocus as At;
        let opts = |options: &[QuestionOption]| -> Vec<At> {
            options
                .iter()
                .filter(|o| o.focused)
                .map(|o| At::Option(o.n))
                .collect()
        };
        let mut at: Vec<At> = Vec::new();
        match &self.form {
            QuestionForm::Single {
                options,
                free_text,
                chat,
                ..
            } => {
                at.extend(opts(options));
                at.extend(free_text.focused.then_some(At::FreeText));
                at.extend(chat.focused.then_some(At::Chat));
            }
            QuestionForm::Multi {
                options,
                free_text,
                button,
                chat,
                ..
            } => {
                at.extend(opts(options));
                at.extend(free_text.focused.then_some(At::FreeText));
                at.extend(button.focused.then_some(At::Button));
                at.extend(chat.focused.then_some(At::Chat));
            }
            QuestionForm::Preview { options, chat, .. } => {
                at.extend(opts(options));
                at.extend(chat.focused.then_some(At::Chat));
            }
            QuestionForm::Review { submit, cancel, .. } => {
                at.extend(submit.focused.then_some(At::ReviewSubmit));
                at.extend(cancel.focused.then_some(At::ReviewCancel));
            }
            QuestionForm::Unknown { .. } => {}
        }
        match at.as_slice() {
            [one] => *one,
            _ => At::None,
        }
    }

    /// The same tab as `other` still up: the same form and the same question
    /// text (any two review tabs are the same).
    #[must_use]
    pub fn same_question(&self, other: &Self) -> bool {
        use QuestionForm as F;
        match (&self.form, &other.form) {
            (F::Review { .. }, F::Review { .. }) => true,
            (F::Single { question: a, .. }, F::Single { question: b, .. })
            | (F::Multi { question: a, .. }, F::Multi { question: b, .. })
            | (F::Preview { question: a, .. }, F::Preview { question: b, .. }) => a.text == b.text,
            _ => false,
        }
    }
}

// ---- the reading's geometry -----------------------------------------------

/// Which form the rows were read as ([`QShape`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QForm {
    Single,
    Multi,
    Preview,
    Review,
    Unknown(&'static str),
}

/// A question dialog's rows on the screen, as the recognizers
/// ([`footed`], [`review`], [`deferred`]) found them: row indices only, so
/// the reader's box (`prompt::Found`) stays `Copy`; [`dialog`] reads the
/// words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QShape {
    pub(crate) form: QForm,
    pub(crate) tab_bar: Option<usize>,
    /// The question's rows, `[start, end)`.
    pub(crate) question: Option<(usize, usize)>,
    /// The options zone, `[start, end)`: the first numbered row to the
    /// divider (the button row and the preview's pane rows inside it); on
    /// the review tab, `1. Submit answers` to one past `2. Cancel`.
    pub(crate) options: (usize, usize),
    pub(crate) button: Option<usize>,
    pub(crate) chat: Option<usize>,
    pub(crate) footer: Option<usize>,
    pub(crate) afk: Option<usize>,
    /// The review's `⚠ You have not answered all questions` row.
    pub(crate) warning: Option<usize>,
    /// The preview pane's column, in cells; 0 elsewhere.
    pub(crate) pane_col: usize,
    pub(crate) width: usize,
    /// The box's first row: the tab bar, or the first visible row.
    pub(crate) first: usize,
    /// The box's last row: the footer, or the review's `2. Cancel`.
    pub(crate) last: usize,
    pub(crate) head_off_screen: bool,
}

impl QShape {
    /// Only these rows, of a question that is not read whole.
    fn unknown(why: &'static str, first: usize, last: usize, head_off_screen: bool) -> Self {
        Self {
            form: QForm::Unknown(why),
            tab_bar: None,
            question: None,
            options: (first, first),
            button: None,
            chat: None,
            footer: None,
            afk: None,
            warning: None,
            pane_col: 0,
            width: 0,
            first,
            last,
            head_off_screen,
        }
    }

    /// A live question footer whose rows are none of the shapes read (a
    /// focused or unnumbered chat row the old walk stopped at, a shape a
    /// later release draws): rows `first..=footer`, the head off the screen
    /// (`prompt::question_fallback`, R7).
    pub(crate) fn fallback(first: usize, footer: usize) -> Self {
        Self {
            footer: Some(footer),
            ..Self::unknown(UNREAD, first, footer, true)
        }
    }
}

/// Why a dialog is not read whole (each an [`QuestionForm::Unknown`]).
pub(crate) const HEAD_CUT: &str = "the dialog's head is not on the rows read";
pub(crate) const NAMED_ONLY: &str =
    "named a question by its footer or its options alone, its rows not read";
pub(crate) const WITHHELD: &str = "withheld behind a draft in the composer";
const UNREAD: &str = "its rows are not a shape aterm-phase reads";
const SCROLLED: &str = "the option list is scrolled";
const NUMBERING: &str = "its options are not numbered 1 to N from the first";
const NO_FREE_TEXT: &str = "no option above its free-text row";
const CHAT_NUMBER: &str = "its chat row is not numbered N+2";
const FOCUS: &str = "not exactly one row carries the focus";
const NO_BUTTON: &str = "a multi-select question with no button row";
const NOTES_OPEN: &str = "the preview's notes are open";
const NO_PANE: &str = "the preview pane is not beside its first option";
const REVIEW_SHAPE: &str = "the review tab's rows are not the shape aterm-phase reads";

/// The pointer column of a dialog row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pointer {
    /// `❯`: the focus.
    Focus,
    Blank,
    /// `↑` / `↓`: the list is scrolled (He() draws them in place of the
    /// pointer, 2.1.282).
    Scrolled,
}

/// A row numbered in the dialog's pointer column — `❯ 1. Yes`, `  2. No`,
/// `↓ 4. Delta` (the pointer at column 0, the number at column 2) — as its
/// pointer, its number and the text after `N. `.
fn numbered(row: &str) -> Option<(Pointer, u8, &str)> {
    let mut cs = row.chars();
    let pointer = match cs.next()? {
        '❯' => Pointer::Focus,
        ' ' => Pointer::Blank,
        '↑' | '↓' => Pointer::Scrolled,
        _ => return None,
    };
    let rest = cs.as_str().strip_prefix(' ')?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let n: u8 = rest.get(..digits)?.parse().ok()?;
    let text = rest.get(digits..)?.strip_prefix(". ")?;
    Some((pointer, n, text))
}

/// A multi-select label's checkbox: `[ ] x` → `(false, "x")`, `[✔] x` →
/// `(true, "x")`.
fn checkbox(text: &str) -> Option<(bool, &str)> {
    if let Some(rest) = text.strip_prefix("[ ] ") {
        Some((false, rest))
    } else {
        text.strip_prefix("[✔] ").map(|rest| (true, rest))
    }
}

/// The chat row: `  N. Chat about this` / `❯ N. Chat about this`, or the
/// preview's unnumbered `  Chat about this` / `❯ Chat about this` — as its
/// number and focus.
fn chat_row(row: &str) -> Option<(Option<u8>, bool)> {
    let chat = anchor_text("question.chat");
    if let Some((pointer, n, text)) = numbered(row) {
        return (pointer != Pointer::Scrolled && text.trim_end() == chat)
            .then_some((Some(n), pointer == Pointer::Focus));
    }
    let (focused, rest) = match row.strip_prefix('❯') {
        Some(rest) => (true, rest),
        None => (false, row),
    };
    (rest.starts_with(' ') && leading_spaces(rest) <= 2 && rest.trim() == chat)
        .then_some((None, focused))
}

/// The multi-select button row: one word at column 5, or `❯` and four
/// spaces before it (the focus).
fn button_row(row: &str) -> Option<(String, bool)> {
    let (focused, rest) = match row.strip_prefix('❯') {
        Some(rest) => (true, rest.strip_prefix("    ")?),
        None => (false, row.strip_prefix("     ")?),
    };
    let word = rest.trim_end();
    (!word.is_empty() && !word.starts_with(' ') && !word.contains(char::is_whitespace))
        .then(|| (word.to_string(), focused))
}

/// The tab bar: ` ☐ Colors` (one chip, column 1), or `←  ☐ A  ☒ B  ✔ Submit
/// →` (column 0) — as its chips and whether a `✔ Submit` chip is drawn.
pub(crate) fn tab_bar(row: &str) -> Option<(Vec<QuestionTab>, bool)> {
    let t = row.trim_end();
    let (inner, submit_tab) = if let Some(rest) = t.strip_prefix('←') {
        let inner = rest.strip_suffix('→')?.trim_end();
        (
            inner.strip_suffix(anchor_text("question.tab_submit"))?,
            true,
        )
    } else {
        let rest = t.strip_prefix(' ')?;
        rest.starts_with(['☐', '☒']).then_some((rest, false))?
    };
    let mut tabs = Vec::new();
    let mut lead = true;
    for (i, c) in inner.char_indices() {
        if !matches!(c, '☐' | '☒') {
            continue;
        }
        let after = &inner[i + c.len_utf8()..];
        let end = after.find(['☐', '☒']).unwrap_or(after.len());
        if lead && !inner[..i].trim().is_empty() {
            return None;
        }
        lead = false;
        let header = after[..end].trim();
        if header.is_empty() || !after.starts_with(' ') {
            return None;
        }
        tabs.push(QuestionTab {
            header: header.to_string(),
            answered: c == '☒',
        });
    }
    if tabs.is_empty() || (!submit_tab && tabs.len() != 1) {
        return None;
    }
    Some((tabs, submit_tab))
}

fn blank(row: &str) -> bool {
    row.trim().is_empty()
}

/// A row's width in cells when it can be measured: printable ASCII, as
/// `aterm-agent`'s `cells_are_chars` measures a box's command row, and the
/// glyphs the dialog itself draws in its own one-cell columns — the pointer
/// `❯`, the checkbox's `✔` and the gutter `│` (each measured one cell in
/// the 2.1.282 captures' styles), and the scroll arrows drawn in the
/// pointer's column. `None` for any other character: the server's text
/// leaves out a wide character's second cell, so the row cannot be
/// measured.
fn measured(row: &str) -> Option<usize> {
    let t = row.trim_end();
    t.chars()
        .all(|c| (' '..='~').contains(&c) || matches!(c, '❯' | '✔' | '↑' | '↓' | '│'))
        .then(|| t.chars().count())
}

/// Whether row `below` may be the wrap of row `above` in a dialog `width`
/// cells wide: `above` plus a space plus `below`'s first word would have
/// run past the edge (two cells of slack for Ink's box: the measured 80-cell
/// label wrapped at cell 78). A row either of which cannot be measured may
/// be one.
fn possibly_wrap(above: &str, below: &str, width: usize) -> bool {
    let (Some(len), Some(_)) = (measured(above), measured(below)) else {
        return true;
    };
    let first = below
        .split_whitespace()
        .next()
        .map_or(0, |w| w.chars().count());
    len + 1 + first > width.saturating_sub(2)
}

/// A character's width in cells: 2 for the East Asian wide and fullwidth
/// blocks and the emoji blocks, else 1 (the grid's own rule; a close
/// reading without a width table, used only to find the preview pane's
/// column on a row).
fn cell_width(c: char) -> usize {
    let wide = matches!(u32::from(c),
        0x1100..=0x115F
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x1F300..=0x1F64F
            | 0x1F900..=0x1F9FF
            | 0x20000..=0x3FFFD);
    if wide { 2 } else { 1 }
}

/// `row` split at cell `col`: the characters that start left of it, and
/// the rest.
fn split_at_cell(row: &str, col: usize) -> (&str, &str) {
    let mut cell = 0;
    for (i, c) in row.char_indices() {
        if cell >= col {
            return (&row[..i], &row[i..]);
        }
        cell += cell_width(c);
    }
    (row, "")
}

/// The cell `c` first starts at on `row`.
fn cell_of(row: &str, c: char) -> Option<usize> {
    let mut cell = 0;
    for x in row.chars() {
        if x == c {
            return Some(cell);
        }
        cell += cell_width(x);
    }
    None
}

// ---- under the dialog (R6) ------------------------------------------------

/// The AFK countdown row (Lme(), right-aligned): `auto-continue in 16s ·
/// any key to stay`.
fn afk_row(row: &str) -> bool {
    let t = row.trim();
    t.starts_with(anchor_text("question.auto_continue"))
        && t.ends_with(anchor_text("question.stay"))
}

/// The REPL's row while a panel holds the queue (Y_(), 2.1.282): `Background
/// task update waiting while this panel is open`, or `<N> background task
/// updates …`.
fn panel_waiting_row(row: &str) -> bool {
    let t = row.trim();
    let Some(head) = t.strip_suffix(anchor_text("dialog.panel_waiting")) else {
        return false;
    };
    let head = head.trim_end();
    head == "Background task update"
        || head
            .strip_suffix(" background task updates")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// One right-aligned plugin notice (Fme(): one row, flex-end, dim) in a
/// dialog `width` cells wide: at least half the width of leading spaces, and
/// none of a dialog's, a rule's or the composer's glyphs.
fn plugin_notice_row(row: &str, width: usize) -> bool {
    let t = row.trim();
    !t.is_empty()
        && leading_spaces(row) >= (width / 2).max(1)
        && !t.contains(['❯', '─', '│', '╭', '╰', '┌', '└'])
        && numbered(t).is_none()
}

/// What sits under a dialog from row `from` down: blank rows, and at most
/// one each of the AFK countdown row, a plugin notice and the panel's
/// waiting row (the critique of 2026-09-25, R6). `Some(afk row)` when that
/// is all; `None` when anything else is there.
pub(crate) fn trailers(rows: &[String], from: usize, width: usize) -> Option<Option<usize>> {
    let (mut afk, mut plugin, mut panel) = (None, false, false);
    for (i, r) in rows.iter().enumerate().skip(from) {
        if blank(r) {
            continue;
        }
        if afk.is_none() && afk_row(r) {
            afk = Some(i);
        } else if !panel && panel_waiting_row(r) {
            panel = true;
        } else if !plugin && plugin_notice_row(r, width) {
            plugin = true;
        } else {
            return None;
        }
    }
    Some(afk)
}

/// The widest row on the screen, in chars: the pane's width wherever a
/// full-width rule is drawn.
pub(crate) fn screen_width(rows: &[String]) -> usize {
    rows.iter()
        .map(|r| r.trim_end().chars().count())
        .max()
        .unwrap_or(0)
}

// ---- the recognizers ------------------------------------------------------

/// A dialog's head over its options: the question's rows and the tab bar
/// (under the top rule).
struct Head {
    question: (usize, usize),
    bar: usize,
}

/// The head of a dialog, walked up from the blank row above its first
/// option (`gap`): the question's rows (all `│ `-led or all bare, at column
/// 0), a blank row, the tab bar, the top rule. `Err(())` when a row is none
/// of those (not this dialog's shape); `Ok(None)` when the walk ran off the
/// screen's top first (the head is cut).
fn head(rows: &[String], gap: usize) -> Result<Option<Head>, ()> {
    let Some(q_last) = gap.checked_sub(1) else {
        return Ok(None);
    };
    if blank(&rows[q_last]) {
        return Err(());
    }
    let mut q_top = q_last;
    while q_top > 0 && !blank(&rows[q_top - 1]) {
        q_top -= 1;
    }
    let question = &rows[q_top..gap];
    let gutter = question.iter().all(|r| r == "│" || r.starts_with("│ "));
    let bare = question
        .iter()
        .all(|r| leading_spaces(r) == 0 && !r.starts_with('│'));
    if !gutter && !bare {
        return Err(());
    }
    // The question ran up to row 0 (it may go on above the screen), or the
    // tab bar or the rule is above it.
    let Some(bar) = q_top.checked_sub(2) else {
        return Ok(None);
    };
    if tab_bar(&rows[bar]).is_none() {
        return Err(());
    }
    let Some(rule) = bar.checked_sub(1) else {
        return Ok(None);
    };
    if !is_rule(&rows[rule]) {
        return Err(());
    }
    Ok(Some(Head {
        question: (q_top, gap),
        bar,
    }))
}

/// A question tab over its key-hint FOOTER (row `footer`, a question's:
/// `Enter to select` and a `… to navigate` item): walked up from it — only
/// blank rows and R6's rows under it, and no composer frame; blank rows;
/// the chat row; the divider (a full-width rule, its width the dialog's);
/// the options zone; a blank row; the question; a blank row; the tab bar;
/// the top rule. `preview` when the footer has the `n to add notes` item.
/// `None` when the rows are not this shape (the caller reads them as
/// before); a shape whose walk ran off the screen's top once the options
/// began is the dialog with its head off the screen.
pub(crate) fn footed(rows: &[String], footer: usize, preview: bool) -> Option<QShape> {
    if crate::phase::composer_top(rows).is_some_and(|t| t > footer) {
        return None;
    }
    let mut chat = footer.checked_sub(1)?;
    if !blank(&rows[chat]) {
        return None;
    }
    while blank(&rows[chat]) {
        chat = chat.checked_sub(1)?;
    }
    chat_row(&rows[chat])?;
    let divider = chat.checked_sub(1)?;
    if !is_rule(&rows[divider]) {
        return None;
    }
    let width = rows[divider].trim_end().chars().count();
    let afk = trailers(rows, footer + 1, width)?;
    let first_visible = || (0..=footer).find(|&i| !blank(&rows[i])).unwrap_or(footer);
    let cut = || QShape {
        footer: Some(footer),
        afk,
        width,
        ..QShape::unknown(HEAD_CUT, first_visible(), footer, true)
    };
    let zone = if preview {
        preview_zone(rows, divider)?
    } else {
        options_zone(rows, divider)?
    };
    let Zone {
        top,
        button,
        pane_col,
        form,
    } = match zone {
        Walk::Cut => return Some(cut()),
        Walk::Rows(z) => z,
    };
    let above = top.checked_sub(1).filter(|&a| blank(&rows[a]));
    let Some(above) = above else {
        return if top == 0 { Some(cut()) } else { None };
    };
    let Head { question, bar } = match head(rows, above) {
        Err(()) => return None,
        Ok(None) => return Some(cut()),
        Ok(Some(h)) => h,
    };
    Some(QShape {
        form,
        tab_bar: Some(bar),
        question: Some(question),
        options: (top, divider),
        button,
        chat: Some(chat),
        footer: Some(footer),
        afk,
        warning: None,
        pane_col,
        width,
        first: bar,
        last: footer,
        head_off_screen: false,
    })
}

/// An options zone, read.
struct Zone {
    /// Its first row (the `1.` row when sound).
    top: usize,
    button: Option<usize>,
    pane_col: usize,
    form: QForm,
}

/// A walk up that either ran off the screen's top (every row consistent) or
/// found its rows.
enum Walk {
    Cut,
    Rows(Zone),
}

/// The single- or multi-select options zone above the divider: the
/// contiguous rows up to a blank row, each a numbered row (the pointer
/// column, the number at column 2), a row at the label column (5, or 9 in
/// multi-select: a description or a wrap) under one, or — multi-select, the
/// zone's last row — the button. `None` for any other row.
fn options_zone(rows: &[String], divider: usize) -> Option<Walk> {
    let mut top = divider.checked_sub(1)?;
    if blank(&rows[top]) {
        return None;
    }
    while top > 0 && !blank(&rows[top - 1]) {
        top -= 1;
    }
    let zone = top..divider;
    let texts: Vec<&str> = zone
        .clone()
        .filter_map(|i| numbered(&rows[i]).map(|(_, _, t)| t))
        .collect();
    let multi = !texts.is_empty() && texts.iter().all(|t| checkbox(t).is_some());
    let label_col = if multi { 9 } else { 5 };
    let mut button = None;
    let mut seen_numbered = false;
    let mut scrolled = false;
    for i in zone.clone() {
        let r = &rows[i];
        if let Some((pointer, _, _)) = numbered(r) {
            seen_numbered = true;
            scrolled |= pointer == Pointer::Scrolled;
        } else if multi && i + 1 == divider && button_row(r).is_some() {
            button = Some(i);
        } else if leading_spaces(r) == label_col && (seen_numbered || top == 0) {
            // A description or a wrap (under a numbered row; at the screen's
            // top, under one that is cut off).
        } else {
            return None;
        }
    }
    if top == 0 {
        return Some(Walk::Cut);
    }
    if !seen_numbered {
        return None;
    }
    let form = if scrolled {
        QForm::Unknown(SCROLLED)
    } else if multi {
        QForm::Multi
    } else {
        QForm::Single
    };
    Some(Walk::Rows(Zone {
        top,
        button,
        pane_col: 0,
        form,
    }))
}

/// The preview form's zone above the divider, walked up: blank rows; the
/// notes row(s) at the pane's column; blank rows; the option block — its
/// numbered rows with the pane (`┌─┐│└┘`) to their right, rows continuing a
/// label left of the pane (R8: a label wraps inside the 30-cell column),
/// and rows of the pane alone. The pane's column is the `┌`'s on the first
/// option row.
fn preview_zone(rows: &[String], divider: usize) -> Option<Walk> {
    let mut i = divider.checked_sub(1)?;
    while blank(&rows[i]) {
        i = i.checked_sub(1)?;
    }
    // The notes: rows indented past the option column.
    let notes_end = i + 1;
    while leading_spaces(&rows[i]) >= 4 && !blank(&rows[i]) {
        match i.checked_sub(1) {
            Some(up) => i = up,
            None => return Some(Walk::Cut),
        }
    }
    let notes_top = i + 1;
    if notes_top == notes_end || !blank(&rows[i]) {
        return None;
    }
    while blank(&rows[i]) {
        i = i.checked_sub(1)?;
    }
    let bottom = i;
    let mut top = bottom;
    while top > 0 && !blank(&rows[top - 1]) {
        top -= 1;
    }
    let first = &rows[top];
    let pane_col = match numbered(first) {
        Some(_) => cell_of(first, '┌'),
        None if top == 0 => None,
        None => return None,
    };
    let Some(pane_col) = pane_col else {
        return if top == 0 {
            Some(Walk::Cut)
        } else {
            Some(Walk::Rows(Zone {
                top,
                button: None,
                pane_col: 0,
                form: QForm::Unknown(NO_PANE),
            }))
        };
    };
    let mut scrolled = false;
    for r in &rows[top..=bottom] {
        let (left, _) = split_at_cell(r, pane_col);
        if let Some((pointer, _, _)) = numbered(left) {
            scrolled |= pointer == Pointer::Scrolled;
        } else if !blank(left) && leading_spaces(left) < 4 {
            return None;
        }
    }
    let notes: Vec<&String> = rows[notes_top..notes_end].iter().collect();
    let pristine = matches!(notes.as_slice(),
        [row] if leading_spaces(row) == pane_col && row.trim() == anchor_text("question.notes"));
    if !notes.iter().all(|r| leading_spaces(r) >= pane_col) {
        return None;
    }
    let form = if scrolled {
        QForm::Unknown(SCROLLED)
    } else if !pristine {
        QForm::Unknown(NOTES_OPEN)
    } else {
        QForm::Preview
    };
    Some(Walk::Rows(Zone {
        top,
        button: None,
        pane_col,
        form,
    }))
}

/// The review (Submit) tab, which draws no footer, read bottom up: under
/// it only blank rows and R6's rows; `2. <one word>` (its cancel, read by
/// position); `1. Submit answers`; a blank row; `Ready to submit your
/// answers?` at column 0; blank rows; the answers (rows indented one or
/// more); blank rows; optionally `⚠ You have not answered all questions`
/// and blank rows; `Review your answers` at column 0; blank rows; a tab bar
/// with a `✔ Submit` chip; the top rule. No composer frame anywhere (the
/// dialog replaces it), and no transcript row among its rows. When the two
/// options and `Ready to submit …` match but the rest does not (a cut, a
/// row out of place), the tab is read with its head off the screen, form
/// unknown: escalated, never idle.
pub(crate) fn review(rows: &[String]) -> Option<QShape> {
    // The reader runs on the window's main thread every frame: a screen
    // with no `Ready to submit …` row is no review, at the cost of a scan.
    let ready_text = anchor_text("question.review_ready");
    if !rows.iter().any(|r| r.trim_end() == ready_text)
        || crate::phase::composer_frame(rows).is_some()
    {
        return None;
    }
    let width = screen_width(rows);
    // The last row that is none of R6's (each at most once).
    let mut last = rows.len().checked_sub(1)?;
    loop {
        if !blank(&rows[last]) && trailers(rows, last, width).is_none() {
            break;
        }
        last = last.checked_sub(1)?;
    }
    let afk = trailers(rows, last + 1, width)?;
    let (pointer, n, cancel_label) = numbered(&rows[last])?;
    let cancel_label = cancel_label.trim_end();
    if n != 2
        || pointer == Pointer::Scrolled
        || cancel_label.is_empty()
        || cancel_label.contains(char::is_whitespace)
    {
        return None;
    }
    let submit = last.checked_sub(1)?;
    match numbered(&rows[submit]) {
        Some((p, 1, label))
            if p != Pointer::Scrolled
                && label.trim_end() == anchor_text("question.review_submit") => {}
        _ => return None,
    }
    let ready = submit.checked_sub(2)?;
    if !blank(&rows[submit - 1]) || rows[ready].trim_end() != ready_text {
        return None;
    }
    if rows[ready..=last].iter().any(|r| is_transcript_row(r)) {
        return None;
    }
    let shape =
        |form: QForm, first: usize, rule: Option<usize>, bar: Option<usize>, warning| QShape {
            form,
            tab_bar: bar,
            question: None,
            options: (submit, last + 1),
            button: None,
            chat: None,
            footer: None,
            afk,
            warning,
            pane_col: 0,
            width: rule.map_or(0, |r| rows[r].trim_end().chars().count()),
            first,
            last,
            head_off_screen: rule.is_none(),
        };
    // Walk up from `Ready to submit …`; `top` is the highest row read so far.
    let mut top = ready;
    let partial = |top: usize| {
        let first = (top..=ready).find(|&i| !blank(&rows[i])).unwrap_or(ready);
        let why = if top == 0 { HEAD_CUT } else { REVIEW_SHAPE };
        (!rows[first..=last].iter().any(|r| is_transcript_row(r)))
            .then(|| shape(QForm::Unknown(why), first, None, None, None))
    };
    // Blank rows, then the answers, then blank rows.
    let skip_blanks = |mut i: usize| -> Option<usize> {
        while blank(&rows[i]) {
            i = i.checked_sub(1)?;
        }
        Some(i)
    };
    let Some(gap) = ready.checked_sub(1).filter(|&i| blank(&rows[i])) else {
        return partial(top);
    };
    let Some(mut at) = skip_blanks(gap) else {
        return partial(0);
    };
    while !blank(&rows[at]) && leading_spaces(&rows[at]) >= 1 {
        top = at;
        match at.checked_sub(1) {
            Some(up) => at = up,
            None => return partial(0),
        }
    }
    let Some(mut at) = skip_blanks(at) else {
        return partial(0);
    };
    let warning_text = anchor_text("question.review_unanswered");
    let warning = rows[at]
        .strip_prefix("⚠ ")
        .is_some_and(|w| w.trim_end() == warning_text)
        .then_some(at);
    if warning.is_some() {
        top = at;
        let Some(up) = at.checked_sub(1).filter(|&u| blank(&rows[u])) else {
            return partial(if at == 0 { 0 } else { top });
        };
        match skip_blanks(up) {
            Some(a) => at = a,
            None => return partial(0),
        }
    }
    if rows[at].trim_end() != anchor_text("question.review_title") {
        return partial(top);
    }
    top = at;
    let Some(up) = at.checked_sub(1).filter(|&u| blank(&rows[u])) else {
        return partial(if at == 0 { 0 } else { top });
    };
    let Some(bar) = skip_blanks(up) else {
        return partial(0);
    };
    if !tab_bar(&rows[bar]).is_some_and(|(_, submit_tab)| submit_tab) {
        return partial(top);
    }
    let Some(rule) = bar.checked_sub(1) else {
        return partial(0);
    };
    if !is_rule(&rows[rule]) {
        return partial(top);
    }
    if rows[rule..=last].iter().any(|r| is_transcript_row(r)) {
        return None;
    }
    Some(shape(QForm::Review, bar, Some(rule), Some(bar), warning))
}

/// A dialog Claude Code WITHHOLDS while the composer holds a draft (Ufe()'s
/// reason `draft`, 2.1.282): no dialog, and in its place the dim notice
/// `Claude has a question for you — it shows once you send or clear what
/// you're typing.` (or `… a suggestion …`), one row or wrapped onto a few
/// at one column. Read where the live UI draws it — outside the composer's
/// frame, at column 0 or 1, no transcript row or user row between it and
/// the frame when it is above it — as a question not read whole (the
/// critique of 2026-09-25, R5: it read idle, a silent stall, while the
/// draft stood).
pub(crate) fn deferred(rows: &[String]) -> Option<QShape> {
    let frame = crate::phase::composer_frame(rows)?;
    let texts = [
        anchor_text("dialog.deferred_question"),
        anchor_text("dialog.deferred_suggestion"),
    ];
    for i in (0..rows.len()).rev() {
        if (frame.top..=frame.bottom).contains(&i) {
            continue;
        }
        let r = &rows[i];
        let col = leading_spaces(r);
        // The notice's first row is the start of one of the texts (the whole
        // of it, unwrapped): checked before anything is joined.
        let head = r.trim();
        if head.is_empty() || col > 1 || !texts.iter().any(|t| t.starts_with(head)) {
            continue;
        }
        let mut joined = head.to_string();
        let mut end = i;
        while !texts.contains(&joined.as_str()) && end < i + 3 {
            let Some(next) = rows.get(end + 1) else { break };
            if blank(next) || leading_spaces(next) != col || end + 1 == frame.top {
                break;
            }
            end += 1;
            joined.push(' ');
            joined.push_str(next.trim());
        }
        if !texts.contains(&joined.as_str()) {
            continue;
        }
        let live = i > frame.bottom
            || !rows[end + 1..frame.top]
                .iter()
                .any(|x| is_transcript_row(x) || x.starts_with('❯'));
        if live {
            return Some(QShape::unknown(WITHHELD, i, end, false));
        }
    }
    None
}

// ---- the reading ------------------------------------------------------------

/// The dialog read from `rows` over its shape: its tabs and its form, and
/// [`QuestionForm::Unknown`] when a soundness check fails (module header).
pub(crate) fn dialog(rows: &[String], shape: &QShape) -> QuestionDialog {
    let (tabs, submit_tab) = shape
        .tab_bar
        .and_then(|b| tab_bar(&rows[b]))
        .unwrap_or_default();
    let form = match shape.form {
        QForm::Unknown(why) => Err(why),
        QForm::Single | QForm::Multi => select_form(rows, shape),
        QForm::Preview => preview_form(rows, shape),
        QForm::Review => review_form(rows, shape),
    }
    .unwrap_or_else(|why| QuestionForm::Unknown { why });
    QuestionDialog {
        tabs,
        submit_tab,
        form,
        auto_continue: shape.afk.map(|a| rows[a].trim().to_string()),
        width: shape.width,
    }
}

/// The question's text over its rows.
fn question_text(rows: &[String], (start, end): (usize, usize)) -> QuestionText {
    let gutter = rows[start].starts_with('│');
    let text = rows[start..end]
        .iter()
        .map(|r| {
            let t = r.trim();
            if gutter {
                t.strip_prefix('│').map_or(t, str::trim)
            } else {
                t
            }
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    QuestionText {
        rows: start..end,
        text,
        gutter,
    }
}

/// A single- or multi-select tab: its options, its free-text row, its
/// button and its chat row, or why it is not sound.
fn select_form(rows: &[String], shape: &QShape) -> Result<QuestionForm, &'static str> {
    let (Some(q), Some(chat_at)) = (shape.question, shape.chat) else {
        return Err(HEAD_CUT);
    };
    let multi = shape.form == QForm::Multi;
    let (start, divider) = shape.options;
    let end = shape.button.unwrap_or(divider);
    // Each numbered row and the rows under it (to the next, or the end).
    let heads: Vec<usize> = (start..end)
        .filter(|&i| numbered(&rows[i]).is_some())
        .collect();
    let groups: Vec<(usize, usize)> = heads
        .iter()
        .enumerate()
        .map(|(k, &h)| (h, heads.get(k + 1).copied().unwrap_or(end)))
        .collect();
    if heads.first() != Some(&start)
        || groups
            .iter()
            .enumerate()
            .any(|(k, &(h, _))| numbered(&rows[h]).map(|(_, n, _)| usize::from(n)) != Some(k + 1))
    {
        return Err(NUMBERING);
    }
    let Some((&(free_row, free_end), option_groups)) = groups.split_last() else {
        return Err(NUMBERING);
    };
    if option_groups.is_empty() {
        return Err(NO_FREE_TEXT);
    }
    let (chat_n, chat_focused) = chat_row(&rows[chat_at]).ok_or(CHAT_NUMBER)?;
    if chat_n.map(usize::from) != Some(groups.len() + 1) {
        return Err(CHAT_NUMBER);
    }
    let mut options = Vec::new();
    for &(h, until) in option_groups {
        let Some((pointer, n, text)) = numbered(&rows[h]) else {
            return Err(NUMBERING);
        };
        let (checked, text) = if multi {
            let (c, t) = checkbox(text).ok_or(NUMBERING)?;
            (Some(c), t)
        } else {
            (None, text)
        };
        let (label, label_rows, description, recommended) =
            split_label(rows, h, until, text.trim_end(), shape.width);
        options.push(QuestionOption {
            n,
            row: h,
            label,
            label_rows,
            description,
            recommended,
            focused: pointer == Pointer::Focus,
            checked,
            selected: false,
        });
    }
    let Some((free_pointer, free_n, free_text)) = numbered(&rows[free_row]) else {
        return Err(NUMBERING);
    };
    let (free_checked, free_text) = if multi {
        let (c, t) = checkbox(free_text).ok_or(NUMBERING)?;
        (Some(c), t)
    } else {
        (None, free_text)
    };
    let free_text = std::iter::once(free_text.trim())
        .chain(rows[free_row + 1..free_end].iter().map(|r| r.trim()))
        .collect::<Vec<_>>()
        .join(" ");
    let placeholder = anchor_text("question.other");
    let pristine = free_end == free_row + 1
        && free_checked != Some(true)
        && free_text.strip_suffix('.').unwrap_or(&free_text) == placeholder;
    let free_text = FreeText {
        n: free_n,
        row: free_row,
        text: free_text,
        pristine,
        focused: free_pointer == Pointer::Focus,
        checked: free_checked,
    };
    let chat = ChatRow {
        n: chat_n,
        row: chat_at,
        focused: chat_focused,
    };
    let question = question_text(rows, q);
    let form = if multi {
        let (label, focused) = shape
            .button
            .and_then(|b| button_row(&rows[b]))
            .ok_or(NO_BUTTON)?;
        QuestionForm::Multi {
            question,
            options,
            free_text,
            button: ButtonRow {
                row: shape.button.unwrap_or(divider),
                label,
                focused,
            },
            chat,
        }
    } else {
        QuestionForm::Single {
            question,
            options,
            free_text,
            chat,
        }
    };
    one_focus(form)
}

/// `form` when exactly one of its rows carries the `❯` (R11: none is a
/// rendering this reader did not understand; 2.1.282 always draws one).
fn one_focus(form: QuestionForm) -> Result<QuestionForm, &'static str> {
    let probe = QuestionDialog {
        tabs: Vec::new(),
        submit_tab: false,
        form,
        auto_continue: None,
        width: 0,
    };
    if probe.focus() == QuestionFocus::None {
        Err(FOCUS)
    } else {
        Ok(probe.form)
    }
}

/// An option's label and description (module header): the option's row and
/// the rows under it to `until`. The LABEL CHAIN is the option's row and the
/// run of rows under it each [`possibly_wrap`] of the row above; the label
/// is [`Recommended::Wrapped`] at chain row `k` when the option's own row
/// does not end ` (Recommended)` and row `k` does. The label is the
/// option's row alone — or, wrapped, the chain to `k` — and the rest is the
/// description.
fn split_label(
    rows: &[String],
    row: usize,
    until: usize,
    text: &str,
    width: usize,
) -> (String, Range<usize>, String, Recommended) {
    let rec = anchor_text("question.recommended");
    // The option's row measured with its pointer as the one cell it is.
    let own = format!(" {}", rows[row].chars().skip(1).collect::<String>());
    let mut chain_end = row + 1;
    let mut above = own;
    while chain_end < until && possibly_wrap(&above, &rows[chain_end], width) {
        above.clone_from(&rows[chain_end]);
        chain_end += 1;
    }
    let recommended = if text.ends_with(rec) {
        Recommended::Label
    } else if let Some(k) = (row + 1..chain_end).find(|&k| rows[k].trim_end().ends_with(rec)) {
        Recommended::Wrapped { label_end: k }
    } else {
        Recommended::No
    };
    let label_end = match recommended {
        Recommended::Wrapped { label_end } => label_end + 1,
        _ => row + 1,
    };
    let label = std::iter::once(text.trim())
        .chain(rows[row + 1..label_end].iter().map(|r| r.trim()))
        .collect::<Vec<_>>()
        .join(" ");
    let description = rows[label_end..until]
        .iter()
        .map(|r| r.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (label, row..label_end, description, recommended)
}

/// The preview form's options (R8): each numbered row's cells left of the
/// pane, and the rows continuing it there, joined; a trailing ` ✔` is the
/// selection tick.
fn preview_form(rows: &[String], shape: &QShape) -> Result<QuestionForm, &'static str> {
    let (Some(q), Some(chat_at)) = (shape.question, shape.chat) else {
        return Err(HEAD_CUT);
    };
    let (start, _) = shape.options;
    let p = shape.pane_col;
    let mut options: Vec<QuestionOption> = Vec::new();
    let mut i = start;
    while let Some(r) = rows.get(i) {
        let (left, _) = split_at_cell(r, p);
        if blank(r) {
            break;
        }
        if let Some((pointer, n, text)) = numbered(left) {
            if usize::from(n) != options.len() + 1 {
                return Err(NUMBERING);
            }
            options.push(QuestionOption {
                n,
                row: i,
                label: text.trim().to_string(),
                label_rows: i..i + 1,
                description: String::new(),
                recommended: Recommended::No,
                focused: pointer == Pointer::Focus,
                checked: None,
                selected: false,
            });
        } else if !blank(left) {
            let Some(last) = options.last_mut() else {
                return Err(NUMBERING);
            };
            if last.label_rows.end != i {
                return Err(NUMBERING);
            }
            last.label.push(' ');
            last.label.push_str(left.trim());
            last.label_rows.end = i + 1;
        }
        i += 1;
    }
    if options.is_empty() {
        return Err(NUMBERING);
    }
    let rec = anchor_text("question.recommended");
    for o in &mut options {
        if let Some(l) = o.label.strip_suffix(" ✔") {
            o.label = l.trim_end().to_string();
            o.selected = true;
        }
        if o.label.ends_with(rec) {
            o.recommended = Recommended::Label;
        }
    }
    let (chat_n, chat_focused) = chat_row(&rows[chat_at]).ok_or(CHAT_NUMBER)?;
    if chat_n.is_some() {
        return Err(CHAT_NUMBER);
    }
    one_focus(QuestionForm::Preview {
        question: question_text(rows, q),
        options,
        chat: ChatRow {
            n: None,
            row: chat_at,
            focused: chat_focused,
        },
        pane_col: p,
    })
}

/// The review tab's two options.
fn review_form(rows: &[String], shape: &QShape) -> Result<QuestionForm, &'static str> {
    let (submit, end) = shape.options;
    let row = |i: usize| {
        numbered(&rows[i]).map(|(p, _, _)| ReviewRow {
            row: i,
            focused: p == Pointer::Focus,
        })
    };
    let (Some(submit), Some(cancel)) = (row(submit), end.checked_sub(1).and_then(row)) else {
        return Err(REVIEW_SHAPE);
    };
    one_focus(QuestionForm::Review {
        unanswered: shape.warning.is_some(),
        submit,
        cancel,
    })
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
