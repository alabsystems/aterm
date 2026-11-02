// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::accounts::broker::{self, Broker, Grant, Token};
use crate::resources::{BoundRequest, Observation, RequestKey};
use aterm_secrets::commons::bootstrap::Bootstrap;

pub(super) fn setup() -> (Fixture, Broker, Token, Grant) {
    setup_on(1)
}

fn setup_on(machines: usize) -> (Fixture, Broker, Token, Grant) {
    setup_on_with_lease_clock(machines, None)
}

fn setup_on_with_lease_clock(
    machines: usize,
    lease_clock: Option<Instant>,
) -> (Fixture, Broker, Token, Grant) {
    let f = Fixture::new(machines);
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![],
    );
    let prepared = f.source(31, 0).prepare_until(deadline()).unwrap();
    let grant = Grant {
        id: [61; 16],
        producer: [62; 32],
        actor: f.lab.bootstrap(0).request().unwrap().binding().actor(),
        binding: prepared.binding().clone(),
        lifetime: Duration::from_secs(300),
    };
    prepared.resources().initialize().unwrap();
    publish(&f, vec![grant.clone()]);
    let mut broker = broker(&f);
    if let Some(now) = lease_clock {
        broker.test_set_lease_time(now);
    }
    let token = broker
        .issue_until(grant.id, Duration::from_secs(120), deadline())
        .unwrap();
    (f, broker, token, grant)
}

#[test]
fn broker_succession_requires_a_new_grant_and_preserves_producer_history() {
    let (f, broker, token, mut grant) = setup_on(2);
    broker
        .dispatch_until(
            "example-v1",
            token.bytes(),
            [80; 16],
            b"private request",
            deadline(),
            |_, _| None,
        )
        .unwrap();
    let source = f.source(31, 0);
    let resources = super::dispatch_tests::succeed(&f, &source, grant.binding.revision);
    assert!(
        broker
            .issue_until(grant.id, Duration::from_secs(120), deadline())
            .is_err()
    );
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [81; 16],
                b"private request",
                deadline(),
                |_, _| panic!("old grant")
            )
            .is_err()
    );
    let selected = crate::application_paths_until(&f.lab.paths[0], deadline()).unwrap();
    grant.actor = Bootstrap::required(&selected)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    grant.binding = source.prepare_until(deadline()).unwrap().binding().clone();
    // Succession moved the second fixture's custody into the selected home.
    // Publish through the original root, then sync the surviving selected client.
    let admin = client::admin_open(&f.lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        broker::ENTRY,
        broker::Policy::new(vec![grant.clone()])
            .unwrap()
            .text()
            .as_bytes(),
    )
    .unwrap();
    client::admin_publish(&f.lab.paths[0], &admin, &doc).unwrap();
    client::sync(&selected, None).unwrap();
    let next = broker
        .issue_until(grant.id, Duration::from_secs(120), deadline())
        .unwrap();
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                next.bytes(),
                [80; 16],
                b"private request",
                deadline(),
                |_, _| panic!("ancestor replay")
            )
            .is_err()
    );
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                next.bytes(),
                [81; 16],
                b"private request",
                deadline(),
                |_, _| None
            )
            .unwrap()
            .entered
    );
    assert_eq!(resources.snapshot().unwrap().used.values().sum::<u64>(), 2);
}

#[test]
fn changing_grant_and_account_revision_does_not_reset_the_producer() {
    let (f, broker, token, mut grant) = setup();
    broker
        .dispatch_until(
            "example-v1",
            token.bytes(),
            [82; 16],
            b"private request",
            deadline(),
            |_, _| None,
        )
        .unwrap();
    f.issue(
        31,
        0,
        f.policy("example-v1").reference([41; 32]).unwrap(),
        vec![grant.binding.revision],
    );
    grant.binding = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .binding()
        .clone();
    grant.id = [83; 16];
    publish(&f, vec![grant.clone()]);
    let next = broker
        .issue_until(grant.id, Duration::from_secs(120), deadline())
        .unwrap();
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                next.bytes(),
                [82; 16],
                b"private request",
                deadline(),
                |_, _| panic!("new grant replay")
            )
            .is_err()
    );
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                next.bytes(),
                [84; 16],
                b"private request",
                deadline(),
                |_, _| None
            )
            .unwrap()
            .entered
    );
}

