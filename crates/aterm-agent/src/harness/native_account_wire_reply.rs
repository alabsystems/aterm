// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// Recorded child classification. None of these values is an execution grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildState {
    Prepared,
    MayExecute,
    NoExec(ImagePhase),
    Sealed { may_execute: bool },
}

impl ChildState {
    fn word(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::MayExecute => "may-execute",
            Self::NoExec(ImagePhase::Forward) => "noexec-forward",
            Self::NoExec(ImagePhase::Rollback) => "noexec-rollback",
            Self::Sealed { may_execute: false } => "sealed-unused",
            Self::Sealed { may_execute: true } => "sealed-used",
        }
    }
    fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "prepared" => Self::Prepared,
            "may-execute" => Self::MayExecute,
            "noexec-forward" => Self::NoExec(ImagePhase::Forward),
            "noexec-rollback" => Self::NoExec(ImagePhase::Rollback),
            "sealed-unused" => Self::Sealed { may_execute: false },
            "sealed-used" => Self::Sealed { may_execute: true },
            _ => return None,
        })
    }
    fn valid_for(self, phase: ImagePhase) -> bool {
        phase != ImagePhase::Rollback || self != Self::NoExec(ImagePhase::Forward)
    }

    fn valid_for_relation(self, relation: &Relation) -> bool {
        self.valid_for(relation.phase)
            && match self {
                Self::NoExec(phase) => super::super::native_account_recipe::active_command(
                    &relation.candidate.plan,
                    phase,
                )
                .is_some(),
                _ => true,
            }
    }
}

/// A diagnostic outcome deliberately lacking a child Binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    Claimed(ChildState),
    Fenced,
}

/// A refusal never authorizes clearing a pending candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denial {
    Authority,
    Epoch,
    Phase,
    Conflict,
    State,
    Deadline,
    Account,
}
/// A transient refusal never authorizes choosing a different candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Busy,
    Capacity,
    Custody,
    Entropy,
    Constructor,
}

macro_rules! words {
    ($ty:ident { $($variant:ident => $word:literal),+ $(,)? }) => {
        impl $ty {
            fn word(self) -> &'static str { match self { $(Self::$variant => $word),+ } }
            fn parse(word: &str) -> Option<Self> {
                Some(match word { $($word => Self::$variant),+, _ => return None })
            }
        }
    };
}
words!(Denial { Authority => "authority", Epoch => "epoch", Phase => "phase", Conflict => "conflict", State => "state", Deadline => "deadline", Account => "account" });
words!(Unavailable { Busy => "busy", Capacity => "capacity", Custody => "custody", Entropy => "entropy", Constructor => "constructor" });

/// Data identifying the original predecessor. Construction proves no closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosureReceipt {
    epoch: String,
    parent: String,
    digest: ClosureDigest,
    sequence: u64,
    recorded_ms: u64,
}

impl ClosureReceipt {
    #[must_use]
    pub fn for_predecessor(parent: &Predecessor, sequence: u64, recorded_ms: u64) -> Self {
        Self {
            epoch: parent.prepare.host_epoch.clone(),
            parent: parent.prepare.id.clone(),
            digest: parent.digest,
            sequence,
            recorded_ms,
        }
    }
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    #[must_use]
    pub const fn recorded_ms(&self) -> u64 {
        self.recorded_ms
    }
    fn matches(&self, parent: &Predecessor) -> bool {
        self.epoch == parent.prepare.host_epoch
            && self.parent == parent.prepare.id
            && self.digest == parent.digest
    }
}

/// Data identifying an immutable relation. Construction proves no journal fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationReceipt {
    epoch: String,
    parent: String,
    candidate: String,
    phase: ImagePhase,
    digest: RelationDigest,
    sequence: u64,
    recorded_ms: u64,
}

