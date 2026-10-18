// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's rating cards and the block they are parked in.
//!
//! Every card of the family (the session survey, the post-compact and long-context
//! surveys, the plugin survey, the memory survey) is drawn by ONE component (`VM` in
//! 2.1.284) above the composer's top rule:
//!
//! ```text
//! ● <question> (optional)            <- `●` in column 0, then the message at column 2
//!   [more message rows]              <- plugin: `Used this session: …`; memory: the cited
//!   1: Bad    2: Fine   3: Good   0: Dismiss     <- the options, two columns in
//! ```
//!
//! The options row is the contract: `0: Dismiss` is the key that closes the card and is
//! the same in every kind. The question row is the vendor's copy and differs by kind, so
//! the block is found from the options row up, never from a question text.
//!
//! **IN A NARROW PANE** (MEASURED on 2.1.300, the harness sweep of 2026-10-15,
//! ATERM-KI-1015-1 S2R-F2; the fixtures `claude-2.1.300-survey-session-NNcol.txt`): below
//! 41 columns Ink wraps the question onto a row at column 2 (`● How is Claude doing this`
//! over `session? (optional)`), and draws the options as four boxes side by side, each
//! key at its box's column and each box's text wrapped inside it (`  1: Bad  2: Fine 3:
//! Good 0:` over `Dismiss` under its `0:` at 34; `  1: Bad2:    3:    0: Dis` over
//! `        Fine  Good  miss` at 26, `Dismiss` broken where its box ends). Read a row at
//! a time no row was the options row, so the card was never dismissed and its rows were
//! read as the worker's last words: the harness typed `keep going` into the standing
//! card, and a card whose question wrapped too hid a live spinner above it, so a running
//! turn read idle (five boxes on the plugin and memory cards, `4: Unsure` among them,
//! wrap the same way). The options are read as that one row of boxes
//! ([`options_line`], [`crate::wrap::rejoined_cells`]) and the question as the line Ink
//! wrapped ([`crate::wrap`]): one card, its kind named by its whole question.

use crate::phase::{
    composer_frame, is_against_right_edge, is_glyph_row, is_rule, is_transcript_row,
};

/// Which card a block is, by its question row. `Unknown` is a structurally valid card
/// whose question this crate has no words for: read through like any other, dismissed
/// when it carries the vendor's `(optional)` mark ([`SurveyBlock::dismissable`]), and
/// said (`kind=unknown`) either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurveyKind {
    /// `How is Claude doing this session?` — also the post-compact and the generic
    /// long-context surveys, which draw the same rows.
    Session,
    /// `How well is Claude following the instructions you gave earlier …`
    LongContext,
    /// `How helpful has the <plugin> plugin been?`
    Plugin,
    /// `Claude recalled a memory:` / `How well did Claude use its memory?`
    Memory,
    Unknown,
}

impl SurveyKind {
    /// The word the journal prints (`kind=<word>`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SurveyKind::Session => "session",
            SurveyKind::LongContext => "long-context",
            SurveyKind::Plugin => "plugin",
            SurveyKind::Memory => "memory",
            SurveyKind::Unknown => "unknown",
        }
    }
}

/// An open rating card, as read from one screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurveyBlock {
    pub kind: SurveyKind,
    /// The `●` question row.
    pub head: usize,
    /// The options row (`1: Bad … 0: Dismiss`), the block's last row; where Ink
    /// wrapped the options' boxes, the last row of those boxes ([`options_line`]).
    pub options: usize,
    /// The `key if=` guard for a row of the card as observed: Claude Code's head row,
    /// `^●.` and its first words; agy's options row whole ([`crate::agy::survey`]) —
    /// spaces as `.`, every metacharacter escaped. Empty when the row holds anything a
    /// wire token cannot carry — the caller then has no guard and presses nothing.
    pub guard: String,
    /// Whether the loop may press its `0`: a card of a KNOWN kind, or a card of a shape
    /// this crate has no words for that carries the vendor's `(optional)` mark. The
    /// readers step over ANY card of the shape (a worker must never read idle under one);
    /// only a press needs to be sure it is one.
    pub dismissable: bool,
}

impl SurveyBlock {
    /// Why a loop must NOT press this card's `0`, or `None` when it may: the readers step
    /// over any card of the shape, but a press wants to be sure — a card of a kind this
    /// crate has no words for that carries no `(optional)` mark, or a question row no
    /// guard can fence, is said and left to a person or a manager.
    #[must_use]
    pub fn withheld(&self) -> Option<&'static str> {
        if !self.dismissable {
            Some("an unrecognised card without the vendor's (optional) mark")
        } else if self.guard.is_empty() {
            Some("its question row cannot be guarded")
        } else {
            None
        }
    }
}

/// How far above the options row the question row may sit: the memory card is the
/// tallest (`●` row, a blank, up to four rows of the cited sentence, a blank, the
/// recollection row) and its sentence runs on when Claude is verbose.
const MAX_ABOVE_OPTIONS: usize = 10;

/// The options row: two columns in, `1: <label>` first, `0: Dismiss` on it (a banner
/// may share the row, as it may in any other).
fn is_options_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  1: ") else {
        return false;
    };
    !rest.starts_with(' ') && rest.contains("0: Dismiss")
}

/// The most rows the options' boxes are read across (module header, "IN A NARROW
/// PANE"): two measured, from 26 to 40 columns; room for a narrower pane's third and
/// fourth.
const OPTION_ROWS: usize = 4;

