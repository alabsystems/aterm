// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit semantic duties and the hosted engine's observed complement of a scoped claim.

use super::{Ctl, CtlReply, Session, claim::Claim};
use crate::harness::contract::Scope;
use crate::supervise::tape::{codec::CodecError, core::TapeValue};
use std::sync::{Arc, Mutex};

/// An observation of the private built-in adapter's authority. This grants no native process
/// act: the adapter must fence an input again at dispatch with the exact current revision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BuiltinAuthority {
    /// An ordinary adapter, retaining its existing claim protocol.
    #[default]
    Unsupported,
    /// No usable observation; a hosted adapter must write nothing.
    Unknown,
    /// This session has never coordinated a scoped claim and retains the legacy protocol.
    Legacy,
    /// A foreign legacy or v2 holder owns the whole session.
    Exclusive,
    /// The built-in keeps the complement of these externally owned duties at this revision.
    Complement { revision: u64, taken: u8 },
}

fn bit(scope: Scope) -> u8 {
    match scope {
        Scope::Relaunch => 1,
        Scope::Upgrade => 2,
        Scope::TurnEnd => 4,
        Scope::Approvals => 8,
        Scope::Questions => 16,
        Scope::Walls => 32,
    }
}

impl BuiltinAuthority {
    /// Parse one fresh targeted status reply. Missing, duplicate or inconsistent ownership
    /// fields never become legacy permission. The server publishes these fields together.
    #[must_use]
    pub fn from_status(reply: &str, builtin_holder: &str) -> Self {
        let mut lines = reply.lines();
        let Some(line) = lines.next().filter(|line| line.len() <= 1 << 20) else {
            return Self::Unknown;
        };
        if lines.next().is_some() {
            return Self::Unknown;
        }
        let mut words = line.split_whitespace();
        if words.next() != Some("OK") {
            return Self::Unknown;
        }
        let (mut holder, mut contract, mut scopes, mut revision) = (None, None, None, None);
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            let slot = match key {
                "supervisor" => &mut holder,
                "supervisor_contract" => &mut contract,
                "supervisor_scope" => &mut scopes,
                "supervisor_revision" => &mut revision,
                _ => continue,
            };
            if value.is_empty() || slot.replace(value).is_some() {
                return Self::Unknown;
            }
        }
        let (Some(holder), Some(contract), Some(scopes), Some(revision)) =
            (holder, contract, scopes, revision)
        else {
            return Self::Unknown;
        };
        let Ok(revision_number) = revision.parse::<u64>() else {
            return Self::Unknown;
        };
        if revision_number.to_string() != revision {
            return Self::Unknown;
        }
        if holder == "-" {
            return if contract == "-" && scopes == "-" {
                Self::complement(revision_number, [])
            } else {
                Self::Unknown
            };
        }
        if !matches!(contract, "-" | "2" | "3") || scopes == "-" {
            return Self::Unknown;
        }
        let mut taken = Vec::new();
        for word in scopes.split(',') {
            let Some(scope) = Scope::from_word(word) else {
                return Self::Unknown;
            };
            if taken.contains(&scope) {
                return Self::Unknown;
            }
            taken.push(scope);
        }
        if contract != "3" && taken.len() != Scope::ALL.len() {
            return Self::Unknown;
        }
        if holder == aterm_codec::pct::encode(builtin_holder) {
            Self::complement(revision_number, [])
        } else if contract == "3" {
            Self::complement(revision_number, taken)
        } else {
            Self::Exclusive
        }
    }

    /// Capture the externally owned duties. An empty set represents a coordinated session
    /// whose scoped claim has ended; it still uses typed, revision-bound input.
    pub fn complement(revision: u64, taken: impl IntoIterator<Item = Scope>) -> Self {
        Self::Complement {
            revision,
            taken: taken.into_iter().fold(0, |mask, scope| mask | bit(scope)),
        }
    }

    /// Whether this observation leaves a duty to the built-in. An unknown observation grants
    /// nothing; an ordinary adapter continues through its legacy claim protocol separately.
    #[must_use]
    pub fn keeps(self, scope: Scope) -> bool {
        matches!(self, Self::Complement { taken, .. } if taken & bit(scope) == 0)
    }

    /// The input revision, only for a complete scoped-complement observation.
    #[must_use]
    pub const fn revision(self) -> Option<u64> {
        match self {
            Self::Complement { revision, .. } => Some(revision),
            _ => None,
        }
    }

    /// A foreign claim requires native host acts to stand down until they have paired act
    /// admission. A fresh snapshot alone cannot authorize an upgrade or relaunch.
    #[must_use]
    pub const fn foreign(self) -> bool {
        matches!(self, Self::Exclusive | Self::Complement { taken: 1.., .. })
    }
}

