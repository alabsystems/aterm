// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Portable negative evidence. Two contradictory signed claims can restrict a
//! registered identity; neither claim acquires payload or writer authority here.

use super::{Scope, Session};
use crate::{
    Error, Result,
    crypto::{MAX_OPERATION, Reader},
};
use astream_wire::sync::{Actor, Digest, Fork, Header};
use aterm_digest::Sha256;

pub const MAX_CONFLICT: usize = 5 + 8 + 2 * MAX_OPERATION;

/// Authenticated contradictory chain claims by one registered identity.
/// The source operations may be unauthorized or undecryptable. This type grants
/// no role, payload, key, spendable balance or right to append an operation.
pub struct Conflict {
    wire: Vec<u8>,
    headers: [Header; 2],
}
impl Conflict {
    /// Verify the exact scope, framing, two signatures, registered actor and
    /// contradiction. Historical writer roles and AEAD are deliberately not
    /// inferred: even a retired identity's signed contradiction remains evidence.
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
        if wire.len() > MAX_CONFLICT {
            return Err(Error::new("table conflict certificate exceeds its bound"));
        }
        let mut reader = Reader::new(wire);
        if reader.take(5)? != b"ACTF1" {
            return Err(Error::new("unknown table conflict certificate"));
        }
        let first = Self::source(&mut reader)?;
        let second = Self::source(&mut reader)?;
        reader.end()?;
        let a = session.envelope(scope, first)?;
        let b = session.envelope(scope, second)?;
        let fork = Fork::between(&a.header, &b.header)
            .ok_or_else(|| Error::new("table certificate has no contradictory actor claims"))?;
        if a.header.id != fork.first || b.header.id != fork.second {
            return Err(Error::new(
                "table conflict sources are not canonically ordered",
            ));
        }
        session
            .journal
            .constraints()
            .map_err(super::failure)?
            .check_known_actor(&fork.actor)
            .map_err(super::failure)?;
        Ok(Self {
            wire: wire.into(),
            headers: [a.header, b.header],
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

    pub fn actor(&self) -> Actor {
        self.headers[0].actor
    }
    pub fn id(&self) -> Digest {
        Sha256::digest(&self.wire)
    }
    /// The complete signed source headers, not authorized application records.
    pub const fn headers(&self) -> &[Header; 2] {
        &self.headers
    }
    pub fn wire(&self) -> &[u8] {
        &self.wire
    }
}
