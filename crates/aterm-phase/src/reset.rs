// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's LIMIT RESET — the grant row of the usage-limit dialog
//! ([`reset_row`]), the confirmation it opens ([`reset_confirm`]), the two
//! pending lines between them ([`reset_pending`]) and the one line the
//! vendor answers with ([`reset_line`]) — as pure functions over a parsed
//! box, a screen's rows or a line of text.
//!
//! It is here for the supervisor's limit-reset step
//! (`docs/SPEC-claude-accounts-resets-2026-10-02.md`, R2, R11, R14, R15): a
//! reset is the owner's scarce purchase, so the row is matched by its exact
//! template and never by its place, the confirmation must read exactly
//! before anything is pressed on it, and only one line says a reset was
//! spent. Every reader here REFUSES what it does not read whole: `None`,
//! [`ConfirmRead::Inexact`] and [`ResetLine::Other`] are the answers to
//! anything reworded, and none of them spends a reset.
//!
//! NOTHING HERE IS MEASURED. No wall with a reset on offer has been seen on
//! a screen or in a transcript: every text below is READ in the 2.1.287
//! bundle, and every fixture ([`fixtures`]) is COMPOSED and says so on its
//! first line. What is read, with where (line@offset of the 2.1.287 strings
//! dump the spec cites):
//!
//! * THE TEXTS (the table `lg`, L335446@11761): the row `Use your reset now
//!   ({left} left, until {date})`; `Use your reset?`; `Refills your {limits}
//!   now · your weekly reset day stays {week}`; `{resets} left · use by
//!   {deadline}`; `Yes, use my reset`; `No, keep it`; the answers `Limits
//!   reset · your weekly reset day stays {week} · {resets} left` and `Reset
//!   kept · run /limit-reset at a limit to use it · use by {deadline}`; the
//!   early lines `You still have {percent}% of your {limit} left — use your
//!   reset anyway?` and `You haven't reached a limit yet — use your reset
//!   anyway?`. The server's flag payload may replace any of them (`rM()`,
//!   the same line): a replaced text is one these readers refuse.
//! * THE GRAMMAR (`Wm`, L335446@14326): `{left}` is a bare number;
//!   `{resets}` is `no resets`, `1 reset` or `<n> resets`. A ` · ` segment
//!   whose placeholder has no value is DROPPED; where all would drop, a
//!   parenthetical holding one is removed — a grant with no end date draws a
//!   bare `Use your reset now`.
//! * THE DATES (`Tu`, L326509@3186; `h4`, L354351@18342): `{date}` is the
//!   day alone (`Oct 5`; `Jan 2, 2027` in another year); `{deadline}` and
//!   `{week}` are a day and an hour (`Oct 5 at 3pm`, `Oct 5 at 3:30pm`),
//!   `en-US`, in the process's zone, with no zone named. The ` at ` is the
//!   runtime's join, MEASURED on a wall notice the same formatter drew
//!   (`resets Oct 1 at 9am`).
//! * THE LIMITS (`ar`, L354351@18603): `session`, `weekly`, `Opus`,
//!   `Sonnet`, each the name of a limit type ([`reset_limit_name`]); one
//!   name reads `<name> limit`, several `a, b and c limits`, none `limits`.
//! * THE DIALOG'S ROWS (L361856@28434): the stop row, then the grant row —
//!   else, ONLY where there is no grant row, the weekly session reset's
//!   `Reset your session limit now` (L335446@15008), which claims AT ONCE
//!   with no confirmation — then the wait row. The two share one slot, which
//!   is why the row is never taken by its place.
//! * THE ROW TAKEN (L361856@32364): the dialog redraws as its title over a
//!   dim `Checking for a reset…`, then either ends with one answer line or
//!   mounts the confirmation in its place.
//! * THE CONFIRMATION (`wGe`, L361846@563; its buttons `qn`, L350439): the
//!   wall dialog's own Dialog component (`he`, L350368@6052) over a column of
//!   `[early line | nothing, body, dim detail]`, then two UNNUMBERED options,
//!   yes then no, the focus opening on yes. While it claims: its title over a
//!   dim `Resetting your limits… this usually takes a few seconds` with no
//!   input guide (L361846@1501).
//! * THE ANSWER LINES (`mr`, L354351@27749; the strings from @32114): the
//!   table on [`reset_line`]. The spent line and the two lifted ones end ` ·
//!   send a message to continue` only where the vendor did not queue its own
//!   continuation (`Mt`, `Wt`).
//!
//! NOT READ: an alternate layout of the confirmation's buttons (`b0n`, taken
//! when a flag of the vendor's UI chunk is on); how a row wraps; what the
//! input guide says under the confirmation (the fixtures draw the Dialog's
//! fallback, `Enter to confirm · Esc to cancel`).

use crate::anchors::anchor;
use crate::phase::{has_composer_frame, is_rule, leading_spaces};
use crate::prompt::{PromptKind, PromptV2, is_panel_rule};

/// The vendor's segment separator (`K8t`, declared beside `Wm`).
const SEP: &str = " · ";

/// `en-US`'s short month names, as the vendor's date formatter draws them.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The grant row of a usage-limit dialog, as read ([`reset_row`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetRow {
    /// Its place among [`PromptV2::options`], from zero.
    pub index: usize,
    /// Its label as drawn: what a press fences on.
    pub label: String,
    /// The resets left, one at least.
    pub left: u32,
    /// The grant's last day as drawn (`Oct 5`, `Jan 2, 2027`): a day, never
    /// a deadline — the hour is the confirmation's ([`ResetConfirm`]).
    pub day: String,
}

/// The one grant row of a usage-limit dialog: an option labelled exactly `Use
/// your reset now (<n> left, until <day>)`, `n` one at least, `<day>` a month
/// and a day (and a year, in another one). `None` with no such row, with more
/// than one, with a row that only opens with those words (the bare `Use your
/// reset now` of a grant with no end date, a reworded one, `0 left`), or
/// with the weekly session reset's row anywhere among the options — the
/// vendor draws that one in the grant row's slot and it claims at once, so a
/// menu that shows it is refused whole although the vendor never draws both.
/// Never by its place: the row is found wherever it stands.
///
/// Only in the dialog read whole as the usage-limit dialog
/// ([`PromptKind::UsageLimit`], its title on the screen), its options
/// numbered `1.` on in order with one cursor at most, as the vendor's Select
/// draws them.
#[must_use]
pub fn reset_row(prompt: &PromptV2) -> Option<ResetRow> {
    if prompt.kind != PromptKind::UsageLimit || prompt.head_off_screen {
        return None;
    }
    let numbered = prompt
        .options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.map(usize::from) == Some(k + 1));
    let cursors = prompt.options.iter().filter(|o| o.focused).count();
    if !numbered
        || cursors > 1
        || prompt
            .options
            .iter()
            .any(|o| opens(&o.label, anchor("reset.weekly_row")))
    {
        return None;
    }
    let mut grants = prompt
        .options
        .iter()
        .enumerate()
        .filter(|(_, o)| opens(&o.label, anchor("reset.row")));
    let (index, opt) = grants.next()?;
    if grants.next().is_some() {
        return None;
    }
    let (left, day) = grant_of(&opt.label)?;
    Some(ResetRow {
        index,
        label: opt.label.clone(),
        left,
        day: day.to_string(),
    })
}

/// Whether choosing an option labelled `label` SPENDS a reset: the grant row
/// in any spelling of its head (`Use your reset now`, bare or with its
/// count), the weekly session reset's row, the confirmation's yes. By the
/// label's opening words, ASCII case ignored, so a reworded tail still reads
/// as a spend — the question a rule asks before it says yes to anything.
#[must_use]
pub fn spends_reset(label: &str) -> bool {
    ["reset.row", "reset.weekly_row", "reset.yes"]
        .iter()
        .any(|id| opens(label, anchor(id)))
}

/// Whether `label` opens with `words`, ASCII case ignored.
fn opens(label: &str, words: &str) -> bool {
    label
        .trim_start()
        .get(..words.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(words))
}

/// `Use your reset now (<n> left, until <day>)` → `(n, day)`, the whole
/// label and nothing else.
fn grant_of(label: &str) -> Option<(u32, &str)> {
    let inner = label
        .strip_prefix(anchor("reset.row"))?
        .strip_prefix(" (")?
        .strip_suffix(')')?;
    let (left, day) = inner.split_once(&format!(" {} ", anchor("reset.row.until")))?;
    let left = number(left).filter(|&n| n >= 1)?;
    (take_day(day) == Some("")).then_some((left, day))
}

/// A number as the vendor's `String(n)` draws it: ASCII digits, no sign and
/// no leading zero.
fn number(text: &str) -> Option<u32> {
    let plain = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    plain.then(|| text.parse().ok()).flatten()
}

