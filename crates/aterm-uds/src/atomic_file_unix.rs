// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILES, WRITTEN THE CAREFUL WAY: the unix half of [`crate::atomic_file`], whose items it
//! re-exports.

use super::writer::write_landing;
#[cfg(test)]
use super::writer::{
    create_temp_salted, sweep_temps_with, write_captured_landing, write_flushing_dir,
    write_produced_landing,
};
use super::{MAX_CONFIG_BYTES, read_regular, write};
#[cfg(test)]
use super::{create_temp_beside, sweep_temps_beside, temp_target, temp_writer, write_checked};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Change an entry's mode without following a final-component symlink, even if
/// that entry is replaced after the metadata check. Ancestors resolve normally.
/// Unlike opening for read, this permits an owner to recover mode-000 entries and
/// harden sockets. A stable symlink is refused; unsupported nofollow chmod fails closed.
///
/// # Errors
/// Refuses symlinks, NUL-containing paths, and any filesystem or permission error.
pub fn set_mode_nofollow(path: &Path, mode: u32) -> io::Result<()> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "chmod refuses a symlink",
        ));
    }
    chmod_entry_nofollow(path, mode)
}

// Separate the kernel operation so the replacement window after metadata can be
// exercised deterministically, without timing races or production test hooks.
fn chmod_entry_nofollow(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: name is NUL-terminated and lives through the syscall. NOFOLLOW is
    // enforced by the kernel/libc operation, not by the preceding metadata read.
    let rc = unsafe {
        libc::fchmodat(
            libc::AT_FDCWD,
            name.as_ptr(),
            mode as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// `O_NOFOLLOW | O_NONBLOCK` (std adds `O_CLOEXEC`).
#[cfg(target_vendor = "apple")]
pub const LOCK_OPEN_FLAGS: i32 = 0x0100 | 0x0004;
/// arm and arm64 Linux spell `O_NOFOLLOW` 0o100000 ([`crate::ownerlock`]).
#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
pub const LOCK_OPEN_FLAGS: i32 = 0o100_000 | 0o4000;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "aarch64", target_arch = "arm"))
))]
pub const LOCK_OPEN_FLAGS: i32 = 0o400_000 | 0o4000;
/// No audited values.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub const LOCK_OPEN_FLAGS: i32 = 0;

/// `O_NOCTTY | O_NOFOLLOW` (std adds `O_CLOEXEC`) for interactive `/dev/tty` opens.
#[cfg(target_vendor = "apple")]
pub const TTY_OPEN_FLAGS: i32 = 0x20000 | 0x0100;
/// `O_NOCTTY | O_NOFOLLOW` on arm/aarch64 Linux (`O_NOCTTY = 0o400`, `O_NOFOLLOW = 0o100000`).
#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
pub const TTY_OPEN_FLAGS: i32 = 0o400 | 0o100_000;
/// `O_NOCTTY | O_NOFOLLOW` on other Linux (`O_NOCTTY = 0o400`, `O_NOFOLLOW = 0o400000`).
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "aarch64", target_arch = "arm"))
))]
pub const TTY_OPEN_FLAGS: i32 = 0o400 | 0o400_000;
/// No audited values.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub const TTY_OPEN_FLAGS: i32 = 0;

/// `O_NOCTTY | O_NOFOLLOW | O_NONBLOCK` (std adds `O_CLOEXEC`) for non-blocking `/dev/tty` probes.
pub const TTY_PROBE_OPEN_FLAGS: i32 = TTY_OPEN_FLAGS | LOCK_OPEN_FLAGS;

/// How long an edit waits for the config service's write lock.
pub const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(2);

/// aterm.toml, read for one edit and written the way the config service writes a Settings save.
pub struct ConfigFile {
    /// The file the text lives in: the config path, or what a link there names.
    target: PathBuf,
    /// Its bytes and mode when read; `None` when there was no file.
    before: Option<(Vec<u8>, u32)>,
}

/// A [`ConfigFile`] under the config service's write lock.
pub struct ConfigEdit<'a> {
    file: &'a ConfigFile,
    _lock: ConfigWriteLock,
}

struct ConfigWriteLock(std::fs::File);

impl ConfigWriteLock {
    fn take(target: &Path) -> io::Result<Self> {
        Self::take_within(target, CONFIG_LOCK_WAIT, || {})
    }

    fn take_within(target: &Path, wait: Duration, mut busy: impl FnMut()) -> io::Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let name = target
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let path = target.with_file_name(format!(".{name}.aterm-write.lock"));
        let not_a_file = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the config write lock {} is not a regular file",
                    path.display()
                ),
            )
        };
        if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(not_a_file());
        }
        #[cfg(test)]
        tests::race();
        let file: std::fs::File = crate::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(LOCK_OPEN_FLAGS)
                .open(&path)
        })?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(not_a_file());
        }
        if meta.permissions().mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let deadline = crate::deadline::saturating_after(Instant::now(), wait);
        let mut backoff = Duration::from_millis(1);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    busy();
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                            "another writer holds {} (a Settings save); run this again",
                            path.display()
                        ),
                    ));
                }
            }
        }
    }
}

