// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control lane transport wrappers for recording and replaying supervision tapes.

use super::codec::{self, CodecError};
use super::core::TapeValue;
use super::doors::{CTL_AGENT_ACT, CTL_AUTHORITY as DUTY_AUTHORITY, CTL_DUTY as DUTY_ACT};
use super::frame::Hint;
use crate::harness::agent_image::Act;
use crate::harness::contract::Scope;
use crate::supervise::fx::Fx;
use crate::supervise::run::duty::DutyReply;
use crate::supervise::{BuiltinAuthority, Ctl, CtlReply, Interrupter};

pub(crate) fn duty_request(duty: Scope, revision: u64, args: &[&str]) -> Vec<u8> {
    let revision = revision.to_string();
    let mut words = Vec::with_capacity(args.len() + 2);
    words.push(duty.word());
    words.push(&revision);
    words.extend_from_slice(args);
    super::codec::ctl_request(&words)
}

/// Decode the complete typed envelope before looking at its actual argv. Its scope and
/// revision remain in the original bytes used for exact act comparison.
pub(crate) fn duty_parts(request: &[u8]) -> Result<(Scope, u64, Vec<String>), CodecError> {
    let words = codec::ctl_args(request)?;
    let bad = || CodecError {
        at: 0,
        why: "invalid semantic duty request",
    };
    let [scope, revision, args @ ..] = words.as_slice() else {
        return Err(bad());
    };
    if args.is_empty() {
        return Err(bad());
    }
    let scope = Scope::from_word(scope).ok_or_else(bad)?;
    let revision_number = revision.parse::<u64>().map_err(|_| bad())?;
    if revision_number.to_string() != *revision {
        return Err(bad());
    }
    Ok((scope, revision_number, args.to_vec()))
}

pub(crate) fn is_control_door(door: &str) -> bool {
    matches!(door, super::doors::CTL | super::doors::TELLER | DUTY_ACT)
}

pub(crate) fn argv(door: &str, request: &[u8]) -> Result<Vec<String>, CodecError> {
    if door == DUTY_ACT {
        duty_parts(request).map(|(_, _, args)| args)
    } else {
        codec::ctl_args(request)
    }
}

/// Human/report argv appends the exact semantic authority after the complete actual argv,
/// preserving the operation prefixes used by report and corpus act censuses.
pub(crate) fn reported_args(door: &str, request: &[u8]) -> Result<Vec<String>, CodecError> {
    if door != DUTY_ACT {
        return codec::ctl_args(request);
    }
    let (duty, revision, mut args) = duty_parts(request)?;
    if !args.iter().any(|word| !word.starts_with('@')) {
        return Err(CodecError {
            at: 0,
            why: "duty request lacks a verb",
        });
    }
    args.extend([
        format!("harness-duty={}", duty.word()),
        format!("if-supervisor-revision={revision}"),
    ]);
    Ok(args)
}

pub(crate) fn control_reply(
    door: &str,
    bytes: &[u8],
) -> Result<Option<Result<CtlReply, String>>, CodecError> {
    if door == DUTY_ACT {
        DutyReply::from_tape(bytes).map(|reply| reply.0)
    } else {
        codec::ctl_reply_of(bytes).map(Some)
    }
}

pub(crate) fn encoded_reply(door: &str, reply: Option<Result<CtlReply, String>>) -> Vec<u8> {
    if door == DUTY_ACT {
        DutyReply(reply).to_tape()
    } else {
        codec::ctl_reply(&reply.expect("ordinary control reply"))
    }
}

/// A transport whose round trips cross a door.
pub(crate) struct TapedCtl<'c, C: Ctl + ?Sized> {
    inner: &'c mut C,
    fx: Fx,
    door: &'static str,
}

impl<'c, C: Ctl + ?Sized> TapedCtl<'c, C> {
    /// Wraps `inner`'s round trips through `fx` at `door`.
    pub(crate) const fn new(inner: &'c mut C, fx: Fx, door: &'static str) -> Self {
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

    fn builtin_authority(&mut self, sid: &str) -> BuiltinAuthority {
        self.fx.exchange(
            DUTY_AUTHORITY,
            || super::codec::ctl_request(&[sid]),
            || self.inner.builtin_authority(sid),
        )
    }

    fn agent_act(&mut self, sid: &str) -> Act {
        self.fx.exchange(
            CTL_AGENT_ACT,
            || super::codec::ctl_request(&[sid]),
            || self.inner.agent_act(sid),
        )
    }

    fn call_for_duty(
        &mut self,
        duty: Scope,
        revision: u64,
        args: &[&str],
    ) -> Option<Result<CtlReply, String>> {
        let reply: DutyReply = self.fx.exchange(
            DUTY_ACT,
            || duty_request(duty, revision, args),
            || DutyReply(self.inner.call_for_duty(duty, revision, args)),
        );
        if is_act(args) {
            self.fx.flush(Hint::Act);
        }
        reply.0
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

    fn builtin_authority(&mut self, sid: &str) -> BuiltinAuthority {
        self.fx.exchange(
            DUTY_AUTHORITY,
            || super::codec::ctl_request(&[sid]),
            || unreachable!("a replay never queries the live host's duty authority"),
        )
    }

    fn agent_act(&mut self, sid: &str) -> Act {
        self.fx.exchange(
            CTL_AGENT_ACT,
            || super::codec::ctl_request(&[sid]),
            || unreachable!("a replay never asks the live host what it may do"),
        )
    }

    fn call_for_duty(
        &mut self,
        duty: Scope,
        revision: u64,
        args: &[&str],
    ) -> Option<Result<CtlReply, String>> {
        let reply: DutyReply = self.fx.exchange(
            DUTY_ACT,
            || duty_request(duty, revision, args),
            || unreachable!("a replay never sends typed input to the live host"),
        );
        reply.0
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
#[path = "ctl_tests.rs"]
mod tests;
