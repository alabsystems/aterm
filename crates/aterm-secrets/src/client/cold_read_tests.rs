// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn cold_read_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Apply" => {
            next.insert("applied", 1);
        }
        "LoseLog" => {
            next.insert("log", 0);
        }
        "RestoreLog" => {
            next.insert("log", 1);
        }
        "Read" => {
            static LOG: std::sync::OnceLock<(crate::membership_log::Log, [u8; 32])> =
                std::sync::OnceLock::new();
            let seeded = LOG.get_or_init(|| {
                let _answering = ServersAnswering::set();
                let lab = log_entries_tests::two_macs(&[]);
                let log = load_records(&lab.m7).unwrap().log().unwrap().clone();
                let digest = log_entries::records_heads_digest(&log);
                lab.server.stop();
                (log, digest)
            });
            let native = super::State {
                records_digest: (state["applied"] == 1).then_some(seeded.1),
                ..super::State::default()
            };
            next.insert("phase", 1);
            next.insert(
                "visible",
                i64::from(log_entries::applied_log_available(
                    &native,
                    (state["log"] == 1).then_some(seeded.1),
                )),
            );
        }
        _ => panic!("unknown cold-read transition {action}"),
    }
    next
}

#[test]
fn cold_read_model_conforms_to_the_native_persistent_floor() {
    let model = aterm_models_swarm::cold_read_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = cold_read_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&actual));
            }
        }
    }
    let mut lost = model.init_state();
    lost.insert("applied", 1);
    lost.insert("log", 0);
    let mut stale = cold_read_transition(&lost, "Read");
    stale.insert("visible", 1);
    assert!(!model.check_invariant("NoFallback", &stale));
    assert!(!model.successors("Read", &lost).contains(&stale));
}

fn visible_readers(paths: &Paths) -> [bool; 4] {
    let config = configured(paths).unwrap();
    [
        entry::observe_reserved_entry(paths, &config, "aterm-cold-probe")
            .is_ok_and(|entry| entry.is_some()),
        swarm_roster_until(
            paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        )
        .is_ok_and(|roster| roster.is_some()),
        sight(paths).is_ok_and(|sight| sight.is_some()),
        matches!(
            observe_commons(paths),
            Ok(crate::commons::Observation::Visible(_))
        ),
    ]
}

#[test]
fn a_standby_server_waits_for_its_applied_records() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let config = configured(&lab.m7).unwrap();
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: config.signer,
        });
    });
    let eligible = || steward::check_standby_eligibility(&lab.m7, None).unwrap();
    assert_eq!(eligible(), Ok("m7".to_owned()));
    let healthy = load_state(&lab.m7).unwrap();
    assert!(healthy.records_digest.is_some());
    {
        let _applying = StateLock::take(&lab.m7).unwrap();
        assert!(
            eligible().is_err(),
            "a fresh setup waits for the active apply"
        );
    }
    let mut failed = healthy.clone();
    failed.failed = true;
    save_state(&lab.m7, &failed).unwrap();
    assert!(
        eligible().is_err(),
        "failed materialization grants no setup"
    );
    save_state(&lab.m7, &healthy).unwrap();
    let records = load_records(&lab.m7).unwrap().encode();
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    assert!(
        eligible().is_err(),
        "standby setup cannot establish a server from an older envelope after applied log loss"
    );
    save_records(&lab.m7, &records).unwrap();
    assert_eq!(eligible(), Ok("m7".to_owned()));
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    lab.server.stop();
}

