// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(super) fn fixture(root: &Path) -> (Paths, crate::server::Running, String) {
    let server = crate::server::start(
        std::net::TcpListener::bind("127.0.0.1:0").expect("loopback"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(20),
            keep_awake: false,
        },
    )
    .expect("vault server");
    let paths = Paths::for_home(&root.join("member"));
    let import = root.join("import");
    fsx::ensure_private_dir(&import).unwrap();
    fsx::write_atomic(&import.join("token"), b"inert-token\n", 0o600).unwrap();
    let url = format!("http://{}/v1/team", server.addr);
    create(&paths, &url, Some(&import), Some("member")).expect("signed vault");
    assert!(
        load_state(&paths)
            .unwrap()
            .network_materialization
            .is_none(),
        "creation's caller-supplied bytes are not a sync observation"
    );
    (paths, server, url)
}

fn observed(paths: &Paths) -> Option<VaultNetworkMaterialization> {
    let config = configured(paths).unwrap();
    match observe_materialization(paths, &config, || Ok(())).unwrap() {
        MaterializationObservation::Ready(held) => held.snapshot.network_materialization(),
        MaterializationObservation::Blocked(_) => None,
    }
}

fn original_time(paths: &Paths) -> Option<u64> {
    load_state(paths)
        .unwrap()
        .network_materialization
        .map(|receipt| receipt.observation.sampled_at_ms)
}

fn fetch_body(paths: &Paths) -> Received {
    let config = configured(paths).unwrap();
    let attempt = Attempt::start(paths, &config, VaultNetworkSource::ConfiguredUrl).unwrap();
    let fetched = transport::fetch(&config.source, None, Duration::from_secs(60)).unwrap();
    let body = attempt.received(&fetched);
    assert!(body.is_some(), "CONTROL: actual URL200 body");
    Received { fetched, body }
}

#[test]
fn actual_url_body_is_recorded_but_unchanged_poll_cache_and_raw_replay_do_not_renew_it() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, server, _) = fixture(root.path());
    sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
    let first = observed(&paths).expect("actual signed URL body admitted");
    assert_eq!(first.source(), VaultNetworkSource::ConfiguredUrl);
    assert_eq!(first.sequence(), load_state(&paths).unwrap().seq);
    let before =
        match observe_materialization(&paths, &configured(&paths).unwrap(), || Ok(())).unwrap() {
            MaterializationObservation::Ready(held) => held.snapshot,
            MaterializationObservation::Blocked(_) => panic!("ready"),
        };
    let mut state = load_state(&paths).unwrap();
    state
        .network_materialization
        .as_mut()
        .unwrap()
        .observation
        .sampled_at_ms = 1;
    save_state(&paths, &state).unwrap();
    let config = configured(&paths).unwrap();
    let unchanged =
        transport::fetch(&config.source, Some(state.seq), Duration::from_secs(60)).unwrap();
    assert!(
        matches!(unchanged, Fetched::NoNewer(_)),
        "CONTROL: actual no-newer response"
    );
    apply_fetched(
        &paths,
        config.clone(),
        load_state(&paths).unwrap(),
        unchanged,
    )
    .unwrap();
    assert_eq!(original_time(&paths), Some(1));
    let cached = fsx::read_private(&paths.cache_file(), envelope::MAX_ENVELOPE)
        .unwrap()
        .unwrap();
    sync_with(&paths, cached.as_slice().to_vec()).unwrap();
    assert_eq!(
        original_time(&paths),
        Some(1),
        "same cached bytes are not a new request"
    );
    apply_fetched_until(
        &paths,
        config,
        load_state(&paths).unwrap(),
        Fetched::Missing,
        None,
        Some(Instant::now() + Duration::from_secs(60)),
    )
    .unwrap();
    assert_eq!(original_time(&paths), Some(1));
    sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
    let fresh = observed(&paths).unwrap();
    assert!(
        fresh.sampled_at_ms() > 1,
        "a new real200 of the same signed version is a new historical event"
    );
    assert_eq!(fresh.sequence(), first.sequence());
    let after =
        match observe_materialization(&paths, &configured(&paths).unwrap(), || Ok(())).unwrap() {
            MaterializationObservation::Ready(held) => held.snapshot,
            MaterializationObservation::Blocked(_) => panic!("ready"),
        };
    assert_eq!(
        before, after,
        "receipt changes do not change materialized-file snapshot identity"
    );
    let opened = admin_open(&paths).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"newly-applied-token")
        .unwrap();
    let published = admin_publish(&paths, &opened, &doc).unwrap();
    let applied = observed(&paths).expect("ordinary sync admitted the new URL body");
    assert!(applied.sequence() > fresh.sequence());
    assert_eq!(applied.sequence(), published.seq);
    assert_eq!(
        std::fs::read(materialize::target_path(&paths.home, "token").unwrap()).unwrap(),
        b"newly-applied-token"
    );
    server.stop();
}