impl RelationReceipt {
    #[must_use]
    pub fn for_relation(relation: &Relation, sequence: u64, recorded_ms: u64) -> Self {
        Self {
            epoch: relation.predecessor.prepare.host_epoch.clone(),
            parent: relation.predecessor.prepare.id.clone(),
            candidate: relation.candidate.id.clone(),
            phase: relation.phase,
            digest: relation.digest,
            sequence,
            recorded_ms,
        }
    }
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    #[must_use]
    pub const fn recorded_ms(&self) -> u64 {
        self.recorded_ms
    }
    fn matches(&self, relation: &Relation) -> bool {
        self.epoch == relation.predecessor.prepare.host_epoch
            && self.parent == relation.predecessor.prepare.id
            && self.candidate == relation.candidate.id
            && self.phase == relation.phase
            && self.digest == relation.digest
    }
    fn write(&self, out: &mut String) -> Option<()> {
        write!(
            out,
            " epoch={} parent={} child={} phase={} digest={} seq={} ms={}",
            self.epoch,
            self.parent,
            self.candidate,
            phase_word(self.phase),
            aterm_codec::hex::encode_lower(&self.digest.0),
            self.sequence,
            self.recorded_ms
        )
        .ok()
    }
}

/// Wire data only. Even a correlated positive reply requires authenticated
/// transport and a checked durable client transition before requesting Resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Closure {
        receipt: ClosureReceipt,
        phase: ImagePhase,
        /// Advisory retained successor identity. None is never an absence
        /// proof and cannot authorize clearing a pending candidate.
        successor: Option<String>,
    },
    Claimed {
        receipt: RelationReceipt,
        binding: Binding,
        state: ChildState,
    },
    Fenced(RelationReceipt),
    Observed {
        receipt: RelationReceipt,
        outcome: Observation,
    },
    Unknown,
    Denied(Denial),
    Unsupported,
    Unavailable(Unavailable),
}

fn phase_word(phase: ImagePhase) -> &'static str {
    match phase {
        ImagePhase::Forward => "forward",
        ImagePhase::Rollback => "rollback",
    }
}

impl Reply {
    /// Canonical bounded ASCII reply, or None for an inconsistent child binding,
    /// backward closure phase or malformed successor identity.
    #[must_use]
    pub fn encode(&self) -> Option<String> {
        let mut out = match self {
            Self::Denied(_) | Self::Unsupported | Self::Unavailable(_) => {
                "ERR native-account v=1 state="
            }
            _ => "OK native-account v=1 state=",
        }
        .to_owned();
        match self {
            Self::Closure {
                receipt,
                phase,
                successor,
            } => {
                if successor
                    .as_ref()
                    .is_some_and(|s| !id(s) || *s == receipt.parent)
                {
                    return None;
                }
                write!(
                    out,
                    "closure epoch={} parent={} digest={} seq={} ms={} phase={} successor={}",
                    receipt.epoch,
                    receipt.parent,
                    aterm_codec::hex::encode_lower(&receipt.digest.0),
                    receipt.sequence,
                    receipt.recorded_ms,
                    phase_word(*phase),
                    successor.as_deref().unwrap_or("-")
                )
                .ok()?;
            }
            Self::Claimed {
                receipt,
                binding,
                state,
            } => {
                if !valid_binding(binding)
                    || binding.id != receipt.candidate
                    || !state.valid_for(receipt.phase)
                {
                    return None;
                }
                out.push_str("claimed");
                receipt.write(&mut out)?;
                write!(
                    out,
                    " cap={} child-state={}",
                    binding.capability,
                    state.word()
                )
                .ok()?;
            }
            Self::Fenced(receipt) => {
                out.push_str("fenced");
                receipt.write(&mut out)?;
            }
            Self::Observed { receipt, outcome } => {
                let word = match outcome {
                    Observation::Fenced => "fenced",
                    Observation::Claimed(state) if state.valid_for(receipt.phase) => state.word(),
                    _ => return None,
                };
                out.push_str("observed");
                receipt.write(&mut out)?;
                write!(out, " outcome={word}").ok()?;
            }
            Self::Unknown => out.push_str("unknown"),
            Self::Denied(reason) => write!(out, "denied reason={}", reason.word()).ok()?,
            Self::Unsupported => out.push_str("unsupported"),
            Self::Unavailable(reason) => {
                write!(out, "unavailable reason={}", reason.word()).ok()?
            }
        }
        out.push('\n');
        (out.len() <= MAX_REPLY).then_some(out)
    }

