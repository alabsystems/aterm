// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use std::io::Write;
fn limits() -> Limits {
    Limits {
        records: 8,
        bytes: 8192,
        record_bytes: 1024,
    }
}
fn open(dir: &Path) -> Log {
    {
        let mut log = Log::open(dir, "test", b"pinned", false, limits()).unwrap();
        log.finish_open().unwrap();
        log
    }
}

#[test]
fn strict_checkpoint_keeps_acknowledged_prefix_and_archives_torn_suffix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    assert_eq!(
        log.append("app/1", b"signed ciphertext".to_vec()).unwrap(),
        Offset::ZERO
    );
    drop(log);
    let path = dir.path().join("test.astream");
    let before = std::fs::read(&path).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"torn").unwrap();
    aterm_durable::durable(&file).unwrap();
    drop(file);
    let log = open(dir.path());
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(log.read_range(Offset::ZERO, 999).unwrap().len(), 1);
    let archived = dir.path().join(format!(
        "test-uncommitted-{}",
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(b"torn"))
    ));
    assert_eq!(std::fs::read(archived).unwrap(), b"torn");
}
#[test]
fn partial_loss_or_wrong_anchor_cannot_reset_history() {
    for file in ["test.anchor", "test.checkpoint", "test.astream"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut log = open(dir.path());
        log.append("app/1", vec![3; 30]).unwrap();
        drop(log);
        assert!(Log::open(dir.path(), "test", b"foreign", true, limits()).is_err());
        std::fs::remove_file(dir.path().join(file)).unwrap();
        assert!(Log::open(dir.path(), "test", b"pinned", true, limits()).is_err());
    }
}
#[test]
fn checkpoint_failure_fences_reads_and_writes_until_exact_retry() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1]).unwrap();
    let cp = dir.path().join("test.checkpoint");
    let saved = dir.path().join("saved");
    std::fs::rename(&cp, &saved).unwrap();
    std::fs::create_dir(&cp).unwrap();
    assert!(log.append("app/1", vec![2]).is_err());
    assert!(log.is_poisoned());
    assert!(log.read_range(Offset::ZERO, 1).is_err());
    assert!(log.append("app/1", vec![3]).is_err());
    drop(log);
    std::fs::remove_dir(&cp).unwrap();
    std::fs::rename(&saved, &cp).unwrap();
    let mut log = open(dir.path());
    assert_eq!(log.read_range(Offset::ZERO, 8).unwrap().len(), 1);
    assert_eq!(log.append("app/1", vec![2]).unwrap(), Offset(1));
}
#[test]
fn bounds_refuse_without_poison_and_replaced_lease_fences() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    assert!(log.append("app/1", vec![0; 1025]).is_err());
    assert!(!log.is_poisoned());
    log.append("app/1", vec![9]).unwrap();
    assert!(Log::open(dir.path(), "test", b"pinned", true, limits()).is_err());
    std::fs::rename(dir.path().join("test.lock"), dir.path().join("old.lock")).unwrap();
    private_file::write_private(dir.path(), "test.lock", b"").unwrap();
    assert!(log.is_poisoned());
    assert!(log.append("app/1", vec![9]).is_err());
}

#[test]
fn live_truncation_withholds_reads_and_new_acknowledgements() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1]).unwrap();
    OpenOptions::new()
        .write(true)
        .open(dir.path().join("test.astream"))
        .unwrap()
        .set_len(0)
        .unwrap();
    assert!(log.is_poisoned());
    assert!(log.read_range(Offset::ZERO, 8).is_err());
    assert!(log.append("app/1", vec![2]).is_err());
    drop(log);
    assert!(Log::open(dir.path(), "test", b"pinned", true, limits()).is_err());
}

#[test]
fn missing_live_anchor_or_checkpoint_is_not_recreated_by_append() {
    for name in ["test.anchor", "test.checkpoint"] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut log = open(dir.path());
        log.append("app/1", vec![1]).unwrap();
        let path = dir.path().join(name);
        std::fs::remove_file(&path).unwrap();
        assert!(log.is_poisoned());
        assert!(log.append("app/1", vec![2]).is_err());
        assert!(!path.exists());
    }
}

#[test]
fn a_one_record_journal_can_archive_a_torn_tail() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut bound = limits();
    bound.records = 1;
    let mut log = Log::open(dir.path(), "test", b"pinned", false, bound).unwrap();
    log.finish_open().unwrap();
    log.append("app/1", vec![1]).unwrap();
    drop(log);
    let mut file = OpenOptions::new()
        .append(true)
        .open(dir.path().join("test.astream"))
        .unwrap();
    file.write_all(b"torn").unwrap();
    aterm_durable::durable(&file).unwrap();
    drop(file);
    let log = Log::open(dir.path(), "test", b"pinned", true, bound).unwrap();
    assert_eq!(log.read_range(Offset::ZERO, 1).unwrap().len(), 1);
}
