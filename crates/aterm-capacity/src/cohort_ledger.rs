// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cohort-aware ledger operations, always relative to the retained backend directory.

use super::*;
use crate::{
    cohort::{Cohort, Lifetime},
    cohort_journal::Journal,
    cohort_namespace::Namespace,
};
use aterm_dirfd::{
    self as fd, AsFd as _, AtFlags, FileType, Mode as FileMode, OFlags, RenameFlags,
};
use aterm_uds::spawnfd::{BirthAttempt, BirthKey, NotSpawned, Spawned};

#[path = "cohort_checkpoint.rs"]
mod checkpoint;

use checkpoint::{Anchor, Capture, Transaction};

#[derive(Debug)]
struct Pending {
    before: String,
    before_admitted: bool,
    after: String,
    next: Journal,
    writer: BirthKey,
    /// A pre-flush origin is minted only after the whole known write/rename succeeded.
    /// A partial write before this point remains unknown and blocks; retry never rebases it
    /// onto a later observed generation, even when the desired bytes are locally retained.
    capture: Option<Capture>,
    rename: Option<String>,
    grant: Option<Held>,
}

#[derive(Debug)]
pub(super) struct Custody {
    namespace: Namespace,
    directory: File,
    /// Private write description, never copied into the inherited guard set.
    writer: File,
    journal: Journal,
    pending: Option<Pending>,
    unused: Option<NotSpawned>,
    actual_birth: Option<(BirthKey, Cohort)>,
    head: String,
    checkpoint: Anchor,
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

impl Custody {
    pub(super) fn adopt(
        namespace: Namespace,
        journal: Journal,
        record: &str,
        guard: &RecordLock,
        writer: File,
        path: &Path,
    ) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| refused("cohort record has no claim directory"))?;
        if parent.parent() != Some(namespace.active().as_path()) {
            return Err(refused(
                "cohort adoption is outside its original active plane",
            ));
        }
        let key = parent
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| refused("invalid claim directory name"))?;
        let active = namespace.open_active()?;
        let checkpoint = Anchor::open_name(&active, key, namespace.nonce())?;
        let directory = checkpoint.directory.try_clone()?;
        let name = guard_name(record)?;
        if path.file_name().is_none_or(|n| n != name.as_str()) {
            return Err(refused("cohort adoption has a different record name"));
        }
        read_only(guard)?;
        use std::os::fd::AsRawFd as _;
        // SAFETY: F_GETFL reads status flags of the separately transferred live writer.
        let flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::O_ACCMODE != libc::O_RDWR {
            return Err(refused(
                "cohort adoption needs a separate read-write controller descriptor",
            ));
        }
        let _ = aterm_uds::keep_out_of_children(&writer);
        same_file(guard, &writer)?;
        Ok(Self {
            namespace,
            directory,
            writer,
            journal,
            pending: None,
            unused: None,
            actual_birth: None,
            head: record
                .split_inclusive('\n')
                .next()
                .ok_or_else(|| refused("missing cohort head"))?
                .to_owned(),
            checkpoint,
        })
    }

    fn check(&self, lock: &RecordLock, path: &Path, transaction: &Transaction) -> io::Result<()> {
        self.namespace.validate()?;
        read_only(lock)?;
        same_file(lock, &self.writer)?;
        let named = fd::fstatat(
            &self.directory,
            path.file_name()
                .ok_or_else(|| refused("claim has no name"))?,
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        let held = fd::fstat(lock.as_fd())?;
        // SAFETY: geteuid takes no arguments and reads this process's effective identity.
        let uid = unsafe { libc::geteuid() };
        if (held.st_dev, held.st_ino) != (named.st_dev, named.st_ino)
            || !FileType::from_raw_mode(named.st_mode).is_file()
            || named.st_nlink != 1
            || named.st_uid != uid
            || named.st_mode & 0o077 != 0
        {
            return Err(refused(
                "cohort claim path no longer names its owned regular file",
            ));
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| refused("invalid claim name"))?;
        let all = snapshot(&self.writer)?;
        if let Some(pending) = &self.pending
            && let Some(capture) = &pending.capture
        {
            capture.check(&self.directory, &self.writer)?;
        } else if transaction.observe(&self.directory, &self.writer, name)? != all {
            return Err(refused("cohort bytes differ from their durable checkpoint"));
        }
        let observed = self.pending.as_ref().map_or(all, |p| p.before.clone());
        if observed.split_inclusive('\n').next() != Some(self.head.as_str()) {
            return Err(refused("cohort declared budget or owner head changed"));
        }
        let (_, journal) = Journal::decode(
            &observed,
            self.pending
                .as_ref()
                .map_or(name.ends_with(CLAIM), |p| p.before_admitted),
            self.namespace.nonce(),
        )?;
        if journal != self.journal {
            return Err(refused("cohort journal changed outside its owning writer"));
        }
        let after = fd::fstatat(
            &self.directory,
            path.file_name()
                .ok_or_else(|| refused("claim has no name"))?,
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
        if (after.st_dev, after.st_ino) != (held.st_dev, held.st_ino) {
            return Err(refused("cohort path changed during its held-writer read"));
        }
        Ok(())
    }
}

fn guard_name(record: &str) -> io::Result<String> {
    let first = record
        .split_inclusive('\n')
        .next()
        .ok_or_else(|| refused("missing cohort head"))?;
    // Only the filename stem is used here; strict Journal decoding and the checkpoint's exact
    // name/origin validation remain mandatory before adoption can return authority.
    let head: aterm_json::Value =
        aterm_json::from_str(first).map_err(|_| refused("invalid cohort head"))?;
    let value = |key| {
        head.get(key)
            .and_then(aterm_json::Value::as_u64)
            .ok_or_else(|| refused("missing cohort filename field"))
    };
    Ok(format!(
        "{:013}-{}-{}{CLAIM}",
        value("start_ms")?,
        value("pid")?,
        value("seq")?
    ))
}

fn open(directory: &File, name: &str, create: bool) -> io::Result<File> {
    open_mode(directory, name, create, true)
}

fn open_mode(directory: &File, name: &str, create: bool, write: bool) -> io::Result<File> {
    let mut flags = (if write { OFlags::RDWR } else { OFlags::RDONLY })
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC
        | OFlags::NONBLOCK;
    if create {
        flags |= OFlags::CREATE | OFlags::EXCL;
    }
    let file = File::from(fd::openat(
        directory,
        name,
        flags,
        FileMode::RUSR | FileMode::WUSR,
    )?);
    let stat = fd::fstat(&file)?;
    // SAFETY: geteuid takes no arguments and reads the effective identity of this process.
    let uid = unsafe { libc::geteuid() };
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || stat.st_nlink != 1
        || stat.st_uid != uid
        || stat.st_mode & 0o077 != 0
    {
        return Err(refused(
            "cohort ledger entry is not a private owned regular file",
        ));
    }
    Ok(file)
}

fn same_file(guard: &RecordLock, writer: &File) -> io::Result<()> {
    let guard = fd::fstat(guard.as_fd())?;
    let writer = fd::fstat(writer)?;
    if (guard.st_dev, guard.st_ino) != (writer.st_dev, writer.st_ino) {
        return Err(refused(
            "private cohort writer opens another inode than its guard",
        ));
    }
    Ok(())
}

fn read_only(guard: &RecordLock) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: F_GETFL reads status flags from the live held descriptor without changing it.
    let flags = unsafe { libc::fcntl(guard.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(refused("cohort guard must be a read-only open description"));
    }
    Ok(())
}

fn snapshot(file: &File) -> io::Result<String> {
    use std::os::unix::fs::{FileExt as _, MetadataExt as _};
    let stamp = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.ctime(),
            m.ctime_nsec(),
            m.mtime(),
            m.mtime_nsec(),
        )
    };
    let before = file.metadata()?;
    let mut bytes = vec![0u8; RECORD_MAX as usize + 1];
    let mut length = 0;
    while length < bytes.len() {
        let read = file.read_at(&mut bytes[length..], length as u64)?;
        if read == 0 {
            break;
        }
        length += read;
    }
    if length as u64 > RECORD_MAX || stamp(&before) != stamp(&file.metadata()?) {
        return Err(refused(
            "cohort record changed during its bounded held-file read",
        ));
    }
    bytes.truncate(length);
    String::from_utf8(bytes).map_err(|_| refused("cohort record is not UTF-8"))
}

