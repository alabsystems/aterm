// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct CommitFixture {
    config: Config,
    old: crate::membership_log::Store,
    new: crate::membership_log::Store,
}

fn commit_fixture() -> &'static CommitFixture {
    static FIXTURE: std::sync::OnceLock<CommitFixture> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let _answering = ServersAnswering::set();
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let old = load_records(&lab.m7).unwrap();
        let config = configured(&lab.m7).unwrap();
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"new")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        let new = load_records(&lab.m22).unwrap();
        lab.server.stop();
        CommitFixture { config, old, new }
    })
}

pub(crate) fn retained_commit_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let fixture = commit_fixture();
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(scratch.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    save_config(&paths, &fixture.config).unwrap();
    write_records(&paths, &fixture.old).unwrap();
    if state["floor"] == 1 {
        write_records(&paths, &fixture.new).unwrap();
    }
    let raw = if state["raw"] == 0 {
        &fixture.old
    } else {
        &fixture.new
    };
    let raw_path = paths.records_data_file().unwrap();
    fsx::write_atomic(&raw_path, &raw.encode(), 0o600).unwrap();
    let checkpoint = paths.dir.join("frontiers.checkpoint");
    if state["damaged"] == 1 {
        fsx::write_atomic(&checkpoint, b"damaged", 0o600).unwrap();
    }
    let mut next = state.clone();
    match action {
        "Publish" => {
            fsx::write_atomic(&raw_path, &fixture.new.encode(), 0o600).unwrap();
            next.insert("read", 0);
            next.insert("seen", 2);
        }
        "Acknowledge" => {
            retained_history::acknowledge(&paths, raw).unwrap();
            next.insert("read", 0);
            next.insert("seen", 2);
        }
        "Rollback" => {
            fsx::write_atomic(&raw_path, &fixture.old.encode(), 0o600).unwrap();
            next.insert("read", 0);
            next.insert("seen", 2);
        }
        "Damage" => {
            fsx::write_atomic(&checkpoint, b"damaged", 0o600).unwrap();
            next.insert("read", 0);
            next.insert("seen", 2);
        }
        "Read" => {
            let view = retained_history::project_store(
                &super::Reads::new(&paths),
                &fixture.config,
                raw.clone(),
            );
            let seen = view.map_or(2, |view| {
                i64::from(view.log().unwrap().heads() == fixture.new.log().unwrap().heads())
            });
            next.insert("read", 1);
            next.insert("seen", seen);
        }
        _ => panic!("unknown retained commit action {action}"),
    }
    let bytes = fsx::read_private(&raw_path, crate::membership_log::MAX_RECORDS_STREAM)
        .unwrap()
        .unwrap();
    let held =
        crate::membership_log::Store::with_compiled(Some(*fixture.old.log().unwrap().network()))
            .recovering(bytes.as_slice());
    next.insert(
        "raw",
        i64::from(held.log().unwrap().heads() == fixture.new.log().unwrap().heads()),
    );
    let damaged = fsx::read_private(&checkpoint, 40)
        .unwrap()
        .unwrap()
        .as_slice()
        == b"damaged";
    next.insert("damaged", i64::from(damaged));
    if !damaged {
        let floor = retained_history::project_store(
            &super::Reads::new(&paths),
            &fixture.config,
            fixture.new.clone(),
        )
        .unwrap();
        let version = i64::from(floor.log().unwrap().heads() == fixture.new.log().unwrap().heads());
        next.insert("floor", version);
        next.insert("known", state["known"].max(version));
    }
    next
}

#[test]
fn retained_commit_model_conforms_to_real_journal_and_signed_records() {
    let model = aterm_models_swarm::retained_commit_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = retained_commit_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{action:?}: {state:?} -> {actual:?}"
                );
            }
        }
    }
    let mut unpublished = model.init_state();
    unpublished.insert("raw", 1);
    let mut unsafe_read = retained_commit_transition(&unpublished, "Read");
    unsafe_read.insert("seen", 1);
    assert!(!model.check_invariant("OnlyAcknowledgedView", &unsafe_read));
    assert!(
        !model
            .successors("Read", &unpublished)
            .contains(&unsafe_read)
    );
}

pub(crate) fn retained_frontier_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "OtherNetwork" => {
            next.insert("same", 0);
        }
        "LoseAncestor" => {
            next.insert("complete", 0);
        }
        "Admit" => {
            static LOG: std::sync::OnceLock<crate::membership_log::Log> =
                std::sync::OnceLock::new();
            let log = LOG.get_or_init(|| {
                let _answering = ServersAnswering::set();
                let lab = log_entries_tests::two_macs(&[]);
                let log = load_records(&lab.m7).unwrap().log().unwrap().clone();
                lab.server.stop();
                log
            });
            let candidate = if state["complete"] == 1 {
                log.clone()
            } else {
                crate::membership_log::Log::new(log.genesis().clone()).unwrap()
            };
            let network = if state["same"] == 1 {
                *log.network()
            } else {
                [91; 32]
            };
            next.insert("phase", 1);
            next.insert(
                "admitted",
                i64::from(retained_frontier::covers(
                    &candidate,
                    &network,
                    &log.heads(),
                )),
            );
        }
        _ => panic!("unknown retained frontier action {action}"),
    }
    next
}

#[test]
fn retained_frontier_model_conforms_to_signed_native_graphs() {
    let model = aterm_models_swarm::retained_frontier_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = retained_frontier_transition(&state, action.name);
                assert!(model.successors(action.name, &state).contains(&actual));
            }
        }
    }
    let mut missing = model.init_state();
    missing.insert("complete", 0);
    let mut unsafe_result = retained_frontier_transition(&missing, "Admit");
    unsafe_result.insert("admitted", 1);
    assert!(!model.check_invariant("RetainsAncestors", &unsafe_result));
    assert!(!model.successors("Admit", &missing).contains(&unsafe_result));
}

#[test]
fn frontier_deltas_keep_ancestors_and_hide_an_unacknowledged_suffix() {
    use retained_frontier::{Delta, Frontier};

    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let old = load_records(&lab.m22).unwrap().log().unwrap().clone();
    let identity = read_identity(&lab.m7).unwrap().unwrap();
    let (peer, peer_key, incarnation) = log_entries::open_local_member_signer(&lab.m7, &old)
        .unwrap()
        .unwrap();
    let mut frontier = Frontier::empty(*old.network());
    assert!(frontier.is_empty());
    let first = frontier.plan(&old).unwrap().unwrap();
    let encoded = first.encode();
    assert_eq!(Delta::decode(&encoded).unwrap(), first);
    frontier.apply(&first).unwrap();
    let initial = frontier.clone();

    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let new = load_records(&lab.m22).unwrap().log().unwrap().clone();
    let projected = frontier.project(&new).unwrap();
    assert_eq!(projected.heads(), old.heads());
    let entries = projected.open_entries(&identity).unwrap().unwrap();
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.name == "shared")
            .unwrap()
            .value
            .as_slice(),
        b"old"
    );

    let next = frontier.plan(&new).unwrap().unwrap();
    frontier
        .apply(&Delta::decode(&next.encode()).unwrap())
        .unwrap();
    assert_eq!(frontier.heads(), new.heads());
    assert!(frontier.plan(&new).unwrap().is_none());
    assert!(
        frontier.plan(&old).unwrap().is_none(),
        "old input does not lower the floor"
    );
    assert!(frontier.project(&old).is_err());
    assert_ne!(frontier.digest(), initial.digest());

    let retired = old
        .heads()
        .into_iter()
        .find(|id| !frontier.heads().contains(id))
        .unwrap();
    let mut resurrection = b"VFD1".to_vec();
    resurrection.extend_from_slice(&frontier.digest());
    resurrection.extend_from_slice(&1_u32.to_le_bytes());
    resurrection.extend_from_slice(&0_u32.to_le_bytes());
    resurrection.extend_from_slice(&retired);
    let before = frontier.clone();
    assert!(
        frontier
            .apply(&Delta::decode(&resurrection).unwrap())
            .is_err()
    );
    assert_eq!(
        frontier, before,
        "a refused delta must not change the retained frontier"
    );
    assert!(Delta::decode(&encoded[..encoded.len() - 1]).is_err());

    // A second machine can send a legitimate concurrent branch while this disk lacks B.
    let record = peer_key
        .with_seed(|seed| {
            old.draft(
                &peer,
                7_000,
                crate::membership_log::Body::Name {
                    name: "offline-m7".to_owned(),
                },
            )
            .sign(seed, incarnation)
        })
        .unwrap();
    let mut partial = old.clone();
    let _ = partial.insert(record.clone()).unwrap();
    assert!(frontier.plan(&partial).unwrap().is_none());
    assert!(frontier.project(&partial).is_err(), "repair still lacks B");
    let mut joined = new.clone();
    let _ = joined.insert(record).unwrap();
    frontier
        .apply(&frontier.plan(&joined).unwrap().unwrap())
        .unwrap();
    assert!(
        frontier.project(&joined).is_ok(),
        "the complete two-machine join covers both branches"
    );
    lab.server.stop();
}