fn broker(f: &Fixture) -> Broker {
    Broker::at(
        f.lab.paths[0].clone(),
        client::config(&f.lab.paths[0]).unwrap().unwrap(),
    )
    .unwrap()
}

pub(super) fn publish(f: &Fixture, grants: Vec<Grant>) {
    let text = broker::Policy::new(grants).unwrap().text();
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, broker::ENTRY, text.as_bytes())
            .unwrap();
    });
}

fn observe(
    f: &Fixture,
    entered: &Cell<i64>,
    request: &BoundRequest,
    credential: &[u8],
) -> Option<Observation> {
    assert_eq!(credential, b"synthetic-private-credential");
    assert_eq!(request.payload(), b"private request");
    assert!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .attempt(request.id())
            .unwrap()
            .is_some()
    );
    entered.set(entered.get() + 1);
    None
}

pub(crate) fn broker_transition(
    before: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_with(before, action, false)
}

fn transition_with(
    before: &aterm_spec::interp::State,
    action: &str,
    bypass: bool,
) -> aterm_spec::interp::State {
    let (f, broker, token, grant) = setup();
    let entered = Cell::new(0);
    let mut scenario = 0;
    let accepted;
    let call = |b: &Broker, t: &[u8]| {
        b.dispatch_until(
            "example-v1",
            t,
            [63; 16],
            b"private request",
            deadline(),
            |r, c| observe(&f, &entered, r, c),
        )
        .is_ok_and(|r| r.refusal.is_none() && r.accounting.refusal.is_none())
    };
    for (index, name) in [
        "Fresh",
        "WrongToken",
        "OtherBroker",
        "Revoked",
        "Expired",
        "PolicyChanged",
        "RevokedAfter",
        "ExpiredAfter",
        "Rotated",
        "Restart",
        "WrongBilling",
        "PolicyAfter",
        "IssueRollback",
        "AbsentRollback",
        "RevocationContended",
    ]
    .into_iter()
    .enumerate()
    {
        if name == action {
            scenario = (index + 1) as i64;
        }
    }
    match action {
        "Fresh" => accepted = call(&broker, token.bytes()),
        "WrongToken" => accepted = call(&broker, &[0; 32]),
        "OtherBroker" => accepted = call(&self::broker(&f), token.bytes()),
        "Revoked" => {
            broker.revoke(token.bytes()).unwrap();
            accepted = call(&broker, token.bytes());
        }
        "RevocationContended" => {
            assert!(broker.test_revoke_after_contention(token.bytes()));
            accepted = call(&broker, token.bytes());
        }
        "Expired" => {
            broker.test_expire(token.bytes());
            accepted = call(&broker, token.bytes());
        }
        "PolicyChanged" => {
            publish(&f, vec![]);
            assert!(!call(&broker, token.bytes()));
            publish(&f, vec![grant.clone()]);
            accepted = call(&broker, token.bytes()); // A failed observation burns the old lease.
        }
        "IssueRollback" | "AbsentRollback" => {
            let old = [
                f.lab.paths[0].dir.join("state.toml"),
                f.lab.paths[0].dir.join("vault"),
                f.lab.paths[0].home.join(".secrets").join(broker::ENTRY),
            ]
            .map(|p| {
                let bytes = std::fs::read(&p).unwrap();
                (p, bytes)
            });
            if action == "AbsentRollback" {
                f.publish(|doc| {
                    doc.unset_entry(&Audience::Everyone, broker::ENTRY).unwrap();
                });
            } else {
                publish(&f, vec![]);
            }
            assert!(
                broker
                    .issue_until(grant.id, Duration::from_secs(120), deadline())
                    .is_err()
            );
            for (path, bytes) in old {
                std::fs::write(path, bytes).unwrap();
            }
            accepted = broker
                .issue_until(grant.id, Duration::from_secs(120), deadline())
                .is_ok();
        }
        "RevokedAfter" | "ExpiredAfter" | "PolicyAfter" => {
            let before = || match action {
                "RevokedAfter" => {
                    broker.revoke(token.bytes()).unwrap();
                }
                "ExpiredAfter" => broker.test_expire(token.bytes()),
                _ => publish(&f, vec![]),
            };
            if bypass {
                // Deliberately omit only the final capability check after durable admission.
                accepted = f
                    .source(31, 0)
                    .dispatch_pinned_until(
                        RequestKey::new(grant.producer, [63; 16]).unwrap(),
                        &grant.binding,
                        b"private request",
                        deadline(),
                        |r, c| {
                            before();
                            observe(&f, &entered, r, c)
                        },
                    )
                    .is_ok();
            } else {
                accepted = broker
                    .test_dispatch_until(
                        token.bytes(),
                        [63; 16],
                        b"private request",
                        deadline(),
                        before,
                        |r, c| observe(&f, &entered, r, c),
                    )
                    .is_ok_and(|r| r.refusal.is_none() && r.accounting.refusal.is_none());
            }
        }
        "Rotated" => {
            assert!(call(&broker, token.bytes()));
            broker.revoke(token.bytes()).unwrap();
            let next = broker
                .issue_until(grant.id, Duration::from_secs(120), deadline())
                .unwrap();
            accepted = call(&broker, next.bytes());
        }
        "Restart" => {
            assert!(call(&broker, token.bytes()));
            let restarted = self::broker(&f);
            assert!(!call(&restarted, token.bytes()));
            let next = restarted
                .issue_until(grant.id, Duration::from_secs(120), deadline())
                .unwrap();
            accepted = call(&restarted, next.bytes());
        }
        "WrongBilling" => {
            f.issue(
                31,
                0,
                f.policy("example-v1").reference([41; 32]).unwrap(),
                vec![grant.binding.revision],
            );
            accepted = call(&broker, token.bytes());
        }
        _ => panic!("unknown broker scenario"),
    }
    let mut next = before.clone();
    let charged = f
        .source(31, 0)
        .prepare_until(deadline())
        .unwrap()
        .resources()
        .snapshot()
        .unwrap()
        .used
        .values()
        .sum::<u64>();
    assert_eq!(
        charged,
        if matches!(
            action,
            "Fresh" | "RevokedAfter" | "ExpiredAfter" | "Rotated" | "Restart" | "PolicyAfter"
        ) {
            1
        } else {
            0
        }
    );
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next.insert("entered", entered.get());
    next
}

