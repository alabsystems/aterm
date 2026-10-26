// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated, locally applied entries. An observation of bytes is never a
//! grant to use them as a provider login or to spend an account's budget.

use super::*;

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
        Ok(
            read_verified_entry(paths, &self.config, &self.name)?.filter(|entry| {
                entry.snapshot.recipient == self.recipient
                    && entry.snapshot.seq >= self.seq
                    && (entry.snapshot.seq > self.seq || entry.snapshot.envelope == self.envelope)
                    && aterm_digest::ct_eq(
                        entry.snapshot.fingerprint.as_slice(),
                        self.fingerprint.as_slice(),
                    )
            }),
        )
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
    if !text::valid_entry_name(name) || crate::commons::reserved_name(name) {
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
    if state.membership != Stored::In
        || state.failed
        || !state.pending.is_empty()
        || !source_bound(&config, &state)
    {
        return Ok(None);
    }
    let Some(cached) = held_copy(paths, &config, &state) else {
        return Ok(None);
    };
    let envelope = envelope::parse(&cached)?;
    let Some(identity) = read_identity(paths)? else {
        return Ok(None);
    };
    let Some(entries) = open_lockers(&envelope, &identity)? else {
        return Ok(None);
    };
    let Some(entry) = entries.into_iter().find(|entry| entry.name == name) else {
        return Ok(None);
    };
    if state.manifest.files.get(name) != Some(&materialize::digest_hex(entry.value.as_slice())) {
        return Ok(None);
    }
    let path = materialize::target_path(&paths.home, name)?;
    // Refuse observed parent links too. A concurrent path replacement cannot
    // substitute returned bytes: the opened file is compared with the signed
    // plaintext below. This does not claim a path lease against other writers.
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
    let Some(value) = fsx::read_private(&path, text::MAX_VALUE)? else {
        return Ok(None);
    };
    if !aterm_digest::ct_eq(value.as_slice(), entry.value.as_slice()) {
        return Ok(None);
    }
    Ok(Some(VerifiedEntry {
        value,
        snapshot: EntrySnapshot {
            config,
            name: name.into(),
            fingerprint: fingerprint(&entry),
            recipient: identity.recipient(),
            seq: state.seq,
            envelope: envelope.digest(),
        },
    }))
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
    if expected.name != reference.vault || expected.signer != Some(reference.signer) {
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