#[test]
fn a_valid_older_disk_log_cannot_roll_back_an_applied_log_only_value() {
    for legacy in [false, true] {
        let _answering = ServersAnswering::set();
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let (reader, request, allowed) =
            log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
        let (bundle, logged) = log_entries_tests::hand(&lab.m22, &request, &allowed, true);
        assert!(logged);
        admit::write_from_log_bundle(&reader, &bundle).unwrap();
        let old = load_records(&reader).unwrap().encode();
        let admin = admin_open(&lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"new")
            .unwrap();
        admin_publish(&lab.m22, &admin, &doc).unwrap();
        save_records(&reader, &load_records(&lab.m22).unwrap().encode()).unwrap();
        let apply = || {
            apply_response_until(
                &reader,
                configured(&reader).unwrap(),
                load_state(&reader).unwrap(),
                Err(Error::new("offline")),
                None,
                None,
                true,
            )
        };
        assert!(matches!(apply().unwrap(), SyncOutcome::Applied { .. }));
        let value = || crate::client::stored(&reader, "shared");
        assert_eq!(value().as_deref(), Some(b"new".as_slice()));
        assert!(materialization_ready(&reader).unwrap());

        let current = load_records(&reader).unwrap().encode();
        if legacy {
            // Model a pre-receipt client: its applied digest and values survive, but no
            // retained-history files were ever enrolled by that older implementation.
            for entry in std::fs::read_dir(&reader.dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "records.frontiers" || name.starts_with("frontiers.") {
                    std::fs::remove_file(entry.path()).unwrap();
                }
            }
        }

        // A copied older file is still signed correctly; retained history must distinguish it.
        fsx::write_atomic(&reader.records_data_file().unwrap(), &old, 0o600).unwrap();
        assert!(
            log_entries::trusted_configured_log(&reader, &configured(&reader).unwrap()).is_err(),
            "authority reads must reject the rollback before a writer runs (legacy={legacy})"
        );
        assert!(roster::log_labels(&reader).is_err());
        assert!(conflicts(&reader).is_err());
        let drafted = std::cell::Cell::new(false);
        let result = log_entries::author_and_commit_records(&reader, |_| {
            drafted.set(true);
            Ok((Vec::new(), ()))
        });
        assert!(
            result.is_err(),
            "lost history must not authorize local signing"
        );
        assert!(
            !drafted.get(),
            "coverage is checked before invoking a signer"
        );
        let recovered = apply();
        assert_ne!(
            value().as_deref(),
            Some(b"old".as_slice()),
            "a valid older disk snapshot must not replace an acknowledged value (legacy={legacy})"
        );
        assert!(!matches!(recovered, Ok(SyncOutcome::Applied { .. })));
        assert!(!materialization_ready(&reader).unwrap());

        // Restoring the exact acknowledged graph permits enrollment or journal recovery.
        fsx::write_atomic(&reader.records_data_file().unwrap(), &current, 0o600).unwrap();
        drop(StateLock::take(&reader).unwrap_or_else(|error| {
            panic!("the complete frontier must recover (legacy={legacy}): {error}")
        }));
        assert_eq!(value().as_deref(), Some(b"new".as_slice()));
        assert!(materialization_ready(&reader).unwrap());
        let admin = admin_open(&lab.m22).unwrap();
        let mut reverted = admin.doc.clone();
        reverted
            .set_entry(&Audience::Everyone, "shared", b"old")
            .unwrap();
        admin_publish(&lab.m22, &admin, &reverted).unwrap();
        save_records(&reader, &load_records(&lab.m22).unwrap().encode()).unwrap();
        assert!(matches!(apply().unwrap(), SyncOutcome::Applied { .. }));
        assert_eq!(
            value().as_deref(),
            Some(b"old".as_slice()),
            "an intentional signed descendant can revert a value without forgetting its history"
        );
        lab.server.stop();
    }
}

#[test]
fn retained_receipt_separates_raw_publication_from_authority_and_detects_loss() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let old = load_records(&lab.m7).unwrap();
    let config = configured(&lab.m7).unwrap();
    let before = retained_history::version(&lab.m7).unwrap();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let new = load_records(&lab.m22).unwrap();

    // Crash after raw publication, before the acknowledgement: cold reads keep F_old.
    fsx::write_atomic(&lab.m7.records_data_file().unwrap(), &new.encode(), 0o600).unwrap();
    let projected = retained_history::project_store(
        &super::Reads::new(&lab.m7),
        &config,
        load_records(&lab.m7).unwrap(),
    )
    .unwrap();
    assert_eq!(projected.log().unwrap().heads(), old.log().unwrap().heads());
    assert_eq!(retained_history::version(&lab.m7).unwrap(), before);
    log_entries::author_and_commit_records(&lab.m7, |snapshot| {
        assert_eq!(
            snapshot.log().unwrap().heads(),
            old.log().unwrap().heads(),
            "a signer sees the acknowledged prefix, not an uncommitted admission or value"
        );
        Ok((Vec::new(), ()))
    })
    .unwrap();
    // The next exclusive writer acknowledges the same durable raw data without a network.
    drop(StateLock::take(&lab.m7).unwrap());
    let projected = retained_history::project_store(
        &super::Reads::new(&lab.m7),
        &config,
        load_records(&lab.m7).unwrap(),
    )
    .unwrap();
    assert_eq!(projected.log().unwrap().heads(), new.log().unwrap().heads());
    assert_ne!(retained_history::version(&lab.m7).unwrap(), before);

    let checkpoint = lab.m7.dir.join("frontiers.checkpoint");
    let saved = lab.m7.dir.join("frontiers.checkpoint.saved");
    std::fs::rename(&checkpoint, &saved).unwrap();
    assert!(
        retained_history::project_store(&super::Reads::new(&lab.m7), &config, new.clone()).is_err()
    );
    assert!(
        StateLock::take(&lab.m7).is_err(),
        "loss never permits re-adoption"
    );
    std::fs::rename(&saved, &checkpoint).unwrap();
    assert!(retained_history::project_store(&super::Reads::new(&lab.m7), &config, new).is_ok());
    lab.server.stop();
}