/// The vendor's `{resets}`: `no resets` is none, `1 reset` one, `<n> resets`
/// two or more — each only in its own spelling.
fn resets_of(text: &str) -> Option<u32> {
    match text {
        "no resets" => Some(0),
        "1 reset" => Some(1),
        _ => number(text.strip_suffix(" resets")?).filter(|&n| n >= 2),
    }
}

/// The digits `text` opens with, as a number in `range`, and the rest.
fn take_number(text: &str, range: std::ops::RangeInclusive<u32>) -> Option<(u32, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let n = number(&text[..end]).filter(|n| range.contains(n))?;
    Some((n, &text[end..]))
}

/// `text` less the day it opens with — `Mon D`, or `Mon D, YYYY` — or `None`
/// where it opens with none.
fn take_day(text: &str) -> Option<&str> {
    let month = MONTHS.iter().find(|m| text.starts_with(**m))?;
    let rest = text[month.len()..].strip_prefix(' ')?;
    let (_, rest) = take_number(rest, 1..=31)?;
    // A year is four digits; anything else after the comma is not the day's.
    Some(
        rest.strip_prefix(", ")
            .and_then(|year| take_number(year, 1000..=9999))
            .map_or(rest, |(_, rest)| rest),
    )
}

/// The day and hour `text` opens with — `Mon D[, YYYY] at H[:MM](am|pm)` —
/// and the rest, or `None` where it opens with none.
fn take_when(text: &str) -> Option<(&str, &str)> {
    let rest = take_day(text)?.strip_prefix(" at ")?;
    let (_, rest) = take_number(rest, 1..=12)?;
    let rest = match rest.strip_prefix(':') {
        Some(minutes) => {
            let two = minutes.get(..2)?;
            let ok = two.bytes().all(|b| b.is_ascii_digit()) && two < "60";
            if !ok {
                return None;
            }
            &minutes[2..]
        }
        None => rest,
    };
    let rest = rest
        .strip_prefix("am")
        .or_else(|| rest.strip_prefix("pm"))?;
    Some((&text[..text.len() - rest.len()], rest))
}

/// The vendor's `{limits}` → its names: `<name> limit` is one, `a, b and c
/// limits` several. `None` for the bare `limits` (a grant that names none)
/// and for a name that is not one word.
fn limits_of(text: &str) -> Option<Vec<String>> {
    let names: Vec<&str> = match text.strip_suffix(" limit") {
        Some(one) => vec![one],
        None => {
            let (head, last) = text.strip_suffix(" limits")?.rsplit_once(" and ")?;
            head.split(", ").chain([last]).collect()
        }
    };
    names
        .iter()
        .all(|n| !n.is_empty() && n.chars().all(char::is_alphanumeric))
        .then(|| names.iter().map(|n| (*n).to_string()).collect())
}

/// The name a walled window is drawn by in the confirmation's `{limits}`
/// ([`ResetConfirm::limits`]), from the vendor's limit type on the wall's own
/// row (`ar`, L354351@18603): the windows a grant can clear. `None` for any
/// other type — a credit wall, a type this build does not know, a row that
/// names none.
///
/// `seven_day_sonnet` is drawn `weekly` on some plans (READ): only its own
/// name is answered here, so on such a plan the confirmation names another
/// limit than the wall and is kept.
#[must_use]
pub fn reset_limit_name(limit_type: &str) -> Option<&'static str> {
    match limit_type {
        "five_hour" => Some("session"),
        "seven_day" | "seven_day_overage_included" => Some("weekly"),
        "seven_day_opus" => Some("Opus"),
        "seven_day_sonnet" => Some("Sonnet"),
        _ => None,
    }
}

/// The reset confirmation, read exactly ([`reset_confirm`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetConfirm {
    /// The row of its title, `Use your reset?`: what an Escape fences on.
    pub title_row: usize,
    /// The row of `Yes, use my reset`: what the Enter fences on.
    pub yes_row: usize,
    /// Whether the cursor is on that row (it opens there).
    pub yes_focused: bool,
    /// The limits the body says it refills, as drawn: `session`, `weekly`,
    /// `Opus`, `Sonnet`.
    pub limits: Vec<String>,
    /// The weekly reset day and hour the body says stays (`Oct 9 at 5am`).
    pub week: String,
    /// The resets left, one at least.
    pub left: u32,
    /// The day and hour the detail says to use it by (`Oct 5 at 3pm`).
    pub deadline: String,
    /// The body and the detail as read: their rows, trimmed, joined with one
    /// space.
    pub head: String,
}

/// What a box reads as, asked whether it is the reset confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmRead {
    /// Not the confirmation: neither its title nor its yes, nor — on a box
    /// of unnumbered options — its no or a row of its body.
    Not,
    /// The confirmation by its title, its yes or what else is left of it,
    /// but not read exactly — and the first thing that was not: `title`,
    /// `options`, `early`, `body`, `week`, `count`, `deadline`. Nothing says
    /// yes to it.
    Inexact(&'static str),
    /// The confirmation, every part of it read.
    Exact(ResetConfirm),
}

/// The reset confirmation on `rows`, where `prompt` is the box read from
/// them. [`ConfirmRead::Not`] unless the box's title is `Use your reset?`,
/// one of its options opens with `Yes, use my reset`, or what is left of the
/// confirmation says so where those two do not ([`keeps_a_reset`]): the
/// server may replace any text of it, and a pane shorter than the box cuts
/// its title off, so a box is told by ANY of its parts and entered only on
/// ALL of them. [`ConfirmRead::Exact`] needs ALL of:
///
/// * the box's head is on the screen and its title row is the title —
///   else `title`;
/// * its options are exactly `Yes, use my reset` then `No, keep it`,
///   unnumbered, the cursor on one of them — else `options`;
/// * the rows from under the title to the first option, trimmed, blank ones
///   dropped, joined with one space, read `Refills your <limits> now · your
///   weekly reset day stays <when> <1 reset|N resets> left · use by <when>`
///   and nothing else, `<when>` being `Mon D[, YYYY] at H[:MM](am|pm)` and
///   `<limits>` `<name> limit` or `a, b and c limits`. The vendor's early
///   line anywhere in them (a reset used with budget left) is `early`; a
///   head that is not `Refills your <limits> now`, or anything after the
///   deadline, is `body`; a week segment missing or unread is `week`; a
///   count that is not `1 reset` or `<n> resets` (two or more) is `count`;
///   a deadline missing or with no hour is `deadline`.
#[must_use]
pub fn reset_confirm(prompt: &PromptV2, rows: &[String]) -> ConfirmRead {
    let title = anchor("reset.confirm.title");
    let yes = anchor("reset.yes");
    if prompt.title != title
        && !prompt.options.iter().any(|o| opens(&o.label, yes))
        && !keeps_a_reset(prompt, rows)
    {
        return ConfirmRead::Not;
    }
    let titled = !prompt.head_off_screen
        && prompt.title == title
        && rows
            .get(prompt.span.0)
            .is_some_and(|r| crate::text::trim(r) == title);
    if !titled {
        return ConfirmRead::Inexact("title");
    }
    let [confirm, keep] = prompt.options.as_slice() else {
        return ConfirmRead::Inexact("options");
    };
    if confirm.label != yes
        || keep.label != anchor("reset.no")
        || confirm.n.is_some()
        || keep.n.is_some()
        || confirm.focused == keep.focused
    {
        return ConfirmRead::Inexact("options");
    }
    let Some(body) = rows.get(prompt.span.0 + 1..confirm.row) else {
        return ConfirmRead::Inexact("body");
    };
    let head = body
        .iter()
        .map(|r| crate::text::trim(r))
        .filter(|r| !r.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (limits, week, left, deadline) = match confirm_text(&head) {
        Ok((limits, week, left, deadline)) => {
            (limits, week.to_string(), left, deadline.to_string())
        }
        Err(why) => return ConfirmRead::Inexact(why),
    };
    ConfirmRead::Exact(ResetConfirm {
        title_row: prompt.span.0,
        yes_row: confirm.row,
        yes_focused: confirm.focused,
        limits,
        week,
        left,
        deadline,
        head,
    })
}

/// Whether a box with neither the confirmation's title nor its yes is the
/// confirmation all the same, by what is left of it ([`reset_confirm`]): its
/// options are UNNUMBERED, as the confirmation's are, and either one of them
/// is its no — `No, keep it`, which no other box of the 2.1.287 bundle
/// spells — or a row above them carries a part of its body: the head
/// `Refills your`, either join (`your weekly reset day stays`, `left · use
/// by`), or the early line's tail.
///
/// Unnumbered, because a permission box — numbered — shows a command or a
/// diff, which may quote any of these words; the vendor's dialogs of
/// unnumbered options draw its own text.
fn keeps_a_reset(prompt: &PromptV2, rows: &[String]) -> bool {
    let Some(first) = prompt.options.first() else {
        return false;
    };
    if prompt.options.iter().any(|o| o.n.is_some()) {
        return false;
    }
    if prompt
        .options
        .iter()
        .any(|o| crate::text::trim(&o.label) == anchor("reset.no"))
    {
        return true;
    }
    let stays = anchor("reset.confirm.stays");
    let stays = stays.split(SEP).last().unwrap_or(stays);
    rows.get(prompt.span.0..first.row.min(rows.len()))
        .unwrap_or_default()
        .iter()
        .map(|row| crate::text::trim(row))
        .any(|row| {
            row.starts_with(anchor("reset.confirm.refills"))
                || [stays, anchor("reset.confirm.use_by"), anchor("reset.early")]
                    .iter()
                    .any(|part| row.contains(part))
        })
}

/// The confirmation's body and detail, joined ([`reset_confirm`]) → the
/// limits, the week, the count and the deadline; or the first part that does
/// not read.
fn confirm_text(text: &str) -> Result<(Vec<String>, &str, u32, &str), &'static str> {
    if text.contains(anchor("reset.early")) {
        return Err("early");
    }
    // The two joins, and the word each opens with: `now`, `left`.
    let stays = anchor("reset.confirm.stays");
    let use_by = anchor("reset.confirm.use_by");
    let now = stays.split(SEP).next().unwrap_or(stays);
    let left = use_by.split(SEP).next().unwrap_or(use_by);
    // `Refills your <limits> now`, the word whole.
    let (limits, rest) = text
        .strip_prefix(anchor("reset.confirm.refills"))
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|rest| rest.split_once(&format!(" {now}")))
        .filter(|(_, rest)| rest.is_empty() || rest.starts_with(' '))
        .ok_or("body")?;
    let limits = limits_of(limits).ok_or("body")?;
    // ` · your weekly reset day stays <when>`.
    let (week, rest) = rest
        .strip_prefix(&stays[now.len()..])
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(take_when)
        .ok_or("week")?;
    // ` <resets> left`, one reset at least.
    let (count, rest) = rest
        .strip_prefix(' ')
        .and_then(|rest| rest.split_once(&format!(" {left}")))
        .ok_or("count")?;
    let count = resets_of(count).filter(|&n| n >= 1).ok_or("count")?;
    // ` · use by <when>`, and nothing after it.
    let (deadline, rest) = rest
        .strip_prefix(&use_by[left.len()..])
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(take_when)
        .ok_or("deadline")?;
    if !rest.is_empty() {
        return Err("body");
    }
    Ok((limits, week, count, deadline))
}