#[test]
fn broker_rotation_and_policy_updates_reclaim_slots_without_evicting_other_grants() {
    // Filling all slots checks rotation and policy retention, independently of
    // how long authenticated fixture I/O waits for a core. Expiry advances below.
    let lease_clock = Instant::now();
    let within = Duration::from_secs(120);
    let (f, mut broker, first, grant) = setup_on_with_lease_clock(1, Some(lease_clock));
    let mut grants = (0u8..64)
        .map(|n| Grant {
            id: [n + 1; 16],
            producer: [n + 100; 32],
            ..grant.clone()
        })
        .collect::<Vec<_>>();
    publish(&f, grants.clone());
    let tokens = grants
        .iter()
        .map(|g| broker.issue_until(g.id, within, deadline()).unwrap())
        .collect::<Vec<_>>();
    let replacement = broker
        .issue_until(grants[0].id, within, deadline())
        .unwrap();
    for stale in [first.bytes(), tokens[0].bytes()] {
        assert!(
            broker
                .dispatch_until(
                    "example-v1",
                    stale,
                    [71; 16],
                    &[],
                    deadline(),
                    |_, _| panic!("old token")
                )
                .is_err()
        );
    }
    assert!(broker.revoke_grant(grants[0].id).unwrap());
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                replacement.bytes(),
                [71; 16],
                &[],
                deadline(),
                |_, _| panic!("revoked grant")
            )
            .is_err()
    );
    let replacement = broker
        .issue_until(grants[0].id, within, deadline())
        .unwrap();
    let entered = Cell::new(0);
    for (token, logical) in [
        (tokens[1].bytes(), [72; 16]),
        (replacement.bytes(), [73; 16]),
    ] {
        assert!(
            broker
                .dispatch_until(
                    "example-v1",
                    token,
                    logical,
                    b"private request",
                    deadline(),
                    |r, c| { observe(&f, &entered, r, c) }
                )
                .unwrap()
                .entered
        );
    }
    assert_eq!(entered.get(), 2);
    grants[0].lifetime = Duration::from_secs(180);
    publish(&f, grants.clone());
    let current = broker
        .issue_until(grants[0].id, within, deadline())
        .unwrap();
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                tokens[1].bytes(),
                [79; 16],
                &[],
                deadline(),
                |_, _| panic!("stale policy token")
            )
            .is_err()
    );
    // Replay proves the replacement remains admitted without requiring more
    // account budget; crossing its exact lease boundary must then refuse it.
    let replay = broker
        .dispatch_until(
            "example-v1",
            current.bytes(),
            [73; 16],
            b"private request",
            deadline(),
            |_, _| panic!("replay entered twice"),
        )
        .unwrap();
    assert!(!replay.entered && replay.refusal.is_none() && replay.accounting.refusal.is_none());
    broker.test_set_lease_time(lease_clock + within);
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                current.bytes(),
                [73; 16],
                b"private request",
                deadline(),
                |_, _| panic!("expired token")
            )
            .is_err()
    );
    assert!(
        broker
            .issue_until(
                grants[0].id,
                within,
                Instant::now() - Duration::from_secs(1)
            )
            .is_err()
    );
}

