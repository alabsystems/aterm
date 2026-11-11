// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code launch-card geometry, continuation suggestions, `/goal` indicators, and turn-end reply extraction.

use crate::phase::{
    Transcript, composer_frame, composer_text, is_against_right_edge, is_placeholder, is_tool_call,
    last_said_in, leading_spaces, status_block, transcript_end_in,
};
use crate::prompt::parse_prompt;

pub(crate) const SAID: [char; 3] = ['❯', '⏺', '●'];

/// Returns whether `rows` show a fresh Claude Code launch with no user turn yet.
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let banner = const { crate::anchor!("launch.banner") };
    let Some(card) = rows[..frame.top].iter().rposition(|r| r.contains(banner)) else {
        return false;
    };
    let mut said = rows[card + 1..frame.top]
        .iter()
        .filter(|r| r.starts_with(SAID));
    match said.next() {
        None => true,
        Some(row) => crate::text::trim_end(row) == "❯ /clear" && said.next().is_none(),
    }
}

/// Returns whether `rows` show a launch's opening exchange whole: its launch card, then one user
/// `❯` message (a `/clear` aside) with no reply drawn above it.
#[must_use]
pub fn opening(rows: &[String]) -> bool {
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let banner = const { crate::anchor!("launch.banner") };
    let Some(card) = rows[..frame.top].iter().rposition(|r| r.contains(banner)) else {
        return false;
    };
    let mut said = rows[card + 1..frame.top]
        .iter()
        .filter(|r| r.starts_with(SAID) && crate::text::trim_end(r) != "❯ /clear");
    said.next().is_some_and(|r| r.starts_with('❯')) && !said.any(|r| r.starts_with('❯'))
}

/// Returns whether a user `❯` message or `⏺` reply is visible on `rows`.
#[must_use]
pub fn message_drawn(rows: &[String]) -> bool {
    let top = composer_frame(rows).map_or(rows.len(), |f| f.top);
    rows[..top]
        .iter()
        .any(|r| r.starts_with('❯') && crate::text::trim_end(r) != "❯ /clear")
}

/// Metadata parsed from Claude Code's launch banner card (`version`, `model`, `effort`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LaunchCard {
    pub version: String,
    pub model: String,
    pub effort: Option<String>,
}

/// [`LaunchCard`] paired with the screen row indices from which its banner and footer were read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCardRead {
    pub card: LaunchCard,
    pub banner: Option<usize>,
    pub status: Option<usize>,
}

/// Extracts the [`LaunchCard`] above the composer when no user message has been sent yet.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<LaunchCard> {
    launch_card_read(rows).map(|read| read.card)
}

/// Extracts [`launch_card`] along with its banner and model-source row indices.
#[must_use]
pub fn launch_card_read(rows: &[String]) -> Option<LaunchCardRead> {
    let frame = composer_frame(rows)?;
    let banner = const { crate::anchor!("launch.banner") };
    let card = rows.iter().rposition(|r| r.contains(banner))?;
    if card >= frame.top || rows[card + 1..frame.top].iter().any(|r| r.starts_with('❯')) {
        return None;
    }
    let (_, after) = rows[card].split_once(banner)?;
    let version = banner_version(after)?.to_owned();
    let row = rows.get(card + 1)?;
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
    (!model.is_empty()).then(|| LaunchCardRead {
        card: LaunchCard {
            version,
            model: model.to_owned(),
            effort,
        },
        banner: Some(card),
        status: None,
    })
}

fn banner_version(after: &str) -> Option<&str> {
    let len = after
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
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

/// Screen bounding box and mascot coordinates of Claude Code's launch card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardPlace {
    pub top: usize,
    pub rows: usize,
    pub mascot_row: usize,
    pub text_col: usize,
    pub mascot_end: usize,
    pub ends: [usize; 3],
}

impl CardPlace {
    /// Returns the `(column, row)` cell of the mascot's wand tip, if the mascot is present.
    #[must_use]
    pub fn wand(&self) -> Option<(usize, usize)> {
        Some((
            self.mascot_end.checked_sub(1)?,
            self.top.checked_add(self.mascot_row)?,
        ))
    }
}

pub const WAND_FLARE_DARK: [u8; 3] = [255, 252, 235];

pub const WAND_FLARE_LIGHT: [u8; 3] = [224, 40, 8];

pub const WAND_FLARE_TOLERANCE: u8 = 6;

/// Returns whether a cell with character `ch` and foreground `rgb` is a wand-flare glyph.
#[must_use]
pub fn wand_flare(ch: char, rgb: [u8; 3]) -> bool {
    let near = |want: [u8; 3]| {
        rgb.iter()
            .zip(want)
            .all(|(&c, w)| c.abs_diff(w) <= WAND_FLARE_TOLERANCE)
    };
    ch == '\u{2726}' && (near(WAND_FLARE_DARK) || near(WAND_FLARE_LIGHT))
}

/// How a program's launch banner version relates to its running binary build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerBuild {
    Exact,
    ProvenanceOnly,
}

pub const LAUNCH_CARD_BUILD: BannerBuild = BannerBuild::Exact;

pub const LAUNCH_CARD_ROWS: usize = CARD_ROWS;

/// Locates the newest Claude Code launch card on `rows` by its box-drawing geometry.
#[must_use]
pub fn launch_card_place(rows: &[String]) -> Option<CardPlace> {
    launch_card_scan(rows).1
}

const CARD_ROWS: usize = 3;

const NARROW_ROWS: usize = 4;

const READ_ROWS: usize = NARROW_ROWS + 1;

/// Scans `rows` for the newest launch banner row and its [`CardPlace`].
#[must_use]
pub fn launch_card_scan(rows: &[String]) -> (Option<usize>, Option<CardPlace>) {
    let banner = const { crate::anchor!("launch.banner") };
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
        BannerRow::Narrow { text_col } => narrow_card(rows, top, text_col),
    };
    (Some(top), place)
}

