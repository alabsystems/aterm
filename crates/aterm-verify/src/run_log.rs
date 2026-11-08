// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run-log custody: creation and retention share a short directory lock; a writer holds its
//! sidecar until its file closes. Retention never waits for a writer. Uncoordinated old logs
//! supply no deletion authority and stay outside the managed quota.
//! Default logs live below the old flat directory, whose older pruners do not traverse folders.

use aterm_uds::file_lock::LockFile;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOGS_KEPT: usize = 20;

pub(super) struct Log {
    pub(super) path: PathBuf,
    file: File,
    // Fields close in declaration order: release custody only after the writer closes.
    events: Option<crate::event_log::Retained>,
    _custody: Arc<LockFile>,
}

impl io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.file, bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.file)
    }
}

/// Owner-private sample directory beside `path`, made lazily if a test binary hangs.
pub(super) fn sample_dir(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| OsString::from("verify.log"), OsStr::to_os_string);
    let mut sample_name = OsString::from(".");
    sample_name.push(name);
    sample_name.push(".hung-samples");
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(sample_name)
}

/// Remove only the sample namespace paired with a log, never following a symlink at its root.
fn remove_samples(path: &Path) {
    let samples = sample_dir(path);
    match std::fs::symlink_metadata(&samples) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            let _ = std::fs::remove_dir_all(samples);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(samples);
        }
        Err(_) => {}
    }
}

pub(super) fn open(
    chosen: Option<&Path>,
    cwd: &Path,
    dir: &Path,
    invocation: Option<&str>,
) -> Option<Log> {
    let path = chosen.map_or_else(
        || {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            dir.join("runs.noindex")
                .join(format!("{secs}-{}.log", std::process::id()))
        },
        |path| cwd.join(path),
    );
    match open_paired_at(&path, chosen.is_none(), Some(invocation)) {
        Ok(log) => Some(log),
        Err(error) => {
            eprintln!(
                "verify: cannot write the run log {}: {error}",
                path.display()
            );
            None
        }
    }
}

pub(super) fn events_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".events");
    PathBuf::from(name)
}

impl Log {
    pub(super) fn take_events(&mut self) -> Option<crate::event_log::Retained> {
        self.events.take()
    }
}

pub(crate) fn file_key(file: &File) -> io::Result<(String, String)> {
    let stat = aterm_dirfd::fstat(file)?;
    Ok((stat.st_dev.to_string(), stat.st_ino.to_string()))
}

fn first_line(file: &File) -> io::Result<String> {
    let mut reader = file.try_clone()?;
    reader.rewind()?;
    let mut bytes = Vec::new();
    reader.take(8192).read_to_end(&mut bytes)?;
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .ok_or_else(|| io::Error::other("run pairing record is incomplete"))?;
    String::from_utf8(bytes[..end].to_vec()).map_err(io::Error::other)
}

fn owned_events(human: &File, path: &Path, events: File) -> io::Result<File> {
    let line = first_line(human)?;
    let value: aterm_json::Value = aterm_json::from_str(
        line.strip_prefix("verify-pair: ")
            .ok_or_else(|| io::Error::other("existing companion has no owned pairing record"))?,
    )
    .map_err(io::Error::other)?;
    let object = value
        .as_object()
        .ok_or_else(|| io::Error::other("invalid pairing record"))?;
    if object.len() != 4
        || object
            .keys()
            .any(|key| !["schema", "id", "device", "inode"].contains(&key.as_str()))
    {
        return Err(io::Error::other("unknown pairing record"));
    }
    let id = value["id"]
        .as_str()
        .filter(|id| crate::event_log::valid_id(id))
        .ok_or_else(|| io::Error::other("pair identity is unknown"))?;
    let (device, inode) = file_key(&events)?;
    if value["schema"].as_u64() != Some(1)
        || value["device"].as_str() != Some(device.as_str())
        || value["inode"].as_str() != Some(inode.as_str())
        || !same_file(&events, path)
    {
        return Err(io::Error::other(
            "existing companion is not the recorded physical file",
        ));
    }
    let line = first_line(&events)?;
    let first: aterm_json::Value = aterm_json::from_str(
        line.strip_prefix("verify-event: ")
            .ok_or_else(|| io::Error::other("existing companion has no stamped first record"))?,
    )
    .map_err(io::Error::other)?;
    if first["schema"].as_u64() != Some(1)
        || first["event"].as_str() != Some("invocation-started")
        || crate::event_log::attribution(&first)
            .as_ref()
            .map(|a| a.id.as_str())
            != Some(id)
    {
        return Err(io::Error::other(
            "existing companion descriptor does not match its pairing",
        ));
    }
    Ok(events)
}

