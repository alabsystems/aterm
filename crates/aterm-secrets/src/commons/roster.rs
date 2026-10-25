// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed membership snapshots with recipient-specific network-key capsules.
//! A current journal and the full retained policy must permit every member.

use super::{
    binding::{BINDING_LEN, Binding, Possession},
    constraints::{Constraints, Fact, key_commitment},
    journal::{Current, Journal, State, Trust},
    membership::{Certificate, Eligibility},
    node::{Edit, Node},
};
use crate::{
    Error, Result, SecretBytes,
    age::{self, Identity},
};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{Ed25519KeyPair, KeyPair},
};
use std::collections::BTreeSet;
use zeroize::Zeroizing;

/// One fact per member plus the epoch fact fits the control node's 64-fact cap.
pub const MAX_MEMBERS: usize = 63;
const MAX_CAPSULE: usize = 2048;
const HEADER: usize = 4 + 16 + 32 + 8 + 32 + 2;
const CAPSULE_PLAIN: usize = 4 + 32 + BINDING_LEN + 1 + 8 + 32;
const MAX_ROSTER: usize = HEADER + MAX_MEMBERS * (BINDING_LEN + 1 + 2 + MAX_CAPSULE);

/// Data privileges only; the root authority and terminal-drive grants are separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Reader,
    Writer,
}
impl Role {
    fn byte(self) -> u8 {
        match self {
            Self::Reader => 0,
            Self::Writer => 1,
        }
    }
    fn decode(byte: u8) -> Result<Self> {
        match byte {
            0 => Ok(Self::Reader),
            1 => Ok(Self::Writer),
            _ => Err(Error::new("unknown Commons roster role")),
        }
    }
}

/// Public membership metadata. Construction alone confers no privileges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    binding: Binding,
    role: Role,
}
impl Member {
    pub fn binding(&self) -> &Binding {
        &self.binding
    }
    pub const fn role(&self) -> Role {
        self.role
    }
}

/// Consumed joint-possession evidence for initial network creation.
pub struct Enrollment {
    host: String,
    possession: Possession,
    role: Role,
}
impl Enrollment {
    pub fn new(host: impl Into<String>, possession: Possession, role: Role) -> Self {
        Self {
            host: host.into(),
            possession,
            role,
        }
    }
}

/// An explicit authority decision for the next roster. Existing recorded devices
/// need no online challenge; newly approved devices carry their certificate.
pub struct Grant {
    host: String,
    member: Member,
    certificate: Option<Certificate>,
}
impl Grant {
    pub fn recorded(host: impl Into<String>, binding: Binding, role: Role) -> Self {
        Self {
            host: host.into(),
            member: Member { binding, role },
            certificate: None,
        }
    }
    pub fn certified(host: impl Into<String>, certificate: Certificate, role: Role) -> Self {
        Self {
            host: host.into(),
            member: Member {
                binding: certificate.binding().clone(),
                role,
            },
            certificate: Some(certificate),
        }
    }
}

struct Capsule {
    member: Member,
    wire: Vec<u8>,
}
struct Roster {
    network: [u8; 16],
    authority: [u8; 32],
    generation: u64,
    commitment: [u8; 32],
    capsules: Vec<Capsule>,
}

fn validate_members(network: [u8; 16], authority: [u8; 32], members: &[Member]) -> Result<()> {
    if members.is_empty() || members.len() > MAX_MEMBERS {
        return Err(Error::new("Commons roster member count is out of bounds"));
    }
    let (mut actors, mut writers, mut machines, mut recipients) = (
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
        BTreeSet::new(),
    );
    for member in members {
        let b = &member.binding;
        if *b.network() != network
            || *b.authority() != authority
            || !actors.insert(b.actor())
            || !writers.insert(*b.writer())
            || !machines.insert(*b.machine())
            || !recipients.insert(b.recipient())
        {
            return Err(Error::new(
                "Commons roster has conflicting or foreign device identities",
            ));
        }
    }
    Ok(())
}

