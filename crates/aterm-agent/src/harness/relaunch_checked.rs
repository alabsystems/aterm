// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private source provenance for a checked live relaunch, not a wire authority flag.

use super::{Restart, RestartAsk, Snapshot};
use crate::harness::native_act::{AskedIntent, NativePlan, Prepare};

/// The next effect the real producer just checked. Recorded effects need no fresh grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckedRelaunchStep {
    /// Publish a new immutable preparation, or reauthorize its exact unsent saved wire.
    Prepare,
    /// First Signal only, after another genuine source safety check.
    Signal,
}

/// A private-field value minted only by the actual checked restart path.
pub struct CheckedRelaunch {
    request: Prepare,
    step: CheckedRelaunchStep,
    tab: String,
    conversation: String,
}

impl CheckedRelaunch {
    /// The complete immutable request the source actually checked.
    #[must_use]
    pub fn request(&self) -> &Prepare {
        &self.request
    }

    /// The exact next effect these checks precede.
    #[must_use]
    pub fn step(&self) -> CheckedRelaunchStep {
        self.step
    }

    /// Actual source tab, independent of the RPC routing word.
    #[must_use]
    pub fn tab(&self) -> &str {
        &self.tab
    }

    /// Actual session record the safety check read.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }
}

/// The host's private session hook; no RPC caller can construct its input value.
pub trait RelaunchAuthorizer: Send + Sync {
    /// Install one scoped exact-request authorization. Drop revokes only still-unused proof.
    ///
    /// # Errors
    /// Refuses a changed/closed session, another pending authorization or expired authority.
    fn authorize(&self, checked: &CheckedRelaunch) -> Result<Box<dyn Send>, &'static str>;
}

impl<F> RelaunchAuthorizer for F
where
    F: Fn(&CheckedRelaunch) -> Result<Box<dyn Send>, &'static str> + Send + Sync,
{
    fn authorize(&self, checked: &CheckedRelaunch) -> Result<Box<dyn Send>, &'static str> {
        self(checked)
    }
}

/// Constructed only after the real restart's fresh safety predicates passed.
pub(crate) struct CheckedSource {
    pid: u32,
    start: String,
    shell: u32,
    ask: RestartAsk,
    human_seq: u64,
    step: CheckedRelaunchStep,
    tab: String,
    conversation: String,
}

impl CheckedSource {
    pub(super) fn new(
        why: &Restart,
        ask: &RestartAsk,
        snap: &Snapshot,
        human_seq: u64,
        step: CheckedRelaunchStep,
        conversation: &str,
    ) -> Option<Self> {
        if !crate::harness::upgrade::is_session_id(conversation)
            || snap.session.as_deref() != Some(conversation)
        {
            return None;
        }
        let run_at: i64 = ask.word().strip_prefix("memory@")?.parse().ok()?;
        if ask != &RestartAsk::of(&Restart::Memory, run_at) {
            return None;
        }
        match why {
            Restart::Memory => {}
            // The existing custody tuple is sampled after send and has only lstart
            // seconds. It cannot mint full receiver identity at the guarded write.
            Restart::InputLost { .. } => return None,
            _ => return None,
        }
        Some(Self {
            pid: snap.pid,
            start: snap.start.clone(),
            shell: snap.shell,
            ask: ask.clone(),
            human_seq,
            step,
            tab: snap.tab.clone(),
            conversation: conversation.to_owned(),
        })
    }

    pub(crate) fn agrees(&self, request: &Prepare) -> bool {
        let NativePlan::Restart {
            process,
            shell,
            observation: None,
            cold: None,
            ..
        } = &request.plan
        else {
            return false;
        };
        process.birth != 0
            && process.pid == self.pid
            && shell.pid == self.shell
            && super::squash(&crate::harness::native_restart::vendor_start(process))
                == super::squash(&self.start)
    }

    pub(crate) fn ask(&self) -> &str {
        self.ask.word()
    }

    pub(crate) fn checked(&self, request: &Prepare) -> Option<CheckedRelaunch> {
        let Some(AskedIntent::Relaunch(intent)) = &request.asked else {
            return None;
        };
        if request.expected_human_seq != Some(self.human_seq)
            || intent.ask != self.ask.word()
            || !self.agrees(request)
        {
            return None;
        }
        Some(CheckedRelaunch {
            request: request.clone(),
            step: self.step,
            tab: self.tab.clone(),
            conversation: self.conversation.clone(),
        })
    }
}
