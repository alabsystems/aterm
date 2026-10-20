// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILES, WRITTEN THE CAREFUL WAY: the one writer of a whole file ([`write`],
//! [`write_checked`]) and aterm.toml's writer ([`ConfigFile`]), which the
//! config service's protocol binds `aterm fabric on|off` and `aterm fleet
//! init|join|leave` to, so each of them and the window are two writers of
//! one file rather than two files.
//!
//! A file is written through a NEW temporary file beside it
//! ([`create_temp_beside`]), never one opened: the path can sit in a
//! directory other users write, so a symlink planted at a predictable temp
//! name must not be written through. The name is unpredictable, the open
//! refuses anything already there, the mode is set on the descriptor, and
//! the bytes are synced before the rename, so a reader sees the old bytes or
//! the new, never a part.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Write `bytes` to `path` with `mode`, whole ([`write_checked`] with no
/// check). A link at `path` itself is replaced, not followed.
///
/// # Errors
///
/// No entropy for the temporary name, a clash on every name tried, or a
/// write's own error; `path` is left as it was.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_checked(path, bytes, mode, || Ok(()))
}

/// Write `bytes` to `path` with `mode`: its directory made, a temp beside it
/// ([`create_temp_beside`]) written, its mode set on the descriptor, synced,
/// then `check` run, then renamed over `path`, and the directory synced (best
/// effort: the file is in place either way). An `Err` anywhere — `check`'s
/// included — removes the temp and leaves `path` as it was.
///
/// # Errors
///
/// As [`write`], and `check`'s error.
pub fn write_checked(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (tmp, mut f) = create_temp_beside(path, mode)?;
    let result = (|| {
        f.write_all(bytes)?;
        // `create` with a mode is masked by the umask; the rename keeps
        // whatever the temp got, so say the mode again — on the DESCRIPTOR,
        // which is the file this process created and nothing else.
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "aterm-link's flushes (this writer moved from aterm-link's enable.rs) are the Ordered-tier work's to classify; the full flush is kept until then"
        )]
        f.sync_all()?;
        drop(f);
        check()?;
        std::fs::rename(&tmp, path)
    })();
    match result {
        Ok(()) => {
            if let Some(dir) = path.parent()
                && let Ok(dir) = std::fs::File::open(dir)
            {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "aterm-link's flushes (this writer moved from aterm-link's enable.rs) are the Ordered-tier work's to classify; best effort, the file is in place either way"
                )]
                let _ = dir.sync_all();
            }
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// A NEW file beside `path` to write it through: `.<name>.tmp.<pid>.<8 hex>`,
/// opened `O_CREAT|O_EXCL` (`create_new`) with `mode`. The exclusive create
/// fails on anything already at the name — a symlink included, dangling or
/// not (POSIX `open(2)`: with `O_CREAT` and `O_EXCL`, a symbolic link is not
/// followed) — so nothing planted there is ever written through; a clash is
/// retried under fresh random hex, a few times, then refused.
///
/// # Errors
///
/// No entropy for the name, a clash on every try, or the create's own error.
pub fn create_temp_beside(path: &Path, mode: u32) -> io::Result<(PathBuf, std::fs::File)> {
    create_temp_salted(path, mode, crate::rand::hex_token::<4>)
}

/// [`create_temp_beside`] with each try's salt from `salt`, which the tests script.
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

/// `O_NOFOLLOW | O_NONBLOCK` (std adds `O_CLOEXEC`), the flags the config
/// service opens its lock with: a link planted at the lock's name is refused,
/// not followed, and a FIFO is not waited on.
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
/// No audited values: the pre-open check and the descriptor check of the
/// config write lock still refuse anything at the lock's name that is not a
/// regular file.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub const LOCK_OPEN_FLAGS: i32 = 0;

/// How long an edit waits for the config service's write lock. The window
/// holds it for one save, whose two `F_FULLFSYNC`s were measured at
/// 82.5-279.7 ms on a busy volume (`native_document_host`'s
/// `PREFLIGHT_RETRY_BUDGET`), so this is several of the longest holds; past
/// it the edit refuses and says to run again.
pub const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(2);

