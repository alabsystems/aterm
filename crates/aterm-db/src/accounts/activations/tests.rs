// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{database::Change, table::tests::Lab};
use aterm_json::json;
use aterm_secrets::{
    client,
    commons::{journal::Journal, roster::Role},
    text::Audience,
};

const ACCOUNT: Digest = [71; 32];

/// The members of a configuration cost two history reads and one capture whatever their number,
/// and a head that moves during the observation refuses it. Three devices show a per-member read.
/// GAP: nothing here runs `prepared_members`, `intent_targets` or `materialize` at fleet width (the
/// 65-writer frontier test holds only `History::read` and `issue` of stops), so a defect only with
/// more than four members, such as host-label prefix confusion (`m1` against `m10`), is not seen.
#[test]
fn current_configuration_members_reads_history_twice_and_rechecks_heads() {
    let roles = vec![Role::Writer; 3];
    let (lab, db, _) = fixture_on(false, &roles);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let request = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![])
        .unwrap();
    let (root, _) = db.issue(&admin, [150; 16], request).unwrap();
    let mut children = Vec::new();
    for n in 0..3 {
        let actor = lab.bootstrap(n).request().unwrap().binding().actor();
        let request = db.intent_member_request(ACCOUNT, actor).unwrap().unwrap();
        children.push(db.issue(&admin, [n as u8 + 151; 16], request).unwrap().0);
    }
    let reads = graph::READS.with(std::cell::Cell::get);
    let captures = configuration::CAPTURES.with(std::cell::Cell::get);
    let (_, _, members) = db.current_configuration_members(ACCOUNT).unwrap();
    assert_eq!(members.len(), 3);
    assert_eq!(graph::READS.with(std::cell::Cell::get) - reads, 2);
    assert_eq!(
        configuration::CAPTURES.with(std::cell::Cell::get) - captures,
        1
    );
    assert!(
        db.current_configuration_members_after(ACCOUNT, || {
            let stop = db
                .stop_request(children[2].request().account, vec![children[2].id()])
                .unwrap();
            db.issue(&admin, [200; 16], stop).unwrap();
        })
        .is_err()
    );
    assert_eq!(
        db.current_configuration_members(ACCOUNT).unwrap().2.len(),
        2
    );
    assert!(
        db.current_configuration_members_after(ACCOUNT, || {
            let stop = db.stop_request(ACCOUNT, vec![root.id()]).unwrap();
            db.issue(&admin, [201; 16], stop).unwrap();
        })
        .is_err()
    );
}

#[test]
fn grouped_account_status_preserves_forks_stops_missing_and_foreign_parents() {
    let (lab, _, request) = fixture(false);
    let root = sign(&lab, request.clone());
    let mut child = request.clone();
    child.generation = 2;
    child.parents = vec![root.id()];
    let child = sign(&lab, child);
    let mut foreign = request.clone();
    foreign.account = [91; 32];
    let foreign = sign(&lab, foreign);
    let mut wrong = request.clone();
    wrong.generation = 2;
    wrong.parents = vec![foreign.id()];
    let wrong = sign(&lab, wrong);
    let mut fork = request.clone();
    if let Action::Prepare(terms) = &mut fork.action {
        terms.device = lab.bootstrap(1).request().unwrap().binding().clone();
    }
    let fork = sign(&lab, fork);
    let mut stop = request;
    stop.generation = 2;
    stop.parents = vec![root.id()];
    stop.action = Action::Stop;
    let stop = sign(&lab, stop);
    let mut heads = vec![root.id(), fork.id()];
    heads.sort();
    for (records, expected) in [
        (vec![root.clone()], State::Ready(root.id())),
        (vec![root.clone(), child.clone()], State::Ready(child.id())),
        (vec![child], State::Pending),
        (vec![foreign, wrong], State::Invalid),
        (vec![root.clone(), fork], State::Conflict(heads)),
        (vec![root, stop.clone()], State::Stopped(stop.id())),
    ] {
        let mut history = history(records);
        assert_eq!(history.statuses()[&ACCOUNT], expected);
        for unavailable in [0, 1] {
            history.unavailable = unavailable;
            let statuses = history.statuses();
            for record in history.records.values() {
                assert_eq!(
                    statuses[&record.request().account],
                    history.status(record.request().account)
                );
            }
        }
    }
}

#[test]
fn current_configuration_rechecks_direct_and_parent_heads_after_capture() {
    for (intent, stop) in [(false, false), (false, true), (true, false), (true, true)] {
        let (lab, db, request) = fixture(false);
        let admin = client::admin_open(&lab.paths[0]).unwrap();
        let request = if intent {
            db.intent_configuration(&admin, ACCOUNT, "one", vec![])
                .unwrap()
        } else {
            request
        };
        let (root, _) = db.issue(&admin, [86; 16], request).unwrap();
        let account = if intent {
            let local = lab.bootstrap(0).request().unwrap().binding().actor();
            let request = db.intent_member_request(ACCOUNT, local).unwrap().unwrap();
            db.issue(&admin, [85; 16], request)
                .unwrap()
                .0
                .request()
                .account
        } else {
            ACCOUNT
        };
        let before = db.current_configuration(account).unwrap().0;
        assert!(
            db.current_configuration_after(account, || {
                let replacement = if stop {
                    db.stop_request(ACCOUNT, vec![root.id()]).unwrap()
                } else if intent {
                    db.intent_configuration(&admin, ACCOUNT, "one", vec![root.id()])
                        .unwrap()
                } else {
                    db.configuration(
                        &admin,
                        ACCOUNT,
                        "one",
                        lab.bootstrap(0).request().unwrap().binding().actor(),
                        vec![root.id()],
                    )
                    .unwrap()
                };
                db.issue(&admin, [87; 16], replacement).unwrap();
            })
            .is_err()
        );
        if intent {
            assert_eq!(
                db.state(account).unwrap(),
                State::Ready(before.id()),
                "parent Stop must invalidate even an unchanged child head"
            );
        }
        if stop || intent {
            assert!(db.current_configuration(account).is_err());
        } else {
            assert_ne!(
                db.current_configuration(account).unwrap().0.id(),
                before.id()
            );
        }
    }
}

