// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-keeper**: the PTY keeper (`docs/DESIGN-pty-keeper-2026-09-26.md`).
//!
//! A window that dies without running its exit path (a fatal signal, jetsam, Force Quit) hangs
//! up every shell whose master it alone holds. The keeper is a per-login launchd job that holds
//! a custody copy of every master, never reads it, and hands a crashed window's masters to the
//! next window through the seamless update's adoption landing. It is opt-in: a window registers
//! only under `[keeper] enabled = true`, and only `aterm keeper start` starts one.
//!
//! * [`core`]: the pure custody table and its transitions, the death classifier, the orphan
//!   prune, offer exclusivity and the relaunch brake, with the `#[refines]` anchors of
//!   `PtyKeeperCustody`, `KeeperRelaunchBrake` and `KeeperDeathJudgement`.
//! * [`wire`]: the AKP1 codec; [`frameio`]: one frame and its descriptor on a stream.
//! * [`server`]: the single-threaded `poll` loop over real sockets and the kernel.
//! * [`client`]: the window's side, one connection and the `KeeperLink` worker.
//! * [`identity`]: who may connect, both ways: the same uid, and this build's designated
//!   requirement.
//! * [`job`]: the launchd label, the socket, and `launchctl submit|remove|print`.
//! * [`code`]: the copy of its own code a keeper runs from, so an update that deletes its build
//!   leaves it checkable.
//! * [`cli`]: `aterm keeper serve|status|start|stop`.
//!
//! It links no AppKit, CoreGraphics or winit (guards `keeper-gui`, `keeper-gui-deps`): it
//! outlives the window because it is not a GUI process.

// Register the `trust` tool namespace for the `trust::skip` opt-outs; inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

pub mod cli;
pub mod core;
pub mod identity;
pub mod job;
pub mod wire;

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod code;
#[cfg(unix)]
pub mod frameio;
#[cfg(unix)]
pub mod server;
#[cfg(unix)]
mod sys;

pub use crate::core::{KeeperCore, RelaunchBrake};

/// Tier-1 of `PtyKeeperCustody`, `KeeperRelaunchBrake` and
/// `KeeperDeathJudgement` over the real core.
#[cfg(all(test, unix))]
mod conformance_custody;
