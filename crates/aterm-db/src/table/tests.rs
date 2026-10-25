// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::{
    client::{self, Paths},
    commons::{
        self,
        binding::Pending,
        bootstrap::{Descriptor, Device},
        roster::{Enrollment, Grant},
    },
    text::Audience,
};
use std::{path::PathBuf, time::Duration};

const NETWORK: [u8; 16] = [9; 16];

struct Lab {
    _home: aterm_tempfile::TempDir,
    paths: Vec<Paths>,
    source: PathBuf,
}
impl Lab {
    fn new(roles: &[Role]) -> Self {
        let home = aterm_tempfile::tempdir().unwrap();
        let source = home.path().join("test.vault");
        let paths: Vec<_> = (0..roles.len())
            .map(|n| Paths::for_home(&home.path().join(format!("m{}", n + 1))))
            .collect();
        client::create(&paths[0], source.to_str().unwrap(), None, Some("m1")).unwrap();
        let admin = client::admin_open(&paths[0]).unwrap();
        let mut document = admin.doc.clone();
        for (n, path) in paths.iter().enumerate().skip(1) {
            let identity = client::machine_identity(path).unwrap();
            document
                .add_machine(&format!("m{}", n + 1), &identity.recipient().to_bech32())
                .unwrap();
        }
        let hosts = || (1..=roles.len()).map(|n| format!("m{n}")).collect();
        document
            .set_entry(&Audience::Hosts(hosts()), commons::ENTRY, b"pending")
            .unwrap();
        client::admin_publish(&paths[0], &admin, &document).unwrap();
        let admin = client::admin_open(&paths[0]).unwrap();
        let enrollments = paths
            .iter()
            .zip(roles)
            .enumerate()
            .map(|(n, (path, role))| {
                let device = Device::open(path).unwrap().unwrap();
                let request = device
                    .request(NETWORK, admin.config.signer.unwrap())
                    .unwrap();
                let (mut pending, challenge) =
                    Pending::issue(&request, NETWORK, *request.binding().authority()).unwrap();
                let proof = pending
                    .verify(&device.answer(&request, &challenge).unwrap())
                    .unwrap();
                Enrollment::new(format!("m{}", n + 1), proof, *role)
            })
            .collect();
        let genesis = admin.prepare_commons_genesis(NETWORK, enrollments).unwrap();
        let descriptor = Descriptor::from_genesis(genesis).unwrap();
        document
            .set_entry(
                &Audience::Hosts(hosts()),
                commons::ENTRY,
                &descriptor.bytes(),
            )
            .unwrap();
        client::admin_publish(&paths[0], &admin, &document).unwrap();
        for (n, path) in paths.iter().enumerate().skip(1) {
            client::join(
                path,
                &client::JoinOptions {
                    source: format!(
                        "{}#{}",
                        source.display(),
                        aterm_secrets::b64::encode_padded(&descriptor.trust().authority)
                    ),
                    host: Some(format!("m{}", n + 1)),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: &|| None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
        }
        Self {
            _home: home,
            paths,
            source,
        }
    }
    fn bootstrap(&self, n: usize) -> Bootstrap {
        Bootstrap::cached(&self.paths[n]).unwrap().unwrap()
    }
    fn rotate(&self, roles: &[Role]) {
        let admin = client::admin_open(&self.paths[0]).unwrap();
        let bootstraps: Vec<_> = (0..self.paths.len()).map(|n| self.bootstrap(n)).collect();
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
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        drop(journal);
        for b in bootstraps.iter().skip(1) {
            Journal::open_existing(b.journal_dir(), b.trust())
                .unwrap()
                .ingest(node.bytes())
                .unwrap();
        }
    }
}

fn causal() -> Causal<'static> {
    Causal {
        sequence: 1,
        previous: None,
        parents: &[],
    }
}

#[test]
fn one_or_two_existing_members_read_signed_ciphertext_without_plaintext_keys() {
    for count in [1, 2] {
        let lab = Lab::new(&vec![Role::Writer; count]);
        let a = lab.bootstrap(0);
        let session = Session::open(&a).unwrap();
        let scope = session.scope().unwrap();
        let wire = session
            .seal_at(scope, causal(), b"private-account-metadata", u64::MAX)
            .unwrap();
        assert!(
            !wire
                .windows(24)
                .any(|bytes| bytes == b"private-account-metadata")
        );
        let other_wire = session
            .seal_at(scope, causal(), b"private-account-metadata", u64::MAX)
            .unwrap();
        assert_ne!(
            wire, other_wire,
            "a restored sequence cannot reuse the record subkey"
        );
        assert!(
            Session::open(&a).is_err(),
            "control observation is locked during the local batch"
        );
        drop(session);
        for n in 0..count {
            let bootstrap = lab.bootstrap(n);
            let session = Session::open(&bootstrap).unwrap();
            let record = session.authenticate(scope, &wire).unwrap();
            assert_eq!(record.payload(), b"private-account-metadata");
            assert_eq!(record.authored_ms(), u64::MAX);
            assert_eq!(
                record.header().actor,
                a.request().unwrap().binding().actor()
            );
            assert_eq!(record.header().id, Sha256::digest(&wire));
            assert_eq!(record.epoch(), 1);
            assert_eq!(record.scope(), scope);
            assert!(!lab.paths[n].dir.join("database.json").exists());
        }
        assert!(lab.source.exists());
    }
}

#[test]
fn readers_can_authenticate_but_cannot_write_even_with_a_copied_header() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let sa = Session::open(&a).unwrap();
    let sb = Session::open(&b).unwrap();
    let scope = sa.scope().unwrap();
    let wire = sa.seal(scope, causal(), b"account").unwrap();
    assert!(sb.authenticate(scope, &wire).is_ok());
    assert!(sb.seal(scope, causal(), b"forged").is_err());
    assert!(sb.scope().is_err());
    let mut forgery = wire;
    let at = 5 + SCOPE_LEN + 32 + 8;
    forgery[at..at + 48].copy_from_slice(&sb.actor());
    resign(&b, &mut forgery);
    assert!(
        sa.authenticate(scope, &forgery)
            .err()
            .unwrap()
            .0
            .contains("historical writer")
    );
}

