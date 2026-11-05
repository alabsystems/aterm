// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct RestoreRetired(Option<Retired>);

impl Drop for RestoreRetired {
    fn drop(&mut self) {
        envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = self.0.take());
    }
}

struct Fixture {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    config: Config,
    before: State,
    base: Vec<u8>,
    good: Vec<u8>,
    revoked: Vec<u8>,
    fork: Vec<u8>,
    old: Vec<u8>,
    pre_rotation: Vec<u8>,
    legacy_kept: Vec<u8>,
    legacy_dropped: Vec<u8>,
    legacy_narrowed: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let reader = machine_identity(&paths).unwrap();
        let admin = Identity::generate().unwrap();
        let mut doc = Document::new_vault("admin", &admin.recipient().to_bech32());
        doc.add_machine("reader", &reader.recipient().to_bech32())
            .unwrap();
        doc.set_entry(&Audience::Everyone, "shared", b"retained")
            .unwrap();
        let seed = [19; 32];
        let config = Config {
            source: Source::Url("http://127.0.0.1:1/v1/team".to_owned()),
            mirrors: Vec::new(),
            name: "team".to_owned(),
            signer: Some(envelope::signer_public(&seed).unwrap()),
            followed: Vec::new(),
            host: "reader".to_owned(),
        };
        save_config(&paths, &config).unwrap();
        let base = seal("team", 4, &seed, &[20; 32], false, &doc, &[]).unwrap();
        sync_with(&paths, base.clone()).unwrap();
        let before = load_state(&paths).unwrap();
        let good = descendant(&base, &seed, 1, before.digest.unwrap());
        let mut revoked = envelope::parse(&good).unwrap();
        revoked.lockers.retain(|locker| locker.label == "admin");
        let revoked = envelope::sign(
            &Unsigned {
                name: revoked.name,
                seq: revoked.seq,
                time: revoked.time,
                handoff: revoked.handoff,
                parents: revoked.parents,
                lockers: revoked.lockers,
            },
            &seed,
        )
        .unwrap();
        let fork = descendant(&base, &seed, 2, [0x55; 32]);
        let old = seal("team", 3, &seed, &[20; 32], false, &doc, &[]).unwrap();
        let pre_rotation = seal("team", 3, &[23; 32], &[20; 32], false, &doc, &[]).unwrap();
        let legacy_kept = seal("team", 7, &seed, &[20; 32], false, &doc, &[]).unwrap();
        let mut branch = Document::new_vault("admin", &admin.recipient().to_bech32());
        branch
            .add_machine("reader", &reader.recipient().to_bech32())
            .unwrap();
        branch
            .set_entry(&Audience::Everyone, "other", b"divergent")
            .unwrap();
        let legacy_dropped = seal("team", 8, &seed, &[20; 32], false, &branch, &[]).unwrap();
        let mut narrowed = Document::new_vault("admin", &admin.recipient().to_bech32());
        narrowed
            .add_machine("reader", &reader.recipient().to_bech32())
            .unwrap();
        narrowed
            .set_entry(
                &Audience::Hosts(vec!["reader".to_owned()]),
                "shared",
                b"retained",
            )
            .unwrap();
        let legacy_narrowed = seal("team", 9, &seed, &[20; 32], false, &narrowed, &[]).unwrap();
        Self {
            _root: root,
            paths,
            config,
            before,
            base,
            good,
            revoked,
            fork,
            old,
            pre_rotation,
            legacy_kept,
            legacy_dropped,
            legacy_narrowed,
        }
    }
}

pub(crate) fn cached_materialization_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "RequestFull" => {
            next.insert("full", 1);
        }
        "LoseFile" => {
            next.insert("dirty", 1);
        }
        "LoseCache" => {
            next.insert("cached", 0);
        }
        "Revoke" => {
            next.insert("active", 0);
        }
        "ForgetOwnership" => {
            next.insert("owned", 0);
        }
        "Older" => {
            next.insert("older", 1);
        }
        "BoundedReader" => {
            next.insert("repairs", 0);
            next.insert("full", 1);
        }
        "Decide" => {
            let mut f = Fixture::new();
            if state["dirty"] == 1 {
                std::fs::remove_file(f.paths.home.join(".secrets/shared")).unwrap();
            }
            if state["cached"] == 0 {
                std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
            }
            if state["active"] == 0 {
                f.before.membership = Stored::Revoked;
            }
            if state["owned"] == 0 {
                f.before.own = None;
            }
            let seq = f.before.seq - u64::from(state["older"] == 1);
            let mut body = network_observation::Attempt::start(
                &f.paths,
                &f.config,
                VaultNetworkSource::ConfiguredUrl,
            )
            .and_then(|a| a.received(&Fetched::Body(f.base.clone())));
            assert!(body.is_some());
            let fetched = peer_selection::cached_materialization(
                &f.paths,
                &f.config,
                &f.before,
                state["full"] == 1,
                state["repairs"] == 1,
                Some(seq),
                &mut body,
            );
            next.insert("phase", 1);
            next.insert("pick", i64::from(fetched.is_some()));
            next.insert("network", i64::from(fetched.is_some() && body.is_some()));
        }
        _ => panic!("cache materialization action {action}"),
    }
    next
}

#[test]
fn cached_materialization_model_conforms_and_rejects_conditional_repair() {
    let model = aterm_models_swarm::cached_materialization_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = cached_materialization_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut conditional = model.init_state();
    conditional.insert("dirty", 1);
    let mut bad = cached_materialization_transition(&conditional, "Decide");
    bad.insert("pick", 1);
    assert!(!model.successors("Decide", &conditional).contains(&bad));
    assert!(!model.check_invariant("ConditionalQuiet", &bad));
}

#[test]
fn cached_log_model_conforms_and_rejects_loss_as_progress() {
    let model = aterm_models_swarm::cached_log_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = cached_log_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut missing = model.init_state();
    missing.insert("log", 0);
    missing.insert("changed", 1);
    let mut bad = cached_log_transition(&missing, "Select");
    bad.insert("pick", 1);
    assert!(!model.successors("Select", &missing).contains(&bad));
    assert!(!model.check_invariant("ReplayAuthority", &bad));
    missing.insert("phase", 1);
    let mut bad = cached_log_transition(&missing, "Guard");
    bad.insert("wait", 0);
    assert!(!model.successors("Guard", &missing).contains(&bad));
    assert!(!model.check_invariant("KeepAppliedLog", &bad));
}

struct CachedLogView {
    config: Config,
    before: State,
    identity: Vec<u8>,
    envelope: Vec<u8>,
    records: Vec<u8>,
    changed: Vec<u8>,
}

struct CachedLogFixture {
    active: CachedLogView,
    pending: CachedLogView,
    advanced: Vec<u8>,
    admitted: Vec<u8>,
    removed: Vec<u8>,
}

