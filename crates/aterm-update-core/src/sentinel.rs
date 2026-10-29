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
        // Required: the swap an arm precedes must not reach the media ahead of it, so a failed
        // flush of the folder is the arm's error, and its caller does not swap. Try to remove
        // the record: an armed sentinel whose caller does not swap would count the
        // running build's launches against a trial nothing started.
        let Err(unflushed) = self.write_record(build, 0, &[], flush)? else {
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

    /// Confirm the running build booted healthy by removing the commit marker.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn confirm(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
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
            aterm_durable::refusal_accepted(crate::durable::finish(&f, flush))?;
        }
        std::fs::rename(&tmp, &self.path)?;
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
mod tests {
    use super::*;

    fn sentinel(label: &str) -> (Sentinel, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("aterm-sentinel-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (Sentinel::new(dir.join("boot.sentinel")), dir)
    }

    /// Which flush each sentinel write takes (aterm-durable's counting seam): the boot
    /// sentinel's arm and launch counts stay durable unless the caller orders the arm; a
    /// named give-back is only ordered; an ordered counter orders every write.
    #[test]
    fn each_sentinel_write_names_its_flush() {
        use aterm_durable::seam::Recording;
        const O: Flush = Flush::Ordered;
        const D: Flush = Flush::Durable;
        let (s, dir) = sentinel("flush-tiers");
        let me = LaunchName {
            pid: 7,
            started: (1, 2),
        };
        let writes = |flush: Flush, n: usize| vec![flush; n * 2];
        let recording = Recording::start();
        s.arm(9).unwrap();
        assert_eq!(
            recording.take(),
            writes(D, 1),
            "a durable arm: the file, the entry"
        );
        s.arm_with(9, O).unwrap();
        assert_eq!(recording.take(), writes(O, 1), "an ordered arm");
        assert_eq!(s.observe_launch_by(9, Some(me)).unwrap(), 1);
        assert_eq!(recording.take(), writes(D, 1), "a launch count");
        assert_eq!(s.forgive_launch_of(9, 7, Some((1, 2))).unwrap(), Some(0));
        assert_eq!(recording.take(), writes(O, 1), "a named give-back");
        let counter = Sentinel::ordered(dir.join("foreign-trial"));
        counter.restart_count(9).unwrap();
        assert_eq!(counter.observe_launch(9).unwrap(), 1);
        assert_eq!(recording.take(), writes(O, 2), "an ordered counter");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn healthy_boot_never_reverts() {
        let (s, dir) = sentinel("healthy");
        s.arm(1235).unwrap();
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        s.confirm().unwrap();
        assert!(!s.should_revert(1235, 2), "a confirmed boot never reverts");
        assert_eq!(s.read_state(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_loop_triggers_revert_at_max_attempts() {
        let (s, dir) = sentinel("crash");
        s.arm(1235).unwrap();
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
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        assert_eq!(s.read_state(), Some((1235, 0)));
        for _ in 0..3 {
            assert_eq!(s.observe_launch(1235).unwrap(), 1);
            assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        }
        assert!(
            !s.should_revert(1235, 3),
            "forgiven launches are not a loop"
        );
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert_eq!(s.observe_launch(1235).unwrap(), 2);
        assert_eq!(
            s.forgive_launch(1235).unwrap(),
            1,
            "an unforgiven died candidate keeps its count"
        );
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
        assert_eq!(s.observe_launch(2000).unwrap(), 0);
        assert_eq!(s.read_state(), Some((1000, 0)));
        assert!(!s.should_revert(2000, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed flush of the sentinel's folder fails the arm (the swap it precedes does not
    /// happen) and takes its record back, a stale one it replaced with it, so a failed arm
    /// leaves no build armed for a trial its caller never starts; it fails no count, give-back
    /// or restarted count, which are in place once renamed. A folder the volume cannot flush
    /// fails nothing. With the record left in place, as the arm's first policy did, a failed
    /// arm left the build armed.
    #[test]
    fn a_failed_folder_flush_fails_the_arm_and_not_a_count() {
        use aterm_durable::seam::{EIO, ENOTSUP, FailingDirs};
        let (s, dir) = sentinel("dir-flush-fails");
        s.arm(8).unwrap();
        {
            let _eio = FailingDirs::with(EIO);
            assert_eq!(
                s.arm_with(9, Flush::Ordered).map_err(|e| e.raw_os_error()),
                Err(Some(EIO)),
                "an arm not ordered ahead of the swap is no arm"
            );
            assert_eq!(s.read_state(), None, "the failed arm is taken back");
            assert!(!s.path.exists(), "and leaves no file behind");
            assert_eq!(s.arm(9).map_err(|e| e.raw_os_error()), Err(Some(EIO)));
            assert_eq!(s.read_state(), None);
        }
        s.arm(9).unwrap();
        {
            let _eio = FailingDirs::with(EIO);
            assert_eq!(s.observe_launch(9).unwrap(), 1, "the count is in place");
            assert_eq!(s.forgive_launch(9).unwrap(), 0, "and so is its give-back");
            assert_eq!(s.read_state(), Some((9, 0)));
            let me = LaunchName {
                pid: 7,
                started: (1, 2),
            };
            assert_eq!(s.observe_launch_by(9, Some(me)).unwrap(), 1);
            assert_eq!(
                s.forgive_launch_of(9, me.pid, Some(me.started)).unwrap(),
                Some(0)
            );
            assert!(s.named_launches(9).is_empty());
            let counter = Sentinel::ordered(dir.join("foreign-trial"));
            counter
                .restart_count(9)
                .expect("a restarted count is in place too");
            assert_eq!(counter.read_state(), Some((9, 0)));
        }
        {
            let _refused = FailingDirs::with(ENOTSUP);
            s.arm(10)
                .expect("a folder the volume cannot flush is no failure");
        }
        assert_eq!(s.read_state(), Some((10, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_prelanding_arm_failure_preserves_the_previous_record() {
        let (s, dir) = sentinel("arm-temp-obstructed");
        s.arm(8).unwrap();
        let tmp = dir.join(format!(".boot-sentinel.tmp-{}", std::process::id()));
        std::fs::create_dir(&tmp).unwrap();
        assert!(s.arm(9).is_err());
        assert_eq!(s.read_state(), Some((8, 0)));
        assert!(tmp.is_dir());
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

    #[test]
    fn only_a_present_marker_that_does_not_read_is_unreadable() {
        let (s, dir) = sentinel("unreadable");
        assert!(
            !s.unreadable(),
            "no marker is no trial, not an unreadable one"
        );
        s.arm(1235).unwrap();
        assert!(!s.unreadable(), "an armed marker reads");
        std::fs::write(&s.path, [0u8; 16]).unwrap();
        assert!(
            s.unreadable(),
            "a zero-filled marker (a cut write) does not"
        );
        s.confirm().unwrap();
        assert!(!s.unreadable(), "a confirmed marker is gone");
        std::fs::create_dir(&s.path).unwrap();
        assert!(
            !s.unreadable(),
            "a directory at the path is no marker that does not read"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_launches_and_forgive_launch_of_match_pid_start_time_and_cap_at_32() {
        let (s, dir) = sentinel("named-launches");
        assert!(s.named_launches(42).is_empty());
        assert_eq!(s.forgive_launch_of(42, 10, None).unwrap(), None);

        s.arm(42).unwrap();
        let first = LaunchName {
            pid: 100,
            started: (10, 20),
        };
        let reused_pid = LaunchName {
            pid: 100,
            started: (30, 40),
        };
        assert_eq!(s.observe_launch_by(42, Some(first)).unwrap(), 1);
        assert_eq!(s.observe_launch_by(42, Some(reused_pid)).unwrap(), 2);
        assert_eq!(s.named_launches(42), vec![first, reused_pid]);
        assert!(s.named_launches(99).is_empty(), "other build sees none");
        assert_eq!(s.forgive_launch_of(99, 100, None).unwrap(), None);
        assert_eq!(
            s.forgive_launch_of(42, 100, Some((99, 99))).unwrap(),
            None,
            "wrong start time leaves count and names untouched"
        );
        assert_eq!(s.read_state(), Some((42, 2)));
        assert_eq!(
            s.forgive_launch_of(42, 100, Some((30, 40))).unwrap(),
            Some(1),
            "exact (pid, started) removes the matching entry"
        );
        assert_eq!(s.named_launches(42), vec![first]);
        assert_eq!(
            s.forgive_launch_of(42, 100, None).unwrap(),
            Some(0),
            "started = None matches remaining pid entry"
        );
        assert!(s.named_launches(42).is_empty());

        // Malformed name tokens are ignored without corrupting the count or valid names.
        std::fs::write(&s.path, "42 3 7:1:2 bad 8:9:10:extra 11:12:13").unwrap();
        assert_eq!(s.read_state(), Some((42, 3)));
        assert_eq!(
            s.named_launches(42),
            vec![
                LaunchName {
                    pid: 7,
                    started: (1, 2),
                },
                LaunchName {
                    pid: 11,
                    started: (12, 13),
                },
            ]
        );

        // Exceeding MAX_NAMED_LAUNCHES (32) evicts the oldest launch names first.
        s.arm(42).unwrap();
        for idx in 0..35u32 {
            let name = LaunchName {
                pid: 1000 + idx,
                started: (i64::from(idx), 0),
            };
            assert_eq!(s.observe_launch_by(42, Some(name)).unwrap(), idx + 1);
        }
        let kept = s.named_launches(42);
        assert_eq!(kept.len(), MAX_NAMED_LAUNCHES);
        assert_eq!(kept.first().map(|n| n.pid), Some(1003));
        assert_eq!(kept.last().map(|n| n.pid), Some(1034));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