/// A line the vendor draws while a reset is being looked up or claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetPending {
    /// `Checking for a reset…` under the wall dialog's title: the row was
    /// taken and the offer is being read. Escape there closes the dialog and
    /// prints nothing (READ).
    Checking,
    /// `Resetting your limits…` under the confirmation's title: the yes was
    /// taken and the claim is in flight. The vendor takes no key there (READ).
    Resetting,
}

/// The pending line on `rows` and its row, where one is what the screen
/// shows. `Some` only when ALL hold:
///
/// * no composer frame is on the rows (the dialog replaces the composer);
/// * the LOWEST row that is a dialog title of the reset program — `What do
///   you want to do?` or `Use your reset?`, alone on its row right under a
///   full-width rule (`▔` or `─`) — has, as the first row under it, a row
///   that opens with that title's pending line: `Checking for a reset` under
///   the first, `Resetting your limits` under the second;
/// * under that there is nothing but the line's own wrapped rows, blank
///   rows, and at most one input guide as the last row — all at the title's
///   column.
///
/// The usage-limit dialog itself (options under its title), the confirmation
/// (its body), a pending line quoted in a transcript (no rule over a title
/// above it, or a composer under it) and the two lines under the wrong title
/// are none.
#[must_use]
pub fn reset_pending(rows: &[String]) -> Option<(ResetPending, usize)> {
    const LINES: [(ResetPending, &str, &str); 2] = [
        (ResetPending::Checking, "limit.title", "reset.checking"),
        (
            ResetPending::Resetting,
            "reset.confirm.title",
            "reset.resetting",
        ),
    ];
    if has_composer_frame(rows) {
        return None;
    }
    let (title, (pending, _, line)) = (1..rows.len()).rev().find_map(|i| {
        let under_rule = is_panel_rule(&rows[i - 1]) || is_rule(&rows[i - 1]);
        let named = LINES
            .iter()
            .find(|(_, title, _)| crate::text::trim(&rows[i]) == anchor(title))?;
        under_rule.then_some((i, *named))
    })?;
    let column = leading_spaces(&rows[title]);
    let mut under: Vec<usize> = (title + 1..rows.len())
        .filter(|&i| !crate::text::trim(&rows[i]).is_empty())
        .collect();
    if under.iter().any(|&i| leading_spaces(&rows[i]) != column) {
        return None;
    }
    if under.len() > 1 && under.last().is_some_and(|&i| is_guide(&rows[i])) {
        under.pop();
    }
    let (&first, &last) = (under.first()?, under.last()?);
    // The line and its wrap are one block of rows.
    let block = under.len() == last - first + 1;
    let text = under
        .iter()
        .map(|&i| crate::text::trim(&rows[i]))
        .collect::<Vec<_>>()
        .join(" ");
    (block && text.starts_with(anchor(line))).then_some((pending, first))
}

/// A dialog's input guide: key hints joined by ` · `, one of them the cancel
/// hint (`Enter to confirm · Esc to cancel`, `Esc to cancel`).
fn is_guide(row: &str) -> bool {
    let t = crate::text::trim(row);
    t.split(SEP).all(|item| item.contains(" to "))
        && t.split(SEP)
            .any(|item| item.eq_ignore_ascii_case(anchor("prompt.cancel")))
}

/// What the vendor's answer to a reset says ([`reset_line`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetLine {
    /// `Limits reset · …`: a reset was spent and the limits are refilled.
    /// `left` is the count its `<resets> left` segment draws, where it draws
    /// one; `asks` is whether it ends ` · send a message to continue` (the
    /// vendor queued no continuation of its own).
    Spent { left: Option<u32>, asks: bool },
    /// `Your limit had already reset on its own · nothing was used`, or
    /// `Your limits are clear`: the wall was gone and no reset was spent.
    Lifted { asks: bool },
    /// `Reset kept · …`: the confirmation was declined.
    Kept,
    /// `Couldn't reset your limits with this login · run /login, then try
    /// again`: the claim was refused for the login.
    Login,
    /// Anything else: every other answer of the vendor's is a failure, and
    /// so is a line this reader does not know. A failure of THIS attempt,
    /// which is not to say nothing was ever spent: two of the vendor's
    /// answers say an EARLIER try at the same grant went through (`Your
    /// earlier reset went through after all · …`, `That reset was already
    /// used · …`).
    Other,
}

/// One line of the vendor's text, as the answer to a reset. The lines of a
/// claim made at a wall (READ, module header), and what each reads as:
///
/// | the line | reads |
/// |---|---|
/// | `Limits reset · your weekly reset day stays {week} · {resets} left` | [`ResetLine::Spent`] |
/// | `Your limit had already reset on its own · nothing was used`; `Your limits are clear` | [`ResetLine::Lifted`] |
/// | `Reset kept · run /limit-reset at a limit to use it · use by {deadline}` | [`ResetLine::Kept`] |
/// | `Couldn't reset your limits with this login · run /login, then try again` | [`ResetLine::Login`] |
/// | every other answer (`That reset was already used · …`, `Couldn't confirm the reset went through · …`, `No reset to use · …`, `Reset used · {resets} left`, …) | [`ResetLine::Other`] |
///
/// `Spent` goes by the line's opening `Limits reset · ` ALONE — its week and
/// its count may be dropped or reworded and the reset is spent all the same —
/// and by nothing less: `Limits reset` with no ` · ` after it is `Other`.
/// `Lifted` is one of its two lines whole, less the ending. A typographic
/// apostrophe reads as the plain one.
#[must_use]
pub fn reset_line(text: &str) -> ResetLine {
    let text = crate::text::trim(text).replace('’', "'");
    let (body, asks) = match text.strip_suffix(&format!("{SEP}{}", anchor("reset.continue"))) {
        Some(body) => (body, true),
        None => (text.as_str(), false),
    };
    let led = |id: &str| text.starts_with(&format!("{}{SEP}", anchor(id)));
    if led("reset.done") {
        let use_by = anchor("reset.confirm.use_by");
        let left = format!(" {}", use_by.split(SEP).next().unwrap_or(use_by));
        let left = body
            .split(SEP)
            .find_map(|segment| segment.strip_suffix(&left).and_then(resets_of));
        ResetLine::Spent { left, asks }
    } else if body == anchor("reset.lifted") || body == anchor("reset.clear") {
        ResetLine::Lifted { asks }
    } else if led("reset.kept") {
        ResetLine::Kept
    } else if led("reset.login") || text == anchor("reset.login") {
        ResetLine::Login
    } else {
        ResetLine::Other
    }
}

