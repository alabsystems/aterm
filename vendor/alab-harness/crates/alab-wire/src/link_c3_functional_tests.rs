// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The optional C3 lifecycle against the test-owned fake, using synthetic native identifiers.

use super::*;
use crate::claim::{ChildProcess, NativeChild};
use crate::fake::{FakeAterm, POLICY_ON, info_with_rows};
use std::num::NonZeroU64;
use std::time::Instant;

const ID: &str = "example-alab";

fn config(tracked: bool, contract: Contract) -> Config {
    let config = Config::new(ID, "example", &[Scope::Relaunch, Scope::Upgrade], "0.2.0")
        .unwrap()
        .with_contracts(&[contract])
        .unwrap()
        .with_stream(false);
    if tracked {
        config.with_c3_child_tracking()
    } else {
        config
    }
}

fn host(name: &str, caps: &str, tracked: bool, contract: Contract) -> (FakeAterm, Link) {
    let config = config(tracked, contract);
    let fake = FakeAterm::for_harness(name, &config);
    fake.serve_contracts(&[contract.number()]);
    fake.set_info(&format!("{} {caps}", info_with_rows(&[ID])));
    let link = Link::launch_at(
        config,
        std::process::id(),
        &fake.locator(),
        Child::default(),
        0,
    );
    assert!(link.judges(), "the requested contract is held");
    (fake, link)
}

fn native(pid: u32, birth: u64) -> NativeObservation {
    NativeObservation::Present(NativeChild::from_native(pid, birth).unwrap())
}

fn pending(link: &mut Link, now_ms: u64) -> NonZeroU64 {
    match link.before_spawn(None, now_ms) {
        ChildStart::Pending { generation } => generation,
        other => panic!("expected pending generation, got {other:?}"),
    }
}

fn begins(fake: &FakeAterm) -> Vec<String> {
    fake.verbs()
        .into_iter()
        .filter(|verb| {
            verb.split_ascii_whitespace()
                .any(|word| word.starts_with("child-start="))
                && !verb
                    .split_ascii_whitespace()
                    .any(|word| word.starts_with("child="))
        })
        .collect()
}

fn lose(fake: &FakeAterm, link: &mut Link) {
    fake.restart();
    link.events(Child::default(), 1);
    assert!(link.is_lost() && !link.judges());
}

#[test]
fn negotiated_c3_begin_binds_the_native_child_and_the_next_attempt_advances() {
    let (fake, mut link) = host(
        "c3-begin",
        "child-proof=1 child-start=1",
        true,
        Contract::Three,
    );
    let first = pending(&mut link, 0);
    assert!(fake.child().is_none());
    let at = fake.verbs().len();
    link.reannounce_native(Child::default(), NativeObservation::Unknown, 0);
    link.tick_native(Child::default(), NativeObservation::Unknown, 0);
    assert_eq!(fake.verbs().len(), at, "an unknown tuple remains pending");
    link.tick_native(Child::default(), native(4242, 12_345), 0);
    assert_eq!(fake.child().as_deref(), Some("4242:12345"));
    assert_eq!(
        fake.claim_words().unwrap().child_start,
        Some(first.to_string())
    );

    // This explicit notification represents the original owned wait event;
    // no native process is created or observed by the scenario.
    link.child_ended();
    let next = pending(&mut link, 1);
    assert!(next > first);
    link.reannounce_native(Child::default(), native(4243, 12_346), 1);
    assert_eq!(fake.child().as_deref(), Some("4243:12346"));
    assert_eq!(
        fake.claim_words().unwrap().child_start,
        Some(next.to_string())
    );
}

#[test]
fn proof_only_reconnect_retains_the_original_child_without_a_generation() {
    let (fake, mut link) = host("c3-proof-reconnect", "child-proof=1", true, Contract::Three);
    assert_eq!(link.before_spawn(None, 0), ChildStart::NoBarrier);
    link.reannounce_native(Child::default(), native(4242, 12_345), 0);
    assert_eq!(fake.child().as_deref(), Some("4242:12345"));
    assert!(fake.claim_words().unwrap().child_start.is_none());
    lose(&fake, &mut link);
    let at = fake.requests().len();
    link.tick_native(
        Child::default(),
        NativeObservation::Unknown,
        1 + RECLAIM_EVERY_MS,
    );
    assert_eq!(fake.requests().len(), at, "unknown is not retirement");
    link.tick_native(
        Child::default(),
        native(4242, 12_346),
        1 + RECLAIM_EVERY_MS * 2,
    );
    assert_eq!(
        fake.requests().len(),
        at,
        "a different birth cannot replace the retained child"
    );
    assert!(link.is_lost());
    link.tick_native(
        Child::default(),
        native(4242, 12_345),
        1 + RECLAIM_EVERY_MS * 3,
    );
    assert!(link.judges());
    assert_eq!(fake.child().as_deref(), Some("4242:12345"));
    assert!(fake.claim_words().unwrap().child_start.is_none());
}

