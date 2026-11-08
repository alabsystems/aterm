// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Fixture {
    bundle: Vec<u8>,
    identity: Vec<u8>,
    stranger: Vec<u8>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: std::sync::OnceLock<Fixture> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let _answering = ServersAnswering::set();
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let (reader, request, allowed) =
            log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
        let (bundle, logged) = log_entries_tests::hand(&lab.m22, &request, &allowed, true);
        assert!(logged);
        let stranger = Paths::for_home(&lab.root.path().join("stranger"));
        machine_identity(&stranger).unwrap();
        let fixture = Fixture {
            bundle: bundle.encode().unwrap().as_slice().to_vec(),
            identity: std::fs::read(reader.identity_file()).unwrap(),
            stranger: std::fs::read(stranger.identity_file()).unwrap(),
        };
        lab.server.stop();
        fixture
    })
}

pub(super) fn applied_log() -> (aterm_tempfile::TempDir, Paths, Config, State) {
    let fixture = fixture();
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    fsx::write_atomic(&paths.identity_file(), &fixture.identity, 0o600).unwrap();
    let bundle = crate::members::Bundle::decode(&fixture.bundle).unwrap();
    admit::write_from_log_bundle(&paths, &bundle).unwrap();
    let config = configured(&paths).unwrap();
    let before = load_state(&paths).unwrap();
    (root, paths, config, before)
}

pub(crate) fn configured_log_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Envelope" => {
            next.insert("pure", 0);
        }
        "HaveCache" => {
            next.insert("empty", 0);
        }
        "Unbind" => {
            next.insert("bound", 0);
        }
        "LoseLog" => {
            next.insert("log", 0);
        }
        "Replace" => {
            next.insert("current", 0);
        }
        "NeedWork" => {
            next.insert("work", 1);
        }
        "Pending" => {
            next.insert("active", 0);
            next.insert("eligible", 0);
            next.insert("work", 1);
        }
        "Admit" => {
            next.insert("eligible", 1);
        }
        "Select" => {
            let (_root, paths, config, mut before) = applied_log();
            if state["pure"] == 0 {
                before.seq = 1;
            }
            if state["empty"] == 0 {
                fsx::write_atomic(&paths.cache_file(), b"not a log-only member", 0o600).unwrap();
            }
            if state["bound"] == 0 {
                before.source_identity = None;
            }
            if state["work"] == 1 {
                before.failed = true;
            }
            if state["active"] == 0 {
                before.membership = Stored::Pending;
                if state["eligible"] == 0 {
                    fsx::write_atomic(&paths.identity_file(), &fixture().stranger, 0o600).unwrap();
                }
            }
            save_state(&paths, &before).unwrap();
            if state["log"] == 0 {
                std::fs::remove_file(paths.records_file()).unwrap();
            }
            let previous = envelope::TEST_RETIRED.with(|cell| {
                cell.replace((state["current"] == 0).then(|| Retired {
                    keys: vec![config.signer.unwrap()],
                    current: Some([91; 32]),
                }))
            });
            let _lock = StateLock::take(&paths).unwrap();
            let pick = peer_selection::configured_log_recovery(&paths, &config, &before).is_some();
            envelope::TEST_RETIRED.with(|cell| {
                cell.replace(previous);
            });
            next.insert("phase", 1);
            next.insert("pick", i64::from(pick));
        }
        _ => panic!("unknown configured-log action {action}"),
    }
    next
}

#[test]
fn configured_log_model_conforms_to_native_selection() {
    let model = aterm_models_swarm::configured_log_model();
    assert!(aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&model, 1)).is_err());
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = configured_log_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut unbound = model.init_state();
    unbound.insert("work", 1);
    unbound.insert("bound", 0);
    let mut forged = configured_log_transition(&unbound, "Select");
    forged.insert("pick", 1);
    assert!(!model.check_invariant("SourceBound", &forged));
    assert!(!model.successors("Select", &unbound).contains(&forged));
}