fn context(
    network: [u8; 16],
    authority: [u8; 32],
    generation: u64,
    commitment: [u8; 32],
    members: &[Member],
) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/commons/roster-context/1\0");
    hash.update(network);
    hash.update(authority);
    hash.update(generation.to_le_bytes());
    hash.update(commitment);
    hash.update((members.len() as u16).to_le_bytes());
    for member in members {
        hash.update(member.binding.bytes());
        hash.update([member.role.byte()]);
    }
    hash.finalize()
}

fn facts(roster: &Roster) -> BTreeSet<Fact> {
    roster
        .capsules
        .iter()
        .map(|c| Fact::Bind {
            binding: c.member.binding.clone(),
        })
        .chain(std::iter::once(Fact::Epoch {
            generation: roster.generation,
            commitment: roster.commitment,
        }))
        .collect()
}

fn encode(roster: &Roster) -> SecretBytes {
    let length = HEADER
        + roster
            .capsules
            .iter()
            .map(|c| BINDING_LEN + 3 + c.wire.len())
            .sum::<usize>();
    let mut wire = Vec::with_capacity(length);
    wire.extend_from_slice(b"ACR1");
    wire.extend_from_slice(&roster.network);
    wire.extend_from_slice(&roster.authority);
    wire.extend_from_slice(&roster.generation.to_le_bytes());
    wire.extend_from_slice(&roster.commitment);
    wire.extend_from_slice(&(roster.capsules.len() as u16).to_le_bytes());
    for capsule in &roster.capsules {
        wire.extend_from_slice(&capsule.member.binding.bytes());
        wire.push(capsule.member.role.byte());
        wire.extend_from_slice(&(capsule.wire.len() as u16).to_le_bytes());
        wire.extend_from_slice(&capsule.wire);
    }
    SecretBytes::new(wire)
}

struct Reader<'a> {
    wire: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.wire.len())
            .ok_or_else(|| Error::new("truncated Commons roster"))?;
        let bytes = &self.wire[self.at..end];
        self.at = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
}
fn decode(wire: &[u8]) -> Result<Roster> {
    if wire.len() < HEADER || wire.len() > MAX_ROSTER {
        return Err(Error::new("Commons roster exceeds its wire bounds"));
    }
    let mut r = Reader { wire, at: 0 };
    if r.take(4)? != b"ACR1" {
        return Err(Error::new("unknown Commons roster version"));
    }
    let network = r.array()?;
    let authority = r.array()?;
    let generation = u64::from_le_bytes(r.array()?);
    let commitment = r.array()?;
    let count = usize::from(u16::from_le_bytes(r.array()?));
    if generation == 0 || commitment == [0; 32] || count == 0 || count > MAX_MEMBERS {
        return Err(Error::new("invalid Commons roster header"));
    }
    let mut capsules = Vec::with_capacity(count);
    for _ in 0..count {
        let binding = Binding::decode(r.take(BINDING_LEN)?)?;
        let role = Role::decode(r.take(1)?[0])?;
        let length = usize::from(u16::from_le_bytes(r.array()?));
        if length == 0 || length > MAX_CAPSULE {
            return Err(Error::new("Commons key capsule exceeds its bounds"));
        }
        let bytes = r.take(length)?;
        if !bytes.starts_with(b"age-encryption.org/v1\n") {
            return Err(Error::new("Commons key capsule requires binary age"));
        }
        capsules.push(Capsule {
            member: Member { binding, role },
            wire: bytes.to_vec(),
        });
    }
    if r.at != wire.len()
        || capsules
            .windows(2)
            .any(|pair| pair[0].member.binding >= pair[1].member.binding)
    {
        return Err(Error::new("noncanonical Commons roster"));
    }
    let members: Vec<_> = capsules.iter().map(|c| c.member.clone()).collect();
    validate_members(network, authority, &members)?;
    Ok(Roster {
        network,
        authority,
        generation,
        commitment,
        capsules,
    })
}

