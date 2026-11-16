// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::corpus::open_work::land_table;
use crate::words::FINISHED;

/// The Land defense table (`crate::corpus::open_work::land_table` reads it).
const LAND_DEFENSE_TABLE: &str = include_str!("../fixtures/land-defense-table.txt");
/// The needs table (`crate::corpus::needs_table` reads it).
const NEEDS_TABLE: &str = include_str!("../fixtures/needs-table.txt");
/// The committed holdout (`crate::corpus::open_work::holdout_cases` reads it).
const HOLDOUT: &str = include_str!("../fixtures/land-holdout-2026-10-20.tsv");
/// The open-work reader's real reports (`crate::corpus::open_work::open_work_reports` reads them).
const OPEN_WORK_CORPUS: &str = include_str!("../fixtures/open-work-corpus.txt");

fn reads_as_labelled(open: bool, reply: &str) -> bool {
    if open {
        !clean_defense(Some(reply)) && !land_open_words(Some(reply)).is_empty()
    } else {
        clean_defense(Some(reply))
    }
}

#[test]
fn land_reads_the_defense_table_as_labelled() {
    let rows = land_table(LAND_DEFENSE_TABLE);
    let opens = rows.iter().filter(|(open, _)| *open).count();
    let cleans = rows.len() - opens;
    assert!(opens >= 80 && cleans >= 60, "{opens} open, {cleans} clean");
    let landed: Vec<&str> = rows
        .iter()
        .filter(|(open, reply)| *open && !reads_as_labelled(true, reply))
        .map(|(_, reply)| reply.as_str())
        .collect();
    let nudged: Vec<String> = rows
        .iter()
        .filter(|(open, reply)| !*open && !reads_as_labelled(false, reply))
        .map(|(_, reply)| {
            format!(
                "{reply:?} -> {:?}",
                open_work_strict(Some(&needs::defense_body(reply)))
            )
        })
        .collect();
    assert!(
        landed.is_empty() && nudged.is_empty(),
        "false landings {}/{opens}:\n{}\n\nfalse non-landings {}/{cleans}:\n{}",
        landed.len(),
        landed.join("\n"),
        nudged.len(),
        nudged.join("\n")
    );
    for report in [
        "The negative control failed to fail.",
        "The negative control fails to fail.",
        "The negative control failed to detect the bug.",
        "The negative control failed to run.",
        "The mutant fails to fail.",
        "The mutation run failed to start.",
    ] {
        let defense = format!("Nothing left: on main. {report}");
        assert!(!clean_defense(Some(&defense)), "{defense:?}");
        assert!(!land_open_words(Some(&defense)).is_empty(), "{defense:?}");
    }
    for report in [
        "The negative control gave 5 FAIL on exactly the new rows.",
        "Each of 8 mutants fails at least one test.",
        "The regression test fails on the old rule and passes on the new one.",
        "The regression test fails when the fix is reverted.",
        "The negative control failed to pass.",
        "The negative control failed to succeed.",
        "The previously failed negative control is fixed.",
        "The negative control failed to run and is now fixed.",
    ] {
        let defense = format!("Nothing left: on main. {report}");
        assert!(clean_defense(Some(&defense)), "{defense:?}");
    }
}

#[test]
fn every_land_family_is_load_bearing() {
    let open: Vec<(String, String)> = land_table(LAND_DEFENSE_TABLE)
        .into_iter()
        .filter(|(open, reply)| {
            *open && needs::defense_line(reply).is_some() && needs::needs_you(reply).is_empty()
        })
        .map(|(_, reply)| (reply.to_lowercase(), needs::defense_body(&reply)))
        .collect();
    for (skip, dropped) in LAND_FAMILIES.iter().enumerate() {
        let families: Vec<LandFamily> = LAND_FAMILIES
            .iter()
            .enumerate()
            .filter(|&(k, _)| k != skip)
            .map(|(_, f)| *f)
            .collect();
        let spells = |lower: &str| dropped.rows.iter().any(|row| lower.contains(row));
        let (named, rest): (Vec<_>, Vec<_>) = open.iter().partition(|(lower, _)| spells(lower));
        assert!(
            named
                .into_iter()
                .chain(rest)
                .any(|(_, body)| open_work_read(body, Some(&families)).is_empty()),
            "the {:?} family reads no OPEN row alone",
            dropped.name
        );
    }
}

