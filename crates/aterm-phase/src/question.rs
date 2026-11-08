// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code `AskUserQuestion` dialog parser ([`crate::prompt::PromptV2::question_dialog`],
//! [`crate::prompt`]): options ([`checkbox`], [`label_col`]), side-by-side preview
//! ([`preview_zone`]), review tab ([`review`]), trailers ([`trailers`]), and unsound form fallback
//! ([`QuestionForm::Unknown`]).

use std::ops::Range;

use crate::phase::{is_rule, is_transcript_row, leading_spaces};
use crate::text::blank;

/// The question dialog, read ([`crate::prompt::PromptV2::question_dialog`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionDialog {
    /// The tab bar's chips in order (the `✔ Submit` chip left out).
    pub tabs: Vec<QuestionTab>,
    /// A `✔ Submit` chip is drawn.
    pub submit_tab: bool,
    pub form: QuestionForm,
    /// The AFK countdown row under the dialog, as drawn (trimmed), when the host draws one.
    pub auto_continue: Option<String>,
    /// The dialog's width in cells.
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

/// Whether an option's label is marked `(Recommended)`, the tool's own convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recommended {
    /// The option's own row ends ` (Recommended)`.
    Label,
    /// A row under the option that may be the label's wrap (the row above
    /// it ran to the dialog's edge) ends ` (Recommended)`: row `label_end`,
    /// the label's last.
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
    /// Its label (the checkbox stripped).
    pub label: String,
    pub label_rows: Range<usize>,
    /// The rows under the label, joined with one space.
    pub description: String,
    pub recommended: Recommended,
    /// The `❯` focus is on it.
    pub focused: bool,
    /// Multi-select: its checkbox, `[✔]` (`Some(true)`) or `[ ]`.
    pub checked: Option<bool>,
    /// Preview form: the label ends with the selection tick ` ✔`.
    pub selected: bool,
}

/// The free-text row, `N+1.`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeText {
    pub n: u8,
    pub row: usize,
    /// Its text as drawn (the checkbox stripped).
    pub text: String,
    /// It shows the placeholder (`Type something.`, `Type something` in multi-select).
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
    /// `Next`, or `Submit` on the last question.
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
    /// Several answers: a digit, Space or Enter toggles.
    Multi {
        question: QuestionText,
        options: Vec<QuestionOption>,
        free_text: FreeText,
        button: ButtonRow,
        chat: ChatRow,
    },
    /// Options with a preview pane at column `pane_col`.
    Preview {
        question: QuestionText,
        options: Vec<QuestionOption>,
        chat: ChatRow,
        pane_col: usize,
    },
    /// The Submit tab: `1. Submit answers` / `2. Cancel` (its cancel rejects the tool call).
    Review {
        /// `⚠ You have not answered all questions` is drawn.
        unanswered: bool,
        submit: ReviewRow,
        cancel: ReviewRow,
    },
    /// Read as a question, not whole (module header).
    Unknown { why: &'static str },
}

/// Where the dialog's one `❯` sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionFocus {
    Option(u8),
    FreeText,
    Chat,
    Button,
    ReviewSubmit,
    ReviewCancel,
    /// No row, or more than one, carries it (and every [`QuestionForm::Unknown`]).
    None,
}

impl QuestionDialog {
    /// A question named by its footer or its options alone, whose rows were not read.
    #[must_use]
    pub(crate) const fn unknown(why: &'static str) -> Self {
        Self {
            tabs: Vec::new(),
            submit_tab: false,
            form: QuestionForm::Unknown { why },
            auto_continue: None,
            width: 0,
        }
    }

    /// The question the tab asks.
    #[must_use]
    pub const fn question(&self) -> Option<&QuestionText> {
        match &self.form {
            QuestionForm::Single { question, .. }
            | QuestionForm::Multi { question, .. }
            | QuestionForm::Preview { question, .. } => Some(question),
            QuestionForm::Review { .. } | QuestionForm::Unknown { .. } => None,
        }
    }

    /// The row a key is guarded by.
    #[must_use]
    pub fn guard_row(&self) -> Option<usize> {
        match &self.form {
            QuestionForm::Review { submit, .. } => Some(submit.row),
            _ => self.question().map(|q| q.rows.start),
        }
    }

    /// Where the `❯` is: exactly one of the dialog's rows carries it, else [`QuestionFocus::None`].
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

