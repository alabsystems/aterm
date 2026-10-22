// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Aos hosted execution: the generic substrate (design of record:
//! `docs/DESIGN-aos-hosted-execution-2026-10-21.md` §3, from Trust's
//! `docs/plans/2026-10-21-aos-hosted-execution.md` §3–§4).
//!
//! One hosted *task* is a pure step function over owned state: `step(code, state, input)
//! → (state', observation)`, with external effects only through an [`effects::EffectPort`].
//! This crate holds everything about such a task that does not depend on what the task is:
//!
//! | Module | Law it carries |
//! |---|---|
//! | [`ident`] | Every artifact (task, code, contract, schema, profile) is named by a domain-separated SHA-256 [`ident::Digest`]; a [`ident::TaskIdentity`] binds all five. |
//! | [`wire`] | One canonical little-endian codec: a decoder refuses every non-canonical byte, names the field it refused, and never panics. |
//! | [`task`] | The [`task::Schema`] / [`task::Code`] traits and the canonical state digest. |
//! | [`effects`] | The effect frontier: shadow replay compares each request with the next recorded one *before* handing back its recorded result, holds no live sink, and stops for good at the first divergence. |
//! | [`log`] | The ordered input log: dense CRC-framed step records; a torn tail is dropped and reported, damage followed by a whole record is refused. |
//! | [`checkpoint`] | Full-copy, immutable checkpoints with their provenance; decode re-verifies the state digest, restore re-checks identity. |
//! | [`replay`] | Deterministic replay, the observation chain, and the first divergence between two runs. |
//! | [`contract`] / [`evidence`] | The corrected contract a repair is judged by, and the witness only `prepare` can build. |
//! | [`publish`] | Same-schema boundary publication: pause, prepare, commit as one value with a new revision, or reject; the queue of inputs that arrive meanwhile is delivered once, in order. |
//!
//! # What this crate never does
//!
//! No clock, thread, file, socket, randomness, `unsafe`, proc macro or global mutable state:
//! time and bytes enter only as the host's recorded inputs (`clippy.toml` bans the clocks).
//! Every decoder is a pure, panic-free function of its bytes; every cursor, length and
//! counter uses checked arithmetic. The evidence it can produce is
//! [`evidence::EvidenceGrade::Tested`] — shadow replay over one recorded trace — never a
//! proof.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod checkpoint;
pub mod contract;
pub mod effects;
pub mod evidence;
pub mod ident;
pub mod log;
pub mod publish;
pub mod replay;
pub mod task;
pub mod wire;

pub use ident::{ArtifactId, ArtifactKind, BranchId, Digest, Revision, TaskIdentity};
