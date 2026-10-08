// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Centralized audit trail for containment gate events.
//!
//! Every gate that denies an operation calls [`log_denial`] before returning an
//! error; posture/permit events use [`log_posture`]. All events use the log
//! target `containment_audit`. The `DENIED:` prefix is reserved for real
//! denials, so filtering on `DENIED` returns only denials (posture lines use
//! `CONTAINMENT:` at `Info`).
//!
//! ## Log target
//!
//! Route the target to a dedicated audit sink, e.g.:
//!
//! ```text
//! RUST_LOG=containment_audit=warn
//! ```

use crate::ContainmentMode;

/// Single-argument `FnOnce` identity dispatcher (the aterm-tempfile Trust idiom).
///
/// [`aterm_log::__log`] requires a `fmt::Arguments`, and a runtime one can only
/// be built with the unsafe `fmt::Arguments::new`, which the Trust strict
/// gate's native lowering fails closed on. Calling `__log` from a closure
/// behind this helper's `FnOnce::call_once` confines that gap to the closure in
/// [`forward`], so `log_denial`/`log_posture` and `forward` still verify.
/// `f(a)` is the same call with the same arguments.
#[inline]
fn call1<F, A>(f: F, a: A)
where
    F: FnOnce(A),
{
    f(a);
}

/// Forward one pre-rendered audit line to [`aterm_log::__log`] as a single
/// `{m}` display of `msg`. See [`call1`] for why the `format_args!` lives in a
/// dispatched closure.
#[inline]
fn forward(level: aterm_log::Level, msg: &str, file: &'static str, line: u32) {
    call1(
        move |m: &str| {
            aterm_log::__log(
                level,
                "containment_audit",
                format_args!("{m}"),
                Some(file),
                Some(line),
            );
        },
        msg,
    );
}

/// Record a containment gate denial.
///
/// Called by gate sites before returning a denial error.
///
/// # Arguments
///
/// * `subsystem` — The gate's domain (e.g. `"spawn"`, `"control_socket"`).
/// * `operation` — What was attempted (e.g. `"spawn initial shell"`, `"image write '<path>'"`).
/// * `mode` — The active containment mode that triggered the denial.
/// * `reason` — Why the operation was denied (e.g. `"path escapes images/ subdir or names a nested target"`).
#[inline]
pub fn log_denial(subsystem: &str, operation: &str, mode: ContainmentMode, reason: &str) {
    // Assembled without `format!` (see `call1`) as
    // `DENIED: {subsystem}::{operation} in {mode} mode — {reason}`. No
    // `with_capacity`: an input-derived capacity is unbounded under the
    // verifier's open model.
    let mut msg = String::new();
    msg.push_str("DENIED: ");
    msg.push_str(subsystem);
    msg.push_str("::");
    msg.push_str(operation);
    msg.push_str(" in ");
    msg.push_str(mode.name());
    msg.push_str(" mode — ");
    msg.push_str(reason);
    forward(aterm_log::Level::Warn, &msg, file!(), line!());
}

/// Record a non-denial containment posture/permit event.
///
/// Same `containment_audit` target as [`log_denial`] without the `DENIED:`
/// prefix, emitted at [`Info`](aterm_log::Level::Info) (below the `Warn`
/// denials), so `RUST_LOG=containment_audit=info` shows posture and `=warn`
/// only denials.
///
/// # Arguments
///
/// * `subsystem` — The gate's domain (e.g. `"spawn"`).
/// * `operation` — What was decided (e.g. `"os-network-sandbox"`).
/// * `mode` — The active containment mode for the decision.
/// * `reason` — The posture recorded (e.g. `"OS sandbox applied: no network; …"`).
#[inline]
pub fn log_posture(subsystem: &str, operation: &str, mode: ContainmentMode, reason: &str) {
    // Assembled as `CONTAINMENT: {subsystem}::{operation} in {mode} mode —
    // {reason}`; see `log_denial`/`call1`.
    let mut msg = String::new();
    msg.push_str("CONTAINMENT: ");
    msg.push_str(subsystem);
    msg.push_str("::");
    msg.push_str(operation);
    msg.push_str(" in ");
    msg.push_str(mode.name());
    msg.push_str(" mode — ");
    msg.push_str(reason);
    forward(aterm_log::Level::Info, &msg, file!(), line!());
}