fn cached_log_fixture() -> &'static CachedLogFixture {
    static FIXTURE: std::sync::OnceLock<CachedLogFixture> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let _answering = ServersAnswering::set();
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let mut active = CachedLogView {
            config: configured(&lab.m7).unwrap(),
            before: load_state(&lab.m7).unwrap(),
            identity: std::fs::read(lab.m7.identity_file()).unwrap(),
            envelope: std::fs::read(lab.m7.cache_file()).unwrap(),
            records: load_records(&lab.m7).unwrap().encode(),
            changed: Vec::new(),
        };
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"new")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        active.changed = load_records(&lab.m22).unwrap().encode();
        let advanced = std::fs::read(lab.m22.cache_file()).unwrap();
        let (reader, request, _) = log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
        let mut config = active.config.clone();
        config.host = "m8".to_owned();
        save_config(&reader, &config).unwrap();
        save_records(&reader, &active.changed).unwrap();
        sync_with(&reader, advanced.clone()).unwrap();
        let mut pending = CachedLogView {
            config,
            before: load_state(&reader).unwrap(),
            identity: std::fs::read(reader.identity_file()).unwrap(),
            envelope: advanced.clone(),
            records: active.changed.clone(),
            changed: Vec::new(),
        };
        assert_eq!(pending.before.membership, Stored::Pending);
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"unrelated")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        pending.changed = load_records(&lab.m22).unwrap().encode();
        log_entries::admit_member_in_log(
            &lab.m22,
            crate::membership_log::Entrant {
                request,
                node_key: [91; 32],
                name: "m8".to_owned(),
                label: "m8".to_owned(),
            },
            crate::membership_log::Evidence::Phrase,
        )
        .unwrap();
        let admitted = load_records(&lab.m22).unwrap().encode();
        let admin = admin_open(&lab.m22).unwrap();
        admin_revoke(&lab.m22, &admin, "m7", true).unwrap();
        let removed = load_records(&lab.m22).unwrap().encode();
        lab.server.stop();
        CachedLogFixture {
            active,
            pending,
            advanced,
            admitted,
            removed,
        }
    })
}

#[test]
fn cached_publication_model_keeps_denial_visible_and_blocks_publication() {
    let model = aterm_models_swarm::cached_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = cached_publication_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&actual));
            }
        }
    }
    let mut revoked = model.init_state();
    revoked.insert("revoked", 1);
    let mut hidden = cached_publication_transition(&revoked, "Read");
    hidden.insert("denied", 0);
    assert!(!model.check_invariant("DenialVisible", &hidden));
    assert!(!model.successors("Read", &revoked).contains(&hidden));
    let observed = cached_publication_transition(&revoked, "Read");
    let mut published = cached_publication_transition(&observed, "Publish");
    published.insert("copy", 1);
    assert!(!model.check_invariant("PublicationDenied", &published));
    assert!(!model.successors("Publish", &observed).contains(&published));
}

pub(crate) fn cached_publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "Revoke" {
        next.insert("revoked", 1);
        return next;
    }
    let fixture = cached_log_fixture();
    let view = &fixture.active;
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    save_config(&paths, &view.config).unwrap();
    save_state(&paths, &view.before).unwrap();
    fsx::write_atomic(&paths.identity_file(), &view.identity, 0o600).unwrap();
    fsx::write_atomic(&paths.cache_file(), &view.envelope, 0o600).unwrap();
    save_records(
        &paths,
        if state["revoked"] == 1 {
            &fixture.removed
        } else {
            &view.records
        },
    )
    .unwrap();
    match action {
        "Read" => {
            next.insert("phase", 1);
            next.insert(
                "denied",
                i64::from(steward::local_recipient_denied_in_roster(&paths)),
            );
        }
        "Publish" => {
            next.insert("phase", 2);
            next.insert(
                "copy",
                i64::from(publishable_copy(&paths, &view.config, &view.before).is_some()),
            );
        }
        _ => panic!("cached publication action {action}"),
    }
    next
}

pub(crate) fn cached_log_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "ChangeLog" => {
            next.insert("changed", 1);
        }
        "LoseLog" => {
            next.insert("log", 0);
        }
        "LoseCache" => {
            next.insert("cache", 0);
        }
        "NoAppliedLog" => {
            next.insert("applied", 0);
            next.insert("changed", 1);
        }
        "ReplaceLineage" => {
            next.insert("replaced", 1);
        }
        "BehindStamp" => {
            next.insert("behind", 1);
            next.insert("changed", 1);
        }
        "Pending" => {
            next.insert("active", 0);
            next.insert("eligible", 0);
            next.insert("changed", 1);
        }
        "Admit" => {
            next.insert("eligible", 1);
        }
        "Select" => {
            let fixture = cached_log_fixture();
            let view = if state["active"] == 1 {
                &fixture.active
            } else {
                &fixture.pending
            };
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(root.path());
            fsx::ensure_private_dir(&paths.dir).unwrap();
            save_config(&paths, &view.config).unwrap();
            fsx::write_atomic(&paths.identity_file(), &view.identity, 0o600).unwrap();
            let mut before = view.before.clone();
            let bytes = if state["behind"] == 1 {
                let envelope = envelope::parse(&fixture.advanced).unwrap();
                before.seq = envelope.seq;
                before.digest = Some(envelope.digest());
                before.records_digest = None;
                &fixture.advanced
            } else {
                &view.envelope
            };
            let records = if state["behind"] == 1 || state["changed"] == 0 {
                &view.records
            } else if state["active"] == 0 && state["eligible"] == 1 {
                &fixture.admitted
            } else {
                &view.changed
            };
            fsx::write_atomic(&paths.cache_file(), bytes, 0o600).unwrap();
            save_records(&paths, records).unwrap();
            if state["applied"] == 0 {
                before.records_digest = None;
            }
            if state["log"] == 0 {
                std::fs::remove_file(paths.records_file()).unwrap();
            }
            if state["cache"] == 0 {
                before.source_identity = None;
            }
            let mut body = network_observation::Attempt::start(
                &paths,
                &view.config,
                VaultNetworkSource::ConfiguredUrl,
            )
            .and_then(|attempt| attempt.received(&Fetched::Body(bytes.clone())));
            assert!(body.is_some());
            let selected =
                peer_selection::cached_log_progress(&paths, &view.config, &before, &mut body);
            next.insert("phase", 1);
            next.insert("pick", i64::from(selected.is_some()));
            next.insert("network", i64::from(selected.is_some() && body.is_some()));
        }
        "Guard" => {
            let mut before = cached_log_fixture().active.before.clone();
            if state["applied"] == 0 || state["replaced"] == 1 {
                before.records_digest = None;
            }
            let wait = peer_selection::awaiting_applied_log(
                &before,
                (state["log"] == 1).then_some([12; 32]),
            );
            next.insert("phase", 2);
            next.insert("wait", i64::from(wait));
        }
        _ => panic!("cached log action {action}"),
    }
    next
}

#[test]
fn bodyless_repair_authenticates_the_exact_cache_and_source() {
    for damage in 0..6 {
        let mut f = Fixture::new();
        std::fs::remove_file(f.paths.home.join(".secrets/shared")).unwrap();
        match damage {
            0 => {
                std::fs::remove_file(f.paths.cache_file()).unwrap();
            }
            1 => {
                std::fs::write(f.paths.cache_file(), b"forged envelope").unwrap();
            }
            2 => {
                std::fs::write(f.paths.cache_file(), &f.old).unwrap();
            }
            3 => {
                f.config.source = Source::Url("http://127.0.0.1:2/v1/team".to_owned());
            }
            4 => {
                f.before.source_identity = None;
            }
            5 => {
                f.before.membership = Stored::Pending;
                f.before.own = None;
            }
            _ => unreachable!(),
        }
        let fetched = peer_selection::cached_materialization(
            &f.paths,
            &f.config,
            &f.before,
            true,
            true,
            Some(f.before.seq),
            &mut None,
        );
        assert!(fetched.is_none(), "damage={damage}");
    }
}

#[test]
fn bodyless_repair_accepts_absent_or_ahead_etags_without_adopting_them() {
    for seq in [None, Some(400)] {
        let f = Fixture::new();
        std::fs::remove_file(f.paths.home.join(".secrets/shared")).unwrap();
        let fetched = peer_selection::cached_materialization(
            &f.paths, &f.config, &f.before, true, true, seq, &mut None,
        )
        .unwrap();
        let bytes = fetched;
        assert_eq!(bytes, f.base);
        assert_eq!(envelope::parse(&bytes).unwrap().seq, f.before.seq);
    }
}