#[test]
fn retained_network_survives_label_endpoint_and_endorsed_signer_changes() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[]);
    let store = load_records(&lab.m7).unwrap();
    let network = *store.log().unwrap().network();
    let mut config = configured(&lab.m7).unwrap();
    let original = config.signer.unwrap();
    config.host = "m100".into();
    config.source = Source::Url("http://offline.local/alab".into());
    assert_eq!(
        retained_history::pinned_network(&super::Reads::new(&lab.m7), &config).unwrap(),
        Some(network)
    );
    config.followed.push(original);
    config.signer = Some([71; 32]);
    assert_eq!(
        retained_history::pinned_network(&super::Reads::new(&lab.m7), &config).unwrap(),
        Some(network)
    );
    assert!(retained_history::project_store(&super::Reads::new(&lab.m7), &config, store).is_ok());
    lab.server.stop();
}

#[test]
fn a_bounded_reader_repairs_an_older_raw_snapshot_from_its_peer() {
    let _answering = ServersAnswering::set();
    let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
    let old = load_records(&lab.m7).unwrap().encode();
    let admin = admin_open(&lab.m22).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "shared", b"new")
        .unwrap();
    admin_publish(&lab.m22, &admin, &doc).unwrap();
    let current = load_records(&lab.m22).unwrap();
    save_records(&lab.m7, &current.encode()).unwrap();
    fsx::write_atomic(&lab.m7.records_data_file().unwrap(), &old, 0o600).unwrap();
    assert_eq!(
        log_entries::trusted_configured_log(&lab.m7, &configured(&lab.m7).unwrap())
            .err()
            .unwrap()
            .kind(),
        crate::ErrorKind::MembershipHistoryUnavailable,
    );
    bounded::sync_reader_until(&lab.m7, Instant::now() + Duration::from_secs(60)).unwrap();
    assert_eq!(crate::client::stored(&lab.m7, "shared").unwrap(), b"new");
    assert!(retained_frontier::covers(
        load_records(&lab.m7).unwrap().log().unwrap(),
        current.log().unwrap().network(),
        &current.log().unwrap().heads(),
    ));
    lab.server.stop();
}

/// A Mac configured for [`commit_fixture`]'s vault that wrote each of `stores` in turn through
/// the store's own writer, which makes the records durable and then acknowledges them.
fn acknowledging(stores: &[&crate::membership_log::Store]) -> (aterm_tempfile::TempDir, Paths) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(scratch.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    save_config(&paths, &commit_fixture().config).unwrap();
    for store in stores {
        write_records(&paths, store).unwrap();
    }
    (scratch, paths)
}

/// Replace the records file with `store`, the acknowledged history left as it was: a write cut
/// after it made `store` durable and before it acknowledged it, or a snapshot put back.
fn put_raw(paths: &Paths, store: &crate::membership_log::Store) {
    fsx::write_atomic(&paths.records_data_file().unwrap(), &store.encode(), 0o600).unwrap();
}

fn heads_of(store: &crate::membership_log::Store) -> Vec<crate::membership_log::RecordId> {
    store.log().expect("a log").heads()
}

/// Both authority reads refuse, as the history this Mac acknowledged being unavailable.
fn assert_history_unavailable(paths: &Paths, when: &str) {
    assert_eq!(
        load_acknowledged_records(paths).err().map(|e| e.kind()),
        Some(crate::ErrorKind::MembershipHistoryUnavailable),
        "{when}: load_acknowledged_records"
    );
    assert_eq!(
        load_acknowledged_and_ahead(paths).err().map(|e| e.kind()),
        Some(crate::ErrorKind::MembershipHistoryUnavailable),
        "{when}: load_acknowledged_and_ahead"
    );
}

/// With no vault configured there is no acknowledged history to project onto: the authority
/// read is the raw store, records a cut write left unacknowledged included, with nothing ahead.
#[test]
fn with_no_vault_configured_the_authority_read_is_the_raw_store() {
    let fixture = commit_fixture();
    let (_scratch, paths) = acknowledging(&[&fixture.old]);
    put_raw(&paths, &fixture.new);
    assert_eq!(
        heads_of(&load_acknowledged_records(&paths).unwrap()),
        heads_of(&fixture.old),
        "PRECONDITION: under the vault the read projects onto the acknowledged history"
    );
    std::fs::remove_file(paths.source_file()).unwrap();
    assert!(config(&paths).unwrap().is_none(), "PRECONDITION: no vault");
    let raw = load_records(&paths).unwrap();
    assert_eq!(heads_of(&raw), heads_of(&fixture.new));
    let read = load_acknowledged_and_ahead(&paths).unwrap();
    assert_eq!(read.acknowledged.encode(), raw.encode());
    assert!(read.ahead.is_none(), "the raw store is the one read");
    assert_eq!(
        load_acknowledged_records(&paths).unwrap().encode(),
        raw.encode()
    );
}

/// A store whose every record this Mac acknowledged reads as the raw store, with nothing ahead.
#[test]
fn an_acknowledged_store_reads_as_the_raw_one_with_nothing_ahead() {
    let fixture = commit_fixture();
    let (_scratch, paths) = acknowledging(&[&fixture.old, &fixture.new]);
    let raw = load_records(&paths).unwrap();
    assert_eq!(heads_of(&raw), heads_of(&fixture.new));
    let read = load_acknowledged_and_ahead(&paths).unwrap();
    assert_eq!(read.acknowledged.encode(), raw.encode());
    assert!(
        read.ahead.is_none(),
        "nothing is ahead of an acknowledged store"
    );
    assert_eq!(
        load_acknowledged_records(&paths).unwrap().encode(),
        raw.encode()
    );
}

/// Records a write made durable and was cut short of acknowledging: the authority read is the
/// prefix this Mac acknowledged, and the raw store of the same read is ahead of it, holding every
/// record that prefix holds. The next writer acknowledges them, and nothing is ahead after.
#[test]
fn records_a_cut_write_left_unacknowledged_are_ahead_of_the_acknowledged_store() {
    let fixture = commit_fixture();
    let (_scratch, paths) = acknowledging(&[&fixture.old]);
    put_raw(&paths, &fixture.new);
    let raw = load_records(&paths).unwrap();
    assert_eq!(
        heads_of(&raw),
        heads_of(&fixture.new),
        "PRECONDITION: the raw store holds the write"
    );
    assert_ne!(heads_of(&fixture.old), heads_of(&fixture.new));

    let read = load_acknowledged_and_ahead(&paths).unwrap();
    assert_eq!(
        heads_of(&read.acknowledged),
        heads_of(&fixture.old),
        "the acknowledged store is the prefix this Mac acknowledged"
    );
    let ahead = read
        .ahead
        .expect("the raw store is ahead of the acknowledged one");
    assert_eq!(ahead.encode(), raw.encode(), "ahead is the raw store");
    let ahead_log = ahead.log().unwrap();
    for record in read.acknowledged.log().unwrap().records() {
        assert!(
            ahead_log.get(record.id()).is_some(),
            "the records ahead hold every acknowledged one"
        );
    }
    assert_eq!(
        heads_of(&load_acknowledged_records(&paths).unwrap()),
        heads_of(&fixture.old)
    );

    assert_eq!(
        save_records(&paths, &[]).unwrap(),
        0,
        "the next writer acknowledges the records and has none to add"
    );
    let read = load_acknowledged_and_ahead(&paths).unwrap();
    assert_eq!(heads_of(&read.acknowledged), heads_of(&fixture.new));
    assert!(read.ahead.is_none(), "acknowledged: nothing is ahead");
}

/// The flat layout's records file gone reads raw as an empty store, which the history this Mac
/// acknowledged, holding the vault's genesis, tells from a store with no genesis yet.
#[test]
fn a_records_file_gone_under_acknowledged_history_is_unavailable() {
    let fixture = commit_fixture();
    let (_scratch, paths) = acknowledging(&[&fixture.old]);
    assert!(
        paths.records_file().is_file(),
        "PRECONDITION: the flat layout"
    );
    let bytes = std::fs::read(paths.records_file()).unwrap();
    std::fs::remove_file(paths.records_file()).unwrap();
    assert!(
        load_records(&paths).unwrap().log().is_none(),
        "PRECONDITION: the raw read takes the file gone for a store with no genesis"
    );
    assert_history_unavailable(&paths, "the records file gone");
    fsx::write_atomic(&paths.records_file(), &bytes, 0o600).unwrap();
    assert_eq!(
        heads_of(&load_acknowledged_records(&paths).unwrap()),
        heads_of(&fixture.old),
        "CONTROL: the file back reads"
    );
}