#[test]
fn offline_log_only_write_failure_keeps_its_binding_for_retry() {
    let (_root, paths, config, before) = applied_log();
    let file = paths.home.join(".secrets/shared");
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    let apply = || {
        apply_response_until(
            &paths,
            config.clone(),
            load_state(&paths).unwrap(),
            Err(Error::new("offline")),
            None,
            None,
            true,
        )
    };
    let first = apply().unwrap();
    assert!(matches!(first, SyncOutcome::Applied { ref report, .. } if apply_failed(report)));
    let failed = load_state(&paths).unwrap();
    assert!(failed.failed);
    assert_eq!(failed.source_identity, before.source_identity);
    std::fs::remove_dir(&file).unwrap();
    assert!(matches!(apply().unwrap(), SyncOutcome::Applied { .. }));
    let repaired = load_state(&paths).unwrap();
    assert!(!repaired.failed);
    assert_eq!(repaired.records_digest, before.records_digest);
    assert_eq!(std::fs::read(&file).unwrap(), b"old");
    assert!(repaired.network_materialization.is_none());
}

#[test]
fn a_nonmember_log_does_not_hide_the_transport_error() {
    let (_root, paths, config, mut before) = applied_log();
    fsx::write_atomic(&paths.identity_file(), &fixture().stranger, 0o600).unwrap();
    before.membership = Stored::Pending;
    before.failed = true;
    save_state(&paths, &before).unwrap();
    let error = apply_response_until(
        &paths,
        config,
        before,
        Err(Error::new("offline original")),
        None,
        None,
        true,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "offline original");
}

#[test]
fn a_wrap_without_membership_cannot_admit_a_log_only_reader() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let (paths, request, allowed) = log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
    let (bundle, _) = log_entries_tests::hand(&lab.m22, &request, &allowed, true);
    admit::write_from_log_bundle(&paths, &bundle).unwrap();
    let config = configured(&paths).unwrap();
    let mut before = load_state(&paths).unwrap();
    let log = load_records(&lab.m22).unwrap().log().unwrap().clone();
    let member = read_identity(&lab.m22).unwrap().unwrap();
    let opened = log.open_audience_entries(&member).unwrap().unwrap();
    let everyone = crate::membership_log::EpochAudience::Everyone;
    let epoch = opened.maximal_epochs[&everyone][0];
    let key = &opened.unwrapped_keys[&epoch];
    let (actor, device, incarnation) = log_entries::open_local_member_signer(&lab.m22, &log)
        .unwrap()
        .unwrap();
    fsx::write_atomic(&paths.identity_file(), &fixture().stranger, 0o600).unwrap();
    let stranger = read_identity(&paths).unwrap().unwrap();
    let wrap = device
        .with_seed(|seed| {
            log.draft_wrap(&actor, 9_000, epoch, key, stranger.recipient())
                .unwrap()
                .sign(seed, incarnation)
        })
        .unwrap();
    save_records(&paths, &crate::membership_log::encode_records(&[wrap])).unwrap();
    let held = load_records(&paths).unwrap().log().unwrap().clone();
    assert!(!log_entries::log_alone_admits(&held, &stranger).unwrap());
    assert!(
        held.open_entries(&stranger).unwrap().is_some(),
        "the wrap alone is readable"
    );
    before.membership = Stored::Pending;
    before.failed = true;
    save_state(&paths, &before).unwrap();
    let lock = StateLock::take(&paths).unwrap();
    assert!(
        peer_selection::configured_log_recovery(&paths, &config, &before).is_none(),
        "a readable key capsule is not a membership grant"
    );
    drop(lock);
    let error = apply_response_until(
        &paths,
        config,
        before,
        Err(Error::new("offline original")),
        None,
        None,
        true,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "offline original");
    assert_eq!(load_state(&paths).unwrap().membership, Stored::Pending);
    lab.server.stop();
}

#[test]
fn missing_log_only_ownership_metadata_requires_local_work() {
    let (_root, paths, config, mut before) = applied_log();
    assert!(before.own.is_some());
    before.own = None;
    save_state(&paths, &before).unwrap();
    let lock = StateLock::take(&paths).unwrap();
    assert!(peer_selection::configured_log_recovery(&paths, &config, &before).is_some());
    drop(lock);
    let outcome = apply_response_until(
        &paths,
        config,
        before,
        Err(Error::new("offline")),
        None,
        None,
        true,
    )
    .unwrap();
    assert!(matches!(outcome, SyncOutcome::Applied { .. }));
    assert!(load_state(&paths).unwrap().own.is_some());
}
