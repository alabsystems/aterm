// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::transport::{self, AdapterReply, Call, Endpoint, Status};
use crate::resources::{Observation, Outcome};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) fn transport_transition(
    world: &super::broker_tests::World,
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(world, before, action, false)
}

fn transition_with(
    (f, broker, token, _): &super::broker_tests::World,
    before: &aterm_spec::interp::State,
    action: &str,
    omit_eof: bool,
) -> aterm_spec::interp::State {
    let entered = AtomicUsize::new(0);
    let run = |request: &crate::resources::BoundRequest, credential: &[u8], until: Instant| {
        assert_eq!(request.payload(), b"private request");
        assert_eq!(credential, b"synthetic-private-credential");
        assert!(until <= deadline());
        entered.fetch_add(1, Ordering::SeqCst);
        Some(
            AdapterReply::new(
                b"synthetic answer",
                Some(Observation::new(Outcome::Completed, [5; 32]).unwrap()),
            )
            .unwrap(),
        )
    };
    let (served, reply) = transport::tests::exchange(broker, token, action, omit_eof, run);
    if action == "Fresh" {
        assert!(served.is_ok(), "server: {served:?}");
        assert!(reply.is_ok(), "client: {reply:?}");
    }
    let mut returned = reply.is_ok_and(|reply| {
        assert!(reply.status != Status::Unconfirmed);
        reply.status != Status::Refused
    });
    if action == "LostReply" {
        assert!(served.is_err());
        assert_eq!(
            entered.load(Ordering::SeqCst),
            1,
            "lost reply server: {served:?}"
        );
        let (_, retry) = transport::tests::exchange(broker, token, "Retry", false, |_, _, _| {
            panic!("lost reply replay")
        });
        let retry = retry.unwrap();
        assert_eq!(retry.status, Status::Completed);
        assert!(!retry.entered);
        assert!(retry.body.is_empty());
    } else if action == "Retry" {
        let (_, retry) =
            transport::tests::exchange(broker, token, "Retry", false, |_, _, _| panic!("replay"));
        let retry = retry.unwrap();
        assert_eq!(retry.status, Status::Completed);
        assert!(!retry.entered);
        returned = true;
    }
    let debit = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    let scenario = [
        "Fresh",
        "Trailing",
        "ShortHeader",
        "ShortBody",
        "Oversize",
        "Version",
        "Expired",
        "WrongToken",
        "WrongAdapter",
        "LostReply",
        "Retry",
        "NoEof",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let mut next = before.clone();
    next.insert("scenario", scenario as i64);
    next.insert("entered", entered.load(Ordering::SeqCst) as i64);
    next.insert("debit", debit as i64);
    next.insert("returned", i64::from(returned));
    next
}

/// The model's steps on three worlds: the nine frames the broker refuses with nothing entered or
/// charged, then `Fresh` on the same world, whose acceptance shows none of them burned the token
/// or left a charge; `LostReply` and `Retry` each on its own, since each changes the world.
/// GAP: a refused frame no longer always meets a pristine broker (`Retry`'s first frame does). A
/// refusal only because of what an earlier refused frame changed, which `Fresh` does not expose,
/// is not seen.
#[test]
fn broker_transport_conforms_and_eof_barrier_is_necessary() {
    let model = aterm_models_db::broker_transport_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    let worlds: [&[&str]; 3] = [
        &[
            "Trailing",
            "ShortHeader",
            "ShortBody",
            "Oversize",
            "Version",
            "Expired",
            "WrongToken",
            "WrongAdapter",
            "NoEof",
            "Fresh",
        ],
        &["LostReply"],
        &["Retry"],
    ];
    // A new action falls out of the walk here, not silently.
    let mut named: Vec<_> = worlds.iter().copied().flatten().copied().collect();
    let mut actions: Vec<_> = model.actions.iter().map(|action| action.name).collect();
    named.sort_unstable();
    actions.sort_unstable();
    assert_eq!(named, actions);
    aterm_spec::fs_lane::each(&worlds, |steps| {
        let world = super::broker_tests::setup();
        for action in *steps {
            let next = super::transport_transition(&world, &initial, action);
            assert!(
                model.successors(action, &initial).contains(&next),
                "{action}: {next:?}"
            );
        }
    });
    let unsafe_next = transition_with(&super::broker_tests::setup(), &initial, "Trailing", true);
    assert!(!model.check_invariant("FrameBeforeEffect", &unsafe_next));
}

#[test]
fn broker_transport_private_endpoint_round_trip_and_uncertain_result() {
    let (_f, broker, token, _) = super::broker_tests::setup();
    let dir = transport::tests::socket_dir();
    aterm_uds::private_file::create_private_dir(dir.path()).unwrap();
    let endpoint = Endpoint::bind(dir.path()).unwrap();
    let until = deadline();
    let received = aterm_uds::secret::Secret::new(token.bytes());
    std::thread::scope(|scope| {
        let task = scope.spawn(|| {
            transport::call_until(
                dir.path(),
                Call {
                    token: received.expose(),
                    logical: [90; 16],
                    adapter: "example-v1",
                    payload: b"question",
                },
                until,
            )
        });
        let stream = loop {
            if let Some(stream) = endpoint.try_accept().unwrap() {
                break stream;
            }
            assert!(!task.is_finished(), "client failed before connect");
            assert!(Instant::now() < until);
            aterm_uds::deadline::poll_yield();
        };
        transport::serve_until(stream, &broker, deadline(), |request, credential, _| {
            assert_eq!(request.payload(), b"question");
            assert_eq!(credential, b"synthetic-private-credential");
            Some(
                AdapterReply::from_secret(aterm_uds::secret::Secret::new(b"partial answer"), None)
                    .unwrap(),
            )
        })
        .unwrap();
        let reply = task.join().unwrap().unwrap();
        assert_eq!(reply.status, Status::Unknown);
        assert!(reply.entered);
        assert_eq!(reply.body.expose(), b"partial answer");
        assert!(!format!("{reply:?}").contains("partial answer"));
    });
    // A retry observes retained uncertainty and never invokes the adapter again.
    broker
        .dispatch_until(
            "example-v1",
            token.bytes(),
            [90; 16],
            b"question",
            deadline(),
            |_, _| panic!("uncertain replay"),
        )
        .unwrap();
}
