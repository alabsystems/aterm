// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure admission policy for the updater's final process-replacement lane.

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
    /// This process keeps the image it was launched with (`aterm_update::ImageReplacement`),
    /// so it never execs in place and the cold lane is closed to it.
    pub(crate) image_kept: bool,
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
    KeptImageNeedsTerminal,
}

impl AdmissionBlock {
    #[must_use]
    pub(crate) fn message(self, facts: AdmissionFacts) -> String {
        match self {
            Self::UnverifiedStage => "No newer verified update is staged".to_string(),
            Self::NativeStateUncertified => {
                "The update is waiting for an open editor to be saved".to_string()
            }
            Self::LivePtysNeedSeamless => format!(
                "Update kept {} live terminal session(s), including {} foreground job(s), running because this copy of aterm could not hand them over; it installs once every terminal is closed",
                facts.live_ptys, facts.foreground_jobs
            ),
            Self::ForegroundProbeUnknown => format!(
                "Update kept {} terminal session(s) running because their foreground process state could not be proven safe",
                facts.unknown_foregrounds
            ),
            Self::KeptImageNeedsTerminal => "The update installs when a terminal tab is open: \
                 this aterm hands its terminals to the new build and never restarts in place"
                .to_string(),
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
pub(crate) const fn classify(facts: AdmissionFacts) -> AdmissionDecision {
    if !facts.staged_verified {
        return AdmissionDecision::Block(AdmissionBlock::UnverifiedStage);
    }
    if !facts.native_state_certified {
        return AdmissionDecision::Block(AdmissionBlock::NativeStateUncertified);
    }
    // The unknown-foreground probe gates only the cold lane; more foreground jobs than
    // PTYs is inconsistent state and fails closed for every lane.
    if facts.foreground_jobs > facts.live_ptys {
        return AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless);
    }
    if facts.seamless_capable {
        return AdmissionDecision::Apply(ApplyLane::Seamless);
    }
    if facts.live_ptys > 0 || facts.foreground_jobs > 0 {
        return AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless);
    }
    if facts.unknown_foregrounds > 0 {
        return AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown);
    }
    if facts.image_kept {
        return AdmissionDecision::Block(AdmissionBlock::KeptImageNeedsTerminal);
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
            image_kept: false,
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
            AdmissionDecision::Block(AdmissionBlock::ForegroundProbeUnknown),
            "an unreadable foreground with no live PTY fails closed"
        );
        assert_eq!(
            classify(AdmissionFacts {
                live_ptys: 2,
                unknown_foregrounds: 1,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
            "without the seamless lane every live terminal blocks, its foreground read or \
             not: the refusal that says why the handoff is unavailable (U26)"
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