    /// Decode one untrusted wire line, including its terminating newline.
    /// This validates syntax and internal consistency,
    /// never journal authenticity, current authority or client durability.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "NextRequest",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "NextReply",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongParent",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongEpoch",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongCandidate",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongDigest",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongPhase",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongBinding",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongLifecycle",
            reason = "The fixture supplies an untrusted request role or reply field"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "MissingRollback",
            reason = "The fixture supplies a child closure phase absent from its exact recipe"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongClosurePhase",
            reason = "The fixture supplies a closure phase unsupported by its predecessor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WrongSuccessor",
            reason = "The fixture supplies an invalid or self-referential successor identity"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountWireReply",
            action = "WithSuccessor",
            reason = "The fixture supplies a valid recorded child identity instead of absence"
        )
    )]
    pub fn decode(text: &str) -> Option<Self> {
        if text.len() > MAX_REPLY || !text.is_ascii() {
            return None;
        }
        let line = text.strip_suffix('\n')?;
        if line.contains(['\n', '\r', '\0', '\t']) {
            return None;
        }
        let mut words = line.split(' ');
        let status = words.next()?;
        if !matches!(status, "OK" | "ERR") || words.next()? != "native-account" {
            return None;
        }
        let mut fields = BTreeMap::new();
        for word in words {
            let (key, value) = word.split_once('=')?;
            if key.is_empty() || value.is_empty() || fields.insert(key, value).is_some() {
                return None;
            }
        }
        if take(&mut fields, "v")? != "1" {
            return None;
        }
        let state = take(&mut fields, "state")?;
        let reply = match state {
            "closure" => {
                let receipt = ClosureReceipt {
                    epoch: identifier(&mut fields, "epoch")?,
                    parent: identifier(&mut fields, "parent")?,
                    digest: ClosureDigest(digest(&mut fields)?),
                    sequence: number(&mut fields, "seq")?,
                    recorded_ms: number(&mut fields, "ms")?,
                };
                let phase = phase(&mut fields)?;
                let successor = match take(&mut fields, "successor")? {
                    "-" => None,
                    s if id(s) => Some(s.to_owned()),
                    _ => return None,
                };
                Self::Closure {
                    receipt,
                    phase,
                    successor,
                }
            }
            "claimed" => {
                let receipt = relation(&mut fields)?;
                let binding = Binding {
                    id: receipt.candidate.clone(),
                    capability: take(&mut fields, "cap")?.to_owned(),
                };
                Self::Claimed {
                    receipt,
                    binding,
                    state: ChildState::parse(take(&mut fields, "child-state")?)?,
                }
            }
            "fenced" => Self::Fenced(relation(&mut fields)?),
            "observed" => {
                let receipt = relation(&mut fields)?;
                let outcome = match take(&mut fields, "outcome")? {
                    "fenced" => Observation::Fenced,
                    word => Observation::Claimed(ChildState::parse(word)?),
                };
                Self::Observed { receipt, outcome }
            }
            "unknown" => Self::Unknown,
            "denied" => Self::Denied(Denial::parse(take(&mut fields, "reason")?)?),
            "unsupported" => Self::Unsupported,
            "unavailable" => Self::Unavailable(Unavailable::parse(take(&mut fields, "reason")?)?),
            _ => return None,
        };
        (fields.is_empty() && reply.encode()?.as_str() == text).then_some(reply)
    }

    /// Decode the contents returned by `RelayClient::request_frame`, whose
    /// framing reader has already consumed the line terminator. This performs
    /// the same validation as `decode` and accepts no embedded terminator.
    #[must_use]
    pub fn decode_line(line: &str) -> Option<Self> {
        if line.len() >= MAX_REPLY || line.contains(['\r', '\n', '\0']) {
            return None;
        }
        let mut wire = String::with_capacity(line.len() + 1);
        wire.push_str(line);
        wire.push('\n');
        Self::decode(&wire)
    }

    /// Correlate a positive reply to the saved request. False for observations,
    /// refusals and Unknown. True alone is not transport or execution authority.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountWireReply",
            action = "Check",
            project = "aterm_agent::harness::native_account_wire::tests::model::transition"
        )
    )]
    pub fn acknowledges(&self, request: &Request) -> bool {
        match (self, request) {
            (
                Self::Closure {
                    receipt,
                    phase,
                    successor,
                },
                Request::Closure(parent),
            ) => {
                receipt.matches(parent)
                    && parent.supports_phase(*phase)
                    && successor
                        .as_ref()
                        .is_none_or(|s| id(s) && *s != receipt.parent)
            }
            (
                Self::Claimed {
                    receipt,
                    binding,
                    state,
                },
                Request::Claim(relation),
            ) => {
                receipt.matches(relation)
                    && valid_binding(binding)
                    && binding.id == relation.candidate.id
                    && state.valid_for_relation(relation)
            }
            (Self::Fenced(receipt), Request::Fence(relation)) => receipt.matches(relation),
            _ => false,
        }
    }

    /// Correlate diagnostic data to an observation request, without turning it
    /// into a claim/fence acknowledgement or an execution grant.
    #[must_use]
    pub fn observation_for(&self, request: &Request) -> Option<Observation> {
        let (Self::Observed { receipt, outcome }, Request::Observe(relation)) = (self, request)
        else {
            return None;
        };
        if !receipt.matches(relation) {
            return None;
        }
        match outcome {
            Observation::Claimed(state) if !state.valid_for_relation(relation) => None,
            outcome => Some(*outcome),
        }
    }
}

