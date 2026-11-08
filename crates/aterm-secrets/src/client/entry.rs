// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated, locally applied entries. An observation of bytes is never a
//! grant to use them as a provider login or to spend an account's budget, with one exception:
//! the typed read of a Claude Code token ([`read_claude_token_until`], `crate::claude_token`),
//! which the owner sanctioned on 2026-11-05 as Claude Code's headless login
//! (`docs/DESIGN-claude-setup-token-2026-11-05.md` §4.2). Only that read returns a token class
//! value; every other reading refuses the class.

use super::*;

const PUBLIC_ROSTER_NAMES: [&str; 2] = crate::entry_class::PUBLIC_ROSTER;

/// Public release-roster bytes observed together in the authenticated, locally applied vault.
/// The caller must verify the release master signature and apply its roster authority policy.
/// This observation does not establish ownership of any materialized file.
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
    if a.config != b.config
        || a.recipient != b.recipient
        || a.seq != b.seq
        || a.envelope != b.envelope
        || a.log_ids != b.log_ids
        || a.mailbox_stamp != b.mailbox_stamp
    {
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
/// is absent or not safely materialized. Carries no secret commitment.
#[derive(Clone)]
pub struct EntryVersion {
    config: Config,
    name: String,
    recipient: Recipient,
    seq: u64,
    envelope: [u8; 32],
    log_ids: Option<std::sync::Arc<std::collections::BTreeSet<[u8; 32]>>>,
    mailbox_stamp: [u8; 32],
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
        (self.name == crate::mailbox_component::SOURCE_ENTRY).then_some(self.mailbox_stamp)
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
        self.config == floor.config
            && self.name == floor.name
            && self.recipient == floor.recipient
            && (self.seq > floor.seq || (self.seq == floor.seq && self.envelope == floor.envelope))
            && match (&self.log_ids, &floor.log_ids) {
                (Some(current), Some(previous)) => previous.is_subset(current),
                (None, Some(_)) => false,
                _ => true,
            }
    }
}

/// A verified vault version and optional matching materialized entry. Absence
/// does not authorize use, but its version can retain an observed revocation.
pub struct EntryObservation {
    pub version: EntryVersion,
    pub entry: Option<VerifiedEntry>,
}

/// One exact authenticated entry, including its audience and materialization
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
    log_ids: Option<std::sync::Arc<std::collections::BTreeSet<[u8; 32]>>>,
    mailbox_stamp: [u8; 32],
}

impl std::fmt::Debug for EntrySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EntrySnapshot").finish_non_exhaustive()
    }
}

/// Owned materialized bytes that matched an entry in the verified, locally
/// applied encrypted vault. This does not prove the provider accepts them.
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
            config: self.config.clone(),
            name: self.name.clone(),
            recipient: self.recipient,
            seq: self.seq,
            envelope: self.envelope,
            log_ids: self.log_ids.clone(),
            mailbox_stamp: self.mailbox_stamp,
        }
    }

    /// Same exact entry value and policy, without rolling back its vault version.
    pub fn continues(&self, floor: &Self) -> bool {
        self.version().not_before(&floor.version())
            && aterm_digest::ct_eq(self.fingerprint.as_slice(), floor.fingerprint.as_slice())
    }

    /// Read again only while this exact entry and configured trust context still
    /// hold. An unrelated vault update may pass; a changed value, audience,
    /// materialization policy or source may not. Rejects rollback below the
    /// observed sequence and a different envelope at the same sequence. This
    /// does not establish global freshness. The lock is released on return;
    /// this is an observation, not a lease across subsequent effects.
    /// Retain the returned snapshot to advance the observation floor; this
    /// immutable snapshot keeps the floor it originally observed.
    /// # Errors
    /// Local state, the signed cache or the entry file cannot be safely read.
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
        self.read_as(paths, Reading::Materialized)
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
/// A busy, unsigned, revoked, partial or mismatched state returns no entry. Neither
/// a manifest name nor a self-consistent edited file/manifest establishes trust:
/// the recipient's decrypted signed entry must match the actual file bytes.
/// This creates no state, fetches no source and grants no application authority.
/// # Errors
/// The name is invalid/reserved, local state is unreadable, decryption fails or
/// the file cannot be safely opened as a bounded, private regular file.
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
        action = "LocalEdit",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "ForgedManifest",
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
        action = "ParentLink",
        project = "aterm_secrets::client::entry::tests::entry_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbVerifiedEntry",
        action = "Missing",
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
        action = "ClaudeTokenMaterialized",
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
    let value = read_entry(paths, expected, name, Reading::Materialized)?.map(|(entry, _)| entry);
    if let Some(value) = &value {
        let _ = super::mailbox_index::record_verified(paths, value);
    }
    Ok(value)
}