/// A validated local observation. Recheck against the journal for every key use.
/// This cannot retract keys a previously authorized caller already copied.
#[derive(Debug)]
pub struct Access {
    trust: Trust,
    head: [u8; 32],
    basis: [u8; 32],
    generation: u64,
    members: Vec<Member>,
    local: usize,
    key: SecretBytes,
}
impl Access {
    pub const fn head(&self) -> &[u8; 32] {
        &self.head
    }
    /// Exact data node supplying the roster; a recovery head has no payload.
    pub const fn basis(&self) -> &[u8; 32] {
        &self.basis
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    pub fn members(&self) -> &[Member] {
        &self.members
    }
    pub fn local(&self) -> &Member {
        &self.members[self.local]
    }
    /// Whether the journal still represents this exact validated observation.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRosterAccess",
            action = "StaleHandle",
            project = "aterm_secrets::commons::roster::tests::access_transition"
        )
    )]
    pub fn is_current(&self, journal: &Journal) -> bool {
        let current = journal.trust() == self.trust
            && journal.state()
                == (State::Ready {
                    head: self.head,
                    basis: self.basis,
                });
        aterm_db_core::roster_access(current, true, true)
    }
    /// Refuse a stale observation, a fork, missing history or failed durability
    /// before exposing the key to a trusted local caller's operation.
    pub fn with_key<T>(
        &self,
        journal: &Journal,
        use_key: impl FnOnce(&[u8; 32]) -> T,
    ) -> Result<T> {
        if !self.is_current(journal) {
            return Err(Error::new(
                "Commons roster observation is no longer current",
            ));
        }
        Ok(use_key(self.key.as_slice().try_into().unwrap()))
    }

    /// Open an older selected roster using the same recipient. This validates
    /// past authorship and releases an old read key, never a current write grant.
    /// New members need an explicit historical-key handoff; it is not inferred.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Current",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Rotated",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Stale",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Missing",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "ForeignRecipient",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Forked",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "RetiredLocal",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Selected",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoricalAccess",
            action = "Discarded",
            project = "aterm_secrets::commons::roster::tests::historical::historical_transition"
        )
    )]
    pub fn historical(
        &self,
        journal: &Journal,
        identity: &Identity,
        basis: [u8; 32],
    ) -> Result<Historical> {
        if !self.is_current(journal) || self.local().binding.recipient() != identity.recipient() {
            return Err(Error::new(
                "historical access requires the current local membership",
            ));
        }
        let snapshot = journal.historical(basis, identity)?;
        let past = validate(journal, identity, snapshot, false)?;
        Ok(Historical {
            trust: self.trust,
            head: self.head,
            basis: self.basis,
            source: past.basis,
            generation: past.generation,
            members: past.members,
            key: past.key,
        })
    }

    pub(crate) fn historical_with_key(
        &self,
        journal: &Journal,
        identity: &Identity,
        basis: [u8; 32],
        file_key: &[u8; 16],
        key: &[u8; 32],
    ) -> Result<Historical> {
        if !self.is_current(journal) || self.local().binding.recipient() != identity.recipient() {
            return Err(Error::new(
                "history handoff requires current local membership",
            ));
        }
        let snapshot = journal.historical_with_key(basis, file_key)?;
        let roster = decode(snapshot.payload.as_slice())?;
        validate_evidence(journal, &snapshot, &roster, key, false)?;
        Ok(Historical {
            trust: self.trust,
            head: self.head,
            basis: self.basis,
            source: basis,
            generation: roster.generation,
            members: roster.capsules.into_iter().map(|c| c.member).collect(),
            key: SecretBytes::from(key.as_slice()),
        })
    }
}

