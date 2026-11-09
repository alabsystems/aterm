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

/// Immutable file-role header, not a run identity or deletion capability.
pub(crate) const PRIVATE_EVENTS_HEADER: &str = "verify-event: {\"schema\":1,\"event\":\"private-run-events\",\"storage\":\"aterm-verify-private-v1\"}\n";
pub(crate) const PRIVATE_HUMAN_HEADER: &str = "verify-event: {\"schema\":1,\"event\":\"private-run-log\",\"storage\":\"aterm-verify-private-human-v1\"}\n";

/// Inspect the actual descriptor, including hard-link aliases, within a fixed prefix bound.
pub(crate) fn private_managed_file(file: &File) -> io::Result<bool> {
    Ok(has_header(file, PRIVATE_EVENTS_HEADER)? || has_header(file, PRIVATE_HUMAN_HEADER)?)
}

fn has_header(file: &File, expected: &str) -> io::Result<bool> {
    // Callers either own an append description or have not exposed the human writer yet.
    // Human admission explicitly seeks to the end after reset, before writing the pairing.
    let mut reader = file.try_clone()?;
    reader.rewind()?;
    let mut header = vec![0; expected.len()];
    let mut count = 0;
    while count < header.len() {
        let read = reader.read(&mut header[count..])?;
        if read == 0 {
            break;
        }
        count += read;
    }
    Ok(&header[..count] == expected.as_bytes())
}

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
    match open_paired_at(&path, chosen.is_none(), invocation) {
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

fn first_line_at(file: &File, offset: u64) -> io::Result<String> {
    let mut reader = file.try_clone()?;
    reader.seek(io::SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    reader.take(8192).read_to_end(&mut bytes)?;
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .ok_or_else(|| io::Error::other("run pairing record is incomplete"))?;
    String::from_utf8(bytes[..end].to_vec()).map_err(io::Error::other)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRecord {
    schema: u64,
    id: String,
    device: String,
    inode: String,
}

fn owned_events(human: &File, path: &Path, events: File) -> io::Result<File> {
    if !has_header(&events, PRIVATE_EVENTS_HEADER)? || !has_header(human, PRIVATE_HUMAN_HEADER)? {
        return Err(io::Error::other("existing pair has no private file roles"));
    }
    let line = first_line_at(human, PRIVATE_HUMAN_HEADER.len() as u64)?;
    let pair: PairRecord = aterm_json::from_str(
        line.strip_prefix("verify-pair: ")
            .ok_or_else(|| io::Error::other("existing companion has no owned pairing record"))?,
    )
    .map_err(io::Error::other)?;
    if !crate::event_log::valid_id(&pair.id) {
        return Err(io::Error::other("pair identity is unknown"));
    }
    let (device, inode) = file_key(&events)?;
    if pair.schema != 1 || pair.device != device || pair.inode != inode || !same_file(&events, path)
    {
        return Err(io::Error::other(
            "existing companion is not the recorded physical file",
        ));
    }
    let line = first_line_at(&events, PRIVATE_EVENTS_HEADER.len() as u64)?;
    let first = crate::event_log::raw_event(
        line.strip_prefix("verify-event: ")
            .ok_or_else(|| io::Error::other("existing companion has no stamped first record"))?,
    )
    .ok_or_else(|| io::Error::other("existing companion has an ambiguous event header"))?;
    if first.value["event"].as_str() != Some("invocation-started")
        || first.attribution.as_ref().map(|a| a.id.as_str()) != Some(pair.id.as_str())
    {
        return Err(io::Error::other(
            "existing companion descriptor does not match its pairing",
        ));
    }
    Ok(events)
}

fn admit_events(human: &File, log: &Path, new_human: bool) -> io::Result<File> {
    let path = events_path(log);
    if new_human {
        return publish_marked(&path, Role::Events);
    }
    let file = Role::Events.open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("companion is not regular"));
    }
    owned_events(human, &path, file)
}

/// Only this created staging inode is eligible for cleanup; the destination is never removed.
struct StagedLogFile {
    file: File,
    path: PathBuf,
}

impl Drop for StagedLogFile {
    fn drop(&mut self) {
        if same_file(&self.file, &self.path) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy)]
enum Role {
    Human,
    Events,
}

impl Role {
    fn header(self) -> &'static str {
        match self {
            Self::Human => PRIVATE_HUMAN_HEADER,
            Self::Events => PRIVATE_EVENTS_HEADER,
        }
    }
    fn open(self, path: &Path) -> io::Result<File> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        match self {
            Self::Human => {
                options.write(true);
            }
            Self::Events => {
                options.append(true);
            }
        }
        crate::event_log::private_flags(&mut options);
        options.open(path)
    }
}