#[test]
fn current_configuration_refuses_changed_proposals_and_ambiguous_decisions() {
    let (lab, db, request) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    db.issue(&admin, [86; 16], request.clone()).unwrap();
    db.current_configuration(ACCOUNT).unwrap();
    let competing = db
        .configuration(
            &admin,
            ACCOUNT,
            "one",
            lab.bootstrap(1).request().unwrap().binding().actor(),
            vec![],
        )
        .unwrap();
    let competing = sign(&lab, competing);
    db.with_store(false, |session, store, _| {
        let mut payload = b"AAOP1".to_vec();
        payload.extend_from_slice(&[87; 16]);
        payload.extend_from_slice(competing.bytes());
        store.commit_catalog(session, &payload).map(|_| ())
    })
    .unwrap();
    assert!(db.current_configuration(ACCOUNT).is_err());

    let (lab, db, request) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    db.issue(&admin, [86; 16], request).unwrap();
    let mut changed = account(&lab, false);
    changed
        .as_object_mut()
        .unwrap()
        .insert("plan".into(), json!("changed"));
    super::super::proposals::Proposals::at(lab.paths[0].clone())
        .write(
            [88; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(changed),
            }],
        )
        .unwrap();
    assert!(db.current_configuration(ACCOUNT).is_err());
}

#[test]
fn new_intents_cannot_leave_legacy_readers_using_a_superseded_device_grant() {
    let (lab, db, legacy) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (old, _) = db.issue(&admin, [80; 16], legacy).unwrap();
    let replacement = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![old.id()])
        .unwrap();
    assert!(db.issue(&admin, [81; 16], replacement).is_err());
    assert_eq!(db.state(ACCOUNT).unwrap(), State::Ready(old.id()));
    let intent_account = [72; 32];
    let request = db
        .intent_configuration(&admin, intent_account, "one", vec![])
        .unwrap();
    let (intent, _) = db.issue(&admin, [82; 16], request).unwrap();
    let legacy_on_intent = db
        .configuration(
            &admin,
            intent_account,
            "one",
            lab.bootstrap(0).request().unwrap().binding().actor(),
            vec![intent.id()],
        )
        .unwrap();
    assert!(db.issue(&admin, [83; 16], legacy_on_intent).is_err());
    assert_eq!(db.state(intent_account).unwrap(), State::Ready(intent.id()));
}

#[test]
fn expired_shared_pass_budget_publishes_no_automatic_preparation() {
    let (lab, db, _) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let request = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![])
        .unwrap();
    db.issue(&admin, [80; 16], request).unwrap();
    let bounded = Activations::at_until(lab.paths[0].clone(), std::time::Instant::now());
    assert!(bounded.materialize_next_intent(&admin).is_err());
    assert!(db.local_preparations().unwrap().is_empty());
}

#[test]
fn fleet_intent_keeps_each_device_current_and_its_stop_revokes_every_child() {
    use aterm_secrets::commons::accounts::IntentRef;
    let (lab, db, _) = fixture(true);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let request = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![])
        .unwrap();
    let (intent, _) = db.issue(&admin, [90; 16], request).unwrap();
    assert_eq!(db.intent_targets().unwrap().len(), 2);
    let reference = IntentRef {
        account: ACCOUNT,
        decision: intent.id(),
    };
    let accounts: Vec<_> = (0..2)
        .map(|n| reference.device_account(lab.bootstrap(n).request().unwrap().binding()))
        .collect();
    assert_ne!(accounts[0], accounts[1]);
    assert!(db.materialize_next_intent(&admin).unwrap().is_some());
    assert!(db.materialize_next_intent(&admin).unwrap().is_some());
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
    assert_eq!(
        db.prepare(accounts[0], &admin.config)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );
    crate::table::tests::repair_commons(&lab, 3);
    let peer = Activations::at(lab.paths[1].clone());
    let peer_config = client::config(&lab.paths[1]).unwrap().unwrap();
    assert_eq!(
        peer.prepare(accounts[1], &peer_config)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );
    assert!(
        peer.intent_targets().unwrap().is_empty(),
        "only the named root issuer materializes children"
    );
    let stopped = db.stop_request(ACCOUNT, vec![intent.id()]).unwrap();
    db.issue(&admin, [91; 16], stopped).unwrap();
    assert!(db.prepare(accounts[0], &admin.config).is_err());
    assert!(db.local_preparations().unwrap().is_empty());
    crate::table::tests::repair_commons(&lab, 3);
    assert!(peer.prepare(accounts[1], &peer_config).is_err());
    assert!(peer.local_preparations().unwrap().is_empty());
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
}

