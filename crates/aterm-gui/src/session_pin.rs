// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A session's pin (`aterm ctl @<sid> pin`, `unpin`): a person's or a lead agent's mark that keeps
//! the session first among its Mac's agents in the fleet band. It is one flag of the session's
//! meta ([`crate::session_timeline::SessionMeta::pinned`]): a fresh session, a worker among them,
//! starts unpinned; a restore and an update carry it with the session's other user fields; and
//! `status` and the survey row say it as `pinned=0|1`.

/// `pin` (`pinned` true) or `unpin` on `ctx`'s session, written by `by`: the reply, and whether
/// the pin moved. Either verb takes no argument, and answers the same whether or not it moved.
pub(crate) fn cmd(
    ctx: &crate::SessionCtx,
    pinned: bool,
    rest: &str,
    by: aterm_slog::Actor,
) -> (String, bool) {
    if !rest.trim().is_empty() {
        let verb = if pinned { "pin" } else { "unpin" };
        return (format!("ERR usage: {verb} (it takes no argument)\n"), false);
    }
    let moved = crate::session_timeline::set_pinned(ctx, pinned, by);
    (format!("OK pinned={}\n", u8::from(pinned)), moved)
}

/// `status`'s and the survey row's `pinned=` value for `meta`: `1` or `0`.
pub(crate) fn field(meta: &crate::session_timeline::SessionMeta) -> u8 {
    u8::from(meta.pinned)
}

#[cfg(test)]
#[path = "session_pin_tests.rs"]
mod tests;