/// A valid older snapshot put back behind the history this Mac acknowledged verifies raw, and
/// the authority read refuses it rather than read the older log.
#[test]
fn an_older_snapshot_put_back_under_acknowledged_history_is_unavailable() {
    let fixture = commit_fixture();
    let (_scratch, paths) = acknowledging(&[&fixture.old, &fixture.new]);
    put_raw(&paths, &fixture.old);
    assert_eq!(
        heads_of(&load_records(&paths).unwrap()),
        heads_of(&fixture.old),
        "PRECONDITION: the older snapshot verifies and reads raw"
    );
    assert_history_unavailable(&paths, "an older snapshot put back");
    put_raw(&paths, &fixture.new);
    assert_eq!(
        heads_of(&load_acknowledged_records(&paths).unwrap()),
        heads_of(&fixture.new),
        "CONTROL: the acknowledged snapshot back reads"
    );
}

/// A Mac from before retained-history receipts, as an older build left it: it applied a value
/// from its membership log, its state names that log's heads digest, and no receipt was ever
/// enrolled. The vault has since moved on without it.
struct PreReceipt {
    lab: log_entries_tests::TwoMacs,
    reader: Paths,
    /// The reader's records before the admin signed anything more: a valid snapshot strictly
    /// inside the graph it applied.
    older: Vec<u8>,
    /// Where the applied envelope is stamped ([`PreReceipt::stamped`]), the records the reader
    /// held when it applied that envelope: every record its stamp names, and not the put the
    /// reader applied from its log afterwards. Otherwise `older`.
    stamp_held: Vec<u8>,
    /// The store it last applied from.
    applied: crate::membership_log::Store,
    /// The admin's records once it signed `newer`: `applied`'s and the put after them.
    newer: Vec<u8>,
    /// A record another Mac signed over the log it held before any of that, concurrent with it,
    /// chosen so a reload of [`PreReceipt::grown`] sorts it among the applied records: the
    /// applied graph is then no prefix of the order the store holds.
    concurrent: crate::membership_log::Record,
    _answering: ServersAnswering,
}

impl PreReceipt {
    /// A Mac the log let in, which holds no envelope locker: nothing it applied is stamped.
    fn build() -> Self {
        Self::build_as(false)
    }

    /// A Mac with a locker: it applied `new` from an envelope stamped with the log's heads, then
    /// `mid` from its log alone, so the stamp names records short of the log it applied.
    fn stamped() -> Self {
        Self::build_as(true)
    }

    fn build_as(stamped: bool) -> Self {
        let answering = ServersAnswering::set();
        let lab = log_entries_tests::two_macs(&[(Audience::Everyone, "shared", b"old")]);
        let (m8, request, allowed) = log_entries_tests::newcomer(lab.root.path(), &lab.m22, "m8");
        let (bundle, logged) = log_entries_tests::hand(&lab.m22, &request, &allowed, true);
        assert!(logged);
        admit::write_from_log_bundle(&m8, &bundle).unwrap();
        let (reader, other) = if stamped {
            (lab.m7.clone(), m8)
        } else {
            (m8, lab.m7.clone())
        };
        let older = load_records(&reader).unwrap().encode();
        let base = load_records(&other).unwrap().log().unwrap().clone();
        let (peer, peer_key, incarnation) = log_entries::open_local_member_signer(&other, &base)
            .unwrap()
            .unwrap();
        let publish = |value: &[u8]| {
            let admin = admin_open(&lab.m22).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, "shared", value).unwrap();
            admin_publish(&lab.m22, &admin, &doc).unwrap();
            load_records(&lab.m22).unwrap().encode()
        };
        save_records(&reader, &publish(b"new")).unwrap();
        let (stamp_held, value) = if stamped {
            sync(&reader, None).unwrap();
            assert_eq!(
                stored(&reader, "shared").as_deref(),
                Some(b"new".as_slice())
            );
            assert!(
                log_entries::applied_version_names_log(&reader).unwrap(),
                "FIXTURE: the applied envelope is stamped"
            );
            let held = load_records(&reader).unwrap().encode();
            save_records(&reader, &publish(b"mid")).unwrap();
            (held, b"mid".as_slice())
        } else {
            (older.clone(), b"new".as_slice())
        };
        let mut this = Self {
            applied: load_records(&reader).unwrap(),
            newer: Vec::new(),
            concurrent: base.genesis().clone(),
            lab,
            reader,
            older,
            stamp_held,
            _answering: answering,
        };
        assert!(matches!(this.apply().unwrap(), SyncOutcome::Applied { .. }));
        assert_eq!(this.value().as_deref(), Some(value));
        assert_eq!(
            log_entries::applied_version_names_log(&this.reader).unwrap(),
            stamped,
            "FIXTURE: an apply from the log alone keeps the envelope"
        );
        this.forget_receipts();
        let admin = admin_open(&this.lab.m22).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, "shared", b"newer")
            .unwrap();
        admin_publish(&this.lab.m22, &admin, &doc).unwrap();
        this.newer = load_records(&this.lab.m22).unwrap().encode();
        let applied_heads = heads_of(&this.applied);
        // A record's id is a hash, and a reload orders records by id wherever their parents allow
        // ([`crate::membership_log::store::causal`]): a record drafted over `base` is released
        // beside the chain the reader applied past `base`, and ahead of that chain's end exactly
        // when its id is below one of the chain's. Every draft races the same chain, so a fixed
        // number of drafts fails whenever that chain's ids all sit low (64 drafts against two
        // records: one build in 2,145). Sign drafts until one is below the chain's highest id:
        // two drafts on average, and past 65,536 in one build of 2^32.
        let in_base: std::collections::BTreeSet<_> = base.records().map(|r| *r.id()).collect();
        let ceiling = this
            .applied
            .log()
            .unwrap()
            .records()
            .map(|r| *r.id())
            .filter(|id| !in_base.contains(id))
            .max()
            .expect("FIXTURE: the reader applied records past what the other Mac held");
        this.concurrent = (0_u64..)
            .map(|n| {
                peer_key
                    .with_seed(|seed| {
                        base.draft(
                            &peer,
                            7_000 + n,
                            crate::membership_log::Body::Name {
                                name: format!("offline-{n}"),
                            },
                        )
                        .sign(seed, incarnation)
                    })
                    .unwrap()
            })
            .find(|record| *record.id() < ceiling)
            .expect("an endless search ends only with a find");
        let reloaded = crate::membership_log::Store::with_compiled(Some(
            *this.applied.log().unwrap().network(),
        ))
        .recovering(&this.grown().encode());
        assert!(
            !heads_of_a_prefix(reloaded.log().unwrap(), &applied_heads),
            "FIXTURE: the concurrent record sorts among the applied ones"
        );
        this
    }

    /// The store an older build left: the applied digest and values stay, no receipt does.
    fn forget_receipts(&self) {
        for entry in std::fs::read_dir(&self.reader.dir).unwrap() {
            let entry = entry.unwrap();
            if receipt_file(&entry.file_name().to_string_lossy()) {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
    }

    /// The names of the receipt files this Mac's vault folder holds.
    fn receipt_files(&self) -> Vec<String> {
        std::fs::read_dir(&self.reader.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| receipt_file(name))
            .collect()
    }

    /// `applied` with every record made since: the admin's `newer` and m7's concurrent record.
    fn grown(&self) -> crate::membership_log::Store {
        let mut grown = self.applied.clone();
        grown.ingest(&self.newer).unwrap();
        assert!(grown.insert(self.concurrent.clone()).unwrap());
        grown
    }

    /// Make the records file hold `store` as an older build's intake did: no state lock taken,
    /// so no writer of this build has seen it. Returns the store a read of the file loads.
    fn put(&self, store: &[u8]) -> crate::membership_log::Store {
        fsx::write_atomic(&self.reader.records_data_file().unwrap(), store, 0o600).unwrap();
        load_records(&self.reader).unwrap()
    }

    fn apply(&self) -> crate::Result<SyncOutcome> {
        apply_response_until(
            &self.reader,
            configured(&self.reader).unwrap(),
            load_state(&self.reader).unwrap(),
            Err(Error::new("offline")),
            None,
            None,
            true,
        )
    }

    fn value(&self) -> Option<Vec<u8>> {
        crate::client::stored(&self.reader, "shared")
    }

    /// The lock-free authority read every reader makes.
    fn read(&self) -> crate::Result<Option<log_entries::ConfiguredLog>> {
        log_entries::trusted_configured_log(&self.reader, &configured(&self.reader).unwrap())
    }

    fn applied_digest(&self) -> Option<[u8; 32]> {
        load_state(&self.reader).unwrap().records_digest
    }
}

fn receipt_file(name: &str) -> bool {
    name == "records.frontiers" || name.starts_with("frontiers.")
}

/// Whether `heads` are the heads of the first records of `log`, for some count of them, in the
/// order it holds them.
fn heads_of_a_prefix(
    log: &crate::membership_log::Log,
    heads: &[crate::membership_log::RecordId],
) -> bool {
    let mut open = std::collections::BTreeSet::new();
    log.records().any(|record| {
        for parent in record.parents() {
            open.remove(parent);
        }
        open.insert(*record.id());
        open.iter().eq(heads)
    })
}

/// The bug of 2026-11-15: records landed after this Mac's last apply and before its first
/// receipt, so its applied digest named a strict part of what it held, no enrollment matched it,
/// and every read was refused until a person repaired it. The store holds the applied graph
/// whole, which the applied heads show: the first writer enrolls it and the pass applies.
#[test]
fn a_pre_receipt_mac_whose_records_grew_past_its_applied_log_enrolls_without_repair() {
    let mac = PreReceipt::build();
    let grown = mac.put(&mac.grown().encode());
    let (grown_heads, grown_digest) = (heads_of(&grown), grown.heads_digest().unwrap());
    assert_ne!(
        mac.applied_digest(),
        Some(grown_digest),
        "FIXTURE: the store grew past the applied log"
    );
    assert_eq!(
        mac.applied_digest(),
        mac.applied.heads_digest(),
        "FIXTURE: the state names the log it applied"
    );
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        None,
        "FIXTURE: no receipt was ever enrolled"
    );
    assert!(
        !heads_of_a_prefix(grown.log().unwrap(), &heads_of(&mac.applied)),
        "FIXTURE: the applied graph is no prefix of the order a reload holds"
    );

    // Before any writer: a reader refuses, and writes nothing.
    let refused = mac.read().err().expect("a reader enrolls nothing");
    assert_eq!(
        refused.kind(),
        crate::ErrorKind::MembershipHistoryUnavailable
    );
    assert_history_unavailable(&mac.reader, "before a writer");
    assert_eq!(
        mac.receipt_files(),
        Vec::<String>::new(),
        "a lock-free read wrote a receipt"
    );

    // One exclusive take, which every vault pass makes.
    drop(StateLock::take(&mac.reader).unwrap());
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(true),
        "the first writer enrolls a store that holds the applied graph"
    );
    assert_eq!(mac.read().unwrap().expect("a log").log.heads(), grown_heads);
    assert!(matches!(mac.apply().unwrap(), SyncOutcome::Applied { .. }));
    assert_eq!(mac.value().as_deref(), Some(b"newer".as_slice()));
    assert_eq!(mac.applied_digest(), Some(grown_digest));
    assert!(
        refused.to_string().contains("aterm secrets sync"),
        "the refusal names the pass that ends it: {refused}"
    );
    mac.lab.server.stop();
}

