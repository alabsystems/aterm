// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run-log custody: creation and retention share a short directory lock; a writer holds its
//! sidecar until its file closes. Retention never waits for a writer. Uncoordinated old logs
//! supply no deletion authority and stay outside the managed quota.
//! Default logs live below the old flat directory, whose older pruners do not traverse folders.

use aterm_uds::file_lock::LockFile;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOGS_KEPT: usize = 20;

pub(super) struct Log {
    pub(super) path: PathBuf,
    file: File,
    // Fields close in declaration order: release custody only after the writer closes.
    _custody: LockFile,
}

impl io::Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.file, bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.file)
    }
}

pub(super) fn open(chosen: Option<&Path>, cwd: &Path, dir: &Path) -> Option<Log> {
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
    match open_at(&path, chosen.is_none()) {
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

fn open_at(path: &Path, generated: bool) -> io::Result<Log> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let directory = directory_lock(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
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
    let file = options.open(path)?;
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
    file.set_len(0)?;
    let log = Log {
        path: path.to_path_buf(),
        file,
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

fn same_file(file: &File, path: &Path) -> bool {
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
    path: PathBuf,
    file: File,
    modified: SystemTime,
    _custody: LockFile,
}

impl Candidate {
    fn take(path: PathBuf) -> Option<Self> {
        let custody =
            LockFile::take_existing(&sidecar(&path), Instant::now(), "active run log").ok()?;
        let (file, metadata) = aterm_uds::regular_file::open(&path).ok()?;
        Some(Self {
            path,
            file,
            modified: metadata.modified().ok()?,
            _custody: custody,
        })
    }

    fn remove(&self) -> bool {
        if !same_file(&self.file, &self.path) || std::fs::remove_file(&self.path).is_err() {
            return false;
        }
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
        count += 1;
        if let Some(candidate) = Candidate::take(path) {
            candidates.push(candidate);
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
    use std::io::Write as _;

    #[test]
    fn default_logs_survive_an_older_flat_directory_pruner() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let flat = dir.path().join("logs.noindex");
        std::fs::create_dir(&flat).unwrap();
        for i in 0..LOGS_KEPT + 3 {
            std::fs::write(flat.join(format!("1700000000-{}.log", i + 1)), b"old log").unwrap();
        }
        let mut live = open(None, dir.path(), &flat).unwrap();
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
        assert_eq!(std::fs::read(&live.path).unwrap(), b"current run\n");
        live.write_all(b"still writing\n").unwrap();
        assert_eq!(
            std::fs::read(&live.path).unwrap(),
            b"current run\nstill writing\n"
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
                write_at(
                    dir.path(),
                    &format!("1700000000-{}.log", i + 2),
                    100 + i as u64,
                )
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
        let _directory = directory_lock(dir.path()).unwrap();
        let candidate = Candidate::take(path.clone()).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"replacement bytes").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(!candidate.remove());
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement bytes");
        assert!(sidecar(&path).exists());
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