#[test]
fn broker_policy_authentication_and_advanced_floor_hold_old_tokens() {
    let (f, broker, token, grant) = setup();
    let file = f.lab.paths[0].home.join(".secrets").join(broker::ENTRY);
    let text = std::fs::read(&file).unwrap();
    std::fs::write(&file, b"version = 1\n").unwrap();
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [74; 16],
                &[],
                deadline(),
                |_, _| panic!("local policy edit")
            )
            .is_err()
    );
    std::fs::write(&file, text).unwrap();
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [74; 16],
                &[],
                deadline(),
                |_, _| panic!("burned token")
            )
            .is_err()
    );
    let token = broker
        .issue_until(grant.id, Duration::from_secs(120), deadline())
        .unwrap();
    let old = ["state.toml", "vault"].map(|name| {
        let path = f.lab.paths[0].dir.join(name);
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    });
    f.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "unrelated", b"updated")
            .unwrap();
    });
    let entered = Cell::new(0);
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [75; 16],
                b"private request",
                deadline(),
                |r, c| observe(&f, &entered, r, c)
            )
            .unwrap()
            .entered
    );
    let newest = old
        .iter()
        .map(|(path, _)| (path.clone(), std::fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    for (path, bytes) in &old {
        std::fs::write(path, bytes).unwrap();
    }
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [76; 16],
                &[],
                deadline(),
                |_, _| panic!("rolled back policy")
            )
            .is_err()
    );
    for (path, bytes) in newest {
        std::fs::write(path, bytes).unwrap();
    }
    assert!(
        broker
            .dispatch_until(
                "example-v1",
                token.bytes(),
                [76; 16],
                &[],
                deadline(),
                |_, _| panic!("burned rollback token")
            )
            .is_err()
    );
    assert_eq!(entered.get(), 1);
}

#[test]
fn broker_capabilities_conform_and_final_revocation_check_is_necessary() {
    let model = aterm_models_db::broker_capability_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let initial = model.init_state();
    aterm_spec::fs_lane::each(&model.actions, |action| {
        let next = super::broker_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
        next
    });
    let unsafe_next = transition_with(&initial, "RevokedAfter", true);
    assert!(!model.check_invariant("Authorization", &unsafe_next));
}

#[test]
fn broker_rejects_bad_tokens_scope_and_lifetime_without_spending() {
    let (f, broker, token, mut grant) = setup();
    assert!(
        broker
            .dispatch_until(
                "different-v1",
                token.bytes(),
                [64; 16],
                &[],
                deadline(),
                |_, _| panic!("wrong adapter")
            )
            .is_err()
    );
    assert!(!format!("{token:?}").contains(&aterm_codec::hex::encode_lower(token.bytes())));
    for token in [&[][..], &[0; 31][..], &[0; 33][..]] {
        assert!(
            broker
                .dispatch_until(
                    "example-v1",
                    token,
                    [64; 16],
                    &[],
                    deadline(),
                    |_, _| panic!("bad token entered")
                )
                .is_err()
        );
    }
    assert!(
        broker
            .issue_until(grant.id, Duration::ZERO, deadline())
            .is_err()
    );
    assert!(
        broker
            .issue_until(
                grant.id,
                grant.lifetime + Duration::from_secs(1),
                deadline()
            )
            .is_err()
    );
    grant.actor = [90; 48];
    publish(&f, vec![grant.clone()]);
    assert!(
        broker
            .issue_until(grant.id, Duration::from_secs(1), deadline())
            .is_err()
    );
    assert_eq!(
        f.source(31, 0)
            .prepare_until(deadline())
            .unwrap()
            .resources()
            .snapshot()
            .unwrap()
            .used
            .values()
            .sum::<u64>(),
        0
    );
}
