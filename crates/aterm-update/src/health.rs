// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The updater's SELF-HEALING ledger (`…/aterm/Updates/health.toml`): a durable record
//! of check failures by CLASS — the memory that lets a silent background updater
//! notice it is persistently broken and say so.
//!
//! Build 826 shipped with a broken download pipeline and reported every failure as
//! "transient": nothing distinguished "the network blipped" from "this binary can
//! never download again", so a permanently stranded client looked healthy. The ledger
//! closes that hole with PER-CLASS failure streaks:
//!
//! * `network` — the releases LIST itself failed: GitHub unreachable / auth broken /
//!   unparseable. Genuinely transient things land here.
//! * `pipeline` — release metadata was visible but an asset that provably exists
//!   could not be fetched. The network is fine, so the download machinery itself is
//!   suspect — the build-826 signature. A `pipeline` streak of [`PERSISTENT_AFTER`]
//!   is surfaced loudly ([`Health::is_persistent`] drives the status wording and a
//!   user notification).
//! * `manifest` — a manifest was FETCHED but rejected (bad signature / unparseable):
//!   a release-side or under-attack state, visible in status but not the pipeline's
//!   fault.
//! * `stage` — the artifact downloaded but failed verification/staging (also
//!   memoized by `FailedMark`).
//!
//! The streaks are PER-CLASS (each cleared only by a fully healthy check), so an
//! interleaved network blip cannot reset a pipeline streak and suppress the
//! escalation. (An on-disk ledger from a pre-v0.26 build may carry the retired
//! rescue-path counters; unknown keys parse fine and are dropped on the next write.)
//!
//! # SCOPE — this ledger covers the CHECK/DOWNLOAD/STAGE lane ONLY
//!
//! Every class above is a failure to *acquire and stage* an update. **Nothing here
//! observes whether a staged update is ever successfully APPLIED.** That gap is not
//! hypothetical: through 2026-07 the owner's machine carried an all-zero
//! `health.toml` — a perfect score — while the seamless apply/handoff lane failed
//! 100% of the time across three releases and every update had to wait for a cold
//! launch. Downloading and staging really were healthy; the ledger was telling the
//! truth about the only thing it measures, and the dashboard still read green on a
//! half-broken updater.
//!
//! So do NOT read "health is clean" as "the updater works". If you extend this
//! ledger, an apply/handoff failure class is the missing one.

use aterm_update_core::FileLock;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Consecutive `pipeline` failures at which the state is called PERSISTENT: the
/// status wording stops saying "deferred" and the GUI raises a notification.
/// (Re-exported from the crate root so cross-platform status consumers share the
/// one threshold.)
///
/// This 3 was chosen against a 6h check interval, where it meant "≈18h — long
/// enough to skip a flaky day". **That interval is retired.** The cadence is now
/// 75s (`ATERM_UPDATE_INTERVAL_SECS`, `spawn_background_check`), so 3 consecutive
/// failures is ≈4 minutes: the threshold now means "three checks in a row", and it
/// escalates far sooner than the original rationale intended. It has not been
/// re-tuned for the new cadence — revisit it against 75s rather than trusting the
/// old "skip a flaky day" reading.
pub use crate::PERSISTENT_AFTER;

/// The durable health record. All fields default so an absent/corrupt file reads as
/// "healthy" (fail-open here is correct: health is observability, never a gate).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    /// Consecutive `network`-class failures; cleared only by a healthy check.
    #[serde(default)]
    pub network_failures: u32,
    /// Consecutive `pipeline`-class failures; cleared only by a healthy check.
    /// This is the streak [`Self::is_persistent`] watches.
    #[serde(default)]
    pub pipeline_failures: u32,
    /// Consecutive `manifest`-class failures (fetched but rejected).
    #[serde(default)]
    pub manifest_failures: u32,
    /// Consecutive `stage`-class failures (downloaded but failed to verify/stage).
    #[serde(default)]
    pub stage_failures: u32,
    /// Class of the MOST RECENT failure (`""` when healthy).
    #[serde(default)]
    pub kind: String,
    /// RFC3339 UTC of the first failure of the current unhealthy period (any class).
    #[serde(default)]
    pub failing_since: String,
    /// RFC3339 UTC of the most recent failure.
    #[serde(default)]
    pub last_failure_at: String,
    /// The most recent failure's error text (truncated), for `update status`.
    #[serde(default)]
    pub last_error: String,
}

impl Health {
    /// Read the ledger; absent/corrupt ⇒ healthy default (observability, not a gate).
    /// Non-UTF-8 IS a corrupt ledger, deliberately folded to the healthy default.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        crate::read_ledger_text(path)
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Total consecutive failed checks across classes (the `failing=` status field).
    #[must_use]
    pub fn total_failures(&self) -> u32 {
        self.network_failures
            .saturating_add(self.pipeline_failures)
            .saturating_add(self.manifest_failures)
            .saturating_add(self.stage_failures)
    }