#[test]
fn land_reads_the_needs_table_as_labelled() {
    use crate::corpus::NeedsRow;
    let rows = crate::corpus::needs_table(NEEDS_TABLE);
    let count = |want: fn(&NeedsRow) -> bool| rows.iter().filter(|(row, _)| want(row)).count();
    let nones = count(|row| matches!(row, NeedsRow::None));
    let wanted = count(|row| matches!(row, NeedsRow::Need(_)));
    let asks = count(|row| matches!(row, NeedsRow::Headless(_)));
    assert!(
        nones >= 150 && wanted >= 150 && asks >= 20,
        "{nones} none, {wanted} need, {asks} headless"
    );
    let (mut lost, mut phantom, mut landed) = (Vec::new(), Vec::new(), Vec::new());
    for (row, reply) in &rows {
        let named = needs::needs_you(reply);
        let clean = clean_defense(Some(reply));
        let said = || format!("{reply:?} -> {named:?}");
        match row {
            NeedsRow::Need(words) => {
                let carried = named
                    .iter()
                    .any(|n| n.to_lowercase().contains(&words.to_lowercase()));
                if !carried || clean || needs::says_no_needs(reply) {
                    lost.push(said());
                }
            }
            NeedsRow::None => {
                let headed = needs::without_needs(reply) != *reply;
                if !named.is_empty() || !clean || headed && !needs::says_no_needs(reply) {
                    phantom.push(said());
                }
            }
            NeedsRow::Headless(_) => {
                if clean || land_open_words(Some(reply)).is_empty() {
                    landed.push(said());
                }
            }
        }
    }
    assert!(
        lost.is_empty() && phantom.is_empty() && landed.is_empty(),
        "lost needs {}/{wanted}:\n{}\n\nphantom needs {}/{nones}:\n{}\n\nheadless asks \
         landed or unnamed {}/{asks}:\n{}",
        lost.len(),
        lost.join("\n"),
        phantom.len(),
        phantom.join("\n"),
        landed.len(),
        landed.join("\n")
    );
    // Negative controls: the open-work reading alone, needs left out, would land some NEED
    // row, and a reader that took every NEEDS YOU line for a need would hold some NONE row.
    assert!(rows.iter().any(|(row, reply)| {
        matches!(row, NeedsRow::Need(_))
            && open_work_strict(Some(&needs::defense_body(reply))).is_empty()
    }));
    assert!(rows.iter().any(|(row, reply)| {
        matches!(row, NeedsRow::None) && needs::without_needs(reply) != *reply
    }));
}

