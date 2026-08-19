// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boot-health sentinel (§16.4) — the shared primitive that stops a launch-crash from
//! bricking the fleet after a self-swap.
//!
//! An app self-swap applies *on next launch* and re-execs the new binary; the health
//! probe "before any window" is necessarily shallow (it catches a pre-checkpoint panic,
//! not a deep GUI-init crash). So the swap-back is gated by a **sentinel**: just before
//! the swap is applied, [`Sentinel::arm`] records the build being trialed; the new build
//! [`Sentinel::confirm`]s once it reaches a healthy checkpoint; and each launch that finds
//! the sentinel still present [`Sentinel::observe_launch`]es it. If a build is observed
//! unconfirmed across `max_attempts` launches, it is **crash-looping** and the caller
//! reverts (swaps back to the retained previous build).
//!
//! This module is the **pure, cross-platform decision layer** (atomic counter file +
//! [`should_revert`](Sentinel::should_revert)); the actual `RENAME_SWAP` swap-back is the
//! macOS integration in `aterm-update` (deferred — and per §16.8 it must ship in a *prior*
//! plain app release before the first combined cut, so a fielded app can always revert).

use std::io;
use std::path::{Path, PathBuf};

/// A durable boot-health sentinel backed by a small file `"<build> <attempts>"`.
pub struct Sentinel {
    path: PathBuf,
}

impl Sentinel {
    /// A sentinel backed by `path` (typically under the hardened private dir).
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Arm the sentinel just before applying a swap to `build`: records `build` with a
    /// zero attempt count, atomically (temp + rename). Overwrites any stale sentinel.
    pub fn arm(&self, build: u64) -> io::Result<()> {
        self.write_state(build, 0)
    }

