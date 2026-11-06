// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Read-only provenance for the canonical native retained custodian.
//! Discovery never creates account state or grants resources. A selected live
//! process, immutable image, original socket and actual native ancestry remain
//! checked through the caller's bounded exchange; JSON and paths alone confer
//! no authority.

#[cfg(unix)]
mod connection;
#[cfg(unix)]
mod private;
#[cfg(unix)]
pub mod process;
#[cfg(unix)]
mod reader;
#[cfg(unix)]
pub mod schema;
#[cfg(unix)]
pub use reader::{Host, Instance, PinnedHelper, Store};
