// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated, locally applied entries. An observation of bytes is never a
//! grant to use them as a provider login or to spend an account's budget, with one exception:
//! the typed read of a Claude Code token ([`read_claude_token_until`], `crate::claude_token`),
//! which the owner sanctioned on 2026-11-05 as Claude Code's headless login
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §4.2). Only that read returns a token class
//! value; every other reading refuses the class.

use super::*;

mod admission;
pub use admission::EntryAdmission;
mod read_context;
mod view;
pub use read_context::{EntryRead, EntryReader};
mod claim;
pub use claim::{EntryClaim, EntryClaimRelation, MAX_ENTRY_CLAIM_BYTES};
mod provenance;
pub use provenance::EntryOrigin;

const PUBLIC_ROSTER_NAMES: [&str; 2] = crate::entry_class::PUBLIC_ROSTER;

/// Public release-roster bytes observed together in the authenticated, locally applied vault.
/// The caller must verify the release master signature and apply its roster authority policy.
/// It reads no file under the home.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicRosterPair {
    /// Opaque canonical roster bytes; release-master verification remains the caller's job.
    pub roster: Vec<u8>,
    /// The detached signature supplied by the same authenticated snapshot.
    pub signature: Vec<u8>,
}

impl std::fmt::Debug for PublicRosterPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublicRosterPair").finish_non_exhaustive()
    }
}

/// No configured source is distinct from a recipient snapshot lacking the canonical pair.
#[derive(Debug)]
pub enum PublicRosterRead {
    /// This account has no configured vault source.
    Unconfigured,
    /// Neither canonical Everyone entry is available in this authenticated recipient snapshot.
    Absent,
    /// Both public entries are available, without release-authority admission.
    Pair(PublicRosterPair),
}

/// How long a read of the public roster waits while the vault's own writer (a sync or an
/// apply in the running aterm) holds the state lock, before it calls the source busy.
const PUBLIC_ROSTER_WAIT: Duration = Duration::from_secs(60);

/// Read only the two canonical Everyone public roster entries under one shared state guard.
/// Materialized files and their ownership do not participate. Absence is returned only after
/// authenticating the current applied recipient snapshot, not as a claim about plaintext
/// available to other recipients. A partial pair is an error. This fetches no
/// source, changes no state and grants no signing authority or lease across later effects.
/// In particular, this does not filter out a newer expired or revoking release roster.
/// A writer holding the vault state is waited for, with a minute hang detector.
/// # Errors
/// Configured state is busy past that wait, pending, denied, untrusted or unreadable; the pair
/// has a host audience or only one entry. Existing vault-entry size bounds apply; the caller
/// must refuse malformed roster or signature bytes through its existing release admission
/// policy.
pub fn current_public_roster(paths: &Paths) -> crate::Result<PublicRosterRead> {
    current_public_roster_after(paths, Duration::ZERO)
}

fn current_public_roster_after(
    paths: &Paths,
    elapsed: Duration,
) -> crate::Result<PublicRosterRead> {
    current_public_roster_until(
        paths,
        Instant::now() + PUBLIC_ROSTER_WAIT.saturating_sub(elapsed),
    )
}

/// [`current_public_roster`], waiting for a writer holding the vault state only until `deadline`.
pub(crate) fn current_public_roster_until(
    paths: &Paths,
    deadline: Instant,
) -> crate::Result<PublicRosterRead> {
    let Some(expected) = config(paths)? else {
        return Ok(PublicRosterRead::Unconfigured);
    };
    if expected.signer.is_none() {
        return Err(Error::new(
            "public roster source has no authenticated signer",
        ));
    }
    let observation = StateLock::observe_until(paths, deadline)?
        .ok_or_else(|| Error::new("public roster source is busy or unavailable"))?;
    let mut versions = [None, None];
    let mut values = Vec::with_capacity(2);
    for (index, name) in PUBLIC_ROSTER_NAMES.into_iter().enumerate() {
        values.push(
            read_entry_with_version_held(
                paths,
                &expected,
                name,
                Reading::PublicRoster,
                &mut versions[index],
                &observation,
            )?
            .map(|(entry, _)| entry.value),
        );
    }
    let [Some(a), Some(b)] = &versions else {
        return Err(Error::new(
            "public roster source is not currently applied and authenticated",
        ));
    };
    if a.vault != b.vault {
        return Err(Error::new(
            "public roster halves belong to different source snapshots",
        ));
    }
    let mut values = values.into_iter();
    match (values.next().flatten(), values.next().flatten()) {
        (None, None) => Ok(PublicRosterRead::Absent),
        (Some(roster), Some(signature)) => Ok(PublicRosterRead::Pair(PublicRosterPair {
            roster: roster.as_slice().to_vec(),
            signature: signature.as_slice().to_vec(),
        })),
        _ => Err(Error::new("canonical public roster pair is incomplete")),
    }
}