/// A historical read key and signed roster, bound to a current local observation.
/// Callers must apply current table cutoffs before projecting old operations.
/// Neither old writer roles nor a copied key establish permission to write now.
#[derive(Debug)]
pub struct Historical {
    trust: Trust,
    head: [u8; 32],
    basis: [u8; 32],
    source: [u8; 32],
    generation: u64,
    members: Vec<Member>,
    key: SecretBytes,
}
impl Historical {
    pub const fn source(&self) -> &[u8; 32] {
        &self.source
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    pub fn members(&self) -> &[Member] {
        &self.members
    }
    /// A changed, forked, pending or uncertain journal invalidates this handle.
    pub fn with_key<T>(
        &self,
        journal: &Journal,
        use_key: impl FnOnce(&[u8; 32]) -> T,
    ) -> Result<T> {
        if journal.trust() != self.trust
            || journal.state()
                != (State::Ready {
                    head: self.head,
                    basis: self.basis,
                })
        {
            return Err(Error::new(
                "historical key observation is no longer current",
            ));
        }
        Ok(use_key(self.key.as_slice().try_into().unwrap()))
    }
}

/// Release a member-specific key only after validating the whole current roster,
/// its exact basis facts and retained binding/epoch restrictions. No member or
/// key is inferred from possession of the outer control ciphertext.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "Valid",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "RetiredMember",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "MissingBasisFact",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "WrongCapsule",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "ForeignRecipient",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "ForkedHistory",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "OldEpoch",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "DuplicateMember",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "CascadedRecovery",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbRosterAccess",
        action = "RepeatedEpoch",
        project = "aterm_secrets::commons::roster::tests::access_transition"
    )
)]
pub fn current(journal: &Journal, identity: &Identity) -> Result<Option<Access>> {
    let Some(current) = journal.current(identity)? else {
        return Ok(None);
    };
    validate(journal, identity, current, true).map(Some)
}

fn validate(
    journal: &Journal,
    identity: &Identity,
    current: Current,
    latest: bool,
) -> Result<Access> {
    let roster = decode(current.payload.as_slice())?;
    let local = roster
        .capsules
        .iter()
        .position(|c| c.member.binding.recipient() == identity.recipient())
        .ok_or_else(|| Error::new("local device is absent from the Commons roster"))?;
    let capsule = &roster.capsules[local];
    let plain = age::decrypt(std::slice::from_ref(identity), &capsule.wire)
        .map_err(|_| Error::new("Commons key capsule could not be opened"))?;
    let members: Vec<_> = roster.capsules.iter().map(|c| c.member.clone()).collect();
    let expected_context = context(
        roster.network,
        roster.authority,
        roster.generation,
        roster.commitment,
        &members,
    );
    let bytes = plain.as_slice();
    if bytes.len() != CAPSULE_PLAIN
        || &bytes[..4] != b"ACK1"
        || bytes[4..36] != expected_context
        || bytes[36..36 + BINDING_LEN] != capsule.member.binding.bytes()
        || bytes[36 + BINDING_LEN] != capsule.member.role.byte()
        || bytes[37 + BINDING_LEN..45 + BINDING_LEN] != roster.generation.to_le_bytes()
    {
        return Err(Error::new(
            "Commons key capsule names another roster or member",
        ));
    }
    let key: &[u8; 32] = bytes[45 + BINDING_LEN..].try_into().unwrap();
    validate_evidence(journal, &current, &roster, key, latest)?;
    Ok(Access {
        trust: journal.trust(),
        head: current.head,
        basis: current.basis,
        generation: roster.generation,
        members,
        local,
        key: SecretBytes::from(key.as_slice()),
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbHistoricalPublication",
        action = "Original",
        project = "aterm_secrets::commons::roster::tests::publication::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbHistoricalPublication",
        action = "FutureDescendant",
        project = "aterm_secrets::commons::roster::tests::publication::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbHistoricalPublication",
        action = "ReusedSource",
        project = "aterm_secrets::commons::roster::tests::publication::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbHistoricalPublication",
        action = "FutureSibling",
        project = "aterm_secrets::commons::roster::tests::publication::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbHistoricalPublication",
        action = "MergedPastCopy",
        project = "aterm_secrets::commons::roster::tests::publication::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbHistoricalPublication",
        action = "DiscardedSource",
        project = "aterm_secrets::commons::roster::tests::publication::publication_transition"
    )
)]
fn validate_evidence(
    journal: &Journal,
    current: &Current,
    roster: &Roster,
    key: &[u8; 32],
    latest: bool,
) -> Result<()> {
    let trust = journal.trust();
    if roster.network != trust.network || roster.authority != trust.authority {
        return Err(Error::new("Commons roster has another trust context"));
    }
    let basis = journal
        .node(&current.basis)
        .ok_or_else(|| Error::new("Commons roster basis is missing"))?;
    let members_valid = basis
        .facts()
        .iter()
        .filter(|fact| matches!(fact, Fact::Bind { .. } | Fact::Epoch { .. }))
        .cloned()
        .collect::<BTreeSet<_>>()
        == facts(roster)
        && roster.capsules.iter().all(|capsule| {
            current
                .constraints
                .check_recorded_device(&capsule.member.binding)
                .is_ok()
        });
    let epoch_valid = current
        .constraints
        .check_key(roster.generation, key)
        .is_ok()
        && key_commitment(trust.network, key) == roster.commitment
        && if latest {
            journal.single_key_publication(roster.generation, roster.commitment)
        } else {
            journal.single_historical_key_publication(
                current.basis,
                roster.generation,
                roster.commitment,
            )
        };
    let same_head = !latest
        || (!journal.payload_superseded(current.basis)
            && journal.state()
                == (State::Ready {
                    head: current.head,
                    basis: current.basis,
                }));
    if !aterm_db_core::roster_access(same_head, members_valid, epoch_valid) {
        return Err(Error::new(
            "Commons roster or key is not currently authorized",
        ));
    }
    Ok(())
}

