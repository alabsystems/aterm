// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The durable "a live process is reading this bundle's sealed payload"
//! marker, owned by the READER.
//!
//! The self-updater must never swap the app bundle while a toolchain install
//! is extracting gigabytes out of it by path — a mid-extraction swap hands the
//! reader a torn bundle (2026-08-20 round-8 audit). The guard used to be
//! CHOREOGRAPHED by the spawning GUI around its `atpkg seed` child, and that
//! split ownership generated a patch per audit round (rounds 8–12: the
//! boot-apply process that never set the flag, the pid that outlived its
//! writer, the second launch's clobber, the teardown that deleted a live
//! owner's record). Each fix was real; the RACE CLASS was the choreography
//! itself — and a user-run `atpkg seed` in a shell had no guard at all,
//! because there was no GUI present to choreograph one.
//!
//! Moving the marker into the reader closes the class by construction:
//! * the pid recorded is the extractor's OWN — nothing to hand across a
//!   process boundary, nothing to go stale on the writer's exit;
//! * every spawn lane is covered identically (the GUI's 6-hour loop, the
//!   Settings worker, and a bare CLI run alike), because the guard travels
//!   with the extraction instead of with one particular spawner;
//! * writes are serialized by atpkg's own store lock — extraction requires
//!   it, so there is never a second live claimant to clobber. A LIVE foreign
//!   pid found under the held store lock is pid reuse by definition, and is
//!   taken over rather than yielded to.
//!
//! The probe side ([`seal_read_active`]) is unchanged in spirit: a marker
//! whose pid is dead is stale and self-heals; a live one blocks the apply.
//! The marker file name and location are the historical ones, so a
//! mixed-version overlap (old GUI still choreographing around a new atpkg)
//! degrades safely: both sides write the SAME pid — the spawned atpkg's own —
//! and whichever teardown runs second finds nothing left to remove.

use std::path::{Path, PathBuf};

/// The `…/aterm/Updates` root shared with `aterm-update`'s staging layout:
/// `ATERM_UPDATE_ROOT` override first (test/demo isolation), else the
/// HOME-keyed Application Support base. `None` when HOME is unset or the
/// directory cannot be made private.
#[must_use]
pub fn updates_root() -> Option<PathBuf> {
    let base = match std::env::var_os("ATERM_UPDATE_ROOT") {
        Some(root) if !root.is_empty() => PathBuf::from(root),
        _ => {
            let home = std::env::var_os("HOME")?;
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("aterm")
        }
    };
    let root = base.join("Updates");
    crate::ensure_private_dir(&root).ok()?;
    Some(root)
}

/// The marker file under an already-resolved Updates root. Split from
/// [`updates_root`] as a pure function so tests drive scratch roots directly
/// (`std::env::set_var` is unsafe-for-a-reason in edition 2024 — the crate
/// convention is pure functions, not env choreography).
#[must_use]
pub fn marker_under(root: &Path) -> PathBuf {
    root.join("toolchain-install")
}

// Both callers are the macOS seal lanes; off macOS the fn compiles as dead.
#[cfg(target_os = "macos")]
fn marker_path() -> Option<PathBuf> {
    Some(marker_under(&updates_root()?))
}

