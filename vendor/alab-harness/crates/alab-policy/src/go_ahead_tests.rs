// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The go-ahead reader against `fixtures/go-ahead-table.txt`, and the park it hands the answer.

use super::*;
use crate::corpus::go_ahead::{
    DRAWN, ECHOES, INCIDENT_TURN, REPROS, Row, drawn, guard_corpus, rendered, table,
};
use crate::words::Said;

/// The go-ahead table ([`table`] and [`guard_corpus`] read it).
const GO_AHEAD_TABLE: &str = include_str!("../fixtures/go-ahead-table.txt");

/// THE INCIDENT'S TURN PARKS AS THE SCREEN DRAWS IT (2026-10-29 review 5): the agent's screen draws
/// inline code bare, so `for_launch` and `ship/release.rs` in an earlier message read as prose;
/// an identifier or a path names no outward act the go-ahead would be read as covering, so the
/// turn parks, as written and drawn bare at every width.
#[test]
fn the_incidents_turn_drawn_bare_parks_whatever_code_its_messages_name() {
    let (earlier, tail) = INCIDENT_TURN;
    let mut wrong = Vec::new();
    for bare in [false, true] {
        let draw = |t: &str| if bare { rendered(t) } else { t.to_string() };
        for columns in DRAWN {
            let said: Vec<String> = earlier.iter().map(|m| drawn(&draw(m), columns)).collect();
            let park = parked(&said.join("\n\n"), &drawn(&draw(tail), columns));
            if park.as_ref().and_then(|p| p.token.as_deref()) != Some("rerun the checks") {
                wrong.push(format!("bare {bare} at {columns:?}: {park:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // An outward act an earlier message names in its words, not in an identifier, still parks
    // nothing (the control).
    let plan = drawn(
        &rendered("Next I'll tag the release and launch it."),
        Some(80),
    );
    assert!(parked(&plan, tail).is_none());
}

/// The rows whose reading differs from their section, at every width of [`DRAWN`], as written
/// and, for a park or a person's need, as the agent's screen renders it ([`rendered`]): its code drawn
/// bare. A plain row that quotes a park in code reads as that park once drawn bare
/// (ATERM-KI-1020-2), and gets the go-ahead, which limits itself.
fn misread() -> Vec<String> {
    let mut wrong = Vec::new();
    for (row, reply) in table(GO_AHEAD_TABLE) {
        let bares: &[bool] = if row == Row::Plain {
            &[false]
        } else {
            &[false, true]
        };
        for &bare in bares {
            let reply = if bare {
                rendered(&reply)
            } else {
                reply.clone()
            };
            for columns in DRAWN {
                let text = drawn(&reply, columns);
                if parked("", &text).is_some() != (row == Row::Park) {
                    wrong.push(format!("{row:?} at {columns:?}, bare {bare}: {reply:?}"));
                }
            }
        }
    }
    wrong
}

#[test]
fn every_go_ahead_row_reads_as_its_section_however_it_wraps() {
    let rows = table(GO_AHEAD_TABLE);
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

/// Ending a process is irreversible, so a park whose work runs `aterm ctl end` (in its words or
/// in the code it carries), or ends a process in plain words (`end`, `stop` or `shut down`, in
/// any form, before a process noun), gets no standing go-ahead, as one that would `kill` gets
/// none.
/// NEGATIVE CONTROL: drop `ctl end` from the outward phrases, and the first two park; drop the
/// process nouns, and the plain ones park; read `end` alone, and `ending`, `stop` and `shut down`
/// park. The same park without the end gets the go-ahead, and so does one that names the end of
/// a run or stops something that is no process.
#[test]
fn a_question_about_ending_a_process_gets_no_go_ahead() {
    for reply in [
        "NEEDS YOU: say \"go\" so I can end the leftover with `aterm ctl end \
         2606@1792098840004410:331` and rerun the checks.",
        "Waiting for your go-ahead to run aterm ctl end 2606@1792098840004410:331 and rebuild.",
        "NEEDS YOU: say \"go\" so I can end the leftover processes and rerun the checks.",
        "Waiting for your go-ahead to end my stale dev server before I rebuild.",
        "Waiting for your go-ahead before ending the leftover dev server and rebuilding.",
        "NEEDS YOU: say \"go\" so I can stop the preview server and rerun the checks.",
        "Waiting for your go-ahead to shut down the docs watcher before I rebuild.",
        "Waiting for your go-ahead to shutdown the dev server and rebuild.",
    ] {
        for text in [reply.to_string(), rendered(reply)] {
            assert!(parked("", &text).is_none(), "{text:?}");
        }
    }
    for reply in [
        "NEEDS YOU: say \"go\" so I can rerun the checks.",
        "NEEDS YOU: say \"go\" so I can rerun the checks at the end of the job.",
        "NEEDS YOU: say \"go\" so I can stop the build and rerun the checks.",
    ] {
        assert!(parked("", reply).is_some(), "the control parks: {reply:?}");
    }
}

#[test]
fn uppercase_owners_keep_their_work_outside_the_standing_go_ahead() {
    // Establish own work, acronym/deadline prose and the existing S2/S7 boundaries before
    // the regression cases. These run through the real reader without typing any answer.
    for reply in [
        "NEEDS YOU: say \"go\" so I can rebuild my branch.",
        "NEEDS YOU: say \"go\" so I can rebuild the branch I made.",
        "WAITING FOR YOUR APPROVAL TO MERGE MY BRANCH TO MAIN.",
        "NEEDS YOU: the go-ahead by EOD so I can merge my branch into main.",
        "NEEDS YOU: your go-ahead by Friday so I can merge my branch into main.",
        "NEEDS YOU: the go-ahead by May so I can merge my branch into main.",
        "NEEDS YOU: the go-ahead by MAY so I can rebuild my branch.",
        "NEEDS YOU: your go-ahead by Monday so I can rebuild my branch.",
        "Waiting for your go-ahead from Slack before I merge my branch to main.",
        "The census check now flags a token in a log line.\n\nNEEDS YOU: say \"go\" so I can rerun the checks and commit.",
    ] {
        assert!(parked("", reply).is_some(), "own work still parks: {reply}");
    }
    for reply in [
        "NEEDS YOU: approval from the security team so I can merge my branch into main.",
        "The build needs your GitHub token, which I don't have.\n\nNEEDS YOU: say \"go\" so I can rebuild the branch.",
    ] {
        assert!(
            parked("", reply).is_none(),
            "the boundary still holds: {reply}"
        );
    }

    let mut wrong = Vec::new();
    for reply in [
        "NEEDS YOU: say \"go\" so I can rebuild ALICE's branch.",
        "NEEDS YOU: say \"go\" so I can rebuild the branch ALICE made.",
        "NEEDS YOU: ALICE's approval so I can rebuild my branch.",
        "NEEDS YOU: say \"go\" so I can rebuild MAY's branch.",
        "NEEDS YOU: say \"go\" so I can rebuild the branch MAY made.",
    ] {
        for columns in DRAWN {
            let text = drawn(reply, columns);
            if let Some(park) = parked("", &text) {
                wrong.push(format!("{columns:?}: {text:?} became {park:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn bot_branches_keep_their_work_outside_the_standing_go_ahead() {
    let mut wrong = Vec::new();
    for reply in [
        "I'll merge renovate/lodash-4.x into main once you say so.",
        "I'll merge `renovate/lodash-4.x` into main once you say so.",
        "NEEDS YOU: say \"go\" so I can merge origin/dependabot/cargo/serde-1.0.210 into main.",
        "Waiting for your go-ahead to merge snyk-fix-abc123 into main.",
        "Waiting for your go-ahead to merge refs/heads/github-actions/bump-ci into main.",
    ] {
        assert!(
            dropping(Part::SomeoneElse, || parked("", reply)).is_some(),
            "negative control: dropping SomeoneElse parks {reply:?}"
        );
        for bare in [false, true] {
            let reply = if bare {
                rendered(reply)
            } else {
                reply.to_string()
            };
            for columns in DRAWN {
                let text = drawn(&reply, columns);
                if let Some(park) = parked("", &text) {
                    wrong.push(format!(
                        "bare {bare} at {columns:?}: {text:?} became {park:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));

    for own in [
        "I'll merge fix/lodash-4.x into main once you say so.",
        "I left renovate/lodash-4.x alone.\n\nNEEDS YOU: say \"go\" so I can rebuild my branch.",
    ] {
        assert!(parked("", own).is_some(), "own work still parks: {own}");
    }
}

#[test]
fn bare_code_file_names_in_earlier_prose_plan_nothing_outward() {
    let tail = "NEEDS YOU: say \"rerun the checks\" so I can rebuild and push my branch to main.";
    let mut wrong = Vec::new();
    for earlier in [
        "Main's change touches `release.rs`, which my branch doesn't.",
        "Checked `release.rs's` imports; my branch leaves it alone.",
        "Updated (deploy.toml) and `launch.ts` on my branch.",
    ] {
        for bare in [false, true] {
            let draw = |t: &str| if bare { rendered(t) } else { t.to_string() };
            for columns in DRAWN {
                let park = parked(
                    &drawn(&draw(earlier), columns),
                    &drawn(&draw(tail), columns),
                );
                if park.as_ref().and_then(|p| p.token.as_deref()) != Some("rerun the checks") {
                    wrong.push(format!(
                        "bare {bare} at {columns:?} ({earlier:?}): {park:?}"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));

    for outward_plan in [
        "Tagged v1.4.0 for the crates.io release.",
        "Next I'll push the build to docs.rs.",
        "Next I'll tag the release.",
    ] {
        assert!(
            parked(outward_plan, tail).is_none(),
            "outward plan still blocks: {outward_plan}"
        );
    }
    assert!(
        parked("", "Waiting for your go-ahead to run deploy.sh.").is_none(),
        "a park whose own work runs deploy.sh still gets no go-ahead"
    );
}

/// Replies of each kind: ones that park, ones that park nothing, and one last message after
/// earlier messages that change its reading.
const REMEMBERED_READS: [(&str, &str); 6] = [
    ("", "NEEDS YOU: say \"go\" so I can rebuild the branch."),
    ("", "Should I rewrite the parser or patch the lexer?"),
    ("", "Stage done."),
    (
        "Stage done.",
        "NEEDS YOU: say \"go\" so I can rebuild the branch.",
    ),
    (
        "NEEDS YOU: say \"go\" so I can rebuild the branch.",
        "Stage done.",
    ),
    (
        "Plan: delete the tags, then push.",
        "NEEDS YOU: say \"go\" so I can rebuild the branch.",
    ),
];

/// A remembered reading is the words' own: asked again in any order, past what the memory holds,
/// each reply reads as it does afresh.
#[test]
fn a_remembered_reading_is_the_one_its_words_read_afresh() {
    let fresh: Vec<_> = REMEMBERED_READS
        .iter()
        .map(|(said, tail)| read_park(said, tail))
        .collect();
    assert!(
        fresh.iter().any(Option::is_some) && fresh.iter().any(Option::is_none),
        "the table holds a reply of each kind"
    );
    assert_ne!(
        fresh[0], fresh[5],
        "one last message reads otherwise after a plan, so the earlier messages are in the key"
    );
    for round in 0..3 {
        for (i, (said, tail)) in REMEMBERED_READS.iter().enumerate() {
            assert_eq!(
                parked(said, tail),
                fresh[i],
                "round {round}: {said:?} {tail:?}"
            );
            assert_eq!(
                parked(said, tail),
                fresh[i],
                "again {round}: {said:?} {tail:?}"
            );
        }
    }
    REMEMBERED.with_borrow(|known| {
        assert_eq!(known.len(), REMEMBERED_REPLIES, "the memory stays bounded");
    });
}

/// A reply past the bound is read and never kept. The bound counts bytes, so the bytes past it are
/// blank and no sentence of them is read: all that is read is the need, in all three shapes. What
/// that gives up: nothing in the policy's tests now reads one sentence past about 1 KB, where the
/// filler of 52,428 words did, so a panic or a quadratic reader at that scale shows nowhere (the
/// old filler asserted nothing about speed, and could only have hung or panicked).
#[test]
fn a_reply_past_the_bound_is_read_and_not_remembered() {
    let long = format!(
        "NEEDS YOU: say \"go\" so I can rebuild the branch.\n\n{}",
        " ".repeat(REMEMBERED_REPLY_BYTES)
    );
    REMEMBERED.with_borrow_mut(std::collections::VecDeque::clear);
    let half = &long[..long.len() / 2];
    for (said, tail) in [("", long.as_str()), (long.as_str(), ""), (half, half)] {
        let read = read_park(said, tail);
        assert!(read.is_some(), "the need parks past the bound");
        assert_eq!(parked(said, tail), read);
        REMEMBERED.with_borrow(|known| assert!(known.is_empty(), "{} kept", known.len()));
    }
}

/// A second look at a reply is answered by the memory, not read again: the reading kept in it is
/// the one that comes back. Nothing else tells a memory that answers from one that never does.
#[test]
fn a_second_look_is_answered_from_the_memory() {
    let (said, tail) = ("", "NEEDS YOU: say \"go\" so I can rebuild the branch.");
    REMEMBERED.with_borrow_mut(std::collections::VecDeque::clear);
    assert!(parked(said, tail).is_some(), "the reply parks");
    REMEMBERED.with_borrow_mut(|known| known[0].2 = None);
    assert_eq!(parked(said, tail), None, "the kept reading answers");
    assert_eq!(
        REMEMBERED.with_borrow(std::collections::VecDeque::len),
        1,
        "and is not kept twice"
    );
}

/// A line asked about alone never pushes out the reply being looked at.
#[test]
fn a_line_asked_about_alone_is_not_remembered() {
    REMEMBERED.with_borrow_mut(std::collections::VecDeque::clear);
    assert!(parks("NEEDS YOU: say \"go\" so I can rebuild the branch."));
    REMEMBERED.with_borrow(|known| assert!(known.is_empty(), "{} kept", known.len()));
}

/// A reader with a part dropped reads otherwise than the words alone would: it never answers from
/// the memory, and never fills it.
#[test]
fn a_dropped_part_is_read_past_the_memory() {
    let (said, tail) = ("", "NEEDS YOU: say \"go\" so I can rebuild the branch.");
    REMEMBERED.with_borrow_mut(std::collections::VecDeque::clear);
    let whole = parked(said, tail);
    assert!(whole.is_some(), "the reply parks");
    assert_ne!(
        dropping(Part::Says, || parked(said, tail)),
        whole,
        "dropping what the reply parks by changes the reading"
    );
    REMEMBERED.with_borrow(|known| assert_eq!(known.len(), 1, "only the whole reading is kept"));
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
    let rows = table(GO_AHEAD_TABLE);
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
    assert!(
        park.quote.starts_with("say \"rerun the checks\""),
        "{park:?}"
    );
    assert_eq!(
        park.said(800),
        format!("{TOKEN_OPEN}rerun the checks{TOKEN_CLOSE}")
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
                 not restarting it on my own).\nNEEDS YOU: sign both agent apps in on this Mac \
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
    assert_eq!(prose.token, None);
}

#[test]
fn a_park_is_quoted_without_hidden_characters_and_fits_its_room() {
    let hidden =
        "NEEDS YOU: say \"rerun\u{200B} the\u{202E} checks 42\" so I can rebuild the branch.";
    let park = parked("", hidden).expect("parks");
    let token = park.token.as_deref().expect("a token");
    assert!(!token.chars().any(crate::text::is_hidden_char), "{token:?}");
    assert!(!park.quote.chars().any(crate::text::is_hidden_char));
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

/// WRITE THE GUARD'S CORPUS for `corpus/guard_base.rs` to read at 246d1ddc2f: one escaped text a
/// line, into the file `GA_CORPUS` names.
#[test]
#[ignore = "writes the corpus corpus/guard_base.rs reads at 246d1ddc2f"]
fn write_guard_corpus() {
    use crate::corpus::guard_base::escaped;
    #[expect(
        clippy::disallowed_methods,
        reason = "an ignored writer of the guard's corpus: GA_CORPUS names the file it writes; the readers read no environment"
    )]
    let path = std::env::var("GA_CORPUS").expect("GA_CORPUS is set");
    let lines: Vec<String> = guard_corpus(GO_AHEAD_TABLE)
        .iter()
        .map(|t| escaped(t))
        .collect();
    #[expect(
        clippy::disallowed_methods,
        reason = "an ignored writer of the guard's corpus writes the file GA_CORPUS names; the readers write no file"
    )]
    std::fs::write(path, lines.join("\n") + "\n").expect("the corpus writes");
}

/// A reading's strength in the irreversible guard: an irreversible ask over a stop over the rest.
pub(crate) fn strength(said: &Said) -> u8 {
    match said {
        Said::Irreversible(_) => 2,
        Said::Stop(_) => 1,
        Said::Question | Said::Offer | Said::Plain => 0,
    }
}

/// The guard's reading at 246d1ddc2f, before the go-ahead reader, as [`strength`] ranks it: a
/// stop phrase anywhere in the text stops, and beside a destructive word anywhere it is an
/// irreversible ask.
pub(crate) fn base_strength(text: &str) -> u8 {
    use crate::words::{STOP_PHRASES, destructive_word};
    let flat = text
        .to_lowercase()
        .replace('’', "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !STOP_PHRASES.iter().any(|p| flat.contains(p)) {
        0
    } else if destructive_word(&flat).is_some() {
        2
    } else {
        1
    }
}

/// THE IRREVERSIBLE GUARD IS NEVER WEAKER THAN THE BASE (2026-10-29 review 3): every table row at
/// every width, every review's repro and every echo reads at least as strong as the base read
/// it, whatever the go-ahead reader makes of it.
#[test]
fn no_reading_is_weaker_than_the_base_guard() {
    use crate::words::classify_said;
    let mut texts: Vec<String> = Vec::new();
    for (_, reply) in table(GO_AHEAD_TABLE) {
        texts.extend(DRAWN.iter().map(|&c| drawn(&reply, c)));
    }
    texts.extend(REPROS.iter().chain(ECHOES).map(|t| (*t).to_string()));
    let weaker: Vec<String> = texts
        .iter()
        .filter_map(|text| {
            let read = classify_said(Some(text));
            (strength(&read) < base_strength(text)).then(|| format!("{text:?}: {read:?}"))
        })
        .collect();
    assert!(weaker.is_empty(), "{}", weaker.join("\n"));
    // The base reads each kind: the control that the corpus holds irreversible asks and stops.
    for want in [1, 2] {
        assert!(
            texts.iter().any(|t| base_strength(t) == want),
            "no text the base reads at {want}"
        );
    }
}

/// THE ECHOES PARK NOTHING: an agent acting on the go-ahead, or reporting it, gets no second one.
#[test]
fn an_echo_or_a_report_of_the_go_ahead_parks_nothing() {
    for text in ECHOES {
        assert!(parked("", text).is_none(), "parks: {text}");
    }
}

/// A WORD ANOTHER NEED ASKS FOR AN ACT THE GO-AHEAD DOES NOT COVER IS NEVER SAID (2026-10-29
/// review 3): the reply's own reversible need parks, and the answer quotes it rather than say
/// `go`, which the other need asks for a deletion on the remote.
#[test]
fn a_word_another_need_asks_for_an_excluded_act_is_never_said() {
    let two = "NEEDS YOU: say go so I can rerun the checks.\nNEEDS YOU: say go so I can delete the \
               stale release tags on the remote.";
    let park = parked("", two).expect("the reversible need parks");
    assert_eq!(park.token, None, "{park:?}");
    assert!(park.others, "{park:?}");
    let said = park.said(800);
    assert!(
        said.starts_with(QUOTE_OPEN) && !said.contains(TOKEN_CLOSE),
        "{said}"
    );
    // Alone, the reversible need's word is said.
    let one = parked("", "NEEDS YOU: say go so I can rerun the checks.").expect("parks");
    assert_eq!(one.token.as_deref(), Some("go"));
    assert!(one.said(800).contains(TOKEN_CLOSE));
}

/// THE LINES A PARK'S GO-AHEAD ANSWERS ARE ITS OWN, AS THE MENU BAR NAMES THEM (§15.6): the
/// table's first park reply names the go-ahead's line beside a sign-in and a publish decision,
/// and only the go-ahead's line is the park's, cleaned as `needs_you` writes it; the incident's
/// reply is its one line at every width; a park in prose answers no line; a turn that plans an
/// outward act parks nothing. Where a parking need took rows only the go-ahead's view joins (a
/// text of the guard's corpus wrapped mid-clause), the line every other reader reads (`needs_you`
/// of the drawn rows, the need's words before those rows) is the park's too.
#[test]
fn a_parks_needs_are_its_go_ahead_lines_as_the_menu_bar_writes_them() {
    let first = table(GO_AHEAD_TABLE)
        .into_iter()
        .find(|(row, reply)| *row == Row::Park && reply.contains("browser OAuth"))
        .map(|(_, reply)| reply)
        .expect("the table's first park reply");
    let lines = needs::needs_you(&first);
    assert_eq!(lines.len(), 3, "{lines:#?}");
    let park = parked("", &first).expect("parks");
    assert_eq!(park.needs, [lines[0].clone()], "{park:?}");
    assert!(
        lines[0].starts_with("say \"rerun the checks\""),
        "{lines:#?}"
    );
    let park = parked("", INCIDENT).expect("the incident parks");
    assert_eq!(park.needs, needs::needs_you(INCIDENT));
    let prose = parked(
        "",
        "The build was stopped for low memory, so I won't start it again unless you ask.",
    )
    .expect("parks");
    assert!(prose.needs.is_empty(), "{prose:?}");
    assert!(
        parked(
            "Plan: delete the stale release tags on the remote, then push my branch to main.",
            "NEEDS YOU: say \"rerun the checks\" so I can push my branch to main.",
        )
        .is_none()
    );
    for columns in DRAWN {
        let text = drawn(INCIDENT, columns);
        let park = parked("", &text).expect("parks at every width");
        assert_eq!(park.needs, needs::needs_you(&text), "{columns:?}");
    }
    let mut moved = 0;
    for text in guard_corpus(GO_AHEAD_TABLE) {
        let Some(park) = parked("", &text) else {
            continue;
        };
        let lines = needs::needs_you(&text);
        for m in needs::go_ahead_view(&text).moved {
            if !park.needs.contains(&needs::sanitized(&m.need)) {
                continue;
            }
            moved += 1;
            let base = needs::sanitized(&m.base);
            assert!(
                park.needs.contains(&base) && lines.contains(&base),
                "{text:?}: {base:?} {park:?} {lines:#?}"
            );
        }
    }
    assert!(
        moved > 0,
        "some parking need takes a row only the go-ahead's view joins"
    );
}

/// A PERSON'S NEED THAT CLEANS TO A LINE THE PARK'S NEED CLEANS TO IS NEVER ANSWERED (2026-10-30
/// review 9): the menu bar matches lines by their cleaned words, so where `needs_you` reads two
/// needs as one line, that line is a person's too. Two ways: the agent's own newline after a
/// clause's `so` (the base reads each need's first row alone, the go-ahead's view joins the rest),
/// and two needs past `NEED_CHARS` that differ only after its cut. Either way the reversible need
/// still parks, and its own joined words stay the park's.
#[test]
fn a_line_a_persons_need_cleans_to_is_never_answered() {
    let apart = "NEEDS YOU: say \"go\" when you're back so\nI can rebuild and push my branch to \
                 main.\nNEEDS YOU: say \"go\" when you're back so\nI can use your App Store \
                 Connect key for the upload.";
    let head = "say \"go\" when you're back so".to_string();
    assert_eq!(needs::needs_you(apart), std::slice::from_ref(&head));
    let park = parked("", apart).expect("the reversible need parks");
    assert!(park.others, "{park:?}");
    assert!(!park.needs.contains(&head), "{park:?}");
    assert!(
        park.needs.contains(
            &"say \"go\" when you're back so I can rebuild and push my branch to main.".to_string()
        ),
        "{park:?}"
    );
    let words = "say \"rerun the checks\" so I can build and test the parser branch after the \
                 rebase, compare its timings with the ones on main, rebuild the docs and the \
                 changelog from it, and then";
    let long = format!(
        "NEEDS YOU: {words} push the branch to main.\nNEEDS YOU: {words} sign the build with your \
         Apple ID password."
    );
    let cut = needs::needs_you(&long);
    assert_eq!(cut.len(), 1, "one line past the cut: {cut:#?}");
    let park = parked("", &long).expect("the reversible need parks");
    assert!(park.others, "{park:?}");
    assert!(!park.needs.contains(&cut[0]), "{park:?}");
}

/// NO PERSON'S LINE IS EVER ONE THE GO-AHEAD ANSWERS (§15.6): every park row of the table that
/// names a `NEEDS YOU:` line, joined with every person row that names one, as written and drawn
/// at every width: where the joined reply parks, none of the person's lines is the park's, and
/// as written each of the park's lines is one the menu bar names. The control: with the
/// person-only exclusion dropped, some person's line is the park's.
#[test]
fn no_persons_line_is_ever_answered() {
    let rows = table(GO_AHEAD_TABLE);
    let needing = |want: Row| -> Vec<String> {
        rows.iter()
            .filter(|(row, reply)| *row == want && reply.contains(needs::NEEDS_YOU))
            .map(|(_, reply)| reply.clone())
            .collect()
    };
    let (parks, persons) = (needing(Row::Park), needing(Row::Person));
    // Each person's lines at each width, read once: every park asks them again.
    let person_lines: Vec<Vec<_>> = persons
        .iter()
        .map(|person| {
            DRAWN
                .into_iter()
                .map(|columns| needs::needs_you(&drawn(person, columns)))
                .collect()
        })
        .collect();
    // Pure reading, so the parks split among four readers, each park read on its own: a fixed
    // pool, as this crate reads no core count (the cores a test may use are its runner's grant).
    let read = |park: &String| {
        let (mut joined_parks, mut wrong) = (0, Vec::new());
        for (person, lines) in persons.iter().zip(&person_lines) {
            for (columns, lines) in DRAWN.into_iter().zip(lines) {
                let joined = drawn(&format!("{park}\n{person}"), columns);
                let Some(p) = parked("", &joined) else {
                    continue;
                };
                joined_parks += 1;
                for line in lines {
                    if p.needs.contains(line) {
                        wrong.push(format!("{columns:?}: {line:?} in {joined:?}"));
                    }
                }
                if columns.is_none() {
                    let named = needs::needs_you(&joined);
                    if !p.needs.iter().all(|l| named.contains(l)) {
                        wrong.push(format!("not on the menu bar: {:?} of {joined:?}", p.needs));
                    }
                }
            }
        }
        (joined_parks, wrong)
    };
    const READERS: usize = 4;
    let (joined_parks, wrong) = std::thread::scope(|scope| {
        let read = &read;
        let readers: Vec<_> = parks
            .chunks(parks.len().div_ceil(READERS).max(1))
            .map(|parks| scope.spawn(move || parks.iter().map(read).collect::<Vec<_>>()))
            .collect();
        readers
            .into_iter()
            .flat_map(|reader| reader.join().expect("a reader of the parks"))
            .collect::<Vec<_>>()
    })
    .into_iter()
    .fold((0, Vec::new()), |mut all, (joined_parks, wrong)| {
        all.0 += joined_parks;
        all.1.extend(wrong);
        all
    });
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert!(joined_parks >= 100, "the sweep parks: {joined_parks}");
    // The control asks only whether one cell answers, so it reads to the first (past the memory:
    // [`parked`] keeps nothing while a part is dropped). Were none to, it would read every cell.
    let answered = dropping(Part::PersonOnly, || {
        parks.iter().any(|park| {
            persons.iter().any(|person| {
                DRAWN.into_iter().any(|columns| {
                    parked("", &drawn(&format!("{park}\n{person}"), columns)).is_some_and(|p| {
                        needs::needs_you(&drawn(person, columns))
                            .iter()
                            .any(|line| p.needs.contains(line))
                    })
                })
            })
        })
    });
    assert!(
        answered,
        "NEGATIVE CONTROL: without the person-only exclusion some person's line is answered"
    );
}

#[test]
fn a_semicolon_joined_pending_work_clause_parks_and_past_or_split_controls_do_not() {
    for (reply, lands, room, token) in [
        (
            "Ready to commit and push; waiting on your sign-off.",
            true,
            false,
            None,
        ),
        (
            "Tests pass and the branch is ready to merge into main; waiting for your go-ahead.",
            true,
            false,
            None,
        ),
        (
            "The fix is uncommitted and needs a build; say \"go\" when ready.",
            false,
            false,
            Some("go"),
        ),
        (
            "I can merge the branch and remove the worktree; just say the word.",
            true,
            false,
            None,
        ),
        (
            "I'll build, test and push to main; waiting on your go-ahead.",
            true,
            false,
            None,
        ),
        (
            "I still need to rerun the checks; holding off until you give the go-ahead.",
            false,
            false,
            None,
        ),
        (
            "The branch has not been pushed yet; waiting on your approval.",
            true,
            false,
            None,
        ),
        (
            "The build was stopped for low memory; waiting for your go-ahead.",
            false,
            true,
            None,
        ),
    ] {
        for columns in DRAWN {
            let text = drawn(reply, columns);
            let park = parked("", &text).unwrap_or_else(|| panic!("missed at {columns:?}: {text}"));
            assert_eq!(park.lands, lands, "{reply}: {park:?}");
            assert_eq!(park.waits_room, room, "{reply}: {park:?}");
            assert_eq!(park.token.as_deref(), token, "{reply}: {park:?}");
        }
    }

    for control in [
        "I built and tested the fix; waiting on your sign-off.",
        "I ran the checks to verify the fix; waiting on your sign-off.",
        "I built the fix and can confirm it works; waiting on your sign-off.",
        "Built the crate and didn't touch unrelated files; waiting on your sign-off.",
        "Nothing left to commit or push; waiting on your sign-off.",
        "I will land slice 1 now; waiting for your go-ahead on the next stage.",
        "Ready to commit and push. Waiting on your sign-off.",
        "Ready to push the release tag; waiting on your sign-off.",
        "Ready to merge Alice's branch; waiting on your sign-off.",
        "Ready to sign in and push; waiting on your sign-off.",
        "Ready to build; I'll continue the migration once you give the go-ahead.",
    ] {
        assert_eq!(
            parked("", control),
            None,
            "control must not park: {control}"
        );
    }
}

/// What a person must give, with the agent's own reversible work in the sentence before: a
/// hostile review (2026-11-09) of joining that sentence as a prior clause (285c967d99, reverted)
/// found each of these read as a park there; none parks, so the split form of KI-1020-2 stays a
/// miss until a join reads the whole paragraph against the person-only exclusions.
#[test]
fn a_person_s_decision_after_a_sentence_of_own_work_never_parks() {
    for reply in [
        "This checkout is the public mirror. Ready to commit and push. Waiting on your sign-off.",
        "This change touches prod billing. Ready to commit and push. Waiting on your sign-off.",
        "Ready to commit and push, then submit the build to the App Store. Waiting for your go-ahead.",
        "Ready to commit and push, then mail the changelog to everyone on the list. Waiting on your sign-off.",
        "Ready to land this and run the schema change against the live database. Waiting on your sign-off.",
        "Ready to commit and push, then message Alice about the outage. Waiting on your go-ahead.",
        "Ready to commit and push, then flip the feature flag on for all users. Waiting on your sign-off.",
        "Ready to commit and push, then refund the three duplicate orders. Waiting on your sign-off.",
        "Ready to commit and push, then clear the shared cache volume on the build server. Waiting on your sign-off.",
        "The branch is ready to merge into main once the reviewer is happy with it. Waiting on their approval.",
        "Priya's fix is ready to merge into main. Waiting on your go-ahead.",
        "The fix from Priya is ready to merge into main. Waiting on your go-ahead.",
        "My teammate will rebase it onto main. Waiting on your go-ahead.",
        "I won't merge this into main, because I think the approach is wrong. Waiting on your go-ahead.",
        "I'd rather not push this to main until Sam has finished reviewing it. Waiting on your go-ahead.",
        "If the API change is acceptable to the other teams, I can push to main. Waiting on your go-ahead.",
        "You'll need to merge the branch into main yourself. Waiting on your go-ahead.",
        "Dana's review is still pending, though the tests pass. Waiting on your go-ahead.",
        "The new plan limits change what every customer is charged next month, and their invoices go out on the 1st. I'll commit the config and push it to main. Waiting on your go-ahead on the pricing.",
        "The GitHub token on the CI runner expired overnight, so the checks there are red. I can rerun them and push once it's replaced. Waiting on your go-ahead.",
        "You said not to touch the payments module until Thursday. I've got the fix ready to commit and push to main. Waiting on your go-ahead.",
        "My fix rewrites every row of the shared staging database in place, and there is no snapshot to go back to. I can commit it and run it as soon as it's pushed. Waiting on your go-ahead.",
        "I'm ready to commit and push. Waiting for your go-ahead once you've unlocked 1Password.",
        "I can rerun the full benchmark matrix on the rented GPU boxes. Waiting on your go-ahead, since each run bills about forty dollars an hour.",
        "The branch is ready to merge into main. Waiting on your go-ahead. Maya asked to review the diff before anything lands. Do you want me to hold it for her?",
    ] {
        for columns in DRAWN {
            let text = drawn(reply, columns);
            assert_eq!(
                parked("", &text),
                None,
                "must not park at {columns:?}: {reply}"
            );
        }
    }
}

/// A CONDITION NAMES WHAT THE GO-AHEAD WAITS FOR, NOT THE AGENT'S WORK (2026-11-09 review): `the
/// test iPhone` in `once the test iPhone is connected` is no test the agent runs, and `once you
/// connect it` is a person's act; work outside the condition, before it or after its `so`, still
/// parks.
#[test]
fn a_condition_s_nouns_are_not_the_agent_s_work() {
    for person in [
        "I'll rebuild the app and rerun the device tests. Waiting on your go-ahead once the test \
         iPhone is connected.",
        "Waiting on your go-ahead once the test iPhone is connected.",
        "Waiting on your go-ahead once you connect the test iPhone.",
        "Waiting on your go-ahead when the build machine is connected.",
        "Waiting on your go-ahead after the test runner is back.",
    ] {
        for columns in DRAWN {
            let text = drawn(person, columns);
            assert_eq!(
                parked("", &text),
                None,
                "must not park at {columns:?}: {person}"
            );
        }
    }
    for park in [
        "The build is waiting for your go-ahead.",
        "Waiting on your go-ahead to rebuild and push my branch once memory frees up.",
        "Say \"go\" when you're back so I can rebuild and push my branch to main.",
        "Nothing more until you tell me to rebuild.",
    ] {
        assert!(parked("", park).is_some(), "must park: {park}");
    }
}

/// OWN WORK BESIDE TOOLS, CODE AND OFFERS STILL PARKS (2026-11-09 hostile review): each word a
/// person's part is read by needs its syntax, so a tool waits on no one (`once Xcode finishes`),
/// a code noun is no act (`the mail module`, `every user of the helper`, `no snapshot tests`), a
/// narrowed scope is no refusal (`I won't commit the generated files, only the sources`) and an
/// offer is no demand (`if you'd rather do it yourself`). CONTROL:
/// `a_persons_part_beside_own_work_gets_no_go_ahead` in `turn_end_go_ahead_tests.rs`.
#[test]
fn own_work_beside_tools_code_and_offers_still_parks() {
    let lost: Vec<&str> = [
        "I'll rebuild once Xcode finishes indexing; waiting on your go-ahead.",
        "I'll commit and push when GitHub is reachable again; waiting on your go-ahead.",
        "I'll push to main once Trust finishes building; waiting on your go-ahead.",
        "Ready to rebuild and rerun the tests when Spotlight finishes indexing; waiting on your go-ahead.",
        "I'll rebuild after the simulator restarts; waiting on your go-ahead to push to main.",
        "Ready to commit the fix to the mail module and push to main; waiting on your go-ahead.",
        "I fixed the mail parser. Ready to commit and push to main; waiting on your go-ahead.",
        "Ready to commit and push the form submit handler fix; waiting on your go-ahead.",
        "Updated every user of the helper; ready to commit and push to main; waiting on your go-ahead.",
        "The tests for all users of the API pass. Ready to commit and push; waiting on your go-ahead.",
        "The change fixes the app store receipt parser tests; ready to commit and push; waiting on your go-ahead.",
        "I didn't touch the live database code; ready to commit and push to main; waiting on your go-ahead.",
        "No snapshot tests changed; ready to commit and push to main; waiting on your go-ahead.",
        "Fixed the $ and dollars rendering bug. Ready to commit and push to main; waiting on your go-ahead.",
        "I won't commit the generated files, only the sources; ready to push to main, waiting on your go-ahead.",
        "I'd rather not push the scratch branch; ready to commit and push my branch to main, waiting on your go-ahead.",
        "The teammate struct rename is done; ready to commit and push; waiting on your go-ahead.",
        "This only touches our crate, no other teams' code; ready to commit and push; waiting on your go-ahead.",
        "Ready to commit and push to main; waiting on your go-ahead, or say so if you'd rather do it yourself.",
        "I won't land it without your go-ahead.",
        "Clippy is happy now; ready to commit and push to main, waiting on your go-ahead.",
        "Clippy is happy. Tests pass; ready to commit and push; waiting on your go-ahead.",
        "Rewrote the token parser. Tests pass; ready to commit and push to main, waiting on your go-ahead.",
        "I fixed the login form validation. Tests pass; ready to commit and push to main, waiting on your go-ahead.",
        "Fixed the OAuth callback; tests pass; ready to push to main; waiting on your go-ahead.",
        "Fixed the admin page crash; all tests pass; ready to commit and push, waiting on your go-ahead.",
        "Fixed the mutex unlock ordering; ready to commit and push to main, waiting on your go-ahead.",
        "Fixed the refund calculation rounding; ready to commit and push to main, waiting on your go-ahead.",
        "Fixed the bug where the client accepts stale frames; ready to commit and push to main, waiting on your go-ahead.",
        "Rebuilt with the fix from the Trust toolchain; ready to commit and push to main, waiting on your go-ahead.",
        "Fixed the GitHub review comment parser; ready to commit and push to main, waiting on your go-ahead.",
        "Added a teammate invite flow; ready to commit and push to main, waiting on your go-ahead.",
        "Fixed the message author field; ready to commit and push to main, waiting on your go-ahead.",
        "Fixed the bug that showed the beta banner to all users; ready to commit and push to main, waiting on your go-ahead.",
        "There's no snapshot to compare against yet, so I recorded one; ready to commit and push to main, waiting on your go-ahead.",
        "I won't push this to main yet; waiting on your go-ahead.",
        "I won't commit this on my own; waiting on your go-ahead.",
        "Ready to push to main; waiting on your go-ahead, unless you want to do it yourself.",
    ]
    .into_iter()
    .filter(|reply| parked("", reply).is_none())
    .collect();
    assert!(lost.is_empty(), "no park for: {lost:#?}");
}

/// A session is named by its full tab ID or the 5 characters aterm shows of one when they hold a
/// digit and a letter, or a tab opened before the `t` form by 6 or more of its `s-` hex digits;
/// an English word or a `t` before a number of the shown shape, and a malformed ID, are not
/// sessions. Words reach it lower-cased, so an ID typed in capitals reads as one; and the test
/// that reads a whole reply holds a `t1000` in prose to no other session's work.
#[test]
fn a_session_is_its_tab_id_or_its_shown_prefix_with_a_digit_and_a_letter() {
    for word in [
        "tqn5r6e0148",
        "t7ee8c3b62f",
        "t0000000000",
        "tqn5r",
        "t0a00",
        "s-7ee8c3",
        "s-7ee8c3b62f",
        "s-0123456789abcdef0123",
    ] {
        assert!(session_id(word), "{word}");
    }
    for word in [
        "there",
        "taken",
        "tests",
        "tqn5",
        "tqn5r6",
        "tqn5r6e014",
        "tqn5r6e01480",
        "tqn5r6e014u",
        "t0000",
        "t1000",
        "t2026",
        "tqnxr",
        "tq15i",
        "s-7ee8c",
        "s-0123456789abcdef01234",
        "s-7ee8cz",
        "s-",
    ] {
        assert!(!session_id(word), "{word}");
    }
}

/// A reply's words reach the session reader lower-cased: another session named in capitals is
/// still another session's work, and a `t` before a number in prose is no session.
/// NEGATIVE CONTROL: the same act on no session's ID is the agent's own work.
#[test]
fn a_reply_names_another_session_by_its_tab_id_in_any_case_and_not_by_a_number() {
    for theirs in [
        "stop the TQN5R6E0148 build",
        "stop the TQN5R build",
        "stop the T0A00 build",
        "stop the s-7ee8c3 build",
        "stop the S-7EE8C3 build",
    ] {
        assert!(someone_else(&Unit::new(theirs, false)), "{theirs}");
    }
    for ours in [
        "stop the t1000 build",
        "stop the t2026 build",
        "stop the build",
    ] {
        assert!(!someone_else(&Unit::new(ours, false)), "{ours}");
    }
}

/// Current and retained legacy session references remain someone else's work at every drawn
/// width, in prose and inline code. Ordinary code identifiers remain the agent's own work.
#[test]
fn current_and_legacy_session_work_stays_protected_in_prose_and_inline_code() {
    for reply in [
        "I won't rebuild tqn5r6e0148's build on my own.",
        "I won't rebuild `tqn5r6e0148`'s build on my own.",
        "I won't rebuild TQN5R's build on my own.",
        "I won't rebuild s-557be3727c7ee8c3b62f's build on my own.",
        "I won't rebuild `s-557be3727c7ee8c3b62f`'s build on my own.",
        "I won't rebuild S-557BE3's build on my own.",
        "I won't rebuild `s-557be3`'s build on my own.",
        "I won't rebuild `S-557BE3`'s build on my own.",
    ] {
        for bare in [false, true] {
            let reply = if bare {
                rendered(reply)
            } else {
                reply.to_owned()
            };
            for columns in DRAWN {
                let text = drawn(&reply, columns);
                assert!(someone_else(&Unit::new(&text, false)), "{text}");
                assert!(parked("", &text).is_none(), "{text}");
            }
        }
    }
    let own = "I won't rebuild `build_cache` on my own.";
    for bare in [false, true] {
        let reply = if bare { rendered(own) } else { own.to_owned() };
        for columns in DRAWN {
            let text = drawn(&reply, columns);
            assert!(parked("", &text).is_some(), "{text}");
        }
    }
    // Extracted code words do not acquire the prose reader's foreign-party meanings.
    for columns in DRAWN {
        let text = drawn("I won't rebuild `worker` on my own.", columns);
        assert!(parked("", &text).is_some(), "{text}");
    }
}
