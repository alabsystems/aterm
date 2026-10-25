// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Encrypted custody for the ALab Commons control record. This record is never a
//! materialized secret file. Its consumers receive an authenticated snapshot of
//! the locally applied vault, including the authority and exact version.

use crate::{SecretBytes, age::Recipient};

pub mod binding;
#[cfg(unix)]
pub mod bootstrap;
pub mod constraints;
#[cfg(unix)]
pub mod journal;
pub mod membership;
pub mod node;
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
#[derive(Debug)]
pub struct Snapshot {
    vault: String,
    signer: [u8; 32],
    followed: Vec<[u8; 32]>,
    sequence: u64,
    digest: [u8; 32],
    recipient: Recipient,
    control: SecretBytes,
}

impl Snapshot {
    #[cfg(unix)]
    pub(crate) fn verified(
        vault: String,
        envelope: &crate::envelope::Envelope,
        followed: Vec<[u8; 32]>,
        recipient: Recipient,
        control: SecretBytes,
    ) -> Self {
        Self {
            vault,
            signer: envelope.signer,
            followed,
            sequence: envelope.seq,
            digest: envelope.digest(),
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
    crate::client::cached_commons(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> crate::text::Document {
        let mut document = crate::text::Document::new_vault(
            "m0",
            &crate::age::Identity::generate()
                .unwrap()
                .recipient()
                .to_bech32(),
        );
        for n in 1..7 {
            document
                .add_machine(
                    &format!("m{n}"),
                    &crate::age::Identity::generate()
                        .unwrap()
                        .recipient()
                        .to_bech32(),
                )
                .unwrap();
        }
        document
    }

    #[test]
    fn control_text_requires_one_explicit_signed_audience() {
        let named = format!("[m1 m2]\n{ENTRY}=inert-control\n");
        assert_eq!(
            crate::text::parse_locker(named.as_bytes()).unwrap().len(),
            1
        );
        assert!(crate::text::parse_plain(named.as_bytes()).is_err());
        for text in [
            format!("[everyone]\n{ENTRY}=inert-control\n"),
            format!("[m1]\n{ENTRY}<<END if-absent\ninert-control\nEND\n"),
            format!("[m1]\n{ENTRY}=one\n[m2]\n{ENTRY}=two\n"),
            format!("[m1]\n{}=inert-control\n", ENTRY.to_uppercase()),
            format!("[m1]\n~/.secrets/{ENTRY}=inert-control\n"),
            "[m1]\naterm-commons-control-v2=inert-future-control\n".into(),
        ] {
            assert!(crate::text::parse_locker(text.as_bytes()).is_err());
        }
    }

    #[test]
    fn control_audience_changes_are_atomic_and_do_not_accumulate_empty_lockers() {
        use crate::text::Audience;
        let mut document = document();
        for bits in 1u16..=80 {
            let hosts = (0..7)
                .filter(|n| bits & (1 << n) != 0)
                .map(|n| format!("m{n}"))
                .collect();
            document
                .set_entry(&Audience::Hosts(hosts), ENTRY, b"inert-control")
                .unwrap();
            assert_eq!(
                document
                    .entries()
                    .iter()
                    .filter(|entry| entry.name == ENTRY)
                    .count(),
                1
            );
            assert_eq!(
                document.lockers().len(),
                2,
                "membership changes do not consume the 62-audience limit"
            );
        }
        let before = document.render();
        for audience in [Audience::Everyone, Audience::Hosts(vec!["unknown".into()])] {
            assert!(
                document
                    .set_entry(&audience, ENTRY, b"invalid replacement")
                    .is_err()
            );
            assert_eq!(
                document.render(),
                before,
                "the existing descriptor survives a failed move"
            );
        }
        document
            .set_entry(&Audience::Hosts(vec!["m1".into()]), ENTRY, b"inert-control")
            .unwrap();
        let before = document.render();
        assert!(document.remove_machine("m1", true).is_err());
        assert_eq!(
            document.render(),
            before,
            "forced member cleanup cannot erase the network descriptor"
        );
        document
            .set_entry(&Audience::Hosts(vec!["m2".into()]), ENTRY, b"inert-control")
            .unwrap();
        assert!(document.remove_machine("m1", true).unwrap());
    }

    #[test]
    fn moving_control_preserves_other_entries_in_its_previous_audience() {
        use crate::text::Audience;
        let mut document = document();
        let previous = Audience::Hosts(vec!["m1".into()]);
        document
            .set_entry(&previous, "ordinary", b"keep this")
            .unwrap();
        document
            .set_entry(&previous, ENTRY, b"inert-control")
            .unwrap();
        document
            .set_entry(&Audience::Hosts(vec!["m2".into()]), ENTRY, b"inert-control")
            .unwrap();
        assert_eq!(
            document
                .entry(&previous, "ordinary")
                .unwrap()
                .value
                .as_slice(),
            b"keep this"
        );
        assert!(document.entry(&previous, ENTRY).is_none());
        assert_eq!(document.lockers().len(), 3);
    }
}
