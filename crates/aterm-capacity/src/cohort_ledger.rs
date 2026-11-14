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
    subgroup_births: Vec<(BirthKey, Cohort)>,
    subgroup_unused: Vec<NotSpawned>,
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
            subgroup_births: Vec::new(),
            subgroup_unused: Vec::new(),
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
        let held = same_file(lock, &self.writer)?;
        let named = fd::fstatat(
            &self.directory,
            path.file_name()
                .ok_or_else(|| refused("claim has no name"))?,
            AtFlags::SYMLINK_NOFOLLOW,
        )?;
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
    let file = aterm_uds::kept_out(|| {
        fd::openat(directory, name, flags, FileMode::RUSR | FileMode::WUSR)
            .map(File::from)
            .map_err(io::Error::from)
    })?;
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

fn same_file(guard: &RecordLock, writer: &File) -> io::Result<fd::Stat> {
    let guard = fd::fstat(guard.as_fd())?;
    let writer = fd::fstat(writer)?;
    if (guard.st_dev, guard.st_ino) != (writer.st_dev, writer.st_ino) {
        return Err(refused(
            "private cohort writer opens another inode than its guard",
        ));
    }
    Ok(guard)
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
    let cap = usize::try_from(before.len().min(RECORD_MAX))
        .unwrap_or(RECORD_MAX as usize)
        .saturating_add(1);
    let mut bytes = vec![0u8; cap];
    let mut length = 0;
    while length < bytes.len() {
        let read = file.read_at(&mut bytes[length..], length as u64)?;
        if read == 0 {
            break;
        }
        length += read;
    }
    if length as u64 > RECORD_MAX
        || length as u64 != before.len()
        || stamp(&before) != stamp(&file.metadata()?)
    {
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
    let (head, journal) = if record.accounting == Accounting::CohortOwned {
        Journal::head_accounted(&record.head_line(), namespace.nonce(), record.accounting)?
    } else {
        Journal::head(&record.head_line(), namespace.nonce())?
    };
    let custody = Custody {
        namespace: namespace.clone(),
        directory,
        writer,
        journal,
        pending: None,
        unused: None,
        actual_birth: None,
        subgroup_births: Vec::new(),
        subgroup_unused: Vec::new(),
        head: head.clone(),
        checkpoint,
    };
    let mut laid = Laid {
        lock: lock.into(),
        path: root.join(&taking),
        granted: None,
        custody: Some(custody),
        detaching: None,
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
    scan_inventory(namespace, reap, named, None, None)
}

pub(super) fn funded_scopes(
    namespace: &Namespace,
    deadline: std::time::Instant,
) -> io::Result<Vec<crate::FundedScope>> {
    if std::time::Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "funded scope observation deadline expired",
        ));
    }
    let mut scopes = Vec::new();
    scan_inventory(
        namespace,
        false,
        &|_| true,
        Some(&mut scopes),
        Some(deadline),
    )?;
    Ok(scopes)
}

fn scan_inventory(
    namespace: &Namespace,
    reap: bool,
    named: &dyn Fn(&str) -> bool,
    mut scopes: Option<&mut Vec<crate::FundedScope>>,
    deadline: Option<std::time::Instant>,
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
        if deadline.is_some_and(|limit| std::time::Instant::now() >= limit) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "funded scope observation deadline expired",
            ));
        }
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
        let Some(ScannedClaim {
            working,
            record,
            scope,
        }) = scan_listed_full(namespace, &active, name, reap, scopes.is_some())?
        else {
            continue;
        };
        if let Some(record) = record
            && named(&working)
        {
            if let Some(scope) = scope
                && let Some(scopes) = scopes.as_mut()
            {
                scopes.push(scope);
            }
            records.push(record)
        }
    }
    namespace.validate()?;
    records.sort_by_key(Record::order);
    Ok(records)
}

/// [`scan_entry`] of the claim directory `name` a scan listed in `active`; `None` once it was
/// retired, by its owner's drop or another reaping scan, after that listing: no longer active,
/// it reserves nothing a scan reports.
///
/// # Errors
/// As [`scan_entry`], for a claim directory still in `active`.
#[cfg(test)]
fn scan_listed(
    namespace: &Namespace,
    active: &File,
    name: &str,
    reap: bool,
) -> io::Result<Option<(String, Option<Record>)>> {
    scan_listed_full(namespace, active, name, reap, false)
        .map(|entry| entry.map(|entry| (entry.working, entry.record)))
}

