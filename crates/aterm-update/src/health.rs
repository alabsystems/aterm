// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable self-healing updater health ledger (`Updates/health.toml`).

use aterm_update_core::FileLock;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How long a ledger write waits for `health.toml.lock` before proceeding unlocked.
const HEALTH_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Failure-count and duration thresholds for persistent updater errors.
pub use crate::{PERSISTENT_AFTER, PERSISTENT_SPAN};

/// Durable updater health ledger persisted at `Updates/health.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Health {
    /// Consecutive `network`-class failures.
    #[serde(default)]
    pub network_failures: u32,
    /// Consecutive `pipeline`-class failures.
    #[serde(default)]
    pub pipeline_failures: u32,
    /// Consecutive `manifest`-class failures.
    #[serde(default)]
    pub manifest_failures: u32,
    /// Consecutive `stage`-class failures.
    #[serde(default)]
    pub stage_failures: u32,
    /// Consecutive `apply`-class failures.
    #[serde(default)]
    pub apply_failures: u32,
    /// RFC3339 UTC timestamp when the current `network` failure streak began.
    #[serde(default)]
    pub network_since: String,
    /// RFC3339 UTC timestamp when the current `pipeline` failure streak began.
    #[serde(default)]
    pub pipeline_since: String,
    /// RFC3339 UTC timestamp when the current `manifest` failure streak began.
    #[serde(default)]
    pub manifest_since: String,
    /// RFC3339 UTC timestamp when the current `stage` failure streak began.
    #[serde(default)]
    pub stage_since: String,
    /// RFC3339 UTC timestamp when the current `apply` failure streak began.
    #[serde(default)]
    pub apply_since: String,
    /// Class of the most recent failure (`""` when healthy).
    #[serde(default)]
    pub kind: String,
    /// RFC3339 UTC timestamp of the first failure in the current unhealthy period.
    #[serde(default)]
    pub failing_since: String,
    /// RFC3339 UTC timestamp of the most recent failure.
    #[serde(default)]
    pub last_failure_at: String,
    /// Truncated error text of the most recent failure.
    #[serde(default)]
    pub last_error: String,
    /// Reason from the most recent `apply`-lane failure.
    #[serde(default)]
    pub last_apply_error: String,
    /// Reason from the most recent diagnosis-class (`pipeline`, `manifest`, `stage`) failure.
    #[serde(default)]
    pub last_diagnosis_error: String,
    /// RFC3339 UTC timestamp of [`Self::last_diagnosis_error`].
    #[serde(default)]
    pub last_diagnosis_at: String,
    /// RFC3339 UTC timestamp of [`Self::last_apply_error`].
    #[serde(default)]
    pub last_apply_failure_at: String,
    /// Build number that was running when the last apply failure occurred.
    #[serde(default)]
    pub last_apply_failure_build: u64,
    /// Target build number that the last failed apply attempted to reach.
    #[serde(default)]
    pub last_apply_failure_target_build: u64,
    /// Consecutive failed applies targeting [`Self::last_apply_failure_target_build`].
    #[serde(default)]
    pub apply_failures_for_target: u32,
    /// Reason from the most recent apply-lane refusal.
    #[serde(default)]
    pub last_apply_refusal: String,
    /// RFC3339 UTC timestamp of [`Self::last_apply_refusal`].
    #[serde(default)]
    pub last_apply_refusal_at: String,
    /// Build number that was running when [`Self::last_apply_refusal`] was recorded.
    #[serde(default)]
    pub last_apply_refusal_build: u64,
    /// Reason [`Self::trial_proof_failure_build`]'s boot trial remains unconfirmed.
    #[serde(default)]
    pub trial_proof_failure: String,
    /// RFC3339 UTC timestamp when [`Self::trial_proof_failure`] was first recorded.
    #[serde(default)]
    pub trial_proof_failure_at: String,
    /// Build number whose trial [`Self::trial_proof_failure`] describes.
    #[serde(default)]
    pub trial_proof_failure_build: u64,
    /// Newer build number currently waiting to be applied.
    #[serde(default)]
    pub pending_build: u64,
    /// RFC3339 UTC timestamp when [`Self::pending_build`] was first seen waiting.
    #[serde(default)]
    pub pending_since: String,
    /// Duration in microseconds of the last timed seamless handoff capture dry run.
    #[serde(default)]
    pub handoff_capture_us: u64,
    /// Session count carried during [`Self::handoff_capture_us`].
    #[serde(default)]
    pub handoff_capture_sessions: u32,
    /// Initial automatic freeze rung in milliseconds seeded by [`Self::handoff_capture_us`].
    #[serde(default)]
    pub handoff_freeze_seed_ms: u64,
    /// Build number that measured [`Self::handoff_capture_us`].
    #[serde(default)]
    pub handoff_capture_build: u64,
    /// RFC3339 UTC timestamp of [`Self::handoff_capture_us`].
    #[serde(default)]
    pub handoff_capture_at: String,
    /// Duration in microseconds of the last landed seamless handoff park.
    #[serde(default)]
    pub handoff_park_us: u64,
    /// Build number that recorded [`Self::handoff_park_us`].
    #[serde(default)]
    pub handoff_park_build: u64,
    /// RFC3339 UTC timestamp of [`Self::handoff_park_us`].
    #[serde(default)]
    pub handoff_park_at: String,
}

/// The longest error text the ledger stores, in characters.
const MAX_STORED_ERROR_CHARS: usize = 400;

/// `error` cut to [`MAX_STORED_ERROR_CHARS`] — WITHOUT CUTTING OFF WHAT IT IS. A
/// passing refusal carries its key ([`crate::PASSING_REFUSAL_KEY`]) at the END of its
/// text, and readers ([`crate::is_passing_refusal`]) tell a moment from a verdict by
/// it.
fn capped_error(error: &str) -> String {
    if error.chars().count() <= MAX_STORED_ERROR_CHARS {
        return error.to_string();
    }
    if !crate::is_passing_refusal(error) {
        return error.chars().take(MAX_STORED_ERROR_CHARS).collect();
    }
    let tail = format!(" \u{2026} ({})", crate::PASSING_REFUSAL_KEY);
    let head = MAX_STORED_ERROR_CHARS.saturating_sub(tail.chars().count());
    let mut kept: String = error.chars().take(head).collect();
    kept.push_str(&tail);
    kept
}