fn wide_card(rows: &[String], top: usize, text_col: usize) -> Option<CardPlace> {
    let mut place = CardPlace {
        top,
        rows: CARD_ROWS,
        mascot_row: 0,
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

fn narrow_card(rows: &[String], top: usize, text_col: usize) -> Option<CardPlace> {
    let banner = const { crate::anchor!("launch.banner") };
    let row = rows.get(top)?;
    let at = crate::text::find(row, banner)?;
    let after = crate::text::trim_end(&row[at + banner.len()..]);
    if !row[..at].chars().all(|c| c == ' ')
        || row[..at].chars().count() != text_col
        || banner_version(after)?.len() != after.len()
    {
        return None;
    }
    if !rows.get(top + 1)?.starts_with(MASCOT_TOP) || !crate::text::blank(rows.get(top + 4)?) {
        return None;
    }
    let mut mascot_end = 0;
    let mut ends = [crate::text::trim_end(row).chars().count(), 0, 0];
    for k in 1..NARROW_ROWS {
        let (m, end) = card_row(&rows[top + k], text_col)?;
        mascot_end = mascot_end.max(m);
        let slot = &mut ends[k.min(2)];
        *slot = (*slot).max(end);
    }
    Some(CardPlace {
        top,
        rows: NARROW_ROWS,
        mascot_row: 1,
        text_col,
        mascot_end,
        ends,
    })
}

/// Returns whether `changed` rows leave a previously scanned launch card unmoved.
#[must_use]
pub fn launch_card_unmoved(rows: &[String], banner: Option<usize>, changed: &[usize]) -> bool {
    let anchor = const { crate::anchor!("launch.banner") };
    if let Some(top) = banner {
        let moved = |span: usize| {
            changed
                .iter()
                .any(|&j| (top..top.saturating_add(span)).contains(&j))
        };
        if moved(2) {
            return false;
        }
        let span = match rows
            .get(top)
            .and_then(|row| banner_row(row, rows.get(top + 1), anchor))
        {
            Some(BannerRow::Wide { .. }) => CARD_ROWS,
            Some(BannerRow::Narrow { .. }) | None => READ_ROWS,
        };
        if moved(span) {
            return false;
        }
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

/// Extracts the version slice (`2.1.300`) from the banner row of a card at `place`.
#[must_use]
pub fn launch_card_version<'a>(rows: &'a [String], place: &CardPlace) -> Option<&'a str> {
    let banner = const { crate::anchor!("launch.banner") };
    let row = rows.get(place.top)?;
    let at = crate::text::find(row, banner)?;
    let opens = match place.mascot_row {
        0 => row.starts_with(MASCOT_TOP),
        _ => row[..at].chars().all(|c| c == ' '),
    };
    if !opens || row[..at].chars().count() != place.text_col {
        return None;
    }
    banner_version(&row[at + banner.len()..])
}

/// Text lines extracted from each row of a launch card (`3` for wide, `4` for narrow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardWords<'a> {
    rows: [&'a str; NARROW_ROWS],
    len: usize,
}

impl<'a> CardWords<'a> {
    /// Returns the card's text lines, starting with the banner row.
    #[must_use]
    pub fn as_slice(&self) -> &[&'a str] {
        &self.rows[..self.len]
    }
}

/// Extracts the [`CardWords`] for a launch card at `place` on `rows`.
#[must_use]
pub fn launch_card_words<'a>(rows: &'a [String], place: &CardPlace) -> Option<CardWords<'a>> {
    let banner = const { crate::anchor!("launch.banner") };
    if !matches!(place.rows, CARD_ROWS | NARROW_ROWS) {
        return None;
    }
    let mut words = CardWords {
        rows: [""; NARROW_ROWS],
        len: place.rows,
    };
    for (k, w) in words.rows[..place.rows].iter_mut().enumerate() {
        *w = card_words(rows.get(place.top.checked_add(k)?)?, place.text_col)?;
    }
    words.rows[0].starts_with(banner).then_some(words)
}

fn card_words(row: &str, text_col: usize) -> Option<&str> {
    let mut chars = row.char_indices();
    if chars
        .by_ref()
        .take(text_col)
        .any(|(_, c)| c.is_alphanumeric())
    {
        return None;
    }
    let (at, text) = chars.next()?;
    (!text.is_whitespace()).then(|| crate::text::trim_end(&row[at..]))
}

/// Returns whether a previously placed card `kept` still occupies the same rows with identical text.
#[must_use]
pub fn launch_card_held(
    rows: &[String],
    scan: (Option<usize>, Option<CardPlace>),
    kept: &CardPlace,
    words: &[&str],
) -> bool {
    let (banner, place) = scan;
    banner.is_none_or(|b| b < kept.top || (b == kept.top && place.is_none()))
        && launch_card_words(rows, kept).is_some_and(|w| w.as_slice() == words)
}

/// Tracks a previously placed launch card after the screen scrolls.
#[must_use]
pub fn launch_card_scrolled(
    rows: &[String],
    scan: (Option<usize>, Option<CardPlace>),
    kept: &CardPlace,
    words: &[&str],
) -> Option<CardPlace> {
    if scan.0.is_some() || words.len() != kept.rows || kept.rows > NARROW_ROWS {
        return None;
    }
    let shown = kept.rows.min(rows.len().saturating_sub(1));
    'cut: for v in (1..=shown).rev() {
        let first = kept.rows - v;
        let mut place = CardPlace {
            top: 0,
            rows: v,
            mascot_row: 0,
            text_col: kept.text_col,
            mascot_end: 0,
            ends: [0; 3],
        };
        for (i, row) in rows[..v].iter().enumerate() {
            let at_edge = row.chars().take(2).any(|c| c != ' ');
            let Some((mascot_end, end)) = card_row(row, kept.text_col).filter(|_| at_edge) else {
                continue 'cut;
            };
            if card_words(row, kept.text_col) != Some(words[first + i]) {
                continue 'cut;
            }
            place.mascot_end = place.mascot_end.max(mascot_end);
            let slot = &mut place.ends[i.min(2)];
            *slot = (*slot).max(end);
        }
        if crate::text::blank(&rows[v]) {
            return Some(place);
        }
    }
    None
}

/// Returns whether `changed` rows leave a scrolled launch card slice `cut` unmoved.
#[must_use]
pub fn launch_card_scrolled_unmoved(rows: &[String], cut: &CardPlace, changed: &[usize]) -> bool {
    changed.iter().all(|&j| j > cut.rows) && launch_card_unmoved(rows, None, changed)
}

const MASCOT_TOP: &str = " \u{2590}";

const NARROW_BANNER_COLS: std::ops::RangeInclusive<usize> = 9..=12;

const NARROW_HELD_COL: usize = 13;

fn is_block(c: char) -> bool {
    ('\u{2580}'..='\u{259F}').contains(&c)
}

fn is_held(c: char) -> bool {
    !c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control() && !is_block(c)
}

fn is_mascot_glyph(c: char) -> bool {
    is_block(c) || is_held(c)
}

enum BannerRow {
    Wide { text_col: usize },
    Narrow { text_col: usize },
}

fn banner_row(row: &str, next: Option<&String>, banner: &str) -> Option<BannerRow> {
    let at = crate::text::find(row, banner)?;
    let before = &row[..at];
    if !before.chars().all(|c| c == ' ' || is_mascot_glyph(c)) {
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
    let quoted = || next.is_some_and(|n| !crate::text::blank(n) && !n.starts_with(MASCOT_TOP));
    let bare = (NARROW_BANNER_COLS.contains(&text_col) || text_col == NARROW_HELD_COL && !quoted())
        && banner_version(after).is_some_and(|v| v.len() == after.len());
    let confirmed =
        next.is_some_and(|n| n.starts_with(MASCOT_TOP) && card_row(n, text_col).is_some());
    (bare || confirmed).then_some(BannerRow::Narrow { text_col })
}

fn card_row(row: &str, text_col: usize) -> Option<(usize, usize)> {
    let mut chars = row.chars();
    let (mut mascot_end, mut block) = (0, false);
    for (col, c) in chars.by_ref().take(text_col).enumerate() {
        if is_mascot_glyph(c) {
            mascot_end = col + 1;
            block |= is_block(c);
        } else if c != ' ' {
            return None;
        }
    }
    let text = chars.next()?;
    if text.is_whitespace() || is_block(text) || !block || mascot_end >= text_col {
        return None;
    }
    Some((mascot_end, crate::text::trim_end(row).chars().count()))
}

/// Extracts Claude Code's dim placeholder continuation suggestion from an empty composer.
#[must_use]
pub fn continuation_suggestion(rows: &[String], cursor_col: usize) -> Option<String> {
    composer_frame(rows)?;
    if parse_prompt(rows).is_some() || !is_placeholder(rows, cursor_col) {
        return None;
    }
    composer_text(rows).filter(|t| !t.is_empty())
}

/// Claude Code's `/goal` indicator.
const GOAL_MARK: &str = "◎ /goal active";

/// Returns whether Claude Code's `◎ /goal active` indicator is present in the live zone: against
/// the right edge on a hint row over the composer or a footer row under it ([`goal_mark`]). A row
/// over the composer is read up to the side panel's rule and against its room ([`Transcript`]),
/// the right edge a hint stands against beside the panel; a footer row, which the panel is drawn
/// beside in no capture, whole and against the composer's width.
#[must_use]
pub fn goal_active(rows: &[String]) -> bool {
    let Some(frame) = composer_frame(rows) else {
        return false;
    };
    let width = crate::text::trim_end(&rows[frame.bottom]).chars().count();
    let from = status_block(rows, frame.top).from;
    let transcript = Transcript::of(rows);
    let room = transcript.room(width);
    let at_edge = |r: &str| crate::text::trim_end(r).chars().count() + 3 >= width;
    transcript.rows[from..frame.top]
        .iter()
        .any(|r| goal_mark(r) && is_against_right_edge(r, room))
        || rows[frame.bottom + 1..]
            .iter()
            .any(|r| goal_mark(r) && at_edge(r))
}

/// `row` carries the goal mark: opening it, or as its rightmost item, bare or with its age
/// (`(1d)`), where Claude Code runs it into the row's other items (2.1.283's update hint,
/// `✔ Update installed · Restart to update◎ /goal active (1d)`; 2.1.311's footer,
/// `⏵⏵ auto mode on (shift+tab to cycle)` … `◎ /goal active (36s)`).
fn goal_mark(row: &str) -> bool {
    let t = crate::text::trim_end(row);
    crate::text::trim_start(t).starts_with(GOAL_MARK)
        || t.rfind(GOAL_MARK).is_some_and(|at| {
            let rest = &t[at + GOAL_MARK.len()..];
            rest.is_empty()
                || rest
                    .strip_prefix(" (")
                    .and_then(|age| age.strip_suffix(')'))
                    .is_some_and(|age| !age.is_empty() && !age.contains(['(', ')']))
        })
}

/// Returns whether the agent's final reply `said` ends with a question mark (`?`).
pub(crate) fn asks(said: &str) -> bool {
    said.lines()
        .last()
        .is_some_and(|l| crate::text::trim_end(l).ends_with('?'))
}

/// Extracts the worker's final `⏺` message from the last turn on `rows`, each of its rows read up
/// to the side panel's rule ([`Transcript`]), as every reader of the turn's words here reads them.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let transcript = Transcript::of(rows);
    said_message(rows, &transcript).map(|(_, said)| said)
}