#[test]
fn a_consumed_child_end_allows_no_child_reclaim_before_another_begin() {
    let (fake, mut link) = host(
        "c3-retire",
        "child-proof=1 child-start=1",
        true,
        Contract::Three,
    );
    let first = pending(&mut link, 0);
    link.reannounce_native(Child::default(), native(4242, 12_345), 0);
    lose(&fake, &mut link);
    let before = begins(&fake).len();
    link.child_ended();
    link.tick_native(
        Child::default(),
        NativeObservation::Unknown,
        1 + RECLAIM_EVERY_MS,
    );
    assert!(
        link.judges(),
        "retirement must precede crash-time reclaim decisions"
    );
    assert!(fake.child().is_none());
    assert_eq!(
        begins(&fake).len(),
        before,
        "retirement did not invent a new spawn"
    );
    let next = pending(&mut link, 2 + RECLAIM_EVERY_MS);
    assert!(next > first);
}

#[test]
fn c3_tracking_defaults_off_even_when_the_fake_advertises_it() {
    let (fake, mut link) = host(
        "c3-disabled",
        "child-proof=1 child-start=1",
        false,
        Contract::Three,
    );
    assert_eq!(link.before_spawn(None, 0), ChildStart::NoBarrier);
    link.reannounce_native(Child::default(), native(4242, 12_345), 0);
    assert!(fake.child().is_none());
    assert!(begins(&fake).is_empty());
    assert!(!fake.verbs().iter().any(|verb| {
        verb.split_ascii_whitespace()
            .any(|word| word.starts_with("child=") || word.starts_with("child-start="))
    }));
}

#[test]
fn c4_keeps_its_epoch_child_and_never_activates_c3_tracking() {
    let (fake, mut link) = host(
        "c3-on-c4",
        "child-proof=1 child-start=1",
        true,
        Contract::Four,
    );
    assert_eq!(link.before_spawn(None, 0), ChildStart::NoBarrier);
    let child = Child {
        process: Some(ChildProcess {
            pid: 4242,
            start: 987_654,
        }),
        ..Child::default()
    };
    link.reannounce_native(child, native(4242, 12_345), 0);
    assert_eq!(fake.child().as_deref(), Some("4242:987654"));
    assert!(begins(&fake).is_empty());
    assert!(!fake.verbs().iter().any(|verb| verb == "native-act info"));
}

