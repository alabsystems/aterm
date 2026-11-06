// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOOLCHAIN, ONE SOURCE, STD ONLY BUT FOR THE SIGNAL DECLARATIONS A WAIT'S INTERRUPT USES,
//! THE PROCESS SWEEP THAT TELLS A WAIT'S HOLDER AT WORK FROM ONE STUCK, AND THE SHA-256 OF THE
//! STATUS DIGEST A SEALED TOOLCHAIN IS RECHECKED BY.

pub mod build_cache;
pub mod cargo_cache;
pub mod checkout;
pub mod discovery;
pub mod gnu_runtime;
pub mod interrupt;
pub mod lasting;
pub mod layout;
pub mod lease;
pub mod named_lock;
pub mod record_lock;
pub mod rustup;
pub mod stall;
pub mod stat_digest;
pub mod waits;

pub use discovery::*;
