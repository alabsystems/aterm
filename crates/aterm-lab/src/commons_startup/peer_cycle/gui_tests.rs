// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;

fn at_mut<'a>(value: &'a mut Value, path: &[&str]) -> &'a mut Value {
    let Some((key, rest)) = path.split_first() else {
        return value;
    };
    let next = match value {
        Value::Object(values) => values.get_mut(*key).expect("fixture object field"),
        Value::Array(values) => values
            .get_mut(key.parse::<usize>().expect("fixture array index"))
            .expect("fixture array entry"),
        _ => panic!("fixture path crosses a scalar"),
    };
    at_mut(next, rest)
}

fn backend() -> StageEvidence {
    StageEvidence {
        network: super::super::tests::observation(),
        account_observed_at_ms: 99_000,
        secret_observed_at_ms: 99_000,
    }
}

fn fixture(backend: &StageEvidence) -> Value {
    let identity = |identity: &MachineIdentity| {
        json!({
            "machine":aterm_codec::hex::encode_lower(&identity.machine_id),
            "incarnation":aterm_codec::hex::encode_lower(&identity.incarnation),
        })
    };
    let network = &backend.network;
    let peer = &network.peers[0];
    json!({"schema":1,"kind":"network-projection","composition_revision":500,
        "source_present":true,"reader_busy":false,
        "network":{"revision":network.revision,"observed_at_ms":network.observed_at_ms,
            "via_local_process":null,"observer":identity(network.observer.as_ref().unwrap()),
            "visibility":"Active","history":"Ready","peers":[{
                "slot":peer.slot,"identity":identity(&peer.identity),
                "first_seen_ms":peer.first_seen_ms,"last_seen_ms":peer.last_seen_ms,
                "last_checked_ms":peer.last_checked_ms,"lease_ms":peer.lease_ms,
                "reachability":"reachable",
            }]},
        "services":{"accounts":{"availability":"Cached","observed_at_ms":99_500,"last_success_ms":99_500},
            "secrets":{"availability":"Unknown","observed_at_ms":99_500,"last_success_ms":null}},
    })
}

fn body(value: &Value) -> String {
    format!(
        "app-inspection version=app/v1 revision=700 subject=network\n{}\n",
        aterm_json::to_string(value).unwrap()
    )
}

#[test]
fn follower_requires_the_current_owner_and_fresh_retained_samples() {
    let backend = backend();
    let expected = backend.network.observer.as_ref().unwrap();
    let mut value = fixture(&backend);
    *at_mut(&mut value, &["network", "via_local_process"]) = json!(42);
    // A producer reading its next sample does not invalidate the retained one.
    *at_mut(&mut value, &["reader_busy"]) = json!(true);
    assert!(follows_owner(&body(&value), 42, expected, Some(100_100)).unwrap());
    assert!(!follows_owner(&body(&value), 43, expected, Some(100_100)).unwrap());
    assert!(!follows_owner(&body(&value), 42, expected, None).unwrap());
    for (path, replacement) in [
        (vec!["source_present"], json!(false)),
        (vec!["network", "visibility"], json!("Waiting")),
        (vec!["network", "history"], json!("Unavailable")),
        (vec!["network", "observed_at_ms"], json!(1)),
        (vec!["network", "observed_at_ms"], json!(100_101)),
        (
            vec!["network", "observer", "incarnation"],
            json!("03".repeat(16)),
        ),
        (
            vec!["services", "accounts", "availability"],
            json!("Unknown"),
        ),
        (vec!["services", "accounts", "observed_at_ms"], json!(1)),
        (
            vec!["services", "accounts", "last_success_ms"],
            json!(100_101),
        ),
    ] {
        let mut refused = value.clone();
        *at_mut(&mut refused, &path) = replacement;
        assert!(!follows_owner(&body(&refused), 42, expected, Some(100_100)).unwrap());
    }
}

