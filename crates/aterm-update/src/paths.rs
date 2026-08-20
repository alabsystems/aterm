// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Staging directory resolution under `~/Library/Application Support/aterm`.
//!
//! The `Updates/` layout (and its `download/`, `staged/aterm.app`, lock + marker
//! files) is `.app`-specific, so it stays here. The private-dir creation itself is
//! delegated to [`aterm_update_core::ensure_private_dir`], which reuses the same
//! ownership predicate as `aterm-gui`'s `control_auth` so the two cannot drift on
//! what "private" means: owned by us, mode `0700`, never group/other-writable.

use std::path::PathBuf;

use aterm_update_core::ensure_private_dir;

/// Layout of the staging area, all under `…/aterm/Updates/`.
#[derive(Clone, Debug)]
pub struct Staging {
    /// The `Updates` root.
    pub root: PathBuf,
    /// flock target guarding the apply critical section.
    pub apply_lock: PathBuf,
    /// flock target serializing the staging critical section (download + extract +
    /// publish) across processes. Distinct from `apply_lock` so a long download
    /// never blocks a starting instance's apply path.
    pub stage_lock: PathBuf,
    /// Scratch dir for in-progress downloads.
    pub download: PathBuf,
    /// The verified, extracted bundle awaiting application.
    pub staged_app: PathBuf,
    /// The "ready" marker — written last; its presence is the sole ready signal.
    pub ready: PathBuf,
    /// Human/operator-readable status record (last check, outcome, staged build).
    /// Observability surface for a silent updater — `cat` it to see what happened.
    pub status: PathBuf,
}

impl Staging {
    /// Resolve (and create, `0700`, ownership-verified) the staging layout.
    /// Returns `None` if `HOME` is unset or the directory cannot be made private.
    pub fn resolve() -> Option<Self> {
        // TEST/DEMO ISOLATION (2026-08-15): `ATERM_UPDATE_ROOT` overrides the
        // HOME-keyed base wholesale. Without it, every `cargo test -p
        // aterm-gui` run drove the REAL per-user ledgers through the real
        // recorders — ~2,125 of the 2,191 "apply failures" on this machine's
        // health ledger were the unit suite's fixture strings (current_build
        // 10, "handoff proof ended TimedOut"), laundered into `update status`
        // as a persistent streak on a healthy, up-to-date install. The GUI
        // test harness pins this to a per-process scratch dir; a demo or
        // rehearsal shell may point it anywhere.
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
        ensure_private_dir(&root).ok()?;
        let download = root.join("download");
        ensure_private_dir(&download).ok()?;
        Some(Self {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download,
            staged_app: root.join("staged").join("aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root,
        })
    }

    /// The `staged/` parent of [`Self::staged_app`].
    pub fn staged_dir(&self) -> PathBuf {
        self.root.join("staged")
    }

    /// The persisted monotonic recency floor (`floor.toml`); see `manifest::Floor`.
    pub fn floor(&self) -> PathBuf {
        self.root.join("floor.toml")
    }

    /// The self-healing ledger (`health.toml`): consecutive-failure streak + class,
    /// rescue-path history. See [`crate::health::Health`].
    pub fn health(&self) -> PathBuf {
        self.root.join("health.toml")
    }

    /// The last-failed-candidate memo (`failed.toml`); see `manifest::FailedMark`.
    pub fn failed(&self) -> PathBuf {
        self.root.join("failed.toml")
    }

    /// The "a toolchain install is reading this bundle" marker, holding the pid of
    /// the process whose child is extracting.
    ///
    /// DURABLE ON PURPOSE. A process-local flag cannot work here: the apply that
    /// swaps the bundle runs at the top of the SUCCESSOR image's boot, in a process
    /// that never set anything, so an in-memory atomic is always false exactly when
    /// it is read (2026-08-20 round-9 audit).
    pub fn toolchain_install(&self) -> PathBuf {
        self.root.join("toolchain-install")
    }

    /// The trialed build's `(build_number, dmg_sha256)` (`trial.toml`), written beside
    /// the boot sentinel at apply time so a LATER crash-loop revert — which no longer
    /// holds the ready marker — can poison exactly the build that crash-looped, so it
    /// isn't re-downloaded + re-applied into another loop (C1). Reuses the
    /// `manifest::FailedMark` (build+sha) record shape.
    pub fn trial(&self) -> PathBuf {
        self.root.join("trial.toml")
    }

    /// Durable receipt for the artifact currently installed by the self-updater.
    /// Unlike [`Self::trial`], this survives healthy-boot confirmation so an
    /// overlapping old process can still prove the exact completed swap.
    pub fn installed_receipt(&self) -> PathBuf {
        self.root.join("installed.toml")
    }

    /// The single-use re-exec nonce stamp (`reexec.stamp`), written just before the
    /// apply re-exec and validated (then deleted) by the post-swap guard. Lives in the
    /// `0700` `Updates` root so only we can create/read it — the spoof-resistant
    /// replacement for trusting a bare inherited `ATERM_UPDATE_REEXEC` env var (F9).
    pub fn reexec_stamp(&self) -> PathBuf {
        self.root.join("reexec.stamp")
    }

    /// Retire only the currently published stage. The caller must hold
    /// [`Self::apply_lock`], which is also acquired for the stager's short final
    /// publication transaction. Deliberately do not touch `download/` or an
    /// unpublished incoming bundle: those belong to a possibly in-flight producer
    /// holding [`Self::stage_lock`].
    pub fn retire_published(&self) {
        let _ = std::fs::remove_file(&self.ready);
        let _ = std::fs::remove_dir_all(&self.staged_app);
    }
}
