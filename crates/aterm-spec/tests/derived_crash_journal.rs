// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `CrashJournalClaim` (`aterm_spec::derive::crash_journal_claim_model`):
//! the crash journal's claim (PTY keeper P1, `docs/DESIGN-pty-keeper-2026-09-26.md`
//! §5.5, §6.3). Proved at `Buggy = 0` and caught at `Buggy = 1` on every
//! invariant; the owner's ends are walked the way a person meets them. Tier-1 is
//! `aterm-gui/src/crash_journal_conformance.rs`.

use aterm_spec::derive::{Model, crash_journal_claim_model};
use aterm_spec::{interp, verify};

fn only(m: &Model, invariant: &str) -> Model {
    let mut m = m.clone();
    m.invariants.retain(|i| i.name == invariant);
    m
}

fn walk(m: &Model, actions: &[&str]) -> interp::State {
    let mut state = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

#[test]
fn crash_journal_claim_proves_and_catches() {
    let model = crash_journal_claim_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|registered| registered.name == model.name),
        "the crash journal's claim must stay enrolled in the spec-link registry"
    );
    let committed: Vec<&str> = interp::fired_actions(&model).into_iter().collect();
    for action in [
        "BootFresh",
        "BootFromJournal",
        "Write",
        "Settle",
        "Quit",
        "HandOff",
        "Die",
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
    verify::prove_and_catch_scalar(
        &model,
        "crash journal: taken once, never live, only after an unclean end, never looped, never lost",
    );
}

/// The ends a person meets. A SIGKILL after a write is reopened. A Cmd-Q leaves
/// nothing to take. The update's hand-off leaves its image behind and it is
/// taken but not reopened. A launch that reopened a journal and died before its
/// 90 s is taken and skipped; one that lived past them is reopened. A second
/// launch finds nothing where the first took the image.
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

    let running = walk(&m, &["BootFresh", "Write"]);
    assert!(
        !m.action_enabled("Claim", &running),
        "a running window's journal is never taken"
    );

    let relapsed = walk(&m, &["BootFromJournal", "Write", "Die", "Claim"]);
    assert_eq!(
        (relapsed["claims"], relapsed["applied"]),
        (1, 0),
        "a relapse inside the 90 s is skipped: {relapsed:?}"
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

    let (looped, _) = interp::bmc(&only(&m, "NoLoop")).expect_err("the probation mark unread");
    assert_eq!(
        (looped["applied"], looped["image_probation"]),
        (1, 1),
        "a relapse inside the 90 s reopened again: {looped:?}"
    );

    let (lost, _) = interp::bmc(&only(&m, "NoLoss")).expect_err("the unlink-first writer");
    assert_eq!(
        (lost["lost"], lost["written"], lost["image"]),
        (1, 1, 0),
        "a published layout gone when its owner died mid-rewrite: {lost:?}"
    );

    assert!(verify::uncaught_invariants(&crash_journal_claim_model()).is_empty());
}
