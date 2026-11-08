// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! USAGE CREDITS, AND THE VENDOR'S OWN WORD THAT A WINDOW IS FILLING OR A MODEL'S WEEK IS FULL: what Claude Code says of the account's usage beside the composer and at a turn's end, read for the record; and the forms of it no reader here records, sighted so a capture can measure them ([`usage_sighting`]).

use crate::prompt::{PromptKind, PromptV2};

/// The words that name usage credits, lower case: `Continue with usage credits`, `Add funds to
/// continue with extra usage` ([`names`]). The approval policy reads a consent box by them, and
/// [`usage_sighting`] sights one.
pub const WORDS: &[&str] = &["usage credit", "extra usage"];

/// Whether `text` names usage credits ([`WORDS`]), whatever its case.
#[must_use]
pub fn names(text: &str) -> bool {
    let text = text.to_lowercase();
    WORDS.iter().any(|w| text.contains(w))
}

/// Whether the box `p` drawn on `rows` names usage credits ([`names`]) in its title or in the
/// words of its rows, from its first row to its last, the frame's own runs of box-drawing
/// characters left out.
#[must_use]
pub fn box_names(p: &PromptV2, rows: &[String]) -> bool {
    let end = p.span.1.saturating_add(1).min(rows.len());
    let frame = |w: &&str| w.chars().all(|c| ('\u{2500}'..='\u{257f}').contains(&c));
    let body = rows
        .get(p.span.0.min(end)..end)
        .unwrap_or_default()
        .iter()
        .flat_map(|r| r.split_whitespace())
        .filter(|w| !frame(w))
        .collect::<Vec<_>>()
        .join(" ");
    names(&p.title) || names(&body)
}

/// Which window a notice names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The five-hour window (`5-hour limit` from 2.1.312, `session limit` before).
    Session,
    /// The seven-day window (`weekly limit`).
    Weekly,
}

impl Window {
    /// The vendor's limit name after `Your ` / `of your ` → its window: 2.1.312 names the
    /// five-hour window `5-hour limit` (`Wce.five_hour`), the builds before it `session limit`.
    fn of_name(name: &str) -> Option<Window> {
        match name {
            "5-hour limit" | "session limit" => Some(Window::Session),
            "weekly limit" => Some(Window::Weekly),
            _ => None,
        }
    }

    /// The word the CLI and the records print.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Window::Session => "session",
            Window::Weekly => "weekly",
        }
    }
}

/// Usage credits carry the session ([`in_use`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credits {
    /// The notice that they have just STARTED (`You're now using usage credits · Your session limit resets 9:43am (…)`), with the window it names, if any, and its reset as printed; `None` for the lasting `Now using usage credits`, which names neither.
    pub started: Option<Started>,
    /// The MODEL whose requests they carry, as the notice names it (`Fable 5.1`, `Opus`): its per-model forms — `Now using usage credits for Opus` with or without its `· Your Opus limit resets 4pm`, `Fable 5.1 is drawing from usage credits`, `Fable 5.1 is now using usage credits instead of your plan limits`.
    pub model: Option<String>,
}

/// The window credits started past, as their first notice names it: the session or the weekly window, or none of the two where the notice names a MODEL's (`Now using usage credits for Opus · Your Opus limit resets 4pm`, the same notification's per-model form, READ in the 2.1.304 bundle: `wbo`), its reset that model's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub window: Option<Window>,
    pub reset: Option<String>,
}

/// The vendor's warning that a window is filling ([`usage_warning`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub window: Window,
    /// Whole percent used, 0 to 100.
    pub pct: u8,
    /// The window's reset as printed (`Oct 18 at 8:50pm (America/Los_Angeles)`).
    pub reset: Option<String>,
}

/// A model whose own week Claude Code names on screen ([`model_week`]): only the names a capture
/// measured. Opus, a numbered Fable (`Fable 5 limit`) and the usage-credits limit are no capture's,
/// so none of them is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekModel {
    /// The Fable week (`Fable limit`).
    Fable,
    /// The Sonnet week (`Sonnet limit`).
    Sonnet,
}

