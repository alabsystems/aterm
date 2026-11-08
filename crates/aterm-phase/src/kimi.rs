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
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(0, 18, "0.18");

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
/// full-width rules ([`rule`]) holds it, the lower one the last drawn row (the trust prompt and
/// every fullscreen panel, which draw no footer) or standing right over the footer's two rows, the screen's last drawn
/// ones, its `context: N%` row the second ([`footer_used`]), and the first drawn row under the
/// upper rule is one of the titles (`TITLES`, [`approval_of`]). The upper rule is the nearest
/// one over the lower at its column and of its width: the panel draws both at its own column
/// (`renderHead`, `renderFoot`) and every row between them further in, so a rule a row of the
/// panel draws (a plan option's description, a command's line) is none and the box is read under
/// its own title. The session picker stands on the same rules under its own title and is none.
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
    let edge = crate::phase::leading_spaces(&rows[lower]);
    let span = crate::text::trim(&rows[lower]).chars().count();
    let upper = rows[..lower].iter().rposition(|r| {
        rule(r)
            && crate::phase::leading_spaces(r) == edge
            && crate::text::trim(r).chars().count() == span
    })?;
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
/// focus, and [`prompt`] numbers them. A row at any other column (an inline option's
/// description, a question's body, a command) is none, and so is a review step's
/// `→  <answer>`, whose mark two spaces follow. A docked panel draws its focused choice's
/// description in a block of its own at the focused option's column (`renderFoot`), so such a
/// description shaped like an option is read as one: [`prompt`] reads a command's or a file's
/// options only off the block over its key row, and another box's options carry no role.
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