#[test]
fn bodyless_repair_uses_the_locked_state_after_another_local_pass() {
    for (revoked, tagged) in [(false, false), (false, true), (true, false), (true, true)] {
        let f = Fixture::new();
        let next = if revoked {
            f.revoked.clone()
        } else {
            f.good.clone()
        };
        sync_with(&f.paths, next).unwrap();
        let current = load_state(&f.paths).unwrap();
        let file = f.paths.home.join(".secrets/shared");
        if file.exists() {
            std::fs::remove_file(&file).unwrap();
        }
        let served = tagged.then_some(f.before.seq);
        let outcome = apply_response_until(
            &f.paths,
            f.config,
            f.before,
            Ok(Fetched::NoNewer(served).into()),
            None,
            None,
            true,
        )
        .unwrap();
        let after = load_state(&f.paths).unwrap();
        assert_eq!((after.seq, after.digest), (current.seq, current.digest));
        if revoked {
            assert_eq!(after.membership, Stored::Revoked);
            assert!(!file.exists());
        } else {
            assert!(matches!(outcome, SyncOutcome::Applied { .. }));
            assert_eq!(std::fs::read(file).unwrap(), b"retained");
        }
    }
}

#[test]
fn an_overtaken_network_body_uses_the_current_cache_without_network_credit() {
    for (dirty, missing_log) in [(false, false), (true, false), (false, true), (true, true)] {
        let f = Fixture::new();
        sync_with(&f.paths, f.good).unwrap();
        let mut current = load_state(&f.paths).unwrap();
        if missing_log {
            current.records_digest = Some([29; 32]);
            save_state(&f.paths, &current).unwrap();
        }
        let file = f.paths.home.join(".secrets/shared");
        if dirty {
            std::fs::remove_file(&file).unwrap();
        }
        let fetched = Fetched::Body(f.base);
        let body = network_observation::Attempt::start(
            &f.paths,
            &f.config,
            VaultNetworkSource::ConfiguredUrl,
        )
        .and_then(|a| a.received(&fetched));
        assert!(body.is_some());
        let config = f.config.clone();
        let outcome = apply_response_until(
            &f.paths,
            f.config,
            f.before,
            Ok(network_observation::Received { fetched, body }),
            None,
            None,
            dirty,
        )
        .unwrap();
        assert_eq!(
            matches!(outcome, SyncOutcome::Applied { .. }),
            dirty && !missing_log
        );
        let after = load_state(&f.paths).unwrap();
        assert_eq!((after.seq, after.digest), (current.seq, current.digest));
        if dirty && missing_log {
            assert!(
                !file.exists(),
                "an unavailable log cannot supply the missing value"
            );
        } else {
            assert_eq!(std::fs::read(file).unwrap(), b"retained");
        }
        if missing_log {
            assert_eq!(after.records_digest, current.records_digest);
        }
        assert!(network_observation::project(&f.paths, &config, &after).is_none());
    }
}

#[test]
fn a_concurrent_equal_version_body_is_verified_and_can_record_real_network_evidence() {
    for fork in [false, true] {
        let f = Fixture::new();
        sync_with(&f.paths, f.good.clone()).unwrap();
        let current = load_state(&f.paths).unwrap();
        let fetched = Fetched::Body(if fork {
            descendant(&f.base, &[19; 32], 1, [0x55; 32])
        } else {
            f.good
        });
        let body = network_observation::Attempt::start(
            &f.paths,
            &f.config,
            VaultNetworkSource::ConfiguredUrl,
        )
        .and_then(|a| a.received(&fetched));
        assert!(body.is_some());
        let config = f.config.clone();
        let outcome = apply_response_until(
            &f.paths,
            f.config,
            f.before,
            Ok(network_observation::Received { fetched, body }),
            None,
            None,
            false,
        );
        assert_eq!(outcome.is_err(), fork);
        let after = load_state(&f.paths).unwrap();
        assert_eq!((after.seq, after.digest), (current.seq, current.digest));
        assert_eq!(
            network_observation::project(&f.paths, &config, &after).is_some(),
            !fork
        );
    }
}

#[test]
fn bodyless_conditional_repair_ignores_a_stale_pre_fetch_log_snapshot() {
    let mut f = Fixture::new();
    // The pre-fetch snapshot saw unapplied log activity. Another pass has
    // already settled it when this response reaches the state lock.
    f.before.records_digest = Some([29; 32]);
    let file = f.paths.home.join(".secrets/shared");
    std::fs::remove_file(&file).unwrap();
    let outcome = apply_response_until(
        &f.paths,
        f.config,
        f.before,
        Ok(Fetched::NoNewer(None).into()),
        None,
        None,
        false,
    )
    .unwrap();
    assert!(matches!(outcome, SyncOutcome::Unchanged { .. }));
    assert!(!file.exists());
}

#[test]
fn losing_the_log_does_not_replay_an_older_cached_value() {
    let _answering = ServersAnswering::set();
    for response in 0..5 {
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let config = configured(&lab.m7).unwrap();
        let before = load_state(&lab.m7).unwrap();
        let cached = std::fs::read(lab.m7.cache_file()).unwrap();
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"middle")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        let middle = std::fs::read(lab.m22.cache_file()).unwrap();
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"new")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
        apply_response_until(
            &lab.m7,
            config.clone(),
            before,
            Err(Error::new("source unavailable")),
            None,
            None,
            false,
        )
        .unwrap();
        if response == 4 {
            let admin = admin_open(&lab.m22).unwrap();
            admin_revoke(&lab.m22, &admin, "m7", true).unwrap();
            save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
            let outcome = apply_response_until(
                &lab.m7,
                config.clone(),
                load_state(&lab.m7).unwrap(),
                Err(Error::new("source unavailable")),
                None,
                None,
                false,
            )
            .unwrap();
            assert!(matches!(outcome, SyncOutcome::NotIn));
        }
        let applied = load_state(&lab.m7).unwrap();
        let file = lab.m7.home.join(".secrets/shared");
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        std::fs::remove_file(lab.m7.records_file()).unwrap();
        let received = match response {
            0 => Err(Error::new("source unavailable")),
            1 => Ok(Fetched::NoNewer(None).into()),
            2 => Ok(Fetched::Body(cached).into()),
            _ => Ok(Fetched::Body(middle).into()),
        };
        let _ = apply_response_until(&lab.m7, config, applied.clone(), received, None, None, true);
        assert_eq!(
            std::fs::read(file).unwrap(),
            b"new",
            "log loss is not a new value: response={response}"
        );
        assert_eq!(
            load_state(&lab.m7).unwrap().records_digest,
            applied.records_digest
        );
        assert_eq!(load_state(&lab.m7).unwrap().membership, applied.membership);
        if response == 4 {
            assert_eq!(applied.membership, Stored::Revoked);
            assert!(!materialization_ready(&lab.m7).unwrap());
        }
        lab.server.stop();
    }
}

#[test]
fn an_exact_peer_remains_usable_when_the_local_log_cannot_support_publication() {
    for older in [false, true] {
        let f = Fixture::new();
        let mut before = f.before.clone();
        before.records_digest = Some([99; 32]);
        save_state(&f.paths, &before).unwrap();
        assert!(held_copy(&f.paths, &f.config, &before).is_some());
        assert!(publishable_copy(&f.paths, &f.config, &before).is_none());
        let primary = if older {
            Fetched::Body(f.old.clone())
        } else {
            Fetched::Missing
        };
        let selected = peer_selection::select(
            &f.paths,
            &f.config,
            &before,
            &Ok(primary),
            vec![head(f.base, &f.config)],
            true,
            None,
        )
        .unwrap()
        .expect("an exact peer can supply the retained body");
        let outcome = apply_fetched_until(
            &f.paths,
            f.config,
            before.clone(),
            Fetched::Body(selected.envelope),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(outcome, SyncOutcome::Unchanged { .. }));
        assert_eq!(
            load_state(&f.paths).unwrap().records_digest,
            before.records_digest
        );
        assert_eq!(
            std::fs::read(f.paths.home.join(".secrets/shared")).unwrap(),
            b"retained"
        );
    }
}

