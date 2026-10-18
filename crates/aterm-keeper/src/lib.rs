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

// `println!` in this crate is a line of a verb's report on stdout (`aterm keeper status`, run
// in-process by the `aterm` executable), written through `aterm_uds::stdout`: a reader that
// closed the pipe (`| head -1`) drops it, where std's panicked with exit 101 on the EPIPE. The
// usage goes through `aterm_uds::stdout::answer_exit`. Defined on every target, so no
// `println!` ever reaches std's; only Unix prints past the usage today (`cli`'s other targets
// refuse on stderr).
#[cfg_attr(
    not(unix),
    expect(
        unused_macros,
        reason = "every verb that prints past the usage is Unix-only; the macro stays defined \
                  so a future non-Unix line cannot reach std's panicking println!"
    )
)]
macro_rules! println {
    () => {
        ::aterm_uds::stdout::print(::std::format_args!("\n"))
    };
    ($($arg:tt)*) => {
        ::aterm_uds::stdout::println(::std::format_args!($($arg)*))
    };
}

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

/// Ends a test that checks code COULD NOT RUN while macOS's `trustd` does not answer:
/// every code check waits on it, so the silence is the machine's, not the code's.
/// (`tests/identity_after_swap.rs` holds the one other copy: it is its own crate.)
#[cfg(all(test, target_vendor = "apple"))]
fn require_trust_service() {
    if aterm_uds::deadline::trust_service_silent() {
        aterm_spec::host::could_not_run(
            "macOS's trustd does not answer, so no code check runs here — the machine's, not the code's",
        );
    }
}

/// Tier-1 of `PtyKeeperCustody`, `KeeperRelaunchBrake` and
/// `KeeperDeathJudgement` over the real core.
#[cfg(all(test, unix))]
mod conformance_custody;

/// Tier-1 of `KeeperCompanionCustody` (the crash-restore companions) over the real core.
#[cfg(all(test, unix))]
mod conformance_companion;
