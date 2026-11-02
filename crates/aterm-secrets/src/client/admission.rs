// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Admission provenance is custody metadata, not editable vault text. The
//! enclosing signed envelope authenticates it; it is never a portable grant.

use super::{Document, Error, Recipient, admin_keys};
use crate::commons::journal::Trust;
use std::collections::BTreeMap;

const LIMIT: usize = 4096;
const RECORD: usize = 81;
const HEADER: usize = 86;
pub(super) const MAX_BYTES: usize = HEADER + LIMIT * RECORD;

/// How the authority admitted a recipient, independent of today's join policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// The first owner explicitly created this Commons network.
    Founder,
    /// A join proof matched the password active when approval occurred.
    Password,
    /// An admin explicitly approved this recipient.
    Manual,
    /// Ordinary open-vault approval: the admin chose to let in every machine that
    /// asks, and that choice admits it to Commons like any other (owner, 2026-10-26).
    Open,
}

impl Method {
    fn tag(self) -> u8 {
        match self {
            Self::Founder => 0,
            Self::Password => 1,
            Self::Manual => 2,
            Self::Open => 3,
        }
    }

    fn from_tag(tag: u8) -> crate::Result<Self> {
        match tag {
            0 => Ok(Self::Founder),
            1 => Ok(Self::Password),
            2 => Ok(Self::Manual),
            3 => Ok(Self::Open),
            _ => Err(Error::new("unknown admission method")),
        }
    }
}

/// An authority's authenticated decision. Time is provenance, not an ordering,
/// expiry, or authorization rule. Only the current admin snapshot can use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Evidence {
    /// Actual approval path, never inferred from current join settings.
    pub method: Method,
    /// Vault publication containing this decision.
    pub sequence: u64,
    /// Author's Unix time in milliseconds, with no clock-trust claim.
    pub authored_ms: u64,
    /// Age identity of the admin machine that published this decision. It is
    /// an authority-signed attribution, not hardware or human attestation.
    pub author: Recipient,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Ledger {
    trust: Option<Trust>,
    entries: BTreeMap<[u8; 32], Evidence>,
}

impl Ledger {
    pub(super) fn bytes(&self) -> Vec<u8> {
        let mut bytes = b"AAD1".to_vec();
        let Some(trust) = self.trust else {
            return bytes;
        };
        bytes.extend_from_slice(&trust.network);
        bytes.extend_from_slice(&trust.authority);
        bytes.extend_from_slice(&trust.genesis);
        bytes.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        for (recipient, evidence) in &self.entries {
            bytes.extend_from_slice(recipient);
            bytes.push(evidence.method.tag());
            bytes.extend_from_slice(&evidence.sequence.to_le_bytes());
            bytes.extend_from_slice(&evidence.authored_ms.to_le_bytes());
            bytes.extend_from_slice(&evidence.author.0);
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8]) -> crate::Result<Self> {
        if !bytes.starts_with(b"AAD1") || bytes.len() > MAX_BYTES {
            return Err(Error::new("invalid admission ledger"));
        }
        if bytes.len() == 4 {
            return Ok(Self::default());
        }
        if bytes.len() < HEADER {
            return Err(Error::new("truncated admission ledger"));
        }
        let count = u16::from_le_bytes(bytes[84..86].try_into().unwrap()) as usize;
        if count > LIMIT || bytes.len() != HEADER + count * RECORD {
            return Err(Error::new("invalid admission ledger length"));
        }
        let mut ledger = Self {
            trust: Some(Trust {
                network: bytes[4..20].try_into().unwrap(),
                authority: bytes[20..52].try_into().unwrap(),
                genesis: bytes[52..84].try_into().unwrap(),
            }),
            entries: BTreeMap::new(),
        };
        for record in bytes[HEADER..].as_chunks::<RECORD>().0 {
            let recipient = record[..32].try_into().unwrap();
            let evidence = Evidence {
                method: Method::from_tag(record[32])?,
                sequence: u64::from_le_bytes(record[33..41].try_into().unwrap()),
                authored_ms: u64::from_le_bytes(record[41..49].try_into().unwrap()),
                author: Recipient(record[49..81].try_into().unwrap()),
            };
            if evidence.sequence == 0
                || Recipient::parse(&evidence.author.to_bech32()).is_err()
                || ledger.entries.insert(recipient, evidence).is_some()
            {
                return Err(Error::new("duplicate or invalid admission decision"));
            }
        }
        if ledger.bytes() != bytes {
            return Err(Error::new("noncanonical admission ledger"));
        }
        Ok(ledger)
    }

