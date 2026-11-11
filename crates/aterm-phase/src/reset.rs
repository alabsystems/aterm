// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact-match parsers for Claude Code usage-limit reset dialogs and outcomes.

use crate::anchors::anchor;
use crate::phase::{has_composer_frame, is_rule, leading_spaces};
use crate::prompt::{PromptKind, PromptV2, is_panel_rule};

const SEP: &str = " · ";

/// The 5-hour kind's `{done}` builds before 2.1.312 drew (2.1.312 names the window `5-hour limit`,
/// anchor `reset.done.session`).
pub const SESSION_DONE_BEFORE_2_1_312: &str = "Session limit reset";

/// The once-a-week session reset's row builds before 2.1.312 drew (anchor `reset.weekly_row`,
/// 2.1.312's `Reset your 5-hour limit now`).
pub const WEEKLY_ROW_BEFORE_2_1_312: &str = "Reset your session limit now";

/// The five-hour window's name in the confirmation's `{limits}` (2.1.312's `ar()`:
/// `case"five_hour":s.add("5-hour")`).
const FIVE_HOUR_LIMIT: &str = "5-hour";

/// The five-hour window's name in the confirmation's `{limits}` builds before 2.1.312 drew
/// (2.1.312 draws `5-hour`).
pub const FIVE_HOUR_LIMIT_BEFORE_2_1_312: &str = "session";

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

    fn noun(self) -> &'static str {
        match self {
            ResetKind::Session => crate::anchor!("reset.kind.session_count"),
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

    /// [`Self::done`] and the head the builds before 2.1.312 drew for this kind
    /// ([`SESSION_DONE_BEFORE_2_1_312`]).
    fn done_heads(self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.done())
            .chain((self == ResetKind::Session).then_some(SESSION_DONE_BEFORE_2_1_312))
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
        format!("{} {}", crate::anchor!("reset.yes"), self.name())
    }

    fn row_head(self) -> String {
        format!("{} {}", crate::anchor!("reset.row"), self.name())
    }

    fn early(self) -> String {
        format!(
            "{} {} {}",
            crate::anchor!("reset.early"),
            self.name(),
            crate::anchor!("reset.early.anyway")
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
    /// Its label as drawn: what a press fences on.
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

/// Whether choosing an option labelled `label` spends a reset: the once-a-week row as any build
/// draws it (anchor `reset.weekly_row`, [`WEEKLY_ROW_BEFORE_2_1_312`]), a grant row or a yes.
#[must_use]
pub fn spends_reset(label: &str) -> bool {
    opens(label, crate::anchor!("reset.weekly_row"))
        || opens(label, WEEKLY_ROW_BEFORE_2_1_312)
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

fn opens(label: &str, words: &str) -> bool {
    label
        .trim_start()
        .get(..words.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(words))
}

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

fn grant_of(label: &str, kind: ResetKind) -> Option<(u32, &str)> {
    let inner = label
        .strip_prefix(kind.row_head().as_str())?
        .strip_prefix(" (")?
        .strip_suffix(')')?;
    let (left, day) = inner.split_once(&format!(" {} ", crate::anchor!("reset.row.until")))?;
    let left = number(left).filter(|&n| n >= 1)?;
    (take_day(day) == Some("")).then_some((left, day))
}

fn number(text: &str) -> Option<u32> {
    let plain = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    plain.then(|| text.parse().ok()).flatten()
}

fn take_number(text: &str, range: std::ops::RangeInclusive<u32>) -> Option<(u32, &str)> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let n = number(&text[..end]).filter(|n| range.contains(n))?;
    Some((n, &text[end..]))
}

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

/// The windows a grant clears that a takeable kind's `{limits}` names, each by
/// [`reset_limit_names`] (`ar()`'s cases in 2.1.311 and 2.1.312). `ar()` also draws `Fable`, but
/// only for a grant that gives back weekly use, which `qe()` makes a Fable reset before anything
/// else; and it names no other window.
const NAMED_WINDOWS: [&str; 5] = [
    "five_hour",
    "seven_day",
    "seven_day_overage_included",
    "seven_day_opus",
    "seven_day_sonnet",
];

/// The names of a confirmation's `{limits}` as drawn, each one a build draws for a window of
/// [`NAMED_WINDOWS`]; none for any other name (`Weekly`, `Fable`, `Session`) or join.
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
        .all(|n| {
            NAMED_WINDOWS
                .iter()
                .any(|window| reset_limit_names(window).contains(n))
        })
        .then(|| names.iter().map(|n| (*n).to_string()).collect())
}

/// Whether `names`, a confirmation's `{limits}` as read, are what a reset of `kind` refills, by the
/// fallback of the vendor's reading of a grant's kind (`qe()`, read in 2.1.310 to 2.1.312: the
/// five-hour limit alone is a 5-hour reset, no five-hour limit a weekly one, the five-hour limit and
/// another a full one), held even where the server types the grant. `qe()` takes the grant's
/// server-sent `type` before its clears, and its fallback also counts clears `ar()` names nowhere
/// (`seven_day_cowork`), so a grant typed against its clears, or with such a clear beside the
/// five-hour one, draws a confirmation this refuses: the reset is kept. The five-hour limit is
/// named once, by one build's word ([`reset_limit_names`]), and no name twice (the vendor draws a
/// set).
fn refills(kind: ResetKind, names: &[String]) -> bool {
    let five = names
        .iter()
        .filter(|n| reset_limit_names("five_hour").contains(&n.as_str()))
        .count();
    let others = names.len() - five;
    let distinct = names
        .iter()
        .enumerate()
        .all(|(k, n)| !names[..k].contains(n));
    distinct
        && match kind {
            ResetKind::Session => five == 1 && others == 0,
            ResetKind::Weekly => five == 0 && others >= 1,
            ResetKind::Full => five == 1 && others >= 1,
            ResetKind::Fable => false,
        }
}

/// The name a walled window is drawn by in the confirmation's `{limits}`, as 2.1.312 draws it (the
/// first of [`reset_limit_names`]).
#[must_use]
pub fn reset_limit_name(limit_type: &str) -> Option<&'static str> {
    reset_limit_names(limit_type).first().copied()
}

