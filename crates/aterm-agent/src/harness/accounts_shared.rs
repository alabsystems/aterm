// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained local custody of shared-account preparations. The root adapter
//! verifies authority; this state enforces local consumption and pin retention.

use super::{
    EMAIL_MAX, LABEL_MAX, MAX_ACCOUNTS, Row, admissible_label, dir_refusal, email_refusal,
};
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const MAX_NAMES: usize = 16;

/// Stable Commons context and shared account, independent of local labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Source {
    pub network: [u8; 32],
    pub account: [u8; 32],
}

/// Already reviewed intent. Constructing this value grants no authority; only
/// the root adapter may pass it after validating the signed preparation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    pub source: Source,
    pub decision: [u8; 32],
    pub generation: u64,
    pub configuration: [u8; 32],
    pub request: [u8; 16],
    pub label: String,
    pub identity: String,
    pub staging: String,
    pub before_generation: Option<u64>,
    /// Claimed local start time; never used to order grants or authorize retries.
    pub begun_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Unknown,
    Held,
    Published { generation: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub intent: Intent,
    pub outcome: Outcome,
    pub finished_at: Option<i64>,
}

/// One in-memory completion handle. No clone or public constructor permits a
/// second completion; the retained state still fences stale handles on reload.
pub struct Token {
    source: Source,
    decision: [u8; 32],
    request: [u8; 16],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pin {
    labels: BTreeSet<String>,
    identities: BTreeSet<String>,
    principal: Option<[u8; 32]>,
    attempt: Attempt,
}

/// Older decisions are rejected by each source's monotonic generation floor.
/// The current attempt supplies diagnostics; losing older detail never makes
/// an older decision usable again. Pins have hard bounds and are never evicted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pins: BTreeMap<Source, Pin>,
}

fn identity(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() > EMAIL_MAX || email_refusal(&value).is_some() {
        return Err("shared preparation needs an explicit provider identity".into());
    }
    Ok(value)
}

impl Intent {
    fn validate(&mut self) -> Result<(), String> {
        if self.source.network == [0; 32]
            || self.source.account == [0; 32]
            || self.decision == [0; 32]
            || self.configuration == [0; 32]
            || self.request == [0; 16]
            || self.generation == 0
            || self.begun_at <= 0
            || !admissible_label(&self.label)
            || self.label.len() > LABEL_MAX
            || dir_refusal(&self.staging).is_some()
        {
            return Err("invalid shared preparation intent".into());
        }
        self.identity = identity(&self.identity)?;
        Ok(())
    }
}

impl State {
    pub fn is_empty(&self) -> bool {
        self.pins.is_empty()
    }

    /// Find required authority from retained label or explicit provider identity.
    pub fn required(&self, row: &Row) -> Result<Option<Source>, String> {
        let email = row.expected_identity_email().map(identity).transpose()?;
        let mut found = None;
        for (source, pin) in &self.pins {
            if pin.labels.contains(&row.label)
                || email
                    .as_ref()
                    .is_some_and(|email| pin.identities.contains(email))
            {
                if found.is_some_and(|old| old != *source) {
                    return Err("local row matches conflicting shared sources".into());
                }
                found = Some(*source);
            }
        }
        Ok(found)
    }

    /// A root-approved or proved full provider identity detects renamed email
    /// addresses. Reserving an identity does not establish successful sign-in.
    /// Display keys truncated for UI use must not be supplied as this identity.
    pub fn principal_source(&self, principal: [u8; 32]) -> Option<Source> {
        self.pins
            .iter()
            .find(|(_, pin)| pin.principal == Some(principal))
            .map(|(source, _)| *source)
    }

    pub fn latest(&self, source: Source) -> Option<&Attempt> {
        self.pins.get(&source).map(|pin| &pin.attempt)
    }

