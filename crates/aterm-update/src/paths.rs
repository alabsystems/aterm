// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `.app` staging layout under `~/Library/Application Support/aterm/Updates/`.

use std::path::PathBuf;

use aterm_update_core::ensure_private_dir;

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

    /// The last-failed-candidate memo (`failed.toml`).
    pub fn failed(&self) -> PathBuf {
        self.root.join("failed.toml")
    }

    /// The crash-loop quarantine ledger (`quarantine.toml`).
    pub fn quarantine(&self) -> PathBuf {
        self.root.join("quarantine.toml")
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
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-update-{label}-"))
            .tempdir()
            .expect("scratch staging root");
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