impl PreReceipt {
    /// One exclusive take of the state, as every vault pass makes; whether a receipt is ready
    /// after it.
    fn writer_pass(&self) -> bool {
        drop(StateLock::take(&self.reader).unwrap());
        retained_history::claim_ready(&self.reader).unwrap() == Some(true)
    }

    /// The genesis alone: a verified store short of every record the applied envelope names.
    fn genesis_only(&self) -> Vec<u8> {
        let log = self.applied.log().unwrap();
        let mut store = crate::membership_log::Store::with_compiled(Some(*log.network()));
        assert!(store.insert(log.genesis().clone()).unwrap());
        store.encode()
    }

    /// What the records on disk are to the log this Mac applied, as `VaultFirstEnrollment` names
    /// it: 1 that log, 2 that log and more, 3 short of it but not of the records the applied
    /// envelope was written at, 0 short of those too.
    fn raw(&self) -> i64 {
        let held = load_records(&self.reader).unwrap();
        let log = held.log().unwrap();
        let applied = heads_of(&self.applied);
        if log.heads() == applied {
            1
        } else if retained_frontier::covers(log, log.network(), &applied) {
            2
        } else if self.covers_applied_stamp(&held) {
            3
        } else {
            0
        }
    }

    /// Records that are `raw` to the applied log ([`Self::raw`]).
    fn records_at(&self, raw: i64) -> Vec<u8> {
        match raw {
            0 => self.genesis_only(),
            1 => self.applied.encode(),
            2 => self.grown().encode(),
            _ => self.stamp_held.clone(),
        }
    }

    fn covers_applied_stamp(&self, store: &crate::membership_log::Store) -> bool {
        log_entries::covers_applied_stamp(
            &super::Reads::new(&self.reader),
            &configured(&self.reader).unwrap(),
            store.log().unwrap(),
        )
        .unwrap()
    }
}

/// Records that lack part of the log this Mac applied are never enrolled, whatever else they
/// hold. The snapshot this Mac held when it applied its envelope holds every record the stamp
/// names and lacks only the put applied from the log afterwards, so a check of the stamp alone
/// would enroll it and the next apply would write the older value back; the same with a valid
/// record of another branch beside it; a snapshot from before the envelope and the genesis alone
/// are short of the stamp too, and are refused with nothing hashed.
#[test]
fn pre_receipt_records_that_lack_part_of_the_applied_log_are_never_enrolled() {
    let mac = PreReceipt::stamped();
    let network = *mac.applied.log().unwrap().network();
    let recover =
        |bytes: &[u8]| crate::membership_log::Store::with_compiled(Some(network)).recovering(bytes);
    let stamp_held = recover(&mac.stamp_held);
    assert!(
        mac.covers_applied_stamp(&stamp_held),
        "FIXTURE: it holds the stamp's records, so only the applied heads refuse it"
    );
    assert!(
        !retained_frontier::covers(stamp_held.log().unwrap(), &network, &heads_of(&mac.applied)),
        "FIXTURE: it lacks the put applied after the envelope"
    );
    let mut beside = stamp_held.clone();
    assert!(beside.insert(mac.concurrent.clone()).unwrap());
    for short in [mac.older.clone(), mac.genesis_only()] {
        assert!(
            !mac.covers_applied_stamp(&recover(&short)),
            "FIXTURE: short of the stamp"
        );
    }
    for (what, store, searched) in [
        ("the stamp's records", mac.stamp_held.clone(), 1),
        ("the stamp's records and another branch", beside.encode(), 1),
        ("a snapshot from before the envelope", mac.older.clone(), 0),
        ("the genesis alone", mac.genesis_only(), 0),
    ] {
        mac.put(&store);
        let before = log_entries::WITNESS_SEARCHES.get();
        assert!(!mac.writer_pass(), "{what}: enrolled");
        assert_eq!(
            log_entries::WITNESS_SEARCHES.get() - before,
            searched,
            "{what}: searches of the held records"
        );
        assert_eq!(mac.receipt_files(), Vec::<String>::new(), "{what}");
        let refused = mac.read().err().unwrap_or_else(|| panic!("{what}: read"));
        assert_eq!(
            refused.kind(),
            crate::ErrorKind::MembershipHistoryUnavailable,
            "{what}"
        );
        // What ends the refusal: the explicit repair only where it would accept these records.
        assert_eq!(
            refused.to_string().contains("aterm secrets repair"),
            searched == 1,
            "{what}: {refused}"
        );
        assert!(
            !matches!(mac.apply(), Ok(SyncOutcome::Applied { .. })),
            "{what}: applied"
        );
        assert_eq!(mac.value(), None, "{what}: a value read");
        assert_eq!(
            mac.applied_digest(),
            mac.applied.heads_digest(),
            "{what}: the applied log moved"
        );
        assert_eq!(mac.receipt_files(), Vec::<String>::new(), "{what}");
    }
    // CONTROL: the same Mac, the same passes, records that hold the applied log.
    mac.put(&mac.grown().encode());
    assert!(mac.writer_pass());
    assert!(matches!(mac.apply().unwrap(), SyncOutcome::Applied { .. }));
    assert_eq!(mac.value().as_deref(), Some(b"newer".as_slice()));
    mac.lab.server.stop();
}