#[cfg(test)]
std::thread_local! {
    static MAILBOX_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Authenticated local vault version for a named entry, even when that entry
/// is absent or its anchor set does not resolve. Carries no secret commitment.
#[derive(Clone)]
pub struct EntryVersion {
    name: String,
    vault: VaultVersion,
}

/// Authenticated recipient view, independent of whether a named entry exists.
#[derive(Clone, PartialEq)]
struct VaultVersion {
    config: Config,
    recipient: Recipient,
    seq: u64,
    envelope: [u8; 32],
    log_ids: Option<std::sync::Arc<std::collections::BTreeSet<[u8; 32]>>>,
    mailbox_stamp: [u8; 32],
}

impl VaultVersion {
    fn named(&self, name: &str) -> EntryVersion {
        EntryVersion {
            name: name.into(),
            vault: self.clone(),
        }
    }
}

impl std::fmt::Debug for EntryVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntryVersion").finish_non_exhaustive()
    }
}

impl EntryVersion {
    /// A public-version binding for the fixed registered mailbox source. This
    /// includes no plaintext entry fingerprint and cannot identify a password.
    #[must_use]
    pub fn mailbox_source_stamp(&self) -> Option<[u8; 32]> {
        (self.name == crate::mailbox_component::SOURCE_ENTRY).then_some(self.vault.mailbox_stamp)
    }

    /// The same authenticated source and entry at a nondecreasing version.
    /// A different signed envelope at the same sequence is a fork, not progress.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "Same",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "Advance",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "Rollback",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "Fork",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "LogExtend",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "LogDrop",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "LogSwap",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbEntryVersion",
            action = "Context",
            project = "aterm_secrets::client::entry::tests::version_transition"
        )
    )]
    pub fn not_before(&self, floor: &Self) -> bool {
        self.vault.config == floor.vault.config
            && self.name == floor.name
            && self.vault.recipient == floor.vault.recipient
            && (self.vault.seq > floor.vault.seq
                || (self.vault.seq == floor.vault.seq
                    && self.vault.envelope == floor.vault.envelope))
            && match (&self.vault.log_ids, &floor.vault.log_ids) {
                (Some(current), Some(previous)) => previous.is_subset(current),
                (None, Some(_)) => false,
                _ => true,
            }
    }
}

/// A verified vault version and optional verified entry. Absence
/// does not authorize use, but its version can retain an observed revocation.
pub struct EntryObservation {
    pub version: EntryVersion,
    pub entry: Option<VerifiedEntry>,
}

/// One exact authenticated entry, including its audience and put-if-absent
/// policy. The private commitment is not a public identifier or a generation
/// counter. It must not be exported: weak secrets may be guessed from hashes.
/// This is not a revocation ledger or proof of uninterrupted membership.
#[derive(Clone)]
pub struct EntrySnapshot {
    config: Config,
    name: String,
    fingerprint: SecretBytes,
    recipient: Recipient,
    seq: u64,
    envelope: [u8; 32],
    envelope_context: Option<super::version::EnvelopeVersion>,
    log_ids: Option<std::sync::Arc<std::collections::BTreeSet<[u8; 32]>>>,
    mailbox_stamp: [u8; 32],
    source: provenance::Source,
}

impl std::fmt::Debug for EntrySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntrySnapshot").finish_non_exhaustive()
    }
}

/// Owned bytes of an entry, read in place from the verified, locally applied
/// encrypted vault. This does not prove the provider accepts them.
pub struct VerifiedEntry {
    value: SecretBytes,
    snapshot: EntrySnapshot,
    unanchored_default: bool,
}

impl VerifiedEntry {
    /// Whether the authenticated entry only fills an unanchored local gap. A local edit of
    /// such a configuration becomes the machine's own on the next ordinary vault sync;
    /// an always-managed entry or an anchor does not grant that setup policy.
    #[must_use]
    pub const fn unanchored_default(&self) -> bool {
        self.unanchored_default
    }

    /// The authenticated bytes, borrowed without changing the observation.
    #[must_use]
    pub const fn value(&self) -> &SecretBytes {
        &self.value
    }