/// The rows a command's or a file's approval draws its options on, over its key row `keys` and
/// below `from`: the run of drawn rows over the blank row above the key row, up to the blank row
/// over it. Kimi Code opens and closes that run with a blank row (`renderFoot`, read in 2.5.2's
/// binary; measured on every 2.4.0 and 2.5.2 approval), and those approvals' choices carry no
/// description (`DEFAULT_APPROVAL_CHOICES`) that could draw a row of its own between them, so
/// the run holds them all, and a row above it shaped like an option (a command's wrap, drawn at
/// an unfocused option's column) is none and stays in the body it belongs to.
fn option_block(rows: &[String], from: usize, keys: usize) -> std::ops::Range<usize> {
    let end = (from..keys)
        .rev()
        .find(|&i| !crate::text::blank(&rows[i]))
        .map_or(from, |i| i + 1);
    let start = (from..end)
        .rev()
        .find(|&i| crate::text::blank(&rows[i]))
        .map_or(from, |i| i + 1);
    start..end
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

/// Whether an approval panel's key row `row` shows its body drawn whole: the focused panel's own
/// key row drawn to its end, `… · ↵ confirm`, past a file block's `· ctrl+e preview`
/// (`renderFoot`, read in 2.5.1's and 2.5.2's binaries; measured on 2.4.0 and 2.5.2). A
/// fullscreen panel docks its body in at most six rows (`maxBodyRows`, fewer on a short screen)
/// and scrolls a taller one, and then its focused key row ends `· shift+↑/↓ scroll`
/// (`bodyOverflows`); the panel draws that hint on no other row, so the feedback line's key row
/// (`Type feedback · ↵ submit.`) and an unfocused panel's (`esc back to approval`) show nothing,
/// and neither does one cut at the box's edge (`...`) or clipped at the screen's (its last cell
/// holds the last glyph written). No column is counted, so no width the grid and the panel
/// disagree on matters.
fn keys_show_whole(row: &str) -> bool {
    let row = crate::text::trim(row);
    row.ends_with(crate::anchor!("kimi.box.keys.confirm"))
        || row.ends_with(crate::anchor!("kimi.box.keys.preview"))
}

/// Whether a box whose rules start `edge` columns in and span `width` cells, its upper rule on
/// screen row `upper` and its key row `keys`, shows its body rows `body` drawn whole. Its key row
/// says so ([`keys_show_whole`]), but its scroll hint is judged off the previous layout
/// (`bodyOverflows` reads `bodyScroll.canScroll`, which the layout sets only after measuring the
/// foot, and nothing renders again), so the first frame of a dock squeezed below its body's rows
/// shows the key row whole over a body drawn in part: measured on 2.5.2, a fourteen-row screen's
/// panel for `cat README.md` and `git push --force` showing its `cwd` and `$` rows over
/// `↵ confirm`. Two marks the same frame draws off the current layout disprove it:
/// - the scrollbar, painted after the layout (`getScrollbarGeometry`) in the screen's last
///   column, one past the rules, where the panel draws no text of its own: a body row that ends
///   in its track `│` or its thumb `┃` (`█` under the pointer), or reaches that column (a row the
///   grid draws wider than Kimi counts, its bar pushed past the screen), is drawn in part;
/// - the dock's place: the fullscreen root gives the transcript at least one row
///   (`minSize: 1`) and the dock the rest, so a squeezed dock stands on the screen's second row,
///   and a box whose upper rule stands there or higher is unproven (an inline box that fills the
///   screen too). While a box waits the dock hides its activity, queue and side question, and a
///   squeeze takes a notice's row (`minSize: 0`) before the box's, so a squeezed box tops the
///   dock; a survey (`shrink: 0`) is the one row kept over it, which moves it lower and leaves
///   the scrollbar the one mark.
fn drawn_whole(
    rows: &[String],
    upper: usize,
    body: std::ops::Range<usize>,
    keys: usize,
    edge: usize,
    width: usize,
) -> bool {
    keys_show_whole(&rows[keys])
        && upper >= 2
        && rows[body].iter().all(|row| {
            let drawn = row.trim_end();
            !drawn.ends_with(['│', '┃', '█']) && crate::question::cells(drawn) <= edge + width
        })
}

/// Whether a body row's `words` were cut at the box's edge: the panel ends every body row too
/// wide for it with `...` (`truncateToWidth`, read in 2.5.2's binary). No width tells a cut row
/// from a whole one: the cut stops short of the edge by up to a grapheme's width less one (a
/// grapheme that does not fit, four cells to Kimi for `ကျွေ`), and the grid may draw a row
/// narrower than Kimi counts it (an asat, U+103A, one cell to Kimi and none on the grid). So
/// every row ending `...` is cut, and a path that really ends so names no file.
fn cut(words: &str) -> bool {
    words.ends_with("...")
}

/// The file a file's approval names, in a box whose rules start `edge` columns in: its block's
/// first row on `rows` `from..to` (a body drawn whole, [`prompt`]), two columns in from the
/// rules, and whether that row shows it whole. A file written whole draws its path there over its numbered
/// content; a file's edits draw a diff headed by the counts it adds and removes before the path
/// (`+1 -1 <path>`, `renderDiffLinesClustered`, read in 2.5.2's binary). Measured on 2.5.2's
/// write and edit frames; a path cut at the box's edge ([`cut`]) names no file.
fn file_of(rows: &[String], from: usize, to: usize, edge: usize) -> Option<(&str, bool)> {
    let row = (from..to).find(|&i| !crate::text::blank(&rows[i]))?;
    if crate::phase::leading_spaces(&rows[row]) != edge + 2 {
        return None;
    }
    let words = crate::text::trim(&rows[row]);
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
    (!text.is_empty()).then(|| (text, !cut(words)))
}

/// A command's approval ([`prompt`]): each line's rows, the words its mark opens and then each
/// row it wraps onto, and whether the block's last row is a wrap, which may be the description.
#[derive(Debug, Default)]
struct Shell {
    lines: Vec<Vec<String>>,
    described: bool,
}

/// The most wraps a command's approval is read across: each is read two ways
/// ([`Shell::spellings`]), so a command drawn on more is left unread.
const MAX_WRAPS: usize = 6;

impl Shell {
    /// The block's last row when it may be the description.
    fn tail(&self) -> Option<&str> {
        self.described
            .then(|| self.lines.last()?.last().map(String::as_str))
            .flatten()
    }

    /// The command's lines as drawn, its rows joined by a space, the description left off.
    fn lines_drawn(&self) -> Vec<String> {
        let mut lines: Vec<Vec<String>> = self.lines.clone();
        if self.described
            && let Some(line) = lines.last_mut()
        {
            line.pop();
        }
        lines.iter().map(|line| line.join(" ")).collect()
    }

    /// Every command the block may stand for: its lines joined by a newline, each wrap joined by
    /// a space (a run of spaces dropped there) and by none (a word cut there, or broken at a
    /// carriage return), the description's row read off and on.
    fn spellings(&self) -> Vec<String> {
        fn spell(lines: &[Vec<String>], tail: bool) -> Vec<String> {
            let mut out = vec![String::new()];
            let rows = lines.len();
            for (n, line) in lines.iter().enumerate() {
                let kept = if tail || n + 1 < rows {
                    line.len()
                } else {
                    line.len() - 1
                };
                for (k, row) in line.iter().take(kept).enumerate() {
                    let joins: &[&str] = match (n, k) {
                        (0, 0) => &[""],
                        (_, 0) => &["\n"],
                        _ => &[" ", ""],
                    };
                    out = out
                        .iter()
                        .flat_map(|head| joins.iter().map(move |join| format!("{head}{join}{row}")))
                        .collect();
                }
            }
            out
        }
        if self.lines.is_empty() {
            return Vec::new();
        }
        let mut out = if self.described {
            spell(&self.lines, false)
        } else {
            Vec::new()
        };
        out.extend(spell(&self.lines, true));
        out
    }
}

/// The command a box whose rules start `edge` columns in shows on its rows `from..to` (read in
/// 2.5.2's `renderShellDisplayBlock`, whose shape 2.4.0's approval frame shows). Its first line
/// follows `$ ` and every later one `· `, both two columns in from the rules (an empty line is
/// its mark alone), a line too long for the box wrapping onto rows two columns further in; the
/// description, when the tool gave one, is the block's last row, at that same depth. The block
/// ends at a blank row, and every row after it up to `to` is blank: a carriage return at a line's
/// end draws a blank row inside the block (`wrapTextWithAnsi` breaks there; measured on 2.5.2,
/// `cat README.md` and `git push --force` split by one), and that, a row the block holds at no
/// depth of its own, more than [`MAX_WRAPS`] wraps, or a `Dangerous: <label>` row over it leaves
/// the command unread (`None`), never read short. Kimi draws that row when its own patterns match the command
/// (`detectDanger`, read off the command itself), and the drawn lines may not be the command:
/// Kimi passes its control characters through raw, and one that moves the cursor draws later
/// words over earlier ones (measured on 2.5.2, `rm -rf build-output`, nineteen backspaces and
/// `cat README.md` drawn as `$ cat README.md` under `Dangerous: recursive delete`); an escape
/// sequence can redraw any row, that one too (measured: a command ending `ESC[1A ESC[2K` blanked
/// the row over its own). Only Kimi's own patterns draw that row, so a command they miss
/// (`git push`, `mv`, a write) can be redrawn as a read with none, which no reading of the
/// screen sees: the verdict here is on the command as drawn, and aterm-gui's presence judges a
/// read-only one again against the command Kimi wrote to its own session record
/// (`docs/DESIGN-kimi-wire-verdict-2026-11-07.md`). No wrap shows what it stood on ([`Shell::spellings`]): a
/// run of spaces that reaches it is dropped there whole (`wrapSingleLine`), so the row over it
/// may have had room for the next word (measured on 2.5.2, `cat README.md`, 130 spaces and
/// `;git push --force`); a word wider than the box is cut with none (`breakLongWord`; measured,
/// `echo`, a word of 138 `a` and `#;git push --force` drawn as three rows), and so is a line at a
/// carriage return within it (measured, `cat README.md`, a carriage return and
/// `#;git push --force` drawn as two). The block's last row at the wrap's depth is that or the
/// description ([`Shell::described`]), and no width tells them apart.
fn shell(rows: &[String], from: usize, to: usize, edge: usize) -> Option<Shell> {
    fn marked<'a>(text: &'a str, mark: &str) -> Option<&'a str> {
        text.strip_prefix(mark)
            .filter(|rest| rest.is_empty() || rest.starts_with(' '))
    }
    let start = (from..to).find(|&i| {
        crate::phase::leading_spaces(&rows[i]) == edge + 2
            && marked(crate::text::trim(&rows[i]), "$").is_some()
    })?;
    let flagged = rows[from..start]
        .iter()
        .any(|r| crate::text::trim(r).starts_with(crate::anchor!("kimi.box.dangerous")));
    if flagged {
        return None;
    }
    let mut lines: Vec<Vec<String>> = Vec::new();
    let mut described = false;
    let mut end = to;
    for (i, row) in rows.iter().enumerate().take(to).skip(start) {
        let text = crate::text::trim(row);
        if text.is_empty() {
            end = i;
            break;
        }
        let lead = crate::phase::leading_spaces(row);
        let opened = if lead == edge + 2 {
            marked(text, if lines.is_empty() { "$" } else { "·" })
        } else {
            None
        };
        match (opened, lines.last_mut()) {
            (Some(line), _) => lines.push(vec![crate::text::trim(line).to_string()]),
            (None, Some(line)) if lead == edge + 4 => line.push(text.to_string()),
            _ => return None,
        }
        described = opened.is_none();
    }
    let wraps: usize = lines.iter().map(|line| line.len() - 1).sum();
    if wraps > MAX_WRAPS || rows[end..to].iter().any(|r| !crate::text::blank(r)) {
        return None;
    }
    Some(Shell { lines, described })
}

