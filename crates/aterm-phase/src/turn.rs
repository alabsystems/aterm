// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a finished turn leaves on the screen for whoever decides what comes
//! next: Claude Code's own suggestion for the next message, whether a `/goal`
//! is armed, and the worker's last words.

use crate::phase::{
    composer_frame, composer_text, is_against_right_edge, is_placeholder, is_tool_call,
    last_said_index, leading_spaces, status_block, transcript_end,
};
use crate::prompt::parse_prompt;

/// No turn yet: Claude Code's launch card (`Claude Code v<version>`, anchor
/// `launch.banner`) above the composer and, between it and the composer's
/// frame, no user message (`❯`) and nothing said (`⏺`, `●`) — the screen
/// `claude` draws before anyone typed. A session resumed on a conversation
/// shows its messages under the card, and one past its first screen has
/// scrolled the card away: neither is fresh. Without this a supervisor reads a
/// brand-new session's first idle point as a worker that stopped short and
/// types `keep going` into it.
///
/// A CONVERSATION A `/clear` JUST STARTED is one too: Claude Code clears the
/// screen and redraws the card with the `❯ /clear` that drew it and nothing
/// else (2.1.300, the lab, 2026-10-15). Read as a prompt not answered yet, it
/// was no point at all, and a usage limit the cleared conversation had met
/// stayed waited on — its ledger row open — for good (W4 of ATERM-KI-1015-1).
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let banner = const { crate::anchors::anchor_text("launch.banner") };
    let Some(card) = rows[..frame.top].iter().rposition(|r| r.contains(banner)) else {
        return false;
    };
    let mut said = rows[card + 1..frame.top]
        .iter()
        .filter(|r| r.starts_with(['❯', '⏺', '●']));
    match said.next() {
        None => true,
        Some(row) => crate::text::trim_end(row) == "❯ /clear" && said.next().is_none(),
    }
}

/// What Claude Code's launch card says about the process that drew it:
/// its version row (`Claude Code v2.1.283`, anchor `launch.banner`) and the
/// row under it (`Opus 5.5 with xhigh effort · Claude Max`, `Haiku 4.5 ·
/// Claude Max`). The model and effort are the ones the card was drawn with:
/// the process's starting ones on a fresh screen. The inline renderer draws
/// it once; the fullscreen renderer draws it again after a `/model` choice,
/// with the new model (measured 2026-09-28 on 2.1.283: `Haiku 4.5 · Claude
/// Max` became `Sonnet 5 · Claude Max` after a switch; the model-switch
/// capture still shows `Haiku 4.5` while the switch is being confirmed) —
/// under a user row by then, where [`launch_card`] does not read it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaunchCard {
    /// The build the card names (`2.1.283`).
    pub version: String,
    /// The model as the card spells it (`Opus 5.5`).
    pub model: String,
    /// The effort level word the card names after `with` (`xhigh`), when it
    /// names one — a model without effort levels has none.
    pub effort: Option<String>,
}

/// The launch card above the composer, read ONLY when it is the newest card
/// on the screen and nothing a person said sits between it and the
/// composer's frame: no user message (`❯`) — a system notice (`⏺ agents-md:
/// …`, which the owner's fresh screen always carries) is not one, so this is
/// not [`fresh`]. A newer card BELOW the frame (the inline renderer
/// relaunched in the same tab, its predecessor's box still above) means the
/// frame is not the newest process's, and nothing is read. A model row the
/// vendor split or cut for a narrow pane (no ` · ` after the model, or a
/// `…` in it) is refused rather than half-read. Whether the frame is a LIVE
/// REPL's, and whether the card's build is the running one, are the caller's
/// to check.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<LaunchCard> {
    let frame = composer_frame(rows)?;
    let banner = const { crate::anchors::anchor_text("launch.banner") };
    let card = rows.iter().rposition(|r| r.contains(banner))?;
    if card >= frame.top || rows[card + 1..frame.top].iter().any(|r| r.starts_with('❯')) {
        return None;
    }
    let (_, after) = rows[card].split_once(banner)?;
    let version = banner_version(after)?.to_owned();
    let row = rows.get(card + 1)?;
    // The logo's block elements (U+2580..=U+259F) and the indent go first.
    let text = row
        .trim_start_matches(|c: char| c == ' ' || ('\u{2580}'..='\u{259F}').contains(&c))
        .trim_end();
    if text.contains('\u{2026}') {
        return None;
    }
    let (model, _billing) = text.split_once(" \u{00B7} ")?;
    let (model, effort) = match model
        .strip_suffix(" effort")
        .and_then(|m| m.rsplit_once(" with "))
    {
        Some((m, level)) if !level.is_empty() && level.bytes().all(|b| b.is_ascii_lowercase()) => {
            (m, Some(level.to_owned()))
        }
        _ => (model, None),
    };
    let model = crate::text::trim(model);
    (!model.is_empty()).then(|| LaunchCard {
        version,
        model: model.to_owned(),
        effort,
    })
}

/// The build a banner names, read from what follows the banner text (anchor
/// `launch.banner`) on its row: the leading run of digits and dots, taken
/// only when it is three numeric parts of one to six digits each
/// (`2.1.300`). Anything after the run is not looked at here — the card's
/// readers decide whether text may follow it. Shared by [`launch_card`],
/// [`launch_card_version`] and the narrow card's banner test, so the three
/// agree on what a version is.
fn banner_version(after: &str) -> Option<&str> {
    let len = after
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    // The run is ASCII, so `len` bytes end on a char boundary.
    let version = &after[..len];
    let mut parts = 0;
    for part in version.split('.') {
        if part.is_empty() || part.len() > 6 {
            return None;
        }
        parts += 1;
    }
    (parts == 3).then_some(version)
}

/// Where Claude Code's launch card sits on the screen, for a decoration
/// drawn around it (the harness card, `DESIGN-harness-autopilot-2026-10-15.md`
/// §3) — [`launch_card_place`]. The card is three rows — the banner, the
/// model and the working directory — the block-glyph mascot down the left
/// and the text in one column right of it:
///
/// ```text
///  ▐▛███▛█   Claude Code v2.1.300
/// ▝▜██████▀  Opus 5.5 with xhigh effort · Claude Max
///  ▝▝   ▝▝   ~/aterm
/// ```
///
/// Columns are CHAR indices into the rows as passed: one char per cell on
/// the window's live text, so there a char index is the cell's column.
/// Measured on the 144 three-row cards in `src/fixtures` (2.1.280 to
/// 2.1.300, 44 to 180 columns): the text at column 11, the mascot ending at
/// column 9, the banner's text at column 31.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardPlace {
    /// The banner row's index in the rows: the card's top row.
    pub top: usize,
    /// How many rows the card spans from [`top`](Self::top): 3.
    pub rows: usize,
    /// The column every card row's text begins at (11).
    pub text_col: usize,
    /// The column just past the mascot: past its rightmost block glyph on
    /// any of the three rows (9; the model row's `▝▜██████▀` is the widest).
    pub mascot_end: usize,
    /// The column just past each card row's text, its trailing blanks
    /// trimmed: the banner's (31 for `Claude Code v2.1.300`), the model
    /// row's and the working directory's.
    ///
    /// A CHAR count, so a row whose last glyph is WIDE (a CJK name in the
    /// working directory, an emoji) ends one cell short: the wide glyph's
    /// spacer cell is a trailing blank in the window's live text and is
    /// trimmed with the rest. The GUI caller, which knows the cells, adds 1
    /// to `ends[k]` when the cell at `ends[k] - 1` holds a wide glyph.
    pub ends: [usize; 3],
}

