// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
use super::*;
use std::io::Write;
pub(super) fn limits() -> Limits {
    Limits {
        records: 8,
        bytes: 8192,
        record_bytes: 1024,
    }
}
pub(super) fn open(dir: &Path) -> Log {
    {
        let mut log = Log::open(dir, "test", b"pinned", false, limits()).unwrap();
        log.finish_open().unwrap();
        log
    }
}

/// The production window stays bounded while append, cold replay and reopen preserve every
/// acknowledged record. Removing the budget retains twice its bytes and fails this oracle.
#[test]
fn a_long_log_keeps_its_decoded_window_within_budget() {
    const BODY: usize = 64 * 1024;
    let count = 2 * DECODED_WINDOW_BYTES / BODY;
    let limits = Limits {
        records: count + 1,
        bytes: 3 * DECODED_WINDOW_BYTES,
        record_bytes: BODY,
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = Log::open(dir.path(), "long", b"pinned", false, limits).unwrap();
    log.finish_open().unwrap();
    for index in 0..count {
        let index = u64::try_from(index).unwrap();
        let mut body = vec![0xa5; BODY];
        body[..8].copy_from_slice(&index.to_le_bytes());
        assert_eq!(log.append("app/1", body).unwrap(), Offset(index));
        assert!(log.log.residency().1 <= DECODED_WINDOW_BYTES);
    }
    let check = |log: &Log| {
        let (hot, bytes, retained) = log.log.residency();
        assert_eq!(retained, count);
        assert!(hot < retained && bytes <= DECODED_WINDOW_BYTES);
        let cold_before = log.log.cold_reads();
        for index in 0..count {
            let index = u64::try_from(index).unwrap();
            let records = log.read_range(Offset(index), 1).unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].seq, Offset(index));
            assert_eq!(records[0].subject, "app/1");
            assert_eq!(&records[0].body[..8], &index.to_le_bytes());
            assert!(records[0].body[8..].iter().all(|byte| *byte == 0xa5));
        }
        assert!(log.log.cold_reads() > cold_before);
        assert!(
            log.read_range(Offset(u64::try_from(count).unwrap()), 1)
                .unwrap()
                .is_empty()
        );
        assert!(log.log.residency().1 <= DECODED_WINDOW_BYTES);
    };
    check(&log);
    drop(log);
    let mut log = Log::open(dir.path(), "long", b"pinned", true, limits).unwrap();
    log.finish_open().unwrap();
    check(&log);
}

/// A same-length fault in a cold frame is an error. The raw broker's empty page is the
/// negative control: treating it as EOF would silently hide acknowledged history.
#[test]
fn an_unreadable_cold_record_is_not_the_end_of_the_journal() {
    use std::os::unix::fs::FileExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.log.set_resident_budget(0);
    log.append("app/1", vec![0xa5; 128]).unwrap();
    log.append("app/1", vec![0x5a; 128]).unwrap();
    let path = dir.path().join("test.astream");
    let bytes = std::fs::read(&path).unwrap();
    let at = bytes
        .windows(32)
        .position(|bytes| bytes == [0xa5; 32])
        .unwrap();
    OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .write_all_at(&[0], u64::try_from(at).unwrap())
        .unwrap();
    log.intact().unwrap();
    assert!(log.log.read_range(Offset::ZERO, 1).is_empty());
    assert_eq!(
        log.read_range(Offset::ZERO, 1).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        log.read_range(Offset(1), 1).unwrap()[0].body,
        vec![0x5a; 128]
    );
}

#[test]
fn writer_contention_is_typed_and_does_not_admit_a_second_writer() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut owner = open(dir.path());
    owner.append("app/1", vec![1]).unwrap();
    let blocked = Log::open(dir.path(), "test", b"pinned", true, limits())
        .err()
        .unwrap();
    assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
    assert!(blocked.to_string().contains("test writer admission"));
    owner.append("app/1", vec![2]).unwrap();
    drop(owner);
    let successor = open(dir.path());
    assert_eq!(successor.read_range(Offset::ZERO, 8).unwrap().len(), 2);
}

#[test]
fn physical_writer_contention_is_distinguished_from_dedicated_admission() {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(open(dir.path()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("test.astream"))
        .unwrap();
    aterm_uds::keep_out_of_children(&file).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the test physical writer is a WriterLock which explicitly unlocks before close"
    )]
    file.try_lock().unwrap();
    let physical = WriterLock(file);
    let blocked = Log::open_until(
        dir.path(),
        "test",
        b"pinned",
        true,
        limits(),
        Instant::now() + std::time::Duration::from_secs(60),
    )
    .err()
    .unwrap();
    assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
    assert!(blocked.to_string().contains("test physical recovery lock"));
    assert!(!blocked.to_string().contains("writer admission"));
    drop(physical);
    drop(open(dir.path()));
}

