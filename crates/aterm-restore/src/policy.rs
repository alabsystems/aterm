// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Crash-restore keyframe scheduling policy (`KeyframePolicy`).

/// Idle duration in milliseconds before a dirty session keyframes (`K1`).
pub const QUIET_MS: u64 = 200;
/// Maximum interval in milliseconds between keyframes on a continuously active session (`K2`).
pub const PERIOD_MS: u64 = 1_000;
/// Minimum interval in milliseconds between keyframes under tail pressure or shell marks (`K3`, `K6`).
pub const PRESSURE_GAP_MS: u64 = 100;
/// Default scrollback bridge line cap for normal keyframes.
pub const BRIDGE_CAP: usize = 512;
/// Reduced scrollback bridge line cap under tail pressure (`K3`) or regrid (`K7`).
pub const PRESSURE_BRIDGE_CAP: usize = 256;

/// Reason a crash-restore keyframe is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Session became quiescent after a burst (`K1`).
    Quiescence,
    /// Periodic 1-second interval elapsed during continuous output (`K2`).
    Periodic,
    /// Tail ring filled at least half its capacity since the last keyframe (`K3`).
    TailPressure,
    /// Pending history clear (`ED3` / `RIS`) requires a forced keyframe (`K4`).
    Clear,
    /// Engine configuration fingerprint changed (`K4`).
    Forced,
    /// Host renumbered scrollback history without new output (`K4`).
    Renumbered,
    /// Slice verified an OSC 133/633 shell-integration mark (`K6`).
    Mark,
    /// Resize changed column width or grew row count (`K7`).
    Regrid,
}

impl Reason {
    /// Scrollback bridge line cap for this keyframe reason.
    #[inline]
    #[must_use]
    pub const fn bridge_cap(self) -> usize {
        match self {
            Self::TailPressure | Self::Regrid => PRESSURE_BRIDGE_CAP,
            Self::Quiescence
            | Self::Periodic
            | Self::Clear
            | Self::Forced
            | Self::Renumbered
            | Self::Mark => BRIDGE_CAP,
        }
    }
}

/// Policy decision returned after a slice or quiet timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decision {
    /// Due keyframe reason, if any.
    pub keyframe: Option<Reason>,
}

/// Per-session crash-restore keyframe scheduling state machine.
#[derive(Debug, Clone)]
pub struct KeyframePolicy {
    /// The tail ring's capacity (K3 weighs the tail against half of it).
    tail_cap: u64,
    /// Something reached the tail since the last keyframe.
    dirty: bool,
    /// When the last batch was processed.
    last_batch_ms: u64,
    /// When the last keyframe was written (`None` before the first).
    last_keyframe_ms: Option<u64>,
    /// A clear is pending its forced keyframe (K4).
    clear_pending: bool,
    /// The config fingerprint the last keyframe was taken under, and the current one.
    config_fp_at_keyframe: u64,
    config_fp: u64,
    /// The keyframe the capture deferred (K5), retried at the next slice end.
    deferred: Option<Reason>,
    /// A width change or a rows grow reached the tail since the last keyframe (K7).
    regrid_pending: bool,
    /// A K5 deferral parked the session: it reads clean to the parse
    /// thread's wait until the next slice or resize reaches the tail.
    parked: bool,
    /// The host renumbered the history with no output ([`Reason::Renumbered`]).
    renumbered: bool,
}

impl KeyframePolicy {
    /// Create a policy for a tail ring of `tail_cap` bytes with an initial keyframe at `now_ms`.
    #[must_use]
    pub fn new(tail_cap: usize, now_ms: u64, config_fp: u64) -> Self {
        Self {
            tail_cap: u64::try_from(tail_cap).unwrap_or(u64::MAX),
            dirty: false,
            last_batch_ms: now_ms,
            last_keyframe_ms: Some(now_ms),
            clear_pending: false,
            config_fp_at_keyframe: config_fp,
            config_fp,
            deferred: None,
            regrid_pending: false,
            parked: false,
            renumbered: false,
        }
    }

