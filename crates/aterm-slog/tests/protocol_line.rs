// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use alab_policy::{land_check, needs};
use aterm_slog::protocol::protocol_line;

/// Each of the four heads the turn-end readers read is a protocol line: a need, a defense, a
/// reason the work ends unfinished, and the land check's answer, read as Land reads it (its NEEDS
/// YOU lines aside), and a NEEDS YOU head that says none stands, which the readers take as the
/// session withdrawing its needs. CONTROL: a sentence that mentions each head without the shape
/// its reader reads is not one, so the predicate is the readers' and no list of words.
#[test]
fn protocol_line_reads_each_of_the_four_heads() {
    for head in [
        "NEEDS YOU: sign in to the vault",
        "Nothing left: every change is on main",
        "Could not finish: the Windows cell is red",
        "LANDED",
        "LANDED. NEEDS YOU: none",
        "NEEDS YOU: none",
    ] {
        assert!(protocol_line(head), "{head:?} is read as a head");
    }
    assert_eq!(
        land_check::land_check_answer("LANDED. NEEDS YOU: none", None, false),
        land_check::CheckAnswer::Landed,
        "Land reads the word beside a cleared head as its answer"
    );
    assert!(
        needs::retracts("NEEDS YOU: none"),
        "the readers take a cleared head as the session withdrawing its needs"
    );
    for mention in [
        "a NEEDS YOU line names what only a person can give",
        "a defense opens with the words Nothing left and a colon",
        "the worker could not finish the Windows cell yesterday",
        "landed the retry fix; the Windows cell is next",
        "no NEEDS YOU stands on this tab",
    ] {
        assert!(!protocol_line(mention), "CONTROL: {mention:?} is no head");
    }
}