#[test]
fn a_cold_server_role_waits_for_its_applied_records() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let config = configured(&lab.m7).unwrap();
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: config.signer,
        });
    });
    let before = load_state(&lab.m7).unwrap();
    assert!(before.records_digest.is_some());
    let records = load_records(&lab.m7).unwrap().encode();
    let cache = std::fs::read(lab.m7.cache_file()).unwrap();
    let store = lab.root.path().join("role-server");
    let advertise = crate::cli::Advertise::Named(crate::alab_advertised().to_owned());
    let decide = |role: &mut crate::cli::ServeRole| {
        role.decide(&advertise, &store, &lab.m7, &mut Vec::new(), config.signer)
            .0
    };
    let mut warm = crate::cli::ServeRole::default();
    assert_eq!(decide(&mut warm).as_deref(), Some("m7"));
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    assert!(
        decide(&mut crate::cli::ServeRole::default()).is_none(),
        "a cold process cannot establish an ALab role from an older envelope after log loss"
    );
    assert!(matches!(
        steward::observe_alab_role(&lab.m7),
        steward::AlabRoleObservation::Unavailable(Some(_))
    ));
    assert_eq!(
        decide(&mut warm).as_deref(),
        Some("m7"),
        "the same-context role already established in this process may survive unavailable custody"
    );
    assert!(steward::steward_alab_policy_role(&lab.m7).is_none());
    save_records(&lab.m7, &records).unwrap();
    assert_eq!(
        decide(&mut crate::cli::ServeRole::default()).as_deref(),
        Some("m7")
    );
    assert_eq!(std::fs::read(lab.m7.cache_file()).unwrap(), cache);
    assert_eq!(
        load_state(&lab.m7).unwrap().records_digest,
        before.records_digest
    );
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    lab.server.stop();
}

#[test]
fn a_server_role_distinguishes_denial_from_unavailable_custody() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let original = configured(&lab.m7).unwrap();
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: original.signer,
        });
    });
    let store = lab.root.path().join("role-server");
    let advertise = crate::cli::Advertise::Named(crate::alab_advertised().to_owned());
    let decide = |role: &mut crate::cli::ServeRole| {
        role.decide(
            &advertise,
            &store,
            &lab.m7,
            &mut Vec::new(),
            original.signer,
        )
        .0
    };
    let mut warm = crate::cli::ServeRole::default();
    assert_eq!(decide(&mut warm).as_deref(), Some("m7"));
    {
        let _held = StateLock::take(&lab.m7).unwrap();
        assert!(matches!(
            steward::observe_alab_role(&lab.m7),
            steward::AlabRoleObservation::Unavailable(Some(_))
        ));
        assert_eq!(decide(&mut warm).as_deref(), Some("m7"));
        assert!(decide(&mut crate::cli::ServeRole::default()).is_none());
    }
    let records = load_records(&lab.m7).unwrap().encode();
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    let mut changed = original.clone();
    changed.host = "m8".to_owned();
    save_config(&lab.m7, &changed).unwrap();
    assert!(
        decide(&mut warm).is_none(),
        "a different context loses the retained role"
    );
    save_config(&lab.m7, &original).unwrap();
    assert!(
        decide(&mut warm).is_none(),
        "restoring the context cannot resurrect the old role"
    );
    save_records(&lab.m7, &records).unwrap();
    assert_eq!(decide(&mut warm).as_deref(), Some("m7"));

    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        b"deny_labels = [\"m7\"]\n",
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    sync(&lab.m7, None).unwrap();
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    assert!(matches!(
        steward::observe_alab_role(&lab.m7),
        steward::AlabRoleObservation::Denied
    ));
    assert!(
        decide(&mut warm).is_none(),
        "an authenticated cached denial withdraws the retained role even without records"
    );
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    lab.server.stop();
}

