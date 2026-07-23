// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Centralized audit trail for containment gate events (#5533).
//!
//! Every containment gate that denies an operation calls [`log_denial`]
//! before returning an error; non-denial posture/permit events use
//! [`log_posture`]. All events share the log target `containment_audit`,
//! allowing operators to filter and aggregate security events independently
//! of general application logging. The `DENIED:` prefix is reserved for
//! actual denials so a `containment_audit`-stream filter on `DENIED` returns
//! only true denials (posture lines use `CONTAINMENT:` at `Info`).
//!
//! ## Log target
//!
//! All audit events use `target: "containment_audit"`. Configure your
//! log backend to route this target to a dedicated audit sink:
//!
//! ```text
//! RUST_LOG=containment_audit=warn
//! ```

use crate::ContainmentMode;

/// Single-argument `FnOnce` identity dispatcher (the aterm-dev/aterm-tempfile
/// Trust idiom).
///
/// Trust gate note: a runtime-argument `format_args!` expansion places the
/// unsafe `fmt::Arguments::new` constructor in the expanding function's MIR,
/// which the strict gate's native TrustIr lowering fails closed on
/// (`Call target `std::fmt::Arguments::<'a>::new` is not present in the
/// TrustIr module`). [`aterm_log::__log`] *requires* a `fmt::Arguments`, and
/// the only safe constructor (`Arguments::from_str`) is `&'static str`-only,
/// so a runtime audit message cannot avoid `Arguments::new` anywhere in this
/// crate — that one construct is a known, documented gate gap. Routing the
/// `__log(.., format_args!("{m}"), ..)` call through a closure invoked behind
/// this helper's polymorphic `FnOnce::call_once` dispatch CONFINES the
/// fail-closed item to the three-line closure in [`forward`]: the message
/// assembly in `log_denial`/`log_posture` and `forward` itself then verify
/// panic-free (the dispatch is scoped out like a dependency call), instead
/// of the whole public entry points being unverifiable. `f(a)` runs the
/// exact same call with the exact same arguments: behavior is identical.
#[inline]
#[allow(
    clippy::doc_markdown,
    reason = "Trust-internals note quotes a verifier error message naming many :: paths (TrustIr, std::fmt::Arguments::<'a>::new); backticking inside the already-quoted message would distort it"
)]
fn call1<F, A>(f: F, a: A)
where
    F: FnOnce(A),
{
    f(a);
}

/// Forward one pre-rendered audit line to [`aterm_log::__log`] as a single
/// `{m}` display of `msg` — byte-identical to formatting the message inline
/// (str's `Display` is a verbatim write; no width/fill/precision options).
/// See [`call1`] for why the `format_args!` lives inside a dispatched closure.
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
/// Called by gate sites across all subsystems before returning a denial
/// error. The consistent log target `containment_audit` allows log
/// backends to route all security events to a dedicated audit sink.
///
/// # Arguments
///
/// * `subsystem` — The gate's domain (e.g. `"process"`, `"mcp"`, `"network"`, `"plugins"`).
/// * `operation` — What was attempted (e.g. `"spawn '/bin/bash'"`, `"tool 'run_command'"`).
/// * `mode` — The active containment mode that triggered the denial.
/// * `reason` — Why the operation was denied (e.g. `"NoFork"`, `"not in allowlist"`).
#[inline]
pub fn log_denial(subsystem: &str, operation: &str, mode: ContainmentMode, reason: &str) {
    // Assembled without `format!`/`format_args!` runtime captures (see
    // `call1`); the push sequence is byte-identical to the former
    // `format_args!("DENIED: {subsystem}::{operation} in {mode} mode — {reason}")`
    // (`ContainmentMode::name` IS the `Display` rendering). No `with_capacity`
    // pre-size: an input-length-derived capacity hint is unbounded under the
    // verifier's open model, and capacity is not observable behavior.
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
/// The neutral counterpart to [`log_denial`]: it shares the same
/// `containment_audit` target (single-stream visibility) but does NOT emit the
/// `DENIED:` prefix, so an operator filtering the audit stream for `DENIED`
/// counts only true denials. Use this for permits and for recording the OS
/// sandbox posture (in force / not in force) at a gate that is NOT denying.
/// Emitted at [`Info`](aterm_log::Level::Info) — below the `Warn` denials — so
/// `RUST_LOG=containment_audit=info` shows posture while `=warn` shows only
/// denials.
///
/// # Arguments
///
/// * `subsystem` — The gate's domain (e.g. `"spawn"`, `"process"`, `"network"`).
/// * `operation` — What was decided (e.g. `"os-network-sandbox"`, `"spawn initial shell"`).
/// * `mode` — The active containment mode for the decision.
/// * `reason` — The posture recorded (e.g. `"OS sandbox ACTUATED via sandbox-exec …"`).
#[inline]
pub fn log_posture(subsystem: &str, operation: &str, mode: ContainmentMode, reason: &str) {
    // Byte-identical manual assembly of the former
    // `format_args!("CONTAINMENT: {subsystem}::{operation} in {mode} mode — {reason}")`;
    // see `log_denial`/`call1` for the Trust rationale.
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
