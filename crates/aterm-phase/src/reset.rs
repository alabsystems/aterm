// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-match parsers for Claude Code usage-limit reset dialogs and outcomes.
//!
//! [`reset_row`], [`reset_confirm`], [`reset_pending`], and [`reset_line`] classify [`ResetKind`]
//! and [`reset_limit_name`] strings (tested against [`fixtures`]), failing closed via
//! [`ConfirmRead::Inexact`] and [`ResetLine::Other`] on any unrecognized wording.

use crate::anchors::anchor;
use crate::phase::{has_composer_frame, is_rule, leading_spaces};
use crate::prompt::{PromptKind, PromptV2, is_panel_rule};

/// The vendor's segment separator (`wrn`, declared beside `Kg`).
const SEP: &str = " · ";

/// `en-US`'s short month names, as the vendor's date formatter draws them.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A kind of reset (2.1.304's `CLe`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetKind {
    /// `session`: the five-hour limit's (`5-hour reset`).
    Session,
    /// `weekly`: a weekly limit's (`weekly reset`).
    Weekly,
    /// `fable`: gives back what Fable used of the weekly limit (`Fable reset`).
    Fable,
    /// `full`: both limits' (`full reset`).
    Full,
}

impl ResetKind {
    /// Every kind, in the vendor's own order (`CLe`).
    pub const ALL: [ResetKind; 4] = [
        ResetKind::Session,
        ResetKind::Weekly,
        ResetKind::Fable,
        ResetKind::Full,
    ];

    /// The vendor's id of the kind (`CLe`; the dialog row's value is `cedar-ember-<id>`).
    #[inline]
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ResetKind::Session => "session",
            ResetKind::Weekly => "weekly",
            ResetKind::Fable => "fable",
            ResetKind::Full => "full",
        }
    }

    /// [`Self::word`] read back.
    #[must_use]
    pub fn of_word(word: &str) -> Option<ResetKind> {
        ResetKind::ALL.into_iter().find(|kind| kind.word() == word)
    }

    /// `{kind}`, as the vendor draws it (`X()`).
    #[must_use]
    pub fn name(self) -> &'static str {
        anchor(match self {
            ResetKind::Session => "reset.kind.session",
            ResetKind::Weekly => "reset.kind.weekly",
            ResetKind::Fable => "reset.kind.fable",
            ResetKind::Full => "reset.kind.full",
        })
    }

    /// `{Kind}`: [`Self::name`] with its first letter upper-cased.
    #[must_use]
    pub fn capital(self) -> String {
        let name = self.name();
        let mut chars = name.chars();
        chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
        })
    }

    /// The noun `{kindResets}` counts (`ce()`): `five-hour reset` for the
    /// session kind, [`Self::name`] for the others.
    fn noun(self) -> &'static str {
        match self {
            ResetKind::Session => anchor("reset.kind.session_count"),
            _ => self.name(),
        }
    }

    /// `{done}`, the head of the line that says a reset of this kind was spent (`Mt()`).
    #[must_use]
    pub fn done(self) -> &'static str {
        anchor(match self {
            ResetKind::Session => "reset.done.session",
            ResetKind::Weekly => "reset.done.weekly",
            ResetKind::Fable => "reset.done.fable",
            ResetKind::Full => "reset.done.full",
        })
    }

    /// Whether the step may take a reset of this kind (the spec's R2).
    #[inline]
    #[must_use]
    pub const fn takeable(self) -> bool {
        !matches!(self, ResetKind::Fable)
    }

    /// The confirmation's title.
    #[must_use]
    pub fn title(self) -> String {
        format!("{}?", self.row_head())
    }

    /// The confirmation's yes.
    #[must_use]
    pub fn yes(self) -> String {
        format!("{} {}", anchor("reset.yes"), self.name())
    }

    /// The grant row's head: `Use a {kind}`.
    fn row_head(self) -> String {
        format!("{} {}", anchor("reset.row"), self.name())
    }

    /// The early lines' tail: `use your {kind} anyway?`.
    fn early(self) -> String {
        format!(
            "{} {} {}",
            anchor("reset.early"),
            self.name(),
            anchor("reset.early.anyway")
        )
    }

    /// `{kindResets}`, as `ce()` counts `n` of this kind.
    #[must_use]
    pub fn resets(self, n: u32) -> String {
        let noun = self.noun();
        match n {
            0 => format!("no {noun}s"),
            1 => format!("1 {noun}"),
            n => format!("{n} {noun}s"),
        }
    }

    /// `{kindResets}` read back: `no <noun>s` is none, `1 <noun>` one, `<n>
    /// <noun>s` two or more — each only in its own spelling, and only with
    /// this kind's noun.
    fn resets_of(self, text: &str) -> Option<u32> {
        let noun = self.noun();
        let plural = |rest: &str| rest.strip_suffix('s') == Some(noun);
        if text.strip_prefix("no ").is_some_and(plural) {
            return Some(0);
        }
        if text.strip_prefix("1 ") == Some(noun) {
            return Some(1);
        }
        let (n, rest) = text.split_once(' ')?;
        plural(rest)
            .then(|| number(n))
            .flatten()
            .filter(|&n| n >= 2)
    }
}

/// The kind whose confirmation title `text` is (`Use a {kind}?`, the whole of it once trimmed).
#[must_use]
pub fn reset_title(text: &str) -> Option<ResetKind> {
    let text = crate::text::trim(text);
    ResetKind::ALL.into_iter().find(|kind| text == kind.title())
}

/// The grant row of a usage-limit dialog the step may take, as read ([`reset_row`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetRow {
    /// Its place among [`PromptV2::options`], from zero.
    pub index: usize,
    /// Its label as drawn: what a press fences.
    pub label: String,
    /// The kind of reset it offers, never [`ResetKind::Fable`].
    pub kind: ResetKind,
    /// The resets of that kind left, one at least.
    pub left: u32,
    /// The grant's last day as drawn (`Oct 5`, `Jan 2, 2027`).
    pub day: String,
}

/// The grant row the step may take in a usage-limit dialog (the spec's R2), under `order`.
#[must_use]
pub fn reset_row(prompt: &PromptV2, order: &[ResetKind]) -> Option<ResetRow> {
    if prompt.kind != PromptKind::UsageLimit || prompt.head_off_screen {
        return None;
    }
    let numbered = prompt
        .options
        .iter()
        .enumerate()
        .all(|(k, o)| o.n.map(usize::from) == Some(k + 1));
    let cursors = prompt.options.iter().filter(|o| o.focused).count();
    if !numbered || cursors > 1 {
        return None;
    }
    let mut offers: Vec<(usize, ResetKind)> = Vec::new();
    for (index, opt) in prompt.options.iter().enumerate() {
        let kind = grant_kind(&opt.label);
        if kind.is_none() && !uses_a_reset(&opt.label) && !spends_reset(&opt.label) {
            continue;
        }
        let kind = kind?;
        if offers.iter().any(|(_, seen)| *seen == kind) {
            return None;
        }
        offers.push((index, kind));
    }
    let (index, kind) = if order.is_empty() {
        match offers.as_slice() {
            [one] => *one,
            _ => return None,
        }
    } else {
        order
            .iter()
            .filter(|want| want.takeable())
            .find_map(|want| offers.iter().find(|(_, kind)| kind == want).copied())?
    };
    let label = &prompt.options[index].label;
    let kind = Some(kind).filter(|kind| kind.takeable())?;
    let (left, day) = grant_of(label, kind)?;
    Some(ResetRow {
        index,
        label: label.clone(),
        kind,
        left,
        day: day.to_string(),
    })
}

/// Whether choosing an option labelled `label` spends a reset.
#[must_use]
pub fn spends_reset(label: &str) -> bool {
    opens(label, anchor("reset.weekly_row"))
        || ResetKind::ALL
            .into_iter()
            .any(|kind| opens(label, &kind.row_head()) || opens(label, &kind.yes()))
}

/// Whether `label`, past a leading `yes, `.
#[must_use]
pub fn uses_a_reset(label: &str) -> bool {
    let l = label.trim().to_lowercase();
    let l = l
        .strip_prefix("yes,")
        .or_else(|| l.strip_prefix("yes "))
        .map_or(l.as_str(), str::trim_start);
    let Some(rest) = ["use a ", "use an ", "use my ", "use your "]
        .iter()
        .find_map(|determiner| l.strip_prefix(determiner))
    else {
        return false;
    };
    let words = rest
        .split(['(', '·', '?'])
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()));
    std::iter::once("")
        .chain(words.clone())
        .zip(words)
        .any(|(before, word)| {
            matches!(word, "reset" | "resets")
                && !matches!(before, "and" | "or" | "then" | "but" | "to")
        })
}

/// Whether `label` opens with `words`, ASCII case ignored.
fn opens(label: &str, words: &str) -> bool {
    label
        .trim_start()
        .get(..words.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(words))
}

/// The kind whose grant row `label` opens as, read whole or not: its row
/// head (`Use a <kind>`, ASCII case ignored) as a word of its own — the
/// label's end, or its ` (` or ` · ` next, as the vendor's `Kvt` draws a row
/// (the bare head of a grant with no end date, the Fable row's suffix after
/// its parentheses).
fn grant_kind(label: &str) -> Option<ResetKind> {
    let label = label.trim_start();
    ResetKind::ALL.into_iter().find(|kind| {
        let head = kind.row_head();
        opens(label, &head) && {
            let rest = &label[head.len()..];
            rest.is_empty() || rest.starts_with(" (") || rest.starts_with(" · ")
        }
    })
}