    /// Reject consumed generations and conflicting identity constraints before
    /// allocating staging. Admission still calls `begin` in the transaction.
    pub(in crate::harness) fn check_preparation(
        &self,
        source: Source,
        generation: u64,
        expected_principal: Option<[u8; 32]>,
    ) -> Result<(), String> {
        if self
            .pins
            .get(&source)
            .is_some_and(|pin| generation <= pin.attempt.intent.generation)
        {
            return Err("shared preparation decision is already consumed or older".into());
        }
        if let Some(principal) = expected_principal
            && (principal == [0; 32]
                || self
                    .pins
                    .get(&source)
                    .and_then(|pin| pin.principal)
                    .is_some_and(|old| old != principal)
                || self
                    .principal_source(principal)
                    .is_some_and(|owner| owner != source))
        {
            return Err("shared preparation conflicts with retained provider identity".into());
        }
        Ok(())
    }

    /// An admitted shared attempt can replace a default-home login as well as
    /// an absent login. Its checked checkpoint, not enrollment, admits recovery.
    pub(in crate::harness) fn pending_for(&self, row: &Row, principal: Option<[u8; 32]>) -> bool {
        let Ok(source) = self.required(row) else {
            return false;
        };
        source
            .or_else(|| principal.and_then(|p| self.principal_source(p)))
            .and_then(|source| self.latest(source))
            .is_some_and(|attempt| attempt.outcome == Outcome::Pending)
    }

    /// Recovery may finish only the exact staging checkpoint previously
    /// admitted for a shared source. A later preparation fences older paths.
    pub(in crate::harness) fn checkpoint_allowed(
        &self,
        row: &Row,
        principal: Option<[u8; 32]>,
        staging: &str,
        parent: &super::RenewalParent,
    ) -> bool {
        let source = match self.required(row) {
            Ok(source) => source,
            Err(_) => return false,
        };
        let source = source.or_else(|| principal.and_then(|p| self.principal_source(p)));
        let Some(source) = source else {
            return self.is_empty() || principal.is_some();
        };
        let pin = &self.pins[&source];
        pin.attempt.intent.label == row.label
            && pin.attempt.intent.staging == staging
            && pin.attempt.intent.before_generation == parent.generation
            && matches!(
                pin.attempt.outcome,
                Outcome::Pending | Outcome::Published { .. }
            )
            && principal.is_some_and(|p| pin.principal.is_none_or(|old| old == p))
    }

    /// Complete the same admitted checkpoint after a crash. This runs in the
    /// binding transaction, so recovery retains the provider identity and
    /// terminal outcome just as the original publication would have done.
    pub(in crate::harness) fn recover_checkpoint(
        &mut self,
        row: &Row,
        principal: Option<[u8; 32]>,
        checkpoint: &super::RenewalCheckpoint,
        generation: u64,
    ) -> Result<(), String> {
        if !self.checkpoint_allowed(row, principal, &checkpoint.dir, &checkpoint.parent) {
            return Err("checkpoint differs from the retained shared preparation".into());
        }
        let Some(source) = self
            .required(row)?
            .or_else(|| principal.and_then(|p| self.principal_source(p)))
        else {
            return Ok(());
        };
        let attempt = &self.pins[&source].attempt;
        if attempt.outcome != Outcome::Pending {
            return Err("shared preparation checkpoint is already completed".into());
        }
        let token = Token {
            source,
            decision: attempt.intent.decision,
            request: attempt.intent.request,
        };
        let email = attempt.intent.identity.clone();
        self.finish(
            token,
            Outcome::Published { generation },
            principal.map(|p| (email.as_str(), p)),
            checkpoint.checked_at,
        )
    }