/// Every name a walled window is drawn by in the confirmation's `{limits}`, 2.1.312's first, then
/// the five-hour window's [`FIVE_HOUR_LIMIT_BEFORE_2_1_312`]; none for a window no grant clears.
#[must_use]
pub fn reset_limit_names(limit_type: &str) -> &'static [&'static str] {
    match limit_type {
        "five_hour" => &[FIVE_HOUR_LIMIT, FIVE_HOUR_LIMIT_BEFORE_2_1_312],
        "seven_day" | "seven_day_overage_included" => &["weekly"],
        "seven_day_opus" => &["Opus"],
        "seven_day_sonnet" => &["Sonnet"],
        _ => &[],
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
        || keep.label != crate::anchor!("reset.no")
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
        .any(|o| crate::text::trim(&o.label) == crate::anchor!("reset.no"))
    {
        return true;
    }
    let stays = crate::anchor!("reset.confirm.stays");
    let stays = stays.split(SEP).last().unwrap_or(stays);
    let early: Vec<String> = ResetKind::ALL.into_iter().map(ResetKind::early).collect();
    rows.get(prompt.span.0..first.row.min(rows.len()))
        .unwrap_or_default()
        .iter()
        .map(|row| crate::text::trim(row))
        .any(|row| {
            row.starts_with(crate::anchor!("reset.confirm.refills"))
                || [stays, crate::anchor!("reset.confirm.use_by")]
                    .iter()
                    .any(|part| row.contains(part))
                || early.iter().any(|tail| row.contains(tail.as_str()))
        })
}

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
    let stays = crate::anchor!("reset.confirm.stays");
    let use_by = crate::anchor!("reset.confirm.use_by");
    let now = stays.split(SEP).next().unwrap_or(stays);
    let left = use_by.split(SEP).next().unwrap_or(use_by);
    let (limits, rest) = text
        .strip_prefix(crate::anchor!("reset.confirm.refills"))
        .and_then(|rest| rest.strip_prefix(' '))
        .and_then(|rest| rest.split_once(&format!(" {now}")))
        .filter(|(_, rest)| rest.is_empty() || rest.starts_with(' '))
        .ok_or("body")?;
    let limits = limits_of(limits)
        .filter(|names| refills(kind, names))
        .ok_or("body")?;
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
        if title == crate::anchor!("limit.title") {
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

fn is_guide(row: &str) -> bool {
    let t = crate::text::trim(row);
    t.split(SEP).all(|item| item.contains(" to "))
        && t.split(SEP)
            .any(|item| item.eq_ignore_ascii_case(crate::anchor!("prompt.cancel")))
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
    let (body, asks) =
        match text.strip_suffix(&format!("{SEP}{}", crate::anchor!("reset.continue"))) {
            Some(body) => (body, true),
            None => (text.as_str(), false),
        };
    if let Some((kind, left)) = spent_line(&text) {
        ResetLine::Spent { kind, left, asks }
    } else if let Some((kind, left)) = walled_line(&text) {
        ResetLine::Walled { kind, left }
    } else if body == crate::anchor!("reset.lifted") || body == crate::anchor!("reset.clear") {
        ResetLine::Lifted { asks }
    } else if ResetKind::ALL.into_iter().any(|kind| {
        text.starts_with(&format!(
            "{} {}{SEP}",
            kind.capital(),
            crate::anchor!("reset.kept")
        ))
    }) {
        ResetLine::Kept
    } else if text.starts_with(&format!("{}{SEP}", crate::anchor!("reset.login")))
        || text == crate::anchor!("reset.login")
    {
        ResetLine::Login
    } else {
        ResetLine::Other
    }
}

fn left_of(segment: &str, kind: ResetKind) -> Option<u32> {
    let use_by = crate::anchor!("reset.confirm.use_by");
    let left = use_by.split(SEP).next().unwrap_or(use_by);
    kind.resets_of(segment.strip_suffix(&format!(" {left}"))?)
}

fn spent_line(text: &str) -> Option<(ResetKind, Option<u32>)> {
    let stays = crate::anchor!("reset.confirm.stays");
    let stays = stays.split(SEP).last().unwrap_or(stays);
    ResetKind::ALL.into_iter().find_map(|kind| {
        let rest = kind
            .done_heads()
            .find_map(|done| text.strip_prefix(done)?.strip_prefix(SEP))?;
        let left = rest.split(SEP).find_map(|segment| left_of(segment, kind));
        let own = kind != ResetKind::Session
            || left.is_some()
            || rest.split(SEP).any(|segment| segment.starts_with(stays));
        own.then_some((kind, left))
    })
}

fn walled_line(text: &str) -> Option<(ResetKind, u32)> {
    ResetKind::ALL.into_iter().find_map(|kind| {
        let rest = text
            .strip_prefix(kind.capital().as_str())?
            .strip_prefix(' ')?
            .strip_prefix(crate::anchor!("reset.walled"))?
            .strip_prefix(' ')?;
        let (limit, count) = rest.split_once(SEP)?;
        if !limit.contains(" until ") {
            return None;
        }
        Some((kind, left_of(count, kind)?))
    })
}

/// What a screen shows of the reset program ([`reset_sighting`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetSight {
    /// The usage-limit dialog (`What do you want to do?`) with no row that uses or spends a reset.
    Dialog,
    /// The usage-limit dialog with a row that uses or spends a reset on offer (a grant row, the
    /// once-a-week row).
    Offer,
    /// A reset's confirmation (`Use a {kind}?`).
    Confirmation,
    /// Another box with a row that uses or spends a reset (the chooser a typed `/limit-reset`
    /// opens).
    Chooser,
    /// A line drawn while a reset is looked up or claimed ([`reset_pending`]).
    Pending(ResetPending),
    /// The answer on the `⎿` row under a `/rate-limit-options` or `/limit-reset` command row,
    /// as [`reset_line`] reads it (the wait row's answer and every line it does not know read
    /// [`ResetLine::Other`]).
    Answer(ResetLine),
}

impl ResetSight {
    /// The sight's word in a capture (`dialog`, `offer`, `confirmation`, `chooser`, `checking`,
    /// `resetting`, `answer`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Dialog => "dialog",
            Self::Offer => "offer",
            Self::Confirmation => "confirmation",
            Self::Chooser => "chooser",
            Self::Pending(ResetPending::Checking) => "checking",
            Self::Pending(ResetPending::Resetting) => "resetting",
            Self::Answer(_) => "answer",
        }
    }
}

