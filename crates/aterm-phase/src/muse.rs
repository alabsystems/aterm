// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Muse Code (`muse`) terminal screen grammar, prompt/card parsers, and phase classifier.

use crate::anchors::anchor_text;
use crate::phase::{Phase, is_rule, leading_spaces, plain_rule_width};
use crate::prompt::{Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select};
use crate::text::blank;
use crate::turn::{CardPlace, LaunchCard};
use crate::wall::{ApiCause, Placement, Wall, WallKind};

#[derive(Debug, Clone, Copy)]
struct Frame {
    top: usize,
    caret: usize,
    bottom: usize,
    footer: usize,
}

fn is_panel_head(row: &str) -> bool {
    let t = crate::text::trim_end(row);
    t == "main"
        || t.strip_prefix("main · ")
            .is_some_and(|rest| rest == const { anchor_text("muse.panel.select") })
}

fn is_panel_row(row: &str) -> bool {
    row.starts_with("└ ") || row.starts_with("├ ") || row.starts_with("│ ")
}

fn bottom_and_footer(rows: &[String]) -> Option<(usize, usize)> {
    let footer = rows.iter().rposition(|r| !blank(r))?;
    if leading_spaces(&rows[footer]) != 2 {
        return None;
    }
    let mut k = footer.checked_sub(1)?;
    if plain_rule_width(&rows[k]).is_some() {
        return Some((k, footer));
    }
    let last_branch = k;
    while is_panel_row(&rows[k]) {
        k = k.checked_sub(1)?;
    }
    if k == last_branch || !is_panel_head(&rows[k]) {
        return None;
    }
    let bottom = k.checked_sub(1)?;
    plain_rule_width(&rows[bottom]).map(|_| (bottom, footer))
}

fn is_top_rule(row: &str) -> bool {
    is_rule(row)
        || (row.starts_with("── ")
            && labelled(row)
            && !row.contains(['┬', '┴', '┼', '├', '┤', '┌', '┐', '└', '┘', '│']))
}

fn any_frame(rows: &[String]) -> Option<Frame> {
    let caret = rows.iter().rposition(|r| r.starts_with('❯'))?;
    let top = caret.checked_sub(1)?;
    if !is_top_rule(&rows[top]) {
        return None;
    }
    let bottom = (caret + 1..rows.len()).find(|&i| plain_rule_width(&rows[i]).is_some())?;
    if rows[caret + 1..bottom].iter().any(|r| is_top_rule(r)) {
        return None;
    }
    let (under, footer) = bottom_and_footer(rows)?;
    (under == bottom).then_some(Frame {
        top,
        caret,
        bottom,
        footer,
    })
}

fn frame(rows: &[String]) -> Option<Frame> {
    any_frame(rows).filter(|f| {
        !crate::text::contains(&rows[f.top], const { anchor_text("muse.sidechat.label") })
            && !crate::text::trim(&rows[f.footer])
                .ends_with(const { anchor_text("muse.sidechat.footer") })
    })
}

fn open_frame(rows: &[String]) -> Option<Frame> {
    frame(rows).filter(|&f| {
        question_box(rows, f).is_none() && review_box(rows, f).is_none() && !drawer(rows, f)
    })
}

fn is_key_hint_row(row: &str) -> bool {
    if leading_spaces(row) == 0 {
        return false;
    }
    let t = crate::text::trim(row);
    let hints: Vec<&str> = t
        .split(if t.contains(" · ") { " · " } else { " - " })
        .collect();
    hints.len() >= 2
        && hints.iter().any(|h| {
            crate::text::trim(h)
                .strip_prefix("esc ")
                .is_some_and(|verb| !verb.is_empty() && !verb.contains(' '))
        })
}

fn drawer(rows: &[String], f: Frame) -> bool {
    (0..f.top)
        .rev()
        .find(|&i| !blank(&rows[i]))
        .is_some_and(|i| is_key_hint_row(&rows[i]))
        || rows[f.caret + 1..f.bottom]
            .iter()
            .any(|r| is_key_hint_row(r))
}

/// Returns the composer's caret row, or `None` while a modal box is open.
#[must_use]
pub fn composer(rows: &[String]) -> Option<usize> {
    open_frame(rows).map(|f| f.caret)
}

fn is_menu_row(row: &str) -> bool {
    let t = crate::text::trim_start(row);
    leading_spaces(row) == 2 && (t.starts_with('/') || t.starts_with("↓ ") || t.starts_with("↑ "))
}

/// Extracts the composer's draft as `(caret_row, lines)` with the leading `❯` stripped.
#[must_use]
pub fn composer_draft(rows: &[String]) -> Option<(usize, Vec<String>)> {
    let f = open_frame(rows)?;
    let caret = rows[f.caret].strip_prefix('❯').unwrap_or(&rows[f.caret]);
    let caret = crate::text::trim(caret);
    let menu = caret.starts_with('/');
    let mut lines = vec![caret.to_string()];
    lines.extend(
        rows[f.caret + 1..f.bottom]
            .iter()
            .filter(|r| !(menu && is_menu_row(r)))
            .map(|r| crate::text::trim(r).to_string()),
    );
    Some((f.caret, lines))
}

const STATUS_GLYPHS: [char; 3] = ['◇', '◈', '◆'];

fn is_status_row(row: &str) -> bool {
    let hint = const { anchor_text("muse.busy.interrupt") };
    row.starts_with(STATUS_GLYPHS)
        && crate::text::trim_end(row)
            .strip_suffix(')')
            .is_some_and(|r| r.ends_with(hint))
}

fn is_status_first_frame(row: &str) -> bool {
    row.strip_prefix(STATUS_GLYPHS[0])
        .and_then(|r| r.strip_prefix(' '))
        .is_some_and(|r| crate::text::trim_end(r) == const { anchor_text("muse.busy.first") })
}

fn is_reply_head(row: &str) -> bool {
    row.starts_with("◆ ") && !is_status_row(row) && !is_question_head(row)
}