#[test]
fn intent_recovery_authenticates_requester_reuses_its_receipt_and_refuses_changed_credentials() {
    use aterm_secrets::commons::accounts::IntentRef;
    let (lab, db, _) = fixture(true);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let request = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![])
        .unwrap();
    let (intent, _) = db.issue(&admin, [90; 16], request).unwrap();
    let root = lab.bootstrap(0).request().unwrap().binding().clone();
    let other = lab.bootstrap(1).request().unwrap().binding().clone();
    let reference = IntentRef {
        account: ACCOUNT,
        decision: intent.id(),
    };
    let account = reference.device_account(&other);
    let request = db
        .intent_member_request(ACCOUNT, other.actor())
        .unwrap()
        .unwrap();
    let (first, _) = db.issue(&admin, [91; 16], request).unwrap();
    assert!(
        db.reissue_intent_preparation(&admin, account, first.id(), &root, [92; 16])
            .is_err()
    );
    let (next, receipt) = db
        .reissue_intent_preparation(&admin, account, first.id(), &other, [92; 16])
        .unwrap();
    let (again, same_receipt) = db
        .reissue_intent_preparation(&admin, account, first.id(), &other, [92; 16])
        .unwrap();
    assert_eq!(next.id(), again.id());
    assert_eq!(receipt.id, same_receipt.id);
    assert_eq!(next.request().parents, vec![first.id()]);
    let mut changed = admin.doc.clone();
    changed
        .set_entry(&Audience::Everyone, "token", b"different-credential")
        .unwrap();
    client::admin_publish(&lab.paths[0], &admin, &changed).unwrap();
    let refreshed = client::admin_open(&lab.paths[0]).unwrap();
    assert!(
        db.reissue_intent_preparation(&refreshed, account, next.id(), &other, [93; 16])
            .is_err()
    );
}

#[test]
fn an_existing_intent_discovers_a_later_admitted_device_without_new_approval() {
    let lab = Lab::with_enrolled(&[Role::Writer, Role::Writer, Role::Writer], 2);
    super::super::proposals::Proposals::at(lab.paths[0].clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(account(&lab, false)),
            }],
        )
        .unwrap();
    let db = Activations::at(lab.paths[0].clone());
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let request = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![])
        .unwrap();
    db.issue(&admin, [90; 16], request).unwrap();
    assert_eq!(db.intent_targets().unwrap().len(), 2);
    assert!(db.materialize_next_intent(&admin).unwrap().is_some());
    assert!(db.materialize_next_intent(&admin).unwrap().is_some());
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
    lab.admit(&[Role::Writer, Role::Writer, Role::Writer]);
    assert_eq!(db.intent_targets().unwrap().len(), 3);
    assert!(db.materialize_next_intent(&admin).unwrap().is_some());
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
}

/// The gap of `current_configuration_members_reads_history_twice_and_rechecks_heads` applies: three
/// enrolled members and one admitted, not a fleet.
#[test]
fn recovered_issuer_prepares_a_newly_admitted_member_without_relaxing_direct_issue() {
    let roles = [Role::Writer; 4];
    let mut lab = Lab::with_enrolled(&roles, 3);
    super::super::proposals::Proposals::at(lab.paths[0].clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(account(&lab, false)),
            }],
        )
        .unwrap();
    let original = lab.paths[0].clone();
    let db = Activations::at(original.clone());
    let admin = client::admin_open(&original).unwrap();
    let request = db
        .intent_configuration(&admin, ACCOUNT, "one", vec![])
        .unwrap();
    let (intent, _) = db.issue(&admin, [90; 16], request).unwrap();
    for n in 0..3 {
        let actor = lab.bootstrap(n).request().unwrap().binding().actor();
        let child = db.intent_member_request(ACCOUNT, actor).unwrap().unwrap();
        db.issue(&admin, [100 + n as u8; 16], child).unwrap();
    }
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
    let config = lab._home.path().join("aterm.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let selected = crate::generation::test_complete_successor(
        lab._home.path(),
        &original,
        &lab.paths[1],
        &config,
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    lab.paths[1] = selected;
    assert!(
        db.intent_targets().unwrap().is_empty(),
        "ordinary discovery stays issuer-strict"
    );
    lab.admit(&roles);
    let new_member = lab.bootstrap(3).request().unwrap().binding().clone();
    let request = db
        .intent_member_request_in(ACCOUNT, new_member.actor(), true)
        .unwrap()
        .unwrap();
    assert!(
        db.issue(&admin, [91; 16], request).is_err(),
        "raw issue cannot claim succession"
    );
    let reads = graph::READS.with(std::cell::Cell::get);
    let (child, _) = db.materialize_next_intent(&admin).unwrap().unwrap();
    assert_eq!(
        graph::READS.with(std::cell::Cell::get) - reads,
        3,
        "prepared members never trigger another per-member history read"
    );
    let Action::Prepare(terms) = &child.request().action else {
        panic!("expected new child")
    };
    assert_eq!(terms.device, new_member);
    assert_eq!(terms.intent.unwrap().decision, intent.id());
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
    // Discovery's observation selects the generation its writer session does, so it drops no
    // preparation that writer permits, through the predecessor's home and the successor's.
    for paths in [&original, &lab.paths[1]] {
        let discovery = Activations::at(paths.clone());
        let found = discovery.local_preparations().unwrap();
        assert!(!found.is_empty(), "PRECONDITION: a preparation here");
        assert_eq!(found, permitted(&discovery));
    }
    // Even a previously minted witness cannot outlive current revocation.
    let successor = Bootstrap::required(&lab.paths[1]).unwrap();
    let predecessor = Bootstrap::required(&original).unwrap();
    let journal = Journal::read_existing(successor.journal_dir(), successor.trust()).unwrap();
    let witness = admin
        .account_issuer_succession(predecessor, &successor, &journal, &intent)
        .unwrap();
    admin
        .prepare_account_decision_after_succession(
            &successor,
            &journal,
            child.request().clone(),
            &intent,
            &witness,
        )
        .unwrap();
    drop(journal);
    let mut revoked = roles;
    revoked[0] = Role::Reader;
    lab.admit(&revoked);
    let journal = Journal::read_existing(successor.journal_dir(), successor.trust()).unwrap();
    assert!(
        admin
            .prepare_account_decision_after_succession(
                &successor,
                &journal,
                child.request().clone(),
                &intent,
                &witness,
            )
            .is_err()
    );
    drop(journal);
    assert!(db.materialize_next_intent(&admin).is_err());
    lab.admit(&roles);
    // A current Stop still invalidates all children even with valid lineage.
    let stop = db.stop_request(ACCOUNT, vec![intent.id()]).unwrap();
    db.issue(&admin, [92; 16], stop).unwrap();
    assert!(db.materialize_next_intent(&admin).unwrap().is_none());
    assert!(db.prepare(child.request().account, &admin.config).is_err());
    // Private metadata is a hint: forging its actor cannot replace custody.
    let selection = original.dir.join("commons/writer-generation/selection");
    let mut value: aterm_json::Value =
        aterm_json::from_slice(&std::fs::read(&selection).unwrap()).unwrap();
    value.as_object_mut().unwrap().insert(
        "actor".into(),
        json!(aterm_codec::hex::encode_lower(&new_member.actor())),
    );
    aterm_secrets::fsx::write_atomic(&selection, &aterm_json::to_vec(&value).unwrap(), 0o600)
        .unwrap();
    assert!(db.materialize_next_intent(&admin).is_err());
}

