// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic [`replay`] of task steps into a [`Run`] stopping at the first [`StepRefusal`].

use core::fmt;

use crate::effects::EffectPort;
use crate::ident::Digest;
use crate::task::{Code, Schema, StepRefusal, state_digest};
use crate::wire::Enc;

/// The domain of the observation chain.
pub const OBSERVATION_DOMAIN: &[u8] = b"aterm-aos-obs-v1\0";

/// What one replay produced.
pub struct Run<S: Schema> {
    /// The state after the last step that ran.
    pub state: S::State,
    /// Its digest.
    pub state_digest: Digest,
    /// Each step's observation, with its input's cursor.
    pub observations: Vec<(u64, S::Observation)>,
    /// The fold of every observation and its cursor.
    pub observation_chain: Digest,
    /// The first refusal, with its input's cursor; nothing after it ran.
    pub refused: Option<(u64, StepRefusal)>,
}

impl<S: Schema> Clone for Run<S> {
    fn clone(&self) -> Self {
        Run {
            state: self.state.clone(),
            state_digest: self.state_digest,
            observations: self.observations.clone(),
            observation_chain: self.observation_chain,
            refused: self.refused.clone(),
        }
    }
}

impl<S: Schema> fmt::Debug for Run<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Run")
            .field("state_digest", &self.state_digest)
            .field("observations", &self.observations)
            .field("observation_chain", &self.observation_chain)
            .field("refused", &self.refused)
            .finish_non_exhaustive()
    }
}

impl<S: Schema> Run<S> {
    /// Drops the retained per-step observations, keeping the replay's summary.
    #[must_use]
    pub fn into_folded(self) -> FoldedRun<S> {
        let observations = self.observations.len();
        let last_cursor = self.observations.last().map(|&(c, _)| c);
        FoldedRun {
            state: self.state,
            state_digest: self.state_digest,
            observations,
            last_cursor,
            observation_chain: self.observation_chain,
            refused: self.refused,
        }
    }
}

/// What one folded replay produced without retaining each step's observation.
pub struct FoldedRun<S: Schema> {
    /// The state after the last step that ran.
    pub state: S::State,
    /// Its digest.
    pub state_digest: Digest,
    /// How many observations the replay produced before it finished or refused.
    pub observations: usize,
    /// The cursor of the last observation, if any.
    pub last_cursor: Option<u64>,
    /// The fold of every observation and its cursor.
    pub observation_chain: Digest,
    /// The first refusal, with its input's cursor; nothing after it ran.
    pub refused: Option<(u64, StepRefusal)>,
}

impl<S: Schema> Clone for FoldedRun<S> {
    fn clone(&self) -> Self {
        FoldedRun {
            state: self.state.clone(),
            state_digest: self.state_digest,
            observations: self.observations,
            last_cursor: self.last_cursor,
            observation_chain: self.observation_chain,
            refused: self.refused.clone(),
        }
    }
}

impl<S: Schema> fmt::Debug for FoldedRun<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FoldedRun")
            .field("state_digest", &self.state_digest)
            .field("observations", &self.observations)
            .field("last_cursor", &self.last_cursor)
            .field("observation_chain", &self.observation_chain)
            .field("refused", &self.refused)
            .finish_non_exhaustive()
    }
}

/// The empty observation chain.
#[inline]
#[must_use]
pub fn empty_chain() -> Digest {
    Digest::of(OBSERVATION_DOMAIN, &[])
}

/// One link of the observation chain.
#[inline]
#[must_use]
pub fn chain_link(chain: &Digest, cursor: u64, encoded_observation: &[u8]) -> Digest {
    Digest::of(
        OBSERVATION_DOMAIN,
        &[&chain.0, &cursor.to_le_bytes(), encoded_observation],
    )
}

/// The law name of a replay stopped because its cursors left the `u64` range.
pub const CURSOR_EXHAUSTED: &str = "aos.cursor-exhausted";