impl TapeValue for BuiltinAuthority {
    fn to_tape(&self) -> Vec<u8> {
        match *self {
            Self::Unsupported => vec![0],
            Self::Unknown => vec![1],
            Self::Legacy => vec![2],
            Self::Exclusive => vec![3],
            Self::Complement { revision, taken } => {
                let mut bytes = vec![4];
                bytes.extend_from_slice(&revision.to_le_bytes());
                bytes.push(taken);
                bytes
            }
        }
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match bytes {
            [0] => Ok(Self::Unsupported),
            [1] => Ok(Self::Unknown),
            [2] => Ok(Self::Legacy),
            [3] => Ok(Self::Exclusive),
            [4, a, b, c, d, e, f, g, h, taken] if taken & !63 == 0 => Ok(Self::Complement {
                revision: u64::from_le_bytes([*a, *b, *c, *d, *e, *f, *g, *h]),
                taken: *taken,
            }),
            _ => Err(CodecError {
                at: 0,
                why: "invalid built-in duty authority",
            }),
        }
    }
}

pub(crate) struct DutyReply(pub Option<Result<CtlReply, String>>);

impl TapeValue for DutyReply {
    fn to_tape(&self) -> Vec<u8> {
        match &self.0 {
            None => vec![0],
            Some(reply) => {
                let mut bytes = vec![1];
                bytes.extend(crate::supervise::tape::codec::ctl_reply(reply));
                bytes
            }
        }
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match bytes {
            [0] => Ok(Self(None)),
            [1, rest @ ..] => {
                crate::supervise::tape::codec::ctl_reply_of(rest).map(|r| Self(Some(r)))
            }
            _ => Err(CodecError {
                at: 0,
                why: "invalid typed-duty reply",
            }),
        }
    }
}

/// A semantic batch's duty, restored on nested operations, errors and unwinding. No lock is
/// held while an operation calls back into the session or its transport.
#[derive(Clone, Copy)]
struct DutyFrame {
    scope: Scope,
    revision: Option<u64>,
}

#[derive(Default)]
pub(super) struct DutyContext(Arc<Mutex<Option<DutyFrame>>>);

impl DutyContext {
    pub(super) fn get(&self) -> Option<Scope> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map(|frame| frame.scope)
    }

    pub(super) fn revision(&self) -> Option<u64> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .and_then(|frame| frame.revision)
    }

    pub(super) fn enter(&self, scope: Scope, revision: Option<u64>) -> DutyRestore {
        let previous = self
            .0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(DutyFrame { scope, revision });
        DutyRestore {
            context: Arc::clone(&self.0),
            previous,
        }
    }
}

// Restores a semantic value; it owns no mutex guard and keeps no lock held.
pub(super) struct DutyRestore {
    context: Arc<Mutex<Option<DutyFrame>>>,
    previous: Option<DutyFrame>,
}

impl Drop for DutyRestore {
    fn drop(&mut self) {
        *self.context.lock().unwrap_or_else(|p| p.into_inner()) = self.previous;
    }
}

impl<C: Ctl> Session<'_, C> {
    pub(super) fn duty_guard(&mut self, scope: Scope) -> DutyRestore {
        self.enter_duty(scope, true)
    }

    /// A metadata retry or cleanup verifies existing authority without taking or renewing a
    /// visibility lease. Nested helpers still retain their caller's semantic revision.
    pub(super) fn metadata_duty_guard(&mut self, scope: Scope) -> DutyRestore {
        self.enter_duty(scope, false)
    }

    fn enter_duty(&mut self, scope: Scope, prepare_visibility: bool) -> DutyRestore {
        if self.duty.get() == Some(scope) {
            return self.duty.enter(scope, self.duty.revision());
        }
        let outermost = self.duty.get().is_none();
        self.refresh_builtin_authority();
        if prepare_visibility
            && outermost
            && self.setup.builtin_duties
            && self.metadata_context == super::claim::MetadataContext::Normal
            && self.renew_claim_if_due()
        {
            // Every successful server claim set, including a TTL renewal, advances its
            // authority revision. Capture the resulting revision before pinning this act.
            self.refresh_builtin_authority();
        }
        self.duty.enter(scope, self.builtin_authority.revision())
    }
    /// Every callback in this semantic operation carries the same explicit duty, including
    /// nested helpers; a nested different operation restores its caller's duty on every exit.
    #[cfg(test)]
    pub(super) fn with_duty<T>(
        &mut self,
        scope: Scope,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let _guard = self.duty_guard(scope);
        operation(self)
    }

    /// Refresh only a private hosted adapter. `true` means it resolved the claim protocol
    /// itself. An empty complement still takes the host's ordinary lease for supervision
    /// visibility; input keeps its semantic duty and revision regardless of that lease.
    pub(super) fn refresh_builtin_authority(&mut self) -> bool {
        if !self.setup.builtin_duties || self.setup.beside.is_some() {
            return false;
        }
        let Some(sid) = self.setup.sid.as_deref() else {
            self.builtin_authority = BuiltinAuthority::Unknown;
            self.claim = Claim::Pending {
                tried: self.fx.mono(),
            };
            return true;
        };
        self.builtin_authority = self.ctl.builtin_authority(sid);
        if matches!(self.claim, Claim::GivenBack { .. }) {
            return true;
        }
        match self.builtin_authority {
            BuiltinAuthority::Complement { taken, .. } => {
                if taken != 0 || !matches!(self.claim, Claim::Held { .. }) {
                    self.claim = Claim::Complement;
                }
                self.claim_managed = true;
                taken != 0
            }
            BuiltinAuthority::Unknown => {
                self.claim = Claim::Pending {
                    tried: self.fx.mono(),
                };
                true
            }
            BuiltinAuthority::Exclusive => {
                if matches!(self.claim, Claim::Complement | Claim::Pending { .. }) {
                    self.claim = Claim::Behind {
                        holder: "foreign supervisor".to_string(),
                        tried: self.fx.mono(),
                    };
                }
                false
            }
            BuiltinAuthority::Legacy | BuiltinAuthority::Unsupported => false,
        }
    }
}