#[test]
fn an_older_log_omission_cannot_block_publication_of_a_newer_admitted_envelope() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"current")]);
    let config = configured(&lab.m7).unwrap();
    let identity = read_identity(&lab.m7).unwrap().unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    {
        let lock = StateLock::take(&lab.m22).unwrap();
        log_entries::retire_recipients_in_log_local(
            &lab.m22,
            &lock,
            &[],
            &BTreeSet::from([identity.recipient()]),
        )
        .unwrap();
    }
    let old_records = load_records(&lab.m22).unwrap().encode();
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
    save_records(
        &lab.m22,
        &crate::membership_log::encode_records(&[next_record]),
    )
    .unwrap();
    let stamp =
        log_entries::stamp_line(&load_records(&lab.m22).unwrap().log().unwrap().heads()).unwrap();
    let bytes = seal_stamped(
        &config.name,
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
    let reader = Paths::for_home(&lab.root.path().join("reader-no-log"));
    log_entries_tests::clone_identity_to(&lab.m7, &reader);
    save_config(&reader, &config).unwrap();
    sync_with(&reader, bytes.clone()).unwrap();
    let before = load_state(&reader).unwrap();
    assert_eq!(before.membership, Stored::In);
    assert!(before.records_digest.is_none());
    save_records(&reader, &old_records).unwrap();
    let envelope = envelope::parse(&bytes).unwrap();
    let (log, _) = log_entries::trusted_seeded_log(&reader, &config, &envelope)
        .unwrap()
        .unwrap();
    assert!(!log.removes(&identity.recipient()));
    assert!(log.audience_left_out_recipient(
        &crate::membership_log::EpochAudience::Everyone,
        &identity.recipient()
    ));
    assert_eq!(
        log_entries::log_base(Some(&log), &envelope, &identity).unwrap(),
        log_entries::LogBase::Behind
    );
    assert!(!steward::local_recipient_denied_in_roster(&reader));
    assert!(publishable_copy(&reader, &config, &before).is_some());
    lab.server.stop();
}

#[test]
fn a_replacement_waiting_for_records_preserves_the_previous_state() {
    let _answering = ServersAnswering::set();
    let old = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let replacement =
        log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"replacement")]);
    let config = configured(&old.m7).unwrap();
    let before = load_state(&old.m7).unwrap();
    let cache = std::fs::read(old.m7.cache_file()).unwrap();
    let prefix = load_records(&replacement.m22).unwrap().encode();
    let identity = read_identity(&old.m7).unwrap().unwrap();
    let admin = admin_open(&replacement.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.add_machine("m40", &identity.recipient().to_bech32())
        .unwrap();
    admin_publish(&replacement.m22, &admin, &doc).unwrap();
    let next = std::fs::read(replacement.m22.cache_file()).unwrap();
    let next_key = configured(&replacement.m22).unwrap().signer.unwrap();
    std::fs::remove_file(old.m7.records_file()).unwrap();
    // A foreign snapshot may be found on disk, but intake under the old root pin refuses it.
    assert!(save_records(&old.m7, &prefix).is_err());
    fsx::write_atomic(&old.m7.records_file(), &prefix, 0o600).unwrap();
    let previous = envelope::TEST_RETIRED.with(|cell| {
        cell.replace(Some(Retired {
            keys: vec![config.signer.unwrap()],
            current: Some(next_key),
        }))
    });
    let _restore = RestoreRetired(previous);
    let outcome = apply_fetched_until(
        &old.m7,
        config.clone(),
        before.clone(),
        Fetched::Body(next.clone()),
        None,
        None,
    )
    .unwrap();
    assert!(matches!(outcome, SyncOutcome::Unchanged { .. }));
    let after = load_state(&old.m7).unwrap();
    assert_eq!(
        (after.seq, after.digest, after.records_digest),
        (before.seq, before.digest, before.records_digest)
    );
    assert_eq!(configured(&old.m7).unwrap(), config);
    assert_eq!(std::fs::read(old.m7.cache_file()).unwrap(), cache);
    // A complete prospective log is inspected under the verified replacement pin and
    // retained before the new configuration or values become current.
    fsx::write_atomic(
        &old.m7.records_data_file().unwrap(),
        &load_records(&replacement.m22).unwrap().encode(),
        0o600,
    )
    .unwrap();
    let outcome =
        apply_fetched_until(&old.m7, config, before, Fetched::Body(next), None, None).unwrap();
    assert!(matches!(outcome, SyncOutcome::Applied { .. }));
    assert_eq!(configured(&old.m7).unwrap().signer, Some(next_key));
    assert_eq!(
        std::fs::read(old.m7.home.join(".secrets/shared")).unwrap(),
        b"replacement"
    );
    assert!(
        log_entries::trusted_configured_log(&old.m7, &configured(&old.m7).unwrap())
            .unwrap()
            .is_some()
    );
    old.server.stop();
    replacement.server.stop();
}

#[test]
fn a_non_admitting_log_change_preserves_the_transport_error() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let (reader, _, _) = log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
    let mut config = configured(&lab.m7).unwrap();
    config.host = "m8".to_owned();
    save_config(&reader, &config).unwrap();
    save_records(&reader, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync_with(&reader, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
    let before = load_state(&reader).unwrap();
    assert_eq!(before.membership, Stored::Pending);
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"unrelated")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&reader, &load_records(&lab.m22).unwrap().encode()).unwrap();
    let outcome = apply_response_until(
        &reader,
        config,
        before,
        Err(Error::new("unreachable pending source")),
        None,
        None,
        false,
    );
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "unreachable pending source"
    );
    lab.server.stop();
}

#[test]
fn an_unbound_cache_cannot_select_a_local_log_change() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let config = configured(&lab.m7).unwrap();
    let mut before = load_state(&lab.m7).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    before.source_identity = None;
    assert!(peer_selection::cached_log_progress(&lab.m7, &config, &before, &mut None).is_none());
    lab.server.stop();
}

#[test]
fn unbounded_sync_applies_received_removal_before_putting_back_a_cache() {
    let _answering = ServersAnswering::set();
    for response in 0..2 {
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let config = configured(&lab.m7).unwrap();
        let before = load_state(&lab.m7).unwrap();
        let admin = admin_open(&lab.m22).unwrap();
        admin_revoke(&lab.m22, &admin, "m7", true).unwrap();
        save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
        assert!(held_copy(&lab.m7, &config, &before).is_some());
        assert!(publishable_copy(&lab.m7, &config, &before).is_none());
        assert!(steward::local_recipient_denied_in_roster(&lab.m7));
        let fetched = if response == 0 {
            Fetched::Missing
        } else {
            Fetched::NoNewer(Some(before.seq - 1))
        };
        let outcome = apply_fetched_until(&lab.m7, config, before, fetched, None, None).unwrap();
        assert!(matches!(outcome, SyncOutcome::NotIn));
        assert_eq!(load_state(&lab.m7).unwrap().membership, Stored::Revoked);
        lab.server.stop();
    }
}

#[test]
fn a_behind_log_cannot_swallow_the_transport_error() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let old_records = load_records(&lab.m7).unwrap().encode();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    sync_with(&lab.m7, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
    let config = configured(&lab.m7).unwrap();
    let before = load_state(&lab.m7).unwrap();
    assert!(files_intact(&lab.m7, &before));
    save_records(&lab.m7, &old_records).unwrap();
    let outcome = apply_response_until(
        &lab.m7,
        config,
        before,
        Err(Error::new("unreachable source with a behind log")),
        None,
        None,
        false,
    );
    assert_eq!(
        outcome.unwrap_err().to_string(),
        "unreachable source with a behind log"
    );
    lab.server.stop();
}

