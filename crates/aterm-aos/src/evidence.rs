// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication evidence grades and witnesses.

use crate::contract::Verdict;
use crate::ident::{ArtifactId, Digest};
use crate::publish::Head;

/// What a witness establishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvidenceGrade {
    /// Shadow replay over one recorded trace, plus the test suite. Not a proof.
    Tested,
}

/// The assumptions every [`EvidenceGrade::Tested`] witness rests on.
pub const TESTED_ASSUMPTIONS: [&str; 5] = [
    "the log is the complete, ordered input history from the ancestor checkpoint to the paused head",
    "each step is a pure function of its state, its input and the effects handed back to it",
    "both code artifacts are the prebuilt first-party implementations their identities name",
    "the contract the boundary was constructed with is the corrected contract its declared artifact (the one the task identity names) stands for",
    "the verdicts hold for this one recorded trace; nothing is claimed for other inputs",
];

/// Witness built by [`crate::publish::Boundary::prepare`] and verified by `commit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Witness {
    trace_digest: Digest,
    from: u64,
    to: u64,
    log_epoch: u64,
    ancestor: Digest,
    head: Head,
    contract: ArtifactId,
    live_code: ArtifactId,
    live_verdict: Verdict,
    live_observation_chain: Digest,
    candidate_code: ArtifactId,
    candidate_verdict: Verdict,
    candidate_observation_chain: Digest,
    candidate_state_digest: Digest,
    same_code_reproduced: bool,
    grade: EvidenceGrade,
}

/// The values a witness binds, as `prepare` computed them.
pub(crate) struct WitnessParts {
    pub(crate) trace_digest: Digest,
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) log_epoch: u64,
    pub(crate) ancestor: Digest,
    pub(crate) head: Head,
    pub(crate) contract: ArtifactId,
    pub(crate) live_code: ArtifactId,
    pub(crate) live_verdict: Verdict,
    pub(crate) live_observation_chain: Digest,
    pub(crate) candidate_code: ArtifactId,
    pub(crate) candidate_verdict: Verdict,
    pub(crate) candidate_observation_chain: Digest,
    pub(crate) candidate_state_digest: Digest,
}

impl Witness {
    /// Builds a witness after same-code replay reproduced the paused head.
    pub(crate) fn reproduced(p: WitnessParts) -> Witness {
        Witness {
            trace_digest: p.trace_digest,
            from: p.from,
            to: p.to,
            log_epoch: p.log_epoch,
            ancestor: p.ancestor,
            head: p.head,
            contract: p.contract,
            live_code: p.live_code,
            live_verdict: p.live_verdict,
            live_observation_chain: p.live_observation_chain,
            candidate_code: p.candidate_code,
            candidate_verdict: p.candidate_verdict,
            candidate_observation_chain: p.candidate_observation_chain,
            candidate_state_digest: p.candidate_state_digest,
            same_code_reproduced: true,
            grade: EvidenceGrade::Tested,
        }
    }

    /// The digest of the replayed inputs ([`crate::log::EventLog::trace_digest`]).
    #[inline]
    #[must_use]
    pub const fn trace_digest(&self) -> Digest {
        self.trace_digest
    }

    /// The replayed cursor range `[from, to)`.
    #[inline]
    #[must_use]
    pub const fn range(&self) -> (u64, u64) {
        (self.from, self.to)
    }

    /// The log epoch the inputs came from.
    #[inline]
    #[must_use]
    pub const fn log_epoch(&self) -> u64 {
        self.log_epoch
    }

    /// The digest of the ancestor checkpoint both runs started from.
    #[inline]
    #[must_use]
    pub const fn ancestor(&self) -> Digest {
        self.ancestor
    }

    /// The paused live head the live code's replay reproduced.
    #[inline]
    #[must_use]
    pub const fn head(&self) -> &Head {
        &self.head
    }

    /// The contract both runs were judged by.
    #[inline]
    #[must_use]
    pub const fn contract(&self) -> &ArtifactId {
        &self.contract
    }

    /// The live code.
    #[inline]
    #[must_use]
    pub const fn live_code(&self) -> &ArtifactId {
        &self.live_code
    }

    /// The live code's verdict.
    #[inline]
    #[must_use]
    pub const fn live_verdict(&self) -> &Verdict {
        &self.live_verdict
    }

    /// The observation chain of the live code's replay.
    #[inline]
    #[must_use]
    pub const fn live_observation_chain(&self) -> Digest {
        self.live_observation_chain
    }

    /// The candidate code.
    #[inline]
    #[must_use]
    pub const fn candidate_code(&self) -> &ArtifactId {
        &self.candidate_code
    }

    /// The candidate's verdict.
    #[inline]
    #[must_use]
    pub const fn candidate_verdict(&self) -> &Verdict {
        &self.candidate_verdict
    }

    /// The observation chain of the candidate's replay.
    #[inline]
    #[must_use]
    pub const fn candidate_observation_chain(&self) -> Digest {
        self.candidate_observation_chain
    }

    /// The digest of the state the candidate's replay reached.
    #[inline]
    #[must_use]
    pub const fn candidate_state_digest(&self) -> Digest {
        self.candidate_state_digest
    }

    /// Whether the live code's replay reproduced the paused head.
    #[inline]
    #[must_use]
    pub const fn same_code_reproduced(&self) -> bool {
        self.same_code_reproduced
    }

    /// The evidence grade.
    #[inline]
    #[must_use]
    pub const fn grade(&self) -> EvidenceGrade {
        self.grade
    }

    /// What the grade rests on.
    #[inline]
    #[must_use]
    pub const fn assumptions(&self) -> &'static [&'static str] {
        &TESTED_ASSUMPTIONS
    }
}
