// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! USAGE CREDITS, AND THE VENDOR'S OWN WORD THAT A WINDOW IS FILLING: what Claude Code says of the account's usage beside the composer, read for the record.

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

/// Whether usage credits carry the session, by Claude Code's own notice: the lasting
/// `Now using usage credits`, `Now using your usage allocation`,
/// `Extra usage is now covering your requests` (the end of lower-priority mode), or the notice they
/// started on — which wins where both show, for the window it names.
#[must_use]
pub fn in_use(rows: &[String]) -> Option<Credits> {
    let mut found: Option<Credits> = None;
    for segment in segments(rows) {
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
    let lead = const { crate::anchor!("usage.warning") };
    segments(rows).find_map(|segment| {
        let rest = segment.strip_prefix(lead)?.strip_prefix(' ')?;
        let (pct, rest) = rest.split_once("% of your ")?;
        let pct: u8 = pct
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| pct.parse().ok())
            .flatten()
            .filter(|&p| p <= 100)?;
        let (name, after) = rest.split_once(" \u{b7} ").unwrap_or((rest, ""));
        Some(Warning {
            window: Window::of_name(crate::text::trim_end(name))?,
            pct,
            reset: crate::phase::notice_reset(after),
        })
    })
}

/// The right segments of the rows a notification is drawn on, the footer's first.
fn segments(rows: &[String]) -> impl Iterator<Item = &str> {
    use crate::phase::{composer_frame, footer_end, status_block};
    use crate::wall::{in_queued_message, right_segment};
    let frame = composer_frame(rows);
    let width = frame.map_or(0, |f| {
        crate::text::trim_end(&rows[f.bottom]).chars().count()
    });
    let reaches = move |row: &str| row.chars().count() + 3 >= width;
    let footer = frame
        .map(|f| f.bottom + 1..footer_end(rows, f.bottom + 1))
        .into_iter()
        .flatten()
        .filter_map(move |i| {
            let row = crate::text::trim_end(&rows[i]);
            reaches(row).then(|| right_segment(row).1)
        });
    let zone = frame
        .map(|f| {
            let block = status_block(rows, f.top);
            let under = block.status.map_or(f.top, |s| s + 1);
            (under..f.top).filter_map(move |i| {
                let row = crate::text::trim_end(&rows[i]);
                let queued = block.status.is_some_and(|s| in_queued_message(rows, s, i));
                (reaches(row) && !queued).then(|| right_segment(row).1)
            })
        })
        .into_iter()
        .flatten();
    footer.chain(zone).filter(|segment| !segment.is_empty())
}

#[cfg(test)]
#[path = "credits_tests.rs"]
mod tests;