/// aterm.toml, read for one edit and written the way the config service
/// writes a Settings save (`aterm-gui`'s `native_document_host`: read by
/// `read_config_atomic_file` with no lock, then `commit_atomic_bytes` under
/// it):
///
/// - A FINAL-FILE LINK IS FOLLOWED, NOT REPLACED. aterm.toml may be a link into
///   a dotfiles checkout, a layout the service binds and keeps. The text, the
///   `.bak` and the rename all land at the file the link names; a rename over
///   the link itself (what [`write`] does to a cap or key file, on purpose)
///   would turn it into a local copy every later save went to.
/// - THE SERVICE'S WRITE LOCK IS TAKEN FOR A WRITE, NEVER A READ
///   ([`Self::lock`]): `.<target name>.aterm-write.lock` beside the target,
///   opened no-follow, `flock` exclusive. A run that changes nothing creates
///   and holds nothing, so a no-op through a link into a directory nobody may
///   write (home-manager's into `/nix/store`) still reads `already`. A
///   Settings save that meets the lock retries for about half a second — its
///   25 ms lock budget inside a 500 ms preflight retry — and then fails as
///   busy, so the hold is the write alone: the `.bak` and the text, never the
///   parse or a line printed to a pipe nobody drains.
/// - NOTHING IS WRITTEN OVER BYTES THAT CHANGED since they were read: checked
///   under the lock before the `.bak` (an older `.bak` is not replaced for an
///   edit that never lands) and again just before the rename, so a writer
///   that ignores the lock (an editor) makes the edit refuse rather than have
///   its change reverted.
pub struct ConfigFile {
    /// The file the text lives in: the config path, or what a link there names.
    target: PathBuf,
    /// Its bytes and mode when read; `None` when there was no file.
    before: Option<(Vec<u8>, u32)>,
}

/// A [`ConfigFile`] under the config service's write lock, for one
/// [`Self::replace`]; the lock is let go when that returns.
pub struct ConfigEdit<'a> {
    file: &'a ConfigFile,
    _lock: ConfigWriteLock,
}

/// The config service's write lock on one config file, let go by `LOCK_UN`
/// before the close (`clippy.toml`'s flock release rule): a child forked
/// while it is held keeps a copy of the descriptor until it execs, and a lock
/// released by the close alone would stay taken on that copy.
struct ConfigWriteLock(std::fs::File);

impl ConfigWriteLock {
    /// Take the lock beside `target`, waiting up to [`CONFIG_LOCK_WAIT`].
    fn take(target: &Path) -> io::Result<Self> {
        Self::take_within(target, CONFIG_LOCK_WAIT, || {})
    }

    /// [`Self::take`], waiting up to `wait` and calling `busy` each time the lock is found
    /// held before waiting again (the tests let go of their holder there).
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
    /// Read `config` for an edit. No lock: a read changes nothing.
    ///
    /// # Errors
    ///
    /// A link at `config` that names no file (it is not written through to a
    /// file nobody made), or the read's own error.
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
    ///
    /// # Errors
    ///
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

    /// Take the config service's write lock for a write, waiting up to
    /// [`CONFIG_LOCK_WAIT`]; the target's directory is made first (a first
    /// edit writes a file nobody made).
    ///
    /// # Errors
    ///
    /// The lock held past the wait or not takeable, or the directory's own
    /// error.
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
    ///
    /// # Errors
    ///
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

impl ConfigEdit<'_> {
    /// Replace the text, keeping what was read as `<target>.bak` (only when
    /// something was there), and answer the `.bak`. An existing file keeps its
    /// own mode — an operator's 0644 aterm.toml is not silently made private —
    /// and a new one is 0600.
    ///
    /// # Errors
    ///
    /// The target's bytes changed since [`ConfigFile::open`] read them —
    /// found before the `.bak` is written, or before the rename — or a
    /// write's own error; in every case the target is left as it is.
    pub fn replace(self, text: &str) -> io::Result<Option<PathBuf>> {
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
        write_checked(&file.target, text.as_bytes(), mode, || file.unchanged())?;
        Ok(bak)
    }
}

#[cfg(test)]
#[path = "atomic_file_tests.rs"]
mod tests;
