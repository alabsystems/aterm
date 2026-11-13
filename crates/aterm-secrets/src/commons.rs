// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Encrypted custody for the ALab Commons control record. This record is never a
//! materialized secret file. Its consumers receive an authenticated snapshot of
//! the locally applied vault, including the authority and exact version.

use crate::{SecretBytes, age::Recipient};

#[cfg(unix)]
pub mod accounts;
pub mod binding;
#[cfg(unix)]
pub mod bootstrap;
pub mod constraints;
#[cfg(unix)]
pub mod everyone_key;
#[cfg(unix)]
pub mod fold;
#[cfg(unix)]
pub mod hearing;
#[cfg(unix)]
pub mod history;
#[cfg(unix)]
pub mod journal;
pub mod membership;
pub mod node;
mod observation;
pub use observation::{Observation, Unavailable};
#[cfg(unix)]
pub mod roster;

/// Reserved entry in one explicitly named Commons-member audience.
pub const ENTRY: &str = "aterm-commons-control-v1";

/// Future control schema versions must not become plaintext on an older client.
pub const CONTROL_PREFIX: &str = "aterm-commons-control-";

/// Whether a name belongs to the reserved control-record namespace.
/// Include case aliases and the explicit home-path spelling on case-insensitive
/// filesystems. Only [`ENTRY`] is a valid control-record spelling.
pub fn reserved_name(name: &str) -> bool {
    let name = if let Some(path) = name.strip_prefix("~/") {
        let Some((directory, file)) = path.split_once('/') else {
            return false;
        };
        if !directory.eq_ignore_ascii_case(".secrets") {
            return false;
        }
        file
    } else {
        name
    };
    name.get(..CONTROL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(CONTROL_PREFIX))
}

