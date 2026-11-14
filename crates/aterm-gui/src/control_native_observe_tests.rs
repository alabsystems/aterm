// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_act::{Image, Process};
use aterm_agent::harness::native_observe::{
    ObservationBinding, Registration, Reply, Request as ObservationRequest,
};

#[cfg(unix)]
pub(crate) fn continuity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    use crate::session_timeline::{self, SupervisorAuthority};
    use aterm_agent::harness::contract::Scope as Duty;
    let handle = crate::session_store::test_handle_on(7622, -1);
    let (_client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let _serving = ServingFd::enter(&server);
    let _direct = DirectPeer::enter();
    let _principal = ServingPrincipal::enter(Principal::Harness);
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERVING_CONNECTION.with(|connection| connection.set(self.0));
        }
    }
    let _restore = Restore(SERVING_CONNECTION.with(|connection| connection.replace(Some(72))));
    assert_eq!(serving_peer_pid(), Some(std::process::id()));
    let holder = crate::harness_host::holder();
    let mut now = 100;
    let mut captured = 0;
    let mut original: Option<SupervisorAuthority> = None;
    let mut proof = 0;
    let mut perform = |action: &str| match action {
        "Capture" => {
            let meta = handle.ctx.meta.lock().unwrap();
            original = Some(meta.supervisor_authority(now));
            captured = i64::try_from(
                meta.builtin_relaunch_epoch
                    .test_value()
                    .expect("live capture epoch"),
            )
            .unwrap();
        }
        "NearExhaustion" => {
            handle.ctx.meta.lock().unwrap().builtin_relaunch_epoch =
                session_timeline::RelaunchEpoch::for_test(u64::MAX - 1);
        }
        "OwnClaim" | "OwnRetake" | "OwnRenew" => {
            session_timeline::claim_supervisor(
                &handle.ctx,
                &holder,
                (action == "OwnClaim").then_some(72),
                (action != "OwnClaim").then_some(200),
                now,
            )
            .unwrap();
        }
        "OwnRelease" | "ForeignReturn" | "OverflowReturn" => {
            assert!(session_timeline::release_supervisor_held_by(
                &handle.ctx,
                &holder,
                now,
                aterm_slog::Actor::Ctl
            ));
        }
        "ForeignRelaunch" | "OverflowForeign" => {
            let _instance = ServingPrincipal::enter(Principal::Instance);
            session_timeline::claim_supervisor(&handle.ctx, &holder, None, Some(200), now).unwrap();
        }
        "ForeignUpgradeOnly" => {
            session_timeline::claim_supervisor_scoped(
                &handle.ctx,
                "grok-alab@4242",
                Some(73),
                session_timeline::scoped_input_tests::scoped(&[Duty::Upgrade]),
                now,
                aterm_slog::Actor::Ctl,
            )
            .unwrap();
        }
        "ForeignExpiry" => now = 201,
        "ProveOwnContinuity"
        | "ProveAfterForeignReturn"
        | "ProveAfterForeignExpiry"
        | "ProveAfterOverflow"
        | "ProveUnrelatedDuty" => {
            proof = i64::from(original.as_ref().unwrap().same_builtin_relaunch_continuity(
                &handle.ctx.meta.lock().unwrap().supervisor_authority(now),
            ));
        }
        "CaptureAfterOverflow" => {
            let authority = handle.ctx.meta.lock().unwrap().supervisor_authority(now);
            proof = i64::from(authority.same_builtin_relaunch_continuity(&authority));
        }
        _ => panic!("unknown continuity action {action}"),
    };
    let mut prefix = vec!["Capture"];
    match state["phase"] {
        0 => prefix.clear(),
        1 => (),
        2 | 15 | 3 | 4 | 5 => {
            prefix.push("OwnClaim");
            let renewed = match state["phase"] {
                15 => true,
                3 => state["revision"] == 3,
                4 | 5 => state["revision"] == 4,
                _ => false,
            };
            if renewed {
                prefix.push("OwnRenew");
            }
            if matches!(state["phase"], 3..=5) {
                prefix.push("OwnRelease");
            }
            if matches!(state["phase"], 4 | 5) {
                prefix.push("OwnRetake");
            }
            if state["phase"] == 5 {
                prefix.push("ProveOwnContinuity");
            }
        }
        7..=11 => {
            prefix.push("ForeignRelaunch");
            if matches!(state["phase"], 8 | 9) {
                prefix.push("ForeignReturn");
            }
            if state["phase"] == 9 {
                prefix.push("ProveAfterForeignReturn");
            }
            if matches!(state["phase"], 10 | 11) {
                prefix.push("ForeignExpiry");
            }
            if state["phase"] == 11 {
                prefix.push("ProveAfterForeignExpiry");
            }
        }
        12 | 13 => {
            prefix.push("ForeignUpgradeOnly");
            if state["phase"] == 13 {
                prefix.push("ProveUnrelatedDuty");
            }
        }
        16..=20 => {
            prefix.push("NearExhaustion");
            if state["phase"] >= 17 {
                prefix.push("OverflowForeign");
            }
            if state["phase"] >= 18 {
                prefix.push("OverflowReturn");
            }
            if state["phase"] == 19 {
                prefix.push("ProveAfterOverflow");
            }
            if state["phase"] == 20 {
                prefix.push("CaptureAfterOverflow");
            }
        }
        other => panic!("unknown continuity fixture phase {other}"),
    }
    for prior in prefix {
        perform(prior);
    }
    perform(action);
    let mut next = state.clone();
    next.insert(
        "phase",
        match action {
            "Capture" => 1,
            "OwnClaim" => 2,
            "OwnRenew" => 15,
            "OwnRelease" => 3,
            "OwnRetake" => 4,
            "ProveOwnContinuity" => 5,
            "ForeignRelaunch" => 7,
            "ForeignReturn" => 8,
            "ProveAfterForeignReturn" => 9,
            "ForeignExpiry" => 10,
            "ProveAfterForeignExpiry" => 11,
            "ForeignUpgradeOnly" => 12,
            "ProveUnrelatedDuty" => 13,
            "NearExhaustion" => 16,
            "OverflowForeign" => 17,
            "OverflowReturn" => 18,
            "ProveAfterOverflow" => 19,
            "CaptureAfterOverflow" => 20,
            _ => unreachable!(),
        },
    );
    let meta = handle.ctx.meta.lock().unwrap();
    let authority = meta.supervisor_authority(now);
    next.insert("revision", i64::try_from(authority.revision()).unwrap());
    next.insert(
        "epoch",
        match meta.builtin_relaunch_epoch.test_value() {
            Some(epoch) if epoch == u64::MAX - 1 => 1,
            Some(u64::MAX) | None => 2,
            Some(epoch) => i64::try_from(epoch).unwrap(),
        },
    );
    next.insert(
        "exhausted",
        i64::from(meta.builtin_relaunch_epoch.test_value().is_none()),
    );
    next.insert(
        "builtin",
        i64::from(authority.same_builtin_relaunch_continuity(&authority)),
    );
    next.insert("captured", captured);
    next.insert("proof", proof);
    next
}

