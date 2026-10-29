// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::legacy::publish_legacy;
use super::*;
use crate::{
    age,
    client::{self, Paths},
    commons::{
        binding::{Pending, Request},
        bootstrap::{Bootstrap, Descriptor, Device},
        journal::Journal,
        roster::{Grant, Role},
    },
    text::Audience,
    transport,
};
use aterm_spec::interp::State;

fn audience() -> Audience {
    Audience::Hosts(vec!["m1".into(), "m2".into()])
}

fn second_identity() -> age::Identity {
    age::Identity::from_secret_bytes([37; 32])
}

struct Lab {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    authority: [u8; 32],
}

impl Lab {
    fn new(legacy: bool) -> Self {
        use crate::commons::roster::Enrollment;
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("test.vault");
        client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        if legacy {
            publish_legacy(&paths, &client::admin_open(&paths).unwrap());
        }
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &second_identity().recipient().to_bech32())
            .unwrap();
        doc.set_entry(&audience(), crate::commons::ENTRY, b"pending")
            .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let admin = client::admin_open(&paths).unwrap();
        let authority = admin.commons_authority().unwrap();
        let device = Device::open(&paths).unwrap().unwrap();
        let request = device.request([29; 16], authority).unwrap();
        let (mut pending, challenge) = Pending::issue(&request, [29; 16], authority).unwrap();
        let possession = pending
            .verify(&device.answer(&request, &challenge).unwrap())
            .unwrap();
        let genesis = admin
            .prepare_commons_genesis(
                [29; 16],
                vec![Enrollment::new("m1", possession, Role::Writer)],
            )
            .unwrap();
        let descriptor = Descriptor::from_genesis(genesis).unwrap();
        doc.set_entry(&audience(), crate::commons::ENTRY, &descriptor.bytes())
            .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        if legacy {
            publish_legacy(&paths, &client::admin_open(&paths).unwrap());
        }
        Self {
            _root: root,
            paths,
            authority,
        }
    }

    fn rotate(&self) {
        let admin = client::admin_open(&self.paths).unwrap();
        let old_signer = admin.config.signer;
        client::admin_rotate(&self.paths, &admin, &admin.doc).unwrap();
        let reopened = client::admin_open(&self.paths).unwrap();
        assert_ne!(reopened.config.signer, old_signer);
    }

    fn sign_control(&self) -> bool {
        let admin = client::admin_open(&self.paths).unwrap();
        let bootstrap = Bootstrap::cached(&self.paths).unwrap().unwrap();
        let access = bootstrap.access().unwrap().unwrap();
        let mut journal =
            Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
        let identity = second_identity();
        let request = Request::create(
            [29; 16],
            self.authority,
            [41; 16],
            [43; 16],
            &identity,
            &[47; 32],
        )
        .unwrap();
        let (mut pending, challenge) = Pending::issue(&request, [29; 16], self.authority).unwrap();
        let possession = pending
            .verify(&request.answer(&identity, &[47; 32], &challenge).unwrap())
            .unwrap();
        let Ok(certificate) = admin.approve_commons_member(&journal, "m2", possession) else {
            return false;
        };
        let next = admin.prepare_commons_rotation(
            &journal,
            vec![
                Grant::recorded("m1", access.local().binding().clone(), Role::Reader),
                Grant::certified("m2", certificate, Role::Writer),
            ],
        );
        if let Ok(next) = next {
            assert_eq!(next.authority(), self.authority);
            journal.ingest(next.bytes()).unwrap();
            let retired = admin
                .prepare_commons_retirement(
                    &journal,
                    vec![Grant::recorded(
                        "m1",
                        access.local().binding().clone(),
                        Role::Reader,
                    )],
                    &[request.binding().actor()],
                )
                .unwrap();
            journal.ingest(retired.bytes()).unwrap();
            let descriptor = Descriptor::from_journal(&journal).unwrap();
            drop(journal);
            let mut doc = admin.doc.clone();
            doc.set_entry(&audience(), crate::commons::ENTRY, &descriptor.bytes())
                .unwrap();
            client::admin_publish(&self.paths, &admin, &doc).unwrap();
            let current = Bootstrap::cached(&self.paths).unwrap().unwrap();
            assert_eq!(
                current.access().unwrap().unwrap().local().role(),
                Role::Reader
            );
            true
        } else {
            false
        }
    }
}