#[test]
fn cold_readers_withhold_stale_envelopes_until_applied_records_return() {
    let _answering = ServersAnswering::set();
    let roster = crate::swarm_roster::SwarmRoster::default().to_toml();
    let lab = log_entries_tests::two_macs(&[
        (Audience::Everyone, "aterm-cold-probe", b"old"),
        (
            Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            roster.as_bytes(),
        ),
    ]);
    let before = load_state(&lab.m7).unwrap();
    let cached = std::fs::read(lab.m7.cache_file()).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "aterm-cold-probe", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let records = load_records(&lab.m22).unwrap().encode();
    save_records(&lab.m7, &records).unwrap();
    apply_response_until(
        &lab.m7,
        configured(&lab.m7).unwrap(),
        before,
        Err(Error::new("offline")),
        None,
        None,
        false,
    )
    .unwrap();
    let joiner = Paths::for_home(&lab.root.path().join("cold-joiner"));
    handover::test_hand_allowed(&lab.m7, &joiner, "m8");
    let allowed =
        crate::members::Allowed::from_text(&handover::stored_allowed(&joiner).unwrap()).unwrap();
    assert_eq!(visible_readers(&lab.m7), [true; 4]);
    assert!(handover::handover_bundle(&lab.m7, &allowed).is_ok());
    assert_eq!(std::fs::read(lab.m7.cache_file()).unwrap(), cached);
    let applied = load_state(&lab.m7).unwrap();
    assert!(applied.records_digest.is_some());
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    // Every call starts without an EntryVersion. A warm observation cache must
    // obey the same persistent floor as a newly started reader process.
    assert_eq!(visible_readers(&lab.m7), [false; 4]);
    assert!(handover::handover_bundle(&lab.m7, &allowed).is_err());
    assert_eq!(
        load_state(&lab.m7).unwrap().records_digest,
        applied.records_digest
    );
    save_records(&lab.m7, &records).unwrap();
    assert_eq!(visible_readers(&lab.m7), [true; 4]);
    assert!(handover::handover_bundle(&lab.m7, &allowed).is_ok());
    let entry =
        entry::read_reserved_entry(&lab.m7, &configured(&lab.m7).unwrap(), "aterm-cold-probe")
            .unwrap()
            .unwrap();
    assert_eq!(entry.value().as_slice(), b"new");
    lab.server.stop();
}

#[test]
fn an_envelope_only_observation_cache_cannot_bypass_a_later_log_floor() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let mut state = load_state(&lab.m7).unwrap();
    let applied = state.records_digest.take().unwrap();
    let records = std::fs::read(lab.m7.records_file()).unwrap();
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    // Simulate an older envelope-only installation before this build adopted a frontier.
    let frontier_files = [
        "records.frontiers",
        "frontiers.anchor",
        "frontiers.astream",
        "frontiers.checkpoint",
    ];
    for name in frontier_files {
        std::fs::rename(
            lab.m7.dir.join(name),
            lab.m7.dir.join(format!("{name}.legacy-test")),
        )
        .unwrap();
    }
    save_state(&lab.m7, &state).unwrap();
    assert!(matches!(
        observe_commons(&lab.m7).unwrap(),
        crate::commons::Observation::Visible(_)
    ));
    // A log application can advance state without replacing its envelope. The
    // cache key must not turn its old empty-log observation into current authority.
    fsx::write_atomic(&lab.m7.records_file(), &records, 0o600).unwrap();
    drop(StateLock::take(&lab.m7).unwrap());
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    state.records_digest = Some(applied);
    save_state(&lab.m7, &state).unwrap();
    assert!(!matches!(
        observe_commons(&lab.m7),
        Ok(crate::commons::Observation::Visible(_))
    ));
    lab.server.stop();
}