/// A NEEDS YOU TOKEN ITS CLAUSE HAS FOR SUBJECT ASKS THE PERSON NOTHING (live 2026-11-04: `- **NEEDS
/// YOU can be lost.** …`): its `YOU` is the head's name, not the person, so Land reads no phrase
/// that opens on it (`you can`, `you could`: [`needs::token_you`]) and a defense beside it lands.
/// Where the token's clause does not have it for subject, the `YOU` is the person's: a bare head
/// before a modal and a verb but `be` names its need (`NEEDS YOU must log in …`), and past a
/// row's start, where no closing mark makes the token a head, Land reads a phrase on it
/// (`… main. NEEDS YOU must approve the deploy …`: `you must`). A `you can` that is no token's,
/// and a question after a bare head, still hold.
#[test]
fn a_needs_you_token_its_clause_has_for_subject_asks_nothing() {
    let defense = "Nothing left: all on origin/main.";
    for said in [
        "- **NEEDS YOU can be lost.** A NEEDS YOU line in a turn that ended on a usage wall …",
        "NEEDS YOU can't be answered by the harness.",
        "The rule: NEEDS-YOU could be read past a comma too.",
    ] {
        let reply = format!("Merged and pushed main.\n{said}\n{defense}");
        assert!(
            clean_defense(Some(&reply)),
            "{reply:?}: {:?}",
            land_open_words(Some(&reply))
        );
    }
    for said in [
        "NEEDS YOU must log in to the Apple Developer portal and accept the new agreement.",
        "NEEDS YOU should run `gh auth login`, then I can push.",
        "NEEDS YOU would need to rotate the nod.",
    ] {
        let reply = format!("Merged and pushed main.\n{said}\n{defense}");
        assert!(!needs::needs_you(&reply).is_empty(), "{reply:?}");
        assert!(!clean_defense(Some(&reply)), "{reply:?}");
    }
    let token = |said: &str| needs::token_you(said, said.find("YOU").expect("a token"));
    assert!(token("- **NEEDS YOU can be lost.** A NEEDS YOU line …"));
    let mid = "Merged and pushed main. NEEDS YOU must approve the deploy at \
               https://example.com/deploy.";
    assert!(!token(mid));
    let reply = format!("{mid}\n{defense}");
    assert!(needs::needs_you(&reply).is_empty(), "{reply:?}");
    assert!(!clean_defense(Some(&reply)), "{reply:?}");
    for said in [
        "If you can, rotate the deploy nod.",
        "The signing step needs you: you can run it from the Settings page.",
        "NEEDS YOU can you sign in to GitHub?",
    ] {
        let reply = format!("Merged and pushed main.\n{said}\n{defense}");
        assert!(!clean_defense(Some(&reply)), "{reply:?}");
    }
}

#[test]
fn a_need_however_headed_holds_the_landing() {
    let defense = "Nothing left: all on origin/main.";
    assert!(clean_defense(Some(defense)));
    for reply in [
        "Nothing left: all on origin/main.\nNEEDS YOU — the Apple signing nod to ship the release.",
        "NEEDS YOU - publish the crate (needs the crates.io sayso).\nNothing left: everything else is on origin/main.",
        "Nothing left: all on origin/main.\nNEEDS YOU (owner): the Apple signing nod.",
        "Nothing left: all on origin/main.\nNEEDS YOU: nothing, but the tag needs your nod.",
    ] {
        assert!(!clean_defense(Some(reply)), "{reply:?}");
        assert_eq!(needs::needs_you(reply).len(), 1, "{reply:?}");
    }
    for none in [
        "NEEDS YOU: **none**",
        "NEEDS YOU: _nothing_",
        "NEEDS YOU: `none`",
        "NEEDS YOU:\n- none",
        "NEEDS YOU: nothing at the moment.",
        "NEEDS YOU: nothing at this time.",
        "NEEDS YOU: (none)",
        "NEEDS YOU: none, everything is landed.",
        "NEEDS YOU: nothing needed from you.",
        "NEEDS YOU: nothing from you.",
    ] {
        let reply = format!("{none}\n{defense}");
        assert!(clean_defense(Some(&reply)), "{reply:?}");
    }
}