fn resign(bootstrap: &Bootstrap, wire: &mut [u8]) {
    let end = wire.len() - 64;
    let signature = bootstrap
        .device()
        .with_seed(|seed| crypto::signed(seed, DOMAIN, &wire[..end]))
        .unwrap();
    wire[end..].copy_from_slice(&signature);
}

#[test]
fn exact_context_epoch_and_causal_shape_are_authenticated() {
    let lab = Lab::new(&[Role::Writer]);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let scope = session.scope().unwrap();
    let wire = session.seal(scope, causal(), b"account").unwrap();
    for alternative in [
        Scope::fresh(a.trust()).unwrap(),
        Scope::new(a.trust(), *scope.table(), 2).unwrap(),
    ] {
        assert!(session.authenticate(alternative, &wire).is_err());
    }
    let mut trust = a.trust();
    trust.genesis[0] ^= 1;
    assert!(
        session
            .authenticate(Scope::new(trust, *scope.table(), 1).unwrap(), &wire)
            .is_err()
    );
    for at in [
        0,
        5,
        5 + SCOPE_LEN,
        5 + SCOPE_LEN + 32,
        wire.len() - 65,
        wire.len() - 1,
    ] {
        let mut tampered = wire.clone();
        tampered[at] ^= 1;
        assert!(session.authenticate(scope, &tampered).is_err());
    }
    for end in [0, 4, 5, 100, wire.len() - 1] {
        assert!(session.authenticate(scope, &wire[..end]).is_err());
    }
    let mut extended = wire.clone();
    extended.push(0);
    assert!(session.authenticate(scope, &extended).is_err());
    let mut epoch = wire.clone();
    epoch[5 + SCOPE_LEN + 32] = 2;
    resign(&a, &mut epoch);
    assert!(session.authenticate(scope, &epoch).is_err());
    let mut bad_chain = wire.clone();
    let sequence = 5 + SCOPE_LEN + 32 + 8 + 48;
    bad_chain[sequence] = 2;
    resign(&a, &mut bad_chain);
    assert!(session.authenticate(scope, &bad_chain).is_err());
    assert!(
        session
            .seal(
                scope,
                Causal {
                    sequence: 0,
                    ..causal()
                },
                b"bad"
            )
            .is_err()
    );
    assert!(
        session
            .seal(
                scope,
                Causal {
                    sequence: 2,
                    previous: Some([0; 32]),
                    parents: &[[0; 32]]
                },
                b"bad"
            )
            .is_err()
    );
    assert!(
        session
            .seal(scope, causal(), &vec![0; MAX_BODY + 1])
            .is_err()
    );
    let maximum = session.seal(scope, causal(), &vec![7; MAX_BODY]).unwrap();
    assert_eq!(
        session
            .authenticate(scope, &maximum)
            .unwrap()
            .payload()
            .len(),
        MAX_BODY
    );
}

