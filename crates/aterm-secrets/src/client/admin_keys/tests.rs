// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    age,
    client::{self, Admin, Paths},
    commons::{
        binding::{Pending, Request},
        bootstrap::{Bootstrap, Descriptor, Device},
        journal::Journal,
        roster::{Grant, Role},
    },
    envelope::Unsigned,
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

fn legacy_plaintext(admin: &Admin) -> SecretBytes {
    let seed = Zeroizing::new(crate::b64::encode_padded(&admin.seed));
    let mut bytes = format!("{LEGACY_MAGIC}\nsigner-seed: {}\n\n", seed.as_str()).into_bytes();
    bytes.extend_from_slice(admin.doc.render().as_slice());
    SecretBytes::new(bytes)
}

/// Emulate an old admin's complete signed publication, including loss of the
/// independent Commons field. This is deliberately unavailable in production.
fn publish_legacy(paths: &Paths, admin: &Admin) {
    let bytes = client::seal(
        &admin.config.name,
        admin.seq() + 1,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &admin.doc,
        &admin.handoff,
    )
    .unwrap();
    let parsed = envelope::parse(&bytes).unwrap();
    let mut lockers = parsed.lockers.clone();
    let recipients = client::recipients_of(&admin.doc.admins(), &admin.doc).unwrap();
    lockers[0].armored =
        age::armor(&age::encrypt(&recipients, legacy_plaintext(admin).as_slice()).unwrap());
    let bytes = envelope::sign(
        &Unsigned {
            name: parsed.name.clone(),
            seq: parsed.seq,
            time: parsed.time,
            handoff: parsed.handoff.clone(),
            lockers,
        },
        &admin.seed,
    )
    .unwrap();
    transport::publish(&admin.config.source, &bytes).unwrap();
    client::sync(paths, None).unwrap();
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
                let bytes = client::seal(
                    &admin.config.name,
                    admin.seq() + 1,
                    &admin.seed,
                    &admin.commons_seed,
                    admin.commons_bound,
                    &doc,
                    &admin.handoff,
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