pub(crate) fn custody_transition(state: &State, action: &str) -> State {
    assert_eq!(state["scenario"], 0);
    if matches!(action, "UnboundRotation" | "LegacyUnknown") {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let source = root.path().join("test.vault");
        client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
        let admin = client::admin_open(&paths).unwrap();
        if action == "UnboundRotation" {
            client::admin_rotate(&paths, &admin, &admin.doc).unwrap();
            let reopened = client::admin_open(&paths).unwrap();
            return State::from([
                ("scenario", 8),
                ("usable", 1),
                (
                    "stable",
                    i64::from(
                        admin.commons_authority().unwrap() == reopened.commons_authority().unwrap(),
                    ),
                ),
                (
                    "separate",
                    i64::from(
                        reopened.config.signer != Some(reopened.commons_authority().unwrap()),
                    ),
                ),
            ]);
        }
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &second_identity().recipient().to_bech32())
            .unwrap();
        doc.add_admin("m2").unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        publish_legacy(&paths, &client::admin_open(&paths).unwrap());
        let admin = client::admin_open(&paths).unwrap();
        assert!(admin.commons_bound);
        let mut doc = admin.doc.clone();
        doc.remove_admin("m2").unwrap();
        return State::from([
            ("scenario", 11),
            (
                "usable",
                i64::from(client::admin_publish(&paths, &admin, &doc).is_ok()),
            ),
            ("stable", 1),
            ("separate", 0),
        ]);
    }
    if action == "MissingCustody" {
        let doc = Document::new_vault(
            "m1",
            &age::Identity::from_secret_bytes([31; 32])
                .recipient()
                .to_bech32(),
        );
        let text = Zeroizing::new(format!(
            "{MAGIC}\nsigner-seed: {}\n\n",
            crate::b64::encode_padded(&[17; 32])
        ));
        let mut bytes = text.as_bytes().to_vec();
        bytes.extend_from_slice(doc.render().as_slice());
        return State::from([
            ("scenario", 6),
            (
                "usable",
                i64::from(parse_admin(&SecretBytes::new(bytes)).is_ok()),
            ),
            ("stable", 1),
            ("separate", 0),
        ]);
    }
    let lab = Lab::new(matches!(
        action,
        "LegacyMigration" | "LegacyRotation" | "LostLegacyKey"
    ));
    let mut separate = 0;
    let mut stable = 1;
    let (scenario, usable) = match action {
        "NewVault" => {
            let admin = client::admin_open(&lab.paths).unwrap();
            separate = i64::from(admin.config.signer != Some(lab.authority));
            (1, lab.sign_control())
        }
        "LegacyMigration" => {
            let admin = client::admin_open(&lab.paths).unwrap();
            client::admin_publish(&lab.paths, &admin, &admin.doc).unwrap();
            stable = i64::from(
                client::admin_open(&lab.paths)
                    .unwrap()
                    .commons_authority()
                    .unwrap()
                    == lab.authority,
            );
            (2, lab.sign_control())
        }
        "VaultRotation" | "LegacyRotation" => {
            lab.rotate();
            lab.rotate();
            let admin = client::admin_open(&lab.paths).unwrap();
            stable = i64::from(admin.commons_authority().unwrap() == lab.authority);
            separate = i64::from(admin.config.signer != Some(lab.authority));
            (
                if action == "VaultRotation" { 3 } else { 4 },
                lab.sign_control(),
            )
        }
        "LostLegacyKey" => {
            lab.rotate();
            publish_legacy(&lab.paths, &client::admin_open(&lab.paths).unwrap());
            let result = client::admin_open(&lab.paths);
            assert!(
                result
                    .as_ref()
                    .unwrap_err()
                    .to_string()
                    .contains("Commons authority custody")
            );
            (5, result.is_ok())
        }
        "ChangedDescriptor" => {
            let other = Lab::new(false);
            let admin = client::admin_open(&lab.paths).unwrap();
            let foreign = client::admin_open(&other.paths).unwrap();
            let audience = audience();
            let mut doc = admin.doc.clone();
            doc.set_entry(
                &audience,
                crate::commons::ENTRY,
                foreign
                    .doc
                    .entry(&audience, crate::commons::ENTRY)
                    .unwrap()
                    .value
                    .as_slice(),
            )
            .unwrap();
            let before = client::load_state(&lab.paths).unwrap().seq;
            let result = client::admin_publish(&lab.paths, &admin, &doc);
            assert_eq!(client::load_state(&lab.paths).unwrap().seq, before);
            (7, result.is_ok())
        }
        "BoundDemotion" | "RemovedDescriptor" => {
            let admin = client::admin_open(&lab.paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.add_admin("m2").unwrap();
            client::admin_publish(&lab.paths, &admin, &doc).unwrap();
            if action == "RemovedDescriptor" {
                let admin = client::admin_open(&lab.paths).unwrap();
                let mut doc = admin.doc.clone();
                doc.unset_entry(&audience(), crate::commons::ENTRY).unwrap();
                assert!(client::admin_publish(&lab.paths, &admin, &doc).is_err());
                // Older clients could publish a missing entry. Reading that
                // signed snapshot must still retain the bound custody guard.
                let bytes = client::seal_with_admissions_and_parents(
                    &admin.config.name,
                    admin.seq() + 1,
                    &admin.seed,
                    &admin.commons_seed,
                    admin.commons_bound,
                    &doc,
                    &admin.handoff,
                    &admin.opened_heads,
                    &crate::client::admission::Ledger::default()
                        .retained(&doc)
                        .unwrap(),
                )
                .unwrap();
                transport::publish(&admin.config.source, &bytes).unwrap();
                client::sync(&lab.paths, None).unwrap();
            }
            let mut admin = client::admin_open(&lab.paths).unwrap();
            assert!(admin.commons_bound);
            admin.doc.remove_admin("m2").unwrap();
            let before = client::load_state(&lab.paths).unwrap().seq;
            let result = client::admin_publish(&lab.paths, &admin, &admin.doc);
            assert_eq!(client::load_state(&lab.paths).unwrap().seq, before);
            (
                if action == "BoundDemotion" { 9 } else { 10 },
                result.is_ok(),
            )
        }
        _ => panic!("unknown custody action"),
    };
    State::from([
        ("scenario", scenario),
        ("usable", i64::from(usable)),
        ("stable", stable),
        ("separate", separate),
    ])
}