fn admit_events(human: &File, log: &Path) -> io::Result<File> {
    let path = events_path(log);
    let mut open = std::fs::OpenOptions::new();
    open.read(true).append(true).create_new(true);
    crate::event_log::private_flags(&mut open);
    match open.open(&path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let mut open = std::fs::OpenOptions::new();
            open.read(true).append(true);
            crate::event_log::private_flags(&mut open);
            let file = open.open(&path)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("companion is not regular"));
            }
            owned_events(human, &path, file)
        }
        Err(error) => Err(error),
    }
}

fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".writer.lock");
    PathBuf::from(name)
}

fn directory_lock(dir: &Path) -> io::Result<LockFile> {
    // Only short open/prune operations hold this lock, never a run. Logging is best effort:
    // bound admission rather than delay a check indefinitely behind another process.
    LockFile::take(
        &dir.join(".aterm-verify-logs.lock"),
        Instant::now() + Duration::from_secs(1),
        "run log directory is busy",
    )
}

#[cfg(test)]
fn open_at(path: &Path, generated: bool) -> io::Result<Log> {
    open_paired_at(path, generated, None)
}

fn open_paired_at(
    path: &Path,
    generated: bool,
    invocation: Option<Option<&str>>,
) -> io::Result<Log> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let directory = directory_lock(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .read(true)
        .create(!generated)
        .create_new(generated)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let mut file = options.open(path)?;
    // A failed generated collision must not give an existing file retention custody.
    // The directory interlock excludes openers and pruners until custody is held.
    let custody = LockFile::take(
        &sidecar(path),
        Instant::now(),
        "run log already has a writer",
    )?;
    if !same_file(&file, path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "run log is no stable regular file",
        ));
    }
    // Admit the paired file before either previous artifact can be truncated.
    let events = invocation.map(|_| admit_events(&file, path)).transpose()?;
    if !same_file(&file, path)
        || events
            .as_ref()
            .is_some_and(|f| !same_file(f, &events_path(path)))
    {
        return Err(io::Error::other("run log pair changed during admission"));
    }
    if let Some(events) = &events {
        events.set_len(0)?;
    }
    file.set_len(0)?;
    file.rewind()?;
    if let (Some(id), Some(events)) = (invocation, events.as_ref()) {
        let (device, inode) = file_key(events)?;
        writeln!(
            file,
            "verify-pair: {}",
            aterm_json::json!({"schema":1, "id":id, "device":device, "inode":inode})
        )?;
    }
    let custody = Arc::new(custody);
    let events = events.map(|file| crate::event_log::Retained {
        file,
        _custody: Arc::clone(&custody),
    });
    // The directory lock and the log's stable-file proof make this sample namespace belong to
    // the admitted new writer. A reused chosen path or a recycled generated name starts clean.
    remove_samples(path);
    let log = Log {
        path: path.to_path_buf(),
        file,
        events,
        _custody: custody,
    };
    if generated {
        // The new log already exists and holds custody before the quota is counted. Every
        // other opener/pruner uses the same directory interlock, closing the publication gap.
        prune(parent, &directory);
    }
    Ok(log)
}

fn is_run_log(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some((secs, pid)) = name.strip_suffix(".log").and_then(|s| s.split_once('-')) else {
        return false;
    };
    let (Ok(secs), Ok(pid)) = (secs.parse::<u64>(), pid.parse::<u32>()) else {
        return false;
    };
    pid != 0 && name == format!("{secs}-{pid}.log")
}

