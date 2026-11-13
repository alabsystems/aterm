// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A ROW THE POOL CARRIES UNDER A LABEL ALREADY TAKEN GETS AN ADMISSIBLE ONE: a 64-byte label
/// keeps its `-N` within [`accounts::LABEL_MAX`] by losing the end of its stem. CONTROLS: a
/// short label gets `-2`, then `-3` where `-2` is taken too, and an untaken one stays itself.
#[test]
fn a_long_label_that_collides_gets_an_admissible_one() {
    let long = "a".repeat(accounts::LABEL_MAX);
    assert!(accounts::admissible_label(&long));
    let next = untaken_label(&long, |l| l == long);
    assert!(
        accounts::admissible_label(&next),
        "{next:?} ({} bytes)",
        next.len()
    );
    assert_eq!(next, format!("{}-2", "a".repeat(accounts::LABEL_MAX - 2)));
    let tenth = untaken_label(&long, |l| {
        l == long || (2..10).any(|n| l.ends_with(&format!("-{n}")))
    });
    assert_eq!(tenth, format!("{}-10", "a".repeat(accounts::LABEL_MAX - 3)));
    assert!(accounts::admissible_label(&tenth), "{tenth:?}");

    assert_eq!(untaken_label("work", |l| l == "work"), "work-2", "CONTROL");
    assert_eq!(
        untaken_label("work", |l| l == "work" || l == "work-2"),
        "work-3",
        "CONTROL"
    );
    assert_eq!(untaken_label("work", |_| false), "work", "CONTROL");
}
