// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Each row says its own mode, in [`Autopilot::ALL`]'s order, and [`mode_words`] finds it.
#[test]
fn each_mode_has_its_own_row_in_order() {
    for (i, mode) in Autopilot::ALL.into_iter().enumerate() {
        assert_eq!(MODE_WORDS[i].mode, mode, "row {i}");
        assert_eq!(mode_words(mode).mode, mode, "{}", mode.word());
    }
}

/// A receipt of `leg` over `/Users//me/src/proj`, its head `3f9a1c2e4b7d`, on `origin/main`
/// and `main` where the leg is on main.
fn receipt(leg: ReceiptLeg, said: &str) -> LandReceipt {
    let named = !matches!(leg, ReceiptLeg::NoRepo | ReceiptLeg::Gone);
    let on_main = leg == ReceiptLeg::OnMain;
    let pick = |yes: bool, s: &str| if yes { s.to_owned() } else { String::new() };
    LandReceipt {
        head: pick(named, "3f9a1c2e4b7d"),
        main_ref: pick(on_main, "origin/main"),
        repo: pick(leg != ReceiptLeg::NoRepo, "/Users//me/src/proj"),
        branch: pick(on_main, "main"),
        leg,
        check: false,
        said: said.to_owned(),
        at_ms: 0,
        statement: crate::harness::land_receipt::Statement::Unread,
    }
}

/// EACH LEG'S WORDS COME FROM ONE SOURCE AND SAY WHAT GIT SAID (§14.9, slice 9c; §14.17 item
/// 4): every leg says its own words, each different; the head where one was read, the ref and
/// the branch only on main, the folder where it is gone; "remote main" on none (git names
/// `origin/main`). The description, the tip and the record's words each lead with them and
/// carry the agent's words; the title's brief drops the clause on main alone and the agent's
/// words on every leg. A landing with no receipt says aterm has no record of checking it, and
/// names no head. Every leg's words begin with [`CHECKED_LEAD`], [`NO_REPO_CHECKED`] or
/// [`GONE_LEAD`], the leads the lab reads. NEGATIVE CONTROLS (run by hand): the pushed leg's
/// words saying "remote main", a 9b receipt's branch read as detached, a brief keeping the
/// clause, and a brief quoting the agent each fail it.
#[test]
fn each_legs_words_come_from_one_source_and_say_what_git_said() {
    let said = "Nothing left: pushed to origin/main.";
    let want = [
        (
            ReceiptLeg::OnMain,
            "aterm checked 3f9a1c2e4b7d on origin/main in /Users//me/src/proj; nothing \
             uncommitted, untracked or unpushed, and no stash on main",
        ),
        (
            ReceiptLeg::Pushed,
            "aterm checked 3f9a1c2e4b7d pushed; no origin main to check against",
        ),
        (
            ReceiptLeg::NoRemote,
            "aterm checked 3f9a1c2e4b7d committed; this repository has no remote",
        ),
        (ReceiptLeg::NoRepo, NO_REPO_CHECKED),
        (ReceiptLeg::Gone, "the folder is gone (/Users//me/src/proj)"),
    ];
    assert_eq!(
        want.map(|(leg, _)| leg),
        ReceiptLeg::ALL,
        "every leg, in order"
    );
    let mut seen = std::collections::BTreeSet::new();
    for (leg, words) in want {
        let r = receipt(leg, said);
        let checked = checked_words(Some(&r));
        assert_eq!(checked, words, "{leg:?}");
        assert!(
            seen.insert(checked.clone()),
            "{leg:?} says another leg's words"
        );
        for surface in [
            checked.clone(),
            landed_words(Some(&r)),
            landed_tip(Some(&r)),
        ] {
            assert!(!surface.contains("remote main"), "{leg:?}: {surface}");
        }
        let title = landed_words(Some(&r));
        assert_eq!(
            title,
            format!("{checked}; it said \u{201c}{said}\u{201d}; {LANDED_REST}"),
            "{leg:?}"
        );
        assert!(
            !title.contains(FACT_SEPARATOR) && !title.contains(" \u{2014} "),
            "the title splits on neither: {title}"
        );
        assert_eq!(
            landed_tip(Some(&r)),
            format!("{checked}{FACT_SEPARATOR}it said \u{201c}{said}\u{201d}")
        );
        let quiet = receipt(leg, "");
        assert_eq!(said_words(&quiet), "it said nothing is left");
    }
    let detached = LandReceipt {
        branch: crate::harness::repo_work::DETACHED.to_owned(),
        ..receipt(ReceiptLeg::OnMain, said)
    };
    assert!(
        checked_words(Some(&detached)).ends_with("no stash on its detached HEAD"),
        "{}",
        checked_words(Some(&detached))
    );
    // A receipt journaled before the branch was read (9b's) names none, and is no
    // detached HEAD.
    let unread = LandReceipt {
        branch: String::new(),
        ..receipt(ReceiptLeg::OnMain, said)
    };
    assert!(
        checked_words(Some(&unread)).ends_with("and no stash on HEAD"),
        "{}",
        checked_words(Some(&unread))
    );
    // The title's brief: on main the head, the ref and the folder, no clause; every other
    // leg its whole words; never the agent's words; with no receipt, no record.
    for leg in ReceiptLeg::ALL {
        let r = receipt(leg, said);
        let brief = checked_brief(Some(&r));
        if leg == ReceiptLeg::OnMain {
            assert_eq!(
                brief,
                "aterm checked 3f9a1c2e4b7d on origin/main in /Users//me/src/proj"
            );
        } else {
            assert_eq!(brief, checked_words(Some(&r)), "{leg:?}");
        }
        assert_eq!(
            landed_brief(Some(&r)),
            format!("{brief}; {LANDED_REST}"),
            "{leg:?}"
        );
        assert!(!landed_brief(Some(&r)).contains(said), "{leg:?}");
        assert!(!landed_brief(Some(&r)).contains("remote main"), "{leg:?}");
        assert!(
            [CHECKED_LEAD, NO_REPO_CHECKED, GONE_LEAD]
                .iter()
                .any(|lead| brief.starts_with(lead)),
            "{leg:?}: {brief}"
        );
    }
    assert_eq!(landed_brief(None), format!("{NO_RECORD}; {LANDED_REST}"));
    assert_eq!(checked_words(None), NO_RECORD);
    assert_eq!(landed_tip(None), NO_RECORD);
    let none = landed_words(None);
    assert_eq!(
        none,
        format!("it said nothing is left; {NO_RECORD}; {LANDED_REST}")
    );
    assert!(!none.contains("3f9a1c2e4b7d"), "{none}");
}