/// Returns whether the `⏺` header of [`said_tail`] is visible on `rows`.
#[must_use]
pub fn said_whole(rows: &[String]) -> bool {
    let transcript = Transcript::of(rows);
    said_message_rows(rows, &transcript).is_some_and(|(head, _)| rows[head].starts_with(['⏺', '●']))
}

/// [`said_tail`], each line Ink wrapped across rows read as one ([`crate::wrap::said_lines`]),
/// beside the side panel in the room its rows wrap in, up to its rule ([`Transcript::margin`]).
#[must_use]
pub fn said_lines(rows: &[String], apart: crate::reader::Apart) -> Option<String> {
    let transcript = Transcript::of(rows);
    let margin = transcript.margin();
    said_message_rows(rows, &transcript)
        .map(|(_, said)| crate::wrap::said_lines(&transcript.rows, &said, margin, apart))
}

/// Extracts earlier `⏺` messages from the same turn above [`said_tail`], joined
/// ([`crate::reader::join_messages`]).
#[must_use]
pub fn turn_said_before(rows: &[String]) -> Option<String> {
    turn_messages_before(rows).map(|m| crate::reader::join_messages(&m))
}

/// [`turn_said_before`] message by message, oldest first.
#[must_use]
pub fn turn_messages_before(rows: &[String]) -> Option<Vec<String>> {
    let (_, said) = turn_before(rows, &Transcript::of(rows), crate::wrap::said_rows)?;
    (!said.is_empty()).then_some(said)
}

/// [`turn_said_before`], its lines read as [`said_lines`] reads them.
#[must_use]
pub fn turn_lines_before(rows: &[String], apart: crate::reader::Apart) -> Option<String> {
    turn_message_lines_before(rows, apart).map(|m| crate::reader::join_messages(&m))
}

/// [`turn_lines_before`] message by message, oldest first.
#[must_use]
pub fn turn_message_lines_before(
    rows: &[String],
    apart: crate::reader::Apart,
) -> Option<Vec<String>> {
    let transcript = Transcript::of(rows);
    let margin = transcript.margin();
    let (_, said) = turn_before(rows, &transcript, |said| {
        crate::wrap::said_lines(&transcript.rows, said, margin, apart)
    })?;
    (!said.is_empty()).then_some(said)
}

/// Returns whether the entire last turn (from the user's `❯` prompt through [`said_tail`]) fits on `rows`.
#[must_use]
pub fn turn_whole(rows: &[String]) -> bool {
    turn_before(rows, &Transcript::of(rows), crate::wrap::said_rows)
        .is_some_and(|(opened, _)| opened)
}

/// Returns whether the last turn on `rows` ran a tool: a call's row (`⏺ Bash(…)`, [`bare_call`]), a collapsed tool group's ([`tool_group`]) or a background task's end ([`task_notice`]) under the `❯` prompt that opened it.
#[must_use]
pub fn turn_ran_tools(rows: &[String]) -> bool {
    let end = crate::phase::composer_frame(rows).map_or(rows.len(), |f| f.top);
    let width = crate::question::screen_width(rows);
    (0..end)
        .rev()
        .take_while(|&i| !rows[i].starts_with('❯'))
        .any(|i| {
            is_tool_call(&rows[i])
                || bare_call(rows, i, end)
                || tool_group(rows, i, end, width)
                || task_notice(rows, i, end)
        })
}

/// What a task's notice draws past its end ([`TASK_NOTICES`]).
#[derive(Clone, Copy)]
enum Tail {
    /// Nothing more.
    Bare,
    /// An exit code between these, or nothing.
    Code(&'static str, &'static str),
    /// One of these, or nothing.
    Or(&'static [&'static str]),
    /// A reason or an error to the row's end.
    Words,
}

/// A background task's notice heads and each end 2.1.268's notice builders draw after its quoted description.
const TASK_NOTICES: [(&str, &str, Tail); 19] = {
    let command = crate::anchor!("turn.task.command");
    let monitor = crate::anchor!("turn.task.monitor");
    let agent = crate::anchor!("turn.task.agent");
    let workflow = crate::anchor!("turn.task.workflow");
    let remote = crate::anchor!("turn.task.remote");
    let completed = crate::anchor!("turn.task.completed");
    let was_stopped = crate::anchor!("turn.task.was_stopped");
    let failed_with = crate::anchor!("turn.task.failed_with");
    let exit = Tail::Code(crate::anchor!("turn.task.exit"), ")");
    [
        (
            command,
            completed,
            Tail::Code(crate::anchor!("turn.task.exit_code"), ")"),
        ),
        (
            command,
            crate::anchor!("turn.task.failed"),
            Tail::Code(crate::anchor!("turn.task.with_exit_code"), ""),
        ),
        (
            command,
            was_stopped,
            Tail::Or(&[crate::anchor!("turn.task.memory_pressure")]),
        ),
        (monitor, crate::anchor!("turn.task.quiet"), exit),
        (
            monitor,
            crate::anchor!("turn.task.stream_ended"),
            Tail::Bare,
        ),
        (monitor, crate::anchor!("turn.task.script_failed"), exit),
        (monitor, crate::anchor!("turn.task.stopped"), Tail::Bare),
        (agent, crate::anchor!("turn.task.finished"), Tail::Bare),
        (agent, crate::anchor!("turn.task.waiting"), Tail::Bare),
        (agent, failed_with, Tail::Words),
        (
            agent,
            was_stopped,
            Tail::Or(&[
                crate::anchor!("turn.task.by_claude"),
                crate::anchor!("turn.task.by_user"),
            ]),
        ),
        (agent, crate::anchor!("turn.task.turn_limit"), Tail::Words),
        (workflow, completed, Tail::Bare),
        (workflow, failed_with, Tail::Words),
        (workflow, was_stopped, Tail::Bare),
        (remote, crate::anchor!("turn.task.succeeded"), Tail::Bare),
        (remote, crate::anchor!("turn.task.failed"), Tail::Bare),
        (remote, crate::anchor!("turn.task.blocked"), Tail::Words),
        (remote, was_stopped, Tail::Bare),
    ]
};