impl Health {
    /// Read the ledger at `path`, returning a healthy default if absent or corrupt.
    #[must_use]
    pub fn read(path: &Path) -> Self {
        let mut h: Self = crate::read_ledger_text(path)
            .and_then(|t| aterm_toml::from_str(&t).ok())
            .unwrap_or_default();
        h.last_diagnosis_at = h.diagnosis_at().to_string();
        h
    }

    /// The newest failure of the standing diagnosis class ([`Self::last_diagnosis_at`]).
    fn diagnosis_at(&self) -> &str {
        &self.last_diagnosis_at
    }

    /// Total consecutive failed checks across all classes.
    #[must_use]
    pub const fn total_failures(&self) -> u32 {
        self.network_failures
            .saturating_add(self.pipeline_failures)
            .saturating_add(self.manifest_failures)
            .saturating_add(self.stage_failures)
            .saturating_add(self.apply_failures)
    }

    /// Consecutive failed checks across acquisition classes (`network`, `pipeline`, `manifest`, `stage`).
    #[must_use]
    pub fn acquisition_failures(&self) -> u32 {
        match self.standing_acquisition_class() {
            Some("pipeline") => self.pipeline_failures,
            Some("manifest") => self.manifest_failures,
            Some("stage") => self.stage_failures,
            Some("network") => self.network_failures,
            _ => 0,
        }
    }

    /// The `(streak, start-of-streak)` pair a failure class owns, or `None` for an
    /// unrecognised kind.
    fn class_streak_mut(&mut self, kind: &str) -> Option<(&mut u32, &mut String)> {
        match kind {
            "network" => Some((&mut self.network_failures, &mut self.network_since)),
            "pipeline" => Some((&mut self.pipeline_failures, &mut self.pipeline_since)),
            "manifest" => Some((&mut self.manifest_failures, &mut self.manifest_since)),
            "stage" => Some((&mut self.stage_failures, &mut self.stage_since)),
            "apply" => Some((&mut self.apply_failures, &mut self.apply_since)),
            _ => None,
        }
    }

    /// RFC3339 timestamp when `kind`'s current failure streak began.
    #[must_use]
    pub fn class_since(&self, kind: &str) -> &str {
        let since = match kind {
            "network" => &self.network_since,
            "pipeline" => &self.pipeline_since,
            "manifest" => &self.manifest_since,
            "stage" => &self.stage_since,
            "apply" => &self.apply_since,
            _ => &self.failing_since,
        };
        if since.is_empty() {
            &self.failing_since
        } else {
            since
        }
    }

    /// Highest-priority failure class whose streak has crossed both persistence gates, with its count.
    #[must_use]
    pub fn persistent_class(&self) -> Option<(&'static str, u32)> {
        [
            ("pipeline", self.pipeline_failures),
            ("manifest", self.manifest_failures),
            ("stage", self.stage_failures),
            ("apply", self.apply_failures),
        ]
        .into_iter()
        .find(|&(class, n)| n >= PERSISTENT_AFTER && self.spans_persistently(class))
    }

    /// Whether `class`'s streak has lasted [`PERSISTENT_SPAN`] (see
    /// [`Self::persistent_class`]).
    fn spans_persistently(&self, class: &str) -> bool {
        if class == "apply" {
            return true;
        }
        crate::install::rfc3339_delta_secs(self.class_since(class), self.diagnosis_at())
            .is_some_and(|span| span >= PERSISTENT_SPAN.as_secs())
    }