fn is_banner_row(row: &str) -> bool {
    crate::text::trim(row)
        .strip_prefix(const { anchor_text("muse.banner") })
        .and_then(|v| v.strip_prefix(' '))
        .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

fn is_session_banner(rows: &[String], i: usize) -> bool {
    is_banner_row(&rows[i])
        && (0..i).rev().find(|&k| !blank(&rows[k])).is_none_or(|k| {
            let r = &rows[k];
            leading_spaces(r) == 0
                && !r.starts_with(STATUS_GLYPHS)
                && r.strip_prefix('❯').is_none_or(|cmd| {
                    cmd.split_whitespace()
                        .next()
                        .and_then(crate::reader::program_of)
                        == Some(crate::reader::Program::Muse)
                })
        })
}

fn scope(rows: &[String], top: usize) -> (Option<usize>, Option<usize>, usize) {
    let banner = (0..top).rev().find(|&i| is_session_banner(rows, i));
    let start = banner.map_or(0, |b| b + 1);
    let user = (start..top).rev().find(|&i| rows[i].starts_with('❯'));
    (banner, user, user.map_or(start, |u| u + 1))
}

fn is_card_row(row: &str) -> bool {
    row.starts_with('┌') || row.starts_with('│') || row.starts_with("└─")
}

fn card_state(rows: &[String]) -> Option<&str> {
    status_head(rows).and_then(|(_, state)| state)
}

fn card_text(row: &str) -> Option<&str> {
    let inner = crate::text::trim_end(row)
        .strip_prefix('│')?
        .strip_suffix('│')?;
    Some(crate::text::trim(inner))
}

fn status_head(rows: &[String]) -> Option<(usize, Option<&str>)> {
    let (i, row) = rows.iter().enumerate().rev().find(|(_, row)| {
        row.strip_prefix('│')
            .map(crate::text::trim_start)
            .is_some_and(|text| text.starts_with(const { anchor_text("muse.card.status") }))
    })?;
    let state = card_text(row)
        .and_then(|text| text.strip_prefix(const { anchor_text("muse.card.status") }))
        .and_then(|text| text.rsplit_once(' ').map(|(_, state)| state))
        .filter(|state| !state.is_empty() && state.bytes().all(|b| b.is_ascii_uppercase()));
    Some((i, state))
}

fn status_spent_none(rows: &[String]) -> bool {
    status_head(rows).is_some_and(|(head, _)| {
        rows[head + 1..]
            .iter()
            .take_while(|r| r.starts_with('│'))
            .filter_map(|r| card_text(r))
            .find_map(|t| t.strip_prefix(const { anchor_text("muse.card.usage") }))
            .and_then(|spent| crate::text::trim(spent).split_once(' '))
            .is_some_and(|(count, unit)| {
                count == "0" && unit.starts_with(const { anchor_text("muse.card.tokens") })
            })
    })
}

fn usage_turns(rows: &[String]) -> Option<u64> {
    rows.iter().rev().find_map(|r| {
        let count = card_text(r)?.strip_prefix(const { anchor_text("muse.card.turns") })?;
        let count = count.strip_prefix(' ').map(crate::text::trim)?;
        (!count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
            .then(|| count.parse().ok())
            .flatten()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rest {
    Completed,
    Failed,
    Cancelled,
    Unnamed,
}

fn carded(rows: &[String]) -> Turn {
    match card_state(rows) {
        Some(s) if s == const { anchor_text("muse.card.idle") } && status_spent_none(rows) => {
            Turn::Fresh
        }
        Some(s) if s == const { anchor_text("muse.card.completed") } => {
            Turn::Carded(Rest::Completed)
        }
        Some(s) if s == const { anchor_text("muse.card.failed") } => Turn::Carded(Rest::Failed),
        Some(s) if s == const { anchor_text("muse.card.cancelled") } => {
            Turn::Carded(Rest::Cancelled)
        }
        Some(_) => Turn::Unknown,
        None if status_head(rows).is_none() && usage_turns(rows).is_some_and(|n| n > 0) => {
            Turn::Carded(Rest::Unnamed)
        }
        None => Turn::Unknown,
    }
}

fn is_view_hint(rows: &[String], i: usize) -> bool {
    leading_spaces(&rows[i]) == 2
        && i.checked_sub(1).is_some_and(|above| blank(&rows[above]))
        && crate::text::trim(&rows[i])
            .strip_prefix(const { anchor_text("muse.panel.view") })
            .and_then(|kind| kind.strip_suffix(')'))
            .is_some_and(|kind| !kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase()))
}

fn in_abnormal_end(rows: &[String], i: usize) -> bool {
    let start = (0..=i)
        .rev()
        .take_while(|&j| !blank(&rows[j]) && leading_spaces(&rows[j]) == 2)
        .last();
    start.is_some_and(|s| {
        s.checked_sub(1).is_some_and(|above| blank(&rows[above]))
            && crate::text::trim(&rows[s])
                .strip_prefix("warning: ")
                .is_some_and(|w| w.starts_with(const { anchor_text("muse.resume.abnormal") }))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Running,
    Sent,
    Ended(usize, usize),
    Tail(usize, usize),
    Tool(usize, usize),
    Carded(Rest),
    Fresh,
    Unknown,
}

fn turn(rows: &[String], top: usize) -> Turn {
    let (banner, user, from) = scope(rows, top);
    if rows[from..top].iter().any(|r| is_status_row(r)) {
        return Turn::Running;
    }
    if (from..top)
        .rev()
        .find(|&i| !blank(&rows[i]))
        .is_some_and(|i| is_status_first_frame(&rows[i]))
    {
        return Turn::Running;
    }
    if user.is_none() && banner.is_some() {
        return Turn::Fresh;
    }
    let drawn = |i: usize| {
        !blank(&rows[i])
            && !is_card_row(&rows[i])
            && !is_view_hint(rows, i)
            && !in_abnormal_end(rows, i)
    };
    let Some(last) = (from..top).rev().find(|&i| drawn(i)) else {
        let cards = rows[from..top].iter().any(|r| is_card_row(r));
        return match (user, cards) {
            (Some(_), false) => Turn::Sent,
            (None, true) => carded(&rows[from..top]),
            _ => Turn::Unknown,
        };
    };
    let mut last = last;
    loop {
        let head = (from..=last)
            .rev()
            .find(|&i| leading_spaces(&rows[i]) == 0 && !blank(&rows[i]) && !branch_at(rows, i));
        if head.is_none()
            && let Some(first) = user.and_then(|u| read_rows(rows, u, last, drawn))
        {
            return Turn::Tool(first, last);
        }
        match head {
            Some(head) if is_own_row_head(&rows[head]) => {
                let Some(above) = (from..head).rev().find(|&i| drawn(i)) else {
                    return Turn::Unknown;
                };
                last = above;
            }
            Some(head) if is_reply_head(&rows[head]) => return Turn::Ended(head, last),
            None if user.is_none()
                && rows[from..=last]
                    .iter()
                    .all(|r| blank(r) || (leading_spaces(r) >= 2 && !is_banner_row(r))) =>
            {
                return Turn::Tail(from, last);
            }
            _ => return Turn::Unknown,
        }
    }
}

fn read_rows(
    rows: &[String],
    user: usize,
    last: usize,
    drawn: impl Fn(usize) -> bool,
) -> Option<usize> {
    let gap = (user + 1..=last).find(|&i| blank(&rows[i]))?;
    let mut read = (gap..=last).filter(|&i| drawn(i)).peekable();
    let first = *read.peek()?;
    read.all(|i| read_row(&rows[i]) || search_row(&rows[i]))
        .then_some(first)
}

fn read_row(row: &str) -> bool {
    leading_spaces(row) == 2
        && crate::text::trim(row)
            .strip_prefix(const { anchor_text("muse.tool.read") })
            .is_some_and(names_a_path)
}

fn search_row(row: &str) -> bool {
    let Some(rest) = crate::text::trim(row)
        .strip_prefix(const { anchor_text("muse.tool.searched") })
        .filter(|_| leading_spaces(row) == 2)
    else {
        return false;
    };
    let Some((pattern, count)) = rest.rsplit_once(" (") else {
        return false;
    };
    let quoted = pattern.len() >= 2 && pattern.starts_with('"') && pattern.ends_with('"');
    let counted = count == "no matches)"
        || count
            .strip_suffix(" match)")
            .or_else(|| count.strip_suffix(" matches)"))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    quoted && counted
}

fn is_outcome_branch(row: &str) -> bool {
    row.starts_with("└ ")
}

fn branch_at(rows: &[String], i: usize) -> bool {
    is_outcome_branch(&rows[i]) && i > 0 && rows[i - 1].starts_with("◆ ")
}

fn branched(rows: &[String], head: usize) -> bool {
    rows[head].starts_with("◆ ") && head + 1 < rows.len() && branch_at(rows, head + 1)
}

fn is_own_row_head(row: &str) -> bool {
    row.strip_prefix("◆ ").is_some_and(|r| {
        r.starts_with(const { anchor_text("muse.recap") })
            || r.starts_with(const { anchor_text("muse.notice.message") })
            || r.strip_prefix(const { anchor_text("muse.worked") })
                .and_then(|t| t.split_once(" · "))
                .is_some_and(|(took, _)| is_elapsed(took))
            || r.strip_prefix(const { anchor_text("muse.notice.finished") })
                .and_then(|t| t.rsplit_once(" · "))
                .is_some_and(|(_, took)| is_elapsed(took))
    })
}

fn is_elapsed(t: &str) -> bool {
    let t = crate::text::trim(t);
    !t.is_empty()
        && t.split(' ').all(|w| {
            let d = w.bytes().take_while(u8::is_ascii_digit).count();
            d > 0 && matches!(&w[d..], "ms" | "s" | "m" | "h")
        })
}

fn ended(rows: &[String]) -> Option<(usize, Vec<&str>)> {
    let (head, last) = ended_rows(rows)?;
    Some((head, block_lines(rows, head, last)))
}

fn ended_rows(rows: &[String]) -> Option<(usize, usize)> {
    let f = open_frame(rows)?;
    let (Turn::Ended(head, last) | Turn::Tail(head, last)) = turn(rows, f.top) else {
        return None;
    };
    Some((head, last))
}

fn block_lines(rows: &[String], head: usize, last: usize) -> Vec<&str> {
    let first = rows[head].strip_prefix('◆').unwrap_or(&rows[head]);
    std::iter::once(crate::text::trim(first))
        .chain(rows[head + 1..=last].iter().map(|r| crate::text::trim(r)))
        .filter(|t| !t.is_empty())
        .collect()
}

fn notice(rows: &[String]) -> Option<(usize, String)> {
    let (head, last) = ended_rows(rows)?;
    let start = (head..=last).find(|&i| !blank(&rows[i]))?;
    let end = (start + 1..=last)
        .find(|&i| blank(&rows[i]))
        .map_or(last, |b| b - 1);
    Some((head, block_lines(rows, start, end).join(" ")))
}

fn interrupting(rows: &[String], f: Frame) -> bool {
    rows.get(f.footer).is_some_and(|r| {
        crate::text::trim_end(r)
            .strip_suffix(const { anchor_text("muse.footer.interrupting") })
            .is_some_and(|left| left.ends_with("  "))
    })
}

const WORK_RUNNING: &str = "background work running";
const MONITOR_WATCHING: &str = "a monitor watching";

fn panel_work(row: &str) -> Option<&'static str> {
    let item = row
        .strip_prefix("└ ")
        .or_else(|| row.strip_prefix("├ "))?
        .strip_prefix(STATUS_GLYPHS)?
        .strip_prefix(' ')?;
    let cells: Vec<&str> = item
        .split("  ")
        .map(crate::text::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let [_, .., state, took] = cells.as_slice() else {
        return None;
    };
    if !is_elapsed(took) {
        return None;
    }
    if *state == const { anchor_text("muse.panel.running") } {
        Some(WORK_RUNNING)
    } else if *state == const { anchor_text("muse.panel.watching") } {
        Some(MONITOR_WATCHING)
    } else {
        None
    }
}

fn work(rows: &[String]) -> Option<&'static str> {
    let f = open_frame(rows)?;
    let branches = &rows[f.bottom + 1..f.footer];
    let named = |what: &'static str| {
        branches
            .iter()
            .any(|r| panel_work(r) == Some(what))
            .then_some(what)
    };
    named(WORK_RUNNING).or_else(|| named(MONITOR_WATCHING))
}

/// Returns the background-work kind (`"work"` or `"bg"`) when a turn is over and background tasks run.
#[must_use]
pub fn background_wait(rows: &[String]) -> Option<&'static str> {
    let what = work(rows)?;
    matches!(
        turn_phase(rows),
        (Phase::Idle | Phase::Question | Phase::Limited { .. }, true)
    )
    .then_some(what)
}

/// Classifies `rows` into `(Phase, is_evidence)` for Muse Code.
#[must_use]
pub fn phase(rows: &[String]) -> (Phase, bool) {
    let read = turn_phase(rows);
    let Some(what) = work(rows) else {
        return read;
    };
    let held = match read {
        (Phase::Idle, true) => true,
        (Phase::Question, true) => what == WORK_RUNNING,
        _ => false,
    };
    if held && wall(rows).is_none() {
        return (Phase::Busy, true);
    }
    read
}

fn turn_phase(rows: &[String]) -> (Phase, bool) {
    if trust_box(rows).is_some() || approval_box(rows).is_some() {
        return (Phase::Prompt, true);
    }
    let Some(f) = frame(rows) else {
        return (Phase::Idle, signed_out(rows).is_some());
    };
    if question_box(rows, f).is_some() || review_box(rows, f).is_some() || drawer(rows, f) {
        return (Phase::Prompt, true);
    }
    match turn(rows, f.top) {
        Turn::Running => (Phase::Busy, true),
        Turn::Sent => (Phase::Busy, false),
        Turn::Fresh => (Phase::Idle, true),
        Turn::Unknown => (Phase::Idle, false),
        Turn::Ended(..) | Turn::Tail(..) | Turn::Tool(..) | Turn::Carded(_)
            if interrupting(rows, f) =>
        {
            (Phase::Busy, false)
        }
        Turn::Carded(_) => (Phase::Idle, true),
        Turn::Ended(..) | Turn::Tail(..) if faulted(rows) => (Phase::Idle, false),
        Turn::Ended(..) | Turn::Tail(..) | Turn::Tool(..) => {
            if let Some(w) = wall(rows).filter(|w| w.kind.reads_limited()) {
                return (
                    Phase::Limited {
                        message: w.message,
                        reset: w.reset,
                    },
                    true,
                );
            }
            let asks = said_tail(rows).is_some_and(|t| crate::turn::asks(&t));
            (if asks { Phase::Question } else { Phase::Idle }, true)
        }
    }
}

/// Returns whether `rows` show a fresh session with no user turn yet.
#[must_use]
pub fn fresh(rows: &[String]) -> bool {
    open_frame(rows).is_some_and(|f| turn(rows, f.top) == Turn::Fresh)
}

/// Returns whether a user message or turn reply is visible on `rows`.
#[must_use]
pub fn message_drawn(rows: &[String]) -> bool {
    let top = frame(rows).map_or(rows.len(), |f| f.top);
    scope(rows, top).1.is_some()
}

fn rests_on(rows: &[String]) -> Option<Rest> {
    match open_frame(rows).map(|f| turn(rows, f.top)) {
        Some(Turn::Carded(rest)) => Some(rest),
        _ => None,
    }
}

/// Returns whether the session rests at a completed turn hidden beneath `/status` or `/usage` cards.
#[must_use]
pub fn end_hidden(rows: &[String]) -> bool {
    phase(rows) == (Phase::Idle, true)
        && matches!(rests_on(rows), Some(Rest::Failed | Rest::Unnamed))
}

const TOOL_HEADS: [&str; 3] = [
    anchor_text("muse.tool.ran"),
    anchor_text("muse.tool.wrote"),
    anchor_text("muse.tool.edited"),
];

const NAMED_TOOL_HEADS: [&str; 4] = [
    anchor_text("muse.tool.read_memory"),
    anchor_text("muse.tool.saved_memory"),
    anchor_text("muse.tool.read_goal"),
    anchor_text("muse.tool.used"),
];

fn tool_row(head: &str) -> bool {
    let [ran, wrote, edited] = TOOL_HEADS;
    head.starts_with(ran)
        || [wrote, edited]
            .iter()
            .any(|h| head.strip_prefix(h).is_some_and(names_a_path))
        || NAMED_TOOL_HEADS.iter().any(|h| {
            head.strip_prefix(h)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(names_a_path)
        })
}

const TOOL_OUTCOMES: [&str; 4] = ["failed", "denied", "rejected", "no effect"];

fn names_a_path(rest: &str) -> bool {
    let token = rest.split(' ').next().unwrap_or_default();
    if token.is_empty() {
        return false;
    }
    let mut tail = &rest[token.len()..];
    if let Some(r) = tail.strip_prefix(" (") {
        let Some((count, after)) = r.split_once(')') else {
            return false;
        };
        let digits = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
        let counted = match count.split_once(' ') {
            Some((added, removed)) => {
                added.strip_prefix('+').is_some_and(digits)
                    && removed.strip_prefix('−').is_some_and(digits)
            }
            None => count.strip_prefix('+').is_some_and(digits),
        };
        if !counted {
            return false;
        }
        tail = after;
    }
    if let Some(r) = tail.strip_prefix(" — ") {
        let Some(after) = TOOL_OUTCOMES.iter().find_map(|o| r.strip_prefix(o)) else {
            return false;
        };
        tail = after;
    }
    tail.strip_prefix(" · ctrl+o").unwrap_or(tail).is_empty()
}

/// Extracts the agent's final `◆` reply from the last completed turn.
#[must_use]
pub fn said_tail(rows: &[String]) -> Option<String> {
    let (at, lines) = ended(rows)?;
    let head = *lines.first()?;
    let (_, message) = notice(rows)?;
    if head == const { anchor_text("muse.turn.interrupted") }
        || tool_row(head)
        || branched(rows, at)
        || wall_of(&message).is_some()
        || is_fault(&message)
    {
        return None;
    }
    Some(lines.join("\n"))
}

/// Returns whether the reply returned by [`said_tail`] has its opening `◆` header visible on `rows`.
#[must_use]
pub fn said_whole(rows: &[String]) -> bool {
    said_tail(rows).is_some()
        && open_frame(rows).is_some_and(|f| matches!(turn(rows, f.top), Turn::Ended(..)))
}

/// Extracts earlier `◆` replies belonging to the same turn as [`said_tail`].
#[must_use]
pub fn turn_said_before(rows: &[String]) -> Option<String> {
    let (_, said) = turn_before(rows)?;
    (!said.is_empty()).then(|| said.join("\n\n"))
}

/// Returns whether the entire last turn (from the user's `❯` message through [`said_tail`]) fits on `rows`.
#[must_use]
pub fn turn_whole(rows: &[String]) -> bool {
    turn_before(rows).is_some_and(|(opened, _)| opened)
}

/// Returns whether `row` is the opening `❯` user message that stops a scrollback history scan.
#[must_use]
pub fn turn_history_stop(row: &str) -> bool {
    row.starts_with('❯') || is_banner_row(row)
}

/// Extracts the user's opening `❯` message for the turn ended by [`said_tail`].
#[must_use]
pub fn turn_opened_by(rows: &[String]) -> Option<String> {
    said_tail(rows)?;
    let f = open_frame(rows)?;
    let (_, user, _) = scope(rows, f.top);
    let user = user?;
    let end = (user + 1..f.top)
        .find(|&i| blank(&rows[i]) || leading_spaces(&rows[i]) == 0)
        .unwrap_or(f.top);
    let first = rows[user].strip_prefix('❯').unwrap_or(&rows[user]);
    let text: Vec<&str> = std::iter::once(first)
        .chain(rows[user + 1..end].iter().map(String::as_str))
        .map(crate::text::trim)
        .filter(|t| !t.is_empty())
        .collect();
    Some(text.join(" "))
}

fn turn_before(rows: &[String]) -> Option<(bool, Vec<String>)> {
    if !said_whole(rows) {
        return None;
    }
    let f = open_frame(rows)?;
    let Turn::Ended(head, _) = turn(rows, f.top) else {
        return None;
    };
    let (_, user, from) = scope(rows, f.top);
    let mut said = Vec::new();
    for h in (from..head)
        .filter(|&h| is_reply_head(&rows[h]) && !is_own_row_head(&rows[h]) && !branched(rows, h))
    {
        let end = (h + 1..head)
            .find(|&k| leading_spaces(&rows[k]) == 0 && !blank(&rows[k]))
            .unwrap_or(head);
        let first = rows[h].strip_prefix('◆').unwrap_or(&rows[h]);
        let lines: Vec<&str> = std::iter::once(crate::text::trim(first))
            .chain(
                (h + 1..end)
                    .filter(|&k| !is_view_hint(rows, k) && !in_abnormal_end(rows, k))
                    .map(|k| crate::text::trim(&rows[k])),
            )
            .filter(|t| !t.is_empty())
            .collect();
        let Some(&top) = lines.first() else {
            continue;
        };
        let message = lines.join(" ");
        if top == const { anchor_text("muse.turn.interrupted") }
            || tool_row(top)
            || wall_of(&message).is_some()
            || is_fault(&message)
        {
            continue;
        }
        said.push(lines.join("\n"));
    }
    Some((user.is_some(), said))
}

fn is_fault(message: &str) -> bool {
    [
        anchor_text("muse.fault.config"),
        anchor_text("muse.fault.prepare"),
        anchor_text("muse.fault.state"),
    ]
    .iter()
    .any(|f| message.starts_with(f))
}

fn faulted(rows: &[String]) -> bool {
    ended(rows).is_some_and(|(_, lines)| is_fault(&lines.join(" ")))
}

/// Returns whether the last turn was interrupted by the user (`◆ Interrupted`).
#[must_use]
pub fn interrupted(rows: &[String]) -> bool {
    ended(rows).is_some_and(|(_, lines)| {
        lines.first().copied() == Some(const { anchor_text("muse.turn.interrupted") })
    }) || rests_on(rows) == Some(Rest::Cancelled)
}

fn signed_out(rows: &[String]) -> Option<usize> {
    let end = rows.iter().rposition(|r| !blank(r))?;
    if crate::text::trim(&rows[end]) != const { anchor_text("muse.signin.keys") } {
        return None;
    }
    rows[..end].iter().rposition(|r| {
        crate::text::trim(r).starts_with(const { anchor_text("muse.signin.browser") })
    })
}

fn wall_of(message: &str) -> Option<(WallKind, Option<String>)> {
    let auth = [
        anchor_text("muse.wall.key"),
        anchor_text("muse.wall.saved_key"),
        anchor_text("muse.wall.no_login"),
        anchor_text("muse.wall.payment_api"),
        anchor_text("muse.wall.payment_muse"),
        anchor_text("muse.wall.onboarding"),
    ];
    let lost = [
        anchor_text("muse.wall.invalid"),
        anchor_text("muse.wall.missing"),
        anchor_text("muse.wall.denied"),
        anchor_text("muse.wall.canceled"),
        anchor_text("muse.wall.unreachable"),
    ];
    if auth.contains(&message) || lost.iter().any(|l| message.starts_with(l)) {
        return Some((WallKind::Auth, None));
    }
    if let Some(rest) = message.strip_prefix(const { anchor_text("muse.wall.usage") }) {
        if matches!(rest, "" | ".") {
            return Some((WallKind::UsageSession, None));
        }
        let rest = rest.strip_prefix(const { anchor_text("muse.wall.usage.upgrade") })?;
        let (_, tail) = rest.split_once(const { anchor_text("muse.wall.usage.more") })?;
        let reset = tail
            .split_once(const { anchor_text("muse.wall.usage.reset") })
            .map(|(_, at)| crate::text::trim(at).trim_end_matches('.').to_string())
            .filter(|at| !at.is_empty());
        return Some((WallKind::UsageSession, reset));
    }
    if let Some((_, tail)) = message
        .strip_prefix(const { anchor_text("muse.wall.quota") })
        .and_then(|r| r.split_once(" quota"))
    {
        let reset = tail
            .strip_prefix(" (resets at ")
            .and_then(|r| r.split_once(')'))
            .map(|(at, _)| crate::text::trim(at).to_string())
            .filter(|at| !at.is_empty());
        return Some((WallKind::UsageSession, reset));
    }
    let rest = message.strip_prefix(const { anchor_text("muse.wall.model_failed") })?;
    let code = rest
        .strip_prefix(const { anchor_text("muse.wall.api_error") })
        .and_then(|r| r.get(..4))
        .and_then(|c| c.strip_suffix(':'))
        .and_then(|c| c.parse::<u16>().ok());
    Some((
        WallKind::ApiError {
            code,
            retryable: code.is_none_or(|c| matches!(c, 408 | 409 | 429) || c >= 500),
            cause: ApiCause::Server,
        },
        None,
    ))
}

/// Extracts Muse Code's own login-remedy clause from an authentication wall message.
#[must_use]
pub fn remedy(message: &str) -> Option<String> {
    let m = crate::text::trim(message);
    if m == const { anchor_text("muse.wall.key") } {
        let (_, r) = m.split_once(" — ")?;
        return Some(
            r.strip_suffix(" it")
                .map_or_else(|| r.to_string(), |verb| format!("{verb} META_API_KEY")),
        );
    }
    if m == const { anchor_text("muse.wall.saved_key") } {
        return m.split_once(" — ").map(|(_, r)| r.to_string());
    }
    if m == const { anchor_text("muse.wall.no_login") } {
        return m.split_once("; ").map(|(_, r)| r.to_string());
    }
    if [
        anchor_text("muse.wall.payment_api"),
        anchor_text("muse.wall.payment_muse"),
        anchor_text("muse.wall.onboarding"),
    ]
    .contains(&m)
    {
        return m
            .split_once(" — ")
            .map(|(_, r)| r.trim_end_matches('.').to_string());
    }
    None
}

/// Extracts a standing authentication or quota/API error [`Wall`] from `rows`.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    if let Some(row) = signed_out(rows) {
        return Some(Wall::sign_in(rows, row));
    }
    let (head, message) = notice(rows)?;
    let (kind, reset) = wall_of(&message)?;
    Some(Wall {
        kind,
        message,
        reset,
        row: head,
        placement: Placement::ErrorRow,
        under_call: false,
    })
}

/// In-progress API retry status line shown beneath a running turn's status row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub code: Option<u16>,
    pub wait: String,
    pub attempt: u32,
    pub of: u32,
}