/// A background task's notice of its run's end, the turn it opens, read across the rows its description wraps onto: `⏺ Background command "…" completed (exit code 0)` or `failed with exit code 1`, `⏺ Monitor "…" stream ended`, `⏺ Agent "…" finished · 2s`, `⏺ Dynamic workflow "…" completed · 5s`, each [`TASK_NOTICES`] end, then the time and the delivery the notice row adds after ` · `. A Monitor's event is none: the monitor runs on.
fn task_notice(rows: &[String], i: usize, until: usize) -> bool {
    let Some(r) = rows[i].strip_prefix("⏺ ") else {
        return false;
    };
    if !TASK_NOTICES.iter().any(|(head, ..)| r.starts_with(head)) {
        return false;
    }
    let end = (i + 1..until)
        .find(|&j| {
            crate::text::blank(&rows[j])
                || crate::text::trim_start(&rows[j]).starts_with(['⎿', '⏺', '❯', '✻'])
        })
        .unwrap_or(until);
    let line = crate::wrap::rejoined(&rows[i..end]);
    let Some(said) = crate::text::trim(&line).strip_prefix("⏺ ") else {
        return false;
    };
    TASK_NOTICES.iter().any(|&(head, ended, tail)| {
        let Some(said) = said.strip_prefix(head) else {
            return false;
        };
        let close = format!("\" {ended}");
        said.rfind(&close).is_some_and(|at| {
            let after = &said[at + close.len()..];
            at > 0
                && match tail {
                    Tail::Words => true,
                    Tail::Bare => notice_suffix(after),
                    Tail::Or(more) => {
                        notice_suffix(after)
                            || more
                                .iter()
                                .any(|m| after.strip_prefix(m).is_some_and(notice_suffix))
                    }
                    Tail::Code(open, shut) => {
                        notice_suffix(after)
                            || after
                                .strip_prefix(open)
                                .map(|r| r.trim_start_matches('-'))
                                .and_then(|r| {
                                    let n = r.bytes().take_while(u8::is_ascii_digit).count();
                                    (n > 0).then(|| &r[n..])
                                })
                                .and_then(|r| r.strip_prefix(shut))
                                .is_some_and(notice_suffix)
                    }
                }
        })
    })
}

/// What a notice's row adds past its end: nothing, or after ` · ` each the task's time or its delivery to the model.
fn notice_suffix(after: &str) -> bool {
    after.is_empty()
        || after.strip_prefix(" · ").is_some_and(|r| {
            r.split(" · ").all(|part| {
                duration(part) || part.starts_with(crate::anchor!("turn.task.delivered"))
            })
        })
}

/// A call's row drawn with no `(…)`, its `⎿` output row under it: TaskStop's `⏺ Stop Task`, an edit of the plan file's `⏺ Updated plan`.
fn bare_call(rows: &[String], i: usize, until: usize) -> bool {
    let names = [
        crate::anchor!("turn.call.stop_task"),
        crate::anchor!("turn.call.updated_plan"),
    ];
    rows[i]
        .strip_prefix("⏺ ")
        .is_some_and(|name| names.contains(&crate::text::trim_end(name)))
        && i + 1 < until
        && rows[i + 1].starts_with("  ⎿")
}

/// Returns whether the `❯` prompt that opened the last turn is above `rows`, so [`turn_ran_tools`] read the turn's lower rows only.
#[must_use]
pub fn turn_opens_above(rows: &[String]) -> bool {
    let end = crate::phase::composer_frame(rows).map_or(rows.len(), |f| f.top);
    !rows[..end].iter().any(|r| r.starts_with('❯'))
}

/// The verbs of 2.1.268's group renderer a count and its nouns follow (`Read 1 file`, `running 2 shell commands…`, `edited 2 files +10 -3`).
const GROUP_VERBS: [&str; 20] = [
    crate::anchor!("turn.group.reading"),
    crate::anchor!("turn.group.read"),
    crate::anchor!("turn.group.searched"),
    crate::anchor!("turn.group.searching"),
    crate::anchor!("turn.group.listed"),
    crate::anchor!("turn.group.listing"),
    crate::anchor!("turn.group.running"),
    crate::anchor!("turn.group.ran"),
    crate::anchor!("turn.group.edited"),
    crate::anchor!("turn.group.editing"),
    crate::anchor!("turn.group.made"),
    crate::anchor!("turn.group.making"),
    crate::anchor!("turn.group.repld"),
    crate::anchor!("turn.group.repling"),
    crate::anchor!("turn.group.called"),
    crate::anchor!("turn.group.calling"),
    crate::anchor!("turn.group.recalled"),
    crate::anchor!("turn.group.recalling"),
    crate::anchor!("turn.group.wrote"),
    crate::anchor!("turn.group.writing"),
];

/// The nouns a group's count takes with no expand hint, singular or plural (`Read 1 file`, `Ran 2 shell commands`, `recalled 3 memories`).
const GROUP_NOUNS: [&str; 8] = [
    crate::anchor!("turn.group.file"),
    crate::anchor!("turn.group.pattern"),
    crate::anchor!("turn.group.directory"),
    crate::anchor!("turn.group.shell_command"),
    crate::anchor!("turn.group.agent"),
    crate::anchor!("turn.group.tool"),
    crate::anchor!("turn.group.memory"),
    crate::anchor!("turn.group.team_memory"),
];

/// The renderer's git clauses and what each names (`Committed 1a2b3c4`, `pushed to main`, `rebased onto origin/main`).
const GROUP_GIT: [(&str, Item); 6] = [
    (crate::anchor!("turn.group.committed"), Item::Sha),
    (crate::anchor!("turn.group.amended"), Item::Sha),
    (crate::anchor!("turn.group.cherry_picked"), Item::Sha),
    (crate::anchor!("turn.group.pushed"), Item::Ref),
    (crate::anchor!("turn.group.merged"), Item::Ref),
    (crate::anchor!("turn.group.rebased"), Item::Ref),
];

/// The renderer's pull and merge request verbs, each before `PR #<n>` or `MR !<n>` (`created PR #12`, `enabled auto-merge on PR #12`).
const GROUP_PR: [&str; 10] = [
    crate::anchor!("turn.group.created"),
    crate::anchor!("turn.group.edited"),
    crate::anchor!("turn.group.merged"),
    crate::anchor!("turn.group.commented_on"),
    crate::anchor!("turn.group.closed"),
    crate::anchor!("turn.group.reopened"),
    crate::anchor!("turn.group.marked_ready"),
    crate::anchor!("turn.group.marked_draft"),
    crate::anchor!("turn.group.enabled_auto_merge"),
    crate::anchor!("turn.group.disabled_auto_merge"),
];

/// What a git or MCP clause names, a list of them going on after `, ` (`committed 1a2b3c4, 5d6e7f8`, `called github, slack 2 times`).
#[derive(Clone, Copy)]
enum Item {
    Sha,
    Ref,
    Server,
}

impl Item {
    /// Whether `clause`, read as naming this, shows what a sentence lacks: a sha, a ref with a `/`.
    fn shows(self, clause: &str) -> bool {
        match self {
            Self::Sha => true,
            Self::Ref => clause.contains('/'),
            Self::Server => false,
        }
    }

    fn reads(self, s: &str) -> bool {
        match self {
            Self::Sha => (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit()),
            Self::Ref => {
                !s.is_empty()
                    && !s.ends_with(['.', '/'])
                    && !s.contains("..")
                    && s.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-')
                    })
            }
            Self::Server => {
                let (server, times) = s.split_once(' ').unwrap_or((s, ""));
                !server.is_empty()
                    && server
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                    && (times.is_empty()
                        || times.strip_suffix(" times").is_some_and(|n| {
                            !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
                        }))
            }
        }
    }
}

