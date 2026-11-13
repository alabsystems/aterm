// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn legacy_receipts_keep_their_prefix_when_new_receipts_are_appended() {
    let _answering = crate::client::ServersAnswering::set();
    let lab = crate::client::log_entries_tests::two_macs(&[(
        crate::text::Audience::Everyone,
        "shared",
        b"old",
    )]);
    let old = crate::client::load_records(&lab.m7).unwrap();
    let config = crate::client::configured(&lab.m7).unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(scratch.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::client::save_config(&paths, &config).unwrap();
    fsx::write_atomic(&paths.records_file(), &old.encode(), 0o600).unwrap();
    let log = old.log().unwrap();
    let Body::Genesis { signer, .. } = log.genesis().body() else {
        unreachable!()
    };
    let mut legacy = Vec::new();
    legacy.extend_from_slice(log.network());
    legacy.extend_from_slice(signer);
    legacy.extend_from_slice(&(config.name.len() as u16).to_le_bytes());
    legacy.extend_from_slice(config.name.as_bytes());
    legacy.extend_from_slice(
        &Frontier::empty(*log.network())
            .plan(log)
            .unwrap()
            .unwrap()
            .encode(),
    );
    let mut journal = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, false, limits()).unwrap();
    journal.finish_open().unwrap();
    journal.append(LEGACY_SUBJECT, legacy.clone()).unwrap();
    drop(journal);
    fsx::write_atomic(&claim(&paths), ANCHOR, 0o600).unwrap();
    let mut unproven = config.clone();
    unproven.signer = Some([55; 32]);
    unproven.followed.clear();
    assert!(
        pinned_network(&Reads::new(&paths), &unproven).is_err(),
        "an old receipt with no adopting-key evidence cannot be silently abandoned"
    );
    assert!(project_store(&Reads::new(&paths), &unproven, Store::with_compiled(None)).is_err());
    assert!(project_store(&Reads::new(&paths), &config, old).is_ok());

    let admin = crate::client::admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&crate::text::Audience::Everyone, "shared", b"new")
        .unwrap();
    crate::client::admin_publish(&lab.m22, &admin, &doc).unwrap();
    let new = crate::client::load_records(&lab.m22).unwrap();
    {
        let _state = crate::client::StateLock::take(&paths).unwrap();
        crate::client::write_records(&paths, &new).unwrap();
    }
    let journal = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, true, limits()).unwrap();
    let records = journal.read_range(astream_wire::Offset::ZERO, 2).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].subject, LEGACY_SUBJECT);
    assert_eq!(records[0].body, legacy);
    assert_eq!(records[1].subject, SUBJECT);
    assert!(project_store(&Reads::new(&paths), &config, new).is_ok());
    lab.server.stop();
}

#[test]
fn pending_enrollment_recovers_but_ready_empty_history_refuses() {
    let _answering = crate::client::ServersAnswering::set();
    let fixture = crate::client::log_entries_tests::two_macs(&[]);
    let source = crate::client::load_records(&fixture.m7).unwrap();
    let config = crate::client::configured(&fixture.m7).unwrap();
    let invalid = aterm_tempfile::tempdir().unwrap();
    let invalid_paths = Paths::for_home(invalid.path());
    fsx::ensure_private_dir(&invalid_paths.dir).unwrap();
    let mut invalid_config = config.clone();
    invalid_config.name = "x".repeat(MAX_NAME + 1);
    assert!(acknowledge_for(&invalid_paths, &source, &invalid_config).is_err());
    assert_eq!(
        fsx::kind(&invalid_paths.dir.join("frontiers.anchor")).unwrap(),
        fsx::Kind::Missing
    );
    assert_eq!(
        fsx::kind(&claim(&invalid_paths)).unwrap(),
        fsx::Kind::Missing
    );
    for committed in [false, true] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(scratch.path());
        fsx::ensure_private_dir(&paths.dir).unwrap();
        crate::client::save_config(&paths, &config).unwrap();
        fsx::write_atomic(&paths.records_file(), &source.encode(), 0o600).unwrap();
        let mut journal =
            aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, false, limits()).unwrap();
        journal.finish_open().unwrap();
        drop(journal);
        fsx::write_atomic(
            &claim(&paths),
            if committed { READY } else { PENDING },
            0o600,
        )
        .unwrap();
        assert!(project_store(&Reads::new(&paths), &config, source.clone()).is_err());
        if !committed {
            assert!(
                project_store(&Reads::new(&paths), &config, Store::with_compiled(None))
                    .unwrap()
                    .log()
                    .is_none(),
                "an empty pending attempt cannot poison an independent envelope-only context"
            );
        }
        let repaired = crate::client::StateLock::take(&paths);
        if committed {
            assert!(
                repaired.is_err(),
                "empty replacement cannot reset established history"
            );
            assert_eq!(
                fsx::read_private(&claim(&paths), PENDING.len())
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                READY
            );
        } else {
            drop(repaired.unwrap());
            assert_eq!(claim_ready(&paths).unwrap(), Some(true));
            assert!(project_store(&Reads::new(&paths), &config, source.clone()).is_ok());
            // A crash after the first delta but before its ready marker can finish locally.
            fsx::write_atomic(&claim(&paths), PENDING, 0o600).unwrap();
            assert!(project_store(&Reads::new(&paths), &config, source.clone()).is_err());
            drop(crate::client::StateLock::take(&paths).unwrap());
            assert!(project_store(&Reads::new(&paths), &config, source.clone()).is_ok());
        }
    }
    fixture.server.stop();
}

