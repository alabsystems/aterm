// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use crate::table::store::Store;
use std::io::Write;

#[test]
fn offline_one_and_two_replica_histories_converge_after_restart() {
    for count in [1, 2] {
        let lab = Lab::new(&vec![Role::Writer; count]);
        let root = aterm_tempfile::tempdir().unwrap();
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, &root.path().join("a")).unwrap();
        let scope = store.scope();
        let first = store
            .commit_catalog(&session, b"private account details")
            .unwrap();
        let mut wires = vec![store.wire(&session, &first.id).unwrap().unwrap().to_vec()];
        if count == 2 {
            let b = lab.bootstrap(1);
            let other = Session::open(&b).unwrap();
            let mut peer = Store::join(&other, &root.path().join("b"), scope).unwrap();
            let second = peer.commit_catalog(&other, b"offline peer edit").unwrap();
            let second_wire = peer.wire(&other, &second.id).unwrap().unwrap().to_vec();
            peer.ingest(&other, &wires[0]).unwrap();
            store.ingest(&session, &second_wire).unwrap();
            assert_eq!(ids(&peer, &other), ids(&store, &session));
            wires.push(second_wire);
            drop(peer);
            let peer = Store::open(&other, &root.path().join("b"), scope).unwrap();
            assert_eq!(ids(&peer, &other), ids(&store, &session));
        }
        assert_eq!(store.ingest(&session, &wires[0]).unwrap(), first);
        drop(store);
        let mut store = Store::open(&session, &root.path().join("a"), scope).unwrap();
        assert_eq!(store.receipt(&session, &first.id).unwrap(), Some(first));
        let next = store.commit_catalog(&session, b"after restart").unwrap();
        assert_eq!(
            session
                .authenticate(scope, store.wire(&session, &next.id).unwrap().unwrap())
                .unwrap()
                .header()
                .sequence,
            2
        );
        let disk = std::fs::read(root.path().join("a/table.astream")).unwrap();
        assert!(!disk.windows(23).any(|b| b == b"private account details"));
    }
}
fn ids(store: &Store, session: &Session<'_>) -> Vec<Digest> {
    store
        .catalog(session)
        .unwrap()
        .iter()
        .map(|r| r.header().id)
        .collect()
}

#[test]
fn future_epoch_republication_does_not_strand_witnessed_tables_after_fresh_rotation() {
    use aterm_secrets::commons::node::Edit;

    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let first = Session::open(&a).unwrap();
    let second = Session::open(&b).unwrap();
    let mut store = Store::create(&first, &root.path().join("a")).unwrap();
    let scope = store.scope();
    let receipt = store
        .commit_catalog(&first, b"old account evidence")
        .unwrap();
    let wire = store.wire(&first, &receipt.id).unwrap().unwrap().to_vec();
    let mut peer = Store::join(&second, &root.path().join("b"), scope).unwrap();
    let peer_receipt = peer.ingest(&second, &wire).unwrap();
    drop((store, peer, first, second));

    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let identity = client::machine_identity(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
    let source = journal.node(&a.trust().genesis).unwrap();
    let payload = source.open(&identity).unwrap();
    let copy = admin
        .seal_commons_restricted(
            NETWORK,
            Edit::Update {
                parent: source.id(),
                hosts: &["m1".into(), "m2".into()],
                payload: payload.as_slice(),
            },
            source.facts(),
        )
        .unwrap();
    journal.ingest(copy.bytes()).unwrap();
    drop(journal);
    Journal::open_existing(b.journal_dir(), b.trust())
        .unwrap()
        .ingest(copy.bytes())
        .unwrap();
    assert!(
        Session::open(&a).is_err(),
        "repeated current keys still refuse"
    );
    lab.rotate(&[Role::Writer, Role::Writer]);

    let first = Session::open(&a).unwrap();
    let second = Session::open(&b).unwrap();
    let mut store = Store::open(&first, &root.path().join("a"), scope).unwrap();
    let peer = Store::open(&second, &root.path().join("b"), scope).unwrap();
    assert_eq!(store.receipt(&first, &receipt.id).unwrap(), Some(receipt));
    assert_eq!(
        peer.receipt(&second, &receipt.id).unwrap(),
        Some(peer_receipt)
    );
    assert_eq!(ids(&store, &first), vec![receipt.id]);
    assert_eq!(ids(&peer, &second), vec![receipt.id]);
    let next = store
        .commit_catalog(&first, b"after fresh rotation")
        .unwrap();
    assert_eq!(
        first
            .authenticate(scope, store.wire(&first, &next.id).unwrap().unwrap())
            .unwrap()
            .header()
            .sequence,
        2
    );
}

#[test]
fn missing_files_are_never_reinitialized_by_join() {
    for file in ["table.astream", "table.checkpoint", "table.anchor"] {
        let lab = Lab::new(&[Role::Writer]);
        let root = aterm_tempfile::tempdir().unwrap();
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let mut store = Store::create(&session, root.path()).unwrap();
        store.commit_catalog(&session, b"acknowledged").unwrap();
        let scope = store.scope();
        drop(store);
        std::fs::remove_file(root.path().join(file)).unwrap();
        assert!(Store::join(&session, root.path(), scope).is_err());
        assert!(!root.path().join(file).exists());
    }
}

#[test]
fn pending_initialization_can_resume_but_established_directory_loss_cannot() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let scope = session.scope().unwrap();
    // An unavailable destination interrupts after persisting the pending claim.
    let blocked = root.path().join("blocked");
    std::fs::write(&blocked, b"file").unwrap();
    assert!(Store::join(&session, &blocked, scope).is_err());
    assert!(Store::open(&session, &root.path().join("ready"), scope).is_err());
    let mut store = Store::join(&session, &root.path().join("ready"), scope).unwrap();
    store.commit_catalog(&session, b"committed").unwrap();
    drop(store);
    assert!(Store::join(&session, &root.path().join("elsewhere"), scope).is_err());
}