impl WeekModel {
    /// The word the records print.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            WeekModel::Fable => "fable",
            WeekModel::Sonnet => "sonnet",
        }
    }

    /// The model a wall's message names by its head: `You've hit your Fable limit`,
    /// `You've reached your Fable limit` or `You've hit your Sonnet limit`, the head the whole
    /// message, or followed by a blank, a full stop or the notice's `·`.
    fn of_wall(message: &str) -> Option<WeekModel> {
        [
            (crate::anchor!("wall.fable_hit"), WeekModel::Fable),
            (crate::anchor!("wall.fable"), WeekModel::Fable),
            (crate::anchor!("wall.sonnet_hit"), WeekModel::Sonnet),
        ]
        .into_iter()
        .find_map(|(head, model)| {
            let rest = message.strip_prefix(head)?;
            (rest.is_empty() || rest.starts_with([' ', '.', '\u{b7}'])).then_some(model)
        })
    }
}

/// A model's week as Claude Code last said it on screen ([`model_week`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelWeek {
    pub model: WeekModel,
    /// Whole percent used, 0 to 100: 100 at its wall.
    pub pct: u8,
    /// The week's reset as printed (`1:36am (America/Los_Angeles)`); `None` where the notice names
    /// none (`You've hit your Fable limit`, `You've reached your Fable limit. Run /usage-credits …`).
    pub reset: Option<String>,
    /// It was read off the model's wall, not its warning.
    pub wall: bool,
}

/// THE WEEK OF A MODEL, as Claude Code says it on `rows`: the model's wall, the notice the turn
/// ended on ([`crate::wall::wall`]: `You've hit your Fable limit · resets 1:36am (…)`, with no
/// reset, or `You've reached your Fable limit. Run /usage-credits …`; `You've hit your Sonnet
/// limit · resets …`), read as 100% with the reset it names; else the Fable warning beside the
/// composer (`You've used 85% of your Fable limit · resets Oct 19 at 11:36pm (…)`, read as
/// [`usage_warning`] reads the session's and the week's). The wall wins where both show: the
/// footer keeps a warning drawn before the wall that followed it. Only the measured names are
/// read ([`WeekModel`]); any other model's week, a numbered name and the usage-credits limit
/// read `None`.
#[must_use]
pub fn model_week(rows: &[String]) -> Option<ModelWeek> {
    if !rows.iter().any(|row| row.contains("limit")) {
        return None;
    }
    let wall = crate::wall::wall(rows)
        .filter(|w| matches!(w.kind, crate::wall::WallKind::ModelBucket { .. }))
        .and_then(|w| Some((WeekModel::of_wall(&w.message)?, w.reset)));
    if let Some((model, reset)) = wall {
        return Some(ModelWeek {
            model,
            pct: 100,
            reset,
            wall: true,
        });
    }
    let fable = const { crate::anchor!("usage.fable_limit") };
    find_warning(rows, |pct, name, after| {
        (name == fable).then(|| ModelWeek {
            model: WeekModel::Fable,
            pct,
            reset: crate::phase::notice_reset(after),
            wall: false,
        })
    })
}

/// Whether usage credits carry the session, by Claude Code's own notice: the lasting
/// `Now using usage credits`, `Now using your usage allocation`,
/// `Extra usage is now covering your requests` (the end of lower-priority mode), or the notice they
/// started on — which wins where both show, for the window it names.
#[must_use]
pub fn in_use(rows: &[String]) -> Option<Credits> {
    let mut found: Option<Credits> = None;
    let transcript = crate::phase::Transcript::of(rows);
    for (_, segment) in segments(rows, &transcript) {
        for head in [
            crate::anchor!("credits.started"),
            crate::anchor!("credits.allocation_started"),
        ] {
            if let Some(rest) = segment.strip_prefix(head) {
                return Some(Credits {
                    started: Some(started(rest)),
                    model: None,
                });
            }
        }
        if let Some(rest) = segment
            .strip_prefix(crate::anchor!("credits.in_use"))
            .and_then(|rest| rest.strip_prefix(" for "))
        {
            let (name, tail) = rest
                .split_once(" \u{b7} ")
                .map_or((rest, ""), |(name, tail)| (name, tail));
            let model = model_name(name);
            let begun = started(&format!(" \u{b7} {tail}"));
            if begun.reset.is_some() {
                return Some(Credits {
                    started: Some(begun),
                    model,
                });
            }
            if model.is_some() {
                found = Some(Credits {
                    started: None,
                    model,
                });
                continue;
            }
        }
        if let Some(model) = [
            crate::anchor!("credits.model_drawing"),
            crate::anchor!("credits.model_instead"),
        ]
        .iter()
        .find_map(|tail| segment.strip_suffix(tail))
        .and_then(model_name)
        {
            found = Some(Credits {
                started: None,
                model: Some(model),
            });
            continue;
        }
        let lasting = [
            crate::anchor!("credits.in_use"),
            crate::anchor!("credits.allocation"),
            crate::anchor!("credits.extra_covering"),
        ]
        .iter()
        .any(|head| segment.starts_with(head));
        if lasting && found.is_none() {
            found = Some(Credits {
                started: None,
                model: None,
            });
        }
    }
    found
}