#[test]
fn lock_io_errors_keep_their_original_errno() {
    let error = lock_error(std::fs::TryLockError::Error(io::Error::from_raw_os_error(
        libc::ENOLCK,
    )));
    assert_eq!(error.raw_os_error(), Some(libc::ENOLCK));
    assert_ne!(error.kind(), io::ErrorKind::WouldBlock);
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
    let checkpoint = dir.path().join("test.checkpoint");
    let before_checkpoint = std::fs::metadata(&checkpoint).unwrap().ino();
    let before = std::fs::read(&path).unwrap();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"torn").unwrap();
    aterm_durable::durable(&file).unwrap();
    drop(file);
    let log = open(dir.path());
    assert_eq!(
        std::fs::metadata(checkpoint).unwrap().ino(),
        before_checkpoint
    );
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
fn checkpoint_io_failure_keeps_cause_and_fences_the_writer() {
    for errno in [libc::EIO, libc::ENOSPC] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut log = open(dir.path());
        let fault = aterm_durable::seam::FailingDirs::with(errno);
        let error = log.append("app/1", vec![2]).unwrap_err();
        drop(fault);
        assert_eq!(
            error.kind(),
            io::ErrorKind::Other,
            "the outcome remains uncertain"
        );
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("uncertain journal checkpoint"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("flush directory"), "{diagnostic}");
        assert!(
            diagnostic.contains(&dir.path().display().to_string()),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains(&format!("os error {errno}")),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("reopen and retry exact bytes"),
            "{diagnostic}"
        );
        let failure = error
            .get_ref()
            .unwrap()
            .downcast_ref::<CheckpointFailure>()
            .unwrap();
        assert!(std::error::Error::source(failure).is_some());
        assert_eq!(
            failure.error.kind(),
            io::Error::from_raw_os_error(errno).kind()
        );
        assert!(log.is_poisoned());
        assert!(log.read_range(Offset::ZERO, 1).is_err());
        assert!(log.append("app/1", vec![3]).is_err());
        drop(log);
        let mut reopened = open(dir.path());
        assert_eq!(reopened.read_range(Offset::ZERO, 8).unwrap().len(), 1);
        reopened.append("app/1", vec![3]).unwrap();
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

/// Where [`Log::append`] stands when the test seam runs, with the log's flushes so far:
/// after the astream write (the checkpoint still names the old prefix), after the
/// checkpoint that acknowledges it was renamed into place, after the caller's dependent
/// write ([`Log::append_then`]), and after the commit's full flush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Published,
    Checkpointed,
    Witnessed,
    Committed,
}
type Hook = Box<dyn FnMut(Step, astream_broker::FlushCounts)>;
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn append_step(step: Step, flushed: astream_broker::FlushCounts) {
    let hook = HOOK.with(|hook| hook.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook(step, flushed);
        HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
    }
}
pub(super) fn on_append(hook: impl FnMut(Step, astream_broker::FlushCounts) + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
pub(super) fn clear_append_hook() {
    HOOK.with(|slot| slot.borrow_mut().take());
}
/// Where [`read_committed`] stands when the reader's seam runs: the checkpoint
/// was opened and is not yet read; the one read was renamed over since its
/// open (its descriptor has no link) and is about to be read again; or the
/// checkpoint was read and the astream file is not yet opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reading {
    Opened,
    Relinked,
    Checkpointed,
}
type ReadHook = Box<dyn FnMut(Reading)>;
thread_local! {
    static READ_HOOK: std::cell::RefCell<Option<ReadHook>> = const { std::cell::RefCell::new(None) };
    static HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
pub(super) fn hashing() {
    HASHES.with(|count| count.set(count.get() + 1));
}
pub(super) fn read_step(step: Reading) {
    let hook = READ_HOOK.with(|hook| hook.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook(step);
        READ_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
    }
}
fn on_read(hook: impl FnMut(Reading) + 'static) {
    READ_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
fn read(dir: &Path, known: Option<&Prefix>) -> io::Result<Committed> {
    read_committed(dir, "test", b"pinned", limits(), known)
}
fn rebuilt(dir: &Path) -> (Vec<BrokerRecord>, Prefix) {
    match read(dir, None).unwrap() {
        Committed::Rebuilt { records, prefix } => (records, prefix),
        _ => panic!("a read with no known prefix replays from zero"),
    }
}

#[test]
fn a_reader_needs_no_lock_while_a_writer_holds_the_journal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut owner = open(dir.path());
    owner.append("app/1", b"first".to_vec()).unwrap();
    let blocked = Log::open(dir.path(), "test", b"pinned", true, limits())
        .err()
        .unwrap();
    assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
    let (records, prefix) = rebuilt(dir.path());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].body, b"first");
    assert_eq!(prefix.acknowledged(), owner.acknowledged());
    assert_eq!(prefix.records(), 1);
    owner.append("app/1", b"second".to_vec()).unwrap();
    assert!(!owner.is_poisoned());
    let Committed::Extended { records, prefix } = read(dir.path(), Some(&prefix)).unwrap() else {
        panic!("the reader's prefix still heads the journal");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(
        (records[0].seq, records[0].body.as_slice()),
        (Offset(1), &b"second"[..])
    );
    assert_eq!(prefix.acknowledged(), owner.acknowledged());
    assert!(matches!(
        read(dir.path(), Some(&prefix)).unwrap(),
        Committed::Unchanged
    ));
    assert!(
        read_committed(dir.path(), "test", b"foreign", limits(), None).is_err(),
        "a reader holds the anchor to the caller's pin"
    );
}

