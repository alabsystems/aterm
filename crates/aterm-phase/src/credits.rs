// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! USAGE CREDITS, AND THE VENDOR'S OWN WORD THAT A WINDOW IS FILLING: what Claude Code says of the account's usage beside the composer, read for the record.

/// Which window a notice names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The five-hour window (`session limit`).
    Session,
    /// The seven-day window (`weekly limit`).
    Weekly,
}

impl Window {
    /// The vendor's limit name after `Your ` / `of your ` → its window.
    fn of_name(name: &str) -> Option<Window> {
        match name {
            "session limit" => Some(Window::Session),
            "weekly limit" => Some(Window::Weekly),
            _ => None,
        }
    }

    /// The word the CLI and the records print.
    #[must_use]
    pub fn name(self) -> &'static str {
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

/// Whether usage credits carry the session, by Claude Code's own notice (module docs): the lasting `Now using usage credits`, `Now using your usage allocation`, `Extra usage is now covering your requests` (the end of lower-priority mode), or the notice they started on — which wins where both show, for the window it names.
#[must_use]
pub fn in_use(rows: &[String]) -> Option<Credits> {
    let mut found: Option<Credits> = None;
    for segment in segments(rows) {
        for id in ["credits.started", "credits.allocation_started"] {
            if let Some(rest) = segment.strip_prefix(crate::anchors::anchor(id)) {
                return Some(Credits {
                    started: Some(started(rest)),
                    model: None,
                });
            }
        }
        if let Some(rest) = segment
            .strip_prefix(crate::anchors::anchor("credits.in_use"))
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
        if let Some(model) = ["credits.model_drawing", "credits.model_instead"]
            .iter()
            .find_map(|id| segment.strip_suffix(crate::anchors::anchor(id)))
            .and_then(model_name)
        {
            found = Some(Credits {
                started: None,
                model: Some(model),
            });
            continue;
        }
        let lasting = [
            "credits.in_use",
            "credits.allocation",
            "credits.extra_covering",
        ]
        .iter()
        .any(|id| segment.starts_with(crate::anchors::anchor(id)));
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

/// The vendor's warning that the session or the weekly window is filling (module docs): `You've used <n>% of your session limit · resets <time>`, or of the `weekly limit`.
#[must_use]
pub fn usage_warning(rows: &[String]) -> Option<Warning> {
    let lead = const { crate::anchors::anchor_text("usage.warning") };
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

/// The right segments of the rows a notification is drawn on (module docs),
/// the footer's first.
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