fn authority(seed: &[u8; 32]) -> Result<[u8; 32]> {
    Ok(Ed25519KeyPair::from_seed_unchecked(seed)
        .map_err(|_| Error::new("invalid Commons authority seed"))?
        .public_key()
        .as_ref()
        .try_into()
        .unwrap())
}
fn eligible(
    eligibility: &Eligibility,
    host: &str,
    binding: &Binding,
    network: [u8; 16],
    root: [u8; 32],
) -> Result<()> {
    if *binding.network() != network
        || *binding.authority() != root
        || !eligibility.includes(host, binding.recipient())
    {
        return Err(Error::new(
            "Commons roster requires an eligible device in its exact trust context",
        ));
    }
    Ok(())
}
fn prepare(
    seed: &[u8; 32],
    network: [u8; 16],
    generation: u64,
    mut members: Vec<Member>,
    parent: Option<[u8; 32]>,
    retirements: &[astream_wire::sync::Actor],
) -> Result<(Node, Zeroizing<[u8; 32]>)> {
    let root = authority(seed)?;
    validate_members(network, root, &members)?;
    if retirements.len() > super::constraints::MAX_FACTS {
        return Err(Error::new("Commons retirement count is out of bounds"));
    }
    let retired: BTreeSet<_> = retirements.iter().copied().collect();
    if members.len() + 1 + retired.len() > super::constraints::MAX_FACTS {
        return Err(Error::new(
            "Commons retirement and roster exceed the fact bound",
        ));
    }
    members.sort_by(|a, b| a.binding.cmp(&b.binding));
    let mut key = Zeroizing::new([0u8; 32]);
    SystemRandom::new()
        .fill(&mut *key)
        .map_err(|_| Error::new("could not generate a Commons network key"))?;
    if *key == [0; 32] {
        return Err(Error::new("generated Commons network key is invalid"));
    }
    let commitment = key_commitment(network, &key);
    let bound_context = context(network, root, generation, commitment, &members);
    let mut capsules = Vec::with_capacity(members.len());
    for member in members {
        let mut bytes = Vec::with_capacity(CAPSULE_PLAIN);
        bytes.extend_from_slice(b"ACK1");
        bytes.extend_from_slice(&bound_context);
        bytes.extend_from_slice(&member.binding.bytes());
        bytes.push(member.role.byte());
        bytes.extend_from_slice(&generation.to_le_bytes());
        bytes.extend_from_slice(&*key);
        let plain = SecretBytes::new(bytes);
        let wire = age::encrypt(&[member.binding.recipient()], plain.as_slice())?;
        if wire.len() > MAX_CAPSULE {
            return Err(Error::new("generated Commons capsule exceeds its bounds"));
        }
        capsules.push(Capsule { member, wire });
    }
    let roster = Roster {
        network,
        authority: root,
        generation,
        commitment,
        capsules,
    };
    let payload = encode(&roster);
    let recipients: Vec<_> = roster
        .capsules
        .iter()
        .map(|c| c.member.binding.recipient())
        .collect();
    let facts: Vec<_> = facts(&roster)
        .into_iter()
        .chain(retired.into_iter().map(|actor| Fact::Retire { actor }))
        .collect();
    let edit = match parent {
        Some(parent) => Edit::Update {
            parent,
            hosts: &[],
            payload: payload.as_slice(),
        },
        None => Edit::Create {
            hosts: &[],
            payload: payload.as_slice(),
        },
    };
    let authored_ms = aterm_time::unix_millis()
        .ok_or_else(|| Error::new("system clock cannot represent an author timestamp"))?;
    Ok((
        Node::seal_restricted(seed, network, edit, &recipients, authored_ms, &facts)?,
        key,
    ))
}