    /// Evidence to check the same entry on a subsequent local read.
    #[must_use]
    pub const fn snapshot(&self) -> &EntrySnapshot {
        &self.snapshot
    }

    /// Consume the observation and take its bytes and subsequent-read evidence.
    #[must_use]
    pub fn into_parts(self) -> (SecretBytes, EntrySnapshot) {
        (self.value, self.snapshot)
    }
}

impl std::fmt::Debug for VerifiedEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedEntry").finish_non_exhaustive()
    }
}

impl EntrySnapshot {
    pub fn version(&self) -> EntryVersion {
        EntryVersion {
            name: self.name.clone(),
            vault: VaultVersion {
                config: self.config.clone(),
                recipient: self.recipient,
                seq: self.seq,
                envelope: self.envelope,
                log_ids: self.log_ids.clone(),
                mailbox_stamp: self.mailbox_stamp,
            },
        }
    }

    /// Same exact entry value and policy, without rolling back its vault version.
    pub fn continues(&self, floor: &Self) -> bool {
        self.version().not_before(&floor.version())
            && aterm_digest::ct_eq(self.fingerprint.as_slice(), floor.fingerprint.as_slice())
    }

    /// Read again only while this exact entry and configured trust context still
    /// hold. An unrelated vault update may pass; a changed value, audience,
    /// put-if-absent policy or source may not. Rejects rollback below the
    /// observed sequence and a different envelope at the same sequence. This
    /// does not establish global freshness. The lock is released on return;
    /// this is an observation, not a lease across subsequent effects.
    /// Retain the returned snapshot to advance the observation floor; this
    /// immutable snapshot keeps the floor it originally observed.
    /// # Errors
    /// Local state or the signed cache cannot be safely read.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVerifiedEntry",
            action = "Unrelated",
            project = "aterm_secrets::client::entry::tests::entry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVerifiedEntry",
            action = "Changed",
            project = "aterm_secrets::client::entry::tests::entry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVerifiedEntry",
            action = "Policy",
            project = "aterm_secrets::client::entry::tests::entry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVerifiedEntry",
            action = "Rollback",
            project = "aterm_secrets::client::entry::tests::entry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVerifiedEntry",
            action = "Recipient",
            project = "aterm_secrets::client::entry::tests::entry_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbVerifiedEntry",
            action = "Fork",
            project = "aterm_secrets::client::entry::tests::entry_transition"
        )
    )]
    pub fn read(&self, paths: &Paths) -> crate::Result<Option<VerifiedEntry>> {
        self.read_as(paths, Reading::Ordinary)
    }

    pub(super) fn read_publication(&self, paths: &Paths) -> crate::Result<Option<VerifiedEntry>> {
        self.read_as(paths, Reading::PublicationMaterial)
    }

    fn read_as(&self, paths: &Paths, reading: Reading) -> crate::Result<Option<VerifiedEntry>> {
        Ok(read_entry(paths, &self.config, &self.name, reading)?
            .map(|(entry, _)| entry)
            .filter(|entry| entry.snapshot.continues(self)))
    }
}

