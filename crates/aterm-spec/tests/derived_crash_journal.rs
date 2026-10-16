// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `CrashJournalClaim`, the crash journal's claim
//! (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.5, §6.3): proved at `Buggy = 0`, caught at
//! `Buggy = 1` on every invariant, and walked the way a person meets its ends. Tier-1 is
//! `aterm-gui/src/crash_journal_conformance.rs`.

use crate::walk::{only, walk};
use aterm_spec::derive::crash_journal_claim_model;
use aterm_spec::{interp, verify};

#[test]
fn crash_journal_claim_proves_and_catches() {
    let model = crash_journal_claim_model();
    let committed: Vec<&str> = interp::fired_actions(&model).into_iter().collect();
    for action in [
        "BootFresh",
        "BootFromJournal",
        "BootSecondChance",
        "Write",
        "Settle",
        "Quit",
        "WriteQuitLayout",
        "HandOff",
        "Die",
        "Crash",
        "Claim",
    ] {
        assert!(
            committed.contains(&action),
            "{action} fires at the committed configuration: {committed:?}"
        );
    }
    // The non-atomic writer's second half exists only in the mutant.
    assert!(
        !committed.contains(&"FinishWrite"),
        "a committed write is one step: {committed:?}"
    );
    for action in ["Adopt", "SuccessorDies"] {
        assert!(committed.contains(&action), "{action}: {committed:?}");
    }
    verify::prove_and_catch_scalar(
        &model,
        "crash journal: taken once, never live, only after an unclean end, never looped, never lost",
    );
}

/// The ends a person meets: a kill is reopened, a quit or an update hand-off is not, and inside
/// the 90 s probation a crash is skipped while a first stop gets one second chance.
#[test]
fn crash_journal_claim_walks_the_ends_the_way_a_person_meets_them() {
    let m = crash_journal_claim_model();

    let killed = walk(&m, &["BootFresh", "Write", "Die", "Claim"]);
    assert_eq!(
        (killed["applied"], killed["claims"], killed["image"]),
        (1, 1, 0),
        "a killed window's layout is reopened, once: {killed:?}"
    );
    assert!(
        !m.action_enabled("Claim", &killed),
        "a second launch finds nothing under the name"
    );

    let quit = walk(&m, &["BootFresh", "Write", "Quit"]);
    assert!(
        !m.action_enabled("Claim", &quit),
        "a clean quit leaves no journal: {quit:?}"
    );

    let handed_off = walk(&m, &["BootFresh", "Write", "HandOff", "Claim"]);
    assert_eq!(
        (handed_off["claims"], handed_off["applied"]),
        (1, 0),
        "the update parent's journal is taken and set aside: {handed_off:?}"
    );

    // A quit cut short after its `session.toml` and before its journal went: the journal is
    // taken and set aside (its desk comes back once, as the quit layout) however the run died.
    for end in ["Die", "Crash"] {
        let cut = walk(&m, &["BootFresh", "Write", "WriteQuitLayout", end, "Claim"]);
        assert_eq!(
            (cut["claims"], cut["applied"]),
            (1, 0),
            "a quit cut short by {end} is not reopened twice: {cut:?}"
        );
    }

    // The successor runs on the live sessions only once the parent's image is its own, so it
    // can end before the take-over only when there was nothing to take.
    let waiting = walk(&m, &["BootFresh", "Write", "HandOff"]);
    assert!(
        !m.action_enabled("SuccessorDies", &waiting),
        "the successor takes the image over before it runs: {waiting:?}"
    );
    let nothing_to_take = walk(&m, &["BootFresh", "HandOff", "SuccessorDies"]);
    assert_eq!(nothing_to_take["lost"], 0, "{nothing_to_take:?}");
    let adopted_died = walk(
        &m,
        &["BootFresh", "Write", "HandOff", "Adopt", "Die", "Claim"],
    );
    assert_eq!(
        (adopted_died["applied"], adopted_died["lost"]),
        (1, 0),
        "a successor killed after its take-over reopens the desk: {adopted_died:?}"
    );

    let running = walk(&m, &["BootFresh", "Write"]);
    assert!(
        !m.action_enabled("Claim", &running),
        "a running window's journal is never taken"
    );

    let crashed = walk(&m, &["BootFromJournal", "Write", "Crash", "Claim"]);
    assert_eq!(
        (crashed["claims"], crashed["applied"]),
        (1, 0),
        "a crash inside the 90 s is skipped: {crashed:?}"
    );

    let stopped = walk(&m, &["BootFromJournal", "Write", "Die", "Claim"]);
    assert_eq!(
        (stopped["claims"], stopped["applied"]),
        (1, 1),
        "a first stop inside the 90 s comes back once: {stopped:?}"
    );

    let again = walk(&m, &["BootSecondChance", "Write", "Die", "Claim"]);
    assert_eq!(
        (again["claims"], again["applied"]),
        (1, 0),
        "the second stop in a row inside the 90 s is skipped: {again:?}"
    );

    let second_settled = walk(
        &m,
        &["BootSecondChance", "Write", "Settle", "Crash", "Claim"],
    );
    assert_eq!(
        second_settled["applied"], 1,
        "a second chance that lived its 90 s is reopened: {second_settled:?}"
    );

    let settled = walk(&m, &["BootFromJournal", "Write", "Settle", "Die", "Claim"]);
    assert_eq!(
        settled["applied"], 1,
        "a launch that lived its 90 s is reopened: {settled:?}"
    );

    let never_wrote = walk(&m, &["BootFresh", "Die"]);
    assert!(
        !m.action_enabled("Claim", &never_wrote),
        "a window that died before its first write has nothing to take"
    );
}

