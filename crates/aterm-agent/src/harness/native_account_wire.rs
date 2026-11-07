// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Data-only account issuer messages. No control verb or execution grant is
//! registered here. Transport authentication and live host checks remain owed.

use super::native_act::{Binding, ImagePhase, NativePlan, Prepare, Request as NativeRequest};
use aterm_digest::Sha256;

#[path = "native_account_wire_reply.rs"]
mod reply;
pub use reply::{
    ChildState, ClosureReceipt, Denial, Observation, RelationReceipt, Reply, Unavailable,
};

/// Independent wire revision; the existing native-act contract is unchanged.
pub const VERSION: u8 = 1;
/// Counted-body cap, below the existing 256 KiB control-body limit.
pub const MAX_BODY: usize = 2 * super::native_act::MAX_REQUEST + 1024;
/// A reply is one bounded ASCII line including its final newline.
pub const MAX_REPLY: usize = 1024;
const MAGIC: [u8; 4] = [b'N', b'A', b'W', VERSION];
// NAW + binary revision, kind, phase, reserved, lengths, ASCII id and cap.
const HEADER: usize = 16 + 32 + 64;

/// Commitment to an exact canonical predecessor and its Binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosureDigest([u8; 32]);
/// Commitment to an exact canonical parent, Binding, phase and candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelationDigest([u8; 32]);

fn commitment(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize()
}

fn phase_tag(phase: ImagePhase) -> u8 {
    match phase {
        ImagePhase::Forward => 1,
        ImagePhase::Rollback => 2,
    }
}

fn read_phase(tag: u8) -> Option<ImagePhase> {
    match tag {
        1 => Some(ImagePhase::Forward),
        2 => Some(ImagePhase::Rollback),
        _ => None,
    }
}

fn id(value: &str) -> bool {
    aterm_codec::hex::is_lower(value, 32)
}

fn valid_binding(binding: &Binding) -> bool {
    id(&binding.id) && aterm_codec::hex::is_lower(&binding.capability, 64)
}

fn canonical(prepare: &Prepare) -> Option<String> {
    NativeRequest::prepare(prepare.clone()).encode()
}

fn decode_prepare(bytes: &[u8]) -> Option<Prepare> {
    let text = std::str::from_utf8(bytes).ok()?;
    let NativeRequest::Prepare(prepare) = NativeRequest::parse(text)? else {
        return None;
    };
    (canonical(&prepare)?.as_bytes() == bytes).then_some(*prepare)
}

/// Immutable original request, never reconstructed from mutable client mirrors.
#[derive(Clone, PartialEq, Eq)]
pub struct Predecessor {
    prepare: Box<Prepare>,
    binding: Binding,
    wire: String,
    digest: ClosureDigest,
}

impl std::fmt::Debug for Predecessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountPredecessor(redacted)")
    }
}

impl Predecessor {
    /// Validate and retain the exact original restart recipe and Binding.
    /// This structural check does not prove that the operation closed.
    #[must_use]
    pub fn new(prepare: Prepare, binding: Binding) -> Option<Self> {
        if !valid_binding(&binding)
            || prepare.id != binding.id
            || !matches!(
                prepare.plan,
                NativePlan::Restart {
                    cold: None,
                    observation: None,
                    ..
                }
            )
            || !(stored_at(&prepare, ImagePhase::Forward)
                || stored_at(&prepare, ImagePhase::Rollback))
        {
            return None;
        }
        let wire = canonical(&prepare)?;
        let digest = ClosureDigest(commitment(
            b"aterm/native-account/closure/1\0",
            &[
                wire.as_bytes(),
                binding.id.as_bytes(),
                binding.capability.as_bytes(),
            ],
        ));
        Some(Self {
            prepare: Box::new(prepare),
            binding,
            wire,
            digest,
        })
    }

    #[must_use]
    pub fn prepare(&self) -> &Prepare {
        &self.prepare
    }
    #[must_use]
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    #[must_use]
    pub const fn digest(&self) -> ClosureDigest {
        self.digest
    }

    /// Structural account-phase eligibility, not current permission or proof
    /// that this phase actually closed on the host.
    #[must_use]
    pub fn supports_phase(&self, phase: ImagePhase) -> bool {
        stored_at(&self.prepare, phase)
    }
}

fn stored_at(prepare: &Prepare, phase: ImagePhase) -> bool {
    super::native_account_recipe::active_command(&prepare.plan, phase).is_some_and(|command| {
        matches!(
            command.account_intent.as_deref(),
            Some(super::accounts_pool::ExecutionIntent::Stored { .. })
        )
    })
}

