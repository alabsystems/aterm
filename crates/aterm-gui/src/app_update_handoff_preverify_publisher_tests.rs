// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PreverifyPublisher, capture_exceeds_policy};
use aterm_update_core::handoff_policy::{CarryCeiling, HandoffPolicy, PolicyFile, PolicyRead};

/// The publisher and the park agree on the key because both take it from the ticket
/// (`PreverifyPublisher::for_attempt`, `App::park_policy`); before any pass `known` is
/// false, so the fork lane's worker checks the capture against the policy once read.
#[test]
fn the_worker_publishes_and_the_park_reads_by_the_same_ticket() {
    let app = crate::App::headless_for_test();
    let running = crate::running_build_number();
    let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
        running.saturating_add(1),
        COMMIT,
        &"ab".repeat(32),
    );
    let unread = app.park_policy(Some(&ticket));
    assert_eq!(unread, super::ParkPolicy::default());
    assert!(!unread.known, "no pass on record: the policy is not known");
    assert_eq!(unread.carry_ceiling(), CarryCeiling::Full);
    assert_eq!(app.park_policy(None), super::ParkPolicy::default());

    let relaxed_repaint = PolicyRead::Parsed(
        PolicyFile::parse(
            "schema = 1\ncarry = \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n",
        )
        .expect("schema 1"),
    );
    PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
        .expect("an attempt names its artifact")
        .publish_policy_pass(&relaxed_repaint, running, "the test candidate");
    let read = app.park_policy(Some(&ticket));
    assert!(read.known);
    assert_eq!(read.carry_ceiling(), CarryCeiling::Repaint);
    assert!(
        read.relaxes_land_gate(),
        "the launched lane's gate reads it too"
    );

    // A pass for a policy-free candidate is known to ask nothing.
    PreverifyPublisher::for_attempt(&app.handoff_preverified, Some(&ticket))
        .expect("an attempt names its artifact")
        .publish_policy_pass(&PolicyRead::Absent, running, "the test candidate");
    let nothing = app.park_policy(Some(&ticket));
    assert!(nothing.known && nothing.policy.is_none());
}

/// A fork-lane capture taken above the policy its worker then reads is never
/// handed over: with no pass on record the lane parked at Full, and the policy
/// read afterwards must refuse this capture, not just the next park.
#[test]
fn a_fork_capture_above_the_policy_read_after_it_is_refused() {
    let policy = |text: &str| Some(HandoffPolicy::parse(text).expect("schema 1"));
    let repaint = policy("schema = 1\ncarry = \"repaint\"\n");
    let visible = policy("schema = 1\ncarry = \"visible\"\n");
    let refusal = capture_exceeds_policy(Some(CarryCeiling::Full), repaint)
        .expect("a Full capture under a repaint policy is refused");
    assert!(refusal.contains("carry=repaint"), "{refusal}");
    assert!(capture_exceeds_policy(Some(CarryCeiling::Full), visible).is_some());
    assert_eq!(
        capture_exceeds_policy(Some(CarryCeiling::Repaint), repaint),
        None,
        "a park that already followed it"
    );
    assert_eq!(
        capture_exceeds_policy(Some(CarryCeiling::Repaint), visible),
        None,
        "a park below the ceiling"
    );
    assert_eq!(
        capture_exceeds_policy(Some(CarryCeiling::Full), None),
        None,
        "no policy for this build"
    );
    assert_eq!(
        capture_exceeds_policy(None, repaint),
        None,
        "the launched lane parks after reading it"
    );
}

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn publisher(app: &crate::App, build: u64) -> PreverifyPublisher {
    PreverifyPublisher {
        slot: std::sync::Arc::clone(&app.handoff_preverified),
        build,
        commit: COMMIT.to_string(),
        artifact: "ab".repeat(32),
    }
}

#[test]
fn a_published_pass_carries_the_policy_to_the_park_by_the_attempt_s_key() {
    let app = crate::App::headless_for_test();
    let running = crate::running_build_number();
    let target = running.saturating_add(1);
    let artifact = "ab".repeat(32);
    let repaint = PolicyRead::Parsed(
        PolicyFile::parse("schema = 1\ncarry = \"repaint\"\n").expect("schema 1"),
    );
    assert_eq!(app.handoff_policy_for(target, COMMIT, &artifact), None);
    publisher(&app, target).publish_policy_pass(&repaint, running, "the test candidate");
    assert_eq!(
        app.handoff_policy_for(target, COMMIT, &artifact)
            .map(|policy| policy.carry_ceiling()),
        Some(CarryCeiling::Repaint),
        "the park reads the published policy"
    );
    let cached = app
        .handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("published");
    assert!(cached.passed && cached.reason.is_none());
    assert_eq!(
        (
            cached.build,
            cached.commit.as_str(),
            cached.artifact.as_str()
        ),
        (target, COMMIT, artifact.as_str())
    );
    // The key is the attempt's: another build, commit or artifact reads none.
    assert_eq!(app.handoff_policy_for(target + 1, COMMIT, &artifact), None);
    assert_eq!(
        app.handoff_policy_for(target, &"f".repeat(40), &artifact),
        None
    );
    assert_eq!(
        app.handoff_policy_for(target, COMMIT, &"cd".repeat(32)),
        None
    );

    // Several rescues: the park follows the `[[ranges]]` entry naming this build
    // (relaxed Land gate, at Full), not the top-level hull above the list.
    let earlier = running.saturating_sub(5);
    let ranged = PolicyRead::Parsed(
        PolicyFile::parse(&format!(
            "schema = 1\napplies_to_producers = [{earlier}, {running}]\ncarry = \
             \"repaint\"\npark_quiet_gate_at_land = \"relaxed\"\n\n[[ranges]]\nproducers = \
             [{earlier}, {earlier}]\ncarry = \"repaint\"\nreason = \"a\"\n\n[[ranges]]\n\
             producers = [{running}, {running}]\npark_quiet_gate_at_land = \"relaxed\"\n\
             reason = \"b\"\n"
        ))
        .expect("schema 1"),
    );
    publisher(&app, target).publish_policy_pass(&ranged, running, "the test candidate");
    let park = app
        .handoff_policy_for(target, COMMIT, &artifact)
        .expect("the rescue naming this build");
    assert_eq!(park.carry_ceiling(), CarryCeiling::Full);
    assert!(park.relaxes_land_gate());

    // A policy that cannot be followed, or is for other builds, still publishes
    // the pass, with no policy.
    let other = running.wrapping_add(7);
    for read in [
        PolicyRead::Ignored("not TOML".to_string()),
        PolicyRead::Absent,
        PolicyRead::Parsed(
            PolicyFile::parse(&format!(
                "schema = 1\napplies_to_producers = [{other}, {other}]\ncarry = \"repaint\"\n"
            ))
            .expect("schema 1"),
        ),
    ] {
        publisher(&app, target).publish_policy_pass(&read, running, "the test candidate");
        assert_eq!(
            app.handoff_policy_for(target, COMMIT, &artifact),
            None,
            "{read:?}"
        );
        assert!(
            app.handoff_preverified
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_some_and(|entry| entry.passed),
            "{read:?}: the pass itself is published"
        );
    }
}