    /// Whether the ledger shows a PERSISTENT pipeline failure — releases visible,
    /// downloads impossible for [`PERSISTENT_AFTER`]+ checks: the surface-it-loudly
    /// state. Per-class streaks mean an interleaved network blip cannot reset this.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.pipeline_failures >= PERSISTENT_AFTER
    }

    /// Record one failed check of `kind` (`network` / `pipeline` / `manifest` /
    /// `stage`); unknown kinds count only toward `kind`/timestamps. Returns the
    /// updated record.
    pub fn record_failure(path: &Path, kind: &str, error: &str) -> Self {
        // Serialize the whole read→mutate→write against the same-pid sibling in this
        // process (the background loop vs. a manual `update check`), which the temp
        // file's pid key cannot separate. Best-effort: health is observability, never
        // a gate — if the lock can't be taken we proceed unlocked rather than drop the
        // update.
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        let now = crate::install::now_rfc3339();
        match kind {
            "network" => h.network_failures = h.network_failures.saturating_add(1),
            "pipeline" => h.pipeline_failures = h.pipeline_failures.saturating_add(1),
            "manifest" => h.manifest_failures = h.manifest_failures.saturating_add(1),
            "stage" => h.stage_failures = h.stage_failures.saturating_add(1),
            _ => {}
        }
        h.kind = kind.to_string();
        if h.failing_since.is_empty() {
            h.failing_since = now.clone();
        }
        h.last_failure_at = now;
        // Cap the stored error so a pathological message can't bloat the ledger.
        h.last_error = error.chars().take(400).collect();
        h.write(path);
        h
    }

    /// Record a fully-healthy check: every failure streak clears.
    pub fn record_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        // No-op skip: a healthy check on an already-clean ledger must not rewrite the
        // file every interval (mirrors `Floor::bump_and_write`).
        if h.total_failures() == 0
            && h.kind.is_empty()
            && h.failing_since.is_empty()
            && h.last_failure_at.is_empty()
            && h.last_error.is_empty()
        {
            return h;
        }
        h.network_failures = 0;
        h.pipeline_failures = 0;
        h.manifest_failures = 0;
        h.stage_failures = 0;
        h.kind = String::new();
        h.failing_since = String::new();
        h.last_failure_at = String::new();
        h.last_error = String::new();
        h.write(path);
        h
    }

    /// Best-effort sibling lock (`…/health.toml.lock`) guarding a whole read→mutate→
    /// write of the ledger. `None` on failure: the caller then proceeds unlocked —
    /// health is observability, never a gate, so a missed lock must never drop the
    /// update. Held for the lifetime of the returned guard (i.e. the record_* call).
    fn lock(path: &Path) -> Option<FileLock> {
        FileLock::acquire(&path.with_extension("toml.lock")).ok()
    }

    /// Best-effort atomic write (temp + rename), mirroring `status::record`.
    fn write(&self, path: &Path) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let Ok(text) = toml::to_string(self) else {
            return;
        };
        // Unique per INVOCATION (pid + a process-wide counter), not just per process:
        // two record_* calls in one process (background loop + manual `update check`)
        // must never stage through the same temp path and clobber each other's write.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = path.with_extension(format!(
            "toml.{}-{}.tmp",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-TEST scratch dir (tests share one process, so a pid-keyed dir would race
    /// across the parallel test threads).
    fn tmp(name: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("aterm-health-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("health.toml")
    }

    #[test]
    fn failure_streaks_count_per_class() {
        let p = tmp("streaks");
        assert!(
            !Health::read(&p).is_persistent(),
            "absent ledger reads healthy"
        );
        Health::record_failure(&p, "network", "dns");
        Health::record_failure(&p, "network", "dns");
        let h = Health::read(&p);
        assert_eq!((h.network_failures, h.kind.as_str()), (2, "network"));
        assert!(
            !h.is_persistent(),
            "network failures never escalate to persistent"
        );

        Health::record_failure(&p, "pipeline", "asset fetch failed");
        Health::record_failure(&p, "pipeline", "asset fetch failed");
        // An interleaved network blip must NOT reset the pipeline streak (or a flaky
        // network could suppress the stranded-client escalation forever).
        Health::record_failure(&p, "network", "blip");
        let h = Health::record_failure(&p, "pipeline", "asset fetch failed");
        assert_eq!(h.pipeline_failures, PERSISTENT_AFTER);
        assert!(
            h.is_persistent(),
            "3 pipeline failures are persistent despite the blip"
        );
        assert_eq!(h.total_failures(), 3 + 3);
        assert!(!h.failing_since.is_empty() && !h.last_error.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn success_clears_all_failure_streaks() {
        let p = tmp("success");
        Health::record_failure(&p, "pipeline", "x");
        Health::record_failure(&p, "stage", "y");
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        assert!(h.kind.is_empty() && h.last_error.is_empty() && h.failing_since.is_empty());
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn success_on_clean_ledger_does_not_write() {
        let p = tmp("clean-noop");
        // Absent ledger is already fully clean: a healthy check must not create it.
        assert!(!p.exists());
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        assert!(
            !p.exists(),
            "success on an already-clean ledger must not write the file"
        );

        // A clean ledger that DOES exist (a streak just cleared by a success) is
        // likewise not rewritten: capture the mtime and assert it is unchanged
        // across a second success.
        Health::record_failure(&p, "network", "blip");
        Health::record_success(&p);
        assert!(p.exists());
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        let after = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(
            before, after,
            "success on an already-clean ledger must not rewrite it"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn corrupt_ledger_reads_healthy_and_error_is_capped() {
        let p = tmp("corrupt");
        std::fs::write(&p, "not = [valid").unwrap();
        assert_eq!(Health::read(&p).total_failures(), 0);
        let long = "e".repeat(2000);
        let h = Health::record_failure(&p, "pipeline", &long);
        assert_eq!(h.last_error.len(), 400, "stored error text is capped");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
