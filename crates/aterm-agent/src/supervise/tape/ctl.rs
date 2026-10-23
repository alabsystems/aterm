// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control lane transport wrappers for recording and replaying supervision tapes.

use super::frame::Hint;
use crate::supervise::fx::Fx;
use crate::supervise::{Ctl, CtlReply, Interrupter};

/// A transport whose round trips cross a door.
pub(crate) struct TapedCtl<'c, C: Ctl + ?Sized> {
    inner: &'c mut C,
    fx: Fx,
    door: &'static str,
}

impl<'c, C: Ctl + ?Sized> TapedCtl<'c, C> {
    /// Wraps `inner`'s round trips through `fx` at `door`.
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

/// A replay's control lane that answers every request from the tape.
pub(crate) struct ReplayCtl {
    fx: Fx,
    door: &'static str,
}

impl ReplayCtl {
    /// Creates the lane at `door` for a replay whose door is `fx`.
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

/// Returns whether `args` is an `await` command.
pub(crate) fn is_wait(args: &[&str]) -> bool {
    words(args).next() == Some("await")
}

/// Returns whether `args` is an action that modifies the session or its process.
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
