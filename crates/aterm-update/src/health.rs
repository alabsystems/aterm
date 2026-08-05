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
//! * `apply` — a VERIFIED, STAGED build failed to become the running build: the
//!   seamless in-session handoff was refused/revoked/mismatched, the child died,
//!   or the swap+re-exec failed. This is the class that used to be missing (see
//!   SCOPE below); it is recorded by [`Health::record_apply_failure`] from the
//!   GUI's apply lane, and it is deliberately NOT cleared by a healthy download
//!   check — only by an apply that actually succeeds
//!   ([`Health::record_apply_success`]). A check succeeding says nothing about
//!   whether applying works, and conflating the two is exactly how a 100%-failing
//!   apply lane read green.
//!
//! The streaks are PER-CLASS (each cleared only by a fully healthy check), so an
//! interleaved network blip cannot reset a pipeline streak and suppress the
//! escalation. (An on-disk ledger from a pre-v0.26 build may carry the retired
//! rescue-path counters; unknown keys parse fine and are dropped on the next write.)
//!
//! # SCOPE — the apply lane is now covered too (2026-07-28)
//!
//! This ledger used to cover the CHECK/DOWNLOAD/STAGE lane ONLY, and said so. The
//! gap was not hypothetical: through 2026-07 this machine carried an all-zero
//! `health.toml` — a perfect score — while the seamless apply/handoff lane failed
//! 100% of the time across three releases and every update had to wait for a cold
//! launch. Downloading and staging really were healthy; the ledger was telling the
//! truth about the only thing it measured, and the dashboard read green on a
//! half-broken updater.
//!
//! The `apply` class closes it. Two rules keep it honest:
//!
//! 1. **A healthy CHECK does not clear it.** [`Health::record_success`] resets the
//!    acquisition streaks and leaves `apply_failures` alone, because "I can see
//!    and download the release" is not evidence that applying it works. Only
//!    [`Health::record_apply_success`] clears it.
//! 2. **It counts toward [`Health::is_persistent`].** A staged build that will not
//!    apply strands the machine exactly as surely as one that will not download,
//!    so it escalates to the same loud notification rather than sitting in a file.

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
    /// Consecutive `apply`-class failures: a verified, staged build that did not
    /// become the running build (handoff refused/revoked/mismatched, child died,
    /// swap or re-exec failed). Cleared ONLY by a successful apply, never by a
    /// healthy download check — see the module SCOPE note.
    #[serde(default)]
    pub apply_failures: u32,
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
    /// The most recent APPLY-lane failure's reason, kept separately from
    /// [`Self::last_error`].
    ///
    /// The two lanes interleave: a handoff can fail at 12:00 and a network blip
    /// land at 12:01, and a single `last_error` would leave the surviving apply
    /// streak described by an unrelated DNS message. Keeping the apply reason
    /// apart lets [`Self::record_success`] restore an honest `kind`/`last_error`
    /// when it clears the acquisition streaks and the apply streak survives.
    #[serde(default)]
    pub last_apply_error: String,
    /// The most recent apply-lane REFUSAL: a verdict that stopped an apply
    /// BEFORE it could fail, in the words of whichever caller refused it.
    ///
    /// Deliberately not a streak and deliberately absent from
    /// [`Self::total_failures`]/[`Self::is_persistent`]: a refusal is a normal,
    /// self-correcting state, and counting one would manufacture a persistent-
    /// failure escalation out of ordinary terminal use. That reasoning is why
    /// refusals were recorded NOWHERE — which is the defect this field closes.
    /// In the field, `update apply` answered "OK apply requested", the reducer
    /// refused it, and `update status` went on reporting a healthy updater with
    /// a staged build for hours: a refusal that records nothing is
    /// indistinguishable from an updater that never ran at all.
    #[serde(default)]
    pub last_apply_refusal: String,
    /// RFC3339 UTC of [`Self::last_apply_refusal`] (empty when there is none).
    /// A refusal without a time cannot be told apart from a stale one.
    #[serde(default)]
    pub last_apply_refusal_at: String,
    /// The build that was RUNNING when [`Self::last_apply_refusal`] was recorded
    /// (0 when there is none).
    ///
    /// A successful in-session apply never returns to clear anything — it execs
    /// into the new image — so without this the last refusal would follow the
    /// machine into the build that proves it was overcome, and `update status`
    /// would explain a fixed problem forever. A refusal is only ever about the
    /// build that could not move; readers drop it once a different build is
    /// running.
    #[serde(default)]
    pub last_apply_refusal_build: u64,
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
            .saturating_add(self.apply_failures)
    }

    /// Whether the ledger shows a PERSISTENT failure of a lane that strands the
    /// machine: either `pipeline` (releases visible, downloads impossible) or
    /// `apply` (a verified build staged but never able to become the running
    /// build) for [`PERSISTENT_AFTER`]+ consecutive attempts. Both are the
    /// surface-it-loudly state; per-class streaks mean an interleaved network blip
    /// cannot reset either.
    ///
    /// `apply` is included because the user-visible symptom is identical — the
    /// machine does not move to the new version — and leaving it out is what let a
    /// 100%-failing handoff lane report healthy for three releases.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.pipeline_failures >= PERSISTENT_AFTER || self.apply_failures >= PERSISTENT_AFTER
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
            "apply" => h.apply_failures = h.apply_failures.saturating_add(1),
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

    /// Record a fully-healthy CHECK: every ACQUISITION streak clears.
    ///
    /// `apply_failures` deliberately survives. A check proves the machine can see,
    /// fetch, verify and stage a release; it proves nothing about whether that
    /// staged build can be made to run, which is a different lane with different
    /// failure modes. Clearing it here would recreate the exact blindness this
    /// class was added to remove — every 75 s check would wipe the evidence that
    /// the handoff never completes. Use [`Self::record_apply_success`] for that.
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
        let apply_streak_survives = h.apply_failures;
        h.network_failures = 0;
        h.pipeline_failures = 0;
        h.manifest_failures = 0;
        h.stage_failures = 0;
        h.apply_failures = apply_streak_survives;
        if apply_streak_survives == 0 {
            h.kind = String::new();
            h.failing_since = String::new();
            h.last_failure_at = String::new();
            h.last_error = String::new();
        } else {
            // The acquisition streaks cleared but the machine still cannot APPLY.
            // Re-describe the ledger in terms of the failure that is actually
            // still standing, or `update status` would report the apply streak
            // under whatever transient network message happened to land last.
            h.kind = "apply".to_string();
            h.last_error = h.last_apply_error.clone();
        }
        h.write(path);
        h
    }

    /// Record that a staged build FAILED to become the running build. `reason` is
    /// the typed handoff/apply outcome (e.g. `ChildDied`, `AdoptionMismatch`,
    /// `ActivityRevoked`, `re-exec failed`), stored for `update status`.
    pub fn record_apply_failure(path: &Path, reason: &str) -> Self {
        Self::record_failure(path, "apply", reason);
        // Mirror the reason into the apply-owned slot so a later acquisition-lane
        // failure cannot overwrite the description of a still-standing apply streak.
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        h.last_apply_error = reason.chars().take(400).collect();
        // A terminal verdict supersedes whatever refusal preceded it: leaving the
        // old "the terminal was busy" standing beside a hard failure would offer
        // an operator two competing answers to one question.
        h.clear_apply_refusal();
        h.write(path);
        h
    }

    /// Record one apply-lane REFUSAL — a block/deferral that stopped an apply
    /// before it became a failure — as observed by `current_build`. `reason` must
    /// say WHAT refused and WHY, since it is the whole answer an operator gets to
    /// "the build is staged, so why is it not running?".
    ///
    /// Every streak is untouched by construction (see [`Self::last_apply_refusal`]);
    /// this only replaces the standing explanation, so a refusal can never
    /// escalate to the persistent-failure notification on its own.
    pub fn record_apply_refusal(path: &Path, current_build: u64, reason: &str) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        h.last_apply_refusal = reason.chars().take(400).collect();
        h.last_apply_refusal_at = crate::install::now_rfc3339();
        h.last_apply_refusal_build = current_build;
        h.write(path);
        h
    }

    /// Whether the stored refusal is still the answer for `current_build`. A
    /// refusal recorded by a build that is no longer running was overcome by
    /// definition — most often by the very apply that execed away without
    /// returning to clear it.
    #[must_use]
    pub fn apply_refusal_applies_to(&self, current_build: u64) -> bool {
        !self.last_apply_refusal.is_empty() && self.last_apply_refusal_build == current_build
    }

    fn clear_apply_refusal(&mut self) {
        self.last_apply_refusal = String::new();
        self.last_apply_refusal_at = String::new();
        self.last_apply_refusal_build = 0;
    }

    /// Record that an apply actually succeeded — the staged build is now the
    /// running build. Clears the `apply` streak only; the acquisition streaks are
    /// owned by [`Self::record_success`].
    pub fn record_apply_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        // An apply that actually went through answers every standing apply-lane
        // question, refusals included: a "the terminal was busy" left behind by an
        // earlier attempt must not outlive the attempt that succeeded.
        if h.apply_failures == 0 && h.last_apply_refusal.is_empty() {
            return h;
        }
        h.clear_apply_refusal();
        if h.apply_failures > 0 {
            h.apply_failures = 0;
            h.last_apply_error = String::new();
            if h.total_failures() == 0 {
                h.kind = String::new();
                h.failing_since = String::new();
                h.last_failure_at = String::new();
                h.last_error = String::new();
            } else if h.kind == "apply" {
                // Acquisition failures are still standing; stop describing the ledger
                // by the apply failure that just resolved.
                h.kind = String::new();
            }
        }
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

    /// THE regression this class exists for. Through 2026-07 the apply lane failed
    /// 100% of the time while the ledger read all-zero, because only the download
    /// lane was recorded and every healthy check cleared everything. Assert the two
    /// halves of the fix: apply failures are recorded, and a healthy CHECK does not
    /// erase them.
    #[test]
    fn a_healthy_check_never_clears_the_apply_streak() {
        let p = tmp("apply-survives-check");
        Health::record_apply_failure(&p, "AdoptionMismatch");
        Health::record_apply_failure(&p, "ActivityRevoked");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 2);
        assert_eq!(h.kind, "apply");
        assert_eq!(h.last_error, "ActivityRevoked");

        // A perfectly healthy download check comes in. It must clear the
        // acquisition streaks and LEAVE the apply streak alone — otherwise the
        // 75 s cadence wipes the evidence before anyone can read it.
        Health::record_failure(&p, "network", "dns");
        Health::record_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.network_failures, 0, "acquisition streak clears");
        assert_eq!(
            h.apply_failures, 2,
            "a healthy check must NOT vouch for the apply lane"
        );
        assert_eq!(
            h.kind, "apply",
            "the standing failure is still the apply one"
        );
        assert_eq!(
            h.last_error, "ActivityRevoked",
            "the standing streak must be described by ITS reason, not the network blip"
        );

        // Only a real apply success clears it.
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.total_failures(), 0);
        assert!(h.kind.is_empty(), "fully healthy ledger reports no class");
    }

    /// A refusal is the answer to "the build is staged, so why is it not
    /// running?" — it must be durable, must NOT manufacture a failure streak (or
    /// ordinary terminal use would escalate to the persistent-failure
    /// notification), and must not outlive the apply that finally succeeds.
    #[test]
    fn a_refusal_is_recorded_without_inventing_a_failure_streak() {
        let p = tmp("apply-refusal");
        let quiet = "terminal input/output is still inside the quiet epoch";
        Health::record_apply_refusal(&p, 812, quiet);
        let h = Health::read(&p);
        assert_eq!(h.last_apply_refusal, quiet);
        assert!(!h.last_apply_refusal_at.is_empty(), "a refusal is timed");
        assert!(h.apply_refusal_applies_to(812));
        assert_eq!(h.total_failures(), 0, "a refusal is not a failure");
        assert!(!h.is_persistent(), "a refusal never escalates on its own");
        assert!(
            h.kind.is_empty(),
            "a refusal does not claim a failure class"
        );

        // A successful in-session apply execs away and never returns to clear the
        // slot, so the running build is what retires a refusal: build 813 is proof
        // that whatever stopped 812 was overcome.
        assert!(
            !h.apply_refusal_applies_to(813),
            "a refusal must not follow the machine into the build that overcame it"
        );

        // A healthy CHECK must leave it alone for the same reason it leaves the
        // apply streak alone: downloading proves nothing about applying.
        Health::record_failure(&p, "network", "dns");
        Health::record_success(&p);
        assert_eq!(Health::read(&p).last_apply_refusal, quiet);

        // A hard failure supersedes it — one standing answer, never two.
        Health::record_apply_failure(&p, "ChildDied");
        let h = Health::read(&p);
        assert!(!h.apply_refusal_applies_to(812));
        assert!(h.last_apply_refusal.is_empty());
        assert!(h.last_apply_refusal_at.is_empty());
        assert_eq!(h.last_apply_error, "ChildDied");

        // And a success clears the whole lane, refusal slot included.
        Health::record_apply_refusal(&p, 812, "updater work is in flight");
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert!(h.last_apply_refusal.is_empty());
        assert!(h.last_apply_refusal_at.is_empty());
        assert_eq!(h.last_apply_refusal_build, 0);
        assert_eq!(h.apply_failures, 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// A stranded apply lane must escalate as loudly as a stranded download lane:
    /// the user-visible symptom (machine does not move to the new version) is the
    /// same, so it drives the same persistent-failure notification.
    #[test]
    fn a_persistent_apply_streak_is_surfaced_like_a_persistent_pipeline_streak() {
        let p = tmp("apply-persistent");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_apply_failure(&p, "ChildDied");
        }
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, PERSISTENT_AFTER);
        assert!(
            h.is_persistent(),
            "a staged build that never applies must escalate, not sit in a file"
        );
        // And a healthy check does not silence it.
        Health::record_success(&p);
        assert!(Health::read(&p).is_persistent());
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
