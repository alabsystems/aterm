// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable self-healing updater health ledger (`Updates/health.toml`).

use aterm_update_core::FileLock;
use serde::{Deserialize, Serialize};
use std::path::Path;

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
    /// Diagnostic context for the refusal, never part of its policy classification.
    #[serde(default)]
    pub last_apply_refusal_detail: String,
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

const MAX_STORED_ERROR_CHARS: usize = 400;

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

/// Quote bounded diagnostic context without classifying any words it contains.
#[must_use]
pub(crate) fn diagnostic_detail(detail: &str) -> String {
    aterm_codec::tty::one_line(detail, crate::MAX_APPLY_REFUSAL_DETAIL_CHARS)
        .chars()
        .take(crate::MAX_APPLY_REFUSAL_DETAIL_CHARS)
        .collect()
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

    /// Consecutive failed checks across acquisition classes (`network`, `pipeline`, `manifest`,
    /// `stage`).
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

    /// Highest-priority failure class whose streak has crossed both persistence gates, with its
    /// count.
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

    /// Highest-priority standing acquisition failure class (`pipeline`, `manifest`, `stage`,
    /// `network`).
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
        if h.stage_failures == 0 {
            h.last_diagnosis_error.clear();
            h.last_diagnosis_at.clear();
        }
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

    /// Clear the apply failure streak if `current_build` has moved past
    /// [`Self::last_apply_failure_build`]: the escalation streak and its clock, and the record of
    /// the last failure with its per-target count.
    pub fn expire_stale_apply_streak(path: &Path, current_build: u64) -> Self {
        Self::expire_apply_streak(path, current_build, false)
    }

    /// [`Self::expire_stale_apply_streak`] for a build whose own boot trial is still armed: the
    /// escalation streak and its clock expire as they would, so [`Self::is_persistent`] and every
    /// notice read from it say what they would, but a failed switch to `current_build` itself
    /// keeps its record (the build that failed, the target, their time and reason, and the
    /// per-target count). A window that could not reach the build on trial booked it, and the
    /// install over that activation reads it; the build itself has moved past nothing. Once the
    /// trial settles, [`Self::expire_stale_apply_streak`] clears that record too.
    pub fn expire_stale_apply_streak_on_trial(path: &Path, current_build: u64) -> Self {
        Self::expire_apply_streak(path, current_build, true)
    }

    fn expire_apply_streak(path: &Path, current_build: u64, on_trial: bool) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if (h.apply_failures == 0 && h.apply_failures_for_target == 0)
            || h.last_apply_failure_build == current_build
        {
            return h;
        }
        if h.last_apply_failure_build != 0 && current_build < h.last_apply_failure_build {
            return h;
        }
        let keeps_switch = on_trial && h.last_apply_failure_target_build == current_build;
        if keeps_switch && h.apply_failures == 0 {
            return h;
        }
        h.apply_failures = 0;
        h.apply_since = String::new();
        if !keeps_switch {
            h.last_apply_error = String::new();
            h.last_apply_failure_at = String::new();
            h.last_apply_failure_build = 0;
            h.apply_failures_for_target = 0;
            h.last_apply_failure_target_build = 0;
        }
        if h.total_failures() == 0 || h.kind == "apply" {
            Self::relabel_by_standing(&mut h);
        }
        h.write(path);
        h
    }

    /// Record an apply-lane refusal observed by `current_build`.
    pub fn record_apply_refusal(path: &Path, current_build: u64, reason: &str) -> Self {
        Self::record_apply_refusal_with_detail(path, current_build, reason, "")
    }

    /// Record the standing reason and separate bounded diagnostic context in one write.
    pub fn record_apply_refusal_with_detail(
        path: &Path,
        current_build: u64,
        reason: &str,
        detail: &str,
    ) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        h.last_apply_refusal = capped_error(reason);
        h.last_apply_refusal_detail = if reason.is_empty() {
            String::new()
        } else {
            detail
                .chars()
                .take(crate::MAX_APPLY_REFUSAL_DETAIL_CHARS)
                .collect()
        };
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

    /// Record that the trial whose proof was refused was settled at its bound without it
    /// (`install::settle_trial_at_bound`): the refusal no longer stands. Nothing else moves;
    /// an unproven settle is no apply success, and the failure counters are not its to clear.
    pub(crate) fn record_trial_settled_unproven(path: &Path) {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.trial_proof_failure.is_empty()
            && h.trial_proof_failure_at.is_empty()
            && h.trial_proof_failure_build == 0
        {
            return;
        }
        h.trial_proof_failure = String::new();
        h.trial_proof_failure_at = String::new();
        h.trial_proof_failure_build = 0;
        h.write(path);
    }

    fn clear_apply_refusal(&mut self) {
        self.last_apply_refusal = String::new();
        self.last_apply_refusal_detail = String::new();
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

    /// Record that an update apply succeeded, clearing `apply` failure, refusal, and trial-proof state.
    pub fn record_apply_success(path: &Path) -> Self {
        let _lock = Self::lock(path);
        let mut h = Self::read(path);
        if h.apply_failures == 0
            && h.apply_failures_for_target == 0
            && h.last_apply_refusal.is_empty()
            && h.last_apply_refusal_detail.is_empty()
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

    fn lock(path: &Path) -> Option<FileLock> {
        FileLock::acquire_within(&path.with_extension("toml.lock"), HEALTH_LOCK_WAIT).ok()
    }

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
        if crate::install::write_private_file(&tmp, text.as_bytes()).is_err()
            || std::fs::rename(&tmp, path).is_err()
        {
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
#[path = "health_tests.rs"]
mod tests;