/// A screen's sight of the reset program ([`reset_sighting`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetSighting {
    /// What the screen shows.
    pub sight: ResetSight,
    /// The rows that show it, trimmed and joined by `\n`: the same sight drawn again (another
    /// read, the screen scrolled) has the same key, and a change to any of its rows (another
    /// row focused, another count, a reworded answer) gives another.
    pub key: String,
    /// The first row a capture of the sight keeps: the first of the rows that show it, or the
    /// rule drawn right over them. The rows from it to the screen's foot are sighted as the same
    /// sight with the same key; the transcript above it is none of the sight's.
    pub top: usize,
}

/// What `rows` show of the reset program, where they show any of it: a pending line first, then
/// a box (the usage-limit dialog, with a reset on offer or none, a confirmation, the chooser),
/// then the answer under the newest `/rate-limit-options` or `/limit-reset` command row while it
/// is the newest thing drawn (nothing but blank rows between it and the composer). A reply or a
/// tool's output that quotes the program's words is none of these.
#[must_use]
pub fn reset_sighting(rows: &[String]) -> Option<ResetSighting> {
    let seen = |sight: ResetSight, from: usize, to: usize| {
        let (key, top) = sighted(rows, from, to);
        ResetSighting { sight, key, top }
    };
    if let Some((pending, row)) = reset_pending(rows) {
        let title = (0..row)
            .rev()
            .find(|&i| !crate::text::trim(&rows[i]).is_empty());
        return Some(seen(
            ResetSight::Pending(pending),
            title.unwrap_or(row),
            row,
        ));
    }
    if let Some(p) = crate::prompt::parse_prompt_v2(rows)
        && let Some(sight) = reset_box(&p)
    {
        let (top, bottom) = p.span;
        return Some(seen(sight, top, bottom));
    }
    let (command, answer, last) = reset_answer(rows)?;
    let foot = crate::phase::composer_frame(rows).map_or(rows.len(), |frame| frame.top);
    let newest = rows
        .get(last + 1..foot)
        .unwrap_or_default()
        .iter()
        .all(|row| crate::text::trim(row).is_empty());
    newest.then(|| seen(ResetSight::Answer(reset_line(&answer)), command, last))
}