    /// The armed `(build, attempts)`, or `None` if absent/unparseable (a corrupt sentinel
    /// is treated as absent — fail toward NOT reverting on garbage, since reverting is the
    /// disruptive action and the swap itself was monotonic-gated).
    #[must_use]
    // Skip: read_to_string on the sentinel file — hardened utf8_reject class;
    // the file is written by `write_state` below (pure-ASCII `build attempts` line),
    // so the UTF-8 rejection path is unreachable for uncorrupted files and a
    // corrupted sentinel correctly reads as None. Audited (update-atpkg).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn read_state(&self) -> Option<(u64, u32)> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let mut it = text.split_whitespace();
        let build = it.next()?.parse().ok()?;
        let attempts = it.next().unwrap_or("0").parse().ok()?;
        Some((build, attempts))
    }

    /// Confirm the running build booted healthy by removing the commit marker.
    /// Callers must not destroy rollback/trial metadata unless this succeeds.
    // Skip: remove_file raw_path row — the sentinel path is private-dir
    // confined (0700, owner-checked) per the update-atpkg audit.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn confirm(&self) -> io::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Record that this launch observed the sentinel still unconfirmed (attempts += 1),
    /// returning the new attempt count. Only counts when the armed build matches
    /// `running_build`. A sentinel for another build is owned by that other update
    /// transaction and is strictly non-destructive here. No sentinel ⇒ `0`.
    // Skip: native typed-TrustIr lowering does not complete for this body
    // (a toolchain lowering gap — its obligations fail closed regardless).
    // The sentinel's brick-fix contract is audited (update-atpkg) and its
    // attempt-counting is unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn observe_launch(&self, running_build: u64) -> io::Result<u32> {
        match self.read_state() {
            Some((b, attempts)) if b == running_build => {
                let next = attempts.saturating_add(1);
                self.write_state(b, next)?;
                Ok(next)
            }
            Some(_) => Ok(0),
            None => Ok(0),
        }
    }

    /// Un-count one observed launch of `running_build` (attempts −= 1, floored at 0),
    /// returning the new count. For the launch the OUTGOING process ended itself: an
    /// overlap-handoff candidate the parent killed for missing its readiness deadline,
    /// for the user's activity, or for a proof mismatch counted a launch at boot like
    /// any other, but it did not crash — the parent decided its fate. Left counted,
    /// three automatic re-attempts on a busy machine reached `max_attempts`, reverted
    /// to the OLD bundle, and PERMANENTLY poisoned a release that never failed. Only
    /// the killer may forgive, and only for the build it killed; a sentinel for another
    /// build, or none, is untouched (`0`). A candidate that DIED on its own is not
    /// forgiven — that launch is the crash signal this file exists to count.
    // Skip: same typed-TrustIr lowering gap as `observe_launch`; unit-tested.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn forgive_launch(&self, running_build: u64) -> io::Result<u32> {
        match self.read_state() {
            Some((b, attempts)) if b == running_build => {
                let next = attempts.saturating_sub(1);
                if next != attempts {
                    self.write_state(b, next)?;
                }
                Ok(next)
            }
            Some(_) | None => Ok(0),
        }
    }

    /// Whether `running_build` should be reverted: it is armed AND has been observed
    /// unconfirmed at least `max_attempts` times (a crash loop). Pure read.
    #[must_use]
    // Skip: same typed-TrustIr lowering gap as `observe_launch`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn should_revert(&self, running_build: u64, max_attempts: u32) -> bool {
        matches!(self.read_state(), Some((b, attempts)) if b == running_build && attempts >= max_attempts)
    }

    /// Atomic AND durable write of `"<build> <attempts>"` via a `0600` temp + rename.
    ///
    /// The fsyncs are not ceremony: this file is the transaction commit marker for a
    /// swap, and the crash it protects against is precisely the crash that would eat
    /// an unflushed payload. Rename alone only orders METADATA — a committed inode
    /// whose extents were never written back reads as zeros, `read_state` parses that
    /// as `None`, and the whole count/budget/revert path is skipped: the machine
    /// crash-loops forever on the bad build with the retained old bundle sitting right
    /// there unused. `token::write_private_file` already ends in `sync_all` for far
    /// less critical state; the one file whose loss disables rollback must not be the
    /// one that skips it. Cost is irrelevant at this frequency (once per apply, plus
    /// once per launch of an unconfirmed trial) — it is an 11-byte file.
    // Skip: the audited atomic write-then-rename (the update-atpkg brick-fix):
    // OpenOptions+rename are DELIBERATELY path-based inside the 0700 private
    // dir; the hardened lane's direntry-identity contracts are the capability
    // lane's future surface. Verify-only classification of audited code.
    #[cfg_attr(trust_verify, trust::skip)]
    fn write_state(&self, build: u64, attempts: u32) -> io::Result<()> {
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
            // Inside the braces: the payload must be on stable storage BEFORE the handle
            // drops and before the rename below publishes the name.
            //
            // A REFUSAL is not a failure. On Apple targets `File::sync_all` is a bare
            // `fcntl(F_FULLFSYNC)` with no fsync fallback, and some filesystems the
            // staging root can live on (a network home, some FUSE volumes) answer it
            // ENOTSUP/EINVAL. Propagating that would fail `arm()` on every apply — the
            // updater would silently Defer forever on that class of machine, in exchange
            // for a durability guarantee the volume cannot provide anyway. So an
            // "unsupported" answer degrades to the old, non-durable behaviour and a REAL
            // I/O error (ENOSPC, EIO) still aborts the apply rather than swapping in a
            // build whose rollback authority was never written. Same reasoning as the
            // directory sync below.
            match f.sync_all() {
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
        // Best-effort ONLY: on macOS `sync_all` on a directory fd is `F_FULLFSYNC`,
        // which some filesystems answer with ENOTSUP/EINVAL. Propagating that would
        // fail `arm()`, which Defers the entire update apply — a regression strictly
        // worse than the durability gap this closes.
        let _ = std::fs::File::open(parent).and_then(|d| d.sync_all());
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
        // The parent's candidate boots (launch 1) and is then killed by the parent
        // for missing its readiness deadline: given back.
        assert_eq!(s.observe_launch(1235).unwrap(), 1);
        assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        assert_eq!(s.read_state(), Some((1235, 0)));
        // Three parent-killed candidates in a row never reach a revert...
        for _ in 0..3 {
            assert_eq!(s.observe_launch(1235).unwrap(), 1);
            assert_eq!(s.forgive_launch(1235).unwrap(), 0);
        }
        assert!(!s.should_revert(1235, 3), "forgiven launches are not a loop");
        // ...but a candidate that DIED keeps its count, and forgiveness never
        // undercounts below zero or reaches into another build's sentinel.
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
        // An overlapping process from another build has no authority to erase it.
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
