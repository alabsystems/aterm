// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{binding::Pending, node::Edit, roster::Role};

struct Lab {
    root: aterm_tempfile::TempDir,
    paths: [Paths; 2],
    source: PathBuf,
}

impl Lab {
    fn new(enrolled: bool, legacy: bool) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = [
            Paths::for_home(&root.path().join("m1")),
            Paths::for_home(&root.path().join("m2")),
        ];
        let source = root.path().join("vault");
        let created = if legacy {
            test_create_unbound(&paths[0], source.to_str().unwrap(), None, Some("m1"))
        } else {
            create(&paths[0], source.to_str().unwrap(), None, Some("m1"))
        }
        .unwrap();
        let recipient = machine_identity(&paths[1]).unwrap().recipient().to_bech32();
        let admin = admin_cached(&paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &recipient).unwrap();
        admin_publish_approved(&paths[0], &admin, &doc, &[recipient]).unwrap();
        join(
            &paths[1],
            &JoinOptions {
                source: format!("{}#{}", source.display(), created.signer),
                host: Some("m2".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        let lab = Self {
            root,
            paths,
            source,
        };
        if enrolled {
            let owner = lab.bootstrap(0);
            let peer = lab.bootstrap(1);
            let request = peer.request().unwrap();
            let (mut pending, challenge) =
                Pending::issue(&request, peer.trust().network, peer.trust().authority).unwrap();
            let answer = peer.device().answer(&request, &challenge).unwrap();
            let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
            admin_cached(&lab.paths[0])
                .unwrap()
                .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
                .unwrap();
        }
        lab
    }
    fn bootstrap(&self, n: usize) -> Bootstrap {
        Bootstrap::required(&self.paths[n]).unwrap()
    }
    fn third(&self) -> String {
        let paths = Paths::for_home(&self.root.path().join("m3"));
        let recipient = machine_identity(&paths).unwrap().recipient().to_bech32();
        let admin = admin_cached(&self.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m3", &recipient).unwrap();
        admin_publish_approved(
            &self.paths[0],
            &admin,
            &doc,
            std::slice::from_ref(&recipient),
        )
        .unwrap();
        join(
            &paths,
            &JoinOptions {
                source: format!(
                    "{}#{}",
                    self.source.display(),
                    crate::b64::encode_padded(&admin.config.signer.unwrap())
                ),
                host: Some("m3".into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: &|| None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        let owner = self.bootstrap(0);
        let peer = Bootstrap::required(&paths).unwrap();
        let request = peer.request().unwrap();
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        let response = peer.device().answer(&request, &challenge).unwrap();
        let admin = admin_cached(&self.paths[0]).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin
            .enroll_commons_member(&owner, &mut journal, pending.verify(&response).unwrap())
            .unwrap();
        recipient
    }
    fn obstruct(&self) {
        std::fs::rename(&self.source, self.root.path().join("held-vault")).unwrap();
        std::fs::create_dir(&self.source).unwrap();
    }
    fn legacy_remove(&self, names: &[&str]) {
        // Simulate a signed publication from the earlier vault-only workflow.
        let admin = admin_cached(&self.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        for name in names {
            doc.remove_machine(name, false).unwrap();
        }
        let sequence = envelope::next_seq(admin.seq).unwrap();
        let admissions = admin.admissions.retained(&doc).unwrap();
        let bytes = seal_with_admissions(
            &admin.config.name,
            sequence,
            &admin.seed,
            &admin.commons_seed,
            true,
            &doc,
            &admin.handoff,
            &admissions,
        )
        .unwrap();
        transport::publish(&admin.config.source, &bytes).unwrap();
        sync(&self.paths[0], None).unwrap();
    }
    fn restore(&self) {
        std::fs::remove_dir(&self.source).unwrap();
        std::fs::rename(self.root.path().join("held-vault"), &self.source).unwrap();
    }
    fn publish_failure(&self) {
        let admin = admin_cached(&self.paths[0]).unwrap();
        self.obstruct();
        let error = admin_revoke(&self.paths[0], &admin, "m2", false)
            .err()
            .unwrap();
        assert!(error.to_string().contains("permanently retired"), "{error}");
        assert!(
            admin_cached(&self.paths[0])
                .unwrap()
                .doc
                .machines()
                .iter()
                .any(|machine| machine.name == "m2")
        );
    }
}

pub(crate) fn retirement_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let bound = action != "Legacy";
    let lab = Lab::new(
        !matches!(action, "NoBinding" | "Legacy"),
        action == "Legacy",
    );
    if action == "Legacy" {
        let admin = admin_cached(&lab.paths[0]).unwrap();
        let result = admin_revoke(&lab.paths[0], &admin, "m2", false).unwrap();
        assert!(result.retirement.is_none());
        let mut next = state.clone();
        for (name, value) in [
            ("scenario", 5),
            ("bound", 0),
            ("retired", 0),
            ("published", 1),
            ("same", 1),
        ] {
            next.insert(name, value);
        }
        return next;
    }
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let actor = peer.request().unwrap().binding().actor();
    let mut targets = vec![peer.request().unwrap().binding().recipient().to_bech32()];
    let scenario = match action {
        "Revoke" => 1,
        "PublishFailure" => 2,
        "Retry" => {
            lab.publish_failure();
            lab.restore();
            3
        }
        "NoBinding" => 4,
        "SelfRemoval" => 6,
        "Admin" => {
            let admin = admin_cached(&lab.paths[0]).unwrap();
            let mut doc = admin.doc.clone();
            doc.add_admin("m2").unwrap();
            admin_publish(&lab.paths[0], &admin, &doc).unwrap();
            7
        }
        "Fork" => {
            let admin = admin_cached(&lab.paths[0]).unwrap();
            let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
            let head = *owner.access_in(&journal).unwrap().unwrap().head();
            for payload in [b"left", b"rght"] {
                let node = admin
                    .seal_commons(
                        owner.trust().network,
                        Edit::Update {
                            parent: head,
                            hosts: &["m1".into()],
                            payload,
                        },
                    )
                    .unwrap();
                journal.ingest(node.bytes()).unwrap();
            }
            8
        }
        "MissingIdentity" => 9,
        "RepairRemoved" => {
            lab.legacy_remove(&["m2"]);
            10
        }
        "BatchRemoved" => {
            targets.push(lab.third());
            lab.legacy_remove(&["m2", "m3"]);
            11
        }
        "UnknownBatch" => {
            targets.push(
                Identity::from_secret_bytes([77; 32])
                    .recipient()
                    .to_bech32(),
            );
            12
        }
        "GenericRemoval" => 13,
        _ => panic!("unknown retirement action"),
    };
    let before = Journal::open_existing(owner.journal_dir(), owner.trust())
        .unwrap()
        .inventory()
        .count();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    if action == "PublishFailure" {
        lab.obstruct();
    }
    if action == "MissingIdentity" {
        std::fs::rename(
            lab.paths[0].identity_file(),
            lab.root.path().join("held-identity"),
        )
        .unwrap();
    }
    let result = if action == "GenericRemoval" {
        let mut doc = admin.doc.clone();
        doc.remove_machine("m2", false).unwrap();
        admin_publish(&lab.paths[0], &admin, &doc).map(|_| ())
    } else if matches!(action, "RepairRemoved" | "BatchRemoved" | "UnknownBatch") {
        admin_retire(&lab.paths[0], &admin, &targets).map(|_| ())
    } else {
        admin_revoke(
            &lab.paths[0],
            &admin,
            if action == "SelfRemoval" { "m1" } else { "m2" },
            false,
        )
        .map(|_| ())
    };
    if action == "MissingIdentity" {
        std::fs::rename(
            lab.root.path().join("held-identity"),
            lab.paths[0].identity_file(),
        )
        .unwrap();
    }
    if (6..=9).contains(&scenario)
        || matches!(action, "PublishFailure" | "UnknownBatch" | "GenericRemoval")
    {
        assert!(result.is_err());
    } else {
        assert!(result.is_ok());
    }
    let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let after = journal.inventory().count();
    let retired = journal
        .inventory()
        .filter_map(|id| journal.node(id))
        .any(|node| {
            node.facts().contains(&Fact::Retire { actor })
                || node.facts().contains(&Fact::RetireRecipient {
                    recipient: peer.request().unwrap().binding().recipient(),
                })
        });
    if action == "BatchRemoved" {
        assert_eq!(
            owner.access_in(&journal).unwrap().unwrap().members().len(),
            1
        );
        assert_eq!(
            journal
                .constraints()
                .unwrap()
                .facts()
                .filter(|fact| matches!(fact, Fact::RetireRecipient { .. }))
                .count(),
            2
        );
    }
    let published = !admin_cached(&lab.paths[0])
        .unwrap()
        .doc
        .machines()
        .iter()
        .any(|machine| machine.name == "m2");
    let mut next = state.clone();
    for (name, value) in [
        ("scenario", scenario),
        ("bound", i64::from(bound)),
        ("retired", i64::from(retired)),
        ("published", i64::from(published)),
        ("same", i64::from(before == after)),
    ] {
        next.insert(name, value);
    }
    next
}

#[test]
fn retirement_publication_matches_real_failures_retries_and_preflight_refusals() {
    let model = aterm_models_db::retirement_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let next = retirement_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
    }
    let mut mutant = retirement_transition(&initial, "Retry");
    mutant.insert("same", 0);
    assert!(!model.check_invariant("RetryKeepsEpoch", &mutant));
}

#[test]
fn retirement_preserves_other_roles_and_reapproval_cannot_resurrect_the_recipient() {
    let lab = Lab::new(true, false);
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let access = owner.access_in(&journal).unwrap().unwrap();
    let grants = access
        .members()
        .iter()
        .map(|member| {
            Grant::recorded(
                if member.binding().recipient() == access.local().binding().recipient() {
                    "m1"
                } else {
                    "m2"
                },
                member.binding().clone(),
                Role::Reader,
            )
        })
        .collect();
    let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
    journal.ingest(node.bytes()).unwrap();
    drop(journal);
    let result = admin_revoke(&lab.paths[0], &admin, "m2", false).unwrap();
    assert!(result.retirement.unwrap().changed);
    assert_eq!(
        owner.access().unwrap().unwrap().local().role(),
        Role::Reader
    );
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let recipient = peer.request().unwrap().binding().recipient().to_bech32();
    doc.add_machine("m2", &recipient).unwrap();
    admin_publish_approved(&lab.paths[0], &admin, &doc, &[recipient]).unwrap();
    let request = peer.request().unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, peer.trust().network, peer.trust().authority).unwrap();
    let response = peer.device().answer(&request, &challenge).unwrap();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    assert!(
        admin
            .enroll_commons_member(&owner, &mut journal, pending.verify(&response).unwrap())
            .is_err()
    );
}

pub(crate) fn recipient_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(false, false);
    let owner = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let request = peer.request().unwrap();
    let recipient = request.binding().recipient();
    let admin = admin_cached(&lab.paths[0]).unwrap();
    let possession = || {
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        pending
            .verify(&peer.device().answer(&request, &challenge).unwrap())
            .unwrap()
    };
    let concurrent = if action == "ConcurrentRecovery" {
        let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        let certificate = admin
            .auto_approve_commons_member(&journal, "m2", possession())
            .unwrap();
        Some(vec![
            admin
                .prepare_commons_rotation(
                    &journal,
                    vec![
                        Grant::recorded(
                            "m1",
                            owner.request().unwrap().binding().clone(),
                            Role::Writer,
                        ),
                        Grant::certified("m2", certificate, Role::Writer),
                    ],
                )
                .unwrap(),
        ])
    } else if action == "UnboundEpoch" {
        let journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        let retained: Vec<_> = journal
            .inventory()
            .map(|id| journal.node(id).unwrap().clone())
            .collect();
        drop(journal);
        let mut other =
            Journal::open(&lab.root.path().join("offline-admin"), owner.trust()).unwrap();
        for node in retained {
            other.ingest(node.bytes()).unwrap();
        }
        let grants = || {
            vec![Grant::recorded(
                "m1",
                owner.request().unwrap().binding().clone(),
                Role::Writer,
            )]
        };
        let first = admin.prepare_commons_rotation(&other, grants()).unwrap();
        other.ingest(first.bytes()).unwrap();
        let second = admin.prepare_commons_rotation(&other, grants()).unwrap();
        Some(vec![first, second])
    } else {
        None
    };
    let retirement = admin_revoke(&lab.paths[0], &admin, "m2", false)
        .unwrap()
        .retirement
        .unwrap();
    assert_eq!(
        retirement.bindings, 0,
        "no actor was known when the recipient was retired"
    );
    let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
    let mut old_ready = false;
    if let Some(concurrent) = concurrent {
        for node in &concurrent {
            journal.ingest(node.bytes()).unwrap();
        }
        let head = concurrent.last().unwrap().id();
        let recovery = admin
            .seal_commons(
                owner.trust().network,
                Edit::Recover {
                    heads: &[retirement.head, head],
                    selected: head,
                },
            )
            .unwrap();
        journal.ingest(recovery.bytes()).unwrap();
        old_ready = owner
            .access_in(&journal)
            .is_ok_and(|access| access.is_some());
        let fresh = admin
            .prepare_commons_rotation(
                &journal,
                vec![Grant::recorded(
                    "m1",
                    owner.request().unwrap().binding().clone(),
                    Role::Writer,
                )],
            )
            .unwrap();
        journal.ingest(fresh.bytes()).unwrap();
    }
    let retired = journal
        .constraints()
        .unwrap()
        .recipient_retired(&recipient)
        .unwrap();
    let allowed = admin
        .enroll_commons_member(&owner, &mut journal, possession())
        .is_ok();
    let ready = owner
        .access_in(&journal)
        .is_ok_and(|access| access.is_some());
    let mut next = state.clone();
    for (name, value) in [
        (
            "scenario",
            match action {
                "LateBinding" => 1,
                "ConcurrentRecovery" => 2,
                "UnboundEpoch" => 3,
                _ => panic!("unknown recipient action"),
            },
        ),
        ("retired", i64::from(retired)),
        ("allowed", i64::from(allowed)),
        ("old_ready", i64::from(old_ready)),
        ("ready", i64::from(ready)),
    ] {
        next.insert(name, value);
    }
    next
}

#[test]
fn recipient_retirement_survives_delayed_binding_and_concurrent_recovery() {
    let model = aterm_models_db::recipient_retirement_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let next = recipient_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&next),
            "{}: {next:?}",
            action.name
        );
    }
    let mut mutant = recipient_transition(&initial, "ConcurrentRecovery");
    mutant.insert("old_ready", 1);
    assert!(!model.check_invariant("OldKeyWithheld", &mutant));
}
