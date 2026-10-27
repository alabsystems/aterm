// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Behavioural contracts and verdicts over replayed runs.

use core::fmt;

use crate::effects::EffectPort;
use crate::ident::ArtifactId;
use crate::replay::{FoldedRun, Run, replay};
use crate::task::{Code, Schema};

/// A behavioural contract over runs of schema `S`.
pub trait Contract<S: Schema> {
    /// The contract's artifact (kind [`crate::ident::ArtifactKind::Contract`]).
    fn id(&self) -> &ArtifactId;
    /// Judges a run of `inputs` (whose observations carry their cursors).
    fn judge(&self, schema: &S, inputs: &[S::Input], run: &Run<S>) -> Verdict;
    /// Replays `code` from `start` over `inputs` from `first_cursor` and judges the run, keeping only what this contract reads when overridden.
    fn replay_judged<C: Code<S>>(
        &self,
        schema: &S,
        code: &C,
        start: S::State,
        inputs: &[S::Input],
        first_cursor: u64,
        port: &mut dyn EffectPort,
    ) -> (FoldedRun<S>, Verdict) {
        let run = replay(schema, code, start, inputs, first_cursor, port);
        let verdict = self.judge(schema, inputs, &run);
        (run.into_folded(), verdict)
    }
}

/// A contract's judgement of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every law holds over the run.
    Pass,
    /// The law `law` fails first at `cursor`.
    Fail {
        /// The cursor of the first failing observation.
        cursor: u64,
        /// The law that fails (a stable name).
        law: &'static str,
        /// The offending values.
        detail: String,
    },
}

impl Verdict {
    /// Whether the verdict is [`Verdict::Pass`].
    #[inline]
    #[must_use]
    pub const fn passed(&self) -> bool {
        matches!(self, Verdict::Pass)
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Pass => f.write_str("every law holds"),
            Verdict::Fail {
                cursor,
                law,
                detail,
            } => write!(f, "the law `{law}` fails at cursor {cursor}: {detail}"),
        }
    }
}
