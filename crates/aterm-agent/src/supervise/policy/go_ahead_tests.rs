// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The go-ahead reader against `fixtures/go-ahead-table.txt`, and the park it hands the answer.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Park,
    Person,
    Plain,
}

/// `go-ahead-table.txt`'s rows, in order: each one's section and its reply.
fn table() -> Vec<(Row, String)> {
    let table = include_str!("fixtures/go-ahead-table.txt");
    let mut rows = Vec::new();
    let mut section = None;
    for line in table.lines() {
        match line {
            "== park" => section = Some(Row::Park),
            "== person" => section = Some(Row::Person),
            "== plain" => section = Some(Row::Plain),
            _ if line.starts_with('#') || line.trim().is_empty() => {}
            _ => rows.push((
                section.unwrap_or_else(|| panic!("a row before its section: {line:?}")),
                line.replace("\\n", "\n"),
            )),
        }
    }
    rows
}

/// `reply` as a screen of `columns` draws it: each line wrapped at a word behind a 2-column
/// gutter, so a sentence straddles rows.
fn wrapped(reply: &str, columns: usize) -> String {
    let width = columns - 2;
    let mut rows = Vec::new();
    for line in reply.lines() {
        let mut row = String::new();
        for word in line.split(' ') {
            if !row.is_empty() && row.chars().count() + 1 + word.chars().count() > width {
                rows.push(format!("  {row}"));
                row.clear();
            }
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(word);
        }
        rows.push(if row.is_empty() {
            String::new()
        } else {
            format!("  {row}")
        });
    }
    rows.join("\n")
}

/// The rows whose reading differs from their section, as written and wrapped at 80, 100 and 120
/// columns.
fn misread() -> Vec<String> {
    let mut wrong = Vec::new();
    for (row, reply) in table() {
        let drawn = [None, Some(80), Some(100), Some(120)];
        for columns in drawn {
            let text = columns.map_or_else(|| reply.clone(), |c| wrapped(&reply, c));
            if parked("", &text).is_some() != (row == Row::Park) {
                wrong.push(format!("{row:?} at {columns:?}: {reply:?}"));
            }
        }
    }
    wrong
}

