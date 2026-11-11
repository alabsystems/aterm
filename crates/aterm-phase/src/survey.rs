// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's rating cards and the other blocks parked above the composer.

use crate::phase::{
    Transcript, composer_frame, is_against_right_edge, is_glyph_row, is_rule, is_transcript_row,
};

/// Which card a block is, by its question row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurveyKind {
    /// `How is Claude doing this session?` — also the post-compact and the generic long-context surveys, which draw the same rows.
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
    pub const fn name(self) -> &'static str {
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
    /// The options row (`1: Bad … 0: Dismiss`), the block's last row; where Ink wrapped the options' boxes, the last row of those boxes ([`options_line`]).
    pub options: usize,
    /// The `key if=` guard for a row of the card as observed: Claude Code's head row, `^●.` and its first words; agy's options row whole ([`crate::agy::survey`]) — spaces as `.`, every metacharacter escaped.
    pub guard: String,
    /// Whether the loop may press its `0`: a known kind, or an unknown one that carries the vendor's `(optional)` mark.
    pub dismissable: bool,
}

impl SurveyBlock {
    /// Why a loop must not press this card's `0`, or `None` when it may: an unknown kind without the `(optional)` mark, or a question row no guard can fence, is said and left to a person or a manager.
    #[must_use]
    pub const fn withheld(&self) -> Option<&'static str> {
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
/// recollection row), and its sentence runs on when Claude is verbose.
const MAX_ABOVE_OPTIONS: usize = 10;

/// The options row: two columns in, `1: <label>` first, `0: Dismiss` on it (a banner
/// may share the row, as it may in any other).
fn is_options_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix("  1: ") else {
        return false;
    };
    !rest.starts_with(' ') && rest.contains("0: Dismiss")
}

/// The most rows the options' boxes are read across: two measured, from 26 to 40
/// columns, with room for a narrower pane's third and fourth.
const OPTION_ROWS: usize = 4;

/// The cells the options' keys start at on the options' first row (`1:` at column 2
/// first, then each `<digit>:` after it).
fn option_keys(row: &str) -> Option<Vec<usize>> {
    row.strip_prefix("  1:")?;
    let chars: Vec<char> = row.chars().collect();
    let mut starts = Vec::new();
    let mut columns = crate::question::Columns::default();
    for (i, &c) in chars.iter().enumerate() {
        let key = c.is_ascii_digit()
            && chars.get(i + 1) == Some(&':')
            && (i == 0 || !chars[i - 1].is_ascii_digit());
        if key {
            starts.push(columns.column());
        }
        columns.step(c);
    }
    (starts.first() == Some(&2)).then_some(starts)
}

/// The last row of the options that start at row `first` (`until` excluded) in a pane
/// `width` cells wide: `first` itself when it is an options row ([`is_options_row`]);
/// else the row of boxes a narrow pane wraps, read across the rows under it
/// ([`crate::wrap::rejoined_cells`]) and whole only when its first box is `1: <label>`
/// and its last exactly `0: Dismiss`, the most such rows up to [`OPTION_ROWS`].
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

/// The open rating card above the composer: its options ([`options_line`]) with only parked rows between them and the composer's top rule, a `●` question row in column 0 within [`MAX_ABOVE_OPTIONS`] rows above them, and only blank or column-2 rows between the two, each row read on the screen's transcript ([`card_rows`]).
#[must_use]
pub fn survey_block(rows: &[String]) -> Option<SurveyBlock> {
    survey_block_on(&Transcript::of(rows))
}

/// THE ROWS A CARD OVER THE COMPOSER IS READ ON: the screen's transcript ([`Transcript`], each
/// row above the composer's top rule cut at the side panel's rule) and the room those rows are
/// laid out in, the rule's column beside the panel and the composer's width with none. Claude Code
/// 2.1.310 and later draw the panel beside every row down to that top rule, the cards' included,
/// and pad each out to the rule, so read whole a card row ended in its padding and the rule: a
/// blank row under the card was no parked row and its rows were no card's (found in review,
/// 2026-11-05; `a_card_parked_over_the_composer_beside_the_side_panel_is_parked`).
fn card_rows<'t>(
    transcript: &'t Transcript<'_>,
) -> Option<(&'t [String], crate::phase::Frame, usize)> {
    let rows: &[String] = &transcript.rows;
    let frame = composer_frame(rows)?;
    let width = transcript.room(crate::text::trim_end(&rows[frame.bottom]).chars().count());
    Some((rows, frame, width))
}