struct ScannedClaim {
    working: String,
    record: Option<Record>,
    scope: Option<crate::FundedScope>,
}

fn scan_listed_full(
    namespace: &Namespace,
    active: &File,
    name: &str,
    reap: bool,
    observe_scope: bool,
) -> io::Result<Option<ScannedClaim>> {
    match scan_entry_full(namespace, active, name, reap, observe_scope) {
        Ok(entry) => Ok(Some(entry)),
        Err(_) if left_active(active, name)? => Ok(None),
        Err(error) => Err(error),
    }
}

/// The active claim directory's original checkpointed working record.
#[cfg(test)]
fn scan_entry(
    namespace: &Namespace,
    active: &File,
    name: &str,
    reap: bool,
) -> io::Result<(String, Option<Record>)> {
    scan_entry_full(namespace, active, name, reap, false).map(|entry| (entry.working, entry.record))
}

fn scan_entry_full(
    namespace: &Namespace,
    active: &File,
    name: &str,
    reap: bool,
    observe_scope: bool,
) -> io::Result<ScannedClaim> {
    let anchor = Anchor::open_name(active, name, namespace.nonce())?;
    let directory = anchor.directory.try_clone()?;
    let mut transaction = if reap {
        anchor.lock_recover(&directory)?
    } else {
        anchor.lock(&directory)?
    };
    if reap {
        transaction.recover_pending(&directory, namespace.nonce())?;
    }
    let working = transaction.scan_name()?.to_owned();
    let observed = probe_under_full(
        namespace,
        &directory,
        &working,
        working.ends_with(CLAIM),
        reap,
        &mut transaction,
    )?;
    let mut scope = None;
    let record = if let Some((record, journal)) = observed {
        if observe_scope && record.held.is_some() {
            let file = open_mode(&directory, &working, false, false)?;
            let metadata = file.metadata()?;
            use std::os::unix::fs::MetadataExt as _;
            let reference = anchor.closure_ref(
                namespace.nonce(),
                ClaimGuardIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            )?;
            let current = transaction.observe(&directory, &file, &working)?;
            let (_, current) =
                Journal::decode(&current, working.ends_with(CLAIM), namespace.nonce())?;
            if current != journal {
                return Err(refused(
                    "funded scope changed during original checkpoint observation",
                ));
            }
            let mut observed = journal.funded_scope(reference);
            // Numeric external credits do not carry an original birth. The
            // current writer API refuses them, but retained generations must
            // still preserve that missing scope rather than infer absence.
            observed.unresolved |= !record.credit.is_empty();
            scope = Some(observed);
        }
        Some(record)
    } else {
        None
    };
    Ok(ScannedClaim {
        working,
        record,
        scope,
    })
}

/// Whether the claim directory `name` has left `active`. Retirement moves the whole directory
/// to `retired` in one rename, so a reader that listed it before then meets a missing name,
/// a moved parent or a replaced directory on its open, lock or load: such a failure is the
/// retirement, not a defect of the ledger.
///
/// # Errors
/// `active` cannot be read for `name`.
fn left_active(active: &File, name: &str) -> io::Result<bool> {
    match fd::fstatat(active, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(false),
        Err(error) if error.is_noent() => Ok(true),
        Err(error) => Err(error.into()),
    }
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
    probe_under_full(namespace, directory, name, admitted, reap, transaction)
        .map(|observed| observed.map(|(record, _)| record))
}

fn probe_under_full(
    namespace: &Namespace,
    directory: &File,
    name: &str,
    admitted: bool,
    reap: bool,
    transaction: &mut Transaction,
) -> io::Result<Option<(Record, Journal)>> {
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
                if record.accounting != Accounting::CohortOwned {
                    record.accounting = Accounting::Whole;
                }
                Ok(Some((record, journal)))
            }
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            Journal::decode(&before, admitted, namespace.nonce()).map(Some)
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
    if left_active(&active, &reference.key())? {
        return checkpoint::archived_state(namespace, reference, reap);
    }
    listed_claim_state(namespace, &active, reference, reap)
}

