// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{binding::Pending, node::Edit};
use std::os::unix::fs::{PermissionsExt, symlink};

const ROOT: [u8; 32] = [11; 32];
const NETWORK: [u8; 16] = [7; 16];

fn fixture() -> (aterm_tempfile::TempDir, Paths) {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::machine_identity(&paths).unwrap();
    (home, paths)
}

fn descriptor(identity: &Identity, network: [u8; 16], seed: &[u8; 32], time: u64) -> Descriptor {
    Descriptor::from_genesis(
        Node::test_seal(
            seed,
            network,
            Edit::Create {
                hosts: &["m1".into()],
                payload: b"test bootstrap",
            },
            &[identity.recipient()],
            time,
            &[],
        )
        .unwrap(),
    )
    .unwrap()
}

fn snapshot(identity: &Identity, descriptor: &Descriptor) -> Snapshot {
    Snapshot {
        vault: "test".into(),
        signer: descriptor.trust().authority,
        followed: vec![],
        sequence: 1,
        digest: [17; 32],
        recipient: identity.recipient(),
        control: descriptor.bytes().into(),
    }
}

fn request(device: &Device) -> Vec<u8> {
    device
        .request(NETWORK, crate::envelope::signer_public(&ROOT).unwrap())
        .unwrap()
        .bytes()
        .to_vec()
}

fn custody_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (_home, paths) = fixture();
    let dir = paths.dir.join("commons");
    let expected = if action == "Fresh" {
        None
    } else {
        Some(request(&Device::open(&paths).unwrap().unwrap()))
    };
    let scenario = match action {
        "Fresh" => 1,
        "Restart" => 2,
        "UnclaimedKey" => {
            std::fs::remove_file(dir.join("device.claim")).unwrap();
            3
        }
        "MissingKey" => {
            std::fs::remove_file(dir.join("device.age")).unwrap();
            4
        }
        "DamagedKey" => {
            fsx::write_atomic(&dir.join("device.age"), b"damaged", 0o600).unwrap();
            5
        }
        "WrongRecipient" => {
            let replacement = Identity::from_secret_bytes([37; 32]);
            fsx::write_atomic(
                &paths.dir.join("identity"),
                replacement.to_bech32().as_slice(),
                0o600,
            )
            .unwrap();
            6
        }
        "ChangedClaim" => {
            fsx::write_atomic(&dir.join("device.claim"), &[3; CLAIM_LEN], 0o600).unwrap();
            7
        }
        "MissingIdentity" => {
            std::fs::remove_file(paths.dir.join("identity")).unwrap();
            8
        }
        _ => panic!("unknown custody action"),
    };
    let opened = Device::open(&paths);
    let usable = match opened {
        Ok(Some(device)) => {
            let actual = request(&device);
            assert!(
                expected.as_ref().is_none_or(|expected| *expected == actual),
                "writer changed"
            );
            assert_eq!(
                std::fs::read(dir.join("device.claim")).unwrap(),
                device.claim()
            );
            let raw = std::fs::read(dir.join("device.age")).unwrap();
            assert!(
                !raw.windows(32).any(|part| part == device.seed.as_slice()),
                "plaintext seed"
            );
            true
        }
        Ok(None) | Err(_) => false,
    };
    if action == "MissingKey" {
        assert!(!dir.join("device.age").exists());
    }
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("usable", i64::from(usable));
    next
}

#[test]
fn actual_custody_conforms_and_regeneration_is_detected() {
    let model = aterm_models_db::device_custody_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = custody_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let mut unsafe_next = custody_transition(&initial, "MissingKey");
    unsafe_next.insert("usable", 1);
    assert!(
        !model
            .successors("MissingKey", &initial)
            .contains(&unsafe_next)
    );
    assert!(!model.check_invariant("InvalidCustodyWithholds", &unsafe_next));
}