#[test]
#[ignore = "a measurement: run with --ignored --nocapture; ATERM_LAND_HOLDOUT names another holdout"]
fn land_reads_a_holdout() {
    #[expect(
        clippy::disallowed_methods,
        reason = "an ignored measurement reads the holdout ATERM_LAND_HOLDOUT names, when it names one; the readers read no environment and no file"
    )]
    let text = std::env::var_os("ATERM_LAND_HOLDOUT").map_or_else(
        || HOLDOUT.to_string(),
        |path| std::fs::read_to_string(&path).expect("the holdout reads"),
    );
    let (mut opens, mut cleans, mut debatable) = (0, 0, 0);
    let (mut landed, mut nudged) = (Vec::new(), Vec::new());
    for (label, id, prov, reply) in crate::corpus::open_work::holdout_cases(&text) {
        let clean = clean_defense(Some(&reply));
        let words = land_open_words(Some(&reply));
        let needs = needs::needs_you(&reply);
        match label {
            "open" => {
                opens += 1;
                if clean {
                    landed.push(format!("{id} ({prov}) {reply:?}"));
                }
            }
            "clean" => {
                cleans += 1;
                if !clean {
                    nudged.push(format!("{id} ({prov}) {words:?} {needs:?} {reply:?}"));
                }
            }
            "debatable" => {
                debatable += 1;
                println!("DEBATABLE {id} lands={clean} {words:?} {needs:?}");
            }
            _ => panic!("unknown label {label:?}"),
        }
    }
    for l in &landed {
        println!("FALSE LANDING {l}");
    }
    for n in &nudged {
        println!("FALSE NON-LANDING {n}");
    }
    println!(
        "holdout: {opens} open, {cleans} clean, {debatable} debatable: {} false landings, {} \
         false non-landings",
        landed.len(),
        nudged.len()
    );
}

const FIRST_LAND_ROWS: &[&str] = &[
    "not landed",
    "unlanded",
    "not pushed",
    "unpushed",
    "not merged",
    "unmerged",
    "not committed",
    "uncommitted",
    "not on main",
    "not on origin",
    "not on the remote main",
    "parked",
    "wip",
    "work in progress",
    "left on a branch",
    "on its own branch",
    "for later",
    "later",
    "except",
    "blocked",
    "pending",
    "awaiting",
    "in review",
    "under review",
    "for review",
    "for your review",
    "for you to review",
    "to review",
    "still needs",
    "still need",
    "still has to",
    "still have to",
    "did not get to",
    "red",
    "separate task",
    "known issue",
    "known issues",
    "next session",
];

#[test]
fn every_land_status_row_is_open_under_land_and_not_under_cruise() {
    let said = |row: &str| -> String {
        match row {
            "except" => "all of it is merged except the docs".to_string(),
            "later" => "the docs come later".to_string(),
            "red" => "the e2e suite is red on main".to_string(),
            "separate task" => "the docs are a separate task".to_string(),
            "known issue" | "known issues" => format!("the flaky lexer test is a {row}"),
            "next session" => "the docs wait for the next session".to_string(),
            "still needs" | "still has to" => format!("the docs {row} be written"),
            "still need" | "still have to" => format!("the docs {row} be written"),
            "did not get to" => "I did not get to the docs".to_string(),
            "to review" => "the docs are up to review".to_string(),
            "awaiting" => "the docs are awaiting a reviewer".to_string(),
            "pending" => "the docs are pending".to_string(),
            "blocked" => "the docs are blocked on the nod".to_string(),
            "wip" => "the WIP docs sit on a branch".to_string(),
            "parked" | "unlanded" | "unpushed" | "unmerged" | "uncommitted" => {
                format!("the docs are {row}")
            }
            "work in progress" => "the docs are a work in progress".to_string(),
            _ => format!("the docs are {row}"),
        }
    };
    let mut missed = Vec::new();
    for row in LAND_STATUS.iter().chain(FIRST_LAND_ROWS) {
        let reply = format!("Merged the parser.\nNothing left: {}.", said(row));
        let body = needs::defense_body(&reply);
        let strict = open_work_strict(Some(&body));
        if strict.is_empty() || clean_defense(Some(&reply)) {
            missed.push(format!("{row}: {reply}"));
        }
        if OPEN_STATUS.contains(row) {
            continue;
        }
        assert!(
            open_work(Some(&body)).is_empty(),
            "{row}: Cruise's reading found {:?} in {body}",
            open_work(Some(&body))
        );
    }
    assert!(missed.is_empty(), "{missed:#?}");
    for reply in [
        "Merged the parser.\nNothing left: nothing is parked or pending or left for later.",
        "Merged the parser.\nNothing left: no branch is unmerged and none is blocked.",
    ] {
        assert!(clean_defense(Some(reply)), "{reply}");
    }
}