/// Where the NEWEST launch card on the screen sits ([`CardPlace`]), read by
/// its geometry: the banner row STARTS with the mascot's ` ▐` and the banner
/// text (anchor `launch.banner`) begins right of the mascot, at the text
/// column; each of the two rows under it holds nothing but the mascot's
/// block glyphs (U+2580..=U+259F, at least one) and spaces before that same
/// column, and text AT it. The rows' block patterns are not compared: the
/// feet row is ` ▝▝   ▝▝` on 2.1.283 and `  ▝▝ ▝▝` on 2.1.280.
///
/// Unlike [`launch_card`] this has no composer gate and does not stop at a
/// user row: the card stays where it was drawn after a person types under it,
/// with a box over the composer, and while a turn runs — and so does a
/// decoration around it. Whether the screen is a live Claude Code's, and
/// whether the card is the running process's, are the caller's to check.
///
/// The newest card is the lowest row that draws a card's banner, in either
/// layout below; a row with anything but the mascot's glyphs and spaces before
/// the banner is something that NAMES the card, never a card, and is passed
/// over: a person's `❯ what changed in Claude Code v2.1.284?`, a tool's
/// output quoting a card under `⎿`, and a quote indented to a message's or a
/// tool output's column (its `▐` right of column 1: at 3 under a message, at
/// 6 or beyond in a tool's output).
///
/// A newest card this cannot place yields `None`, never an older card above
/// it, which a finished process drew (the inline renderer relaunched in the
/// same tab keeps its predecessor's card on the screen):
///
/// * the NARROW layout (measured at 34 and 36 columns, 2.1.295 to 2.1.300):
///   the banner alone on its row at the text column, over FOUR rows — the
///   mascot beside the model, the billing and the working directory. It is
///   refused: the decoration is laid out for the three-row card. A row that
///   holds NOTHING but the banner and its version, indented by spaces alone
///   to the narrow card's column ([`NARROW_BANNER_COLS`]), is read as this
///   layout whatever follows it — cut by the screen's bottom edge, over a
///   blank row, over a bare mascot or over rows laid out otherwise — so a
///   narrow card whose rows under the banner did not draw as measured is
///   still a refusal, never a pass to the older card above. The same bare
///   row at another indent is passed over: a message's line at column 2,
///   and a narrow card QUOTED in a message (column 13) or in a tool's
///   output (column 16), whose banner row loses its `⎿` once it is not the
///   output's first line;
/// * a card cut by the screen's bottom edge, and one whose rows under the
///   banner are laid out otherwise (a vendor redesign): the card goes bare,
///   never wrong.
///
/// Never panics, on any rows.
#[must_use]
pub fn launch_card_place(rows: &[String]) -> Option<CardPlace> {
    launch_card_scan(rows).1
}

/// The three-row card's height ([`CardPlace::rows`]).
const CARD_ROWS: usize = 3;

/// [`launch_card_place`], with where its scan stopped: the newest row that
/// draws a card's banner, in either layout — `None` when no row does. The
/// scan runs bottom-up and never reads a row above the first banner it
/// meets, so the reading is a function of that row and the rows under it
/// alone: a caller that knows which rows changed since can keep it without
/// scanning again ([`launch_card_unmoved`]). Never panics, on any rows.
#[must_use]
pub fn launch_card_scan(rows: &[String]) -> (Option<usize>, Option<CardPlace>) {
    let banner = const { crate::anchors::anchor_text("launch.banner") };
    let Some((top, layout)) = rows
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, r)| banner_row(r, rows.get(i + 1), banner).map(|l| (i, l)))
    else {
        return (None, None);
    };
    let place = match layout {
        BannerRow::Wide { text_col } => wide_card(rows, top, text_col),
        BannerRow::Narrow => None,
    };
    (Some(top), place)
}

/// The three-row card whose banner is row `top`, its text at `text_col`:
/// `None` when a row under the banner is not a card row or is cut off.
fn wide_card(rows: &[String], top: usize, text_col: usize) -> Option<CardPlace> {
    let mut place = CardPlace {
        top,
        rows: CARD_ROWS,
        text_col,
        mascot_end: 0,
        ends: [0; 3],
    };
    for (k, end) in place.ends.iter_mut().enumerate() {
        let (mascot_end, text_end) = card_row(rows.get(top + k)?, text_col)?;
        place.mascot_end = place.mascot_end.max(mascot_end);
        *end = text_end;
    }
    Some(place)
}

/// Whether [`launch_card_scan`] reads `rows` as it read the rows they were
/// before the rows `changed` names changed (ascending indices; the count of
/// rows the same) — known without a scan. `banner` is that reading's banner
/// row. The reading is a function of the banner's row and the rows under it
/// alone: the place, of the card's three rows; whether a newer banner sits
/// lower, of each lower row and the row under it (a banner's test reads the
/// row under it). So it holds while none of the card's three rows changed
/// and no row below the banner's that changed — nor the row over one — now
/// draws a banner; with no banner, while no changed row (nor the row over
/// one) draws one. A row above the banner's never moves it, and the rows
/// that did not change are not read: a keystroke in the composer under the
/// card costs a banner test of its own rows. Never panics, on any rows and
/// indices.
#[must_use]
pub fn launch_card_unmoved(rows: &[String], banner: Option<usize>, changed: &[usize]) -> bool {
    let anchor = const { crate::anchors::anchor_text("launch.banner") };
    if let Some(top) = banner
        && changed
            .iter()
            .any(|&j| (top..top.saturating_add(CARD_ROWS)).contains(&j))
    {
        return false;
    }
    let below = |i: usize| banner.is_none_or(|top| i > top);
    let draws = |i: usize| {
        rows.get(i)
            .is_some_and(|row| banner_row(row, rows.get(i + 1), anchor).is_some())
    };
    changed.iter().all(|&j| {
        [Some(j), j.checked_sub(1)]
            .into_iter()
            .flatten()
            .all(|i| !below(i) || !draws(i))
    })
}

/// The build the placed card's banner names (`2.1.300`), as a slice of its
/// banner row — the version [`launch_card`] reads, by the same parse (three
/// numeric parts), for a caller holding a [`CardPlace`] (the harness card
/// matching the card to the running build without a composer). `None` when
/// the row at `place.top` is not a card's banner row as `place` describes
/// it — it must start with the mascot's top ([`MASCOT_TOP`]) and hold the
/// banner at `place.text_col` (a char count), so a place read from other
/// rows, or a person's `❯ … Claude Code v2.1.284?` that came to sit at
/// `place.top`, names no build — or when the banner's version does not
/// parse. [`launch_card_place`] itself does not need the version to place a
/// card.
///
/// A function over the rows rather than a span in [`CardPlace`]: the place
/// stays `Copy` and plain columns, and the version is read only by the
/// caller that wants it.
#[must_use]
pub fn launch_card_version<'a>(rows: &'a [String], place: &CardPlace) -> Option<&'a str> {
    let banner = const { crate::anchors::anchor_text("launch.banner") };
    let row = rows.get(place.top)?;
    if !row.starts_with(MASCOT_TOP) {
        return None;
    }
    let at = crate::text::find(row, banner)?;
    if row[..at].chars().count() != place.text_col {
        return None;
    }
    banner_version(&row[at + banner.len()..])
}

/// The mascot's top row as the card draws it from column 0: a space, then
/// `▐`.
const MASCOT_TOP: &str = " \u{2590}";

/// The columns a narrow card's banner may begin at when its row holds
/// nothing else ([`banner_row`]'s bare reading). Measured at 11 on every
/// narrow capture in `src/fixtures` (the five `*-34col` and `*-36col`
/// screens, 2.1.295 to 2.1.300) — the three-row card's text column too; two
/// columns left of it (9, 10) and one right (12) are room for a vendor's
/// mascot up to two cells narrower or one wider. The range stays clear of
/// the columns that QUOTE the banner: a message's line at 2, a narrow card
/// indented to a message's column at 13 (2 + 11) and to a tool output's at
/// 16 (5 + 11), so only one column is left on that side.
const NARROW_BANNER_COLS: std::ops::RangeInclusive<usize> = 9..=12;

/// One of the mascot's block glyphs (U+2580..=U+259F).
fn is_block(c: char) -> bool {
    ('\u{2580}'..='\u{259F}').contains(&c)
}

/// How a row that draws a launch card's banner lays the card out
/// ([`launch_card_place`]).
enum BannerRow {
    /// The three-row card: the banner row starts with the mascot
    /// ([`MASCOT_TOP`]), its text at `text_col`.
    Wide { text_col: usize },
    /// The narrow pane's four-row card: the banner alone on its row,
    /// indented by spaces to the narrow card's column
    /// ([`NARROW_BANNER_COLS`]) — the row holds nothing after the banner's
    /// version — or, with text after the version, over a row that starts
    /// with the mascot and has its text at the banner's column.
    Narrow,
}