fn history_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let (home, paths) = fixture();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let initial = descriptor(&identity, NETWORK, &ROOT, 1);
    let mut observation = snapshot(&identity, &initial);
    let prior = (!matches!(
        action,
        "FirstBoot" | "PreparingResume" | "ForeignPreparing" | "WrongIdentity" | "UnclaimedPartial"
    ))
    .then(|| Bootstrap::from_snapshot(&paths, &observation).unwrap());
    let mut held = None;
    let scenario = match action {
        "FirstBoot" => 1,
        "Reopen" => 2,
        "VaultHandoff" => {
            observation.signer = crate::envelope::signer_public(&[12; 32]).unwrap();
            observation.followed.push(initial.trust().authority);
            3
        }
        "LostHistory" => {
            std::fs::rename(
                prior.as_ref().unwrap().journal_dir(),
                home.path().join("preserved-history"),
            )
            .unwrap();
            4
        }
        "ChangedTrust" => {
            observation = snapshot(&identity, &descriptor(&identity, [8; 16], &ROOT, 1));
            5
        }
        "MissingOuterPin" => {
            std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
            6
        }
        "MissingPinAndHistory" => {
            std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
            std::fs::rename(
                prior.as_ref().unwrap().journal_dir(),
                home.path().join("preserved-history"),
            )
            .unwrap();
            7
        }
        "MissingCommonsDirectory" => {
            std::fs::rename(
                paths.dir.join("commons"),
                home.path().join("preserved-commons"),
            )
            .unwrap();
            8
        }
        "ChangedTrustMissingPin" => {
            std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
            observation = snapshot(&identity, &descriptor(&identity, [8; 16], &ROOT, 1));
            9
        }
        "CorruptHistoryClaim" => {
            fsx::write_atomic(&paths.dir.join("commons.claim"), b"damaged", 0o600).unwrap();
            10
        }
        "LegacyBackfill" => {
            std::fs::remove_file(paths.dir.join("commons.claim")).unwrap();
            11
        }
        "MissingOuterClaims" => {
            std::fs::remove_file(paths.dir.join("commons.claim")).unwrap();
            std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
            12
        }
        "BusyBootstrap" => {
            held =
                Some(CustodyLock::acquire_commons_custody(&paths, "commons.lock", false).unwrap());
            13
        }
        "PreparingResume" => {
            Device::open(&paths).unwrap().unwrap();
            fsx::write_atomic(
                &paths.dir.join("commons.claim"),
                &preparing_bytes(initial.trust()),
                0o600,
            )
            .unwrap();
            fsx::ensure_private_dir(&paths.dir.join("commons/control")).unwrap();
            14
        }
        "ForeignPreparing" => {
            Device::open(&paths).unwrap().unwrap();
            let foreign = descriptor(&identity, [8; 16], &ROOT, 1);
            fsx::write_atomic(
                &paths.dir.join("commons.claim"),
                &preparing_bytes(foreign.trust()),
                0o600,
            )
            .unwrap();
            15
        }
        "LegacyDamagedAnchor" => {
            std::fs::remove_file(paths.dir.join("commons.claim")).unwrap();
            std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
            std::fs::rename(
                paths.dir.join("commons/control/control.anchor"),
                home.path().join("saved-anchor"),
            )
            .unwrap();
            16
        }
        "OrphanTables" | "OrphanHistory" => {
            std::fs::remove_file(paths.dir.join("commons.claim")).unwrap();
            std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
            std::fs::rename(
                paths.dir.join("commons/control"),
                home.path().join("saved-control"),
            )
            .unwrap();
            let name = if action == "OrphanTables" {
                "tables"
            } else {
                "history"
            };
            fsx::ensure_private_dir(&paths.dir.join("commons").join(name)).unwrap();
            if action == "OrphanTables" { 17 } else { 21 }
        }
        "WrongIdentity" => {
            observation.recipient = Identity::from_secret_bytes([37; 32]).recipient();
            18
        }
        "UnclaimedPartial" => {
            Device::open(&paths).unwrap().unwrap();
            fsx::ensure_private_dir(&paths.dir.join("commons/control")).unwrap();
            19
        }
        "PreparingPinLostLog" => {
            fsx::write_atomic(
                &paths.dir.join("commons.claim"),
                &preparing_bytes(initial.trust()),
                0o600,
            )
            .unwrap();
            std::fs::rename(
                paths.dir.join("commons/control"),
                home.path().join("saved-control"),
            )
            .unwrap();
            20
        }
        _ => panic!("unknown bootstrap action"),
    };
    let opened = match Bootstrap::from_snapshot(&paths, &observation) {
        Ok(opened) => {
            assert_eq!(opened.trust(), initial.trust());
            assert_eq!(
                fsx::read_private(&paths.dir.join("commons.claim"), PIN_LEN)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                claim_bytes(initial.trust()),
            );
            assert_eq!(
                fsx::read_private(&paths.dir.join("commons/default.pin"), PIN_LEN)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                pin_bytes(initial.trust())
            );
            true
        }
        Err(_) => false,
    };
    drop(held);
    if matches!(
        action,
        "LostHistory"
            | "MissingPinAndHistory"
            | "OrphanTables"
            | "OrphanHistory"
            | "PreparingPinLostLog"
    ) {
        assert!(!paths.dir.join("commons/control").exists());
    }
    if matches!(action, "MissingCommonsDirectory" | "WrongIdentity") {
        assert!(!paths.dir.join("commons").exists());
    }
    if matches!(
        action,
        "MissingPinAndHistory" | "ChangedTrustMissingPin" | "OrphanTables" | "OrphanHistory"
    ) {
        assert!(!paths.dir.join("commons/default.pin").exists());
    }
    if matches!(action, "LegacyDamagedAnchor" | "UnclaimedPartial") {
        assert!(!paths.dir.join("commons/control/control.anchor").exists());
    }
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("opened", i64::from(opened));
    next
}

