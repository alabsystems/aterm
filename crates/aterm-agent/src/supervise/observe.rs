// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE GATE ON THE HARNESS'S INPUT: a hosted loop types into its session only while the
//! host confirms the session's foreground agent by its image ([`Act::On`],
//! [`crate::harness::agent_image`]). An agent named but not confirmed is observed: its boxes are
//! read, logged and escalated to the person ([`OBSERVE_REASON`]), and every input verb the loop
//! would send is refused here ([`OBSERVE_REFUSAL`]) before the transport sees it.

use super::*;
use crate::harness::agent_image::Act;
use crate::supervise::tape::{codec::CodecError, core::TapeValue};

/// What [`Session::call`] answers an input verb while the agent is observed: an `ERR busy` reply,
/// which every write path reads as "nothing was written, back off" (never `busy sink`, which is
/// retried, and never a `supervisor=` word, which names another holder).
pub(super) const OBSERVE_REFUSAL: &str = "aterm-ctl: ERR busy supervisor_act=observe (the agent's image is unconfirmed: this loop observes)\n";

#[cfg(test)]
thread_local! {
    /// How many input verbs the gate refused on this thread: a test's count of the round trips
    /// an observed loop made to it.
    pub(super) static REFUSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The escalation's reason for a box the loop would have answered for an unconfirmed agent.
pub(super) const OBSERVE_REASON: &str =
    "the agent's image is unconfirmed (supervisor_act=observe): the harness types nothing into it";

impl TapeValue for Act {
    fn to_tape(&self) -> Vec<u8> {
        match self {
            Self::On => vec![0],
            Self::Observe => vec![1],
        }
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match bytes {
            [0] => Ok(Self::On),
            [1] => Ok(Self::Observe),
            _ => Err(CodecError {
                at: 0,
                why: "invalid supervisor act",
            }),
        }
    }
}

impl<C: Ctl> Session<'_, C> {
    /// The host's act for this loop's session, read fresh ([`Ctl::agent_act`]): [`Act::On`] for a
    /// loop whose host confirms no agent ([`Session::set_confirms_agent`] unset), and
    /// [`Act::Observe`] for one that names no session.
    pub(super) fn agent_act(&mut self) -> Act {
        if !self.setup.confirms_agent {
            return Act::On;
        }
        match self.setup.sid.as_deref() {
            Some(sid) => self.ctl.agent_act(sid),
            None => Act::Observe,
        }
    }

    /// Whether the loop observes only: its host confirms no agent in the session.
    pub(super) fn observing(&mut self) -> bool {
        self.agent_act() == Act::Observe
    }

    /// The refusal of `args` when it is input and the loop observes ([`OBSERVE_REFUSAL`]).
    pub(super) fn observe_refuses(&mut self, args: &[&str]) -> Option<CtlReply> {
        let refuses = self.setup.confirms_agent && claim::is_input(args) && self.observing();
        #[cfg(test)]
        if refuses {
            REFUSED.set(REFUSED.get() + 1);
        }
        refuses.then(|| CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: OBSERVE_REFUSAL.to_string(),
        })
    }
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
