// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE GATE ON THE HARNESS'S INPUT: a hosted loop types into its session only while the
//! host confirms the session's foreground agent by its image ([`Act::On`],
//! [`crate::harness::agent_image`]). An agent named but not confirmed is observed: its boxes are
//! read, logged and escalated to the person ([`OBSERVE_REASON`]), and every input verb the loop
//! would send, and every verb that ends or signals its process ([`drives_agent`]: a restart ends
//! the process and the host types its relaunch line and continuation), is refused here
//! ([`refusal`], the control server's own words for it, [`agent_image::OBSERVE_REFUSAL`]) before
//! the transport sees it. The server's input admission is the gate every harness write passes;
//! the loop asks the same verdict first, so that a box it would have answered is escalated
//! instead, and its remedies that would restart the agent (the stall's, the memory ceiling's
//! beside a self-claim) withhold the restart before they send it, as the turn end's Restart is
//! withheld at its decision ([`RESTART_WITHHELD`]).

use super::*;
use crate::harness::agent_image::{self, Act};
use crate::supervise::tape::{codec::CodecError, core::TapeValue};

/// What [`Session::call`] answers an input verb while the agent is observed: the server's own
/// refusal ([`agent_image::OBSERVE_REFUSAL`]), an `ERR busy` reply, which every write path reads
/// as "nothing was written, back off" (never `busy sink`, which is retried, and never a
/// `supervisor=` word, which names another holder).
pub(super) fn refusal() -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: format!("aterm-ctl: {}\n", agent_image::OBSERVE_REFUSAL),
    }
}

#[cfg(test)]
thread_local! {
    /// How many input verbs the gate refused on this thread: a test's count of the round trips
    /// an observed loop made to it.
    pub(super) static REFUSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The escalation's reason for a box the loop would have answered for an unconfirmed agent.
pub(super) const OBSERVE_REASON: &str =
    "the agent's image is unconfirmed (supervisor_act=observe): the harness types nothing into it";

/// Why a remedy that would restart an unconfirmed agent withheld it: its journal line's reason,
/// said once a stall or a memory episode.
pub(super) const RESTART_WITHHELD: &str =
    "the agent's image is unconfirmed (supervisor_act=observe): the harness restarts nothing";

/// Whether `args` ends or signals the session's foreground process: `restart` (the host ends
/// the agent and types its relaunch line and continuation past the loop's connection) or
/// `signal`. Observed, the loop sends neither ([`Session::observe_refuses`]).
pub(super) fn drives_agent(args: &[&str]) -> bool {
    matches!(
        args.iter().copied().find(|a| !a.starts_with('@')),
        Some("restart" | "signal")
    )
}

/// Whether `r` is the gate's refusal ([`refusal`]) or the server's own
/// ([`agent_image::observe_refused`]).
pub(super) fn refused(r: &CtlReply) -> bool {
    r.code != 0 && agent_image::observe_refused(r.err_text())
}

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

    /// The refusal of `args` when it is input or drives the agent's process ([`drives_agent`])
    /// and the loop observes ([`refusal`]).
    pub(super) fn observe_refuses(&mut self, args: &[&str]) -> Option<CtlReply> {
        let refuses = self.setup.confirms_agent
            && (claim::is_input(args) || drives_agent(args))
            && self.observing();
        #[cfg(test)]
        if refuses {
            REFUSED.set(REFUSED.get() + 1);
        }
        refuses.then(refusal)
    }
}

#[cfg(test)]
#[path = "observe_tests.rs"]
mod tests;