#[test]
fn actual_bootstrap_conforms_and_silent_history_reset_is_detected() {
    let model = aterm_models_db::bootstrap_history_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = history_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let mut reset = history_transition(&initial, "LostHistory");
    reset.insert("opened", 1);
    assert!(!model.successors("LostHistory", &initial).contains(&reset));
    assert!(!model.check_invariant("EstablishedHistoryNeverResets", &reset));
    let mut reset = history_transition(&initial, "MissingPinAndHistory");
    reset.insert("opened", 1);
    assert!(
        !model
            .successors("MissingPinAndHistory", &initial)
            .contains(&reset)
    );
    assert!(!model.check_invariant("EstablishedHistoryNeverResets", &reset));
}

#[test]
fn concurrent_creators_reuse_one_encrypted_device_and_answer_joint_proof() {
    let (_home, paths) = fixture();
    let requests = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| request(&Device::open(&paths).unwrap().unwrap())))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(requests.iter().all(|r| r == &requests[0]));
    let device = Device::open(&paths).unwrap().unwrap();
    let request = device
        .request(NETWORK, crate::envelope::signer_public(&ROOT).unwrap())
        .unwrap();
    let (mut pending, challenge) =
        Pending::issue(&request, NETWORK, *request.binding().authority()).unwrap();
    assert!(
        pending
            .verify(&device.answer(&request, &challenge).unwrap())
            .is_ok()
    );
}

