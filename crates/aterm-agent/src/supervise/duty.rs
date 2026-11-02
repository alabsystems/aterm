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
        | PromptKind::AutoModeSetup => Scope::Approvals,
    })
}

#[cfg(test)]
#[path = "duty_visibility_tests.rs"]
mod visibility_tests;

#[cfg(test)]
mod tests {
    use super::*;

    struct Adapter {
        authority: Arc<Mutex<BuiltinAuthority>>,
        typed: Vec<(Scope, u64, Vec<String>)>,
        legacy: Vec<Vec<String>>,
    }

    impl Ctl for Adapter {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            self.legacy
                .push(args.iter().map(|word| word.to_string()).collect());
            Ok(CtlReply {
                code: 0,
                stdout: if args.last() == Some(&"meta") {
                    "OK supervisor=- autopilot=cruise\n"
                } else {
                    "OK\n"
                }
                .to_string(),
                stderr: String::new(),
            })
        }
        fn builtin_authority(&mut self, _: &str) -> BuiltinAuthority {
            *self.authority.lock().unwrap()
        }
        fn call_for_duty(
            &mut self,
            duty: Scope,
            revision: u64,
            args: &[&str],
        ) -> Option<Result<CtlReply, String>> {
            let current = *self.authority.lock().unwrap();
            self.typed.push((
                duty,
                revision,
                args.iter().map(|word| word.to_string()).collect(),
            ));
            Some(Ok(CtlReply {
                code: i32::from(current.revision() != Some(revision) || !current.keeps(duty)),
                stdout: "OK\n".to_string(),
                stderr: String::new(),
            }))
        }
    }

    #[test]
    fn actual_session_retains_complement_and_pins_semantic_revision_without_legacy_fallback() {
        let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(
            41,
            [Scope::Upgrade],
        )));
        let mut adapter = Adapter {
            authority: Arc::clone(&authority),
            typed: Vec::new(),
            legacy: Vec::new(),
        };
        let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
        session.set_builtin_duties(true);
        assert!(session.refresh_builtin_authority());
        assert!(
            !session.call(&["key", "1"]).unwrap().ok(),
            "unlabelled input is refused"
        );
        session.with_duty(Scope::Approvals, |session| {
            assert!(session.call(&["key", "1"]).unwrap().ok());
            *authority.lock().unwrap() = BuiltinAuthority::complement(42, [Scope::Upgrade]);
            assert!(
                !session.call(&["key", "enter"]).unwrap().ok(),
                "later unsent command keeps old revision and is refused"
            );
        });
        *authority.lock().unwrap() = BuiltinAuthority::complement(43, [Scope::Questions]);
        session.with_duty(Scope::Questions, |session| {
            assert!(!session.call(&["send", "answer"]).unwrap().ok());
        });
        drop(session);
        assert_eq!(adapter.typed.len(), 2);
        assert_eq!(adapter.typed[0].0, Scope::Approvals);
        assert_eq!(adapter.typed[0].1, 41);
        assert_eq!(adapter.typed[1].1, 41);
        assert!(
            adapter.legacy.is_empty(),
            "typed refusal never retries via ordinary adapter"
        );
    }

    #[test]
    fn unclaimed_host_keeps_its_visibility_lease_and_revision_bound_input() {
        let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(0, [])));
        let mut adapter = Adapter {
            authority: Arc::clone(&authority),
            typed: Vec::new(),
            legacy: Vec::new(),
        };
        let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
        session.set_builtin_duties(true);
        session.claim_at_naming();
        assert!(matches!(session.claim, Claim::Held { .. }));
        *authority.lock().unwrap() = BuiltinAuthority::complement(1, []);
        session.with_duty(Scope::Approvals, |session| {
            assert!(matches!(session.claim, Claim::Held { .. }));
            assert!(session.call(&["key", "1"]).unwrap().ok());
        });
        *authority.lock().unwrap() = BuiltinAuthority::complement(2, [Scope::Upgrade]);
        session.refresh_builtin_authority();
        session.renew_claim_if_due();
        assert!(matches!(session.claim, Claim::Complement));
        *authority.lock().unwrap() = BuiltinAuthority::complement(3, []);
        session.refresh_builtin_authority();
        session.renew_claim_if_due();
        assert!(matches!(session.claim, Claim::Held { .. }));
        session.give_back_claim();
        session.refresh_builtin_authority();
        session.renew_claim_if_due();
        assert!(matches!(session.claim, Claim::GivenBack { .. }));
        drop(session);
        assert_eq!(adapter.typed.len(), 1);
        assert_eq!(adapter.typed[0].0, Scope::Approvals);
        assert_eq!(adapter.typed[0].1, 1);
        assert_eq!(
            adapter
                .legacy
                .iter()
                .map(|args| args[2].as_str())
                .collect::<Vec<_>>(),
            ["set", "set", "unset"],
            "only naming, the foreign owner's departure, and Off change the lease"
        );
    }

    #[test]
    fn ordinary_adapter_and_v2_exclusivity_remain_distinct() {
        let authority = Arc::new(Mutex::new(BuiltinAuthority::Exclusive));
        let mut adapter = Adapter {
            authority,
            typed: Vec::new(),
            legacy: Vec::new(),
        };
        let mut ordinary = Session::new(&mut adapter, None);
        ordinary.with_duty(Scope::Questions, |session| {
            assert!(session.call(&["key", "1"]).unwrap().ok())
        });
        drop(ordinary);
        assert_eq!(adapter.legacy.len(), 1);
        let mut hosted = Session::new(&mut adapter, Some("@s-duty".to_string()));
        hosted.set_builtin_duties(true);
        hosted.claim = Claim::Behind {
            holder: "codex-alab@4242".to_string(),
            tried: hosted.fx.mono(),
        };
        hosted.with_duty(Scope::Approvals, |session| {
            assert!(!session.call(&["key", "1"]).unwrap().ok())
        });
        drop(hosted);
        assert!(adapter.typed.is_empty());
        assert_eq!(adapter.legacy.len(), 1);
    }

    #[test]
    fn usage_resume_keeps_its_explicit_turn_end_origin_for_all_three_ownership_cases() {
        let action = super::super::TurnEndAction::Type {
            text: "continue".to_string(),
            rule_id: super::super::RULE_LIMIT_RESUME,
        };
        let scope = action_scope(&action, true, false);
        assert_eq!(scope, Scope::TurnEnd);
        for (taken, allowed) in [
            (vec![], true),
            (vec![Scope::Walls], true),
            (vec![Scope::TurnEnd], false),
        ] {
            let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, taken)));
            let mut adapter = Adapter {
                authority,
                typed: Vec::new(),
                legacy: Vec::new(),
            };
            let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
            session.set_builtin_duties(true);
            session.with_duty(scope, |session| {
                assert_eq!(session.call(&["turn", "continue"]).unwrap().ok(), allowed)
            });
            drop(session);
            assert!(
                adapter
                    .legacy
                    .iter()
                    .all(|args| args.get(1).map(String::as_str) == Some("meta")),
                "only the visibility lease uses the ordinary adapter; input stays typed"
            );
            assert_eq!(adapter.typed.len(), usize::from(allowed));
        }
    }

    #[test]
    fn complementary_off_park_never_unsets_foreign_claim_and_ordinary_remedy_is_preserved() {
        let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, [])));
        let mut adapter = Adapter {
            authority: Arc::clone(&authority),
            typed: Vec::new(),
            legacy: Vec::new(),
        };
        let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
        session.set_builtin_duties(true);
        assert!(
            !session.remedies_withheld(),
            "ordinary no-foreign session retains outside remedy policy"
        );
        *authority.lock().unwrap() = BuiltinAuthority::complement(42, [Scope::Upgrade]);
        assert!(
            session.remedies_withheld(),
            "foreign coordination conservatively stands down"
        );
        session.give_back_claim();
        assert!(matches!(session.claim, Claim::GivenBack { .. }));
        session.refresh_builtin_authority();
        assert!(
            matches!(session.claim, Claim::GivenBack { .. }),
            "a read cannot unpark the loop"
        );
        session.retake_claim(&mut super::super::Alone);
        assert!(matches!(session.claim, Claim::Complement));
        *authority.lock().unwrap() = BuiltinAuthority::Unknown;
        assert!(
            session.remedies_withheld(),
            "unknown private authority never grants a native act"
        );
        drop(session);
        assert!(
            adapter.legacy.is_empty(),
            "parking the complement never sends meta unset supervisor"
        );
    }

    #[test]
    fn direct_needs_cleanup_and_timer_retry_retain_explicit_turn_end_metadata_duty() {
        let authority = Arc::new(Mutex::new(BuiltinAuthority::complement(41, [])));
        let mut adapter = Adapter {
            authority,
            typed: Vec::new(),
            legacy: Vec::new(),
        };
        let mut session = Session::new(&mut adapter, Some("@s-duty".to_string()));
        session.set_builtin_duties(true);
        session.needs_told = Some(0);
        session.needs_pending = true;
        session
            .needs_down(1, "controlled cleanup", &mut super::super::Alone)
            .unwrap();
        session
            .retry_needs(
                &super::super::Screen::default(),
                &super::super::SuperviseOpts::default(),
                &mut super::super::Alone,
            )
            .unwrap();
        drop(session);
        assert!(
            !adapter.typed.is_empty(),
            "direct cleanup reached the typed metadata precheck"
        );
        assert!(
            adapter
                .typed
                .iter()
                .all(|(scope, revision, _)| *scope == Scope::TurnEnd && *revision == 41)
        );
        assert!(
            adapter
                .legacy
                .iter()
                .all(|words| words.last().map(String::as_str) == Some("meta")),
            "only ownership observations use the ordinary read lane"
        );
    }

    #[test]
    fn status_authority_rejects_unknown_duplicate_and_noncanonical_fields() {
        let good = "OK supervisor=grok-alab@42 supervisor_contract=3 supervisor_scope=upgrade supervisor_revision=41";
        assert_eq!(
            BuiltinAuthority::from_status(good, "host@7"),
            BuiltinAuthority::complement(41, [Scope::Upgrade])
        );
        for bad in [
            good.replace("supervisor_revision=41", ""),
            format!("{good} supervisor_revision=42"),
            good.replace("revision=41", "revision=041"),
            good.replace("scope=upgrade", "scope=upgrade,upgrade"),
            good.replace("contract=3", "contract=4"),
            format!("{good}\nOK another=reply"),
        ] {
            assert_eq!(
                BuiltinAuthority::from_status(&bad, "host@7"),
                BuiltinAuthority::Unknown,
                "{bad}"
            );
        }
        assert_eq!(
            BuiltinAuthority::from_status(
                "OK supervisor=- supervisor_contract=- supervisor_scope=- supervisor_revision=42",
                "host@7"
            ),
            BuiltinAuthority::complement(42, [])
        );
    }

    #[test]
    fn complement_and_recorded_authority_preserve_all_duties_and_revision() {
        for authority in [
            BuiltinAuthority::Unsupported,
            BuiltinAuthority::Unknown,
            BuiltinAuthority::Legacy,
            BuiltinAuthority::Exclusive,
            BuiltinAuthority::complement(41, [Scope::Upgrade]),
            BuiltinAuthority::complement(42, []),
        ] {
            assert_eq!(
                BuiltinAuthority::from_tape(&authority.to_tape()).unwrap(),
                authority
            );
        }
        let authority = BuiltinAuthority::complement(41, [Scope::Upgrade]);
        assert!(!authority.keeps(Scope::Upgrade));
        for duty in [
            Scope::Approvals,
            Scope::Questions,
            Scope::TurnEnd,
            Scope::Walls,
        ] {
            assert!(authority.keeps(duty));
        }
        assert!(BuiltinAuthority::from_tape(&[4, 0]).is_err());
        assert!(BuiltinAuthority::from_tape(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 64]).is_err());
        assert!(BuiltinAuthority::from_tape(&[2, 0]).is_err());
    }

    #[test]
    fn semantic_duty_restores_after_nested_error_and_unwind() {
        let duty = DutyContext::default();
        let outer = duty.enter(Scope::Approvals, Some(41));
        let result = std::panic::catch_unwind(|| {
            let _inner = duty.enter(Scope::Questions, Some(42));
            assert_eq!(duty.get(), Some(Scope::Questions));
            panic!("controlled semantic-operation failure");
        });
        assert!(result.is_err());
        assert_eq!(duty.get(), Some(Scope::Approvals));
        assert_eq!(duty.revision(), Some(41));
        drop(outer);
        assert_eq!(duty.get(), None);
    }
}