#[test]
fn authority_custody_conforms_with_negative_controls() {
    let model = aterm_models_db::authority_custody_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = custody_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
                let mut wrong = next;
                wrong.insert("stable", 1 - wrong["stable"]);
                let law = if wrong["scenario"] == 8 {
                    "UnusedAuthorityRotates"
                } else {
                    "RetainedAuthority"
                };
                assert!(!model.check_invariant(law, &wrong));
                assert!(!model.successors(action.name, &state).contains(&wrong));
            }
        }
    }
}

#[test]
fn malformed_custody_never_becomes_a_legacy_fallback() {
    let doc = Document::new_vault(
        "m1",
        &age::Identity::from_secret_bytes([31; 32])
            .recipient()
            .to_bech32(),
    );
    let signer = crate::b64::encode_padded(&[17; 32]);
    let commons = crate::b64::encode_padded(&[19; 32]);
    for header in [
        format!("{MAGIC}\nsigner-seed: {signer}"),
        format!("{MAGIC}\nsigner-seed: {signer}\ncommons-seed: bad"),
        format!("{MAGIC}\nsigner-seed: {signer}\ncommons-seed: {commons}\ncommons-seed: {commons}"),
        format!("{LEGACY_MAGIC}\nsigner-seed: {signer}\ncommons-seed: {commons}"),
        format!("alab-vault-admin/3\nsigner-seed: {signer}\ncommons-seed: {commons}"),
    ] {
        let mut bytes = format!("{header}\n\n").into_bytes();
        bytes.extend_from_slice(doc.render().as_slice());
        assert!(parse_admin(&SecretBytes::new(bytes)).is_err());
    }
}

