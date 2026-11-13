// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boot-health sentinel: stops a launch crash from bricking the fleet after a self-swap.

use std::io;
use std::path::{Path, PathBuf};

pub use aterm_durable::Flush;

/// The process that made one counted launch: its pid and the kernel's start time for it (seconds, microseconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchName {
    pub pid: u32,
    pub started: (i64, i64),
}

/// The most launch names the sentinel keeps; the oldest go first. Every build that
/// reaches the revert threshold counts only a few, so this only bounds the file.
const MAX_NAMED_LAUNCHES: usize = 32;

/// A durable boot-health sentinel backed by a small file `"<build> <attempts>[ <pid>:<seconds>:<micros>]*"`.
pub struct Sentinel {
    path: PathBuf,
    /// How [`Self::arm`], the launch counts and [`Self::forgive_launch`] finish their
    /// write ([`Self::new`]: durable; [`Self::ordered`]: ordered).
    flush: Flush,
}

impl Sentinel {
    /// A sentinel backed by `path` (typically under the hardened private dir).
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self {
            path,
            flush: Flush::Durable,
        }
    }

    /// A sentinel-shaped counter backed by `path` whose every write is only ORDERED ([`aterm_durable::ordered`]): for a count whose older copy is the conservative one (the budget for escaping a wedged foreign trial, which an older count restarts).
    #[must_use]
    pub const fn ordered(path: PathBuf) -> Self {
        Self {
            path,
            flush: Flush::Ordered,
        }
    }

    /// Arm the sentinel just before applying a swap to `build`: records `build` with zero attempts, overwriting any stale sentinel.
    /// # Errors
    /// As [`Self::arm_with`].
    pub fn arm(&self, build: u64) -> io::Result<()> {
        self.arm_with(build, self.flush)
    }

    /// [`Self::arm`], finished with `flush` whatever this sentinel's own: the swap's caller arms with [`Flush::Ordered`] when the swap it precedes lands on this sentinel's device ([`Flush::ordered_if_same_device`]), so the barrier keeps a swapped bundle from persisting ahead of its armed sentinel.
    /// # Errors
    /// Before the rename, a write, flush or rename failure leaves the previous record as it
    /// was. A directory flush failure after the rename attempts to remove the new arm;
    /// failure of that removal is logged and can leave the arm present. Removal has no
    /// directory barrier, so a power loss may also restore the record. On any `Err`, the
    /// caller must neither swap nor keep the trial identity it wrote for this arm.
    pub fn arm_with(&self, build: u64, flush: Flush) -> io::Result<()> {
        self.arm_at(build, 0, flush)
    }

    /// [`Self::arm_with`] for a resumed trial: the count of `build` starts at `attempts`, the
    /// launches an earlier trial of the same bytes counted and nothing proved, so the launch
    /// that reaches the revert threshold is the same whatever the spacing of the launches.
    /// # Errors
    /// As [`Self::arm_with`].
    pub fn arm_at(&self, build: u64, attempts: u32, flush: Flush) -> io::Result<()> {
        // Required: the swap an arm precedes must not reach the media ahead of it, so a failed
        // flush of the folder is the arm's error, and its caller does not swap. Try to remove
        // the record: an armed sentinel whose caller does not swap would count the
        // running build's launches against a trial nothing started.
        let Err(unflushed) = self.write_record(build, attempts, &[], flush)? else {
            return Ok(());
        };
        if let Err(error) = self.confirm() {
            aterm_log::warn!(
                "boot sentinel {}: flushing its folder failed after the arm for build {build} \
                 ({unflushed}), and taking the arm back failed too: {error}",
                self.path.display()
            );
        }
        Err(unflushed)
    }

    /// Start the count of `build` at zero, as [`Self::arm`] does, for a counter no swap
    /// follows (the budget for escaping a wedged foreign trial): the write is a count's
    /// ([`Self::write_count`]), so a failed flush of the folder after the rename is logged
    /// and the count stands.
    /// # Errors
    /// The write, the flush or the rename failed: the record is as it was.
    pub fn restart_count(&self, build: u64) -> io::Result<()> {
        self.write_count(build, 0, &[], self.flush)
    }

    /// The armed `(build, attempts)`, or `None` if absent or unparseable: a corrupt sentinel fails toward not reverting, the disruptive action.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn read_state(&self) -> Option<(u64, u32)> {
        self.read_record()
            .map(|(build, attempts, _)| (build, attempts))
    }

    /// Whether a sentinel FILE is present that [`Self::read_state`] cannot read — a corrupt or unreadable marker, which `read_state` answers as no trial at all.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn unreadable(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|meta| meta.is_file())
            && self.read_record().is_none()
    }

    /// The named launches of `running_build` still on the count (see [`Self::forgive_launch_of`]); empty for another build or none.
    #[must_use]
    pub fn named_launches(&self, running_build: u64) -> Vec<LaunchName> {
        match self.read_record() {
            Some((build, _, names)) if build == running_build => names,
            _ => Vec::new(),
        }
    }

    /// The whole line: [`Self::read_state`] plus the launch names. A name that does not parse
    /// is dropped, so its launch stays counted.
    #[cfg_attr(trust_verify, trust::skip)]
    fn read_record(&self) -> Option<(u64, u32, Vec<LaunchName>)> {
        use std::io::Read as _;
        const MAX_SENTINEL_BYTES: u64 = 4096;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        #[cfg(not(unix))]
        if std::fs::symlink_metadata(&self.path)
            .ok()
            .is_some_and(|m| m.file_type().is_symlink())
        {
            return None;
        }
        let file = opts.open(&self.path).ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() > MAX_SENTINEL_BYTES {
            return None;
        }
        let mut text =
            String::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
        file.take(MAX_SENTINEL_BYTES + 1)
            .read_to_string(&mut text)
            .ok()?;
        if text.len() as u64 > MAX_SENTINEL_BYTES {
            return None;
        }
        let uint = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let mut it = text.split_whitespace();
        let build = it.next().filter(|s| uint(s))?.parse().ok()?;
        let attempts = it.next().unwrap_or("0");
        let attempts = uint(attempts).then(|| attempts.parse().ok()).flatten()?;
        let names = it
            .filter_map(|token| {
                let mut fields = token.split(':');
                let name = LaunchName {
                    pid: fields.next().filter(|s| uint(s))?.parse().ok()?,
                    started: (
                        fields.next().filter(|s| uint(s))?.parse().ok()?,
                        fields.next().filter(|s| uint(s))?.parse().ok()?,
                    ),
                };
                fields.next().is_none().then_some(name)
            })
            .collect();
        Some((build, attempts, names))
    }

    /// Confirm the running build booted healthy by removing the commit marker.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn confirm(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|dir| !dir.as_os_str().is_empty())
            && std::fs::symlink_metadata(parent).is_ok_and(|meta| !meta.file_type().is_dir())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "boot sentinel parent path is not a directory",
            ));
        }
        match std::fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.file_type().is_file() => match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            },
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "boot sentinel path is not a regular file",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Record that this launch found the sentinel unconfirmed (attempts += 1), returning the new count.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn observe_launch(&self, running_build: u64) -> io::Result<u32> {
        self.observe_launch_by(running_build, None)
    }

    /// [`Self::observe_launch`], naming the process that made the launch so exactly it can be given back ([`Self::forgive_launch_of`]); the count and the name land in one write.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn observe_launch_by(
        &self,
        running_build: u64,
        name: Option<LaunchName>,
    ) -> io::Result<u32> {
        match self.read_record() {
            Some((b, attempts, mut names)) if b == running_build => {
                let next = attempts.saturating_add(1);
                names.extend(name);
                self.write_count(b, next, &names, self.flush)?;
                Ok(next)
            }
            Some(_) => Ok(0),
            None => Ok(0),
        }
    }

    /// Un-count one observed launch of `running_build` (floored at 0), returning the new count.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn forgive_launch(&self, running_build: u64) -> io::Result<u32> {
        match self.read_record() {
            Some((b, attempts, names)) if b == running_build => {
                let next = attempts.saturating_sub(1);
                if next != attempts {
                    self.write_count(b, next, &names, self.flush)?;
                }
                Ok(next)
            }
            Some(_) | None => Ok(0),
        }
    }

    /// Give back the one launch of `running_build` that process `pid` made (born at `started` when the caller knows it), if still counted: its name leaves the record and the count drops by one, in one atomic write.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn forgive_launch_of(
        &self,
        running_build: u64,
        pid: u32,
        started: Option<(i64, i64)>,
    ) -> io::Result<Option<u32>> {
        let Some((b, attempts, mut names)) = self.read_record() else {
            return Ok(None);
        };
        if b != running_build {
            return Ok(None);
        }
        let Some(at) = names.iter().position(|name| {
            name.pid == pid && started.is_none_or(|started| started == name.started)
        }) else {
            return Ok(None);
        };
        names.remove(at);
        let next = attempts.saturating_sub(1);
        self.write_count(b, next, &names, Flush::Ordered)?;
        Ok(Some(next))
    }

    /// Whether `running_build` should be reverted: armed and observed unconfirmed at least `max_attempts` times (a crash loop).
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn should_revert(&self, running_build: u64, max_attempts: u32) -> bool {
        matches!(self.read_state(), Some((b, attempts)) if b == running_build && attempts >= max_attempts)
    }

    /// Atomic write of `"<build> <attempts>"` and the newest [`MAX_NAMED_LAUNCHES`] launch
    /// names via a `0600` temp + rename, the temp and the directory finished with `flush`
    /// (a volume that refuses the temp's flush is accepted: [`aterm_durable::refusal_accepted`]).
    /// # Errors
    /// The outer `Err`: the write, the temp's flush or the rename failed, and the record is as
    /// it was. The inner `Err`: the directory's flush after the rename failed other than as
    /// unavailable ([`Flush::dir_best_effort`]), so the record is in place but may not
    /// survive a power loss.
    #[cfg_attr(trust_verify, trust::skip)]
    fn write_record(
        &self,
        build: u64,
        attempts: u32,
        names: &[LaunchName],
        flush: Flush,
    ) -> io::Result<io::Result<()>> {
        use std::io::Write as _;
        let parent = match self.path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        if !std::fs::symlink_metadata(parent)?.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "boot sentinel parent path is not a directory",
            ));
        }
        let tmp = parent.join(format!(".boot-sentinel.tmp-{}", std::process::id()));
        let mut f = {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            }
            opts.open(&tmp)?
        };
        let wrote = (|| -> io::Result<()> {
            let meta = f.metadata()?;
            if !meta.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "boot sentinel temp path is not a regular file",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if meta.permissions().mode() & 0o7777 != 0o600 {
                    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
            }
            write!(f, "{build} {attempts}")?;
            for name in &names[names.len().saturating_sub(MAX_NAMED_LAUNCHES)..] {
                let LaunchName {
                    pid,
                    started: (seconds, micros),
                } = name;
                write!(f, " {pid}:{seconds}:{micros}")?;
            }
            aterm_durable::refusal_accepted(crate::durable::finish(&f, flush))?;
            drop(f);
            match std::fs::symlink_metadata(&self.path) {
                Ok(meta) if !meta.file_type().is_file() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "boot sentinel path is not a regular file",
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(error) = wrote {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        Ok(flush
            .dir_best_effort(parent)
            .map(|_flushed_or_unavailable| ()))
    }

    /// [`Self::write_record`] for a launch count, a give-back or a restarted count. Logged, not
    /// returned: the count is in place once its rename lands, though a power loss may lose
    /// the latest directory entry and counts. An Err would tell the caller the count was
    /// never made (a launch's boot-health check is then deferred).
    #[cfg_attr(trust_verify, trust::skip)]
    fn write_count(
        &self,
        build: u64,
        attempts: u32,
        names: &[LaunchName],
        flush: Flush,
    ) -> io::Result<()> {
        if let Err(error) = self.write_record(build, attempts, names, flush)? {
            aterm_log::warn!(
                "boot sentinel {}: the count is in place, but flushing its folder failed, so a \
                 power loss may take it back: {error}",
                self.path.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "sentinel_tests.rs"]
mod tests;
