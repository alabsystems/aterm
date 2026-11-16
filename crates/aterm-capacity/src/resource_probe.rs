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
    /// No observation: another admission held the ledger's lock when one was to be taken
    /// ([`Capture::Turn`]), its pid where it wrote one. It decides nothing about the envelope;
    /// a fresh admission would have met the same lock and read nothing either.
    Turn(Option<u32>),
}

/// What one try at a shared observation found ([`ResourceProbe::capture_at`]).
pub enum Capture {
    /// The observation, which may reject envelopes until it expires.
    Observed(Box<ResourceProbe>),
    /// Another admission held the ledger's lock at that instant, its pid where it wrote one:
    /// nothing was read, no claim was laid, and a later try may observe.
    Turn(Option<u32>),
    /// The ledger or the machine answered nothing a probe judges by (a release run, a cohort's
    /// ledger, an unread or inconsistent reading, a claim of another accounting, a process whose
    /// resources cannot be observed): every envelope takes fresh admission.
    Unsupported,
}

impl Capture {
    /// The observation, where one was taken.
    #[must_use]
    pub fn observed(self) -> Option<ResourceProbe> {
        match self {
            Capture::Observed(probe) => Some(*probe),
            Capture::Turn(_) | Capture::Unsupported => None,
        }
    }
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
    /// The ledger it read: a [`Place`] is one of this ledger's or none.
    dir: PathBuf,
    reading: Reading,
    records: Vec<Record>,
    used: ProcessUse,
    /// What every look reads of every claim the probe read, and of the probe's own need as the
    /// claim its look would read ([`Ledgerwide`]).
    wide: Ledgerwide,
}

impl ResourceProbe {
    /// Observe ordinary holder admission without entering a waiter or writing a claim
    /// ([`Self::capture_at`] now).
    ///
    /// # Errors
    /// A ledger read or admission-turn check failed. No claim was created.
    pub fn capture(ledger: &Ledger, machine: &dyn Machine) -> io::Result<Capture> {
        Self::capture_at(ledger, machine, Instant::now())
    }

    /// Capture with an explicit monotonic observation start, for callers that supply their
    /// clock. Production callers must use the actual current instant; the result still grants
    /// no authority. Check against a fresh instant after any slow observation completes.
    ///
    /// THREE ANSWERS: the observation ([`Capture::Observed`]); the ledger's lock held by
    /// another admission at that instant ([`Capture::Turn`], its holder named), which costs one
    /// try of the lock and reads nothing else; or nothing a probe judges by
    /// ([`Capture::Unsupported`]), where the caller must use the original admission path.
    ///
    /// # Errors
    /// A ledger read or admission-turn check failed. No claim was created.
    pub fn capture_at(ledger: &Ledger, machine: &dyn Machine, at: Instant) -> io::Result<Capture> {
        if machine.release_run() || ledger.cohort_aware()? {
            return Ok(Capture::Unsupported);
        }
        let _turn = match ledger.try_turn()? {
            Ok(turn) => turn,
            Err(holder) => return Ok(Capture::Turn(holder)),
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
            return Ok(Capture::Unsupported);
        }
        let records = ledger.scan(false)?;
        if records
            .iter()
            .any(|record| record.accounting != crate::Accounting::Holder)
        {
            return Ok(Capture::Unsupported);
        }
        let procs = if records.is_empty() {
            None
        } else {
            let Some(procs) = machine.procs() else {
                return Ok(Capture::Unsupported);
            };
            if !std::iter::once(std::process::id())
                .chain(records.iter().flat_map(|record| {
                    std::iter::once(record.pid).chain(record.credit.iter().copied())
                }))
                .all(|pid| procs.observes_resources(pid))
            {
                return Ok(Capture::Unsupported);
            }
            Some(procs)
        };
        let pid = std::process::id();
        // A stand-in for the waiting claim each need checked against this look would be, asked
        // last: one claim more of its process's, whose line every lead's fit reads
        // ([`ledgerwide`]). It asks for nothing and leads nothing, so it reserves nothing and no
        // waiter counts it: it moves another claim's standing only as that waiting claim would, by
        // its process's claim count (a held claim of its process no longer gives back the rest of
        // the tree at its end, [`Other::sole`]) and its line in each lead's fit; each need is
        // judged against the look in `check_at`.
        let asking = Record {
            accounting: crate::Accounting::Holder,
            pid,
            start_ms: now,
            laid_at: None,
            seq: u64::MAX,
            checkout: String::new(),
            need: Need::new("probe"),
            timing: crate::TimingEvidence::default(),
            held: None,
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        };
        let LookRead {
            counted: records,
            used,
            wide,
        } = look_read(
            &records
                .iter()
                .chain(std::iter::once(&asking))
                .collect::<Vec<_>>(),
            &asking,
            procs.as_ref(),
            &reading,
        );
        let records: Vec<_> = records.into_iter().cloned().collect();
        Ok(Capture::Observed(Box::new(Self {
            captured,
            now,
            pid,
            dir: ledger.dir().to_path_buf(),
            reading,
            records,
            used,
            wide,
        })))
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
        self.rejection_placed(need, at, None)
    }

    /// [`Self::rejection_at`] for a need its caller would enter at `place`
    /// ([`Waiter::enter_at`]): judged behind the claims ahead of that place where a record of
    /// this process still stands at it in the ledger this probe read, else as one laid now. A
    /// need judged as laid now would be rejected behind every large waiter laid since the
    /// place, which an entry at the place is ahead of.
    #[must_use]
    pub fn rejection_placed(
        &self,
        need: &Need,
        at: Instant,
        place: Option<&Place>,
    ) -> Option<ProbeRejection> {
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
        let others = others(
            &refs,
            self.used.claimants.clone(),
            &self.used.trees,
            &self.used.lines,
            &self.wide,
        );
        let placed = place
            .and_then(|place| place.standing_in(&self.dir))
            .filter(|stood| stood.pid == self.pid)
            .map_or(self.now, |stood| stood.ms.min(self.now));
        let mine = (placed, self.pid, u64::MAX);
        decide(need, mine, &self.reading, &others, self.now)
            .err()
            .map(ProbeRejection::Waiting)
    }
}

#[cfg(test)]
#[path = "resource_probe_tests.rs"]
mod tests;
