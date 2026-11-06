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

#[test]
fn local_denial_observation_finishes_while_bootstrap_holds_device_custody() {
    use crate::{swarm_roster::SwarmRoster, text::Audience};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::create(
        &paths,
        home.path().join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let roster = SwarmRoster {
        members: Vec::new(),
        deny_labels: Vec::new(),
        deny_nodes: vec![aterm_codec::hex::encode_lower(&[99; 32])],
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    let device = Device::open_existing(&paths).unwrap();
    let request = device.request(NETWORK, [2; 32]).unwrap();
    let encrypted_before = fsx::read_private(&paths.dir.join("commons/device.age"), MAX_DEVICE)
        .unwrap()
        .unwrap();
    let claim_before = fsx::read_private(&paths.dir.join("commons/device.claim"), CLAIM_LEN)
        .unwrap()
        .unwrap();
    let held = CustodyLock::acquire_commons_custody(&paths, "commons-device.lock", true).unwrap();
    std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::channel();
        let paths = &paths;
        let worker = scope.spawn(move || {
            send.send(client::local_recipient_denied_in_roster(paths))
                .unwrap();
        });
        let observed = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        drop(held);
        worker.join().unwrap();
        assert!(!observed.unwrap());
    });
    for denied in [
        request.binding().writer().as_slice(),
        request.binding().incarnation().as_slice(),
        request.binding().machine().as_slice(),
    ] {
        let mut roster = roster.clone();
        roster.deny_nodes = vec![aterm_codec::hex::encode_lower(denied)];
        assert!(
            Device::denied_in_roster(
                &paths,
                client::read_identity(&paths).unwrap().unwrap(),
                &roster,
            )
            .unwrap()
        );
    }
    assert_eq!(
        fsx::read_private(&paths.dir.join("commons/device.age"), MAX_DEVICE)
            .unwrap()
            .unwrap()
            .as_slice(),
        encrypted_before.as_slice()
    );
    assert_eq!(
        fsx::read_private(&paths.dir.join("commons/device.claim"), CLAIM_LEN)
            .unwrap()
            .unwrap()
            .as_slice(),
        claim_before.as_slice()
    );
    let identity = || client::read_identity(&paths).unwrap().unwrap();
    let encrypted_path = paths.dir.join("commons/device.age");
    let mut corrupt = encrypted_before.as_slice().to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    fsx::write_atomic(&encrypted_path, &corrupt, 0o600).unwrap();
    assert!(Device::denied_in_roster(&paths, identity(), &roster).is_err());
    fsx::write_atomic(&encrypted_path, encrypted_before.as_slice(), 0o600).unwrap();
    let claim_path = paths.dir.join("commons/device.claim");
    let mut corrupt = claim_before.as_slice().to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    fsx::write_atomic(&claim_path, &corrupt, 0o600).unwrap();
    assert!(Device::denied_in_roster(&paths, identity(), &roster).is_err());
    fsx::write_atomic(&claim_path, claim_before.as_slice(), 0o600).unwrap();
    assert!(
        Device::denied_in_roster(&paths, Identity::from_secret_bytes([99; 32]), &roster,).is_err()
    );
    assert!(!Device::denied_in_roster(&paths, identity(), &roster).unwrap());
}

