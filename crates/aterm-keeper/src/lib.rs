// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **aterm-keeper** — the PTY keeper (`docs/DESIGN-pty-keeper-2026-09-26.md`).
//!
//! Today the window holds every terminal's master alone, so a window that dies
//! without running its exit path — a fatal signal, a jetsam or Force Quit
//! kill — hangs up every shell and every agent in it. The keeper is a second
//! holder that outlives the window: a per-login launchd job that holds a
//! CUSTODY COPY of every master, never reads it, and hands a crashed window's
//! masters to the next window through the adoption landing the seamless update
//! already uses.
//!
//! P3 is OPT-IN: a window registers with a keeper only under `[keeper] enabled
//! = true` (`aterm-gui`'s `keeper_link`), and nothing starts one by itself
//! (`aterm keeper start` does, by hand; P4 makes it the default). What is here:
//!
//! * [`core`] — the PURE keeper: the custody table and its transitions, the
//!   §5.4 death classifier (with row 3's crash-marker cross-check), the
//!   prune of an orphan whose shell died, offer exclusivity and the relaunch
//!   brake. It carries the `#[refines]` anchors of `PtyKeeperCustody`,
//!   `KeeperRelaunchBrake` and `KeeperDeathJudgement` (`aterm_spec::derive`).
//! * [`wire`] — the AKP1 codec; [`frameio`] — one frame and its descriptor on a
//!   stream.
//! * [`server`] — the single-threaded `poll` loop over real sockets and the
//!   real kernel (exit watches, the holder scan, `fstat` possession checks).
//! * [`client`] — the window's side: one connection, and the `KeeperLink`
//!   worker with its bounded queue.
//! * [`identity`] — who may connect, both ways: the same uid, and code that
//!   satisfies this build's designated requirement.
//! * [`job`] — the launchd label (`com.aterm.aterm.keeper`), the socket, and
//!   `launchctl submit|remove|print`.
//! * [`cli`] — `aterm keeper serve|status`.
//!
//! It links no AppKit, CoreGraphics or winit (`tools/grep_guard.sh` B19): the
//! keeper outlives the window precisely because it is not a GUI process.

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-outs on the FFI wrappers
// resolve; plain rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

pub mod cli;
pub mod core;
pub mod identity;
pub mod job;
pub mod wire;

#[cfg(unix)]
pub mod client;
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