#[test]
fn missing_applied_records_do_not_hide_authenticated_roster_denials() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    let identity = machine_identity(&paths).unwrap();
    let device = crate::commons::bootstrap::Device::open(&paths)
        .unwrap()
        .unwrap();
    let request = device.request([1; 16], [2; 32]).unwrap();
    let admin = Identity::generate().unwrap();
    let mut doc = Document::new_vault("admin", &admin.recipient().to_bech32());
    doc.add_machine("m7", &identity.recipient().to_bech32())
        .unwrap();
    let denied = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m7".to_owned()],
        deny_nodes: vec![aterm_codec::hex::encode_lower(request.binding().writer())],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        denied.to_toml().as_bytes(),
    )
    .unwrap();
    let seed = [77; 32];
    let config = Config {
        source: Source::Url("http://127.0.0.1:1/v1/cold".to_owned()),
        mirrors: Vec::new(),
        name: "cold".to_owned(),
        signer: Some(envelope::signer_public(&seed).unwrap()),
        followed: Vec::new(),
        host: "m7".to_owned(),
    };
    save_config(&paths, &config).unwrap();
    sync_with(
        &paths,
        seal("cold", 1, &seed, &[78; 32], false, &doc, &[]).unwrap(),
    )
    .unwrap();
    let mut state = load_state(&paths).unwrap();
    assert_eq!(state.membership, Stored::In);
    // Model the durable applied-log witness after the records themselves were lost.
    state.records_digest = Some([79; 32]);
    save_state(&paths, &state).unwrap();
    assert!(
        swarm_roster_until(
            &paths,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .is_none()
    );
    assert!(steward::local_recipient_denied_in_roster(&paths));
    assert!(
        admit::roster_keeps_out(&paths, None, request.binding(), [0; 32], None)
            .unwrap()
            .is_some()
    );
    let lock = StateLock::observe(&paths).unwrap().unwrap();
    assert!(
        admit::roster_keeps_out(&paths, None, request.binding(), [0; 32], Some(&lock))
            .unwrap()
            .is_some()
    );
    assert!(read_held_copy(&paths, &config, &state).unwrap().is_some());

    for noncurrent in noncurrent_states(&state) {
        save_state(&paths, &noncurrent).unwrap();
        assert!(
            swarm_roster_until(
                &paths,
                std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
            )
            .unwrap()
            .is_none()
        );
        assert!(
            steward::local_recipient_denied_in_roster(&paths),
            "noncurrent state must retain authenticated cached denial: {noncurrent:?}"
        );
    }
    drop(lock);
    std::fs::remove_file(paths.lock_file()).unwrap();
    assert!(
        steward::local_recipient_denied_in_roster(&paths),
        "losing the observation lock cannot erase retained denial evidence"
    );
    assert!(
        !matches!(
            admit::roster_keeps_out(&paths, None, request.binding(), [0; 32], None),
            Ok(None)
        ),
        "unavailable denial custody cannot authorize admission"
    );
}

fn noncurrent_states(state: &State) -> Vec<State> {
    let mut pending = state.clone();
    pending.membership = Stored::Pending;
    let mut revoked = state.clone();
    revoked.membership = Stored::Revoked;
    let mut failed = state.clone();
    failed.failed = true;
    let mut unapplied = state.clone();
    unapplied
        .pending
        .insert("other".to_owned(), "00".repeat(32));
    let mut absent = state.clone();
    absent.membership = Stored::None;
    let mut plain = state.clone();
    plain.membership = Stored::Plain;
    vec![pending, revoked, failed, unapplied, absent, plain]
}

pub(crate) fn caller_custody_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Advance" => {
            next.insert("behind", 1);
        }
        "Noncurrent" => {
            next.insert("current", 0);
        }
        "Read" => {
            static LOGS: std::sync::OnceLock<(
                crate::membership_log::Log,
                (crate::membership_log::Log, [u8; 32]),
            )> = std::sync::OnceLock::new();
            let (older, custody) = LOGS.get_or_init(|| {
                let _answering = ServersAnswering::set();
                let lab = log_entries_tests::two_macs(&[]);
                let older = load_records(&lab.m7).unwrap().log().unwrap().clone();
                let admin = admin_open(&lab.m22).unwrap();
                let mut doc = admin.doc.clone();
                doc.set_entry(&Audience::Everyone, "aterm-custody-probe", b"new")
                    .unwrap();
                admin_publish(&lab.m22, &admin, &doc).unwrap();
                let held = load_records(&lab.m22).unwrap().log().unwrap().clone();
                let digest = log_entries::records_heads_digest(&held);
                lab.server.stop();
                (older, (held, digest))
            });
            next.insert("phase", 1);
            next.insert(
                "admitted",
                i64::from(log_entries::caller_covers_custody(
                    if state["behind"] == 1 {
                        older
                    } else {
                        &custody.0
                    },
                    Some(custody),
                )),
            );
        }
        _ => panic!("unknown caller custody transition {action}"),
    }
    next
}

#[test]
fn caller_custody_model_conforms_to_native_head_coverage() {
    let model = aterm_models_swarm::caller_custody_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = caller_custody_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&actual));
            }
        }
    }
    let mut noncurrent = model.init_state();
    noncurrent.insert("current", 0);
    noncurrent.insert("behind", 1);
    let mut stale = caller_custody_transition(&noncurrent, "Read");
    stale.insert("admitted", 1);
    assert!(!model.check_invariant("RetainedHeads", &stale));
    assert!(!model.successors("Read", &noncurrent).contains(&stale));
}

