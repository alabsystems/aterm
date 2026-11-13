// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn workers_and_managers_wear_their_marks_and_a_plain_strip_borrows() {
    let worker = TabTeam {
        worker: true,
        workers: 0,
    };
    let manager = TabTeam {
        worker: false,
        workers: 3,
    };
    assert!(team_title("bench", worker).starts_with('\u{21b3}'));
    assert!(team_title("lab-sweep", manager).ends_with("\u{21c9}3"));
    let both = team_title(
        "mid",
        TabTeam {
            worker: true,
            workers: 2,
        },
    );
    assert!(both.starts_with('\u{21b3}') && both.ends_with("\u{21c9}2"));
    assert!(matches!(
        team_title("plain", TabTeam::NONE),
        std::borrow::Cow::Borrowed("plain")
    ));
    let titles = vec!["a".to_string(), "b".to_string()];
    assert!(matches!(
        team_titles(&titles, &[PLAIN_TAB; 2]),
        std::borrow::Cow::Borrowed(_)
    ));
    let marked = team_titles(
        &titles,
        &[
            PLAIN_TAB,
            TabStripMetadata {
                team: worker,
                ..PLAIN_TAB
            },
        ],
    );
    assert_eq!(marked[0], "a");
    assert!(marked[1].starts_with('\u{21b3}'));
}
