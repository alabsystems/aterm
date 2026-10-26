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