pub(crate) fn same_file(file: &File, path: &Path) -> bool {
    let Ok(held) = aterm_dirfd::fstat(file) else {
        return false;
    };
    let Ok((now, _)) = aterm_uds::regular_file::open(path) else {
        return false;
    };
    let Ok(now) = aterm_dirfd::fstat(&now) else {
        return false;
    };
    (held.st_dev, held.st_ino) == (now.st_dev, now.st_ino)
}

struct Candidate {
    events: Option<File>,
    path: PathBuf,
    file: File,
    modified: SystemTime,
    _custody: LockFile,
}

#[derive(Debug)]
enum CandidateError {
    Active,
    Unproven,
}

impl Candidate {
    fn take(path: PathBuf) -> Result<Self, CandidateError> {
        let custody = LockFile::take_existing(&sidecar(&path), Instant::now(), "active run log")
            .map_err(|error| {
                if error.kind() == io::ErrorKind::WouldBlock {
                    CandidateError::Active
                } else {
                    CandidateError::Unproven
                }
            })?;
        let (file, metadata) =
            aterm_uds::regular_file::open(&path).map_err(|_| CandidateError::Unproven)?;
        let events_at = events_path(&path);
        let events = match std::fs::symlink_metadata(&events_at) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Ok(metadata) if metadata.file_type().is_file() => {
                let (events, _) = aterm_uds::regular_file::open(&events_at)
                    .map_err(|_| CandidateError::Unproven)?;
                Some(
                    owned_events(&file, &events_at, events)
                        .map_err(|_| CandidateError::Unproven)?,
                )
            }
            _ => return Err(CandidateError::Unproven),
        };
        Ok(Self {
            events,
            path,
            file,
            modified: metadata.modified().map_err(|_| CandidateError::Unproven)?,
            _custody: custody,
        })
    }

    fn remove(&self) -> bool {
        if !same_file(&self.file, &self.path)
            || self
                .events
                .as_ref()
                .is_some_and(|file| !same_file(file, &events_path(&self.path)))
        {
            return false;
        }
        if std::fs::remove_file(&self.path).is_err() {
            return false;
        }
        if self.events.is_some() {
            let _ = std::fs::remove_file(events_path(&self.path));
        }
        remove_samples(&self.path);
        // The directory interlock still excludes an opener of this name, and custody is
        // retained until this candidate drops. Only an admitted generated log has this sidecar.
        let _ = std::fs::remove_file(sidecar(&self.path));
        true
    }
}

