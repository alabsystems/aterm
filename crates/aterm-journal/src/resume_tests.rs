// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A parked writer's resume ([`Log::park`], [`Log::resume`]): what it hashes, and what makes it
//! hash the whole prefix again.

use super::tests::{hashed_bytes, limits, open};
use super::*;
use std::os::unix::fs::FileExt as _;

/// A journal of `records` appends, its writer parked.
fn parked(dir: &Path, records: usize) -> Parked {
    let mut log = open(dir);
    for n in 0..records {
        log.append("app/1", vec![u8::try_from(n).unwrap(); 40])
            .unwrap();
    }
    log.park().expect("an untouched writer parks")
}

fn resume(dir: &Path, parked: Option<Parked>) -> io::Result<(Log, Reopened)> {
    Log::resume(parked, dir, "test", b"pinned", limits())
}

/// The bytes `run` hashed on this thread.
fn hashing<T>(run: impl FnOnce() -> T) -> (T, usize) {
    let before = hashed_bytes();
    let out = run();
    (out, hashed_bytes() - before)
}

fn length(dir: &Path) -> usize {
    usize::try_from(std::fs::metadata(dir.join("test.astream")).unwrap().len()).unwrap()
}

/// Rewrite one byte of the astream file in place, at its length.
fn rewrite(dir: &Path, at: u64) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("test.astream"))
        .unwrap();
    let mut byte = [0];
    file.read_exact_at(&mut byte, at).unwrap();
    file.write_all_at(&[byte[0] ^ 1], at).unwrap();
}

/// A parked writer resumes hashing nothing of its prefix, holds no lock while parked, and its
/// next append hashes that append alone; an open of the same journal hashes the prefix twice
/// (its recovery's and astream's prepare's), which is what a resume stands for.
#[test]
fn a_parked_writer_resumes_hashing_nothing_of_its_prefix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let parked = parked(dir.path(), 4);
    let whole = length(dir.path());
    // Parked, it holds no lock: another open is admitted, and changes nothing a resume judges.
    let (opened, by_open) = hashing(|| resume(dir.path(), None).unwrap());
    assert_eq!(opened.1, Reopened::Opened);
    assert_eq!(by_open, 2 * whole, "an open hashes the whole prefix twice");
    drop(opened);
    let ((mut log, how), by_resume) = hashing(|| resume(dir.path(), Some(parked)).unwrap());
    assert_eq!(how, Reopened::Resumed);
    assert_eq!(by_resume, 0, "a resume hashes nothing of the prefix");
    let (_, by_append) = hashing(|| log.append("app/1", vec![9; 40]).unwrap());
    assert_eq!(
        by_append,
        length(dir.path()) - whole,
        "an append hashes itself alone"
    );
    assert_eq!(log.acknowledged(), {
        drop(log);
        Log::open(dir.path(), "test", b"pinned", true, limits())
            .unwrap()
            .acknowledged()
    });
}

/// Another writer's append while one is parked moves the astream file's stamp: the resume hashes
/// the whole prefix, as an open does, and serves every record.
/// NEGATIVE CONTROL: with `Parked::seed` answering whatever the journal shows, the resume
/// hashes only the other writer's append and starts from the parked state:
/// `` left: Resumed right: Opened ``.
#[test]
fn another_writers_append_while_parked_is_hashed_whole_and_served() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let parked = parked(dir.path(), 3);
    let mut other = Log::open(dir.path(), "test", b"pinned", true, limits()).unwrap();
    other.append("app/1", vec![7; 40]).unwrap();
    drop(other);
    let ((log, how), hashed) = hashing(|| resume(dir.path(), Some(parked)).unwrap());
    assert_eq!(how, Reopened::Opened);
    assert_eq!(hashed, 2 * length(dir.path()));
    assert_eq!(log.read_range(Offset(0), 8).unwrap().len(), 4);
}

/// A byte of the prefix rewritten in place while the writer is parked moves the stamp: the resume
/// hashes the whole prefix and refuses the journal, as an open does.
/// NEGATIVE CONTROL: with `Parked::seed` ignoring the stamp, the prefix is not hashed and only
/// astream's frame CRC refuses the byte: `` a rewritten prefix is refused: Err(… frame payload
/// CRC … ``.
#[test]
fn a_prefix_rewritten_while_parked_is_hashed_and_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let parked = parked(dir.path(), 3);
    rewrite(dir.path(), 20);
    let resumed = resume(dir.path(), Some(parked)).map(|(_, how)| how);
    assert!(
        resumed
            .as_ref()
            .is_err_and(|error| error.to_string().contains("history changed")),
        "a rewritten prefix is refused: {resumed:?}"
    );
}

/// A checkpoint rolled back while the writer is parked is not the one it saved: the resume admits
/// the journal exactly as an open does, cutting the tail past the older checkpoint.
#[test]
fn a_checkpoint_rolled_back_while_parked_is_admitted_as_an_open_admits_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 40]).unwrap();
    let older = std::fs::read(dir.path().join("test.checkpoint")).unwrap();
    log.append("app/1", vec![2; 40]).unwrap();
    let parked = log.park().unwrap();
    write_private_durable(dir.path(), "test.checkpoint", &older).unwrap();
    let (log, how) = resume(dir.path(), Some(parked)).unwrap();
    assert_eq!(how, Reopened::Opened);
    assert_eq!(log.read_range(Offset(0), 8).unwrap().len(), 1);
}

/// A change no write of the writer made, reaching the astream file during its hold, parks
/// nothing: its stamp is no longer the writer's own.
/// NEGATIVE CONTROL: with the hold's witness kept across that change, the writer parks and
/// resumes over the rewritten byte: `` a changed hold parks nothing: true ``.
#[test]
fn a_change_during_the_hold_parks_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 40]).unwrap();
    rewrite(dir.path(), 20);
    log.append("app/1", vec![2; 40]).unwrap();
    let parked = log.park();
    assert!(
        parked.is_none(),
        "a changed hold parks nothing: {}",
        parked.is_some()
    );
}

/// A resumed writer serves no record below the last one it parked: it hashed none of them.
/// NEGATIVE CONTROL: with no floor on `read_range`, the first record is served:
/// `` below the parked head: Ok(..) ``.
#[test]
fn a_resumed_writer_serves_nothing_below_its_parked_head() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let parked = parked(dir.path(), 3);
    let (log, how) = resume(dir.path(), Some(parked)).unwrap();
    assert_eq!(how, Reopened::Resumed);
    let below = log.read_range(Offset(0), 8);
    assert!(
        below
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::InvalidInput),
        "below the parked head: {below:?}"
    );
    assert_eq!(log.read_range(Offset(2), 8).unwrap().len(), 1);
}

/// Of each [`RESUMES`] + 1 reopens in a row, the last hashes the whole prefix again, as an open.
/// NEGATIVE CONTROL: with the cap ignored, every reopen resumes:
/// `` left: Resumed right: Opened ``.
#[test]
fn every_resumes_th_resume_hashes_the_whole_prefix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut parked = Some(parked(dir.path(), 2));
    let whole = length(dir.path());
    for _ in 0..RESUMES {
        let (log, how) = resume(dir.path(), parked).unwrap();
        assert_eq!(how, Reopened::Resumed);
        parked = log.park();
    }
    let ((log, how), hashed) = hashing(|| resume(dir.path(), parked).unwrap());
    assert_eq!(how, Reopened::Opened);
    assert_eq!(hashed, 2 * whole);
    let (_, how) = resume(dir.path(), log.park()).unwrap();
    assert_eq!(how, Reopened::Resumed);
}
