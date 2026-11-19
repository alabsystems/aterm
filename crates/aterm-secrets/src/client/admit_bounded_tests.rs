// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::bootstrap::Device;
use crate::crossing::{Faults, RecordsBound, inject};
use crate::membership_log::{Body, Evidence, RecordBudgets, encode_records};

const ROOT: [u8; 32] = [0x31; 32];
const WRITER: [u8; 32] = [0x32; 32];
const INCARNATION: [u8; 16] = [0x33; 16];

struct Fixture {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    bundle: Bundle,
    log: Log,
    founder: Request,
    /// The founder's `[everyone]` epoch and its key.
    #[cfg_attr(
        not(unix),
        expect(dead_code, reason = "only the unix pair-admission test reads it")
    )]
    epoch: (RecordId, zeroize::Zeroizing<[u8; 32]>),
}

impl Fixture {
    fn new(source: Option<RecordsBound>) -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        let signer = crate::envelope::signer_public(&ROOT).unwrap();
        let network = [0x34; 16];
        let identity = Identity::from_secret_bytes([0x35; 32]);
        let founder =
            Request::create(network, signer, [0x36; 16], INCARNATION, &identity, &WRITER).unwrap();
        let genesis = Record::found(
            &ROOT,
            vec![Entrant {
                request: founder.clone(),
                node_key: [0x37; 32],
                name: "m1".into(),
                label: "m1".into(),
            }],
            1,
        )
        .unwrap();
        let mut log = Log::new(genesis).unwrap();
        machine_identity(&paths).unwrap();
        let device = Device::open(&paths).unwrap().unwrap();
        let request = device.request(network, signer).unwrap();
        let admit = log
            .draft(
                &founder.binding().actor(),
                2,
                Body::Admit {
                    entrant: Entrant {
                        request: request.clone(),
                        node_key: *request.binding().writer(),
                        name: "m2".into(),
                        label: "m2".into(),
                    },
                    evidence: Evidence::Phrase,
                },
            )
            .sign(&WRITER, INCARNATION)
            .unwrap();
        let admit_id = *admit.id();
        log.insert(admit).unwrap();
        let actor = founder.binding().actor();
        let (epoch, key) = log
            .draft_epoch(&actor, 3, EpochAudience::Everyone, 1)
            .unwrap();
        let epoch = epoch.sign(&WRITER, INCARNATION).unwrap();
        let epoch_id = *epoch.id();
        log.insert(epoch).unwrap();
        let founder_wrap = log
            .draft_wrap(&actor, 4, epoch_id, &key, identity.recipient())
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        log.insert(founder_wrap).unwrap();
        let wrap = log
            .draft_wrap(&actor, 5, epoch_id, &key, request.binding().recipient())
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        log.insert(wrap).unwrap();
        let put = log
            .draft_put(
                &actor,
                6,
                epoch_id,
                &key,
                &Entry {
                    name: "bundle-value".into(),
                    value: SecretBytes::new(b"synthetic-value".to_vec()),
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(&WRITER, INCARNATION)
            .unwrap();
        log.insert(put).unwrap();
        let bundle = Bundle {
            vault: "join-tests".into(),
            signer,
            followed: Vec::new(),
            servers: vec!["http://127.0.0.1:9/v1/join-tests".into()],
            allowed: Allowed::sign(request.binding(), "m2", &WRITER).unwrap(),
            handed: Handed::Log {
                source,
                admit: admit_id,
                records: encode_records(log.records()),
            },
        };
        Self {
            _root: root,
            paths,
            bundle,
            log,
            founder,
            epoch: (epoch_id, key),
        }
    }

    fn unchanged(&self) {
        assert!(config(&self.paths).unwrap().is_none());
        assert!(!self.paths.records_file().exists());
        assert!(!self.paths.home.join(".secrets/bundle-value").exists());
    }

    fn add_name(&mut self) {
        let record = self
            .log
            .draft(
                &self.founder.binding().actor(),
                self.log.held_count() as u64 + 1,
                Body::Name {
                    name: "later-name".into(),
                },
            )
            .sign(&WRITER, INCARNATION)
            .unwrap();
        self.log.insert(record).unwrap();
        if let Handed::Log { records, .. } = &mut self.bundle.handed {
            *records = encode_records(self.log.records());
        }
    }
}

#[test]
fn log_bundle_inherits_source_mark_only_when_it_learns_complete_records() {
    for duplicate in [false, true] {
        let fixture = Fixture::new(Some(RecordsBound::here(true)));
        if duplicate {
            save_records(&fixture.paths, &encode_records(fixture.log.records())).unwrap();
        }
        accepts_bundle(&fixture.paths, &fixture.bundle).unwrap();
        assert!(!records_marked_crossed(&fixture.paths).unwrap());
        write_from_log_bundle(&fixture.paths, &fixture.bundle).unwrap();
        assert_eq!(records_marked_crossed(&fixture.paths).unwrap(), !duplicate);
        assert_eq!(
            load_records(&fixture.paths).unwrap().complete_inventory(),
            fixture.log.records().map(|r| *r.id()).collect()
        );
        assert_eq!(
            crate::client::stored(&fixture.paths, "bundle-value").unwrap(),
            b"synthetic-value"
        );
    }
}

/// A new Mac refuses a member's log it cannot take, before custody, saying which Mac's aterm to
/// update: its own where the member may hold more than it takes, the member's where this Mac
/// crossed and the member's `alab-join-bundle/2` states no bound.
///
/// NEGATIVE CONTROL: the same bundle from a member within this Mac's bound is taken
/// (`log_bundle_inherits_source_mark_only_when_it_learns_complete_records`).
#[test]
fn log_bundle_refuses_incompatible_or_unbounded_source_before_custody() {
    for (source, update) in [
        (
            Some(RecordsBound::of(2 * RecordBudgets::PRODUCTION.held, true)),
            "update aterm on this Mac",
        ),
        (None, "update aterm on the allowing Mac"),
    ] {
        let fixture = Fixture::new(source);
        if source.is_none() {
            super::super::crossing_mark(&fixture.paths).store().unwrap();
        }
        let refused = accepts_bundle(&fixture.paths, &fixture.bundle).unwrap_err();
        assert!(refused.to_string().contains(update), "{refused}");
        let refused = write_from_log_bundle(&fixture.paths, &fixture.bundle).unwrap_err();
        assert!(refused.to_string().contains(update), "{refused}");
        fixture.unchanged();
    }
}

#[test]
fn log_bundle_refuses_a_partially_retained_security_batch() {
    let mut fixture = Fixture::new(Some(RecordsBound::of(16, false)));
    fixture.add_name();
    RecordBudgets {
        held: 16,
        keyed: 4,
        ..RecordBudgets::PRODUCTION
    }
    .within(|| {
        assert!(write_from_log_bundle(&fixture.paths, &fixture.bundle).is_err());
        fixture.unchanged();
    });
}

#[test]
fn log_bundle_mark_failure_or_interruption_precedes_config_and_values() {
    for (faults, marked) in [
        (
            Faults {
                fail_store: true,
                ..Faults::default()
            },
            false,
        ),
        (
            Faults {
                dies_between: true,
                ..Faults::default()
            },
            true,
        ),
    ] {
        let fixture = Fixture::new(Some(RecordsBound::here(true)));
        let mark = super::super::crossing_mark(&fixture.paths);
        inject(mark.path(), faults);
        let result = write_from_log_bundle(&fixture.paths, &fixture.bundle);
        inject(mark.path(), Faults::default());
        assert!(result.is_err());
        fixture.unchanged();
        assert_eq!(mark.stored().unwrap(), marked);
    }
}

#[test]
fn log_bundle_keeps_a_complete_history_larger_than_the_legacy_wire_bound() {
    const LARGER: RecordBudgets = RecordBudgets {
        held: 8192,
        ..RecordBudgets::PRODUCTION
    };
    LARGER.within(|| {
        let mut fixture = Fixture::new(Some(RecordsBound::here(true)));
        while fixture.log.held_count() <= crate::membership_log::LEGACY_RECORDS {
            let count = fixture.log.held_count();
            let record = fixture
                .log
                .draft(
                    &fixture.founder.binding().actor(),
                    count as u64 + 1,
                    Body::Name {
                        name: format!("large-bundle-{count}"),
                    },
                )
                .sign(&WRITER, INCARNATION)
                .unwrap();
            fixture.log.insert(record).unwrap();
        }
        let wire = encode_records(fixture.log.records());
        assert!(
            crate::membership_log::store::decode_records_bounded(
                &wire,
                crate::membership_log::LEGACY_RECORDS
            )
            .is_err()
        );
        if let Handed::Log { records, .. } = &mut fixture.bundle.handed {
            *records = wire;
        }
        write_from_log_bundle(&fixture.paths, &fixture.bundle).unwrap();
        assert!(records_marked_crossed(&fixture.paths).unwrap());
        assert_eq!(
            load_records(&fixture.paths).unwrap().complete_inventory(),
            fixture.log.records().map(|r| *r.id()).collect()
        );
    });
}

#[test]
fn log_bundle_refuses_new_waiting_only_custody_from_a_crossed_source() {
    let mut fixture = Fixture::new(Some(RecordsBound::here(true)));
    let base = encode_records(fixture.log.records());
    save_records(&fixture.paths, &base).unwrap();
    let parent = fixture
        .log
        .draft(
            &fixture.founder.binding().actor(),
            7,
            Body::Name {
                name: "missing-parent".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    fixture.log.insert(parent).unwrap();
    let child = fixture
        .log
        .draft(
            &fixture.founder.binding().actor(),
            8,
            Body::Name {
                name: "waiting-child".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    if let Handed::Log { records, .. } = &mut fixture.bundle.handed {
        records.extend_from_slice(child.bytes());
    }
    assert!(write_from_log_bundle(&fixture.paths, &fixture.bundle).is_err());
    assert!(!records_marked_crossed(&fixture.paths).unwrap());
    assert!(config(&fixture.paths).unwrap().is_none());
    assert_eq!(std::fs::read(fixture.paths.records_file()).unwrap(), base);
    assert!(!fixture.paths.home.join(".secrets/bundle-value").exists());
}

#[test]
fn log_bundle_rejoin_preserves_newer_local_history_from_the_same_root() {
    let mut fixture = Fixture::new(Some(RecordsBound::here(false)));
    let earlier = encode_records(fixture.log.records());
    fixture.add_name();
    let expected = fixture.log.records().map(|r| *r.id()).collect();
    save_records(&fixture.paths, &encode_records(fixture.log.records())).unwrap();
    if let Handed::Log { records, .. } = &mut fixture.bundle.handed {
        *records = earlier;
    }
    write_from_log_bundle(&fixture.paths, &fixture.bundle).unwrap();
    assert_eq!(
        load_records(&fixture.paths).unwrap().complete_inventory(),
        expected
    );
    assert!(!records_marked_crossed(&fixture.paths).unwrap());
}

#[test]
fn log_bundle_refuses_a_union_that_cannot_preserve_both_histories() {
    let mut fixture = Fixture::new(Some(RecordsBound::of(16, false)));
    let mut remote = fixture.log.clone();
    fixture.add_name();
    let record = remote
        .draft(
            &fixture.founder.binding().actor(),
            7,
            Body::Name {
                name: "remote-name".into(),
            },
        )
        .sign(&WRITER, INCARNATION)
        .unwrap();
    remote.insert(record).unwrap();
    if let Handed::Log { records, .. } = &mut fixture.bundle.handed {
        *records = encode_records(remote.records());
    }
    RecordBudgets {
        held: 16,
        keyed: 5,
        ..RecordBudgets::PRODUCTION
    }
    .within(|| {
        let before = encode_records(fixture.log.records());
        save_records(&fixture.paths, &before).unwrap();
        assert!(write_from_log_bundle(&fixture.paths, &fixture.bundle).is_err());
        assert!(std::fs::read(fixture.paths.records_file()).unwrap() == before);
        assert!(config(&fixture.paths).unwrap().is_none());
        assert!(!fixture.paths.home.join(".secrets/bundle-value").exists());
    });
}

#[test]
fn log_bundle_repairs_an_oversized_local_mark_before_publishing_config() {
    const LARGER: RecordBudgets = RecordBudgets {
        held: 8192,
        ..RecordBudgets::PRODUCTION
    };
    LARGER.within(|| {
        let mut fixture = Fixture::new(Some(RecordsBound::here(true)));
        while fixture.log.held_count() <= crate::membership_log::LEGACY_RECORDS {
            let count = fixture.log.held_count();
            let record = fixture
                .log
                .draft(
                    &fixture.founder.binding().actor(),
                    count as u64 + 1,
                    Body::Name {
                        name: format!("old-local-{count}"),
                    },
                )
                .sign(&WRITER, INCARNATION)
                .unwrap();
            fixture.log.insert(record).unwrap();
        }
        let before = encode_records(fixture.log.records());
        crate::fsx::write_atomic(&fixture.paths.records_file(), &before, 0o600).unwrap();
        let mark = super::super::crossing_mark(&fixture.paths);
        assert!(!mark.stored().unwrap());
        inject(
            mark.path(),
            Faults {
                dies_between: true,
                ..Faults::default()
            },
        );
        let result = write_from_log_bundle(&fixture.paths, &fixture.bundle);
        inject(mark.path(), Faults::default());
        assert!(result.is_err());
        assert!(mark.stored().unwrap());
        assert!(config(&fixture.paths).unwrap().is_none());
        assert!(std::fs::read(fixture.paths.records_file()).unwrap() == before);
        assert!(!fixture.paths.home.join(".secrets/bundle-value").exists());
    });
}

/// A `Put` the founder signs as record version 2, added to `fixture`'s log and its bundle.
#[cfg(unix)]
fn add_two(fixture: &mut Fixture) -> Record {
    let record = fixture
        .log
        .draft_put(
            &fixture.founder.binding().actor(),
            fixture.log.held_count() as u64 + 1,
            fixture.epoch.0,
            &fixture.epoch.1,
            &Entry {
                name: "format-two-value".into(),
                value: SecretBytes::new(b"synthetic-two".to_vec()),
                audience: Audience::Everyone,
                if_absent: false,
                anchor: None,
            },
        )
        .unwrap()
        .sign_two(
            &WRITER,
            INCARNATION,
            Some(&crate::membership_log::Statement {
                supersedes: Vec::new(),
                carries: None,
            }),
        )
        .unwrap();
    assert_eq!(record.version(), 2);
    fixture.log.insert(record.clone()).unwrap();
    if let Handed::Log { records, .. } = &mut fixture.bundle.handed {
        *records = encode_records(fixture.log.records());
    }
    record
}

/// A new Mac joins a format-2 vault by the bundle its member hands it (tracker FL-148e): where
/// its build reads format 2 it takes a bundle whose member states format 2 as a format-2 reader
/// would, into a store nothing reads, and on a seat's signed version-2 record among the
/// bundle's records stores its format mark before its configuration, its records or a value;
/// its records file then holds that record and it states format 2. A bundle that only states
/// format 2 (its records all of format 1) is refused and stores no mark, a Mac that dies
/// between its mark and its files holds the mark and nothing else and takes the same bundle
/// again, and a Mac whose build reads no format 2 refuses the bundle as before, looking for no
/// mark.
///
/// NEGATIVE CONTROLS, each run against this test and seen to fail:
/// - a Mac that holds the mark reading the bundle at format 1 (`checked_log_bundle` decoding
///   with `Reading::ONE` where it does not follow, as this slice's first build did): ``a Mac
///   that holds the format mark accepts a format-2 member's bundle: Error("not a membership
///   record", Other)``.
/// - `Faults::mark_on_header` (the member's statement is the evidence): ``a bundle that only
///   states format 2 is refused``.
/// - `Faults::format_after_write` (the files first, then the mark): the dying Mac's
///   ``assertion failed: config(&self.paths).unwrap().is_none()``.
/// - `FormatMark::follows` answering yes for every node (no gate): the dark Mac's
///   ``called `Result::unwrap_err()` on an `Ok` value``.
#[cfg(unix)]
#[test]
fn vault_checkpoint_a_new_mac_joins_a_format_two_vault() {
    use crate::crossing::{FORMAT_ONE, FORMAT_TWO};
    let two_stated = RecordsBound::here(false).at_format(FORMAT_TWO);
    let follows = Faults {
        follows: true,
        ..Faults::default()
    };
    let mark_of = |fixture: &Fixture| super::super::format_mark(&fixture.paths);
    let suffering = |fixture: &Fixture, faults: Faults| {
        let mark = mark_of(fixture);
        inject(mark.path(), faults);
        let result = write_from_log_bundle(&fixture.paths, &fixture.bundle);
        inject(mark.path(), Faults::default());
        result
    };

    // A build that reads no format 2: refused as a bundle of another format ever was.
    let mut dark = Fixture::new(Some(two_stated));
    add_two(&mut dark);
    let refused = suffering(&dark, Faults::default()).unwrap_err();
    assert!(refused.to_string().contains("format 2"), "{refused}");
    dark.unchanged();
    assert!(!mark_of(&dark).path().exists());

    // What the member states is no evidence.
    let claimed = Fixture::new(Some(two_stated));
    assert!(
        suffering(&claimed, follows).is_err(),
        "a bundle that only states format 2 is refused"
    );
    claimed.unchanged();
    assert_eq!(mark_of(&claimed).stored().unwrap(), FORMAT_ONE);

    // Mark first.
    let mut dies = Fixture::new(Some(two_stated));
    let died_before = add_two(&mut dies);
    let died = suffering(
        &dies,
        Faults {
            dies_between: true,
            ..follows
        },
    );
    assert!(died.is_err());
    dies.unchanged();
    assert_eq!(mark_of(&dies).stored().unwrap(), FORMAT_TWO);
    // The Mac that died holds its mark and none of the files: it takes the bundle again,
    // reading it at the format its mark names.
    inject(mark_of(&dies).path(), follows);
    accepts_bundle(&dies.paths, &dies.bundle)
        .expect("a Mac that holds the format mark accepts a format-2 member's bundle");
    write_from_log_bundle(&dies.paths, &dies.bundle)
        .expect("the Mac that died after its mark takes the bundle again");
    assert!(load_records(&dies.paths).unwrap().holds(died_before.id()));
    inject(mark_of(&dies).path(), Faults::default());

    let mut joins = Fixture::new(Some(two_stated));
    let two = add_two(&mut joins);
    let mark = mark_of(&joins);
    inject(mark.path(), follows);
    accepts_bundle(&joins.paths, &joins.bundle).unwrap();
    assert_eq!(
        mark.stored().unwrap(),
        FORMAT_ONE,
        "accepting stores no mark"
    );
    write_from_log_bundle(&joins.paths, &joins.bundle).unwrap();
    assert_eq!(mark.stored().unwrap(), FORMAT_TWO);
    let store = load_records(&joins.paths).unwrap();
    assert!(store.holds(two.id()));
    assert_eq!(
        store.reading(),
        crate::membership_log::Reading::Format(FORMAT_TWO)
    );
    assert_eq!(
        super::super::records_bound(&joins.paths, &store)
            .unwrap()
            .format,
        FORMAT_TWO
    );
    assert_eq!(
        crate::client::stored(&joins.paths, "bundle-value").unwrap(),
        b"synthetic-value"
    );
    assert_eq!(
        crate::client::stored(&joins.paths, "format-two-value").unwrap(),
        b"synthetic-two",
        "the version-2 Put's value is read"
    );
    inject(mark.path(), Faults::default());
}

/// A new Mac's home and the bundle a member of [`crate::membership_log::pruned_vault`] hands
/// it: seat A let the Mac in after the vault's final checkpoint, and A's log is rooted there,
/// so the bundle's records end with the log's roots and stubs (`held_records`). The member
/// states format 2.
#[cfg(unix)]
struct RootedJoin {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    vault: crate::membership_log::PrunedVault,
    bundle: Bundle,
}

#[cfg(unix)]
impl RootedJoin {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(root.path());
        machine_identity(&paths).unwrap();
        let device = Device::open(&paths).unwrap().unwrap();
        let request = device.request([2; 16], [3; 32]).unwrap();
        let mut vault = crate::membership_log::pruned_vault();
        let (admit, allowed) = vault.admit(&request, "m4");
        let bundle = Bundle {
            vault: "join-tests".into(),
            signer: crate::envelope::signer_public(&vault.founder).unwrap(),
            followed: Vec::new(),
            servers: vec!["http://127.0.0.1:9/v1/join-tests".into()],
            allowed,
            handed: Handed::Log {
                source: Some(RecordsBound::here(false).at_format(crate::crossing::FORMAT_TWO)),
                admit,
                records: held_records(&vault.peer).unwrap(),
            },
        };
        Self {
            _root: root,
            paths,
            vault,
            bundle,
        }
    }

    /// This Mac's build reads format 2 from now on, or reads none.
    fn follows(&self, follows: bool) {
        inject(
            super::super::format_mark(&self.paths).path(),
            Faults {
                follows,
                ..Faults::default()
            },
        );
    }

    fn format(&self) -> u32 {
        super::super::format_mark(&self.paths).stored().unwrap()
    }

    /// The rewrite generation of this Mac's records entry and its data file's bytes, as a
    /// reader of format 2 selects them.
    fn entry(&self) -> (u64, Vec<u8>) {
        let custody =
            crate::records_storage::rewrite::data_path(&self.paths.records_file()).unwrap();
        (custody.generation, std::fs::read(custody.path).unwrap())
    }
}

#[cfg(unix)]
impl Drop for RootedJoin {
    fn drop(&mut self) {
        self.follows(false);
    }
}

/// A new Mac takes the bundle of a member whose log is rooted at a final checkpoint
/// (`write_log_bundle`, tracker FL-148E2): on a build that reads format 2 it reads the
/// bundle's records and the roots after them, stores its format mark on a seat's version-2
/// record among them, and its records entry is then a rewritten layout whose file begins with
/// the member's frame and reads back as the member's log: the record that follows only the
/// dropped one held, the dropped one known and not held. A Mac whose build reads no format 2
/// refuses the bundle and holds no mark, no configuration and no records.
///
/// NEGATIVE CONTROL, a change to the code run against this test and seen to fail:
/// `checked_log_bundle` taking the bundle's records without the roots `bundle_records` read
/// after them (`ingest_served(decoded, None)`): ``a Mac that reads format 2 accepts a rooted
/// member's bundle``, the bundle refused as one that ``states membership records of format 2
/// and holds no record of that format a seat signed``: records alone leave every version-2
/// record after the cut waiting on the dropped one.
#[cfg(unix)]
#[test]
fn vault_checkpoint_a_new_mac_takes_a_rooted_members_bundle() {
    use crate::crossing::{FORMAT_ONE, FORMAT_TWO};
    let dark = RootedJoin::new();
    let refused = write_from_log_bundle(&dark.paths, &dark.bundle).unwrap_err();
    assert!(refused.to_string().contains("format 2"), "{refused}");
    assert!(config(&dark.paths).unwrap().is_none());
    assert!(crate::records_storage::absent(&dark.paths.records_file()).unwrap());
    assert!(!super::super::format_mark(&dark.paths).path().exists());

    let joins = RootedJoin::new();
    joins.follows(true);
    accepts_bundle(&joins.paths, &joins.bundle)
        .expect("a Mac that reads format 2 accepts a rooted member's bundle");
    assert_eq!(joins.format(), FORMAT_ONE, "accepting stores no mark");
    write_from_log_bundle(&joins.paths, &joins.bundle)
        .expect("a Mac that reads format 2 takes a rooted member's bundle");
    assert_eq!(joins.format(), FORMAT_TWO);
    let (generation, file) = joins.entry();
    assert_eq!(generation, 1, "the entry is a rewritten layout");
    let frame = joins.vault.peer.kept_frame().unwrap().expect("a frame");
    assert!(
        file.starts_with(&frame),
        "its file begins with the member's frame"
    );
    let store = load_records(&joins.paths).unwrap();
    let log = store.log().expect("a log");
    assert_eq!(
        log.newest_root().map(|root| root.id),
        Some(joins.vault.root)
    );
    assert!(log.get(&joins.vault.slept).is_some());
    assert!(log.is_dropped(&joins.vault.dropped) && log.get(&joins.vault.dropped).is_none());
    assert_eq!(store.inventory(), joins.vault.peer.inventory());
    assert_eq!(store.heads_digest(), joins.vault.peer.heads_digest());
    assert_eq!(
        crate::client::stored(&joins.paths, "a.key").unwrap(),
        b"two",
        "the vault's value is read"
    );
}

/// A Mac whose records entry is already a rewritten layout takes a member's bundle again
/// (tracker FL-148E2): a Mac that joined a format-2 vault by a rooted member's bundle, and
/// whose state then says the vault let it go, is handed the member's log once more, grown by a
/// record. The bundle's write selects the entry as a writer of format 2 does, so it is not
/// refused before its configuration is written; the entry stays of the generation it was, its
/// file holds the frame, the records it held and the new one, and the Mac is in the vault
/// again.
///
/// NEGATIVE CONTROL, a change to the code run against this test and seen to fail:
/// `write_log_bundle` selecting the entry through `client::records_file_for_write`, the
/// format-1 selection, whatever the store reads (as before this fix): ``a Mac at a rewritten
/// entry takes a bundle again: Error("membership records storage: a layout file is missing",
/// Other)``.
#[cfg(unix)]
#[test]
fn vault_checkpoint_a_mac_at_a_rewritten_entry_takes_a_bundle_again() {
    let mut joins = RootedJoin::new();
    joins.follows(true);
    write_from_log_bundle(&joins.paths, &joins.bundle).unwrap();
    let (generation, file) = joins.entry();
    assert_eq!(generation, 1);

    let mut state = load_state(&joins.paths).unwrap();
    assert_eq!(state.membership, Stored::In);
    state.membership = Stored::Revoked;
    save_state(&joins.paths, &state).unwrap();
    let next = joins.vault.next.clone();
    assert_eq!(
        joins.vault.peer.ingest_records(vec![next.clone()]).unwrap(),
        1
    );
    if let Handed::Log { records, .. } = &mut joins.bundle.handed {
        *records = held_records(&joins.vault.peer).unwrap();
    }

    accepts_bundle(&joins.paths, &joins.bundle).unwrap();
    write_from_log_bundle(&joins.paths, &joins.bundle)
        .expect("a Mac at a rewritten entry takes a bundle again");
    let (generation, grown) = joins.entry();
    assert_eq!(generation, 1, "the entry is of the generation it was");
    let frame = joins.vault.peer.kept_frame().unwrap().expect("a frame");
    let wire = next.wire_bytes().unwrap();
    assert!(file.starts_with(&frame) && grown.starts_with(&frame));
    assert!(
        grown.len() == file.len() + wire.len()
            && grown.windows(wire.len()).any(|window| window == &wire[..]),
        "the file holds the frame, the records it held and the new one"
    );
    let store = load_records(&joins.paths).unwrap();
    assert!(store.holds(next.id()));
    assert_eq!(store.inventory(), joins.vault.peer.inventory());
    assert_eq!(load_state(&joins.paths).unwrap().membership, Stored::In);
}