#[test]
fn unsafe_files_and_lost_control_identity_are_never_replaced() {
    for variant in 0..5 {
        let (_home, paths) = fixture();
        let device = Device::open(&paths).unwrap().unwrap();
        let dir = paths.dir.join("commons");
        match variant {
            0 => std::fs::set_permissions(
                dir.join("device.age"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap(),
            1 => {
                std::fs::rename(dir.join("device.age"), dir.join("saved.age")).unwrap();
                symlink("saved.age", dir.join("device.age")).unwrap();
            }
            2 => {
                fsx::write_atomic(&dir.join("device.age"), &[0; MAX_DEVICE + 1], 0o600).unwrap();
            }
            3 => {
                std::fs::remove_file(dir.join("device.age")).unwrap();
                std::fs::remove_file(dir.join("device.claim")).unwrap();
                std::fs::create_dir(dir.join("control")).unwrap();
            }
            _ => {
                let mut plain = device.encode().as_slice().to_vec();
                plain.push(0);
                let encrypted = age::encrypt(&[device.identity.recipient()], &plain).unwrap();
                fsx::write_atomic(&dir.join("device.age"), &encrypted, 0o600).unwrap();
            }
        }
        assert!(Device::open(&paths).is_err(), "variant {variant}");
    }
}

#[test]
fn bootstrap_restarts_offline_and_refuses_every_pin_change() {
    let (_home, paths) = fixture();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let original = descriptor(&identity, NETWORK, &ROOT, 1);
    let observation = snapshot(&identity, &original);
    let first = Bootstrap::from_snapshot(&paths, &observation).unwrap();
    assert!(
        first.witness().unwrap().is_none(),
        "a descriptor alone cannot enroll a writer"
    );
    let expected = first.request().unwrap().bytes().to_vec();
    let restarted = Bootstrap::from_snapshot(&paths, &observation).unwrap();
    assert_eq!(restarted.request().unwrap().bytes(), expected);
    let mut handoff = snapshot(&identity, &original);
    handoff.signer = crate::envelope::signer_public(&[12; 32]).unwrap();
    handoff.followed.push(observation.signer);
    assert_eq!(
        Bootstrap::from_snapshot(&paths, &handoff).unwrap().trust(),
        first.trust()
    );
    for changed in [
        descriptor(&identity, [8; 16], &ROOT, 1),
        descriptor(&identity, NETWORK, &[12; 32], 1),
        descriptor(&identity, NETWORK, &ROOT, 2),
    ] {
        assert!(Bootstrap::from_snapshot(&paths, &snapshot(&identity, &changed)).is_err());
    }
    // Even loss of the outer pin cannot replace the journal's independent pin.
    std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
    let changed = descriptor(&identity, [8; 16], &ROOT, 1);
    assert!(Bootstrap::from_snapshot(&paths, &snapshot(&identity, &changed)).is_err());
    assert!(!paths.dir.join("commons/default.pin").exists());
    assert_eq!(
        Bootstrap::from_snapshot(&paths, &observation)
            .unwrap()
            .trust(),
        first.trust()
    );
}

#[test]
fn loss_of_established_journal_state_cannot_restart_from_genesis() {
    for missing in [
        "directory",
        "control.anchor",
        "control.astream",
        "control.checkpoint",
    ] {
        let (home, paths) = fixture();
        let identity = client::read_identity(&paths).unwrap().unwrap();
        let initial = descriptor(&identity, NETWORK, &ROOT, 1);
        let observation = snapshot(&identity, &initial);
        let opened = Bootstrap::from_snapshot(&paths, &observation).unwrap();
        let dir = opened.journal_dir();
        let lost = if missing == "directory" {
            dir.to_path_buf()
        } else {
            dir.join(missing)
        };
        std::fs::rename(lost, home.path().join("preserved-lost-state")).unwrap();
        assert!(
            Bootstrap::from_snapshot(&paths, &observation).is_err(),
            "{missing}"
        );
        assert!(opened.witness().is_err(), "{missing}");
        assert!(opened.access().is_err(), "{missing}");
    }
}

#[test]
fn bootstrap_lock_outlives_control_loss_and_rechecks_retained_history() {
    let (home, paths) = fixture();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let observation = snapshot(&identity, &descriptor(&identity, NETWORK, &ROOT, 1));
    let opened = Bootstrap::from_snapshot(&paths, &observation).unwrap();
    let lock = CustodyLock::acquire_commons_custody(&paths, "commons.lock", false).unwrap();
    let saved = home.path().join("preserved-control");
    std::fs::rename(opened.journal_dir(), &saved).unwrap();
    std::fs::remove_file(paths.dir.join("commons/default.pin")).unwrap();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let error = Bootstrap::from_snapshot(&paths, &observation)
                    .err()
                    .unwrap();
                assert!(error.to_string().contains("bootstrap is active"), "{error}");
            })
            .join()
            .unwrap();
    });
    assert!(!opened.journal_dir().exists());
    drop(lock);
    assert!(Bootstrap::from_snapshot(&paths, &observation).is_err());
    assert!(!opened.journal_dir().exists());
    std::fs::rename(saved, opened.journal_dir()).unwrap();
    assert_eq!(
        Bootstrap::from_snapshot(&paths, &observation)
            .unwrap()
            .trust(),
        opened.trust()
    );
}

#[test]
fn retained_history_prevents_replacement_device_after_local_directory_loss() {
    for whole_directory in [false, true] {
        let (home, paths) = fixture();
        let identity = client::read_identity(&paths).unwrap().unwrap();
        let observation = snapshot(&identity, &descriptor(&identity, NETWORK, &ROOT, 1));
        Bootstrap::from_snapshot(&paths, &observation).unwrap();
        let dir = paths.dir.join("commons");
        if whole_directory {
            std::fs::rename(&dir, home.path().join("preserved-commons")).unwrap();
        } else {
            for file in ["device.age", "device.claim", "control"] {
                std::fs::rename(dir.join(file), home.path().join(file)).unwrap();
            }
        }
        assert!(Device::open(&paths).is_err());
        assert!(!dir.join("device.age").exists());
        assert!(!dir.join("device.claim").exists());
        assert!(!dir.join("control").exists());
        if whole_directory {
            assert!(!dir.exists());
        }
    }
}