/// Extracts the active [`Retry`] notice beneath a running turn's status row, if present.
#[must_use]
pub fn retrying(rows: &[String]) -> Option<Retry> {
    let f = open_frame(rows)?;
    if turn(rows, f.top) != Turn::Running {
        return None;
    }
    let status = (0..f.top).rev().find(|&i| is_status_row(&rows[i]))?;
    let t = crate::text::trim(rows.get(status + 1)?.strip_prefix("└ ")?);
    let rest = t.strip_prefix(const { anchor_text("muse.retry.api") })?;
    let (reason, tail) = rest.split_once(const { anchor_text("muse.retry.in") })?;
    let (wait, attempt) = tail.split_once(const { anchor_text("muse.retry.attempt") })?;
    let (k, n) = attempt.split_once('/')?;
    let code = reason
        .rsplit_once("(HTTP ")
        .and_then(|(_, c)| c.strip_suffix(')'))
        .and_then(|c| c.parse().ok());
    Some(Retry {
        code,
        wait: wait.to_string(),
        attempt: k.parse().ok()?,
        of: crate::text::trim(n).parse().ok()?,
    })
}

fn option(row: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(row, "⟩ ", "  ")
}

fn trust_box(rows: &[String]) -> Option<(usize, std::ops::Range<usize>, usize)> {
    let keys = rows.iter().rposition(|r| !blank(r))?;
    if !crate::text::trim(&rows[keys]).ends_with(const { anchor_text("muse.trust.keys") }) {
        return None;
    }
    let title = rows[..keys]
        .iter()
        .position(|r| crate::text::trim(r) == const { anchor_text("muse.trust.title") })?;
    let first = (title + 1..keys).find(|&i| option(&rows[i]).is_some())?;
    let end = (first..keys)
        .find(|&i| option(&rows[i]).is_none())
        .unwrap_or(keys);
    Some((title, first..end, keys))
}

