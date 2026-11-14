// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Successful Ed25519 checks of immutable table wire bytes. The key includes the
//! public signer and the whole wire's content identifier, including its signature.
//! The global cache retains no ciphertext, plaintext, epoch key, writer grant or custody decision.
//! An identified wire shares its immutable bytes and successful public-signer check across replay
//! clones. It can also retain successful authentication of a non-cutoff payload under an HMAC
//! comparison token for the exact payload key. The caller must first obtain that key and judge
//! the historical writer grant again; this fact grants no access. Cutoff payloads still decrypt.
//! The journal cache charges the allocations, including both fixed fact slots. A compact entry
//! can retain these facts without retaining ciphertext; newly read bytes are hashed independently
//! before attaching them. Historical keys, plaintext, source classification and admission verdicts
//! are never retained.

use super::DOMAIN;
use crate::{Error, Result, crypto};
use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

const MAX_SIGNATURES: usize = 4096;
type Key = ([u8; 32], Digest);
static CACHE: Cache = Cache::new(MAX_SIGNATURES);

/// The signed wire's content identifier, once its signature by `signer` checks.
pub(super) fn verify(signer: &[u8; 32], wire: &[u8]) -> Result<Digest> {
    CACHE.verify(signer, wire)
}

/// [`verify`] of an identified wire's bytes, under the identifier [`Identified::new`] took of
/// them, so they are not hashed again.
pub(super) fn verify_identified(signer: &[u8; 32], wire: &Identified) -> Result<Digest> {
    CACHE.verify_identified(signer, wire)
}

/// Table wire bytes with their content identifier, `Sha256::digest` of exactly those bytes, which
/// only [`Self::new`] takes (a clone shares it with the immutable bytes) and nothing changes after: a table
/// replay names each record's receipt by it, and checks an operation record's signature under it
/// ([`verify_identified`]), hashing that record's bytes once. A conflict certificate is signed by
/// nobody: the signed sources it holds are checked as any wire is, and hashed for that.
#[derive(Clone)]
pub(super) struct Identified(Arc<Wire>);

struct Wire {
    wire: Vec<u8>,
    facts: Arc<Verified>,
}

struct Verified {
    id: Digest,
    /// A successful check under the fixed signature domain, never a historical writer grant.
    signer: OnceLock<[u8; 32]>,
    /// The current payload key's comparison token after successful authentication of a payload
    /// that is not a cutoff. This is neither a key nor a permission to obtain one.
    non_cut_key: OnceLock<crypto::PayloadKeyIdentity>,
}

/// Content-bound cryptographic facts, independently retainable without wire bodies.
/// Neither the signer check nor the payload comparison token grants authority.
#[derive(Clone)]
pub(super) struct Facts(Arc<Vec<Arc<Verified>>>);

impl Facts {
    pub(super) fn of<'a>(wires: impl Iterator<Item = &'a Identified>) -> Self {
        let mut facts: Vec<_> = wires.map(|wire| Arc::clone(&wire.0.facts)).collect();
        facts.sort_unstable_by_key(|fact| fact.id);
        facts.dedup_by_key(|fact| fact.id);
        Self(Arc::new(facts))
    }

    /// Retained heap allocations, charging a shared fact in full in each owner.
    /// Allocator rounding is outside this charge.
    pub(super) fn allocation_bytes(&self) -> usize {
        size_of::<Vec<Arc<Verified>>>()
            + 2 * size_of::<usize>()
            + self.0.capacity() * size_of::<Arc<Verified>>()
            + self.0.len() * (size_of::<Verified>() + 2 * size_of::<usize>())
    }

    fn find(&self, id: Digest) -> Option<Arc<Verified>> {
        self.0
            .binary_search_by_key(&id, |fact| fact.id)
            .ok()
            .map(|at| Arc::clone(&self.0[at]))
    }
}

impl Identified {
    pub(super) fn new(wire: Vec<u8>) -> Self {
        Self::new_with(wire, None)
    }

