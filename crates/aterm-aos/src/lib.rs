// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Aos hosted execution.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod checkpoint;
pub mod contract;
pub mod effects;
pub mod evidence;
pub mod ident;
pub mod log;
pub mod publish;
pub mod registry;
pub mod replay;
pub mod task;
pub mod wire;

pub use ident::{ArtifactId, ArtifactKind, BranchId, Digest, Revision, TaskIdentity};