#[test]
fn local_discovery_requires_current_root_head_and_exact_device() {
    let (lab, db, request) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (local, _) = db.issue(&admin, [11; 16], request).unwrap();
    let foreign = db
        .configuration(
            &admin,
            [72; 32],
            "one",
            lab.bootstrap(1).request().unwrap().binding().actor(),
            vec![],
        )
        .unwrap();
    db.issue(&admin, [12; 16], foreign).unwrap();
    assert_eq!(
        db.local_preparations().unwrap(),
        vec![(ACCOUNT, local.id())]
    );
    let stop = db.stop_request(ACCOUNT, vec![local.id()]).unwrap();
    db.issue(&admin, [13; 16], stop).unwrap();
    assert!(db.local_preparations().unwrap().is_empty());
}

/// DISCOVERING NOTHING PREPARED HERE IS NO FAILURE AND TAKES NO WRITER: `local_preparations` is
/// empty in a Commons that declared only another table, and in an activations table that holds no
/// current decision for this Mac (none at all, then only another Mac's), and each time it reads
/// while the test holds this Mac's control journal writer, the writer converge's own writers fail
/// fast against. CONTROL: the other Mac finds the decision that names it, a current decision for
/// this Mac is found, and only through its permit: while the test holds the writer that permit
/// needs (a wait the admission's one-minute hang detector ends), it finds nothing permitted;
/// demoted to a Reader, the same Mac finds nothing, and readmitted as a Writer, finds it again.
#[test]
fn local_discovery_of_nothing_prepared_is_empty_and_takes_no_writer() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    super::super::proposals::Proposals::at(lab.paths[0].clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(account(&lab, false)),
            }],
        )
        .unwrap();
    let db = Activations::at(lab.paths[0].clone());
    assert!(db.inspect(ACCOUNT).is_err(), "PRECONDITION: undeclared");
    let held = || {
        let bootstrap = lab.bootstrap(0);
        Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap()
    };
    let discover = |hold: bool| {
        let _writer = hold.then(held);
        db.local_preparations().map_err(|why| why.0)
    };
    assert_eq!(discover(false), Ok(vec![]), "undeclared");
    assert_eq!(discover(true), Ok(vec![]), "undeclared, writer held");
    db.initialize().unwrap();
    assert_eq!(discover(true), Ok(vec![]), "no decision, writer held");
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let actor = |n: usize| lab.bootstrap(n).request().unwrap().binding().actor();
    let foreign = db
        .configuration(&admin, [72; 32], "one", actor(1), vec![])
        .unwrap();
    let (foreign, _) = db.issue(&admin, [12; 16], foreign).unwrap();
    assert_eq!(discover(true), Ok(vec![]), "another Mac's, writer held");
    crate::table::tests::repair_commons(&lab, 3);
    assert_eq!(
        Activations::at(lab.paths[1].clone())
            .local_preparations()
            .map_err(|why| why.0),
        Ok(vec![([72; 32], foreign.id())]),
        "CONTROL: the Mac it names"
    );
    let local = db
        .configuration(&admin, ACCOUNT, "one", actor(0), vec![])
        .unwrap();
    let (local, _) = db.issue(&admin, [11; 16], local).unwrap();
    assert_eq!(discover(false), Ok(vec![(ACCOUNT, local.id())]), "CONTROL");
    assert!(
        !matches!(discover(true), Ok(found) if !found.is_empty()),
        "CONTROL: a busy writer permits nothing"
    );
    lab.admit(&[Role::Reader, Role::Writer]);
    assert_eq!(discover(false), Ok(vec![]), "CONTROL: no permit");
    lab.admit(&[Role::Writer, Role::Writer]);
    assert_eq!(
        discover(false),
        Ok(vec![(ACCOUNT, local.id())]),
        "CONTROL: readmitted"
    );
}