fn trust_prompt(
    rows: &[String],
    (title, opts, keys): (usize, std::ops::Range<usize>, usize),
) -> PromptV2 {
    let mut options: Vec<Opt> = opts
        .clone()
        .filter_map(|i| {
            option(&rows[i]).map(|(n, label, focused)| Opt {
                n: Some(n),
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            })
        })
        .collect();
    if sound(&options) {
        for o in &mut options {
            o.role = if o.label == const { anchor_text("muse.trust.yes") } {
                Role::Trust
            } else if o.label == const { anchor_text("muse.trust.quit") } {
                Role::Exit
            } else {
                Role::Other
            };
        }
    }
    let path = rows[title + 1..opts.start].iter().find_map(|r| {
        crate::text::trim(r)
            .strip_prefix(const { anchor_text("muse.trust.path") })
            .map(|p| crate::text::trim(p).to_string())
    });
    let footer = crate::text::trim(&rows[keys]).to_string();
    let verb = footer
        .to_lowercase()
        .split_once("esc ")
        .map(|(_, v)| v.trim().trim_end_matches('.').to_string())
        .unwrap_or_default();
    PromptV2 {
        kind: PromptKind::Trust,
        title: crate::text::trim(&rows[title]).to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: path.clone().unwrap_or_default(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: None,
        path,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb,
            effect: CancelEffect::Exit,
        }),
        question: Some(title),
        span: (title, keys),
        footer: Some(footer),
        head_off_screen: false,
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

fn sound(options: &[Opt]) -> bool {
    options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.is_some_and(|n| usize::from(n) == k + 1))
        && options.iter().filter(|o| o.focused).count() == 1
}

fn box_option(row: &str) -> Option<(u8, &str, bool)> {
    crate::prompt::numbered_option(row, "⟩ ", ". ")
}

fn question_option(row: &str) -> Option<(u8, &str, bool)> {
    box_option(row.strip_prefix("  ")?)
}

fn label_and_key(text: &str) -> (&str, Option<&str>) {
    match text.strip_suffix(')').and_then(|t| t.rsplit_once(" (")) {
        Some((label, key))
            if (1..=3).contains(&key.len()) && key.chars().all(|c| c.is_ascii_lowercase()) =>
        {
            (label, Some(key))
        }
        _ => (text, None),
    }
}

#[derive(Debug, Clone, Copy)]
struct Approval {
    title: Option<usize>,
    first: usize,
    last: usize,
    end: usize,
}

const TOP_LABELS: [&str; 8] = [
    anchor_text("muse.composer.voice"),
    anchor_text("muse.composer.voice_prefix"),
    anchor_text("muse.composer.talk"),
    anchor_text("muse.composer.recording"),
    anchor_text("muse.composer.transcribing"),
    anchor_text("muse.composer.offline"),
    anchor_text("muse.composer.error"),
    anchor_text("muse.sidechat.label"),
];

fn labelled(rule: &str) -> bool {
    TOP_LABELS.iter().any(|l| crate::text::contains(rule, l))
}

fn approval_box(rows: &[String]) -> Option<Approval> {
    let (bottom, _) = bottom_and_footer(rows)?;
    let end = (0..bottom).rev().find(|&i| !blank(&rows[i]))?;
    let hint = crate::text::trim(&rows[end]) == const { anchor_text("muse.box.permissions") };
    let last = if hint {
        (0..end).rev().find(|&i| !blank(&rows[i]))?
    } else {
        end
    };
    box_option(&rows[last])?;
    let mut first = last;
    while first > 0
        && (box_option(&rows[first - 1]).is_some()
            || is_wrap(&rows[first - 1])
            || is_edge_tail(&rows[first - 1]))
    {
        first -= 1;
    }
    while box_option(&rows[first]).is_none() {
        first += 1;
    }
    match (0..first).rev().find(|&i| is_top_rule(&rows[i])) {
        Some(top) => {
            if !labelled(&rows[top]) || rows[top + 1..first].iter().any(|r| r.starts_with('❯')) {
                return None;
            }
            let title = top + 1;
            if title >= first || blank(&rows[title]) {
                return None;
            }
            Some(Approval {
                title: Some(title),
                first,
                last,
                end,
            })
        }
        None if hint => Some(Approval {
            title: None,
            first,
            last,
            end,
        }),
        None => None,
    }
}

fn is_wrap(row: &str) -> bool {
    !blank(row) && leading_spaces(row) >= 5 && box_option(row).is_none()
}

fn is_edge_tail(row: &str) -> bool {
    !blank(row)
        && leading_spaces(row) == 0
        && !is_top_rule(row)
        && !row.starts_with('❯')
        && box_option(row).is_none()
}

fn approval_kind(title: &str, write_path: bool, url: bool, tool: bool) -> PromptKind {
    let t = crate::text::trim(title);
    if t == const { anchor_text("muse.box.shell") } {
        PromptKind::Bash
    } else if t == const { anchor_text("muse.box.file") } && write_path {
        PromptKind::Write
    } else if t == const { anchor_text("muse.box.network") } && url {
        PromptKind::NetworkRequest
    } else if t == const { anchor_text("muse.box.tool") } && tool {
        PromptKind::Tool
    } else {
        PromptKind::Other
    }
}

