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

/// Default logs and event streams stay out of both fseventsd and Spotlight. Explicit
/// `--log` paths remain the caller's choice.
pub(super) fn default_dir(state: &Path) -> PathBuf {
    state.join(aterm_tempfile::QUIET).join("logs.noindex")
}

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
    signal_lock: Arc<std::sync::Mutex<()>>,
    // Fields close in declaration order: release custody only after the writer closes.
    events: Option<crate::event_log::Retained>,
    _custody: Arc<LockFile>,
}

impl io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _held = self
            .signal_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        io::Write::write(&mut self.file, bytes)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let _held = self
            .signal_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.file.write_all(bytes)
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
    /// Retain provenance immediately, even if the run cannot reach its final report.
    #[cfg(unix)]
    pub(super) fn observe_signals(
        &self,
        events: Option<crate::event_log::EventLog>,
    ) -> io::Result<()> {
        let file = self.file.try_clone()?;
        let lock = Arc::clone(&self.signal_lock);
        let custody = Arc::clone(&self._custody);
        aterm_uds::signal_origin::observe(move |line| {
            let _custody = &custody;
            let _held = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = writeln!(&file, "{line}");
            if let Some(events) = &events {
                let event = format!(
                    "verify-event: {}",
                    aterm_json::json!({"schema": 1, "event": "signal", "evidence": line})
                );
                events.write_prepared(&events.prepare(&event));
            }
        });
        Ok(())
    }
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
        signal_lock: Arc::new(std::sync::Mutex::new(())),
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
#[path = "run_log_tests.rs"]
mod tests;