/// A model's name as a notice prints it before its words (`Fable 5.1`,
/// `Opus`): a few words, no run of blanks, none of the notice's own
/// separators.
fn model_name(name: &str) -> Option<String> {
    let name = name.trim();
    let words = name.split(' ').count();
    (!name.is_empty()
        && words <= 4
        && !name.contains("  ")
        && !name.contains('\u{b7}')
        && name.chars().next().is_some_and(char::is_alphanumeric))
    .then(|| name.to_string())
}

/// What follows a started notice's words: ` · Your <limit> resets <time>`.
fn started(rest: &str) -> Started {
    let window = rest
        .strip_prefix(" \u{b7} Your ")
        .and_then(|r| r.split_once(" resets "))
        .and_then(|(name, _)| Window::of_name(name));
    Started {
        window,
        reset: crate::phase::notice_reset(rest),
    }
}

/// The vendor's warning that the session or the weekly window is filling (module docs): `You've used <n>% of your session limit · resets <time>` (`5-hour limit` from 2.1.312), or of the `weekly limit`.
#[must_use]
pub fn usage_warning(rows: &[String]) -> Option<Warning> {
    find_warning(rows, |pct, name, after| {
        Some(Warning {
            window: Window::of_name(name)?,
            pct,
            reset: crate::phase::notice_reset(after),
        })
    })
}

/// The first `You've used <n>% of your <limit> · <rest>` the notice segments hold, the footer's
/// first ([`segments`]), that `read` takes: its whole percent (at most 100), the limit's name and
/// what follows the name's `·` (empty where nothing does).
fn find_warning<T>(
    rows: &[String],
    mut read: impl FnMut(u8, &str, &str) -> Option<T>,
) -> Option<T> {
    let transcript = crate::phase::Transcript::of(rows);
    segments(rows, &transcript).find_map(|(_, segment)| {
        let (pct, name, after) = warning_of(segment)?;
        read(pct, name, after)
    })
}

/// A notice segment that opens on the warning's lead (`You've used `), where it is one.
fn warning_lead(segment: &str) -> Option<&str> {
    segment
        .strip_prefix(const { crate::anchor!("usage.warning") })?
        .strip_prefix(' ')
}

/// A `You've used <n>% of your <limit> · <rest>` segment's whole percent (at most 100), the
/// limit's name and what follows the name's `·` (empty where nothing does).
fn warning_of(segment: &str) -> Option<(u8, &str, &str)> {
    let rest = warning_lead(segment)?;
    let (pct, rest) = rest.split_once("% of your ")?;
    let pct: u8 = pct
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| pct.parse().ok())
        .flatten()
        .filter(|&p| p <= 100)?;
    let (name, after) = rest.split_once(" \u{b7} ").unwrap_or((rest, ""));
    Some((pct, crate::text::trim_end(name), after))
}

/// What a screen shows of the account's usage in a form no reader here reads
/// ([`usage_sighting`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSighting {
    /// What it is, as a capture names it: `usage-consent`, `usage-wall-unread` or
    /// `usage-warning-unread`.
    pub what: &'static str,
    /// The box's rows (trimmed, joined by `\n`, as [`crate::ResetSighting::key`]) or the notice's
    /// text: the same form drawn again has the same key, and a change to it (another figure,
    /// another reset, another row focused) gives another.
    pub key: String,
    /// The first row a capture of it keeps: the box's first row or the rule drawn right over it,
    /// or the notice's row. The transcript above it is none of the form's.
    pub top: usize,
}