pub(super) fn turn(namespace: &Namespace) -> io::Result<Result<Turn, Option<u32>>> {
    let directory = namespace.open_backend()?;
    // The rendezvous is persistent, unlike a new claim. Follow no link and require its type.
    let file = match open(&directory, LOCK, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => match open(&directory, LOCK, true)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                open(&directory, LOCK, false)?
            }
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };
    match RecordLock::try_lock(file, RECORD_MAX) {
        Ok(mut lock) => {
            lock.set_len(0)?;
            lock.seek(SeekFrom::Start(0))?;
            writeln!(lock, "{}", std::process::id())?;
            namespace.validate()?;
            Ok(Ok(Turn { _lock: lock }))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(Err(None)),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

pub(super) fn lay(namespace: &Namespace, record: Record) -> io::Result<(Record, Laid)> {
    crate::ledger::checkout_exact(&record.checkout)?;
    let active = namespace.open_active()?;
    let taking = format!("{}{TAKING}", record.stem());
    let waiting = format!("{}{WAIT}", record.stem());
    let (checkpoint, writer) = Anchor::create(&active, &taking, namespace.nonce())?;
    let root = namespace.active().join(checkpoint.key());
    let directory = checkpoint.directory.try_clone()?;
    let mut transaction = checkpoint.lock(&directory)?;
    let _ = aterm_uds::keep_out_of_children(&writer);
    let guard = open_mode(&directory, &taking, false, false)?;
    let lock = RecordLock::try_lock(guard, RECORD_MAX).map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => refused("new private cohort claim was already locked"),
        std::fs::TryLockError::Error(error) => error,
    })?;
    same_file(&lock, &writer)?;
    let (head, journal) = Journal::head(&record.head_line(), namespace.nonce())?;
    let custody = Custody {
        namespace: namespace.clone(),
        directory,
        writer,
        journal,
        pending: None,
        unused: None,
        actual_birth: None,
        head: head.clone(),
        checkpoint,
    };
    let mut laid = Laid {
        lock,
        path: root.join(&taking),
        granted: None,
        custody: Some(custody),
    };
    let writer = &mut laid.custody.as_mut().expect("cohort claim").writer;
    writer.write_all(head.as_bytes())?;
    let custody = laid.custody.as_ref().expect("cohort claim");
    fd::renameat_with(
        &custody.directory,
        taking.as_str(),
        &custody.directory,
        waiting.as_str(),
        RenameFlags::NOREPLACE,
    )?;
    laid.path = root.join(&waiting);
    let capture =
        transaction.capture(&custody.directory, &custody.writer, &waiting, &head, false)?;
    transaction.commit(&custody.directory, &custody.writer, &capture)?;
    custody.check(&laid.lock, &laid.path, &transaction)?;
    Ok((record, laid))
}

pub(super) fn scan(
    namespace: &Namespace,
    reap: bool,
    named: &dyn Fn(&str) -> bool,
) -> io::Result<Vec<Record>> {
    let backend = namespace.open_backend()?;
    for entry in fd::Dir::read_from(backend.as_fd())? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| refused("non-UTF8 backend child"))?;
        if !matches!(
            name,
            "." | ".." | LOCK | ".ready" | "active.noindex" | "retired.noindex"
        ) && !crate::leadership::metadata_name(name.as_bytes())
        {
            return Err(refused("unknown cohort backend child"));
        }
    }
    let active = namespace.open_active()?;
    let mut records = Vec::new();
    let mut count = 0;
    for entry in fd::Dir::read_from(active.as_fd())? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| refused("non-UTF8 cohort entry"))?;
        if matches!(name, "." | "..") {
            continue;
        }
        count += 1;
        if count > 4096 || !checkpoint::metadata_name(name) {
            return Err(refused("unknown or unbounded active cohort entry"));
        }
        let anchor = Anchor::open_name(&active, name, namespace.nonce())?;
        let directory = anchor.directory.try_clone()?;
        let mut transaction = if reap {
            anchor.lock_recover(&directory)?
        } else {
            anchor.lock(&directory)?
        };
        if reap {
            transaction.recover_pending(&directory, namespace.nonce())?
        }
        let working = transaction.scan_name()?.to_owned();
        if let Some(record) = probe_under(
            namespace,
            &directory,
            &working,
            working.ends_with(CLAIM),
            reap,
            &mut transaction,
        )? && named(&working)
        {
            records.push(record)
        }
    }
    namespace.validate()?;
    records.sort_by_key(Record::order);
    Ok(records)
}

#[cfg(test)]
fn probe(
    namespace: &Namespace,
    directory: &File,
    name: &str,
    admitted: bool,
    reap: bool,
) -> io::Result<Option<Record>> {
    let checkpoint = Anchor::open(directory, name, namespace.nonce())?;
    let mut transaction = checkpoint.lock(directory)?;
    probe_under(namespace, directory, name, admitted, reap, &mut transaction)
}

fn probe_under(
    namespace: &Namespace,
    directory: &File,
    name: &str,
    admitted: bool,
    reap: bool,
    transaction: &mut Transaction,
) -> io::Result<Option<Record>> {
    if reap {
        transaction.recover_pending(directory, namespace.nonce())?;
    }
    let file = match open_mode(directory, name, false, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return if transaction.missing_retired() {
                Ok(None)
            } else {
                Err(refused("active checkpoint lost its working claim"))
            };
        }
        Err(error) => return Err(error),
    };
    if transaction.retired(directory, &file, name)? {
        if reap {
            let lock = RecordLock::try_lock(file, RECORD_MAX).map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    io::Error::new(io::ErrorKind::WouldBlock, "terminal guard is still held")
                }
                std::fs::TryLockError::Error(error) => error,
            })?;
            transaction.archive(namespace, &lock)?;
        }
        return Ok(None);
    }
    let before = transaction.observe(directory, &file, name)?;
    match RecordLock::try_lock(file, RECORD_MAX) {
        Ok(lock) => {
            // Re-read under the free description's lock. A prior read may precede ARM.
            let observed = transaction.observe(directory, &lock, name)?;
            let (record, journal) = Journal::decode(&observed, admitted, namespace.nonce())?;
            // Only a reaping scan's durable tombstone releases a drained admitted claim. Until
            // it commits, every other reader keeps the whole budget, as `claim_state` reads it
            // Retained; a free waiter's record holds no budget and no custody.
            if journal.lifetime() == Lifetime::Drained && (reap || !admitted) {
                if reap {
                    retire_drained(namespace, directory, name, &lock, &observed, transaction)?;
                }
                Ok(None)
            } else {
                let mut record = record;
                record.accounting = Accounting::Whole;
                Ok(Some(record))
            }
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            Journal::decode(&before, admitted, namespace.nonce()).map(|(r, _)| Some(r))
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Retire the claim `name` that no other description holds (`lock` is the guard lock the scan
/// took on it), whose journal `observed` under that lock the scan found drained: the
/// description's retirement, and an armed or bound birth's finish with it, in one durable
/// tombstone committed before covered archive transfer and protocol-history compaction. Only
/// a reaping scan reaches it, inside the claim's checkpoint `transaction`.
///
/// # Errors
/// The private writer cannot be opened or names another inode than `lock`, or the tombstone's
/// capture or commit fails, or archive transfer, original-inventory validation or its covering
/// I/O fails. Unknown payloads are preserved.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityCohort",
        action = "FinishDrainedCohort",
        project = "aterm_capacity::ledger::failure_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CapacityCohort",
        action = "RetireDescription",
        project = "aterm_capacity::ledger::failure_tests::project"
    )
)]
fn retire_drained(
    namespace: &Namespace,
    directory: &File,
    name: &str,
    lock: &RecordLock,
    observed: &str,
    transaction: &mut Transaction,
) -> io::Result<()> {
    let writer = open(directory, name, false)?;
    same_file(lock, &writer)?;
    let capture = transaction.capture(directory, &writer, name, observed, true)?;
    transaction.commit(directory, &writer, &capture)?;
    transaction.archive(namespace, lock)
}

pub(super) fn claim_state(
    namespace: &Namespace,
    reference: &crate::CohortClosureRef,
    reap: bool,
) -> io::Result<CohortClaimState> {
    if reference.namespace() != namespace.nonce() {
        return Ok(CohortClaimState::Unknown);
    }
    let active = namespace.open_active()?;
    match fd::fstatat(&active, reference.key(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => {}
        Err(error) if error.is_noent() => {
            return checkpoint::archived_state(namespace, reference, reap);
        }
        Err(error) => return Err(error.into()),
    }
    let anchor = Anchor::open_name(&active, &reference.key(), namespace.nonce())?;
    let directory = anchor.directory.try_clone()?;
    if !anchor.matches_guard(reference.guard()) {
        return Ok(CohortClaimState::Unknown);
    }
    let mut transaction = if reap {
        anchor.lock_recover(&directory)?
    } else {
        anchor.lock(&directory)?
    };
    if !transaction.matches_claim(reference.marker(), reference.guard(), namespace.nonce())? {
        return Ok(CohortClaimState::Unknown);
    }
    if reap {
        transaction.recover_pending(&directory, namespace.nonce())?;
        let name = transaction
            .working_name()
            .ok_or_else(|| refused("claim recovery has no committed generation"))?
            .to_owned();
        probe_under(
            namespace,
            &directory,
            &name,
            name.ends_with(CLAIM),
            true,
            &mut transaction,
        )?;
        // A successful reap may have transferred the whole original directory atomically.
        match std::fs::symlink_metadata(namespace.active().join(reference.key())) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return checkpoint::archived_state(namespace, reference, reap);
            }
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    }
    let state = transaction.claim_state(
        &directory,
        reference.marker(),
        reference.guard(),
        namespace.nonce(),
    )?;
    let state =
        if state == CohortClaimState::Closed && !transaction.active_closed(namespace, reference)? {
            CohortClaimState::Unknown
        } else {
            state
        };
    namespace.validate()?;
    Ok(state)
}