fn take<'a>(fields: &mut BTreeMap<&str, &'a str>, key: &str) -> Option<&'a str> {
    fields.remove(key)
}
fn identifier(fields: &mut BTreeMap<&str, &str>, key: &str) -> Option<String> {
    let word = take(fields, key)?;
    id(word).then(|| word.to_owned())
}
fn number(fields: &mut BTreeMap<&str, &str>, key: &str) -> Option<u64> {
    let word = take(fields, key)?;
    let value: u64 = word.parse().ok()?;
    (value.to_string() == word).then_some(value)
}
fn digest(fields: &mut BTreeMap<&str, &str>) -> Option<[u8; 32]> {
    let word = take(fields, "digest")?;
    if !aterm_codec::hex::is_lower(word, 64) {
        return None;
    }
    aterm_codec::hex::decode_vec(word.as_bytes())?
        .try_into()
        .ok()
}
fn phase(fields: &mut BTreeMap<&str, &str>) -> Option<ImagePhase> {
    match take(fields, "phase")? {
        "forward" => Some(ImagePhase::Forward),
        "rollback" => Some(ImagePhase::Rollback),
        _ => None,
    }
}
fn relation(fields: &mut BTreeMap<&str, &str>) -> Option<RelationReceipt> {
    let receipt = RelationReceipt {
        epoch: identifier(fields, "epoch")?,
        parent: identifier(fields, "parent")?,
        candidate: identifier(fields, "child")?,
        phase: phase(fields)?,
        digest: RelationDigest(digest(fields)?),
        sequence: number(fields, "seq")?,
        recorded_ms: number(fields, "ms")?,
    };
    (receipt.parent != receipt.candidate).then_some(receipt)
}