/// A take that finishes an enrollment a crash left before its claim goes on from the claim it
/// wrote, not from what it read before writing it.
///
/// NEGATIVE CONTROL (2026-11-05): with `StateLock::recovered` going on under the [`Reads`] it
/// began with, the take fails (`retained membership enrollment needs exclusive recovery`).
#[test]
fn a_take_goes_on_from_the_enrollment_claim_it_wrote() {
    let _answering = crate::client::ServersAnswering::set();
    let fixture = crate::client::log_entries_tests::two_macs(&[]);
    let source = crate::client::load_records(&fixture.m7).unwrap();
    let config = crate::client::configured(&fixture.m7).unwrap();
    fixture.server.stop();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(scratch.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::client::save_config(&paths, &config).unwrap();
    fsx::write_atomic(&paths.records_file(), &source.encode(), 0o600).unwrap();
    let mut journal = aterm_journal::Log::open(&paths.dir, NAME, ANCHOR, false, limits()).unwrap();
    journal.finish_open().unwrap();
    drop(journal);
    assert_eq!(
        claim_ready(&paths).unwrap(),
        None,
        "the crash left no claim"
    );

    drop(crate::client::StateLock::take(&paths).unwrap());

    assert_eq!(claim_ready(&paths).unwrap(), Some(true));
    assert!(project_store(&Reads::new(&paths), &config, source).is_ok());
}

#[test]
fn switching_vaults_retains_each_genesis_without_reusing_its_frontier() {
    let _answering = crate::client::ServersAnswering::set();
    let a = crate::client::log_entries_tests::two_macs(&[]);
    let b = crate::client::log_entries_tests::fresh_two_macs(&[]);
    let ca = crate::client::configured(&a.m7).unwrap();
    let cb = crate::client::configured(&b.m7).unwrap();
    let sa = crate::client::load_records(&a.m7).unwrap();
    let sb = crate::client::load_records(&b.m7).unwrap();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(scratch.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::client::save_config(&paths, &ca).unwrap();
    crate::client::write_records(&paths, &sa).unwrap();
    let mut other_name = ca.clone();
    other_name.name = "another-vault".into();
    assert!(project_store(&Reads::new(&paths), &other_name, sa.clone()).is_err());
    other_name.signer = None;
    other_name.followed.clear();
    crate::client::save_config(&paths, &other_name).unwrap();
    let no_pin = crate::client::load_records(&paths).unwrap();
    assert!(
        no_pin.log().is_none(),
        "an envelope-only source cannot inherit another vault's raw authority"
    );
    assert!(project_store(&Reads::new(&paths), &other_name, no_pin).is_ok());
    crate::client::save_config(&paths, &cb).unwrap();
    assert!(project_store(&Reads::new(&paths), &cb, sa.clone()).is_err());
    assert!(
        crate::client::load_records(&paths).unwrap().log().is_none(),
        "the new key line cannot load the previous vault's raw root"
    );
    crate::client::write_records(&paths, &sb).unwrap();
    assert_eq!(read(&paths).unwrap().0.len(), 2);
    assert_eq!(
        pinned_network(&Reads::new(&paths), &ca).unwrap(),
        Some(*sa.log().unwrap().network())
    );
    assert_eq!(
        pinned_network(&Reads::new(&paths), &cb).unwrap(),
        Some(*sb.log().unwrap().network())
    );
    crate::client::save_config(&paths, &ca).unwrap();
    assert!(
        project_store(
            &Reads::new(&paths),
            &ca,
            crate::client::load_records(&paths).unwrap()
        )
        .is_err()
    );
    crate::client::save_records(&paths, &sa.encode()).unwrap();
    assert!(
        project_store(
            &Reads::new(&paths),
            &ca,
            crate::client::load_records(&paths).unwrap()
        )
        .is_ok()
    );
    assert_eq!(
        read(&paths).unwrap().0.len(),
        2,
        "returning to A cannot forget B"
    );
    a.server.stop();
    b.server.stop();
}

#[test]
fn cached_endorsement_pins_a_new_signers_genesis_before_config_repinning() {
    let _answering = crate::client::ServersAnswering::set();
    let lab = crate::client::log_entries_tests::two_macs(&[]);
    let other = crate::client::log_entries_tests::fresh_two_macs(&[]);
    let admin = crate::client::admin_open(&lab.m22).unwrap();
    let source = crate::client::load_records(&lab.m7).unwrap();
    let foreign = crate::client::load_records(&other.m7).unwrap();
    let old_seed = [53; 32];
    let mut config = crate::client::configured(&lab.m7).unwrap();
    config.signer = Some(crate::envelope::signer_public(&old_seed).unwrap());
    config.followed.clear();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(scratch.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::client::save_config(&paths, &config).unwrap();
    let envelope = crate::envelope::sign_unchecked(
        &crate::envelope::Unsigned {
            name: config.name.clone(),
            seq: 1,
            time: 1,
            handoff: crate::envelope::test_chain(&config.name, &[old_seed, admin.seed]),
            parents: Vec::new(),
            lockers: vec![crate::envelope::Locker {
                label: "admin".into(),
                armored: crate::envelope::test_armor(b"inert lineage witness"),
            }],
        },
        &admin.seed,
    )
    .unwrap();
    fsx::write_atomic(&paths.cache_file(), &envelope, 0o600).unwrap();
    crate::client::write_records(&paths, &source).unwrap();
    assert_eq!(claim_ready(&paths).unwrap(), Some(true));
    assert_eq!(
        pinned_network(&Reads::new(&paths), &config).unwrap(),
        Some(*source.log().unwrap().network())
    );
    let before = std::fs::read(paths.records_data_file().unwrap()).unwrap();
    assert!(crate::client::write_records(&paths, &foreign).is_err());
    assert_eq!(
        std::fs::read(paths.records_data_file().unwrap()).unwrap(),
        before
    );
    std::fs::rename(paths.cache_file(), paths.dir.join("cache.saved")).unwrap();
    assert_eq!(
        pinned_network(&Reads::new(&paths), &config).unwrap(),
        Some(*source.log().unwrap().network()),
        "cache loss cannot forget the key that admitted this retained genesis"
    );
    assert!(project_store(&Reads::new(&paths), &config, Store::with_compiled(None)).is_err());
    assert!(
        crate::client::log_entries::trusted_configured_log(&paths, &config)
            .unwrap()
            .is_some()
    );
    assert!(
        crate::client::log_entries::commons_log_matches_local_lineage(
            &paths,
            source.log().unwrap()
        )
        .unwrap()
    );
    let Body::Genesis { root, founders, .. } = source.log().unwrap().genesis().body() else {
        unreachable!();
    };
    let older =
        crate::membership_log::Record::genesis(&old_seed, *root, founders.clone(), 1).unwrap();
    let mut older_store = Store::with_compiled(None);
    older_store.ingest(older.bytes()).unwrap();
    assert!(crate::client::write_records(&paths, &older_store).is_err());
    assert_eq!(
        std::fs::read(paths.records_data_file().unwrap()).unwrap(),
        before
    );
    lab.server.stop();
    other.server.stop();
}

#[test]
fn a_key_only_receipt_retains_a_repin_after_cache_loss() {
    let _answering = crate::client::ServersAnswering::set();
    let lab = crate::client::log_entries_tests::two_macs(&[]);
    let admin = crate::client::admin_open(&lab.m22).unwrap();
    let source = crate::client::load_records(&lab.m7).unwrap();
    let mut config = crate::client::configured(&lab.m7).unwrap();
    let next_seed = [53; 32];
    config.signer = Some(crate::envelope::signer_public(&next_seed).unwrap());
    config.followed.clear();
    let envelope = crate::envelope::sign_unchecked(
        &crate::envelope::Unsigned {
            name: config.name.clone(),
            seq: 1,
            time: 1,
            handoff: crate::envelope::test_chain(&config.name, &[admin.seed, next_seed]),
            parents: Vec::new(),
            lockers: vec![crate::envelope::Locker {
                label: "admin".into(),
                armored: crate::envelope::test_armor(b"inert lineage witness"),
            }],
        },
        &next_seed,
    )
    .unwrap();
    fsx::write_atomic(&lab.m7.cache_file(), &envelope, 0o600).unwrap();
    let before = version(&lab.m7).unwrap();
    let mut full = (*read(&lab.m7).unwrap()).clone();
    let records = full.1;
    full.1 = limits().records;
    assert!(
        full.plan(&Reads::new(&lab.m7), &config, source.log().unwrap(), false)
            .is_err()
    );
    full.1 = records;
    full.2 = limits().bytes;
    assert!(
        full.plan(&Reads::new(&lab.m7), &config, source.log().unwrap(), false)
            .is_err()
    );
    acknowledge_for(&lab.m7, &source, &config).unwrap();
    assert_ne!(
        version(&lab.m7).unwrap(),
        before,
        "a binding-only receipt invalidates authority observations"
    );
    std::fs::rename(lab.m7.cache_file(), lab.m7.dir.join("cache.saved")).unwrap();
    assert_eq!(
        pinned_network(&Reads::new(&lab.m7), &config).unwrap(),
        Some(*source.log().unwrap().network())
    );
    assert!(project_store(&Reads::new(&lab.m7), &config, Store::with_compiled(None)).is_err());
    assert!(project_store(&Reads::new(&lab.m7), &config, source).is_ok());
    lab.server.stop();
}