    /// Consume a strictly newer reviewed root decision before provider entry.
    /// The enclosing local account transaction must durably commit before using
    /// the returned token. A new nonce never bypasses the generation floor.
    /// An approved full identity is reserved now, even if sign-in later fails.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedSelection",
            action = "Reserve",
            project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
        )
    )]
    pub fn begin(
        &mut self,
        mut intent: Intent,
        expected_principal: Option<[u8; 32]>,
    ) -> Result<Token, String> {
        intent.validate()?;
        self.check_preparation(intent.source, intent.generation, expected_principal)?;
        if self.pins.iter().any(|(source, pin)| {
            *source != intent.source
                && (pin.labels.contains(&intent.label) || pin.identities.contains(&intent.identity))
        }) {
            return Err("shared preparation would replace another retained source".into());
        }
        if !self.pins.contains_key(&intent.source) && self.pins.len() == MAX_ACCOUNTS {
            return Err("shared account pin capacity reached".into());
        }
        let old = self.pins.get(&intent.source);
        let mut labels = old.map(|p| p.labels.clone()).unwrap_or_default();
        let mut identities = old.map(|p| p.identities.clone()).unwrap_or_default();
        labels.insert(intent.label.clone());
        identities.insert(intent.identity.clone());
        if labels.len() > MAX_NAMES || identities.len() > MAX_NAMES {
            return Err("shared account name history capacity reached".into());
        }
        let token = Token {
            source: intent.source,
            decision: intent.decision,
            request: intent.request,
        };
        let pin = Pin {
            labels,
            identities,
            principal: expected_principal.or_else(|| old.and_then(|p| p.principal)),
            attempt: Attempt {
                intent,
                outcome: Outcome::Pending,
                finished_at: None,
            },
        };
        self.pins.insert(token.source, pin);
        Ok(token)
    }

    /// Finish only the still-current attempt. A published result requires both
    /// the approved email and a full provider identity, and retains that identity
    /// across later email and label edits. Authority is rechecked by the adapter.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedSelection",
            action = "Ready",
            project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
        )
    )]
    pub fn finish(
        &mut self,
        token: Token,
        outcome: Outcome,
        proved: Option<(&str, [u8; 32])>,
        finished_at: i64,
    ) -> Result<(), String> {
        let pin = self
            .pins
            .get(&token.source)
            .ok_or_else(|| "shared preparation custody is absent".to_owned())?;
        if pin.attempt.intent.decision != token.decision
            || pin.attempt.intent.request != token.request
            || pin.attempt.outcome != Outcome::Pending
            || outcome == Outcome::Pending
            || finished_at <= 0
        {
            return Err("shared preparation completion is stale".into());
        }
        let principal = if let Outcome::Published { generation } = outcome {
            let (email, principal) = proved
                .ok_or_else(|| "shared preparation has no proved provider identity".to_owned())?;
            if generation == 0
                || generation <= pin.attempt.intent.before_generation.unwrap_or(0)
                || principal == [0; 32]
                || identity(email)? != pin.attempt.intent.identity
                || pin.principal.is_some_and(|old| old != principal)
                || self
                    .principal_source(principal)
                    .is_some_and(|source| source != token.source)
            {
                return Err("shared preparation proved another account or generation".into());
            }
            Some(principal)
        } else {
            pin.principal
        };
        let pin = self.pins.get_mut(&token.source).unwrap();
        pin.principal = principal;
        pin.attempt.outcome = outcome;
        pin.attempt.finished_at = Some(finished_at);
        Ok(())
    }

    /// Ordinary roster, usage and binding edits cannot erase authority history
    /// or lower consumed decisions. The normal account transaction checks this.
    pub(super) fn extends(&self, before: &Self) -> bool {
        before.pins.iter().all(|(source, old)| {
            self.pins.get(source).is_some_and(|new| {
                old.labels.is_subset(&new.labels)
                    && old.identities.is_subset(&new.identities)
                    && old.principal.is_none_or(|p| new.principal == Some(p))
                    && (new.attempt.intent.generation > old.attempt.intent.generation
                        || (new.attempt.intent == old.attempt.intent
                            && (new.attempt == old.attempt
                                || old.attempt.outcome == Outcome::Pending)))
            })
        })
    }

    pub(super) fn fits_clock(&self, clock: u64) -> bool {
        self.pins.values().all(|pin| {
            pin.attempt
                .intent
                .before_generation
                .is_none_or(|n| n <= clock)
                && match pin.attempt.outcome {
                    Outcome::Published { generation } => generation <= clock,
                    _ => true,
                }
        })
    }

    pub(super) fn new_attempt_labels(&self, before: &Self) -> Vec<(String, bool)> {
        self.pins
            .iter()
            .filter(|(source, pin)| {
                before
                    .pins
                    .get(source)
                    .is_none_or(|old| old.attempt.intent.generation < pin.attempt.intent.generation)
            })
            .flat_map(|(_, pin)| {
                pin.labels
                    .iter()
                    .map(|label| (label.clone(), *label == pin.attempt.intent.label))
            })
            .collect()
    }

    pub(super) fn fence_new_attempts(
        &mut self,
        before: &Self,
        bindings: &BTreeMap<String, super::Binding>,
    ) -> Result<(), String> {
        for (source, pin) in &mut self.pins {
            if before
                .pins
                .get(source)
                .is_none_or(|old| old.attempt.intent.generation < pin.attempt.intent.generation)
            {
                let generation = bindings
                    .get(&pin.attempt.intent.label)
                    .map(|b| b.renewal_generation)
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        "shared preparation has no binding-generation fence".to_owned()
                    })?;
                pin.attempt.intent.before_generation = Some(generation);
            }
        }
        Ok(())
    }

    pub(super) fn published_into(
        &self,
        before: &Self,
        bindings: &BTreeMap<String, super::Binding>,
    ) -> bool {
        self.pins.iter().all(|(source, pin)| {
            if let Outcome::Published { generation } = pin.attempt.outcome {
                before
                    .pins
                    .get(source)
                    .is_some_and(|old| old.attempt == pin.attempt)
                    || bindings
                        .get(&pin.attempt.intent.label)
                        .is_some_and(|binding| binding.renewal_generation == generation)
            } else {
                true
            }
        })
    }

    pub(super) fn to_json(&self) -> Value {
        use aterm_codec::hex::encode_lower as hex;
        Value::Array(self.pins.iter().map(|(source,pin)| {
            let i=&pin.attempt.intent;
            let (outcome,generation)=match pin.attempt.outcome {Outcome::Pending=>("pending",None),Outcome::Unknown=>("unknown",None),Outcome::Held=>("held",None),Outcome::Published {generation}=>("published",Some(generation))};
            json!({"network":hex(&source.network),"account":hex(&source.account),"labels":pin.labels.iter().collect::<Vec<_>>(),"identities":pin.identities.iter().collect::<Vec<_>>(),
                "principal":pin.principal.as_ref().map(|p|hex(p)),"decision":hex(&i.decision),"generation":i.generation,"configuration":hex(&i.configuration),
                "request":hex(&i.request),"label":i.label,"identity":i.identity,"staging":i.staging,"before_generation":i.before_generation,
                "outcome":outcome,"published_generation":generation,"begun_at":i.begun_at,"finished_at":pin.attempt.finished_at})
        }).collect())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedRenewal",
            action = "Restart",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
        )
    )]
    pub(super) fn from_json(value: &Value) -> Result<Self, String> {
        let rows = value
            .as_array()
            .filter(|rows| !rows.is_empty() && rows.len() <= MAX_ACCOUNTS)
            .ok_or_else(|| "invalid shared account pins".to_owned())?;
        let mut state = Self::default();
        for row in rows {
            let allowed = [
                "network",
                "account",
                "labels",
                "identities",
                "principal",
                "decision",
                "generation",
                "configuration",
                "request",
                "label",
                "identity",
                "staging",
                "before_generation",
                "outcome",
                "published_generation",
                "begun_at",
                "finished_at",
            ];
            if row.as_object().is_none_or(|row| {
                row.len() != allowed.len() || row.keys().any(|key| !allowed.contains(&key.as_str()))
            }) {
                return Err("unknown or missing shared pin fields".into());
            }
            let word = |key: &str| {
                row[key]
                    .as_str()
                    .ok_or_else(|| "invalid shared account pin field".to_owned())
            };
            let source = Source {
                network: hex(word("network")?)?,
                account: hex(word("account")?)?,
            };
            let mut intent = Intent {
                source,
                decision: hex(word("decision")?)?,
                generation: row["generation"]
                    .as_u64()
                    .ok_or_else(|| "invalid shared decision generation".to_owned())?,
                configuration: hex(word("configuration")?)?,
                request: hex(word("request")?)?,
                label: word("label")?.into(),
                identity: word("identity")?.into(),
                staging: word("staging")?.into(),
                before_generation: optional_u64(&row["before_generation"])?,
                begun_at: row["begun_at"]
                    .as_i64()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| "invalid shared start time".to_owned())?,
            };
            intent.validate()?;
            let labels = names(&row["labels"], admissible_label)?;
            let identities = names(&row["identities"], |s| {
                identity(s).ok().as_deref() == Some(s)
            })?;
            if !labels.contains(&intent.label) || !identities.contains(&intent.identity) {
                return Err("shared account pin lost current identity".into());
            }
            let principal = if row["principal"].is_null() {
                None
            } else {
                Some(hex(word("principal")?)?)
            };
            if principal == Some([0; 32]) {
                return Err("invalid shared provider identity".into());
            }
            let published = optional_u64(&row["published_generation"])?;
            let outcome = match (word("outcome")?, published) {
                ("pending", None) => Outcome::Pending,
                ("unknown", None) => Outcome::Unknown,
                ("held", None) => Outcome::Held,
                ("published", Some(generation))
                    if generation > intent.before_generation.unwrap_or(0)
                        && principal.is_some() =>
                {
                    Outcome::Published { generation }
                }
                _ => return Err("invalid shared preparation outcome".into()),
            };
            let finished_at = if row["finished_at"].is_null() {
                None
            } else {
                Some(
                    row["finished_at"]
                        .as_i64()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| "invalid shared finish time".to_owned())?,
                )
            };
            if matches!(outcome, Outcome::Pending) != finished_at.is_none() {
                return Err("inconsistent shared attempt time".into());
            }
            if state.pins.iter().any(|(_, pin)| {
                !pin.labels.is_disjoint(&labels)
                    || !pin.identities.is_disjoint(&identities)
                    || principal.is_some_and(|p| pin.principal == Some(p))
            }) {
                return Err("conflicting retained shared identities".into());
            }
            if state
                .pins
                .insert(
                    source,
                    Pin {
                        labels,
                        identities,
                        principal,
                        attempt: Attempt {
                            intent,
                            outcome,
                            finished_at,
                        },
                    },
                )
                .is_some()
            {
                return Err("duplicate shared source pin".into());
            }
        }
        Ok(state)
    }
}

fn hex<const N: usize>(value: &str) -> Result<[u8; N], String> {
    let mut bytes = [0; N];
    if !aterm_codec::hex::decode_into(value.as_bytes(), &mut bytes)
        || aterm_codec::hex::encode_lower(&bytes) != value
    {
        return Err("invalid shared identity encoding".into());
    }
    Ok(bytes)
}
fn optional_u64(value: &Value) -> Result<Option<u64>, String> {
    if value.is_null() {
        Ok(None)
    } else {
        value
            .as_u64()
            .map(Some)
            .ok_or_else(|| "invalid shared generation".into())
    }
}
fn names(value: &Value, valid: impl Fn(&str) -> bool) -> Result<BTreeSet<String>, String> {
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= MAX_NAMES)
        .ok_or_else(|| "invalid shared account name history".to_owned())?;
    let mut names = BTreeSet::new();
    for value in rows {
        let word = value
            .as_str()
            .filter(|word| valid(word))
            .ok_or_else(|| "invalid shared account name".to_owned())?;
        if !names.insert(word.to_owned()) {
            return Err("duplicate shared account name".into());
        }
    }
    Ok(names)
}

#[cfg(test)]
#[path = "accounts_shared_tests.rs"]
mod tests;