/// Whether `row` draws a card's banner, and in which layout: `None` for a
/// row without the banner, for one with anything but the mascot's glyphs
/// and spaces before it (a row that names the card), and for one indented
/// past the card's own columns (a quote).
///
/// A row indented by spaces alone to the narrow card's column
/// ([`NARROW_BANNER_COLS`]) is the narrow card's banner when it holds the
/// banner and its version and nothing after them (trailing blanks
/// trimmed), whatever `next` is: the rows under a narrow card's banner may
/// be cut by the screen's edge or drawn otherwise, and that is still a
/// narrow card, refused — never a row passed over for the older card above.
/// A message's line that is nothing but the banner and a version, at that
/// column, is refused with it; the card then goes bare, which is the safe
/// way to be wrong. The same bare row at any other indent is passed over:
/// at column 2 it is a message's line, at 13 and beyond a QUOTED narrow
/// card (a message's or a tool output's), and refusing it would strip the
/// live card above whenever a tool printed a narrow card. With text after
/// the version (`  Claude Code v2.1.284 added the drafts card.`, a
/// message's words) the row is a narrow banner only when `next` confirms
/// it — a mascot row from column 0 with its text at the banner's column,
/// which no quote has — and is otherwise passed over.
fn banner_row(row: &str, next: Option<&String>, banner: &str) -> Option<BannerRow> {
    let at = crate::text::find(row, banner)?;
    let before = &row[..at];
    if !before.chars().all(|c| c == ' ' || is_block(c)) {
        return None;
    }
    let text_col = before.chars().count();
    if row.starts_with(MASCOT_TOP) {
        return Some(BannerRow::Wide { text_col });
    }
    if text_col == 0 || !before.chars().all(|c| c == ' ') {
        return None;
    }
    let after = crate::text::trim_end(&row[at + banner.len()..]);
    let bare = NARROW_BANNER_COLS.contains(&text_col)
        && banner_version(after).is_some_and(|v| v.len() == after.len());
    let confirmed =
        next.is_some_and(|n| n.starts_with(MASCOT_TOP) && card_row(n, text_col).is_some());
    (bare || confirmed).then_some(BannerRow::Narrow)
}

/// One card row read against the text column: before `text_col`, nothing
/// but the mascot's block glyphs (at least one) and spaces, at least one
/// space between the mascot and the text, and text (neither blank nor a
/// block glyph) AT `text_col`. Yields the column past the mascot's last
/// glyph and the column past the row's text, trailing blanks trimmed.
fn card_row(row: &str, text_col: usize) -> Option<(usize, usize)> {
    let mut chars = row.chars();
    let mut mascot_end = 0;
    for (col, c) in chars.by_ref().take(text_col).enumerate() {
        if is_block(c) {
            mascot_end = col + 1;
        } else if c != ' ' {
            return None;
        }
    }
    // A row shorter than the text column has no char here.
    let text = chars.next()?;
    if text.is_whitespace() || is_block(text) || mascot_end == 0 || mascot_end >= text_col {
        return None;
    }
    Some((mascot_end, crate::text::trim_end(row).chars().count()))
}

/// Claude Code's own suggestion for the next message: the DIM placeholder
/// in an empty composer ([`is_placeholder`]: the caret row reads `❯ keep
/// going` but the cursor sits at column 2, where typing would have pushed
/// it). Measured 2026-09-21 on a live session
/// ([`crate::prompt::fixtures::GOAL_ACTIVE_SUGGESTION`]). `None` without the
/// composer frame (the caret-looking row is then a user row or an option),
/// with a box up, or when the composer holds typed text.
#[must_use]
pub fn continuation_suggestion(rows: &[String], cursor_col: usize) -> Option<String> {
    composer_frame(rows)?;
    if parse_prompt(rows).is_some() || !is_placeholder(rows, cursor_col) {
        return None;
    }
    composer_text(rows).filter(|t| !t.is_empty())
}

/// Whether a `/goal` is armed: Claude Code's `◎ /goal active (3h)` indicator
/// in the live zone — between the status row (the last transcript row when
/// there is none) and the composer's top rule, or in the footer — against
/// the right edge, where it was measured (2026-09-21, right-aligned above
/// the rule). A worker's words quoting it are transcript: with no done row
/// under them they fall in the live zone too, but at the message's column,
/// so they do not count. Without the frame, `false`.
#[must_use]
pub fn goal_active(rows: &[String]) -> bool {
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let from = status_block(rows, frame.top).from;
    rows[from..frame.top]
        .iter()
        .chain(&rows[frame.bottom + 1..])
        .any(|r| {
            crate::text::trim_start(r).starts_with("◎ /goal active")
                && is_against_right_edge(r, width)
        })
}

/// Whether the agent's last words `said` ask: their last line ends in `?` —
/// how Codex's, agy's and Muse Code's grammars tell a question from an idle
/// point.
pub(crate) fn asks(said: &str) -> bool {
    said.lines()
        .last()
        .is_some_and(|l| crate::text::trim_end(l).ends_with('?'))
}

/// The worker's last words: the `⏺` message that holds the last thing said
/// ([`last_said_index`]), from its `⏺` row down to that row, the marker and
/// indentation stripped, blank rows and any `⎿` block under it (the
/// vendor's or a tool's rows) left out, one line per row. `None` when the
/// last thing said is not in a message — a user row, a tool call, a shell's
/// screen — so a stop-phrase check never reads anyone else's words.
///
/// A message TALLER than the screen has its `⏺` row scrolled off: every
/// visible row above the last said is indented (the message's continuation
/// column). Its tail is then the visible rows from the top — what the
/// worker ended on — never `None`, which a policy would read as "nothing
/// asked" and continue past `Should I drop the prod table…?` under 30 rows
/// of summary.
///
/// CLAUDE CODE'S OWN IDLE COMPACTION speaks after the worker and answers
/// nobody ([`idle_compacted`]): its notice, the attachments it restored
/// (`⎿  Read …`) and a `※ recap:` drawn before them are the vendor's, so the
/// last words are the worker's above them — a finished report still ends
/// with ATERM-TASK-DONE, a question held for a person still asks it.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let (head, said) = message_at(rows, last_said_index(rows)?)?;
    if !is_idle_notice(&said) {
        return Some(said);
    }
    let above = before_idle_compaction(rows, head);
    let last = (0..above)
        .rev()
        .find(|&i| crate::phase::is_said(&rows[i], None))?;
    message_at(rows, last).map(|(_, said)| said)
}