#[test]
fn cutoffs_constrain_demoted_history_and_its_dependents_without_discarding_evidence() {
    use aterm_secrets::commons::constraints::{Fact, Frontier};
    for cutoff in [false, true] {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let root = aterm_tempfile::tempdir().unwrap();
        let a = lab.bootstrap(0);
        let b = lab.bootstrap(1);
        let first = Session::open(&a).unwrap();
        let second = Session::open(&b).unwrap();
        let mut store = Store::create(&first, root.path()).unwrap();
        let scope = store.scope();
        let peer_actor = second.actor();
        let permitted = second.seal(scope, causal(), b"old peer account").unwrap();
        let receipt = store.ingest(&first, &permitted).unwrap();
        let later = second
            .seal(
                scope,
                Causal {
                    sequence: 2,
                    previous: Some(receipt.id),
                    parents: &[receipt.id],
                },
                b"backdated old-key edit",
            )
            .unwrap();
        let later_id = store.ingest(&first, &later).unwrap().id;
        let dependent = first
            .seal(
                scope,
                Causal {
                    sequence: 1,
                    previous: None,
                    parents: &[later_id],
                },
                b"carries disallowed state",
            )
            .unwrap();
        store.ingest(&first, &dependent).unwrap();
        drop(store);
        drop(first);
        drop(second);
        let facts = if cutoff {
            vec![Fact::Cutoff {
                table: scope.digest(),
                actor: peer_actor,
                through: Some(Frontier {
                    sequence: 1,
                    digest: receipt.id,
                }),
            }]
        } else {
            vec![]
        };
        rotate_with_facts(&lab, &[Role::Writer, Role::Reader], &facts);
        let current = Session::open(&a).unwrap();
        let store = Store::open(&current, root.path(), scope).unwrap();
        assert_eq!(
            ids(&store, &current),
            if cutoff { vec![receipt.id] } else { vec![] }
        );
        assert_eq!(
            store.inventory(&current).unwrap().count(),
            3,
            "liability/fork evidence is retained"
        );
    }
}
fn rotate_with_facts(
    lab: &Lab,
    roles: &[Role],
    extras: &[aterm_secrets::commons::constraints::Fact],
) {
    use aterm_secrets::commons::node::Edit;
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let bootstraps: Vec<_> = (0..lab.paths.len()).map(|n| lab.bootstrap(n)).collect();
    let mut journal =
        Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
    let grants = bootstraps
        .iter()
        .zip(roles)
        .enumerate()
        .map(|(n, (b, role))| {
            Grant::recorded(
                format!("m{}", n + 1),
                b.request().unwrap().binding().clone(),
                *role,
            )
        })
        .collect();
    let mut node = admin.prepare_commons_rotation(&journal, grants).unwrap();
    if !extras.is_empty() {
        let identity = client::machine_identity(&lab.paths[0]).unwrap();
        let payload = node.open(&identity).unwrap();
        let mut facts = node.facts().to_vec();
        facts.extend_from_slice(extras);
        let hosts: Vec<_> = (1..=roles.len()).map(|n| format!("m{n}")).collect();
        node = admin
            .seal_commons_restricted(
                NETWORK,
                Edit::Update {
                    parent: node.parents()[0],
                    hosts: &hosts,
                    payload: payload.as_slice(),
                },
                &facts,
            )
            .unwrap();
    }
    journal.ingest(node.bytes()).unwrap();
    drop(journal);
    for b in bootstraps.iter().skip(1) {
        Journal::open_existing(b.journal_dir(), b.trust())
            .unwrap()
            .ingest(node.bytes())
            .unwrap();
    }
}

