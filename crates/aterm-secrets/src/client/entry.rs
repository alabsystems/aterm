// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated, locally applied entries. An observation of bytes is never a
//! grant to use them as a provider login or to spend an account's budget.

use super::*;

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
}

impl VerifiedEntry {
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

fn signing_material_name(name: &str) -> bool {
    name.strip_prefix(super::release::APPLE_PREFIX)
        .and_then(|leaf| {
            leaf.strip_suffix(".cer")
                .or_else(|| leaf.strip_suffix(".key"))
        })
        .is_some_and(|stem| !stem.is_empty() && !stem.contains(['/', '\\']))
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

#[derive(Clone, Copy)]
enum Reading {
    Materialized,
    SigningMaterial,
    PublicationMaterial,
    Reserved,
    ReservedAndAdmin,
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
    let valid = if matches!(reading, Reading::SigningMaterial) {
        text::valid_entry_name(name) && signing_material_name(name)
    } else if matches!(reading, Reading::PublicationMaterial) {
        matches!(name, "gh_access_token" | "gh_access_token_alabsystems")
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
        if !super::log_entries::applied_log_available(&state, seeded.as_ref()) {
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
    if !materialized && entry.is_some_and(|entry| entry.if_absent || entry.anchor.is_some()) {
        return Ok(None);
    }
    let log_ids = trusted_log
        .as_ref()
        .map(|log| std::sync::Arc::new(log.records().map(|record| *record.id()).collect()));
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
    let mut bytes = Vec::new();
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
