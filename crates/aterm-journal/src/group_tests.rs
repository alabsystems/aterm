// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::tests::{Step, clear_append_hook, on_append};
use std::cell::RefCell;
use std::rc::Rc;

fn bounds() -> Limits {
    Limits {
        records: 1024,
        bytes: 1024 * 1024,
        record_bytes: 1024,
    }
}

fn open(dir: &Path, existing: bool) -> Log {
    let mut log = Log::open(dir, "group", b"pinned", existing, bounds()).unwrap();
    log.finish_open().unwrap();
    log
}

fn records(count: usize) -> Vec<(&'static str, Vec<u8>)> {
    (0..count)
        .map(|i| ("app/group", vec![i as u8; 32]))
        .collect()
}

#[test]
fn group_commit_uses_one_checkpoint_and_one_witness_for_256_records() {
    for strict in [false, true] {
        let _strict = strict.then(seam::serve_strict);
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut log = open(dir.path(), false);
        let steps = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&steps);
        let path = dir.path().to_path_buf();
        on_append(move |step, _| {
            // The checkpoint exposes either the previous prefix or the entire group,
            // including to a lock-free reader between individual record publications.
            let Committed::Rebuilt {
                records: committed, ..
            } = read_committed(&path, "group", b"pinned", bounds(), None).unwrap()
            else {
                panic!("a reader without a prefix rebuilds the journal");
            };
            assert_eq!(
                committed.len(),
                if step == Step::Published { 0 } else { 256 }
            );
            seen.borrow_mut().push(step);
        });
        let mut writes = 0;
        assert_eq!(
            log.append_many_then(records(256), dir.path(), |offset, flush| {
                writes += 1;
                assert_eq!(offset, Offset::ZERO);
                private_file::write_private_flushed(dir.path(), "witness", b"256", flush)
            })
            .unwrap(),
            Offset::ZERO
        );
        clear_append_hook();
        assert_eq!(writes, 1);
        let steps = steps.borrow();
        assert_eq!(steps.len(), 259);
        assert!(steps[..256].iter().all(|step| *step == Step::Published));
        assert_eq!(
            &steps[256..],
            &[Step::Checkpointed, Step::Committed, Step::Witnessed]
        );
        drop(log);
        let log = open(dir.path(), true);
        let kept = log.read_range(Offset::ZERO, 256).unwrap();
        assert_eq!(kept.len(), 256);
        for (i, record) in kept.iter().enumerate() {
            assert_eq!(record.seq, Offset(i as u64));
            assert_eq!(record.body, vec![i as u8; 32]);
        }
    }
}

#[test]
fn group_preflight_rejects_the_whole_input_without_publication() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path(), false);
    for _ in 0..2 {
        assert_eq!(log.check_append_many(&records(2)).unwrap(), Offset::ZERO);
    }
    for invalid in [
        vec![],
        records(257),
        vec![("app/valid", vec![1]), ("", vec![2])],
        vec![("app/valid", vec![1]), ("app/large", vec![2; 1025])],
    ] {
        assert!(log.check_append_many(&invalid).is_err());
        assert!(
            log.append_many_then(invalid, dir.path(), |_, _| {
                panic!("invalid input reached its dependent write")
            })
            .is_err()
        );
        assert!(!log.is_poisoned());
        assert_eq!(log.log.stats().head, Offset::ZERO);
        assert_eq!(
            std::fs::metadata(dir.path().join("group.astream"))
                .unwrap()
                .len(),
            0
        );
    }
    // Aggregate record capacity, rather than just each record's individual fit.
    log.layout.limits.records = 2;
    assert!(
        log.append_many_then(records(3), dir.path(), |_, _| Ok(()))
            .is_err()
    );
    // Aggregate framed bytes likewise must refuse before the first publication.
    log.layout.limits.records = 1024;
    log.layout.limits.bytes = 400;
    assert!(
        log.append_many_then(records(2), dir.path(), |_, _| Ok(()))
            .is_err()
    );
    assert_eq!(log.log.stats().head, Offset::ZERO);
    assert!(!log.is_poisoned());
    assert_eq!(log.append("app/scalar", vec![1]).unwrap(), Offset::ZERO);
}