#[test]
fn legacy_pin_or_application_history_prevents_replacement_device() {
    for name in ["default.pin", "history", "tables"] {
        let (home, paths) = fixture();
        let device = Device::open(&paths).unwrap().unwrap();
        let dir = paths.dir.join("commons");
        std::fs::rename(dir.join("device.age"), home.path().join("saved-device.age")).unwrap();
        std::fs::rename(
            dir.join("device.claim"),
            home.path().join("saved-device.claim"),
        )
        .unwrap();
        if name == "default.pin" {
            let initial = descriptor(&device.identity, NETWORK, &ROOT, 1);
            fsx::write_new(&dir.join(name), &pin_bytes(initial.trust()), 0o600).unwrap();
        } else {
            fsx::ensure_private_dir(&dir.join(name)).unwrap();
        }
        assert!(Device::open(&paths).is_err(), "{name}");
        assert!(!dir.join("device.age").exists());
        assert!(!dir.join("device.claim").exists());
    }
}

#[test]
fn legacy_migration_requires_intact_history_before_retaining_a_claim() {
    let (home, paths) = fixture();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let observation = snapshot(&identity, &descriptor(&identity, NETWORK, &ROOT, 1));
    let opened = Bootstrap::from_snapshot(&paths, &observation).unwrap();
    std::fs::remove_file(paths.dir.join("commons.claim")).unwrap();
    std::fs::rename(opened.journal_dir(), home.path().join("preserved-control")).unwrap();
    assert!(Bootstrap::from_snapshot(&paths, &observation).is_err());
    assert!(!paths.dir.join("commons.claim").exists());
    assert!(!opened.journal_dir().exists());
}

