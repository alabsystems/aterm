// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recipient-bound historical read handoffs. An issuer transports keys, never
//! asserts past roles: those come from the original authority-signed ciphertext.
//! This implements Commons' whole-epoch history, not keychain candidate rewraps.

use super::{
    binding::{BINDING_LEN, Binding},
    journal::{Journal, Trust},
    roster::{Access, Historical},
};
use crate::{
    Error, Result,
    age::{self, Identity},
};
use aterm_digest::Sha256;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use zeroize::Zeroizing;

const DOMAIN: &[u8] = b"aterm/commons/history-handoff/1\0";
const CONTEXT_DOMAIN: &[u8] = b"aterm/commons/history-context/1\0";
const HEADER: usize = 5 + 80 + 32 + 32 + 2 * BINDING_LEN + 8;
const PLAIN: usize = 4 + 32 + 16 + 32;
/// Bound checked before signature verification, allocation or age parsing.
pub const MAX_HANDOFF: usize = 2048;

/// Signed encrypted key transport. Its signature identifies the issuer; only
/// opening it against current membership and original signed history grants a read.
#[derive(Clone, Debug)]
pub struct Handoff {
    trust: Trust,
    source: [u8; 32],
    issued_under: [u8; 32],
    issuer: Binding,
    recipient: Binding,
    authored_ms: u64,
    wire: Vec<u8>,
}
impl Handoff {
    /// Parse a bounded signed envelope. This alone establishes no membership.
    pub fn decode(wire: &[u8]) -> Result<Self> {
        if wire.len() <= HEADER + 4 + 64 || wire.len() > MAX_HANDOFF || &wire[..5] != b"ACHG1" {
            return Err(Error::new("invalid history handoff frame"));
        }
        let trust = Trust {
            network: wire[5..21].try_into().unwrap(),
            authority: wire[21..53].try_into().unwrap(),
            genesis: wire[53..85].try_into().unwrap(),
        };
        let source = wire[85..117].try_into().unwrap();
        let issued_under = wire[117..149].try_into().unwrap();
        let issuer = Binding::decode(&wire[149..149 + BINDING_LEN])?;
        let recipient = Binding::decode(&wire[149 + BINDING_LEN..149 + 2 * BINDING_LEN])?;
        let authored_ms = u64::from_le_bytes(wire[HEADER - 8..HEADER].try_into().unwrap());
        let length = u32::from_le_bytes(wire[HEADER..HEADER + 4].try_into().unwrap()) as usize;
        if length != wire.len() - HEADER - 4 - 64
            || !wire[HEADER + 4..].starts_with(b"age-encryption.org/v1\n")
            || [source, issued_under, trust.genesis].contains(&[0; 32])
            || [&issuer, &recipient]
                .iter()
                .any(|b| *b.network() != trust.network || *b.authority() != trust.authority)
        {
            return Err(Error::new("history handoff has another context or length"));
        }
        let signed = wire.len() - 64;
        UnparsedPublicKey::new(&ED25519, issuer.writer())
            .verify(&[DOMAIN, &wire[..signed]].concat(), &wire[signed..])
            .map_err(|_| Error::new("history handoff signature does not verify"))?;
        Ok(Self {
            trust,
            source,
            issued_under,
            issuer,
            recipient,
            authored_ms,
            wire: wire.to_vec(),
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.wire
    }
    pub fn id(&self) -> [u8; 32] {
        Sha256::digest(&self.wire)
    }
    pub const fn source(&self) -> &[u8; 32] {
        &self.source
    }
    pub const fn issued_under(&self) -> &[u8; 32] {
        &self.issued_under
    }
    pub fn issuer(&self) -> &Binding {
        &self.issuer
    }
    pub fn recipient(&self) -> &Binding {
        &self.recipient
    }
    /// Claimed author time, never an ordering, expiry or authorization input.
    pub const fn authored_ms(&self) -> u64 {
        self.authored_ms
    }

    pub(crate) fn issue(
        journal: &Journal,
        access: &Access,
        identity: &Identity,
        seed: &[u8; 32],
        source: [u8; 32],
        recipient: &Binding,
        inherited: Option<&Self>,
    ) -> Result<Self> {
        if !access.is_current(journal)
            || access.local().binding().recipient() != identity.recipient()
            || !access.members().iter().any(|m| m.binding() == recipient)
        {
            return Err(Error::new(
                "history handoff requires current issuer and recipient membership",
            ));
        }
        let pair = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| Error::new("invalid history issuer seed"))?;
        if pair.public_key().as_ref() != access.local().binding().writer() {
            return Err(Error::new("history handoff has another issuer key"));
        }
        let (past, file_key) = match access.historical(journal, identity, source) {
            Ok(past) => (
                past,
                journal.node(&source).unwrap().history_file_key(identity)?,
            ),
            Err(error) => {
                let Some(prior) = inherited.filter(|h| h.source == source) else {
                    return Err(error);
                };
                prior.open_material(journal, access, identity)?
            }
        };
        let authored_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent handoff author time"))?;
        let trust = journal.trust();
        let mut wire = Vec::with_capacity(MAX_HANDOFF);
        wire.extend_from_slice(b"ACHG1");
        wire.extend_from_slice(&trust.network);
        wire.extend_from_slice(&trust.authority);
        wire.extend_from_slice(&trust.genesis);
        wire.extend_from_slice(&source);
        wire.extend_from_slice(access.basis());
        wire.extend_from_slice(&access.local().binding().bytes());
        wire.extend_from_slice(&recipient.bytes());
        wire.extend_from_slice(&authored_ms.to_le_bytes());
        let mut plain = Zeroizing::new(Vec::with_capacity(PLAIN));
        plain.extend_from_slice(b"AHK1");
        plain.extend_from_slice(&context(&wire));
        plain.extend_from_slice(file_key.as_ref());
        past.with_key(journal, |key| plain.extend_from_slice(key))?;
        let sealed = age::encrypt(&[recipient.recipient()], &plain)
            .map_err(|_| Error::new("history handoff could not be encrypted"))?;
        wire.extend_from_slice(&(sealed.len() as u32).to_le_bytes());
        wire.extend_from_slice(&sealed);
        wire.extend_from_slice(pair.sign(&[DOMAIN, &wire].concat()).as_ref());
        Self::decode(&wire)
    }

    pub(crate) fn open(
        &self,
        journal: &Journal,
        access: &Access,
        identity: &Identity,
    ) -> Result<Historical> {
        self.open_material(journal, access, identity)
            .map(|(past, _)| past)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "NewMember",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "Rotated",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "Onward",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "WrongRecipient",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "WrongEpochKey",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "WrongFileKey",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "Stale",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "RetiredRecipient",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "Forked",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "DiscardedIssuance",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbHistoryHandoff",
            action = "FutureSource",
            project = "aterm_secrets::commons::history::tests::handoff_transition"
        )
    )]
    fn open_material(
        &self,
        journal: &Journal,
        access: &Access,
        identity: &Identity,
    ) -> Result<(Historical, Zeroizing<[u8; 16]>)> {
        if self.trust != journal.trust()
            || !access.is_current(journal)
            || &self.recipient != access.local().binding()
            || self.recipient.recipient() != identity.recipient()
        {
            return Err(Error::new(
                "history handoff requires its exact current recipient",
            ));
        }
        // The recipient was already admitted at issuance and can open this
        // roster natively. No recursive trust in another handoff is necessary.
        let issuance = access.historical(journal, identity, self.issued_under)?;
        if !journal.selected_ancestor(self.source, self.issued_under) {
            return Err(Error::new(
                "history source is not an ancestor of its issuance",
            ));
        }
        if ![&self.issuer, &self.recipient]
            .iter()
            .all(|binding| issuance.members().iter().any(|m| m.binding() == *binding))
        {
            return Err(Error::new(
                "history handoff issuer or recipient was not admitted",
            ));
        }
        let plain = age::decrypt(
            std::slice::from_ref(identity),
            &self.wire[HEADER + 4..self.wire.len() - 64],
        )
        .map_err(|_| Error::new("history handoff could not be opened"))?;
        let bytes = plain.as_slice();
        if bytes.len() != PLAIN
            || &bytes[..4] != b"AHK1"
            || bytes[4..36] != context(&self.wire[..HEADER])
        {
            return Err(Error::new("history handoff plaintext has another context"));
        }
        let file_key: Zeroizing<[u8; 16]> = Zeroizing::new(bytes[36..52].try_into().unwrap());
        let past = access.historical_with_key(
            journal,
            identity,
            self.source,
            &file_key,
            bytes[52..].try_into().unwrap(),
        )?;
        Ok((past, file_key))
    }
}

fn context(header: &[u8]) -> [u8; 32] {
    Sha256::digest([CONTEXT_DOMAIN, header].concat())
}

#[cfg(test)]
mod tests;
