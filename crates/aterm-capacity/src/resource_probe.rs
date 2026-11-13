// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Rejection-only observations for one ordinary resource-dispatch pass.

use super::*;

/// A probe cannot authorize a start. `Possible` always requires fresh ordinary admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeDecision {
    /// This observation rejects the envelope. It authorizes no operation.
    Blocked,
    /// Consult fresh admission, including for unavailable or unsupported observations.
    Possible,
}

/// The rejection obtained from the shared observation. This is diagnostic evidence,
/// never custody or authority to start work.
#[derive(Clone, Debug, PartialEq)]
pub enum ProbeRejection {
    /// The existing admission arithmetic found these unavailable resources.
    Waiting(Shortfall),
    /// The requested envelope cannot fit the observed machine.
    Unreachable(String),
}

/// Maximum age of an observation reused within one dispatcher generation.
pub const RESOURCE_PROBE_MAX_AGE: Duration = Duration::from_millis(500);

/// A bounded resource observation, never a claim or an admission certificate.
///
/// Callers discard it after a successful acquisition or a dispatcher generation change.
/// Missing observations, unsupported needs and expiry all return [`ProbeDecision::Possible`],
/// delegating to fresh admission rather than inferring a grant or a refusal.
pub struct ResourceProbe {
    captured: Instant,
    now: u64,
    pid: u32,
    reading: Reading,
    records: Vec<Record>,
    used: ProcessUse,
}

impl ResourceProbe {
    /// Observe ordinary holder admission without entering a waiter or writing a claim.
    /// `None` means the caller must use the original admission path.
    ///
    /// # Errors
    /// A ledger read or admission-turn check failed. No claim was created.
    pub fn capture(ledger: &Ledger, machine: &dyn Machine) -> io::Result<Option<Self>> {
        Self::capture_at(ledger, machine, Instant::now())
    }

    /// Capture with an explicit monotonic observation start, for callers that supply their
    /// clock. Production callers must use the actual current instant; the result still grants
    /// no authority. Check against a fresh instant after any slow observation completes.
    ///
    /// # Errors
    /// A ledger read or admission-turn check failed. No claim was created.
    pub fn capture_at(
        ledger: &Ledger,
        machine: &dyn Machine,
        at: Instant,
    ) -> io::Result<Option<Self>> {
        if machine.release_run() || ledger.cohort_aware()? {
            return Ok(None);
        }
        let Ok(_turn) = ledger.try_turn()? else {
            return Ok(None);
        };
        let captured = at;
        let now = now_ms();
        let reading = machine.read(None);
        if reading
            .available
            .zip(reading.physical)
            .is_none_or(|(available, physical)| physical == 0 || available > physical)
            || reading.cores == 0
            || reading
                .wired
                .zip(reading.physical)
                .is_some_and(|(wired, physical)| wired >= physical)
            || [reading.load1, reading.load5]
                .into_iter()
                .flatten()
                .any(|load| !load.is_finite() || load < 0.0)
        {
            return Ok(None);
        }
        let records = ledger.scan(false)?;
        if records
            .iter()
            .any(|record| record.accounting != crate::Accounting::Holder)
        {
            return Ok(None);
        }
        let procs = if records.is_empty() {
            None
        } else {
            let Some(procs) = machine.procs() else {
                return Ok(None);
            };
            if !std::iter::once(std::process::id())
                .chain(records.iter().flat_map(|record| {
                    std::iter::once(record.pid).chain(record.credit.iter().copied())
                }))
                .all(|pid| procs.observes_resources(pid))
            {
                return Ok(None);
            }
            Some(procs)
        };
        let pid = std::process::id();
        let records: Vec<_> = counted(
            records.iter().collect(),
            pid,
            procs.as_ref(),
            reading.available,
        )
        .into_iter()
        .cloned()
        .collect();
        let refs: Vec<_> = records.iter().collect();
        let used = process_use(&refs, pid, procs.as_ref());
        Ok(Some(Self {
            captured,
            now,
            pid,
            reading,
            records,
            used,
        }))
    }

    /// Whether the caller must obtain a new observation for its next pass.
    #[must_use]
    pub fn expired(&self) -> bool {
        self.expired_at(Instant::now())
    }

    fn expired_at(&self, at: Instant) -> bool {
        at.saturating_duration_since(self.captured) >= RESOURCE_PROBE_MAX_AGE
    }

    /// Reject an ordinary memory/core envelope against this pass's observation.
    /// This result never replaces the current ledger/custody checks of `try_admit_with`.
    #[must_use]
    pub fn check(&self, need: &Need) -> ProbeDecision {
        self.check_at(need, Instant::now())
    }

    /// Check against the caller's current monotonic instant. Explicit clocks make expiry
    /// deterministic without changing the production observation's 500 ms lifetime.
    #[must_use]
    pub fn check_at(&self, need: &Need, at: Instant) -> ProbeDecision {
        if self.rejection_at(need, at).is_some() {
            ProbeDecision::Blocked
        } else {
            ProbeDecision::Possible
        }
    }

    /// Retain the same bounded rejection used by [`Self::check_at`] for wait attribution.
    /// `None` requires fresh admission, including for unsupported or expired observations.
    #[must_use]
    pub fn rejection_at(&self, need: &Need, at: Instant) -> Option<ProbeRejection> {
        // An exact default envelope check makes future Need fields opt out automatically.
        let mut supported = Need::new(need.what.clone()).mem(need.mem).cores(need.cores);
        supported.alone = need.alone;
        supported.covers = need.covers;
        if self.expired_at(at)
            || need != &supported
            || (need.cores > 0 && self.reading.load5.is_none())
        {
            return None;
        }
        if let Some(why) = never(need, &self.reading) {
            return Some(ProbeRejection::Unreachable(why));
        }
        let refs: Vec<_> = self.records.iter().collect();
        let others = others(&refs, self.used.claimants.clone(), &self.used.trees);
        let mine = (self.now, self.pid, u64::MAX);
        decide(need, mine, &self.reading, &others, self.now)
            .err()
            .map(ProbeRejection::Waiting)
    }
}

#[cfg(test)]
#[path = "resource_probe_tests.rs"]
mod tests;