#[test]
fn unchanged_fresh_prefix_needs_no_repeated_digest_computation() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut owner = open(dir.path());
    owner
        .append("app/1", b"private journal value".to_vec())
        .unwrap();
    let (_, prefix) = rebuilt(dir.path());
    let before = HASHES.with(std::cell::Cell::get);
    for _ in 0..2 {
        assert!(matches!(
            read(dir.path(), Some(&prefix)).unwrap(),
            Committed::Unchanged
        ));
    }
    assert_eq!(
        HASHES.with(std::cell::Cell::get),
        before,
        "equal fresh bytes need no new digest"
    );
}

#[test]
fn retained_bytes_do_not_hide_a_changed_checkpoint_or_enter_debug_output() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut owner = open(dir.path());
    owner
        .append("app/1", b"private journal value".to_vec())
        .unwrap();
    let (_, prefix) = rebuilt(dir.path());
    let physical = std::fs::read(dir.path().join("test.astream")).unwrap();
    assert!(!format!("{prefix:?}").contains(&format!("{physical:?}")));
    let checkpoint = std::fs::read(dir.path().join("test.checkpoint")).unwrap();
    let mut changed = checkpoint.clone();
    changed[8] ^= 1;
    private_file::write_private(dir.path(), "test.checkpoint", &changed).unwrap();
    assert!(read(dir.path(), Some(&prefix)).is_err());
    private_file::write_private(dir.path(), "test.checkpoint", &checkpoint).unwrap();
    assert!(matches!(
        read(dir.path(), Some(&prefix)).unwrap(),
        Committed::Unchanged
    ));
}

#[test]
fn retained_prefix_requires_current_private_files_and_anchor() {
    use std::os::unix::fs::PermissionsExt;

    let dir = aterm_tempfile::tempdir().unwrap();
    let mut owner = open(dir.path());
    owner.append("app/1", b"value".to_vec()).unwrap();
    let (_, prefix) = rebuilt(dir.path());
    for name in ["test.anchor", "test.checkpoint", "test.astream"] {
        let path = dir.path().join(name);
        let private = std::fs::metadata(&path).unwrap().permissions();
        let mut exposed = private.clone();
        exposed.set_mode(private.mode() | 0o040);
        std::fs::set_permissions(&path, exposed).unwrap();
        let refused = read(dir.path(), Some(&prefix)).is_err();
        std::fs::set_permissions(&path, private).unwrap();
        assert!(refused, "a retained prefix cannot admit a nonprivate file");
        assert!(matches!(
            read(dir.path(), Some(&prefix)).unwrap(),
            Committed::Unchanged
        ));
    }
    private_file::write_private(dir.path(), "test.anchor", b"foreign").unwrap();
    assert!(read(dir.path(), Some(&prefix)).is_err());
    private_file::write_private(dir.path(), "test.anchor", b"pinned").unwrap();
    assert!(matches!(
        read(dir.path(), Some(&prefix)).unwrap(),
        Committed::Unchanged
    ));
}

#[test]
fn large_prefixes_still_verify_without_retaining_physical_bytes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let capacity = Limits {
        records: 1,
        bytes: PREFIX_BYTES + 4096,
        record_bytes: PREFIX_BYTES,
    };
    let mut owner = Log::open(dir.path(), "test", b"pinned", false, capacity).unwrap();
    owner.finish_open().unwrap();
    owner.append("app/1", vec![7; PREFIX_BYTES]).unwrap();
    let Committed::Rebuilt { prefix, .. } =
        read_committed(dir.path(), "test", b"pinned", capacity, None).unwrap()
    else {
        panic!("initial observation must provide the complete prefix");
    };
    let before = HASHES.with(std::cell::Cell::get);
    assert!(matches!(
        read_committed(dir.path(), "test", b"pinned", capacity, Some(&prefix)).unwrap(),
        Committed::Unchanged
    ));
    assert_eq!(HASHES.with(std::cell::Cell::get), before + 1);
}

/// What a read handed a known prefix answered, as a reader sees it: the verdict, the records it
/// gave, and what the new prefix counts; or the refusal's text.
#[derive(Debug, PartialEq)]
enum Answer {
    Unchanged,
    Extended(Vec<BrokerRecord>, Acknowledged, u64, usize),
    Rebuilt(Vec<BrokerRecord>, Acknowledged, u64, usize),
    Refused(String),
}

