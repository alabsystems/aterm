// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Kimi Code (`kimi`, Moonshot AI) CLI harness version and screen grammar.
//!
//! Measured on Kimi Code 2.3.1 signed out (`fixtures/kimi-2.3.1-*.txt`): the welcome box, the
//! composer (a rounded box whose first row is `│ > ` and the draft), the `context: N%` footer
//! under it, and the indented `Error: <why>` row a failed turn ends on. The quota and balance
//! texts are the server's words, which 2.3.3's classifier matched a 429 by
//! (`KIMI_QUOTA_EXHAUSTED_MESSAGE_PATTERNS`); 2.5 classifies by code alone and shows the server's
//! message (`[provider.api_error] …`). Its concurrency throttle is a retryable rate limit, not a
//! wall.
//!
//! Its phase is measured on 2.4.0's frames at 110 columns
//! (`docs/measured/kimi-forever-2026-10-29.md`, `fixtures/kimi-2.4.0-*.txt`): a running turn draws
//! one spinner row right over the composer's top border (a moon phase, `🌓` alone, with
//! ` · Tip: …` or a retry's `Retrying (N/10) · …`, or a braille spinner and `Working…`), with a
//! retry's detail or a queued message's block between it and the border; the turn's end takes it
//! away and leaves a blank row there ([`phase`]). An approval, a question, a plan, the trust
//! prompt and the session picker are drawn in the composer's place and wait on the person. The
//! first four are boxes a tool or a launch raised: each is a prompt, read as a box for display
//! ([`prompt`]) and answered by no one here, since Kimi Code is read, not supervised. The session
//! picker is the person's own and decides nothing. Other widths are unmeasured.

use crate::phase::Phase;
use crate::prompt::{Opt, PromptKind, PromptV2, Role, Select};
use crate::wall::{Placement, Wall, WallKind};

/// The Kimi Code (`kimi`) harness version, `MAJOR.MINOR` with no third part.
///
/// Its minor goes up once per public release window in which its sources changed
/// ([`crate::CliHarness::sources`]); `targo --unverified ship harness-bump` writes the bump.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 15, "0.15");

/// The usage limit is spent: the server's quota words 2.3.3's patterns matched, and its code
/// (lowercase).
const QUOTA: [&str; 3] = [
    "exceeded your current quota",
    "exceeded your current token quota",
    "exceeded_current_quota_error",
];

/// The account's balance is spent: the server's balance words 2.3.3's patterns matched
/// (lowercase).
const BALANCE: [&str; 5] = [
    "check your account balance",
    "insufficient balance",
    "recharge your account",
    "please recharge",
    "in arrears",
];

/// The login is gone (lowercase): no model is signed in, or the stored one is refused.
const AUTH: [&str; 4] = [
    "llm not set",
    "re-login required",
    "no token for \"kimi-code\"",
    "send \"/login\" to login",
];

/// A passing throttle (lowercase): a request over the account's concurrency, which Kimi Code
/// retries and which ends no quota.
const THROTTLE: [&str; 1] = ["concurrent request limit"];

/// The composer's rows `(top, caret, bottom)`: the last rounded box whose first inner row opens
/// with `│ >`, with Kimi Code's `context: N%` footer under it ([`footer_used`]).
#[must_use]
pub fn composer_rows(rows: &[String]) -> Option<(usize, usize, usize)> {
    let caret = rows.iter().rposition(|r| caret_text(r).is_some())?;
    let top = caret.checked_sub(1)?;
    if !crate::text::trim_start(&rows[top]).starts_with('╭') {
        return None;
    }
    let bottom = caret
        + 1
        + rows[caret + 1..]
            .iter()
            .position(|r| crate::text::trim_start(r).starts_with('╰'))?;
    rows[bottom + 1..]
        .iter()
        .any(|r| footer_used(r).is_some())
        .then_some((top, caret, bottom))
}

/// The draft text of a composer row: past `│ > ` (the caret row) and before its closing `│`.
fn caret_text(row: &str) -> Option<&str> {
    let inner = crate::text::trim_start(row).strip_prefix('│')?;
    let draft = inner.strip_prefix(" >")?;
    if !(draft.is_empty() || draft.starts_with(' ')) {
        return None;
    }
    let draft = crate::text::trim_end(draft);
    Some(crate::text::trim(draft.strip_suffix('│').unwrap_or(draft)))
}