/// The box that waits on the person in the composer's place on `rows` ([`card`]), for display:
/// `None` on any other screen. Nothing answers it here: Kimi Code is read, not supervised
/// ([`crate::Program::supervisable`]), so its options carry no role ([`Role::Other`]), and Esc on
/// an open box is unmeasured, so none is read as cancelling it.
///
/// A command's approval reads its command's lines and description ([`shell`]): its rows are the
/// lines, and its last row at the description's depth, the description or the last line's wrap,
/// is kept as both ([`PromptV2::description_rows`]); the read-only rule reads every command the
/// rows may stand for ([`PromptV2::spellings`]). A file's write or edits
/// names its file ([`file_of`]): its command is the file as drawn and its path the file only
/// when drawn whole. Either is read only off a body proven drawn whole ([`drawn_whole`]): a
/// fullscreen panel's body taller than its dock is drawn in part, its command's hidden lines
/// unjudged and its file's head row first only at the top of its scroll, and a key row that
/// shows nothing (the feedback line's, an unfocused panel's, a cut one) leaves the body
/// unproven. Any other tool's approval is read by its title alone: its blocks are unmeasured.
/// A question's text is its `? ` row; its review step's
/// is the last row over its options. The trust prompt's command is the folder it names, and its
/// two options are the unnumbered rows over its lower rule, picked by arrows and Enter. Every
/// other box's options stand at the columns [`choice`] reads, a command's or a file's approval's
/// only in the block over its key row ([`option_block`]), whose body is read; another box's
/// choices may carry descriptions, drawn among them or, docked, under them in a block of their
/// own, so its options are read wherever they stand in it, and its body for nothing. A docked
/// description stands at the focused option's column, so one shaped like an option is read as
/// one there; such a box's options are display alone. A digit
/// picks one, or toggles
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
        let block = match c.kind {
            PromptKind::Bash | PromptKind::Write | PromptKind::Edit => {
                drawn.last().map_or(c.lower..c.lower, |&keys| {
                    option_block(rows, c.title + 1, keys)
                })
            }
            _ => c.title + 1..c.lower,
        };
        let mut unnumbered = first_shown(rows, block.start, block.end);
        block
            .filter_map(|i| {
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
    let keys = if trust {
        drawn.first().copied()
    } else {
        drawn.last().copied()
    };
    let whole =
        keys.is_some_and(|i| drawn_whole(rows, c.upper, c.title + 1..first_option, i, edge, width));
    let shown = match c.kind {
        PromptKind::Bash if whole => {
            shell(rows, c.title + 1, first_option, edge).unwrap_or_default()
        }
        _ => Shell::default(),
    };
    let mut command_rows = shown.lines_drawn();
    let command = command_rows.join(" ");
    let tail = shown.tail().map(str::to_string);
    let description_rows = usize::from(tail.is_some());
    command_rows.extend(tail.iter().cloned());
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
        PromptKind::Write | PromptKind::Edit if whole => {
            file_of(rows, c.title + 1, first_option, edge)
        }
        _ => None,
    };
    let path = named.and_then(|(path, whole)| whole.then(|| path.to_string()));
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
        description_rows,
        spellings: (c.kind == PromptKind::Bash).then(|| shown.spellings()),
        gutter: c.kind == PromptKind::Bash,
        command,
        description: match c.kind {
            PromptKind::Bash => tail.unwrap_or_default(),
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
    /// Kimi Code 2.5.2 asking to run `cat README.md`, an empty line (its `·` alone) and
    /// `git push --force` (MEASURED).
    pub const BOX_SHELL_EMPTY_LINE_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-empty-line.txt");
    /// Kimi Code 2.5.2 asking to run `cat README.md` ended by a carriage return, then
    /// `git push --force`: a blank row inside the block (MEASURED).
    pub const BOX_SHELL_CARRIAGE_RETURN_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-carriage-return.txt");
    /// Kimi Code 2.5.2 asking to run `cat README.md`, 130 spaces and `;git push --force` on one
    /// line: the wrap drawn under a row with room for its first word (MEASURED).
    pub const BOX_SHELL_SPACED_252: &str = include_str!("fixtures/kimi-2.5.2-box-shell-spaced.txt");
    /// Kimi Code 2.5.2 asking to run `echo`, a word of 138 `a` and `#;git push --force` with no
    /// space between: the word cut at the box's edge with none (MEASURED).
    pub const BOX_SHELL_LONG_WORD_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-long-word.txt");
    /// Kimi Code 2.5.2 asking to approve a plan whose second option's description draws a rule,
    /// `Run this command?` and `$ cat README.md` among the options (MEASURED).
    pub const BOX_PLAN_FORGED_252: &str = include_str!("fixtures/kimi-2.5.2-box-plan-forged.txt");
    /// Kimi Code 2.5.2 asking to run `cat README.md`, 130 spaces and `1. ;git push --force`: the
    /// wrap opening like an option at the unfocused options' column (MEASURED).
    pub const BOX_SHELL_OPTION_SHAPED_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-option-shaped.txt");
    /// Kimi Code 2.5.2 asking to run `cat README.md`, two carriage returns and
    /// `1. ;git push --force`: that row under a blank one (MEASURED).
    pub const BOX_SHELL_OPTION_SHAPED_CR_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-option-shaped-cr.txt");
    /// Kimi Code 2.5.2 asking to run `cat README.md`, a carriage return and
    /// `#;git push --force` on one line: the line broken there with no space (MEASURED).
    pub const BOX_SHELL_CARRIAGE_RETURN_MID_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-carriage-return-mid.txt");
    /// Kimi Code 2.5.2 asking to run `rm -rf build-output` redrawn by backspaces as
    /// `cat README.md` under its `Dangerous: recursive delete` row (MEASURED).
    pub const BOX_SHELL_BACKSPACED_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-backspaced.txt");
    /// Kimi Code 2.5.2 asking to run `cat README.md`, described `Show the readme`, the box
    /// whose record line is aterm-agent's `kimi-2.5.2-request.jsonl` (MEASURED).
    pub const BOX_SHELL_READ_252: &str = include_str!("fixtures/kimi-2.5.2-box-shell-read.txt");
    /// Kimi Code 2.5.2 asking to run `git push --force` redrawn by backspaces as
    /// `cat README.md`, the row [`BOX_SHELL_READ_252`] draws, with no danger named: the box whose
    /// record line is aterm-agent's `kimi-2.5.2-request-backspaced.jsonl` (MEASURED).
    pub const BOX_SHELL_BACKSPACED_PUSH_252: &str =
        include_str!("fixtures/kimi-2.5.2-box-shell-backspaced-push.txt");
    /// Kimi Code 2.5.2 under `tui_mode = "fullscreen"` asking to run a two-line command, with a
    /// description, whose body fits the panel's dock (MEASURED).
    pub const FULLSCREEN_BOX_SHELL_252: &str =
        include_str!("fixtures/kimi-2.5.2-fullscreen-box-shell.txt");
    /// A fourteen-row screen's dock squeezed under that command's body, on its first frame: a
    /// scrollbar over the cwd and `$` rows, `· git push --force` hidden, and the key row still
    /// `↵ confirm` (MEASURED).
    pub const FULLSCREEN_BOX_SHELL_SQUEEZED_252: &str =
        include_str!("fixtures/kimi-2.5.2-fullscreen-box-shell-squeezed.txt");
    /// The same panel asking to run six lines, the last two (`rm -rf build-output` the last)
    /// and the description hidden below the six rows its dock shows (MEASURED).
    pub const FULLSCREEN_BOX_SHELL_OVERFLOW_252: &str =
        include_str!("fixtures/kimi-2.5.2-fullscreen-box-shell-overflow.txt");
    /// A fourteen-row screen's dock squeezed to two body rows, on its feedback line: no scroll
    /// hint on its key row, a thumb alone for its bar, and two lines of two-cell glyphs over
    /// hidden `mv` and `rm -rf` lines (MEASURED).
    pub const FULLSCREEN_BOX_SHELL_FEEDBACK_WIDE_252: &str =
        include_str!("fixtures/kimi-2.5.2-fullscreen-box-shell-feedback-wide.txt");
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