/// A prefix that keeps only its digest ([`Retain::Digest`]) answers every read as one that keeps
/// its bytes ([`Retain::Bytes`]) does: the journal unchanged, extended, rewritten in place at the
/// same length under a checkpoint that attests the new bytes, replaced by a copy (another inode),
/// truncated below its checkpoint, and read under narrowed limits each give the same verdict,
/// records and prefix, or the same refusal; each new prefix keeps its bytes only where asked to.
/// CONTROL: unchanged, the retained prefix hashes nothing and the digest-only one hashes once.
/// NEGATIVE CONTROL: a digest-only prefix answered `Unchanged` on its identity and length alone
/// answers the in-place rewrite `Unchanged`, where the retained one rebuilds.
#[test]
fn a_digest_only_prefix_answers_as_a_retained_one() {
    type Change = fn(&Path);
    fn none(_: &Path) {}
    fn extend(dir: &Path) {
        let mut log = Log::open(dir, "test", b"pinned", true, limits()).unwrap();
        log.finish_open().unwrap();
        log.append("app/1", vec![3; 30]).unwrap();
    }
    fn rewrite(dir: &Path) {
        let journal = dir.join("test.astream");
        substitute(&journal, 0, &[1; 30], &[9; 30]);
        let bytes = std::fs::read(&journal).unwrap();
        let mut checkpoint = (bytes.len() as u64).to_le_bytes().to_vec();
        checkpoint.extend_from_slice(&aterm_digest::Sha256::digest(&bytes));
        private_file::write_private(dir, "test.checkpoint", &checkpoint).unwrap();
    }
    fn copy(dir: &Path) {
        let bytes = std::fs::read(dir.join("test.astream")).unwrap();
        private_file::write_private(dir, "test.astream", &bytes).unwrap();
    }
    fn truncate(dir: &Path) {
        let journal = OpenOptions::new()
            .write(true)
            .open(dir.join("test.astream"))
            .unwrap();
        let length = journal.metadata().unwrap().len();
        journal.set_len(length - 1).unwrap();
    }
    fn answer(read: io::Result<Committed>, retain: Retain) -> Answer {
        let prefix = |prefix: &Prefix| {
            assert_eq!(
                prefix.retains_bytes(),
                retain == Retain::Bytes,
                "a new prefix keeps its bytes only where asked to"
            );
            (
                prefix.acknowledged(),
                prefix.records(),
                prefix.max_record_bytes,
            )
        };
        match read {
            Ok(Committed::Unchanged) => Answer::Unchanged,
            Ok(Committed::Extended { records, prefix: p }) => {
                let (acknowledged, count, max) = prefix(&p);
                Answer::Extended(records, acknowledged, count, max)
            }
            Ok(Committed::Rebuilt { records, prefix: p }) => {
                let (acknowledged, count, max) = prefix(&p);
                Answer::Rebuilt(records, acknowledged, count, max)
            }
            Err(error) => Answer::Refused(error.to_string()),
        }
    }
    let changes: [(&str, Change, Limits); 7] = [
        ("unchanged", none, limits()),
        ("extended", extend, limits()),
        (
            "rewritten in place under a new checkpoint",
            rewrite,
            limits(),
        ),
        ("replaced by a copy", copy, limits()),
        ("truncated below its checkpoint", truncate, limits()),
        (
            "read under fewer records",
            none,
            Limits {
                records: 1,
                ..limits()
            },
        ),
        (
            "read under a smaller record",
            none,
            Limits {
                record_bytes: 20,
                ..limits()
            },
        ),
    ];
    let mut hashed = [0; 2];
    for (what, change, read_under) in changes {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut log = open(dir.path());
        log.append("app/1", vec![1; 30]).unwrap();
        log.append("app/1", vec![2; 30]).unwrap();
        drop(log);
        let known = [Retain::Bytes, Retain::Digest].map(|retain| {
            let (committed, _) =
                read_committed_opened(dir.path(), "test", b"pinned", limits(), None, retain)
                    .unwrap();
            let Committed::Rebuilt { prefix, .. } = committed else {
                panic!("a read with no known prefix replays from zero");
            };
            assert_eq!(prefix.retains_bytes(), retain == Retain::Bytes);
            (retain, prefix)
        });
        change(dir.path());
        let answers = known.map(|(retain, prefix)| {
            let before = HASHES.with(std::cell::Cell::get);
            let read = read_committed_opened(
                dir.path(),
                "test",
                b"pinned",
                read_under,
                Some(&prefix),
                retain,
            )
            .map(|(committed, _)| committed);
            let answer = answer(read, retain);
            (answer, HASHES.with(std::cell::Cell::get) - before)
        });
        assert_eq!(answers[0].0, answers[1].0, "{what}: the same answer");
        if what == "unchanged" {
            assert_eq!(answers[0].0, Answer::Unchanged);
            hashed = [answers[0].1, answers[1].1];
        }
    }
    assert_eq!(
        hashed,
        [0, 1],
        "CONTROL: unchanged, retained bytes are compared and a digest is hashed again"
    );
}