#[cfg(target_os = "macos")]
fn pid_alive(pid: i32) -> bool {
    // `kill(pid, 0)` asks only "does this process exist and may I signal it".
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// RAII claim on the seal-read marker: holds this process's pid in the
/// marker file for the guard's lifetime, and removes it on drop iff the
/// marker still names us.
///
/// Claim it while holding the store lock, before the first sealed byte is
/// read; a crash between the two leaves a dead-pid marker that
/// [`seal_read_active`] self-heals.
#[cfg(target_os = "macos")]
pub struct SealReadGuard {
    marker: PathBuf,
}

#[cfg(target_os = "macos")]
impl SealReadGuard {
    /// Record this process as the live seal reader. `None` when there is no
    /// resolvable marker location (no HOME, unownable dir) — the extraction
    /// then proceeds unguarded, exactly the pre-existing failure posture.
    #[must_use]
    pub fn claim() -> Option<Self> {
        Self::claim_at(marker_path()?)
    }

    /// [`Self::claim`] on an explicit marker path (the testable form).
    #[must_use]
    pub fn claim_at(marker: PathBuf) -> Option<Self> {
        // The store lock serializes extractions, so a marker naming a LIVE
        // foreign pid here cannot belong to a live extraction — it is pid
        // reuse over a stale record. Taking it over is the correct direction;
        // yielding would leave OUR real extraction unguarded to protect a
        // fiction.
        std::fs::write(&marker, std::process::id().to_string()).ok()?;
        Some(Self { marker })
    }
}

#[cfg(target_os = "macos")]
impl Drop for SealReadGuard {
    fn drop(&mut self) {
        // ONLY OUR OWN RECORD. The slot is shared across time; if something
        // newer claimed it (a takeover after our pid was wrongly judged dead),
        // deleting would strip that live reader's guard.
        if std::fs::read_to_string(&self.marker)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            == Some(std::process::id())
        {
            let _ = std::fs::remove_file(&self.marker);
        }
    }
}

/// Non-macOS: the staging/apply machinery this marker guards is `.app`-only.
#[cfg(not(target_os = "macos"))]
pub struct SealReadGuard {}

#[cfg(not(target_os = "macos"))]
impl SealReadGuard {
    /// No marker to claim off macOS.
    #[must_use]
    pub fn claim() -> Option<Self> {
        None
    }
}

/// Whether a LIVE process is reading this bundle's sealed payload right now.
/// A marker whose pid is gone — the reader was killed — is stale and is
/// removed here rather than blocking updates forever.
#[cfg(target_os = "macos")]
#[must_use]
pub fn seal_read_active() -> bool {
    marker_path().is_some_and(seal_read_active_at)
}

/// [`seal_read_active`] on an explicit marker path (the testable form).
#[cfg(target_os = "macos")]
#[must_use]
pub fn seal_read_active_at(marker: PathBuf) -> bool {
    let Ok(text) = std::fs::read_to_string(&marker) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        let _ = std::fs::remove_file(&marker);
        return false;
    };
    let alive = pid_alive(pid);
    if !alive {
        let _ = std::fs::remove_file(&marker);
    }
    alive
}

/// Non-macOS twin: no marker, never active.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn seal_read_active() -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn scratch_marker(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aterm-seal-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch root");
        marker_under(&dir.join(name))
    }

    #[test]
    fn claim_records_own_pid_and_drop_removes_only_it() {
        let marker = scratch_marker("own");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        {
            let guard = SealReadGuard::claim_at(marker.clone()).expect("claim");
            let recorded = std::fs::read_to_string(&marker).expect("marker written");
            assert_eq!(recorded, std::process::id().to_string());
            assert!(
                seal_read_active_at(marker.clone()),
                "our own live pid reads as active"
            );
            drop(guard);
        }
        assert!(!marker.exists(), "drop removes our own record");
        assert!(!seal_read_active_at(marker.clone()));

        // A FOREIGN record survives our drop: simulate a takeover landing
        // between claim and drop.
        {
            let _guard = SealReadGuard::claim_at(marker.clone()).expect("claim");
            std::fs::write(&marker, "1").expect("foreign overwrite");
        }
        assert_eq!(
            std::fs::read_to_string(&marker).expect("still there"),
            "1",
            "drop must not strip a record that is no longer ours"
        );
        let _ = std::fs::remove_file(&marker);
    }

    #[test]
    fn stale_and_malformed_markers_self_heal() {
        let marker = scratch_marker("stale");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();

        std::fs::write(&marker, "not-a-pid").unwrap();
        assert!(
            !seal_read_active_at(marker.clone()),
            "malformed is not active"
        );
        assert!(!marker.exists(), "malformed self-heals");

        // A pid that cannot be alive (beyond pid_max on every macOS).
        std::fs::write(&marker, "999999999").unwrap();
        assert!(
            !seal_read_active_at(marker.clone()),
            "dead pid is not active"
        );
        assert!(!marker.exists(), "stale self-heals");
    }

    #[test]
    fn a_stale_record_is_taken_over_not_yielded_to() {
        let marker = scratch_marker("takeover");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        // A dead pid's leftover — the store lock guarantees no live extraction
        // owns it, so claim overwrites.
        std::fs::write(&marker, "999999999").unwrap();
        let _guard = SealReadGuard::claim_at(marker.clone()).expect("claim over stale");
        assert_eq!(
            std::fs::read_to_string(&marker).expect("ours now"),
            std::process::id().to_string()
        );
    }
}