#[test]
fn missing_applied_records_do_not_commit_a_signer_rotation() {
    let f = Fixture::new();
    let mut before = f.before.clone();
    before.records_digest = Some([11; 32]);
    save_state(&f.paths, &before).unwrap();
    let state_bytes = std::fs::read(f.paths.state_file()).unwrap();
    let mut rotated = envelope::parse(&f.good).unwrap();
    let next_seed = [21; 32];
    rotated.handoff = vec![(
        rotated.signer,
        envelope::endorse(
            &[19; 32],
            "team",
            &envelope::signer_public(&next_seed).unwrap(),
        )
        .unwrap(),
    )];
    let bytes = envelope::sign(
        &Unsigned {
            name: rotated.name,
            seq: rotated.seq,
            time: rotated.time,
            handoff: rotated.handoff,
            parents: rotated.parents,
            lockers: rotated.lockers,
        },
        &next_seed,
    )
    .unwrap();
    assert!(matches!(
        apply_response_until(
            &f.paths,
            f.config.clone(),
            before,
            Ok(Fetched::Body(bytes).into()),
            None,
            None,
            true
        )
        .unwrap(),
        SyncOutcome::Unchanged { .. }
    ));
    assert_eq!(configured(&f.paths).unwrap(), f.config);
    assert_eq!(std::fs::read(f.paths.state_file()).unwrap(), state_bytes);
    assert_eq!(std::fs::read(f.paths.cache_file()).unwrap(), f.base);
}

#[test]
fn failed_fetch_uses_the_current_locked_cache_for_local_log_progress() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let before = load_state(&lab.m7).unwrap();
    let config = configured(&lab.m7).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"middle")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    sync(&lab.m7, None).unwrap();
    let current = load_state(&lab.m7).unwrap();
    assert!(
        current.seq > before.seq,
        "another local pass advanced the head"
    );
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
    let outcome = apply_response_until(
        &lab.m7,
        config,
        before,
        Err(Error::new("source unavailable")),
        None,
        None,
        false,
    )
    .unwrap();
    assert!(matches!(outcome, SyncOutcome::Applied { .. }));
    let after = load_state(&lab.m7).unwrap();
    assert_eq!((after.seq, after.digest), (current.seq, current.digest));
    assert_ne!(after.records_digest, current.records_digest);
    assert_eq!(
        std::fs::read(lab.m7.home.join(".secrets/shared")).unwrap(),
        b"new"
    );
    lab.server.stop();
}

#[test]
fn failed_fetch_repairs_only_when_the_locked_state_admits_it() {
    for repair_requested in [false, true] {
        for revoked in [false, true] {
            let f = Fixture::new();
            sync_with(&f.paths, if revoked { f.revoked } else { f.good }).unwrap();
            let current = load_state(&f.paths).unwrap();
            let file = f.paths.home.join(".secrets/shared");
            if file.exists() {
                std::fs::remove_file(&file).unwrap();
            }
            let result = apply_response_until(
                &f.paths,
                f.config.clone(),
                f.before,
                Err(Error::new("source unavailable")),
                None,
                None,
                repair_requested,
            );
            let admitted = repair_requested && !revoked;
            assert_eq!(
                result.is_ok(),
                admitted,
                "repair={repair_requested} revoked={revoked}"
            );
            if admitted {
                assert!(matches!(result.unwrap(), SyncOutcome::Applied { .. }));
            } else {
                assert_eq!(result.unwrap_err().to_string(), "source unavailable");
            }
            assert_eq!(file.exists(), admitted);
            let after = load_state(&f.paths).unwrap();
            assert_eq!((after.seq, after.digest), (current.seq, current.digest));
            assert_eq!(after.membership, current.membership);
            assert!(network_observation::project(&f.paths, &f.config, &after).is_none());
        }
    }
}

#[test]
fn cached_retry_keeps_a_verified_source_binding_after_a_local_write_failure() {
    for offline in [false, true] {
        let f = Fixture::new();
        let dir = f.paths.home.join(".secrets");
        let file = dir.join("shared");
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        std::fs::write(&dir, b"local obstruction").unwrap();
        sync_with(&f.paths, f.base).unwrap();
        let failed = load_state(&f.paths).unwrap();
        assert!(failed.failed);
        assert!(source_bound(&f.config, &failed));
        std::fs::remove_file(&dir).unwrap();
        let response = if offline {
            Err(Error::new("source unavailable"))
        } else {
            Ok(Fetched::NoNewer(None).into())
        };
        apply_response_until(&f.paths, f.config, failed, response, None, None, false).unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"retained");
        assert!(!load_state(&f.paths).unwrap().failed);
    }
}

#[test]
fn a_lost_cache_does_not_let_a_legacy_peer_discard_the_exact_recovery_copy() {
    for cached in [true, false] {
        for repairs in [true, false] {
            for reverse in [false, true] {
                let f = Fixture::new();
                if !cached {
                    std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
                }
                let mut peers = vec![
                    head(f.base.clone(), &f.config),
                    head(f.legacy_dropped, &f.config),
                ];
                if reverse {
                    peers.reverse();
                }
                let selected = peer_selection::select(
                    &f.paths,
                    &f.config,
                    &f.before,
                    &Err(Error::new("source unavailable")),
                    peers,
                    repairs,
                    None,
                )
                .unwrap()
                .unwrap();
                assert_eq!(selected.digest, f.before.digest.unwrap(), "cached={cached}");
                sync_with(&f.paths, selected.envelope).unwrap();
                assert_eq!(
                    std::fs::read(f.paths.home.join(".secrets/shared")).unwrap(),
                    b"retained"
                );
            }
        }
    }
}

#[test]
fn an_exact_recovery_copy_allows_a_legacy_peer_that_preserves_held_entries() {
    let f = Fixture::new();
    std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
    let expected = envelope::parse(&f.legacy_kept).unwrap().digest();
    let selected = peer_selection::select(
        &f.paths,
        &f.config,
        &f.before,
        &Err(Error::new("source unavailable")),
        vec![head(f.base, &f.config), head(f.legacy_kept, &f.config)],
        true,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.digest, expected);
}

#[test]
fn a_changed_primary_preserves_exact_cached_comparison_evidence() {
    let f = Fixture::new();
    let mut config = f.config.clone();
    config.source = Source::Url("http://127.0.0.1:2/v1/team".to_owned());
    save_config(&f.paths, &config).unwrap();
    assert!(verified_cached_envelope(&f.paths, &config, &f.before).is_none());
    let expected = envelope::parse(&f.legacy_kept).unwrap().digest();
    let selected = peer_selection::select(
        &f.paths,
        &config,
        &f.before,
        &Err(Error::new("replacement source unavailable")),
        vec![
            head(f.legacy_dropped, &config),
            head(f.legacy_kept, &config),
        ],
        true,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.digest, expected);
    assert!(held_copy(&f.paths, &config, &f.before).is_none());
}