impl Laid {
    pub(crate) fn check_cohort_namespace(&self, expected: &Namespace) -> io::Result<()> {
        expected.validate()?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("host leadership requires actual cohort custody"))?;
        if &custody.namespace != expected {
            return Err(refused("claim belongs to a different capacity namespace"));
        }
        self.check_custody()
    }

    pub(crate) fn cohort_closure_ref(&self) -> io::Result<crate::CohortClosureRef> {
        use std::os::unix::fs::MetadataExt as _;
        self.check_custody()?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no cohort closure reference"))?;
        let m = self.lock.metadata()?;
        custody.checkpoint.closure_ref(
            custody.namespace.nonce(),
            ClaimGuardIdentity {
                device: m.dev(),
                inode: m.ino(),
            },
        )
    }
    pub(crate) fn append_cohort(&mut self, line: &str) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.check_locked(&transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        let mut expected = snapshot(&custody.writer)?;
        if expected.len().saturating_add(line.len()) as u64 > RECORD_MAX - WROTE_LINE as u64 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "cohort record is full",
            ));
        }
        expected.push_str(line);
        self.stage(
            &mut transaction,
            expected,
            custody.journal.clone(),
            None,
            None,
        )
    }

    pub(crate) fn wrote_cohort(&mut self, at: &mut Option<u64>, bytes: u64) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.check_locked(&transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        let mut expected = snapshot(&custody.writer)?;
        let offset = match *at {
            Some(offset) => offset,
            None => expected.len() as u64,
        };
        if offset.saturating_add(WROTE_LINE as u64) > RECORD_MAX {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "cohort record is full",
            ));
        }
        let offset_usize =
            usize::try_from(offset).map_err(|_| refused("write offset is too large"))?;
        if offset_usize > expected.len()
            || (offset_usize < expected.len() && offset_usize + WROTE_LINE > expected.len())
        {
            return Err(refused(
                "measured-write offset is outside its captured record",
            ));
        }
        if offset_usize == expected.len() {
            expected.push_str(&Record::wrote_line(bytes));
        } else {
            let old = expected
                .get(offset_usize..offset_usize + WROTE_LINE)
                .ok_or_else(|| refused("measured-write offset splits its captured text"))?;
            if !old.starts_with("{\"wrote\":") || !old.ends_with("}\n") {
                return Err(refused(
                    "measured-write offset no longer names its recorded line",
                ));
            }
            expected.replace_range(
                offset_usize..offset_usize + WROTE_LINE,
                &Record::wrote_line(bytes),
            );
        }
        self.stage(
            &mut transaction,
            expected,
            custody.journal.clone(),
            None,
            None,
        )?;
        *at = Some(offset);
        Ok(())
    }

    pub(crate) fn cohort_writer_descriptor(&self) -> io::Result<std::os::fd::OwnedFd> {
        self.check_custody()?;
        self.custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no cohort writer"))?
            .writer
            .as_fd()
            .try_clone_to_owned()
    }
    pub(crate) fn check_custody(&self) -> io::Result<()> {
        if self.custody.is_some() {
            let transaction = self.transaction()?;
            self.check_locked(&transaction)?;
        }
        Ok(())
    }

    fn transaction(&self) -> io::Result<Transaction> {
        use std::os::unix::fs::MetadataExt as _;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no cohort transaction"))?;
        custody.namespace.validate()?;
        read_only(&self.lock)?;
        same_file(&self.lock, &custody.writer)?;
        let m = self.lock.metadata()?;
        if !custody.checkpoint.matches_guard(ClaimGuardIdentity {
            device: m.dev(),
            inode: m.ino(),
        }) {
            return Err(refused(
                "controller lost its original immutable guard binding",
            ));
        }
        custody.checkpoint.lock_recover(&custody.directory)
    }

    fn check_locked(&self, transaction: &Transaction) -> io::Result<()> {
        if let Some(custody) = &self.custody {
            if custody.pending.is_some() {
                return Err(refused(
                    "cohort write remains uncertain; retry its durable event first",
                ));
            }
            custody.check(&self.lock, &self.path, transaction)?;
        }
        Ok(())
    }

    pub(super) fn admit_cohort(&mut self, record: &mut Record, grant: Held) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        let held = self.granted.unwrap_or(grant);
        let claim = format!("{}{CLAIM}", record.stem());
        let custody = self.custody.as_ref().expect("cohort claim");
        if self.granted.is_none() || self.path.file_name().is_none_or(|n| n != claim.as_str()) {
            let mut expected = snapshot(&custody.writer)?;
            if self.granted.is_none() {
                expected.push_str(&Record::held_line(held));
            }
            self.stage(
                &mut transaction,
                expected,
                custody.journal.clone(),
                Some(claim),
                Some(held),
            )?;
        }
        record.held = Some(held);
        Ok(())
    }

    fn event(
        &mut self,
        transaction: &mut Transaction,
        text: String,
        next: Journal,
    ) -> io::Result<()> {
        self.check_locked(transaction)?;
        let prefix = snapshot(
            &self
                .custody
                .as_ref()
                .ok_or_else(|| refused("legacy claim has no cohort writer"))?
                .writer,
        )?;
        if prefix.len().saturating_add(text.len()) as u64 > RECORD_MAX - WROTE_LINE as u64 {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "cohort journal is full",
            ));
        }
        let mut expected = prefix;
        expected.push_str(&text);
        self.stage(transaction, expected, next, None, None)
    }

    fn stage(
        &mut self,
        transaction: &mut Transaction,
        after: String,
        next: Journal,
        rename: Option<String>,
        grant: Option<Held>,
    ) -> io::Result<()> {
        self.check_locked(transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        transaction.ensure_room(&custody.directory, &custody.writer)?;
        if after.len() as u64 > RECORD_MAX {
            return Err(refused("cohort record is full"));
        }
        let writer = BirthAttempt::new()?.key().clone();
        let before = snapshot(&self.custody.as_ref().expect("cohort claim").writer)?;
        let before_admitted = self.path.to_str().is_some_and(|p| p.ends_with(CLAIM));
        self.custody
            .as_mut()
            .ok_or_else(|| refused("legacy claim cannot bind a cohort"))?
            .pending = Some(Pending {
            before,
            before_admitted,
            after,
            next,
            writer,
            capture: None,
            rename,
            grant,
        });
        self.retry_event(transaction)
    }

    /// Retry the exact locally retained event, preserving all earlier durable state if torn.
    /// The live writer check prevents a fork's copied memory from completing another owner.
    fn retry_event(&mut self, transaction: &mut Transaction) -> io::Result<()> {
        let custody = self
            .custody
            .as_mut()
            .ok_or_else(|| refused("legacy claim has no cohort custody"))?;
        let Some(pending) = &custody.pending else {
            return Ok(());
        };
        if !pending.writer.is_current() {
            return Err(refused("cohort event writer is no longer current"));
        }
        custody.check(&self.lock, &self.path, transaction)?;
        if pending.capture.is_none() {
            custody.writer.set_len(0)?;
            custody.writer.seek(SeekFrom::Start(0))?;
            #[cfg(test)]
            if FAIL_EVENT_PARTIAL_WRITE.with(|failure| failure.replace(false)) {
                custody
                    .writer
                    .write_all(&pending.after.as_bytes()[..pending.after.len() / 2])?;
                return Err(io::Error::other(
                    "injected partial cohort write before origin capture",
                ));
            }
            custody.writer.write_all(pending.after.as_bytes())?;
            if let Some(name) = &pending.rename
                && self.path.file_name().is_none_or(|n| n != name.as_str())
            {
                fd::renameat_with(
                    &custody.directory,
                    self.path
                        .file_name()
                        .ok_or_else(|| refused("claim has no name"))?,
                    &custody.directory,
                    name.as_str(),
                    RenameFlags::NOREPLACE,
                )?;
                self.path = self
                    .path
                    .parent()
                    .ok_or_else(|| refused("claim lost its root"))?
                    .join(name);
            }
            let name = self
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| refused("claim has no name"))?;
            let capture = transaction.capture(
                &custody.directory,
                &custody.writer,
                name,
                &pending.after,
                false,
            )?;
            custody.pending.as_mut().expect("pending event").capture = Some(capture);
        }
        #[cfg(test)]
        if FAIL_EVENT_AFTER_WRITE.with(|failure| failure.replace(false)) {
            return Err(io::Error::other(
                "injected custody failure after write and before flush",
            ));
        }
        let pending = custody.pending.as_ref().expect("pending event");
        transaction.commit(
            &custody.directory,
            &custody.writer,
            pending.capture.as_ref().expect("captured event"),
        )?;
        custody.journal = pending.next.clone();
        if let Some(grant) = pending.grant {
            self.granted = Some(grant);
        }
        custody.pending = None;
        Ok(())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "DurableArm",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    pub(crate) fn arm_cohort(&mut self) -> io::Result<BirthAttempt> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.check_locked(&transaction)?;
        if self.granted.is_none() {
            return Err(refused("a cohort must be admitted before ARM"));
        }
        let attempt = BirthAttempt::new()?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim cannot ARM a cohort"))?;
        let (line, next) = custody.journal.arm(attempt.key())?;
        if let Err(error) = self.event(&mut transaction, line, next) {
            if self.custody.as_ref().is_some_and(|c| c.pending.is_some())
                && let Ok(proof) = attempt.cancel()
            {
                self.custody.as_mut().expect("cohort claim").unused = Some(proof);
            }
            return Err(error);
        }
        Ok(attempt)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "BindActualChild",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    pub(crate) fn bind_cohort(&mut self, spawned: &mut Spawned) -> io::Result<Cohort> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        if let Some((key, cohort)) = self.custody.as_ref().and_then(|c| c.actual_birth.as_ref())
            && key == spawned.key()
            && key.is_current()
            && self
                .custody
                .as_ref()
                .is_some_and(|c| c.journal.bound(&cohort.record()))
        {
            return Ok(cohort.clone());
        }
        let cohort = Cohort::of_spawned(spawned)?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim cannot bind a cohort"))?;
        let (line, next) = custody.journal.bind(spawned.key(), &cohort.record())?;
        self.custody.as_mut().expect("cohort claim").actual_birth =
            Some((spawned.key().clone(), cohort.clone()));
        self.event(&mut transaction, line, next)?;
        Ok(cohort)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "CancelMatchingNoChild",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    pub(crate) fn cancel_cohort(&mut self, proof: NotSpawned) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim cannot cancel a cohort"))?;
        let (line, next) = custody.journal.cancel(proof.key())?;
        self.event(&mut transaction, line, next)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "FinishDrainedCohort",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    pub(crate) fn finish_cohort(&mut self) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.check_locked(&transaction)?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no cohort to finish"))?;
        if let Some((line, next)) = custody.journal.finish()? {
            self.event(&mut transaction, line, next)?;
        }
        self.check_locked(&transaction)
    }

    pub(crate) fn cohort_bound(&self, cohort: &Cohort) -> bool {
        self.check_custody().is_ok()
            && self
                .custody
                .as_ref()
                .is_some_and(|c| c.pending.is_none() && c.journal.bound(&cohort.record()))
    }

    pub(crate) fn is_cohort(&self) -> bool {
        self.custody.is_some()
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "FinishDrainedCohort",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "RetireDescription",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    pub(super) fn drop_cohort(&mut self) {
        let Ok(mut transaction) = self.transaction() else {
            return;
        };
        if self.custody.as_ref().is_some_and(|c| c.unused.is_some()) {
            let _ = self
                .retry_event(&mut transaction)
                .and_then(|()| self.finish_unused(&mut transaction));
        }
        let Some(custody) = &self.custody else { return };
        // No unlock/free description, dead holder or failed append proves admitted work ended.
        if custody.pending.is_some()
            || custody.check(&self.lock, &self.path, &transaction).is_err()
            || custody.journal.lifetime() != Lifetime::Drained
        {
            return;
        }
        if let Some(name) = self.path.file_name() {
            let Some(name) = name.to_str() else { return };
            let Ok(text) = transaction.observe(&custody.directory, &custody.writer, name) else {
                return;
            };
            let Ok(capture) =
                transaction.capture(&custody.directory, &custody.writer, name, &text, true)
            else {
                return;
            };
            if transaction
                .commit(&custody.directory, &custody.writer, &capture)
                .is_err()
            {
                return;
            }
            let _ = transaction.archive(&custody.namespace, &self.lock);
        }
    }

    /// Cancel the ARM with the `NotSpawned` receipt `arm_cohort` kept as `unused` when its ARM
    /// event failed with a write pending, through the same `Journal::cancel` match as
    /// `cancel_cohort`; a journal already back at its description only drops the receipt.
    ///
    /// # Errors
    /// The claim has no cohort custody, the receipt belongs to another process or scope, it
    /// does not match the durable ARM, or the cancel event cannot be made durable.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CapacityCohort",
            action = "CancelMatchingNoChild",
            project = "aterm_capacity::ledger::failure_tests::project"
        )
    )]
    fn finish_unused(&mut self, transaction: &mut Transaction) -> io::Result<()> {
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no cohort custody"))?;
        let Some(proof) = &custody.unused else {
            return Ok(());
        };
        if !proof.key().is_current() {
            return Err(refused(
                "unused attempt belongs to another process or scope",
            ));
        }
        if !custody.journal.description() {
            let (line, next) = custody.journal.cancel(proof.key())?;
            self.event(transaction, line, next)?;
        }
        self.custody.as_mut().expect("cohort claim").unused = None;
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_EVENT_AFTER_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_EVENT_PARTIAL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{Claim, Fixed, Polled, Reading, Waiter};
    use aterm_uds::spawnfd::{self, Child, Stdio};
    use std::os::unix::fs::DirBuilderExt as _;

    pub(crate) struct Fixture {
        _scratch: checkpoint::TestWorld,
        pub(crate) namespace: Namespace,
        pub(crate) ledger: Ledger,
        machine: Fixed,
    }

    impl Fixture {
        pub(crate) fn new() -> Self {
            let scratch = checkpoint::TestWorld::new("cohort");
            let home = scratch.path().join("home");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&home)
                .unwrap();
            let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
            let ledger = Ledger::at(namespace.fence());
            Self {
                _scratch: scratch,
                namespace,
                ledger,
                machine: Fixed::new(Reading::of_gib(64, 40, 8)),
            }
        }

        pub(crate) fn claim(&self) -> Claim {
            match Waiter::enter(&self.ledger, &self.machine, Need::new("cohort").mem_gib(30))
                .unwrap()
                .poll(&self.machine)
            {
                Polled::Admitted(claim) => claim,
                other => panic!("claim was not admitted: {other:?}"),
            }
        }
    }

    #[test]
    fn timing_claim_roundtrips_v2_without_minting_authority_from_observations() {
        let f = Fixture::new();
        let machine = crate::tests::TimingMachine::new(&[(0, 0), (1, 99)]);
        let claim = crate::tests::timed_in(&f.ledger, &machine);
        let text = std::fs::read_to_string(claim.path()).unwrap();
        let (decoded, journal) = Journal::decode(&text, true, f.namespace.nonce()).unwrap();
        assert!(journal.description());
        assert_eq!(decoded.need.quiet_policy, crate::QuietPolicy::Timing);
        assert!(decoded.timing.admission.unwrap().ready());
        assert_eq!(
            f.ledger.list().unwrap().len(),
            1,
            "real cohort admission remains readable"
        );
        for value in [
            aterm_json::Value::Null,
            aterm_json::json!({"future":"policy"}),
        ] {
            let (head, tail) = text.split_once('\n').unwrap();
            let mut head: aterm_json::Value = aterm_json::from_str(head).unwrap();
            head.as_object_mut()
                .unwrap()
                .insert("quiet_policy".into(), value);
            let (decoded, journal) =
                Journal::decode(&format!("{head}\n{tail}"), true, f.namespace.nonce()).unwrap();
            assert!(journal.description());
            assert!(decoded.need.quiet.is_some() && decoded.held.is_some());
            assert_ne!(decoded.need.quiet_policy, crate::QuietPolicy::Timing);
            assert_eq!(decoded.need.quiet_policy, crate::QuietPolicy::Unknown);
            let admitted = decoded.held.unwrap().admitted_ms;
            assert_eq!(decoded.timing.admitted_ms, Some(admitted));
            assert!(decoded.reserves(admitted).is_some());
            let expires = admitted + millis(decoded.need.patience.unwrap().hold);
            assert_eq!(decoded.reserves(expires), None);
            let mut ordinary = decoded.clone();
            ordinary.need = Need::new("ordinary nonkin").cores(1);
            ordinary.pid = ordinary.pid.saturating_add(1);
            ordinary.start_ms = ordinary.start_ms.saturating_add(1);
            assert!(
                crate::admit::reserving(&ordinary, &[&decoded], &|_| false, admitted).is_some()
            );
        }
        let (decoded, journal) = Journal::decode(
            &format!("{text}{{\"timing\":{{\"invalid\":true}}}}\n"),
            true,
            f.namespace.nonce(),
        )
        .unwrap();
        assert!(journal.description());
        assert_eq!(decoded.timing.cause, crate::TimingCause::Unknown);
        assert!(
            decoded
                .reserves(aterm_time::unix_millis().unwrap())
                .is_some()
        );
        assert!(
            Journal::decode(&format!("{text}{{\"timing\":"), true, f.namespace.nonce()).is_err(),
            "torn v2 authority journal stays strict"
        );
    }

    struct Running {
        child: Child,
        input: Option<std::process::ChildStdin>,
    }
    impl Running {
        fn finish(&mut self) {
            self.input.take();
            assert!(self.child.wait().unwrap().success());
        }
    }
    impl Drop for Running {
        fn drop(&mut self) {
            self.input.take();
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }

    fn start(claim: &mut Claim) -> (Running, Cohort) {
        let attempt = claim.arm_cohort().unwrap();
        let mut spawned = spawnfd::command("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn_cohort(attempt)
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
        let cohort = claim.bind_cohort(&mut spawned).unwrap();
        (
            Running {
                child: spawned.into_child(),
                input: Some(input),
            },
            cohort,
        )
    }

    #[test]
    fn exact_no_child_receipt_cancels_arm_and_an_unrelated_receipt_does_not() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let attempt = claim.arm_cohort().unwrap();
        assert_eq!(f.ledger.list().unwrap()[0].accounting, Accounting::Whole);
        assert!(
            claim
                .cancel_cohort(BirthAttempt::new().unwrap().cancel().unwrap())
                .is_err()
        );
        let failure = spawnfd::command(f.namespace.backend().join("missing-target"))
            .spawn_cohort(attempt)
            .unwrap_err();
        claim
            .cancel_cohort(failure.into_not_spawned().unwrap())
            .unwrap();
        claim.finish_cohort().unwrap();
        drop(claim);
        assert!(!path.exists());
    }

    #[test]
    fn free_description_retains_bound_work_and_blocks_admission_until_actual_drain() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let (mut running, cohort) = start(&mut claim);
        assert!(claim.cohort_bound(&cohort));
        assert_eq!(
            claim.finish_cohort().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        claim.wrote(u64::MAX).unwrap();
        assert!(claim.credit(&[running.child.id()]).is_err());
        drop(claim.descriptor().unwrap());
        drop(claim);
        let listed = f.ledger.scan(true).unwrap();
        assert!(path.exists());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].accounting, Accounting::Whole);
        assert!(f.ledger.cohort_aware().unwrap());
        assert!(
            crate::admit_with(
                &f.ledger,
                &f.machine,
                Need::new("forced next").mem_gib(20),
                crate::Admit::Now,
                crate::Pace::default(),
                &mut |_| {}
            )
            .is_err(),
            "force admission must not bypass a retained cohort"
        );
        let waiter = Waiter::enter(&f.ledger, &f.machine, Need::new("next").mem_gib(20)).unwrap();
        let waiter = match waiter.poll(&f.machine) {
            Polled::Waiting(waiter, _) => waiter,
            other => panic!("live cohort did not retain its full budget: {other:?}"),
        };
        running.finish();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "actual cohort drain",
            || (cohort.lifetime() == Lifetime::Drained).then_some(()),
        );
        assert!(matches!(waiter.poll(&f.machine), Polled::Admitted(_)));
        assert!(!path.exists());
    }

    /// On a cohort-aware ledger `--admit now` only polls, and a poll that met another admission
    /// holding the ledger's lock is no refusal: it waits its turn, saying so, and is admitted
    /// once the lock is let go. NEGATIVE CONTROL: a forced request refused at the held lock ends
    /// before any wait line.
    #[test]
    fn a_forced_request_waits_its_turn_at_the_cohort_ledgers_lock() {
        let f = Fixture::new();
        assert!(f.ledger.cohort_aware().unwrap());
        let turn = f.ledger.try_turn().unwrap().expect("free");
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let ledger = f.ledger.clone();
        let forcing = std::thread::spawn(move || {
            crate::admit_with(
                &ledger,
                &Fixed::new(Reading::of_gib(64, 40, 8)),
                Need::new("forced").mem_gib(1),
                crate::Admit::Now,
                crate::Pace {
                    poll: std::time::Duration::from_millis(5),
                    line_every: std::time::Duration::ZERO,
                },
                &mut |line| {
                    let _ = tx.send(line.to_owned());
                },
            )
            .map(|claim| claim.record().held.is_some())
        });
        let line = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("its lock line");
        let held_turn = crate::Short::Turn { holder: None }.to_string();
        assert!(
            line.contains("forced") && line.contains(&held_turn),
            "{line:?}"
        );
        assert!(!forcing.is_finished(), "it waits while the lock is held");
        drop(turn);
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "its admission", || {
            forcing.is_finished().then_some(())
        });
        assert!(
            forcing
                .join()
                .unwrap()
                .expect("admitted once the lock is let go"),
            "held"
        );
    }

    #[test]
    fn adoption_preserves_binding_and_completion_requires_kernel_drain() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let (mut running, cohort) = start(&mut claim);
        let descriptor = claim.descriptor().unwrap();
        let writer = claim.cohort_writer_descriptor().unwrap();
        assert!(
            Claim::adopt(claim.descriptor().unwrap(), &path).is_err(),
            "a guard alone cannot create a mutating controller"
        );
        assert!(
            Claim::adopt_cohort(
                claim.descriptor().unwrap(),
                claim.descriptor().unwrap(),
                &path
            )
            .is_err(),
            "a second read-only alias is not writer authority"
        );
        claim.hand_off();
        let mut adopted = Claim::adopt_cohort(descriptor, writer, &path).unwrap();
        assert!(adopted.cohort_bound(&cohort));
        assert!(adopted.finish_cohort().is_err());
        running.finish();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "adopted cohort drain",
            || (cohort.lifetime() == Lifetime::Drained).then_some(()),
        );
        adopted.finish_cohort().unwrap();
        drop(adopted);
        assert!(!path.exists());
    }

    #[test]
    fn torn_custody_tail_never_reverts_to_free_lock_or_empty_ledger() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let attempt = claim.arm_cohort().unwrap();
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writer.write_all(b"{\"cohort\":").unwrap();
        drop(writer);
        assert!(f.ledger.list().is_err());
        assert!(claim.cancel_cohort(attempt.cancel().unwrap()).is_err());
        drop(claim);
        assert!(path.exists());
        assert!(f.ledger.scan(true).is_err());
        let waiter = Waiter::enter(&f.ledger, &f.machine, Need::new("next").mem_gib(20)).unwrap();
        assert!(matches!(waiter.poll(&f.machine), Polled::Waiting(_, _)));
    }

    #[test]
    fn a_failed_arm_keeps_its_actual_unused_receipt_and_can_settle() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        FAIL_EVENT_AFTER_WRITE.with(|failure| failure.set(true));
        assert!(claim.arm_cohort().is_err());
        assert!(
            f.ledger.list().is_err(),
            "an uncheckpointed tail blocks rather than decoding an older description"
        );
        claim.finish_cohort().unwrap();
        drop(claim);
        assert!(!path.exists());
    }

    #[test]
    fn a_failed_bind_retries_the_original_capture_and_only_settles_after_drain() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let attempt = claim.arm_cohort().unwrap();
        let mut spawned = spawnfd::command("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn_cohort(attempt)
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
        FAIL_EVENT_AFTER_WRITE.with(|failure| failure.set(true));
        assert!(claim.bind_cohort(&mut spawned).is_err());
        let original = claim.bind_cohort(&mut spawned).unwrap();
        assert_eq!(claim.bind_cohort(&mut spawned).unwrap(), original);
        let mut running = Running {
            child: spawned.into_child(),
            input: Some(input),
        };
        assert!(
            claim.finish_cohort().is_err(),
            "retry BIND cannot settle a live group"
        );
        assert!(claim.cohort_bound(&original));
        running.finish();
        claim.finish_cohort().unwrap();
    }

    #[test]
    fn bound_observation_refuses_changed_namespace_and_missing_budget_fields_do_not_mean_zero() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let (mut running, cohort) = start(&mut claim);
        let original = std::fs::read_to_string(claim.path()).unwrap();
        let (head, rest) = original.split_once('\n').unwrap();
        let mut head: Value = aterm_json::from_str(head).unwrap();
        head.as_object_mut().unwrap().remove("mem");
        let changed = format!("{}\n{rest}", aterm_json::to_string(&head).unwrap());
        checkpoint::past_the_clock_tick(
            &f.namespace.open_backend().unwrap(),
            &File::open(claim.path()).unwrap(),
        );
        std::fs::write(claim.path(), changed).unwrap();
        assert!(!claim.cohort_bound(&cohort));
        assert!(f.ledger.list().is_err());
        std::fs::write(claim.path(), original).unwrap();
        let header = f
            .namespace
            .fence()
            .parent()
            .unwrap()
            .join(".capacity-namespace.json");
        let original_header = std::fs::read(&header).unwrap();
        std::fs::write(&header, b"broken").unwrap();
        assert!(!claim.cohort_bound(&cohort));
        std::fs::write(&header, original_header).unwrap();
        running.finish();
        assert!(
            claim.finish_cohort().is_err(),
            "equal-byte restoration cannot regain the committed record origin"
        );
        let path = claim.path().to_owned();
        drop(claim);
        assert!(path.exists());
        assert!(f.ledger.scan(true).is_err());
    }

    #[test]
    fn transferred_controllers_cannot_arm_from_the_same_stale_description() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let mut alias = Claim::adopt_cohort(
            claim.descriptor().unwrap(),
            claim.cohort_writer_descriptor().unwrap(),
            &path,
        )
        .unwrap();
        let attempt = claim.arm_cohort().unwrap();
        assert!(alias.arm_cohort().is_err());
        drop(alias);
        assert!(path.exists(), "stale alias Drop cannot retire the new ARM");
        assert_eq!(f.ledger.list().unwrap()[0].accounting, Accounting::Whole);
        claim.cancel_cohort(attempt.cancel().unwrap()).unwrap();
        claim.finish_cohort().unwrap();
        drop(claim);
        assert!(!path.exists());
    }

    #[test]
    fn a_same_inode_head_and_prefix_restore_refuses_live_cold_and_adopted_authority() {
        use std::os::unix::fs::{FileExt as _, MetadataExt as _};
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let descriptor = claim.descriptor().unwrap();
        let writer = claim.cohort_writer_descriptor().unwrap();
        let (mut running, cohort) = start(&mut claim);
        let original = std::fs::read(&path).unwrap();
        let before = std::fs::metadata(&path).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        checkpoint::past_the_clock_tick(&f.namespace.open_backend().unwrap(), &file);
        file.write_all_at(b"!", 0).unwrap();
        file.write_all_at(&original, 0).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(before.ino(), std::fs::metadata(&path).unwrap().ino());
        assert!(!claim.cohort_bound(&cohort));
        assert!(claim.wrote(1).is_err());
        assert!(claim.finish_cohort().is_err());
        assert!(Claim::adopt_cohort(descriptor, writer, &path).is_err());
        assert!(f.ledger.list().is_err());
        running.finish();
        drop(claim);
        assert!(path.exists());
        assert!(f.ledger.scan(true).is_err());
    }

    #[test]
    fn restoring_a_live_bound_tail_to_an_older_description_never_releases_its_budget() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let old_description = std::fs::read(&path).unwrap();
        let (mut running, cohort) = start(&mut claim);
        std::fs::write(&path, old_description).unwrap();
        assert!(!claim.cohort_bound(&cohort));
        assert!(claim.arm_cohort().is_err());
        drop(claim);
        assert!(path.exists());
        assert!(f.ledger.scan(true).is_err());
        let waiter = Waiter::enter(
            &f.ledger,
            &f.machine,
            Need::new("blocked by restored claim").mem_gib(20),
        )
        .unwrap();
        assert!(matches!(waiter.poll(&f.machine), Polled::Waiting(_, _)));
        running.finish();
        assert!(
            f.ledger.scan(true).is_err(),
            "drain does not validate a restored generation"
        );
    }

    #[test]
    fn a_restore_after_the_covering_flush_cannot_release_an_unused_arm() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        checkpoint::restore_after_next_flush();
        assert!(claim.arm_cohort().is_err());
        assert!(
            claim.finish_cohort().is_err(),
            "the original unused receipt cannot bless a different physical generation"
        );
        assert!(f.ledger.list().is_err());
        drop(claim);
        assert!(path.exists());
        assert!(f.ledger.scan(true).is_err());
    }

    #[test]
    fn a_partial_write_before_origin_capture_retains_unknown_custody_without_rebasing() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        FAIL_EVENT_PARTIAL_WRITE.with(|failure| failure.set(true));
        assert!(claim.arm_cohort().is_err());
        assert!(
            claim.finish_cohort().is_err(),
            "retained desired bytes are not authority to capture a later partial generation"
        );
        assert!(f.ledger.list().is_err());
        drop(claim);
        assert!(path.exists());
        assert!(f.ledger.scan(true).is_err());
        let waiter = Waiter::enter(
            &f.ledger,
            &f.machine,
            Need::new("unknown partial write").mem_gib(20),
        )
        .unwrap();
        assert!(matches!(waiter.poll(&f.machine), Polled::Waiting(_, _)));
    }

    #[test]
    fn an_exact_tombstone_is_required_for_source_closure_and_absence_is_unknown() {
        let f = Fixture::new();
        let claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        let guard = reference.guard();
        assert_eq!(
            f.ledger.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Retained
        );
        let wrong_guard = ClaimGuardIdentity {
            inode: guard.inode + 1,
            ..guard
        };
        let wrong = crate::CohortClosureRef::captured(
            reference.namespace(),
            reference.marker(),
            reference.epoch(),
            wrong_guard,
        )
        .unwrap();
        assert_eq!(
            f.ledger.cohort_claim_state(&wrong).unwrap(),
            CohortClaimState::Unknown
        );
        drop(claim);
        let cold = Ledger::at(f.namespace.fence());
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Closed
        );
        assert_eq!(
            cold.cohort_claim_state(&wrong).unwrap(),
            CohortClaimState::Unknown
        );
        let wrong_marker = crate::CohortClosureRef::captured(
            reference.namespace(),
            "0000000000000-1-0",
            reference.epoch(),
            guard,
        )
        .unwrap();
        assert_eq!(
            cold.cohort_claim_state(&wrong_marker).unwrap(),
            CohortClaimState::Unknown
        );

        let mut unknown = f.claim();
        let reference = unknown.cohort_closure_ref().unwrap();
        let attempt = unknown.arm_cohort().unwrap();
        std::fs::remove_file(unknown.path()).unwrap();
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Unknown
        );
        assert!(cold.reap_cohort_claim(&reference).is_err());
        assert!(unknown.cancel_cohort(attempt.cancel().unwrap()).is_err());
        drop(unknown);
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Unknown
        );
    }

    #[test]
    fn cold_pending_arm_recovery_never_synthesizes_the_lost_no_child_receipt() {
        let f = Fixture::new();
        let (mut record, mut laid) = f
            .ledger
            .lay(&Need::new("cold ARM").mem_gib(30), 123)
            .unwrap();
        f.ledger
            .admit(
                &mut laid,
                &mut record,
                Held {
                    admitted_ms: 124,
                    cores: 1,
                    alone: false,
                },
            )
            .unwrap();
        let reference = laid.cohort_closure_ref().unwrap();
        let path = laid.path().to_owned();
        checkpoint::interrupt_next_publication();
        assert!(laid.arm_cohort().is_err());
        assert!(laid.custody.as_ref().unwrap().unused.is_some());
        // Model loss of the original controller's private memory, not a NoChild operation:
        // discard the real receipt and let every actual descriptor close on Drop.
        laid.custody.as_mut().unwrap().unused = None;
        drop(laid);
        let cold = Ledger::at(f.namespace.fence());
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Unknown
        );
        checkpoint::refuse_next_pending_file_flush();
        assert!(cold.reap_cohort_claim(&reference).is_err());
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Unknown
        );
        assert_eq!(
            cold.reap_cohort_claim(&reference).unwrap(),
            CohortClaimState::Retained
        );
        assert_eq!(cold.list().unwrap()[0].accounting, Accounting::Whole);
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Retained
        );
        assert!(path.exists());
    }

    #[test]
    fn cold_pending_bind_recovery_preserves_real_work_until_actual_kernel_drain() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        let guard = reference.guard();
        let path = claim.path().to_owned();
        let attempt = claim.arm_cohort().unwrap();
        let mut spawned = spawnfd::command("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn_cohort(attempt)
            .unwrap();
        let input = aterm_uds::deadline::cat_echoes(spawned.child_mut());
        checkpoint::interrupt_next_publication();
        assert!(claim.bind_cohort(&mut spawned).is_err());
        drop(claim);
        let mut running = Running {
            child: spawned.into_child(),
            input: Some(input),
        };
        let cold = Ledger::at(f.namespace.fence());
        assert_eq!(
            cold.reap_cohort_claim(
                &crate::CohortClosureRef::captured(
                    reference.namespace(),
                    reference.marker(),
                    reference.epoch(),
                    ClaimGuardIdentity {
                        inode: guard.inode + 1,
                        ..guard
                    }
                )
                .unwrap()
            )
            .unwrap(),
            CohortClaimState::Unknown
        );
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Unknown,
            "a different guard cannot publish this pending generation"
        );
        checkpoint::refuse_next_pending_file_flush();
        assert!(cold.reap_cohort_claim(&reference).is_err());
        assert_eq!(
            cold.reap_cohort_claim(&reference).unwrap(),
            CohortClaimState::Retained
        );
        assert_eq!(cold.list().unwrap()[0].accounting, Accounting::Whole);
        assert!(path.exists());
        running.finish();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "cold recovered actual cohort drain and tombstone",
            || {
                (cold.reap_cohort_claim(&reference).unwrap() == CohortClaimState::Closed)
                    .then_some(())
            },
        );
        assert!(!path.exists());
        assert_eq!(
            cold.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Closed
        );
    }

    #[test]
    fn cold_pending_retirement_requires_actual_drain_and_a_free_guard() {
        for live in [false, true] {
            let f = Fixture::new();
            let mut claim = f.claim();
            let mut running = live.then(|| start(&mut claim).0);
            let reference = claim.cohort_closure_ref().unwrap();
            let path = claim.path().to_owned();
            let file = File::from(claim.descriptor().unwrap());
            let directory = File::open(path.parent().unwrap()).unwrap();
            let name = path.file_name().unwrap().to_str().unwrap();
            let anchor = Anchor::open(&directory, name, f.namespace.nonce()).unwrap();
            let mut transaction = anchor.lock(&directory).unwrap();
            let text = snapshot(&file).unwrap();
            // A sealed intent is data, not fresh scope/guard proof. Inject the interrupted
            // retirement even while its original guard or actual child is still present.
            let capture = transaction
                .capture(&directory, &file, name, &text, true)
                .unwrap();
            checkpoint::interrupt_next_publication();
            assert!(transaction.commit(&directory, &file, &capture).is_err());
            drop(transaction);
            drop(file);
            let cold = Ledger::at(f.namespace.fence());
            assert_eq!(
                cold.reap_cohort_claim(&reference).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert_eq!(
                cold.cohort_claim_state(&reference).unwrap(),
                CohortClaimState::Unknown
            );
            assert!(path.exists());
            drop(claim);
            if let Some(running) = &mut running {
                assert_eq!(
                    cold.reap_cohort_claim(&reference).unwrap_err().kind(),
                    io::ErrorKind::WouldBlock,
                    "a free guard cannot retire a real live bound group"
                );
                assert!(path.exists());
                running.finish();
            }
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "pending retirement's actual drain and free guard",
                || match cold.reap_cohort_claim(&reference) {
                    Ok(CohortClaimState::Closed) => Some(()),
                    Ok(_) => None,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
                    Err(error) => panic!("pending retirement recovery failed: {error}"),
                },
            );
            assert!(!path.exists());
        }
    }

    #[test]
    fn a_missing_live_record_after_metadata_validation_is_not_an_empty_probe() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let path = claim.path().to_owned();
        let name = path.file_name().unwrap().to_str().unwrap();
        let attempt = claim.arm_cohort().unwrap();
        let directory = File::open(path.parent().unwrap()).unwrap();
        let metadata = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        checkpoint::validate_metadata(
            &f.namespace.open_active().unwrap(),
            metadata,
            f.namespace.nonce(),
        )
        .unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(probe(&f.namespace, &directory, name, true, true).is_err());
        assert!(f.ledger.scan(true).is_err());
        assert!(claim.cancel_cohort(attempt.cancel().unwrap()).is_err());
        drop(claim);
        assert!(
            f.namespace.active().join(metadata).is_dir(),
            "unknown custody remains discoverable"
        );
    }

    #[test]
    fn grant_next_and_measured_writes_all_require_the_committed_physical_generation() {
        use std::os::unix::fs::FileExt as _;
        for operation in 0..3 {
            let f = Fixture::new();
            let (mut record, mut laid) = f.ledger.lay(&Need::new("generation audit"), 123).unwrap();
            laid.append(&Record::next_line(100)).unwrap();
            assert_eq!(f.ledger.list().unwrap()[0].next_ms, Some(100));
            let grant = Held {
                admitted_ms: 101,
                cores: 1,
                alone: false,
            };
            let mut at = None;
            if operation == 2 {
                f.ledger.admit(&mut laid, &mut record, grant).unwrap();
                laid.wrote(&mut at, 1).unwrap();
                assert_eq!(f.ledger.list().unwrap()[0].wrote, Some(1));
            }
            let original = std::fs::read(laid.path()).unwrap();
            let writer = std::fs::OpenOptions::new()
                .write(true)
                .open(laid.path())
                .unwrap();
            checkpoint::past_the_clock_tick(&f.namespace.open_backend().unwrap(), &writer);
            writer.write_all_at(b"!", 0).unwrap();
            writer.write_all_at(&original, 0).unwrap();
            let result = match operation {
                0 => laid.append(&Record::next_line(102)),
                1 => f.ledger.admit(&mut laid, &mut record, grant),
                _ => laid.wrote(&mut at, 2),
            };
            assert!(
                result.is_err(),
                "mutation {operation} must not recapture a restored origin"
            );
            assert!(f.ledger.list().is_err());
            let path = laid.path().to_owned();
            drop(laid);
            assert!(path.exists());
        }
    }

    #[test]
    fn inherited_guard_cannot_write_or_truncate_the_authoritative_record() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let before = std::fs::read(claim.path()).unwrap();
        let mut guard = File::from(claim.descriptor().unwrap());
        assert!(guard.write_all(b"{\"mem\":0}").is_err());
        assert!(guard.set_len(0).is_err());
        assert_eq!(std::fs::read(claim.path()).unwrap(), before);
        let attempt = claim.arm_cohort().unwrap();
        claim.cancel_cohort(attempt.cancel().unwrap()).unwrap();
        claim.finish_cohort().unwrap();
    }

    #[test]
    fn terminal_transfer_recovers_each_covered_boundary_and_keeps_exact_closure() {
        for stage in [
            "terminal-covered",
            "archive-covered",
            "archive-moved",
            "compaction-covered",
            "compaction-unlinked",
        ] {
            let f = Fixture::new();
            let claim = f.claim();
            let reference = claim.cohort_closure_ref().unwrap();
            let path = claim.path().to_owned();
            checkpoint::interrupt_archive(stage);
            drop(claim);
            let cold = Ledger::at(f.namespace.fence());
            assert_eq!(
                cold.reap_cohort_claim(&reference).unwrap(),
                CohortClaimState::Closed
            );
            assert_eq!(
                cold.cohort_claim_state(&reference).unwrap(),
                CohortClaimState::Closed
            );
            assert!(!path.exists());
            let slot = f.namespace.retired().join(reference.key());
            assert!(!slot.join("claim.noindex").exists());
            assert!(slot.join("receipt.json").is_file());
            assert!(slot.join("compacted.json").is_file());
            assert!(cold.list().unwrap().is_empty());
        }
    }

    #[test]
    fn retired_evidence_is_outside_admission_and_missing_archive_is_unknown() {
        let f = Fixture::new();
        let mut references = Vec::new();
        for _ in 0..32 {
            let claim = f.claim();
            references.push(claim.cohort_closure_ref().unwrap());
            drop(claim);
        }
        assert_eq!(std::fs::read_dir(f.namespace.active()).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(f.namespace.retired()).unwrap().count(),
            references.len()
        );
        let first = &references[0];
        let slot = f.namespace.retired().join(first.key());
        let receipt = slot.join("receipt.json");
        let saved = std::fs::read(&receipt).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&receipt, b"invalid terminal record").unwrap();
        assert!(f.ledger.cohort_claim_state(first).is_err());
        let live = f.claim();
        assert_eq!(
            f.ledger.list().unwrap().len(),
            1,
            "admission never replays retired bodies"
        );
        std::fs::remove_file(&receipt).unwrap();
        assert_eq!(
            f.ledger.cohort_claim_state(first).unwrap(),
            CohortClaimState::Unknown
        );
        assert!(!saved.is_empty());
        drop(live);
    }

    #[test]
    fn closure_references_round_trip_only_observations_and_do_not_reuse_epochs() {
        let f = Fixture::new();
        let first = f.claim();
        let reference = first.cohort_closure_ref().unwrap();
        let encoded = reference.to_json();
        assert_eq!(
            crate::CohortClosureRef::from_json(&encoded).unwrap(),
            reference
        );
        let mut malformed = encoded.clone();
        malformed
            .as_object_mut()
            .unwrap()
            .insert("obsolete".into(), Value::Bool(true));
        assert!(crate::CohortClosureRef::from_json(&malformed).is_err());
        let other = crate::CohortClosureRef::captured(
            reference.namespace(),
            reference.marker(),
            "00000000000000000000000000000000",
            reference.guard(),
        )
        .unwrap();
        assert_ne!(other.epoch(), reference.epoch());
        assert_eq!(
            f.ledger.cohort_claim_state(&other).unwrap(),
            CohortClaimState::Unknown
        );
        drop(first);
        assert_eq!(
            f.ledger.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Closed
        );
        assert_eq!(
            f.ledger.cohort_claim_state(&other).unwrap(),
            CohortClaimState::Unknown
        );
    }

    #[test]
    fn unknown_claim_data_is_preserved_before_any_terminal_move() {
        let f = Fixture::new();
        let claim = f.claim();
        let path = claim.path().to_owned();
        let other = path.parent().unwrap().join("another-actors-image");
        std::fs::write(&other, b"the unknown object is never moved or removed").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o700)).unwrap();
        drop(claim);
        assert!(path.is_file());
        assert_eq!(
            std::fs::read(&other).unwrap(),
            b"the unknown object is never moved or removed"
        );
        assert!(f.ledger.scan(true).is_err());
        assert_eq!(std::fs::read_dir(f.namespace.retired()).unwrap().count(), 0);
    }

    #[test]
    fn archived_closure_covers_exact_receipt_files_without_refreshing_a_restored_chain() {
        for copied in [false, true] {
            let f = Fixture::new();
            let claim = f.claim();
            let reference = claim.cohort_closure_ref().unwrap();
            drop(claim);
            for name in ["receipt.json", "compacted.json"] {
                checkpoint::refuse_named_file_flush(name);
                assert!(f.ledger.cohort_claim_state(&reference).is_err());
                assert_eq!(
                    f.ledger.cohort_claim_state(&reference).unwrap(),
                    CohortClaimState::Closed
                );
            }
            let slot = f.namespace.retired().join(reference.key());
            let receipt = slot.join("receipt.json");
            let original = std::fs::read(&receipt).unwrap();
            if copied {
                let copy = slot.join("copy.json");
                std::fs::copy(&receipt, &copy).unwrap();
                std::fs::rename(&copy, &receipt).unwrap();
            } else {
                use std::os::unix::fs::{FileExt as _, PermissionsExt as _};
                checkpoint::past_the_clock_tick(
                    &f.namespace.open_backend().unwrap(),
                    &File::open(&receipt).unwrap(),
                );
                std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o600)).unwrap();
                let writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&receipt)
                    .unwrap();
                writer.write_all_at(b"!", 0).unwrap();
                writer.write_all_at(&original, 0).unwrap();
                std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o400)).unwrap();
            }
            assert_eq!(
                std::fs::read(&receipt).unwrap(),
                original,
                "CONTROL: the original archive bytes alone would accept the restore"
            );
            assert!(
                f.ledger.cohort_claim_state(&reference).is_err(),
                "covering a changed receipt cannot refresh the origin frozen by its compaction successor"
            );
            assert!(f.ledger.reap_cohort_claim(&reference).is_err());
        }
    }

    #[test]
    fn unpublished_slot_gate_faults_recover_without_replacing_the_original_data_file() {
        use std::os::unix::fs::MetadataExt as _;
        let stamp = |path: &Path| {
            let m = std::fs::symlink_metadata(path).unwrap();
            (
                m.dev(),
                m.ino(),
                m.len(),
                m.mode(),
                m.nlink(),
                m.uid(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        for stage in [
            "before-gate-create",
            "gate-created",
            "gate-short-write",
            "gate-complete",
            "gate-sealed",
            "gate-file-flush",
            "gate-covered",
            "slot-published",
            "slot-parent-covered",
        ] {
            let f = Fixture::new();
            let claim = f.claim();
            let reference = claim.cohort_closure_ref().unwrap();
            let path = claim.path().to_owned();
            if stage == "gate-file-flush" {
                checkpoint::refuse_named_file_flush(".slot.lock");
            } else {
                checkpoint::interrupt_slot_preparation(stage);
            }
            drop(claim);
            assert!(
                path.is_file(),
                "interrupted unpublished work cannot move the actual claim"
            );
            let slot = f.namespace.retired().join(reference.key());
            let preparation = f
                .namespace
                .retired()
                .join(format!(".preparing-{}", reference.key()));
            let gate = if preparation.exists() {
                preparation.join(".slot.lock")
            } else {
                slot.join(".slot.lock")
            };
            let before = gate.exists().then(|| stamp(&gate));
            if !matches!(stage, "slot-published" | "slot-parent-covered") {
                assert!(
                    !slot.exists(),
                    "partial or unacknowledged preparation is not a final slot"
                )
            }
            assert_eq!(
                f.ledger.reap_cohort_claim(&reference).unwrap(),
                CohortClaimState::Closed
            );
            assert!(!path.exists());
            assert!(!preparation.exists());
            let gate = slot.join(".slot.lock");
            let after = stamp(&gate);
            if let Some(before) = before {
                assert_eq!(
                    (before.0, before.1),
                    (after.0, after.1),
                    "even an owned partial gate is completed through its SAME original inode"
                );
                if before.3 & 0o7777 == 0o400 {
                    assert_eq!(
                        before, after,
                        "covered or sealed gate FILE origin survives the directory publication unchanged"
                    )
                }
            }
            assert_eq!(std::fs::read(&gate).unwrap(), b"cohort archive gate v1\n");
            assert_eq!(after.3 & 0o7777, 0o400);
            assert_eq!(
                f.ledger.cohort_claim_state(&reference).unwrap(),
                CohortClaimState::Closed
            );
        }
    }

    #[test]
    fn unpublished_gate_bytes_never_close_an_armed_claim_and_unknown_preparation_is_preserved() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        let attempt = claim.arm_cohort().unwrap();
        checkpoint::interrupt_slot_preparation("gate-short-write");
        assert!(checkpoint::with_archive_gate(&f.namespace, &reference, true, || ()).is_err());
        assert!(!f.namespace.retired().join(reference.key()).exists());
        assert_eq!(
            f.ledger.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Retained
        );
        checkpoint::with_archive_gate(&f.namespace, &reference, true, || ()).unwrap();
        assert_eq!(
            f.ledger.reap_cohort_claim(&reference).unwrap(),
            CohortClaimState::Retained,
            "a complete gate without terminal evidence is still no closure"
        );
        claim.cancel_cohort(attempt.cancel().unwrap()).unwrap();
        claim.finish_cohort().unwrap();
        drop(claim);
        assert_eq!(
            f.ledger.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Closed
        );

        let f = Fixture::new();
        let claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        let path = claim.path().to_owned();
        checkpoint::interrupt_slot_preparation("before-gate-create");
        drop(claim);
        let preparation = f
            .namespace
            .retired()
            .join(format!(".preparing-{}", reference.key()));
        let unknown = preparation.join("another-actors-image");
        std::fs::write(&unknown, b"unknown work stays exactly here").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&unknown, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(f.ledger.reap_cohort_claim(&reference).is_err());
        assert!(path.is_file());
        assert!(!f.namespace.retired().join(reference.key()).exists());
        assert_eq!(
            std::fs::read(&unknown).unwrap(),
            b"unknown work stays exactly here"
        );
    }

    #[test]
    fn an_existing_final_slot_never_repairs_a_missing_or_torn_gate() {
        use std::os::unix::fs::PermissionsExt as _;
        for missing in [false, true] {
            let f = Fixture::new();
            let claim = f.claim();
            let reference = claim.cohort_closure_ref().unwrap();
            drop(claim);
            let slot = f.namespace.retired().join(reference.key());
            let gate = slot.join(".slot.lock");
            if missing {
                std::fs::remove_file(&gate).unwrap()
            } else {
                std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::write(&gate, b"co").unwrap();
                std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o400)).unwrap();
            }
            assert!(!matches!(
                f.ledger.reap_cohort_claim(&reference),
                Ok(CohortClaimState::Closed)
            ));
            if missing {
                assert!(!gate.exists())
            } else {
                assert_eq!(std::fs::read(&gate).unwrap(), b"co")
            }
            assert!(
                !f.namespace
                    .retired()
                    .join(format!(".preparing-{}", reference.key()))
                    .exists()
            );
        }
    }

    #[test]
    fn unpublished_unknown_gate_data_is_not_a_recoverable_constant_prefix() {
        use std::os::unix::fs::PermissionsExt as _;
        for (body, mode) in [
            (b"another writer's work".as_slice(), 0o600),
            (b"co".as_slice(), 0o400),
            (b"cohort archive gate v1\n".as_slice(), 0o700),
        ] {
            let f = Fixture::new();
            let claim = f.claim();
            let reference = claim.cohort_closure_ref().unwrap();
            let path = claim.path().to_owned();
            checkpoint::interrupt_slot_preparation("before-gate-create");
            drop(claim);
            let preparation = f
                .namespace
                .retired()
                .join(format!(".preparing-{}", reference.key()));
            let gate = preparation.join(".slot.lock");
            std::fs::write(&gate, body).unwrap();
            std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(f.ledger.reap_cohort_claim(&reference).is_err());
            assert!(path.is_file());
            assert!(!f.namespace.retired().join(reference.key()).exists());
            assert_eq!(std::fs::read(&gate).unwrap(), body);
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                std::fs::symlink_metadata(&gate).unwrap().mode() & 0o7777,
                mode
            );
        }
    }

    #[test]
    fn detached_archive_slots_never_supply_closed_during_file_or_directory_cover() {
        for phase in ["file", "directory"] {
            for reap in [false, true] {
                let f = Fixture::new();
                let claim = f.claim();
                let reference = claim.cohort_closure_ref().unwrap();
                drop(claim);
                let detached =
                    checkpoint::replace_archive_after_cover(&f.namespace, &reference, phase)
                        .unwrap();
                let result = if reap {
                    f.ledger.reap_cohort_claim(&reference)
                } else {
                    f.ledger.cohort_claim_state(&reference)
                };
                assert!(
                    result.is_err(),
                    "the old detached directory cannot return Closed: {result:?}"
                );
                assert!(
                    detached.is_dir(),
                    "the actual original metadata directory remains preserved"
                );
                f.namespace.validate().unwrap();
                assert!(
                    f.ledger.cohort_claim_state(&reference).is_err(),
                    "CONTROL: copied current bytes still bind the different original slot/gate"
                );
            }
        }
    }

    #[test]
    fn transient_slot_entry_restore_cannot_refresh_the_frozen_directory_origin() {
        for phase in ["file", "directory"] {
            let f = Fixture::new();
            let claim = f.claim();
            let reference = claim.cohort_closure_ref().unwrap();
            drop(claim);
            let slot = f.namespace.retired().join(reference.key());
            let before = std::fs::read(slot.join("receipt.json")).unwrap();
            checkpoint::transient_archive_after_cover(&f.namespace, &reference, phase).unwrap();
            assert!(
                f.ledger.cohort_claim_state(&reference).is_err(),
                "unchanged file bytes and slot identity cannot refresh a changed directory origin"
            );
            assert_eq!(std::fs::read(slot.join("receipt.json")).unwrap(), before);
            assert_eq!(
                f.ledger.cohort_claim_state(&reference).unwrap(),
                CohortClaimState::Closed,
                "a later independent strict query observes its own unchanged valid metadata"
            );
        }
    }

    #[test]
    fn cold_queries_and_reapers_contend_on_the_same_fresh_archive_description() {
        let f = Fixture::new();
        let claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        drop(claim);
        let slot = f.namespace.retired().join(reference.key());
        let before = std::fs::read(slot.join("receipt.json")).unwrap();
        checkpoint::with_archive_gate(&f.namespace, &reference, false, || {
            assert_eq!(
                f.ledger.cohort_claim_state(&reference).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert_eq!(
                f.ledger.reap_cohort_claim(&reference).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            let namespace = f.namespace.clone();
            let reference = reference.clone();
            let concurrent = std::thread::spawn(move || {
                Ledger::at(namespace.fence())
                    .reap_cohort_claim(&reference)
                    .unwrap_err()
                    .kind()
            });
            assert_eq!(concurrent.join().unwrap(), io::ErrorKind::WouldBlock);
        })
        .unwrap();
        assert_eq!(std::fs::read(slot.join("receipt.json")).unwrap(), before);
        assert_eq!(
            f.ledger.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Closed
        );
    }

    #[test]
    fn a_prepared_slot_gate_serializes_owner_transfer_and_cold_recovery() {
        let f = Fixture::new();
        let claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        let path = claim.path().to_owned();
        checkpoint::with_archive_gate(&f.namespace, &reference, true, || {
            drop(claim);
            assert!(
                path.is_file(),
                "busy slot cannot move or prune the original claim"
            );
            assert_eq!(
                f.ledger.reap_cohort_claim(&reference).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(path.is_file());
        })
        .unwrap();
        assert_eq!(
            f.ledger.reap_cohort_claim(&reference).unwrap(),
            CohortClaimState::Closed
        );
        assert!(!path.exists());
    }

    #[test]
    fn a_wrong_original_guard_cannot_complete_a_prepared_epoch() {
        let f = Fixture::new();
        let mut claim = f.claim();
        let reference = claim.cohort_closure_ref().unwrap();
        let guard = reference.guard();
        let wrong = crate::CohortClosureRef::captured(
            reference.namespace(),
            reference.marker(),
            reference.epoch(),
            ClaimGuardIdentity {
                inode: guard.inode + 1,
                ..guard
            },
        )
        .unwrap();
        checkpoint::interrupt_epoch("base-covered");
        let mut interrupted = false;
        for bytes in 1..=64 {
            if claim.wrote(bytes).is_err() {
                interrupted = true;
                break;
            }
        }
        assert!(
            interrupted,
            "finite real writes must reach the bounded epoch transition"
        );
        let root = claim.path().parent().unwrap();
        let prepared = root.join("epoch-0000000000000002.preparing.noindex");
        assert!(prepared.is_dir());
        assert!(!root.join("epoch-0000000000000002.noindex").exists());
        assert_eq!(
            f.ledger.reap_cohort_claim(&wrong).unwrap(),
            CohortClaimState::Unknown
        );
        assert!(
            prepared.is_dir(),
            "wrong observations cannot publish a known intent"
        );
        assert!(!root.join("epoch-0000000000000002.noindex").exists());
        assert_eq!(
            f.ledger.reap_cohort_claim(&reference).unwrap(),
            CohortClaimState::Retained
        );
        assert!(!prepared.exists());
        assert_eq!(
            f.ledger.cohort_claim_state(&reference).unwrap(),
            CohortClaimState::Retained
        );
    }
}
