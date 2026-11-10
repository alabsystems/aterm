// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed membership snapshots with recipient-specific network-key capsules.
//! A current journal and the full retained policy must permit every member.

use super::{
    binding::{BINDING_LEN, Binding, Possession},
    constraints::{Constraints, Fact, LEGACY_MAX_FACTS, MAX_FACTS, key_commitment},
    hearing::Gate,
    journal::{Current, Journal, State, Trust},
    membership::{Certificate, Eligibility},
    node::{Edit, MAX_NODE, MAX_PAYLOAD, Node},
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

/// One fact per member plus the epoch fact fills a control node's [`MAX_FACTS`]; the roster
/// fits [`MAX_PAYLOAD`] and its node [`MAX_NODE`] with room to spare (checked below).
pub const MAX_MEMBERS: usize = MAX_FACTS - 1;
/// The roster bound of 0.145 and earlier. A larger roster is written only through [`Gate`].
pub const LEGACY_MAX_MEMBERS: usize = LEGACY_MAX_FACTS - 1;
/// Binary age's version line and MAC line, and one X25519 stanza (a 54-byte share line and a
/// 44-byte body line).
const AGE_LINES: usize = 22 + 48;
const AGE_STANZA: usize = 98;
/// A capsule is binary age to one recipient: its header, the payload nonce, the plaintext and
/// one tag. Every capsule written so far has exactly this size
/// (`tests::one_recipient_capsule_is_measured`).
const MAX_CAPSULE: usize = AGE_LINES + AGE_STANZA + 16 + CAPSULE_PLAIN + 16;
const HEADER: usize = 4 + 16 + 32 + 8 + 32 + 2;
const CAPSULE_PLAIN: usize = 4 + 32 + BINDING_LEN + 1 + 8 + 32;
const MAX_ROSTER: usize = HEADER + MAX_MEMBERS * (BINDING_LEN + 1 + 2 + MAX_CAPSULE);
/// The largest roster node: its fixed fields and signature, every fact a binding, and the
/// largest roster sealed to every member in 64 KiB chunks.
const MAX_ROSTER_NODE: usize = 4
    + 16
    + 32
    + 2
    + 32
    + 32
    + 8
    + 2
    + 4
    + 64
    + MAX_FACTS * (1 + BINDING_LEN)
    + AGE_LINES
    + MAX_MEMBERS * AGE_STANZA
    + 16
    + MAX_ROSTER
    + MAX_ROSTER.div_ceil(64 * 1024) * 16;
const _: () = assert!(MAX_ROSTER <= MAX_PAYLOAD && MAX_ROSTER_NODE <= MAX_NODE);
const _: () = assert!(MAX_MEMBERS >= 128 && MAX_MEMBERS <= u16::MAX as usize);

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
    control_history_head: [u8; 32],
    basis: [u8; 32],
    generation: u64,
    members: Vec<Member>,
    local: usize,
    key: SecretBytes,
    log_paths: Option<crate::client::Paths>,
    log_heads_digest: Option<[u8; 32]>,
    seals: bool,
}
impl Access {
    pub const fn head(&self) -> &[u8; 32] {
        &self.head
    }
    /// The control-history commitment used by older application peers. This does
    /// not replace [`Self::head`]'s binding-sensitive local authorization fence.
    pub const fn control_history_head(&self) -> &[u8; 32] {
        &self.control_history_head
    }
    /// Exact data node supplying the roster; a recovery head has no payload.
    pub const fn basis(&self) -> &[u8; 32] {
        &self.basis
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    /// Whether this observation derives from the peer-synced membership log (`F18`).
    pub const fn is_log_backed(&self) -> bool {
        self.log_paths.is_some()
    }
    /// Whether a writer may seal new rows under [`Self::basis`]: not while that epoch is due a
    /// rotation ([`crate::membership_log::CommonsEpochView::due`], P1's gate), so that a Mac a
    /// `Remove` took out could open them, until the rotation lands. Reading is unaffected.
    pub const fn seals(&self) -> bool {
        self.seals
    }
    /// Whether a table writer that waits for keys (`waits_for_key`) keeps its operation pending
    /// rather than sealing it now (M4, docs/DESIGN-alab-one-mesh-2026-11-05.md §11 B11): the
    /// basis is due a rotation (P1's gate, `Log::head_due`: its causal past lacks a revoking
    /// row's entry or the `Remove`s that row's gate needs, or it still wraps a Mac the log
    /// removed). A table writer never mints an epoch itself.
    /// PF6's `writer_waits` kernel decides it, the vault's wait's own, and the 8,500 stop never
    /// judges the rotation it waits on (`stop_judges`).
    #[must_use]
    pub fn waits_for_rotation(&self, waits_for_key: bool) -> bool {
        aterm_mesh_core::capacity::writer_waits(waits_for_key, false, !self.seals)
    }
    pub fn members(&self) -> &[Member] {
        &self.members
    }
    /// Validate the prospective roster's capacity and device identity uniqueness.
    /// This grants no membership and carries no certificate or key outward.
    pub(crate) fn check_addition(&self, binding: &Binding, role: Role) -> Result<()> {
        let mut members = self.members.clone();
        members.push(Member {
            binding: binding.clone(),
            role,
        });
        validate_members(self.trust.network, self.trust.authority, &members)
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
        self.is_current_observed(journal).unwrap_or(false)
    }

    /// [`Self::is_current`] for a caller that may observe again: a vault writer that holds the
    /// state lock at this moment is [`crate::ErrorKind::VaultBusy`], not "no longer current".
    /// Every other answer is the boolean's, an observation that cannot be read still not current.
    ///
    /// # Errors
    /// A vault writer holds the state lock ([`crate::ErrorKind::VaultBusy`]); nothing was judged.
    pub fn is_current_observed(&self, journal: &Journal) -> crate::Result<bool> {
        let current = if let Some(paths) = &self.log_paths {
            journal.trust() == self.trust
                && match crate::client::with_roster_observation_or_busy(paths, |observation| {
                    let Some(lock) = observation.state_lock() else {
                        return Ok(false);
                    };
                    log_observation_is_current(
                        paths,
                        self.trust,
                        Some(self.local().binding.recipient()),
                        self.head,
                        self.basis,
                        self.log_heads_digest,
                        (lock, &crate::fsx::Glance::new()),
                    )
                }) {
                    Err(busy) if busy.kind() == crate::ErrorKind::VaultBusy => return Err(busy),
                    observed => observed.ok().flatten().unwrap_or(false),
                }
        } else {
            journal.trust() == self.trust
                && journal.state()
                    == (State::Ready {
                        head: self.head,
                        basis: self.basis,
                    })
        };
        Ok(aterm_db_core::roster_access(current, true, true))
    }

    /// [`Self::is_current`] through `look`, one look at this access's vault for checks that stand
    /// for one moment of it ([`crate::client::VaultLook`]).
    pub fn is_current_in(&self, journal: &Journal, look: &crate::client::VaultLook<'_>) -> bool {
        self.is_current_observed_in(journal, look).unwrap_or(false)
    }

    /// [`Self::is_current_observed`] through `look`: judged under the look's shared observation of
    /// the state lock and its look at each file, rather than an observation and a look of its own,
    /// where it looks at this access's vault and observes that lock; otherwise
    /// [`Self::is_current_observed`]'s own. A file that changed under the look without taking the
    /// lock is judged by what the look's memos kept for their first stamps of it, where they kept
    /// an answer (a miss reads it afresh, as the memo's own call does), and the next look sees it.
    ///
    /// # Errors
    /// As [`Self::is_current_observed`].
    pub fn is_current_observed_in(
        &self,
        journal: &Journal,
        look: &crate::client::VaultLook<'_>,
    ) -> crate::Result<bool> {
        let (Some(paths), Some((lock, glance))) = (&self.log_paths, look.held()) else {
            return self.is_current_observed(journal);
        };
        if look.paths() != paths {
            return self.is_current_observed(journal);
        }
        let current = journal.trust() == self.trust
            && match log_observation_is_current(
                paths,
                self.trust,
                Some(self.local().binding.recipient()),
                self.head,
                self.basis,
                self.log_heads_digest,
                (lock, glance),
            ) {
                Err(busy) if busy.kind() == crate::ErrorKind::VaultBusy => return Err(busy),
                observed => observed.unwrap_or(false),
            };
        Ok(aterm_db_core::roster_access(current, true, true))
    }
    /// Refuse a stale observation, a fork, missing history or failed durability
    /// before exposing the key to a trusted local caller's operation.
    pub fn with_key<T>(
        &self,
        journal: &Journal,
        use_key: impl FnOnce(&[u8; 32]) -> T,
    ) -> Result<T> {
        if let Some(paths) = &self.log_paths {
            return crate::client::with_roster_observation(paths, |observation| {
                let lock = observation
                    .state_lock()
                    .ok_or_else(|| Error::new("Commons roster observation is unavailable"))?;
                if journal.trust() != self.trust
                    || !log_observation_is_current(
                        paths,
                        self.trust,
                        Some(self.local().binding.recipient()),
                        self.head,
                        self.basis,
                        self.log_heads_digest,
                        (lock, &crate::fsx::Glance::new()),
                    )?
                {
                    return Err(Error::new(
                        "Commons roster observation is no longer current",
                    ));
                }
                Ok(use_key(self.key.as_slice().try_into().unwrap()))
            })?
            .ok_or_else(|| Error::new("Commons roster observation is unavailable"));
        }
        if !self.is_current(journal) {
            return Err(Error::new(
                "Commons roster observation is no longer current",
            ));
        }
        Ok(use_key(self.key.as_slice().try_into().unwrap()))
    }

    pub(super) fn historical_from_log(
        &self,
        journal: &Journal,
        identity: &Identity,
        basis: [u8; 32],
    ) -> Result<Option<Historical>> {
        self.with_log_history(journal, identity, |history| match history? {
            Some(history) => history.historical(basis),
            None => Ok(None),
        })
    }

    /// Lend `read` one check of the current local membership, one read of the log and one lease
    /// of its complete history, which every historical read of a batch shares: `None` where this
    /// observation is not log-backed, and an error where this is not the current local
    /// membership, or the log is unavailable or one of its complete records cannot be leased. A
    /// repair round reads a batch (`aterm-db`'s `Session::history_needs`), and when each basis
    /// checked, read and leased again that was 30% of a quiet round's CPU over 4,805 records
    /// (`docs/measured/commons-long-log-rounds-2026-11-07.md`).
    pub(super) fn with_log_history<R>(
        &self,
        journal: &Journal,
        identity: &Identity,
        read: impl FnOnce(Result<Option<LogHistory<'_>>>) -> R,
    ) -> R {
        let Some(paths) = &self.log_paths else {
            return read(Ok(None));
        };
        if !self.is_current(journal) || self.local().binding.recipient() != identity.recipient() {
            return read(Err(Error::new(
                "historical access requires the current local membership",
            )));
        }
        let log = match crate::client::trusted_commons_log_now(paths, self.trust) {
            Ok(Some((log, _heads_digest))) => log,
            Ok(None) => return read(Err(Error::new("Commons membership log is unavailable"))),
            Err(error) => return read(Err(error)),
        };
        let history = crate::membership_log::history::HistoryView::new(&log);
        // Validate required history before a warm positive or negative cache entry. Pending
        // records do not change the complete-generation digest and must not hit cached absence.
        match history.lease_complete() {
            Ok(lease) => read(Ok(Some(LogHistory {
                access: self,
                identity,
                lease: &lease,
            }))),
            Err(error) => read(Err(error)),
        }
    }

    // The public path checks current local membership before it leases this immutable history.
    fn historical_from_lease(
        &self,
        identity: &Identity,
        basis: [u8; 32],
        lease: &crate::membership_log::history::HistoryLease<'_>,
    ) -> Result<Option<Historical>> {
        use crate::membership_log::history::{Generation, RecordState};
        type HistoricalCacheKey = (Trust, [u8; 32], Generation, [u8; 32]);
        type HistoricalCacheVal = Option<(u64, Vec<Member>, Zeroizing<[u8; 32]>)>;
        static HISTORICAL_CACHE: std::sync::Mutex<
            std::collections::VecDeque<(HistoricalCacheKey, HistoricalCacheVal)>,
        > = std::sync::Mutex::new(std::collections::VecDeque::new());

        #[cfg(any(test, debug_assertions))]
        KEY_CACHE_ASKS.with(|asks| asks.set(asks.get() + 1));
        let Some(paths) = &self.log_paths else {
            return Ok(None);
        };
        let epoch = match lease.record(&basis)? {
            RecordState::Complete(epoch) => Some(epoch),
            RecordState::Pending => {
                return Err(Error::new("historical Commons epoch is not complete"));
            }
            RecordState::Absent => None,
        };
        let cache_key = (
            self.trust,
            identity.recipient().0,
            lease.generation(),
            basis,
        );
        if let Ok(cache) = HISTORICAL_CACHE.lock()
            && let Some((_, cached)) = cache.iter().find(|(k, _)| *k == cache_key)
        {
            return Ok(cached
                .as_ref()
                .map(|(generation, members, key_bytes)| Historical {
                    trust: self.trust,
                    head: self.head,
                    basis: self.basis,
                    source: basis,
                    generation: *generation,
                    members: members.clone(),
                    key: SecretBytes::from(key_bytes.as_slice()),
                    log_paths: Some(paths.clone()),
                    log_heads_digest: self.log_heads_digest,
                    local_recipient: Some(self.local().binding.recipient()),
                }));
        }
        let opened = match epoch.as_ref() {
            Some(epoch) => open_from_lease(identity, epoch, lease)?,
            None => None,
        };
        let Some((generation, bindings, key)) = opened else {
            if let Ok(mut cache) = HISTORICAL_CACHE.lock() {
                if cache.len() >= 16 {
                    cache.pop_front();
                }
                cache.push_back((cache_key, None));
            }
            return Ok(None);
        };
        let members = self.log_writers(bindings);
        if let Ok(mut cache) = HISTORICAL_CACHE.lock() {
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((cache_key, Some((generation, members.clone(), key.clone()))));
        }
        Ok(Some(Historical {
            trust: self.trust,
            head: self.head,
            basis: self.basis,
            source: basis,
            generation,
            members,
            key: SecretBytes::from(key.as_slice()),
            log_paths: Some(paths.clone()),
            log_heads_digest: self.log_heads_digest,
            local_recipient: Some(self.local().binding.recipient()),
        }))
    }

    /// Whether [`Access::historical_from_lease`] would answer `basis` with a key, asked without
    /// one: an epoch `identity` opened under the same held records before is answered without its
    /// key unwrapped again ([`HistoryLease::opened`]), and a key unwrapped to answer is neither kept
    /// nor given. A repair round asks whether this Mac holds each epoch of its history, sixteen a
    /// round on each side (`aterm-db`'s `Session::history_needs`): unwrapping each again took 7.7%
    /// of a quiet round over a log of 9,605 records, and keeping each pushed the keys a table's
    /// reads take out of the sixteen kept (2026-11-09).
    ///
    /// [`HistoryLease::opened`]: crate::membership_log::history::HistoryLease::opened
    ///
    /// # Errors
    /// As [`Access::historical_from_lease`]: the epoch is pending its parents, or its key is not
    /// wrapped to `identity` or does not open.
    fn opens_from_lease(
        &self,
        identity: &Identity,
        basis: [u8; 32],
        lease: &crate::membership_log::history::HistoryLease<'_>,
    ) -> Result<bool> {
        use crate::membership_log::history::RecordState;
        if self.log_paths.is_none() {
            return Ok(false);
        }
        let epoch = match lease.record(&basis)? {
            RecordState::Complete(epoch) => epoch,
            RecordState::Pending => {
                return Err(Error::new("historical Commons epoch is not complete"));
            }
            RecordState::Absent => return Ok(false),
        };
        if lease.opened(&epoch, identity)? {
            return Ok(true);
        }
        Ok(open_from_lease(identity, &epoch, lease)?.is_some())
    }

    /// The writers of a log-era Commons epoch in this network, by binding, one per actor.
    fn log_writers(&self, bindings: Vec<Binding>) -> Vec<Member> {
        let mut members: Vec<Member> = bindings
            .into_iter()
            .filter(|b| {
                *b.network() == self.trust.network && *b.authority() == self.trust.authority
            })
            .map(|binding| Member {
                binding,
                role: Role::Writer,
            })
            .collect();
        members.sort_by(|a, b| a.binding.cmp(&b.binding));
        members.dedup_by(|a, b| a.binding.actor() == b.binding.actor());
        members
    }

    /// A log-era Commons epoch `source` opened with `key`, a history handoff's: `None` where
    /// this observation is not log-backed or the log holds no such epoch (a control journal
    /// node's id instead). The log's signed `Epoch` must commit to `key`, and `source` must be
    /// `under` (the handoff's issuance) or in its causal past; its writers come from the log, as
    /// for a member that holds its own `Wrap`.
    ///
    /// # Errors
    /// This is not the current local membership, the log is unavailable or holds `source` only
    /// pending its parents, or `key` is not `source`'s key or `source` is not in the issuance's
    /// past.
    pub(super) fn historical_from_log_with_key(
        &self,
        journal: &Journal,
        identity: &Identity,
        source: [u8; 32],
        under: [u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<Historical>> {
        self.with_log_history(journal, identity, |history| match history? {
            Some(history) => history.historical_with_key(source, under, key),
            None => Ok(None),
        })
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
        let snapshot = if self.is_log_backed() {
            journal.retained_historical(basis, identity)?
        } else {
            journal.historical(basis, identity)?
        };
        let past = validate(journal, identity, snapshot, false)?;
        Ok(Historical {
            trust: self.trust,
            head: self.head,
            basis: self.basis,
            source: past.basis,
            generation: past.generation,
            members: past.members,
            key: past.key,
            log_paths: self.log_paths.clone(),
            log_heads_digest: self.log_heads_digest,
            local_recipient: Some(self.local().binding.recipient()),
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
        let snapshot = if self.is_log_backed() {
            journal.retained_historical_with_key(basis, file_key)?
        } else {
            journal.historical_with_key(basis, file_key)?
        };
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
            log_paths: self.log_paths.clone(),
            log_heads_digest: self.log_heads_digest,
            local_recipient: Some(self.local().binding.recipient()),
        })
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// The calls of [`Access::historical_from_lease`], which asks the kept historical keys, made
    /// on this thread, counted (never timed).
    pub(super) static KEY_CACHE_ASKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The calls of [`Access::historical_from_lease`], which asks the kept historical keys, this
/// thread made: a law that a history ask takes no kept key (`aterm-db`'s `Session::history_needs`)
/// reads it before and after.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn test_kept_key_asks() -> usize {
    KEY_CACHE_ASKS.with(std::cell::Cell::get)
}

/// What [`open_from_lease`] unwraps of an epoch: its generation, the bindings of its writers and
/// its key.
type OpenedEpoch = (u64, Vec<Binding>, Zeroizing<[u8; 32]>);

/// `epoch`'s generation, the bindings of its writers and its key, unwrapped for `identity` under
/// `lease` and noted as opened there ([`HistoryLease::note_opened`]), or `None` where the log names
/// no writers of it.
///
/// [`HistoryLease::note_opened`]: crate::membership_log::history::HistoryLease::note_opened
///
/// # Errors
/// The epoch belongs to another history, or its key is not wrapped to `identity` or does not open.
fn open_from_lease(
    identity: &Identity,
    epoch: &crate::membership_log::history::RecordLease<'_>,
    lease: &crate::membership_log::history::HistoryLease<'_>,
) -> Result<Option<OpenedEpoch>> {
    let Some((generation, bindings)) = lease.historical_commons_writers(epoch)? else {
        return Ok(None);
    };
    let Some(key) = lease.unwrap_commons_epoch(epoch, identity)? else {
        return Err(Error::new(
            "historical Commons epoch is not wrapped to this device",
        ));
    };
    lease.note_opened(epoch, identity)?;
    Ok(Some((generation, bindings, key)))
}

/// One check of the current local membership and one lease of the log's complete history, which
/// the historical reads of one batch share ([`Access::with_log_history`]).
pub(super) struct LogHistory<'h> {
    access: &'h Access,
    identity: &'h Identity,
    lease: &'h crate::membership_log::history::HistoryLease<'h>,
}

impl LogHistory<'_> {
    /// Whether this history was leased for `access` and `identity`.
    pub(super) fn serves(&self, access: &Access, identity: &Identity) -> bool {
        std::ptr::eq(self.access, access) && self.identity.recipient() == identity.recipient()
    }

    /// [`Access::historical_from_log`] of `basis`, over this history.
    pub(super) fn historical(&self, basis: [u8; 32]) -> Result<Option<Historical>> {
        self.access
            .historical_from_lease(self.identity, basis, self.lease)
    }

    /// Whether [`LogHistory::historical`] would answer `basis` with a key, asked without one
    /// ([`Access::opens_from_lease`]).
    pub(super) fn opens(&self, basis: [u8; 32]) -> Result<bool> {
        self.access
            .opens_from_lease(self.identity, basis, self.lease)
    }

    /// [`Access::historical_from_log_with_key`], over this history.
    pub(super) fn historical_with_key(
        &self,
        source: [u8; 32],
        under: [u8; 32],
        key: &[u8; 32],
    ) -> Result<Option<Historical>> {
        use crate::membership_log::history::RecordState;
        let access = self.access;
        let lease = self.lease;
        let epoch = match lease.record(&source)? {
            RecordState::Complete(epoch) => epoch,
            RecordState::Pending => {
                return Err(Error::new("historical Commons epoch is not complete"));
            }
            RecordState::Absent => return Ok(None),
        };
        let Some((generation, bindings)) = lease.historical_commons_writers(&epoch)? else {
            return Ok(None);
        };
        if !lease.commons_epoch_key_under(&epoch, &under, key)? {
            return Err(Error::new(
                "history handoff key does not open its Commons epoch under its issuance",
            ));
        }
        Ok(Some(Historical {
            trust: access.trust,
            head: access.head,
            basis: access.basis,
            source,
            generation,
            members: access.log_writers(bindings),
            key: SecretBytes::from(key.as_slice()),
            log_paths: access.log_paths.clone(),
            log_heads_digest: access.log_heads_digest,
            local_recipient: Some(access.local().binding.recipient()),
        }))
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
    log_paths: Option<crate::client::Paths>,
    log_heads_digest: Option<[u8; 32]>,
    local_recipient: Option<crate::age::Recipient>,
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
        if let Some(paths) = &self.log_paths {
            return crate::client::with_roster_observation(paths, |observation| {
                let lock = observation
                    .state_lock()
                    .ok_or_else(|| Error::new("historical key observation is unavailable"))?;
                if journal.trust() != self.trust
                    || !log_observation_is_current(
                        paths,
                        self.trust,
                        self.local_recipient,
                        self.head,
                        self.basis,
                        self.log_heads_digest,
                        (lock, &crate::fsx::Glance::new()),
                    )?
                {
                    return Err(Error::new(
                        "historical key observation is no longer current",
                    ));
                }
                Ok(use_key(self.key.as_slice().try_into().unwrap()))
            })?
            .ok_or_else(|| Error::new("historical key observation is unavailable"));
        }
        let current = journal.trust() == self.trust
            && journal.state()
                == (State::Ready {
                    head: self.head,
                    basis: self.basis,
                });
        if !current {
            return Err(Error::new(
                "historical key observation is no longer current",
            ));
        }
        Ok(use_key(self.key.as_slice().try_into().unwrap()))
    }
}

fn log_observation_is_current(
    paths: &crate::client::Paths,
    trust: Trust,
    recipient: Option<crate::age::Recipient>,
    head: [u8; 32],
    basis: [u8; 32],
    remembered: Option<[u8; 32]>,
    (lock, glance): (&crate::client::StateLock, &crate::fsx::Glance),
) -> Result<bool> {
    // One look at the vault's files for the whole check, `glance` (the caller's, or the look's
    // of a moment the check stands for: [`crate::fsx::Glance`]): the identity's, the log memo's
    // stamps and the state digest name many of the same files.
    // The recipient alone answers a quiet check, kept while the identity file stands; the
    // identity itself is read only to judge the log afresh.
    let Some(local) = crate::client::identity_recipient(paths, Some(glance))? else {
        return Ok(false);
    };
    if recipient.is_some_and(|expected| expected != local) {
        return Ok(false);
    }
    let Some((log, heads_digest)) =
        crate::client::trusted_commons_log_under(paths, trust, lock, glance)?
    else {
        return Ok(false);
    };
    if remembered == Some(commons_log_state_digest(paths, heads_digest, Some(glance))?) {
        return Ok(true);
    }
    let Some(identity) = crate::client::read_identity(paths)? else {
        return Ok(false);
    };
    if recipient.is_some_and(|expected| expected != identity.recipient()) {
        return Ok(false);
    }
    Ok(
        current_from_loaded_log(paths, trust, &identity, &log, heads_digest, lock, glance)?
            .is_some_and(|fresh| fresh.head == head && fresh.basis == basis),
    )
}

/// The digest of the state a Commons log observation stands on, each file's `lstat` `glance`'s
/// where the caller's check looks through one ([`crate::fsx::Glance`]).
fn commons_log_state_digest(
    paths: &crate::client::Paths,
    heads_digest: [u8; 32],
    glance: Option<&crate::fsx::Glance>,
) -> Result<[u8; 32]> {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(heads_digest);
    // Each file's digest is kept while the file stands ([`crate::fsx::private_digest`]); the
    // state's is of what it holds but when the vault last heard this Mac and the receipts of its
    // rounds ([`crate::client::state_standing_in`]), which no access is judged by and which a
    // round of the vault's subscriber rewrites about every 25 s, each rewrite judging every
    // remembered access afresh (2026-11-09).
    let state = crate::client::state_standing_in(paths, glance)?;
    let source = crate::fsx::public_digest(
        &paths.dir.join("source.toml"),
        crate::client::MAX_STATE,
        glance,
    )?;
    for digest in [state, source] {
        if let Some(digest) = digest {
            hash.update([1]);
            hash.update(digest);
        } else {
            hash.update([0]);
        }
    }
    // The files an access is judged by beside the log and the state: the vault and this Mac's
    // identity, the device whose binding a node denial judges and the claim it is held to
    // (`Device::existing_binding_for_observation`), and the fold mark that names the audience.
    for extra in [
        paths.dir.join("vault"),
        paths.dir.join("identity"),
        paths.dir.join("commons").join("device.age"),
        paths.dir.join("commons").join("device.claim"),
        crate::commons::fold::mark(paths),
    ] {
        if let Some(meta) = crate::fsx::lstat_in(glance, &extra)? {
            hash.update([1]);
            hash.update(meta.len().to_le_bytes());
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                hash.update(meta.dev().to_le_bytes());
                hash.update(meta.ino().to_le_bytes());
                hash.update(meta.mode().to_le_bytes());
                hash.update(meta.uid().to_le_bytes());
                hash.update(meta.gid().to_le_bytes());
                hash.update(meta.nlink().to_le_bytes());
                hash.update(meta.ctime().to_le_bytes());
                hash.update(meta.ctime_nsec().to_le_bytes());
            }
            let dur = meta.modified().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            })?;
            hash.update(dur.as_nanos().to_le_bytes());
        } else {
            hash.update([0]);
        }
    }
    Ok(hash.finalize())
}

