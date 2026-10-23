// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Centralized audit trail for containment gate events (`containment_audit` log target).

use crate::ContainmentMode;

/// Single-argument `FnOnce` identity dispatcher (the aterm-tempfile Trust idiom): calling
/// `__log` from a closure behind it confines the unsafe `fmt::Arguments::new`, which the Trust
/// strict gate fails closed on, to [`forward`]'s closure.
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

/// Record a containment gate denial (`DENIED:` at `Warn`).
#[inline]
pub fn log_denial(subsystem: &str, operation: &str, mode: ContainmentMode, reason: &str) {
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

/// Record a non-denial containment posture or permit event (`CONTAINMENT:` at `Info`).
#[inline]
pub fn log_posture(subsystem: &str, operation: &str, mode: ContainmentMode, reason: &str) {
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