/// A search that ends at its budget proves nothing, so nothing is enrolled, and its answer is
/// kept: the next pass over the same records hashes nothing again, in this process (which
/// remembers it) and in the next (which reads it beside the state), and a reader makes no take
/// for records a search already refused. A kept answer that does not read is no answer: the
/// pass searches again and fails nothing. A search under another budget is another search, and
/// with the budget back it finds the applied heads.
///
/// NEGATIVE CONTROL (2026-11-16): with [`log_entries::legacy_applied_held`] keeping no miss
/// beside the state, `a second process searched again` fails, 2 searches for 1.
#[test]
fn pre_receipt_search_past_its_budget_is_refused_and_remembered() {
    let mac = PreReceipt::build();
    mac.put(&mac.grown().encode());
    let misses = mac.reader.dir.join("records.frontier-misses");
    log_entries::TEST_WITNESS_BUDGET.set(Some(0));
    let before = log_entries::WITNESS_SEARCHES.get();
    let searches = || log_entries::WITNESS_SEARCHES.get() - before;
    assert!(!misses.exists(), "FIXTURE: no search ran yet");
    assert!(!mac.writer_pass(), "a search of no work found the heads");
    assert_eq!(searches(), 1);
    let refused = mac.read().err().expect("unenrolled");
    assert_eq!(
        refused.kind(),
        crate::ErrorKind::MembershipHistoryUnavailable
    );
    assert!(
        refused.to_string().contains("bounded search")
            && refused.to_string().contains("may lack records it applied"),
        "the refusal says the search was cut short, and what the repair it names accepts: \
         {refused}"
    );
    // The same process: the answer is the remembered one.
    assert!(!mac.writer_pass());
    assert_eq!(searches(), 1, "a pass over the same records searched again");
    // Another process remembers nothing and reads the miss kept beside the state.
    log_entries::forget_witnesses();
    assert!(!mac.writer_pass());
    assert_eq!(searches(), 1, "a second process searched again");
    log_entries::forget_witnesses();
    let again = mac.read().err().expect("unenrolled");
    assert_eq!(
        again.to_string(),
        refused.to_string(),
        "a reader of a second process says what the search found"
    );
    // A reader makes no take while that answer stands.
    log_entries::forget_witnesses();
    let minute = || Instant::now() + Duration::from_secs(60);
    assert!(
        !StateLock::first_enrollment_due(&mac.reader, &again, minute()),
        "a read made the take a kept miss already answered"
    );
    assert_eq!(searches(), 1);

    // A kept answer that does not read is no answer, and no refusal of its own.
    for damaged in [&b"not a misses file"[..], &[0; 4096][..]] {
        fsx::write_atomic(&misses, damaged, 0o600).unwrap();
        log_entries::forget_witnesses();
        let searched = searches();
        assert!(
            StateLock::first_enrollment_due(&mac.reader, &again, minute()),
            "CONTROL: with no answer kept, the read makes the take"
        );
        assert_eq!(searches(), searched + 1, "the take searched");
        log_entries::forget_witnesses();
        assert!(!mac.writer_pass());
        assert_eq!(searches(), searched + 1, "the search's miss was kept again");
    }
    assert_eq!(mac.receipt_files(), Vec::<String>::new());

    // The budget back: another key, so the search runs, finds the heads and enrolls.
    log_entries::TEST_WITNESS_BUDGET.set(None);
    log_entries::forget_witnesses();
    let searched = searches();
    assert!(
        mac.writer_pass(),
        "a miss under another budget was believed"
    );
    assert_eq!(searches(), searched + 1);
    mac.lab.server.stop();
}

/// A kept answer refuses and never enrolls: `Held` is shown by a search in each process. A
/// file beside the state that claims the applied log is held (the byte no build writes), or
/// that claims a miss, leaves records that lack part of the applied log unenrolled, and a
/// claimed miss costs records that hold it only until the claim is gone.
#[test]
fn pre_receipt_kept_answer_never_enrolls_and_a_false_miss_only_refuses() {
    let mac = PreReceipt::stamped();
    let short = mac.put(&mac.stamp_held);
    for claim in [0, 1, 2, 3] {
        log_entries::forget_witnesses();
        log_entries::forge_kept_answer(&mac.reader, short.log().unwrap(), claim);
        assert!(!mac.writer_pass(), "a kept answer {claim} enrolled");
        assert_eq!(mac.receipt_files(), Vec::<String>::new(), "{claim}");
        assert!(mac.read().is_err(), "{claim}");
    }
    // A false miss for records that hold the applied log: refused, with no search and no take.
    let grown = mac.put(&mac.grown().encode());
    log_entries::forget_witnesses();
    log_entries::forge_kept_answer(&mac.reader, grown.log().unwrap(), 1);
    let before = log_entries::WITNESS_SEARCHES.get();
    assert!(!mac.writer_pass(), "FIXTURE: the kept miss is believed");
    assert_eq!(log_entries::WITNESS_SEARCHES.get(), before);
    std::fs::remove_file(mac.reader.dir.join("records.frontier-misses")).unwrap();
    log_entries::forget_witnesses();
    assert!(
        mac.writer_pass(),
        "CONTROL: with the claim gone a search enrolls"
    );
    mac.lab.server.stop();
}

/// A store put back to an older snapshot is refused, and heals by itself once intake brings the
/// records back: the write enrolls what it made durable, and the pass applies the newest value.
#[test]
fn pre_receipt_rollback_then_intake_of_the_full_store_heals_to_the_newest_value() {
    let mac = PreReceipt::build();
    mac.put(&mac.older);
    assert!(!mac.writer_pass());
    assert!(mac.read().is_err());
    let grown = mac.grown();
    save_records(&mac.reader, &grown.encode()).unwrap();
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(true),
        "the intake that restored the applied log enrolled it"
    );
    assert!(matches!(mac.apply().unwrap(), SyncOutcome::Applied { .. }));
    assert_eq!(mac.value().as_deref(), Some(b"newer".as_slice()));
    assert_eq!(mac.applied_digest(), grown.heads_digest());
    mac.lab.server.stop();
}