#[test]
fn a_reader_racing_an_append_sees_the_old_prefix_or_the_new_one() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 40]).unwrap();
    let (_, before) = rebuilt(dir.path());
    let path = dir.path().to_path_buf();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let steps = std::rc::Rc::clone(&seen);
    on_append(move |step, _| {
        // The writer stands between its astream write and its checkpoint, or past
        // the checkpoint (before its commit or after it): one read with the old
        // prefix, the others without.
        let from_old = read(&path, Some(&before)).unwrap();
        let (records, _) = rebuilt(&path);
        match step {
            Step::Published => {
                assert!(matches!(from_old, Committed::Unchanged));
                assert_eq!(records.len(), 1);
            }
            Step::Checkpointed | Step::Witnessed | Step::Committed => {
                let Committed::Extended { records: new, .. } = from_old else {
                    panic!("an acknowledged append extends the old prefix");
                };
                assert_eq!(new.len(), 1);
                assert_eq!(
                    (new[0].seq, new[0].body.as_slice()),
                    (Offset(1), &[2; 40][..])
                );
                assert_eq!(records.len(), 2);
            }
        }
        steps.borrow_mut().push(step);
    });
    log.append("app/1", vec![2; 40]).unwrap();
    HOOK.with(|slot| slot.borrow_mut().take());
    assert_eq!(
        *seen.borrow(),
        [Step::Published, Step::Checkpointed, Step::Committed]
    );

    // A writer cut off mid-frame leaves part of a record past the checkpoint.
    let (_, acknowledged) = rebuilt(dir.path());
    let frame = BrokerRecord {
        seq: Offset(2),
        producer_id: 1,
        producer_seq: 3,
        subject: "app/1".into(),
        body: vec![3; 40],
        commit: None,
    }
    .encode()
    .unwrap();
    let journal = dir.path().join("test.astream");
    let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
    file.write_all(&frame[..frame.len() / 2]).unwrap();
    drop(file);
    let torn = std::fs::read(&journal).unwrap();
    assert!(matches!(
        read(dir.path(), Some(&acknowledged)).unwrap(),
        Committed::Unchanged
    ));
    assert_eq!(rebuilt(dir.path()).0.len(), 2);
    assert_eq!(
        std::fs::read(&journal).unwrap(),
        torn,
        "a reader never repairs or archives a tail"
    );
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("test-uncommitted-")
    }));
}

/// Rewrite the frame at byte `at` of `journal` in place, its body bytes `from`
/// replaced by `to` of the same length and the frame's CRC made valid again: a
/// whole, in-order record only the checkpoint's digest can tell apart.
fn substitute(journal: &Path, at: u64, from: &[u8], to: &[u8]) {
    let at = usize::try_from(at).unwrap();
    let mut bytes = std::fs::read(journal).unwrap();
    let (payload, consumed) = astream_wire::frame::decode_frame_slice(&bytes[at..])
        .unwrap()
        .unwrap();
    let mut payload = payload.to_vec();
    let body = payload
        .windows(from.len())
        .position(|window| window == from)
        .unwrap();
    payload[body..body + to.len()].copy_from_slice(to);
    let frame = astream_wire::frame::Frame::new(payload).encode().unwrap();
    assert_eq!(frame.len(), consumed);
    bytes[at..at + consumed].copy_from_slice(&frame);
    let (payload, _) = astream_wire::frame::decode_frame_slice(&bytes[at..])
        .unwrap()
        .unwrap();
    assert_eq!(BrokerRecord::from_payload(payload).unwrap().body, to);
    std::fs::write(journal, &bytes).unwrap();
}

#[test]
fn a_replaced_or_shorter_journal_rebuilds_and_changed_bytes_refuse() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 30]).unwrap();
    let journal = dir.path().join("test.astream");
    let checkpoint = dir.path().join("test.checkpoint");
    let old = (
        std::fs::read(&journal).unwrap(),
        std::fs::read(&checkpoint).unwrap(),
    );
    log.append("app/1", vec![2; 30]).unwrap();
    log.append("app/1", vec![3; 30]).unwrap();
    drop(log);
    let (_, three) = rebuilt(dir.path());
    assert_eq!(three.records(), 3);

    // The same bytes under another inode are another file. The copy lands by
    // rename while the old file is still linked, so it cannot reuse its inode.
    let bytes = std::fs::read(&journal).unwrap();
    private_file::write_private(dir.path(), "test.astream", &bytes).unwrap();
    let Committed::Rebuilt { records, prefix } = read(dir.path(), Some(&three)).unwrap() else {
        panic!("another inode rebuilds");
    };
    assert_eq!(records.len(), 3);
    assert_ne!(
        prefix.acknowledged().identity,
        three.acknowledged().identity
    );

    // An older copy restored in place shrinks the acknowledged prefix.
    OpenOptions::new()
        .write(true)
        .open(&journal)
        .unwrap()
        .set_len(old.0.len() as u64)
        .unwrap();
    private_file::write_private(dir.path(), "test.checkpoint", &old.1).unwrap();
    let Committed::Rebuilt {
        records,
        prefix: one,
    } = read(dir.path(), Some(&prefix)).unwrap()
    else {
        panic!("a shorter prefix rebuilds");
    };
    assert_eq!((records.len(), one.records()), (1, 1));

    // Acknowledged bytes rewritten as another whole record match neither the
    // reader's prefix nor the checkpoint.
    substitute(&journal, 0, &[1; 30], &[9; 30]);
    assert!(read(dir.path(), Some(&one)).is_err());
    assert!(read(dir.path(), None).is_err());
}