/// Read an ordinary entry under a signed vault's shared state lock. The current
/// configuration must match the independently observed `expected` configuration.
/// A busy, unsigned, revoked, partial or mismatched state, or an anchor chain that does not
/// resolve in the view, returns no entry. The value is the recipient's decrypted signed entry,
/// read in place: no file under the home is opened, so neither a file nor the manifest can
/// supply or block it. This creates no state, fetches no source and grants no application
/// authority.
/// # Errors
/// The name is invalid/reserved, local state is unreadable or decryption fails.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Valid",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "FileIgnored",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Cache",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Config",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Busy",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Revoked",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Partial",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Unsigned",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Reserved",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "ClaudeTokenOrdinary",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
pub fn read_verified_entry(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<VerifiedEntry>> {
    #[cfg(test)]
    if name == crate::mailbox_component::SOURCE_ENTRY {
        MAILBOX_READS.with(|reads| reads.set(reads.get() + 1));
    }
    let value = read_entry(paths, expected, name, Reading::Ordinary)?.map(|(entry, _)| entry);
    if let Some(value) = &value {
        let _ = super::mailbox_index::record_verified(paths, value);
    }
    Ok(value)
}

/// [`read_verified_entry`], waiting until `deadline` while a writer holds this Mac's vault state.
/// A store still busy then is an error, never an absent entry, so a caller does not take a sync
/// in progress for a missing value.
///
/// # Errors
/// As [`read_verified_entry`], or the vault state stayed busy until `deadline`.
pub fn read_verified_entry_until(
    paths: &Paths,
    expected: &Config,
    name: &str,
    deadline: Instant,
) -> crate::Result<Option<VerifiedEntry>> {
    validate_reading(name, Reading::Ordinary)?;
    if expected.signer.is_none() {
        return Ok(None);
    }
    match StateLock::observe_classified_until(paths, deadline)? {
        StateObservation::Held(observation) => Ok(read_entry_with_version_held(
            paths,
            expected,
            name,
            Reading::Ordinary,
            &mut None,
            &observation,
        )?
        .map(|(entry, _)| entry)),
        StateObservation::Absent => Ok(None),
        StateObservation::Busy => Err(Error::new(
            "this Mac's vault state stayed busy; run the command again",
        )),
    }
}

/// Read every entry of `names` from one verified view, in place, or none of them: an entry set
/// that only works together (the release identity, a Developer ID pair) is never read across two
/// versions (DESIGN-secrets §4, "Read together"). `None` where any name is absent, an anchored
/// member's chain does not resolve in that view, or the reads saw different versions. A writer
/// holding this Mac's vault state is waited for, with a minute hang detector.
///
/// # Errors
/// A name is invalid or reserved, local state cannot be read, or the vault state stayed busy
/// past that wait: a sync in progress is never taken for an absent set.
pub fn read_verified_set(
    paths: &Paths,
    expected: &Config,
    names: &[&str],
) -> crate::Result<Option<Vec<VerifiedEntry>>> {
    for name in names {
        validate_reading(name, Reading::Ordinary)?;
    }
    if expected.signer.is_none() {
        return Ok(None);
    }
    let observation =
        match StateLock::observe_classified_until(paths, Instant::now() + PUBLIC_ROSTER_WAIT)? {
            StateObservation::Held(observation) => observation,
            StateObservation::Absent => return Ok(None),
            StateObservation::Busy => {
                return Err(Error::new(
                    "this Mac's vault state stayed busy; run the command again",
                ));
            }
        };
    let mut first: Option<EntryVersion> = None;
    let mut read = Vec::with_capacity(names.len());
    for name in names {
        let mut version = None;
        let Some((entry, _)) = read_entry_with_version_held(
            paths,
            expected,
            name,
            Reading::Ordinary,
            &mut version,
            &observation,
        )?
        else {
            return Ok(None);
        };
        let Some(version) = version else {
            return Ok(None);
        };
        if let Some(first) = &first
            && first.vault != version.vault
        {
            return Ok(None);
        }
        first.get_or_insert(version);
        read.push(entry);
    }
    Ok(Some(read))
}

/// Read the authenticated vault version even if the named entry is absent or
/// its anchor set does not resolve. Untrusted/unavailable vault state has
/// no version. Callers must retain the version before interpreting `entry`.
/// # Errors
/// The local source or entry cannot be safely read.
pub fn observe_verified_entry(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<EntryObservation>> {
    let mut version = None;
    let entry = read_entry_with_version(paths, expected, name, Reading::Ordinary, &mut version)?;
    Ok(version.map(|version| EntryObservation {
        version,
        entry: entry.map(|(entry, _)| entry),
    }))
}

/// THE NAMES THIS MAC'S VERIFIED VIEW HOLDS, value-free: every entry of the authenticated
/// recipient view an ordinary read may return (no control record and no Claude Code token, which
/// only their typed reads see), sorted. The values are decrypted in memory to authenticate the
/// view and dropped; none is returned, written or logged. A writer holding the vault state is
/// waited for, with a minute hang detector. `None` where the view is not authenticated now (not a
/// member, a pending or denied view, a writer still holding the state past that wait), which says
/// nothing of what the vault holds.
///
/// # Errors
/// Local state cannot be read.
pub(crate) fn held_names(paths: &Paths, expected: &Config) -> crate::Result<Option<Vec<String>>> {
    if expected.signer.is_none() {
        return Ok(None);
    }
    let Some(observation) = StateLock::observe_until(paths, Instant::now() + PUBLIC_ROSTER_WAIT)?
    else {
        return Ok(None);
    };
    let Some(view) = view::read(paths, expected, view::Request::View, &observation)? else {
        return Ok(None);
    };
    Ok(Some(
        view.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .filter(|name| {
                validate_reading(name, Reading::Ordinary).is_ok()
                    && !crate::entry_class::claude_token(&name.to_ascii_lowercase())
            })
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    ))
}

/// Exact authenticated publication-token bytes, read in place. The reading is narrowed to the
/// two GitHub token names for the explicit release probe; its snapshot uses the same narrow
/// mode on revalidation.
pub(super) fn read_publication_material(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<VerifiedEntry>> {
    Ok(read_entry(paths, expected, name, Reading::PublicationMaterial)?.map(|(entry, _)| entry))
}

pub(crate) fn read_reserved_entry(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<VerifiedEntry>> {
    Ok(read_entry(paths, expected, name, Reading::Reserved)?.map(|(entry, _)| entry))
}

/// Observe a reserved entry or its authenticated absence. An unavailable snapshot or a present
/// entry whose conditional policy cannot be used as reserved data has no observation.
pub(crate) fn observe_reserved_entry(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<EntryObservation>> {
    let mut version = None;
    let entry = read_entry_with_version(paths, expected, name, Reading::Reserved, &mut version)?;
    Ok(version.map(|version| EntryObservation {
        version,
        entry: entry.map(|(entry, _)| entry),
    }))
}

pub(crate) fn read_reserved_entry_and_admin(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<(VerifiedEntry, bool)>> {
    read_entry(paths, expected, name, Reading::ReservedAndAdmin)
}

/// What one typed read of Claude Code token entries saw, under one observation of this Mac's
/// applied vault ([`read_claude_token_until`]): each value as the log decodes it (opened from its
/// inner seal where this Mac holds the sealing epoch, `membership_log::entries::inner`), the
/// trusted log the read opened, and this Mac's identity.
pub(crate) struct ClaudeTokenSnapshot {
    /// Each name's value, in the order asked, where the vault holds one.
    pub(crate) values: Vec<Option<SecretBytes>>,
    /// The trusted membership log the values came from.
    pub(crate) log: crate::membership_log::Log,
    /// This Mac's identity, which opened them.
    pub(crate) identity: Identity,
}

/// Read Claude Code token entries (`names`, each of the class,
/// [`crate::entry_class::claude_token`]) through [`Reading::ClaudeToken`], under one shared state
/// guard, waiting for a writer holding the vault state only until `deadline`. With no names it
/// authenticates the snapshot alone (by the reserved read of the swarm roster, which every
/// member may make) and hands out its log, for a listing. `None` where the state is busy past the
/// deadline, unconfigured, unsigned, revoked, failed or mid-apply (`State.pending`), where this
/// Mac reads no trusted log, or where two reads saw different snapshots: as every reader, it
/// serves nothing from a state it cannot authenticate. An absent entry is a snapshot with that
/// value `None`. It fetches no source and changes no state.
///
/// # Errors
/// A name is not of the class, or local state cannot be read.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "ClaudeToken",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
pub(crate) fn read_claude_token_until(
    paths: &Paths,
    names: &[&str],
    deadline: Instant,
) -> crate::Result<Option<ClaudeTokenSnapshot>> {
    if !names
        .iter()
        .all(|name| crate::entry_class::claude_token(name))
    {
        return Err(Error::new("not a Claude Code token entry name"));
    }
    let Some(expected) = config(paths)? else {
        return Ok(None);
    };
    if expected.signer.is_none() {
        return Ok(None);
    }
    let Some(observation) = StateLock::observe_until(paths, deadline)? else {
        return Ok(None);
    };
    let reads: Vec<(&str, Reading)> = if names.is_empty() {
        vec![(crate::SWARM_ROSTER_ENTRY, Reading::Reserved)]
    } else {
        names
            .iter()
            .map(|name| (*name, Reading::ClaudeToken))
            .collect()
    };
    let mut seen: Option<(EntryVersion, crate::membership_log::Log)> = None;
    let mut values = Vec::with_capacity(names.len());
    for (name, reading) in reads {
        let (mut version, mut log) = (None, None);
        let value = read_entry_held(
            paths,
            &expected,
            name,
            reading,
            &mut version,
            &observation,
            &mut log,
        )?
        .map(|(entry, _)| entry.value);
        let (Some(version), Some(log)) = (version, log) else {
            return Ok(None);
        };
        if let Some((first, _)) = &seen
            && (first.vault.config != version.vault.config
                || first.vault.recipient != version.vault.recipient
                || first.vault.seq != version.vault.seq
                || first.vault.envelope != version.vault.envelope
                || first.vault.log_ids != version.vault.log_ids)
        {
            return Ok(None);
        }
        seen.get_or_insert((version, log));
        if matches!(reading, Reading::ClaudeToken) {
            values.push(value);
        }
    }
    let (Some((_, log)), Some(identity)) = (seen, read_identity(paths)?) else {
        return Ok(None);
    };
    Ok(Some(ClaudeTokenSnapshot {
        values,
        log,
        identity,
    }))
}

#[derive(Clone, Copy)]
enum Reading {
    Ordinary,
    PublicationMaterial,
    Reserved,
    ReservedAndAdmin,
    PublicRoster,
    /// The typed read of a Claude Code token entry ([`read_claude_token_until`]): only a class
    /// name, read from the trusted log in place, never from a file.
    ClaudeToken,
}

fn holds_admin_locker(
    paths: &Paths,
    config: &Config,
    envelope: &envelope::Envelope,
    identity: &Identity,
) -> crate::Result<bool> {
    let Some(locker) = envelope.admin_locker() else {
        return Ok(false);
    };
    let plain = match age::decrypt(std::slice::from_ref(identity), locker.armored.as_bytes()) {
        Ok(plain) => plain,
        Err(age::AgeError::NoMatch) => return Ok(false),
        Err(error) => return Err(Error::new(format!("admin locker: {error}"))),
    };
    if let Ok(opened) = parse_admin(&plain)
        && super::admin_removed_in_log(paths, config, envelope, identity, &opened.doc)
    {
        return Ok(false);
    }
    Ok(true)
}

fn read_entry(
    paths: &Paths,
    expected: &Config,
    name: &str,
    reading: Reading,
) -> crate::Result<Option<(VerifiedEntry, bool)>> {
    read_entry_with_version(paths, expected, name, reading, &mut None)
}

fn read_entry_with_version(
    paths: &Paths,
    expected: &Config,
    name: &str,
    reading: Reading,
    version: &mut Option<EntryVersion>,
) -> crate::Result<Option<(VerifiedEntry, bool)>> {
    validate_reading(name, reading)?;
    if expected.signer.is_none() {
        return Ok(None);
    }
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    read_entry_with_version_held(paths, expected, name, reading, version, &_observation)
}

/// Whether `name` is set apart (DESIGN-secrets §4): an AI CLI's login file, a home path aterm does
/// not name, another spelling of a held or consumed name, or a name in the Claude Code token
/// namespace outside its grammar. Every read refuses it, whatever a vault an older build wrote
/// holds.
fn set_apart(name: &str) -> bool {
    matches!(
        crate::entry_class::of(name),
        crate::entry_class::EntryClass::SetApart(_)
    )
}

fn validate_reading(name: &str, reading: Reading) -> crate::Result<()> {
    let ordinary = matches!(reading, Reading::Ordinary | Reading::PublicationMaterial);
    let valid = if matches!(reading, Reading::PublicRoster) {
        PUBLIC_ROSTER_NAMES.contains(&name)
    } else if matches!(reading, Reading::ClaudeToken) {
        crate::entry_class::claude_token(name)
    } else if matches!(reading, Reading::PublicationMaterial) {
        name == crate::entry_class::DEV_GH_TOKEN || name == crate::entry_class::RELEASE_GH_TOKEN
    } else if ordinary {
        text::valid_entry_name(name) && !crate::reserved_name(name) && !set_apart(name)
    } else {
        text::valid_entry_name(name)
            && name.starts_with("aterm-")
            && (!crate::reserved_name(name)
                || matches!(
                    name,
                    "aterm-fleet-code"
                        | crate::SWARM_ROSTER_ENTRY
                        | crate::SWARM_POLICY_ENTRY
                        | crate::SWARM_LEASE_ENTRY
                ))
    };
    if !valid {
        return Err(Error::new("invalid or reserved secret entry name"));
    }
    Ok(())
}

fn read_entry_with_version_held(
    paths: &Paths,
    expected: &Config,
    name: &str,
    reading: Reading,
    version: &mut Option<EntryVersion>,
    observation: &StateLock,
) -> crate::Result<Option<(VerifiedEntry, bool)>> {
    read_entry_held(
        paths,
        expected,
        name,
        reading,
        version,
        observation,
        &mut None,
    )
}

/// Membership checks shared by current and historical log reads.
fn log_entry_reader_allowed(log: &crate::membership_log::Log, identity: &Identity) -> bool {
    !log.removes(&identity.recipient())
        && !super::log_entries::recipient_denied_by_roster_or_log(None, log, &identity.recipient())
        && !log.audience_left_out_recipient(
            &crate::membership_log::EpochAudience::Everyone,
            &identity.recipient(),
        )
        && super::log_entries::reads_log(log, identity)
}

/// The same roster denial applies to current reads and claimed historical reads.
fn selected_entry_reader_allowed(
    entries: &[Entry],
    log: Option<&crate::membership_log::Log>,
    config: &Config,
    reading_roster: bool,
    recipient: Recipient,
) -> bool {
    if !reading_roster
        && let Some(entry) = entries
            .iter()
            .find(|entry| entry.name == crate::SWARM_ROSTER_ENTRY)
        && let Ok(text) = std::str::from_utf8(entry.value.as_slice())
        && let Ok(mut roster) = crate::swarm_roster::SwarmRoster::from_toml(text)
    {
        roster.canonicalize();
        if let Some(log) = log {
            super::roster::overlay_log_membership(log, None, &mut roster);
            if super::log_entries::recipient_denied_by_roster_or_log(Some(&roster), log, &recipient)
            {
                return false;
            }
        }
        if let Some(member) = roster.member_by_recipient(&recipient.to_bech32()) {
            if steward::member_is_denied_in_roster(&roster, member) {
                return false;
            }
        } else if roster.is_label_denied(&config.host) {
            return false;
        }
    }
    true
}

/// [`read_entry_with_version_held`], also handing out in `trusted` the trusted membership log the
/// read opened, wherever it sets `version`.
fn read_entry_held(
    paths: &Paths,
    expected: &Config,
    name: &str,
    reading: Reading,
    version: &mut Option<EntryVersion>,
    _observation: &StateLock,
    trusted: &mut Option<crate::membership_log::Log>,
) -> crate::Result<Option<(VerifiedEntry, bool)>> {
    read_entry_held_observing(
        paths,
        expected,
        name,
        reading,
        version,
        (_observation, &mut false),
        trusted,
    )
}

/// Retain authenticated presence beside the entry read.
fn read_entry_held_observing(
    paths: &Paths,
    expected: &Config,
    name: &str,
    reading: Reading,
    version: &mut Option<EntryVersion>,
    (_observation, present): (&StateLock, &mut bool),
    trusted: &mut Option<crate::membership_log::Log>,
) -> crate::Result<Option<(VerifiedEntry, bool)>> {
    if matches!(reading, Reading::ClaudeToken) != crate::entry_class::claude_token(name) {
        return Err(Error::new("invalid or reserved secret entry name"));
    }
    let ordinary = matches!(reading, Reading::Ordinary | Reading::PublicationMaterial);
    let Some(view::View {
        entries,
        version: context,
        is_admin,
        trusted_log,
        selected_log,
        envelope_context,
    }) = view::read(
        paths,
        expected,
        view::Request::Entry { name, reading },
        _observation,
    )?
    else {
        return Ok(None);
    };
    let entry = entries.iter().find(|entry| entry.name == name);
    if matches!(reading, Reading::PublicRoster)
        && entry.is_some_and(|entry| entry.audience != Audience::Everyone)
    {
        return Err(Error::new(
            "canonical public roster entry has a host-specific audience",
        ));
    }
    if !ordinary
        && !matches!(reading, Reading::PublicRoster)
        && entry.is_some_and(|entry| entry.if_absent || entry.anchor.is_some())
    {
        return Ok(None);
    }
    *version = Some(context.named(name));
    *present = entry.is_some();
    let Some(entry) = entry else {
        *trusted = trusted_log;
        return Ok(None);
    };
    let source = provenance::Source::selected(selected_log, trusted_log.as_ref(), entry)?;
    *trusted = trusted_log;
    // Every reading takes the value in place from the verified view; no file under the home is
    // read (DESIGN-secrets §7). An anchored entry is read only where its whole anchor chain
    // resolves to put-if-absent entries of this same view, so a set is read from one version.
    if ordinary && entry.anchor.is_some() && !anchors_resolve(&entries, entry) {
        return Ok(None);
    }
    // Two names that fold alike are one name (DESIGN-secrets §4): neither spelling reads.
    if ordinary
        && crate::entry_class::folded_pairs(entries.iter().map(|entry| entry.name.as_str()))
            .contains_key(name)
    {
        return Ok(None);
    }
    let (value, admin) = (entry.value.clone(), !ordinary && is_admin);
    Ok(Some((
        VerifiedEntry {
            value,
            unanchored_default: entry.if_absent && entry.anchor.is_none(),
            snapshot: EntrySnapshot {
                mailbox_stamp: context.mailbox_stamp,
                config: context.config,
                name: name.into(),
                fingerprint: fingerprint(entry),
                recipient: context.recipient,
                seq: context.seq,
                envelope: context.envelope,
                envelope_context,
                log_ids: context.log_ids,
                source,
            },
        },
        admin,
    )))
}

pub(super) fn public_state_stamp(config: &Config, state: &State, recipient: Recipient) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm/mailbox-source-public-version/1\0");
    digest.update(source_identity(config));
    digest.update(config.host.as_bytes());
    digest.update([0]);
    digest.update(recipient.to_bech32().as_bytes());
    digest.update(state.seq.to_be_bytes());
    for value in [state.digest, state.records_digest] {
        digest.update([u8::from(value.is_some())]);
        if let Some(value) = value {
            digest.update(value);
        }
    }
    digest.finalize()
}

/// Whether `entry`'s anchor chain resolves within `entries`, one view: each anchor it names is
/// present and put-if-absent. A chain that comes back to a name already walked is a complete set
/// (the release ring's three files, a Developer ID pair, the public roster pair are published as
/// such cycles, DESIGN-secrets §4) and resolves; an anchor naming an absent or ordinary entry
/// does not.
fn anchors_resolve(entries: &[Entry], entry: &Entry) -> bool {
    let mut seen = BTreeSet::from([entry.name.as_str()]);
    let mut anchor = entry.anchor.as_deref();
    while let Some(name) = anchor {
        if !seen.insert(name) {
            return true;
        }
        #[cfg(test)]
        if HIDDEN_ANCHOR.with_borrow(|hidden| hidden.as_deref() == Some(name)) {
            return false;
        }
        let Some(entry) = entries.iter().find(|entry| entry.name == name) else {
            return false;
        };
        if !entry.if_absent {
            return false;
        }
        anchor = entry.anchor.as_deref();
    }
    true
}

#[cfg(test)]
std::thread_local! {
    /// An anchor name the walk reads as absent from the view. The document check refuses every
    /// envelope whose anchor some host does not receive, so no envelope fixture can hold the
    /// unresolved chain a reader must treat as unavailable; this seam stands in for it.
    pub(super) static HIDDEN_ANCHOR: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn field(hash: &mut aterm_digest::Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

pub(super) fn fingerprint(entry: &Entry) -> SecretBytes {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/private-entry-snapshot/1\0");
    field(&mut hash, entry.name.as_bytes());
    match &entry.audience {
        Audience::Everyone => hash.update([0]),
        Audience::Hosts(hosts) => {
            hash.update([1]);
            hash.update((hosts.len() as u64).to_be_bytes());
            for host in hosts {
                field(&mut hash, host.as_bytes());
            }
        }
    }
    hash.update([u8::from(entry.if_absent), u8::from(entry.anchor.is_some())]);
    if let Some(anchor) = &entry.anchor {
        field(&mut hash, anchor.as_bytes());
    }
    field(&mut hash, entry.value.as_slice());
    SecretBytes::new(hash.finalize().to_vec())
}

/// Observe bytes matching a recipient-sealed binding in a root-signed account
/// record. This still grants no capability: callers must independently check
/// current Commons membership and the complete account decision history.
/// # Errors
/// A verified local read or the recipient-sealed binding cannot be decoded.
pub fn read_account_bound_entry(
    paths: &Paths,
    expected: &Config,
    record: &crate::commons::accounts::Record,
) -> crate::Result<Option<VerifiedEntry>> {
    let crate::commons::accounts::Action::Prepare(preparation) = &record.request().action else {
        return Ok(None);
    };
    let Some(reference) = &preparation.secret else {
        return Ok(None);
    };
    if expected.name != reference.vault
        || expected.signer.is_none()
        || (expected.signer != Some(reference.signer)
            && !expected.followed.contains(&reference.signer))
    {
        return Ok(None);
    }
    let Some(entry) = read_verified_entry(paths, expected, &reference.entry)? else {
        return Ok(None);
    };
    if entry.snapshot.recipient != preparation.device.recipient() {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    if identity.recipient() != entry.snapshot.recipient
        || !record.matches_entry(&identity, entry.snapshot.fingerprint.as_slice())?
    {
        return Ok(None);
    }
    Ok(Some(entry))
}

#[cfg(test)]
mod tests;
