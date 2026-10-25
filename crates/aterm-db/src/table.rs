// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secrets-backed table envelopes. A short-lived session pins the control journal
//! while authenticating a batch; it must be dropped before network I/O. Historical
//! authentication is evidence, not current application authorization. Durable
//! storage, catalog cutoffs and resource admission belong above this boundary.

use crate::{
    Error, Result,
    crypto::{self, Causal, MAX_BODY, MAX_OPERATION, Reader},
};
use astream_wire::sync::{Actor, Digest, Header, Index, MAX_PARENTS};
use aterm_digest::{Sha256, hkdf_expand_32, hkdf_extract};
use aterm_secrets::{
    SecretBytes,
    commons::{
        bootstrap::Bootstrap,
        journal::{Journal, Trust},
        roster::{Access, Role},
    },
};
use ring::aead;
use zeroize::Zeroizing;

const DOMAIN: &[u8] = b"aterm/commons/table-operation/1\0";
const SCOPE_LEN: usize = 16 + 32 + 32 + 16 + 8;

fn failure(error: aterm_secrets::Error) -> Error {
    Error::new(&error.to_string())
}

/// Immutable table/audience identity. Display names do not enter key derivation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope {
    trust: Trust,
    table: [u8; 16],
    generation: u64,
}
impl Scope {
    /// A known table descriptor, learned inside an authenticated catalog.
    pub fn new(trust: Trust, table: [u8; 16], generation: u64) -> Result<Self> {
        if table == [0; 16] || generation == 0 {
            return Err(Error::new(
                "table identity and audience generation must be nonzero",
            ));
        }
        Ok(Self {
            trust,
            table,
            generation,
        })
    }
    /// A fresh table can be created offline by an admitted writer.
    pub fn fresh(trust: Trust) -> Result<Self> {
        Self::new(trust, crypto::random()?, 1)
    }
    pub const fn trust(&self) -> Trust {
        self.trust
    }
    pub const fn table(&self) -> &[u8; 16] {
        &self.table
    }
    pub const fn generation(&self) -> u64 {
        self.generation
    }
    /// The exact table identifier used in signed revocation cutoff facts.
    pub fn digest(&self) -> Digest {
        let mut bytes = b"aterm/commons/table-id/1\0".to_vec();
        bytes.extend_from_slice(&self.bytes());
        Sha256::digest(&bytes)
    }
    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SCOPE_LEN);
        bytes.extend_from_slice(&self.trust.network);
        bytes.extend_from_slice(&self.trust.authority);
        bytes.extend_from_slice(&self.trust.genesis);
        bytes.extend_from_slice(&self.table);
        bytes.extend_from_slice(&self.generation.to_le_bytes());
        bytes
    }
    fn key(&self, roster: &Digest, epoch: u64, network_key: &[u8; 32]) -> Zeroizing<[u8; 32]> {
        let prk = Zeroizing::new(hkdf_extract(b"aterm/commons/table-key/1\0", network_key));
        let mut info = self.bytes();
        info.extend_from_slice(roster);
        info.extend_from_slice(&epoch.to_le_bytes());
        Zeroizing::new(hkdf_expand_32(&prk, &info))
    }
}

/// Authenticated historical evidence. This type deliberately grants no writer
/// capability and performs no database projection. Check current table cutoffs
/// and application roles before using its payload; never discard budget liabilities.
pub struct Record {
    scope: Scope,
    roster: Digest,
    epoch: u64,
    header: Header,
    authored_ms: u64,
    payload: SecretBytes,
}
impl Record {
    pub const fn scope(&self) -> Scope {
        self.scope
    }
    pub const fn roster(&self) -> &Digest {
        &self.roster
    }
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn header(&self) -> &Header {
        &self.header
    }
    /// Signed wall-clock claim, never an ordering or permission input.
    pub const fn authored_ms(&self) -> u64 {
        self.authored_ms
    }
    pub fn payload(&self) -> &[u8] {
        self.payload.as_slice()
    }
}

