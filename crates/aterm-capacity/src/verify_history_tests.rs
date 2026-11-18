// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::book::{
    KEPT, LEGACY_VERIFY_RECORD, Peak, ROWS_KEPT, VERIFY_RECORD, VERIFY_SHAPING_RECORD,
};

const GIB: u64 = 1 << 30;

fn stage(n: usize) -> Key {
    Key {
        stage: "Tippy".into(),
        checkout: format!("/fixture/{n}"),
        crates: None,
        variant: Some("compiler jobs 1".into()),
    }
}

fn unit(n: usize) -> Key {
    Key {
        stage: crate::book::UNIT_STAGE.into(),
        checkout: "/fixture/0".into(),
        crates: Some(vec![format!("unit-{n}")]),
        variant: Some(format!("unit_{n} lib")),
    }
}

#[test]
fn a_unit_burst_retains_the_measured_aggregate_and_its_original_key() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    keep_stage(&path, stage(0), 24 * GIB, 1, true).unwrap();
    let sealed = std::fs::read(&path).unwrap();
    let mut old_policy = Records::default();
    old_policy.push(stage(0), 24 * GIB, 1);
    for n in 0..ROWS_KEPT {
        keep_shaping_unit(&path, unit(n), GIB, 2).unwrap();
        old_policy.push(unit(n), GIB, 2);
    }
    assert_eq!(
        old_policy.learned(&stage(0)),
        None,
        "old shared retention loses the stage"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        sealed,
        "units never rewrite the aggregate"
    );
    let stages = stage_records(&path);
    assert_eq!(stages.rows().len(), 1);
    assert_eq!(stages.learned(&stage(0)).unwrap().peak, 24 * GIB);
    assert!(
        stages.learned(&unit(0)).is_none(),
        "aggregate readers never see weak units"
    );
    let shaped = ShapingHistory::load(&path);
    assert_eq!(shaped.rows().len(), ROWS_KEPT + 1);
    assert_eq!(shaped.learned(&unit(0)).unwrap().peak, GIB);
    assert!(
        shaped
            .learned(&Key {
                variant: Some("compiler jobs 2".into()),
                ..stage(0)
            })
            .is_none()
    );
}

#[test]
fn each_domain_evicts_only_its_own_oldest_row_and_keeps_eight_peaks() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    for n in 0..=ROWS_KEPT {
        keep_stage(&path, stage(n), GIB, n as u64, false).unwrap();
        keep_shaping_unit(&path, unit(n), GIB, n as u64).unwrap();
    }
    let stages = stage_records(&path);
    let units = Records::load(&dir.path().join(VERIFY_SHAPING_RECORD));
    assert_eq!(
        (stages.rows().len(), units.rows().len()),
        (ROWS_KEPT, ROWS_KEPT)
    );
    assert!(stages.learned(&stage(0)).is_none());
    assert!(units.learned(&unit(0)).is_none());
    assert!(stages.learned(&stage(1)).is_some() && units.learned(&unit(1)).is_some());
    for n in 0..KEPT + 3 {
        keep_stage(
            &path,
            stage(1),
            (n as u64 + 1) * GIB,
            10_000 + n as u64,
            false,
        )
        .unwrap();
        keep_shaping_unit(&path, unit(1), (n as u64 + 1) * GIB, 10_000 + n as u64).unwrap();
    }
    for records in [
        stage_records(&path),
        Records::load(&dir.path().join(VERIFY_SHAPING_RECORD)),
    ] {
        let row = records
            .rows()
            .iter()
            .find(|r| r.key == stage(1) || r.key == unit(1))
            .unwrap();
        assert_eq!(row.peaks.len(), KEPT);
        assert_eq!(row.peaks.first().unwrap().at_ms, 10_003);
    }
    assert!(keep_stage(&path, unit(2), GIB, 20_000, false).is_err());
    assert!(keep_shaping_unit(&path, stage(2), GIB, 20_000).is_err());
}

