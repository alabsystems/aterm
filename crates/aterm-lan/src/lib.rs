// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The root LAN broker (`docs/DESIGN-lan-broker-2026-11-05.md`): `aterm lan serve`, run by the
//! system LaunchDaemon [`aterm_uds::lan::LABEL`] as root, which macOS's Local Network privacy
//! exempts (`docs/measured/local-network-exemption-2026-11-05.md`).
//!
//! It makes aterm's own outbound LAN sockets (a TCP connect, a UDP socket with its groups) and
//! passes them to the asking process over its Unix socket, resolves local names, and makes the
//! DNS-SD calls (browse, resolve, query, register) itself, streaming what they hear. It serves
//! only local-network destinations, runs no code and opens no file a client names, and admits a
//! client by its code signature: the broker's own team requirement, or an ad-hoc image's own
//! designated requirement. The wire and the client are `aterm_uds::lan`'s.

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

#[cfg(unix)]
mod bonjour;
#[cfg(target_os = "macos")]
mod bplist;
#[cfg(target_os = "macos")]
mod direct;
#[cfg(target_os = "macos")]
pub mod grant;
#[cfg(unix)]
mod server;

#[cfg(target_os = "macos")]
mod adopt;

#[cfg(target_os = "macos")]
pub use direct::provide_direct;
#[cfg(unix)]
pub use server::{Opts, serve};

#[cfg(all(test, unix))]
mod tests;
