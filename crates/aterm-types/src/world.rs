// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A scratch world: the private HOME, configuration and runtime folders a check, a measurement
//! or a smoke starts an aterm in. That aterm is never the person's, so it starts none of the
//! automatic lanes that act for the login's own account or reach the person's real LAN, and none
//! that can put an OS consent prompt on the person's desk: macOS keys the firewall's
//! incoming-connection question and a background item to the account and the app, never to
//! HOME, so a world cannot answer them privately and must not ask them at all.
//!
//! A world says what it is in the two places the lanes read, because a release build of aterm
//! (no development seams, [`crate::dev_seam!`]) reads only some of them:
//! - its environment carries [`MARKER`] at [`state_home`], which the lanes that reach the local
//!   network read as a plain environment variable in every build: the converge runner's window
//!   lane, the `_alab._tcp` browse, the Commons runtime, and the node job's updater and account
//!   claims;
//! - its `aterm.toml` carries [`LANES_OFF`], each table the product's own switch for one lane.
//!
//! The fleet listener reads neither: which vault it reads follows where its state root resolved
//! ([`crate::dirs::runs_under_private_state_root`]), so a release build in a world reads its
//! HOME's vault, and `[swarm]` in [`LANES_OFF`] keeps it from listening.
//!
//! verify measure's world, the GUI smoke's sandbox, the perf arena's windowed lanes and the
//! headless images a macOS release link's layout probe measures (`crates/aterm/build.rs`, laid
//! out by `probe_world` in `crates/aterm/src/macho_order.rs`) take both. That probe runs in a
//! wrapper that links no crate, so the words it needs ([`MARKER`], [`state_home`] and
//! [`LANES_OFF`]) live in a `std`-only file of their own (`world_words.rs`), which it mounts by
//! `#[path]` and this module re-exports. The lab's worlds (`aterm-lab`'s `instance::World`) carry
//! the marker at this folder and keep their own tables, which have no `[swarm]` table, and two of
//! its drives (`commons_lan`, `commons_startup`) remove the marker to drive the local-network
//! lanes themselves.

#[path = "world_words.rs"]
mod words;

pub use words::{LANES_OFF, MARKER, state_home};

#[cfg(test)]
#[path = "world_tests.rs"]
mod tests;