#[test]
fn gui_wait_uses_producer_and_service_samples_instead_of_the_ui_revision() {
    let backend = backend();
    let peer = &backend.network.peers[0].identity;
    let value = fixture(&backend);
    let valid = |value: &Value| {
        matches(
            &parse(&body(value)).unwrap(),
            &backend,
            42,
            peer,
            Some(100_100),
        )
    };
    assert!(valid(&value), "direct GUI source needs no relay PID");
    let mut relayed = value.clone();
    *at_mut(&mut relayed, &["network", "via_local_process"]) = json!(42);
    assert!(valid(&relayed));
    for field in 0..19 {
        let mut stale = value.clone();
        *at_mut(&mut stale, &["composition_revision"]) = json!(999_999);
        match field {
            0 => *at_mut(&mut stale, &["network", "revision"]) = json!(9),
            1 => *at_mut(&mut stale, &["network", "observed_at_ms"]) = json!(99_999),
            2 => *at_mut(&mut stale, &["services", "accounts", "observed_at_ms"]) = json!(98_999),
            3 => *at_mut(&mut stale, &["source_present"]) = json!(false),
            4 => *at_mut(&mut stale, &["reader_busy"]) = json!(true),
            5 => *at_mut(&mut stale, &["network", "via_local_process"]) = json!(43),
            6 => {
                *at_mut(&mut stale, &["network", "observer", "incarnation"]) =
                    json!("03".repeat(16))
            }
            7 => {
                *at_mut(&mut stale, &["network", "peers", "0", "reachability"]) = json!("last-seen")
            }
            8 => *at_mut(&mut stale, &["network", "peers", "0", "slot"]) = json!(8),
            9 => *at_mut(&mut stale, &["network", "peers", "0", "first_seen_ms"]) = json!(90_001),
            10 => *at_mut(&mut stale, &["network", "peers", "0", "last_seen_ms"]) = json!(98_000),
            11 => *at_mut(&mut stale, &["network", "peers", "0", "last_checked_ms"]) = Value::Null,
            12 => *at_mut(&mut stale, &["network", "observed_at_ms"]) = json!(100_101),
            13 => *at_mut(&mut stale, &["services", "accounts", "observed_at_ms"]) = json!(100_101),
            14 => {
                *at_mut(&mut stale, &["services", "accounts", "last_success_ms"]) = json!(100_101)
            }
            15 => *at_mut(&mut stale, &["services", "accounts", "availability"]) = json!("Unknown"),
            16 => *at_mut(&mut stale, &["services", "secrets", "availability"]) = json!("Cached"),
            17 => *at_mut(&mut stale, &["services", "secrets", "observed_at_ms"]) = json!(98_999),
            _ => *at_mut(&mut stale, &["services", "secrets", "observed_at_ms"]) = json!(100_101),
        }
        assert!(!valid(&stale), "invalid observation field {field}");
    }
    let projection = parse(&body(&value)).unwrap();
    assert!(!matches(&projection, &backend, 42, peer, None));
    assert!(!matches(&projection, &backend, 42, peer, Some(115_001)));
    let mut old_contact = value.clone();
    *at_mut(&mut old_contact, &["network", "observed_at_ms"]) = json!(116_000);
    assert!(
        !matches(
            &parse(&body(&old_contact)).unwrap(),
            &backend,
            42,
            peer,
            Some(116_001),
        ),
        "a fresh heartbeat cannot renew old peer contact"
    );
    let old_service_backend = StageEvidence {
        network: backend.network.clone(),
        account_observed_at_ms: 1,
        secret_observed_at_ms: backend.secret_observed_at_ms,
    };
    let mut old_service = value;
    *at_mut(
        &mut old_service,
        &["services", "accounts", "observed_at_ms"],
    ) = json!(1);
    *at_mut(
        &mut old_service,
        &["services", "accounts", "last_success_ms"],
    ) = json!(1);
    assert!(!matches(
        &parse(&body(&old_service)).unwrap(),
        &old_service_backend,
        42,
        peer,
        Some(100_100)
    ));
}

#[test]
fn gui_projection_rejects_malformed_versions_types_and_ambiguous_peers() {
    let value = fixture(&backend());
    let good = body(&value);
    for bad in [
        good.replace("app/v1", "app/v2"),
        good.replace("subject=network", "subject=tabs"),
        good.replace("revision=700", "revision=unknown"),
        format!("{good}extra\n"),
    ] {
        assert!(parse(&bad).is_err());
    }
    for field in 0..8 {
        let mut bad = value.clone();
        match field {
            0 => *at_mut(&mut bad, &["schema"]) = json!(2),
            1 => *at_mut(&mut bad, &["kind"]) = json!("other"),
            2 => *at_mut(&mut bad, &["reader_busy"]) = json!("false"),
            3 => *at_mut(&mut bad, &["network", "observer", "machine"]) = json!("ab"),
            4 => *at_mut(&mut bad, &["network", "visibility"]) = json!("Maybe"),
            5 => *at_mut(&mut bad, &["services", "accounts", "observed_at_ms"]) = json!("99500"),
            6 => {
                let mut peer = bad["network"]["peers"][0].clone();
                *at_mut(&mut peer, &["identity", "machine"]) = json!("03".repeat(16));
                at_mut(&mut bad, &["network", "peers"])
                    .as_array_mut()
                    .unwrap()
                    .push(peer);
            }
            _ => {
                let mut peer = bad["network"]["peers"][0].clone();
                *at_mut(&mut peer, &["slot"]) = json!(8);
                at_mut(&mut bad, &["network", "peers"])
                    .as_array_mut()
                    .unwrap()
                    .push(peer);
            }
        }
        assert!(
            parse(&body(&bad)).is_err(),
            "malformed observation field {field}"
        );
    }
}