/// Observe current Commons membership and network key from the peer-synced membership log (`F18`),
/// the state it stands on as the caller's check looked at it (`glance`, [`crate::fsx::Glance`]).
pub(super) fn current_from_loaded_log(
    paths: &crate::client::Paths,
    trust: Trust,
    identity: &Identity,
    log: &crate::membership_log::Log,
    heads_digest: [u8; 32],
    lock: &crate::client::StateLock,
    glance: &crate::fsx::Glance,
) -> Result<Option<Access>> {
    type AccessCacheKey = (Trust, [u8; 32], [u8; 32]);
    type AccessCacheVal = Option<(
        [u8; 32],
        [u8; 32],
        [u8; 32],
        u64,
        Vec<Member>,
        usize,
        Zeroizing<[u8; 32]>,
        bool,
    )>;
    static ACCESS_CACHE: std::sync::Mutex<
        std::collections::VecDeque<(AccessCacheKey, AccessCacheVal)>,
    > = std::sync::Mutex::new(std::collections::VecDeque::new());

    let state_digest = commons_log_state_digest(paths, heads_digest, Some(glance))?;
    let cache_key = (trust, identity.recipient().0, state_digest);
    if let Ok(cache) = ACCESS_CACHE.lock()
        && let Some((_, cached)) = cache.iter().find(|(k, _)| *k == cache_key)
    {
        return Ok(cached.as_ref().map(
            |(head, control_history_head, basis, generation, members, local, key_bytes, seals)| {
                Access {
                    trust,
                    head: *head,
                    control_history_head: *control_history_head,
                    basis: *basis,
                    generation: *generation,
                    members: members.clone(),
                    local: *local,
                    key: SecretBytes::from(key_bytes.as_slice()),
                    log_paths: Some(paths.clone()),
                    log_heads_digest: Some(state_digest),
                    seals: *seals,
                }
            },
        ));
    }
    if crate::client::local_recipient_denial_held(paths, lock)? {
        if let Ok(mut cache) = ACCESS_CACHE.lock() {
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((cache_key, None));
        }
        return Ok(None);
    }
    // The fold's adoption gate (`commons::fold`) decides which audience this Mac writes under;
    // readers open both whatever it says (`historical_from_lease`).
    let view = if crate::commons::fold::on(paths) {
        log.fold_epoch_view(identity)?
    } else {
        log.commons_epoch_view(identity)?
    };
    let Some(view) = view else {
        if let Ok(mut cache) = ACCESS_CACHE.lock() {
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((cache_key, None));
        }
        return Ok(None);
    };
    let roster = crate::client::local_swarm_roster_for_log_held(paths, log, lock)?;
    let membership = log.membership();
    let mut by_recipient: std::collections::BTreeMap<
        crate::age::Recipient,
        Vec<&crate::membership_log::Member>,
    > = std::collections::BTreeMap::new();
    for (actor, m) in membership.members() {
        if *m.binding.network() == trust.network
            && *m.binding.authority() == trust.authority
            && !log.recipient_is_removed(&m.binding.recipient())
            && !log.audience_left_out(&crate::membership_log::EpochAudience::Commons, actor)
            && !log.audience_left_out(&crate::membership_log::EpochAudience::Everyone, actor)
            && !log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Commons,
                &m.binding.recipient(),
            )
            && !log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &m.binding.recipient(),
            )
            && !membership.member_node_removed(m)
            && !roster
                .as_ref()
                .is_some_and(|r| crate::client::member_denied_by_roster(r, m))
        {
            by_recipient
                .entry(m.binding.recipient())
                .or_default()
                .push(m);
        }
    }
    let genesis_id = *log.network();
    let mut chosen_per_recipient: Vec<([u8; 32], bool, Binding)> =
        Vec::with_capacity(by_recipient.len());
    for (_, group) in by_recipient {
        if group.len() == 1 {
            let m = group[0];
            chosen_per_recipient.push((m.entry, m.entry == genesis_id, m.binding.clone()));
            continue;
        }
        let earliest_is_genesis = group.iter().any(|m| m.entry == genesis_id);
        let earliest_entry = group
            .iter()
            .map(|m| (m.entry != genesis_id, m.entry))
            .min()
            .map(|(_, e)| e)
            .unwrap_or(genesis_id);
        let chosen = group
            .iter()
            .copied()
            .filter(|a| {
                !group.iter().any(|b| {
                    a.entry != b.entry
                        && log
                            .ancestor_chain(&b.entry)
                            .is_some_and(|chain| chain.iter().any(|r| *r.id() == a.entry))
                })
            })
            .min_by(|a, b| {
                (a.entry == genesis_id, &a.entry, &a.binding).cmp(&(
                    b.entry == genesis_id,
                    &b.entry,
                    &b.binding,
                ))
            })
            .unwrap_or(group[0]);
        chosen_per_recipient.push((earliest_entry, earliest_is_genesis, chosen.binding.clone()));
    }
    chosen_per_recipient.sort_by(|a, b| (!a.1, &a.0, &a.2).cmp(&(!b.1, &b.0, &b.2)));
    let (mut actors, mut writers, mut machines) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    let mut members: Vec<Member> = Vec::with_capacity(chosen_per_recipient.len().min(MAX_MEMBERS));
    for (_, _, binding) in chosen_per_recipient {
        if members.len() >= MAX_MEMBERS {
            break;
        }
        if actors.contains(&binding.actor())
            || writers.contains(binding.writer())
            || machines.contains(binding.machine())
        {
            continue;
        }
        actors.insert(binding.actor());
        writers.insert(*binding.writer());
        machines.insert(*binding.machine());
        members.push(Member {
            binding,
            role: Role::Writer,
        });
    }
    if members.is_empty() {
        return Ok(None);
    }
    members.sort_by(|a, b| a.binding.cmp(&b.binding));
    validate_members(trust.network, trust.authority, &members)?;
    let Some(local) = members
        .iter()
        .position(|m| m.binding.recipient() == identity.recipient())
    else {
        if let Ok(mut cache) = ACCESS_CACHE.lock() {
            if cache.len() >= 16 {
                cache.pop_front();
            }
            cache.push_back((cache_key, None));
        }
        return Ok(None);
    };
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"ALCO2");
    hash.update(heads_digest);
    hash.update(view.basis);
    let control_history_head = hash.clone().finalize();
    for m in &members {
        hash.update(m.binding.bytes());
    }
    let head = hash.finalize();
    if let Ok(mut cache) = ACCESS_CACHE.lock() {
        if cache.len() >= 16 {
            cache.pop_front();
        }
        cache.push_back((
            cache_key,
            Some((
                head,
                control_history_head,
                view.basis,
                view.generation,
                members.clone(),
                local,
                view.key.clone(),
                !view.due,
            )),
        ));
    }
    Ok(Some(Access {
        trust,
        head,
        control_history_head,
        basis: view.basis,
        generation: view.generation,
        members,
        local,
        key: SecretBytes::from(view.key.as_slice()),
        log_paths: Some(paths.clone()),
        log_heads_digest: Some(state_digest),
        seals: !view.due,
    }))
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
        control_history_head: current.head,
        basis: current.basis,
        generation: roster.generation,
        members,
        local,
        key: SecretBytes::from(key.as_slice()),
        log_paths: None,
        log_heads_digest: None,
        seals: true,
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
    retired_recipients: &[crate::age::Recipient],
) -> Result<(Node, Zeroizing<[u8; 32]>)> {
    let root = authority(seed)?;
    validate_members(network, root, &members)?;
    if retirements.len() > MAX_FACTS || retired_recipients.len() > MAX_FACTS {
        return Err(Error::new("Commons retirement count is out of bounds"));
    }
    let retired: BTreeSet<_> = retirements.iter().copied().collect();
    let retired_recipients: BTreeSet<_> = retired_recipients.iter().copied().collect();
    if members.len() + 1 + retired.len() + retired_recipients.len() > MAX_FACTS {
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
        .chain(
            retired_recipients
                .into_iter()
                .map(|recipient| Fact::RetireRecipient { recipient }),
        )
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

/// A genesis stays within what 0.145 reads: before it there is no ledger to open [`Gate`]
/// with, and the roster grows past [`LEGACY_MAX_MEMBERS`] by enrollment.
pub(crate) fn create(
    seed: &[u8; 32],
    eligibility: &Eligibility,
    network: [u8; 16],
    enrollments: Vec<Enrollment>,
) -> Result<Node> {
    if enrollments.is_empty() || enrollments.len() > LEGACY_MAX_MEMBERS {
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
    let (node, key) = prepare(seed, network, 1, members, None, &[], &[])?;
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
    rotate_retiring_recipients(seed, eligibility, journal, grants, retirements, &[])
}

pub(crate) fn rotate_retiring_recipients(
    seed: &[u8; 32],
    eligibility: &Eligibility,
    journal: &Journal,
    grants: Vec<Grant>,
    retirements: &[astream_wire::sync::Actor],
    retired_recipients: &[crate::age::Recipient],
) -> Result<Node> {
    rotate_gated(
        seed,
        eligibility,
        journal,
        grants,
        retirements,
        retired_recipients,
        None,
    )
}

/// Whether this control history already holds a node 0.145 refuses. A ready journal has one
/// head, so every retained node is its ancestor.
pub(crate) fn crossed(journal: &Journal) -> bool {
    journal
        .inventory()
        .filter_map(|id| journal.node(id))
        .any(|node| node.facts().len() > LEGACY_MAX_FACTS)
}

/// [`rotate_retiring_recipients`] that may write a node past the 0.145 caps when `gate` admits
/// it ([`Gate::admit`]); without `gate` such a node is refused until the history holds one.
pub(crate) fn rotate_gated(
    seed: &[u8; 32],
    eligibility: &Eligibility,
    journal: &Journal,
    grants: Vec<Grant>,
    retirements: &[astream_wire::sync::Actor],
    retired_recipients: &[crate::age::Recipient],
    gate: Option<&Gate<'_>>,
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
        retired_recipients,
    )?;
    let State::Ready { basis, .. } = journal.state() else {
        return Err(Error::new(
            "roster publication requires a ready control journal",
        ));
    };
    // Every device that reads either roster: the current members, and those this one adds.
    let mut readers = std::collections::BTreeMap::new();
    for fact in journal
        .node(&basis)
        .into_iter()
        .flat_map(Node::facts)
        .chain(node.facts())
    {
        if let Fact::Bind { binding } = fact {
            readers.insert(binding.actor(), binding);
        }
    }
    Gate::admit(
        gate,
        node.facts().len(),
        crossed(journal),
        readers.into_values(),
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
