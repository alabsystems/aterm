// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication of an evaluator-admitted plan over a paused momentum glow (design §10.6, §10.7).
//!
//! A result cannot be published twice:
//!
//! ```compile_fail,E0382
//! use aterm_effects::aos_momentum::MomentumHost;
//! use aterm_effects::aos_momentum::m4::Admitted;
//! fn twice(host: &mut MomentumHost, admitted: Admitted) {
//!     let _ = host.aos_publish(admitted);
//!     let _ = host.aos_publish(admitted);
//! }
//! ```

use core::fmt;

use aterm_aos::publish::{Commit, Prepared, PublishRefusal};
use aterm_aos::task::StepRefusal;
use aterm_aos::{Digest, Revision};

use super::bundle::{CandidateBundle, Publication, PublishedHead};
use super::m4::{Admitted, row_verdict};
use super::policy::{AcceptancePolicy, M4_POLICY_V1_DIGEST, Replace};
use super::{
    HostMode, Hosted, MomentumHost, MomentumObservation, MomentumSchema, PrebuiltMomentum,
};

/// Why an admitted plan was not published, in the order the checks run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanPublishRefusal {
    /// The glow is not attached.
    NotAttached,
    /// The attached glow is not paused: [`MomentumHost::aos_pause`] first.
    NotPaused,
    /// The result was admitted under another policy than the pinned one.
    PolicyMismatch {
        /// The digest of the policy it was admitted under.
        admitted: Digest,
        /// [`M4_POLICY_V1_DIGEST`].
        pinned: Digest,
    },
    /// The result was judged in another build than the host's.
    ForeignBuild {
        /// The build it was judged in.
        admitted: String,
        /// The build the host's code is compiled into.
        host: String,
    },
    /// The live code is not a base the policy names, as a plan already published is not.
    BaseNotInPolicy {
        /// The live code's short name ([`super::Sched::name`]).
        live: &'static str,
    },
    /// The live code is the shipped code and the plan is not useful.
    NotUseful {
        /// The plan's digest.
        plan: Digest,
        /// Its held-out gain, in parts per million.
        gain_ppm: i64,
    },
    /// The boundary refused `prepare` or `commit`.
    Boundary(PublishRefusal),
}

impl fmt::Display for PlanPublishRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanPublishRefusal::NotAttached => f.write_str("the glow is not attached"),
            PlanPublishRefusal::NotPaused => {
                f.write_str("the glow is not paused: pause it first (aos pause)")
            }
            PlanPublishRefusal::PolicyMismatch { admitted, pinned } => write!(
                f,
                "the plan was admitted under the policy {}, not the pinned {}",
                admitted.short(),
                pinned.short()
            ),
            PlanPublishRefusal::ForeignBuild { admitted, host } => write!(
                f,
                "the plan was judged in the build {admitted}, not the host's {host}"
            ),
            PlanPublishRefusal::BaseNotInPolicy { live } => write!(
                f,
                "the live code {live} is not a base the policy names (shipped, demo-stranded-settle)"
            ),
            PlanPublishRefusal::NotUseful { plan, gain_ppm } => write!(
                f,
                "the plan {} is not useful (held-out gain {gain_ppm} ppm): replacing the shipped \
                 code is an optimization and must be useful",
                plan.short()
            ),
            PlanPublishRefusal::Boundary(r) => write!(f, "{r}"),
        }
    }
}

impl std::error::Error for PlanPublishRefusal {}

impl From<PublishRefusal> for PlanPublishRefusal {
    fn from(r: PublishRefusal) -> Self {
        PlanPublishRefusal::Boundary(r)
    }
}

/// Published plan: bundle with witness publication data, minted revision, and queued input results.
#[derive(Debug)]
pub struct Published {
    bundle: CandidateBundle,
    revision: Revision,
    delivered: Vec<Result<MomentumObservation, StepRefusal>>,
}

impl Published {
    /// The bundle, its publication part filled.
    #[inline]
    #[must_use]
    pub const fn bundle(&self) -> &CandidateBundle {
        &self.bundle
    }

    /// The bundle's publication part (always filled here).
    #[inline]
    #[must_use]
    pub const fn publication(&self) -> Option<&Publication> {
        self.bundle.parts().publication.as_ref()
    }

    /// The revision the commit minted.
    #[inline]
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// What each queued input returned, once each, in order, through the new pair.
    #[inline]
    pub fn delivered(&self) -> &[Result<MomentumObservation, StepRefusal>] {
        &self.delivered
    }

    /// The bundle, the result consumed.
    #[inline]
    #[must_use]
    pub fn into_bundle(self) -> CandidateBundle {
        self.bundle
    }
}

struct Staged {
    prepared: Prepared<MomentumSchema, PrebuiltMomentum>,
    bundle: CandidateBundle,
}