#[test]
fn rotation_preserves_old_read_evidence_and_demotion_does_not_grant_writes() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let session = Session::open(&a).unwrap();
    let scope = session.scope().unwrap();
    let old = session.seal(scope, causal(), b"old-epoch").unwrap();
    drop(session);
    lab.rotate(&[Role::Writer, Role::Reader]);
    let session = Session::open(&a).unwrap();
    assert_eq!(session.epoch(), 2);
    assert_eq!(
        session.authenticate(scope, &old).unwrap().payload(),
        b"old-epoch"
    );
    let new = session.seal(scope, causal(), b"new-epoch").unwrap();
    assert_eq!(session.authenticate(scope, &new).unwrap().epoch(), 2);
    drop(session);
    lab.rotate(&[Role::Reader, Role::Reader]);
    let session = Session::open(&a).unwrap();
    assert!(
        session.authenticate(scope, &old).is_ok(),
        "past authorship is evidence, not a current write grant"
    );
    assert!(session.seal(scope, causal(), b"denied").is_err());
}

fn envelope_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    let a = lab.bootstrap(0);
    let b = lab.bootstrap(1);
    let mut session = Session::open(&a).unwrap();
    let mut scope = session.scope().unwrap();
    let mut wire = session.seal(scope, causal(), b"account").unwrap();
    let scenario = match action {
        "Valid" => 1,
        "UnknownTable" => {
            scope = session.scope().unwrap();
            2
        }
        "ForgedReader" => {
            let at = 5 + SCOPE_LEN + 32 + 8;
            wire[at..at + 48].copy_from_slice(&b.request().unwrap().binding().actor());
            resign(&b, &mut wire);
            3
        }
        "WrongEpoch" => {
            wire[5 + SCOPE_LEN + 32] = 2;
            resign(&a, &mut wire);
            4
        }
        "Tampered" => {
            let end = wire.len() - 65;
            wire[end] ^= 1;
            5
        }
        "Malformed" => {
            wire[5 + SCOPE_LEN + 32 + 8 + 48] = 2;
            resign(&a, &mut wire);
            6
        }
        "ReaderWrites" => 7,
        "OldEpoch" => {
            drop(session);
            lab.rotate(&[Role::Writer, Role::Reader]);
            session = Session::open(&a).unwrap();
            8
        }
        _ => panic!("unknown envelope action"),
    };
    let accepted = if action == "ReaderWrites" {
        Session::open(&b)
            .unwrap()
            .seal(scope, causal(), b"reader write")
            .is_ok()
    } else {
        session.authenticate(scope, &wire).is_ok()
    };
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("accepted", i64::from(accepted));
    next
}

#[test]
fn signed_envelope_conforms_and_reader_forgery_mutant_is_detected() {
    let model = aterm_models_db::table_envelope_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = envelope_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let mut forged = initial.clone();
    forged.insert("scenario", 3);
    forged.insert("accepted", 1);
    assert!(!model.successors("ForgedReader", &initial).contains(&forged));
    assert!(!model.check_invariant("ExactWriterEvidenceOnly", &forged));
}