#[test]
fn prepared_configuration_owns_only_its_checked_closure_after_live_records_change() {
    let (lab, db, _) = fixture(false);
    let proposals = super::super::proposals::Proposals::at(lab.paths[0].clone());
    let mut value = account(&lab, false);
    value
        .as_object_mut()
        .unwrap()
        .insert("identities".into(), json!([{"kind":"email","id":"alias"}]));
    proposals
        .write(
            [2; 16],
            vec![
                Change {
                    collection: "accounts".into(),
                    key: "one".into(),
                    value: Some(value),
                },
                Change {
                    collection: "email-addresses".into(),
                    key: "alias".into(),
                    value: Some(json!({"address":"alias@example.test","alias_of":"primary"})),
                },
                Change {
                    collection: "email-addresses".into(),
                    key: "primary".into(),
                    value: Some(json!({"address":"primary@example.test"})),
                },
                Change {
                    collection: "email-addresses".into(),
                    key: "unrelated".into(),
                    value: Some(json!({"address":"unrelated@example.test"})),
                },
            ],
        )
        .unwrap();
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let request = db
        .configuration(
            &admin,
            ACCOUNT,
            "one",
            lab.bootstrap(0).request().unwrap().binding().actor(),
            vec![],
        )
        .unwrap();
    db.issue(&admin, [3; 16], request).unwrap();
    let prepared = db.prepare(ACCOUNT, &admin.config).unwrap();
    let configuration = prepared.configuration();
    assert_eq!(
        configuration.account()["provider"].as_str(),
        Some("anthropic")
    );
    assert_eq!(configuration.key(), "one");
    assert_eq!(
        configuration.get("email-addresses", "primary").unwrap()["address"].as_str(),
        Some("primary@example.test")
    );
    assert!(configuration.get("email-addresses", "unrelated").is_none());
    let Action::Prepare(terms) = &prepared.decision().request().action else {
        unreachable!()
    };
    assert_eq!(configuration.scope().digest(), terms.table);
    assert_eq!(configuration.digest(), terms.configuration);
    proposals
        .write(
            [4; 16],
            vec![Change {
                collection: "email-addresses".into(),
                key: "primary".into(),
                value: Some(json!({"address":"replacement@example.test"})),
            }],
        )
        .unwrap();
    assert!(db.prepare(ACCOUNT, &admin.config).is_err());
    assert_eq!(
        configuration.get("email-addresses", "primary").unwrap()["address"].as_str(),
        Some("primary@example.test")
    );
}
fn account(lab: &Lab, secret: bool) -> aterm_json::Value {
    let mut value = json!({"provider":"anthropic","method":"oauth","label":"m1.1"});
    if secret {
        value.as_object_mut().unwrap().insert(
            "secret_ref".into(),
            json!({"vault":client::config(&lab.paths[0]).unwrap().unwrap().name,"entry":"token"}),
        );
    }
    value
}
fn fixture(secret: bool) -> (Lab, Activations, Request) {
    fixture_on(secret, &[Role::Writer, Role::Writer])
}
fn fixture_on(secret: bool, roles: &[Role]) -> (Lab, Activations, Request) {
    let lab = Lab::new(roles);
    if secret {
        let admin = client::admin_open(&lab.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-account-token")
            .unwrap();
        client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
        if roles.len() > 1 {
            client::sync(&lab.paths[1], None).unwrap();
        }
    }
    super::super::proposals::Proposals::at(lab.paths[0].clone())
        .write(
            [1; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(account(&lab, secret)),
            }],
        )
        .unwrap();
    let db = Activations::at(lab.paths[0].clone());
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let actor = lab.bootstrap(0).request().unwrap().binding().actor();
    let request = db
        .configuration(&admin, ACCOUNT, "one", actor, vec![])
        .unwrap();
    (lab, db, request)
}

#[test]
fn one_offline_machine_can_issue_prepare_and_stop_without_a_voter() {
    let (lab, db, request) = fixture_on(true, &[Role::Writer]);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (grant, _) = db.issue(&admin, [2; 16], request).unwrap();
    assert_eq!(
        db.prepare(ACCOUNT, &admin.config)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );
    let stop = db.stop_request(ACCOUNT, vec![grant.id()]).unwrap();
    db.issue(&admin, [3; 16], stop).unwrap();
    assert!(db.prepare(ACCOUNT, &admin.config).is_err());
}