#[test]
#[cfg(unix)]
fn authenticated_builtin_claim_lifecycle_preserves_passive_continuity() {
    let base = aterm_models_harness::native_observation_continuity_model();
    for actions in [
        &[
            "Capture",
            "OwnClaim",
            "OwnRenew",
            "OwnRelease",
            "OwnRetake",
            "ProveOwnContinuity",
        ][..],
        &[
            "Capture",
            "ForeignRelaunch",
            "ForeignReturn",
            "ProveAfterForeignReturn",
        ][..],
        &[
            "Capture",
            "ForeignRelaunch",
            "ForeignExpiry",
            "ProveAfterForeignExpiry",
        ][..],
        &["Capture", "ForeignUpgradeOnly", "ProveUnrelatedDuty"][..],
        &[
            "Capture",
            "NearExhaustion",
            "OverflowForeign",
            "OverflowReturn",
            "ProveAfterOverflow",
        ][..],
        &[
            "Capture",
            "NearExhaustion",
            "OverflowForeign",
            "OverflowReturn",
            "CaptureAfterOverflow",
        ][..],
    ] {
        let mut state = base.init_state();
        let mut caught = std::collections::BTreeSet::new();
        for action in actions {
            let actual = continuity_transition(&state, action);
            let mut expected = state.clone();
            assert!(base.fire(action, &mut expected));
            assert_eq!(actual, expected, "actual continuity action {action}");
            for bug in [1, 2, 3, 4, 5, 6, 7, 8] {
                let mutant = aterm_spec::interp::with_buggy(&base, bug);
                let mut bad = state.clone();
                assert!(mutant.fire(action, &mut bad));
                if actual != bad {
                    caught.insert(bug);
                }
            }
            state = actual;
        }
        let required: &[i64] = match actions[1] {
            "OwnClaim" => &[1, 2, 8],
            "ForeignRelaunch" => {
                if actions[2] == "ForeignReturn" {
                    &[3, 4]
                } else {
                    &[3]
                }
            }
            "NearExhaustion" => {
                if actions.last() == Some(&"CaptureAfterOverflow") {
                    &[6, 7]
                } else {
                    &[6]
                }
            }
            _ => &[5],
        };
        for bug in required {
            assert!(caught.contains(bug), "actual path catches mutant {bug}");
        }
    }
}