/// The share of context a footer row says is used: `context: 0%`, or the measured 2.4.1 form
/// with numeric counters, `context: 15% (13.7k/97.7k)`. Other trailing words are not a footer.
#[must_use]
pub fn context_row(row: &str) -> Option<u8> {
    let rest = crate::text::trim(row).strip_prefix(crate::anchor!("kimi.footer.context"))?;
    let (percent, tail) = rest.split_once('%')?;
    if percent.is_empty() || !percent.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !tail.is_empty() {
        let counts = tail.strip_prefix(" (")?.strip_suffix(')')?;
        let (used, total) = counts.split_once('/')?;
        if !context_count(used) || !context_count(total) {
            return None;
        }
    }
    percent.parse().ok()
}

/// The share of context a footer row says is used ([`context_row`]), where the exit hint a first
/// Ctrl-C arms for 1.5 s may stand at the footer row's left before it (`Press Ctrl+C again to
/// exit`, measured on 2.4.0's exit-confirm frame).
fn footer_used(row: &str) -> Option<u8> {
    let text = crate::text::trim(row);
    context_row(
        text.strip_prefix(crate::anchor!("kimi.footer.exit_confirm"))
            .map_or(text, crate::text::trim_start),
    )
}

/// A decimal token count, optionally in compact thousands or millions.
fn context_count(count: &str) -> bool {
    let digits = count
        .strip_suffix('k')
        .or_else(|| count.strip_suffix('M'))
        .unwrap_or(count);
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or_default();
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(fraction), None) => {
            !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    }
}

/// How many of the screen's last drawn rows its `context: N%` footer may stand in when no
/// composer is drawn: it stays at the foot under an approval or question box (measured on
/// 2.4.0's box and question frames), however much blank screen stands under the drawing.
const FOOTER_ROWS: usize = 4;

/// The context the footer says is left: the share its `context: N%` row leaves, read under the
/// composer where one is drawn ([`composer_rows`]), else on a footer row at the screen's foot,
/// where it stands under a box a question or an approval drew in the composer's place. For
/// the window's displays only ([`crate::ScreenReader::context_shown`]).
#[must_use]
pub fn context_left(rows: &[String]) -> Option<u8> {
    let used = match composer_rows(rows) {
        Some((_, _, bottom)) => rows[bottom + 1..].iter().find_map(|r| footer_used(r)),
        None => {
            let drawn = rows
                .iter()
                .rposition(|r| !crate::text::trim(r).is_empty())
                .map_or(0, |last| last + 1);
            rows[..drawn]
                .iter()
                .rev()
                .take(FOOTER_ROWS)
                .find_map(|r| context_row(r))
        }
    }?;
    Some(100u8.saturating_sub(used.min(100)))
}

/// The composer as `(caret row, lines)`, the caret row's draft first ([`composer_rows`]).
#[must_use]
pub fn composer(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let (_, caret, bottom) = composer_rows(rows)?;
    let mut lines = vec![caret_text(&rows[caret]).unwrap_or_default().to_string()];
    for r in &rows[caret + 1..bottom] {
        let inner = crate::text::trim_start(r).strip_prefix('│').unwrap_or(r);
        let inner = crate::text::trim_end(inner);
        lines.push(crate::text::trim(inner.strip_suffix('│').unwrap_or(inner)).to_string());
    }
    Some((caret, lines))
}

/// Whether `row` is Kimi Code's spinner row (measured on 2.4.0's busy frames): a moon phase
/// (U+1F311 to U+1F318) alone or before a space (` · Tip: …`, ` Retrying (6/10) · …`), or a
/// braille spinner's glyph (U+2801 to U+28FF) and `Working…`.
fn spinner_row(row: &str) -> bool {
    let mut chars = crate::text::trim(row).chars();
    match chars.next() {
        Some('\u{1F311}'..='\u{1F318}') => matches!(chars.next(), None | Some(' ')),
        Some('\u{2801}'..='\u{28FF}') => chars
            .as_str()
            .strip_prefix(' ')
            .is_some_and(|rest| rest.starts_with(crate::anchor!("kimi.busy.working"))),
        _ => false,
    }
}