#[test]
fn foreign_accounts_and_malformed_relays_cannot_shadow_authority() {
    let (lab, db, request) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (grant, _) = db.issue(&admin, [2; 16], request.clone()).unwrap();
    let mut other = request.clone();
    other.account = [72; 32];
    let other = sign(&lab, other);
    db.with_store(false, |session, store, _| {
        for (prefix, wire) in [
            (b"AAOP1".as_slice(), other.bytes()),
            (b"BOGUS".as_slice(), grant.bytes()),
        ] {
            let mut payload = prefix.to_vec();
            payload.extend_from_slice(&[3; 16]);
            payload.extend_from_slice(wire);
            store.commit_catalog(session, &payload)?;
        }
        let mut forged = b"AAOP1".to_vec();
        forged.extend_from_slice(&[4; 16]);
        let mut body = grant.bytes().to_vec();
        *body.last_mut().unwrap() ^= 1;
        forged.extend(body);
        store.commit_catalog(session, &forged)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(db.state(ACCOUNT).unwrap(), State::Ready(grant.id()));
    assert_eq!(db.state([72; 32]).unwrap(), State::Ready(other.id()));
    assert!(db.prepare(ACCOUNT, &admin.config).is_ok());
    let report = db.inspect(ACCOUNT).unwrap();
    assert_eq!(report["transport"].as_array().unwrap().len(), 1);
    assert_eq!(report["ignored"].as_u64(), Some(2));
    let mut invalid = request;
    invalid.generation = 2;
    invalid.parents = vec![other.id()];
    let invalid = sign(&lab, invalid);
    let h = history(vec![grant, other, invalid]);
    assert_eq!(h.status(ACCOUNT), State::Invalid);
    assert!(matches!(h.status([72; 32]), State::Ready(_)));
}

#[test]
fn more_than_the_parent_bound_can_be_recovered_by_explicit_merge_cascades() {
    let (lab, _, request) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let bootstrap = lab.bootstrap(0);
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let sign = |request| {
        admin
            .prepare_account_decision(&bootstrap, &journal, request)
            .unwrap()
    };
    let root = sign(request.clone());
    let mut h = history(vec![root.clone()]);
    let mut heads = vec![];
    for n in 1..=65 {
        let mut branch = request.clone();
        branch.generation = 2;
        branch.parents = vec![root.id()];
        if let Action::Prepare(p) = &mut branch.action {
            p.proposal = [n; 32];
        }
        let branch = sign(branch);
        heads.push(branch.id());
        h.records.insert(branch.id(), branch);
    }
    heads.sort();
    assert!(matches!(h.status(ACCOUNT),State::Conflict(ref v) if v.len()==65));
    let merge = Request {
        account: ACCOUNT,
        generation: 3,
        parents: heads[..64].to_vec(),
        action: Action::Stop,
    };
    h.check_next(&merge).unwrap();
    let merge = sign(merge);
    h.records.insert(merge.id(), merge.clone());
    assert!(matches!(h.status(ACCOUNT),State::Conflict(ref v) if v.len()==2));
    let mut parents = vec![heads[64], merge.id()];
    parents.sort();
    let mut final_merge = request.clone();
    final_merge.generation = 4;
    final_merge.parents = parents;
    h.check_next(&final_merge).unwrap();
    let final_merge = sign(final_merge);
    h.records.insert(final_merge.id(), final_merge.clone());
    assert_eq!(h.status(ACCOUNT), State::Ready(final_merge.id()));
    let mut exhausted = request;
    exhausted.generation = u64::MAX;
    exhausted.parents = vec![root.id()];
    let exhausted = sign(exhausted);
    h.records.insert(exhausted.id(), exhausted.clone());
    assert!(next_generation(&h, &[exhausted.id()]).is_err());
}
fn sign(lab: &Lab, request: Request) -> Record {
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let bootstrap = lab.bootstrap(0);
    let journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    admin
        .prepare_account_decision(&bootstrap, &journal, request)
        .unwrap()
}
/// What the admitted writer session alone permits: `local_preparations` without its observation.
fn permitted(db: &Activations) -> Vec<(Digest, Digest)> {
    db.with_store(false, |session, store, scope| {
        let history = History::read(session, store, scope.trust())?;
        Ok(history
            .statuses()
            .into_iter()
            .filter_map(|(account, state)| {
                let State::Ready(decision) = state else {
                    return None;
                };
                intents::current(&history, &history.records[&decision]).ok()?;
                session
                    .permit_account(&history.records[&decision])
                    .ok()
                    .map(|()| (account, decision))
            })
            .collect())
    })
    .unwrap()
}
fn history(records: Vec<Record>) -> History {
    History {
        records: records.into_iter().map(|r| (r.id(), r)).collect(),
        requests: BTreeMap::new(),
        unavailable: 0,
        ignored: 0,
    }
}

#[test]
fn root_preparation_returns_only_its_bound_bytes_and_retries_preserve_ciphertext() {
    let (lab, db, request) = fixture(true);
    let mut admin = client::admin_open(&lab.paths[0]).unwrap();
    admin
        .doc
        .set_entry(&Audience::Everyone, "token", b"unpublished-draft")
        .unwrap();
    let (first, receipt) = db.issue(&admin, [2; 16], request.clone()).unwrap();
    let (retry, same) = db.issue(&admin, [2; 16], request.clone()).unwrap();
    assert_eq!(first.bytes(), retry.bytes());
    assert_eq!(receipt, same);
    assert_eq!(db.state(ACCOUNT).unwrap(), State::Ready(first.id()));
    let expected = client::config(&lab.paths[0]).unwrap().unwrap();
    assert_eq!(
        db.prepare(ACCOUNT, &expected)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );
    let mut changed = request.clone();
    if let Action::Prepare(p) = &mut changed.action {
        p.provider = "other".into();
    }
    assert!(db.issue(&admin, [2; 16], changed).is_err());
    let fresh = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = fresh.doc.clone();
    doc.set_entry(&Audience::Everyone, "unrelated", b"other-value")
        .unwrap();
    client::admin_publish(&lab.paths[0], &fresh, &doc).unwrap();
    assert!(db.prepare(ACCOUNT, &expected).is_ok());
    let fresh = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = fresh.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"rotated-token")
        .unwrap();
    client::admin_publish(&lab.paths[0], &fresh, &doc).unwrap();
    assert!(db.prepare(ACCOUNT, &expected).is_err());
    let fresh = client::admin_open(&lab.paths[0]).unwrap();
    assert_eq!(
        db.issue(&fresh, [2; 16], request).unwrap().0.bytes(),
        first.bytes()
    );
}

#[test]
fn stop_survives_reopen_and_old_grant_replay_then_explicit_reactivation_works() {
    let (lab, db, first) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let (old, _) = db.issue(&admin, [2; 16], first.clone()).unwrap();
    let stop = db.stop_request(ACCOUNT, vec![old.id()]).unwrap();
    let (stopped, _) = db.issue(&admin, [3; 16], stop).unwrap();
    let reopened = Activations::at(lab.paths[0].clone());
    assert_eq!(
        reopened.state(ACCOUNT).unwrap(),
        State::Stopped(stopped.id())
    );
    assert!(reopened.prepare(ACCOUNT, &admin.config).is_err());
    assert_eq!(
        reopened.issue(&admin, [2; 16], first).unwrap().0.id(),
        old.id()
    );
    assert_eq!(
        reopened.state(ACCOUNT).unwrap(),
        State::Stopped(stopped.id())
    );
    let next = reopened
        .configuration(
            &admin,
            ACCOUNT,
            "one",
            lab.bootstrap(0).request().unwrap().binding().actor(),
            vec![stopped.id()],
        )
        .unwrap();
    let (active, _) = reopened.issue(&admin, [4; 16], next).unwrap();
    assert_eq!(
        reopened
            .prepare(ACCOUNT, &admin.config)
            .unwrap()
            .decision()
            .id(),
        active.id()
    );
}

#[test]
fn complete_root_dag_requires_all_forks_and_missing_ancestry_before_use() {
    let (lab, _, request) = fixture(false);
    let first = sign(&lab, request.clone());
    let mut stop = request.clone();
    stop.parents = vec![first.id()];
    stop.generation = 2;
    stop.action = Action::Stop;
    let stopped = sign(&lab, stop.clone());
    let mut branch = request.clone();
    branch.parents = vec![first.id()];
    branch.generation = 2;
    let fork = sign(&lab, branch.clone());
    for records in [
        vec![first.clone(), stopped.clone(), fork.clone()],
        vec![fork.clone(), stopped.clone(), first.clone()],
    ] {
        let h = history(records);
        assert!(matches!(h.status(ACCOUNT), State::Conflict(_)));
    }
    assert_eq!(
        history(vec![stopped.clone()]).status(ACCOUNT),
        State::Pending
    );
    let mut wrong = branch.clone();
    wrong.generation = 3;
    let wrong = sign(&lab, wrong);
    assert_eq!(
        history(vec![first.clone(), wrong]).status(ACCOUNT),
        State::Invalid
    );
    let mut merged = branch;
    merged.parents = vec![stopped.id(), fork.id()];
    merged.parents.sort();
    merged.generation = 3;
    let merged = sign(&lab, merged);
    let h = history(vec![first, stopped, fork, merged.clone()]);
    assert_eq!(h.status(ACCOUNT), State::Ready(merged.id()));
    let mut unavailable = h;
    unavailable.unavailable = 1;
    assert_eq!(unavailable.status(ACCOUNT), State::Pending);
}

#[test]
fn proposal_label_changes_are_inert_but_changed_authentication_and_retirement_refuse() {
    let (lab, db, request) = fixture(false);
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    db.issue(&admin, [2; 16], request).unwrap();
    let proposals = super::super::proposals::Proposals::at(lab.paths[0].clone());
    let mut value = account(&lab, false);
    value
        .as_object_mut()
        .unwrap()
        .insert("label".into(), json!("m100.1"));
    proposals
        .write(
            [3; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(value.clone()),
            }],
        )
        .unwrap();
    assert!(db.prepare(ACCOUNT, &admin.config).is_ok());
    value
        .as_object_mut()
        .unwrap()
        .insert("email".into(), json!("changed@example.test"));
    proposals
        .write(
            [4; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(value),
            }],
        )
        .unwrap();
    assert!(db.prepare(ACCOUNT, &admin.config).is_err());
    proposals
        .write(
            [5; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(account(&lab, false)),
            }],
        )
        .unwrap();
    assert!(db.prepare(ACCOUNT, &admin.config).is_ok());
    // Removing write privileges must withhold a previously signed preparation.
    lab.rotate(&[Role::Reader, Role::Writer]);
    assert!(db.prepare(ACCOUNT, &admin.config).is_err());
}

