// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure admission policy for the updater's final process-replacement lane.
//!
//! Foreground terminal jobs are safe only after the live-session handoff is fully
//! prepared.  Keeping this decision separate from the mechanics makes the safety
//! boundary executable, unit-testable, and available to the derived formal model's
//! real-code conformance test.

/// Complete facts at the point immediately before process replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionFacts {
    /// The reducer authorized an authenticated, strictly newer staged artifact.
    pub(crate) staged_verified: bool,
    /// Every live PTY required by this attempt is encoded in a prepared handoff.
    pub(crate) seamless_capable: bool,
    /// Opaque close-preflight evidence says native document/restore state is safe.
    pub(crate) native_state_certified: bool,
    /// Every currently-owned PTY, including an idle shell.
    pub(crate) live_ptys: usize,
    /// Terminals whose foreground process group is not their owning shell.
    pub(crate) foreground_jobs: usize,
    /// PTYs for which the foreground process group could not be observed.
    pub(crate) unknown_foregrounds: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyLane {
    Seamless,
    Cold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionBlock {
    UnverifiedStage,
    NativeStateUncertified,
    LivePtysNeedSeamless,
    ForegroundProbeUnknown,
}

impl AdmissionBlock {
    #[must_use]
    pub(crate) fn message(self, facts: AdmissionFacts) -> String {
        match self {
            Self::UnverifiedStage => "No newer verified update is staged".to_string(),
            Self::NativeStateUncertified => {
                "Update relaunch is waiting for native document state to become safe".to_string()
            }
            Self::LivePtysNeedSeamless => format!(
                "Update kept {} live terminal session(s), including {} foreground job(s), running because a live-PTY and visible-screen handoff could not be prepared; retry manually when handoff is available",
                facts.live_ptys, facts.foreground_jobs
            ),
            Self::ForegroundProbeUnknown => format!(
                "Update kept {} terminal session(s) running because their foreground process state could not be proven safe",
                facts.unknown_foregrounds
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionDecision {
    Apply(ApplyLane),
    Block(AdmissionBlock),
}

/// Choose the only safe replacement lane for the supplied runtime facts.
#[must_use]
pub(crate) fn classify(facts: AdmissionFacts) -> AdmissionDecision {
    if !facts.staged_verified {
        return AdmissionDecision::Block(AdmissionBlock::UnverifiedStage);
    }
    if !facts.native_state_certified {
        return AdmissionDecision::Block(AdmissionBlock::NativeStateUncertified);
    }
    // THE UNKNOWN-FOREGROUND PROBE GATES THE COLD LANE ONLY.
    //
    // That probe answers exactly one question: would replacing the app KILL
    // something the user is running? It belongs to the cold lane, which quits the
    // process and closes every PTY, hanging up whatever was in the foreground. The
    // seamless lane signals nothing — it parks readers, dups the PTY masters,
    // spawns the successor, and the shell keeps running undisturbed on the far end
    // of the same pty throughout. An unprobeable foreground is not a hazard it can
    // realize.
    //
    // Checking it first made one unprobeable PTY veto BOTH lanes. `foreground_pgrp`
    // is a bare `tcgetpgrp` (quit_safety.rs) and ANY error counts as unknown, so an
    // exited shell in a still-open tab or an orphaned pty was enough to block every
    // automatic update — and, because a Block becomes
    // `UpdateHandoffStartError::failed`, to latch automatic apply off for that
    // build. "Fail closed" was the right instinct aimed at the wrong lane: refusing
    // the LOSSLESS path because the DESTRUCTIVE path's precondition is unproven
    // buys no safety and costs the update.
    //
    // The incoherence check above deliberately stays ahead of seamless: that one is
    // not a hazard test but a "this fact set is not real" test, and no lane should
    // act on it. There is also no fallthrough risk — a seamless attempt that later
    // fails does not re-enter this classifier and pick up the cold lane; it goes to
    // the manual-only path.
    // An INCOHERENT observation still fails closed for every lane, seamless
    // included: more foreground jobs than PTYs means the fact set does not
    // describe any real session state, and no decision should be taken on it.
    if facts.foreground_jobs > facts.live_ptys {
        return AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless);
    }
    if facts.seamless_capable {
        return AdmissionDecision::Apply(ApplyLane::Seamless);
    }
    // A probe failure is not evidence of an idle shell, and everything past this
    // point is destructive. Fail closed.
    if facts.unknown_foregrounds > 0 {
        return AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown);
    }
    // A cold replacement is destructive even for an idle shell. It is admitted only
    // when there is literally no PTY/session to lose; inconsistent job facts fail closed.
    if facts.live_ptys > 0 || facts.foreground_jobs > 0 {
        return AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless);
    }
    AdmissionDecision::Apply(ApplyLane::Cold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> AdmissionFacts {
        AdmissionFacts {
            staged_verified: true,
            seamless_capable: false,
            native_state_certified: true,
            live_ptys: 0,
            foreground_jobs: 0,
            unknown_foregrounds: 0,
        }
    }

    #[test]
    fn foreground_job_requires_and_accepts_live_pty_handoff() {
        let with_job = AdmissionFacts {
            live_ptys: 1,
            foreground_jobs: 1,
            ..facts()
        };
        assert_eq!(
            classify(with_job),
            AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless)
        );
        assert_eq!(
            classify(AdmissionFacts {
                seamless_capable: true,
                ..with_job
            }),
            AdmissionDecision::Apply(ApplyLane::Seamless)
        );
    }

    #[test]
    fn cold_lane_is_only_for_verified_safe_sessionless_state() {
        assert_eq!(classify(facts()), AdmissionDecision::Apply(ApplyLane::Cold));
        assert!(matches!(
            classify(AdmissionFacts {
                staged_verified: false,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::UnverifiedStage)
        ));
        assert!(matches!(
            classify(AdmissionFacts {
                seamless_capable: true,
                native_state_certified: false,
                foreground_jobs: 2,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::NativeStateUncertified)
        ));
        assert_eq!(
            classify(AdmissionFacts {
                live_ptys: 1,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless)
        );
        assert_eq!(
            classify(AdmissionFacts {
                unknown_foregrounds: 1,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown)
        );
        assert_eq!(
            classify(AdmissionFacts {
                seamless_capable: true,
                live_ptys: 1,
                unknown_foregrounds: 1,
                ..facts()
            }),
            AdmissionDecision::Apply(ApplyLane::Seamless),
            "an unprobeable foreground must not veto the LOSSLESS lane: seamless \
             signals nothing, so a foreground job is not a hazard it can realize. \
             Blocking here made one orphaned pty disable automatic update entirely."
        );
        assert_eq!(
            classify(AdmissionFacts {
                seamless_capable: true,
                foreground_jobs: 1,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
            "inconsistent foreground/session counts fail closed"
        );
    }
}
