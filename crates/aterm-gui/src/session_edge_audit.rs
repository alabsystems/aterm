// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `session_edge` audit stream: one event per connection-authority act (grant, revoke,
//! source sweep, handoff drop), the same for wire verbs and UI paths, on its own log target so
//! operators can filter it (`RUST_LOG=session_edge=info`).
//!
//! The bearer token never appears in an event: an act is identified by `(src, dst, op)`.

use aterm_session::SessionId;

/// The audited authority acts; closed so the `action=` values operators grep stay fixed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EdgeAction {
    /// One edge minted (one event per op).
    Grant,
    /// One edge revoked by its bearer token.
    Revoke,
    /// Every edge from one source swept (`revoke src=<sid>` or the source's close).
    RevokeSrc,
    /// One carried seamless-handoff edge not re-established at adoption: its endpoint vanished
    /// across the swap, or re-minting its op would widen authority. Dropping only loses authority.
    Drop,
}

impl EdgeAction {
    /// The stable `action=` field value.
    const fn as_str(self) -> &'static str {
        match self {
            EdgeAction::Grant => "grant",
            EdgeAction::Revoke => "revoke",
            EdgeAction::RevokeSrc => "revoke_src",
            EdgeAction::Drop => "drop",
        }
    }
}

/// Emit one `session_edge` audit event at `Info`:
/// `EDGE: action=<act> origin=<origin> src=<sid> dst=<sid> op=<op>`.
///
/// `origin` names the surface that acted: `"wire"` for the control-socket verbs, else the UI or
/// lifecycle path (`"menu"`, `"drag"`, `"close"`, `"handoff"`). `op` is the edge's wire token
/// ([`aterm_session::Op::as_str`]), or `"*"` for a source sweep.
pub(crate) fn emit(action: EdgeAction, origin: &str, src: &SessionId, dst: &SessionId, op: &str) {
    // Direct `__log`: the log macros would stamp `module_path!()` as the target.
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