#[test]
fn existing_binding_observation_never_takes_or_repairs_device_custody() {
    let (_home, paths) = fixture();
    let device = Device::open(&paths).unwrap().unwrap();
    let expected = device.request([1; 16], [2; 32]).unwrap().binding().clone();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let _held = aterm_uds::private_file::PrivateLock::take(
        &paths.dir,
        "commons-device.lock",
        std::time::Duration::from_secs(60),
        "fixture custody",
    )
    .unwrap();
    assert_eq!(
        Device::existing_binding_for_observation(&paths, &identity).unwrap(),
        Some(expected.clone())
    );
    let claim = paths.dir.join("commons/device.claim");
    std::fs::remove_file(&claim).unwrap();
    assert_eq!(
        Device::existing_binding_for_observation(&paths, &identity).unwrap(),
        Some(expected.clone()),
        "authenticated device bytes still identify a denial without repairing custody"
    );
    assert!(!claim.exists(), "a query must not recreate a claim");
    let matching = device.claim();
    assert_eq!(
        Device::observe_binding(&paths, &identity, || {
            fsx::write_new(&claim, &matching, 0o600).unwrap();
        })
        .unwrap(),
        Some(expected),
        "matching claim publication does not change the observed binding"
    );
    fsx::write_atomic(&claim, b"invalid claim", 0o600).unwrap();
    assert!(Device::existing_binding_for_observation(&paths, &identity).is_err());
}

fn switch_source(paths: &Paths, target: &Path) {
    let path = paths.dir.join("source.toml");
    let text = fsx::read_private(&path, 4 << 20).unwrap().unwrap();
    let mut config: aterm_toml::Value = std::str::from_utf8(text.as_slice())
        .unwrap()
        .parse()
        .unwrap();
    config.as_table_mut().unwrap().insert(
        "path".into(),
        aterm_toml::Value::String(target.display().to_string()),
    );
    fsx::write_atomic(
        &path,
        aterm_toml::to_string(&config).unwrap().as_bytes(),
        0o600,
    )
    .unwrap();
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
        source_config: Box::new(client::Config {
            source: crate::transport::Source::Path(PathBuf::from("test.vault")),
            mirrors: Vec::new(),
            name: "test".into(),
            signer: Some(descriptor.trust().authority),
            followed: vec![],
            host: "m1".into(),
        }),
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
fn custody_wait_rejects_replacement_and_an_expired_deadline_stays_busy() {
    for replace_inode in [false, true] {
        let (_home, paths) = fixture();
        let mut owner =
            Some(CustodyLock::acquire_commons_custody(&paths, "commons.lock", false).unwrap());
        let refused = CustodyLock::acquire_until(
            &paths,
            "commons.lock",
            false,
            Some(std::time::Instant::now()),
        )
        .err()
        .unwrap();
        assert_eq!(refused.kind(), crate::ErrorKind::CustodyBusy);
        let path = paths.dir.join("commons.lock");
        let mut waited = 0;
        let refused = CustodyLock::acquire_until_with(
            &paths,
            "commons.lock",
            false,
            Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
            || {
                waited += 1;
                if replace_inode {
                    std::fs::rename(&path, paths.dir.join("preserved-commons.lock")).unwrap();
                    fsx::write_new(&path, &[], 0o600).unwrap();
                } else {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                }
                drop(owner.take());
            },
        )
        .err()
        .unwrap();
        assert_eq!(waited, 1);
        assert_eq!(refused.kind(), crate::ErrorKind::Other);
        assert!(
            refused.to_string().contains(if replace_inode {
                "lock changed"
            } else {
                "not private"
            }),
            "{refused}"
        );
        // Neither a changed inode nor changed privacy leaves the old lock held.
        let original = if replace_inode {
            "preserved-commons.lock"
        } else {
            "commons.lock"
        };
        std::fs::set_permissions(
            paths.dir.join(original),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let _original = CustodyLock::acquire_commons_custody(&paths, original, false).unwrap();
    }
}

#[test]
fn bootstrap_wait_reobserves_the_source_before_reusing_custody() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::create(
        &paths,
        home.path().join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let observation = super::super::cached(&paths).unwrap().unwrap();
    let mut owner =
        Some(CustodyLock::acquire_commons_custody(&paths, "commons.lock", false).unwrap());
    let mut waited = 0;
    let refused = Bootstrap::open_snapshot_with(
        &paths,
        &observation,
        false,
        Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
        || {
            waited += 1;
            switch_source(&paths, &home.path().join("other.vault"));
            drop(owner.take());
        },
    )
    .err()
    .unwrap();
    assert_eq!(
        waited, 1,
        "the source changed during actual lock contention"
    );
    assert!(refused.to_string().contains("source-changed"), "{refused}");
}

#[test]
fn bootstrap_revalidates_source_after_device_and_control_waits() {
    use std::{cell::Cell, rc::Rc};

    for device_lock in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        client::create(
            &paths,
            home.path().join("team.vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let (dir, name) = if device_lock {
            (paths.dir.clone(), "commons-device.lock")
        } else {
            (paths.dir.join("commons/control"), "control.lock")
        };
        let (file, _) = aterm_uds::private_file::open_private(&dir, name).unwrap();
        aterm_uds::keep_out_of_children(&file).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the test holder is a CustodyLock guard which unlocks before close"
        )]
        file.try_lock().unwrap();
        let mut owner = Some(CustodyLock(file));
        let changed_paths = paths.clone();
        let target = home.path().join("other.vault");
        let waited = Rc::new(Cell::new(0));
        let observed = waited.clone();
        let error = aterm_uds::file_lock::with_lock_wait_observer(
            move |_| {
                observed.set(observed.get() + 1);
                switch_source(&changed_paths, &target);
                drop(owner.take());
            },
            || {
                Bootstrap::required_with_journal_until(
                    &paths,
                    std::time::Instant::now() + std::time::Duration::from_secs(60),
                )
                .err()
                .expect("a source switch during the last custody wait must refuse")
            },
        );
        assert_eq!(waited.get(), 1);
        assert!(error.to_string().contains("source-changed"), "{error}");
        let file = aterm_uds::private_file::open_private(&dir, name).unwrap().0;
        #[expect(
            clippy::disallowed_methods,
            reason = "the test holder is a CustodyLock guard which unlocks before close"
        )]
        file.try_lock()
            .expect("refusal released the last acquired lock");
        let _released = CustodyLock(file);
    }
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
        paths: joined.paths.clone(),
        snapshot: joined.snapshot.clone(),
        immutable: joined.immutable.clone(),
    };
    assert!(other_writer.witness().unwrap().is_none());
    assert!(other_writer.access().is_err());
}