/// A collapsed group's clause read ([`group_clause`]).
#[derive(Clone, Copy)]
enum Clause {
    /// A tool's, a list of the [`Item`] it names going on after it.
    Tool(Option<Item>),
    /// A git or pull request operation's, which a reply's own words can say too.
    Git(Option<Item>),
    /// A lone agent's description to the row's end (`ran agent · `, or its first word conjugated).
    ToEnd,
    /// `Thought for` the turn's thinking, opening the row.
    Thought,
}

/// Whether row `i` is Claude Code's collapsed tool group of a tool run, read across the rows it wraps onto: a `⏺ ` or two-space row of clauses (`⏺ Thought for 3s, read 1 file, pushed to main (ctrl+o to expand)`), the first the renderer's and any one a tool's, before the expand hint; with no hint, every clause the renderer's and its counts of [`GROUP_NOUNS`], a two-space row standing alone under a blank row or the screen's top (`  Read 1 file`; under another group it adds nothing, that one ran), never a reply's next row, and one whose tool clauses are all git or pull request clauses only by what a sentence lacks ([`group_ran`]), wherever it stands. A row a `⏺ ` or two-space line above wraps onto is that line's, read once from its first row. A stop hook's summary and a `PreToolUse` hooks' are none.
fn tool_group(rows: &[String], i: usize, until: usize, width: usize) -> bool {
    let row = rows[i].as_str();
    let headed = row.starts_with("⏺ ");
    if !headed && !(row.starts_with("  ") && leading_spaces(row) == 2) {
        return false;
    }
    let above = i.checked_sub(1).map(|k| rows[k].as_str());
    if !headed && above.is_some_and(|a| wraps_onto(a, row, width)) {
        group_read(1);
        return false;
    }
    let (end, line) = crate::phase::line_at(rows, i, until, width);
    group_read(end + 1 - i);
    let text = crate::text::trim(&line);
    let text = text.strip_prefix('⏺').map_or(text, crate::text::trim_start);
    let hinted = text.strip_suffix(const { crate::anchor!("turn.group.expand") });
    let text = crate::text::trim_end(hinted.unwrap_or(text));
    let text = text.strip_suffix('…').unwrap_or(text);
    match hinted {
        Some(_) => group_ran(text, false).is_some(),
        None => {
            (headed || above.is_none_or(crate::text::blank)) && group_ran(text, true) == Some(true)
        }
    }
}

/// Whether `row` is the rest of the `⏺ ` or two-space line on the row `above` it, both drawn from column 2, as [`crate::phase::line_at`] reads that line. It keeps [`crate::wrap::room`] and its slack, not the transcript's box ([`crate::wrap::transcript_room`]): the row above may be a vendor `⏺` notice's, its text wrapped in the pane less ten (`NOTICE_MARGIN`), or a dialog's, drawn in [`crate::wrap::room`], which the two rows alone do not tell from a reply's.
fn wraps_onto(above: &str, row: &str, width: usize) -> bool {
    let said = match above.strip_prefix("⏺ ") {
        Some(said) => said,
        None if leading_spaces(above) == 2 && !crate::text::trim_start(above).starts_with('⎿') => {
            above
        }
        None => return false,
    };
    crate::wrap::continues(crate::text::trim(said), 2, row, crate::wrap::room(2, width))
}

#[cfg(test)]
thread_local! {
    /// The rows [`tool_group`] read on this thread: its work count.
    static GROUP_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Count `n` rows [`tool_group`] read (tests only).
#[cfg(test)]
fn group_read(n: usize) {
    GROUP_ROWS.with(|c| c.set(c.get() + n));
}

/// The shipped build counts nothing.
#[cfg(not(test))]
const fn group_read(_: usize) {}

/// Whether a group row's `text` ran a tool by its clauses, which 2.1.268's renderer joins with `, `: the first the renderer's and any one a tool's; `strict`, every one the renderer's. `Some(false)` where every tool clause is a git or pull request one, none showing what a sentence lacks ([`Item::shows`], a request's number) and no second clause beside it (`  Pushed to main, rebased onto origin/main`): `  Merged cleanly` and `  Pushed to production` are words.
fn group_ran(text: &str, strict: bool) -> Option<bool> {
    // A lone agent's few words, never an error the vendor cut at 1000 characters and drew the hint under.
    let described = !strict && text.len() <= DESCRIBED;
    let (mut ran, mut shown, mut list, mut clauses) = (false, false, None, 0);
    for (k, clause) in text.split(", ").map(crate::text::trim_end).enumerate() {
        let read = group_clause(clause, k == 0, strict, described);
        clauses += usize::from(read.is_some());
        match read {
            Some(Clause::ToEnd) => return Some(true),
            Some(Clause::Tool(item)) => (ran, shown, list) = (true, true, item),
            Some(Clause::Git(item)) => {
                (ran, list) = (true, item);
                shown |= item.is_none_or(|item| item.shows(clause));
            }
            Some(Clause::Thought) => list = None,
            None if list.is_some_and(|item| item.reads(clause)) => {
                shown |= list.is_some_and(|item| item.shows(clause));
            }
            None if strict || k == 0 => return None,
            None => list = None,
        }
    }
    ran.then_some(shown || clauses > 1)
}

/// The longest hinted group row a lone agent's conjugated description is read in ([`group_ran`]): a heuristic, no vendor constant, half the vendor's 1000-character cut (EPe) so an error cut and hinted there is never one.
const DESCRIBED: usize = 500;

/// One clause of a group row, `first` where it opens the row; `named`, a count's nouns one of [`GROUP_NOUNS`]; `described`, a lone agent's description may stand as one ([`lone_agent`]).
fn group_clause(clause: &str, first: bool, named: bool, described: bool) -> Option<Clause> {
    let after = |head: &str| opens(clause, head, first).and_then(|r| r.strip_prefix(' '));
    let thought = [
        crate::anchor!("turn.group.thought"),
        crate::anchor!("turn.group.thinking"),
    ];
    if first && thought.iter().any(|h| after(h).is_some_and(duration)) {
        return Some(Clause::Thought);
    }
    if counted(clause, first, named) || bare(clause, first) {
        return Some(Clause::Tool(None));
    }
    if pull_request(clause, first) {
        return Some(Clause::Git(None));
    }
    let called = [
        crate::anchor!("turn.group.called"),
        crate::anchor!("turn.group.calling"),
    ];
    if called
        .iter()
        .any(|h| after(h).is_some_and(|r| Item::Server.reads(r)))
    {
        return Some(Clause::Tool(Some(Item::Server)));
    }
    if let Some(&(_, item)) = GROUP_GIT
        .iter()
        .find(|(h, item)| after(h).is_some_and(|r| item.reads(r)))
    {
        return Some(Clause::Git(Some(item)));
    }
    let ran = [
        crate::anchor!("turn.group.ran"),
        crate::anchor!("turn.group.running"),
    ];
    (ran.iter()
        .any(|h| after(h).is_some_and(|r| r.starts_with(crate::anchor!("turn.group.agent_named"))))
        || (described && lone_agent(clause)))
    .then_some(Clause::ToEnd)
}

/// `clause` past `head` as the renderer draws it: as it stands where the clause opens the row, its first letter lower-case after it (an acronym's, `REPL'd`, as it stands).
fn opens<'a>(clause: &'a str, head: &str, first: bool) -> Option<&'a str> {
    let rest = clause.get(head.len()..)?;
    let (h, c) = (head.as_bytes(), clause.as_bytes());
    let lead = if first || h.get(1).is_some_and(u8::is_ascii_uppercase) {
        h[0]
    } else {
        h[0].to_ascii_lowercase()
    };
    (c[0] == lead && c[1..h.len()] == h[1..]).then_some(rest)
}