#[test]
fn noncurrent_callers_cannot_supply_a_log_behind_authenticated_custody() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let older = load_records(&lab.m7).unwrap().log().unwrap().clone();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    let denied = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m99".to_owned()],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        denied.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync_with(&lab.m7, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
    let current = load_records(&lab.m7).unwrap().log().unwrap().clone();
    let state = load_state(&lab.m7).unwrap();
    for noncurrent in noncurrent_states(&state) {
        save_state(&lab.m7, &noncurrent).unwrap();
        assert!(
            log_entries::local_swarm_roster_for_log(&lab.m7, &older).is_err(),
            "an older caller log bypassed authenticated custody: {noncurrent:?}"
        );
        assert!(
            log_entries::local_swarm_roster_for_log(&lab.m7, &current)
                .unwrap()
                .unwrap()
                .is_label_denied("m99")
        );
    }
    // A newer readable roster can explicitly remove an earlier denial, even while
    // the local materialization is pending and its cached envelope still contains it.
    save_state(&lab.m7, &state).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut cleared = admin.doc.clone();
    cleared
        .set_entry(
            &Audience::Everyone,
            crate::SWARM_ROSTER_ENTRY,
            crate::swarm_roster::SwarmRoster::default()
                .to_toml()
                .as_bytes(),
        )
        .unwrap();
    admin_publish(&lab.m22, &admin, &cleared).unwrap();
    let draft = load_records(&lab.m22).unwrap().log().unwrap().clone();
    for noncurrent in noncurrent_states(&state) {
        save_state(&lab.m7, &noncurrent).unwrap();
        assert!(
            !log_entries::local_swarm_roster_for_log(&lab.m7, &draft)
                .unwrap()
                .unwrap()
                .is_label_denied("m99"),
            "a newer caller roster must clear a denial before saving its records"
        );
    }
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    for noncurrent in noncurrent_states(&state) {
        save_state(&lab.m7, &noncurrent).unwrap();
        assert!(
            !roster::swarm_roster_for_denial(&lab.m7)
                .unwrap()
                .unwrap()
                .is_label_denied("m99"),
            "a readable later roster must be able to clear a denial"
        );
    }
    lab.server.stop();
}

#[test]
fn an_unseeded_denial_probe_honors_record_sidecars() {
    for marker in ["records.crossed", "records.layout", "records.preparing"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        machine_identity(&paths).unwrap();
        let config = Config {
            source: Source::Url("http://127.0.0.1:1/v1/sidecar".to_owned()),
            mirrors: Vec::new(),
            name: "sidecar".to_owned(),
            signer: Some(envelope::signer_public(&[84; 32]).unwrap()),
            followed: Vec::new(),
            host: "m7".to_owned(),
        };
        save_config(&paths, &config).unwrap();
        save_state(&paths, &State::default()).unwrap();
        fsx::write_atomic(&paths.dir.join(marker), b"incomplete custody", 0o600).unwrap();
        if marker == "records.crossed" {
            assert!(records_marked_crossed(&paths).unwrap());
        } else {
            assert!(
                load_records(&paths).is_err(),
                "fixture must refuse {marker}"
            );
        }
        assert!(
            steward::local_recipient_denied_in_roster(&paths),
            "a missing data file with {marker} is not an unseeded reader"
        );
    }
}

#[test]
fn a_readable_caller_roster_reports_malformed_metadata() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        b"not = [toml",
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let log = load_records(&lab.m22).unwrap().log().unwrap().clone();
    assert!(log_entries::local_swarm_roster_for_log(&lab.m7, &log).is_err());
    lab.server.stop();
}