/// `Buggy = 1` — a claim that neither honours the owner's lock nor renames,
/// reads neither the crash marker nor the probation mark, and a writer that
/// unlinks before it publishes — is caught on each invariant by its own
/// schedule.
#[test]
fn crash_journal_claim_the_defects_are_caught_on_every_invariant() {
    let m = interp::with_buggy(&crash_journal_claim_model(), 1);

    let (double, _) = interp::bmc(&only(&m, "SingleUse")).expect_err("the copying claim");
    assert_eq!(double["claims"], 2, "one image taken twice: {double:?}");

    let (live, _) = interp::bmc(&only(&m, "LiveOwnerNeverClaimed")).expect_err("the lock unread");
    assert_eq!(
        (live["live_claim"], live["live"]),
        (1, 1),
        "a running window's journal taken: {live:?}"
    );

    let (clean, _) =
        interp::bmc(&only(&m, "CleanEndNeverRestored")).expect_err("the marker unread");
    assert_eq!(
        (clean["applied"], clean["clean"]),
        (1, 1),
        "a clean end reopened as a crash: {clean:?}"
    );

    let (crash_looped, _) =
        interp::bmc(&only(&m, "NoCrashLoop")).expect_err("the probation mark unread");
    assert_eq!(
        (
            crash_looped["applied"],
            crash_looped["image_probation"],
            crash_looped["crashed"]
        ),
        (1, 1, 1),
        "a crash inside the 90 s reopened again: {crash_looped:?}"
    );

    let (looped, _) = interp::bmc(&only(&m, "NoLoop")).expect_err("the second-chance mark unread");
    assert_eq!(
        (looped["applied"], looped["image_second"]),
        (1, 1),
        "a second chance stopped inside the 90 s reopened again: {looped:?}"
    );

    let (lost, _) = interp::bmc(&only(&m, "NoLoss")).expect_err("the unlink-first writer");
    assert_eq!(
        (lost["lost"], lost["written"], lost["image"]),
        (1, 1, 0),
        "a published layout gone when its owner died mid-rewrite: {lost:?}"
    );

    // A Commit waiter that releases the readers before the take-over runs the new build on the
    // live sessions while the desk on disk is the parent's clean end; a death there loses it.
    let released_first = walk(
        &m,
        &[
            "BootFresh",
            "Write",
            "FinishWrite",
            "HandOff",
            "SuccessorDies",
        ],
    );
    assert_eq!(
        (released_first["lost"], released_first["image"]),
        (1, 1),
        "the parent's image left behind a dead successor: {released_first:?}"
    );
    assert!(!m.check_invariant("NoLoss", &released_first));
}