    /// Whether unkeyframed tail data is awaiting a quiet timeout.
    #[inline]
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty && !self.parked
    }

    /// Record a history clear (`ED3` / `RIS`) to force a keyframe.
    #[inline]
    pub const fn note_clear(&mut self) {
        self.clear_pending = true;
    }

    /// Record a scrollback renumbering to force a keyframe.
    #[inline]
    pub const fn note_renumbered(&mut self) {
        self.renumbered = true;
    }

    /// Update the engine configuration fingerprint, forcing a keyframe if changed.
    #[inline]
    pub const fn note_config(&mut self, config_fp: u64) {
        self.config_fp = config_fp;
    }

    /// Record a terminal resize at `now_ms`, flagging a regrid keyframe when `regridded` is `true`.
    #[inline]
    pub const fn note_resize(&mut self, now_ms: u64, regridded: bool) {
        self.dirty = true;
        self.parked = false;
        self.last_batch_ms = now_ms;
        self.regrid_pending |= regridded;
    }

    #[inline]
    const fn since_keyframe(&self, now_ms: u64) -> u64 {
        match self.last_keyframe_ms {
            Some(at) => now_ms.saturating_sub(at),
            None => u64::MAX,
        }
    }

    /// A forced or deferred keyframe, which no rate applies to. A pending clear comes
    /// first, so the keyframe its settle follows is never reported as another reason.
    #[inline]
    const fn owed(&self) -> Option<Reason> {
        if self.clear_pending {
            Some(Reason::Clear)
        } else if self.config_fp != self.config_fp_at_keyframe {
            Some(Reason::Forced)
        } else if self.renumbered {
            Some(Reason::Renumbered)
        } else {
            self.deferred
        }
    }

    /// Evaluate whether a keyframe is due after appending a slice at `now_ms`.
    pub const fn after_slice(
        &mut self,
        now_ms: u64,
        tail_since_keyframe: u64,
        marks_verified: u64,
    ) -> Decision {
        self.dirty = true;
        self.parked = false;
        self.last_batch_ms = now_ms;
        if let Some(reason) = self.owed() {
            return Decision {
                keyframe: Some(reason),
            };
        }
        let gap_ok = self.since_keyframe(now_ms) >= PRESSURE_GAP_MS;
        let keyframe = if gap_ok && marks_verified > 0 {
            Some(Reason::Mark)
        } else if gap_ok && self.regrid_pending {
            Some(Reason::Regrid)
        } else if gap_ok && tail_since_keyframe >= self.tail_cap / 2 {
            Some(Reason::TailPressure)
        } else if self.since_keyframe(now_ms) >= PERIOD_MS {
            Some(Reason::Periodic)
        } else {
            None
        };
        Decision { keyframe }
    }

    /// Evaluate whether a keyframe is due when the quiet wait times out at `now_ms`.
    pub const fn on_quiet(&mut self, now_ms: u64) -> Decision {
        let keyframe = if let Some(reason) = self.owed() {
            Some(reason)
        } else if self.dirty && now_ms.saturating_sub(self.last_batch_ms) >= QUIET_MS {
            Some(Reason::Quiescence)
        } else {
            None
        };
        Decision { keyframe }
    }

    /// Record that a keyframe was written at `now_ms`.
    #[inline]
    pub const fn keyframe_written(&mut self, now_ms: u64) {
        self.dirty = false;
        self.last_keyframe_ms = Some(now_ms);
        self.clear_pending = false;
        self.config_fp_at_keyframe = self.config_fp;
        self.deferred = None;
        self.regrid_pending = false;
        self.parked = false;
        self.renumbered = false;
    }

    /// Defer a keyframe whose capture returned `None` (`K5`) until the next slice.
    #[inline]
    pub const fn keyframe_deferred(&mut self, reason: Reason) {
        self.deferred = Some(reason);
        self.parked = true;
    }

    /// Re-queue a captured keyframe that could not be written so the next quiet wait retries it.
    #[inline]
    pub const fn keyframe_retry(&mut self, reason: Reason) {
        self.deferred = Some(reason);
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