#[test]
fn a_legacy_peer_cannot_narrow_an_everyone_entry_to_this_reader() {
    let f = Fixture::new();
    let narrowed = f.legacy_narrowed;
    assert!(
        peer_selection::select(
            &f.paths,
            &f.config,
            &f.before,
            &Err(Error::new("source unavailable")),
            vec![head(narrowed.clone(), &f.config)],
            true,
            None,
        )
        .unwrap()
        .is_none()
    );
    // The configured authority can intentionally make this change.
    assert!(
        peer_selection::select(
            &f.paths,
            &f.config,
            &f.before,
            &Ok(Fetched::Body(narrowed.clone())),
            vec![head(narrowed, &f.config)],
            true,
            None,
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn a_compiled_signer_replacement_is_not_bound_by_the_retired_lineage() {
    for seq in [1, 5] {
        for cached in [true, false] {
            for repairs in [true, false] {
                let f = Fixture::new();
                let admin = Identity::generate().unwrap();
                let reader = read_identity(&f.paths).unwrap().unwrap();
                let mut doc = Document::new_vault("admin", &admin.recipient().to_bech32());
                doc.add_machine("reader", &reader.recipient().to_bech32())
                    .unwrap();
                doc.set_entry(&Audience::Everyone, "replacement", b"current lineage")
                    .unwrap();
                let seed = [25; 32];
                let current = envelope::signer_public(&seed).unwrap();
                let previous = envelope::TEST_RETIRED.with(|cell| {
                    cell.replace(Some(Retired {
                        keys: vec![f.config.signer.unwrap()],
                        current: Some(current),
                    }))
                });
                let _restore = RestoreRetired(previous);
                if !cached {
                    std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
                }
                let replacement = seal("team", seq, &seed, &[20; 32], false, &doc, &[]).unwrap();
                let selected = peer_selection::select(
                    &f.paths,
                    &f.config,
                    &f.before,
                    &Err(Error::new("source unavailable")),
                    vec![head(replacement.clone(), &f.config)],
                    repairs,
                    None,
                )
                .unwrap()
                .expect("compiled replacement peer");
                assert_eq!(
                    selected.digest,
                    envelope::parse(&replacement).unwrap().digest()
                );
                let primary = seal("team", seq + 1, &seed, &[20; 32], false, &doc, &[]).unwrap();
                assert!(
                    peer_selection::select(
                        &f.paths,
                        &f.config,
                        &f.before,
                        &Ok(Fetched::Body(primary)),
                        vec![head(replacement.clone(), &f.config)],
                        repairs,
                        None,
                    )
                    .unwrap()
                    .is_none(),
                    "the configured replacement keeps its receipt"
                );
                sync_with(&f.paths, selected.envelope).unwrap();
                assert_eq!(load_state(&f.paths).unwrap().seq, seq);
                assert_eq!(configured(&f.paths).unwrap().signer, Some(current));
                assert_eq!(
                    std::fs::read(f.paths.home.join(".secrets/replacement")).unwrap(),
                    b"current lineage"
                );
            }
        }
    }
}

pub(crate) fn replacement_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Retire" => {
            next.insert("retired", 1);
        }
        "WrongKey" => {
            next.insert("valid", 0);
        }
        "Newer" => {
            next.insert("newer", 1);
        }
        "LoseCache" => {
            next.insert("cached", 0);
        }
        "Bounded" => {
            next.insert("bounded", 1);
        }
        "UsePrimary" => {
            next.insert("primary", 1);
        }
        "Select" => {
            let f = Fixture::new();
            let current = envelope::signer_public(&[25; 32]).unwrap();
            let retired = (state["retired"] == 1).then(|| Retired {
                keys: vec![f.config.signer.unwrap()],
                current: Some(current),
            });
            let previous = envelope::TEST_RETIRED.with(|cell| cell.replace(retired));
            let _restore = RestoreRetired(previous);
            if state["cached"] == 0 {
                std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
            }
            let e = envelope::parse(&f.base).unwrap();
            let seed = if state["valid"] == 1 {
                [25; 32]
            } else {
                [26; 32]
            };
            let bytes = envelope::sign(
                &Unsigned {
                    name: e.name,
                    seq: if state["newer"] == 1 { 7 } else { 1 },
                    time: e.time,
                    handoff: Vec::new(),
                    parents: Vec::new(),
                    lockers: e.lockers,
                },
                &seed,
            )
            .unwrap();
            let e = envelope::parse(&bytes).unwrap();
            // Deliberately bypass the earlier network filter: the selector must
            // independently reject a key the compiled policy did not replace.
            let candidate = SyncPeerHead {
                url: "http://peer.local:7484/v1/test".to_owned(),
                seq: e.seq,
                digest: e.digest(),
                parents: e.parents,
                envelope: bytes,
                network_body: None,
            };
            let primary = if state["primary"] == 1 {
                Ok(Fetched::Body(candidate.envelope.clone()))
            } else {
                Err(Error::new("source unavailable"))
            };
            let picked = peer_selection::select(
                &f.paths,
                &f.config,
                &f.before,
                &primary,
                vec![candidate],
                state["bounded"] == 0,
                None,
            )
            .unwrap();
            next.insert("phase", 1);
            next.insert("pick", i64::from(picked.is_some()));
        }
        _ => panic!("replacement action {action}"),
    }
    next
}

#[test]
fn peer_replacement_model_conforms_and_rejects_an_unauthorized_key() {
    let model = aterm_models_swarm::peer_replacement_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = replacement_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut bad = replacement_transition(&model.init_state(), "Select");
    bad.insert("pick", 1);
    assert!(
        !model
            .successors("Select", &model.init_state())
            .contains(&bad)
    );
    assert!(!model.check_invariant("AuthorizedReplacement", &bad));
}

#[test]
fn a_legacy_peer_waits_for_a_baseline_but_a_proven_descendant_does_not() {
    for repairs in [true, false] {
        let f = Fixture::new();
        std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
        assert!(
            peer_selection::select(
                &f.paths,
                &f.config,
                &f.before,
                &Err(Error::new("source unavailable")),
                vec![head(f.legacy_kept, &f.config)],
                repairs,
                None,
            )
            .unwrap()
            .is_none()
        );
        let selected = peer_selection::select(
            &f.paths,
            &f.config,
            &f.before,
            &Err(Error::new("source unavailable")),
            vec![head(f.revoked.clone(), &f.config)],
            repairs,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            selected.digest,
            envelope::parse(&f.revoked).unwrap().digest()
        );
    }
}