/// THE ACCOUNT'S USAGE AS `rows` SAY IT IN A FORM NO READER HERE RECORDS, the first of:
/// - `usage-consent`: a box of Claude Code's own ([`crate::prompt::parse_prompt_v2`]'s
///   [`PromptKind::Other`], the one kind the approval policy reads a credits consent in) whose
///   title, rows or an option names usage credits ([`box_names`], [`names`]), other than a box of
///   the reset program ([`crate::reset_sighting`]: the usage-limit dialog's `Add funds to continue
///   with usage credits` row is that program's). A tool's box, a question or a plan whose command,
///   diff or words quote them is the agent's, never the account's;
/// - `usage-wall-unread`: the wall the turn ended on ([`crate::wall::wall`]) of a model's bucket
///   that [`model_week`] does not read (Opus, a numbered Fable) or whose reset `places` does not
///   place, or of money (a spend limit, a shared budget, no credits left);
/// - `usage-warning-unread`: a `You've used …` notice beside the composer that neither
///   [`usage_warning`] nor [`model_week`] puts on record: another limit's name, no whole percent,
///   or a Fable figure whose reset is unnamed or one `places` does not place.
///
/// `places` is the recorder's clock: whether it places a reset's words (`1:36am
/// (America/Los_Angeles)`) on an instant. A model's week is recorded only with its reset placed or,
/// at its wall, none named, so a week whose reset it cannot place is recorded nowhere and is
/// sighted here instead. The session's and the week's warnings are recorded whatever their reset
/// says, so they are read by their name.
///
/// A wall comes before a warning: a capture keeps the rows from its `top` to the screen's foot,
/// so the wall's keeps the footer's warning too. A recorded form, and a program's own words
/// quoting one, are none.
#[must_use]
pub fn usage_sighting(rows: &[String], places: impl Fn(&str) -> bool) -> Option<UsageSighting> {
    if let Some(p) = crate::prompt::parse_prompt_v2(rows)
        && p.kind == PromptKind::Other
        && crate::reset::reset_box(&p).is_none()
        && (box_names(&p, rows) || p.options.iter().any(|o| names(&o.label)))
    {
        let (key, top) = crate::reset::sighted(rows, p.span.0, p.span.1);
        return Some(UsageSighting {
            what: "usage-consent",
            key,
            top,
        });
    }
    let unread_wall = crate::wall::wall(rows).filter(|w| match w.kind {
        crate::wall::WallKind::ModelBucket { .. } => {
            WeekModel::of_wall(&w.message).is_none()
                || w.reset.as_deref().is_some_and(|reset| !places(reset))
        }
        crate::wall::WallKind::Spend => true,
        _ => false,
    });
    if let Some(w) = unread_wall {
        return Some(UsageSighting {
            what: "usage-wall-unread",
            key: w.message,
            top: w.row,
        });
    }
    let fable = const { crate::anchor!("usage.fable_limit") };
    let read = |segment: &str| {
        warning_of(segment).is_some_and(|(_, name, after)| {
            Window::of_name(name).is_some()
                || (name == fable
                    && crate::phase::notice_reset(after).is_some_and(|reset| places(&reset)))
        })
    };
    let transcript = crate::phase::Transcript::of(rows);
    segments(rows, &transcript)
        .find(|(_, segment)| warning_lead(segment).is_some() && !read(segment))
        .map(|(top, segment)| UsageSighting {
            what: "usage-warning-unread",
            key: segment.to_string(),
            top,
        })
}

/// The right segments of the rows a notification is drawn on, each with its row, the footer's
/// first: a footer row whole and against the composer's width, a row over the composer read on
/// its `transcript`, up to the side panel's rule and against its room
/// ([`crate::phase::Transcript`]), the right edge a notice stands against beside the panel.
fn segments<'a>(
    rows: &'a [String],
    transcript: &'a crate::phase::Transcript<'_>,
) -> impl Iterator<Item = (usize, &'a str)> {
    use crate::phase::{composer_frame, footer_end, status_block};
    use crate::wall::{in_queued_message, right_segment};
    let frame = composer_frame(rows);
    let width = frame.map_or(0, |f| {
        crate::text::trim_end(&rows[f.bottom]).chars().count()
    });
    let room = transcript.room(width);
    let said: &'a [String] = &transcript.rows;
    let reaches = |row: &str, width: usize| row.chars().count() + 3 >= width;
    let footer = frame
        .map(|f| f.bottom + 1..footer_end(rows, f.bottom + 1))
        .into_iter()
        .flatten()
        .filter_map(move |i| {
            let row = crate::text::trim_end(&rows[i]);
            reaches(row, width).then(|| (i, right_segment(row).1))
        });
    let zone = frame
        .map(|f| {
            let block = status_block(rows, f.top);
            let under = block.status.map_or(f.top, |s| s + 1);
            (under..f.top).filter_map(move |i| {
                let row = crate::text::trim_end(&said[i]);
                let queued = block.status.is_some_and(|s| in_queued_message(said, s, i));
                (reaches(row, room) && !queued).then(|| (i, right_segment(row).1))
            })
        })
        .into_iter()
        .flatten();
    footer
        .chain(zone)
        .filter(|(_, segment)| !segment.is_empty())
}

#[cfg(test)]
#[path = "credits_tests.rs"]
mod tests;