#[test]
fn history_cache_combines_source_bounds_and_preserves_overwrites_and_leftover_bounds() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let first = history_name(&[1; 32]);
    let second = history_name(&[2; 32]);
    let third = history_name(&[3; 32]);
    assert!(history_capacity(dir.path(), &first, usize::MAX, 1).is_err());
    std::fs::write(dir.path().join(&first), []).unwrap();
    std::fs::write(dir.path().join(&second), []).unwrap();
    for n in 0..32 {
        std::fs::write(dir.path().join(format!(".{first}.tmp.100.{n}")), []).unwrap();
    }
    assert!(history_capacity(dir.path(), &first, 1, 1).is_ok());
    assert!(history_capacity(dir.path(), &third, 1, 1).is_err());
    std::fs::remove_file(dir.path().join(&second)).unwrap();
    assert!(history_capacity(dir.path(), &third, 1, 1).is_ok());
    std::fs::write(dir.path().join("unknown"), []).unwrap();
    assert!(history_capacity(dir.path(), &first, 1, 1).is_err());
}

#[test]
fn control_state_is_a_claim_a_pin_or_history_and_never_a_device_key_alone() {
    let (_root, paths) = fixture();
    assert!(!retains_control(&paths).unwrap(), "nothing");
    std::fs::create_dir_all(paths.dir.join("commons")).unwrap();
    std::fs::write(paths.dir.join("commons/device.age"), b"").unwrap();
    std::fs::write(paths.dir.join("commons/device.claim"), b"").unwrap();
    assert!(!retains_control(&paths).unwrap(), "a device key alone");
    for kept in [
        "commons.claim",
        "commons/default.pin",
        "commons/control",
        "commons/history",
        "commons/tables",
    ] {
        let path = paths.dir.join(kept);
        if kept.contains('.') {
            std::fs::write(&path, b"").unwrap();
        } else {
            std::fs::create_dir(&path).unwrap();
        }
        assert!(retains_control(&paths).unwrap(), "{kept}");
        if kept.contains('.') {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::remove_dir(&path).unwrap();
        }
        assert!(!retains_control(&paths).unwrap(), "{kept} removed");
    }
    // A `commons` that is a symlink, even to nothing, is not read through as empty.
    std::fs::rename(paths.dir.join("commons"), paths.dir.join("moved")).unwrap();
    symlink(paths.dir.join("gone"), paths.dir.join("commons")).unwrap();
    assert!(retains_control(&paths).unwrap(), "a dangling commons link");
}