/// Kimi Code's phase on `rows` and whether the screen decides it.
///
/// Only its composer decides it ([`composer_rows`]), by the run of drawn rows standing right over
/// the composer's top border: none (a blank row there, as every measured turn's end leaves) is
/// idle; a run opened by the spinner row ([`spinner_row`]) is busy, whatever stands between the
/// spinner and the border (a retry's detail, a queued message's block); a run ending in the
/// notice a resume draws (`Resumed session (<id>).`, measured on 2.4.0's `resumed-picked` frame)
/// is idle. Any other run decides nothing. A screen with no composer is a prompt where a box
/// stands in its place ([`card`]), and decides nothing otherwise (the session picker, an exit).
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    let Some((top, _, _)) = composer_rows(rows) else {
        return if card(rows).is_some() {
            (Phase::Prompt, true)
        } else {
            (Phase::Idle, false)
        };
    };
    let run = rows[..top]
        .iter()
        .rposition(|r| crate::text::blank(r))
        .map_or(0, |gap| gap + 1);
    match &rows[run..top] {
        [] => (Phase::Idle, true),
        [first, ..] if spinner_row(first) => (Phase::Busy, true),
        [.., last] if crate::text::trim(last).starts_with(crate::anchor!("kimi.resumed")) => {
            (Phase::Idle, true)
        }
        _ => (Phase::Idle, false),
    }
}

/// How few cells a full-width rule ([`rule`]) may hold: the measured ones span the screen
/// (108 at 110 columns), and a word's underline or a short divider in a reply is narrower.
const RULE_MIN: usize = 40;

/// Whether `row` is a full-width rule (` ─…─`), the edge Kimi Code draws over and under a box in
/// the composer's place (measured on 2.4.0's approval, plan, question and trust frames).
fn rule(row: &str) -> bool {
    let text = crate::text::trim(row);
    text.chars().count() >= RULE_MIN && text.chars().all(|c| c == '─')
}

/// The box standing in the composer's place on `rows` ([`card`]): its kind, its two rules and
/// its title row.
#[derive(Debug, Clone, Copy)]
struct Card {
    kind: PromptKind,
    upper: usize,
    title: usize,
    lower: usize,
}

/// The titles a box opens with, after its upper rule and an optional `▶ `, and the kind each
/// names. A command's approval, a plan's, a question and its review step and the folder-trust
/// prompt are measured on 2.4.0, a file's write and its edits on 2.5.2. The approval panel takes
/// its title from the tool it asks about (`headerFor`, read in 2.5.1's and 2.5.2's binaries,
/// which draw the measured ones the same way), stopping a task besides those. Every other tool's
/// is `Approve <tool>?` ([`approval_of`]).
const TITLES: [(&str, PromptKind); 7] = [
    (crate::anchor!("kimi.box.run"), PromptKind::Bash),
    (crate::anchor!("kimi.box.write"), PromptKind::Write),
    (crate::anchor!("kimi.box.edit"), PromptKind::Edit),
    (crate::anchor!("kimi.box.stop"), PromptKind::Tool),
    (crate::anchor!("kimi.box.plan"), PromptKind::PlanExit),
    (crate::anchor!("kimi.box.question"), PromptKind::Question),
    (crate::anchor!("kimi.trust"), PromptKind::Trust),
];

/// Whether `title` is the approval panel's title for a tool `headerFor` names none for:
/// `Approve <tool>?`, the tool's name one word (read in 2.5.1's and 2.5.2's binaries).
fn approval_of(title: &str) -> bool {
    title
        .strip_prefix(crate::anchor!("kimi.box.approve"))
        .and_then(|rest| rest.strip_suffix('?'))
        .is_some_and(|tool| !tool.is_empty() && !tool.contains(char::is_whitespace))
}

/// The title on a box's title row: past an optional `▶ `, and short of the two spaces before the
/// environment an approval may name after it (`renderHead`, read in 2.5.2's binary).
fn titled(row: &str) -> &str {
    let words = crate::text::trim(row);
    let named = words.strip_prefix('▶').map_or(words, crate::text::trim);
    named.split_once("  ").map_or(named, |(title, _)| title)
}

