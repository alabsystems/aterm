// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The online watcher's judgement of one retired log epoch of a hosted engine.

use core::fmt;

use aterm_aos::contract::Contract;
use aterm_aos::effects::NoEffects;
use aterm_aos::publish::{Retire, Retired};
use aterm_aos::registry::Judged;

use super::{EffectCode, EffectSchema, EffectTrace, HostedEngine};

/// One retired log epoch, with the code that ran it, for [`Segment::judge`].
pub struct Segment<E: HostedEngine> {
    schema: EffectSchema<E>,
    retired: Retired<EffectSchema<E>, EffectCode<E>>,
}

impl<E: HostedEngine> fmt::Debug for Segment<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Segment")
            .field("retired", &self.retired)
            .finish_non_exhaustive()
    }
}

/// What [`Segment::judge`] found.
pub struct SegmentJudgement<E: HostedEngine> {
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
    pub fault: Option<EffectTrace<E>>,
}

impl<E: HostedEngine> Clone for SegmentJudgement<E> {
    #[inline]
    fn clone(&self) -> Self {
        SegmentJudgement {
            cause: self.cause,
            code: self.code,
            log_epoch: self.log_epoch,
            from: self.from,
            to: self.to,
            judged: self.judged.clone(),
            fault: self.fault.clone(),
        }
    }
}

impl<E: HostedEngine> PartialEq for SegmentJudgement<E> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.cause == other.cause
            && self.code == other.code
            && self.log_epoch == other.log_epoch
            && self.from == other.from
            && self.to == other.to
            && self.judged == other.judged
            && self.fault == other.fault
    }
}

impl<E: HostedEngine> Eq for SegmentJudgement<E> {}

impl<E: HostedEngine> fmt::Debug for SegmentJudgement<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SegmentJudgement")
            .field("cause", &self.cause)
            .field("code", &self.code)
            .field("log_epoch", &self.log_epoch)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("judged", &self.judged)
            .field("fault", &self.fault)
            .finish()
    }
}

impl<E: HostedEngine> Segment<E> {
    /// The retired recording `retired` over `schema`.
    #[inline]
    #[must_use]
    pub(crate) const fn new(
        schema: EffectSchema<E>,
        retired: Retired<EffectSchema<E>, EffectCode<E>>,
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
    #[inline]
    fn judgement(&self, judged: Judged) -> SegmentJudgement<E> {
        let log = &self.retired.log;
        SegmentJudgement {
            cause: self.retired.cause,
            code: E::code_name(self.retired.code.code),
            log_epoch: log.header().log_epoch,
            from: log.header().first_cursor,
            to: log.end_cursor(),
            judged,
            fault: None,
        }
    }

    /// The segment left unjudged for `why` with no replay, as when the watcher's worker did not run.
    #[inline]
    #[must_use]
    pub fn unjudged(self, why: &'static str) -> SegmentJudgement<E> {
        self.judgement(Judged::Unjudged(why))
    }

    /// Replays the segment from its checkpoint under the code that ran it, requires the replay to reach the state the segment ended in, and judges the run by the task's contract.
    #[inline]
    #[must_use]
    pub fn judge(self) -> SegmentJudgement<E> {
        let mut out = self.judgement(Judged::Unjudged("not judged"));
        let Segment { schema, retired } = self;
        let log = &retired.log;
        let from = out.from;
        let contract = E::contract();
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
        let (run, verdict) = contract.replay_judged(
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
        out.judged = Judged::from(&verdict);
        if out.judged.failed()
            && let (Ok(checkpoint), Ok(encoded)) =
                (retired.checkpoint.encode(), log.encode(&schema))
        {
            out.fault = Some(EffectTrace::new(
                identity.clone(),
                checkpoint,
                encoded,
                run.state_digest,
                run.observation_chain,
            ));
        }
        out
    }
}