thread_local! {
    /// Run when this thread's local custody or vault admission finds a lock held.
    static HELD: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}
pub(in crate::commons) fn admission_held() {
    HELD.with(|held| {
        if let Some(held) = held.borrow_mut().as_mut() {
            held();
        }
    });
}

#[test]
fn an_observation_waits_out_a_short_custody_hold_and_refuses_a_long_one_as_busy() {
    let (_home, paths) = fixture();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let observation = snapshot(&identity, &descriptor(&identity, NETWORK, &ROOT, 1));
    let opened = Bootstrap::from_snapshot(&paths, &observation).unwrap();

    // Held past the wait: refused as busy, nothing observed.
    let lock = CustodyLock::acquire_commons_custody(&paths, "commons.lock", false).unwrap();
    let busy = std::thread::scope(|scope| {
        scope
            .spawn(|| Bootstrap::from_snapshot(&paths, &observation).err())
            .join()
            .unwrap()
    })
    .unwrap();
    assert_eq!(busy.kind(), crate::ErrorKind::CustodyBusy);

    // Let go while another observation waits: that one goes ahead. The hold
    // ends on the waiter's first blocked try, in its own thread, so no other
    // thread's scheduling races the wait's bound.
    let (paths, observation) = (&paths, &observation);
    let (reopened, waited) = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let waited = std::rc::Rc::new(std::cell::Cell::new(0));
                let count = std::rc::Rc::clone(&waited);
                let mut lock = Some(lock);
                HELD.with(|held| {
                    *held.borrow_mut() = Some(Box::new(move || {
                        count.set(count.get() + 1);
                        drop(lock.take());
                    }));
                });
                let reopened = Bootstrap::from_snapshot(paths, observation);
                HELD.with(|held| held.borrow_mut().take());
                (reopened, waited.get())
            })
            .join()
            .unwrap()
    });
    assert_eq!(waited, 1, "the observation found the lock held once");
    assert_eq!(reopened.unwrap().trust(), opened.trust());
}