/// Complete immutable relation. Changing any field requires new validation and
/// a new commitment; no mutable field can invalidate a cached digest.
#[derive(Clone, PartialEq, Eq)]
pub struct Relation {
    predecessor: Predecessor,
    phase: ImagePhase,
    candidate: Box<Prepare>,
    wire: String,
    digest: RelationDigest,
}

impl std::fmt::Debug for Relation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountRelation(redacted)")
    }
}

impl Relation {
    /// Check the account-only recipe relation without reading credentials or
    /// deciding current permission, closure, claim or execution authority.
    #[must_use]
    pub fn new(predecessor: Predecessor, phase: ImagePhase, candidate: Prepare) -> Option<Self> {
        if !predecessor
            .prepare
            .account_successor_recipe(&candidate, phase)
        {
            return None;
        }
        let wire = canonical(&candidate)?;
        let digest = RelationDigest(commitment(
            b"aterm/native-account/relation/1\0",
            &[
                predecessor.wire.as_bytes(),
                predecessor.binding.id.as_bytes(),
                predecessor.binding.capability.as_bytes(),
                &[phase_tag(phase)],
                wire.as_bytes(),
            ],
        ));
        Some(Self {
            predecessor,
            phase,
            candidate: Box::new(candidate),
            wire,
            digest,
        })
    }

    #[must_use]
    pub fn predecessor(&self) -> &Predecessor {
        &self.predecessor
    }
    #[must_use]
    pub const fn phase(&self) -> ImagePhase {
        self.phase
    }
    #[must_use]
    pub fn candidate(&self) -> &Prepare {
        &self.candidate
    }
    #[must_use]
    pub const fn digest(&self) -> RelationDigest {
        self.digest
    }
}

/// These messages have no live dispatcher until host and client transactions
/// have both been implemented and qualified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Closure(Predecessor),
    Claim(Relation),
    Fence(Relation),
    Observe(Relation),
}

impl Request {
    /// Encode a counted body. It must not be sent as a control request line.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let (kind, parent, phase, candidate) = match self {
            Self::Closure(parent) => (1, parent, 0, ""),
            Self::Claim(relation) => (
                2,
                &relation.predecessor,
                phase_tag(relation.phase),
                relation.wire.as_str(),
            ),
            Self::Fence(relation) => (
                3,
                &relation.predecessor,
                phase_tag(relation.phase),
                relation.wire.as_str(),
            ),
            Self::Observe(relation) => (
                4,
                &relation.predecessor,
                phase_tag(relation.phase),
                relation.wire.as_str(),
            ),
        };
        let mut bytes = Vec::with_capacity(HEADER + parent.wire.len() + candidate.len());
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&[kind, phase, 0, 0]);
        bytes.extend_from_slice(&(parent.wire.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(candidate.len() as u32).to_le_bytes());
        bytes.extend_from_slice(parent.binding.id.as_bytes());
        bytes.extend_from_slice(parent.binding.capability.as_bytes());
        bytes.extend_from_slice(parent.wire.as_bytes());
        bytes.extend_from_slice(candidate.as_bytes());
        bytes
    }

    /// Decode only a complete bounded body. Lengths, tags and canonical nested
    /// requests are validated before constructing an immutable relation.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < HEADER
            || bytes.len() > MAX_BODY
            || bytes[..4] != MAGIC
            || bytes[6..8] != [0, 0]
            || !(1..=4).contains(&bytes[4])
        {
            return None;
        }
        let parent_len = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        let candidate_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        if parent_len == 0
            || parent_len > super::native_act::MAX_REQUEST
            || candidate_len > super::native_act::MAX_REQUEST
            || HEADER.checked_add(parent_len)?.checked_add(candidate_len)? != bytes.len()
            || (bytes[4] == 1 && (bytes[5] != 0 || candidate_len != 0))
            || (bytes[4] != 1 && (read_phase(bytes[5]).is_none() || candidate_len == 0))
        {
            return None;
        }
        let binding = Binding {
            id: std::str::from_utf8(&bytes[16..48]).ok()?.to_owned(),
            capability: std::str::from_utf8(&bytes[48..HEADER]).ok()?.to_owned(),
        };
        let parent = Predecessor::new(
            decode_prepare(&bytes[HEADER..HEADER + parent_len])?,
            binding,
        )?;
        if bytes[4] == 1 {
            return Some(Self::Closure(parent));
        }
        let relation = Relation::new(
            parent,
            read_phase(bytes[5])?,
            decode_prepare(&bytes[HEADER + parent_len..])?,
        )?;
        Some(match bytes[4] {
            2 => Self::Claim(relation),
            3 => Self::Fence(relation),
            4 => Self::Observe(relation),
            _ => return None,
        })
    }
}

#[cfg(test)]
#[path = "native_account_wire_tests.rs"]
mod tests;