#[test]
fn custody_records_its_format_and_a_later_format_names_itself() {
    let doc = Document::new_vault(
        "m1",
        &age::Identity::from_secret_bytes([31; 32])
            .recipient()
            .to_bech32(),
    );
    let with = |header: &str| {
        let mut bytes = format!("{header}\n\n").into_bytes();
        bytes.extend_from_slice(doc.render().as_slice());
        SecretBytes::new(bytes)
    };
    let signer = crate::b64::encode_padded(&[17; 32]);
    let legacy = parse_admin(&with(&format!("{LEGACY_MAGIC}\nsigner-seed: {signer}"))).unwrap();
    assert_eq!(legacy.custody, LEGACY_MAGIC);
    let current =
        admin_plaintext_with_admissions(&[17; 32], &[19; 32], false, &doc, &Ledger::default());
    assert_eq!(parse_admin(&current).unwrap().custody, MAGIC);

    let written: u32 = MAGIC
        .strip_prefix("alab-vault-admin/")
        .unwrap()
        .parse()
        .unwrap();
    let later = format!("alab-vault-admin/{}", written + 1);
    let refused = parse_admin(&with(&format!("{later}\nsigner-seed: {signer}")))
        .err()
        .expect("a later format is refused")
        .to_string();
    assert!(
        refused.contains(&format!(
            "{later} is newer than this aterm reads ({MAGIC} at most)"
        )),
        "{refused}"
    );
    for magic in [
        "alab-vault-admin/0",
        "alab-vault-admin/x",
        "alab-vault-admin/1234567",
        "vault/9",
        "",
    ] {
        let refused = parse_admin(&with(&format!("{magic}\nsigner-seed: {signer}")))
            .err()
            .expect("an unknown format is refused")
            .to_string();
        assert_eq!(
            refused, "admin locker: unsupported custody version",
            "{magic}"
        );
    }
}

#[test]
fn a_publish_that_rewrites_a_legacy_locker_asks_the_running_approver_first() {
    use client::locker_skew::{PLAYED, Running};
    let play = |running: Option<Running>| {
        PLAYED.with(|played| *played.borrow_mut() = running.map(|running| vec![running]));
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("test.vault");
    client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    publish_legacy(&paths, &client::admin_open(&paths).unwrap());
    let admin = client::admin_open(&paths).unwrap();
    assert_eq!(admin.custody, LEGACY_MAGIC);
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &second_identity().recipient().to_bech32())
        .unwrap();
    let old = Running {
        pid: Some(8000),
        program: "/opt/old/aterm".into(),
        version: Ok((0, 1, 0)),
    };

    play(Some(old.clone()));
    let refused = client::admin_publish(&paths, &admin, &doc)
        .expect_err("an older approver refuses the rewrite")
        .to_string();
    assert!(
        refused.contains(&format!("from {LEGACY_MAGIC} to {MAGIC}")),
        "{refused}"
    );
    assert!(refused.contains("(/opt/old/aterm secrets …)"), "{refused}");
    let after = client::admin_open(&paths).unwrap();
    assert_eq!(after.seq(), admin.seq(), "nothing was published");
    assert_eq!(after.custody, LEGACY_MAGIC);

    // A revoke's publish, checked before its retirement, does not ask again.
    client::admin_publish_checked(&paths, &admin, &doc).unwrap();
    assert_eq!(client::admin_open(&paths).unwrap().custody, MAGIC);
    publish_legacy(&paths, &client::admin_open(&paths).unwrap());
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&audience(), "again", b"x").unwrap();

    // CONTROL: with no approver running here, the same publish rewrites the locker.
    play(None);
    client::admin_publish(&paths, &admin, &doc).unwrap();
    let rewritten = client::admin_open(&paths).unwrap();
    assert_eq!(rewritten.custody, MAGIC);

    // A publish that keeps the locker's format asks nothing of the approver.
    play(Some(old));
    let mut doc = rewritten.doc.clone();
    doc.set_entry(&audience(), "note", b"kept").unwrap();
    client::admin_publish(&paths, &rewritten, &doc).unwrap();
    play(None);
}