fn approval_role(label: &str) -> Role {
    if label == const { anchor_text("muse.box.once") }
        || label == const { anchor_text("muse.box.stage_once") }
    {
        Role::Once
    } else if label == "No"
        || label.starts_with("No,")
        || label == const { anchor_text("muse.box.stage_abort") }
        || label == const { anchor_text("muse.box.reject") }
    {
        Role::Deny
    } else if (label.starts_with("Yes,") || label.starts_with("Yes "))
        && label.ends_with("this session")
    {
        Role::Session
    } else if label.starts_with("Yes,")
        || label.starts_with("Yes ")
        || label.starts_with(const { anchor_text("muse.box.always_prefix") })
    {
        Role::Persist
    } else {
        Role::Other
    }
}

fn is_stage_row(t: &str) -> bool {
    t.starts_with(const { anchor_text("muse.box.stage_skipped") })
        || t.starts_with(const { anchor_text("muse.box.stage_argv") })
        || t.strip_prefix(const { anchor_text("muse.box.stage") })
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
}

fn approval_prompt(rows: &[String], b: Approval) -> PromptV2 {
    let body = b.title.map_or(0, |t| t + 1)..b.first;
    let trimmed = |i: usize| crate::text::trim(&rows[i]);
    let block_from = |at: usize| -> Vec<&str> {
        (at..b.first)
            .map(trimmed)
            .take_while(|t| !t.is_empty() && !is_stage_row(t))
            .collect()
    };
    let notes: Vec<String> = body
        .clone()
        .map(trimmed)
        .filter(|t| is_stage_row(t))
        .map(str::to_string)
        .collect();
    let command_rows: Vec<String> = body
        .clone()
        .find(|&i| trimmed(i).starts_with("$ "))
        .map(|at| {
            block_from(at)
                .into_iter()
                .enumerate()
                .map(|(k, t)| {
                    if k == 0 {
                        t.trim_start_matches('$').trim_start().to_string()
                    } else {
                        t.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let write_label = const { anchor_text("muse.box.write_path") }.trim_end();
    let path = body
        .clone()
        .find(|&i| {
            trimmed(i)
                .strip_prefix(write_label)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
        .and_then(|at| {
            let rows = block_from(at);
            let first = rows[0].strip_prefix(write_label).unwrap_or_default().trim();
            let out: String = std::iter::once(first)
                .chain(rows[1..].iter().copied())
                .collect();
            (!out.is_empty()).then_some(out)
        });
    let url_label = const { anchor_text("muse.box.full_url") };
    let url = body
        .clone()
        .find(|&i| trimmed(i).starts_with(url_label))
        .and_then(|at| {
            let rows = block_from(at);
            let first = rows[0].strip_prefix(url_label).unwrap_or_default().trim();
            let out: String = std::iter::once(first)
                .chain(rows[1..].iter().copied())
                .collect();
            (!out.is_empty()).then_some(out)
        });
    let network: Vec<String> = body
        .clone()
        .map(trimmed)
        .filter(|t| t.starts_with(const { anchor_text("muse.box.network_row") }))
        .map(str::to_string)
        .collect();
    let tool_label = const { anchor_text("muse.box.tool_row") };
    let tool = body
        .clone()
        .find(|&i| trimmed(i).starts_with(tool_label))
        .and_then(|at| {
            let rows = block_from(at);
            let first = rows[0].strip_prefix(tool_label).unwrap_or_default().trim();
            let out: String = std::iter::once(first)
                .chain(rows[1..].iter().copied())
                .collect();
            (!out.is_empty()).then_some(out)
        });
    let kind = match b.title {
        Some(t) => approval_kind(&rows[t], path.is_some(), url.is_some(), tool.is_some()),
        None => PromptKind::Other,
    };
    let network_command = (kind == PromptKind::Other
        && b.title
            .is_some_and(|t| trimmed(t) == const { anchor_text("muse.box.network") }))
    .then(|| command_rows.join(" "));
    let (command_rows, path, notes) = match kind {
        PromptKind::Bash => (command_rows, None, notes),
        PromptKind::Write => (Vec::new(), path, Vec::new()),
        PromptKind::NetworkRequest => (Vec::new(), None, network),
        _ => (Vec::new(), None, Vec::new()),
    };

    let mut texts: Vec<(u8, String, bool, usize)> = Vec::new();
    for (i, row) in rows.iter().enumerate().take(b.last + 1).skip(b.first) {
        if let Some((n, text, focused)) = box_option(row) {
            texts.push((n, text.to_string(), focused, i));
        } else if let Some((_, text, _, _)) = texts.last_mut() {
            *text = format!("{text} {}", crate::text::trim(row));
        }
    }
    let mut options: Vec<Opt> = Vec::new();
    let mut keys: Vec<Option<String>> = Vec::new();
    for (n, text, focused, i) in &texts {
        let named = text.split_once("  ").map_or(text.as_str(), |(l, _)| l);
        let (label, key) = label_and_key(crate::text::trim(named));
        options.push(Opt {
            n: Some(*n),
            label: label.to_string(),
            role: Role::Other,
            focused: *focused,
            row: *i,
            kept: false,
        });
        keys.push(key.map(str::to_string));
    }
    if matches!(
        kind,
        PromptKind::Bash | PromptKind::Write | PromptKind::NetworkRequest | PromptKind::Tool
    ) && sound(&options)
    {
        for o in &mut options {
            o.role = approval_role(&o.label);
        }
    }
    let cancel = options
        .iter()
        .zip(&keys)
        .find(|(_, k)| k.as_deref() == Some("esc"))
        .map(|(o, _)| Cancel {
            key: "esc".to_string(),
            verb: o.label.clone(),
            effect: match approval_role(&o.label) {
                Role::Deny => CancelEffect::Reject,
                _ => CancelEffect::Back,
            },
        });
    let command = match kind {
        PromptKind::Bash => command_rows.join(" "),
        PromptKind::NetworkRequest => url.unwrap_or_default(),
        PromptKind::Tool => tool.unwrap_or_default(),
        _ => network_command.or_else(|| path.clone()).unwrap_or_default(),
    };
    let title_row = b.title.unwrap_or(b.first);
    PromptV2 {
        kind,
        title: trimmed(title_row).to_string(),
        command_rows,
        description_rows: 0,
        gutter: false,
        command,
        description: String::new(),
        notes,
        auto_deny: None,
        path,
        options,
        select: Select::ArrowsEnter,
        cancel,
        question: b.title,
        span: (title_row, b.end),
        footer: None,
        head_off_screen: b.title.is_none(),
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

#[derive(Debug, Clone)]
struct Question {
    head: Option<usize>,
    first: usize,
    last: usize,
    position: Option<usize>,
    keys: (usize, usize),
    key_words: String,
}

fn question_keys(rows: &[String], end: usize) -> Option<(usize, String)> {
    let mut words = String::new();
    for start in (end.saturating_sub(2)..=end).rev() {
        let row = &rows[start];
        if blank(row) || leading_spaces(row) != 2 {
            return None;
        }
        let t = crate::text::trim(row);
        words = if words.is_empty() {
            t.to_string()
        } else {
            format!("{t} {words}")
        };
        if words.starts_with("Enter to ") {
            return words
                .ends_with(const { anchor_text("muse.question.esc") })
                .then_some((start, words));
        }
    }
    None
}

fn is_question_head(row: &str) -> bool {
    row.strip_prefix(STATUS_GLYPHS)
        .and_then(|r| r.strip_prefix(' '))
        .is_some_and(|r| r.starts_with(const { anchor_text("muse.question.title") }))
}

fn is_position_row(row: &str) -> bool {
    leading_spaces(row) == 2
        && crate::text::trim(row)
            .split_once(" of ")
            .is_some_and(|(k, n)| {
                [k, n]
                    .iter()
                    .all(|d| !d.is_empty() && d.len() <= 2 && d.bytes().all(|b| b.is_ascii_digit()))
            })
}

fn is_note_row(row: &str) -> bool {
    !blank(row) && leading_spaces(row) == 6 && question_option(row).is_none()
}

fn question_box(rows: &[String], f: Frame) -> Option<Question> {
    let end = (0..f.top).rev().find(|&i| !blank(&rows[i]))?;
    let (start, key_words) = question_keys(rows, end)?;
    let mut last = (0..start).rev().find(|&i| !blank(&rows[i]))?;
    let position = is_position_row(&rows[last]).then_some(last);
    if position.is_some() {
        last = (0..last).rev().find(|&i| !blank(&rows[i]))?;
    }
    let under = |row: &str| is_question_wrap(row) || is_note_row(row);
    let mut option_row = last;
    while under(&rows[option_row]) {
        option_row = option_row.checked_sub(1)?;
    }
    question_option(&rows[option_row])?;
    let mut first = option_row;
    while first > 0 && (question_option(&rows[first - 1]).is_some() || under(&rows[first - 1])) {
        first -= 1;
    }
    while question_option(&rows[first]).is_none() {
        first += 1;
    }
    let head = (0..first)
        .rev()
        .find(|&i| !blank(&rows[i]) && leading_spaces(&rows[i]) == 0)
        .filter(|&i| is_question_head(&rows[i]));
    Some(Question {
        head,
        first,
        last,
        position,
        keys: (start, end),
        key_words,
    })
}

fn is_question_wrap(row: &str) -> bool {
    !blank(row) && leading_spaces(row) >= 7 && question_option(row).is_none()
}

fn question_title(rows: &[String], head: Option<usize>, first: usize) -> String {
    match head {
        Some(h) => {
            let t = crate::text::trim(rows[h].strip_prefix(STATUS_GLYPHS).unwrap_or(&rows[h]));
            t.rsplit_once(" — ").map_or(t, |(t, _)| t).to_string()
        }
        None => crate::text::trim(&rows[first]).to_string(),
    }
}

fn question_prompt(rows: &[String], q: Question) -> PromptV2 {
    let trimmed = |i: usize| crate::text::trim(&rows[i]);
    let beside = |text: &str| {
        text.split_once("  ")
            .is_some_and(|(_, rest)| !crate::text::trim(rest).is_empty())
    };
    let stacked = !(q.first..=q.last)
        .any(|i| question_option(&rows[i]).is_some_and(|(_, text, _)| beside(text)));
    let mut options: Vec<Opt> = Vec::new();
    let mut described = false;
    for (i, row) in rows.iter().enumerate().take(q.last + 1).skip(q.first) {
        if let Some((n, text, focused)) = question_option(row) {
            let (label, _) = text.split_once("  ").unwrap_or((text, ""));
            options.push(Opt {
                n: Some(n),
                label: crate::text::trim(label).to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            });
            described = stacked || beside(text);
        } else if is_note_row(row) {
            // Note row: not part of an option label.
        } else if !described && let Some(o) = options.last_mut() {
            // A wrapped label; under a described option, the description's.
            o.label = format!("{} {}", o.label, crate::text::trim(row));
        }
    }
    let keys = q.key_words.clone();
    if q.head.is_some()
        && q.position.is_none()
        && keys == const { anchor_text("muse.question.keys") }
        && sound(&options)
    {
        for o in &mut options {
            o.role = if o.label == const { anchor_text("muse.question.none") } {
                Role::Other
            } else {
                Role::Answer
            };
        }
    }
    let asked: Vec<usize> = (q.head.map_or(0, |h| h + 1)..q.first)
        .filter(|&i| !blank(&rows[i]))
        .collect();
    PromptV2 {
        kind: PromptKind::Question,
        title: question_title(rows, q.head, q.first),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: asked
            .iter()
            .map(|&i| trimmed(i))
            .collect::<Vec<_>>()
            .join(" "),
        notes: q
            .position
            .map(|r| trimmed(r).to_string())
            .into_iter()
            .collect(),
        auto_deny: None,
        path: None,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb: "interrupt".to_string(),
            effect: CancelEffect::Interrupt,
        }),
        question: asked.first().copied(),
        span: (q.head.unwrap_or(q.first), q.keys.1),
        footer: Some(keys),
        head_off_screen: q.head.is_none(),
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// Parsed `Note (optional)` field on a Muse Code question card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionNotes {
    pub row: usize,
    pub text: String,
}

/// Extracts the [`QuestionNotes`] field from the active question card on `rows`.
#[must_use]
pub fn question_notes(rows: &[String]) -> Option<QuestionNotes> {
    let f = frame(rows)?;
    let q = question_box(rows, f)?;
    let label = const { anchor_text("muse.question.note") }.trim_end();
    let at = (q.first..=q.last)
        .find(|&i| is_note_row(&rows[i]) && crate::text::trim(&rows[i]).starts_with(label))?;
    let mut text = crate::text::trim(&rows[at])
        .strip_prefix(label)
        .unwrap_or_default()
        .trim_start()
        .to_string();
    for r in rows[at + 1..=q.last].iter().take_while(|r| is_note_row(r)) {
        text.push(' ');
        text.push_str(crate::text::trim(r));
    }
    let text = text.trim_end_matches('▌').trim_end().to_string();
    Some(QuestionNotes { row: at, text })
}

#[derive(Debug, Clone)]
struct Review {
    head: Option<usize>,
    keys: (usize, usize),
    key_words: String,
    answers: std::ops::Range<usize>,
    options: std::ops::RangeInclusive<usize>,
}

fn review_option(row: &str) -> Option<(&'static str, bool)> {
    let (focused, label) = match row.strip_prefix("  ⟩ ") {
        Some(rest) => (true, rest),
        None => (false, row.strip_prefix("    ")?),
    };
    let label = crate::text::trim(label);
    [
        anchor_text("muse.question.submit"),
        anchor_text("muse.question.interrupt"),
    ]
    .into_iter()
    .find(|&l| l == label)
    .map(|l| (l, focused))
}

fn is_review_answer_row(row: &str) -> bool {
    !blank(row) && (leading_spaces(row) >= 4 || row.starts_with("  ⟩ "))
}

fn review_box(rows: &[String], f: Frame) -> Option<Review> {
    let end = (0..f.top).rev().find(|&i| !blank(&rows[i]))?;
    review_option(&rows[end])?;
    let mut first = end;
    while first > 0 && review_option(&rows[first - 1]).is_some() {
        first -= 1;
    }
    let mut answers_start = first;
    while answers_start > 0 && is_review_answer_row(&rows[answers_start - 1]) {
        answers_start -= 1;
    }
    let keys_end = answers_start.checked_sub(1)?;
    let mut words = String::new();
    let mut keys_start = None;
    for k in (keys_end.saturating_sub(1)..=keys_end).rev() {
        if blank(&rows[k]) || leading_spaces(&rows[k]) != 2 {
            return None;
        }
        let t = crate::text::trim(&rows[k]);
        words = if words.is_empty() {
            t.to_string()
        } else {
            format!("{t} {words}")
        };
        if words.starts_with(const { anchor_text("muse.question.review") }) {
            keys_start = Some(k);
            break;
        }
    }
    let keys_start = keys_start?;
    if !words.ends_with(const { anchor_text("muse.question.review.back") }) {
        return None;
    }
    let head = (0..keys_start)
        .rev()
        .find(|&i| !blank(&rows[i]) && leading_spaces(&rows[i]) == 0)
        .filter(|&i| is_question_head(&rows[i]));
    Some(Review {
        head,
        keys: (keys_start, keys_end),
        key_words: words,
        answers: answers_start..first,
        options: first..=end,
    })
}

fn review_prompt(rows: &[String], r: Review) -> PromptV2 {
    let options: Vec<Opt> = r
        .options
        .clone()
        .filter_map(|i| {
            review_option(&rows[i]).map(|(label, focused)| Opt {
                n: None,
                label: label.to_string(),
                role: Role::Other,
                focused,
                row: i,
                kept: false,
            })
        })
        .collect();
    let answers: Vec<&str> = r
        .answers
        .clone()
        .map(|i| {
            let t = crate::text::trim(&rows[i]);
            t.strip_prefix("⟩ ").unwrap_or(t)
        })
        .collect();
    PromptV2 {
        kind: PromptKind::Question,
        title: question_title(rows, r.head, r.keys.0),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: answers.join(" "),
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options,
        select: Select::ArrowsEnter,
        cancel: Some(Cancel {
            key: "esc".to_string(),
            verb: "go back".to_string(),
            effect: CancelEffect::Back,
        }),
        question: Some(r.keys.0),
        span: (r.head.unwrap_or(r.keys.0), *r.options.end()),
        footer: Some(r.key_words),
        head_off_screen: r.head.is_none(),
        question_dialog: None,
        reason_rows: Vec::new(),
    }
}

/// Returns whether `p` is a Muse Code multi-question review card.
#[must_use]
pub fn is_review(p: &PromptV2) -> bool {
    p.kind == PromptKind::Question
        && p.footer
            .as_deref()
            .is_some_and(|f| f.starts_with(const { anchor_text("muse.question.review") }))
}

/// Parses the active modal prompt card (trust, approval, question, or review) on `rows`.
#[must_use]
pub fn prompt(rows: &[String]) -> Option<PromptV2> {
    if let Some(t) = trust_box(rows) {
        return Some(trust_prompt(rows, t));
    }
    if let Some(b) = approval_box(rows) {
        return Some(approval_prompt(rows, b));
    }
    let f = frame(rows)?;
    if let Some(q) = question_box(rows, f) {
        return Some(question_prompt(rows, q));
    }
    review_box(rows, f).map(|r| review_prompt(rows, r))
}

/// Returns the top row index where a running turn's status indicator may appear.
#[must_use]
pub fn busy_zone_start(rows: &[String]) -> usize {
    frame(rows).map_or(0, |f| scope(rows, f.top).2)
}

/// Returns whether `rows` match Muse Code's screen layout when the process name is generic.
pub(crate) fn is_muse_screen(rows: &[String]) -> bool {
    any_frame(rows).is_some_and(|f| labelled(&rows[f.top]))
        || approval_box(rows).is_some()
        || trust_box(rows).is_some()
        || signed_out(rows).is_some()
}

/// Returns the `(top, bottom)` rule row indices of Muse Code's open composer frame.
#[must_use]
pub fn composer_rules(rows: &[String]) -> Option<(usize, usize)> {
    open_frame(rows).map(|f| (f.top, f.bottom))
}

fn muse_banner_version(row: &str) -> Option<&str> {
    if leading_spaces(row) != 2 {
        return None;
    }
    let after = crate::text::trim(row)
        .strip_prefix(const { anchor_text("muse.banner") })?
        .strip_prefix(' ')?;
    let len = after
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'-' | b'_' | b'+'))
        .count();
    let version = &after[..len];
    (!version.is_empty()
        && version.starts_with(|c: char| c.is_ascii_digit())
        && crate::text::trim(&after[len..]).is_empty())
    .then_some(version)
}

pub const LAUNCH_CARD_BUILD: crate::BannerBuild = crate::BannerBuild::ProvenanceOnly;

pub const LAUNCH_CARD_ROWS: usize = 1;

fn muse_card_at(rows: &[String], top: usize) -> Option<CardPlace> {
    let row0 = rows.get(top)?;
    muse_banner_version(row0)?;
    let end0 = crate::text::trim_end(row0).chars().count();
    Some(CardPlace {
        top,
        rows: 1,
        mascot_row: 0,
        text_col: 2,
        mascot_end: 0,
        ends: [end0, end0, end0],
    })
}

/// Scans `rows` for Muse Code's launch banner row and [`CardPlace`].
#[must_use]
pub fn launch_card_scan(rows: &[String]) -> (Option<usize>, Option<CardPlace>) {
    for i in (0..rows.len()).rev() {
        if is_session_banner(rows, i) {
            return (Some(i), muse_card_at(rows, i));
        }
    }
    (None, None)
}

/// Returns whether `changed` rows leave a previously scanned launch banner unmoved.
#[must_use]
pub fn launch_card_unmoved(rows: &[String], banner: Option<usize>, changed: &[usize]) -> bool {
    if banner.is_some_and(|b| b >= rows.len() || changed.contains(&b))
        || changed.iter().any(|&row| row >= rows.len())
    {
        return false;
    }
    let draws =
        |i: usize| banner.is_none_or(|b| i >= b) && rows.get(i).is_some_and(|r| is_banner_row(r));
    changed.iter().all(|&j| {
        let under = (j + 1..rows.len()).find(|&k| !blank(&rows[k]));
        !draws(j) && under.is_none_or(|k| !draws(k))
    })
}

/// Extracts the version string (`1.4.10`) from a Muse Code launch banner at `place`.
#[must_use]
pub fn launch_card_version<'a>(rows: &'a [String], place: &CardPlace) -> Option<&'a str> {
    muse_banner_version(rows.get(place.top)?)
}

/// Extracts the three header text lines from a Muse Code launch banner at `place`.
#[must_use]
pub fn launch_card_words<'a>(rows: &'a [String], place: &CardPlace) -> Option<[&'a str; 3]> {
    let w = crate::text::trim(rows.get(place.top)?);
    (!w.is_empty()).then_some([w, w, w])
}

/// Returns whether `row` is the composer's top rule at rest (the idle voice-input rule).
#[must_use]
pub fn is_idle_voice_rule(row: &str) -> bool {
    is_top_rule(row) && crate::text::contains(row, const { anchor_text("muse.composer.voice") })
}

fn newest_cards(rows: &[String]) -> Vec<std::ops::Range<usize>> {
    let Some(f) = frame(rows) else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    let mut i = 0;
    while i < f.top {
        if rows[i].starts_with('┌')
            && let Some(end) = (i + 1..f.top).find(|&j| rows[j].starts_with('└'))
        {
            let head = rows.get(i + 1).map_or("", String::as_str);
            if crate::text::contains(head, const { anchor_text("muse.card.status") })
                || crate::text::contains(head, const { anchor_text("muse.card.session_usage") })
            {
                cards.push(i + 1..end);
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    cards
}

fn card_value<'a>(rows: &'a [String], label: &str) -> Option<&'a str> {
    newest_cards(rows)
        .into_iter()
        .rev()
        .find_map(|body| card_body_value(rows, body, label))
}

fn card_body_value<'a>(
    rows: &'a [String],
    body: std::ops::Range<usize>,
    label: &str,
) -> Option<&'a str> {
    body.rev().find_map(|j| {
        let t = card_text(&rows[j])?;
        let rest = t.strip_prefix(label)?;
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            return None;
        }
        let value = crate::text::trim(rest);
        (!value.is_empty()).then_some(value)
    })
}

fn subscription_tier(line: &str) -> Option<String> {
    let (_, tier) = line.rsplit_once(" · ")?;
    let tier = crate::text::trim(tier);
    (!tier.is_empty()).then(|| tier.to_owned())
}

/// Extracts the subscription plan tier from a `/status` or `/usage` card on `rows`.
#[must_use]
pub fn subscription_card_plan(rows: &[String]) -> Option<String> {
    let billing = card_value(rows, const { anchor_text("muse.card.billing") });
    let line = billing.or_else(|| {
        newest_cards(rows).into_iter().rev().find_map(|body| {
            body.rev().find_map(|j| {
                let t = card_text(&rows[j])?;
                t.starts_with(const { anchor_text("muse.card.subscription") })
                    .then_some(t)
            })
        })
    })?;
    subscription_tier(line)
}

/// Extracts `(account_email, billing_plan)` from a Muse Code `/status` card on `rows`.
#[must_use]
pub fn status_card_account(rows: &[String]) -> Option<(String, Option<String>)> {
    let (account, body) = newest_cards(rows).into_iter().rev().find_map(|body| {
        let account = card_body_value(
            rows,
            body.clone(),
            const { anchor_text("muse.card.account") },
        )?;
        Some((account, body))
    })?;
    let email = account
        .strip_suffix(')')
        .and_then(|s| s.rsplit_once(" ("))
        .map(|(_, email)| crate::text::trim(email))?;
    if !email.contains('@') || email.chars().any(char::is_whitespace) {
        return None;
    }
    let plan = card_body_value(rows, body, const { anchor_text("muse.card.billing") })
        .and_then(subscription_tier);
    Some((email.to_owned(), plan))
}

/// Weekly subscription quota percentage and reset text read from a Muse Code `/usage` card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageCardWeekly {
    pub text: String,
    pub reset: Option<String>,
}