    /// The same tab as `other`.
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

/// Which form the rows were read as ([`QShape`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QForm {
    Single,
    Multi,
    Preview,
    Review,
    Unknown(&'static str),
}

/// A question dialog's rows on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QShape {
    pub(crate) form: QForm,
    pub(crate) tab_bar: Option<usize>,
    /// The question's rows, `[start, end)`.
    pub(crate) question: Option<(usize, usize)>,
    /// The options zone, `[start, end)`.
    pub(crate) options: (usize, usize),
    pub(crate) button: Option<usize>,
    pub(crate) chat: Option<usize>,
    pub(crate) footer: Option<usize>,
    pub(crate) afk: Option<usize>,
    /// The review's `⚠ You have not answered all questions` row.
    pub(crate) warning: Option<usize>,
    /// The review's auto-deny countdown rows `[start, end)`, the one place a dialog's countdown is read.
    pub(crate) countdown: Option<(usize, usize)>,
    /// The preview pane's column, in cells.
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
    const fn unknown(why: &'static str, first: usize, last: usize, head_off_screen: bool) -> Self {
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
            countdown: None,
            pane_col: 0,
            width: 0,
            first,
            last,
            head_off_screen,
        }
    }

    /// A live question footer whose rows are none of the shapes read (one a later release draws).
    pub(crate) const fn fallback(first: usize, footer: usize) -> Self {
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
    /// `↑` / `↓` in place of the pointer.
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

/// Parse a multi-select label's `[ ]` or `[✔]` checkbox and remaining label text.
fn checkbox(text: &str) -> Option<(bool, &str)> {
    let (ticked, rest) = match text.strip_prefix("[ ]") {
        Some(rest) => (false, rest),
        None => (true, text.strip_prefix("[✔]")?),
    };
    match rest.strip_prefix(' ') {
        Some(label) => Some((ticked, label)),
        None => rest
            .starts_with(|c: char| !c.is_whitespace())
            .then_some((ticked, rest)),
    }
}

/// The column a numbered row's label starts at, its rows under it with it:
/// 5, past `N. `, and in multi-select past the checkbox as
/// drawn ([`checkbox`]: 9 past `[ ] `, 8 past `[✔]` or `[ ]` with no space).
fn label_col(text: &str, multi: bool) -> usize {
    match checkbox(text).filter(|_| multi) {
        Some((_, label)) => 5 + cells(text) - cells(label),
        None => 5,
    }
}

/// The chat row: `  N. Chat about this` / `❯ N. Chat about this`, or the
/// preview's unnumbered `  Chat about this` / `❯ Chat about this` — as its
/// number and focus.
fn chat_row(row: &str) -> Option<(Option<u8>, bool)> {
    let chat = const { crate::anchor!("question.chat") };
    if let Some((pointer, n, text)) = numbered(row) {
        return (pointer != Pointer::Scrolled && crate::text::trim_end(text) == chat)
            .then_some((Some(n), pointer == Pointer::Focus));
    }
    let (focused, rest) = match row.strip_prefix('❯') {
        Some(rest) => (true, rest),
        None => (false, row),
    };
    (rest.starts_with(' ') && leading_spaces(rest) <= 2 && crate::text::trim(rest) == chat)
        .then_some((None, focused))
}

/// The multi-select button row: one word of letters at column 5 (`Submit`,
/// `Next`), or `❯` and four spaces before it (the focus).
fn button_row(row: &str) -> Option<(String, bool)> {
    let (focused, rest) = match row.strip_prefix('❯') {
        Some(rest) => (true, rest.strip_prefix("    ")?),
        None => (false, row.strip_prefix("     ")?),
    };
    let word = crate::text::trim_end(rest);
    (!word.is_empty() && word.chars().all(char::is_alphabetic)).then(|| (word.to_string(), focused))
}

/// The tab bar: ` ☐ Colors` (one chip, column 1), or `← ☐ A ☒ B ✔ Submit →` (column 0).
pub(crate) fn tab_bar(row: &str) -> Option<(Vec<QuestionTab>, bool)> {
    let t = crate::text::trim_end(row);
    let (inner, submit_tab) = if let Some(rest) = t.strip_prefix('←') {
        let inner = rest.strip_suffix('→')?.trim_end();
        (
            inner.strip_suffix(const { crate::anchor!("question.tab_submit") })?,
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
        if lead && !crate::text::trim(&inner[..i]).is_empty() {
            return None;
        }
        lead = false;
        let header = crate::text::trim(&after[..end]);
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

/// A row's width in cells when it can be measured: printable ASCII (as `aterm-agent`'s
/// `cells_are_chars` measures a box's command row) and the one-cell glyphs the dialog
/// draws (`❯`, `✔`, `│` and the scroll arrows).
fn measured(row: &str) -> Option<usize> {
    let t = crate::text::trim_end(row);
    t.chars()
        .all(|c| (' '..='~').contains(&c) || matches!(c, '❯' | '✔' | '↑' | '↓' | '│'))
        .then(|| t.chars().count())
}

/// Whether row `below` may be the wrap of row `above` in a dialog `width` cells wide.
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

/// The characters aterm's grid draws two cells wide, as inclusive code point ranges in order:
/// the grid's own widths (`aterm_grapheme::char_width`, which aterm-core's write path and its
/// emoji bitmaps agree with), East Asian wide and fullwidth and the emoji presented as emoji.
/// Spelled out, with [`ZERO`], [`VS16_WIDENS`] and [`is_modifier_base`], so the library keeps its
/// one dependency (`Cargo.toml` says why); `a_character_is_counted_in_the_cells_the_grid_draws_it_in`
/// holds each to the grid's own table over every code point.
#[rustfmt::skip]
const WIDE: [(u32, u32); 123] = [
    (0x1100, 0x115F), (0x231A, 0x231B), (0x2329, 0x232A), (0x23E9, 0x23EC),
    (0x23F0, 0x23F0), (0x23F3, 0x23F3), (0x25FD, 0x25FE), (0x2614, 0x2615),
    (0x2630, 0x2637), (0x2648, 0x2653), (0x267F, 0x267F), (0x268A, 0x268F),
    (0x2693, 0x2693), (0x26A1, 0x26A1), (0x26AA, 0x26AB), (0x26BD, 0x26BE),
    (0x26C4, 0x26C5), (0x26CE, 0x26CE), (0x26D4, 0x26D4), (0x26EA, 0x26EA),
    (0x26F2, 0x26F3), (0x26F5, 0x26F5), (0x26FA, 0x26FA), (0x26FD, 0x26FD),
    (0x2705, 0x2705), (0x270A, 0x270B), (0x2728, 0x2728), (0x274C, 0x274C),
    (0x274E, 0x274E), (0x2753, 0x2755), (0x2757, 0x2757), (0x2795, 0x2797),
    (0x27B0, 0x27B0), (0x27BF, 0x27BF), (0x2B1B, 0x2B1C), (0x2B50, 0x2B50),
    (0x2B55, 0x2B55), (0x2E80, 0x2E99), (0x2E9B, 0x2EF3), (0x2F00, 0x2FD5),
    (0x2FF0, 0x3029), (0x302E, 0x303E), (0x3041, 0x3096), (0x309B, 0x30FF),
    (0x3105, 0x312F), (0x3131, 0x318E), (0x3190, 0x31E5), (0x31EF, 0x321E),
    (0x3220, 0x3247), (0x3250, 0xA48C), (0xA490, 0xA4C6), (0xA960, 0xA97C),
    (0xAC00, 0xD7A3), (0xF900, 0xFAFF), (0xFE10, 0xFE19), (0xFE30, 0xFE52),
    (0xFE54, 0xFE66), (0xFE68, 0xFE6B), (0xFF01, 0xFF60), (0xFFE0, 0xFFE6),
    (0x16FE0, 0x16FE3), (0x16FF0, 0x16FF1), (0x17000, 0x187F7), (0x18800, 0x18CD5),
    (0x18CFF, 0x18D08), (0x1AFF0, 0x1AFF3), (0x1AFF5, 0x1AFFB), (0x1AFFD, 0x1AFFE),
    (0x1B000, 0x1B122), (0x1B132, 0x1B132), (0x1B150, 0x1B152), (0x1B155, 0x1B155),
    (0x1B164, 0x1B167), (0x1B170, 0x1B2FB), (0x1D300, 0x1D356), (0x1D360, 0x1D376),
    (0x1F004, 0x1F004), (0x1F0CF, 0x1F0CF), (0x1F18E, 0x1F18E), (0x1F191, 0x1F19A),
    (0x1F1E6, 0x1F202), (0x1F210, 0x1F23B), (0x1F240, 0x1F248), (0x1F250, 0x1F251),
    (0x1F260, 0x1F265), (0x1F300, 0x1F320), (0x1F32D, 0x1F335), (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393), (0x1F3A0, 0x1F3CA), (0x1F3CF, 0x1F3D3), (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4), (0x1F3F8, 0x1F43E), (0x1F440, 0x1F440), (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D), (0x1F54B, 0x1F54E), (0x1F550, 0x1F567), (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596), (0x1F5A4, 0x1F5A4), (0x1F5FB, 0x1F64F), (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC), (0x1F6D0, 0x1F6D2), (0x1F6D5, 0x1F6D7), (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC), (0x1F6F4, 0x1F6FC), (0x1F7E0, 0x1F7EB), (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A), (0x1F93C, 0x1F945), (0x1F947, 0x1F9FF), (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA89), (0x1FA8F, 0x1FAC6), (0x1FACE, 0x1FADC), (0x1FADF, 0x1FAE9),
    (0x1FAF0, 0x1FAF8), (0x20000, 0x2FFFD), (0x30000, 0x323AF),
];

/// The characters aterm's grid draws in no cell of their own (`aterm_grapheme::char_width` 0), as
/// inclusive code point ranges in order: combining marks, variation selectors, joiners and the
/// other format characters, Hangul's medial vowels and finals, and the controls. The grid stores
/// one in the cell before it ([`Columns`]).
#[rustfmt::skip]
const ZERO: [(u32, u32); 370] = [
    (0x0000, 0x001F), (0x007F, 0x009F), (0x0300, 0x036F), (0x0483, 0x0489),
    (0x0591, 0x05BD), (0x05BF, 0x05BF), (0x05C1, 0x05C2), (0x05C4, 0x05C5),
    (0x05C7, 0x05C7), (0x0600, 0x0605), (0x0610, 0x061A), (0x061C, 0x061C),
    (0x064B, 0x065F), (0x0670, 0x0670), (0x06D6, 0x06DD), (0x06DF, 0x06E4),
    (0x06E7, 0x06E8), (0x06EA, 0x06ED), (0x070F, 0x070F), (0x0711, 0x0711),
    (0x0730, 0x074A), (0x07A6, 0x07B0), (0x07EB, 0x07F3), (0x07FD, 0x07FD),
    (0x0816, 0x0819), (0x081B, 0x0823), (0x0825, 0x0827), (0x0829, 0x082D),
    (0x0859, 0x085B), (0x0890, 0x0891), (0x0897, 0x089F), (0x08CA, 0x0902),
    (0x093A, 0x093A), (0x093C, 0x093C), (0x0941, 0x0948), (0x094D, 0x094D),
    (0x0951, 0x0957), (0x0962, 0x0963), (0x0981, 0x0981), (0x09BC, 0x09BC),
    (0x09C1, 0x09C4), (0x09CD, 0x09CD), (0x09E2, 0x09E3), (0x09FE, 0x09FE),
    (0x0A01, 0x0A02), (0x0A3C, 0x0A3C), (0x0A41, 0x0A42), (0x0A47, 0x0A48),
    (0x0A4B, 0x0A4D), (0x0A51, 0x0A51), (0x0A70, 0x0A71), (0x0A75, 0x0A75),
    (0x0A81, 0x0A82), (0x0ABC, 0x0ABC), (0x0AC1, 0x0AC5), (0x0AC7, 0x0AC8),
    (0x0ACD, 0x0ACD), (0x0AE2, 0x0AE3), (0x0AFA, 0x0AFF), (0x0B01, 0x0B01),
    (0x0B3C, 0x0B3C), (0x0B3F, 0x0B3F), (0x0B41, 0x0B44), (0x0B4D, 0x0B4D),
    (0x0B55, 0x0B56), (0x0B62, 0x0B63), (0x0B82, 0x0B82), (0x0BC0, 0x0BC0),
    (0x0BCD, 0x0BCD), (0x0C00, 0x0C00), (0x0C04, 0x0C04), (0x0C3C, 0x0C3C),
    (0x0C3E, 0x0C40), (0x0C46, 0x0C48), (0x0C4A, 0x0C4D), (0x0C55, 0x0C56),
    (0x0C62, 0x0C63), (0x0C81, 0x0C81), (0x0CBC, 0x0CBC), (0x0CBF, 0x0CBF),
    (0x0CC6, 0x0CC6), (0x0CCC, 0x0CCD), (0x0CE2, 0x0CE3), (0x0D00, 0x0D01),
    (0x0D3B, 0x0D3C), (0x0D41, 0x0D44), (0x0D4D, 0x0D4D), (0x0D62, 0x0D63),
    (0x0D81, 0x0D81), (0x0DCA, 0x0DCA), (0x0DD2, 0x0DD4), (0x0DD6, 0x0DD6),
    (0x0E31, 0x0E31), (0x0E34, 0x0E3A), (0x0E47, 0x0E4E), (0x0EB1, 0x0EB1),
    (0x0EB4, 0x0EBC), (0x0EC8, 0x0ECE), (0x0F18, 0x0F19), (0x0F35, 0x0F35),
    (0x0F37, 0x0F37), (0x0F39, 0x0F39), (0x0F71, 0x0F7E), (0x0F80, 0x0F84),
    (0x0F86, 0x0F87), (0x0F8D, 0x0F97), (0x0F99, 0x0FBC), (0x0FC6, 0x0FC6),
    (0x102D, 0x1030), (0x1032, 0x1037), (0x1039, 0x103A), (0x103D, 0x103E),
    (0x1058, 0x1059), (0x105E, 0x1060), (0x1071, 0x1074), (0x1082, 0x1082),
    (0x1085, 0x1086), (0x108D, 0x108D), (0x109D, 0x109D), (0x1160, 0x11FF),
    (0x135D, 0x135F), (0x1712, 0x1714), (0x1732, 0x1733), (0x1752, 0x1753),
    (0x1772, 0x1773), (0x17B4, 0x17B5), (0x17B7, 0x17BD), (0x17C6, 0x17C6),
    (0x17C9, 0x17D3), (0x17DD, 0x17DD), (0x180B, 0x180F), (0x1885, 0x1886),
    (0x18A9, 0x18A9), (0x1920, 0x1922), (0x1927, 0x1928), (0x1932, 0x1932),
    (0x1939, 0x193B), (0x1A17, 0x1A18), (0x1A1B, 0x1A1B), (0x1A56, 0x1A56),
    (0x1A58, 0x1A5E), (0x1A60, 0x1A60), (0x1A62, 0x1A62), (0x1A65, 0x1A6C),
    (0x1A73, 0x1A7C), (0x1A7F, 0x1A7F), (0x1AB0, 0x1ACE), (0x1B00, 0x1B03),
    (0x1B34, 0x1B34), (0x1B36, 0x1B3A), (0x1B3C, 0x1B3C), (0x1B42, 0x1B42),
    (0x1B6B, 0x1B73), (0x1B80, 0x1B81), (0x1BA2, 0x1BA5), (0x1BA8, 0x1BA9),
    (0x1BAB, 0x1BAD), (0x1BE6, 0x1BE6), (0x1BE8, 0x1BE9), (0x1BED, 0x1BED),
    (0x1BEF, 0x1BF1), (0x1C2C, 0x1C33), (0x1C36, 0x1C37), (0x1CD0, 0x1CD2),
    (0x1CD4, 0x1CE0), (0x1CE2, 0x1CE8), (0x1CED, 0x1CED), (0x1CF4, 0x1CF4),
    (0x1CF8, 0x1CF9), (0x1DC0, 0x1DFF), (0x200B, 0x200F), (0x202A, 0x202E),
    (0x2060, 0x2064), (0x2066, 0x206F), (0x20D0, 0x20F0), (0x2CEF, 0x2CF1),
    (0x2D7F, 0x2D7F), (0x2DE0, 0x2DFF), (0x302A, 0x302D), (0x3099, 0x309A),
    (0xA66F, 0xA672), (0xA674, 0xA67D), (0xA69E, 0xA69F), (0xA6F0, 0xA6F1),
    (0xA802, 0xA802), (0xA806, 0xA806), (0xA80B, 0xA80B), (0xA825, 0xA826),
    (0xA82C, 0xA82C), (0xA8C4, 0xA8C5), (0xA8E0, 0xA8F1), (0xA8FF, 0xA8FF),
    (0xA926, 0xA92D), (0xA947, 0xA951), (0xA980, 0xA982), (0xA9B3, 0xA9B3),
    (0xA9B6, 0xA9B9), (0xA9BC, 0xA9BD), (0xA9E5, 0xA9E5), (0xAA29, 0xAA2E),
    (0xAA31, 0xAA32), (0xAA35, 0xAA36), (0xAA43, 0xAA43), (0xAA4C, 0xAA4C),
    (0xAA7C, 0xAA7C), (0xAAB0, 0xAAB0), (0xAAB2, 0xAAB4), (0xAAB7, 0xAAB8),
    (0xAABE, 0xAABF), (0xAAC1, 0xAAC1), (0xAAEC, 0xAAED), (0xAAF6, 0xAAF6),
    (0xABE5, 0xABE5), (0xABE8, 0xABE8), (0xABED, 0xABED), (0xD7B0, 0xD7C6),
    (0xD7CB, 0xD7FB), (0xFB1E, 0xFB1E), (0xFE00, 0xFE0F), (0xFE20, 0xFE2F),
    (0xFEFF, 0xFEFF), (0xFFF9, 0xFFFB), (0x101FD, 0x101FD), (0x102E0, 0x102E0),
    (0x10376, 0x1037A), (0x10A01, 0x10A03), (0x10A05, 0x10A06), (0x10A0C, 0x10A0F),
    (0x10A38, 0x10A3A), (0x10A3F, 0x10A3F), (0x10AE5, 0x10AE6), (0x10D24, 0x10D27),
    (0x10D69, 0x10D6D), (0x10EAB, 0x10EAC), (0x10EFC, 0x10EFF), (0x10F46, 0x10F50),
    (0x10F82, 0x10F85), (0x11001, 0x11001), (0x11038, 0x11046), (0x11070, 0x11070),
    (0x11073, 0x11074), (0x1107F, 0x11081), (0x110B3, 0x110B6), (0x110B9, 0x110BA),
    (0x110BD, 0x110BD), (0x110C2, 0x110C2), (0x110CD, 0x110CD), (0x11100, 0x11102),
    (0x11127, 0x1112B), (0x1112D, 0x11134), (0x11173, 0x11173), (0x11180, 0x11181),
    (0x111B6, 0x111BE), (0x111C9, 0x111CC), (0x111CF, 0x111CF), (0x1122F, 0x11231),
    (0x11234, 0x11234), (0x11236, 0x11237), (0x1123E, 0x1123E), (0x11241, 0x11241),
    (0x112DF, 0x112DF), (0x112E3, 0x112EA), (0x11300, 0x11301), (0x1133B, 0x1133C),
    (0x11340, 0x11340), (0x11366, 0x1136C), (0x11370, 0x11374), (0x113BB, 0x113C0),
    (0x113CE, 0x113CE), (0x113D0, 0x113D0), (0x113D2, 0x113D2), (0x113E1, 0x113E2),
    (0x11438, 0x1143F), (0x11442, 0x11444), (0x11446, 0x11446), (0x1145E, 0x1145E),
    (0x114B3, 0x114B8), (0x114BA, 0x114BA), (0x114BF, 0x114C0), (0x114C2, 0x114C3),
    (0x115B2, 0x115B5), (0x115BC, 0x115BD), (0x115BF, 0x115C0), (0x115DC, 0x115DD),
    (0x11633, 0x1163A), (0x1163D, 0x1163D), (0x1163F, 0x11640), (0x116AB, 0x116AB),
    (0x116AD, 0x116AD), (0x116B0, 0x116B5), (0x116B7, 0x116B7), (0x1171D, 0x1171D),
    (0x1171F, 0x1171F), (0x11722, 0x11725), (0x11727, 0x1172B), (0x1182F, 0x11837),
    (0x11839, 0x1183A), (0x1193B, 0x1193C), (0x1193E, 0x1193E), (0x11943, 0x11943),
    (0x119D4, 0x119D7), (0x119DA, 0x119DB), (0x119E0, 0x119E0), (0x11A01, 0x11A0A),
    (0x11A33, 0x11A38), (0x11A3B, 0x11A3E), (0x11A47, 0x11A47), (0x11A51, 0x11A56),
    (0x11A59, 0x11A5B), (0x11A8A, 0x11A96), (0x11A98, 0x11A99), (0x11C30, 0x11C36),
    (0x11C38, 0x11C3D), (0x11C3F, 0x11C3F), (0x11C92, 0x11CA7), (0x11CAA, 0x11CB0),
    (0x11CB2, 0x11CB3), (0x11CB5, 0x11CB6), (0x11D31, 0x11D36), (0x11D3A, 0x11D3A),
    (0x11D3C, 0x11D3D), (0x11D3F, 0x11D45), (0x11D47, 0x11D47), (0x11D90, 0x11D91),
    (0x11D95, 0x11D95), (0x11D97, 0x11D97), (0x11EF3, 0x11EF4), (0x11F00, 0x11F01),
    (0x11F36, 0x11F3A), (0x11F40, 0x11F40), (0x11F42, 0x11F42), (0x11F5A, 0x11F5A),
    (0x13430, 0x13440), (0x13447, 0x13455), (0x1611E, 0x16129), (0x1612D, 0x1612F),
    (0x16AF0, 0x16AF4), (0x16B30, 0x16B36), (0x16F4F, 0x16F4F), (0x16F8F, 0x16F92),
    (0x16FE4, 0x16FE4), (0x1BC9D, 0x1BC9E), (0x1BCA0, 0x1BCA3), (0x1CF00, 0x1CF2D),
    (0x1CF30, 0x1CF46), (0x1D167, 0x1D169), (0x1D173, 0x1D182), (0x1D185, 0x1D18B),
    (0x1D1AA, 0x1D1AD), (0x1D242, 0x1D244), (0x1DA00, 0x1DA36), (0x1DA3B, 0x1DA6C),
    (0x1DA75, 0x1DA75), (0x1DA84, 0x1DA84), (0x1DA9B, 0x1DA9F), (0x1DAA1, 0x1DAAF),
    (0x1E000, 0x1E006), (0x1E008, 0x1E018), (0x1E01B, 0x1E021), (0x1E023, 0x1E024),
    (0x1E026, 0x1E02A), (0x1E08F, 0x1E08F), (0x1E130, 0x1E136), (0x1E2AE, 0x1E2AE),
    (0x1E2EC, 0x1E2EF), (0x1E4EC, 0x1E4EF), (0x1E5EE, 0x1E5EF), (0x1E8D0, 0x1E8D6),
    (0x1E944, 0x1E94A), (0xE0000, 0xE0FFF),
];

/// The one-cell characters a VS16 (U+FE0F) after them widens to two cells, as inclusive code point
/// ranges in order: the emoji whose own presentation is text (`aterm_grapheme::is_vs16_emoji_capable`
/// and one cell wide: `❤`, `⚠`, `©`, the keycaps' `#`, `*` and digits).
#[rustfmt::skip]
const VS16_WIDENS: [(u32, u32); 113] = [
    (0x0023, 0x0023), (0x002A, 0x002A), (0x0030, 0x0039), (0x00A9, 0x00A9),
    (0x00AE, 0x00AE), (0x203C, 0x203C), (0x2049, 0x2049), (0x2122, 0x2122),
    (0x2139, 0x2139), (0x2194, 0x2199), (0x21A9, 0x21AA), (0x2328, 0x2328),
    (0x23CF, 0x23CF), (0x23ED, 0x23EF), (0x23F1, 0x23F2), (0x23F8, 0x23FA),
    (0x24C2, 0x24C2), (0x25AA, 0x25AB), (0x25B6, 0x25B6), (0x25C0, 0x25C0),
    (0x25FB, 0x25FC), (0x2600, 0x2604), (0x260E, 0x260E), (0x2611, 0x2611),
    (0x2618, 0x2618), (0x261D, 0x261D), (0x2620, 0x2620), (0x2622, 0x2623),
    (0x2626, 0x2626), (0x262A, 0x262A), (0x262E, 0x262F), (0x2638, 0x263A),
    (0x2640, 0x2640), (0x2642, 0x2642), (0x265F, 0x2660), (0x2663, 0x2663),
    (0x2665, 0x2666), (0x2668, 0x2668), (0x267B, 0x267B), (0x267E, 0x267E),
    (0x2692, 0x2692), (0x2694, 0x2697), (0x2699, 0x2699), (0x269B, 0x269C),
    (0x26A0, 0x26A0), (0x26A7, 0x26A7), (0x26B0, 0x26B1), (0x26C8, 0x26C8),
    (0x26CF, 0x26CF), (0x26D1, 0x26D1), (0x26D3, 0x26D3), (0x26E9, 0x26E9),
    (0x26F0, 0x26F1), (0x26F4, 0x26F4), (0x26F7, 0x26F9), (0x2702, 0x2702),
    (0x2708, 0x2709), (0x270C, 0x270D), (0x270F, 0x270F), (0x2712, 0x2712),
    (0x2714, 0x2714), (0x2716, 0x2716), (0x271D, 0x271D), (0x2721, 0x2721),
    (0x2733, 0x2734), (0x2744, 0x2744), (0x2747, 0x2747), (0x2763, 0x2764),
    (0x27A1, 0x27A1), (0x2934, 0x2935), (0x2B05, 0x2B07), (0x1F170, 0x1F171),
    (0x1F17E, 0x1F17F), (0x1F321, 0x1F321), (0x1F324, 0x1F32C), (0x1F336, 0x1F336),
    (0x1F37D, 0x1F37D), (0x1F396, 0x1F397), (0x1F399, 0x1F39B), (0x1F39E, 0x1F39F),
    (0x1F3CB, 0x1F3CE), (0x1F3D4, 0x1F3DF), (0x1F3F3, 0x1F3F3), (0x1F3F5, 0x1F3F5),
    (0x1F3F7, 0x1F3F7), (0x1F43F, 0x1F43F), (0x1F441, 0x1F441), (0x1F4FD, 0x1F4FD),
    (0x1F549, 0x1F54A), (0x1F56F, 0x1F570), (0x1F573, 0x1F579), (0x1F587, 0x1F587),
    (0x1F58A, 0x1F58D), (0x1F590, 0x1F590), (0x1F5A5, 0x1F5A5), (0x1F5A8, 0x1F5A8),
    (0x1F5B1, 0x1F5B2), (0x1F5BC, 0x1F5BC), (0x1F5C2, 0x1F5C4), (0x1F5D1, 0x1F5D3),
    (0x1F5DC, 0x1F5DE), (0x1F5E1, 0x1F5E1), (0x1F5E3, 0x1F5E3), (0x1F5E8, 0x1F5E8),
    (0x1F5EF, 0x1F5EF), (0x1F5F3, 0x1F5F3), (0x1F5FA, 0x1F5FA), (0x1F6CB, 0x1F6CB),
    (0x1F6CD, 0x1F6CF), (0x1F6E0, 0x1F6E5), (0x1F6E9, 0x1F6E9), (0x1F6F0, 0x1F6F0),
    (0x1F6F3, 0x1F6F3),
];

/// The variation selector that asks for text presentation: it narrows a two-cell emoji's cell.
const VS15: char = '\u{FE0E}';
/// The variation selector that asks for emoji presentation: it widens a [`VS16_WIDENS`] cell.
const VS16: char = '\u{FE0F}';
/// The zero-width joiner: the grid folds the character after it into its cell.
const ZWJ: char = '\u{200D}';
/// The characters one cell stores after its base (`aterm_grid`'s `CellExtra::MAX_COMBINING`).
const MAX_JOINED: u8 = 16;

/// Whether `c` falls in one of `table`'s inclusive ranges, which are in order and apart.
fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let cp = u32::from(c);
    let at = table.partition_point(|&(_, last)| last < cp);
    table.get(at).is_some_and(|&(first, _)| first <= cp)
}

/// A character's own width in cells, as aterm's grid reads it (`aterm_grapheme::char_width`): none
/// for a character in [`ZERO`], two for one in [`WIDE`], one for any other. A row's cells are its
/// characters laid out together ([`Columns`]), which this width alone does not tell.
fn cell_width(c: char) -> usize {
    if (' '..='~').contains(&c) {
        1
    } else if in_ranges(&ZERO, c) {
        0
    } else if in_ranges(&WIDE, c) {
        2
    } else {
        1
    }
}

/// A Fitzpatrick skin-tone modifier (U+1F3FB..=U+1F3FF).
const fn is_skin_tone(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// A regional indicator (U+1F1E6..=U+1F1FF), the half of a flag.
const fn is_regional_indicator(c: char) -> bool {
    matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
}

/// Whether `c` takes a skin-tone modifier (`aterm_grapheme::is_emoji_modifier_base`).
const fn is_modifier_base(c: char) -> bool {
    matches!(
        c as u32,
        0x261D | 0x26F9
            | 0x270A..=0x270D
            | 0x1F385
            | 0x1F3C2..=0x1F3C4
            | 0x1F3C7
            | 0x1F3CA..=0x1F3CC
            | 0x1F442..=0x1F443
            | 0x1F446..=0x1F450
            | 0x1F466..=0x1F478
            | 0x1F47C
            | 0x1F481..=0x1F483
            | 0x1F485..=0x1F487
            | 0x1F48F
            | 0x1F491
            | 0x1F4AA
            | 0x1F574..=0x1F575
            | 0x1F57A
            | 0x1F590
            | 0x1F595..=0x1F596
            | 0x1F645..=0x1F647
            | 0x1F64B..=0x1F64F
            | 0x1F6A3
            | 0x1F6B4..=0x1F6B6
            | 0x1F6C0
            | 0x1F6CC
            | 0x1F90C
            | 0x1F90F
            | 0x1F918..=0x1F91F
            | 0x1F926
            | 0x1F930..=0x1F939
            | 0x1F93C..=0x1F93E
            | 0x1F977
            | 0x1F9B5..=0x1F9B6
            | 0x1F9B8..=0x1F9B9
            | 0x1F9BB
            | 0x1F9CD..=0x1F9CF
            | 0x1F9D1..=0x1F9DD
            | 0x1FAC3..=0x1FAC5
            | 0x1FAF0..=0x1FAF8
    )
}

/// The cell the last printing character opened, as the grid holds it.
#[derive(Debug, Clone, Copy)]
struct Cell {
    base: char,
    /// Drawn two cells wide: a VS16 sets it, a VS15 clears it, and only a wide cell takes a
    /// skin-tone modifier.
    wide: bool,
    /// The regional indicators stored in it, its base's included: a flag is two.
    flag: u8,
    /// The characters stored after its base.
    joined: u8,
    /// The last character stored is a [`ZWJ`].
    ends_joined: bool,
}

impl Cell {
    fn open(base: char, wide: bool) -> Self {
        Self {
            base,
            wide,
            flag: u8::from(is_regional_indicator(base)),
            joined: 0,
            ends_joined: false,
        }
    }

    /// `c` stored after the base, up to [`MAX_JOINED`] of them.
    fn store(&mut self, c: char) {
        if self.joined < MAX_JOINED {
            self.joined += 1;
            self.ends_joined = c == ZWJ;
            self.flag = self.flag.saturating_add(u8::from(is_regional_indicator(c)));
        }
    }

    /// Whether the grid folds printing character `c` into this cell rather than open one: a
    /// skin-tone modifier onto a wide modifier base, a flag's second regional indicator onto its
    /// first, and any character but ASCII after a stored [`ZWJ`].
    fn folds(&self, c: char) -> bool {
        (is_skin_tone(c) && self.wide && is_modifier_base(self.base))
            || (is_regional_indicator(c) && is_regional_indicator(self.base) && self.flag < 2)
            || (self.ends_joined && !c.is_ascii())
    }
}

/// THE GRID'S LAYOUT OF A ROW: the cells aterm's grid lays a row out in, fed the row's text as the
/// grid reads it back (aterm-core's `Terminal::row_text`, the text the server's `screen_rows`
/// hands the readers: each cell's base and then the characters it stores, a two-cell character's
/// second column skipped and an empty cell a space) one character at a time from its first
/// column. It is the walk `aterm_grapheme::GridColumns` models the grid's write path by (East Asian
/// Ambiguous characters one cell wide, the grid's default), which aterm-core's
/// `grid_columns_agree_with_what_the_grid_paints` holds to the painted grid: a character of no
/// width of its own ([`ZERO`]) is stored in the cell before it, a VS16 widens a one-cell emoji's
/// cell ([`VS16_WIDENS`]: `❤️` is two cells), and the grid folds into the cell before it a
/// skin-tone modifier after a wide base, a flag's second half and any character but ASCII after a
/// joiner, so `✅️`, `👍🏽`, `🇯🇵` and `👨‍👩‍👧` are each two cells and `é` written with a
/// combining mark one. The one place the text read back is not the text written is a VS15 after a
/// two-cell character: the grid narrows the glyph to its first column and empties the second, its
/// cursor staying past both, and reads that emptied cell back as a space after the VS15 (any
/// character the row then stores there follows that space), so here a VS15 narrows the cell and
/// steps back to its second column, which that space opens again: `✅︎` and the space read back
/// after it are two cells, as written. Spelled out so the library keeps its one dependency
/// (`Cargo.toml` says why); `a_row_is_counted_in_the_cells_the_grid_lays_it_out_in` holds it to
/// `aterm_grapheme::str_grid_columns` over every code point, and
/// `a_row_the_grid_reads_back_is_counted_in_the_cells_it_paints` to the columns aterm-core's grid
/// paints, over the text it reads back.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Columns {
    /// The cursor's column: the cells everything fed so far occupies.
    cursor: usize,
    /// The cell a joining character lands in; `None` before the first one.
    cell: Option<Cell>,
}

impl Columns {
    /// The cursor's column: the cells everything fed so far occupies.
    pub(crate) const fn column(&self) -> usize {
        self.cursor
    }

    /// Lay out `c` after everything fed so far: whether it opened a cell of its own (at the
    /// column before it), rather than being stored in or folded into the cell before it.
    pub(crate) fn step(&mut self, c: char) -> bool {
        // Printable ASCII always opens a new one-column cell, including after a joiner.
        // Retain its base so a following variation selector or combining mark still sees it.
        if (' '..='~').contains(&c) {
            self.cell = Some(Cell::open(c, false));
            self.cursor += 1;
            return true;
        }
        let width = cell_width(c);
        match self.cell.as_mut() {
            Some(cell) if width == 0 => {
                cell.store(c);
                if c == VS16 && !cell.wide && in_ranges(&VS16_WIDENS, cell.base) {
                    cell.wide = true;
                    self.cursor += 1;
                } else if c == VS15 && cell.wide {
                    // The glyph narrows to its first column. The grid empties the second, its
                    // cursor staying past it, and reads that cell back as a space, the text's
                    // next character, which opens it again.
                    cell.wide = false;
                    self.cursor -= 1;
                }
                false
            }
            Some(cell) if cell.folds(c) => {
                cell.store(c);
                false
            }
            None if width == 0 => false,
            _ => {
                self.cell = Some(Cell::open(c, width == 2));
                self.cursor += width;
                true
            }
        }
    }
}

/// `s`'s width in cells, laid out from a row's first column as the grid lays it out
/// ([`Columns`]).
pub(crate) fn cells(s: &str) -> usize {
    if s.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        return s.len();
    }
    let mut columns = Columns::default();
    for c in s.chars() {
        columns.step(c);
    }
    columns.column()
}

/// `row` split at cell `col`: before the first character that opens a cell at `col` or past it
/// ([`Columns`]), so what the grid stores in a cell stays with it.
pub(crate) fn split_at_cell(row: &str, col: usize) -> (&str, &str) {
    let mut columns = Columns::default();
    for (i, c) in row.char_indices() {
        let at = columns.column();
        if columns.step(c) && at >= col {
            return (&row[..i], &row[i..]);
        }
    }
    (row, "")
}

/// Row 0 of aterm's grid once `written` is drawn on it from its first column, as the grid reads it
/// back (aterm-core's `Terminal::row_text`, the text the server's `screen_rows` hands the
/// readers): the rows a test constructs, read as the readers are handed them.
#[cfg(test)]
pub(crate) fn read_back(written: &str) -> String {
    let mut term = aterm_core::terminal::Terminal::new(3, 512);
    term.process(written.as_bytes());
    term.row_text(0).expect("row 0")
}

/// The cell `c` first starts at on `row` ([`Columns`]).
fn cell_of(row: &str, c: char) -> Option<usize> {
    let mut columns = Columns::default();
    for x in row.chars() {
        let at = columns.column();
        if columns.step(x) && x == c {
            return Some(at);
        }
    }
    None
}

/// The right-aligned AFK countdown row: `auto-continue in 16s · any key to stay`.
fn afk_row(row: &str) -> bool {
    let t = crate::text::trim(row);
    t.starts_with(const { crate::anchor!("question.auto_continue") })
        && t.ends_with(const { crate::anchor!("question.stay") })
}

/// The REPL's row while a panel holds the queue: `Background task update waiting while
/// this panel is open`, or `<N> background task updates …`.
fn panel_waiting_row(row: &str) -> bool {
    let t = crate::text::trim(row);
    let Some(head) = t.strip_suffix(const { crate::anchor!("dialog.panel_waiting") }) else {
        return false;
    };
    let head = crate::text::trim_end(head);
    head == "Background task update"
        || head
            .strip_suffix(" background task updates")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// One right-aligned, dim plugin notice in a dialog `width` cells wide: at least half the
/// width of leading spaces, and none of a dialog's, a rule's or the composer's glyphs.
fn plugin_notice_row(row: &str, width: usize) -> bool {
    let t = crate::text::trim(row);
    !t.is_empty()
        && leading_spaces(row) >= (width / 2).max(1)
        && !t.contains(['❯', '─', '│', '╭', '╰', '┌', '└'])
        && numbered(t).is_none()
}

/// What sits under a dialog from row `from`.
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

/// The widest row on the screen, in the cells the grid lays it out in ([`cells`]): what the grid
/// stores in a cell with the character before it (a combining mark, a joiner, a variation
/// selector) widens no row, so a row drawn to the pane's edge measures the pane, whatever it holds.
pub(crate) fn screen_width(rows: &[String]) -> usize {
    rows.iter()
        .map(|r| cells(crate::text::trim_end(r)))
        .max()
        .unwrap_or(0)
}

/// A dialog's head over its options: the question's rows and the tab bar
/// (under the top rule).
struct Head {
    question: (usize, usize),
    bar: usize,
}

/// The head of a dialog, walked up from the blank row above its first
/// option (`gap`): the question's rows (all `│ `-led or all bare, at column
/// 0), a blank row, the tab bar, the top rule.
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

/// A question tab over its key-hint footer.
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
    let width = crate::text::trim_end(&rows[divider]).chars().count();
    let afk = trailers(rows, footer + 1, width)?;
    let first_visible = || (0..=footer).find(|&i| !blank(&rows[i])).unwrap_or(footer);
    let cut = || QShape {
        footer: Some(footer),
        afk,
        width,
        ..QShape::unknown(HEAD_CUT, first_visible(), footer, true)
    };
    if divider == 0 {
        return Some(cut());
    }
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
        countdown: None,
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

/// The single- or multi-select options zone above the divider: the contiguous rows up to
/// a blank row, each a numbered row, a row at its label column under one (a description
/// or a wrap: [`label_col`]), or, multi-select, the zone's last row, the button.
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
    let cut_col = (top == 0 && numbered(&rows[0]).is_none()).then(|| leading_spaces(&rows[0]));
    let multi = if texts.is_empty() {
        cut_col.is_some_and(|c| matches!(c, 8 | 9))
            || button_row(&rows[divider - 1]).is_some_and(|(_, focused)| focused)
    } else {
        texts.iter().all(|t| checkbox(t).is_some())
    };
    let mut col = match cut_col {
        Some(c @ (8 | 9)) if multi => c,
        _ if multi => 9,
        _ => 5,
    };
    let mut button = None;
    let mut seen_numbered = false;
    let mut scrolled = false;
    for i in zone.clone() {
        let r = &rows[i];
        if let Some((pointer, _, text)) = numbered(r) {
            seen_numbered = true;
            scrolled |= pointer == Pointer::Scrolled;
            col = label_col(text, multi);
        } else if multi && i + 1 == divider && button_row(r).is_some() {
            button = Some(i);
        } else if leading_spaces(r) == col && (seen_numbered || top == 0) {
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

/// The preview form's zone above the divider, walked up: blank rows; the notes row(s) at
/// the pane's column; blank rows; the option block (numbered rows with the pane `┌─┐│└┘`
/// to their right, rows continuing a label inside its 30-cell column, rows of the pane
/// alone).
fn preview_zone(rows: &[String], divider: usize) -> Option<Walk> {
    let mut i = divider.checked_sub(1)?;
    while blank(&rows[i]) {
        i = i.checked_sub(1)?;
    }
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
        [row] if leading_spaces(row) == pane_col && crate::text::trim(row) == const { crate::anchor!("question.notes") });
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

/// The review (Submit) tab, which draws no footer, read bottom up.
pub(crate) fn review(rows: &[String]) -> Option<QShape> {
    let ready_text = const { crate::anchor!("question.review_ready") };
    if !rows.iter().any(|r| crate::text::trim_end(r) == ready_text)
        || crate::phase::composer_frame(rows).is_some()
    {
        return None;
    }
    let width = screen_width(rows);
    let mut last = rows.len().checked_sub(1)?;
    loop {
        if !blank(&rows[last]) && trailers(rows, last, width).is_none() {
            break;
        }
        last = last.checked_sub(1)?;
    }
    let afk = trailers(rows, last + 1, width)?;
    let (pointer, n, cancel_label) = numbered(&rows[last])?;
    let cancel_label = crate::text::trim_end(cancel_label);
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
                && crate::text::trim_end(label)
                    == const { crate::anchor!("question.review_submit") } => {}
        _ => return None,
    }
    let ready = submit.checked_sub(2)?;
    if !blank(&rows[submit - 1]) || crate::text::trim_end(&rows[ready]) != ready_text {
        return None;
    }
    if rows[ready..=last].iter().any(|r| is_transcript_row(r)) {
        return None;
    }
    let countdown = ready
        .checked_sub(1)
        .filter(|&gap| blank(&rows[gap]))
        .and_then(|gap| (0..=gap).rev().find(|&i| !blank(&rows[i])))
        .and_then(|end| countdown_head(rows, end).map(|head| (head, end + 1)));
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
            countdown,
            pane_col: 0,
            width: rule.map_or(0, |r| crate::text::trim_end(&rows[r]).chars().count()),
            first,
            last,
            head_off_screen: rule.is_none(),
        };
    let mut top = ready;
    let partial = |top: usize| {
        let first = (top..=ready).find(|&i| !blank(&rows[i])).unwrap_or(ready);
        let why = if top == 0 { HEAD_CUT } else { REVIEW_SHAPE };
        (!rows[first..=last].iter().any(|r| is_transcript_row(r)))
            .then(|| shape(QForm::Unknown(why), first, None, None, None))
    };
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
    if let Some((head, _)) = countdown {
        top = head;
        let Some(up) = head.checked_sub(1).filter(|&u| blank(&rows[u])) else {
            return partial(if head == 0 { 0 } else { top });
        };
        match skip_blanks(up) {
            Some(a) => at = a,
            None => return partial(0),
        }
    }
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
    let warning_text = const { crate::anchor!("question.review_unanswered") };
    let warning = rows[at]
        .strip_prefix("⚠ ")
        .is_some_and(|w| crate::text::trim_end(w) == warning_text)
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
    if crate::text::trim_end(&rows[at]) != const { crate::anchor!("question.review_title") } {
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

/// The first row of the auto-deny countdown block whose last row is `at`, if those rows are one.
fn countdown_head(rows: &[String], at: usize) -> Option<usize> {
    let mut i = at;
    loop {
        let r = &rows[i];
        if blank(r) || leading_spaces(r) > 0 {
            return None;
        }
        if let Some(head) = r.strip_prefix('⚠') {
            let text = std::iter::once(head)
                .chain(rows[i + 1..=at].iter().map(String::as_str))
                .map(crate::text::trim)
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            return crate::prompt::after_countdown_head(&text)
                .is_some()
                .then_some(i);
        }
        i = i.checked_sub(1)?;
    }
}

/// A dialog Claude Code withholds while the composer holds a draft.
pub(crate) fn deferred(rows: &[String]) -> Option<QShape> {
    let frame = crate::phase::composer_frame(rows)?;
    let texts = [
        const { crate::anchor!("dialog.deferred_question") },
        const { crate::anchor!("dialog.deferred_suggestion") },
    ];
    for i in (0..rows.len()).rev() {
        if (frame.top..=frame.bottom).contains(&i) {
            continue;
        }
        let r = &rows[i];
        let col = leading_spaces(r);
        let head = crate::text::trim(r);
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
            joined.push_str(crate::text::trim(next));
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

/// The dialog read from `rows` over its shape.
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
        auto_continue: shape.afk.map(|a| crate::text::trim(&rows[a]).to_string()),
        width: shape.width,
    }
}

/// The question's text over its rows.
fn question_text(rows: &[String], (start, end): (usize, usize)) -> QuestionText {
    let gutter = rows[start].starts_with('│');
    let text = rows[start..end]
        .iter()
        .map(|r| {
            let t = crate::text::trim(r);
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
            split_label(rows, h, until, crate::text::trim_end(text), shape.width);
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
    let free_text = std::iter::once(crate::text::trim(free_text))
        .chain(
            rows[free_row + 1..free_end]
                .iter()
                .map(|r| crate::text::trim(r)),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let placeholder = const { crate::anchor!("question.other") };
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

/// Return `form` when exactly one of its rows carries the `❯` focus marker.
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

/// An option's label and description: the option's row and the rows under it to `until`.
fn split_label(
    rows: &[String],
    row: usize,
    until: usize,
    text: &str,
    width: usize,
) -> (String, Range<usize>, String, Recommended) {
    let rec = const { crate::anchor!("question.recommended") };
    let own = format!(" {}", rows[row].chars().skip(1).collect::<String>());
    let mut chain_end = row + 1;
    let mut above = own;
    while chain_end < until && possibly_wrap(&above, &rows[chain_end], width) {
        above.clone_from(&rows[chain_end]);
        chain_end += 1;
    }
    let recommended = if text.ends_with(rec) {
        Recommended::Label
    } else if let Some(k) =
        (row + 1..chain_end).find(|&k| crate::text::trim_end(&rows[k]).ends_with(rec))
    {
        Recommended::Wrapped { label_end: k }
    } else {
        Recommended::No
    };
    let label_end = match recommended {
        Recommended::Wrapped { label_end } => label_end + 1,
        _ => row + 1,
    };
    let label = std::iter::once(crate::text::trim(text))
        .chain(
            rows[row + 1..label_end]
                .iter()
                .map(|r| crate::text::trim(r)),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let description = rows[label_end..until]
        .iter()
        .map(|r| crate::text::trim(r))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (label, row..label_end, description, recommended)
}

/// The preview form's options: each numbered row's cells left of the
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
                label: crate::text::trim(text).to_string(),
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
            last.label.push_str(crate::text::trim(left));
            last.label_rows.end = i + 1;
        }
        i += 1;
    }
    if options.is_empty() {
        return Err(NUMBERING);
    }
    let rec = const { crate::anchor!("question.recommended") };
    for o in &mut options {
        if let Some(l) = o.label.strip_suffix(" ✔") {
            o.label = crate::text::trim_end(l).to_string();
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