    /// Whether a stranding failure class (`pipeline`, `manifest`, `stage`, `apply`) is persistent.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.persistent_class().is_some()
    }

    /// Record a failed check of `kind` with `error` and persist to `path`.
    pub fn record_failure(path: &Path, kind: &str, error: &str) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        Self::advance_failure_streak(&mut h, kind, error);
        h.write(path);
        h
    }

    /// The streak arithmetic shared by [`Self::record_failure`] and
    /// [`Self::record_apply_failure`], applied to an ALREADY-LOCKED, already-read
    /// record so a caller that must do more under the same lock can.
    fn advance_failure_streak(h: &mut Self, kind: &str, error: &str) {
        let now = crate::install::now_rfc3339();
        if let Some((count, since)) = h.class_streak_mut(kind) {
            if *count == 0 {
                *since = now.clone();
            } else if since.is_empty() {
                *count = 0;
                *since = now.clone();
            }
            *count = count.saturating_add(1);
        }
        if matches!(kind, "pipeline" | "manifest" | "stage") {
            for sibling in ["pipeline", "manifest", "stage"] {
                if sibling != kind
                    && let Some((count, since)) = h.class_streak_mut(sibling)
                {
                    *count = 0;
                    since.clear();
                }
            }
        }
        h.kind = kind.to_string();
        if h.failing_since.is_empty() {
            h.failing_since = now.clone();
        }
        h.last_failure_at = now;
        h.last_error = capped_error(error);
        if matches!(kind, "pipeline" | "manifest" | "stage") {
            h.last_diagnosis_error = h.last_error.clone();
            h.last_diagnosis_at = h.last_failure_at.clone();
        }
    }

    /// Re-describe the ledger by the class that still stands, with THAT
    /// class's own reason: the apply lane's for `apply`, the diagnosis lane's
    /// for `pipeline`/`manifest`/`stage`, nothing for a bare network blip
    /// (weather has no diagnosis).
    fn relabel_by_standing(h: &mut Self) {
        if h.total_failures() == 0 {
            h.kind = String::new();
            h.failing_since = String::new();
            h.last_failure_at = String::new();
            h.last_error = String::new();
            h.last_diagnosis_error = String::new();
            h.last_diagnosis_at = String::new();
            return;
        }
        match h.standing_acquisition_class() {
            Some(standing @ ("pipeline" | "manifest" | "stage")) => {
                h.kind = standing.to_string();
                if !h.last_diagnosis_error.is_empty() {
                    h.last_error = h.last_diagnosis_error.clone();
                }
            }
            Some(standing) => {
                // `network` alone: weather, described by whatever it said.
                h.kind = standing.to_string();
            }
            None => {
                h.kind = "apply".to_string();
                h.last_error = h.last_apply_error.clone();
            }
        }
    }

    /// Record a healthy check, clearing all acquisition streaks while preserving `apply` state.
    pub fn record_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
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
        h.network_since = String::new();
        h.pipeline_since = String::new();
        h.manifest_since = String::new();
        h.stage_since = String::new();
        h.apply_failures = apply_streak_survives;
        if apply_streak_survives == 0 {
            h.apply_since = String::new();
        }
        h.last_diagnosis_error = String::new();
        h.last_diagnosis_at = String::new();
        Self::relabel_by_standing(&mut h);
        h.write(path);
        h
    }

    /// Highest-priority standing acquisition failure class (`pipeline`, `manifest`, `stage`, `network`).
    #[must_use]
    pub(crate) fn standing_acquisition_class(&self) -> Option<&'static str> {
        [
            ("pipeline", self.pipeline_failures),
            ("manifest", self.manifest_failures),
            ("stage", self.stage_failures),
            ("network", self.network_failures),
        ]
        .into_iter()
        .find(|(_, n)| *n > 0)
        .map(|(class, _)| class)
    }

    /// Record a check that proved acquisition works without attempting a stage.
    pub fn record_acquisition_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.network_failures == 0 && h.pipeline_failures == 0 && h.manifest_failures == 0 {
            return h;
        }
        h.network_failures = 0;
        h.pipeline_failures = 0;
        h.manifest_failures = 0;
        h.network_since = String::new();
        h.pipeline_since = String::new();
        h.manifest_since = String::new();
        Self::relabel_by_standing(&mut h);
        h.write(path);
        h
    }

    /// Record that staged `target_build` failed to become the running build under `current_build`.
    pub fn record_apply_failure(
        path: &Path,
        current_build: u64,
        target_build: u64,
        reason: &str,
    ) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        Self::advance_failure_streak(&mut h, "apply", reason);
        h.last_apply_error = capped_error(reason);
        h.last_apply_failure_at = crate::install::now_rfc3339();
        h.last_apply_failure_build = current_build;
        if h.last_apply_failure_target_build != target_build {
            h.apply_failures_for_target = 0;
        }
        h.apply_failures_for_target = h.apply_failures_for_target.saturating_add(1);
        h.last_apply_failure_target_build = target_build;
        h.clear_apply_refusal();
        h.write(path);
        h
    }

    /// Clear the apply failure streak if `current_build` has moved past [`Self::last_apply_failure_build`].
    pub fn expire_stale_apply_streak(path: &Path, current_build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.apply_failures == 0 || h.last_apply_failure_build == current_build {
            return h;
        }
        if h.last_apply_failure_build != 0 && current_build < h.last_apply_failure_build {
            return h;
        }
        h.apply_failures = 0;
        h.apply_since = String::new();
        h.last_apply_error = String::new();
        h.last_apply_failure_at = String::new();
        h.last_apply_failure_build = 0;
        h.apply_failures_for_target = 0;
        h.last_apply_failure_target_build = 0;
        if h.total_failures() == 0 || h.kind == "apply" {
            // Some acquisition streak may still stand; let it own the headline
            // WITH ITS OWN REASON (`kind == ""` means healthy).
            Self::relabel_by_standing(&mut h);
        }
        h.write(path);
        h
    }

    /// Record an apply-lane refusal observed by `current_build`.
    pub fn record_apply_refusal(path: &Path, current_build: u64, reason: &str) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        h.last_apply_refusal = capped_error(reason);
        h.last_apply_refusal_at = crate::install::now_rfc3339();
        h.last_apply_refusal_build = current_build;
        h.write(path);
        h
    }

    /// Whether the recorded apply refusal still applies to `current_build`.
    #[must_use]
    pub const fn apply_refusal_applies_to(&self, current_build: u64) -> bool {
        !self.last_apply_refusal.is_empty() && self.last_apply_refusal_build == current_build
    }

    /// Record that `build`'s boot trial recovery proof failed with `words`.
    pub fn record_trial_proof_failure(path: &Path, build: u64, words: &str) -> bool {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        let words = capped_error(words);
        if h.trial_proof_failure_build == build && h.trial_proof_failure == words {
            return false;
        }
        h.trial_proof_failure = words;
        h.trial_proof_failure_at = crate::install::now_rfc3339();
        h.trial_proof_failure_build = build;
        h.write(path);
        true
    }

    fn clear_apply_refusal(&mut self) {
        self.last_apply_refusal = String::new();
        self.last_apply_refusal_at = String::new();
        self.last_apply_refusal_build = 0;
    }

    /// Clear the standing apply refusal if its text contains `key`.
    pub fn withdraw_apply_refusal_containing(path: &Path, key: &str) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if key.is_empty() || !h.last_apply_refusal.contains(key) {
            return h;
        }
        h.clear_apply_refusal();
        h.write(path);
        h
    }

    /// Record that an update apply succeeded, clearing `apply` failure, refusal, and pending state.
    pub fn record_apply_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.apply_failures == 0
            && h.apply_failures_for_target == 0
            && h.last_apply_refusal.is_empty()
            && h.trial_proof_failure.is_empty()
        {
            return h;
        }
        h.clear_apply_refusal();
        h.trial_proof_failure = String::new();
        h.trial_proof_failure_at = String::new();
        h.trial_proof_failure_build = 0;
        h.apply_failures_for_target = 0;
        h.last_apply_failure_target_build = 0;
        if h.apply_failures > 0 {
            h.apply_failures = 0;
            h.apply_since = String::new();
            h.last_apply_error = String::new();
            h.last_apply_failure_at = String::new();
            h.last_apply_failure_build = 0;
            if h.total_failures() == 0 || h.kind == "apply" {
                // Acquisition failures may still stand; stop describing the ledger
                // by the apply failure that just resolved — and NAME the one that
                // is still standing, with its own reason, because `kind == ""`
                // means healthy.
                Self::relabel_by_standing(&mut h);
            }
        }
        h.write(path);
        h
    }

    /// Update the pending-build clock for `observed` as seen by `current_build`.
    pub fn note_pending_update(path: &Path, current_build: u64, observed: Option<u64>) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        let observed = observed.filter(|build| *build > current_build);
        let changed = match observed {
            Some(build) if build != h.pending_build || h.pending_since.is_empty() => {
                h.pending_build = build;
                h.pending_since = crate::install::now_rfc3339();
                true
            }
            Some(_) => false,
            None if h.pending_build != 0 && h.pending_build <= current_build => {
                h.pending_build = 0;
                h.pending_since = String::new();
                true
            }
            None => false,
        };
        if changed {
            h.write(path);
        }
        h
    }

    /// Restart the pending clock for `build` now.
    pub fn restart_pending_update(path: &Path, build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.pending_build == build && build != 0 {
            h.pending_since = crate::install::now_rfc3339();
            h.write(path);
        }
        h
    }

    /// Clear the pending clock if it tracks withdrawn `build`.
    pub fn forget_pending_update(path: &Path, build: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.pending_build == build {
            h.pending_build = 0;
            h.pending_since = String::new();
            h.write(path);
        }
        h
    }

    /// Record a timed handoff capture dry run.
    pub fn record_handoff_capture(
        path: &Path,
        current_build: u64,
        capture_us: u64,
        sessions: u32,
        freeze_seed_ms: u64,
    ) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if capture_us == 0 {
            return h;
        }
        h.handoff_capture_us = capture_us;
        h.handoff_capture_sessions = sessions;
        h.handoff_freeze_seed_ms = freeze_seed_ms;
        h.handoff_capture_build = current_build;
        h.handoff_capture_at = crate::install::now_rfc3339();
        h.write(path);
        h
    }

    /// Record the duration of a landed seamless handoff park.
    pub fn record_handoff_park(path: &Path, current_build: u64, park_us: u64) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if park_us == 0 {
            return h;
        }
        h.handoff_park_us = park_us;
        h.handoff_park_build = current_build;
        h.handoff_park_at = crate::install::now_rfc3339();
        h.write(path);
        h
    }

    /// Best-effort sibling lock (`…/health.toml.lock`) guarding a whole read→mutate→
    /// write of the ledger.
    fn lock(path: &Path) -> Option<FileLock> {
        FileLock::acquire_within(&path.with_extension("toml.lock"), HEALTH_LOCK_WAIT).ok()
    }

    /// Best-effort atomic write (temp + rename), mirroring `status::record`.
    fn write(&self, path: &Path) {
        use std::sync::atomic::{AtomicU64, Ordering};
        let Ok(text) = aterm_toml::to_string(self) else {
            return;
        };
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

/// Test seam: shift all active streak timestamps in `path` back by `by`.
#[cfg(test)]
pub(crate) fn backdate_streaks(path: &Path, by: std::time::Duration) {
    let mut h = Health::read(path);
    for since in [
        &mut h.network_since,
        &mut h.pipeline_since,
        &mut h.manifest_since,
        &mut h.stage_since,
        &mut h.apply_since,
        &mut h.failing_since,
    ] {
        if let Some(at) = aterm_types::rfc3339::parse_utc(since)
            .and_then(|at| u64::try_from(at).ok())
            .and_then(|at| at.checked_sub(by.as_secs()))
        {
            *since = aterm_types::rfc3339::format_rfc3339(at);
        }
    }
    h.write(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `health.toml` in a per-TEST scratch dir (tests share one process, so a
    /// pid-keyed dir would race across the parallel test threads).
    struct Scratch {
        file: std::path::PathBuf,
        _dir: aterm_tempfile::TempDir,
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.file
        }
    }

    impl AsRef<Path> for Scratch {
        fn as_ref(&self) -> &Path {
            &self.file
        }
    }

    fn tmp(name: &str) -> Scratch {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-health-test-{name}-"))
            .tempdir()
            .unwrap();
        Scratch {
            file: dir.path().join("health.toml"),
            _dir: dir,
        }
    }

    /// A TIMED HANDOFF CAPTURE IS A PRIOR: it survives a re-read, the newest
    /// measurement replaces the last outright, a zero reading
    /// records nothing, and no streak or standing slot moves for it.
    #[test]
    fn a_timed_handoff_capture_is_kept_as_a_prior_and_moves_no_streak() {
        let p = tmp("handoff-capture");
        Health::record_apply_failure(&p, 9, 909, "ActivityRevoked");
        Health::record_apply_refusal(&p, 9, "the terminal was busy");
        let recorded = Health::record_handoff_capture(&p, 9, 22_000, 2, 33);
        let read = Health::read(&p);
        for h in [&recorded, &read] {
            assert_eq!(
                (
                    h.handoff_capture_us,
                    h.handoff_capture_sessions,
                    h.handoff_freeze_seed_ms,
                    h.handoff_capture_build
                ),
                (22_000, 2, 33, 9)
            );
            assert!(
                !h.handoff_capture_at.is_empty(),
                "a measurement carries its time"
            );
            assert_eq!(h.apply_failures, 1, "no streak moves");
            assert_eq!(h.apply_failures_for_target, 1);
            assert_eq!(h.last_apply_refusal, "the terminal was busy");
            assert!(!h.is_persistent());
        }
        let newer = Health::record_handoff_capture(&p, 10, 9_000, 1, 20);
        assert_eq!(
            (
                newer.handoff_capture_us,
                newer.handoff_capture_sessions,
                newer.handoff_capture_build
            ),
            (9_000, 1, 10),
            "the newest measurement replaces the last, cheaper or not"
        );
        let zero = Health::record_handoff_capture(&p, 11, 0, 3, 20);
        assert_eq!(
            (zero.handoff_capture_us, zero.handoff_capture_build),
            (9_000, 10),
            "a zero reading cannot be told from none, so it records nothing"
        );

        let parked = Health::record_handoff_park(&p, 12, 22_000);
        assert_eq!(
            (parked.handoff_park_us, parked.handoff_park_build),
            (22_000, 12)
        );
        assert!(!parked.handoff_park_at.is_empty());
        assert_eq!(
            parked.handoff_capture_us, 9_000,
            "the dry run's slot is untouched"
        );
        let zero = Health::record_handoff_park(&p, 13, 0);
        assert_eq!(
            (zero.handoff_park_us, zero.handoff_park_build),
            (22_000, 12)
        );
        let read = Health::read(&p);
        assert_eq!(
            (
                read.handoff_park_us,
                read.handoff_capture_us,
                read.apply_failures
            ),
            (22_000, 9_000, 1)
        );
    }

    /// The stage-backoff check proves ACQUISITION works while deliberately declining to
    /// download, so it must not vouch for the lane it just refused to exercise.
    #[test]
    fn an_acquisition_only_success_preserves_the_stage_and_apply_streaks() {
        let p = tmp("acquisition-success");
        Health::record_failure(&p, "network", "dns");
        Health::record_failure(&p, "stage", "bundle would not verify");
        Health::record_apply_failure(&p, 9, 900 + 9, "ActivityRevoked");
        let h = Health::read(&p);
        assert_eq!(
            (h.network_failures, h.stage_failures, h.apply_failures),
            (1, 1, 1)
        );

        let h = Health::record_acquisition_success(&p);
        assert_eq!(h.network_failures, 0, "acquisition streak clears");
        assert!(h.network_since.is_empty(), "and so does its clock");
        assert_eq!(h.stage_failures, 1, "the skipped lane keeps its streak");
        assert!(!h.stage_since.is_empty(), "and its clock");
        assert_eq!(
            h.apply_failures, 1,
            "a check never vouches for the apply lane"
        );
        assert_eq!(h.kind, "stage", "the standing streak owns the headline");
    }

    /// `kind == ""` is the documented HEALTHY sentinel, so the apply-lane clears must
    /// never blank it while an acquisition streak is still standing — that reported a
    /// healthy class on an unhealthy machine, and `update status` printed a non-zero
    /// `failing=` beside no class at all.
    #[test]
    fn clearing_the_apply_lane_names_the_streak_that_is_still_standing() {
        let p = tmp("apply-clear-names-standing");
        Health::record_failure(&p, "manifest", "bad signature");
        Health::record_apply_failure(&p, 11, 900 + 11, "AdoptionMismatch");
        assert_eq!(Health::read(&p).kind, "apply");

        let h = Health::record_apply_success(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.manifest_failures, 1, "the acquisition streak survives");
        assert_eq!(h.kind, "manifest", "and is NAMED, not blanked to healthy");

        let p = tmp("apply-expire-names-standing");
        Health::record_failure(&p, "pipeline", "asset fetch failed");
        Health::record_apply_failure(&p, 11, 900 + 11, "ChildDied");
        let h = Health::expire_stale_apply_streak(&p, 12);
        assert_eq!(h.apply_failures, 0, "a streak from another build is stale");
        assert_eq!(h.pipeline_failures, 1);
        assert_eq!(h.kind, "pipeline", "named, not blanked");
    }

    /// `failing_since` is any-class: it stamps when the ledger stopped being
    /// clean and does not move again until it is clean.
    #[test]
    fn each_failure_class_carries_its_own_clock() {
        let p = tmp("per-class-clock");
        Health::record_apply_failure(&p, 5, 900 + 5, "ActivityRevoked");
        let after_apply = Health::read(&p);
        assert_eq!(after_apply.apply_failures, 1);
        assert!(!after_apply.failing_since.is_empty());
        let unhealthy_since = after_apply.failing_since.clone();
        assert_eq!(after_apply.class_since("apply"), unhealthy_since);

        Health::record_failure(&p, "manifest", "bad signature");
        let h = Health::read(&p);
        assert_eq!(
            h.failing_since, unhealthy_since,
            "the any-class stamp is sticky until the ledger is clean"
        );
        assert_eq!(h.manifest_failures, 1);
        assert_eq!(
            h.class_since("apply"),
            unhealthy_since,
            "the apply streak still dates from when IT started"
        );
        assert!(
            !h.manifest_since.is_empty(),
            "the manifest class started its own clock"
        );

        Health::record_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.manifest_failures, 0);
        assert!(
            h.manifest_since.is_empty(),
            "a cleared class must not keep a start time"
        );
        assert_eq!(h.apply_failures, 1);
        assert_eq!(h.class_since("apply"), unhealthy_since);

        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert!(h.apply_since.is_empty());
    }

    /// A ledger written BEFORE per-class clocks existed carries no class stamp.
    #[test]
    fn a_pre_upgrade_ledger_falls_back_to_the_any_class_clock() {
        let h = Health {
            manifest_failures: 6,
            failing_since: "2026-08-11T07:16:17Z".to_string(),
            ..Health::default()
        };
        assert!(
            h.manifest_since.is_empty(),
            "the field is new; old files lack it"
        );
        assert_eq!(h.class_since("manifest"), "2026-08-11T07:16:17Z");
        assert_eq!(
            h.class_since("not-a-class"),
            "2026-08-11T07:16:17Z",
            "an unrecognised kind falls back too"
        );
    }

    /// Apply failures are recorded in `apply_failures`, and a healthy download
    /// check clears only the acquisition streaks without erasing the apply streak.
    #[test]
    fn a_healthy_check_never_clears_the_apply_streak() {
        let p = tmp("apply-survives-check");
        Health::record_apply_failure(&p, 77, 900 + 77, "AdoptionMismatch");
        Health::record_apply_failure(&p, 77, 900 + 77, "ActivityRevoked");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 2);
        assert_eq!(h.kind, "apply");
        assert_eq!(h.last_error, "ActivityRevoked");

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

        assert!(
            !h.apply_refusal_applies_to(813),
            "a refusal must not follow the machine into the build that overcame it"
        );

        Health::record_failure(&p, "network", "dns");
        Health::record_success(&p);
        assert_eq!(Health::read(&p).last_apply_refusal, quiet);

        Health::record_apply_failure(&p, 77, 900 + 77, "ChildDied");
        let h = Health::read(&p);
        assert!(!h.apply_refusal_applies_to(812));
        assert!(h.last_apply_refusal.is_empty());
        assert!(h.last_apply_refusal_at.is_empty());
        assert_eq!(h.last_apply_error, "ChildDied");

        Health::record_apply_refusal(&p, 812, "updater work is in flight");
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert!(h.last_apply_refusal.is_empty());
        assert!(h.last_apply_refusal_at.is_empty());
        assert_eq!(h.last_apply_refusal_build, 0);
        assert_eq!(h.apply_failures, 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// TWO COUNTS, BECAUSE THERE ARE TWO QUESTIONS.
    #[test]
    fn the_artifact_scoped_count_restarts_with_the_artifact_and_the_escalation_does_not() {
        let p = tmp("apply-target-scope");
        Health::record_apply_failure(&p, 77, 500, "handoff proof ended ChildDied");
        Health::record_apply_failure(&p, 77, 500, "handoff proof ended ChildDied");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 2);
        assert_eq!(h.apply_failures_for_target, 2);
        assert_eq!(h.last_apply_failure_target_build, 500);

        Health::record_apply_failure(&p, 77, 501, "handoff proof ended TimedOut");
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 3, "the machine still has not moved");
        assert_eq!(
            h.apply_failures_for_target, 1,
            "…but THIS build has been tried exactly once"
        );
        assert_eq!(h.last_apply_failure_target_build, 501);

        Health::expire_stale_apply_streak(&p, 78);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.apply_failures_for_target, 0);
        assert_eq!(h.last_apply_failure_target_build, 0);

        Health::record_apply_failure(&p, 78, 900, "handoff proof ended ChildDied");
        Health::record_apply_success(&p);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures_for_target, 0);
        assert_eq!(h.last_apply_failure_target_build, 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// An apply streak recorded by a different running build is proven stale
    /// and expires; the same build's streak stands.
    #[test]
    fn an_apply_streak_expires_once_a_different_build_is_running() {
        let p = tmp("apply-expiry");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_apply_failure(&p, 77, 900 + 77, "handoff proof ended TimedOut");
        }
        let h = Health::read(&p);
        assert!(h.is_persistent());
        assert_eq!(h.last_apply_failure_build, 77);
        assert!(
            !h.last_apply_failure_at.is_empty(),
            "the lane owns its clock"
        );
        for straggler_or_same in [77, 76] {
            let h = Health::expire_stale_apply_streak(&p, straggler_or_same);
            assert!(h.is_persistent(), "build {straggler_or_same}");
            assert_eq!(h.apply_failures_for_target, PERSISTENT_AFTER);
        }
        Health::expire_stale_apply_streak(&p, 78);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0, "stale streak expires");
        assert!(!h.is_persistent());
        assert!(h.last_apply_error.is_empty());
        assert!(h.last_apply_failure_at.is_empty());
        assert_eq!(h.last_apply_failure_build, 0);
        assert!(
            h.kind.is_empty(),
            "an expired lane stops owning the headline"
        );
        Health::record_failure(&p, "network", "dns");
        Health::record_apply_failure(&p, 78, 900 + 78, "ChildDied");
        Health::expire_stale_apply_streak(&p, 79);
        let h = Health::read(&p);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.network_failures, 1, "the network streak survives");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// Every class that strands the machine (`manifest`, `stage`, `pipeline`)
    /// escalates and names its own count; `network` is transient and never
    /// escalates.
    #[test]
    fn each_stranding_class_escalates_under_its_own_name_and_network_never_does() {
        for (class, escalates) in [
            ("manifest", true),
            ("stage", true),
            ("pipeline", true),
            ("network", false),
        ] {
            let p = tmp(&format!("{class}-persistent"));
            for _ in 0..PERSISTENT_AFTER * 4 {
                Health::record_failure(&p, class, "reason");
            }
            backdate_streaks(&p, PERSISTENT_SPAN * 4);
            let h = Health::read(&p);
            assert_eq!(h.is_persistent(), escalates, "{class}");
            assert_eq!(
                h.persistent_class(),
                escalates.then_some((class, PERSISTENT_AFTER * 4)),
                "{class}"
            );
        }
    }

    /// THREE FAILED CHECKS ARE PERSISTENT ONLY ONCE THEY SPAN HALF AN HOUR. At the
    /// two-minute cadence three failures of a check class come 0, 2 and 6 minutes
    /// apart — a GitHub incident or a slow asset upload, not the broken lane the
    /// status, the update screen's headline and the loud notice promise.
    #[test]
    fn a_check_class_streak_is_persistent_only_once_it_spans_half_an_hour() {
        let streak = |class: &str, count: u32, newest: &str| {
            let mut h = Health {
                last_failure_at: newest.to_string(),
                last_diagnosis_at: newest.to_string(),
                kind: class.to_string(),
                ..Health::default()
            };
            let (n, since) = h.class_streak_mut(class).expect("a ledger class");
            *n = count;
            *since = "2026-10-15T10:00:00Z".to_string();
            h
        };
        for class in ["pipeline", "manifest", "stage"] {
            let quick = streak(class, PERSISTENT_AFTER, "2026-10-15T10:06:00Z");
            assert_eq!(
                quick.persistent_class(),
                None,
                "{class}: three inside 7 minutes"
            );
            let long = streak(class, PERSISTENT_AFTER, "2026-10-15T10:30:00Z");
            assert_eq!(
                long.persistent_class(),
                Some((class, PERSISTENT_AFTER)),
                "{class}: three spanning 30 minutes"
            );
            let few = streak(class, PERSISTENT_AFTER - 1, "2026-10-15T14:00:00Z");
            assert_eq!(few.persistent_class(), None, "{class}: two in four hours");
            let mut blipped = quick.clone();
            blipped.network_failures = 1;
            blipped.last_failure_at = "2026-10-15T18:00:00Z".to_string();
            assert_eq!(
                blipped.persistent_class(),
                None,
                "{class}: weather stretches nothing"
            );
            let mut older = long.clone();
            older.last_diagnosis_at.clear();
            assert_eq!(older.persistent_class(), None);
            older.last_failure_at = "2026-10-15T10:06:00Z".to_string();
            assert_eq!(older.persistent_class(), None);
        }
        let apply = streak("apply", PERSISTENT_AFTER, "2026-10-15T10:06:00Z");
        assert_eq!(apply.persistent_class(), Some(("apply", PERSISTENT_AFTER)));

        let p = tmp("persistent-span");
        for _ in 0..PERSISTENT_AFTER {
            Health::record_failure(&p, "pipeline", "asset fetch failed");
        }
        let h = Health::read(&p);
        assert_eq!(h.pipeline_failures, PERSISTENT_AFTER);
        assert!(!h.is_persistent(), "{h:?}");
        assert_eq!(
            h.last_diagnosis_at, h.last_failure_at,
            "the class's own clock"
        );
        backdate_streaks(&p, PERSISTENT_SPAN);
        assert_eq!(Health::read(&p).persistent_class(), Some(("pipeline", 3)));
        let h = Health::record_success(&p);
        assert!(
            h.last_diagnosis_at.is_empty() && !h.is_persistent(),
            "{h:?}"
        );
    }

    /// A standing class label does not identify the last failure's timestamp.
    #[test]
    fn a_relabelled_legacy_diagnosis_has_no_proven_end_stamp() {
        let captured = format!(
            r#"kind = "stage"
failing_since = "2026-10-15T10:00:00Z"
last_failure_at = "2026-10-15T18:00:00Z"
last_error = "standing stage verdict"
last_diagnosis_error = "standing stage verdict"
stage_failures = {PERSISTENT_AFTER}
stage_since = "2026-10-15T10:00:00Z"
"#
        );
        let legacy: Health = aterm_toml::from_str(&captured).unwrap();
        let mut known_long = legacy.clone();
        known_long.last_diagnosis_at = "2026-10-15T10:30:00Z".into();
        assert_eq!(
            known_long.persistent_class(),
            Some(("stage", PERSISTENT_AFTER)),
            "a dedicated half-hour span still escalates"
        );
        let mut known_quick = known_long.clone();
        known_quick.last_diagnosis_at = "2026-10-15T10:06:00Z".into();
        assert_eq!(
            known_quick.persistent_class(),
            None,
            "weather extends nothing"
        );
        let apply: Health = aterm_toml::from_str(&format!(
            r#"kind = "apply"
apply_failures = {PERSISTENT_AFTER}
"#
        ))
        .unwrap();
        assert_eq!(apply.persistent_class(), Some(("apply", PERSISTENT_AFTER)));
        let mut weather_label = legacy.clone();
        weather_label.kind = "network".into();
        let weather = weather_label.persistent_class();
        Health::relabel_by_standing(&mut weather_label);
        assert_eq!(weather_label.kind, "stage");
        assert_eq!(weather_label.last_failure_at, legacy.last_failure_at);
        assert_eq!(
            [
                legacy.persistent_class(),
                weather,
                weather_label.persistent_class()
            ],
            [None, None, None],
            "a legacy class label is not a diagnosis end stamp"
        );
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
        Health::record_failure(&p, "network", "blip");
        backdate_streaks(&p, PERSISTENT_SPAN);
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
        assert!(!p.exists());
        let h = Health::record_success(&p);
        assert_eq!(h.total_failures(), 0);
        assert!(
            !p.exists(),
            "success on an already-clean ledger must not write the file"
        );

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

    /// A PASSING REFUSAL LONGER THAN THE CAP IS STILL A PASSING REFUSAL WHEN STORED.
    #[test]
    fn a_long_passing_refusal_keeps_its_key_in_the_ledger() {
        let p = tmp("long-passing");
        let attempt = "hdiutil attach did not finish within 120s; treating as a failure";
        let error = crate::verify::mark_passing(format!(
            "hdiutil attach failed after 4 attempts (/Users//someone/Library/Application \
             Support/aterm/Updates/download/aterm-0.93.0.dmg, 52428800 bytes): {}",
            [attempt; 4].join(" | ")
        ));
        assert!(error.chars().count() > 400, "{}", error.len());
        assert!(crate::is_passing_refusal(&error));
        for _ in 0..crate::PERSISTENT_AFTER {
            Health::record_failure(&p, "stage", &error);
        }
        let h = Health::read(&p);
        assert!(h.last_error.chars().count() <= 400, "still capped");
        assert!(
            h.last_error
                .starts_with("hdiutil attach failed after 4 attempts")
        );
        assert!(crate::is_passing_refusal(&h.last_error), "{}", h.last_error);
        let h = Health::record_failure(&p, "network", "could not resolve the host");
        assert!(h.stage_failures >= crate::PERSISTENT_AFTER, "{h:?}");
        assert!(
            crate::is_passing_refusal(&h.last_diagnosis_error),
            "{}",
            h.last_diagnosis_error
        );
        let h = Health::record_apply_failure(&p, 1, 2, &error);
        assert!(crate::is_passing_refusal(&h.last_apply_error));
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// NO SPLICED COUNT: a ledger carrying a dead, clock-less `manifest` streak of 999
    /// meets this build's `pipeline` failures.
    #[test]
    fn a_pipeline_failure_clears_its_sibling_diagnoses_and_the_count_is_its_own() {
        let p = tmp("sibling-clear");
        let legacy = Health {
            manifest_failures: 999,
            pipeline_failures: 19,
            kind: "pipeline".to_string(),
            failing_since: "2026-08-27T22:04:36Z".to_string(),
            ..Health::default()
        };
        legacy.write(&p);
        let h = Health::record_failure(&p, "pipeline", "HTTP 404 on the appcast");
        assert_eq!(h.manifest_failures, 0, "the sibling diagnosis is retired");
        assert!(h.manifest_since.is_empty());
        assert_eq!(
            h.pipeline_failures, 1,
            "a clock-less count is another build's; this ledger's streak starts at one"
        );
        assert!(
            !h.pipeline_since.is_empty() && h.pipeline_since != "2026-08-27T22:04:36Z",
            "the pipeline clock is its own, not the any-class fallback: {}",
            h.pipeline_since
        );
        assert_eq!(
            h.acquisition_failures(),
            1,
            "the count is the standing class's"
        );
        assert_eq!(h.standing_acquisition_class(), Some("pipeline"));

        Health::record_failure(&p, "pipeline", "again");
        let h = Health::record_failure(&p, "network", "dns");
        assert_eq!(h.pipeline_failures, 2);
        assert_eq!(
            h.acquisition_failures(),
            2,
            "the pipeline streak outranks the blip and is reported alone, not summed"
        );
        assert_eq!(h.kind, "network", "the last failure was the blip");
        assert_eq!(
            h.standing_acquisition_class(),
            Some("pipeline"),
            "the label beside the count is the standing class, not the last blip"
        );
        assert_eq!(h.network_failures, 1);
        assert_ne!(
            (h.acquisition_failures(), h.kind.as_str()),
            (h.network_failures, "network"),
            "count and kind must not be read as one pair"
        );
        let h = Health::record_failure(&p, "manifest", "bad signature");
        assert_eq!((h.pipeline_failures, h.manifest_failures), (0, 1));
        assert!(h.pipeline_since.is_empty());
        assert_eq!(h.acquisition_failures(), 1);
        assert_eq!(
            h.network_failures, 1,
            "weather is neither reset nor summed in"
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// NO SPLICED REASON. Counts and clocks are per class, but there is ONE
    /// `last_error`, and it belongs to whichever failure landed LAST.
    #[test]
    fn a_standing_diagnosis_keeps_its_own_reason_across_a_blip() {
        let p = tmp("diagnosis-reason");
        let stage_reason =
            "staged bundle failed verification: codesign --verify (team-pinned requirement) failed";
        Health::record_failure(&p, "stage", stage_reason);
        Health::record_failure(&p, "stage", stage_reason);
        Health::record_failure(&p, "stage", stage_reason);
        backdate_streaks(&p, PERSISTENT_SPAN);
        let h = Health::record_failure(&p, "network", "curl: (6) could not resolve host");
        assert_eq!(h.standing_acquisition_class(), Some("stage"));
        assert_eq!(h.persistent_class(), Some(("stage", 3)));

        let h = Health::record_acquisition_success(&p);
        assert_eq!(h.kind, "stage");
        assert_eq!(h.stage_failures, 3);
        assert!(
            h.last_error.contains("codesign"),
            "the standing stage diagnosis is described by a network blip: {:?}",
            h.last_error
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// The same splice from the apply-expiry side: `expire_stale_apply_streak` blanks
    /// `last_error` while NAMING a standing acquisition class, so the status line
    /// would read `failing=1:stage` with no reason at all.
    #[test]
    fn expiring_a_stale_apply_streak_leaves_the_standing_diagnosis_its_reason() {
        let p = tmp("expire-keeps-reason");
        let stage_reason = "ditto zip extract failed (exit status: 1)";
        Health::record_failure(&p, "stage", stage_reason);
        Health::record_apply_failure(&p, 11, 900 + 11, "ChildDied");
        assert_eq!(Health::read(&p).kind, "apply");
        let h = Health::expire_stale_apply_streak(&p, 12);
        assert_eq!(h.apply_failures, 0);
        assert_eq!(h.kind, "stage", "named, not blanked");
        assert!(
            h.last_error.contains("ditto"),
            "a named standing class must carry its reason, not an empty string: {:?}",
            h.last_error
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    /// THE PENDING CLOCK SURVIVES RE-STAGES (`ready.toml`'s `staged_at` does not).
    #[test]
    fn the_pending_clock_is_the_first_sighting_and_survives_a_restage() {
        let p = tmp("pending-clock");
        let first = Health::note_pending_update(&p, 10, Some(11));
        assert_eq!(first.pending_build, 11);
        assert!(
            !first.pending_since.is_empty(),
            "the first sighting is stamped"
        );
        let mut aged = Health::read(&p);
        aged.pending_since = "2026-09-22T00:10:45Z".to_string();
        aged.write(&p);
        let again = Health::note_pending_update(&p, 10, Some(11));
        assert_eq!(
            again.pending_since, "2026-09-22T00:10:45Z",
            "re-observing the same build must never move its clock"
        );
        let gone = Health::note_pending_update(&p, 10, None);
        assert_eq!(
            (gone.pending_build, gone.pending_since.as_str()),
            (11, "2026-09-22T00:10:45Z")
        );
        let stale = Health::note_pending_update(&p, 10, Some(10));
        assert_eq!(
            stale.pending_build, 11,
            "a non-newer observation changes nothing"
        );
        let newer = Health::note_pending_update(&p, 10, Some(12));
        assert_eq!(newer.pending_build, 12);
        assert_ne!(newer.pending_since, "2026-09-22T00:10:45Z");
        let landed = Health::note_pending_update(&p, 12, None);
        assert_eq!(
            (landed.pending_build, landed.pending_since.as_str()),
            (0, "")
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