    /// Hash the new bytes independently before reusing any content-bound fact.
    pub(super) fn new_with(wire: Vec<u8>, known: Option<&Facts>) -> Self {
        #[cfg(test)]
        HASHED.set(HASHED.get() + 1);
        let id = Sha256::digest(&wire);
        let facts = known.and_then(|known| known.find(id)).unwrap_or_else(|| {
            Arc::new(Verified {
                id,
                signer: OnceLock::new(),
                non_cut_key: OnceLock::new(),
            })
        });
        Self(Arc::new(Wire { wire, facts }))
    }

    pub(super) fn id(&self) -> Digest {
        self.0.facts.id
    }

    pub(super) fn wire(&self) -> &[u8] {
        &self.0.wire
    }

    pub(super) fn verified_non_cut(&self, key_identity: &crypto::PayloadKeyIdentity) -> bool {
        self.0
            .facts
            .non_cut_key
            .get()
            .is_some_and(|known| known.matches(key_identity))
    }

    pub(super) fn remember_non_cut(&self, key_identity: crypto::PayloadKeyIdentity) {
        let _ = self.0.facts.non_cut_key.set(key_identity);
    }

    /// A retained wire's allocations: its byte capacity and both shared objects, including
    /// their reference counts. Allocator rounding is outside the journal cache's charge.
    pub(super) fn allocation_bytes(&self) -> usize {
        self.0.wire.capacity() + size_of::<Wire>() + size_of::<Verified>() + 4 * size_of::<usize>()
    }
}

#[cfg(test)]
thread_local! {
    /// How many wires this thread hashed for their identifiers, here and in [`Identified::new`].
    static HASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`HASHED`]'s count on this thread.
#[cfg(test)]
pub(super) fn test_hashed() -> usize {
    HASHED.get()
}

struct Entries {
    known: BTreeSet<Key>,
    order: VecDeque<Key>,
}

struct Cache {
    entries: Mutex<Entries>,
    capacity: usize,
    #[cfg(test)]
    checks: std::sync::atomic::AtomicUsize,
}

impl Cache {
    const fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(Entries {
                known: BTreeSet::new(),
                order: VecDeque::new(),
            }),
            capacity,
            #[cfg(test)]
            checks: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn verify(&self, signer: &[u8; 32], wire: &[u8]) -> Result<Digest> {
        self.verify_as(signer, wire, || {
            #[cfg(test)]
            HASHED.set(HASHED.get() + 1);
            Sha256::digest(wire)
        })
    }

    fn verify_identified(&self, signer: &[u8; 32], wire: &Identified) -> Result<Digest> {
        if wire.0.facts.signer.get() == Some(signer) {
            return Ok(wire.id());
        }
        let id = self.verify_as(signer, wire.wire(), || wire.id())?;
        // Retain successes alone. Another valid signer can still be checked through the global
        // cache; losing this optional publication never changes a verification result.
        let _ = wire.0.facts.signer.set(*signer);
        Ok(id)
    }

    /// [`Self::verify`] where `id` gives `Sha256::digest(wire)`, asked once the wire's shape is
    /// admitted.
    fn verify_as(
        &self,
        signer: &[u8; 32],
        wire: &[u8],
        id: impl FnOnce() -> Digest,
    ) -> Result<Digest> {
        if wire.len() > crypto::MAX_OPERATION {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let signed_end = wire
            .len()
            .checked_sub(64)
            .ok_or_else(|| Error::new("truncated table signature"))?;
        let id = id();
        let key = (*signer, id);
        if self.capacity > 0
            && let Ok(entries) = self.entries.try_lock()
            && entries.known.contains(&key)
        {
            return Ok(id);
        }

        // Contention or poison only loses reuse. Verify outside the lock, and
        // retain successes alone. The domain is fixed for this private cache.
        #[cfg(test)]
        self.checks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crypto::verify(signer, DOMAIN, &wire[..signed_end], &wire[signed_end..])?;
        if self.capacity > 0
            && let Ok(mut entries) = self.entries.try_lock()
            && entries.known.insert(key)
        {
            if entries.order.len() == self.capacity
                && let Some(oldest) = entries.order.pop_front()
            {
                entries.known.remove(&oldest);
            }
            entries.order.push_back(key);
        }
        Ok(id)
    }
}

#[cfg(test)]
#[path = "signatures_tests.rs"]
mod tests;