#[test]
fn an_unstamped_cache_cannot_hide_a_readable_caller_denial() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let admin = admin_open(&lab.m22).unwrap();
    let bytes = seal(
        &admin.config.name,
        admin.seq,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &admin.doc,
        &admin.handoff,
    )
    .unwrap();
    let env = envelope::parse(&bytes).unwrap();
    let mut state = load_state(&lab.m7).unwrap();
    state.seq = env.seq;
    state.digest = Some(env.digest());
    fsx::write_atomic(&lab.m7.cache_file(), &bytes, 0o600).unwrap();
    save_state(&lab.m7, &state).unwrap();
    let mut doc = admin.doc.clone();
    let denied = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m99".to_owned()],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        denied.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let caller = load_records(&lab.m22).unwrap().log().unwrap().clone();
    assert_eq!(
        log_entries::log_base(
            Some(&caller),
            &env,
            &read_identity(&lab.m7).unwrap().unwrap()
        )
        .unwrap(),
        log_entries::LogBase::Unstamped
    );
    assert!(
        log_entries::local_swarm_roster_for_log(&lab.m7, &caller)
            .unwrap()
            .unwrap()
            .is_label_denied("m99")
    );
    lab.server.stop();
}

#[test]
fn noncurrent_peer_denials_retain_explicit_removals_behind_an_envelope() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let (boot7, actor7, _) = log_entries_tests::member_key(&lab.m7);
    let request = boot7.request().unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    remove_member_in_log(&lab.m22, actor7).unwrap();
    let log = load_records(&lab.m22).unwrap().log().unwrap().clone();
    let (actor, device, incarnation) = log_entries::open_local_member_signer(&lab.m22, &log)
        .unwrap()
        .unwrap();
    let next_record = device
        .with_seed(|seed| {
            log.draft(
                &actor,
                99,
                crate::membership_log::Body::Name {
                    name: "m22-next".into(),
                },
            )
            .sign(seed, incarnation)
        })
        .unwrap();
    let mut later = log.clone();
    later.insert(next_record).unwrap();
    let stamp = log_entries::stamp_line(&later.heads()).unwrap();
    let bytes = seal_stamped(
        &admin.config.name,
        admin.seq + 1,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &admin.doc,
        &admin.handoff,
        &admin.admissions,
        Some(&stamp),
        &admin.opened_heads,
    )
    .unwrap();
    let envelope = envelope::parse(&bytes).unwrap();
    assert_eq!(
        log_entries::log_base(
            Some(&log),
            &envelope,
            &read_identity(&lab.m22).unwrap().unwrap()
        )
        .unwrap(),
        log_entries::LogBase::Behind,
    );
    fsx::write_atomic(&lab.m22.cache_file(), &bytes, 0o600).unwrap();
    let mut state = load_state(&lab.m22).unwrap();
    state.seq = envelope.seq;
    state.digest = Some(envelope.digest());
    for noncurrent in noncurrent_states(&state) {
        save_state(&lab.m22, &noncurrent).unwrap();
        assert!(
            admit::roster_keeps_out(&lab.m22, None, request.binding(), [0; 32], None)
                .unwrap()
                .is_some(),
            "an explicit peer removal is never an epoch omission"
        );
    }
    lab.server.stop();
}

