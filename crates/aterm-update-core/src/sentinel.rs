// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boot-health sentinel: stops a launch crash from bricking the fleet after a self-swap.
//!
//! A self-swap is applied at the top of a launch and re-execs the new binary, and the health
//! probe before any window is shallow (it catches a pre-checkpoint panic, not a deep GUI-init
//! crash). So just before the swap [`Sentinel::arm`] records the build on trial, the new build
//! [`Sentinel::confirm`]s at a healthy checkpoint, and each launch that finds the sentinel
//! still present [`Sentinel::observe_launch`]es it. A build observed unconfirmed across
//! `max_attempts` launches is crash-looping and the caller swaps back to the retained build.
//!
//! This is the cross-platform decision layer; the `RENAME_SWAP` swap-back is in
//! `aterm-update`.

use std::io;
use std::path::{Path, PathBuf};

/// The process that made one counted launch: its pid and the kernel's start time
/// for it (seconds, microseconds). A reused pid never carries the start time of
/// the process it replaced, so the pair names exactly one launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchName {
    pub pid: u32,
    pub started: (i64, i64),
}

/// The most launch names the sentinel keeps; the oldest go first. Every build that
/// reaches the revert threshold counts only a few, so this only bounds the file.
const MAX_NAMED_LAUNCHES: usize = 32;

/// A durable boot-health sentinel backed by a small file
/// `"<build> <attempts>[ <pid>:<seconds>:<micros>]*"`.
///
/// Several aterm windows can hand off to the same trialed build, so launches of one trial
/// overlap, and a give-back measured as "the count moved since my snapshot" could erase a
/// sibling's launch (the crash signal). So each counted launch is named by its process and
/// [`Self::forgive_launch_of`] gives back exactly that launch, once. The names follow the
/// count on the same line: readers of the first two fields ignore them, and a writer that
/// drops them leaves those launches counted, which keeps the crash signal.
pub struct Sentinel {
    path: PathBuf,
}

impl Sentinel {
    /// A sentinel backed by `path` (typically under the hardened private dir).
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Arm the sentinel just before applying a swap to `build`: records `build` with zero
    /// attempts, overwriting any stale sentinel.
    pub fn arm(&self, build: u64) -> io::Result<()> {
        self.write_record(build, 0, &[])
    }