/// A QUOTE CUT TO ITS LINE KEEPS ITS CLOSING MARK (the review round of slice 9c): the Messages
/// record caps its line at `aterm_messages::DETAIL_LINE_CAP` characters, and a receipt keeps
/// up to `SAID_BYTES` of the agent's words, so `it said “` and the mark would push a long
/// quote past the cap. Cut to the cap, a quote ends in one `…` and `”`, whether or not the
/// receipt had already cut it; one that fits is whole. NEGATIVE CONTROL (run by hand): the
/// whole quote at any bound fails it.
#[test]
fn a_quote_cut_to_its_line_keeps_its_closing_mark() {
    use crate::harness::land_receipt::SAID_BYTES;
    let cap = 240;
    let long = format!("Nothing left: {}", "x".repeat(SAID_BYTES));
    let cut_by_9b = format!("{}\u{2026}", &long[..SAID_BYTES - 3]);
    for said in [long.as_str(), cut_by_9b.as_str()] {
        let r = receipt(ReceiptLeg::OnMain, said);
        let words = said_within(&r, cap);
        assert_eq!(words.chars().count(), cap, "{words}");
        assert!(words.ends_with("x\u{2026}\u{201d}"), "{words}");
        assert_eq!(words.matches('\u{2026}').count(), 1, "{words}");
        assert!(
            words.starts_with("it said \u{201c}Nothing left: "),
            "{words}"
        );
    }
    let short = receipt(ReceiptLeg::OnMain, "Nothing left: pushed.");
    assert_eq!(
        said_within(&short, cap),
        "it said \u{201c}Nothing left: pushed.\u{201d}"
    );
    assert_eq!(said_within(&short, cap), said_words(&short));
    assert_eq!(
        said_within(&receipt(ReceiptLeg::OnMain, ""), 3),
        "it said nothing is left"
    );
}

/// A LANDING THAT STOOD ON THE LAND CHECK SAYS SO, AND ONLY THEN (slice 13a merged into
/// 9c's words): with the receipt's `check`, what the agent said is [`ANSWERED_CHECK`], then
/// the defense the check audited; without it, the defense alone; a receipt that kept no
/// defense says the answer or that it said nothing is left. The cut to a line keeps the
/// answer and the closing mark. NEGATIVE CONTROL (run by hand): `said_within` reading no
/// `check` fails it.
#[test]
fn a_landing_on_the_land_check_says_it_answered_it() {
    let mut r = receipt(ReceiptLeg::OnMain, "Nothing left: pushed.");
    assert_eq!(
        said_words(&r),
        "it said \u{201c}Nothing left: pushed.\u{201d}"
    );
    assert!(!landed_words(Some(&r)).contains("land check"));
    r.check = true;
    assert_eq!(
        said_words(&r),
        format!("{ANSWERED_CHECK}, after it said \u{201c}Nothing left: pushed.\u{201d}")
    );
    assert!(landed_words(Some(&r)).contains(ANSWERED_CHECK));
    assert!(landed_tip(Some(&r)).contains(ANSWERED_CHECK));
    assert!(!landed_brief(Some(&r)).contains("land check"));
    r.said = "x".repeat(400);
    let cut = said_within(&r, 120);
    assert_eq!(cut.chars().count(), 120, "{cut}");
    assert!(
        cut.starts_with(ANSWERED_CHECK) && cut.ends_with("x\u{2026}\u{201d}"),
        "{cut}"
    );
    r.said.clear();
    assert_eq!(said_words(&r), ANSWERED_CHECK);
    r.check = false;
    assert_eq!(said_words(&r), "it said nothing is left");
}