/// `Use a <kind> (<n> left, until <day>)` → `(n, day)`, the whole label and
/// nothing else.
fn grant_of(label: &str, kind: ResetKind) -> Option<(u32, &str)> {
    let inner = label
        .strip_prefix(kind.row_head().as_str())?
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

/// The name a walled window is drawn by in the confirmation's `{limits}`.
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
    /// The row of its title, `Use a <kind>?`.
    pub title_row: usize,
    /// The row of `Yes, use my <kind>`.
    pub yes_row: usize,
    /// Whether the cursor is on that row (it opens there).
    pub yes_focused: bool,
    /// The kind its title, its yes and its count name.
    pub kind: ResetKind,
    /// The limits the body says it refills, as drawn.
    pub limits: Vec<String>,
    /// The weekly reset day and hour the body says stays (`Oct 9 at 5am`).
    pub week: String,
    /// The resets of its kind left, one at least.
    pub left: u32,
    /// The day and hour the detail says to use it by (`Oct 5 at 3pm`).
    pub deadline: String,
    /// The body and the detail as read.
    pub head: String,
}

/// What a box reads as, asked whether it is the reset confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmRead {
    /// Not the confirmation: neither a title nor a yes.
    Not,
    /// The confirmation by its title, its yes or what else is left of it, but not read exactly.
    Inexact(&'static str),
    /// The confirmation, every part of it read.
    Exact(ResetConfirm),
}

/// The reset confirmation on `rows`, where `prompt` is the box read from them.
#[must_use]
pub fn reset_confirm(prompt: &PromptV2, rows: &[String]) -> ConfirmRead {
    let titled_as = reset_title(&prompt.title);
    let says_yes = prompt.options.iter().any(|o| {
        ResetKind::ALL
            .into_iter()
            .any(|kind| opens(&o.label, &kind.yes()))
    });
    if titled_as.is_none() && !says_yes && !keeps_a_reset(prompt, rows) {
        return ConfirmRead::Not;
    }
    let titled = titled_as.filter(|kind| {
        !prompt.head_off_screen
            && rows
                .get(prompt.span.0)
                .is_some_and(|r| crate::text::trim(r) == kind.title())
    });
    let Some(kind) = titled else {
        return ConfirmRead::Inexact("title");
    };
    let [confirm, keep] = prompt.options.as_slice() else {
        return ConfirmRead::Inexact("options");
    };
    if confirm.label != kind.yes()
        || keep.label != anchor("reset.no")
        || confirm.n.is_some()
        || keep.n.is_some()
        || confirm.focused == keep.focused
    {
        return ConfirmRead::Inexact("options");
    }
    if !kind.takeable() {
        return ConfirmRead::Inexact("kind");
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
    let (limits, week, left, deadline) = match confirm_text(&head, kind) {
        Ok((limits, week, left, deadline)) => {
            (limits, week.to_string(), left, deadline.to_string())
        }
        Err(why) => return ConfirmRead::Inexact(why),
    };
    ConfirmRead::Exact(ResetConfirm {
        title_row: prompt.span.0,
        yes_row: confirm.row,
        yes_focused: confirm.focused,
        kind,
        limits,
        week,
        left,
        deadline,
        head,
    })
}

/// Whether a box with neither a confirmation's title nor its yes is the
/// confirmation all the same, by what is left of it ([`reset_confirm`]): its
/// options are UNNUMBERED, as the confirmation's are, and either one of them
/// is its no — `No, keep it`, which one other box of the 2.1.304 bundle
/// spells (a delete dialog's, over `Yes, delete it`, whose refusal is as
/// safe an answer) — or a row above them carries a part of its body: the
/// head `Refills your`, either join (`your weekly reset day stays`, `left ·
/// use by`), or an early line's tail of any kind (`use your 5-hour reset
/// anyway?`).
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
    let early: Vec<String> = ResetKind::ALL.into_iter().map(ResetKind::early).collect();
    rows.get(prompt.span.0..first.row.min(rows.len()))
        .unwrap_or_default()
        .iter()
        .map(|row| crate::text::trim(row))
        .any(|row| {
            row.starts_with(anchor("reset.confirm.refills"))
                || [stays, anchor("reset.confirm.use_by")]
                    .iter()
                    .any(|part| row.contains(part))
                || early.iter().any(|tail| row.contains(tail.as_str()))
        })
}

/// The confirmation's body and detail, joined ([`reset_confirm`]), of a
/// confirmation of `kind` → the limits, the week, the count and the
/// deadline; or the first part that does not read.
fn confirm_text(
    text: &str,
    kind: ResetKind,
) -> Result<(Vec<String>, &str, u32, &str), &'static str> {
    if ResetKind::ALL
        .into_iter()
        .any(|any| text.contains(any.early().as_str()))
    {
        return Err("early");
    }
    let stays = anchor("reset.confirm.stays");
    let use_by = anchor("reset.confirm.use_by");
    let now = stays.split(SEP).next().unwrap_or(stays);
    let left = use_by.split(SEP).next().unwrap_or(use_by);
    let (limits, rest) = text
        .strip_prefix(anchor("reset.confirm.refills"))
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|rest| rest.split_once(&format!(" {now}")))
        .filter(|(_, rest)| rest.is_empty() || rest.starts_with(' '))
        .ok_or("body")?;
    let limits = limits_of(limits).ok_or("body")?;
    let (week, rest) = rest
        .strip_prefix(&stays[now.len()..])
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(take_when)
        .filter(|(_, rest)| !rest.starts_with(" ("))
        .ok_or("week")?;
    let (count, rest) = rest
        .strip_prefix(' ')
        .and_then(|rest| rest.split_once(&format!(" {left}")))
        .ok_or("count")?;
    let count = kind.resets_of(count).filter(|&n| n >= 1).ok_or("count")?;
    let (deadline, rest) = rest
        .strip_prefix(&use_by[left.len()..])
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(take_when)
        .filter(|(_, rest)| !rest.starts_with(" ("))
        .ok_or("deadline")?;
    if !rest.is_empty() {
        return Err("body");
    }
    Ok((limits, week, count, deadline))
}

/// A line the vendor draws while a reset is being looked up or claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetPending {
    /// `Checking for a reset…` under the wall dialog's title.
    Checking,
    /// `Resetting your limits…` under a confirmation's title.
    Resetting,
}