/// The `⏺` message whose last row is `last` ([`said_tail`]): its head's index
/// (`0` when the head is above the screen) and its words.
fn message_at(rows: &[String], last: usize) -> Option<(usize, String)> {
    let head = (0..=last).rev().find(|&i| {
        let r = &rows[i];
        !crate::text::trim(r).is_empty() && leading_spaces(r) == 0
    });
    let (head, headless) = match head {
        Some(h) => (h, false),
        // The message's head is above the screen: its visible rows, from
        // the top.
        None => (0, true),
    };
    if !headless && (!rows[head].starts_with(['⏺', '●']) || is_tool_call(&rows[head])) {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    let mut in_gutter = false;
    for (i, r) in rows.iter().enumerate().take(last + 1).skip(head) {
        let t = crate::text::trim(r);
        if t.is_empty() {
            in_gutter = false;
            continue;
        }
        if t.starts_with('⎿') {
            in_gutter = true;
            continue;
        }
        if in_gutter && leading_spaces(r) >= 5 {
            continue;
        }
        in_gutter = false;
        let t = if i == head && !headless {
            t.trim_start_matches(['⏺', '●']).trim_start()
        } else {
            t
        };
        out.push(t);
    }
    (!out.is_empty()).then(|| (head, out.join("\n")))
}

/// The words of Claude Code's idle compaction notice (anchor
/// `turn.idle_compacted`), the message whole — wrapped onto more rows in a
/// narrow window.
fn is_idle_notice(said: &str) -> bool {
    let notice = const { crate::anchors::anchor_text("turn.idle_compacted") };
    said.split_whitespace().eq(notice.split_whitespace())
}

/// Where the rows Claude Code appended with its idle compaction begin,
/// walking up from its notice's `⏺` row at `head`: blank rows, the
/// attachments it restored under the `⎿` gutter (their wrapped rows deeper
/// in), and a `※ recap:` (anchor `turn.recap`) with the rows wrapped under
/// it, drawn while the conversation sat idle.
fn before_idle_compaction(rows: &[String], head: usize) -> usize {
    let recap = const { crate::anchors::anchor_text("turn.recap") };
    let mut k = head;
    while k > 0 {
        let r = &rows[k - 1];
        let t = crate::text::trim(r);
        if t.is_empty() || t.starts_with('⎿') {
            k -= 1;
            continue;
        }
        // Indentation also belongs to the worker's own last words. Strip
        // wrapped attachment rows only when they lead back to a gutter
        // without crossing a blank, which ends that gutter in message_at.
        if leading_spaces(r) >= 5 {
            let gutter = (0..k).rev().find(|&i| {
                let text = crate::text::trim(&rows[i]);
                text.is_empty() || text.starts_with('⎿') || leading_spaces(&rows[i]) < 5
            });
            if let Some(gutter) = gutter.filter(|&i| crate::text::trim(&rows[i]).starts_with('⎿'))
            {
                k = gutter;
                continue;
            }
        }
        let top = (0..k)
            .rev()
            .find(|&i| leading_spaces(&rows[i]) == 0 && !crate::text::trim(&rows[i]).is_empty());
        match top {
            Some(i)
                if rows[i]
                    .strip_prefix('※')
                    .is_some_and(|rest| rest.trim_start().starts_with(recap)) =>
            {
                k = i;
            }
            _ => break,
        }
    }
    k
}

/// Whether CLAUDE CODE'S OWN HOUSEKEEPING ended the last turn: the last thing
/// said is the notice its idle compaction appends, `⏺ Compacted while idle,
/// before the prompt cache expired` (anchor `turn.idle_compacted`), the
/// message whole — wrapped onto more rows in a narrow window. Claude Code
/// compacts by itself only with nobody at the tab and no turn in flight, so
/// no message was answered there. Negative: the words inside a longer
/// message, a user row quoting them, a turn that ended after the notice, and
/// `Compacted after an hour idle, …`, which the compaction on a person's
/// return draws inside their own turn, ahead of the reply.
#[must_use]
pub fn idle_compacted(rows: &[String]) -> bool {
    last_said_index(rows)
        .and_then(|last| message_at(rows, last))
        .is_some_and(|(_, said)| is_idle_notice(&said))
}

/// Whether a PERSON stopped the last turn: the vendor's `⎿  Interrupted ·
/// What should Claude do instead?` row (anchor `turn.interrupted`) under
/// the transcript's last block — the last `⏺` message or tool row, with
/// no user `❯` row after it — above the live zone. A turn stopped so is a
/// stop, never a point to continue: the person is at the keyboard and the
/// next message is theirs, never a `keep going`. Negative: the same words
/// in a message, or an interrupt a new user message has answered.
///
/// A question dialog a person declined — Esc on it, or `2. Cancel` on its
/// review tab — is the same stop: the last block is `⏺ User declined to
/// answer questions` (anchor `question.declined`; measured 2026-09-25, S7
/// and S8), no `Interrupted ·` row under it, and the tool result tells the
/// model to STOP and wait for the person: read as no interrupt, a turn-end
/// policy would type `keep going` over the person's refusal. Negative: the
/// words inside a longer message row.
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    let end = transcript_end(rows).min(rows.len());
    let Some(block) = (0..end)
        .rev()
        .find(|&i| rows[i].starts_with(['⏺', '●', '❯']))
    else {
        return false;
    };
    let said = rows[block].trim_start_matches(['⏺', '●']).trim();
    if rows[block].starts_with(['⏺', '●'])
        && said == const { crate::anchors::anchor_text("question.declined") }
    {
        return true;
    }
    let text = const { crate::anchors::anchor_text("turn.interrupted") };
    rows[block..end].iter().any(|r| {
        crate::text::trim_start(r)
            .strip_prefix('⎿')
            .is_some_and(|rest| crate::text::trim_start(rest).starts_with(text))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::fixtures::{
        BOX_BASH_TOUCH, END_529, END_OFFER, GOAL_ACTIVE_SUGGESTION, composer, rows, screen,
    };

    fn framed(body: &[&str], footer: &str) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer(footer));
        r
    }

    /// The measured screen: the dim `keep going` with the cursor at column 2
    /// is the suggestion; the same text with the cursor past it is typed.
    #[test]
    fn the_dim_suggestion_is_read_and_a_typed_draft_is_not() {
        let r = screen(GOAL_ACTIVE_SUGGESTION);
        assert_eq!(
            continuation_suggestion(&r, 2).as_deref(),
            Some("keep going")
        );
        assert_eq!(continuation_suggestion(&r, 12), None, "typed text");
        // A box's option row and a user row are never a suggestion: no frame.
        let b = screen(BOX_BASH_TOUCH);
        assert_eq!(continuation_suggestion(&b, 2), None);
        // An empty composer suggests nothing.
        let idle = framed(&["⏺ Done.", ""], "  ? for shortcuts");
        assert_eq!(continuation_suggestion(&idle, 2), None);
    }

    #[test]
    fn goal_active_is_read_from_the_live_zone_only() {
        assert!(goal_active(&screen(GOAL_ACTIVE_SUGGESTION)));
        let quoted = framed(
            &[
                "⏺ The indicator reads",
                "  ◎ /goal active (3h)",
                "  when a goal is armed.",
                "",
                "✻ Worked for 3s · done 9:01 AM",
                "",
            ],
            "  ? for shortcuts",
        );
        assert!(!goal_active(&quoted), "quoted in the transcript");
        assert!(!goal_active(&screen(END_OFFER)));
        // No done row under the quote: the message's rows reach the live
        // zone, and still do not count — they sit at the message's column.
        for body in [
            &[
                "⏺ The manager's footer read:",
                "",
                "  ◎ /goal active (3h)",
                "",
            ][..],
            &["⏺ The manager's footer read:", "  ◎ /goal active (3h)", ""],
        ] {
            assert!(!goal_active(&framed(body, "  ? for shortcuts")), "{body:?}");
        }
        // The control: the indicator against the right edge over the rule.
        let right = format!("{}◎ /goal active (3h)", " ".repeat(99));
        let armed = framed(&["⏺ Done.", "", &right], "  ? for shortcuts");
        assert!(goal_active(&armed));
    }

    #[test]
    fn said_tail_is_the_last_message_without_its_gutter() {
        assert_eq!(
            said_tail(&screen(END_OFFER)).as_deref(),
            Some(
                "Done: 12 of 67 divisions now solve. Next: the FP-sorted binder path; want me to take that on?"
            )
        );
        // The 529 row under the message is the vendor's, not the worker's.
        assert_eq!(
            said_tail(&screen(END_529)).as_deref(),
            Some("Suites are running; I'll report the number when it lands.")
        );
        let para = framed(
            &[
                "⏺ Merged the fix.",
                "",
                "  Next I would port the reader. Shall I go on?",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(
            said_tail(&para).as_deref(),
            Some("Merged the fix.\nNext I would port the reader. Shall I go on?")
        );
        // Negative controls: a user row, a tool call's output.
        let user = framed(&["❯ carry on with the parser", ""], "  ? for shortcuts");
        assert_eq!(said_tail(&user), None);
        let tool = framed(
            &["⏺ Bash(cargo test)", "  ⎿  test result: ok", ""],
            "  ? for shortcuts",
        );
        assert_eq!(said_tail(&tool), None);
    }

    /// A message taller than the screen: its `⏺` row scrolled off, every
    /// visible row indented. The tail is the visible rows — the question the
    /// worker ended on is read. The
    /// measured goal fixture (row 0 `  with zero hooks installed…`) reads
    /// its last words too. Negative control: a column-0 user row above
    /// still ends the message there.
    #[test]
    fn a_message_whose_head_scrolled_off_still_has_a_tail() {
        let mut body: Vec<String> = (0..30)
            .map(|i| format!("  - finding {i}: details"))
            .collect();
        body.push(String::new());
        body.push("  Should I drop the prod table or keep it?".to_string());
        body.push(String::new());
        body.push("✻ Cooked for 3m 2s · done 4:24 PM".to_string());
        body.push(String::new());
        let refs: Vec<&str> = body.iter().map(String::as_str).collect();
        let long = framed(&refs, "  ? for shortcuts");
        let tail = said_tail(&long).expect("a tail");
        assert!(tail.starts_with("- finding 0: details"), "{tail}");
        assert!(
            tail.ends_with("Should I drop the prod table or keep it?"),
            "{tail}"
        );
        let goal = said_tail(&screen(GOAL_ACTIVE_SUGGESTION)).expect("the goal fixture's tail");
        assert!(goal.starts_with("with zero hooks installed."), "{goal}");
        let user = framed(
            &[
                "❯ summarise",
                "  - finding 1: details",
                "  Should I drop it?",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(said_tail(&user), None);
    }

    /// The measured interrupt row, under a tool row and under a message
    /// row, reads as a person's stop. Negative controls: the words in the
    /// worker's own message, and a user message typed after the interrupt.
    #[test]
    fn a_persons_interrupt_is_read_under_the_last_block_only() {
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let tool = framed(
            &[
                "⏺ Running the schema migration against the staging database now.",
                "",
                "⏺ Bash(./migrate.sh --env staging)",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
            ],
            foot,
        );
        assert!(interrupted(&tool));
        let message = framed(
            &[
                "⏺ Running the schema migration against the staging database now.",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
            ],
            foot,
        );
        assert!(interrupted(&message));
        let quoted = framed(
            &[
                "⏺ The vendor prints Interrupted · What should Claude do instead? on Esc.",
                "",
            ],
            foot,
        );
        assert!(!interrupted(&quoted));
        let answered = framed(
            &[
                "⏺ Bash(./migrate.sh --env staging)",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
                "❯ run it against the scratch database instead",
                "",
                "⏺ Running it against the scratch database.",
                "",
            ],
            foot,
        );
        assert!(!interrupted(&answered));
    }

    /// R12 (the critique of 2026-09-25): a question dialog a person declined
    /// — `2. Cancel` on its review tab (S7-03), Esc on it (S8-02), both
    /// MEASURED — ends the turn on `⏺ User declined to answer questions`
    /// with no `Interrupted ·` row, and reads as a person's stop. Negative
    /// controls: the words inside a longer message row, and a decline a new
    /// user message has answered.
    #[test]
    fn a_declined_question_is_a_persons_stop() {
        use crate::prompt::fixtures::{QUESTION_DECLINED_CANCEL, QUESTION_DECLINED_ESC, screen};
        for text in [QUESTION_DECLINED_CANCEL, QUESTION_DECLINED_ESC] {
            let r = screen(text);
            assert!(interrupted(&r), "{r:?}");
            assert_eq!(crate::phase::worker_phase(&r), crate::phase::Phase::Idle);
        }
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let quoted = framed(
            &[
                "⏺ If you press Esc, Claude Code prints User declined to answer questions.",
                "",
            ],
            foot,
        );
        assert!(!interrupted(&quoted));
        let answered = framed(
            &[
                "⏺ User declined to answer questions",
                "  ⎿  · Which shell should the demo target? (zsh (Recommended) / bash)",
                "",
                "❯ use zsh",
                "",
                "⏺ Using zsh.",
                "",
            ],
            foot,
        );
        assert!(!interrupted(&answered));
    }

    /// Claude Code's IDLE COMPACTION is its own housekeeping: the measured
    /// screen (an hour after a finished report, nobody at the tab) and the
    /// notice wrapped in a narrow pane are read so, through the Claude
    /// reader too. NEGATIVE CONTROLS: the words inside a longer message, a
    /// user row quoting them, a turn that ended after the notice, the
    /// on-return compaction's notice, and the generic and Codex readers.
    #[test]
    fn claude_codes_idle_compaction_is_its_own_housekeeping() {
        use crate::prompt::fixtures::IDLE_COMPACTED_MEASURED;
        use crate::reader::Program;
        let measured = screen(IDLE_COMPACTED_MEASURED);
        assert!(idle_compacted(&measured));
        assert!(Program::Claude.reader().housekeeping(&measured));
        // The worker's last words are read through it: the report's tail.
        assert_eq!(said_tail(&measured).as_deref(), Some("ATERM-TASK-DONE"));
        assert_eq!(
            crate::phase::worker_phase(&measured),
            crate::phase::Phase::Idle
        );
        for p in [Program::Generic, Program::Codex] {
            assert!(!p.reader().housekeeping(&measured), "{p:?}");
        }
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let narrow = framed(
            &[
                "⏺ Compacted while idle, before the prompt",
                "  cache expired",
                "",
            ],
            foot,
        );
        assert!(idle_compacted(&narrow));
        for body in [
            &[
                "⏺ Compacted while idle, before the prompt cache expired, so the next",
                "  turn starts from the summary.",
                "",
            ][..],
            &[
                "❯ Compacted while idle, before the prompt cache expired",
                "",
            ],
            &[
                "⏺ Compacted while idle, before the prompt cache expired",
                "",
                "❯ keep going",
                "",
                "⏺ Nothing is left to do.",
                "",
            ],
            &[
                "⏺ Compacted after an hour idle, using a summary written while you were away",
                "",
            ],
        ] {
            assert!(!idle_compacted(&framed(body, foot)), "{body:?}");
        }
    }

    /// The worker's LAST WORDS ARE READ THROUGH CLAUDE CODE'S IDLE
    /// COMPACTION — its notice, the attachments it restored and a recap drawn
    /// before them (the shape of the owner's tab 1, 2026-10-15) — so a
    /// finished report and a question held for a person are what the last
    /// turn left. Nothing of the worker's above them: no words, as a tool
    /// call there has none.
    #[test]
    fn the_last_words_are_read_through_an_idle_compaction() {
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        let compacted = |said: &[&str]| {
            let mut body = said.to_vec();
            body.extend([
                "",
                "✻ Worked for 57s · done 4:28 PM",
                "",
                "※ recap: You wanted the auto-prompting fixed, and that fix is now on main. Next is",
                "  your call: should I also retire the token? (disable recaps in /config)",
                "  ⎿  Read ../.claude/projects/-Users-user00-aterm/memory/notes.md (33 lines)",
                "  ⎿  Referenced file ../../../private/tmp/claude-1000/-Users-user00-aterm/scratchpad/",
                "     design-final.md",
                "  ⎿  Skills restored (workflow-authoring)",
                "",
                "⏺ Compacted while idle, before the prompt cache expired",
                "",
            ]);
            framed(&body, foot)
        };
        let report = compacted(&[
            "⏺ Merged the fix; the checks pass.",
            "",
            "  ATERM-TASK-DONE",
        ]);
        assert!(idle_compacted(&report));
        assert_eq!(
            said_tail(&report).as_deref(),
            Some("Merged the fix; the checks pass.\nATERM-TASK-DONE")
        );
        let held = compacted(&["⏺ Shall I drop the old table?"]);
        assert_eq!(
            said_tail(&held).as_deref(),
            Some("Shall I drop the old table?")
        );
        let tool = compacted(&["⏺ Bash(targo --unverified test)", "  ⎿  ok"]);
        assert_eq!(said_tail(&tool), None);
        let alone = framed(
            &[
                "  ⎿  Skills restored (workflow-authoring)",
                "",
                "⏺ Compacted while idle, before the prompt cache expired",
                "",
            ],
            foot,
        );
        assert!(idle_compacted(&alone));
        assert_eq!(said_tail(&alone), None);
    }

    #[test]
    fn idle_compaction_keeps_an_indented_worker_tail() {
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        for tail in ["ATERM-TASK-DONE", "Should I remove the old table?"] {
            for restored in [
                &[][..],
                &["  ⎿  Read notes.md", "     wrapped attachment path"][..],
                &[
                    "※ recap: The change is ready.",
                    "  It is waiting for you.",
                    "  ⎿  Read notes.md",
                    "     wrapped attachment path",
                ][..],
            ] {
                let indented = format!("      {tail}");
                let mut body = vec!["⏺ Finished the review.", indented.as_str(), ""];
                body.extend_from_slice(restored);
                body.extend([
                    "",
                    "⏺ Compacted while idle, before the prompt cache expired",
                    "",
                ]);
                let rows = framed(&body, foot);
                assert!(idle_compacted(&rows));
                assert_eq!(
                    said_tail(&rows).as_deref(),
                    Some(format!("Finished the review.\n{tail}").as_str()),
                    "{restored:?}"
                );
            }
        }
        // A blank ends the preceding tool gutter, including when the
        // worker's message head is above the visible screen.
        for head in ["⏺ Finished the review.", "      Finished the review."] {
            let rows = framed(
                &[
                    head,
                    "  ⎿  tool output",
                    "",
                    "      ATERM-TASK-DONE",
                    "",
                    "⏺ Compacted while idle, before the prompt cache expired",
                    "",
                ],
                foot,
            );
            assert_eq!(
                said_tail(&rows).as_deref(),
                Some("Finished the review.\nATERM-TASK-DONE"),
                "{head}"
            );
        }
    }

    /// A brand-new session is FRESH: the launch card over the composer, no
    /// message and nothing said (the E2E probe of 2026-09-25: it was
    /// continued). NEGATIVE CONTROLS: the same launch after a first message
    /// (in its first turn), after an answer, and a screen with no card (the
    /// card scrolled away) are not.
    #[test]
    fn a_launch_with_no_turn_is_fresh() {
        use crate::prompt::fixtures::{first_turn_under_a_table, launch_under_a_table};
        let launch = launch_under_a_table("❯ Try \"refactor <filepath>\"", "  ? for shortcuts");
        assert!(fresh(&launch), "{launch:#?}");
        assert!(!fresh(&first_turn_under_a_table(&["❯ then add tests"])));
        let mut answered = launch_under_a_table("❯", "  ? for shortcuts");
        let top = answered.len() - 3;
        for (k, r) in ["❯ fix the parser", "", "⏺ Fixed.", ""].iter().enumerate() {
            answered.insert(top + k, (*r).to_string());
        }
        assert!(!fresh(&answered), "{answered:#?}");
        assert!(!fresh(&framed(&["⏺ Done.", ""], "  ? for shortcuts")));
        assert!(!fresh(&screen(END_OFFER)));
    }

    /// A CONVERSATION A `/clear` JUST STARTED IS FRESH (W4 of ATERM-KI-1015-1,
    /// the lab on Claude Code 2.1.300): the card redrawn over `❯ /clear` and
    /// nothing else. NEGATIVE CONTROLS: a turn asked since the `/clear`, and
    /// another local command in its place (`❯ /usage`, a turn's history
    /// under the card).
    #[test]
    fn a_conversation_a_clear_just_started_is_fresh() {
        let cleared = measured("claude-2.1.300-cleared-at-armed-limit.txt");
        assert!(fresh(&cleared), "{cleared:#?}");
        let clear = cleared
            .iter()
            .position(|r| r == "❯ /clear")
            .expect("the /clear row");
        let mut asked = cleared.clone();
        asked.splice(
            clear + 1..clear + 1,
            ["", "❯ fix the parser"].map(str::to_string),
        );
        assert!(!fresh(&asked), "{asked:#?}");
        let mut usage = cleared.clone();
        usage[clear] = "❯ /usage".to_string();
        assert!(!fresh(&usage), "{usage:#?}");
    }

    /// A measured screen from `src/fixtures`, its `#` header dropped.
    fn measured(name: &str) -> Vec<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect()
    }

    /// THE LAUNCH CARD, over the measured 2.1.283 screens: the owner's fresh
    /// bypass screen names `Opus 5.5` at `xhigh` even with its `⏺ agents-md`
    /// notice between the card and the composer (which [`fresh`] refuses);
    /// a model with no effort levels names none. NEGATIVE CONTROLS: a user
    /// message under the card, a box over the composer, a model row cut or
    /// split for a narrow pane, and a newer card below the frame.
    #[test]
    fn the_launch_card_names_the_starting_model() {
        assert_eq!(
            launch_card(&measured("claude-2.1.283-footer-bypass-idle.txt")),
            Some(LaunchCard {
                version: "2.1.283".into(),
                model: "Opus 5.5".into(),
                effort: Some("xhigh".into()),
            })
        );
        assert!(!fresh(&measured("claude-2.1.283-footer-bypass-idle.txt")));
        assert_eq!(
            launch_card(&measured("claude-2.1.283-launch-repl-ready.txt")),
            Some(LaunchCard {
                version: "2.1.283".into(),
                model: "Haiku 4.5".into(),
                effort: None,
            })
        );
        // The inline renderer relaunched in the same tab: the NEWEST card,
        // the one above the live frame, is read — never its predecessor's.
        let mut relaunch = measured("claude-2.1.283-inline-relaunch-repl-ready.txt");
        let first = relaunch
            .iter()
            .position(|r| r.contains("Claude Code v"))
            .unwrap();
        relaunch[first + 1] = "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5 \u{00B7} Claude Max".into();
        assert_eq!(
            launch_card(&relaunch).map(|c| c.model).as_deref(),
            Some("Haiku 4.5")
        );
        // A newer card BELOW the frame: that frame is a predecessor's.
        let mut below = measured("claude-2.1.283-launch-repl-ready.txt");
        let card: Vec<String> = below
            .iter()
            .filter(|r| r.contains("Claude Code v") || r.contains("Claude Max"))
            .cloned()
            .collect();
        below.extend(card);
        assert_eq!(launch_card(&below), None);
        // A user message between the card and the frame.
        assert_eq!(
            launch_card(&measured("claude-2.1.283-footer-busy.txt")),
            None
        );
        // A box over the composer: no frame, no card.
        assert_eq!(
            launch_card(&measured("claude-2.1.283-effort-switch.txt")),
            None
        );
        // Cut or split for a narrow pane.
        let base = measured("claude-2.1.283-footer-bypass-idle.txt");
        let at = base.iter().position(|r| r.contains("Claude Max")).unwrap();
        for cut in [
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 with xh\u{2026}",
            "\u{259D}\u{259C}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}  Opus 5.5 with xhigh effort",
        ] {
            let mut narrow = base.clone();
            narrow[at] = cut.into();
            assert_eq!(launch_card(&narrow), None, "{cut}");
        }
    }

    /// Every recorded screen: a card that is read always names the build
    /// its fixture was captured on, and a non-empty model.
    #[test]
    fn every_read_card_names_its_fixtures_build() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
        let mut read = 0;
        for entry in std::fs::read_dir(&dir).expect("the fixtures") {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let Some(header) = text.lines().next() else {
                continue;
            };
            let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
            let Some(card) = launch_card(&rows) else {
                continue;
            };
            assert!(
                header.contains(&format!("claude-code {}", card.version)),
                "{}: {card:?} under {header:?}",
                path.display()
            );
            assert!(!card.model.is_empty());
            read += 1;
        }
        assert!(read >= 5, "the corpus must exercise the reader: {read}");
    }

    /// The three rows of the measured 2.1.294 card (144 columns).
    fn card_rows() -> Vec<String> {
        measured("claude-2.1.294-session-limit-then-local-commands.txt")[..3].to_vec()
    }

    /// The build a capture's header names: the whole whitespace-delimited
    /// token after `claude-code ` (`# claude-code 2.1.300 · MEASURED …`).
    fn header_version(header: &str) -> Option<&str> {
        header
            .split_once("claude-code ")?
            .1
            .split_whitespace()
            .next()
    }

    /// THE CARD'S PLACE, over every recorded screen that draws the banner
    /// (149 when this was written): each three-row card is placed on the
    /// newest banner row with its text at column 11, its mascot ending at 9
    /// and its banner's text at 31; each narrow pane's four-row card (the
    /// `*-34col`, `*-36col` and `*-40col` captures: main's 2.1.300 survey at
    /// 40 columns draws its banner on a row of its own too) is refused.
    #[test]
    fn every_recorded_card_is_placed_or_refused_by_its_layout() {
        let banner = crate::anchors::anchor_text("launch.banner");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .expect("the fixtures")
            .map(|e| e.expect("a fixture").path())
            .collect();
        paths.sort();
        let (mut placed, mut narrow) = (0, 0);
        for path in paths {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let rows = crate::prompt::fixtures::screen(&text);
            if !rows.iter().any(|r| r.contains(banner)) {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let place = launch_card_place(&rows);
            if ["-34col.txt", "-36col.txt", "-40col.txt"]
                .iter()
                .any(|narrow| name.ends_with(narrow))
            {
                assert_eq!(place, None, "{name}: the narrow card");
                narrow += 1;
                continue;
            }
            let place = place.unwrap_or_else(|| panic!("{name}: no card placed"));
            let newest = rows
                .iter()
                .rposition(|r| r.starts_with(" \u{2590}") && r.contains(banner))
                .unwrap();
            assert_eq!(
                (
                    place.top,
                    place.rows,
                    place.text_col,
                    place.mascot_end,
                    place.ends[0]
                ),
                (newest, 3, 11, 9, 31),
                "{name}"
            );
            assert!(place.ends[1..].iter().all(|&e| e > 11), "{name}: {place:?}");
            placed += 1;
        }
        assert!(
            placed >= 144 && narrow >= 5,
            "the corpus must exercise the reader: {placed} placed, {narrow} narrow"
        );
    }

    /// The places measured independently of this reader (a script over the
    /// fixture files, 2026-10-15, display columns, every card row narrow):
    /// the newest card's top row and each row's text end — the two-card
    /// relaunch screens' NEWER card among them.
    #[test]
    fn the_card_is_placed_where_it_was_measured() {
        for (name, top, ends) in [
            ("claude-2.1.280-box-bash-touch.txt", 1, [31, 33, 108]),
            ("claude-2.1.281-ask-multi.txt", 0, [31, 33, 49]),
            ("claude-2.1.282-question-afk.txt", 1, [31, 33, 122]),
            (
                "claude-2.1.283-api-error-enotfound-measured.txt",
                3,
                [31, 52, 112],
            ),
            ("claude-2.1.283-footer-bypass-idle.txt", 1, [31, 50, 24]),
            (
                "claude-2.1.283-ctrlc-relaunch-stale-question.txt",
                40,
                [31, 33, 60],
            ),
            (
                "claude-2.1.283-exit-relaunch-stale-footer.txt",
                25,
                [31, 33, 60],
            ),
            (
                "claude-2.1.283-inline-relaunch-repl-ready.txt",
                16,
                [31, 33, 126],
            ),
            ("claude-2.1.286-compact-failed-first.txt", 0, [31, 39, 36]),
            (
                "claude-2.1.294-session-limit-then-local-commands.txt",
                0,
                [31, 34, 36],
            ),
            ("claude-2.1.295-question-one-44col.txt", 0, [31, 39, 36]),
        ] {
            assert_eq!(
                launch_card_place(&measured(name)),
                Some(CardPlace {
                    top,
                    rows: 3,
                    text_col: 11,
                    mascot_end: 9,
                    ends,
                }),
                "{name}"
            );
        }
    }

    /// The inline renderer relaunched in the same tab: the NEWER card is
    /// placed (row 16), the predecessor's (row 3) only once the newer one is
    /// off the screen. A newer card cut by the screen's bottom edge is
    /// refused, never passed over for the predecessor's.
    #[test]
    fn a_relaunch_places_the_newer_card_and_never_falls_back() {
        let relaunch = measured("claude-2.1.283-inline-relaunch-repl-ready.txt");
        assert_eq!(launch_card_place(&relaunch).map(|p| p.top), Some(16));
        assert_eq!(launch_card_place(&relaunch[..16]).map(|p| p.top), Some(3));
        for cut in [17, 18] {
            assert_eq!(launch_card_place(&relaunch[..cut]), None, "rows[..{cut}]");
        }
    }

    /// The card stays placed after a person types: a user message under it
    /// (where [`launch_card`] reads nothing), one that NAMES the card, and a
    /// box over the composer.
    #[test]
    fn the_card_is_placed_under_a_persons_rows() {
        let busy = measured("claude-2.1.283-footer-busy.txt");
        assert_eq!(launch_card(&busy), None);
        assert_eq!(launch_card_place(&busy).map(|p| p.top), Some(1));
        let mut named = card_rows();
        named.extend(rows(&[
            "",
            "❯ what changed in Claude Code v2.1.284?",
            "",
            "⏺ Claude Code v2.1.284 added the drafts card.",
            "",
        ]));
        assert_eq!(launch_card_place(&named).map(|p| p.top), Some(0));
        let boxed = measured("claude-2.1.283-effort-switch.txt");
        assert_eq!(launch_card(&boxed), None);
        assert_eq!(launch_card_place(&boxed).map(|p| p.top), Some(1));
    }

    /// A card QUOTED — a tool's output under `⎿`, indented to a message's
    /// column or to a tool output's — is not a card: alone it places
    /// nothing, and under a real card it is passed over for that card.
    #[test]
    fn a_quoted_card_is_not_a_card() {
        let card = card_rows();
        let under_gutter = vec![
            "⏺ Bash(head -3 card.txt)".to_string(),
            format!("  ⎿  {}", card[0]),
            format!("     {}", card[1]),
            format!("     {}", card[2]),
        ];
        let indent = |by: usize| -> Vec<String> {
            let mut r = vec!["⏺ The card read:".to_string(), String::new()];
            r.extend(card.iter().map(|c| format!("{}{c}", " ".repeat(by))));
            r
        };
        for (what, quote) in [
            ("under the gutter", under_gutter),
            ("at a message's column", indent(2)),
            ("at a tool output's column", indent(5)),
        ] {
            assert_eq!(launch_card_place(&quote), None, "{what}: {quote:#?}");
            let mut under = card.clone();
            under.push(String::new());
            under.extend(quote);
            assert_eq!(
                launch_card_place(&under).map(|p| p.top),
                Some(0),
                "{what}: {under:#?}"
            );
        }
    }

    /// The narrow pane's four-row card is refused, alone and as the newest
    /// card under a three-row one (which a finished process drew) — and so
    /// is a narrow banner whose rows under it did not draw as measured: cut
    /// by the screen's bottom edge, over a blank row, over the mascot alone,
    /// over a row without the mascot. None of them falls back to the older
    /// card above. A bare banner row at any column but the narrow card's (a
    /// message's line, a narrow card quoted in a message or a tool's output)
    /// is passed over for the card above.
    #[test]
    fn the_narrow_card_is_refused() {
        let narrow = measured("claude-2.1.300-prose-opens-with-limit-words-34col.txt");
        let banner = narrow[0].clone();
        assert_eq!(
            banner.trim(),
            "Claude Code v2.1.300",
            "the fixture's banner row"
        );
        let over = |tail: &[&str]| {
            let mut r = vec![banner.clone()];
            r.extend(rows(tail));
            r
        };
        for (what, newest) in [
            ("the whole narrow card", narrow.clone()),
            ("cut to its banner row", narrow[..1].to_vec()),
            ("over one blank row", over(&[""])),
            (
                "over the mascot alone",
                over(&[" \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}"]),
            ),
            (
                "over a row without the mascot",
                over(&["           Opus 5.5"]),
            ),
            ("with trailing blanks", vec![format!("{banner}      ")]),
        ] {
            assert_eq!(launch_card_place(&newest), None, "{what}: {newest:#?}");
            let mut both = card_rows();
            both.extend(rows(&["", "❯ exit", ""]));
            both.extend(newest);
            assert_eq!(
                launch_card_place(&both),
                None,
                "under a card, {what}: {both:#?}"
            );
        }
        // A message's words AFTER the version, at a narrow banner's indent,
        // are passed over for the card above — unless a mascot row under
        // them confirms the narrow layout.
        let mut words = card_rows();
        words.extend(rows(&[
            "",
            "  Claude Code v2.1.284 added the drafts card.",
            "",
        ]));
        assert_eq!(
            launch_card_place(&words).map(|p| p.top),
            Some(0),
            "{words:#?}"
        );
        let mut confirmed = card_rows();
        confirmed.push(String::new());
        confirmed.push(format!("{banner}  (latest)"));
        confirmed.extend(narrow[1..].iter().cloned());
        assert_eq!(launch_card_place(&confirmed), None, "{confirmed:#?}");

        // A bare banner row is the narrow card's only at the narrow card's
        // column (measured 11, 9..=12 accepted): there it is refused, and
        // anywhere else — a message's line at 2, a narrow card quoted at a
        // message's column (13) or in a tool's output (16), and the columns
        // just outside the range — it is passed over for the card above.
        let bare = banner.trim_start();
        for (col, refused) in [
            (2, false),
            (8, false),
            (9, true),
            (11, true),
            (12, true),
            (13, false),
            (16, false),
        ] {
            let mut under = card_rows();
            under.extend(rows(&["", "⏺ Done.", ""]));
            under.push(format!("{}{bare}", " ".repeat(col)));
            assert_eq!(
                launch_card_place(&under).map(|p| p.top),
                (!refused).then_some(0),
                "a bare banner at column {col}: {under:#?}"
            );
        }
        // A Bash tool's output quoting the whole narrow card, its banner a
        // row under the output's first (so at column 16, without the `⎿`):
        // the live wide card above keeps its decoration.
        let mut quoted = card_rows();
        quoted.extend(rows(&[
            "",
            "❯ show me the narrow card",
            "",
            "⏺ Bash(cat narrow-card.txt)",
            "  ⎿  captured at 34 columns:",
        ]));
        quoted.extend(narrow[..4].iter().map(|r| format!("     {r}")));
        assert_eq!(
            quoted[8]
                .find(bare)
                .map(|at| quoted[8][..at].chars().count()),
            Some(16),
            "the quoted banner's column: {quoted:#?}"
        );
        assert_eq!(
            launch_card_place(&quoted).map(|p| p.top),
            Some(0),
            "{quoted:#?}"
        );
    }

    /// The placed card's version is its banner's, the build the capture's
    /// header names (its whole token), over every three-row card in the
    /// corpus; a place that does not name a card's banner row — a row that
    /// merely names a version, without the mascot or off the text column —
    /// or a banner whose version is not three numeric parts, yields `None`.
    #[test]
    fn the_placed_cards_version_is_its_banners() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
        let mut read = 0;
        for entry in std::fs::read_dir(&dir).expect("the fixtures") {
            let path = entry.expect("a fixture").path();
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let Some((header, _)) = text.split_once('\n') else {
                continue;
            };
            let rows = crate::prompt::fixtures::screen(&text);
            let Some(place) = launch_card_place(&rows) else {
                continue;
            };
            let version = launch_card_version(&rows, &place)
                .unwrap_or_else(|| panic!("{}: no version", path.display()));
            assert_eq!(
                header_version(header),
                Some(version),
                "{}: {version:?} under {header:?}",
                path.display()
            );
            read += 1;
        }
        assert!(read >= 144, "the corpus must exercise the reader: {read}");
        // The header's token is compared whole: a version cut short (or run
        // on) is not the header's build.
        let header = "# claude-code 2.1.300 \u{00B7} MEASURED 2026-10-14";
        assert_eq!(header_version(header), Some("2.1.300"));
        for wrong in ["2.1.30", "2.1.3000", "2.1"] {
            assert_ne!(header_version(header), Some(wrong), "{wrong}");
        }

        let card = card_rows();
        let place = launch_card_place(&card).expect("the measured card");
        assert_eq!(launch_card_version(&card, &place), Some("2.1.294"));
        // The place read from other rows: off their end, or on a row
        // without the banner.
        assert_eq!(launch_card_version(&card[..0], &place), None);
        let shifted = CardPlace { top: 1, ..place };
        assert_eq!(launch_card_version(&card, &shifted), None);
        // A row that NAMES a version at `place.top` (the screen moved under
        // a place read earlier) names no build: a person's row — even one
        // holding the banner at the text column — the banner at the text
        // column without the mascot, and a mascot row whose banner is not
        // at the text column.
        let banner = crate::anchors::anchor_text("launch.banner");
        let at_text_col = "\u{276F} is it at Claude Code v2.1.284";
        assert_eq!(
            at_text_col
                .find(banner)
                .map(|at| at_text_col[..at].chars().count()),
            Some(place.text_col),
            "the person's row holds the banner at the text column"
        );
        for named in [
            "\u{276F} what changed in Claude Code v2.1.284?",
            at_text_col,
            "           Claude Code v2.1.284",
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}     Claude Code v2.1.284",
        ] {
            assert!(
                named.contains(&format!("{banner}2.1.284")),
                "the row names a version: {named:?}"
            );
            let mut edited = card.clone();
            edited[0] = named.to_string();
            assert_eq!(launch_card_version(&edited, &place), None, "{named:?}");
        }
        for banner in [
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1",
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.",
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2..300",
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.3.0",
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Claude Code v2.1.1234567",
        ] {
            let mut edited = card.clone();
            edited[0] = banner.to_string();
            assert_eq!(launch_card_version(&edited, &place), None, "{banner:?}");
        }
    }

    /// Empty and garbage screens place nothing, and so does a card whose
    /// rows under the banner are not the measured ones.
    #[test]
    fn garbage_places_nothing() {
        let card = card_rows();
        let edited = |k: usize, row: &str| {
            let mut r = card.clone();
            r[k] = row.to_string();
            r
        };
        for screen in [
            Vec::new(),
            rows(&[""]),
            rows(&["Claude Code v"]),
            rows(&[" \u{2590}Claude Code v2.1.300", " \u{2590}x", " \u{2590}x"]),
            rows(&["\u{2590}", "\u{2580}", "\u{259F}"]),
            card[..1].to_vec(),
            card[..2].to_vec(),
            // The model row without its mascot, its text a column off, and
            // a block glyph where its text belongs.
            edited(1, "           Sonnet 5.5 · Claude API"),
            edited(1, "▝▜██████▀   Sonnet 5.5 · Claude API"),
            edited(1, "▝▜██████▀  ▀Sonnet 5.5"),
            // The mascot against the text, and the feet row cut short.
            edited(2, " ▝▝   ▝▝  ▝/private/tmp/at.0000/proj"),
            edited(2, " ▝▝   ▝▝"),
        ] {
            assert_eq!(launch_card_place(&screen), None, "{screen:#?}");
        }
    }

    /// A READING KEPT WITHOUT A SCAN IS THE SCAN'S (`launch_card_unmoved`):
    /// over every recorded screen that draws the banner, each row in turn
    /// rewritten — blanked, a person's words, a bare narrow banner, a newer
    /// three-row card (its banner over the row under it), a card's model row
    /// alone — whenever the check keeps the old reading, scanning the new rows
    /// gives exactly it. The sweep is not vacuous: rows above the card and
    /// plain words below it keep the reading (most rewrites), and a newer card
    /// below, a narrow banner below and an edit of the card's own rows do not.
    /// CONTROL: dropping either half of the check — the card's own rows, or
    /// the row over a changed one — keeps readings the scan disagrees with
    /// (mutation-checked).
    #[test]
    fn a_kept_card_reading_is_the_scans() {
        let anchor = crate::anchors::anchor_text("launch.banner");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .expect("the fixtures")
            .map(|e| e.expect("a fixture").path())
            .collect();
        paths.sort();
        let card = card_rows();
        let rewrites: [&[String]; 5] = [
            &rows(&[""]),
            &rows(&["\u{276F} what changed in Claude Code v2.1.284?"]),
            &rows(&["           Claude Code v2.1.299"]),
            &card[..2],
            &card[1..2],
        ];
        let (mut kept, mut moved, mut screens) = (0, 0, 0);
        let (mut newer_below, mut own_rows) = (0, 0);
        for path in paths {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let before = crate::prompt::fixtures::screen(&text);
            if !before.iter().any(|r| r.contains(anchor)) {
                continue;
            }
            screens += 1;
            let reading = launch_card_scan(&before);
            for at in 0..before.len() {
                for rewrite in rewrites {
                    let mut after = before.clone();
                    for (k, row) in rewrite.iter().enumerate() {
                        if let Some(slot) = after.get_mut(at + k) {
                            slot.clone_from(row);
                        }
                    }
                    let changed: Vec<usize> = (0..after.len())
                        .filter(|&i| after[i] != before[i])
                        .collect();
                    let scan = launch_card_scan(&after);
                    if launch_card_unmoved(&after, reading.0, &changed) {
                        assert_eq!(scan, reading, "{}: rows {at}.. {rewrite:?}", path.display());
                        kept += 1;
                    } else {
                        moved += 1;
                        let top = reading.0.unwrap_or(0);
                        if at > top && scan != reading {
                            newer_below += 1;
                        }
                        if changed.iter().any(|j| (top..top + 3).contains(j)) {
                            own_rows += 1;
                        }
                    }
                }
            }
        }
        assert!(
            screens >= 140,
            "the corpus must exercise the check: {screens}"
        );
        assert!(
            kept > moved,
            "most rewrites keep the reading: {kept} kept, {moved} moved"
        );
        assert!(newer_below > 0 && own_rows > 0, "{newer_below} {own_rows}");
        // A newer card below the old one moves it; words under it do not.
        let mut screen = card_rows();
        screen.extend(rows(&["", "", "", ""]));
        let reading = launch_card_scan(&screen);
        assert_eq!(reading.0, Some(0));
        let mut words = screen.clone();
        words[4] = "\u{276F} keep going".into();
        assert!(launch_card_unmoved(&words, reading.0, &[4]));
        let mut newer = screen.clone();
        newer[4..].clone_from_slice(&card_rows()[..3]);
        assert!(!launch_card_unmoved(&newer, reading.0, &[4, 5, 6]));
        assert_eq!(launch_card_scan(&newer).0, Some(4));
        // The row OVER a changed one: words that name a build at the narrow
        // card's column are no banner until a mascot row comes under them,
        // and then they are a narrow card's — the reading moves though the
        // changed row draws no banner itself.
        let mut named = screen.clone();
        named[4] = "           Claude Code v2.1.299 is out".into();
        let reading = launch_card_scan(&named);
        assert_eq!(reading.0, Some(0));
        let mut confirmed = named.clone();
        confirmed[5] =
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Opus 5.5".into();
        assert_eq!(launch_card_scan(&confirmed), (Some(4), None));
        assert!(!launch_card_unmoved(&confirmed, reading.0, &[5]));
        // With no banner, a changed row that draws one is a reading moved.
        let blank = rows(&["", "", ""]);
        assert!(launch_card_unmoved(&blank, None, &[0, 1, 2]));
        assert!(!launch_card_unmoved(&card_rows(), None, &[0, 1, 2]));
        // Garbage indices never panic.
        let _ = launch_card_unmoved(&blank, Some(usize::MAX), &[usize::MAX, 0]);
        assert!(launch_card_unmoved(&blank, Some(1), &[0, usize::MAX]));
    }
}