fn publication(commit: &Commit<MomentumSchema>) -> Publication {
    let w = &commit.witness;
    let head = w.head();
    let (from, to) = w.range();
    Publication {
        head: PublishedHead {
            identity: head.identity,
            revision: head.revision.0,
            branch: head.branch.0,
            state: head.state_digest,
            input_cursor: head.input_cursor,
            effect_cursor: head.effect_cursor,
        },
        revision: commit.revision.0,
        live_code: w.live_code().clone(),
        live_verdict: row_verdict(w.live_verdict()),
        trace: w.trace_digest(),
        from,
        to,
        log_epoch: w.log_epoch(),
        ancestor: w.ancestor(),
        candidate_code: w.candidate_code().clone(),
        candidate_verdict: row_verdict(w.candidate_verdict()),
        candidate_chain: w.candidate_observation_chain(),
        candidate_state: w.candidate_state_digest(),
    }
}

impl Hosted {
    fn published(
        &mut self,
        first: u64,
        commit: Commit<MomentumSchema>,
        mut bundle: CandidateBundle,
    ) -> Published {
        self.fold_delivered(first, &commit.delivered);
        bundle.parts_mut().publication = Some(publication(&commit));
        Published {
            bundle,
            revision: commit.revision,
            delivered: commit.delivered,
        }
    }
}

impl MomentumHost {
    /// Publishes `admitted`'s plan over the paused glow.
    /// # Errors
    /// [`PlanPublishRefusal::NotAttached`] or [`PlanPublishRefusal::NotPaused`], then
    /// `PolicyMismatch`, `ForeignBuild`, `BaseNotInPolicy`, `NotUseful` and `Boundary` in that
    /// order, none of which changes the host's pause, queue or live pair.
    pub fn aos_publish(&mut self, admitted: Admitted) -> Result<Published, PlanPublishRefusal> {
        let staged = self.stage(admitted)?;
        self.commit_staged(staged)
    }

    fn stage(&mut self, admitted: Admitted) -> Result<Staged, PlanPublishRefusal> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PlanPublishRefusal::NotAttached);
        };
        if !h.boundary.is_paused() {
            return Err(PlanPublishRefusal::NotPaused);
        }
        let policy = AcceptancePolicy::v1();
        if admitted.policy() != policy.id() || policy.id().digest != M4_POLICY_V1_DIGEST {
            return Err(PlanPublishRefusal::PolicyMismatch {
                admitted: admitted.policy().digest,
                pinned: M4_POLICY_V1_DIGEST,
            });
        }
        let judged_in = &admitted.bundle().parts().build;
        if *judged_in != h.build {
            return Err(PlanPublishRefusal::ForeignBuild {
                admitted: judged_in.clone(),
                host: h.build.clone(),
            });
        }
        let live = h.sched();
        let Some(how) = policy.base(live) else {
            return Err(PlanPublishRefusal::BaseNotInPolicy { live: live.name() });
        };
        if how == Replace::Optimization && !admitted.useful() {
            return Err(PlanPublishRefusal::NotUseful {
                plan: admitted.plan().digest(),
                gain_ppm: admitted.gain().ppm(),
            });
        }
        let planned = PrebuiltMomentum::planned(admitted.plan(), &h.build);
        let prepared = h.boundary.prepare(planned)?;
        Ok(Staged {
            prepared,
            bundle: admitted.into_bundle(),
        })
    }

    fn commit_staged(&mut self, staged: Staged) -> Result<Published, PlanPublishRefusal> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PlanPublishRefusal::NotAttached);
        };
        let first = h.cursor();
        let commit = h.boundary.commit(staged.prepared)?;
        Ok(h.published(first, commit, staged.bundle))
    }

    /// [`Self::aos_publish`] with `between` run on the host after `prepare` and before `commit`.
    #[cfg(test)]
    pub(super) fn aos_publish_after(
        &mut self,
        admitted: Admitted,
        between: impl FnOnce(&mut MomentumHost),
    ) -> Result<Published, PlanPublishRefusal> {
        let staged = self.stage(admitted)?;
        between(self);
        self.commit_staged(staged)
    }

    /// [`Self::aos_publish`] with the commit stopped after its `stores`-th primitive store.
    #[cfg(all(test, debug_assertions))]
    pub(super) fn aos_publish_stopping_after(
        &mut self,
        admitted: Admitted,
        stores: usize,
    ) -> Option<Result<Published, PlanPublishRefusal>> {
        let staged = match self.stage(admitted) {
            Ok(staged) => staged,
            Err(r) => return Some(Err(r)),
        };
        let HostMode::Hosted(h) = &mut self.mode else {
            return Some(Err(PlanPublishRefusal::NotAttached));
        };
        let first = h.cursor();
        Some(
            match h.boundary.commit_stopping_after(staged.prepared, stores)? {
                Ok(commit) => Ok(h.published(first, commit, staged.bundle)),
                Err(r) => Err(PlanPublishRefusal::Boundary(r)),
            },
        )
    }
}