#[test]
fn settled_bounded_bootstrap_reads_beside_a_writer_but_pair_admission_requires_the_writer() {
    use std::time::{Duration, Instant};

    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::create(
        &paths,
        home.path().join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let (held_bootstrap, writer) =
        Bootstrap::required_with_journal_until(&paths, Instant::now() + Duration::from_secs(60))
            .unwrap();
    assert!(writer.is_writer());
    let observed = Bootstrap::cached_until(&paths, Instant::now())
        .unwrap()
        .unwrap();
    assert_eq!(observed.trust(), held_bootstrap.trust());
    assert!(observed.access().unwrap().is_some());
    let busy = Bootstrap::required_with_journal_until(&paths, Instant::now())
        .err()
        .expect("a returned writer cannot be replaced by a read-only cached view");
    assert_eq!(busy.kind(), crate::ErrorKind::JournalBusy);
    drop(writer);
    let (observed, writer) =
        Bootstrap::required_with_journal_until(&paths, Instant::now() + Duration::from_secs(60))
            .unwrap();
    assert!(writer.is_writer());
    assert_eq!(writer.directory(), observed.journal_dir());
}

#[test]
fn established_observation_neither_initializes_nor_repairs_custody() {
    use std::time::{Duration, Instant};
    for missing in [
        "commons/device.age",
        "commons/device.claim",
        "commons/default.pin",
        "commons.claim",
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        client::create(
            &paths,
            home.path().join("team.vault").to_str().unwrap(),
            None,
            Some("m1"),
        )
        .unwrap();
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(
            &paths,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        drop(journal);
        let (observed, read) = Bootstrap::established_with_journal_until(
            &paths,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        assert!(!read.is_writer());
        assert_eq!(observed.trust(), bootstrap.trust());
        assert!(observed.access_in(&read).unwrap().is_some());
        drop(read);
        let path = paths.dir.join(missing);
        std::fs::remove_file(&path).unwrap();
        assert!(
            Bootstrap::established_with_journal_until(
                &paths,
                Instant::now() + Duration::from_secs(60)
            )
            .is_err(),
            "{missing}"
        );
        assert!(!path.exists(), "an observation must not recreate {missing}");
    }
}

#[test]
fn established_observation_does_not_narrow_foreign_directory_permissions() {
    use std::time::{Duration, Instant};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::create(
        &paths,
        home.path().join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let commons = paths.dir.join("commons");
    std::fs::set_permissions(&commons, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        Bootstrap::established_with_journal_until(&paths, Instant::now() + Duration::from_secs(60))
            .is_err()
    );
    assert_eq!(
        std::fs::metadata(&commons).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn established_custody_acquisition_never_recreates_a_lock_lost_after_precheck() {
    use std::time::{Duration, Instant};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::create(
        &paths,
        home.path().join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    established_files(&paths).unwrap();
    let held = CustodyLock::observe_until_with(
        &paths,
        "commons.lock",
        Some(Instant::now() + Duration::from_secs(60)),
        || {},
    )
    .unwrap();
    let busy = CustodyLock::observe_until_with(&paths, "commons.lock", Some(Instant::now()), || {})
        .err()
        .unwrap();
    assert_eq!(busy.kind(), crate::ErrorKind::CustodyBusy);
    drop(held);
    std::fs::remove_file(paths.dir.join("commons.lock")).unwrap();
    assert!(
        CustodyLock::observe_until_with(&paths, "commons.lock", Some(Instant::now()), || {})
            .is_err()
    );
    assert!(!paths.dir.join("commons.lock").exists());
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        CustodyLock::observe_until_with(&paths, "commons-device.lock", Some(Instant::now()), || {})
            .is_err()
    );
    assert_eq!(
        std::fs::metadata(&paths.dir).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn a_journal_read_as_holding_the_descriptor_is_made_durable_first() {
    let (home, paths) = fixture();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let first = descriptor(&identity, NETWORK, &ROOT, 1);
    let opened = Bootstrap::from_snapshot(&paths, &snapshot(&identity, &first)).unwrap();
    {
        let recording = aterm_durable::seam::Recording::start();
        assert!(
            holds_descriptor(opened.journal_dir(), &first)
                .unwrap()
                .is_some()
        );
        assert_eq!(recording.take(), [aterm_durable::Flush::Durable]);
    }
    {
        // The journal is read as holding the descriptor only once its
        // directory is durable.
        let _failing = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO);
        assert!(holds_descriptor(opened.journal_dir(), &first).is_err());
    }
    // A journal that lacks a node, or is missing, is not made durable here:
    // the writer that ingests the node does that.
    let device = Device::open(&paths).unwrap().unwrap();
    let request = device.request(NETWORK, first.trust().authority).unwrap();
    let witness = Node::test_seal(
        &ROOT,
        NETWORK,
        Edit::Update {
            parent: first.genesis.id(),
            hosts: &["m1".into()],
            payload: b"witness",
        },
        &[identity.recipient()],
        2,
        &[Fact::Bind {
            binding: request.binding().clone(),
        }],
    )
    .unwrap();
    let mut source = Journal::open(&home.path().join("source-control"), first.trust()).unwrap();
    for node in [&first.genesis, &witness] {
        source.ingest(node.bytes()).unwrap();
    }
    let published = Descriptor::from_journal(&source).unwrap();
    assert_eq!(published.trust(), first.trust());
    let recording = aterm_durable::seam::Recording::start();
    assert!(
        holds_descriptor(opened.journal_dir(), &published)
            .unwrap()
            .is_none()
    );
    assert!(
        holds_descriptor(&home.path().join("missing"), &first)
            .unwrap()
            .is_none()
    );
    assert_eq!(recording.take(), []);
}

#[test]
fn an_established_journal_missing_a_published_witness_takes_the_writer_to_ingest_it() {
    let (home, paths) = fixture();
    let device = Device::open(&paths).unwrap().unwrap();
    let identity = client::read_identity(&paths).unwrap().unwrap();
    let first = descriptor(&identity, NETWORK, &ROOT, 1);
    let opened = Bootstrap::from_snapshot(&paths, &snapshot(&identity, &first)).unwrap();
    let request = device.request(NETWORK, first.trust().authority).unwrap();
    let witness = Node::test_seal(
        &ROOT,
        NETWORK,
        Edit::Update {
            parent: first.genesis.id(),
            hosts: &["m1".into()],
            payload: b"witness",
        },
        &[identity.recipient()],
        2,
        &[Fact::Bind {
            binding: request.binding().clone(),
        }],
    )
    .unwrap();
    let mut source = Journal::open(&home.path().join("source-control"), first.trust()).unwrap();
    for node in [&first.genesis, &witness] {
        source.ingest(node.bytes()).unwrap();
    }
    let published = snapshot(&identity, &Descriptor::from_journal(&source).unwrap());

    // Settled custody reads beside a writer; a witness it lacks needs one.
    let held = Journal::open_existing(opened.journal_dir(), opened.trust()).unwrap();
    assert_eq!(
        Bootstrap::from_snapshot(&paths, &snapshot(&identity, &first))
            .unwrap()
            .trust(),
        opened.trust()
    );
    let busy = Bootstrap::from_snapshot(&paths, &published).err().unwrap();
    assert_eq!(busy.kind(), crate::ErrorKind::JournalBusy);
    assert!(held.node(&witness.id()).is_none());
    drop(held);
    let joined = Bootstrap::from_snapshot(&paths, &published).unwrap();
    assert_eq!(joined.witness().unwrap().unwrap().id(), witness.id());
}

#[test]
fn vault_revalidation_retains_busy_and_changed_kinds_without_trusting_error_words() {
    use std::time::{Duration, Instant};
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    client::create(
        &paths,
        home.path().join("team.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let bootstrap = Bootstrap::cached(&paths).unwrap().unwrap();
    let check = |error: Error, kind| {
        assert_eq!(error.kind(), kind, "{error}");
        assert!(is_transient_vault_revalidation(&error));
        let copied_words = Error::new(format!("local context: {error}"));
        assert!(
            !is_transient_vault_revalidation(&copied_words),
            "only a typed source observation can request a retry"
        );
    };
    let held =
        aterm_uds::private_file::PrivateLock::take(&paths.dir, ".lock", Duration::ZERO, "held")
            .unwrap();
    for error in [
        bootstrap.revalidate_custody().err().unwrap(),
        bootstrap.local_source_read().err().unwrap(),
        Bootstrap::required_until(&paths, Instant::now())
            .err()
            .unwrap(),
        Bootstrap::required_with_journal_until(&paths, Instant::now())
            .err()
            .unwrap(),
        Bootstrap::established_with_journal_until(&paths, Instant::now())
            .err()
            .unwrap(),
    ] {
        check(error, crate::ErrorKind::VaultBusy);
    }
    drop(held);
    bootstrap.revalidate_custody().unwrap();
    assert!(bootstrap.local_source_read().is_ok());

    let admin = client::admin_open(&paths).unwrap();
    client::admin_publish(&paths, &admin, &admin.doc).unwrap();
    check(
        bootstrap.revalidate_custody().err().unwrap(),
        crate::ErrorKind::VaultChanged,
    );
    check(
        bootstrap.local_source_read().err().unwrap(),
        crate::ErrorKind::VaultChanged,
    );
    let current = Bootstrap::cached(&paths).unwrap().unwrap();
    current.revalidate_custody().unwrap();

    std::fs::rename(
        paths.dir.join(".lock"),
        paths.dir.join("retained-state-lock"),
    )
    .unwrap();
    let missing = current.local_source_read().err().unwrap();
    assert_eq!(missing.kind(), crate::ErrorKind::Other);
    assert!(!is_transient_vault_revalidation(&missing));
}

#[test]
fn bounded_bootstrap_retries_transient_vault_lock_after_releasing_custody() {
    use std::{cell::Cell, rc::Rc, time::Instant};

    for established in [false, true] {
        for change_source in [false, true] {
            let home = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(home.path());
            client::create(
                &paths,
                home.path().join("team.vault").to_str().unwrap(),
                None,
                Some("m1"),
            )
            .unwrap();
            let initial = Bootstrap::cached(&paths).unwrap().unwrap();
            {
                let _held_journal =
                    Journal::open_existing(initial.journal_dir(), initial.trust()).unwrap();
                assert!(
                    super::writer_here(&paths),
                    "writer_here must read settled custody beside a concurrent journal writer"
                );
            }
            let (file, _) =
                aterm_uds::private_file::open_private(&paths.dir, "commons.lock").unwrap();
            aterm_uds::keep_out_of_children(&file).unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "the test holder is a CustodyLock guard which unlocks before close"
            )]
            file.try_lock().unwrap();
            let mut commons_owner = Some(CustodyLock(file));
            let vault_file = std::fs::File::open(paths.dir.join(".lock")).unwrap();
            aterm_uds::keep_out_of_children(&vault_file).unwrap();
            let vault = Rc::new(CustodyLock(vault_file));
            let vf = Rc::clone(&vault);
            let waited = Rc::new(Cell::new(0usize));
            let observed = Rc::clone(&waited);
            let changed_paths = paths.clone();
            let target = home.path().join("other.vault");
            let on_hold = move || {
                let step = observed.get() + 1;
                observed.set(step);
                let vault_file: &std::fs::File = &vf.0;
                if step == 1 {
                    // The first source observation has completed. A vault writer wins
                    // immediately before the reader obtains Commons custody.
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "the test holder is a CustodyLock guard which unlocks before close"
                    )]
                    vault_file.try_lock().unwrap();
                    drop(commons_owner.take());
                } else {
                    assert_eq!(step, 2, "one custody race then one vault wait");
                    let (file, _) =
                        aterm_uds::private_file::open_private(&changed_paths.dir, "commons.lock")
                            .unwrap();
                    aterm_uds::keep_out_of_children(&file).unwrap();
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "the test holder is a CustodyLock guard which unlocks before close"
                    )]
                    file.try_lock()
                        .expect("a vault wait must release Commons custody");
                    let _released = CustodyLock(file);
                    vault_file.unlock().unwrap();
                    if change_source {
                        switch_source(&changed_paths, &target);
                    }
                }
            };
            HELD.with(|held| *held.borrow_mut() = Some(Box::new(on_hold)));
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            let reopened = if established {
                Bootstrap::established_with_journal_until(&paths, deadline)
                    .map(|(bootstrap, _)| Some(bootstrap))
            } else {
                Bootstrap::cached_until(&paths, deadline)
            };
            HELD.with(|held| held.borrow_mut().take());
            assert_eq!(
                waited.get(),
                2,
                "established={established}, result={:?}",
                reopened.as_ref().err()
            );
            if change_source {
                assert!(
                    !matches!(reopened, Ok(Some(_))),
                    "a changed source grants no observation"
                );
            } else {
                assert_eq!(reopened.unwrap().unwrap().trust(), initial.trust());
            }
        }
    }
}