#[test]
fn every_go_ahead_row_reads_as_its_section_however_it_wraps() {
    let rows = table();
    for row in [Row::Park, Row::Person, Row::Plain] {
        assert!(
            rows.iter().filter(|(r, _)| *r == row).count() >= 10,
            "the table keeps ten {row:?} rows at least"
        );
    }
    let wrong = misread();
    assert!(
        wrong.is_empty(),
        "{} misread:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}

fn dropping<T>(part: Part, read: impl FnOnce() -> T) -> T {
    DROPPED.with(|d| d.set(part as u16));
    let out = read();
    DROPPED.with(|d| d.set(0));
    out
}

#[test]
fn every_family_and_exclusion_holds_some_row() {
    let families = [
        Part::Consent,
        Part::Says,
        Part::Fixed,
        Part::Slots,
        Part::OwnInitiative,
        Part::StopNote,
    ];
    let exclusions = [
        Part::PersonOnly,
        Part::PersonFirst,
        Part::Outward,
        Part::SomeoneElse,
        Part::Declined,
        Part::Chooses,
        Part::Echoes,
        Part::Attributed,
    ];
    let rows = table();
    for part in families {
        let missed = dropping(part, || {
            rows.iter()
                .any(|(row, reply)| *row == Row::Park && parked("", reply).is_none())
        });
        assert!(
            missed,
            "dropping family {} leaves every park row parked",
            part as u16
        );
    }
    for part in exclusions {
        let parks = dropping(part, || {
            rows.iter()
                .any(|(row, reply)| *row != Row::Park && parked("", reply).is_some())
        });
        assert!(
            parks,
            "dropping exclusion {} parks no person or plain row",
            part as u16
        );
    }
}

/// The incident's last reply (2026-10-29), the one the owner read.
const INCIDENT: &str = "Nothing has changed. The build is waiting only for your go-ahead.\n\nNEEDS YOU: \
                        say \"rerun the checks\" so I can build and test the upgrade fix branch, push \
                        it to main and remove the worktree.";

#[test]
fn the_incident_reply_parks_on_the_word_it_asked_for() {
    let park = parked("", INCIDENT).expect("the incident parks");
    assert_eq!(park.token.as_deref(), Some("rerun the checks"));
    assert!(park.need && !park.others && !park.waits_room, "{park:?}");
    assert_eq!(park.mark, "rerun the checks");
    assert!(
        park.quote.starts_with("say \"rerun the checks\""),
        "{park:?}"
    );
    assert_eq!(
        park.said(800),
        "You asked the owner to say \"rerun the checks\": consider it said."
    );
}

#[test]
fn a_park_in_an_earlier_message_of_the_turn_parks() {
    let earlier = "NEEDS YOU: say \"rerun the checks\" so I can build and test the branch.";
    let park = parked(earlier, "Checked the branch again; nothing moved.").expect("parks");
    assert_eq!(park.token.as_deref(), Some("rerun the checks"));
    assert!(parked("", "Checked the branch again; nothing moved.").is_none());
}

#[test]
fn a_park_beside_a_persons_needs_keeps_them_and_names_room() {
    let first = "The fix isn't on main yet.\n\nNEEDS YOU: say \"rerun the checks\" when the Mac has \
                 memory to spare (the run on the merged branch was stopped for low memory and I'm \
                 not restarting it on my own).\nNEEDS YOU: sign agy and Muse in on this Mac \
                 (browser OAuth; run `aterm ai doctor` at a terminal).\nNEEDS YOU: decide on \
                 public hosting of the signed fleet document (outward-facing publish).";
    let park = parked("", first).expect("the go-ahead need parks");
    assert!(park.need && park.others && park.waits_room, "{park:?}");
    let prose = parked(
        "",
        "The build was stopped for low memory, so I won't start it again unless you ask.",
    )
    .expect("parks");
    assert!(
        prose.waits_room && !prose.need && !prose.others,
        "{prose:?}"
    );
    assert_eq!((prose.token, prose.mark.as_str()), (None, ""));
}

#[test]
fn a_park_is_quoted_without_hidden_characters_and_fits_its_room() {
    let hidden =
        "NEEDS YOU: say \"rerun\u{200B} the\u{202E} checks 42\" so I can rebuild the branch.";
    let park = parked("", hidden).expect("parks");
    let token = park.token.as_deref().expect("a token");
    assert!(
        !token.chars().any(aterm_messages::text::is_hidden_char),
        "{token:?}"
    );
    assert_eq!(park.mark, "rerun the checks ##");
    assert!(!park.quote.chars().any(aterm_messages::text::is_hidden_char));
    let long = format!(
        "The build is waiting for your go-ahead, {}.",
        "and the rebuild after it ".repeat(12)
    );
    let park = parked("", &long).expect("parks");
    assert!(park.quote.encode_utf16().count() <= QUOTE_UNITS && park.quote.ends_with('…'));
    for room in [0, 10, 40, 80, 200] {
        let said = park.said(room);
        assert!(said.encode_utf16().count() <= room, "{room}: {said:?}");
    }
    assert!(park.said(17).is_empty());
    assert!(
        park.said(60)
            .starts_with("You parked on: \"The build is waiting")
    );
}

#[test]
fn the_answer_parks_nothing() {
    for text in [
        GO_AHEAD_ANSWER,
        GO_AHEAD_ROOM,
        GO_AHEAD_DROP,
        GO_AHEAD_OTHERS,
    ] {
        assert!(parked("", text).is_none(), "{text}");
        assert!(!parks(text), "{text}");
    }
}

#[test]
fn an_outward_plan_anywhere_in_the_turn_parks_nothing() {
    let plan = "Plan: delete the stale release tags on the remote, then push my branch to main. \
                Tests are green.";
    let ask = "Waiting for your go-ahead to push.";
    assert!(parked(plan, ask).is_none());
    assert!(parked("Tests are green.", ask).is_some());
    let tagged = "Tagged v1.4.0; the next push runs the crates.io publish.";
    assert!(parked(tagged, "NEEDS YOU: say go so I can push.").is_none());
}
