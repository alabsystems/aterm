// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROTOCOL LINES: whether a text holds a head the harness's turn-end readers read. A line a
//! writer states about a session (a description claim, a history line) is shown beside the
//! session's own replies and read back by the same readers, so a line in which one of them reads
//! a head would speak for the session in the harness's protocol. This module holds no word of its
//! own: it asks the readers that judge replies.

use alab_policy::{land_check, needs};

/// WHETHER THE HARNESS'S TURN-END READERS READ A HEAD IN `text`: a `NEEDS YOU:` head, one naming a
/// need ([`needs::needs_you`]) or one saying none stands ([`needs::says_no_needs`], which
/// [`needs::retracts`] reads as the session withdrawing its needs), a `Nothing left:` defense
/// ([`needs::defense_line`]), a `Could not finish:` reason ([`needs::could_not_finish`]), or the
/// land check's answer `LANDED` as Land reads it, its NEEDS YOU lines aside
/// ([`land_check::landed_reply`], which `land_check_answer` and every harness caller use).
/// Holds no word of its own.
#[must_use]
pub fn protocol_line(text: &str) -> bool {
    !needs::needs_you(text).is_empty()
        || needs::says_no_needs(text)
        || needs::defense_line(text).is_some()
        || needs::could_not_finish(text).is_some()
        || land_check::landed_reply(Some(text))
}