#[test]
fn an_unverified_exact_candidate_cannot_supply_a_legacy_baseline() {
    let f = Fixture::new();
    std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
    let mut false_exact = head(f.base, &f.config);
    false_exact.envelope = b"not a signed envelope".to_vec();
    assert!(
        peer_selection::select(
            &f.paths,
            &f.config,
            &f.before,
            &Err(Error::new("source unavailable")),
            vec![false_exact, head(f.legacy_kept, &f.config)],
            true,
            None,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        std::fs::read(f.paths.cache_file()).unwrap(),
        b"damaged cache"
    );
}

#[test]
fn unreadable_identity_does_not_waive_a_legacy_peer_comparison() {
    let f = Fixture::new();
    std::fs::write(f.paths.identity_file(), b"damaged identity").unwrap();
    let selected = peer_selection::select(
        &f.paths,
        &f.config,
        &f.before,
        &Err(Error::new("source unavailable")),
        vec![head(f.legacy_dropped, &f.config), head(f.base, &f.config)],
        true,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.digest, f.before.digest.unwrap());
}

#[test]
fn a_recovered_exact_body_restores_the_handoff_floor_before_peer_selection() {
    let f = Fixture::new();
    let mut rotated = envelope::parse(&f.base).unwrap();
    let next_seed = [21; 32];
    rotated.seq += 1;
    rotated.parents = vec![f.before.digest.unwrap()];
    rotated.handoff = vec![(
        rotated.signer,
        envelope::endorse(
            &[19; 32],
            "team",
            &envelope::signer_public(&next_seed).unwrap(),
        )
        .unwrap(),
    )];
    let signed = |e: Envelope| {
        envelope::sign(
            &Unsigned {
                name: e.name,
                seq: e.seq,
                time: e.time,
                handoff: e.handoff,
                parents: e.parents,
                lockers: e.lockers,
            },
            &next_seed,
        )
        .unwrap()
    };
    let exact = signed(rotated);
    sync_with(&f.paths, exact.clone()).unwrap();
    let config = configured(&f.paths).unwrap();
    let before = load_state(&f.paths).unwrap();
    let mut stripped = envelope::parse(&exact).unwrap();
    stripped.seq += 1;
    stripped.parents = vec![before.digest.unwrap()];
    stripped.handoff.clear();
    let stripped = signed(stripped);
    assert!(
        envelope::verify_for_reader(
            &stripped,
            &config.name,
            config.signer.as_ref().unwrap(),
            &config.followed,
            Some(&Floor::version(before.seq, before.digest.unwrap())),
        )
        .is_ok()
    );
    std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
    for primary in [
        Err(Error::new("source unavailable")),
        Ok(Fetched::Body(exact.clone())),
    ] {
        let selected = peer_selection::select(
            &f.paths,
            &config,
            &before,
            &primary,
            vec![
                head(stripped.clone(), &config),
                head(exact.clone(), &config),
            ],
            true,
            None,
        )
        .unwrap();
        if primary.is_err() {
            assert_eq!(selected.unwrap().digest, before.digest.unwrap());
        } else {
            assert!(selected.is_none());
        }
        assert_eq!(
            std::fs::read(f.paths.cache_file()).unwrap(),
            b"damaged cache"
        );
    }
}

/// The two homes every `Select` of a walk reads, each built once: a fresh home per state costs a
/// machine identity, seven seals and a sync, and [`peer_selection::select`] writes nothing there
/// (it leaves publishing and caching to the normal apply path). [`SelectionHomes::files`] holds
/// the walk to that.
pub(crate) struct SelectionHomes {
    /// The model's `cached = 1`.
    whole: Fixture,
    /// The model's `cached = 0`.
    damaged: Fixture,
}

impl SelectionHomes {
    fn new() -> Self {
        let damaged = Fixture::new();
        std::fs::write(damaged.paths.cache_file(), b"damaged cache").unwrap();
        Self {
            whole: Fixture::new(),
            damaged,
        }
    }

    fn at(&self, state: &aterm_spec::interp::State) -> &Fixture {
        if state["cached"] == 0 {
            &self.damaged
        } else {
            &self.whole
        }
    }

    /// Every file both homes hold, cache and records among them, byte for byte.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn read(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    read(&path, files);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    files.insert(path, bytes);
                }
            }
        }
        let mut files = BTreeMap::new();
        read(&self.whole.paths.home, &mut files);
        read(&self.damaged.paths.home, &mut files);
        files
    }
}

pub(crate) fn selection_transition(
    homes: &SelectionHomes,
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "HaveExact" => {
            next.insert("exact", 1);
        }
        "HaveGood" => {
            next.insert("good", 1);
        }
        "HaveFork" => {
            next.insert("fork", 1);
        }
        "NextLegacy" => {
            next.insert("legacy", state["legacy"] + 1);
        }
        "NextSource" => {
            next.insert("source", state["source"] + 1);
        }
        "Bounded" => {
            next.insert("bounded", 1);
        }
        "Revoke" => {
            next.insert("revoke", 1);
        }
        "LoseCache" => {
            next.insert("cached", 0);
        }
        "Select" => {
            let f = homes.at(state);
            let good = if state["revoke"] == 1 {
                &f.revoked
            } else {
                &f.good
            };
            let mut peers = Vec::new();
            for (name, bytes) in [("fork", &f.fork), ("exact", &f.base), ("good", good)] {
                if state[name] == 1 {
                    peers.push(head(bytes.clone(), &f.config));
                }
            }
            match state["legacy"] {
                0 => {}
                1 => peers.push(head(f.legacy_kept.clone(), &f.config)),
                2 => peers.push(head(f.legacy_dropped.clone(), &f.config)),
                3 => peers.push(head(f.legacy_narrowed.clone(), &f.config)),
                _ => panic!("legacy kind"),
            }
            let primary = match state["source"] {
                0 => Err(Error::new("source unavailable")),
                1 => Ok(Fetched::Missing),
                2 => Ok(Fetched::Body(f.base.clone())),
                3 => Ok(Fetched::NoNewer(Some(4))),
                4 => Ok(Fetched::Body(b"not an envelope".to_vec())),
                5 => Ok(Fetched::Body(f.good.clone())),
                6 => Ok(Fetched::Body(f.old.clone())),
                7 => Ok(Fetched::Body(f.pre_rotation.clone())),
                _ => panic!("source kind"),
            };
            let chosen = peer_selection::select(
                &f.paths,
                &f.config,
                &f.before,
                &primary,
                peers,
                state["bounded"] == 0,
                None,
            )
            .unwrap();
            next.insert("phase", 1);
            next.insert(
                "pick",
                chosen.map_or(0, |p| match p.seq {
                    4 => 1,
                    5 => 2,
                    6 => 3,
                    7 => 4,
                    8 => 5,
                    9 => 5,
                    _ => panic!("peer sequence"),
                }),
            );
        }
        _ => panic!("selection action {action}"),
    }
    next
}