fn storage_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let mut store = Store::create(&session, root.path()).unwrap();
    let scope = store.scope();
    let first = store.commit_catalog(&session, b"first").unwrap();
    let before = std::fs::read(root.path().join("table.astream")).unwrap();
    let checkpoint_before = std::fs::read(root.path().join("table.checkpoint")).unwrap();
    let scenario = match action {
        "Restart" => 1,
        "Truncate" => 2,
        "LostLog" => 3,
        "LostClaim" => 4,
        "ChangedScope" => 5,
        "TornSuffix" => 6,
        "UncertainAppend" => 7,
        "OwnFork" => 8,
        "CopiedStore" => 9,
        "RestoreBackup" => 10,
        other => panic!("unknown action {other}"),
    };
    let mut usable = true;
    match action {
        "Restart" => {}
        "Truncate" | "RestoreBackup" => {
            store.commit_catalog(&session, b"second").unwrap();
        }
        "UncertainAppend" => {
            let cp = root.path().join("table.checkpoint");
            let backup = root.path().join("saved-checkpoint");
            std::fs::rename(&cp, &backup).unwrap();
            std::fs::create_dir(&cp).unwrap();
            let wire = session
                .seal(
                    scope,
                    Causal {
                        sequence: 2,
                        previous: Some(first.id),
                        parents: &[first.id],
                    },
                    b"retry exact bytes",
                )
                .unwrap();
            assert!(store.ingest(&session, &wire).is_err());
            assert!(store.wire(&session, &first.id).is_err());
            assert!(store.catalog(&session).is_err());
            drop(store);
            std::fs::remove_dir(&cp).unwrap();
            std::fs::rename(&backup, &cp).unwrap();
            store = Store::open(&session, root.path(), scope).unwrap();
            assert_eq!(store.inventory(&session).unwrap().count(), 1);
            store.ingest(&session, &wire).unwrap();
        }
        "OwnFork" => {
            let fork = session
                .seal(scope, causal(), b"cloned writer fork")
                .unwrap();
            store.ingest(&session, &fork).unwrap();
            usable = store.commit_catalog(&session, b"must refuse").is_ok();
        }
        _ => {}
    }
    drop(store);
    match action {
        "RestoreBackup" => {
            std::fs::write(root.path().join("table.astream"), &before).unwrap();
            std::fs::write(root.path().join("table.checkpoint"), checkpoint_before).unwrap();
        }
        "Truncate" => std::fs::write(root.path().join("table.astream"), before).unwrap(),
        "LostLog" => std::fs::remove_file(root.path().join("table.astream")).unwrap(),
        "LostClaim" => {
            let dir = a.journal_dir().parent().unwrap().join("table-claims");
            std::fs::remove_file(dir.join(format!(
                "{}.claim",
                aterm_codec::hex::encode_lower(&scope.digest())
            )))
            .unwrap();
        }
        "TornSuffix" => {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(root.path().join("table.astream"))
                .unwrap();
            file.write_all(b"torn unacknowledged suffix").unwrap();
            aterm_durable::durable(&file).unwrap();
        }
        _ => {}
    }
    let expected_scope = if action == "ChangedScope" {
        session.scope().unwrap()
    } else {
        scope
    };
    let mut open_path = root.path().to_path_buf();
    if action == "CopiedStore" {
        open_path = root.path().join("copy");
        aterm_uds::private_file::create_private_dir(&open_path).unwrap();
        for entry in std::fs::read_dir(root.path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), open_path.join(entry.file_name())).unwrap();
            }
        }
    }
    let opened = Store::open(&session, &open_path, expected_scope);
    usable &= opened.is_ok();
    if let Ok(opened) = opened {
        assert!(opened.receipt(&session, &first.id).unwrap().is_some());
    }
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("usable", i64::from(usable));
    next
}
#[test]
fn durable_store_scenarios_conform_and_resetting_history_is_a_negative_control() {
    let model = aterm_models_db::table_durability_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = storage_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let before = aterm_spec::interp::State::from([("scenario", 0), ("usable", 0)]);
    let mut bad = storage_transition(&before, "LostLog");
    bad.insert("usable", 1);
    assert!(!model.successors("LostLog", &before).contains(&bad));
    assert!(!model.check_invariant("DamagedHistoryWithholdsWriter", &bad));
}

