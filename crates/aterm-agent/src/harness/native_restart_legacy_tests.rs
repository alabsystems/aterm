// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts::{AuthRoute, Mode};
use crate::harness::native_act::{ColdBilling, ColdIntent};

pub(super) fn old_prepare(seed: &St, stored: bool) -> (Request, String) {
    let Request::Prepare(mut prepare) = Request::parse(&seed.native_prepare).unwrap() else {
        unreachable!();
    };
    // An older harness's prepare carries neither stamp.
    prepare.boot = None;
    prepare.custody = None;
    let NativePlan::Restart { cold, .. } = &mut prepare.plan else {
        unreachable!();
    };
    *cold = Some(Box::new(ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: if stored {
            AuthRoute::Stored
        } else {
            AuthRoute::Outside
        },
        conversation: None,
        profile: None,
        mode: Mode::Wall,
        billing: ColdBilling::Keep,
    }));
    let request = Request::Prepare(prepare);
    let current = request.encode().unwrap();
    let (prefix, _) = current.rsplit_once(" cold=").unwrap();
    let old_cold = format!(
        "{{\"login\":null,\"home\":null,\"auth_observed\":true,\"stored_login\":{stored},\"conversation\":null,\"profile\":null,\"mode\":\"wall\",\"billing\":\"Keep\"}}"
    );
    let legacy = format!(
        "{prefix} cold={}",
        aterm_codec::hex::encode_lower(old_cold.as_bytes())
    );
    assert_ne!(legacy, current);
    assert_eq!(Request::parse(&legacy), Some(request.clone()));
    (request, legacy)
}

#[test]
fn recovery_replays_the_exact_old_wire_only_after_its_durable_save() {
    let seed_home = aterm_tempfile::tempdir().unwrap();
    let command = planned();
    let seed = lost_prepare(&opts(seed_home.path()), &command);
    for stored in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let (mut st, _) = state(&command);
        let (_, legacy) = old_prepare(&seed, stored);
        st.native_prepare = legacy.clone();
        let durable_opts = opts.clone();
        let expected = format!("@{SID} native-act {legacy}");
        let (mut client, worker) = endpoint(move |line| {
            assert_eq!(
                line, expected,
                "an older GUI must see the original spelling"
            );
            let saved = upgrade_drive::load(&durable_opts, "conversation").unwrap();
            assert_eq!(line, format!("@{SID} native-act {}", saved.native_prepare));
            Some("OK native-act state=unknown\n".into())
        });
        let fault = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
        assert_eq!(
            recover(&opts, "conversation", &mut client, SID, &mut st),
            Err("native-unsaved")
        );
        drop(fault);
        assert_eq!(
            recover(&opts, "conversation", &mut client, SID, &mut st),
            Err("native-unknown")
        );
        assert_eq!(st.native_prepare, legacy);
        drop(client);
        assert_eq!(worker.join().unwrap().len(), 1);
    }
}

#[test]
fn retained_exchange_rejects_alternate_bytes_before_writing_any_request() {
    let home = aterm_tempfile::tempdir().unwrap();
    let seed = lost_prepare(&opts(home.path()), &planned());
    let (request, legacy) = old_prepare(&seed, true);
    let current = request.encode().unwrap();
    let (mut client, worker) = endpoint(|_| panic!("invalid retained bytes reached the host"));
    for bad in [
        format!("{legacy}\n@other close"),
        format!(" {legacy}"),
        legacy.replace("prepare id=", "prepare  id="),
        format!("{legacy} extra=1"),
        current.replace("deadline-us=", "deadline-us=0"),
        "info".into(),
    ] {
        assert!(native_act::exchange_retained(&mut client, SID, &request, &bad).is_err());
    }
    assert!(native_act::exchange_retained(&mut client, SID, &Request::Info, "info").is_err());
    assert!(native_act::exchange_retained(&mut client, "bad\nclose", &request, &legacy).is_err());
    drop(client);
    assert!(worker.join().unwrap().is_empty());
}

#[test]
fn legacy_encoding_cannot_drop_a_new_route_or_settled_image() {
    let home = aterm_tempfile::tempdir().unwrap();
    let seed = lost_prepare(&opts(home.path()), &planned());
    let (request, _) = old_prepare(&seed, true);
    let Request::Prepare(mut prepare) = request else {
        unreachable!()
    };
    let NativePlan::Restart {
        cold: Some(cold), ..
    } = &mut prepare.plan
    else {
        unreachable!()
    };
    cold.route = AuthRoute::VaultToken {
        entry: crate::harness::accounts::TokenEntry::parse(
            "aterm-claude-token-ac-0123456789abcdef",
        )
        .unwrap(),
    };
    assert!(
        Request::prepare(prepare.as_ref().clone())
            .encode_legacy_cold()
            .is_none()
    );
    let NativePlan::Restart {
        cold: Some(cold), ..
    } = &mut prepare.plan
    else {
        unreachable!()
    };
    cold.route = AuthRoute::Stored;
    for case in 0..4 {
        let mut hybrid = prepare.as_ref().clone();
        let NativePlan::Restart {
            forward, rollback, ..
        } = &mut hybrid.plan
        else {
            unreachable!()
        };
        let command = if case < 2 {
            forward.as_mut()
        } else {
            rollback.insert(forward.clone()).as_mut()
        };
        if case % 2 == 0 {
            command.settles = Some(command.image.clone());
        } else {
            command.account_intent = Some(Box::new(
                crate::harness::accounts_pool::ExecutionIntent::Token {
                    namespace: "11".repeat(32),
                    login: None,
                    owner: None,
                    entry: crate::harness::accounts::TokenEntry::parse(
                        "aterm-claude-token-ac-0123456789abcdef",
                    )
                    .unwrap(),
                },
            ));
        }
        let request = Request::prepare(hybrid);
        assert!(request.encode().is_some(), "current encoding remains valid");
        assert!(request.encode_legacy_cold().is_none(), "hybrid case {case}");
    }
}

#[test]
fn a_decodable_but_noncanonical_preparation_is_never_persisted_or_replayed() {
    let seed_home = aterm_tempfile::tempdir().unwrap();
    let command = planned();
    let seed = lost_prepare(&opts(seed_home.path()), &command);
    let (_, legacy) = old_prepare(&seed, true);
    for bad in [
        format!(" {legacy}"),
        legacy.replace("prepare id=", "prepare\tid="),
    ] {
        assert!(
            Request::parse(&bad).is_some(),
            "exercise the exact-byte guard"
        );
        let home = aterm_tempfile::tempdir().unwrap();
        let opts = opts(home.path());
        let (mut st, _) = state(&command);
        st.native_prepare = bad;
        let original = st.clone();
        let (mut client, worker) = endpoint(|_| panic!("invalid preparation sent"));
        assert_eq!(
            recover(&opts, "conversation", &mut client, SID, &mut st),
            Err("native-plan")
        );
        assert_eq!(st, original);
        assert!(!opts.state.exists(), "no record was written");
        drop(client);
        assert!(worker.join().unwrap().is_empty());
    }
}