/// The pending line on `rows` and its row.
#[must_use]
pub fn reset_pending(rows: &[String]) -> Option<(ResetPending, usize)> {
    let named = |row: &str| {
        let title = crate::text::trim(row);
        if title == anchor("limit.title") {
            Some((ResetPending::Checking, "reset.checking"))
        } else {
            reset_title(title).map(|_| (ResetPending::Resetting, "reset.resetting"))
        }
    };
    if has_composer_frame(rows) {
        return None;
    }
    let (title, (pending, line)) = (1..rows.len()).rev().find_map(|i| {
        let under_rule = is_panel_rule(&rows[i - 1]) || is_rule(&rows[i - 1]);
        let named = named(&rows[i])?;
        under_rule.then_some((i, named))
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
    /// A reset of `kind` was spent.
    Spent {
        kind: ResetKind,
        left: Option<u32>,
        asks: bool,
    },
    /// `{Kind} used · you're still at your {limit} until {date} · {kindResets} left`.
    Walled { kind: ResetKind, left: u32 },
    /// `Your limit had already reset on its own · nothing was used`, or `Your limits are clear`.
    Lifted { asks: bool },
    /// `<Kind> kept · …`: the confirmation was declined.
    Kept,
    /// `Couldn't reset your limits with this login · run /login, then try again`.
    Login,
    /// Anything else: every other answer, and any line this reader does not know.
    Other,
}

/// One line of the vendor's text, as the answer to a reset made at a wall.
#[must_use]
pub fn reset_line(text: &str) -> ResetLine {
    let text = crate::text::trim(text).replace('’', "'");
    let (body, asks) = match text.strip_suffix(&format!("{SEP}{}", anchor("reset.continue"))) {
        Some(body) => (body, true),
        None => (text.as_str(), false),
    };
    if let Some((kind, left)) = spent_line(&text) {
        ResetLine::Spent { kind, left, asks }
    } else if let Some((kind, left)) = walled_line(&text) {
        ResetLine::Walled { kind, left }
    } else if body == anchor("reset.lifted") || body == anchor("reset.clear") {
        ResetLine::Lifted { asks }
    } else if ResetKind::ALL
        .into_iter()
        .any(|kind| text.starts_with(&format!("{} {}{SEP}", kind.capital(), anchor("reset.kept"))))
    {
        ResetLine::Kept
    } else if text.starts_with(&format!("{}{SEP}", anchor("reset.login")))
        || text == anchor("reset.login")
    {
        ResetLine::Login
    } else {
        ResetLine::Other
    }
}

/// The count segment `<kindResets> left` of `kind`, whole.
fn left_of(segment: &str, kind: ResetKind) -> Option<u32> {
    let use_by = anchor("reset.confirm.use_by");
    let left = use_by.split(SEP).next().unwrap_or(use_by);
    kind.resets_of(segment.strip_suffix(&format!(" {left}"))?)
}

/// `{done} · …` ([`reset_line`]): the kind, and the count of the first
/// segment that is that kind's `<kindResets> left` whole.
fn spent_line(text: &str) -> Option<(ResetKind, Option<u32>)> {
    let stays = anchor("reset.confirm.stays");
    let stays = stays.split(SEP).last().unwrap_or(stays);
    ResetKind::ALL.into_iter().find_map(|kind| {
        let rest = text.strip_prefix(kind.done())?.strip_prefix(SEP)?;
        let left = rest.split(SEP).find_map(|segment| left_of(segment, kind));
        let own = kind != ResetKind::Session
            || left.is_some()
            || rest.split(SEP).any(|segment| segment.starts_with(stays));
        own.then_some((kind, left))
    })
}

/// `{Kind} used · you're still at your {limit} until {date} · {kindResets}
/// left`, whole: the kind and the count.
fn walled_line(text: &str) -> Option<(ResetKind, u32)> {
    ResetKind::ALL.into_iter().find_map(|kind| {
        let rest = text
            .strip_prefix(kind.capital().as_str())?
            .strip_prefix(' ')?
            .strip_prefix(anchor("reset.walled"))?
            .strip_prefix(' ')?;
        let (limit, count) = rest.split_once(SEP)?;
        if !limit.contains(" until ") {
            return None;
        }
        Some((kind, left_of(count, kind)?))
    })
}

/// The reset program's screens, as rows.
#[doc(hidden)]
pub mod fixtures {
    /// The usage-limit dialog at a weekly limit with one kind on offer.
    pub const DIALOG_GRANT: &str = include_str!("fixtures/claude-2.1.304-reset-dialog-grant.txt");
    /// The dialog at a five-hour limit with three kinds on offer, in the vendor's order.
    pub const DIALOG_KINDS: &str = include_str!("fixtures/claude-2.1.304-reset-dialog-kinds.txt");
    /// The dialog at a Fable limit.
    pub const DIALOG_FABLE: &str = include_str!("fixtures/claude-2.1.304-reset-dialog-fable.txt");
    /// The dialog with the weekly session reset's row where the grant rows go.
    pub const DIALOG_WEEKLY: &str = include_str!("fixtures/claude-2.1.304-reset-dialog-weekly.txt");
    /// The dialog with a bare `Use a weekly reset`.
    pub const DIALOG_BARE: &str = include_str!("fixtures/claude-2.1.304-reset-dialog-bare.txt");
    /// The row taken: the dialog's title over `Checking for a reset…`.
    pub const CHECKING: &str = include_str!("fixtures/claude-2.1.304-reset-checking.txt");
    /// The weekly reset's confirmation as it opens, the cursor on `Yes, use my weekly reset`.
    pub const CONFIRM: &str = include_str!("fixtures/claude-2.1.304-reset-confirm.txt");
    /// The 5-hour reset's confirmation as it opens.
    pub const CONFIRM_SESSION: &str =
        include_str!("fixtures/claude-2.1.304-reset-confirm-session.txt");
    /// The full reset's confirmation as it opens.
    pub const CONFIRM_FULL: &str = include_str!("fixtures/claude-2.1.304-reset-confirm-full.txt");
    /// The Fable reset's confirmation, its body's `returns …` segment drawn.
    pub const CONFIRM_FABLE: &str = include_str!("fixtures/claude-2.1.304-reset-confirm-fable.txt");
    /// The weekly reset's confirmation on a 60-column grid, its body wrapped.
    pub const CONFIRM_60: &str = include_str!("fixtures/claude-2.1.304-reset-confirm-60col.txt");
    /// The weekly reset's confirmation of a reset used early.
    pub const CONFIRM_EARLY: &str = include_str!("fixtures/claude-2.1.304-reset-confirm-early.txt");
    /// The weekly reset's confirmation with the cursor moved onto `No, keep it`.
    pub const CONFIRM_FOCUS_NO: &str =
        include_str!("fixtures/claude-2.1.304-reset-confirm-focus-no.txt");
    /// The weekly reset's yes taken.
    pub const RESETTING: &str = include_str!("fixtures/claude-2.1.304-reset-resetting.txt");

    /// Every fixture of the reset program, with its name.
    pub const ALL: [(&str, &str); 14] = [
        ("dialog-grant", DIALOG_GRANT),
        ("dialog-kinds", DIALOG_KINDS),
        ("dialog-fable", DIALOG_FABLE),
        ("dialog-weekly", DIALOG_WEEKLY),
        ("dialog-bare", DIALOG_BARE),
        ("checking", CHECKING),
        ("confirm", CONFIRM),
        ("confirm-session", CONFIRM_SESSION),
        ("confirm-full", CONFIRM_FULL),
        ("confirm-fable", CONFIRM_FABLE),
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
    const GRANT: &str = "Use a weekly reset (2 left, until Oct 5)";
    const SESSION: &str = "Use a 5-hour reset (2 left, until Oct 5)";
    const FULL: &str = "Use a full reset (1 left, until Oct 5)";
    const FABLE: &str = "Use a Fable reset (1 left, until Oct 5)";
    const FABLE_AT_WEEK: &str =
        "Use a Fable reset (1 left, until Oct 5) · frees up only what Fable used this week";
    const WEEKLY: &str = "Reset your session limit now";
    const BODY: &str = "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am";
    const DETAIL: &str = "2 weekly resets left · use by Oct 5 at 3pm";
    const ASKS: &str = " · send a message to continue";
    /// The 2.1.287–2.1.303 wording, which 2.1.304 no longer draws.
    const OLD_GRANT: &str = "Use your reset now (2 left, until Oct 5)";
    const OLD_DETAIL: &str = "2 resets left · use by Oct 5 at 3pm";

    fn boxed(rows: &[String]) -> PromptV2 {
        parse_prompt_v2(rows).unwrap_or_else(|| panic!("a box: {rows:#?}"))
    }

    /// The grant row of the dialog whose options are `labels` (the
    /// fullscreen panel's frame, the cursor on the first).
    fn row_among(labels: &[&str]) -> Option<ResetRow> {
        reset_row(&boxed(&limit_options_panel(labels, 0, false)), &[])
    }

    /// MEASURED on 2.1.301 and 2.1.304 in the lab (crates/aterm-lab,
    /// 2026-10-15 and 2026-10-19): the usage-limit dialog at a five-hour wall
    /// with no reset on offer (2.1.301's at-wall reads of the usage body
    /// failed, both reset programs flagged on; 2.1.304's stub account holds
    /// none) has no grant row, and no option whose words spend one.
    #[test]
    fn the_measured_dialog_with_no_reset_on_offer_has_no_grant_row() {
        use crate::prompt::fixtures::{
            LIMIT_DIALOG_NO_RESET_ROWS_2_1_301, LIMIT_DIALOG_NO_RESET_ROWS_2_1_304,
        };
        for (fixture, options) in [
            (LIMIT_DIALOG_NO_RESET_ROWS_2_1_301, 4),
            (LIMIT_DIALOG_NO_RESET_ROWS_2_1_304, 3),
        ] {
            let dialog = boxed(&screen(fixture));
            assert_eq!(reset_row(&dialog, &[]), None);
            assert_eq!(dialog.options.len(), options, "{:?}", dialog.options);
            assert!(dialog.options.iter().all(|o| !spends_reset(&o.label)));
        }
        let measured = screen(LIMIT_DIALOG_NO_RESET_ROWS_2_1_304);
        let stop = measured
            .iter()
            .position(|row| row.trim_start().starts_with("❯ 1. Stop and wait"))
            .expect("the stop row");
        let offered = |rows: &[&str]| -> Vec<String> {
            let mut out = measured[..=stop].to_vec();
            out.extend(
                rows.iter()
                    .enumerate()
                    .map(|(k, label)| format!("    {}. {label}", k + 2)),
            );
            let n = rows.len();
            out.extend(measured[stop + 1..].iter().map(|row| {
                row.replacen("    2. Wait", &format!("    {}. Wait", 2 + n), 1)
                    .replacen("    3. Upgrade", &format!("    {}. Upgrade", 3 + n), 1)
            }));
            out
        };
        for (rows, index, label, kind, left, day) in [
            (
                &["Use a full reset (2 left, until Oct 5)"][..],
                1,
                "Use a full reset (2 left, until Oct 5)",
                ResetKind::Full,
                2,
                "Oct 5",
            ),
            (&[SESSION], 1, SESSION, ResetKind::Session, 2, "Oct 5"),
            (
                &["Use a weekly reset (1 left, until Jan 2, 2027)"],
                1,
                "Use a weekly reset (1 left, until Jan 2, 2027)",
                ResetKind::Weekly,
                1,
                "Jan 2, 2027",
            ),
        ] {
            let dialog = boxed(&offered(rows));
            assert_eq!(dialog.options.len(), 3 + rows.len(), "{:?}", dialog.options);
            let row =
                reset_row(&dialog, &[]).unwrap_or_else(|| panic!("the offered row: {rows:?}"));
            assert_eq!(
                (
                    row.index,
                    row.label.as_str(),
                    row.kind,
                    row.left,
                    row.day.as_str()
                ),
                (index, label, kind, left, day),
                "{rows:?}"
            );
        }
        for rows in [
            &[SESSION, "Use a full reset (2 left, until Oct 5)"][..],
            &[
                FABLE_AT_WEEK,
                "Use a weekly reset (1 left, until Jan 2, 2027)",
            ],
            &[FABLE_AT_WEEK],
        ] {
            let dialog = boxed(&offered(rows));
            assert_eq!(dialog.options.len(), 3 + rows.len(), "{:?}", dialog.options);
            assert_eq!(reset_row(&dialog, &[]), None, "{rows:?}");
        }
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

    /// [`fixtures::CONFIRM`] with its title, its body and its two options
    /// replaced (the cursor on the first).
    fn confirm_of(title: &str, body: &[&str], yes: &str, no: &str) -> Vec<String> {
        let base = screen(fixtures::CONFIRM);
        let mut out = base[..3].to_vec();
        out.push(format!("   {title}"));
        out.push(String::new());
        out.extend(body.iter().map(|row| format!("   {row}")));
        out.push(String::new());
        out.push(format!("   ❯ {yes}"));
        out.push(format!("     {no}"));
        out.extend(base[10..].iter().cloned());
        out
    }

    fn confirm_read(rows: &[String]) -> ConfirmRead {
        reset_confirm(&boxed(rows), rows)
    }

    /// `rows`, a screen of the fullscreen renderer's panel, as the inline renderer draws
    /// the same Dialog (measured on 2.1.295, [`LIMIT_OPTIONS_DIALOG`]): a `─` rule and the
    /// box two columns in.
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

    /// THE KINDS' WORDS, as the bundle's `X()`, `ce()` and `Mt()` draw them,
    /// and each read back from its id.
    #[test]
    fn every_kind_is_drawn_by_its_own_words() {
        for (kind, word, name, capital, done, title, yes) in [
            (
                ResetKind::Session,
                "session",
                "5-hour reset",
                "5-hour reset",
                "Session limit reset",
                "Use a 5-hour reset?",
                "Yes, use my 5-hour reset",
            ),
            (
                ResetKind::Weekly,
                "weekly",
                "weekly reset",
                "Weekly reset",
                "Weekly limits reset",
                "Use a weekly reset?",
                "Yes, use my weekly reset",
            ),
            (
                ResetKind::Fable,
                "fable",
                "Fable reset",
                "Fable reset",
                "Fable reset used · what Fable used is back in your weekly limit",
                "Use a Fable reset?",
                "Yes, use my Fable reset",
            ),
            (
                ResetKind::Full,
                "full",
                "full reset",
                "Full reset",
                "Limits reset",
                "Use a full reset?",
                "Yes, use my full reset",
            ),
        ] {
            assert_eq!(
                (
                    kind.word(),
                    kind.name(),
                    kind.capital().as_str(),
                    kind.done()
                ),
                (word, name, capital, done)
            );
            assert_eq!((kind.title(), kind.yes()), (title.into(), yes.into()));
            assert_eq!(ResetKind::of_word(word), Some(kind));
            assert_eq!(reset_title(title), Some(kind));
            assert_eq!(reset_title(&format!("   {title}  ")), Some(kind));
            assert_eq!(kind.takeable(), kind != ResetKind::Fable);
        }
        for (kind, none, one, many) in [
            (
                ResetKind::Session,
                "no five-hour resets",
                "1 five-hour reset",
                "12 five-hour resets",
            ),
            (
                ResetKind::Weekly,
                "no weekly resets",
                "1 weekly reset",
                "12 weekly resets",
            ),
            (
                ResetKind::Fable,
                "no Fable resets",
                "1 Fable reset",
                "12 Fable resets",
            ),
            (
                ResetKind::Full,
                "no full resets",
                "1 full reset",
                "12 full resets",
            ),
        ] {
            assert_eq!(
                [none, one, many].map(|t| kind.resets_of(t)),
                [Some(0), Some(1), Some(12)],
                "{kind:?}"
            );
            assert_eq!(
                [0, 1, 12].map(|n| kind.resets(n)),
                [none, one, many].map(str::to_owned),
                "{kind:?}: the record's words are the vendor's"
            );
        }
        for wrong in [
            "1 resets",
            "2 resets",
            "no resets",
            "1 reset",
            "0 weekly resets",
            "1 weekly resets",
            "2 weekly reset",
            "02 weekly resets",
            "two weekly resets",
            "2 weekly resets left",
        ] {
            assert_eq!(ResetKind::Weekly.resets_of(wrong), None, "{wrong}");
        }
        assert_eq!(ResetKind::of_word("5-hour"), None);
        assert_eq!(ResetKind::Session.resets_of("2 5-hour resets"), None);
        assert_eq!(ResetKind::Weekly.resets_of("2 five-hour resets"), None);
        for title in [
            "Use your reset?",
            "Use a reset?",
            "Use a weekly reset",
            "Use a hourly reset?",
        ] {
            assert_eq!(reset_title(title), None, "{title}");
        }
    }

    /// THE GRANT ROW, by its exact label: the composed dialog gives its
    /// place, its kind, its count and its day, and the label a press fences
    /// on; found wherever it stands, in both frames, with a year in its day,
    /// for each kind the step takes.
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
        assert_eq!(p.options[1].role, Role::Other);
        assert_eq!(
            reset_row(&p, &[]),
            Some(ResetRow {
                index: 1,
                label: GRANT.to_string(),
                kind: ResetKind::Weekly,
                left: 2,
                day: "Oct 5".to_string(),
            })
        );
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
        assert_eq!(reset_row(&focused, &[]).map(|r| r.index), Some(1));
        let inline_frame = boxed(&limit_options_dialog(&[STOP, GRANT, WAIT, CREDITS], 0));
        assert_eq!(reset_row(&inline_frame, &[]).map(|r| r.left), Some(2));
        for (label, kind, left, day) in [
            (SESSION, ResetKind::Session, 2, "Oct 5"),
            (FULL, ResetKind::Full, 1, "Oct 5"),
            (
                "Use a weekly reset (1 left, until Dec 31)",
                ResetKind::Weekly,
                1,
                "Dec 31",
            ),
            (
                "Use a 5-hour reset (12 left, until Jan 2, 2027)",
                ResetKind::Session,
                12,
                "Jan 2, 2027",
            ),
        ] {
            let read = row_among(&[STOP, label, WAIT]).expect(label);
            assert_eq!(
                (read.kind, read.left, read.day.as_str(), read.label.as_str()),
                (kind, left, day, label)
            );
        }

        assert_eq!(
            reset_row(&boxed(&screen(fixtures::DIALOG_WEEKLY)), &[]),
            None
        );
        assert_eq!(reset_row(&boxed(&screen(fixtures::DIALOG_BARE)), &[]), None);
        assert_eq!(
            row_among(&[STOP, WEEKLY, GRANT, WAIT]),
            None,
            "both rows in one menu"
        );
        assert_eq!(
            row_among(&[STOP, GRANT, WAIT, WEEKLY]),
            None,
            "the weekly session reset's row anywhere"
        );
        assert_eq!(row_among(&[STOP, WAIT, CREDITS]), None, "no grant row");
        assert_eq!(
            row_among(&[STOP, GRANT, GRANT, WAIT]),
            None,
            "one kind drawn twice"
        );
        assert_eq!(
            row_among(&[
                STOP,
                SESSION,
                GRANT,
                "Use a 5-hour reset (1 left, until Oct 9)",
                WAIT
            ]),
            None,
            "one kind drawn twice, the second unread"
        );
        assert_eq!(
            row_among(&[STOP, FABLE, GRANT, FABLE_AT_WEEK, WAIT]),
            None,
            "the Fable kind drawn twice"
        );
        for label in [
            "Use a weekly reset (0 left, until Oct 5)",
            "Use a weekly reset (2 left)",
            "Use a weekly reset (until Oct 5)",
            "Use a weekly reset (two left, until Oct 5)",
            "Use a weekly reset (02 left, until Oct 5)",
            "Use a weekly reset (-2 left, until Oct 5)",
            "Use a weekly reset (2 left, until tomorrow)",
            "Use a weekly reset (2 left, until Oct 5 at 3pm)",
            "Use a weekly reset (2 left, until Oct 32)",
            "Use a weekly reset (2 left, until Oct 5, 27)",
            "Use a weekly reset (2 left, until Oct 5 (UTC))",
            "Use a weekly reset (2 left, until Oct 5) and continue",
            "Use a weekly reset (2 left, until Oct 5) · frees up only what Fable used this week",
            "Use a weekly reset — 2 left, until Oct 5",
            "use a weekly reset (2 left, until Oct 5)",
            "Use a weekly resets (2 left, until Oct 5)",
            "Use the weekly reset (2 left, until Oct 5)",
            "Use a reset now (2 left, until Oct 5)",
            OLD_GRANT,
            "Use your reset now",
        ] {
            assert_eq!(row_among(&[STOP, label, WAIT, CREDITS]), None, "{label}");
        }
        let mut retitled = rows.clone();
        retitled[3] = "   What would you like?".to_string();
        assert_ne!(boxed(&retitled).kind, PromptKind::UsageLimit);
        assert_eq!(reset_row(&boxed(&retitled), &[]), None);
        for edit in [
            (|p: &mut PromptV2| p.options[0].n = None) as fn(&mut PromptV2),
            |p| p.options[2].n = Some(4),
            |p| p.options[1].focused = true,
            |p| p.head_off_screen = true,
            |p| p.kind = PromptKind::Other,
        ] {
            let mut edited = p.clone();
            edit(&mut edited);
            assert_eq!(reset_row(&edited, &[]), None, "{edited:?}");
        }
    }

    /// ONLY THE ONE RESET ON OFFER (the spec's R2): a row of a kind the step
    /// takes is taken where it is the one option of the dialog that spends a
    /// reset — which of several to spend is the owner's choice — and a Fable
    /// row never, even alone.
    #[test]
    fn a_row_is_taken_only_as_the_one_reset_on_offer_and_never_a_fable_row() {
        const UNKNOWN: &str = "Use a scheduled full weekly reset (1 left, until Oct 7)";
        assert!(uses_a_reset(UNKNOWN), "an unknown kind still spends");
        let kinds = boxed(&screen(fixtures::DIALOG_KINDS));
        assert_eq!(kinds.options.len(), 6, "{:?}", kinds.options);
        assert_eq!(reset_row(&kinds, &[]), None, "three kinds on offer");
        let fable = boxed(&screen(fixtures::DIALOG_FABLE));
        assert_eq!(fable.options[1].label, FABLE);
        assert_eq!(
            reset_row(&fable, &[]),
            None,
            "a weekly reset beside a Fable one"
        );
        for labels in [
            &[STOP, GRANT, SESSION, WAIT][..],
            &[STOP, FULL, GRANT, WAIT],
            &[STOP, SESSION, FULL, WAIT],
            &[STOP, FABLE, FULL, WAIT],
            &[FABLE, STOP, WAIT, SESSION],
            &[STOP, GRANT, FABLE_AT_WEEK, WAIT],
            &[STOP, FABLE_AT_WEEK, GRANT, WAIT],
            &[STOP, "Use a Fable reset", GRANT, WAIT],
            &[STOP, FABLE, "Use a weekly reset (1 left)", FULL, WAIT],
            &[STOP, "Use a 5-hour reset", GRANT, WAIT],
            &[STOP, UNKNOWN, FULL, WAIT],
            &[STOP, FULL, UNKNOWN, WAIT],
            &[STOP, GRANT, OLD_GRANT, WAIT],
            &[
                STOP,
                SESSION,
                "Use a full reset plus (1 left, until Oct 5)",
                WAIT,
            ],
            &[
                STOP,
                "use a 5-hour resetting (2 left, until Oct 5)",
                "Use a weekly reset (1 left, until Oct 9)",
                WAIT,
            ],
        ] {
            assert_eq!(row_among(labels), None, "several on offer: {labels:?}");
            for (index, label) in labels.iter().enumerate() {
                let alone: Vec<&str> = labels
                    .iter()
                    .enumerate()
                    .filter(|(k, l)| *k == index || !spends_reset(l) && !uses_a_reset(l))
                    .map(|(_, l)| *l)
                    .collect();
                if let Some(kind) = grant_kind(label).filter(|k| k.takeable())
                    && grant_of(label, kind).is_some()
                {
                    assert_eq!(
                        row_among(&alone).map(|r| (r.kind, r.label)),
                        Some((kind, (*label).to_string())),
                        "{alone:?}"
                    );
                }
            }
        }
        for label in [
            FABLE,
            FABLE_AT_WEEK,
            UNKNOWN,
            OLD_GRANT,
            "Use a full reset plus (1 left, until Oct 5)",
            "use a 5-hour resetting (2 left, until Oct 5)",
            "use a 5-hour reset (2 left, until Oct 5)",
            "Use a 5-hour and weekly reset (1 left, until Oct 7)",
        ] {
            assert_eq!(row_among(&[STOP, label, WAIT]), None, "{label}");
        }
    }

    /// THE OWNER'S ORDER (`[harness] limit_reset_kinds`, the spec's R2):
    /// under a list, the row taken is the first listed kind's the dialog
    /// offers, whatever else is on offer — the three kinds of
    /// `DIALOG_KINDS`, a weekly reset beside a Fable one — and an unlisted
    /// kind or a Fable row never.
    #[test]
    fn under_the_owners_order_the_first_listed_kind_on_offer_is_taken() {
        use ResetKind::{Fable, Full, Session, Weekly};
        let kinds = boxed(&screen(fixtures::DIALOG_KINDS));
        let fable = boxed(&screen(fixtures::DIALOG_FABLE));
        let taken = |p: &PromptV2, order: &[ResetKind]| {
            reset_row(p, order).map(|row| (row.index, row.kind, row.left, row.day))
        };
        assert_eq!(taken(&kinds, &[]), None, "the lone-offer rule");
        assert_eq!(taken(&fable, &[]), None, "the lone-offer rule");
        let at = |p: &PromptV2, label: &str| {
            p.options
                .iter()
                .position(|o| o.label == label)
                .unwrap_or_else(|| panic!("{label}: {:?}", p.options))
        };
        for (order, label, kind, left, day) in [
            (&[Full, Weekly, Session][..], FULL, Full, 1, "Oct 5"),
            (
                &[Weekly, Full],
                "Use a weekly reset (1 left, until Oct 9)",
                Weekly,
                1,
                "Oct 9",
            ),
            (&[Session], SESSION, Session, 2, "Oct 5"),
            (&[Fable, Session, Full], SESSION, Session, 2, "Oct 5"),
        ] {
            assert_eq!(
                taken(&kinds, order),
                Some((at(&kinds, label), kind, left, day.to_string())),
                "{order:?}"
            );
        }
        let weekly = "Use a weekly reset (1 left, until Oct 9)";
        assert_eq!(
            taken(&fable, &[Weekly]),
            Some((at(&fable, weekly), Weekly, 1, "Oct 9".to_string())),
            "a weekly reset beside a Fable one"
        );
        for order in [&[Full][..], &[Session, Full], &[Fable]] {
            assert_eq!(
                taken(&fable, order),
                None,
                "nothing listed on offer: {order:?}"
            );
        }
        let among = |labels: &[&str], order: &[ResetKind]| {
            reset_row(&boxed(&limit_options_panel(labels, 0, false)), order)
                .map(|row| (row.kind, row.label))
        };
        for first in ["Use a full reset", "Use a full reset (0 left, until Oct 5)"] {
            assert_eq!(
                among(&[STOP, first, GRANT, WAIT], &[Full, Weekly]),
                None,
                "{first}"
            );
            assert_eq!(
                among(&[STOP, first, GRANT, WAIT], &[Weekly, Full]),
                Some((Weekly, GRANT.to_string())),
                "{first}: the first listed kind is the weekly one"
            );
        }
        for labels in [
            &[STOP, GRANT, GRANT, WAIT][..],
            &[
                STOP,
                GRANT,
                "Use a weekly reset (1 left, until Oct 9)",
                WAIT,
            ],
            &[
                STOP,
                FULL,
                "Use a scheduled full weekly reset (1 left, until Oct 7)",
                WAIT,
            ],
            &[STOP, FULL, OLD_GRANT, WAIT],
            &[STOP, FULL, WEEKLY, WAIT],
        ] {
            for order in [&[Full, Weekly, Session][..], &[Weekly], &[Full]] {
                assert_eq!(among(labels, order), None, "{labels:?} under {order:?}");
            }
        }
        assert_eq!(among(&[STOP, FABLE, WAIT], &[Fable, Full]), None);
        assert_eq!(
            among(&[STOP, FABLE, FULL, WAIT], &[Fable, Full]),
            Some((Full, FULL.to_string()))
        );
    }

    /// WHICH KIND A ROW OPENS AS: a kind's row head, ASCII case ignored,
    /// that ends where the label does or at its ` (` or ` · ` — the head a
    /// word of its own, as the vendor's `Kvt` draws it (a bare head where
    /// the grant names no end date; the Fable row's suffix after its
    /// parentheses).
    #[test]
    fn a_row_opens_as_a_kind_only_where_the_kinds_head_is_a_word_of_its_own() {
        for (label, kind) in [
            (SESSION, ResetKind::Session),
            (GRANT, ResetKind::Weekly),
            (FULL, ResetKind::Full),
            (FABLE, ResetKind::Fable),
            (FABLE_AT_WEEK, ResetKind::Fable),
            ("Use a weekly reset", ResetKind::Weekly),
            (
                "Use a Fable reset · frees up only what Fable used this week",
                ResetKind::Fable,
            ),
            (
                "use a 5-hour reset (2 left, until Oct 5)",
                ResetKind::Session,
            ),
            ("USE A FULL RESET (1 left, until Oct 5)", ResetKind::Full),
        ] {
            assert_eq!(grant_kind(label), Some(kind), "{label}");
        }
        for label in [
            "Use a full reset plus (1 left, until Oct 5)",
            "Use a full resets (1 left, until Oct 5)",
            "use a 5-hour resetting (2 left, until Oct 5)",
            "Use a weekly reset— 1 left",
            "Use a weekly",
            OLD_GRANT,
            WEEKLY,
            STOP,
            WAIT,
            "",
        ] {
            assert_eq!(grant_kind(label), None, "{label}");
        }
    }

    /// WHAT SPENDS A RESET, by a label's opening words: a grant row of any
    /// kind in any spelling of its head (the Fable one too), the weekly row,
    /// the confirmation's yes of any kind.
    #[test]
    fn a_label_that_spends_a_reset_is_told_by_its_opening_words() {
        for label in [
            GRANT,
            SESSION,
            FULL,
            FABLE,
            FABLE_AT_WEEK,
            "Use a weekly reset",
            "Use a 5-hour reset (…)",
            "use a full reset — 1 left",
            "  Use a weekly reset (2 left, until Oct 5)",
            WEEKLY,
            "Reset your session limit now (1/week)",
            "Yes, use my 5-hour reset",
            "Yes, use my weekly reset",
            "Yes, use my Fable reset",
            "YES, USE MY FULL RESET",
            "Yes, use my weekly reset now",
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
            "Yes, use my browser",
            "Yes, use my enabled plugins in cloud sessions",
            "Yes, use my",
            "Use a",
            "Use a different model",
            "Reset used · no resets left",
            "Reset the view",
            "Use your",
            "Use a fresh session and reset nothing later",
            "Use a new branch, then reset it",
            "Use my settings to reset the view",
            "Don’t continue automatically",
            "",
            OLD_GRANT,
            "Yes, use my reset",
        ] {
            assert!(!spends_reset(label), "{label}");
        }
        for label in [
            GRANT,
            SESSION,
            FULL,
            "Yes, use my weekly reset now",
            "Use a scheduled full weekly reset (1 left, until Oct 7)",
            "Use an unlisted reset, 1 left",
            "Use a 5-hour and weekly reset (1 left, until Oct 7)",
            "Use a reset credit (1 left)",
            OLD_GRANT,
            "Yes, use my reset",
        ] {
            assert!(uses_a_reset(label), "{label}");
        }
        for label in [
            "Use a fresh session and reset nothing later",
            "Use a fresh session (reset nothing later)",
            "Use a fresh session but reset nothing",
            "Use a new branch, then reset it",
            "Use my settings, then reset them",
            "Use my settings to reset the view",
            "Use a new session to reset the view",
            "Use a different model",
        ] {
            assert!(!uses_a_reset(label), "{label}");
        }
    }

    /// THE CONFIRMATION, READ EXACTLY: the composed weekly box gives its two
    /// fenced rows, its kind, the limit it refills, the week, the count and
    /// the deadline; the same at 60 columns, its body wrapped onto a second
    /// row; the same in the inline frame; with the cursor on `No, keep it` it
    /// is still exact and says the yes is not focused; and the 5-hour and
    /// full boxes, each by its own words (`5-hour` in the title, `five-hour`
    /// in the count).
    #[test]
    fn the_confirmation_reads_exactly_plain_wrapped_and_with_the_focus_on_no() {
        let rows = screen(fixtures::CONFIRM);
        assert_eq!(rows, confirm_with(&[BODY, DETAIL]));
        let p = boxed(&rows);
        assert_eq!((p.kind, p.select), (PromptKind::Other, Select::ArrowsEnter));
        assert_eq!(
            p.options.iter().map(|o| o.role).collect::<Vec<_>>(),
            [Role::Other, Role::Deny]
        );
        let exact = ResetConfirm {
            title_row: 3,
            yes_row: 8,
            yes_focused: true,
            kind: ResetKind::Weekly,
            limits: vec!["weekly".to_string()],
            week: "Oct 9 at 5am".to_string(),
            left: 2,
            deadline: "Oct 5 at 3pm".to_string(),
            head: format!("{BODY} {DETAIL}"),
        };
        assert_eq!(reset_confirm(&p, &rows), ConfirmRead::Exact(exact.clone()));
        assert_eq!(rows[exact.title_row].trim(), "Use a weekly reset?");
        assert_eq!(rows[exact.yes_row].trim(), "❯ Yes, use my weekly reset");

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
        for (fixture, kind, limits, left) in [
            (
                fixtures::CONFIRM_SESSION,
                ResetKind::Session,
                &["session"][..],
                2,
            ),
            (
                fixtures::CONFIRM_FULL,
                ResetKind::Full,
                &["session", "weekly"],
                1,
            ),
        ] {
            let ConfirmRead::Exact(read) = confirm_read(&screen(fixture)) else {
                panic!("{kind:?}: {:?}", confirm_read(&screen(fixture)));
            };
            assert_eq!(
                (
                    read.kind,
                    read.limits.iter().map(String::as_str).collect::<Vec<_>>(),
                    read.left,
                    read.week.as_str(),
                    read.deadline.as_str(),
                    (read.title_row, read.yes_row, read.yes_focused)
                ),
                (
                    kind,
                    limits.to_vec(),
                    left,
                    "Oct 9 at 5am",
                    "Oct 5 at 3pm",
                    (3, 8, true)
                )
            );
        }
        for (body, detail, limits, week, left, deadline) in [
            (
                "Refills your session limit now · your weekly reset day stays Oct 9 at 5:30am",
                "1 weekly reset left · use by Oct 5 at 12pm",
                &["session"][..],
                "Oct 9 at 5:30am",
                1,
                "Oct 5 at 12pm",
            ),
            (
                "Refills your session and weekly limits now · your weekly reset day stays Jan 2, \
                 2027 at 9am",
                "12 weekly resets left · use by Dec 31 at 11:59pm",
                &["session", "weekly"],
                "Jan 2, 2027 at 9am",
                12,
                "Dec 31 at 11:59pm",
            ),
            (
                "Refills your session, weekly and Opus limits now · your weekly reset day stays \
                 Oct 9 at 5am",
                "2 weekly resets left · use by Jan 1, 2027 at 1am",
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
    /// BACK: the vendor's own `{limits}` for each type (`Yn` @244635663: the
    /// five-hour type `session`, the week `weekly`, `Opus`, `Sonnet`, the
    /// overage-included week `weekly` for a grant that does not give Fable's
    /// usage back), drawn in the body, reads back as exactly the name
    /// [`reset_limit_name`] answers — what a go's limits are matched against,
    /// case and spelling alike — and the weekly types' name is the one the
    /// composed confirmation reads.
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

        assert_eq!(read_back("Weekly limit"), ["Weekly"]);
        assert_ne!(read_back("Weekly limit"), read_back("weekly limit"));
    }

    /// ANYTHING ELSE IS INEXACT, and says the first thing that did not read
    /// — one control for each word, each the exact confirmation with one
    /// thing changed.
    #[test]
    fn a_confirmation_not_read_exactly_names_what_did_not_read() {
        const EARLY: &str =
            "You still have 40% of your weekly limit left — use your weekly reset anyway?";
        for (why, body) in [
            ("early", &[EARLY, BODY, DETAIL][..]),
            (
                "early",
                &[
                    "You haven't reached a limit yet — use your weekly reset anyway?",
                    BODY,
                    DETAIL,
                ],
            ),
            (
                "early",
                &[
                    "You haven't reached a limit yet — use your 5-hour reset anyway?",
                    BODY,
                    DETAIL,
                ],
            ),
            (
                "early",
                &[
                    "You still have 40% of your weekly limit left — use your weekly",
                    "reset anyway?",
                    BODY,
                    DETAIL,
                ],
            ),
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
            (
                "week",
                &[
                    "Refills your weekly limit now · returns what Fable used to your weekly limit · \
                     your weekly reset day stays Oct 9 at 5am",
                    DETAIL,
                ],
            ),
            (
                "week",
                &[
                    "Refills your weekly limit now · your weekly reset day stays Oct 9 at 5am (UTC)",
                    DETAIL,
                ],
            ),
            ("count", &[BODY, "weekly resets left · use by Oct 5 at 3pm"]),
            (
                "count",
                &[BODY, "no weekly resets left · use by Oct 5 at 3pm"],
            ),
            (
                "count",
                &[BODY, "0 weekly resets left · use by Oct 5 at 3pm"],
            ),
            (
                "count",
                &[BODY, "1 weekly resets left · use by Oct 5 at 3pm"],
            ),
            (
                "count",
                &[BODY, "2 weekly reset left · use by Oct 5 at 3pm"],
            ),
            (
                "count",
                &[BODY, "2 five-hour resets left · use by Oct 5 at 3pm"],
            ),
            ("count", &[BODY, "2 full resets left · use by Oct 5 at 3pm"]),
            ("count", &[BODY, OLD_DETAIL]),
            ("count", &[BODY, "… left · use by …"]),
            ("count", &[BODY, "use by Oct 5 at 3pm"]),
            ("count", &[BODY]),
            ("deadline", &[BODY, "2 weekly resets left · use by Oct 5"]),
            ("deadline", &[BODY, "2 weekly resets left"]),
            (
                "deadline",
                &[BODY, "2 weekly resets left · expires Oct 5 at 3pm"],
            ),
            (
                "deadline",
                &[BODY, "2 weekly resets left · use by tomorrow at 3pm"],
            ),
            (
                "deadline",
                &[BODY, "2 weekly resets left · use by Oct 5 at 3 PM"],
            ),
            (
                "deadline",
                &[BODY, "2 weekly resets left · use by Oct 5 at 3pm (UTC)"],
            ),
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
        assert_eq!(
            confirm_read(&confirm_of(
                "Use your reset?",
                &[BODY, OLD_DETAIL],
                "Yes, use my reset",
                "No, keep it",
            )),
            ConfirmRead::Inexact("title")
        );
        assert_eq!(
            confirm_read(&screen(fixtures::CONFIRM_FABLE)),
            ConfirmRead::Inexact("kind")
        );

        let exact = screen(fixtures::CONFIRM);
        let options = |yes: &str, no: &str, more: &[&str]| {
            let mut rows = exact[..8].to_vec();
            rows.push(format!("   {yes}"));
            rows.push(format!("   {no}"));
            rows.extend(more.iter().map(|row| format!("   {row}")));
            rows.extend(exact[10..].iter().cloned());
            rows
        };
        assert_eq!(
            options("❯ Yes, use my weekly reset", "  No, keep it", &[]),
            exact
        );
        for rows in [
            options("❯ 1. Yes, use my weekly reset", "  2. No, keep it", &[]),
            options(
                "❯ Yes, use my weekly reset",
                "  No, keep it",
                &["  Ask me later"],
            ),
            options("  No, keep it", "❯ Yes, use my weekly reset", &[]),
            options("❯ Yes, use my weekly reset", "  No", &[]),
            options("❯ Yes, use my weekly reset now", "  No, keep it", &[]),
            options(
                "❯ Yes, use my weekly reset",
                "  Yes, use my weekly reset",
                &[],
            ),
            options("❯ Yes, use my 5-hour reset", "  No, keep it", &[]),
            options("❯ Yes, use my full reset", "  No, keep it", &[]),
            options("❯ Yes, use my reset", "  No, keep it", &[]),
        ] {
            assert_eq!(
                confirm_read(&rows),
                ConfirmRead::Inexact("options"),
                "{rows:#?}"
            );
        }
        let p = boxed(&exact);
        for focused in [(true, true), (false, false)] {
            let mut edited = p.clone();
            (edited.options[0].focused, edited.options[1].focused) = focused;
            assert_eq!(
                reset_confirm(&edited, &exact),
                ConfirmRead::Inexact("options")
            );
        }

        for title in [
            "   Spend your reset?",
            "   Use your reset?",
            "   Use a weekly reset",
        ] {
            let mut retitled = exact.clone();
            retitled[3] = title.to_string();
            assert_eq!(
                confirm_read(&retitled),
                ConfirmRead::Inexact("title"),
                "{title}"
            );
            assert_eq!(reset_confirm(&p, &retitled), ConfirmRead::Inexact("title"));
        }
        let mut cut = p.clone();
        cut.head_off_screen = true;
        assert_eq!(reset_confirm(&cut, &exact), ConfirmRead::Inexact("title"));
        assert_eq!(
            reset_confirm(&p, &exact[..3]),
            ConfirmRead::Inexact("title")
        );
    }

    /// A CONFIRMATION WITH NEITHER ITS TITLE NOR ITS YES IS STILL THE
    /// CONFIRMATION, by any one part left of it: the server may reword every
    /// text (READ: `mH()`), and a short pane cuts the title off.
    #[test]
    fn a_confirmation_is_told_by_any_part_left_of_it() {
        const QUESTION: &str = "Spend it now?";
        const PLAIN: &str = "This takes effect at once";
        let exact = screen(fixtures::CONFIRM);
        let boxed_as = |title: &str, body: &[&str], yes: &str, no: &str, cut: bool| {
            let rows = confirm_of(title, body, yes, no);
            if cut { rows[5..].to_vec() } else { rows }
        };
        assert_eq!(
            boxed_as(
                "Use a weekly reset?",
                &[BODY, DETAIL],
                "Yes, use my weekly reset",
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
                    &["2 weekly resets left · use by Oct 5 at 3pm"][..],
                    "No",
                ),
                (
                    "the early line",
                    &["You haven't reached a limit yet — use your weekly reset anyway?"][..],
                    "No",
                ),
                (
                    "another kind's early line",
                    &["You haven't reached a limit yet — use your full reset anyway?"][..],
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
                let rows = boxed_as(QUESTION, body, "Yes", no, cut);
                assert_eq!(
                    confirm_read(&rows),
                    ConfirmRead::Inexact("title"),
                    "{what}, cut {cut}, a plain yes"
                );
            }
            let rows = boxed_as(QUESTION, &[PLAIN], "Yes, use my 5-hour reset", "No", cut);
            assert_eq!(
                confirm_read(&rows),
                ConfirmRead::Inexact("title"),
                "cut {cut}"
            );
            let other = boxed_as(QUESTION, &[PLAIN], "Yes", "No", cut);
            assert_eq!(confirm_read(&other), ConfirmRead::Not, "cut {cut}");
        }
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
        for dialog in [
            fixtures::DIALOG_GRANT,
            fixtures::DIALOG_KINDS,
            fixtures::DIALOG_FABLE,
            fixtures::DIALOG_WEEKLY,
        ] {
            assert_eq!(confirm_read(&screen(dialog)), ConfirmRead::Not);
        }
    }

    /// THE CHOOSER `Which reset do you want to use?` (READ, `Fnt`
    /// @257468497: only a typed `/limit-reset` opens it, which the harness
    /// never types): its unnumbered rows are each a kind's `Use a <kind> (…)`
    /// or a disabled `<Kind> · <reason>`, with no keep row.
    #[test]
    fn the_chooser_is_neither_the_dialog_nor_the_confirmation() {
        let base = screen(fixtures::CONFIRM);
        let mut rows = base[..3].to_vec();
        rows.push("   Which reset do you want to use?".to_string());
        rows.push(String::new());
        rows.push(format!("   ❯ {SESSION}"));
        rows.push(format!("     {GRANT}"));
        rows.push("     Full reset · can't be used while you're at your session limit".to_string());
        rows.extend(base[10..].iter().cloned());
        let p = boxed(&rows);
        assert_ne!(p.kind, PromptKind::UsageLimit);
        assert_eq!(p.options.len(), 3, "{:?}", p.options);
        assert_eq!(reset_row(&p, &[]), None);
        assert_eq!(reset_confirm(&p, &rows), ConfirmRead::Not);
        assert!(p.options[..2].iter().all(|o| spends_reset(&o.label)));
        assert_eq!(reset_pending(&rows), None);
        assert_eq!(
            row_among(&[STOP, SESSION, WAIT]).map(|r| r.kind),
            Some(ResetKind::Session)
        );
    }

    /// THE TWO PENDING LINES, each under its own title, with the row the
    /// line is on: `Checking for a reset…` under the wall dialog's (its
    /// input guide under it), `Resetting your limits…` under the
    /// confirmation's of each kind (no guide) — in both frames, and with the
    /// line wrapped.
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
        assert_eq!(resetting[3].trim(), "Use a weekly reset?");
        assert!(
            resetting[5]
                .trim()
                .starts_with("Resetting your limits… this usually")
        );
        for kind in ResetKind::ALL {
            let mut titled = resetting.clone();
            titled[3] = format!("   {}", kind.title());
            assert_eq!(
                reset_pending(&titled),
                Some((ResetPending::Resetting, 5)),
                "{kind:?}"
            );
        }
        assert_eq!(
            reset_pending(&inline(&checking)),
            Some((ResetPending::Checking, 5))
        );
        assert_eq!(
            reset_pending(&inline(&resetting)),
            Some((ResetPending::Resetting, 5))
        );
        assert_eq!(
            reset_pending(&checking[..6]),
            Some((ResetPending::Checking, 5))
        );
        let mut wrapped = resetting[..5].to_vec();
        wrapped.push("   Resetting your limits… this usually takes a few".to_string());
        wrapped.push("   seconds".to_string());
        assert_eq!(reset_pending(&wrapped), Some((ResetPending::Resetting, 5)));
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
        assert_eq!(
            edited(&resetting, &|r| r[3] = "   Use your reset?".to_string()),
            None,
            "the 2.1.287 title"
        );
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

    /// The vendor's other answers at a wall, as its templates draw them (dates and counts
    /// invented): the lines of a claim, then the row's own answers, then the rest of what the
    /// answers' templates compose (2.1.308's are 2.1.304's), then the answers away from a
    /// wall, which a person's own `/limit-reset` draws.
    const FAILURES: [&str; 43] = [
        "That reset was already used · nothing changed just now · 1 weekly reset left",
        "That reset was already used · nothing changed just now · 1 five-hour reset left · run \
         /limit-reset again to use one",
        "This reset isn't available any more · nothing was used · this limit resets Oct 9 at 5am",
        "This reset isn't available any more · your earlier try may have gone through · checking \
         your limits now",
        "A different reset is on offer now · nothing was used · run /limit-reset again to see it",
        "Your weekly reset is no longer on offer · nothing was used · you can use a 5-hour reset",
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
        "No 5-hour reset to use · you can use a weekly reset",
        "Your reset can't be used for this limit right now · this limit resets Oct 9 at 5am",
        "None of your resets refill your session limit · this limit resets 3pm",
        "Your reset doesn't refill your session limit, so it can't be used until that resets · \
         this limit resets Oct 9 at 5am",
        "This limit may have lifted already · checking your limits now · run /limit-reset again \
         in a moment to use your reset anyway",
        "Reset used · no resets left",
        "Couldn't check for a reset right now · try again in a moment",
        "Resets can't be used while you continue at lower priority · run /limit-reset after that \
         ends",
        "Resets kept · run /limit-reset when you want one · first expires Oct 5 at 3pm",
        "No reset to use · this limit resets Oct 9 at 5am",
        "Your Fable reset can't be used right now · no Fable usage this week to give back · this \
         limit resets Oct 9 at 5am",
        "Your reset can't be used for this limit right now · it is paused · this limit resets \
         Oct 9 at 5am",
        "Your reset can't be used for this limit right now · try again in a minute · this limit \
         resets 3pm",
        "This reset isn't available any more · nothing was used · you're not at a usage limit \
         right now",
        "Your weekly reset can't be used while you're at your session limit · you can use a \
         5-hour reset",
        "Your full reset can't be used while you're at your Opus limit · you can use a weekly \
         reset",
        "Your 5-hour reset can't be used while you're at another limit it doesn't refill · you \
         can use a weekly reset or a full reset",
        "Your weekly reset can't be used right now · you can use a 5-hour reset",
        "Your weekly reset can't be used right now · doesn't refill the limit you've reached · \
         you can use a 5-hour reset, a full reset or a Fable reset",
        "Your weekly reset can't be used right now · doesn't refill your session limit · you can \
         use a 5-hour reset",
        "Your full reset can't be used right now · usable once you reach a limit it refills · you \
         can use a 5-hour reset",
        "Your Fable reset can't be used right now · no Fable usage this week to give back · you \
         can use a weekly reset",
        "Your weekly reset can't be used right now · try again in a minute · you can use a 5-hour \
         reset",
        "You have a reset saved for when you reach a usage limit · run /limit-reset when you do · \
         1 reset left · use by Oct 5",
        "Your reset can't be used right now · it is paused · 2 resets left · use by Oct 5",
        "Your reset can't be used right now · try again later · 1 reset left · use by Oct 5",
        "Your reset only works at a usage limit, and you're not at one now · nothing was used",
        "Another reset was just started on your account · this one wasn't used · try again in a \
         minute",
        "Couldn't confirm the reset went through · check /usage in a moment · if your limits \
         weren't refilled, run /limit-reset again",
    ];
    const SPENT: &str =
        "Weekly limits reset · your weekly reset day stays Oct 9 at 5am · 1 weekly reset left";
    const LIFTED: [&str; 2] = [
        "Your limit had already reset on its own · nothing was used",
        "Your limits are clear",
    ];
    const KEPT: &str =
        "Weekly reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    const LOGIN: &str = "Couldn't reset your limits with this login · run /login, then try again";
    /// The weekly session reset's answers (`yJ`, `Ir` @244662428): not this
    /// step's, though the first opens as the 5-hour kind's spent line does;
    /// the last three are `Ir`'s own (its unavailable, in-flight and login
    /// answers, @~244664184).
    const JUNIPER: [&str; 7] = [
        "Session limit reset · next reset available Oct 9 at 5am · your weekly limit still applies",
        "Session limit reset · your weekly limit still applies",
        "Weekly reset used · available again Oct 9 at 5am",
        "Couldn't reset your session limit right now · try again in a moment",
        "A session-limit reset isn't available right now.",
        "Your session limit is already being reset · one moment",
        "Couldn't reset your session limit with this login · run /login, then try again",
    ];

    /// THE ANSWER LINE, one row for each line of the vendor's table: each
    /// kind's spent line with its count and with the week's segment dropped,
    /// the Fable kind's still-walled line, the two lifted lines, each kind's
    /// kept lines, the login line, and every other answer a failure — `asks`
    /// with the ending and without it.
    #[test]
    fn a_spent_line_is_read_by_its_kinds_head() {
        let asking = |line: &str| format!("{line}{ASKS}");
        for (line, kind, left) in [
            (SPENT, ResetKind::Weekly, 1),
            (
                "Session limit reset · your weekly reset day stays Oct 9 at 5am · 1 five-hour \
                 reset left",
                ResetKind::Session,
                1,
            ),
            (
                "Session limit reset · your weekly reset day stays Oct 9 at 5am · no five-hour \
                 resets left",
                ResetKind::Session,
                0,
            ),
            (
                "Limits reset · your weekly reset day stays Jan 2, 2027 at 5:30am · 12 full \
                 resets left",
                ResetKind::Full,
                12,
            ),
            (
                "Fable reset used · what Fable used is back in your weekly limit · your weekly \
                 reset day stays Oct 9 at 5am · 1 Fable reset left",
                ResetKind::Fable,
                1,
            ),
            (
                "Session limit reset · 1 five-hour reset left",
                ResetKind::Session,
                1,
            ),
            (
                "Weekly limits reset · no weekly resets left",
                ResetKind::Weekly,
                0,
            ),
            ("Limits reset · 3 full resets left", ResetKind::Full, 3),
        ] {
            assert_eq!(
                reset_line(line),
                ResetLine::Spent {
                    kind,
                    left: Some(left),
                    asks: false
                },
                "{line}"
            );
            assert_eq!(
                reset_line(&asking(line)),
                ResetLine::Spent {
                    kind,
                    left: Some(left),
                    asks: true
                },
                "{line}"
            );
        }
        for (line, kind, left) in [
            (
                "Limits reset · send a message to continue",
                ResetKind::Full,
                None,
            ),
            ("Limits reset · you have 3 more", ResetKind::Full, None),
            (
                "Weekly limits reset · you have 3 more",
                ResetKind::Weekly,
                None,
            ),
            (
                "Weekly limits reset · your weekly reset day stays Oct 9 at 5am",
                ResetKind::Weekly,
                None,
            ),
            (
                "Weekly limits reset · your weekly reset day stays Oct 9 · 1 weekly reset left",
                ResetKind::Weekly,
                Some(1),
            ),
            (
                "Weekly limits reset · 1 five-hour reset left",
                ResetKind::Weekly,
                None,
            ),
            (
                "Session limit reset · your weekly reset day stays Oct 9 at 5am",
                ResetKind::Session,
                None,
            ),
            (
                "Session limit reset · refilled · 2 five-hour resets left",
                ResetKind::Session,
                Some(2),
            ),
            (
                "Fable reset used · what Fable used is back in your weekly limit · enjoy",
                ResetKind::Fable,
                None,
            ),
            (
                "Limits reset · your weekly reset day stays Oct 9 at 5am · 1 reset left",
                ResetKind::Full,
                None,
            ),
            ("Limits reset · 3 resets left", ResetKind::Full, None),
        ] {
            let asks = line.ends_with(ASKS);
            assert_eq!(
                reset_line(line),
                ResetLine::Spent { kind, left, asks },
                "{line}"
            );
        }
        let walled = "Fable reset used · you're still at your weekly limit until Oct 9 at 5am · no \
                      Fable resets left";
        assert_eq!(
            reset_line(walled),
            ResetLine::Walled {
                kind: ResetKind::Fable,
                left: 0,
            }
        );
        assert_eq!(
            reset_line(&walled.replace("you're", "you’re")),
            reset_line(walled),
            "a typographic apostrophe"
        );
        assert_eq!(reset_line(&asking(walled)), ResetLine::Other);
        assert_eq!(
            reset_line(&format!("  {SPENT}\n")),
            ResetLine::Spent {
                kind: ResetKind::Weekly,
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
            "Weekly reset kept · run /limit-reset whenever you want to use it · use by Oct 5 at \
             3pm",
            "5-hour reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
            "Full reset kept · run /limit-reset at a limit to use it",
            "Fable reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
        ] {
            assert_eq!(reset_line(line), ResetLine::Kept, "{line}");
        }
        assert_eq!(reset_line(LOGIN), ResetLine::Login);
        assert_eq!(
            reset_line("Couldn’t reset your limits with this login · run /login, then try again"),
            ResetLine::Login,
            "a typographic apostrophe"
        );
        for line in FAILURES.iter().chain(&JUNIPER) {
            assert_eq!(reset_line(line), ResetLine::Other, "{line}");
            assert_eq!(reset_line(&asking(line)), ResetLine::Other, "{line}");
        }
        for line in [
            "Limits reset",
            "Weekly limits reset",
            "Limits reset on Oct 5",
            "Limits reset: 1 full reset left",
            "Limits resets · 1 full reset left",
            "limits reset · 1 full reset left",
            "WEEKLY LIMITS RESET · 1 weekly reset left",
            "Session limit reset · 1 weekly reset left",
            "Session limit reset · 1 5-hour reset left",
            "Session limit reset · send a message to continue",
            "Session limit reset · refilled",
            "The line was: Weekly limits reset · 1 weekly reset left",
            "Fable reset used · 1 Fable reset left",
            "Fable reset used · you're still at your weekly limit · 1 Fable reset left",
            "Your limits are clear · nothing was used",
            "Your limits are clear now",
            "Your limit had already reset on its own",
            "Weekly reset kept",
            "weekly reset kept · run /limit-reset at a limit to use it",
            "Weekly reset kept, run /limit-reset at a limit to use it",
            "Couldn't reset your limits",
            "Reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
            "Claude Code will continue automatically at 6am. Keep this session open; it may \
             still pause for permission prompts. Press esc to cancel the wait.",
            "send a message to continue",
            "",
        ] {
            assert_eq!(reset_line(line), ResetLine::Other, "{line:?}");
        }
    }

    /// NO LINE OF THE RESET PROGRAM IS A WALL: the wall table places none of
    /// the answers of any kind, the pending lines, the confirmation's rows,
    /// the grant rows, the chooser's rows or the footer notices — an answer
    /// printed after the wall must not read as a wall still standing (`Weekly
    /// limits reset · …` opens with the words of a weekly limit and the verb
    /// `reset`), or as a lost login (the login line names `/login` past its
    /// first ` · `).
    #[test]
    fn no_line_of_the_reset_program_is_a_wall() {
        use crate::wall::{WallKind, classify_wall};
        let fixed = [
            SPENT,
            "Weekly limits reset · no weekly resets left",
            "Session limit reset · your weekly reset day stays Oct 9 at 5am · 1 five-hour reset left",
            "Session limit reset · 1 five-hour reset left",
            "Limits reset · your weekly reset day stays Oct 9 at 5am · 2 full resets left",
            "Fable reset used · what Fable used is back in your weekly limit · your weekly reset \
             day stays Oct 9 at 5am · 1 Fable reset left",
            "Fable reset used · you're still at your weekly limit until Oct 9 at 5am · no Fable \
             resets left",
            LIFTED[0],
            LIFTED[1],
            KEPT,
            LOGIN,
            "Weekly reset kept · run /limit-reset whenever you want to use it · use by Oct 5 at 3pm",
            "5-hour reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
            "Full reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
            "Fable reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm",
            "Checking for a reset…",
            "Resetting your limits… this usually takes a few seconds",
            GRANT,
            SESSION,
            FULL,
            FABLE,
            FABLE_AT_WEEK,
            "Use a weekly reset",
            "Use a 5-hour reset?",
            "Use a weekly reset?",
            "Use a full reset?",
            "Use a Fable reset?",
            BODY,
            "Refills your session limit now · your weekly reset day stays Oct 9 at 5am",
            "Refills your session and weekly limits now · your weekly reset day stays Oct 9 at 5am",
            "Refills your Fable limit now · returns what Fable used to your weekly limit · your \
             weekly reset day stays Oct 9 at 5am",
            DETAIL,
            "2 five-hour resets left · use by Oct 5 at 3pm",
            "1 full reset left · use by Oct 5 at 3pm",
            "Yes, use my weekly reset",
            "Yes, use my 5-hour reset",
            "No, keep it",
            "You still have 40% of your weekly limit left — use your weekly reset anyway?",
            "You still have 40% of your session limit left — use your 5-hour reset anyway?",
            "You haven't reached a limit yet — use your full reset anyway?",
            "/limit-reset to use your 5-hour reset · 2 left · use by Oct 5",
            "/limit-reset to use your weekly reset · 1 left · use by Oct 9",
            "/limit-reset · 2 five-hour, 1 Fable resets · first expires Oct 5",
            "Reset used · 1 reset left",
            "Which reset do you want to use?",
            WEEKLY,
            "/limit-reset to reset your session limit now · uses weekly limit · 1/week",
            "Resetting your session limit… this usually takes a few seconds",
        ];
        let reasons = [
            "try again in a minute",
            "can't be used while you're at your session limit",
            "can't be used while you're at your weekly limit",
            "can't be used while you're at your Opus limit",
            "can't be used while you're at your Sonnet limit",
            "no Fable usage this week to give back",
            "can't be used while you're at another limit it doesn't refill",
            "doesn't refill the limit you've reached",
            "usable once you reach a limit it refills",
            "can't be used right now",
            "doesn't refill your session limit",
            "doesn't refill your weekly limit",
            "doesn't refill your Opus limit",
            "doesn't refill your Sonnet limit",
            "doesn't refill your Fable limit",
            "doesn't refill your usage credit limit",
        ];
        let chooser = ["5-hour reset", "Weekly reset", "Full reset", "Fable reset"]
            .into_iter()
            .flat_map(|kind| reasons.map(|reason| format!("{kind}{SEP}{reason}")));
        let lines = fixed.iter().chain(&FAILURES).chain(&JUNIPER);
        for line in lines.map(|line| (*line).to_string()).chain(chooser) {
            for line in [format!("{line}{ASKS}"), line] {
                assert_eq!(classify_wall(&line), None, "{line}");
            }
        }
        assert_eq!(
            classify_wall("You've hit your weekly limit · resets Oct 9 at 5am"),
            Some(WallKind::UsageWeekly)
        );
        assert_eq!(
            classify_wall("Weekly limit reached · resets Oct 9 at 5am"),
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