    pub(super) fn validate(&self, doc: &Document, sequence: u64) -> crate::Result<()> {
        if self.trust != admin_keys::control_descriptor(doc)?.map(|d| d.trust()) {
            return Err(Error::new(
                "admission ledger belongs to another Commons root",
            ));
        }
        let recipients = recipients(doc)?;
        if self.entries.iter().any(|(recipient, evidence)| {
            !recipients.contains_key(recipient) || evidence.sequence > sequence
        }) {
            return Err(Error::new("admission ledger is not current for this vault"));
        }
        Ok(())
    }

    /// Ordinary edits preserve surviving decisions but cannot mint a decision.
    /// A published absence invalidates provenance even if a later publication
    /// re-adds the same key. Intermediate draft edits are not membership events.
    pub(super) fn retained(&self, doc: &Document) -> crate::Result<Self> {
        let recipients = recipients(doc)?;
        let mut next = self.clone();
        let trust = admin_keys::control_descriptor(doc)?.map(|d| d.trust());
        if self.trust.is_some() && self.trust != trust {
            return Err(Error::new(
                "cannot move admission evidence to another Commons root",
            ));
        }
        next.trust = trust;
        next.entries.retain(|key, _| recipients.contains_key(key));
        Ok(next)
    }

    /// Combine authenticated branch decisions for surviving recipients under the
    /// same root. Branch order breaks ties; an open approval never displaces an
    /// existing non-open decision. No machine-list entry creates evidence.
    pub(super) fn merged<'a>(
        doc: &Document,
        branches: impl IntoIterator<Item = &'a Self>,
    ) -> crate::Result<Self> {
        let mut merged = Self::default().retained(doc)?;
        for branch in branches {
            for (recipient, evidence) in branch.retained(doc)?.entries {
                if merged.entries.get(&recipient).is_none_or(|held| {
                    held.method == Method::Open && evidence.method != Method::Open
                }) {
                    merged.entries.insert(recipient, evidence);
                }
            }
            if merged.entries.len() > LIMIT {
                return Err(Error::new("merged admission ledger is full"));
            }
        }
        Ok(merged)
    }

    pub(super) fn record(
        &mut self,
        doc: &Document,
        recipient: &str,
        evidence: Evidence,
    ) -> crate::Result<()> {
        let key = Recipient::parse(recipient)
            .map_err(|_| Error::new("invalid admission recipient"))?
            .0;
        if !recipients(doc)?.contains_key(&key) {
            return Err(Error::new(
                "admission recipient is absent from the published draft",
            ));
        }
        let Some(descriptor) = admin_keys::control_descriptor(doc)? else {
            // Legacy vaults have no authenticated Commons context to admit to.
            return Ok(());
        };
        let trust = descriptor.trust();
        if self.trust.is_some_and(|old| old != trust) || evidence.sequence == 0 {
            return Err(Error::new("invalid admission context"));
        }
        if self.entries.len() >= LIMIT && !self.entries.contains_key(&key) {
            return Err(Error::new("admission ledger is full"));
        }
        // An open approval never replaces the decision a recipient already holds.
        if evidence.method == Method::Open && self.entries.contains_key(&key) {
            return Ok(());
        }
        self.trust = Some(trust);
        self.entries.insert(key, evidence);
        Ok(())
    }

    pub(super) fn evidence(&self, recipient: &Recipient) -> Option<Evidence> {
        self.entries.get(&recipient.0).copied()
    }

    /// Any recorded decision under this root qualifies, an open one included: the
    /// vault's admin chose open admission (owner, 2026-10-26: "always approve by default").
    pub(super) fn permits(&self, trust: Trust, recipient: &Recipient) -> bool {
        self.trust == Some(trust) && self.evidence(recipient).is_some()
    }
}

fn recipients(doc: &Document) -> crate::Result<BTreeMap<[u8; 32], ()>> {
    doc.machines()
        .into_iter()
        .map(|machine| {
            Recipient::parse(&machine.recipient)
                .map(|recipient| (recipient.0, ()))
                .map_err(|_| Error::new("invalid machine recipient"))
        })
        .collect()
}

#[cfg(test)]
mod tests;