#[test]
fn actual_peer_url_body_keeps_peer_role_after_primary_missing() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, server, peer) = fixture(root.path());
    let mut config = configured(&paths).unwrap();
    config.source = Source::Url(format!("http://{}/v1/missing", server.addr));
    save_config(&paths, &config).unwrap();
    bounded::sync_reader_with_candidates_until(
        &paths,
        Instant::now() + Duration::from_secs(60),
        &[peer],
    )
    .unwrap();
    assert_eq!(
        observed(&paths).unwrap().source(),
        VaultNetworkSource::PeerUrl
    );
    server.stop();
}

#[test]
fn an_exact_peer_does_not_replace_a_healthy_configured_source_receipt() {
    for bounded in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let (paths, primary, _) = fixture(root.path());
        let peer = crate::server::start(
            std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store: root.path().join("peer"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(20),
                keep_awake: false,
            },
        )
        .unwrap();
        let peer_url = format!("http://{}/v1/team", peer.addr);
        transport::publish(
            &Source::Url(peer_url.clone()),
            &std::fs::read(paths.cache_file()).unwrap(),
        )
        .unwrap();
        if bounded {
            super::super::bounded::sync_reader_with_candidates_until(
                &paths,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                &[peer_url],
            )
            .unwrap();
        } else {
            sync_with_candidates(&paths, None, &[peer_url]).unwrap();
        }
        assert_eq!(
            observed(&paths).unwrap().source(),
            VaultNetworkSource::ConfiguredUrl,
            "bounded={bounded}: the configured URL supplied the held body"
        );
        peer.stop();
        primary.stop();
    }
}

#[test]
fn deferred_unchanged_repair_does_not_record_a_real_body() {
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, server, _) = fixture(root.path());
    let received = fetch_body(&paths);
    let config = configured(&paths).unwrap();
    let state = load_state(&paths).unwrap();
    let token = materialize::target_path(&paths.home, "token").unwrap();
    fsx::write_atomic(&token, b"locally-changed", 0o600).unwrap();
    assert!(!files_intact(&paths, &state), "CONTROL: files need repair");
    let mut repaired = Some((
        (source_identity(&config), state.seq),
        unsettled(&paths, &state),
    ));
    let outcome =
        apply_fetched_repairing(&paths, config, state, received, Some(&mut repaired)).unwrap();
    assert!(
        matches!(outcome, SyncOutcome::Unchanged { .. }),
        "CONTROL: unchanged debounce path"
    );
    assert_eq!(std::fs::read(token).unwrap(), b"locally-changed");
    assert_eq!(original_time(&paths), None);
    server.stop();
}

#[test]
fn failed_actual_body_apply_retains_old_receipt_without_projecting_it() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, server, _) = fixture(root.path());
    sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
    let old = original_time(&paths).unwrap();
    let token = materialize::target_path(&paths.home, "token").unwrap();
    std::fs::remove_file(&token).unwrap();
    std::fs::create_dir(&token).unwrap();
    let opened = admin_open(&paths).unwrap();
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"changed-token")
        .unwrap();
    admin_publish(&paths, &opened, &doc).unwrap();
    let outcome = sync_reader_until(&paths, Instant::now() + Duration::from_secs(60)).unwrap();
    assert!(matches!(outcome, SyncOutcome::Applied { report, .. } if !report.failed.is_empty()));
    assert_eq!(original_time(&paths), Some(old));
    assert_eq!(observed(&paths), None);
    server.stop();
}

#[test]
fn cache_substitution_clears_even_identical_previous_body_provenance() {
    let _answering = ServersAnswering::set();
    let lab = crate::client::log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let paths = &lab.m7;
    let Received { body, .. } = fetch_body(paths);
    let config = configured(paths).unwrap();
    let mut state = load_state(paths).unwrap();
    state.network_materialization = None;
    save_state(paths, &state).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    save_records(paths, &load_records(&lab.m22).unwrap().encode()).unwrap();
    assert!(
        log_entries::current_records_digest(paths, &config, &state) != state.records_digest,
        "CONTROL: local records need reapplication"
    );
    apply_fetched_until(
        paths,
        config,
        state,
        Received {
            fetched: Fetched::Missing,
            body,
        },
        None,
        Some(Instant::now() + Duration::from_secs(60)),
    )
    .unwrap();
    assert_eq!(
        original_time(paths),
        None,
        "local substitution cannot inherit even an exact-body earlier network token"
    );
    assert_eq!(
        std::fs::read(paths.home.join(".secrets/shared")).unwrap(),
        b"new"
    );
    lab.server.stop();
}

