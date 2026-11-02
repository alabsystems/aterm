// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A REPLY THE MODEL GAVE, NOT AN ERROR DRAWN AS ONE (v6 §14.17.5.4, slice 12c): a turn that
//! ended on a reply witnesses that the API answered its route only where the reply reads as the
//! model's ([`reads_as_error`] false). A program that draws an error where its replies go (Muse
//! Code's `◆`, or an error form no reader names, KI-1025-12's G7) would otherwise book the
//! outage's own words as the route answering, and every tab waiting on that route would be told
//! the outage cleared while it stands.

/// The words an error's first line opens with, as a program or the API prints it (`Error: …`,
/// `model failed: …`, `agent executor error: …`, `API error 503 …`, `Request failed …`), read
/// lower-cased with any leading glyph skipped. A reply of the model's that opens so (`Errors in
/// the build are fixed.`) is taken for an error too: it costs that route one witness, never a
/// line typed into a standing wall.
pub const ERROR_OPENINGS: [&str; 10] = [
    "error",
    "api error",
    "fatal",
    "failed",
    "failure",
    "exception",
    "panic",
    "model failed",
    "agent executor error",
    "request failed",
];

/// Whether `reply` (a program's last reply as its reader reads it) READS AS AN ERROR rather than
/// the model's answer: it is empty, its first line opens with an error's word
/// ([`ERROR_OPENINGS`]), or any of its lines opens as a wall the table names
/// ([`crate::classify_wall`]: an overload, a rate limit, an API error) or names the network
/// failing (`ECONNREFUSED`, `connection reset`, `timed out`).
#[must_use]
pub fn reads_as_error(reply: &str) -> bool {
    let mut lines = reply
        .lines()
        .map(|l| l.trim_start_matches(|c: char| !c.is_alphanumeric()))
        .filter(|l| !l.trim().is_empty());
    let Some(head) = lines.next() else {
        return true;
    };
    let lower = head.to_lowercase();
    ERROR_OPENINGS.iter().any(|w| lower.starts_with(w))
        || std::iter::once(head)
            .chain(lines)
            .any(|l| crate::wall::classify_wall(l).is_some() || crate::wall::is_unreachable(l))
}

#[cfg(test)]
#[path = "answered_tests.rs"]
mod tests;