/// Extracts the [`UsageCardWeekly`] quota summary from a Muse Code `/usage` card on `rows`.
#[must_use]
pub fn usage_card_weekly(rows: &[String]) -> Option<UsageCardWeekly> {
    let weekly = card_value(rows, const { anchor_text("muse.card.weekly") })?;
    let (used, rest) = weekly.split_once('%')?;
    if !crate::text::trim(rest).starts_with("used") {
        return None;
    }
    let used: u32 = crate::text::trim(used).parse().ok()?;
    let reset = weekly
        .split_once("Resets ")
        .map(|(_, words)| crate::text::trim(words).to_owned())
        .filter(|words| !words.is_empty());
    Some(UsageCardWeekly {
        text: format!("weekly {}% left", 100u32.saturating_sub(used)),
        reset,
    })
}

fn parse_muse_footer_model(row: &str) -> Option<(String, Option<String>)> {
    if leading_spaces(row) != 2 {
        return None;
    }
    let parts: Vec<&str> = crate::text::trim(row).split(" \u{00B7} ").collect();
    if parts.len() >= 4 {
        let model = crate::text::trim(parts[0]);
        let effort = crate::text::trim(parts[1]).to_ascii_lowercase();
        let effort = matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max")
            .then_some(effort);
        if !model.is_empty() {
            return Some((model.to_owned(), effort));
        }
    } else if parts.len() == 3 {
        let model = crate::text::trim(parts[0]);
        if !model.is_empty() {
            return Some((model.to_owned(), None));
        }
    }
    None
}

