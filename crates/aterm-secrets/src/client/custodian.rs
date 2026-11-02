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

/// Validate the same four fields the subscriber's notary consumer reads, without
/// opening a keychain or launching any tool.
///
/// # Errors
/// The bytes do not have the consumer's `use=aterm`, Apple id, team id and password shape.
pub fn validate_notary_contribution(bytes: &[u8]) -> crate::Result<()> {
    super::release::parse_notary(bytes).map(|_| ())
}

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
    /// Fill a requester's own audience from a delivered value. This path cannot rotate a
    /// value; an existing entry may only be copied unchanged into another audience.
    /// The caller validates the want and rechecks membership and policy before publication.
    ///
    /// # Errors
    /// The requester is unknown, the want/entries are invalid, an existing value differs,
    /// the audience limit is reached, or the draft refuses an entry.
    pub fn contribute_to_requester(
        &mut self,
        recipient: &age::Recipient,
        want: &crate::need::Want,
        values: &crate::need::transfer::Values,
    ) -> crate::Result<String> {
        values.encode()?;
        let expected = want.entries();
        if !want.posted()
            || values.0.first().map(|(name, _)| name) != expected.first()
            || values.0.iter().any(|(name, _)| !expected.contains(name))
        {
            return Err(Error::new(
                "credential contribution does not match the want",
            ));
        }
        let host = self
            .host_of_recipient(recipient)
            .ok_or_else(|| Error::new("the vault does not name the requester's recipient"))?;
        let own = Audience::Hosts(vec![host.clone()]);
        let audiences: std::collections::BTreeSet<_> = self
            .doc
            .entries()
            .into_iter()
            .map(|entry| entry.audience)
            .filter(|audience| matches!(audience, Audience::Hosts(_)))
            .collect();
        if !audiences.contains(&own) && audiences.len() >= MAX_HOST_AUDIENCES {
            return Err(Error::new("the vault host-audience limit is reached"));
        }
        let mut draft = self.doc.clone();
        for (name, value) in &values.0 {
            let entries = self.published_doc.entries();
            if let Some(held) = entries
                .iter()
                .find(|entry| entry.name == *name && entry.audience.includes(&host))
            {
                if held.value.as_slice() != value.as_slice() {
                    return Err(Error::new(
                        "a contribution cannot replace the requester's existing entry",
                    ));
                }
                continue;
            }
            if entries.iter().any(|entry| entry.name == *name)
                && !entries
                    .iter()
                    .any(|entry| entry.name == *name && entry.value.as_slice() == value.as_slice())
            {
                return Err(Error::new(
                    "a contribution cannot rotate a value the vault holds",
                ));
            }
            draft
                .set_entry_with(&own, name, value.as_slice(), want.if_absent(), None)
                .map_err(|_| Error::new("the vault refused the contribution"))?;
        }
        self.doc = draft;
        Ok(host)
    }

    /// Sign this connection-bound credential handoff with the current vault key.
    ///
    /// # Errors
    /// The transcript names another vault or has an invalid vault name.
    pub fn prove_credential_custody(
        &self,
        exchange: &crate::need::transfer::Exchange,
    ) -> crate::Result<[u8; 64]> {
        if exchange.vault != self.config.name {
            return Err(Error::new("credential handoff names another vault"));
        }
        exchange.prove(&self.seed)
    }

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
        let roster = matches!(
            self.log_base,
            super::log_entries::LogBase::NoLog | super::log_entries::LogBase::Behind
        )
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
                        if let Some(row) = r.member_by_recipient(&machine.recipient) {
                            super::steward::member_is_denied_in_roster(r, row)
                        } else {
                            r.is_label_denied(&machine.name)
                                || r.member_by_label_or_alias(&machine.name)
                                    .is_some_and(|row| {
                                        super::steward::member_is_denied_in_roster(r, row)
                                    })
                        }
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