#[test]
fn bootstrap_refuses_unsafe_outer_lock_and_history_claim() {
    for (file, symlinked) in [
        ("commons.lock", false),
        ("commons.lock", true),
        ("commons.claim", false),
        ("commons.claim", true),
        ("commons-device.lock", false),
        ("commons-device.lock", true),
    ] {
        let (_home, paths) = fixture();
        let identity = client::read_identity(&paths).unwrap().unwrap();
        let observation = snapshot(&identity, &descriptor(&identity, NETWORK, &ROOT, 1));
        Bootstrap::from_snapshot(&paths, &observation).unwrap();
        let path = paths.dir.join(file);
        if symlinked {
            std::fs::rename(&path, paths.dir.join("saved-file")).unwrap();
            symlink("saved-file", &path).unwrap();
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(
            Bootstrap::from_snapshot(&paths, &observation).is_err(),
            "{file}"
        );
    }
}

#[test]
fn unauthenticated_absent_and_invalid_bootstrap_do_not_create_a_device() {
    let (_home, paths) = fixture();
    assert!(Bootstrap::cached(&paths).unwrap().is_none());
    assert!(!paths.dir.join("commons").exists());
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let valid = descriptor(&identity, NETWORK, &ROOT, 1);
    let wire = valid.bytes();
    for bytes in [
        vec![],
        wire[..wire.len() - 1].to_vec(),
        [wire.as_slice(), &[0]].concat(),
        vec![0; MAX_DESCRIPTOR + 1],
    ] {
        assert!(Descriptor::decode(&bytes).is_err());
    }
    let mut invalid = snapshot(&identity, &valid);
    invalid.control = b"invalid".as_slice().into();
    assert!(Bootstrap::from_snapshot(&paths, &invalid).is_err());
    assert!(!paths.dir.join("commons").exists());
}

#[test]
fn a_new_member_can_repair_before_incomplete_history_grants_data_access() {
    let (home, paths) = fixture();
    let device = Device::open(&paths).unwrap().unwrap();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let first = descriptor(&identity, NETWORK, &ROOT, 1);
    let request = device.request(NETWORK, first.trust().authority).unwrap();
    let middle = Node::test_seal(
        &ROOT,
        NETWORK,
        Edit::Update {
            parent: first.genesis.id(),
            hosts: &["m1".into()],
            payload: b"middle",
        },
        &[identity.recipient()],
        2,
        &[],
    )
    .unwrap();
    let approved = Node::test_seal(
        &ROOT,
        NETWORK,
        Edit::Update {
            parent: middle.id(),
            hosts: &["m1".into()],
            payload: b"approved",
        },
        &[identity.recipient()],
        3,
        &[Fact::Bind {
            binding: request.binding().clone(),
        }],
    )
    .unwrap();
    let mut source = Journal::open(&home.path().join("source-control"), first.trust()).unwrap();
    for node in [&first.genesis, &middle, &approved] {
        source.ingest(node.bytes()).unwrap();
    }
    let published = Descriptor::from_journal(&source).unwrap();
    assert_eq!(published.repair.len(), 1);
    let joined = Bootstrap::from_snapshot(&paths, &snapshot(&identity, &published)).unwrap();
    assert_eq!(joined.witness().unwrap().unwrap().id(), approved.id());
    assert_eq!(
        Journal::open(joined.journal_dir(), joined.trust())
            .unwrap()
            .state(),
        State::Pending
    );
    assert!(joined.access().unwrap().is_none());
    // Explicitly corrupted, duplicated and noncanonical witness lists fail closed.
    let mut damaged = published.bytes();
    *damaged.last_mut().unwrap() ^= 1;
    assert!(Descriptor::decode(&damaged).is_err());
    let mut duplicated = published.clone();
    duplicated.repair.push(approved);
    assert!(Descriptor::decode(&duplicated.bytes()).is_err());
    assert!(
        Descriptor::from_journal(&Journal::open(joined.journal_dir(), joined.trust()).unwrap())
            .is_err()
    );
}

#[test]
fn a_roster_capsule_for_the_same_age_recipient_cannot_activate_another_writer() {
    use crate::{
        commons::{
            membership::Eligibility,
            roster::{self, Enrollment, Role},
        },
        text::{Audience, Document},
    };
    let (_home, paths) = fixture();
    let device = Device::open(&paths).unwrap().unwrap();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let authority = crate::envelope::signer_public(&ROOT).unwrap();
    let request = device.request(NETWORK, authority).unwrap();
    let (mut pending, challenge) = Pending::issue(&request, NETWORK, authority).unwrap();
    let possession = pending
        .verify(&device.answer(&request, &challenge).unwrap())
        .unwrap();
    let mut document = Document::new_vault("m1", &identity.recipient().to_bech32());
    document
        .set_entry(
            &Audience::Hosts(vec!["m1".into()]),
            crate::commons::ENTRY,
            b"bootstrap",
        )
        .unwrap();
    let genesis = roster::create(
        &ROOT,
        &Eligibility::from_document(&document),
        NETWORK,
        vec![Enrollment::new("m1", possession, Role::Writer)],
    )
    .unwrap();
    let descriptor = Descriptor::from_genesis(genesis).unwrap();
    let joined = Bootstrap::from_snapshot(&paths, &snapshot(&identity, &descriptor)).unwrap();
    assert!(joined.access().unwrap().is_some());
    let other_writer = Bootstrap {
        descriptor,
        device: Device::generate(identity).unwrap(),
        journal_dir: joined.journal_dir.clone(),
    };
    assert!(other_writer.witness().unwrap().is_none());
    assert!(other_writer.access().is_err());
}

#[test]
fn history_cache_crash_leftovers_preserve_full_capacity_and_allow_exact_overwrites() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let first = history_name(&[1; 32]);
    let second = history_name(&[2; 32]);
    let third = history_name(&[3; 32]);
    std::fs::write(dir.path().join(&first), []).unwrap();
    std::fs::write(dir.path().join(&second), []).unwrap();
    for n in 0..32 {
        std::fs::write(dir.path().join(format!(".{first}.tmp.100.{n}")), []).unwrap();
    }
    assert!(history_capacity(dir.path(), &first, 2).is_ok());
    assert!(history_capacity(dir.path(), &third, 2).is_err());
    std::fs::remove_file(dir.path().join(&second)).unwrap();
    assert!(history_capacity(dir.path(), &third, 2).is_ok());
    std::fs::write(dir.path().join("unknown"), []).unwrap();
    assert!(history_capacity(dir.path(), &first, 2).is_err());
}