/// [`survey_block`] on a screen's transcript.
fn survey_block_on(transcript: &Transcript<'_>) -> Option<SurveyBlock> {
    let (rows, frame, width) = card_rows(transcript)?;
    let parked_under = |last: usize| {
        rows[last + 1..frame.top]
            .iter()
            .all(|r| is_parked(r, width))
    };
    let low = (1..frame.top)
        .rev()
        .find(|&i| !is_parked(&rows[i], width))?;
    let (first, options) = (low.saturating_sub(OPTION_ROWS - 1)..=low)
        .rev()
        .find_map(|i| {
            options_line(rows, i, frame.top, width)
                .filter(|&last| last >= low && parked_under(last))
                .map(|last| (i, last))
        })?;
    let head = (first.saturating_sub(MAX_ABOVE_OPTIONS)..first)
        .rev()
        .find_map(|i| {
            let row = &rows[i];
            if row.starts_with('●') {
                Some(Some(i))
            } else if crate::text::trim(row).is_empty()
                || (row.starts_with("  ") && !row.starts_with("   "))
            {
                None
            } else {
                Some(None)
            }
        })??;
    let row = rows[head].trim_start_matches('●');
    let col = 1 + crate::phase::leading_spaces(row);
    let end = crate::wrap::line_end(rows, head, crate::text::trim(row), col, width, first);
    let line = crate::wrap::rejoined(&rows[head..=end]);
    let kind = kind_of(line.trim_start_matches('●').trim());
    let question = row.trim();
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
    if question.contains(crate::anchor!("survey.question")) {
        SurveyKind::Session
    } else if question.starts_with(crate::anchor!("survey.long_context")) {
        SurveyKind::LongContext
    } else if question.starts_with(crate::anchor!("survey.plugin")) {
        SurveyKind::Plugin
    } else if question.starts_with(crate::anchor!("survey.memory"))
        || question.starts_with(crate::anchor!("survey.memory_plain"))
    {
        SurveyKind::Memory
    } else {
        SurveyKind::Unknown
    }
}

/// `^●.` and the first four words of `question`, as a `key if=` token; the guard ends before
/// the first word that is over 24 characters or not plain text, and is empty under three words.
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
    if words.len() < 3 {
        return String::new();
    }
    format!("^●.{}", crate::anchors::guard_regex(&words.join(" ")))
}

/// The Claude-drafted-feedback card (`✻ Bug report drafted: <title>` over its preview and `1 to review · 2 to send · 0 to dismiss`), parked in the same slot, read on the screen's transcript ([`card_rows`]).
#[must_use]
pub fn drafts_card(rows: &[String]) -> Option<(usize, usize)> {
    drafts_card_on(&Transcript::of(rows))
}