#[test]
fn group_dependent_failure_fences_reads_and_writes_but_preserves_the_committed_group() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path(), false);
    assert!(
        log.append_many_then(records(8), dir.path(), |_, _| {
            Err(io::Error::other("dependent write failed"))
        })
        .is_err()
    );
    assert!(log.is_poisoned());
    assert!(log.read_range(Offset::ZERO, 8).is_err());
    assert!(log.append("app/scalar", vec![1]).is_err());
    drop(log);
    assert_eq!(
        open(dir.path(), true)
            .read_range(Offset::ZERO, 8)
            .unwrap()
            .len(),
        8
    );
}

#[test]
fn group_refuses_a_retained_producer_key_in_a_later_record() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path(), false);
    // Replayed broker producer metadata can name a future sequence even when the
    // application receipt chain is intact. A local deduplication hit is not a new
    // physical group record, including when its bytes equal the proposed record.
    log.log
        .publish_checked(1, 4, "app/group".into(), vec![2; 32])
        .unwrap();
    assert_eq!(log.append("app/seed", vec![9; 32]).unwrap(), Offset(1));
    let checkpoint = std::fs::read(dir.path().join("group.checkpoint")).unwrap();
    let mut witnessed = false;
    let appended = log.append_many_then(
        vec![("app/group", vec![1; 32]), ("app/group", vec![2; 32])],
        dir.path(),
        |_, _| {
            witnessed = true;
            Ok(())
        },
    );
    assert!(
        appended.is_err(),
        "a retained producer key stood in for a new group record"
    );
    assert!(!witnessed);
    assert!(log.is_poisoned());
    assert_eq!(
        std::fs::read(dir.path().join("group.checkpoint")).unwrap(),
        checkpoint
    );
    drop(log);
    // The first new record preceded the refusal, but never entered a checkpoint.
    // Reopen archives and discards it, preserving the prior acknowledged prefix.
    assert_eq!(
        open(dir.path(), true)
            .read_range(Offset::ZERO, 256)
            .unwrap()
            .len(),
        2
    );
}

/// Real process cuts test journal checkpoint atomicity, not simulated power loss or
/// the application's independent custody recovery policy.
#[test]
fn group_crash_cuts_keep_whole_checkpointed_groups() {
    const HOME: &str = "ATERM_JOURNAL_GROUP_CRASH_DIR";
    const CUT: &str = "ATERM_JOURNAL_GROUP_CRASH_CUT";
    if let Some(dir) = std::env::var_os(HOME) {
        let dir = PathBuf::from(dir);
        let cut: usize = std::env::var(CUT).unwrap().parse().unwrap();
        let mut log = open(&dir, false);
        log.append_many_then(records(3), &dir, |_, flush| {
            private_file::write_private_flushed(&dir, "witness", b"3", flush)
        })
        .unwrap();
        let mut step_index = 0;
        on_append(move |_, _| {
            step_index += 1;
            if step_index == cut {
                aterm_uds::process::kill_self_now();
            }
        });
        log.append_many_then(records(8), &dir, |_, flush| {
            private_file::write_private_flushed(&dir, "witness", b"11", flush)
        })
        .unwrap();
        panic!("crash cut was not reached");
    }
    use std::os::unix::process::ExitStatusExt as _;
    for cut in [1, 4, 8, 9, 10, 11] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "group_tests::group_crash_cuts_keep_whole_checkpointed_groups",
        ])
        .env(HOME, dir.path())
        .env(CUT, cut.to_string());
        let output =
            aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
                .unwrap();
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGKILL),
            "cut {cut}: {output:?}"
        );
        let kept = if cut <= 8 { 3 } else { 11 };
        assert_eq!(
            open(dir.path(), true)
                .read_range(Offset::ZERO, 256)
                .unwrap()
                .len(),
            kept
        );
        assert_eq!(
            std::fs::read(dir.path().join("witness")).unwrap(),
            if cut < 11 {
                b"3".as_slice()
            } else {
                b"11".as_slice()
            }
        );
    }
}