/// The renderer's time: `3s`, `1m 5s`, `2h 3m`.
fn duration(text: &str) -> bool {
    text.split(' ').all(|t| {
        t.strip_suffix(['s', 'm', 'h', 'd']).is_some_and(|n| {
            n.starts_with(|c: char| c.is_ascii_digit())
                && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        })
    })
}

/// A group's clause of a verb ([`GROUP_VERBS`]), a count and up to three lower-case nouns (an edit's `+a -r` after them), `named` one of [`GROUP_NOUNS`]; a stop hook's is none.
fn counted(clause: &str, first: bool, named: bool) -> bool {
    GROUP_VERBS.iter().any(|verb| {
        let Some((n, nouns)) = opens(clause, verb, first)
            .and_then(|r| r.strip_prefix(' '))
            .and_then(|r| r.split_once(' '))
        else {
            return false;
        };
        let nouns = without_lines(nouns);
        let words: Vec<&str> = nouns.split(' ').collect();
        !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && words.len() <= 3
            && words
                .iter()
                .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()))
            && !nouns.starts_with(const { crate::anchor!("hook.ran.stop") })
            && (!named || GROUP_NOUNS.iter().any(|noun| group_noun(nouns, noun)))
    })
}

/// `nouns` without an edit's `+a -r` after them.
fn without_lines(nouns: &str) -> &str {
    let mut out = nouns;
    while let Some((head, tail)) = out.rsplit_once(' ')
        && tail.len() > 1
        && tail.starts_with(['+', '-'])
        && tail[1..].bytes().all(|b| b.is_ascii_digit())
    {
        out = head;
    }
    out
}

/// Whether `nouns` is `noun`, or its plural (`files`, `directories`).
fn group_noun(nouns: &str, noun: &str) -> bool {
    nouns == noun
        || nouns.strip_suffix('s') == Some(noun)
        || noun
            .strip_suffix('y')
            .is_some_and(|stem| nouns.strip_suffix("ies") == Some(stem))
}