/// [`drafts_card`] on a screen's transcript.
fn drafts_card_on(transcript: &Transcript<'_>) -> Option<(usize, usize)> {
    let (rows, frame, width) = card_rows(transcript)?;
    let hint = (1..frame.top).rev().find_map(|i| {
        if is_drafts_hint(&rows[i]) {
            Some(Some(i))
        } else if is_parked(&rows[i], width) {
            None
        } else {
            Some(None)
        }
    })??;
    let drafted = const { crate::anchor!("card.drafted") };
    let head = (hint.saturating_sub(MAX_DRAFTS_ROWS)..hint)
        .rev()
        .find_map(|i| {
            let row = &rows[i];
            if row.starts_with('✻') && row.contains(drafted) {
                Some(Some(i))
            } else if crate::text::trim(row).is_empty() {
                None
            } else if is_glyph_row(row) || is_transcript_row(row) || is_rule(row) {
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
        .strip_prefix(const { crate::anchor!("card.drafts_hint") })
        .is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix(" · +")
                    .and_then(|r| r.strip_suffix(" more queued"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
}

/// The prompts drawn in the rating card's slot that are not ratings, with the same
/// `<key>: <label>` cells and a `Don't ask again` choice: the transcript-share ask that
/// follows a rating (`⏺ Can Anthropic look at your session transcript …?`, the docs URL
/// at column 2, `  y: Yes    n: No     d: Don't ask again`) and the web-setup offer
/// (`⏺ Pushed to GitHub. Connect GitHub to claude.ai …?` over
/// `  1: Yes, run /web-setup   2: Not now   3: Don't ask again`).
/// Each row is read on the screen's transcript ([`card_rows`]).
#[must_use]
pub fn ask_card(rows: &[String]) -> Option<(usize, usize)> {
    ask_card_on(&Transcript::of(rows))
}

/// [`ask_card`] on a screen's transcript.
fn ask_card_on(transcript: &Transcript<'_>) -> Option<(usize, usize)> {
    let (rows, frame, width) = card_rows(transcript)?;
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
                None
            } else {
                Some(None)
            }
        })??;
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
    ) && rest.contains(const { crate::anchor!("card.dont_ask") })
}

/// The labels a rating names itself by: the card's buttons, and the word the pending
/// step echoes.
const RATING_LABELS: [&str; 4] = ["Bad", "Fine", "Good", "Unsure"];

/// `Feedback: <label> · Esc to undo`, in column 0: the rating's pending step, drawn for
/// 3 s after a person's `1`/`2`/`3` (the key word is their `escape` binding).
fn is_pending_row(row: &str) -> bool {
    let Some(rest) = row.strip_prefix(const { crate::anchor!("survey.pending") }) else {
        return false;
    };
    let undo = const { crate::anchor!("survey.pending_undo") }
        .split_once(' ')
        .map_or("", |(_, tail)| tail);
    rest.split_once(" · ").is_some_and(|(label, hint)| {
        RATING_LABELS.contains(&label) && crate::text::trim_end(hint).ends_with(undo)
    })
}

/// `Thanks for the feedback!` alone on its row, in column 0.
fn is_thanks_row(row: &str) -> bool {
    crate::text::trim_end(row) == const { crate::anchor!("survey.thanks") }
}

/// The row under the thanks: `(Optional) Press [1] to tell us what went wrong · /feedback`,
/// or, where the follow-up is off, `Use /feedback to share detailed feedback anytime.`
fn is_thanks_second_row(row: &str) -> bool {
    let row = crate::text::trim_end(row);
    (row.starts_with(const { crate::anchor!("survey.optional_press") })
        && row.ends_with(" · /feedback"))
        || row == const { crate::anchor!("survey.feedback_anytime") }
}

/// The rating's own aftermath, parked in the card's slot above the composer: the pending step (`Feedback: Bad · Esc to undo`, 3 s), then the thanks (`Thanks for the feedback!` over the follow-up offer, 5 s), read on a screen's transcript ([`card_rows`]).
fn rating_aftermath(transcript: &Transcript<'_>) -> Option<(usize, usize)> {
    let (rows, frame, width) = card_rows(transcript)?;
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

/// The rows the readers step over as one parked block: the open rating card ([`survey_block`]), the drafts card ([`drafts_card`]), an ask card ([`ask_card`]) or the rating's own aftermath ([`rating_aftermath`]), each read on the screen's transcript ([`card_rows`]; rows already cut are their own transcript).
#[must_use]
pub(crate) fn parked_span(rows: &[String]) -> Option<(usize, usize)> {
    let transcript = Transcript::of(rows);
    survey_block_on(&transcript)
        .map(|b| (b.head, b.options))
        .or_else(|| drafts_card_on(&transcript))
        .or_else(|| ask_card_on(&transcript))
        .or_else(|| rating_aftermath(&transcript))
}

#[cfg(test)]
#[path = "survey_tests.rs"]
mod tests;