/// Runs `code` from `start` over `inputs` from `first_cursor` ([`CURSOR_EXHAUSTED`]), passing each observation to `on_step` after linking it into the observation chain.
pub fn replay_fold<S: Schema, C: Code<S>, F: FnMut(usize, u64, &S::Input, S::Observation)>(
    schema: &S,
    code: &C,
    start: S::State,
    inputs: &[S::Input],
    first_cursor: u64,
    port: &mut dyn EffectPort,
    mut on_step: F,
) -> FoldedRun<S> {
    let mut state = start;
    let mut observations = 0usize;
    let mut last_cursor = None;
    let mut chain = empty_chain();
    let mut refused = None;
    let mut cursor = Some(first_cursor);
    let mut e = Enc::new();
    for (index, input) in inputs.iter().enumerate() {
        let Some(at) = cursor else {
            refused = Some((
                u64::MAX,
                StepRefusal {
                    law: CURSOR_EXHAUSTED,
                    detail: format!("an input follows cursor {}", u64::MAX),
                },
            ));
            break;
        };
        match code.step(schema, &mut state, input, port) {
            Ok(obs) => {
                e.clear();
                schema.encode_observation(&obs, &mut e);
                chain = chain_link(&chain, at, e.as_bytes());
                observations += 1;
                last_cursor = Some(at);
                on_step(index, at, input, obs);
            }
            Err(r) => {
                refused = Some((at, r));
                break;
            }
        }
        cursor = at.checked_add(1);
    }
    let state_digest = state_digest(schema, &state);
    FoldedRun {
        state,
        state_digest,
        observations,
        last_cursor,
        observation_chain: chain,
        refused,
    }
}

/// Runs `code` from `start` over `inputs` from `first_cursor` ([`CURSOR_EXHAUSTED`]).
pub fn replay<S: Schema, C: Code<S>>(
    schema: &S,
    code: &C,
    start: S::State,
    inputs: &[S::Input],
    first_cursor: u64,
    port: &mut dyn EffectPort,
) -> Run<S> {
    let mut observations = Vec::with_capacity(inputs.len());
    let folded = replay_fold(
        schema,
        code,
        start,
        inputs,
        first_cursor,
        port,
        |_, at, _, obs| {
            observations.push((at, obs));
        },
    );
    Run {
        state: folded.state,
        state_digest: folded.state_digest,
        observations,
        observation_chain: folded.observation_chain,
        refused: folded.refused,
    }
}

/// Where two runs first differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divergence {
    /// Same observations, different end states.
    State {
        /// The first run's state digest.
        a: Digest,
        /// The second run's state digest.
        b: Digest,
    },
    /// The observation at `cursor` differs (or sits at another cursor).
    Observation {
        /// The cursor of the first differing observation.
        cursor: u64,
    },
    /// One run made more observations than the other, neither refused.
    Length {
        /// The first run's observation count.
        a: usize,
        /// The second run's observation count.
        b: usize,
    },
    /// One run refused at `cursor` where the other did not (or refused otherwise).
    Refused {
        /// The cursor of the earlier refusal.
        cursor: u64,
    },
}

impl fmt::Display for Divergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Divergence::State { a, b } => write!(
                f,
                "the runs end in different states ({} against {})",
                a.short(),
                b.short()
            ),
            Divergence::Observation { cursor } => {
                write!(f, "the runs observe differently at cursor {cursor}")
            }
            Divergence::Length { a, b } => {
                write!(f, "one run made {a} observations and the other {b}")
            }
            Divergence::Refused { cursor } => {
                write!(f, "the runs differ in what they refused at cursor {cursor}")
            }
        }
    }
}

impl std::error::Error for Divergence {}

/// The first point where `a` and `b` differ, in run order.
#[must_use]
pub fn first_divergence<S: Schema>(a: &Run<S>, b: &Run<S>) -> Option<Divergence> {
    for ((ca, oa), (cb, ob)) in a.observations.iter().zip(&b.observations) {
        if ca != cb || oa != ob {
            return Some(Divergence::Observation {
                cursor: (*ca).min(*cb),
            });
        }
    }
    if a.refused != b.refused {
        let cursor = match (&a.refused, &b.refused) {
            (Some((x, _)), Some((y, _))) => (*x).min(*y),
            (Some((x, _)), None) | (None, Some((x, _))) => *x,
            (None, None) => 0,
        };
        return Some(Divergence::Refused { cursor });
    }
    if a.observations.len() != b.observations.len() {
        return Some(Divergence::Length {
            a: a.observations.len(),
            b: b.observations.len(),
        });
    }
    if a.state_digest != b.state_digest {
        return Some(Divergence::State {
            a: a.state_digest,
            b: b.state_digest,
        });
    }
    None
}
