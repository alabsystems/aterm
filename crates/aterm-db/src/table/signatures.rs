// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Successful Ed25519 checks of immutable table wire bytes. The key includes the
//! public signer and the whole wire's content identifier, including its signature.
//! No ciphertext, plaintext, epoch key, writer grant or custody decision is retained.

use super::DOMAIN;
use crate::{Error, Result, crypto};
use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use std::{
    collections::{BTreeSet, VecDeque},
    sync::Mutex,
};

const MAX_SIGNATURES: usize = 4096;
type Key = ([u8; 32], Digest);

pub(super) fn verify(signer: &[u8; 32], wire: &[u8]) -> Result<Digest> {
    static CACHE: Cache = Cache::new(MAX_SIGNATURES);
    CACHE.verify(signer, wire)
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
        if wire.len() > crypto::MAX_OPERATION {
            return Err(Error::new("table operation exceeds its bound"));
        }
        let signed_end = wire
            .len()
            .checked_sub(64)
            .ok_or_else(|| Error::new("truncated table signature"))?;
        let id = Sha256::digest(wire);
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
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::Ordering};

    fn signed(tag: u8, domain: &[u8]) -> ([u8; 32], Vec<u8>) {
        let seed = [17; 32];
        let mut wire = vec![tag; 128];
        let signature = crypto::signed(&seed, domain, &wire).unwrap();
        wire.extend_from_slice(&signature);
        (crypto::public(&seed).unwrap(), wire)
    }

    #[test]
    fn reuse_requires_the_same_signer_and_complete_signed_bytes() {
        let cache = Cache::new(4);
        let (signer, wire) = signed(1, DOMAIN);
        let id = cache.verify(&signer, &wire).unwrap();
        assert_eq!(id, Sha256::digest(&wire));
        assert_eq!(cache.verify(&signer, &wire).unwrap(), id);
        assert_eq!(cache.checks.load(Ordering::Relaxed), 1);

        let mut changed = wire.clone();
        changed[0] ^= 1;
        for _ in 0..2 {
            assert!(cache.verify(&signer, &changed).is_err());
        }
        let mut changed_signature = wire.clone();
        *changed_signature.last_mut().unwrap() ^= 1;
        assert!(cache.verify(&signer, &changed_signature).is_err());
        assert!(
            cache
                .verify(&crypto::public(&[18; 32]).unwrap(), &wire)
                .is_err()
        );
        let (_, other_domain) = signed(1, b"another signature domain");
        assert!(cache.verify(&signer, &other_domain).is_err());
        assert_eq!(cache.checks.load(Ordering::Relaxed), 6);
        assert_eq!(cache.verify(&signer, &wire).unwrap(), id);
        assert_eq!(cache.checks.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn successful_checks_are_bounded_and_eviction_requires_verification_again() {
        let cache = Cache::new(2);
        let (signer, first) = signed(1, DOMAIN);
        let (_, second) = signed(2, DOMAIN);
        let (_, third) = signed(3, DOMAIN);
        for wire in [&first, &second, &first, &third] {
            cache.verify(&signer, wire).unwrap();
        }
        assert_eq!(cache.checks.load(Ordering::Relaxed), 3);
        cache.verify(&signer, &first).unwrap();
        assert_eq!(cache.checks.load(Ordering::Relaxed), 4);
        let entries = cache.entries.lock().unwrap();
        assert_eq!(entries.known.len(), 2);
        assert_eq!(entries.order.len(), 2);
        assert_eq!(
            entries.known,
            entries.order.iter().copied().collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn contention_and_poison_fall_back_to_fresh_verification() {
        let cache = Arc::new(Cache::new(2));
        let (signer, wire) = signed(1, DOMAIN);
        cache.verify(&signer, &wire).unwrap();
        let held = cache.entries.lock().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = {
            let cache = cache.clone();
            let wire = wire.clone();
            std::thread::spawn(move || send.send(cache.verify(&signer, &wire)).unwrap())
        };
        let result = receive.recv_timeout(std::time::Duration::from_secs(60));
        drop(held);
        worker.join().unwrap();
        assert!(
            result
                .expect("a busy cache cannot block verification")
                .is_ok()
        );
        assert_eq!(cache.checks.load(Ordering::Relaxed), 2);

        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = cache.entries.lock().unwrap();
            panic!("test a poisoned optional cache");
        }));
        assert!(poisoned.is_err());
        assert!(cache.verify(&signer, &wire).is_ok());
        assert_eq!(cache.checks.load(Ordering::Relaxed), 3);
        let mut changed = wire;
        changed[0] ^= 1;
        assert!(cache.verify(&signer, &changed).is_err());
        assert_eq!(cache.checks.load(Ordering::Relaxed), 4);
    }
}