/// A decrypted control record whose signed envelope matches the applied local
/// vault. Fields are immutable so a caller cannot relabel its authority/version.
/// This establishes custody, not the Commons payload schema, membership roles or
/// network policy: a consumer must validate those before granting access.
#[derive(Clone)]
pub struct Snapshot {
    #[cfg(unix)]
    source_config: Box<crate::client::Config>,
    vault: String,
    signer: [u8; 32],
    followed: Vec<[u8; 32]>,
    sequence: u64,
    digest: [u8; 32],
    recipient: Recipient,
    control: SecretBytes,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("vault", &self.vault)
            .field("sequence", &self.sequence)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl Snapshot {
    #[cfg(unix)]
    pub(crate) fn verified(
        config: crate::client::Config,
        envelope: &crate::envelope::Envelope,
        recipient: Recipient,
        control: SecretBytes,
    ) -> Self {
        Self {
            vault: config.name.clone(),
            signer: envelope.signer,
            followed: config.followed.clone(),
            source_config: Box::new(config),
            sequence: envelope.seq,
            digest: envelope.digest(),
            recipient,
            control,
        }
    }

    #[cfg(unix)]
    pub(crate) fn verified_from_log(
        config: crate::client::Config,
        signer: [u8; 32],
        log_digest: [u8; 32],
        recipient: Recipient,
        control: SecretBytes,
    ) -> Self {
        Self {
            vault: config.name.clone(),
            signer,
            followed: config.followed.clone(),
            source_config: Box::new(config),
            sequence: 1,
            digest: log_digest,
            recipient,
            control,
        }
    }

    /// The pinned vault name.
    pub fn vault(&self) -> &str {
        &self.vault
    }

    /// The signing authority that authenticated this snapshot.
    pub const fn signer(&self) -> &[u8; 32] {
        &self.signer
    }

    /// Previously pinned authorities in this vault's verified handoff lineage.
    pub fn followed(&self) -> &[[u8; 32]] {
        &self.followed
    }

    /// Exact locally applied vault sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Exact signed envelope digest, which distinguishes forks at one sequence.
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// The local age recipient that opened the control audience.
    pub const fn recipient(&self) -> Recipient {
        self.recipient
    }

    /// Control bytes for the Commons schema decoder. They are wiped with this
    /// snapshot and are deliberately absent from its debug representation.
    pub fn control(&self) -> &[u8] {
        self.control.as_slice()
    }

    #[cfg(unix)]
    /// Whether both authenticated observations name the exact same configured
    /// source, signing pin/lineage, applied version, recipient and control bytes.
    /// Equal envelope digests alone do not establish the same local source
    /// context. This compares observations; it grants no ongoing authorization.
    pub fn same_observation(&self, other: &Self) -> bool {
        self.source_config == other.source_config
            && self.vault == other.vault
            && self.signer == other.signer
            && self.followed == other.followed
            && self.sequence == other.sequence
            && self.digest == other.digest
            && self.recipient == other.recipient
            && self.control.as_slice() == other.control.as_slice()
    }
}

/// Read an authenticated control record offline, without creating an identity,
/// contacting the vault, or materializing keys. `None` means no eligible applied
/// record is available or a concurrent vault update holds the observation lock.
///
/// # Errors
/// Returns an error for unreadable/corrupt local state, an invalid signed cache,
/// or a malformed/ambiguous control audience. A plaintext source never qualifies.
#[cfg(unix)]
pub fn cached(paths: &crate::client::Paths) -> crate::Result<Option<Snapshot>> {
    observe(paths).map(Observation::into_snapshot)
}

/// Explain an unavailable offline control observation without creating state or
/// contacting the vault. Missing visibility never authorizes a fresh network.
/// Corrupt or ambiguous authenticated state remains an error.
#[cfg(unix)]
pub fn observe(paths: &crate::client::Paths) -> crate::Result<Observation> {
    crate::client::observe_commons(paths)
}

/// Observe fresh offline control, waiting only for a busy vault until `deadline`.
/// Call before acquiring Commons or journal custody: waiting while holding those
/// locks could prevent the vault writer from finishing. Every attempt reads the
/// current source and applied state; no previous snapshot grants admission.
///
/// # Errors
/// Returns [`observe`]'s errors. A vault still busy at the deadline remains
/// [`Unavailable::Busy`]; other unavailable reasons return immediately.
#[cfg(unix)]
pub fn observe_until(
    paths: &crate::client::Paths,
    deadline: std::time::Instant,
) -> crate::Result<Observation> {
    observe_until_with(paths, deadline, || {})
}

/// How long a read right after a publication waits for this Mac's subscriber, which the
/// publication wakes, to let the vault's state lock go: its apply of one version holds the lock
/// for a moment. A founding's read-back waits so ([`crate::client::admin_commons_init`]), and so
/// does `sync.shared-data`'s read, which may follow one ([`observe_until_settled`]).
#[cfg(unix)]
pub const SETTLE: std::time::Duration = std::time::Duration::from_secs(60);

/// [`observe`] for a reader that asks again every few seconds whatever it found (the Commons
/// runtime's presence poll, the members desk): where every file the observation reads, and the
/// vault folder, are as they were at the last settled observation, it takes that observation's
/// answer instead of reading, decrypting and evaluating them again (`client::vault_memo`). A
/// changed file reads again at once, so a revocation is seen at the next call; a vault a writer
/// holds and an error are never kept.
///
/// # Errors
/// As [`observe`].
#[cfg(unix)]
pub fn observe_settled(paths: &crate::client::Paths) -> crate::Result<Observation> {
    crate::client::observe_commons_now(paths)
}

/// [`observe_until`] by [`observe_settled`]: a busy vault is waited for until `deadline`, and
/// each attempt reads again whatever moved since the last settled observation.
///
/// # Errors
/// As [`observe_until`].
#[cfg(unix)]
pub fn observe_until_settled(
    paths: &crate::client::Paths,
    deadline: std::time::Instant,
) -> crate::Result<Observation> {
    observe_until_by(paths, deadline, || {}, observe_settled)
}

#[cfg(unix)]
pub(crate) fn observe_until_with(
    paths: &crate::client::Paths,
    deadline: std::time::Instant,
    held: impl FnMut(),
) -> crate::Result<Observation> {
    observe_until_by(paths, deadline, held, observe)
}

/// [`observe_until`]'s busy-vault wait around `observe`.
#[cfg(unix)]
fn observe_until_by(
    paths: &crate::client::Paths,
    deadline: std::time::Instant,
    mut held: impl FnMut(),
    observe: impl Fn(&crate::client::Paths) -> crate::Result<Observation>,
) -> crate::Result<Observation> {
    let mut backoff = std::time::Duration::from_millis(1);
    loop {
        let observation = observe(paths)?;
        if !matches!(observation, Observation::Unavailable(Unavailable::Busy)) {
            return Ok(observation);
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(observation);
        }
        #[cfg(test)]
        bootstrap::tests::admission_held();
        held();
        std::thread::sleep(backoff.min(remaining));
        backoff = (backoff * 2).min(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
#[path = "commons_tests.rs"]
mod tests;