#[test]
fn a_reader_refuses_an_extension_whose_bytes_do_not_hash_to_the_checkpoint() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 30]).unwrap();
    let (_, one) = rebuilt(dir.path());
    log.append("app/1", vec![2; 30]).unwrap();
    drop(log);
    // The reader's prefix still heads the file; only the extension changed.
    let journal = dir.path().join("test.astream");
    substitute(&journal, one.acknowledged().length, &[2; 30], &[9; 30]);
    assert!(read(dir.path(), Some(&one)).is_err());
    assert!(read(dir.path(), None).is_err());
}

#[test]
fn a_divergent_history_written_over_the_same_inode_rebuilds() {
    let (dir, other) = (
        aterm_tempfile::tempdir().unwrap(),
        aterm_tempfile::tempdir().unwrap(),
    );
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 30]).unwrap();
    log.append("app/1", vec![2; 30]).unwrap();
    drop(log);
    let (_, two) = rebuilt(dir.path());
    let mut log = open(other.path());
    log.append("app/1", vec![3; 30]).unwrap();
    log.append("app/1", vec![4; 30]).unwrap();
    let equal = (
        std::fs::read(other.path().join("test.astream")).unwrap(),
        std::fs::read(other.path().join("test.checkpoint")).unwrap(),
    );
    log.append("app/1", vec![5; 30]).unwrap();
    drop(log);
    let longer = (
        std::fs::read(other.path().join("test.astream")).unwrap(),
        std::fs::read(other.path().join("test.checkpoint")).unwrap(),
    );
    let journal = dir.path().join("test.astream");
    // As `cp` does: another history's bytes written over this file in place,
    // with that history's checkpoint, first as long as the reader's, then longer.
    for ((bytes, checkpoint), records) in [(equal, 2), (longer, 3)] {
        std::fs::write(&journal, &bytes).unwrap();
        private_file::write_private(dir.path(), "test.checkpoint", &checkpoint).unwrap();
        let Committed::Rebuilt {
            records: replay,
            prefix,
        } = read(dir.path(), Some(&two)).unwrap()
        else {
            panic!("a history that does not extend the reader's prefix rebuilds");
        };
        assert_eq!(prefix.acknowledged().identity, two.acknowledged().identity);
        assert_eq!(replay.len(), records);
        assert_eq!(replay[0].body, [3; 30]);
    }
}

#[test]
fn a_reader_racing_an_append_after_its_checkpoint_read_keeps_the_old_prefix() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = std::rc::Rc::new(std::cell::RefCell::new(open(dir.path())));
    log.borrow_mut().append("app/1", vec![1; 40]).unwrap();
    let (_, before) = rebuilt(dir.path());
    let writer = std::rc::Rc::clone(&log);
    let fired = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = std::rc::Rc::clone(&fired);
    on_read(move |step| {
        if step != Reading::Checkpointed {
            return;
        }
        // A whole append lands between the checkpoint read and the astream open.
        writer.borrow_mut().append("app/1", vec![2; 40]).unwrap();
        count.set(count.get() + 1);
    });
    let raced = read(dir.path(), Some(&before));
    READ_HOOK.with(|slot| slot.borrow_mut().take());
    assert_eq!(fired.get(), 1);
    assert!(matches!(raced.unwrap(), Committed::Unchanged));
    let Committed::Extended { records, .. } = read(dir.path(), Some(&before)).unwrap() else {
        panic!("the next read sees the append");
    };
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].body, [2; 40]);
}

#[test]
fn a_checkpoint_renamed_over_mid_read_is_read_again_and_a_second_link_refused() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = std::rc::Rc::new(std::cell::RefCell::new(open(dir.path())));
    log.borrow_mut().append("app/1", vec![1; 30]).unwrap();
    // A writer's append saves a new checkpoint over the name after the reader
    // opened the old one: that descriptor is left no link, and is read again.
    let writer = std::rc::Rc::clone(&log);
    let steps = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&steps);
    on_read(move |step| {
        let first = seen.borrow().is_empty();
        seen.borrow_mut().push(step);
        if first {
            assert_eq!(step, Reading::Opened);
            writer.borrow_mut().append("app/1", vec![2; 30]).unwrap();
        }
    });
    let once = read(dir.path(), None);
    READ_HOOK.with(|slot| slot.borrow_mut().take());
    assert_eq!(
        *steps.borrow(),
        [
            Reading::Opened,
            Reading::Relinked,
            Reading::Opened,
            Reading::Checkpointed
        ]
    );
    let Committed::Rebuilt { records, .. } = once.unwrap() else {
        panic!("a first read rebuilds");
    };
    assert_eq!(records.len(), 2, "the newer checkpoint was read");
    drop(log);

    // A second link is refused, and not read again.
    std::fs::hard_link(dir.path().join("test.checkpoint"), dir.path().join("held")).unwrap();
    let relinked = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = std::rc::Rc::clone(&relinked);
    on_read(move |step| {
        if step == Reading::Relinked {
            count.set(count.get() + 1);
        }
    });
    let refused = read(dir.path(), None);
    READ_HOOK.with(|slot| slot.borrow_mut().take());
    assert!(refused.is_err());
    assert_eq!(relinked.get(), 0);

    // So is one made after the open, before the descriptor's link count is read.
    std::fs::remove_file(dir.path().join("held")).unwrap();
    assert!(read(dir.path(), None).is_ok(), "one link reads");
    let checkpoint = dir.path().join("test.checkpoint");
    let held = dir.path().join("held");
    on_read(move |step| {
        if step == Reading::Opened {
            std::fs::hard_link(&checkpoint, &held).unwrap();
        }
    });
    let refused = read(dir.path(), None);
    READ_HOOK.with(|slot| slot.borrow_mut().take());
    assert!(refused.is_err(), "a link made after the open is refused");
}

