// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The online watcher's judgement of one retired log epoch of the glow.

use core::fmt;

use aterm_aos::contract::Contract;
use aterm_aos::effects::NoEffects;
use aterm_aos::publish::{Retire, Retired};
use aterm_aos::registry::Judged;
use aterm_aos::replay::replay;

use super::{MomentumSchema, MomentumTrace, NoStrandedLight, PrebuiltMomentum};

/// One retired log epoch of the glow, with the code that ran it, for [`Segment::judge`].
pub struct Segment {
    schema: MomentumSchema,
    retired: Retired<MomentumSchema, PrebuiltMomentum>,
}

impl fmt::Debug for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Segment")
            .field("retired", &self.retired)
            .finish_non_exhaustive()
    }
}

/// What [`Segment::judge`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentJudgement {
    /// Why the boundary retired it.
    pub cause: Retire,
    /// The short name of the code that ran it.
    pub code: &'static str,
    pub log_epoch: u64,
    /// Its first cursor.
    pub from: u64,
    /// The cursor after its last input.
    pub to: u64,
    pub judged: Judged,
    /// The segment as a trace file's contents when it failed, which a host keeps as a fault.
    pub fault: Option<MomentumTrace>,
}

impl Segment {
    /// The retired recording `retired` over `schema`.
    #[inline]
    #[must_use]
    pub(super) const fn new(
        schema: MomentumSchema,
        retired: Retired<MomentumSchema, PrebuiltMomentum>,
    ) -> Self {
        Segment { schema, retired }
    }

    /// Why the boundary retired it.
    #[inline]
    #[must_use]
    pub const fn cause(&self) -> Retire {
        self.retired.cause
    }

    /// The segment as a judgement of `judged`, with no fault kept.
    fn judgement(&self, judged: Judged) -> SegmentJudgement {
        let log = &self.retired.log;
        SegmentJudgement {
            cause: self.retired.cause,
            code: self.retired.code.sched().name(),
            log_epoch: log.header().log_epoch,
            from: log.header().first_cursor,
            to: log.end_cursor(),
            judged,
            fault: None,
        }
    }

    /// The segment left unjudged for `why` with no replay, as when the watcher's worker did not run.
    #[must_use]
    pub fn unjudged(self, why: &'static str) -> SegmentJudgement {
        self.judgement(Judged::Unjudged(why))
    }

    /// Replays the segment from its checkpoint under the code that ran it, requires the replay to reach the state the segment ended in, and judges the run by `momentum-glow.no-stranded-light`.
    #[must_use]
    pub fn judge(self) -> SegmentJudgement {
        let mut out = self.judgement(Judged::Unjudged("not judged"));
        let Segment { schema, retired } = self;
        let log = &retired.log;
        let from = out.from;
        let contract = NoStrandedLight::new();
        let identity = &log.header().identity;
        if log.broken().is_some() {
            out.judged = Judged::Unjudged("the log is broken");
            return out;
        }
        if &identity.contract != contract.id() {
            out.judged = Judged::Unjudged("the segment names another contract");
            return out;
        }
        let Ok(start) = retired.checkpoint.restore(&schema, identity) else {
            out.judged = Judged::Unjudged("the checkpoint does not restore");
            return out;
        };
        let run = replay(
            &schema,
            &retired.code,
            start,
            log.inputs(),
            from,
            &mut NoEffects,
        );
        if run.state_digest != retired.end_state {
            out.judged =
                Judged::Unjudged("the replay does not reach the state the segment ended in");
            return out;
        }
        let verdict = contract.judge(&schema, log.inputs(), &run);
        out.judged = Judged::from(&verdict);
        if out.judged.failed() {
            out.fault = retired
                .checkpoint
                .encode()
                .ok()
                .zip(log.encode(&schema).ok())
                .map(|(checkpoint, encoded)| MomentumTrace {
                    identity: identity.clone(),
                    checkpoint,
                    log: encoded,
                    head_state: run.state_digest,
                    head_chain: run.observation_chain,
                });
        }
        out
    }
}
