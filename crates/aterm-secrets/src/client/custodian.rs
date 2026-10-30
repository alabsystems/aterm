// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CUSTODIAN'S VAULT SIDE (docs/DESIGN-credential-requests-2026-10-27.md §7.1, §8.1, §8.3):
//! what only a Mac that can sign with the vault's current key does for a credential request.
//! It attests the records it writes, matches a requester to a vault host by the age recipient
//! its Commons binding names (never by a name), and copies one entry the vault already holds
//! into that host's own section, never by renaming a section, which would hand over every
//! other entry in it.

use super::Admin;
use crate::need::custody;
use crate::text::{Audience, Document};
use crate::{Error, age};

/// The most host audiences a vault version holds (`text::Document::lockers`), less
/// `[everyone]`.
pub const MAX_HOST_AUDIENCES: usize = 62;

/// What a copy from vault text did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// The entry went into the host's own section.
    Copied,
    /// The host already reads it: its own section, a section it shares, or `[everyone]`.
    AlreadyThere,
    /// The vault holds it under no audience.
    NotHeld,
    /// A copy would pass the vault's host-audience limit: the request waits, naming it.
    AudienceLimit,
    /// No `[machines]` row has the requester's recipient: the vault does not hold that Mac.
    NoHost,
}

impl Admin {
    /// The vault signing key this admin signs with, which every member pins.
    ///
    /// # Errors
    /// The seed is not a usable Ed25519 seed.
    pub fn custodian_signer(&self) -> crate::Result<[u8; 32]> {
        crate::envelope::signer_public(&self.seed)
    }

    /// Attest a credential record (its kind, key and body) on the Commons `network` (§5).
    ///
    /// # Errors
    /// As [`custody::attest`].
    pub fn attest_credential_record(
        &self,
        network: &[u8; 16],
        record: custody::Attested<'_>,
    ) -> crate::Result<[u8; 64]> {
        custody::attest(&self.seed, network, record)
    }

    /// The `[machines]` row whose recipient is `recipient`, byte for byte (§7.1): the vault
    /// text holds no two machines with one recipient, so a host section names one recipient.
    #[must_use]
    pub fn host_of_recipient(&self, recipient: &age::Recipient) -> Option<String> {
        let wanted = recipient.to_bech32();
        if self
            .published_doc
            .code_only()
            .into_iter()
            .any(|code_only| code_only == wanted)
        {
            return None;
        }
        let roster = (self.log_base == super::log_entries::LogBase::NoLog)
            .then(|| {
                crate::swarm_roster::SwarmRoster::from_document(&self.published_doc)
                    .ok()
                    .flatten()
            })
            .flatten();
        self.published_doc
            .machines()
            .into_iter()
            .find(|machine| {
                machine.recipient == wanted
                    && !roster.as_ref().is_some_and(|r| {
                        r.is_label_denied(&machine.name)
                            || r.member_by_recipient(&machine.recipient)
                                .is_some_and(|row| {
                                    super::steward::member_is_denied_in_roster(r, row)
                                })
                    })
            })
            .map(|machine| machine.name)
    }

    /// Copy `entry` from the authenticated text this admin opened into the own section of
    /// the host whose recipient is `recipient`, in the draft [`Admin::doc`] the caller then
    /// publishes (§8.1). The source is the published text, never the draft, so an edit
    /// through the draft cannot make a value appear held.
    ///
    /// # Errors
    /// The draft refuses the entry.
    pub fn copy_to_requester(
        &mut self,
        recipient: &age::Recipient,
        entry: &str,
        if_absent: bool,
    ) -> crate::Result<(Copied, Option<String>)> {
        let Some(host) = self.host_of_recipient(recipient) else {
            return Ok((Copied::NoHost, None));
        };
        let copied = copy_into_host(&self.published_doc, &mut self.doc, entry, &host, if_absent)?;
        Ok((copied, Some(host)))
    }
}