#[test]
fn past_three_rereads_the_checkpoint_read_last_stands() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = std::rc::Rc::new(std::cell::RefCell::new(open(dir.path())));
    log.borrow_mut().append("app/1", vec![1; 30]).unwrap();
    // Every read races a save: each open's checkpoint is renamed over before it is read.
    let writer = std::rc::Rc::clone(&log);
    let steps = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = std::rc::Rc::clone(&steps);
    on_read(move |step| {
        seen.borrow_mut().push(step);
        if step == Reading::Opened {
            writer.borrow_mut().append("app/1", vec![2; 30]).unwrap();
        }
    });
    let read_last = read(dir.path(), None);
    READ_HOOK.with(|slot| slot.borrow_mut().take());
    use Reading::{Checkpointed as C, Opened as O, Relinked as R};
    assert_eq!(*steps.borrow(), [O, R, O, R, O, R, O, C]);
    let Committed::Rebuilt { records, .. } = read_last.unwrap() else {
        panic!("a first read rebuilds");
    };
    assert_eq!(
        records.len(),
        4,
        "the fourth open's checkpoint, an older prefix of the five appended"
    );
}

#[test]
fn a_reader_refuses_an_unanchored_or_over_capacity_journal() {
    // Between Log::open and finish_open the checkpoint and the astream file
    // exist and the anchor does not: nothing is established to read.
    let fresh = aterm_tempfile::tempdir().unwrap();
    let opening = Log::open(fresh.path(), "test", b"pinned", false, limits()).unwrap();
    assert!(read(fresh.path(), None).is_err());
    drop(opening);

    // A real, hash-matching, decodable prefix past the reader's bounds.
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut log = open(dir.path());
    log.append("app/1", vec![1; 600]).unwrap();
    log.append("app/1", vec![2; 600]).unwrap();
    drop(log);
    assert!(read(dir.path(), None).is_ok());
    let tight = Limits {
        bytes: 1024,
        ..limits()
    };
    assert!(read_committed(dir.path(), "test", b"pinned", tight, None).is_err());
    let few = Limits {
        records: 1,
        ..limits()
    };
    assert!(read_committed(dir.path(), "test", b"pinned", few, None).is_err());

    let (_, known) = rebuilt(dir.path());
    assert!(read_committed(dir.path(), "test", b"pinned", few, Some(&known)).is_err());
    let small_record = Limits {
        record_bytes: 500,
        ..limits()
    };
    assert!(read_committed(dir.path(), "test", b"pinned", small_record, None).is_err());
    assert!(read_committed(dir.path(), "test", b"pinned", small_record, Some(&known)).is_err());
    assert!(Log::open(dir.path(), "test", b"pinned", true, small_record).is_err());

    let oversized = aterm_tempfile::tempdir().unwrap();
    let over_log = open(oversized.path());
    drop(over_log);
    let max_bytes = (limits().bytes + limits().record_bytes) as u64;
    std::fs::OpenOptions::new()
        .write(true)
        .open(oversized.path().join("test.astream"))
        .unwrap()
        .set_len(max_bytes + 1)
        .unwrap();
    assert!(Log::open(oversized.path(), "test", b"pinned", true, limits()).is_err());

    let mut reopened = Log::open(dir.path(), "test", b"pinned", true, limits()).unwrap();
    reopened.finish_open().unwrap();
    assert!(reopened.append("", b"x".to_vec()).is_err());
    assert!(reopened.append("/a/will", b"x".to_vec()).is_err());
    assert!(
        !reopened.is_poisoned(),
        "invalid subjects refuse pre-mutation without poisoning"
    );
    reopened.append("app/1", b"ok".to_vec()).unwrap();
    drop(reopened);

    for bad_subject in ["", "/a/will", "/a/commit", "/a/bind", "/a/retire"] {
        let crafted = aterm_tempfile::tempdir().unwrap();
        let log = open(crafted.path());
        drop(log);
        let frame = BrokerRecord {
            seq: Offset(0),
            producer_id: 1,
            producer_seq: 1,
            subject: bad_subject.into(),
            body: b"x".to_vec(),
            commit: None,
        }
        .encode()
        .unwrap();
        private_file::write_private(crafted.path(), "test.astream", &frame).unwrap();
        let mut ckpt = (frame.len() as u64).to_le_bytes().to_vec();
        ckpt.extend_from_slice(&aterm_digest::Sha256::digest(&frame));
        private_file::write_private(crafted.path(), "test.checkpoint", &ckpt).unwrap();
        assert!(
            read(crafted.path(), None).is_err(),
            "read_committed must refuse subject {bad_subject:?}"
        );
        assert!(
            Log::open(crafted.path(), "test", b"pinned", true, limits()).is_err(),
            "Log::open must refuse subject {bad_subject:?}"
        );
    }
}