#[test]
fn land_reads_a_reason_as_no_excuse_and_a_landed_thing_as_landed() {
    for body in [
        "the docs branch is not merged, because no reviewer was available",
        "there is no reviewer so the docs branch is not merged",
        "the docs are not pushed: no remote is set",
        "the docs are blocked on the signing nod",
        "the docs are pending a review",
        "the docs are awaiting a reviewer",
        "the docs still has to be written",
        "I didn't get to the docs",
        "the lexer's e2e suite is skipped",
        "CI is red on main",
        "the docs are a separate task",
        "the docs are left for a follow-up session",
    ] {
        let reply = format!("Merged the parser.\nNothing left: {body}.");
        assert!(
            !open_work_strict(Some(&needs::defense_body(&reply))).is_empty(),
            "{body}"
        );
        assert!(!clean_defense(Some(&reply)), "{body}");
    }
    for body in [
        "the earlier WIP branch was merged and deleted",
        "the parked lexer branch is now merged",
    ] {
        let reply = format!("Merged the parser.\nNothing left: {body}.");
        assert!(
            clean_defense(Some(&reply)),
            "{body}: {:?}",
            land_open_words(Some(&reply))
        );
    }
    assert!(
        !open_work_strict(Some(
            "the WIP branch was merged, but the docs are not merged"
        ))
        .is_empty()
    );
    assert!(!open_work_strict(Some("the WIP branch is not merged")).is_empty());
}

#[test]
fn open_work_keeps_fenced_examples_out_of_the_report() {
    for tail in [
        "A sample report:\n```text\nStill open: finish the tests.\n```\nATERM-TASK-DONE",
        "A sample report:\n````text\n```\nTests were not run.\n```\n````\nATERM-TASK-DONE",
        "A sample report:\n~~~text\nStill open: finish the tests.\n~~~\nATERM-TASK-DONE",
        "A sample report:\n```text\nStill open: finish the tests.\nATERM-TASK-DONE",
        "Still open:\n```text\nAn example, not a reported item.\n```\nAll checks passed.\nATERM-TASK-DONE",
    ] {
        assert!(open_work(Some(tail)).is_empty(), "{tail:?}");
    }
    for fence in ["```", "~~~~"] {
        let tail = format!(
            "A sample report:\n{fence}text\nLeft: the example only.\n{fence}\n\
             Still open: finish the actual tests.\nATERM-TASK-DONE"
        );
        assert_eq!(open_work(Some(&tail)), &["still open"], "{tail:?}");
    }
    assert_eq!(
        open_work(Some(
            "All work completed\n```text\nAn example.\n```\nstill open: the actual tests.\nATERM-TASK-DONE"
        )),
        &["still open"]
    );

    for (open, inner, close) in [
        ("~~~~markdown", "```", "~~~~"),
        ("````markdown", "~~~", "````"),
        ("````markdown", "```", "````"),
        ("~~~~markdown", "~~~", "~~~~"),
        ("```markdown", "``` not a close", "```"),
        ("~~~markdown", "~~~ not a close", "~~~"),
        ("```markdown", "~~~", "````"),
    ] {
        let tail = format!("{open}\n{inner}\nAll done.\n{close}");
        assert!(open_work(Some(&tail)).is_empty(), "{tail:?}");
        assert_eq!(
            open_work(Some(&format!(
                "{tail}\nStill open: finish the actual tests."
            ))),
            &["still open"]
        );
    }
}

