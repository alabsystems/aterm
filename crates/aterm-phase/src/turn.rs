// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code launch-card geometry, continuation suggestions, `/goal` indicators, and turn-end reply extraction.

use crate::phase::{
    composer_frame, composer_text, is_against_right_edge, is_placeholder, is_tool_call,
    last_said_index, leading_spaces, status_block, transcript_end,
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

/// Returns whether Claude Code's `◎ /goal active` indicator is present in the live zone.
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

/// Returns whether the agent's final reply `said` ends with a question mark (`?`).
pub(crate) fn asks(said: &str) -> bool {
    said.lines()
        .last()
        .is_some_and(|l| crate::text::trim_end(l).ends_with('?'))
}

/// Extracts the worker's final `⏺` message from the last turn on `rows`.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    said_message(rows).map(|(_, said)| said)
}

/// Returns whether the `⏺` header of [`said_tail`] is visible on `rows`.
#[must_use]
pub fn said_whole(rows: &[String]) -> bool {
    said_message(rows).is_some_and(|(head, _)| rows[head].starts_with(['⏺', '●']))
}

/// Extracts earlier `⏺` messages from the same turn above [`said_tail`].
#[must_use]
pub fn turn_said_before(rows: &[String]) -> Option<String> {
    let (_, said) = turn_before(rows)?;
    (!said.is_empty()).then(|| said.join("\n\n"))
}

/// Returns whether the entire last turn (from the user's `❯` prompt through [`said_tail`]) fits on `rows`.
#[must_use]
pub fn turn_whole(rows: &[String]) -> bool {
    turn_before(rows).is_some_and(|(opened, _)| opened)
}

/// Returns whether the last turn on `rows` ran a tool: a call's row (`⏺ Bash(…)`, [`stop_task`]) or a collapsed tool group's ([`tool_group`]) under the `❯` prompt that opened it.
#[must_use]
pub fn turn_ran_tools(rows: &[String]) -> bool {
    let end = crate::phase::composer_frame(rows).map_or(rows.len(), |f| f.top);
    let width = crate::question::screen_width(rows);
    (0..end)
        .rev()
        .take_while(|&i| !rows[i].starts_with('❯'))
        .any(|i| {
            is_tool_call(&rows[i]) || stop_task(rows, i, end) || tool_group(rows, i, end, width)
        })
}

/// TaskStop's call row, drawn with no `(…)` as its use message is empty: `⏺ Stop Task`, its `⎿` output row under it.
fn stop_task(rows: &[String], i: usize, until: usize) -> bool {
    rows[i].strip_prefix("⏺ ").is_some_and(|name| {
        crate::text::trim_end(name) == const { crate::anchor!("turn.call.stop_task") }
    }) && i + 1 < until
        && rows[i + 1].starts_with("  ⎿")
}

/// Returns whether the `❯` prompt that opened the last turn is above `rows`, so [`turn_ran_tools`] read the turn's lower rows only.
#[must_use]
pub fn turn_opens_above(rows: &[String]) -> bool {
    let end = crate::phase::composer_frame(rows).map_or(rows.len(), |f| f.top);
    !rows[..end].iter().any(|r| r.starts_with('❯'))
}

/// The verbs a collapsed tool group's first clause opens with, a count and its nouns after (`Read 1 file`, `Running 2 shell commands…`).
const GROUP_VERBS: [&str; 8] = [
    crate::anchor!("turn.group.reading"),
    crate::anchor!("turn.group.read"),
    crate::anchor!("turn.group.searched"),
    crate::anchor!("turn.group.searching"),
    crate::anchor!("turn.group.listed"),
    crate::anchor!("turn.group.listing"),
    crate::anchor!("turn.group.running"),
    crate::anchor!("turn.group.ran"),
];

/// The nouns a group's clause counts with no expand hint, singular or plural (`Read 1 file`, `Ran 2 shell commands`).
const GROUP_NOUNS: [&str; 4] = [
    crate::anchor!("turn.group.file"),
    crate::anchor!("turn.group.pattern"),
    crate::anchor!("turn.group.directory"),
    crate::anchor!("turn.group.shell_command"),
];

/// Whether row `i` opens Claude Code's collapsed tool group, read across the rows it wraps onto: a `⏺ ` or two-space row whose first clause is a verb, a count and up to three lower-case nouns (`⏺ Reading 1 file… (ctrl+o to expand)`), or `Called` and a server, then the expand hint; with no hint, every clause such a count of a [`GROUP_NOUNS`] noun, a two-space row standing alone under a blank row, another group or the screen's top (`  Read 1 file`), never a reply's next row. A stop hook's summary is no tool.
fn tool_group(rows: &[String], i: usize, until: usize, width: usize) -> bool {
    let row = rows[i].as_str();
    let headed = row.starts_with("⏺ ");
    if !headed && !(row.starts_with("  ") && leading_spaces(row) == 2) {
        return false;
    }
    let (_, line) = crate::phase::line_at(rows, i, until, width);
    let text = crate::text::trim(&line);
    let text = text.strip_prefix('⏺').map_or(text, crate::text::trim_start);
    let hinted = text.strip_suffix(const { crate::anchor!("turn.group.expand") });
    let text = crate::text::trim_end(hinted.unwrap_or(text));
    let text = text.strip_suffix('…').unwrap_or(text);
    let mut clauses = text.split(", ");
    let first = clauses.next().unwrap_or_default();
    match hinted {
        Some(_) => counted(first, false, false) || called(first),
        None => {
            let alone = headed
                || i == 0
                || crate::text::trim(&rows[i - 1]).is_empty()
                || tool_group(rows, i - 1, until, width);
            alone && counted(first, false, true) && clauses.all(|c| counted(c, true, true))
        }
    }
}

