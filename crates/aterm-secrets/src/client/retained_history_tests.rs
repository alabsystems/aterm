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
        let value = reader.home.join(".secrets/shared");
        assert_eq!(std::fs::read(&value).unwrap(), b"new");
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
        assert_eq!(
            std::fs::read(&value).unwrap(),
            b"new",
            "a valid older disk snapshot must not replace an acknowledged value (legacy={legacy})"
        );
        assert!(!matches!(recovered, Ok(SyncOutcome::Applied { .. })));
        assert!(!materialization_ready(&reader).unwrap());

        // Restoring the exact acknowledged graph permits enrollment or journal recovery.
        fsx::write_atomic(&reader.records_data_file().unwrap(), &current, 0o600).unwrap();
        drop(StateLock::take(&reader).unwrap_or_else(|error| {
            panic!("the complete frontier must recover (legacy={legacy}): {error}")
        }));
        assert_eq!(std::fs::read(&value).unwrap(), b"new");
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
            std::fs::read(&value).unwrap(),
            b"old",
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
    assert_eq!(
        std::fs::read(lab.m7.home.join(".secrets/shared")).unwrap(),
        b"new"
    );
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