/// Extracts Muse Code's [`LaunchCard`] metadata for the harness footer.
#[must_use]
pub fn launch_card(rows: &[String]) -> Option<LaunchCard> {
    launch_card_read(rows).map(|read| read.card)
}

/// Extracts [`launch_card`] together with the screen row indices it was read from.
#[must_use]
pub fn launch_card_read(rows: &[String]) -> Option<crate::LaunchCardRead> {
    let f = frame(rows)?;
    let footer_model = rows
        .get(f.footer)
        .and_then(|r| parse_muse_footer_model(r))
        .map(|model| (f.footer, model));
    let (_, place) = launch_card_scan(&rows[..f.top]);
    let banner = place
        .as_ref()
        .and_then(|p| Some((p.top, launch_card_version(rows, p)?)))
        .filter(|(_, version)| !version.is_empty())
        .map(|(top, version)| (top, version.to_owned()));
    let (card, banner, status) = match (banner, footer_model) {
        (Some((top, version)), Some((row, (model, effort)))) => (
            LaunchCard {
                version,
                model,
                effort,
            },
            Some(top),
            Some(row),
        ),
        (Some((top, version)), None) => (
            LaunchCard {
                version,
                model: String::new(),
                effort: None,
            },
            Some(top),
            None,
        ),
        (None, Some((row, (model, effort)))) => (
            LaunchCard {
                version: String::new(),
                model,
                effort,
            },
            None,
            Some(row),
        ),
        (None, None) => return None,
    };
    Some(crate::LaunchCardRead {
        card,
        banner,
        status,
    })
}

#[doc(hidden)]
pub mod fixtures {
    pub const BOX_TRUST: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-trust.txt");
    pub const IDLE: &str = include_str!("fixtures/muse-1.4.9-R6297.1-idle.txt");
    pub const IDLE_NARROW: &str =
        include_str!("fixtures/muse-1.4.11-R6667.1-idle-narrow-26-40col.txt");
    pub const DRAFT: &str = include_str!("fixtures/muse-1.4.9-R6297.1-draft.txt");
    pub const BUSY: &str = include_str!("fixtures/muse-1.4.9-R6297.1-busy-thinking.txt");
    pub const BUSY_ALT_GLYPH: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-busy-thinking-alt-glyph.txt");
    pub const END_OF_TURN: &str = include_str!("fixtures/muse-1.4.9-R6297.1-end-of-turn.txt");
    pub const INTERRUPTED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-interrupted-draft-restored.txt");
    pub const SLASH_MENU: &str = include_str!("fixtures/muse-1.4.9-R6297.1-slash-menu.txt");
    pub const EXIT_CONFIRM: &str = include_str!("fixtures/muse-1.4.9-R6297.1-exit-confirm.txt");
    pub const EXITED: &str = include_str!("fixtures/muse-1.4.9-R6297.1-exited-resume.txt");
    pub const SIGNED_OUT: &str = include_str!("fixtures/muse-1.4.9-R6297.1-wall-signed-out.txt");

