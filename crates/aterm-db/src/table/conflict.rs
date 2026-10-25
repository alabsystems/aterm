// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable negative evidence. Contradictory signed claims or a signed source
//! discarded by a named control recovery restrict only their signing identity.
//! Neither proof acquires payload or writer authority here.

use super::{Scope, Session};
use crate::{
    Error, Result,
    crypto::{MAX_OPERATION, Reader},
};
use astream_wire::sync::{Actor, Digest, Fork, Header};
use aterm_digest::Sha256;
use std::ops::Range;

pub const MAX_CONFLICT: usize = 5 + 8 + 2 * MAX_OPERATION;

/// Actor, proof class (discarded source = 0, fork = 1), and source identity.
/// Fork proofs use zero as their source: any valid pair proves the same fence.
pub type ProofKey = [u8; 81];

#[derive(Clone, Debug)]
pub struct Discarded {
    pub header: Header,
    pub roster: Digest,
    pub epoch: u64,
    pub authored_ms: u64,
}

/// Authenticated negative evidence against one registered identity.
/// The source operations may be unauthorized or undecryptable. This type grants
/// no role, payload, key, spendable balance or right to append an operation.
pub struct Conflict {
    wire: Vec<u8>,
    headers: Vec<Header>,
    sources: Vec<Range<usize>>,
    discarded: Option<Discarded>,
    active: bool,
}
impl Conflict {
    /// Verify scope, framing, signatures, registered actor and either a chain
    /// contradiction or an exact signed recovery discarding the source roster.
    /// Historical writer roles and AEAD are deliberately not inferred. A fork
    /// remains restrictive after retirement; quarantine requires current control
    /// history to keep the signed source discarded.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "WrongScope",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Forged",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Retired",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    pub fn authenticate(session: &Session<'_>, scope: Scope, wire: &[u8]) -> Result<Self> {
        let proof = Self::restore(session, scope, wire)?;
        if !proof.active {
            return Err(Error::new(
                "discarded-source proof is outside current recovery history",
            ));
        }
        Ok(proof)
    }

    /// Retain an authentic old proof without treating an overruled recovery as
    /// current authority. Replay must survive a later control-history choice.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbSourceReselection",
            action = "Reselect",
            project = "aterm_db::table::store::replay::tests::conflict::reselection_transition"
        )
    )]
    pub(crate) fn restore(session: &Session<'_>, scope: Scope, wire: &[u8]) -> Result<Self> {
        if wire.len() > MAX_CONFLICT {
            return Err(Error::new("table conflict certificate exceeds its bound"));
        }
        let mut reader = Reader::new(wire);
        let mut sources = Vec::new();
        let mut discarded = None;
        let mut active = true;
        let headers = match reader.take(5)? {
            b"ACTF1" => {
                let first = Self::source(&mut reader)?;
                let end = wire.len() - reader.remaining();
                sources.push(end - first.len()..end);
                let second = Self::source(&mut reader)?;
                let end = wire.len() - reader.remaining();
                sources.push(end - second.len()..end);
                reader.end()?;
                let a = session.envelope(scope, first)?;
                let b = session.envelope(scope, second)?;
                let fork = Fork::between(&a.header, &b.header).ok_or_else(|| {
                    Error::new("table certificate has no contradictory actor claims")
                })?;
                if a.header.id != fork.first || b.header.id != fork.second {
                    return Err(Error::new(
                        "table conflict sources are not canonically ordered",
                    ));
                }
                vec![a.header, b.header]
            }
            b"ACTQ1" => {
                let recovery = reader.array()?;
                let source = Self::source(&mut reader)?;
                let end = wire.len() - reader.remaining();
                sources.push(end - source.len()..end);
                reader.end()?;
                let envelope = session.envelope(scope, source)?;
                if !session.discarded_by(&envelope, recovery) {
                    return Err(Error::new(
                        "table restriction lacks a matching signed recovery",
                    ));
                }
                active = session.discarded_source(&envelope);
                discarded = Some(Discarded {
                    header: envelope.header.clone(),
                    roster: envelope.roster,
                    epoch: envelope.epoch,
                    authored_ms: envelope.authored_ms,
                });
                vec![envelope.header]
            }
            _ => return Err(Error::new("unknown table conflict certificate")),
        };
        session
            .journal
            .constraints()
            .map_err(super::failure)?
            .check_known_actor(&headers[0].actor)
            .map_err(super::failure)?;
        Ok(Self {
            wire: wire.into(),
            headers,
            sources,
            discarded,
            active,
        })
    }

    fn source<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8]> {
        let len = u32::from_le_bytes(reader.array()?) as usize;
        if len > MAX_OPERATION {
            return Err(Error::new("table conflict source exceeds its bound"));
        }
        reader.take(len)
    }

    pub(crate) fn from_pair(
        session: &Session<'_>,
        scope: Scope,
        a: &[u8],
        b: &[u8],
    ) -> Result<Self> {
        let mut sources = [a, b];
        sources.sort_by_key(|source| Sha256::digest(source));
        let mut wire = b"ACTF1".to_vec();
        for source in sources {
            if source.len() > MAX_OPERATION {
                return Err(Error::new("table conflict source exceeds its bound"));
            }
            wire.extend_from_slice(&(source.len() as u32).to_le_bytes());
            wire.extend_from_slice(source);
        }
        Self::authenticate(session, scope, &wire)
    }

    /// A local custody witness establishes when to issue this proof. A receiver
    /// needs only the exact signed source and root-signed recovery: old role and
    /// decryption rights are deliberately unnecessary for a self-restriction.
    pub(crate) fn from_discarded(
        session: &Session<'_>,
        scope: Scope,
        source: &[u8],
    ) -> Result<Self> {
        let envelope = session.envelope(scope, source)?;
        let recovery = session
            .discarding_recovery(&envelope)
            .ok_or_else(|| Error::new("no signed recovery discarded this source"))?;
        let mut wire = b"ACTQ1".to_vec();
        wire.extend_from_slice(&recovery);
        wire.extend_from_slice(&(source.len() as u32).to_le_bytes());
        wire.extend_from_slice(source);
        Self::authenticate(session, scope, &wire)
    }

    pub fn actor(&self) -> Actor {
        self.headers[0].actor
    }
    pub fn key(&self) -> ProofKey {
        let mut key = [0; 81];
        key[..48].copy_from_slice(&self.actor());
        if self.discarded.is_some() {
            key[49..].copy_from_slice(&self.headers[0].id);
        } else {
            key[48] = 1;
        }
        key
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn discarded(&self) -> Option<&Discarded> {
        self.discarded.as_ref()
    }
    pub(crate) fn source_for(&self, id: &Digest) -> Option<&[u8]> {
        self.headers
            .iter()
            .position(|header| &header.id == id)
            .map(|index| &self.wire[self.sources[index].clone()])
    }
    pub fn id(&self) -> Digest {
        Sha256::digest(&self.wire)
    }
    /// The complete signed source headers, not authorized application records.
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }
    pub fn wire(&self) -> &[u8] {
        &self.wire
    }
}
