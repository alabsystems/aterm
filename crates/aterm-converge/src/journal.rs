// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `<state_dir>/converge/journal.ndjson`: one line per change, flushed to stable media before
//! applying, and one per item an apply run ended failed.

use crate::engine::Journal;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Bounded NDJSON change journal (`{"time", "id", "from", "to", "revert"}`, and `{"time", "id",
/// "failed"}` for an item an apply run ended failed) with single-rotation locking.
#[derive(Debug, Clone)]
pub struct FileJournal {
    dir: PathBuf,
    bound: u64,
    wait: Duration,
}

#[derive(Serialize)]
struct Line<'a> {
    time: u64,
    id: &'a str,
    from: &'a str,
    to: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    revert: Option<&'a str>,
}

#[derive(Serialize)]
struct Failed<'a> {
    time: u64,
    id: &'a str,
    failed: &'a str,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl FileJournal {
    /// The folder under the state directory.
    pub const DIR: &'static str = "converge";
    /// The journal.
    pub const FILE: &'static str = "journal.ndjson";
    /// The one rotation kept.
    pub const ROTATED: &'static str = "journal.1.ndjson";
    /// The most bytes the journal holds before it rotates: 1 MiB.
    pub const BOUND: u64 = 1 << 20;
    /// The advisory lock each append holds, beside the journal.
    pub const LOCK: &'static str = "journal.lock";
    /// Maximum lock wait before refusing an append.
    pub const WAIT: Duration = Duration::from_secs(60);

    /// Returns a journal rooted at `<state_dir>/converge`.
    #[must_use]
    pub fn under(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(Self::DIR),
            bound: Self::BOUND,
            wait: Self::WAIT,
        }
    }

    /// The journal file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.dir.join(Self::FILE)
    }

    /// The rotation kept beside it.
    #[must_use]
    pub fn rotated(&self) -> PathBuf {
        self.dir.join(Self::ROTATED)
    }

    /// [`FileJournal::under`] with a small bound, so a test rotates in a few lines.
    #[cfg(test)]
    fn bounded(state_dir: &Path, bound: u64) -> Self {
        Self {
            bound,
            ..Self::under(state_dir)
        }
    }

    fn append(&self, line: &[u8]) -> Result<(), String> {
        let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
        if len > self.bound {
            return Err(format!(
                "a {len}-byte line is over the journal's {}-byte bound",
                self.bound
            ));
        }
        let path = self.path();
        let shown = |e: io::Error, at: &Path| format!("{}: {e}", at.display());
        make_dir(&self.dir).map_err(|e| shown(e, &self.dir))?;
        let lock_path = self.dir.join(Self::LOCK);
        let _held = Held::take(&lock_path, self.wait).map_err(|e| shown(e, &lock_path))?;
        let (mut file, held) = open_append(&path).map_err(|e| shown(e, &path))?;
        let fresh = held == 0 || held.saturating_add(len) > self.bound;
        if held > 0 && held.saturating_add(len) > self.bound {
            drop(file);
            let rotated = self.rotated();
            match fs::symlink_metadata(&rotated) {
                Ok(meta) if meta.is_file() => {
                    #[cfg(unix)]
                    ensure_effective_owner(&meta, "rotated journal")
                        .map_err(|e| shown(e, &rotated))?;
                }
                Ok(_) => return Err(format!("{}: not a regular file", rotated.display())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(shown(e, &rotated)),
            }
            fs::rename(&path, &rotated).map_err(|e| {
                format!(
                    "{}: moving it to {}: {e}",
                    path.display(),
                    rotated.display()
                )
            })?;
            (file, _) = open_append(&path).map_err(|e| shown(e, &path))?;
        }
        file.write_all(line).map_err(|e| shown(e, &path))?;
        aterm_durable::durable_data(&file).map_err(|e| shown(e, &path))?;
        if fresh {
            aterm_durable::durable_dir(&self.dir).map_err(|e| shown(e, &self.dir))?;
        }
        Ok(())
    }
}

impl Journal for FileJournal {
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        let mut line = aterm_json::to_string(&Line {
            time: now_secs(),
            id,
            from,
            to,
            revert,
        })
        .map_err(|e| e.to_string())?;
        line.push('\n');
        self.append(line.as_bytes())
    }

    fn failed(&mut self, id: &str, why: &str) -> Result<(), String> {
        let mut line = aterm_json::to_string(&Failed {
            time: now_secs(),
            id,
            failed: why,
        })
        .map_err(|e| e.to_string())?;
        line.push('\n');
        self.append(line.as_bytes())
    }
}

#[cfg(unix)]
fn ensure_effective_owner(meta: &fs::Metadata, what: &str) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    // SAFETY: `geteuid` takes no pointers or preconditions and always returns the effective UID.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{what} is not owned by the current user"),
        ));
    }
    Ok(())
}

fn make_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty())
        && let Ok(meta) = fs::symlink_metadata(parent)
        && !meta.is_dir()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    if let Ok(meta) = fs::symlink_metadata(dir)
        && !meta.is_dir()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let dir_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)
            .map_err(|e| match e.raw_os_error() {
                Some(libc::ELOOP | libc::ENOTDIR) => {
                    io::Error::new(io::ErrorKind::InvalidInput, "not a directory")
                }
                _ => e,
            })?;
        let meta = dir_file.metadata()?;
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
        ensure_effective_owner(&meta, "journal directory")?;
        if meta.permissions().mode() & 0o7777 != 0o700 {
            dir_file.set_permissions(fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    {
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
    }
    Ok(())
}

struct Held(File);

impl Held {
    fn take(path: &Path, wait: Duration) -> io::Result<Self> {
        #[cfg(not(unix))]
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            std::os::unix::fs::OpenOptionsExt::custom_flags(
                &mut options,
                libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            );
        }
        let file: std::fs::File = options.open(path).map_err(|e| match e.raw_os_error() {
            #[cfg(unix)]
            Some(libc::ELOOP | libc::EISDIR) => {
                io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
            }
            _ if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) => {
                io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
            }
            _ => e,
        })?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            ensure_effective_owner(&meta, "journal lock")?;
            if meta.permissions().mode() & 0o7777 != 0o600 {
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
        }
        let deadline = Instant::now().checked_add(wait);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock)
                    if deadline.is_none_or(|d| Instant::now() < d) =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "another run held the journal for more than {}s",
                            wait.as_secs()
                        ),
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn open_append(path: &Path) -> io::Result<(File, u64)> {
    #[cfg(not(unix))]
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        std::os::unix::fs::OpenOptionsExt::custom_flags(
            &mut options,
            libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        );
    }
    let file = options.open(path).map_err(|e| match e.raw_os_error() {
        #[cfg(unix)]
        Some(libc::ELOOP | libc::EISDIR) => {
            io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
        }
        _ if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) => {
            io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
        }
        _ => e,
    })?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        ensure_effective_owner(&meta, "journal file")?;
        if meta.permissions().mode() & 0o7777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok((file, meta.len()))
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