/// What the box `p` shows of the reset program, where it is one of its boxes: the usage-limit
/// dialog, with a reset on offer or none, a confirmation, or another box with a row that uses or
/// spends a reset (the chooser).
pub(crate) fn reset_box(p: &PromptV2) -> Option<ResetSight> {
    let uses = p
        .options
        .iter()
        .any(|o| spends_reset(&o.label) || uses_a_reset(&o.label));
    if p.kind == PromptKind::UsageLimit {
        Some(if uses {
            ResetSight::Offer
        } else {
            ResetSight::Dialog
        })
    } else if reset_title(&p.title).is_some() {
        Some(ResetSight::Confirmation)
    } else if uses {
        Some(ResetSight::Chooser)
    } else {
        None
    }
}

/// A sighting's key and top: `rows[from..=to]` trimmed and joined by `\n`, and the first row a
/// capture of it keeps, `from` or the rule drawn right over it.
pub(crate) fn sighted(rows: &[String], from: usize, to: usize) -> (String, usize) {
    let key = rows
        .get(from..=to)
        .unwrap_or_default()
        .iter()
        .map(|row| crate::text::trim(row))
        .collect::<Vec<_>>()
        .join("\n");
    let ruled = from
        .checked_sub(1)
        .filter(|&over| is_panel_rule(&rows[over]) || is_rule(&rows[over]));
    (key, ruled.unwrap_or(from))
}