/// The explicit repair accepts the records this Mac holds only where they hold what the applied
/// envelope was written at: a store short of the stamp is refused with the state as it was, since
/// a reader after the repair checks the digest alone. CONTROL: the records the stamp names,
/// short of a later put, are accepted, the prior state kept beside the new one.
#[test]
fn pre_receipt_repair_refuses_records_short_of_the_applied_stamp() {
    let mac = PreReceipt::stamped();
    let state = std::fs::read(mac.reader.state_file()).unwrap();
    let backup = mac.reader.dir.join("state.before-frontier-repair");
    for short in [mac.older.clone(), mac.genesis_only()] {
        let short = mac.put(&short);
        assert!(
            !mac.covers_applied_stamp(&short),
            "FIXTURE: short of the stamp"
        );
        let refused = repair_legacy_frontier(&mac.reader, Instant::now() + Duration::from_secs(60))
            .expect_err("a store short of the stamp was accepted");
        assert_eq!(
            refused.kind(),
            crate::ErrorKind::MembershipHistoryUnavailable
        );
        assert_eq!(std::fs::read(mac.reader.state_file()).unwrap(), state);
        assert!(!backup.exists(), "a refused repair kept a state");
        assert_eq!(mac.receipt_files(), Vec::<String>::new());
    }

    let held = mac.put(&mac.stamp_held);
    assert!(mac.covers_applied_stamp(&held));
    assert_ne!(mac.applied_digest(), held.heads_digest());
    assert!(
        repair_legacy_frontier(&mac.reader, Instant::now() + Duration::from_secs(60)).unwrap(),
        "CONTROL: the explicit door accepts records that hold the stamp's"
    );
    assert_eq!(mac.applied_digest(), held.heads_digest());
    assert_eq!(std::fs::read(&backup).unwrap(), state);
    mac.lab.server.stop();
}

/// End to end: the first subscriber pass over the grown store enrolls it and applies, with no
/// refusal for its log.
#[test]
fn pre_receipt_first_subscriber_pass_over_grown_records_applies() {
    let mac = PreReceipt::build();
    let grown = mac.put(&mac.grown().encode());
    assert!(mac.read().is_err(), "FIXTURE: refused before the pass");
    let (_, outcome) = subscriber_pass(&mac.reader, &mut None, &FOUND_NONE)
        .unwrap_or_else(|refusal| panic!("the pass logs a refusal: {refusal}"));
    assert!(
        matches!(outcome, SyncOutcome::Applied { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(true)
    );
    assert_eq!(mac.value().as_deref(), Some(b"newer".as_slice()));
    assert!(
        retained_frontier::covers(
            load_acknowledged_records(&mac.reader)
                .unwrap()
                .log()
                .unwrap(),
            grown.log().unwrap().network(),
            &heads_of(&grown),
        ),
        "the pass acknowledged every record the store held"
    );
    mac.lab.server.stop();
}

/// With no subscriber running, a person's read is the pass: `aterm secrets get` and git's
/// credential helper read through [`read_in_place`], which enrolls and applies before it
/// answers. A store that lacks part of the applied log gets no such pass: the read refuses and
/// writes no receipt.
#[test]
fn pre_receipt_read_in_place_enrolls_and_applies_with_no_writer_running() {
    let mac = PreReceipt::build();
    mac.put(&mac.older);
    let refused = read_in_place(&mac.reader, "shared").expect_err("a rollback read");
    assert_eq!(
        refused.kind(),
        crate::ErrorKind::MembershipHistoryUnavailable
    );
    assert_eq!(mac.receipt_files(), Vec::<String>::new());

    let grown = mac.put(&mac.grown().encode());
    assert_eq!(
        read_in_place(&mac.reader, "shared")
            .unwrap()
            .as_ref()
            .map(SecretBytes::as_slice),
        Some(b"newer".as_slice())
    );
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(true)
    );
    assert!(retained_frontier::covers(
        load_acknowledged_records(&mac.reader)
            .unwrap()
            .log()
            .unwrap(),
        grown.log().unwrap().network(),
        &heads_of(&grown),
    ));
    mac.lab.server.stop();
}

/// Every in-place entry read that takes its own observation is that pass, not `read_in_place`
/// alone: the waiting reads ([`read_verified_entry_until`], [`read_verified_set`]) and the ones
/// that wait for no writer ([`read_verified_entry`], [`observe_verified_entry`]).
#[test]
fn pre_receipt_entry_reads_enroll_with_no_writer_running() {
    let mac = PreReceipt::build();
    let config = configured(&mac.reader).unwrap();
    let value = |entry: VerifiedEntry| entry.value().as_slice().to_vec();
    let enrolls = |reader: &str, read: &dyn Fn() -> crate::Result<Option<Vec<u8>>>| {
        mac.forget_receipts();
        mac.put(&mac.grown().encode());
        assert_eq!(
            retained_history::claim_ready(&mac.reader).unwrap(),
            None,
            "FIXTURE {reader}: no receipt"
        );
        assert!(mac.read().is_err(), "FIXTURE {reader}: refused unenrolled");
        assert_eq!(
            read().unwrap().as_deref(),
            Some(b"newer".as_slice()),
            "{reader}"
        );
        assert_eq!(
            retained_history::claim_ready(&mac.reader).unwrap(),
            Some(true),
            "{reader}: the read enrolled"
        );
    };
    enrolls("read_verified_entry", &|| {
        Ok(read_verified_entry(&mac.reader, &config, "shared")?.map(value))
    });
    enrolls("read_verified_entry_until", &|| {
        let minute = Instant::now() + Duration::from_secs(60);
        Ok(read_verified_entry_until(&mac.reader, &config, "shared", minute)?.map(value))
    });
    enrolls("read_verified_set", &|| {
        Ok(read_verified_set(&mac.reader, &config, &["shared"])?
            .and_then(|mut set| set.pop())
            .map(value))
    });
    enrolls("observe_verified_entry", &|| {
        Ok(observe_verified_entry(&mac.reader, &config, "shared")?
            .and_then(|seen| seen.entry)
            .map(value))
    });
    // The authority read a writer makes under its own lock takes none and enrolls nothing.
    mac.forget_receipts();
    mac.put(&mac.grown().encode());
    assert_history_unavailable(&mac.reader, "the lock-free authority read");
    assert_eq!(mac.receipt_files(), Vec::<String>::new());
    mac.lab.server.stop();
}

/// A first enrollment cut after its pending claim and before its receipt is recovered by a read
/// as by a vault pass: the claim stands over an empty journal, every reader is refused, and
/// the read's take finishes the enrollment.
///
/// NEGATIVE CONTROL (2026-11-16): with a read's take due only where no claim stands, the read
/// is refused.
#[test]
fn pre_receipt_read_in_place_recovers_a_first_enrollment_cut_before_its_receipt() {
    let mac = PreReceipt::build();
    mac.put(&mac.grown().encode());
    retained_history::cut_first_enrollment(&mac.reader);
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(false),
        "FIXTURE: a pending claim"
    );
    assert_history_unavailable(&mac.reader, "a pending claim over no receipt");
    assert_eq!(
        read_in_place(&mac.reader, "shared")
            .unwrap()
            .as_ref()
            .map(SecretBytes::as_slice),
        Some(b"newer".as_slice())
    );
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(true)
    );
    mac.lab.server.stop();
}