fn publish_marked(path: &Path, role: Role) -> io::Result<File> {
    // Atomic-file staging is private implementation work, not a supported public destination.
    // This does not defend against deliberate same-account interference with a staging inode,
    // nor promise power-loss durability. The role is complete before destination publication.
    let (temporary, file) = aterm_uds::atomic_file::create_temp_beside(path, 0o600)?;
    let mut staged = StagedLogFile {
        file,
        path: temporary,
    };
    staged.file.write_all(role.header().as_bytes())?;
    staged.file.flush()?;
    // The first visible destination already has its entire immutable role marker. A public
    // append opener that won this name is preserved, never truncated or replaced.
    std::fs::hard_link(&staged.path, path)?;
    let file = role.open(path)?;
    if !file.metadata()?.is_file()
        || file_key(&file)? != file_key(&staged.file)?
        || !has_header(&file, role.header())?
    {
        return Err(io::Error::other("published companion changed"));
    }
    Ok(file)
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
    let mut events = crate::event_log::EventLog::unrecorded();
    let mut log = open_paired_at(path, generated, events.id())?;
    events.retain(log.take_events().expect("admitted companion"));
    events.write("verify-event: {\"schema\":1,\"event\":\"invocation-started\"}");
    Ok(log)
}

fn open_paired_at(path: &Path, generated: bool, invocation: Option<&str>) -> io::Result<Log> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let directory = directory_lock(parent)?;
    let (mut file, new_human) = match publish_marked(path, Role::Human) {
        Ok(file) => (file, true),
        Err(error) if !generated && error.kind() == io::ErrorKind::AlreadyExists => {
            (Role::Human.open(path)?, false)
        }
        Err(error) => return Err(error),
    };
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
    let events = admit_events(&file, path, new_human)?;
    if !same_file(&file, path) || !same_file(&events, &events_path(path)) {
        return Err(io::Error::other("run log pair changed during admission"));
    }
    // Both private roles remain visible throughout reuse, before new run records.
    events.set_len(PRIVATE_EVENTS_HEADER.len() as u64)?;
    file.set_len(PRIVATE_HUMAN_HEADER.len() as u64)?;
    file.seek(io::SeekFrom::End(0))?;
    {
        let (device, inode) = file_key(&events)?;
        writeln!(
            file,
            "verify-pair: {}",
            aterm_json::json!({"schema":1, "id":invocation, "device":device, "inode":inode})
        )?;
    }
    let custody = Arc::new(custody);
    let events = Some(crate::event_log::Retained {
        file: events,
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
    events: File,
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
        let (events, _) =
            aterm_uds::regular_file::open(&events_at).map_err(|_| CandidateError::Unproven)?;
        let events =
            owned_events(&file, &events_at, events).map_err(|_| CandidateError::Unproven)?;
        Ok(Self {
            events,
            path,
            file,
            modified: metadata.modified().map_err(|_| CandidateError::Unproven)?,
            _custody: custody,
        })
    }

    fn remove(&self) -> bool {
        if !same_file(&self.file, &self.path) || !same_file(&self.events, &events_path(&self.path))
        {
            return false;
        }
        if std::fs::remove_file(&self.path).is_err() {
            return false;
        }
        let _ = std::fs::remove_file(events_path(&self.path));
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
        let mut log = open_paired_at(path, false, events.id()).unwrap();
        events.retain(log.take_events().unwrap());
        crate::event_log::emit(
            Some(&events),
            "verify-event: {\"schema\":1,\"event\":\"invocation-started\"}",
        );
        (log, events)
    }

    fn payload(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).unwrap();
        let after_header = bytes.strip_prefix(PRIVATE_HUMAN_HEADER.as_bytes()).unwrap();
        let newline = after_header.iter().position(|byte| *byte == b'\n').unwrap();
        assert!(after_header.starts_with(b"verify-pair: "));
        after_header[newline + 1..].to_vec()
    }

    #[test]
    fn ambiguous_raw_pair_or_first_descriptor_never_grants_reuse_or_pruning() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut preserved = Vec::new();
        for (at, (human, key, inserted, prior)) in [
            (true, "schema", "schema", "0"),
            (true, "id", "id", "\"unknown\""),
            (true, "id", "\\u0069d", "\"unknown\""),
            (true, "device", "device", "\"other\""),
            (true, "inode", "inode", "\"other\""),
            (false, "schema", "schema", "0"),
            (false, "event", "\\u0065vent", "\"other\""),
            (false, "invocation", "invocation", "null"),
            (false, "id", "\\u0069d", "\"unknown\""),
            (false, "identity_error", "identity_error", "\"unavailable\""),
            (false, "source_observation", "source_observation", "{}"),
        ]
        .into_iter()
        .enumerate()
        {
            let path = dir.path().join(format!("1-{}.log", at + 100));
            let (log, events) = paired(&path);
            log.file
                .set_modified(UNIX_EPOCH + Duration::from_secs(1))
                .unwrap();
            drop((log, events));
            let target = if human {
                path.clone()
            } else {
                events_path(&path)
            };
            let original = std::fs::read_to_string(&target).unwrap();
            let header = if human {
                PRIVATE_HUMAN_HEADER
            } else {
                PRIVATE_EVENTS_HEADER
            };
            let body = original.strip_prefix(header).unwrap();
            let needle = format!("\"{key}\":");
            let malformed = format!(
                "{header}{}",
                body.replacen(&needle, &format!("\"{inserted}\":{prior},{needle}"), 1)
            );
            assert_ne!(malformed, original);
            std::fs::write(&target, malformed).unwrap();
            let human_before = std::fs::read(&path).unwrap();
            let events_before = std::fs::read(events_path(&path)).unwrap();
            let human_key = file_key(&aterm_uds::regular_file::open(&path).unwrap().0).unwrap();
            let events_key = file_key(
                &aterm_uds::regular_file::open(&events_path(&path))
                    .unwrap()
                    .0,
            )
            .unwrap();
            assert!(Candidate::take(path.clone()).is_err());
            let next = crate::event_log::EventLog::unrecorded();
            assert!(open_paired_at(&path, false, next.id()).is_err());
            preserved.push((path, human_before, events_before, human_key, events_key));
        }
        let mut managed = Vec::new();
        for at in 0..LOGS_KEPT + 5 {
            let path = dir.path().join(format!("{}-{}.log", 100 + at, 1000 + at));
            let (log, events) = paired(&path);
            log.file
                .set_modified(UNIX_EPOCH + Duration::from_secs((100 + at) as u64))
                .unwrap();
            drop((log, events));
            managed.push(path);
        }
        let lock = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &lock);
        assert_eq!(
            managed.iter().filter(|path| path.exists()).count(),
            LOGS_KEPT
        );
        for (path, human, events, human_key, events_key) in preserved {
            assert_eq!(std::fs::read(&path).unwrap(), human);
            assert_eq!(std::fs::read(events_path(&path)).unwrap(), events);
            assert_eq!(
                file_key(&aterm_uds::regular_file::open(&path).unwrap().0).unwrap(),
                human_key
            );
            assert_eq!(
                file_key(
                    &aterm_uds::regular_file::open(&events_path(&path))
                        .unwrap()
                        .0
                )
                .unwrap(),
                events_key
            );
        }
    }

    #[test]
    fn published_private_roles_survive_reset_and_refuse_public_hardlink_openers() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.log");
        let (log, events) = paired(&path);
        let companion = events_path(&path);
        let alias = dir.path().join("alias");
        std::fs::hard_link(&companion, &alias).unwrap();
        let human_alias = dir.path().join("human-alias");
        std::fs::hard_link(&path, &human_alias).unwrap();
        drop((log, events));
        for target in [&companion, &alias, &path, &human_alias] {
            assert!(crate::event_log::EventLog::open(target).is_err());
        }
        let next = crate::event_log::EventLog::unrecorded();
        let log = open_paired_at(&path, false, next.id()).unwrap();
        // The interval before the new invocation descriptor is still private, never empty.
        assert_eq!(
            std::fs::read(&companion).unwrap(),
            PRIVATE_EVENTS_HEADER.as_bytes()
        );
        assert_eq!(
            std::fs::read(&alias).unwrap(),
            PRIVATE_EVENTS_HEADER.as_bytes()
        );
        assert!(crate::event_log::EventLog::open(&alias).is_err());
        assert!(
            std::fs::read(&human_alias)
                .unwrap()
                .starts_with(PRIVATE_HUMAN_HEADER.as_bytes())
        );
        assert!(crate::event_log::EventLog::open(&human_alias).is_err());
        assert!(Candidate::take(path.clone()).is_err());
        drop(log);
        // A marker alone supplies no positive pairing/descriptor authority after a crash.
        assert!(Candidate::take(path.clone()).is_err());
        assert!(companion.exists());
    }

    #[test]
    fn a_public_human_destination_wins_before_either_managed_artifact_changes() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1-17.log");
        let public = crate::event_log::EventLog::open(&path).unwrap();
        public.write("verify-event: {\"schema\":1,\"event\":\"public-human\"}");
        let before = std::fs::read(&path).unwrap();
        let companion = events_path(&path);
        std::fs::write(&companion, b"foreign existing companion\n").unwrap();
        assert!(open_paired_at(&path, false, public.id()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read(&companion).unwrap(),
            b"foreign existing companion\n"
        );
        public.write("verify-event: {\"schema\":1,\"event\":\"still-public\"}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("still-public")
        );
        assert!(Candidate::take(path.clone()).is_err());
    }

    #[test]
    fn a_human_header_without_pairing_is_not_reuse_or_deletion_authority() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1-18.log");
        let (log, events) = paired(&path);
        log.file.set_len(PRIVATE_HUMAN_HEADER.len() as u64).unwrap();
        drop((log, events));
        let companion_before = std::fs::read(events_path(&path)).unwrap();
        assert!(open_paired_at(&path, false, None).is_err());
        assert!(Candidate::take(path.clone()).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            PRIVATE_HUMAN_HEADER.as_bytes()
        );
        assert_eq!(std::fs::read(events_path(&path)).unwrap(), companion_before);
        assert!(crate::event_log::EventLog::open(&path).is_err());
    }

    #[test]
    fn an_old_unmarked_human_pair_preserves_its_public_writer_under_real_retention_pressure() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1-19.log");
        let (log, events) = paired(&path);
        log.file
            .set_modified(UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        drop((log, events));
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(
            &path,
            bytes.strip_prefix(PRIVATE_HUMAN_HEADER.as_bytes()).unwrap(),
        )
        .unwrap();
        // Restore deterministic old age after the historical-format write.
        Role::Human
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let public = crate::event_log::EventLog::open(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let companion_before = std::fs::read(events_path(&path)).unwrap();
        let physical = file_key(&aterm_uds::regular_file::open(&path).unwrap().0).unwrap();
        assert!(open_paired_at(&path, false, public.id()).is_err());
        assert!(Candidate::take(path.clone()).is_err());
        let mut managed = Vec::new();
        for at in 0..LOGS_KEPT + 5 {
            let newer = dir.path().join(format!("{}-{}.log", 200 + at, 2000 + at));
            let (log, events) = paired(&newer);
            log.file
                .set_modified(UNIX_EPOCH + Duration::from_secs((200 + at) as u64))
                .unwrap();
            drop((log, events));
            managed.push(newer);
        }
        let lock = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &lock);
        assert_eq!(
            managed.iter().filter(|path| path.exists()).count(),
            LOGS_KEPT
        );
        for old in managed.iter().take(5) {
            assert!(!old.exists());
            assert!(!events_path(old).exists());
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(events_path(&path)).unwrap(), companion_before);
        assert_eq!(
            file_key(&aterm_uds::regular_file::open(&path).unwrap().0).unwrap(),
            physical
        );
        public.write("verify-event: {\"schema\":1,\"event\":\"still-public-human\"}");
        let after = std::fs::read(&path).unwrap();
        assert!(after.starts_with(&before));
        assert!(
            String::from_utf8(after)
                .unwrap()
                .contains("still-public-human")
        );
    }

    #[test]
    fn a_public_destination_winner_cannot_be_truncated_replaced_or_pruned_as_managed() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1-15.log");
        let companion = events_path(&path);
        let public = crate::event_log::EventLog::open(&companion).unwrap();
        public.write("verify-event: {\"schema\":1,\"event\":\"public-one\"}");
        let before = std::fs::read(&companion).unwrap();
        let physical = file_key(&aterm_uds::regular_file::open(&companion).unwrap().0).unwrap();
        assert!(open_paired_at(&path, false, public.id()).is_err());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            PRIVATE_HUMAN_HEADER.as_bytes()
        );
        assert_eq!(std::fs::read(&companion).unwrap(), before);
        assert_eq!(
            file_key(&aterm_uds::regular_file::open(&companion).unwrap().0).unwrap(),
            physical
        );
        public.write("verify-event: {\"schema\":1,\"event\":\"public-two\"}");
        let after = std::fs::read(&companion).unwrap();
        assert!(after.starts_with(&before));
        let lock = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &lock);
        assert!(Candidate::take(path.clone()).is_err());
        assert_eq!(std::fs::read(&companion).unwrap(), after);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            PRIVATE_HUMAN_HEADER.as_bytes()
        );
        assert!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
        );
    }

    #[test]
    fn an_old_unmarked_pair_stays_unknown_while_its_public_writer_remains_usable() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("1-16.log");
        let (log, events) = paired(&path);
        log.file
            .set_modified(UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        drop((log, events));
        let companion = events_path(&path);
        let marked = std::fs::read(&companion).unwrap();
        std::fs::write(
            &companion,
            marked
                .strip_prefix(PRIVATE_EVENTS_HEADER.as_bytes())
                .unwrap(),
        )
        .unwrap();
        let human = std::fs::read(&path).unwrap();
        let public = crate::event_log::EventLog::open(&companion).unwrap();
        let before = std::fs::read(&companion).unwrap();
        let physical = file_key(&aterm_uds::regular_file::open(&companion).unwrap().0).unwrap();
        assert!(open_paired_at(&path, false, public.id()).is_err());
        assert!(Candidate::take(path.clone()).is_err());
        let mut managed = Vec::new();
        for at in 0..LOGS_KEPT + 5 {
            let newer = dir.path().join(format!("{}-{}.log", 100 + at, 1000 + at));
            let (log, events) = paired(&newer);
            log.file
                .set_modified(UNIX_EPOCH + Duration::from_secs((100 + at) as u64))
                .unwrap();
            drop((log, events));
            managed.push(newer);
        }
        let lock = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &lock);
        assert_eq!(
            managed.iter().filter(|path| path.exists()).count(),
            LOGS_KEPT
        );
        for removed in managed.iter().take(5) {
            assert!(!removed.exists());
            assert!(!events_path(removed).exists());
        }
        assert_eq!(std::fs::read(&companion).unwrap(), before);
        assert_eq!(std::fs::read(&path).unwrap(), human);
        assert_eq!(
            file_key(&aterm_uds::regular_file::open(&companion).unwrap().0).unwrap(),
            physical
        );
        public.write("verify-event: {\"schema\":1,\"event\":\"still-public\"}");
        let after = std::fs::read(&companion).unwrap();
        assert!(after.starts_with(&before));
        assert!(String::from_utf8(after).unwrap().contains("still-public"));
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
        let mut log = open_paired_at(&active, true, events.id()).unwrap();
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
        assert!(open_paired_at(&path, false, events.id()).is_err());
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
        let key = file_key(
            &aterm_uds::regular_file::open(&events_path(&path))
                .unwrap()
                .0,
        )
        .unwrap();
        drop((log, events));
        let (log, events) = paired(&path);
        assert_eq!(
            file_key(
                &aterm_uds::regular_file::open(&events_path(&path))
                    .unwrap()
                    .0
            )
            .unwrap(),
            key
        );
        assert_ne!(events.id(), Some(old_id.as_str()));
        let retained = std::fs::read_to_string(events_path(&path)).unwrap();
        assert!(retained.starts_with(PRIVATE_EVENTS_HEADER));
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
            format!(
                "{}{}",
                PRIVATE_EVENTS_HEADER,
                foreign.prepare("verify-event: {\"schema\":1,\"event\":\"invocation-started\"}")
            ),
        )
        .unwrap();
        let human_before = std::fs::read(&path).unwrap();
        let events_before = std::fs::read(events_path(&path)).unwrap();
        assert!(open_paired_at(&path, false, foreign.id()).is_err());
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
        assert!(open_paired_at(&path, false, Some("0123456789abcdef0123456789abcdef")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unproven human bytes\n");
        assert_eq!(
            std::fs::read(events_path(&path)).unwrap(),
            b"foreign bytes\n"
        );
        std::fs::remove_file(events_path(&path)).unwrap();
        // An old unmarked human is never migrated, even when its companion is absent.
        assert!(open_paired_at(&path, false, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unproven human bytes\n");
        assert!(!events_path(&path).exists());
        std::fs::remove_file(&path).unwrap();
        let events = crate::event_log::EventLog::unrecorded();
        drop(open_paired_at(&path, false, events.id()).unwrap());
        let before = std::fs::read(&path).unwrap();
        assert!(
            open_paired_at(&path, false, events.id()).is_err(),
            "an old empty file is not the current admitted creation"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read(events_path(&path)).unwrap(),
            PRIVATE_EVENTS_HEADER.as_bytes()
        );
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
        assert!(open_paired_at(&path, false, Some("0123456789abcdef0123456789abcdef")).is_err());
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
        let mut old = open_at(&log, false).unwrap();
        old.write_all(b"old run\n").unwrap();
        drop(old);
        let samples = sample_dir(&log);
        std::fs::create_dir(&samples).unwrap();
        std::fs::write(samples.join("old.sample.txt"), b"old stack\n").unwrap();

        drop(open_at(&log, false).expect("reopen the explicit log"));
        assert_eq!(payload(&log), b"");
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
        assert_eq!(payload(&path), b"still writing\n");
        active.file.write_all(b"finished\n").unwrap();
        assert_eq!(payload(&path), b"still writing\nfinished\n");
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
        assert_eq!(payload(&path), b"retained");
        drop(first);
        assert_eq!(
            open_at(&path, true).err().unwrap().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(payload(&path), b"retained");
        let chosen = open_at(&path, false).unwrap();
        assert!(
            payload(&path).is_empty(),
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
        assert_eq!(payload(&path), b"child writing\n");
        drop(directory);
        drop(writer.0.stdin.take());
        let ended = until_within(MUST_HAPPEN, "the owned writer ends", || {
            writer.0.try_wait().unwrap()
        });
        assert!(ended.success(), "{ended}");
        assert_eq!(payload(&path), b"child writing\nchild finished\n");
        pressure(dir.path(), 100);
        let directory = directory_lock(dir.path()).unwrap();
        prune(dir.path(), &directory);
        assert!(!path.exists(), "closed child log is now eligible");
    }
}