/// The cells the options' keys start at on the options' first row — `1:` at column 2
/// first, then each `<digit>:` after it, whatever ran into it (`  1: Bad2:    3:` at 26
/// columns): the columns of the boxes Ink lays the options out in. `None` when the row
/// does not open `  1: ` two columns in.
fn option_keys(row: &str) -> Option<Vec<usize>> {
    let rest = row.strip_prefix("  1:")?;
    if rest.starts_with(' ') && rest[1..].starts_with(' ') {
        return None;
    }
    let chars: Vec<char> = row.chars().collect();
    let mut starts = Vec::new();
    let mut cell = 0;
    for (i, &c) in chars.iter().enumerate() {
        let key = c.is_ascii_digit()
            && chars.get(i + 1) == Some(&':')
            && (i == 0 || !chars[i - 1].is_ascii_digit());
        if key {
            starts.push(cell);
        }
        cell += crate::question::cells(c.encode_utf8(&mut [0; 4]));
    }
    (starts.first() == Some(&2)).then_some(starts)
}

/// The options read from row `first` of `rows`, `until` excluded, in a pane `width`
/// cells wide: its last row. One row ([`is_options_row`]), as every wide pane draws it;
/// else the row of boxes Ink wrapped (module header, "IN A NARROW PANE"), read across
/// the rows under it that are its boxes' own ([`crate::wrap::rejoined_cells`]) and whole
/// only when its first box is `1: <label>` and its last exactly `0: Dismiss` — the most
/// such rows, at most [`OPTION_ROWS`]. `None` when neither reads.
fn options_line(rows: &[String], first: usize, until: usize, width: usize) -> Option<usize> {
    if is_options_row(&rows[first]) {
        return Some(first);
    }
    let starts = option_keys(&rows[first])?;
    let whole = |boxes: Vec<String>| {
        boxes.first().is_some_and(|b| b.starts_with("1: "))
            && boxes.last().is_some_and(|b| b == "0: Dismiss")
    };
    (first + 1..until.min(first + OPTION_ROWS))
        .take_while(|&last| !crate::text::trim(&rows[last]).is_empty())
        .filter(|&last| {
            crate::wrap::rejoined_cells(&rows[first..=last], &starts, width).is_some_and(whole)
        })
        .last()
}

/// A row Claude Code parks between the transcript and an idle composer that is not
/// part of the card: blank, a hint or banner against the right edge, a `⎿  Tip:` row.
fn is_parked(row: &str, width: usize) -> bool {
    let t = crate::text::trim_start(row);
    t.is_empty()
        || is_against_right_edge(row, width)
        || t.strip_prefix('⎿')
            .is_some_and(|g| crate::text::trim_start(g).starts_with("Tip:"))
}

/// The open rating card above the composer, if there is one: its options ([`options_line`],
/// one row or the row of boxes a narrow pane wraps) with nothing between them and the
/// composer's top rule but parked rows, a `●` question row in column 0 within
/// [`MAX_ABOVE_OPTIONS`] rows above them, and only blank or indented rows between the
/// two. Anything else — a copy quoted in a tool's output (`⎿`), in the worker's message,
/// a question with no options, the options with words after them — is not open.
#[must_use]
pub fn survey_block(rows: &[String]) -> Option<SurveyBlock> {
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    // Walking up from the rule: the options' first row, under which every row down to the
    // rule is the options' own or parked. A box's wrapped row is passed over as a parked
    // hint: it ends against the pane's right edge, for the last box's label (`0:
    // Dismiss`) is the longest, so the boxes shrink it first and it wraps onto every row
    // any box does (`Dismiss` alone at 34 and 40 columns, `Fine  Good  miss` at 26).
    let (first, options) = (1..frame.top).rev().find_map(|i| {
        if let Some(last) = options_line(rows, i, frame.top, width)
            && rows[last + 1..frame.top]
                .iter()
                .all(|r| is_parked(r, width))
        {
            Some(Some((i, last)))
        } else if is_parked(&rows[i], width) {
            None
        } else {
            Some(None)
        }
    })??;
    let head = (first.saturating_sub(MAX_ABOVE_OPTIONS)..first)
        .rev()
        .find_map(|i| {
            let row = &rows[i];
            if row.starts_with('●') {
                Some(Some(i))
            } else if crate::text::trim(row).is_empty()
                || (row.starts_with("  ") && !row.starts_with("   "))
            {
                // A body row: the message continues at column 2 (a blank is the
                // memory card's paragraph break).
                None
            } else {
                Some(None)
            }
        })??;
    // The question is one line, which a narrow pane wraps onto the rows under it at
    // column 2 ([`crate::wrap`]): its kind is read whole, its guard from the row the
    // guard is matched on.
    let row = rows[head].trim_start_matches('●');
    let col = 1 + crate::phase::leading_spaces(row);
    let end = crate::wrap::line_end(rows, head, crate::text::trim(row), col, width, first);
    let line = crate::wrap::rejoined(&rows[head..=end]);
    let kind = kind_of(line.trim_start_matches('●').trim());
    let question = row.trim();
    // A future card's words are not known; its shape is — and `(optional)` is the
    // vendor's mark on every one of them: what a PRESS wants before it writes a `0`.
    let dismissable = kind != SurveyKind::Unknown
        || rows[head..=options]
            .iter()
            .any(|r| r.contains("(optional)"));
    Some(SurveyBlock {
        kind,
        head,
        options,
        guard: guard_of(question),
        dismissable,
    })
}

fn kind_of(question: &str) -> SurveyKind {
    use crate::anchors::anchor_text as a;
    if question.contains(a("survey.question")) {
        SurveyKind::Session
    } else if question.starts_with(a("survey.long_context")) {
        SurveyKind::LongContext
    } else if question.starts_with(a("survey.plugin")) {
        SurveyKind::Plugin
    } else if question.starts_with(a("survey.memory"))
        || question.starts_with(a("survey.memory_plain"))
    {
        SurveyKind::Memory
    } else {
        SurveyKind::Unknown
    }
}