#[test]
fn a_caller_log_cannot_bypass_lost_roster_denials() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let older = load_records(&lab.m7).unwrap().log().unwrap().clone();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    let denied = crate::swarm_roster::SwarmRoster {
        deny_labels: vec!["m99".to_owned()],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_ROSTER_ENTRY,
        denied.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync_with(&lab.m7, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
    assert!(
        swarm_roster_until(
            &lab.m7,
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .unwrap()
        .unwrap()
        .is_label_denied("m99")
    );
    std::fs::remove_file(lab.m7.records_file()).unwrap();
    let observed = log_entries::local_swarm_roster_for_log(&lab.m7, &older);
    assert!(observed.is_err() || observed.unwrap().is_some_and(|r| r.is_label_denied("m99")));
    lab.server.stop();
}

#[test]
fn an_endorsed_handoff_preserves_authenticated_record_custody() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let admin = admin_open(&lab.m22).unwrap();
    let envelope = envelope::parse(&std::fs::read(lab.m7.cache_file()).unwrap()).unwrap();
    let predecessor = [81; 32];
    let predecessor_key = envelope::signer_public(&predecessor).unwrap();
    let bytes = envelope::sign(
        &envelope::Unsigned {
            name: envelope.name.clone(),
            seq: envelope.seq,
            time: envelope.time,
            handoff: vec![(
                predecessor_key,
                envelope::endorse(&predecessor, &envelope.name, &envelope.signer).unwrap(),
            )],
            parents: envelope.parents.clone(),
            lockers: envelope.lockers.clone(),
        },
        &admin.seed,
    )
    .unwrap();
    let mut config = configured(&lab.m7).unwrap();
    config.signer = Some(predecessor_key);
    config.followed.clear();
    save_config(&lab.m7, &config).unwrap();
    let mut state = load_state(&lab.m7).unwrap();
    state.digest = Some(envelope::parse(&bytes).unwrap().digest());
    state.source_identity = Some(source_identity(&config));
    save_state(&lab.m7, &state).unwrap();
    fsx::write_atomic(&lab.m7.cache_file(), &bytes, 0o600).unwrap();
    assert!(
        log_entries::trusted_configured_log(&lab.m7, &config)
            .unwrap()
            .is_none()
    );
    assert!(
        read_verified_cached_envelope(&lab.m7, &config, &state)
            .unwrap()
            .is_some()
    );
    let store = load_records(&lab.m7).unwrap();
    assert!(log_entries::local_swarm_roster_for_log(&lab.m7, store.log().unwrap()).is_ok());
    lab.server.stop();
}

#[test]
fn a_failed_role_apply_cannot_hide_a_handoff_authenticated_removal() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let admin = admin_open(&lab.m22).unwrap();
    let previous = envelope::parse(&std::fs::read(lab.m7.cache_file()).unwrap()).unwrap();
    let next_seed = [82; 32];
    let next_key = envelope::signer_public(&next_seed).unwrap();
    let bytes = envelope::sign(
        &envelope::Unsigned {
            name: previous.name.clone(),
            seq: previous.seq,
            time: previous.time,
            handoff: vec![(
                previous.signer,
                envelope::endorse(&admin.seed, &previous.name, &next_key).unwrap(),
            )],
            parents: previous.parents.clone(),
            lockers: previous.lockers.clone(),
        },
        &next_seed,
    )
    .unwrap();
    let (_, actor, _) = log_entries_tests::member_key(&lab.m7);
    remove_member_in_log(&lab.m22, actor).unwrap();
    let removed_records = load_records(&lab.m22).unwrap().encode();
    let mut config = configured(&lab.m7).unwrap();
    config.signer = Some(next_key);
    config.followed.clear();
    save_config(&lab.m7, &config).unwrap();
    let mut state = load_state(&lab.m7).unwrap();
    state.digest = Some(envelope::parse(&bytes).unwrap().digest());
    state.source_identity = Some(source_identity(&config));
    save_state(&lab.m7, &state).unwrap();
    fsx::write_atomic(&lab.m7.cache_file(), &bytes, 0o600).unwrap();
    envelope::TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: vec![previous.signer],
            current: Some(next_key),
        });
    });
    assert!(
        log_entries::trusted_configured_log(&lab.m7, &config)
            .unwrap()
            .is_none()
    );
    assert!(
        log_entries::authenticated_custody(&lab.m7, &config, &state)
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        steward::observe_alab_role(&lab.m7),
        steward::AlabRoleObservation::Allowed { .. }
    ));
    assert_eq!(
        steward::check_standby_eligibility(&lab.m7, None).unwrap(),
        Ok("m7".to_owned())
    );
    save_records(&lab.m7, &removed_records).unwrap();
    state.failed = true;
    save_state(&lab.m7, &state).unwrap();
    assert!(
        matches!(
            steward::observe_alab_role(&lab.m7),
            steward::AlabRoleObservation::Denied
        ),
        "a pending or failed application cannot hide a removal authenticated by the cached handoff"
    );
    assert!(
        steward::check_standby_eligibility(&lab.m7, None)
            .unwrap()
            .is_err()
    );
    envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    lab.server.stop();
}