#[test]
fn a_revoke_that_would_strand_the_approver_refuses_before_it_publishes_anything() {
    use client::locker_skew::{PLAYED, Running};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("test.vault");
    client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &second_identity().recipient().to_bech32())
        .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    publish_legacy(&paths, &client::admin_open(&paths).unwrap());
    let admin = client::admin_open(&paths).unwrap();
    assert_eq!(admin.custody, LEGACY_MAGIC);

    PLAYED.with(|played| {
        *played.borrow_mut() = Some(vec![Running {
            pid: Some(8000),
            program: "/opt/old/aterm".into(),
            version: Ok((0, 1, 0)),
        }]);
    });
    let refused = match client::admin_revoke(&paths, &admin, "m2", false) {
        Ok(_) => panic!("an older approver refuses the revoke"),
        Err(error) => error.to_string(),
    };
    assert!(refused.starts_with("refusing to publish:"), "{refused}");
    assert_eq!(
        client::admin_open(&paths).unwrap().seq(),
        admin.seq(),
        "nothing was published"
    );
    // CONTROL: with no approver here, the same revoke publishes. A version-1 vault (bound,
    // with no Commons control) removes the member from the vault alone, and stays bound.
    PLAYED.with(|played| *played.borrow_mut() = None);
    let done = match client::admin_revoke(&paths, &admin, "m2", false) {
        Ok(done) => done,
        Err(error) => panic!("a vault without Commons control revokes: {error}"),
    };
    assert!(done.retirement.is_none());
    let after = client::admin_open(&paths).unwrap();
    assert_eq!(after.custody, MAGIC);
    assert!(after.commons_bound);
    assert!(
        !after
            .doc
            .machines()
            .iter()
            .any(|machine| machine.name == "m2")
    );
}

#[test]
fn rotated_custody_reaches_current_admins_and_refuses_unsafe_demotion() {
    let lab = Lab::new(false);
    let identity = second_identity();
    let decrypt = || {
        let bytes = std::fs::read(lab.paths.cache_file()).unwrap();
        let envelope = envelope::parse(&bytes).unwrap();
        age::decrypt(
            std::slice::from_ref(&identity),
            envelope.admin_locker().unwrap().armored.as_bytes(),
        )
    };
    assert!(
        decrypt().is_err(),
        "an ordinary member receives no admin keys"
    );
    let admin = client::admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_admin("m2").unwrap();
    client::admin_publish(&lab.paths, &admin, &doc).unwrap();
    lab.rotate();
    let held = parse_admin(&decrypt().unwrap()).unwrap();
    assert_eq!(
        envelope::signer_public(&held.commons_seed).unwrap(),
        lab.authority
    );
    assert_ne!(held.seed, held.commons_seed);
    let admin = client::admin_open(&lab.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.remove_admin("m2").unwrap();
    assert!(client::admin_publish(&lab.paths, &admin, &doc).is_err());
    assert!(
        decrypt().is_ok(),
        "the refused edit leaves the vault unchanged"
    );
    // A vault-only demotion could not erase this retained Commons authority.
    assert_eq!(
        envelope::signer_public(&held.commons_seed).unwrap(),
        lab.authority
    );
    assert!(lab.sign_control(), "the remaining admin retains control");
}

#[test]
fn editing_the_public_draft_cannot_hide_an_unbound_admin_removal() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("test.vault");
    client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let mut admin = client::admin_open(&paths).unwrap();
    admin
        .doc
        .add_machine("m2", &second_identity().recipient().to_bech32())
        .unwrap();
    admin.doc.add_admin("m2").unwrap();
    client::admin_publish(&paths, &admin, &admin.doc).unwrap();
    let mut admin = client::admin_open(&paths).unwrap();
    let authority = admin.commons_authority().unwrap();
    admin.doc.remove_admin("m2").unwrap();
    let published = client::admin_publish(&paths, &admin, &admin.doc).unwrap();
    assert!(published.rotated);
    let reopened = client::admin_open(&paths).unwrap();
    assert_ne!(reopened.commons_authority().unwrap(), authority);
    assert!(!reopened.commons_bound);
    let bytes = std::fs::read(paths.cache_file()).unwrap();
    let envelope = envelope::parse(&bytes).unwrap();
    assert!(
        age::decrypt(
            &[second_identity()],
            envelope.admin_locker().unwrap().armored.as_bytes()
        )
        .is_err()
    );
}