/// `^●.` and the first four words of `question`, as a `key if=` token. Each word must
/// be plain text (letters, digits and the punctuation a question uses); the guard ends
/// before the first word that is not, and is empty under three words.
fn guard_of(question: &str) -> String {
    let words: Vec<&str> = question
        .split_whitespace()
        .take(4)
        .take_while(|w| {
            w.chars().count() <= 24
                && w.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '’' | ',' | ':' | '?' | '-'))
        })
        .collect();
    // Under three words a prefix is too loose to fence a press (and an ASCII `'` ends the
    // guard: the printed command is single-quoted for the manager's shell).
    if words.len() < 3 {
        return String::new();
    }
    format!("^●.{}", crate::anchors::guard_regex(&words.join(" ")))
}

/// The Claude-drafted-feedback card (`✻ Bug report drafted: <title>` over its preview and
/// `1 to review · 2 to send · 0 to dismiss`): parked in the same slot, NOT a survey — its
/// `0` leads to a "turn off" question whose second `0` writes a setting — so it is only
/// ever SKIPPED by the readers, never pressed. `(first, last)` rows.
///
/// The card is drawn at column 0 (`Cle`: a column box with `marginTop: 1`, its head, its
/// preview and its hint as children, no indent of its own), and so is what tells it from
/// a quote of it: the worker's messages continue at column 2, so a copy in one — the hint
/// row, the `✻` head row — never starts where the card's rows do. The head is the nearest
/// `✻ … drafted:` row above the hint within [`MAX_DRAFTS_ROWS`], with only blank rows and
/// rows that are not the start of a block of the transcript or the live zone between
/// (the preview may hold blank rows of its own).
#[must_use]
pub fn drafts_card(rows: &[String]) -> Option<(usize, usize)> {
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let hint = (1..frame.top).rev().find_map(|i| {
        if is_drafts_hint(&rows[i]) {
            Some(Some(i))
        } else if is_parked(&rows[i], width) {
            None
        } else {
            Some(None)
        }
    })??;
    let drafted = const { crate::anchors::anchor_text("card.drafted") };
    let head = (hint.saturating_sub(MAX_DRAFTS_ROWS)..hint)
        .rev()
        .find_map(|i| {
            let row = &rows[i];
            if row.starts_with('✻') && row.contains(drafted) {
                Some(Some(i))
            } else if crate::text::trim(row).is_empty() {
                None
            } else if is_glyph_row(row) || is_transcript_row(row) || is_rule(row) {
                // Another block starts here: the card has no head above its hint.
                Some(None)
            } else {
                None
            }
        })??;
    Some((head, hint))
}

/// How far above the drafts card's hint its head may sit: the head, up to six preview
/// rows (`maxRows`), and blank rows the preview may hold.
const MAX_DRAFTS_ROWS: usize = 12;

/// The hint row, at column 0 (trailing space aside): `1 to review · 2 to send · 0 to
/// dismiss`, then ` · +N more queued` when more are queued.
fn is_drafts_hint(row: &str) -> bool {
    crate::text::trim_end(row)
        .strip_prefix(const { crate::anchors::anchor_text("card.drafts_hint") })
        .is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix(" · +")
                    .and_then(|r| r.strip_suffix(" more queued"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// The prompts drawn in the rating card's slot that are NOT ratings, with the same
/// `<key>: <label>` cells and a `Don't ask again` choice: the transcript-share ask that
/// follows a rating (`⏺ Can Anthropic look at your session transcript …?`, the docs URL
/// at column 2, `  y: Yes    n: No     d: Don't ask again`; `Ele` in 2.1.284) and the
/// web-setup offer (`⏺ Pushed to GitHub. Connect GitHub to claude.ai …?` over `  1: Yes,
/// run /web-setup   2: Not now   3: Don't ask again`). Like the drafts card they are only
/// ever SKIPPED by the readers — a busy worker must not read idle under one, and nothing
/// presses a key on them (a `0` there is text, and a digit is a real answer). `(first,
/// last)` rows.
///
/// Found from the options row up, as [`survey_block`]: the row two columns in, `<key>: `
/// first and the `Don't ask again` label on it, with only parked rows between it and the
/// composer's rule; then a `⏺` (`●` where the platform draws that) head in column 0 within
/// [`MAX_ASK_ROWS`], only column-2 rows between, and a question mark ending a row of the
/// block. The question's words are not matched, so a reworded ask is still stepped over;
/// the shape is what tells it from the worker's own message, whose lead-in to a quote of
/// the options row (`⏺ The ask reads:`) asks nothing.
#[must_use]
pub fn ask_card(rows: &[String]) -> Option<(usize, usize)> {
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let options = (1..frame.top).rev().find_map(|i| {
        if is_ask_options_row(&rows[i]) {
            Some(Some(i))
        } else if is_parked(&rows[i], width) {
            None
        } else {
            Some(None)
        }
    })??;
    let head = (options.saturating_sub(MAX_ASK_ROWS)..options)
        .rev()
        .find_map(|i| {
            let row = &rows[i];
            if row.starts_with(['⏺', '●']) {
                Some(Some(i))
            } else if row.starts_with("  ")
                && !row.starts_with("   ")
                && !crate::text::trim(row).is_empty()
            {
                // The docs URL, or the row a long question wraps to: column 2. (No blank
                // row lies inside the card — it is a stack of rows — so a message, a
                // blank, and a row that only looks like the options is not an ask.)
                None
            } else {
                Some(None)
            }
        })??;
    // Both prompts are QUESTIONS (`… Claude Code?`, `… offline?`; a narrow terminal wraps
    // the question, so any row of the block will do): the worker's own lead-in to a quote
    // of the options row (`⏺ The ask reads:`) is no head of one.
    rows[head..options]
        .iter()
        .any(|r| crate::text::trim_end(r).ends_with('?'))
        .then_some((head, options))
}

/// How far above the ask's options row its head may sit: a docs-URL row and the rows a
/// long question wraps to.
const MAX_ASK_ROWS: usize = 6;

/// `  <key>: <label> …` two columns in, with the `Don't ask again` choice on it.
fn is_ask_options_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  ") else {
        return false;
    };
    let mut cells = rest.chars();
    matches!(
        (cells.next(), cells.next(), cells.next()),
        (Some(key), Some(':'), Some(' ')) if key.is_ascii_alphanumeric()
    ) && rest.contains(const { crate::anchors::anchor_text("card.dont_ask") })
}

/// The labels a rating names itself by (`QVe` in 2.1.286): the card's buttons, and the
/// word the pending step echoes.
const RATING_LABELS: [&str; 4] = ["Bad", "Fine", "Good", "Unsure"];

/// `Feedback: <label> · Esc to undo`, in column 0: the rating's pending step, drawn for
/// 3 s after a person's `1`/`2`/`3` (the key word is their `escape` binding).
fn is_pending_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix(const { crate::anchors::anchor_text("survey.pending") })
    else {
        return false;
    };
    let undo = const { crate::anchors::anchor_text("survey.pending_undo") }
        .split_once(' ')
        .map_or("", |(_, tail)| tail);
    rest.split_once(" · ").is_some_and(|(label, hint)| {
        RATING_LABELS.contains(&label) && crate::text::trim_end(hint).ends_with(undo)
    })
}