#[test]
fn f20_log_only_update_requires_a_real_body_to_record_the_new_materialization() {
    use crate::client::log_entries::OLD_MAC_WINDOW_SECS;
    use crate::client::log_entries_tests::{member_key, two_macs_at};
    use crate::membership_log::{EpochAudience, encode_records};
    use std::sync::atomic::{AtomicU64, Ordering};
    const START: u64 = 1_800_000_000;
    static CLOCK: AtomicU64 = AtomicU64::new(START);
    fn clock() -> u64 {
        CLOCK.load(Ordering::SeqCst)
    }

    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let macs = two_macs_at(
        &[(Audience::Everyone, "shared", b"v1".as_slice())],
        false,
        Some(clock),
    );
    let (reader, writer) = (&macs.m22, &macs.m7);
    sync(reader, None).expect("initial actual URL body");
    let before = load_state(reader).unwrap();
    let prior_receipt = before.network_materialization.as_ref().unwrap();
    let original_sample = prior_receipt.observation.sampled_at_ms;
    assert_eq!(prior_receipt.records_digest, before.records_digest);
    assert!(
        observed(reader).is_some(),
        "CONTROL: initial receipt projects"
    );
    // The ordinary non-admin log writer makes an authenticated Put without publishing an
    // envelope, as F20's quiet-vault fixture does. Both members have device-key proofs. The
    // server's clock steps past F20's window: its heard history then covers the window, and the
    // plain envelope reads made while the vault was set up (m7's join is one), all at START, are
    // older than it, so no Mac reads only the envelope.
    CLOCK.store(START + OLD_MAC_WINDOW_SECS + 1, Ordering::SeqCst);
    let log = load_records(writer).unwrap().log().unwrap().clone();
    let identity = read_identity(writer).unwrap().unwrap();
    let opened = log.open_audience_entries(&identity).unwrap().unwrap();
    let epoch = opened.maximal_epochs[&EpochAudience::Everyone][0];
    let key = *opened.unwrapped_keys[&epoch];
    let (bootstrap, actor, incarnation) = member_key(writer);
    let entry = Entry {
        name: "shared".into(),
        value: SecretBytes::from(b"v2".as_slice()),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    };
    let put = bootstrap
        .device()
        .with_seed(|seed| {
            log.draft_put(&actor, 9_000, epoch, &key, &entry)
                .unwrap()
                .sign(seed, incarnation)
        })
        .unwrap();
    transport::post_records(&macs.url, &encode_records(std::slice::from_ref(&put))).unwrap();
    sync_records(reader, None).expect("reader receives signed log-only edit");
    assert_eq!(
        admin_mirror_log(reader, &[]).unwrap(),
        None,
        "CONTROL: F20 leaves the envelope alone"
    );
    let config = configured(reader).unwrap();
    let recipient = read_identity(reader)
        .unwrap()
        .unwrap()
        .recipient()
        .to_bech32();
    let Source::Url(url) = &config.source else {
        panic!("configured URL");
    };
    let Fetched::Body(served) =
        transport::fetch_within_as(url, None, Duration::from_secs(60), Some(&recipient)).unwrap()
    else {
        panic!("CONTROL: actual signed URL200");
    };
    let (envelope, _, _) = envelope::verify_for_reader(
        &served,
        &config.name,
        &config.signer.unwrap(),
        &config.followed,
        None,
    )
    .unwrap();
    assert_eq!(envelope.seq, before.seq);
    assert_eq!(Some(envelope.digest()), before.digest);

    // A deadline-bound local cache repair admits the new records without a new URL body.
    apply_fetched_until(
        reader,
        config.clone(),
        load_state(reader).unwrap(),
        Fetched::NoNewer(None),
        None,
        Some(Instant::now() + Duration::from_secs(60)),
    )
    .unwrap();
    let cached = load_state(reader).unwrap();
    assert_eq!(cached.seq, before.seq);
    assert_eq!(cached.digest, before.digest);
    assert_ne!(cached.records_digest, before.records_digest);
    assert_eq!(
        std::fs::read(reader.home.join(".secrets/shared")).unwrap(),
        b"v2"
    );
    let retained = cached.network_materialization.as_ref().unwrap();
    assert_eq!(retained.observation.sampled_at_ms, original_sample);
    assert_eq!(
        retained.records_digest, before.records_digest,
        "cache repair keeps the original receipt context"
    );
    match observe_materialization(reader, &config, || Ok(())).unwrap() {
        MaterializationObservation::Ready(held) => assert_eq!(
            held.snapshot.network_materialization(),
            None,
            "old receipt does not match the new records"
        ),
        MaterializationObservation::Blocked(_) => panic!("CONTROL: log-only v2 is locally ready"),
    }

    // Ordinary sync asks for an actual body; the unchanged envelope can now witness the
    // successful current local materialization, including its distinct records digest.
    let gets = macs.server.envelope_gets();
    assert!(matches!(
        sync(reader, None).unwrap(),
        SyncOutcome::Unchanged { .. }
    ));
    assert!(
        macs.server.envelope_gets() > gets,
        "ordinary sync really fetched the URL"
    );
    let after = load_state(reader).unwrap();
    assert_eq!(after.seq, before.seq);
    assert_eq!(after.digest, before.digest);
    assert_eq!(after.records_digest, cached.records_digest);
    assert_eq!(
        after
            .network_materialization
            .as_ref()
            .unwrap()
            .records_digest,
        after.records_digest
    );
    let current = observed(reader).expect("new actual body records the matching materialization");
    assert_eq!(current.source(), VaultNetworkSource::ConfiguredUrl);
    assert_eq!(current.sequence(), before.seq);
    assert_eq!(
        std::fs::read(reader.home.join(".secrets/shared")).unwrap(),
        b"v2"
    );
    macs.server.stop();
}