#[test]
fn mixed_history_import_preserves_keys_peaks_and_times_even_for_light_runs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    let legacy = dir.path().join(LEGACY_VERIFY_RECORD);
    let units = dir.path().join(VERIFY_SHAPING_RECORD);
    let mut old = Records::default();
    old.push(stage(0), 24 * GIB, 10);
    old.push(unit(0), 8 * GIB, 11);
    old.save(&legacy).unwrap();
    let previous = std::fs::read(&legacy).unwrap();
    assert_eq!(ShapingHistory::load(&path).rows().len(), 2);
    assert!(
        !path.exists() && !units.exists(),
        "a read never publishes migration"
    );
    assert!(!keep_stage(&path, stage(0), GIB, 12, true).unwrap());
    assert!(!keep_shaping_unit(&path, unit(0), GIB, 12).unwrap());
    assert!(
        path.exists() && units.exists(),
        "light runs still isolate the retained heavy rows"
    );
    assert_eq!(
        stage_records(&path).rows()[0].peaks,
        vec![Peak {
            bytes: 24 * GIB,
            at_ms: 10
        }]
    );
    assert_eq!(
        Records::load(&units).rows()[0].peaks,
        vec![Peak {
            bytes: 8 * GIB,
            at_ms: 11
        }]
    );
    assert_eq!(std::fs::read(&legacy).unwrap(), previous);
    // An old writer can replace or churn its own book without changing either new domain.
    Records::default().save(&legacy).unwrap();
    assert_eq!(
        stage_records(&path).learned(&stage(0)).unwrap().peak,
        24 * GIB
    );
    assert_eq!(
        ShapingHistory::load(&path).learned(&unit(0)).unwrap().peak,
        8 * GIB
    );
}

#[test]
fn malformed_and_oversized_new_books_stay_closed_without_legacy_fallback_or_overwrite() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    let legacy = dir.path().join(LEGACY_VERIFY_RECORD);
    let mut old = Records::default();
    old.push(stage(0), 24 * GIB, 1);
    old.save(&legacy).unwrap();
    for bytes in [
        b"{\"schema\":2,\"rows\":[".to_vec(),
        vec![0xff],
        vec![b' '; super::super::BOOK_MAX_BYTES as usize + 1],
    ] {
        std::fs::write(&path, &bytes).unwrap();
        assert!(stage_records(&path).rows().is_empty());
        assert!(keep_stage(&path, stage(1), GIB, 2, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&legacy, b"malformed legacy").unwrap();
    assert!(stage_records(&path).rows().is_empty());
    assert!(keep_stage(&path, stage(0), GIB, 3, false).is_err());
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn a_symlinked_domain_cannot_read_or_replace_legacy_evidence() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    let legacy = dir.path().join(LEGACY_VERIFY_RECORD);
    let mut old = Records::default();
    old.push(stage(0), GIB, 1);
    old.save(&legacy).unwrap();
    let bytes = std::fs::read(&legacy).unwrap();
    std::os::unix::fs::symlink(&legacy, &path).unwrap();
    assert!(stage_records(&path).rows().is_empty());
    assert!(keep_stage(&path, stage(0), GIB, 2, false).is_err());
    assert_eq!(std::fs::read(&legacy).unwrap(), bytes);
}

#[test]
fn each_domain_refuses_publication_past_the_existing_byte_bound() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    for (domain, mut key) in [(Domain::Stages, stage(0)), (Domain::Units, unit(0))] {
        domain.keep(&path, key.clone(), GIB, 1, false).unwrap();
        let target = domain.path(&path);
        let previous = std::fs::read(&target).unwrap();
        key.variant = Some("x".repeat(super::super::BOOK_MAX_BYTES as usize));
        assert_eq!(
            domain.keep(&path, key, GIB, 2, false).unwrap_err().kind(),
            std::io::ErrorKind::FileTooLarge
        );
        assert_eq!(std::fs::read(&target).unwrap(), previous);
    }
}

#[test]
fn concurrent_first_writers_import_once_without_losing_each_others_rows() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join(VERIFY_RECORD);
    let mut old = Records::default();
    old.push(stage(0), 24 * GIB, 1);
    old.save(&dir.path().join(LEGACY_VERIFY_RECORD)).unwrap();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let jobs: Vec<_> = (1..=2)
        .map(|n| {
            let (path, start) = (path.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                keep_stage(&path, stage(n), GIB, n as u64 + 1, false).unwrap();
            })
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    let kept = stage_records(&path);
    assert_eq!(kept.rows().len(), 3);
    for n in 0..=2 {
        assert!(kept.learned(&stage(n)).is_some());
    }
}