/// Whether the claim `reference` names is still in this namespace's active directory, read
/// without its record's mutation lock: a look that never makes its owner's drop meet a busy
/// record and leave retirement to a reaping scan.
///
/// # Errors
/// The active directory cannot be opened or read for the claim.
#[cfg(any(test, debug_assertions))]
pub(super) fn claim_listed(
    namespace: &Namespace,
    reference: &crate::CohortClosureRef,
) -> io::Result<bool> {
    if reference.namespace() != namespace.nonce() {
        return Ok(false);
    }
    Ok(!left_active(&namespace.open_active()?, &reference.key())?)
}

/// [`claim_state`] of a reference whose claim directory a look found in `active`: from the
/// archive once it was retired, by its owner's drop or a reaping scan, after that look.
///
/// # Errors
/// As [`active_claim_state`], for a claim directory still in `active`.
fn listed_claim_state(
    namespace: &Namespace,
    active: &File,
    reference: &crate::CohortClosureRef,
    reap: bool,
) -> io::Result<CohortClaimState> {
    match active_claim_state(namespace, active, reference, reap) {
        Err(_) if left_active(active, &reference.key())? => {
            checkpoint::archived_state(namespace, reference, reap)
        }
        state => state,
    }
}

/// [`claim_state`] of a reference whose claim directory was listed in `active`.
fn active_claim_state(
    namespace: &Namespace,
    active: &File,
    reference: &crate::CohortClosureRef,
    reap: bool,
) -> io::Result<CohortClaimState> {
    let anchor = Anchor::open_name(active, &reference.key(), namespace.nonce())?;
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
        if left_active(active, &reference.key())? {
            return checkpoint::archived_state(namespace, reference, reap);
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
    pub(crate) fn check_retained_description(&self) -> io::Result<()> {
        self.check_custody()?;
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("retained CPU binding requires original cohort custody"))?;
        if self.granted.is_none()
            || !custody.journal.description()
            || custody.actual_birth.is_some()
            || custody.unused.is_some()
        {
            return Err(refused(
                "retained CPU binding requires an unused admitted description",
            ));
        }
        Ok(())
    }

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
        // A covered subgroup snapshot can move the measured-write row. Find its current
        // position instead of trusting a byte offset captured before that compaction.
        let mut position = 0u64;
        let mut wrote = None;
        for row in expected.split_inclusive('\n') {
            if row.len() == WROTE_LINE
                && aterm_json::from_str::<aterm_json::Value>(row)
                    .ok()
                    .is_some_and(|value| {
                        value.as_object().is_some_and(|map| map.len() == 1)
                            && value
                                .get("wrote")
                                .and_then(aterm_json::Value::as_u64)
                                .is_some()
                    })
            {
                wrote = Some(position);
            }
            position += row.len() as u64;
        }
        let offset = wrote.unwrap_or(expected.len() as u64);
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
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no cohort transaction"))?;
        custody.namespace.validate()?;
        read_only(&self.lock)?;
        let held = same_file(&self.lock, &custody.writer)?;
        if !custody.checkpoint.matches_guard(ClaimGuardIdentity {
            device: held.st_dev,
            inode: held.st_ino,
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
        #[cfg(test)]
        if FAIL_EVENT_BEFORE_WRITE.with(|failure| failure.replace(false)) {
            return Err(io::Error::other(
                "injected custody failure before write or pending capture",
            ));
        }
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

    pub(crate) fn require_boot_closure(&mut self) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        if self.granted.is_none() {
            return Err(refused("boot closure requires an admitted claim"));
        }
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no boot closure policy"))?;
        let (line, next) = custody.journal.require_boot_closure()?;
        self.event(&mut transaction, line, next)
    }

    fn compact_subgroups(&mut self, transaction: &mut Transaction) -> io::Result<()> {
        self.check_locked(transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        let before = snapshot(&custody.writer)?;
        let after = custody.journal.compact(&before)?;
        if before != after {
            let next = custody.journal.clone();
            self.stage(transaction, after, next, None, None)?;
        }
        Ok(())
    }

    pub(crate) fn arm_subgroup(&mut self, root: &Cohort) -> io::Result<BirthAttempt> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.check_locked(&transaction)?;
        if self.granted.is_none()
            || !self
                .custody
                .as_ref()
                .is_some_and(|custody| custody.journal.root_bound(&root.record()))
        {
            return Err(refused(
                "subgroup requires this claim's original bound root",
            ));
        }
        self.compact_subgroups(&mut transaction)?;
        let custody = self.custody.as_mut().expect("cohort claim");
        custody
            .subgroup_births
            .retain(|(key, _)| custody.journal.has_subgroup(key));
        let attempt = BirthAttempt::new()?;
        let custody = self.custody.as_ref().expect("cohort claim");
        let (line, next) = custody
            .journal
            .arm_subgroup(&root.record(), attempt.key())?;
        if let Err(error) = self.event(&mut transaction, line, next) {
            if self
                .custody
                .as_ref()
                .is_some_and(|custody| custody.pending.is_some())
                && let Ok(proof) = attempt.cancel()
            {
                self.custody
                    .as_mut()
                    .expect("cohort claim")
                    .subgroup_unused
                    .push(proof);
            }
            return Err(error);
        }
        Ok(attempt)
    }

    pub(crate) fn bind_subgroup(&mut self, spawned: &mut Spawned) -> io::Result<Cohort> {
        if !spawned.key().is_current() || !spawned.is_original_child() {
            return Err(refused(
                "subgroup binding needs its original current actual child",
            ));
        }
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        if let Some((key, cohort)) = custody
            .subgroup_births
            .iter()
            .find(|(key, _)| key == spawned.key())
            && custody.journal.subgroup_bound(key, &cohort.record())
        {
            return Ok(cohort.clone());
        }
        let cohort = Cohort::of_spawned(spawned)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        let (line, next) = custody
            .journal
            .bind_subgroup(spawned.key(), &cohort.record())?;
        let births = &mut self.custody.as_mut().expect("cohort claim").subgroup_births;
        if let Some((_, actual)) = births.iter_mut().find(|(key, _)| key == spawned.key()) {
            *actual = cohort.clone();
        } else {
            births.push((spawned.key().clone(), cohort.clone()));
        }
        self.event(&mut transaction, line, next)?;
        Ok(cohort)
    }

    pub(crate) fn check_subgroup_cancel(&self, proof: &NotSpawned) -> io::Result<()> {
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim has no subgroups"))?;
        if custody
            .subgroup_unused
            .iter()
            .any(|unused| unused.key() == proof.key())
        {
            return Err(refused("subgroup no-child receipt is already retained"));
        }
        custody.journal.cancel_subgroup(proof.key())?;
        Ok(())
    }

    pub(crate) fn cancel_subgroup(&mut self, proof: NotSpawned) -> io::Result<()> {
        self.check_subgroup_cancel(&proof)?;
        self.custody
            .as_mut()
            .expect("cohort claim")
            .subgroup_unused
            .push(proof);
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.compact_subgroups(&mut transaction)
    }

    pub(crate) fn retire_subgroup(&mut self, key: &BirthKey, cohort: &Cohort) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.check_locked(&transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        let known_actual = custody
            .subgroup_births
            .iter()
            .any(|(actual, captured)| actual == key && captured == cohort);
        if !key.is_current()
            || (!known_actual && !custody.journal.subgroup_bound(key, &cohort.record()))
        {
            return Err(refused(
                "subgroup retirement needs its actual registered child receipt",
            ));
        }
        if !known_actual {
            self.custody
                .as_mut()
                .expect("cohort claim")
                .subgroup_births
                .push((key.clone(), cohort.clone()));
        }
        self.compact_subgroups(&mut transaction)?;
        let custody = self.custody.as_ref().expect("cohort claim");
        if custody.journal.has_subgroup(key) {
            let (line, next) = custody.journal.retire_subgroup(key, &cohort.record())?;
            self.event(&mut transaction, line, next)?;
        }
        self.compact_subgroups(&mut transaction)?;
        self.custody
            .as_mut()
            .expect("cohort claim")
            .subgroup_births
            .retain(|(actual, _)| actual != key);
        Ok(())
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

    pub(crate) fn check_cohort_cancel(&self, proof: &NotSpawned) -> io::Result<()> {
        let custody = self
            .custody
            .as_ref()
            .ok_or_else(|| refused("legacy claim cannot cancel a cohort"))?;
        if custody.unused.is_some() {
            return Err(refused("the original no-child receipt is already retained"));
        }
        custody.journal.cancel(proof.key()).map(|_| ())
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
        // Match actual receipt custody before transaction I/O, then keep it even
        // when transaction creation, validation or the durable event fails. The
        // existing finish path can retry this exact proof without inventing one.
        self.check_cohort_cancel(&proof)?;
        self.custody
            .as_mut()
            .expect("validated cohort claim")
            .unused = Some(proof);
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.check_locked(&transaction)
    }

    pub(crate) fn prepare_gnu_epoch(
        &mut self,
        epoch: &crate::retained_gnu::Epoch,
    ) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        let journal = &self
            .custody
            .as_ref()
            .ok_or_else(|| refused("GNU epoch needs original cohort custody"))?
            .journal;
        if journal.gnu_matches(epoch.id()) {
            return Ok(());
        }
        let (line, next) = journal.gnu_event("gnu_prepare", epoch.id())?;
        self.event(&mut transaction, line, next)
    }

    pub(crate) fn check_gnu_scope_drained(&mut self) -> io::Result<()> {
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.finish_unused(&mut transaction)?;
        self.check_locked(&transaction)?;
        if self
            .custody
            .as_ref()
            .ok_or_else(|| refused("GNU epoch needs original cohort custody"))?
            .journal
            .native_lifetime()
            != Lifetime::Drained
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "original native GNU cohort scope has not drained",
            ));
        }
        Ok(())
    }

    pub(crate) fn retire_gnu_epoch(
        &mut self,
        epoch: &crate::retained_gnu::Epoch,
    ) -> io::Result<()> {
        self.check_gnu_scope_drained()?;
        let mut transaction = self.transaction()?;
        self.retry_event(&mut transaction)?;
        self.check_locked(&transaction)?;
        let journal = &self
            .custody
            .as_ref()
            .ok_or_else(|| refused("GNU epoch needs original cohort custody"))?
            .journal;
        if journal.gnu_retired(epoch.retirement_id()?) {
            return Ok(());
        }
        let (line, next) = journal.gnu_event("gnu_retire", epoch.retirement_id()?)?;
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
        if let Some(custody) = &mut self.custody
            && custody.journal.description()
        {
            custody.subgroup_births.clear();
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

    pub(crate) fn original_root_bound(&self, cohort: &Cohort) -> bool {
        self.check_custody().is_ok()
            && self.granted.is_some()
            && self.custody.as_ref().is_some_and(|custody| {
                custody.pending.is_none() && custody.journal.root_bound(&cohort.record())
            })
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
        if self
            .custody
            .as_ref()
            .is_some_and(|c| c.unused.is_some() || !c.subgroup_unused.is_empty())
        {
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

    /// Cancel the ARM with its retained `NotSpawned` receipt, from explicit cancellation
    /// or an ARM event that failed with a write pending. Both use the same actual
    /// `Journal::cancel` match; an already committed description only drops the receipt.
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
        loop {
            let custody = self
                .custody
                .as_ref()
                .ok_or_else(|| refused("legacy claim has no subgroup custody"))?;
            let Some(proof) = custody.subgroup_unused.last() else {
                break;
            };
            if !proof.key().is_current() {
                return Err(refused("subgroup no-child owner changed"));
            }
            if custody.journal.has_subgroup(proof.key()) {
                let (line, next) = custody.journal.cancel_subgroup(proof.key())?;
                self.event(transaction, line, next)?;
            }
            self.custody
                .as_mut()
                .expect("cohort claim")
                .subgroup_unused
                .pop();
        }
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
    static FAIL_EVENT_BEFORE_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_EVENT_AFTER_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_EVENT_PARTIAL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "cohort_ledger_tests.rs"]
pub(super) mod tests;
