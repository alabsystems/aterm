// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILES, WRITTEN THE CAREFUL WAY.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Write `bytes` to `path` with `mode`, whole ([`write_checked`] with no check).
/// # Errors
/// No entropy for the temporary name, a clash on every name tried, or a write's own error: `path`
/// is left as it was. Or the directory's flush after the rename failed: `path` holds `bytes`,
/// but its entry may not survive a power loss.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_checked(path, bytes, mode, || Ok(()))
}

/// Write `bytes` to `path` with `mode`: a temporary file beside it, durably flushed
/// ([`aterm_durable::durable`]), renamed over it, and the directory's entries flushed after
/// the rename. That flush is best-effort ([`aterm_durable::Flush::dir_best_effort`]): a
/// directory whose entries cannot be flushed here is no error, and any other failure is.
/// # Errors
/// As [`fn@write`], and `check`'s error.
pub fn write_checked(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    write_flushing_dir(path, bytes, mode, check, |dir| {
        aterm_durable::Flush::Durable.dir(dir)
    })
}

/// [`write_checked`] with the directory's flush (`flush_dir`) handed in, so a test can make
/// it fail; its outcome is classified here either way.
fn write_flushing_dir(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
    flush_dir: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    write_landing(path, bytes, mode, check, flush_dir)?
}

/// What a careful write's rename left ([`write_landing`]): `path` holds the bytes either way.
/// `Err` is the directory's flush after the rename, failed other than as unavailable, so the
/// entry may not survive a power loss.
type Landed = io::Result<()>;

/// [`write_flushing_dir`], telling a failure before the rename (the outer `Err`: `path` is as
/// it was) from one after it ([`Landed`]).
fn write_landing(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
    flush_dir: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Landed> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (tmp, mut f) = create_temp_beside(path, mode)?;
    let result = (|| {
        f.write_all(bytes)?;
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        aterm_durable::durable(&f)?;
        drop(f);
        check()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    // Unavailable (a volume that cannot flush a directory, a directory this process may
    // write but not open) is accepted: the bytes are flushed on their own handle, and the
    // rename has landed. A failed flush is returned as it came, its errno kept for a caller
    // that classifies it.
    Ok(aterm_durable::DirFlushed::classify(flush_dir(dir)).map(|_flushed_or_unavailable| ()))
}

/// A NEW file beside `path` to write it through.
/// # Errors
/// No entropy for the name, a clash on every try, or the create's own error.
pub fn create_temp_beside(path: &Path, mode: u32) -> io::Result<(PathBuf, std::fs::File)> {
    create_temp_salted(path, mode, crate::rand::hex_token::<4>)
}

fn create_temp_salted(
    path: &Path,
    mode: u32,
    mut salt: impl FnMut() -> io::Result<String>,
) -> io::Result<(PathBuf, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    for _ in 0..8 {
        let salt = salt()?;
        let tmp = path.with_file_name(format!(".{name}.tmp.{}.{salt}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
        {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "every temporary name tried beside {} was already taken",
            path.display()
        ),
    ))
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
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(LOCK_OPEN_FLAGS)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(not_a_file());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let deadline = Instant::now() + wait;
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
        let before = match std::fs::read(&target) {
            Ok(bytes) => {
                let mode = std::fs::metadata(&target)?.permissions().mode() & 0o777;
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
        let now = match std::fs::read(&self.target) {
            Ok(bytes) => Some(bytes),
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
