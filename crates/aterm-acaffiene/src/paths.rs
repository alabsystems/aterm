// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File paths for `aterm acaffiene`, derived from the three root directories in [`Paths`].

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
        self.state_dir.join("acaffiene.state")
    }

    /// The lock a running mug holds for its whole life, with its pid inside
    /// ([`crate::instance`]).
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("acaffiene.lock")
    }

    /// PID file for `aterm acaffiene bridge` across an update restart.
    #[must_use]
    pub fn bridge_file(&self) -> PathBuf {
        self.state_dir.join("acaffiene.bridge")
    }

    /// Cloned bundle directory (`.noindex` excludes copies from Spotlight).
    #[must_use]
    pub fn code_dir(&self) -> PathBuf {
        self.state_dir.join("acaffiene-code.noindex")
    }

    /// What keeps the mug from keeping this Mac awake, and since when ([`crate::trouble`]).
    #[must_use]
    pub fn trouble_file(&self) -> PathBuf {
        self.state_dir.join("acaffiene.trouble")
    }

    /// The record that the person turned Enable at Startup off for `label`
    /// ([`crate::launchd::choose_startup`]): while it is there, no launch turns it back on.
    #[must_use]
    pub fn startup_off_file(&self, label: &str) -> PathBuf {
        self.state_dir
            .join("acaffiene")
            .join(format!("{label}.startup-off"))
    }

    /// The folder of the session-only launchd plists ([`Self::session_plist`]).
    #[must_use]
    pub fn session_dir(&self) -> PathBuf {
        self.state_dir.join("acaffiene")
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
        self.logs_dir.join("acaffiene.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_lives_under_its_root() {
        let paths = Paths {
            state_dir: PathBuf::from("/s"),
            home: PathBuf::from("/h"),
            logs_dir: PathBuf::from("/l"),
            local_volume: |_| true,
        };
        assert_eq!(paths.state_file(), PathBuf::from("/s/acaffiene.state"));
        assert_eq!(paths.lock_file(), PathBuf::from("/s/acaffiene.lock"));
        assert_eq!(paths.bridge_file(), PathBuf::from("/s/acaffiene.bridge"));
        assert_eq!(paths.code_dir(), PathBuf::from("/s/acaffiene-code.noindex"));
        assert_eq!(
            paths.session_plist("x.y"),
            PathBuf::from("/s/acaffiene/x.y.plist")
        );
        assert_eq!(
            paths.startup_plist("x.y"),
            PathBuf::from("/h/Library/LaunchAgents/x.y.plist")
        );
        assert_eq!(
            paths.startup_plist("x.y").parent(),
            Some(paths.launch_agents().as_path())
        );
        assert_eq!(paths.log_file(), PathBuf::from("/l/acaffiene.log"));
        assert_eq!(paths.trouble_file(), PathBuf::from("/s/acaffiene.trouble"));
        assert_eq!(
            paths.startup_off_file("x.y"),
            PathBuf::from("/s/acaffiene/x.y.startup-off")
        );
    }

    /// The login's paths read whether a folder is on this Mac's disk, as `statfs` says: a
    /// temporary folder is, and on macOS a path `statfs` cannot read is not, so a home whose
    /// LaunchAgents cannot be read is judged as a network one.
    #[test]
    fn the_logins_paths_read_the_volume() {
        let paths = Paths::from_env().expect("a home");
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        assert!((paths.local_volume)(dir.path()), "a temporary folder");
        #[cfg(target_os = "macos")]
        assert!(
            !(paths.local_volume)(&dir.path().join("missing")),
            "a path statfs cannot read"
        );
    }

    /// SYMLINKED `state_dir` IS REFUSED ACROSS ALL MUG FILES: `trouble`, `state`, `instance`,
    /// `launchd`, `code`, and `sweep_*` refuse to read, create, overwrite, or unlink files or
    /// subdirectories inside an outside directory aliased by a symlinked `state_dir`.
    /// NEGATIVE CONTROL: a real `state_dir` owned by the current user reads, writes, locks, and
    /// clears cleanly.
    #[cfg(unix)]
    #[test]
    fn symlinked_state_dir_is_refused_across_mug_files() {
        use crate::model::{Clocks, Mode};

        let root = aterm_tempfile::tempdir().expect("tempdir");
        let now = Clocks {
            since_boot: 100,
            wall: 1_700_000_000,
        };
        let label = "com.aterm.aterm.acaffiene.dev.0123abcd";
        let program = Path::new("/t/aterm");

        // Negative control: a real state directory works end to end.
        let real = Paths {
            state_dir: root.path().join("real-state"),
            home: root.path().join("home"),
            logs_dir: root.path().join("logs"),
            local_volume: |_| true,
        };
        crate::trouble::note_at(&real, "disk full", 10).expect("note real trouble");
        assert_eq!(crate::trouble::read(&real).map(|t| t.since), Some(10));
        crate::trouble::clear(&real);
        assert_eq!(crate::trouble::read(&real), None);
        crate::state::save_at(&real.state_file(), Mode::Off, now).expect("save real state");
        assert_eq!(
            crate::state::read(&real.state_file(), now).map(|s| s.mode),
            Some(Mode::Off)
        );
        let held = crate::instance::Instance::try_hold(&real.lock_file())
            .expect("hold real lock")
            .expect("free");
        assert_eq!(
            crate::instance::running_pid(&real.lock_file()),
            Some(std::process::id())
        );
        drop(held);

        // Adversarial case: `state_dir` is a symlink to `outside` holding pre-existing files.
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("acaffiene.trouble"), b"since=7\nwhy=keep\n")
            .expect("seed outside trouble");
        std::fs::write(outside.join("acaffiene.state"), b"mode=off\n").expect("seed outside state");
        std::fs::write(outside.join("acaffiene.lock"), b"99999\n").expect("seed outside lock");
        std::fs::write(outside.join("caffeine.bridge"), b"4242\n")
            .expect("seed outside legacy bridge");
        let aliased_state = root.path().join("aliased-state");
        std::os::unix::fs::symlink(&outside, &aliased_state).expect("symlink state_dir");
        let aliased = Paths {
            state_dir: aliased_state,
            home: root.path().join("aliased-home"),
            logs_dir: root.path().join("aliased-logs"),
            local_volume: |_| true,
        };

        assert_eq!(crate::trouble::read(&aliased), None);
        assert!(crate::trouble::note_at(&aliased, "spoof", 99).is_err());
        crate::trouble::clear(&aliased);
        assert_eq!(
            std::fs::read(outside.join("acaffiene.trouble")).expect("outside trouble kept"),
            b"since=7\nwhy=keep\n"
        );

        assert_eq!(crate::state::read(&aliased.state_file(), now), None);
        assert!(crate::state::save_at(&aliased.state_file(), Mode::Forever, now).is_err());
        for (_, res) in crate::state::sweep_temps(&aliased) {
            assert!(res.is_err());
        }
        assert_eq!(
            std::fs::read(outside.join("acaffiene.state")).expect("outside state kept"),
            b"mode=off\n"
        );

        assert!(crate::instance::Instance::try_hold(&aliased.lock_file()).is_err());
        assert_eq!(crate::instance::running_holder(&aliased.lock_file()), None);
        assert_eq!(crate::instance::last_holder(&aliased.lock_file()), None);
        assert_eq!(
            std::fs::read(outside.join("acaffiene.lock")).expect("outside lock kept"),
            b"99999\n"
        );

        assert!(crate::launchd::choose_startup(&aliased, label, program, false).is_err());
        assert!(crate::launchd::choose_startup(&aliased, label, program, true).is_err());
        assert!(!crate::launchd::startup_turned_off(&aliased, label));
        assert!(!outside.join("acaffiene").exists());
        let plist_swept = crate::launchd::sweep_plist_temps(&aliased);
        assert_eq!(plist_swept.len(), 2);
        for (_, res) in plist_swept {
            assert!(res.is_err());
        }

        let app = root.path().join("Fake.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&app).expect("fake bundle");
        let exe = app.join("aterm");
        std::fs::write(&exe, b"bin").expect("fake exe");
        assert!(crate::code::prepare_copy(&exe, &aliased.code_dir(), label).is_err());
        assert!(crate::code::discard_copies(&aliased.code_dir()).is_err());
        assert!(!outside.join("acaffiene-code.noindex").exists());
        assert_eq!(
            std::fs::read(outside.join("caffeine.bridge")).expect("outside legacy bridge kept"),
            b"4242\n"
        );
    }
}