/// `Thanks for the feedback!` alone on its row, in column 0.
fn is_thanks_row(row: &str) -> bool {
    crate::text::trim_end(row) == const { crate::anchors::anchor_text("survey.thanks") }
}

/// The row under the thanks: `(Optional) Press [1] to tell us what went wrong · /feedback`,
/// or, where the follow-up is off, `Use /feedback to share detailed feedback anytime.`
fn is_thanks_second_row(row: &str) -> bool {
    let row = crate::text::trim_end(row);
    (row.starts_with(const { crate::anchors::anchor_text("survey.optional_press") })
        && row.ends_with(" · /feedback"))
        || row == const { crate::anchors::anchor_text("survey.feedback_anytime") }
}

/// The rating's own aftermath, parked in the card's slot above the composer: the pending
/// step (`Feedback: Bad · Esc to undo`, 3 s) and then the thanks (`Thanks for the
/// feedback!`, with the row of the follow-up offer under it, 5 s). Not a card — nothing
/// presses a key on it — but not what the worker said either: read as the last words
/// they stood in the journal's `EVENT idle` (measured on Claude Code 2.1.286, 3 of 3).
/// Found from the composer's rule up like the cards, only parked rows between; drawn at
/// column 0, where the worker's own messages never are. `(first, last)` rows.
#[must_use]
pub(crate) fn rating_aftermath(rows: &[String]) -> Option<(usize, usize)> {
    let frame = composer_frame(rows)?;
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let last = (1..frame.top)
        .rev()
        .find(|&i| !is_parked(&rows[i], width))?;
    let row = rows[last].as_str();
    if is_pending_row(row) || is_thanks_row(row) {
        Some((last, last))
    } else if is_thanks_second_row(row) && is_thanks_row(&rows[last - 1]) {
        Some((last - 1, last))
    } else {
        None
    }
}