#[test]
fn two_replicas_repair_a_delayed_stop_even_after_its_relay_loses_write_privileges() {
    let (lab, a, _) = fixture(true);
    let b = Activations::at(lab.paths[1].clone());
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let actor = lab.bootstrap(1).request().unwrap().binding().actor();
    let wanted = a
        .configuration(&admin, ACCOUNT, "one", actor, vec![])
        .unwrap();
    let (first, _) = a.issue(&admin, [2; 16], wanted).unwrap();
    assert!(b.prepare(ACCOUNT, &admin.config).is_err());
    crate::table::tests::repair_commons(&lab, 3);
    let bconfig = client::config(&lab.paths[1]).unwrap().unwrap();
    assert_eq!(
        b.prepare(ACCOUNT, &bconfig)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );
    assert!(a.prepare(ACCOUNT, &admin.config).is_err());
    let stop = a.stop_request(ACCOUNT, vec![first.id()]).unwrap();
    let (stopped, _) = a.issue(&admin, [3; 16], stop).unwrap();
    lab.rotate(&[Role::Reader, Role::Writer]);
    crate::table::tests::repair_commons(&lab, 3);
    assert_eq!(b.state(ACCOUNT).unwrap(), State::Stopped(stopped.id()));
    assert!(b.prepare(ACCOUNT, &bconfig).is_err());
    b.with_store(false, |session, store, _| {
        let mut duplicate = b"AAOP1".to_vec();
        duplicate.extend_from_slice(&[4; 16]);
        duplicate.extend_from_slice(first.bytes());
        store.commit_catalog(session, &duplicate).map(|_| ())
    })
    .unwrap();
    assert_eq!(b.state(ACCOUNT).unwrap(), State::Stopped(stopped.id()));
}

#[test]
fn activation_commands_are_explicit_and_never_print_credential_material() {
    use aterm_codec::hex::encode_lower as hex;
    let (lab, db, _) = fixture(true);
    let account = hex(&ACCOUNT);
    let actor = hex(&lab.bootstrap(0).request().unwrap().binding().actor());
    let nonce = hex(&[2; 16]);
    let options = BTreeMap::from([
        ("--account", account.as_str()),
        ("--actor", actor.as_str()),
        ("--key", "one"),
        ("--parents", "none"),
        ("--request", nonce.as_str()),
    ]);
    let first =
        command::run_at("accounts-authorize", options.clone(), lab.paths[0].clone()).unwrap();
    assert_eq!(
        command::run_at("accounts-authorize", options, lab.paths[0].clone()).unwrap(),
        first
    );
    let prepared = command::run_at(
        "accounts-prepare",
        BTreeMap::from([("--account", account.as_str())]),
        lab.paths[0].clone(),
    )
    .unwrap();
    assert_eq!(prepared["credential_bound"].as_bool(), Some(true));
    assert_eq!(prepared["provider_login"].as_str(), Some("not-attempted"));
    let visible = aterm_json::to_string(&db.inspect(ACCOUNT).unwrap()).unwrap();
    assert!(!visible.contains("inert-account-token"));
    let hash = aterm_digest::Sha256::digest(b"inert-account-token");
    assert!(!visible.contains(&hex(&hash)));
    assert!(
        command::run_at(
            "accounts-prepare",
            BTreeMap::from([("--account", account.as_str()), ("--force", "true")]),
            lab.paths[0].clone()
        )
        .is_err()
    );
}