/// THE BUILT-IN HARNESS'S OWN LEASE RENEWS IN PLACE, A LOOKALIKE'S NEVER: aterm's harness renewing
/// its `ttl=` claim keeps the revision, while the same holder text renewed by another principal,
/// every field of the claim but its lease the same, is no authenticated built-in custody: it moves
/// the authority's Relaunch epoch, and the revision with it.
#[test]
#[cfg(unix)]
fn only_the_builtin_harness_renews_its_own_lease_in_place() {
    use crate::session_timeline::claim_supervisor;
    let handle = crate::session_store::test_handle_on(7624, -1);
    let (_client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let _serving = ServingFd::enter(&server);
    let _direct = DirectPeer::enter();
    let _principal = ServingPrincipal::enter(Principal::Harness);
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERVING_CONNECTION.with(|connection| connection.set(self.0));
        }
    }
    let _restore = Restore(SERVING_CONNECTION.with(|connection| connection.replace(Some(72))));
    let holder = crate::harness_host::holder();
    let authority = || handle.ctx.meta.lock().unwrap().supervisor_authority(100);
    claim_supervisor(&handle.ctx, &holder, None, Some(200), 100).unwrap();
    let held = authority();
    claim_supervisor(&handle.ctx, &holder, None, Some(300), 100).unwrap();
    assert_eq!(authority(), held, "the harness's own lease renews in place");
    let _instance = ServingPrincipal::enter(Principal::Instance);
    claim_supervisor(&handle.ctx, &holder, None, Some(300), 100).unwrap();
    let lookalike = authority();
    assert!(!held.same_builtin_relaunch_continuity(&lookalike));
    assert_ne!(
        lookalike.revision(),
        held.revision(),
        "a lookalike's renewal is a new authority"
    );
}

fn requests() -> [ObservationRequest; 2] {
    let process = |pid, parent| Process {
        pid,
        birth: 1,
        parent,
        group: pid,
        session: 22,
        image: Image {
            path: b"/synthetic/vendor".to_vec(),
            device: 1,
            inode: 1,
        },
    };
    [
        ObservationRequest::Register(Registration {
            id: "1".repeat(32),
            host_epoch: "2".repeat(32),
            vendor: process(33, 22),
            shell: process(22, 1),
        }),
        ObservationRequest::Refresh(ObservationBinding {
            id: "1".repeat(32),
            host_epoch: "2".repeat(32),
            capability: "3".repeat(64),
        }),
    ]
}

#[test]
fn passive_observation_requests_reach_the_session_frontend_without_granting_authority() {
    let handle = crate::session_store::test_handle_on(7621, -1);
    struct RestoreConnection(Option<u64>);
    impl Drop for RestoreConnection {
        fn drop(&mut self) {
            SERVING_CONNECTION.with(|connection| connection.set(self.0));
        }
    }
    let _restore =
        RestoreConnection(SERVING_CONNECTION.with(|connection| connection.replace(None)));
    for observation in requests() {
        let tail = observation
            .encode()
            .expect("valid bounded synthetic request");
        assert_eq!(ObservationRequest::parse(&tail), Some(observation));
        let request = Request::parse(format!("@self native-observe {tail}"));
        let (routed, _) = route(request.head().verb, request.form()).unwrap();
        assert_eq!(
            routed,
            Route::Session,
            "the owner dispatcher has no observation handler"
        );

        for (principal, connection) in [(Principal::Instance, Some(71)), (Principal::Harness, None)]
        {
            SERVING_CONNECTION.with(|current| current.set(connection));
            let _principal = ServingPrincipal::enter(principal);
            let reply = crate::native_act::serve_observation(&handle.ctx, -1, &tail);
            assert!(
                matches!(Reply::parse(&reply), Some(Reply::Unknown)),
                "routing alone supplies neither Harness connection nor process proof"
            );
        }
    }
}