    /// The armed `(build, attempts)`, or `None` if absent or unparseable: a corrupt sentinel
    /// fails toward not reverting, the disruptive action.
    #[must_use]
    // Skip: read_to_string; `write_record` writes ASCII, so only a corrupt file fails UTF-8,
    // and it reads as None.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn read_state(&self) -> Option<(u64, u32)> {
        self.read_record()
            .map(|(build, attempts, _)| (build, attempts))
    }

    /// The named launches of `running_build` still on the count (see
    /// [`Self::forgive_launch_of`]); empty for another build or none.
    #[must_use]
    pub fn named_launches(&self, running_build: u64) -> Vec<LaunchName> {
        match self.read_record() {
            Some((build, _, names)) if build == running_build => names,
            _ => Vec::new(),
        }
    }

    /// The whole line: [`Self::read_state`] plus the launch names. A name that does not parse
    /// is dropped, so its launch stays counted.
    // Skip: as `read_state`.
    #[cfg_attr(trust_verify, trust::skip)]
    fn read_record(&self) -> Option<(u64, u32, Vec<LaunchName>)> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let mut it = text.split_whitespace();
        let build = it.next()?.parse().ok()?;
        let attempts = it.next().unwrap_or("0").parse().ok()?;
        let names = it
            .filter_map(|token| {
                let mut fields = token.split(':');
                let name = LaunchName {
                    pid: fields.next()?.parse().ok()?,
                    started: (fields.next()?.parse().ok()?, fields.next()?.parse().ok()?),
                };
                fields.next().is_none().then_some(name)
            })
            .collect();
        Some((build, attempts, names))
    }

    /// Confirm the running build booted healthy by removing the commit marker. Callers must
    /// not destroy rollback/trial metadata unless this succeeds.
    // Skip: remove_file on a path confined to the 0700 private dir.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn confirm(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Record that this launch found the sentinel unconfirmed (attempts += 1), returning the
    /// new count. Only counts when the armed build is `running_build`; another build's
    /// sentinel belongs to its own transaction and is left alone. No sentinel ⇒ `0`.
    // Skip: native typed-TrustIr lowering does not complete for this body; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn observe_launch(&self, running_build: u64) -> io::Result<u32> {
        self.observe_launch_by(running_build, None)
    }

    /// [`Self::observe_launch`], naming the process that made the launch so exactly it can be
    /// given back ([`Self::forgive_launch_of`]); the count and the name land in one write.
    // Skip: as `observe_launch`.
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
                self.write_record(b, next, &names)?;
                Ok(next)
            }
            Some(_) => Ok(0),
            None => Ok(0),
        }
    }

    /// Un-count one observed launch of `running_build` (floored at 0), returning the new
    /// count. For a launch the outgoing process ended itself (an overlap-handoff candidate
    /// killed for a missed readiness deadline, user activity or a proof mismatch): it did not
    /// crash, and left counted, a few re-attempts on a busy machine would revert and poison a
    /// release that never failed. Only the killer forgives, only for the build it killed; a
    /// candidate that died on its own stays counted. Unnamed, so the names stay as they were;
    /// [`Self::forgive_launch_of`] gives back a named launch.
    // Skip: as `observe_launch`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn forgive_launch(&self, running_build: u64) -> io::Result<u32> {
        match self.read_record() {
            Some((b, attempts, names)) if b == running_build => {
                let next = attempts.saturating_sub(1);
                if next != attempts {
                    self.write_record(b, next, &names)?;
                }
                Ok(next)
            }
            Some(_) | None => Ok(0),
        }
    }

    /// Give back the one launch of `running_build` that process `pid` made (born at `started`
    /// when the caller knows it), if still counted: its name leaves the record and the count
    /// drops by one, in one atomic write. `None` when none of that process's launches is
    /// counted, so a repeated ask is a no-op and no ask can take another process's launch.
    // Skip: as `observe_launch`.
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
        self.write_record(b, next, &names)?;
        Ok(Some(next))
    }

    /// Whether `running_build` should be reverted: armed and observed unconfirmed at least
    /// `max_attempts` times (a crash loop).
    #[must_use]
    // Skip: as `observe_launch`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn should_revert(&self, running_build: u64, max_attempts: u32) -> bool {
        matches!(self.read_state(), Some((b, attempts)) if b == running_build && attempts >= max_attempts)
    }

    /// Atomic and durable write of `"<build> <attempts>"` and the newest
    /// [`MAX_NAMED_LAUNCHES`] launch names via a `0600` temp + rename.
    ///
    /// This file is the swap's commit marker, and the crash it guards against is the one that
    /// would eat an unflushed payload: rename orders only metadata, an unwritten inode reads as
    /// zeros, `read_state` parses that as `None`, and the machine crash-loops on the bad build
    /// with the retained old bundle unused. Hence the fsyncs.
    // Skip: the path-based write-then-rename inside the 0700 private dir.
    #[cfg_attr(trust_verify, trust::skip)]
    fn write_record(&self, build: u64, attempts: u32, names: &[LaunchName]) -> io::Result<()> {
        use std::io::Write as _;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = parent.join(format!(".boot-sentinel.tmp-{}", std::process::id()));
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            write!(f, "{build} {attempts}")?;
            for name in &names[names.len().saturating_sub(MAX_NAMED_LAUNCHES)..] {
                let LaunchName {
                    pid,
                    started: (seconds, micros),
                } = name;
                write!(f, " {pid}:{seconds}:{micros}")?;
            }
            // Before the handle drops and the rename publishes the name. A refusal is not a
            // failure: on Apple targets `sync_all` is a bare `F_FULLFSYNC`, which a network
            // home or some FUSE volumes answer ENOTSUP/EINVAL, and failing `arm()` there would
            // defer every apply for a guarantee the volume cannot give. A real I/O error
            // (ENOSPC, EIO) still aborts the apply.
            match crate::durable::sync_all(&f) {
                Ok(()) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        std::fs::rename(&tmp, &self.path)?;
        // Best effort: a directory `F_FULLFSYNC` can be refused the same way, and failing
        // `arm()` would defer the whole apply.
        let _ = crate::durable::sync_dir(parent);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentinel(label: &str) -> (Sentinel, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("aterm-sentinel-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Sentinel::new(dir.join("boot.sentinel")), dir)
    }

    #[test]
    fn arm_read_confirm_round_trip() {
        let (s, dir) = sentinel("rt");
        assert_eq!(s.read_state(), None);
        s.arm(1235).unwrap();
        assert_eq!(s.read_state(), Some((1235, 0)));
        s.confirm().unwrap();
        assert_eq!(s.read_state(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_boot_never_reverts() {
        let (s, dir) = sentinel("healthy");
        s.arm(1235).unwrap();
        // First boot of the new build observes the sentinel once...
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        // ...then reaches the healthy checkpoint and confirms.
        s.confirm().unwrap();
        assert!(!s.should_revert(1235, 2), "a confirmed boot never reverts");
        assert_eq!(s.read_state(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_loop_triggers_revert_at_max_attempts() {
        let (s, dir) = sentinel("crash");
        s.arm(1235).unwrap();
        // The new build crashes before confirming, twice.
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert!(
            !s.should_revert(1235, 2),
            "one unconfirmed boot is not yet a loop"
        );
        assert_eq!(s.observe_launch(1235).unwrap(), 2);
        assert!(s.should_revert(1235, 2), "two unconfirmed boots ⇒ revert");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_launch_the_killer_forgives_is_not_a_crash() {
        let (s, dir) = sentinel("forgive");
        s.arm(1235).unwrap();
        // A candidate the parent killed (a missed readiness deadline) is given back.
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        assert_eq!(s.read_state(), Some((1235, 0)));
        // Three parent-killed candidates in a row never reach a revert...
        for _ in 0..3 {
            assert_eq!(s.observe_launch(1235).unwrap(), 1);
            assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        }
        assert!(
            !s.should_revert(1235, 3),
            "forgiven launches are not a loop"
        );
        // ...forgiveness floors at zero and never reaches another build's sentinel.
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        assert_eq!(s.forgive_launch(1235).unwrap(), 0, "floored at zero");
        assert_eq!(s.forgive_launch(9999).unwrap(), 0);
        assert_eq!(s.read_state(), Some((1235, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sentinel_for_another_build_is_preserved_and_not_counted() {
        let (s, dir) = sentinel("stale");
        s.arm(1000).unwrap();
        // A process of another build has no authority to erase it.
        assert_eq!(s.observe_launch(2000).unwrap(), 0);
        assert_eq!(s.read_state(), Some((1000, 0)));
        assert!(!s.should_revert(2000, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_sentinel_does_not_force_a_revert() {
        let (s, dir) = sentinel("corrupt");
        std::fs::write(&s.path, "not a sentinel").unwrap();
        assert_eq!(s.read_state(), None);
        assert!(
            !s.should_revert(1235, 1),
            "garbage never forces the disruptive revert"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
