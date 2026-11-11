// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File paths for `aterm acaffeine`, derived from the three root directories in [`Paths`].

use std::io;
use std::path::{Path, PathBuf};

/// Whether `dir` already exists as an owned real directory (`Ok(true)`), is missing (`Ok(false)`),
/// or is a symlink, non-directory, or owned by another user (`Err`).
pub(crate) fn existing_real_dir(dir: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.file_type().is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a directory", dir.display()),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if meta.uid() != aterm_uds::peer::our_uid() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("{} is not this user's directory", dir.display()),
                    ));
                }
            }
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Ensures `dir` is an owned real directory, refusing symlinked `dir` or an existing symlinked
/// parent before creating missing directories.
pub(crate) fn ensure_real_dir(dir: &Path) -> io::Result<()> {
    if existing_real_dir(dir)? {
        return Ok(());
    }
    if let Some(parent) = dir.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = existing_real_dir(parent)?;
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(dir)
            .map_err(|err| match err.raw_os_error() {
                Some(code) if code == libc::ELOOP || code == libc::ENOTDIR => io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a directory", dir.display()),
                ),
                _ => err,
            })?;
        let meta = opened.metadata()?;
        if !meta.is_dir() || meta.uid() != aterm_uds::peer::our_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not this user's directory", dir.display()),
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if existing_real_dir(dir)? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} does not exist", dir.display()),
            ))
        }
    }
}

/// Whether `path`'s parent directory is an existing, owned real directory (or `path` is bare).
#[must_use]
pub(crate) fn parent_is_real_dir(path: &Path) -> bool {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => matches!(existing_real_dir(dir), Ok(true)),
        _ => true,
    }
}

/// The mug's files, under three roots, and how to read whether a folder is on this Mac's disk.
#[derive(Clone, Debug)]
pub struct Paths {
    /// aterm's state directory (`~/Library/Application Support/aterm`).
    pub state_dir: PathBuf,
    /// The user's home directory (for `~/Library/LaunchAgents`).
    pub home: PathBuf,
    /// aterm's logs directory (`~/Library/Logs/aterm`).
    pub logs_dir: PathBuf,
    /// Whether a folder is on a volume local to this Mac: [`crate::launchd::on_local_volume`] in
    /// [`Self::from_env`]'s, and a fixed answer in a test's, which can then stand for a network
    /// home ([`Self::launch_agents_local`]).
    pub local_volume: fn(&Path) -> bool,
}

impl Paths {
    /// The roots aterm uses for this user (`aterm_types::dirs`); `None` without a home.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Some(Self {
            state_dir: aterm_types::dirs::state_dir()?,
            home: aterm_types::dirs::home_dir()?,
            logs_dir: aterm_types::dirs::logs_dir()?,
            local_volume: crate::launchd::on_local_volume,
        })
    }

    /// Whether [`Self::launch_agents`] is on a volume local to this Mac ([`Self::local_volume`]):
    /// where it is not, the home is a network one that another Mac may read at the same time.
    #[must_use]
    pub fn launch_agents_local(&self) -> bool {
        (self.local_volume)(&self.launch_agents())
    }

    /// The mode on disk ([`crate::state`]).
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("acaffeine.state")
    }

    /// Keep Awake with Lid Closed, the person's opt-in ([`crate::state::load_lid`]).
    #[must_use]
    pub fn lid_file(&self) -> PathBuf {
        self.state_dir.join("acaffeine.lid")
    }

    /// The lock a running mug holds for its whole life, with its pid inside
    /// ([`crate::instance`]).
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("acaffeine.lock")
    }

    /// PID file for `aterm acaffeine bridge` across an update restart.
    #[must_use]
    pub fn bridge_file(&self) -> PathBuf {
        self.state_dir.join("acaffeine.bridge")
    }

    /// Cloned bundle directory (`.noindex` excludes copies from Spotlight).
    #[must_use]
    pub fn code_dir(&self) -> PathBuf {
        self.state_dir.join("acaffeine-code.noindex")
    }

    /// What keeps the mug from keeping this Mac awake, and since when ([`crate::trouble`]).
    #[must_use]
    pub fn trouble_file(&self) -> PathBuf {
        self.state_dir.join("acaffeine.trouble")
    }

    /// The record that the person turned Enable at Startup off for `label`
    /// ([`crate::launchd::choose_startup`]): while it is there, no launch turns it back on.
    #[must_use]
    pub fn startup_off_file(&self, label: &str) -> PathBuf {
        self.state_dir
            .join("acaffeine")
            .join(format!("{label}.startup-off"))
    }

    /// The folder of the session-only launchd plists ([`Self::session_plist`]).
    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.state_dir.join("acaffeine")
    }

    /// Session-only launchd plist when Enable at Startup is off.
    #[must_use]
    pub fn session_plist(&self, label: &str) -> PathBuf {
        self.session_dir().join(format!("{label}.plist"))
    }

    /// Where launchd finds the jobs it loads at every login: `~/Library/LaunchAgents`.
    #[must_use]
    pub fn launch_agents(&self) -> PathBuf {
        self.home.join("Library").join("LaunchAgents")
    }

    /// Login-time launchd plist when Enable at Startup is on.
    #[must_use]
    pub fn startup_plist(&self, label: &str) -> PathBuf {
        self.launch_agents().join(format!("{label}.plist"))
    }

    /// The job's stderr.
    #[must_use]
    pub fn log_file(&self) -> PathBuf {
        self.logs_dir.join("acaffeine.log")
    }
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