/// The newest `/rate-limit-options` or `/limit-reset` command row with a `⎿` row under it: the
/// command's row, the answer's words (its wrapped rows joined) and the answer's last row.
fn reset_answer(rows: &[String]) -> Option<(usize, String, usize)> {
    let dialog = crate::anchor!("wall.limit_dialog");
    let dialog = dialog.strip_prefix('/').unwrap_or(dialog);
    let reset = crate::anchor!("reset.command");
    let command = rows.iter().rposition(|row| {
        leading_spaces(row) == 0
            && crate::text::trim(row)
                .strip_prefix('❯')
                .map(crate::text::trim)
                .and_then(|typed| typed.strip_prefix('/'))
                .and_then(|typed| typed.split_whitespace().next())
                .is_some_and(|name| name == dialog || name == reset)
    })?;
    let first = (command + 1..rows.len()).find(|&i| !crate::text::trim(&rows[i]).is_empty())?;
    let head = crate::text::trim(&rows[first]).strip_prefix('⎿')?;
    let column = leading_spaces(&rows[first]);
    let mut words = vec![crate::text::trim(head)];
    let mut last = first;
    for row in &rows[first + 1..] {
        let t = crate::text::trim(row);
        if t.is_empty() || leading_spaces(row) <= column || t.starts_with('⎿') {
            break;
        }
        words.push(t);
        last += 1;
    }
    Some((command, words.join(" "), last))
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
    /// Claude Code 2.1.312 at a five-hour wall, then the weekly 5-hour reset's answer under
    /// `/rate-limit-options` (`5-hour limit reset · next reset available …`).
    pub const DONE_WEEKLY_ROW_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reset-5-hour-limit-done.txt");
    /// The same wall, then a 5-hour reset's spent line (`5-hour limit reset · your weekly reset
    /// day stays … · 1 five-hour reset left`).
    pub const SPENT_SESSION_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reset-5-hour-kind-spent.txt");
    /// NEGATIVE CONTROL: the worker's tool output, a diff and its reply quoting 2.1.312's reset
    /// and wall words, none of it Claude Code's own notice.
    pub const QUOTED_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reply-quotes-5-hour-reset-words.txt");
    /// 2.1.312's dialog with its once-a-week row (`Reset your 5-hour limit now`) where the grant
    /// rows go.
    pub const DIALOG_WEEKLY_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reset-dialog-once-a-week.txt");
    /// NEGATIVE CONTROL: 2.1.312's once-a-week row beside a 5-hour grant row, a menu no build
    /// draws.
    pub const DIALOG_WEEKLY_BESIDE_GRANT_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reset-dialog-once-a-week-beside-grant.txt");
    /// 2.1.312's 5-hour reset's confirmation as it opens (`Refills your 5-hour limit now`).
    pub const CONFIRM_SESSION_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reset-confirm-5-hour.txt");
    /// 2.1.312's full reset's confirmation as it opens (`Refills your 5-hour and weekly limits`).
    pub const CONFIRM_FULL_2_1_312: &str =
        include_str!("fixtures/claude-2.1.312-reset-confirm-full.txt");

    /// Every fixture of the reset program, with its name.
    pub const ALL: [(&str, &str); 21] = [
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
        ("done-weekly-row-2.1.312", DONE_WEEKLY_ROW_2_1_312),
        ("spent-session-2.1.312", SPENT_SESSION_2_1_312),
        ("quoted-2.1.312", QUOTED_2_1_312),
        ("dialog-weekly-2.1.312", DIALOG_WEEKLY_2_1_312),
        (
            "dialog-weekly-beside-grant-2.1.312",
            DIALOG_WEEKLY_BESIDE_GRANT_2_1_312,
        ),
        ("confirm-session-2.1.312", CONFIRM_SESSION_2_1_312),
        ("confirm-full-2.1.312", CONFIRM_FULL_2_1_312),
    ];
}

#[cfg(test)]
#[path = "reset_tests.rs"]
mod tests;
