// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Private proxy control data. Decoding creates no resource authority. The
//! custodian must authenticate the actual native producer, original live job,
//! CPU/GNU descriptions and retained memory before launching a member.

mod context;
mod helper_fds;
mod locator;
pub use helper_fds::HelperFds;
mod types;
pub use context::*;
mod validate;
mod wire;
pub use types::*;
pub use wire::{Cancel, ProducerChallenge};
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod channel;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use channel::Channel;
#[cfg(test)]
mod tests;

#[cfg(all(feature = "client", any(target_os = "linux", target_os = "macos")))]
pub mod client;