#[test]
fn open_work_reads_balanced_underscore_emphasis_without_splitting_identifiers() {
    for (tail, want) in [
        (
            "__Still open__: finish the tests.\nATERM-TASK-DONE",
            "still open",
        ),
        (
            "_Remaining work_: finish the tests.\nATERM-TASK-DONE",
            "remaining",
        ),
        ("The tests were _not run_.\nATERM-TASK-DONE", "not run"),
        ("The checks were __skipped__.\nATERM-TASK-DONE", "skipped"),
    ] {
        assert_eq!(open_work(Some(tail)), &[want], "{tail:?}");
    }
    for tail in [
        "Removed the skipped_test registry.\nATERM-TASK-DONE",
        "Removed the _skipped_test_ registry.\nATERM-TASK-DONE",
        "__Still open: an unmatched example marker.\nATERM-TASK-DONE",
        "Ran `_not run_` as the example string.\nATERM-TASK-DONE",
    ] {
        assert!(open_work(Some(tail)).is_empty(), "{tail:?}");
    }
}

#[test]
fn open_work_reads_a_blocked_item_and_the_sentences_after_it() {
    for (tail, want) in [
        (
            "Still open: the release needs a credential. Tests were not run.\nATERM-TASK-DONE",
            &["still open", "not run"][..],
        ),
        (
            "Left (blocked on the owner): the release. The tests were not run.\nATERM-TASK-DONE",
            &["left", "not run"],
        ),
        (
            "Still open: the release needs a credential; the tests were not run.\nATERM-TASK-DONE",
            &["still open", "not run"],
        ),
        (
            "Still open: the release tests were not run without the owner's credential.\nATERM-TASK-DONE",
            &["still open"],
        ),
        (
            "Left (blocked on the owner): the release. No tests were skipped.\nATERM-TASK-DONE",
            &["left"],
        ),
        (
            "Left: fix the tests. Tests were skipped.\nATERM-TASK-DONE",
            &["left", "skipped"],
        ),
    ] {
        assert_eq!(open_work(Some(tail)), want, "{tail:?}");
    }
}

#[test]
fn open_work_reads_work_left_mid_sentence_and_a_blocker_named_after_it() {
    for (tail, want) in [
        ("The lexer is still open.", &["still open"][..]),
        ("I did not find the cause.", &["did not find"]),
        (
            "One spike at load 18: observed, not explained.",
            &["not explained"],
        ),
        (
            "I traced it but didn't fix it, because a fix is a code change.",
            &["did not fix"],
        ),
        ("Both helpers are not yet run at once.", &["not run"]),
        (
            "Still open from AU-1:\n- the boot apply's second half",
            &["still open"],
        ),
        (
            "Open items, all out of scope:\n- the held pane's check",
            &["open items"],
        ),
        (
            "Follow-ups for the owner: bound the vendor DNS time.",
            &["follow-ups"],
        ),
        (
            "## Not measured, and why\n- **The index hint:** not passed in the replay.",
            &["not measured"],
        ),
        (
            "Left open, each with its blocker:\n- the primer: the window rewrites it at its next spawn",
            &["left open"],
        ),
        (
            "Another chain's trust run needs a re-run.",
            &["needs a re-run"],
        ),
        (
            "At this host's load the spike was not explained.",
            &["not explained"],
        ),
        (
            "Tests were not run; the release needs a credential.",
            &["not run"],
        ),
        (
            "Not run: the e2e suite. The docs built. Lint passed. The release needs the owner's credential.",
            &["not run"],
        ),
        ("Tests: 12 passed, 2 skipped, no failures.", &["skipped"]),
        ("The memory leak is still open.", &["still open"]),
        (
            "Still open: the verifier rejected the bundle's signature.",
            &["still open"],
        ),
        ("Left: the reviewer's race finding in spawn.rs.", &["left"]),
        (
            "- The write was not allowed (EPERM) in the sandbox; not fixed.",
            &["not fixed"],
        ),
        (
            "- The release-mode tests: not run, because there was no time.",
            &["not run"],
        ),
    ] {
        assert_eq!(
            open_work(Some(&format!("{tail}\n{FINISHED}"))),
            want,
            "{tail:?}"
        );
    }
    for (tail, want) in [
        (
            "- Windows build: not compiled. Blocker: no Windows std on this host.",
            &["not compiled"][..],
        ),
        (
            "- **Windowed checks:** not run.\n  - Blocker: no windows on the owner's desktop.",
            &["not run"],
        ),
        (
            "- Moving the checks: not done, per the reviewer's correction.",
            &["not done"],
        ),
        (
            "- au-4: not done; its review verdict rejected it.",
            &["not done"],
        ),
        (
            "- Segmented downloads: not done on purpose; 8 streams measured slower.",
            &["not done"],
        ),
        (
            "- Trust for atpkg: skipped for memory, as round 1 measured.",
            &["skipped"],
        ),
        (
            "- GUI side: not done here; that is TC-3 per the plan.",
            &["not done"],
        ),
        (
            "- The stalled-mount case: not reproduced: no SMB mount is available.",
            &["not reproduced"],
        ),
        (
            "- Windowed Cmd-T was not measured (no windows allowed).",
            &["not measured"],
        ),
        (
            "- The trust floor check could not run on this host.",
            &["not run"],
        ),
        (
            "- aterm-gui's tests were not run. aterm-gui is not modified.",
            &["not run"],
        ),
        (
            "- I did not run `aterm agents install`: the brief forbids touching ~/.config.",
            &["not run"],
        ),
    ] {
        assert_eq!(
            open_work(Some(&format!("{tail}\n{FINISHED}"))),
            want,
            "{tail:?}"
        );
    }
    for tail in [
        "- contract-probe: not run, because no requires/ensures contract was added.",
        "- contract-probe was not run because the diff adds no contract.",
        "- contract-probe: not run. No contract was added.",
        "- Headless QoS: not done, because main already has it.",
        "A Window place skips the \"window still open\" check.",
    ] {
        assert!(
            open_work(Some(&format!("{tail}\n{FINISHED}"))).is_empty(),
            "{tail:?}"
        );
    }
}