pub(super) fn known_action_scope(action: &super::TurnEndAction) -> Option<Scope> {
    use super::TurnEndAction;
    match action {
        TurnEndAction::Restart { .. } => Some(Scope::Relaunch),
        // The contract explicitly assigns continuing work at usage resets to turn-end.
        action if action.rule_id() == Some(super::RULE_LIMIT_RESUME) => Some(Scope::TurnEnd),
        TurnEndAction::Confirm { .. } | TurnEndAction::Cancel { .. } => Some(Scope::Walls),
        action
            if action
                .rule_id()
                .is_some_and(crate::supervise::policy::turn_end::is_wall_retry) =>
        {
            Some(Scope::Walls)
        }
        _ => None,
    }
}

pub(super) fn action_scope(
    action: &super::TurnEndAction,
    visible_wall: bool,
    prose_question: bool,
) -> Scope {
    known_action_scope(action).unwrap_or(if visible_wall {
        Scope::Walls
    } else if prose_question {
        Scope::Questions
    } else {
        Scope::TurnEnd
    })
}

impl<C: Ctl> Session<'_, C> {
    /// This is a conservative precheck, not a native act ticket. Ordinary hosted sessions
    /// retain the existing remedy behavior; foreign scoped or unknown authority stands down.
    pub(super) fn remedies_withheld(&mut self) -> bool {
        if self.setup.builtin_duties {
            self.refresh_builtin_authority();
            match self.builtin_authority {
                BuiltinAuthority::Complement { .. } => return self.builtin_authority.foreign(),
                BuiltinAuthority::Unknown | BuiltinAuthority::Exclusive => return true,
                _ => {}
            }
        }
        self.claim.remedies_withheld()
    }
}

/// The duty that may ANSWER a dialog of this kind. A dialog of no kind aterm reads has none, so no
/// key goes to it; telling a person of it still needs one (`Session::escalate_point`).
pub(super) fn prompt_scope(kind: aterm_phase::PromptKind) -> Option<Scope> {
    use aterm_phase::PromptKind;
    Some(match kind {
        PromptKind::Question => Scope::Questions,
        PromptKind::UsageLimit | PromptKind::ModelSwitch | PromptKind::AutocompactOffer => {
            Scope::Walls
        }
        PromptKind::Other | PromptKind::HeldMessage => return None,
        PromptKind::Bash
        | PromptKind::Edit
        | PromptKind::Write
        | PromptKind::Overwrite
        | PromptKind::Read
        | PromptKind::Workflow
        | PromptKind::Trust
        | PromptKind::PowerShell
        | PromptKind::Fetch
        | PromptKind::NetworkRequest
        | PromptKind::Browser
        | PromptKind::Skill
        | PromptKind::Monitor
        | PromptKind::Tool
        | PromptKind::PlanEnter
        | PromptKind::PlanExit
        | PromptKind::GoalProposal
        | PromptKind::ComputerUse
        | PromptKind::ReadOutsideSetting
        | PromptKind::McpServer
        | PromptKind::AutoModeSetup
        | PromptKind::ConversationResume
        | PromptKind::ChromeIntro => Scope::Approvals,
    })
}

#[cfg(test)]
#[path = "duty_visibility_tests.rs"]
mod visibility_tests;

#[cfg(test)]
#[path = "duty_tests.rs"]
mod tests;