#[test]
fn a_valid_pre_capture_projection_cannot_authorize_a_different_post_capture_state() {
    let backend = backend();
    let peer = &backend.network.peers[0].identity;
    let before = fixture(&backend);
    let mut after = before.clone();
    *at_mut(&mut after, &["network", "revision"]) = json!(11);
    *at_mut(&mut after, &["network", "observed_at_ms"]) = json!(100_001);
    *at_mut(&mut after, &["network", "peers", "0", "reachability"]) = json!("unknown");
    assert!(matches(
        &parse(&body(&before)).unwrap(),
        &backend,
        42,
        peer,
        Some(100_100)
    ));
    assert!(!matches(
        &parse(&body(&after)).unwrap(),
        &backend,
        42,
        peer,
        Some(100_100)
    ));
}

#[test]
fn restored_gui_requires_retained_contact_and_absent_lifetime_check() {
    let mut backend = backend();
    backend.network.via_local_process = Some(43);
    backend.network.peers[0].reachability = Reachability::LastSeen;
    backend.network.peers[0].last_checked_ms = None;
    let peer = &backend.network.peers[0].identity;
    let mut value = fixture(&backend);
    *at_mut(&mut value, &["network", "peers", "0", "reachability"]) = json!("last-seen");
    let valid = |value: &Value| {
        matches(
            &parse(&body(value)).unwrap(),
            &backend,
            43,
            peer,
            Some(100_100),
        )
    };
    assert!(valid(&value));
    for field in 0..6 {
        let mut wrong = value.clone();
        match field {
            0 => *at_mut(&mut wrong, &["network", "peers", "0", "last_seen_ms"]) = json!(99_001),
            1 => *at_mut(&mut wrong, &["network", "peers", "0", "last_checked_ms"]) = json!(99_000),
            2 => {
                *at_mut(&mut wrong, &["network", "peers", "0", "reachability"]) = json!("reachable")
            }
            3 => *at_mut(&mut wrong, &["network", "via_local_process"]) = json!(42),
            4 => *at_mut(&mut wrong, &["network", "peers", "0", "slot"]) = json!(8),
            _ => *at_mut(&mut wrong, &["network", "peers", "0", "first_seen_ms"]) = json!(90_001),
        }
        assert!(!valid(&wrong), "field {field}");
    }
}

/// A Reachable row is current by the lease its observer measured for that peer: a peer reached
/// every 24 s is Reachable 20 s after its contact, where one with no measured lease is not.
#[test]
fn a_reachable_row_is_current_within_its_measured_lease() {
    let mut backend = backend();
    backend.network.peers[0].last_seen_ms = 80_000;
    backend.network.peers[0].last_checked_ms = Some(80_000);
    backend.network.peers[0].lease_ms = Some(48_000);
    let peer = &backend.network.peers[0].identity;
    let value = fixture(&backend);
    let valid = |value: &Value| {
        matches(
            &parse(&body(value)).unwrap(),
            &backend,
            42,
            peer,
            Some(100_100),
        )
    };
    assert!(valid(&value));
    for lease in [json!(null), json!(20_000)] {
        let mut expired = value.clone();
        *at_mut(&mut expired, &["network", "peers", "0", "lease_ms"]) = lease.clone();
        assert!(!valid(&expired), "{lease:?}");
    }
    // The row names its lease, as every other field: a GUI that omits it is not this build's.
    let mut missing = value.clone();
    at_mut(&mut missing, &["network", "peers", "0"])
        .as_object_mut()
        .unwrap()
        .remove("lease_ms");
    assert!(parse(&body(&missing)).is_err());
}
