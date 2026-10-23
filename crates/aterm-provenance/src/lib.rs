// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Type-level data-flow provenance markers for PTY input.
//!
//! * [`Origin`] is a sealed trait implemented by the [`Pty`] marker.
//! * [`Provenance<T, O>`] wraps a value with a compile-time origin marker (`#[repr(transparent)]`).

mod origin;
mod provenance;
mod pty_wrap;

pub use origin::{Origin, Pty};
pub use provenance::{
    Provenance, pty_slice_byte_at, pty_slice_has_csi_prefix, pty_slice_has_dcs_prefix,
    pty_slice_has_osc_prefix, pty_slice_u16_be_at, pty_slice_u16_le_at, pty_slice_u32_be_at,
    pty_slice_u32_le_at, pty_slice_u64_be_at, pty_slice_u64_le_at,
};
pub use pty_wrap::pty_wrap_ref;