impl Drop for ConfigWriteLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl ConfigFile {
    /// Read `config` for an edit.
    /// # Errors
    /// A link at `config` that names no file (it is not written through to a file nobody made), or
    /// the read's own error.
    pub fn open(config: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let target = match std::fs::symlink_metadata(config) {
            Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(config).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "a link to {}, which does not resolve ({e})",
                        std::fs::read_link(config)
                            .map_or_else(|_| "?".into(), |t| t.display().to_string())
                    ),
                )
            })?,
            _ => config.to_path_buf(),
        };
        let before = match read_regular(&target, MAX_CONFIG_BYTES) {
            Ok((bytes, meta)) => {
                let mode = meta.permissions().mode() & 0o777;
                Some((bytes, mode))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Self { target, before })
    }

    /// The text as read; `None` when there was no file.
    /// # Errors
    /// A file that is not UTF-8.
    pub fn text(&self) -> io::Result<Option<&str>> {
        self.before
            .as_ref()
            .map(|(bytes, _)| {
                std::str::from_utf8(bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            })
            .transpose()
    }

    /// Take the config service's write lock for a write.
    /// # Errors
    /// The lock held past the wait or not takeable, or the directory's own error.
    pub fn lock(&self) -> io::Result<ConfigEdit<'_>> {
        if let Some(dir) = self.target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(ConfigEdit {
            file: self,
            _lock: ConfigWriteLock::take(&self.target)?,
        })
    }

    /// `Ok` while the target holds the bytes [`Self::open`] read.
    /// # Errors
    /// It changed — or appeared, or went — since; or the read's own error.
    fn unchanged(&self) -> io::Result<()> {
        let now = match read_regular(&self.target, MAX_CONFIG_BYTES) {
            Ok((bytes, _)) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if now.as_deref() == self.before.as_ref().map(|(b, _)| b.as_slice()) {
            Ok(())
        } else {
            Err(io::Error::other(
                "aterm.toml changed while this was editing it; run again",
            ))
        }
    }
}

/// [`ConfigEdit::replace`]'s error once the edit is in place: the target holds the new text,
/// but the directory's flush after the rename failed (other than as unavailable,
/// [`aterm_durable::DirFlushed::classify`]), so the edit is in effect and may not survive a
/// power loss. It travels inside the [`io::Error`] `replace` returns, which keeps the
/// flush's kind; [`io::Error::get_ref`] and a downcast find it.
#[derive(Debug)]
pub struct EditLanded {
    /// The file that holds the new text.
    pub target: PathBuf,
    /// Where the text it replaced was saved; `None` when there was no file.
    pub bak: Option<PathBuf>,
    /// The directory flush's error, as it came.
    pub flush: io::Error,
}

impl std::fmt::Display for EditLanded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the edit is in place in {}", self.target.display())?;
        if let Some(bak) = &self.bak {
            write!(f, " (the previous text saved as {})", bak.display())?;
        }
        write!(
            f,
            ", but flushing its directory failed, so it may not survive a power loss: {}",
            self.flush
        )
    }
}

impl std::error::Error for EditLanded {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.flush)
    }
}

impl EditLanded {
    /// The [`EditLanded`] `error` carries: an edit that is in effect though its directory's
    /// flush failed. `None` for any other error from [`ConfigEdit::replace`], which left the
    /// target as it was.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref::<Self>()
    }
}

impl ConfigEdit<'_> {
    /// Replace the text.
    /// # Errors
    /// The target's bytes changed since [`ConfigFile::open`] read them — found before the `.bak` is
    /// written, or before the rename — or a write's own error: the target is left as it is. Or
    /// the directory's flush after the rename failed other than as unavailable: the edit is in
    /// effect, and the error carries [`EditLanded`] under the flush's kind.
    pub fn replace(self, text: &str) -> io::Result<Option<PathBuf>> {
        self.replace_flushing_dir(text, |dir| aterm_durable::Flush::Durable.dir(dir))
    }

    /// [`Self::replace`] with the directory's flush after the rename (`flush_dir`) handed in,
    /// so a test can make it fail.
    fn replace_flushing_dir(
        self,
        text: &str,
        flush_dir: impl FnOnce(&Path) -> io::Result<()>,
    ) -> io::Result<Option<PathBuf>> {
        let file = self.file;
        file.unchanged()?;
        let (bak, mode) = match &file.before {
            Some((old, mode)) => {
                let mut bak = file.target.clone().into_os_string();
                bak.push(".bak");
                let bak = PathBuf::from(bak);
                write(&bak, old, *mode)?;
                (Some(bak), *mode)
            }
            None => (None, 0o600),
        };
        #[cfg(test)]
        tests::race();
        write_landing(
            &file.target,
            text.as_bytes(),
            mode,
            || file.unchanged(),
            flush_dir,
        )?
        .map_err(|flush| {
            io::Error::new(
                flush.kind(),
                EditLanded {
                    target: file.target.clone(),
                    bak: bak.clone(),
                    flush,
                },
            )
        })?;
        Ok(bak)
    }
}

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "atomic_file_producer_tests.rs"]
mod producer_tests;