#[test]
fn a_refused_begin_retries_the_same_generation_then_drops_both_connections() {
    let (fake, mut link) = host(
        "c3-refused-begin",
        "child-proof=1 child-start=1",
        true,
        Contract::Three,
    );
    fake.set_child_start_reply(Some("ERR beginning unavailable; retry"));
    assert!(matches!(
        link.before_spawn(None, 0),
        ChildStart::Unclaimed(_)
    ));
    assert!(link.is_lost() && !link.judges() && link.event_fd().is_none());
    assert!(link.grant().is_none());
    let requests = begins(&fake);
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    let until = Instant::now() + Duration::from_secs(60);
    while fake.holder().is_some() {
        assert!(
            Instant::now() < until,
            "the closed request claim did not retire"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn acknowledged_pending_generation_survives_release_and_reconnect_until_retirement() {
    for reconnect in [false, true] {
        let (fake, mut link) = host(
            "c3-pending-continuation",
            "child-proof=1 child-start=1",
            true,
            Contract::Three,
        );
        let generation = pending(&mut link, 0);
        if reconnect {
            lose(&fake, &mut link);
        } else {
            let on = POLICY_ON.replace("contract=2,3,4", "contract=3");
            let off = on.replace("effective=on why=enabled", "effective=off why=disabled");
            fake.set_policy_quietly(&off);
            link.safe_point(Duration::ZERO, 1);
            assert!(!link.judges(), "the policy released the claim");
            assert!(fake.holder().is_none());
            fake.set_policy_quietly(&on);
        }
        link.tick_native(
            Child::default(),
            NativeObservation::Unknown,
            1 + RECLAIM_EVERY_MS,
        );
        assert!(link.judges());
        assert!(fake.child().is_none());
        assert_eq!(
            fake.claim_words().unwrap().child_start,
            Some(generation.to_string())
        );
        link.reannounce_native(Child::default(), native(4242, 12_345), 2 + RECLAIM_EVERY_MS);
        assert_eq!(fake.child().as_deref(), Some("4242:12345"));
        assert_eq!(
            fake.claim_words().unwrap().child_start,
            Some(generation.to_string())
        );

        link.child_ended();
        fake.restart();
        link.events(Child::default(), 3 + RECLAIM_EVERY_MS);
        assert!(link.is_lost());
        link.tick_native(
            Child::default(),
            NativeObservation::Unknown,
            3 + RECLAIM_EVERY_MS * 2,
        );
        assert!(link.judges());
        assert!(fake.child().is_none());
        assert!(
            fake.claim_words().unwrap().child_start.is_none(),
            "retirement removed the old generation"
        );
    }
}

#[test]
fn same_child_metadata_ack_and_failure_preserve_the_hold_until_a_new_decision() {
    for accepted in [true, false] {
        let (fake, mut link) = host("c3-renewal-hold", "child-proof=1", true, Contract::Three);
        assert_eq!(link.before_spawn(None, 0), ChildStart::NoBarrier);
        link.reannounce_native(Child::default(), native(4242, 12_345), 0);
        fake.set_status("hold", Some("1"));
        let hold = match link.safe_point(Duration::ZERO, 1) {
            Verdict::Hold(hold) => hold,
            other => panic!("the host's current hold must be retained: {other:?}"),
        };
        if !accepted {
            fake.set_claim_reply("ERR renewal unavailable");
        }
        link.reannounce_native(Child::default(), native(4242, 12_345), 2);
        if accepted {
            assert!(link.judges());
            assert!(
                matches!(&link.state, State::Held(claimed) if claimed.last_hold.as_ref() == Some(&hold)),
                "a metadata ACK is not a fresh safe-point decision"
            );
            fake.restart();
            link.events(Child::default(), 3);
        }
        assert!(link.is_lost());
        assert!(matches!(&link.state, State::Lost { held: Some(saved), .. } if saved == &hold));
        assert!(link.lost_hold().is_some());
        link.tick_native(
            Child::default(),
            NativeObservation::Unknown,
            3 + RECLAIM_EVERY_MS,
        );
        assert!(
            link.lost_hold().is_some(),
            "an unknown observation cannot withdraw a host hold"
        );
        fake.set_claim_reply("OK");
        fake.set_status("hold", Some("0"));
        link.tick_native(
            Child::default(),
            native(4242, 12_345),
            3 + RECLAIM_EVERY_MS * 2,
        );
        assert!(link.judges());
        assert_eq!(
            link.safe_point(Duration::ZERO, 3 + RECLAIM_EVERY_MS * 2),
            Verdict::Safe
        );
        assert!(
            link.lost_hold().is_none(),
            "normal fresh host decisions replace the old hold"
        );
    }
}

#[test]
fn positive_retirement_and_fresh_begin_outcomes_preserve_the_host_hold() {
    for refused in [false, true] {
        let (fake, mut link) = host(
            "c3-retired-hold",
            "child-proof=1 child-start=1",
            true,
            Contract::Three,
        );
        let _ = pending(&mut link, 0);
        link.reannounce_native(Child::default(), native(4242, 12_345), 0);
        fake.set_status("hold", Some("1"));
        let hold = match link.safe_point(Duration::ZERO, 1) {
            Verdict::Hold(hold) => hold,
            other => panic!("the host's current hold must be retained: {other:?}"),
        };
        link.child_ended();
        assert!(matches!(&link.state, State::Held(claimed)
        if claimed.last_hold.as_ref() == Some(&hold)
            && claimed.native.known.is_none() && claimed.native.generation.is_none()));
        let expected_generation = if refused {
            fake.set_child_start_reply(Some("ERR fresh begin unavailable; retry"));
            assert!(matches!(
                link.before_spawn(None, 2),
                ChildStart::Unclaimed(_)
            ));
            None
        } else {
            let generation = pending(&mut link, 2);
            assert!(
                matches!(&link.state, State::Held(claimed)
            if claimed.last_hold.as_ref() == Some(&hold)),
                "pending-generation proof is not a new safe-point decision"
            );
            fake.restart();
            link.events(Child::default(), 3);
            Some(generation)
        };
        let State::Lost { held, native, .. } = &link.state else {
            panic!("the failed begin or connection loss closes the link");
        };
        assert_eq!(held.as_ref(), Some(&hold));
        assert!(native.known.is_none());
        assert_eq!(native.generation, expected_generation);
        assert!(link.event_fd().is_none() && !link.judges());
        assert!(link.lost_hold().is_some());
    }
}

#[test]
fn a_failed_fresh_begin_cannot_reuse_an_earlier_unbound_generation() {
    let (fake, mut link) = host(
        "c3-failed-fallback-begin",
        "child-proof=1 child-start=1",
        true,
        Contract::Three,
    );
    let first = pending(&mut link, 0);
    // The first local spawn failed before a native child existed. Its fallback
    // is a fresh attempt, whose begin now fails rather than being acknowledged.
    fake.set_child_start_reply(Some("ERR fallback begin unavailable; retry"));
    assert!(matches!(
        link.before_spawn(None, 1),
        ChildStart::Unclaimed(_)
    ));
    assert!(link.is_lost() && link.event_fd().is_none());
    let attempts = begins(&fake);
    assert_eq!(attempts.len(), 3);
    assert_eq!(attempts[1], attempts[2], "the new attempt alone is retried");
    let generation = attempts[1]
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("child-start="))
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!(generation > first.get());

    fake.set_child_start_reply(None);
    link.tick_native(Child::default(), native(4243, 12_346), 1 + RECLAIM_EVERY_MS);
    assert!(link.judges());
    assert_eq!(fake.child().as_deref(), Some("4243:12346"));
    assert!(
        fake.claim_words().unwrap().child_start.is_none(),
        "an unacknowledged fresh attempt cannot inherit the earlier pending generation"
    );
}

#[test]
fn rejected_native_claim_metadata_does_not_consume_a_generation() {
    let (fake, mut link) = host(
        "c3-rejected-metadata",
        "child-proof=1 child-start=1",
        true,
        Contract::Three,
    );
    let generation = pending(&mut link, 0);
    let attempted = generation.get().checked_add(1).unwrap();
    let holder = config(true, Contract::Three).holder(std::process::id());
    for (scope, version) in [("relaunch,upgrade", "malformed"), ("state", "0.2.0")] {
        let request = format!(
            "@{} meta set supervisor {holder} contract=3 scope={scope} version={version} child-start={attempted}",
            fake.sid
        );
        let State::Held(claimed) = &mut link.state else {
            panic!("the original claim is held");
        };
        let reply = claimed.conn.request_line(&request).unwrap();
        assert!(reply.starts_with("ERR"));
        assert_eq!(
            fake.claim_words().unwrap().child_start,
            Some(generation.to_string())
        );
        assert_eq!(
            crate::c3_child::highest(&fake.status_line()).unwrap(),
            generation.get()
        );
    }
}

#[test]
fn native_sampling_waits_for_pending_completion_or_due_reclaim() {
    let (fake, mut link) = host(
        "c3-lazy-observation",
        "child-proof=1",
        true,
        Contract::Three,
    );
    let sampled = std::cell::Cell::new(0);
    let observe = || {
        sampled.set(sampled.get() + 1);
        native(4242, 12_345)
    };
    link.tick_native_with(Child::default(), 0, observe);
    assert_eq!(sampled.get(), 1);
    assert_eq!(fake.child().as_deref(), Some("4242:12345"));
    for now in 1..4 {
        link.tick_native_with(Child::default(), now, observe);
    }
    assert_eq!(sampled.get(), 1, "steady held polls do not sample");
    lose(&fake, &mut link);
    link.tick_native_with(Child::default(), 2, observe);
    assert_eq!(sampled.get(), 1, "reclaim is not due");
    link.tick_native_with(Child::default(), 1 + RECLAIM_EVERY_MS, observe);
    assert_eq!(sampled.get(), 2, "one due reclaim samples once");
    assert!(link.judges());
}

#[test]
fn c4_and_untracked_c3_never_invoke_the_native_observer() {
    for (tracked, contract) in [(true, Contract::Four), (false, Contract::Three)] {
        let (fake, mut link) = host(
            "c3-unused-observer",
            "child-proof=1 child-start=1",
            tracked,
            contract,
        );
        let unneeded = || panic!("this link does not need native C3 observations");
        link.tick_native_with(Child::default(), 0, unneeded);
        lose(&fake, &mut link);
        link.tick_native_with(Child::default(), 1 + RECLAIM_EVERY_MS, unneeded);
        assert!(link.judges());
    }
}