/// A pull or merge request's clause: one of [`GROUP_PR`], then `PR #<n>` or `MR !<n>`.
fn pull_request(clause: &str, first: bool) -> bool {
    let tags = [
        crate::anchor!("turn.group.pr"),
        crate::anchor!("turn.group.mr"),
    ];
    GROUP_PR.iter().any(|verb| {
        opens(clause, verb, first)
            .and_then(|r| r.strip_prefix(' '))
            .is_some_and(|r| {
                tags.iter().any(|tag| {
                    r.strip_prefix(tag)
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
            })
    })
}

/// A clause with no count: `Published`, `searched memories`, `searched team memories`.
fn bare(clause: &str, first: bool) -> bool {
    let published = [
        crate::anchor!("turn.group.published"),
        crate::anchor!("turn.group.publishing"),
    ];
    let searched = [
        crate::anchor!("turn.group.searched_bare"),
        crate::anchor!("turn.group.searching_bare"),
    ];
    let memories = [
        crate::anchor!("turn.group.memory"),
        crate::anchor!("turn.group.team_memory"),
    ];
    published
        .iter()
        .any(|h| opens(clause, h, first) == Some(""))
        || searched.iter().any(|h| {
            opens(clause, h, first)
                .and_then(|r| r.strip_prefix(' '))
                .is_some_and(|r| memories.iter().any(|noun| group_noun(r, noun)))
        })
}

/// A lone agent's description as 2.1.268's group renderer draws it in place of `ran agent · `: its first word conjugated ([`agent_verb`]), past where the agent is done, `-ing` while it runs (`Reviewed the diff`), never a count after it (a stop hook's `Ran 2 stop hooks`, `Ran 2 PreToolUse hooks`).
fn lone_agent(clause: &str) -> bool {
    let (word, rest) = clause.split_once(' ').unwrap_or((clause, ""));
    !rest.starts_with(|c: char| c.is_ascii_digit()) && agent_verb::conjugated(word)
}

/// 2.1.268's verb forms for an agent's description, its UWt as read in the binary: the irregular tables ([`crate::anchors`]' `turn.agent.*`), then its suffix rules.
mod agent_verb {
    const PAST: &str = crate::anchor!("turn.agent.past");
    const RUNNING: &str = crate::anchor!("turn.agent.running");

    /// The irregular form `table` gives `base`.
    fn irregular(table: &'static str, base: &str) -> Option<&'static str> {
        table.split(',').find_map(|pair| {
            let (k, v) = pair.split_once(':')?;
            (k == base).then(|| v.trim_matches('"'))
        })
    }

    /// The bases `table` gives `form`.
    fn bases(table: &'static str, form: &str) -> impl Iterator<Item = &'static str> {
        table.split(',').filter_map(move |pair| {
            let (k, v) = pair.split_once(':')?;
            (v.trim_matches('"') == form).then_some(k)
        })
    }

    /// A past form no description opens with (`ran`, `written`).
    fn participle(t: &str) -> bool {
        PAST.split(',').any(|pair| {
            pair.split_once(':')
                .is_some_and(|(k, v)| v.trim_matches('"') == t && k != v.trim_matches('"'))
        }) || crate::anchor!("turn.agent.participles")
            .split(',')
            .any(|p| p.trim_matches('"') == t)
    }

    fn vowel(b: u8) -> bool {
        b"aeiou".contains(&b.to_ascii_lowercase())
    }

    /// A consonant, a vowel and a consonant the binary doubles, at the word's end, its only vowel.
    fn doubles(w: &str) -> bool {
        let b = w.as_bytes();
        let n = b.len();
        n >= 3
            && !vowel(b[n - 3])
            && vowel(b[n - 2])
            && b"bcdfghjklmnpqrstvz".contains(&b[n - 1].to_ascii_lowercase())
            && b.iter().filter(|&&c| vowel(c)).count() == 1
    }

    fn ends(w: &str, end: &str) -> bool {
        w.len() >= end.len() && w[w.len() - end.len()..].eq_ignore_ascii_case(end)
    }

    /// Before its last letter, a letter not in `not`.
    fn before_last(w: &str, not: &[u8]) -> bool {
        w.len() >= 2 && !not.contains(&w.as_bytes()[w.len() - 2].to_ascii_lowercase())
    }

    fn running(w: &str) -> String {
        let n = w.len();
        if let Some(f) = irregular(RUNNING, &w.to_ascii_lowercase()) {
            f.to_owned()
        } else if ends(w, "ie") {
            format!("{}ying", &w[..n - 2])
        } else if ends(w, "e") && before_last(w, b"eoy") {
            format!("{}ing", &w[..n - 1])
        } else if doubles(w) {
            format!("{w}{}ing", &w[n - 1..])
        } else if ends(w, "c") {
            format!("{w}king")
        } else {
            format!("{w}ing")
        }
    }

    fn done(w: &str) -> String {
        let n = w.len();
        let t = w.to_ascii_lowercase();
        if let Some(f) = irregular(PAST, &t) {
            f.to_owned()
        } else if ends(w, "e") {
            format!("{w}d")
        } else if ends(w, "y") && before_last(w, b"aeiou") {
            format!("{}ied", &w[..n - 1])
        } else if doubles(w) {
            format!("{w}{}ed", &w[n - 1..])
        } else if ends(w, "c") {
            format!("{w}ked")
        } else if let Some(f) = irregular(RUNNING, &t) {
            format!("{}ed", &f[..f.len() - 3])
        } else {
            format!("{w}ed")
        }
    }

    /// `form` with `base`'s capital, where it opens with one.
    fn capital(base: &str, form: String) -> String {
        if base.starts_with(|c: char| c.is_ascii_uppercase()) {
            let mut out = form;
            out[..1].make_ascii_uppercase();
            out
        } else {
            form
        }
    }

    /// A word the binary conjugates: 2 to 16 letters, not all capitals, no past participle, no `-ing`, `-ed` or plural form.
    fn verb(e: &str) -> bool {
        let n = e.len();
        let t = e.to_ascii_lowercase();
        (2..=16).contains(&n)
            && e.bytes().all(|b| b.is_ascii_alphabetic())
            && e != e.to_ascii_uppercase()
            && !participle(&t)
            && (irregular(PAST, &t).is_some()
                || !((t.ends_with("ing") && n > 4)
                    || (t.ends_with("ed") && !t.ends_with("eed") && n > 3)
                    || (t.ends_with('s') && !t.ends_with("ss") && !t.ends_with("us") && n > 3))
                    && t.bytes().any(|b| vowel(b) || b == b'y'))
    }

    /// The running and done forms of a description's first word, a prefix's hyphen kept (`Re-ran`).
    fn forms(word: &str) -> Option<(String, String)> {
        if let Some((p, c)) = word.split_once('-')
            && crate::anchor!("turn.agent.prefixes")
                .split('|')
                .any(|x| x.eq_ignore_ascii_case(p))
            && verb(c)
        {
            let f = format!("{p}{c}").to_ascii_lowercase();
            if participle(&f) || participle(&c.to_ascii_lowercase()) {
                return None;
            }
            let joined = |table: &'static str, form: fn(&str) -> String| {
                let own = irregular(table, &f).and_then(|v| v.get(p.len()..));
                format!(
                    "{p}-{}",
                    capital(c, own.map_or_else(|| form(c), str::to_owned))
                )
            };
            return Some((joined(RUNNING, running), joined(PAST, done)));
        }
        verb(word).then(|| (capital(word, running(word)), capital(word, done(word))))
    }

    /// The bases `form` may be drawn from, by the tables and the suffix rules.
    fn candidates(form: &str) -> Vec<String> {
        let t = form.to_ascii_lowercase();
        let n = form.len();
        let mut out: Vec<String> = bases(PAST, &t)
            .chain(bases(RUNNING, &t))
            .map(str::to_owned)
            .collect();
        if t.ends_with("ed") {
            out.extend([&form[..n - 2], &form[..n - 1]].map(str::to_owned));
            if n > 3 {
                out.extend([format!("{}y", &form[..n - 3]), form[..n - 3].to_owned()]);
            }
            out.extend(
                RUNNING
                    .split(',')
                    .filter_map(|pair| pair.split_once(':'))
                    .filter(|(_, v)| {
                        v.trim_matches('"')
                            .strip_suffix("ing")
                            .is_some_and(|s| format!("{s}ed") == t)
                    })
                    .map(|(k, _)| k.to_owned()),
            );
        }
        if t.ends_with("ing") && n > 3 {
            out.extend([form[..n - 3].to_owned(), format!("{}e", &form[..n - 3])]);
            if n > 4 {
                out.extend([format!("{}ie", &form[..n - 4]), form[..n - 4].to_owned()]);
            }
        }
        out
    }

    /// Whether `word` is how the renderer draws a description's first word, done or running, the case of its first letter aside.
    pub(super) fn conjugated(word: &str) -> bool {
        let same = |drawn: &str| {
            drawn.len() == word.len()
                && drawn[1..] == word[1..]
                && drawn[..1].eq_ignore_ascii_case(&word[..1])
        };
        if word.is_empty() || !word.is_ascii() {
            return false;
        }
        let bases: Vec<String> = match word.split_once('-') {
            // A prefix's own irregular form keeps its part past the prefix (`over-ride`: `Over-rode`).
            Some((p, c)) => {
                let (p_low, c_low) = (p.to_ascii_lowercase(), c.to_ascii_lowercase());
                let own = [PAST, RUNNING].into_iter().flat_map(|table| {
                    table.split(',').filter_map(|pair| {
                        let (k, v) = pair.split_once(':')?;
                        let v = v.trim_matches('"');
                        (k.starts_with(&p_low) && v.get(p.len()..) == Some(c_low.as_str()))
                            .then(|| k[p.len()..].to_owned())
                    })
                });
                candidates(c)
                    .into_iter()
                    .chain(own.collect::<Vec<_>>())
                    .map(|b| format!("{p}-{b}"))
                    .collect()
            }
            None => candidates(word),
        };
        bases
            .iter()
            .any(|base| forms(base).is_some_and(|(running, done)| same(&running) || same(&done)))
    }
}

/// Returns whether `row` is a column-0 turn header that stops a scrollback history scan.
#[must_use]
pub fn turn_history_stop(row: &str) -> bool {
    !row.is_empty() && !row.starts_with(char::is_whitespace) && !row.starts_with(['⏺', '●'])
}

fn is_monitor_event(row: &str) -> bool {
    row.strip_prefix(['⏺', '●'])
        .is_some_and(|rest| crate::text::trim_start(rest).starts_with("Monitor event:"))
}

/// The turn's messages above [`said_tail`]'s, each read by `read`, and whether the turn's opening
/// row is on `rows`: the transcript's rows read up to the side panel's rule, in its room
/// ([`Transcript`]).
fn turn_before<'a>(
    screen: &[String],
    transcript: &'a Transcript<'_>,
    read: impl Fn(&[(usize, &'a str)]) -> String,
) -> Option<(bool, Vec<String>)> {
    let (head, _) = said_message_rows(screen, transcript)?;
    let rows: &'a [String] = &transcript.rows;
    if !rows[head].starts_with(['⏺', '●']) {
        return None;
    }
    let width = transcript.room(crate::question::screen_width(rows));
    let mut said: Vec<String> = Vec::new();
    let mut opened = false;
    let mut below = head;
    for i in (0..head).rev() {
        let r = &rows[i];
        if crate::text::trim(r).is_empty() || leading_spaces(r) > 0 {
            continue;
        }
        if !r.starts_with(['⏺', '●']) {
            opened = true;
            break;
        }
        let own_row = is_tool_call(r)
            || bare_call(rows, i, below)
            || tool_group(rows, i, below, width)
            || task_notice(rows, i, below)
            || is_monitor_event(r)
            || crate::phase::is_stop_hook_summary(r)
            || crate::phase::vendor_paragraph(rows, i) == Some(i);
        if !own_row {
            let last = (i..below)
                .rev()
                .find(|&k| !crate::text::trim(&rows[k]).is_empty())
                .unwrap_or(i);
            if let Some((_, words)) = message_rows(rows, last).filter(|(h, _)| *h == i) {
                let plain = crate::wrap::said_rows(&words);
                if !is_idle_notice(&plain) && !is_hook_verdict(&plain) {
                    said.push(read(&words));
                }
            }
        }
        below = i;
    }
    said.reverse();
    Some((opened, said))
}

fn said_message(rows: &[String], transcript: &Transcript<'_>) -> Option<(usize, String)> {
    said_message_rows(rows, transcript).map(|(head, said)| (head, crate::wrap::said_rows(&said)))
}

/// The rows of the agent's last message on `screen` and its head row, read on the transcript's
/// rows up to the side panel's rule ([`Transcript`]).
fn said_message_rows<'a>(
    screen: &[String],
    transcript: &'a Transcript<'_>,
) -> Option<(usize, Vec<(usize, &'a str)>)> {
    let rows: &'a [String] = &transcript.rows;
    let (head, said) = message_rows(rows, last_said_in(screen, transcript)?)?;
    let (head, said) = if is_idle_notice(&crate::wrap::said_rows(&said)) {
        let above = before_idle_compaction(rows, head);
        let last = (0..above)
            .rev()
            .find(|&i| crate::phase::is_said(&rows[i], None))?;
        message_rows(rows, last)?
    } else {
        (head, said)
    };
    (!is_hook_verdict(&crate::wrap::said_rows(&said))).then_some((head, said))
}

fn is_hook_verdict(said: &str) -> bool {
    let words = said.split_whitespace().collect::<Vec<_>>().join(" ");
    let stopped = const { crate::anchor!("hook.stopped") };
    let opens = |label: &str| {
        words
            .strip_prefix(label)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    };
    words
        .strip_prefix(stopped)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(':'))
        || opens(const { crate::anchor!("hook.prompt_blocked") })
        || opens(const { crate::anchor!("hook.expansion_blocked") })
}

/// Extracts the user's opening `❯` prompt for the turn ended by [`said_tail`].
#[must_use]
pub fn answered_prompt(rows: &[String]) -> Option<String> {
    let frame = composer_frame(rows)?;
    let transcript = Transcript::of(rows);
    let (head, _) = said_message(rows, &transcript)?;
    let rows: &[String] = &transcript.rows;
    if head >= frame.top || !rows[head].starts_with(['⏺', '●']) {
        return None;
    }
    let open = (0..head).rev().find(|&i| {
        let r = &rows[i];
        !crate::text::blank(r) && leading_spaces(r) == 0 && !r.starts_with(['⏺', '●'])
    })?;
    let first = rows[open].strip_prefix('❯')?;
    let mut words = vec![crate::text::trim(first)];
    for row in &rows[open + 1..head] {
        let t = crate::text::trim(row);
        if t.is_empty() {
            continue;
        }
        if leading_spaces(row) == 0 || t.starts_with('⎿') {
            break;
        }
        words.push(t);
    }
    let said = words.join(" ");
    (!crate::text::trim(&said).is_empty()).then_some(said)
}

fn message_at(rows: &[String], last: usize) -> Option<(usize, String)> {
    message_rows(rows, last).map(|(head, said)| (head, crate::wrap::said_rows(&said)))
}

/// The rows of the `⏺` message that row `last` ends, each with its text there.
fn message_rows(rows: &[String], last: usize) -> Option<(usize, Vec<(usize, &str)>)> {
    let head = (0..=last).rev().find(|&i| {
        let r = &rows[i];
        !crate::text::trim(r).is_empty() && leading_spaces(r) == 0
    });
    let (head, headless) = match head {
        Some(h) => (h, false),
        None => (0, true),
    };
    if !headless && (!rows[head].starts_with(['⏺', '●']) || is_tool_call(&rows[head])) {
        return None;
    }
    let mut out: Vec<(usize, &str)> = Vec::new();
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
        out.push((i, t));
    }
    (!out.is_empty()).then_some((head, out))
}

fn is_idle_notice(said: &str) -> bool {
    let notice = const { crate::anchor!("turn.idle_compacted") };
    let mut words = said.split_whitespace();
    notice.split_whitespace().all(|w| words.next() == Some(w))
        && words.next().is_none_or(|w| w == "·")
}

/// Claude Code's row for a task the compaction restored, `line` its rows rejoined: `⏺ Task "…" `
/// and the task's status, in the words its renderer gives it and in no others (read in the 2.1.310,
/// 2.1.312, 2.1.314 and 2.1.316 binaries: `running` is `still running in background`, `completed`
/// is `completed in background`, `killed` is `stopped`, and any other status is drawn as its own
/// word, `failed` or `pending`). A reply that opens `Task "…"` goes on in words of its own.
fn is_task_status(line: &str) -> bool {
    let drawn = |status: &str| {
        [
            const { crate::anchor!("turn.task_running") },
            const { crate::anchor!("turn.task_completed") },
            const { crate::anchor!("turn.task_stopped") },
        ]
        .contains(&status)
            || (!status.is_empty() && status.bytes().all(|b| b.is_ascii_lowercase()))
    };
    line.strip_prefix(['⏺', '●'])
        .map(str::trim_start)
        .and_then(|row| row.strip_prefix(const { crate::anchor!("turn.task_status") }))
        .and_then(|rest| rest.rsplit_once("\" "))
        .is_some_and(|(task, status)| !task.is_empty() && drawn(status))
}

/// The row `line` (its rows rejoined) is an idle compaction's notice: a compaction an earlier one
/// left as the conversation's last words is no turn either.
fn is_idle_notice_row(line: &str) -> bool {
    line.strip_prefix(['⏺', '●'])
        .is_some_and(|said| is_idle_notice(said.trim_start()))
}

/// The first row an idle compaction drew, its notice opening on row `head`: walking up from it
/// over the rows the compaction drew above its notice (blank rows, `⎿` rows and the rows their
/// text wrapped onto, a `※ recap`, a restored task's row, an earlier compaction's notice). `rows`
/// are the transcript's, each cut at the side panel's rule ([`Transcript`]), so a row blank up to
/// the rule is a blank row and a task row's words end at the rule.
fn before_idle_compaction(rows: &[String], head: usize) -> usize {
    let recap = const { crate::anchor!("turn.recap") };
    let mut k = head;
    while k > 0 {
        let r = &rows[k - 1];
        let t = crate::text::trim(r);
        if t.is_empty() || t.starts_with('⎿') {
            k -= 1;
            continue;
        }
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
                    .is_some_and(|rest| rest.trim_start().starts_with(recap))
                    || is_task_status(&crate::wrap::rejoined(&rows[i..k]))
                    || is_idle_notice_row(&crate::wrap::rejoined(&rows[i..k])) =>
            {
                k = i;
            }
            _ => break,
        }
    }
    k
}

/// Where the rows Claude Code's idle compaction drew begin, its notice being the last thing said.
pub(crate) fn idle_compaction_from(rows: &[String]) -> Option<usize> {
    let transcript = Transcript::of(rows);
    let (head, said) = message_at(&transcript.rows, last_said_in(rows, &transcript)?)?;
    is_idle_notice(&said).then(|| before_idle_compaction(&transcript.rows, head))
}

/// Returns whether the last turn ended on Claude Code's idle context-compaction notice.
#[must_use]
pub fn idle_compacted(rows: &[String]) -> bool {
    idle_compaction_from(rows).is_some()
}

/// Returns whether the last turn was interrupted by the user (`⎿  Interrupted`), or ended on a
/// question the person declined with Esc (`⏺ User declined to answer questions`, the anchor
/// `question.declined`). The last block is read on the transcript ([`Transcript`]), each row up
/// to the side panel's rule, so beside the panel the declined row is its words alone.
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    let transcript = Transcript::of(rows);
    let end = transcript_end_in(rows, &transcript).min(rows.len());
    let rows = &transcript.rows;
    let Some(block) = (0..end)
        .rev()
        .find(|&i| rows[i].starts_with(['⏺', '●', '❯']))
    else {
        return false;
    };
    let said = rows[block].trim_start_matches(['⏺', '●']).trim();
    if rows[block].starts_with(['⏺', '●']) && said == const { crate::anchor!("question.declined") }
    {
        return true;
    }
    rows[block..end].iter().any(|r| interrupt_notice(r))
}

/// Whether `row` is Claude Code's notice that a person stopped the turn with Esc, under the call
/// or the reply it cut short (`⎿  Interrupted · What should Claude do instead?`, the anchor
/// `turn.interrupted`): its question is the vendor's to the person, never the agent asking.
#[must_use]
pub fn interrupt_notice(row: &str) -> bool {
    let text = const { crate::anchor!("turn.interrupted") };
    crate::text::trim_start(row)
        .strip_prefix('⎿')
        .is_some_and(|rest| crate::text::trim_start(rest).starts_with(text))
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
