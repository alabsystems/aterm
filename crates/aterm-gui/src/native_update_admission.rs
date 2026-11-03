// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure admission policy for the updater's final process-replacement lane.

/// Complete facts at the point immediately before process replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdmissionFacts {
    /// The reducer authorized an authenticated, strictly newer staged artifact.
    pub(crate) staged_verified: bool,
    /// The overlap handoff can carry this attempt: every live PTY it requires, or, with none,
    /// the windows alone ([`AdmissionFacts::observed`]).
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

/// What `App::start_unix_update_handoff` measures before it classifies an attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    /// [`AdmissionFacts::staged_verified`].
    pub(crate) staged_verified: bool,
    /// The overlap handoff is available to this process (no `HandoffUnavailable`).
    pub(crate) overlap_available: bool,
    /// The handoff can run with no terminal session: the successor reads a sessionless grant,
    /// the launched lane can reach it, and the windows carry a restorable layout.
    pub(crate) sessionless_handoff: bool,
    /// [`AdmissionFacts::native_state_certified`].
    pub(crate) native_state_certified: bool,
    /// [`AdmissionFacts::live_ptys`].
    pub(crate) live_ptys: usize,
    /// [`AdmissionFacts::foreground_jobs`].
    pub(crate) foreground_jobs: usize,
    /// [`AdmissionFacts::unknown_foregrounds`].
    pub(crate) unknown_foregrounds: usize,
    /// [`AdmissionFacts::image_kept`].
    pub(crate) image_kept: bool,
}

impl AdmissionFacts {
    /// The facts an observation gives: the overlap handoff carries an attempt with live
    /// sessions whenever it is available, and one with none where it can run sessionless.
    #[must_use]
    pub(crate) const fn observed(seen: Observation) -> Self {
        Self {
            staged_verified: seen.staged_verified,
            seamless_capable: seen.overlap_available
                && (seen.live_ptys > 0 || seen.sessionless_handoff),
            native_state_certified: seen.native_state_certified,
            live_ptys: seen.live_ptys,
            foreground_jobs: seen.foreground_jobs,
            unknown_foregrounds: seen.unknown_foregrounds,
            image_kept: seen.image_kept,
        }
    }
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
    KeptImageWithoutHandoff,
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
            Self::KeptImageWithoutHandoff => "The update waits: this aterm cannot hand itself \
                 over to the new build here, and it never restarts in place"
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
        return AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff);
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
        assert_eq!(
            classify(AdmissionFacts {
                seamless_capable: true,
                image_kept: true,
                ..facts()
            }),
            AdmissionDecision::Apply(ApplyLane::Seamless),
            "a kept image with no terminal session hands over when the handoff can"
        );
        assert_eq!(
            classify(AdmissionFacts {
                image_kept: true,
                ..facts()
            }),
            AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff)
        );
    }

    fn seen() -> Observation {
        Observation {
            staged_verified: true,
            overlap_available: true,
            sessionless_handoff: true,
            native_state_certified: true,
            live_ptys: 0,
            foreground_jobs: 0,
            unknown_foregrounds: 0,
            image_kept: true,
        }
    }

    /// 0.145.0, 2026-11-02: a Dock-launched window holding only native tabs was refused
    /// 249 times in four hours because the facts called the handoff incapable at zero
    /// sessions.
    #[test]
    fn a_kept_image_with_no_terminal_hands_over_when_the_successor_reads_a_sessionless_grant() {
        assert_eq!(
            classify(AdmissionFacts::observed(seen())),
            AdmissionDecision::Apply(ApplyLane::Seamless)
        );
        assert_eq!(
            classify(AdmissionFacts::observed(Observation {
                sessionless_handoff: false,
                ..seen()
            })),
            AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff),
            "a successor that cannot read a sessionless grant is never handed one"
        );
        assert_eq!(
            classify(AdmissionFacts::observed(Observation {
                overlap_available: false,
                ..seen()
            })),
            AdmissionDecision::Block(AdmissionBlock::KeptImageWithoutHandoff)
        );
        assert_eq!(
            classify(AdmissionFacts::observed(Observation {
                sessionless_handoff: false,
                image_kept: false,
                ..seen()
            })),
            AdmissionDecision::Apply(ApplyLane::Cold),
            "a process that can replace its image keeps the cold lane at zero sessions"
        );
        for sessionless_handoff in [false, true] {
            assert_eq!(
                classify(AdmissionFacts::observed(Observation {
                    sessionless_handoff,
                    live_ptys: 2,
                    foreground_jobs: 1,
                    ..seen()
                })),
                AdmissionDecision::Apply(ApplyLane::Seamless),
                "live sessions hand over whatever the sessionless grant"
            );
        }
        assert!(
            !AdmissionBlock::KeptImageWithoutHandoff
                .message(AdmissionFacts::observed(seen()))
                .contains("terminal tab")
        );
    }
}
