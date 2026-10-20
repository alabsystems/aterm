// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONTROL LANE'S DOOR: [`TapedCtl`] wraps a loop's transport, so every
//! request the loop sends and every reply it gets back crosses its [`Fx`] as
//! one round trip.
//!
//! It wraps the TRANSPORT, not the session's own `call`: the session's
//! requests that skip `Session::call` — the claim's renewal and release
//! (`meta set|unset supervisor`), the worker's `who` — still go through the
//! one transport the session holds, so they are taped by construction, and no
//! new bypass can be written that is not. An older build's teller lane (the
//! removed `aterm drive watch`'s `story`, its own client) crossed a door of
//! its own ([`super::doors::TELLER`]), so a reader of its tapes still tells
//! its replies, which no decision read, from the loop's.
//!
//! The door also closes the tape's open frame where a reader of a live tape
//! needs it on disk: before a WAIT (`await …`: an idle loop's tape is on disk
//! while it waits, which may be for hours) and after an ACT (a key, a send,
//! a post, a badge, a signal, a restart: what the loop acted on is on disk
//! before anything that follows). A live loop's door does neither: `Fx`
//! does nothing for a frame it does not keep.
//!
//! In a live run (no `--tape`) the wrapper is a pass-through: the request's
//! bytes are never built and nothing is locked.

use super::frame::Hint;
use crate::supervise::fx::Fx;
use crate::supervise::{Ctl, CtlReply, Interrupter};

/// A transport whose round trips cross a door ([module docs](self)).
pub(crate) struct TapedCtl<'c, C: Ctl + ?Sized> {
    inner: &'c mut C,
    fx: Fx,
    door: &'static str,
}

impl<'c, C: Ctl + ?Sized> TapedCtl<'c, C> {
    /// `inner`'s round trips, through `fx` at `door` ([`super::doors::CTL`] for the
    /// loop's lane).
    pub(crate) fn new(inner: &'c mut C, fx: Fx, door: &'static str) -> Self {
        Self { inner, fx, door }
    }
}

impl<C: Ctl + ?Sized> Ctl for TapedCtl<'_, C> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        if is_wait(args) {
            self.fx.flush(Hint::BeforeWait);
        }
        let inner = &mut *self.inner;
        let reply = self.fx.exchange(
            self.door,
            || super::codec::ctl_request(args),
            || inner.call(args),
        );
        if is_act(args) {
            self.fx.flush(Hint::Act);
        }
        reply
    }

    fn interrupter(&self) -> Option<Interrupter> {
        self.inner.interrupter()
    }
}

/// A REPLAY's control lane: every request answered from the tape through a
/// replaying [`Fx`] at `door`, checked there against the recorded one. It
/// holds no transport at all — no socket, no client, no stand-in — so a
/// replay cannot reach a live session by construction, and it has no
/// [`Interrupter`]: nothing it waits on can be cut short, because nothing it
/// waits on is real.
pub(crate) struct ReplayCtl {
    fx: Fx,
    door: &'static str,
}

impl ReplayCtl {
    /// The lane at `door` of a replay whose door is `fx`.
    ///
    /// # Panics
    ///
    /// `fx` does not replay: a lane with no transport over a door that would
    /// call its live world has nothing to call.
    pub(crate) fn new(fx: Fx, door: &'static str) -> Self {
        assert!(
            fx.replaying(),
            "a replay's lane over a door that does not replay"
        );
        Self { fx, door }
    }
}

impl Ctl for ReplayCtl {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.fx.exchange(
            self.door,
            || super::codec::ctl_request(args),
            || unreachable!("a replaying door never makes its live call"),
        )
    }
}

/// The request's words after any `@sid` selector.
fn words<'a>(args: &'a [&'a str]) -> impl Iterator<Item = &'a str> {
    args.iter().copied().filter(|a| !a.starts_with('@'))
}

/// A wait: `await …`.
pub(crate) fn is_wait(args: &[&str]) -> bool {
    words(args).next() == Some("await")
}

/// An ACT the loop takes on the session: a request that writes into the
/// worker or its badge ([`crate::supervise::run::is_write`]: a key, a send, a
/// turn, a paste, a post, an attention write), a signal, or a restart. A
/// claim's renewal, a read and a wait are not.
pub(crate) fn is_act(args: &[&str]) -> bool {
    crate::supervise::run::is_write(args)
        || matches!(words(args).next(), Some("signal" | "restart"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The act classes the frame closes on: writes, attention, signals and
    /// restarts are acts; a claim's renewal, a read and a wait are not, and a
    /// selector in front changes nothing.
    #[test]
    fn acts_and_waits_are_told_apart_by_their_verb() {
        for act in [
            &["key", "1"][..],
            &["@s-1", "send", "hi"],
            &["turn", "go"],
            &["post", "@s-2", "x"],
            &["meta", "set", "attention", "x"],
            &["meta", "unset", "attention"],
            &["signal", "INT"],
            &["restart", "--pid", "9"],
        ] {
            assert!(is_act(act), "{act:?} is an act");
            assert!(!is_wait(act), "{act:?} is no wait");
        }
        for other in [
            &["text", "--json", "tail=40"][..],
            &["meta", "set", "supervisor", "aterm-supervise:1"],
            &["meta", "unset", "supervisor"],
            &["status"],
            &["@s-1", "who"],
            &["story", "approved"],
        ] {
            assert!(!is_act(other), "{other:?} is no act");
            assert!(!is_wait(other), "{other:?} is no wait");
        }
        assert!(is_wait(&["@s-1", "await", "seq", "4", "timeout", "20000"]));
        assert!(!is_act(&["await", "seq", "4"]));
    }
}
