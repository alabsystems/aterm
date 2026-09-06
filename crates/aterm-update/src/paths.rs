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
        // The base derivation — `ATERM_UPDATE_ROOT` test/demo override first
        // (2026-08-15: without it every `cargo test -p aterm-gui` run drove
        // the REAL per-user ledgers), else the HOME-keyed Application Support
        // base — lives in `aterm_update_core::seal_guard::updates_root` so
        // the seal-read marker atpkg writes and the staging layout this
        // struct describes can never disagree about where `Updates/` is.
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

    /// Where curl dumps the TOKEN-LANE release listing's RESPONSE HEADERS
    /// (`list.headers`), so the `x-ratelimit-*` block can be read back and a rate-limited
    /// check can hold until the server's own reset.
    ///
    /// A file, not `-D -`: the body is captured from curl's stdout with the status
    /// trailer appended to it, so headers on the same stream would corrupt both. It is
    /// overwritten by every request and read immediately; nothing durable lives here.
    /// The web lane writes nothing here — its one HEAD carries its answer on stdout.
    pub fn list_headers(&self) -> PathBuf {
        self.root.join("list.headers")
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
    ///
    /// Also forgets the ledger's authorized tag ([`crate::status::clear_latest_tag`]):
    /// every caller retires "so the next check re-stages", and on the web lane a check
    /// whose pointer still names the retired stage's tag would otherwise stop at its
    /// HEAD with "up to date" forever — the machine stranded on the old build until the
    /// publisher cut a NEW tag.
    pub fn retire_published(&self) {
        let _ = std::fs::remove_file(&self.ready);
        let _ = std::fs::remove_dir_all(&self.staged_app);
        crate::status::clear_latest_tag(self);
    }

    /// A `Staging` rooted at a fresh, unique temp dir with `download/` created — the
    /// one scratch layout every test module in this crate builds. Field-by-field
    /// rather than through [`Self::resolve`] + `$ATERM_UPDATE_ROOT`, because
    /// `std::env::set_var` is `unsafe` in edition 2024 and a data race under a
    /// multi-threaded test runner — and because a test must never be able to touch
    /// the real per-user ledgers. The caller removes `root` when done.
    #[cfg(test)]
    pub(crate) fn scratch(label: &str) -> Self {
        static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aterm-update-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("download")).expect("scratch staging root");
        Self {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged").join("aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root,
        }
    }
}
