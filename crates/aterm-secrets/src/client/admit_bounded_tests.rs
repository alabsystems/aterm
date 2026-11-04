// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::age::Identity;
use crate::commons::bootstrap::Device;
use crate::crossing::{Faults, RecordsBound, inject};
use crate::membership_log::{Body, Evidence, RecordBudgets};

const ROOT: [u8; 32] = [0x31; 32];
const WRITER: [u8; 32] = [0x32; 32];
const INCARNATION: [u8; 16] = [0x33; 16];

struct Fixture {
    _root: aterm_tempfile::TempDir,
    paths: Paths,
    bundle: Bundle,
    log: Log,
    founder: Request,
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
            std::fs::read(fixture.paths.home.join(".secrets/bundle-value")).unwrap(),
            b"synthetic-value"
        );
    }
}

#[test]
fn log_bundle_refuses_incompatible_or_unbounded_source_before_custody() {
    for source in [Some(RecordsBound::of(8192, true)), None] {
        let fixture = Fixture::new(source);
        if source.is_none() {
            super::super::crossing_mark(&fixture.paths).store().unwrap();
        }
        assert!(accepts_bundle(&fixture.paths, &fixture.bundle).is_err());
        assert!(write_from_log_bundle(&fixture.paths, &fixture.bundle).is_err());
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
        assert!(crate::membership_log::decode_records(&wire).is_err());
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