/// Every `Select` the model reaches takes what the real [`peer_selection::select`] takes, over
/// the two homes of [`SelectionHomes`] and leaving both as they were. THE GAP: the keys are drawn
/// once per run rather than once per state, so a defect that shows only under some drawn key
/// bytes is sampled twice here, not once per `Select`.
#[test]
fn peer_selection_model_conforms_and_rejects_an_unfiltered_choice() {
    let model = aterm_models_swarm::peer_selection_model();
    aterm_spec::interp::prove_and_catch(&model);
    let faults = aterm_spec::verify::sweep_faults(std::slice::from_ref(&model));
    assert!(faults.is_empty(), "{faults:?}");
    let homes = SelectionHomes::new();
    let held = homes.files();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = selection_transition(&homes, &state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{}: {state:?} -> {actual:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("good", 1);
    state.insert("fork", 1);
    let mut bad = selection_transition(&homes, &state, "Select");
    bad.insert("pick", 3);
    assert!(!model.successors("Select", &state).contains(&bad));
    assert!(!model.check_invariant("AdmittedOnly", &bad));
    assert!(homes.files() == held, "a selection wrote to its home");
}

#[test]
fn parent_proven_removal_does_not_require_an_open_member_locker() {
    let f = Fixture::new();
    assert!(
        open_lockers(
            &envelope::parse(&f.revoked).unwrap(),
            &read_identity(&f.paths).unwrap().unwrap()
        )
        .unwrap()
        .is_none()
    );
    for primary in [
        Err(Error::new("source unavailable")),
        Ok(Fetched::Body(f.base.clone())),
    ] {
        let selected = peer_selection::select(
            &f.paths,
            &f.config,
            &f.before,
            &primary,
            vec![head(f.revoked.clone(), &f.config)],
            true,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            selected.digest,
            envelope::parse(&f.revoked).unwrap().digest()
        );
    }
    sync_with(&f.paths, f.revoked).unwrap();
    assert_eq!(load_state(&f.paths).unwrap().membership, Stored::Revoked);
}

#[test]
fn parents_without_a_held_digest_do_not_prove_removal() {
    let f = Fixture::new();
    let mut before = f.before.clone();
    before.digest = None;
    assert!(peer_subsumes_held(
        &f.paths,
        &f.config,
        &before,
        &head(f.good, &f.config),
        Some(&f.base),
    ));
    assert!(!peer_subsumes_held(
        &f.paths,
        &f.config,
        &before,
        &head(f.revoked, &f.config),
        Some(&f.base),
    ));
}

#[test]
fn an_exact_peer_recovers_an_unusable_cache_when_the_primary_has_no_body() {
    for missing in [false, true] {
        for response in [
            Fetched::Missing,
            Fetched::NoNewer(None),
            Fetched::NoNewer(Some(4)),
            Fetched::NoNewer(Some(3)),
        ] {
            let f = Fixture::new();
            if missing {
                std::fs::remove_file(f.paths.cache_file()).unwrap();
            } else {
                std::fs::write(f.paths.cache_file(), b"damaged cache").unwrap();
            }
            let selected = peer_selection::select(
                &f.paths,
                &f.config,
                &f.before,
                &Ok(response),
                vec![head(f.base, &f.config)],
                true,
                None,
            )
            .unwrap()
            .unwrap();
            sync_with(&f.paths, selected.envelope).unwrap();
            assert_eq!(load_state(&f.paths).unwrap().digest, f.before.digest);
            assert!(materialization_ready(&f.paths).unwrap());
        }
    }
}

#[test]
fn a_no_change_primary_retains_a_verified_revoked_cache() {
    let f = Fixture::new();
    sync_with(&f.paths, f.revoked.clone()).unwrap();
    let before = load_state(&f.paths).unwrap();
    assert_eq!(before.membership, Stored::Revoked);
    assert!(held_copy(&f.paths, &f.config, &before).is_none());
    assert!(verified_cached_envelope(&f.paths, &f.config, &before).is_some());
    assert!(
        peer_selection::select(
            &f.paths,
            &f.config,
            &before,
            &Ok(Fetched::NoNewer(Some(before.seq))),
            vec![head(f.revoked, &f.config)],
            true,
            None,
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn a_bodyless_response_applies_log_admission_from_a_pending_cache() {
    let _answering = ServersAnswering::set();
    for bounded in [true, false] {
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"newly admitted")]);
        let (reader, request, _) = log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
        let mut config = configured(&lab.m7).unwrap();
        config.host = "m8".to_owned();
        save_config(&reader, &config).unwrap();
        save_records(&reader, &load_records(&lab.m22).unwrap().encode()).unwrap();
        sync_with(&reader, std::fs::read(lab.m22.cache_file()).unwrap()).unwrap();
        let before = load_state(&reader).unwrap();
        assert_eq!(before.membership, Stored::Pending);
        let config = configured(&reader).unwrap();
        assert!(verified_cached_envelope(&reader, &config, &before).is_some());
        apply_fetched_until(
            &reader,
            config.clone(),
            before.clone(),
            Fetched::NoNewer(Some(before.seq)),
            None,
            bounded.then(|| Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        )
        .unwrap();
        assert_eq!(load_state(&reader).unwrap().membership, Stored::Pending);
        assert!(!reader.home.join(".secrets/shared").exists());
        log_entries::admit_member_in_log(
            &lab.m22,
            crate::membership_log::Entrant {
                request,
                node_key: [91; 32],
                name: "m8".to_owned(),
                label: "m8".to_owned(),
            },
            crate::membership_log::Evidence::Phrase,
        )
        .unwrap();
        save_records(&reader, &load_records(&lab.m22).unwrap().encode()).unwrap();
        apply_fetched_until(
            &reader,
            config,
            before.clone(),
            Fetched::NoNewer(Some(before.seq)),
            None,
            bounded.then(|| Instant::now() + aterm_spec::wait::MUST_HAPPEN),
        )
        .unwrap();
        let after = load_state(&reader).unwrap();
        assert_eq!(after.membership, Stored::In, "bounded={bounded}");
        assert_eq!(after.digest, before.digest);
        assert!(after.network_materialization.is_none());
        assert_eq!(
            std::fs::read(reader.home.join(".secrets/shared")).unwrap(),
            b"newly admitted"
        );
        lab.server.stop();
    }
}

fn head(bytes: Vec<u8>, config: &Config) -> SyncPeerHead {
    sync_peer_head(
        "http://peer.local:7484/v1/test",
        Fetched::Body(bytes),
        None,
        config,
        config.signer.as_ref().unwrap(),
    )
    .unwrap()
}

fn descendant(base: &[u8], seed: &[u8; 32], advance: u64, parent: [u8; 32]) -> Vec<u8> {
    let base = envelope::parse(base).unwrap();
    envelope::sign(
        &Unsigned {
            name: base.name,
            seq: base.seq + advance,
            time: base.time,
            handoff: base.handoff,
            parents: vec![parent],
            lockers: base.lockers,
        },
        seed,
    )
    .unwrap()
}

#[test]
fn a_higher_unmerged_peer_does_not_mask_a_lower_descendant() {
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"retained")]);
    let config = configured(&lab.m7).unwrap();
    let before = load_state(&lab.m7).unwrap();
    let base = std::fs::read(lab.m7.cache_file()).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let good = descendant(&base, &admin.seed, 1, before.digest.unwrap());
    let bad = descendant(&base, &admin.seed, 2, [0x55; 32]);
    let expected = envelope::parse(&good).unwrap().digest();
    for repairs in [true, false] {
        for primary in [
            Err(Error::new("primary unavailable")),
            Ok(Fetched::Missing),
            Ok(Fetched::Body(base.clone())),
        ] {
            for reverse in [true, false] {
                let mut peers = vec![head(good.clone(), &config), head(bad.clone(), &config)];
                if reverse {
                    peers.reverse();
                }
                let selected = peer_selection::select(
                    &lab.m7, &config, &before, &primary, peers, repairs, None,
                )
                .unwrap()
                .unwrap();
                assert_eq!(selected.digest, expected);
            }
        }
    }
    lab.server.stop();
}

#[test]
fn a_log_behind_head_does_not_mask_an_applicable_descendant() {
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let config = configured(&lab.m7).unwrap();
    let before = load_state(&lab.m7).unwrap();
    let base = std::fs::read(lab.m7.cache_file()).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let good = descendant(&base, &admin.seed, 1, before.digest.unwrap());
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let newer = std::fs::read(lab.m22.cache_file()).unwrap();
    let behind = descendant(&newer, &admin.seed, 1, before.digest.unwrap());
    let identity = read_identity(&lab.m7).unwrap().unwrap();
    let parsed = envelope::parse(&behind).unwrap();
    let seeded = log_entries::trusted_seeded_log(&lab.m7, &config, &parsed)
        .unwrap()
        .unwrap();
    assert_eq!(
        log_entries::log_base(Some(&seeded.0), &parsed, &identity).unwrap(),
        log_entries::LogBase::Behind
    );
    let selected = peer_selection::select(
        &lab.m7,
        &config,
        &before,
        &Err(Error::new("primary unavailable")),
        vec![head(behind, &config), head(good.clone(), &config)],
        true,
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.digest, envelope::parse(&good).unwrap().digest());
    lab.server.stop();
}

#[test]
fn ordinary_and_bounded_readers_apply_log_progress_without_an_envelope_server() {
    for bounded in [false, true] {
        for removed in [false, true] {
            let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
            let before = load_state(&lab.m7).unwrap();
            let admin = admin_open(&lab.m22).unwrap();
            if removed {
                admin_revoke(&lab.m22, &admin, "m7", true).unwrap();
            } else {
                let mut doc = admin.doc.clone();
                doc.set_entry(&Audience::Everyone, "shared", b"new")
                    .unwrap();
                admin_publish(&lab.m22, &admin, &doc).unwrap();
            }
            save_records(&lab.m7, &load_records(&lab.m22).unwrap().encode()).unwrap();
            lab.server.stop();
            if bounded {
                super::bounded::sync_reader_with_candidates_until(
                    &lab.m7,
                    Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                    &[],
                )
                .unwrap();
            } else {
                sync_with_candidates(&lab.m7, None, &[]).unwrap();
            }
            assert_eq!(load_state(&lab.m7).unwrap().digest, before.digest);
            if removed {
                assert_eq!(load_state(&lab.m7).unwrap().membership, Stored::Revoked);
            } else {
                assert_eq!(
                    std::fs::read(lab.m7.home.join(".secrets/shared")).unwrap(),
                    b"new"
                );
            }
        }
    }
}