/// The box standing in the composer's place on `rows`: no composer is drawn, a pair of
/// full-width rules ([`rule`]) holds it, the lower one the last drawn row (the trust prompt,
/// which draws no footer) or standing right over the footer's two rows, the screen's last drawn
/// ones, its `context: N%` row the second ([`footer_used`]), and the first drawn row under the
/// upper rule is one of the titles (`TITLES`, [`approval_of`]). The session picker stands on the
/// same rules under its own title and is none.
fn card(rows: &[String]) -> Option<Card> {
    if composer_rows(rows).is_some() {
        return None;
    }
    let last = rows.iter().rposition(|r| !crate::text::blank(r))?;
    let lower = rows[..=last].iter().rposition(|r| rule(r))?;
    let footed = lower == last
        || (last == lower + 2
            && !crate::text::blank(&rows[lower + 1])
            && footer_used(&rows[last]).is_some());
    if !footed {
        return None;
    }
    let upper = rows[..lower].iter().rposition(|r| rule(r))?;
    let title = (upper + 1..lower).find(|&i| !crate::text::blank(&rows[i]))?;
    let named = titled(&rows[title]);
    let kind = TITLES
        .iter()
        .find(|(text, _)| named == *text)
        .map(|&(_, kind)| kind)
        .or_else(|| approval_of(named).then_some(PromptKind::Tool))?;
    Some(Card {
        kind,
        upper,
        title,
        lower,
    })
}

/// An option row of a box whose rules start `edge` columns in: its number, whether the focus is
/// on it and its label. Kimi Code draws the focused option two columns in from the rules, its
/// mark and one space before its number (`▶ 1. Approve once` on an approval or a plan,
/// `→ [1] Red (Recommended)` on a question), and every other option's number two columns further
/// in (`2. Approve for this session`, `[2] Blue`), measured on 2.4.0. A multi-select question's
/// options stand two columns in as `[✓] <label>` or `[ ] <label>`, numbered on no row and focused
/// by colour alone (read in 2.5.2's `renderQuestionTab`): each is read with no number and no
/// focus, and [`prompt`] numbers them. A row at any other column (an option's description, a
/// question's body, a command) is none, and so is a review step's `→  <answer>`, whose mark two
/// spaces follow.
fn choice(row: &str, edge: usize) -> Option<(Option<u8>, bool, &str)> {
    let text = crate::text::trim(row);
    let at = crate::phase::leading_spaces(row);
    if at == edge + 2 {
        if let Some(rest) = text.strip_prefix("▶ ").or_else(|| text.strip_prefix("→ ")) {
            return numbered(rest).map(|(n, label)| (Some(n), true, label));
        }
        return text
            .strip_prefix("[✓] ")
            .or_else(|| text.strip_prefix("[ ] "))
            .map(|label| (None, false, crate::text::trim(label)));
    }
    (at == edge + 4)
        .then(|| numbered(text))
        .flatten()
        .map(|(n, label)| (Some(n), false, label))
}

/// A numbered option's number and label: `1. Approve once`, `[1] Red (Recommended)`.
fn numbered(text: &str) -> Option<(u8, &str)> {
    let (digits, label) = match text.strip_prefix('[') {
        Some(rest) => rest.split_once("] ")?,
        None => text.split_once(". ")?,
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, crate::text::trim(label)))
}

/// The number of the first option a multi-select question shows: one past the hidden ones when
/// its list scrolls, which its `showing a-b of n` row names (read in 2.5.2's
/// `renderQuestionTab`), else 1.
fn first_shown(rows: &[String], from: usize, to: usize) -> u8 {
    (from..to)
        .find_map(|i| {
            let shown = crate::text::trim(&rows[i])
                .strip_prefix(crate::anchor!("kimi.question.showing"))?;
            shown.split_once('-')?.0.parse().ok()
        })
        .unwrap_or(1)
}

