// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_full_pass_notes_its_ending_and_its_unlanded_index_and_a_seed_only_the_ladder() {
    let at = 1_790_000_000;
    let mut own = OwnPasses::default();
    let mut unlanded = None;
    let settled = note_pass_ending(
        PassVerb::Update,
        PassWhy::Published(44),
        PassEnding::Settled,
        false,
        at,
        &mut own,
        &mut unlanded,
    );
    assert!(!settled);
    assert_eq!(unlanded, Some(44), "the settled pass's index is noted");
    assert_eq!((own.ok_at, own.failed_at), (Some(at), None));
    let mut unlanded = None;
    assert!(note_pass_ending(
        PassVerb::Update,
        PassWhy::Published(45),
        PassEnding::Failed,
        false,
        at + 60,
        &mut own,
        &mut unlanded
    ));
    assert_eq!(unlanded, Some(45));
    assert_eq!((own.ok_at, own.failed_at), (Some(at), Some(at + 60)));
    let mut landed = None;
    assert!(!note_pass_ending(
        PassVerb::Update,
        PassWhy::Published(46),
        PassEnding::Ok,
        true,
        at + 120,
        &mut OwnPasses::default(),
        &mut landed
    ));
    assert_eq!(landed, None, "an index the pass landed");
    let (seed_ok, seed_failed) = (
        note_pass_ending(
            PassVerb::Seed,
            PassWhy::LaunchDue,
            PassEnding::Ok,
            true,
            at + 180,
            &mut own,
            &mut unlanded,
        ),
        note_pass_ending(
            PassVerb::Seed,
            PassWhy::LaunchDue,
            PassEnding::Failed,
            true,
            at + 240,
            &mut own,
            &mut unlanded,
        ),
    );
    assert_eq!(
        (seed_ok, seed_failed),
        (false, true),
        "a seed moves the ladder"
    );
    assert_eq!(
        (own.ok_at, own.failed_at),
        (Some(at), Some(at + 60)),
        "and neither the walk's clock nor the retry's"
    );
}

#[test]
fn exit_2_settles_and_everything_else_is_the_ladders() {
    let update = PassVerb::Update;
    assert_eq!(pass_ending(update, Some(0), true), PassEnding::Ok);
    assert_eq!(pass_ending(update, Some(2), true), PassEnding::Settled);
    assert_eq!(
        pass_ending(update, Some(0), false),
        PassEnding::Failed,
        "a published index the pass did not land"
    );
    assert_eq!(pass_ending(update, Some(1), true), PassEnding::Failed);
    assert_eq!(
        pass_ending(
            update,
            Some(i32::from(aterm_update_core::pkg_check::PASS_OFFLINE_EXIT)),
            true
        ),
        PassEnding::Failed,
        "offline retries quietly on the ladder"
    );
    assert_eq!(pass_ending(update, None, true), PassEnding::Failed);
    assert_eq!(
        pass_ending(PassVerb::Seed, Some(2), true),
        PassEnding::Failed
    );
    assert_eq!(pass_ending(PassVerb::Seed, Some(0), true), PassEnding::Ok);
}