/// The reset program's screens, as rows — EVERY ONE COMPOSED from the
/// bundle's texts on the usage-limit dialog's hand-built frame
/// ([`crate::prompt::fixtures::LIMIT_OPTIONS_PANEL`]), never captured; each
/// file's first line says so, and names what it is. Read them with
/// [`crate::prompt::fixtures::screen`]. Always compiled, as
/// [`crate::prompt::fixtures`] is, so a dependent crate's tests reach them.
#[doc(hidden)]
pub mod fixtures {
    /// The usage-limit dialog at a weekly limit with a reset on offer: the
    /// grant row `Use your reset now (2 left, until Oct 5)` second.
    pub const DIALOG_GRANT: &str = include_str!("fixtures/claude-2.1.287-reset-dialog-grant.txt");
    /// The same dialog with the weekly session reset's row in that slot.
    pub const DIALOG_WEEKLY: &str = include_str!("fixtures/claude-2.1.287-reset-dialog-weekly.txt");
    /// The same dialog with a bare `Use your reset now`.
    pub const DIALOG_BARE: &str = include_str!("fixtures/claude-2.1.287-reset-dialog-bare.txt");
    /// The row taken: the dialog's title over `Checking for a reset…`.
    pub const CHECKING: &str = include_str!("fixtures/claude-2.1.287-reset-checking.txt");
    /// The confirmation as it opens, the cursor on `Yes, use my reset`.
    pub const CONFIRM: &str = include_str!("fixtures/claude-2.1.287-reset-confirm.txt");
    /// The confirmation on a 60-column grid, its body wrapped.
    pub const CONFIRM_60: &str = include_str!("fixtures/claude-2.1.287-reset-confirm-60col.txt");
    /// The confirmation of a reset used early: the early line over the body.
    pub const CONFIRM_EARLY: &str = include_str!("fixtures/claude-2.1.287-reset-confirm-early.txt");
    /// The confirmation with the cursor moved onto `No, keep it`.
    pub const CONFIRM_FOCUS_NO: &str =
        include_str!("fixtures/claude-2.1.287-reset-confirm-focus-no.txt");
    /// The yes taken: the confirmation's title over `Resetting your limits…`,
    /// no input guide.
    pub const RESETTING: &str = include_str!("fixtures/claude-2.1.287-reset-resetting.txt");

