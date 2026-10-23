// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The toy task every suite test hosts: an accumulator with a "hot" flag that a tick must clear, an
//! order-sensitive fold of what it was fed, and an effect-emitting input.

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::{EffectPort, EffectRequest, NoEffects};
use aterm_aos::publish::{Admit, Boundary};
use aterm_aos::replay::Run;
use aterm_aos::task::{Code, EntryRefusal, PausedClass, Schema, StepRefusal};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, Digest, TaskIdentity};

#[derive(Clone, Debug)]
pub struct ToySchema {
    id: ArtifactId,
    pub lossy: bool,
    pub sloppy: bool,
}

impl ToySchema {
    pub fn new() -> Self {
        ToySchema {
            id: ArtifactId::new(ArtifactKind::Schema, "toy-schema", 1, b"total,hot,seen"),
            lossy: false,
            sloppy: false,
        }
    }

    pub fn lossy() -> Self {
        ToySchema {
            lossy: true,
            ..Self::new()
        }
    }

    pub fn sloppy() -> Self {
        ToySchema {
            sloppy: true,
            ..Self::new()
        }
    }

    /// The same codec under another declared schema artifact.
    pub fn declared(id: ArtifactId) -> Self {
        ToySchema { id, ..Self::new() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToyState {
    pub total: i64,
    pub hot: bool,
    pub seen: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToyInput {
    Add(i64),
    Reset,
    Tick,
    Emit {
        target: u32,
        op: u16,
        payload: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToyObs {
    Added { total: i64 },
    WasReset,
    Ticked { hot: bool },
    Emitted { result: Vec<u8> },
}

impl Schema for ToySchema {
    type State = ToyState;
    type Input = ToyInput;
    type Observation = ToyObs;

    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn encode_state(&self, s: &ToyState, e: &mut Enc) {
        e.i64(s.total);
        if !self.lossy {
            e.bool(s.hot);
        }
        e.u64(s.seen);
    }

    fn decode_state(&self, d: &mut Dec<'_>) -> Result<ToyState, WireFault> {
        let total = d.i64("toy.total")?;
        let hot = if self.lossy {
            false
        } else if self.sloppy {
            d.u8("toy.hot")? != 0
        } else {
            d.bool("toy.hot")?
        };
        let seen = d.u64("toy.seen")?;
        Ok(ToyState { total, hot, seen })
    }

    fn encode_input(&self, i: &ToyInput, e: &mut Enc) {
        match i {
            ToyInput::Add(n) => {
                e.u8(0);
                e.i64(*n);
            }
            ToyInput::Reset => e.u8(1),
            ToyInput::Tick => e.u8(2),
            ToyInput::Emit {
                target,
                op,
                payload,
            } => {
                e.u8(3);
                e.u32(*target);
                e.u16(*op);
                e.bytes(payload);
            }
        }
    }

    fn decode_input(&self, d: &mut Dec<'_>) -> Result<ToyInput, WireFault> {
        match d.u8("toy.input")? {
            0 => Ok(ToyInput::Add(d.i64("toy.add")?)),
            1 => Ok(ToyInput::Reset),
            2 => Ok(ToyInput::Tick),
            3 => Ok(ToyInput::Emit {
                target: d.u32("toy.emit.target")?,
                op: d.u16("toy.emit.op")?,
                payload: d.bytes("toy.emit.payload")?.to_vec(),
            }),
            tag => Err(WireFault::BadTag {
                field: "toy.input",
                tag,
            }),
        }
    }

    fn encode_observation(&self, o: &ToyObs, e: &mut Enc) {
        match o {
            ToyObs::Added { total } => {
                e.u8(0);
                e.i64(*total);
            }
            ToyObs::WasReset => e.u8(1),
            ToyObs::Ticked { hot } => {
                e.u8(2);
                e.bool(*hot);
            }
            ToyObs::Emitted { result } => {
                e.u8(3);
                e.bytes(result);
            }
        }
    }

    fn paused_class(&self, i: &ToyInput) -> PausedClass {
        match i {
            ToyInput::Tick => PausedClass::Skip,
            _ => PausedClass::Queue,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Good,
    Stranded,
    Strict,
    Picky,
    Leaky,
    OtherSchema,
    ShiftTarget,
    ShiftOp,
    AlterPayload,
    ExtraEffect,
    SkipEffect,
}

#[derive(Clone, Debug)]
pub struct ToyCode {
    id: ArtifactId,
    pub variant: Variant,
}

pub fn code(variant: Variant) -> ToyCode {
    let name = format!("toy-{variant:?}").to_lowercase();
    ToyCode {
        id: ArtifactId::new(ArtifactKind::Code, &name, 1, b"prebuilt; build=test"),
        variant,
    }
}

/// A code value whose artifact is of another kind than code.
pub fn mislabelled_code() -> ToyCode {
    ToyCode {
        id: ArtifactId::new(ArtifactKind::Contract, "toy-mislabelled", 1, b""),
        variant: Variant::Good,
    }
}

/// A code value whose artifact is an acceptance policy.
pub fn policy_labelled_code() -> ToyCode {
    ToyCode {
        id: ArtifactId::new(ArtifactKind::Policy, "toy-policy", 1, b""),
        variant: Variant::Good,
    }
}

/// The order-sensitive fold of what the task was fed.
pub fn mix(seen: u64, tag: u64, value: u64) -> u64 {
    seen.wrapping_mul(1_000_003)
        .wrapping_add(tag.wrapping_mul(7919))
        .wrapping_add(value)
}

impl Code<ToySchema> for ToyCode {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn schema_digest(&self, schema: &ToySchema) -> Digest {
        match self.variant {
            Variant::OtherSchema => {
                ArtifactId::new(
                    ArtifactKind::Schema,
                    "toy-schema",
                    2,
                    b"total,hot,seen,extra",
                )
                .digest
            }
            _ => schema.id().digest,
        }
    }

    fn entry(&self, _schema: &ToySchema, state: &ToyState) -> Result<(), EntryRefusal> {
        if self.variant == Variant::Strict && state.total < 0 {
            return Err(EntryRefusal {
                law: "toy.non-negative",
                detail: format!("total {}", state.total),
            });
        }
        Ok(())
    }

    fn step(
        &self,
        _schema: &ToySchema,
        state: &mut ToyState,
        input: &ToyInput,
        port: &mut dyn EffectPort,
    ) -> Result<ToyObs, StepRefusal> {
        match input {
            ToyInput::Add(n) => {
                if self.variant == Variant::Leaky && *n == 13 {
                    state.seen = mix(state.seen, 9, 13);
                    return Err(StepRefusal {
                        law: "toy.no-thirteen",
                        detail: "13, after a leak".to_owned(),
                    });
                }
                if self.variant == Variant::Picky && *n == 13 {
                    return Err(StepRefusal {
                        law: "toy.no-thirteen",
                        detail: "13".to_owned(),
                    });
                }
                let total = state.total.checked_add(*n).ok_or_else(|| StepRefusal {
                    law: "toy.overflow",
                    detail: format!("{} + {n}", state.total),
                })?;
                state.total = total;
                state.hot = true;
                state.seen = mix(state.seen, 1, *n as u64);
                Ok(ToyObs::Added { total })
            }
            ToyInput::Reset => {
                *state = ToyState::default();
                Ok(ToyObs::WasReset)
            }
            ToyInput::Tick => {
                if self.variant != Variant::Stranded {
                    state.hot = false;
                }
                Ok(ToyObs::Ticked { hot: state.hot })
            }
            ToyInput::Emit {
                target,
                op,
                payload,
            } => {
                let mut req = EffectRequest {
                    target: *target,
                    op: *op,
                    payload: payload.clone(),
                };
                match self.variant {
                    Variant::ShiftTarget => req.target = req.target.wrapping_add(1),
                    Variant::ShiftOp => req.op = req.op.wrapping_add(1),
                    Variant::AlterPayload => req.payload.push(0xEE),
                    _ => {}
                }
                let effect = |r| StepRefusal {
                    law: "toy.effect",
                    detail: format!("{r}"),
                };
                let result = if self.variant == Variant::SkipEffect {
                    Vec::new()
                } else {
                    port.submit(req.clone()).map_err(effect)?
                };
                if self.variant == Variant::ExtraEffect {
                    port.submit(req).map_err(effect)?;
                }
                state.seen = mix(state.seen, 3, u64::from(*target));
                Ok(ToyObs::Emitted { result })
            }
        }
    }
}

/// The corrected contract: a tick leaves nothing hot.
#[derive(Clone, Debug)]
pub struct TickClears {
    id: ArtifactId,
}

pub const TICK_CLEARS: &str = "toy.tick-clears";

pub fn contract() -> TickClears {
    TickClears {
        id: ArtifactId::new(
            ArtifactKind::Contract,
            TICK_CLEARS,
            1,
            b"every tick observes hot == false",
        ),
    }
}

/// The same judge under another contract artifact (a relabelled contract, not a swapped policy).
pub fn other_contract() -> TickClears {
    TickClears {
        id: ArtifactId::new(ArtifactKind::Contract, "toy.anything-goes", 1, b""),
    }
}

impl Contract<ToySchema> for TickClears {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn judge(&self, _schema: &ToySchema, _inputs: &[ToyInput], run: &Run<ToySchema>) -> Verdict {
        for (cursor, obs) in &run.observations {
            if *obs == (ToyObs::Ticked { hot: true }) {
                return Verdict::Fail {
                    cursor: *cursor,
                    law: TICK_CLEARS,
                    detail: "the tick left the accumulator hot".to_owned(),
                };
            }
        }
        Verdict::Pass
    }
}

pub fn task() -> ArtifactId {
    ArtifactId::new(ArtifactKind::Task, "toy-accumulator", 1, b"one window")
}

pub fn profile() -> ArtifactId {
    ArtifactId::new(ArtifactKind::Profile, "toy-profile", 1, b"single-threaded")
}

pub fn identity(variant: Variant) -> TaskIdentity {
    TaskIdentity {
        task: task(),
        code: code(variant).id().clone(),
        contract: contract().id().clone(),
        schema: ToySchema::new().id().clone(),
        profile: profile(),
    }
}

pub type ToyBoundary = Boundary<ToySchema, ToyCode, TickClears>;

/// A detached boundary over `state` running `variant`, judged by the corrected contract.
pub fn boundary_over(variant: Variant, state: ToyState, cap: usize) -> ToyBoundary {
    judged_by(variant, state, cap, contract())
}

/// A detached boundary over `state` running `variant`, constructed with `judge`.
pub fn judged_by<K: Contract<ToySchema>>(
    variant: Variant,
    state: ToyState,
    cap: usize,
    judge: K,
) -> Boundary<ToySchema, ToyCode, K> {
    Boundary::new(
        ToySchema::new(),
        identity(variant),
        code(variant),
        judge,
        state,
        cap,
    )
}

/// An attached boundary (log epoch 1, 64 events) over the default state.
pub fn attached(variant: Variant, cap: usize) -> ToyBoundary {
    let mut b = boundary_over(variant, ToyState::default(), cap);
    b.attach(1, 64).expect("a new boundary attaches");
    b
}

/// The host's live path for one input: admit, then (if it runs) the live code's step with no
/// external effects, then advance or withdraw.
pub fn live(b: &mut ToyBoundary, input: &ToyInput) -> (Admit, Option<Result<ToyObs, StepRefusal>>) {
    live_with(b, input, &mut NoEffects)
}

pub fn live_with(
    b: &mut ToyBoundary,
    input: &ToyInput,
    port: &mut dyn EffectPort,
) -> (Admit, Option<Result<ToyObs, StepRefusal>>) {
    let admit = b.admit(input);
    if admit != Admit::Run {
        return (admit, None);
    }
    let schema = b.schema().clone();
    let (code, state) = b.pair_mut();
    let r = code.step(&schema, state, input, port);
    if r.is_ok() {
        b.advance();
    } else {
        b.withdraw();
    }
    (admit, Some(r))
}

/// The host's step of `input` through the live code (no effects), with no `admit` before it and no
/// `advance` after: what a host does between the two.
pub fn apply(b: &mut ToyBoundary, input: &ToyInput) -> Result<ToyObs, StepRefusal> {
    let schema = b.schema().clone();
    let (code, state) = b.pair_mut();
    code.step(&schema, state, input, &mut NoEffects)
}

/// Feeds every input through the live path.
pub fn feed(b: &mut ToyBoundary, inputs: &[ToyInput]) {
    for i in inputs {
        live(b, i);
    }
}

/// The straight fold of `variant` over `inputs` from `start`, with no external effects.
pub fn fold(variant: Variant, start: ToyState, inputs: &[ToyInput]) -> Run<ToySchema> {
    aterm_aos::replay::replay(
        &ToySchema::new(),
        &code(variant),
        start,
        inputs,
        0,
        &mut NoEffects,
    )
}

pub fn adds(ns: &[i64]) -> Vec<ToyInput> {
    ns.iter().map(|n| ToyInput::Add(*n)).collect()
}