/// Read the authenticated vault version even if the named entry is absent or
/// its materialized file does not match. Untrusted/unavailable vault state has
/// no version. Callers must retain the version before interpreting `entry`.
/// # Errors
/// The local source or entry cannot be safely read.
pub fn observe_verified_entry(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<EntryObservation>> {
    let mut version = None;
    let entry =
        read_entry_with_version(paths, expected, name, Reading::Materialized, &mut version)?;
    Ok(version.map(|version| EntryObservation {
        version,
        entry: entry.map(|(entry, _)| entry),
    }))
}

/// Read existing signing bytes only when they equal the currently authorized vault entry.
/// Unlike the general reader, an untracked file may supply these bytes without transferring
/// its ownership to the vault. Any anchors must still be current vault-owned files.
/// No file or manifest is changed.
/// # Errors
/// The name is outside the signing directory, or the existing authenticated entry or private
/// file cannot be safely read.
pub(super) fn read_signing_material(
    paths: &Paths,
    expected: &Config,
    name: &str,
) -> crate::Result<Option<SecretBytes>> {
    Ok(read_entry(paths, expected, name, Reading::SigningMaterial)?.map(|(entry, _)| entry.value))
}

/// Exact authenticated publication-token bytes, without claiming an existing file.
/// This exception is private to the explicit release probe; ordinary entry reads retain
/// their manifest requirement. Its snapshot must use the same narrow mode on revalidation.
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
            && (first.config != version.config
                || first.recipient != version.recipient
                || first.seq != version.seq
                || first.envelope != version.envelope
                || first.log_ids != version.log_ids)
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
    Materialized,
    SigningMaterial,
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
    let materialized = matches!(
        reading,
        Reading::Materialized | Reading::SigningMaterial | Reading::PublicationMaterial
    );
    let valid = if matches!(reading, Reading::PublicRoster) {
        PUBLIC_ROSTER_NAMES.contains(&name)
    } else if matches!(reading, Reading::ClaudeToken) {
        crate::entry_class::claude_token(name)
    } else if matches!(reading, Reading::SigningMaterial) {
        text::valid_entry_name(name) && crate::entry_class::signing_material(name)
    } else if matches!(reading, Reading::PublicationMaterial) {
        name == "gh_access_token" || name == crate::entry_class::RELEASE_GH_TOKEN
    } else if materialized {
        text::valid_entry_name(name) && !crate::reserved_name(name)
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
    if expected.signer.is_none() {
        return Ok(None);
    }
    let Some(_observation) = StateLock::observe(paths)? else {
        return Ok(None);
    };
    read_entry_with_version_held(paths, expected, name, reading, version, &_observation)
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
    if matches!(reading, Reading::ClaudeToken) != crate::entry_class::claude_token(name) {
        return Err(Error::new("invalid or reserved secret entry name"));
    }
    let materialized = matches!(
        reading,
        Reading::Materialized | Reading::SigningMaterial | Reading::PublicationMaterial
    );
    let Some(config) = config(paths)? else {
        return Ok(None);
    };
    if config != *expected || !pin_admitted(&config, None) {
        return Ok(None);
    }
    let state = load_state(paths)?;
    if state.membership != Stored::In || state.failed || !state.pending.is_empty() {
        return Ok(None);
    }
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    // Selection happens after the shared register and anchor resolution. Reserved readers
    // also retain the roster used below to establish this recipient's policy.
    let requested = [name, crate::SWARM_ROSTER_ENTRY];
    let (entries, envelope_digest, is_admin, trusted_log) = if state.digest.is_none()
        && fsx::kind(&paths.cache_file())? == fsx::Kind::Missing
    {
        if state.source_identity != Some(source_identity(&config)) {
            return Ok(None);
        }
        let Some(configured) = super::log_entries::trusted_configured_log(paths, &config)? else {
            return Ok(None);
        };
        if configured.log.removes(&identity.recipient())
            || super::log_entries::recipient_denied_by_roster_or_log(
                None,
                &configured.log,
                &identity.recipient(),
            )
            || configured.log.audience_left_out_recipient(
                &crate::membership_log::EpochAudience::Everyone,
                &identity.recipient(),
            )
            || !super::log_entries::reads_log(&configured.log, &identity)
        {
            return Ok(None);
        }
        let entries = if materialized {
            configured.log.open_entries(&identity)?
        } else {
            configured.log.open_entries_named(&identity, &requested)?
        };
        let Some(entries) = entries else {
            return Ok(None);
        };
        let digest = *configured.log.genesis().id();
        (entries, digest, false, Some(configured.log))
    } else {
        if !source_bound(&config, &state) {
            return Ok(None);
        }
        let Some(cached) = read_held_copy(paths, &config, &state)? else {
            return Ok(None);
        };
        let envelope = envelope::parse(&cached)?;
        let seeded = super::log_entries::trusted_seeded_log(paths, &config, &envelope)?;
        if !super::log_entries::applied_log_available(
            &state,
            seeded.as_ref().map(|(_, digest)| *digest),
        ) {
            return Ok(None);
        }
        let (entries, _) = super::log_entries::open_entries_with_log(
            seeded.as_ref().map(|(log, digest)| (log, *digest)),
            &envelope,
            &identity,
            if materialized { None } else { Some(&requested) },
        )?;
        let Some(entries) = entries else {
            return Ok(None);
        };
        let trusted_log = seeded.map(|(log, _)| log);
        // Admin standing reads the base roster. A reserved read of that roster must not ask
        // for standing again; only the caller returning admin status needs that proof.
        let admin = matches!(reading, Reading::ReservedAndAdmin)
            && holds_admin_locker(paths, &config, &envelope, &identity)?;
        (entries, envelope.digest(), admin, trusted_log)
    };
    if name != crate::SWARM_ROSTER_ENTRY
        && let Some(roster_entry) = entries.iter().find(|e| e.name == crate::SWARM_ROSTER_ENTRY)
        && let Ok(text) = std::str::from_utf8(roster_entry.value.as_slice())
        && let Ok(mut r) = crate::swarm_roster::SwarmRoster::from_toml(text)
    {
        r.canonicalize();
        if let Some(log) = &trusted_log {
            super::roster::overlay_log_membership(log, None, &mut r);
            if super::log_entries::recipient_denied_by_roster_or_log(
                Some(&r),
                log,
                &identity.recipient(),
            ) {
                return Ok(None);
            }
        }
        let my_rec = identity.recipient().to_bech32();
        if let Some(m) = r.member_by_recipient(&my_rec) {
            if steward::member_is_denied_in_roster(&r, m) {
                return Ok(None);
            }
        } else if r.is_label_denied(&config.host) {
            return Ok(None);
        }
    }
    let entry = entries.iter().find(|entry| entry.name == name);
    if matches!(reading, Reading::PublicRoster)
        && entry.is_some_and(|entry| entry.audience != Audience::Everyone)
    {
        return Err(Error::new(
            "canonical public roster entry has a host-specific audience",
        ));
    }
    if !materialized
        && !matches!(reading, Reading::PublicRoster)
        && entry.is_some_and(|entry| entry.if_absent || entry.anchor.is_some())
    {
        return Ok(None);
    }
    let log_ids = trusted_log
        .as_ref()
        .map(|log| std::sync::Arc::new(log.records().map(|record| *record.id()).collect()));
    *trusted = trusted_log;
    *version = Some(EntryVersion {
        config: config.clone(),
        name: name.into(),
        recipient: identity.recipient(),
        seq: state.seq,
        envelope: envelope_digest,
        log_ids: log_ids.clone(),
        mailbox_stamp: public_state_stamp(&config, &state, identity.recipient()),
    });
    let Some(entry) = entry else {
        return Ok(None);
    };
    let (value, admin) = if materialized {
        let untracked = matches!(
            reading,
            Reading::SigningMaterial | Reading::PublicationMaterial
        );
        let strict = matches!(reading, Reading::PublicationMaterial);
        if untracked
            && !state.manifest.files.contains_key(name)
            && !material_anchors_current(paths, &state, &entries, entry, strict)?
        {
            return Ok(None);
        }
        let Some(value) = materialized_value(paths, &state, entry, !untracked, strict)? else {
            return Ok(None);
        };
        (value, false)
    } else {
        (entry.value.clone(), is_admin)
    };
    Ok(Some((
        VerifiedEntry {
            value,
            unanchored_default: entry.if_absent && entry.anchor.is_none(),
            snapshot: EntrySnapshot {
                mailbox_stamp: public_state_stamp(&config, &state, identity.recipient()),
                config,
                name: name.into(),
                fingerprint: fingerprint(entry),
                recipient: identity.recipient(),
                seq: state.seq,
                envelope: envelope_digest,
                log_ids,
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

fn material_anchors_current(
    paths: &Paths,
    state: &State,
    entries: &[Entry],
    entry: &Entry,
    strict: bool,
) -> crate::Result<bool> {
    let mut seen = BTreeSet::new();
    let mut anchor = entry.anchor.as_deref();
    while let Some(name) = anchor {
        if !seen.insert(name) {
            return Ok(false);
        }
        let Some(entry) = entries.iter().find(|entry| entry.name == name) else {
            return Ok(false);
        };
        if !entry.if_absent || materialized_value(paths, state, entry, true, strict)?.is_none() {
            return Ok(false);
        }
        anchor = entry.anchor.as_deref();
    }
    Ok(true)
}

fn materialized_value(
    paths: &Paths,
    state: &State,
    entry: &Entry,
    require_tracked: bool,
    strict: bool,
) -> crate::Result<Option<SecretBytes>> {
    let digest = materialize::digest_hex(entry.value.as_slice());
    let tracked = state.manifest.files.get(&entry.name);
    if tracked.is_some_and(|held| held != &digest) || (require_tracked && tracked.is_none()) {
        return Ok(None);
    }
    let path = materialize::target_path(&paths.home, &entry.name)?;
    // Refuse observed parent links too. A concurrent path replacement cannot substitute
    // returned bytes: the opened file is compared with the signed plaintext below.
    // This does not claim a path lease against other writers.
    let mut parent = path.parent();
    loop {
        let Some(directory) = parent.filter(|directory| directory.starts_with(&paths.home)) else {
            return Ok(None);
        };
        if fsx::kind(directory)? != fsx::Kind::Dir {
            return Ok(None);
        }
        if directory == paths.home {
            break;
        }
        parent = directory.parent();
    }
    let Some(value) = (if strict {
        read_single_private(&path)?
    } else {
        fsx::read_private(&path, text::MAX_VALUE)?
    }) else {
        return Ok(None);
    };
    if !aterm_digest::ct_eq(value.as_slice(), entry.value.as_slice()) {
        return Ok(None);
    }
    Ok(Some(value))
}

fn read_single_private(path: &Path) -> crate::Result<Option<SecretBytes>> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let dir = path
        .parent()
        .ok_or_else(|| Error::new("publication material has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::new("publication material has an invalid name"))?;
    let (file, metadata) = match aterm_uds::private_file::open_private(dir, name) {
        Ok(opened) => opened,
        Err(aterm_uds::private_file::PrivateFileError::Missing) => return Ok(None),
        Err(_) => {
            return Err(Error::new(
                "publication material is not a private owned file",
            ));
        }
    };
    if metadata.nlink() != 1 || metadata.len() > text::MAX_VALUE as u64 {
        return Err(Error::new("publication material has unsafe links or size"));
    }
    let mut bytes = crate::fsx::read_buffer(metadata.len(), text::MAX_VALUE);
    crate::secret::read_to_end_wiping_on_error(file.take(text::MAX_VALUE as u64 + 1), &mut bytes)?;
    let value = SecretBytes::new(bytes);
    if value.as_slice().len() > text::MAX_VALUE {
        return Err(Error::new("publication material exceeds its bound"));
    }
    Ok(Some(value))
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
