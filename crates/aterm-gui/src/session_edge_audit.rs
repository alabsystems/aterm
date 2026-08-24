// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `session_edge` AUDIT seam (design §1.4#5, §7): one structured, hex-free
//! event per connection-authority act — grant, token revoke, source sweep —
//! emitted identically for the wire verbs and the UI paths, on the dedicated
//! log target `session_edge` (the `containment_audit` pattern) so operators can
//! route/filter the authority stream independently of general logging:
//!
//! ```text
//! RUST_LOG=session_edge=info
//! ```
//!
//! The bearer token NEVER appears in an event: an act is audited by its
//! `(src, dst, op)` identity. `EdgeToken`'s `Debug` is redacted for the same
//! reason — nothing routed through here may weaken that.

use aterm_session::SessionId;

/// The closed vocabulary of audited authority acts. An enum rather than a free
/// string so a call site cannot typo a new action into the stream — the
/// `action=` grammar is part of the surface operators grep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EdgeAction {
    /// One edge minted (the `grant` verb / a connection mint — one event per op).
    Grant,
    /// One edge revoked by its bearer token.
    Revoke,
    /// EVERY edge from one source swept (`revoke src=<sid>` / source death,
    /// design §1.4#4).
    RevokeSrc,
    /// One carried seamless-handoff triple NOT re-established at adoption
    /// (design §1.4#6 fail-soft): its endpoint vanished across the swap, or
    /// its op is outside the connection vocabulary the one kind-bounded mint
    /// helper can express (a re-mint there would WIDEN authority). Dropping
    /// only loses authority — audited so the dissolution is on record.
    Drop,
}

impl EdgeAction {
    /// The stable `action=` field value.
    fn as_str(self) -> &'static str {
        match self {
            EdgeAction::Grant => "grant",
            EdgeAction::Revoke => "revoke",
            EdgeAction::RevokeSrc => "revoke_src",
            EdgeAction::Drop => "drop",
        }
    }
}

/// Emit one `session_edge` audit event:
/// `EDGE: action=<act> origin=<origin> src=<sid> dst=<sid> op=<op>`.
///
/// `origin` names the surface that performed the act — `"wire"` for the
/// control-socket verbs; UI paths pass their own surface (`"menu"`/`"drag"`,
/// design §7). `op` is the edge's wire token ([`aterm_session::Op::as_str`]),
/// or `"*"` for a source sweep (it removes every op the source held). Emitted
/// at `Info` — these are acts, not denials — mirroring `containment_audit`'s
/// posture tier, so `RUST_LOG=session_edge=info` shows the full trail.
pub(crate) fn emit(action: EdgeAction, origin: &str, src: &SessionId, dst: &SessionId, op: &str) {
    // Direct `__log` (the `containment_audit` idiom): the log macros stamp
    // `module_path!()` as the target, and this stream's whole point is the
    // dedicated, stable `session_edge` target.
    aterm_log::__log(
        aterm_log::Level::Info,
        "session_edge",
        format_args!(
            "EDGE: action={} origin={origin} src={} dst={} op={op}",
            action.as_str(),
            src.as_str(),
            dst.as_str(),
        ),
        Some(file!()),
        Some(line!()),
    );
}