thread_local! {
    /// Run each time this thread's writer open finds the lock held.
    static HELD: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}
pub(super) fn lock_held() {
    HELD.with(|held| {
        if let Some(held) = held.borrow_mut().as_mut() {
            held();
        }
    });
}

#[test]
fn a_writer_open_waits_out_a_held_lock_within_its_bound() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut owner = open(dir.path());
    owner.append("app/1", b"first".to_vec()).unwrap();
    let short = Log::open_until(
        dir.path(),
        "test",
        b"pinned",
        true,
        limits(),
        Instant::now() + std::time::Duration::from_millis(5),
    )
    .err()
    .unwrap();
    assert_eq!(short.kind(), io::ErrorKind::WouldBlock);

    let (waiting, waited) = std::sync::mpsc::channel();
    let path = dir.path();
    let successor = std::thread::scope(|scope| {
        let opener = scope.spawn(move || {
            HELD.with(|held| {
                *held.borrow_mut() = Some(Box::new(move || {
                    let _ = waiting.send(());
                }));
            });
            let opened = Log::open_until(
                path,
                "test",
                b"pinned",
                true,
                limits(),
                Instant::now() + std::time::Duration::from_secs(60),
            );
            HELD.with(|held| held.borrow_mut().take());
            opened
        });
        waited
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap();
        drop(owner);
        opener.join().unwrap()
    });
    let successor = successor.unwrap();
    assert_eq!(successor.read_range(Offset::ZERO, 8).unwrap().len(), 1);
}

/// A journal as `Log` wrote it at astream `94f7b77`: three appends and a clean drop, its
/// lock left out. These files stand for a log on a person's disk, so they are never
/// regenerated: a format that moves reads them first.
const FROZEN: [(&str, &[u8]); 4] = [
    (
        "test.anchor",
        include_bytes!("../tests/fixtures/frozen-journal/test.anchor"),
    ),
    (
        "test.astream",
        include_bytes!("../tests/fixtures/frozen-journal/test.astream"),
    ),
    (
        "test.astream.id",
        include_bytes!("../tests/fixtures/frozen-journal/test.astream.id"),
    ),
    (
        "test.checkpoint",
        include_bytes!("../tests/fixtures/frozen-journal/test.checkpoint"),
    ),
];

/// Every other test here writes and reads through the one pin, so a bump that moves the
/// BREC record or the frame without reading the old bytes stays green in all of them, and
/// strands every person's journal. This one reads bytes an earlier pin wrote, by the
/// reader that decodes them itself and by the writer's open through astream, then goes on
/// appending after them and leaves them as they were.
#[test]
fn a_journal_an_earlier_pin_wrote_opens_and_reads_at_this_pin() {
    let written: [(&str, &[u8]); 3] = [
        ("app/1", b"signed ciphertext"),
        ("app/2", &[0, 1, 2, 255]),
        ("app/1", b"third"),
    ];
    let holds = |records: &[&BrokerRecord]| {
        assert_eq!(records.len(), written.len());
        for (at, (record, (subject, body))) in records.iter().zip(written).enumerate() {
            assert_eq!(record.seq, Offset(at as u64));
            assert_eq!(record.subject, subject);
            assert_eq!(record.body, body);
        }
    };
    let dir = aterm_tempfile::tempdir().unwrap();
    for (name, bytes) in FROZEN {
        write_private_durable(dir.path(), name, bytes).unwrap();
    }
    let (read, _) = rebuilt(dir.path());
    holds(&read.iter().collect::<Vec<_>>());

    let mut log = Log::open(dir.path(), "test", b"pinned", true, limits()).unwrap();
    log.finish_open().unwrap();
    let replay = log.read_range(Offset::ZERO, 8).unwrap();
    holds(&replay.iter().map(AsRef::as_ref).collect::<Vec<_>>());
    assert_eq!(log.append("app/3", vec![7]).unwrap(), Offset(3));
    drop(log);

    let (read, _) = rebuilt(dir.path());
    holds(&read[..3].iter().collect::<Vec<_>>());
    assert_eq!(read[3].body, [7]);
    let frozen = FROZEN[1].1;
    let on_disk = std::fs::read(dir.path().join("test.astream")).unwrap();
    assert_eq!(&on_disk[..frozen.len()], frozen);
}