/// How many cells `text` may take on the screen: two for each character past ASCII, so a wide
/// character is never counted short.
fn cells(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// A body row's words in a box whose rules start `edge` columns in and span `width` cells, short
/// of the scrollbar a fullscreen panel paints at the box's right edge over a body taller than it
/// (`│` its track, `┃` its thumb, `█` while it is dragged: `paintScrollbar`, read in 2.5.2's
/// binary; unmeasured): the glyph is the row's last, after a space, at the edge.
fn body_words(row: &str, edge: usize, width: usize) -> &str {
    let text = crate::text::trim(row);
    let end = crate::phase::leading_spaces(row) + text.chars().count();
    match text.strip_suffix(['│', '┃', '█']) {
        Some(rest) if end + 1 >= edge + width && rest.ends_with(' ') => crate::text::trim_end(rest),
        _ => text,
    }
}

/// Whether a body row's `words` were cut at the box's edge: the panel ends every body row too
/// wide for it with `...` at its last cells (`truncateToWidth`, read in 2.5.2's binary).
fn cut(row: &str, words: &str, edge: usize, width: usize) -> bool {
    let end = crate::phase::leading_spaces(row) + words.chars().count();
    words.ends_with("...") && end + 2 >= edge + width
}

/// The file a file's approval names, in a box whose rules start `edge` columns in and span
/// `width` cells: its block's first row on `rows` `from..to`, two columns in from the rules, and
/// whether that row shows it whole. A file written whole draws its path there over its numbered
/// content; a file's edits draw a diff headed by the counts it adds and removes before the path
/// (`+1 -1 <path>`, `renderDiffLinesClustered`, read in 2.5.2's binary). Measured on 2.5.2's
/// write and edit frames; a path cut at the box's edge ([`cut`]) names no file.
fn file_of(
    rows: &[String],
    from: usize,
    to: usize,
    edge: usize,
    width: usize,
) -> Option<(&str, bool)> {
    let row = (from..to).find(|&i| !crate::text::blank(&rows[i]))?;
    if crate::phase::leading_spaces(&rows[row]) != edge + 2 {
        return None;
    }
    let words = body_words(&rows[row], edge, width);
    let mut text = words;
    for sign in ['+', '-'] {
        let count = text
            .strip_prefix(sign)
            .and_then(|rest| rest.split_once(' '));
        if let Some((n, rest)) = count
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            text = rest;
        }
    }
    (!text.is_empty()).then(|| (text, !cut(&rows[row], words, edge, width)))
}

/// A command's approval ([`prompt`]): its lines, its description, and whether the row read as
/// its description may instead end its last line.
#[derive(Debug, Default)]
struct Shell {
    lines: Vec<String>,
    description: String,
    unsure: bool,
}

/// The command a box whose rules start `edge` columns in and span `width` cells shows on its
/// rows `from..to` (read in 2.5.2's `renderShellDisplayBlock`, whose shape 2.4.0's approval
/// frame shows). Its first line follows `$ ` and every later one `· `, both two columns in from
/// the rules, a line too long for the box wrapping onto rows two columns further in; the
/// description, when the tool gave one, is the block's last row, at that same depth, never
/// wrapped. That last row is read as the description when the row over it had room for its
/// first word, so it cannot be a wrap; otherwise it may be either, and it is kept for both
/// readings (`unsure`).
fn shell(rows: &[String], from: usize, to: usize, edge: usize, width: usize) -> Option<Shell> {
    let at_line = |i: usize| crate::phase::leading_spaces(&rows[i]) == edge + 2;
    let start =
        (from..to).find(|&i| at_line(i) && crate::text::trim(&rows[i]).starts_with("$ "))?;
    let wrap = width.saturating_sub(4);
    let mut lines: Vec<String> = Vec::new();
    let mut prev_cells = 0;
    let mut tail: Option<(String, bool)> = None;
    for row in &rows[start..to] {
        let text = body_words(row, edge, width);
        if text.is_empty() {
            break;
        }
        let lead = crate::phase::leading_spaces(row);
        let opened = if lead == edge + 2 {
            text.strip_prefix(if lines.is_empty() { "$ " } else { "· " })
        } else {
            None
        };
        if opened.is_none() && lead != edge + 4 {
            break;
        }
        // A deeper row with a row of the block under it is a wrap: only the last can describe.
        if let (Some((held, _)), Some(last)) = (tail.take(), lines.last_mut()) {
            last.push(' ');
            last.push_str(&held);
        }
        if let Some(line) = opened {
            let line = crate::text::trim(line);
            prev_cells = cells(line);
            lines.push(line.to_string());
        } else {
            let first_word = text.split_whitespace().next().map_or(0, cells);
            let may_wrap = prev_cells + 1 + first_word > wrap;
            prev_cells = cells(text);
            tail = Some((text.to_string(), may_wrap));
        }
    }
    let (description, unsure) = match tail {
        Some((text, may_wrap)) => (text, may_wrap),
        None => (String::new(), false),
    };
    Some(Shell {
        lines,
        description,
        unsure,
    })
}