/// A group's clause of a verb ([`GROUP_VERBS`], lower-case past the first), a count and up to three lower-case nouns, `named` one of [`GROUP_NOUNS`]; a stop hook's is none.
fn counted(clause: &str, later: bool, named: bool) -> bool {
    GROUP_VERBS.iter().any(|verb| {
        let Some((n, nouns)) = clause
            .get(..verb.len())
            .filter(|head| {
                if later {
                    head.eq_ignore_ascii_case(verb)
                        && head.starts_with(|c: char| c.is_ascii_lowercase())
                } else {
                    head == verb
                }
            })
            .and_then(|_| clause[verb.len()..].strip_prefix(' '))
            .and_then(|r| r.split_once(' '))
        else {
            return false;
        };
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

/// Whether `nouns` is `noun`, or its plural (`files`, `directories`).
fn group_noun(nouns: &str, noun: &str) -> bool {
    nouns == noun
        || nouns.strip_suffix('s') == Some(noun)
        || noun
            .strip_suffix('y')
            .is_some_and(|stem| nouns.strip_suffix("ies") == Some(stem))
}

/// A group's `Called` clause: the server's name, and how many times past one (`Called github 2 times`).
fn called(clause: &str) -> bool {
    clause
        .strip_prefix(const { crate::anchor!("turn.group.called") })
        .and_then(|r| r.strip_prefix(' '))
        .is_some_and(|r| {
            let (server, times) = r.split_once(' ').unwrap_or((r, ""));
            !server.is_empty()
                && server
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                && (times.is_empty()
                    || times
                        .strip_suffix(" times")
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
        })
}

/// Returns whether `row` is a column-0 turn header that stops a scrollback history scan.
#[must_use]
pub fn turn_history_stop(row: &str) -> bool {
    !row.is_empty() && !row.starts_with(char::is_whitespace) && !row.starts_with(['⏺', '●'])
}

fn turn_before(rows: &[String]) -> Option<(bool, Vec<String>)> {
    let (head, _) = said_message(rows)?;
    if !rows[head].starts_with(['⏺', '●']) {
        return None;
    }
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
        if !is_tool_call(r) {
            let last = (i..below)
                .rev()
                .find(|&k| !crate::text::trim(&rows[k]).is_empty())
                .unwrap_or(i);
            if let Some((_, words)) = message_at(rows, last).filter(|(h, _)| *h == i) {
                said.push(words);
            }
        }
        below = i;
    }
    said.reverse();
    Some((opened, said))
}

fn said_message(rows: &[String]) -> Option<(usize, String)> {
    let (head, said) = message_at(rows, last_said_index(rows)?)?;
    let (head, said) = if is_idle_notice(&said) {
        let above = before_idle_compaction(rows, head);
        let last = (0..above)
            .rev()
            .find(|&i| crate::phase::is_said(&rows[i], None))?;
        message_at(rows, last)?
    } else {
        (head, said)
    };
    (!is_hook_verdict(&said)).then_some((head, said))
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
    let (head, _) = said_message(rows)?;
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

fn is_idle_notice(said: &str) -> bool {
    let notice = const { crate::anchor!("turn.idle_compacted") };
    let mut words = said.split_whitespace();
    notice.split_whitespace().all(|w| words.next() == Some(w))
        && words.next().is_none_or(|w| w == "·")
}

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
                    .is_some_and(|rest| rest.trim_start().starts_with(recap)) =>
            {
                k = i;
            }
            _ => break,
        }
    }
    k
}

/// Returns whether the last turn ended on Claude Code's idle context-compaction notice.
#[must_use]
pub fn idle_compacted(rows: &[String]) -> bool {
    last_said_index(rows)
        .and_then(|last| message_at(rows, last))
        .is_some_and(|(_, said)| is_idle_notice(&said))
}

/// Returns whether the last turn was interrupted by the user (`⎿  Interrupted`).
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
    if rows[block].starts_with(['⏺', '●']) && said == const { crate::anchor!("question.declined") }
    {
        return true;
    }
    let text = const { crate::anchor!("turn.interrupted") };
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

    #[test]
    fn a_turn_runs_tools_by_the_call_rows_under_its_prompt() {
        let shells = screen(crate::prompt::fixtures::IDLE_SHELLS_2_1_271);
        assert!(turn_ran_tools(&shells), "its two Bash calls");
        let frame = crate::phase::composer_frame(&shells)
            .expect("the composer")
            .top;
        let mut later = shells[..frame].to_vec();
        later.extend(rows(&["❯ thanks", "", "⏺ You're welcome.", ""]));
        later.extend(shells[frame..].iter().cloned());
        assert!(
            !turn_ran_tools(&later),
            "the control: the calls are a turn before"
        );
        assert!(!turn_ran_tools(&framed(
            &["⏺ Done.", ""],
            "  ? for shortcuts"
        )));
    }

    #[test]
    fn a_turn_opens_above_the_screen_when_no_prompt_is_drawn_over_its_composer() {
        let turn = framed(&["❯ go on", "", "⏺ Done.", ""], "  ? for shortcuts");
        assert!(!turn_opens_above(&turn), "its prompt drawn");
        assert!(turn_opens_above(&turn[1..]), "the rows under its prompt");
        assert!(
            turn_opens_above(&screen(crate::prompt::fixtures::IDLE_SHELLS_2_1_271)),
            "a screen of a turn's last calls and words"
        );
    }

    #[test]
    fn a_turn_runs_tools_by_its_collapsed_group_row() {
        use crate::prompt::fixtures::{
            BOX_BASH_FIND_CONTINUED_2_1_308, BOX_EDIT, BOX_FETCH_2_1_287, BOX_READ_OUTSIDE_2_1_284,
            BOX_READ_OUTSIDE_2_1_292, COLLAPSED_GROUP_CONTEXT_WALL_MEASURED,
            IDLE_AFTER_LIMIT_AND_MODEL_SWITCH, SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED,
            STOP_HOOK_CONTEXT_WALL_MEASURED, STOP_HOOK_STOP_REASON_DETAILED_MEASURED,
            STOP_HOOKS_VERBOSE_PROMPT_ROW_MEASURED, WRAPPED_GROUP_ROW_CONTEXT_WALL,
        };
        // A turn of the measured `row` alone, under the screen above it.
        let ended_on = |text: &str, row: &str| -> Vec<String> {
            let r = screen(text);
            let at = r.iter().rposition(|x| x == row).expect("the row measured");
            let mut out = r[..at].to_vec();
            out.extend(rows(&["❯ go on", "", row, ""]));
            out.extend(composer("  ? for shortcuts"));
            out
        };
        let swapped = |rows: &[String], from: &str, to: &str| -> Vec<String> {
            let at = rows.iter().rposition(|r| r == from).expect("the group row");
            let mut out = rows.to_vec();
            out[at] = to.to_owned();
            out
        };
        for (name, text, group) in [
            ("2.1.280, no hint", BOX_EDIT, "  Read 1 file"),
            (
                "2.1.284",
                BOX_READ_OUTSIDE_2_1_284,
                "⏺ Reading 1 file… (ctrl+o to expand)",
            ),
            (
                "2.1.292",
                BOX_READ_OUTSIDE_2_1_292,
                "⏺ Reading 1 file… (ctrl+o to expand)",
            ),
            (
                "2.1.287",
                BOX_FETCH_2_1_287,
                "  Read 1 file (ctrl+o to expand)",
            ),
            (
                "2.1.294",
                COLLAPSED_GROUP_CONTEXT_WALL_MEASURED,
                "  Read 1 file (ctrl+o to expand)",
            ),
            (
                "2.1.294, an MCP server's",
                SHELL_MODE_OUTPUT_ENDS_TURN_MEASURED,
                "  Called github (ctrl+o to expand)",
            ),
            (
                "2.1.308",
                BOX_BASH_FIND_CONTINUED_2_1_308,
                "  Listed 1 directory (ctrl+o to expand)",
            ),
            (
                "2.1.308",
                BOX_BASH_FIND_CONTINUED_2_1_308,
                "⏺ Searching for 1 pattern… (ctrl+o to expand)",
            ),
            (
                "the 2026-09-12 capture, shell commands",
                IDLE_AFTER_LIMIT_AND_MODEL_SWITCH,
                "  Ran 2 shell commands",
            ),
        ] {
            let r = ended_on(text, group);
            assert!(turn_ran_tools(&r), "{name}: {group:?}");
            for prose in [
                "⏺ Reading the parser.",
                "  Read 1 file before the rest",
                "  Read 2 files, then fixed the parser",
                "  Ran 2 shell commands.",
                "  Called github about it",
            ] {
                assert!(
                    !turn_ran_tools(&swapped(&r, group, prose)),
                    "the control, {name}: {prose:?} is words"
                );
            }
        }
        for (name, text, hook) in [
            (
                "2.1.294",
                STOP_HOOK_CONTEXT_WALL_MEASURED,
                "⏺ Ran 1 stop hook (ctrl+o to expand)",
            ),
            (
                "2.1.302, no hint",
                STOP_HOOK_STOP_REASON_DETAILED_MEASURED,
                "⏺ Ran 1 stop hook",
            ),
            (
                "2.1.304",
                STOP_HOOKS_VERBOSE_PROMPT_ROW_MEASURED,
                "⏺ Ran 2 stop hooks",
            ),
        ] {
            assert!(
                !turn_ran_tools(&ended_on(text, hook)),
                "the control, {name}: a stop hook is no tool"
            );
        }
        // A reply's next row is its words, a count or not; standing alone, a noun no group counts is words too.
        for said in [
            &[
                "⏺ Nothing changed since my last report:",
                "  Ran 312 tests",
                "  The suite is green and I am waiting on review.",
            ][..],
            &["⏺ Rebuilt and reran the suite:", "  Ran 2 shell commands"],
            &[
                "⏺ Nothing changed since my last report.",
                "",
                "  Ran 312 tests",
            ],
        ] {
            let mut turn = rows(&["❯ go on", ""]);
            turn.extend(rows(said));
            turn.extend(rows(&["", "✻ Cogitated for 3s"]));
            turn.extend(composer("  ? for shortcuts"));
            assert!(!turn_ran_tools(&turn), "the control: {said:?} is words");
        }
        // TaskStop's row, as the 2026-09-12 capture draws it, its output under it.
        let r = screen(IDLE_AFTER_LIMIT_AND_MODEL_SWITCH);
        let at = r
            .iter()
            .rposition(|x| x == "⏺ Stop Task")
            .expect("the row measured");
        let mut stopped = rows(&["❯ go on", ""]);
        stopped.extend(r[at..at + 3].iter().cloned());
        stopped.extend(rows(&[
            "",
            "⏺ Stopped the rebuild.",
            "",
            "✻ Cogitated for 3s",
        ]));
        stopped.extend(composer("  ? for shortcuts"));
        assert!(turn_ran_tools(&stopped), "{:?}", &r[at..at + 3]);
        assert!(
            !turn_ran_tools(&swapped(&stopped, "⏺ Stop Task", "⏺ Stop Task now")),
            "the control: words"
        );
        let mut said = stopped.clone();
        said.drain(3..5);
        assert!(!turn_ran_tools(&said), "the control: no output under it");
        // ExitPlanMode's row hands a plan to a person: no tool run by design (2.1.300).
        let plan = screen(crate::prompt::fixtures::PLAN_READY_70_2_1_300);
        let at = plan
            .iter()
            .position(|x| x == "⏺ Exited plan mode")
            .expect("the row measured");
        let mut planned = rows(&["❯ go on", ""]);
        planned.extend(plan[at..at + 2].iter().cloned());
        planned.extend(rows(&["⏺ Tool call done.", "", "✻ Sautéed for 0s"]));
        planned.extend(composer("  ? for shortcuts"));
        assert!(!turn_ran_tools(&planned), "{planned:?}");
        // Hand-built; `Searched for` unwrapped is measured (2.1.295) in aterm-agent's
        // `claude_codes_measured_group_rows_are_a_tool_run`.
        let wrapped = screen(WRAPPED_GROUP_ROW_CONTEXT_WALL);
        assert!(
            turn_ran_tools(&wrapped),
            "the group row wrapped at its hint"
        );
        let hooked = swapped(
            &wrapped,
            "  Searched for 2 patterns, read 1 file (ctrl+o",
            "⏺ Ran 1 stop hook (ctrl+o",
        );
        assert!(
            !turn_ran_tools(&hooked),
            "the control: a stop hook is no tool"
        );
    }

    #[test]
    fn the_dim_suggestion_is_read_and_a_typed_draft_is_not() {
        let r = screen(GOAL_ACTIVE_SUGGESTION);
        assert_eq!(
            continuation_suggestion(&r, 2).as_deref(),
            Some("keep going")
        );
        assert_eq!(continuation_suggestion(&r, 12), None, "typed text");
        let b = screen(BOX_BASH_TOUCH);
        assert_eq!(continuation_suggestion(&b, 2), None);
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
        let user = framed(&["❯ carry on with the parser", ""], "  ? for shortcuts");
        assert_eq!(said_tail(&user), None);
        let tool = framed(
            &["⏺ Bash(cargo test)", "  ⎿  test result: ok", ""],
            "  ? for shortcuts",
        );
        assert_eq!(said_tail(&tool), None);
    }

    #[test]
    fn a_message_is_whole_only_where_its_head_is_on_the_rows() {
        let whole = framed(
            &[
                "⏺ Merged it.",
                "",
                "  Nothing left: all of it is on main.",
                "",
            ],
            "  ? for shortcuts",
        );
        assert!(said_whole(&whole));
        let cut = framed(
            &[
                "  - finding 29: details",
                "",
                "  Nothing left: all of it is on main.",
                "",
            ],
            "  ? for shortcuts",
        );
        assert!(said_tail(&cut).is_some(), "the visible rows are read");
        assert!(!said_whole(&cut));
        let user = framed(&["❯ carry on", ""], "  ? for shortcuts");
        assert!(!said_whole(&user));
    }

    #[test]
    fn the_turns_other_words_are_its_messages_above_the_last() {
        let turn = framed(
            &[
                "⏺ An older turn's words.",
                "",
                "❯ land the parser",
                "",
                "⏺ Merged the lexer.",
                "  NEEDS YOU: the signing credential",
                "",
                "⏺ Bash(git push)",
                "  ⎿  Everything up-to-date",
                "",
                "⏺ Pushed the docs too.",
                "",
                "⏺ Nothing left: all of it is on main.",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(
            said_tail(&turn).as_deref(),
            Some("Nothing left: all of it is on main.")
        );
        assert_eq!(
            turn_said_before(&turn).as_deref(),
            Some("Merged the lexer.\nNEEDS YOU: the signing credential\n\nPushed the docs too.")
        );
        let alone = framed(
            &[
                "⏺ An older turn's words.",
                "",
                "❯ land the parser",
                "",
                "⏺ Nothing left: all of it is on main.",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(turn_said_before(&alone), None);
        let cut = framed(
            &["  - finding 29: details", "  Nothing left: on main.", ""],
            "  ? for shortcuts",
        );
        assert_eq!(turn_said_before(&cut), None);
    }

    #[test]
    fn a_turn_whose_start_is_above_the_rows_is_no_whole_turn() {
        let body = [
            "⏺ Bash(git push)",
            "  ⎿  Everything up-to-date",
            "",
            "⏺ Nothing left: all of it is on main.",
            "",
        ];
        let cut = framed(&body, "  ? for shortcuts");
        assert!(said_whole(&cut) && !turn_whole(&cut));
        for opener in ["❯ land the parser", "✻ Worked for 3m 2s"] {
            let mut rows = vec![opener, ""];
            rows.extend(body);
            let whole = framed(&rows, "  ? for shortcuts");
            assert!(turn_whole(&whole), "{opener}");
        }
        let tall = framed(
            &["  - finding 29: details", "  Nothing left: on main.", ""],
            "  ? for shortcuts",
        );
        assert!(!turn_whole(&tall));
        assert!(turn_history_stop("❯ land the parser") && turn_history_stop("✻ Worked for 3m"));
        for row in [
            "⏺ Merged the lexer.",
            "● Bash(ls)",
            "  NEEDS YOU: the key",
            "",
        ] {
            assert!(!turn_history_stop(row), "{row}");
        }
    }

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

    #[test]
    fn claude_codes_idle_compaction_is_its_own_housekeeping() {
        use crate::prompt::fixtures::{IDLE_COMPACTED_HINT_MEASURED, IDLE_COMPACTED_MEASURED};
        use crate::reader::Program;
        for fixture in [IDLE_COMPACTED_MEASURED, IDLE_COMPACTED_HINT_MEASURED] {
            let measured = screen(fixture);
            assert!(idle_compacted(&measured));
            assert!(Program::Claude.reader().housekeeping(&measured));
            assert_eq!(said_tail(&measured).as_deref(), Some("ATERM-TASK-DONE"));
            assert_eq!(
                crate::phase::worker_phase(&measured),
                crate::phase::Phase::Idle
            );
            for p in [Program::Generic, Program::Codex] {
                assert!(!p.reader().housekeeping(&measured), "{p:?}");
            }
        }
        let foot = "  ⏵⏵ bypass permissions on (shift+tab to cycle)";
        for body in [
            &[
                "⏺ Compacted while idle, before the prompt",
                "  cache expired",
                "",
            ][..],
            &[
                "⏺ Compacted while idle, before the prompt cache expired ·\u{a0}turn off",
                "  with \"idleCompaction\": false in settings.json",
                "",
            ],
        ] {
            assert!(idle_compacted(&framed(body, foot)), "{body:?}");
        }
        for body in [
            &[
                "⏺ Compacted while idle, before the prompt cache expired, so the next",
                "  turn starts from the summary.",
                "",
            ][..],
            &[
                "⏺ Compacted while idle, before the prompt cache expired and moved on.",
                "",
            ],
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
        let mut below = measured("claude-2.1.283-launch-repl-ready.txt");
        let card: Vec<String> = below
            .iter()
            .filter(|r| r.contains("Claude Code v") || r.contains("Claude Max"))
            .cloned()
            .collect();
        below.extend(card);
        assert_eq!(launch_card(&below), None);
        assert_eq!(
            launch_card(&measured("claude-2.1.283-footer-busy.txt")),
            None
        );
        assert_eq!(
            launch_card(&measured("claude-2.1.283-effort-switch.txt")),
            None
        );
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

    fn card_rows() -> Vec<String> {
        measured("claude-2.1.294-session-limit-then-local-commands.txt")[..3].to_vec()
    }

    fn header_version(header: &str) -> Option<&str> {
        header
            .split_once("claude-code ")?
            .1
            .split_whitespace()
            .next()
    }

    #[test]
    fn every_recorded_card_is_placed_or_refused_by_its_layout() {
        let banner = crate::anchor!("launch.banner");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fixtures");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .expect("the fixtures")
            .map(|e| e.expect("a fixture").path())
            .collect();
        paths.sort();
        let (mut placed, mut narrow, mut entrance) = (0, 0, 0);
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
                || name.contains("-narrow-")
            {
                let top = rows.iter().rposition(|r| r.contains(banner)).unwrap();
                let (text_col, mascot_end) = if name.contains("-workspace-") {
                    (13, 11)
                } else {
                    (11, 9)
                };
                let place = place.unwrap_or_else(|| panic!("{name}: the narrow card"));
                assert_eq!(
                    (
                        place.top,
                        place.rows,
                        place.mascot_row,
                        place.text_col,
                        place.mascot_end,
                        place.ends[0]
                    ),
                    (top, 4, 1, text_col, mascot_end, text_col + 20),
                    "{name}: the narrow card"
                );
                narrow += 1;
                continue;
            }
            if name.contains("-entrance-") && (name.contains("arms-up-") || name.contains("-dust-"))
            {
                assert_eq!(
                    place, None,
                    "{name}: the mascot's arms up, or a row low, mid-entrance"
                );
                entrance += 1;
                continue;
            }
            let place = place.unwrap_or_else(|| panic!("{name}: no card placed"));
            let newest = rows
                .iter()
                .rposition(|r| r.starts_with(" \u{2590}") && r.contains(banner))
                .unwrap();
            let (text_col, mascot_end) = if name.contains("-entrance-") && name.contains("-stand-")
            {
                (13, 9)
            } else if name.contains("-workspace-") {
                (13, 11)
            } else {
                (11, 9)
            };
            assert_eq!(
                (
                    place.top,
                    place.rows,
                    place.text_col,
                    place.mascot_end,
                    place.ends[0]
                ),
                (newest, 3, text_col, mascot_end, text_col + 20),
                "{name}"
            );
            assert!(
                place.ends[1..].iter().all(|&e| e > text_col),
                "{name}: {place:?}"
            );
            placed += 1;
        }
        assert!(
            placed >= 150 && narrow >= 8 && entrance >= 4,
            "the corpus must exercise the reader: {placed} placed, {narrow} narrow, \
             {entrance} mid-entrance"
        );
    }

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
            ("claude-2.1.302-launch-144col.txt", 0, [31, 39, 36]),
        ] {
            assert_eq!(
                launch_card_place(&measured(name)),
                Some(CardPlace {
                    top,
                    rows: 3,
                    mascot_row: 0,
                    text_col: 11,
                    mascot_end: 9,
                    ends,
                }),
                "{name}"
            );
        }
        for (name, top) in [
            ("claude-2.1.302-launch-workspace-144col.txt", 0),
            ("claude-2.1.302-launch-workspace-120col.txt", 0),
            ("claude-2.1.302-launch-workspace-80col.txt", 0),
            ("claude-2.1.302-launch-workspace-61col.txt", 0),
            ("claude-2.1.302-launch-workspace-fullscreen-144col.txt", 1),
        ] {
            assert_eq!(
                launch_card_place(&measured(name)),
                Some(CardPlace {
                    top,
                    rows: 3,
                    mascot_row: 0,
                    text_col: 13,
                    mascot_end: 11,
                    ends: [33, 59, 38],
                }),
                "{name}"
            );
        }
    }

    #[test]
    fn the_wand_flares_in_claudes_two_inks() {
        let off = |c: [u8; 3], by: i16| c.map(|v| (i16::from(v) + by).clamp(0, 255) as u8);
        for ink in [WAND_FLARE_DARK, WAND_FLARE_LIGHT] {
            assert!(wand_flare('\u{2726}', ink), "{ink:?}");
            let t = i16::from(WAND_FLARE_TOLERANCE);
            assert!(
                wand_flare('\u{2726}', off(ink, -t)),
                "{ink:?} within tolerance"
            );
            let mut far = ink;
            far[1] = far[1].wrapping_sub(WAND_FLARE_TOLERANCE + 1);
            assert!(!wand_flare('\u{2726}', far), "{ink:?}: a channel past it");
            for ch in ['\u{2571}', '\u{2588}', '*'] {
                assert!(!wand_flare(ch, ink), "{ch:?} in {ink:?}");
            }
        }
        let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
        for measured in [
            0xffe18d, 0xffd77b, 0xffcc68, 0xf08048, 0xfdc364, 0xffd476, 0xffdc84, 0xffe492,
            0xffeca0, 0xaf87ff, 0x8700ff, 0xd0a264, 0xa76e27, 0xd77757,
        ] {
            assert!(
                !wand_flare('\u{2726}', rgb(measured)),
                "{measured:06x} is no flare"
            );
        }
        assert!(
            wand_flare('\u{2726}', rgb(0xfffceb)),
            "the measured dark flare"
        );
        let plain = measured("claude-2.1.302-launch-144col.txt");
        let place = launch_card_place(&plain).expect("the plain card");
        assert_eq!(place.wand(), Some((8, 0)), "the mascot's right edge");
        assert_ne!(plain[0].chars().nth(8), Some('\u{2726}'));
    }

    #[test]
    fn the_workspace_card_is_placed_with_its_wand() {
        let card = measured("claude-2.1.302-launch-workspace-144col.txt");
        assert_eq!(
            card[0],
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}\u{2584}\u{2571}\u{2726}  Claude Code v2.1.302",
            "the measured banner row"
        );
        let place = launch_card_place(&card).expect("the workspace card");
        assert_eq!((place.top, place.text_col), (0, 13));
        assert_eq!(place.wand(), Some((10, 0)), "the wand's cell");
        assert_eq!(
            card[0].chars().nth(10),
            Some('\u{2726}'),
            "the wand's `✦` is where the place says"
        );
        assert_eq!(launch_card_version(&card, &place), Some("2.1.302"));
        assert_eq!(
            launch_card(&card),
            Some(LaunchCard {
                version: "2.1.302".into(),
                model: "Opus 5.5".into(),
                effort: Some("xhigh".into()),
            })
        );
        let entrance =
            measured("claude-2.1.302-launch-workspace-fullscreen-entrance-arms-up-144col.txt");
        assert!(
            entrance[1].starts_with("\u{2597}\u{259F}"),
            "{:?}",
            entrance[1]
        );
        assert_eq!(launch_card_scan(&entrance), (None, None));
    }

    fn jump(pose: &str) -> Vec<String> {
        measured(&format!(
            "claude-2.1.302-launch-workspace-fullscreen-entrance-jump-{pose}-144col.txt"
        ))
    }

    fn jump_entrance() -> Vec<Vec<String>> {
        let poses = ["1-stand", "2-dust", "3-dust", "4-arms-up"];
        let mut frames: Vec<_> = poses.iter().chain(&poses).map(|p| jump(p)).collect();
        frames.extend([jump("1-stand"), jump("5-rest")]);
        frames
    }

    fn follow(
        rows: &[String],
        kept: Option<&(CardPlace, Vec<String>)>,
    ) -> Option<(CardPlace, Vec<String>)> {
        let scan = launch_card_scan(rows);
        if let Some((place, words)) = kept {
            let words: Vec<&str> = words.iter().map(String::as_str).collect();
            if launch_card_held(rows, scan, place, &words) {
                return kept.cloned();
            }
        }
        let place = scan.1?;
        let words = launch_card_words(rows, &place).expect("a placed card's words");
        Some((place, owned(&words)))
    }

    fn owned(words: &CardWords<'_>) -> Vec<String> {
        words.as_slice().iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn the_entrance_holds_the_card_it_placed() {
        let frames = jump_entrance();
        let mut kept = None;
        let mut shown = Vec::new();
        for rows in &frames {
            kept = follow(rows, kept.as_ref());
            shown.push(kept.as_ref().map(|(place, _)| *place));
        }
        let stand = launch_card_place(&frames[0]).expect("the mascot standing");
        assert_eq!((stand.top, stand.text_col, stand.mascot_end), (1, 13, 9));
        let rest = launch_card_place(&frames[9]).expect("the mascot at rest");
        assert_eq!((rest.top, rest.text_col, rest.mascot_end), (1, 13, 11));
        let mut want = vec![Some(stand); 9];
        want.push(Some(rest));
        assert_eq!(
            shown, want,
            "the card on every frame, held through the entrance"
        );
        let (_, words) = kept.expect("the card");
        assert_eq!(
            words,
            [
                "Claude Code v2.1.302",
                "Opus 5.5 with xhigh effort \u{b7} API Usage Billing",
                "/private/tmp/at.TOxX/proj",
            ]
            .map(str::to_owned)
        );
        let bare: Vec<bool> = frames
            .iter()
            .map(|f| launch_card_place(f).is_some())
            .collect();
        assert_eq!(
            bare,
            [
                true, false, false, false, true, false, false, false, true, true
            ]
        );
        assert_eq!(launch_card_scan(&frames[1]), (Some(1), None));
        assert_eq!(launch_card_scan(&frames[2]), (Some(1), None));
        assert_eq!(launch_card_scan(&frames[3]), (None, None));
        for alone in [
            jump("2-dust"),
            jump("3-dust"),
            jump("4-arms-up"),
            measured("claude-2.1.302-launch-workspace-fullscreen-entrance-arms-up-144col.txt"),
        ] {
            assert_eq!(follow(&alone, None), None, "{alone:#?}");
        }
    }

    #[test]
    fn a_held_card_goes_with_its_banner() {
        let stand = jump("1-stand");
        let arms = jump("4-arms-up");
        let kept = follow(&stand, None).expect("the standing card");
        let place = kept.0;
        let then = |rows: &[String]| follow(rows, Some(&kept)).map(|(p, _)| p);
        assert_eq!(then(&arms), Some(place), "CONTROL: the raised arms hold it");

        let blank = || vec![String::new(); arms.len()];
        let up = |rows: &[String], by: usize| {
            let mut moved = rows[by..].to_vec();
            moved.resize(rows.len(), String::new());
            moved
        };
        let with = |rows: &[String], at: usize, new: &[&str]| {
            let mut edited = rows.to_vec();
            for (k, row) in new.iter().enumerate() {
                edited[at + k] = (*row).to_string();
            }
            edited
        };
        let model = &arms[2];
        assert_eq!(model.chars().nth(12), Some(' '));
        let over_mascot = format!("say hi, then{}", model.chars().skip(12).collect::<String>());
        let mut narrow = vec![String::new()];
        narrow.extend(measured("claude-2.1.302-launch-workspace-narrow-60col.txt"));
        let newer_narrow = with(&arms, 10, &["             Claude Code v2.1.302", ""]);
        let newer_wide = with(&arms, 10, &[&stand[1], &stand[2], &stand[3]]);
        for (what, rows, want) in [
            ("scrolled a row", up(&arms, 1), None),
            ("scrolled three rows", up(&arms, 3), None),
            ("cleared", blank(), None),
            (
                "a box over it",
                with(
                    &arms,
                    1,
                    &[
                        "\u{256D}\u{2500}\u{2500}\u{2500}\u{2500}\u{256E}",
                        "\u{2502} Do you want to proceed? \u{2502}",
                        "\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{256F}",
                    ],
                ),
                None,
            ),
            (
                "words over the mascot",
                with(&arms, 2, &[&over_mascot]),
                None,
            ),
            (
                "the program gone",
                with(&blank(), 0, &["$ claude", "$ "]),
                None,
            ),
            ("the narrow layout, the scan's card", narrow, Some(1)),
            ("a newer narrow banner below", newer_narrow, None),
            ("a newer card below", newer_wide, Some(10)),
            ("scrolled standing", up(&stand, 1), Some(0)),
        ] {
            assert_eq!(then(&rows).map(|p| p.top), want, "{what}: {rows:#?}");
        }
    }

    #[test]
    fn a_cards_words_open_with_its_banner() {
        let narrow = measured("claude-2.1.302-launch-workspace-narrow-60col.txt");
        let rest = jump("5-rest");
        let wide = launch_card_place(&rest).expect("the card at rest");
        let own = launch_card_place(&narrow).expect("the narrow card");
        assert_eq!((own.rows, own.mascot_row), (4, 1));
        assert_eq!(
            launch_card_words(&narrow, &own).map(|w| owned(&w)),
            Some(
                [
                    "Claude Code v2.1.302",
                    "Opus 5.5 with xhigh effort",
                    "API Usage Billing",
                    "/private/tmp/at.tPwT/proj",
                ]
                .map(str::to_owned)
                .to_vec()
            ),
        );
        let under = CardPlace { top: 1, ..wide };
        let words: Vec<_> = (1..4).map(|k| card_words(&narrow[k], 13)).collect();
        assert_eq!(
            words,
            [
                Some("Opus 5.5 with xhigh effort"),
                Some("API Usage Billing"),
                Some("/private/tmp/at.tPwT/proj"),
            ],
            "CONTROL: a card row's words, each: {narrow:#?}"
        );
        assert_eq!(launch_card_words(&narrow, &under), None, "{narrow:#?}");
        assert_eq!(
            launch_card_words(&rest, &wide).map(|w| owned(&w)),
            Some(
                [
                    "Claude Code v2.1.302",
                    "Opus 5.5 with xhigh effort \u{b7} API Usage Billing",
                    "/private/tmp/at.TOxX/proj",
                ]
                .map(str::to_owned)
                .to_vec()
            ),
            "CONTROL: the card's own words"
        );
    }

    #[test]
    fn the_workspace_narrow_card_is_placed_never_passed_over() {
        let under_a_card = |newest: &[String]| {
            let mut both = card_rows();
            both.extend(rows(&["", "\u{276F} exit", ""]));
            let at = both.len();
            both.extend(newest.iter().cloned());
            (at, both)
        };
        for name in [
            "claude-2.1.302-launch-workspace-narrow-60col.txt",
            "claude-2.1.302-launch-workspace-narrow-40col.txt",
            "claude-2.1.302-launch-workspace-narrow-34col.txt",
        ] {
            let narrow = measured(name);
            let place = launch_card_place(&narrow).expect("the narrow card");
            assert_eq!(
                (
                    place.top,
                    place.rows,
                    place.mascot_row,
                    place.text_col,
                    place.mascot_end
                ),
                (0, 4, 1, 13, 11),
                "{name}"
            );
            assert_eq!(place.wand(), Some((10, 1)), "{name}: the wand's cell");
            let (at, both) = under_a_card(&narrow);
            assert_eq!(
                launch_card_scan(&both),
                (Some(at), Some(CardPlace { top: at, ..place })),
                "{name}: {both:#?}"
            );
            let mascot_alone: String = narrow[1].chars().take(11).collect();
            let mut no_margin = narrow[1..4].to_vec();
            no_margin.push(narrow[3].clone());
            for (what, tail) in [
                ("cut under its banner", vec![]),
                ("over a blank row", vec![String::new()]),
                ("over the mascot without its text", vec![mascot_alone]),
                ("over a fourth row of text", no_margin),
            ] {
                let mut newest = vec![narrow[0].clone()];
                newest.extend(tail);
                let (at, both) = under_a_card(&newest);
                assert_eq!(
                    launch_card_scan(&both),
                    (Some(at), None),
                    "{name} {what}: {both:#?}"
                );
            }
        }
        let classic = measured("claude-2.1.300-prose-opens-with-limit-words-34col.txt");
        let quote: Vec<String> = classic[..4].iter().map(|r| format!("  {r}")).collect();
        assert_eq!(
            quote[0].find('C').map(|at| quote[0][..at].chars().count()),
            Some(NARROW_HELD_COL),
            "the quoted banner's column: {quote:#?}"
        );
        assert_eq!(launch_card_place(&quote), None, "{quote:#?}");
        let mut under = card_rows();
        under.extend(rows(&["", "\u{23FA} The narrow card read:", ""]));
        under.extend(quote);
        assert_eq!(
            launch_card_place(&under).map(|p| p.top),
            Some(0),
            "{under:#?}"
        );
        let wrapped = measured("claude-2.1.302-launch-workspace-narrow-26col.txt");
        assert_eq!(launch_card_scan(&wrapped), (None, None));
    }

    #[test]
    fn the_narrow_card_is_placed_by_its_measured_shape() {
        let capture = |name: &str| measured(&format!("claude-2.1.304-update-notice-{name}.txt"));
        for (name, top, text_col, mascot_end) in [
            ("narrow-workspace-fullscreen-50col", 1, 13, 11),
            ("narrow-workspace-fullscreen-34col", 1, 13, 11),
            ("narrow-workspace-fullscreen-33col", 1, 13, 11),
            ("narrow-workspace-inline-50col", 0, 13, 11),
            ("narrow-workspace-inline-33col", 0, 13, 11),
            ("narrow-xhigh-fullscreen-50col", 1, 11, 9),
            ("narrow-xhigh-fullscreen-31col", 1, 11, 9),
        ] {
            let screen = capture(name);
            let place = launch_card_place(&screen).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(
                (
                    place.top,
                    place.rows,
                    place.mascot_row,
                    place.text_col,
                    place.mascot_end
                ),
                (top, 4, 1, text_col, mascot_end),
                "{name}"
            );
            assert_eq!(launch_card_version(&screen, &place), Some("2.1.304"));
            assert_eq!(place.wand(), Some((mascot_end - 1, top + 1)), "{name}");
        }
        for name in [
            "narrow-workspace-fullscreen-32col",
            "narrow-xhigh-fullscreen-30col",
        ] {
            assert_eq!(launch_card_scan(&capture(name)), (None, None), "{name}");
        }
        let screen = capture("narrow-workspace-fullscreen-50col");
        assert!(
            crate::text::blank(&screen[5]),
            "CONTROL: the margin under the card: {screen:#?}"
        );
        let edited = |at: usize, row: String| {
            let mut s = screen.clone();
            s[at] = row;
            s
        };
        for (why, rows) in [
            (
                "words after the version",
                edited(1, format!("{} is out", screen[1])),
            ),
            (
                "the mascot's top row a column in",
                edited(2, format!(" {}", screen[2])),
            ),
            (
                "the model a column off",
                edited(2, screen[2].replacen("  Opus", "   Opus", 1)),
            ),
            (
                "a fifth row of the card's text",
                edited(5, screen[4].clone()),
            ),
            (
                "the billing's text gone",
                edited(3, screen[3].chars().take(10).collect()),
            ),
        ] {
            assert_eq!(launch_card_place(&rows), None, "{why}: {rows:#?}");
        }
    }

    #[test]
    fn a_row_naming_the_workspace_build_is_no_card() {
        let card = measured("claude-2.1.302-launch-workspace-144col.txt");
        for named in [
            "\u{23FA} Claude Code v2.1.302 holds a wand on workspace.",
            "  \u{2726} Claude Code v2.1.302",
            "\u{2726}\u{2571} Claude Code v2.1.302",
            " \u{2571}\u{2726}  Claude Code v2.1.302",
            "\u{276F} \u{2571}\u{2726} Claude Code v2.1.302",
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}\u{2584}x\u{2726}  Claude Code v2.1.302",
            " \u{2590} see  Claude Code v2.1.302",
        ] {
            let mut screen = vec![named.to_string()];
            screen.extend(card[1..3].iter().cloned());
            assert_eq!(launch_card_scan(&screen), (None, None), "{named:?}");
        }
        let mut wand_alone = card.clone();
        wand_alone[1] = format!(
            "         \u{2571}\u{2726}  {}",
            card[1].chars().skip(13).collect::<String>()
        );
        assert_eq!(launch_card_place(&wand_alone), None, "{wand_alone:#?}");
        for by in [2, 5] {
            let quote: Vec<String> = card[..3]
                .iter()
                .map(|r| format!("{}{r}", " ".repeat(by)))
                .collect();
            assert_eq!(launch_card_place(&quote), None, "quoted at {by}");
            let mut under = card_rows();
            under.extend(rows(&["", "\u{23FA} The card read:", ""]));
            under.extend(quote);
            assert_eq!(
                launch_card_place(&under).map(|p| p.top),
                Some(0),
                "quoted at {by}: {under:#?}"
            );
        }
    }

    #[test]
    fn a_relaunch_places_the_newer_card_and_never_falls_back() {
        let relaunch = measured("claude-2.1.283-inline-relaunch-repl-ready.txt");
        assert_eq!(launch_card_place(&relaunch).map(|p| p.top), Some(16));
        assert_eq!(launch_card_place(&relaunch[..16]).map(|p| p.top), Some(3));
        for cut in [17, 18] {
            assert_eq!(launch_card_place(&relaunch[..cut]), None, "rows[..{cut}]");
        }
    }

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

    #[test]
    fn a_narrow_banner_is_its_card_or_refused() {
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
        let whole = launch_card_place(&narrow).expect("the whole narrow card");
        assert_eq!((whole.top, whole.rows, whole.text_col), (0, 4, 11));
        let mut both = card_rows();
        both.extend(rows(&["", "❯ exit", ""]));
        let at = both.len();
        both.extend(narrow.iter().cloned());
        assert_eq!(
            launch_card_place(&both),
            Some(CardPlace { top: at, ..whole }),
            "the newest card, under an older one: {both:#?}"
        );
        for (what, newest) in [
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

        let bare = banner.trim_start();
        for (col, cut_refused, over_quote_refused) in [
            (2, false, false),
            (8, false, false),
            (9, true, true),
            (11, true, true),
            (12, true, true),
            (13, true, false),
            (14, false, false),
            (16, false, false),
        ] {
            for (refused, tail) in [
                (cut_refused, None),
                (
                    over_quote_refused,
                    Some("   \u{2590}\u{259B}\u{2588}   Opus 5.5"),
                ),
            ] {
                let mut under = card_rows();
                under.extend(rows(&["", "⏺ Done.", ""]));
                under.push(format!("{}{bare}", " ".repeat(col)));
                under.extend(tail.map(str::to_owned));
                assert_eq!(
                    launch_card_place(&under).map(|p| p.top),
                    (!refused).then_some(0),
                    "a bare banner at column {col} over {tail:?}: {under:#?}"
                );
            }
        }
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
        let header = "# claude-code 2.1.300 \u{00B7} MEASURED 2026-10-14";
        assert_eq!(header_version(header), Some("2.1.300"));
        for wrong in ["2.1.30", "2.1.3000", "2.1"] {
            assert_ne!(header_version(header), Some(wrong), "{wrong}");
        }

        let card = card_rows();
        let place = launch_card_place(&card).expect("the measured card");
        assert_eq!(launch_card_version(&card, &place), Some("2.1.294"));
        assert_eq!(launch_card_version(&card[..0], &place), None);
        let shifted = CardPlace { top: 1, ..place };
        assert_eq!(launch_card_version(&card, &shifted), None);
        let banner = crate::anchor!("launch.banner");
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
            edited(1, "           Sonnet 5.5 · Claude API"),
            edited(1, "▝▜██████▀   Sonnet 5.5 · Claude API"),
            edited(1, "▝▜██████▀  ▀Sonnet 5.5"),
            edited(2, " ▝▝   ▝▝  ▝/private/tmp/at.0000/proj"),
            edited(2, " ▝▝   ▝▝"),
        ] {
            assert_eq!(launch_card_place(&screen), None, "{screen:#?}");
        }
    }

    #[test]
    fn a_kept_card_reading_is_the_scans() {
        let anchor = crate::anchor!("launch.banner");
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
        let mut screen = card_rows();
        screen.extend(rows(&["", "", "", ""]));
        let reading = launch_card_scan(&screen);
        assert_eq!(reading.0, Some(0));
        let mut words = screen.clone();
        words[3] = "\u{276F} keep going".into();
        assert!(launch_card_unmoved(&words, reading.0, &[3]));
        words[2] = "\u{276F} keep going".into();
        assert!(!launch_card_unmoved(&words, reading.0, &[2, 3]));
        let narrow = measured("claude-2.1.304-update-notice-narrow-workspace-fullscreen-50col.txt");
        let at = launch_card_scan(&narrow);
        assert_eq!(at.0, Some(1), "CONTROL: the narrow card's banner");
        let mut words = narrow.clone();
        words[6] = "\u{276F} keep going".into();
        assert!(launch_card_unmoved(&words, at.0, &[6]));
        words[5] = "\u{276F} keep going".into();
        assert!(!launch_card_unmoved(&words, at.0, &[5, 6]), "its margin");
        assert_eq!(launch_card_scan(&words), (Some(1), None), "read again");
        let mut newer = screen.clone();
        newer[4..].clone_from_slice(&card_rows()[..3]);
        assert!(!launch_card_unmoved(&newer, reading.0, &[4, 5, 6]));
        assert_eq!(launch_card_scan(&newer).0, Some(4));
        let mut named = screen.clone();
        named[4] = "           Claude Code v2.1.299 is out".into();
        let reading = launch_card_scan(&named);
        assert_eq!(reading.0, Some(0));
        let mut confirmed = named.clone();
        confirmed[5] =
            " \u{2590}\u{259B}\u{2588}\u{2588}\u{2588}\u{259B}\u{2588}   Opus 5.5".into();
        assert_eq!(launch_card_scan(&confirmed), (Some(4), None));
        assert!(!launch_card_unmoved(&confirmed, reading.0, &[5]));
        let blank = rows(&["", "", ""]);
        assert!(launch_card_unmoved(&blank, None, &[0, 1, 2]));
        assert!(!launch_card_unmoved(&card_rows(), None, &[0, 1, 2]));
        let _ = launch_card_unmoved(&blank, Some(usize::MAX), &[usize::MAX, 0]);
        assert!(launch_card_unmoved(&blank, Some(1), &[0, usize::MAX]));
    }

    #[test]
    fn the_answered_prompt_is_the_row_over_the_last_words() {
        let measured = screen(include_str!(
            "fixtures/claude-2.1.303-fast-answers-same-idle-line-measured.txt"
        ));
        let idle = "Both branches have landed and no lander is running.";
        assert_eq!(said_tail(&measured).as_deref(), Some(idle));
        assert_eq!(answered_prompt(&measured).as_deref(), Some("keep going"));
        let caret = measured
            .iter()
            .position(|r| r == "❯")
            .expect("the caret row");
        let top = caret - 1;
        let keep = measured
            .iter()
            .rposition(|r| r == "❯ keep going")
            .expect("the last act's row");
        let checked: Vec<String> = measured[..keep]
            .iter()
            .chain(&measured[top..])
            .cloned()
            .collect();
        let check = answered_prompt(&checked).expect("the check's row");
        assert!(
            check.starts_with("Before you stop: do every open item"),
            "{check}"
        );
        assert!(
            check.contains("(build, test, measure where numbers"),
            "{check}"
        );
        assert!(
            check.ends_with("a slow check is not a blocker. Otherwise continue."),
            "{check}"
        );
        assert_eq!(said_tail(&checked).as_deref(), Some(idle));
        let mut drafted = checked.clone();
        let at = drafted
            .iter()
            .position(|r| r == "❯")
            .expect("the caret row");
        drafted[at] = "❯ keep going".to_string();
        assert_eq!(answered_prompt(&drafted).as_deref(), Some(check.as_str()));
        let pending: Vec<String> = measured[..keep + 2]
            .iter()
            .chain(&measured[top..])
            .cloned()
            .collect();
        assert_eq!(said_tail(&pending).as_deref(), Some(idle));
        assert_eq!(answered_prompt(&pending).as_deref(), Some(check.as_str()));
        let unanswered = framed(&["⏺ Done.", "", "❯ keep going", ""], "  ? for shortcuts");
        assert_eq!(answered_prompt(&unanswered), None);
        let tooled = framed(
            &[
                "❯ run the suite",
                "",
                "⏺ Bash(targo --unverified test -p aterm-phase)",
                "  ⎿  test result: ok. 412 passed",
                "",
                "⏺ All green.",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(answered_prompt(&tooled).as_deref(), Some("run the suite"));
        let woke = framed(
            &[
                "❯ keep going",
                "",
                &format!("⏺ {idle}"),
                "",
                "✻ Baked for 0s · done 11:06 PM",
                "",
                "⏺ The background suite finished: all green.",
                "",
            ],
            "  ? for shortcuts",
        );
        assert_eq!(answered_prompt(&woke), None);
        assert_eq!(answered_prompt(&measured[..4]), None);
        assert_eq!(
            answered_prompt(&rows(&["❯ keep going", "", "⏺ Done."])),
            None
        );
    }

    #[test]
    fn a_prompt_a_hook_stopped_closes_no_turn_and_its_label_is_no_words() {
        use crate::prompt::fixtures::{
            PROMPT_HOOK_BLOCKED_LOGIN_PHRASE_MEASURED, PROMPT_HOOK_STOPPED_LOGIN_PHRASE_MEASURED,
        };
        let stopped = screen(PROMPT_HOOK_STOPPED_LOGIN_PHRASE_MEASURED);
        let label = stopped
            .iter()
            .position(|r| r == "⏺ Operation stopped by hook: Not logged in · Please run /login")
            .expect("the measured label row");
        let with = |words: &[&str]| -> Vec<String> {
            let mut r = stopped[..label].to_vec();
            r.extend(words.iter().map(|w| (*w).to_string()));
            r.extend_from_slice(&stopped[label + 1..]);
            r
        };
        for (name, read) in [
            ("measured", stopped.clone()),
            ("no reason", with(&["⏺ Operation stopped by hook"])),
            (
                "a reason on two rows",
                with(&[
                    "⏺ Operation stopped by hook: Not logged in ·",
                    "  Please run /login",
                ]),
            ),
        ] {
            assert_eq!(answered_prompt(&read), None, "{name}");
            assert_eq!(said_tail(&read), None, "{name}");
        }
        let blocked = screen(PROMPT_HOOK_BLOCKED_LOGIN_PHRASE_MEASURED);
        assert_eq!(said_tail(&blocked), None);
        assert_eq!(answered_prompt(&blocked), None);
        let expansion: Vec<String> = blocked
            .iter()
            .map(|r| r.replace("⏺ UserPromptSubmit ", "⏺ UserPromptExpansion "))
            .collect();
        assert_ne!(expansion, blocked);
        assert_eq!(said_tail(&expansion), None);
        let answered = with(&["⏺ Nothing to say."]);
        assert_eq!(answered_prompt(&answered).as_deref(), Some("say nothing"));
        assert_eq!(said_tail(&answered).as_deref(), Some("Nothing to say."));
        for own in [
            "The hook printed Operation stopped by hook: Not logged in.",
            "Operation stopped by hooks in CI, so I removed them.",
            "I read UserPromptSubmit operation blocked by hook: in the log.",
            "The operation blocked by hook: none, the suite ran.",
        ] {
            let mine = with(&[&format!("⏺ {own}")]);
            assert_eq!(
                answered_prompt(&mine).as_deref(),
                Some("say nothing"),
                "{own}"
            );
            assert_eq!(said_tail(&mine).as_deref(), Some(own), "{own}");
        }
    }
}