#[test]
fn a_log_commit_before_the_custody_write_is_recovered_without_reusing_its_sequence() {
    for count in [1, 2] {
        let lab = Lab::new(&[Role::Writer]);
        let root = aterm_tempfile::tempdir().unwrap();
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let mut store = Store::create(&session, root.path()).unwrap();
        let scope = store.scope();
        let claim_dir = bootstrap
            .journal_dir()
            .parent()
            .unwrap()
            .join("table-claims");
        let name = format!("{}.claim", aterm_codec::hex::encode_lower(&scope.digest()));
        let before = aterm_uds::private_file::read_private(&claim_dir, &name, 4096).unwrap();
        let first = store.commit_catalog(&session, b"committed log").unwrap();
        if count == 2 {
            store
                .commit_catalog(&session, b"impossible second unwitnessed append")
                .unwrap();
        }
        drop(store);
        // Recreate the cut point between log checkpoint and custody witness.
        // More than one such entry cannot arise from this single-writer protocol.
        aterm_uds::private_file::write_private(&claim_dir, &name, &before).unwrap();
        let opened = Store::open(&session, root.path(), scope);
        if count == 2 {
            assert!(opened.is_err());
            continue;
        }
        let mut store = opened.unwrap();
        assert_eq!(store.receipt(&session, &first.id).unwrap(), Some(first));
        let after = aterm_uds::private_file::read_private(&claim_dir, &name, 4096).unwrap();
        assert_ne!(before, after);
        let second = store.commit_catalog(&session, b"next sequence").unwrap();
        let record = session
            .authenticate(scope, store.wire(&session, &second.id).unwrap().unwrap())
            .unwrap();
        assert_eq!(record.header().sequence, 2);
    }
}

#[test]
fn custody_rejects_an_alternate_valid_transcript_at_the_same_record_count() {
    let lab = Lab::new(&[Role::Writer]);
    let root = aterm_tempfile::tempdir().unwrap();
    let alternate = aterm_tempfile::tempdir().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let mut store = Store::create(&session, root.path()).unwrap();
    let scope = store.scope();
    let first = store
        .commit_catalog(&session, b"acknowledged account")
        .unwrap();
    drop(store);
    let wire = session
        .seal(scope, causal(), b"different signed account")
        .unwrap();
    let anchor = std::fs::read(root.path().join("table.anchor")).unwrap();
    let mut log = aterm_journal::Log::open(
        alternate.path(),
        "table",
        &anchor,
        false,
        aterm_journal::Limits {
            records: 8,
            bytes: 1024 * 1024,
            record_bytes: MAX_OPERATION + 8,
        },
    )
    .unwrap();
    log.finish_open().unwrap();
    let mut body = first.received_ms.to_le_bytes().to_vec();
    body.extend_from_slice(&wire);
    log.append("commons/table/1", body).unwrap();
    drop(log);
    for name in ["table.astream", "table.checkpoint"] {
        std::fs::copy(alternate.path().join(name), root.path().join(name)).unwrap();
    }
    assert!(Store::open(&session, root.path(), scope).is_err());
}