/// The box that waits on the person in the composer's place on `rows` ([`card`]), for display:
/// `None` on any other screen. Nothing answers it here: Kimi Code is read, not supervised
/// ([`crate::Program::supervisable`]), so its options carry no role ([`Role::Other`]), and Esc on
/// an open box is unmeasured, so none is read as cancelling it.
///
/// A command's approval reads its command's lines and description ([`shell`]): its rows are the
/// lines, joined by a newline for the read-only rule's reading ([`PromptV2::readings`]), and a
/// last row that may be either a wrap or the description is kept as both
/// ([`PromptV2::description_rows`]), so the rule reads the command with and without it. A file's
/// write or edits names its file ([`file_of`]): its command is the file as drawn and its path
/// the file only when drawn whole. Any other tool's approval is read by its title alone: its
/// blocks are unmeasured. A question's text is its `? ` row; its review step's
/// is the last row over its options. The trust prompt's command is the folder it names, and its
/// two options are the unnumbered rows over its lower rule, picked by arrows and Enter. Every
/// other box's options stand at the columns [`choice`] reads and a digit picks one, or toggles
/// one on a multi-select question, whose options are numbered in order from the first it shows
/// ([`first_shown`]) and whose focus and ticks are not read. Its key row is the last row in the
/// box, the trust prompt's the row under its title.
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    let c = card(rows)?;
    let words = |i: usize| crate::text::trim(&rows[i]);
    let edge = crate::phase::leading_spaces(&rows[c.upper]);
    let drawn: Vec<usize> = (c.title + 1..c.lower)
        .filter(|&i| !crate::text::blank(&rows[i]))
        .collect();
    let trust = c.kind == PromptKind::Trust;
    let options: Vec<Opt> = if trust {
        let mut block = Vec::new();
        for i in (c.title + 1..c.lower).rev() {
            if !crate::text::blank(&rows[i]) {
                block.push(i);
            } else if !block.is_empty() {
                break;
            }
        }
        block
            .into_iter()
            .rev()
            .map(|i| {
                let text = words(i);
                let label = text.strip_prefix('❯').map(crate::text::trim);
                Opt {
                    n: None,
                    label: label.unwrap_or(text).to_string(),
                    role: Role::Other,
                    focused: label.is_some(),
                    row: i,
                    kept: false,
                }
            })
            .collect()
    } else {
        let mut unnumbered = first_shown(rows, c.title + 1, c.lower);
        drawn
            .iter()
            .filter_map(|&i| {
                let (n, focused, label) = choice(&rows[i], edge)?;
                let n = n.or_else(|| {
                    let n = unnumbered;
                    unnumbered = unnumbered.saturating_add(1);
                    Some(n)
                });
                Some(Opt {
                    n,
                    label: label.to_string(),
                    role: Role::Other,
                    focused,
                    row: i,
                    kept: false,
                })
            })
            .collect()
    };
    let first_option = options.first().map_or(c.lower, |o| o.row);
    let width = crate::text::trim(&rows[c.upper]).chars().count();
    let shown = match c.kind {
        PromptKind::Bash => shell(rows, c.title + 1, first_option, edge, width).unwrap_or_default(),
        _ => Shell::default(),
    };
    let command = shown.lines.join(" ");
    let mut command_rows = shown.lines;
    if shown.unsure {
        command_rows.push(shown.description.clone());
    }
    let asked = match c.kind {
        PromptKind::Question => drawn
            .iter()
            .copied()
            .find(|&i| words(i).starts_with("? "))
            .or_else(|| drawn.iter().copied().rfind(|&i| i < first_option)),
        _ => Some(c.title),
    };
    let named = match c.kind {
        PromptKind::Trust => drawn
            .iter()
            .map(|&i| words(i))
            .find(|t| t.starts_with('/') || t.starts_with('~'))
            .map(|path| (path, true)),
        PromptKind::Write | PromptKind::Edit => {
            file_of(rows, c.title + 1, first_option, edge, width)
        }
        _ => None,
    };
    let path = named.and_then(|(path, whole)| whole.then(|| path.to_string()));
    let keys = if trust {
        drawn.first().copied()
    } else {
        drawn.last().copied()
    };
    let command = match c.kind {
        PromptKind::Bash => command,
        PromptKind::Trust | PromptKind::Write | PromptKind::Edit => named
            .map(|(drawn, _)| drawn.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    Some(PromptV2 {
        kind: c.kind,
        title: titled(&rows[c.title]).to_string(),
        command_rows,
        description_rows: usize::from(shown.unsure),
        gutter: c.kind == PromptKind::Bash,
        command,
        description: match c.kind {
            PromptKind::Bash => shown.description,
            PromptKind::Question => asked
                .map(|i| {
                    let text = words(i);
                    text.strip_prefix("? ").unwrap_or(text).to_string()
                })
                .unwrap_or_default(),
            _ => String::new(),
        },
        notes: Vec::new(),
        auto_deny: None,
        path,
        options,
        select: if trust {
            Select::ArrowsEnter
        } else {
            Select::Digits
        },
        cancel: None,
        question: asked,
        span: (c.upper, c.lower),
        footer: keys.map(|i| words(i).to_string()),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    })
}

/// Whether `rows` are Kimi Code's own screen: its welcome row, its composer over its
/// `context: N%` footer ([`composer_rows`]), or that footer standing at the screen's foot
/// under a box drawn in the composer's place ([`context_left`], measured on 2.4.0's approval
/// and question frames).
#[must_use]
pub fn is_kimi_screen(rows: &[String]) -> bool {
    composer_rows(rows).is_some()
        || context_left(rows).is_some()
        || rows.iter().any(|r| {
            crate::text::trim_start(r)
                .strip_prefix('│')
                .is_some_and(|inner| crate::text::contains(inner, crate::anchor!("kimi.welcome")))
        })
}

/// Whether `row` continues the `Error: …` block opened at `indent` columns in a composer frame
/// `box_cols` wide: either an indented continuation paragraph (`   If this persists, …`), or a
/// word that wrapped back to Kimi Code's one-column margin because `prev` had no room for it.
fn continues_error(prev: &str, row: &str, indent: usize, box_cols: usize) -> bool {
    let lead = crate::phase::leading_spaces(row);
    if lead >= indent {
        return true;
    }
    if lead == 0 {
        return false;
    }
    let trimmed = crate::text::trim_start(row);
    if trimmed.starts_with(['❯', '●', '✗', '✦', '▶', '╭', '┌', '│', '╰', '└', '─'])
    {
        return false;
    }
    let Some(first_word) = trimmed.split_whitespace().next() else {
        return false;
    };
    let prev_cols = crate::text::trim_end(prev).chars().count();
    let word_cols = first_word.chars().count();
    box_cols > 0 && prev_cols + 1 + word_cols >= box_cols
}

/// The wall the last turn ended on: the `Error: …` row standing last above the composer, its
/// indented continuation rows joined (along with words Kimi Code wrapped back to column 1 at the
/// composer's right edge), read as Kimi Code reads it — a spent quota
/// ([`WallKind::UsageSession`]: it names no window), a spent balance ([`WallKind::Spend`]), or a
/// login gone ([`WallKind::Auth`]). A concurrency throttle is none, and so is any other error.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    let (top, _, _) = composer_rows(rows)?;
    let box_cols = crate::text::trim_end(&rows[top]).chars().count();
    let last = rows[..top].iter().rposition(|r| !crate::text::blank(r))?;
    let opener = crate::anchor!("kimi.error");
    let at = (0..=last)
        .rev()
        .take_while(|&i| !crate::text::blank(&rows[i]))
        .find(|&i| crate::text::trim_start(&rows[i]).starts_with(opener))?;
    let indent = crate::phase::leading_spaces(&rows[at]);
    if (at + 1..=last).any(|i| !continues_error(&rows[i - 1], &rows[i], indent, box_cols)) {
        return None;
    }
    let message = rows[at..=last]
        .iter()
        .map(|r| crate::text::trim(r))
        .collect::<Vec<_>>()
        .join(" ");
    let said = message.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| said.contains(w));
    let kind = if has(&THROTTLE) {
        return None;
    } else if has(&QUOTA) {
        WallKind::UsageSession
    } else if has(&BALANCE) {
        WallKind::Spend
    } else if has(&AUTH) {
        WallKind::Auth
    } else {
        return None;
    };
    Some(Wall {
        kind,
        message,
        reset: None,
        row: at,
        placement: Placement::ErrorRow,
        under_call: false,
    })
}