    /// Every fixture of the reset program, with its name.
    pub const ALL: [(&str, &str); 9] = [
        ("dialog-grant", DIALOG_GRANT),
        ("dialog-weekly", DIALOG_WEEKLY),
        ("dialog-bare", DIALOG_BARE),
        ("checking", CHECKING),
        ("confirm", CONFIRM),
        ("confirm-60col", CONFIRM_60),
        ("confirm-early", CONFIRM_EARLY),
        ("confirm-focus-no", CONFIRM_FOCUS_NO),
        ("resetting", RESETTING),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::fixtures::{
        LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL, limit_options_dialog, limit_options_panel,
        screen,
    };
    use crate::prompt::{Role, Select, parse_prompt_v2};

    const STOP: &str = "Stop and wait for limit to reset";
    const WAIT: &str = "Wait here, then continue automatically at Oct 9 at 5am";
    const CREDITS: &str = "Switch to usage credits";
    const GRANT: &str = "Use your reset now (2 left, until Oct 5)";
    const WEEKLY: &str = "Reset your session limit now";
    const BODY: &str = "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am";
    const DETAIL: &str = "2 resets left · use by Oct 5 at 3pm";
    const ASKS: &str = " · send a message to continue";

    fn boxed(rows: &[String]) -> PromptV2 {
        parse_prompt_v2(rows).unwrap_or_else(|| panic!("a box: {rows:#?}"))
    }

    /// The grant row of the dialog whose options are `labels` (the
    /// fullscreen panel's frame, the cursor on the first).
    fn row_among(labels: &[&str]) -> Option<ResetRow> {
        reset_row(&boxed(&limit_options_panel(labels, 0, false)))
    }

    /// MEASURED on 2.1.301 in the lab (crates/aterm-lab, 2026-10-15): the
    /// usage-limit dialog at a five-hour wall whose at-wall reads of the
    /// usage body failed (both reset programs flagged on) offers no reset:
    /// no grant row, and no option whose words spend one.
    #[test]
    fn the_measured_dialog_with_no_reset_on_offer_has_no_grant_row() {
        use crate::prompt::fixtures::LIMIT_DIALOG_NO_RESET_ROWS_2_1_301;
        let dialog = boxed(&screen(LIMIT_DIALOG_NO_RESET_ROWS_2_1_301));
        assert_eq!(reset_row(&dialog), None);
        assert_eq!(dialog.options.len(), 4, "{:?}", dialog.options);
        assert!(dialog.options.iter().all(|o| !spends_reset(&o.label)));
    }

    /// [`fixtures::CONFIRM`] with the rows between its title and its options
    /// replaced by `body`, each at the dialog's column.
    fn confirm_with(body: &[&str]) -> Vec<String> {
        let base = screen(fixtures::CONFIRM);
        assert_eq!(
            [base[5].trim(), base[6].trim()],
            [BODY, DETAIL],
            "the fixture's rows"
        );
        let mut out = base[..5].to_vec();
        out.extend(body.iter().map(|row| format!("   {row}")));
        out.extend(base[7..].iter().cloned());
        out
    }

    fn confirm_read(rows: &[String]) -> ConfirmRead {
        reset_confirm(&boxed(rows), rows)
    }

    /// `rows`, a screen of the fullscreen renderer's panel, as the inline
    /// renderer draws the same Dialog (MEASURED on 2.1.295, the wall dialog
    /// [`LIMIT_OPTIONS_DIALOG`]): a `─` rule and the box two columns in.
    fn inline(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                if is_panel_rule(row) {
                    "─".repeat(row.chars().count())
                } else {
                    row.strip_prefix(' ').unwrap_or(row).to_string()
                }
            })
            .collect()
    }

    /// THE GRANT ROW, by its exact label: the composed dialog gives its
    /// place, its count and its day, and the label a press fences on; found
    /// wherever it stands, in both frames, with a year in its day. NEGATIVE
    /// CONTROLS, each the same dialog with one thing changed: the weekly
    /// session reset's row in the slot (the row by its place would take it);
    /// a bare row; `0 left`; both rows in one menu; two grant rows; a label
    /// that only opens with the row's words; the same options under another
    /// title; options the vendor's Select would not draw.
    #[test]
    fn the_grant_row_is_read_by_its_exact_label_never_by_its_place() {
        let rows = screen(fixtures::DIALOG_GRANT);
        assert_eq!(
            rows,
            limit_options_panel(&[STOP, GRANT, WAIT, CREDITS], 0, false),
            "the fixture is the wall dialog's frame"
        );
        let p = boxed(&rows);
        assert_eq!((p.kind, p.select), (PromptKind::UsageLimit, Select::Digits));
        // The reader gives the row no role: nothing chooses it by one.
        assert_eq!(p.options[1].role, Role::Other);
        assert_eq!(
            reset_row(&p),
            Some(ResetRow {
                index: 1,
                label: GRANT.to_string(),
                left: 2,
                day: "Oct 5".to_string(),
            })
        );
        // Wherever it stands, and whatever the focus is on.
        for (labels, index) in [
            (&[GRANT, STOP, WAIT, CREDITS][..], 0),
            (&[STOP, WAIT, GRANT, CREDITS], 2),
            (&[CREDITS, STOP, WAIT, GRANT], 3),
            (&[STOP, GRANT], 1),
        ] {
            assert_eq!(
                row_among(labels).map(|r| r.index),
                Some(index),
                "{labels:?}"
            );
        }
        let focused = boxed(&limit_options_panel(&[STOP, GRANT, WAIT], 1, true));
        assert_eq!(reset_row(&focused).map(|r| r.index), Some(1));
        let inline_frame = boxed(&limit_options_dialog(&[STOP, GRANT, WAIT, CREDITS], 0));
        assert_eq!(reset_row(&inline_frame).map(|r| r.left), Some(2));
        for (label, left, day) in [
            ("Use your reset now (1 left, until Dec 31)", 1, "Dec 31"),
            (
                "Use your reset now (12 left, until Jan 2, 2027)",
                12,
                "Jan 2, 2027",
            ),
        ] {
            let read = row_among(&[STOP, label, WAIT]).expect(label);
            assert_eq!(
                (read.left, read.day.as_str(), read.label.as_str()),
                (left, day, label)
            );
        }

        // The controls.
        assert_eq!(reset_row(&boxed(&screen(fixtures::DIALOG_WEEKLY))), None);
        assert_eq!(reset_row(&boxed(&screen(fixtures::DIALOG_BARE))), None);
        assert_eq!(
            row_among(&[STOP, WEEKLY, GRANT, WAIT]),
            None,
            "both rows in one menu"
        );
        assert_eq!(
            row_among(&[STOP, GRANT, GRANT, WAIT]),
            None,
            "two grant rows"
        );
        assert_eq!(row_among(&[STOP, WAIT, CREDITS]), None, "no grant row");
        for label in [
            "Use your reset now (0 left, until Oct 5)",
            "Use your reset now (2 left)",
            "Use your reset now (until Oct 5)",
            "Use your reset now (two left, until Oct 5)",
            "Use your reset now (02 left, until Oct 5)",
            "Use your reset now (-2 left, until Oct 5)",
            "Use your reset now (2 left, until tomorrow)",
            "Use your reset now (2 left, until Oct 5 at 3pm)",
            "Use your reset now (2 left, until Oct 32)",
            "Use your reset now (2 left, until Oct 5, 27)",
            "Use your reset now (2 left, until Oct 5) and continue",
            "Use your reset now — 2 left, until Oct 5",
            "use your reset now (2 left, until Oct 5)",
            "Use a reset now (2 left, until Oct 5)",
        ] {
            assert_eq!(row_among(&[STOP, label, WAIT, CREDITS]), None, "{label}");
        }
        // The same rows under another title are not the usage-limit dialog.
        let mut retitled = rows.clone();
        retitled[3] = "   What would you like?".to_string();
        assert_ne!(boxed(&retitled).kind, PromptKind::UsageLimit);
        assert_eq!(reset_row(&boxed(&retitled)), None);
        // Options the Select would not draw: a number missing, two cursors,
        // a head off the screen.
        for edit in [
            (|p: &mut PromptV2| p.options[0].n = None) as fn(&mut PromptV2),
            |p| p.options[2].n = Some(4),
            |p| p.options[1].focused = true,
            |p| p.head_off_screen = true,
            |p| p.kind = PromptKind::Other,
        ] {
            let mut edited = p.clone();
            edit(&mut edited);
            assert_eq!(reset_row(&edited), None, "{edited:?}");
        }
    }

    /// WHAT SPENDS A RESET, by a label's opening words: the grant row in
    /// any spelling of its head, the weekly row, the confirmation's yes.
    /// NEGATIVE CONTROL: every other row of the two boxes, and the plain
    /// yeses a permission box offers, spend none.
    #[test]
    fn a_label_that_spends_a_reset_is_told_by_its_opening_words() {
        for label in [
            GRANT,
            "Use your reset now",
            "Use your reset now (…)",
            "use your reset now — 1 left",
            "  Use your reset now (2 left, until Oct 5)",
            WEEKLY,
            "Reset your session limit now (1/week)",
            "Yes, use my reset",
            "YES, USE MY RESET",
            "Yes, use my reset now",
        ] {
            assert!(spends_reset(label), "{label}");
        }
        for label in [
            STOP,
            WAIT,
            CREDITS,
            "No, keep it",
            "Yes",
            "Yes, proceed",
            "Reset used · no resets left",
            "Reset the view",
            "Use your",
            "Don’t continue automatically",
            "",
        ] {
            assert!(!spends_reset(label), "{label}");
        }
    }

    /// THE CONFIRMATION, READ EXACTLY: the composed box gives its two fenced
    /// rows, the limit it refills, the week, the count and the deadline; the
    /// same at 60 columns, its body wrapped onto a second row; the same in
    /// the inline frame; with the cursor on `No, keep it` it is still
    /// exact and says the yes is not focused. NEGATIVE CONTROL: the wall
    /// dialog it replaces, and its own pending state, are not it.
    #[test]
    fn the_confirmation_reads_exactly_plain_wrapped_and_with_the_focus_on_no() {
        let rows = screen(fixtures::CONFIRM);
        assert_eq!(rows, confirm_with(&[BODY, DETAIL]));
        let p = boxed(&rows);
        // As the box parser reads it: no kind of its own, chosen by arrows
        // and Enter, the yes with no role and the no a refusal.
        assert_eq!((p.kind, p.select), (PromptKind::Other, Select::ArrowsEnter));
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            [Role::Other, Role::Deny]
        );
        let exact = ResetConfirm {
            title_row: 3,
            yes_row: 8,
            yes_focused: true,
            limits: vec!["weekly".to_string()],
            week: "Oct 9 at 5am".to_string(),
            left: 2,
            deadline: "Oct 5 at 3pm".to_string(),
            head: format!("{BODY} {DETAIL}"),
        };
        assert_eq!(reset_confirm(&p, &rows), ConfirmRead::Exact(exact.clone()));
        assert_eq!(rows[exact.title_row].trim(), "Use your reset?");
        assert_eq!(rows[exact.yes_row].trim(), "❯ Yes, use my reset");

        let narrow = screen(fixtures::CONFIRM_60);
        assert!(narrow.iter().all(|row| row.chars().count() <= 60));
        assert_eq!(narrow[6].trim(), "stays Oct 9 at 5am", "the wrapped row");
        assert_eq!(
            confirm_read(&narrow),
            ConfirmRead::Exact(ResetConfirm {
                yes_row: 9,
                ..exact.clone()
            })
        );
        assert_eq!(
            confirm_read(&inline(&rows)),
            ConfirmRead::Exact(exact.clone())
        );
        assert_eq!(
            confirm_read(&screen(fixtures::CONFIRM_FOCUS_NO)),
            ConfirmRead::Exact(ResetConfirm {
                yes_focused: false,
                ..exact.clone()
            })
        );
        // The grammar's other spellings: several limits, one reset, a year,
        // minutes.
        for (body, detail, limits, week, left, deadline) in [
            (
                "Refills your session limit now · your weekly reset day stays Oct 9 at 5:30am",
                "1 reset left · use by Oct 5 at 12pm",
                &["session"][..],
                "Oct 9 at 5:30am",
                1,
                "Oct 5 at 12pm",
            ),
            (
                "Refills your session and weekly limits now · your weekly reset day stays Jan 2, \
                 2027 at 9am",
                "12 resets left · use by Dec 31 at 11:59pm",
                &["session", "weekly"],
                "Jan 2, 2027 at 9am",
                12,
                "Dec 31 at 11:59pm",
            ),
            (
                "Refills your session, weekly and Opus limits now · your weekly reset day stays \
                 Oct 9 at 5am",
                "2 resets left · use by Jan 1, 2027 at 1am",
                &["session", "weekly", "Opus"],
                "Oct 9 at 5am",
                2,
                "Jan 1, 2027 at 1am",
            ),
        ] {
            let ConfirmRead::Exact(read) = confirm_read(&confirm_with(&[body, detail])) else {
                panic!("{body} / {detail}");
            };
            assert_eq!(
                (
                    read.limits.iter().map(String::as_str).collect::<Vec<_>>(),
                    read.week.as_str()
                ),
                (limits.to_vec(), week)
            );
            assert_eq!((read.left, read.deadline.as_str()), (left, deadline));
        }

        for (name, text) in fixtures::ALL {
            let screen = screen(text);
            let read = parse_prompt_v2(&screen).map(|p| reset_confirm(&p, &screen));
            let confirmation = name.starts_with("confirm");
            assert_eq!(
                read.is_some_and(|read| read != ConfirmRead::Not),
                confirmation,
                "{name}"
            );
        }
        for wall in [LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL] {
            assert_eq!(confirm_read(&screen(wall)), ConfirmRead::Not);
        }
    }

    /// EVERY LIMIT TYPE A GRANT CLEARS IS NAMED AS THE CONFIRMATION READS IT
    /// BACK: the vendor's own `{limits}` for each type, quoted from the
    /// 2.1.287 bundle (`ar` over `Dhe`, L354351@18603: `five_hour:"session
    /// limit"`, `seven_day:"weekly limit"`, `seven_day_opus:"Opus limit"`,
    /// `seven_day_sonnet:"Sonnet limit"`, the overage-included week drawn
    /// `weekly`), drawn in the body, reads back as exactly the name
    /// [`reset_limit_name`] answers — what a go's limits are matched against,
    /// case and spelling alike — and the weekly types' name is the one the
    /// composed confirmation reads. Any other type is no name. NEGATIVE
    /// CONTROL: a name in another case (`Weekly`) reads back all the same,
    /// so only the vendor's spelling tells it from theirs.
    #[test]
    fn every_limit_type_maps_to_a_name_the_confirmation_reads_back() {
        let ConfirmRead::Exact(composed) = confirm_read(&screen(fixtures::CONFIRM)) else {
            panic!("the composed confirmation reads exactly");
        };
        let read_back = |limits: &str| {
            let body =
                format!("Refills your {limits} now · your weekly reset day stays Oct 9 at 5am");
            match confirm_read(&confirm_with(&[&body, DETAIL])) {
                ConfirmRead::Exact(read) => read.limits,
                other => panic!("{body}: {other:?}"),
            }
        };
        for (limit_type, drawn) in [
            ("five_hour", "session limit"),
            ("seven_day", "weekly limit"),
            ("seven_day_overage_included", "weekly limit"),
            ("seven_day_opus", "Opus limit"),
            ("seven_day_sonnet", "Sonnet limit"),
        ] {
            let name = reset_limit_name(limit_type).unwrap_or_else(|| panic!("{limit_type}"));
            assert_eq!(read_back(drawn), [name], "{limit_type}");
        }
        for limit_type in ["seven_day", "seven_day_overage_included"] {
            assert_eq!(
                reset_limit_name(limit_type).map(|name| vec![name.to_string()]),
                Some(composed.limits.clone()),
                "{limit_type}"
            );
        }
        for limit_type in ["limit", "overage", "", "seven_day_haiku"] {
            assert_eq!(reset_limit_name(limit_type), None, "{limit_type:?}");
        }

        // The control.
        assert_eq!(read_back("Weekly limit"), ["Weekly"]);
        assert_ne!(read_back("Weekly limit"), read_back("weekly limit"));
    }

    /// ANYTHING ELSE IS INEXACT, and says the first thing that did not read
    /// — one control for each word, each the exact confirmation with one
    /// thing changed. Every one of them would be the confirmation of a reset
    /// that must not be spent: an early use, another offer, a reworded box.
    #[test]
    fn a_confirmation_not_read_exactly_names_what_did_not_read() {
        const EARLY: &str = "You still have 40% of your weekly limit left — use your reset anyway?";
        for (why, body) in [
            // An early line above the body, in each of its spellings, and
            // wrapped.
            ("early", &[EARLY, BODY, DETAIL][..]),
            (
                "early",
                &[
                    "You haven't reached a limit yet — use your reset anyway?",
                    BODY,
                    DETAIL,
                ],
            ),
            (
                "early",
                &[
                    "You still have 40% of your weekly limit left — use your reset",
                    "anyway?",
                    BODY,
                    DETAIL,
                ],
            ),
            // The body's head reworded, a grant that names no limit, a name
            // that is no word, a row that is not the box's.
            (
                "body",
                &[
                    "Refill your weekly limit now · your weekly reset day stays Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            (
                "body",
                &[
                    "Refills your limits now · your weekly reset day stays Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            (
                "body",
                &[
                    "Refills your weekly limit nowhere · your weekly reset day stays Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            (
                "body",
                &[
                    "Refills your  limit now · your weekly reset day stays Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            (
                "body",
                &[
                    "Refills your weekly, limits now · your weekly reset day stays Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            ("body", &["Heads up: this cannot be undone", BODY, DETAIL]),
            ("body", &[BODY, DETAIL, "This cannot be undone"]),
            ("body", &[DETAIL, BODY]),
            ("body", &[]),
            // The week's segment dropped (the vendor drops a segment whose
            // value it lacks), reworded, or with no hour.
            ("week", &["Refills your weekly limit now", DETAIL]),
            (
                "week",
                &[
                    "Refills your weekly limit now · your weekly reset day moves to Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            (
                "week",
                &[
                    "Refills your weekly limit now · your weekly reset day stays Oct 9",
                    DETAIL,
                ],
            ),
            (
                "week",
                &[
                    "Refills your weekly limit now · your weekly reset day stays …",
                    DETAIL,
                ],
            ),
            (
                "week",
                &[
                    "Refills your weekly limit now · your weekly reset day stays Oct 9 at 13pm",
                    DETAIL,
                ],
            ),
            (
                "week",
                &[
                    "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5:3am",
                    DETAIL,
                ],
            ),
            // `resets left` with no number, none left, the wrong plural, no
            // detail at all.
            ("count", &[BODY, "resets left · use by Oct 5 at 3pm"]),
            ("count", &[BODY, "no resets left · use by Oct 5 at 3pm"]),
            ("count", &[BODY, "0 resets left · use by Oct 5 at 3pm"]),
            ("count", &[BODY, "1 resets left · use by Oct 5 at 3pm"]),
            ("count", &[BODY, "2 reset left · use by Oct 5 at 3pm"]),
            ("count", &[BODY, "… left · use by …"]),
            ("count", &[BODY, "use by Oct 5 at 3pm"]),
            ("count", &[BODY]),
            // A deadline with no hour, dropped, reworded.
            ("deadline", &[BODY, "2 resets left · use by Oct 5"]),
            ("deadline", &[BODY, "2 resets left"]),
            ("deadline", &[BODY, "2 resets left · expires Oct 5 at 3pm"]),
            (
                "deadline",
                &[BODY, "2 resets left · use by tomorrow at 3pm"],
            ),
            ("deadline", &[BODY, "2 resets left · use by Oct 5 at 3 PM"]),
        ] {
            assert_eq!(
                confirm_read(&confirm_with(body)),
                ConfirmRead::Inexact(why),
                "{body:?}"
            );
        }
        assert_eq!(
            confirm_read(&screen(fixtures::CONFIRM_EARLY)),
            ConfirmRead::Inexact("early")
        );

        // The options: numbered, a third one, the two the other way round,
        // either one reworded.
        let exact = screen(fixtures::CONFIRM);
        let options = |yes: &str, no: &str, more: &[&str]| {
            let mut rows = exact[..8].to_vec();
            rows.push(format!("   {yes}"));
            rows.push(format!("   {no}"));
            rows.extend(more.iter().map(|row| format!("   {row}")));
            rows.extend(exact[10..].iter().cloned());
            rows
        };
        assert_eq!(options("❯ Yes, use my reset", "  No, keep it", &[]), exact);
        for rows in [
            options("❯ 1. Yes, use my reset", "  2. No, keep it", &[]),
            options("❯ Yes, use my reset", "  No, keep it", &["  Ask me later"]),
            options("  No, keep it", "❯ Yes, use my reset", &[]),
            options("❯ Yes, use my reset", "  No", &[]),
            options("❯ Yes, use my reset now", "  No, keep it", &[]),
            options("❯ Yes, use my reset", "  Yes, use my reset", &[]),
        ] {
            assert_eq!(
                confirm_read(&rows),
                ConfirmRead::Inexact("options"),
                "{rows:#?}"
            );
        }
        // Two cursors, or none: not what a Select draws.
        let p = boxed(&exact);
        for focused in [(true, true), (false, false)] {
            let mut edited = p.clone();
            (edited.options[0].focused, edited.options[1].focused) = focused;
            assert_eq!(
                reset_confirm(&edited, &exact),
                ConfirmRead::Inexact("options")
            );
        }

        // The title reworded over the same yes; a head off the screen; rows
        // that are not the ones the box was read from.
        let mut retitled = exact.clone();
        retitled[3] = "   Spend your reset?".to_string();
        assert_eq!(confirm_read(&retitled), ConfirmRead::Inexact("title"));
        let mut cut = p.clone();
        cut.head_off_screen = true;
        assert_eq!(reset_confirm(&cut, &exact), ConfirmRead::Inexact("title"));
        assert_eq!(reset_confirm(&p, &retitled), ConfirmRead::Inexact("title"));
        assert_eq!(
            reset_confirm(&p, &exact[..3]),
            ConfirmRead::Inexact("title")
        );
    }

    /// A CONFIRMATION WITH NEITHER ITS TITLE NOR ITS YES IS STILL THE
    /// CONFIRMATION, by any one part left of it: the server may reword every
    /// text (READ: `rM()`), and a short pane cuts the title off. Its no, or
    /// one row of its body — the head, either join, the early line — on a box
    /// of unnumbered options reads [`ConfirmRead::Inexact`], which nothing
    /// says yes to and an owed step Escapes; read as
    /// [`ConfirmRead::Not`] it was any other box, and full power's catch-all
    /// took its reworded yes. NEGATIVE CONTROLS: the same box with every part
    /// reworded is some other box; a NUMBERED box (a permission box, whose
    /// body is a command or a diff that may quote these words) is not the
    /// confirmation by its body or by a `No, keep it` of its own; and the
    /// usage-limit dialog is not.
    #[test]
    fn a_confirmation_is_told_by_any_part_left_of_it() {
        const QUESTION: &str = "Spend it now?";
        const PLAIN: &str = "This takes effect at once";
        let exact = screen(fixtures::CONFIRM);
        // The exact confirmation with its title, body rows and options
        // replaced; `cut` drops the rule and the title, as a pane shorter
        // than the box does.
        let boxed_as = |title: &str, body: &[&str], yes: &str, no: &str, cut: bool| {
            let mut rows = exact[..3].to_vec();
            rows.push(format!("   {title}"));
            rows.push(String::new());
            rows.extend(body.iter().map(|row| format!("   {row}")));
            rows.push(String::new());
            rows.push(format!("   ❯ {yes}"));
            rows.push(format!("     {no}"));
            rows.extend(exact[10..].iter().cloned());
            if cut { rows[5..].to_vec() } else { rows }
        };
        assert_eq!(
            boxed_as(
                "Use your reset?",
                &[BODY, DETAIL],
                "Yes, use my reset",
                "No, keep it",
                false
            ),
            exact
        );
        for cut in [false, true] {
            for (what, body, no) in [
                ("its no", &[PLAIN][..], "No, keep it"),
                ("the body's head", &["Refills your tokens today"][..], "No"),
                (
                    "the week's join",
                    &["All yours again · your weekly reset day stays Oct 9 at 5am"][..],
                    "No",
                ),
                (
                    "the detail's join",
                    &["2 resets left · use by Oct 5 at 3pm"][..],
                    "No",
                ),
                (
                    "the early line",
                    &["You haven't reached a limit yet — use your reset anyway?"][..],
                    "No",
                ),
                ("the whole body", &[BODY, DETAIL][..], "Not now"),
            ] {
                let rows = boxed_as(QUESTION, body, "Yes, refill now", no, cut);
                let p = boxed(&rows);
                assert_eq!(p.head_off_screen, cut, "{what}");
                assert_eq!(
                    reset_confirm(&p, &rows),
                    ConfirmRead::Inexact("title"),
                    "{what}, cut {cut}: {rows:#?}"
                );
                // With the plain `Yes` the reviewers' reading took.
                let rows = boxed_as(QUESTION, body, "Yes", no, cut);
                assert_eq!(
                    confirm_read(&rows),
                    ConfirmRead::Inexact("title"),
                    "{what}, cut {cut}, a plain yes"
                );
            }
            // NEGATIVE CONTROL: no part of the confirmation left.
            let other = boxed_as(QUESTION, &[PLAIN], "Yes", "No", cut);
            assert_eq!(confirm_read(&other), ConfirmRead::Not, "cut {cut}");
        }
        // NEGATIVE CONTROL: numbered options — the body's words and a no of
        // that spelling make no confirmation of a permission box.
        let rows = boxed_as(QUESTION, &[BODY, DETAIL], "Yes", "No, keep it", false);
        let mut numbered = boxed(&rows);
        assert_eq!(
            reset_confirm(&numbered, &rows),
            ConfirmRead::Inexact("title"),
            "unnumbered, it is"
        );
        (numbered.options[0].n, numbered.options[1].n) = (Some(1), Some(2));
        assert_eq!(reset_confirm(&numbered, &rows), ConfirmRead::Not);
        numbered.options[1].n = None;
        assert_eq!(reset_confirm(&numbered, &rows), ConfirmRead::Not);
        for wall in [LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL] {
            assert_eq!(confirm_read(&screen(wall)), ConfirmRead::Not);
        }
        for dialog in [fixtures::DIALOG_GRANT, fixtures::DIALOG_WEEKLY] {
            assert_eq!(confirm_read(&screen(dialog)), ConfirmRead::Not);
        }
    }

    /// THE TWO PENDING LINES, each under its own title, with the row the
    /// line is on: `Checking for a reset…` under the wall dialog's (its
    /// input guide under it), `Resetting your limits…` under the
    /// confirmation's (no guide) — in both frames, and with the line wrapped.
    /// NEGATIVE CONTROLS: every other screen of the program and the measured
    /// wall dialog; each line under the other's title; no rule over the
    /// title; a composer under it; a row under the line that is not the
    /// dialog's; the line quoted at an idle composer.
    #[test]
    fn a_pending_line_is_read_under_its_own_title_and_nowhere_else() {
        let checking = screen(fixtures::CHECKING);
        let resetting = screen(fixtures::RESETTING);
        assert_eq!(reset_pending(&checking), Some((ResetPending::Checking, 5)));
        assert_eq!(checking[5].trim(), "Checking for a reset…");
        assert_eq!(
            reset_pending(&resetting),
            Some((ResetPending::Resetting, 5))
        );
        assert!(
            resetting[5]
                .trim()
                .starts_with("Resetting your limits… this usually")
        );
        assert_eq!(
            reset_pending(&inline(&checking)),
            Some((ResetPending::Checking, 5))
        );
        assert_eq!(
            reset_pending(&inline(&resetting)),
            Some((ResetPending::Resetting, 5))
        );
        // The checking state with no guide, and the claiming line wrapped
        // at 54 columns.
        assert_eq!(
            reset_pending(&checking[..6]),
            Some((ResetPending::Checking, 5))
        );
        let mut wrapped = resetting[..5].to_vec();
        wrapped.push("   Resetting your limits… this usually takes a few".to_string());
        wrapped.push("   seconds".to_string());
        assert_eq!(reset_pending(&wrapped), Some((ResetPending::Resetting, 5)));
        // What the crate's own reader says of a pending screen: idle by
        // default, no composer, no box — its word is no evidence, and this
        // reader is the only one that names the state.
        for rows in [&checking, &resetting] {
            let reading = crate::read(Some("claude"), rows, None);
            assert_eq!(
                (
                    reading.phase,
                    reading.phase_authoritative,
                    reading.composer.is_some(),
                    reading.prompt.is_some()
                ),
                (crate::Phase::Idle, false, false, false)
            );
        }

        for (name, text) in fixtures::ALL {
            let pending = matches!(name, "checking" | "resetting");
            assert_eq!(reset_pending(&screen(text)).is_some(), pending, "{name}");
        }
        for wall in [LIMIT_OPTIONS_DIALOG, LIMIT_OPTIONS_PANEL] {
            assert_eq!(reset_pending(&screen(wall)), None);
        }
        let edited = |base: &[String], edit: &dyn Fn(&mut Vec<String>)| {
            let mut rows = base.to_vec();
            edit(&mut rows);
            reset_pending(&rows)
        };
        for base in [&checking, &resetting] {
            let other = if base == &checking {
                &resetting
            } else {
                &checking
            };
            assert_eq!(
                edited(base, &|r| r[3] = other[3].clone()),
                None,
                "the other state's title"
            );
            assert_eq!(edited(base, &|r| r[2] = String::new()), None, "no rule");
            assert_eq!(
                edited(base, &|r| r[3] = format!("{} now", r[3])),
                None,
                "a title run on"
            );
            assert_eq!(
                edited(base, &|r| r[5] = "   Looking for a reset…".to_string()),
                None,
                "another line"
            );
            assert_eq!(
                edited(base, &|r| r.insert(
                    5,
                    "   ❯ 1. Stop and wait for limit to reset".to_string()
                )),
                None,
                "an option over the line"
            );
            assert_eq!(
                edited(base, &|r| r.push("host:work user$ ".to_string())),
                None,
                "a shell line under it"
            );
            assert_eq!(
                edited(base, &|r| {
                    r.splice(6..6, [String::new(), "   Something else".to_string()]);
                }),
                None,
                "a row of another block under the line"
            );
            assert_eq!(
                edited(base, &|r| r[5] = r[5][1..].to_string()),
                None,
                "the line one column out"
            );
            assert_eq!(
                edited(base, &|r| r
                    .extend(crate::prompt::fixtures::composer("  ? for shortcuts"))),
                None,
                "the composer under it"
            );
        }
        // The composer's frame ABOVE the dialog (its rows under it are
        // refused for their column alone): the frame's own refusal.
        for base in [&checking, &resetting] {
            let rule = "─".repeat(60);
            let framed: Vec<String> = [rule.clone(), "❯ ".to_string(), rule]
                .into_iter()
                .chain(base.iter().cloned())
                .collect();
            assert!(has_composer_frame(&framed));
            assert_eq!(reset_pending(&framed), None, "the composer is drawn");
            assert!(
                reset_pending(&framed[3..]).is_some(),
                "the same rows without it"
            );
        }
        // The line said at an idle composer (a transcript that quotes it).
        let mut quoted = vec![
            "⏺ It printed this:".to_string(),
            "  What do you want to do?".to_string(),
            "  Checking for a reset…".to_string(),
            String::new(),
        ];
        assert_eq!(reset_pending(&quoted), None, "no rule over the title");
        quoted.extend(crate::prompt::fixtures::composer("  ? for shortcuts"));
        assert_eq!(reset_pending(&quoted), None);
        assert_eq!(reset_pending(&[]), None);
    }

    /// The vendor's answers at a wall, each as its templates draw it (READ,
    /// the module header; the dates and counts invented): the lines of a
    /// claim, then the row's own answers.
    const FAILURES: [&str; 19] = [
        "That reset was already used · nothing changed just now · 1 reset left",
        "That reset was already used · nothing changed just now · 1 reset left · run \
         /limit-reset again to use one",
        "This reset isn't available any more · nothing was used · this limit resets Oct 9 at 5am",
        "This reset isn't available any more · your earlier try may have gone through · checking \
         your limits now",
        "A different reset is on offer now · nothing was used · run /limit-reset again to see it",
        "Another reset was just started on your account · checking your limits now · if you're \
         still at the limit in a minute, try again",
        "Your earlier reset may still be going through · checking your limits now · try again \
         in a minute if they aren't refilled",
        "Your earlier reset went through after all · checking your limits now",
        "Couldn't reset your limits · nothing was used · try again in a moment",
        "Couldn't confirm the reset went through · if you're still at the limit in a moment, try \
         again",
        "Still couldn't confirm your reset · nothing more was used · if your limits aren't \
         refilled in a few minutes, contact support",
        "Your limits are already being reset · one moment",
        "No reset to use · you're not at a usage limit right now",
        "Your reset can't be used for this limit right now · this limit resets Oct 9 at 5am",
        "Your reset doesn't cover this limit · this limit resets Oct 9 at 5am",
        "Your reset doesn't refill your session limit, so it can't be used until that resets · \
         this limit resets Oct 9 at 5am",
        "This limit may have lifted already · checking your limits now · run /limit-reset again \
         in a moment to use your reset anyway",
        "Reset used · no resets left",
        "Couldn't check for a reset right now · try again in a moment",
    ];
    const SPENT: &str = "Limits reset · your weekly reset day stays Oct 9 at 5am · 1 reset left";
    const LIFTED: [&str; 2] = [
        "Your limit had already reset on its own · nothing was used",
        "Your limits are clear",
    ];
    const KEPT: &str = "Reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    const LOGIN: &str = "Couldn't reset your limits with this login · run /login, then try again";

    /// THE ANSWER LINE, one row for each line of the vendor's table: the
    /// spent line with its count, the two lifted lines, the kept line, the
    /// login line, and every other answer a failure — `asks` with the
    /// ending and without it. NEGATIVE CONTROLS: `Limits reset` with no ` ·
    /// ` after it, a lifted line with anything after it, the weekly session
    /// reset's own success line, the wait row's line, and the same words in
    /// lower case are none of the four.
    #[test]
    fn an_answer_line_is_spent_only_by_its_opening_words() {
        let asking = |line: &str| format!("{line}{ASKS}");
        for (line, left) in [
            (SPENT, Some(1)),
            (
                "Limits reset · your weekly reset day stays Oct 9 at 5am · no resets left",
                Some(0),
            ),
            (
                "Limits reset · your weekly reset day stays Jan 2, 2027 at 5:30am · 12 resets left",
                Some(12),
            ),
            // The week's segment dropped, the count's dropped or reworded:
            // spent all the same.
            ("Limits reset · 3 resets left", Some(3)),
            (
                "Limits reset · your weekly reset day stays Oct 9 at 5am",
                None,
            ),
            ("Limits reset · you have 3 more", None),
            ("Limits reset · 1 resets left", None),
        ] {
            assert_eq!(
                reset_line(line),
                ResetLine::Spent { left, asks: false },
                "{line}"
            );
            assert_eq!(
                reset_line(&asking(line)),
                ResetLine::Spent { left, asks: true },
                "{line}"
            );
        }
        assert_eq!(
            reset_line("Limits reset · send a message to continue"),
            ResetLine::Spent {
                left: None,
                asks: true
            }
        );
        assert_eq!(
            reset_line(&format!("  {SPENT}\n")),
            ResetLine::Spent {
                left: Some(1),
                asks: false
            },
            "the row's own white space"
        );
        for line in LIFTED {
            assert_eq!(
                reset_line(line),
                ResetLine::Lifted { asks: false },
                "{line}"
            );
            assert_eq!(
                reset_line(&asking(line)),
                ResetLine::Lifted { asks: true },
                "{line}"
            );
        }
        for line in [
            KEPT,
            "Reset kept · run /limit-reset whenever you want to use it · use by Oct 5 at 3pm",
            "Reset kept · run /limit-reset at a limit to use it",
        ] {
            assert_eq!(reset_line(line), ResetLine::Kept, "{line}");
        }
        assert_eq!(reset_line(LOGIN), ResetLine::Login);
        assert_eq!(
            reset_line("Couldn’t reset your limits with this login · run /login, then try again"),
            ResetLine::Login,
            "a typographic apostrophe"
        );
        for line in FAILURES {
            assert_eq!(reset_line(line), ResetLine::Other, "{line}");
            assert_eq!(reset_line(&asking(line)), ResetLine::Other, "{line}");
        }
        for line in [
            "Limits reset",
            "Limits reset on Oct 5",
            "Limits reset: 1 reset left",
            "Limits resets · 1 reset left",
            "limits reset · 1 reset left",
            "The line was: Limits reset · 1 reset left",
            "Your limits are clear · nothing was used",
            "Your limits are clear now",
            "Your limit had already reset on its own",
            "Reset kept",
            "Reset kept, run /limit-reset at a limit to use it",
            "Couldn't reset your limits",
            "Session limit reset · next reset available Oct 9 · your weekly limit still applies",
            "Claude Code will continue automatically at 6am. Keep this session open; it may \
             still pause for permission prompts. Press esc to cancel the wait.",
            "send a message to continue",
            "",
        ] {
            assert_eq!(reset_line(line), ResetLine::Other, "{line:?}");
        }
    }

    /// NO LINE OF THE RESET PROGRAM IS A WALL: the wall table places none of
    /// the answers, the pending lines, the confirmation's rows, the grant
    /// row or the footer notice — an answer printed after the wall must not
    /// read as a wall still standing (or as a lost login: the login line
    /// names `/login` past its first ` · `). Nor is any text of the weekly
    /// session reset, the program whose row stands in the grant row's slot:
    /// its answer says the session limit is gone, and read as a session wall
    /// it would have a refilled account waited out or moved from. NEGATIVE
    /// CONTROL: the notices these lines sit beside are walls to the same
    /// classifier, a session limit's own among them.
    #[test]
    fn no_line_of_the_reset_program_is_a_wall() {
        use crate::wall::{WallKind, classify_wall};
        let fixed = [
            SPENT,
            LIFTED[0],
            LIFTED[1],
            KEPT,
            LOGIN,
            "Reset kept · run /limit-reset whenever you want to use it · use by Oct 5 at 3pm",
            "Checking for a reset…",
            "Resetting your limits… this usually takes a few seconds",
            GRANT,
            "Use your reset now",
            "Use your reset?",
            BODY,
            "Refills your session limit now · your weekly reset day stays Oct 9 at 5am",
            "Refills your session, weekly and Opus limits now · your weekly reset day stays Oct 9 \
             at 5am",
            DETAIL,
            "Yes, use my reset",
            "No, keep it",
            "You still have 40% of your weekly limit left — use your reset anyway?",
            "You still have 40% of your session limit left — use your reset anyway?",
            "You haven't reached a limit yet — use your reset anyway?",
            "/limit-reset to refill your limits · 2 resets left · use by Oct 5",
            "Reset used · 1 reset left",
            // The weekly session reset (L335446@15008): its row, its footer
            // notice, its three answers and its pending line.
            WEEKLY,
            "/limit-reset to reset your session limit now · uses weekly limit · 1/week",
            "Weekly reset used · available again Oct 9",
            "Session limit reset · next reset available Oct 9 · your weekly limit still applies",
            "Session limit reset · your weekly limit still applies",
            "Couldn't reset your session limit right now · try again in a moment",
            "Resetting your session limit… this usually takes a few seconds",
        ];
        for line in fixed.iter().chain(&FAILURES) {
            for line in [(*line).to_string(), format!("{line}{ASKS}")] {
                assert_eq!(classify_wall(&line), None, "{line}");
            }
        }
        assert_eq!(
            classify_wall("You've hit your weekly limit · resets Oct 9 at 5am"),
            Some(WallKind::UsageWeekly)
        );
        assert_eq!(
            classify_wall("Please run /login · API Error: 401"),
            Some(WallKind::Auth)
        );
        for wall in [
            "You've hit your session limit · resets 3pm",
            "Session limit reached ∙ resets 5pm",
            "You've hit your limit · resets 7:30pm (America/Los_Angeles)",
        ] {
            assert_eq!(classify_wall(wall), Some(WallKind::UsageSession), "{wall}");
        }
    }
}
