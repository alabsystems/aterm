// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `.app` staging layout under `~/Library/Application Support/aterm/Updates/`.

use std::path::PathBuf;

use aterm_update_core::ensure_private_dir;

/// Files earlier updaters kept a verdict in, which the [record](Staging::record) replaced: the
/// crash-loop quarantine ledger, the failed-candidate memo and the window-start failure record
/// (now the record's `start_failure`), with their locks. Nothing reads
/// them, nothing is imported from them (a held build is simply tried again), and
/// [`Staging::retire`] deletes them.
pub const RETIRED: [&str; 6] = [
    "quarantine.toml",
    "quarantine.toml.lock",
    "failed.toml",
    "failed.toml.lock",
    "window-start-failure.toml",
    "window-start-failure.toml.lock",
];

/// Delete the [`RETIRED`] files under `root`, best effort, and nothing else.
pub fn retire_in(root: &std::path::Path) {
    for name in RETIRED {
        match std::fs::remove_file(root.join(name)) {
            Ok(()) => aterm_log::info!("aterm-update: retired the old update verdict file {name}"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => aterm_log::warn!("aterm-update: could not retire {name}: {error}"),
        }
    }
}

/// Layout of the staging area, all under `…/aterm/Updates/`.
#[derive(Clone, Debug)]
pub struct Staging {
    /// The `Updates` root.
    pub root: PathBuf,
    /// flock target guarding the apply critical section.
    pub apply_lock: PathBuf,
    /// flock target serializing the staging critical section across processes.
    pub stage_lock: PathBuf,
    /// Scratch dir for in-progress downloads.
    pub download: PathBuf,
    /// The verified, extracted bundle awaiting application.
    pub staged_app: PathBuf,
    /// The "ready" marker — written last; its presence is the sole ready signal.
    pub ready: PathBuf,
    /// Human-readable status record (last check, outcome, staged build).
    pub status: PathBuf,
}

impl Staging {
    /// Resolve and create (`0700`, ownership-verified) the staging layout.
    pub fn resolve() -> Option<Self> {
        let root = aterm_update_core::seal_guard::updates_root()?;
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

    /// The persisted monotonic recency floor (`floor.toml`).
    pub fn floor(&self) -> PathBuf {
        self.root.join("floor.toml")
    }

    /// The self-healing ledger (`health.toml`).
    pub fn health(&self) -> PathBuf {
        self.root.join("health.toml")
    }

    /// This copy's one update record (`record.json`, [`aterm_update_core::update_record`]): the
    /// trial under way, the one hold and how the last one ended. It is the lab row's local copy,
    /// and no other file here holds a verdict.
    pub fn record(&self) -> PathBuf {
        self.root.join(crate::RECORD)
    }

    /// Delete the [`RETIRED`] files, best effort. They are never read.
    pub fn retire(&self) {
        retire_in(&self.root);
    }

    /// The trialed build's `(build_number, dmg_sha256)` (`trial.toml`).
    pub fn trial(&self) -> PathBuf {
        self.root.join("trial.toml")
    }

    /// Durable receipt for the artifact currently installed by the self-updater.
    pub fn installed_receipt(&self) -> PathBuf {
        self.root.join("installed.toml")
    }

    /// Single-use re-exec nonce (`reexec.stamp`) inside the `0700` `Updates` root.
    pub fn reexec_stamp(&self) -> PathBuf {
        self.root.join("reexec.stamp")
    }

    /// Retire the currently published stage (`ready`, `staged_app`, and the latest tag in
    /// `status.toml`).
    pub fn retire_published(&self) {
        let _ = std::fs::remove_file(&self.ready);
        let _ = std::fs::remove_dir_all(&self.staged_app);
        crate::status::clear_latest_tag(self);
    }

    /// Create an isolated `Staging` rooted in a fresh temporary directory
    /// (cleaned up on drop by [`ScratchStaging`]) without mutating process env.
    #[cfg(test)]
    pub(crate) fn scratch(label: &str) -> ScratchStaging {
        let dir = scratch_dir(label).into_tempdir();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("download")).expect("scratch staging root");
        ScratchStaging {
            staging: Self {
                apply_lock: root.join("apply.lock"),
                stage_lock: root.join("stage.lock"),
                download: root.join("download"),
                staged_app: root.join("staged").join("aterm.app"),
                ready: root.join("ready.toml"),
                status: root.join("status.toml"),
                root,
            },
            _dir: dir,
        }
    }
}

#[cfg(test)]
pub(crate) use crate::{ScratchDir, scratch_dir};

/// [`Staging::scratch`]'s layout, which dereferences to the [`Staging`] and removes
/// its root on drop.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ScratchStaging {
    staging: Staging,
    _dir: aterm_tempfile::TempDir,
}

#[cfg(test)]
impl std::ops::Deref for ScratchStaging {
    type Target = Staging;
    fn deref(&self) -> &Staging {
        &self.staging
    }
}

#[cfg(test)]
impl std::ops::DerefMut for ScratchStaging {
    fn deref_mut(&mut self) -> &mut Staging {
        &mut self.staging
    }
}
