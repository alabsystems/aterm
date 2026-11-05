// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real legacy claim locks must not become free capacity through unreadable metadata.

use super::*;
use crate::{Admit, Error, Fixed, Pace, Polled, Reading, Short, Waiter, admit_with};

fn locked(path: &Path, bytes: &[u8]) -> RecordLock {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    RecordLock::try_lock(file, RECORD_MAX).unwrap()
}

/// Lay `bytes` over the start of `guard`'s record through the holder's own handle, as only the
/// holder writes a held record: Windows locks the range from [`RECORD_MAX`] on against every
/// other handle, so another handle's write that reaches it (or truncates the file) is refused.
fn rewrite(guard: &mut Laid, bytes: &[u8]) {
    guard.lock.seek(SeekFrom::Start(0)).unwrap();
    guard.lock.write_all(bytes).unwrap();
}

/// `guard`'s record as its holder reads it, for the same reason.
fn contents(guard: &mut Laid) -> Vec<u8> {
    guard.lock.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    guard.lock.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn legacy_locked_unreadable_headers_are_not_empty_observations() {
    for suffix in ["claim", "wait"] {
        for bytes in [
            b"".as_slice(),
            b"not JSON\n",
            b"{\"v\":2}\n",
            b"{\"v\":1}\n",
            &[0xff, 0xfe, b'\n'],
        ] {
            let temp = aterm_tempfile::tempdir().unwrap();
            let ledger = Ledger::at(temp.path());
            let path = temp.path().join(format!("1-999999-0.{suffix}"));
            let _guard = locked(&path, bytes);
            let listed = ledger.list();
            assert!(
                matches!(&listed, Err(error) if error.kind() == io::ErrorKind::InvalidData),
                "locked {suffix} with unknown header must refuse the snapshot: {listed:?}"
            );
            assert!(
                listed
                    .unwrap_err()
                    .to_string()
                    .contains(path.to_string_lossy().as_ref())
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(matches!(
                RecordLock::try_lock(crate::open_regular_reader(&path).unwrap(), RECORD_MAX),
                Err(std::fs::TryLockError::WouldBlock)
            ));
        }
    }
}

#[test]
fn legacy_locked_unreadable_headers_block_even_explicit_admission() {
    for mode in [Admit::Wait, Admit::Now] {
        let temp = aterm_tempfile::tempdir().unwrap();
        let ledger = Ledger::at(temp.path());
        let path = temp.path().join("1-999999-0.claim");
        let _guard = locked(&path, b"invalid live claim\n");
        let machine = Fixed::new(Reading::of_gib(48, 40, 14));
        let need = Need::new("must remain unadmitted").mem_gib(1);
        if mode == Admit::Wait {
            let waiter = Waiter::enter(&ledger, &machine, need).unwrap();
            match waiter.poll(&machine) {
                Polled::Waiting(waiter, short) => {
                    assert!(
                        short
                            .shorts
                            .iter()
                            .any(|why| matches!(why, Short::Ledger(_)))
                    );
                    assert!(waiter.record().held.is_none());
                }
                other => panic!("an unreadable live claim must keep admission waiting: {other:?}"),
            }
        } else {
            let result = admit_with(&ledger, &machine, need, mode, Pace::default(), &mut |_| {});
            assert!(matches!(result, Err(Error::Ledger(ref error))
                if error.kind() == io::ErrorKind::InvalidData));
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid live claim\n");
    }
}

#[test]
fn legacy_unlocked_bad_files_do_not_block_admission() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(temp.path());
    let path = temp.path().join("1-999999-0.claim");
    std::fs::write(&path, b"old invalid claim\n").unwrap();
    assert!(ledger.list().unwrap().is_empty());
    assert!(path.exists(), "an observation must not reap dead files");
    let machine = Fixed::new(Reading::of_gib(48, 40, 14));
    let claim = admit_with(
        &ledger,
        &machine,
        Need::new("ordinary admission").mem_gib(1),
        Admit::Now,
        Pace::default(),
        &mut |_| {},
    )
    .unwrap();
    assert!(claim.record().held.is_some());
}

#[test]
fn legacy_locked_oversized_records_are_not_accepted_by_their_prefix() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(temp.path());
    let (_, mut guard) = ledger
        .lay(&Need::new("live 24 GiB reservation").mem_gib(24), 1)
        .unwrap();
    let mut bytes = contents(&mut guard);
    bytes.resize(RECORD_MAX as usize + 1, b' ');
    rewrite(&mut guard, &bytes);
    assert!(matches!(ledger.list(), Err(error)
        if error.kind() == io::ErrorKind::InvalidData));
    assert_eq!(contents(&mut guard), bytes);
}

#[test]
fn legacy_locked_records_accept_the_exact_size_bound() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(temp.path());
    let (record, mut guard) = ledger
        .lay(&Need::new("record at the readable limit").mem_gib(24), 1)
        .unwrap();
    let mut bytes = contents(&mut guard);
    bytes.resize(RECORD_MAX as usize, b'\n');
    rewrite(&mut guard, &bytes);
    let observed = ledger.list().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].need.mem, record.need.mem);
    assert_eq!(contents(&mut guard), bytes);
}

#[cfg(unix)]
#[test]
fn legacy_claim_lock_and_metadata_use_the_same_open_file() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(temp.path());
    let (record, guard) = ledger
        .lay(&Need::new("original reservation").mem_gib(24), 1)
        .unwrap();
    let reader = crate::open_regular_reader(guard.path()).unwrap();
    std::fs::rename(guard.path(), temp.path().join("retained-original")).unwrap();
    let mut replacement = record.clone();
    replacement.need.mem = crate::floor::GIB;
    let text = replacement.head_line();
    std::fs::write(guard.path(), &text).unwrap();

    let observed = probe_opened(reader, guard.path(), false, false)
        .unwrap()
        .expect("the original file still has a held lock");
    assert_eq!(observed.need.mem, record.need.mem);
    assert_ne!(observed.need.mem, replacement.need.mem);
    assert_eq!(std::fs::read_to_string(guard.path()).unwrap(), text);
}