#[test]
fn open_work_reads_three_rounds_of_real_reports_as_labelled() {
    let reports = crate::corpus::open_work::open_work_reports(OPEN_WORK_CORPUS);
    let reads = |text: &str| !open_work(Some(&format!("{text}{FINISHED}"))).is_empty();
    let mut wrong: Vec<String> = Vec::new();
    for (name, excerpts) in &reports {
        for (reads_open, text) in excerpts {
            if reads(text) != *reads_open {
                wrong.push(format!(
                    "{name}: reads {}, labelled {}:\n{text}",
                    reads(text),
                    reads_open
                ));
            }
        }
        let joined: String = excerpts
            .iter()
            .map(|(_, text)| format!("{text}\n"))
            .collect();
        let any = excerpts.iter().any(|(reads_open, _)| *reads_open);
        if reads(&joined) != any {
            wrong.push(format!(
                "{name}: the joined report reads {}",
                reads(&joined)
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n----\n"));
    let report = |name: &str| {
        let (_, excerpts) = reports.iter().find(|(n, _)| *n == name).expect(name);
        let joined: String = excerpts
            .iter()
            .map(|(_, text)| format!("{text}\n"))
            .collect();
        reads(&joined)
    };
    for name in [
        "round 1, AU-1 review",
        "round 1, measurement report",
        "round 2, measurement report",
        "round 2, TAB-1 implementer",
    ] {
        assert!(report(name), "{name}: ended with the token over open work");
    }
    assert!(
        report("round 3, integration notes"),
        "its leftovers are blocked, and open"
    );
    assert!(reports.len() > 60, "{} reports", reports.len());
}

/// Every word of the tables has its index, and no other word does. That every word a strict reading
/// names is among them is `every_phrase_keeps_its_contract_on_every_corpus_row`'s.
#[test]
fn every_word_of_the_tables_has_an_index() {
    for w in open_words() {
        let i = open_word_index(w).expect("indexed");
        assert_eq!(open_word_at(i), Some(w));
    }
    assert_eq!(open_word_index("a word no table holds"), None);
}