pub(crate) fn create(
    seed: &[u8; 32],
    eligibility: &Eligibility,
    network: [u8; 16],
    enrollments: Vec<Enrollment>,
) -> Result<Node> {
    if enrollments.is_empty() || enrollments.len() > MAX_MEMBERS {
        return Err(Error::new("Commons enrollment count is out of bounds"));
    }
    let root = authority(seed)?;
    let mut members = Vec::with_capacity(enrollments.len());
    for enrollment in enrollments {
        let binding = enrollment.possession.binding().clone();
        eligible(eligibility, &enrollment.host, &binding, network, root)?;
        members.push(Member {
            binding,
            role: enrollment.role,
        });
    }
    let (node, key) = prepare(seed, network, 1, members, None, &[])?;
    Constraints::collect(network, root, node.id(), std::iter::once(&node))?.check_key(1, &key)?;
    Ok(node)
}

pub(crate) fn rotate(
    seed: &[u8; 32],
    eligibility: &Eligibility,
    journal: &Journal,
    grants: Vec<Grant>,
) -> Result<Node> {
    rotate_retiring(seed, eligibility, journal, grants, &[])
}

pub(crate) fn rotate_retiring(
    seed: &[u8; 32],
    eligibility: &Eligibility,
    journal: &Journal,
    grants: Vec<Grant>,
    retirements: &[astream_wire::sync::Actor],
) -> Result<Node> {
    if grants.is_empty() || grants.len() > MAX_MEMBERS {
        return Err(Error::new("Commons grant count is out of bounds"));
    }
    let trust = journal.trust();
    if authority(seed)? != trust.authority {
        return Err(Error::new("Commons rotation requires the pinned authority"));
    }
    let constraints = journal.constraints()?;
    let generation = constraints
        .facts()
        .filter_map(|f| match f {
            Fact::Epoch { generation, .. } => Some(*generation),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::new("Commons epoch is exhausted"))?;
    let mut members = Vec::with_capacity(grants.len());
    for grant in grants {
        eligible(
            eligibility,
            &grant.host,
            &grant.member.binding,
            trust.network,
            trust.authority,
        )?;
        constraints.check_device_candidate(&grant.member.binding)?;
        if let Some(certificate) = grant.certificate {
            if certificate.binding() != &grant.member.binding
                || journal.node(certificate.issued_on()).is_none()
            {
                return Err(Error::new(
                    "Commons approval is absent from this control history",
                ));
            }
        } else {
            constraints.check_recorded_device(&grant.member.binding)?;
        }
        members.push(grant.member);
    }
    let (node, key) = prepare(
        seed,
        trust.network,
        generation,
        members,
        Some(constraints.head()),
        retirements,
    )?;
    let prospective = journal.prospective(&node)?;
    prospective.check_key(generation, &key)?;
    for fact in node.facts() {
        if let Fact::Bind { binding } = fact {
            prospective.check_recorded_device(binding)?;
        }
    }
    Ok(node)
}

#[cfg(test)]
pub(crate) mod tests;