    pub const TRUST_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-trust.txt");
    pub const IDLE_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-idle.txt");
    pub const DRAFT_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-draft.txt");
    pub const SLASH_MENU_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-slash-menu.txt");
    pub const END_OF_TURN_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn.txt");
    pub const REPLY_WRAPPED_50_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-reply-wrapped-50col.txt");
    pub const END_OF_TURN_READ_ROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-end-of-turn-read-row.txt");
    pub const TOOL_ROW_SEARCH: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-search.txt");
    pub const TOOL_ROW_SEARCH_NONE: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-search-none.txt");
    pub const TOOL_ROW_WRITE: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-write.txt");
    pub const TOOL_ROW_EDIT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-edit.txt");
    pub const TOOL_ROW_BASH: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-bash.txt");
    pub const TOOL_ROW_READ_MEMORY: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-read-memory.txt");
    pub const TOOL_ROW_ADD_MEMORY: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-add-memory.txt");
    pub const TOOL_ROW_GOAL: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-goal.txt");
    pub const TOOL_ROW_WORK_LIST: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-work-list.txt");
    pub const TOOL_ROW_TODOS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-tool-row-todos.txt");
    pub const BUSY_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-busy-thinking.txt");
    pub const BUSY_FIRST_FRAME: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-lab-busy-first-frame.txt");
    pub const INTERRUPTING: &str = include_str!("fixtures/muse-1.4.10-R6427.1-interrupted.txt");
    pub const INTERRUPTED_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-interrupted-settled.txt");
    pub const EXIT_CONFIRM_1_4_10: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-exit-confirm.txt");
    pub const EXITED_1_4_10: &str = include_str!("fixtures/muse-1.4.10-R6427.1-exited-resume.txt");
    pub const RESUMED_AFTER_CRASH: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-resumed-after-crash.txt");
    pub const BOX_SHELL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-shell.txt");
    pub const BOX_SHELL_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-approved.txt");
    pub const BOX_SHELL_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-denied.txt");
    pub const BOX_WRITE: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-write.txt");
    pub const BOX_WRITE_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-write-approved.txt");
    pub const BOX_EDIT: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-edit.txt");
    pub const BOX_EDIT_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-edit-denied.txt");
    pub const QUESTION: &str = include_str!("fixtures/muse-1.4.10-R6427.1-question.txt");
    pub const QUESTION_DOWN: &str = include_str!("fixtures/muse-1.4.10-R6427.1-question-down.txt");
    pub const QUESTION_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-answered.txt");
    pub const QUESTION_NONE: &str = include_str!("fixtures/muse-1.4.10-R6427.1-question-none.txt");
    pub const QUESTION_NONE_ENTER: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-none-enter.txt");
    pub const RATE_LIMIT_RETRY: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-rate-limit-retry.txt");
    pub const RATE_LIMIT_FINAL: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-rate-limit-final.txt");
    pub const SERVER_ERROR_RETRY: &str = include_str!("fixtures/muse-1.4.10-R6427.1-500-retry.txt");
    pub const SERVER_ERROR_FINAL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-500-final.txt");
    pub const KEY_REJECTED: &str = include_str!("fixtures/muse-1.4.10-R6427.1-401.txt");
    pub const USAGE_WALL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-limit-429.txt");
    pub const USAGE_CARD: &str = include_str!("fixtures/muse-1.4.10-R6427.1-usage.txt");
    pub const STATUS_CARD: &str = include_str!("fixtures/muse-1.4.10-R6427.1-status.txt");

    pub const USAGE_SIGNED_IN: &str =
        include_str!("fixtures/muse-1.4.11-R6667.1-usage-signed-in.txt");
    pub const USAGE_WALL_RULES_UNDER: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-limit-rules-under.txt");
    pub const USAGE_WALL_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-limit-status-cards.txt");
    pub const STATUS_MID_TURN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-status-mid-turn.txt");
    pub const STATUS_MID_TURN_ENDED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-status-mid-turn-ended.txt");
    pub const FRESH_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-fresh-status-cards.txt");
    pub const INTERRUPTED_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-interrupted-status-cards.txt");
    pub const LIMIT_UNNAMED: &str = include_str!("fixtures/muse-1.4.10-R6427.1-limit-unnamed.txt");
    pub const LIMIT_UNNAMED_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-limit-unnamed-status-cards.txt");
    pub const COMPLETED_STATUS_CARDS: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-completed-status-cards.txt");

    pub const BOX_SHELL_STAGE: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-stage.txt");
    pub const BOX_SHELL_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-wrapped.txt");
    pub const BOX_WRITE_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-write-wrapped.txt");
    pub const QUESTION_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-wrapped.txt");
    pub const QUESTION_WRAPPED_72: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-wrapped-72.txt");
    pub const DRAFT_WRAPPED: &str = include_str!("fixtures/muse-1.4.10-R6427.1-draft-wrapped.txt");
    pub const DRAFT_BURST_ENTER: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-draft-burst-enter.txt");
    pub const END_OF_TURN_FINISHED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-finished.txt");
    pub const END_OF_TURN_WORKED_RECAP: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-worked-recap.txt");

    pub const BOX_COMMAND: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-command.txt");
    pub const BOX_COMMAND_FOCUS_DENY: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-focus-deny.txt");
    pub const BOX_COMMAND_WRAPPED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-wrapped.txt");
    pub const BOX_COMMAND_STAGED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-staged.txt");
    pub const BOX_COMMAND_STAGE_2_OF_2: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-command-stage-2-of-2.txt");
    pub const COMMAND_DENIED: &str = include_str!("fixtures/muse-1.4.9-R6297.1-command-denied.txt");
    pub const END_OF_TURN_RAN_COMMAND: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-end-of-turn-ran-command.txt");
    pub const BOX_QUESTION: &str = include_str!("fixtures/muse-1.4.9-R6297.1-box-question.txt");
    pub const BOX_QUESTION_NOTE: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-note.txt");
    pub const BOX_QUESTION_1_OF_2: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-1-of-2.txt");
    pub const BOX_QUESTION_REVIEW: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-review.txt");
    pub const BOX_QUESTION_MULTI_SELECT: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-multi-select.txt");
    pub const BOX_QUESTION_PREVIEW: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-box-question-preview.txt");
    pub const QUESTION_ANSWERED_1_4_9: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-question-answered.txt");
    pub const QUESTION_INTERRUPTED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-question-interrupted.txt");
    pub const INTERRUPTED_MARKER: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-interrupted-marker.txt");
    pub const IDLE_RELAUNCHED: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-idle-relaunched.txt");
    pub const IDLE_RULES_NOTICE: &str =
        include_str!("fixtures/muse-1.4.9-R6297.1-idle-claude-rules-notice.txt");

    pub const BOX_NETWORK_COMMAND: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-command.txt");
    pub const END_OF_TURN_FOOTNOTES_HINT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-footnotes-hint.txt");

    pub const BOX_NETWORK: &str = include_str!("fixtures/muse-1.4.10-R6427.1-box-network.txt");
    pub const BOX_NETWORK_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-narrow.txt");
    pub const BOX_NETWORK_MONITOR: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-monitor.txt");
    pub const BOX_NETWORK_DIGIT_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-digit-denied.txt");
    pub const BOX_NETWORK_DIGIT_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-network-digit-approved.txt");
    pub const BOX_SHELL_DIGIT_2: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-digit-2.txt");
    pub const BOX_SHELL_KEY_Y: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-key-y.txt");
    pub const BOX_SHELL_DIGIT_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-shell-digit-approved.txt");
    pub const END_OF_TURN_WORK_PANEL: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-work-panel.txt");
    pub const END_OF_TURN_WORK_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-work-running.txt");
    pub const END_OF_TURN_WORK_HINT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-work-hint.txt");
    pub const DRAFT_WORK_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-draft-work-running.txt");
    pub const BUSY_WORK_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-busy-work-running.txt");
    pub const END_OF_TURN_BG_RUNNING: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-bg-running.txt");
    pub const END_OF_TURN_BG_FINISHED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-end-of-turn-bg-finished.txt");
    pub const QUESTION_MULTI: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi.txt");
    pub const QUESTION_MULTI_SECOND: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-second.txt");
    pub const QUESTION_MULTI_DIGIT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-digit.txt");
    pub const QUESTION_MULTI_NOTE_TYPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-note-typed.txt");
    pub const QUESTION_MULTI_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-review.txt");
    pub const QUESTION_MULTI_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-answered.txt");
    pub const QUESTION_MULTI_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-narrow.txt");
    pub const QUESTION_MULTI_REVIEW_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-review-narrow.txt");
    pub const QUESTION_MULTI_REVIEW_INTERRUPT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multi-review-interrupt.txt");
    pub const QUESTION_MULTISELECT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect.txt");
    pub const QUESTION_MULTISELECT_TOGGLED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-toggled.txt");
    pub const QUESTION_MULTISELECT_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-review.txt");
    pub const QUESTION_MULTISELECT_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-answered.txt");
    pub const QUESTION_MULTISELECT_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-multiselect-narrow.txt");
    pub const QUESTION_NOTE_OPEN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-open.txt");
    pub const QUESTION_NOTE_TYPED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-typed.txt");
    pub const QUESTION_NOTE_LONG: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-long.txt");
    pub const QUESTION_NOTE_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-review.txt");
    pub const QUESTION_NOTE_LONG_REVIEW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-long-review.txt");
    pub const QUESTION_NOTE_LONG_ANSWERED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-long-answered.txt");
    pub const QUESTION_NOTE_ESC: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-esc.txt");
    pub const QUESTION_NOTE_SHORT: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-short.txt");
    pub const QUESTION_NOTE_TAB_CLEARED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-question-note-tab-cleared.txt");

    pub const BOX_TOOL_ACTION: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action.txt");
    pub const BOX_TOOL_ACTION_100X30: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-100x30.txt");
    pub const BOX_TOOL_ACTION_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-narrow.txt");
    pub const BOX_TOOL_ACTION_DOWN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-down.txt");
    pub const BOX_TOOL_ACTION_HTTP: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-http.txt");
    pub const BOX_TOOL_ACTION_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-approved.txt");
    pub const BOX_TOOL_ACTION_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-tool-action-denied.txt");
    pub const BOX_UNIX_SOCKET: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket.txt");
    pub const BOX_UNIX_SOCKET_DOWN: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-down.txt");
    pub const BOX_UNIX_SOCKET_NARROW: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-narrow.txt");
    pub const BOX_UNIX_SOCKET_APPROVED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-approved.txt");
    pub const BOX_UNIX_SOCKET_DENIED: &str =
        include_str!("fixtures/muse-1.4.10-R6427.1-box-unix-socket-denied.txt");
}

#[cfg(test)]
#[path = "muse_tests.rs"]
mod tests;
