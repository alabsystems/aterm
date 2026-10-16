// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! USAGE CREDITS, AND THE VENDOR'S OWN WORD THAT A WINDOW IS FILLING: what
//! Claude Code says of the account's usage beside the composer, read FOR THE
//! RECORD. Neither is a wall and neither decides a turn's end: with credits
//! on, a turn that crosses a window's limit goes on, paid for, and the screen
//! says only `Now using usage credits` (lab E4; the accounts design §3.3).
//!
//! Measured on 2.1.300 in the real-Claude lab (`targo --unverified lab`,
//! crates/aterm-lab; the stub answering every request with the five-hour
//! window rejected and credits allowed and in use; the fixtures
//! `claude-2.1.300-credits-*`), the notices are Claude Code's notifications,
//! drawn right-aligned:
//! * the inline renderer draws them under the composer's bottom rule, on the
//!   mode row's right (`⏵⏵ auto mode on (shift+tab to cycle) · ← for agents
//!   Now using usage credits`) or on a row of their own;
//! * the fullscreen renderer (`CLAUDE_CODE_NO_FLICKER=1`) draws them in the
//!   live zone, on the row right above the composer's top rule;
//! * as credits start, for about ten seconds, `You're now using usage credits
//!   · Your session limit resets 9:43am (America/Los_Angeles)` (cut with `…`
//!   at 80 columns), then the lasting `Now using usage credits`.
//!
//! The vendor's warning is drawn the same way: `You've used 80% of your
//! weekly limit · resets Oct 18 at 8:50pm (America/Los_Angeles)` (measured;
//! its thresholds are the bundle's: the five-hour window at 90% while at most
//! 72% of it has passed, the week at 75%, 50% and 25% while at most 60%, 35%
//! and 15% of it has).
//!
//! Each is read by PLACE: the RIGHT SEGMENT of a row — the text after its last
//! run of two or more blanks, or the whole row — that reaches the right edge
//! (within three columns of the composer's rules), on a footer row, or on a
//! row of the live zone under the status row that no message a person queued
//! there holds; the segment OPENS with the vendor's words ([`crate::anchors`]'
//! `credits.*`, `usage.warning`). A tool's output and the model's words are
//! the transcript's, above the status row; with no status row nothing above
//! the composer is read. What can still print the words in place is a
//! statusLine command of the person's own, at the right edge: a wrong record
//! at most.

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
    /// The notice that they have just STARTED (`You're now using usage
    /// credits · Your session limit resets 9:43am (…)`), with the window it
    /// names, if any, and its reset as printed; `None` for the lasting `Now
    /// using usage credits`, which names neither.
    pub started: Option<Started>,
}

/// The window credits started past, as their first notice names it.
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

/// Whether usage credits carry the session, by Claude Code's own notice
/// (module docs): the lasting `Now using usage credits` (`Now using usage
/// credits for Opus · …` too), `Now using your usage allocation`, or the
/// notice they started on — which wins where both show, for the window it
/// names. `None` with no such notice, and with no composer frame (a box, a
/// shell).
#[must_use]
pub fn in_use(rows: &[String]) -> Option<Credits> {
    let mut lasting = false;
    for segment in segments(rows) {
        for id in ["credits.started", "credits.allocation_started"] {
            if let Some(rest) = segment.strip_prefix(crate::anchors::anchor(id)) {
                return Some(Credits {
                    started: Some(started(rest)),
                });
            }
        }
        lasting |= ["credits.in_use", "credits.allocation"]
            .iter()
            .any(|id| segment.starts_with(crate::anchors::anchor(id)));
    }
    lasting.then_some(Credits { started: None })
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

/// The vendor's warning that the session or the weekly window is filling
/// (module docs): `You've used <n>% of your session limit · resets <time>`, or
/// of the `weekly limit`. `None` for none, for a model's or the credits' own
/// (`of your Opus limit`, `of your usage credits`), and with no composer frame.
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