/// A current, secrets-backed membership observation and its journal lock. No raw
/// keys are saved, no identity is generated, and no new admission root is created.
pub struct Session<'a> {
    bootstrap: &'a Bootstrap,
    journal: Journal,
    access: Access,
}
impl<'a> Session<'a> {
    pub fn open(bootstrap: &'a Bootstrap) -> Result<Self> {
        let journal =
            Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).map_err(failure)?;
        let access = bootstrap
            .access_in(&journal)
            .map_err(failure)?
            .ok_or_else(|| Error::new("Commons membership is not ready for table access"))?;
        Ok(Self {
            bootstrap,
            journal,
            access,
        })
    }
    pub fn actor(&self) -> Actor {
        self.access.local().binding().actor()
    }
    pub const fn epoch(&self) -> u64 {
        self.access.generation()
    }
    pub fn scope(&self) -> Result<Scope> {
        self.writer()?;
        Scope::fresh(self.journal.trust())
    }
    fn context(&self, scope: Scope) -> Result<()> {
        if scope.trust != self.journal.trust() || !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "table has another trust context or stale membership",
            ));
        }
        Ok(())
    }
    fn writer(&self) -> Result<()> {
        if self.access.local().role() != Role::Writer || !self.access.is_current(&self.journal) {
            return Err(Error::new(
                "current Commons membership does not grant table writing",
            ));
        }
        Ok(())
    }
    /// Seal using the current epoch and local writer. The caller supplies a
    /// durably reserved causal position; sealing itself is not a durable commit.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "ReaderWrites",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    pub fn seal(&self, scope: Scope, causal: Causal<'_>, payload: &[u8]) -> Result<Vec<u8>> {
        let authored_ms = aterm_time::unix_millis()
            .ok_or_else(|| Error::new("system clock cannot represent an author timestamp"))?;
        self.seal_at(scope, causal, payload, authored_ms)
    }
    fn seal_at(
        &self,
        scope: Scope,
        causal: Causal<'_>,
        payload: &[u8],
        authored_ms: u64,
    ) -> Result<Vec<u8>> {
        self.context(scope)?;
        self.writer()?;
        if payload.len() > MAX_BODY
            || causal.parents.len() > MAX_PARENTS
            || causal.previous == Some([0; 32])
        {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let mut wire = b"ACTO1".to_vec();
        wire.extend_from_slice(&scope.bytes());
        wire.extend_from_slice(self.access.basis());
        wire.extend_from_slice(&self.access.generation().to_le_bytes());
        wire.extend_from_slice(&self.actor());
        wire.extend_from_slice(&causal.sequence.to_le_bytes());
        wire.extend_from_slice(&authored_ms.to_le_bytes());
        wire.extend_from_slice(&causal.previous.unwrap_or([0; 32]));
        wire.push(causal.parents.len() as u8);
        for parent in causal.parents {
            wire.extend_from_slice(parent);
        }
        let salt = crypto::random()?;
        wire.extend_from_slice(&salt);
        wire.extend_from_slice(&((payload.len() + 16) as u32).to_le_bytes());
        let mut body = payload.to_vec();
        self.access
            .with_key(&self.journal, |network_key| {
                crypto::cipher(
                    &scope.key(self.access.basis(), self.epoch(), network_key),
                    &salt,
                    &wire,
                )?
                .seal_in_place_append_tag(
                    aead::Nonce::assume_unique_for_key([0; 12]),
                    aead::Aad::from(&wire),
                    &mut body,
                )
                .map_err(|_| Error::new("table encryption failed"))
            })
            .map_err(failure)??;
        wire.extend_from_slice(&body);
        wire.extend_from_slice(
            &self
                .bootstrap
                .device()
                .with_seed(|seed| crypto::signed(seed, DOMAIN, &wire))?,
        );
        check_shape(Header {
            id: Sha256::digest(&wire),
            actor: self.actor(),
            sequence: causal.sequence,
            previous: causal.previous,
            parents: causal.parents.to_vec(),
        })?;
        Ok(wire)
    }
    /// Authenticate a bounded record under its exact historical writer roster.
    /// Missing keys/history, discarded control branches and reader signatures
    /// refuse. The returned evidence still requires current application policy.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "Valid",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "UnknownTable",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "ForgedReader",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "WrongEpoch",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "Tampered",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "Malformed",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableEnvelope",
            action = "OldEpoch",
            project = "aterm_db::table::tests::envelope_transition"
        )
    )]
    pub fn authenticate(&self, scope: Scope, wire: &[u8]) -> Result<Record> {
        self.context(scope)?;
        if wire.len() > MAX_OPERATION {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let mut r = Reader::new(wire);
        if r.take(5)? != b"ACTO1" || r.take(SCOPE_LEN)? != scope.bytes() {
            return Err(Error::new(
                "table operation has another audience or protocol",
            ));
        }
        let roster = r.array()?;
        let epoch = u64::from_le_bytes(r.array()?);
        let actor: Actor = r.array()?;
        let sequence = u64::from_le_bytes(r.array()?);
        let authored_ms = u64::from_le_bytes(r.array()?);
        let value = r.array()?;
        let previous = if value == [0; 32] { None } else { Some(value) };
        let count = r.byte()? as usize;
        if count > MAX_PARENTS {
            return Err(Error::new("too many table causal parents"));
        }
        let mut parents = Vec::with_capacity(count);
        for _ in 0..count {
            parents.push(r.array()?);
        }
        let salt = r.array()?;
        let n = u32::from_le_bytes(r.array()?) as usize;
        if !(16..=MAX_BODY + 16).contains(&n) {
            return Err(Error::new("invalid table payload length"));
        }
        let aad_end = wire.len() - r.remaining();
        let body = r.take(n)?;
        let signed_end = wire.len() - r.remaining();
        let signature = r.take(64)?;
        r.end()?;
        crypto::verify(
            &actor[..32].try_into().unwrap(),
            DOMAIN,
            &wire[..signed_end],
            signature,
        )?;
        let header = Header {
            id: Sha256::digest(wire),
            actor,
            sequence,
            previous,
            parents,
        };
        check_shape(header.clone())?;
        let history = self
            .bootstrap
            .historical(&self.journal, &self.access, roster)
            .map_err(failure)?;
        if history.generation() != epoch
            || !history
                .members()
                .iter()
                .any(|member| member.binding().actor() == actor && member.role() == Role::Writer)
        {
            return Err(Error::new("record lacks an exact historical writer grant"));
        }
        let mut payload = Zeroizing::new(body.to_vec());
        let len = history
            .with_key(&self.journal, |network_key| {
                crypto::cipher(
                    &scope.key(&roster, epoch, network_key),
                    &salt,
                    &wire[..aad_end],
                )?
                .open_in_place(
                    aead::Nonce::assume_unique_for_key([0; 12]),
                    aead::Aad::from(&wire[..aad_end]),
                    &mut payload,
                )
                .map(|plain| plain.len())
                .map_err(|_| Error::new("table payload authentication failed"))
            })
            .map_err(failure)??;
        payload.truncate(len);
        Ok(Record {
            scope,
            roster,
            epoch,
            header,
            authored_ms,
            payload: SecretBytes::new(std::mem::take(&mut *payload)),
        })
    }
}

fn check_shape(header: Header) -> Result<()> {
    Index::new(1)
        .insert(header)
        .map(|_| ())
        .map_err(|_| Error::new("malformed table causal header"))
}

#[cfg(test)]
mod tests;