/// A read's take waits out another reader: an observation held as the take first tries the lock
/// and dropped while it waits costs the read nothing.
///
/// NEGATIVE CONTROL (2026-11-16): with the take tried once and any busy lock read as a writer's
/// (`first_enrollment_due` as it was first written), the read is refused, with no pass running
/// and nothing wrong.
#[test]
fn pre_receipt_read_in_place_waits_out_a_passing_observer() {
    let mac = PreReceipt::build();
    mac.put(&mac.grown().encode());
    let observer = std::rc::Rc::new(std::cell::RefCell::new(Some(
        StateLock::observe(&mac.reader)
            .unwrap()
            .expect("an observation"),
    )));
    let blocked = std::rc::Rc::new(std::cell::Cell::new(0));
    let (held, counted) = (std::rc::Rc::clone(&observer), std::rc::Rc::clone(&blocked));
    // A minute is the hang detector of the take's wait and the read's, never reached.
    let read = with_current_wait(Duration::from_secs(60), || {
        while_take_blocked(
            move || {
                counted.set(counted.get() + 1);
                drop(held.borrow_mut().take());
            },
            || read_in_place(&mac.reader, "shared"),
        )
    });
    assert_eq!(
        read.unwrap().as_ref().map(SecretBytes::as_slice),
        Some(b"newer".as_slice())
    );
    assert_eq!(blocked.get(), 1, "FIXTURE: the take found the observation");
    assert_eq!(
        retained_history::claim_ready(&mac.reader).unwrap(),
        Some(true)
    );
    mac.lab.server.stop();
}

/// What a read's take does with a lock still held at the end of its wait. Observers alone
/// enroll nothing, so the refusal stands and the read is not made again; a writer's own take
/// enrolled, so the read is made again and waits for it; and a thread that observes the state
/// across its reads makes no take at all.
#[test]
fn pre_receipt_read_take_tells_an_observer_from_a_writer_at_the_end_of_its_wait() {
    let mac = PreReceipt::build();
    mac.put(&mac.grown().encode());
    let refused = mac.read().err().expect("FIXTURE: refused unenrolled");
    let now = Instant::now;

    // Under this thread's own observation across reads: no take, and no wait for one.
    let observing = Observing::new(
        StateLock::observe(&mac.reader)
            .unwrap()
            .expect("an observation"),
        &mac.reader,
    );
    let takes = std::rc::Rc::new(std::cell::Cell::new(0));
    let counted = std::rc::Rc::clone(&takes);
    assert!(!while_take_blocked(
        move || counted.set(counted.get() + 1),
        || StateLock::first_enrollment_due(&mac.reader, &refused, now() + Duration::from_secs(60))
    ));
    assert_eq!(
        takes.get(),
        0,
        "a take was tried under this thread's observation"
    );
    drop(observing);

    // An observer that outlasts the wait: the first refusal is the answer.
    let observer = StateLock::observe(&mac.reader)
        .unwrap()
        .expect("an observation");
    let read = with_current_wait(Duration::ZERO, || read_in_place(&mac.reader, "shared"));
    assert_eq!(
        read.err().map(|refused| refused.kind()),
        Some(crate::ErrorKind::MembershipHistoryUnavailable)
    );
    assert_eq!(mac.receipt_files(), Vec::<String>::new());

    // A writer that takes the lock while the read's take waits, and holds it past the wait:
    // the read is made again, and finds the state busy rather than its records refused.
    let observer = std::rc::Rc::new(std::cell::RefCell::new(Some(observer)));
    let writer = std::rc::Rc::new(std::cell::RefCell::new(None));
    let (held, taken, paths) = (
        std::rc::Rc::clone(&observer),
        std::rc::Rc::clone(&writer),
        mac.reader.clone(),
    );
    let read = with_current_wait(Duration::ZERO, || {
        while_take_blocked(
            move || {
                drop(held.borrow_mut().take());
                *taken.borrow_mut() = Some(StateLock::take(&paths).unwrap());
            },
            || read_in_place(&mac.reader, "shared"),
        )
    });
    let busy = read.expect_err("the writer still held the state");
    assert_ne!(
        busy.kind(),
        crate::ErrorKind::MembershipHistoryUnavailable,
        "{busy}"
    );
    assert!(busy.to_string().contains("stayed busy"), "{busy}");
    drop(
        writer
            .borrow_mut()
            .take()
            .expect("FIXTURE: the writer took"),
    );
    assert_eq!(
        read_in_place(&mac.reader, "shared")
            .unwrap()
            .as_ref()
            .map(SecretBytes::as_slice),
        Some(b"newer".as_slice()),
        "the writer's take enrolled"
    );
    mac.lab.server.stop();
}

/// One step of `VaultFirstEnrollment` on a real Mac: its receipts and records set as `state`
/// says, then `action` (a writer's exclusive take for `Enroll`, the records file rewritten for
/// the environment's), then what the files show.
fn first_enrollment_transition(
    mac: &PreReceipt,
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    mac.forget_receipts();
    if state["enrolled"] == 1 {
        mac.put(&mac.records_at(1));
        assert!(mac.writer_pass(), "FIXTURE: the applied log itself enrolls");
    }
    mac.put(&mac.records_at(state["raw"]));
    assert_eq!(mac.raw(), state["raw"], "FIXTURE: the records `raw` names");
    let mut next = state.clone();
    match action {
        "Intake" | "Restore" => drop(mac.put(&mac.records_at(2))),
        "Rollback" => drop(mac.put(&mac.records_at(0))),
        "Truncate" => drop(mac.put(&mac.records_at(3))),
        "Enroll" => {
            let enrolled = mac.writer_pass();
            if state["raw"] == 2 && !enrolled {
                next.insert("refused", 1);
            }
        }
        _ => panic!("unknown first enrollment action {action}"),
    }
    let enrolled = retained_history::claim_ready(&mac.reader).unwrap() == Some(true);
    let floor = if enrolled {
        let acknowledged = retained_history::project_store(
            &super::Reads::new(&mac.reader),
            &configured(&mac.reader).unwrap(),
            mac.grown(),
        )
        .unwrap();
        let log = acknowledged.log().unwrap();
        if retained_frontier::covers(log, log.network(), &heads_of(&mac.applied)) {
            1
        } else {
            2
        }
    } else {
        0
    };
    next.insert("raw", mac.raw());
    next.insert("enrolled", i64::from(enrolled));
    next.insert("floor", floor);
    next
}

#[test]
fn pre_receipt_first_enrollment_model_conforms_to_a_real_writer_and_signed_records() {
    let model = aterm_models_swarm::first_enrollment_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    assert_eq!(
        aterm_spec::verify::audit_liveness(
            &model,
            &aterm_models_swarm::first_enrollment_liveness()
        ),
        Ok(1)
    );
    let mac = PreReceipt::stamped();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = first_enrollment_transition(&mac, &state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{action:?}: {state:?} -> {actual:?}"
                );
            }
        }
    }
    // NEGATIVE CONTROLS: each mutant's step is one the real writer does not take. Over records
    // short of the applied log it enrolls nothing, where the mutants acknowledge a floor that
    // lacks part of it; over records that hold it and more it enrolls, where the equality rule
    // refuses.
    let mutant = aterm_spec::interp::with_buggy(&model, 1);
    for (raw, action) in [(3, "EnrollByStamp"), (0, "EnrollAny")] {
        let mut short = model.init_state();
        short.insert("raw", raw);
        assert!(!model.action_enabled("Enroll", &short));
        let actual = first_enrollment_transition(&mac, &short, "Enroll");
        assert_eq!(
            actual, short,
            "{action}: a writer enrolled records short of the log"
        );
        let stepped = mutant.successors(action, &short);
        assert!(
            !stepped.is_empty() && !stepped.contains(&actual),
            "{action}"
        );
        for wrong in &stepped {
            assert!(
                !model.check_invariant("FloorHoldsApplied", wrong),
                "{action}"
            );
        }
    }
    let mut ahead = model.init_state();
    ahead.insert("raw", 2);
    let actual = first_enrollment_transition(&mac, &ahead, "Enroll");
    assert_eq!((actual["enrolled"], actual["floor"]), (1, 1));
    let stepped = mutant.successors("RefuseGrown", &ahead);
    assert!(!stepped.is_empty() && !stepped.contains(&actual));
    for wrong in &stepped {
        assert!(!model.check_invariant("AHeldLogIsNeverRefused", wrong));
    }
    mac.lab.server.stop();
}