#[cfg(test)]
#[path = "kimi_tests.rs"]
mod tests;

/// Kimi Code's screens, measured or hand-built (each file's head says which), for tests here and
/// in dependent crates; not part of the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// Kimi Code 2.4.1's local API-stub reply and context counters (MEASURED).
    pub const STUB_REPLY: &str = include_str!("fixtures/kimi-2.4.1-local-stub-reply.txt");
    /// Kimi Code 2.4.0 after ten 429 quota failures: `Error: [provider.rate_limit] 429 …`
    /// soft-wrapped to column 1 over the `/export-debug-zip` advice (MEASURED).
    pub const LIMIT_429_FINAL_240: &str = include_str!("fixtures/kimi-2.4.0-limit-429-final.txt");
    /// Kimi Code 2.4.0 retrying a 429 quota failure (`Retrying (6/10) · …`): not yet a wall
    /// (MEASURED).
    pub const LIMIT_429_RETRYING_240: &str =
        include_str!("fixtures/kimi-2.4.0-limit-429-retrying.txt");
    /// Kimi Code 2.4.0 after a custom provider's 401 (`[provider.auth_error]`) under an earlier
    /// quota wall: neither a managed login wall nor the earlier quota wall (MEASURED).
    pub const AUTH_401_240: &str = include_str!("fixtures/kimi-2.4.0-auth-401.txt");
    /// Kimi Code 2.4.0 asking to run a shell command, its approval in the composer's place
    /// over the `context: N%` footer (MEASURED).
    pub const BOX_SHELL_240: &str = include_str!("fixtures/kimi-2.4.0-box-shell.txt");
    /// Kimi Code 2.4.0's question standing in the composer's place (MEASURED).
    pub const QUESTION_240: &str = include_str!("fixtures/kimi-2.4.0-question.txt");
    /// Kimi Code 2.5.2 asking to apply a file's edits, its diff in the approval panel (MEASURED).
    pub const BOX_EDIT_252: &str = include_str!("fixtures/kimi-2.5.2-box-edit.txt");
    /// Kimi Code 2.4.0 holding a turn before its first byte: one moon phase over the composer
    /// (MEASURED).
    pub const BUSY_HELD_240: &str = include_str!("fixtures/kimi-2.4.0-busy-held.txt");
    /// That turn's end: the reply, a blank row, the empty composer (MEASURED).
    pub const END_OF_TURN_240: &str = include_str!("fixtures/kimi-2.4.0-end-of-turn.txt");
    /// Kimi Code 2.3.1 signed out at its idle composer, after the folder was trusted (MEASURED).
    pub const IDLE: &str = include_str!("fixtures/kimi-2.3.1-idle.txt");
    /// The same with `hello` typed and unsent (MEASURED).
    pub const DRAFT: &str = include_str!("fixtures/kimi-2.3.1-draft.txt");
    /// The turn a signed-out session ends on: `Error: LLM not set, …` (MEASURED).
    pub const SIGNED_OUT_TURN: &str = include_str!("fixtures/kimi-2.3.1-signed-out-turn.txt");
    /// A turn ended on the quota-exhausted 429 (HAND-BUILT on the measured error row's shape).
    pub const WALL_QUOTA: &str = include_str!("fixtures/kimi-2.3.1-hand-built-wall-quota.txt");
    /// A turn ended on the concurrency throttle's 429 (HAND-BUILT on the same shape): no wall.
    pub const CONCURRENT_LIMIT: &str =
        include_str!("fixtures/kimi-2.3.1-hand-built-concurrent-limit.txt");
}