fn prune(dir: &Path, _directory: &LockFile) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut count = 0usize;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) || !is_run_log(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        if !std::fs::symlink_metadata(sidecar(&path)).is_ok_and(|m| m.is_file() && m.len() == 0) {
            continue;
        }
        match Candidate::take(path) {
            Ok(candidate) => {
                count += 1;
                candidates.push(candidate);
            }
            Err(CandidateError::Active) => count += 1,
            Err(CandidateError::Unproven) => {}
        }
    }
    candidates.sort_unstable_by(|a, b| {
        a.modified
            .cmp(&b.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut remaining = count.saturating_sub(LOGS_KEPT);
    for candidate in candidates {
        if remaining == 0 {
            break;
        }
        if candidate.remove() {
            remaining -= 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paired(path: &Path) -> (Log, crate::event_log::EventLog) {
        let mut events = crate::event_log::EventLog::unrecorded();
        let mut log = open_paired_at(path, false, Some(events.id())).unwrap();
        events.retain(log.take_events().unwrap());
        crate::event_log::emit(
            Some(&events),
            "verify-event: {\"schema\":1,\"event\":\"invocation-started\"}",
        );
        (log, events)
    }

    #[test]
    fn invalid_pairs_do_not_displace_the_managed_retention_quota() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut invalid = Vec::new();
        for at in 0..25 {
            let path = write_at(dir.path(), &format!("1-{}.log", at + 1), 1);
            std::fs::write(events_path(&path), b"foreign\n").unwrap();
            invalid.push(path);
        }
        let mut valid = Vec::new();
        for at in 0..LOGS_KEPT {
            let name = format!("{}-{}.log", 100 + at, 1000 + at);
            let path = dir.path().join(&name);
            if at < 5 {
                let (log, events) = paired(&path);
                log.file
                    .set_modified(UNIX_EPOCH + Duration::from_secs((100 + at) as u64))
                    .unwrap();
                drop((log, events));
            } else {
                write_at(dir.path(), &name, (100 + at) as u64);
            }
            valid.push(path);
        }
        let active = dir.path().join("999-999.log");
        let mut events = crate::event_log::EventLog::unrecorded();
        let mut log = open_paired_at(&active, true, Some(events.id())).unwrap();
        events.retain(log.take_events().unwrap());
        crate::event_log::emit(
            Some(&events),
            "verify-event: {\"schema\":1,\"event\":\"invocation-started\"}",
        );
        assert_eq!(
            valid.iter().filter(|path| path.exists()).count(),
            LOGS_KEPT - 1
        );
        assert!(
            !valid[0].exists(),
            "oldest managed pair alone leaves room for the active writer"
        );
        assert!(!events_path(&valid[0]).exists());
        for path in invalid {
            assert!(path.exists());
            assert_eq!(std::fs::read(events_path(&path)).unwrap(), b"foreign\n");
        }
        assert!(active.exists());
    }

    #[test]
    fn a_retained_writer_keeps_custody_after_the_human_log_drops_and_through_unwind() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1-10.log");
        let (log, events) = paired(&path);
        drop(log);
        assert!(Candidate::take(path.clone()).is_err());
        assert!(open_paired_at(&path, false, Some(events.id())).is_err());
        events.write("verify-event: {\"schema\":1,\"event\":\"still-owned\"}");
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _writer = events;
            panic!("retire the actual retained writer on unwind");
        }));
        assert!(unwound.is_err());
        let candidate =
            Candidate::take(path.clone()).expect("both writer files closed before custody ended");
        assert!(candidate.remove());
        assert!(!path.exists());
        assert!(!events_path(&path).exists());
    }

    #[test]
    fn only_the_physically_bound_nonempty_companion_can_be_reused() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.log");
        let (mut log, events) = paired(&path);
        log.write_all(b"old human record\n").unwrap();
        let old_id = events.id().unwrap().to_owned();
        drop((log, events));
        let (log, events) = paired(&path);
        assert_ne!(events.id(), Some(old_id.as_str()));
        let retained = std::fs::read_to_string(events_path(&path)).unwrap();
        assert!(retained.contains(events.id().unwrap()));
        assert!(!retained.contains(&old_id));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("old human record")
        );
        drop((log, events));

        // Same recorded inode, but a different actual stamped descriptor: no reuse/pruning.
        let foreign = crate::event_log::EventLog::unrecorded();
        std::fs::write(
            events_path(&path),
            foreign.prepare("verify-event: {\"schema\":1,\"event\":\"invocation-started\"}"),
        )
        .unwrap();
        let human_before = std::fs::read(&path).unwrap();
        let events_before = std::fs::read(events_path(&path)).unwrap();
        assert!(open_paired_at(&path, false, Some(foreign.id())).is_err());
        assert!(Candidate::take(path.clone()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), human_before);
        assert_eq!(std::fs::read(events_path(&path)).unwrap(), events_before);
    }

    #[test]
    fn foreign_and_empty_companions_refuse_before_either_artifact_is_truncated() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.log");
        std::fs::write(&path, b"unproven human bytes\n").unwrap();
        std::fs::write(events_path(&path), b"foreign bytes\n").unwrap();
        assert!(
            open_paired_at(&path, false, Some(Some("0123456789abcdef0123456789abcdef"))).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"unproven human bytes\n");
        assert_eq!(
            std::fs::read(events_path(&path)).unwrap(),
            b"foreign bytes\n"
        );
        std::fs::remove_file(events_path(&path)).unwrap();
        let events = crate::event_log::EventLog::unrecorded();
        drop(open_paired_at(&path, false, Some(events.id())).unwrap());
        let before = std::fs::read(&path).unwrap();
        assert!(
            open_paired_at(&path, false, Some(events.id())).is_err(),
            "an old empty file is not the current admitted creation"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(events_path(&path)).unwrap(), b"");
    }

    #[cfg(unix)]
    #[test]
    fn a_companion_symlink_is_never_followed_or_removed() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.log");
        let outside = dir.path().join("outside");
        std::fs::write(&path, b"human\n").unwrap();
        std::fs::write(&outside, b"keep\n").unwrap();
        std::os::unix::fs::symlink(&outside, events_path(&path)).unwrap();
        assert!(
            open_paired_at(&path, false, Some(Some("0123456789abcdef0123456789abcdef"))).is_err()
        );
        assert!(
            std::fs::symlink_metadata(events_path(&path))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"human\n");
    }

    /// Reusing an explicit log starts a new diagnostic namespace. A stale real sample directory
    /// is removed, while a symlink at that derived path is unlinked without touching its target.
    #[cfg(unix)]
    #[test]
    fn a_reused_explicit_log_clears_only_its_old_sample_path() {
        use std::os::unix::fs::symlink;

        let root = aterm_tempfile::tempdir().unwrap();
        let log = root.path().join("chosen.log");
        std::fs::write(&log, b"old run\n").unwrap();
        let samples = sample_dir(&log);
        std::fs::create_dir(&samples).unwrap();
        std::fs::write(samples.join("old.sample.txt"), b"old stack\n").unwrap();

        drop(open_at(&log, false).expect("reopen the explicit log"));
        assert_eq!(std::fs::read(&log).unwrap(), b"");
        assert!(!samples.exists(), "the old run's samples survived");

        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let sentinel = outside.join("belongs-to-someone-else");
        std::fs::write(&sentinel, b"keep").unwrap();
        symlink(&outside, &samples).unwrap();
        drop(open_at(&log, false).expect("reopen beside a sample symlink"));
        assert!(std::fs::symlink_metadata(&samples).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
    }

    #[test]
    fn default_logs_survive_an_older_flat_directory_pruner() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let flat = dir.path().join("logs.noindex");
        std::fs::create_dir(&flat).unwrap();
        for i in 0..LOGS_KEPT + 3 {
            std::fs::write(flat.join(format!("1700000000-{}.log", i + 1)), b"old log").unwrap();
        }
        let mut live = open(None, dir.path(), &flat, None).unwrap();
        live.write_all(b"current run\n").unwrap();
        assert_ne!(live.path.parent(), Some(flat.as_path()));
        let mut removed = 0;
        // Stronger than the old bounded prune: remove every flat regular file. That old
        // algorithm never traverses a folder, so neither the new data nor its locks is seen.
        for entry in std::fs::read_dir(&flat).unwrap().map(Result::unwrap) {
            if entry.file_type().unwrap().is_file() {
                std::fs::remove_file(entry.path()).unwrap();
                removed += 1;
            }
        }
        assert_eq!(removed, LOGS_KEPT + 3, "the old cleanup actually ran");
        let before = std::fs::read(&live.path).unwrap();
        assert!(before.ends_with(b"current run\n"));
        live.write_all(b"still writing\n").unwrap();
        assert_eq!(
            std::fs::read(&live.path).unwrap(),
            [before.as_slice(), b"still writing\n"].concat()
        );
    }

    fn write_at(dir: &Path, name: &str, seconds: u64) -> PathBuf {
        let path = dir.join(name);
        let mut log = open_at(&path, false).unwrap();
        log.file.write_all(name.as_bytes()).unwrap();
        log.file
            .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
        path
    }

    fn pressure(dir: &Path, first_pid: usize) {
        for i in 0..LOGS_KEPT + 4 {
            write_at(
                dir,
                &format!("1700000001-{}.log", first_pid + i),
                100 + i as u64,
            );
        }
    }

    #[test]
    fn active_writer_survives_pressure_and_its_closed_log_becomes_eligible() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1700000000-1.log");
        let mut active = open_at(&path, true).unwrap();
        active.file.write_all(b"still writing\n").unwrap();
        active.file.set_modified(UNIX_EPOCH).unwrap();
        pressure(dir.path(), 2);
        let directory = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &directory);
        assert_eq!(std::fs::read(&path).unwrap(), b"still writing\n");
        active.file.write_all(b"finished\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"still writing\nfinished\n");
        active.file.set_modified(UNIX_EPOCH).unwrap();
        drop(active);
        pressure_cannot_open_while_locked(dir.path());
        drop(directory);
        pressure(dir.path(), 100);
        let directory = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &directory);
        assert!(!path.exists(), "the ended writer's oldest log is eligible");
        assert!(
            !sidecar(&path).exists(),
            "its admitted sidecar retires with it"
        );
    }

    fn pressure_cannot_open_while_locked(dir: &Path) {
        // A contender's nonblocking observation proves the directory interlock is held;
        // this is not a timing test or an attempt to open through a known held interlock.
        assert_eq!(
            LockFile::take_existing(&dir.join(".aterm-verify-logs.lock"), Instant::now(), "busy")
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn retention_keeps_foreign_and_uncoordinated_files_and_removes_old_managed_logs() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let foreign = [
            "notes.txt",
            "chosen.log",
            "01-2.log",
            "1-02.log",
            "1-0.log",
            "1-2-3.log",
            "+1-2.log",
            "1-+2.log",
            "1-.log",
            "18446744073709551616-1.log",
            "1-4294967296.log",
        ];
        for name in foreign {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let legacy = dir.path().join("1-1.log");
        std::fs::write(&legacy, b"no custody record").unwrap();
        let unknown = dir.path().join("2-2.log");
        std::fs::write(&unknown, b"unknown custody").unwrap();
        std::fs::write(sidecar(&unknown), b"keep this record").unwrap();
        let logs: Vec<_> = (0..LOGS_KEPT + 4)
            .map(|i| {
                let path = write_at(
                    dir.path(),
                    &format!("1700000000-{}.log", i + 2),
                    100 + i as u64,
                );
                let samples = sample_dir(&path);
                std::fs::create_dir(&samples).unwrap();
                std::fs::write(samples.join("stack.txt"), b"owned sample").unwrap();
                path
            })
            .collect();
        let current = open_at(&dir.path().join("1700000001-123.log"), true).unwrap();
        for name in foreign {
            assert_eq!(
                std::fs::read_to_string(dir.path().join(name)).unwrap(),
                name
            );
        }
        assert_eq!(std::fs::read(&legacy).unwrap(), b"no custody record");
        assert_eq!(std::fs::read(&unknown).unwrap(), b"unknown custody");
        assert_eq!(
            std::fs::read(sidecar(&unknown)).unwrap(),
            b"keep this record"
        );
        for (i, path) in logs.iter().enumerate() {
            assert_eq!(path.exists(), i >= 5, "{}", path.display());
            assert_eq!(
                sample_dir(path).exists(),
                i >= 5,
                "the sample namespace follows only its retired log: {}",
                path.display()
            );
        }
        assert_eq!(
            logs.iter().filter(|p| p.exists()).count() + usize::from(current.path.exists()),
            LOGS_KEPT
        );
    }

    #[test]
    fn a_second_writer_and_a_generated_name_collision_never_truncate_a_log() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1700000000-1.log");
        let mut first = open_at(&path, true).unwrap();
        first.file.write_all(b"retained").unwrap();
        assert_eq!(
            open_at(&path, false).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"retained");
        drop(first);
        assert_eq!(
            open_at(&path, true).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"retained");
        let chosen = open_at(&path, false).unwrap();
        assert!(
            std::fs::read(&path).unwrap().is_empty(),
            "an admitted chosen path retains truncation"
        );
        drop(chosen);
    }

    #[test]
    fn a_generated_collision_never_admits_an_uncoordinated_log_for_retention() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1700000000-1.log");
        let mut foreign = File::create(&path).unwrap();
        foreign.write_all(b"uncoordinated bytes").unwrap();
        foreign.set_modified(UNIX_EPOCH).unwrap();
        drop(foreign);
        assert_eq!(
            open_at(&path, true).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        pressure(dir.path(), 100);
        let directory = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &directory);
        assert_eq!(std::fs::read(&path).unwrap(), b"uncoordinated bytes");
        assert!(
            !dir.path().join("1700000001-100.log").exists(),
            "retention still removes admitted old logs under the same pressure"
        );
    }

    #[test]
    fn quota_never_supplies_authority_to_remove_active_writers() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut active: Vec<_> = (0..LOGS_KEPT + 3)
            .map(|i| open_at(&dir.path().join(format!("1700000000-{}.log", i + 1)), true).unwrap())
            .collect();
        assert!(active.iter().all(|log| log.path.exists()));
        let closed = active.remove(0).path.clone();
        let _next = open_at(&dir.path().join("1700000001-999.log"), true).unwrap();
        assert!(
            !closed.exists(),
            "a closed log can retire under the same pressure"
        );
        assert!(
            active.iter().all(|log| log.path.exists()),
            "every actual writer survives above the target"
        );
    }

    #[test]
    fn a_replaced_path_never_borrows_the_opened_candidates_deletion_authority() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = write_at(dir.path(), "1700000000-1.log", 1);
        let samples = sample_dir(&path);
        std::fs::create_dir(&samples).unwrap();
        let sample = samples.join("belongs-to-opened-log");
        std::fs::write(&sample, b"retained sample").unwrap();
        let _directory = directory_lock(dir.path()).unwrap();
        let candidate = Candidate::take(path.clone()).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"replacement bytes").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(!candidate.remove());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement bytes");
        assert!(sidecar(&path).exists());
        assert_eq!(std::fs::read(&sample).unwrap(), b"retained sample");
    }

    #[cfg(unix)]
    const CHILD_LOG: &str = "ATERM_VERIFY_RUN_LOG_CHILD";

    #[cfg(unix)]
    #[test]
    fn standin_writer_if_asked() {
        use std::io::Read as _;
        let Some(path) = std::env::var_os(CHILD_LOG) else {
            return;
        };
        let mut log = open_at(Path::new(&path), true).unwrap();
        log.file.write_all(b"child writing\n").unwrap();
        log.file.set_modified(UNIX_EPOCH).unwrap();
        println!("RUN_LOG_WRITER_READY");
        std::io::stdout().flush().unwrap();
        std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
        log.file.write_all(b"child finished\n").unwrap();
        log.file.set_modified(UNIX_EPOCH).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_separate_process_holds_custody_until_its_writer_ends() {
        use aterm_spec::wait::{MUST_HAPPEN, until_within};
        use aterm_uds::spawnfd::{Child, Stdio};
        struct Writer(Child);
        impl Drop for Writer {
            fn drop(&mut self) {
                drop(self.0.stdin.take());
                if self.0.try_wait().ok().flatten().is_none() {
                    aterm_uds::deadline::kill_within(&mut self.0, MUST_HAPPEN);
                }
            }
        }
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1700000000-1.log");
        let mut writer = Writer(
            aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "run_log::tests::standin_writer_if_asked",
                    "--nocapture",
                ])
                .env(CHILD_LOG, &path)
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        aterm_uds::deadline::prints(&mut writer.0, "RUN_LOG_WRITER_READY", MUST_HAPPEN);
        pressure(dir.path(), 2);
        let directory = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &directory);
        assert_eq!(std::fs::read(&path).unwrap(), b"child writing\n");
        drop(directory);
        drop(writer.0.stdin.take());
        let ended = until_within(MUST_HAPPEN, "the owned writer ends", || {
            writer.0.try_wait().unwrap()
        });
        assert!(ended.success(), "{ended}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"child writing\nchild finished\n"
        );
        pressure(dir.path(), 100);
        let directory = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &directory);
        assert!(!path.exists(), "closed child log is now eligible");
    }
}
