// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grok Build (`grok`, xAI) CLI harness version and screen grammar.
//!
//! WHAT IS MEASURED. Grok Build 1.0.70's signed-out screen (`fixtures/grok-1.0.70-signed-out.txt`,
//! a private headless aterm, a scratch `GROK_HOME`): the logo, `Approve in your browser to
//! finish signing in.`, the device code, `Waiting for approval...` and the `ctrl+q  quit` key
//! row, drawn instead of any composer. The limit and login-ended texts are the binary's own
//! strings, the same bytes in 1.0.67 and 1.0.70 ([`crate::anchors::GROK_ANCHORS`]); no signed-in
//! account has drawn them on a screen yet, so where they sit on one is not measured. The reader
//! therefore claims nothing it has not seen: every phase is `idle` NOT authoritatively (its busy
//! row and idle composer are unmeasured), and a wall is read only off a row that OPENS with one
//! of those texts in the screen's live zone, never off prose that quotes one.

use crate::anchors::anchor_text;
use crate::wall::{Placement, Wall, WallKind};

/// Major component of the Grok (`grok`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MAJOR: u16 = 0;

/// Minor component of the Grok (`grok`) harness version, starting at `0.1`.
pub const HARNESS_VERSION_MINOR: u16 = 1;

/// Version of the Grok (`grok`) harness, tracked independently per harness starting at `"0.1"`.
pub const HARNESS_VERSION: &str = "0.1";

/// Structured incremental numeric version of the Grok (`grok`) harness.
pub const HARNESS_VER: crate::HarnessVersion = crate::HarnessVersion::new(
    HARNESS_VERSION_MAJOR,
    HARNESS_VERSION_MINOR,
    HARNESS_VERSION,
);

/// How many of the screen's last drawn rows a wall row may stand in ([`crate::live_zone_start`]):
/// the notice a turn ended on, with room for a composer and key rows under it.
const LIVE_ROWS: usize = 12;

/// The texts a usage limit opens its row with: the TUI's error card (an ASCII apostrophe) and
/// the sampler's (a typographic one), each as the build carries it.
const LIMIT_OPENERS: [&str; 3] = [
    anchor_text("grok.wall.plan_rate_limit"),
    anchor_text("grok.wall.plan_rate_limit.sampler"),
    anchor_text("grok.wall.free_usage"),
];

/// The texts an ended login opens its row with.
const AUTH_OPENERS: [&str; 2] = [
    anchor_text("grok.auth.expired"),
    anchor_text("grok.auth.not_logged_in"),
];

/// The row Grok Build's sign-in screen asks the person to approve on, where the screen is that
/// screen (its approval line with `Waiting for approval...` under it), or `None`.
#[must_use]
pub fn sign_in_row(rows: &[String]) -> Option<usize> {
    let approve = rows
        .iter()
        .position(|r| crate::text::trim(r) == anchor_text("grok.signin.approve"))?;
    rows[approve + 1..]
        .iter()
        .any(|r| crate::text::trim(r) == anchor_text("grok.signin.waiting"))
        .then_some(approve)
}

/// Whether `rows` are Grok Build's own screen by what it alone draws: its sign-in screen over its
/// `ctrl+q  quit` key row ([`sign_in_row`]).
#[must_use]
pub fn is_grok_screen(rows: &[String]) -> bool {
    sign_in_row(rows).is_some_and(|at| {
        rows[at..]
            .iter()
            .any(|r| crate::text::trim(r) == anchor_text("grok.keys.quit"))
    })
}

/// The text of `row` past its indent and at most one leading mark (a glyph or a frame's edge
/// and the space after it), where the row's words start.
fn opening(row: &str) -> &str {
    let text = crate::text::trim(row);
    let mut chars = text.char_indices();
    match chars.next() {
        Some((_, c)) if !c.is_alphanumeric() => match chars.next() {
            Some((at, ' ')) => crate::text::trim(&text[at..]),
            _ => text,
        },
        _ => text,
    }
}

/// The wall `rows` show: the sign-in screen ([`sign_in_row`], [`WallKind::Auth`]), or a row in
/// the live zone that opens with a limit text ([`WallKind::UsageSession`]: Grok names no window)
/// or an ended login's ([`WallKind::Auth`]). A passing throttle (`The service is busy. …`) is
/// none, and so is any row whose words only quote one.
#[must_use]
pub fn wall(rows: &[String]) -> Option<Wall> {
    if let Some(at) = sign_in_row(rows) {
        return Some(Wall::sign_in(rows, at));
    }
    let from = crate::reader::live_zone_start(rows, LIVE_ROWS);
    (from..rows.len()).rev().find_map(|at| {
        let text = opening(&rows[at]);
        let kind = if LIMIT_OPENERS.iter().any(|w| text.starts_with(w)) {
            WallKind::UsageSession
        } else if AUTH_OPENERS.iter().any(|w| text.starts_with(w)) {
            WallKind::Auth
        } else {
            return None;
        };
        Some(Wall {
            kind,
            message: text.to_string(),
            reset: None,
            row: at,
            placement: Placement::ErrorRow,
            under_call: false,
        })
    })
}

#[cfg(test)]
#[path = "grok_tests.rs"]
mod tests;

/// Grok Build's screens, measured or hand-built (each file's head says which), for this crate's tests and a dependent crate's `#[cfg(test)]` code; not part of the documented API.
#[doc(hidden)]
pub mod fixtures {
    /// Grok Build 1.0.70 signed out, its device sign-in screen (MEASURED).
    pub const SIGNED_OUT: &str = include_str!("fixtures/grok-1.0.70-signed-out.txt");
    /// The plan rate limit's error card under a reply (HAND-BUILT: the text is the binary's).
    pub const WALL_PLAN_RATE_LIMIT: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-wall-plan-rate-limit.txt");
    /// A reply whose prose quotes `rate limit` (HAND-BUILT), which is no wall.
    pub const REPLY_QUOTES_RATE_LIMIT: &str =
        include_str!("fixtures/grok-1.0.70-hand-built-reply-quotes-rate-limit.txt");
}