/// The rows the readers step over as ONE parked block: the open rating card
/// ([`survey_block`]), the drafts card ([`drafts_card`]), an ask card
/// ([`ask_card`]) or the rating's own aftermath ([`rating_aftermath`]).
/// `(first, last)`, inclusive.
#[must_use]
pub(crate) fn parked_span(rows: &[String]) -> Option<(usize, usize)> {
    survey_block(rows)
        .map(|b| (b.head, b.options))
        .or_else(|| drafts_card(rows))
        .or_else(|| ask_card(rows))
        .or_else(|| rating_aftermath(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::{
        Phase, busy_signal, last_said_row, survey_open, transcript_end, worker_phase,
    };

    const RULE: &str =
        "──────────────────────────────────────────────────────────────────────────────────";
    const FOOTER: &str = "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents";

    fn r(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| (*s).to_string()).collect()
    }

    /// A screen: `above` rows, the composer frame under them.
    fn screen(above: &[&str]) -> Vec<String> {
        let mut v = r(above);
        v.extend(r(&[RULE, "❯", RULE, FOOTER]));
        v
    }

    const SESSION: [&str; 3] = [
        "",
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];
    const LONG: [&str; 3] = [
        "",
        "● How well is Claude following the instructions you gave earlier in this conversation? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];
    const PLUGIN: [&str; 4] = [
        "",
        "● How helpful has the gh plugin been? (optional)",
        "  Used this session: the skill, the command",
        "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
    ];
    const MEMORY: [&str; 8] = [
        "",
        "● Claude recalled a memory:",
        "",
        "  The owner prefers small commits and reviews each diff before it lands.",
        "",
        "  How was Claude's recollection? (optional)",
        "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
        "",
    ];
    const MEMORY_PLAIN: [&str; 3] = [
        "",
        "● How well did Claude use its memory? (optional)",
        "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
    ];
    const DRAFTS: [&str; 6] = [
        "",
        "✻ Bug report drafted: the stand-in title of a queued draft",
        "│ details preview line one",
        "│ details preview line two",
        "1 to review · 2 to send · 0 to dismiss · +1 more queued",
        "",
    ];

    /// The transcript-share ask and the web-setup offer (`Ele`, `kde` in 2.1.284).
    const ASK: [&str; 4] = [
        "",
        "⏺ Can Anthropic look at your session transcript to help us improve Claude Code?",
        "  https://code.claude.com/docs/en/data-usage#session-quality-surveys",
        "  y: Yes    n: No     d: Don't ask again",
    ];
    const WEB: [&str; 3] = [
        "",
        "⏺ Pushed to GitHub. Connect GitHub to claude.ai so you can work on this repo even when this machine is offline?",
        "  1: Yes, run /web-setup   2: Not now   3: Don't ask again",
    ];

    fn with(body: &[&str], card: &[&str]) -> Vec<String> {
        let mut above = body.to_vec();
        above.extend_from_slice(card);
        screen(&above)
    }

    const DONE: [&str; 3] = [
        "⏺ Merged and pushed.",
        "",
        "✻ Cooked for 52m 54s · done 12:31 PM",
    ];
    const WAITING: [&str; 3] = [
        "⏺ Workflow(Second review pass)",
        "  ⎿  Allowed by auto mode classifier",
        "✻ Waiting for 1 dynamic workflow to finish",
    ];
    const SPINNER: [&str; 3] = [
        "⏺ Running 1 shell command…",
        "✽ Ebbing… (3m 2s · ↓ 10.8k tokens)",
        "  ⎿  Tip: Use /btw to ask a quick side question",
    ];

    /// No card, recognised or not, hides the status row: a BUSY worker never reads idle.
    #[test]
    fn no_card_turns_a_busy_worker_idle() {
        for card in [
            &SESSION[..],
            &LONG[..],
            &PLUGIN[..],
            &MEMORY[..],
            &MEMORY_PLAIN[..],
        ] {
            for body in [&WAITING[..], &SPINNER[..]] {
                let rows = with(body, card);
                assert!(busy_signal(&rows).is_some(), "{card:?} over {body:?}");
                assert_eq!(worker_phase(&rows), Phase::Busy, "{card:?} over {body:?}");
            }
            let idle = with(&DONE, card);
            assert_eq!(worker_phase(&idle), Phase::Idle);
            assert_eq!(last_said_row(&idle), Some("⏺ Merged and pushed."));
            assert_eq!(transcript_end(&idle), 3, "{card:?}");
        }
        // The drafts card and the ask cards, which are no survey, are read through the
        // same way.
        for card in [&DRAFTS[..], &ASK[..], &WEB[..]] {
            for body in [&WAITING[..], &SPINNER[..]] {
                let rows = with(body, card);
                assert_eq!(worker_phase(&rows), Phase::Busy, "{card:?} over {body:?}");
                assert!(!survey_open(&rows), "{card:?} is no survey");
            }
            let idle = with(&DONE, card);
            assert_eq!(worker_phase(&idle), Phase::Idle);
            assert_eq!(
                last_said_row(&idle),
                Some("⏺ Merged and pushed."),
                "{card:?}"
            );
            assert_eq!(transcript_end(&idle), 3, "{card:?}");
        }
    }

    /// The hand-built 2.1.284 screens, one per kind (the card's rows from the render
    /// code, the framing from the real captures): each is read whole — the card's kind
    /// and guard, the worker's phase UNDER it, what it last said, where the transcript ends.
    #[test]
    fn every_kind_of_fixture_is_read_whole() {
        use crate::prompt::fixtures::screen;
        for (text, kind, guard, phase) in [
            (
                include_str!("fixtures/claude-2.1.284-survey-session-busy-workflow.txt"),
                Some(SurveyKind::Session),
                "^●.How.is.Claude.doing",
                Phase::Busy,
            ),
            (
                include_str!("fixtures/claude-2.1.284-survey-long-context.txt"),
                Some(SurveyKind::LongContext),
                "^●.How.well.is.Claude",
                Phase::Idle,
            ),
            (
                include_str!("fixtures/claude-2.1.284-survey-long-context-busy-spinner.txt"),
                Some(SurveyKind::LongContext),
                "^●.How.well.is.Claude",
                Phase::Busy,
            ),
            (
                include_str!("fixtures/claude-2.1.284-survey-plugin.txt"),
                Some(SurveyKind::Plugin),
                "^●.How.helpful.has.the",
                Phase::Idle,
            ),
            (
                include_str!("fixtures/claude-2.1.284-survey-memory-cited.txt"),
                Some(SurveyKind::Memory),
                "^●.Claude.recalled.a.memory:",
                Phase::Idle,
            ),
            (
                include_str!("fixtures/claude-2.1.284-survey-memory-plain-busy-workflow.txt"),
                Some(SurveyKind::Memory),
                "^●.How.well.did.Claude",
                Phase::Busy,
            ),
            (
                include_str!("fixtures/claude-2.1.284-drafts-card.txt"),
                None,
                "",
                Phase::Idle,
            ),
            (
                include_str!("fixtures/claude-2.1.284-drafts-card-busy-workflow.txt"),
                None,
                "",
                Phase::Busy,
            ),
            (
                include_str!("fixtures/claude-2.1.284-share-ask-busy-workflow.txt"),
                None,
                "",
                Phase::Busy,
            ),
            (
                include_str!("fixtures/claude-2.1.284-share-ask-idle.txt"),
                None,
                "",
                Phase::Idle,
            ),
            (
                include_str!("fixtures/claude-2.1.284-web-setup-busy-workflow.txt"),
                None,
                "",
                Phase::Busy,
            ),
        ] {
            let rows = screen(text);
            let found = survey_block(&rows);
            assert_eq!(found.as_ref().map(|b| b.kind), kind, "{text:.80}");
            if let Some(b) = found {
                assert_eq!(b.guard, guard);
            } else {
                assert!(
                    drafts_card(&rows).is_some() || ask_card(&rows).is_some(),
                    "the drafts or an ask card: {text:.80}"
                );
            }
            assert_eq!(worker_phase(&rows), phase, "{text:.80}");
            assert_eq!(survey_open(&rows), kind.is_some());
            // The card is never what the worker last said, and the transcript ends above it.
            let said = last_said_row(&rows).expect("a last said row");
            assert!(
                !said.starts_with('●') && !said.contains("Dismiss"),
                "{said}"
            );
            let end = transcript_end(&rows);
            let (first, _) = parked_span(&rows).expect("a parked block");
            assert!(end <= first, "{end} <= {first}");
        }
    }

    /// A person's `1`/`2`/`3` replaces the card with its aftermath — `Feedback: Bad · Esc to
    /// undo` for 3 s, then `Thanks for the feedback!` over the follow-up offer for 5 s
    /// (MEASURED on 2.1.286) — which is parked like the card, never what the worker last
    /// said: with a done row above the walk stops at that, but after an Esc the row above
    /// is the `Interrupted` notice, so the aftermath must not become the last words in
    /// the journal's `EVENT idle`. The Esc'd frames are the measured ones with the done row
    /// replaced by the notice (the vendor's own, as `survey-card-after-esc` shows it).
    /// CONTROLS: the same words indented under the worker's message are its words; a
    /// column-0 row that only starts alike is no aftermath.
    #[test]
    fn the_ratings_aftermath_is_parked_and_never_what_the_worker_said() {
        use crate::prompt::fixtures::screen;
        const REPLY: &str = "⏺ Hi! I am the local lab stub, not a model. (reply 1)";
        const INTERRUPTED: &str = "  ⎿  Interrupted · What should Claude do instead?";
        for (text, span) in [
            (
                include_str!("fixtures/claude-2.1.286-survey-after-1-pending.txt"),
                (11, 11),
            ),
            (
                include_str!("fixtures/claude-2.1.286-survey-after-1-thanks.txt"),
                (11, 12),
            ),
        ] {
            let done = screen(text);
            assert_eq!(rating_aftermath(&done), Some(span), "{text:.90}");
            assert!(!survey_open(&done));
            assert_eq!(worker_phase(&done), Phase::Idle);
            assert_eq!(last_said_row(&done), Some(REPLY));
            let mut esc = done.clone();
            esc[9] = INTERRUPTED.to_string();
            assert_eq!(rating_aftermath(&esc), Some(span));
            assert_eq!(last_said_row(&esc), Some(INTERRUPTED), "{text:.90}");
            assert!(transcript_end(&esc) <= span.0, "{text:.90}");
            // CONTROLS.
            let quoted: Vec<String> = esc
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    if (span.0..=span.1).contains(&i) {
                        format!("  {r}")
                    } else {
                        r.clone()
                    }
                })
                .collect();
            assert_eq!(rating_aftermath(&quoted), None);
            assert_eq!(
                last_said_row(&quoted),
                Some(quoted[span.1].as_str()),
                "{text:.90}"
            );
            let mut other = esc.clone();
            other[span.0] = "Feedback: Great · Esc to undo".to_string();
            assert_eq!(rating_aftermath(&other), None, "{text:.90}");
        }
    }

    #[test]
    fn a_future_card_with_the_same_shape_is_a_card() {
        let rows = with(
            &DONE,
            &[
                "",
                "● How likely are you to recommend this mode? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
        );
        let b = survey_block(&rows).expect("the shape is the contract");
        assert_eq!(b.kind, SurveyKind::Unknown);
        assert_eq!(b.guard, "^●.How.likely.are.you");
        assert!(b.dismissable);
        // Without the vendor's `(optional)` an unknown card is still READ through (a worker
        // must never read idle under one) but is not pressed.
        let plain = with(
            &DONE,
            &[
                "",
                "● How likely are you to recommend this mode?",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
        );
        let b = survey_block(&plain).expect("read through");
        assert!(!b.dismissable);
        assert_eq!(worker_phase(&plain), Phase::Idle);
    }

    #[test]
    fn a_question_the_wire_cannot_carry_gets_no_guard() {
        // An ASCII apostrophe ends the guard (the printed command is single-quoted); under
        // three plain words there is none, and the loop then SAYS the card instead.
        let rows = with(
            &DONE,
            &[
                "",
                "● Claude's recall of a memory (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
        );
        let b = survey_block(&rows).expect("the shape is a card");
        assert_eq!(b.kind, SurveyKind::Unknown);
        assert_eq!(b.guard, "");
        let rows = with(
            &DONE,
            &[
                "",
                "● How was Claude's recall (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ],
        );
        assert_eq!(survey_block(&rows).expect("card").guard, "");
    }

    #[test]
    fn copies_and_lookalikes_are_not_open() {
        for above in [
            // quoted in a tool's output
            r(&[
                "⏺ Bash(cat survey-open.txt)",
                "  ⎿  ● How is Claude doing this session? (optional)",
                "       1: Bad    2: Fine   3: Good   0: Dismiss",
                "",
            ]),
            // quoted by the worker, indented
            r(&[
                "⏺ Claude Code parks this above the composer:",
                "  ● How is Claude doing this session? (optional)",
                "    1: Bad    2: Fine   3: Good   0: Dismiss",
                "",
            ]),
            // the question with no options
            r(&["", "● How is Claude doing this session? (optional)", ""]),
            // the options with no question
            r(&["⏺ done", "", "  1: Bad    2: Fine   3: Good   0: Dismiss"]),
            // the card, then the worker speaks
            r(&[
                "",
                "● How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
                "  I left it for you to answer.",
            ]),
            // a `⏺` message between the question and the options
            r(&[
                "● How is Claude doing this session? (optional)",
                "⏺ text",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ]),
            // the worker's own `⏺` message that reads as the question
            r(&[
                "⏺ How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ]),
            // the card above a done row: history
            r(&[
                SESSION[1],
                SESSION[2],
                "",
                "✻ Cogitated for 4s · done 2:41 PM",
                "",
            ]),
        ] {
            let rows = screen(&above.iter().map(String::as_str).collect::<Vec<_>>());
            assert!(survey_block(&rows).is_none(), "{above:?}");
            assert!(!survey_open(&rows), "{above:?}");
        }
    }

    /// A CARD INK WRAPPED IS STILL THE CARD (module header, "IN A NARROW PANE"; KI
    /// S2R-F2). Under 42 columns the options break inside their boxes (`… 0:` over a
    /// `Dismiss` against the right edge), and read a row at a time the card was none: an
    /// idle card took `keep going`, and a card whose question wrapped too (`● How is
    /// Claude doing this` over `session? (optional)`) hid the spinner above it, so a
    /// running turn read idle; so did a plugin card's, its `4: Unsure` broken as well.
    /// Each is the card, parked whole. NEGATIVE CONTROLS: the same option rows under a
    /// `⏺` lead-in (no `●` head) are no card; a block with no `0:` and `Dismiss` is none;
    /// the worker's words under a whole options row are no wrap of it.
    #[test]
    fn a_card_ink_wrapped_is_still_the_card() {
        let narrow = |above: &[&str], width: usize, footer: &str| {
            let rule = "─".repeat(width);
            let mut v = r(above);
            v.extend(r(&[&rule, "❯", &rule, footer]));
            v
        };
        let idle = narrow(
            &[
                "⏺ Done with the change.",
                "",
                "● How is Claude doing this session?",
                "  (optional)",
                "  1: Bad  2: Fine 3: Good 0:",
                "                          Dismiss",
                "",
            ],
            36,
            "  ⏵⏵ auto mode on (shift+tab to",
        );
        let b = survey_block(&idle).expect("the 36-column card");
        assert_eq!(
            (b.kind, b.head, b.options, b.dismissable),
            (SurveyKind::Session, 2, 5, true)
        );
        assert_eq!(b.guard, "^●.How.is.Claude.doing");
        assert!(survey_open(&idle));
        assert_eq!(last_said_row(&idle), Some("⏺ Done with the change."));

        let busy = narrow(
            &[
                "⏺ Working.",
                "",
                "✶ Deliberating… (12s)",
                "",
                "● How is Claude doing this",
                "  session? (optional)",
                "  1: Bad 2: Fine3: Good0:",
                "                       Dismiss",
                "",
            ],
            30,
            "  ⏵⏵ auto mode on (shift+tab",
        );
        assert!(survey_open(&busy));
        assert_eq!(
            survey_block(&busy).map(|b| b.kind),
            Some(SurveyKind::Session)
        );
        assert_eq!(worker_phase(&busy), Phase::Busy);
        assert_eq!(
            busy_signal(&busy).map(|s| s.to_string()),
            Some("status row: spinner".to_string())
        );

        let plugin = narrow(
            &[
                "⏺ Working.",
                "",
                "✶ Deliberating… (12s)",
                "",
                "● How helpful has the lab plugin been?",
                "  (optional)",
                "  Used this session: lab-tool",
                "  1: Bad  2: Fine 3: Good 4:     0:",
                "                          Unsure Dismiss",
                "",
            ],
            40,
            "  ⏵⏵ auto mode on (shift+tab to cycle)",
        );
        assert_eq!(
            survey_block(&plugin).map(|b| b.kind),
            Some(SurveyKind::Plugin)
        );
        assert_eq!(worker_phase(&plugin), Phase::Busy);

        // NEGATIVE CONTROLS.
        let quoted = narrow(
            &[
                "⏺ The card reads:",
                "  1: Bad  2: Fine 3: Good 0:",
                "                          Dismiss",
                "",
            ],
            36,
            "  ⏵⏵ auto mode on (shift+tab to",
        );
        assert!(survey_block(&quoted).is_none());
        assert!(!survey_open(&quoted));
        let no_dismiss = narrow(
            &[
                "⏺ Done.",
                "",
                "● How is Claude doing this session?",
                "  (optional)",
                "  1: Bad  2: Fine",
                "",
            ],
            36,
            "  ⏵⏵ auto mode on (shift+tab to",
        );
        assert!(survey_block(&no_dismiss).is_none());
        let said_under = narrow(
            &[
                "",
                "● How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
                "  I left it for you to answer.",
            ],
            82,
            FOOTER,
        );
        assert!(survey_block(&said_under).is_none());
    }

    /// A worker QUOTING the drafts card (or an ask) in its message — its rows continue at
    /// column 2, the card's do not — is not the card: it must not park the worker's own
    /// last words away from the report views. (This repo's agents quote these strings
    /// routinely.)
    #[test]
    fn a_quote_of_the_drafts_or_an_ask_card_in_a_message_is_not_the_card() {
        let quoted_drafts = with(
            &DONE[..0],
            &[
                "⏺ The card reads:",
                "",
                "  ✻ Bug report drafted: foo",
                "  │ details",
                "  1 to review · 2 to send · 0 to dismiss",
            ],
        );
        assert!(drafts_card(&quoted_drafts).is_none());
        // The quote is the worker's own words: the last of them is what it last said, and
        // the transcript runs to the end of the message.
        assert_eq!(
            last_said_row(&quoted_drafts),
            Some("  1 to review · 2 to send · 0 to dismiss")
        );
        assert_eq!(transcript_end(&quoted_drafts), 5);
        let quoted_ask = with(
            &DONE[..0],
            &[
                "⏺ The ask reads:",
                "  ⏺ Can Anthropic look at your session transcript to help us improve Claude Code?",
                "    y: Yes    n: No     d: Don't ask again",
            ],
        );
        assert!(ask_card(&quoted_ask).is_none());
        assert_eq!(
            last_said_row(&quoted_ask),
            Some("    y: Yes    n: No     d: Don't ask again")
        );
        // The plainest quote — a short message whose last row is the options row, as the
        // worker's text continues at column 2 like the card's own — asks nothing: no row
        // of it is a question.
        let plain = with(
            &DONE[..0],
            &[
                "⏺ The ask reads:",
                "  y: Yes    n: No     d: Don't ask again",
            ],
        );
        assert!(ask_card(&plain).is_none());
        assert_eq!(
            last_said_row(&plain),
            Some("  y: Yes    n: No     d: Don't ask again")
        );
        assert_eq!(transcript_end(&plain), 2);
        // …and the same rows drawn as the card draws them (column 0 head and hint) are the
        // card: the two differ by the indent alone.
        let card = with(
            &DONE[..0],
            &[
                "⏺ The card reads:",
                "",
                "✻ Bug report drafted: foo",
                "│ details",
                "1 to review · 2 to send · 0 to dismiss",
            ],
        );
        assert!(drafts_card(&card).is_some());
    }

    /// An ask card is its options row (`<key>: …` two columns in, `Don't ask again` on it)
    /// under a `⏺` head in column 0, and nothing else.
    #[test]
    fn an_ask_card_needs_its_options_and_its_head() {
        assert!(ask_card(&with(&DONE, &ASK)).is_some());
        assert!(ask_card(&with(&DONE, &WEB)).is_some());
        // Options with no head (a stray row of a tool's output), a head that is no
        // transcript glyph, the label alone, and rating cards are not asks.
        for above in [
            r(&["⏺ done", "", "  y: Yes    n: No     d: Don't ask again"]),
            r(&[
                "",
                "Can Anthropic look?",
                "  y: Yes    n: No     d: Don't ask again",
            ]),
            r(&["", "⏺ Ask?", "  Don't ask again"]),
            r(&["", "⏺ Ask?", "     y: Yes    n: No     d: Don't ask again"]),
            r(&[
                "",
                "● How is Claude doing this session? (optional)",
                "  1: Bad    2: Fine   3: Good   0: Dismiss",
            ]),
        ] {
            let rows = screen(&above.iter().map(String::as_str).collect::<Vec<_>>());
            assert!(ask_card(&rows).is_none(), "{above:?}");
        }
        // A narrow terminal wraps the question (column 2), and it is still the card.
        let wrapped = with(
            &DONE,
            &[
                "",
                "⏺ Can Anthropic look at your session transcript to help us",
                "  improve Claude Code?",
                "  https://code.claude.com/docs/en/data-usage#session-quality-surveys",
                "  y: Yes    n: No     d: Don't ask again",
            ],
        );
        assert_eq!(
            ask_card(&wrapped).map(|(h, _)| wrapped[h].as_str()),
            Some("⏺ Can Anthropic look at your session transcript to help us")
        );
        // The card is not a survey: nothing reads `survey` off it, nothing presses it.
        let rows = with(&DONE, &ASK);
        assert!(survey_block(&rows).is_none());
        assert!(!survey_open(&rows));
    }

    #[test]
    fn the_drafts_card_needs_its_head_and_its_hint() {
        let rows = with(&DONE, &DRAFTS);
        assert!(drafts_card(&rows).is_some());
        // The hint alone, or a head with another hint, is not the card.
        let no_head = with(&DONE, &["", "1 to review · 2 to send · 0 to dismiss", ""]);
        assert!(drafts_card(&no_head).is_none());
        let other = with(&DONE, &["", "✻ Bug report drafted: x", "Press any key", ""]);
        assert!(drafts_card(&other).is_none());
    }

    /// ATERM-KI-1015-1 S2R-F2 (the harness sweep of 2026-10-15, Claude Code
    /// 2.1.300): in a pane narrower than 41 columns Ink wraps the session
    /// survey's question onto a row at column two and draws its options as
    /// four columns whose words wrap inside each column (`  1: Bad  2: Fine
    /// 3: Good 0:` over `Dismiss` at 34; `  1: Bad2:    3:    0: Dis` over
    /// `        Fine  Good  miss` at 26). Read a row at a time no row was the
    /// options row: the card was never dismissed, its rows were read as the
    /// worker's last words, and the continue policy typed `keep going` into
    /// the standing card. The card is read as one block across those rows.
    #[test]
    fn a_survey_card_ink_wrapped_into_columns_is_one_card() {
        use crate::prompt::fixtures::{
            SURVEY_SESSION_26_2_1_300, SURVEY_SESSION_34_2_1_300, SURVEY_SESSION_40_2_1_300,
            screen as fixture,
        };
        for text in [
            SURVEY_SESSION_26_2_1_300,
            SURVEY_SESSION_34_2_1_300,
            SURVEY_SESSION_40_2_1_300,
        ] {
            let r = fixture(text);
            let block = survey_block(&r).unwrap_or_else(|| panic!("the card: {r:#?}"));
            assert_eq!(block.kind, SurveyKind::Session);
            assert!(r[block.head].starts_with("● How is Claude doing this"));
            // The card's last row is its options' last: the blank row and the
            // composer's top rule under it.
            assert!(r[block.options + 1].trim().is_empty(), "{r:#?}");
            assert!(crate::phase::is_rule(&r[block.options + 2]));
            assert_eq!(block.withheld(), None);
            assert_eq!(block.guard, "^●.How.is.Claude.doing");
            assert!(survey_open(&r));
            assert_eq!(worker_phase(&r), Phase::Idle);
            let said = last_said_row(&r).expect("a last said row");
            assert!(
                said.contains("(reply 1)") || said.ends_with("not a model."),
                "{said:?}"
            );
        }
        // CONTROLS: the boxes' first row alone (its `Dismiss` gone), and a
        // `Dismiss` drawn a column off its box, are no card.
        let r = fixture(SURVEY_SESSION_34_2_1_300);
        let cut: Vec<String> = r
            .iter()
            .map(|x| {
                if x.trim() == "Dismiss" {
                    String::new()
                } else {
                    x.clone()
                }
            })
            .collect();
        assert!(survey_block(&cut).is_none(), "{cut:#?}");
        let off: Vec<String> = r
            .iter()
            .map(|x| {
                if x.trim() == "Dismiss" {
                    format!(" {x}")
                } else {
                    x.clone()
                }
            })
            .collect();
        assert!(survey_block(&off).is_none(), "{off:#?}");
    }
}