/// Copy `entry`, which the authenticated `published` text holds under some audience, into
/// `host`'s own section of `draft` (§8.1), `if-absent` where `if_absent`. A section header is
/// never renamed and no other entry moves. A host that already reads the entry, from its own
/// section, a section it shares or `[everyone]`, gets no copy: the vault text refuses one name in
/// two host sections that share a host, so such a copy would fail the whole publication.
///
/// # Errors
/// The draft refuses the entry.
pub fn copy_into_host(
    published: &Document,
    draft: &mut Document,
    entry: &str,
    host: &str,
    if_absent: bool,
) -> crate::Result<Copied> {
    let own = Audience::Hosts(vec![host.to_owned()]);
    if draft
        .entries()
        .into_iter()
        .any(|held| held.name == entry && held.audience.includes(host))
    {
        return Ok(Copied::AlreadyThere);
    }
    let Some(source) = published
        .entries()
        .into_iter()
        .find(|held| held.name == entry)
    else {
        return Ok(Copied::NotHeld);
    };
    let audiences: std::collections::BTreeSet<Audience> = draft
        .entries()
        .into_iter()
        .map(|held| held.audience)
        .filter(|audience| matches!(audience, Audience::Hosts(_)))
        .collect();
    if !audiences.contains(&own) && audiences.len() >= MAX_HOST_AUDIENCES {
        return Ok(Copied::AudienceLimit);
    }
    draft
        .set_entry_with(&own, entry, source.value.as_slice(), if_absent, None)
        .map_err(|error| Error::new(error.to_string()))?;
    Ok(Copied::Copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        let recipient = || age::Identity::generate().unwrap().recipient().to_bech32();
        let mut doc = Document::new_vault("m22", &recipient());
        for host in ["m3", "andrews"] {
            doc.add_machine(host, &recipient()).unwrap();
        }
        let m3 = Audience::Hosts(vec!["m3".into()]);
        doc.set_entry(&m3, "notary", b"apple").unwrap();
        doc.set_entry(&m3, "other", b"m3 only").unwrap();
        doc
    }

    #[test]
    fn a_copy_puts_only_the_entry_in_the_requesters_own_section_and_leaves_the_header() {
        let published = doc();
        let mut draft = published.clone();
        let copied = copy_into_host(&published, &mut draft, "notary", "andrews", true).unwrap();
        assert_eq!(copied, Copied::Copied);
        let own = Audience::Hosts(vec!["andrews".into()]);
        let m3 = Audience::Hosts(vec!["m3".into()]);
        let mine = draft.entry(&own, "notary").unwrap();
        assert!(mine.if_absent && mine.value.as_slice() == b"apple");
        assert!(
            draft.entry(&own, "other").is_none(),
            "the rest of [m3] stays m3's"
        );
        assert!(draft.entry(&m3, "notary").is_some() && draft.entry(&m3, "other").is_some());
        assert!(
            draft
                .render()
                .as_str()
                .unwrap()
                .lines()
                .any(|line| line == "[m3]"),
            "[m3]'s header is unchanged"
        );
        // The copy parses as any vault text does, and a second copy changes nothing.
        Document::parse(draft.render().as_str().unwrap()).unwrap();
        assert_eq!(
            copy_into_host(&published, &mut draft, "notary", "andrews", true).unwrap(),
            Copied::AlreadyThere
        );
        assert_eq!(
            copy_into_host(&published, &mut draft, "absent", "andrews", false).unwrap(),
            Copied::NotHeld
        );
    }

    /// A host that already reads the entry through a section it shares gets no second copy,
    /// which the vault text would refuse (one name in two sections that share a host).
    #[test]
    fn a_host_that_reads_the_entry_through_a_shared_section_gets_no_copy() {
        let mut published = doc();
        let shared = Audience::Hosts(vec!["andrews".into(), "m3".into()]);
        published.set_entry(&shared, "shared", b"both").unwrap();
        published
            .set_entry(&Audience::Everyone, "everywhere", b"all")
            .unwrap();
        let mut draft = published.clone();
        for entry in ["shared", "everywhere"] {
            assert_eq!(
                copy_into_host(&published, &mut draft, entry, "andrews", false).unwrap(),
                Copied::AlreadyThere,
                "{entry}"
            );
        }
        assert!(
            draft.render().as_slice() == published.render().as_slice(),
            "the draft is unchanged"
        );
        Document::parse(draft.render().as_str().unwrap()).unwrap();
    }
}