fn model_records() -> &'static [Record; 5] {
    static RECORDS: std::sync::OnceLock<[Record; 5]> = std::sync::OnceLock::new();
    RECORDS.get_or_init(|| {
        let (lab, _, request) = fixture(false);
        let root = sign(&lab, request.clone());
        let mut next = request.clone();
        next.parents = vec![root.id()];
        next.generation = 2;
        next.action = Action::Stop;
        let stop = sign(&lab, next);
        let mut next = request.clone();
        next.parents = vec![stop.id()];
        next.generation = 3;
        let resume = sign(&lab, next);
        let mut next = request.clone();
        next.parents = vec![root.id()];
        next.generation = 2;
        if let Action::Prepare(p) = &mut next.action {
            p.proposal = [88; 32];
        }
        let fork = sign(&lab, next);
        let mut next = request;
        next.parents = vec![resume.id(), fork.id()];
        next.parents.sort();
        next.generation = 4;
        let merged = sign(&lab, next);
        [root, stop, resume, fork, merged]
    })
}

pub(crate) fn activation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let variable = match action {
        "Root" => "root",
        "Stop" => "stop",
        "Resume" => "resume",
        "Fork" => "fork",
        "Merge" => "merged",
        "Replay" => "replayed",
        _ => panic!("unknown activation action"),
    };
    next.insert(variable, 1);
    if action == "Stop" {
        next.insert("seen_stop", 1);
    }
    let records = model_records();
    let mut h = history(
        ["root", "stop", "resume", "fork", "merged"]
            .into_iter()
            .enumerate()
            .filter(|(_, name)| next[name] == 1)
            .map(|(n, _)| records[n].clone())
            .collect(),
    );
    if action == "Replay" {
        h.records.insert(records[0].id(), records[0].clone());
    }
    next.insert(
        "active",
        i64::from(matches!(h.status(ACCOUNT), State::Ready(_))),
    );
    next
}

#[test]
fn every_bounded_delivery_order_conforms_and_replay_or_fork_bypasses_are_caught() {
    let model = aterm_models_db::account_activation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 32);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = activation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let root = activation_transition(&model.init_state(), "Root");
    let stopped = activation_transition(&root, "Stop");
    let mut bad = activation_transition(&stopped, "Replay");
    bad.insert("stop", 0);
    assert!(!model.check_invariant("RetainStop", &bad));
    assert!(!model.successors("Replay", &stopped).contains(&bad));
    let mut bad = activation_transition(&stopped, "Fork");
    bad.insert("active", 1);
    assert!(!model.check_invariant("ForkWithholds", &bad));
    assert!(!model.successors("Fork", &stopped).contains(&bad));
}

#[test]
fn log_backed_account_preparation_survives_log_appends_and_epoch_rotations_until_revoked() {
    let (lab, a, _) = fixture_on(true, &[Role::Writer, Role::Writer, Role::Writer]);
    client::sync(&lab.paths[2], None).unwrap();
    let genesis_out = lab.paths[0].home.join("f18.genesis");
    client::test_sign_genesis(&lab.paths[0], &genesis_out, &lab.signer).unwrap();
    let initial_wire = client::load_records(&lab.paths[0]).unwrap().encode();
    for p in [&lab.paths[1], &lab.paths[2]] {
        client::save_records(p, &initial_wire).unwrap();
    }

    let b = Activations::at(lab.paths[1].clone());
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let actor_1 = lab.bootstrap(1).request().unwrap().binding().actor();
    let actor_2 = lab.bootstrap(2).request().unwrap().binding().actor();
    let wanted = a
        .configuration(&admin, ACCOUNT, "one", actor_1, vec![])
        .unwrap();
    let (_first, _) = a.issue(&admin, [2; 16], wanted).unwrap();
    crate::table::fixture::repair_commons_complete(&lab, 3);

    let bconfig = client::config(&lab.paths[1]).unwrap().unwrap();
    assert_eq!(
        b.prepare(ACCOUNT, &bconfig)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );

    // 1. Appending an unrelated record to the membership log changes `Access::head()` (`ALCO2`
    //    digest) without changing `Access::basis()`. `b.prepare` must still succeed.
    let fresh = client::admin_open(&lab.paths[0]).unwrap();
    let mut doc = fresh.doc.clone();
    doc.set_entry(&Audience::Everyone, "unrelated", b"log-append")
        .unwrap();
    client::admin_publish(&lab.paths[0], &fresh, &doc).unwrap();
    client::sync(&lab.paths[1], None).unwrap();
    assert_eq!(
        b.prepare(ACCOUNT, &bconfig)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );

    // 2. Removing a third member (`actor_2`) in the membership log rotates `EpochAudience::Commons`
    //    to a new epoch (`Access::basis()` changes). `b.prepare` must still succeed on `m2` via
    //    `Session::historical_writer_in`.
    client::remove_member_in_log(&lab.paths[0], actor_2).unwrap();
    let wire_after_rem2 = client::load_records(&lab.paths[0]).unwrap().encode();
    client::save_records(&lab.paths[1], &wire_after_rem2).unwrap();
    assert_eq!(
        b.prepare(ACCOUNT, &bconfig)
            .unwrap()
            .credential()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-account-token"
    );

    // 3. Once `m2` itself (`actor_1`) is removed in the membership log, `b.prepare` is refused.
    client::remove_member_in_log(&lab.paths[0], actor_1).unwrap();
    let wire_after_rem1 = client::load_records(&lab.paths[0]).unwrap().encode();
    client::save_records(&lab.paths[1], &wire_after_rem1).unwrap();
    assert!(b.prepare(ACCOUNT, &bconfig).is_err());
}
